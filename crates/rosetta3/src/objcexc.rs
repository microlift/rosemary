use std::ffi::{CString, c_char};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use static_init::dynamic;

use machish::error::Result;
use machish::exports::ExportTable;
use machish::report::LinkReport;

/// `__cxxabiv1::__si_class_type_info`, which is `{ vtable, name, base }`.
///
/// A root class uses the same three words with `base` zero and the *no-bases*
/// vtable, which reads only the first two - so one layout serves both.
///
/// # This is the **Itanium** record, and only the Itanium one reads it
///
/// There are two exception ABIs in play in this tree's guests and they are
/// **mutually exclusive**, and the measurement that separates them is in
/// [`objc_record_for`]'s note. In short: this shape is what
/// `__gxx_personality_v0` - the vendored `libc++abi`'s, reached through
/// `__cxa_throw` by [`crate::objc::dispatch`]'s `_objc_exception_throw` - reads
/// and compares by walking the base chain. It is correct for that caller.
#[repr(C)]
struct TypeInfo {
    /// Set by [`bind`], because it is an address in the vendored `libc++abi`
    /// and that image has not been placed when the record is made.
    vptr: u64,
    name: *const c_char,
    base: u64,
}

// SAFETY: `name` points at a leaked `CString` and is never written after the
// record is made; `vptr` is written once, before any guest code runs.
unsafe impl Send for TypeInfo {}

/// `struct objc_typeinfo`, which is what an `_OBJC_EHTYPE_$_X` record is.
///
/// Apple's own definition, from `runtime/objc-exception.mm` in the
/// `opensource-apple/objc4` tree:
///
/// ```c
/// struct objc_typeinfo {
///     // Position of vtable and name fields must match C++ typeinfo object
///     const void **vtable;  // always objc_ehtype_vtable+2
///     const char *name;     // c++ typeinfo string
///     Class cls_unremapped;
/// };
/// ```
///
/// Three words, like [`TypeInfo`], and the first two mean the same thing in
/// both. **The third does not**, and that is the whole of the difference:
///
/// | word | Itanium ([`TypeInfo`]) | Objective-C (`objc_typeinfo`) |
/// |--:|---|---|
/// | 0 | a `__cxxabiv1::__si_class_type_info` vtable slot | `objc_ehtype_vtable + 2` |
/// | 1 | the mangled type name | the class name |
/// | 2 | a `__class_type_info *`, the **base** | a `Class *`, the class itself |
///
/// So a record built one way and read the other does not fault; it *compares
/// unrelated words*. libc++abi's `can_catch` would call through word 0, which is
/// the wrong function; libobjc's `_objc_exception_do_catch` would hand word 2 to
/// `objc_remapClass` as if it were a `Class *`.
#[repr(C)]
struct ObjcTypeInfo {
    vtable: u64,
    name: *const c_char,
    class: u64,
}

// SAFETY: as [`TypeInfo`].
unsafe impl Send for ObjcTypeInfo {}

/// Every record made so far, by class name, so that two images asking for
/// `_OBJC_EHTYPE_$_NSException` get the same address - which is what makes
/// libc++abi's pointer comparison of two `type_info`s correct.
///
/// A `Vec` and a linear scan, which is O(classes thrown through) per lookup.
/// That is a cost, not a correctness problem, and it is not the reason this is
/// worth stating: the reason is that the map's *invariant* is one address per
/// name, and [`record_for`] below is the only writer.
static RECORDS: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

/// The two `type_info` vtables, from the vendored `libc++abi`, and the
/// personality routine that goes with them.
///
/// Statics rather than arguments because a record is also made on the *throw*
/// path, long after [`bind`] has run and with nothing there to pass them.
static NO_BASES: AtomicU64 = AtomicU64::new(0);
static ONE_BASE: AtomicU64 = AtomicU64::new(0);

/// `objc_ehtype_vtable + 2`: the function table an `objc_typeinfo` record's
/// first word must hold.
///
/// **Zero, because nothing in this tree can put a real address there**, and that
/// is the measurement, not an omission:
///
/// * the word is `_objc_ehtype_vtable`, an `OBJC_EXPORT const void *[]` defined
///   by libobjc. Scanned across the whole bundle - every `__stubs` and every
///   `__la_symbol_ptr` entry of all 15 Mach-Os, resolved through the indirect
///   symbol table - `_objc_ehtype_vtable` is **imported by none of them and
///   defined by none of them**. The Framework's whole indirect symbol table is
///   4,029 entries and the only related names in it are
///   `_OBJC_EHTYPE_$_NSException` (index 757), `_OBJC_EHTYPE_id` (indices 1736
///   and 3315) and `___objc_personality_v0` (index 665);
/// * the only other way to reach it is `dlsym`, and there is no libobjc on this
///   host;
/// * and a zero is not a usable substitute. `_objc_exception_do_catch` begins
///   `if (throw_tinfo->vtable != objc_ehtype_vtable + 2) return false;`, so a
///   record with a null - or a libc++abi - vtable word is one that **no
///   Objective-C exception ever matches**, and installing one would be a catch
///   that silently never fires.
///
/// So the Objective-C records are withheld rather than fabricated, and this
/// static is where a real libobjc would publish the address. See
/// [`objc_record_for`].
static OBJC_EHTYPE_VTABLE: AtomicU64 = AtomicU64::new(0);

/// Publish `objc_ehtype_vtable + 2` for [`objc_record_for`] to build records
/// with.
///
/// The wiring point, and it has no caller: the address does not exist on this
/// host. It is here so that the *shape* of the change is visible - one function
/// to call when a libobjc image is mapped - rather than spread through the
/// binder.
pub fn publish_objc_ehtype_vtable(address: u64) {
    OBJC_EHTYPE_VTABLE.store(address, Ordering::Release);
}

/// The `type_info` for `class`, making it and its superclasses' if needed.
///
/// `None` before [`bind`] has found the vtables, and for a class this runtime
/// does not have: a record whose base chain stops short would answer "not a
/// subclass" for a class that is one, which is a wrong catch rather than a
/// missing one.
///
/// # One address per class name, which takes two things to get right
///
/// The address is the *identity* a personality routine compares two catch types
/// by, so two records for one class name is not a leak and not a waste: it makes
/// every `@catch` for that class match one of the two and miss the other, and
/// which one is decided by which thread threw first. Two things used to make
/// that reachable, and the first is still true:
///
/// * **The search and the insert were not one critical section.** It was lock,
///   search, unlock, build, lock, push. [`bind`] is single-threaded, so at load
///   time nothing raced - but the *throw* path is not single-threaded, and
///   `exception_throw`'s shim calls this on every throw, from whatever guest
///   thread made it. Two threads throwing the same class could both miss the
///   search and both push.
/// * **Holding the lock across the whole build would deadlock**, because making
///   a record recurses into its superclass and this is a `Mutex`, not a
///   reentrant one.
///
/// So the build happens unlocked and the *install* is the critical section: the
/// first thread to get in wins, and a loser takes the winner's address rather
/// than its own. The loser's record is leaked and never handed out, which is
/// 24 bytes per racing throw and is the price of not needing a reentrant lock.
pub fn record_for(class: &'static objc::class::Class) -> Option<u64> {
    let name = class.name_str().to_string();
    if let Some(addr) = interned(&name) {
        return Some(addr);
    }
    // The gate sits here on purpose: it is the *only* place where the two
    // critical sections of the old lock/search/unlock/push were separated, and a
    // test has to be able to put a thread in exactly that gap to say anything
    // about it. See [`gated`].
    gated();
    let base = match class.superclass() {
        Some(parent) => record_for(parent)?,
        None => 0,
    };
    let vptr = match base {
        0 => NO_BASES.load(Ordering::Acquire),
        _ => ONE_BASE.load(Ordering::Acquire),
    };
    if vptr == 0 {
        return None;
    }
    let text = CString::new(name.as_str()).ok()?;
    let record = Box::leak(Box::new(TypeInfo {
        vptr,
        name: Box::leak(text.into_boxed_c_str()).as_ptr(),
        base,
    }));
    let mine = record as *const TypeInfo as usize as u64;

    // Check and insert as one step. Nothing can have put *this* name here between
    // the search above and now except another thread, so an entry found here is
    // a racing twin's - and that twin's address is the answer, because a
    // personality routine compares two catch types by address and a loser that
    // kept its own would hand out a second identity for one class.
    let mut records = RECORDS.lock().unwrap_or_else(|e| e.into_inner());
    match records.iter().find(|(n, _)| *n == name) {
        Some((_, theirs)) => Some(*theirs),
        None => {
            records.push((name, mine));
            Some(mine)
        }
    }
}

