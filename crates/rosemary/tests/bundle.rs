mod common;

/// Apple's CallMachOFramework pattern, which Feral's ports use to reach `free`:
/// find the frameworks folder on the "appropriate disk", open
/// `System.framework` in it as a bundle, and look a function up by name.
const SYSTEM_FRAMEWORK_C: &str = r#"
#include <CoreServices/CoreServices.h>
#include <stdio.h>
#include <stdlib.h>

typedef void (*free_fn)(void *);

int main(void) {
    FSRef folder;
    OSErr err = FSFindFolder(kOnAppropriateDisk, kFrameworksFolderType, true, &folder);
    printf("find %d\n", err);
    if (err) return 1;
    CFURLRef base = CFURLCreateFromFSRef(NULL, &folder);
    CFURLRef url = CFURLCreateCopyAppendingPathComponent(
        NULL, base, CFSTR("System.framework"), false);
    CFBundleRef bundle = CFBundleCreate(NULL, url);
    printf("bundle %d\n", bundle != NULL);
    if (!bundle) return 1;
    printf("loaded %d\n", CFBundleLoadExecutable(bundle));
    free_fn f = (free_fn)CFBundleGetFunctionPointerForName(bundle, CFSTR("free"));
    printf("free %d\n", f != NULL);
    if (f) f(malloc(16));
    printf("missing %d\n",
        CFBundleGetFunctionPointerForName(bundle, CFSTR("no_such_function_here")) != NULL);
    CFRelease(bundle);
    CFRelease(url);
    CFRelease(base);
    return 0;
}
"#;

const FLAGS: &[&str] = &[
    "-Wno-deprecated-declarations",
    "-framework",
    "CoreServices",
    "-framework",
    "CoreFoundation",
];

#[test]
fn a_function_is_found_in_system_framework_by_name() {
    let Some(guest) = common::build_x86_64("system_framework", SYSTEM_FRAMEWORK_C, FLAGS) else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = common::run_guest(&guest, &[]);
    assert_eq!(status, Some(0), "stdout: {out}\nstderr: {err}");
    assert_eq!(
        out, "find 0\nbundle 1\nloaded 1\nfree 1\nmissing 0\n",
        "stderr: {err}"
    );
}

#[test]
fn the_system_framework_lookup_agrees_with_the_host() {
    let Some(guest) = common::build_x86_64("system_framework", SYSTEM_FRAMEWORK_C, FLAGS) else {
        skip!("cannot build x86_64 fixtures")
    };
    let Some(native) =
        common::build_native_with("system_framework_native", SYSTEM_FRAMEWORK_C, FLAGS)
    else {
        skip!("cannot build native fixtures")
    };
    let (_, guest_out, err) = common::run_guest(&guest, &[]);
    let Some((_, native_out)) = common::run_native(&native) else {
        skip!("the native fixture would not run")
    };
    assert_eq!(guest_out, native_out, "stderr: {err}");
}

/// The OpenGL-extension pattern, against a framework every guest links: find
/// an Apple framework by identifier, then a variable in it by name.
const BY_IDENTIFIER_C: &str = r#"
#include <CoreFoundation/CoreFoundation.h>
#include <stdio.h>

int main(void) {
    CFStringRef name = CFSTR("com.apple.CoreFoundation");
    CFBundleRef bundle = CFBundleGetBundleWithIdentifier(name);
    printf("bundle %d\n", bundle != NULL);
    if (!bundle) return 1;
    printf("same %d\n", bundle == CFBundleGetBundleWithIdentifier(name));
    printf("identifier %d\n",
        CFStringCompare(CFBundleGetIdentifier(bundle), name, 0) == kCFCompareEqualTo);
    const void *data = CFBundleGetDataPointerForName(bundle, CFSTR("kCFTypeArrayCallBacks"));
    printf("data %d\n", data == &kCFTypeArrayCallBacks);
    printf("missing data %d\n",
        CFBundleGetDataPointerForName(bundle, CFSTR("no_such_variable_here")) != NULL);
    printf("missing bundle %d\n",
        CFBundleGetBundleWithIdentifier(CFSTR("com.example.no-such-bundle")) != NULL);
    return 0;
}
"#;

const BY_IDENTIFIER_FLAGS: &[&str] = &["-framework", "CoreFoundation"];

#[test]
fn a_variable_is_found_in_a_framework_named_by_identifier() {
    let Some(guest) =
        common::build_x86_64("bundle_by_identifier", BY_IDENTIFIER_C, BY_IDENTIFIER_FLAGS)
    else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = common::run_guest(&guest, &[]);
    assert_eq!(status, Some(0), "stdout: {out}\nstderr: {err}");
    assert_eq!(
        out, "bundle 1\nsame 1\nidentifier 1\ndata 1\nmissing data 0\nmissing bundle 0\n",
        "stderr: {err}"
    );
}

#[test]
fn the_identifier_lookup_agrees_with_the_host() {
    let Some(guest) =
        common::build_x86_64("bundle_by_identifier", BY_IDENTIFIER_C, BY_IDENTIFIER_FLAGS)
    else {
        skip!("cannot build x86_64 fixtures")
    };
    let Some(native) = common::build_native_with(
        "bundle_by_identifier_native",
        BY_IDENTIFIER_C,
        BY_IDENTIFIER_FLAGS,
    ) else {
        skip!("cannot build native fixtures")
    };
    let (_, guest_out, err) = common::run_guest(&guest, &[]);
    let Some((_, native_out)) = common::run_native(&native) else {
        skip!("the native fixture would not run")
    };
    assert_eq!(guest_out, native_out, "stderr: {err}");
}
