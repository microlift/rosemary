// The C spellings are the interface.
#![allow(non_snake_case)]

//! AppKit's kill ring, which Chromium drives itself.
//!
//! # Why the guest owns the ring rather than the text system
//!
//! On a Mac, cutting and pasting inside a text field goes through the field's
//! `NSTextView`, and that view's kill ring is what `-yank:` reads. Chromium does
//! not put an `NSTextView` in the page's text field - it edits Blink's own tree
//! - so it keeps AppKit's kill ring in step by hand, and these five private
//! functions are the whole of that interface.
//!
//! Every call site is the same shape, and the shape is the specification. On the
//! cut side, `0x97808c0`:
//!
//! ```text
//! 97808d1  cmp  BYTE PTR [rdi+0x10], 1     ; the receiver's own flag
//! 97808d7  cmp  BYTE PTR [0xe935369], 0    ; a one-shot, process-global
//! 97808de  jne  skip
//! 97808e0  mov  BYTE PTR [0xe935369], 1    ; claim it
//! 97808e7  call __NSInitializeKillRing     ; so this runs once per process
//! skip:
//! 97808ec  call __NSNewKillRingSequence    ; the next append starts an entry
//! 9780910  call 0x98208d0                  ; which is the appender
//! ```
//!
//! and on the paste side, `0x97e0f50`:
//!
//! ```text
//! 97e0f6b  mov  BYTE PTR [0xe935369], 1
//! 97e0f72  call __NSInitializeKillRing
//! 97e0f77  call __NSYankFromKillRing       ; -> autoreleased NSString
//! 97e0f7f  call _objc_retainAutoreleasedReturnValue
//! 97e0f8e  call 0x9820310                  ; std::string::operator=(id)
//! 97e1089  call __NSYankFromKillRing       ; ... and once more at 0x97e1089,
//! 97e111a  call __NSSetKillRingToYankedState;    with this between
//! ```
//!
//! So the ring is fed by [`__NSAppendToKillRing`] and read by
//! [`__NSYankFromKillRing`], both from the guest, and nothing else can touch it.
//! That is what makes implementing it the *true* answer here rather than a
//! plausible one: every entry is one the guest itself put there.
//!
//! # The one signature the binary contradicts a header on
//!
//! `__NSSetKillRingToYankedState` is spelled with a `BOOL` parameter in AppKit's
//! private header. **The guest does not pass one.** Both call sites (`0x97e0ef9`
//! and `0x97e111a`) are reached straight out of a call to
//! `__NSInitializeKillRing` with no `mov edi, …` anywhere between, so the value
//! in `rdi` on entry is whatever that call happened to leave - an accident on any
//! host. A caller that meant to pass `true` would have written it into the
//! register. It is therefore declared here with no parameters, which is what the
//! binary says, and it is `void` on both backends: the native backend calls it
//! directly, and the marshaller has no signature row for it.
//!
//! # What a nil from the yank means
//!
//! `0x9820310`, the `std::string` a yank is stored into, checks its argument:
//!
//! ```text
//! 9820338  call _objc_retain
//! 9820341  mov  QWORD PTR [r14], 0x0    ; the string is emptied first
//! 9820348  test rax, rax
//! 982034b  je   0x982079a               ; ... and a nil is taken
//! ```
//!
//! so the guest is written against the empty-ring answer, and an empty ring is
//! not a failure: it is what a program gets before anything has been cut.

use std::sync::Mutex;

use objc::object::Id;

/// How many entries the ring keeps.
///
/// The guest's own use is one entry per cut and it only ever asks for the
/// newest, so the depth is not observable from anything this binary does. Eight
/// is the depth AppKit's own ring has held for as long as there has been one, and
/// it is bounded on purpose: a ring a guest can grow without limit is a leak
/// dressed as a data structure.
const DEPTH: usize = 8;

