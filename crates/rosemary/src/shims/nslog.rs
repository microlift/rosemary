use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::OnceLock;

use microlift::arch::x86_64::context::RegisterContext;

/// CoreFoundation, from whichever one this process is using.
///
/// Not `dlsym`: under `ROSEMARY_OBJC=builtin` the strings this module reads are
/// objects of *this tree's* runtime, and handing one to Apple's
/// `CFStringGetCString` would be a foreign structure read as a native one. The
/// frameworks register themselves with `darwin`, which is what makes one lookup
/// answer for both. See [`crate::frameworks`].
fn dlsym(name: &str) -> Option<*mut c_void> {
    crate::frameworks::symbol(name)
        .or_else(|| crate::host::symbol(name))
        .map(|addr| addr as usize as *mut c_void)
}
use crate::abi::{GuestMemory, Result};
use crate::objc::send::{Id, class, responds, sel, send0, send1};

use super::printf::{VarArgs, render};
use super::set_int_return;

/// `kCFStringEncodingUTF8`. A constant rather than an import: it is part of the
/// CoreFoundation *interface*, and one that has never changed.
const UTF8: u32 = 0x0800_0100;

/// The largest rendering of one object or format this shim will read back out of
/// CoreFoundation.
///
/// Totality, in the same spirit as [`crate::abi::MAX_CSTR`].
const MAX_TEXT: usize = 64 * 1024;

/// Service `NSLog`.
pub fn nslog(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let Some(out) = render_format(symbol, ctx, &*mem, ctx.rdi, 1)? else {
        // A null or unreadable format is the guest's error to have, and the
        // host's `NSLog` treats one as nothing to print.
        set_int_return(ctx, 0);
        return Ok(());
    };
    emit(&out);
    Ok(())
}

/// Service `NSRunAlertPanel`.
///
/// `NSInteger NSRunAlertPanel(NSString *title, NSString *msgFormat, NSString
/// *defaultButton, NSString *alternateButton, NSString *otherButton, ...)` - the same
/// two complications as `NSLog` (variadic, `NSString` format) with the format in the
/// *second* position rather than the first, so five integer registers are named rather
/// than one.
pub fn run_alert_panel(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    // Five named parameters, and the format is the second. Everything after
    // `otherButton` is the variadic tail the format consumes.
    let out = render_format(symbol, ctx, &*mem, ctx.rsi, 5)?.unwrap_or_default();
    let title = cf_string_bytes(ctx.rdi).unwrap_or_default();
    eprintln!(
        "rosemary: guest alert: {}: {}",
        String::from_utf8_lossy(&title),
        String::from_utf8_lossy(&out)
    );

    if crate::frameworks::using_builtin() {
        let [default, alternate, other] = [ctx.rdx, ctx.rcx, ctx.r8].map(cf_string_bytes);
        ctx.rax = appkit::alert::run_alert_panel(
            &title,
            &out,
            default.as_deref(),
            alternate.as_deref(),
            other.as_deref(),
        ) as u64;
        return Ok(());
    }

    // With the host Objective-C runtime in use, its AppKit understands the
    // guest's NSString button objects and can provide the full three-button UI.
    let host = dlsym(symbol);
    let (Some(host), Some(message)) = (host, escaped_string(&out)) else {
        // Nothing to show it with, so nobody answered: the guest's caller sees
        // the alternate button rather than the default, because a panel this
        // runtime never displayed cannot be reported as one the user agreed to.
        ctx.rax = NS_ALERT_ALTERNATE_RETURN;
        return Ok(());
    };
    // SAFETY: `symbol` resolved in an AppKit this process has loaded - the
    // guest's own window brought it in - and its declaration is the one above.
    // Called through a *variadic* prototype with no variadic arguments, which
    // is what the rendered format standing in for the guest's makes correct:
    // there is nothing left for it to convert. The buttons are the guest's own
    // `NSString`s, which outlive a modal call it is blocked in.
    let n = unsafe {
        let f: unsafe extern "C" fn(u64, u64, u64, u64, u64) -> i64 =
            core::mem::transmute(host as usize);
        f(ctx.rdi, message, ctx.rdx, ctx.rcx, ctx.r8)
    };
    // Which button the panel came back with is part of the diagnostic.
    eprintln!("rosemary: guest alert: answered {n}");
    // `NSInteger` is sixty-four bits wide, so the whole of `rax` carries it
    // rather than [`super::set_int_return`]'s `int`.
    ctx.rax = n as u64;
    Ok(())
}

