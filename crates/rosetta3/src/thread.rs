use std::cell::{Cell, RefCell, UnsafeCell};
use std::ffi::c_void;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};

use microlift::arch::x86_64::context::RegisterContext;
use microlift::core::link::{BlockLinkConfig, BlockLinkEntry};
use microlift::runtime::ThreadLinks;
use static_init::dynamic;

use crate::stack::GuestStack;
use crate::vm::VmError;

/// The part of a guest thread another thread may read, and the whole of it.
pub struct GuestThread {
    /// Address of the innermost live [`RegisterContext`] on this thread, or zero when
    /// the thread is not inside guest code.
    context: AtomicU64,
    /// The guest stack, which is *not* the host stack this thread was created
    /// with - see [`crate::shims::machthread`] for why that matters to anything
    /// asking `pthread_get_stackaddr_np`.
    stack: Range<u64>,
}

impl GuestThread {
    /// The guest's registers, as far as this runtime can state them.
    pub fn context(&self) -> Option<&RegisterContext> {
        let at = self.context.load(Ordering::Acquire);
        // SAFETY: non-zero means `call_with_stack` has published its own local
        // and has not yet returned, so the context outlives this borrow. The
        // reader is either this thread or one that has suspended this one,
        // which is the only arrangement in which the value can be read at all.
        (at != 0).then(|| unsafe { &*(at as *const RegisterContext) })
    }

    /// Where this thread's guest frames live.
    pub fn stack(&self) -> Range<u64> {
        self.stack.clone()
    }
}

/// One registry entry: a thread's key and the state behind it. See [`key`].
struct Slot {
    key: AtomicU32,
    thread: AtomicPtr<GuestThread>,
}

/// Guest threads by [`key`], looked up **without taking a lock**.
///
/// The one race left is a *miss*.
static TABLE: [Slot; SLOTS] = [const {
    Slot {
        key: AtomicU32::new(0),
        thread: AtomicPtr::new(core::ptr::null_mut()),
    }
}; SLOTS];

const SLOTS: usize = 512;

/// Publish `thread` under `key`. The `Arc` is leaked into the table.
fn register(key: u32, thread: Arc<GuestThread>) {
    let raw = Arc::into_raw(thread) as *mut GuestThread;
    for slot in &TABLE {
        // The slot is claimed by its *pointer* and published by its key, in
        // that order, so a lookup that matches a key always finds a pointer.
        if slot
            .thread
            .compare_exchange(
                core::ptr::null_mut(),
                raw,
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            slot.key.store(key, Ordering::Release);
            return;
        }
    }
    // Out of slots. Not fatal and not silent.
    eprintln!(
        "rosetta3: more than {SLOTS} guest threads; thread {key} will not be          described to a collector"
    );
    // SAFETY: `raw` came from `Arc::into_raw` immediately above and no slot
    // took it, so this is the only reference.
    drop(unsafe { Arc::from_raw(raw) });
}

/// Withdraw `key`, freeing its slot for a thread created later.
fn unregister(key: u32) {
    for slot in &TABLE {
        if slot.key.load(Ordering::Acquire) == key {
            // Key first: a lookup racing this sees zero and misses, which is
            // the right answer for a thread that is ending.
            slot.key.store(0, Ordering::Release);
            slot.thread.store(core::ptr::null_mut(), Ordering::Release);
            return;
        }
    }
}

/// What identifies this host thread to something looking at it from outside.
///
/// The Mach port name on Darwin, which is what `thread_get_state` is given; the kernel
/// thread id elsewhere. Either way it is unique while the thread runs, readable from
/// the thread without a lock, and the key the table above is built on. See
/// [`darwin::pthread::id`].
pub fn key() -> u32 {
    darwin::pthread::id()
}

/// The guest thread registered under `key`, if it is one.
pub fn by_key(key: u32) -> Option<&'static GuestThread> {
    if key == 0 {
        return None;
    }
    for slot in &TABLE {
        if slot.key.load(Ordering::Acquire) == key {
            let raw = slot.thread.load(Ordering::Acquire);
            // SAFETY: a non-null pointer in the table came from
            // `Arc::into_raw` in `register` and is never freed - see the note
            // there on why it is leaked.
            return (!raw.is_null()).then(|| unsafe { &*raw });
        }
    }
    None
}

