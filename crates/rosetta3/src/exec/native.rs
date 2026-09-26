use machish::image::EntryPoint;
use machish::loader::Loader;
use machish::memory::MmapSpace;
use machish::report::LinkReport;
use machish::resolve::{NativeDylibProvider, SymbolProvider, eager_binding_shims};
use machish::spec::constants::{CPU_SUBTYPE_ARM64E, subtype_of};
use machish::spec::{MachSpec, NativelyExecutable};

use crate::cxxlib;
use crate::exec::Execution;
use crate::stack::GuestStack;
use crate::vm::VmError;

#[derive(Copy, Clone, Debug)]
pub struct HostRunnable<S>(pub S);

impl<S: MachSpec> MachSpec for HostRunnable<S> {
    const CPU_TYPE: i32 = S::CPU_TYPE;

    fn page_size(&self) -> u64 {
        self.0.page_size()
    }

    fn accepts_subtype(&self, cpusubtype: i32) -> bool {
        self.0.accepts_subtype(cpusubtype) && subtype_of(cpusubtype) != CPU_SUBTYPE_ARM64E
    }

    fn subtype_rank(&self, cpusubtype: i32) -> u32 {
        self.0.subtype_rank(cpusubtype)
    }

    fn natively_executable(&self) -> bool {
        self.0.natively_executable()
    }
}

#[cfg(target_arch = "aarch64")]
pub type HostSpec = HostRunnable<NativelyExecutable<machish::spec::arch::Arm64>>;
#[cfg(target_arch = "aarch64")]
pub const HOST_SPEC: HostSpec = HostRunnable(NativelyExecutable(machish::spec::arch::Arm64));

#[cfg(not(target_arch = "aarch64"))]
pub type HostSpec = HostRunnable<NativelyExecutable<machish::spec::arch::X86_64>>;
#[cfg(not(target_arch = "aarch64"))]
pub const HOST_SPEC: HostSpec = HostRunnable(NativelyExecutable(machish::spec::arch::X86_64));

/// Whether the native backend can run this slice - the same question the
/// loader will ask when it selects one.
///
/// A matching instruction set is not on its own enough, because the code is
/// still Darwin's: it reads its thread-specific data straight out of the
/// thread pointer register. [`darwin::tls::can_enter_darwin`] is what says the
/// host can give it that array, and it is the narrower of the two conditions.
/// On aarch64 Linux it is false - Darwin arm64 code reads `TPIDRRO_EL0`, which
/// Linux does not let userspace write - so a universal Mach-O there takes the
/// emulated backend's x86-64 slice instead.
pub fn runnable(slice: &machish::fat::Slice) -> bool {
    darwin::tls::can_enter_darwin()
        && slice.cputype == HostSpec::CPU_TYPE
        && HOST_SPEC.accepts_subtype(slice.cpusubtype)
}

/// A program of the host's own architecture, mapped executable and called directly.
pub struct Native {
    report: LinkReport,
    /// The mappings the report's addresses point into, and what says an address
    /// is the program's. Dropping it would unmap the program.
    memory: MmapSpace,
    /// The `argc`/`argv`/`envp`/`apple` block and, for an `LC_UNIXTHREAD`
    /// image, the stack its entry point runs on. An `LC_MAIN` image is entered
    /// by a call on the host's own stack.
    stack: GuestStack,
}

/// Enter a process entry point with the kernel argument frame at the stack
/// pointer.
///
/// This is a jump, not a call: an `LC_UNIXTHREAD` entry point finds `argc` in
/// the first word at `rsp`/`sp`, where a call would have put its return
/// address. Process entry points finish by calling `exit` and have nowhere to
/// return to, so abandoning this function's frame is intentional.
///
/// # Safety
///
/// `entry` must name host-executable code and `sp` must name a suitably aligned
/// kernel argument frame with enough writable stack below it.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn enter_unix_thread(_entry: u64, _sp: u64) -> ! {
    core::arch::naked_asm!("mov rsp, rsi", "jmp rdi");
}

/// The arm64 form of [`enter_unix_thread`].
///
/// # Safety
///
/// The requirements are the same as for the x86-64 form.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn enter_unix_thread(_entry: u64, _sp: u64) -> ! {
    core::arch::naked_asm!("mov sp, x1", "br x0");
}

unsafe impl Sync for Native {}

