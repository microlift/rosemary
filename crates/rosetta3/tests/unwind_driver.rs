pub const FIXTURE: &str = r#"/* THE FIXTURE.  A Mach-O that reproduces the configuration `Chromium Framework`
 * actually runs in, and that is the *only* configuration in which this tree's
 * driver is reachable.
 *
 * ## The shape being reproduced, and why it is this shape
 *
 * The Framework throws through exactly one imported name,
 * `__Unwind_RaiseException`, at two call sites, and both are inside its own
 * **statically linked** `//third_party/libc++abi`: `Framework+0x5680840` is a
 * private *guest* `__cxa_throw` that builds the `_Unwind_Exception` on the
 * stack and calls `__Unwind_RaiseException` at `+0x568089c`.  The personality
 * is likewise the image's own - `0x5680bd0` - named by `__TEXT,__unwind_info`
 * for 3,435 of its 3,488 frames.  So the two halves of the ABI are guest code
 * and the *driver* is an import.
 *
 * That is the crossing, and it is the only one.  A fixture that links a real
 * libc++abi resolves every `_Unwind_*` **into that image**, and
 * `Vm::call_site` transfers the call to the guest before `shims::dispatch` is
 * ever consulted - so the vendored unwinder runs and ours never does.  This
 * fixture therefore *defines* the whole `__cxa_*` ABI itself and imports only
 * `_Unwind_RaiseException` and the seven context accessors, which is precisely
 * what the static linker produces inside the Framework.
 *
 * ## What makes a bypass fail rather than pass quietly
 *
 * A fixture that merely ran would be worth nothing: the vendored unwinder could
 * run it perfectly while ours sat out, and a green test would be a lie.  So
 * three things are arranged deliberately, and each is checked by the test:
 *
 *  * `personality calls` is printed and must be non-zero.  A driver that is
 *    entered but does nothing leaves it zero, and `main` reports `NO-PERSONALITY`
 *    and exits non-zero rather than reporting success.
 *  * `lsda` is printed, and the test checks it is a **guest** address inside
 *    this image's own `__TEXT,__gcc_except_tab`.  The personality dereferences
 *    it (`lsda[0]`, printed too) exactly as the Framework's does at
 *    `0x5680cf6`, so a driver that handed over one of the context's own five
 *    words would hand the personality eight bytes of zeroes to parse as a type
 *    table.
 *  * `ip`, `ip_before` and the derived `pc` are printed, and the test checks
 *    `pc == ip - 1` lands inside the frame's own call-site range.  The
 *    Framework's live personality computes `ip - region - 1` itself
 *    (`not r9` at `0x5680d1f`, `add r8,r9` at `0x5680d98` over the `_Unwind_GetIP`
 *    result), so a driver that pre-decremented would land one byte before
 *    every call and the lookup would miss.
 */

#include <stdint.h>

typedef unsigned long long r3_word;
typedef long long r3_sword;
typedef unsigned long r3_ptr;

struct _Unwind_Context;
struct _Unwind_Exception;

/* The nine imports.  `extern "C"` because a C++ declaration would mangle them
 * into names this image does not import - the project's standing third way to
 * be wrong, and the one `shims::dispatch` reduces before it looks a name up. */
extern "C" {
int _Unwind_RaiseException(struct _Unwind_Exception *);
r3_ptr _Unwind_GetIPInfo(struct _Unwind_Context *, int *);
void _Unwind_SetGR(struct _Unwind_Context *, int, r3_word);
void _Unwind_SetIP(struct _Unwind_Context *, r3_ptr);
r3_ptr _Unwind_GetRegionStart(struct _Unwind_Context *);
void *_Unwind_GetLanguageSpecificData(struct _Unwind_Context *);
}

extern "C" int printf(const char *, ...);

/* ------------------------------------------------------------------ trace --- */
/* Not instrumentation for its own sake.  It is how the test reads what the
 * personality was *handed*, rather than taking the driver's word for it: a
 * driver that reported success while offering a host address would still leave
 * these numbers, and the numbers are what the test asserts on. */
#define R3_TRACE_MAX 64
struct r3_trace_entry {
    const char *what;
    r3_word a, b, c;
};
static struct r3_trace_entry r3_trace[R3_TRACE_MAX];
static int r3_trace_len;
static int r3_personality_calls;

