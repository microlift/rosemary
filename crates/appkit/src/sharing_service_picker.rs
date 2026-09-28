//! `NSSharingServicePicker`: `-initWithItems:` and `-setDelegate:` refused by
//! name, and the two `+` selectors the queue counted are **not** a wall.
//!
//! # The census
//!
//! The classref slot is `0xe6e7380`, and the loader's own unbound report names
//! it: `_OBJC_CLASS_$_NSSharingServicePicker from /System/Library/Frameworks/
//! AppKit.framework/Versions/C/AppKit`.
//!
//! | | |
//! |--|--:|
//! | confirmed loads of the classref | **3** |
//! | sends the queue's 96-byte window found | **5** |
//! | loads that send to **nothing** | **2** |
//! | distinct selectors refused here | **2** |
//! | distinct selectors deliberately **not** installed | **2** |
//!
//! **Two of the three loads are not a wall, and the reason is in the guest's own
//! source.** They are `-[ShareMenuController moreImage]`, and it is this
//! function at `0xbc8efc0 .. 0xbc8f019`:
//!
//! ```text
//!  bc8efc6  mov  rdi,[0xe6e7380]            ; the class
//!  bc8efcd  mov  rbx,[0xe6ed138]            ; sharedMoreMenuImage
//!  bc8efd4  mov  rsi,[0xe6e8ac8]            ; respondsToSelector:
//!  bc8efdb  mov  rdx,rbx
//!  bc8efde  call objc_msgSend              ; +[NSSharingServicePicker
//!                                           ;     respondsToSelector:@selector(
//!                                           ;       sharedMoreMenuImage)]
//!  bc8efe4  test al,al
//!  bc8efe6  je   0xbc8f00c                  ; NO  -> `xor edi,edi; return nil`
//!  bc8efe8  mov  rdi,[0xe6e7380]            ; the class again
//!  bc8efef  mov  rsi,[0xe6e9258]            ; performSelector:
//!  bc8eff6  mov  rdx,rbx
//!  bc8eff9  call objc_msgSend              ; +[NSSharingServicePicker
//!                                           ;     performSelector:@selector(
//!                                           ;       sharedMoreMenuImage)]
//!  bc8efff  mov  rdi,rax ; retainAutoreleasedReturnValue
//!  bc8f014  jmp  _objc_autoreleaseReturnValue
//!  bc8f00c  xor  edi,edi ; 0xbc8f014 jmp _objc_autoreleaseReturnValue
//! ```
//!
//! Fetched at tag `130.0.6723.91`,
//! `chrome/browser/ui/cocoa/share_menu_controller.mm`:
//!
//! ```objc
//! // Returns the image to be used for the "More..." menu item, or nil on macOS
//! // version where this private method is unsupported.
//! - (NSImage*)moreImage {
//!   if ([NSSharingServicePicker
//!           respondsToSelector:@selector(sharedMoreMenuImage)]) {
//!     return
//!         [NSSharingServicePicker performSelector:@selector(sharedMoreMenuImage)];
//!   }
//!   return nil;
//! }
//! ```
//!
//! The attribution is **positively established**, not bracketed: the `__FILE__`
//! string `../../chrome/browser/ui/cocoa/share_menu_controller.mm` is at
//! `0xbc8ec59`, and the scope string `-[ShareMenuController performShare:]`
//! immediately before it at `0xbc8ec52` is read by the enclosing region - both
//! found by `test/cstrings-in.py` over `0xbc8e800:0xbc8f400`.
//!
//! ## So `+performSelector:` is unreachable, and `+respondsToSelector:` is
//! ## answered NO, which is **true**
//!
//! `+[NSSharingServicePicker respondsToSelector:@selector(sharedMoreMenuImage)]`
//! asks whether a **private** AppKit method exists. On this host it does not,
//! and the runtime answers NO from the class's own method list -
//! `objc::class::lookup(c"NSSharingServicePicker").method(sel)` is `None`, which
//! is the same thing `-respondsToSelector:` reports. **NO is the truth**, and the
//! guest's own comment anticipates it: "or nil on macOS version where this
//! private method is unsupported".
//!
//! Two consequences, and both are the reason nothing is installed here:
//!
//! - the `+performSelector:` send at `+0xbc8eff9` is on the **not-taken** path.
//!   The queue counts it because a census over a byte window cannot know a
//!   branch; `test/callcensus.py` over the whole function still counts it, and
//!   the branch at `+0xbc8efe6` is what removes it.
//! - installing a refusal on `+performSelector:` would name a missing services
//!   list at a site the guest has no path to. A refusal is a statement about a
//!   place the guest actually goes.
//!
//! # The wall: one load, seven bytes apart from an allocator
//!
//! ```text
//!  3a8b6c9  lea  rdi,[rbp-0x38]
//!  3a8b6cd  call _objc_msgSendSuper2        ; [super init]
//!  3a8b6d2  mov  r15,rax
//!  3a8b6d5  test rax,rax ; je 0x3a8b767     ; <-- on [super init], NOT on the picker
//!  3a8b6de  mov  rdi,QWORD PTR [rip+0xac5bc9b]   # 0xe6e7380  the class
//!  3a8b6e5  call _objc_alloc
//!  3a8b6ea  mov  rsi,[0xe6e8ed0] = initWithItems:
//!  3a8b6f1  mov  rdi,rax
//!  3a8b6f4  mov  rdx,rbx
//!  3a8b6f7  call objc_msgSend              ; -[NSSharingServicePicker initWithItems:]
//!  3a8b6fd  mov  rdi,QWORD PTR [r15+0x8]   ; the old ivar
//!  3a8b701  mov  QWORD PTR [r15+0x8],rax   ; <-- THE ANSWER, INTO THE IVAR
//!  3a8b705  call _objc_release             ; release the old ivar
//!  3a8b70b  mov  rdi,QWORD PTR [r15+0x8]   ; the ivar, again
//!  3a8b70f  mov  rsi,[0xe6e8e50] = setDelegate:
//!  3a8b716  mov  rdx,r15                    ; `self`
//!  3a8b719  call objc_msgSend              ; -[picker setDelegate:self]
//! ```
//!
//! `+0x3a8b701` is the load-bearing instruction. The `test rax,rax` two
//! instructions after the send is on `[super init]`'s answer, six bytes earlier -
//! it is the guest's own "did my superclass initialiser survive" guard and has
//! nothing to do with the picker. **This is the queue's nil-check column being
//! wrong a fourth time**, and it is right about the absence of a test and wrong
//! about the consequence: after the store there is no branch at all.
//!
//! **Who the caller is: not established, and the class name is the authority.**
//! The image's own `__objc_const` method list says the enclosing method belongs
//! to the guest's **own** class `SharingServicePicker`, which is authoritative
//! for the class. Its file is not: `test/tools/whichfile.py` brackets
//! `0x3a8b6de` between `../../mojo/public/c/system/thunks.cc` (`+0xc8`) and
//! `../../content/app_shim_remote_cocoa/web_contents_view_cocoa.mm` (`-0x3da3`),
//! and `test/cstrings-in.py` over `0x3a8b5a0:0x3a8b800` returns only that mojo
//! marker and its two "Mojo has not been initialized" strings, so the function
//! carries **no `__FILE__` reference of its own**. No claim is made about it.
//!
//! # The missing capability, named
//!
//! `NSSharingServicePicker` is the system panel behind the "Share…" menu item.
//! What it shows is **the list of sharing services installed on the machine** -
//! Mail, Messages, AirDrop, the Reminders extension - which the system builds
//! by enumerating the share extensions of the user's installed applications,
//! through LaunchServices, in a login session, drawn by the window server.
//!
//! **None of that exists here.** No LaunchServices, no installed share
//! extensions, no bundle registry to enumerate, no login session, and no system
//! panel to draw one. A picker that appeared with an empty list would be a
//! control the user can open and that offers nothing, which is a *worse* outcome
//! than the refusal: it moves the failure from a named line at start-up to a
//! silent dead menu item in the middle of using the browser.
//!
//! The corroboration is already in this tree and it is not a guess:
//! `crates/darwin/src/data_unavailable.rs:908-909` refuses
//!
//! ```text
//!             "_NSSharingServiceNameAddToSafariReadingList" => refused(&PTR),
//!             "_NSSharingServiceNameComposeEmail" => refused(&PTR),
//! ```
//!
//! - the two `NSSharingServiceName*` constants the guest uses to *name* services
//! are named zeroed refusals for the same reason, and the sharing-service class
//! `NSSharingService` is itself in the 122-class queue at 2 loads and 0 sends
//! (`test/logs/class-wall-queue.md:87`).
//!
//! # Why nil is refused here, and the argument is not "it would crash"
//!
//! With nil the run would not crash, and that is the point. `-setDelegate:` on a
//! null picker is a message to nil, so it changes nothing; the guest's ivar holds
//! nil; and when the user clicks Share, `-[picker showRelativeToRect:ofView:
//! preferredEdge:]` is a message to nil, so **nothing happens and nothing is
//! logged**. The menu item is still there, still enabled - `-[ShareMenuController
//! validateMenuItem:]` gates it on `CanShare()`, which is about the URL, not
//! about any service existing - and it does nothing.
//!
//! That is the outcome `accessibility_remote_ui.rs` refuses for by name and the
//! one the orchestrator ruled on for `UNUserNotificationCenter`: **a capability
//! that looks present and answers nothing.** A refusal says "there is no sharing
//! service list here"; nil says "the answer is nothing" and lets the guest carry
//! on believing it can share.
//!
//! # What a real answer needs
//!
//! LaunchServices' share-extension registry and the system sharing panel. That
//! is a *platform* absence rather than a gap in this tree's AppKit: the panel is
//! drawn by the window server from a list the system owns. Until the platform
//! has it, the honest outcome is a refusal that says which one.
//!
//! # `ROSETTA3_LOG_UNRECOGNIZED`
//!
//! These two selectors are messages no class can answer, so leaving them out of
//! that switch would make this wall the one place the lever stops working - and
//! the lever is how the queue behind a wall is read in one run. Under it the line
//! is still printed, and it names the class, the selector, the missing service
//! and the caller. The default run, with the variable unset, still aborts here.

