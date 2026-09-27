use microlift::core::srcloc::{GuestCodeMap, GuestCodeRange};
use static_init::dynamic;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

/// One compiled translation, as a fault needs to see it.
struct Placed {
    /// Lowest host address of the compiled blob, and the key this vector is sorted
    /// by.
    code: u64,
    /// The address [`map`](Self::map)'s offsets are relative to, which is
    /// `link_target` and not necessarily [`code`](Self::code).
    map_base: u64,
    /// The host-callable entry thunk: the prologue and the `rip` dispatch every
    /// run-loop entry goes through.
    stub: u64,
    /// Guest addresses the region covers.
    guest: (u64, u64),
    /// The guest `rip` the region was lifted to be entered at.
    lifted_for: u64,
    /// Host offset -> guest instruction, from microlift's own source locations.
    map: Vec<GuestCodeRange>,
}

impl Placed {
    /// One past the last host byte any guest instruction was lifted into.
    fn attributed_end(&self) -> u64 {
        self.map
            .last()
            .map_or(self.code, |r| self.map_base + u64::from(r.end))
    }
}

/// How far past a blob's start a `pc` may be and still be believed to be in it, when
/// nothing follows it to say otherwise.
const MAX_BLOB: u64 = 1 << 20;

/// Compiled translations by the host address of their entry point.
///
/// Global rather than per-thread even though the caches are per-thread.
static PLACED: Mutex<Vec<Placed>> = Mutex::new(Vec::new());

/// How many host-to-guest ranges to keep across the whole process.
const MAP_BUDGET_MIB: usize = 64;

/// Bytes per kept range, which is what turns the environment's MiB into a count.
const RANGE_BYTES: usize = 16;

/// The ceiling this run uses.
fn map_budget() -> usize {
    #[dynamic]
    static BUDGET: usize = {
        let mib = std::env::var("ROSETTA3_MAP_BUDGET")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(MAP_BUDGET_MIB);
        mib.saturating_mul(1024 * 1024) / RANGE_BYTES
    };
    *BUDGET
}

/// Ranges kept so far, against [`map_budget`].
static MAPPED: AtomicUsize = AtomicUsize::new(0);

/// Note where a translation's compiled code went, and what it came from.
pub fn placed(
    entry_point: u64,
    link_target: u64,
    guest_start: u64,
    guest_end: u64,
    lifted_for: u64,
    map: &GuestCodeMap,
) -> usize {
    let code = entry_point.min(link_target);
    // Keyed low, but the thunk's own address is kept so a sample in it can be
    // named as what it is rather than as whichever side of the body it fell on.
    let stub = entry_point;
    let ranges = map.ranges();
    let keep = MAPPED.fetch_add(ranges.len(), Ordering::Relaxed) + ranges.len() <= map_budget();
    let entry = Placed {
        code,
        map_base: link_target,
        stub,
        guest: (guest_start, guest_end),
        lifted_for,
        map: match keep {
            true => ranges.to_vec(),
            false => Vec::new(),
        },
    };
    let mut placed = PLACED.lock().unwrap_or_else(|e| e.into_inner());
    match placed.binary_search_by_key(&code, |p| p.code) {
        Ok(i) => placed[i] = entry,
        Err(i) => placed.insert(i, entry),
    }
    placed.len()
}

/// What a host `pc` turned out to be, as far as the placed translations can say.
///
/// The crash path answers this question with [`say_region`], inside a signal handler,
/// into a fixed buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attribution {
    /// The `pc` is inside a translation, and microlift's source locations name the
    /// guest instruction it was lifted from.
    Instruction {
        /// The guest address of the instruction.
        guest_va: u64,
        /// The first guest address of the region it belongs to, which is the
        /// key a per-region rollup aggregates on.
        region: u64,
    },
    /// Inside a translation, in code no guest instruction was attributed to.
    Translated { region: u64, why: Unnamed },
    /// In no translation this process placed.
    Elsewhere,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unnamed {
    /// The host-callable entry thunk: the prologue and the `rip` dispatch that
    /// begins every run-loop entry. Region entry overhead, not guest work.
    Prologue,
    /// This region kept no map, because [`map_budget`] was spent before it was
    /// compiled. The `pc` could be any instruction in it.
    Budget,
    /// The region has a map and the `pc` is inside its span, in a range no guest
    /// instruction claimed.
    Gap,
    /// Past everything the map attributed - most likely the epilogue.
    Beyond,
}

/// The translation a host `pc` falls in.
///
/// The upper bound matters as much as the lower one: without it every host
/// address above the last translation - a libc frame, most of them - is
/// attributed to whichever translation happens to sit highest.
fn covering(placed: &[Placed], pc: u64) -> Option<&Placed> {
    let at = match placed.binary_search_by_key(&pc, |p| p.code) {
        Ok(i) => i,
        Err(0) => return None,
        Err(i) => i - 1,
    };
    let entry = &placed[at];
    let end = placed
        .get(at + 1)
        .map_or(u64::MAX, |next| next.code)
        .min(entry.code.saturating_add(MAX_BLOB));
    (pc < end).then_some(entry)
}

/// Say once that a dispatch worker was adopted as a guest thread, and whether
/// it had to be.
///
/// Without this, "the fault did not move" and "the hook never fired" look
/// identical, and this investigation has already published two conclusions
/// drawn from an instrument that had not been shown to work.
pub fn note_block_adopted(was_untracked: bool) {
    use std::sync::Mutex;
    static STATE: Mutex<(u8, bool)> = Mutex::new((0, false));

    let Ok(mut state) = STATE.lock() else {
        return;
    };
    let first = state.0 == 0;
    if state.0 < 2 {
        state.0 += 1;
    }
    if was_untracked {
        state.1 = true;
    }
    if !first {
        return;
    }
    let line = format!(
        "rosetta3: a dispatch worker entered guest code on a thread this runtime started \
         (adopted; at least one such thread had no guest thread state before this)\n"
    );
    // SAFETY: a live buffer for the length written.
    unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
}

/// Say once, per process, what the guest-thread registry looked like at the moment a
/// fault was reported on it: the kernel's thread id, the **host** thread name, whether
/// *this* thread had guest thread state, how many threads were registered at all, and
/// which backend is running.
///
/// Off unless `ROSETTA3_THREAD_WHO` is set. Nothing about the report changes either
/// way - this writes its own line and returns.
///
/// **Why it exists.** `say_thread`'s "not a guest thread" is derived from
/// `crate::thread::current()`, which is `None` unless `install` has run on this
/// thread - and on the **native** backend `install` never runs, for any thread,
/// because imports are bound straight to host addresses and nothing is intercepted
/// (see `exec::native::Native::load`). So on native the line is printed for the main
/// thread too and carries no information about the faulting thread at all, and a
/// reader who takes it at face value goes looking for a thread-registration race that
/// cannot exist. This line says which backend ran and how big the registry was, so the
/// claim is checkable in the run that made it rather than inferred from the code.
///
/// The two fields are deliberately independent: `backend` and `registered` are facts
/// about the process, `here` is a fact about this thread. A run where `registered` is
/// zero while `backend` is `emulated` would be a *different* bug - a guest thread
/// that was created and never adopted - and this line is what would tell the two
/// apart.
pub fn note_thread_who(here: bool) {
    /// `ROSETTA3_THREAD_WHO=1` - off by default, and the only thing this reads.
    const ENV: &str = "ROSETTA3_THREAD_WHO";

    use std::sync::Mutex;
    static ONCE: Mutex<bool> = Mutex::new(false);

    if !std::env::var_os(ENV).is_some_and(|v| v != "0") {
        return;
    }
    let Ok(mut once) = ONCE.lock() else {
        return;
    };
    if *once {
        return;
    }
    *once = true;
    drop(once);

    // The host's own name for this thread, which is the only place a thread name is
    // readable without the guest's help. `pthread_getname_np` is a `prctl` read on
    // Linux: no allocation, no lock, nothing that can fault in a handler.
    let mut name = [0u8; 32];
    // SAFETY: `name` is a live local of the length passed, and this thread is live.
    let named = unsafe {
        libc::pthread_getname_np(libc::pthread_self(), name.as_mut_ptr().cast(), name.len())
    } == 0;
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    let name = match named {
        true => format!("{:?}", String::from_utf8_lossy(&name[..end])),
        false => "<unnamed>".to_string(),
    };

    let line = format!(
        "rosetta3: THREAD_WHO tid {:#x} host name {name} guest thread state here: {here}, \
         registered guest threads: {}, backend {}\n",
        crate::thread::key(),
        crate::thread::registered(),
        crate::exec::active().map_or("none", |e| e.engine().name()),
    );
    // SAFETY: a live buffer for the length written.
    unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
}

/// Report the first guest instruction executed inside a nominated address range,
/// once, with the registers the instruction was about to use.
///
/// This exists because **breakpoints do not work on guest code in this setup**,
/// and the only other way to observe a guest pc is to ask the runtime, which
/// already knows it. LLDB reports a breakpoint on a guest address `unresolved`
/// with zero hits - *including at the faulting address itself*, which by
/// definition executes - so a debugger cannot answer "did the guest ever reach
/// here?", and a zero hit count from one proves nothing at all.
///
/// Named by environment variable, as a pair of guest offsets:
/// `ROSETTA3_PC_WATCH=0x5d29050,0x5d29300`.
///
/// Off unless set. One line per hit, and only the first hit per distinct
/// address inside the range, so a hot loop cannot flood the log.
pub fn watch_guest_pc(pc: u64) {
    /// `ROSETTA3_PC_WATCH=start,end` - guest offsets, not host addresses.
    const ENV: &str = "ROSETTA3_PC_WATCH";
    /// Cap on distinct addresses reported, so a loop inside the range cannot
    /// produce an unbounded log.
    const MAX_REPORTED: usize = 8;

    use std::sync::Mutex;
    static SEEN: Mutex<Vec<u64>> = Mutex::new(Vec::new());

    let Attribution::Instruction { guest_va, .. } = attribute(pc) else {
        return;
    };
    let Ok(spec) = std::env::var(ENV) else {
        return;
    };
    let Some((start, end)) = spec.split_once(',').and_then(|(a, b)| {
        let a = u64::from_str_radix(a.trim().trim_start_matches("0x"), 16).ok()?;
        let b = u64::from_str_radix(b.trim().trim_start_matches("0x"), 16).ok()?;
        Some((a, b))
    }) else {
        return;
    };
    if guest_va < start || guest_va >= end {
        return;
    }
    {
        let Ok(mut seen) = SEEN.lock() else {
            return;
        };
        if seen.contains(&guest_va) || seen.len() >= MAX_REPORTED {
            return;
        }
        seen.push(guest_va);
    }
    let line = format!(
        "rosetta3: PC_WATCH guest instruction at Framework+{guest_va:#x} \
         (host {pc:#x})\n"
    );
    // SAFETY: a live buffer for the length written.
    unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
}

/// Which guest code a host `pc` is, if it is any.
pub fn attribute(pc: u64) -> Attribution {
    let placed = PLACED.lock().unwrap_or_else(|e| e.into_inner());
    let Some(entry) = covering(&placed, pc) else {
        return Attribution::Elsewhere;
    };
    let region = entry.guest.0;
    let in_stub = match entry.stub >= entry.map_base {
        true => pc >= entry.stub && entry.stub != entry.map_base,
        false => pc < entry.map_base,
    };
    if in_stub {
        return Attribution::Translated {
            region,
            why: Unnamed::Prologue,
        };
    }
    // Instruction-level, when the map for this one survived the budget.
    if let Ok(offset) = u32::try_from(pc.wrapping_sub(entry.map_base))
        && let Some(guest_va) = lookup(&entry.map, offset)
    {
        return Attribution::Instruction { guest_va, region };
    }
    Attribution::Translated {
        region,
        why: match () {
            _ if entry.map.is_empty() => Unnamed::Budget,
            _ if pc >= entry.attributed_end() => Unnamed::Beyond,
            _ => Unnamed::Gap,
        },
    }
}

/// One placed translation, as anything outside this module may see it.
///
/// A view rather than a borrow of [`Placed`] because the vector is behind a mutex that
/// must not escape.
pub struct PlacedRegion {
    /// Host address the compiled code begins at.
    pub code: u64,
    /// One past the last host byte attributed to a guest instruction - see
    /// [`Placed::attributed_end`] for why this is not the code's length.
    pub code_end: u64,
    /// Guest addresses the region covers.
    pub guest: (u64, u64),
    /// The guest `rip` the region was lifted to be entered at.
    pub lifted_for: u64,
}

/// Walk every translation placed so far, newest layout order (by host address).
///
/// The lock is held for the whole walk, so `f` must not compile anything.
pub fn for_each_placed(mut f: impl FnMut(&PlacedRegion, &[GuestCodeRange])) {
    let placed = PLACED.lock().unwrap_or_else(|e| e.into_inner());
    for entry in placed.iter() {
        let view = PlacedRegion {
            code: entry.code,
            code_end: entry.attributed_end(),
            guest: entry.guest,
            lifted_for: entry.lifted_for,
        };
        f(&view, &entry.map);
    }
}

/// Whether [`install`] has run. Handlers are process-wide.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Longest image name the report keeps. `libmonobdwgc-2.0.dylib` is 23.
const NAME: usize = 48;

/// One loaded guest image, flattened so a handler can search it.
struct Image {
    range: (u64, u64),
    name: [u8; NAME],
    used: usize,
}

/// Where the guest's images landed: the program's before any guest code runs,
/// and each `dlopen`ed image as it is linked.
static IMAGES: Mutex<Vec<Image>> = Mutex::new(Vec::new());

/// One import nothing satisfied, flattened so a handler can search it.
struct Unbound {
    /// The `__stubs` entry a call to this symbol goes through.
    stub: u64,
    name: [u8; NAME],
    used: usize,
}

/// Every unbound import that has a stub, by stub address.
static UNBOUND: Mutex<Vec<Unbound>> = Mutex::new(Vec::new());

/// Record which imports went unbound, so a fault at zero can name the one the
/// guest called.
///
/// An unbound import leaves a zero in its slot and its stub jumps through that
/// slot, so calling one lands at address zero with the symbol nowhere in the
/// register state: `rip` is zero and the stub - the only thing that identifies
/// the symbol - was jumped through rather than called, so it is not on the
/// stack either. All that survives is where the call came from - the link
/// register on AAPCS64, the word the call pushed on x86-64 - and the call
/// instruction immediately before it names its own target. That is enough to
/// recover the stub, and this table turns the stub back into a name.
///
/// Called once per link - the program's, then every `dlopen` - and each call
/// *adds* to the table. Replacing it instead is what made a call through one of
/// Chromium Framework's unbound stubs unnameable: the Framework's list
/// overwrote the launcher's, and the reverse would have been as bad.
pub fn note_unbound(entries: &[(&str, &machish::report::MissingSymbol)]) {
    let mut unbound = UNBOUND.lock().unwrap_or_else(|e| e.into_inner());
    merge_unbound(
        &mut unbound,
        entries.iter().filter_map(|(_, m)| {
            let stub = m.stub_va?;
            let bytes = m.symbol.as_bytes();
            let used = bytes.len().min(NAME);
            let mut name = [0u8; NAME];
            name[..used].copy_from_slice(&bytes[..used]);
            Some(Unbound { stub, name, used })
        }),
    );
}

