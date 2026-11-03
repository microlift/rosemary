use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{AbiError, GuestMemory, Result};

use super::{bad_pointer, set_int_return};

/// SysV AMD64 integer argument registers, in order.
const INT_REGS: usize = 6;
/// Bytes of `va_list` register save area holding the six integer registers.
const GP_SAVE_END: u32 = 48;
/// ...and the end of the eight 16-byte vector slots that follow them.
const FP_SAVE_END: u32 = GP_SAVE_END + 8 * 16;
/// SysV AMD64 vector argument registers, in order.
const FP_REGS: usize = 8;
/// One stack argument slot.
const SLOT: u64 = 8;

/// A walk over the variadic tail of a call.
pub struct VarArgs<'a> {
    symbol: &'a str,
    mem: &'a dyn GuestMemory,
    source: Source<'a>,
    /// How to render `%@`, for a format string that is an `NSString`.
    object: Option<&'a dyn Fn(u64) -> Vec<u8>>,
}

/// Where the arguments are.
enum Source<'a> {
    /// The guest's own registers and stack, as SysV lays them out: how many of
    /// each register file the walk has consumed, and how far down the stack.
    Registers {
        ctx: &'a RegisterContext,
        int: usize,
        fp: usize,
        stack: u64,
    },
    /// A SysV `va_list` - the guest's, or this process's own under the native
    /// backend, where the two have the same layout because they are the same ABI.
    List(VaList),
    /// AArch64's variadic tail, which Darwin puts entirely on the stack: a
    /// pointer, and every argument one eight-byte slot. Only the native backend
    /// produces one.
    Stack(u64),
}

impl<'a> VarArgs<'a> {
    /// Start after `fixed_int` integer arguments have already been consumed by
    /// the named parameters - one for `printf`'s format, two for `fprintf`'s
    /// stream and format, three for `snprintf`.
    pub fn new(
        symbol: &'a str,
        ctx: &'a RegisterContext,
        mem: &'a dyn GuestMemory,
        fixed_int: usize,
    ) -> Self {
        VarArgs {
            symbol,
            mem,
            source: Source::Registers {
                ctx,
                int: fixed_int,
                fp: 0,
                stack: 0,
            },
            object: None,
        }
    }

    /// Read arguments from the SysV `va_list` at `addr` rather than from the
    /// register context.
    pub fn from_list(
        symbol: &'a str,
        _ctx: &'a RegisterContext,
        mem: &'a dyn GuestMemory,
        addr: u64,
    ) -> Result<Self> {
        VarArgs::from_va_list(symbol, mem, addr)
    }

    /// [`VarArgs::from_list`] without a register context, for the native
    /// backend: the `va_list` a host trampoline built is the same structure,
    /// and there are no guest registers behind it.
    pub fn from_va_list(symbol: &'a str, mem: &'a dyn GuestMemory, addr: u64) -> Result<Self> {
        Ok(VarArgs {
            symbol,
            mem,
            source: Source::List(VaList::read(symbol, mem, addr)?),
            object: None,
        })
    }

    /// Read arguments from an AAPCS64 variadic tail: a pointer to the first of
    /// them, which is all Darwin's `va_list` is.
    pub fn from_stack(symbol: &'a str, mem: &'a dyn GuestMemory, ap: u64) -> Self {
        VarArgs {
            symbol,
            mem,
            source: Source::Stack(ap),
            object: None,
        }
    }

    /// The `va_list` as it now stands, for a caller that must write it back.
    pub fn list(&self) -> Option<VaList> {
        match self.source {
            Source::List(list) => Some(list),
            _ => None,
        }
    }

    /// Render `%@` as an object's description, which is what an `NSString`
    /// format adds to the C ones.
    pub fn describing(mut self, object: &'a dyn Fn(u64) -> Vec<u8>) -> Self {
        self.object = Some(object);
        self
    }

    /// As [`VarArgs::new`], for an `NSString` format, where `%@` prints an
    /// object's description.
    pub fn with_objects(
        symbol: &'a str,
        ctx: &'a RegisterContext,
        mem: &'a dyn GuestMemory,
        fixed_int: usize,
        object: &'a dyn Fn(u64) -> Vec<u8>,
    ) -> Self {
        VarArgs::new(symbol, ctx, mem, fixed_int).describing(object)
    }

