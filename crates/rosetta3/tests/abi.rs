mod common;

use std::ffi::CString;

use machish::memory::{AddressSpace, BufferSpace, Placement};

use microlift::arch::x86_64::context::RegisterContext;
use microlift::function_traits::FunctionArgumentType as T;

use rosetta3::abi::table::{self, FunctionAbi, Lookup};
use rosetta3::abi::{AbiError, GuestMemory, marshal};

/// Where the synthetic guest stack lives. Nothing is mapped there for real -
/// it is a `BufferSpace`, so only the marshaller's own reads go through it.
const STACK_BASE: u64 = 0x2_0000_0000;
const STACK_SIZE: usize = 0x1000;

/// An argument, in the form the caller wants to write it.
#[derive(Clone, Copy, Debug)]
enum A {
    /// An integer or pointer, already 64 bits wide.
    I(u64),
    /// A pointer to host memory, which the guest passes verbatim under identity
    /// mapping.
    P(*const u8),
    F32(f32),
    F64(f64),
}

/// Resolve a symbol in the host's own libSystem.
fn host_symbol(name: &str) -> Option<u64> {
    let c = CString::new(name).ok()?;
    // SAFETY: `c` is a valid NUL-terminated string for the duration of the
    // call, and `RTLD_DEFAULT` searches images already loaded.
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c.as_ptr()) };
    (!p.is_null()).then_some(p as usize as u64)
}

fn guest_stack() -> BufferSpace {
    let mut space = BufferSpace::new(STACK_BASE);
    space
        .reserve(STACK_SIZE, 16, Placement::Fixed(STACK_BASE))
        .expect("reserve guest stack");
    space
}

fn context_for(args: &[A], mem: &mut BufferSpace) -> RegisterContext {
    let mut ctx = RegisterContext::new();
    ctx.rsp = STACK_BASE;

    let mut int = 0usize;
    let mut fp = 0usize;
    let mut stack = 0u64;

    for arg in args {
        let (is_fp, bits) = match *arg {
            A::I(v) => (false, v),
            A::P(p) => (false, p as usize as u64),
            A::F32(v) => (true, u64::from(v.to_bits())),
            A::F64(v) => (true, v.to_bits()),
        };
        if is_fp {
            if fp < 8 {
                ctx.xmm[fp][0] = bits;
                fp += 1;
                continue;
            }
        } else if int < 6 {
            *[
                &mut ctx.rdi,
                &mut ctx.rsi,
                &mut ctx.rdx,
                &mut ctx.rcx,
                &mut ctx.r8,
                &mut ctx.r9,
            ][int] = bits;
            int += 1;
            continue;
        }
        AddressSpace::write(mem, STACK_BASE + stack, &bits.to_le_bytes())
            .expect("write stack argument");
        stack += 8;
    }
    ctx
}

fn call(symbol: &str, args: &[A]) -> Option<RegisterContext> {
    let Lookup::Known(abi) = table::lookup(0, symbol) else {
        panic!("{symbol} is not in the table, so this case is testing nothing")
    };
    let host = host_symbol(symbol.strip_prefix('_').unwrap_or(symbol))?;
    let mut mem = guest_stack();
    let mut ctx = context_for(args, &mut mem);
    marshal::call(symbol, abi, host, &mut ctx, &mem as &dyn GuestMemory)
        .unwrap_or_else(|e| panic!("{symbol}: {e}"));
    Some(ctx)
}

/// Assert an integer return, skipping if the host lacks the symbol.
macro_rules! check_int {
    ($symbol:literal, [$($arg:expr),* $(,)?], $want:expr) => {{
        if let Some(ctx) = call($symbol, &[$($arg),*]) {
            assert_eq!(ctx.rax, $want as u64, "{} returned the wrong value", $symbol);
        }
    }};
}

/// Assert a `double` return within `eps`.
macro_rules! check_f64 {
    ($symbol:literal, [$($arg:expr),* $(,)?], $want:expr) => {{
        if let Some(ctx) = call($symbol, &[$($arg),*]) {
            let got = f64::from_bits(ctx.xmm[0][0]);
            assert!(
                (got - $want).abs() < 1e-9,
                "{} returned {got}, wanted {}", $symbol, $want
            );
        }
    }};
}