impl Native {
    /// Load and link `path`, with `args` laid out for its entry point.
    pub fn load(path: &str, args: &[std::ffi::OsString]) -> Result<Self, VmError> {
        // Before anything can hand the program a symbol. A trampoline is five
        // bytes of x86-64 that turn a call back into an import for the lifter to
        // intercept, and the native backend intercepts nothing: a `dlsym` result
        // here has to be the host function itself. The program is this host's
        // own architecture, so on aarch64 it would `blr` into x86-64 bytes and
        // on x86-64 it would call a bridge with no run loop behind it.
        crate::trampoline::disarm();

        let bytes = std::fs::read(path).map_err(|e| VmError::Load {
            path: path.to_string(),
            reason: e.to_string(),
        })?;

        let mut memory = MmapSpace::new();
        let mut host = NativeDylibProvider::system();
        host.allow_everything();
        darwin::divergent::deny_in(&mut host);
        crate::objc::host::deny_foreign_classes(&mut host);
        crate::frameworks::deny_host_frameworks(&mut host);
        crate::opengl::deny_host_gl(&mut host);

        let eager = eager_binding_shims();
        // The thread-local bootstrap comes from here rather than from a
        // provider of its own: `HostProviders::chain` reaches the same one for
        // a `dlopen`ed image, and a name two providers answer differently is a
        // name that is right on one link path and wrong on the other - which is
        // exactly how a `dlopen`ed `__thread_vars` ended up bound to the plain
        // `bootstrap` and losing the guest's registers.
        let runtime = crate::shims::runtime::provider();
        let platform = darwin::provider();
        let objc = crate::objc::dispatch::provider();
        let frameworks = crate::frameworks::provider();
        let dyld = crate::dyld::provider();
        let steam = crate::steam::provider();
        let opengl = crate::opengl::provider();
        let openal = crate::openal::provider();
        let mut providers: Vec<&dyn SymbolProvider> = match crate::frameworks::using_builtin() {
            true => vec![
                &eager,
                &runtime,
                &platform,
                &objc,
                &frameworks,
                &dyld,
                &steam,
                &opengl,
                &openal,
            ],
            false => vec![&eager, &runtime, &platform, &steam],
        };
        match darwin::strict::enabled() {
            true => crate::program::announce_strict(),
            false => providers.push(&host),
        }

        darwin::sysctl::set_guest_arch(match cfg!(target_arch = "aarch64") {
            true => darwin::sysctl::GuestArch::Arm64,
            false => darwin::sysctl::GuestArch::X86_64,
        });
        #[cfg(not(target_os = "macos"))]
        darwin::sysctl::set_runs_host_instructions(true);

        let mut report = Loader::new(HOST_SPEC)
            .link_with(
                &bytes,
                path,
                &mut memory,
                &mut crate::cxxlib::source(cxxlib::HOST),
                &providers,
            )
            .map_err(|e| VmError::Load {
                path: path.to_string(),
                reason: e.to_string(),
            })?;

        let load = |e: machish::Error| VmError::Load {
            path: path.to_string(),
            reason: e.to_string(),
        };
        crate::cxxlib::bind_unwinder_to_the_vendored_one(&mut report, &mut memory, cxxlib::HOST)
            .map_err(load)?;
        crate::cxxlib::bind_cxxabi_to_the_vendored_one(&mut report, &mut memory, cxxlib::HOST)
            .map_err(load)?;
        crate::cxxlib::bind_objc_exceptions_to_the_vendored_cxxabi(
            &mut report,
            &mut memory,
            cxxlib::HOST,
        )
        .map_err(load)?;

        crate::interpose::apply(&mut report, &mut memory).map_err(|e| VmError::Load {
            path: path.to_string(),
            reason: e.to_string(),
        })?;
        crate::hostheap::apply(&mut report, &mut memory).map_err(|e| VmError::Load {
            path: path.to_string(),
            reason: e.to_string(),
        })?;
        crate::heapcheck::install(&mut report, &mut memory).map_err(|e| VmError::Load {
            path: path.to_string(),
            reason: e.to_string(),
        })?;
        crate::interpose::adopt_guest_allocator(&report);

        let unbound = crate::program::announce_unbound_imports(&report, path);
        crate::program::announce_passthrough_imports(&report, path);
        if !unbound.is_empty() && crate::program::strict_imports() {
            return Err(VmError::Load {
                path: path.to_string(),
                reason: format!(
                    "{} unbound import(s), the first being {} ({} is set)",
                    unbound.len(),
                    unbound[0].1.symbol,
                    crate::program::STRICT_IMPORTS_ENV,
                ),
            });
        }

        flush_icache(&report);
        crate::tls::register(&report);
        crate::dyld::set_executable_path(path, report.root().map_or(0, |i| i.slide));
        if let Some(root) = report.root() {
            crate::dyld::set_executable_header(root.base);
        }
        crate::dyld::register_mapped_images(&report);
        crate::lldb::register_images(&report, path);

        let stack = GuestStack::new(crate::stack::DEFAULT_STACK, path, args)?;
        let (argc, argv, _, _) = stack.main_args();
        crate::shims::runtime::set_process_args(argc, argv);
        foundation::processinfo::set_arguments(args);
        Ok(Native {
            report,
            memory,
            stack,
        })
    }
}