    /// The next integer or pointer argument.
    pub fn next_int(&mut self) -> Result<u64> {
        let at = match &mut self.source {
            Source::Registers {
                ctx, int, stack, ..
            } => {
                if *int < INT_REGS {
                    let v = [ctx.rdi, ctx.rsi, ctx.rdx, ctx.rcx, ctx.r8, ctx.r9][*int];
                    *int += 1;
                    return Ok(v);
                }
                let at = ctx.rsp.wrapping_add(*stack);
                *stack += SLOT;
                at
            }
            Source::List(list) => {
                let in_regs = list.gp_offset < GP_SAVE_END;
                let at = match in_regs {
                    true => list.reg_save.wrapping_add(u64::from(list.gp_offset)),
                    false => list.overflow,
                };
                match in_regs {
                    true => list.gp_offset += 8,
                    false => list.overflow = list.overflow.wrapping_add(SLOT),
                }
                at
            }
            Source::Stack(ap) => {
                let at = *ap;
                *ap = ap.wrapping_add(SLOT);
                at
            }
        };
        self.mem.read_u64(at, self.symbol)
    }

    /// The next floating-point argument. C promotes a variadic `float` to
    /// `double`, so the slot always holds a `double`.
    pub fn next_fp(&mut self) -> Result<f64> {
        let at = match &mut self.source {
            Source::Registers { ctx, fp, stack, .. } => {
                if *fp < FP_REGS {
                    let v = ctx.xmm[*fp][0];
                    *fp += 1;
                    return Ok(f64::from_bits(v));
                }
                let at = ctx.rsp.wrapping_add(*stack);
                *stack += SLOT;
                at
            }
            Source::List(list) => {
                // A vector slot in the save area is sixteen bytes wide and
                // holds a `double` in its low half; a spilled one is eight.
                let in_regs = list.fp_offset < FP_SAVE_END;
                let at = match in_regs {
                    true => list.reg_save.wrapping_add(u64::from(list.fp_offset)),
                    false => list.overflow,
                };
                match in_regs {
                    true => list.fp_offset += 16,
                    false => list.overflow = list.overflow.wrapping_add(SLOT),
                }
                at
            }
            Source::Stack(ap) => {
                let at = *ap;
                *ap = ap.wrapping_add(SLOT);
                at
            }
        };
        Ok(f64::from_bits(self.mem.read_u64(at, self.symbol)?))
    }
}

/// The guest's `va_list`, as SysV AMD64 defines it.
///
/// Four fields and a register save area, and the reason a `v`-prefixed function cannot
/// be forwarded to the host's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VaList {
    /// Where the structure itself lives, so an updated copy can be written back.
    pub addr: u64,
    /// Bytes of the integer half of the save area already consumed.
    pub gp_offset: u32,
    /// ...and of the vector half, which begins at [`GP_SAVE_END`].
    pub fp_offset: u32,
    /// The next argument that did not fit in a register.
    pub overflow: u64,
    /// The 176-byte block the callee's prologue spilled its registers into.
    pub reg_save: u64,
}

impl VaList {
    fn read(symbol: &str, mem: &dyn GuestMemory, addr: u64) -> Result<Self> {
        if addr == 0 {
            return Err(bad_pointer(symbol, 0));
        }
        let head = mem.read_u64(addr, symbol)?;
        Ok(VaList {
            addr,
            gp_offset: head as u32,
            fp_offset: (head >> 32) as u32,
            overflow: mem.read_u64(addr.wrapping_add(8), symbol)?,
            reg_save: mem.read_u64(addr.wrapping_add(16), symbol)?,
        })
    }

