use std::ffi::{CStr, CString, c_char, c_int, c_void};

use super::*;

/// A path under the system temporary directory, unique to this test.
fn scratch(name: &str) -> CString {
    let dir = std::env::temp_dir().join(format!("rosemary-stdio-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    CString::new(dir.join(name).into_os_string().into_encoded_bytes()).unwrap()
}

fn contents(path: &CStr) -> Vec<u8> {
    std::fs::read(path.to_str().unwrap()).unwrap()
}

/// What a guest's inlined `putc_unlocked` does: Darwin's `__sputc`, verbatim.
unsafe fn inline_putc(c: c_int, fp: *mut File) -> c_int {
    // SAFETY: the test's open stream.
    unsafe {
        (*fp).w -= 1;
        if (*fp).w >= 0 || ((*fp).w >= (*fp).lbfsize && c as u8 != b'\n') {
            *(*fp).p = c as u8;
            (*fp).p = (*fp).p.add(1);
            return c;
        }
        __swbuf(c, fp)
    }
}

/// What a guest's inlined `getc_unlocked` does: Darwin's `__sgetc`.
unsafe fn inline_getc(fp: *mut File) -> c_int {
    // SAFETY: the test's open stream.
    unsafe {
        (*fp).r -= 1;
        if (*fp).r < 0 {
            return __srget(fp);
        }
        let c = *(*fp).p;
        (*fp).p = (*fp).p.add(1);
        c_int::from(c)
    }
}

#[test]
fn the_static_streams_are_darwin_s_three_with_their_descriptors() {
    let sf = symbol("__sF").unwrap() as usize as *mut File;
    for (i, name) in ["__stdinp", "__stdoutp", "__stderrp"].iter().enumerate() {
        let at = symbol(name).unwrap() as usize as *const *mut File;
        // SAFETY: the exported pointer cells and the streams they name.
        unsafe {
            assert_eq!(*at, sf.add(i), "{name} points into __sF");
            assert_eq!(c_int::from((**at).file), i as c_int);
        }
    }
    // SAFETY: the static `stderr`.
    assert_ne!(
        unsafe { (*sf.add(2)).flags } & flag::NBF,
        0,
        "stderr is unbuffered"
    );
}

#[test]
fn a_written_file_reads_back_through_every_reader() {
    let path = scratch("roundtrip");
    // SAFETY: NUL-terminated strings and the streams they open.
    unsafe {
        let fp = fopen(path.as_ptr(), c"w".as_ptr());
        assert!(!fp.is_null());
        assert_eq!(fputs(c"first line\n".as_ptr(), fp), 11);
        assert_eq!(fwrite(b"second\nthird".as_ptr().cast(), 1, 12, fp), 12);
        assert_eq!(fputc(c_int::from(b'!'), fp), c_int::from(b'!'));
        assert_eq!(fclose(fp), 0);
    }
    assert_eq!(contents(&path), b"first line\nsecond\nthird!");

    // SAFETY: as above.
    unsafe {
        let fp = fopen(path.as_ptr(), c"r".as_ptr());
        let mut buf = [0 as c_char; 64];
        assert!(!fgets(buf.as_mut_ptr(), 64, fp).is_null());
        assert_eq!(CStr::from_ptr(buf.as_ptr()).to_bytes(), b"first line\n");

        let mut line: *mut c_char = std::ptr::null_mut();
        let mut cap = 0usize;
        assert_eq!(getline(&mut line, &mut cap, fp), 7);
        assert_eq!(CStr::from_ptr(line).to_bytes(), b"second\n");
        assert_eq!(getline(&mut line, &mut cap, fp), 6);
        assert_eq!(CStr::from_ptr(line).to_bytes(), b"third!");
        assert_eq!(getline(&mut line, &mut cap, fp), -1);
        assert_ne!(feof(fp), 0);
        crate::heap::free(line.cast());

        rewind(fp);
        assert_eq!(feof(fp), 0);
        let mut all = [0u8; 64];
        assert_eq!(fread(all.as_mut_ptr().cast(), 1, 64, fp), 24);
        assert_eq!(&all[..24], b"first line\nsecond\nthird!");
        assert_eq!(fclose(fp), 0);
    }
}

#[test]
fn the_inlined_macros_and_the_functions_share_one_buffer() {
    let path = scratch("inline");
    // SAFETY: the test's own streams.
    unsafe {
        let fp = fopen(path.as_ptr(), c"w+".as_ptr());
        for &b in b"inline " {
            inline_putc(c_int::from(b), fp);
        }
        fputs(c"call".as_ptr(), fp);
        assert_eq!(
            ftell(fp),
            11,
            "the buffered bytes count toward the position"
        );
        assert_eq!(fseek(fp, 0, libc::SEEK_SET), 0);
        let mut got = Vec::new();
        loop {
            let c = inline_getc(fp);
            if c == EOF {
                break;
            }
            got.push(c as u8);
        }
        assert_eq!(got, b"inline call");
        assert_ne!(feof_unlocked(fp), 0);
        fclose(fp);
    }
}

#[test]
fn ungetc_pushes_back_more_than_the_small_buffer_holds() {
    let path = scratch("ungetc");
    std::fs::write(path.to_str().unwrap(), b"xyz").unwrap();
    // SAFETY: the test's own stream.
    unsafe {
        let fp = fopen(path.as_ptr(), c"r".as_ptr());
        assert_eq!(fgetc(fp), c_int::from(b'x'));
        // More than `_ubuf`'s three bytes, none of them the byte just read.
        for b in (b'a'..=b'e').rev() {
            assert_eq!(ungetc(c_int::from(b), fp), c_int::from(b));
        }
        let mut got = Vec::new();
        loop {
            let c = fgetc(fp);
            if c == EOF {
                break;
            }
            got.push(c as u8);
        }
        assert_eq!(got, b"abcdeyz");
        fclose(fp);
    }
}

#[test]
fn append_mode_writes_at_the_end_and_says_so() {
    let path = scratch("append");
    std::fs::write(path.to_str().unwrap(), b"0123").unwrap();
    // SAFETY: the test's own stream.
    unsafe {
        let fp = fopen(path.as_ptr(), c"a+".as_ptr());
        assert_eq!(ftell(fp), 4);
        fseek(fp, 0, libc::SEEK_SET);
        assert_eq!(fgetc(fp), c_int::from(b'0'));
        fseek(fp, 0, libc::SEEK_CUR);
        fputs(c"45".as_ptr(), fp);
        assert_eq!(ftell(fp), 6);
        fclose(fp);
    }
    assert_eq!(contents(&path), b"012345");
}

#[test]
fn x_mode_refuses_an_existing_file() {
    let path = scratch("exclusive");
    std::fs::write(path.to_str().unwrap(), b"").unwrap();
    // SAFETY: NUL-terminated strings.
    let fp = unsafe { fopen(path.as_ptr(), c"wx".as_ptr()) };
    assert!(fp.is_null());
    assert_eq!(crate::errno::get(), libc::EEXIST);
    // SAFETY: as above.
    assert!(unsafe { fopen(path.as_ptr(), c"rx".as_ptr()) }.is_null());
}

#[test]
fn line_buffering_holds_output_until_a_newline() {
    let mut ends = [0; 2];
    // SAFETY: a writable pair.
    assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
    // SAFETY: the pipe's ends, nonblocking so an empty read returns.
    unsafe {
        libc::fcntl(ends[0], libc::F_SETFL, libc::O_NONBLOCK);
        let fp = fdopen(ends[1], c"w".as_ptr());
        assert_eq!(setvbuf(fp, std::ptr::null_mut(), IOLBF, 0), 0);
        let mut buf = [0u8; 32];
        inline_putc(c_int::from(b'a'), fp);
        fputs(c"bc".as_ptr(), fp);
        assert_eq!(
            libc::read(ends[0], buf.as_mut_ptr().cast(), 32),
            -1,
            "nothing yet"
        );
        inline_putc(c_int::from(b'\n'), fp);
        assert_eq!(libc::read(ends[0], buf.as_mut_ptr().cast(), 32), 4);
        assert_eq!(&buf[..4], b"abc\n");
        fclose(fp);
        libc::close(ends[0]);
    }
}

#[test]
fn a_full_buffer_is_flushed_by_fflush_and_by_exit_s_walk() {
    let path = scratch("flush");
    // SAFETY: the test's own stream.
    unsafe {
        let fp = fopen(path.as_ptr(), c"w".as_ptr());
        fputs(c"held".as_ptr(), fp);
        assert_eq!(contents(&path), b"");
        assert_eq!(api::flush_all(false), 0);
        assert_eq!(contents(&path), b"held");
        fputs(c" more".as_ptr(), fp);
        assert_eq!(fflush(fp), 0);
        assert_eq!(contents(&path), b"held more");
        fclose(fp);
    }
}

#[test]
fn freopen_keeps_the_descriptor_number() {
    let first = scratch("reopen-a");
    let second = scratch("reopen-b");
    // SAFETY: the test's own streams.
    unsafe {
        let fp = fopen(first.as_ptr(), c"w".as_ptr());
        let fd = fileno(fp);
        fputs(c"one".as_ptr(), fp);
        assert_eq!(freopen(second.as_ptr(), c"w".as_ptr(), fp), fp);
        assert_eq!(fileno(fp), fd);
        fputs(c"two".as_ptr(), fp);
        fclose(fp);
    }
    assert_eq!(contents(&first), b"one", "the old stream was flushed");
    assert_eq!(contents(&second), b"two");
}

struct Source {
    data: &'static [u8],
    at: usize,
    closed: bool,
}

unsafe extern "C" fn read_source(cookie: *mut c_void, buf: *mut c_char, n: c_int) -> c_int {
    // SAFETY: the test's own cookie and the engine's buffer.
    unsafe {
        let s = &mut *cookie.cast::<Source>();
        let take = (n as usize).min(s.data.len() - s.at);
        std::ptr::copy_nonoverlapping(s.data[s.at..].as_ptr().cast(), buf, take);
        s.at += take;
        take as c_int
    }
}

unsafe extern "C" fn close_source(cookie: *mut c_void) -> c_int {
    // SAFETY: the test's own cookie.
    unsafe { (*cookie.cast::<Source>()).closed = true };
    0
}

#[test]
fn a_funopen_stream_reads_through_the_callback_and_fgetln_splits_lines() {
    let mut source = Source {
        data: b"first\nsecond",
        at: 0,
        closed: false,
    };
    // SAFETY: a cookie that outlives the stream, and callbacks of this test's.
    unsafe {
        let f = funopen(
            (&raw mut source).cast(),
            Some(read_source),
            None,
            None,
            Some(close_source),
        );
        assert!(!f.is_null());
        assert_eq!(fileno(f), -1);
        let mut len = 0;
        let line = fgetln(f, &mut len);
        assert_eq!(
            std::slice::from_raw_parts(line.cast::<u8>(), len),
            b"first\n"
        );
        let line = fgetln(f, &mut len);
        assert_eq!(
            std::slice::from_raw_parts(line.cast::<u8>(), len),
            b"second"
        );
        assert!(fgetln(f, &mut len).is_null());
        assert_eq!(fputc(c_int::from(b'x'), f), EOF, "read only");
        assert_eq!(fclose(f), 0);
    }
    assert!(source.closed);
}

#[test]
fn a_stream_with_no_direction_is_refused() {
    // SAFETY: no callbacks, so nothing is ever called.
    let f = unsafe { funopen(std::ptr::null(), None, None, None, None) };
    assert!(f.is_null());
    assert_eq!(crate::errno::get(), libc::EINVAL);
}

#[test]
fn fmemopen_reads_writes_and_stops_at_the_end() {
    let mut buf = *b"hello world\0\0\0\0\0";
    // SAFETY: a buffer that outlives the streams.
    unsafe {
        let fp = fmemopen(buf.as_mut_ptr().cast(), 11, c"r".as_ptr());
        let mut word = [0 as c_char; 8];
        assert!(!fgets(word.as_mut_ptr(), 6, fp).is_null());
        assert_eq!(CStr::from_ptr(word.as_ptr()).to_bytes(), b"hello");
        fclose(fp);

        let fp = fmemopen(buf.as_mut_ptr().cast(), 8, c"w".as_ptr());
        assert_eq!(buf[0], 0, "w empties a text buffer");
        assert_eq!(fwrite(b"0123456789".as_ptr().cast(), 1, 10, fp), 8);
        assert_ne!(ferror(fp), 0);
        fclose(fp);
    }
    assert_eq!(&buf[..8], b"01234567");
}

#[test]
fn open_memstream_keeps_the_caller_s_variables_current() {
    let mut out: *mut c_char = std::ptr::null_mut();
    let mut size = 0usize;
    // SAFETY: two variables that outlive the stream.
    unsafe {
        let fp = open_memstream(&mut out, &mut size);
        fputs(c"grow".as_ptr(), fp);
        fflush(fp);
        assert_eq!(size, 4);
        assert_eq!(CStr::from_ptr(out).to_bytes(), b"grow");
        for _ in 0..1000 {
            fputc(c_int::from(b'.'), fp);
        }
        fclose(fp);
        assert_eq!(size, 1004);
        assert_eq!(CStr::from_ptr(out).to_bytes().len(), 1004);
        crate::heap::free(out.cast());
    }
}

/// The variadic entry points, as a guest's call reaches them.
#[cfg(not(all(target_arch = "aarch64", not(target_vendor = "apple"))))]
fn variadic<T>(name: &str) -> T {
    let at = symbol(name).unwrap_or_else(|| panic!("{name} is published"));
    assert_eq!(size_of::<T>(), size_of::<usize>());
    // SAFETY: the caller names the symbol's real C type.
    unsafe { std::mem::transmute_copy(&(at as usize)) }
}

/// The published entries take Darwin's convention here, which this host's
/// ellipsis is not, so each is reached through its `va_list` form instead.
#[cfg(all(target_arch = "aarch64", not(target_vendor = "apple")))]
fn variadic<T>(name: &str) -> T {
    unsafe extern "C" fn fprintf(fp: *mut File, format: *const c_char, args: ...) -> c_int {
        // SAFETY: the caller's obligations.
        unsafe { vfprintf(fp, format, args) }
    }
    unsafe extern "C" fn fscanf(fp: *mut File, format: *const c_char, args: ...) -> c_int {
        // SAFETY: the caller's obligations.
        unsafe { vfscanf(fp, format, args) }
    }
    unsafe extern "C" fn fwprintf(fp: *mut File, format: *const u32, args: ...) -> c_int {
        // SAFETY: the caller's obligations.
        unsafe { vfwprintf(fp, format, args) }
    }
    let at = match name {
        "fprintf" => fprintf as *const (),
        "fscanf" => fscanf as *const (),
        "fwprintf" => fwprintf as *const (),
        _ => panic!("{name} has no host-convention form here"),
    };
    assert_eq!(size_of::<T>(), size_of::<usize>());
    // SAFETY: the caller names the function's real C type.
    unsafe { std::mem::transmute_copy(&(at as usize)) }
}

#[test]
fn fprintf_renders_into_the_stream() {
    let path = scratch("fprintf");
    let fprintf: unsafe extern "C" fn(*mut File, *const c_char, ...) -> c_int = variadic("fprintf");
    // SAFETY: the test's own stream and a format matching its arguments.
    unsafe {
        let fp = fopen(path.as_ptr(), c"w".as_ptr());
        let n = fprintf(
            fp,
            c"%s=%d %.2f\n".as_ptr(),
            c"x".as_ptr(),
            42 as c_int,
            2.5f64,
        );
        assert_eq!(n, 10);
        fclose(fp);
    }
    assert_eq!(contents(&path), b"x=42 2.50\n");
}

#[test]
fn fscanf_converts_and_reports_what_it_assigned() {
    let path = scratch("fscanf");
    std::fs::write(
        path.to_str().unwrap(),
        b"  -17 0x1F 3.5e2 word abc]def 077\n",
    )
    .unwrap();
    let fscanf: unsafe extern "C" fn(*mut File, *const c_char, ...) -> c_int = variadic("fscanf");
    // SAFETY: the test's own stream and destinations matching the format.
    unsafe {
        let fp = fopen(path.as_ptr(), c"r".as_ptr());
        let (mut d, mut x, mut f, mut o, mut n) =
            (0 as c_int, 0 as c_int, 0f64, 0 as c_int, 0 as c_int);
        let mut word = [0 as c_char; 16];
        let mut set = [0 as c_char; 16];
        let assigned = fscanf(
            fp,
            c"%d %i %lf %15s %15[a-c]]%*s %i%n".as_ptr(),
            &mut d,
            &mut x,
            &mut f,
            word.as_mut_ptr(),
            set.as_mut_ptr(),
            &mut o,
            &mut n,
        );
        assert_eq!(assigned, 6);
        assert_eq!((d, x, f, o), (-17, 31, 350.0, 0o77));
        assert_eq!(CStr::from_ptr(word.as_ptr()).to_bytes(), b"word");
        assert_eq!(CStr::from_ptr(set.as_ptr()).to_bytes(), b"abc");
        assert_eq!(n, 33);
        assert_eq!(fgetc(fp), c_int::from(b'\n'), "the rest is still unread");
        assert_eq!(fscanf(fp, c"%d".as_ptr(), &mut d), EOF);
        fclose(fp);
    }
}

#[test]
fn a_matching_failure_leaves_the_input_where_it_failed() {
    let path = scratch("fscanf-fail");
    std::fs::write(path.to_str().unwrap(), b"12 abc").unwrap();
    let fscanf: unsafe extern "C" fn(*mut File, *const c_char, ...) -> c_int = variadic("fscanf");
    // SAFETY: as above.
    unsafe {
        let fp = fopen(path.as_ptr(), c"r".as_ptr());
        let (mut a, mut b) = (0 as c_int, 0 as c_int);
        assert_eq!(fscanf(fp, c"%d %d".as_ptr(), &mut a, &mut b), 1);
        assert_eq!(a, 12);
        assert_eq!(fgetc(fp), c_int::from(b'a'));
        fclose(fp);
    }
}

#[test]
fn wide_characters_round_trip_and_orient_the_stream() {
    let path = scratch("wide");
    // SAFETY: the test's own stream.
    unsafe {
        let fp = fopen(path.as_ptr(), c"w+".as_ptr());
        assert_eq!(fwide(fp, 0), 0);
        for wc in "wide\n".chars() {
            assert_eq!(fputwc(wc as c_int, fp), wc as c_int);
        }
        assert!(fwide(fp, -1) > 0, "a wide write fixed the orientation");
        rewind(fp);
        let mut got = [0u32; 8];
        assert!(!fgetws(got.as_mut_ptr(), 8, fp).is_null());
        assert_eq!(&got[..6], &[119, 105, 100, 101, 10, 0]);
        assert_eq!(fgetwc(fp), WEOF);
        fclose(fp);
    }
}

#[test]
fn popen_reads_a_command_s_output_and_pclose_returns_its_status() {
    // A Mach-O `/bin/sh` is relaunched through the runtime, which in a unit
    // test is this test binary rather than rosemary.
    if crate::spawn::is_mach_o(c"/bin/sh") {
        eprintln!("skipped: /bin/sh is a Mach-O and needs the real runtime to start");
        return;
    }
    // SAFETY: constant strings and the stream `popen` returns.
    unsafe {
        let fp = popen(c"echo popened; exit 3".as_ptr(), c"r".as_ptr());
        assert!(!fp.is_null());
        let mut buf = [0 as c_char; 32];
        assert!(!fgets(buf.as_mut_ptr(), 32, fp).is_null());
        assert_eq!(CStr::from_ptr(buf.as_ptr()).to_bytes(), b"popened\n");
        let status = pclose(fp);
        assert_eq!((status >> 8) & 0xff, 3, "status {status:#x}");
    }
}

#[test]
fn tmpfile_is_readable_writable_and_already_unlinked() {
    // SAFETY: the stream `tmpfile` returns.
    unsafe {
        let fp = tmpfile();
        assert!(!fp.is_null());
        fputs(c"scratch".as_ptr(), fp);
        rewind(fp);
        let mut buf = [0 as c_char; 16];
        assert!(!fgets(buf.as_mut_ptr(), 16, fp).is_null());
        assert_eq!(CStr::from_ptr(buf.as_ptr()).to_bytes(), b"scratch");
        let mut st: libc::stat = std::mem::zeroed();
        assert_eq!(libc::fstat(fileno(fp), &mut st), 0);
        assert_eq!(st.st_nlink, 0);
        fclose(fp);
    }
}

#[test]
fn fwprintf_writes_multibyte_text() {
    let path = scratch("fwprintf");
    let wide: Vec<u32> = "n=%d\n\0".chars().map(|c| c as u32).collect();
    let fwprintf: unsafe extern "C" fn(*mut File, *const u32, ...) -> c_int = variadic("fwprintf");
    // SAFETY: the test's own stream and a format matching its argument.
    unsafe {
        let fp = fopen(path.as_ptr(), c"w".as_ptr());
        assert_eq!(fwprintf(fp, wide.as_ptr(), 5 as c_int), 4);
        assert!(fwide(fp, 0) > 0);
        fclose(fp);
    }
    assert_eq!(contents(&path), b"n=5\n");
}

#[test]
fn a_long_double_is_stored_in_this_architecture_s_format() {
    let path = scratch("long-double");
    std::fs::write(path.to_str().unwrap(), b"1.5 -0.25").unwrap();
    let fscanf: unsafe extern "C" fn(*mut File, *const c_char, ...) -> c_int = variadic("fscanf");
    let (mut a, mut b) = ([0u8; 16], [0u8; 16]);
    // SAFETY: two buffers at least as large as any `long double`.
    unsafe {
        let fp = fopen(path.as_ptr(), c"r".as_ptr());
        assert_eq!(
            fscanf(fp, c"%Lf %Lg".as_ptr(), a.as_mut_ptr(), b.as_mut_ptr()),
            2
        );
        fclose(fp);
    }
    if cfg!(target_arch = "x86_64") {
        // x87: explicit integer bit, 15-bit exponent biased by 16383.
        assert_eq!(&a[..10], &[0, 0, 0, 0, 0, 0, 0, 0xc0, 0xff, 0x3f]);
        assert_eq!(&b[..10], &[0, 0, 0, 0, 0, 0, 0, 0x80, 0xfd, 0xbf]);
    } else {
        assert_eq!(f64::from_le_bytes(a[..8].try_into().unwrap()), 1.5);
        assert_eq!(f64::from_le_bytes(b[..8].try_into().unwrap()), -0.25);
    }
}
