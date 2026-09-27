use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{AbiError, GuestMemory, Result};

pub mod atexit;
/// Not gated: the callback substitution is this runtime's business on either
/// host, and which implementation the call lands on is decided inside.
pub mod audiounit;
pub mod block;
/// Not gated: which registry the legacy spellings reach is decided inside, the
/// same way [`audiounit`]'s is.
pub mod component;
pub mod corefoundation;
/// Not gated: what this family bridges is a *calling convention* - a struct
/// passed or returned by value, which the signature table cannot name - and
/// that is true of the tree's own CoreGraphics as much as of Apple's. Every
/// entry resolves through [`host::host_or_refuse`], so a host with no answer
/// is refused by name rather than gated out.
pub mod coregraphics;
pub mod curl;
pub mod err;
pub(crate) mod host;
pub mod intdiv;
pub mod ioctl;
pub mod math;
pub mod openat;
pub mod printf;
pub mod process;
pub mod pthread;
pub mod runtime;
pub mod sscanf;
pub mod stackprobe;
pub mod sysvsem;
pub mod vmmap;
pub mod zlib;

#[cfg(target_os = "macos")]
pub mod appkit;
#[cfg(target_os = "macos")]
pub mod cfuuid;
#[cfg(target_os = "macos")]
pub mod coremedia;
#[cfg(target_os = "macos")]
pub mod iokit;
#[cfg(target_os = "macos")]
pub mod machthread;

/// Not gated: `NSLog`'s two complications are a variadic tail and an `NSString`
/// format, and both are this runtime's problem on either host. What it reaches
/// for - CoreFoundation, and Foundation's own `NSLog` - it reaches by `dlsym`
/// and does without when there is none. See [`nslog::emit`].
pub mod nslog;
pub mod variadic;

/// A shim that reads and writes the guest's registers and memory.
pub type Shim = fn(&str, &mut RegisterContext, &mut dyn GuestMemory) -> Result<()>;

/// A shim that may also call *into* the guest.
pub type CallbackShim =
    fn(&str, &mut RegisterContext, &mut dyn GuestMemory, &dyn crate::abi::Callbacks) -> Result<()>;

/// The families this host has, whichever host it is.
///
/// The Objective-C runtime is here on both, because [`crate::objc::host`] chooses
/// one rather than requiring the host to be one.
fn platform_shim_for(name: &str) -> Option<Shim> {
    crate::objc::dispatch::shim_for(name)
        .or_else(|| component::shim_for(name))
        .or_else(|| coregraphics::shim_for(name))
        .or_else(|| macos_shim_for(name))
}

/// The macOS-only families, which do not exist on a host with no CoreFoundation
/// and no Mach.
#[cfg(target_os = "macos")]
fn macos_shim_for(name: &str) -> Option<Shim> {
    coremedia::shim_for(name)
        .or_else(|| cfuuid::shim_for(name))
        .or_else(|| appkit::shim_for(name))
        .or_else(|| machthread::shim_for(name))
        // The overlay's OpenGL seam. Only present when the overlay is on;
        // see [`crate::gui::hook::shim_for`].
        .or_else(|| crate::gui::hook::shim_for(name))
}

#[cfg(not(target_os = "macos"))]
fn macos_shim_for(_name: &str) -> Option<Shim> {
    None
}

/// The families that may call into the guest, on either host.
fn callback_shim_for(name: &str) -> Option<CallbackShim> {
    corefoundation::shim_for(name)
        .or_else(|| audiounit::shim_for(name))
        .or_else(|| macos_callback_shim_for(name))
}

/// The macOS-only families that may call into the guest.
#[cfg(target_os = "macos")]
fn macos_callback_shim_for(name: &str) -> Option<CallbackShim> {
    iokit::shim_for(name).or_else(|| coremedia::callback_shim_for(name))
}

#[cfg(not(target_os = "macos"))]
fn macos_callback_shim_for(_name: &str) -> Option<CallbackShim> {
    None
}

