use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::sync::atomic::{AtomicI32, Ordering};

/// POSIX's distinguished `(locale_t)-1`, omitted by the Rust libc bindings on
/// this target.
pub(crate) fn global_locale() -> libc::locale_t {
    usize::MAX as libc::locale_t
}

/// Darwin numbers locale categories in BSD order; glibc uses the order from
/// its locale archive. Both sets of values are public ABI because callers pass
/// the category itself to `setlocale` and one bit per category to `newlocale`.
mod category {
    use super::c_int;

    pub const ALL: c_int = 0;
    pub const COLLATE: c_int = 1;
    pub const CTYPE: c_int = 2;
    pub const MONETARY: c_int = 3;
    pub const NUMERIC: c_int = 4;
    pub const TIME: c_int = 5;
    pub const MESSAGES: c_int = 6;

    pub const COLLATE_MASK: c_int = 1 << 0;
    pub const CTYPE_MASK: c_int = 1 << 1;
    pub const MESSAGES_MASK: c_int = 1 << 2;
    pub const MONETARY_MASK: c_int = 1 << 3;
    pub const NUMERIC_MASK: c_int = 1 << 4;
    pub const TIME_MASK: c_int = 1 << 5;
    pub const ALL_MASK: c_int = (1 << 6) - 1;
}

fn host_category(guest: c_int) -> Option<c_int> {
    Some(match guest {
        category::ALL => libc::LC_ALL,
        category::COLLATE => libc::LC_COLLATE,
        category::CTYPE => libc::LC_CTYPE,
        category::MONETARY => libc::LC_MONETARY,
        category::NUMERIC => libc::LC_NUMERIC,
        category::TIME => libc::LC_TIME,
        category::MESSAGES => libc::LC_MESSAGES,
        _ => return None,
    })
}

fn host_category_mask(guest: c_int) -> Option<c_int> {
    if guest & !category::ALL_MASK != 0 {
        return None;
    }
    let mut host = 0;
    for (guest_bit, host_bit) in [
        (category::COLLATE_MASK, libc::LC_COLLATE_MASK),
        (category::CTYPE_MASK, libc::LC_CTYPE_MASK),
        (category::MESSAGES_MASK, libc::LC_MESSAGES_MASK),
        (category::MONETARY_MASK, libc::LC_MONETARY_MASK),
        (category::NUMERIC_MASK, libc::LC_NUMERIC_MASK),
        (category::TIME_MASK, libc::LC_TIME_MASK),
    ] {
        if guest & guest_bit != 0 {
            host |= host_bit;
        }
    }
    Some(host)
}

/// `char *setlocale(int category, const char *locale)`.
///
/// # Safety
///
/// `locale` is null or a NUL-terminated locale name.
pub unsafe extern "C" fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char {
    let Some(category) = host_category(category) else {
        crate::errno::set(libc::EINVAL);
        return core::ptr::null_mut();
    };
    // SAFETY: the caller's string, which is null or NUL-terminated.
    let name = if !locale.is_null() && unsafe { *locale } == 0 {
        unsafe { set_from_guest_environment(category) }
    } else {
        // SAFETY: the caller's string and the translated host category.
        unsafe { libc::setlocale(category, locale) }
    };
    if !name.is_null() && (category == libc::LC_ALL || category == libc::LC_CTYPE) {
        let max = unsafe { plain::__ctype_get_mb_cur_max() };
        MB_CUR_MAX.store(max as c_int, Ordering::Relaxed);
    }
    name
}

/// `setlocale(category, "")`: each category from the guest's `LC_ALL`, then
/// its own variable, then `LANG`, never the host's environment. All or
/// nothing, as POSIX asks: when one category's name is refused, the ones
/// already changed are put back.
///
/// # Safety
///
/// `category` is a host category.
unsafe fn set_from_guest_environment(category: c_int) -> *mut c_char {
    const CATEGORIES: [(c_int, &CStr); 6] = [
        (libc::LC_COLLATE, c"LC_COLLATE"),
        (libc::LC_CTYPE, c"LC_CTYPE"),
        (libc::LC_MONETARY, c"LC_MONETARY"),
        (libc::LC_NUMERIC, c"LC_NUMERIC"),
        (libc::LC_TIME, c"LC_TIME"),
        (libc::LC_MESSAGES, c"LC_MESSAGES"),
    ];
    let guest = |name: &CStr| {
        // SAFETY: a NUL-terminated name; the answer is null or a guest string.
        let value = unsafe { crate::env::getenv(name.as_ptr()) };
        // SAFETY: a non-null answer is a NUL-terminated guest string, copied
        // before anything can change it.
        (!value.is_null() && unsafe { *value } != 0)
            .then(|| unsafe { CStr::from_ptr(value) }.to_owned())
    };
    let mut changed: Vec<(c_int, std::ffi::CString)> = Vec::new();
    for (host, variable) in CATEGORIES {
        if category != libc::LC_ALL && category != host {
            continue;
        }
        let name = guest(c"LC_ALL")
            .or_else(|| guest(variable))
            .or_else(|| guest(c"LANG"))
            .unwrap_or_else(|| c"C".to_owned());
        // SAFETY: a query, whose answer is copied before the next call.
        let before = unsafe { libc::setlocale(host, core::ptr::null()) };
        let before = (!before.is_null()).then(|| unsafe { CStr::from_ptr(before) }.to_owned());
        // SAFETY: a NUL-terminated name and a host category.
        if unsafe { libc::setlocale(host, name.as_ptr()) }.is_null() {
            for (host, before) in changed.iter().rev() {
                // SAFETY: a name the host itself reported for this category.
                unsafe { libc::setlocale(*host, before.as_ptr()) };
            }
            return core::ptr::null_mut();
        }
        if let Some(before) = before {
            changed.push((host, before));
        }
    }
    // SAFETY: a query, which changes nothing.
    unsafe { libc::setlocale(category, core::ptr::null()) }
}

/// Darwin's `int __mb_cur_max`, the global `MB_CUR_MAX` read before it became
/// a function. Old headers still inline a load of it, so it has to follow the
/// global locale the way libSystem's does; 1 is the C locale's answer.
static MB_CUR_MAX: AtomicI32 = AtomicI32::new(1);