use std::collections::BTreeSet;
use std::sync::Mutex;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

/// Published so a guest binding `_OBJC_CLASS_$_NSSharingServicePicker` gets a
/// class that **names itself** rather than an anonymous empty one.
pub const CLASSES: &[&str] = &["NSSharingServicePicker"];

/// What is missing, said in the refusal so the line names the capability and
/// not merely the class.
const MISSING_SERVICES_LIST: &str = "The missing capability is the system's sharing-services \
    list. NSSharingServicePicker is the panel behind the Share menu item, and what it shows is \
    the sharing services installed on the machine - Mail, Messages, AirDrop, the Reminders \
    extension - which the system builds by enumerating the share extensions of the user's installed \
    applications through LaunchServices, in a login session, drawn by the window server. There is \
    no LaunchServices here, no installed share extension, no bundle registry to enumerate, no login \
    session and no panel to draw one. A nil would not be 'no services' - the guest would keep an \
    enabled Share menu item, and clicking it would send showRelativeToRect:ofView:preferredEdge: \
    to nil, which does nothing and logs nothing.";

/// The caller, said in the refusal, by name and by address.
const CALLERS: &str = "The send sites are -[NSSharingServicePicker initWithItems:] at 0x3a8b6f7, \
    whose answer is stored in the caller's ivar at 0x3a8b701 and then messaged -setDelegate: at \
    0x3a8b719 with no test anywhere between; the enclosing method belongs to the guest's own \
    SharingServicePicker class, read from the image's __objc_const method list, and its source \
    file is not established - the function carries no __FILE__ reference of its own, and \
    whichfile.py's bracket for it (mojo/public/c/system/thunks.cc .. \
    content/app_shim_remote_cocoa/web_contents_view_cocoa.mm) is 0x3da3 bytes wide and therefore \
    not evidence. The other two loads of this classref are NOT this wall: they are \
    -[ShareMenuController moreImage] at 0xbc8efc0, whose \
    +[NSSharingServicePicker respondsToSelector:@selector(sharedMoreMenuImage)] at 0xbc8efde \
    answers NO - truthfully, because there is no such private method here - so the +performSelector: \
    at 0xbc8eff9 is on a path the guest never takes and nothing is installed for it.";