/// Assert a `float` return, which lives in the low half of `xmm0`.
macro_rules! check_f32 {
    ($symbol:literal, [$($arg:expr),* $(,)?], $want:expr) => {{
        if let Some(ctx) = call($symbol, &[$($arg),*]) {
            let got = f32::from_bits(ctx.xmm[0][0] as u32);
            assert!(
                (got - $want).abs() < 1e-5,
                "{} returned {got}, wanted {}", $symbol, $want
            );
        }
    }};
}

fn cstr(s: &str) -> CString {
    CString::new(s).expect("no interior NUL")
}

#[test]
fn string_functions() {
    let hello = cstr("hello");
    let world = cstr("world");
    let hello2 = cstr("hello, world");
    let empty = cstr("");

    check_int!("_strlen", [A::P(hello.as_ptr().cast())], 5u64);
    check_int!("_strlen", [A::P(empty.as_ptr().cast())], 0u64);
    check_int!("_strnlen", [A::P(hello.as_ptr().cast()), A::I(3)], 3u64);
    // A negative `int` return, which only survives if `rax` is sign-extended -
    // the magnitude is libc's business, the sign is ours.
    if let Some(ctx) = call(
        "_strcmp",
        &[A::P(hello.as_ptr().cast()), A::P(world.as_ptr().cast())],
    ) {
        assert!(
            (ctx.rax as i64) < 0,
            "strcmp(hello, world) returned {}",
            ctx.rax as i64
        );
    }
    check_int!(
        "_strcmp",
        [A::P(hello.as_ptr().cast()), A::P(hello.as_ptr().cast())],
        0u64
    );
    // The length argument is the third register; getting it wrong compares the
    // whole string and the answer flips.
    check_int!(
        "_strncmp",
        [
            A::P(hello.as_ptr().cast()),
            A::P(hello2.as_ptr().cast()),
            A::I(5)
        ],
        0u64
    );
    check_int!(
        "_strcasecmp",
        [
            A::P(cstr("HELLO").as_ptr().cast()),
            A::P(hello.as_ptr().cast())
        ],
        0u64
    );
    check_int!(
        "_strncasecmp",
        [
            A::P(cstr("HELLO!").as_ptr().cast()),
            A::P(hello.as_ptr().cast()),
            A::I(5)
        ],
        0u64
    );
    check_int!(
        "_memcmp",
        [
            A::P(hello.as_ptr().cast()),
            A::P(hello.as_ptr().cast()),
            A::I(5)
        ],
        0u64
    );
    // `bcmp` promises only "nonzero when they differ" - Apple's is `memcmp`.
    if let Some(ctx) = call(
        "_bcmp",
        &[
            A::P(hello.as_ptr().cast()),
            A::P(world.as_ptr().cast()),
            A::I(5),
        ],
    ) {
        assert_ne!(ctx.rax, 0);
    }
    check_int!(
        "_strspn",
        [
            A::P(hello.as_ptr().cast()),
            A::P(cstr("hel").as_ptr().cast())
        ],
        4u64
    );
    check_int!(
        "_strcspn",
        [A::P(hello.as_ptr().cast()), A::P(cstr("l").as_ptr().cast())],
        2u64
    );
    check_int!(
        "_strcoll",
        [A::P(hello.as_ptr().cast()), A::P(hello.as_ptr().cast())],
        0u64
    );
}

#[test]
fn functions_returning_pointers_into_their_arguments() {
    let hello = cstr("hello, world");
    let base = hello.as_ptr() as usize as u64;

    // A pointer return is only right if the whole 64 bits came back - a
    // truncated one would still be non-null and still look plausible.
    if let Some(ctx) = call(
        "_strchr",
        &[A::P(hello.as_ptr().cast()), A::I(u64::from(b','))],
    ) {
        assert_eq!(ctx.rax, base + 5);
    }
    if let Some(ctx) = call(
        "_strrchr",
        &[A::P(hello.as_ptr().cast()), A::I(u64::from(b'l'))],
    ) {
        assert_eq!(ctx.rax, base + 10);
    }
    if let Some(ctx) = call(
        "_strstr",
        &[
            A::P(hello.as_ptr().cast()),
            A::P(cstr("world").as_ptr().cast()),
        ],
    ) {
        assert_eq!(ctx.rax, base + 7);
    }
    if let Some(ctx) = call(
        "_memchr",
        &[
            A::P(hello.as_ptr().cast()),
            A::I(u64::from(b'w')),
            A::I(hello.as_bytes().len() as u64),
        ],
    ) {
        assert_eq!(ctx.rax, base + 7);
    }
    // A miss returns null, and null is the value most likely to appear by
    // accident, so it is worth asserting separately.
    if let Some(ctx) = call(
        "_strchr",
        &[A::P(hello.as_ptr().cast()), A::I(u64::from(b'z'))],
    ) {
        assert_eq!(ctx.rax, 0);
    }
}

