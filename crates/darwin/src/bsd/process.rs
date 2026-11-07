use std::ffi::{CStr, CString, VaList, c_char};

/// A title that starts with this is shown without the program's name.
const BARE_TITLE_MARKER: u8 = b'-';

/// The title BSD's `setproctitle` would show: `progname: title`, the bare
/// title after a leading `-`, or the program's name alone for a null `fmt`.
///
/// # Safety
///
/// `fmt` is null or a format that `ap` matches.
unsafe fn title(fmt: *const c_char, ap: VaList) -> Option<CString> {
    // SAFETY: `getprogname` answers a NUL-terminated string.
    let progname = unsafe { CStr::from_ptr(crate::progname::getprogname()) };
    if fmt.is_null() {
        return Some(progname.to_owned());
    }
    // SAFETY: the caller states `fmt` is NUL-terminated.
    let bare = unsafe { *fmt.cast::<u8>() } == BARE_TITLE_MARKER;
    // SAFETY: the marker is a byte of the string, so the rest is too.
    let fmt = if bare { unsafe { fmt.add(1) } } else { fmt };

    let mut rendered: *mut c_char = std::ptr::null_mut();
    // SAFETY: a live `char *` slot, and the caller's format and arguments.
    let written = unsafe { crate::sprintf::rosemary_vasprintf(&raw mut rendered, fmt, ap) };
    if written < 0 || rendered.is_null() {
        return None;
    }
    // SAFETY: a NUL-terminated rendering, freed with the `malloc` that made it.
    let body = unsafe { CStr::from_ptr(rendered) }.to_owned();
    unsafe { libc::free(rendered.cast()) };

    if bare {
        return Some(body);
    }
    let mut full = progname.to_bytes().to_vec();
    full.extend_from_slice(b": ");
    full.extend_from_slice(body.to_bytes());
    // Neither part holds a NUL, as both came from C strings.
    CString::new(full).ok()
}

/// `void setproctitle(const char *fmt, ...)`
///
/// FIXME (ACCURACY): BSD rewrites the argument area `ps` shows; the host only
/// lets us rename the process, which it may truncate (15 bytes on Linux).
///
/// # Safety
///
/// `fmt` is null or a format that the arguments match.
pub unsafe extern "C" fn setproctitle(fmt: *const c_char, args: ...) {
    // SAFETY: the caller's obligations.
    let Some(title) = (unsafe { title(fmt, args) }) else {
        return;
    };
    // The call has no way to report failure, and a host that cannot rename
    // processes leaves the name the guest started with.
    let _ = starnix::process::set_name(&title);
}

exports::exports! {
    functions {
        "setproctitle" => setproctitle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders through [`title`] into `out`.
    unsafe extern "C" fn render(out: *mut Option<CString>, fmt: *const c_char, args: ...) {
        // SAFETY: the test's format and arguments match, and `out` is live.
        unsafe { *out = title(fmt, args) };
    }

    fn rendered(fmt: *const c_char, value: i32) -> Option<CString> {
        let mut out = None;
        // SAFETY: every format below takes one `int`.
        unsafe { render(&raw mut out, fmt, value) };
        out
    }

    /// Other tests rename the program, so compare against whatever it is now.
    fn progname() -> Vec<u8> {
        // SAFETY: `getprogname` answers a NUL-terminated string.
        unsafe { CStr::from_ptr(crate::progname::getprogname()) }
            .to_bytes()
            .to_vec()
    }

    #[test]
    fn prefixes_the_title_with_the_program_name() {
        let mut expected = progname();
        expected.extend_from_slice(b": worker 3");
        assert_eq!(
            rendered(c"worker %d".as_ptr(), 3).unwrap().as_bytes(),
            expected
        );
    }

    #[test]
    fn a_leading_dash_drops_the_program_name() {
        assert_eq!(
            rendered(c"-worker %d".as_ptr(), 3).as_deref(),
            Some(c"worker 3")
        );
    }

    #[test]
    fn a_null_format_restores_the_program_name() {
        assert_eq!(
            rendered(std::ptr::null(), 0).unwrap().as_bytes(),
            progname()
        );
    }
}
