mod common;

use common::{exceptions_fixture, run_guest, run_native, run_native_with, unwind_refusals_fixture};

/// This file's fixture, or why there is not one.
///
/// Not one reason for both ways of not having a path. A fixture that will not
/// build is a different fact from a runtime that will not run one, and until
/// `test/logs/test-hygiene.md` they shared a message - `"cannot build x86_64
/// C++ fixtures"` - which is how a build failure and a runtime limitation both
/// arrived as eleven silent `ok`s. The build now succeeds on a host with
/// `clang` and `ld64.lld`; the runtime still refuses, and
/// [`unwinding_refused_here`] is what says so.
fn exceptions() -> Result<String, String> {
    exceptions_fixture().ok_or_else(|| {
        "cannot build the x86_64 C++ fixture EXCEPTIONS_CPP on this host".to_string()
    })
}

fn refusals() -> Result<String, String> {
    unwind_refusals_fixture().ok_or_else(|| {
        "cannot build the x86_64 C++ fixture UNWIND_REFUSALS_CPP on this host".to_string()
    })
}

/// The runtime's own refusal, if it has one - which today it does, and the
/// eleven tests below are therefore specifications rather than runs.
///
/// Read out of a real run by [`common::cxx_unwinding_refusal`], so this gate
/// opens by itself the day the `__eh_frame` half of the M5 gap is closed. Until
/// then each of the eleven says, on stderr, in the terminal, exactly what is
/// missing - and `tests/unwind.rs::cxx_exceptions_are_refused_rather_than_run`
/// is the test that fails when that stops being true.
fn unwinding_refused_here() -> Option<String> {
    common::cxx_unwinding_refusal()
}

/// What the fixture prints when unwinding works, and what the same binary prints
/// natively. Every line is a separate claim; see [`common::EXCEPTIONS_CPP`].
const EXPECTED: &str = "\
~level3
~level2
~level1
caught Boom 6
~Boom(6)
outer caught inner
caught something
same frame 11
by value by value
base caught 4
std my error
rethrowing 1
finally 1
~Boom(1)
~thread
thread caught 7
~Boom(7)
thread returned 1
done
";

/// The wall this file was gated on, and what happened to it.
///
/// `cxx_exceptions_are_refused_rather_than_run` used to live here, pinning the
/// one thing this file could not otherwise say: that a C++ `throw` was refused
/// by name, because `rosetta3::unwind` parsed only the compact encodings in
/// `__unwind_info` and `clang -target x86_64-apple-darwin` emits DWARF CFI in
/// `__eh_frame` (M5). It was written to **panic the day the wall closed**, and
/// it did: on 2026-09-27 at 22:5x the eleven specifications below stopped
/// skipping, `cxx_unwinding_refusal()` began returning `None`, and the pin
/// failed. It has been removed, which is the outcome it existed to bring about,
/// and what is left is the eleven tests and the gate that opens by itself.
///
/// The gate stays: it reads the runtime's own sentence out of a run rather than
/// carrying a constant, so on a host where the wall is still up - one without
/// the fix - it still says so, out loud, once per test, instead of reporting
/// eleven passes.

#[test]
fn a_guest_throw_is_caught_and_the_program_carries_on() {
    let path = match exceptions() {
        Ok(path) => path,
        Err(why) => skip!("{why}"),
    };
    if let Some(refusal) = unwinding_refused_here() {
        skip!("{refusal}")
    }
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(out, EXPECTED, "stderr: {err}");
}

