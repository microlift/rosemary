use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::ops::Range;

use crate::cxxlib::Flavor;
use machish::loader::Loader;
use machish::memory::{BufferSpace, Regions};
use machish::report::LinkReport;
use machish::resolve::{NativeDylibProvider, SymbolProvider, eager_binding_shims};
use machish::spec::arch::X86_64;

use microlift::arch::x86_64::context::RegisterContext;
use microlift::core::exit::MemorySegment;
use microlift::core::optimize::Optimizations;
use microlift::core::specialize::Specialization;
use microlift::core::{AddressMode, Atomic128Mode, LifterConfig, VmResumptionPoints};

use crate::abi::{AbiError, Classification, Lookup};

/// Whether an import with a known signature is inlined by the lifter or left to
/// a VM exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ImportPolicy {
    /// Inline every call whose signature is known and marshallable; everything
    /// else - variadics, refusals, unknowns - stays a slow exit.
    #[default]
    FastWhenKnown,
    /// Every import is a slow exit. Correct, and the baseline the fast path is
    /// measured against.
    AlwaysSlow,
}

impl ImportPolicy {
    /// The signature to inline this site's call with, and the address to inline a call
    /// to.
    fn promotion_for(self, site: &ImportSite) -> Option<crate::abi::Promotion> {
        if self == ImportPolicy::AlwaysSlow || site.resolved == 0 {
            return None;
        }
        // A traced symbol is never promoted.
        if crate::trace::watching(&site.symbol) {
            return None;
        }
        // A shimmed symbol must never be promoted: the whole reason it has a
        // shim is that calling the host function directly would be wrong.
        if crate::shims::is_shimmed(&site.symbol) {
            let promotion = crate::abi::promotion(&site.symbol, site.resolved)?;
            // Classified as well as described, so a promoted entry passes the
            // same marshallability test every other fast import passes rather
            // than being taken on the table's word.
            return (crate::abi::classify(site.tag, &site.symbol) == Classification::Fast)
                .then_some(promotion);
        }
        if crate::abi::classify(site.tag, &site.symbol) != Classification::Fast {
            return None;
        }
        match crate::abi::table::lookup(site.tag, &site.symbol) {
            Lookup::Known(abi) => Some(crate::abi::Promotion {
                abi,
                address: site.resolved,
            }),
            _ => None,
        }
    }
}

/// Anything that can go wrong loading a program.
#[derive(Debug)]
pub enum ProgramError {
    Io(std::io::Error),
    Link(machish::Error),
    /// The image has no `__TEXT,__text`, so there is nothing to lift.
    NoText,
    /// Non-weak imports nothing satisfied, with [`STRICT_IMPORTS_ENV`] set.
    /// Never raised otherwise: the default is to announce and run, because a
    /// program that never calls the symbol runs correctly without it.
    UnboundImports {
        count: usize,
        first: String,
    },
}

impl fmt::Display for ProgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProgramError::Io(e) => write!(f, "{e}"),
            ProgramError::Link(e) => write!(f, "{e}"),
            ProgramError::NoText => write!(f, "image has no __TEXT,__text section"),
            ProgramError::UnboundImports { count, first } => write!(
                f,
                "{count} unbound import(s), the first being {first} ({STRICT_IMPORTS_ENV} is set)"
            ),
        }
    }
}

impl std::error::Error for ProgramError {}

impl From<std::io::Error> for ProgramError {
    fn from(e: std::io::Error) -> Self {
        ProgramError::Io(e)
    }
}

impl From<machish::Error> for ProgramError {
    fn from(e: machish::Error) -> Self {
        ProgramError::Link(e)
    }
}

/// An Intel Mach-O loaded, linked against the host, and ready for the lifter.
pub struct Program<M: Regions = BufferSpace> {
    /// The full linking transcript.
    pub report: LinkReport,
    /// The guest's mapped memory.
    pub memory: M,
    /// `(virtual address, bytes)` of the root image's `__TEXT,__text`.
    pub text: (u64, Vec<u8>),
    /// Slid extents of each function in `__text`, from `LC_FUNCTION_STARTS`.
    pub functions: Vec<std::ops::Range<u64>>,
    /// Every address a call site might name, mapped to the host address behind
    /// it: both `slot_va` and `stub_va` for each binding.
    pub imports: BTreeMap<u64, ImportSite>,
    /// How to leave each function of the root image, from `__unwind_info`.
    pub unwind: machish::image::UnwindInfo,
    /// The same sites, keyed by the address they *resolved to*.
    imports_by_target: AddrMap<ImportSite>,
    /// One bit per hashed key of [`imports`](Self::imports)
    import_filter: Box<[u64]>,
    /// The same, over [`imports_by_target`](Self::imports_by_target). Asked once per
    /// run-loop miss, where the hash probe it replaces was a cache miss every time.
    target_filter: Box<[u64]>,
}

/// A hasher for maps keyed by a guest or host **address**.
#[derive(Clone, Copy, Default)]
pub struct AddrHasher(u64);

impl std::hash::Hasher for AddrHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // Only reached if someone keys one of these maps by something other
        // than an integer.
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0 ^ n).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.0 ^= self.0 >> 32;
    }

    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }
}

/// [`AddrHasher`] as a `HashMap` may ask for one.
pub type AddrHash = std::hash::BuildHasherDefault<AddrHasher>;

/// A map from an address to whatever it names.
pub type AddrMap<V> = HashMap<u64, V, AddrHash>;

/// The bit `va` claims in a filter of `words` words.
fn filter_bit(va: u64, words: usize) -> usize {
    let mixed = va.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 24;
    (mixed as usize) & (words * 64 - 1)
}

/// Whether `filter` has heard of `va`. A yes may be a collision; a no is final.
pub(crate) fn filter_has(filter: &[u64], va: u64) -> bool {
    let bit = filter_bit(va, filter.len());
    filter[bit / 64] >> (bit % 64) & 1 != 0
}

