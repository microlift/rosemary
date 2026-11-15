//! `syslog(3)`'s formatting calls: rendered as Darwin renders them and handed
//! to the host's logger whole. The priorities and facilities are the same
//! numbers on both.

use std::ffi::{VaList, c_char, c_int};

/// `void vsyslog(int priority, const char *format, va_list ap)`
///
/// # Safety
///
/// `format` is NUL-terminated and `ap` matches it.
unsafe extern "C" fn vsyslog(priority: c_int, format: *const c_char, ap: VaList) {
    let mut text: *mut c_char = std::ptr::null_mut();
    // SAFETY: a live `char *` slot, and the caller's format and arguments.
    if unsafe { crate::sprintf::rosemary_vasprintf(&raw mut text, format, ap) } < 0
        || text.is_null()
    {
        return;
    }
    // SAFETY: a constant format consuming the one NUL-terminated string, which this
    // host's `malloc` allocated.
    unsafe {
        libc::syslog(priority, c"%s".as_ptr(), text);
        libc::free(text.cast());
    }
}

crate::variadic! {
    /// `void syslog(int priority, const char *format, ...)`
    fn syslog(priority: c_int, format: *const c_char) => vsyslog;
}

crate::va_list! {
    darwin_vsyslog = vsyslog, 2;
}

exports::exports! {
    functions {
        "syslog" => syslog,
        "vsyslog" => darwin_vsyslog,
    }
}
