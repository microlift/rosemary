mod common;

use common::{run_guest, run_native};

/// Sorted lines, so a test can assert on a set that four racing threads produce
/// in an arbitrary order.
fn lines(text: &str) -> Vec<&str> {
    let mut v: Vec<&str> = text.lines().collect();
    v.sort_unstable();
    v
}

#[test]
fn a_thread_local_reads_its_own_value_and_not_a_neighbours() {
    let Some(path) = common::tls_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    // 11 from `__thread_data`, 33 stored into a `__thread_bss` variable, 22 from
    // `__thread_data` again at a non-zero offset, and both ends of a 100-byte
    // array - an off-by-one block base gets at most one of these right.
    assert_eq!(out, "11 33 22 z 0\n", "stderr: {err}");
}

#[test]
fn four_threads_get_four_independent_thread_local_counters() {
    let Some(path) = common::thread_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(
        lines(&out),
        vec![
            "joined 0 -> 1000",
            "joined 1 -> 1001",
            "joined 2 -> 1002",
            "joined 3 -> 1003",
            "main counter 0",
            "thread 0 counter 10",
            "thread 1 counter 10",
            "thread 2 counter 10",
            "thread 3 counter 10",
        ],
        "stderr: {err}"
    );
}

#[test]
fn a_join_returns_the_guest_start_routines_own_value() {
    let Some(path) = common::thread_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (_, out, err) = run_guest(&path, &[]);

    // `counter * 100 + id`: the `id` half can only be right if the thunk passed
    // the guest its `void *` argument, and the `counter` half only if the start
    // routine ran against its own thread-local block. A runtime that dropped the
    // return value would print zeros.
    for id in 0..4 {
        assert!(
            out.contains(&format!("joined {id} -> 100{id}\n")),
            "join {id} lost the start routine's value: {out:?} (stderr: {err})"
        );
    }
    // And no thread saw another's count.
    assert_eq!(out.matches("counter 10\n").count(), 4, "{out:?}");
    assert!(
        out.contains("main counter 0\n"),
        "the main thread shared a block: {out:?}"
    );
}

#[test]
fn a_guest_thread_sees_its_own_errno() {
    let Some(path) = common::errno_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    // 2 is ENOENT. The main thread's `errno` must be untouched by the failure on
    // the spawned one, which is the whole content of "errno is thread-local and
    // passthrough is correct".
    assert!(
        out.contains("worker fd=-1 errno=2 (No such file or directory)\n"),
        "{out:?} (stderr: {err})"
    );
    assert!(
        out.contains("main errno still 0\n"),
        "{out:?} (stderr: {err})"
    );
}

#[test]
fn mutexes_once_and_thread_specific_destructors_all_work() {
    let Some(path) = common::pthread_sync_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    // 4000 exactly: a lost update under a mutex that is not really held would
    // give something smaller, essentially every run.
    assert!(
        out.contains("shared=4000 inits=1\n"),
        "{out:?} (stderr: {err})"
    );
    for id in 1..=4 {
        assert!(out.contains(&format!("tsd {id}\n")), "{out:?}");
        assert!(
            out.contains(&format!("tsd destructor {id}\n")),
            "libpthread never called the guest's key destructor: {out:?}"
        );
    }
}

#[test]
fn a_guest_thread_runs_on_the_stack_its_attributes_asked_for() {
    let Some(path) = common::pthread_stack_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest(&path, &[]);
    assert_eq!(status, Some(0), "stderr: {err}");

    // The main thread is 8 MiB with its guard page inside that, exactly as the
    // kernel gives a real one - so the number a guest reads is the native one
    // rather than one page more.
    assert!(
        out.contains("main: size=8372224 inside=1\n"),
        "{out:?} (stderr: {err})"
    );
    // A null attribute set is not "no size": it is libpthread's defaults, whose
    // size is 512 KiB, and a guest thread gets what the same program would get
    // natively rather than this runtime's idea of a good stack.
    assert!(
        out.contains("default: size=524288 local_inside=1 other_thread_agrees=1\n"),
        "{out:?} (stderr: {err})"
    );
    assert!(
        out.contains("initialised: size=524288 local_inside=1 other_thread_agrees=1\n"),
        "{out:?} (stderr: {err})"
    );
    // The one the guest actually asked for. Before this, the request sized the
    // *host* thread - the one running the JIT - and the guest's own frames went
    // on a default stack it never mentioned.
    assert!(
        out.contains("sized: size=1048576 local_inside=1 other_thread_agrees=1\n"),
        "{out:?} (stderr: {err})"
    );
    // And a stack the guest allocated and handed over is the stack its frames
    // are on, at the address it named.
    assert!(
        out.contains("own: size=262144 local_inside=1 other_thread_agrees=1\n"),
        "{out:?} (stderr: {err})"
    );
    assert!(out.contains("own: supplied=1\n"), "{out:?} (stderr: {err})");
    // Every worker still ran and returned its argument, which is what says the
    // stack it was given is one it could really run on.
    assert_eq!(out.matches("joined 4660\n").count(), 4, "{out:?}");
}