#[test]
fn functions_with_side_effects_on_a_buffer() {
    let mut buf = [0xaau8; 16];
    let ptr = buf.as_mut_ptr();

    if let Some(ctx) = call("_memset", &[A::P(ptr), A::I(u64::from(b'x')), A::I(4)]) {
        assert_eq!(
            ctx.rax, ptr as usize as u64,
            "memset returns its destination"
        );
        assert_eq!(&buf[..5], b"xxxx\xaa");
    }

    let src = cstr("abc");
    if let Some(_ctx) = call("_memcpy", &[A::P(ptr), A::P(src.as_ptr().cast()), A::I(3)]) {
        assert_eq!(&buf[..3], b"abc");
    }
    if let Some(_ctx) = call("_strcpy", &[A::P(ptr), A::P(cstr("hello").as_ptr().cast())]) {
        assert_eq!(&buf[..6], b"hello\0");
    }
    if let Some(_ctx) = call("_bzero", &[A::P(ptr), A::I(8)]) {
        assert_eq!(&buf[..8], &[0u8; 8]);
    }
    // A void return must leave `rax` alone rather than writing whatever the
    // host happened to leave in `x0`.
    let mut mem = guest_stack();
    let mut ctx = context_for(&[A::P(ptr), A::I(8)], &mut mem);
    ctx.rax = 0xdead_beef;
    if let Some(host) = host_symbol("bzero") {
        let Lookup::Known(abi) = table::lookup(0, "_bzero") else {
            unreachable!()
        };
        marshal::call("_bzero", abi, host, &mut ctx, &mem as &dyn GuestMemory).unwrap();
        assert_eq!(ctx.rax, 0xdead_beef, "a void return must not clobber rax");
    }
}

#[test]
fn integer_conversions_and_narrow_returns() {
    check_int!(
        "_atoi",
        [A::P(cstr("-42").as_ptr().cast())],
        (-42i64) as u64
    );
    check_int!(
        "_atol",
        [A::P(cstr("9999999999").as_ptr().cast())],
        9_999_999_999u64
    );
    check_int!(
        "_atoll",
        [A::P(cstr("-9999999999").as_ptr().cast())],
        (-9_999_999_999i64) as u64
    );
    check_int!(
        "_strtol",
        [A::P(cstr("ff").as_ptr().cast()), A::I(0), A::I(16)],
        255u64
    );
    check_int!(
        "_strtoul",
        [A::P(cstr("-1").as_ptr().cast()), A::I(0), A::I(10)],
        u64::MAX
    );
    check_int!("_abs", [A::I((-7i32) as u32 as u64)], 7u64);
    // `abs(INT_MIN)` is undefined in C but must not be *misplaced*: the point
    // is that a 32-bit argument with dirty upper bits still arrives as an int.
    check_int!("_abs", [A::I(0xdead_beef_0000_0005)], 5u64);
    check_int!(
        "_labs",
        [A::I((-9_999_999_999i64) as u64)],
        9_999_999_999u64
    );
}

#[test]
fn character_classification_takes_and_returns_int() {
    check_int!("_toupper", [A::I(u64::from(b'a'))], u64::from(b'A'));
    check_int!("_tolower", [A::I(u64::from(b'Z'))], u64::from(b'z'));
    // A dirty upper half in the argument register: `int` is 32 bits, and
    // forwarding all 64 would leave the host classifying nonsense.
    check_int!("_toupper", [A::I(0xffff_ffff_0000_0061)], u64::from(b'A'));
    for (f, arg, want) in [
        ("_isalpha", b'a', true),
        ("_isalpha", b'1', false),
        ("_isdigit", b'7', true),
        ("_isdigit", b'x', false),
        ("_isspace", b' ', true),
        ("_isupper", b'A', true),
        ("_islower", b'A', false),
        ("_isalnum", b'9', true),
        ("_isxdigit", b'f', true),
        ("_isprint", b'~', true),
        ("_ispunct", b',', true),
    ] {
        if let Some(ctx) = call(f, &[A::I(u64::from(arg))]) {
            assert_eq!(ctx.rax != 0, want, "{f}('{}')", arg as char);
        }
    }
}

