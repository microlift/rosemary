use std::ffi::{VaList, c_char, c_int, c_void};

unsafe extern "C" {
    fn vasprintf(out: *mut *mut c_char, format: *const c_char, ap: VaList) -> c_int;
}

/// `int asl_log(aslclient, aslmsg, int level, const char *format, ...)`, sent to the
/// host's logger as `syslog` would be: ASL's levels are syslog's priorities.
///
/// # Safety
///
/// `format` is NUL-terminated and the arguments match it. The client and message are
/// not read.
unsafe extern "C" fn asl_log(
    _client: *mut c_void,
    _message: *mut c_void,
    level: c_int,
    format: *const c_char,
    args: ...
) -> c_int {
    let mut text: *mut c_char = std::ptr::null_mut();
    // SAFETY: a live `char *` slot, and the caller's format and arguments.
    if unsafe { vasprintf(&raw mut text, format, args) } < 0 || text.is_null() {
        return -1;
    }
    let priority = level.clamp(libc::LOG_EMERG, libc::LOG_DEBUG);
    // SAFETY: a constant format consuming the one NUL-terminated string, which this
    // host's `malloc` allocated.
    unsafe {
        libc::syslog(priority, c"%s".as_ptr(), text);
        libc::free(text.cast());
    }
    0
}

exports::exports! {
    functions {
        "asl_log" => asl_log,
    }
}