/// A ceiling on the size of one entry, in bytes.
///
/// A `std::string` the guest cuts is the length of a selection, so this is
/// generous by several orders of magnitude. It is here so that a guest looping in
/// `__NSAppendToKillRing` cannot make one entry unbounded.
const MAX_ENTRY: usize = 1 << 20;

/// A diagnostic said once, so a refusal is legible rather than silent.
fn say_once(what: &str) {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if ONCE.set(()).is_ok() {
        eprintln!("rosetta3: {what}");
    }
}

/// Newest last. Index 0 is the oldest entry still held.
struct Ring {
    entries: Vec<Vec<u8>>,
    /// Set by [`__NSNewKillRingSequence`]: the next append starts a new entry
    /// rather than extending the newest one.
    fresh: bool,
    /// What [`__NSSetKillRingToYankedState`] recorded.
    yanked: bool,
    initialised: bool,
}

fn ring() -> &'static Mutex<Ring> {
    static RING: Mutex<Ring> = Mutex::new(Ring {
        entries: Vec::new(),
        fresh: false,
        yanked: false,
        initialised: false,
    });
    &RING
}

/// The bytes of a string this runtime can read, or `None` if it is not one.
///
/// # Safety
///
/// `value` must be null or an object this runtime allocated.
unsafe fn text_of(value: Id) -> Option<Vec<u8>> {
    if value.is_null() {
        return None;
    }
    // SAFETY: the caller's obligation. `as_string` reads the `CFString` layout
    // this runtime registered, and every string a guest builds here is one of
    // them - the guest's `CFStringCreateWithCharacters` is this runtime's.
    unsafe { corefoundation::string::as_string(value as corefoundation::base::CFTypeRef) }
        .map(|s| s.utf8())
}

/// Two entries joined, truncated at [`MAX_ENTRY`].
fn joined(into: &[u8], onto: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(into.len() + onto.len());
    out.extend_from_slice(into);
    out.extend_from_slice(onto);
    out.truncate(MAX_ENTRY);
    out
}

/// `void __NSInitializeKillRing(void)`
///
/// Empties the ring. The guest calls this once per process behind a flag of its
/// own, so there is nothing here to undo and nothing to migrate.
pub extern "C" fn __NSInitializeKillRing() {
    let mut ring = ring().lock().unwrap_or_else(|e| e.into_inner());
    ring.entries.clear();
    ring.fresh = false;
    ring.yanked = false;
    ring.initialised = true;
}

/// `void __NSNewKillRingSequence(void)`
///
/// The next append starts a new entry. Within one sequence, appends extend the
/// newest entry, which is what makes a sequence a *cut* rather than a character.
/// This guest only ever makes one append per sequence, so the two readings are
/// indistinguishable here and the rule is recorded rather than invented - it is
/// what the name says, and the binary's `0x97808ec` is the only evidence for it.
pub extern "C" fn __NSNewKillRingSequence() {
    let mut ring = ring().lock().unwrap_or_else(|e| e.into_inner());
    ring.fresh = true;
}

/// `void __NSAppendToKillRing(NSString *string)`
///
/// Adds `string` to the ring. The string is **not** consumed: the guest releases
/// its own reference immediately afterwards (`0x9820948`, a tail jump to
/// `objc_release`), so the entry is a copy of the text and not the object.
pub extern "C" fn __NSAppendToKillRing(string: Id) {
    // SAFETY: the caller is AppKit's own prototype, `void (id)`, and the guest
    // only ever passes an object it holds a reference to.
    let Some(text) = (unsafe { text_of(string) }) else {
        say_once("__NSAppendToKillRing: not a string this runtime can read; nothing added");
        return;
    };
    let mut text = text;
    if text.len() > MAX_ENTRY {
        // Reported, because a truncation the guest did not ask for is exactly the
        // kind of thing that would otherwise be discovered by a paste that came
        // back short.
        say_once(&format!(
            "__NSAppendToKillRing: an entry of {} bytes was truncated to {MAX_ENTRY}",
            text.len()
        ));
        text.truncate(MAX_ENTRY);
    }
    let mut ring = ring().lock().unwrap_or_else(|e| e.into_inner());
    if ring.fresh || ring.entries.is_empty() {
        ring.entries.push(text);
        ring.fresh = false;
    } else if let Some(newest) = ring.entries.last_mut() {
        *newest = joined(newest, &text);
    }
    if ring.entries.len() > DEPTH {
        let excess = ring.entries.len() - DEPTH;
        ring.entries.drain(..excess);
    }
}

