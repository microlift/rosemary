//! `+[NSPrintPanel printPanel]`.
//!
//! # The census
//!
//! The classref slot is `0xe6e7b00`; the loader's own unbound report names it
//! `_OBJC_CLASS_$_NSPrintPanel from /System/Library/Frameworks/AppKit.framework`.
//!
//! | | |
//! |--|--:|
//! | confirmed loads of the classref | **1** |
//! | sends measured after it | **3** |
//! | enclosing method | `BridgedNativeWindowTracker` (all three) |
//!
//! The queue's nil-check column reads `yes` here, and this is the one entry in
//! my batch where the queue and the disassembly agree for the *right* reason -
//! so it is worth being precise about what the reason is, because the obvious
//! reading of the column ("the guest tested the answer for null") is not it.
//!
//! # The site
//!
//! `Framework+0x8b50162`, in full:
//!
//! ```text
//!  8b5015a:  call   _objc_autoreleasePoolPush
//!  8b50162:  mov    rdi,QWORD PTR [rip+0x5b97997]  # 0xe6e7b00  the class
//!  8b50169:  mov    rsi,QWORD PTR [rip+0x5b9c640]  # printPanel
//!  8b50170:  call   QWORD PTR [rip+0x51ffec2]      # objc_msgSend
//!  8b50176:  mov    rdi,rax
//!  8b50179:  call   _objc_retainAutoreleasedReturnValue
//!  8b5017e:  mov    r12,rax                        ; the panel
//!  8b50181:  mov    rsi,QWORD PTR [rip+0x5b9c630]  # options
//!  8b50188:  mov    rdi,rax
//!  8b5018b:  call   QWORD PTR [rip+0x51ffea7]      # -[panel options]
//!  8b50191:  or     rax,0x1c                       ; NSPrintSaveJob-ish flag bits
//!  8b50195:  mov    rsi,QWORD PTR [rip+0x5b9c624]  # setOptions:
//!  8b5019c:  mov    rdi,r12
//!  8b5019f:  mov    rdx,rax
//!  8b501a2:  call   QWORD PTR [rip+0x51ffe90]      # -[panel setOptions:]
//!  8b501a8:  mov    rdi,QWORD PTR [r14+0x10]       ; something else entirely
//!  8b501ac:  mov    rax,QWORD PTR [rdi]
//!  8b501af:  call   QWORD PTR [rax+0x10]           ; a C++ virtual call
//!  8b501b2:  test   rax,rax
//!  8b501b5:  je     0x8b50263
//! ```
//!
//! Two things to read off that, and the second is the one that matters:
//!
//! 1. **All three sends share one receiver**, which is the answer to
//!    `+printPanel`. `-options` and `-setOptions:` are *not* messages to an
//!    `NSPrintPanel` this runtime allocated; they are messages to whatever
//!    `+printPanel` returned. So installing `-options` and `-setOptions:` would
//!    install methods on a class that is never instantiated, and the only
//!    method that has to exist is the class method.
//! 2. **The `test rax,rax / je` at `+0x8b501b2` is not about the panel.** It
//!    tests the result of a C++ virtual call through `vtable + 0x10` on
//!    `[r14+0x10]`, an object the enclosing method already had. This is exactly
//!    the confusion the queue's column made elsewhere: a `test` in the window
//!    that is not a test of the window's answer. Here it does not change the
//!    verdict, but it is not the reason for it.
//!
//! And the reason the verdict holds is `%r12`: the panel is live in a register
//! from `+0x8b5017e` to `+0x8b501a2` and is **not read again**. From
//! `+0x8b501a8` to the `pop`s at `+0x8b5025b` the live values are `%r13`, `%r14`
//! and `%rbx`; `%r12` does not appear again in the whole tail. The guest asks
//! the shared panel for its options, ORs one flag into them, puts them back, and
//! walks away - which is a read-modify-write of a value it is about to replace
//! anyway, so the round trip carries no information out of the panel.
//!
//! # Why nil is the truthful answer
//!
//! `+[NSPrintPanel printPanel]` is documented as returning the process's
//! *shared* print panel, and on a machine with no printing subsystem there is
//! no shared print panel: there is no `NSPrintOperation`, no CUPS queue, no
//! `_PMPrintSettingsSetJobName` to hand a job name to. That last one is worth
//! noting because it is in the same method - `call _PMPrintSettingsSetJobName`
//! at `+0x8b5023c`, guarded by `test r13,r13 / je +0x8b50252` - and it is the
//! printing path proper, which is why this site is a print-setup call and not
//! the print itself.
//!
//! The answer changes if a printing subsystem appears, which is the test a real
//! answer has to pass; so the alternative would be to build a panel. That is
//! refused here for a reason worth stating plainly rather than assuming: an
//! `NSPrintPanel` that answers `-options` and swallows `-setOptions:` is a
//! **setter that discards its write**. A guest that saved options, changed them,
//! and read them back would be told it had changed them, and the only way it
//! could find out otherwise is by printing something and watching it come out
//! wrong. nil is not a consolation prize here; it is the answer that cannot lie.
//!
//! # What is deliberately not installed
//!
//! - `-options` and `-setOptions:`. See point 1 above: their receiver is the
//!   null, so `objc_msgSend` never reaches an IMP. Installing them would be
//!   unreachable surface that reads as coverage.
//! - `-[NSPrintPanel runModal]`, `-beginSheet:...`, and the rest of the panel.
//!   The census measured three sends and all three go to the answer of
//!   `+printPanel`. A `runModal` on a host with no printer would have to either
//!   block forever or return a made-up result code, and the first is a hang and
//!   the second is the plausible-constant this project ranks first.

