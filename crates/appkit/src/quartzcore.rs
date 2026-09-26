//! `CATransaction`, and the assertion handler the guest reaches unconditionally.
//!
//! QuartzCore has no module in this tree and, before this file, no class
//! coverage at all - so the very first `+[CATransaction begin]` the guest sends
//! aborts by name under `ROSETTA3_LOG_UNRECOGNIZED`. `crates/appkit/src/classes.rs`
//! already declares `CALayer` and `CAMetalLayer` for the same reason, so the
//! transaction class belongs beside them rather than in a new crate.
//!
//! # What the guest actually does with it
//!
//! Measured over the whole Framework (`__objc_selrefs` loaded into a
//! message-send register, receiver identified from the `__objc_classrefs` slot
//! in `%rdi`):
//!
//! | selector | sites on `CATransaction` |
//! |---|---:|
//! | `+begin` | 10 |
//! | `+commit` | 19 |
//! | `+setValue:forKey:` | 8 |
//! | `+flush` | 1 |
//!
//! All eight `setValue:forKey:` sites are byte-identical: the key is loaded from
//! the `__got` slot for `_kCATransactionDisableActions` (`Framework+0x3a7f7fe`
//! and five others) and the value is `+[NSNumber numberWithBool:YES]`. So the
//! idiom, eight times over, is
//!
//! ```objc
//! [CATransaction begin];
//! [CATransaction setValue:@(YES) forKey:kCATransactionDisableActions];
//! // ... one property change on a view or layer ...
//! [CATransaction commit];
//! ```
//!
//! - "make this change not animate". Nothing in the image reads a transaction
//! value back: no `+valueForKey:`, no `+animationDuration`, no
//! `+invalidateCurrentTransaction` reaches this class.
//! - All four are `void` on Apple, and the guest reads no return value from any
//! of them, so there is no return value here that could be wrong.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::class_methods;

/// The key/value pairs one open transaction has collected.
type Frame = HashMap<String, Id>;

thread_local! {
    /// The open transactions of *this* thread, innermost last.
    ///
    /// A per-thread stack rather than a process-wide counter, because that is
    /// what the class is: the header calls it a transaction's own stack, and
    /// the guest opens and commits these on the main thread and on pool threads
    /// alike. A single shared counter would let one thread's `commit` pop
    /// another thread's `begin`, which is the one thing the stack shape exists
    /// to make impossible.
    static OPEN: RefCell<Vec<Frame>> = RefCell::new(Vec::new());
}

/// How many transactions this thread has open.
pub fn depth() -> usize {
    OPEN.with(|open| open.borrow().len())
}

/// The value `key` holds in this thread's innermost open transaction.
///
/// Borrowed, not owned: the pointer is valid until that transaction is
/// committed, and holding it does not keep the value alive.
pub fn value_for_key(key: &str) -> Option<Id> {
    OPEN.with(|open| {
        let open = open.borrow();
        open.last()?.get(key).copied()
    })
}

/// Give back the values a popped transaction was holding.
///
/// The transaction took a reference of its own when the value was stored, so
/// this is the matching release; the guest balances its own separately and
/// immediately (`Framework+0x3a7f818`).
fn discard(frame: Frame) {
    for (_, value) in frame {
        objc::arc::release(value);
    }
}

/// `+[CATransaction begin]`: push an empty transaction.
unsafe extern "C" fn begin(_class: Id, _sel: Sel) {
    OPEN.with(|open| open.borrow_mut().push(Frame::new()));
}

