use microlift::cranelift_codegen;
use microlift::cranelift_native;

use cranelift_codegen::isa::TargetFrontendConfig;
use cranelift_codegen::settings::{self, Configurable};
use microlift::MicroliftError;
use microlift::arch::x86_64::pin;
use microlift::backend::CraneliftBackend;
use microlift::core::callret::CallRetConfig;
use microlift::core::hostframe::HostFrameConfig;
use microlift::core::pin::{PinnedAbi, PinnedReg};
use static_init::dynamic;

/// The environment variables this module reads.
pub const OPT_ENV: &str = "ROSEMARY_JIT_OPT";
pub const VERIFY_ENV: &str = "ROSEMARY_JIT_VERIFY";
pub const DEAD_FLAGS_ENV: &str = "ROSEMARY_DEAD_FLAGS";
/// Whether guest memory accesses are fenced to keep x86 store order. See [`total_store_order`].
pub const TSO_ENV: &str = "ROSEMARY_TSO";
/// Turns [`block_linking`] off. Named for what setting it does, since the
/// feature itself is on by default.
pub const NO_BLOCK_LINK_ENV: &str = "ROSEMARY_NO_BLOCK_LINK";
/// Turns [`pinned_link_abi`] off, leaving links resolved by the host.
pub const NO_PINNED_LINK_ENV: &str = "ROSEMARY_NO_PINNED_LINK";
/// Gives each exit site its own link probe instead of sharing one per region.
pub const INLINE_LINK_PROBES_ENV: &str = "ROSEMARY_INLINE_LINK_PROBES";
/// Turns [`link_cells`] off, leaving every exit to the shared table probe.
pub const NO_LINK_CELLS_ENV: &str = "ROSEMARY_NO_LINK_CELLS";
/// Turns [`call_return_stack`] **on**. Off by default; see that function for the
/// measurement that decided it.
pub const CALL_RETURN_ENV: &str = "ROSEMARY_CALL_RETURN";
/// How many frames the per-thread call-return ring holds. A power of two.
pub const CALLRET_FRAMES_ENV: &str = "ROSEMARY_CALLRET_FRAMES";
/// Which addresses a translation is published as linkable at: `entry` (the one
/// the thread entered) or `all` (every resume point). See [`link_publish`].
pub const LINK_PUBLISH_ENV: &str = "ROSEMARY_LINK_PUBLISH";
/// Turns [`host_call_frames`] **on**. Off by default while it is being measured.
pub const HOST_FRAMES_ENV: &str = "ROSEMARY_HOST_FRAMES";
/// How deep guest calls may nest as host frames.
pub const HOST_FRAME_DEPTH_ENV: &str = "ROSEMARY_HOST_FRAME_DEPTH";
/// Replaces the pinned register set: a comma-separated list of x86-64 register
/// names, e.g. `ROSEMARY_PINNED_REGS=rsp,rax,rdi,rsi,rbp`.
pub const PINNED_REGS_ENV: &str = "ROSEMARY_PINNED_REGS";
/// Which register allocator Cranelift runs: `backtracking` or `single_pass`.
/// See [`regalloc_algorithm`].
pub const REGALLOC_ENV: &str = "ROSEMARY_REGALLOC";

/// What to ask of the JIT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptLevel {
    /// `none` - emit what the lifter produced, unoptimised.
    ///
    /// Slowest by a wide margin. It is here as a bisection tool.
    None,
    /// `speed` - Cranelift's `opt_level=speed`.
    Speed,
    /// `all` - `speed`, plus host-specific choices. The default.
    #[default]
    All,
}