/// `NSAlertAlternateReturn`, the panel's second button - conventionally the one
/// that declines whatever the default would have agreed to.
const NS_ALERT_ALTERNATE_RETURN: u64 = 0;

/// Render the `NSString` format at `fmt_obj` against the guest's variadic tail.
///
/// `fixed_int` is how many integer argument registers the *named* parameters have
/// already used, exactly as [`VarArgs::new`] means it:
///
/// `Ok(None)` means the format was not a readable string, which is the guest's error
/// and not this runtime's - a logging or formatting call must not be the thing that
/// stops a program.
pub fn render_format(
    symbol: &str,
    ctx: &RegisterContext,
    mem: &dyn GuestMemory,
    fmt_obj: u64,
    fixed_int: usize,
) -> Result<Option<Vec<u8>>> {
    let Some(fmt) = cf_string_bytes(fmt_obj) else {
        return Ok(None);
    };
    let describe = |obj: u64| description_bytes(obj).unwrap_or_default();
    let mut args = VarArgs::with_objects(symbol, ctx, mem, fixed_int, &describe);
    render(&fmt, &mut args).map(Some)
}

/// `bytes` as an autoreleased `NSString`, with every `%` doubled.
pub fn escaped_string(bytes: &[u8]) -> Option<u64> {
    let mut escaped = Vec::with_capacity(bytes.len());
    for b in bytes {
        escaped.push(*b);
        if *b == b'%' {
            escaped.push(b'%');
        }
    }
    let create = dlsym("CFStringCreateWithBytes")?;
    let autorelease = dlsym("CFAutorelease")?;
    // SAFETY: both are CoreFoundation's own, called with their declared
    // signatures. A null allocator means the default one; the byte slice
    // outlives the call, and `CFStringCreateWithBytes` copies it.
    unsafe {
        let f: unsafe extern "C" fn(*const c_void, *const u8, isize, u32, u8) -> *const c_void =
            core::mem::transmute(create);
        let s = f(
            core::ptr::null(),
            escaped.as_ptr(),
            escaped.len() as isize,
            UTF8,
            0,
        );
        if s.is_null() {
            return None;
        }
        let auto: unsafe extern "C" fn(*const c_void) -> *const c_void =
            core::mem::transmute(autorelease);
        Some(auto(s) as usize as u64)
    }
}

/// Hand `text` to the host's own `NSLog`, as the whole of a `%s` conversion.
///
/// With no Foundation to hand it to, it goes to stderr in the shape Apple's
/// `NSLog` writes. Losing the guest's log entirely is the one outcome worth
/// avoiding: it is how a Cocoa program says what went wrong.
fn emit(text: &[u8]) {
    // An interior NUL would end the C string early; replacing it keeps the rest
    // of the line, which is what a reader wants from a log.
    let sanitised: Vec<u8> = text
        .iter()
        .map(|b| if *b == 0 { b'?' } else { *b })
        .collect();
    let (Some(nslog), Some(fmt)) = (host_nslog(), percent_s()) else {
        eprintln!("{}", String::from_utf8_lossy(&sanitised));
        return;
    };
    let Ok(c) = CString::new(sanitised) else {
        return;
    };
    // SAFETY: `nslog` is Foundation's own `NSLog`, called through a *variadic*
    // prototype - which is the load-bearing part, since Apple's arm64 ABI
    // passes a variadic argument on the stack and a fixed prototype would put
    // `c` in `x1` where the callee never looks. `fmt` is a `CFStringRef` this
    // module created and keeps alive for the process, and `c` outlives the
    // call.
    unsafe { nslog(fmt, c.as_ptr()) };
}

