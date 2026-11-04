use std::collections::HashMap;
use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use machish::memory::Regions;

use microlift::arch::x86_64::X86_64;
use microlift::arch::x86_64::context::RegisterContext;
use microlift::core::exit::ExitReason;
use microlift::core::{LifterConfig, lift_to_code};

use crate::program::Program;
use crate::vm::VmError;

/// One lifted, compiled guest region
pub use microlift::runtime::Translation;

/// Enter `translation`, resuming at `ctx.rip`, and report how it left.
///
/// A thin wrapper over [`microlift::runtime::enter`], which is where the sequence
/// itself lives: the fields an entry owes a region, the call, and the chain of
/// successors the region and its successors resolve for themselves. What this adds
/// is the accounting, which is this crate's - a linked entry is an entry that never
/// reached the run loop, and both counters have to keep counting the events they
/// always counted or a profile stops comparing against older ones.
///
/// The exits that *do* need the run loop must never be linked, and they are not.
pub fn enter(translation: &Translation, ctx: &mut RegisterContext) -> ExitReason {
    // SAFETY: `translation` was compiled by this process's own `CodeCache` for this
    // `RegisterContext` layout and this thread's table geometry, and every row
    // reachable from `ctx.link_table` names code the cache keeps mapped for the life
    // of the process - see [`Published::retained`]. That is microlift's trust model,
    // and past the address compare lifted code checks none of it.
    let entered = unsafe {
        microlift::runtime::enter_with(translation, ctx, |exit, ctx| {
            // Once per hop, with the exit of the region that resolved it. Both
            // counters, so that the entry *rate* and the exit *mix* keep counting
            // what they always did.
            if crate::stats::enabled() {
                crate::stats::record_exit(crate::vm::classify(exit, ctx.exit_code));
                crate::stats::record_linked_entry();
            }
        })
    };
    // The shared cache's counter, taken from the count rather than bumped inside the
    // chain: reaching it from in there would hold the cache's lock across the
    // execution of guest code.
    //
    // Guarded on there being something to count, which is not a micro-optimisation:
    // finding the VM is a TLS read, this runs once per entry into guest code, and
    // under a pinned ABI the answer is *always* zero because a link is a branch the
    // host never sees. So the guard removes the read entirely from the default
    // configuration rather than saving an add in it.
    if entered.links > 0
        && let Some(vm) = crate::thunk::current()
    {
        vm.note_links_followed(entered.links);
    }
    entered.exit
}

/// Every address a published translation can be entered at, as an open-addressed
/// table read without taking the cache's lock.
struct EntryIndex {
    table: AtomicPtr<Table>,
    /// Addresses in the live table. Written only by a lift, which holds the cache's
    /// write lock and is therefore the only writer.
    live: AtomicUsize,
}

struct Table {
    mask: u64,
    slots: Box<[Slot]>,
}

struct Slot {
    pc: AtomicU64,
    translation: AtomicPtr<Translation>,
}

/// A slot nothing has been written to. No guest address is `u64::MAX`.
const EMPTY: u64 = u64::MAX;

/// Guest addresses are page- and instruction-aligned in the low bits and identical in
/// the high ones, so the index is taken from the *middle* of a multiply.
fn mix(pc: u64) -> u64 {
    pc.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 24
}

impl Table {
    fn empty(capacity: usize) -> Box<Table> {
        let capacity = capacity.next_power_of_two();
        let mut slots = Vec::with_capacity(capacity);
        slots.resize_with(capacity, || Slot {
            pc: AtomicU64::new(EMPTY),
            translation: AtomicPtr::new(std::ptr::null_mut()),
        });
        Box::new(Table {
            mask: capacity as u64 - 1,
            slots: slots.into_boxed_slice(),
        })
    }

