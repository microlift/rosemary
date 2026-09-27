#![allow(dead_code, clippy::doc_lazy_continuation)]

use std::path::PathBuf;
use std::process::Command;

/// Where compiled fixtures are cached.
fn fixture_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fixtures");
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir
}

/// How a guest fixture is built on this host.
///
/// Two ways, and the order matters: a host that can do it the ordinary way must
/// keep doing it the ordinary way, so a Mac - where `cc -arch x86_64` has a real
/// SDK behind it, so `-framework Foundation` and `<stdio.h>` mean something -
/// gets exactly the fixtures it always got, and only a host with no Mac SDK
/// falls through to the second.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Builder {
    /// A `cc` whose driver takes `-arch x86_64`: Apple's, and a host whose
    /// toolchain is built to emit Mach-O. Unchanged from before this existed.
    Cc,
    /// `test/tools/build-macho.sh`: `clang -target x86_64-apple-darwin` for the
    /// compile and the rustup toolchain's `ld64.lld` for the link.
    ///
    /// This is the whole of why the directory used to be silent. Probing
    /// `cc -arch x86_64` on a host with no Mac toolchain does not fail to find
    /// a compiler - it finds gcc, which has no `-arch` option at all, and the
    /// answer was false. So *every* fixture-based test in `crates/rosetta3/
    /// tests/` was green because it never ran, which is the third way this
    /// project's suite has lied and the one PLAN.md §1 calls a wrong answer
    /// that looks like success.
    MachOScript,
}

/// `test/tools/build-macho.sh`, if this checkout has it.
fn build_macho_script() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test/tools/build-macho.sh");
    path.is_file().then_some(path)
}

/// Whether `cc` here is a driver that takes `-arch x86_64`.
///
/// Probed with a *link*, not a compile: `clang` accepts `-arch x86_64` and then
/// finds no linker for the target, so a compile-only probe would say yes on a
/// host that cannot produce a Mach-O.
fn cc_builds_x86_64(src: &std::path::Path, out: &std::path::Path) -> bool {
    // `stderr` to null: this probe's job is to answer a question, and on a host
    // with no Mach-O toolchain the answer is always no, so its `cc: error:
    // unrecognized command-line option '-arch'` would otherwise be printed once
    // per test binary into the middle of a real test run. A fixture that fails
    // to build still says so - the probe is the only thing silenced.
    Command::new("cc")
        .args(["-arch", "x86_64", "-o"])
        .arg(out)
        .arg(src)
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && host_macho(&out.to_string_lossy())
}

/// Whether [`build_macho_script`]` can build one here.
///
/// Also checked with a link and the magic number, for the same reason, and
/// because a script that exists is not a script that works: it needs a `clang`
/// that targets Darwin *and* an `ld64.lld`, and the second is reached through a
/// symlink because the linker dispatches on `argv[0]`.
fn script_builds_x86_64(src: &std::path::Path, out: &std::path::Path) -> bool {
    let Some(script) = build_macho_script() else {
        return false;
    };
    Command::new(script)
        .arg(src)
        .arg(out)
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && host_macho(&out.to_string_lossy())
}

/// How a fixture is built here, or `None` if this host cannot.
fn builder() -> Option<Builder> {
    static WHICH: std::sync::OnceLock<Option<Builder>> = std::sync::OnceLock::new();
    *WHICH.get_or_init(|| {
        let dir = fixture_dir();
        let src = dir.join("probe.c");
        std::fs::write(&src, "int main(void){return 0;}").ok()?;
        // Probed one after the other and not with `||`, because which one works
        // is the answer: a host where `cc` takes the flag must not be answered
        // for by the script, which has no SDK behind it.
        let out = dir.join("probe_cc");
        if cc_builds_x86_64(&src, &out) {
            return Some(Builder::Cc);
        }
        let out = dir.join("probe_macho");
        script_builds_x86_64(&src, &out).then_some(Builder::MachOScript)
    })
}

/// Whether this host can build x86_64 Mach-O binaries.
pub fn can_build_x86_64() -> bool {
    builder().is_some()
}

/// Compile `source` into an x86_64 Mach-O and return its path.
pub fn build_x86_64(name: &str, source: &str, extra: &[&str]) -> Option<String> {
    let builder = builder()?;
    let dir = fixture_dir();
    let src = dir.join(format!("{name}.c"));
    let out = dir.join(name);
    std::fs::write(&src, source).ok()?;

    let status = match builder {
        Builder::Cc => Command::new("cc")
            .args(["-arch", "x86_64"])
            .args(extra)
            .arg("-o")
            .arg(&out)
            .arg(&src)
            .status()
            .ok()?,
        // The script's own order is `source out [extra clang args...]`, and it
        // puts the extra flags ahead of the input - so a `-x c++` that arrives
        // last, as `cc` would have accepted it, is still seen before the file.
        Builder::MachOScript => Command::new(build_macho_script()?)
            .arg(&src)
            .arg(&out)
            .args(extra)
            .status()
            .ok()?,
    };
    status.success().then(|| out.to_string_lossy().into_owned())
}

pub const HELLO_C: &str = r#"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

static void say_goodbye(void) { puts("goodbye"); }

int main(int argc, char **argv) {
    const char *who = argc > 1 ? argv[1] : "world";
    char buf[64];
    atexit(say_goodbye);
    snprintf(buf, sizeof buf, "%s (%zu)", who, strlen(who));
    printf("Hello, %s! %d %.2f\n", buf, argc, 1.5);
    puts("done");
    return abs(argc) - 1;
}
"#;

pub fn hello() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("hello", HELLO_C, &[]))
        .clone()
}

pub const LINK_C: &str = r#"
#include <stdio.h>
#include <stdlib.h>

long __attribute__((noinline)) step(long a) { return a * 3 + 1; }
long __attribute__((noinline)) other(long a) { return a ^ 0x5a5a; }

static long (* volatile through_a_pointer)(long) = other;

int main(int argc, char **argv) {
    long n = argc > 1 ? atol(argv[1]) : 400000;
    long acc = 1;
    for (long i = 0; i < n; i++) {
        acc = step(acc);
        acc = through_a_pointer(acc);
        acc &= 0xffffff;
    }
    printf("acc=%ld\n", acc);
    return 0;
}
"#;

pub const POLYLINK_C: &str = r#"
#include <stdio.h>
#include <stdlib.h>

long __attribute__((noinline)) a(long x) { return x * 3 + 1; }
long __attribute__((noinline)) b(long x) { return x ^ 0x5a5a; }
long __attribute__((noinline)) c(long x) { return x + 7; }
long __attribute__((noinline)) d(long x) { return x - 11; }

long __attribute__((noinline)) fan_out(long x, int k) {
    if (k == 0) return a(x);
    if (k == 1) return b(x);
    if (k == 2) return c(x);
    return d(x);
}

long __attribute__((noinline)) leaf(long x) { return (x | 1) * 5; }

long __attribute__((noinline)) ladder(long x) {
    for (int i = 0; i < 16; i++) {
        if (x & 1) x = x * 3 + 1; else x >>= 1;
        if (x & 2) x ^= 0x2b; else x += 3;
    }
    return x;
}

int main(int argc, char **argv) {
    long n = argc > 1 ? atol(argv[1]) : 200000;
    long acc = 1;
    for (long i = 0; i < n; i++) {
        acc = fan_out(acc, (int)(i & 3));
        acc = leaf(acc) + leaf(acc >> 1) + leaf(acc >> 2) + leaf(acc >> 3);
        acc = ladder(acc);
        acc &= 0xffffff;
    }
    printf("acc=%ld\n", acc);
    return 0;
}
"#;

pub const QSORT_C: &str = r#"
#include <stdio.h>
#include <stdlib.h>

static int calls = 0;

static int by_int(const void *a, const void *b) {
    calls++;
    int x = *(const int *)a, y = *(const int *)b;
    return (x > y) - (x < y);
}

static void bye(void) { puts("bye"); }

int main(void) {
    int v[8] = {5, 3, 9, 1, 7, 2, 8, 4};
    atexit(bye);
    qsort(v, 8, sizeof(int), by_int);
    for (int i = 0; i < 8; i++) printf("%d", v[i]);
    printf(" calls=%s\n", calls > 0 ? "yes" : "no");
    return 0;
}
"#;

pub const CTOR_C: &str = r#"
#include <stdio.h>

static char order[8];
static int n = 0;

__attribute__((constructor(101))) static void first(void)  { order[n++] = 'a'; }
__attribute__((constructor(102))) static void second(void) { order[n++] = 'b'; }
__attribute__((constructor(103))) static void third(void)  { order[n++] = 'c'; }

int main(void) {
    order[n] = 0;
    printf("order=%s\n", order);
    return n == 3 ? 0 : 1;
}
"#;

pub const DTOR_C: &str = r#"
#include <stdio.h>
#include <stdlib.h>

static void plain(void) { puts("atexit"); }

__attribute__((destructor)) static void late(void) { puts("destructor"); }

int main(void) {
    atexit(plain);
    puts("main");
    return 0;
}
"#;

pub const LATE_CALLBACK_C: &str = r#"
#include <stdio.h>
#include <stdlib.h>

static int by_value(const void *a, const void *b) {
    int x = *(const int *)a, y = *(const int *)b;
    return (x > y) - (x < y);
}

static void late(void) {
    int v[5] = {4, 2, 5, 1, 3};
    qsort(v, 5, sizeof v[0], by_value);
    printf("late=%d%d%d%d%d\n", v[0], v[1], v[2], v[3], v[4]);
}

int main(void) {
    atexit(late);
    puts("main");
    return 0;
}
"#;

pub const THREAD_C: &str = r#"
#include <pthread.h>
#include <stdio.h>

_Thread_local int counter = 0;

static void *worker(void *arg) {
    long id = (long)arg;
    for (int i = 0; i < 10; i++) counter++;
    printf("thread %ld counter %d\n", id, counter);
    return (void *)(long)(counter * 100 + id);
}

