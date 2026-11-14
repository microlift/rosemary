//! `fparseln`, `<util.h>`'s reader for configuration-file lines.

use std::ffi::{c_char, c_int};

use super::{File, fgetln};

/// `FPARSELN_UNESC*`: which escapes a parsed line loses.
const FPARSELN_UNESCESC: c_int = 0x01;
const FPARSELN_UNESCCONT: c_int = 0x02;
const FPARSELN_UNESCCOMM: c_int = 0x04;
const FPARSELN_UNESCREST: c_int = 0x08;
const FPARSELN_UNESCALL: c_int = 0x0f;

/// The escape, continuation and comment characters when the caller names none.
const DEFAULT_DELIMITERS: [u8; 3] = *b"\\\\#";

/// Whether `line[at]` follows an odd run of `esc`. With no escape character
/// everything counts as escaped, which is how libutil turns comments and
/// continuations off with it.
fn escaped(line: &[u8], at: usize, esc: u8) -> bool {
    if esc == 0 {
        return true;
    }
    line[..at].iter().rev().take_while(|&&b| b == esc).count() % 2 == 1
}

/// Drop the escapes `flags` names from `text`. A lone escape at the end is
/// dropped too, as libutil does.
fn unescape(text: &[u8], [esc, con, com]: [u8; 3], flags: c_int) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i] != esc {
            out.push(text[i]);
            i += 1;
            continue;
        }
        let Some(&next) = text.get(i + 1) else {
            break;
        };
        let skip = if next == com || next == con || next == esc {
            (next == com && flags & FPARSELN_UNESCCOMM != 0)
                || (next == con && flags & FPARSELN_UNESCCONT != 0)
                || (next == esc && flags & FPARSELN_UNESCESC != 0)
        } else {
            flags & FPARSELN_UNESCREST != 0
        };
        if !skip {
            out.push(esc);
        }
        out.push(next);
        i += 2;
    }
    out
}

/// One logical line from `next_line`'s physical lines: joined at
/// continuations, without comments or the newline. `None` at end-of-file.
fn parse(
    mut next_line: impl FnMut() -> Option<Vec<u8>>,
    lineno: &mut usize,
    [esc, con, com]: [u8; 3],
    flags: c_int,
) -> Option<Vec<u8>> {
    let mut buf: Option<Vec<u8>> = None;
    let mut more = true;
    while more {
        more = false;
        *lineno += 1;
        let Some(line) = next_line() else {
            break;
        };
        let mut s = line.len();
        if s > 0
            && com != 0
            && let Some(at) = (0..s).find(|&i| line[i] == com && !escaped(&line, i, esc))
        {
            s = at;
            // A line that is all comment is skipped, not returned empty.
            more = s == 0 && buf.is_none();
        }
        if s > 0 && line[s - 1] == b'\n' {
            s -= 1;
        }
        if s > 0 && con != 0 && line[s - 1] == con && !escaped(&line, s - 1, esc) {
            s -= 1;
            more = true;
        }
        if s == 0 && buf.is_some() {
            continue;
        }
        buf.get_or_insert_default().extend_from_slice(&line[..s]);
    }
    let mut buf = buf?;
    // libutil unescapes with C string functions, so only up to a NUL.
    let text = buf
        .iter()
        .position(|&b| b == 0)
        .map_or(&buf[..], |nul| &buf[..nul]);
    if flags & FPARSELN_UNESCALL != 0 && esc != 0 && text.contains(&esc) {
        buf = unescape(text, [esc, con, com], flags);
    }
    Some(buf)
}

