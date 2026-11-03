#![cfg(not(target_os = "macos"))]

use std::ffi::{CStr, c_char, c_int};

/// Darwin's items, in Darwin's order, as this host's.
///
/// `None` for `D_MD_ORDER`, which only Darwin has and [`nl_langinfo`] derives.
fn host_item(item: c_int) -> Option<libc::nl_item> {
    use libc::*;
    const DAYS: [nl_item; 7] = [DAY_1, DAY_2, DAY_3, DAY_4, DAY_5, DAY_6, DAY_7];
    const ABDAYS: [nl_item; 7] = [
        ABDAY_1, ABDAY_2, ABDAY_3, ABDAY_4, ABDAY_5, ABDAY_6, ABDAY_7,
    ];
    const MONS: [nl_item; 12] = [
        MON_1, MON_2, MON_3, MON_4, MON_5, MON_6, MON_7, MON_8, MON_9, MON_10, MON_11, MON_12,
    ];
    const ABMONS: [nl_item; 12] = [
        ABMON_1, ABMON_2, ABMON_3, ABMON_4, ABMON_5, ABMON_6, ABMON_7, ABMON_8, ABMON_9, ABMON_10,
        ABMON_11, ABMON_12,
    ];
    Some(match item {
        0 => CODESET,
        1 => D_T_FMT,
        2 => D_FMT,
        3 => T_FMT,
        4 => T_FMT_AMPM,
        5 => AM_STR,
        6 => PM_STR,
        7..=13 => DAYS[(item - 7) as usize],
        14..=20 => ABDAYS[(item - 14) as usize],
        21..=32 => MONS[(item - 21) as usize],
        33..=44 => ABMONS[(item - 33) as usize],
        45 => ERA,
        46 => ERA_D_FMT,
        47 => ERA_D_T_FMT,
        48 => ERA_T_FMT,
        49 => ALT_DIGITS,
        50 => RADIXCHAR,
        51 => THOUSEP,
        52 => YESEXPR,
        53 => NOEXPR,
        54 => YESSTR,
        55 => NOSTR,
        56 => CRNCYSTR,
        _ => return None,
    })
}

const D_FMT: c_int = 2;
const D_MD_ORDER: c_int = 57;

/// `char *nl_langinfo(nl_item item)`
///
/// # Safety
///
/// Takes a scalar; the answer is libc's storage or a literal, and a caller
/// must not write to it.
pub unsafe extern "C" fn nl_langinfo(item: c_int) -> *mut c_char {
    if item == D_MD_ORDER {
        // Month before day unless the locale's date format says otherwise,
        // which is Darwin's default and its answer for the C locale.
        // SAFETY: a valid item, and libc's own string.
        let format = unsafe { CStr::from_ptr(nl_langinfo(D_FMT)) }.to_bytes();
        let day = format.windows(2).position(|w| w == b"%d" || w == b"%e");
        let month = format.windows(2).position(|w| w == b"%m" || w == b"%b");
        let order = match (day, month) {
            (Some(d), Some(m)) if d < m => c"dm",
            _ => c"md",
        };
        return order.as_ptr().cast_mut();
    }
    let Some(host) = host_item(item) else {
        // Darwin answers an unknown item with an empty string, never null.
        return c"".as_ptr().cast_mut();
    };
    // SAFETY: a valid host item.
    let answer = unsafe { libc::nl_langinfo(host) };
    if host == libc::CODESET && !answer.is_null() {
        // glibc's name for ASCII is the standard's; Darwin's is the one every
        // Mac program compares against.
        // SAFETY: libc's own NUL-terminated string.
        if unsafe { CStr::from_ptr(answer) } == c"ANSI_X3.4-1968" {
            return c"US-ASCII".as_ptr().cast_mut();
        }
    }
    answer
}

/// `char *nl_langinfo_l(nl_item item, locale_t loc)`
///
/// # Safety
///
/// `loc` is a `locale_t` this process obtained, `LC_GLOBAL_LOCALE`, or null
/// for the C locale. The answer belongs to `loc` and a caller must not write
/// to it.
pub unsafe extern "C" fn nl_langinfo_l(item: c_int, loc: libc::locale_t) -> *mut c_char {
    let _switched = crate::xlocale::switch(loc);
    // SAFETY: a scalar, answered for the locale just selected.
    unsafe { nl_langinfo(item) }
}

exports::exports! {
    functions {
        "nl_langinfo" => nl_langinfo,
        "nl_langinfo_l" => nl_langinfo_l,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(item: c_int) -> String {
        // SAFETY: a scalar, answered with a NUL-terminated string.
        unsafe { CStr::from_ptr(nl_langinfo(item)) }
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn darwin_numbers_reach_the_right_host_items() {
        // The process starts in the C locale.
        assert_eq!(ask(0), "US-ASCII");
        assert_eq!(ask(7), "Sunday");
        assert_eq!(ask(14), "Sun");
        assert_eq!(ask(21), "January");
        assert_eq!(ask(44), "Dec");
        assert_eq!(ask(50), ".");
        assert_eq!(ask(D_MD_ORDER), "md");
        assert_eq!(ask(1000), "");
    }

    #[test]
    fn nl_langinfo_l_answers_for_the_named_locale() {
        let at = |item, loc| {
            // SAFETY: a scalar and a locale this test owns, or null.
            unsafe { CStr::from_ptr(nl_langinfo_l(item, loc)) }
                .to_string_lossy()
                .into_owned()
        };
        // Darwin's null `locale_t` is the C locale.
        assert_eq!(at(0, std::ptr::null_mut()), "US-ASCII");
        assert_eq!(at(D_MD_ORDER, std::ptr::null_mut()), "md");
        assert_eq!(at(21, crate::xlocale::global_locale()), "January");
    }
}