static void r3_record(const char *what, r3_word a, r3_word b, r3_word c) {
    if (r3_trace_len < R3_TRACE_MAX) {
        struct r3_trace_entry *e = &r3_trace[r3_trace_len++];
        e->what = what;
        e->a = a;
        e->b = b;
        e->c = c;
    }
}

/* ------------------------------------------------------------------ codes --- */
#define R3_URC_FATAL_PHASE1_ERROR 3
#define R3_URC_HANDLER_FOUND 6
#define R3_URC_INSTALL_CONTEXT 7
#define R3_URC_CONTINUE_UNWIND 8

#define R3_UA_SEARCH_PHASE 1
#define R3_UA_CLEANUP_PHASE 2
#define R3_UA_HANDLER_FRAME 4

/* `DW_EH_PE_*`, the subset that appears. */
#define PE_OMIT 0xff
#define PE_ABSPTR 0x00
#define PE_ULEB128 0x01
#define PE_UDATA2 0x02
#define PE_UDATA4 0x03
#define PE_UDATA8 0x04
#define PE_SLEB128 0x09
#define PE_SDATA2 0x0a
#define PE_SDATA4 0x0b
#define PE_SDATA8 0x0c
#define PE_PCREL 0x10
#define PE_INDIRECT 0x80

static r3_ptr r3_read_uleb(const unsigned char **p) {
    r3_ptr v = 0;
    unsigned shift = 0;
    unsigned char b;
    do {
        b = *(*p)++;
        v |= (r3_ptr)(b & 0x7f) << shift;
        shift += 7;
    } while (b & 0x80);
    return v;
}

static r3_sword r3_read_sleb(const unsigned char **p) {
    r3_sword v = 0;
    unsigned shift = 0;
    unsigned char b;
    do {
        b = *(*p)++;
        v |= (r3_sword)(b & 0x7f) << shift;
        shift += 7;
    } while (b & 0x80);
    if (shift < 64 && (b & 0x40))
        v |= -((r3_sword)1 << shift);
    return v;
}

/* Bytes one value of `enc` occupies, ignoring the indirect bit.  A personality
 * has to know this to index the type table, and getting it wrong reads the
 * *wrong type* rather than crashing - the most expensive kind of wrong. */
static unsigned r3_encoded_size(unsigned enc) {
    switch (enc & 0x0f) {
    case PE_ABSPTR: return 8;
    case PE_UDATA2: case PE_SDATA2: return 2;
    case PE_UDATA4: case PE_SDATA4: return 4;
    case PE_UDATA8: case PE_SDATA8: return 8;
    default: return 0; /* uleb128 / sleb128: variable length */
    }
}

static r3_ptr r3_read_encoded(const unsigned char **p, unsigned enc, r3_ptr base) {
    r3_ptr v;
    unsigned size = r3_encoded_size(enc);
    if (size == 0) {
        if ((enc & 0x0f) == PE_ULEB128) v = r3_read_uleb(p);
        else v = (r3_ptr)r3_read_sleb(p);
    } else {
        v = 0;
        for (unsigned i = 0; i < size; i++)
            v |= (r3_ptr)(*p)[i] << (8 * i);
        *p += size;
    }
    if (enc & PE_PCREL) v += base;
    if (enc & PE_INDIRECT) v = *(r3_ptr *)v;
    return v;
}

/* libc++abi's `kOurExceptionClass`, held as a `uint64_t` the way the Itanium
 * ABI holds it - byte-reversed relative to its `char[8]` spelling.  The
 * Framework's own throw stamps `0x434c4e47432b2b00` (`0x5680855`), and its
 * personality masks the low byte before comparing (`and rbx,0xff...00` at
 * `0x5680c1b`), so this does the same. */
#define R3_CLASS ((r3_word)0x434c4e47432b2b00ULL)

/* ------------------------------------------------------- the personality --- */
/* `__gxx_personality_v0`, defined **in this image** so `__TEXT,__unwind_info`
 * names it, which is the Framework's arrangement and the reason the driver can
 * call guest code at all.
 *
 * SCOPE, stated rather than implied: it walks the LSDA, finds the call site
 * covering the pc, follows the action chain for a type match, and installs the
 * landing pad.  It does not implement destructors, exception specifications or
 * longjmp buffers, because this fixture has none of them and a personality that
 * claimed to would be untested. */