/// How many guest threads the table holds right now.
///
/// A count, not a `bool`, so a report can say "0" rather than leaving the reader to
/// wonder whether the question was asked. Diagnostic only: nothing in the run loop
/// reads it. See [`crate::fault::say_thread`] for why a fault report needs it.
pub fn registered() -> usize {
    TABLE
        .iter()
        .filter(|slot| slot.key.load(Ordering::Acquire) != 0)
        .count()
}

/// Call `f` with the [`key`] of every registered guest thread.
///
/// A thread that ends mid-walk is reported and then fails to suspend, which is the
/// ordinary race and is why the sampler treats a failed suspend as a miss rather than
/// an error.
pub fn each_key(mut f: impl FnMut(u32)) {
    for slot in &TABLE {
        let key = slot.key.load(Ordering::Acquire);
        if key != 0 {
            f(key);
        }
    }
}

/// Everything about executing guest code that belongs to one host thread.
pub struct ThreadState {
    /// The half of this state other threads read. Held here as well so the
    /// owning thread updates it without going near the registry's lock.
    published: Arc<GuestThread>,
    stack: GuestStack,
    /// Where the next nested guest entry should put its first frame.
    pub(crate) rsp: Cell<u64>,
    /// How many host->guest callbacks are live on this thread.
    pub(crate) depth: Cell<usize>,
    /// An error raised inside a host->guest callback, which cannot be returned through
    /// the host function that caused it.
    pub(crate) pending: RefCell<Option<VmError>>,
    /// Which entry into guest code is live on this thread.
    entry: Cell<u64>,
    /// Whether the call being serviced set `rip` itself instead of returning.
    redirected: Cell<bool>,
    /// Whether the import being serviced was reached by a **jump** rather than a call.
    entered_by_jump: Cell<bool>,
    /// The exception being unwound on this thread, if any.
    unwinding: Cell<Option<crate::unwind::InFlight>>,
    /// Exception headers whose `catch` blocks are running, innermost last.
    caught: RefCell<Vec<u64>>,
    /// Translations this thread has already entered, by the PC it entered them
    /// at, plus the two tables lifted code reads. See [`CodeWindow`].
    code: CodeWindow,
    /// Where this thread's indirect calls went last time. See
    /// [`crate::dispatch::IndirectCache`].
    indirect: crate::dispatch::IndirectCache,
}

impl ThreadState {
    /// Wrap a stack this thread will run the guest on.
    pub fn new(stack: GuestStack) -> Self {
        let rsp = stack.initial_rsp();
        let published = Arc::new(GuestThread {
            context: AtomicU64::new(0),
            stack: stack.range(),
        });
        ThreadState {
            published,
            stack,
            rsp: Cell::new(rsp),
            depth: Cell::new(0),
            pending: RefCell::new(None),
            entry: Cell::new(0),
            redirected: Cell::new(false),
            entered_by_jump: Cell::new(false),
            unwinding: Cell::new(None),
            caught: RefCell::new(Vec::new()),
            code: CodeWindow::new(),
            indirect: crate::dispatch::IndirectCache::new(),
        }
    }

    /// Publish `ctx` as this thread's live guest context, returning whatever
    /// was published before so a nested entry can restore it.
    pub(crate) fn publish_context(&self, ctx: *mut RegisterContext) -> u64 {
        self.published
            .context
            .swap(ctx as usize as u64, Ordering::Release)
    }

    /// Restore what [`ThreadState::publish_context`] displaced.
    pub(crate) fn restore_context(&self, previous: u64) {
        self.published.context.store(previous, Ordering::Release);
    }

    /// This thread's view of the code cache.
    pub(crate) fn code(&self) -> &CodeWindow {
        &self.code
    }

    /// This thread's memory of where its indirect calls went.
    pub(crate) fn indirect(&self) -> &crate::dispatch::IndirectCache {
        &self.indirect
    }

    /// The entry into guest code currently live on this thread, or zero when
    /// none is.
    pub(crate) fn entry(&self) -> u64 {
        self.entry.get()
    }

    /// Begin a new entry, returning the previous one to be restored after it.
    pub(crate) fn enter(&self) -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let previous = self.entry.get();
        self.entry
            .set(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        previous
    }

