use microlift::arch::x86_64::context::RegisterContext;

use super::printf::{format_from, format_from_list, write_guest};
use crate::abi::{GuestMemory, Result};
use crate::shims::bind;

/// What follows the message: nothing, or the text for an error number.
#[derive(Clone, Copy)]
enum Suffix {
    None,
    /// The guest's `errno` as it stood on entry.
    Errno,
    /// An error number the caller passed (`warnc`, `errc`).
    Code(i32),
}

/// Write one report to the host's `stderr`.
fn report(message: Option<Vec<u8>>, suffix: Suffix) {
    // Captured before anything here can disturb it.
    let errno = match suffix {
        Suffix::None => None,
        Suffix::Errno => Some(darwin::errno::guest_errno()),
        Suffix::Code(code) => Some(code),
    };
    // SAFETY: the name `getprogname` returns is NUL-terminated and lives for
    // the process.
    let name_ptr = darwin::progname::getprogname();
    let name = unsafe { std::ffi::CStr::from_ptr(name_ptr) };
    let mut line = name.to_bytes().to_vec();
    line.extend_from_slice(b": ");
    let had_message = message.is_some();
    if let Some(message) = message {
        line.extend(message);
    }
    if let Some(errno) = errno {
        if had_message {
            line.extend_from_slice(b": ");
        }
        // SAFETY: the Darwin-numbered `strerror`, answered with a
        // NUL-terminated string.
        let text_ptr = unsafe { darwin::string::strerror(errno) };
        let text = unsafe { std::ffi::CStr::from_ptr(text_ptr) };
        line.extend_from_slice(text.to_bytes());
    }
    line.push(b'\n');
    write_guest(darwin::stdio::stderr(), &line);
}

/// The message a variadic form's format renders, or `None` for a null format.
fn message(
    symbol: &str,
    ctx: &RegisterContext,
    mem: &dyn GuestMemory,
    fmt: u64,
    fixed_int: usize,
) -> Result<Option<Vec<u8>>> {
    if fmt == 0 {
        return Ok(None);
    }
    format_from(symbol, ctx, mem, fmt, fixed_int).map(Some)
}

/// The same for a `va_list` form.
fn message_list(
    symbol: &str,
    ctx: &RegisterContext,
    mem: &mut dyn GuestMemory,
    fmt: u64,
    list: u64,
) -> Result<Option<Vec<u8>>> {
    if fmt == 0 {
        return Ok(None);
    }
    format_from_list(symbol, ctx, mem, fmt, list).map(Some)
}

/// Leave the process as the guest's own `exit` would: through the host's,
/// which runs the handlers the guest registered with `atexit`.
fn exit(status: u64) -> ! {
    // SAFETY: ends the process.
    unsafe { libc::exit(status as i32) }
}

fn shim(name: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    match name {
        "warn" => report(message(name, ctx, mem, ctx.rdi, 1)?, Suffix::Errno),
        "warnx" => report(message(name, ctx, mem, ctx.rdi, 1)?, Suffix::None),
        "warnc" => report(
            message(name, ctx, mem, ctx.rsi, 2)?,
            Suffix::Code(ctx.rdi as i32),
        ),
        "vwarn" => report(
            message_list(name, ctx, mem, ctx.rdi, ctx.rsi)?,
            Suffix::Errno,
        ),
        "vwarnx" => report(
            message_list(name, ctx, mem, ctx.rdi, ctx.rsi)?,
            Suffix::None,
        ),
        "vwarnc" => report(
            message_list(name, ctx, mem, ctx.rsi, ctx.rdx)?,
            Suffix::Code(ctx.rdi as i32),
        ),
        "err" => {
            report(message(name, ctx, mem, ctx.rsi, 2)?, Suffix::Errno);
            exit(ctx.rdi)
        }
        "errx" => {
            report(message(name, ctx, mem, ctx.rsi, 2)?, Suffix::None);
            exit(ctx.rdi)
        }
        "errc" => {
            report(
                message(name, ctx, mem, ctx.rdx, 3)?,
                Suffix::Code(ctx.rsi as i32),
            );
            exit(ctx.rdi)
        }
        "verr" => {
            report(
                message_list(name, ctx, mem, ctx.rsi, ctx.rdx)?,
                Suffix::Errno,
            );
            exit(ctx.rdi)
        }
        "verrx" => {
            report(
                message_list(name, ctx, mem, ctx.rsi, ctx.rdx)?,
                Suffix::None,
            );
            exit(ctx.rdi)
        }
        "verrc" => {
            report(
                message_list(name, ctx, mem, ctx.rdx, ctx.rcx)?,
                Suffix::Code(ctx.rsi as i32),
            );
            exit(ctx.rdi)
        }
        _ => unreachable!("err::shim_for answered for {name}"),
    }
    Ok(())
}

pub fn shim_for(name: &str) -> Option<super::Shim> {
    Some(match name {
        "warn" => bind!("warn" => shim),
        "warnx" => bind!("warnx" => shim),
        "warnc" => bind!("warnc" => shim),
        "vwarn" => bind!("vwarn" => shim),
        "vwarnx" => bind!("vwarnx" => shim),
        "vwarnc" => bind!("vwarnc" => shim),
        "err" => bind!("err" => shim),
        "errx" => bind!("errx" => shim),
        "errc" => bind!("errc" => shim),
        "verr" => bind!("verr" => shim),
        "verrx" => bind!("verrx" => shim),
        "verrc" => bind!("verrc" => shim),
        _ => return None,
    })
}
