use core::ffi::{c_int, c_void};

use crate::signal::{Code, Context};

pub const SIGEMT: Option<c_int> = None;
pub const SIGINFO: Option<c_int> = None;

pub fn code(_signo: c_int, _code: c_int) -> Code {
    Code::Unknown
}

pub unsafe fn context(_: *const c_void) -> Option<Context> {
    None
}

pub unsafe fn set_context(_: *mut c_void, _: &Context) {}

pub fn private_signal(_index: u8) -> Option<c_int> {
    None
}

pub fn send_to_thread(_tid: i32, _signo: c_int) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

pub fn send_to_thread_of(_pid: i32, _tid: i32, _signo: c_int) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

pub fn queue(pid: i32, signo: c_int, _value: usize) -> std::io::Result<()> {
    // SAFETY: plain values.
    match unsafe { libc::kill(pid, signo) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}

pub fn for_each_thread(_each: &mut dyn FnMut(i32)) -> bool {
    false
}