type NsLogFn = unsafe extern "C" fn(*const c_void, ...);

/// Foundation's `NSLog`, if this process has a Foundation it may hand a string.
///
/// **Not under the builtin frameworks.** [`dlsym`] answers `NSLog` from the host
/// - this tree has no `NSLog` of its own - and answers
/// `CFStringCreateWithCString` from *here*, so the two together would hand
/// Apple's Foundation an object of this runtime and crash inside libobjc. The
/// stderr fallback in [`emit`] is what a process with no Foundation already
/// took, and it is the right path for a process whose Foundation is this one.
fn host_nslog() -> Option<NsLogFn> {
    if crate::frameworks::using_builtin() {
        return None;
    }
    static FOUND: OnceLock<Option<usize>> = OnceLock::new();
    let addr = *FOUND.get_or_init(|| dlsym("NSLog").map(|p| p as usize));
    // SAFETY: `dlsym` returned Foundation's `NSLog`, whose C declaration is
    // `void NSLog(NSString *format, ...)`.
    addr.map(|a| unsafe { core::mem::transmute::<usize, NsLogFn>(a) })
}

/// A `CFStringRef` for `"%s"`, made once and never released.
fn percent_s() -> Option<*const c_void> {
    static FMT: OnceLock<Option<usize>> = OnceLock::new();
    let addr = *FMT.get_or_init(|| {
        let create = dlsym("CFStringCreateWithCString")?;
        // SAFETY: the signature is CoreFoundation's, and a null allocator means
        // the default one. The literal outlives the call.
        let s = unsafe {
            let f: unsafe extern "C" fn(*const c_void, *const c_char, u32) -> *const c_void =
                core::mem::transmute(create);
            f(core::ptr::null(), c"%s".as_ptr(), UTF8)
        };
        (!s.is_null()).then_some(s as usize)
    });
    addr.map(|a| a as *const c_void)
}

/// The UTF-8 bytes of a `CFStringRef`/`NSString *`.
///
/// A word that is not an object *at all* is still not survivable, and cannot be.
fn cf_string_bytes(obj: u64) -> Option<Vec<u8>> {
    if obj == 0 {
        return None;
    }
    if !is_kind_of(obj, "NSString") {
        return None;
    }
    let get = dlsym("CFStringGetCString")?;
    let length = dlsym("CFStringGetLength")?;
    // SAFETY: both are CoreFoundation's own, called with their declared
    // signatures. `obj` is a `CFStringRef` - either the host's, or a constant
    // `@"..."` in the guest's `__cfstring` whose `isa` is a bind of
    // `___CFConstantStringClassReference` and which the host runtime therefore
    // understands (see the roadmap).
    unsafe {
        let len_fn: unsafe extern "C" fn(*const c_void) -> isize = core::mem::transmute(length);
        let n = len_fn(obj as usize as *const c_void);
        if n < 0 {
            return None;
        }
        // Four bytes per UTF-16 unit is the worst case UTF-8 needs, plus the
        // NUL. Capped, because the string may be a guest object's own.
        let cap = (n as usize)
            .saturating_mul(4)
            .saturating_add(1)
            .min(MAX_TEXT);
        let mut buf = vec![0u8; cap];
        let get_fn: unsafe extern "C" fn(*const c_void, *mut c_char, isize, u32) -> u8 =
            core::mem::transmute(get);
        let ok = get_fn(
            obj as usize as *const c_void,
            buf.as_mut_ptr().cast(),
            cap as isize,
            UTF8,
        );
        if ok == 0 {
            return None;
        }
        let s = CStr::from_bytes_until_nul(&buf).ok()?;
        Some(s.to_bytes().to_vec())
    }
}

