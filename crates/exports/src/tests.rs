use crate::{Exports, Module, NonNull, Recording};

extern "C" fn one() -> u32 {
    1
}

extern "C" fn two() -> u32 {
    2
}

mod leaf {
    use super::{one, two};

    crate::exports! {
        functions {
            "leaf_one" => one,
            /// An alias group: one implementation, several spellings a guest
            /// may import it by.
            "leaf_two" | "leaf_two_np" | "leaf_two_deprecated" => two,
            #[cfg(any())]
            "leaf_never" => one,
            #[cfg(not(any()))]
            "leaf_always" => one,
        }
        data {
            "_leaf_datum" => 0xdead_beef,
            "_OBJC_CLASS_$_Leaf" => 0x1000,
        }
        null_data {
            "__leaf_empty_vtable",
            #[cfg(any())]
            "__leaf_never_null",
        }
        constants {
            "leaf_constant" => 0x2000,
        }
    }
}

mod other {
    use super::two;

    crate::exports! {
        functions {
            "other_one" => two,
        }
    }
}

/// A module whose functions are C declarations rather than export rows.
mod declared {
    use super::one;
    use crate::decl::UInt;

    crate::c_functions! {
        "Declared/Declared.h" {
            fn declared_one() -> UInt = one;
        }
    }
}

mod group {
    crate::group! { super::leaf, super::other, super::declared }
}

#[test]
fn every_listed_name_resolves() {
    let missing: crate::Names = leaf::names()
        .into_iter()
        .filter(|n| leaf::symbol(n).is_none())
        .collect();
    assert!(missing.is_empty(), "listed but unresolved: {missing:?}");
}

#[test]
fn an_alias_group_resolves_to_one_address() {
    let canonical = leaf::symbol("leaf_two").expect("the canonical spelling");
    for alias in ["leaf_two_np", "leaf_two_deprecated"] {
        assert_eq!(leaf::symbol(alias), Some(canonical), "{alias}");
    }
    assert_ne!(canonical, leaf::symbol("leaf_one").expect("a second entry"));
}

#[test]
fn a_cfg_gated_entry_is_absent_from_names_and_from_symbol() {
    assert!(!leaf::names().contains(&"leaf_never"));
    assert_eq!(leaf::symbol("leaf_never"), None);
    // The other side of the same gate, so that a gate which excluded everything
    // would not pass this test by accident.
    assert!(leaf::names().contains(&"leaf_always"));
    assert!(leaf::symbol("leaf_always").is_some());
}

#[test]
fn a_published_function_carries_the_mach_o_underscore() {
    let mut out = Recording::default();
    leaf::provide(&mut out);
    let published = out.symbols();
    assert!(published.contains(&"_leaf_one"), "{published:?}");
    assert!(!published.contains(&"leaf_one"), "{published:?}");
}

/// A data name is published exactly as written: `_OBJC_CLASS_$_Leaf` has no
/// bare C spelling to derive one from.
#[test]
fn a_published_data_symbol_keeps_the_spelling_it_was_given() {
    let mut out = Recording::default();
    leaf::provide(&mut out);
    let names: alloc::vec::Vec<&str> = out.data.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        ["_leaf_datum", "_OBJC_CLASS_$_Leaf", "_leaf_constant"]
    );
}

/// Data is published but is not part of the function lookup a `dlsym` uses.
#[test]
fn a_data_symbol_is_not_a_name_symbol_answers() {
    assert_eq!(leaf::symbol("_leaf_datum"), None);
    assert!(!leaf::names().contains(&"_leaf_datum"));
}

/// A constant is at a fixed address, so answering it twice - once to the loader
/// and once to a `dlsym` - gives the same one. That is what lets it be in both.
#[test]
fn a_constant_is_published_and_answered() {
    let mut out = Recording::default();
    leaf::provide(&mut out);
    assert!(out.symbols().contains(&"_leaf_constant"));
    assert_eq!(leaf::symbol("leaf_constant"), Some(0x2000));
    assert!(leaf::names().contains(&"leaf_constant"));
}