/// `char *fparseln(FILE *stream, size_t *len, size_t *lineno,
/// const char delim[3], int flags)`
///
/// The line is the guest's to `free`.
///
/// # Safety
///
/// `stream` is open; `len` and `lineno` are null or writable; `delim` is null
/// or three readable bytes.
pub unsafe extern "C" fn fparseln(
    fp: *mut File,
    size: *mut usize,
    lineno: *mut usize,
    delim: *const c_char,
    flags: c_int,
) -> *mut c_char {
    let delimiters = match delim.is_null() {
        true => DEFAULT_DELIMITERS,
        // SAFETY: three readable bytes, by contract.
        false => unsafe { delim.cast::<[u8; 3]>().read() },
    };
    let mut ignored = 0;
    let count = match lineno.is_null() {
        true => &mut ignored,
        // SAFETY: the caller's writable counter.
        false => unsafe { &mut *lineno },
    };
    let next_line = || {
        let mut len = 0;
        // SAFETY: the caller's stream and a local length.
        let line = unsafe { fgetln(fp, &mut len) };
        // SAFETY: fgetln's line is `len` bytes, valid until the next read.
        (!line.is_null())
            .then(|| unsafe { std::slice::from_raw_parts(line.cast::<u8>(), len) }.to_vec())
    };
    let Some(line) = parse(next_line, count, delimiters, flags) else {
        if !size.is_null() {
            // SAFETY: the caller's writable length.
            unsafe { *size = 0 };
        }
        return std::ptr::null_mut();
    };
    // SAFETY: the guest's allocator, so the guest can free the line.
    let out = unsafe { crate::heap::malloc(line.len() + 1) }.cast::<u8>();
    if out.is_null() {
        crate::errno::set(libc::ENOMEM);
        return std::ptr::null_mut();
    }
    // SAFETY: `out` holds the line and its terminator.
    unsafe {
        std::ptr::copy_nonoverlapping(line.as_ptr(), out, line.len());
        out.add(line.len()).write(0);
        if !size.is_null() {
            *size = line.len();
        }
    }
    out.cast()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every logical line of `text`, with the line count after each.
    fn lines(text: &str, flags: c_int) -> Vec<(String, usize)> {
        let mut physical = text.split_inclusive('\n').map(|l| l.as_bytes().to_vec());
        let mut lineno = 0;
        std::iter::from_fn(|| {
            let line = parse(|| physical.next(), &mut lineno, DEFAULT_DELIMITERS, flags)?;
            Some((String::from_utf8(line).unwrap(), lineno))
        })
        .collect()
    }

    #[test]
    fn joins_continuations_and_drops_comments() {
        let text = "# This is a test\nline 1\nline 2 \\\nline 3 # Comment\n\
                    line 4 \\# Not comment \\\\\\\\\n\n# And a comment \\\nline 5 \\\\\\\nline 6\n";
        assert_eq!(
            lines(text, 0),
            [
                ("line 1".into(), 2),
                ("line 2 line 3 ".into(), 4),
                ("line 4 \\# Not comment \\\\\\\\".into(), 5),
                ("".into(), 6),
                ("line 5 \\\\line 6".into(), 9),
            ]
        );
    }

    #[test]
    fn unescapes_what_the_flags_name() {
        let text = "a \\# b \\\\ c \\x\n";
        assert_eq!(lines(text, FPARSELN_UNESCALL)[0].0, "a # b \\ c x");
        assert_eq!(lines(text, FPARSELN_UNESCCOMM)[0].0, "a # b \\\\ c \\x");
        assert_eq!(lines(text, FPARSELN_UNESCREST)[0].0, "a \\# b \\\\ c x");
    }

    #[test]
    fn a_last_line_without_a_newline_is_still_read() {
        assert_eq!(lines("tail", 0), [("tail".into(), 1)]);
    }

    #[test]
    fn fparseln_reads_a_stream_into_a_guest_block() {
        let mut text = *b"key \\\n  value # note\nlast";
        let mut len = 0;
        let mut lineno = 0;
        unsafe {
            let fp = super::super::fmemopen(text.as_mut_ptr().cast(), text.len(), c"r".as_ptr());
            let line = fparseln(fp, &mut len, &mut lineno, std::ptr::null(), 0);
            assert_eq!(std::ffi::CStr::from_ptr(line), c"key   value ");
            assert_eq!((len, lineno), (12, 2));
            crate::heap::free(line.cast());
            let line = fparseln(fp, &mut len, &mut lineno, c"\\\\;".as_ptr(), 0);
            assert_eq!(std::ffi::CStr::from_ptr(line), c"last");
            crate::heap::free(line.cast());
            assert!(fparseln(fp, &mut len, &mut lineno, std::ptr::null(), 0).is_null());
            assert_eq!((len, lineno), (0, 4));
            super::super::fclose(fp);
        }
    }

    #[test]
    fn end_of_file_still_counts_a_line() {
        let mut lineno = 0;
        assert_eq!(parse(|| None, &mut lineno, DEFAULT_DELIMITERS, 0), None);
        assert_eq!(lineno, 1);
    }
}