/// `[obj description]`, as UTF-8.
fn description_bytes(obj: u64) -> Option<Vec<u8>> {
    if obj == 0 {
        // Cocoa prints `(null)` for a nil object, and a reader who sees an
        // empty field instead would look for the wrong bug.
        return Some(b"(null)".to_vec());
    }
    // Every `NSObject` implements `description`, and not every object is an
    // `NSObject` - a class built on `NSProxy`, or one of the few root classes
    // Foundation keeps, answers `doesNotRecognizeSelector:` and raises.
    if !responds_to(obj, "description") {
        return None;
    }
    // SAFETY: the prototype `- (NSString *)description` has, against an object
    // that was just shown to respond to it.
    let text = unsafe { send0::<Id>(obj as usize as Id, sel("description")) };
    cf_string_bytes(text as usize as u64)
}

/// Whether `obj` is an instance of `class_name` or one of its subclasses.
fn is_kind_of(obj: u64, class_name: &str) -> bool {
    let cls = class(class_name);
    if cls.is_null() {
        return false;
    }
    // SAFETY: `- (BOOL)isKindOfClass:(Class)`, whose argument is the class just
    // looked up. A `BOOL` is one byte.
    unsafe { send1::<u8, Id>(obj as usize as Id, sel("isKindOfClass:"), cls) != 0 }
}

