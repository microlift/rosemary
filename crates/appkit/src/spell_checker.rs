//! `+[NSSpellChecker sharedSpellChecker]`.
//!
//! # The census
//!
//! The classref slot is `0xe6e7328`, and the loader's own unbound report names
//! it and where it comes from: `_OBJC_CLASS_$_NSSpellChecker from
//! /System/Library/Frameworks/AppKit.framework`.
//!
//! | | |
//! |--|--:|
//! | confirmed loads of the classref | **18** |
//! | sends measured after a load | **23** |
//! | loads that send `sharedSpellChecker` | **15** |
//! | enclosing methods | `AppShimTerminationObserver` (13), `RenderWidgetHostViewCocoa` (1), plus 3 virtual-call sites |
//!
//! **This is the largest single item in the 122-class queue by sends**, and the
//! queue's own nil-check column reads `no` for 8 of the 15 - which puts it in
//! the "nil is the next dereference, nil is fatal" half of the queue. That
//! reading is **wrong**, and the error is the one the brief warns about by name:
//! a `test` on a register is a branch, but it is not necessarily a branch *on
//! this answer*. Checking it is the whole of this module.
//!
//! # What the disassembly actually says
//!
//! The fifteen sites are not scattered. Fourteen of them are in **one
//! contiguous 0x1030-byte region**, `Framework+0x47db8d0 .. +0x47dc900`, which
//! is `mac_notification_service`-adjacent Chromium spell-check glue
//! (`AppShimTerminationObserver` is the enclosing method, in the image's own
//! `__objc_const` method list). That contiguity is what makes the next
//! paragraph a **structural** claim rather than a sample of five: it lets me
//! enumerate every call instruction in the region and ask what kind it is.
//!
//! Every call in that region, by kind:
//!
//! | target | what it is | a nil receiver |
//! |---|---|---|
//! | `0xdd50038` | `objc_msgSend` | returns zero - Apple's contract |
//! | `0xdd50040` | `objc_retain` | no-op |
//! | `0xdd50030` | `objc_release` | no-op |
//! | `0xcca042c` | `objc_retainAutoreleasedReturnValue` | no-op |
//! | `0xcca0432` | `objc_autoreleaseReturnValue` | no-op |
//! | `0xcca0438` / `0xcca043e` / `0xcca0912` | `objc_alloc` / `objc_alloc_init` | not reached |
//! | `0xcca10fe` / `0xcca10f8` | `objc_begin_catch` / `objc_end_catch` | not reached |
//! | `0xcca02dc` | `__Unwind_Resume` | not reached |
//! | `0x5d99b20`, `0x5d99b80`, `0x5d99e30`, `0x5d99ad0` | `std::string` / `std::string_view` helpers | **not reached** - none of them takes the checker |
//! | `0x5decc50` | `operator new` | not reached |
//! | `call rbx` / `r12` / `r13` / `r15` | a hoisted `objc_release` or a block `invoke` | no-op |
//! | `call QWORD PTR [rbx+0x8]` / `[rbx+0x10]` / `[rdi+0x10]` | `std::shared_ptr` reset and destroy | **not reached** - see below |
//! | `0x4abca0`, `0x49ae60`, `0xcca0a0e`, `0xcca02e2`, `0xcca0282` | out-of-region helpers | not reached |
//!
//! The three `shared_ptr` calls are the only non-ObjC call in the region and
//! they are worth naming because they are the one thing that *could* have made
//! nil fatal. They are guarded by their own `test rbx,rbx` at `+0x47dbf25` and
//! operate on `%rbx`, which at `+0x47dbec2` is `mov rbx,QWORD PTR [rbp-0x30]`
//! - a C++ object the caller passed in. The checker at that point is `%r12`
//! (`mov r12,rax` at `+0x47dbeb`), and the languages are `%r14`
//! (`mov r14,rax` at `+0x47dbf06`). **Not one C function in the region ever
//! receives the checker.**
//!
//! # What the answer therefore is
//!
//! nil. And it is a *complete* answer, not a tolerated failure:
//!
//! - `-availableLanguages` on a nil checker is a message to nil, so the language
//!   list is nil; `-[nil containsObject:]` at `+0x47dbf1f` is `NO`; the method
//!   at `+0x47dbecf` hands that `NO` back as "this language is not available",
//!   which on this host is **true**.
//! - `-checkSpellingOfString:startingAt:language:wrap:inSpellDocumentWithTag:wordCount:`
//!   on a nil checker returns zero in every register, so `%rdx` is 0 and
//!   `test r14,r14 / sete r14b` at `+0x47dc0a3` returns **NO - "not a
//!   misspelling"**. With no spell checker installed, nothing is a misspelling.
//! - `-learnWord:`, `-unlearnWord:`, `-ignoreWord:inSpellDocumentWithTag:`,
//!   `-closeSpellDocumentWithTag:` and
//!   `-performSelectorOnMainThread:withObject:waitUntilDone:YES` on a nil
//!   checker are messages to nil and change nothing, which is the right answer
//!   to each of them: there is no dictionary to add a word to.
//! - The one method that *is* the class method, `-[RenderWidgetHostViewCocoa
//!   …]` at `+0x3a81dac`, is `return [NSSpellChecker sharedSpellChecker]` and
//!   nothing else - `jmp objc_autoreleaseReturnValue` at `+0x3a81dcc`. A nil
//!   passes straight out of it.
//!
//! # Why nil is the truthful answer and not a stand-in
//!
//! There is no spell-checking engine on this host: no `NSSpellChecker`
//! dictionary, no Hunspell, no `CFSpellChecker`, and the guest's own
//! `availableLanguages` is empty on macOS too until the user downloads a
//! dictionary - `NSSpellChecker` documents `-availableLanguages` as "possibly
//! empty". So "no shared checker exists" is a *fact about this host*, not a
//! guess about what the guest would like. It would change if a dictionary were
//! installed, which is the test a real answer has to pass and this one would
//! fail - so it is deliberately **not** installed as an object. A checker that
//! answered `-checkSpellingOfString:...` with a plausible "no" would be a
//! stand-in that looks like a working spell-checker, and the guest would then
//! report spelling as *supported* with a dictionary it does not have. That is
//! the outcome this project ranks first, and it is what nil avoids: with nil
//! the guest's own `availableLanguages` probe answers empty and Chromium's
//! spell-check menu item is disabled, which is the truth.
//!
//! # What is deliberately not installed
//!
//! - `+[NSSpellChecker sharedSpellCheckerWithLanguage:]` and
//!   `initWithLanguage:`. The census measured one selector sent to this class.
//!   A checker that existed for a *named* language but not for the shared one
//!   would be an answer nobody asked for, and if the host has no dictionaries it
//!   would be an invented one.
//! - `-setLanguage:`, `-ignoresWords:`, and the rest of the instance surface.
//!   With no checker there is no object to send them to; installing them on a
//!   class nothing allocates from would be unreachable surface pretending
//!   otherwise.
//! - Any spelling *implementation*. A home-grown one would be a worse lie than
//!   nil: it would be right about the words it knows and wrong about every word
//!   it does not, and the guest could not tell which was which.