/// A filter over `keys`, sized to eight bits per key.
pub(crate) fn build_import_filter(keys: impl Iterator<Item = u64>, count: usize) -> Box<[u64]> {
    let words = (count.max(1) / 8 + 1).next_power_of_two();
    let mut filter = vec![0u64; words];
    for va in keys {
        let bit = filter_bit(va, words);
        filter[bit / 64] |= 1 << (bit % 64);
    }
    filter.into_boxed_slice()
}

/// One address microlift should treat as a call out of the guest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportSite {
    pub symbol: String,
    pub library: Option<String>,
    /// The host address the guest call should end up at.
    pub resolved: u64,
    /// Provider-defined; an index into the embedder's shim table. See the crate
    /// note on why the seam is a `u64` and not a typed ABI.
    pub tag: u64,
    /// Whether this address is the `__stubs` entry rather than the pointer slot.
    pub is_stub: bool,
}

/// Turns [`announce_unbound_imports`] from a warning into a refusal to load.
pub const STRICT_IMPORTS_ENV: &str = "ROSETTA3_STRICT_IMPORTS";

/// How many unbound imports are named one by one before the rest are counted.
///
/// Capped because a large application has hundreds and the interesting output is
/// usually what comes *after* this list. `ROSETTA3_UNBOUND_DETAIL` raises it, which
/// is what turns a fault with no symbol into a search of the real work queue - set it
/// to `all` for the whole list.
const UNBOUND_DETAIL_LIMIT: usize = 40;

fn unbound_detail_limit() -> usize {
    match std::env::var("ROSETTA3_UNBOUND_DETAIL") {
        Err(_) => UNBOUND_DETAIL_LIMIT,
        Ok(value) if value.eq_ignore_ascii_case("all") => usize::MAX,
        Ok(value) => value.trim().parse().unwrap_or(UNBOUND_DETAIL_LIMIT),
    }
}

/// Symbols whose slot is left unwritten at link time *on purpose*, because
/// something later fills it in: `_objc_ehtype_vtable` is written by
/// [`crate::objcexc::repair_image_records`] once the guest's classes are
/// registered, which is long after this runs. Announcing one would be a false
/// alarm.
const REPAIRED_AFTER_LINKING: [&str; 1] = ["_objc_ehtype_vtable"];

/// Every non-weak import in `report` that nothing answered, as
/// `(image name, symbol)`.
///
/// A weak import is excluded because a null slot is what the image asked for and
/// its own code tests for; everything here is a slot the guest will use as if it
/// held an address.
pub fn unbound_imports(report: &LinkReport) -> Vec<(&str, &machish::report::MissingSymbol)> {
    report
        .images
        .iter()
        .flat_map(|image| image.hard_missing().map(move |m| (image.name.as_str(), m)))
        .filter(|(_, m)| !REPAIRED_AFTER_LINKING.contains(&m.symbol.as_str()))
        .collect()
}

/// `ROSETTA3_FILL_UNBOUND_DATA=1`: put a readable address in every unbound
/// data slot instead of leaving it zero.
fn fill_unbound_data(unbound: &[(&str, &machish::report::MissingSymbol)]) {
    let Ok(which) = std::env::var("ROSETTA3_FILL_UNBOUND_DATA") else {
        return;
    };
    let everything = which.trim() == "all";
    let page = {
        static PAGE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        *PAGE.get_or_init(|| {
            // SAFETY: an anonymous private mapping of one page, never unmapped:
            // the addresses handed out live as long as the guest does.
            let p = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    4096,
                    libc::PROT_READ,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            match p == libc::MAP_FAILED {
                true => 0,
                false => p as usize,
            }
        })
    };
    if page == 0 {
        eprintln!("rosetta3: ROSETTA3_FILL_UNBOUND_DATA: no page to point at");
        return;
    }
    let mut filled = 0;
    for (_, m) in unbound
        .iter()
        .filter(|(_, m)| m.stub_va.is_none())
        .filter(|(_, m)| everything || is_framework_constant(&m.symbol))
    {
        // SAFETY: the slot is in an image this runtime mapped writable for
        // exactly this - the linker has just finished writing the others.
        unsafe { (m.slot_va as *mut usize).write(page) };
        filled += 1;
    }
    eprintln!(
        "rosetta3: ROSETTA3_FILL_UNBOUND_DATA: {filled} data slot(s) point at a readable page \
         instead of zero"
    );
}

/// `ROSETTA3_NAME_MISSING_CLASSES=1`: bind every unbound `_OBJC_CLASS_$_X` to an
/// empty class called `X`, so the first message to it names the class and the
/// selector.
///
/// The gap this closes in the *diagnostics*, not in the runtime. A class symbol
/// nothing defines binds to zero, `[nil alloc]` is nil, and every message after
/// it is nil - so a feature the guest built on that class does nothing at all
/// and says nothing at all. There are forty-odd of those here, and no way to
/// tell which the program actually uses.
///
/// An empty class turns each into `objc`'s own refusal-by-name:
/// `unrecognized selector sent to instance of NSTask: launchedTaskWithLaunchPath:`.
/// That aborts, which is why this is off by default - but it aborts *naming the
/// thing to implement next*, where the default silently does not. It is the
/// `abi::table` rule ("refused by name rather than guessed at") applied to the
/// half of the surface that is data rather than functions.
fn name_missing_classes(unbound: &[(&str, &machish::report::MissingSymbol)]) {
    if std::env::var_os("ROSETTA3_NAME_MISSING_CLASSES").is_none() {
        return;
    }
    const PREFIX: &str = "_OBJC_CLASS_$_";
    let mut named = 0;
    for (_, m) in unbound.iter().filter(|(_, m)| m.stub_va.is_none()) {
        let Some(name) = m.symbol.strip_prefix(PREFIX) else {
            continue;
        };
        let Ok(name) = std::ffi::CString::new(name) else {
            continue;
        };
        let class = corefoundation::base::register(&name, None, size_of::<usize>(), false);
        // SAFETY: the slot is in an image this runtime mapped writable, and the
        // linker has just finished writing every other binding in it.
        unsafe { (m.slot_va as *mut usize).write(objc::class::ptr(class) as usize) };
        named += 1;
    }
    eprintln!(
        "rosetta3: ROSETTA3_NAME_MISSING_CLASSES: {named} missing class(es) will now abort by \
         name on the first message instead of answering nil"
    );
}