/// Whether `obj`'s class implements `selector`.
fn responds_to(obj: u64, selector: &str) -> bool {
    // SAFETY: `obj` is an object pointer - the host runtime's own allocation,
    // or a guest constant whose `isa` is a bind of a Foundation class.
    unsafe { responds(obj as usize as Id, sel(selector)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Load Foundation, and say whether these tests can run at all.
    ///
    /// The driver deliberately never links it - the roadmap records what a framework's
    /// initializers do to the `errno` a guest then reads - so a test binary has no
    /// `NSString` until it asks for one.
    ///
    /// Also false under `ROSEMARY_OBJC=builtin`. Everything below reaches the
    /// host's CoreFoundation by `dlsym` and hands it an object found by name,
    /// and under the builtin runtime that object is one of ours. The subject of
    /// these tests is the host's CF, so with the host's runtime out of the
    /// picture there is nothing here to test rather than something failing.
    fn foundation() -> bool {
        if crate::objc::host::backend() != crate::objc::host::Backend::Native {
            return false;
        }
        // SAFETY: `dlopen` of an absolute path, answering null when absent.
        let handle = unsafe {
            libc::dlopen(
                c"/System/Library/Frameworks/Foundation.framework/Foundation".as_ptr(),
                libc::RTLD_LAZY | libc::RTLD_GLOBAL,
            )
        };
        !handle.is_null()
    }

    /// The guard, against the host's own runtime: an object that is not a string must
    /// come back as `None` rather than raise.
    #[test]
    fn a_non_string_object_is_not_asked_for_its_bytes() {
        if !foundation() {
            return;
        }
        // SAFETY: `objc_getClass` answers null for a name no class has, which
        // is what the skip below checks. A class object is an object.
        let object_class = unsafe { crate::objc::host::objc_getClass(c"NSObject".as_ptr()) };
        if object_class.is_null() {
            return;
        }
        assert_eq!(cf_string_bytes(object_class as u64), None);
    }

    /// The other side of the same claim: a real string still renders, so the
    /// guard is a filter rather than a wall.
    #[test]
    fn a_real_string_still_reads_back() {
        if !foundation() {
            return;
        }
        let Some(fmt) = percent_s() else {
            return;
        };
        assert_eq!(
            cf_string_bytes(fmt as usize as u64).as_deref(),
            Some(&b"%s"[..])
        );
    }

    /// `description` is guarded the same way, and a class object answers it -
    /// so the guard must not refuse the case it is meant to let through.
    #[test]
    fn a_class_describes_itself() {
        if !foundation() {
            return;
        }
        let object_class = unsafe { crate::objc::host::objc_getClass(c"NSObject".as_ptr()) };
        if object_class.is_null() {
            return;
        }
        assert_eq!(
            description_bytes(object_class as u64).as_deref(),
            Some(&b"NSObject"[..])
        );
    }
}

/// `NSLog` for the native backend, which calls it rather than intercepting it.
///
/// The rendering is the same; only the argument source differs. There is no
/// `RegisterContext` here, so a trampoline captures the variadic tail the way a
/// callee's own `va_start` would and hands [`render_native`] its address.
pub mod native {
    use std::ffi::c_void;

    use crate::abi::GuestMemory;

    use super::super::printf::{VarArgs, render};
    use super::{cf_string_bytes, description_bytes, emit};

    /// The address `_NSLog` is bound to.
    pub fn nslog() -> *const c_void {
        trampoline as *const c_void
    }

    /// The address `_NSRunAlertPanel` is bound to by the built-in frameworks.
    pub fn run_alert_panel() -> *const c_void {
        alert_trampoline as *const c_void
    }

    /// The address `_CFStringCreateWithFormat` is bound to by the built-in
    /// frameworks.
    pub fn cfstring_create_with_format() -> *const c_void {
        cfstring_trampoline as *const c_void
    }

    /// The address `_CFStringCreateWithFormatAndArguments` is bound to.
    pub fn cfstring_create_with_format_and_arguments() -> *const c_void {
        render_cfstring as *const c_void
    }

    /// This process's own memory.
    ///
    /// A native image's pointers are host pointers, and what a `%s` names is as
    /// likely to be in a host library as in the program - so there is nothing
    /// to bound the read against but the process itself. A pointer that is not
    /// mapped faults here exactly as it would inside the real `NSLog`.
    struct ProcessMemory;

    impl GuestMemory for ProcessMemory {
        fn read(&self, va: u64, out: &mut [u8]) -> Result<(), machish::Error> {
            if va == 0 {
                return Err(machish::Error::Memory("a null pointer"));
            }
            // SAFETY: the caller of `NSLog` promised these pointers are its own.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    va as usize as *const u8,
                    out.as_mut_ptr(),
                    out.len(),
                )
            };
            Ok(())
        }

        fn write(&mut self, _va: u64, _data: &[u8]) -> Result<(), machish::Error> {
            // `%n` is the only conversion that writes, and this tree's renderer
            // does not implement it.
            Err(machish::Error::Memory(
                "NSLog does not write through arguments",
            ))
        }
    }

    /// Render `format` against the tail a trampoline captured.
    ///
    /// `tail` is a `va_list` on x86-64 and the address of the first stack
    /// argument on AArch64, which is what Darwin's `va_list` is there.
    ///
    /// `None` is a format that was not a readable string, which is the caller's
    /// error and not this runtime's - a logging or formatting call must not be
    /// the thing that stops a program.
    pub fn render_tail(symbol: &str, format: u64, tail: u64) -> Option<Vec<u8>> {
        let mem = ProcessMemory;
        let bytes = cf_string_bytes(format)?;
        let describe = |obj: u64| description_bytes(obj).unwrap_or_default();
        #[cfg(target_arch = "aarch64")]
        let args: crate::abi::Result<VarArgs> = Ok(VarArgs::from_stack(symbol, &mem, tail));
        #[cfg(not(target_arch = "aarch64"))]
        let args: crate::abi::Result<VarArgs> = VarArgs::from_va_list(symbol, &mem, tail);
        let mut args = args.ok()?.describing(&describe);
        render(&bytes, &mut args).ok()
    }

    /// The arguments of a captured tail, one per conversion in `kinds`: `d`
    /// for an integer, `f` for a double, anything else for an object pointer.
    pub fn tail_arguments(
        symbol: &str,
        tail: u64,
        kinds: &[char],
    ) -> Option<Vec<foundation::predicate::Argument>> {
        use foundation::predicate::Argument;
        let mem = ProcessMemory;
        #[cfg(target_arch = "aarch64")]
        let mut args = VarArgs::from_stack(symbol, &mem, tail);
        #[cfg(not(target_arch = "aarch64"))]
        let mut args = VarArgs::from_va_list(symbol, &mem, tail).ok()?;
        kinds
            .iter()
            .map(|kind| {
                Some(match kind {
                    'd' => Argument::Integer(args.next_int().ok()? as i64),
                    'f' => Argument::Real(args.next_fp().ok()?),
                    _ => Argument::Object(args.next_int().ok()? as usize as objc::object::Id),
                })
            })
            .collect()
    }

    /// [`render_tail`] as an autoreleased `NSString` with every `%` doubled,
    /// which is the form the frameworks' own format methods expect.
    pub fn render_escaped(symbol: &str, format: u64, tail: u64) -> Option<u64> {
        super::escaped_string(&render_tail(symbol, format, tail)?)
    }

    unsafe extern "C" fn render_native(fmt: u64, tail: u64) {
        if let Some(out) = render_tail("NSLog", fmt, tail) {
            emit(&out);
        }
    }

    /// `CFStringRef CFStringCreateWithFormatAndArguments(CFAllocatorRef,
    /// CFDictionaryRef formatOptions, CFStringRef format, va_list)`, which is
    /// also where [`cfstring_trampoline`] lands with the tail it captured.
    ///
    /// The result follows the create rule, and the options - a locale
    /// dictionary - are not consulted, as `NSLog` does not consult one either.
    unsafe extern "C" fn render_cfstring(
        _allocator: u64,
        _options: u64,
        format: u64,
        tail: u64,
    ) -> u64 {
        match render_tail("CFStringCreateWithFormat", format, tail) {
            Some(out) => corefoundation::string::from_bytes(&out, false) as usize as u64,
            None => 0,
        }
    }

    unsafe extern "C" fn render_alert_native(
        title: u64,
        format: u64,
        default_button: u64,
        alternate_button: u64,
        other_button: u64,
        tail: u64,
    ) -> i64 {
        let title = cf_string_bytes(title).unwrap_or_default();
        let message = render_tail("NSRunAlertPanel", format, tail).unwrap_or_default();
        let [default, alternate, other] =
            [default_button, alternate_button, other_button].map(cf_string_bytes);
        appkit::alert::run_alert_panel(
            &title,
            &message,
            default.as_deref(),
            alternate.as_deref(),
            other.as_deref(),
        )
    }

    /// Darwin's AArch64 ABI passes every variadic argument on the stack, so
    /// `va_start` is the stack pointer on entry and nothing has to be spilled.
    #[cfg(target_arch = "aarch64")]
    #[unsafe(naked)]
    unsafe extern "C" fn trampoline() {
        core::arch::naked_asm!(
            "mov x1, sp",
            "b   {render}",
            render = sym render_native,
        )
    }

    /// The tail after three named arguments is on the stack from its start.
    #[cfg(target_arch = "aarch64")]
    #[unsafe(naked)]
    unsafe extern "C" fn cfstring_trampoline() {
        core::arch::naked_asm!(
            "mov x3, sp",
            "b   {render}",
            render = sym render_cfstring,
        )
    }

    /// Darwin puts the variadic tail after the five named arguments on the
    /// stack. Pass that address alongside the named arguments already in x0-x4.
    #[cfg(target_arch = "aarch64")]
    #[unsafe(naked)]
    unsafe extern "C" fn alert_trampoline() {
        core::arch::naked_asm!(
            "mov x5, sp",
            "b   {render}",
            render = sym render_alert_native,
        )
    }

    /// SysV puts the first arguments in registers, so the save area a callee's
    /// prologue would build has to be built here: five integer registers (`rdi`
    /// is the format and is not part of the tail) and, when `al` says the call
    /// site passed any, the eight vector ones.
    #[cfg(not(target_arch = "aarch64"))]
    #[unsafe(naked)]
    unsafe extern "C" fn trampoline() {
        core::arch::naked_asm!(
            "push rbp",
            "mov  rbp, rsp",
            // 176 bytes of save area, then the four-field `va_list`.
            "sub  rsp, 208",
            "mov  [rsp + 8],  rsi",
            "mov  [rsp + 16], rdx",
            "mov  [rsp + 24], rcx",
            "mov  [rsp + 32], r8",
            "mov  [rsp + 40], r9",
            "test al, al",
            "je   2f",
            "movaps [rsp + 48],  xmm0",
            "movaps [rsp + 64],  xmm1",
            "movaps [rsp + 80],  xmm2",
            "movaps [rsp + 96],  xmm3",
            "movaps [rsp + 112], xmm4",
            "movaps [rsp + 128], xmm5",
            "movaps [rsp + 144], xmm6",
            "movaps [rsp + 160], xmm7",
            "2:",
            // gp_offset skips `rdi`; fp_offset is the start of the vector half.
            "mov  dword ptr [rsp + 176], 8",
            "mov  dword ptr [rsp + 180], 48",
            // The overflow area is whatever the call site pushed, which begins
            // above the return address and the saved frame pointer.
            "lea  rax, [rbp + 16]",
            "mov  [rsp + 184], rax",
            "mov  [rsp + 192], rsp",
            "lea  rsi, [rsp + 176]",
            "call {render}",
            "mov  rsp, rbp",
            "pop  rbp",
            "ret",
            render = sym render_native,
        )
    }

    /// Build the SysV `va_list` for the tail after five named integer
    /// arguments. Only r9 remains available to the variadic arguments.
    #[cfg(not(target_arch = "aarch64"))]
    #[unsafe(naked)]
    unsafe extern "C" fn alert_trampoline() {
        core::arch::naked_asm!(
            "push rbp",
            "mov  rbp, rsp",
            "sub  rsp, 208",
            "mov  [rsp],      rdi",
            "mov  [rsp + 8],  rsi",
            "mov  [rsp + 16], rdx",
            "mov  [rsp + 24], rcx",
            "mov  [rsp + 32], r8",
            "mov  [rsp + 40], r9",
            "test al, al",
            "je   2f",
            "movaps [rsp + 48],  xmm0",
            "movaps [rsp + 64],  xmm1",
            "movaps [rsp + 80],  xmm2",
            "movaps [rsp + 96],  xmm3",
            "movaps [rsp + 112], xmm4",
            "movaps [rsp + 128], xmm5",
            "movaps [rsp + 144], xmm6",
            "movaps [rsp + 160], xmm7",
            "2:",
            "mov  dword ptr [rsp + 176], 40",
            "mov  dword ptr [rsp + 180], 48",
            "lea  rax, [rbp + 16]",
            "mov  [rsp + 184], rax",
            "mov  [rsp + 192], rsp",
            "lea  r9, [rsp + 176]",
            "call {render}",
            "mov  rsp, rbp",
            "pop  rbp",
            "ret",
            render = sym render_alert_native,
        )
    }

    /// The tail after three named integer arguments, which leaves `rcx`, `r8`
    /// and `r9` to it.
    #[cfg(not(target_arch = "aarch64"))]
    #[unsafe(naked)]
    unsafe extern "C" fn cfstring_trampoline() {
        core::arch::naked_asm!(
            "push rbp",
            "mov  rbp, rsp",
            "sub  rsp, 208",
            "mov  [rsp + 24], rcx",
            "mov  [rsp + 32], r8",
            "mov  [rsp + 40], r9",
            "test al, al",
            "je   2f",
            "movaps [rsp + 48],  xmm0",
            "movaps [rsp + 64],  xmm1",
            "movaps [rsp + 80],  xmm2",
            "movaps [rsp + 96],  xmm3",
            "movaps [rsp + 112], xmm4",
            "movaps [rsp + 128], xmm5",
            "movaps [rsp + 144], xmm6",
            "movaps [rsp + 160], xmm7",
            "2:",
            "mov  dword ptr [rsp + 176], 24",
            "mov  dword ptr [rsp + 180], 48",
            "lea  rax, [rbp + 16]",
            "mov  [rsp + 184], rax",
            "mov  [rsp + 192], rsp",
            "lea  rcx, [rsp + 176]",
            "call {render}",
            "mov  rsp, rbp",
            "pop  rbp",
            "ret",
            render = sym render_cfstring,
        )
    }
}
