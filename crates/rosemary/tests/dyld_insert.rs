mod common;

const INSERTED: &str = r#"
extern int puts(const char *);
__attribute__((constructor)) static void init(void) { puts("inserted"); }
"#;

const MAIN: &str = r#"
extern int puts(const char *);
int main(void) { puts("main"); return 0; }
"#;

#[test]
fn host_variable_inserts_a_library() {
    let Some(lib) = common::build_x86_64("insert_lib", INSERTED, &["-dynamiclib"]) else {
        return;
    };
    let Some(main) = common::build_x86_64("insert_main", MAIN, &[]) else {
        return;
    };
    let (_, out, _) =
        common::run_guest_env(&[("ROSEMARY_DYLD_INSERT_LIBRARIES", &lib)], &main, &[]);
    assert_eq!(out, "inserted\nmain\n");
}

#[test]
fn inserts_a_library_whose_path_has_spaces() {
    let Some(lib) = common::build_x86_64("insert_spaced_lib", INSERTED, &["-dynamiclib"]) else {
        return;
    };
    let Some(main) = common::build_x86_64("insert_spaced_main", MAIN, &[]) else {
        return;
    };
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("insert with spaces");
    std::fs::create_dir_all(&dir).expect("create spaced dir");
    let spaced = dir.join("insert lib.dylib");
    std::fs::copy(&lib, &spaced).expect("copy library");
    let (_, out, _) = common::run_guest_env(
        &[("ROSEMARY_DYLD_INSERT_LIBRARIES", spaced.to_str().unwrap())],
        &main,
        &[],
    );
    assert_eq!(out, "inserted\nmain\n");
}

#[test]
fn guest_variable_is_the_one_that_counts() {
    let Some(lib) = common::build_x86_64("insert_lib", INSERTED, &["-dynamiclib"]) else {
        return;
    };
    let Some(main) = common::build_x86_64("insert_main", MAIN, &[]) else {
        return;
    };
    // The host's spelling is not the guest's, so it inserts nothing.
    let (_, out, _) = common::run_guest_env(&[("DYLD_INSERT_LIBRARIES", &lib)], &main, &[]);
    assert_eq!(out, "main\n");
    let line = format!("DYLD_INSERT_LIBRARIES={lib}");
    let (_, out, _) = common::run_guest_env(&[("ROSEMARY_ENVIRON", &line)], &main, &[]);
    assert_eq!(out, "inserted\nmain\n");
}

const LINKED: &str = r#"
int linked_value(void) { return 7; }
"#;

const FINDS_LINKED: &str = r#"
#include <mach-o/dyld.h>
#include <string.h>
extern int puts(const char *);
__attribute__((constructor)) static void init(void) {
    for (unsigned i = 0; i < _dyld_image_count(); i++) {
        const char *name = _dyld_get_image_name(i);
        if (name && strstr(name, "insert_listed_dep") && _dyld_get_image_header(i)) {
            puts("found");
            return;
        }
    }
    puts("missing");
}
"#;

const CALLS_LINKED: &str = r#"
extern int puts(const char *);
extern int linked_value(void);
int main(void) { return linked_value() == 7 ? puts("main"), 0 : 1; }
"#;

/// A library inserted ahead of the program finds the program's own dependencies
/// in dyld's image list from its initializer, as Unity Doorstop looks for
/// UnityPlayer to hook.
#[test]
fn inserted_library_sees_the_programs_dependencies() {
    let Some(dep) = common::build_x86_64("insert_listed_dep", LINKED, &["-dynamiclib"]) else {
        return;
    };
    let Some(lib) = common::build_x86_64("insert_lister", FINDS_LINKED, &["-dynamiclib"]) else {
        return;
    };
    let Some(main) = common::build_x86_64("insert_listed_main", CALLS_LINKED, &[&dep]) else {
        return;
    };
    let (_, out, err) =
        common::run_guest_env(&[("ROSEMARY_DYLD_INSERT_LIBRARIES", &lib)], &main, &[]);
    assert_eq!(out, "found\nmain\n", "stderr: {err}");
}