/// Whether a symbol is a framework *constant* - a key, a notification name, a
/// media type - rather than something the runtimes underneath depend on.
///
/// The distinction is what makes [`fill_unbound_data`] usable. Filling every
/// data slot aborts before the first log line, and correctly so: `__objc_empty_cache`
/// is a class's cache and objc writes through it, a `__ZTV` slot is a vtable the
/// C++ runtime walks, and `_environ` is a pointer libc dereferences. None of
/// those is a constant a guest tests against nil, which is the only thing the
/// A/B is about.
fn is_framework_constant(symbol: &str) -> bool {
    const PREFIXES: [&str; 5] = ["_NS", "_AV", "_GC", "_UN", "_k"];
    !symbol.starts_with("_OBJC_")
        && !symbol.starts_with("__")
        && PREFIXES.iter().any(|p| symbol.starts_with(p))
}

/// Say on stderr which imports went unbound, and return them.
///
/// The alternative is what this replaces. An import nobody satisfied leaves a
/// zero in its slot, and the shape of the failure depends on what the guest does
/// with it: a *call* through one reaches [`crate::abi::AbiError::Unsatisfied`]
/// and names the symbol, but a *data* import is dereferenced with no runtime
/// involved at all, and the whole diagnostic the user gets is a fault at a small
/// address, hours into a run, in code that has nothing to do with the symbol
/// that is actually missing. Both cases are decided here, at link time, where
/// the name is still in hand - so this says it then, whether or not the guest
/// ever goes near the slot.
///
/// `what` names the thing being loaded, for the message: a path, or
/// `dlopen(...)`.
pub fn announce_unbound_imports<'r>(
    report: &'r LinkReport,
    what: &str,
) -> Vec<(&'r str, &'r machish::report::MissingSymbol)> {
    let unbound = unbound_imports(report);
    if unbound.is_empty() {
        return unbound;
    }
    eprintln!(
        "rosetta3: {what}: {} import{} unbound, continuing anyway",
        unbound.len(),
        if unbound.len() > 1 { "s" } else { "" }
    );
    let limit = unbound_detail_limit();
    for (image, m) in unbound.iter().take(limit) {
        // The slot and the stub, because they are the addresses a later fault
        // reports: a call through an unbound import arrives at the stub, and a
        // load of one reads the slot. Printing them is what connects "faulted
        // at 0x0 from 0x1000ab2c4" to a symbol name.
        let stub = match m.stub_va {
            Some(stub) => format!(", stub {stub:#x}"),
            None => String::new(),
        };
        eprintln!(
            "rosetta3:   {} from {} in {image}, slot {:#x}{stub}",
            m.symbol,
            m.library.as_deref().unwrap_or("<no library>"),
            m.slot_va,
        );
    }
    if let Some(rest) = unbound.len().checked_sub(limit).filter(|n| *n > 0) {
        eprintln!("rosetta3:   (and {rest} more)");
    }
    // So a later fault at zero can say which of these the guest called, rather
    // than leaving the reader to match a return address against this list by
    // hand.
    crate::fault::note_unbound(&unbound);
    fill_unbound_data(&unbound);
    name_missing_classes(&unbound);
    unbound
}

/// List every import that resolved to this host's own libc rather than to
/// anything in this tree, under `ROSETTA3_TRACE_PASSTHROUGH`.
///
/// The audit that answers "which names is the guest calling that nobody here
/// looked at". A passthrough is safe only when the two platforms agree on the
/// arguments, the structures and the errno values - `darwin::passthrough` is
/// where that claim is recorded - and a name on neither that list nor
/// `darwin::divergent` is passed through because nobody decided to, not because
/// somebody decided it was safe.
///
/// The test is the address, not a table: this tree's entries are Rust functions
/// with no dynamic symbol, so a binding whose resolved address is what `dlsym`
/// answers for the same name came from the host and from nowhere else.
pub fn announce_passthrough_imports(report: &LinkReport, what: &str) {
    if std::env::var_os("ROSETTA3_TRACE_PASSTHROUGH").is_none() {
        return;
    }
    let mut named: Vec<(&str, &str, u64)> = Vec::new();
    for image in &report.images {
        let short = image.name.rsplit('/').next().unwrap_or(&image.name);
        for binding in &image.bindings {
            if !binding.is_satisfied() {
                continue;
            }
            let name = binding.symbol.as_str();
            if crate::host::symbol(name.strip_prefix('_').unwrap_or(name)) != Some(binding.resolved)
            {
                continue;
            }
            named.push((name, short, binding.resolved));
        }
    }
    named.sort_unstable();
    named.dedup();
    eprintln!(
        "rosetta3: {what}: {} import(s) reach this host's libc unaltered",
        named.len()
    );
    for (name, image, addr) in named {
        let claimed = if darwin::passthrough::claimed(name) {
            "claimed"
        } else {
            "UNREVIEWED"
        };
        eprintln!("rosetta3:   {name} in {image} -> {addr:#x} ({claimed})");
    }
}

/// Whether an unbound import is a refusal to load rather than a warning. Read
/// per load rather than cached: loads are counted in ones.
pub fn strict_imports() -> bool {
    match std::env::var(STRICT_IMPORTS_ENV) {
        Err(_) => false,
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off" | ""
        ),
    }
}