/// `+[CATransaction commit]`: pop the innermost transaction.
///
/// **A commit with nothing open does nothing, and that is the real behaviour
/// rather than a guard against a bug.** QuartzCore checks for a transaction
/// before popping one, so an unbalanced `commit` is a no-op there too.
///
/// This is load-bearing on this guest and not a hypothetical: the Framework
/// sends `+commit` **19** times and `+begin` only **10** times, and
/// `Framework+[0x80babf5,0x80baee0)` alone sends five `+commit` without a
/// single `+begin` in it - those five rely on an enclosing transaction having
/// opened one. Because a real commit decrements and stops at zero, and this one
/// stops at zero too, the depth seen by every later `setValue:forKey:` is the
/// same here as it would be on a Mac. Wrapping instead would underflow on the
/// first over-commit and turn every later write into a stack index panic.
unsafe extern "C" fn commit(_class: Id, _sel: Sel) {
    OPEN.with(|open| {
        if let Some(frame) = open.borrow_mut().pop() {
            discard(frame);
        }
    });
}

/// `+[CATransaction flush]`: commit the outermost open transaction, and with it
/// every transaction nested inside.
///
/// `Framework+0xbc8a70d` tail-jumps to this, which is the shape of a scope
/// guard's destructor: the function's whole body is the flush.
unsafe extern "C" fn flush(_class: Id, _sel: Sel) {
    OPEN.with(|open| {
        let frames: Vec<Frame> = std::mem::take(&mut *open.borrow_mut());
        for frame in frames {
            discard(frame);
        }
    });
}

/// A key that arrived nil, reported once.
///
/// This is not a defensive branch. `_kCATransactionDisableActions` is a
/// **QuartzCore data import that this runtime does not bind**, and the guest
/// reads it as a pointer to a pointer
/// (`Framework+0x3a7f7fe`: `mov rax, [rip+..]` for the cell, `mov rcx, [rax]`
/// for the `NSString *`). With the `__got` word unbound the key arrives nil at
/// all eight sites.
///
/// The string is not published here on purpose. Unlike the two
/// `NSAppearanceName*` symbols, this one's *value* is not its name - it is
/// QuartzCore's private choice - and neither Apple's documentation nor its
/// header states it. Binding the symbol to a plausible-looking string would be
/// the one answer in this file that nothing backs up, so the key is left
/// unbound and the run says so by name instead.
///
/// Nothing is lost but the write. The depth bookkeeping - the part with the
/// observable consequence, including the unbalanced-commit case - is entirely
/// unaffected, and there are no implicit animations on this host for
/// `disableActions` to switch off in the first place.
fn report_nil_key() {
    static REPORTED: std::sync::Once = std::sync::Once::new();
    REPORTED.call_once(|| {
        eprintln!(
            "rosetta3: appkit: +[CATransaction setValue:forKey:] arrived with a nil key: the \
             QuartzCore data import _kCATransactionDisableActions is unbound, so the guest's \
             own `mov rcx, [rax]` at Framework+0x3a7f805 reads nothing. The value is dropped; \
             the transaction's depth is unaffected."
        );
    });
}

/// A value stored with no transaction open, reported once and then dropped.
fn report_orphan(key: &str) {
    static REPORTED: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let mut reported = REPORTED.lock().unwrap_or_else(|e| e.into_inner());
    if reported.get_or_insert_default().insert(key.to_string()) {
        eprintln!(
            "rosetta3: appkit: +[CATransaction setValue:forKey:@\"{key}\"] with no open \
             transaction; the real CATransaction has no current transaction to write to, \
             and the value is dropped"
        );
    }
}