    fn get(&self, pc: u64) -> Option<*const Translation> {
        let mut at = mix(pc) & self.mask;
        loop {
            let slot = &self.slots[at as usize];
            match slot.pc.load(Ordering::Acquire) {
                EMPTY => return None,
                seen if seen == pc => {
                    // Null once retracted; see [`EntryIndex::retract`].
                    let t = slot.translation.load(Ordering::Acquire);
                    return (!t.is_null()).then_some(t.cast_const());
                }
                _ => at = (at + 1) & self.mask,
            }
        }
    }

    /// Write `pc`, reporting whether the address was new. Writer side only.
    fn put(&self, pc: u64, translation: *const Translation) -> bool {
        let mut at = mix(pc) & self.mask;
        loop {
            let slot = &self.slots[at as usize];
            match slot.pc.load(Ordering::Relaxed) {
                EMPTY => {
                    // The pointer goes in first, so a reader that sees the address
                    // sees a translation to go with it.
                    slot.translation
                        .store(translation as *mut Translation, Ordering::Release);
                    slot.pc.store(pc, Ordering::Release);
                    return true;
                }
                // A re-lift of the same address. Either pointer answers for it.
                seen if seen == pc => {
                    slot.translation
                        .store(translation as *mut Translation, Ordering::Release);
                    return false;
                }
                _ => at = (at + 1) & self.mask,
            }
        }
    }
}

impl EntryIndex {
    fn new() -> Self {
        EntryIndex {
            table: AtomicPtr::new(Box::into_raw(Table::empty(1024))),
            live: AtomicUsize::new(0),
        }
    }

    fn table(&self) -> &'static Table {
        // SAFETY: every table this pointer has ever held was leaked by `Box::into_raw`
        // and is never freed, precisely so that a reader still probing the table a
        // grow displaced keeps reading a live allocation.
        unsafe { &*self.table.load(Ordering::Acquire) }
    }

    /// A translation that can be entered at `pc`, or `None` if this table does not
    /// know of one - which a caller must read as "ask the regions", not as "there is
    /// none": a lift racing this probe publishes after it.
    fn get(&self, pc: u64) -> Option<*const Translation> {
        self.table().get(pc)
    }

    /// Publish `translation` under every address it may be entered at.
    ///
    /// The caller holds the cache's write lock, which is what makes the stores below
    /// the only ones in flight.
    fn publish(&self, entries: &[u64], translation: *const Translation) {
        self.grow_for(entries.len());
        let table = self.table();
        let mut added = 0;
        for &pc in entries {
            added += usize::from(table.put(pc, translation));
        }
        self.live.fetch_add(added, Ordering::Relaxed);
    }

    /// Forget `pc`, which a write to guest code retired. The slot keeps its address,
    /// since an open-addressed probe cannot have holes punched in it; a null pointer
    /// reads as a miss, and a re-lift fills it again. Writer side only.
    fn retract(&self, pc: u64) {
        let table = self.table();
        let mut at = mix(pc) & table.mask;
        loop {
            let slot = &table.slots[at as usize];
            match slot.pc.load(Ordering::Relaxed) {
                EMPTY => return,
                seen if seen == pc => {
                    slot.translation
                        .store(std::ptr::null_mut(), Ordering::Release);
                    return;
                }
                _ => at = (at + 1) & table.mask,
            }
        }
    }

    /// Keep the table under half full, so a probe of a missing address always reaches
    /// an empty slot rather than running the whole array.
    fn grow_for(&self, extra: usize) {
        let table = self.table();
        let wanted = self.live.load(Ordering::Relaxed) + extra;
        if wanted * 2 <= table.slots.len() {
            return;
        }
        let bigger = Table::empty(wanted * 4);
        // Rehashed out of the old table rather than rebuilt from the regions: an
        // address is never removed from it, only nulled, so the old table is exactly
        // the set to keep.
        for slot in &table.slots {
            let pc = slot.pc.load(Ordering::Relaxed);
            if pc != EMPTY {
                bigger.put(pc, slot.translation.load(Ordering::Relaxed));
            }
        }
        // The displaced table is leaked. Capacity quadruples, so everything ever
        // leaked together is a third of the live table.
        self.table.store(Box::into_raw(bigger), Ordering::Release);
    }
}

