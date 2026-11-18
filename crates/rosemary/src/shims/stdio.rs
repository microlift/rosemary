use std::ffi::{c_char, c_void};

use microlift::arch::x86_64::context::RegisterContext;

use super::printf::{
    VarArgs, c_locale_or_refuse, format_from, format_from_list, render, wide_format,
    wide_from_utf8, write_guest,
};
use super::{bad_pointer, set_int_return};
use crate::abi::{AbiError, GuestMemory, Result};
use crate::shims::bind;

/// Where a conversion's arguments are.
#[derive(Clone, Copy)]
enum Tail {
    /// A variadic call, after this many named integer parameters.
    Registers(usize),
    /// A guest `va_list` at this address.
    List(u64),
}

fn args<'a>(
    symbol: &'a str,
    ctx: &'a RegisterContext,
    mem: &'a dyn GuestMemory,
    tail: Tail,
) -> Result<VarArgs<'a>> {
    match tail {
        Tail::Registers(fixed) => Ok(VarArgs::new(symbol, ctx, mem, fixed)),
        Tail::List(at) => VarArgs::from_list(symbol, ctx, mem, at),
    }
}

/// Under the native libc a guest's stream is the host's, which only the
/// host's own functions can read.
fn builtin_or_refuse(symbol: &str) -> Result<()> {
    match darwin::libc_backend::using_builtin() {
        true => Ok(()),
        false => Err(AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "a host stream, which only the builtin libc's scanner and wide writer reach",
        }),
    }
}

/// An emulated guest's pointer arguments, as the scanner takes them.
struct Guest<'a> {
    first: VarArgs<'a>,
    args: VarArgs<'a>,
    /// The first argument that could not be read; the scanner saw a null.
    failed: Option<AbiError>,
}

impl darwin::stdio::Destinations for Guest<'_> {
    fn next(&mut self) -> *mut c_void {
        match self.args.next_int() {
            Ok(at) => at as usize as *mut c_void,
            Err(error) => {
                self.failed.get_or_insert(error);
                std::ptr::null_mut()
            }
        }
    }

    fn rewind(&mut self) {
        self.args = self.first.clone();
    }

    fn long_double(&self) -> darwin::stdio::LongDouble {
        darwin::stdio::LongDouble::X87
    }
}

/// What a scan reads.
enum Scanned {
    Stream(*mut c_void),
    Text(u64),
}

/// Scan with `format`, storing through the tail's pointers.
fn scan(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    input: Scanned,
    loc: Option<u64>,
    format: u64,
    tail: Tail,
) -> Result<()> {
    match input {
        Scanned::Stream(_) => builtin_or_refuse(symbol)?,
        Scanned::Text(0) => return Err(bad_pointer(symbol, 0)),
        Scanned::Text(_) => {}
    }
    if format == 0 {
        return Err(bad_pointer(symbol, 0));
    }
    let assigned = {
        let args = args(symbol, ctx, &*mem, tail)?;
        let mut out = Guest {
            first: args.clone(),
            args,
            failed: None,
        };
        let loc = loc.map(|loc| loc as usize as *mut c_void);
        let format = format as usize as *const c_char;
        // SAFETY: the guest's input and format, and destinations it passed; a
        // pointer that could not be read is null, which the scanner skips.
        let assigned = unsafe {
            match input {
                Scanned::Stream(fp) => darwin::stdio::scan_with(fp, loc, format, &mut out),
                Scanned::Text(text) => darwin::stdio::scan_str_with(
                    text as usize as *const c_char,
                    loc,
                    format,
                    &mut out,
                ),
            }
        };
        if let Some(error) = out.failed {
            return Err(error);
        }
        assigned
    };
    set_int_return(ctx, assigned);
    Ok(())
}

fn stream(at: u64) -> *mut c_void {
    at as usize as *mut c_void
}

/// `int fscanf(FILE *stream, const char *format, ...)`
pub fn fscanf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (fp, format) = (stream(ctx.rdi), ctx.rsi);
    scan(
        symbol,
        ctx,
        mem,
        Scanned::Stream(fp),
        None,
        format,
        Tail::Registers(2),
    )
}

