use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{AbiError, GuestMemory, Result};

/// Not gated, for the reason [`coregraphics`] is not: the tree's own AppKit
/// takes these structs by value too.
pub mod appkit;
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
pub mod opengl;
pub mod printf;
pub mod process;
pub mod pthread;
pub mod runtime;
pub mod sscanf;
pub mod stackprobe;
pub mod stdio;
pub mod sysvsem;
pub mod vmmap;
pub mod zlib;

pub mod cfuuid;
pub mod coremedia;
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
///
/// Takes no name: each one is bound to the symbol it serves by [`bind!`], so a
/// call neither carries nor re-derives it.
pub type Shim = fn(&mut RegisterContext, &mut dyn GuestMemory) -> Result<()>;

/// A shim that may also call *into* the guest.
pub type CallbackShim =
    fn(&mut RegisterContext, &mut dyn GuestMemory, &dyn crate::abi::Callbacks) -> Result<()>;

/// `f` as a [`Shim`] (or, with `callback`, a [`CallbackShim`]) bound to `name`.
///
/// Implementations still take the name - for their diagnostics, for what they
/// look up by it, and to tell apart the spellings they share - but as a constant
/// fixed where the table spells it, not an argument every call has to pass.
macro_rules! bind {
    ($name:literal => $f:path) => {{
        fn bound(
            ctx: &mut ::microlift::arch::x86_64::context::RegisterContext,
            mem: &mut dyn $crate::abi::GuestMemory,
        ) -> $crate::abi::Result<()> {
            $f($name, ctx, mem)
        }
        bound as $crate::shims::Shim
    }};
    (callback $name:literal => $f:path) => {{
        fn bound(
            ctx: &mut ::microlift::arch::x86_64::context::RegisterContext,
            mem: &mut dyn $crate::abi::GuestMemory,
            callbacks: &dyn $crate::abi::Callbacks,
        ) -> $crate::abi::Result<()> {
            $f($name, ctx, mem, callbacks)
        }
        bound as $crate::shims::CallbackShim
    }};
}
pub(crate) use bind;

/// The families this host has, whichever host it is.
///
/// The Objective-C runtime is on both too, because [`crate::objc::host`] chooses
/// one rather than requiring the host to be one; [`resolve`] asks it first, for
/// an entry rather than a [`Shim`].
fn platform_shim_for(name: &str) -> Option<Shim> {
    component::shim_for(name)
        .or_else(|| coregraphics::shim_for(name))
        .or_else(|| appkit::shim_for(name))
        .or_else(|| opengl::shim_for(name))
        .or_else(|| coremedia::shim_for(name))
        .or_else(|| cfuuid::shim_for(name))
        .or_else(|| macos_shim_for(name))
}

/// The macOS-only families, which do not exist on a host with no CoreFoundation
/// and no Mach.
#[cfg(target_os = "macos")]
fn macos_shim_for(name: &str) -> Option<Shim> {
    machthread::shim_for(name).or_else(|| crate::gui::hook::shim_for(name))
}

#[cfg(not(target_os = "macos"))]
fn macos_shim_for(_name: &str) -> Option<Shim> {
    None
}

/// The families that may call into the guest, on either host.
fn callback_shim_for(name: &str) -> Option<CallbackShim> {
    corefoundation::shim_for(name)
        .or_else(|| audiounit::shim_for(name))
        .or_else(|| coregraphics::callback_shim_for(name))
        .or_else(|| macos_callback_shim_for(name))
}

fn macos_callback_shim_for(name: &str) -> Option<CallbackShim> {
    iokit::shim_for(name).or_else(|| coremedia::callback_shim_for(name))
}

/// The two variadics whose format string is an `NSString`, and so are Foundation's.
fn foundation_variadic(name: &str) -> Option<Shim> {
    match name {
        "NSLog" => Some(bind!("NSLog" => nslog::nslog)),
        // The other `NSString`-format variadic, and the one a Cocoa game reports a
        // fatal error through. See [`nslog::run_alert_panel`].
        "NSRunAlertPanel" => Some(bind!("NSRunAlertPanel" => nslog::run_alert_panel)),
        _ => None,
    }
}

/// A shim of either shape, as [`resolve`] finds it.
#[derive(Clone, Copy)]
pub enum Resolved {
    Plain(Shim),
    WithCallbacks(CallbackShim),
    /// The Objective-C runtime's, resolved past the name so a message send does
    /// not search its tables again.
    Objc(crate::objc::dispatch::Entry),
}

impl Resolved {
    /// Run the shim. It already knows which symbol it serves.
    pub fn call(
        self,
        ctx: &mut RegisterContext,
        mem: &mut dyn GuestMemory,
        callbacks: &dyn crate::abi::Callbacks,
    ) -> Result<()> {
        match self {
            Resolved::Plain(f) => f(ctx, mem),
            Resolved::WithCallbacks(f) => f(ctx, mem, callbacks),
            Resolved::Objc(entry) => entry.call(ctx, mem),
        }
    }
}

