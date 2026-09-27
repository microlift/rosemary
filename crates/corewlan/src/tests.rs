//! Tests for the five answers, and for the two properties the crate's whole
//! argument rests on.
//!
//! Every assertion here is about **this host**, read live. None of them asserts
//! what the host *is*, because a test that said "this machine has a Wi-Fi radio"
//! would be a claim about the world wearing the shape of a test. What they
//! assert is that the code reports the reading, and that it refuses rather than
//! invents.

use super::*;
use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

fn named(name: &CStr) -> &'static Class {
    bootstrap();
    objc::class::lookup(name).unwrap_or_else(|| panic!("{name:?} is registered"))
}

/// The installed `IMP` for a selector, transmuted to a prototype.
///
/// Called directly rather than through `objc_msgSend`, for the reason
/// `opendirectory`'s tests give: a `BOOL`-returning selector does not come back
/// in a full register through a variadic prototype, so a test that read one as a
/// `usize` would be testing the harness.
fn imp_of(
    class: &'static Class,
    selector: &str,
    class_method: bool,
) -> objc::class::Imp {
    let owner = if class_method {
        class.metaclass()
    } else {
        class
    };
    owner
        .method(objc::sel::get(selector))
        .unwrap_or_else(|| panic!("{selector} is not installed"))
        .implementation()
}

/// `+[CWWiFiClient sharedWiFiClient]`, called the way the guest calls it.
fn shared_client() -> Id {
    let class = named(c"CWWiFiClient");
    let f: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(imp_of(class, "sharedWiFiClient", true)) };
    unsafe { f(objc::class::ptr(class) as Id, objc::sel::get("sharedWiFiClient")) }
}

/// `-[CWWiFiClient interface]`, called the way the guest calls it.
fn default_interface(client: Id) -> Id {
    let class = named(c"CWWiFiClient");
    let f: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(imp_of(class, "interface", false)) };
    unsafe { f(client, objc::sel::get("interface")) }
}

/// The whole chain, in the guest's order, ending on the `BOOL` it consumes.
fn is_wifi_enabled() -> u8 {
    let client = shared_client();
    let interface = default_interface(client);
    if interface.is_null() {
        // `return (interface && interface.powerOn);` - the guest's own shape.
        return 0;
    }
    let class = named(c"CWInterface");
    let f: unsafe extern "C" fn(Id, Sel) -> u8 =
        unsafe { std::mem::transmute(imp_of(class, "powerOn", false)) };
    unsafe { f(interface, objc::sel::get("powerOn")) }
}

/// The client is a real, retained object, and it is **one** object.
///
/// A non-nil answer is the decision this crate argues for in its note, so it is
/// asserted rather than assumed - along with the reason it is defensible: the
/// thing the handle is a handle *on* has to exist, and that is a separate test.
#[test]
fn the_shared_client_is_one_retained_object() {
    let first = shared_client();
    assert!(!first.is_null(), "+sharedWiFiClient must not answer nil");
    let second = shared_client();
    assert_eq!(
        first, second,
        "a process-wide client that answers a different object each time is a leak with a \
         different name"
    );
    assert!(
        objc::arc::retain_count(first) > 1,
        "the singleton is retained for the process, so a guest releasing it cannot dangle it"
    );
}

/// The client is non-nil **because** there is a radio, not instead of one.
///
/// This is the load-bearing pair. Read together with
/// `the_wifi_radio_state_is_read_from_the_host`, the two tests say: the answer
/// is non-nil and the reason is a measured radio. Take the radio away and the
/// first test's justification is gone; there is no reading of this host in which
/// `powerOn` is `YES` with no radio behind it.
#[test]
fn a_client_exists_only_while_the_host_has_a_radio_to_point_at() {
    let interfaces = radio::interfaces();
    let powered = radio::radio_on(&interfaces);
    let answered = is_wifi_enabled();
    if interfaces.is_empty() {
        assert_eq!(
            answered, 0,
            "with no wireless interface, -powerOn must not be sent and the answer must be NO"
        );
        assert!(default_interface(shared_client()).is_null());
    } else {
        assert_eq!(
            answered, powered as u8,
            "the answer must be the radio's state, not a constant"
        );
        assert!(
            !default_interface(shared_client()).is_null(),
            "an interface exists, so -interface must hand one back"
        );
    }
}

/// The `BOOL` the guest branches on is this host's radio, read live.
#[test]
fn the_wifi_radio_state_is_read_from_the_host() {
    let expected = radio::radio_on(&radio::interfaces());
    assert_eq!(
        is_wifi_enabled(),
        expected as u8,
        "-powerOn disagrees with the rfkill reading it is supposed to report"
    );
    // And it is the *interface's own* switch, not a constant: the per-interface
    // predicate and the machine-level one are both computed, and on a host with
    // one radio they must agree.
    for interface in radio::interfaces() {
        assert_eq!(
            interface.powered,
            radio::radio_on(&[interface.clone()]),
            "{}: the machine-level and per-interface readings disagree",
            interface.name
        );
    }
}