/// `int scanf(const char *format, ...)`
pub fn scanf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let format = ctx.rdi;
    scan(
        symbol,
        ctx,
        mem,
        Scanned::Stream(darwin::stdio::stdin()),
        None,
        format,
        Tail::Registers(1),
    )
}

/// `int vfscanf(FILE *stream, const char *format, va_list ap)`
pub fn vfscanf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (fp, format, list) = (stream(ctx.rdi), ctx.rsi, ctx.rdx);
    scan(
        symbol,
        ctx,
        mem,
        Scanned::Stream(fp),
        None,
        format,
        Tail::List(list),
    )
}

/// `int vscanf(const char *format, va_list ap)`
pub fn vscanf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (format, list) = (ctx.rdi, ctx.rsi);
    scan(
        symbol,
        ctx,
        mem,
        Scanned::Stream(darwin::stdio::stdin()),
        None,
        format,
        Tail::List(list),
    )
}

/// `int fscanf_l(FILE *stream, locale_t loc, const char *format, ...)`
pub fn fscanf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (fp, loc, format) = (stream(ctx.rdi), ctx.rsi, ctx.rdx);
    scan(
        symbol,
        ctx,
        mem,
        Scanned::Stream(fp),
        Some(loc),
        format,
        Tail::Registers(3),
    )
}

/// `int scanf_l(locale_t loc, const char *format, ...)`
pub fn scanf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (loc, format) = (ctx.rdi, ctx.rsi);
    let fp = darwin::stdio::stdin();
    scan(
        symbol,
        ctx,
        mem,
        Scanned::Stream(fp),
        Some(loc),
        format,
        Tail::Registers(2),
    )
}

/// `int vfscanf_l(FILE *stream, locale_t loc, const char *format, va_list ap)`
pub fn vfscanf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (fp, loc, format, list) = (stream(ctx.rdi), ctx.rsi, ctx.rdx, ctx.rcx);
    scan(
        symbol,
        ctx,
        mem,
        Scanned::Stream(fp),
        Some(loc),
        format,
        Tail::List(list),
    )
}

/// `int vscanf_l(locale_t loc, const char *format, va_list ap)`
pub fn vscanf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (loc, format, list) = (ctx.rdi, ctx.rsi, ctx.rdx);
    let fp = darwin::stdio::stdin();
    scan(
        symbol,
        ctx,
        mem,
        Scanned::Stream(fp),
        Some(loc),
        format,
        Tail::List(list),
    )
}

/// `int sscanf(const char *str, const char *format, ...)`
pub fn sscanf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (text, format) = (ctx.rdi, ctx.rsi);
    scan(
        symbol,
        ctx,
        mem,
        Scanned::Text(text),
        None,
        format,
        Tail::Registers(2),
    )
}

/// `int vsscanf(const char *str, const char *format, va_list ap)`
pub fn vsscanf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (text, format, list) = (ctx.rdi, ctx.rsi, ctx.rdx);
    scan(
        symbol,
        ctx,
        mem,
        Scanned::Text(text),
        None,
        format,
        Tail::List(list),
    )
}

/// `int sscanf_l(const char *str, locale_t loc, const char *format, ...)`
pub fn sscanf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (text, loc, format) = (ctx.rdi, ctx.rsi, ctx.rdx);
    let text = Scanned::Text(text);
    scan(
        symbol,
        ctx,
        mem,
        text,
        Some(loc),
        format,
        Tail::Registers(3),
    )
}

/// `int vsscanf_l(const char *str, locale_t loc, const char *format, va_list ap)`
pub fn vsscanf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (text, loc, format, list) = (ctx.rdi, ctx.rsi, ctx.rdx, ctx.rcx);
    let text = Scanned::Text(text);
    scan(symbol, ctx, mem, text, Some(loc), format, Tail::List(list))
}