/// Add `fresh` to `table`, keeping it sorted by stub for the handler's binary
/// search. A stub already present is replaced: the same address in a later link
/// is a later image mapped where an earlier one was.
fn merge_unbound(table: &mut Vec<Unbound>, fresh: impl Iterator<Item = Unbound>) {
    for entry in fresh {
        match table.binary_search_by_key(&entry.stub, |u| u.stub) {
            Ok(i) => table[i] = entry,
            Err(i) => table.insert(i, entry),
        }
    }
}

/// Record the guest's image map, so a fault can name a guest address rather
/// than only state it.
///
/// Without this the report's guest range is a number that needs a second run
/// with `ROSETTA3_TRACE_VM` to interpret - and the whole reason this module
/// exists is that the interesting crash is the one that does not come back.
///
/// Called for the program's own link and again for every `dlopen`, and each
/// call adds to the map. A `dlopen`ed image missing from it is not a cosmetic
/// gap: [`say_unbound_call`] only decodes a return address inside a known
/// image, so a call through an unbound stub in a framework the launcher
/// `dlopen`s - Chromium Framework, which is all of Chromium - was reported as
/// "in no guest image" with no symbol, and cost a debugger session to name.
pub fn note_images(report: &machish::report::LinkReport) {
    let mut images = IMAGES.lock().unwrap_or_else(|e| e.into_inner());
    merge_images(
        &mut images,
        report.images.iter().filter_map(|image| {
            let end = image
                .segments
                .iter()
                .filter(|s| s.vmsize != 0)
                .map(|s| s.vmaddr.wrapping_add(s.vmsize))
                .max()?;
            let leaf = image.name.rsplit('/').next().unwrap_or(&image.name);
            let bytes = leaf.as_bytes();
            let used = bytes.len().min(NAME);
            let mut name = [0u8; NAME];
            name[..used].copy_from_slice(&bytes[..used]);
            Some(Image {
                range: (image.base, end),
                name,
                used,
            })
        }),
    );
}

/// Add `fresh` to `table`, sorted by start. An image overlapping one already
/// recorded replaces it: two live images cannot share an address, so the older
/// entry is one that has since been unmapped.
fn merge_images(table: &mut Vec<Image>, fresh: impl Iterator<Item = Image>) {
    for image in fresh {
        let (start, end) = image.range;
        table.retain(|i| i.range.1 <= start || end <= i.range.0);
        let at = table.partition_point(|i| i.range.0 < start);
        table.insert(at, image);
    }
}

/// Install the reporter for the faults a translation can raise.
///
/// Idempotent, and safe to call before any guest code runs - which is where
/// [`crate::startup`] calls it, because a fault during the first initializer is
/// as much worth naming as one an hour in.
pub fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    keep_the_fault_signals();
    // `SIGABRT` beside the two faults, because the allocator is the third way a
    // program of this size dies and the only one that used to leave no report at
    // all: glibc's `free(): invalid size` goes to stderr and then straight to
    // `abort`, with no pc, no image and no registers. The handler restores the
    // previous disposition and returns, so the abort still happens - this only
    // adds the sentence in front of it.
    //
    // `SIGTRAP` joins them only when [`STACK_WHO_ENV`] is set, and that is not
    // politeness - it is the only way to see the fault this run actually dies
    // of. A Chromium `LOG(FATAL)` in an official build does **not** reach
    // `abort()`: `base::CheckError::~CheckError` prints the message from
    // `~LogMessage` and then runs `base::ImmediateCrash()`, which on x86-64 is
    // `int3` - `CheckError::~CheckError` at `Framework+0x5cb8770` ends in
    // `cmp r14d,0x3; je; int3; ud2` at `0x5cb87b7`. So the process dies of
    // **SIGTRAP** and exits 133, not 134, and a runtime that reports only
    // SIGSEGV, SIGBUS and SIGABRT reports nothing at all - which is why runs 090
    // and 092 carry no `rosetta3: fault:` block and this abort looks as though
    // it has no stack.
    //
    // **Unconditional now, and it used to be gated on `stack_who()`.** The gate
    // was justified by "a guest that uses SIGTRAP as a control-flow signal would
    // be interrupted" — and that fear is already handled by
    // [`keep_the_fault_signals`]: a guest that uses SIGTRAP installs a handler
    // for it, and `ROSETTA3_KEEP_FAULT_HANDLER` (which the test script sets, so
    // every run log carries the "will be accepted and ignored" line) makes the
    // guest's handler win. So the gate bought nothing, and it cost the
    // diagnostic: **the current wall is a bare `int3` at
    // `Framework+0x3ab0d2b` in Chromium's sandbox-parameter setter**, so a
    // default `run-chromium.sh` printed `exit 133` and *no `fault:` block at
    // all*, and "no fault line" had to be read as "a trap the runtime does not
    // catch" rather than "no fault".
    //
    // A guest that has installed no SIGTRAP handler of its own cannot be
    // disturbed by ours, because it has no handler to disturb.
    let signals: Vec<i32> = vec![
        libc::SIGSEGV,
        libc::SIGBUS,
        libc::SIGABRT,
        libc::SIGTRAP,
    ];
    for signal in signals {
        // SAFETY: `action` and `old` are live locals of the right type;
        // `sa_sigaction` holds a handler with the `SA_SIGINFO` signature, which
        // is what `SA_SIGINFO` in the flags declares it to be. Installing a
        // handler is what this function is.
        let old = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = report as *const () as usize;
            // `SA_NODEFER` because the handler reads memory a fault has just
            // said nothing can be trusted about, and so can fault itself. The
            // signal is blocked inside its own handler by default, and a
            // synchronous fault raised while it is blocked is forced to
            // `SIG_DFL` and kills the process on the spot - so without this the
            // second fault is not a second report, it is silence. `REPORTING`
            // is what keeps the re-entry to one level; see [`salvage`].
            action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_NODEFER;
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(signal, &action, &mut old) != 0 {
                continue;
            }
            old
        };
        let saved = saved_for(signal);
        saved.handler.store(old.sa_sigaction, Ordering::SeqCst);
        // SAFETY: `install` runs once during start-up. A fault arriving between the
        // `sigaction` above and this line reads the empty mask, which is what the
        // atomic it replaced would have held too.
        unsafe { saved.mask.set(darwin::signal::Mask::of(&old)) };
        saved.flags.store(old.sa_flags, Ordering::SeqCst);
    }
}

/// Set to stop the guest from replacing the handlers [`install`] just put in.
///
/// A guest with its own crash reporter installs a `SIGSEGV` handler within the
/// first second, and from then on a fault produces whatever that reporter says -
/// which for a program whose reporter has been stubbed out is nothing at all,
/// and the process exits 139 with no pc, no image and no symbol. Set this and
/// the report below is what runs instead.
///
/// Off by default: the guest's handler is part of the guest, and a program that
/// recovers from a fault by design does not survive having its handler ignored.
pub const KEEP_FAULT_HANDLER_ENV: &str = "ROSETTA3_KEEP_FAULT_HANDLER";

fn keep_the_fault_signals() {
    if !std::env::var_os(KEEP_FAULT_HANDLER_ENV).is_some_and(|v| v != "0") {
        return;
    }
    // `SIGTRAP` alongside `SIGILL`, and for the same reason they are both
    // already here: `ImmediateCrash` is a trap, and a guest's own crash
    // reporter takes a `SIGTRAP` handler for a breakpoint long before it takes
    // one for `ud2`.
    let mut signals: Vec<i32> = vec![libc::SIGSEGV, libc::SIGBUS, libc::SIGABRT, libc::SIGILL];
    if stack_who() {
        signals.push(libc::SIGTRAP);
    }
    for signal in signals {
        // The registry is keyed by Darwin's numbering, which is what the guest's
        // `sigaction` is given. `SIGBUS` is 10 there and 7 here.
        if let Some(darwin) = darwin::signal::signal_from_host(signal) {
            darwin::signal::retain(darwin);
        }
    }
    install_alternate_stack();
    darwin::signal::retain_altstack();
    eprintln!(
        "rosetta3: the guest's own handlers for SIGSEGV, SIGBUS, SIGABRT and SIGILL will be \
         accepted and ignored ({KEEP_FAULT_HANDLER_ENV} is set)"
    );
}

/// An alternate signal stack of the runtime's own, big enough for the report.
///
/// `SA_ONSTACK` means "run on whatever alternate stack this thread has", and the
/// thread has exactly one. Crashpad installs `MINSIGSTKSZ` - two kilobytes - and
/// from then on the report below was being composed in it, faulting on its own
/// frame. Installed here, and [`darwin::signal::retain_altstack`] keeps it.
///
/// Per thread, because `sigaltstack` is: a thread without one runs the handler
/// on its own stack, and the fault which brought the handler here is often a
/// wild pointer that has already made that stack unusable. A guest thread must
/// call this before it enters the guest.
pub fn install_alternate_stack() {
    /// Room for the report's frames with a wide margin. Leaked deliberately:
    /// this stack has to outlive everything, including a fault during exit.
    const BYTES: usize = 512 * 1024;

    // SAFETY: reading the disposition this thread already has.
    let existing = unsafe {
        let mut current: libc::stack_t = std::mem::zeroed();
        libc::sigaltstack(std::ptr::null(), &mut current);
        current
    };
    if existing.ss_size >= BYTES && existing.ss_flags & libc::SS_DISABLE == 0 {
        return;
    }

    let stack = Box::leak(vec![0u8; BYTES].into_boxed_slice());
    // SAFETY: `stack` is a live, leaked, writable allocation of `BYTES` bytes,
    // which is what `sigaltstack` is being told.
    unsafe {
        let mut wanted: libc::stack_t = std::mem::zeroed();
        wanted.ss_sp = stack.as_mut_ptr().cast();
        wanted.ss_size = BYTES;
        wanted.ss_flags = 0;
        libc::sigaltstack(&wanted, std::ptr::null_mut());
    }
}

/// The disposition [`install`] displaced, taken apart into fields a handler may read
/// without a lock.
///
/// A `sigaction` in a `static mut` would be the obvious shape and the wrong one.
struct Saved {
    handler: AtomicUsize,
    /// Not an atomic: a `sigset_t` is a word on Darwin and a 128-byte structure under
    /// glibc, and there is no atomic of that size. Written once by [`install`] and
    /// read from the handler thereafter, which is the discipline
    /// [`darwin::signal::StaticMask`] is for.
    mask: darwin::signal::StaticMask,
    flags: AtomicI32,
}

const fn saved() -> Saved {
    Saved {
        handler: AtomicUsize::new(libc::SIG_DFL),
        mask: darwin::signal::StaticMask::empty(),
        flags: AtomicI32::new(0),
    }
}

static SEGV: Saved = saved();
static BUS: Saved = saved();
static ABRT: Saved = saved();
/// Written only when [`STACK_WHO_ENV`] is set; see [`install`].
static TRAP: Saved = saved();

fn saved_for(signal: i32) -> &'static Saved {
    match signal {
        libc::SIGBUS => &BUS,
        libc::SIGABRT => &ABRT,
        libc::SIGTRAP => &TRAP,
        _ => &SEGV,
    }
}

/// The report being composed, in a `static` rather than on the handler's stack.
///
/// The report is one `write` so that two threads faulting together cannot
/// interleave, and that is the reason it is built up before any of it is
/// printed. The cost is that a fault *inside* the handler used to take the whole
/// report with it - the header, the pc, the registers - and the process then
/// exited with no output at all, which is the hardest of the three endings to
/// work with. Here the buffer outlives the frame that filled it, so the nested
/// entry can print what was known before it faulted; see [`salvage`].
struct Shared(std::cell::UnsafeCell<Buffer>);

/// SAFETY: `REPORTING` admits one writer, and the only other access is
/// [`salvage`] on the way out of the process.
unsafe impl Sync for Shared {}

static REPORT: Shared = Shared(std::cell::UnsafeCell::new(Buffer::empty()));

/// Whether [`REPORT`] holds a report that has not been printed.
static BUILDING: AtomicBool = AtomicBool::new(false);

/// The section the handler is in, so that a report which stops names where.
///
/// Every section below the registers reads memory the fault has just said
/// nothing can be trusted about, and the one which faults takes the rest of the
/// report with it. Without this the report ended at the last line that had
/// already been appended, which is a different place from the one that faulted
/// and says nothing about which walk to distrust.
static STAGE: AtomicUsize = AtomicUsize::new(0);

const STAGES: [&str; 7] = [
    "the header",
    "the faulting address",
    "the host pc and its region",
    "the registers",
    "the thread",
    "the guest stack",
    "the report",
];

fn stage(which: usize) {
    STAGE.store(which, Ordering::SeqCst);
}

/// This thread, as the kernel numbers it.
///
/// `gettid` rather than anything in `std`: the answer is needed inside a signal
/// handler, where the only calls allowed are the async-signal-safe ones.
fn thread_id() -> i32 {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::gettid()
    }
    #[cfg(target_os = "freebsd")]
    // SAFETY: as above; FreeBSD spells it `pthread_getthreadid_np`, which is
    // the kernel's own thread id and is safe to call from a handler.
    unsafe {
        libc::pthread_getthreadid_np()
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    // SAFETY: as above; Darwin spells it `pthread_mach_thread_np(self)`, and
    // the port number is as good an identity as the kernel's own tid.
    unsafe {
        libc::pthread_mach_thread_np(libc::pthread_self()) as i32
    }
}

/// Print a report the handler faulted part-way through building.
fn salvage() {
    if !BUILDING.swap(false, Ordering::SeqCst) {
        return;
    }
    // SAFETY: the swap above hands this over exactly once, and the buffer is a
    // `static` - the frame that was filling it may be gone, its storage is not.
    let out = unsafe { &mut *REPORT.0.get() };
    out.str("\n  (the handler faulted while reporting ");
    out.str(
        STAGES
            .get(STAGE.load(Ordering::SeqCst))
            .copied()
            .unwrap_or("an unnamed section"),
    );
    out.str("; the rest of the report is missing)\n");
    out.flush();
}