use objc::object::Id;
use objc::sel::Sel;

use crate::define::class_methods;

/// Published so a guest binding `_OBJC_CLASS_$_NSSpellChecker` gets this class
/// rather than an empty one.
///
/// Publication is not registration. `classes::HIERARCHY` is where AppKit's own
/// classes are registered, and this list is what `appkit::data_symbols`
/// publishes; a class in only one of the two is reachable through one spelling
/// and not the other, which is `test/logs/class-gap.md`'s finding about
/// `formatter::install` restated.
pub const CLASSES: &[&str] = &["NSSpellChecker"];

/// The shared spell checker, of which there is none.
///
/// Zero colons, so zero arguments: `self` and `_cmd` occupy the frame's two
/// words and the return is the third, which is the `@16` in the encoding. The
/// frame is `8 * (2 + 0)` = 16.
///
/// The return is `id` and it is **null**, and null is the whole answer. See
/// this module's comment for the instruction-level reason that is a complete
/// answer rather than a tolerated one, and for the four `AppShimTerminationObserver`
/// selectors that are then sent *to the null* and are each right to go
/// unanswered.
unsafe extern "C" fn shared_spell_checker(_class: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

/// Every selector this module installs, with the encoding it declared.
///
/// One list, and the install below is written from it by hand rather than
/// through a `for` loop, so that `test/tools/objcenc.py diff` can see the rows
/// at all - it parses `class_methods!` bodies and would report a table-driven
/// install as silence. The test iterates *this*, which is the point: a selector
/// added to one and not the other fails, and a selector added to both with the
/// wrong encoding fails the arity check rather than the run.
#[cfg(test)]
const MEASURED: &[(&str, &std::ffi::CStr)] = &[("sharedSpellChecker", c"@16@0:8")];

pub(crate) fn install() {
    let class = corefoundation::base::abstract_class(c"NSSpellChecker", None);
    class_methods!(class, {
        "sharedSpellChecker" => shared_spell_checker, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every declared encoding has as many arguments as the selector has
    /// colons, and its frame reaches the last one.
    ///
    /// A self-consistent encoding can still be wrong about arity - the
    /// `appearance.rs` bug was arithmetically perfect and moved a block
    /// argument into a register `objc_msgSend` never writes, and its unit test
    /// stayed green because the test made the same mistake. The colon count is
    /// the one thing about a selector that cannot be made self-consistent by
    /// accident.
    #[test]
    fn the_declared_arity_is_the_selector_s() {
        for (selector, types) in MEASURED {
            crate::encoding_check::assert_row(selector, types);
        }
        // And the table is the encoding the runtime holds, not a copy of it -
        // `objcenc.py` will happily certify the table alone.
        for (selector, types) in MEASURED {
            crate::encoding_check::assert_installed(c"NSSpellChecker", selector, types);
        }
    }

    /// The class is registered *and* published under the guest's own spelling.
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

    /// The answer is nil, and it is nil *as an `id`* - not a tagged null, not a
    /// class object, not a stand-in. A test that only checked "it returns
    /// something falsy" would pass for a stand-in too.
    #[test]
    fn the_shared_checker_is_null() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSSpellChecker").expect("installed by this module");
        // SAFETY: `+sharedSpellChecker` is `id (Class, SEL)`, installed above.
        let answer = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(
                objc::class::ptr(class) as Id,
                objc::sel::get("sharedSpellChecker"),
            )
        };
        assert!(answer.is_null(), "there is no spell checker on this host");
    }

    /// The consequence the census called fatal is the consequence the guest
    /// actually gets, measured the way the guest measures it: the follow-on
    /// message is sent to the null and answers zero.
    ///
    /// This is the check the queue's nil-check column could not make. The column
    /// reported `no` - "nil is the next dereference" - for eight of the fifteen
    /// sites, and this is why that was a false negative: on `objc_msgSend` the
    /// answer to a nil receiver *is* the zero this asserts, so there is no
    /// dereference to survive.
    #[test]
    fn the_follow_on_message_the_guest_sends_answers_zero() {
        crate::bootstrap();
        // `-[NSSpellChecker availableLanguages]` is what
        // `-[AppShimTerminationObserver …]` sends to the answer at
        // `Framework+0x47dbef8`. A guest asking a null receiver answers nil,
        // which is what makes the guest's own `-[nil containsObject:]` at
        // `+0x47dbf1f` answer "this language is not available" - true here.
        // SAFETY: the selector is `id (id, SEL)`.
        let languages = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(std::ptr::null_mut(), objc::sel::get("availableLanguages"))
        };
        assert!(languages.is_null());
    }
}