/// Write a rendering and return its length, or -1 if the stream refused it.
fn emit(ctx: &mut RegisterContext, fp: *mut c_void, out: &[u8]) {
    let ok = write_guest(fp, out) || out.is_empty();
    set_int_return(ctx, if ok { out.len() as i32 } else { -1 });
}

/// `int fprintf_l(FILE *stream, locale_t loc, const char *format, ...)`
pub fn fprintf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    c_locale_or_refuse(symbol, ctx.rsi)?;
    let out = format_from(symbol, ctx, &*mem, ctx.rdx, 3)?;
    emit(ctx, stream(ctx.rdi), &out);
    Ok(())
}

/// `int printf_l(locale_t loc, const char *format, ...)`
pub fn printf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    c_locale_or_refuse(symbol, ctx.rdi)?;
    let out = format_from(symbol, ctx, &*mem, ctx.rsi, 2)?;
    emit(ctx, darwin::stdio::stdout(), &out);
    Ok(())
}

/// `int vfprintf_l(FILE *stream, locale_t loc, const char *format, va_list ap)`
pub fn vfprintf_l(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    c_locale_or_refuse(symbol, ctx.rsi)?;
    let out = format_from_list(symbol, ctx, mem, ctx.rdx, ctx.rcx)?;
    emit(ctx, stream(ctx.rdi), &out);
    Ok(())
}

/// `int vprintf_l(locale_t loc, const char *format, va_list ap)`
pub fn vprintf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    c_locale_or_refuse(symbol, ctx.rdi)?;
    let out = format_from_list(symbol, ctx, mem, ctx.rsi, ctx.rdx)?;
    emit(ctx, darwin::stdio::stdout(), &out);
    Ok(())
}

/// Render a wide format and write it to `fp` as the stream's multibyte text.
fn wide(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    fp: *mut c_void,
    format: u64,
    tail: Tail,
) -> Result<()> {
    builtin_or_refuse(symbol)?;
    let (out, walked) = {
        let mut args = args(symbol, ctx, &*mem, tail)?;
        let format = wide_format(&args, format)?;
        (render(&format, &mut args)?, args.list())
    };
    if let Some(list) = walked {
        list.write_back(symbol, mem)?;
    }
    let text = wide_from_utf8(&out);
    // SAFETY: the guest's stream, which the builtin libc opened.
    let ok = unsafe { darwin::stdio::write_wide(fp, &text) };
    set_int_return(ctx, if ok { text.len() as i32 } else { -1 });
    Ok(())
}

/// `int fwprintf(FILE *stream, const wchar_t *format, ...)`
pub fn fwprintf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (fp, format) = (stream(ctx.rdi), ctx.rsi);
    wide(symbol, ctx, mem, fp, format, Tail::Registers(2))
}

/// `int wprintf(const wchar_t *format, ...)`
pub fn wprintf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let format = ctx.rdi;
    wide(
        symbol,
        ctx,
        mem,
        darwin::stdio::stdout(),
        format,
        Tail::Registers(1),
    )
}

/// `int vfwprintf(FILE *stream, const wchar_t *format, va_list ap)`
pub fn vfwprintf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (fp, format, list) = (stream(ctx.rdi), ctx.rsi, ctx.rdx);
    wide(symbol, ctx, mem, fp, format, Tail::List(list))
}

/// `int vwprintf(const wchar_t *format, va_list ap)`
pub fn vwprintf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (format, list) = (ctx.rdi, ctx.rsi);
    wide(
        symbol,
        ctx,
        mem,
        darwin::stdio::stdout(),
        format,
        Tail::List(list),
    )
}

