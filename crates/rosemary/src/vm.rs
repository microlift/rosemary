use std::fmt;
use std::ops::Range;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use machish::memory::{MmapSpace, Regions};

use microlift::arch::x86_64::context::RegisterContext;
use microlift::core::exit::{ExitReason, TransferKind};
use static_init::dynamic;

use crate::abi::{AbiError, GuestMemory};
use crate::cache::CodeCache;
use crate::program::Program;
use crate::stack::GuestStack;
use crate::thunk::ThunkTable;

/// Anything that stops the guest.
#[derive(Debug)]
pub enum VmError {
    /// Nothing at this address decodes as guest code, or it is not mapped.
    NoCode { pc: u64, from: Option<Transfer> },
    /// Microlift could not translate the region - an instruction it does not implement,
    /// or a compilation failure.
    Lift {
        pc: u64,
        start: u64,
        source: microlift::MicroliftError,
        /// `start` as `image+unslid-va`, the form a disassembler can be pointed at.
        at: Option<String>,
    },
    /// An intercepted import could not be called. See [`AbiError`]: every
    /// variant names the symbol.
    Abi(AbiError),
    /// An indirect call to an address that is neither guest code nor anything
    /// the runtime can describe well enough to call.
    UnknownIndirectCall { callee: u64, resume: u64 },
    /// The guest executed a `syscall` instruction. A stop rather than a passthrough.
    Syscall { number: u64, pc: u64 },
    /// An instruction the recompiler handed to [`crate::interp`], which does not
    /// perform it either. `what` names it, or says why it was refused.
    Interpret { pc: u64, what: String },
    /// An exit the runtime has nothing to do about: an invalid instruction, a
    /// trap, a divide error.
    Trap {
        reason: ExitReason,
        code: u32,
        pc: u64,
    },
    /// The guest stack could not be reserved or written.
    Stack { source: machish::Error },
    /// A host->guest thunk could not be built.
    Thunk { guest: u64, reason: String },
    /// A guest callback the *host* invoked failed partway through.
    Callback { guest: u64, reason: String },
    /// The program could not be loaded or linked.
    Load { path: String, reason: String },
    /// Guest execution nested deeper than [`MAX_DEPTH`] host->guest callbacks.
    TooDeep { pc: u64 },
    /// The selected execution backend does not implement what this image needs.
    Unsupported { what: String },
}

/// How many times the runtime will say the same thing before going quiet.
const SAY_LIMIT: usize = 16;

/// Report a failure once, and stop repeating yourself.
fn say(err: &VmError) {
    use std::sync::Mutex;
    static SAID: Mutex<(usize, String)> = Mutex::new((0, String::new()));

    let text = err.to_string();
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    let (count, last) = &mut *said;
    if *count > SAY_LIMIT || *last == text {
        return;
    }
    *count += 1;
    last.clear();
    last.push_str(&text);
    match *count > SAY_LIMIT {
        // The cap is announced rather than silently applied: a run that simply
        // stopped reporting would look like a run that stopped failing.
        true => eprintln!(
            "rosemary: {SAY_LIMIT} failures reported; further ones are parked and not printed"
        ),
        false => eprintln!("rosemary: {text}"),
    }
}

/// How often [`Vm::run_loop`] should report the guest's position, if at all.
fn trace_interval() -> Option<u64> {
    #[dynamic]
    static EVERY: Option<u64> = {
        std::env::var("ROSEMARY_TRACE_PC")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
    };
    *EVERY
}

/// How control reached the address that stopped the guest.
#[derive(Debug, Clone)]
pub struct Transfer {
    /// The region control left, as a *slid* address.
    pub pc: u64,
    /// The exit that transferred it, and microlift's sub-code.
    pub exit: ExitReason,
    pub code: u32,
    /// The guest stack pointer as the region left it.
    pub rsp: u64,
    /// `pc` as `image+offset`, filled in only on the error path.
    pub at: Option<String>,
}

impl fmt::Display for Transfer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "reached from {:#x}", self.pc)?;
        if let Some(at) = &self.at {
            write!(f, " ({at})")?;
        }
        write!(
            f,
            " by {} (code {:#x}), rsp {:#x}",
            self.exit, self.code, self.rsp
        )
    }
}

/// The registers a returning guest call can have left its answer in.
///
/// SysV AMD64 splits a two-eightbyte aggregate across `rax`/`rdx` and
/// `xmm0`/`xmm1` by class, so a caller that knows only `rax` cannot read one back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Returned {
    pub rax: u64,
    pub rdx: u64,
    pub xmm0: u64,
    pub xmm1: u64,
}

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VmError::NoCode { pc, from: None } => write!(f, "no guest code at {pc:#x}"),
            VmError::NoCode {
                pc,
                from: Some(from),
            } => write!(f, "no guest code at {pc:#x}: {from}"),
            VmError::Lift {
                pc,
                start,
                source,
                at,
            } => {
                write!(f, "lifting the region at {start:#x}")?;
                if let Some(at) = at {
                    write!(f, " ({at})")?;
                }
                write!(f, " (for pc {pc:#x}): {source}")
            }
            VmError::Abi(e) => write!(f, "{e}"),
            VmError::UnknownIndirectCall { callee, resume } => write!(
                f,
                "indirect call to {callee:#x} from {resume:#x}: neither guest code nor a \
                 described host function"
            ),
            VmError::Syscall { number, pc } => {
                write!(f, "guest syscall {number:#x} at {pc:#x}: not translated")
            }
            VmError::Interpret { pc, what } => {
                write!(f, "the interpreter refused {pc:#x}: {what}")
            }
            VmError::Trap { reason, code, pc } => {
                write!(f, "guest stopped at {pc:#x}: {reason} (code {code:#x})")
            }
            VmError::Stack { source } => write!(f, "guest stack: {source}"),
            VmError::Thunk { guest, reason } => {
                write!(f, "no host->guest thunk for {guest:#x}: {reason}")
            }
            VmError::Callback { guest, reason } => {
                write!(f, "in the guest callback at {guest:#x}: {reason}")
            }
            VmError::Load { path, reason } => write!(f, "{path}: {reason}"),
            VmError::TooDeep { pc } => {
                write!(f, "guest re-entered {MAX_DEPTH} deep at {pc:#x}")
            }
            VmError::Unsupported { what } => write!(f, "not supported by this backend: {what}"),
        }
    }
}

impl std::error::Error for VmError {}

/// An exit, as [`crate::stats`] counts them.
pub(crate) fn classify(exit: ExitReason, code: u32) -> crate::stats::Exit {
    use crate::stats::Exit;
    match exit {
        // The end of a call this runtime made - the `int3` in [`crate::retstub`]
        // - and not a guest breakpoint, which does not come back from `step` at
        // all.
        ExitReason::Breakpoint => Exit::Return,
        ExitReason::None => Exit::Fallthrough,
        ExitReason::ImportedFunctionCall => Exit::Import,
        ExitReason::IndirectCall => Exit::Indirect,
        ExitReason::Cpuid => Exit::Instruction,
        ExitReason::UnknownJump => match TransferKind::try_from(code) {
            Ok(TransferKind::DirectCall) => Exit::DirectCall,
            Ok(TransferKind::RegionTail | TransferKind::NoResumePoint) => Exit::RegionTail,
            Ok(TransferKind::IndirectJump) => Exit::IndirectJump,
            Ok(TransferKind::Return) => Exit::ReturnOut,
            Ok(TransferKind::DirectJump) => Exit::DirectJump,
            Ok(TransferKind::ConditionalJump) => Exit::CondJump,
            _ => Exit::OtherJump,
        },
        _ => Exit::Other,
    }
}

impl From<AbiError> for VmError {
    fn from(e: AbiError) -> Self {
        VmError::Abi(e)
    }
}

/// How many host->guest callbacks may nest before the runtime calls it runaway.
pub const MAX_DEPTH: usize = 64;

/// Guest memory, as a value that can be handed to a shim without borrowing the VM.
///
/// ## Addresses outside the guest are permitted, and why
///
/// A guest under high-level emulation routinely holds pointers to *host* memory:
///
/// It answers "is anything here", not "may I read this", which is enough to turn the
/// common bug - a null or wild pointer - into a named error instead of a fault.
///
/// ## Why the page cache is not a set behind a lock
///
/// It was, and `read_cstr`'s byte-at-a-time walk is what made that untenable:
///
/// ## What is *not* in the region list
///
/// The stacks of threads the guest spawned.
#[derive(Clone)]
pub struct GuestView {
    regions: Arc<Vec<Range<u64>>>,
    mapped: Arc<PageCache>,
}

/// Pages [`GuestView`] has already probed, direct-mapped and lock-free.
///
/// A zero slot is empty.
pub struct PageCache {
    slots: Box<[std::sync::atomic::AtomicU64]>,
}