/// Translations, keyed by every guest address each may be entered at.
///
/// Two maps of the same thing, on purpose:
///
/// - [`microlift::runtime::CodeCache`] is the authority. It owns the translations, it
///   owns the page index an invalidation sweeps (a write to generated code; see
///   [`crate::smc`]), and - the reason this crate stopped
///   keeping its own map - it owns the **link cells**. Inserting a translation there is
///   what fills in the cells of every region that had been branching to it and the
///   cells of its own that name something already compiled, in both directions and
///   eagerly, which is the whole of what the new block linking buys.
/// - [`EntryIndex`] is a lock-free read of the same answer, for the one caller that
///   asks on a hot path.
///
/// A hit reads only the index; a miss takes the write lock and lifts. That the lock is
/// on the cold path is not an assumption - `CacheStats::miss_rate` measures it, minus
/// the hits the index answered without asking.
pub struct CodeCache {
    published: RwLock<Published>,
    /// The lock-free half of [`published`](Self::published): what a hit reads.
    index: EntryIndex,
    /// How many translations have been inserted, and what [`CodeCache::len`] last
    /// answered after that many. See `len`.
    inserts: AtomicU64,
    counted: std::sync::Mutex<(u64, usize)>,
    /// Images whose cached code has been looked for, by path: each is looked for
    /// once, at its first miss.
    adopted: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Everything microlift needs to know before it lifts anything: the registered
    /// import addresses and the read-only segment hints. Immutable, so a lift reads it
    /// without taking anything.
    config: LifterConfig,
}

/// Reads image code at `va`, from the program or an image the guest `dlopen`ed.
fn read_image_code<M: Regions>(program: &Program<M>, va: u64, out: &mut [u8]) -> bool {
    if program.memory.read(va, out).is_ok() {
        return true;
    }
    let Some((base, bytes)) = crate::dyld::region_containing(va) else {
        return false;
    };
    let from = (va - base) as usize;
    match bytes.get(from..from + out.len()) {
        Some(code) => {
            out.copy_from_slice(code);
            true
        }
        None => false,
    }
}

/// The half of the cache a lift mutates.
struct Published {
    cache: microlift::runtime::CodeCache,
    /// Every translation ever inserted, held so that none is ever freed.
    ///
    /// **This is what makes the raw pointers in [`EntryIndex`] sound**, and it is the
    /// same bargain the old `retired` vector struck for the same reason: an index that
    /// hands out a `*const Translation` without a lock cannot also be told when one
    /// dies. It covers two ways a translation would otherwise be dropped - a re-lift of
    /// an address two threads missed on at once, which displaces the loser from
    /// microlift's map, and an invalidation, whose retired list this crate never
    /// [`reclaim`](microlift::runtime::CodeCache::reclaim)s because it has no
    /// cross-thread safepoint to reclaim at.
    ///
    /// So nothing is unmapped for the life of the process. That was already true before
    /// the switch; what is new is that it is now the *only* thing keeping it true.
    retained: Vec<Arc<Translation>>,
    /// The bytes each translation of guest-generated code was lifted from, by host
    /// page: what a drain compares a written page against. See [`crate::smc`].
    snapshots: HashMap<u64, Vec<(Arc<Translation>, Arc<crate::smc::Snapshot>)>>,
}

/// How many times a lift of generated code is retried when its bytes change before
/// it can be published. Past it the translation is published anyway and its pages
/// queued, so it runs at most until the next drain retires it: a write racing an
/// execution on another thread may be seen either side of it.
const STALE_RETRIES: usize = 4;

