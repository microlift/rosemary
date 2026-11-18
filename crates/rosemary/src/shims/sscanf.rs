use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{GuestMemory, Result};

use super::printf::VarArgs;
use super::{bad_pointer, set_int_return};

#[derive(Clone, Copy)]
enum Length {
    Default,
    Hh,
    H,
    L,
    Ll,
    J,
    Z,
    T,
    BigL,
}

fn digit(c: u8, base: u32) -> bool {
    c.is_ascii_digit() && u32::from(c - b'0') < base
        || c.is_ascii_lowercase() && u32::from(c - b'a' + 10) < base
        || c.is_ascii_uppercase() && u32::from(c - b'A' + 10) < base
}

fn integer_token(input: &[u8], base: u32, auto_base: bool, width: usize) -> Option<(usize, u64)> {
    let mut end = 0;
    if input.first().is_some_and(|c| *c == b'+' || *c == b'-') {
        end = 1;
    }
    let start_digits = end;
    let actual = if auto_base && end + 2 <= width && input.get(end..end + 2) == Some(b"0x") {
        end += 2;
        16
    } else if auto_base && end + 2 <= width && input.get(end..end + 2) == Some(b"0X") {
        end += 2;
        16
    } else if auto_base && input.get(end) == Some(&b'0') {
        8
    } else {
        base
    };
    while end < input.len() && end < width && digit(input[end], actual) {
        end += 1;
    }
    // A sign or a `0x` prefix without a digit is not a conversion.
    let digits = if actual == 16 && end >= start_digits + 2 {
        start_digits + 2
    } else {
        start_digits
    };
    if end == digits {
        return None;
    }
    let text = core::str::from_utf8(&input[..end]).ok()?;
    let negative = text.starts_with('-');
    let magnitude = text
        .trim_start_matches(['+', '-'])
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    let unsigned = u64::from_str_radix(magnitude, actual).ok()?;
    Some((
        end,
        if negative {
            0u64.wrapping_sub(unsigned)
        } else {
            unsigned
        },
    ))
}

fn float_token(input: &[u8], width: usize) -> Option<(usize, f64)> {
    let mut end = 0;
    while end < input.len() && end < width {
        let c = input[end];
        if c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.' | b'e' | b'E') {
            end += 1;
        } else {
            break;
        }
    }
    let text = core::str::from_utf8(&input[..end]).ok()?;
    let value = text.parse::<f64>().ok()?;
    Some((end, value))
}

fn put(writes: &mut Vec<(u64, Vec<u8>)>, ptr: u64, bytes: Vec<u8>) {
    writes.push((ptr, bytes));
}

fn integer_bytes(value: u64, length: Length, signed: bool) -> Vec<u8> {
    match length {
        Length::Hh => vec![value as u8],
        Length::H => (value as u16).to_le_bytes().to_vec(),
        Length::L | Length::Ll | Length::J | Length::Z | Length::T => value.to_le_bytes().to_vec(),
        Length::Default => {
            if signed {
                (value as i32).to_le_bytes().to_vec()
            } else {
                (value as u32).to_le_bytes().to_vec()
            }
        }
        Length::BigL => value.to_le_bytes().to_vec(),
    }
}

fn scanset(format: &[u8], at: &mut usize) -> Option<(bool, [bool; 256])> {
    let mut set = [false; 256];
    let mut inverted = false;
    if format.get(*at) == Some(&b'^') {
        inverted = true;
        *at += 1;
    }
    let first = *at;
    if format.get(*at) == Some(&b']') {
        set[b']' as usize] = true;
        *at += 1;
    }
    while *at < format.len() && format[*at] != b']' {
        let lo = format[*at];
        *at += 1;
        if format.get(*at) == Some(&b'-') && format.get(*at + 1).is_some_and(|c| *c != b']') {
            *at += 1;
            let hi = format[*at];
            *at += 1;
            for c in lo.min(hi)..=lo.max(hi) {
                set[c as usize] = true;
            }
        } else {
            set[lo as usize] = true;
        }
    }
    if *at >= format.len() || *at == first {
        return None;
    }
    *at += 1;
    Some((inverted, set))
}

/// Where the conversions' destination pointers are to be read from.
#[derive(Clone, Copy)]
enum Tail {
    /// A variadic call, after this many named integer parameters.
    Registers(usize),
    /// A guest `va_list` at this address.
    List(u64),
}

/// `int sscanf(const char *input, const char *format, ...)`.
pub fn sscanf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    scan(symbol, ctx, mem, ctx.rdi, ctx.rsi, Tail::Registers(2))
}

/// `int sscanf_l(const char *input, locale_t loc, const char *format, ...)`
pub fn sscanf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    super::printf::c_locale_or_refuse(symbol, ctx.rsi)?;
    scan(symbol, ctx, mem, ctx.rdi, ctx.rdx, Tail::Registers(3))
}