impl OptLevel {
    /// Parse an `ROSEMARY_JIT_OPT` value. Case- and whitespace-insensitive.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Some(OptLevel::None),
            "speed" => Some(OptLevel::Speed),
            "all" => Some(OptLevel::All),
            _ => None,
        }
    }

    /// The name of this level, as it is written in the environment.
    pub fn as_str(self) -> &'static str {
        match self {
            OptLevel::None => "none",
            OptLevel::Speed => "speed",
            OptLevel::All => "all",
        }
    }

    /// The Cranelift `opt_level` setting this level maps to.
    ///
    /// `All` deliberately maps to `speed` and not `speed_and_size`.
    fn cranelift_opt_level(self) -> &'static str {
        match self {
            OptLevel::None => "none",
            OptLevel::Speed | OptLevel::All => "speed",
        }
    }

    /// Whether host-specific lift decisions are allowed at this level.
    pub fn host_specific(self) -> bool {
        matches!(self, OptLevel::All)
    }

    /// The Cranelift flags to build a backend with.
    pub fn flags(self) -> settings::Flags {
        let mut builder = settings::builder();
        // The only way this fails is a setting Cranelift does not have or a
        // value it does not accept, both of which are fixed constants above.
        builder
            .set("opt_level", self.cranelift_opt_level())
            .expect("cranelift accepts its own opt_level values");
        // Set at every level rather than inside the `Speed | All` arm below,
        // because it is not an optimisation setting.
        builder
            .set("enable_verifier", if verify() { "true" } else { "false" })
            .expect("cranelift accepts its own enable_verifier values");
        // We don't process this, so enabling it just wastes cycles.
        builder
            .set("machine_code_cfg_info", "false")
            .expect("failed to turn off machine code cfg info");
        // These aren't necessary since the guest shares our entire address space anyway.
        builder
            .set("enable_heap_access_spectre_mitigation", "false")
            .expect("failed to turn off heap access pass");
        builder
            .set("enable_table_access_spectre_mitigation", "false")
            .expect("failed to turn off table access pass");
        // Not a choice: the x64 backend refuses to emit a `tail` call without
        // one, and every block-linked jump this runtime lifts is a tail call.
        // The aarch64 backend builds the frame record either way, so this is
        // gated rather than set unconditionally only to leave that side alone.
        #[cfg(target_arch = "x86_64")]
        builder
            .set("preserve_frame_pointers", "true")
            .expect("cranelift accepts its own preserve_frame_pointers values");
        match self {
            OptLevel::Speed | OptLevel::All => {
                builder
                    .set("enable_alias_analysis", "true")
                    .expect("couldn't enable alias analysis");
                builder
                    .set("regalloc_algorithm", regalloc_algorithm())
                    .expect("couldn't set the regalloc algorithm");
            }
            OptLevel::None => {}
        };
        settings::Flags::new(builder)
    }

    /// The ISA-specific `(name, value)` pairs to build a backend with.
    pub fn isa_settings(self) -> Vec<(&'static str, &'static str)> {
        match self {
            OptLevel::None => TUNABLE_FEATURES.iter().map(|&f| (f, "false")).collect(),
            OptLevel::Speed => Vec::new(),
            OptLevel::All => host_features(),
        }
    }
}

/// The host target features this crate has an opinion about.
///
/// Deliberately the tuning half of the ISA settings and nothing else; see
/// [`OptLevel::isa_settings`] for what is left out and why.
#[cfg(target_arch = "x86_64")]
const TUNABLE_FEATURES: &[&str] = &[
    "has_sse3",
    "has_ssse3",
    "has_sse41",
    "has_sse42",
    "has_popcnt",
    "has_lzcnt",
    "has_bmi1",
    "has_bmi2",
    "has_avx",
    "has_avx2",
    "has_fma",
    "has_avx512f",
    "has_avx512vl",
    "has_avx512dq",
    "has_avx512bitalg",
    "has_avx512vbmi",
];

/// The host target features this crate has an opinion about.
#[cfg(target_arch = "aarch64")]
const TUNABLE_FEATURES: &[&str] = &["has_lse", "has_fp16", "has_dotprod", "has_i8mm"];

/// The host target features this crate has an opinion about.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const TUNABLE_FEATURES: &[&str] = &[];