impl Program<BufferSpace> {
    /// Load and link `path` against the host, with no guest system libraries.
    ///
    /// `extra` is consulted *before* the native provider, which is how a symbol that
    /// must be virtualized rather than passed through gets intercepted - see
    /// [`machish::resolve`] for why ordering is the mechanism.
    pub fn load(path: &str, extra: &[&dyn SymbolProvider]) -> Result<Self, ProgramError> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes, path, extra)
    }

    /// Load and link an image already in memory.
    pub fn from_bytes(
        bytes: &[u8],
        path: &str,
        extra: &[&dyn SymbolProvider],
    ) -> Result<Self, ProgramError> {
        Self::load_into(bytes, path, extra, BufferSpace::new(0x1_0000_0000))
    }
}

impl Program<machish::memory::MmapSpace> {
    /// Load and link `path` into **host memory at the guest's own addresses**.
    ///
    /// The mapping is never executable.
    pub fn load_executable(
        path: &str,
        extra: &[&dyn SymbolProvider],
    ) -> Result<Self, ProgramError> {
        let bytes = std::fs::read(path)?;
        Self::load_into(
            bytes.as_slice(),
            path,
            extra,
            machish::memory::MmapSpace::new(),
        )
    }
}

/// The provider chain every link in this runtime uses, in the order that makes it safe.
pub(crate) struct HostProviders {
    /// `___stack_chk_guard` and `__tlv_bootstrap`: symbols the host declines to export
    /// and every guest with an array on its stack dereferences in its own prologue.
    runtime: machish::resolve::SyntheticDylib,
    /// The macOS API surface, with a host implementation of each entry. Ahead of
    /// [`HostProviders::native`], so a Darwin-specific answer always wins over a
    /// coincidentally-named host symbol - `___stdoutp` is the host's `stdout` on
    /// Linux, and `hw.machine` is the guest's architecture on either host.
    darwin: machish::resolve::SyntheticDylib,
    /// The Objective-C runtime. The native provider denies the `_objc_`, `_class_`,
    /// `_sel_` and `_protocol_` families precisely so a shim can claim them.
    objc: machish::resolve::SyntheticDylib,
    /// CoreFoundation and the frameworks above it, when this tree's own are the
    /// ones in use. Empty otherwise. See [`crate::frameworks`].
    frameworks: machish::resolve::SyntheticDylib,
    /// The `dl` family, which the host exports and cannot usefully answer for an
    /// x86-64 file. See [`crate::dyld`].
    dyld: machish::resolve::SyntheticDylib,
    /// The Steam bridge's one dispatch symbol, which the generated
    /// `steamclient.dylib` leaves undefined. Empty when this install has no
    /// generated dylib. See [`crate::steam`].
    steam: machish::resolve::SyntheticDylib,
    /// OpenGL, on the GL library SDL made the context against rather than on
    /// whichever one the host loader has. See [`crate::opengl`].
    opengl: machish::resolve::LazyDylib,
    /// OpenAL, on this host's own implementation of the specification. See
    /// [`crate::openal`].
    openal: machish::resolve::LazyDylib,
    eager: machish::resolve::SyntheticDylib,
    native: NativeDylibProvider,
}

/// Say once that the host's libraries answer nothing, because the alternative is a
/// page of unbound imports with no cause given.
///
/// Both backends link, so both have to say it: `darwin::strict` withdraws the
/// native provider from [`HostProviders::chain`] and from `exec::native`'s chain
/// alike.
pub(crate) fn announce_strict() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    if !darwin::strict::enabled() {
        return;
    }
    ONCE.call_once(|| {
        eprintln!(
            "rosetta3: {}: the host's own libraries answer no import; every symbol this \
             tree does not implement is unbound. Set ROSETTA3_UNBOUND_DETAIL=all for the \
             whole list.",
            darwin::strict::VAR
        );
    });
}

impl HostProviders {
    pub(crate) fn new() -> Self {
        let mut native = NativeDylibProvider::system();
        // Symbols whose Darwin and host ABIs differ must not become an address by
        // accident. Empty on macOS; see `darwin::divergent`.
        darwin::divergent::deny_in(&mut native);
        crate::objc::host::deny_foreign_classes(&mut native);
        crate::frameworks::deny_host_frameworks(&mut native);
        crate::opengl::deny_host_gl(&mut native);
        HostProviders {
            runtime: crate::shims::runtime::provider(),
            darwin: darwin::provider(),
            objc: crate::objc::dispatch::provider(),
            frameworks: crate::frameworks::provider(),
            dyld: crate::dyld::provider(),
            steam: crate::steam::provider(),
            openal: crate::openal::provider(),
            opengl: crate::opengl::provider(),
            eager: eager_binding_shims(),
            native,
        }
    }

    /// The chain, with `extra` consulted ahead of everything.
    pub(crate) fn chain<'a>(
        &'a self,
        extra: &[&'a dyn SymbolProvider],
    ) -> Vec<&'a dyn SymbolProvider> {
        announce_strict();
        let mut providers: Vec<&dyn SymbolProvider> = extra.to_vec();
        providers.push(&self.runtime);
        providers.push(&self.darwin);
        providers.push(&self.objc);
        providers.push(&self.frameworks);
        providers.push(&self.dyld);
        providers.push(&self.steam);
        providers.push(&self.opengl);
        providers.push(&self.openal);
        providers.push(&self.eager);
        // Last, and only when the host's own libraries are allowed to answer at
        // all: under `darwin::strict` an import this tree does not implement goes
        // unbound and is named by `announce_unbound_imports` instead.
        if !darwin::strict::enabled() {
            providers.push(&self.native);
        }
        providers
    }
}