/// The shim for `symbol`, if there is one.
///
/// A function of the name alone - and of start-up choices such as the frameworks
/// backend, which are fixed before the first guest call - so a caller may keep
/// the answer for as long as it keeps the name.
pub fn resolve(symbol: &str) -> Option<Resolved> {
    let name = symbol.strip_prefix('_').unwrap_or(symbol);
    let f = match name {
        "printf" => bind!("printf" => printf::printf),
        "fprintf" => bind!("fprintf" => printf::fprintf),
        "sprintf" => bind!("sprintf" => printf::sprintf),
        "snprintf" => bind!("snprintf" => printf::snprintf),
        // The locale-aware pair, which the **vendored libc++** uses for every
        // number it formats so that `std::to_string` does not pick up the
        // program's locale.
        "snprintf_l" => bind!("snprintf_l" => printf::snprintf_l),
        "sprintf_l" => bind!("sprintf_l" => printf::sprintf_l),
        // The `_FORTIFY_SOURCE` variants clang emits by default on macOS.
        "__sprintf_chk" => bind!("__sprintf_chk" => printf::sprintf_chk),
        "__snprintf_chk" => bind!("__snprintf_chk" => printf::snprintf_chk),
        // The `v`-prefixed family.
        "vprintf" => bind!("vprintf" => printf::vprintf),
        "vfprintf" => bind!("vfprintf" => printf::vfprintf),
        "vsprintf" => bind!("vsprintf" => printf::vsprintf),
        "vsnprintf" => bind!("vsnprintf" => printf::vsnprintf),
        // The wide form, which is how Unreal Engine 3 formats every string it
        // has.
        "vswprintf" => bind!("vswprintf" => printf::vswprintf),
        "vasprintf" => bind!("vasprintf" => printf::vasprintf),
        "asprintf" => bind!("asprintf" => printf::asprintf),
        "syslog" => bind!("syslog" => printf::syslog),
        "syslog$DARWIN_EXTSN" => bind!("syslog$DARWIN_EXTSN" => printf::syslog),
        "__vsnprintf_chk" => bind!("__vsnprintf_chk" => printf::vsnprintf_chk),
        "sscanf" => bind!("sscanf" => sscanf::sscanf),
        // The locale-aware and `va_list` spellings, which are the same parser
        // reached with the destinations somewhere else.
        "sscanf_l" => bind!("sscanf_l" => sscanf::sscanf_l),
        "vsscanf" => bind!("vsscanf" => sscanf::vsscanf),
        // libsystem_malloc's own printer, which the interposing mimalloc uses.
        "malloc_printf" => bind!("malloc_printf" => printf::malloc_printf),
        "ioctl" => bind!("ioctl" => ioctl::ioctl),
        // Variadic in its declaration and one argument wide in fact, decided by
        // a command number rather than a format string; see [`openat`].
        "fcntl" => bind!("fcntl" => openat::fcntl),
        // Not variadic, and here for the same underlying reason: forwarding it
        // would operate on the *host's* registers and the host's stack.
        _ => {
            if let Some(f) = foundation_variadic(name)
                .or_else(|| math::shim_for(name))
                .or_else(|| err::shim_for(name))
                .or_else(|| intdiv::shim_for(name))
                .or_else(|| process::shim_for(name))
                .or_else(|| stackprobe::shim_for(name))
                .or_else(|| stdio::shim_for(name))
                .or_else(|| sysvsem::shim_for(name))
                .or_else(|| vmmap::shim_for(name))
                .or_else(|| crate::jmpbuf::shim_for(name))
            // .or_else(|| crate::unwind::shim_for(name))
            {
                return Some(Resolved::Plain(f));
            }
            // First of the platform shims, kept as an entry so a message send
            // does not search its tables again.
            if let Some(entry) = crate::objc::dispatch::resolve(name) {
                return Some(Resolved::Objc(entry));
            }
            if let Some(f) = platform_shim_for(name) {
                return Some(Resolved::Plain(f));
            }
            // And the families that need one thing more than the rest.
            return crate::signal::shim_for(name)
                .or_else(|| pthread::shim_for(name))
                .or_else(|| block::shim_for(name))
                .or_else(|| atexit::shim_for(name))
                .or_else(|| zlib::shim_for(name))
                .or_else(|| curl::shim_for(name))
                .or_else(|| callback_shim_for(name))
                .map(Resolved::WithCallbacks);
        }
    };
    Some(Resolved::Plain(f))
}

/// Whether [`resolve`] would handle `symbol`.
pub fn is_shimmed(symbol: &str) -> bool {
    let name = symbol.strip_prefix('_').unwrap_or(symbol);
    if math::shim_for(name).is_some()
        || err::shim_for(name).is_some()
        || intdiv::shim_for(name).is_some()
        || process::shim_for(name).is_some()
        || stackprobe::shim_for(name).is_some()
        || stdio::shim_for(name).is_some()
        || sysvsem::shim_for(name).is_some()
        || vmmap::shim_for(name).is_some()
        || crate::jmpbuf::shim_for(name).is_some()
        || crate::unwind::shim_for(name).is_some()
        || crate::signal::shim_for(name).is_some()
        || pthread::shim_for(name).is_some()
        || block::shim_for(name).is_some()
        || atexit::shim_for(name).is_some()
        || zlib::shim_for(name).is_some()
        || curl::shim_for(name).is_some()
        || foundation_variadic(name).is_some()
        || crate::objc::dispatch::resolve(name).is_some()
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
