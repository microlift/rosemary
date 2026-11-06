use objc::{object::Id, sel::Sel};

unsafe fn send(receiver: Id, selector: &str, argument: Id) -> Id {
    let sel = objc::sel::get(selector);
    // SAFETY: callers supply live objects and selectors with one object argument.
    unsafe {
        let class = objc::object::class_of(receiver).expect("an object");
        let imp = objc::class::method_implementation(class, sel);
        if selector.contains(':') {
            let call: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(imp);
            call(receiver, sel, argument)
        } else {
            let call: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(imp);
            call(receiver, sel)
        }
    }
}

#[test]
fn argument_domain_enumerates_protocol_string_and_overrides_defaults() {
    let home = std::env::temp_dir().join(format!("rosemary-defaults-test-{}", std::process::id()));
    let preferences = home.join("Library/Preferences");
    std::fs::create_dir_all(&preferences).unwrap();
    let path = preferences.join("rosemary-defaults.tsv");
    let stored = "protocolString\tpersistent\nfallback\tstored\n";
    std::fs::write(&path, stored).unwrap();
    // SAFETY: this integration test process has one test and no other threads
    // accessing the environment.
    unsafe { std::env::set_var("HOME", &home) };
    foundation::bootstrap();
    let args = [
        "/Guest",
        "positional",
        "-protocolString",
        "old",
        "-protocolString",
        "example-player:1+launchmode:app",
        "-empty",
        "",
        "-trailing",
    ];
    foundation::processinfo::set_arguments(&args.map(Into::into));
    // SAFETY: registered classes and methods, with NSString keys and values.
    unsafe {
        let class = objc::class::lookup(c"NSUserDefaults").unwrap();
        let defaults = send(
            objc::class::ptr(class) as Id,
            "standardUserDefaults",
            std::ptr::null_mut(),
        );
        let name = foundation::string::from_str("NSArgumentDomain");
        let domain = send(defaults, "volatileDomainForName:", name);
        let keys = send(domain, "allKeys", std::ptr::null_mut());
        let keys = corefoundation::collections::array(keys.cast()).unwrap();
        let keys: Vec<_> = keys
            .values
            .iter()
            .map(|key| foundation::string::bytes(*key as Id))
            .collect();
        assert!(keys.contains(&b"protocolString".to_vec()));
        assert_eq!(keys.len(), 2);
        let key = foundation::string::from_str("protocolString");
        for selector in ["objectForKey:", "stringForKey:"] {
            let result = send(defaults, selector, key);
            assert_eq!(foundation::string::bytes(result), args[5].as_bytes());
        }
        let fallback = foundation::string::from_str("fallback");
        assert_eq!(
            foundation::string::bytes(send(defaults, "stringForKey:", fallback)),
            b"stored"
        );
        let empty = foundation::string::from_str("empty");
        let result = send(defaults, "stringForKey:", empty);
        assert!(!result.is_null());
        assert!(foundation::string::bytes(result).is_empty());
        foundation::processinfo::set_arguments(&["/Guest".into()]);
        assert_eq!(
            foundation::string::bytes(send(defaults, "stringForKey:", key)),
            b"persistent"
        );
        let persistent = send(defaults, "persistentDomainForName:", name);
        assert!(
            corefoundation::collections::dictionary(persistent.cast())
                .unwrap()
                .entries()
                .is_empty()
        );
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), stored);
    std::fs::remove_dir_all(home).unwrap();
}