/// The handler. Prints, restores the previous disposition, and returns.
extern "C" fn report(signal: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    // A fault *inside* this handler would recurse until the alternate stack ran
    // out, which would replace one unexplained crash with another.
    static REPORTING: AtomicBool = AtomicBool::new(false);
    /// Which thread holds [`REPORTING`], so that a second entry can tell the
    /// two reasons for one apart.
    static REPORTER: AtomicI32 = AtomicI32::new(0);
    let me = thread_id();
    if REPORTING.swap(true, Ordering::SeqCst) {
        // A *nested* fault: this thread was already building a report and the
        // handler itself faulted. What it had is worth printing.
        if REPORTER.load(Ordering::SeqCst) == me {
            salvage();
            restore(signal);
            return;
        }
        // A *second* thread. It has its own fault to report and must not touch
        // the buffer the first one is still writing: a whole process coming
        // down takes several threads with it, and salvaging here used to print
        // the first thread's half-built report and call it a handler crash.
        // One line, from the kernel's own words, and out of the way.
        let mut line = Buffer::empty();
        line.str("rosetta3: fault: ");
        line.str(match signal {
            libc::SIGBUS => "SIGBUS",
            libc::SIGSEGV => "SIGSEGV",
            libc::SIGABRT => "SIGABRT",
            libc::SIGTRAP => "SIGTRAP",
            _ => "signal",
        });
        line.str(" on thread ");
        line.hex(me as u32 as u64);
        line.str(" at host pc ");
        line.hex(host_pc(context).unwrap_or(0));
        line.str(" - another thread is reporting its own fault first\n");
        line.flush();
        restore(signal);
        return;
    }
    REPORTER.store(me, Ordering::SeqCst);

    // SAFETY: the swap above is what makes this the only writer - every other
    // entry to the handler takes the branch that only reads it, and does so on
    // its way out of the process.
    let out = unsafe { &mut *REPORT.0.get() };
    BUILDING.store(true, Ordering::SeqCst);
    stage(0);
    out.str("rosetta3: fault: ");
    out.str(match signal {
        libc::SIGBUS => "SIGBUS",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGABRT => "SIGABRT",
        libc::SIGTRAP => "SIGTRAP",
        _ => "signal",
    });

    // SAFETY: the kernel passes a live `siginfo_t` for the duration of the
    // handler; `si_addr` is the faulting address for both signals handled here.
    stage(1);
    let addr = unsafe { info.as_ref() }.map_or(0, |i| unsafe { darwin::signal::fault_address(i) });
    out.str(" at ");
    out.hex(addr);
    // `say_access` reads `si_code` against SIGSEGV's meanings, and those are 1
    // for "unmapped" and 2 for "not permitted". SIGTRAP's are 1 for *breakpoint*
    // and 2 for *divide by zero*, so the same lookup would name both wrongly.
    // Silent is the correct answer for anything it was not written for.
    if matches!(signal, libc::SIGSEGV | libc::SIGBUS) {
        say_access(out, info);
    }
    if let Some(text) = ascii_word(addr) {
        out.str(" (ascii \"");
        out.bytes(&text);
        out.str("\")");
    }

    // The registers and the thread before the two walks below, because those two
    // read memory the fault has just said nothing can be trusted about, and a
    // handler that faults leaves only what [`salvage`] finds in the buffer.
    if let Some(pc) = host_pc(context) {
        out.str("\n  host pc ");
        out.hex(pc);
        stage(2);
        say_region(out, pc);
    }
    stage(3);
    say_registers(out, context);
    stage(4);
    say_thread(out);
    stage(5);
    // Everything above is read from the kernel's own `siginfo_t` and
    // `ucontext_t` and cannot fault. Everything below walks the guest's stack
    // and its images, and can. Printed here so the two arrive separately rather
    // than not at all - [`salvage`] covers the case where the handler dies
    // before it can add the rest.
    out.flush();

    if let Some(pc) = host_pc(context) {
        // Before the frame walk, because when it applies it is the answer and
        // the walk is not: a jump to zero has no frame of its own.
        if pc == 0 {
            say_unbound_call(out, context);
        }
        // The one guest site whose object this runtime has something to say
        // about. Narrow on purpose; see `say_controller`.
        say_controller(out, context);
        if !in_translation(pc) {
            say_caller(out, context);
        }
    }
    // An abort has no interesting pc of its own - it is always inside the C
    // library - so the frame walk is the whole report. Nor does a trap: the
    // `int3` is in guest code, but the `pc` the kernel hands back is only as
    // good as the handler below, and the walk is what says anything.
    else if matches!(signal, libc::SIGABRT | libc::SIGTRAP) {
        say_caller(out, context);
    }
    // The whole guest stack, not just its first guest frame. Additive, and off
    // unless `ROSETTA3_ABORT_WHO` is set; see `say_guest_stack` for why the
    // report above cannot answer "which of the many identical reports fired".
    say_guest_stack(out, context);

    stage(6);
    out.str("\n");
    BUILDING.store(false, Ordering::SeqCst);
    out.flush();

    // Handed back, so that a later fault - a different one, on any thread -
    // gets a report of its own rather than the line above.
    REPORTER.store(0, Ordering::SeqCst);
    REPORTING.store(false, Ordering::SeqCst);
    restore(signal);
}

/// Byte offset of `uc_mcontext` within Darwin's `ucontext_t`, where it is a
/// pointer to the thread state. The same on both architectures: nothing ahead of
/// it is architecture-dependent. Linux inlines its `mcontext_t` instead, so this
/// is only ever applied under `target_os = "macos"`.
#[cfg(target_os = "macos")]
const UC_MCONTEXT: usize = 48;

/// Whether a host `pc` is inside a translation at all.
fn in_translation(pc: u64) -> bool {
    PLACED
        .try_lock()
        .is_ok_and(|placed| covering(&placed, pc).is_some())
}

/// Name the guest instruction the fault was reached *from*, when the faulting
/// `pc` is host code.
///
/// This is the common shape of a guest bug: a null pointer handed to a host
/// library faults inside that library, where the report has nothing to say.
/// Which guest instruction passed it is the whole answer, and it is one frame
/// chain away.
fn say_caller(out: &mut Buffer, context: *mut libc::c_void) {
    /// How far to walk before giving up. A guest calling into a host library
    /// is a handful of frames; a hundred means the chain is not one.
    const FRAMES: usize = 128;

    let Some((mut fp, sp, lr)) = host_frame(context) else {
        return;
    };
    let candidates = lr.into_iter().chain(std::iter::from_fn(|| {
        let (next, ret) = readable(fp, sp).and_then(|f| frame(f))?;
        fp = next;
        Some(ret)
    }));
    // A translation under the emulated backend, and an image the loader mapped
    // under the native one - where there are no translations at all, so looking
    // only for those answers "nothing" for every fault the program has.
    let found = candidates
        .take(FRAMES)
        .find(|&ret| in_translation(ret) || in_image(ret));
    let Some(ret) = found else {
        out.str("\n  reached from: no host frame returns into guest code");
        return;
    };
    out.str("\n  reached from host pc ");
    out.hex(ret);
    if in_translation(ret) {
        say_region(out, ret);
    } else {
        out.str(" in");
        let _ = say_image(out, ret);
    }
}

/// Say what is in the `ThreadControllerImpl` the guest faulted on.
///
/// A one-shot diagnostic, and deliberately narrow. `Framework+0x5d46240` is
/// `mov ecx, DWORD PTR [rax]` with `rax = [r14+0x218]` null, inside
/// `base::sequence_manager::internal::ThreadControllerImpl::DoWork` (the four
/// `__cstring` literals in that range name it: `ThreadControllerImpl::DoWork`,
/// `::RunTask`, `ThreadController: application tasks disallowed`).
///
/// `+0x218` is written by exactly one place - the bind step at
/// `Framework+0x5d45440`, which asks the guest's own `thread_local` accessor
/// `0x5cd7420` for a `uint32_t` cell and stores the pointer. That accessor has
/// no path returning 0, so a null field means the bind never ran on this
/// object, not that the cell was missing.
///
/// There are two factories, and they differ in exactly this:
///
/// | factory | allocates | binds |
/// |---|---|---|
/// | `0x5d29050` | `new(0x180)`, ctor `0x5d42e50` | **no** |
/// | `0x5d291a0` | `new(0x270)`, ctor `0x5d44e40` | yes, via `0x5d29254` |
///
/// So the fields printed here are the cheapest way to characterise the object,
/// without changing the guest. `[r14]` and `[r14+0xf0]` are its two vtable
/// pointers (Itanium puts the second base's vptr at the subobject offset, and
/// every caller passes `this-0xf0`). `0x160`, `0x240`, `0x248` and the
/// held-flag byte at `0x260` are the other fields `DoWork` touches on the way
/// to the fault, which is what shows the object is live rather than a stub.
///
/// **Which factory built it is settled, by the vtable pair.** The 0x180 class's
/// two vtable pointers are `0xe268390` and `0xe268448` - `0xb8` apart - and the
/// 0x270 class's are `0xe268490` and `0xe268598` - `0x108` apart. The object
/// faulting here has a pair `0x108` apart, so **it is the 0x270 class**: the one
/// the *bound* factory builds, which calls the bind at `0x5d29254` immediately
/// after constructing it.
///
/// So the contradiction is real rather than a misreading on my part. A class
/// whose factory unconditionally calls a function whose store is unconditional
/// has a field that store writes reading null. Either the bind ran and something
/// cleared the field afterwards, or it ran against a different object.
/// **Left unexplained deliberately** - a tidy account here would be a guess, and
/// this is the live blocker.
///
/// This prints once per distinct faulting pc. It reads guest addresses with a
/// null check on each word, because the whole point is to inspect an object
/// that is already partly broken.
fn say_controller(out: &mut Buffer, context: *mut libc::c_void) {
    use std::sync::Mutex;
    static SAID: Mutex<Vec<u64>> = Mutex::new(Vec::new());

    let Some(pc) = host_pc(context) else {
        return;
    };
    // The pc is a *slid* address, so compare the image-relative offset the
    // report itself prints, not the raw pc.
    let Some(offset) = image_offset(pc) else {
        return;
    };
    let pc = offset;
    // Report a nominated range from here too, so `ROSETTA3_PC_WATCH` still works
    // on a run that faults inside the range. See `watch_guest_pc`.
    watch_guest_pc(host_pc(context).unwrap_or(0));
    // second base's vptr at the subobject offset, and every caller of this
    // function passes `this - 0xf0`, so `r14` is the `ThreadControllerImpl*`.
    #[cfg(target_arch = "x86_64")]
    let r14 = {
        if context.is_null() {
            return;
        }
        // SAFETY: the kernel passes a live `ucontext_t` for the handler's
        // duration and fills `gregs` itself.
        let gregs = unsafe { &(*(context as *const libc::ucontext_t)).uc_mcontext.gregs };
        gregs[libc::REG_R14 as usize] as u64
    };
    #[cfg(not(target_arch = "x86_64"))]
    let r14: u64 = {
        return;
    };
    // `r14` is the guest object base on x86-64. The Itanium layout puts the
    // Only this one site: the point is to explain 0x5d46240, not to decorate
    // every fault the program ever takes.
    const SITE: u64 = 0x5d46240;
    if pc != SITE {
        return;
    }
    {
        let Ok(mut said) = SAID.lock() else { return };
        if said.contains(&pc) {
            return;
        }
        said.push(pc);
    }

    // A guest word, or `None` if it is not mapped. Every read is checked: this
    // runs *because* the object has a null in it.
    let gword = |at: u64| -> Option<u64> {
        if at == 0 || at % 8 != 0 {
            return None;
        }
        // SAFETY: guarded by the null/alignment check, and a wild guest address
        // only means this one line does not print.
        Some(unsafe { std::ptr::read_volatile(at as *const u64) })
    };
    let gbyte = |at: u64| -> Option<u8> {
        if at == 0 {
            return None;
        }
        // SAFETY: as above.
        Some(unsafe { std::ptr::read_volatile(at as *const u8) })
    };

    out.str("\n  ThreadControllerImpl (DoWork):");
    out.str("\n    r14");
    out.hex(r14);
    for (name, at) in [
        ("[r14] vtable", 0x0),
        ("[r14+0x0f0] NestingObserver vtable", 0x0f0),
        ("[r14+0x150] bound flag", 0x150),
        ("[r14+0x160]", 0x160),
        ("[r14+0x178] work_batch_size_", 0x178),
        ("[r14+0x190]", 0x190),
        ("[r14+0x1e8] work_deduplicator_ state", 0x1e8),
        ("[r14+0x210]", 0x210),
        ("[r14+0x218] thread_local cell (NULL here)", 0x218),
        ("[r14+0x240]", 0x240),
        ("[r14+0x248] the 10s wait event", 0x248),
    ] {
        out.str("\n    ");
        out.str(name);
        match gword(r14.wrapping_add(at)) {
            Some(v) => out.hex(v),
            None => out.str("unreadable"),
        }
    }
    match gbyte(r14.wrapping_add(0x260)) {
        Some(v) => {
            out.str("\n    [r14+0x260] held flag = ");
            out.str(if v == 0 { "0" } else { "non-zero" });
        }
        None => out.str("\n    [r14+0x260] held flag unreadable"),
    }
    out.str("\n    -> +0x218 has exactly one writer, the bind at 0x5d45440");
    out.str("\n       (store at 0x5d454d7, unconditional), and exactly one");
    out.str("\n       clearer, the teardown at 0x5d4514e:");
    out.str("\n         5d4514e  mov QWORD PTR [rbx+0x218], 0x0");
    out.str("\n    -> +0x150 is written by the bind at 0x5d454f1, four");
    out.str("\n       instructions LATER, and the teardown never touches it.");
    out.str("\n       So +0x150 == 1 together with +0x218 == 0 is the signature");
    out.str("\n       of BOUND, THEN DESTROYED: a use-after-free. The bind is");
    out.str("\n       proven to have run; the object is simply no longer alive.");
    out.str("\n    -> the two vtables above deslide to 0xe268490/0xe268598, so");
    out.str("\n       r14 is a correctly aligned 0x270 controller base. No");
    out.str("\n       misalignment, no offset shift, and both vtables share");
    out.str("\n       one slide, which is what makes that check meaningful.");
    out.str("\n    -> the question is who invokes DoWork after the teardown. It");
    out.str("\n       arrives through a closure: new(0x38), vtable 0xe1e7f48,");
    out.str("\n       DoWork at slot +0x10, built in Blink's");
    out.str("\n       non_main_thread_impl.cc and gpu_child_thread.cc, bound");
    out.str("\n       at Framework+0x4b47edf.");
    // Separate output buffer: the report above is already long, and truncation
    // is silent. Additive, and off unless its own variable is set.
    out.flush();
    say_dowork_who(out, context);
    // Also separate and also off by default: the controller's own words, so
    // the layout claims above are checkable rather than asserted.
    out.flush();
    say_tc_layout(out, r14);
    // Its own switch, and deliberately *not* nested inside `say_tc_layout`: the
    // two answer different questions and must be settable independently.
    out.flush();
    say_tls_cell(out);
}