/// `int vsscanf(const char *input, const char *format, va_list ap)`
pub fn vsscanf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    scan(symbol, ctx, mem, ctx.rdi, ctx.rsi, Tail::List(ctx.rdx))
}

fn scan(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    input_va: u64,
    format_va: u64,
    tail: Tail,
) -> Result<()> {
    let input = (*mem).read_cstr(input_va, symbol)?;
    let format = (*mem).read_cstr(format_va, symbol)?;
    let mut args = match tail {
        Tail::Registers(fixed) => VarArgs::new(symbol, ctx, &*mem, fixed),
        Tail::List(at) => VarArgs::from_list(symbol, ctx, &*mem, at)?,
    };
    let mut writes = Vec::new();
    let mut fi = 0;
    let mut ii = 0;
    let mut assigned = 0i32;
    let mut consumed = 0usize;
    let mut input_failure = false;

    while fi < format.len() {
        if format[fi].is_ascii_whitespace() {
            while fi < format.len() && format[fi].is_ascii_whitespace() {
                fi += 1;
            }
            let before = ii;
            while ii < input.len() && input[ii].is_ascii_whitespace() {
                ii += 1;
            }
            consumed += ii - before;
            continue;
        }
        if format[fi] != b'%' {
            if input.get(ii) != Some(&format[fi]) {
                input_failure = ii >= input.len();
                break;
            }
            fi += 1;
            ii += 1;
            consumed += 1;
            continue;
        }
        fi += 1;
        if format.get(fi) == Some(&b'%') {
            if input.get(ii) != Some(&b'%') {
                input_failure = ii >= input.len();
                break;
            }
            fi += 1;
            ii += 1;
            consumed += 1;
            continue;
        }
        let suppress = if format.get(fi) == Some(&b'*') {
            fi += 1;
            true
        } else {
            false
        };
        let mut width = 0usize;
        while fi < format.len() && format[fi].is_ascii_digit() {
            width = width
                .saturating_mul(10)
                .saturating_add((format[fi] - b'0') as usize);
            fi += 1;
        }
        let width = if width == 0 { usize::MAX } else { width };
        let length = if format.get(fi..fi + 2) == Some(b"hh") {
            fi += 2;
            Length::Hh
        } else if format.get(fi..fi + 2) == Some(b"ll") {
            fi += 2;
            Length::Ll
        } else {
            match format.get(fi).copied() {
                Some(b'h') => {
                    fi += 1;
                    Length::H
                }
                Some(b'l') => {
                    fi += 1;
                    Length::L
                }
                Some(b'j') => {
                    fi += 1;
                    Length::J
                }
                Some(b'z') => {
                    fi += 1;
                    Length::Z
                }
                Some(b't') => {
                    fi += 1;
                    Length::T
                }
                Some(b'L') => {
                    fi += 1;
                    Length::BigL
                }
                _ => Length::Default,
            }
        };
        let conv = match format.get(fi).copied() {
            Some(c) => {
                fi += 1;
                c
            }
            None => break,
        };
        let skips = !matches!(conv, b'c' | b'[' | b'n');
        if skips {
            let before = ii;
            while ii < input.len() && input[ii].is_ascii_whitespace() {
                ii += 1;
            }
            consumed += ii - before;
        }
        let before = ii;
        let result: Option<(usize, Vec<u8>)> = match conv {
            b'c' => {
                let n = width.min(input.len().saturating_sub(ii));
                (n > 0).then(|| (n, input[ii..ii + n].to_vec()))
            }
            b's' => {
                let end = (ii..input.len())
                    .take(width)
                    .find(|&n| input[n].is_ascii_whitespace())
                    .unwrap_or((ii + width).min(input.len()));
                (end > ii).then(|| {
                    (end - ii, {
                        let mut v = input[ii..end].to_vec();
                        v.push(0);
                        v
                    })
                })
            }
            b'd' => integer_token(&input[ii..], 10, false, width)
                .map(|(n, v)| (n, integer_bytes(v, length, true))),
            b'i' => integer_token(&input[ii..], 10, true, width)
                .map(|(n, v)| (n, integer_bytes(v, length, true))),
            b'u' => integer_token(&input[ii..], 10, false, width)
                .map(|(n, v)| (n, integer_bytes(v, length, false))),
            b'o' => integer_token(&input[ii..], 8, false, width)
                .map(|(n, v)| (n, integer_bytes(v, length, false))),
            b'x' | b'X' => integer_token(&input[ii..], 16, false, width)
                .map(|(n, v)| (n, integer_bytes(v, length, false))),
            b'p' => integer_token(&input[ii..], 16, true, width)
                .map(|(n, v)| (n, v.to_le_bytes().to_vec())),
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'a' | b'A' => {
                float_token(&input[ii..], width).map(|(n, v)| {
                    (
                        n,
                        if matches!(length, Length::Default) {
                            (v as f32).to_le_bytes().to_vec()
                        } else {
                            v.to_le_bytes().to_vec()
                        },
                    )
                })
            }
            b'[' => match scanset(&format, &mut fi) {
                Some((invert, set)) => {
                    let end = (ii..input.len())
                        .take(width)
                        .find(|&n| set[input[n] as usize] == invert)
                        .unwrap_or((ii + width).min(input.len()));
                    (end > ii).then(|| {
                        (end - ii, {
                            let mut v = input[ii..end].to_vec();
                            v.push(0);
                            v
                        })
                    })
                }
                None => None,
            },
            b'n' => Some((0, integer_bytes(consumed as u64, length, true))),
            _ => None,
        };
        let Some((n, bytes)) = result else {
            input_failure = ii >= input.len();
            break;
        };
        ii += n;
        consumed += n;
        if !suppress && conv != b'%' {
            let ptr = args.next_int()?;
            put(&mut writes, ptr, bytes);
            if conv != b'n' {
                assigned += 1;
            }
        }
        if ii == before && conv != b'n' {
            break;
        }
    }
    drop(args);
    for (ptr, bytes) in writes {
        mem.write(ptr, &bytes)
            .map_err(|_| bad_pointer(symbol, ptr))?;
    }
    set_int_return(
        ctx,
        if assigned == 0 && input_failure {
            -1
        } else {
            assigned
        },
    );
    let _ = consumed;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u64 = 0x1000;

    /// One flat region of guest memory starting at `BASE`.
    struct Flat(Vec<u8>);

    impl Flat {
        fn place(&mut self, bytes: &[u8]) -> u64 {
            let at = BASE + self.0.len() as u64;
            self.0.extend_from_slice(bytes);
            at
        }

        fn slice(&self, va: u64, len: usize) -> &[u8] {
            let at = (va - BASE) as usize;
            &self.0[at..at + len]
        }
    }

    impl GuestMemory for Flat {
        fn read(&self, va: u64, out: &mut [u8]) -> core::result::Result<(), machish::Error> {
            let at = va.wrapping_sub(BASE) as usize;
            let bytes = self
                .0
                .get(at..at + out.len())
                .ok_or(machish::Error::OutOfBounds {
                    addr: va,
                    len: out.len(),
                })?;
            out.copy_from_slice(bytes);
            Ok(())
        }

        fn write(&mut self, va: u64, data: &[u8]) -> core::result::Result<(), machish::Error> {
            let at = va.wrapping_sub(BASE) as usize;
            self.0
                .get_mut(at..at + data.len())
                .ok_or(machish::Error::OutOfBounds {
                    addr: va,
                    len: data.len(),
                })?
                .copy_from_slice(data);
            Ok(())
        }
    }

    #[test]
    fn negated_scanset_stops_at_the_first_excluded_character() {
        let mut mem = Flat(Vec::new());
        let input = mem.place(b"type=Asset&id=134289558904675&w=384&h=216&filters=\0");
        let format = mem.place(b"type=%[^&]&id=%lld&w=%d&h=%d\0");
        let kind = mem.place(&[0; 16]);
        let id = mem.place(&[0; 8]);
        let w = mem.place(&[0; 4]);
        let h = mem.place(&[0; 4]);

        let mut ctx = RegisterContext::new();
        (ctx.rdi, ctx.rsi, ctx.rdx, ctx.rcx, ctx.r8, ctx.r9) = (input, format, kind, id, w, h);
        sscanf("sscanf", &mut ctx, &mut mem).expect("scan");

        assert_eq!(ctx.rax as i32, 4);
        assert_eq!(mem.slice(kind, 6), b"Asset\0");
        assert_eq!(mem.slice(id, 8), 134289558904675i64.to_le_bytes());
        assert_eq!(mem.slice(w, 4), 384i32.to_le_bytes());
        assert_eq!(mem.slice(h, 4), 216i32.to_le_bytes());
    }

    #[test]
    fn scanset_stops_at_the_first_character_outside_it() {
        let mut mem = Flat(Vec::new());
        let input = mem.place(b"abcabX\0");
        let format = mem.place(b"%[abc]\0");
        let out = mem.place(&[0xff; 8]);

        let mut ctx = RegisterContext::new();
        (ctx.rdi, ctx.rsi, ctx.rdx) = (input, format, out);
        sscanf("sscanf", &mut ctx, &mut mem).expect("scan");

        assert_eq!(ctx.rax as i32, 1);
        assert_eq!(mem.slice(out, 6), b"abcab\0");
    }
}