/// `locale_t newlocale(int mask, const char *name, locale_t base)`.
///
/// # Safety
///
/// `name` is NUL-terminated and `base` is null or a locale returned by this
/// process's locale functions.
pub unsafe extern "C" fn newlocale(
    mask: c_int,
    name: *const c_char,
    base: libc::locale_t,
) -> libc::locale_t {
    let replaces_all = mask == category::ALL_MASK;
    let Some(mask) = host_category_mask(mask) else {
        crate::errno::set(libc::EINVAL);
        return core::ptr::null_mut();
    };
    // Darwin accepts a null name as the C locale. glibc requires a string.
    let name = if name.is_null() { c"C".as_ptr() } else { name };

    // Darwin starts an omitted base from the caller's current locale, whereas
    // glibc starts one from the C locale. Make an owned copy because glibc is
    // allowed to consume the base object it receives. LC_ALL is the documented
    // exception on Darwin: it ignores `base` completely.
    let owned_base = if base.is_null() && !replaces_all {
        // SAFETY: a null query returns the current thread locale (possibly the
        // distinguished global-locale value), both accepted by `duplocale`.
        let current = unsafe { libc::uselocale(core::ptr::null_mut()) };
        // SAFETY: `current` came from `uselocale`.
        let copy = unsafe { libc::duplocale(current) };
        if copy.is_null() {
            return core::ptr::null_mut();
        }
        Some(copy)
    } else {
        None
    };
    let base = owned_base.unwrap_or(base);
    // SAFETY: the caller's name and base, with a translated category mask.
    let result = unsafe { libc::newlocale(mask, name, base) };
    if result.is_null()
        && let Some(copy) = owned_base
    {
        // On failure `newlocale` leaves its base untouched.
        unsafe { libc::freelocale(copy) };
    }
    result
}

/// `locale_t duplocale(locale_t)`.
///
/// Darwin defines a null locale as the caller's current locale here; glibc
/// requires an actual locale object (or `LC_GLOBAL_LOCALE`).
pub unsafe extern "C" fn duplocale(loc: libc::locale_t) -> libc::locale_t {
    let loc = if loc.is_null() {
        // SAFETY: a null argument is a query.
        unsafe { libc::uselocale(core::ptr::null_mut()) }
    } else {
        loc
    };
    // SAFETY: the resolved current locale or the caller's locale object.
    unsafe { libc::duplocale(loc) }
}

/// `int freelocale(locale_t)`.
///
/// Darwin returns a status; POSIX/glibc's otherwise-identical function returns
/// `void`. Forwarding it leaves the guest reading an arbitrary return register.
pub unsafe extern "C" fn freelocale(loc: libc::locale_t) -> c_int {
    if loc.is_null() || loc == global_locale() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    // SAFETY: the caller's owned locale object.
    unsafe { libc::freelocale(loc) };
    0
}

/// The thread's locale, put back when this is dropped.
///
/// A guard rather than a pair of calls, because every wrapper below has an early
/// return in it and a restore that is skipped leaves the *thread* in the guest's
/// locale for good.
pub(crate) struct Switched(libc::locale_t);

impl Drop for Switched {
    fn drop(&mut self) {
        // SAFETY: `self.0` is whatever `uselocale` reported before the switch,
        // which is either a live locale or `LC_GLOBAL_LOCALE`.
        unsafe { libc::uselocale(self.0) };
    }
}

/// Make `loc` the thread's locale for as long as the guard lives.
///
/// `None` only if the host's C locale could not be constructed.
pub(crate) fn switch(loc: libc::locale_t) -> Option<Switched> {
    let loc = if loc.is_null() { c_locale()? } else { loc };
    // SAFETY: `loc` is a `locale_t` this process obtained - a guest's came from
    // `newlocale`, which is passed through. A null result means the switch did
    // not happen, and the guard then restores a null, which `uselocale` reads as
    // "the global locale" - the state a thread that never switched is in.
    Some(Switched(unsafe { libc::uselocale(loc) }))
}

/// A host object representing Darwin's `LC_C_LOCALE` null sentinel when it is
/// used as the locale argument of a convenience function.
fn c_locale() -> Option<libc::locale_t> {
    static C: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let at = *C.get_or_init(|| {
        // SAFETY: the C locale always exists and a null base is valid to glibc.
        unsafe {
            libc::newlocale(
                crate::locale::all_mask(),
                c"C".as_ptr(),
                core::ptr::null_mut(),
            ) as usize
        }
    });
    (at != 0).then_some(at as libc::locale_t)
}

/// The plain functions each wrapper reaches, declared here because the `libc`
/// crate does not carry all of them and half a family from two sources is how a
/// prototype comes to disagree with its definition.
///
/// `wchar_t` is `int` and `wint_t` is `unsigned int` on every host this builds
/// for; `mbstate_t` is opaque - see the module note on why it is not converted.
mod plain {
    use super::{Mbstate, c_char, c_int, c_uint};