int main(void) {
    pthread_t t[4];
    for (long i = 0; i < 4; i++) pthread_create(&t[i], 0, worker, (void *)i);
    for (long i = 0; i < 4; i++) {
        void *r = 0;
        pthread_join(t[i], &r);
        printf("joined %ld -> %ld\n", i, (long)r);
    }
    printf("main counter %d\n", counter);
    return 0;
}
"#;

pub const ERRNO_C: &str = r#"
#include <pthread.h>
#include <stdio.h>
#include <errno.h>
#include <fcntl.h>
#include <string.h>

static void *worker(void *arg) {
    (void)arg;
    int fd = open("/nonexistent/definitely/not/here", O_RDONLY);
    printf("worker fd=%d errno=%d (%s)\n", fd, errno, strerror(errno));
    return 0;
}

int main(void) {
    int fd = open("/dev/null", O_RDONLY);
    printf("main fd ok=%d errno=%d\n", fd >= 0, errno);
    pthread_t t;
    pthread_create(&t, 0, worker, 0);
    pthread_join(t, 0);
    printf("main errno still %d\n", errno);
    return 0;
}
"#;

pub const OPEN_C: &str = r#"
#include <stdio.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include <string.h>
#include <semaphore.h>

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    umask(0);

    char path[1024];
    strcpy(path, argv[1]);
    strcat(path, "/created");
    unlink(path);
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL, 0604);
    if (fd < 0) { perror("open"); return 1; }
    close(fd);
    printf("open ok\n");

    // The same call without `O_CREAT`, whose third argument is not a mode at
    // all - it must not reach the kernel, and the file it opens must keep the
    // bits the call above gave it.
    fd = open(path, O_RDONLY, 0777);
    if (fd < 0) { perror("reopen"); return 1; }
    close(fd);
    printf("reopen ok\n");

    int dirfd = open(argv[1], O_RDONLY);
    if (dirfd < 0) { perror("opendir"); return 1; }
    fd = openat(dirfd, "created-at", O_WRONLY | O_CREAT | O_EXCL, 0641);
    if (fd < 0) { perror("openat"); return 1; }
    close(fd);
    close(dirfd);
    printf("openat ok\n");

    // `sem_open` is the same rule with the same flag and *two* optional
    // arguments. The count is what says they arrived: a semaphore created with
    // 2 gives two `sem_trywait`s and refuses the third.
    char name[32];
    snprintf(name, sizeof name, "/r3sem%d", getpid());
    sem_unlink(name);
    sem_t *sem = sem_open(name, O_CREAT | O_EXCL, 0600, 2);
    if (sem == SEM_FAILED) { perror("sem_open"); return 1; }
    printf("sem %d%d%d\n",
           sem_trywait(sem) == 0, sem_trywait(sem) == 0, sem_trywait(sem) == 0);
    sem_close(sem);
    sem_unlink(name);
    return 0;
}
"#;

pub const PTHREAD_SYNC_C: &str = r#"
#include <pthread.h>
#include <stdio.h>

static pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
static pthread_once_t once = PTHREAD_ONCE_INIT;
static pthread_key_t key;
static int shared = 0;
static int inits = 0;

static void init_once(void) { inits++; }
static void destroy(void *p) { printf("tsd destructor %ld\n", (long)p); }

static void *worker(void *arg) {
    pthread_once(&once, init_once);
    pthread_setspecific(key, (void *)((long)arg + 1));
    for (int i = 0; i < 1000; i++) {
        pthread_mutex_lock(&m);
        shared++;
        pthread_mutex_unlock(&m);
    }
    return (void *)(long)pthread_getspecific(key);
}

int main(void) {
    pthread_key_create(&key, destroy);
    pthread_t t[4];
    for (long i = 0; i < 4; i++) pthread_create(&t[i], 0, worker, (void *)i);
    for (int i = 0; i < 4; i++) { void *r; pthread_join(t[i], &r); printf("tsd %ld\n", (long)r); }
    printf("shared=%d inits=%d\n", shared, inits);
    return 0;
}
"#;

pub const PTHREAD_STACK_C: &str = r#"
#include <pthread.h>
#include <stdio.h>
#include <stdint.h>
#include <sys/mman.h>

static pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cv = PTHREAD_COND_INITIALIZER;
/* 0: worker running, 1: worker has reported, 2: main is done asking */
static int state = 0;
static void *w_addr;
static size_t w_size;
static int w_local_inside;

static int inside(uintptr_t p, void *addr, size_t size) {
    return p < (uintptr_t)addr && p >= (uintptr_t)addr - size;
}

static void *worker(void *arg) {
    int local = 0;
    void *addr = pthread_get_stackaddr_np(pthread_self());
    size_t size = pthread_get_stacksize_np(pthread_self());
    pthread_mutex_lock(&m);
    w_addr = addr;
    w_size = size;
    w_local_inside = inside((uintptr_t)&local, addr, size);
    state = 1;
    pthread_cond_broadcast(&cv);
    while (state != 2) pthread_cond_wait(&cv, &m);
    pthread_mutex_unlock(&m);
    return arg;
}

static void probe(const char *what, pthread_attr_t *a, void *supplied, size_t supplied_size) {
    pthread_t t;
    state = 0;
    if (pthread_create(&t, a, worker, (void *)0x1234) != 0) {
        printf("%s: create failed\n", what);
        return;
    }
    pthread_mutex_lock(&m);
    while (state != 1) pthread_cond_wait(&cv, &m);
    pthread_mutex_unlock(&m);

    /* From another thread entirely, by pthread_t. */
    void *other_addr = pthread_get_stackaddr_np(t);
    size_t other_size = pthread_get_stacksize_np(t);
    printf("%s: size=%zu local_inside=%d other_thread_agrees=%d\n", what, w_size,
           w_local_inside, other_addr == w_addr && other_size == w_size);
    if (supplied) {
        printf("%s: supplied=%d\n", what,
               w_addr == (void *)((char *)supplied + supplied_size) && w_size == supplied_size);
    }

    pthread_mutex_lock(&m);
    state = 2;
    pthread_cond_broadcast(&cv);
    pthread_mutex_unlock(&m);
    void *r = 0;
    pthread_join(t, &r);
    printf("%s: joined %ld\n", what, (long)r);
}

int main(void) {
    int local = 0;
    void *addr = pthread_get_stackaddr_np(pthread_self());
    printf("main: size=%zu inside=%d\n", pthread_get_stacksize_np(pthread_self()),
           inside((uintptr_t)&local, addr, pthread_get_stacksize_np(pthread_self())));

    probe("default", 0, 0, 0);

    pthread_attr_t a;
    pthread_attr_init(&a);
    probe("initialised", &a, 0, 0);
    pthread_attr_setstacksize(&a, 1024 * 1024);
    probe("sized", &a, 0, 0);
    pthread_attr_destroy(&a);

    size_t size = 256 * 1024;
    void *mem = mmap(0, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    if (mem == MAP_FAILED) {
        printf("no mapping\n");
        return 1;
    }
    pthread_attr_t b;
    pthread_attr_init(&b);
    pthread_attr_setstack(&b, mem, size);
    probe("own", &b, mem, size);
    pthread_attr_destroy(&b);
    return 0;
}
"#;

pub const TLS_C: &str = r#"
#include <stdio.h>

_Thread_local int a = 11;
_Thread_local int b;
_Thread_local long c = 22;
_Thread_local char pad[100];

int main(void) {
    b = 33;
    pad[99] = 'z';
    printf("%d %d %ld %c %d\n", a, b, c, pad[99], pad[0]);
    return 0;
}
"#;

pub const TLV_PLUGIN_C: &str = r#"
_Thread_local int plug_a = 77;
_Thread_local long plug_b;

int plug_sum(void) { plug_b += 1; return plug_a + (int)plug_b; }
"#;

pub const TLV_ROUTES_C: &str = r#"
#include <dlfcn.h>
#include <pthread.h>
#include <stdio.h>

_Thread_local int a = 11;
_Thread_local long c = 22;

/* The descriptor as the ABI lays it out, under the same asm name the
   `_Thread_local` above emits - so `desc_a.thunk` is the very word the
   compiler's own `callq *(%rdi)` loads. */
struct tlv_descriptor {
    void *(*thunk)(struct tlv_descriptor *);
    unsigned long key;
    unsigned long offset;
};
extern struct tlv_descriptor desc_a __asm__("_a");

static void *(*volatile indirect)(struct tlv_descriptor *);

static void *worker(void *arg) {
    (void)arg;
    a += 100;
    c += 200;
    printf("worker a=%d c=%ld\n", a, c);
    return NULL;
}

int main(void) {
    printf("main a=%d c=%ld\n", a, c);

    pthread_t t;
    pthread_create(&t, NULL, worker, NULL);
    pthread_join(t, NULL);
    /* The worker wrote its own block, so this thread's values are untouched. */
    printf("after a=%d c=%ld\n", a, c);

    indirect = desc_a.thunk;
    int *p = (int *)indirect(&desc_a);
    printf("byhand a=%d\n", *p);

    void *h = dlopen("libtlvplugin.dylib", RTLD_NOW);
    if (!h) { printf("dlopen failed: %s\n", dlerror()); return 1; }
    int (*plug_sum)(void) = (int (*)(void))dlsym(h, "plug_sum");
    if (!plug_sum) { printf("dlsym failed: %s\n", dlerror()); return 2; }
    printf("plug %d %d\n", plug_sum(), plug_sum());
    return 0;
}
"#;

/// Build [`TLV_ROUTES_C`] and its plugin beside it, or `None` if this host
/// cannot.
pub fn tlv_routes_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            build_x86_64(
                "libtlvplugin.dylib",
                TLV_PLUGIN_C,
                &["-dynamiclib", "-install_name", "@rpath/libtlvplugin.dylib"],
            )?;
            build_x86_64("tlv_routes", TLV_ROUTES_C, &[])
        })
        .clone()
}