extern "C" int __gxx_personality_v0(int version, int actions, r3_word exc_class,
                                    struct _Unwind_Exception *exc,
                                    struct _Unwind_Context *ctx) {
    r3_personality_calls++;
    r3_record("enter", (r3_word)version, (r3_word)actions, exc_class);
    if (version != 1) {
        r3_record("bad-version", (r3_word)version, 0, 0);
        return R3_URC_FATAL_PHASE1_ERROR;
    }
    if ((exc_class & ~0xffULL) != (R3_CLASS & ~0xffULL)) {
        r3_record("class-mismatch", exc_class, R3_CLASS, 0);
        return R3_URC_CONTINUE_UNWIND;
    }

    /* INVARIANT 1.  Dereferenced on the next line, as the Framework's does at
     * `0x5680cf6` (`movzx eax,BYTE PTR [r13+0x0]`), and the pointer is printed
     * so the test can check it is guest memory. */
    unsigned char *lsda = (unsigned char *)_Unwind_GetLanguageSpecificData(ctx);
    r3_record("lsda", (r3_ptr)lsda, 0, 0);
    if (!lsda) {
        /* The one value that means "nothing here", and it routes outward. */
        r3_record("no-lsda", 0, 0, 0);
        return R3_URC_CONTINUE_UNWIND;
    }
    r3_record("lsda-first-byte", lsda[0], 0, 0);

    /* INVARIANT 2.  `GetIPInfo` hands back the *return address*; the decrement
     * is this function's job.  The Framework's live copy computes
     * `ip - region_start - 1` itself (`not r9` at `0x5680d1f` over the
     * `_Unwind_GetIP` result, `add r8,r9` at `0x5680d98`), and its other copy
     * does it with the accessor: `cmp DWORD PTR [rbp-0x2c],0x1; sbb r12,0x0` at
     * `0x41768e`.  Both subtract exactly once. */
    int ip_before = 0;
    r3_ptr ip = _Unwind_GetIPInfo(ctx, &ip_before);
    r3_ptr region = _Unwind_GetRegionStart(ctx);
    r3_ptr pc = ip_before ? ip - 1 : ip;
    r3_record("ip", ip, (r3_word)(unsigned long)ip_before, region);
    r3_record("pc", pc, region, pc >= region ? pc - region : 0);

    const unsigned char *base = lsda;
    const unsigned char *p = lsda;

    /* LSDA header: LPStart encoding, LPStart, TType encoding, TType offset,
     * call-site encoding, call-site table length.  The TType offset counts from
     * the byte *after* itself to the **end** of the type table, which is what
     * LLVM's `EHStreamer` emits with
     * `emitLabelDifferenceAsULEB128(TTBaseLabel, TTBaseRefLabel)`. */
    unsigned lp_enc = *p++;
    if (lp_enc != PE_OMIT)
        (void)r3_read_encoded(&p, lp_enc, (r3_ptr)(p - base));
    unsigned tt_enc = *p++;
    r3_ptr tt_end = 0;
    unsigned tt_size = 0;
    if (tt_enc != PE_OMIT) {
        /* The field is an offset from the byte *after itself* to the end of the
         * type table - LLVM's `emitLabelDifferenceAsULEB128(TTBaseLabel,
         * TTBaseRefLabel)`, with `TTBaseRefLabel` placed right here.  The value
         * is read first, because `p` has to have moved before its position is
         * used. */
        r3_ptr rel = r3_read_uleb(&p);
        tt_end = (r3_ptr)base + (r3_ptr)(p - base) + rel;
        tt_size = r3_encoded_size(tt_enc);
        r3_record("ttype", tt_end, (r3_word)tt_enc, tt_size);
    }
    unsigned cs_enc = *p++;
    r3_ptr cs_len = r3_read_uleb(&p);
    const unsigned char *cs = p;
    const unsigned char *cs_end = p + cs_len;
    r3_record("callsite-table", (r3_ptr)cs, cs_len, cs_enc);

    r3_ptr ip_off = pc >= region ? pc - region : 0;
    r3_ptr hit_start = 0, hit_len = 0, hit_pad = 0, hit_action = 0;
    int have = 0;
    while (p < cs_end) {
        r3_ptr s = r3_read_encoded(&p, cs_enc, region);
        r3_ptr l = r3_read_encoded(&p, cs_enc, region);
        r3_ptr pd = r3_read_encoded(&p, cs_enc, region);
        r3_ptr a = r3_read_uleb(&p);
        if (!have && s <= ip_off && ip_off < s + l) {
            hit_start = s; hit_len = l; hit_pad = pd; hit_action = a;
            have = 1;
        }
    }
    if (!have) {
        r3_record("no-callsite", ip_off, 0, 0);
        return R3_URC_CONTINUE_UNWIND;
    }
    r3_record("callsite-hit", hit_start, hit_len, (hit_pad << 32) | hit_action);
    if (!hit_pad) {
        /* In range, but nothing to run in this frame. */
        return R3_URC_CONTINUE_UNWIND;
    }
    r3_ptr pad_addr = region + hit_pad;
    r3_record("pad", pad_addr, hit_action, 0);

    /* The action table follows the call-site table as (type index, next) sleb
     * pairs, chained by byte offsets from the start of the table.  `hit_action`
     * is 1-based. */
    r3_ptr want = *(r3_ptr *)((char *)exc - 0x50);
    const unsigned char *rec[64];
    int nrec = 0;
    const unsigned char *a = cs_end;
    while (a < (const unsigned char *)tt_end && nrec < 64) {
        rec[nrec++] = a;
        (void)r3_read_sleb(&a);
        if (r3_read_sleb(&a) == 0) break;
    }
    r3_sword index = (r3_sword)hit_action;
    r3_ptr selector = 0;
    while (index > 0 && index <= nrec) {
        const unsigned char *cur = rec[index - 1];
        r3_sword type_index = r3_read_sleb(&cur);
        r3_sword next = r3_read_sleb(&cur);
        r3_record("action-record", (r3_word)type_index, (r3_word)next, (r3_ptr)index);
        if (type_index <= 0) {
            /* A catch-all, or a filter this fixture does not implement. */
            selector = 1;
            break;
        }
        r3_ptr slot = tt_end - (r3_ptr)tt_size * (r3_ptr)type_index;
        const unsigned char *q = (const unsigned char *)slot;
        r3_ptr got = r3_read_encoded(&q, tt_enc, slot);
        r3_record("type-entry", slot, got, (r3_ptr)type_index);
        if (got == want) {
            selector = (r3_ptr)type_index;
            break;
        }
        index = next;
    }
    if (!selector) {
        r3_record("no-match", want, hit_action, 0);
        return R3_URC_CONTINUE_UNWIND;
    }
    r3_record("match", selector, want, 0);

    if (actions & R3_UA_SEARCH_PHASE) {
        r3_record("search-handler", selector, (r3_word)actions, 0);
        return R3_URC_HANDLER_FOUND;
    }
    if ((actions & R3_UA_CLEANUP_PHASE) && (actions & R3_UA_HANDLER_FRAME)) {
        _Unwind_SetGR(ctx, 0, (r3_word)exc);
        _Unwind_SetGR(ctx, 1, selector);
        _Unwind_SetIP(ctx, pad_addr);
        r3_record("installed", (r3_word)exc, selector, pad_addr);
        return R3_URC_INSTALL_CONTEXT;
    }
    return R3_URC_CONTINUE_UNWIND;
}