impl Execution for Native {
    fn engine(&self) -> crate::exec::Engine {
        crate::exec::Engine::Native
    }

    fn report(&self) -> &LinkReport {
        &self.report
    }

    fn main_args(&self) -> (u64, u64, u64, u64) {
        self.stack.main_args()
    }

    fn call(&self, entry: u64, args: &[u64]) -> Result<u64, VmError> {
        if entry == 0 {
            return Err(VmError::NoCode { pc: 0, from: None });
        }
        let mut a = [0u64; 6];
        for (slot, &arg) in a.iter_mut().zip(args) {
            *slot = arg;
        }
        // Six arguments always, whatever the callee's arity: under both AAPCS64
        // and SysV a non-variadic callee ignores the registers it did not
        // declare, so this costs five moves and needs no signature.
        //
        // SAFETY: `entry` is an address the loader mapped executable from an
        // image of this host's architecture, so it is a real function; calling
        // it with more integer registers set than it reads is what the two ABIs
        // above permit.
        let f: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
            unsafe { std::mem::transmute(entry as usize as *const ()) };
        // The image is Darwin code, so for the duration of the call this thread's
        // thread pointer must be the Darwin TSD array rather than the host's:
        // `pthread_self`, `errno` and the stack canary are all direct loads off it
        // and never reach a shim. Compiles away on macOS, where it is already.
        let _darwin = darwin::tls::DarwinThread::enter();
        Ok(f(a[0], a[1], a[2], a[3], a[4], a[5]))
    }

    fn prepare(&self) -> Result<(), VmError> {
        if crate::frameworks::using_builtin() {
            return self.realize_objc();
        }
        // Registering the image's classes with the host's Objective-C runtime, which
        // is a step only a host that has one can take.
        #[cfg(target_os = "macos")]
        crate::objc::native::map_images(&self.report);
        Ok(())
    }

    fn realize_loaded_objc(&self, report: &LinkReport) -> Result<(), VmError> {
        self.realize_loaded(report)
    }

    fn run_loaded_load_methods(&self, report: &LinkReport) -> Result<usize, VmError> {
        match crate::objc::published() {
            Some(rt) => crate::objc::call_loaded_load_methods(self, rt, report),
            None => Ok(0),
        }
    }

    fn enter_main(&self) -> Result<u64, VmError> {
        let root = self
            .report
            .root()
            .ok_or(VmError::NoCode { pc: 0, from: None })?;
        let entry = root.entry.ok_or(VmError::NoCode { pc: 0, from: None })?;
        if let Some(EntryPoint::UnixThread { .. }) = root.entry_command {
            let sp = self.stack.kernel_sp().expect(
                "an LC_UNIXTHREAD program starts on the stack its arguments were laid out on",
            );
            // As with `call`, Darwin code needs the Darwin TSD array installed
            // before its first instruction. A process entry point does not
            // return, so this guard deliberately lives until `exit` ends the
            // process.
            let _darwin = darwin::tls::DarwinThread::enter();
            // SAFETY: the loader mapped `entry` from a host-architecture image,
            // and `GuestStack` owns the aligned, writable frame at `sp`.
            unsafe { enter_unix_thread(entry, sp) }
        }
        let (argc, argv, envp, apple) = self.main_args();
        self.call(entry, &[argc, argv, envp, apple])
    }
}

impl Native {
    /// Realize the Objective-C metadata of an image the guest `dlopen`ed.
    /// before the other's classes went in and would then replace it.
    fn realize_loaded(&self, report: &LinkReport) -> Result<(), VmError> {
        static REGISTERING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = REGISTERING.lock().unwrap_or_else(|e| e.into_inner());
        let Some(current) = crate::objc::published() else {
            // Nothing has registered, so there is no runtime to extend and no
            // class of the program's this image could subclass. The selectors
            // are still its own.
            crate::objc::unique_loaded_selectors(self, report)?;
            return Ok(());
        };
        let mut extended = current.clone();
        crate::objc::register_loaded(self, &mut extended, report)?;
        let leaked: &'static crate::objc::ObjcRuntime = Box::leak(Box::new(extended));
        crate::objc::publish(leaked);
        crate::objc::rewrite_refs(self, leaked, report)?;
        Ok(())
    }