#[test]
fn double_arguments_and_returns() {
    check_f64!("_sqrt", [A::F64(2.0)], std::f64::consts::SQRT_2);
    check_f64!("_pow", [A::F64(2.0), A::F64(10.0)], 1024.0);
    check_f64!("_fmod", [A::F64(10.5), A::F64(3.0)], 1.5);
    check_f64!(
        "_atan2",
        [A::F64(1.0), A::F64(1.0)],
        std::f64::consts::FRAC_PI_4
    );
    check_f64!("_hypot", [A::F64(3.0), A::F64(4.0)], 5.0);
    check_f64!("_floor", [A::F64(-1.5)], -2.0);
    check_f64!("_ceil", [A::F64(-1.5)], -1.0);
    check_f64!("_round", [A::F64(-1.5)], -2.0);
    check_f64!("_trunc", [A::F64(-1.9)], -1.0);
    check_f64!("_fabs", [A::F64(-3.25)], 3.25);
    check_f64!("_log", [A::F64(std::f64::consts::E)], 1.0);
    check_f64!("_log2", [A::F64(1024.0)], 10.0);
    check_f64!("_log10", [A::F64(1000.0)], 3.0);
    check_f64!("_exp", [A::F64(0.0)], 1.0);
    check_f64!("_exp2", [A::F64(10.0)], 1024.0);
    check_f64!("_sin", [A::F64(0.0)], 0.0);
    check_f64!("_cos", [A::F64(0.0)], 1.0);
    check_f64!("_tan", [A::F64(0.0)], 0.0);
    check_f64!("_asin", [A::F64(1.0)], std::f64::consts::FRAC_PI_2);
    check_f64!("_acos", [A::F64(1.0)], 0.0);
    check_f64!("_atan", [A::F64(1.0)], std::f64::consts::FRAC_PI_4);
    check_f64!("_fmin", [A::F64(1.0), A::F64(2.0)], 1.0);
    check_f64!("_fmax", [A::F64(1.0), A::F64(2.0)], 2.0);
    check_f64!("_atof", [A::P(cstr("2.5").as_ptr().cast())], 2.5);
    check_f64!(
        "_strtod",
        [A::P(cstr("1.25e2").as_ptr().cast()), A::I(0)],
        125.0
    );
    // Mixed classes: the `int` goes to `x1` and the `double` to `d0`, from two
    // independent counters. A unified count would put the exponent in `x0`.
    check_f64!("_ldexp", [A::F64(1.5), A::I(4)], 24.0);
}

#[test]
fn float_arguments_and_returns_use_the_low_half_of_the_register() {
    // The single case where a cast rather than a bit move would silently ruin
    // every value: `s0` aliases the low 32 bits of `v0`.
    check_f32!("_sqrtf", [A::F32(2.0)], std::f32::consts::SQRT_2);
    check_f32!("_powf", [A::F32(3.0), A::F32(3.0)], 27.0);
    check_f32!("_fabsf", [A::F32(-2.5)], 2.5);
    check_f32!("_floorf", [A::F32(-1.5)], -2.0);
}

#[test]
fn allocation_round_trips() {
    let Some(malloc) = call("_malloc", &[A::I(64)]) else {
        return;
    };
    let p = malloc.rax;
    assert_ne!(p, 0, "malloc returned null for 64 bytes");

    if let Some(ctx) = call("_malloc_size", &[A::I(p)]) {
        assert!(
            ctx.rax >= 64,
            "malloc_size reported {} for a 64-byte block",
            ctx.rax
        );
    }
    if let Some(ctx) = call("_realloc", &[A::I(p), A::I(128)]) {
        assert_ne!(ctx.rax, 0);
        call("_free", &[A::I(ctx.rax)]);
    } else {
        call("_free", &[A::I(p)]);
    }

    if let Some(ctx) = call("_calloc", &[A::I(8), A::I(8)]) {
        assert_ne!(ctx.rax, 0);
        // SAFETY: `calloc` returned a live 64-byte allocation.
        let zeroed = unsafe { std::slice::from_raw_parts(ctx.rax as usize as *const u8, 64) };
        assert!(zeroed.iter().all(|&b| b == 0));
        call("_free", &[A::I(ctx.rax)]);
    }

    // `strdup` allocates *and* copies, so it checks both directions at once.
    if let Some(ctx) = call("_strdup", &[A::P(cstr("copy me").as_ptr().cast())]) {
        assert_ne!(ctx.rax, 0);
        // SAFETY: `strdup` returned a NUL-terminated copy we own.
        let s = unsafe { std::ffi::CStr::from_ptr(ctx.rax as usize as *const libc::c_char) };
        assert_eq!(s.to_bytes(), b"copy me");
        call("_free", &[A::I(ctx.rax)]);
    }
}

