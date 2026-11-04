mod common;

/// The executable asks `RTLD_NEXT` for what its dependencies export, as Roblox
/// does for `objc_autoreleasePoolPush` from a static initializer and then calls
/// the answer unchecked.
const RTLD_NEXT_C: &str = r#"
#include <dlfcn.h>
#include <stdio.h>

static void *(*pool_push)(void);
static int found_at_init;

__attribute__((constructor)) static void early(void) {
    pool_push = (void *(*)(void))dlsym(RTLD_NEXT, "objc_autoreleasePoolPush");
    found_at_init = pool_push != NULL;
}

int main(void) {
    printf("objc at init %d\n", found_at_init);
    printf("objc %d\n", dlsym(RTLD_NEXT, "objc_autoreleasePoolPush") != NULL);
    printf("libSystem %d\n", dlsym(RTLD_NEXT, "strlen") != NULL);
    return 0;
}
"#;

/// Built once: the tests share the output path.
fn rtld_next_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| common::build_x86_64("dlsym_rtld_next", RTLD_NEXT_C, &["-lobjc"]))
        .clone()
}

fn assert_rtld_next_finds_everything(env: &[(&str, &str)]) {
    let Some(guest) = rtld_next_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = common::run_guest_env(env, &guest, &[]);
    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(
        out, "objc at init 1\nobjc 1\nlibSystem 1\n",
        "stderr: {err}"
    );
}

/// Every dependency is a library this runtime is.
#[test]
fn rtld_next_reaches_a_builtin_dependency() {
    assert_rtld_next_finds_everything(&[]);
}

/// libobjc and libSystem are the host's own, which the host's loader bound.
#[cfg(target_os = "macos")]
#[test]
fn rtld_next_reaches_a_host_dependency() {
    assert_rtld_next_finds_everything(&[("ROSEMARY_PREFIX", "off"), ("ROSEMARY_LIBC", "native")]);
}