/// Every Objective-C record made so far, by class name.
///
/// A **second** table from [`RECORDS`], and that is the fix rather than a
/// duplication. The two record kinds are the same size and share their first two
/// words, so one table keyed by class name would hand the same address to both
/// kinds of reader - and the two readers disagree about what the third word
/// means. See [`ObjcTypeInfo`].
static OBJC_RECORDS: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

/// The `objc_typeinfo` record for `class`, in the shape libobjc's
/// `__objc_personality_v0` reads.
///
/// **Always `None` today, and the reason is a measurement rather than a gap in
/// the code.** Three things have to be true before this can answer, and none of
/// them is:
///
/// 1. **A real `objc_ehtype_vtable`.** The record's first word is
///    `objc_ehtype_vtable + 2`, a symbol from libobjc. It is imported by **0 of
///    the 15** Mach-Os in the bundle and defined by none - see
///    [`OBJC_EHTYPE_VTABLE`].
/// 2. **A `Class *` the guest can use.** Word 2 is the class the `@catch` names,
///    as libobjc would hand it to `objc_remapClass`. This runtime's
///    `objc::class::Class` is not a guest-visible `Class *`, and manufacturing a
///    pointer to one would be a pointer into nothing.
/// 3. **A personality to read it.** `__unwind_info` in the Framework names
///    `___objc_personality_v0` for the **53** frames that carry Objective-C
///    `@catch` clauses - 15 of them in one run, `0x47db8d0`..`0x47dd010` - and
///    that is libobjc's routine, which this tree does not have. A record nothing
///    reads is not a small answer; it is a record whose first use would be
///    unverified.
///
/// # And a third reason, which is the one that is cheapest to fix
///
/// **These records cannot be built at load time at all, whoever the vtable is.**
/// `bind` runs inside [`crate::Program::load`] - `program.rs:729` - and
/// `objc::register` runs later, from `exec/native.rs:363` and `vm.rs:561`. So
/// when `bind` asked `objc::class::lookup(c"NSException")` it was asking a
/// registry that had never seen a guest class, and the answer was `None` for
/// every name. That is the whole reason the 15 clauses read **eight zero bytes**
/// from their slots, and it is why this is a *timing* defect wearing a
/// *capability* defect's clothes.
///
/// The right place to answer these names is [`repair_image_records`], which
/// already runs after registration - `objc/mod.rs:230` and `:244` - and which
/// therefore has both the class and the slot in hand. `bind` cannot, and asking
/// it to is what made the answer look like it was working.
///
/// # Why `_OBJC_EHTYPE_id` is not answerable even with all three
///
/// Because libobjc matches it by **address identity**, not by content:
///
/// ```c
/// // `catch (id)` always catches objc types.
/// if (catch_tinfo == &OBJC_EHTYPE_id) { return true; }
/// ```
///
/// The one address that satisfies that is libobjc's own `OBJC_EHTYPE_id` symbol.
/// Any address this tree could publish would be a different address, so every
/// `@catch (id)` would miss - and `id` is **14 of the 15** clauses in the
/// Framework. There is no synthesisable answer to this one; it needs the symbol.
///
/// # What the *Itanium* record is for, so the two are not confused again
///
/// [`record_for`], which is what [`crate::objc::dispatch`]'s
/// `_objc_exception_throw` uses, is the right shape for its caller: it hands the
/// record to `__cxa_throw`, so the reader is the vendored `libc++abi`'s
/// `__gxx_personality_v0`, which is an Itanium personality and matches by
/// walking the base chain. The two callers want different records, and one
/// function serving both is how a record ends up in a shape no reader can use.
pub fn objc_record_for(class: &'static objc::class::Class) -> Option<u64> {
    let vtable = OBJC_EHTYPE_VTABLE.load(Ordering::Acquire);
    if vtable == 0 {
        return None;
    }
    // The class as libobjc would want it: a `Class *` in guest memory. Nothing
    // produces one yet, and that is the second of the three reasons above.
    let class_ptr = 0u64;
    if class_ptr == 0 {
        return None;
    }
    let name = class.name_str().to_string();
    if let Some(addr) = objc_interned(&name) {
        return Some(addr);
    }
    let text = CString::new(name.as_str()).ok()?;
    let record = Box::leak(Box::new(ObjcTypeInfo {
        vtable,
        name: Box::leak(text.into_boxed_c_str()).as_ptr(),
        class: class_ptr,
    }));
    let mine = record as *const ObjcTypeInfo as usize as u64;

    // The same check-and-install as [`record_for`], for the same reason: one
    // address per class name, so that a reader comparing two records by identity
    // sees one object. libobjc's `catch_tinfo == &OBJC_EHTYPE_id` is exactly
    // such a comparison.
    let mut records = OBJC_RECORDS.lock().unwrap_or_else(|e| e.into_inner());
    match records.iter().find(|(n, _)| *n == name) {
        Some((_, theirs)) => Some(*theirs),
        None => {
            records.push((name, mine));
            Some(mine)
        }
    }
}

/// The read half of [`objc_record_for`]'s check-and-insert.
fn objc_interned(name: &str) -> Option<u64> {
    OBJC_RECORDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, addr)| *addr)
}

/// The address already interned for `name`, if any. The read half of
/// [`record_for`]'s check-and-insert, factored out so the fast path is one
/// obvious thing.
fn interned(name: &str) -> Option<u64> {
    RECORDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, addr)| *addr)
}

/// A gate that holds a thread between [`record_for`]'s search and its install,
/// so a test can *arrange* the interleaving rather than hope for it.
///
/// Test-only, and it exists because the obvious test for
/// [`tests::concurrent_throws_of_one_class_intern_one_address`] is not a test.
/// N threads released by a barrier do not reproduce that window reliably,
/// because the window is a handful of instructions between two critical
/// sections and the first thread through usually closes it. That was measured,
/// not assumed: the barrier version passed ten times out of ten **against the
/// code it was written to catch**. A gate is the difference between a test and a
/// decoration.
///
/// `GATE_TARGET` threads park in [`gated`], each arrival counts, and nobody
/// leaves until that many have arrived.
#[cfg(test)]
static GATE_TARGET: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many have arrived. Never reset; the test zeroes it when it arms.
#[cfg(test)]
static GATE_ARRIVED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Park at the gate if a test armed it, and disarm it once the rendezvous the
/// test wanted has happened.
///
/// The `not(test)` half is the whole point of the `cfg`: production code pays
/// one relaxed load of a `static` that is always zero, and there is no gate in a
/// release build to be surprised by.
#[cfg(not(test))]
#[inline]
fn gated() {}

#[cfg(test)]
fn gated() {
    use std::sync::atomic::Ordering as O;
    let target = GATE_TARGET.load(O::Acquire);
    if target == 0 {
        return;
    }
    if GATE_ARRIVED.fetch_add(1, O::AcqRel) + 1 >= target {
        GATE_TARGET.store(0, O::Release);
        return;
    }
    // Microseconds wide in practice, and a spin that gives up reports rather
    // than hangs the suite.
    for _ in 0..2_000_000 {
        if GATE_TARGET.load(O::Acquire) == 0 {
            return;
        }
        std::hint::spin_loop();
    }
    panic!(
        "the test gate never filled: {} of {target} arrived",
        GATE_ARRIVED.load(O::Acquire)
    );
}