#[test]
fn process_and_environment_queries() {
    if let Some(ctx) = call("_getpid", &[]) {
        assert_eq!(ctx.rax as i32, std::process::id() as i32);
    }
    if let Some(ctx) = call("_getuid", &[]) {
        // SAFETY: `getuid` has no preconditions.
        assert_eq!(ctx.rax as u32, unsafe { libc::getuid() });
    }
    // `__error` is how `errno` is reached, and the whole passthrough story
    // depends on it being the *host's* thread-local - the same pointer the
    // host's own libc hands out.
    if let Some(ctx) = call("___error", &[]) {
        assert_eq!(ctx.rax as usize as *mut i32, darwin::errno::location());
    }
    // Set a variable through the host and read it back through the marshaller,
    // so the two ends of the same table entry are checked against each other.
    let key = cstr("ROSETTA3_ABI_TEST");
    let value = cstr("present");
    // SAFETY: both strings outlive the call; `setenv` copies them.
    unsafe { libc::setenv(key.as_ptr(), value.as_ptr(), 1) };
    if let Some(ctx) = call("_getenv", &[A::P(key.as_ptr().cast())]) {
        assert_ne!(ctx.rax, 0, "getenv found nothing");
        // SAFETY: `getenv` returned a pointer into the environment block.
        let got = unsafe { std::ffi::CStr::from_ptr(ctx.rax as usize as *const libc::c_char) };
        assert_eq!(got.to_bytes(), b"present");
    }
    if let Some(ctx) = call("_time", &[A::I(0)]) {
        assert!(
            ctx.rax as i64 > 1_700_000_000,
            "time() returned {}",
            ctx.rax as i64
        );
    }
}

unsafe extern "C" fn eight_ints(
    a: i64,
    b: i64,
    c: i64,
    d: i64,
    e: i64,
    f: i64,
    g: i64,
    h: i64,
) -> i64 {
    a + b * 2 + c * 3 + d * 4 + e * 5 + f * 6 + g * 7 + h * 8
}

static EIGHT_INTS: FunctionAbi = FunctionAbi {
    name: "eight_ints",
    callbacks: &[],
    reenters_guest: false,
    types: &[
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
    ],
    variadic: false,
};

unsafe extern "C" fn twelve_ints_two_doubles(
    a: i64,
    b: i64,
    c: i64,
    d: i64,
    e: i64,
    f: i64,
    g: i64,
    h: i64,
    i: i64,
    j: i64,
    k: i64,
    l: i64,
    x: f64,
    y: f64,
) -> i64 {
    let sum: i64 = [a, b, c, d, e, f, g, h, i, j, k, l]
        .iter()
        .enumerate()
        .map(|(n, v)| v * (n as i64 + 1))
        .sum();
    sum + (x * y) as i64
}

static TWELVE_INTS: FunctionAbi = FunctionAbi {
    name: "twelve_ints_two_doubles",
    callbacks: &[],
    reenters_guest: false,
    types: &[
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::DoublePrecisionFp,
        T::DoublePrecisionFp,
    ],
    variadic: false,
};

#[test]
fn twelve_integer_arguments_reach_the_hosts_own_stack() {
    let mut args: Vec<A> = (1..=12).map(|i| A::I(i as u64)).collect();
    args.push(A::F64(3.0));
    args.push(A::F64(4.0));
    let mut mem = guest_stack();
    let mut ctx = context_for(&args, &mut mem);

    assert!(
        marshal::is_marshallable(&TWELVE_INTS),
        "twelve doubleword arguments must be marshallable"
    );
    marshal::call(
        "twelve_ints_two_doubles",
        &TWELVE_INTS,
        twelve_ints_two_doubles as *const () as usize as u64,
        &mut ctx,
        &mem as &dyn GuestMemory,
    )
    .expect("twelve integer arguments are marshallable");

    let want: i64 = (1..=12).map(|i| i * i).sum::<i64>() + 12;
    assert_eq!(ctx.rax as i64, want);
}