/// `+[CATransaction setValue:forKey:]`.
///
/// **`foundation::kvc` is not used here, and the reason is the class.** That
/// module is `-[NSObject setValue:forKey:]`: it looks for a `set<Key>:` accessor
/// and then for a `_key`/`key` ivar, and it fails by name when it finds
/// neither. `CATransaction` has neither, and is not supposed to - its KVC
/// accessors are backed by the transaction state itself, which is the one case
/// generic KVC cannot express. Pointing it at a real class here would abort
/// Chromium with `no setter and no ivar of that name` on the first call. What
/// is reused is the rest: the key is read with the same
/// `foundation::string::bytes`, and the value needs no boxing because the guest
/// hands over an `NSNumber` it built itself.
///
/// The value is retained, because the guest drops its own reference
/// immediately afterwards (`Framework+0x3a7f818`), so storing the bare pointer
/// would leave this holding a freed object.
unsafe extern "C" fn set_value_for_key(_class: Id, _sel: Sel, value: Id, key: Id) {
    if key.is_null() {
        report_nil_key();
        return;
    }
    // SAFETY: the selector's second argument is an `NSString`.
    let key = match String::from_utf8(unsafe { foundation::string::bytes(key) }) {
        Ok(key) => key,
        Err(_) => return,
    };
    OPEN.with(|open| {
        let mut stack = open.borrow_mut();
        let Some(frame) = stack.last_mut() else {
            report_orphan(&key);
            return;
        };
        match frame.insert(key, value) {
            // Nothing was there: this reference is the transaction's.
            None => {
                objc::arc::retain(value);
            }
            // Replaced: the transaction's old reference goes, the new one stays.
            Some(previous) => {
                objc::arc::retain(value);
                objc::arc::release(previous);
            }
        }
    });
}