    /// Restore the entry that was live before [`ThreadState::enter`].
    pub(crate) fn leave(&self, previous: u64) {
        self.entry.set(previous);
    }

    /// The exception being unwound on this thread.
    pub(crate) fn in_flight(&self) -> Option<crate::unwind::InFlight> {
        self.unwinding.get()
    }

    /// Set or clear it. Cleared by `__cxa_end_catch`, and by a walk that ends
    /// with nowhere to go.
    pub(crate) fn set_in_flight(&self, flight: Option<crate::unwind::InFlight>) {
        self.unwinding.set(flight);
    }

    /// Note that a `catch` block for `header` has begun on this thread.
    pub(crate) fn begin_catch(&self, header: u64) {
        self.caught.borrow_mut().push(header);
    }

    /// The exception whose `catch` block is innermost, without ending it.
    pub(crate) fn caught(&self) -> Option<u64> {
        self.caught.borrow().last().copied()
    }

    /// End the innermost `catch` block, naming the exception it was for.
    pub(crate) fn end_catch(&self) -> Option<u64> {
        self.caught.borrow_mut().pop()
    }

    /// Record that the call being serviced transferred control rather than
    /// returning: its `rip` is a destination, not a resume point.
    pub(crate) fn redirect(&self) {
        self.redirected.set(true);
    }

    /// Whether the last serviced call redirected, clearing the flag.
    pub(crate) fn took_redirect(&self) -> bool {
        self.redirected.replace(false)
    }

    /// Record how the import now being serviced was reached.
    pub(crate) fn set_entered_by_jump(&self, jumped: bool) {
        self.entered_by_jump.set(jumped);
    }

    /// Whether it was a jump. Only meaningful while a shim is running.
    pub(crate) fn entered_by_jump(&self) -> bool {
        self.entered_by_jump.get()
    }

    /// `(argc, argv, envp, apple)`, as laid out on this thread's stack.
    pub fn main_args(&self) -> (u64, u64, u64, u64) {
        self.stack.main_args()
    }

    /// The `rsp` an `LC_UNIXTHREAD` entry point is entered with, for the same stack and
    /// with the same restriction: only the thread that started the program has one.
    pub fn kernel_sp(&self) -> Option<u64> {
        self.stack.kernel_sp()
    }

    /// The address range this thread's guest stack occupies.
    ///
    /// Two loads and no search, which is what makes it worth asking before
    /// [`crate::vm::GuestView`] is asked the same question the general way - see
    /// `Vm::enter_guest_callee`.
    pub fn stack_range(&self) -> Range<u64> {
        self.stack.range()
    }

    /// The guest stack pointer as of the last thing that moved it.
    ///
    /// For [`crate::fault`], which asks on a thread that is about to die and wants the
    /// answer without a borrow it could not take.
    pub fn live_rsp(&self) -> u64 {
        match self.published.context() {
            Some(ctx) => ctx.rsp,
            None => self.rsp.get(),
        }
    }

    /// The register context of the entry below this one, if there is one.
    ///
    /// Published by [`publish_context`](Self::publish_context) for the duration of an
    /// entry, so during the *setup* of a nested one it is still the outer entry's -
    /// which is what
    /// [`ThreadLinks::resume_call_return`](microlift::runtime::ThreadLinks::resume_call_return)
    /// wants, and the only reader that needs it before the swap.
    pub(crate) fn published_context(&self) -> Option<&RegisterContext> {
        self.published.context()
    }
}

/// One thread's window onto the code cache, and the two tables lifted code reads.
///
/// Never invalidated, for the reason the cache itself is never invalidated.
pub(crate) struct CodeWindow {
    slots: RefCell<Box<[Option<(u64, std::sync::Arc<crate::cache::Translation>)>]>>,
    /// The two tables lifted code reads: the rows a probe hashes into, and the
    /// `<return address, link cell>` frames a `call` pushes and a `RET` pops.
    ///
    /// Microlift's own type rather than a pair of bare arrays, so that the slot a
    /// probe computes and the slot a publish writes are the same function called
    /// twice, and so that "which pointers a context owes a region" is stated in one
    /// place instead of here as well. See [`ThreadLinks`].
    links: UnsafeCell<ThreadLinks>,
    /// The geometry `slot` uses, copied out of [`link_config`] once.
    config: BlockLinkConfig,
}