/// Dump the stack around the fault so the *caller* can be identified from what is
/// actually there, rather than from an assumed frame layout. Additive, and off
/// unless `ROSETTA3_DOWORK_WHO` is set.
///
/// Added for the live blocker, and only for it. `Framework+0x5d46240` is reached
/// from `Framework+0x5cd7329`, which is the instruction after `call 0x5d46020`
/// at `0x5cd7324` inside the function at `0x5cd72b0`. That function builds the
/// receiver as its **second argument** minus `0xf0` - `add r14,
/// 0xffffffffffffff10` at `0x5cd72da`, then `mov rsi, r14` at `0x5cd7321` - so
/// the question is who supplies that argument. `0x5cd72b0` has no direct caller
/// at all: it is reached only through the vtable at `0xe1e7f48`, whose address
/// point is installed in exactly two places, `0x5cd70d5` and `0xc8bf471`.
///
/// A frame-pointer walk cannot answer it, and the reason is worth recording:
/// `0x5cd72b0` establishes its own frame, so the slot holding the return address
/// into *it* is the first thing above the faulting `rbp` and everything older is
/// above that. That is exactly why [`say_caller`] reports `0x5cd7329` and stops,
/// correctly - the frames that matter are past its reach.
///
/// So this measures instead of computing. Counting a prologue to place saved
/// registers is a guess, and a measured guess is still a guess: an earlier
/// version of this function derived the offsets, got two of the five saved
/// registers wrong while its control word was right, and would have published
/// two confident wrong addresses. It therefore prints the raw window, and then
/// probes *every* word in it that could be a frame pointer, so the chain is read
/// off the output rather than asserted.
///
/// The control is the annotation on `rbp+8`: it must say `+0x5cd7329`, the
/// address the main report has already named by a different route. If it does
/// not, `rbp` is not the frame this claims and nothing below it means anything.
fn say_dowork_who(out: &mut Buffer, context: *mut libc::c_void) {
    /// What to look for, so the variable is a switch and not a tuning knob.
    const ENV: &str = "ROSETTA3_DOWORK_WHO";
    /// Window around the faulting frame pointer. `0x5cd72b0`'s frame is 0x60
    /// bytes below its own `rbp` and the frames above it start immediately after,
    /// so this is generous rather than tight.
    const BELOW: i64 = 0x60;
    const ABOVE: i64 = 0x200;
    /// How far above `rbp` a word may be and still be worth probing as a frame
    /// pointer. The main thread's stack is 8 MB; a guest frame is not 2 KB.
    const PROBE: u64 = 0x100000;

    if !std::env::var_os(ENV).is_some_and(|v| v != "0") {
        return;
    }
    let Some((rbp, sp, _)) = host_frame(context) else {
        out.str("\n  DOWORK_WHO: no frame pointer for this thread");
        return;
    };
    // A word from the window. `at` is a whole number of eight bytes from an
    // eight-aligned `rbp` and no more than `ABOVE`/`BELOW` away, and the faulting
    // thread's stack is mapped at least `sp` bytes upwards, so it is read only
    // when it is at or above `sp`.
    let word = |at: u64| -> Option<u64> {
        (at % 8 == 0 && at >= sp).then(|| {
            // SAFETY: as above - a word inside this thread's own stack, above the
            // kernel's `rsp` for it.
            unsafe { std::ptr::read_volatile(at as *const u64) }
        })
    };
    let say_offset = |out: &mut Buffer, label: &str, at: u64, v: u64| {
        out.str("\n      ");
        out.str(label);
        out.str(" = ");
        out.hex(v);
        if in_image(v) {
            out.str("  ");
            let _ = say_image(out, v);
        }
        let _ = at;
    };

    out.str("\n  DOWORK_WHO - the stack around the call into DoWork, raw:");
    out.str("\n    rbp ");
    out.hex(rbp);
    out.str("  rsp ");
    out.hex(sp);
    out.str("  rbp-rsp ");
    out.hex(rbp.wrapping_sub(sp));
    out.flush();

    let mut control_ok = false;
    let mut candidates: Vec<u64> = Vec::new();
    for step in -BELOW..=ABOVE {
        let at = rbp.wrapping_add(step as u64);
        let Some(v) = word(at) else { continue };
        let label = if step < 0 {
            let mut s = String::from("[rbp-");
            s.push_str(&format!("{:x}", step.unsigned_abs()));
            s.push(']');
            s
        } else {
            format!("[rbp+{step:x}]")
        };
        say_offset(out, &label, at, v);
        out.flush();
        if step == 8 && image_offset(v) == Some(0x5cd7329) {
            control_ok = true;
        }
        // Anything that could be a frame pointer of an outer frame: on the stack,
        // eight-aligned, and above `rbp`.
        if v >= rbp && v % 8 == 0 && v <= rbp.saturating_add(PROBE) {
            candidates.push(v);
        }
    }

    out.str("\n    control: [rbp+8] is ");
    if control_ok {
        out.str("Framework+0x5cd7329, as it must be");
    } else {
        out.str("NOT Framework+0x5cd7329");
    }
    out.str(" - ");
    out.str(if control_ok {
        "so the window above is the real frame."
    } else {
        "so this window is not the frame it claims and NONE of it is evidence."
    });
    out.str("\n    probing every word above rbp that could be a frame pointer:");
    out.flush();
    let mut probed: Vec<u64> = Vec::new();
    for fp in candidates {
        if probed.contains(&fp) {
            continue;
        }
        probed.push(fp);
        let Some((next, ret)) = frame(fp) else {
            continue;
        };
        out.str("\n      frame ");
        out.hex(fp);
        out.str(" -> next ");
        out.hex(next);
        out.str(", returns to ");
        out.hex(ret);
        if in_image(ret) {
            out.str("  ");
            let _ = say_image(out, ret);
        }
        out.flush();
    }
    out.flush();
}

/// `ROSETTA3_ABORT_WHO` - the switch for the guest-stack dump.
///
/// It is also the switch for *catching* the signal that reaches it, because
/// SIGTRAP is not handled at all otherwise; see [`install`].
const STACK_WHO_ENV: &str = "ROSETTA3_ABORT_WHO";

/// Whether [`STACK_WHO_ENV`] asks for the guest stack.
///
/// Read from [`install`] before any guest code runs, and from a signal handler
/// afterwards, so it is the plain `var_os` test rather than a cached copy: a
/// cached `OnceLock` would be fine in the handler but would have to be a
/// different function for the start-up read, and one switch read twice is
/// simpler to reason about than two.
fn stack_who() -> bool {
    std::env::var_os(STACK_WHO_ENV).is_some_and(|v| v != "0")
}