/// Everything a lift is configured with that does not depend on which program is
/// being lifted: the host's own capabilities, this runtime's knobs, and the two
/// resumption settings.
///
/// One definition rather than two, because a tool that dumps translations has to
/// lift under the same settings a run does - and a copy of this list that drifts on
/// `address_mode` produces code no run of this runtime executes.
pub fn lifter_config_base() -> LifterConfig {
    // Host-specific, so it is the half of the optimisation setting that
    // belongs to the lift rather than to the lowering - see [`crate::jit`].
    let use_vector_xmm = crate::jit::opt_level().host_specific() && {
        #[cfg(target_arch = "aarch64")]
        {
            std::arch::is_aarch64_feature_detected!("neon")
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            std::arch::is_x86_feature_detected!("sse2")
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86", target_arch = "x86_64")))]
        {
            false
        }
    };
    // `CMPXCHG16B`, and the one setting here that is a correctness decision
    // rather than a tuning one.
    let atomic128 = {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            if std::arch::is_x86_feature_detected!("cmpxchg16b") {
                Atomic128Mode::Native
            } else {
                Atomic128Mode::Interpret
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            if std::arch::is_aarch64_feature_detected!("lse") {
                Atomic128Mode::Native
            } else {
                Atomic128Mode::Interpret
            }
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Atomic128Mode::Interpret
        }
    };
    let specialization = if crate::jit::opt_level().host_specific() {
        Specialization::host()
    } else {
        Specialization::generic()
    };
    LifterConfig {
        interpreter_fallback: true,
        use_vector_xmm,
        outline_helpers: true,
        address_mode: AddressMode::Direct,
        atomic128,
        specialization,
        eliminate_dead_flags: crate::jit::eliminate_dead_flags(),
        resumption: VmResumptionPoints::ExitOnly,
        // Declaring the few entry addresses rather than making every instruction
        // boundary an arm: over the same 128 KiB of a real program's `__text`, in
        // 4 KiB pieces, that is 1.42s of compile time against 19.36s.
        strict_resume: true,
        // Let a region resolve its own successor out of this thread's
        // window instead of returning to the run loop to have the same
        // address looked up.
        block_link: crate::jit::block_linking().then(crate::thread::link_config),
        inline_link_probes: crate::jit::inline_link_probes(),
        // A cell of its own for every exit whose destination the lift already
        // knows, so a static edge is a load and a branch rather than a hash of an
        // address that was a constant all along.
        link_cells: crate::jit::link_cells(),
        // And a ring of `<return address, cell>` frames for the exit that has no
        // constant to specialize on. See [`crate::jit::call_return_stack`].
        call_return_stack: crate::jit::call_return_stack(),
        // Or the other answer to the same question: make the guest's call a host
        // call, so its `RET` is a `ret` the return-address stack predicts. See
        // [`crate::jit::host_call_frames`].
        host_call_frames: crate::jit::host_call_frames(),
        optimizations: Optimizations::all(),
        // `exit_trampoline_address` is left at its default `None`, and that
        // is a decision rather than an omission.
        ..LifterConfig::default()
    }
}

impl<M: Regions> Program<M> {
    /// Load and link into a caller-supplied address space.
    pub fn load_into(
        bytes: &[u8],
        path: &str,
        extra: &[&dyn SymbolProvider],
        mut memory: M,
    ) -> Result<Self, ProgramError> {
        // Vendored libc++ ahead of the filesystem; see [`crate::cxxlib`].
        let mut source = crate::cxxlib::source(Flavor::Intel);
        let hosts = HostProviders::new();
        let providers = hosts.chain(extra);

        // What the guest believes it is running on, which is a property of the image
        // and not of this host: `hw.machine` under Rosetta 2 is `x86_64` on an arm64
        // Mac, and a guest reading `arm64` there goes looking for code paths it does
        // not have. See [`darwin::sysctl`].
        darwin::sysctl::set_guest_arch(darwin::sysctl::GuestArch::X86_64);
        darwin::sysctl::set_is_translated(true);

        let mut report =
            Loader::new(X86_64).link_with(bytes, path, &mut memory, &mut source, &providers)?;

        // Before anything reads the report.
        crate::cxxlib::bind_unwinder_to_the_vendored_one(&mut report, &mut memory, Flavor::Intel)?;

        // And the same for the C++ ABI.
        crate::cxxlib::bind_cxxabi_to_the_vendored_one(&mut report, &mut memory, Flavor::Intel)?;

        // And the Objective-C half of the same ABI, which is stated in terms of
        // the addresses the line above settled on.
        crate::cxxlib::bind_objc_exceptions_to_the_vendored_cxxabi(
            &mut report,
            &mut memory,
            Flavor::Intel,
        )?;

        // And last, because it is stated in terms of what everything above
        // settled on.
        crate::interpose::apply(&mut report, &mut memory)?;

        // Say what did not bind, now that the repairs above have had their say
        // and `missing` means missing. See [`announce_unbound_imports`] for why
        // this is not left to the guest to discover.
        let unbound = announce_unbound_imports(&report, path);
        announce_passthrough_imports(&report, path);
        if !unbound.is_empty() && strict_imports() {
            return Err(ProgramError::UnboundImports {
                count: unbound.len(),
                first: unbound[0].1.symbol.clone(),
            });
        }

        // The root image's `__text` is what the driver lifts.
        let obj = machish::image::MachObject::parse_any(bytes, X86_64)?;
        let text_sect = obj
            .section("__TEXT", "__text")
            .ok_or(ProgramError::NoText)?;
        let slide = report.root().map_or(0, |i| i.slide);
        let text_va = text_sect.addr.wrapping_add(slide);

        let mut text = vec![0u8; text_sect.size as usize];
        memory.read(text_va, &mut text)?;

        let functions: Vec<std::ops::Range<u64>> =
            machish::image::function_ranges(&obj, text_sect.addr, text_sect.addr + text_sect.size)?
                .into_iter()
                .map(|r| r.start.wrapping_add(slide)..r.end.wrapping_add(slide))
                .collect();

        let unwind = machish::image::UnwindInfo::parse(&obj, slide)?;

        let mut imports = BTreeMap::new();
        for b in report.bindings() {
            let site = ImportSite {
                symbol: b.symbol.clone(),
                library: b.library.clone(),
                resolved: b.resolved,
                tag: b.tag,
                is_stub: false,
            };
            imports.insert(b.slot_va, site.clone());
            if let Some(stub) = b.stub_va {
                imports.insert(
                    stub,
                    ImportSite {
                        is_stub: true,
                        ..site
                    },
                );
            }
        }

        // Imports **nothing satisfied** are registered too, resolved to zero.
        // Not for completeness: for the diagnostic.
        for image in &report.images {
            for m in &image.missing {
                let site = ImportSite {
                    symbol: m.symbol.clone(),
                    library: m.library.clone(),
                    resolved: 0,
                    tag: 0,
                    is_stub: false,
                };
                imports.insert(m.slot_va, site.clone());
                if let Some(stub) = m.stub_va {
                    imports.insert(
                        stub,
                        ImportSite {
                            is_stub: true,
                            ..site
                        },
                    );
                }
            }
        }

        let mut by_symbol: Vec<&ImportSite> = imports.values().filter(|s| !s.is_stub).collect();
        by_symbol.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        let mut imports_by_target: AddrMap<ImportSite> = AddrMap::default();
        for site in by_symbol {
            // Guest-resolved sites are excluded for the reason [`crate::dyld`]
            // spells out.
            if site.resolved != 0 && memory.region_at(site.resolved).is_none() {
                imports_by_target
                    .entry(site.resolved)
                    .or_insert_with(|| site.clone());
            }
        }

        let import_filter = build_import_filter(imports.keys().copied(), imports.len());
        let target_filter =
            build_import_filter(imports_by_target.keys().copied(), imports_by_target.len());

        Ok(Program {
            report,
            memory,
            text: (text_va, text),
            functions,
            unwind,
            imports,
            imports_by_target,
            import_filter,
            target_filter,
        })
    }

    /// The shared [`LifterConfig`] every on-demand lift is performed with.
    pub fn lifter_config(&self) -> LifterConfig {
        self.lifter_config_with(ImportPolicy::FastWhenKnown)
    }

    /// [`Program::lifter_config`], with the promotion decision made explicitly.
    pub fn lifter_config_with(&self, policy: ImportPolicy) -> LifterConfig {
        let mut config = lifter_config_base();
        // A mapped region is a whole image's reservation, spanning segments of
        // both kinds, so it is cut at the writable segments' edges rather than
        // handed over whole.
        let writable = self.writable_ranges();
        let mut segments: Vec<MemorySegment> = Vec::new();
        for (base, bytes) in self.memory.regions() {
            let end = base.wrapping_add(bytes.len() as u64);
            let mut piece = |from: u64, to: u64, is_writable: bool| {
                let (lo, hi) = ((from - base) as usize, (to - base) as usize);
                segments.push(MemorySegment::new(
                    from,
                    bytes[lo..hi].to_vec(),
                    is_writable,
                ));
            };
            let mut at = base;
            for range in writable.iter().filter(|r| r.start < end && r.end > base) {
                let (start, stop) = (range.start.max(base), range.end.min(end));
                if at < start {
                    piece(at, start, false);
                }
                piece(start, stop, true);
                at = stop;
            }
            if at < end {
                piece(at, end, false);
            }
        }
        // Set rather than pushed one by one, because the field is shared.
        config.set_segments(segments);
        for (&va, site) in &self.imports {
            // A fast import is an **absolute native call** the lifter bakes
            // into the translation, so the address had better be native code.
            let promoted = policy
                .promotion_for(site)
                .filter(|_| !self.is_guest_address(site.resolved));
            match promoted {
                Some(p) => config.register_fast_import(
                    va,
                    crate::abi::dynamic_symbol(p.abi, p.address),
                    crate::abi::guest_call_abi(),
                ),
                // Everything a fast import cannot express - a variadic, a
                // callback taker, a shim, a signature the marshaller refuses -
                // becomes a bridged call. See [`crate::bridge`].
                // None if policy != ImportPolicy::AlwaysSlow => {
                //     config.register_bridge_import(va, crate::bridge::import_bridge())
                // }
                None => config.register_slow_import(va),
            }
        }

        // Only under the fast policy, because [`ImportPolicy::AlwaysSlow`] is
        // the baseline the rest is measured against and a baseline with two
        // kinds of inline call in it measures nothing.
        if policy != ImportPolicy::AlwaysSlow {
            // Sites that did not exist when this config was built - see
            // [`crate::bridge::DynamicImports`] and [`crate::dyld`].
            config.set_import_registry(crate::bridge::registry());
            // And the calls whose target no lift-time question can answer: a
            // `dlsym` result, a stored `objc_msgSend`, a `__tlv_bootstrap`.
            // config.set_indirect_call_bridge(crate::bridge::indirect_bridge());
        }
        config
    }

    /// Every VA range the guest may store to, merged and in address order.
    fn writable_ranges(&self) -> Vec<Range<u64>> {
        let mut ranges: Vec<Range<u64>> = self
            .report
            .images
            .iter()
            .flat_map(|image| &image.segments)
            .filter(|seg| seg.writable)
            .map(|seg| seg.vmaddr..seg.vmaddr.wrapping_add(seg.vmsize))
            .filter(|r| r.start < r.end)
            .collect();
        ranges.sort_by_key(|r| r.start);
        let mut merged: Vec<Range<u64>> = Vec::with_capacity(ranges.len());
        for r in ranges {
            match merged.last_mut() {
                Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
                _ => merged.push(r),
            }
        }
        merged
    }

    /// How every import of this program will be reached, by symbol.
    pub fn classify_imports(&self) -> BTreeMap<&str, Classification> {
        let mut out = BTreeMap::new();
        for b in self.report.bindings() {
            out.insert(b.symbol.as_str(), crate::abi::classify(b.tag, &b.symbol));
        }
        out
    }

    /// Service an `ExitReason::ImportedFunctionCall`.
    pub fn service_import_call(&mut self, ctx: &mut RegisterContext) -> Result<(), AbiError> {
        let va = ctx.imported_function_va;
        let Some(site) = self.imports.get(&va) else {
            return Err(AbiError::NoSignature {
                symbol: format!("<unregistered {va:#x}>"),
            });
        };
        let (symbol, tag, resolved) = (site.symbol.clone(), site.tag, site.resolved);
        crate::abi::call_import(&symbol, tag, resolved, ctx, &mut self.memory)
    }

    /// The region a JIT should lift in order to execute `va`:
    ///
    /// `None` when `va` is not in mapped guest memory, or when nothing there decodes.
    pub fn lift_region(&self, va: u64) -> Option<(crate::region::Region, Vec<u8>)> {
        let Some((base, bytes)) = self.memory.region_at(va) else {
            // Or an image the *guest* loaded, which is not in this `Program`
            // and cannot be.
            return crate::dyld::lift_region(va)
                // Or code the guest *wrote*, which is in no image at all - see
                // [`crate::shims::vmmap`].
                .or_else(|| lift_generated(va))
                // Or code *this runtime* wrote, which is in no image either.
                .or_else(|| crate::trampoline::lift_region(va))
                // Including the one instruction that ends a call: the return
                // address every entry into guest code pushes.
                .or_else(|| crate::retstub::lift_region(va));
        };
        // A loaded image is not memory the guest writes, so the Mono call rule
        // does not apply to it. See [`crate::region`].
        let region = crate::region::region_for(bytes, base, va, false)?;
        let code = slice_of(bytes, base, &region)?;
        Some((region, code))
    }

    /// The recorded function starts inside `range`, ascending.
    ///
    /// Two binary searches and a slice, not a scan: `functions` comes out of
    /// `LC_FUNCTION_STARTS`, which is a chain of *positive* ULEB128 deltas and so
    /// is already ascending, and the slide added to every entry is the same one.
    /// It matters because this is asked once per lifted region against an image
    /// with a hundred thousand functions in it - a scan made region selection
    /// quadratic in the program, and showed up on a Subnautica profile.
    pub fn function_starts_in(&self, range: std::ops::Range<u64>) -> Vec<u64> {
        let mut starts: Vec<u64> = starts_within(&self.functions, &range)
            .chain(crate::dyld::function_starts_in(range.clone()))
            .collect();
        starts.sort_unstable();
        starts.dedup();
        starts
    }

    /// Whether `va` lies inside a mapped guest image.
    pub fn is_guest_address(&self, va: u64) -> bool {
        self.memory.region_at(va).is_some()
            || crate::dyld::contains(va)
            // Or memory the guest *generated* code into, which is in no image
            // at all.
            || crate::shims::vmmap::contains(va)
            // Or one of this runtime's own five-byte stubs, which is guest code
            // by construction: it exists to be lifted. See [`crate::trampoline`].
            || crate::trampoline::contains(va)
            // Or the return stub, which is guest code for the same reason.
            || crate::retstub::contains(va)
    }

    /// `va` as `image+offset`, in the image's *own* address space.
    pub fn locate(&self, va: u64) -> Option<String> {
        for image in &self.report.images {
            for seg in &image.segments {
                let start = seg.vmaddr;
                if va >= start && va < start.wrapping_add(seg.vmsize) {
                    let name = image.name.rsplit('/').next().unwrap_or(&image.name);
                    return Some(format!("{name}+{:#x}", va.wrapping_sub(image.slide)));
                }
            }
        }
        crate::dyld::locate(va)
            .or_else(|| crate::trampoline::locate(va))
            .or_else(|| crate::retstub::locate(va))
    }

    /// Point this program's import slots at [trampolines](crate::trampoline).
    pub fn install_trampolines(&mut self) -> usize {
        // Collected first, because deciding needs `&self` (for
        // `is_guest_address`) and writing needs `&mut self.memory`.
        let eligible: Vec<(u64, ImportSite)> = self
            .imports
            .iter()
            .filter(|(_, site)| !site.is_stub && !self.is_guest_address(site.resolved))
            .map(|(&va, site)| (va, site.clone()))
            .collect();
        crate::trampoline::install(
            eligible.iter().map(|(va, site)| (*va, site)),
            // Already filtered above, where the borrow was available.
            &|_| false,
            &mut self.memory,
        )
    }

    /// The extent of `__text`, which is what counts as "inside the guest" when
    /// classifying a call target.
    pub fn text_range(&self) -> std::ops::Range<u64> {
        let (va, bytes) = &self.text;
        *va..va.wrapping_add(bytes.len() as u64)
    }

    /// The root image's entry point, slid.
    pub fn entry(&self) -> Option<u64> {
        self.report.root().and_then(|i| i.entry)
    }

    /// Whether the root image's entry point is an `LC_UNIXTHREAD` one - which
    /// is to say whether it reads the process arguments off its own stack
    /// instead of taking them in registers. See [`crate::stack`].
    pub fn entry_reads_stack(&self) -> bool {
        matches!(
            self.report.root().and_then(|i| i.entry_command),
            Some(machish::image::EntryPoint::UnixThread { .. })
        )
    }

    /// How to leave the function containing `va`.
    pub fn unwind_entry(&self, va: u64) -> Option<machish::image::UnwindEntry> {
        self.unwind.lookup(va).copied()
    }

    /// The import registered at `va`, if any.
    pub fn import_at(&self, va: u64) -> Option<&ImportSite> {
        if let Some(site) = self.may_import(va).then(|| self.imports.get(&va)).flatten() {
            return Some(site);
        }
        crate::dyld::import_at(va)
            // And the synthetic addresses a trampoline jumps to.
            .or_else(|| crate::trampoline::import_at(va))
    }

    /// Whether [`import_filter`](Self::import_filter) has heard of `va`. A yes may be
    /// a collision; a no is final.
    fn may_import(&self, va: u64) -> bool {
        filter_has(&self.import_filter, va)
    }

    /// What an imported symbol resolved to, by name.
    pub fn resolved_symbol(&self, symbol: &str) -> Option<u64> {
        self.imports
            .values()
            .find(|s| !s.is_stub && s.symbol == symbol && s.resolved != 0)
            .map(|s| s.resolved)
    }

    /// The import that *resolved to* `host`, if any.
    pub fn import_by_target(&self, host: u64) -> Option<&ImportSite> {
        if let Some(site) = filter_has(&self.target_filter, host)
            .then(|| self.imports_by_target.get(&host))
            .flatten()
        {
            return Some(site);
        }
        crate::dyld::import_by_target(host)
    }

    /// Every import address, in ascending order.
    pub fn import_addresses(&self) -> impl Iterator<Item = u64> + '_ {
        self.imports.keys().copied()
    }
}