pub const SETJMP_C: &str = r#"
#include <setjmp.h>
#include <stdio.h>
#include <string.h>

static jmp_buf env;
static sigjmp_buf senv;

/* File scope, not locals: a non-volatile local modified between `setjmp` and
   `longjmp` has an indeterminate value afterwards - C says so - and a fixture
   whose expected output depends on undefined behaviour tests the compiler, not
   the runtime. */
static int trips = 0, total = 0;

static void level3(int v) { printf("level3\n"); longjmp(env, v); }
static void level2(int v) { printf("level2\n"); level3(v); printf("BUG: level2 returned\n"); }
static void level1(int v) { printf("level1\n"); level2(v); printf("BUG: level1 returned\n"); }

static void plain(void)  { _longjmp(env, 9); }
static void masked(void) { siglongjmp(senv, 11); }

int main(void) {
    char guard[256];
    memset(guard, 'g', sizeof guard);
    guard[255] = 0;

    int v = setjmp(env);
    if (v == 0) {
        printf("first return\n");
        level1(42);
        printf("BUG: setjmp fell through\n");
    }
    trips++;
    total += v;
    printf("came back with %d (trip %d)\n", v, trips);
    if (v == 42) level1(0);

    if (_setjmp(env) == 0) plain();
    else printf("plain spelling ok\n");

    if (sigsetjmp(senv, 1) == 0) masked();
    else printf("masked spelling ok\n");

    printf("guard %c%c len %zu trips %d total %d\n",
           guard[0], guard[254], strlen(guard), trips, total);
    return 0;
}
"#;

/// Build [`SETJMP_C`], or `None` if this host cannot.
pub fn setjmp_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("setjmp", SETJMP_C, &[]))
        .clone()
}

pub const LONGJMP_ACROSS_C: &str = r#"
#include <setjmp.h>
#include <stdio.h>
#include <stdlib.h>

static jmp_buf env;

static int by_int(const void *a, const void *b) {
    if (*(const int *)a == 9) longjmp(env, 5);
    return 0;
}

int main(void) {
    int v[4] = {5, 9, 1, 7};
    if (setjmp(env) == 0) {
        qsort(v, 4, sizeof(int), by_int);
        printf("no jump\n");
    } else {
        printf("jumped out of the comparator\n");
    }
    return 0;
}
"#;

/// Build [`LONGJMP_ACROSS_C`], or `None` if this host cannot.
pub fn longjmp_across_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("longjmp_across", LONGJMP_ACROSS_C, &[]))
        .clone()
}

/// Where the freestanding declarations for a Darwin target on an SDK-less host
/// live, if this checkout has them.
///
/// `None` on a host that does not need them, because a host that has an SDK
/// must use it: a real `<cstdio>` is the platform's, and a fixture built against
/// these would be testing this repository's declarations instead of macOS's.
/// `tests/fixtures/include`, absolute, as the compiler sees this crate.
const FREESTANDING_INCLUDE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/include");

fn freestanding_include() -> Option<&'static str> {
    match builder() {
        Some(Builder::MachOScript) => Some(FREESTANDING_INCLUDE),
        Some(Builder::Cc) | None => None,
    }
}

/// A C++ fixture, with the freestanding declarations prepended when this host
/// has no SDK to supply the C++ headers from.
///
/// `-std=gnu++17` stated rather than left to clang's default, so a fixture that
/// compiles is compiling as the language it is written in: they all use
/// `noexcept` and `override`, and a toolchain whose default were C++98 would
/// turn that into an error rather than a working build.
///
/// `-nostdlibinc` and **not** `-nostdinc`: the first drops this host's glibc
/// headers - `/usr/include/stdio.h:191` is a hard error for a Darwin target,
/// `__nonnull` being a glibc annotation the Darwin `sys/cdefs.h` does not
/// define - and the second would also drop clang's own resource directory,
/// which is where `stdarg.h` and friends live. What is *left* on the search
/// path is this directory, so a fixture asking for a header these do not
/// provide - `<string>`, `<mutex>`, `<stdexcept>` - still fails, which is the
/// answer it should get: a freestanding `std::string` would be this
/// repository's reimplementation wearing the standard's name. See
/// `tests/fixtures/include/freestanding.h`.
pub fn build_cpp(name: &str, source: &str) -> Option<String> {
    let mut flags = vec!["-x", "c++", "-std=gnu++17"];
    match freestanding_include() {
        // A host with no SDK, so `-lc++` is **dropped** rather than satisfied
        // by whatever `clang` finds: it would link this host's libstdc++/libc++
        // into a Mach-O, which is a different library wearing the name, and it
        // fails anyway with `ld64.lld: error: library not found for -lc++`.
        // The fixture's `__cxa_*` and `operator new` imports are left to
        // `-undefined dynamic_lookup`, and at load time they reach the vendored
        // `libc++abi.dylib` that `cxxlib` installs at
        // `/usr/lib/libc++abi.1.dylib` - which is the library a real guest
        // binds, so this is the *same* binding, not a substitute for it.
        Some(include) => flags.extend(["-nostdlibinc", "-I", include]),
        None => flags.push("-lc++"),
    }
    build_x86_64(name, source, &flags)
}

pub fn build_objcpp(name: &str, source: &str) -> Option<String> {
    build_x86_64(
        name,
        source,
        &["-x", "objective-c++", "-lc++", "-framework", "Foundation"],
    )
}

pub fn build_objc(name: &str, source: &str) -> Option<String> {
    build_x86_64(
        name,
        source,
        &["-x", "objective-c", "-framework", "Foundation"],
    )
}

pub const OBJC_MESSAGES_M: &str = r#"
#import <Foundation/Foundation.h>
#import <objc/runtime.h>

static int loaded = 0;

@protocol Speaks
- (const char *)says;
@end

@interface Animal : NSObject { int legs; }
- (instancetype)initWithLegs:(int)n;
- (int)legs;
- (const char *)says;
- (int)twiceLegs;
@end

@implementation Animal
+ (void)load { loaded += 1; }
- (instancetype)initWithLegs:(int)n { self = [super init]; if (self) legs = n; return self; }
- (int)legs { return legs; }
- (const char *)says { return "..."; }
/* One message and nothing else, so clang tail-jumps to objc_msgSend at -O2. */
- (int)twiceLegs { return [self legs] * 2; }
@end

@interface Dog : Animal <Speaks> { double weight; }
- (instancetype)initWithWeight:(double)w;
- (double)weight;
- (const char *)describeWith:(const char *)prefix;
+ (const char *)species;
@end

@implementation Dog
+ (void)load { loaded += 10; }
- (instancetype)initWithWeight:(double)w {
    self = [super initWithLegs:4];
    if (self) weight = w;
    return self;
}
- (double)weight { return weight; }
- (const char *)says { return "woof"; }
- (const char *)describeWith:(const char *)prefix { return prefix; }
+ (const char *)species { return "canis"; }
@end

@interface NSObject (Rosetta)
- (const char *)rosetta;
@end
@implementation NSObject (Rosetta)
- (const char *)rosetta { return "category on a host class"; }
@end

int main(void) {
    Dog *d = [[Dog alloc] initWithWeight:12.5];
    printf("load %d\n", loaded);
    printf("legs %d says %s\n", [d legs], [d says]);
    printf("inherited %s\n", [[[Animal alloc] initWithLegs:2] says]);
    printf("twice %d\n", [d twiceLegs]);
    printf("weight %.1f\n", [d weight]);
    printf("prefix %s\n", [d describeWith:"argument"]);
    printf("species %s\n", [Dog species]);
    printf("identity %d %d\n", [d class] == [Dog class], [d class] == [Animal class]);
    printf("kind %d\n", [d isKindOfClass:[Animal class]]);
    printf("name %s super %s\n",
           class_getName([d class]), class_getName(class_getSuperclass([d class])));
    printf("responds %d %d\n",
           [d respondsToSelector:@selector(weight)], [d respondsToSelector:@selector(fly)]);
    printf("conforms %d\n", [d conformsToProtocol:@protocol(Speaks)]);
    printf("category %s\n", [d rosetta]);
    NSString *s = [NSString stringWithUTF8String:"a host object"];
    printf("host %s %lu\n", [s UTF8String], (unsigned long)[s length]);
    printf("constant %s\n", [@"a constant string" UTF8String]);
    printf("nil %d %p\n", [(Dog *)nil legs], (void *)[(Dog *)nil says]);
    /* Aggregates by value, in the three shapes the two ABIs disagree about
       most. An NSRect is thirty-two bytes: the guest passes it on its *stack*
       and returns it through a hidden buffer (`objc_msgSend_stret`), while the
       host takes it in v0-v3 and returns it there. An NSPoint is two SSE
       eightbytes on one side and a two-member HFA on the other. An NSRange is
       two integer eightbytes and two `x` registers, which is the one case the
       two conventions agree on - and is worth having next to the others so a
       failure says which of the three broke. */
    NSRect rc = [[NSValue valueWithRect:NSMakeRect(1.5, 2.5, 3.5, 4.5)] rectValue];
    printf("rect %.1f %.1f %.1f %.1f\n", rc.origin.x, rc.origin.y, rc.size.width, rc.size.height);
    NSPoint pt = [[NSValue valueWithPoint:NSMakePoint(6.5, 7.5)] pointValue];
    printf("point %.1f %.1f\n", pt.x, pt.y);
    NSRange rg = [[NSValue valueWithRange:NSMakeRange(8, 9)] rangeValue];
    printf("range %lu %lu\n", (unsigned long)rg.location, (unsigned long)rg.length);
    /* A variadic method, rendered by the runtime and re-sent with the result
       standing in for its format - including `%@`, which is a message. */
    printf("format %s\n",
           [[NSString stringWithFormat:@"%d/%@/%.2f/%%", 42, @"x", 1.5] UTF8String]);
    @synchronized (d) { printf("synchronized\n"); }
    @autoreleasepool { printf("pool\n"); }
    return 0;
}
"#;