/// Null on purpose is its own call, so a sink never confuses it with a zero
/// address that should have been something.
#[test]
fn null_data_is_published_as_null_and_not_as_data() {
    let mut out = Recording::default();
    leaf::provide(&mut out);
    assert_eq!(out.nulls, ["__leaf_empty_vtable"]);
    assert!(!out.data.iter().any(|(n, _)| n == "__leaf_empty_vtable"));
}

#[test]
fn null_data_is_listed_but_not_answered() {
    assert_eq!(leaf::null_data(), ["__leaf_empty_vtable"]);
    assert_eq!(group::null_data(), ["__leaf_empty_vtable"]);
    assert_eq!(leaf::symbol("__leaf_empty_vtable"), None);
    assert!(!leaf::names().contains(&"__leaf_empty_vtable"));
}

#[test]
fn a_group_answers_every_members_name() {
    let missing: crate::Names = group::names()
        .into_iter()
        .filter(|n| group::symbol(n).is_none())
        .collect();
    assert!(missing.is_empty(), "listed but unresolved: {missing:?}");
    assert_eq!(group::symbol("leaf_one"), leaf::symbol("leaf_one"));
    assert_eq!(group::symbol("other_one"), other::symbol("other_one"));
    assert_eq!(group::symbol("nothing_defines_this"), None);
}

#[test]
fn a_group_publishes_every_members_symbols() {
    let mut out = Recording::default();
    group::provide(&mut out);
    let published = out.symbols();
    for expected in ["_leaf_one", "_leaf_datum", "_other_one"] {
        assert!(published.contains(&expected), "{expected}: {published:?}");
    }
}

#[test]
fn a_null_address_is_not_published() {
    let mut recorded = Recording::default();
    {
        let mut out = NonNull(&mut recorded);
        out.function("_present", 0x1000);
        out.function("_absent", 0);
        out.data("_absent_datum", 0);
        out.null("_declared_null");
    }
    assert_eq!(recorded.symbols(), ["_present", "_declared_null"]);
}

#[test]
fn a_denied_symbol_is_not_published() {
    fn deny(name: &str) -> bool {
        name == "_withheld"
    }

    let mut recorded = Recording::default();
    {
        let mut out = crate::Denying {
            inner: &mut recorded,
            deny,
        };
        out.function("_kept", 0x1000);
        out.function("_withheld", 0x2000);
        out.null("_withheld");
    }
    assert_eq!(recorded.symbols(), ["_kept"]);
}

/// The invariant the crates above depend on: the address a link gets and the
/// address a `dlsym` gets are the same one, because both come from this table.
#[test]
fn a_published_address_is_the_one_symbol_answers() {
    let mut out = Recording::default();
    group::provide(&mut out);
    for (mach_o, address) in &out.functions {
        let bare = mach_o.strip_prefix('_').expect("the Mach-O underscore");
        assert_eq!(group::symbol(bare), Some(*address), "{mach_o}");
    }
}

#[test]
fn a_declaration_is_its_own_export_row() {
    let mut out = Recording::default();
    declared::provide(&mut out);
    let address = declared::symbol("declared_one").expect("answered by name");
    assert_eq!(out.functions, [("_declared_one".into(), address)]);
    assert_eq!(declared::names(), ["declared_one"]);
}

#[test]
fn a_module_row_answers_the_same_as_the_module() {
    let row: Module = leaf::MODULE;
    assert!(row.path.ends_with("leaf"), "{}", row.path);
    assert_eq!((row.symbol)("leaf_one"), leaf::symbol("leaf_one"));
    assert_eq!((row.names)(), leaf::names());

    // A group is a row too, which is what lets one nest inside another.
    let nested: Module = group::MODULE;
    assert_eq!((nested.symbol)("other_one"), other::symbol("other_one"));
}
