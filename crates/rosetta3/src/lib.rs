#![allow(
    clippy::chunks_exact_to_as_chunks,
    clippy::collapsible_if,
    clippy::doc_lazy_continuation,
    clippy::drop_non_drop,
    clippy::if_same_then_else,
    clippy::identity_op,
    clippy::manual_is_multiple_of,
    clippy::missing_safety_doc,
    clippy::needless_borrow,
    clippy::obfuscated_if_else,
    clippy::question_mark,
    clippy::redundant_closure,
    clippy::type_complexity,
    clippy::unnecessary_cast
)]

pub mod abi;
pub mod atomic128;
pub mod audit;
pub mod bridge;
pub mod cache;
pub mod cxxlib;
pub mod dispatch;
pub mod dyld;
pub mod exec;
pub mod fault;
pub mod heapcheck;
pub mod host;
pub mod hostheap;
pub mod interp;
pub mod interpose;
pub mod jit;
pub mod jmpbuf;
pub mod lldb;
pub mod program;
pub mod region;
pub mod retstub;
pub mod shims;
pub mod signal;
pub mod stack;
pub mod startup;
pub mod stats;
/// The Steam bridge, which puts a guest's Steamworks calls on the host's own
/// Steam client instead of a macOS one that does not exist here.
pub mod steam;
pub mod thread;
pub mod thunk;
pub mod tls;
pub mod trace;
pub mod trampoline;
pub mod unwind;
pub mod unwind_native;
pub mod userland;
pub mod vm;

pub fn darwin_name(symbol: &str) -> &str {
    symbol.strip_prefix('_').unwrap_or(symbol)
}

/// The frameworks this tree implements, and whether they are the ones in use.
pub mod frameworks;

/// Realizing the guest's Objective-C classes, and servicing the messages it sends.
///
/// Not gated, because the runtime it talks to need not be the host's: everything
/// here goes through libobjc's public C API, and [`objc::host`] chooses between
/// the host's implementation of that API and this tree's own.
pub mod objc;
pub mod objcexc;

/// `OpenGL.framework`, bound to the GL library SDL is using.
pub mod glsl;
pub mod openal;
pub mod opengl;

/// Subsystems that exist because macOS does.
///
/// [`gui`] is the debug overlay drawn through Metal and AppKit, [`profile`] a
/// sampler built on Mach thread suspension, [`bundle`] the `.app` layout the
/// host's `NSBundle` expects, and [`compat`] a `NSUserDefaults` registration.
/// None of them has a meaning on a host with none of those, and none is stubbed:
/// a guest that imports into one is refused by name at the import. See
/// [`darwin::divergent`].
#[cfg(target_os = "macos")]
pub mod bundle;
#[cfg(target_os = "macos")]
pub mod compat;
#[cfg(target_os = "macos")]
pub mod gui;
#[cfg(target_os = "macos")]
pub mod profile;

pub use abi::{AbiError, Classification, call_import};
pub use audit::{CallTargetAudit, UnresolvedCall, audit_call_targets};
pub use exec::{Engine, Execution, Native};
pub use program::{ImportPolicy, Program, ProgramError};
pub use vm::{Vm, VmError};