/* ------------------------------------------------------------ __cxa_throw --- */
/* The Framework's own `__cxa_throw` is a private guest function
 * (`Framework+0x5680840`) that builds the `_Unwind_Exception` on the stack and
 * calls `__Unwind_RaiseException` at `+0x568089c`.  This is the same shape, and
 * the offsets are the ones measured out of that code:
 *
 *   +0x00 exception_class   0x5680855  `mov QWORD PTR [rcx-0x20],0x434c4e47432b2b00`
 *   +0x08 exception_cleanup 0x5680890  `mov QWORD PTR [rcx-0x18],0x56807c0`
 *   +0x20 the thrown object 0x56807de  `lea rdi,[rbx+0x20]; call rax`
 *
 * and `exceptionType` at `exc - 0x50`, from `0x5680aab` masking
 * `QWORD PTR [r15+0x60]` and `0x5680ac4` passing `r15+0x60` to the driver.
 */
struct r3_unwind_exception {
    r3_word exception_class;
    void (*exception_cleanup)(void *);
    r3_word private_1;
    r3_word private_2;
    char thrown[16];
};

/* One object, big enough for the whole `__cxa_exception` the personality reads
 * below the header.  Static, so the addresses the personality prints stay put. */
static char r3_exc_storage[0x100];

extern "C" void __cxa_throw(void *obj, void *typeinfo, void (*dtor)(void *)) {
    struct r3_unwind_exception *exc = (struct r3_unwind_exception *)r3_exc_storage;
    exc->exception_class = R3_CLASS;
    exc->exception_cleanup = 0;
    exc->private_1 = 0;
    exc->private_2 = 0;
    *(void **)(r3_exc_storage - 0x50) = typeinfo;
    /* The thrown object belongs at `exc + 0x20` in libc++abi's own layout,
     * where `__cxa_begin_catch` finds it.  The compiler allocated it wherever
     * it liked - an imported `__cxa_allocate_exception`, so a host address
     * here - and copying it in is what makes the two halves agree.  The size is
     * the one this fixture throws, `int`, and it is written down rather than
     * guessed at: the fixture throws exactly one type. */
    *(int *)exc->thrown = *(const int *)obj;
    (void)dtor;
    r3_record("throw", (r3_ptr)exc, (r3_ptr)typeinfo, (r3_ptr)obj);
    _Unwind_RaiseException((struct _Unwind_Exception *)exc);
    /* Unreachable on any system: a driver that returns has not entered a
     * landing pad, and saying so is the only honest thing left to do. */
    printf("BUG: _Unwind_RaiseException returned\n");
}