/// How many probed pages are remembered. A power of two, so the index is a mask.
const PAGES: usize = 4096;

impl PageCache {
    fn new() -> Self {
        PageCache {
            slots: (0..PAGES)
                .map(|_| std::sync::atomic::AtomicU64::new(0))
                .collect(),
        }
    }

    /// The slot a page-aligned address maps to. Shifted past the page offset
    /// first, which is all zeros and would leave most of the table empty.
    fn slot(page: u64) -> usize {
        (page >> 12) as usize & (PAGES - 1)
    }

    /// Whether `page` is known to be mapped.
    fn known(&self, page: u64) -> bool {
        self.slots[Self::slot(page)].load(Ordering::Relaxed) == page
    }

    /// Record that it is.
    fn remember(&self, page: u64) {
        self.slots[Self::slot(page)].store(page, Ordering::Relaxed);
    }
}

impl GuestView {
    /// A view over `regions`, which must be identity-mapped host memory.
    ///
    /// Sorted here, once, because [`GuestView::region_of`] is a binary search and this
    /// answers a question for every guest byte the *runtime* touches. A Hat in Time
    /// spent 1.2% of its running time walking the list unsorted.
    pub fn new(mut regions: Vec<Range<u64>>) -> Self {
        regions.sort_by_key(|r| r.start);
        // The search below takes the last region beginning at or below `va` and
        // checks only *its* end, which is the right answer exactly when the
        // regions do not overlap - a `va` inside an enclosing region but past a
        // nested one would be missed.
        debug_assert!(
            regions.windows(2).all(|p| p[0].end <= p[1].start),
            "GuestView's regions must not overlap"
        );
        GuestView {
            regions: Arc::new(regions),
            mapped: Arc::new(PageCache::new()),
        }
    }

    /// The mapped guest region holding `va`, if there is one.
    fn region_of(&self, va: u64) -> Option<&Range<u64>> {
        let at = self.regions.partition_point(|r| r.start <= va);
        self.regions.get(at.checked_sub(1)?).filter(|r| va < r.end)
    }

    /// Whether `va` is inside a mapped guest region.
    pub fn contains(&self, va: u64) -> bool {
        self.region_of(va).is_some()
    }

    /// Check `[va, va+len)` is addressable, and return the host pointer.
    fn resolve(&self, va: u64, len: usize) -> Option<*mut u8> {
        let end = va.checked_add(len as u64)?;
        if va == 0 {
            return None;
        }
        let inside = self.region_of(va).is_some_and(|r| end <= r.end);
        if !inside && !self.host_readable(va, end) {
            return None;
        }
        usize::try_from(va).ok().map(|a| a as *mut u8)
    }

    /// Whether every page of `[va, end)` is mapped in this process.
    fn host_readable(&self, va: u64, end: u64) -> bool {
        let page = machish::memory::host_page_size();
        let mut at = va & !(page - 1);
        while at < end {
            if !self.mapped.known(at) {
                if !page_is_mapped(at, page) {
                    return false;
                }
                self.mapped.remember(at);
            }
            at += page;
        }
        true
    }
}

/// Whether the host has anything mapped at `[at, at+len)`.
fn page_is_mapped(at: u64, len: u64) -> bool {
    let Ok(addr) = usize::try_from(at) else {
        return false;
    };
    // SAFETY: `msync` inspects the process's own mappings and, for a private
    // anonymous one, has nothing to write back. An unmapped address is
    // reported as `ENOMEM` rather than faulting, which is the entire reason
    // for asking this way.
    let rc = unsafe {
        libc::msync(
            addr as *mut libc::c_void,
            len as libc::size_t,
            libc::MS_ASYNC,
        )
    };
    rc == 0
}

impl GuestView {
    /// Write `data` at `va` without taking a `&mut` view.
    ///
    /// The same store [`GuestMemory::write`] makes - nothing in a write mutates the
    /// view, and `PageCache` is atomics - reachable from a shared borrow, so the
    /// per-call paths never clone the view's two `Arc`s, a line every guest thread
    /// shares.
    pub fn store(&self, va: u64, data: &[u8]) -> Result<(), machish::Error> {
        let dst = self
            .resolve(va, data.len())
            .ok_or(machish::Error::OutOfBounds {
                addr: va,
                len: data.len(),
            })?;
        // SAFETY: as for `GuestMemory::write`, which is this function with a `&mut`
        // borrow in front of it.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len()) };
        Ok(())
    }
}

impl GuestMemory for GuestView {
    fn read(&self, va: u64, out: &mut [u8]) -> Result<(), machish::Error> {
        let src = self
            .resolve(va, out.len())
            .ok_or(machish::Error::OutOfBounds {
                addr: va,
                len: out.len(),
            })?;
        // SAFETY: `resolve` checked that `[va, va+len)` lies in a guest region
        // this VM keeps mapped, or in a page the host has mapped. Guest and
        // host share one address space by construction, so the address is the
        // pointer.
        unsafe { std::ptr::copy_nonoverlapping(src, out.as_mut_ptr(), out.len()) };
        Ok(())
    }

    // Writability is the guest's business: a store to a page it made read-only
    // faults exactly as it would have on x86.
    fn write(&mut self, va: u64, data: &[u8]) -> Result<(), machish::Error> {
        GuestView::store(self, va, data)
    }
}

// Nothing in a write mutates the view, so a borrow serves wherever a
// `&mut dyn GuestMemory` is wanted without cloning one.
impl GuestMemory for &GuestView {
    fn read(&self, va: u64, out: &mut [u8]) -> Result<(), machish::Error> {
        GuestView::read(self, va, out)
    }

    fn write(&mut self, va: u64, data: &[u8]) -> Result<(), machish::Error> {
        GuestView::store(self, va, data)
    }
}

/// A loaded program and everything needed to run it.
///
/// ## What is shared between guest threads, and what is not
///
/// Everything in this struct is shared, because everything in it is either immutable or
/// genuinely common to every thread.
pub struct Vm {
    /// The linked guest, identity-mapped. Immutable once loaded: guest *memory*
    /// changes constantly, but through [`GuestView`], not through this.
    pub program: SharedProgram,
    cache: CodeCache,
    thunks: RwLock<ThunkTable>,
    /// The published, immutable half of `thunks` - see [`crate::thunk::ThunkView`].
    thunk_view: AtomicPtr<crate::thunk::ThunkView>,
    /// How many thunks exist, readable without the lock.
    thunk_count: AtomicUsize,
    view: GuestView,
    /// The guest's Objective-C classes, once realized in the selected runtime.
    objc: AtomicPtr<crate::objc::ObjcRuntime>,
    /// Held across a registration pass, so two `dlopen`s cannot each extend the
    /// same snapshot and publish one of the results.
    objc_registration: Mutex<()>,
}

/// A linked program, shareable between the threads running it.
///
/// The `unsafe impl` is deliberately confined to this one field rather than asserted
/// over the whole [`Vm`], so that everything else - the code cache, the thunk table,
/// the guest view - still has to convince the compiler on its own.
pub struct SharedProgram(Program<MmapSpace>);

// SAFETY: a `Program` is not mutated after construction; what makes it `!Sync` is
// the raw pointers inside its `MmapSpace`, which are read-only handles to mappings
// living exactly as long as the `Vm`. Guest memory is shared and unsynchronised
// deliberately - two guest threads racing on a guest variable is a race the guest
// wrote and would have had on Intel. What this runtime owes is that its own
// structures survive concurrent use, which is what the mutexes are for.
unsafe impl Sync for SharedProgram {}
unsafe impl Send for SharedProgram {}

impl std::ops::Deref for SharedProgram {
    type Target = Program<MmapSpace>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// A `Vm` is reached from more than one thread - through
// [`crate::thunk::publish`], which is how a thread the guest spawned finds it -
// and nothing in the type system was demanding it be sound, because a published
// VM travels through an `AtomicPtr` and a raw pointer carries no bounds.
const _: () = {
    const fn requires_sync<T: Sync>() {}
    requires_sync::<Vm>();
};

impl Vm {
    /// Load `path`, identity-mapped, with a guest stack and `argv`.
    pub fn load(path: &str, args: &[std::ffi::OsString]) -> Result<Self, VmError> {
        let program = Program::load_executable(path, &[]).map_err(|e| VmError::Load {
            path: path.to_string(),
            reason: e.to_string(),
        })?;
        Self::new(program, path, args)
    }

    /// Wrap an already-linked program.
    pub fn new(
        program: Program<MmapSpace>,
        exe_path: &str,
        args: &[std::ffi::OsString],
    ) -> Result<Self, VmError> {
        Self::with_policy(program, exe_path, args, crate::ImportPolicy::default())
    }

