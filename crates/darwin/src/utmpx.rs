#![cfg(not(target_os = "macos"))]

use std::ffi::{CStr, CString, c_char, c_int};
use std::sync::Mutex;

/// Darwin's `MAXPATHLEN`, which holds the name and its NUL.
const MAXPATHLEN: usize = 1024;

/// The guest's `wtmpx` file, or `None` for Darwin's default.
static WTMPX: Mutex<Option<CString>> = Mutex::new(None);

/// `int wtmpxname(const char *fname)`
///
/// Returns 1 if the name was taken and 0 if not, leaving the previous name in
/// place. Null goes back to the default, as Darwin's does.
///
/// # Safety
///
/// `fname` is null or NUL-terminated.
pub unsafe extern "C" fn wtmpxname(fname: *const c_char) -> c_int {
    let name = if fname.is_null() {
        None
    } else {
        let name = unsafe { CStr::from_ptr(fname) };
        let bytes = name.to_bytes();
        // Darwin insists on the suffix so an old-format `wtmp` is never
        // appended to as if it were `wtmpx`.
        if bytes.len() >= MAXPATHLEN || bytes.last() != Some(&b'x') {
            return 0;
        }
        Some(name.to_owned())
    };
    *WTMPX.lock().unwrap_or_else(|e| e.into_inner()) = name;
    1
}

exports::exports! {
    functions {
        "wtmpxname" => wtmpxname,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current() -> Option<CString> {
        WTMPX.lock().unwrap().clone()
    }

    #[test]
    fn wtmpxname_takes_only_x_suffixed_paths() {
        // SAFETY: NUL-terminated literals and null.
        unsafe {
            assert_eq!(wtmpxname(c"/tmp/wtmpx".as_ptr()), 1);
            assert_eq!(current().as_deref(), Some(c"/tmp/wtmpx"));

            assert_eq!(wtmpxname(c"/tmp/wtmp".as_ptr()), 0);
            assert_eq!(wtmpxname(c"".as_ptr()), 0);
            let long = CString::new(format!("/{}x", "a".repeat(MAXPATHLEN))).unwrap();
            assert_eq!(wtmpxname(long.as_ptr()), 0);
            assert_eq!(
                current().as_deref(),
                Some(c"/tmp/wtmpx"),
                "a refusal keeps the old name"
            );

            assert_eq!(wtmpxname(std::ptr::null()), 1);
            assert_eq!(current(), None);
        }
    }
}
