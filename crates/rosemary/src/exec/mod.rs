pub mod native;

use std::sync::OnceLock;

use machish::report::LinkReport;

use crate::vm::VmError;

pub use native::Native;

/// The backend running this process, for calls entered without an explicit
/// [`Execution`] reference.
///
/// A guest reaches `dlopen` as an ordinary imported function. Its loaded
/// image's initializers must be called through whichever backend launched the
/// executable, including [`Native`], rather than assuming a JIT-backed VM.
static ACTIVE: OnceLock<&'static dyn Execution> = OnceLock::new();

/// Publish the backend that will execute the program.
pub fn publish(exec: &'static dyn Execution) {
    assert!(
        ACTIVE.set(exec).is_ok(),
        "an execution backend is already active"
    );
}

/// The backend executing this process, if program startup has published it.
pub(crate) fn active() -> Option<&'static dyn Execution> {
    ACTIVE.get().copied()
}

/// A loaded program that can be started.
///
/// Deliberately free of any register context: what a shim sees on the way
/// through is emulation vocabulary, and a native backend has none of it.
pub trait Execution: Sync {
    /// Which backend calls this image's instructions.
    fn engine(&self) -> Engine;

    /// The linking transcript, which is what the diagnostics are written against.
    fn report(&self) -> &LinkReport;

    /// `(argc, argv, envp, apple)` for the entry point and the initializers.
    fn main_args(&self) -> (u64, u64, u64, u64);

    /// Call a function in the program with up to six integer arguments.
    fn call(&self, entry: u64, args: &[u64]) -> Result<u64, VmError>;

    /// Whatever this backend must do after the fault reporter and before the
    /// first initializer.
    fn prepare(&self) -> Result<(), VmError>;

    /// Enter the root image's entry point; returns what `main` returned.
    ///
    /// Which of the two entry-point encodings the image used is the backend's
    /// business, because reaching an `LC_UNIXTHREAD` one means entering on a
    /// stack rather than through a call.
    fn enter_main(&self) -> Result<u64, VmError>;

    /// Realize the Objective-C metadata of an image the guest just loaded, and
    /// unique its selector references.
    ///
    /// On the trait rather than done by `dyld` itself because the two backends
    /// realize against different things: the emulated one extends the snapshot
    /// its `Vm` holds, and the native one has no `Vm` at all. `dlopen` used to
    /// ask for a `Vm` and do nothing when there was none, which left a native
    /// guest's loaded image with *raw selector strings* in its `__objc_selrefs`
    /// - so the first message it sent was refused by name, for a selector the
    /// runtime really did have.
    fn realize_loaded_objc(&self, report: &LinkReport) -> Result<(), VmError>;

    /// Call the `+load` methods the image declares, returning how many ran.
    ///
    /// Separate from [`Execution::realize_loaded_objc`] because a `+load` is
    /// guest code: it must run after the registration pass has finished and
    /// outside whatever lock that pass holds.
    fn run_loaded_load_methods(&self, report: &LinkReport) -> Result<usize, VmError>;

    /// What realizes a class against this backend: reads guest memory and
    /// makes guest methods callable by the runtime.
    fn objc_realizer(&self) -> &dyn crate::objc::Realizer;
}

/// Which backend runs a program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// Recompile the guest through microlift.
    Emulated,
    /// Link it against the host's real dylibs and call it.
    Native,
}

impl Engine {
    /// The backend `bytes` should run under: native when it has a slice this
    /// CPU can execute, emulated when it has one this runtime can recompile.
    ///
    /// `fat::slices` answers for a thin file too, so a universal binary and a
    /// single-slice one take the same path.
    pub fn select(bytes: &[u8]) -> Option<Engine> {
        let slices = machish::fat::slices(bytes).ok()?;
        if slices.iter().any(native::runnable) {
            return Some(Engine::Native);
        }
        slices.iter().any(emulatable).then_some(Engine::Emulated)
    }

    /// Why this backend cannot run `bytes`, as a sentence naming what was found.
    pub fn refusal(self, bytes: &[u8]) -> Option<String> {
        let slices = machish::fat::slices(bytes).ok()?;
        let accepts: fn(&machish::fat::Slice) -> bool = match self {
            Engine::Native => native::runnable,
            Engine::Emulated => emulatable,
        };
        if slices.iter().any(accepts) {
            return None;
        }
        let found: Vec<String> = slices.iter().map(slice_name).collect();
        Some(format!(
            "the {} backend needs {}; this binary has {}",
            self.name(),
            self.wants(),
            found.join(", ")
        ))
    }

    pub fn name(self) -> &'static str {
        match self {
            Engine::Emulated => "emulated",
            Engine::Native => "native",
        }
    }

    /// What a slice would have to be for this backend to take it.
    fn wants(self) -> &'static str {
        match self {
            Engine::Emulated => "an x86-64 slice",
            Engine::Native if cfg!(target_arch = "aarch64") => "an arm64 or arm64e slice",
            Engine::Native => "an x86-64 slice",
        }
    }
}

/// Whether the lifter can recompile this slice.
fn emulatable(slice: &machish::fat::Slice) -> bool {
    use machish::spec::MachSpec;
    slice.cputype == machish::spec::constants::CPU_TYPE_X86_64
        && machish::spec::arch::X86_64.accepts_subtype(slice.cpusubtype)
}

/// A slice as a person would name it, `arm64e` included.
fn slice_name(slice: &machish::fat::Slice) -> String {
    use machish::spec::constants::{
        CPU_SUBTYPE_ARM64E, CPU_SUBTYPE_X86_64_H, CPU_TYPE_ARM64, CPU_TYPE_X86_64, subtype_of,
    };
    let sub = subtype_of(slice.cpusubtype);
    match (slice.cputype, sub) {
        (CPU_TYPE_ARM64, CPU_SUBTYPE_ARM64E) => "arm64e".to_string(),
        (CPU_TYPE_ARM64, _) => "arm64".to_string(),
        (CPU_TYPE_X86_64, CPU_SUBTYPE_X86_64_H) => "x86-64h".to_string(),
        (CPU_TYPE_X86_64, _) => "x86-64".to_string(),
        (cputype, _) => format!("cputype {cputype:#x}"),
    }
}