// SAFETY: the one thing keeping a cache off a second thread is the raw pointer
// inside `LifterConfig`'s `DynamicSymbol`s - the *host address* a fast import
// resolved to. It is a function address, cloned into each lift and never
// dereferenced on this side; the code that eventually calls it is compiled arm64
// doing what any thread may do with a function pointer. The translations beside
// it carry their own claim, as `microlift::runtime::Translation` states. `Sync` on
// top of `Send` because the lock is inside the cache: threads share one by reference.
unsafe impl Send for CodeCache {}
unsafe impl Sync for CodeCache {}

impl CodeCache {
    /// A cache that lifts with `config`.
    pub fn new(config: LifterConfig) -> Self {
        CodeCache {
            published: RwLock::new(Published {
                cache: microlift::runtime::CodeCache::new(),
                retained: Vec::new(),
                snapshots: HashMap::new(),
            }),
            index: EntryIndex::new(),
            inserts: AtomicU64::new(0),
            counted: std::sync::Mutex::new((0, 0)),
            adopted: std::sync::Mutex::default(),
            config,
        }
    }

    /// How many distinct regions have been translated.
    ///
    /// Microlift's count and not `retained.len()`: that one counts inserts, and two
    /// threads that missed on the same address both compiled it - see
    /// [`Published::retained`]. A caller watching this number grow wants regions, not
    /// races.
    pub fn len(&self) -> usize {
        let inserts = self.inserts.load(Ordering::Acquire);
        let mut counted = self.counted.lock().unwrap_or_else(|e| e.into_inner());
        if counted.0 == inserts && inserts != 0 {
            return counted.1;
        }
        let n = self.read().cache.len();
        *counted = (inserts, n);
        n
    }

    pub fn is_empty(&self) -> bool {
        self.read().cache.is_empty()
    }

    /// What the shared cache has been asked to do, and how much of it block linking
    /// spared it. See [`microlift::runtime::CacheStats`].
    pub fn stats(&self) -> microlift::runtime::CacheStats {
        self.read().cache.stats()
    }

    /// Record `n` successors reached without a host lookup.
    pub fn note_links_followed(&self, n: usize) {
        self.read().cache.note_links_followed(n);
    }