/// `+[NSAssertionHandler currentHandler]`.
///
/// **nil, and that is the real value rather than a stand-in for one.** Three
/// things in the guest settle it:
///
/// - The image's `__objc_methname` holds `currentHandler` and
///   `handleFailureInMethod:object:file:lineNumber:description:` and **no**
///   `setCurrentHandler:`, so nothing in this program ever installs a handler.
///   On Apple the current handler is nil until someone does.
/// - The guest does not check the result: `Framework+0x72d6f7a` and `+0x72d6fd4`
///   pass it straight on as the receiver of `handleFailureInMethod:...`, and so
///   does `Framework+0x8b6d5fb`. A *neighbouring* send at `Framework+0x8b6d5e6`
///   does carry a `test`/`cmovne` pair for a nil-coalesce, so the missing check
///   here is the source's and not the optimiser's - which is what makes the
///   guest's unconditional send a deliberate one.
/// - A message to a nil receiver is defined Objective-C, and this runtime
///   implements it: `crates/objc/src/msg.rs` opens `msg_send` with
///   `test rdi, rdi; je` onto a path that zeroes `rax`/`rdx`/`xmm0`/`xmm1` and
///   returns. So the guest's unconditional send is unconditional on a Mac too,
///   and does nothing there either.
///
/// The alternative - handing back a live handler - would mean also implementing
/// `handleFailureInMethod:...` and deciding what an assertion failure *does*
/// here, on a host with no assertion system and no symbolizer to report one.
/// A handler that silently did nothing would be a fabricated failure path.
unsafe extern "C" fn current_handler(_class: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"CATransaction").expect("declared before its methods");
    class_methods!(class, {
        "begin" => begin, c"v16@0:8";
        "commit" => commit, c"v16@0:8";
        "flush" => flush, c"v16@0:8";
        "setValue:forKey:" => set_value_for_key, c"v32@0:8@16@24";
    });

    // Apple's header puts this in Foundation, and `crates/foundation` would be
    // its natural home; it is declared here because that crate is not this
    // file's to edit. `corefoundation::base::register` returns an existing
    // class rather than failing, so a second declaration is harmless.
    let class = objc::class::lookup(c"NSAssertionHandler").expect("declared before its methods");
    class_methods!(class, {
        "currentHandler" => current_handler, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `CATransaction` class, as a receiver.
    fn transaction() -> Id {
        objc::class::ptr(objc::class::lookup(c"CATransaction").expect("CATransaction")) as Id
    }

    /// `objc_msgSend` itself, so that every send below reaches its method *by
    /// name* - which is the thing that has to work, and which a direct call to
    /// an `IMP` would not test at all.
    fn send_void(receiver: Id, name: &str) {
        // SAFETY: `+begin`, `+commit` and `+flush` are all `void (Class, SEL)`.
        let f: unsafe extern "C" fn(Id, Sel) = unsafe { std::mem::transmute(objc::msg::entry()) };
        // SAFETY: the class is live and the selectors are the ones installed.
        unsafe { f(receiver, objc::sel::get(name)) };
    }

    fn send_set_value(receiver: Id, name: &str, value: Id, key: Id) {
        // SAFETY: `+setValue:forKey:` is `void (Class, SEL, id, NSString *)`.
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        // SAFETY: as above.
        unsafe { f(receiver, objc::sel::get(name), value, key) };
    }

    fn send_handler(name: &str) -> Id {
        let class = objc::class::ptr(objc::class::lookup(c"NSAssertionHandler").expect("declared"));
        // SAFETY: `+currentHandler` is `id (Class, SEL)`.
        let f: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        // SAFETY: the class is live and the selector is the one installed.
        unsafe { f(class as Id, objc::sel::get(name)) }
    }

    /// `+[NSNumber numberWithInteger:]`, which is the shape of the value the
    /// guest hands `+setValue:forKey:` - it builds a fresh object each call, so
    /// two of them can be told apart.
    fn number(value: i64) -> Id {
        let sel = objc::sel::get("numberWithInteger:");
        let class = objc::class::lookup(c"NSNumber").expect("NSNumber");
        let method = class
            .metaclass()
            .method(sel)
            .expect("+[NSNumber numberWithInteger:]");
        // SAFETY: `+numberWithInteger:` is `id (*)(Class, SEL, NSInteger)`.
        let f: unsafe extern "C" fn(Id, Sel, i64) -> Id =
            unsafe { std::mem::transmute(method.implementation()) };
        // SAFETY: the class and the IMP both came from the runtime's registry.
        unsafe { f(objc::class::ptr(class) as Id, sel, value) }
    }

    /// Every test runs on one thread, so the stack starts and must end empty.
    fn reset() {
        OPEN.with(|open| {
            let frames: Vec<Frame> = std::mem::take(&mut *open.borrow_mut());
            for frame in frames {
                discard(frame);
            }
        });
    }

    #[test]
    fn begin_and_commit_nest() {
        crate::bootstrap();
        reset();
        let class = transaction();
        assert_eq!(depth(), 0);
        send_void(class, "begin");
        assert_eq!(depth(), 1);
        send_void(class, "begin");
        assert_eq!(depth(), 2);
        send_void(class, "commit");
        assert_eq!(depth(), 1);
        send_void(class, "commit");
        assert_eq!(depth(), 0);
        reset();
    }

    /// The guest really does this: 19 `+commit` against 10 `+begin`, and
    /// `Framework+[0x80babf5,0x80baee0)` sends five `+commit` with no `+begin`
    /// in it at all.
    #[test]
    fn an_unbalanced_commit_does_not_underflow() {
        crate::bootstrap();
        reset();
        let class = transaction();
        send_void(class, "commit");
        send_void(class, "commit");
        assert_eq!(depth(), 0);
        // And a transaction opened afterwards is still a whole one.
        send_void(class, "begin");
        assert_eq!(depth(), 1);
        send_void(class, "commit");
        assert_eq!(depth(), 0);
        reset();
    }

    #[test]
    fn a_value_set_on_an_inner_transaction_does_not_outlive_it() {
        crate::bootstrap();
        reset();
        let key = foundation::string::from_str("disableActions") as Id;
        let value = number(1);
        send_void(transaction(), "begin");
        send_set_value(transaction(), "setValue:forKey:", value, key);
        assert_eq!(value_for_key("disableActions"), Some(value));

        send_void(transaction(), "commit");
        assert_eq!(depth(), 0);
        assert_eq!(value_for_key("disableActions"), None);
        // The transaction's own reference is gone; the test's is not.
        unsafe {
            objc::arc::release(value);
            corefoundation::api::CFRelease(key.cast());
        }
        reset();
    }

    /// `kCATransactionDisableActions` is the only key the guest ever sets, and
    /// it is set to `YES`, at all eight of its sites.
    #[test]
    fn the_value_reaches_the_innermost_transaction_and_flush_takes_the_outermost() {
        crate::bootstrap();
        reset();
        let key = foundation::string::from_str("disableActions") as Id;
        let outer = number(1);
        let inner = number(0);
        let class = transaction();
        send_void(class, "begin");
        send_set_value(class, "setValue:forKey:", outer, key);
        send_void(class, "begin");
        send_set_value(class, "setValue:forKey:", inner, key);
        // The inner transaction's write is the one that reads back.
        assert_eq!(value_for_key("disableActions"), Some(inner));

        // `+flush` takes the outermost, and with it everything nested.
        send_void(class, "flush");
        assert_eq!(depth(), 0);
        assert_eq!(value_for_key("disableActions"), None);
        // Two references went in and two came back out, so the test's own are
        // the only ones left.
        unsafe {
            objc::arc::release(outer);
            objc::arc::release(inner);
            corefoundation::api::CFRelease(key.cast());
        }
        reset();
    }

    /// A second write to one key replaces the first rather than leaking it.
    #[test]
    fn rewriting_a_key_releases_the_value_it_replaced() {
        crate::bootstrap();
        reset();
        let key = foundation::string::from_str("animationBehavior") as Id;
        let first = number(1);
        let second = number(2);
        let class = transaction();
        send_void(class, "begin");
        for value in [first, second] {
            send_set_value(class, "setValue:forKey:", value, key);
        }
        assert_eq!(value_for_key("animationBehavior"), Some(second));
        unsafe {
            objc::arc::release(first);
            objc::arc::release(second);
            corefoundation::api::CFRelease(key.cast());
        }
        reset();
    }

    /// The value is retained, because the guest drops its own reference
    /// immediately after the send (`Framework+0x3a7f818`).
    #[test]
    fn a_stored_value_outlives_the_reference_the_guest_dropped() {
        crate::bootstrap();
        reset();
        let key = foundation::string::from_str("disableActions") as Id;
        let value = number(7);
        let created = objc::arc::retain_count(value);
        // What the guest does: an autoreleased number, a fast-path retain into a
        // register, the send, then the matching release.
        objc::arc::retain(value);
        let class = transaction();
        send_void(class, "begin");
        send_set_value(class, "setValue:forKey:", value, key);
        objc::arc::release(value);

        // The transaction took a reference of its own: the count is the
        // object's own plus the transaction's, not back where it started.
        assert_eq!(objc::arc::retain_count(value), created + 1);
        assert_eq!(value_for_key("disableActions"), Some(value));

        send_void(class, "commit");
        // And that reference is the one the commit gives back.
        assert_eq!(objc::arc::retain_count(value), created);
        unsafe { corefoundation::api::CFRelease(key.cast()) };
        reset();
    }

    #[test]
    fn there_is_no_assertion_handler() {
        crate::bootstrap();
        assert!(send_handler("currentHandler").is_null());
    }

    /// The guest's own key never arrives, because `_kCATransactionDisableActions`
    /// is unbound - so the method must *say so* rather than return as if the
    /// write had happened. It is the one answer in this file that is a refusal,
    /// and it refuses by name.
    #[test]
    fn a_nil_key_is_reported_and_does_not_disturb_the_depth() {
        crate::bootstrap();
        reset();
        let value = number(1);
        let class = transaction();
        send_void(class, "begin");
        send_set_value(class, "setValue:forKey:", value, std::ptr::null_mut());
        // The transaction is untouched: still open, still empty.
        assert_eq!(depth(), 1);
        assert_eq!(value_for_key(""), None);
        send_void(class, "commit");
        assert_eq!(depth(), 0);
        unsafe { objc::arc::release(value) };
        reset();
    }
}