unsafe extern "C" fn eight_longs_int_int_ptr(
    a: i64,
    b: i64,
    c: i64,
    d: i64,
    e: i64,
    f: i64,
    g: i64,
    h: i64,
    i: i32,
    j: i32,
    p: *const i64,
) -> i64 {
    let regs: i64 = [a, b, c, d, e, f, g, h]
        .iter()
        .enumerate()
        .map(|(n, v)| v * (n as i64 + 1))
        .sum();
    // SAFETY: the test passes the address of a live local.
    regs + i64::from(i) * 1_000 + i64::from(j) * 1_000_000 + unsafe { *p }
}

static NARROW_TAIL_4_4_8: FunctionAbi = FunctionAbi {
    name: "eight_longs_int_int_ptr",
    callbacks: &[],
    reenters_guest: false,
    types: &[
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedWord,
        T::SignedWord,
        T::Pointer,
    ],
    variadic: false,
};

/// One narrow argument past the eighth and nothing after it -
/// `glCopyTexSubImage3D`'s shape, and the case where a wrong width is
/// *invisible* unless the value has bits above 32.
unsafe extern "C" fn eight_longs_int(
    a: i64,
    b: i64,
    c: i64,
    d: i64,
    e: i64,
    f: i64,
    g: i64,
    h: i64,
    i: i32,
) -> i64 {
    let regs: i64 = [a, b, c, d, e, f, g, h]
        .iter()
        .enumerate()
        .map(|(n, v)| v * (n as i64 + 1))
        .sum();
    regs + i64::from(i) * 1_000
}

static NARROW_TAIL_4: FunctionAbi = FunctionAbi {
    name: "eight_longs_int",
    callbacks: &[],
    reenters_guest: false,
    types: &[
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedWord,
    ],
    variadic: false,
};

#[test]
fn a_narrow_argument_past_the_eighth_lands_where_apple_packs_it() {
    let behind: i64 = 7_000_000_000;
    let args: Vec<A> = (1..=8)
        .map(|i| A::I(i as u64))
        .chain([
            // A negative ninth, so a marshaller that zero-extended instead of
            // sign-extending is caught as well as one that misplaced it.
            A::I(-3i64 as u64),
            A::I(5),
            A::P(std::ptr::addr_of!(behind).cast()),
        ])
        .collect();
    let mut mem = guest_stack();
    let mut ctx = context_for(&args, &mut mem);

    assert!(
        marshal::is_marshallable(&NARROW_TAIL_4_4_8),
        "a four/four/eight tail has a prototype"
    );
    marshal::call(
        "eight_longs_int_int_ptr",
        &NARROW_TAIL_4_4_8,
        eight_longs_int_int_ptr as *const () as usize as u64,
        &mut ctx,
        &mem as &dyn GuestMemory,
    )
    .expect("a narrow tail is marshallable");

    let want: i64 = (1..=8).map(|i| i * i).sum::<i64>() - 3_000 + 5_000_000 + behind;
    assert_eq!(ctx.rax as i64, want);
}

#[test]
fn a_single_narrow_overflow_is_placed_and_extended() {
    let args: Vec<A> = (1..=8)
        .map(|i| A::I(i as u64))
        // Dirty upper bits: the guest's register holds more than the `int` the
        // callee declares, and only the low half may reach the stack slot.
        .chain([A::I(0xdead_beef_0000_0004)])
        .collect();
    let mut mem = guest_stack();
    let mut ctx = context_for(&args, &mut mem);

    marshal::call(
        "eight_longs_int",
        &NARROW_TAIL_4,
        eight_longs_int as *const () as usize as u64,
        &mut ctx,
        &mem as &dyn GuestMemory,
    )
    .expect("a single narrow overflow is marshallable");

    let want: i64 = (1..=8).map(|i| i * i).sum::<i64>() + 4_000;
    assert_eq!(ctx.rax as i64, want);
}

unsafe extern "C" fn thirteen_with_a_bool_on_the_stack(
    a: i64,
    b: i64,
    c: i64,
    d: i64,
    e: i64,
    f: i64,
    g: i64,
    planar: bool,
    is_planar: bool,
    name: *const i64,
    format: u64,
    row: i64,
    depth: i64,
) -> i64 {
    let regs: i64 = [a, b, c, d, e, f, g]
        .iter()
        .enumerate()
        .map(|(n, v)| v * (n as i64 + 1))
        .sum();
    // SAFETY: the test passes the address of a live local.
    regs + i64::from(planar) * 10
        + i64::from(is_planar) * 100
        + unsafe { *name } * 1_000
        + format as i64 * 10_000
        + row * 100_000
        + depth * 1_000_000
}

