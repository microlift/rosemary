//! `+[NSStatusBar systemStatusBar]`.
//!
//! # The census
//!
//! The classref slot is `0xe6e7c78`, and the loader's own unbound report names
//! it: `_OBJC_CLASS_$_NSStatusBar from
//! /System/Library/Frameworks/AppKit.framework/Versions/C/AppKit`.
//!
//! | | |
//! |--|--:|
//! | confirmed loads of the classref | **3** |
//! | sends the queue's 96-byte window found | **4** |
//! | sends I measured | **6** |
//! | enclosing method | `StatusItemController` (all three loads) |
//!
//! **The queue's four is a lower bound and the window is why.** `0xbc8f96e` is
//! the load in the *create* path; the `+[NSStatusBar systemStatusBar]` send is at
//! `+0xbc8f983` (21 bytes later, inside the window) and the
//! `-[statusBar statusItemWithLength:]` send is at `+0xbc8f9a3` (53 bytes later,
//! also inside it), and the tool reports neither, because the selector for the
//! second is loaded into `%rsi` at `+0xbc8f991` and the window's
//! "last argument register loaded from `__objc_selrefs`" rule does not survive
//! `%r12` being reloaded from `[rip+0x20c06b5]` at `+0xbc8f97c` in between. I
//! re-measured rather than took the count, so the six below are the six I can
//! point at.
//!
//! # The three sites, and what the guest does with the answer
//!
//! Two of the three are destructors, and they are the same function twice:
//!
//! ```text
//!  bc8f800  push rbp; mov rbp,rsp; push r14; push rbx
//!  bc8f807  mov  rbx,rdi
//!  bc8f80a  lea  rax,[rip+0x238b1a7]        # 0xe01a9b8   the class's vtable
//!  bc8f811  mov  [rdi],rax
//!  bc8f814  cmp  QWORD PTR [rdi+0x48],0x0  ; do I have a status item?
//!  bc8f819  je   0xbc8f857                 ; no: nothing to remove
//!  bc8f81b  mov  rdi,[rip+0x2a58456]       # 0xe6e7c78   the class
//!  bc8f822  mov  rsi,[rip+0x2a5d937]       # 0xe6ed160   systemStatusBar
//!  bc8f829  call [0xdd50038]               # objc_msgSend
//!  ...      -[statusBar removeStatusItem:[rbx+0x48]] at +0xbc8f848
//!  bc8f8a0  the identical function, and its two sends at +0xbc8f8c9 / +0xbc8f8e8
//! ```
//!
//! And one is the create, at `+0xbc8f950`:
//!
//! ```text
//!  bc8f961  mov  rdi,[rdi+0x48]            ; do I already have one?
//!  bc8f965  test rdi,rdi
//!  bc8f968  jne  0xbc8fa50                 ; yes: reuse it
//!  bc8f96e  mov  rdi,[rip+0x2a58303]       # 0xe6e7c78   the class
//!  bc8f975  mov  rsi,[rip+0x2a5d7e4]       # 0xe6ed160   systemStatusBar
//!  bc8f983  call r12                       # objc_msgSend
//!  bc8f98e  call _objc_retainAutoreleasedReturnValue
//!  bc8f991  mov  rsi,[rip+0x2a5d7d8]       # 0xe6ed170   statusItemWithLength:
//!  bc8f998  movsd xmm0,[rip+0x19bbf30]     # 0xd64b8d0 = -2.0
//!  bc8f9a3  call r12                       # -[bar statusItemWithLength:-2.0]
//!  bc8f9b2  mov  [rbx+0x48],rax            ; the item, into the ivar
//!  bc8f9ca  mov  rsi,[rip+0x2a5d7a7]       # 0xe6ed178   button
//!  bc8f9e9  mov  edx,1                     ; -[item button], then -[button setEnabled:YES]
//!  bc8f9f5  mov  rsi,[rip+0x2a597b4]       # 0xe6e91b0   setTarget:
//!  bc8fa09  mov  rsi,[rip+0x2a5a808]       # 0xe6ea218   setAction:
// 0xe6ed180 = @selector(handleClick:)       # 0xe6ebaa0 = -[item cell]
//! ```
//!
//! Three things to read off that.
//!
//! 1. **The `-2.0` is Apple's own constant, read out of the image.** `0xd64b8d0`
//!    is a `__TEXT,__const` double and its value is `-2.0`, which is
//!    `NSSquareStatusItemLength` (`NSSquareStatusItemLength = -2`,
//!    `NSVariableStatusItemLength = -1`). So this is not a length the runtime
//!    chose; the guest is asking for the square status item, the menu-bar button
//!    that shows an image rather than text.
//! 2. **All three sites share one receiver**, the answer to
//!    `+systemStatusBar`. `-statusItemWithLength:` and `-removeStatusItem:` are
//!    not messages to an `NSStatusBar` this runtime allocated; they are messages
//!    to whatever `+systemStatusBar` returned. So the only method that has to
//!    exist is the class method.
//! 3. **The queue's nil-check column reads `no x4`** - "nil is the next
//!    dereference" - and that is right *about the four sends it counted* and
//!    wrong about the answer. On `objc_msgSend` the answer to a nil receiver **is**
//!    the zero the guest then stores, so there is no dereference to survive. The
//!    `test rdi,rdi` at `+0xbc8f965` and the `cmp QWORD PTR [rdi+0x48],0x0` at
//!    `+0xbc8f814` are the guest's own "do I already have one" guard, on the
//!    ivar, not on the bar.
//!
//! # The answer is nil, and it is a fact about this host
//!
//! `+[NSStatusBar systemStatusBar]` returns **the system's status bar** - the
//! object that owns the menu-bar extras area, one per login session, drawn by the
//! system's window server on behalf of `SystemUIServer`.
//!
//! **There is no such object here.** No macOS window server, no menu bar, no
//! login session, no `SystemUIServer`, and no menu-bar extras area for an item to
//! be drawn into. That is an absence with an honest answer, and the answer is
//! `nil`.
//!
//! **The consequence is measured, not assumed.** With `nil`:
//!
//! - `-statusItemWithLength:` is a message to `nil`, so it answers `nil`, and
//!   `[rbx+0x48]` is written with `nil`.
//! - `-removeStatusItem:` in the two destructors is a message to `nil` and does
//!   nothing, which is right: there is no item to remove.
//! - The whole wiring chain - `-[item button]`, `-[button setEnabled:]`,
//!   `-[item setTarget:]`, `-[item setAction:@selector(handleClick:)]`,
//!   `-[item cell]`, `-[item setToolTip:]` - is six more messages to `nil`. **No
//!   target is ever wired and `handleClick:` can never run**, so the guest never
//!   believes it has a live menu-bar button whose action is a no-op.
//! - The guard at `+0xbc8f965` (`jne` when the ivar is already set) therefore
//!   never short-circuits, and each call re-asks. That is a re-ask, not a lie:
//!   the guest is asking a real question again because the previous answer
//!   legitimately gave it nothing to keep.
//!
//! **A stand-in is worse than nil here, and the reason is specific.** An
//! `NSStatusItem` that existed without being drawn would be an item the guest
//! believes it owns: it would have a target and an action, and the only way it
//! could find out the action never fires is by a user clicking a menu bar that
//! is not there. `nil` is the answer that cannot lie, and it is the same answer
//! the `+[NSPrintPanel printPanel]` module gives for the same reason - a printer
//! is missing - and the same answer `+[NSSpellChecker sharedSpellChecker]` gives
//! for a missing dictionary.
//!
//! **What would make it change:** a menu bar. A window server drawing one would
//! make this answer `nil` wrong, and the person who adds one changes one
//! function, in one place, with the reason beside it.
//!
//! # What is deliberately not installed
//!
//! - `-statusItemWithLength:` and `-removeStatusItem:`. Their receiver is the
//!   null above, so `objc_msgSend` never reaches an IMP for either. Installing
//!   them would be unreachable surface that reads as coverage.
//! - `-[NSStatusBar thickness]`, `-setStatusItem:`:`, and the rest of the bar.
//!   The census measured three loads of the classref and every send in this
//!   module is one of the six above; there is no fourth thing the guest asks.
//! - `-[NSStatusItem button]`, `-[NSStatusItem setTarget:]` and the rest of the
//!   item. The item is `nil` for the same reason the bar is, and a second class
//!   to register would be a second thing the guest never receives.