/// How many PCs one thread remembers, unless [`WINDOW_ENV`] says otherwise. A power
/// of two, so the index is a mask.
const WINDOW: usize = 8192;

pub const WINDOW_ENV: &str = "ROSETTA3_LINK_ROWS";

/// How many rows this run uses.
fn window() -> usize {
    #[dynamic]
    static ROWS: usize = {
        match std::env::var(WINDOW_ENV) {
            Ok(v) => match v.trim().parse::<usize>() {
                Ok(n) if n >= 256 => n.next_power_of_two(),
                _ => {
                    eprintln!(
                        "rosetta3: {WINDOW_ENV}={v:?} is not an integer >= 256; using {WINDOW}"
                    );
                    WINDOW
                }
            },
            Err(_) => WINDOW,
        }
    };
    *ROWS
}

/// The table geometry, shared with the lifter so that the slot lifted code
/// computes and the slot [`CodeWindow::put`] writes are the same arithmetic
/// rather than two copies of it.
pub fn link_config() -> BlockLinkConfig {
    #[dynamic]
    static CONFIG: BlockLinkConfig = {
        let config = BlockLinkConfig::new(window() as u32);
        // Which of the two shapes of linking this process runs.
        match crate::jit::pinned_link_abi() {
            Some(abi) => config.pinned(abi),
            None => config,
        }
    };
    *CONFIG
}

impl CodeWindow {
    fn new() -> Self {
        let config = link_config();
        let rows = config.entries as usize;
        CodeWindow {
            slots: RefCell::new(vec![None; rows].into_boxed_slice()),
            links: UnsafeCell::new(ThreadLinks::new(config, crate::jit::call_return_stack())),
            config,
        }
    }

    /// The slot a PC maps to.
    ///
    /// Microlift's own `slot`, so this and the shift-and-mask baked into every region
    /// cannot drift apart - called on the copy this window was built with rather than
    /// on the static, which is the same arithmetic without the read.
    fn slot(&self, pc: u64) -> usize {
        self.config.slot(pc)
    }

    /// The address of the block-link table for this thread, as `ctx.link_table`
    /// wants it.
    pub(crate) fn link_table(&self) -> *mut BlockLinkEntry {
        // SAFETY: taking the address of the allocation, not reading it. The
        // table is owned by this `CodeWindow` and never reallocated, so the
        // address is stable for the life of the thread - which is what lets it
        // be handed to compiled code at all.
        unsafe { (*self.links.get()).link_table_ptr() }
    }

    /// Point `ctx` at both tables, so it may enter lifted code on this thread.
    ///
    /// One call rather than a store per field, and microlift's own, so a field
    /// added to the pair is not a store this crate has to remember to add.
    pub(crate) fn attach(&self, ctx: &mut RegisterContext) {
        // SAFETY: as `link_table`.
        unsafe { (*self.links.get()).attach(ctx) };
    }

    pub(crate) fn relink(&self, pc: u64, translation: &std::sync::Arc<crate::cache::Translation>) {
        // SAFETY: as `put` - the only writer, on the thread that owns this window,
        // in the run loop rather than inside a region.
        unsafe {
            let links = &mut *self.links.get();
            if links.table().get(pc).is_some() {
                return;
            }
            if links.publish_at(translation, pc) {
                crate::stats::record_relink();
            }
        }
    }

    /// The translation this thread last entered at `pc`, if it still holds one.
    pub(crate) fn get(&self, pc: u64) -> Option<std::sync::Arc<crate::cache::Translation>> {
        match self.slots.borrow().get(self.slot(pc)) {
            Some(Some((at, translation))) if *at == pc => Some(std::sync::Arc::clone(translation)),
            _ => None,
        }
    }