/// The shim for `name`, if this family has one.
pub fn shim_for(name: &str) -> Option<super::Shim> {
    Some(match name {
        "fscanf" => bind!("fscanf" => fscanf),
        "scanf" => bind!("scanf" => scanf),
        "vfscanf" => bind!("vfscanf" => vfscanf),
        "vscanf" => bind!("vscanf" => vscanf),
        "fscanf_l" => bind!("fscanf_l" => fscanf_l),
        "scanf_l" => bind!("scanf_l" => scanf_l),
        "vfscanf_l" => bind!("vfscanf_l" => vfscanf_l),
        "vscanf_l" => bind!("vscanf_l" => vscanf_l),
        "sscanf" => bind!("sscanf" => sscanf),
        "vsscanf" => bind!("vsscanf" => vsscanf),
        "sscanf_l" => bind!("sscanf_l" => sscanf_l),
        "vsscanf_l" => bind!("vsscanf_l" => vsscanf_l),
        "fprintf_l" => bind!("fprintf_l" => fprintf_l),
        "printf_l" => bind!("printf_l" => printf_l),
        "vfprintf_l" => bind!("vfprintf_l" => vfprintf_l),
        "vprintf_l" => bind!("vprintf_l" => vprintf_l),
        "fwprintf" => bind!("fwprintf" => fwprintf),
        "wprintf" => bind!("wprintf" => wprintf),
        "vfwprintf" => bind!("vfwprintf" => vfwprintf),
        "vwprintf" => bind!("vwprintf" => vwprintf),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guest memory that is this process's memory, as it is for a guest.
    struct Identity;

    impl GuestMemory for Identity {
        fn read(&self, va: u64, out: &mut [u8]) -> core::result::Result<(), machish::Error> {
            // SAFETY: the tests only pass the addresses of live locals.
            unsafe { core::ptr::copy_nonoverlapping(va as *const u8, out.as_mut_ptr(), out.len()) };
            Ok(())
        }

        fn write(&mut self, va: u64, data: &[u8]) -> core::result::Result<(), machish::Error> {
            // SAFETY: as above.
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), va as *mut u8, data.len()) };
            Ok(())
        }
    }

    fn at<T>(value: &mut T) -> u64 {
        value as *mut T as u64
    }

    #[test]
    fn sscanf_reads_its_destinations_from_the_guest_s_registers() {
        let (mut kind, mut id, mut w, mut h) = ([0u8; 16], 0i64, 0i32, 0i32);
        let mut ctx = RegisterContext::new();
        ctx.rdi = c"type=Asset&id=134289558904675&w=384&h=216&filters=".as_ptr() as u64;
        ctx.rsi = c"type=%[^&]&id=%lld&w=%d&h=%d".as_ptr() as u64;
        (ctx.rdx, ctx.rcx, ctx.r8, ctx.r9) = (at(&mut kind), at(&mut id), at(&mut w), at(&mut h));

        sscanf("sscanf", &mut ctx, &mut Identity).expect("scan");

        assert_eq!(ctx.rax as i32, 4);
        assert_eq!(&kind[..6], b"Asset\0");
        assert_eq!((id, w, h), (134289558904675, 384, 216));
    }

    #[test]
    fn a_position_rewinds_to_the_guest_s_first_destination() {
        let (mut a, mut b) = (0i32, 0i32);
        let mut ctx = RegisterContext::new();
        ctx.rdi = c"7 9".as_ptr() as u64;
        ctx.rsi = c"%2$d %1$d".as_ptr() as u64;
        (ctx.rdx, ctx.rcx) = (at(&mut a), at(&mut b));

        sscanf("sscanf", &mut ctx, &mut Identity).expect("scan");

        assert_eq!((ctx.rax as i32, a, b), (2, 9, 7));
    }

    #[test]
    fn a_long_double_is_stored_in_the_guest_s_x87_format() {
        let mut value = [0u8; 16];
        let mut ctx = RegisterContext::new();
        ctx.rdi = c"2.5".as_ptr() as u64;
        ctx.rsi = c"%Lf".as_ptr() as u64;
        ctx.rdx = at(&mut value);

        sscanf("sscanf", &mut ctx, &mut Identity).expect("scan");

        // 2.5 is 1.25 * 2^1: an explicit integer bit and a biased exponent.
        let x87 = [0, 0, 0, 0, 0, 0, 0, 0xa0, 0x00, 0x40];
        assert_eq!((ctx.rax as i32, &value[..10]), (1, &x87[..]));
    }
}