    /// Store the walked state back where the guest keeps it.
    pub fn write_back(&self, symbol: &str, mem: &mut dyn GuestMemory) -> Result<()> {
        let head = u64::from(self.gp_offset) | (u64::from(self.fp_offset) << 32);
        mem.write(self.addr, &head.to_le_bytes())
            .and_then(|()| mem.write(self.addr.wrapping_add(8), &self.overflow.to_le_bytes()))
            .map_err(|_| bad_pointer(symbol, self.addr))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Len {
    Char,
    Short,
    Int,
    Long,
    LongLong,
    Size,
}

#[derive(Default)]
struct Spec {
    minus: bool,
    plus: bool,
    space: bool,
    hash: bool,
    zero: bool,
    width: usize,
    prec: Option<usize>,
}

/// Format `fmt` against `args`, exactly as C would print it.
pub fn render(fmt: &[u8], args: &mut VarArgs<'_>) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(fmt.len());
    let mut i = 0usize;

    while i < fmt.len() {
        if fmt[i] != b'%' {
            out.push(fmt[i]);
            i += 1;
            continue;
        }
        let start = i;
        i += 1;

        let mut spec = Spec::default();
        while i < fmt.len() {
            match fmt[i] {
                b'-' => spec.minus = true,
                b'+' => spec.plus = true,
                b' ' => spec.space = true,
                b'#' => spec.hash = true,
                b'0' => spec.zero = true,
                _ => break,
            }
            i += 1;
        }

        if i < fmt.len() && fmt[i] == b'*' {
            i += 1;
            let w = args.next_int()? as u32 as i32;
            if w < 0 {
                spec.minus = true;
                spec.width = w.unsigned_abs() as usize;
            } else {
                spec.width = w as usize;
            }
        } else {
            while i < fmt.len() && fmt[i].is_ascii_digit() {
                spec.width = spec.width.saturating_mul(10) + usize::from(fmt[i] - b'0');
                i += 1;
            }
        }

        if i < fmt.len() && fmt[i] == b'.' {
            i += 1;
            if i < fmt.len() && fmt[i] == b'*' {
                i += 1;
                let p = args.next_int()? as u32 as i32;
                // A negative `*` precision is as if it were omitted.
                spec.prec = (p >= 0).then_some(p as usize);
            } else {
                let mut p = 0usize;
                while i < fmt.len() && fmt[i].is_ascii_digit() {
                    p = p.saturating_mul(10) + usize::from(fmt[i] - b'0');
                    i += 1;
                }
                spec.prec = Some(p);
            }
        }

        let mut len = Len::Int;
        loop {
            match fmt.get(i) {
                Some(b'h') => {
                    len = if len == Len::Short {
                        Len::Char
                    } else {
                        Len::Short
                    };
                    i += 1;
                }
                Some(b'l') => {
                    len = if len == Len::Long {
                        Len::LongLong
                    } else {
                        Len::Long
                    };
                    i += 1;
                }
                // `z`, `j`, `t` are all 64-bit here, and so is BSD's `q`
                // ("quad") - which is not a historical curiosity to skip.
                Some(b'z') | Some(b'j') | Some(b't') | Some(b'q') => {
                    len = Len::Size;
                    i += 1;
                }
                Some(b'L') => i += 1,
                _ => break,
            }
        }

        let Some(&conv) = fmt.get(i) else {
            // A format string ending mid-conversion: emit what was there.
            out.extend_from_slice(&fmt[start..]);
            break;
        };
        i += 1;

        match conv {
            b'%' => out.push(b'%'),
            // Cocoa's object conversion.
            b'@' if args.object.is_some() => {
                let obj = args.next_int()?;
                let text = args.object.expect("checked")(obj);
                pad_bytes(&mut out, &text, &spec);
            }
            b'd' | b'i' => {
                let v = sign_extend(args.next_int()?, len);
                let neg = v < 0;
                let body = digits_with_precision(v.unsigned_abs(), 10, false, spec.prec);
                pad_number(&mut out, &body, neg, &spec, "", spec.prec);
            }
            b'u' | b'o' | b'x' | b'X' => {
                let v = zero_extend(args.next_int()?, len);
                let (base, upper) = match conv {
                    b'o' => (8, false),
                    b'x' => (16, false),
                    b'X' => (16, true),
                    _ => (10, false),
                };
                let prefix = match (spec.hash, conv, v) {
                    (true, _, 0) => "",
                    (true, b'o', _) => "0",
                    (true, b'x', _) => "0x",
                    (true, b'X', _) => "0X",
                    _ => "",
                };
                let body = digits_with_precision(v, base, upper, spec.prec);
                pad_number(&mut out, &body, false, &spec, prefix, spec.prec);
            }
            // `%lc` and `%C` are a `wint_t` - a whole `wchar_t` rather than a
            // byte - and come out as UTF-8 because that is what this renderer's
            // output is.
            b'C' | b'c' if len == Len::Long || conv == b'C' => {
                let v = args.next_int()? as u32;
                let body = char::from_u32(v).map(String::from).unwrap_or_default();
                pad_bytes(&mut out, body.as_bytes(), &spec);
            }
            b'c' => {
                let v = args.next_int()? as u8;
                pad_bytes(&mut out, &[v], &spec);
            }
            // `%ls` and `%S` are a `wchar_t *`.
            b'S' | b's' if len == Len::Long || conv == b'S' => {
                let ptr = args.next_int()?;
                let mut s = read_wide_string(args, ptr)?;
                // A wide string's precision counts *characters*, so it is
                // applied before the UTF-8 encoding rather than after.
                if let Some(p) = spec.prec {
                    s.truncate(p);
                }
                let body: String = s.into_iter().collect();
                pad_bytes(&mut out, body.as_bytes(), &spec);
            }
            b's' => {
                let ptr = args.next_int()?;
                // Read to the NUL first and truncate after.
                let mut s = read_string(args, ptr)?;
                if let Some(p) = spec.prec {
                    s.truncate(p);
                }
                pad_bytes(&mut out, &s, &spec);
            }
            b'p' => {
                let v = args.next_int()?;
                // Apple's libc prints `0x0` for null and lower-case hex
                // otherwise, with no padding of its own.
                let body = format!("0x{}", String::from_utf8_lossy(&digits(v, 16, false)));
                let mut spec = Spec { prec: None, ..spec };
                spec.zero = false;
                pad_bytes(&mut out, body.as_bytes(), &spec);
            }
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' => {
                let v = args.next_fp()?;
                let (body, neg) = format_float(v, conv, &spec);
                pad_number(&mut out, body.as_bytes(), neg, &spec, "", None);
            }
            // Everything unhandled - `%n` included - comes out verbatim and
            // fetches no argument of its own, so the rest of the format still
            // lines up with the rest of the arguments.
            _ => out.extend_from_slice(&fmt[start..i]),
        }
    }
    Ok(out)
}

/// Follow a `%s` pointer into guest memory.
fn read_string(args: &VarArgs<'_>, ptr: u64) -> Result<Vec<u8>> {
    if ptr == 0 {
        // What both glibc and Apple's libc do rather than fault.
        return Ok(b"(null)".to_vec());
    }
    args.mem.read_cstr(ptr, args.symbol)
}

/// Follow a `%ls` pointer into guest memory.
fn read_wide_string(args: &VarArgs<'_>, ptr: u64) -> Result<Vec<char>> {
    if ptr == 0 {
        return Ok("(null)".chars().collect());
    }
    let mut out = Vec::new();
    for i in 0..MAX_WIDE {
        let unit = args
            .mem
            .read_u32(ptr.wrapping_add(i as u64 * 4), args.symbol)?;
        if unit == 0 {
            break;
        }
        out.push(char::from_u32(unit).unwrap_or('\u{fffd}'));
    }
    Ok(out)
}

/// How many `wchar_t` a wide string or format is followed for.
const MAX_WIDE: usize = 64 * 1024;

/// A wide format string, as the bytes [`render`] formats.
pub(crate) fn wide_format(args: &VarArgs<'_>, ptr: u64) -> Result<Vec<u8>> {
    let chars = read_wide_string(args, ptr)?;
    Ok(chars.into_iter().collect::<String>().into_bytes())
}

/// A rendering, back as `wchar_t` units.
pub(crate) fn wide_from_utf8(bytes: &[u8]) -> Vec<u32> {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(u32::from)
        .collect()
}

fn sign_extend(raw: u64, len: Len) -> i64 {
    match len {
        Len::Char => raw as u8 as i8 as i64,
        Len::Short => raw as u16 as i16 as i64,
        Len::Int => raw as u32 as i32 as i64,
        Len::Long | Len::LongLong | Len::Size => raw as i64,
    }
}

fn zero_extend(raw: u64, len: Len) -> u64 {
    match len {
        Len::Char => u64::from(raw as u8),
        Len::Short => u64::from(raw as u16),
        Len::Int => u64::from(raw as u32),
        Len::Long | Len::LongLong | Len::Size => raw,
    }
}

/// As [`digits`], except that zero with an explicit precision of zero prints *nothing*
/// - the one place C's integer precision changes the digits rather than padding them.
fn digits_with_precision(v: u64, base: u64, upper: bool, prec: Option<usize>) -> Vec<u8> {
    if v == 0 && prec == Some(0) {
        return Vec::new();
    }
    digits(v, base, upper)
}

fn digits(mut v: u64, base: u64, upper: bool) -> Vec<u8> {
    if v == 0 {
        return vec![b'0'];
    }
    let alphabet: &[u8] = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut out = Vec::new();
    while v > 0 {
        out.push(alphabet[(v % base) as usize]);
        v /= base;
    }
    out.reverse();
    out
}

/// Render a float's magnitude, C-style, returning `(body, negative)`.
fn format_float(v: f64, conv: u8, spec: &Spec) -> (String, bool) {
    let upper = conv.is_ascii_uppercase();
    let neg = v.is_sign_negative();
    let mag = v.abs();

    if mag.is_nan() {
        return (if upper { "NAN".into() } else { "nan".into() }, neg);
    }
    if mag.is_infinite() {
        return (if upper { "INF".into() } else { "inf".into() }, neg);
    }

    let prec = spec.prec.unwrap_or(6);
    let body = match conv {
        b'f' | b'F' => format!("{mag:.prec$}"),
        b'e' | b'E' => exponential(mag, prec, upper),
        _ => {
            // %g: C chooses %e when the exponent is < -4 or >= the precision,
            // and strips trailing zeros unless `#` was given.
            let p = prec.max(1);
            let exp = decimal_exponent(mag, p);
            let mut s = if exp < -4 || exp >= p as i32 {
                exponential(mag, p - 1, upper)
            } else {
                let decimals = (p as i32 - 1 - exp).max(0) as usize;
                format!("{mag:.decimals$}")
            };
            if !spec.hash {
                s = strip_trailing_zeros(&s);
            }
            s
        }
    };
    (body, neg)
}

/// `%e`: C requires at least two exponent digits, Rust's `{:e}` emits the
/// fewest, so the exponent is rebuilt rather than patched.
fn exponential(mag: f64, prec: usize, upper: bool) -> String {
    let exp = decimal_exponent(mag, prec + 1);
    let scaled = if mag == 0.0 { 0.0 } else { mag / powi10(exp) };
    // Rounding the mantissa can carry it to 10.0 (9.99 at precision 1); when it
    // does, the exponent is one larger and the mantissa is 1.0.
    let (mantissa, exp) = {
        let rounded = format!("{scaled:.prec$}");
        if rounded.starts_with("10") {
            (format!("{:.prec$}", scaled / 10.0), exp + 1)
        } else {
            (rounded, exp)
        }
    };
    let e = if upper { 'E' } else { 'e' };
    let sign = if exp < 0 { '-' } else { '+' };
    format!("{mantissa}{e}{sign}{:02}", exp.abs())
}

/// The base-10 exponent of `mag` rounded to `sig` significant digits.
fn decimal_exponent(mag: f64, sig: usize) -> i32 {
    if mag == 0.0 || !mag.is_finite() {
        return 0;
    }
    let s = format!("{:.*e}", sig.saturating_sub(1), mag);
    s.split('e')
        .nth(1)
        .and_then(|e| e.parse::<i32>().ok())
        .unwrap_or(0)
}

fn powi10(exp: i32) -> f64 {
    10f64.powi(exp)
}

fn strip_trailing_zeros(s: &str) -> String {
    let (mantissa, exp) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    if !mantissa.contains('.') {
        return s.to_string();
    }
    let trimmed = mantissa.trim_end_matches('0').trim_end_matches('.');
    format!("{trimmed}{exp}")
}

/// Place sign, prefix, precision zeros and field padding around a number.
fn pad_number(
    out: &mut Vec<u8>,
    body: &[u8],
    negative: bool,
    spec: &Spec,
    prefix: &str,
    min_digits: Option<usize>,
) {
    let sign: &[u8] = if negative {
        b"-"
    } else if spec.plus {
        b"+"
    } else if spec.space {
        b" "
    } else {
        b""
    };

    let zeros = min_digits.map_or(0, |p| p.saturating_sub(body.len()));
    let zero_pad = spec.zero && min_digits.is_none();
    let fixed = sign.len() + prefix.len() + zeros + body.len();
    let pad = spec.width.saturating_sub(fixed);

    if !spec.minus && !zero_pad {
        out.extend(core::iter::repeat_n(b' ', pad));
    }
    out.extend_from_slice(sign);
    out.extend_from_slice(prefix.as_bytes());
    if !spec.minus && zero_pad {
        out.extend(core::iter::repeat_n(b'0', pad));
    }
    out.extend(core::iter::repeat_n(b'0', zeros));
    out.extend_from_slice(body);
    if spec.minus {
        out.extend(core::iter::repeat_n(b' ', pad));
    }
}

/// Field padding for `%c`, `%s` and `%p`, which never take a sign or zeros.
fn pad_bytes(out: &mut Vec<u8>, body: &[u8], spec: &Spec) {
    let pad = spec.width.saturating_sub(body.len());
    if !spec.minus {
        out.extend(core::iter::repeat_n(b' ', pad));
    }
    out.extend_from_slice(body);
    if spec.minus {
        out.extend(core::iter::repeat_n(b' ', pad));
    }
}

/// The guest's `stdout`, which a guest `printf` writes to.
///
/// [`darwin::stdio`]'s answer, which is also what the guest's own `___stdoutp`
/// import holds - so a shim's output and the guest's share one buffer and
/// interleave as they would natively.
fn guest_stdout() -> *mut core::ffi::c_void {
    darwin::stdio::stdout()
}

/// `stderr`, for the same reason.
fn guest_stderr() -> *mut core::ffi::c_void {
    darwin::stdio::stderr()
}

/// Write `bytes` to a guest stream, returning whether all of them went.
pub(crate) fn write_guest(stream: *mut core::ffi::c_void, bytes: &[u8]) -> bool {
    // SAFETY: `stream` is a `FILE *` the guest holds, which is the kind the
    // selected libc made - `darwin::stdio`'s, or the host's under the native
    // libc - and `write` picks the same one.
    unsafe { darwin::stdio::write(stream, bytes) == bytes.len() }
}

/// What a `printf` returns: the length, or -1 if the stream refused it.
fn written(ok: bool, out: &[u8]) -> i32 {
    match ok || out.is_empty() {
        true => out.len() as i32,
        false => -1,
    }
}

/// `int printf(const char *fmt, ...)`
pub fn printf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let out = format_from(symbol, ctx, &*mem, ctx.rdi, 1)?;
    set_int_return(ctx, written(write_guest(guest_stdout(), &out), &out));
    Ok(())
}