/* The landing pad calls both, exactly as the Framework's does (its copy at
 * `0x56807de` and the `__cxa_end_catch` at the end of the handler).  The thrown
 * object is at `exc + 0x20`, so that is what `begin_catch` hands back. */
extern "C" void *__cxa_begin_catch(void *exc) { return (char *)exc + 0x20; }
extern "C" void __cxa_end_catch(void *exc) { (void)exc; }

/* ----------------------------------------------------------------- driver --- */
__attribute__((noinline)) static void thrower(int v) { throw v; }

/* A second function, defined *after* `thrower` and never on the throw path, and
 * it is here for a measured reason rather than for the unwinder.
 *
 * `thrower` ends with its `call __cxa_throw`, so the return address that frame
 * contributes is one byte past the last instruction in the image - and
 * `machish::image::UnwindInfo::lookup` rejects `va >= end`, where `end` is the
 * last first-level index entry's function offset.  That is a real limitation of
 * the parser (reported, not worked around: a return address at the very end of
 * an image is legal, and the check exists to stop the *rest of the address
 * space* being attributed to the last function), but it has nothing to do with
 * this driver's protocol, and a fixture whose only job is to exercise that
 * protocol should not also be the thing that trips an unrelated edge.
 *
 * Putting a function after `thrower` moves its return address inside the
 * covered range, where it is in every real image.
 */
__attribute__((noinline)) static int twice(int v) { return v * 2; }

int main(void) {
    int caught = -1;
    try {
        thrower(42);
    } catch (int v) {
        caught = twice(v);
    }

    printf("caught=%d\n", caught);
    printf("personality-calls=%d\n", r3_personality_calls);
    for (int i = 0; i < r3_trace_len; i++)
        printf("trace %d %s %llx %llx %llx\n", i, r3_trace[i].what,
               (unsigned long long)r3_trace[i].a, (unsigned long long)r3_trace[i].b,
               (unsigned long long)r3_trace[i].c);

    /* The arrangement that makes a bypass *fail*.  A driver that was never
     * entered, or entered and did nothing, must not be able to report success:
     * this is a non-zero exit and a named line, not a skip. */
    if (r3_personality_calls == 0) {
        printf("NO-PERSONALITY\n");
        return 3;
    }
    if (caught != 84) {
        printf("NOT-CAUGHT\n");
        return 4;
    }
    printf("OK\n");
    return 0;
}
"#;




// ---------------------------------------------------------------------------
// Everything below is the harness.  The fixture above is the instrument; this is
// what makes a bypassed driver fail rather than pass quietly.
// ---------------------------------------------------------------------------

mod common;

use std::path::PathBuf;
use std::process::Command;

/// Build the fixture with `test/tools/build-macho.sh`, or `None` if this host
/// has no Mach-O toolchain.
///
/// The script is used directly rather than `common::can_build_x86_64()`, which
/// shells out to `cc -arch x86_64` and is false on a host whose `cc` is gcc.
/// Pointing `can_build_x86_64()` at clang and this script is the durable fix and
/// belongs in `common`, which another agent owns; see
/// `test/logs/macho-fixtures.md`.
fn build() -> Option<String> {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let root = ROOT.get_or_init(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test/tools/build-macho.sh")
            .canonicalize()
            .expect("test/tools/build-macho.sh")
    });
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fixtures");
    std::fs::create_dir_all(&dir).ok()?;
    let src = dir.join("unwind_driver.cpp");
    let out = dir.join("unwind_driver");
    std::fs::write(&src, FIXTURE).ok()?;
    let status = Command::new(root)
        .arg(&src)
        .arg(&out)
        // `-fexceptions` for the LSDA and the landing pad, `-fno-rtti` because
        // the personality is this file's and needs no type graph, `-O0` so
        // every frame has a frame pointer the compact unwind can describe.
        .args(["-x", "c++", "-O0", "-fexceptions", "-fno-rtti"])
        .env("TMPDIR", "/home/frolleks/tmp")
        .output()
        .ok()?;
    if !status.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&status.stderr));
        return None;
    }
    Some(out.to_string_lossy().into_owned())
}