    /// `(installed, pending)` link cells.
    ///
    /// The second number is what separates the two ways a run can report no cells
    /// installed: a lift that emitted none, and cells waiting for a target nothing has
    /// translated yet. Only the first would mean the mechanism is off.
    pub fn cell_counts(&self) -> (usize, usize) {
        self.read().cache.cell_counts()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Published> {
        self.published.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Published> {
        self.published.write().unwrap_or_else(|e| e.into_inner())
    }

    /// The translation that can execute `pc`, lifting one if necessary.
    pub fn get_or_lift<M: Regions>(
        &self,
        program: &Program<M>,
        pc: u64,
    ) -> Result<Arc<Translation>, VmError> {
        if let Some(hit) = self.lookup(pc) {
            return Ok(hit);
        }
        if self.adopt_cached(program, pc)
            && let Some(hit) = self.lookup(pc)
        {
            return Ok(hit);
        }
        crate::aot::profile::note_miss(program, pc);
        let mut attempt = 0;
        loop {
            match self.lift_and_publish(program, pc, attempt == STALE_RETRIES)? {
                Some(translation) => return Ok(translation),
                None => attempt += 1,
            }
        }
    }

    /// The lifter config every region of this cache is lifted under.
    pub(crate) fn config(&self) -> &LifterConfig {
        &self.config
    }

    /// Publish the cached code of the image holding `pc`, the first time one of its
    /// addresses misses. Returns whether anything was published.
    fn adopt_cached<M: Regions>(&self, program: &Program<M>, pc: u64) -> bool {
        let Some(layout) = crate::aot::Layout::current() else {
            return false;
        };
        let Some((path, slide)) = program.code_image(pc) else {
            return false;
        };
        let first = self
            .adopted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.clone());
        if !first {
            return false;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            return false;
        };
        let id = crate::aot::ImageId::of(&bytes);
        crate::aot::profile::remember_id(&path, id);
        // Linked with the program, or brought in by a `dlopen`.
        let matches = match program.report.images.iter().find(|r| r.path == path) {
            Some(report) => crate::aot::matches_file(&program.memory, report, &bytes),
            None => crate::dyld::image_holding(pc).is_some_and(|image| {
                image.report.images.iter().any(|report| {
                    report.path == path
                        && image
                            .with_memory(|memory| crate::aot::matches_file(memory, report, &bytes))
                            .unwrap_or(false)
                })
            }),
        };
        let tracing = crate::shims::vmmap::tracing();
        if !matches {
            if tracing {
                eprintln!("rosemary: aot: {path}: the code loaded is not the code in its file");
            }
            return false;
        }
        let regions = match crate::aot::load(layout, &id, &self.config, slide) {
            Ok(Some(regions)) => regions,
            Ok(None) => {
                if tracing {
                    eprintln!(
                        "rosemary: aot: {path}: nothing cached at {}",
                        layout.image(&id).display()
                    );
                }
                return false;
            }
            Err(e) => {
                eprintln!("rosemary: aot: {path}: the cached code was not used: {e}");
                return false;
            }
        };
        if tracing {
            eprintln!("rosemary: aot: {path}: {} cached region(s)", regions.len());
        }
        let mut published = self.write();
        let any = !regions.is_empty();
        for region in regions {
            let translation = region.translation;
            let start = translation.covered.iter().map(|r| r.start).min();
            let end = translation.covered.iter().map(|r| r.end).max();
            let (Some(start), Some(end)) = (start, end) else {
                continue;
            };
            crate::fault::placed(
                translation.entry as usize as u64,
                translation.link_target as usize as u64,
                start,
                end,
                region.lift_base,
                &region.map,
            );
            if crate::lldb::enabled() {
                let mut guest_bytes = vec![0u8; (end - start) as usize];
                if read_image_code(program, start, &mut guest_bytes) {
                    crate::lldb::register(
                        translation.link_target as usize as u64,
                        start,
                        end,
                        &program.locate(start).unwrap_or_default(),
                        &guest_bytes,
                        &region.map,
                    );
                }
            }
            let translation = published.cache.insert(translation);
            published.retained.push(Arc::clone(&translation));
            self.inserts.fetch_add(1, Ordering::Release);
            self.index
                .publish(&translation.resume_points, Arc::as_ptr(&translation));
        }
        any
    }

    /// One lift of `pc`, published - or `None` when the guest rewrote the bytes it
    /// was made from before it could be, and `last` is not set.
    fn lift_and_publish<M: Regions>(
        &self,
        program: &Program<M>,
        pc: u64,
        last: bool,
    ) -> Result<Option<Arc<Translation>>, VmError> {
        let selecting = crate::stats::enabled().then(std::time::Instant::now);
        // A region always begins at the address it was made for, so its cache
        // key is `pc` and two entry points into one function are two keys.
        let (region, bytes) = program
            .lift_region(pc)
            .ok_or(VmError::NoCode { pc, from: None })?;
        if let Some(selecting) = selecting {
            crate::stats::record_select(selecting.elapsed());
        }
        let start = region.range.start;

        // How long one region took to compile, when tracing. Not curiosity. A
        // big region is not slow in proportion to its size.
        let started = crate::shims::vmmap::tracing().then(std::time::Instant::now);

        // Where control can arrive from outside without any lift knowing: a
        // function reached only through a pointer. See the module note.
        let mut declared = program.function_starts_in(region.range.clone());
        declared.push(pc);
        declared.sort_unstable();
        declared.dedup();

        // A region that will not compile is refused outright.
        let compiled = match compile_region(&self.config, &bytes, &region, &declared) {
            Ok(compiled) => compiled,
            Err(source) => {
                crate::stats::record_refused();
                if crate::shims::vmmap::tracing() {
                    let head: Vec<String> =
                        bytes.iter().take(64).map(|b| format!("{b:02x}")).collect();
                    eprintln!(
                        "rosemary: region {start:#x}..{:#x} would not compile, bytes {}",
                        region.range.end,
                        head.join(" ")
                    );
                }
                return Err(VmError::Lift {
                    pc,
                    start,
                    source,
                    at: program.locate(start),
                });
            }
        };
        let end = region.range.end;

        if let Some(started) = started {
            let took = started.elapsed();
            // Only the ones worth reading. A warm program lifts thousands of
            // regions and a line each would bury the one that matters.
            if took.as_millis() >= 50 {
                eprintln!(
                    "rosemary: compiled {} ({start:#x}..{end:#x}, {} instruction(s), \
                     {} byte(s), {} link cell(s)) in {took:.1?}",
                    program
                        .locate(start)
                        .unwrap_or_else(|| "generated code".to_string()),
                    region.instructions(),
                    end - start,
                    compiled.link_sites.len(),
                );
            }
        }

        crate::fault::placed(
            compiled.region.entry as usize as u64,
            compiled.region.link_target as usize as u64,
            start,
            end,
            pc,
            &compiled.map,
        );
        crate::lldb::register(
            compiled.region.link_target as usize as u64,
            start,
            end,
            &program
                .locate(start)
                .unwrap_or_else(|| "generated code".to_string()),
            &compiled.guest_bytes,
            &compiled.map,
        );

        // Lifted at run-time addresses with a zero slide, so the address a region was
        // lifted for is the address it runs at, and the image base is the region's own start.
        let translation = Translation::from_compiled(
            compiled.region,
            start,
            compiled.resume_points,
            compiled.covered,
            compiled.data_dependencies,
            compiled.link_sites,
            compiled.backend,
        );
        debug_assert!(
            translation.covers(pc),
            "translation {start:#x}..{end:#x} cannot be entered at the {pc:#x} \
             it was lifted for"
        );

        let generated = crate::shims::vmmap::contains(start);

        // Two threads that missed on the same address both compiled it. The loser is
        // displaced from microlift's map but not dropped - see [`Published::retained`].
        let mut published = self.write();
        let snapshot = match generated {
            false => None,
            true => {
                let snapshot = snapshot_of(&translation, start, &bytes);
                // Guarded, then compared: a store since the copy the lift was made
                // from is either seen here or faults and is seen by a drain, which
                // cannot run before this lock is let go.
                crate::smc::guard(&snapshot.spans());
                if !snapshot.changed().is_empty() {
                    crate::stats::record_stale_lift();
                    if !last {
                        published.retained.push(Arc::new(translation));
                        return Ok(None);
                    }
                    crate::smc::requeue(&snapshot.spans());
                }
                Some(snapshot)
            }
        };
        // The one call that matters: as well as making the translation findable, this
        // fills in every link cell whose target is already compiled, and every cell
        // *elsewhere* that has been waiting for one of these resume points.
        let translation = published.cache.insert(translation);
        published.retained.push(Arc::clone(&translation));
        // What makes `len`'s memo stale, bumped under the same lock the insert took.
        self.inserts.fetch_add(1, Ordering::Release);
        self.index
            .publish(&translation.resume_points, Arc::as_ptr(&translation));
        if let Some(snapshot) = snapshot {
            let snapshot = Arc::new(snapshot);
            let mut pages: Vec<u64> = snapshot.pages().collect();
            pages.dedup();
            for page in pages {
                published
                    .snapshots
                    .entry(page)
                    .or_default()
                    .push((Arc::clone(&translation), Arc::clone(&snapshot)));
            }
        }
        Ok(Some(translation))
    }

    /// Retire every translation whose guest bytes a write has changed since it was
    /// lifted. Called by the run loop before it looks anything up, whenever
    /// [`crate::smc::dirty`] says a guarded page was written.
    ///
    /// Retired translations are unpublished here and in [`EntryIndex`]; each thread
    /// drops its own rows when it replays the log [`crate::smc::retire`] appends to.
    /// None is freed - see [`Published::retained`].
    pub fn drain_writes(&self) {
        let mut published = self.write();
        let published = &mut *published;
        let mut retired = Vec::new();
        for page in crate::smc::take_pending() {
            let start = page.start();
            let Some(snaps) = published.snapshots.get_mut(&start) else {
                continue;
            };
            page.guard();
            let changed: Vec<(u64, u64)> = snaps.iter().flat_map(|(_, s)| s.changed()).collect();
            for (at, len) in changed {
                for pc in published.cache.invalidate_range(at, len) {
                    self.index.retract(pc);
                    retired.push(pc);
                }
            }
            // A snapshot is kept while its translation still answers for the address
            // it was published under.
            let cache = &published.cache;
            snaps.retain(|(translation, s)| {
                cache
                    .peek(s.entry)
                    .is_some_and(|t| Arc::ptr_eq(t, translation))
            });
            if snaps.is_empty() {
                published.snapshots.remove(&start);
            }
        }
        if !retired.is_empty() {
            crate::stats::record_retired(retired.len());
            crate::smc::retire(&retired);
        }
    }

    /// An existing translation covering `pc`, if there is one.
    ///
    /// Public because the run loop asks it before it asks whether `pc` is an
    /// import: a translation existing at an address is proof the address is guest
    /// code, and the import lookups are the more expensive question. See
    /// `Vm::run_loop`.
    pub(crate) fn lookup(&self, pc: u64) -> Option<Arc<Translation>> {
        // Not before: a translation that is still in the index here may be one a
        // write has already made wrong.
        if crate::smc::dirty() {
            self.drain_writes();
        }
        if let Some(hit) = self.index.get(pc) {
            // SAFETY: the pointer was taken from an `Arc` this cache put in
            // `retained`, which never gives one up. So the strong count is at least
            // one for as long as `&self` is borrowed, which is what makes an
            // increment off a bare pointer legal here and nowhere else.
            return Some(unsafe {
                Arc::increment_strong_count(hit);
                Arc::from_raw(hit)
            });
        }
        // The one race the index cannot cover: a probe that overtook another thread's
        // publish. Counted by microlift as a host dispatch, which it is - the index
        // hits above are the ones that never became one.
        self.read().cache.lookup(pc)
    }
}

/// What a translation of generated code was made from: the region's own bytes, as
/// lifted, and any data it read outside them, as they are now.
fn snapshot_of(translation: &Translation, start: u64, bytes: &[u8]) -> crate::smc::Snapshot {
    let end = start + bytes.len() as u64;
    let mut ranges: Vec<(u64, Box<[u8]>)> = vec![(start, bytes.into())];
    for dep in &translation.data_dependencies {
        if dep.start >= start && dep.end <= end {
            continue;
        }
        if !crate::smc::tracked(dep.start, dep.end - dep.start) {
            continue;
        }
        // FIXME (ACCURACY): read after the lift rather than by it, so a store to a
        // jump table between the two is missed. Every arena this has met keeps its
        // tables inside the region.
        // SAFETY: tracked arena memory, which is never unmapped.
        let live = unsafe {
            std::slice::from_raw_parts(
                dep.start as usize as *const u8,
                (dep.end - dep.start) as usize,
            )
        };
        ranges.push((dep.start, live.into()));
    }
    crate::smc::Snapshot {
        entry: start,
        ranges,
    }
}

/// Lift and compile `region`'s `bytes` into native code.
fn compile_region(
    config: &microlift::core::LifterConfig,
    bytes: &[u8],
    region: &crate::region::Region,
    declared: &[u64],
) -> microlift::Result<Compiled> {
    let clock = crate::stats::enabled().then(std::time::Instant::now);
    crate::stats::record_extent(region.range.start, region.range.end);

    // Cheap: the two fields with a program's worth of data in them are behind
    // `Arc`s, so this copies pointers. It did not always, and that clone was 7%
    // of a process's wall clock.
    let mut config = config.clone();
    // Declared per region rather than once, because the answer is per region.
    config.resume_points = declared.to_vec();
    // Which of these bytes are code.
    config.block_ranges = region.blocks.clone();

    let lifted = lift_to_code::<X86_64>(bytes, region.range.start, config)?;
    // Cloned out before the lift is consumed: these four are what the shared cache
    // needs to publish, invalidate and link this translation, and
    // `compile_lifted_region` takes the `LiftedCode` by value. `Translation::compile`
    // does exactly this and is not used here only because it discards the source map
    // that [`crate::fault`] and [`crate::lldb`] are handed below.
    let resume_points = lifted.resume_points.clone();
    let covered = lifted.covered.clone();
    let data_dependencies = lifted.data_dependencies.clone();
    let link_sites = lifted.link_sites.clone();
    let lift_took = clock.map(|c| c.elapsed());
    let mut backend = crate::jit::backend()?;
    let (compiled_region, map) = backend.compile_lifted_region(lifted)?;
    if let (Some(clock), Some(lift)) = (clock, lift_took) {
        crate::stats::record_lift(
            region.instructions(),
            bytes.len(),
            lift,
            clock.elapsed() - lift,
        );
    }
    Ok(Compiled {
        backend,
        region: compiled_region,
        map,
        guest_bytes: crate::lldb::enabled()
            .then(|| bytes.to_vec())
            .unwrap_or_default(),
        resume_points,
        covered,
        data_dependencies,
        link_sites,
    })
}

/// One region through the lifter and the compiler, with everything about it that
/// only the lift knows.
struct Compiled {
    backend: microlift::backend::CraneliftBackend,
    /// The two entry points and this region's link-cell array.
    region: microlift::backend::CompiledRegion,
    /// Host offset -> guest instruction, so a fault inside this code can name
    /// the instruction rather than the region. See [`crate::fault`].
    map: microlift::core::srcloc::GuestCodeMap,
    /// Guest bytes retained only so the opt-in LLDB listing can show x86-64.
    guest_bytes: Vec<u8>,
    /// Where the compiled code may be entered, ascending - and so the exact set of
    /// addresses it may be published in a link table under.
    resume_points: Vec<u64>,
    /// The guest bytes this region was derived from: the invalidation key.
    covered: Vec<std::ops::Range<u64>>,
    /// Guest bytes outside `covered` that the lift read and baked in - a jump
    /// table, chiefly.
    data_dependencies: Vec<std::ops::Range<u64>>,
    /// The lift-time-known destinations this region has a cell for, in cell order.
    link_sites: Vec<microlift::core::link::LinkSite>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table never dereferences what it stores, so a test may store anything.
    fn marker(n: u64) -> *const Translation {
        n as usize as *const Translation
    }