/// [`TUNABLE_FEATURES`], each paired with whether this CPU has it.
fn host_features() -> Vec<(&'static str, &'static str)> {
    /// Both spellings Cranelift accepts for a boolean setting.
    fn value(present: bool) -> &'static str {
        if present { "true" } else { "false" }
    }

    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::is_x86_feature_detected as has;
        vec![
            ("has_sse3", value(has!("sse3"))),
            ("has_ssse3", value(has!("ssse3"))),
            ("has_sse41", value(has!("sse4.1"))),
            ("has_sse42", value(has!("sse4.2"))),
            ("has_popcnt", value(has!("popcnt"))),
            ("has_lzcnt", value(has!("lzcnt"))),
            ("has_bmi1", value(has!("bmi1"))),
            ("has_bmi2", value(has!("bmi2"))),
            ("has_avx", value(has!("avx"))),
            ("has_avx2", value(has!("avx2"))),
            ("has_fma", value(has!("fma"))),
            ("has_avx512f", value(has!("avx512f"))),
            ("has_avx512vl", value(has!("avx512vl"))),
            ("has_avx512dq", value(has!("avx512dq"))),
            ("has_avx512bitalg", value(has!("avx512bitalg"))),
            ("has_avx512vbmi", value(has!("avx512vbmi"))),
        ]
    }
    #[cfg(target_arch = "aarch64")]
    {
        use std::arch::is_aarch64_feature_detected as has;
        vec![
            ("has_lse", value(has!("lse"))),
            ("has_fp16", value(has!("fp16"))),
            ("has_dotprod", value(has!("dotprod"))),
            ("has_i8mm", value(has!("i8mm"))),
        ]
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = value(false);
        Vec::new()
    }
}

/// Which register allocator Cranelift runs, at `speed` and above.
///
/// `backtracking` (the default) splits live ranges and produces better code;
/// `single_pass` produces more spills and moves but is far cheaper to run. The
/// difference is not academic here: on a Subnautica profile, regalloc2's
/// backtracking allocator was around 18% of the whole runtime's time during the
/// two minutes the title screen takes to appear, and this runtime compiles a
/// great many regions it then enters only a handful of times.
///
/// A knob rather than a decision, because the two ends of that trade are the load
/// time and the frame rate and only a measurement says which one a given program
/// is short of.
fn regalloc_algorithm() -> &'static str {
    #[dynamic]
    static ALGORITHM: &'static str = match std::env::var(REGALLOC_ENV) {
        Err(_) => "backtracking",
        Ok(value) => match value.trim() {
            "backtracking" => "backtracking",
            "single_pass" => "single_pass",
            other => {
                eprintln!(
                    "rosemary: {REGALLOC_ENV}={other:?} is not one of backtracking, \
                     single_pass; using backtracking"
                );
                "backtracking"
            }
        },
    };
    *ALGORITHM
}

/// The level this run is using.
pub fn opt_level() -> OptLevel {
    #[dynamic]
    static LEVEL: OptLevel = match std::env::var(OPT_ENV) {
        Err(_) => OptLevel::default(),
        Ok(value) => OptLevel::parse(&value).unwrap_or_else(|| {
            // Not fatal, but not silent either: a typo'd level that quietly
            // fell back would look like the level had no effect.
            eprintln!(
                "rosemary: {OPT_ENV}={value:?} is not one of none, speed, all; using {}",
                OptLevel::default().as_str()
            );
            OptLevel::default()
        }),
    };
    *LEVEL
}

/// Enable Cranelift IR verification. Defaults to on in debug builds.
pub fn verify() -> bool {
    #[dynamic]
    static VERIFY: bool = match std::env::var(VERIFY_ENV) {
        Err(_) => cfg!(debug_assertions),
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => {
                // Not fatal, but not silent.
                eprintln!(
                    "rosemary: {VERIFY_ENV}={value:?} is not a boolean; using {}",
                    cfg!(debug_assertions)
                );
                cfg!(debug_assertions)
            }
        },
    };
    *VERIFY
}