/// Rewrite the `objc_typeinfo` records the *guest* laid out for its own classes.
///
/// clang emits `_OBJC_EHTYPE_$_Foo` into the image that implements `Foo`, as
/// `{ objc_ehtype_vtable + 2, "Foo", OBJC_CLASS_$_Foo }`.
///
/// **This no longer rewrites them into the Itanium shape, and that is the fix
/// rather than a removal.** It used to, and the rewrite was wrong for the reader
/// that exists: an in-image record is an [`ObjcTypeInfo`], and the consumer of
/// the frames that name it is `__objc_personality_v0` - libobjc's, per
/// [`objc_record_for`]'s measurement of the 53 `__unwind_info` entries that
/// name it. Rewriting word 0 to libc++abi's `__si_class_type_info` vtable and
/// word 2 to a *superclass's* record produced a record on which
/// `_objc_exception_do_catch` would hand a `type_info *` to
/// `objc_remapClass`, and on which nothing could ever match.
///
/// What is left for it to do, once [`OBJC_EHTYPE_VTABLE`] is real, is word 0 -
/// and for the Framework there is nothing to do at all, because it imports
/// `_objc_ehtype_vtable` from nothing and defines no such record itself.
///
/// Separate from [`bind`] because it needs the guest's classes to be
/// *registered*, and that happens long after linking - see
/// [`crate::objc::register`].
pub fn repair_image_records(mem: &mut dyn crate::abi::GuestMemory, report: &LinkReport) {
    let vtable = OBJC_EHTYPE_VTABLE.load(Ordering::Acquire);
    if vtable == 0 {
        return;
    }
    for image in &report.images {
        // The *records*, not the vtable symbol's slot: the symbol is what a
        // record's first word points two entries into, so an image that imports
        // it is an image whose records this could fill in. Finding none is the
        // ordinary case and is why the Framework needs nothing here.
        let records = image
            .missing
            .iter()
            .filter(|m| m.symbol.starts_with("_OBJC_EHTYPE_$_"))
            .map(|m| m.slot_va)
            .chain(
                image
                    .bindings
                    .iter()
                    .filter(|b| b.symbol.starts_with("_OBJC_EHTYPE_$_"))
                    .map(|b| b.slot_va),
            );
        for record in records {
            // The name in the middle already says which class it is, which is
            // what makes this possible without a symbol table. Word 2 is left
            // alone on purpose: it is the class the record is *for*, not its
            // superclass, and the linker already put the right `Class *` there.
            if class_named_in(mem, record).is_none() {
                continue;
            }
            let _ = mem.write(record, &vtable.to_le_bytes());
        }
    }
}

/// The class an in-image `type_info` names, read out of its `name` word.
fn class_named_in(
    mem: &dyn crate::abi::GuestMemory,
    record: u64,
) -> Option<&'static objc::class::Class> {
    /// A class name past this is not one, and the read has to stop somewhere.
    const MAX_NAME: usize = 256;
    let mut word = [0u8; 8];
    mem.read(record.wrapping_add(8), &mut word).ok()?;
    let at = u64::from_le_bytes(word);
    let mut name = Vec::new();
    let mut byte = [0u8; 1];
    for i in 0..MAX_NAME {
        mem.read(at.wrapping_add(i as u64), &mut byte).ok()?;
        if byte[0] == 0 {
            break;
        }
        name.push(byte[0]);
    }
    objc::class::lookup(&CString::new(name).ok()?)
}

/// The 23 names a Mach-O image's exception machinery is made of, as the image
/// spells them.
///
/// Closed by walking the bind and lazy-bind opcodes of `Chromium Framework`
/// rather than by reading a header, and the count is 23 rather than the 24 the
/// brief carries because the two names the rest of this file is built around -
/// `_objc_exception_throw` and `_objc_exception_rethrow` - **are not imported by
/// that image at all**. It catches Objective-C exceptions; it throws none through
/// libobjc. That is a property of the image, not of this list, and it is the
/// reason the census is worth having: an inventory built from what this tree
/// expects to serve would be a list of names the guest never asks for.
///
/// Split by what each group is *for*, because they are three different problems
/// and reading them as one is what made the personality look like a liberty
/// rather than a refusal:
///
/// | group | names | who wants it |
/// |---|--:|---|
/// | exception-type records | 2 | a personality routine, matching a `@catch` |
/// | the personality routine | 1 | the unwinder, named by `__TEXT,__unwind_info` |
/// | `objc_begin_catch` / `objc_end_catch` | 2 | a landing pad, 16 and 17 call sites |
/// | `_Unwind_*` | 12 | 1 of them (`Resume`, 3581 sites) is a landing pad; the other 11 are Chromium's own `base/debug` in-process unwinder |
/// | `unw_*` | 5 | the same unwinder, its libunwind `ll.h` half - 1 site each |
/// | `__cxa_atexit` | 1 | static destructors, 251 sites |
const EH_NAMES: &[&str] = &[
    // 2 exception-type records. Both are *imports* from libobjc.A.dylib; the
    // Framework's symbol table has no `_OBJC_EHTYPE_$_X` definition of its own.
    "_OBJC_EHTYPE_$_NSException",
    "_OBJC_EHTYPE_id",
    // 1 personality routine, and the only reference to it in the image is the
    // four-byte word at `0xdc1ae74` in `__TEXT,__unwind_info`.
    "___objc_personality_v0",
    // 2 libobjc catch entry points, 16 and 17 call sites.
    "_objc_begin_catch",
    "_objc_end_catch",
    // 12 `_Unwind_*`.
    "__Unwind_RaiseException",
    "__Unwind_Resume",
    "__Unwind_DeleteException",
    "__Unwind_GetDataRelBase",
    "__Unwind_GetTextRelBase",
    "__Unwind_GetIP",
    "__Unwind_GetIPInfo",
    "__Unwind_GetLanguageSpecificData",
    "__Unwind_GetRegionStart",
    "__Unwind_SetGR",
    "__Unwind_SetIP",
    "__Unwind_Backtrace",
    // 5 libunwind `ll.h`.
    "_unw_getcontext",
    "_unw_init_local",
    "_unw_step",
    "_unw_get_proc_info",
    "_unw_get_proc_name",
    // 1 static-destructor registrar.
    "___cxa_atexit",
];

/// `ROSETTA3_EH_CENSUS`: print the whole exception machinery, and where each
/// name ended up.
///
/// Off by default, and additive: with it unset this function reads one
/// `var_os` and returns. It exists because **the unbound-import census cannot
/// see any of this** - every one of these names is *bound*, so the count it
/// reports is unaffected by whether the answer is right, and `___objc_personality_v0`
/// sat on eight zero bytes in a read-only page while being reported bound. A
/// census of *bindings* is the only instrument that can see this class of defect,
/// and this is that instrument, over the 23 names the guest actually asks for.
fn census_wanted() -> bool {
    #[dynamic]
    static ON: bool = std::env::var_os("ROSETTA3_EH_CENSUS").is_some();
    *ON
}