    #[test]
    fn probes_find_every_published_address() {
        let index = EntryIndex::new();
        // Page-aligned and closely spaced, the two shapes real entry points take.
        let addresses: Vec<u64> = (0..4096u64)
            .map(|n| 0x1_0000_0000 + n * 0x1000)
            .chain((0..4096u64).map(|n| 0x2_0000_0000 + n * 16))
            .collect();
        for (n, &pc) in addresses.iter().enumerate() {
            index.publish(&[pc], marker(n as u64 + 1));
        }
        for (n, &pc) in addresses.iter().enumerate() {
            assert_eq!(index.get(pc), Some(marker(n as u64 + 1)), "{pc:#x}");
        }
        assert_eq!(index.get(0x3_0000_0000), None);
        assert_eq!(index.get(0x1_0000_0000 + 8), None);
    }

    #[test]
    fn a_relift_replaces_the_address_rather_than_filling_a_second_slot() {
        let index = EntryIndex::new();
        index.publish(&[0x1000, 0x2000], marker(1));
        index.publish(&[0x1000], marker(2));
        assert_eq!(index.get(0x1000), Some(marker(2)));
        assert_eq!(index.get(0x2000), Some(marker(1)));
        assert_eq!(index.live.load(Ordering::Relaxed), 2);
    }
}