/// Whether guest memory accesses are fenced to keep x86's total store order.
///
/// On by default wherever the host is not x86: without it, lock-free guest code that
/// publishes with plain stores (Unity's job queues, Mono's class setup) races on a
/// weakly ordered host, which is how a loading screen ends up waiting on a job no
/// thread holds.
pub fn total_store_order() -> bool {
    #[dynamic]
    static ON: bool = {
        let default = !cfg!(any(target_arch = "x86", target_arch = "x86_64"));
        match std::env::var(TSO_ENV) {
            Err(_) => default,
            Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => {
                    eprintln!("rosemary: {TSO_ENV}={value:?} is not a boolean; using {default}");
                    default
                }
            },
        }
    };
    *ON
}

/// Whether microlift is asked to skip the IR for flag writes nothing reads.
pub fn eliminate_dead_flags() -> bool {
    #[dynamic]
    static ON: bool = match std::env::var(DEAD_FLAGS_ENV) {
        Err(_) => false,
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => {
                eprintln!("rosemary: {DEAD_FLAGS_ENV}={value:?} is not a boolean; using false");
                false
            }
        },
    };
    *ON
}

/// Whether a translated region may resolve its own successor rather than returning to
/// the run loop for it - microlift's `LifterConfig::block_link`.
pub fn block_linking() -> bool {
    #[dynamic]
    static ON: bool = match std::env::var(NO_BLOCK_LINK_ENV) {
        Err(_) => true,
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => false,
            "0" | "false" | "no" | "off" | "" => true,
            _ => {
                eprintln!(
                    "rosemary: {NO_BLOCK_LINK_ENV}={value:?} is not a boolean; linking stays on"
                );
                true
            }
        },
    };
    *ON
}

/// Whether each exit site has its own probe. Off by default.
pub fn inline_link_probes() -> bool {
    #[dynamic]
    static ON: bool = !boolean_env(INLINE_LINK_PROBES_ENV, false, "probes stay shared");
    *ON
}

/// Whether a known destination gets its own link cell.
pub fn link_cells() -> bool {
    #[dynamic]
    static ON: bool = block_linking() && boolean_env(NO_LINK_CELLS_ENV, false, "cells stay on");
    *ON
}

/// Configure the call-return ring. Disabled by default.
pub fn call_return_stack() -> Option<CallRetConfig> {
    #[dynamic]
    static CONFIG: Option<CallRetConfig> = {
        if !link_cells() || !opted_in(CALL_RETURN_ENV) {
            return None;
        }
        Some(CallRetConfig::new(callret_frames()))
    };
    *CONFIG
}

/// Whether `name` is set to something truthy.
///
/// The inverse of [`boolean_env`], which reads a `NO_`-prefixed variable and is
/// therefore false when set. A knob that is off by default needs the other sense, and
/// spelling it as `!boolean_env(..)` reads as a double negative at every call site.
fn opted_in(name: &str) -> bool {
    match std::env::var(name) {
        Err(_) => false,
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" | "" => false,
            _ => {
                eprintln!("rosemary: {name}={value:?} is not a boolean; leaving it off");
                false
            }
        },
    }
}

/// How much of a translation a thread declares linkable when it enters one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkPublish {
    /// Only the address this thread entered at.
    Entry,
    /// Every address the region can be entered at.
    All,
}

/// Which of the two this run uses. `All` unless [`LINK_PUBLISH_ENV`] says `entry`.
pub fn link_publish() -> LinkPublish {
    #[dynamic]
    static CHOICE: LinkPublish = {
        match std::env::var(LINK_PUBLISH_ENV) {
            Ok(v) if v.trim().eq_ignore_ascii_case("entry") => LinkPublish::Entry,
            Ok(v) if v.trim().eq_ignore_ascii_case("all") => LinkPublish::All,
            Ok(v) => {
                eprintln!(
                    "rosemary: {LINK_PUBLISH_ENV}={v:?} is not `entry` or `all`; using `all`"
                );
                LinkPublish::All
            }
            Err(_) => LinkPublish::All,
        }
    };
    *CHOICE
}