pub fn objc_messages_fixture(name: &str, extra: &[&str]) -> Option<String> {
    let mut flags = vec!["-x", "objective-c", "-framework", "Foundation"];
    flags.extend_from_slice(extra);
    build_x86_64(name, OBJC_MESSAGES_M, &flags)
}

pub const OBJC_HOST_AGGREGATE_M: &str = r#"
#import <Foundation/Foundation.h>

/* Ivars declared rather than synthesized: writing both accessors by hand
   suppresses synthesis, and `bounds` collides with a Carbon typedef if the
   compiler is left to guess. */
@interface Canvas : NSObject {
    NSRect _bounds;
    NSPoint _origin;
    double _scale;
}
@property (nonatomic) NSRect bounds;
@property (nonatomic) NSPoint origin;
@property (nonatomic) double scale;
@end

@implementation Canvas
/* Written out rather than @synthesize'd so the setter is unambiguously a guest
   method taking the struct by value, and so a reader can see the shape. */
- (void)setBounds:(NSRect)r { _bounds = r; }
- (NSRect)bounds { return _bounds; }
- (void)setOrigin:(NSPoint)p { _origin = p; }
- (NSPoint)origin { return _origin; }
- (void)setScale:(double)s { _scale = s; }
- (double)scale { return _scale; }
@end

int main(void) {
    Canvas *c = [[Canvas alloc] init];

    /* Host -> guest, struct by value: KVC invokes the setter itself. */
    [c setValue:[NSValue valueWithRect:NSMakeRect(1.5, 2.5, 3.5, 4.5)] forKey:@"bounds"];
    NSRect b = [c bounds];
    printf("bounds %.1f %.1f %.1f %.1f\n", b.origin.x, b.origin.y, b.size.width, b.size.height);

    [c setValue:[NSValue valueWithPoint:NSMakePoint(6.5, 7.5)] forKey:@"origin"];
    NSPoint o = [c origin];
    printf("origin %.1f %.1f\n", o.x, o.y);

    /* Host -> guest with a floating-point *return*: KVC calls the getter and
       boxes what it finds in `d0`. */
    [c setValue:@(9.25) forKey:@"scale"];
    printf("scale %.2f\n", [[c valueForKey:@"scale"] doubleValue]);

    return 0;
}
"#;

pub const OBJC_AFTER_MEMORY_ARG_M: &str = r#"
#import <Cocoa/Cocoa.h>

int main(void) {
    NSTrackingAreaOptions opts =
        NSTrackingMouseEnteredAndExited | NSTrackingMouseMoved | NSTrackingActiveAlways;
    id owner = [[NSObject alloc] init];
    NSTrackingArea *area = [[NSTrackingArea alloc] initWithRect:NSMakeRect(1.5, 2.5, 30.5, 40.5)
                                                        options:opts
                                                          owner:owner
                                                       userInfo:nil];
    NSRect r = [area rect];
    printf("rect %.1f %.1f %.1f %.1f\n", r.origin.x, r.origin.y, r.size.width, r.size.height);
    printf("options %d owner %d\n", [area options] == opts, [area owner] == owner);
    return 0;
}
"#;

/// Build [`OBJC_AFTER_MEMORY_ARG_M`], or `None` if this host cannot.
pub fn objc_after_memory_arg_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            build_x86_64(
                "objc_after_memory_arg",
                OBJC_AFTER_MEMORY_ARG_M,
                &["-x", "objective-c", "-framework", "Cocoa"],
            )
        })
        .clone()
}

/// Build [`OBJC_HOST_AGGREGATE_M`], or `None` if this host cannot.
pub fn objc_host_aggregate_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_objc("objc_host_aggregate", OBJC_HOST_AGGREGATE_M))
        .clone()
}

pub const OBJC_GUEST_UNION_M: &str = r#"
#import <Foundation/Foundation.h>

union Word {
    double d;
    struct { int lo; int hi; } halves;
};

/* Large enough to be MEMORY-class on both conventions, so the send is
   `objc_msgSend_stret` rather than the ordinary trampoline. */
struct Wide {
    union Word a;
    union Word b;
    union Word c;
};

@interface Packer : NSObject
- (struct Wide)wide;
- (NSString *)describe;
@end

@implementation Packer
- (struct Wide)wide {
    struct Wide w;
    w.a.d = 1.5;
    w.b.halves.lo = 7;
    w.b.halves.hi = 9;
    w.c.d = 2.25;
    return w;
}
/* Guest -> guest: the sender and `wide` are both this class's. */
- (NSString *)describe {
    struct Wide w = [self wide];
    return [NSString stringWithFormat:@"%.2f %d %d %.2f",
            w.a.d, w.b.halves.lo, w.b.halves.hi, w.c.d];
}
@end

int main(void) {
    printf("%s\n", [[[Packer alloc] init] describe].UTF8String);
    return 0;
}
"#;

/// Build [`OBJC_GUEST_UNION_M`], or `None` if this host cannot.
pub fn objc_guest_union_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_objc("objc_guest_union", OBJC_GUEST_UNION_M))
        .clone()
}

pub const OBJC_REFUSALS_M: &str = r#"
#import <Foundation/Foundation.h>
#include <string.h>

int main(int argc, char **argv) {
    const char *which = argc > 1 ? argv[1] : "";
    printf("before\n");
    if (!strcmp(which, "variadic")) {
        /* `appendFormat:` takes an ellipsis, and no type encoding records it. */
        NSMutableString *s = [[NSMutableString alloc] init];
        [s appendFormat:@"%d", 3];
        printf("appended %s\n", [s UTF8String]);
    } else if (!strcmp(which, "aggregate")) {
        /* `rangeOfString:` returns an NSRange by value. */
        NSRange r = [@"hello world" rangeOfString:@"world"];
        printf("range %lu\n", (unsigned long)r.location);
    } else if (!strcmp(which, "throw")) {
        @throw [NSException exceptionWithName:@"E" reason:@"r" userInfo:nil];
    }
    printf("after\n");
    return 0;
}
"#;

pub const OBJC_EXCEPTIONS_MM: &str = r#"
#import <Foundation/Foundation.h>
#include <stdio.h>

@interface Narrower : NSException @end
@implementation Narrower @end

int main(void) {
    @try {
        @throw [NSException exceptionWithName:@"E" reason:@"r" userInfo:nil];
    } @catch (NSException *e) {
        printf("caught %s %s\n", [[e name] UTF8String], [[e reason] UTF8String]);
    } @finally {
        printf("finally\n");
    }

    @try {
        @throw [Narrower exceptionWithName:@"N" reason:@"n" userInfo:nil];
    } @catch (NSException *e) {
        printf("subclass %s\n", [[e name] UTF8String]);
    }

    @try {
        @try {
            @throw [NSException exceptionWithName:@"O" reason:@"o" userInfo:nil];
        } @catch (Narrower *e) {
            printf("wrong clause\n");
        }
    } @catch (NSException *e) {
        printf("outer %s\n", [[e name] UTF8String]);
    }

    printf("done\n");
    return 0;
}
"#;

/// Build [`OBJC_EXCEPTIONS_MM`], or `None` if this host cannot.
pub fn objc_exceptions_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_objcpp("objc_exceptions", OBJC_EXCEPTIONS_MM))
        .clone()
}

/// Build [`OBJC_REFUSALS_M`], or `None` if this host cannot.
pub fn objc_refusals_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_objc("objc_refusals", OBJC_REFUSALS_M))
        .clone()
}

pub const EXCEPTIONS_CPP: &str = r#"
#include <cstdio>
#include <exception>
#include <pthread.h>

struct Trace { const char *n; ~Trace() { printf("~%s\n", n); } };
struct Boom { int v; ~Boom() { printf("~Boom(%d)\n", v); } };
struct Other { const char *why; };
struct Base { int v; Base(int x) : v(x) {} };
struct Derived : Base { Derived(int x) : Base(x) {} };
struct MyError : std::exception {
    const char *what() const noexcept override { return "my error"; }
};

static int level3(int n) { Trace t{"level3"}; if (n > 0) throw Boom{n * 2}; return 1; }
static int level2(int n) { Trace t{"level2"}; return level3(n) + 1; }
static int level1(int n) { Trace t{"level1"}; return level2(n) + 1; }

static void nested() {
    try {
        try { throw Other{"inner"}; }
        catch (const Boom &) { printf("BUG: matched the wrong clause\n"); }
    } catch (const Other &o) { printf("outer caught %s\n", o.why); }
}

static int same_frame() { try { throw 11; } catch (int i) { return i; } }

static void rethrower() {
    try { throw Boom{1}; }
    catch (const Boom &b) { printf("rethrowing %d\n", b.v); throw; }
}

static void *worker(void *arg) {
    try { Trace t{"thread"}; throw Boom{(int)(long)arg}; }
    catch (const Boom &b) { printf("thread caught %d\n", b.v); }
    return (void *)1;
}

int main(void) {
    try { level1(3); } catch (const Boom &b) { printf("caught Boom %d\n", b.v); }
    nested();
    try { throw 3.5; } catch (...) { printf("caught something\n"); }
    printf("same frame %d\n", same_frame());
    try { throw Other{"by value"}; } catch (Other o) { printf("by value %s\n", o.why); }
    try { throw Derived(4); } catch (const Base &b) { printf("base caught %d\n", b.v); }
    try { throw MyError{}; } catch (const std::exception &e) { printf("std %s\n", e.what()); }
    try { rethrower(); } catch (const Boom &b) { printf("finally %d\n", b.v); }

    pthread_t t;
    pthread_create(&t, 0, worker, (void *)7L);
    void *r = 0;
    pthread_join(t, &r);
    printf("thread returned %ld\n", (long)r);

    printf("done\n");
    return 0;
}
"#;

