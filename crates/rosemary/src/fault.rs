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
        let mib = std::env::var("ROSEMARY_MAP_BUDGET")
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

/// Where the guest's images landed. Filled once, before any guest code runs.
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
pub fn note_unbound(entries: &[(&str, &machish::report::MissingSymbol)]) {
    let mut unbound = UNBOUND.lock().unwrap_or_else(|e| e.into_inner());
    for (_, m) in entries {
        let Some(stub) = m.stub_va else { continue };
        let bytes = m.symbol.as_bytes();
        let used = bytes.len().min(NAME);
        let mut name = [0u8; NAME];
        name[..used].copy_from_slice(&bytes[..used]);
        let entry = Unbound { stub, name, used };
        match unbound.binary_search_by_key(&stub, |u| u.stub) {
            Ok(i) => unbound[i] = entry,
            Err(i) => unbound.insert(i, entry),
        }
    }
}

/// Record the guest's image map, so a fault can name a guest address rather
/// than only state it.
///
/// Without this the report's guest range is a number that needs a second run
/// with `ROSEMARY_TRACE_VM` to interpret - and the whole reason this module
/// exists is that the interesting crash is the one that does not come back.
pub fn note_images(report: &machish::report::LinkReport) {
    let mut images = IMAGES.lock().unwrap_or_else(|e| e.into_inner());
    for image in &report.images {
        let end = image
            .segments
            .iter()
            .filter(|s| s.vmsize != 0)
            .map(|s| s.vmaddr.wrapping_add(s.vmsize))
            .max();
        let Some(end) = end else { continue };
        let leaf = image.name.rsplit('/').next().unwrap_or(&image.name);
        let bytes = leaf.as_bytes();
        let used = bytes.len().min(NAME);
        let mut name = [0u8; NAME];
        name[..used].copy_from_slice(&bytes[..used]);
        let entry = Image {
            range: (image.base, end),
            name,
            used,
        };
        match images.binary_search_by_key(&image.base, |i| i.range.0) {
            Ok(i) => images[i] = entry,
            Err(i) => images.insert(i, entry),
        }
    }
}