/// `void malloc_printf(const char *fmt, ...)`
pub fn malloc_printf(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let out = format_from(symbol, ctx, &*mem, ctx.rdi, 1)?;
    write_guest(guest_stderr(), &out);
    Ok(())
}

/// `int fprintf(FILE *stream, const char *fmt, ...)`
pub fn fprintf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let out = format_from(symbol, ctx, &*mem, ctx.rsi, 2)?;
    set_int_return(
        ctx,
        written(write_guest(ctx.rdi as usize as *mut _, &out), &out),
    );
    Ok(())
}

/// `int sprintf(char *dst, const char *fmt, ...)`
pub fn sprintf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let mut out = format_from(symbol, ctx, &*mem, ctx.rsi, 2)?;
    let written = out.len();
    out.push(0);
    mem.write(ctx.rdi, &out)
        .map_err(|_| bad_pointer(symbol, ctx.rdi))?;
    set_int_return(ctx, written as i32);
    Ok(())
}

/// `int snprintf(char *dst, size_t n, const char *fmt, ...)`
pub fn snprintf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let out = format_from(symbol, ctx, &*mem, ctx.rdx, 3)?;
    store_bounded(symbol, ctx, mem, ctx.rdi, ctx.rsi, out)
}

/// `int snprintf_l(char *dst, size_t n, locale_t loc, const char *fmt, ...)`
///
/// ## The locale is checked rather than ignored
///
/// This shim renders in Rust, which produces **C-locale** output - `.` for the radix
/// point, no digit grouping.
pub fn snprintf_l(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    c_locale_or_refuse(symbol, ctx.rdx)?;
    let out = format_from(symbol, ctx, &*mem, ctx.rcx, 4)?;
    store_bounded(symbol, ctx, mem, ctx.rdi, ctx.rsi, out)
}