/// Build [`EXCEPTIONS_CPP`], or `None` if this host cannot.
pub fn exceptions_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_cpp("exceptions", EXCEPTIONS_CPP))
        .clone()
}

pub const UNWIND_REFUSALS_CPP: &str = r#"
#include <cstdio>
#include <cstdlib>
#include <cstring>

struct Trace { const char *n; ~Trace() { printf("~%s\n", n); } };
struct Boom { int v; };
struct A { int a; };
struct B { int b; };
struct AB : A, B { };

static void uncaught() { Trace t{"uncaught"}; throw Boom{1}; }

static int compare(const void *a, const void *b) {
    if (*(const int *)a == 9) throw Boom{2};
    return 0;
}

int main(int argc, char **argv) {
    const char *which = argc > 1 ? argv[1] : "";
    printf("before\n");
    if (!strcmp(which, "uncaught")) {
        uncaught();
    } else if (!strcmp(which, "across")) {
        int v[2] = {9, 1};
        try { qsort(v, 2, sizeof(int), compare); }
        catch (const Boom &) { printf("caught across a host frame\n"); }
    } else if (!strcmp(which, "multiple")) {
        try { throw AB{}; } catch (const B &) { printf("multiple inheritance\n"); }
    }
    printf("after\n");
    return 0;
}
"#;

/// Build [`UNWIND_REFUSALS_CPP`], or `None` if this host cannot.
pub fn unwind_refusals_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_cpp("unwind_refusals", UNWIND_REFUSALS_CPP))
        .clone()
}

pub const SIGNAL_C: &str = r#"
#include <signal.h>
#include <stdio.h>

static volatile sig_atomic_t hits = 0;

static void on_usr1(int sig) { hits++; printf("handler saw %d\n", sig); }

int main(void) {
    printf("layout %zu %zu\n", sizeof(struct sigaction), sizeof(sigset_t));

    void (*previous)(int) = signal(SIGUSR1, on_usr1);
    printf("previous was default %d\n", previous == SIG_DFL);
    raise(SIGUSR1);

    /* The round trip: what `signal` gives back must be the guest's own
       function, or restoring it installs something else entirely. */
    void (*saved)(int) = signal(SIGUSR1, SIG_IGN);
    printf("saved is mine %d\n", saved == on_usr1);
    raise(SIGUSR1);
    signal(SIGUSR1, saved);
    raise(SIGUSR1);

    struct sigaction sa, old;
    sa.sa_handler = on_usr1;
    sa.sa_flags = 0;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR2, &sa, &old);
    printf("old was default %d\n", old.sa_handler == SIG_DFL);
    raise(SIGUSR2);

    sigaction(SIGUSR2, 0, &old);
    printf("queried is mine %d\n", old.sa_handler == on_usr1);

    signal(SIGUSR1, SIG_DFL);
    printf("hits %d\n", hits);
    return 0;
}
"#;

/// Build [`SIGNAL_C`], or `None` if this host cannot.
pub fn signal_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("signals", SIGNAL_C, &[]))
        .clone()
}

pub const SIGINFO_C: &str = r#"
#include <signal.h>
#include <stdio.h>

static void detailed(int sig, siginfo_t *info, void *uc) {
    (void)uc;
    printf("detailed %d %p\n", sig, info->si_addr);
}

int main(void) {
    struct sigaction sa;
    sa.sa_sigaction = detailed;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, 0);
    raise(SIGUSR1);
    printf("done\n");
    return 0;
}
"#;

/// Build [`SIGINFO_C`], or `None` if this host cannot.
pub fn siginfo_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("siginfo", SIGINFO_C, &[]))
        .clone()
}

/// Build [`THREAD_C`], or `None` if this host cannot.
pub fn thread_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("threads", THREAD_C, &[]))
        .clone()
}

/// Build [`ERRNO_C`], or `None` if this host cannot.
pub fn errno_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("errno_thread", ERRNO_C, &[]))
        .clone()
}

/// Build [`OPEN_C`], or `None` if this host cannot.
pub fn open_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("open_mode", OPEN_C, &[]))
        .clone()
}

/// Build [`PTHREAD_STACK_C`], or `None` if this host cannot.
pub fn pthread_stack_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("pthread_stack", PTHREAD_STACK_C, &[]))
        .clone()
}

/// Build [`PTHREAD_SYNC_C`], or `None` if this host cannot.
pub fn pthread_sync_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("pthread_sync", PTHREAD_SYNC_C, &[]))
        .clone()
}

pub const INT128_C: &str = r#"
#include <stdio.h>

static void show(const char *tag, unsigned __int128 v) {
    printf("%s %llu:%llu\n", tag, (unsigned long long)(v >> 64), (unsigned long long)v);
}

int main(int argc, char **argv) {
    (void)argv;
    unsigned __int128 a = ((unsigned __int128)1 << 100) + 12345;
    unsigned __int128 b = (unsigned __int128)argc * 3;
    show("uq", a / b);
    show("ur", a % b);
    __int128 c = -(__int128)a;
    __int128 d = (__int128)argc * 3;
    show("sq", (unsigned __int128)(c / d));
    show("sr", (unsigned __int128)(c % d));
    return 0;
}
"#;

/// Build [`INT128_C`], or `None` if this host cannot.
pub fn int128_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("int128", INT128_C, &[]))
        .clone()
}

pub const CFSTRING_C: &str = r#"
#include <CoreFoundation/CoreFoundation.h>
#include <stdio.h>

int main(void) {
    CFStringRef s = CFStringCreateWithCString(NULL, "fmodstudio", kCFStringEncodingUTF8);
    CFIndex len = CFStringGetLength(s);
    CFIndex max = CFStringGetMaximumSizeForEncoding(len, kCFStringEncodingUTF8);
    char buf[256];
    buf[0] = 0;
    Boolean ok = CFStringGetCString(s, buf, max + 1, kCFStringEncodingUTF8);
    printf("len %ld max %ld ok %d buf '%s'\n", (long)len, (long)max, (int)ok, buf);
    CFRelease(s);
    return 0;
}
"#;

pub const RUNLOOP_C: &str = r#"
#include <CoreFoundation/CoreFoundation.h>
#include <stdio.h>

static int source_calls = 0;
static int timer_calls = 0;
static CFRunLoopSourceRef the_source;

static void performed(void *info) {
    source_calls++;
    printf("source %d info %ld\n", source_calls, (long)(intptr_t)info);
}

static void fired(CFRunLoopTimerRef timer, void *info) {
    timer_calls++;
    printf("timer %d info %ld\n", timer_calls, (long)(intptr_t)info);
    /* Signalled from inside a timer callback, so the next turn of the loop
       services it: a source and a timer in the same loop, in order. */
    CFRunLoopSourceSignal(the_source);
    CFRunLoopWakeUp(CFRunLoopGetCurrent());
}

int main(void) {
    CFRunLoopSourceContext sc;
    memset(&sc, 0, sizeof sc);
    sc.info = (void *)(intptr_t)42;
    sc.perform = performed;
    the_source = CFRunLoopSourceCreate(NULL, 0, &sc);
    CFRunLoopAddSource(CFRunLoopGetCurrent(), the_source, kCFRunLoopDefaultMode);

    CFRunLoopTimerContext tc;
    memset(&tc, 0, sizeof tc);
    tc.info = (void *)(intptr_t)7;
    CFRunLoopTimerRef timer = CFRunLoopTimerCreate(
        NULL, CFAbsoluteTimeGetCurrent() + 0.01, 0.01, 0, 0, fired, &tc);
    CFRunLoopAddTimer(CFRunLoopGetCurrent(), timer, kCFRunLoopDefaultMode);

    while (timer_calls < 2 || source_calls < 2) {
        if (CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0, true) == kCFRunLoopRunFinished) {
            printf("the loop emptied\n");
            break;
        }
    }
    CFRunLoopTimerInvalidate(timer);
    printf("done %d %d\n", source_calls >= 2, timer_calls >= 2);
    return 0;
}
"#;

pub const DLOPEN_FRAMEWORK_C: &str = r#"
#include <dlfcn.h>
#include <stdio.h>
int main(void) {
    const char *cf = "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation";
    void *h = dlopen(cf, RTLD_LAZY);
    printf("handle %d\n", h != NULL);
    void *f = h ? dlsym(h, "CFStringGetLength") : NULL;
    printf("scoped %d\n", f != NULL);
    printf("absent %d\n", h ? dlsym(h, "NoSuchFunctionAnywhere") != NULL : 0);
    printf("global %d\n", dlsym(RTLD_DEFAULT, "CFStringGetLength") != NULL);
    return 0;
}
"#;

/// Build [`DLOPEN_FRAMEWORK_C`], or `None` if this host cannot.
pub fn dlopen_framework_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("dlopen_framework", DLOPEN_FRAMEWORK_C, &[]))
        .clone()
}

pub const BLOCK_C: &str = r#"
#include <Block.h>
#include <stdio.h>

typedef void (^Adder)(int);

/// The captured `base` outlives this frame only if the copy took it along.
static Adder made(int base, int *sink) {
    return Block_copy(^(int x) { *sink = base + x; });
}

int main(void) {
    int sink = 0;
    Adder add = made(40, &sink);
    add(2);
    printf("captured %d\n", sink);

    __block int counter = 0;
    void (^bump)(void) = ^{ counter++; };
    void (^kept)(void) = Block_copy(bump);
    bump();
    kept();
    kept();
    printf("byref %d\n", counter);

    Block_release(kept);
    Block_release(add);

    void (^global)(void) = ^{ printf("global ran\n"); };
    void (^copied)(void) = Block_copy(global);
    printf("global same %d\n", copied == global);
    copied();
    Block_release(copied);
    return 0;
}
"#;