/// `NSString *__NSYankFromKillRing(void)`
///
/// The newest entry, autoreleased, or nil when the ring is empty - which is what
/// AppKit answers and what the guest's own `std::string` store is written against
/// (see the module note).
pub extern "C" fn __NSYankFromKillRing() -> Id {
    let newest = ring()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entries
        .last()
        .cloned();
    let Some(text) = newest else {
        return std::ptr::null_mut();
    };
    let string = corefoundation::string::from_bytes(&text, false);
    // SAFETY: a string this runtime just made, and `CFAutorelease` is
    // CoreFoundation's own. The guest calls `objc_retainAutoreleasedReturnValue`
    // on the answer, which is what an autoreleased return is for.
    unsafe { corefoundation::api::CFAutorelease(string) as Id }
}

/// `void __NSSetKillRingToYankedState(void)`
///
/// Records that the ring has just been yanked from. **No parameter, on the
/// binary's evidence** - see the module note.
///
/// The flag is written and read by nothing here, and that is the honest state of
/// it rather than a stub: its consumer is AppKit's own text machinery, which does
/// not exist in this runtime, and the guest keeps its own `true` beside the call
/// (`mov r14b, 1` at `0x97e111f`) and acts on *that*.
pub extern "C" fn __NSSetKillRingToYankedState() {
    let mut ring = ring().lock().unwrap_or_else(|e| e.into_inner());
    ring.yanked = true;
}

/// Whether the ring has been yanked from. For a test to observe.
#[cfg(test)]
pub(crate) fn yanked() -> bool {
    ring().lock().unwrap_or_else(|e| e.into_inner()).yanked
}

/// Whether [`__NSInitializeKillRing`] has run. For a test to observe.
#[cfg(test)]
pub(crate) fn initialised() -> bool {
    ring()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .initialised
}

exports::exports! {
    functions {
        // The guest's own indirect symbol table spells these five with **two**
        // leading underscores - `__NSInitializeKillRing` and so on, which is what
        // `test/tools/callers.py` prints and what the unbound-import report
        // prints. `exports!` publishes exactly `"_" + <this row>`, and
        // `machish::resolve::SyntheticDylib::resolve` looks the import up by that
        // published string with no normalisation, so the row is the guest's
        // spelling with **one** leading underscore removed: it keeps AppKit's own
        // leading underscore and not the Mach-O one. The C identifiers really are
        // `_NSInitializeKillRing` and the rest - the `__NS` is Apple's private
        // prefix on top of the Mach-O convention, and a private name of one's own
        // is the one case where a Mach-O symbol is not a C name plus one
        // underscore. Every other `NS...` import in this guest is spelled with a
        // single underscore, which is exactly why this one is easy to get wrong.
        //
        // Getting it wrong is invisible to a green build and a green suite: the
        // crate compiles, every test passes, and the import's slot stays zero -
        // a call through it is a jump to address 0. The unbound-import census is
        // the only instrument that sees it, so the rule is pinned in `names`
        // below and the guest's spelling is quoted in each row.
        "_NSInitializeKillRing" => __NSInitializeKillRing,
        "_NSNewKillRingSequence" => __NSNewKillRingSequence,
        "_NSAppendToKillRing" => __NSAppendToKillRing,
        "_NSYankFromKillRing" => __NSYankFromKillRing,
        "_NSSetKillRingToYankedState" => __NSSetKillRingToYankedState,
    }
}