/// Nest a guest `CALL`/`RET` pair as a real host call and return
pub fn host_call_frames() -> Option<HostFrameConfig> {
    #[dynamic]
    static CONFIG: Option<HostFrameConfig> = {
        // Cells first: the callee's address comes from one.
        if !link_cells() || !opted_in(HOST_FRAMES_ENV) {
            return None;
        }
        Some(HostFrameConfig::new(host_frame_depth()))
    };
    *CONFIG
}

/// How deep guest calls may nest as host frames.
///
/// 256 against an 8 MiB main thread stack: a region's frame is small, but the
/// guest's own recursion is unbounded and this is what stops it becoming the
/// host's.
pub const HOST_FRAME_DEPTH: u32 = 256;

/// The ceiling this run uses.
fn host_frame_depth() -> u32 {
    let Ok(value) = std::env::var(HOST_FRAME_DEPTH_ENV) else {
        return HOST_FRAME_DEPTH;
    };
    match value.trim().parse::<u32>() {
        // Zero would make every call take the fallback, which is what leaving
        // the feature off already does, so it is refused here where the variable
        // that caused it can be named.
        Ok(depth) if depth > 0 => depth,
        _ => {
            eprintln!(
                "rosemary: {HOST_FRAME_DEPTH_ENV}={value:?} is not a positive \
                 integer; using {HOST_FRAME_DEPTH}"
            );
            HOST_FRAME_DEPTH
        }
    }
}

/// How deep the call-return ring is, unless [`CALLRET_FRAMES_ENV`] says otherwise.
///
/// 1024 frames is 16 KiB a thread, against a call depth that a program which is not
/// recursing spends its life in the low tens.
pub const CALLRET_FRAMES: u32 = 1024;

/// The frame count this run uses.
fn callret_frames() -> u32 {
    let Ok(value) = std::env::var(CALLRET_FRAMES_ENV) else {
        return CALLRET_FRAMES;
    };
    match value.trim().parse::<u32>() {
        // A non-power-of-two would panic inside `CallRetConfig::new`, and a
        // zero-frame ring is a mask of `-1`. Both are refused here, where the
        // variable that caused it can be named.
        Ok(frames) if frames.is_power_of_two() => frames,
        _ => {
            eprintln!(
                "rosemary: {CALLRET_FRAMES_ENV}={value:?} is not a power of two; \
                 using {CALLRET_FRAMES}"
            );
            CALLRET_FRAMES
        }
    }
}

/// Which guest registers this runtime keeps in **host registers** across a link, or
/// `None` to resolve links through the host as before.
///
/// **The set is a policy and a guess, and it is deliberately overridable.** There are
/// five slots to spend on this host (aarch64's tail convention passes six integer
/// values and the context pointer takes one), and which five are worth it is a property
/// of the guest program rather than of the mechanism.
///
/// - `RSP` - every `call`, `ret`, `push` and `pop` in the profile touches it.
pub fn pinned_link_abi() -> Option<PinnedAbi> {
    #[dynamic]
    static ABI: Option<PinnedAbi> = {
        if !boolean_env(NO_PINNED_LINK_ENV, false, "pinned links stay on") {
            return None;
        }
        let abi = match std::env::var(PINNED_REGS_ENV) {
            Err(_) => default_pinned_abi(),
            Ok(list) => match parse_pinned_regs(&list) {
                Some(abi) => abi,
                None => return None,
            },
        };
        // A slot past the host's budget is not an error in microlift - it is a
        // stack slot, which is slower than leaving the register in the context
        // and invisible in anything but a profile. So it is an error here.
        if let Err(e) = abi.check_host_budget() {
            eprintln!("rosemary: {e}; pinned links are off");
            return None;
        }
        Some(abi)
    };
    *ABI
}

