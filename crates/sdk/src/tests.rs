use std::collections::BTreeSet;

use exports::decl::{Api, CType, Char, ConstPtr, Fragment, Long, Ptr, Type};

use super::*;

mod kit {
    #![allow(non_upper_case_globals)]

    use exports::decl::{ConstPtr, Long, Void};

    #[repr(C)]
    pub struct Pair {
        pub first: isize,
        pub second: *const core::ffi::c_void,
    }

    pub unsafe extern "C" fn first(pair: *const Pair) -> isize {
        unsafe { (*pair).first }
    }

    pub extern "C" fn reset(_pair: *const Pair) {}

    // Out of order on purpose: `KitPair` names `KitIndex`.
    exports::c_types! {
        "Kit/KitPair.h" {
            struct KitPair = Pair { first: KitIndex, second: ConstPtr<Void> }
        }
        "Kit/KitBase.h" {
            typedef KitIndex = Long;
        }
    }

    exports::c_functions! {
        "Kit/KitPair.h" {
            fn KitPairFirst(pair: ConstPtr<KitPair>) -> KitIndex = first;
            fn KitPairReset(pair: ConstPtr<KitPair>) = reset;
        }
    }

    exports::c_variables! {
        "Kit/KitPair.h" {
            kKitEmptyPair: KitPair;
        }
    }

    exports::c_enums! {
        "Kit/KitBase.h" {
            KitIndex { kKitNotFound = -1 }
        }
    }
}

const KIT: Api = Api {
    framework: "Kit",
    types: kit::TYPES,
    functions: kit::FUNCTIONS,
    variables: kit::VARIABLES,
    enums: kit::ENUMS,
    fragments: &[],
};

fn kit_header(name: &str) -> String {
    headers(&KIT).unwrap().remove(name).unwrap()
}

#[test]
fn const_qualifies_a_pointer_after_its_star() {
    assert_eq!(spell(<ConstPtr<Char> as CType>::TYPE), "const char *");
    assert_eq!(
        spell(<ConstPtr<Ptr<Char>> as CType>::TYPE),
        "char * const *"
    );
}

#[test]
fn a_typedef_follows_the_typedefs_it_names() {
    let base = kit_header("Kit/KitBase.h");
    let pair = kit_header("Kit/KitPair.h");
    assert!(base.contains("typedef long KitIndex;"));
    assert!(pair.contains("#include <Kit/KitBase.h>"));
    assert!(
        pair.contains("typedef struct {\n    KitIndex first;\n    const void *second;\n} KitPair;")
    );
}

#[test]
fn declarations_are_spelled_as_c() {
    let pair = kit_header("Kit/KitPair.h");
    assert!(pair.contains("extern KitIndex KitPairFirst(const KitPair *pair);"));
    assert!(pair.contains("extern void KitPairReset(const KitPair *pair);"));
    assert!(pair.contains("extern const KitPair kKitEmptyPair;"));
    assert!(kit_header("Kit/KitBase.h").contains("    kKitNotFound = -1,"));
}

#[test]
fn the_umbrella_includes_every_header() {
    let umbrella = kit_header("Kit/Kit.h");
    assert!(umbrella.contains("#include <Kit/KitBase.h>\n#include <Kit/KitPair.h>"));
    assert!(umbrella.contains("#ifndef __KIT_KIT__"));
}

#[test]
fn a_declaration_outside_the_framework_is_refused() {
    const FOREIGN: Api = Api {
        fragments: &[Fragment {
            header: "Other/Other.h",
            text: "",
        }],
        ..KIT
    };
    assert_eq!(
        headers(&FOREIGN),
        Err(Error::ForeignHeader {
            item: "fragment",
            header: "Other/Other.h"
        })
    );
}

#[test]
fn an_enum_constant_wider_than_int_is_refused() {
    const WIDE: &[Enum] = &[Enum {
        header: "Kit/KitBase.h",
        ty: <Long as CType>::TYPE,
        cases: &[("kWide", 1 << 40)],
    }];
    const API: Api = Api { enums: WIDE, ..KIT };
    assert_eq!(
        headers(&API),
        Err(Error::EnumOutOfRange {
            case: "kWide",
            value: 1 << 40
        })
    );
}

#[test]
fn a_declared_symbol_must_be_exported() {
    let exported: BTreeSet<String> = ["_KitPairFirst", "_kKitEmptyPair"].map(String::from).into();
    assert_eq!(
        check_exported(&KIT, &exported),
        Err(Error::NotExported {
            symbol: "_KitPairReset".into()
        })
    );
}

#[test]
fn a_stub_lists_its_symbols_quoted() {
    let symbols: BTreeSet<String> = ["_A", "_OBJC_CLASS_$_B"].map(String::from).into();
    let text = stub("/K", "1", &symbols);
    assert!(text.starts_with("--- !tapi-tbd\ntbd-version: 4\n"));
    assert!(text.contains("symbols: [ '_A', '_OBJC_CLASS_$_B' ]"));
}

#[test]
fn void_is_not_a_value_type() {
    assert_eq!(
        value_type(&Type::Builtin("void"), "x"),
        Err(Error::VoidValue { item: "x" })
    );
}