/// `-interfaces` is a real `NSArray` holding one object per interface, and
/// never an empty array standing in for "I did not look".
///
/// The empty array is the *convenient* answer and it is the one that would be
/// invisible: the caller returns `interface_error_count == 0`, so an empty array
/// makes a scan that never happened report success. The test therefore asserts
/// the count against the measurement, which is the only way to tell the two
/// apart.
#[test]
fn the_interface_array_agrees_with_sysfs() {
    let client = shared_client();
    let class = named(c"CWWiFiClient");
    let f: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(imp_of(class, "interfaces", false)) };
    let array = unsafe { f(client, objc::sel::get("interfaces")) };
    assert!(!array.is_null(), "-interfaces must answer an array, not nil");
    let expected = radio::interfaces().len();
    // The runtime's own `NSArray` count, through the real message send, because
    // "how many" is exactly what the caller at 0x3cd6ff3 asks.
    let entry: unsafe extern "C" fn(Id, Sel, ...) -> usize =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let count = unsafe { entry(array, objc::sel::get("count")) };
    assert_eq!(
        count, expected,
        "the array must hold one object per wireless interface: an empty array would report a \
         scan that never happened as a scan that found nothing"
    );
    objc::arc::autorelease(array);
}

/// `-interfaceName` gives the kernel's own name, and the guest's comparison
/// against it is the whole reason it must not be renamed to look like `en0`.
#[test]
fn the_interface_keeps_the_name_the_kernel_gave_it() {
    let interface = default_interface(shared_client());
    if interface.is_null() {
        return; // nothing to name, and saying so is the correct answer
    }
    let class = named(c"CWInterface");
    let f: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(imp_of(class, "interfaceName", false)) };
    let name = unsafe { f(interface, objc::sel::get("interfaceName")) };
    assert!(!name.is_null());
    let text = String::from_utf8(unsafe { foundation::string::bytes(name) })
        .expect("an interface name is text");
    let known: Vec<String> = radio::interfaces().into_iter().map(|i| i.name).collect();
    assert!(
        known.contains(&text),
        "{text:?} is not a name this host has; the guest compares it with isEqualToString:, so a \
         renamed interface would fail invisibly"
    );
    objc::arc::release(name);
}

/// The refusal half, asserted as an absence.
///
/// This is the test that keeps the MAC address from being invented: if anyone
/// ever adds an implementation for one of the twelve, this fails. It is also why
/// the list is a `const` in the crate rather than a comment.
#[test]
fn none_of_the_refused_selectors_has_an_implementation() {
    bootstrap();
    for (name, kind, selector) in REFUSED {
        // A class the image never names is not registered at all, so a refused
        // selector on it is refused one step earlier. Both are refusals; the
        // second is named in the test below so the distinction stays visible.
        let Some(class) = objc::class::lookup(name) else {
            continue;
        };
        let owner = if *kind == "+" {
            class.metaclass()
        } else {
            class
        };
        assert!(
            owner.method(objc::sel::get(selector)).is_none(),
            "{kind}[{name:?} {selector}] is implemented. Every one of these is a question about a \
             Wi-Fi *network*, answered by CoreWLAN from the airport kext, a private plist and a \
             keychain item. -[CWNetwork bssid] is a MAC address and it goes to a geolocation \
             service; inventing one is the single most tempting wrong answer available here"
        );
    }
}

/// Two of the three classes in the census are not registered, on purpose.
///
/// `__objc_classrefs` names no `_OBJC_CLASS_$_CWNetwork` and no
/// `_OBJC_CLASS_$_CWChannel` slot, so the image has no import that could ask for
/// either. Registering them would be creating surface for a caller that does
/// not exist, which is what the eight absent selectors in the crate note are.
#[test]
fn two_of_the_three_classes_are_not_registered_because_nothing_imports_them() {
    bootstrap();
    assert!(
        objc::class::lookup(c"CWNetwork").is_none(),
        "the image imports no _OBJC_CLASS_$_CWNetwork"
    );
    assert!(
        objc::class::lookup(c"CWChannel").is_none(),
        "the image imports no _OBJC_CLASS_$_CWChannel"
    );
    // `CWInterface` *is* registered - the object `-interface` returns has to be
    // an instance of something - and the spelling test below checks that it is
    // not published.
    assert!(objc::class::lookup(c"CWInterface").is_some());
}

