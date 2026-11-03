use objc::object::Id;
use objc::sel::Sel;

/// `CNAuthorizationStatusRestricted`.
pub const AUTHORIZATION_STATUS_RESTRICTED: i64 = 1;

/// Install this crate's classes and methods. Idempotent.
pub fn bootstrap() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        foundation::bootstrap();
        let class = corefoundation::base::abstract_class(c"CNContactStore", None);
        class.metaclass().replace_method(
            objc::sel::get("authorizationStatusForEntityType:"),
            authorization_status_for_entity_type as *const std::ffi::c_void,
            c"q24@0:8q16",
        );
    });
}

/// `+[CNContactStore authorizationStatusForEntityType:]`.
///
/// Unknown entity types also receive the conservative `Restricted` status.
unsafe extern "C" fn authorization_status_for_entity_type(
    _class: Id,
    _sel: Sel,
    _entity_type: i64,
) -> i64 {
    AUTHORIZATION_STATUS_RESTRICTED
}

/// The Objective-C class objects exported by Contacts.framework.
pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    let class = objc::class::lookup(c"CNContactStore").expect("CNContactStore is registered");
    let mut symbols = corefoundation::base::string_globals(&[
        // AddressBook's property names, which a program reads the card of
        // the current user with.
        ("_kABEmailProperty", "Email"),
        ("_kABFirstNameProperty", "First"),
        ("_kABLastNameProperty", "Last"),
    ]);
    symbols.extend(vec![
        (
            "_OBJC_CLASS_$_CNContactStore".to_string(),
            objc::class::ptr(class) as usize as u64,
        ),
        (
            "_OBJC_METACLASS_$_CNContactStore".to_string(),
            objc::class::ptr(class.metaclass()) as usize as u64,
        ),
    ]);
    symbols
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_store_reports_restricted() {
        bootstrap();
        let class = objc::class::lookup(c"CNContactStore").expect("CNContactStore");
        let method = class
            .metaclass()
            .method(objc::sel::get("authorizationStatusForEntityType:"))
            .expect("class method");
        let implementation: unsafe extern "C" fn(Id, Sel, i64) -> i64 =
            unsafe { std::mem::transmute(method.implementation()) };
        assert_eq!(
            unsafe {
                implementation(
                    objc::class::ptr(class) as Id,
                    objc::sel::get("authorizationStatusForEntityType:"),
                    0,
                )
            },
            AUTHORIZATION_STATUS_RESTRICTED
        );
    }

    #[test]
    fn contact_store_exports_its_class_and_metaclass() {
        let symbols = data_symbols();
        assert!(
            symbols
                .iter()
                .any(|(name, _)| name == "_OBJC_CLASS_$_CNContactStore")
        );
        assert!(
            symbols
                .iter()
                .any(|(name, _)| name == "_OBJC_METACLASS_$_CNContactStore")
        );
        assert!(symbols.iter().all(|(_, address)| *address != 0));
    }
}