/// `(class, selector)` pairs already named, so the refusal speaks once each.
static SAID: Mutex<BTreeSet<(String, String)>> = Mutex::new(BTreeSet::new());

/// Say once that there is no sharing-services list, then stop.
fn refuse(class: &str, sel: Sel) {
    // SAFETY: `objc::sel::name` is the selector itself, which `sel_registerName`
    // made a NUL-terminated string, and this is the only read of it.
    let name = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(sel)) };
    let selector = name.to_string_lossy().into_owned();
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.insert((class.to_string(), selector.clone())) {
        eprintln!(
            "rosetta3: appkit: {class} {selector}: refused by name. {MISSING_SERVICES_LIST} \
             {CALLERS}{switch}",
            switch = if objc::msg::logging_unrecognised() {
                " ROSETTA3_LOG_UNRECOGNIZED is set, so this refusal is answering nil instead of \
                 aborting; a run this way cannot be trusted."
            } else {
                ""
            },
        );
    }
    match objc::msg::logging_unrecognised() {
        // `0` is not a stand-in for an answer: it is what "answer nil" means and
        // the only way out of this function. The guest's own arithmetic is what
        // makes it a bad answer, which is why the default configuration stops.
        true => {}
        false => std::process::abort(),
    }
}

/// The IMP for a selector that returns an object.
unsafe extern "C" fn refusing_returning(receiver: Id, sel: Sel, _items: Id) -> usize {
    // SAFETY: a receiver here is a live object this runtime allocated; a null
    // receiver cannot reach an IMP, because `objc_msgSend` returns before one is
    // chosen. The class name is read off the **receiver** rather than baked in,
    // so the message cannot name a class other than the one it was installed on.
    let class = unsafe { objc::object::class_of(receiver) }
        .map_or("<no class: the receiver was null>", |c| c.name_str());
    refuse(class, sel);
    0
}