/// Build [`BLOCK_C`], or `None` if this host cannot.
pub fn block_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("block", BLOCK_C, &[]))
        .clone()
}

/// Build [`RUNLOOP_C`], or `None` if this host cannot.
pub fn runloop_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("runloop", RUNLOOP_C, &["-framework", "CoreFoundation"]))
        .clone()
}

/// Build [`CFSTRING_C`], or `None` if this host cannot.
pub fn cfstring_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("cfstring", CFSTRING_C, &["-framework", "CoreFoundation"]))
        .clone()
}

pub const TAILCALL_C: &str = r#"
#include <netdb.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>

__attribute__((noinline))
static int fwd(const struct sockaddr *sa, socklen_t sl, char *h, socklen_t hl,
               char *s, socklen_t sl2, int flags) {
    __attribute__((musttail)) return getnameinfo(sa, sl, h, hl, s, sl2, flags);
}

static char host[128], serv[64];

int main(int argc, char **argv) {
    (void)argv;
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_len = sizeof a;
    a.sin_family = AF_INET;
    a.sin_port = htons(80);
    a.sin_addr.s_addr = htonl(0x7f000001);
    /* Opaque to the optimiser, so the flags word really travels. */
    int flags = argc > 99 ? 0 : (NI_NUMERICHOST | NI_NUMERICSERV);
    int rc = fwd((struct sockaddr *)&a, sizeof a, host, sizeof host,
                 serv, sizeof serv, flags);
    printf("rc %d host %s serv %s\n", rc, host, serv);
    return 0;
}
"#;

/// Build [`TAILCALL_C`], or `None` if this host cannot.
pub fn tailcall_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("tailcall", TAILCALL_C, &[]))
        .clone()
}

pub const TRAMPOLINE_C: &str = r#"
#include <arpa/inet.h>
#include <dlfcn.h>
#include <netdb.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>

/* `volatile`, so the call really goes through the pointer and clang does not
   turn it back into a call site - which would test the path that already
   worked. */
static size_t (*volatile len_slot)(const char *) = strlen;
static int (*volatile gni_slot)(const struct sockaddr *, socklen_t, char *,
                                socklen_t, char *, socklen_t, int) = getnameinfo;

int main(void) {
    printf("slot len=%zu\n", len_slot("abcdef"));

    size_t (*len_dl)(const char *) =
        (size_t (*)(const char *))dlsym(RTLD_DEFAULT, "strlen");
    printf("dlsym len=%zu same=%d\n", len_dl ? len_dl("abcdefgh") : 0,
           (void *)len_dl == (void *)len_slot);

    char host[64], serv[32];
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_len = sizeof a;
    a.sin_family = AF_INET;
    a.sin_port = htons(80);
    a.sin_addr.s_addr = htonl(0x7f000001);
    int rc = gni_slot((struct sockaddr *)&a, sizeof a, host, sizeof host,
                      serv, sizeof serv, NI_NUMERICHOST | NI_NUMERICSERV);
    printf("gni rc=%d host=%s serv=%s\n", rc, host, serv);
    return 0;
}
"#;

/// Build [`TRAMPOLINE_C`], or `None` if this host cannot.
pub fn trampoline_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("trampoline", TRAMPOLINE_C, &[]))
        .clone()
}

pub const CMPXCHG16B_C: &str = r#"
#include <pthread.h>
#include <stdio.h>

typedef struct { unsigned long lo, hi; } pair;
static _Alignas(16) pair shared;

static void *worker(void *unused) {
    (void)unused;
    for (int i = 0; i < 10000; i++) {
        for (;;) {
            pair seen = shared;
            pair next = { seen.lo + 1, seen.hi + 1 };
            __int128 want, give;
            __builtin_memcpy(&want, &seen, 16);
            __builtin_memcpy(&give, &next, 16);
            if (__sync_bool_compare_and_swap((__int128 *)&shared, want, give)) break;
        }
    }
    return 0;
}

int main(void) {
    pthread_t t[4];
    for (int i = 0; i < 4; i++) pthread_create(&t[i], 0, worker, 0);
    for (int i = 0; i < 4; i++) pthread_join(t[i], 0);
    printf("lo %lu hi %lu torn %d\n", shared.lo, shared.hi, shared.lo != shared.hi);
    return 0;
}
"#;

/// Build [`CMPXCHG16B_C`], or `None` if this host cannot.
pub fn cmpxchg16b_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("cmpxchg16b", CMPXCHG16B_C, &["-mcx16"]))
        .clone()
}

pub const FAULT_C: &str = r#"
#include <stdio.h>
#include <stdlib.h>

int main(void) {
    printf("about to fault\n");
    fflush(stdout);
    volatile long *p = (volatile long *)strtoull("0x4141414141414141", NULL, 16);
    *p = 1;
    printf("still here\n");
    return 0;
}
"#;

pub const SYSCALL_C: &str = r#"
#include <stdio.h>

int main(void) {
    printf("before\n");
    fflush(stdout);
    __asm__ volatile(
        "movq $0x2000004, %%rax\n\t"
        "movq $1, %%rdi\n\t"
        "leaq 1f(%%rip), %%rsi\n\t"
        "movq $4, %%rdx\n\t"
        "syscall\n\t"
        "jmp 2f\n\t"
        "1: .ascii \"raw\\n\"\n\t"
        "2:"
        ::: "rax", "rdi", "rsi", "rdx", "rcx", "r11", "memory");
    printf("after\n");
    return 0;
}
"#;

/// Build [`SYSCALL_C`], or `None` if this host cannot.
pub fn syscall_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("guest_syscall", SYSCALL_C, &[]))
        .clone()
}

/// Build [`FAULT_C`], or `None` if this host cannot.
pub fn fault_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("guest_fault", FAULT_C, &[]))
        .clone()
}

pub const PROFILE_C: &str = r#"
#include <stdio.h>
#include <stdlib.h>

static unsigned long spin(unsigned long n) {
    unsigned long a = 1;
    for (unsigned long i = 0; i < n; i++) {
        a = a * 6364136223846793005UL + 1442695040888963407UL;
        a ^= a >> 33;
    }
    return a;
}

static unsigned long chatty(unsigned long n) {
    unsigned long a = 0;
    char buf[32];
    for (unsigned long i = 0; i < n; i++) {
        snprintf(buf, sizeof buf, "%lu", i);
        a += (unsigned long)atoi(buf);
    }
    return a;
}

int main(void) {
    printf("%lu %lu\n", spin(200000000UL), chatty(100000UL));
    return 0;
}
"#;

/// Build [`PROFILE_C`], or `None` if this host cannot.
pub fn profile_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("guest_profile", PROFILE_C, &["-O1"]))
        .clone()
}

/// Build [`TLS_C`], or `None` if this host cannot.
pub fn tls_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("thread_locals", TLS_C, &[]))
        .clone()
}

pub fn build_native(name: &str, source: &str) -> Option<String> {
    build_native_with(name, source, &[])
}

pub fn build_native_with(name: &str, source: &str, extra: &[&str]) -> Option<String> {
    let dir = fixture_dir();
    let src = dir.join(format!("{name}.c"));
    let out = dir.join(name);
    std::fs::write(&src, source).ok()?;

    // Before the source, as [`build_x86_64`] has them: `cc` ignores a `-x c++`
    // that comes after the last input file, and says so only as a warning.
    let status = Command::new("cc")
        .args(extra)
        .arg("-o")
        .arg(&out)
        .arg(&src)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    // And it has to be a Mach-O, which is the whole point of a "native" fixture:
    // it exists to be run by the native backend, and that backend loads Mach-O. On a
    // host that is not a Mac, `cc` succeeds and produces an ELF, and a test handed one
    // of those fails on a magic number rather than skipping.
    host_macho(out.to_str()?).then(|| out.to_string_lossy().into_owned())
}

pub fn run_native(path: &str) -> Option<(Option<i32>, String)> {
    run_native_with(path, &[])
}

/// [`run_native`], with arguments - for a fixture that selects a case by one.
pub fn run_native_with(path: &str, args: &[&str]) -> Option<(Option<i32>, String)> {
    let out = Command::new(path).args(args).output().ok()?;
    out.status.code().is_some().then(|| {
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    })
}

pub fn link_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("blocklink", LINK_C, &["-O1"]))
        .clone()
}

/// Build [`POLYLINK_C`], or `None` if this host cannot.
///
/// `-O1` for the same reason [`link_fixture`] uses it.
pub fn polylink_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("polylink", POLYLINK_C, &["-O1"]))
        .clone()
}

/// Build [`QSORT_C`], or `None` if this host cannot.
pub fn qsort_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("qsort_cb", QSORT_C, &[]))
        .clone()
}

/// Build [`CTOR_C`], or `None` if this host cannot.
pub fn ctor_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("ctors", CTOR_C, &[]))
        .clone()
}

/// Build [`DTOR_C`], or `None` if this host cannot.
pub fn dtor_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("dtors", DTOR_C, &[]))
        .clone()
}

/// Build [`LATE_CALLBACK_C`], or `None` if this host cannot.
pub fn late_callback_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("late_callback", LATE_CALLBACK_C, &[]))
        .clone()
}

pub const VALIST_C: &str = r#"
#include <stdio.h>
#include <stdarg.h>
#include <stdlib.h>

static void logline(const char *fmt, ...) {
    va_list ap; va_start(ap, fmt);
    vfprintf(stdout, fmt, ap);
    va_end(ap);
}
static void tobuf(char *out, size_t n, const char *fmt, ...) {
    va_list ap; va_start(ap, fmt);
    vsnprintf(out, n, fmt, ap);
    va_end(ap);
}
static char *alloced(const char *fmt, ...) {
    va_list ap; va_start(ap, fmt); char *p = NULL;
    vasprintf(&p, fmt, ap);
    va_end(ap);
    return p;
}