    unsafe extern "C" {
        pub fn mbtowc(pwc: *mut c_int, s: *const c_char, n: usize) -> c_int;
        pub fn btowc(c: c_int) -> c_uint;
        pub fn wctob(c: c_uint) -> c_int;
        pub fn mbrlen(s: *const c_char, n: usize, ps: Mbstate) -> usize;
        pub fn mbrtowc(pwc: *mut c_int, s: *const c_char, n: usize, ps: Mbstate) -> usize;
        pub fn mbsrtowcs(
            dst: *mut c_int,
            src: *mut *const c_char,
            len: usize,
            ps: Mbstate,
        ) -> usize;
        pub fn mbsnrtowcs(
            dst: *mut c_int,
            src: *mut *const c_char,
            nms: usize,
            len: usize,
            ps: Mbstate,
        ) -> usize;
        pub fn wcrtomb(s: *mut c_char, wc: c_int, ps: Mbstate) -> usize;
        pub fn wcsnrtombs(
            dst: *mut c_char,
            src: *mut *const c_int,
            nwc: usize,
            len: usize,
            ps: Mbstate,
        ) -> usize;
        pub fn wcwidth(wc: c_int) -> c_int;
        pub fn tolower(c: c_int) -> c_int;
        pub fn toupper(c: c_int) -> c_int;
        pub fn localeconv() -> *mut c_void;
        pub fn isalpha(c: c_int) -> c_int;
        pub fn iscntrl(c: c_int) -> c_int;
        pub fn isdigit(c: c_int) -> c_int;
        pub fn isgraph(c: c_int) -> c_int;
        pub fn islower(c: c_int) -> c_int;
        pub fn ispunct(c: c_int) -> c_int;
        pub fn isspace(c: c_int) -> c_int;
        pub fn isupper(c: c_int) -> c_int;
        pub fn isxdigit(c: c_int) -> c_int;
        pub fn isblank(c: c_int) -> c_int;
        pub fn isprint(c: c_int) -> c_int;
        pub fn iswalpha(c: c_uint) -> c_int;
        pub fn iswcntrl(c: c_uint) -> c_int;
        pub fn iswdigit(c: c_uint) -> c_int;
        pub fn iswgraph(c: c_uint) -> c_int;
        pub fn iswlower(c: c_uint) -> c_int;
        pub fn iswpunct(c: c_uint) -> c_int;
        pub fn iswspace(c: c_uint) -> c_int;
        pub fn iswupper(c: c_uint) -> c_int;
        pub fn iswxdigit(c: c_uint) -> c_int;
        pub fn iswblank(c: c_uint) -> c_int;
        pub fn iswprint(c: c_uint) -> c_int;
        pub fn towlower(c: c_uint) -> c_uint;
        pub fn towupper(c: c_uint) -> c_uint;
        #[cfg(target_os = "linux")]
        pub fn __ctype_get_mb_cur_max() -> usize;
        // The BSDs' `MB_CUR_MAX` is a macro over this, which is the same
        // function under the name their `<stdlib.h>` gives it.
        #[cfg(not(target_os = "linux"))]
        #[link_name = "___mb_cur_max"]
        pub fn __ctype_get_mb_cur_max() -> usize;
        pub fn strtol(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> i64;
        pub fn strtoll(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> i64;
        pub fn strtoul(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> u64;
        pub fn strtoull(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> u64;
        pub fn strtod(nptr: *const c_char, endptr: *mut *mut c_char) -> f64;
        pub fn strtof(nptr: *const c_char, endptr: *mut *mut c_char) -> f32;
        pub fn strcoll(a: *const c_char, b: *const c_char) -> c_int;
        pub fn strxfrm(dst: *mut c_char, src: *const c_char, n: usize) -> usize;
        pub fn strftime(buf: *mut c_char, n: usize, fmt: *const c_char, tm: *const c_void)
        -> usize;
    }

    use super::c_void;
}

/// A `mbstate_t *`, opaque on purpose.
pub type Mbstate = *mut c_void;

/// Wrap a call in a locale switch, as one `_l` entry point.
///
/// A macro because the alternative is a dozen functions whose bodies differ only
/// in the call in the middle, which is exactly the shape where a near-copy hides
/// the wrong argument order.
macro_rules! localised {
    ($(
        $(#[$meta:meta])*
        $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty = $plain:ident;
    )*) => {$(
        $(#[$meta])*
        /// # Safety
        ///
        /// Every argument is the one the plain function takes, and `loc` is a
        /// `locale_t` this process obtained or null.
        pub unsafe extern "C" fn $name($($arg: $ty),*, loc: libc::locale_t) -> $ret {
            let _switched = switch(loc);
            // SAFETY: the caller's obligation, and the arguments are passed
            // through untouched.
            unsafe { plain::$plain($($arg),*) }
        }
    )*};
}

localised! {
    /// `int mbtowc_l(wchar_t *, const char *, size_t, locale_t)`
    mbtowc_l(pwc: *mut c_int, s: *const c_char, n: usize) -> c_int = mbtowc;
    /// `wint_t btowc_l(int, locale_t)`
    btowc_l(c: c_int) -> c_uint = btowc;
    /// `int wctob_l(wint_t, locale_t)`
    wctob_l(c: c_uint) -> c_int = wctob;
    /// `size_t mbrlen_l(const char *, size_t, mbstate_t *, locale_t)`
    mbrlen_l(s: *const c_char, n: usize, ps: Mbstate) -> usize = mbrlen;
    /// `size_t mbrtowc_l(wchar_t *, const char *, size_t, mbstate_t *, locale_t)`
    mbrtowc_l(pwc: *mut c_int, s: *const c_char, n: usize, ps: Mbstate) -> usize = mbrtowc;
    /// `size_t mbsrtowcs_l(wchar_t *, const char **, size_t, mbstate_t *, locale_t)`
    mbsrtowcs_l(
        dst: *mut c_int,
        src: *mut *const c_char,
        len: usize,
        ps: Mbstate
    ) -> usize = mbsrtowcs;
    /// `size_t mbsnrtowcs_l(wchar_t *, const char **, size_t, size_t, mbstate_t *, locale_t)`
    mbsnrtowcs_l(
        dst: *mut c_int,
        src: *mut *const c_char,
        nms: usize,
        len: usize,
        ps: Mbstate
    ) -> usize = mbsnrtowcs;
    /// `size_t wcrtomb_l(char *, wchar_t, mbstate_t *, locale_t)`
    wcrtomb_l(s: *mut c_char, wc: c_int, ps: Mbstate) -> usize = wcrtomb;
    /// `size_t wcsnrtombs_l(char *, const wchar_t **, size_t, size_t, mbstate_t *, locale_t)`
    wcsnrtombs_l(
        dst: *mut c_char,
        src: *mut *const c_int,
        nwc: usize,
        len: usize,
        ps: Mbstate
    ) -> usize = wcsnrtombs;
    /// `int wcwidth_l(wchar_t, locale_t)`
    wcwidth_l(wc: c_int) -> c_int = wcwidth;
    /// `int tolower_l(int, locale_t)`, which Darwin's `<ctype.h>` also inlines as
    /// `__tolower_l`.
    tolower_l(c: c_int) -> c_int = tolower;
    /// `int toupper_l(int, locale_t)`, likewise.
    toupper_l(c: c_int) -> c_int = toupper;

    // The number and string family. This host *has* all of these under their own
    // names, and that is the trap: forwarding them looks free and is not, because
    // the `locale_t` goes with them. `libc++` on Darwin spells the C locale as a
    // **null** `locale_t`, which Darwin's manual pages document as "the current
    // locale" and glibc's implementations dereference - so a guest parsing a
    // number through `std::num_get` faulted inside `____strtoull_l_internal`, with
    // nothing about it to say the locale was the problem.

    /// `long strtol_l(const char *, char **, int, locale_t)`
    strtol_l(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> i64 = strtol;
    /// `long long strtoll_l(const char *, char **, int, locale_t)`
    strtoll_l(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> i64 = strtoll;
    /// `unsigned long strtoul_l(const char *, char **, int, locale_t)`
    strtoul_l(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> u64 = strtoul;
    /// `unsigned long long strtoull_l(const char *, char **, int, locale_t)`
    strtoull_l(nptr: *const c_char, endptr: *mut *mut c_char, base: c_int) -> u64 = strtoull;
    /// `double strtod_l(const char *, char **, locale_t)`
    strtod_l(nptr: *const c_char, endptr: *mut *mut c_char) -> f64 = strtod;
    /// `float strtof_l(const char *, char **, locale_t)`
    strtof_l(nptr: *const c_char, endptr: *mut *mut c_char) -> f32 = strtof;
    /// `int strcoll_l(const char *, const char *, locale_t)`
    strcoll_l(a: *const c_char, b: *const c_char) -> c_int = strcoll;
    /// `size_t strxfrm_l(char *, const char *, size_t, locale_t)`
    strxfrm_l(dst: *mut c_char, src: *const c_char, n: usize) -> usize = strxfrm;
    /// `size_t strftime_l(char *, size_t, const char *, const struct tm *, locale_t)`
    ///
    /// `struct tm` agrees on both platforms, so only the locale needs bridging.
    strftime_l(
        buf: *mut c_char,
        n: usize,
        fmt: *const c_char,
        tm: *const c_void
    ) -> usize = strftime;
}

/// The `_CTYPE_*` bits Darwin's `<ctype.h>` masks a rune type with.
///
/// They are ABI: a guest compiled against that header has the constants baked
/// into its own code and hands one to `__maskrune`, so a runtime that numbered
/// them differently would answer a question nobody asked.
mod ctype {
    pub const ALPHA: u32 = 0x0000_0100;
    pub const CONTROL: u32 = 0x0000_0200;
    pub const DIGIT: u32 = 0x0000_0400;
    pub const GRAPH: u32 = 0x0000_0800;
    pub const LOWER: u32 = 0x0000_1000;
    pub const PUNCT: u32 = 0x0000_2000;
    pub const SPACE: u32 = 0x0000_4000;
    pub const UPPER: u32 = 0x0000_8000;
    pub const XDIGIT: u32 = 0x0001_0000;
    pub const BLANK: u32 = 0x0002_0000;
    pub const PRINT: u32 = 0x0004_0000;
}

/// The rune type word for `c`, assembled from the host's `<ctype.h>`.
///
/// Assembled rather than read out of a table, because the table is Darwin's
/// `_DefaultRuneLocale` and this host has no such object - [`default_rune_locale`]
/// builds one from this. Every bit below is a question the host answers directly;
/// the ones it cannot - `_CTYPE_I`, `_CTYPE_T`, `_CTYPE_Q`, and the
/// character-width field - are left clear, which is what they are for every
/// character in a C or UTF-8 locale anyway.
fn runetype(c: c_int) -> u32 {
    if c < 0 {
        return 0;
    }
    let mut kinds = 0;
    // The byte ctype functions are defined only for `unsigned char` and EOF.
    // Darwin's rune call-outs receive full Unicode scalar values, so using
    // `isalpha` for (say) U+10FFFF indexes outside glibc's ctype table. Use the
    // wide family outside the byte range instead.
    unsafe {
        if c <= u8::MAX as c_int {
            for (test, bit) in [
                (
                    plain::isalpha as unsafe extern "C" fn(c_int) -> c_int,
                    ctype::ALPHA,
                ),
                (plain::iscntrl, ctype::CONTROL),
                (plain::isdigit, ctype::DIGIT),
                (plain::isgraph, ctype::GRAPH),
                (plain::islower, ctype::LOWER),
                (plain::ispunct, ctype::PUNCT),
                (plain::isspace, ctype::SPACE),
                (plain::isupper, ctype::UPPER),
                (plain::isxdigit, ctype::XDIGIT),
                (plain::isblank, ctype::BLANK),
                (plain::isprint, ctype::PRINT),
            ] {
                if test(c) != 0 {
                    kinds |= bit;
                }
            }
        } else {
            let c = c as c_uint;
            for (test, bit) in [
                (
                    plain::iswalpha as unsafe extern "C" fn(c_uint) -> c_int,
                    ctype::ALPHA,
                ),
                (plain::iswcntrl, ctype::CONTROL),
                (plain::iswdigit, ctype::DIGIT),
                (plain::iswgraph, ctype::GRAPH),
                (plain::iswlower, ctype::LOWER),
                (plain::iswpunct, ctype::PUNCT),
                (plain::iswspace, ctype::SPACE),
                (plain::iswupper, ctype::UPPER),
                (plain::iswxdigit, ctype::XDIGIT),
                (plain::iswblank, ctype::BLANK),
                (plain::iswprint, ctype::PRINT),
            ] {
                if test(c) != 0 {
                    kinds |= bit;
                }
            }
        }
    }
    kinds
}

/// How many characters Darwin's `_RuneLocale` caches, from `<runetype.h>`'s
/// `_CACHED_RUNES`. The inline `__maskrune` indexes the table directly for a `c`
/// in `0.._CACHED_RUNES` and calls [`maskrune`] for anything outside it.
const CACHED_RUNES: usize = 256;

/// Darwin's `_RuneRange`, left empty here: a character above the cached range
/// reaches [`maskrune`] instead of a range table, so there is nothing to fill in.
#[repr(C)]
struct RuneRange {
    nranges: c_int,
    ranges: *const c_void,
}

/// Darwin's `_RuneLocale`, laid out as `<runetype.h>` declares it.
///
/// The layout is ABI rather than an implementation detail. A guest compiled
/// against Darwin's `<ctype.h>` does not *call* `isspace` - the header inlines it
/// to a load from `__runetype[c]` at a byte offset baked into the guest's own
/// code - so a field out of place here is a wrong answer, not a link error.
#[repr(C)]
struct RuneLocale {
    magic: [c_char; 8],
    encoding: [c_char; 32],
    sgetrune: *const c_void,
    sputrune: *const c_void,
    invalid_rune: i32,
    runetype: [u32; CACHED_RUNES],
    maplower: [i32; CACHED_RUNES],
    mapupper: [i32; CACHED_RUNES],
    runetype_ext: RuneRange,
    maplower_ext: RuneRange,
    mapupper_ext: RuneRange,
    variable: *const c_void,
    variable_len: c_int,
    ncharclasses: c_int,
    charclasses: *const c_void,
}

/// The address of this process's stand-in for `_DefaultRuneLocale`.
///
/// Built once and leaked, because the guest holds the address for the life of the
/// process: its `__runetype` loads are compiled against this object, not against
/// a call that could be re-dispatched.
///
/// The classifications come from [`runetype`] and the case maps from the host's
/// own `tolower`/`toupper`, so the inlined path and the [`maskrune`] call-out
/// answer the same question the same way. Both are read in the C locale, which is
/// the one `_DefaultRuneLocale` describes on Darwin too - `setlocale` there
/// replaces `_CurrentRuneLocale` and leaves this table alone.
fn default_rune_locale() -> u64 {
    /// `_RUNE_MAGIC_1`, and the encoding name Darwin's own default table carries.
    const MAGIC: &[u8; 8] = b"RuneMagi";

    static AT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *AT.get_or_init(|| {
        // `_DefaultRuneLocale` is the C table regardless of whichever locale
        // the thread that first constructs the provider happens to use.
        let _c = switch(core::ptr::null_mut());
        let mut locale = Box::new(RuneLocale {
            magic: [0; 8],
            encoding: [0; 32],
            sgetrune: core::ptr::null(),
            sputrune: core::ptr::null(),
            // What Darwin's default table reports for a byte that decodes to no
            // character.
            invalid_rune: 0xfffd,
            runetype: [0; CACHED_RUNES],
            maplower: [0; CACHED_RUNES],
            mapupper: [0; CACHED_RUNES],
            runetype_ext: RuneRange {
                nranges: 0,
                ranges: core::ptr::null(),
            },
            maplower_ext: RuneRange {
                nranges: 0,
                ranges: core::ptr::null(),
            },
            mapupper_ext: RuneRange {
                nranges: 0,
                ranges: core::ptr::null(),
            },
            variable: core::ptr::null(),
            variable_len: 0,
            ncharclasses: 0,
            charclasses: core::ptr::null(),
        });
        for (slot, byte) in locale.magic.iter_mut().zip(MAGIC) {
            *slot = *byte as c_char;
        }
        for (slot, byte) in locale.encoding.iter_mut().zip(b"NONE") {
            *slot = *byte as c_char;
        }
        for c in 0..CACHED_RUNES {
            locale.runetype[c] = runetype(c as c_int);
            // SAFETY: both take an `int` in the cached range and touch no memory.
            unsafe {
                locale.maplower[c] = plain::tolower(c as c_int);
                locale.mapupper[c] = plain::toupper(c as c_int);
            }
        }
        Box::leak(locale) as *const RuneLocale as usize as u64
    })
}

/// `int __maskrune(__darwin_ct_rune_t, unsigned long)`
///
/// Darwin's `<ctype.h>` inlines `isalpha(c)` to a lookup in
/// `_DefaultRuneLocale` for an ASCII `c` and a call to this for anything else,
/// so a guest reaches it for every non-ASCII character it classifies.
pub unsafe extern "C" fn maskrune(c: c_int, mask: u64) -> c_int {
    (runetype(c) & mask as u32) as c_int
}

/// `wctype_t wctype(const char *property)`
///
/// On Darwin a `wctype_t` *is* a rune-type mask: `<wctype.h>` inlines
/// `iswctype(wc, t)` to `__maskrune(wc, t)`, so the answer has to be the
/// `_CTYPE_*` bits for the class and not the host's opaque handle. Apple's
/// table, the BSD extensions included; an unknown name is zero.
///
/// # Safety
///
/// `property` is NUL-terminated.
pub unsafe extern "C" fn wctype(property: *const c_char) -> u64 {
    const IDEOGRAM: u32 = 0x0008_0000;
    const SPECIAL: u32 = 0x0010_0000;
    const PHONOGRAM: u32 = 0x0020_0000;
    if property.is_null() {
        return 0;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(property) }.to_bytes();
    let mask = match name {
        b"alnum" => ctype::ALPHA | ctype::DIGIT,
        b"alpha" => ctype::ALPHA,
        b"blank" => ctype::BLANK,
        b"cntrl" => ctype::CONTROL,
        b"digit" => ctype::DIGIT,
        b"graph" => ctype::GRAPH,
        b"lower" => ctype::LOWER,
        b"print" => ctype::PRINT,
        b"punct" => ctype::PUNCT,
        b"space" => ctype::SPACE,
        b"upper" => ctype::UPPER,
        b"xdigit" => ctype::XDIGIT,
        b"ideogram" => IDEOGRAM,
        b"special" => SPECIAL,
        b"phonogram" => PHONOGRAM,
        b"rune" => 0xffff_ff00,
        _ => 0,
    };
    mask as u64
}

/// `int iswctype(wint_t wc, wctype_t charclass)`, the out-of-line form of
/// what the header inlines.
pub unsafe extern "C" fn iswctype(wc: c_uint, charclass: u64) -> c_int {
    // SAFETY: touches no memory.
    (unsafe { maskrune(wc as c_int, charclass) } != 0) as c_int
}

/// `int __maskrune_l(__darwin_ct_rune_t, unsigned long, locale_t)`
///
/// # Safety
///
/// `loc` is a `locale_t` this process obtained, or null.
pub unsafe extern "C" fn maskrune_l(c: c_int, mask: u64, loc: libc::locale_t) -> c_int {
    let _switched = switch(loc);
    // SAFETY: touches no memory.
    unsafe { maskrune(c, mask) }
}

/// `int __tolower(__darwin_ct_rune_t)` and `int __toupper(...)`, which Darwin's
/// `<ctype.h>` inlines to for a character outside the cached ASCII range.
pub unsafe extern "C" fn tolower_(c: c_int) -> c_int {
    if c < 0 {
        return c;
    }
    // SAFETY: the byte function is used only in its defined domain; the wide
    // function accepts every nonnegative value representable by Darwin's rune.
    unsafe {
        if c <= u8::MAX as c_int {
            plain::tolower(c)
        } else {
            plain::towlower(c as c_uint) as c_int
        }
    }
}

pub unsafe extern "C" fn toupper_(c: c_int) -> c_int {
    if c < 0 {
        return c;
    }
    unsafe {
        if c <= u8::MAX as c_int {
            plain::toupper(c)
        } else {
            plain::towupper(c as c_uint) as c_int
        }
    }
}

/// `struct lconv *localeconv_l(locale_t)`
///
/// The pointer is libc's own and is valid until the thread's locale changes
/// again - which the guard below does before returning. That is the same
/// lifetime Darwin's gives, and the caller of a `localeconv` has always had to
/// copy what it wants out before doing anything else.
///
/// # Safety
///
/// `loc` is a `locale_t` this process obtained, or null.
pub unsafe extern "C" fn localeconv_l(loc: libc::locale_t) -> *mut c_void {
    let _switched = switch(loc);
    unsafe { plain::localeconv() }
}

/// `int __mb_cur_max_l(locale_t)`: how many bytes one character can take.
///
/// `MB_CUR_MAX` is a macro over a function on both hosts - glibc's
/// `__ctype_get_mb_cur_max()` and the BSDs' `___mb_cur_max()` - which is the
/// function this reaches for.
///
/// # Safety
///
/// As [`localeconv_l`].
pub unsafe extern "C" fn mb_cur_max_l(loc: libc::locale_t) -> c_int {
    let _switched = switch(loc);
    unsafe { plain::__ctype_get_mb_cur_max() as c_int }
}

/// `int ___mb_cur_max(void)`, which Darwin's `MB_CUR_MAX` expands to.
///
/// # Safety
///
/// Takes nothing; `unsafe` only to match the table's shape.
pub unsafe extern "C" fn mb_cur_max() -> c_int {
    unsafe { plain::__ctype_get_mb_cur_max() as c_int }
}

/// `void __collate_lookup_l(const wchar_t *, int *len, int *prim, int *sec,
/// locale_t)`: the collation weights of the element starting at `t`.
///
/// A private libSystem entry that Apple's bash calls for `[[=c=]]`, comparing
/// only the primary weights of two characters.
///
/// FIXME (ACCURACY): glibc publishes no per-character weights, so every
/// character is its own primary weight - what Darwin answers in the C locale.
/// In a real locale Darwin would also put `e` and e-acute in one class.
///
/// # Safety
///
/// `t` points at a readable `wchar_t` and the three outputs are writable.
pub unsafe extern "C" fn collate_lookup_l(
    t: *const u32,
    len: *mut c_int,
    prim: *mut c_int,
    sec: *mut c_int,
    _loc: libc::locale_t,
) {
    unsafe {
        *len = 1;
        *prim = *t as c_int;
        *sec = 0;
    }
}

/// `void __collate_lookup(const unsigned char *, int *len, int *prim, int *sec)`:
/// [`collate_lookup_l`] for a multibyte string in the current locale, with
/// `len` in bytes.
///
/// # Safety
///
/// `t` is NUL-terminated and the three outputs are writable.
pub unsafe extern "C" fn collate_lookup(
    t: *const c_char,
    len: *mut c_int,
    prim: *mut c_int,
    sec: *mut c_int,
) {
    let mut wc: c_int = 0;
    let mut state = [0u8; 128];
    // SAFETY: at most MB_LEN_MAX bytes of a NUL-terminated string, which mbrtowc
    // stops reading at the NUL; `state` is a zeroed conversion state.
    let n = unsafe { plain::mbrtowc(&mut wc, t, 16, state.as_mut_ptr().cast()) };
    unsafe {
        *sec = 0;
        match n {
            // An invalid or truncated sequence: Darwin reports no element.
            n if n >= usize::MAX - 1 => {
                *len = 0;
                *prim = 0;
            }
            // The NUL itself is one byte long.
            0 => {
                *len = 1;
                *prim = 0;
            }
            n => {
                *len = n as c_int;
                *prim = wc;
            }
        }
    }
}

// Two pairs are the same function under two names: Darwin's `<ctype.h>` inlines
// `tolower_l` to `__tolower_l`, and a guest compiled against that header imports
// the underscored one.
exports::exports! {
    functions {
        "setlocale" => setlocale,
        "newlocale" => newlocale,
        "duplocale" => duplocale,
        "freelocale" => freelocale,
        "mbtowc_l" => mbtowc_l,
        "btowc_l" => btowc_l,
        "wctob_l" => wctob_l,
        "mbrlen_l" => mbrlen_l,
        "mbrtowc_l" => mbrtowc_l,
        "mbsrtowcs_l" => mbsrtowcs_l,
        "mbsnrtowcs_l" => mbsnrtowcs_l,
        "wcrtomb_l" => wcrtomb_l,
        "wcsnrtombs_l" => wcsnrtombs_l,
        "wcwidth_l" => wcwidth_l,
        "localeconv_l" => localeconv_l,
        "__mb_cur_max_l" | "___mb_cur_max_l" => mb_cur_max_l,
        "___mb_cur_max" => mb_cur_max,
        "__collate_lookup" => collate_lookup,
        "__collate_lookup_l" => collate_lookup_l,
        "tolower_l" | "__tolower_l" | "___tolower_l" => tolower_l,
        "toupper_l" | "__toupper_l" | "___toupper_l" => toupper_l,
        "__maskrune" | "___maskrune" => maskrune,
        "wctype" => wctype,
        "iswctype" => iswctype,
        "__maskrune_l" | "___maskrune_l" => maskrune_l,
        "strtol_l" => strtol_l,
        "strtoll_l" => strtoll_l,
        "strtoul_l" => strtoul_l,
        "strtoull_l" => strtoull_l,
        "strtod_l" => strtod_l,
        "strtof_l" => strtof_l,
        "strcoll_l" => strcoll_l,
        "strxfrm_l" => strxfrm_l,
        "strftime_l" => strftime_l,
        "__tolower" | "___tolower" => tolower_,
        "__toupper" | "___toupper" => toupper_,
    }
    data {
        // The Mach-O underscore, put back: the guest imports
        // `__DefaultRuneLocale`.
        "__DefaultRuneLocale" => default_rune_locale(),
    }
    constants {
        "__mb_cur_max" => MB_CUR_MAX.as_ptr() as usize as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The global locale and the guest's environment are process-wide, so the
    /// tests that change them take turns.
    static GLOBAL_LOCALE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn turn() -> std::sync::MutexGuard<'static, ()> {
        GLOBAL_LOCALE.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sets guest variables for one test and puts the old values back, along
    /// with the host's global locale. Made while holding [`turn`].
    struct GuestLocale {
        saved: Vec<(&'static CStr, Option<std::ffi::CString>)>,
    }

    impl GuestLocale {
        fn new(vars: &[(&'static CStr, Option<&CStr>)]) -> Self {
            let names = [
                c"LC_ALL",
                c"LANG",
                c"LC_COLLATE",
                c"LC_CTYPE",
                c"LC_NUMERIC",
            ];
            // SAFETY: NUL-terminated names and values.
            let saved = names
                .iter()
                .map(|&name| unsafe {
                    let value = crate::env::getenv(name.as_ptr());
                    let value = (!value.is_null()).then(|| CStr::from_ptr(value).to_owned());
                    crate::env::unsetenv(name.as_ptr());
                    (name, value)
                })
                .collect();
            for (name, value) in vars {
                if let Some(value) = value {
                    // SAFETY: as above.
                    unsafe { crate::env::setenv(name.as_ptr(), value.as_ptr(), 1) };
                }
            }
            GuestLocale { saved }
        }
    }

    impl Drop for GuestLocale {
        fn drop(&mut self) {
            // SAFETY: NUL-terminated names and values.
            unsafe {
                for (name, value) in &self.saved {
                    match value {
                        Some(value) => crate::env::setenv(name.as_ptr(), value.as_ptr(), 1),
                        None => crate::env::unsetenv(name.as_ptr()),
                    };
                }
                libc::setlocale(libc::LC_ALL, c"C".as_ptr());
            }
            MB_CUR_MAX.store(1, Ordering::Relaxed);
        }
    }

    /// A UTF-8 locale this host has, if any: hosts differ in which they ship.
    fn utf8_locale() -> Option<&'static CStr> {
        [c"C.UTF-8", c"en_US.UTF-8"].into_iter().find(|name| {
            // SAFETY: NUL-terminated names; the global locale is the test's.
            unsafe {
                let found = !libc::setlocale(libc::LC_CTYPE, name.as_ptr()).is_null();
                libc::setlocale(libc::LC_CTYPE, c"C".as_ptr());
                found
            }
        })
    }

    #[test]
    fn an_empty_name_without_guest_variables_is_c() {
        let _turn = turn();
        let _guest = GuestLocale::new(&[]);
        // SAFETY: NUL-terminated names.
        unsafe {
            let set = setlocale(category::NUMERIC, c"".as_ptr());
            assert!(!set.is_null());
            assert_eq!(CStr::from_ptr(set), c"C");
        }
    }

    #[test]
    fn an_empty_name_reads_the_guests_environment() {
        let _turn = turn();
        let Some(utf8) = utf8_locale() else {
            eprintln!("skipping: this host has no UTF-8 locale");
            return;
        };
        let _guest = GuestLocale::new(&[(c"LC_NUMERIC", Some(utf8))]);
        // SAFETY: NUL-terminated names.
        unsafe {
            let set = setlocale(category::NUMERIC, c"".as_ptr());
            assert!(!set.is_null());
            assert_eq!(CStr::from_ptr(set), utf8);
        }
    }

    /// One refused category leaves every other one, and `MB_CUR_MAX`, as it
    /// was.
    #[test]
    fn a_refused_category_changes_nothing() {
        let _turn = turn();
        let Some(utf8) = utf8_locale() else {
            eprintln!("skipping: this host has no UTF-8 locale");
            return;
        };
        let _guest = GuestLocale::new(&[
            (c"LC_CTYPE", Some(utf8)),
            (c"LC_NUMERIC", Some(c"xx_NOWHERE.none")),
        ]);
        // SAFETY: NUL-terminated names.
        unsafe {
            libc::setlocale(libc::LC_ALL, c"C".as_ptr());
            assert!(setlocale(category::ALL, c"".as_ptr()).is_null());
            let ctype = libc::setlocale(libc::LC_CTYPE, core::ptr::null());
            assert_eq!(CStr::from_ptr(ctype), c"C");
        }
        assert_eq!(MB_CUR_MAX.load(Ordering::Relaxed), 1);
    }

    /// The switch is undone even when the wrapped call fails, which is the whole
    /// reason it is a guard: a thread left in the guest's locale formats every
    /// number this runtime prints afterwards.
    #[test]
    fn a_switch_is_undone() {
        // SAFETY: a locale this test makes and frees, and `uselocale` with a
        // null argument only reports.
        unsafe {
            let before = libc::uselocale(std::ptr::null_mut());
            let c = libc::newlocale(
                crate::locale::all_mask(),
                c"C".as_ptr(),
                std::ptr::null_mut(),
            );
            if c.is_null() {
                return;
            }
            {
                let _switched = switch(c);
                assert_eq!(libc::uselocale(std::ptr::null_mut()), c);
            }
            assert_eq!(libc::uselocale(std::ptr::null_mut()), before);
            libc::freelocale(c);
        }
    }

    #[test]
    fn darwin_locale_categories_are_translated() {
        assert_eq!(host_category(category::ALL), Some(libc::LC_ALL));
        assert_eq!(host_category(category::NUMERIC), Some(libc::LC_NUMERIC));
        assert_ne!(category::NUMERIC, libc::LC_NUMERIC);
        assert_eq!(host_category(99), None);

        assert_eq!(
            host_category_mask(category::NUMERIC_MASK),
            Some(libc::LC_NUMERIC_MASK)
        );
        assert_eq!(
            host_category_mask(category::COLLATE_MASK | category::TIME_MASK),
            Some(libc::LC_COLLATE_MASK | libc::LC_TIME_MASK)
        );
        assert_eq!(host_category_mask(1 << 20), None);
    }

    #[test]
    fn a_darwin_newlocale_mask_creates_the_requested_locale() {
        // SAFETY: a static locale name and a null base.
        let loc =
            unsafe { newlocale(category::NUMERIC_MASK, c"C".as_ptr(), core::ptr::null_mut()) };
        assert!(!loc.is_null());
        // SAFETY: `newlocale` returned this object and nothing else uses it.
        unsafe { libc::freelocale(loc) };

        // Darwin uniquely defines a null locale name as the C locale.
        // SAFETY: an all-category mask ignores the null base.
        let loc =
            unsafe { newlocale(category::ALL_MASK, core::ptr::null(), core::ptr::null_mut()) };
        assert!(!loc.is_null());
        unsafe { libc::freelocale(loc) };
    }

    #[test]
    fn a_null_convenience_locale_selects_c() {
        let _switched = switch(std::ptr::null_mut()).expect("the C locale exists");
        // SAFETY: a null argument only reports the current locale.
        let current = unsafe { libc::uselocale(std::ptr::null_mut()) };
        assert_eq!(current, c_locale().expect("the C locale exists"));
    }

    #[test]
    fn null_duplocale_and_darwin_freelocale_semantics_are_preserved() {
        // SAFETY: null asks for a copy of the current locale.
        let copy = unsafe { duplocale(core::ptr::null_mut()) };
        assert!(!copy.is_null());
        // SAFETY: `copy` is owned by this test.
        assert_eq!(unsafe { freelocale(copy) }, 0);
        // Null is Darwin's C-locale sentinel, not an allocated object.
        // SAFETY: testing the shim's checked error path.
        assert_eq!(unsafe { freelocale(core::ptr::null_mut()) }, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
        crate::errno::set(0);
    }

    /// And a null locale reaches the number parsers, which is the case that
    /// mattered: `libc++` spells the C locale as a null `locale_t`, and this
    /// host's own `strtoull_l` dereferences one.
    #[test]
    fn a_null_locale_parses_a_number_instead_of_faulting() {
        let text = c"0x2a tail";
        let mut end: *mut c_char = std::ptr::null_mut();
        // SAFETY: a NUL-terminated string, a live `char *` slot, and the null
        // locale a `libc++` caller passes.
        let value = unsafe { strtoull_l(text.as_ptr(), &mut end, 16, std::ptr::null_mut()) };
        assert_eq!(value, 42);
        // SAFETY: `end` points inside `text`, which outlives this.
        assert_eq!(
            unsafe { std::ffi::CStr::from_ptr(end) }.to_bytes(),
            b" tail"
        );
    }

    /// The bits are the header's, and a character has the ones it has.
    #[test]
    fn a_rune_type_says_what_a_character_is() {
        let kinds = runetype('A' as c_int);
        assert_ne!(kinds & ctype::ALPHA, 0);
        assert_ne!(kinds & ctype::UPPER, 0);
        assert_ne!(kinds & ctype::XDIGIT, 0);
        assert_eq!(kinds & ctype::DIGIT, 0);
        assert_eq!(kinds & ctype::SPACE, 0);

        // SAFETY: touches no memory.
        unsafe {
            assert_ne!(maskrune('7' as c_int, ctype::DIGIT as u64), 0);
            assert_eq!(maskrune('7' as c_int, ctype::ALPHA as u64), 0);
        }
    }

    /// Darwin calls this entry for runes outside the cached byte table. Byte
    /// ctype functions have a much smaller domain and glibc can fault when
    /// handed this perfectly valid rune.
    #[test]
    fn a_unicode_rune_does_not_enter_the_byte_ctype_table() {
        // SAFETY: neither function touches caller memory.
        unsafe {
            assert_eq!(maskrune(0x10ffff, ctype::DIGIT as u64), 0);
            assert_eq!(tolower_(0x10ffff), 0x10ffff);
            assert_eq!(toupper_(0x10ffff), 0x10ffff);
            assert_eq!(maskrune(-2, u32::MAX as u64), 0);
            assert_eq!(tolower_(-2), -2);
        }
    }

    /// The offsets a guest's inlined `<ctype.h>` has baked into its own code.
    /// Measured from Darwin's `<runetype.h>` on a 64-bit target, and checked here
    /// because nothing else in this process ever reads these fields by name: if
    /// `__runetype` moved, every classification would come back from the wrong
    /// bytes and still look like a plausible answer.
    #[test]
    fn the_rune_locale_is_laid_out_the_way_the_header_declares_it() {
        let at = default_rune_locale();
        let offset = |field: *const u8| field as usize as u64 - at;
        // SAFETY: `at` is the leaked object, live for the process.
        let locale = unsafe { &*(at as usize as *const RuneLocale) };
        assert_eq!(offset(locale.magic.as_ptr().cast()), 0);
        assert_eq!(offset(locale.encoding.as_ptr().cast()), 8);
        // 0x3c, which is the displacement in the guest's own `movl
        // 0x3c(%r13,%rax,4), %eax`.
        assert_eq!(offset(locale.runetype.as_ptr().cast()), 60);
        assert_eq!(offset(locale.maplower.as_ptr().cast()), 60 + 1024);
        assert_eq!(offset(locale.mapupper.as_ptr().cast()), 60 + 2048);
        assert_eq!(size_of::<RuneLocale>(), 3208);
    }

    /// The table a guest indexes and the function it calls out to have to agree:
    /// the header picks between them on the character's value alone, so a
    /// disagreement is a program that classifies the same byte two ways.
    #[test]
    fn the_cached_table_answers_as_maskrune_does() {
        // SAFETY: the leaked object, live for the process.
        let locale = unsafe { &*(default_rune_locale() as usize as *const RuneLocale) };
        for c in 0..CACHED_RUNES {
            // SAFETY: touches no memory.
            let called = unsafe { maskrune(c as c_int, u32::MAX as u64) };
            assert_eq!(locale.runetype[c], called as u32, "runetype[{c}]");
        }
        assert_ne!(locale.runetype[' ' as usize] & ctype::SPACE, 0);
        assert_ne!(locale.runetype['\t' as usize] & ctype::SPACE, 0);
        assert_eq!(locale.runetype['x' as usize] & ctype::SPACE, 0);
        assert_eq!(locale.maplower['A' as usize], 'a' as i32);
        assert_eq!(locale.mapupper['a' as usize], 'A' as i32);
        assert_eq!(locale.maplower['5' as usize], '5' as i32);
    }

    /// One object, at one address: the guest binds the symbol once and keeps the
    /// pointer, so a second call that built a second table would leave half the
    /// program reading a table nothing else updates.
    #[test]
    fn the_rune_locale_is_built_once() {
        assert_eq!(default_rune_locale(), default_rune_locale());
    }

    #[test]
    fn one_character_converts() {
        let mut wc: c_int = 0;
        // SAFETY: a writable wide character and a NUL-terminated byte.
        let used = unsafe { mbtowc_l(&mut wc, c"a".as_ptr(), 1, std::ptr::null_mut()) };
        assert_eq!((used, wc), (1, 'a' as c_int));
    }
}
