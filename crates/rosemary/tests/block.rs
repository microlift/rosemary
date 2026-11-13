mod common;

use common::run_guest_env;

fn run_builtin(path: &str) -> (Option<i32>, String, String) {
    run_guest_env(&[("ROSEMARY_OBJC", "builtin")], path, &[])
}

#[test]
fn a_guest_copies_a_block_and_it_still_works() {
    let Some(path) = common::block_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_builtin(&path);
    assert_eq!(status, Some(0), "stdout: {out}\nstderr: {err}");
    assert!(
        out.contains("captured 42\n"),
        "the copy did not bring the capture with it: {out}\nstderr: {err}"
    );
    assert!(
        out.contains("byref 3\n"),
        "the stack block and its copy did not share one __block variable: \
         {out}\nstderr: {err}"
    );
    assert!(
        out.contains("global same 1\n"),
        "a global block was copied rather than left alone: {out}\nstderr: {err}"
    );
    assert!(out.contains("global ran\n"), "stdout: {out}\nstderr: {err}");
}

#[test]
fn copying_a_block_copies_the_blocks_it_captured() {
    let Some(path) = common::block_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_builtin(&path);
    assert_eq!(status, Some(0), "stdout: {out}\nstderr: {err}");
    assert!(
        out.contains("nested 7\n"),
        "the captured block did not survive the copy: {out}\nstderr: {err}"
    );
    assert!(
        out.contains("held 9\n"),
        "the __block block variable did not survive the copy: {out}\nstderr: {err}"
    );
}

/// And it agrees with the same program built for the host, which is the only
/// specification of libclosure worth having.
#[test]
fn the_guest_agrees_with_the_same_program_built_for_the_host() {
    let Some(guest) = common::block_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let Some(native) = common::build_native_with("block_native", common::BLOCK_C, &[]) else {
        skip!("cannot build native fixtures")
    };
    let (_, guest_out, err) = run_builtin(&guest);
    let Some((_, native_out)) = common::run_native(&native) else {
        skip!("the native fixture would not run")
    };
    assert_eq!(guest_out, native_out, "stderr: {err}");
}