/// Guest registers to pin, most worth pinning first.
///
/// Order matters because the list is cut to fit the host: an x86-64 host passes
/// eight integer values under the `tail` convention and an aarch64 one thirteen,
/// both counting the context pointer, so the same default gives a different set
/// on each and the ones at the front are the ones that survive.
///
/// The front five are the set this ran with when aarch64 could carry no more.
/// The rest are in the order x86-64 code uses them: the SysV argument registers,
/// then the scratch ones, then the callee-saved.
const PIN_PRIORITY: &[PinnedReg] = &[
    pin::RSP,
    pin::RDI,
    pin::RAX,
    pin::RBP,
    pin::RSI,
    pin::RDX,
    pin::RCX,
    pin::RBX,
    pin::R8,
    pin::R9,
    pin::R10,
    pin::R11,
    pin::R12,
    pin::R13,
    pin::R14,
    pin::R15,
];

/// As much of [`PIN_PRIORITY`] as this host can carry in registers.
///
/// A slot past the host's budget is a *stack* slot in microlift - correct, and
/// slower than leaving the register in the context - so the default takes what
/// fits rather than asking for a fixed count and being quietly demoted. A host
/// whose budget microlift does not record gets the five this ran with before the
/// budget was widened, which is the conservative answer rather than a guess.
fn default_pinned_abi() -> PinnedAbi {
    const FALLBACK: usize = 5;
    let slots = match microlift::core::pin::RegisterBudget::for_arch(
        microlift::target_lexicon::Triple::host().architecture,
    ) {
        // The context pointer is a slot too, and it is the one nobody counts.
        Some(budget) => (budget.gpr as usize)
            .saturating_sub(1)
            .min(PIN_PRIORITY.len()),
        None => FALLBACK,
    };
    PIN_PRIORITY
        .iter()
        .take(slots)
        .fold(PinnedAbi::new(), |abi, &reg| abi.with(reg))
}

/// Parse a `ROSEMARY_PINNED_REGS` list.
fn parse_pinned_regs(list: &str) -> Option<PinnedAbi> {
    let mut abi = PinnedAbi::new();
    for name in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let reg = match name.to_ascii_lowercase().as_str() {
            "rax" => pin::RAX,
            "rcx" => pin::RCX,
            "rdx" => pin::RDX,
            "rbx" => pin::RBX,
            "rsp" => pin::RSP,
            "rbp" => pin::RBP,
            "rsi" => pin::RSI,
            "rdi" => pin::RDI,
            "r8" => pin::R8,
            "r9" => pin::R9,
            "r10" => pin::R10,
            "r11" => pin::R11,
            "r12" => pin::R12,
            "r13" => pin::R13,
            "r14" => pin::R14,
            "r15" => pin::R15,
            // `xmm0`..`xmm15`. Worth naming because the host's *vector* budget
            // under the tail ABI is eight values and nothing has ever spent any
            // of it, while the general-purpose budget is six counting the
            // context pointer and the default already spends all of it.
            other => match other
                .strip_prefix("xmm")
                .and_then(|n| n.parse::<u16>().ok())
            {
                Some(n) if n < 16 => pin::xmm(n),
                _ => {
                    eprintln!(
                        "rosemary: {PINNED_REGS_ENV} names {other:?}, which is not an \
                         x86-64 general-purpose or XMM register; pinned links are off"
                    );
                    return None;
                }
            },
        };
        if abi.contains(reg) {
            eprintln!("rosemary: {PINNED_REGS_ENV} names {name} twice; pinned links are off");
            return None;
        }
        abi = abi.with(reg);
    }
    // An XMM slot is one value only under `use_vector_xmm`; in the scalar
    // representation it is two variables and microlift turns pinning off for the
    // whole lift rather than pin half a register. Refusing here says so, instead
    // of silently producing a run with no pinned links at all.
    if abi.count(microlift::core::pin::RegClass::Vector) > 0
        && !crate::program::lifter_config_base().use_vector_xmm
    {
        eprintln!(
            "rosemary: {PINNED_REGS_ENV} names an XMM register, which needs the vector \
             XMM representation this run is not using; pinned links are off"
        );
        return None;
    }
    (!abi.is_empty()).then_some(abi)
}