use objc::object::Id;
use objc::sel::Sel;

use crate::define::class_methods;

/// Published so a guest binding `_OBJC_CLASS_$_NSPrintPanel` gets this class
/// rather than an empty one.
pub const CLASSES: &[&str] = &["NSPrintPanel"];

/// The shared print panel, of which there is none.
///
/// Zero colons, so zero arguments: the frame is `8 * (2 + 0)` = 16 and the
/// return is the third word, which is the `@16`.
///
/// Null, and null is the answer rather than a tolerated one - see this module's
/// comment, which shows that the panel this produces is never read again and
/// that the two sends behind it in the census window are messages *to* it.
unsafe extern "C" fn print_panel(_class: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

/// Every selector this module installs, with the encoding it declared. See
/// `spell_checker::MEASURED` for why this is a list the test reads rather than
/// a loop the install reads.
#[cfg(test)]
const MEASURED: &[(&str, &std::ffi::CStr)] = &[("printPanel", c"@16@0:8")];

pub(crate) fn install() {
    let class = corefoundation::base::abstract_class(c"NSPrintPanel", None);
    class_methods!(class, {
        "printPanel" => print_panel, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared arity is the selector's, and the frame reaches the last
    /// argument. Checked against the **name**, which is the one thing about a
    /// selector that cannot be made self-consistent by accident - the
    /// `appearance.rs` encoding was arithmetically perfect and wrong.
    #[test]
    fn the_declared_arity_is_the_selector_s() {
        for (selector, types) in MEASURED {
            crate::encoding_check::assert_row(selector, types);
        }
        for (selector, types) in MEASURED {
            crate::encoding_check::assert_installed(c"NSPrintPanel", selector, types);
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

    /// The answer is null as an `id`, and the two sends the census measured
    /// behind it answer zero on that null rather than reaching an IMP. A test
    /// that only asserted "falsy" would pass for a stand-in panel, which is the
    /// outcome this module exists to avoid.
    #[test]
    fn the_shared_panel_is_null_and_its_instance_sends_answer_zero() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSPrintPanel").expect("installed by this module");
        // SAFETY: `+printPanel` is `id (Class, SEL)`, installed above.
        let panel = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(objc::class::ptr(class) as Id, objc::sel::get("printPanel"))
        };
        assert!(panel.is_null(), "there is no shared print panel here");
        // `-[panel options]` and `-[panel setOptions:]` at
        // `Framework+0x8b5018b` and `+0x8b501a2`, sent to whatever
        // `+printPanel` returned.
        // SAFETY: `-options` is `id (id, SEL)`.
        let options = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(panel, objc::sel::get("options"))
        };
        assert!(options.is_null());
    }

    /// Neither `-options` nor `-setOptions:` is installed, and that is the
    /// decision rather than an omission: their receiver is the null above, so
    /// `objc_msgSend` never reaches an IMP, and installing them would be
    /// unreachable surface that reads as coverage.
    #[test]
    fn the_instance_sends_are_not_installed() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSPrintPanel").expect("installed by this module");
        assert!(class.own_method(objc::sel::get("options")).is_none());
        assert!(class.own_method(objc::sel::get("setOptions:")).is_none());
    }
}