/// The two variadics whose format string is an `NSString`, and so are Foundation's.
fn foundation_variadic(name: &str) -> Option<Shim> {
    match name {
        "NSLog" => Some(nslog::nslog),
        // `NSLogv` is `NSLog`'s other half: the format is still an `NSString`,
        // but the arguments arrive as a `va_list` the caller already built.
        // See [`nslog::nslogv`].
        "NSLogv" => Some(nslog::nslogv),
        // The other `NSString`-format variadic, and the one a Cocoa game reports a
        // fatal error through. See [`nslog::run_alert_panel`].
        "NSRunAlertPanel" => Some(nslog::run_alert_panel),
        _ => None,
    }
}

/// Run the shim for `symbol`, if there is one.
///
/// `None` means "not shimmed" and the caller should carry on with the table;
/// `Some(Err(..))` means the shim itself refused, which is a different thing
/// and must not fall through to a passthrough call.
pub fn dispatch(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn crate::abi::Callbacks,
) -> Option<Result<()>> {
    let name = symbol.strip_prefix('_').unwrap_or(symbol);
    let f = match name {
        "printf" => printf::printf,
        "fprintf" => printf::fprintf,
        "sprintf" => printf::sprintf,
        "snprintf" => printf::snprintf,
        // The locale-aware pair, which the **vendored libc++** uses for every
        // number it formats so that `std::to_string` does not pick up the
        // program's locale.
        "snprintf_l" => printf::snprintf_l,
        "sprintf_l" => printf::sprintf_l,
        // The `_FORTIFY_SOURCE` variants clang emits by default on macOS.
        "__sprintf_chk" => printf::sprintf_chk,
        "__snprintf_chk" => printf::snprintf_chk,
        // The `v`-prefixed family.
        "vprintf" => printf::vprintf,
        "vfprintf" => printf::vfprintf,
        "vsprintf" => printf::vsprintf,
        "vsnprintf" => printf::vsnprintf,
        // The wide form, which is how Unreal Engine 3 formats every string it
        // has.
        "vswprintf" => printf::vswprintf,
        "vasprintf" => printf::vasprintf,
        "__vsnprintf_chk" => printf::vsnprintf_chk,
        "sscanf" => sscanf::sscanf,
        // The locale-aware and `va_list` spellings, which are the same parser
        // reached with the destinations somewhere else.
        "sscanf_l" => sscanf::sscanf_l,
        "vsscanf" => sscanf::vsscanf,
        // libsystem_malloc's own printer, which the interposing mimalloc uses.
        "malloc_printf" => printf::malloc_printf,
        "ioctl" => ioctl::ioctl,
        // Variadic in its declaration and one argument wide in fact, decided by
        // a command number rather than a format string; see [`openat`].
        "fcntl" => openat::fcntl,
        // Not variadic, and here for the same underlying reason: forwarding it
        // would operate on the *host's* registers and the host's stack.
        _ => {
            if let Some(f) = foundation_variadic(name)
                .or_else(|| math::shim_for(name))
                .or_else(|| err::shim_for(name))
                .or_else(|| intdiv::shim_for(name))
                .or_else(|| process::shim_for(name))
                .or_else(|| stackprobe::shim_for(name))
                .or_else(|| sysvsem::shim_for(name))
                .or_else(|| vmmap::shim_for(name))
                .or_else(|| crate::jmpbuf::shim_for(name))
                // Measured, not assumed: on **native** this line changes nothing
                // (run 144 and 145 are byte-identical once addresses and pids are
                // normalised) because `dispatch` is reached only from
                // `abi::call_import`, and `exec::native::Native::load` binds
                // every import to a host address and disarms the trampolines, so
                // no guest call ever arrives here. On **emulated** it also
                // changed nothing observable, which settles the worry that
                // `is_shimmed` had been *promising* these names to a caller that
                // relied on the promise: nothing did. A census of every Mach-O in
                // the Chromium.app bundle (14 images) says the guest imports
                // `__Unwind_RaiseException` and `__Unwind_Resume` and **no**
                // `__cxa_*` name except `___cxa_atexit` - it links libc++abi
                // statically, so its `__cxa_throw` at `Framework+0x5680840` is a
                // private guest function, not an import. Which is why
                // un-commenting this line changes no run: the two names the
                // guest does import are `__Unwind_RaiseException` (2 call sites,
                // neither served by `crate::unwind`) and `_Unwind_Resume`
                // (3581 sites), and this line serves the second of those.
                //
                // It is landed anyway, because leaving it out is what makes
                // `is_shimmed` and `dispatch` disagree - and `is_shimmed` is on
                // the load path (`abi::classify`, `ImportPolicy::promotion_for`),
                // where a name it claims is shimmed is deliberately never
                // promoted and is classified without consulting the table. A
                // promise nothing keeps is how `__cxa_throw` came to be
                // "reachable" and unserved at the same time.
                .or_else(|| crate::unwind::shim_for(name))
                // Ahead of `crate::unwind`, and only when
                // `ROSETTA3_NATIVE_UNWINDER=1`: the two entry points, answered by
                // a driver that asks each frame's *own* personality routine
                // instead of re-reading its `__gcc_except_table` here, and **the
                // seven `_Unwind_Context` accessors**, which a personality
                // routine calls back through and which no other shim in this
                // file answers. Order matters and is the whole of the gate - with
                // the variable unset this returns `None` for all nine before
                // looking at any of them, so `crate::unwind` answers the two
                // entry points exactly as before and the seven stay as
                // unreachable as they were.
                //
                // `is_shimmed` below asks the *same* function, and that is the
                // point rather than a convenience: the two halves of this file
                // claiming different sets of names is the defect the test at the
                // bottom of this file is about. A claim is what a shim needs on
                // the emulated backend, where a guest call is intercepted before
                // any published address is consulted; on **native** it is not a
                // wiring, because `exec::native::Native::load` disarms the
                // trampolines and binds every import to a host address, so a
                // guest call is an ordinary machine call that never arrives here.
                // Measured, and the measurement is written up in
                // `crate::unwind_native`.
                .or_else(|| crate::unwind_native::shim_for(name))
                .or_else(|| platform_shim_for(name))
            {
                return Some(f(name, ctx, mem));
            }
            // And the families that need one thing more than the rest.
            return crate::signal::shim_for(name)
                .or_else(|| pthread::shim_for(name))
                .or_else(|| block::shim_for(name))
                .or_else(|| atexit::shim_for(name))
                .or_else(|| zlib::shim_for(name))
                .or_else(|| curl::shim_for(name))
                .or_else(|| callback_shim_for(name))
                .map(|f| f(name, ctx, mem, callbacks));
        }
    };
    Some(f(name, ctx, mem))
}

