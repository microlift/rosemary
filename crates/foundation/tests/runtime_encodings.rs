// Every Foundation method encoding **installed in a live class**, read back out
// of the runtime, against three rules.
//
// What is new here, and what is not:
//
// * **New: the population.** `crates/foundation/src/encoding_check.rs` is
//   opt-in per row — `assert_row` is called by a test a person wrote next to
//   the row, so a row nobody remembered is a row nobody checked, and nothing
//   in the tree notices the gap. This walks every installed method of every
//   class and asks about all of them, so the population is the runtime's.
// * **New: the other two rules.** `assert_row` checks the colon count and the
//   offsets against `8 * i`; it does **not** check that the frame covers its
//   last argument's *slot*, and it does not distinguish a width it measured
//   from one it derived. Both are here.
// * **Not new: reading the runtime.** `assert_installed` already reads
//   `Method::types()`, and it is right to. This does the same and the coverage
//   is the difference.
//
// The rules themselves, the `slot` table, and the measurement that justifies
// them are in `test/tools/objc-runtime-census.rs` (this file `include!`s it) and
// `test/tools/objcenc-frame.py`. The `FRAME` rule's premise — that **0** of the
// guest's own 2951 declarations fail it — is measured there, not asserted here.

#![allow(dead_code, unused_imports, unused_variables)]

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../test/tools/objc-runtime-census.rs"
));

/// Every class `foundation::bootstrap()` installs.
///
/// `foundation::CLASSES` is the crate's own list, so it is the same list the
/// crate believes in — which is a limitation, not a strength, and is why
/// `every_class_the_crate_names_is_registered` is a separate test: a name in
/// this list that does not resolve would otherwise reduce the census silently.
fn class_names() -> Vec<&'static str> {
    foundation::CLASSES.to_vec()
}

/// The census. `bootstrap` first, so the methods exist to be read.
#[test]
fn every_installed_encoding_is_a_declaration_a_compiler_emits() {
    foundation::bootstrap();
    let names = class_names();
    assert_census(&names, 40, 200);
}

/// The census's own population, asserted. A class list that has drifted is
/// invisible from inside a census that skips what it cannot find, so the
/// resolution is checked here rather than inferred.
#[test]
fn every_class_the_crate_names_is_registered_after_bootstrap() {
    foundation::bootstrap();
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
        "{} of the {} class name(s) in `foundation::CLASSES` do not resolve after \
         `bootstrap()`, so their methods were never walked: {missing:?}",
        missing.len(),
        names.len()
    );
}

/// **The positive control**, and the reason the other test means anything.
///
/// A guard that has never been seen red is not a guard. This installs three
/// deliberately wrong rows — the exact string this session shipped, a
/// frame-one-byte-short row, and an overlapping-offsets row — into a real class
/// in a real runtime, and requires the census to reject each one for the reason
/// it was built.
///
/// The first is the interesting one. `v32@0:8@16@?24` on a **one**-colon
/// selector is a *self-consistent* encoding: the frame is `last_offset + 8`, the
/// offsets are a regular ladder, and a rule that asked "is this string
/// internally coherent?" would pass it. Its unit test passed throughout, because
/// the test sent the same four registers. Only the selector's **name** — a
/// property of a string, needing no binary and no author — says three of those
/// words are wrong.
#[test]
fn the_census_can_go_red() {
    foundation::bootstrap();
    assert_controls_can_go_red();
}

/// What a green run of the test above still does not establish.
///
/// Stated as a test because a limit nobody reads is a limit nobody has. Each
/// clause is a way this session's second-worst bug - `-floatValue` wired to an
/// `f64` return under a `d16` encoding, answering `0.0` for almost every input -
/// passed a green suite: the encoding is *well formed*, so every rule here says
/// it is fine, and **a method encoding is never read at dispatch**, so nothing
/// at runtime could notice.
#[test]
fn what_this_census_cannot_see_is_written_down_here() {
    // 1. The `IMP` behind a correct-looking encoding. A `d16` return where the
    //    IMP reads `eax` is a perfect encoding with the wrong unit under it, and
    //    no rule in this file can see it: the rules are about declarations.
    // 2. A class this list does not name. `objc::class` publishes `lookup` and
    //    `Class::own_methods` but no enumeration of registered classes, so the
    //    census's population is a maintained list. The method count in
    //    `assert_census` is the only evidence it still covers the runtime.
    // 3. Anything about behaviour. Every finding here is a static property of a
    //    string, which is why a value/answer confusion - the class this project
    //    treats as worst - is invisible to all of it.

    // The one that is checkable here, and is: the census does not see a class
    // whose name is missing, and that is measurable.
    foundation::bootstrap();
    let names = class_names();
    let (_rejected, _unverified, checked, _missing) = census(&names);
    assert!(
        checked > 0,
        "the census walked no methods, so the other test's population is empty"
    );
}