/// Print [`EH_NAMES`] and what each one is bound to, under
/// `ROSETTA3_EH_CENSUS`.
///
/// A name is reported with **where the slot points**, not merely that it is
/// bound, because the whole failure mode here is a name that is bound to
/// something that is not an answer. The classification is deliberately coarse
/// and every branch of it is checkable:
///
/// * a `darwin::data_unavailable` cell, named by shape - the zero-refusal, and
///   now a distinct cell for the exception-type records;
/// * **this tree**, by `darwin::is_implemented`;
/// * **a guest image**, by segment containment - which is how a vendored
///   `libc++abi` or `libunwind` answers are told apart from the host's;
/// * **the host**, by `dlsym` equality on the reduced name, which is the test
///   `ROSETTA3_TRACE_PASSTHROUGH` already uses and is what distinguishes "fell
///   through to `libgcc_s`" from "bound here";
/// * and **nothing recognisable**, which is the case worth a second look.
fn announce_census(report: &LinkReport) {
    if !census_wanted() {
        return;
    }
    let guest: Vec<(&str, u64, u64)> = report
        .images
        .iter()
        .flat_map(|i| {
            i.segments
                .iter()
                .map(move |s| (i.name.as_str(), s.vmaddr, s.vmaddr.wrapping_add(s.vmsize)))
        })
        .collect();
    let in_guest = |addr: u64| {
        guest
            .iter()
            .find(|(_, lo, hi)| addr >= *lo && addr < *hi)
            .map(|(name, _, _)| *name)
    };

    let mut lines = Vec::new();
    for image in &report.images {
        let short = image.name.rsplit('/').next().unwrap_or(&image.name);
        for name in EH_NAMES {
            let bound = image.bindings.iter().find(|b| b.symbol == *name);
            let missing = image.missing.iter().any(|m| m.symbol == *name);
            let Some(binding) = bound else {
                if missing {
                    lines.push(format!("  {name:<34} {short}: UNBOUND"));
                }
                continue;
            };
            let cell = darwin::data_unavailable::which_cell(binding.resolved);
            let where_ = if cell.is_some() {
                format!("a zeroed refusal cell ({cell:?})")
            } else if let Some(image) = in_guest(binding.resolved) {
                format!(
                    "a guest image: {}",
                    image.rsplit('/').next().unwrap_or(image)
                )
                // `is_implemented` **reduces the name itself** -
                // `darwin::is_implemented` is `implemented(base_name(symbol))` -
                // so it must be handed the Mach-O spelling. Reducing here as
                // well took one underscore off twice, and `___objc_personality_v0`
                // became `_objc_personality_v0`, which matches no row in any of
                // the three tables. That is why this census printed
                // `NOTHING RECOGNISABLE` for a name that
                // `darwin::data_unavailable` publishes. The rule is stated at
                // `darwin::last_unavailable` and this was the one call site of
                // the four that this crate owns.
            } else if darwin::is_implemented(name) {
                "this tree".to_string()
            } else if crate::host::symbol(darwin::base_name(name)) == Some(binding.resolved) {
                "THE HOST, unaltered".to_string()
            } else {
                "NOTHING RECOGNISABLE".to_string()
            };
            lines.push(format!(
                "  {name:<34} {short}: slot {:#x} -> {:#x}  {where_}",
                binding.slot_va, binding.resolved
            ));
        }
    }
    eprintln!(
        "rosetta3: eh census: {} name(s) x {} image(s)",
        EH_NAMES.len(),
        report.images.len()
    );
    for line in lines {
        eprintln!("rosetta3:{line}");
    }
    let missing = report
        .images
        .iter()
        .flat_map(|i| &i.missing)
        .filter(|m| EH_NAMES.contains(&m.symbol.as_str()))
        .count();
    if missing == 0 {
        eprintln!(
            "rosetta3: eh census: none of the {} is unbound. That is NOT the same as answered: \
             read the `->` column.",
            EH_NAMES.len()
        );
    }
}

/// Point every `_OBJC_EHTYPE_$_` binding at a record.
///
/// Runs after [`crate::cxxlib::bind_cxxabi_to_the_vendored_one`], because the
/// vtable addresses it needs are that image's.
///
/// # `___objc_personality_v0` is deliberately **not** pointed at libc++abi's
///
/// It used to be, and it was wrong in a way no test could see. The two
/// personality routines are **two different functions in two different
/// libraries, speaking two different exception ABIs**:
///
/// * libobjc's `__objc_personality_v0` matches a catch clause against the
///   one-word `objc_exception_type` records this file's `TypeInfo` is shaped
///   after - `{ vptr, name }` and no base word, because `@catch` in an
///   Objective-C++ frame is matched by walking the class chain rather than by
///   the Itanium `__si_class_type_info` base pointer;
/// * libc++abi's `__gxx_personality_v0` matches `__class_type_info` and
///   `__si_class_type_info` records, and reads a `base` word at offset 16 out of
///   the record. On an `objc_exception_type` record that word is whatever
///   follows the name, so every type comparison is a comparison of unrelated
///   bytes, and the outcome is a catch that matches or misses on data that has
///   nothing to do with the classes involved.
///
/// The ABI mismatch is invisible from the emulated side, because the name is
/// intercepted by symbol before its address is read. It is equally invisible
/// for `Chromium Framework`, which imports `___objc_personality_v0` but carries
/// its **own statically linked** routine at `Framework+0x417640` - it links no
/// libc++abi, so [`bind`] returns early and the rebinding never ran. It is
/// latent: any program that does link `/usr/lib/libc++abi.dylib` gets it, and
/// gets a personality routine that will not match its own catches.
///
/// The truthful answer for the name is libobjc's routine, which this tree does
/// not have, so the name keeps whatever refuses it by name -
/// `darwin::data_unavailable::announce_and_abort` - and says so when reached.
/// A refusal naming a missing library beats a wrong routine silently matching
/// the wrong records. See also the comment on the `__objc_personality_v0` row in
/// `crate::abi::table`.
pub fn bind<M: machish::memory::Regions>(
    report: &mut machish::report::LinkReport,
    memory: &mut M,
    exports: &ExportTable,
    is_libcxxabi: &dyn Fn(&str) -> bool,
) -> Result<usize> {
    // Before every early return below, because the two questions this function is
    // asked most often are "did it do anything" and "what is that name bound to",
    // and both of them are answered by running with no C++ runtime in the program.
    announce_census(report);

    // Under the host's libobjc every one of these symbols is Apple's, and its
    // `type_info` records describe Apple's classes.
    if !crate::frameworks::using_builtin() {
        return Ok(0);
    }
    let Some(slide) = report
        .images
        .iter()
        .find(|i| is_libcxxabi(&i.name))
        .map(|i| i.slide)
    else {
        // No C++ runtime in the graph, so no `type_info` vtables and nothing for a
        // record to be compared against.
        //
        // **This is not `Chromium Framework`'s state, and a reader who assumed it
        // was would draw the wrong conclusion about everything above.** The
        // Framework's `LC_LOAD_DYLIB` list has no `libc++abi` and no `libunwind` -
        // it names `/usr/lib/libobjc.A.dylib`, `/usr/lib/libstdc++.6.dylib` and
        // `/usr/lib/libSystem.B.dylib` - but
        // `crate::cxxlib::VendoredCxx::fetch` answers a **`libstdc++` request with
        // the vendored `libc++abi`'s bytes**, and pulls `libunwind` in as an
        // implied dependency. So a libc++abi image *is* mapped, and this function
        // does run for it. What the Framework does not have is the guest's own
        // classes registered yet - see [`objc_record_for`] - and that, not the
        // runtime's absence, is why the exception-type records are unset.
        return Ok(0);
    };
    let at = |symbol: &str| exports.get(symbol).and_then(|e| e.address(slide));

    // Two words in, which is where a C++ vtable *pointer* points: past the
    // offset-to-top and the type_info pointer every vtable begins with.
    //
    // Two vtables and nothing else. There is deliberately no third: see the
    // module note on why `___objc_personality_v0` is not pointed at
    // `___gxx_personality_v0`, which used to be the third.
    let (Some(no_bases), Some(one_base)) = (
        at("__ZTVN10__cxxabiv117__class_type_infoE").map(|a| a.wrapping_add(16)),
        at("__ZTVN10__cxxabiv120__si_class_type_infoE").map(|a| a.wrapping_add(16)),
    ) else {
        return Ok(0);
    };

    // The libobjc entry points that are their C++ counterpart and nothing else.
    // They are bound here rather than served as shims because a shim would be a
    // host frame in the middle of an unwind. `objc_begin_catch` is not among
    // them: it returns the object rather than the exception buffer, which is one
    // dereference and therefore not an alias.
    let aliases = [
        ("_objc_end_catch", "___cxa_end_catch"),
        ("_objc_exception_rethrow", "___cxa_rethrow"),
    ];

    NO_BASES.store(no_bases, Ordering::Release);
    ONE_BASE.store(one_base, Ordering::Release);

    let mut bound = 0;
    // The Objective-C type records, and out of `missing` rather than `bindings`:
    // a symbol no provider answered leaves no binding record at all, only the
    // slot it did not write, and that slot is precisely the zero the LSDA reads
    // as `catch (...)`.
    //
    // **The record is [`objc_record_for`], not [`record_for`], and that is the
    // whole change.** `record_for` builds the *Itanium* `__si_class_type_info`,
    // and a frame whose catch type comes from here is a frame whose
    // `__unwind_info` names `___objc_personality_v0` - measured: 53 entries in
    // the Framework, which is where its Objective-C `@catch` clauses live. That
    // routine reads word 0 as a call target and word 2 as a `Class *`; the
    // Itanium record has a `type_info *` there. So the record this used to
    // install was one that **no Objective-C exception could ever match**, and it
    // was installed silently.
    //
    // `objc_record_for` answers `None` until a real `objc_ehtype_vtable` is
    // published, so the name stays missing and the census keeps counting it -
    // which is the correct outcome, because the alternative is a bound slot
    // holding a record that is wrong in a way nothing can observe until an
    // `@catch` silently stops firing.
    for image in &mut report.images {
        let mut satisfied = Vec::new();
        for (i, missing) in image.missing.iter().enumerate() {
            let Some(class) = missing.symbol.strip_prefix("_OBJC_EHTYPE_$_") else {
                continue;
            };
            let Some(record) = std::ffi::CString::new(class)
                .ok()
                .and_then(|n| objc::class::lookup(&n))
                .and_then(objc_record_for)
            else {
                continue;
            };
            memory.write(missing.slot_va, &record.to_le_bytes())?;
            satisfied.push((i, record));
        }
        for (i, record) in satisfied.iter().rev() {
            let was = image.missing.remove(*i);
            image.bindings.push(machish::report::BindingRecord {
                slot_va: was.slot_va,
                stub_va: was.stub_va,
                library: was.library,
                symbol: was.symbol,
                resolved: *record,
                kind: machish::fixups::BindKind::NonLazy,
                weak_import: was.weak_import,
                tag: 0,
            });
            bound += 1;
        }
    }

    for image in &mut report.images {
        for binding in &mut image.bindings {
            // `___objc_personality_v0` is **not** in this list, and its absence
            // is the fix rather than an oversight. See the module note: the
            // Itanium routine matches `__si_class_type_info` records and this
            // tree's are `objc_exception_type` records, so pointing the name
            // there made a personality routine compare unrelated bytes. The name
            // now keeps the refusal
            // `darwin::data_unavailable::announce_and_abort` publishes for it.
            let Some((_, cxx)) = aliases.iter().find(|(objc, _)| *objc == binding.symbol) else {
                continue;
            };
            // Unconditionally, over whatever provider answered: this
            // runtime publishes these names so that a call to one is
            // refused by name rather than silently unresolved, and the
            // vendored library is the real implementation.
            let Some(addr) = at(cxx) else {
                continue;
            };
            memory.write(binding.slot_va, &addr.to_le_bytes())?;
            binding.resolved = addr;
            binding.tag = 0;
            bound += 1;
        }
    }
    if crate::shims::vmmap::tracing() {
        eprintln!("rosetta3: objc exceptions: {bound} binding(s) repaired");
    }
    Ok(bound)
}