/// The `/proc/self/maps` entry that contains `at`, as `(start, end)`.
///
/// A signal handler that faults taking its own report with it loses everything
/// after the faulting word, so a walk over a thread's stack needs a bound it can
/// *establish* rather than one it assumes. This is the same file and the same
/// field layout [`say_host_mapping`] already parses, read a second time for the
/// one number a walk needs, and it is the only reason [`say_guest_stack`] can
/// say where it stopped.
///
/// `None` off Linux, on a host without `/proc`, or when nothing contains the
/// address - in which case the caller falls back to its own declared window and
/// says so.
#[cfg(target_os = "linux")]
fn stack_bounds(at: u64) -> Option<(u64, u64)> {
    // SAFETY: a constant path and constant flags.
    let fd = unsafe {
        libc::open(
            c"/proc/self/maps".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    // The same small buffer and the same reason as `say_host_mapping`: this runs
    // on a signal stack.
    let mut buf = [0u8; 512];
    let mut held = 0usize;
    let mut found = None;
    'reading: loop {
        // SAFETY: writing into the unused tail of a live local array.
        let read = unsafe { libc::read(fd, buf[held..].as_mut_ptr().cast(), buf.len() - held) };
        if read <= 0 {
            break;
        }
        let filled = held + read as usize;
        let mut start = 0;
        while let Some(end) = buf[start..filled].iter().position(|b| *b == b'\n') {
            let line = &buf[start..start + end];
            if let Some(range) = maps_range(line) {
                if (range.0..range.1).contains(&at) {
                    found = Some(range);
                    break 'reading;
                }
            }
            start += end + 1;
        }
        held = filled - start;
        if held == buf.len() {
            held = 0;
        } else {
            buf.copy_within(start..filled, 0);
        }
    }
    // SAFETY: the descriptor opened above, which nothing else holds.
    unsafe { libc::close(fd) };
    found
}

/// The `lo-hi` a `/proc/self/maps` line starts with, if it starts with one.
#[cfg(target_os = "linux")]
fn maps_range(line: &[u8]) -> Option<(u64, u64)> {
    let (low, rest) = hex_prefix(line);
    let rest = rest.strip_prefix(b"-")?;
    let (high, _) = hex_prefix(rest);
    (high > low).then_some((low, high))
}

#[cfg(not(target_os = "linux"))]
fn stack_bounds(_at: u64) -> Option<(u64, u64)> {
    None
}

/// Dump every guest return address on the faulting thread's stack, with the call
/// edge each one implies decoded out of the image.
///
/// Additive, and off unless `ROSETTA3_ABORT_WHO` is set. `ROSETTA3_DOWORK_WHO`
/// prints the raw words around one frame because a frame-pointer walk could not
/// reach the frames that mattered; this is the same raw window, filtered - and
/// the filter is the part that is new.
///
/// **Why this exists.** `check.cc(361)` is a *constant*, not a location.
/// `base/check.cc:358-363` is `NotReachedError::TriggerNotReached()`, which
/// calls `NotReached() << "..."` **with no location argument**, so the
/// `Location()` carries `check.cc`'s own `__FILE__:__LINE__`. Every migrated
/// `NOTREACHED_IN_MIGRATION()` in this build prints that identical line, there
/// are thousands of them, and the log cannot tell them apart. The only thing
/// that can is the stack: the caller of `TriggerNotReached` is the site.
///
/// **The filter, and why it is not a guess.** A word on the stack is reported
/// when it is inside a guest image *and* the byte five before it in that image
/// is `0xe8` (a `call rel32` that ends exactly there) or `0xe9` (a `jmp rel32`,
/// i.e. a tail call). The return address of a real call is by construction the
/// address just past the call, and on x86-64 a direct call *is* `e8` with its
/// displacement last, so `target = v - 5 + disp32` is the call's own operand
/// rather than a reconstruction - it is decoded from the same bytes the guest
/// executed. A word of data that happens to hold an image address is preceded
/// by whatever that data is, and is dropped. An indirect call (`ff 15` / `ff
/// 25`, six bytes) is recognised and reported without a target, because its
/// target is in memory rather than in the instruction.
///
/// The negative is bounded and stated: a `call` through a *register* or a
/// *trampoline* leaves a return address this filter drops, so a caller reached
/// only that way is missing from the list. That is a reason to re-read the
/// disassembly at whatever the list names, not a reason to trust the list
/// alone.
///
/// **The control is in the same run.** The frame-pointer chain is walked first,
/// with [`frame`] - which the rest of this file has used for every report, and
/// which is not a reconstruction of anything. Every address that walk produces
/// is *also* an eight-aligned word at or above `rsp`, so the scan must find
/// every one of them. The report says how many it found and names any it
/// missed. If the two routes disagree, the list is not evidence and the report
/// says so.
fn say_guest_stack(out: &mut Buffer, context: *mut libc::c_void) {
    /// `ROSETTA3_ABORT_WHO=1` - off by default, and the only thing this reads.
    const ENV: &str = STACK_WHO_ENV;
    /// How far above the kernel's `rsp` to look, and *also* a bound on the read:
    /// the scan stops at the top of the mapping that holds `rsp` (see
    /// [`stack_bounds`]), because a signal handler that faults taking its own
    /// report with it loses everything after the faulting word. The frames that
    /// matter are the innermost, which sit in the first three kilobytes, so this
    /// is slack rather than a choice: on the run that produced the first report
    /// the sixteenth frame was 0xda8 bytes in and the chain already reached the
    /// launcher's `main`.
    const WINDOW: u64 = 64 * 1024;
    /// Cap on reported candidates, so a stack full of stale frames cannot
    /// produce an unbounded log. The innermost ones come first, so the cap
    /// costs the shallowest and not the answer.
    const MAX_LINES: usize = 400;
    /// How far the frame-pointer chain is followed. A guest calling a host
    /// library is a handful of frames; a few hundred is start-up.
    const CHAIN_FRAMES: usize = 256;
    /// Guest addresses the frame chain contributed, kept for the cross-check.
    /// A fixed array rather than a `Vec`: nothing in a signal handler should
    /// allocate, and the existing `Vec`s in this file are the one thing here
    /// that is worth *not* copying.
    const CHAIN_MAX: usize = 64;

    if !std::env::var_os(ENV).is_some_and(|v| v != "0") {
        return;
    }
    let Some((rbp, sp, _)) = host_frame(context) else {
        out.str("\n  ABORT_WHO: no frame pointer for this thread");
        return;
    };
    // The image ranges, taken once. `in_image` and the module's `image_offset`
    // each take the lock, and this walks half a megabyte of stack; one snapshot
    // is both cheaper and simpler to reason about. Refused outright if another
    // thread holds the lock, because there would then be nothing to attribute
    // to and a list of bare numbers is not worth printing.
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    let mut names: Vec<[u8; NAME]> = Vec::new();
    let mut used: Vec<usize> = Vec::new();
    {
        let Ok(images) = IMAGES.try_lock() else {
            out.str("\n  ABORT_WHO: another thread holds the image table");
            return;
        };
        for image in images.iter().filter(|i| i.range.1 > i.range.0).take(16) {
            ranges.push(image.range);
            let mut name = [0u8; NAME];
            name[..image.used].copy_from_slice(&image.name[..image.used]);
            names.push(name);
            used.push(image.used);
        }
    }
    let which = |addr: u64| ranges.iter().position(|r| (r.0..r.1).contains(&addr));
    // `(image index, offset in it)`, or `None` for no recorded image.
    let offset_of = |addr: u64| which(addr).map(|i| (i, addr - ranges[i].0));
    // `Chromium Framework+0x5cb87d0`, rather than a bare offset: two images are
    // live and a bare offset names neither.
    let say_where = |out: &mut Buffer, at: u64| -> bool {
        let Some((i, off)) = offset_of(at) else {
            return false;
        };
        out.bytes(&names[i][..used[i]]);
        out.str("+");
        out.hex(off);
        true
    };

    // One guest byte, by absolute address. The `which` check is what makes the
    // read safe: it is inside an image the loader mapped.
    let byte = |addr: u64| -> Option<u8> {
        which(addr)?;
        // SAFETY: `which` established that `addr` is inside a mapped guest
        // image. A wild address simply failed that test and nothing is read.
        Some(unsafe { std::ptr::read_volatile(addr as *const u8) })
    };
    let rel32 = |addr: u64| -> Option<i32> {
        let bytes = [
            byte(addr)?,
            byte(addr.wrapping_add(1))?,
            byte(addr.wrapping_add(2))?,
            byte(addr.wrapping_add(3))?,
        ];
        Some(i32::from_le_bytes(bytes))
    };
    // The top of the mapping that holds this thread's stack, so the scan below
    // has a bound it established rather than one it assumed. `None` leaves the
    // declared window, and the report says where it stopped either way.
    let top = stack_bounds(sp).map_or(sp.saturating_add(WINDOW), |(_, hi)| {
        sp.saturating_add(WINDOW).min(hi)
    });
    // A stack word. The guard is the *stack*, not the image table: an earlier
    // version of this read tested `which(addr)` - the address being read - and
    // so rejected every word, because a stack address is in no guest image.
    // Section B came out empty and section C said so; the control earned its
    // keep on its first run.
    let word = |addr: u64| -> Option<u64> {
        (addr % 8 == 0 && addr >= sp && addr < top).then(|| {
            // SAFETY: a word inside this thread's own stack, above the kernel's
            // `rsp` for it and below the top of the mapping holding it.
            unsafe { std::ptr::read_volatile(addr as *const u64) }
        })
    };
    // The call a return address came from, if the instruction before it says
    // so. `(kind byte, target)`, or `None` when the filter rejects the word.
    //
    // `e8` sits at `v - 5`, so the instruction it starts is `v - 5`, the
    // displacement is the four bytes at `v - 4`, and the target is
    // **`v + disp`** - `(v-5) + 5 + disp`. Adding the five a second time is an
    // error this function made and the report now catches on its own: every
    // printed target was five bytes past its real entry, so none of them matched
    // `NOT_REACHED_CHAIN` and the one line that had to light up stayed dark.
    let edge = |v: u64| -> Option<(u8, u64)> {
        let opcode = byte(v.wrapping_sub(5))?;
        match opcode {
            // `call rel32` / `jmp rel32`: displacement is the last four bytes.
            0xe8 | 0xe9 => {
                let disp = rel32(v.wrapping_sub(4))?;
                Some((opcode, v.wrapping_add(disp as i64 as u64)))
            }
            // `ff 15` / `ff 25`: six bytes, and the target is in memory.
            0xff => {
                let m = byte(v.wrapping_sub(5))?;
                (m == 0x15 || m == 0x25).then_some((m, 0))
            }
            _ => None,
        }
    };
    let named_target = |off: u64| {
        NOT_REACHED_CHAIN
            .iter()
            .find(|(at, _)| *at == off)
            .map(|(_, name)| *name)
    };
    // Offsets from `NOT_REACHED_CHAIN` that some return address on this stack
    // was returned from. Recorded rather than printed-and-forgotten, so the
    // summary after section B can state an absence as well as a presence.
    let mut seen_targets: Vec<u64> = Vec::new();
    let note_target = |seen: &mut Vec<u64>, at: u64| {
        if NOT_REACHED_CHAIN.iter().any(|(want, _)| *want == at) && !seen.contains(&at) {
            seen.push(at);
        }
    };

    out.str("\n  ABORT_WHO - the guest stack at this fault, shallowest first.");
    out.str("\n    rbp ");
    out.hex(rbp);
    out.str("  rsp ");
    out.hex(sp);
    out.str("  thread ");
    out.hex(u64::from(thread_id().max(0) as u32));
    out.str("  images in the table: ");
    out.hex(ranges.len() as u64);
    for (i, range) in ranges.iter().enumerate() {
        out.str("\n      [");
        out.hex(i as u64);
        out.str("] ");
        out.bytes(&names[i][..used[i]]);
        out.str("  ");
        out.hex(range.0);
        out.str("..");
        out.hex(range.1);
    }
    out.str("\n    the faulting pc is ");
    match host_pc(context) {
        Some(pc) if offset_of(pc).is_some() => {
            out.str("IN a guest image at ");
            say_where(out, pc);
            out.str(" - it is frame 0");
        }
        Some(pc) => {
            out.str("host code, so it is not a guest frame; it is in ");
            say_host_mapping(out, pc);
        }
        None => out.str("unreadable"),
    }
    out.flush();

    // ------------------------------------------------------------------ the
    // frame-pointer chain, with the addresses it contributes kept for the
    // cross-check at the end.
    let mut chain: [(usize, u64); CHAIN_MAX] = [(0, 0); CHAIN_MAX];
    let mut chain_len = 0usize;
    out.str("\n    A. the frame-pointer chain, the route the main report used:");
    out.flush();
    let mut fp = rbp;
    let mut printed = 0usize;
    for n in 0..CHAIN_FRAMES {
        let Some(here) = readable(fp, sp) else {
            out.str("\n      (chain ends: rbp is not on this stack)");
            break;
        };
        let Some((next, ret)) = frame(here) else {
            out.str("\n      (chain ends: this frame holds no usable pair)");
            break;
        };
        if printed < MAX_LINES {
            out.str("\n      #");
            out.hex(n as u64);
            out.str(" fp ");
            out.hex(here);
            out.str(" -> fp ");
            out.hex(next);
            out.str(", returns to ");
            out.hex(ret);
            if offset_of(ret).is_some() {
                out.str(" = ");
                say_where(out, ret);
            } else {
                // Not a guest address, so the *caller* is not guest code. That is
                // worth saying out loud rather than leaving as "not in a guest
                // image": a reader otherwise assumes the walk stopped, when what
                // it means is that the chain left the guest here.
                out.str(" which is in no guest image; it is ");
                say_host_mapping(out, ret);
            }
            if let Some(e) = edge(ret) {
                out.str(if e.0 == 0xe8 {
                    "  called "
                } else {
                    "  jumped to "
                });
                if e.1 != 0 {
                    out.hex(e.1);
                    if offset_of(e.1).is_some() {
                        out.str(" = ");
                        say_where(out, e.1);
                        let (_, t) = offset_of(e.1).unwrap();
                        note_target(&mut seen_targets, t);
                        if let Some(name) = named_target(t) {
                            out.str("  <<< ");
                            out.str(name);
                        }
                    } else {
                        out.str(" (not in a guest image)");
                    }
                } else {
                    out.str("indirect");
                }
            } else if offset_of(ret).is_some() {
                out.str("  (the byte before it is not a call or jmp)");
            }
            out.flush();
            printed += 1;
        }
        if let Some((i, off)) = offset_of(ret) {
            if chain_len < CHAIN_MAX {
                chain[chain_len] = (i, off);
                chain_len += 1;
            }
        }
        fp = next;
    }
    out.str("\n      ");
    out.hex(printed as u64);
    out.str(" frame(s) printed");
    out.flush();

    // ------------------------------------------------------------------ the
    // raw scan. Every eight-aligned word at or above the kernel's `rsp`, in
    // increasing address order, so shallower frames come first.
    out.str("\n    B. every eight-aligned word at or above rsp that a call left behind:");
    out.str("\n       (shallowest first; `called` is the operand of the");
    out.str("\n        instruction the guest actually executed)");
    out.flush();
    let mut found = 0usize;
    let mut at = sp;
    let stop = top;
    while at < stop && found < MAX_LINES {
        let Some(v) = word(at) else {
            at += 8;
            continue;
        };
        if offset_of(v).is_none() {
            at += 8;
            continue;
        }
        let Some((kind, target)) = edge(v) else {
            at += 8;
            continue;
        };
        out.str("\n       [rsp+");
        out.hex(at.wrapping_sub(sp));
        out.str("] ");
        say_where(out, v);
        out.str(if kind == 0xe8 {
            "  called "
        } else {
            "  jumped to "
        });
        if target != 0 {
            out.hex(target);
            if offset_of(target).is_some() {
                out.str(" = ");
                say_where(out, target);
                let (_, t) = offset_of(target).unwrap();
                note_target(&mut seen_targets, t);
                if let Some(name) = named_target(t) {
                    out.str("  <<< ");
                    out.str(name);
                }
            } else {
                out.str(" (not in a guest image)");
            }
        } else {
            out.str("indirect (ff 15/25: the target is in memory, not the instruction)");
        }
        out.flush();
        found += 1;
        at += 8;
    }
    out.str("\n       ");
    out.hex(found as u64);
    out.str(" candidate(s) between rsp and ");
    out.hex(stop);
    out.str(" (rsp+");
    out.hex(stop.wrapping_sub(sp));
    out.str(")");
    out.flush();
    // The one line that has to light up when the `check.cc` chain is on this
    // stack. Stated as an absence too, because a decoder quietly five bytes out
    // prints a list that looks complete and names nothing - which is exactly
    // what an earlier version of [`edge`] did.
    out.str("\n       the four `check.cc` entries, each with whether some return");
    out.str("\n       address on this stack was returned FROM it:");
    let mut lit = 0usize;
    for (at, what) in NOT_REACHED_CHAIN {
        let seen = seen_targets.contains(at);
        out.str("\n         ");
        out.hex(*at);
        out.str(" ");
        out.str(what);
        out.str(if seen { "   SEEN" } else { "   not seen" });
        if seen {
            lit += 1;
        }
    }
    out.str("\n       ");
    out.hex(lit as u64);
    out.str(" of the ");
    out.hex(NOT_REACHED_CHAIN.len() as u64);
    out.str(" were on this stack");
    out.flush();

    // ------------------------------------------------------------------ the
    // control, in the same run and by a different route.
    out.str("\n    C. control: section A found ");
    out.hex(chain_len as u64);
    out.str(" guest address(es) by walking frames; section B found ");
    out.hex(found as u64);
    out.str(" by scanning memory. Every one of A's must also be in B, because a");
    out.str("\n       saved return address IS an eight-aligned word at or above rsp:");
    out.flush();
    // The comparison, without a second buffer: for each address A contributed,
    // ask whether the scan's own test would have accepted it. Deliberately the
    // same test B used, so a disagreement means B dropped a real frame rather
    // than that the two are testing different things.
    //
    // Two different reasons an address can be in A and not in B, and conflating
    // them is a false alarm: a saved "return address" that is really a *branch
    // target* - which a tail `jmp` leaves behind - is not preceded by a call,
    // so B's filter drops it by design and section A says so on the same line.
    // Only an address that *passes* the filter and is still missing means the
    // scan lost a real frame.
    let mut lost = 0usize;
    let mut rejected = 0usize;
    for slot in &chain[..chain_len] {
        let (image, off) = *slot;
        // A's own address, recovered from the range, so `edge` can be asked.
        let abs = ranges[image].0 + off;
        let mut seen = false;
        let mut cursor = sp;
        while cursor < stop && !seen {
            if let Some(v) = word(cursor)
                && offset_of(v) == Some((image, off))
                && edge(v).is_some()
            {
                seen = true;
            }
            cursor += 8;
        }
        if seen {
            continue;
        }
        let filterable = edge(abs).is_some();
        if filterable {
            lost += 1;
        } else {
            rejected += 1;
        }
        out.str("\n       ");
        out.bytes(&names[image][..used[image]]);
        out.str("+");
        out.hex(off);
        out.str(if filterable {
            "  <- from A, passes B's filter, and B still lost it"
        } else {
            "  <- from A, and B's filter rejects it: not a call return, which is what A said too"
        });
        out.flush();
    }
    out.str("\n       of those, ");
    out.hex(rejected as u64);
    out.str(" are branch targets rather than call returns (A says so on each), and ");
    out.hex(lost as u64);
    out.str(" pass B's filter and were still missed.");
    out.flush();
    if lost == 0 {
        out.str("\n       B lost nothing, so it is a superset of A and the list is");
        out.str("\n       complete as far as this window reaches. Read B, and");
        out.str("\n       re-disassemble anything it names: `callers.py` scans raw");
        out.str("\n       bytes and has reported ModRM bytes as calls, and macho.py has");
        out.str("\n       39 mid-instruction starts in 258.");
    } else {
        out.str("\n       ");
        out.hex(lost as u64);
        out.str(" real frame(s) were lost, so the list above is NOT complete evidence.");
    }
    out.flush();
}

/// The `base/check.cc` frames a migrated `NOTREACHED_IN_MIGRATION()` unwinds
/// through, by image offset.
///
/// This build has **no symbol table** - `macho.py`'s `symbols` is empty and
/// there is no `LC_FUNCTION_STARTS` - so the offsets are derived from the image
/// and written down here in full, because a reader must be able to check them:
///
/// ```text
/// 5cb87d0:  push rbp; mov rbp,rsp; push r14; push rbx; sub rsp,0x30
/// 5cb87db:  lea  rsi,[rip+…]  # 0xd8eba55 "TriggerNotReached"      (its __func__)
/// 5cb87e2:  lea  rdx,[rip+…]  # 0xd8eba67 "../../base/check.cc"    (its __FILE__)
/// 5cb87ed:  mov  ecx,0x169                                       (its __LINE__ = 361)
/// 5cb87f2:  call 0x5ccd790                                       (Location ctor)
/// ```
///
/// That is `NotReachedError::TriggerNotReached()`: the two `__cstring` loads
/// are the only ones in the whole image that reach those strings, and
/// `0x169 == 361` is the number the guest printed. A site that *streams a real
/// location* would print its own file and line instead, so a report carrying
/// `check.cc(361)` is a report that came through here - which is what makes
/// this list the answer rather than one candidate among thousands.
///
/// The three below it are what `TriggerNotReached` itself unwinds through, all
/// in the same file and read off the same disassembly: it ends with
/// `call 0x5cb8890`, which calls `0x5cb87c0`, which calls `0x5cb8770`
/// (`CheckError::~CheckError`: virtual-dispatch the `LogMessage`, then
/// `int3; ud2`). The abort happens inside the virtual's callee, in
/// `LogMessage::Flush`, which is why the frames stop there.
const NOT_REACHED_CHAIN: &[(u64, &str)] = &[
    (
        0x5cb87d0,
        "NotReachedError::TriggerNotReached()  <- the site that fired",
    ),
    (0x5cb8890, "NotReachedError::~NotReachedError()"),
    (0x5cb87c0, "the out-of-line ~CheckError wrapper"),
    (0x5cb8770, "CheckError::~CheckError()"),
];

///
/// Additive and off by default. `ROSETTA3_TC_LAYOUT=1`.
///
/// Why this exists, and why it is not decoration. Three separate claims about
/// the object at `Framework+0x5d46240` have been made from single fields, and
/// two of them are wrong in ways this prints directly:
///
/// - `0x5cd72b0` builds the receiver as its second argument **minus `0x10`**
///   (`add r14,0xffffffffffffff10` at `0x5cd72da`), while its only live caller
///   passes `&this + 0xf0` (`lea rsi,[rbx+0xf0]` at `0x5d46f77`). The two are
///   *not* inverses: the receiver is `caller_this + 0xe0`. So the object `r14`
///   points at and the object `0x5d46e60` is running on are 0xe0 apart, and
///   every field name in the main report is being read at one of two bases.
/// - The constructor writes **three** more vtable pointers than the report
///   prints, at `+0xf8` and `+0x158` (and the destructor writes the same two
///   values again), so the four of them together are a positive fingerprint of
///   "this address was a `ThreadControllerImpl`, constructed".
/// - The destructor clears `+0x268` (`0x5d45078`) as well as `+0x218`
///   (`0x5d4514e`). Those two clears were both executed or neither, so `+0x268`
///   is a *second, independent* test of the same hypothesis that `+0x218` is
///   being used for, and it is not being used for it.
///
/// The last point is the reason to run this: `+0x218` alone cannot distinguish
/// "destructor ran" from "block was reused", and `+0x268` is a free second
/// witness.
fn say_tc_layout(out: &mut Buffer, r14: u64) {
    /// `ROSETTA3_TC_LAYOUT=1` - off by default, and the only thing this reads.
    const ENV: &str = "ROSETTA3_TC_LAYOUT";
    /// How far below the controller to print. The caller of the DoWork closure
    /// computes its own `this` as `r14 - 0xe0` and then reads a member at
    /// `+0x268` from *that*, i.e. `r14 + 0x188`, so the region below is not
    /// slack - it is where the object that holds the controller lives.
    const BELOW: u64 = 0x100;
    /// The controller is a `new(0x270)`, and `DoWork` reads `+0x260`, so this
    /// is the whole allocation plus a little.
    const ABOVE: u64 = 0x280;

    if !std::env::var_os(ENV).is_some_and(|v| v != "0") {
        return;
    }
    let word = |at: u64| -> Option<u64> {
        if at == 0 || at % 8 != 0 {
            return None;
        }
        // SAFETY: a guest word, read only after the null and alignment check. A
        // wild address means this one line does not print, which is the same
        // failure mode as every other probe here.
        Some(unsafe { std::ptr::read_volatile(at as *const u64) })
    };
    let line = |out: &mut Buffer, label: &str, v: Option<u64>| {
        out.str("\n      ");
        out.str(label);
        out.str(" = ");
        match v {
            Some(v) => {
                out.hex(v);
                // Deslide so a vtable pointer can be compared with the
                // addresses the disassembly prints.
                if let Some(off) = image_offset(v) {
                    out.str("  guest+");
                    out.hex(off);
                }
            }
            None => out.str("unreadable"),
        }
    };

    out.str("\n  TC_LAYOUT - the four vptrs the ctor and dtor both write:");
    for (name, at) in [
        ("r14+0x000 vptr   (0x5d44ebe / 0x5d45044)", 0x000u64),
        ("r14+0x0f0 vptr   (0x5d44ec8 / 0x5d4504e)", 0x0f0),
        ("r14+0x0f8 vptr   (0x5d44ed6 / 0x5d45055)", 0x0f8),
        ("r14+0x158 vptr   (0x5d44ee4 / 0x5d4506a)", 0x158),
    ] {
        line(out, name, word(r14.wrapping_add(at)));
    }
    out.str("\n    expected guest offsets 0xe268490 / 0xe268598 / 0xe2685f0 / 0xe268628");
    out.flush();

    out.str("\n  TC_LAYOUT - the destructor's clears, and what it released:");
    out.flush();
    for (name, at) in [
        ("r14+0x218  cleared at 0x5d4514e", 0x218u64),
        ("r14+0x268  cleared at 0x5d45078", 0x268),
        ("r14+0x1a0  pthread_mutex_destroy at 0x5d4519a", 0x1a0),
        ("r14+0x1e0  released at 0x5d4517b", 0x1e0),
        ("r14+0x1f0  released at 0x5d45165", 0x1f0),
        ("r14+0x200  destroyed at 0x5d45160", 0x200),
        ("r14+0x220  vector begin (ctor 0x5d44fcb)", 0x220),
        ("r14+0x228  vector end", 0x228),
        ("r14+0x238  list head (bind 0x5d45529)", 0x238),
        ("r14+0x150  bound flag  (bind 0x5d454f1)", 0x150),
    ] {
        line(out, name, word(r14.wrapping_add(at)));
        out.flush();
    }
    out.str("\n    -> +0x218 and +0x268 are cleared by the same destructor,");
    out.str("\n       0x4f2 bytes apart with no branch between them, so they");
    out.str("\n       were both written or neither was. Two witnesses, not one.");
    out.flush();

    out.str("\n  TC_LAYOUT - the object 0xe0 below the controller, word by word:");
    out.str("\n    (0x5d46e60 runs on r14-0xe0 and reads its +0x268, +0xf0, +0x218)");
    out.flush();
    let mut below = BELOW;
    while below >= 8 {
        below -= 8;
        line(out, &format!("r14-{below:#05x}"), word(r14.wrapping_sub(below)));
    }
    out.flush();
    out.str("\n  TC_LAYOUT - past the end of the 0x270 allocation:");
    out.flush();
    for step in (0x270u64..=ABOVE).step_by(8) {
        line(out, &format!("r14+{step:#05x}"), word(r14.wrapping_add(step)));
    }
    out.flush();
}

/// Why is the `+0x218` cell null?  It is produced by `0x5cd7420`, and that
/// function is a **thread-local cell accessor**: it reads `gs:[...]`, checks a
/// generation word, and has a `xor eax,eax` exit that returns null when either
/// check fails. The bind at `0x5d45440` stores that return value unguarded and
/// four instructions later sets `+0x150 = 1`.
///
/// So this is the whole question, and it is a *measurement* of the faulting
/// thread's `%gs` rather than an argument about it. `rdfsgs` is a single
/// instruction, and the handler runs on the faulting thread, so the base read
/// here is that thread's.
///
/// Additive, and off unless `ROSETTA3_TLS_CELL=1`.
fn say_tls_cell(out: &mut Buffer) {
    /// `ROSETTA3_TLS_CELL=1` - off by default, and the only thing this reads.
    const ENV: &str = "ROSETTA3_TLS_CELL";
    /// The three guest globals `0x5cd7420` reads. The offset of the thread_local
    /// block, the slot index, and the generation word - all at image offsets
    /// taken from the disassembly, not guessed.
    const TLS_OFFSET_PTR: u64 = 0xe6a9db0;
    const SLOT_INDEX: u64 = 0xe8729c0;
    const GENERATION: u64 = 0xe8729c8;
    /// The once-flag `0x5cd7420` tests before any of this.
    const ONCE_FLAG: u64 = 0xe8729d0;

    if !std::env::var_os(ENV).is_some_and(|v| v != "0") {
        return;
    }
    // A guest *file* offset, as the disassembly prints it, turned into the host
    // address it was loaded at. The three globals below are named by their
    // unrelocated offsets because that is what the disassembly says, and
    // reading them unrelocated faults - which is what the first run of this
    // probe did, and reported as "unreadable" rather than as "wrong address".
    let host_of = |guest_off: u64| -> Option<u64> {
        IMAGES.try_lock().ok().and_then(|images| {
            images
                .iter()
                .find(|i| guest_off < i.range.1 - i.range.0)
                .map(|i| i.range.0 + guest_off)
        })
    };
    // SAFETY: a null/alignment-checked word, as everywhere else here.
    let word = |at: u64| -> Option<u64> {
        (at != 0 && at % 8 == 0)
            .then(|| unsafe { std::ptr::read_volatile(at as *const u64) })
    };
    // The guest global, named by its file offset and read where it was loaded.
    let global = |guest_off: u64| -> Option<u64> { word(host_of(guest_off)?) };
    // The faulting thread's `%gs` base, as the runtime installed it. This is a
    // plain memory read of a box the thread already owns, not a `gs:` load:
    // the handler must not itself fault, and it must not assume `%gs` is
    // installed at all on the thread that faulted.
    let gs: u64 = crate::thread::tsd_base();
    out.str("\n  TLS_CELL - the faulting thread's %gs and the three globals");
    out.str("\n    0x5cd7420 reads gs:[%rip+0xe6a9db0], so:");
    out.str("\n    %gs base = ");
    out.hex(gs);
    out.str("  (0 = unset, which is the whole answer if true)");
    out.flush();
    for (name, at) in [
        ("[0xe6a9db0] tls block offset", TLS_OFFSET_PTR),
        ("[0xe8729c0] slot index", SLOT_INDEX),
        ("[0xe8729c8] generation", GENERATION),
        ("[0xe8729d0] once flag (byte)", ONCE_FLAG),
    ] {
        out.str("\n    ");
        out.str(name);
        out.str(" = ");
        match global(at) {
            Some(v) => {
                out.hex(v);
                if let Some(off) = image_offset(v) {
                    out.str("  guest+");
                    out.hex(off);
                }
            }
            None => out.str("unreadable"),
        }
    }
    out.flush();
    // Reproduce the guest's own three steps, in its own order, reading only
    // memory this thread already owns. The guest does:
    //
    //   mov rax,[rip+0xe6a9db0]      ; a word index
    //   mov rax,gs:[rax*8]           ; THE THREAD POINTER
    //   and rax,~3 ; je -> allocate
    //
    // so `%gs` is indexed by that word, and what comes back is the pointer the
    // rest of the function treats as the per-thread block. `tsd_base()` is that
    // same `%gs` value.
    match global(TLS_OFFSET_PTR) {
        Some(idx) => {
            out.str("\n    the word index at 0xe6a9db0 = ");
            out.hex(idx);
            out.str("\n    so the guest loads gs:[index*8] = tsd_base + index*8");
            out.str("\n    = ");
            let at = gs.wrapping_add(idx.wrapping_mul(8));
            out.hex(at);
            out.flush();
            match word(at) {
                Some(tp) => {
                    out.str("\n    the THREAD POINTER it reads there = ");
                    out.hex(tp);
                    out.str("  (0 = the `and rax,~3; je` fires and it allocates)");
                    out.flush();
                    // Now the generation check, exactly as at 0x5cd7518-0x5cd752d.
                    let index = global(SLOT_INDEX).unwrap_or(0);
                    let gen_at = tp.wrapping_add(index.wrapping_mul(16)).wrapping_add(8);
                    let gen_here = word(gen_at);
                    let gen_want = global(GENERATION);
                    out.str("\n    slot index [0xe8729c0] = ");
                    out.hex(index);
                    out.str(", shifted <<4 = ");
                    out.hex(index.wrapping_mul(16));
                    out.str("\n    generation at [tp+rcx+8] = ");
                    match gen_here {
                        Some(v) => out.hex(v),
                        None => out.str("unreadable"),
                    }
                    out.str("\n    generation wanted [0xe8729c8] = ");
                    match gen_want {
                        Some(v) => out.hex(v),
                        None => out.str("unreadable"),
                    }
                    match (gen_here, gen_want) {
                        (Some(a), Some(b)) if a == b => out.str(
                            "\n    -> they MATCH, so 0x5cd7420 returned the cell, not null",
                        ),
                        _ => out.str(
                            "\n    -> they DIFFER, so 0x5cd752d took `jne 0x5cd7537`,",
                        ),
                    }
                    out.flush();
                    out.str("\n    and the cell it would return is [tp+rcx] = ");
                    let cell = word(tp.wrapping_add(index.wrapping_mul(16)));
                    match cell {
                        Some(v) => out.hex(v),
                        None => out.str("unreadable"),
                    }
                    out.flush();
                }
                None => out.str(
                    "\n    UNREADABLE, so the thread-pointer load itself is the fault",
                ),
            }
        }
        None => out.str("\n    the index word is unreadable, so this says nothing"),
    }
    out.str("\n    a healthy thread has a non-zero thread pointer, a generation");
    out.str("\n    that matches, and a non-null cell. Any one of those three");
    out.str("\n    failing is enough to make the bind store a null into +0x218.");
    out.flush();
}

/// Name the unbound import a jump to zero went through.
///
/// See [`note_unbound`] for why this has to be reconstructed rather than read.
/// The call is `call rel32` - five bytes, the last four a signed displacement
/// from the instruction's end, which is the return address itself. Anything
/// else (an indirect call, a tail jump) simply does not match a stub and says
/// nothing, which is the same as the report said before.
fn say_unbound_call(out: &mut Buffer, context: *mut libc::c_void) {
    let Some((_, sp, lr)) = host_frame(context) else {
        return;
    };
    // Where the call came from. AAPCS64 leaves it in the link register; x86-64
    // pushed it, and a `pc` of zero means the push is the last thing that
    // happened, so the word at `sp` is it.
    let ret = match lr {
        Some(lr) => lr,
        None => {
            if sp == 0 || sp % 8 != 0 {
                return;
            }
            // SAFETY: `sp` is the kernel's own value for this thread's stack
            // pointer, and the word at it is the return address the call pushed.
            unsafe { std::ptr::read(sp as *const u64) }
        }
    };
    // The address itself, always. A jump to zero from a host library or from
    // this runtime names no import, and the return address is then the only
    // thing that says where to look.
    out.str("\n  called from ");
    out.hex(ret);
    let _ = say_image(out, ret);

    // Only inside a guest image, which is both what makes the read below safe -
    // the loader mapped it - and what makes the call a plausible reading.
    if !in_image(ret) || ret < 4 {
        return;
    }
    let Some(stub) = call_target(ret) else {
        return;
    };

    let Ok(unbound) = UNBOUND.try_lock() else {
        return;
    };
    let Ok(index) = unbound.binary_search_by_key(&stub, |u| u.stub) else {
        return;
    };
    out.str("\n  called ");
    out.bytes(&unbound[index].name[..unbound[index].used]);
    out.str(", an import nothing satisfied - its stub ");
    out.hex(stub);
    out.str(" jumps through a slot left zero");
}

/// The address the call just before `ret` was made to.
///
/// `ret` is inside a mapped guest image, so the instruction ahead of it is
/// readable. The encoding is the host's own, because a native image is host code
/// and an emulated one reaches its imports through a translation rather than
/// through this path.
#[cfg(target_arch = "aarch64")]
fn call_target(ret: u64) -> Option<u64> {
    // SAFETY: `ret - 4 .. ret` is inside a mapped, readable guest image, and an
    // instruction is four aligned bytes.
    let instruction = unsafe { std::ptr::read_unaligned((ret - 4) as *const u32) };
    // `bl imm26`, whose displacement is in words and signed.
    if instruction & 0xfc00_0000 != 0x9400_0000 {
        return None;
    }
    let words = ((instruction << 6) as i32 >> 6) as i64;
    Some((ret - 4).wrapping_add((words * 4) as u64))
}

#[cfg(not(target_arch = "aarch64"))]
fn call_target(ret: u64) -> Option<u64> {
    // SAFETY: `ret - 4 .. ret` is inside a mapped, readable guest image, just
    // checked by the caller.
    let displacement = unsafe { std::ptr::read_unaligned((ret - 4) as *const i32) };
    Some(ret.wrapping_add(displacement as i64 as u64))
}

/// Whether `addr` is inside an image the loader mapped.
fn in_image(addr: u64) -> bool {
    IMAGES.try_lock().is_ok_and(|images| {
        images
            .iter()
            .any(|i| (i.range.0..i.range.1).contains(&addr))
    })
}

/// A frame pointer worth dereferencing: on this thread's stack, aligned, and
/// above the one that named it.
fn readable(fp: u64, sp: u64) -> Option<u64> {
    /// Widest stack this walk will believe, which is the main thread's.
    const SPAN: u64 = 8 << 20;
    (fp >= sp && fp < sp.saturating_add(SPAN) && fp % 8 == 0).then_some(fp)
}

/// The saved frame pointer and return address a frame holds, in the layout both
/// hosts use: the caller's `fp` at `[fp]` and its return address at `[fp + 8]`.
fn frame(fp: u64) -> Option<(u64, u64)> {
    // SAFETY: `readable` established that `fp` is an aligned address inside the
    // faulting thread's stack, so the two words below are mapped.
    let (next, ret) = unsafe {
        (
            std::ptr::read((fp as *const u64).offset(0)),
            std::ptr::read((fp as *const u64).offset(1)),
        )
    };
    (next > fp && ret != 0).then_some((next, ret))
}

/// The frame pointer, stack pointer and return address the fault interrupted.
#[cfg(target_os = "macos")]
fn host_frame(context: *mut libc::c_void) -> Option<(u64, u64, Option<u64>)> {
    /// `__ss.__fp`, `__ss.__lr` and `__ss.__sp`, after `__es` and the twenty-nine
    /// `__x` registers - see [`host_pc`] for the same measurement.
    #[cfg(target_arch = "aarch64")]
    const MC: (usize, usize, usize) = (16 + 232, 16 + 240, 16 + 248);
    /// `__ss.__rbp` and `__ss.__rsp`, the seventh and eighth of the sixteen
    /// general-purpose registers. x86-64 has no link register.
    #[cfg(target_arch = "x86_64")]
    const MC: (usize, usize, usize) = (16 + 48, 16 + 48, 16 + 56);

    if context.is_null() {
        return None;
    }
    // SAFETY: the same live `ucontext_t` [`host_pc`] reads, at measured offsets
    // of pointer-sized fields the kernel filled in.
    unsafe {
        let mcontext = std::ptr::read((context as *const u8).add(UC_MCONTEXT) as *const *const u8);
        if mcontext.is_null() {
            return None;
        }
        let at = |offset: usize| std::ptr::read(mcontext.add(offset) as *const u64);
        let lr = cfg!(target_arch = "aarch64").then(|| at(MC.1));
        Some((at(MC.0), at(MC.2), lr))
    }
}

/// The frame pointer, stack pointer and return address the fault interrupted.
///
/// Linux embeds its `mcontext_t` in the `ucontext_t` rather than pointing at one,
/// and the `libc` crate declares both - so this reads fields rather than offsets.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn host_frame(context: *mut libc::c_void) -> Option<(u64, u64, Option<u64>)> {
    if context.is_null() {
        return None;
    }
    // SAFETY: the kernel passes a live `ucontext_t` for the duration of the
    // handler, and `gregs` is the general-purpose register array it filled in.
    let gregs = unsafe { &(*(context as *const libc::ucontext_t)).uc_mcontext.gregs };
    let at = |reg: libc::c_int| gregs[reg as usize] as u64;
    // x86-64 has no link register.
    Some((at(libc::REG_RBP), at(libc::REG_RSP), None))
}