int main(void) {
    logline("ints %d %d %d %d %d %d %d %d %d\n", 1, 2, 3, 4, 5, 6, 7, 8, 9);
    logline("fps %.1f %.1f %.1f %.1f %.1f %.1f %.1f %.1f %.1f\n",
            1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5, 8.5, 9.5);
    logline("mixed %s %d %.2f %s %d\n", "a", 7, 2.25, "b", 8);
    char buf[16];
    tobuf(buf, sizeof buf, "%s-%d-%s", "truncate", 42, "me");
    printf("bounded %s\n", buf);
    char *p = alloced("alloc %d %s", 5, "ok");
    printf("%s\n", p);
    free(p);
    return 0;
}
"#;

/// Build [`VALIST_C`], or `None` if this host cannot.
pub fn valist_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("valist", VALIST_C, &[]))
        .clone()
}

pub const PLUGIN_C: &str = r#"
#include <stdio.h>
#include <string.h>

extern int executable_value(void);

__attribute__((constructor)) static void plugin_init(void) {
    printf("plugin loaded %d\n", executable_value());
}

int plugin_len(const char *s) { return (int)strlen(s); }
"#;

/// The program that loads [`PLUGIN_C`], named without a directory - which is
/// what a bundled application does, and what makes the search beside the
/// executable load-bearing rather than a convenience.
pub const DLOPEN_C: &str = r#"
#include <dlfcn.h>
#include <stdio.h>

int main(void) {
    void *h = dlopen("libplugin.dylib", RTLD_NOW);
    if (!h) { printf("dlopen failed: %s\n", dlerror()); return 1; }
    int (*len)(const char *) = (int (*)(const char *))dlsym(h, "plugin_len");
    if (!len) { printf("dlsym failed: %s\n", dlerror()); return 2; }
    printf("len=%d\n", len("abcdef"));
    /* A name nothing defines must be null *and* leave a message behind. */
    void *missing = dlsym(h, "plugin_no_such_symbol");
    const char *why = dlerror();
    printf("missing=%d reported=%d\n", missing == NULL, why != NULL);
    dlclose(h);
    return 0;
}

int executable_value(void) { return 42; }
"#;

pub fn dlopen_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            build_x86_64(
                "libplugin.dylib",
                PLUGIN_C,
                &[
                    "-dynamiclib",
                    "-install_name",
                    "@rpath/libplugin.dylib",
                    "-undefined",
                    "dynamic_lookup",
                ],
            )?;
            build_x86_64("dlopener", DLOPEN_C, &[])
        })
        .clone()
}

pub const OBJC_PLUGIN_M: &str = r#"
#import <Foundation/Foundation.h>
#include <stdio.h>

@interface Plugin : NSObject { int count; }
+ (void)load;
+ (int)greet;
- (int)bump;
@end

@implementation Plugin
+ (void)load { puts("plugin +load"); }
+ (int)greet { return 41; }
- (int)bump { count += 1; return count; }
@end

__attribute__((constructor)) static void plugin_init(void) { puts("plugin ctor"); }

int plugin_run(void) {
    Plugin *p = [[Plugin alloc] init];
    int n = [Plugin greet] + [p bump];
    NSString *s = [NSString stringWithUTF8String:"twelve"];
    return n + (int)[s length];
}
"#;

/// The program that loads [`OBJC_PLUGIN_M`] and calls into it.
pub const OBJC_DLOPEN_M: &str = r#"
#import <Foundation/Foundation.h>
#include <dlfcn.h>
#include <stdio.h>

/* The program declares a class too, so the loaded image's registration has to
   *extend* an existing runtime rather than build one. */
@interface Host : NSObject
+ (int)base;
@end
@implementation Host
+ (int)base { return 100; }
@end

int main(void) {
    printf("host=%d\n", [Host base]);
    void *h = dlopen("@executable_path/libobjcplugin.dylib", RTLD_NOW);
    if (!h) { printf("dlopen failed: %s\n", dlerror()); return 1; }
    int (*run)(void) = (int (*)(void))dlsym(h, "plugin_run");
    if (!run) { printf("dlsym failed: %s\n", dlerror()); return 2; }
    printf("plugin=%d\n", run());
    return 0;
}
"#;

/// Build [`OBJC_DLOPEN_M`] and its plugin beside it, or `None` if this host
/// cannot.
pub fn objc_dlopen_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            build_x86_64(
                "libobjcplugin.dylib",
                OBJC_PLUGIN_M,
                &[
                    "-x",
                    "objective-c",
                    "-framework",
                    "Foundation",
                    "-dynamiclib",
                    "-install_name",
                    "@rpath/libobjcplugin.dylib",
                ],
            )?;
            build_objc("objcdlopener", OBJC_DLOPEN_M)
        })
        .clone()
}

/// Run an x86-64 guest program under the driver, returning `(status, stdout, stderr)`.
/// through [`run_guest_with`] directly.
pub fn run_guest(path: &str, args: &[&str]) -> (Option<i32>, String, String) {
    run_guest_with(&["--emulated"], path, args)
}

pub fn run_guest_env(
    env: &[(&str, &str)],
    path: &str,
    args: &[&str],
) -> (Option<i32>, String, String) {
    let mut command = driver_command();
    command.arg("run").arg("--emulated").arg(path).args(args);
    // After the isolation `driver_command` set, so that a test may still name a
    // prefix of its own.
    for (key, value) in env {
        command.env(key, value);
    }
    let out = command.output().expect("running the driver");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

pub fn driver() -> &'static str {
    env!("CARGO_BIN_EXE_rosetta3")
}

pub fn test_prefix() -> String {
    let name = std::path::Path::new(driver())
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rosetta3".to_string());
    std::env::temp_dir()
        .join(format!("rosetta3-test-prefix-{name}"))
        .to_string_lossy()
        .into_owned()
}

/// [`Command`] for the driver, isolated from this user's prefix.
pub fn driver_command() -> Command {
    let mut command = Command::new(driver());
    command.env("ROSETTA3_PREFIX", test_prefix());
    // A test must never download a Darwin base, nor wait on a dialog asking
    // whether to: the prefix is made without one.
    command.env("ROSETTA3_PREFIX_SETUP", "fail");
    command
}

pub fn run_guest_with(flags: &[&str], path: &str, args: &[&str]) -> (Option<i32>, String, String) {
    let out = driver_command()
        .arg("run")
        .args(flags)
        .arg(path)
        .args(args)
        .output()
        .expect("running the driver");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

pub fn host_macho(path: &str) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let Some(magic) = bytes.get(..4) else {
        return false;
    };
    let magic = u32::from_be_bytes([magic[0], magic[1], magic[2], magic[3]]);
    matches!(
        magic,
        // Thin, 32- and 64-bit, either endianness...
        0xfeed_face | 0xcefa_edfe | 0xfeed_facf | 0xcffa_edfe
        // ...and fat, likewise.
        | 0xcafe_babe | 0xbeba_feca | 0xcafe_babf | 0xbfba_feca
    )
}

pub const APPKIT_M: &str = r####"
#import <Cocoa/Cocoa.h>
#import <objc/runtime.h>
#include <stdio.h>

@interface R3Delegate : NSObject
@property (nonatomic, assign) int launches;
@end

@implementation R3Delegate
- (void)applicationWillFinishLaunching:(NSNotification *)note {
    printf("will %s\n", [[note name] UTF8String]);
}
- (void)applicationDidFinishLaunching:(NSNotification *)note {
    printf("did %s\n", [[note name] UTF8String]);
    printf("delegate is self %d\n", [NSApp delegate] == self);

    unsigned int count = 0;
    objc_property_t *properties = class_copyPropertyList([self class], &count);
    printf("properties %u\n", count);
    for (unsigned int i = 0; i < count; i++) {
        printf("property %s %s\n",
               property_getName(properties[i]),
               property_getAttributes(properties[i]));
        unsigned int fields = 0;
        objc_property_attribute_t *list =
            property_copyAttributeList(properties[i], &fields);
        printf("attributes %u first %s=%s\n", fields, list[0].name, list[0].value);
        free(list);
    }
    free(properties);

    fflush(stdout);
    [NSApp stop:nil];
}
@end

int main(int argc, const char *argv[]) {
    return NSApplicationMain(argc, argv);
}
"####;

pub const MAIN_NIB: &str = r####"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>$archiver</key>
	<string>NSKeyedArchiver</string>
	<key>$objects</key>
	<array>
		<string>$null</string>
		<dict>
			<key>$class</key><dict><key>CF$UID</key><integer>8</integer></dict>
			<key>NSConnections</key><dict><key>CF$UID</key><integer>4</integer></dict>
			<key>NSRoot</key><dict><key>CF$UID</key><integer>2</integer></dict>
		</dict>
		<dict>
			<key>$class</key><dict><key>CF$UID</key><integer>9</integer></dict>
			<key>NSClassName</key><dict><key>CF$UID</key><integer>3</integer></dict>
		</dict>
		<string>NSApplication</string>
		<dict>
			<key>$class</key><dict><key>CF$UID</key><integer>10</integer></dict>
			<key>NS.objects</key>
			<array><dict><key>CF$UID</key><integer>5</integer></dict></array>
		</dict>
		<dict>
			<key>$class</key><dict><key>CF$UID</key><integer>11</integer></dict>
			<key>NSSource</key><dict><key>CF$UID</key><integer>2</integer></dict>
			<key>NSDestination</key><dict><key>CF$UID</key><integer>6</integer></dict>
			<key>NSLabel</key><dict><key>CF$UID</key><integer>7</integer></dict>
		</dict>
		<dict>
			<key>$class</key><dict><key>CF$UID</key><integer>9</integer></dict>
			<key>NSClassName</key><dict><key>CF$UID</key><integer>12</integer></dict>
		</dict>
		<string>delegate</string>
		<dict>
			<key>$classes</key><array><string>NSIBObjectData</string></array>
			<key>$classname</key><string>NSIBObjectData</string>
		</dict>
		<dict>
			<key>$classes</key><array><string>NSCustomObject</string></array>
			<key>$classname</key><string>NSCustomObject</string>
		</dict>
		<dict>
			<key>$classes</key><array><string>NSMutableArray</string><string>NSArray</string></array>
			<key>$classname</key><string>NSMutableArray</string>
		</dict>
		<dict>
			<key>$classes</key><array><string>NSNibOutletConnector</string></array>
			<key>$classname</key><string>NSNibOutletConnector</string>
		</dict>
		<string>R3Delegate</string>
	</array>
	<key>$top</key>
	<dict>
		<key>IB.objectdata</key><dict><key>CF$UID</key><integer>1</integer></dict>
	</dict>
	<key>$version</key>
	<integer>100000</integer>