/// `int sprintf_l(char *dst, locale_t loc, const char *fmt, ...)`
pub fn sprintf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    c_locale_or_refuse(symbol, ctx.rsi)?;
    let mut out = format_from(symbol, ctx, &*mem, ctx.rdx, 3)?;
    let written = out.len();
    out.push(0);
    mem.write(ctx.rdi, &out)
        .map_err(|_| bad_pointer(symbol, ctx.rdi))?;
    set_int_return(ctx, written as i32);
    Ok(())
}

/// Refuse unless `loc` formats numbers the way this renderer does.
pub(crate) fn c_locale_or_refuse(symbol: &str, loc: u64) -> Result<()> {
    // SAFETY: `loc` is the `locale_t` the guest passed. A handle this process did
    // not issue is the guest's own bug, and the answer to it is a refusal either way.
    match unsafe { darwin::locale::is_c_numeric(loc as usize as *mut _) } {
        Some(true) => Ok(()),
        _ => Err(AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "a locale whose numbers this renderer does not reproduce",
        }),
    }
}

/// `int __sprintf_chk(char *dst, int flag, size_t dstlen, const char *fmt, ...)`
pub fn sprintf_chk(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let out = format_from(symbol, ctx, &*mem, ctx.rcx, 4)?;
    store_bounded(symbol, ctx, mem, ctx.rdi, ctx.rdx, out)
}

