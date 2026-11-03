use objc::{object::Id, sel::Sel};

/// `[NSString alloc]`.
fn alloc_string() -> Id {
    let class = objc::class::lookup(c"NSString").unwrap();
    let sel = objc::sel::get("alloc");
    let imp = class.metaclass().method(sel).unwrap().implementation();
    // SAFETY: `+alloc` is `id (Class, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute_copy(&imp);
        f(objc::class::ptr(class).cast(), sel)
    }
}

fn implementation(receiver: Id, selector: &str) -> (Sel, *const ()) {
    let sel = objc::sel::get(selector);
    // SAFETY: a live object.
    let class = unsafe { objc::object::class_of(receiver) }.expect("a live object");
    let imp = class
        .method(sel)
        .unwrap_or_else(|| panic!("missing {selector}"));
    (sel, imp.implementation() as *const ())
}

/// The references an initializer's answer holds once the pool it ran in is
/// gone, besides one the caller took: an `-init...` hands its caller one.
fn owned_after_pool(init: impl FnOnce() -> Id) -> usize {
    let pool = objc::arc::pool_push();
    let string = init();
    objc::arc::retain(string);
    objc::arc::pool_pop(pool);
    let owned = objc::arc::retain_count(string) - 1;
    for _ in 0..=owned {
        objc::arc::release(string);
    }
    owned
}

#[test]
fn init_with_string_returns_an_owned_copy() {
    foundation::bootstrap();
    let source = foundation::string::from_str("marked");
    let owned = owned_after_pool(|| {
        let receiver = alloc_string();
        let (sel, imp) = implementation(receiver, "initWithString:");
        // SAFETY: `-initWithString:` is `id (id, SEL, NSString *)`.
        let string = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(imp);
            f(receiver, sel, source)
        };
        assert_ne!(string, source);
        // SAFETY: an `NSString`.
        assert_eq!(unsafe { foundation::string::bytes(string) }, b"marked");
        string
    });
    assert_eq!(owned, 1);
    objc::arc::release(source);
}

#[test]
fn init_with_characters_returns_an_owned_string() {
    foundation::bootstrap();
    let characters: Vec<u16> = "caf\u{e9}".encode_utf16().collect();
    let owned = owned_after_pool(|| {
        let receiver = alloc_string();
        let (sel, imp) = implementation(receiver, "initWithCharacters:length:");
        // SAFETY: `-initWithCharacters:length:` is `id (id, SEL, unichar *, NSUInteger)`.
        let string = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, *const u16, usize) -> Id =
                std::mem::transmute(imp);
            f(receiver, sel, characters.as_ptr(), characters.len())
        };
        // SAFETY: an `NSString`.
        assert_eq!(
            unsafe { foundation::string::bytes(string) },
            "caf\u{e9}".as_bytes()
        );
        string
    });
    assert_eq!(owned, 1);
}