fn fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT.get_or_init(build).clone()
}

/// Run the fixture, with the driver gate open or shut.
fn run(path: &str, gate: bool) -> (Option<i32>, String, String) {
    let mut command = common::driver_command();
    command
        .arg("run")
        .arg("--emulated")
        .arg(path)
        .env("TMPDIR", "/home/frolleks/tmp");
    if gate {
        command.env("ROSETTA3_NATIVE_UNWINDER", "1");
    } else {
        command.env_remove("ROSETTA3_NATIVE_UNWINDER");
    }
    let out = command.output().expect("running the driver");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// One `trace N what a b c` line the fixture printed.
struct Trace {
    what: String,
    a: u64,
    b: u64,
    c: u64,
}

fn traces(stdout: &str) -> Vec<Trace> {
    stdout
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("trace ")?;
            let mut f = rest.split_whitespace();
            let _n = f.next()?;
            Some(Trace {
                what: f.next()?.to_string(),
                a: u64::from_str_radix(f.next()?, 16).ok()?,
                b: u64::from_str_radix(f.next()?, 16).ok()?,
                c: u64::from_str_radix(f.next()?, 16).ok()?,
            })
        })
        .collect()
}

fn first<'a>(t: &'a [Trace], what: &str) -> &'a Trace {
    t.iter()
        .find(|e| e.what == what)
        .unwrap_or_else(|| {
            let seen: Vec<&str> = t.iter().map(|e| e.what.as_str()).collect();
            panic!("the personality never recorded {what:?}; it recorded {seen:?}")
        })
}

/// The ranges the loader mapped for the fixture, from the report itself.
fn guest_segments(path: &str) -> Vec<(u64, u64)> {
    let program = rosetta3::Program::load(path, &[]).expect("loading the fixture");
    program
        .report
        .images
        .iter()
        .flat_map(|i| &i.segments)
        .map(|s| (s.vmaddr, s.vmaddr.wrapping_add(s.vmsize)))
        .collect()
}

// ---------------------------------------------------------------------------
// The four claims.  Each is one thing that would be invisible if the driver had
// not run, which is the property that makes them worth having.
// ---------------------------------------------------------------------------

/// **The driver runs.**  Until this test existed nothing in the tree had ever
/// entered `unwind_native`, so this is the whole answer to "does the driver
/// work?" in one assertion.
///
/// It is worth being precise about what the fixture had to reproduce to get
/// here, because the shape is not a convenience:
///
/// * the `__cxa_*` ABI is **defined in the fixture**, so `_Unwind_RaiseException`
///   is an import of the image - which is what the Framework does, where
///   `Framework+0x5680840` is a private guest `__cxa_throw` and the driver is
///   the import;
/// * `__gxx_personality_v0` is likewise the image's own, so
///   `__TEXT,__unwind_info` names a personality *inside* the image, as the
///   Framework's names `0x5680bd0` for 3,435 of its 3,488 frames;
/// * and that is only reachable because the fixture is built **without** a
///   libc++abi.  A fixture that links one resolves every `_Unwind_*` into that
///   image, and `Vm::call_site` transfers the call to the guest before
///   `shims::dispatch` is consulted - which is why `tests/unwind.rs` passes with
///   the gate shut and tests nothing here.
#[test]
fn the_driver_walks_the_stack_and_lands_in_the_guests_landing_pad() {
    let Some(path) = fixture() else {
        skip!("no Mach-O toolchain on this host; see test/logs/macho-fixtures.md")
    };
    let (status, out, err) = run(&path, true);

    assert_eq!(
        status,
        Some(0),
        "the fixture's own check did not pass\nstdout:\n{out}\nstderr:\n{err}"
    );
    assert!(out.contains("OK\n"), "the fixture did not report OK:\n{out}");
    // Two phases, two calls: `_UA_SEARCH_PHASE` and
    // `_UA_CLEANUP_PHASE|_UA_HANDLER_FRAME`.  One would mean the driver found a
    // handler without looking for one.
    assert!(
        out.contains("personality-calls=2\n"),
        "the personality was entered {} times, and two is the number two phases need:\n{out}",
        out.lines()
            .find(|l| l.starts_with("personality-calls="))
            .unwrap_or("(not reported)")
    );
    // And the landing pad really ran, with the value the thrower put there: the
    // `catch (int v) { caught = twice(v); }` in the fixture, so 84 and not 42.
    assert!(
        out.contains("caught=84\n"),
        "the handler did not run with the thrown value:\n{out}"
    );
}

