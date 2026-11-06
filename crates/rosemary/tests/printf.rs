mod common;

use std::ffi::CString;

use machish::memory::{AddressSpace, BufferSpace, Placement};

use microlift::arch::x86_64::context::RegisterContext;

use rosemary::abi::GuestMemory;
use rosemary::shims::printf::{VarArgs, render};

/// The guest's data and stack, in one region: the format string, any `%s`
/// arguments, and the spilled variadic tail.
const BASE: u64 = 0x3_0000_0000;
const SIZE: usize = 0x2000;
/// The stack sits in the upper half so string writes and stack writes cannot
/// collide.
const STACK: u64 = BASE + 0x1000;

/// One variadic argument, in the class C would promote it to.
#[derive(Clone, Copy, Debug)]
enum V {
    Int(u64),
    Str(&'static str),
    Double(f64),
}

struct Guest {
    mem: BufferSpace,
    next_string: u64,
}

impl Guest {
    fn new() -> Self {
        let mut mem = BufferSpace::new(BASE);
        mem.reserve(SIZE, 16, Placement::Fixed(BASE))
            .expect("reserve guest memory");
        Guest {
            mem,
            next_string: BASE,
        }
    }

    /// Copy a NUL-terminated string into guest memory and return its address.
    fn intern(&mut self, s: &str) -> u64 {
        let at = self.next_string;
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        AddressSpace::write(&mut self.mem, at, &bytes).expect("write guest string");
        self.next_string += bytes.len() as u64 + 8;
        at
    }