#[cfg(test)]
mod names {
    /// Each row, and the guest's spelling, under the one rule that joins them.
    ///
    /// `exports!` publishes `"_" + <row>`; the loader looks the import up by that
    /// string verbatim. So the row must be the guest's spelling with exactly one
    /// leading underscore removed - which for these five means the private `__NS`
    /// prefix stays, and for every other `NS...` import in this guest means no
    /// prefix at all.
    ///
    /// This is pinned because the failure is one no compiler and no unit test
    /// sees. Two versions of this table were wrong before the rule was written
    /// down - one dropped the private prefix and one kept both of it - and in
    /// both cases the crate compiled, all 261 tests passed, and the report still
    /// listed all five as unbound.
    #[test]
    fn each_row_is_the_guests_spelling_less_exactly_one_underscore() {
        crate::bootstrap();
        for (row, guest) in [
            ("_NSInitializeKillRing", "__NSInitializeKillRing"),
            ("_NSNewKillRingSequence", "__NSNewKillRingSequence"),
            ("_NSAppendToKillRing", "__NSAppendToKillRing"),
            ("_NSYankFromKillRing", "__NSYankFromKillRing"),
            ("_NSSetKillRingToYankedState", "__NSSetKillRingToYankedState"),
        ] {
            assert_eq!(
                format!("_{row}"),
                guest,
                "`exports!` puts one underscore in front of the row, and that is the \
                 string the loader looks up"
            );
            assert!(
                crate::symbol(row).is_some(),
                "{row} is not answered, so the guest's {guest} would be unbound"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Held for the whole body of every test here.
    ///
    /// The ring is process-global *by design* - it is AppKit's, and the guest
    /// reaches it through a private C interface with no handle to serialise on -
    /// and every one of these tests begins by emptying it. Without this the
    /// bounded-depth test would watch another test's `__NSInitializeKillRing` land
    /// in the middle of its own twelve appends.
    static LOCK: Mutex<()> = Mutex::new(());

    /// The lock, taken for as long as the returned guard lives.
    fn serialised() -> std::sync::MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A string the caller owns, built the way the guest's appender builds the
    /// one it hands `__NSAppendToKillRing`.
    fn string(text: &str) -> Id {
        crate::bootstrap();
        corefoundation::string::from_bytes(text.as_bytes(), false) as Id
    }

    /// The text a yank answered, or `None` for the nil that means "empty ring".
    ///
    /// Read through CoreFoundation rather than by sending `-UTF8String`, because
    /// nil is a legitimate answer here and must not become a message to it.
    fn yanked_text(object: Id) -> Option<String> {
        if object.is_null() {
            return None;
        }
        // SAFETY: a string this runtime made.
        let bytes = unsafe { text_of(object) }.expect("a string");
        Some(String::from_utf8(bytes).expect("UTF-8"))
    }

    /// The guest's own sequence, and then the cut and paste that has to work
    /// through it: initialize, start a sequence, append, yank.
    ///
    /// The round trip is the point. A ring that answered a fixed string, or whose
    /// append did nothing, would pass a test that only checked the functions were
    /// callable - and the guest's yank result goes straight into a `std::string`
    /// that is then pasted, so a wrong entry is a wrong paste.
    #[test]
    fn a_cut_is_the_string_a_paste_gets() {
        crate::bootstrap();
        let _serialised = serialised();
        __NSInitializeKillRing();
        assert!(initialised());
        assert!(
            yanked_text(__NSYankFromKillRing()).is_none(),
            "an emptied ring yanks nothing, and nil is what the guest's own \
             std::string store is written against"
        );

        __NSNewKillRingSequence();
        let cut = string("the quick brown fox");
        __NSAppendToKillRing(cut);
        assert_eq!(
            yanked_text(__NSYankFromKillRing()).as_deref(),
            Some("the quick brown fox")
        );
        // The guest releases its own reference right after the append
        // (`0x9820948`), so the entry must not be the object.
        assert!(!yanked_text(__NSYankFromKillRing()).unwrap().is_empty());
        objc::arc::release(cut);
    }

    /// Two cuts are two entries, and the newer one is what a yank answers.
    ///
    /// Which is what the sequence is for: without it the second append would
    /// extend the first and the guest would paste both cuts as one.
    #[test]
    fn a_second_sequence_does_not_extend_the_first_entry() {
        crate::bootstrap();
        let _serialised = serialised();
        __NSInitializeKillRing();
        __NSNewKillRingSequence();
        let first = string("first");
        __NSAppendToKillRing(first);
        __NSNewKillRingSequence();
        let second = string("second");
        __NSAppendToKillRing(second);
        assert_eq!(yanked_text(__NSYankFromKillRing()).as_deref(), Some("second"));
        objc::arc::release(first);
        objc::arc::release(second);
    }

    /// Two appends inside one sequence *are* one entry.
    ///
    /// The pinned half of the sequence rule, and the half this guest never
    /// exercises: `0x97808ec` starts a sequence and `0x98208d0` appends once.
    #[test]
    fn appends_inside_one_sequence_extend_the_newest_entry() {
        crate::bootstrap();
        let _serialised = serialised();
        __NSInitializeKillRing();
        __NSNewKillRingSequence();
        let one = string("one ");
        __NSAppendToKillRing(one);
        let two = string("entry");
        __NSAppendToKillRing(two);
        assert_eq!(yanked_text(__NSYankFromKillRing()).as_deref(), Some("one entry"));
        objc::arc::release(one);
        objc::arc::release(two);
    }

    /// The ring does not grow without bound: it keeps the newest eight cuts and
    /// drops the oldest, so a guest looping in `__NSAppendToKillRing` cannot turn
    /// it into a leak.
    #[test]
    fn the_ring_is_bounded_and_keeps_the_newest() {
        crate::bootstrap();
        let _serialised = serialised();
        __NSInitializeKillRing();
        let mut cuts = Vec::new();
        for i in 0..(DEPTH + 4) {
            __NSNewKillRingSequence();
            let cut = string(&format!("cut {i}"));
            __NSAppendToKillRing(cut);
            cuts.push(cut);
        }
        {
            let held = ring().lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(held.entries.len(), DEPTH, "the ring is bounded");
            assert_eq!(held.entries[0], b"cut 4", "the oldest survivor is the fifth cut");
        }
        assert_eq!(
            yanked_text(__NSYankFromKillRing()).as_deref(),
            Some(format!("cut {}", DEPTH + 3).as_str())
        );
        for cut in cuts {
            objc::arc::release(cut);
        }
    }

    /// `__NSSetKillRingToYankedState` records what its name says and takes no
    /// argument - the shape the guest's two call sites establish.
    #[test]
    fn the_yanked_state_is_recorded_by_a_call_that_takes_no_argument() {
        crate::bootstrap();
        let _serialised = serialised();
        __NSInitializeKillRing();
        assert!(!yanked());
        // SAFETY: a void function of no arguments, called through a function
        // pointer of exactly that type - which is how the guest's two sites
        // reach it, with nothing written into `rdi`.
        let f: extern "C" fn() = __NSSetKillRingToYankedState;
        f();
        assert!(yanked());
    }

    /// `nil` in, nothing added and nothing read back: the ring holds strings, and
    /// a caller not holding one has said nothing.
    #[test]
    fn a_null_string_adds_nothing() {
        crate::bootstrap();
        let _serialised = serialised();
        __NSInitializeKillRing();
        __NSNewKillRingSequence();
        __NSAppendToKillRing(std::ptr::null_mut());
        assert!(yanked_text(__NSYankFromKillRing()).is_none());
    }
}
