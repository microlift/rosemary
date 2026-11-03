/*
 * WebKitGTK, moved out of the Nix store.
 *
 * WebKitGTK finds its helper processes, bwrap and xdg-dbus-proxy at paths
 * fixed when it was built, and has no run-time override outside developer
 * builds. The bundler adds this library to libwebkitgtk's and
 * libjavascriptcoregtk's DT_NEEDED, ahead of GIO, so their calls into the two
 * spawn functions below come here first, in every process that loads them and
 * without LD_PRELOAD. Each call is passed on with those paths rewritten:
 *
 *   BAKED_LIBEXEC     -> <root>/libexec/webkitgtk-6.0
 *   BAKED_BWRAP       -> the host's bwrap, else the bundle's copy
 *   BAKED_DBUS_PROXY  -> the host's xdg-dbus-proxy, else the bundle's copy
 *
 * <root> is the directory above the one this library is in. Bubblewrap's
 * arguments arrive in a sealed memfd (`--args N`), so that is rewritten into
 * a new one at the same descriptor, with the bundle bound into the sandbox
 * and nixpkgs' bind of /nix/store made optional.
 *
 * A program from the bundle runs under the bundle's loader when the launcher
 * chose the bundled glibc (ROSEMARY_GLIBC_SELECTED); a program from the host
 * gets the host's LD_LIBRARY_PATH, since the bundle's would hand it the
 * bundled libc.
 *
 * Built with -DBAKED_LIBEXEC=... -DBAKED_BWRAP=... -DBAKED_DBUS_PROXY=...
 * -DLOADER_NAME=... (nix/bundle.nix).
 */

#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <limits.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

typedef struct _GSubprocessLauncher GSubprocessLauncher;
typedef struct _GSubprocess GSubprocess;
typedef struct _GError GError;
typedef void (*GSpawnChildSetupFunc)(void *data);

typedef GSubprocess *(*spawnv_fn)(GSubprocessLauncher *, const char *const *, GError **);
typedef int (*spawn_sync_fn)(const char *, char **, char **, int, GSpawnChildSetupFunc, void *,
                             char **, char **, int *, GError **);
typedef void (*setenv_fn)(GSubprocessLauncher *, const char *, const char *, int);
typedef void (*unsetenv_fn)(GSubprocessLauncher *, const char *);

static const char *const BWRAP_CANDIDATES[] = {"/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap", NULL};
static const char *const DBUS_PROXY_CANDIDATES[] = {
    "/usr/bin/xdg-dbus-proxy", "/bin/xdg-dbus-proxy", "/usr/local/bin/xdg-dbus-proxy", NULL};

static struct {
    /* Empty when this library is not inside a bundle: nothing is rewritten. */
    char root[PATH_MAX];
    char libexec[PATH_MAX];
    char loader[PATH_MAX];
    const char *bwrap;
    const char *dbus_proxy;
    int bundled_glibc;
} self;

static char bundled_bwrap[PATH_MAX];
static char bundled_dbus_proxy[PATH_MAX];

static const char *first_executable(const char *const *candidates, const char *fallback)
{
    for (; *candidates; candidates++)
        if (access(*candidates, X_OK) == 0)
            return *candidates;
    return fallback;
}

/* `dir` + `name` into `out`, or 0 if it does not fit. */
static int join(char out[PATH_MAX], const char *dir, const char *name)
{
    size_t a = strlen(dir), b = strlen(name);
    if (a + b + 1 >= PATH_MAX)
        return 0;
    memcpy(out, dir, a);
    out[a] = '/';
    memcpy(out + a + 1, name, b + 1);
    return 1;
}