static THIRTEEN_INTS: FunctionAbi = FunctionAbi {
    name: "thirteen_with_a_bool_on_the_stack",
    callbacks: &[],
    reenters_guest: false,
    types: &[
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
        T::UnsignedByte,
        T::UnsignedByte,
        T::Pointer,
        T::UnsignedDoubleword,
        T::SignedDoubleword,
        T::SignedDoubleword,
    ],
    variadic: false,
};

#[test]
fn thirteen_integer_arguments_with_a_byte_among_them_are_placed() {
    let behind: i64 = 6;
    let args: Vec<A> = (1..=7)
        .map(|i| A::I(i as u64))
        .chain([
            A::I(1),
            A::I(1),
            A::P(std::ptr::addr_of!(behind).cast()),
            A::I(2),
            A::I(3),
            A::I(4),
        ])
        .collect();
    let mut mem = guest_stack();
    let mut ctx = context_for(&args, &mut mem);

    assert!(
        marshal::is_marshallable(&THIRTEEN_INTS),
        "a byte that realigns to where widening puts it needs no special tail"
    );
    marshal::call(
        "thirteen_with_a_bool_on_the_stack",
        &THIRTEEN_INTS,
        thirteen_with_a_bool_on_the_stack as *const () as usize as u64,
        &mut ctx,
        &mem as &dyn GuestMemory,
    )
    .expect("thirteen integer arguments are marshallable");

    let want: i64 =
        (1..=7).map(|i| i * i).sum::<i64>() + 10 + 100 + 6_000 + 20_000 + 300_000 + 4_000_000;
    assert_eq!(ctx.rax as i64, want);
}

#[cfg(target_arch = "aarch64")]
#[test]
fn an_overflow_too_wide_to_place_is_still_refused_by_name() {
    // Four overflowing arguments beginning with two *adjacent* narrow ones.
    // Adjacency is what makes widening wrong - the second packs in beside the
    // first - and four is past what `NarrowTail` enumerates, so this must
    // refuse rather than misplace the two doublewords behind them.
    static ADJACENT_NARROWS_TOO_LONG: FunctionAbi = FunctionAbi {
        name: "adjacent_narrows_too_long",
        callbacks: &[],
        reenters_guest: false,
        types: &[
            T::SignedDoubleword,
            T::SignedDoubleword,
            T::SignedDoubleword,
            T::SignedDoubleword,
            T::SignedDoubleword,
            T::SignedDoubleword,
            T::SignedDoubleword,
            T::SignedDoubleword,
            T::SignedDoubleword,
            T::SignedWord,
            T::SignedWord,
            T::SignedDoubleword,
            T::SignedDoubleword,
        ],
        variadic: false,
    };
    assert!(!marshal::is_marshallable(&ADJACENT_NARROWS_TOO_LONG));
}

#[test]
fn arguments_spilled_to_the_guest_stack_are_read_back() {
    let args: Vec<A> = (1..=8).map(|i| A::I(i as u64)).collect();
    let mut mem = guest_stack();
    let mut ctx = context_for(&args, &mut mem);

    let host: unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, i64, i64) -> i64 = eight_ints;
    marshal::call(
        "eight_ints",
        &EIGHT_INTS,
        host as usize as u64,
        &mut ctx,
        &mem as &dyn GuestMemory,
    )
    .expect("eight integer arguments are marshallable");

    // 1*1 + 2*2 + ... + 8*8
    let want: i64 = (1..=8).map(|i| i * i).sum();
    assert_eq!(ctx.rax as i64, want);
}

#[test]
fn a_symbol_with_no_signature_fails_by_name_and_changes_nothing() {
    // The design decision, as a test. Refusing has to be *cheap* - a
    // diagnostic naming the symbol - and it has to leave the guest exactly as
    // it was, or a caller that logs the error and continues is corrupted.
    let mut mem = guest_stack();
    let mut ctx = context_for(&[A::I(1), A::I(2)], &mut mem);
    let before = ctx;

    let err = rosetta3::call_import(
        "_a_symbol_nobody_has_described",
        0,
        0xdead_0000,
        &mut ctx,
        &mut mem as &mut dyn GuestMemory,
    )
    .expect_err("an undescribed symbol must not be callable");

    assert_eq!(err.symbol(), "_a_symbol_nobody_has_described");
    assert!(matches!(err, AbiError::NoSignature { .. }));
    assert!(err.to_string().contains("_a_symbol_nobody_has_described"));
    assert_eq!(ctx.rax, before.rax);
    assert_eq!(ctx.rdi, before.rdi);
    assert_eq!(ctx.rsi, before.rsi);
    assert_eq!(ctx.xmm[0], before.xmm[0]);
}