/// Drop what [`note_images`] and [`note_unbound`] recorded for an unloaded
/// image, so a later mapping at the same address is not named after it.
pub fn forget_images(ranges: &[std::ops::Range<u64>]) {
    let inside = |va: u64| ranges.iter().any(|r| r.contains(&va));
    IMAGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|i| !inside(i.range.0));
    UNBOUND
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|u| !inside(u.stub));
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
    darwin::signal::set_fault_filter(guarded_write_filter);
    // The main thread runs the guest too, and still has the 8 KiB stack the
    // Rust runtime gave it.
    install_alternate_stack();
    keep_the_fault_signals();
    // `SIGABRT` beside the two faults, because the allocator is the third way a
    // program of this size dies and the only one that used to leave no report at
    // all: glibc's `free(): invalid size` goes to stderr and then straight to
    // `abort`, with no pc, no image and no registers. The handler restores the
    // previous disposition and returns, so the abort still happens - this only
    // adds the sentence in front of it.
    for signal in [libc::SIGSEGV, libc::SIGBUS, libc::SIGABRT] {
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
pub const KEEP_FAULT_HANDLER_ENV: &str = "ROSEMARY_KEEP_FAULT_HANDLER";

fn keep_the_fault_signals() {
    if !std::env::var_os(KEEP_FAULT_HANDLER_ENV).is_some_and(|v| v != "0") {
        return;
    }
    for signal in [libc::SIGSEGV, libc::SIGBUS, libc::SIGABRT, libc::SIGILL] {
        // The registry is keyed by Darwin's numbering, which is what the guest's
        // `sigaction` is given. `SIGBUS` is 10 there and 7 here.
        if let Some(darwin) = darwin::signal::signal_from_host(signal) {
            darwin::signal::retain(darwin);
        }
    }
    darwin::signal::retain_altstack();
    eprintln!(
        "rosemary: the guest's own handlers for SIGSEGV, SIGBUS, SIGABRT and SIGILL will be \
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

fn saved_for(signal: i32) -> &'static Saved {
    match signal {
        libc::SIGBUS => &BUS,
        libc::SIGABRT => &ABRT,
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

/// Whether a `SIGSEGV`/`SIGBUS` is a store to a guarded arena page, now writable.
/// Async-signal-safe.
fn is_guarded_write(signal: i32, info: *mut libc::siginfo_t) -> bool {
    if signal != libc::SIGSEGV && signal != libc::SIGBUS {
        return false;
    }
    // SAFETY: the kernel's own `siginfo_t`, live for the handler.
    let Some(info) = (unsafe { info.as_ref() }) else {
        return false;
    };
    // SAFETY: as above; `si_addr` is defined for both signals.
    if !crate::smc::on_fault(unsafe { darwin::signal::fault_address(info) }) {
        return false;
    }
    // The write may have changed code this thread is about to link into.
    crate::thread::break_links();
    true
}

/// The same question for [`darwin::signal`]'s handler, which displaces this one
/// when the guest installs a fault handler of its own.
pub(crate) fn guarded_write_filter(signal: i32, info: *mut libc::siginfo_t) -> bool {
    is_guarded_write(signal, info)
}

/// The handler. Prints, restores the previous disposition, and returns.
extern "C" fn report(signal: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    // A store to a page the code cache guarded is not a crash: unprotect, note it,
    // and let the store retry. First, and before any lock, since the store may be a
    // host `memcpy` as easily as lifted code. See [`crate::smc`].
    if is_guarded_write(signal, info) {
        return;
    }
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
        line.str("rosemary: fault: ");
        line.str(match signal {
            libc::SIGBUS => "SIGBUS",
            libc::SIGSEGV => "SIGSEGV",
            libc::SIGABRT => "SIGABRT",
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
    out.str("rosemary: fault: ");
    out.str(match signal {
        libc::SIGBUS => "SIGBUS",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGABRT => "SIGABRT",
        _ => "signal",
    });

    // SAFETY: the kernel passes a live `siginfo_t` for the duration of the
    // handler; `si_addr` is the faulting address for both signals handled here.
    stage(1);
    let addr = unsafe { info.as_ref() }.map_or(0, |i| unsafe { darwin::signal::fault_address(i) });
    out.str(" at ");
    out.hex(addr);
    say_access(out, info);
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
        // Host code that called a guest address directly: under the emulated
        // backend guest pages are not executable, so this is the callee's first
        // instruction and the word at `sp` names the host caller.
        else if in_image(pc) && !in_translation(pc) {
            say_host_call_into_guest(out, context);
        }
        if !in_translation(pc) {
            say_caller(out, context);
        }
    }
    // An abort has no interesting pc of its own - it is always inside the C
    // library - so the frame walk is the whole report.
    else if signal == libc::SIGABRT {
        say_caller(out, context);
    }

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

/// Name the host caller of a guest address that was called as host code - a
/// guest function pointer that reached the host without a thunk.
fn say_host_call_into_guest(out: &mut Buffer, context: *mut libc::c_void) {
    let Some((_, sp, lr)) = host_frame(context) else {
        return;
    };
    let ret = match lr {
        Some(lr) => lr,
        None if sp != 0 && sp % 8 == 0 => {
            // SAFETY: `sp` is the kernel's value for this thread's stack
            // pointer, and the fault is on the callee's first instruction, so
            // the word there is the return address the call pushed.
            unsafe { std::ptr::read(sp as *const u64) }
        }
        None => return,
    };
    out.str("\n  called as host code from ");
    out.hex(ret);
    if !say_image(out, ret) {
        say_host_mapping(out, ret);
    }
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
fn say_thread(out: &mut Buffer) {
    out.str("\n  thread ");
    out.hex(u64::from(crate::thread::key()));
    let Some(state) = crate::thread::current() else {
        out.str(" - not a guest thread; the fault is in this runtime or in a host library");
        return;
    };
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
        // Returning to a synchronous fault raises another Mach exception on
        // macOS, which a guest crash server may leave unanswered. Deliver the
        // default disposition as a signal instead of retrying the instruction.
        #[cfg(target_os = "macos")]
        if action.sa_sigaction == libc::SIG_DFL {
            clear_crash_ports();
            libc::raise(signal);
        }
    }
}

/// Fatal exit must not wait on a guest's Mach crash server after our report.
#[cfg(target_os = "macos")]
unsafe fn clear_crash_ports() {
    unsafe extern "C" {
        fn task_set_exception_ports(
            task: u32,
            mask: u32,
            port: u32,
            behavior: i32,
            flavor: i32,
        ) -> i32;
        fn thread_set_exception_ports(
            thread: u32,
            mask: u32,
            port: u32,
            behavior: i32,
            flavor: i32,
        ) -> i32;
        fn mach_port_deallocate(task: u32, port: u32) -> i32;
        fn mach_thread_self() -> u32;
        static mach_task_self_: u32;
    }
    const FATAL_EXCEPTIONS: u32 = (1 << 10) | (1 << 13); // EXC_CRASH | EXC_CORPSE_NOTIFY
    // SAFETY: current task/thread ports; MACH_PORT_NULL removes the server.
    unsafe {
        task_set_exception_ports(mach_task_self_, FATAL_EXCEPTIONS, 0, 1, 0);
        let thread = mach_thread_self();
        thread_set_exception_ports(thread, FATAL_EXCEPTIONS, 0, 1, 0);
        mach_port_deallocate(mach_task_self_, thread);
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