    /// As [`Vm::new`], choosing how imports reach the host.
    pub fn with_policy(
        program: Program<MmapSpace>,
        exe_path: &str,
        args: &[std::ffi::OsString],
        policy: crate::ImportPolicy,
    ) -> Result<Self, VmError> {
        let mut program = program;
        // Before anything reads the program's memory, and only under the fast
        // policy.
        match policy {
            crate::ImportPolicy::AlwaysSlow => crate::trampoline::disarm(),
            _ => {
                program.install_trampolines();
            }
        }
        let program = program;

        let stack = GuestStack::new(crate::stack::DEFAULT_STACK, exe_path, args)?;
        // What `_NSGetArgc`/`_NSGetArgv` answer with, now that there is a layout
        // for them to point into.
        let (argc, argv, _, _) = stack.main_args();
        crate::shims::runtime::set_process_args(argc, argv);
        foundation::processinfo::set_arguments(args);

        let mut ranges: Vec<Range<u64>> = program
            .memory
            .regions()
            .into_iter()
            .map(|(base, bytes)| base..base + bytes.len() as u64)
            .collect();
        ranges.push(stack.range());

        // Every `_Thread_local` in the program becomes addressable here, before
        // a line of guest code runs - an initializer may touch one.
        crate::tls::register(&program.report, crate::exec::Engine::Emulated);
        // Where `@executable_path` points, and where a guest `dlopen` of a bare
        // library name looks.
        crate::dyld::set_executable_path(exe_path, program.report.root().map_or(0, |i| i.slide));
        // Where the guest's own `mach_header` is, for the guest's own sake.
        if let Some(root) = program.report.root() {
            crate::dyld::set_executable_header(root.base);
        }

        let cache = CodeCache::new(program.lifter_config_with(policy));
        // The calling thread becomes the guest's main thread: this is the stack
        // `argv` was laid out on, so it is the only one `main` can run on.
        crate::thread::install(crate::thread::ThreadState::new(stack));
        Ok(Vm {
            program: SharedProgram(program),
            cache,
            thunks: RwLock::new(ThunkTable::new()),
            thunk_view: AtomicPtr::new(core::ptr::null_mut()),
            thunk_count: AtomicUsize::new(0),
            view: GuestView::new(ranges),
            objc: AtomicPtr::new(core::ptr::null_mut()),
            objc_registration: Mutex::new(()),
        })
    }

    /// Realize the guest's Objective-C classes in the selected runtime.
    ///
    /// Idempotent, and deliberately *not* done in [`Vm::with_policy`].
    pub fn register_objc(&self) -> Result<&'static crate::objc::ObjcRuntime, VmError> {
        let _guard = self
            .objc_registration
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = self.objc() {
            return Ok(existing);
        }
        Ok(self.publish_objc(crate::objc::register(self)?))
    }

    /// Realize the classes of an image the guest `dlopen`ed, into the same runtime.
    ///
    /// A `Vm` that has never registered at all is left alone.
    pub fn register_loaded_objc(
        &self,
        report: &machish::report::LinkReport,
    ) -> Result<(), VmError> {
        let _guard = self
            .objc_registration
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(current) = self.objc() else {
            return Ok(());
        };
        let mut extended = current.clone();
        crate::objc::register_loaded(self, &mut extended, report)?;
        self.publish_objc(extended);
        Ok(())
    }

    /// Call the `+load` methods of an image [`Vm::register_loaded_objc`] just realized,
    /// returning how many ran.
    ///
    /// Deliberately outside that call's lock.
    pub fn run_loaded_load_methods(
        &self,
        report: &machish::report::LinkReport,
    ) -> Result<usize, VmError> {
        match self.objc() {
            Some(rt) => crate::objc::call_loaded_load_methods(self, rt, report),
            None => Ok(0),
        }
    }