static void locate(void)
{
    Dl_info info;
    char root[PATH_MAX];
    if (!dladdr((void *)locate, &info) || !info.dli_fname || !realpath(info.dli_fname, root))
        return;
    /* <root>/lib/<this library> */
    for (int up = 0; up < 2; up++) {
        char *slash = strrchr(root, '/');
        if (!slash || slash == root)
            return;
        *slash = 0;
    }
    if (!join(self.libexec, root, "libexec/webkitgtk-6.0") || access(self.libexec, X_OK) != 0
        || !join(self.loader, root, "glibc/lib/" LOADER_NAME)
        || !join(bundled_bwrap, self.libexec, "bwrap")
        || !join(bundled_dbus_proxy, self.libexec, "xdg-dbus-proxy"))
        return;
    strcpy(self.root, root);
    self.bwrap = first_executable(BWRAP_CANDIDATES, bundled_bwrap);
    self.dbus_proxy = first_executable(DBUS_PROXY_CANDIDATES, bundled_dbus_proxy);
    const char *selected = getenv("ROSEMARY_GLIBC_SELECTED");
    self.bundled_glibc = selected && strcmp(selected, "bundled") == 0 && access(self.loader, X_OK) == 0;
}

static int located(void)
{
    static pthread_once_t once = PTHREAD_ONCE_INIT;
    pthread_once(&once, locate);
    return self.root[0] != 0;
}

static const char NIX_STORE[] = "/nix/store";

static const char *const BAKED[] = {BAKED_LIBEXEC, BAKED_BWRAP, BAKED_DBUS_PROXY};

/* Which of BAKED `path` is or is inside, or -1. */
static int baked(const char *path)
{
    for (int i = 0; i < 3; i++) {
        size_t n = strlen(BAKED[i]);
        if (strncmp(path, BAKED[i], n) == 0 && (path[n] == 0 || path[n] == '/'))
            return i;
    }
    return -1;
}

/* `path` with a baked prefix replaced, newly allocated, or NULL if it has none. */
static char *rewrite(const char *path)
{
    int which = baked(path);
    if (which < 0)
        return NULL;
    const char *to = which == 0 ? self.libexec : which == 1 ? self.bwrap : self.dbus_proxy;
    const char *rest = path + strlen(BAKED[which]);
    char *out = malloc(strlen(to) + strlen(rest) + 1);
    if (out)
        strcat(strcpy(out, to), rest);
    return out;
}

static int from_bundle(const char *path)
{
    size_t n = strlen(self.root);
    return strncmp(path, self.root, n) == 0 && path[n] == '/';
}

/* A growable argument list. Strings are owned when `owned` says so. */
struct args {
    char **v;
    char *owned;
    size_t len, cap;
};

static int push(struct args *a, const char *s, int own)
{
    if (a->len + 2 > a->cap) {
        size_t cap = a->cap ? a->cap * 2 : 32;
        char **v = realloc(a->v, cap * sizeof *v);
        if (!v)
            return -1;
        a->v = v;
        char *owned = realloc(a->owned, cap);
        if (!owned)
            return -1;
        a->owned = owned;
        a->cap = cap;
    }
    a->owned[a->len] = (char)own;
    a->v[a->len++] = (char *)s;
    a->v[a->len] = NULL;
    return 0;
}

/* Push `s`, rewritten if it names a baked path. */
static int push_rewritten(struct args *a, const char *s)
{
    char *r = rewrite(s);
    return r ? push(a, r, 1) : push(a, s, 0);
}

static void drop(struct args *a)
{
    for (size_t i = 0; i < a->len; i++)
        if (a->owned[i])
            free(a->v[i]);
    free(a->v);
    free(a->owned);
}

/* Push `program`, behind the bundle's loader if it needs it. */
static int push_program(struct args *a, const char *program)
{
    char *r = rewrite(program);
    const char *path = r ? r : program;
    if (self.bundled_glibc && from_bundle(path))
        if (push(a, self.loader, 0) || push(a, "--argv0", 0) || push(a, path, 0))
            goto fail;
    if (push(a, path, r != NULL))
        goto fail;
    return 0;
fail:
    free(r);
    return -1;
}

static const char *host_library_path(void)
{
    return getenv("ROSEMARY_HOST_LD_LIBRARY_PATH");
}