/// Whether [`dispatch`] would handle `symbol`.
pub fn is_shimmed(symbol: &str) -> bool {
    let name = symbol.strip_prefix('_').unwrap_or(symbol);
    if math::shim_for(name).is_some()
        || err::shim_for(name).is_some()
        || intdiv::shim_for(name).is_some()
        || process::shim_for(name).is_some()
        || stackprobe::shim_for(name).is_some()
        || sysvsem::shim_for(name).is_some()
        || vmmap::shim_for(name).is_some()
        || crate::jmpbuf::shim_for(name).is_some()
        || crate::unwind::shim_for(name).is_some()
        || crate::unwind_native::shim_for(name).is_some()
        || crate::signal::shim_for(name).is_some()
        || pthread::shim_for(name).is_some()
        || block::shim_for(name).is_some()
        || atexit::shim_for(name).is_some()
        || zlib::shim_for(name).is_some()
        || curl::shim_for(name).is_some()
        || foundation_variadic(name).is_some()
        || platform_shim_for(name).is_some()
        || callback_shim_for(name).is_some()
    {
        return true;
    }
    matches!(
        name,
        "printf"
            | "fprintf"
            | "sprintf"
            | "snprintf"
            | "__sprintf_chk"
            | "__snprintf_chk"
            | "vprintf"
            | "vfprintf"
            | "vsprintf"
            | "vsnprintf"
            | "vswprintf"
            | "vasprintf"
            | "__vsnprintf_chk"
            | "sscanf"
            | "sscanf_l"
            | "vsscanf"
            | "malloc_printf"
            | "ioctl"
            | "fcntl"
    )
}

/// Write the guest's `int` return value.
pub(crate) fn set_int_return(ctx: &mut RegisterContext, value: i32) {
    ctx.rax = value as i64 as u64;
}

