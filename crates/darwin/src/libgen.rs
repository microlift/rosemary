//! `libgen.h`'s reentrant spellings, which glibc does not have.

use std::ffi::{CStr, c_char};

use crate::stat::DARWIN_PATH_MAX;

/// The parent of `path` by FreeBSD's rules, which Darwin's libc keeps: no
/// slash means `.`, and trailing slashes belong to neither half.
fn parent(path: &[u8]) -> &[u8] {
    if path.is_empty() {
        return b".";
    }
    let mut end = path.len() - 1;
    while end > 0 && path[end] == b'/' {
        end -= 1;
    }
    while end > 0 && path[end] != b'/' {
        end -= 1;
    }
    if end == 0 {
        return if path[0] == b'/' { b"/" } else { b"." };
    }
    loop {
        end -= 1;
        if end == 0 || path[end] != b'/' {
            break;
        }
    }
    &path[..=end]
}

/// `char *dirname_r(const char *path, char *dname)`, with `dname` at least
/// `MAXPATHLEN` bytes.
///
/// # Safety
///
/// `path` is null or a NUL-terminated string, and `dname` is writable for
/// `MAXPATHLEN` bytes.
pub unsafe extern "C" fn dirname_r(path: *const c_char, dname: *mut c_char) -> *mut c_char {
    let path = match path.is_null() {
        true => &[][..],
        // SAFETY: the caller's.
        false => unsafe { CStr::from_ptr(path) }.to_bytes(),
    };
    let parent = parent(path);
    if parent.len() >= DARWIN_PATH_MAX {
        crate::errno::set(libc::ENAMETOOLONG);
        return std::ptr::null_mut();
    }
    // SAFETY: `dname` holds `MAXPATHLEN` bytes, and `parent` is shorter.
    unsafe {
        std::ptr::copy(parent.as_ptr(), dname.cast::<u8>(), parent.len());
        dname.add(parent.len()).write(0);
    }
    dname
}

exports::exports! {
    functions {
        "dirname_r" => dirname_r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parent_follows_freebsds_rules() {
        assert_eq!(parent(b"/usr/lib/libc.dylib"), b"/usr/lib");
        assert_eq!(parent(b"/usr/lib//"), b"/usr");
        assert_eq!(parent(b"a//b"), b"a");
        assert_eq!(parent(b"/a"), b"/");
        assert_eq!(parent(b"///"), b"/");
        assert_eq!(parent(b"name"), b".");
        assert_eq!(parent(b""), b".");
    }

    #[test]
    fn dirname_r_writes_into_the_callers_buffer() {
        let mut out = [0 as c_char; DARWIN_PATH_MAX];
        let got = unsafe { dirname_r(c"/tmp/x".as_ptr(), out.as_mut_ptr()) };
        assert_eq!(got, out.as_mut_ptr());
        assert_eq!(unsafe { CStr::from_ptr(got) }, c"/tmp");
    }
}
