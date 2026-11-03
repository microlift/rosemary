mod common;

use common::{build_x86_64, run_guest_env};

const H264_GUEST: &str = include_str!("fixtures/coremedia_h264.c");

/// On a Mac this is Apple's CoreMedia, which makes the fixture's expectations
/// a parity check on the builtin one the same test reaches elsewhere.
#[test]
fn emulated_x86_64_guest_describes_h264_parameter_sets() {
    let Some(path) = build_x86_64(
        "coremedia_h264_emulated",
        H264_GUEST,
        &["-framework", "CoreMedia", "-framework", "CoreFoundation"],
    ) else {
        skip!("cannot build x86_64 Mach-O CoreMedia fixture");
    };
    let prefix = format!("{}-coremedia-x86_64", common::test_prefix());
    let (status, out, err) = run_guest_env(
        &[("ROSEMARY_OBJC", "builtin"), ("ROSEMARY_PREFIX", &prefix)],
        &path,
        &[],
    );
    assert_eq!(status, Some(0), "stdout: {out}\nstderr: {err}");
    assert_eq!(out, "coremedia ok\n", "stderr: {err}");
}