/// Report a guest pointer that could not be followed.
pub(crate) fn bad_pointer(symbol: &str, addr: u64) -> AbiError {
    AbiError::Memory {
        symbol: symbol.to_string(),
        addr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::NoCallbacks;

    /// A memory that is not there, which is what a shim must refuse rather than
    /// read through.
    struct NoMemory;

    impl GuestMemory for NoMemory {
        fn read(&self, va: u64, out: &mut [u8]) -> core::result::Result<(), machish::Error> {
            Err(machish::Error::OutOfBounds {
                addr: va,
                len: out.len(),
            })
        }
        fn write(&mut self, va: u64, data: &[u8]) -> core::result::Result<(), machish::Error> {
            Err(machish::Error::OutOfBounds {
                addr: va,
                len: data.len(),
            })
        }
    }

    /// The defect this file's own two halves had: `is_shimmed` said `true` for a
    /// name and `dispatch` declined it, so a name was simultaneously
    /// **reachable and unserved**.
    ///
    /// It is a real defect and not a stylistic one, because `is_shimmed` is on
    /// the *load* path, not a diagnostic. `abi::classify` returns on it before
    /// it ever consults the signature table, and `ImportPolicy::promotion_for`
    /// refuses to promote any name it claims - so for a shimmed name the table
    /// is never asked whether there is a signature, and the name's fate rests
    /// entirely on a shim existing. `crate::unwind`'s seven names were claimed
    /// and not served, which is exactly that state.
    ///
    /// Checked over the unwind family because that is where it was open, and
    /// checked by **calling** `dispatch` rather than by reading the table: a
    /// test that only compared two lists would have passed while `dispatch` was
    /// missing a row.
    ///
    /// The names are written in the **Mach-O** spelling, because that is what
    /// `abi::classify` and `ImportPolicy::promotion_for` are handed, and both
    /// this file's functions reduce exactly one leading underscore themselves.
    /// Writing the already-reduced name here was the first draft of this test
    /// and it failed on its first assertion - which is the spelling trap paying
    /// out, and the reason the spellings are now stated rather than implied.
    #[test]
    fn a_name_is_shimmed_exactly_when_dispatch_serves_it() {
        for name in [
            "___cxa_allocate_exception",
            "___cxa_free_exception",
            "___cxa_throw",
            "___cxa_rethrow",
            "___cxa_begin_catch",
            "___cxa_end_catch",
            "__Unwind_Resume",
        ] {
            assert!(is_shimmed(name), "{name} must be claimed");
            let mut ctx = RegisterContext::new();
            let served = dispatch(name, &mut ctx, &mut NoMemory, &NoCallbacks);
            assert!(
                served.is_some(),
                "{name} is claimed by is_shimmed but dispatch declines it: the two \
                 halves of this file disagree, and only the second one is on the \
                 call path"
            );
        }
    }

    /// Both spellings of each name, and the one that is *neither*.
    ///
    /// The C identifier is `_Unwind_Resume` and the Mach-O import is
    /// `__Unwind_Resume`; `__cxa_throw` is the same pair with a third
    /// underscore, because the C++ ABI's own spelling begins with two. Getting
    /// this wrong is invisible to a green suite and to a clean build - only
    /// asking the question catches it, which is what this test is.
    /// The nine names the unwinder claims, held to the same rule as the seven
    /// above - and this one is **env-gated**, so the test has to be too.
    ///
    /// `ROSETTA3_NATIVE_UNWINDER` is a `static_init` cache read once per
    /// process, so a test cannot flip it. What it can do is check the property
    /// that actually matters and is gate-independent: the two halves of this
    /// file agree, for all nine, in the Mach-O spelling. That is the defect
    /// `a_name_is_shimmed_exactly_when_dispatch_serves_it` is about, and it is
    /// a real one - `is_shimmed` is on the load path, where a name it claims is
    /// deliberately never promoted and is classified without consulting the
    /// table.
    ///
    /// The arm is `unwind_native::shim_for`, and both halves call it, so the
    /// agreement is structural rather than checked. This test is what makes the
    /// *set* right: a name added to one and not the other is exactly the class
    /// of defect that compiles, passes, and leaves an import unserved.
    #[test]
    fn the_unwind_family_agrees_across_both_halves() {
        for imported in [
            "__Unwind_RaiseException",
            "__Unwind_Resume",
            "__Unwind_GetIP",
            "__Unwind_GetIPInfo",
            "__Unwind_GetGR",
            "__Unwind_SetGR",
            "__Unwind_SetIP",
            "__Unwind_GetRegionStart",
            "__Unwind_GetLanguageSpecificData",
        ] {
            let reduced = imported.strip_prefix('_').expect("a leading underscore");
            // The Mach-O spelling is the one the images import, and it reduces
            // to the C identifier with exactly one underscore removed - the
            // project's standing third way to be wrong, and the reason these
            // are written in the imported spelling rather than the C one.
            assert_eq!(format!("_{reduced}"), imported);
            let claimed = is_shimmed(imported);
            let mut ctx = RegisterContext::new();
            // The five accessors are handed a `struct _Unwind_Context *` in
            // `rdi`, and `darwin`'s accessors **refuse a pointer they did not
            // create** - by name, and by stopping the process. Which is correct:
            // reading five words of whatever a caller passed would be reading
            // memory this crate never allocated. So the test offers a real
            // context first, exactly as the driver does, or it would abort the
            // test binary rather than assert anything. (It did, once, and that
            // is the refusal working: this is the shape of the bug the check
            // exists to catch.)
            if imported.starts_with("__Unwind_Get") || imported.starts_with("__Unwind_Set") {
                ctx.rdi =
                    darwin::last_unavailable::unwind_context::offer(
                        &darwin::last_unavailable::unwind_context::Context {
                            ip: 0x1000,
                            region_start: 0x900,
                            ..Default::default()
                        },
                    ) as u64;
            }
            let served =
                dispatch(imported, &mut ctx, &mut NoMemory, &NoCallbacks).is_some();
            assert_eq!(
                claimed, served,
                "{imported}: is_shimmed says {claimed}, dispatch says {served}"
            );
            // And the C identifier is neither of them - treating it as one would
            // be the family widened by a typo.
            assert!(!is_shimmed(reduced), "{reduced} must not be claimed");
        }
    }

    /// The refusal for a context pointer this crate did not create is the
    /// correct outcome, and this is what it looks like from the outside.
    ///
    /// It cannot be tested by calling the accessor: `darwin` refuses by
    /// **stopping the process**, which is the right answer to a caller that
    /// handed it a pointer it never allocated, and untestable in-process. So
    /// what is pinned here is the *shape* of the contract instead - that the
    /// pointer handed to a personality is the one `offer` returns, and that
    /// `offer` is the only thing that produces it. A driver that built a fresh
    /// context, or passed any other address, is the failure this project ranks
    /// first, and it is invisible to a test that only checks which names are
    /// claimed.
    #[test]
    fn a_context_is_only_ever_the_one_offer_returned() {
        let at = darwin::last_unavailable::unwind_context::offer(
            &darwin::last_unavailable::unwind_context::Context {
                ip: 0x2000,
                region_start: 0x1000,
                ..Default::default()
            },
        ) as u64;
        // Stable across offers, so a guest holding it across a walk cannot have
        // it move under it.
        let again =
            darwin::last_unavailable::unwind_context::offer(
                &darwin::last_unavailable::unwind_context::Context::default(),
            ) as u64;
        assert_eq!(at, again, "the context pointer moved under the guest");
        assert_ne!(at, 0, "a null context pointer is not the one offer returns");
    }

    #[test]
    fn the_underscore_count_is_the_one_the_guest_imports() {
        for (macho, c_identifier) in [
            ("__Unwind_Resume", "_Unwind_Resume"),
            ("___cxa_throw", "__cxa_throw"),
        ] {
            assert_eq!(
                macho.strip_prefix('_'),
                Some(c_identifier),
                "{macho} does not reduce to {c_identifier}"
            );
            // Handed the Mach-O spelling, both halves agree.
            assert!(is_shimmed(macho), "{macho} is not claimed");
            let mut ctx = RegisterContext::new();
            assert!(
                dispatch(macho, &mut ctx, &mut NoMemory, &NoCallbacks).is_some(),
                "{macho} is claimed but not served"
            );
            // Handed the C identifier, neither half claims it - it is a name
            // nothing in this tree publishes, and treating it as one would be
            // the family widened by a typo.
            assert!(!is_shimmed(c_identifier), "{c_identifier} must not be claimed");
            let mut ctx = RegisterContext::new();
            assert!(
                dispatch(c_identifier, &mut ctx, &mut NoMemory, &NoCallbacks).is_none(),
                "{c_identifier} is served, but no provider publishes it"
            );
        }
    }
}