    /// Leak `rt` and make it the runtime every dispatching thread reads.
    fn publish_objc(&self, rt: crate::objc::ObjcRuntime) -> &'static crate::objc::ObjcRuntime {
        let leaked: &'static crate::objc::ObjcRuntime = Box::leak(Box::new(rt));
        self.objc.store(
            leaked as *const _ as *mut crate::objc::ObjcRuntime,
            Ordering::Release,
        );
        // And the same reference where a caller with no `Vm` can find it.
        crate::objc::publish(leaked);
        leaked
    }

    /// The registered Objective-C classes, if [`Vm::register_objc`] has run.
    pub fn objc(&self) -> Option<&'static crate::objc::ObjcRuntime> {
        let published = self.objc.load(Ordering::Acquire);
        // SAFETY: non-null only after `publish_objc` stored a leaked runtime,
        // which is never freed and never mutated after publication - an
        // extending pass builds a separate copy. See the field note.
        (!published.is_null()).then(|| unsafe { &*published })
    }

    /// A class pointer on its way *out* to the runtime.
    ///
    /// Identity before [`Vm::register_objc`] has run, when there is nothing to
    /// translate against.
    pub fn objc_host_class(&self, va: u64) -> u64 {
        self.objc().map_or(va, |rt| rt.host_or(va))
    }

    /// A class pointer on its way back *in*.
    pub fn objc_guest_class(&self, host: u64) -> u64 {
        self.objc().map_or(host, |rt| rt.guest_or(host))
    }

    /// A `Protocol *` on its way out to the runtime.
    ///
    /// [`Vm::objc_host_class`] for protocols.
    pub fn objc_host_protocol(&self, va: u64) -> u64 {
        self.objc().map_or(va, |rt| rt.host_protocol_or(va))
    }

    /// A guest pointer on its way out where the encoding cannot say what it is.
    ///
    /// A method's type encoding spells `id`, `Class` and `Protocol *` all as
    /// `@`, so `-conformsToProtocol:` is indistinguishable from any other
    /// one-object message until the argument is looked up. Classes first, then
    /// protocols
    ///
    /// Only messages need this. A runtime entry point is described by
    /// [`crate::objc::dispatch::RUNTIME`], which names which parameter is which.
    pub fn objc_host_id(&self, va: u64) -> u64 {
        let Some(rt) = self.objc() else {
            return va;
        };
        match rt.host_or(va) {
            same if same == va => rt.host_protocol_or(va),
            class => class,
        }
    }

    /// Guest memory, borrowed. A shim that needs to hold it clones it; everything on
    /// a per-call path borrows, because a clone is two atomic increments on a line
    /// every guest thread shares.
    pub fn view(&self) -> &GuestView {
        &self.view
    }

    /// How many regions have been translated so far.
    pub fn translated(&self) -> usize {
        self.cache.len()
    }

    /// What the shared code cache has been asked to do, and how much of it block
    /// linking spared it.
    pub fn cache_stats(&self) -> microlift::runtime::CacheStats {
        self.cache.stats()
    }

    /// `(installed, pending)` link cells. See [`crate::cache::CodeCache::cell_counts`].
    pub fn cell_counts(&self) -> (usize, usize) {
        self.cache.cell_counts()
    }

    /// Record `n` successors reached without this loop being asked.
    ///
    /// Counted here rather than inside the chain because the counter is the shared
    /// cache's and the chain runs guest code - see
    /// [`microlift::runtime::Entered::links`].
    pub fn note_links_followed(&self, n: usize) {
        self.cache.note_links_followed(n);
    }

    /// `(argc, argv, envp, apple)` for the entry point and the initializers.
    pub fn main_args(&self) -> (u64, u64, u64, u64) {
        crate::thread::current().map_or((0, 0, 0, 0), |t| t.main_args())
    }

    /// The `rsp` an `LC_UNIXTHREAD` entry point is entered with - the word holding
    /// `argc`, with `argv`, `envp` and `apple` laid out above it.
    ///
    /// Same thread restriction as [`Vm::main_args`], and for the same reason.
    pub fn kernel_sp(&self) -> Option<u64> {
        crate::thread::current().and_then(|t| t.kernel_sp())
    }

    /// Call a guest function with up to six integer arguments, and return whatever it
    /// leaves in `rax`.
    pub fn call(&self, entry: u64, args: &[u64]) -> Result<u64, VmError> {
        let mut ctx = RegisterContext::new();
        let regs = [
            &mut ctx.rdi,
            &mut ctx.rsi,
            &mut ctx.rdx,
            &mut ctx.rcx,
            &mut ctx.r8,
        ];
        for (slot, &arg) in regs.into_iter().zip(args) {
            *slot = arg;
        }
        if let Some(&sixth) = args.get(5) {
            ctx.r9 = sixth;
        }
        self.call_with(entry, ctx)
    }

    /// As [`Vm::call`], with the whole initial register context supplied - the
    /// form a thunk needs, since it has floating-point arguments to place too.
    pub fn call_with(&self, entry: u64, ctx: RegisterContext) -> Result<u64, VmError> {
        self.call_with_stack(entry, ctx, &[]).map(|r| r.rax)
    }

    /// As [`Vm::call_with`], with arguments the guest's ABI puts on its *stack*.
    ///
    /// Registers alone cannot express every call, and the case that forces this is not
    /// exotic: an aggregate SysV classifies MEMORY travels there.
    pub fn call_with_stack(
        &self,
        entry: u64,
        ctx: RegisterContext,
        stack: &[u8],
    ) -> Result<Returned, VmError> {
        // A thread the guest spawned reaches here first, through a thunk, with
        // no stack of its own yet.
        let state = crate::thread::ensure()?;

        // Below the innermost live frame, past the red zone the guest's ABI
        // lets a leaf function use without adjusting `rsp`.
        let top = (state.rsp.get() - 128) & !0xf;
        // Padded to a multiple of sixteen so the return address below it leaves
        // the callee at the alignment the ABI promises.
        let padded = (stack.len() as u64).next_multiple_of(16);
        let args_at = top - padded;
        if !stack.is_empty() {
            self.view
                .store(args_at, stack)
                .map_err(|source| VmError::Stack { source })?;
        }
        let rsp = args_at - 8;
        self.view
            .store(rsp, &crate::retstub::address().to_le_bytes())
            .map_err(|source| VmError::Stack { source })?;
        self.enter(state, entry, ctx, rsp)
    }

    /// Enter guest code at `entry` with `rsp` exactly as given, and **no return
    /// address**.
    pub fn jump_to(&self, entry: u64, ctx: RegisterContext, rsp: u64) -> Result<Returned, VmError> {
        let state = crate::thread::ensure()?;
        self.enter(state, entry, ctx, rsp)
    }

    /// Run the guest from `entry` with `rsp`, and service its exits until the entry
    /// this made is finished.
    ///
    /// Where the per-entry bookkeeping lives - depth, the published register context,
    /// the `setjmp` identity - so that the two ways in, a call with a return address
    /// and a jump without one, differ only in who wrote the word at `rsp`.
    fn enter(
        &self,
        state: &crate::thread::ThreadState,
        entry: u64,
        mut ctx: RegisterContext,
        rsp: u64,
    ) -> Result<Returned, VmError> {
        if state.depth.get() >= MAX_DEPTH {
            return Err(VmError::TooDeep { pc: entry });
        }
        ctx.rsp = rsp;
        ctx.rip = entry;
        // The guest reads thread-specific data off `%gs` without calling
        // anything, so the base has to be right before the first region runs and
        // it is a fact about *this* thread - which is why it is set here, in the
        // one place every entry into guest code passes through, rather than
        // wherever a context happens to be built. See [`crate::thread`].
        ctx.gs_base = crate::thread::tsd_base();
        // The two events lifted code services *inline*, by calling the host
        // rather than exiting to it: a `syscall`, and an instruction the
        // recompiler does not lift. Set here for the reason `gs_base` is - this
        // is the one place every entry into guest code passes through - and
        // because microlift's own defaults abort rather than return.
        ctx.callbacks = crate::bridge::callbacks();
        // The two tables lifted code reads: the block-link rows a probe hashes into
        // and the ring a `call` pushes its return address onto. Facts about *this
        // thread*, for the same reason and in the same place as `gs_base`, and set
        // through microlift's own `ThreadLinks::attach` so that a field added to the
        // pair is not a store this crate has to remember to add.
        state.code().attach(&mut ctx);
        // Resumed where the entry below this one left off rather than at the top, so a
        // callback's first `call` does not overwrite the frame its caller will return
        // through. Read from that entry's own context, which is still the published
        // one here - `publish_context` below is what displaces it.
        microlift::runtime::ThreadLinks::resume_call_return(&mut ctx, state.published_context());

        let saved_rsp = state.rsp.get();
        state.rsp.set(rsp);
        state.depth.set(state.depth.get() + 1);
        // This entry gets an identity of its own, which `setjmp` records and
        // `longjmp` checks.
        let outer_entry = state.enter();
        // Publish the register context for the duration, for the same shape of
        // reason and a different reader.
        let outer_context = state.publish_context(&mut ctx);
        // Publish the VM for the duration: a thunk called from inside a host
        // function has no other way to find it. See `crate::thunk`.
        let result = crate::thunk::with_current(self, || self.run_loop(state, &mut ctx));
        state.restore_context(outer_context);
        state.leave(outer_entry);
        state.depth.set(state.depth.get() - 1);
        state.rsp.set(saved_rsp);
        // Everything but `rax` is read from the context the loop just finished
        // with, which is where the guest left it.
        result.map(|rax| Returned {
            rax,
            rdx: ctx.rdx,
            xmm0: ctx.xmm[0][0],
            xmm1: ctx.xmm[1][0],
        })
    }

    /// Enter regions and service exits until the call that started this returns.
    fn run_loop(
        &self,
        state: &crate::thread::ThreadState,
        ctx: &mut RegisterContext,
    ) -> Result<u64, VmError> {
        // How control left the previous region, kept for exactly one step. It is
        // what makes a lost `rip` diagnosable: see [`Transfer`].
        let mut from: Option<Transfer> = None;
        let mut ticks: u64 = 0;
        loop {
            let pc = ctx.rip;

            // `ROSEMARY_TRACE_PC=n` reports where the guest is every `n` region
            // entries, as `image+offset`.
            if let Some(every) = trace_interval() {
                ticks += 1;
                if ticks.is_multiple_of(every) {
                    let at = self
                        .program
                        .locate(pc)
                        .unwrap_or_else(|| format!("{pc:#x}"));
                    // The two lookups are reported because a *livelock* is what
                    // this exists to find, and a livelock here is always one of
                    // them matching an address it should not.
                    let by_site = self.program.import_at(pc).map(|s| s.symbol.as_str());
                    let by_target = self.program.import_by_target(pc).map(|s| s.symbol.as_str());
                    // The previous exit is on the line because a livelock is
                    // always a region that leaves the way it came, and which
                    // exit it leaves by is the difference between "the guest is
                    // spinning" and "this runtime is".
                    eprintln!(
                        "rosemary: trace {ticks}: {at} rsp={:#x} import_at={by_site:?} \
                         by_target={by_target:?} from={:?}",
                        ctx.rsp,
                        from.as_ref().map(|f| (f.exit, f.code, f.pc)),
                    );
                }
            }

            // A write to generated code retires what was lifted from it, here and
            // in every thread's tables, before anything is looked up. See
            // [`crate::smc`].
            if crate::smc::dirty() {
                self.cache.drain_writes();
            }
            state.code().catch_up();

            let mut missed = false;
            let translation = match state.code().get(pc) {
                Some(hit) => {
                    // The window answered, so nothing would otherwise notice that
                    // this thread can no longer link here. See
                    // [`CodeWindow::relink`].
                    state.code().relink(pc, &hit);
                    hit
                }
                None => {
                    missed = true;

                    // Asked before the import lookups below, and not only to save
                    // the second lookup on a hit: a translation existing at `pc`
                    // settles that `pc` is guest code, and the two import questions
                    // together cost more than this one does. They are a bloom
                    // filter, a bounds check and a binary search each, on a path the
                    // guest takes two million times a second - measured at 1.1% of a
                    // Subnautica steady state against 0.5% for this lookup.
                    //
                    // An address can only stop being guest code by becoming an
                    // import, and every way that happens gives an address that was
                    // *nothing* beforehand - the same argument [`crate::dispatch`]
                    // makes for caching a verdict at all.
                    let known = self.cache.lookup(pc);

                    // Control *jumped* to an import rather than calling one.
                    let site = match known {
                        Some(_) => None,
                        None => self
                            .program
                            .import_at(pc)
                            .or_else(|| self.program.import_by_target(pc)),
                    };
                    if let Some(site) = site {
                        crate::stats::record_tail_import();
                        ctx.imported_function_va = pc;
                        self.service_site_as(site, ctx, true)?;
                        // Unless the call transferred control instead of
                        // returning.
                        if state.took_redirect() {
                            continue;
                        }
                        let mut ret = [0u8; 8];
                        self.view
                            .read(ctx.rsp, &mut ret)
                            .map_err(|source| VmError::Stack { source })?;
                        ctx.rsp = ctx.rsp.wrapping_add(8);
                        let ret = u64::from_le_bytes(ret);
                        if crate::retstub::is_exit(ret) {
                            return Ok(ctx.rax);
                        }
                        ctx.rip = ret;
                        continue;
                    }

                    // The lock is dropped before guest code runs, which is
                    // what keeps a callback re-entering the VM on this same
                    // thread from deadlocking against its own caller.
                    let lifted = match known {
                        Some(hit) => hit,
                        None => self
                            .cache
                            .get_or_lift(&self.program, pc)
                            .map_err(|e| match e {
                                VmError::NoCode { pc, from: None } => VmError::NoCode {
                                    pc,
                                    from: from.map(|mut t| {
                                        t.at = self.program.locate(t.pc);
                                        t
                                    }),
                                },
                                other => other,
                            })?,
                    };
                    state.code().put(pc, &lifted);
                    lifted
                }
            };
            ctx.rip = pc;
            // Where a region that ends in a `ret`, a jump or a call out of
            // itself looks its own successor up, instead of coming back here to
            // have this loop do it.
            ctx.link_table = state.code().link_table();
            crate::stats::record_entry(missed);
            let exit = crate::cache::enter(&translation, ctx);
            state.rsp.set(ctx.rsp);
            // Reaching here at all is what makes this a probe miss: a linked exit
            // continues in guest code and never comes back to this loop.
            let kind = classify(exit, ctx.exit_code);
            crate::stats::record_exit(kind);
            crate::stats::record_probe_miss(kind);
            from = Some(Transfer {
                pc,
                exit,
                code: ctx.exit_code,
                rsp: ctx.rsp,
                // Left empty here: this runs on every exit, and rendering a name
                // per exit would be a trace. Filled in where it is reported.
                at: None,
            });

            if let Some(done) = self.step(ctx, exit)? {
                return Ok(done);
            }
            if let Some(err) = state.pending.borrow_mut().take() {
                return Err(err);
            }
        }
    }

    /// Service one VM exit. `Ok(Some(v))` means the call is finished with `v`.
    ///
    /// ## What each arm assumes about the stack
    ///
    /// Microlift leaves the guest stack exactly as the instruction would have.
    fn step(&self, ctx: &mut RegisterContext, exit: ExitReason) -> Result<Option<u64>, VmError> {
        match exit {
            // The guest returned to the stub this VM pushed and executed the
            // one `int3` there: the function it called is finished.
            ExitReason::Breakpoint if crate::retstub::is_exit(ctx.rip) => Ok(Some(ctx.rax)),

            // Any other breakpoint is the guest's own `int3` - a debugger trap,
            // an assertion, a sanitizer's - and what it wants is a `SIGTRAP`
            // delivered to a handler this runtime has no path to yet.
            ExitReason::Breakpoint => panic!(
                "rosemary: guest breakpoint at {:#x}{}, which this runtime cannot deliver",
                ctx.rip,
                self.program
                    .locate(ctx.rip)
                    .map(|at| format!(" ({at})"))
                    .unwrap_or_default()
            ),

            // Control fell out of the *bottom* of the region.
            ExitReason::None => Ok(None),

            // Control left the region: a jump, a return into a region not yet
            // translated, or a call to one.
            ExitReason::UnknownJump => match TransferKind::try_from(ctx.exit_code) {
                Ok(TransferKind::DirectCall) => {
                    ctx.rip = ctx.imported_function_va;
                    Ok(None)
                }
                // Two of the kinds are not about the guest's control flow at
                // all - they are the translation saying it cannot serve this
                // address, either because the instruction there runs past the
                // bytes it was given or because its dispatch was never offered
                // that address.
                Ok(TransferKind::RegionTail | TransferKind::NoResumePoint) => {
                    crate::stats::record_tail_exit();
                    Ok(None)
                }
                // A tail `jmp` through a register or a memory operand, whose
                // target may be **one of this runtime's own thunks**.
                Ok(TransferKind::IndirectJump) => {
                    if !self.program.is_guest_address(ctx.rip) {
                        if let Some(guest) = self.guest_behind_thunk(ctx.rip) {
                            ctx.rip = guest;
                        }
                    }
                    Ok(None)
                }
                Ok(_) => Ok(None),
                Err(()) => Err(VmError::Trap {
                    reason: exit,
                    code: ctx.exit_code,
                    pc: ctx.rip,
                }),
            },

            ExitReason::ImportedFunctionCall => {
                self.service_import(ctx, false)?;
                Ok(None)
            }

            // The answer is discarded here and load-bearing in
            // [`Vm::dispatch_indirect_bridged`].
            ExitReason::IndirectCall => {
                self.dispatch_indirect(ctx, crate::thread::current())?;
                Ok(None)
            }

            // The guest asked what CPU it is running on.
            ExitReason::Cpuid => {
                let [eax, ebx, ecx, edx] = cpuid(ctx.rax as u32, ctx.rcx as u32);
                // `CPUID` writes the 32-bit registers, which zero-extend.
                ctx.rax = eax as u64;
                ctx.rbx = ebx as u64;
                ctx.rcx = ecx as u64;
                ctx.rdx = edx as u64;
                Ok(None)
            }

            _ => Err(VmError::Trap {
                reason: exit,
                code: ctx.exit_code,
                pc: ctx.rip,
            }),
        }
    }

    /// Call the host function an intercepted import resolved to.
    fn service_import(&self, ctx: &mut RegisterContext, jumped: bool) -> Result<(), VmError> {
        let va = ctx.imported_function_va;
        let Some(site) = self.program.import_at(va) else {
            return Err(AbiError::NoSignature {
                symbol: format!("<unregistered {va:#x}>"),
            }
            .into());
        };
        self.service_site_as(site, ctx, jumped)
    }

    /// Service an import from a fast-call bridge rather than from an exit.
    pub(crate) fn service_bridged_import(
        &self,
        import_va: u64,
        ctx: &mut RegisterContext,
    ) -> Result<bool, VmError> {
        let Some(site) = self.program.import_at(import_va) else {
            return Err(AbiError::NoSignature {
                symbol: format!("<unregistered {import_va:#x}>"),
            }
            .into());
        };
        self.service_site_as(site, ctx, false)?;
        Ok(self.redirected())
    }

    /// [`Vm::dispatch_indirect`] from a fast-call bridge.
    pub(crate) fn dispatch_indirect_bridged(
        &self,
        target: u64,
        ctx: &mut RegisterContext,
        state: Option<&crate::thread::ThreadState>,
    ) -> Result<bool, VmError> {
        // `dispatch_indirect` reads the callee from the context, and microlift
        // has already parked it there. Set explicitly rather than relied upon.
        ctx.imported_function_va = target;
        self.dispatch_indirect(ctx, state)
    }

    /// Whether the call just serviced moved `rip` instead of returning a value,
    /// clearing the flag.
    fn redirected(&self) -> bool {
        crate::thread::current().is_some_and(|s| s.took_redirect())
    }

    /// [`Vm::service_import`] for a site already looked up.
    fn service_site_as(
        &self,
        site: &crate::program::ImportSite,
        ctx: &mut RegisterContext,
        jumped: bool,
    ) -> Result<(), VmError> {
        // Both flags are set here rather than read here, so each always
        // describes *this* call.
        if let Some(state) = crate::thread::current() {
            state.took_redirect();
            state.set_entered_by_jump(jumped);
        }
        self.call_site(site, ctx)
    }

    /// Marshal and perform the call `site` describes.
    fn call_site(
        &self,
        site: &crate::program::ImportSite,
        ctx: &mut RegisterContext,
    ) -> Result<(), VmError> {
        // Every question below is a string lookup or a region search whose answer
        // never changes for a site, so it is asked once and kept on the site.
        let plan = site.plan(|va| self.program.is_guest_address(va));
        // An import that resolved into another **guest** image, which happens
        // as soon as one exists.
        if plan.resolves_to_guest {
            return self.transfer_to_guest(site.resolved, ctx);
        }
        let mut mem = &self.view;
        crate::abi::call_planned(plan.call, &site.symbol, site.resolved, ctx, &mut mem, self)?;
        Ok(())
    }

    /// Continue the guest at `target` as if the import just serviced had tail-called
    /// it, for a shim that must not leave a host frame behind - see
    /// `objc::dispatch::exception_throw`.
    pub(crate) fn tail_call(&self, target: u64, ctx: &mut RegisterContext) -> Result<(), VmError> {
        self.transfer_to_guest(target, ctx)
    }

    /// Continue the guest at `target`, as the intercepted transfer would have.
    fn transfer_to_guest(&self, target: u64, ctx: &mut RegisterContext) -> Result<(), VmError> {
        let Some(state) = crate::thread::current() else {
            return Err(AbiError::Refused {
                symbol: format!("<guest import at {target:#x}>"),
                reason: "a guest-to-guest import outside a running guest thread",
            }
            .into());
        };
        if !state.entered_by_jump() {
            let rsp = ctx.rsp.wrapping_sub(8);
            self.view
                .store(rsp, &ctx.rip.to_le_bytes())
                .map_err(|source| VmError::Stack { source })?;
            ctx.rsp = rsp;
        }
        ctx.rip = target;
        // The stack pointer moved without an exit reporting it, and `rip` is
        // now a destination rather than a resume point.
        state.rsp.set(ctx.rsp);
        state.redirect();
        Ok(())
    }

    /// Stand in for the `CALL` microlift declined to perform: push `resume` and
    /// continue at `target`.
    ///
    /// The push is onto this thread's own guest stack, which is one range it already
    /// knows, so `state` is asked before [`GuestView`] is: the general answer is a
    /// binary search over every mapped region, and after the lookups above it became
    /// cheap this was the largest thing left on the path. A guest that has switched
    /// stacks - a fibre, a `makecontext` - answers no and takes the search.
    fn enter_guest_callee(
        &self,
        target: u64,
        resume: u64,
        ctx: &mut RegisterContext,
        state: Option<&crate::thread::ThreadState>,
    ) -> Result<(), VmError> {
        let rsp = ctx.rsp.wrapping_sub(8);
        // `[rsp, ctx.rsp)` inside the stack, stated so that an `rsp` below eight
        // cannot wrap into a yes.
        let on_own_stack = state.is_some_and(|s| {
            let stack = s.stack_range();
            ctx.rsp >= stack.start.wrapping_add(8) && ctx.rsp <= stack.end
        });
        if on_own_stack {
            // SAFETY: `[rsp, rsp+8)` is inside the range this runtime mapped for
            // this thread's guest stack, and guest addresses are host addresses.
            // This is `GuestView::store`'s own store, with the check it makes
            // answered by a range compare instead of a search.
            unsafe { std::ptr::write_unaligned(rsp as usize as *mut u64, resume.to_le()) };
        } else {
            self.view
                .store(rsp, &resume.to_le_bytes())
                .map_err(|source| VmError::Stack { source })?;
        }
        ctx.rsp = rsp;
        ctx.rip = target;
        Ok(())
    }

    /// Decide what an indirect call through a runtime-computed address means.
    ///
    /// `state` is this thread's, read once by the caller: the fast path needs it for
    /// the cache and the slow one for the redirect flag, and reading it here as well
    /// would be a second `pthread_getspecific` per call.
    fn dispatch_indirect(
        &self,
        ctx: &mut RegisterContext,
        state: Option<&crate::thread::ThreadState>,
    ) -> Result<bool, VmError> {
        let callee = ctx.imported_function_va;
        let resume = ctx.rip;

        // Where this address went last time, which is where it goes now. Ahead of
        // every lookup below because it stands in for all four of them - see
        // [`crate::dispatch`].
        if let Some(known) = state
            && let Some(target) = known.indirect().get(callee)
        {
            crate::stats::record_dispatch(true);
            self.enter_guest_callee(target, resume, ctx, state)?;
            return Ok(true);
        }
        crate::stats::record_dispatch(false);

        // A pointer read out of an import slot: the same host function the
        // import would have reached, so service it identically.
        if self.program.import_at(callee).is_some() {
            self.service_import(ctx, false)?;
            return Ok(self.redirected());
        }

        // One of our own thunks, handed out earlier and now called by the guest
        // through a variable.
        let target = self.guest_behind_thunk(callee).unwrap_or(callee);

        if self.program.is_guest_address(target) {
            self.enter_guest_callee(target, resume, ctx, state)?;
            // Only this verdict is remembered, and only once it has been reached the
            // long way round.
            if state.is_some_and(|s| s.indirect().put(callee, target)) {
                crate::stats::record_dispatch_eviction();
            }
            return Ok(true);
        }

        // A host address the guest *loaded from an import slot* and then called
        // through a register, rather than at a `call` site the lifter could
        // match - a descriptor `tls::register` declined still calls its
        // `__tlv_bootstrap` this way.
        if let Some(site) = self.program.import_by_target(callee) {
            // A host pointer that reached the guest by neither of the routes
            // [`crate::trampoline`] substitutes - not out of a slot, not out of
            // `dlsym`, but returned by some host function the guest then called
            // through.
            if let Some(stub) = crate::trampoline::for_site(site) {
                self.enter_guest_callee(stub, resume, ctx, state)?;
                return Ok(true);
            }
            // `service_site_as`, not `call_site`.
            self.service_site_as(site, ctx, false)?;
            return Ok(self.redirected());
        }

        Err(VmError::UnknownIndirectCall { callee, resume })
    }

    /// A native arm64 address that re-enters the guest at `guest_va`.
    ///
    /// The generic trampoline forwards six integer and eight floating-point registers
    /// because a bare function pointer says nothing about its own shape.
    pub fn typed_thunk_for(
        &self,
        guest_va: u64,
        signature: crate::thunk::ThunkSignature,
    ) -> Result<u64, VmError> {
        // A generic thunk is not ABI-compatible, and a guest address may be
        // used with more than one callback prototype. Typed thunks are not in
        // the published view, but a read lock does not stall other readers.
        let cached = self
            .thunks
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .typed_host_for(guest_va, &signature);
        if let Some(existing) = cached {
            return Ok(existing);
        }
        // Compiled outside the lock; a thread that lost the race to insert
        // drops its copy, which nothing has seen yet.
        let thunk = crate::thunk::build_typed(guest_va, signature.clone())?;
        let mut table = self.thunks.write().unwrap_or_else(|e| e.into_inner());
        let (host, kept) = table.insert_typed(signature, thunk);
        if kept {
            self.publish_thunk_view(&table);
        }
        drop(table);
        // As in `thunk_for`: handing the pointer out is the demand signal.
        let _ = self.cache.get_or_lift(&self.program, guest_va);
        Ok(host)
    }

    pub fn thunk_for(&self, guest_va: u64) -> Result<u64, VmError> {
        if let Some(existing) = self.host_thunk_for(guest_va) {
            return Ok(existing);
        }
        // As for typed thunks: built outside the lock, and the first one
        // inserted is the one every caller gets.
        let thunk = crate::thunk::build(guest_va)?;
        let host = self.insert_thunk(thunk);

        // Translate the callback now, while translating is possible.
        let _ = self.cache.get_or_lift(&self.program, guest_va);
        Ok(host)
    }

    /// How many host thunks exist, including typed variants of one guest address.
    pub fn thunks(&self) -> usize {
        self.thunks.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether `va` is a thunk this VM handed out.
    pub fn guest_behind_thunk(&self, va: u64) -> Option<u64> {
        self.ask_thunks(|view| view.guest_for(va), |table| table.guest_for(va))
    }

    /// The thunk already built for a guest address, if there is one.
    fn host_thunk_for(&self, guest_va: u64) -> Option<u64> {
        self.ask_thunks(
            |view| view.host_for(guest_va),
            |table| table.host_for(guest_va),
        )
    }

    /// Ask the published snapshot, and fall back to the lock only when a miss might be
    /// stale.
    ///
    /// The published half is what every indirect call consults; see
    /// [`crate::thunk::ThunkView`] for why a miss cannot simply be believed.
    fn ask_thunks(
        &self,
        from_view: impl Fn(&crate::thunk::ThunkView) -> Option<u64>,
        from_table: impl Fn(&ThunkTable) -> Option<u64>,
    ) -> Option<u64> {
        let published = self.thunk_view.load(Ordering::Acquire);
        if !published.is_null() {
            // SAFETY: the pointer was leaked by `publish_thunks` and is never
            // freed, so it stays valid for the life of the process.
            let view = unsafe { &*published };
            if let Some(hit) = from_view(view) {
                return Some(hit);
            }
            if view.count == self.thunk_count.load(Ordering::Acquire) {
                return None;
            }
        } else if self.thunk_count.load(Ordering::Acquire) == 0 {
            // Nothing has ever crossed outward, which is the whole answer for a
            // program that hands the host no function pointers at all.
            return None;
        }
        from_table(&self.thunks.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// Add a thunk and republish the snapshot, both under the one lock.
    fn insert_thunk(&self, thunk: crate::thunk::Thunk) -> u64 {
        let mut table = self.thunks.write().unwrap_or_else(|e| e.into_inner());
        let (host, kept) = table.insert(thunk);
        if kept {
            self.publish_thunk_view(&table);
        }
        host
    }

    fn publish_thunk_view(&self, table: &ThunkTable) {
        let view = Box::leak(Box::new(table.view()));
        let count = view.count;
        self.thunk_view.store(view, Ordering::Release);
        // After the view, so a reader that sees this count has already had the
        // chance to see the view that produced it.
        self.thunk_count.store(count, Ordering::Release);
        // An address that meant nothing a moment ago is now a thunk. See
        // [`crate::dispatch::invalidate`].
        crate::dispatch::invalidate();
    }

    /// Record an error raised inside a host->guest callback, on the thread it happened
    /// on.
    pub(crate) fn set_pending(&self, err: VmError) {
        let Some(state) = crate::thread::current() else {
            return;
        };
        let mut slot = state.pending.borrow_mut();
        if slot.is_none() {
            say(&err);
            *slot = Some(err);
        }
    }

    /// Take whatever error a callback left behind on this thread.
    pub fn take_pending(&self) -> Option<VmError> {
        crate::thread::current().and_then(|s| s.pending.borrow_mut().take())
    }
}

/// The highest basic leaf this runtime answers. Leaves above it read as zero.
const MAX_BASIC_LEAF: u32 = 1;

/// The highest extended leaf answered: the last quarter of the brand string.
const MAX_EXTENDED_LEAF: u32 = 0x8000_0004;

/// The processor brand string, `NUL`-padded to the 48 bytes leaves
/// `0x80000002..=0x80000004` spell out.
const BRAND: [u8; 48] = brand();

const fn brand() -> [u8; 48] {
    let name = b"rosemary (microlift x86-64 on arm64) @ 2.67GHz";
    let mut out = [0u8; 48];
    let mut i = 0;
    while i < name.len() {
        out[i] = name[i];
        i += 1;
    }
    out
}

fn cpuid(leaf: u32, subleaf: u32) -> [u32; 4] {
    /// A four-byte slice of the brand string, as the little-endian word the
    /// register holds.
    fn brand_word(offset: usize) -> u32 {
        u32::from_le_bytes([
            BRAND[offset],
            BRAND[offset + 1],
            BRAND[offset + 2],
            BRAND[offset + 3],
        ])
    }

    match leaf {
        // Vendor String: "GenuineIntel"
        0x0 => [
            MAX_BASIC_LEAF,
            u32::from_le_bytes(*b"Genu"),
            u32::from_le_bytes(*b"ntel"),
            u32::from_le_bytes(*b"ineI"),
        ],

        // Family / Model / Stepping & Standard Features
        0x1 => {
            // Family 6, Model 44 (0x2C), Stepping 0.
            let version = 0x0002_06C0;

            // Cache line size (64 bytes -> 8 units of 8-bytes) in bits 15:8
            let misc = 8 << 8;

            // FPU | TSC | CX8 | CMOV | SSE | SSE2
            let edx = (1 << 0) | (1 << 4) | (1 << 8) | (1 << 15) | (1 << 25) | (1 << 26);

            // SSE3 (bit 0) | SSSE3 (bit 9) | SSE4.1 (bit 19) | SSE4.2 (bit 20) | AES-NI (bit 25) | Hypervisor (bit 31)
            let ecx = (1 << 0) | (1 << 9) | (1 << 19) | (1 << 20) | (1 << 25) | (1 << 31);

            [version, misc, ecx, edx]
        }

        // Leaf 4: Deterministic Cache Parameters
        0x4 => match subleaf {
            // L1 Data Cache (128 KB, 64-byte line, 8-way)
            0 => {
                let eax = (1 << 0) | (1 << 5) | (1 << 8); // Type: Data (1), Level: 1, Self Initializing
                let ebx = (63 << 0) | (0 << 12) | (7 << 22); // Line size (63+1=64), Partitions (0+1=1), Ways (7+1=8)
                let ecx = 255; // Sets (255+1=256) -> 256 * 64 * 1 * 8 = 128 KB
                let edx = 0;
                [eax, ebx, ecx, edx]
            }
            // L1 Instruction Cache (128 KB, 64-byte line, 8-way)
            1 => {
                let eax = (2 << 0) | (1 << 5) | (1 << 8); // Type: Instruction (2), Level: 1, Self Initializing
                let ebx = (63 << 0) | (0 << 12) | (7 << 22); // Line size 64, 8-way
                let ecx = 255; // Sets 256 -> 128 KB
                let edx = 0;
                [eax, ebx, ecx, edx]
            }
            // L2 Unified Cache (8192 KB / 8 MB, 64-byte line, 16-way)
            2 => {
                let eax = (3 << 0) | (2 << 5) | (1 << 8); // Type: Unified (3), Level: 2, Self Initializing
                let ebx = (63 << 0) | (0 << 12) | (15 << 22); // Line size 64, Ways (15+1=16)
                let ecx = 8191; // Sets (8191+1=8192) -> 8192 * 64 * 1 * 16 = 8192 KB
                let edx = 0;
                [eax, ebx, ecx, edx]
            }
            // No more caches
            _ => [0, 0, 0, 0],
        },

        // Leaf 7: Structured Extended Feature Flags
        0x7 => [0, 0, 0, 0], // Sub-leaf 0: No AVX2, BMI, AVX-512, SHA, etc.

        // Extended Leaves
        0x8000_0000 => [MAX_EXTENDED_LEAF, 0, 0, 0],

        // Extended Processor Info (64-bit Long Mode enabled)
        0x8000_0001 => [0, 0, 0, 1 << 29],

        // Brand String across leaves 0x80000002 to 0x80000004
        0x8000_0002..=0x8000_0004 => {
            let base = (leaf - 0x8000_0002) as usize * 16;
            [
                brand_word(base),
                brand_word(base + 4),
                brand_word(base + 8),
                brand_word(base + 12),
            ]
        }

        _ => [0, 0, 0, 0],
    }
}

impl crate::exec::Execution for Vm {
    fn engine(&self) -> crate::exec::Engine {
        crate::exec::Engine::Emulated
    }

    fn report(&self) -> &machish::report::LinkReport {
        &self.program.report
    }

    fn main_args(&self) -> (u64, u64, u64, u64) {
        Vm::main_args(self)
    }

    fn call(&self, entry: u64, args: &[u64]) -> Result<u64, VmError> {
        Vm::call(self, entry, args)
    }

    /// The emulated backend extends the snapshot this `Vm` holds. No
    /// `rewrite_refs` pass: a send from an emulated image is intercepted and
    /// `dispatch` translates the receiver, so the guest's own class references
    /// never have to hold a host pointer.
    fn realize_loaded_objc(&self, report: &machish::report::LinkReport) -> Result<(), VmError> {
        match self.objc() {
            // Registered classes: extend that runtime, selectors included.
            Some(_) => Vm::register_loaded_objc(self, report),
            // A program with no Objective-C of its own never registered, so
            // there is no runtime to extend and no class of the program's a
            // loaded image could subclass. The selectors are still its own.
            None => crate::objc::unique_loaded_selectors(self, report).map(|_| ()),
        }
    }

    fn run_loaded_load_methods(
        &self,
        report: &machish::report::LinkReport,
    ) -> Result<usize, VmError> {
        Vm::run_loaded_load_methods(self, report)
    }

    fn prepare(&self) -> Result<(), VmError> {
        // Another task's `vm_protect` and `vm_write` reach this one's memory
        // from the service handler, past the shims: they keep the code cache's
        // books through these instead.
        crate::smc::warm();
        darwin::mach::local::set_protect_hook(crate::shims::vmmap::protect_hook);
        darwin::mach::local::set_write_hook(crate::smc::before_write);
        // The sampler and the overlay, neither of which a host without Mach and
        // Metal has. See the module list in `lib.rs`.
        #[cfg(target_os = "macos")]
        {
            crate::profile::start();
            crate::gui::install();
            crate::shims::machthread::warm();
        }
        let objc = self.register_objc()?;
        // `+load` before the C constructors of the same image, which is dyld's
        // order and the one a program that uses both depends on.
        crate::objc::call_load_methods(self, objc)?;
        Ok(())
    }

    fn enter_main(&self) -> Result<u64, VmError> {
        let entry = self
            .program
            .entry()
            .ok_or(VmError::NoCode { pc: 0, from: None })?;
        // Which of the two entry-point encodings the root image used decides how it
        // is reached, and the difference is not cosmetic.
        if self.program.entry_reads_stack() {
            let sp = self.kernel_sp().expect(
                "an LC_UNIXTHREAD program starts on the stack its arguments were laid out on",
            );
            return Ok(self.jump_to(entry, RegisterContext::new(), sp)?.rax);
        }
        let (argc, argv, envp, apple) = Vm::main_args(self);
        Vm::call(self, entry, &[argc, argv, envp, apple])
    }
}

impl crate::objc::Realizer for Vm {
    fn report(&self) -> &machish::report::LinkReport {
        &self.program.report
    }

    fn memory(&self) -> Box<dyn crate::abi::GuestMemory + '_> {
        Box::new(&self.view)
    }

    fn is_guest_address(&self, va: u64) -> bool {
        self.program.is_guest_address(va)
    }

    fn call(&self, f: u64, args: &[u64]) -> Result<u64, VmError> {
        Vm::call(self, f, args)
    }

    /// A trampoline built from the method's own type encoding, which is what
    /// makes this callback different from every other one: a host caller enters
    /// guest code through it. The untyped thunk is the fallback, and what it
    /// cannot carry is reported rather than silently dropped.
    fn imp_for(
        &self,
        m: &machish::extensions::objc::ObjcMethod,
    ) -> Result<(u64, Option<&'static str>), &'static str> {
        let typed = crate::objc::thunk_signature(m).and_then(|signature| {
            self.typed_thunk_for(m.imp, signature)
                .map_err(|_| "a host->guest thunk could not be built")
        });
        match typed {
            Ok(imp) => Ok((imp, None)),
            Err(reason) => Ok((
                self.thunk_for(m.imp)
                    .map_err(|_| "a host->guest thunk could not be built")?,
                Some(reason),
            )),
        }
    }

    /// The guest's own `objc_class`: a send to it is intercepted, and
    /// `crate::objc::dispatch` is what turns it into the registered one.
    fn class_value(&self, _rt: &crate::objc::ObjcRuntime, guest_va: u64) -> u64 {
        guest_va
    }
}

/// What a guest-held function pointer is on its way out to host code.
enum Outbound {
    /// Already callable by the host, and passed as it is.
    Host(u64),
    /// Guest code, which needs a thunk of the callee's chosen kind.
    Guest(u64),
}

impl Vm {
    /// Classify a pointer for [`crate::abi::Callbacks::substitute`] and its typed twin,
    /// so both apply the same rules.
    fn outbound(&self, va: u64) -> Outbound {
        if va == 0 {
            return Outbound::Host(0);
        }
        // A thunk the guest read back out of host-visible state. Its guest
        // target is what gets a thunk again: the one it came from may have a
        // different ABI from the one this callee calls through.
        if let Some(guest) = self.guest_behind_thunk(va) {
            return Outbound::Guest(guest);
        }
        // One of this runtime's own import stubs, which the guest is holding
        // because a slot it loaded was substituted for one.
        if let Some(host) = crate::trampoline::host_behind(va) {
            return Outbound::Host(host);
        }
        // Not guest code: a host pointer the guest is handing back, which the
        // host may compare against one it gave out.
        if !self.program.is_guest_address(va) {
            return Outbound::Host(va);
        }
        Outbound::Guest(va)
    }
}

fn thunk_refusal(symbol: &str, e: VmError) -> AbiError {
    AbiError::Refused {
        symbol: symbol.to_string(),
        reason: match e {
            VmError::Thunk { .. } => "a host->guest thunk could not be built",
            _ => "the guest function pointer could not be made callable",
        },
    }
}

impl crate::abi::Callbacks for Vm {
    fn substitute(&self, symbol: &str, va: u64) -> Result<u64, AbiError> {
        match self.outbound(va) {
            Outbound::Host(host) => Ok(host),
            Outbound::Guest(guest) => self.thunk_for(guest).map_err(|e| thunk_refusal(symbol, e)),
        }
    }

    fn substitute_typed(
        &self,
        symbol: &str,
        va: u64,
        signature: crate::thunk::ThunkSignature,
    ) -> Result<u64, AbiError> {
        match self.outbound(va) {
            Outbound::Host(host) => Ok(host),
            Outbound::Guest(guest) => self
                .typed_thunk_for(guest, signature)
                .map_err(|e| thunk_refusal(symbol, e)),
        }
    }

    /// A thunk handed back by host code becomes the guest pointer it stands for.
    fn unsubstitute(&self, va: u64) -> u64 {
        self.guest_behind_thunk(va).unwrap_or(va)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The binary search must agree with the linear scan it replaced, including on the
    /// two cases a `partition_point` is easy to get wrong.
    #[test]
    fn a_region_is_found_by_search_wherever_a_scan_would_find_it() {
        // Deliberately out of order on the way in, because `new` is what sorts
        // them and a caller has never been asked to.
        let ranges = vec![0x3000..0x3100, 0x1000..0x1100, 0x2000..0x2400];
        let view = GuestView::new(ranges.clone());
        for va in (0x0fff..0x3200u64).step_by(1) {
            let scan = ranges.iter().any(|r| r.contains(&va));
            assert_eq!(view.contains(va), scan, "disagreed at {va:#x}");
        }
        // And the ends specifically, spelled out so a regression names itself.
        assert!(view.contains(0x1000), "a region includes its start");
        assert!(!view.contains(0x1100), "a region excludes its end");
        assert!(!view.contains(0x0fff));
    }

    /// Finding the region `va` is in is not the same as the whole span fitting inside
    /// it.
    #[test]
    fn a_span_that_runs_past_its_region_does_not_count_as_inside_one() {
        let view = GuestView::new(vec![0x1000..0x1100, 0x4000..0x4100]);
        let fits = |va: u64, len: u64| view.region_of(va).is_some_and(|r| va + len <= r.end);
        assert!(
            fits(0x10f8, 8),
            "eight bytes at 0x10f8 end exactly at 0x1100"
        );
        assert!(!fits(0x10f8, 9), "nine run past the end of the region");
        // And a span may not bridge two regions, however close they look in a
        // sorted list.
        assert!(!fits(0x10ff, 0x2f02));
    }

    /// A guest reads the vendor before it reads anything else, and it reads it
    /// out of three registers in an order that is easy to transpose.
    #[test]
    fn leaf_zero_spells_the_vendor() {
        let [eax, ebx, ecx, edx] = cpuid(0, 0);
        let mut vendor = Vec::new();
        vendor.extend_from_slice(&ebx.to_le_bytes());
        vendor.extend_from_slice(&edx.to_le_bytes());
        vendor.extend_from_slice(&ecx.to_le_bytes());
        assert_eq!(&vendor, b"GenuineIntel");
        assert_eq!(eax, MAX_BASIC_LEAF);
    }

    /// The whole point of the reply: SSE2 yes, everything microlift cannot lift no.
    #[test]
    fn leaf_one_advertises_the_baseline_and_no_more() {
        let [_, _, ecx, edx] = cpuid(1, 0);
        for (bit, name) in [
            (0, "fpu"),
            (4, "tsc"),
            (8, "cx8"),
            (25, "sse"),
            (26, "sse2"),
        ] {
            assert_ne!(edx & (1 << bit), 0, "{name} should be advertised");
        }
        // MMX and FXSR: baseline on real hardware, unimplemented here.
        for (bit, name) in [(23, "mmx"), (24, "fxsr")] {
            assert_eq!(edx & (1 << bit), 0, "{name} should not be advertised");
        }
        for (bit, name) in [
            (13, "cmpxchg16b"),
            (23, "popcnt"),
            (26, "xsave"),
            (27, "osxsave"),
            (28, "avx"),
        ] {
            assert_eq!(ecx & (1 << bit), 0, "{name} should not be advertised");
        }
        // Emulated, and saying so.
        assert_ne!(ecx & (1 << 31), 0, "hypervisor bit should be set");
    }

    /// The brand string is one 48-byte block split across twelve registers,
    /// and a misordered word would still look like a plausible string.
    #[test]
    fn brand_string_reassembles() {
        let mut bytes = Vec::new();
        for leaf in 0x8000_0002..=0x8000_0004 {
            for word in cpuid(leaf, 0) {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
        }
        assert_eq!(bytes.len(), 48);
        let text = String::from_utf8(bytes).expect("brand is ascii");
        assert!(
            text.starts_with("rosemary (microlift x86-64 on arm64)"),
            "{text}"
        );
        assert!(text.ends_with('\0'), "brand string is NUL-terminated");
        assert_eq!(cpuid(0x8000_0000, 0)[0], MAX_EXTENDED_LEAF);
    }

    /// Long mode, since the code asking is 64-bit and already running.
    #[test]
    fn extended_leaf_one_reports_long_mode() {
        assert_ne!(cpuid(0x8000_0001, 0)[3] & (1 << 29), 0);
    }

    /// Out of range in either space reads as zero rather than as some other
    /// leaf's feature words, and the sub-leaf changes nothing.
    #[test]
    fn unknown_leaves_are_zero() {
        assert_eq!(cpuid(MAX_BASIC_LEAF + 1, 0), [0, 0, 0, 0]);
        assert_eq!(cpuid(7, 0), [0, 0, 0, 0]);
        assert_eq!(cpuid(0x4000_0000, 0), [0, 0, 0, 0]);
        assert_eq!(cpuid(MAX_EXTENDED_LEAF + 1, 0), [0, 0, 0, 0]);
        assert_eq!(cpuid(1, 0), cpuid(1, 7));
    }
}