/// `int __snprintf_chk(char *dst, size_t n, int flag, size_t dstlen, const char *fmt, ...)`
pub fn snprintf_chk(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let out = format_from(symbol, ctx, &*mem, ctx.r8, 5)?;
    let limit = ctx.rsi.min(ctx.rcx);
    store_bounded(symbol, ctx, mem, ctx.rdi, limit, out)
}

/// Write `out` to `dst`, truncated to `limit` bytes including the terminator, and leave
/// the length the whole rendering would have had in `rax`.
pub(crate) fn format_from_list(
    symbol: &str,
    ctx: &RegisterContext,
    mem: &mut dyn GuestMemory,
    fmt_ptr: u64,
    list_ptr: u64,
) -> Result<Vec<u8>> {
    if fmt_ptr == 0 {
        return Err(bad_pointer(symbol, 0));
    }
    let fmt = (*mem).read_cstr(fmt_ptr, symbol)?;
    let (out, walked) = {
        let mut args = VarArgs::from_list(symbol, ctx, &*mem, list_ptr)?;
        let out = render(&fmt, &mut args)?;
        (out, args.list())
    };
    if let Some(list) = walked {
        list.write_back(symbol, mem)?;
    }
    Ok(out)
}

pub fn vprintf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let out = format_from_list(symbol, ctx, mem, ctx.rdi, ctx.rsi)?;
    set_int_return(ctx, written(write_guest(guest_stdout(), &out), &out));
    Ok(())
}