/// Read a boolean environment variable that *disables* something, returning
/// whether the thing stays on.
fn boolean_env(name: &str, unset: bool, on_bad_value: &str) -> bool {
    match std::env::var(name) {
        Err(_) => !unset,
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => false,
            "0" | "false" | "no" | "off" | "" => true,
            _ => {
                eprintln!("rosemary: {name}={value:?} is not a boolean; {on_bad_value}");
                true
            }
        },
    }
}

/// Every knob this module reads, as the run uses it.
///
/// Part of a code cache's configuration key. Most already reach the lifter config or
/// the ISA flags; all of them are listed so none can be forgotten.
pub fn knobs() -> String {
    format!(
        "opt={} verify={} tso={} dead_flags={} block_link={} inline_probes={} cells={} \
         call_return={:?} publish={:?} host_frames={:?} pinned={:?} regalloc={}",
        opt_level().as_str(),
        verify(),
        total_store_order(),
        eliminate_dead_flags(),
        block_linking(),
        inline_link_probes(),
        link_cells(),
        call_return_stack(),
        link_publish(),
        host_call_frames(),
        pinned_link_abi(),
        regalloc_algorithm(),
    )
}

/// The Cranelift flags every backend in this process is built with.
pub fn flags() -> settings::Flags {
    #[dynamic]
    static FLAGS: settings::Flags = opt_level().flags();
    FLAGS.clone()
}

/// The ISA settings every backend in this process is built with.
pub fn isa_settings() -> &'static [(&'static str, &'static str)] {
    #[dynamic]
    static SETTINGS: Vec<(&'static str, &'static str)> = opt_level().isa_settings();
    &SETTINGS
}

/// A JIT backend configured for this run.
///
/// ## Why it holds an `errno` guard
///
/// Compiling runs *on a guest thread*, between two guest instructions, and `errno` is
/// passthrough - so anything this does to `errno` is something the guest reads as its
/// own.
pub fn backend() -> microlift::Result<CraneliftBackend> {
    let _errno = crate::thread::Errno::now();
    CraneliftBackend::with_flags(flags(), isa_settings())
}

/// Every ISA setting the JIT's ISA resolved to, host CPU detection included.
///
/// An image compiled offline starts from the bare host triple, which knows no CPU
/// features; naming each setting makes its ISA the one the JIT compiles for.
pub fn resolved_isa_settings() -> microlift::Result<Vec<(&'static str, String)>> {
    let backend = backend()?;
    Ok(backend
        .isa()
        .isa_flags()
        .iter()
        .map(|value| (value.name, value.value_string()))
        .collect())
}