use objc::object::Id;
use objc::sel::Sel;

use crate::define::class_methods;

/// Published so a guest binding `_OBJC_CLASS_$_NSStatusBar` gets this class
/// rather than an anonymous empty one. See `spell_checker::CLASSES` for why
/// publication is a separate list from registration.
pub const CLASSES: &[&str] = &["NSStatusBar"];

/// The system's status bar, of which there is none.
///
/// Zero colons, so zero arguments: `self` and `_cmd` occupy the frame's two
/// words and the return is the third, which is the `@16` in the encoding. The
/// frame is `8 * (2 + 0)` = 16.
///
/// Null, and null is the answer rather than a tolerated failure - see this
/// module's comment, which shows that all six sends the census found are
/// messages *to* the null and that the guest's own status item is therefore
/// never created.
unsafe extern "C" fn system_status_bar(_class: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

/// Every selector this module installs, with the encoding it declared. A list
/// the test reads rather than a loop the install reads, because
/// `test/tools/objcenc.py` reads a `#[cfg(test)]` table as readily as it reads
/// the `class_methods!` body and would otherwise certify a copy of the encoding
/// that never reached the runtime.
#[cfg(test)]
const MEASURED: &[(&str, &std::ffi::CStr)] = &[("systemStatusBar", c"@16@0:8")];

pub(crate) fn install() {
    let class = corefoundation::base::abstract_class(c"NSStatusBar", None);
    class_methods!(class, {
        "systemStatusBar" => system_status_bar, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared arity is the selector's own name's, and the frame reaches
    /// the last argument. Checked against the runtime as well as the table:
    /// `assert_installed` reads `Method::types` back out of the class's own
    /// method list, which is the half a `const` table cannot certify.
    #[test]
    fn the_declared_arity_is_the_selector_s() {
        for (selector, types) in MEASURED {
            crate::encoding_check::assert_row(selector, types);
        }
        for (selector, types) in MEASURED {
            crate::encoding_check::assert_installed(c"NSStatusBar", selector, types);
        }
    }

    /// Registered *and* published. Registration alone is why
    /// `formatter::install`'s stated intent was never achieved.
    #[test]
    fn the_class_is_published() {
        crate::bootstrap();
        for name in CLASSES {
            let cname = std::ffi::CString::new(*name).unwrap();
            assert!(objc::class::lookup(&cname).is_some(), "{name} registered");
            assert!(
                crate::data_symbols()
                    .iter()
                    .any(|(symbol, _)| symbol == &format!("_OBJC_CLASS_$_{name}")),
                "{name} is published as data, not only registered"
            );
        }
    }

    /// The answer is null as an `id`, and the sends the census measured behind
    /// it answer zero on that null rather than reaching an IMP. A test that
    /// only asserted "falsy" would pass for a stand-in bar, which is the outcome
    /// this module exists to avoid.
    #[test]
    fn the_system_status_bar_is_null_and_its_instance_sends_answer_zero() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSStatusBar").expect("installed by this module");
        // SAFETY: `+systemStatusBar` is `id (Class, SEL)`, installed above.
        let bar = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(objc::class::ptr(class) as Id, objc::sel::get("systemStatusBar"))
        };
        assert!(bar.is_null(), "there is no menu bar on this host");
        // `-[bar statusItemWithLength:-2.0]` at `Framework+0xbc8f9a3` and
        // `-[bar removeStatusItem:]` at `+0xbc8f848` and `+0xbc8f8e8`, sent to
        // whatever `+systemStatusBar` returned. `-2.0` is
        // `NSSquareStatusItemLength` and is a `CGFloat`, so it goes in `%xmm0`
        // and never in a general register - the call shape `objc_msgSend`'s
        // varargs form uses, which is why this call is made by hand.
        // SAFETY: the selector is `id (id, SEL, CGFloat)`.
        let item = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, f64) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(bar, objc::sel::get("statusItemWithLength:"), -2.0)
        };
        assert!(item.is_null(), "there is no status bar to make an item from");
    }

    /// Neither instance method is installed, and that is the decision rather
    /// than an omission: their receiver is the null above, so `objc_msgSend`
    /// never reaches an IMP, and installing them would be unreachable surface
    /// that reads as coverage.
    #[test]
    fn the_instance_sends_are_not_installed() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSStatusBar").expect("installed by this module");
        assert!(
            class
                .own_method(objc::sel::get("statusItemWithLength:"))
                .is_none()
        );
        assert!(class.own_method(objc::sel::get("removeStatusItem:")).is_none());
    }
}
