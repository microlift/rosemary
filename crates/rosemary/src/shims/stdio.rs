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

/// Scan `stream` with `format`, storing through the tail's pointers.
fn scan(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    stream: *mut c_void,
    loc: Option<u64>,
    format: u64,
    tail: Tail,
) -> Result<()> {
    builtin_or_refuse(symbol)?;
    if format == 0 {
        return Err(bad_pointer(symbol, 0));
    }
    let assigned = {
        let mut args = args(symbol, ctx, &*mem, tail)?;
        let mut failed = None;
        let mut next = || match args.next_int() {
            Ok(at) => at as usize as *mut c_void,
            Err(error) => {
                failed.get_or_insert(error);
                std::ptr::null_mut()
            }
        };
        // SAFETY: the guest's stream and format, and destinations it passed;
        // a pointer that could not be read is null, which the scanner skips.
        let assigned = unsafe {
            darwin::stdio::scan_with(
                stream,
                loc.map(|loc| loc as usize as *mut c_void),
                format as usize as *const c_char,
                &mut next,
            )
        };
        if let Some(error) = failed {
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
    scan(symbol, ctx, mem, fp, None, format, Tail::Registers(2))
}

/// `int scanf(const char *format, ...)`
pub fn scanf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let format = ctx.rdi;
    scan(
        symbol,
        ctx,
        mem,
        darwin::stdio::stdin(),
        None,
        format,
        Tail::Registers(1),
    )
}

/// `int vfscanf(FILE *stream, const char *format, va_list ap)`
pub fn vfscanf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (fp, format, list) = (stream(ctx.rdi), ctx.rsi, ctx.rdx);
    scan(symbol, ctx, mem, fp, None, format, Tail::List(list))
}

/// `int vscanf(const char *format, va_list ap)`
pub fn vscanf(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (format, list) = (ctx.rdi, ctx.rsi);
    scan(
        symbol,
        ctx,
        mem,
        darwin::stdio::stdin(),
        None,
        format,
        Tail::List(list),
    )
}

/// `int fscanf_l(FILE *stream, locale_t loc, const char *format, ...)`
pub fn fscanf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (fp, loc, format) = (stream(ctx.rdi), ctx.rsi, ctx.rdx);
    scan(symbol, ctx, mem, fp, Some(loc), format, Tail::Registers(3))
}

/// `int scanf_l(locale_t loc, const char *format, ...)`
pub fn scanf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (loc, format) = (ctx.rdi, ctx.rsi);
    let fp = darwin::stdio::stdin();
    scan(symbol, ctx, mem, fp, Some(loc), format, Tail::Registers(2))
}

/// `int vfscanf_l(FILE *stream, locale_t loc, const char *format, va_list ap)`
pub fn vfscanf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (fp, loc, format, list) = (stream(ctx.rdi), ctx.rsi, ctx.rdx, ctx.rcx);
    scan(symbol, ctx, mem, fp, Some(loc), format, Tail::List(list))
}

/// `int vscanf_l(locale_t loc, const char *format, va_list ap)`
pub fn vscanf_l(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (loc, format, list) = (ctx.rdi, ctx.rsi, ctx.rdx);
    let fp = darwin::stdio::stdin();
    scan(symbol, ctx, mem, fp, Some(loc), format, Tail::List(list))
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