/// **The positive control, and the reason the other three mean anything.**
///
/// A test that passes whether or not the driver runs is worse than no test,
/// because it looks like evidence.  So the same binary is run with the gate shut
/// and the run is required to **fail**.
///
/// It fails loudly, and the way it fails is worth stating because it is what
/// "refuse by name" is supposed to look like: with the gate shut nothing in
/// this tree claims `_Unwind_RaiseException`, so the guest's call is refused -
/// `rosetta3: __Unwind_RaiseException: no signature; add one to
/// rosetta3::abi::table` - and the process exits non-zero.  The fixture's own
/// arrangement is the second line of defence: `main` prints `NO-PERSONALITY` and
/// exits 3 if its personality was never called, so a bypass that *were* answered
/// by something else would still not be able to report success.
///
/// If this test ever starts passing with the gate shut, the other three have
/// stopped measuring this tree's driver and started measuring whatever else
/// answers `_Unwind_RaiseException`, and that is the failure to look at.
#[test]
fn with_the_gate_shut_the_same_run_fails() {
    let Some(path) = fixture() else {
        skip!("no Mach-O toolchain on this host; see test/logs/macho-fixtures.md")
    };
    let (status, out, err) = run(&path, false);

    assert_ne!(
        status,
        Some(0),
        "the fixture succeeded with ROSETTA3_NATIVE_UNWINDER unset, so it is not \
         measuring this tree's driver at all - something else answered \
         _Unwind_RaiseException\nstdout:\n{out}\nstderr:\n{err}"
    );
    assert!(
        !out.contains("OK\n"),
        "the fixture reported OK with the gate shut:\n{out}"
    );
    // The guest is not expected to get as far as printing: with the gate shut
    // nothing in this tree claims `_Unwind_RaiseException`, so the call is
    // refused by name.  What matters is that it is refused **loudly** and
    // **by that name** - a silent answer is what a false success looks like.
    assert!(
        err.contains("_Unwind_RaiseException"),
        "the gate-shut run failed without naming the name it could not answer, so \
         there is no way to tell a refusal from an accident\nstdout:\n{out}\nstderr:\n{err}"
    );
}

/// **Invariant 1: `lsda` is a guest address.**
///
/// The personality *dereferences* the answer - the Framework's does at
/// `0x5680cf6`, `movzx eax,BYTE PTR [r13+0x0]` - and walks it as a ULEB table.
/// So there are two separate claims, and this test makes both:
///
/// * the pointer lies inside a segment **this tree mapped for this image**, read
///   out of the link report rather than out of a constant.  A driver that
///   offered one of the `_Unwind_Context`'s own five words would offer a *host*
///   address - the context is a host thread-local - and that is outside every
///   segment here;
/// * and the byte at it is `0xff`, the LSDA's own first byte, which is only
///   readable if it really is the guest's `__gcc_except_tab`.
///
/// `0` is the one value that means "nothing here" and routes to
/// `_URC_CONTINUE_UNWIND`, so a null would not be a wrong answer but the
/// absence of one; the assertion is `!= 0` *and* inside the image.
#[test]
fn the_personality_is_offered_a_guest_address_for_the_lsda() {
    let Some(path) = fixture() else {
        skip!("no Mach-O toolchain on this host; see test/logs/macho-fixtures.md")
    };
    let (_, out, _err) = run(&path, true);
    let t = traces(&out);
    let lsda = first(&t, "lsda").a;

    assert_ne!(lsda, 0, "a null language-specific pointer means 'no try here':\n{out}");
    let segments = guest_segments(&path);
    let ranges: Vec<String> = segments.iter().map(|(a, b)| format!("{a:#x}..{b:#x}")).collect();
    assert!(
        segments.iter().any(|(a, b)| lsda >= *a && lsda < *b),
        "the LSDA {lsda:#x} is outside every segment the loader mapped for this \
         image, so it is not a guest address - segments {ranges:?}\nstdout:\n{out}"
    );
    // And it was dereferenced, successfully, by guest code.
    assert_eq!(
        first(&t, "lsda-first-byte").a,
        0xff,
        "the first byte of the table is DW_EH_PE_omit, and reading it is what \
         makes this a guest pointer rather than a host one:\n{out}"
    );
}