#[test]
fn the_translated_program_agrees_with_the_untranslated_one() {
    let path = match exceptions() {
        Ok(path) => path,
        Err(why) => skip!("{why}"),
    };
    if let Some(refusal) = unwinding_refused_here() {
        skip!("{refusal}")
    }
    let Some((native_status, native_out)) = run_native(&path) else {
        skip!("cannot run x86_64 binaries natively")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(
        (status, out.as_str()),
        (native_status, native_out.as_str()),
        "stderr: {err}"
    );
    assert_eq!(
        native_out, EXPECTED,
        "the fixture's own expectations have drifted"
    );
}

#[test]
fn destructors_run_in_order_before_the_handler() {
    // The whole of phase two in one assertion. A runtime that entered the
    // handler directly would print the cleanups late, in the wrong order, or -
    // most likely - not at all, and the array's own order is the claim.
    let path = match exceptions() {
        Ok(path) => path,
        Err(why) => skip!("{why}"),
    };
    if let Some(refusal) = unwinding_refused_here() {
        skip!("{refusal}")
    }
    let (_, out, err) = run_guest(&path, &[]);
    let lines: Vec<&str> = out.lines().collect();

    assert_eq!(
        &lines[..5],
        &["~level3", "~level2", "~level1", "caught Boom 6", "~Boom(6)"],
        "stderr: {err}"
    );
}

#[test]
fn a_handler_is_chosen_by_type_and_not_by_position() {
    // The nested `try` throws `Other` past a `catch (const Boom &)`. A runtime
    // that entered the first landing pad it found would print the BUG line the
    // fixture plants there, and everything after it would still look right.
    let path = match exceptions() {
        Ok(path) => path,
        Err(why) => skip!("{why}"),
    };
    if let Some(refusal) = unwinding_refused_here() {
        skip!("{refusal}")
    }
    let (_, out, err) = run_guest(&path, &[]);

    assert!(
        !out.contains("BUG:"),
        "the wrong clause ran:\n{out}\n(stderr: {err})"
    );
    assert!(out.contains("outer caught inner"), "{out:?}");
}

#[test]
fn a_base_class_clause_catches_a_derived_type() {
    // Both directions of the single-inheritance walk: a base defined in the
    // guest, and `std::exception`.
    //
    // The second is now declared by `tests/fixtures/include/exception` rather
    // than by libc++, and the comment that used to say otherwise - "`type_info`
    // is the host's and reached through the guest's own import of it" - was
    // written for a host with a macOS SDK. It is **not** what happens here, and
    // the reason is measured: declaring `std::exception` with a key function
    // makes the fixture import `_ZTVSt9exception` and `_ZTISt9exception`, the
    // vendored `libc++abi.dylib` exports neither, and both land on
    // `abi/table.rs:3941`, which publishes `_ZTVSt9exception` as *data* - a
    // zeroed refusal the test would have called through. So the header defines
    // the class in the fixture, and what this assertion now proves is a
    // **guest-defined** base matching. `throw Derived(4)` above it is the same
    // mechanism and is unchanged; what is lost is the cross-image typeinfo
    // match, and only that. `test/logs/test-hygiene.md` has the run.
    let path = match exceptions() {
        Ok(path) => path,
        Err(why) => skip!("{why}"),
    };
    if let Some(refusal) = unwinding_refused_here() {
        skip!("{refusal}")
    }
    let (_, out, err) = run_guest(&path, &[]);

    assert!(out.contains("base caught 4"), "{out:?} (stderr: {err})");
    assert!(out.contains("std my error"), "{out:?} (stderr: {err})");
}

#[test]
fn a_rethrown_exception_survives_the_handler_that_caught_it() {
    // `throw;` continues the same exception outward. The failure this catches is
    // freeing it at the end of the handler that rethrew: the outer `catch` would
    // then bind to memory that had been returned, and would usually still print
    // the right number.
    let path = match exceptions() {
        Ok(path) => path,
        Err(why) => skip!("{why}"),
    };
    if let Some(refusal) = unwinding_refused_here() {
        skip!("{refusal}")
    }
    let (_, out, err) = run_guest(&path, &[]);
    let lines: Vec<&str> = out.lines().collect();
    let at = lines
        .iter()
        .position(|l| *l == "rethrowing 1")
        .expect("the rethrow ran");

    assert_eq!(
        &lines[at..at + 3],
        &["rethrowing 1", "finally 1", "~Boom(1)"],
        "stderr: {err}"
    );
}

#[test]
fn a_thread_unwinds_its_own_stack() {
    // Per-thread in-flight state. Two walks sharing one would be a race in a
    // fixture with one thread throwing, and a corruption in any real program.
    let path = match exceptions() {
        Ok(path) => path,
        Err(why) => skip!("{why}"),
    };
    if let Some(refusal) = unwinding_refused_here() {
        skip!("{refusal}")
    }
    let (_, out, err) = run_guest(&path, &[]);

    assert!(out.contains("thread caught 7"), "{out:?} (stderr: {err})");
    assert!(
        out.contains("thread returned 1"),
        "the start routine's value: {out:?}"
    );
}

#[test]
fn an_uncaught_exception_terminates_exactly_as_it_does_natively() {
    // C++ requires `std::terminate` with the stack *intact*, which is the whole
    // reason the search is a separate phase. `~uncaught` printing here would
    // mean the runtime unwound first and asked afterwards.
    //
    // Compared against the same binary run natively rather than against a
    // message this project chose. That is now possible and was not before: the
    // guest runs a **real** `libc++abi`, so what it prints when nothing catches
    // is what the platform prints - `libc++abi: terminating due to uncaught
    // exception of type Boom`, down to the type name - and the exit status is
    // the same `SIGABRT`. The earlier version of this test asserted a
    // rosetta3-authored diagnostic containing "terminate"; the real one says
    // "terminating", which is how the improvement first showed up as a failure.
    let path = match refusals() {
        Ok(path) => path,
        Err(why) => skip!("{why}"),
    };
    if let Some(refusal) = unwinding_refused_here() {
        skip!("{refusal}")
    }
    let Some((native_status, expected)) = run_native_with(&path, &["uncaught"]) else {
        skip!("cannot run x86_64 binaries natively")
    };
    let (status, out, err) = run_guest(&path, &["uncaught"]);

    assert_eq!(out, expected, "stderr: {err}");
    assert_eq!(status, native_status, "stderr: {err}");
    assert!(
        err.contains("terminating due to uncaught exception"),
        "the stop should be libc++abi's own: {err}"
    );
    assert!(
        !out.contains("~uncaught"),
        "a destructor ran before terminate:\n{out}"
    );
    assert!(
        !out.contains("after"),
        "execution continued past the throw:\n{out}"
    );
}

#[test]
fn a_throw_across_a_host_frame_is_still_not_caught() {
    let path = match refusals() {
        Ok(path) => path,
        Err(why) => skip!("{why}"),
    };
    if let Some(refusal) = unwinding_refused_here() {
        skip!("{refusal}")
    }
    let (status, out, err) = run_guest(&path, &["across"]);

    assert_ne!(status, Some(0), "{out}");
    assert!(
        !out.contains("caught across"),
        "it cannot actually have been caught:\n{out}"
    );
    // Named as the *current* diagnosis, so that improving it fails here and is
    // looked at rather than passing silently. **It changed on 2026-09-27** and
    // this is the second time: it said libc++abi's
    // `terminating due to uncaught exception`, and when the `__eh_frame` half of
    // M5 was closed the refusal became the runtime's own, and *more* precise -
    // it now names which frame stopped it rather than reporting that nothing
    // caught the exception:
    //
    //     __cxa_throw: not callable: an exception thrown out of a callback the
    //     host called, across host frames the runtime cannot unwind yet (M5)
    //
    // So the test's *claim* is unchanged and still true - the throw really is
    // not caught, `caught across` is still absent, the status is still
    // non-zero - and what moved is which of the two remaining M5 walls it lands
    // on. `AcrossHostFrame` is a different wall from `NeedsDwarf`, and it is
    // named at `crates/rosetta3/src/unwind.rs:131`.
    assert!(
        err.contains("across host frames the runtime cannot unwind yet"),
        "the refusal should still be the runtime's own, got: {err}"
    );
}

#[test]
fn a_catch_needing_multiple_inheritance_works() {
    // A base subobject at a non-zero offset has to move the pointer the `catch`
    // binds to. This project's own unwinder could not, and **refused by name**
    // rather than reporting "nothing catches this" - which would have been a lie
    // of the most expensive kind, terminating a program whose handler is right
    // there in the source.
    //
    // Vendoring a real `libc++abi` closed it: the offset adjustment is exactly
    // the sort of thing the Itanium ABI's own implementation does correctly, and
    // this test now asserts the clause runs. It is the clearest single win from
    // running the C++ runtime rather than reimplementing it, and it is checked
    // against the native run rather than against a claim.
    let path = match refusals() {
        Ok(path) => path,
        Err(why) => skip!("{why}"),
    };
    if let Some(refusal) = unwinding_refused_here() {
        skip!("{refusal}")
    }
    let Some((native_status, expected)) = run_native_with(&path, &["multiple"]) else {
        skip!("cannot run x86_64 binaries natively")
    };
    let (status, out, err) = run_guest(&path, &["multiple"]);

    assert_eq!(out, expected, "stderr: {err}");
    assert_eq!(status, native_status, "stderr: {err}");
    assert!(out.contains("multiple inheritance"), "{out}");
}

#[test]
fn the_slow_import_policy_unwinds_identically() {
    // `--slow` sends every promotable import through the marshaller instead of
    // inlining it. The unwinder reads the guest's registers at the moment of the
    // throw, so a policy that changed where those registers came from would show
    // up here and nowhere else.
    let path = match exceptions() {
        Ok(path) => path,
        Err(why) => skip!("{why}"),
    };
    if let Some(refusal) = unwinding_refused_here() {
        skip!("{refusal}")
    }
    let (fast_status, fast_out, err) = run_guest(&path, &[]);
    let (slow_status, slow_out, slow_err) =
        common::run_guest_with(&["--slow", "--emulated"], &path, &[]);

    assert_eq!(fast_status, Some(0), "stderr: {err}");
    assert_eq!(slow_status, Some(0), "stderr: {slow_err}");
    assert_eq!(fast_out, slow_out);
}