#[test]
fn a_refused_symbol_says_why() {
    let mut mem = guest_stack();
    let mut ctx = RegisterContext::new();
    let err = rosetta3::call_import(
        "_pthread_cancel",
        0,
        0x1000,
        &mut ctx,
        &mut mem as &mut dyn GuestMemory,
    )
    .expect_err("cancellation walks frames the host cannot read");
    assert!(matches!(err, AbiError::Refused { .. }));
    assert!(
        err.to_string().contains("M5"),
        "the reason should point at what is missing"
    );
}

#[test]
fn a_callback_taker_is_refused_without_a_vm_rather_than_passed_through() {
    // `qsort` has a signature and a marked callback argument, so it is callable
    // - but only by something that can build a thunk. Reached through
    // `call_import`, which has no JIT behind it, the guest pointer must not be
    // handed to the host's `qsort` to be called as arm64.
    let mut mem = guest_stack();
    let mut ctx = RegisterContext::new();
    ctx.rcx = 0x1_0000_1234; // the comparator
    let err = rosetta3::call_import(
        "_qsort",
        0,
        0x1000,
        &mut ctx,
        &mut mem as &mut dyn GuestMemory,
    )
    .expect_err("there is nowhere for the guest function pointer to go");
    assert!(matches!(err, AbiError::Refused { .. }), "{err}");
    assert_eq!(ctx.rax, 0, "a refusal leaves the guest context alone");
}

#[test]
fn a_variadic_with_no_shim_is_refused_rather_than_forwarded() {
    // `syslog(LOG_NOTICE, "...", ...)` cannot be forwarded: its variadic tail
    // is in registers on the guest's side and on the stack on the host's.
    //
    // **`syslog`, and not `err` as this used to name.** `err` *is* shimmed -
    // `src/shims/err.rs` prints and then calls `exit(ctx.rdi)`, which is what
    // BSD `err(3)` is specified to do and is therefore correct - so
    // `call_import` never reached the `NoShim` arm the test was written to
    // check. It reached `shims::err`, which called `libc::exit(0)` **in the
    // test process**: the harness died mid-suite, cargo saw exit status 0, and
    // this file reported neither a failure nor a result. `running 20 tests`,
    // six lines of output, no `test result:` line at all. Fourteen tests of
    // this file - a quarter of the directory - had never run, and nothing
    // said so. That is the same class of thing as the skip macro: a run that
    // reports success and executed less than it appears to.
    //
    // Nine of the 31 variadics in `abi/table.rs` have no shim, and this is one
    // of them. The claim is about the *mechanism* - a variadic tail that would
    // be read from the wrong place - so any unshimmed variadic carries it, and
    // none of them can do anything at all, because `NoShim` is decided before
    // the callee is reached.
    let mut mem = guest_stack();
    let mut ctx = RegisterContext::new();
    let err = rosetta3::call_import(
        "_syslog",
        0,
        0x1000,
        &mut ctx,
        &mut mem as &mut dyn GuestMemory,
    )
    .expect_err("an unshimmed variadic must not be forwarded");
    assert!(matches!(err, AbiError::NoShim { .. }), "got {err:?}");
}

#[test]
fn an_unsatisfied_weak_import_is_refused_before_it_is_called() {
    // A weak import nothing satisfied has zero in its slot. Calling it would
    // branch to null; naming it costs a line.
    let mut mem = guest_stack();
    let mut ctx = RegisterContext::new();
    let err = rosetta3::call_import("_strlen", 0, 0, &mut ctx, &mut mem as &mut dyn GuestMemory)
        .expect_err("a null resolution must not be called");
    assert!(matches!(err, AbiError::Unsatisfied { .. }));
}

#[test]
fn every_table_entry_is_marshallable_or_variadic() {
    // A signature that is neither callable nor variadic is a table entry that
    // can only ever fail, which is worth catching when it is written rather
    // than when a program imports it.
    for abi in table::SIGNATURES {
        assert!(
            abi.variadic || marshal::is_marshallable(abi),
            "{} is neither variadic nor marshallable",
            abi.name
        );
    }
}