/// **Invariant 2: `GetIP` must not pre-decrement.**
///
/// The Itanium rule is that the accessor hands back the **return address** and
/// the *caller* turns it into a call-site offset.  The Framework's own code
/// shows both ways of doing that: its live personality computes `ip - region -
/// 1` itself (`not r9` at `0x5680d1f` over the `_Unwind_GetIP` result, `add
/// r8,r9` at `0x5680d98`), and its other copy does it with the accessor -
/// `cmp DWORD PTR [rbp-0x2c],0x1; sbb r12,0x0` at `0x41768e`.  Exactly one
/// decrement either way.
///
/// **And a plain "it unwound" test cannot see this**, which is why the check is
/// written the way it is.  The personality records the call site it hit:
/// `start` and `len`, and the region start it measured from.  The two
/// candidates differ by one byte, and the call-site range is closed at the top:
/// `start <= off < start + len`.  So
///
/// * `pc - region` (the personality's own `ip - 1`) is **inside** the range, and
/// * `ip - region` (the value `GetIP` returned) is **outside** it.
///
/// If this driver pre-decremented inside `GetIP`, the personality would
/// subtract again, land one byte lower still, and *miss the range entirely* -
/// which is a clean "no handler", not a wrong one.  The discriminator is that
/// the *recorded* `ip` is the one that misses.  A driver that hands back a
/// pre-decremented `ip` fails the second half of this assertion, and one that
/// hands back `ip - 1` with `ip_before == 1` fails the first.
#[test]
fn getip_returns_the_return_address_and_not_the_instruction_before_it() {
    let Some(path) = fixture() else {
        skip!("no Mach-O toolchain on this host; see test/logs/macho-fixtures.md")
    };
    let (_, out, err) = run(&path, true);
    let t = traces(&out);

    let ip = first(&t, "ip");
    let (addr, before, region) = (ip.a, ip.b, ip.c);
    assert_eq!(before, 1, "`ip_before` must say the ip is after a call:\n{out}");
    assert!(
        addr > region,
        "the ip {addr:#x} is not above its own region start {region:#x}:\n{out}"
    );

    // The range the personality actually matched, and the pad it chose.
    let hit = first(&t, "callsite-hit");
    let (start, len, packed) = (hit.a, hit.b, hit.c);
    let pad_off = packed >> 32;
    assert!(len > 0, "the matched call site is empty:\n{out}");

    // The call site table is addressed from the region start, and the personality
    // records `pc - region` in its own `pc` line's third word.
    let pc_off = first(&t, "pc").c;
    assert_eq!(
        pc_off,
        addr.wrapping_sub(region).wrapping_sub(1),
        "the personality's pc is not `ip - 1`"
    );

    let (end, _) = (start + len, region + start + len);
    assert!(
        start <= pc_off && pc_off < end,
        "the personality's own pc (offset {pc_off:#x}) is not inside the call site \
         it selected [{start:#x}, {end:#x}) - so the walk found a landing \
         pad for the wrong instruction\n{out}"
    );
    assert!(
        addr.wrapping_sub(region) >= end,
        "the ip `GetIP` returned is itself inside the call site [{start:#x}, \
         {end:#x}) at offset {:#x} from the region start {region:#x}, which means it \
         was already decremented and the personality subtracted a second time. The \
         range is closed at the top precisely so this is visible. The pad offset \
         was {pad_off:#x}.\n{out}",
        addr.wrapping_sub(region)
    );
    // And the pad is the region-relative offset the call site carried, which is
    // the last thing `_Unwind_GetRegionStart` is *for*.
    let pad = first(&t, "pad").a;
    assert_eq!(
        pad,
        region + pad_off,
        "the landing pad is not region_start + the call site's pad offset"
    );
    // And the install wrote that pad into the program counter.
    let installed = first(&t, "installed");
    assert_eq!(installed.c, pad, "SetIP did not carry the landing pad");
}