pub fn vfprintf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let out = format_from_list(symbol, ctx, mem, ctx.rsi, ctx.rdx)?;
    set_int_return(
        ctx,
        written(write_guest(ctx.rdi as usize as *mut _, &out), &out),
    );
    Ok(())
}

pub fn vsprintf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let mut out = format_from_list(symbol, ctx, mem, ctx.rsi, ctx.rdx)?;
    let written = out.len();
    out.push(0);
    let dst = ctx.rdi;
    mem.write(dst, &out).map_err(|_| bad_pointer(symbol, dst))?;
    set_int_return(ctx, written as i32);
    Ok(())
}

/// `int vswprintf(wchar_t *ws, size_t n, const wchar_t *fmt, va_list ap)`
///
/// ## Why the return value is not `snprintf`'s
///
/// `swprintf` and `snprintf` disagree about truncation, and the difference is
/// load-bearing rather than pedantic.
pub fn vswprintf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (fmt, list_ptr) = (ctx.rdx, ctx.rcx);
    let (out, walked) = {
        let mut args = VarArgs::from_list(symbol, ctx, &*mem, list_ptr)?;
        let fmt = wide_format(&args, fmt)?;
        let out = render(&fmt, &mut args)?;
        (out, args.list())
    };
    if let Some(list) = walked {
        list.write_back(symbol, mem)?;
    }

    let wide = wide_from_utf8(&out);
    let (dst, limit) = (ctx.rdi, ctx.rsi);
    // `n` counts wide characters including the terminator, so a rendering of
    // exactly `n` units does not fit.
    if limit == 0 || wide.len() as u64 >= limit {
        set_int_return(ctx, -1);
        return Ok(());
    }
    let mut buf = Vec::with_capacity((wide.len() + 1) * 4);
    for unit in &wide {
        buf.extend_from_slice(&unit.to_le_bytes());
    }
    buf.extend_from_slice(&0u32.to_le_bytes());
    mem.write(dst, &buf).map_err(|_| bad_pointer(symbol, dst))?;
    set_int_return(ctx, wide.len() as i32);
    Ok(())
}