    /// Remember that `translation` may be entered at `pc`, and make it linkable from
    /// this thread at every address it can be resumed at.
    ///
    /// Not just at `pc`: publishing the whole set is what lets a link to an address
    /// this thread has never entered hit anyway, and it is the protocol microlift's
    /// own `ThreadRuntime::publish` states - a row must exist at exactly the resume
    /// points and nowhere else, since an address that is not one either runs the
    /// region from the top or exits having made no progress.
    ///
    /// A row for an address whose slot is taken evicts the row that was there, which
    /// costs a lookup - so how much of a translation goes in is a knob:
    /// `ROSETTA3_LINK_PUBLISH=entry` publishes only the entry row. See
    /// [`crate::jit::link_publish`].
    pub(crate) fn put(&self, pc: u64, translation: &std::sync::Arc<crate::cache::Translation>) {
        let slot = self.slot(pc);
        // `try_borrow_mut` rather than `borrow_mut`.
        if let Ok(mut slots) = self.slots.try_borrow_mut() {
            slots[slot] = Some((pc, std::sync::Arc::clone(translation)));
            // The rows go in with the `Arc`, and only with it: a row names code
            // that must stay mapped, and the cache is what keeps it so - see
            // `Published::retained`. The borrow above is also what serialises this
            // against a nested run loop writing the same slot.
            //
            // SAFETY: the only writer, on the thread that owns this window, at a
            // point where this thread is in the run loop rather than inside a
            // region. See the field's own note.
            let rows = unsafe {
                match crate::jit::link_publish() {
                    crate::jit::LinkPublish::Entry => {
                        usize::from((*self.links.get()).publish_at(translation, pc))
                    }
                    crate::jit::LinkPublish::All => {
                        (*self.links.get()).publish(translation);
                        translation.resume_points.len()
                    }
                }
            };
            crate::stats::record_published(rows);
        }
    }
}

/// The key every thread's [`ThreadState`] hangs off.
///
/// A raw pthread key, not `thread_local!`. Rust's is thread-specific data with a
/// "destroyed" flag in front of it, and macOS trips that flag on the main thread
/// *inside* `exit` - precisely when the guest's `atexit` handlers run and re-enter the
/// JIT. A raw key has no such flag: the value stays readable until the thread ends.
/// The destructor frees a *secondary* thread's stack, so a guest that spawns and joins
/// in a loop does not accumulate an 8 MiB reservation per thread.
#[dynamic]
static KEY: libc::pthread_key_t = {
    let mut key: libc::pthread_key_t = 0;
    // SAFETY: `key` is a live local of the right type, and `release` matches
    // the destructor signature libpthread calls it with.
    let rc = unsafe { libc::pthread_key_create(&mut key, Some(release)) };
    assert_eq!(
        rc, 0,
        "pthread_key_create failed; the runtime cannot run a guest thread"
    );
    key
};

/// The pthread key `ThreadState` hangs off - not [`key`], which identifies the thread
/// itself.
#[inline(always)]
fn tsd_key() -> libc::pthread_key_t {
    *KEY
}

/// Free a finished thread's state. Never called on the main thread.
unsafe extern "C" fn release(value: *mut c_void) {
    if value.is_null() {
        return;
    }
    // Unregistered first: after this the port may be reused by a thread created
    // later, and an entry naming a freed stack is worse than no entry.
    unregister(key());
    // SAFETY: the only thing ever stored under this key is a `Box::into_raw` of
    // a `ThreadState`, and libpthread calls this once, with the key cleared, so
    // the box is not reachable from anywhere else.
    drop(unsafe { Box::from_raw(value as *mut ThreadState) });
}

/// This thread's state, if it has entered guest code before.
pub fn current() -> Option<&'static ThreadState> {
    // SAFETY: reading a thread-specific value, which is null or the pointer
    // `install` stored. Dereferencing is sound because `release` is the only
    // thing that frees it and it runs after the last code on this thread.
    let p = unsafe { libc::pthread_getspecific(tsd_key()) } as *const ThreadState;
    (!p.is_null()).then(|| unsafe { &*p })
}

/// Give this thread `state`, replacing and freeing whatever it had.
pub fn install(state: ThreadState) -> &'static ThreadState {
    // Replacing rather than adding: a thread runs one guest at a time, and a
    // second `Vm` built on this thread is a new program, not a second stack for
    // the old one.
    // SAFETY: reads this thread's slot; anything non-null there is a box this
    // module leaked, and the slot is about to be overwritten so nothing can
    // reach it again.
    let old = unsafe { libc::pthread_getspecific(tsd_key()) };
    if !old.is_null() {
        drop(unsafe { Box::from_raw(old as *mut ThreadState) });
    }
    let raw = Box::into_raw(Box::new(state));
    // SAFETY: `raw` is a live leaked allocation; `release` takes ownership of it
    // when the thread ends.
    unsafe { libc::pthread_setspecific(tsd_key(), raw as *mut c_void) };
    // SAFETY: just stored, and nothing has run since that could free it.
    let state = unsafe { &*raw };
    // Registered under this thread's Mach port, which is how a collector on
    // another thread finds it.
    register(key(), Arc::clone(&state.published));
    clear_errno();
    state
}

