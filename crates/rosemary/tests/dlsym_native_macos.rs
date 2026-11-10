#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

mod common;

const RTLD_NEXT_FIXTURE: &str = r#"
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>

typedef void *(*push_t)(void);
typedef void (*pop_t)(void *);

__attribute__((constructor)) static void lookup_objc_pool(void) {
    push_t push = (push_t)dlsym(RTLD_NEXT, "objc_autoreleasePoolPush");
    pop_t pop = (pop_t)dlsym(RTLD_NEXT, "objc_autoreleasePoolPop");
    if (!push || !pop) {
        fprintf(stderr, "static RTLD_NEXT Objective-C pool lookup failed\n");
        _Exit(73);
    }
    // Non-pseudo handles from the host loader must keep working.
    void *libobjc = dlopen("/usr/lib/libobjc.A.dylib", RTLD_NOW);
    if (!libobjc || !dlsym(libobjc, "objc_autoreleasePoolPush")) {
        fprintf(stderr, "native dlopen handle lookup failed\n");
        _Exit(74);
    }
    dlclose(libobjc);
    void *pool = push();
    pop(pool);
}

int main(void) {
    puts("native RTLD_NEXT initializer ok");
    return 0;
}
"#;

#[test]
fn native_macos_initializer_resolves_objc_using_rtld_next() {
    let Some(guest) = common::build_native_with(
        "objc_rtld_next_native_macos",
        RTLD_NEXT_FIXTURE,
        &["-arch", "arm64", "-lobjc"],
    ) else {
        panic!("could not compile ARM64 Mach-O fixture");
    };

    let original = std::process::Command::new(&guest)
        .output()
        .expect("run fixture through Apple dyld");
    assert!(
        original.status.success(),
        "native stderr: {}",
        String::from_utf8_lossy(&original.stderr)
    );

    let output = common::driver_command()
        .env("ROSEMARY_PREFIX", "off")
        .env("ROSEMARY_LIBC", "native")
        .env("ROSEMARY_OBJC", "native")
        .arg("run")
        .arg("--native")
        .arg(&guest)
        .output()
        .expect("run ARM64 fixture through Rosemary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(stdout, "native RTLD_NEXT initializer ok\n");
}

// Calls other than guest-originated RTLD_NEXT must still reach Apple's dlsym.
const NATIVE_DLSYM_PASSTHROUGH_FIXTURE: &str = r#"
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>

typedef void *(*dlsym_fn)(void *, const char *);
int main(void) {
    void *objc = dlopen("/usr/lib/libobjc.A.dylib", RTLD_NOW);
    if (!objc) return 10;
    if (!dlsym(objc, "objc_autoreleasePoolPush")) return 11;
    if (!dlsym(RTLD_DEFAULT, "objc_autoreleasePoolPush")) return 12;
    if (dlsym(RTLD_MAIN_ONLY, "rosemary_missing_symbol_minimal")) return 13;
    if (!dlerror()) return 14;
    if (dlerror()) return 15;

    void *libsystem = dlopen("/usr/lib/libSystem.B.dylib", RTLD_NOW);
    if (!libsystem) return 16;
    dlsym_fn indirect = (dlsym_fn)dlsym(libsystem, "dlsym");
    if (!indirect) return 17;
    if (!indirect(objc, "objc_autoreleasePoolPop")) return 18;
    if (!indirect(RTLD_DEFAULT, "objc_autoreleasePoolPop")) return 19;
    if (dlclose(libsystem) != 0 || dlclose(objc) != 0) return 20;
    puts("native host-handle and indirect dlsym passthrough ok");
    return 0;
}
"#;

#[test]
fn native_macos_non_guest_scoped_dlsym_matches_apple_loader() {
    let guest = common::build_native_with(
        "objc_native_host_dlsym_passthrough",
        NATIVE_DLSYM_PASSTHROUGH_FIXTURE,
        &["-arch", "arm64", "-lobjc"],
    )
    .expect("compile ARM64 native dlsym passthrough fixture");
    let apple = std::process::Command::new(&guest)
        .output()
        .expect("run through Apple dyld");
    assert!(
        apple.status.success(),
        "Apple: {:?} stderr {}",
        apple.status,
        String::from_utf8_lossy(&apple.stderr)
    );
    let rosemary = common::driver_command()
        .env("ROSEMARY_PREFIX", "off")
        .env("ROSEMARY_LIBC", "native")
        .env("ROSEMARY_OBJC", "native")
        .args(["run", "--native"])
        .arg(&guest)
        .output()
        .expect("run through Rosemary");
    assert!(
        rosemary.status.success(),
        "Rosemary: {:?} stdout {} stderr {}",
        rosemary.status,
        String::from_utf8_lossy(&rosemary.stdout),
        String::from_utf8_lossy(&rosemary.stderr)
    );
    assert_eq!(rosemary.stdout, apple.stdout);
}

const NATIVE_RTLD_NEXT_MISS: &str = r#"
#include <dlfcn.h>
#include <stdio.h>
int main(void) {
    dlerror();
    if (dlsym(RTLD_NEXT, "rosemary_intentionally_missing_rtld_next")) return 30;
    if (!dlerror()) return 31;
    if (dlerror()) return 32;
    puts("native RTLD_NEXT miss preserves host dlerror");
    return 0;
}
"#;

#[test]
fn native_macos_unresolved_rtld_next_uses_host_error_contract() {
    let guest = common::build_native_with(
        "objc_native_rtld_next_missing",
        NATIVE_RTLD_NEXT_MISS,
        &["-arch", "arm64"],
    )
    .expect("compile RTLD_NEXT failure fixture");
    let apple = std::process::Command::new(&guest)
        .output()
        .expect("Apple dyld");
    assert!(
        apple.status.success(),
        "Apple: {:?} stderr {}",
        apple.status,
        String::from_utf8_lossy(&apple.stderr)
    );
    let rosemary = common::driver_command()
        .env("ROSEMARY_PREFIX", "off")
        .env("ROSEMARY_LIBC", "native")
        .env("ROSEMARY_OBJC", "native")
        .args(["run", "--native"])
        .arg(&guest)
        .output()
        .expect("Rosemary");
    assert!(
        rosemary.status.success(),
        "Rosemary: {:?} stderr {}",
        rosemary.status,
        String::from_utf8_lossy(&rosemary.stderr)
    );
    assert_eq!(rosemary.stdout, apple.stdout);
}

// Host `dlsym` interposition is an existing Rosemary provider behavior, not
// necessarily the same as Apple loading this standalone executable directly.
// Check that this fix does not override the selected host implementation.
const HOST_DLSYM_INTERPOSER: &str = r#"
#include <dlfcn.h>
#include <string.h>
void *dlsym(void *handle, const char *name) {
    (void)handle;
    return name && strcmp(name, "rosemary_dlsym_guard_sentinel") == 0
        ? (void *)0x12345678 : NULL;
}
"#;

const HOST_DLSYM_INTERPOSER_MAIN: &str = r#"
#include <dlfcn.h>
#include <stdio.h>
int main(void) {
    if (dlsym(RTLD_DEFAULT, "rosemary_dlsym_guard_sentinel") != (void *)0x12345678)
        return 40;
    puts("existing host dlsym interposition preserved");
    return 0;
}
"#;

#[test]
fn native_macos_host_dlsym_interposer_is_not_overridden() {
    let interposer = common::build_native_with(
        "librosemary_dlsym_guard_interposer.dylib",
        HOST_DLSYM_INTERPOSER,
        &["-arch", "arm64", "-dynamiclib"],
    )
    .expect("compile host dlsym interposer");
    let guest = common::build_native_with(
        "objc_native_dlsym_interpose_guard",
        HOST_DLSYM_INTERPOSER_MAIN,
        &["-arch", "arm64"],
    )
    .expect("compile guest dlsym interposition fixture");
    let rosemary = common::driver_command()
        .env("ROSEMARY_PREFIX", "off")
        .env("ROSEMARY_LIBC", "native")
        .env("ROSEMARY_OBJC", "native")
        .env("DYLD_INSERT_LIBRARIES", &interposer)
        .args(["run", "--native"])
        .arg(&guest)
        .output()
        .expect("run guest with host dlsym interposer");
    assert!(
        rosemary.status.success(),
        "Rosemary: {:?}, stdout {} stderr {}",
        rosemary.status,
        String::from_utf8_lossy(&rosemary.stdout),
        String::from_utf8_lossy(&rosemary.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&rosemary.stdout),
        "existing host dlsym interposition preserved\n"
    );
}