/// As above. FreeBSD embeds its `mcontext_t` too, and names each register
/// rather than indexing an array.
#[cfg(all(target_os = "freebsd", target_arch = "x86_64"))]
fn host_frame(context: *mut libc::c_void) -> Option<(u64, u64, Option<u64>)> {
    if context.is_null() {
        return None;
    }
    // SAFETY: the kernel passes a live `ucontext_t` for the duration of the
    // handler.
    let mc = unsafe { &(*(context as *const libc::ucontext_t)).uc_mcontext };
    // x86-64 has no link register.
    Some((mc.mc_rbp as u64, mc.mc_rsp as u64, None))
}

/// The frame pointer, stack pointer and return address the fault interrupted.
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn host_frame(context: *mut libc::c_void) -> Option<(u64, u64, Option<u64>)> {
    if context.is_null() {
        return None;
    }
    // SAFETY: as above; `regs[29]` is the frame pointer and `regs[30]` the link
    // register, in the layout AAPCS64 and the kernel agree on.
    let mc = unsafe { &(*(context as *const libc::ucontext_t)).uc_mcontext };
    Some((mc.regs[29], mc.sp, Some(mc.regs[30])))
}

/// As above, out of FreeBSD's `mc_gpregs`, which keeps the link register and
/// the stack pointer beside the thirty general-purpose ones.
#[cfg(all(target_os = "freebsd", target_arch = "aarch64"))]
fn host_frame(context: *mut libc::c_void) -> Option<(u64, u64, Option<u64>)> {
    if context.is_null() {
        return None;
    }
    let gp = unsafe {
        &(*(context as *const libc::ucontext_t))
            .uc_mcontext
            .mc_gpregs
    };
    Some((gp.gp_x[29] as u64, gp.gp_sp as u64, Some(gp.gp_lr as u64)))
}

