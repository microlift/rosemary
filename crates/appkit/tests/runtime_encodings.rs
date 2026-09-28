// Every AppKit method encoding **installed in a live class**, read back out of
// the runtime, against three rules.
//
// Same checker as `crates/foundation/tests/runtime_encodings.rs` - it is one
// file, `test/tools/objc-runtime-census.rs`, `include!`d by both, because two
// copies of a checker is two answers to "what does this rule say" and they will
// differ. The two tests differ only in the classes they walk, because
// `appkit::bootstrap()` calls `foundation::bootstrap()` underneath and the two
// class populations are disjoint in what they add.
//
// What this covers that `crates/appkit/src/encoding_check.rs` does not:
//
// * **Population.** `assert_row` is called by hand, from a test somebody wrote
//   next to a row.  A row nobody remembered is a row nobody checked, and
//   nothing in the tree notices the gap. This walks every installed method of
//   every class AppKit registers.
// * **The frame.** `assert_row` compares the frame against `8 * (2 + colons)`,
//   which cannot be derived for a `char` argument - the guest writes both
//   `v20@0:8c16` and `v24@0:8c16` for the same shape - so it has to *assert* a
//   number the format does not fix. This uses the rule the guest's own 2951
//   declarations fix: `frame >= last_offset + slot(last)`, where a narrow type
//   gets a four-byte slot.  `test/tools/objcenc-frame.py` derives the table and
//   asserts the premise (0 of 2951 fail it).
// * **Widths it cannot measure.** A C++ record with `std::optional` internals
//   has a width this checker derives rather than measures, and it says so
//   instead of checking anyway.  The count of such rows is asserted, so one
//   appearing is visible.

#![allow(dead_code, unused_imports, unused_variables)]

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../test/tools/objc-runtime-census.rs"
));

/// Every class `appkit::bootstrap()` installs.
///
/// `appkit::classes::names()` is the crate's own list, so it is the same list
/// the crate believes in.  That is a limitation, not a strength, and it is why
/// `every_class_the_crate_names_is_registered` is a separate test: a name here
/// that does not resolve would otherwise reduce the census silently.
fn class_names() -> Vec<&'static str> {
    appkit::classes::names()
}

/// The census. `bootstrap` first, so the methods exist to be read.
#[test]
fn every_installed_encoding_is_a_declaration_a_compiler_emits() {
    appkit::bootstrap();
    let names = class_names();
    assert_census(&names, 40, 500);
}

/// The census's own population, asserted.  A class list that has drifted is
/// invisible from inside a census that skips what it cannot find, so the
/// resolution is checked here rather than inferred from a green run.
#[test]
fn every_class_the_crate_names_is_registered_after_bootstrap() {
    appkit::bootstrap();
    let names = class_names();
    let missing: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| {
            std::ffi::CString::new(*n)
                .ok()
                .and_then(|c| objc::class::lookup(&c))
                .is_none()
        })
        .collect();
    assert!(
        missing.is_empty(),
        "{} of the {} class name(s) in `appkit::classes::names()` do not resolve \
         after `bootstrap()`, so their methods were never walked: {missing:?}",
        missing.len(),
        names.len()
    );
}

/// **The positive control.**  A guard that has never been seen red is not a
/// guard.
///
/// This installs three deliberately wrong rows into a real class in a real
/// runtime and requires the census to reject each for the reason it was built.
/// The first is the row this session shipped: `v32@0:8@16@?24` on a
/// **one**-colon selector.  It is a *self-consistent* encoding - the frame is
/// `last + 8`, the offsets are a regular ladder - so a rule asking "is this
/// string internally coherent?" passes it, and its unit test passed throughout
/// because the test sent the same four registers.  Only the selector's name
/// says three of those words are wrong.
#[test]
fn the_census_can_go_red() {
    appkit::bootstrap();
    assert_controls_can_go_red();
}