/// How the frontend is told which machine a `Function` is being built for.
///
/// Built from `cranelift-native`, which is where microlift's [`CraneliftBackend`] gets
/// its ISA too, so the function and the backend it is handed to cannot disagree about
/// the machine.
pub fn frontend_config() -> microlift::Result<TargetFrontendConfig> {
    // The same guard, for the same reason and the same call: see [`backend`].
    let _errno = crate::thread::Errno::now();
    let isa = cranelift_native::builder()
        .map_err(|e| MicroliftError::CraneliftError(e.to_string()))?
        .finish(flags())
        .map_err(|e| MicroliftError::CraneliftError(e.to_string()))?;
    Ok(isa.frontend_config())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_names() {
        assert_eq!(OptLevel::parse("none"), Some(OptLevel::None));
        assert_eq!(OptLevel::parse("speed"), Some(OptLevel::Speed));
        assert_eq!(OptLevel::parse(" ALL\n"), Some(OptLevel::All));
        assert_eq!(OptLevel::parse("fast"), None);
        assert_eq!(OptLevel::parse(""), None);
    }

    #[test]
    fn all_is_the_default_and_the_only_host_specific_level() {
        assert_eq!(OptLevel::default(), OptLevel::All);
        assert!(OptLevel::All.host_specific());
        assert!(!OptLevel::Speed.host_specific());
        assert!(!OptLevel::None.host_specific());
    }

    /// The flags are what actually reach Cranelift, so assert on them and not
    /// just on the mapping function.
    #[test]
    fn levels_reach_cranelift_as_opt_level() {
        assert!(OptLevel::None.flags().opt_level() == settings::OptLevel::None);
        assert!(OptLevel::Speed.flags().opt_level() == settings::OptLevel::Speed);
        assert!(OptLevel::All.flags().opt_level() == settings::OptLevel::Speed);
    }

    /// The verifier is a property of the build and not of the optimisation
    /// level, so every level has to agree with [`verify`] about it - including
    /// `none`, which is the level a lifting bug is bisected at and therefore the
    /// one it would be worst to stop checking.
    #[test]
    fn the_verifier_is_the_same_answer_at_every_level() {
        for level in [OptLevel::None, OptLevel::Speed, OptLevel::All] {
            assert_eq!(
                level.flags().enable_verifier(),
                verify(),
                "{} disagrees about the verifier",
                level.as_str()
            );
        }
    }

    /// And with nothing in the environment, that answer is the build profile.
    ///
    /// The whole safety of turning the verifier off in a shipped binary is that the
    /// suite which would catch an invalid lift still runs with it on - so this pins the
    /// coupling rather than leaving it to be read off `flags()`.
    #[test]
    fn the_default_is_the_build_profile() {
        if std::env::var_os(VERIFY_ENV).is_none() {
            assert_eq!(verify(), cfg!(debug_assertions));
        }
    }

    /// A backend has to be constructible at every level, since an unknown ISA flag
    /// combination fails at `finish()` rather than at `set()`.
    #[test]
    fn every_level_builds_a_backend() {
        for level in [OptLevel::None, OptLevel::Speed, OptLevel::All] {
            assert!(
                CraneliftBackend::with_flags(level.flags(), &level.isa_settings()).is_ok(),
                "{} did not build a backend",
                level.as_str()
            );
        }
    }

    /// The three levels are three different statements about the host, not one
    /// statement at three volumes.
    #[test]
    fn levels_differ_in_what_they_say_about_the_host() {
        // `none` pins the baseline: every tunable named, all off.
        let baseline = OptLevel::None.isa_settings();
        assert_eq!(baseline.len(), TUNABLE_FEATURES.len());
        assert!(baseline.iter().all(|&(_, value)| value == "false"));

        // `speed` defers to cranelift-native's inference by saying nothing.
        assert!(OptLevel::Speed.isa_settings().is_empty());

        // `all` answers the same set of knobs, from detection.
        let host = OptLevel::All.isa_settings();
        assert_eq!(host.len(), TUNABLE_FEATURES.len());
        for &(name, _) in &host {
            assert!(
                TUNABLE_FEATURES.contains(&name),
                "{name} is tuned but not baselined"
            );
        }
        assert!(
            host.iter()
                .all(|&(_, value)| value == "true" || value == "false")
        );
    }

    /// The knobs that are ABI or correctness decisions stay out of the tuning,
    /// at every level - a JIT that stopped signing return addresses because the
    /// user asked for `none` would be a different ABI, not a slower one.
    #[test]
    fn abi_settings_are_not_tunable() {
        for level in [OptLevel::None, OptLevel::Speed, OptLevel::All] {
            for &(name, _) in &level.isa_settings() {
                assert!(
                    !matches!(
                        name,
                        "has_cmpxchg16b"
                            | "has_pauth"
                            | "use_bti"
                            | "sign_return_address"
                            | "sign_return_address_all"
                            | "sign_return_address_with_bkey"
                    ),
                    "{} touches {name}",
                    level.as_str()
                );
            }
        }
    }

    /// Detection is per-run state, so it must be the *same* per-run state everywhere.
    #[test]
    fn the_cached_settings_are_the_level_settings() {
        assert_eq!(isa_settings(), opt_level().isa_settings().as_slice());
    }
}
