//! Every name `darwin` publishes, checked against every answer `darwin` gives.
//!
//! # The defect this exists for
//!
//! `darwin::provider()` and `darwin::symbol()` are two lookups over one list, and
//! they do not agree about which entry wins:
//!
//! * `provider()` is `exports::Module::provide_all(MODULES, …)`, and its sink is a
//!   `SyntheticDylib`, whose `define` is `BTreeMap::insert` - so the **last**
//!   module in `MODULES` holds the published address.
//! * `symbol()` is `exports::Module::resolve(MODULES, …)`, which is
//!   `iter().find_map(…)` - so the **first** module in `MODULES` answers.
//!
//! For every name exactly one module defines, the two are the same address and
//! nothing can go wrong. The moment two modules define one name, the published
//! address and the `dlsym` address are two different functions, and a guest
//! reaches whichever one its mechanism happens to ask for. `_copyfile` was in
//! that state: `darwin::copyfile` (first, so the `dlsym` answer) and
//! `darwin::media_unavailable` (last, so the published answer, and therefore the
//! one every guest import bound to) - and they did not merely duplicate each
//! other, they disagreed about what `COPYFILE_DATA` is. See the note in
//! `media_unavailable.rs`.
//!
//! So this is a **census**, not a test of behaviour: it compares the two
//! lookups for every published name and reports every name where they differ.
//! It needs no access to `darwin`'s private `MODULES`, which is why it is an
//! integration test rather than a `#[cfg(test)] mod` inside the crate - and that
//! is also why it can see a duplicate that `darwin`'s own
//! `no_module_defines_a_name_another_one_does` cannot: **that test `panic!`s on
//! the first duplicate it finds**, and `sched_yield` (`idle` at index 0, `pthread`
//! at index 9) comes before `copyfile` (index 56) and `media_unavailable`
//! (index 122), so it has been reporting the same known failure since
//! `sched_yield` was duplicated and reporting nothing at all about anything
//! after it. A guard that stops at the first hit is not a guard on the rest.

use std::collections::BTreeMap;

use machish::resolve::{SymbolKind, SymbolProvider};

/// Every name `darwin` publishes as a **function**, with the address it is
/// published at.
///
/// Functions only, and that is the whole of the point rather than a
/// convenience. `exports!` leaves ordinary `data` rows out of the generated
/// `symbol()` on purpose - "not every such expression can be evaluated twice and
/// give the same answer" - so a `dlsym` of a data name is *meant* to fall
/// through, and a census that counted those would report 500-odd "split
/// brains" that are the design working. A `functions` row, by contrast, is in
/// both: `provide` writes it and `symbol` matches it, and the two must be the
/// same address or the name has two answers.
///
/// A `constants` row is published as data and so is invisible here. That is a
/// real gap in *this* census and it is stated rather than papered over; the
/// `crates/darwin/tests/published_census.rs` companion the source census fills
/// sees them, because it reads the rows rather than the answers.
fn published() -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    darwin::provider().enumerate(&mut |name, resolved| {
        if resolved.kind == SymbolKind::Function {
            out.insert(name.to_string(), resolved.addr);
        }
    });
    assert!(!out.is_empty(), "darwin published no functions at all");
    out
}

/// Names this crate publishes at two addresses, known and not yet fixed.
///
/// An allowlist rather than a bare assertion, so that the census's verdict does
/// not change when an unrelated fix lands, and so that the known-bad set is a
/// written record instead of a line inside somebody's panic message. Anything
/// *not* on this list still fails.
///
/// `_sched_yield` is the pre-existing one, documented in `test/PLAN.md` §8 and
/// in the `AGENT-BRIEF.md` baseline table: `darwin::idle` and `darwin::pthread`
/// both define it, and `pthread`'s row is `"sched_yield" => libc::sched_yield`
/// while `idle`'s is its own. It is on this list because it is *real*, not
/// because it is harmless: a guest import binds `pthread`'s and a guest
/// `dlsym` gets `idle`'s. `darwin::idle`'s row is the one that should not
/// exist, and removing it is a two-line change in a file this test does not own.
const KNOWN: &[&str] = &["_sched_yield"];

#[test]
fn the_published_address_is_the_address_a_dlsym_answers() {
    let published = published();
    // `darwin::register_provider` is not called here - that is
    // `rosetta3::frameworks::install`'s job and it needs the whole runtime - so
    // `darwin::symbol` falls through to `Module::resolve` and answers from
    // `MODULES` directly. That is the `dlsym` half of the comparison in
    // isolation, which is what makes it checkable from this crate at all.
    let split: Vec<(&str, u64, Option<u64>)> = published
        .iter()
        .filter_map(|(name, address)| {
            let answered = darwin::symbol(name);
            (answered != Some(*address)).then_some((name.as_str(), *address, answered))
        })
        .collect();

    let fresh: Vec<_> = split
        .iter()
        .filter(|(name, _, _)| !KNOWN.contains(name))
        .collect();
    let all: Vec<&str> = split.iter().map(|(name, _, _)| *name).collect();
    assert!(
        fresh.is_empty(),
        "{} name(s) are published at one address and answered at another, so a \
         guest import and a guest `dlsym` of the same symbol reach different \
         functions - a split-brain, and which one a guest gets is decided by \
         how the symbol was resolved rather than by what it means. \
         `provider()` takes the LAST module in `MODULES` (`BTreeMap::insert`) \
         and `symbol()` takes the FIRST (`find_map`).\n\
         new:\n  {fresh:#?}\n\
         all mismatches, `KNOWN` included: {all:?}",
        fresh.len(),
    );
}

#[test]
fn every_published_name_is_a_name_the_guest_could_ask_for() {
    // `exports!` publishes `"_" + row`; a name that does not round-trip is a
    // name the loader can never look up. Cheap, and it is the spelling trap the
    // brief calls out: nothing in `cargo build` or `cargo test` sees it.
    for name in published().keys() {
        assert!(
            name.starts_with('_'),
            "{name} is published without the Mach-O underscore"
        );
    }
}