/* Bubblewrap's `--args` memfd, rewritten in place: same descriptor, new file. */
static int rewrite_bwrap_args(int fd, int command_from_host)
{
    size_t size = 0, cap = 4096;
    char *data = malloc(cap);
    if (!data)
        return -1;
    for (;;) {
        if (size == cap) {
            char *grown = realloc(data, cap *= 2);
            if (!grown)
                goto fail;
            data = grown;
        }
        ssize_t n = pread(fd, data + size, cap - size, (off_t)size);
        if (n < 0 && errno == EINTR)
            continue;
        if (n < 0)
            goto fail;
        if (n == 0)
            break;
        size += (size_t)n;
    }

    struct args out = {0};
    for (size_t at = 0; at < size;) {
        const char *arg = data + at;
        size_t len = strnlen(arg, size - at);
        at += len + 1;
        /* WebKit hands the sandbox its own LD_LIBRARY_PATH; a host program
         * needs the host's. */
        if (command_from_host && strcmp(arg, "--setenv") == 0 && at < size
            && strcmp(data + at, "LD_LIBRARY_PATH") == 0) {
            at += strlen(data + at) + 1;
            at += strnlen(data + at, size - at) + 1;
            const char *host = host_library_path();
            if (host && *host && (push(&out, "--setenv", 0) || push(&out, "LD_LIBRARY_PATH", 0) || push(&out, host, 0)))
                goto fail_out;
            continue;
        }
        /* nixpkgs' WebKit binds the store unconditionally, and bwrap refuses
         * to start on a host without one. Nothing from it is needed here. */
        if (strcmp(arg, "--ro-bind") == 0 && at < size && strcmp(data + at, NIX_STORE) == 0) {
            if (push(&out, "--ro-bind-try", 0))
                goto fail_out;
            continue;
        }
        if (push_rewritten(&out, arg))
            goto fail_out;
    }
    if (push(&out, "--ro-bind", 0) || push(&out, self.root, 0) || push(&out, self.root, 0))
        goto fail_out;

    int memfd = memfd_create("bwrap", MFD_CLOEXEC);
    if (memfd < 0)
        goto fail_out;
    for (size_t i = 0; i < out.len; i++) {
        const char *s = out.v[i];
        size_t left = strlen(s) + 1;
        while (left) {
            ssize_t n = write(memfd, s, left);
            if (n < 0 && errno == EINTR)
                continue;
            if (n <= 0) {
                close(memfd);
                goto fail_out;
            }
            s += n;
            left -= (size_t)n;
        }
    }
    /* bwrap reads from the current offset. The launcher already owns `fd`
     * by number and passes it to the child. */
    int ok = lseek(memfd, 0, SEEK_SET) == 0 && dup2(memfd, fd) == fd;
    close(memfd);
    drop(&out);
    free(data);
    return ok ? 0 : -1;

fail_out:
    drop(&out);
fail:
    free(data);
    return -1;
}

static int names_bwrap(const char *program)
{
    return strcmp(program, BAKED_BWRAP) == 0;
}