/// The starts of `functions` that fall inside `range`, ascending.
///
/// `functions` must be sorted by `start`; every producer of one is a
/// `LC_FUNCTION_STARTS` walk, which cannot produce anything else.
pub(crate) fn starts_within<'a>(
    functions: &'a [std::ops::Range<u64>],
    range: &std::ops::Range<u64>,
) -> impl Iterator<Item = u64> + 'a {
    debug_assert!(
        functions.windows(2).all(|w| w[0].start <= w[1].start),
        "function starts must be ascending",
    );
    let lo = functions.partition_point(|f| f.start < range.start);
    let hi = functions.partition_point(|f| f.start < range.end);
    functions[lo..hi].iter().map(|f| f.start)
}

/// The bytes of a region, cut out of the mapping it was found in.
fn slice_of(bytes: &[u8], base: u64, region: &crate::region::Region) -> Option<Vec<u8>> {
    let start = usize::try_from(region.range.start.checked_sub(base)?).ok()?;
    let end = usize::try_from(region.range.end.checked_sub(base)?)
        .ok()?
        .min(bytes.len());
    Some(bytes.get(start..end)?.to_vec())
}

/// The region to lift in order to execute `va`, when `va` is code the guest *wrote*
/// rather than code it loaded.
///
/// ## What is different about an arena
///
/// Only one thing, and it is the reason this function exists rather than being a call
/// site of the image path:
///
/// ## The bytes are copied, deliberately
///
/// The arena is live guest memory, and the guest's own JIT is still writing to it -
/// possibly on another thread.
///
/// ## Scope
///
/// The code cache never invalidates, so a guest that frees a method and emits a
/// different one at the same address is served the first translation.
fn lift_generated(va: u64) -> Option<(crate::region::Region, Vec<u8>)> {
    let (base, len) = crate::shims::vmmap::arena_at(va)?;
    let offset = usize::try_from(va - base).ok()?;
    let len = usize::try_from(len).ok()?;

    // SAFETY: `base` came from a successful `mmap` this runtime made on the
    // guest's behalf and never unmapped - see [`crate::shims::vmmap`], where the
    // arena is recorded - so the whole range is mapped and readable. The guest
    // may be writing into it concurrently, which is why the bytes are copied
    // here rather than borrowed: a torn read produces an instruction stream that
    // fails to decode, not a dangling reference.
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(base as usize as *const u8, len) };

    let region = crate::region::region_for(bytes, base, va, true)?;
    if crate::shims::vmmap::tracing() {
        let head = &bytes[offset..(offset + 64).min(len)];
        eprintln!(
            "rosetta3: lift generated {va:#x} in arena {base:#x}+{len:#x}, region {:#x}..{:#x}, {} starts, bytes {}",
            region.range.start,
            region.range.end,
            region.starts.len(),
            head.iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    let code = slice_of(bytes, base, &region)?;
    Some((region, code))
}

#[cfg(test)]
mod tests {
    use super::{build_import_filter, filter_bit, unbound_imports};
    use machish::report::{ImageReport, LinkReport, MissingSymbol};

    fn missing(symbol: &str, weak: bool) -> MissingSymbol {
        MissingSymbol {
            symbol: symbol.to_string(),
            library: None,
            slot_va: 0x1_0000_0000,
            stub_va: None,
            weak_import: weak,
        }
    }

    #[test]
    fn only_the_imports_a_guest_would_fault_on_are_announced() {
        let report = LinkReport {
            images: vec![ImageReport {
                name: "a.out".to_string(),
                missing: vec![
                    missing("_hard", false),
                    // Legal, and the image tests for it itself.
                    missing("_weak", true),
                    // Written after registration; see `objcexc`.
                    missing("_objc_ehtype_vtable", false),
                ],
                ..ImageReport::default()
            }],
        };
        let named: Vec<&str> = unbound_imports(&report)
            .iter()
            .map(|(_, m)| m.symbol.as_str())
            .collect();
        assert_eq!(named, ["_hard"]);
    }

    #[test]
    fn the_filter_never_says_no_to_a_key() {
        // Slot and stub addresses as they land: two clusters, one per section.
        let keys: Vec<u64> = (0..8192u64)
            .map(|n| 0x1_0000_2000 + n * 8)
            .chain((0..8192u64).map(|n| 0x1_0100_0000 + n * 6))
            .collect();
        let filter = build_import_filter(keys.iter().copied(), keys.len());
        for &va in &keys {
            let bit = filter_bit(va, filter.len());
            assert!(filter[bit / 64] >> (bit % 64) & 1 != 0, "{va:#x}");
        }
        // And says no often enough to be worth having.
        let misses = (0..100_000u64)
            .map(|n| 0x2_0000_0000 + n * 4)
            .filter(|&va| {
                let bit = filter_bit(va, filter.len());
                filter[bit / 64] >> (bit % 64) & 1 == 0
            })
            .count();
        assert!(
            misses > 80_000,
            "only {misses} of 100,000 non-imports rejected"
        );
    }

    #[test]
    fn a_program_with_no_imports_still_has_a_filter() {
        let filter = build_import_filter(std::iter::empty(), 0);
        assert!(!filter.is_empty());
        assert_eq!(filter_bit(0x1234, filter.len()) / 64, 0);
    }
}