/// Name the translation the faulting `pc` fell in, and the guest instruction
/// inside it when microlift's source locations reach that far.
fn say_region(out: &mut Buffer, pc: u64) {
    let Ok(placed) = PLACED.try_lock() else {
        out.str("\n  guest region: unavailable (a thread holds the map)");
        return;
    };
    let Some(entry) = covering(&placed, pc) else {
        // The native backend has no translations at all, so "no translation" is
        // not the same statement there that it is under emulation: the image is
        // host code the loader mapped, and the guest's own frames are exactly
        // the ones worth naming.
        out.str("\n  in");
        if !say_image(out, pc) {
            out.str(
                " no guest image - the pc is in no translation either, so this fault is in this \
                 runtime or in a host library",
            );
            say_host_mapping(out, pc);
        }
        return;
    };

    out.str("\n  guest region ");
    out.hex(entry.guest.0);
    out.str("..");
    out.hex(entry.guest.1);
    let _ = say_image(out, entry.guest.0);
    out.str(", entered at ");
    out.hex(entry.lifted_for);

    // The exact instruction, when the map has it. This is the whole point of
    // carrying microlift's source locations. Relative to `map_base`, which is what
    // the offsets were recorded against - not to `code`, which may be the entry
    // stub below it.
    let offset = pc.wrapping_sub(entry.map_base);
    if let Ok(offset) = u32::try_from(offset) {
        if let Some(va) = lookup(&entry.map, offset) {
            out.str("\n  guest instruction ");
            out.hex(va);
            let _ = say_image(out, va);
            return;
        }
    }

    out.str("\n  host offset +");
    out.hex(offset);
    out.str(" into its compiled code");
    // A `pc` outside everything the map attributed is not necessarily inside
    // this translation at all.
    if entry.map.is_empty() {
        out.str(" - no source map was kept for this region (map budget spent)");
    } else if pc >= entry.attributed_end() {
        out.str(
            " - past every range this translation attributed to a guest \
                 instruction, so the fault is probably not in it",
        );
    } else {
        out.str(
            " - inside it, but in code with no guest origin: a prologue, \
                 an epilogue, or the entry dispatch",
        );
    }
}

/// The guest instruction covering a host offset, over microlift's own sorted,
/// non-overlapping ranges.
fn lookup(map: &[GuestCodeRange], offset: u32) -> Option<u64> {
    let i = map.partition_point(|r| r.start <= offset);
    let range = map.get(i.checked_sub(1)?)?;
    range.contains(offset).then_some(range.guest_va)
}

/// Name the image a guest address is in, as `name+offset`. Whether it named one.
/// The offset of `addr` inside the image that mapped it, which is the number
/// the report prints and the number every address in this file's notes is
/// written in. A raw `pc` is slid by the loader and means nothing on its own.
fn image_offset(addr: u64) -> Option<u64> {
    IMAGES.try_lock().ok().and_then(|images| {
        images
            .iter()
            .find(|i| (i.range.0..i.range.1).contains(&addr))
            .map(|i| addr - i.range.0)
    })
}

fn say_image(out: &mut Buffer, addr: u64) -> bool {
    let Ok(images) = IMAGES.try_lock() else {
        return false;
    };
    let Some(image) = images
        .iter()
        .find(|i| (i.range.0..i.range.1).contains(&addr))
    else {
        return false;
    };
    out.str(" (");
    out.bytes(&image.name[..image.used]);
    out.str("+");
    out.hex(addr - image.range.0);
    out.str(")");
    true
}

/// What this runtime knows about the faulting thread.
///
/// **The "not a guest thread" line below is a statement about
/// [`crate::thread::current`] and nothing else**, and on the **native** backend it
/// is `None` for *every* thread including the main one, because nothing there ever
/// calls [`crate::thread::install`] - `exec::native::Native::load` binds every
/// import straight to a host address and disarms the trampolines, so
/// `crate::shims::dispatch` is never reached and a guest `pthread_create` lands in
/// `darwin::pthread::create`, whose `run` gives the thread a Darwin thread pointer
/// and no guest thread state. It is printed for every fault in every native run, so
/// on that backend it distinguishes nothing. `ROSETTA3_THREAD_WHO=1` adds the two
/// facts that make it checkable - see [`note_thread_who`].
fn say_thread(out: &mut Buffer) {
    out.str("\n  thread ");
    out.hex(u64::from(crate::thread::key()));
    let Some(state) = crate::thread::current() else {
        note_thread_who(false);
        // **This clause used to claim the fault was in this runtime or a host
        // library, and on the native backend that is false every time.** The
        // thread registry is populated by `install`, whose only live caller is
        // `abi::call_import_inner` — reached from the `Vm` run loop, which the
        // native backend never enters because it disarms the trampolines and
        // calls guest code directly. So on native `current()` is `None` for
        // *every* thread, guest or not, and 66 of 66 recorded reports said
        // "not a guest thread" and none ever printed `guest rsp`.
        //
        // It cost four sessions of chasing a ghost thread. The read itself is
        // honest — one writer of that slot, `install` — it simply says nothing
        // on this backend, so the report must not imply it does.
        if crate::exec::active().is_some_and(|e| e.engine() == crate::exec::Engine::Native) {
            out.str(" - the native backend keeps no guest thread registry, so this says");
            out.str(" nothing about the thread; the fault is in guest code if the");
            out.str(" pc above is in a guest image");
        } else {
            out.str(" - not a guest thread; the fault is in this runtime or in a host library");
        }
        return;
    };
    note_thread_who(true);
    out.str(", guest rsp ");
    out.hex(state.live_rsp());
    out.str(", entry ");
    out.hex(state.entry());
    let stack = state.stack_range();
    out.str(", stack ");
    out.hex(stack.start);
    out.str("..");
    out.hex(stack.end);
    if !stack.contains(&state.live_rsp()) {
        out.str(" - rsp is outside it");
    }
}

/// Put the disposition that was in place before [`install`] back, so the second
/// fault is handled the way the first would have been.
fn restore(signal: i32) {
    let saved = saved_for(signal);
    // SAFETY: `action` is a live local, rebuilt from the three fields `install`
    // read out of the disposition it displaced. Installing it is the inverse of
    // what `install` did.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = saved.handler.load(Ordering::SeqCst);
        saved.mask.get().apply(&mut action);
        action.sa_flags = saved.flags.load(Ordering::SeqCst);
        libc::sigaction(signal, &action, std::ptr::null_mut());
    }
}

/// The faulting `pc`, out of the `ucontext_t` the kernel passed.
///
/// `None` on any null, because a handler that dereferences its way to a second fault
/// has destroyed the evidence it was called to preserve.
fn host_pc(context: *mut libc::c_void) -> Option<u64> {
    // SAFETY: `context` is the third argument of this `SA_SIGINFO` handler, which
    // is exactly what `interrupted_pc` asks for. It knows both hosts' layouts -
    // Darwin's out-of-line thread state and Linux's inline `mcontext_t`.
    unsafe { darwin::signal::interrupted_pc(context) }
}

/// The address's own bytes, when every one of them is printable.
///
/// Little-endian, so the low byte is the first character - which is the order
/// the text was written in memory.
fn ascii_word(addr: u64) -> Option<[u8; 8]> {
    let bytes = addr.to_le_bytes();
    bytes
        .iter()
        .all(|b| (0x20..0x7f).contains(b))
        .then_some(bytes)
}

/// Whether the address was unmapped or merely not writable.
///
/// The difference is the whole diagnosis. `SEGV_MAPERR` on a plausible-looking
/// address is a corrupted pointer; `SEGV_ACCERR` is a *write* to memory that is
/// mapped and read-only, which is a different bug entirely - a guest writing
/// through a pointer into its own `__TEXT`, or a segment this loader mapped
/// with the wrong protection.
#[cfg(target_os = "linux")]
fn say_access(out: &mut Buffer, info: *mut libc::siginfo_t) {
    // SAFETY: the kernel passes a live `siginfo_t` for the duration of the
    // handler.
    // `SEGV_MAPERR` and `SEGV_ACCERR`, which the `libc` crate does not declare.
    // Both are `si_code` values for `SIGSEGV` alone, and they are 1 and 2 on
    // every Linux architecture.
    const MAPERR: i32 = 1;
    const ACCERR: i32 = 2;

    let Some(code) = unsafe { info.as_ref() }.map(|i| i.si_code) else {
        return;
    };
    out.str(match code {
        MAPERR => " (nothing is mapped there)",
        ACCERR => " (mapped, but the access is not permitted - a write to read-only memory)",
        // What a non-canonical address gives on x86-64: the CPU raises a general
        // protection fault rather than a page fault, and the kernel has no
        // faulting address to report - so `si_addr` reads zero however wild the
        // pointer was. Worth naming, because "SIGSEGV at 0x0" otherwise sends a
        // reader looking for a null dereference that never happened.
        libc::SI_KERNEL => " (a non-canonical address; the reported address is not the pointer)",
        _ => return,
    });
}

#[cfg(not(target_os = "linux"))]
fn say_access(_out: &mut Buffer, _info: *mut libc::siginfo_t) {}

/// The integer registers the fault stopped on.
///
/// Printed because the two questions a fault in guest code always raises -
/// *which* pointer was null, and which of the values around it is the corrupt
/// one - are answered by the registers and by nothing else in this report. A
/// disassembly of the faulting instruction names a register; without this there
/// is nothing to look its value up in, and gdb cannot be used to find out (it
/// is far too slow to reach the fault in a program of this size).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn say_registers(out: &mut Buffer, context: *mut libc::c_void) {
    if context.is_null() {
        return;
    }
    // The order `libc::REG_*` indexes them in, with the names a disassembly uses.
    const NAMED: &[(&str, usize)] = &[
        ("rax", libc::REG_RAX as usize),
        ("rbx", libc::REG_RBX as usize),
        ("rcx", libc::REG_RCX as usize),
        ("rdx", libc::REG_RDX as usize),
        ("rsi", libc::REG_RSI as usize),
        ("rdi", libc::REG_RDI as usize),
        ("rbp", libc::REG_RBP as usize),
        ("rsp", libc::REG_RSP as usize),
        ("r8", libc::REG_R8 as usize),
        ("r9", libc::REG_R9 as usize),
        ("r10", libc::REG_R10 as usize),
        ("r11", libc::REG_R11 as usize),
        ("r12", libc::REG_R12 as usize),
        ("r13", libc::REG_R13 as usize),
        ("r14", libc::REG_R14 as usize),
        ("r15", libc::REG_R15 as usize),
    ];
    // SAFETY: the kernel passes a live `ucontext_t` for the duration of an
    // `SA_SIGINFO` handler, and `gregs` is the register array inside it.
    let gregs = unsafe { &(*(context as *const libc::ucontext_t)).uc_mcontext.gregs };
    for (i, (name, index)) in NAMED.iter().enumerate() {
        out.str(match i % 4 {
            0 => "\n  ",
            _ => "  ",
        });
        out.str(name);
        out.str("=");
        out.hex(gregs[*index] as u64);
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn say_registers(_out: &mut Buffer, _context: *mut libc::c_void) {}

/// Name the host mapping a `pc` outside any translation fell in.
///
/// Without this the report says "this fault is in host code" and stops, which
/// leaves an address and nothing to do with it. The native backend has no
/// translations at all, so that is *every* fault it reports.
///
/// `/proc/self/maps` rather than `dladdr`: `dladdr` takes the loader's lock,
/// and a handler that blocks on a lock the faulting thread already holds hangs
/// instead of printing. Reading a file with `open`/`read`/`close` is
/// async-signal-safe, which is the same rule [`Buffer`] exists for.
///
/// Silent on a host without `/proc`. macOS has `dyld_image_count` and a
/// different answer; nothing here needs it yet.
#[cfg(target_os = "linux")]
fn say_host_mapping(out: &mut Buffer, pc: u64) {
    // SAFETY: a constant path and constant flags.
    let fd = unsafe {
        libc::open(
            c"/proc/self/maps".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return;
    }
    // Small on purpose. This is a frame on the *signal* stack, and the one a
    // thread has is not always this runtime's 512 KiB: a guest with a crash
    // reporter installs its own - Breakpad's is `MINSIGSTKSZ` - and every
    // `SA_ONSTACK` handler then runs in it. An eight-kilobyte buffer here is
    // what overflowed such a stack, and the fault that followed was reported as
    // the handler crashing while naming the pc's region, with the real crash
    // lost. A `/proc/self/maps` line is well under this, and a line longer than
    // the whole buffer is already handled below.
    let mut buf = [0u8; 512];
    let mut held = 0usize;
    'reading: loop {
        // SAFETY: writing into the unused tail of a live local array.
        let read = unsafe { libc::read(fd, buf[held..].as_mut_ptr().cast(), buf.len() - held) };
        if read <= 0 {
            break;
        }
        let filled = held + read as usize;
        let mut start = 0;
        while let Some(end) = buf[start..filled].iter().position(|b| *b == b'\n') {
            if say_if_containing(out, &buf[start..start + end], pc) {
                break 'reading;
            }
            start += end + 1;
        }
        held = filled - start;
        // A line longer than the whole buffer is not one this can parse, and
        // stalling on it would leave the report without a thread line.
        if held == buf.len() {
            held = 0;
        } else {
            buf.copy_within(start..filled, 0);
        }
    }
    // SAFETY: the descriptor opened above, which nothing else holds.
    unsafe { libc::close(fd) };
}

#[cfg(not(target_os = "linux"))]
fn say_host_mapping(_out: &mut Buffer, _pc: u64) {}

/// One `/proc/self/maps` line: `lo-hi perms offset dev inode  path`.
///
/// Answers whether it was the one, so the caller can stop reading.
#[cfg(target_os = "linux")]
fn say_if_containing(out: &mut Buffer, line: &[u8], pc: u64) -> bool {
    let (low, rest) = hex_prefix(line);
    let Some(rest) = rest.strip_prefix(b"-") else {
        return false;
    };
    let (high, rest) = hex_prefix(rest);
    if !(low..high).contains(&pc) {
        return false;
    }
    // One field - the permissions - then the file offset, which is what turns
    // this into something `objdump --start-address` can be pointed at. Exactly
    // one: `hex_prefix` above stopped on the space before `perms`, so skipping
    // twice reads the device major number instead and reports every mapping as
    // starting at offset zero.
    let (file_offset, _) = hex_prefix(skip_field(rest));

    out.str("\n  in ");
    match line.iter().position(|b| *b == b'/') {
        Some(at) => out.bytes(&line[at..]),
        // An anonymous mapping - a JIT page, a stack, a heap - which is worth
        // saying rather than leaving blank.
        None => out.str("an anonymous mapping"),
    }
    out.str(" + ");
    out.hex(pc - low);
    out.str(" (file offset ");
    out.hex(file_offset + (pc - low));
    out.str(")");
    true
}

/// The leading hexadecimal number and whatever follows it.
#[cfg(target_os = "linux")]
fn hex_prefix(bytes: &[u8]) -> (u64, &[u8]) {
    let mut value = 0u64;
    let mut n = 0;
    for byte in bytes {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => break,
        };
        value = (value << 4) | u64::from(digit);
        n += 1;
    }
    (value, &bytes[n..])
}