/// Hand this thread the `errno` a thread that has just started really has.
#[inline(always)]
pub(crate) fn clear_errno() {
    darwin::errno::set(0);
}

/// The guest's `errno`, put back exactly as it was.
///
/// **Not defensive tidying - a measured bug, twice.** `dlopen` of a framework this
/// system does not have sets `ENOENT`, and [`crate::gui`]'s `sys` does exactly that on
/// the main thread between [`clear_errno`] and the guest's `main`.
pub(crate) type Errno = darwin::errno::Held;

/// The base of this thread's Darwin thread-specific data array - the value a guest's
/// `%gs` segment base must hold.
///
/// The host's own array where the host is a Mac, and a synthesised one where it is
/// not. See [`darwin::tls`], which owns both answers because which one is right is a
/// question about the platform rather than about this runtime.
pub fn tsd_base() -> u64 {
    darwin::tls::tsd_base()
}

/// Stack for a guest thread that asked for no particular one.
///
/// **libpthread's default for a created thread, not the main thread's.** The two
/// differ by a factor of sixteen: a main thread gets 8 MiB from the kernel and
/// `pthread_create` with a null attribute set gets 512 KiB. A guest that never
/// said is not asking for "as much as possible" - it is asking for the default,
/// and `pthread_get_stacksize_np` is how it finds out which it got.
///
/// Getting this wrong is not a crash. It is a guest thread with sixteen times
/// the stack it would have had, which only shows up as a program that does not
/// overflow where it would natively - or, on a program that sizes something from
/// it, as a number that disagrees with the same binary run for the host.
pub const THREAD_STACK: usize = 512 * 1024;

/// Where a guest thread's frames are to live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackRequest {
    /// Reserve [`THREAD_STACK`] bytes, libpthread's default.
    Default,
    /// Reserve `bytes`, rounded up to a page. What the guest asked for through
    /// `pthread_attr_setstacksize`.
    Sized(usize),
    /// Use memory the guest already owns: `top` is the highest address, exclusive,
    /// matching `pthread_attr_getstackaddr`. See [`GuestStack::supplied`].
    Supplied { top: u64, size: u64 },
}

impl StackRequest {
    /// Build the stack this asks for.
    fn build(self) -> Result<GuestStack, VmError> {
        match self {
            // The guard on top of what was asked for, because what the guest
            // asked for is the part it may use. Paired with the subtraction in
            // [`GuestStack::bare`], so the two have to name the same quantity -
            // and the default is a size the guest asked for by not asking, so it
            // is reserved the same way rather than one guard page short.
            StackRequest::Default => GuestStack::bare(THREAD_STACK + crate::stack::guard()),
            StackRequest::Sized(bytes) => GuestStack::bare(bytes + crate::stack::guard()),
            StackRequest::Supplied { top, size } => Ok(GuestStack::supplied(top, size)),
        }
    }
}

/// Make this host thread a guest thread with the stack `request` describes, replacing
/// any state it already had.
pub fn adopt(request: StackRequest) -> Result<&'static ThreadState, VmError> {
    Ok(install(ThreadState::new(request.build()?)))
}

/// What a new guest thread is to run, and on what.
pub struct GuestStart {
    /// The **thunk** for the guest's start routine, not the guest address.
    pub entry: u64,
    /// The `void *` the guest passed, untranslated. Only guest code ever
    /// dereferences it, and the two sides share an address space.
    pub arg: u64,
    /// Where the guest's frames go.
    pub stack: StackRequest,
    /// Set by the new thread once its Darwin id is published. See [`spawn`].
    published: Arc<AtomicBool>,
}