#[cfg(test)]
mod tests {
    use machish::fixups::BindKind;
    use machish::memory::{AddressSpace, BufferSpace, Placement};
    use machish::report::{ImageReport, MissingSymbol};

    use super::*;

    /// The two tests in here share [`NO_BASES`], [`ONE_BASE`] and [`RECORDS`],
    /// which [`bind`] writes and [`record_for`] reads, and the test harness runs
    /// them in parallel. One lock, taken by both, rather than one test that does
    /// two jobs - which is what this file did before, and which meant neither
    /// name said what it checked.
    static STATICS: Mutex<()> = Mutex::new(());

    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        STATICS.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The three properties libc++abi reads a record for, in one test because
    /// they share the statics [`bind`] fills in and separate tests would race
    /// over them.
    #[test]
    fn a_record_is_a_class_type_info_for_its_class() {
        let _one_at_a_time = exclusive();
        foundation::bootstrap();
        // Nothing is published before the vendored library has been found: a
        // record with a null vtable is a crash inside libc++abi, which is worse
        // than the binding it replaces.
        let exception = objc::class::lookup(c"NSException").expect("NSException");
        NO_BASES.store(0, Ordering::Release);
        ONE_BASE.store(0, Ordering::Release);
        assert_eq!(record_for(exception), None, "a record without the vtables");

        NO_BASES.store(0x1000, Ordering::Release);
        ONE_BASE.store(0x2000, Ordering::Release);

        let string = objc::class::lookup(c"NSString").expect("NSString");
        let first = record_for(string).expect("a record");
        assert_eq!(record_for(string), Some(first), "not interned");

        let object = objc::class::lookup(c"NSObject").expect("NSObject");
        // SAFETY: addresses this module leaked, whose `name` is a leaked CStr.
        unsafe {
            let record = &*(first as *const TypeInfo);
            // The base chain is the class chain, which is what makes
            // `@catch (NSObject *)` take an `NSString`.
            assert_eq!(Some(record.base), record_for(object));
            assert_eq!(
                std::ffi::CStr::from_ptr(record.name).to_str(),
                Ok("NSString")
            );
            // The root uses the vtable with no base word, and everything else
            // the one with it.
            assert_eq!((*(record.base as *const TypeInfo)).base, 0);
            assert_eq!((*(record.base as *const TypeInfo)).vptr, 0x1000);
            assert_eq!(record.vptr, 0x2000);
        }
    }