    /// Lay out `args` as the variadic tail of a call whose first integer
    /// register is already taken by the format pointer.
    fn context(&mut self, fmt: u64, args: &[V]) -> RegisterContext {
        let mut ctx = RegisterContext::new();
        ctx.rsp = STACK;
        ctx.rdi = fmt;

        let mut int = 1usize;
        let mut fp = 0usize;
        let mut stack = 0u64;

        for arg in args {
            let (is_fp, bits) = match *arg {
                V::Int(v) => (false, v),
                V::Str(s) => (false, self.intern(s)),
                V::Double(v) => (true, v.to_bits()),
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
            AddressSpace::write(&mut self.mem, STACK + stack, &bits.to_le_bytes())
                .expect("write stack argument");
            stack += 8;
        }
        ctx
    }
}

/// What the shim renders.
fn shimmed(fmt: &str, args: &[V]) -> String {
    let mut guest = Guest::new();
    let fmt_va = guest.intern(fmt);
    let ctx = guest.context(fmt_va, args);
    let mem = &guest.mem as &dyn GuestMemory;
    let mut walk = VarArgs::new("_printf", &ctx, mem, 1);
    let bytes = render(fmt.as_bytes(), &mut walk).expect("rendering must not fail");
    String::from_utf8(bytes).expect("output is ASCII in these cases")
}

fn native(fmt: &str, args: &[V]) -> String {
    let cfmt = CString::new(fmt).unwrap();
    let mut buf = vec![0u8; 512];
    // Keep every interned string alive for the duration of the call.
    let strings: Vec<CString> = args
        .iter()
        .filter_map(|a| match a {
            V::Str(s) => Some(CString::new(*s).unwrap()),
            _ => None,
        })
        .collect();
    let mut next_str = strings.iter();

    // SAFETY: `cfmt` and every element of `strings` outlive the call, `buf` has
    // room for the terminator, and the argument list matches `fmt` by
    // construction of each test case.
    let n = unsafe {
        let p = buf.as_mut_ptr().cast();
        let n = buf.len();
        let f = cfmt.as_ptr();
        match args {
            [] => libc::snprintf(p, n, f),
            [a] => match a {
                V::Int(v) => libc::snprintf(p, n, f, *v),
                V::Double(v) => libc::snprintf(p, n, f, *v),
                V::Str(_) => libc::snprintf(p, n, f, next_str.next().unwrap().as_ptr()),
            },
            [a, b] => match (a, b) {
                (V::Int(x), V::Int(y)) => libc::snprintf(p, n, f, *x, *y),
                (V::Int(x), V::Double(y)) => libc::snprintf(p, n, f, *x, *y),
                (V::Double(x), V::Double(y)) => libc::snprintf(p, n, f, *x, *y),
                (V::Str(_), V::Int(y)) => {
                    libc::snprintf(p, n, f, next_str.next().unwrap().as_ptr(), *y)
                }
                (V::Int(x), V::Str(_)) => {
                    libc::snprintf(p, n, f, *x, next_str.next().unwrap().as_ptr())
                }
                (V::Str(_), V::Str(_)) => {
                    let (x, y) = (next_str.next().unwrap(), next_str.next().unwrap());
                    libc::snprintf(p, n, f, x.as_ptr(), y.as_ptr())
                }
                (V::Double(x), V::Int(y)) => libc::snprintf(p, n, f, *x, *y),
                (V::Str(_), V::Double(y)) => {
                    libc::snprintf(p, n, f, next_str.next().unwrap().as_ptr(), *y)
                }
                (V::Double(x), V::Str(_)) => {
                    libc::snprintf(p, n, f, *x, next_str.next().unwrap().as_ptr())
                }
            },
            [V::Int(x), V::Str(_), V::Double(z)] => {
                libc::snprintf(p, n, f, *x, next_str.next().unwrap().as_ptr(), *z)
            }
            [V::Str(_), V::Int(y), V::Double(z)] => {
                libc::snprintf(p, n, f, next_str.next().unwrap().as_ptr(), *y, *z)
            }
            [V::Int(x), V::Int(y), V::Int(z)] => libc::snprintf(p, n, f, *x, *y, *z),
            other => panic!("no native call shape for {other:?}"),
        }
    };
    assert!(n >= 0, "the host's snprintf failed for {fmt:?}");
    buf.truncate(n as usize);
    String::from_utf8(buf).expect("output is ASCII in these cases")
}

/// The assertion every case makes.
fn same(fmt: &str, args: &[V]) {
    let ours = shimmed(fmt, args);
    let theirs = native(fmt, args);
    assert_eq!(ours, theirs, "rendering {fmt:?} with {args:?}");
}

#[test]
fn the_milestone_case() {
    // The acceptance condition, spelled out: `%d %s %f` renders correctly.
    same("%d %s %f\n", &[V::Int(42), V::Str("hello"), V::Double(1.5)]);
}

#[test]
fn integers() {
    for v in [0i64, 1, -1, 42, -42, i32::MIN as i64, i32::MAX as i64] {
        same("%d", &[V::Int(v as u64)]);
        same("%i", &[V::Int(v as u64)]);
        same("[%8d]", &[V::Int(v as u64)]);
        same("[%-8d]", &[V::Int(v as u64)]);
        same("[%08d]", &[V::Int(v as u64)]);
        same("[%+d]", &[V::Int(v as u64)]);
        same("[% d]", &[V::Int(v as u64)]);
        same("[%.5d]", &[V::Int(v as u64)]);
        same("[%8.5d]", &[V::Int(v as u64)]);
        // `%.0d` of zero prints nothing at all - the one case where an
        // integer precision changes the digits instead of padding them.
        same("[%.0d]", &[V::Int(v as u64)]);
    }
}

#[test]
fn unsigned_and_bases() {
    for v in [0u64, 1, 255, 4096, u32::MAX as u64] {
        same("%u", &[V::Int(v)]);
        same("%o", &[V::Int(v)]);
        same("%x", &[V::Int(v)]);
        same("%X", &[V::Int(v)]);
        same("%#x", &[V::Int(v)]);
        same("%#o", &[V::Int(v)]);
        same("[%10x]", &[V::Int(v)]);
        same("[%-10x]", &[V::Int(v)]);
        same("[%010x]", &[V::Int(v)]);
        same("[%.6x]", &[V::Int(v)]);
    }
    // Length modifiers change which half of the register is meaningful.
    same("%lx", &[V::Int(0xdead_beef_cafe_babe)]);
    same("%llx", &[V::Int(0xdead_beef_cafe_babe)]);
    same("%zu", &[V::Int(0xdead_beef_cafe_babe)]);
    same("%x", &[V::Int(0xdead_beef_cafe_babe)]);
    same("%hx", &[V::Int(0xdead_beef_cafe_babe)]);
    same("%hhx", &[V::Int(0xdead_beef_cafe_babe)]);
    same("%hhd", &[V::Int(0xffff_ffff_ffff_ff80)]);
}

#[test]
fn strings_and_characters() {
    same("%s", &[V::Str("hello")]);
    same("%s", &[V::Str("")]);
    same("[%10s]", &[V::Str("hi")]);
    same("[%-10s]", &[V::Str("hi")]);
    same("[%.2s]", &[V::Str("truncate me")]);
    same("[%10.3s]", &[V::Str("truncate me")]);
    same(
        "%c%c%c",
        &[
            V::Int(u64::from(b'a')),
            V::Int(u64::from(b'b')),
            V::Int(u64::from(b'c')),
        ],
    );
    same("[%5c]", &[V::Int(u64::from(b'x'))]);
    same("100%%", &[]);
    same("no conversions at all", &[]);
}

#[test]
fn floats() {
    for v in [
        0.0f64,
        1.5,
        -1.5,
        std::f64::consts::PI,
        1e10,
        1e-10,
        -0.125,
        12345.6789,
    ] {
        same("%f", &[V::Double(v)]);
        same("%.0f", &[V::Double(v)]);
        same("%.2f", &[V::Double(v)]);
        same("%.10f", &[V::Double(v)]);
        same("[%12.3f]", &[V::Double(v)]);
        same("[%-12.3f]", &[V::Double(v)]);
        same("[%012.3f]", &[V::Double(v)]);
        same("[%+.3f]", &[V::Double(v)]);
        same("%e", &[V::Double(v)]);
        same("%E", &[V::Double(v)]);
        same("%.2e", &[V::Double(v)]);
        same("%g", &[V::Double(v)]);
        same("%G", &[V::Double(v)]);
        same("%.3g", &[V::Double(v)]);
    }
}

#[test]
fn exponent_digits_are_exact() {
    for v in [
        0.1f64,
        1e23,
        f64::MAX,
        f64::MIN_POSITIVE,
        5e-324,
        9.995e-5,
        123_456_789.123_456_79,
    ] {
        same("%.16e", &[V::Double(v)]);
        same("%.17g", &[V::Double(v)]);
        same("%.0e", &[V::Double(v)]);
    }
}

#[test]
fn a_nan_prints_without_a_sign() {
    let negative = -f64::NAN;
    assert_eq!(shimmed("%f", &[V::Double(negative)]), "nan");
    assert_eq!(shimmed("%G", &[V::Double(negative)]), "NAN");
    assert_eq!(shimmed("[%+6e]", &[V::Double(f64::NAN)]), "[   nan]");
    assert_eq!(shimmed("[% f]", &[V::Double(negative)]), "[nan]");
}

#[test]
fn non_finite_values_are_not_zero_padded() {
    same("[%06f]", &[V::Double(f64::NEG_INFINITY)]);
    assert_eq!(shimmed("[%06f]", &[V::Double(f64::NAN)]), "[   nan]");
}

#[test]
fn non_finite_values() {
    for v in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        same("%f", &[V::Double(v)]);
        same("%e", &[V::Double(v)]);
        same("%g", &[V::Double(v)]);
        same("%F", &[V::Double(v)]);
    }
}

#[test]
fn star_width_and_precision() {
    same("[%*d]", &[V::Int(8), V::Int(42)]);
    same("[%-*d]", &[V::Int(8), V::Int(42)]);
    same("[%.*f]", &[V::Int(3), V::Double(1.0 / 3.0)]);
    // A negative `*` width means left-justify, which is the one case where an
    // argument changes a *flag* rather than a number.
    same("[%*d]", &[V::Int((-8i32) as u32 as u64), V::Int(42)]);
}

#[test]
fn arguments_beyond_the_registers_come_from_the_guest_stack() {
    // Nine integer conversions: SysV has five registers left after the format
    // pointer, so the sixth argument onward is on the stack. If the walk did
    // not spill, everything from `f` on would be garbage.
    let args: Vec<V> = (1..=9).map(|i| V::Int(i * 11)).collect();
    let ours = shimmed("%d %d %d %d %d %d %d %d %d", &args);
    assert_eq!(ours, "11 22 33 44 55 66 77 88 99");
}

#[test]
fn a_null_string_argument_prints_the_libc_placeholder() {
    // Faulting on a null `%s` would be defensible in a fresh libc and is not
    // what the guest was compiled against.
    assert_eq!(shimmed("[%s]", &[V::Int(0)]), "[(null)]");
}

#[test]
fn a_malformed_format_string_is_total() {
    // A format string is guest data. Every one of these must produce bytes
    // rather than a panic, and must not consume arguments it cannot name.
    for fmt in [
        "%", "%.", "%-", "%*", "%hh", "%l", "%q", "%n", "%!", "%.*", "%5", "abc%",
    ] {
        let out = shimmed(fmt, &[V::Int(1)]);
        assert!(
            !out.is_empty() || fmt.is_empty(),
            "{fmt:?} rendered nothing at all"
        );
    }
}

#[test]
fn percent_n_is_not_implemented() {
    // A write primitive driven by a format string. It comes out literally, so
    // it is visible in the output rather than silently writing memory.
    assert_eq!(shimmed("x%nx", &[V::Int(0)]), "x%nx");
}