impl GuestStart {
    pub fn new(entry: u64, arg: u64, stack: StackRequest) -> Self {
        Self {
            entry,
            arg,
            stack,
            published: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Start a host thread that runs `start` as a guest thread.
///
/// Returns only once the new thread has published its Darwin id, because the
/// guest may read it the instant `pthread_create` returns: Unreal records a
/// worker's `pthread_mach_thread_np` there and asserts on it from then on. This
/// is `darwin::pthread::create`'s handshake, which the emulated backend cannot
/// reuse - a guest thread needs a guest stack, so it starts here instead.
pub fn spawn(start: GuestStart, attr: &libc::pthread_attr_t) -> Result<libc::pthread_t, i32> {
    let mut thread: libc::pthread_t = darwin::pthread::null_thread();
    let published = Arc::clone(&start.published);
    let boxed = Box::into_raw(Box::new(start)) as *mut c_void;
    // SAFETY: `entry` is this module's own function; `boxed` is a live leaked
    // allocation which the new thread takes ownership of, and which is
    // reclaimed here if no thread is ever started to take it.
    let rc = unsafe { libc::pthread_create(&mut thread, attr, entry, boxed) };
    if rc != 0 {
        // SAFETY: `pthread_create` failed, so nothing else holds the pointer.
        drop(unsafe { Box::from_raw(boxed as *mut GuestStart) });
        return Err(rc);
    }
    while !published.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    Ok(thread)
}

/// What libpthread calls on the new thread: install the guest state, then enter the
/// guest.
extern "C" fn entry(raw: *mut c_void) -> *mut c_void {
    // Unboxed before the guest runs, and the fields copied out, because the
    // guest's start routine may call `pthread_exit` - which ends this thread
    // from underneath these Rust frames without unwinding them. A `Box` still
    // live at that moment is a leak once per thread that exits that way.
    // SAFETY: the pointer came from `Box::into_raw` in `spawn` and is handed to
    // exactly one thread, which is this one.
    let GuestStart {
        entry,
        arg,
        stack,
        published,
    } = *unsafe { Box::from_raw(raw as *mut GuestStart) };

    // Before anything else this thread does, and before `spawn` returns: the
    // handle the parent is about to be handed has to name a thread whose id the
    // registry already knows.
    darwin::pthread::publish_self();
    published.store(true, Ordering::Release);

    // `sigaltstack` is per thread. Without one here, a fault whose cause has
    // already made this thread's stack unusable takes the report with it.
    crate::fault::install_alternate_stack();

    if let Err(e) = adopt(stack) {
        // No error channel.
        eprintln!("rosetta3: a guest thread could not be given a stack: {e}");
        return core::ptr::null_mut();
    }

    // SAFETY: `entry` is a thunk this runtime compiled - an arm64 function
    // taking the guest's six integer argument registers and eight vector ones,
    // of which a start routine reads the first. It stays mapped for the VM's
    // lifetime, which outlives every thread that can reach it. See
    // [`crate::thunk`].
    let f: extern "C" fn(u64) -> u64 = unsafe { core::mem::transmute::<u64, _>(entry) };
    f(arg) as *mut c_void
}

/// This thread's state, creating a default one if it has none.
pub fn ensure() -> Result<&'static ThreadState, VmError> {
    if let Some(existing) = current() {
        return Ok(existing);
    }
    adopt(StackRequest::Default)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard restores, rather than clearing.
    ///
    /// The mechanism is `darwin::errno`'s and tested there; what this asserts is that
    /// [`Errno`] still names it, because a shim holding the wrong guard restores the
    /// wrong thread's `errno`.
    #[test]
    fn a_held_errno_comes_back_exactly_as_it_was() {
        for value in [libc::EPIPE, 0] {
            darwin::errno::set(value);
            {
                let _held = Errno::now();
                darwin::errno::set(libc::ENOENT);
            }
            assert_eq!(darwin::errno::get(), value);
        }
    }

    /// Every guest thread must read its own thread-specific data, because slot zero
    /// of that array is what the guest's `pthread_self` returns.
    #[test]
    fn each_thread_has_its_own_tsd_base() {
        let mine = tsd_base();
        assert_ne!(mine, 0);
        let theirs = std::thread::spawn(tsd_base)
            .join()
            .expect("a thread has one");
        assert_ne!(mine, theirs);
    }
}