/// `bssid` is named in this test on purpose.
///
/// If it is ever removed from [`REFUSED`] the test above stops covering it, and
/// the reason it must not be covered is worth having in executable form.
#[test]
fn the_bssid_is_still_on_the_refused_list() {
    assert!(
        REFUSED.contains(&(c"CWNetwork", "-", "bssid"))
            && REFUSED.contains(&(c"CWInterface", "-", "bssid")),
        "a BSSID is a MAC address. It is not derivable here, it is not this runtime's to publish, \
         and nothing downstream would ever check it"
    );
}

/// The installed set is exactly the census's answered half.
#[test]
fn the_selector_census_and_the_installed_methods_agree() {
    for (name, kind, selector) in SELECTORS {
        let class = named(name);
        let owner = if *kind == "+" {
            class.metaclass()
        } else {
            class
        };
        assert!(
            owner.method(objc::sel::get(selector)).is_some(),
            "{kind}[{name:?} {selector}] is not installed, and an absent selector on a published \
             class is an abort by name"
        );
    }
    for (name, kind, selector) in REFUSED {
        assert!(
            !SELECTORS.contains(&(*name, *kind, *selector)),
            "{selector} is in both lists, so the census would claim an answer and a refusal at once"
        );
    }
}

/// Every installed method's encoding has one word per selector colon.
///
/// The rule from `test/tools/OBJCENC.md`, enforced here so a future row cannot
/// break it: a mismatched arity is the shape that has crashed this guest twice,
/// and it is invisible to a clean build and to a green suite.
///
/// The count is `@`-shaped types minus two - the return type and `self`. That is
/// exact for every encoding in this crate, all of which are `@` or `c` plus
/// `@0:8` plus bare-word arguments; it would not be for an argument that is a
/// struct containing an object pointer, and there is none.
#[test]
fn every_installed_encoding_matches_its_selector_colon_count() {
    fn colons(selector: &str) -> usize {
        selector.bytes().filter(|byte| *byte == b':').count()
    }
    bootstrap();
    for (name, kind, selector) in SELECTORS {
        let class = named(name);
        let owner = if *kind == "+" {
            class.metaclass()
        } else {
            class
        };
        let method = owner
            .method(objc::sel::get(selector))
            .unwrap_or_else(|| panic!("{selector} is installed"));
        // SAFETY: a `method_t`'s `types` field is a `const char *` the runtime
        // copied out of the encoding string passed to `replace_method`.
        let types = unsafe { std::ffi::CStr::from_ptr(method.types()) }
            .to_string_lossy()
            .into_owned();
        let arguments = types.matches('@').count().saturating_sub(2);
        assert_eq!(
            arguments,
            colons(selector),
            "{kind}[{name:?} {selector}] is declared {types}, which passes {arguments} argument(s) \
             for a selector with {} colon(s)",
            colons(selector),
        );
    }
}

/// The published spelling is the image's spelling.
///
/// `exports!` publishes `"_" + row` and imports resolve verbatim, so this is the
/// only check that catches a name off by one underscore - a clean build and a
/// green suite both accept it.
#[test]
fn the_published_spelling_is_the_images_spelling() {
    bootstrap();
    let symbols = data_symbols();
    let found = symbols
        .iter()
        .find(|(published, _)| published == "_OBJC_CLASS_$_CWWiFiClient")
        .map(|(_, address)| *address);
    assert!(found.is_some(), "the image's spelling is not published");
    assert_ne!(found, Some(0), "a class at address zero is the unbound-import failure mode");
    assert_eq!(
        symbol("_OBJC_CLASS_$_CWWiFiClient"),
        found,
        "dlsym must answer the address the flat provider binds"
    );
    // And nothing extra: `__objc_classrefs` names no other CoreWLAN class, so a
    // name published that no import asks for is a name that can only be wrong.
    assert_eq!(symbols.len(), 2, "the class and its metaclass, and no more");
    assert!(
        symbols
            .iter()
            .all(|(published, _)| !published.contains("CWInterface")),
        "the image imports no _OBJC_CLASS_$_CWInterface; publishing one is inventing surface"
    );
}

/// Both classes descend from `NSObject`, which is what makes `+class` and
/// `isKindOfClass:` resolve without this crate implementing anything.
#[test]
fn both_classes_are_ns_objects() {
    // `bootstrap()` first, not `named()`: the `NSObject` lookup below is the
    // first thing this thread asks the runtime for, and the runtime's own
    // `NSObject` is installed by `objc::bootstrap()`. Looking it up first is
    // the shape `crates/objc`'s pre-existing 3% flake takes.
    bootstrap();
    let nsobject = objc::class::lookup(c"NSObject").expect("NSObject");
    for name in CLASSES {
        let class = named(name);
        assert_eq!(class.name_str(), name.to_str().unwrap());
        assert!(
            class.chain().any(|ancestor| std::ptr::eq(ancestor, nsobject)),
            "{name:?} must descend from NSObject"
        );
    }
}