    /// The `record_for` invariant the whole file rests on: **one address per
    /// class name**, whatever order the callers arrive in.
    ///
    /// The load-bearing half is the *miss* path, and the obvious way to reach it
    /// - N threads, a barrier, one class none of them has seen - **is not a
    /// test**: it passed ten times out of ten against the code it was written to
    /// catch, because the window between the search and the insert is a handful
    /// of instructions and the first thread through closes it. So the
    /// interleaving is *arranged* instead: [`gated`] parks every thread that
    /// misses the search until [`Gate::target`] of them have missed it, which is
    /// the state the old code could not survive and the new one can.
    ///
    /// Why it matters: a personality routine compares two catch types by
    /// **address identity** first, so two records for one class name means every
    /// `@catch` for that class matches one of them and misses the other, decided
    /// by which thread threw first. Two concurrent throws of one class is not a
    /// rare event; a browser throwing the same exception class from a dozen
    /// threads is Tuesday.
    #[test]
    fn concurrent_throws_of_one_class_intern_one_address() {
        let _one_at_a_time = exclusive();
        foundation::bootstrap();
        NO_BASES.store(0x1000, Ordering::Release);
        ONE_BASE.store(0x2000, Ordering::Release);

        // A class this file's other tests do not use, so no record for it can
        // already be interned and the search really does miss.
        let class = objc::class::lookup(c"NSMutableString").expect("NSMutableString");
        assert_eq!(
            interned("NSMutableString"),
            None,
            "already interned; the race under test cannot be open"
        );

        const THREADS: usize = 4;
        GATE_ARRIVED.store(0, Ordering::Release);
        GATE_TARGET.store(THREADS, Ordering::Release);
        let got: Vec<u64> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..THREADS)
                .map(|_| scope.spawn(|| record_for(class).expect("a record")))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("a thread"))
                .collect()
        });

        let first = got[0];
        let distinct = got.iter().collect::<std::collections::BTreeSet<_>>();
        assert!(
            got.iter().all(|a| *a == first),
            "{THREADS} threads all missed the search and interned {} distinct \
             addresses for one class: {got:#x?}. A personality routine compares \
             catch types by address, so every `@catch` for this class would now \
             match one of these and miss the other.",
            distinct.len()
        );
        let interned_count = RECORDS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(n, a)| n == "NSMutableString" && *a == first)
            .count();
        assert_eq!(
            interned_count, 1,
            "the table holds the class more than once for the same address"
        );
        // The gate disarms itself, and this makes sure of it: an armed gate left
        // behind would spin a later test in this binary rather than fail it.
        GATE_TARGET.store(0, Ordering::Release);
    }

    /// The negative control for the gate test above, and the reason that test
    /// exists at all: **the interleaving it arranges really does produce two
    /// addresses under the old shape, and exactly one under the new one.**
    ///
    /// [`old_record_for`] below is the pre-fix body transcribed - search under the
    /// lock, unlock, build, lock, push without re-checking - with the same gate
    /// in the same place. Run against it, the same four threads and the same
    /// rendezvous produce more than one address, deterministically, because the
    /// gate makes every thread miss the search before any of them can insert.
    ///
    /// This is the difference between a test and a decoration. The barrier
    /// version of this experiment passed ten times out of ten against the code it
    /// was written to catch, and a test that cannot fail is worse than no test:
    /// it reports the property as held.
    #[test]
    fn the_old_check_and_insert_really_does_hand_out_two_addresses() {
        let _one_at_a_time = exclusive();
        foundation::bootstrap();
        NO_BASES.store(0x1000, Ordering::Release);
        ONE_BASE.store(0x2000, Ordering::Release);

        // A class none of this file's other tests interns, so the search
        // genuinely misses.
        let class = objc::class::lookup(c"NSMutableArray").expect("NSMutableArray");
        assert_eq!(
            interned("NSMutableArray"),
            None,
            "already interned; the shape under test cannot be observed"
        );

        const THREADS: usize = 4;
        let got: Vec<u64> = std::thread::scope(|scope| {
            GATE_ARRIVED.store(0, Ordering::Release);
            GATE_TARGET.store(THREADS, Ordering::Release);
            let handles: Vec<_> = (0..THREADS)
                .map(|_| scope.spawn(|| old_record_for(class).expect("a record")))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("a thread"))
                .collect()
        });
        GATE_TARGET.store(0, Ordering::Release);

        let distinct = got.iter().collect::<std::collections::BTreeSet<_>>();
        assert!(
            distinct.len() > 1,
            "the old shape handed out {} address(es) for one class with the same \
             gate in the same place: {got:#x?}. If this now passes, the gate does \
             not reproduce the window and the test beside it is decoration.",
            distinct.len()
        );
        // And the new shape, on a class of its own, does not - so the two
        // results are about the shape and not about the timing.
        let other = objc::class::lookup(c"NSMutableSet").expect("NSMutableSet");
        assert_eq!(interned("NSMutableSet"), None);
        let fixed: Vec<u64> = std::thread::scope(|scope| {
            GATE_ARRIVED.store(0, Ordering::Release);
            GATE_TARGET.store(THREADS, Ordering::Release);
            let handles: Vec<_> = (0..THREADS)
                .map(|_| scope.spawn(|| record_for(other).expect("a record")))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("a thread"))
                .collect()
        });
        GATE_TARGET.store(0, Ordering::Release);
        let one = fixed[0];
        assert!(
            fixed.iter().all(|a| *a == one),
            "the check-and-insert still produced {fixed:#x?}"
        );
    }

    /// [`record_for`] as it was before the check-and-insert was made atomic,
    /// with [`gated`] in the same place, for
    /// `the_old_check_and_insert_really_does_hand_out_two_addresses` to run.
    ///
    /// Transcribed rather than reconstructed from a diff so that the test and the
    /// code it judges cannot drift apart silently: this is the body, not a
    /// description of it. The two differences from [`record_for`] are the two
    /// that matter and each is marked.
    fn old_record_for(class: &'static objc::class::Class) -> Option<u64> {
        let name = class.name_str().to_string();
        if let Some(addr) = interned(&name) {
            return Some(addr);
        }
        gated();
        let base = match class.superclass() {
            Some(parent) => old_record_for(parent)?,
            None => 0,
        };
        let vptr = match base {
            0 => NO_BASES.load(Ordering::Acquire),
            _ => ONE_BASE.load(Ordering::Acquire),
        };
        if vptr == 0 {
            return None;
        }
        let text = CString::new(name.as_str()).ok()?;
        let record = Box::leak(Box::new(TypeInfo {
            vptr,
            name: Box::leak(text.into_boxed_c_str()).as_ptr(),
            base,
        }));
        let mine = record as *const TypeInfo as usize as u64;
        // (1) The insert does not re-check what the search above found, and (2) it
        // is a separate critical section from it.
        RECORDS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((name, mine));
        Some(mine)
    }

    /// [`bind`] must leave `_OBJC_EHTYPE_$_X` **unbound**, and this is the test
    /// for that rather than for a feature.
    ///
    /// It is a test for a removal because the code it guards was *working*: the
    /// name resolved, the slot was written, the count went up, and the binding
    /// was reported repaired. What it installed was a **`__cxxabiv1::
    /// __si_class_type_info`** - `{ vtable, name, base }` - into a slot that a
    /// frame reading it through `___objc_personality_v0` would take apart as a
    /// libobjc `objc_typeinfo` - `{ objc_ehtype_vtable + 2, name, Class * }`.
    /// Same size, same first two words, and a **third word that means something
    /// else**: libobjc's `_objc_exception_do_catch` hands it to
    /// `objc_remapClass` as a `Class *`, and an Itanium record's third word is
    /// the *base class's* record. Every Objective-C `@catch` decision made
    /// through that pairing is arbitrary rather than absent, which is the one
    /// failure mode this project ranks above a lost run.
    ///
    /// Which personality reads it is not a matter of taste. `__TEXT,__unwind_info`
    /// in the Framework has two personality entries: index 1, used by 3,435
    /// frames, holds `0x5680bd0` - its own statically linked
    /// `__gxx_personality_v0` - and index 2, used by **53** frames, holds the
    /// `___objc_personality_v0` import. Fifteen of those 53 sit in one run,
    /// `0x47db8d0`..`0x47dd010`. The frames with `@catch` clauses are the 53.
    ///
    /// So `bind` now asks [`objc_record_for`], which answers `None` until a real
    /// `objc_ehtype_vtable` is published - and none can be, because the symbol is
    /// imported by **0 of the 15** Mach-Os in the bundle and defined by none.
    ///
    /// * **Positive control:** `bind` *does* still repair the two libobjc
    ///   aliases, which is the other half of its job and the half that is
    ///   answerable. Without it this test could pass by `bind` doing nothing.
    /// * **The withheld one:** the slot keeps its previous contents and the
    ///   symbol stays in `missing`, so the census keeps counting it. That is the
    ///   point: a bound slot holding a record that can never match would be
    ///   invisible to every instrument this tree has.
    /// * **Negative control:** no `libc++abi` image, and `bind` returns
    ///   `Ok(0)` and changes nothing - the state `Chromium Framework` is in.
    #[test]
    fn bind_withholds_an_objective_c_record_it_cannot_shape_correctly() {
        let _one_at_a_time = exclusive();
        foundation::bootstrap();
        // The state a fresh process is in, so each control below is about the
        // thing it names and not about a leftover from another test.
        NO_BASES.store(0, Ordering::Release);
        ONE_BASE.store(0, Ordering::Release);
        publish_objc_ehtype_vtable(0);

        let flavor = crate::cxxlib::Flavor::Intel;
        let exports = flavor
            .exports(flavor.libcxxabi())
            .expect("the vendored libc++abi is an x86-64 Mach-O");

        const SLIDE: u64 = 0x100_000;
        let mut memory = BufferSpace::new(0x10_0000);
        let record_slot = memory
            .reserve(0x1000, 0x1000, Placement::Anywhere)
            .expect("a slot for the type record");
        let alias_slot = memory
            .reserve(0x2000, 0x1000, Placement::Anywhere)
            .expect("a slot for the alias");
        // Neither starts at zero: a binding that only ever sees zeros cannot
        // tell a write from a no-op, and "the slot was left alone" is exactly
        // the claim under test.
        const SEEDED: u64 = 0xfeed_face_f00d_beef;
        memory
            .write(record_slot, &SEEDED.to_le_bytes())
            .expect("a writable record slot");
        memory
            .write(alias_slot, &SEEDED.to_le_bytes())
            .expect("a writable alias slot");

        let mut report = LinkReport {
            images: vec![
                ImageReport {
                    name: "/usr/lib/libc++abi.1.dylib".to_string(),
                    slide: SLIDE,
                    ..ImageReport::default()
                },
                ImageReport {
                    name: "Chromium Framework".to_string(),
                    missing: vec![
                        MissingSymbol {
                            slot_va: record_slot,
                            stub_va: None,
                            symbol: "_OBJC_EHTYPE_$_NSException".to_string(),
                            library: Some("/usr/lib/libobjc.A.dylib".to_string()),
                            weak_import: false,
                        },
                        MissingSymbol {
                            slot_va: record_slot + 8,
                            stub_va: None,
                            symbol: "_OBJC_EHTYPE_id".to_string(),
                            library: Some("/usr/lib/libobjc.A.dylib".to_string()),
                            weak_import: false,
                        },
                    ],
                    bindings: vec![machish::report::BindingRecord {
                        slot_va: alias_slot,
                        stub_va: None,
                        library: Some("/usr/lib/libobjc.A.dylib".to_string()),
                        symbol: "_objc_end_catch".to_string(),
                        resolved: SEEDED,
                        kind: BindKind::NonLazy,
                        weak_import: false,
                        tag: 0,
                    }],
                    ..ImageReport::default()
                },
            ],
        };

        let bound = bind(
            &mut report,
            &mut memory,
            &exports,
            &crate::cxxlib::is_libcxxabi,
        )
        .expect("bind does not fail");
        assert_eq!(
            bound, 1,
            "exactly the libobjc alias; the exception-type record is not one bind can shape"
        );

        let guest = &report.images[1];
        // The positive control: the alias moved to the vendored libc++abi's
        // `__cxa_end_catch`, and the slot says so.
        let alias = guest
            .bindings
            .iter()
            .find(|b| b.symbol == "_objc_end_catch")
            .expect("the alias is still a binding");
        let vendored = exports
            .get("___cxa_end_catch")
            .and_then(|e| e.address(SLIDE))
            .expect("the vendored libc++abi exports __cxa_end_catch");
        assert_eq!(alias.resolved, vendored, "the alias was not repaired");
        let mut word = [0u8; 8];
        memory.read(alias_slot, &mut word).expect("a readable slot");
        assert_eq!(u64::from_le_bytes(word), vendored);

        // The property under test: the exception-type record is still missing,
        // and the slot still holds what it held before `bind` ran.
        assert!(
            guest
                .missing
                .iter()
                .any(|m| m.symbol == "_OBJC_EHTYPE_$_NSException"),
            "bind answered _OBJC_EHTYPE_$_NSException; the only record it can build is \
             an Itanium __si_class_type_info, and the frame that reads it is \
             ___objc_personality_v0, which wants a vtable, a name and a Class pointer"
        );
        assert!(
            !guest
                .bindings
                .iter()
                .any(|b| b.symbol == "_OBJC_EHTYPE_$_NSException"),
            "the record is in bindings; a bound slot is reported bound and a wrong \\
             record is then invisible to the unbound-import census"
        );
        let mut before = [0u8; 8];
        memory
            .read(record_slot, &mut before)
            .expect("a readable record slot");
        assert_eq!(
            u64::from_le_bytes(before),
            SEEDED,
            "the slot was written even though the report was not"
        );

        // And the two halves of why, both asserted rather than left to chance.
        let exception = objc::class::lookup(c"NSException").expect("NSException");
        assert_eq!(
            objc_record_for(exception),
            None,
            "a record was built with no objc_ehtype_vtable"
        );
        assert_eq!(
            publish_objc_ehtype_vtable(0),
            (),
            "publishing zero must not look like publishing an address"
        );
        assert_eq!(objc_record_for(exception), None);

        // The `id` record, which is unanswerable even with the vtable: libobjc
        // matches it by *address identity* against its own `OBJC_EHTYPE_id`
        // symbol, so no address this tree could publish would equal it. It is
        // also not an `_OBJC_EHTYPE_$_` name, and there is no class called `id`.
        assert!(
            guest.missing.iter().any(|m| m.symbol == "_OBJC_EHTYPE_id"),
            "`_OBJC_EHTYPE_id` was answered; libobjc matches it against its own \\
             symbol by address, so any address this tree picks is the wrong one"
        );

        // The negative control: no C++ runtime, and nothing moves.
        let mut without = LinkReport {
            images: vec![ImageReport {
                name: "Chromium Framework".to_string(),
                missing: vec![MissingSymbol {
                    slot_va: record_slot,
                    stub_va: None,
                    symbol: "_OBJC_EHTYPE_$_NSException".to_string(),
                    library: Some("/usr/lib/libobjc.A.dylib".to_string()),
                    weak_import: false,
                }],
                ..ImageReport::default()
            }],
        };
        let mut memory2 = BufferSpace::new(0x10_0000);
        let reserved2 = memory2
            .reserve(0x1000, 0x1000, Placement::Anywhere)
            .expect("a slot to write into");
        without.images[0].missing[0].slot_va = reserved2;
        assert_eq!(
            bind(
                &mut without,
                &mut memory2,
                &exports,
                &crate::cxxlib::is_libcxxabi
            )
            .expect("bind does not fail"),
            0,
            "bind claimed work with no C++ runtime in the program"
        );
        assert!(without.images[0].bindings.is_empty());
        assert_eq!(without.images[0].missing.len(), 1);
    }

    /// The two record shapes are **not** interchangeable, and the difference is
    /// in one word.
    ///
    /// Asserted on the layouts rather than on behaviour because nothing in this
    /// tree reads either one today for the Objective-C case, and a test that
    /// waited for a reader would never run. The sizes being equal is the trap:
    /// a record in the wrong shape has no symptom until a `@catch` silently
    /// stops matching, which is why this is checked as a layout.
    #[test]
    fn the_two_record_shapes_differ_in_exactly_one_word() {
        assert_eq!(
            size_of::<TypeInfo>(),
            size_of::<ObjcTypeInfo>(),
            "the shapes are the same size, which is why a mix-up is invisible"
        );
        // Words 0 and 1 line up: a vtable pointer and a name, which is why the
        // same three-word allocation is tempting for both and why the difference
        // is so easy to miss.
        assert_eq!(std::mem::offset_of!(TypeInfo, vptr), 0);
        assert_eq!(std::mem::offset_of!(ObjcTypeInfo, vtable), 0);
        assert_eq!(std::mem::offset_of!(TypeInfo, name), 8);
        assert_eq!(std::mem::offset_of!(ObjcTypeInfo, name), 8);
        // Word 2 lines up too, and that is the trap: `__class_type_info *`
        // against `Class *`, at the same offset, read by different code.
        assert_eq!(std::mem::offset_of!(TypeInfo, base), 16);
        assert_eq!(std::mem::offset_of!(ObjcTypeInfo, class), 16);
        // The *meanings* are what differ, and they are not expressible as an
        // offset, so they are stated where the types are: the Itanium one walks
        // its third word as a base chain, the Objective-C one hands it to
        // `objc_remapClass`. Asserted here only so that deleting either
        // definition's documentation is a test failure rather than a silent
        // merge - the two types must both exist for this to compile at all.
        assert_eq!(
            std::any::type_name::<TypeInfo>(),
            std::any::type_name::<ObjcTypeInfo>().replace("ObjcTypeInfo", "TypeInfo"),
            "the two records are distinct types, which is the only thing keeping \
             them from being silently unified"
        );
    }

    /// `bind` must **not** repair `___objc_personality_v0`, and this is the test
    /// for the removal rather than for a feature.
    ///
    /// It is a test rather than a comment because the code it guards was
    /// *working*: the name resolved, the slot was written, the count went up, and
    /// the binding was reported repaired. What it installed was the **Itanium**
    /// personality routine at a name whose frames carry libobjc's one-word
    /// `objc_exception_type` records - and that routine reads a base pointer out
    /// of offset 16 of the record to walk a base chain, so on those records it
    /// compares unrelated bytes. Every `@catch` decision made through it is
    /// arbitrary rather than absent, which is the one failure mode this project
    /// ranks above a lost run.
    ///
    /// Positive control: the same program **with** a libc++abi image and an
    /// `_OBJC_EHTYPE_$_NSException` import still gets its record, so removing the
    /// personality row did not quietly remove the work that row's function was
    /// actually built to do.
    #[test]
    fn bind_leaves_the_objective_c_personality_alone() {
        let _one_at_a_time = exclusive();
        foundation::bootstrap();
        NO_BASES.store(0, Ordering::Release);
        ONE_BASE.store(0, Ordering::Release);

        let flavor = crate::cxxlib::Flavor::Intel;
        let exports = flavor
            .exports(flavor.libcxxabi())
            .expect("the vendored libc++abi is an x86-64 Mach-O");

        const SLIDE: u64 = 0x100_000;
        let mut memory = BufferSpace::new(0x10_0000);
        let record_slot = memory
            .reserve(0x1000, 0x1000, Placement::Anywhere)
            .expect("a slot for the type record");
        let personality_slot = memory
            .reserve(0x2000, 0x1000, Placement::Anywhere)
            .expect("a slot for the personality");
        // Not zero, so "the slot still holds what the provider put there" is a
        // statement about the provider's answer rather than about the buffer.
        memory
            .write(personality_slot, &0x1111_2222_3333_4444u64.to_le_bytes())
            .expect("a writable slot");
        let before = {
            let mut w = [0u8; 8];
            memory.read(personality_slot, &mut w).expect("readable");
            u64::from_le_bytes(w)
        };

        let mut report = LinkReport {
            images: vec![
                ImageReport {
                    name: "/usr/lib/libc++abi.1.dylib".to_string(),
                    slide: SLIDE,
                    ..ImageReport::default()
                },
                ImageReport {
                    name: "Chromium Framework".to_string(),
                    missing: vec![MissingSymbol {
                        slot_va: record_slot,
                        stub_va: None,
                        symbol: "_OBJC_EHTYPE_$_NSException".to_string(),
                        library: Some("/usr/lib/libobjc.A.dylib".to_string()),
                        weak_import: false,
                    }],
                    // A *bound* personality import: this is the shape
                    // `darwin::data_unavailable` produces, and it is why the
                    // repair had to look in `bindings` to find the name at all.
                    bindings: vec![machish::report::BindingRecord {
                        slot_va: personality_slot,
                        stub_va: None,
                        library: Some("/usr/lib/libobjc.A.dylib".to_string()),
                        symbol: "___objc_personality_v0".to_string(),
                        resolved: before,
                        kind: BindKind::NonLazy,
                        weak_import: false,
                        tag: 0,
                    }],
                    ..ImageReport::default()
                },
            ],
        };

        let bound = bind(
            &mut report,
            &mut memory,
            &exports,
            &crate::cxxlib::is_libcxxabi,
        )
        .expect("bind does not fail");

        // The positive control: `bind` still did the half of its job that is
        // answerable. It used to be the exception-type record, which is no
        // longer one of those - see
        // `bind_withholds_an_objective_c_record_it_cannot_shape_correctly` - so
        // the control is the libobjc alias instead. Without it this test could
        // pass by `bind` declining to do anything at all.
        assert_eq!(
            bound, 0,
            "the only thing bind can repair in this report is nothing: there is no \
             _objc_end_catch import here, and the exception-type record is withheld"
        );
        assert!(
            !report.images[1]
                .bindings
                .iter()
                .any(|b| b.symbol == "_OBJC_EHTYPE_$_NSException"),
            "an exception-type record was repaired; with no objc_ehtype_vtable there \
             is no correct shape for one"
        );

        // The property under test.
        let after = report.images[1]
            .bindings
            .iter()
            .find(|b| b.symbol == "___objc_personality_v0")
            .expect("the personality import is still a binding");
        assert_eq!(
            after.resolved, before,
            "bind moved ___objc_personality_v0 to another address: the Itanium \
             personality routine is not libobjc's, and on an objc_typeinfo record \
             - {{ vtable, name, Class * }} - it reads a base-class type_info out \
             of offset 16 where libobjc put a Class pointer"
        );
        let mut w = [0u8; 8];
        memory
            .read(personality_slot, &mut w)
            .expect("the personality slot is readable");
        assert_eq!(
            u64::from_le_bytes(w),
            before,
            "the slot was rewritten even though the report was not"
        );
    }

    /// The census must not reduce a name twice, and the way to prove that is to
    /// ask `is_implemented` the only way its definition allows.
    ///
    /// `darwin::is_implemented` is `implemented(base_name(symbol))` - it reduces
    /// the Mach-O spelling itself, which is why the rule is written down at
    /// `darwin::last_unavailable`. The census used to hand it
    /// `base_name(name)` as well, and a name with two leading underscores lost
    /// two of them: `___objc_personality_v0` became `_objc_personality_v0`,
    /// which matches no row in any of `darwin`'s tables, so the census printed
    /// `NOTHING RECOGNISABLE` for a name `darwin::data_unavailable` publishes
    /// and `rosetta3` had been reporting bound all along.
    ///
    /// A census that cannot name what it finds is worse than no census, because
    /// its silence reads as a finding.
    #[test]
    fn the_census_reduces_each_name_exactly_once() {
        for name in [
            "___objc_personality_v0",
            "__Unwind_Resume",
            "__Unwind_RaiseException",
        ] {
            // The reduction `is_implemented` performs, and what one more
            // reduction on top of it would produce.
            let once = darwin::base_name(name);
            let twice = darwin::base_name(once);
            assert_ne!(
                once, twice,
                "{name}: this name does not change under a second reduction, so \
                 this test cannot detect the double reduction it exists to catch"
            );
        }
        // The concrete consequence, stated as the positive it is: the published
        // name *is* implemented when it is reduced once, which is what the
        // census now reports instead of `NOTHING RECOGNISABLE`.
        assert!(
            darwin::is_implemented("___objc_personality_v0"),
            "___objc_personality_v0 is published by darwin::data_unavailable and \
             must be recognised as implemented when handed the Mach-O spelling"
        );
        // And the name the double reduction used to produce is nothing, which is
        // why the census said NOTHING RECOGNISABLE rather than something subtler.
        assert!(
            !darwin::is_implemented("_objc_personality_v0"),
            "_objc_personality_v0 matches a row, so the double reduction was not \
             what broke the census and this explanation is wrong"
        );
    }

    /// The census is off unless asked for, and its inventory is the 23 names the
    /// guest actually imports.
    ///
    /// The count is asserted rather than derived, because an inventory that
    /// silently loses a name is indistinguishable at runtime from one that never
    /// had it - and the two names this file is *about* (`_objc_exception_throw`,
    /// `_objc_exception_rethrow`) are the two a reader would most expect to see
    /// listed. `Chromium Framework` imports neither.
    #[test]
    fn the_eh_census_covers_exactly_what_the_guest_imports() {
        assert_eq!(EH_NAMES.len(), 23);
        for absent in ["_objc_exception_throw", "_objc_exception_rethrow"] {
            assert!(
                !EH_NAMES.contains(&absent),
                "{absent} is not imported by the image; listing it would make the \
                 census an inventory of what this tree expected rather than of \
                 what the guest asks for"
            );
        }
        for present in [
            "_OBJC_EHTYPE_$_NSException",
            "_OBJC_EHTYPE_id",
            "___objc_personality_v0",
            "_objc_begin_catch",
            "_objc_end_catch",
            "__Unwind_Resume",
            "__Unwind_RaiseException",
            "_unw_step",
            "___cxa_atexit",
        ] {
            assert!(EH_NAMES.contains(&present), "{present} is missing");
        }
        // No duplicates, which would make the count a lie in the other direction.
        let mut sorted = EH_NAMES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), EH_NAMES.len(), "a name is listed twice");
    }
}
