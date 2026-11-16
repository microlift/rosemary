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

/// The executable opens itself by its own path, as Roblox does, and looks one of
/// its own globals up through the handle.
const DLOPEN_SELF_C: &str = r#"
#include <dlfcn.h>
#include <mach-o/dyld.h>
#include <stdint.h>
#include <stdio.h>

__attribute__((visibility("default"))) int self_marker = 41;

int main(void) {
    char path[4096];
    uint32_t size = sizeof path;
    if (_NSGetExecutablePath(path, &size) != 0) return 2;
    void *self = dlopen(path, RTLD_NOW);
    if (!self) return 3;
    int *found = (int *)dlsym(self, "self_marker");
    printf("same %d\n", found == &self_marker);
    self_marker += 1;
    printf("value %d\n", found ? *found : -1);
    return 0;
}
"#;

#[test]
fn an_executable_that_opens_itself_gets_itself_not_a_copy() {
    let Some(guest) = common::build_x86_64("dlopen_self", DLOPEN_SELF_C, &[]) else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = common::run_guest_env(&[], &guest, &[]);
    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(out, "same 1\nvalue 42\n", "stderr: {err}");
}

const LINKED_DEP_C: &str = r#"
__attribute__((visibility("default"))) int dep_marker = 41;
"#;

/// The executable opens a library it was linked against, by the path the image
/// list reports for it, as Unity Doorstop opens UnityPlayer to hook it.
const DLOPEN_LINKED_C: &str = r#"
#include <dlfcn.h>
#include <mach-o/dyld.h>
#include <stdio.h>
#include <string.h>

extern int dep_marker;

int main(void) {
    const char *path = NULL;
    uint32_t before = _dyld_image_count();
    for (uint32_t i = 0; i < before; i++) {
        const char *name = _dyld_get_image_name(i);
        if (name && strstr(name, "dlopen_linked_dep")) path = name;
    }
    if (!path) return 2;
    void *dep = dlopen(path, RTLD_NOLOAD | RTLD_LAZY);
    if (!dep) return 3;
    int *found = (int *)dlsym(dep, "dep_marker");
    printf("same %d\n", found == &dep_marker);
    printf("count %d\n", _dyld_image_count() == before);
    printf("close %d\n", dlclose(dep));
    return 0;
}
"#;

#[test]
fn opening_a_linked_library_gets_the_loaded_image() {
    let Some(dep) = common::build_x86_64("dlopen_linked_dep", LINKED_DEP_C, &["-dynamiclib"])
    else {
        skip!("cannot build x86_64 fixtures")
    };
    let Some(guest) = common::build_x86_64("dlopen_linked", DLOPEN_LINKED_C, &[&dep]) else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = common::run_guest_env(&[], &guest, &[]);
    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(out, "same 1\ncount 1\nclose 0\n", "stderr: {err}");
}