</dict>
</plist>
"####;

pub fn build_app(name: &str, source: &str, info: &str, nib: Option<&str>) -> Option<String> {
    if !can_build_x86_64() {
        return None;
    }
    let app = fixture_dir().join(format!("{name}.app"));
    let contents = app.join("Contents");
    let resources = contents.join("Resources");
    std::fs::create_dir_all(contents.join("MacOS")).ok()?;
    std::fs::create_dir_all(&resources).ok()?;
    std::fs::write(contents.join("Info.plist"), info).ok()?;
    if let Some(nib) = nib {
        std::fs::write(resources.join("MainMenu.nib"), nib).ok()?;
    }

    let src = fixture_dir().join(format!("{name}.m"));
    std::fs::write(&src, source).ok()?;
    let out = contents.join("MacOS").join(name);
    // An Objective-C fixture like any other: through the builder, so it is
    // gated on the same toolchain and gets the same `-x objective-c` whether
    // that is Apple's `cc` or the script.
    let status = match builder()? {
        Builder::Cc => Command::new("cc")
            .args([
                "-arch",
                "x86_64",
                "-x",
                "objective-c",
                "-framework",
                "Cocoa",
            ])
            .arg("-o")
            .arg(&out)
            .arg(&src)
            .status()
            .ok()?,
        Builder::MachOScript => Command::new(build_macho_script()?)
            .arg(&src)
            .arg(&out)
            .args(["-x", "objective-c", "-framework", "Cocoa"])
            .status()
            .ok()?,
    };
    status.success().then(|| out.to_string_lossy().into_owned())
}

/// Build [`APPKIT_M`] into a `.app` whose `Info.plist` names the nib, or `None`
/// if this host cannot.
pub fn appkit_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            build_app(
                "appkitmain",
                APPKIT_M,
                r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key><string>appkitmain</string>
	<key>CFBundleIdentifier</key><string>org.rosetta3.appkitmain</string>
	<key>NSPrincipalClass</key><string>NSApplication</string>
	<key>NSMainNibFile</key><string>MainMenu</string>
</dict>
</plist>
"#,
                Some(MAIN_NIB),
            )
        })
        .clone()
}

pub const GLOBALS_C: &str = r#"
#include <CoreFoundation/CoreFoundation.h>
#include <stdio.h>

int main(void) {
    printf("mode %ld\n", (long)CFStringGetLength(kCFRunLoopDefaultMode));
    printf("common %ld\n", (long)CFStringGetLength(kCFRunLoopCommonModes));
    printf("null allocator %d\n", kCFAllocatorDefault == NULL);
    CFStringRef made =
        CFStringCreateWithCString(kCFAllocatorDefault, "abc", kCFStringEncodingUTF8);
    printf("made %ld\n", (long)CFStringGetLength(made));
    CFRelease(made);
    return 0;
}
"#;

pub fn globals_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_x86_64("cfglobals", GLOBALS_C, &["-framework", "CoreFoundation"]))
        .clone()
}

/// The reason a C++ fixture that **built** still cannot be run here, or `None`
/// if it can.
///
/// ## Measured, not declared
///
/// The C++ fixtures build on this host as of `test/logs/test-hygiene.md` - the
/// gap was `<cstdio>`, `<exception>`, `<pthread.h>`, `<cstdlib>` and
/// `<cstring>`, and `tests/fixtures/include/` declares all five. What stops
/// them is a different thing, and it is in the runtime rather than in the
/// harness: `clang -target x86_64-apple-darwin` emits **DWARF** CFI in
/// `__eh_frame` for these frames, and `rosetta3::unwind` parses only the
/// compact encodings in `__unwind_info`. 25 of the 27 common entries in a built
/// fixture's `__unwind_info` header are `UNWIND_SECTION_MODE_DWARF`. The
/// runtime says so itself, by name, and this is that sentence:
///
///     rosetta3: <fixture>: __cxa_throw: not callable: a frame unwinds
///     through __eh_frame, which this runtime does not parse (M5)
///
/// No optimisation level and no `-f` flag moves it - `-O0`, `-O1`, `-Os`,
/// `-O2 -fno-omit-frame-pointer`, `-fexceptions -funwind-tables` and the
/// default were each built and run, and all six refuse identically.
///
/// ## Why it is read out of the run rather than hard-coded
///
/// So that a test gating on it cannot rot into a green tick: when the wall is
/// lifted this returns `None`, the gate opens, and the eleven specifications
/// in `tests/unwind.rs` run with no edit. A constant `false` here would be a
/// second silent skip wearing the first one's clothes.
pub fn cxx_unwinding_refusal() -> Option<String> {
    static WALL: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    WALL.get_or_init(|| {
        let path = exceptions_fixture()?;
        let (_, _, err) = run_guest(&path, &[]);
        // The runtime's own sentence, not a paraphrase of it, so the reason a
        // test gives is the reason the runtime gave.
        let line = err
            .lines()
            .find(|line| line.contains("not callable:"))?
            .trim()
            .to_string();
        Some(line)
    })
    .clone()
}

/// Skip the calling test, **loudly**.
///
/// ## Why this is not `eprintln!`
///
/// A test that returns early is `ok`. The summary says `11 passed; 0 failed`
/// and the reason - the only thing in the whole system that says the test did
/// not run - goes into a buffer libtest throws away for a test that passed.
/// `eprintln!` is captured exactly as `println!` is, so for eleven skipped
/// `unwind.rs` tests the run printed **11 passed** and said nothing else. That
/// is how `test/logs/macho-fixtures.md` came to record "9 of 11": the nine were
/// the ones whose skip line happened to be the only output, and the other two
/// were the same skip next to a compiler error. Nobody counted passes; nobody
/// counted runs. `test/PLAN.md` §1 calls a green suite a wrong answer that
/// looks like success, and this macro is where the looking like success came
/// from.
///
/// So the line is written with `libc::write(2, ...)` - the same call
/// `darwin::data_unavailable::announce` makes, and for the same stated reason:
/// a message that has to be out cannot be buffered. It lands on the terminal
/// as the test runs, it survives a passing test, and `cargo test ... 2>&1 |
/// grep '^SKIP'` is a census of everything in the suite that did not execute.
///
/// `file:line` is in the line because a census of skips is worth nothing
/// without a place to go and read what the skip is guarding.
#[macro_export]
macro_rules! skip {
    ($($arg:tt)*) => {{
        let reason = format!($($arg)*);
        let line = format!(
            "SKIP {}:{} [{}] {}\n",
            file!(),
            line!(),
            module_path!(),
            reason
        );
        // SAFETY: a live buffer, written whole or not at all.
        unsafe {
            libc::write(2, line.as_ptr().cast(), line.len());
        }
        return;
    }};
}

pub mod steam {
    use std::path::{Path, PathBuf};

    /// The generator's output, or `None` when this checkout has not generated
    /// it - it is derived from headers Valve ships and is not checked in.
    pub fn generated() -> Option<PathBuf> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/steamclient/generated")
            .canonicalize()
            .ok()?;
        (dir.join("steamclient_dispatch.bin").exists()
            && dir.join("steamclient.dylib").exists()
            && dir.join("steamclient_methods.h").exists())
        .then_some(dir)
    }

    /// The dispatch id the generator gave `name`, read from the header the
    /// guest half compiles against rather than assumed.
    pub fn method_id(header: &str, name: &str) -> u32 {
        let needle = format!("STEAM_METHOD_{name} = ");
        let line = header
            .lines()
            .find(|l| l.trim_start().starts_with(&needle))
            .unwrap_or_else(|| panic!("no dispatch id for {name}"));
        line.trim_start()[needle.len()..]
            .trim_end_matches(',')
            .parse()
            .expect("dispatch id")
    }

    /// One call, as the generated dylib makes it.
    pub fn call(id: u32, params: &mut [u8]) -> i32 {
        // SAFETY: each caller builds the packed struct `id` names, laid out as
        // the manifest records it.
        unsafe { rosetta3::steam::dispatch(id, params.as_mut_ptr().cast()) }
    }

    /// The interface pointer every params struct starts with, and the rest of
    /// the struct after it.
    pub fn params_with(iface: *mut std::ffi::c_void, rest: &[u8]) -> Vec<u8> {
        let mut buf = (iface as usize as u64).to_le_bytes().to_vec();
        buf.extend_from_slice(rest);
        buf
    }

    /// `CreateInterface(version)`, through the bridge.
    pub fn create_interface(version: &str) -> (*mut std::ffi::c_void, i32) {
        let name = std::ffi::CString::new(version).unwrap();
        let mut code: i32 = -1;
        let mut params = Vec::new();
        params.extend((name.as_ptr() as usize as u64).to_le_bytes());
        params.extend((&mut code as *mut i32 as usize as u64).to_le_bytes());
        params.extend(0u64.to_le_bytes());
        // 2 is `create_interface` in the generator's `BUILTIN_DISPATCH`.
        assert_eq!(
            call(2, &mut params),
            0,
            "CreateInterface({version}) was refused"
        );
        let found = u64::from_le_bytes(params[16..24].try_into().unwrap());
        (found as *mut std::ffi::c_void, code)
    }
}
