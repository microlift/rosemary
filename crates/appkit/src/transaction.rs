//! `CATransaction`: nested groups of layer changes, one stack per thread.
//!
//! Layer changes here apply as they are made and nothing animates implicitly,
//! so a transaction only keeps its settings and its completion block.

use std::cell::RefCell;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::class_methods;

/// `+animationDuration`'s default.
const DEFAULT_DURATION: f64 = 0.25;

/// One level of the stack. A setting left unset is the enclosing level's, as
/// Core Animation looks a key up from the innermost transaction outwards.
#[derive(Default)]
struct Transaction {
    /// Opened by a setter with nothing begun, rather than by `+begin`.
    implicit: bool,
    disable_actions: Option<bool>,
    duration: Option<f64>,
    /// A copied `void (^)(void)`, run when the transaction commits.
    completion: Option<Id>,
}

thread_local! {
    /// The innermost transaction last.
    static STACK: RefCell<Vec<Transaction>> = const { RefCell::new(Vec::new()) };
}

/// The innermost transaction, opening the implicit one if nothing is begun.
fn with_current<T>(f: impl FnOnce(&mut Transaction) -> T) -> T {
    STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        if stack.is_empty() {
            stack.push(Transaction {
                implicit: true,
                ..Transaction::default()
            });
        }
        f(stack.last_mut().expect("just pushed"))
    })
}

/// The innermost value of a setting, or `default` if no level set it.
fn lookup<T>(setting: impl Fn(&Transaction) -> Option<T>, default: T) -> T {
    STACK.with(|stack| {
        stack
            .borrow()
            .iter()
            .rev()
            .find_map(setting)
            .unwrap_or(default)
    })
}

unsafe extern "C" fn begin(_class: Id, _sel: Sel) {
    STACK.with(|stack| stack.borrow_mut().push(Transaction::default()));
}

/// Pop the innermost transaction and run its completion block.
///
/// FIXME (ACCURACY): Core Animation runs a completion block once the
/// animations added inside its transaction finish; this runs it at commit.
fn commit_innermost() {
    let Some(done) = STACK.with(|stack| stack.borrow_mut().pop()) else {
        return;
    };
    let Some(block) = done.completion else {
        return;
    };
    // SAFETY: a `void (^)(void)` copied by `+setCompletionBlock:`, whose
    // `invoke` takes the block alone.
    unsafe {
        if let Some(invoke) = objc::block::invoke(block) {
            let f: unsafe extern "C" fn(Id) = std::mem::transmute(invoke);
            f(block);
        }
    }
    objc::block::release(block);
}

unsafe extern "C" fn commit(_class: Id, _sel: Sel) {
    commit_innermost();
}

/// `+flush`: commit the implicit transaction, unless an explicit one is open.
///
/// FIXME (ACCURACY): Core Animation also commits the implicit transaction at
/// the end of the run loop's pass; here only `+flush` does.
unsafe extern "C" fn flush(_class: Id, _sel: Sel) {
    let only_implicit = STACK.with(|stack| {
        let stack = stack.borrow();
        stack.len() == 1 && stack[0].implicit
    });
    if only_implicit {
        commit_innermost();
    }
}

unsafe extern "C" fn disable_actions(_class: Id, _sel: Sel) -> u8 {
    u8::from(lookup(|t| t.disable_actions, false))
}

unsafe extern "C" fn set_disable_actions(_class: Id, _sel: Sel, flag: u8) {
    with_current(|t| t.disable_actions = Some(flag != 0));
}

unsafe extern "C" fn animation_duration(_class: Id, _sel: Sel) -> f64 {
    lookup(|t| t.duration, DEFAULT_DURATION)
}

unsafe extern "C" fn set_animation_duration(_class: Id, _sel: Sel, duration: f64) {
    with_current(|t| t.duration = Some(duration));
}

/// `+setCompletionBlock:` replaces the transaction's block; nil clears it.
unsafe extern "C" fn set_completion_block(_class: Id, _sel: Sel, block: Id) {
    let block = Some(objc::block::copy(block)).filter(|block| !block.is_null());
    let replaced = with_current(|t| std::mem::replace(&mut t.completion, block));
    if let Some(replaced) = replaced {
        objc::block::release(replaced);
    }
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"CATransaction").expect("declared before its methods");
    class_methods!(class, {
        "begin" => begin, c"v16@0:8";
        "commit" => commit, c"v16@0:8";
        "flush" => flush, c"v16@0:8";
        "disableActions" => disable_actions, c"c16@0:8";
        "setDisableActions:" => set_disable_actions, c"v20@0:8c16";
        "animationDuration" => animation_duration, c"d16@0:8";
        "setAnimationDuration:" => set_animation_duration, c"v24@0:8d16";
        "setCompletionBlock:" => set_completion_block, c"v24@0:8@?16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nested_transaction_inherits_settings_until_it_sets_its_own() {
        // SAFETY: class methods that ignore their receiver.
        unsafe {
            let none = std::ptr::null_mut();
            let sel = objc::sel::get("t");
            begin(none, sel);
            set_disable_actions(none, sel, 1);
            begin(none, sel);
            assert_eq!(disable_actions(none, sel), 1);
            assert_eq!(animation_duration(none, sel), DEFAULT_DURATION);
            set_disable_actions(none, sel, 0);
            assert_eq!(disable_actions(none, sel), 0);
            commit(none, sel);
            assert_eq!(disable_actions(none, sel), 1);
            commit(none, sel);
            assert_eq!(disable_actions(none, sel), 0);
        }
    }

    #[test]
    fn flush_leaves_an_explicit_transaction_open() {
        // SAFETY: class methods that ignore their receiver.
        unsafe {
            let none = std::ptr::null_mut();
            let sel = objc::sel::get("t");
            begin(none, sel);
            set_disable_actions(none, sel, 1);
            flush(none, sel);
            assert_eq!(disable_actions(none, sel), 1);
            commit(none, sel);
            set_disable_actions(none, sel, 1);
            flush(none, sel);
            assert_eq!(disable_actions(none, sel), 0);
        }
    }
}