    /// Realize the image's Objective-C metadata in this tree's own runtime.
    ///
    /// What `_objc_map_images` does for the host's libobjc, minus the part dyld
    /// would have done: the classes are rebuilt through the public C API, and
    /// the guest's own references to them are pointed at the result.
    fn realize_objc(&self) -> Result<(), VmError> {
        // Before the program can send one: a format method reached as an `IMP`
        // has no variadic tail without this.
        crate::shims::variadic::install();
        let rt = Box::leak(Box::new(crate::objc::register(self)?));
        crate::objc::publish(rt);
        crate::objc::rewrite_refs(self, rt, &self.report)?;
        // `+load` before the C constructors of the same image, which is dyld's
        // order and the one a program that uses both depends on.
        crate::objc::call_load_methods(self, rt)?;
        Ok(())
    }
}

/// The program's memory, for the passes that write back into its own metadata.
struct HostView<'a>(&'a MmapSpace);

impl HostView<'_> {
    fn span(&self, va: u64, len: usize) -> Result<*mut u8, machish::Error> {
        use machish::memory::Regions;
        let outside = || machish::Error::Memory("the address is in no mapped region");
        let (base, bytes) = self
            .0
            .region_at(va)
            .or_else(|| crate::dyld::region_containing(va))
            .ok_or_else(outside)?;
        let offset = usize::try_from(va - base).map_err(|_| outside())?;
        if bytes.len() - offset < len {
            return Err(outside());
        }
        Ok(va as usize as *mut u8)
    }
}

impl crate::abi::GuestMemory for HostView<'_> {
    fn read(&self, va: u64, out: &mut [u8]) -> Result<(), machish::Error> {
        let src = self.span(va, out.len())?;
        // SAFETY: `span` checked the whole range lies in a region the loader
        // mapped, and the mapping outlives this view.
        unsafe { core::ptr::copy_nonoverlapping(src, out.as_mut_ptr(), out.len()) };
        Ok(())
    }

    fn write(&mut self, va: u64, data: &[u8]) -> Result<(), machish::Error> {
        let dst = self.span(va, data.len())?;
        // SAFETY: as above. Every caller writes into `__DATA`, which the loader
        // left writable.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len()) };
        Ok(())
    }
}

impl crate::objc::Realizer for Native {
    fn report(&self) -> &LinkReport {
        &self.report
    }

    fn memory(&self) -> Box<dyn crate::abi::GuestMemory + '_> {
        Box::new(HostView(&self.memory))
    }

    /// Both loaders, for the reason [`HostView`] gives.
    fn is_guest_address(&self, va: u64) -> bool {
        use machish::memory::Regions;
        self.memory.region_at(va).is_some() || crate::dyld::contains(va)
    }

    fn call(&self, f: u64, args: &[u64]) -> Result<u64, VmError> {
        Execution::call(self, f, args)
    }

    /// The address itself. A native image's methods are host functions with the
    /// host's ABI, so there is nothing for a trampoline to convert.
    fn imp_for(
        &self,
        m: &machish::extensions::objc::ObjcMethod,
    ) -> Result<(u64, Option<&'static str>), &'static str> {
        Ok((m.imp, None))
    }

    /// The registered class. Nothing intercepts the sends a native image makes,
    /// so the guest's own `objc_class` would be a receiver this runtime never
    /// made - see [`crate::objc::rewrite_refs`].
    fn class_value(&self, rt: &crate::objc::ObjcRuntime, guest_va: u64) -> u64 {
        rt.host_or(guest_va)
    }
}

/// Flush the instruction cache over every executable segment.
///
/// `MmapSpace` only does this for `MAP_JIT` writes, and these are ordinary ones.
/// The pages are freshly mapped, so what is being guarded against is the kernel
/// handing back a physical page whose lines some earlier process left behind.
fn flush_icache(report: &LinkReport) {
    for seg in report.images.iter().flat_map(|i| i.segments.iter()) {
        if !seg.executable || seg.vmsize == 0 {
            continue;
        }
        // SAFETY: the range is a segment the loader just mapped and wrote, and
        // invalidating cache lines over it changes nothing about its contents.
        unsafe {
            invalidate_icache(seg.vmaddr as usize as *mut u8, seg.vmsize as usize);
        }
    }
}

/// Invalidate the instruction cache over `len` bytes at `start`.
unsafe fn invalidate_icache(start: *mut u8, len: usize) {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn sys_icache_invalidate(start: *mut core::ffi::c_void, len: usize);
        }
        unsafe { sys_icache_invalidate(start as *mut core::ffi::c_void, len) };
    }
    #[cfg(not(target_os = "macos"))]
    {
        unsafe extern "C" {
            fn __clear_cache(start: *mut core::ffi::c_char, end: *mut core::ffi::c_char);
        }
        // SAFETY: as above. `__clear_cache` takes the half-open range rather than a
        // base and a length, which is the only difference between the two.
        unsafe {
            __clear_cache(
                start as *mut core::ffi::c_char,
                start.add(len) as *mut core::ffi::c_char,
            )
        };
    }
}