/// The same program, run both ways.
#[test]
fn thread_stacks_agree_with_the_same_binary_run_natively() {
    let Some(path) = common::pthread_stack_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let Some((native_status, native_out)) = run_native(&path) else {
        skip!("this host cannot run an x86_64 binary natively")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, native_status, "stderr: {err}");
    assert_eq!(lines(&out), lines(&native_out), "stderr: {err}");
}

#[test]
fn a_threaded_guest_agrees_with_the_same_binary_run_natively() {
    let Some(path) = common::pthread_sync_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let Some((native_status, native_out)) = run_native(&path) else {
        skip!("this host cannot run an x86_64 binary natively")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, native_status, "stderr: {err}");
    assert_eq!(lines(&out), lines(&native_out), "stderr: {err}");
}

#[test]
fn an_imports_resolved_address_maps_back_to_its_symbol() {
    let Some(path) = common::tls_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let program = rosemary::Program::load(&path, &[]).expect("the fixture links");

    let bootstrap = program
        .report
        .bindings()
        .find(|b| b.symbol.ends_with("tlv_bootstrap"))
        .expect("a program with thread-locals imports a bootstrap");
    assert_ne!(
        bootstrap.resolved, 0,
        "the runtime must supply __tlv_bootstrap itself"
    );

    let site = program
        .import_by_target(bootstrap.resolved)
        .expect("an import must be reachable by the address it resolved to");
    assert_eq!(site.symbol, bootstrap.symbol);

    // And a host address nothing imported still has no answer, which is what
    // keeps `UnknownIndirectCall` meaningful.
    assert!(program.import_by_target(0xdead_beef).is_none());
}

#[test]
fn pthread_types_agree_across_architectures() {
    const SIZES: &str = r#"
#include <pthread.h>
#include <stdio.h>
int main(void) {
    printf("%zu %zu %zu %zu %zu %zu %zu %zu\n",
        sizeof(pthread_t), sizeof(pthread_mutex_t), sizeof(pthread_cond_t),
        sizeof(pthread_rwlock_t), sizeof(pthread_attr_t), sizeof(pthread_mutexattr_t),
        sizeof(pthread_once_t), sizeof(pthread_key_t));
    return 0;
}
"#;
    let Some(guest) = common::build_x86_64("pthread_sizes_x86", SIZES, &[]) else {
        skip!("cannot build x86_64 fixtures")
    };
    let Some(host) = common::build_native("pthread_sizes_arm", SIZES) else {
        skip!("cannot build a native fixture")
    };

    // The x86_64 build's sizes come from running it under this runtime, which is
    // also a small end-to-end check that `printf("%zu")` and the loader agree.
    let (_, guest_out, err) = run_guest(&guest, &[]);
    let Some((_, host_out)) = run_native(&host) else {
        skip!("cannot run a native fixture")
    };

    assert_eq!(
        guest_out, host_out,
        "pthread types differ between architectures (stderr: {err})"
    );
}

/// A thread that leaves through `pthread_exit` with cleanup handlers pushed, as
/// Boehm GC's thread start routine does inside Mono.
const CLEANUP_ON_EXIT_C: &str = r#"
#include <pthread.h>
#include <stdio.h>

static void cleanup(void *arg) { printf("cleanup %s\n", (const char *)arg); }

static void *body(void *arg) {
    pthread_cleanup_push(cleanup, "outer");
    pthread_cleanup_push(cleanup, "inner");
    pthread_exit((void *)42);
    pthread_cleanup_pop(0);
    pthread_cleanup_pop(0);
    return NULL;
}

int main(void) {
    pthread_t thread;
    void *value = NULL;
    if (pthread_create(&thread, NULL, body, NULL) != 0) return 2;
    pthread_join(thread, &value);
    printf("joined %ld\n", (long)value);
    return 0;
}
"#;

#[test]
fn pthread_exit_runs_the_guests_cleanup_handlers() {
    let Some(path) = common::build_x86_64("thread_cleanup_exit", CLEANUP_ON_EXIT_C, &[]) else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest(&path, &[]);
    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(
        out, "cleanup inner\ncleanup outer\njoined 42\n",
        "stderr: {err}"
    );
}