/// The IMP for a `void` selector with one argument.
unsafe extern "C" fn refusing_void_one(receiver: Id, sel: Sel, _argument: Id) {
    // SAFETY: as above.
    let class = unsafe { objc::object::class_of(receiver) }
        .map_or("<no class: the receiver was null>", |c| c.name_str());
    refuse(class, sel);
}

/// Every selector this module installs, with the encoding declared for it.
/// `#[cfg(test)]` because only the tests read it, as `spell_checker.rs:152` and
/// `print_panel.rs:124` do: a table-driven install written from a `const` is
/// invisible to `test/tools/objcenc.py`, so the certifier for these rows is in
/// this crate.
#[cfg(test)]
const MEASURED: &[(&str, &std::ffi::CStr)] =
    &[("initWithItems:", c"@24@0:8@16"), ("setDelegate:", c"v24@0:8@16")];

pub(crate) fn install() {
    // `register`, not `abstract_class`: `+0x3a8b6e5` is `call _objc_alloc` on
    // this classref, seven bytes after the load, so the class **is**
    // instantiated by the guest and has to be instantiable here or the refusal
    // would be reported for a different reason than the missing services list.
    let class = corefoundation::base::register(
        c"NSSharingServicePicker",
        None,
        size_of::<usize>(),
        false,
    );
    methods!(class, {
        "initWithItems:" => refusing_returning, c"@24@0:8@16";
        "setDelegate:" => refusing_void_one, c"v24@0:8@16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only `exactly_the_measured_selectors_are_installed` casts a class pointer
    // for `class_respondsToSelector:`, so this is the only place `c_void` is
    // named; a module-level import is an unused import in a release build.
    use std::ffi::c_void;

    /// The declared arity is the selector's own name's, and the encoding **that
    /// reached the runtime** is the one declared rather than a copy of it.
    #[test]
    fn the_declared_arity_is_the_selector_s() {
        for (selector, types) in MEASURED {
            crate::encoding_check::assert_row(selector, types);
            crate::encoding_check::assert_installed_instance(
                c"NSSharingServicePicker",
                selector,
                types,
            );
        }
    }

    /// Two selectors refused, and the two the queue counted are **not** touched.
    ///
    /// The negative half is the load-bearing one. `+respondsToSelector:` is
    /// answered by `NSObject`'s metaclass and its honest answer here is NO;
    /// installing anything on it would replace a true answer with a refusal.
    /// `+performSelector:` is on the path that NO skips, so an IMP for it would
    /// be unreachable surface naming a missing capability at a site the guest
    /// has no path to.
    #[test]
    fn exactly_the_measured_selectors_are_installed() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSSharingServicePicker")
            .expect("installed by this module");
        for (selector, _) in MEASURED {
            assert!(
                class.own_method(objc::sel::get(selector)).is_some(),
                "-[{selector}] is measured and must be refused by name"
            );
        }
        let class_sel = objc::sel::get("sharedMoreMenuImage");
        // SAFETY: `class_respondsToSelector` is the runtime's own
        // `+respondsToSelector:` implementation over the class's own method
        // list, and the class is registered by `install`.
        assert!(
            !unsafe {
                objc::api::class_respondsToSelector(
                    objc::class::ptr(class) as *const c_void,
                    class_sel,
                )
            },
            "the whole argument for installing nothing on +performSelector: is that \
             +respondsToSelector:@selector(sharedMoreMenuImage) answers NO here, and it must stay \
             that way - if it ever answered YES the guest would reach a refusal this module \
             deliberately does not install"
        );
        assert!(
            class.metaclass().own_method(objc::sel::get("respondsToSelector:")).is_none(),
            "+respondsToSelector: is answered by NSObject's metaclass and a true NO must not be \
             replaced by a refusal"
        );
        assert!(
            class.metaclass().own_method(objc::sel::get("performSelector:")).is_none(),
            "+performSelector: is unreachable: its +respondsToSelector: guard answers NO"
        );
    }

    /// Registered **and** published, so the guest's classref lands on a class
    /// whose methods name themselves rather than on an anonymous empty one.
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

    /// The class is **instantiable**, because the guest allocates it.
    ///
    /// `_objc_alloc` on this classref is at `+0x3a8b6e5`, seven bytes after the
    /// load at `+0x3a8b6de`. A class registered as abstract would refuse
    /// `_objc_alloc` for a reason that has nothing to do with the missing
    /// services list, and the guest would report that instead.
    ///
    /// The message sent is `+alloc`, not `-release`: this module installs a
    /// **refusal** on nothing named `release`, and the test must not depend on
    /// what some other module happens to have installed on `NSObject` to clean
    /// up after itself.
    #[test]
    fn the_class_is_instantiable_because_the_guest_allocates_it() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSSharingServicePicker")
            .expect("installed by this module");
        // SAFETY: `+alloc` is `id (Class, SEL)`, the class is registered, and
        // `objc::msg::entry()` is this runtime's own `objc_msgSend`.
        let alloc: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let object = unsafe { alloc(objc::class::ptr(class) as Id, objc::sel::get("alloc")) };
        assert!(!object.is_null(), "+alloc on this class must succeed");
        // And the allocation's class is the one the refusal will name, read with
        // the runtime's own `class_of` rather than by sending `-description`,
        // which this tree does not implement and whose absence would abort the
        // test binary for a reason unrelated to the property under test.
        //
        // SAFETY: `object` is live and `class_of` is a field read.
        let of = unsafe { objc::object::class_of(object) };
        assert_eq!(
            of.map(|c| c.name_str()),
            Some("NSSharingServicePicker"),
            "the allocation's class must be the one the refusal names"
        );
    }

    /// The missing capability is named, and the line says why nil was refused -
    /// not only what is absent.
    #[test]
    fn the_missing_capability_is_named_rather_than_declared_unsupported() {
        assert!(MISSING_SERVICES_LIST.contains("LaunchServices"));
        assert!(
            MISSING_SERVICES_LIST.contains("would not be"),
            "the line has to say why nil was refused, not only what is missing"
        );
        assert!(
            MISSING_SERVICES_LIST.contains("logs nothing"),
            "the consequence of nil is a dead menu item, and the line has to say that rather than \
             leaving a reader to infer it from 'no services'"
        );
        for site in ["0x3a8b6f7", "0x3a8b701", "0x3a8b719", "0xbc8efde", "0xbc8eff9"] {
            assert!(
                CALLERS.contains(site),
                "the refusal names no address for {site}, and the address is the checkable part"
            );
        }
    }
}