GSubprocess *g_subprocess_launcher_spawnv(GSubprocessLauncher *launcher, const char *const *argv, GError **error)
{
    static spawnv_fn next;
    if (!next)
        next = (spawnv_fn)dlsym(RTLD_NEXT, "g_subprocess_launcher_spawnv");
    if (!argv || !argv[0] || baked(argv[0]) < 0 || !located())
        return next(launcher, argv, error);

    struct args out = {0};
    const char *program = argv[0];
    size_t i = 1;
    if (names_bwrap(program)) {
        /* bwrap --args N -- COMMAND ARGS... */
        const char *command = NULL;
        for (size_t j = 1; argv[j]; j++)
            if (strcmp(argv[j], "--") == 0) {
                command = argv[j + 1];
                break;
            }
        char *rewritten = command ? rewrite(command) : NULL;
        int command_from_host = command && !from_bundle(rewritten ? rewritten : command);
        free(rewritten);
        if (push_program(&out, program))
            goto fail;
        for (; argv[i]; i++) {
            if (strcmp(argv[i], "--args") == 0 && argv[i + 1]) {
                if (rewrite_bwrap_args(atoi(argv[i + 1]), command_from_host) < 0)
                    goto fail;
                if (push(&out, argv[i], 0) || push(&out, argv[i + 1], 0))
                    goto fail;
                i++;
                continue;
            }
            if (strcmp(argv[i], "--") == 0) {
                if (push(&out, argv[i], 0))
                    goto fail;
                i++;
                if (argv[i] && push_program(&out, argv[i++]))
                    goto fail;
                break;
            }
            if (push_rewritten(&out, argv[i]))
                goto fail;
        }
    } else if (push_program(&out, program)) {
        goto fail;
    }
    for (; argv[i]; i++)
        if (push_rewritten(&out, argv[i]))
            goto fail;

    /* bwrap itself, when it is the host's. */
    if (!from_bundle(out.v[0])) {
        static setenv_fn set;
        static unsetenv_fn unset;
        if (!set) {
            set = (setenv_fn)dlsym(RTLD_DEFAULT, "g_subprocess_launcher_setenv");
            unset = (unsetenv_fn)dlsym(RTLD_DEFAULT, "g_subprocess_launcher_unsetenv");
        }
        const char *host = host_library_path();
        if (host && *host && set)
            set(launcher, "LD_LIBRARY_PATH", host, 1);
        else if (unset)
            unset(launcher, "LD_LIBRARY_PATH");
    }

    GSubprocess *process = next(launcher, (const char *const *)out.v, error);
    drop(&out);
    return process;

fail:
    drop(&out);
    /* Run what WebKit asked for; it reports the failure better than this can. */
    return next(launcher, argv, error);
}

/* The host's environment for a host program: the bundle's LD_LIBRARY_PATH
 * swapped for the one the launcher saved. */
static char **host_environment(char **allocated)
{
    extern char **environ;
    size_t n = 0;
    while (environ[n])
        n++;
    char **env = calloc(n + 2, sizeof *env);
    if (!env)
        return NULL;
    size_t k = 0;
    for (size_t j = 0; j < n; j++)
        if (strncmp(environ[j], "LD_LIBRARY_PATH=", 16) != 0)
            env[k++] = environ[j];
    const char *host = host_library_path();
    if (host && *host) {
        char *entry = malloc(strlen(host) + 17);
        if (entry)
            env[k++] = *allocated = strcat(strcpy(entry, "LD_LIBRARY_PATH="), host);
    }
    return env;
}

/* WebKit's check that bwrap works inside a container (WTF::Sandbox). */
int g_spawn_sync(const char *working_directory, char **argv, char **envp, int flags,
                 GSpawnChildSetupFunc child_setup, void *user_data, char **standard_output,
                 char **standard_error, int *wait_status, GError **error)
{
    static spawn_sync_fn next;
    if (!next)
        next = (spawn_sync_fn)dlsym(RTLD_NEXT, "g_spawn_sync");
    if (!argv || !argv[0] || envp || !names_bwrap(argv[0]) || !located())
        return next(working_directory, argv, envp, flags, child_setup, user_data, standard_output,
                    standard_error, wait_status, error);

    struct args out = {0};
    char **env = NULL;
    char *entry = NULL;
    if (push_program(&out, argv[0]))
        goto fail;
    for (size_t i = 1; argv[i]; i++)
        if (push_rewritten(&out, argv[i]))
            goto fail;
    if (!from_bundle(out.v[0]) && !(env = host_environment(&entry)))
        goto fail;
    int result = next(working_directory, out.v, env, flags, child_setup, user_data, standard_output,
                      standard_error, wait_status, error);
    free(entry);
    free(env);
    drop(&out);
    return result;

fail:
    drop(&out);
    return next(working_directory, argv, envp, flags, child_setup, user_data, standard_output,
                standard_error, wait_status, error);
}