pub fn vsnprintf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let out = format_from_list(symbol, ctx, mem, ctx.rdx, ctx.rcx)?;
    store_bounded(symbol, ctx, mem, ctx.rdi, ctx.rsi, out)
}

/// `__vsnprintf_chk(buf, maxlen, flag, os, fmt, ap)` - the fortified form, whose fourth
/// argument is the destination's compile-time size.
pub fn vsnprintf_chk(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let out = format_from_list(symbol, ctx, mem, ctx.r8, ctx.r9)?;
    let limit = ctx.rsi.min(ctx.rcx);
    store_bounded(symbol, ctx, mem, ctx.rdi, limit, out)
}

/// `vasprintf(char **ret, const char *fmt, va_list ap)`.
pub fn vasprintf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let out = format_from_list(symbol, ctx, mem, ctx.rsi, ctx.rdx)?;
    store_allocated(symbol, ctx, mem, out)
}

/// `void syslog(int priority, const char *fmt, ...)`, rendered here and handed
/// to the host's logger whole. The priorities and facilities are the same
/// numbers on both.
pub fn syslog(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let out = format_from(symbol, ctx, &*mem, ctx.rsi, 2)?;
    let text = std::ffi::CString::new(out).unwrap_or_default();
    // SAFETY: a constant format consuming the one NUL-terminated string.
    unsafe { libc::syslog(ctx.rdi as i32, c"%s".as_ptr(), text.as_ptr()) };
    Ok(())
}

/// `int asl_log(aslclient, aslmsg, int level, const char *format, ...)`, sent to the
/// host's logger as `syslog` would be: ASL's levels are syslog's priorities.
pub fn asl_log(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let out = format_from(symbol, ctx, &*mem, ctx.rcx, 4)?;
    let text = std::ffi::CString::new(out).unwrap_or_default();
    let priority = (ctx.rdx as i32).clamp(libc::LOG_EMERG, libc::LOG_DEBUG);
    // SAFETY: a constant format consuming the one NUL-terminated string.
    unsafe { libc::syslog(priority, c"%s".as_ptr(), text.as_ptr()) };
    set_int_return(ctx, 0);
    Ok(())
}

/// `int asprintf(char **ret, const char *fmt, ...)`
pub fn asprintf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let out = format_from(symbol, ctx, &*mem, ctx.rsi, 2)?;
    store_allocated(symbol, ctx, mem, out)
}

/// Put `out` in a fresh allocation and its address in `*ret` (`rdi`), as the
/// `asprintf` pair answers.
fn store_allocated(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    out: Vec<u8>,
) -> Result<()> {
    // SAFETY: an ordinary allocation of a known size, checked for null before
    // anything is written through it.
    let buf = unsafe { libc::malloc(out.len() + 1) };
    if buf.is_null() {
        set_int_return(ctx, -1);
        return Ok(());
    }
    // SAFETY: `buf` has `out.len() + 1` bytes and the two do not overlap.
    unsafe {
        std::ptr::copy_nonoverlapping(out.as_ptr(), buf.cast::<u8>(), out.len());
        buf.cast::<u8>().add(out.len()).write(0);
    }
    let dst = ctx.rdi;
    mem.write(dst, &(buf as usize as u64).to_le_bytes())
        .map_err(|_| bad_pointer(symbol, dst))?;
    set_int_return(ctx, out.len() as i32);
    Ok(())
}

fn store_bounded(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    dst: u64,
    limit: u64,
    out: Vec<u8>,
) -> Result<()> {
    let would_be = out.len();
    if limit > 0 {
        let keep = out.len().min((limit - 1) as usize);
        let mut buf = out[..keep].to_vec();
        buf.push(0);
        mem.write(dst, &buf).map_err(|_| bad_pointer(symbol, dst))?;
    }
    set_int_return(ctx, would_be as i32);
    Ok(())
}

/// Read the format string at `fmt_ptr` and render it, with `fixed_int` integer
/// registers already spoken for by named parameters.
pub(crate) fn format_from(
    symbol: &str,
    ctx: &RegisterContext,
    mem: &dyn GuestMemory,
    fmt_ptr: u64,
    fixed_int: usize,
) -> Result<Vec<u8>> {
    if fmt_ptr == 0 {
        return Err(bad_pointer(symbol, 0));
    }
    let fmt = mem.read_cstr(fmt_ptr, symbol)?;
    let mut args = VarArgs::new(symbol, ctx, mem, fixed_int);
    render(&fmt, &mut args)
}