/// Past one space-separated field and the spaces around it.
#[cfg(target_os = "linux")]
fn skip_field(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|b| *b != b' ').unwrap_or(bytes.len());
    let rest = &bytes[start..];
    let end = rest.iter().position(|b| *b == b' ').unwrap_or(rest.len());
    let rest = &rest[end..];
    let start = rest.iter().position(|b| *b != b' ').unwrap_or(rest.len());
    &rest[start..]
}

/// A fixed message buffer, written out in one `write(2)`.
struct Buffer {
    bytes: [u8; 2048],
    used: usize,
}

impl Buffer {
    const fn empty() -> Self {
        Buffer {
            bytes: [0; 2048],
            used: 0,
        }
    }

    /// Append what fits. Truncation is silent because the alternative - a handler that
    /// reports its own overflow - is not more useful than a shorter sentence.
    fn bytes(&mut self, data: &[u8]) {
        let room = self.bytes.len() - self.used;
        let n = data.len().min(room);
        self.bytes[self.used..self.used + n].copy_from_slice(&data[..n]);
        self.used += n;
    }

    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }

    /// `0x` and the value, with no leading zeros. Hand-rolled for the reason
    /// the type exists.
    fn hex(&mut self, mut value: u64) {
        let mut digits = [0u8; 16];
        let mut n = 0;
        loop {
            digits[n] = b"0123456789abcdef"[(value & 0xf) as usize];
            value >>= 4;
            n += 1;
            if value == 0 {
                break;
            }
        }
        self.str("0x");
        for i in (0..n).rev() {
            self.bytes(&[digits[i]]);
        }
    }

    fn flush(&mut self) {
        // SAFETY: a `write` of `used` bytes from a buffer that has at least
        // that many. `write` is async-signal-safe, which is the whole reason
        // this type exists; a short write loses the tail of a diagnostic and is
        // not worth a retry loop inside a handler.
        unsafe {
            libc::write(2, self.bytes.as_ptr() as *const libc::c_void, self.used);
        }
        self.used = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(text: &str) -> ([u8; NAME], usize) {
        let mut name = [0u8; NAME];
        name[..text.len()].copy_from_slice(text.as_bytes());
        (name, text.len())
    }

    fn image(start: u64, end: u64, text: &str) -> Image {
        let (name, used) = named(text);
        Image {
            range: (start, end),
            name,
            used,
        }
    }

    fn stub(at: u64, text: &str) -> Unbound {
        let (name, used) = named(text);
        Unbound {
            stub: at,
            name,
            used,
        }
    }

    #[test]
    fn a_dlopened_image_joins_the_program_rather_than_replacing_it() {
        // The launcher, then the framework it dlopens - the shape of every
        // Chromium run. Both must stay nameable.
        let mut table = Vec::new();
        merge_images(
            &mut table,
            [image(0x1_0000_0000, 0x1_0003_0000, "Chromium")].into_iter(),
        );
        merge_images(
            &mut table,
            [image(
                0x7fe9_42e8_4000,
                0x7fe9_50f0_0000,
                "Chromium Framework",
            )]
            .into_iter(),
        );
        let names: Vec<&[u8]> = table.iter().map(|i| &i.name[..i.used]).collect();
        assert_eq!(names, [&b"Chromium"[..], &b"Chromium Framework"[..]]);

        // An image mapped over an old one's range supersedes it.
        merge_images(
            &mut table,
            [image(0x1_0001_0000, 0x1_0002_0000, "later")].into_iter(),
        );
        let names: Vec<&[u8]> = table.iter().map(|i| &i.name[..i.used]).collect();
        assert_eq!(names, [&b"later"[..], &b"Chromium Framework"[..]]);
    }

    #[test]
    fn every_links_unbound_stubs_stay_findable_and_sorted() {
        let mut table = Vec::new();
        merge_unbound(
            &mut table,
            [stub(0x1_0002_5a3e, "___cxa_atexit")].into_iter(),
        );
        merge_unbound(
            &mut table,
            [
                stub(0x7fe9_4fb2_441a, "_os_unfair_lock_trylock"),
                stub(0x7fe9_4fb2_4414, "_os_unfair_lock_unlock"),
            ]
            .into_iter(),
        );
        fn find(table: &[Unbound], at: u64) -> Option<&[u8]> {
            table
                .binary_search_by_key(&at, |u| u.stub)
                .ok()
                .map(|i| &table[i].name[..table[i].used])
        }
        assert_eq!(find(&table, 0x1_0002_5a3e), Some(&b"___cxa_atexit"[..]));
        assert_eq!(
            find(&table, 0x7fe9_4fb2_441a),
            Some(&b"_os_unfair_lock_trylock"[..])
        );
        assert_eq!(
            find(&table, 0x7fe9_4fb2_4414),
            Some(&b"_os_unfair_lock_unlock"[..])
        );

        // The same stub from a later link replaces, rather than duplicates.
        merge_unbound(&mut table, [stub(0x1_0002_5a3e, "_other")].into_iter());
        assert_eq!(table.len(), 3);
        assert_eq!(find(&table, 0x1_0002_5a3e), Some(&b"_other"[..]));
    }

    #[test]
    fn hex_has_no_leading_zeros_and_a_zero_is_a_zero() {
        let mut b = Buffer::empty();
        b.hex(0);
        b.str(" ");
        b.hex(0x70536573556e6143);
        assert_eq!(&b.bytes[..b.used], b"0x0 0x70536573556e6143");
    }

    #[test]
    fn an_ascii_address_is_recognised_and_an_ordinary_one_is_not() {
        // The word the crash faulted on. Recognising it is what turned that
        // report from "a bad pointer" into "a pointer read out of text".
        assert_eq!(ascii_word(0x70536573556e6143), Some(*b"CanUseSp"));
        assert_eq!(ascii_word(0x1383bf610), None);
        assert_eq!(ascii_word(0), None);
    }

    /// A map with one range per guest instruction, laid out back to back.
    fn map(guest_start: u64, instructions: u32) -> GuestCodeMap {
        GuestCodeMap::from_ranges(
            (0..instructions)
                .map(|i| GuestCodeRange {
                    start: i * 16,
                    end: (i + 1) * 16,
                    guest_va: guest_start + u64::from(i),
                })
                .collect(),
        )
    }

    #[test]
    fn a_host_pc_resolves_to_the_guest_instruction_it_came_from() {
        // The claim the whole module turns on.
        placed(
            0x40_0000,
            0x40_0000,
            0x1_0000,
            0x1_0010,
            0x1_0000,
            &map(0x1_0000, 16),
        );
        let placed = PLACED.lock().unwrap();
        let entry = placed.iter().find(|p| p.code == 0x40_0000).unwrap();

        assert_eq!(lookup(&entry.map, 0), Some(0x1_0000));
        assert_eq!(lookup(&entry.map, 15), Some(0x1_0000));
        assert_eq!(lookup(&entry.map, 16), Some(0x1_0001));
        assert_eq!(lookup(&entry.map, 0x57), Some(0x1_0005));
        // Past everything attributed: a prologue, an epilogue, or another
        // translation altogether.
        assert_eq!(lookup(&entry.map, 16 * 16), None);
    }

    #[test]
    fn the_nearest_translation_below_the_pc_is_the_one_reported() {
        placed(
            0x4000,
            0x4000,
            0x1_0000,
            0x1_0100,
            0x1_0000,
            &map(0x1_0000, 4),
        );
        placed(
            0x8000,
            0x8000,
            0x2_0000,
            0x2_0080,
            0x2_0040,
            &map(0x2_0000, 4),
        );
        let placed = PLACED.lock().unwrap();
        let find = |pc: u64| match placed.binary_search_by_key(&pc, |p| p.code) {
            Ok(i) => Some(placed[i].guest),
            Err(0) => None,
            Err(i) => Some(placed[i - 1].guest),
        };
        assert_eq!(find(0x3fff), None);
        assert_eq!(find(0x4000), Some((0x1_0000, 0x1_0100)));
        assert_eq!(find(0x4010), Some((0x1_0000, 0x1_0100)));
        assert_eq!(find(0x9000), Some((0x2_0000, 0x2_0080)));
    }

    #[test]
    fn a_pc_past_everything_a_translation_covers_is_not_claimed_for_it() {
        // The misattribution this replaced: `+0x57c` into a sixty-two-byte
        // region was reported as if it were inside it.
        placed(
            0x10_0000,
            0x10_0000,
            0x3_0000,
            0x3_0004,
            0x3_0000,
            &map(0x3_0000, 4),
        );
        let placed = PLACED.lock().unwrap();
        let entry = placed.iter().find(|p| p.code == 0x10_0000).unwrap();
        assert_eq!(entry.attributed_end(), 0x10_0040);
        assert_eq!(lookup(&entry.map, 0x3f), Some(0x3_0003));
        assert_eq!(lookup(&entry.map, 0x40), None);
    }

    /// The run loop enters through the thunk on every host dispatch, so a profile
    /// that cannot name it cannot see the busiest code in the process. Cranelift
    /// emits the thunk *above* the body, which is why it read as an epilogue.
    #[test]
    fn the_entry_stub_is_named_when_it_sits_above_the_body() {
        let (link_target, entry_point) = (0x50_0000, 0x50_0100);
        placed(
            entry_point,
            link_target,
            0x5_0000,
            0x5_0010,
            0x5_0000,
            &map(0x5_0000, 4),
        );
        assert_eq!(
            attribute(entry_point + 8),
            Attribution::Translated {
                region: 0x5_0000,
                why: Unnamed::Prologue,
            }
        );
        assert_eq!(
            attribute(link_target),
            Attribution::Instruction {
                guest_va: 0x5_0000,
                region: 0x5_0000,
            }
        );
    }

    /// And the other layout, which is what a region with no separate thunk has.
    #[test]
    fn the_entry_stub_below_the_link_target_belongs_to_its_region() {
        // Its own host address: `placed` keys on `min(entry_point, link_target)`, so
        // a pair sharing that minimum with another test overwrites the other's row.
        let (entry_point, link_target) = (0x51_0000, 0x51_0100);
        placed(
            entry_point,
            link_target,
            0x5_0000,
            0x5_0010,
            0x5_0000,
            &map(0x5_0000, 4),
        );
        // In the stub, below the address a link row names.
        assert_eq!(
            attribute(entry_point + 8),
            Attribution::Translated {
                region: 0x5_0000,
                why: Unnamed::Prologue,
            }
        );
        // And the map still reads against the body, not against the stub.
        assert_eq!(
            attribute(link_target),
            Attribution::Instruction {
                guest_va: 0x5_0000,
                region: 0x5_0000,
            }
        );
    }

    /// A translation whose map the budget discarded still owns its addresses.
    /// Reporting it as `Elsewhere` is what put 70% of a real profile in the
    /// unaccounted bucket.
    #[test]
    fn a_region_with_no_map_still_owns_its_code() {
        let code = 0x60_0000;
        placed(code, code, 0x6_0000, 0x6_0010, 0x6_0000, &map(0x6_0000, 0));
        assert_eq!(
            attribute(code + 0x40),
            Attribution::Translated {
                region: 0x6_0000,
                // Nothing was attributed, so this is the budget and not an epilogue.
                why: Unnamed::Budget,
            }
        );
    }

    /// The bound is the next blob, so an address between two translations belongs
    /// to the lower one and an address past the last is still refused.
    #[test]
    fn a_blob_ends_where_the_next_one_starts() {
        placed(
            0x70_0000,
            0x70_0000,
            0x7_0000,
            0x7_0010,
            0x7_0000,
            &map(0x7_0000, 1),
        );
        placed(
            0x70_1000,
            0x70_1000,
            0x7_1000,
            0x7_1010,
            0x7_1000,
            &map(0x7_1000, 1),
        );

        // Past its map but before its neighbour: still the first region's.
        assert_eq!(
            attribute(0x70_0800),
            Attribution::Translated {
                region: 0x7_0000,
                why: Unnamed::Beyond,
            }
        );
        // The neighbour's first byte is the neighbour's.
        assert!(matches!(
            attribute(0x70_1000),
            Attribution::Instruction {
                region: 0x7_1000,
                ..
            }
        ));
        // And nothing is claimed a megabyte past the last one.
        assert_eq!(attribute(0x70_1000 + MAX_BLOB), Attribution::Elsewhere);
    }

    #[test]
    fn a_buffer_truncates_rather_than_panicking() {
        let mut b = Buffer::empty();
        for _ in 0..500 {
            b.str("0123456789");
        }
        assert_eq!(b.used, b.bytes.len());
    }
}
