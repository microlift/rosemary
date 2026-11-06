#![allow(non_snake_case)]

use std::ffi::c_void;

use corefoundation::base::{CFIndex, CFTypeRef};
use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

/// The shape each formatter here shares: the object, and its settings.
#[repr(C)]
struct Body<T> {
    isa: *const Class,
    contents: *mut T,
}

/// Register a class whose instances are a [`Body<T>`], with a `-dealloc` that
/// gives the contents back.
fn register<T: 'static>(name: &'static std::ffi::CStr) -> &'static Class {
    let base = corefoundation::base::abstract_class(name, None);
    corefoundation::base::note_type_class(base);
    let concrete = corefoundation::base::register(
        // `__NSCF` + the name, which is how every toll-free class in this tree
        // spells the concrete half of the pair.
        leaked_concrete_name(name),
        Some(base),
        size_of::<Body<T>>(),
        false,
    );
    unsafe extern "C" fn dealloc<T>(receiver: Id, _sel: objc::sel::Sel) {
        // SAFETY: `-dealloc` is reached with the last reference gone, and the
        // receiver is a `Body<T>` whose `contents` came from `Box::into_raw`.
        unsafe {
            let body = receiver as *mut Body<T>;
            if !(*body).contents.is_null() {
                drop(Box::from_raw((*body).contents));
                (*body).contents = std::ptr::null_mut();
            }
            objc::arc::finalize(receiver);
        }
    }
    concrete.replace_method(
        objc::sel::get("dealloc"),
        dealloc::<T> as *const c_void,
        c"v16@0:8",
    );
    concrete
}

/// `NSFoo` -> `__NSCFFoo`, as a name that lives for the process.
///
/// Leaked because [`corefoundation::base::register`] takes a name it keeps: the
/// class table holds the pointer for as long as the class, which is forever.
fn leaked_concrete_name(name: &std::ffi::CStr) -> &'static std::ffi::CStr {
    let text = format!("__NSCF{}", name.to_string_lossy().trim_start_matches("NS"));
    Box::leak(
        std::ffi::CString::new(text)
            .expect("a class name with no NUL")
            .into_boxed_c_str(),
    )
}

/// Allocate an instance of `class` holding `contents`.
fn make<T>(class: &'static Class, contents: T) -> CFTypeRef {
    // SAFETY: `Body<T>` begins with `isa` and is the layout `class` was
    // registered with.
    let body: *mut Body<T> = unsafe { corefoundation::base::allocate(class) };
    if body.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a live instance whose second word is ours to fill in.
    unsafe { (*body).contents = Box::into_raw(Box::new(contents)) };
    body as CFTypeRef
}

/// The settings of `object`, if it is an instance of `class`.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
unsafe fn contents<'a, T>(object: CFTypeRef, class: &'static Class) -> Option<&'a mut T> {
    if !unsafe { corefoundation::base::is_kind_of(object, class) } {
        return None;
    }
    // SAFETY: an instance of `class`, whose layout is `Body<T>`.
    unsafe {
        let body = object as *mut Body<T>;
        (!(*body).contents.is_null()).then(|| &mut *(*body).contents)
    }
}

//
// Number formatting
//

/// `CFNumberFormatterStyle`. Apple's values, because a caller passes one in.
mod number_style {
    pub const NONE: i32 = 0;
    pub const DECIMAL: i32 = 1;
    pub const CURRENCY: i32 = 2;
    pub const PERCENT: i32 = 3;
    pub const SCIENTIFIC: i32 = 4;
    pub const CURRENCY_ISO_CODE: i32 = 8;
    pub const CURRENCY_PLURAL: i32 = 9;
    pub const CURRENCY_ACCOUNTING: i32 = 10;
}

#[derive(Default)]
struct NumberFormat {
    style: i32,
    /// What [`cf::CFNumberFormatterSetProperty`] was told to use, which a caller
    /// formatting a price in another currency than the locale's sets.
    currency_symbol: Option<String>,
    /// What the Objective-C setters changed from the style's defaults.
    settings: Settings,
}

/// `NSNumberFormatter`'s own properties, each `None` until set so the
/// style's default shows through.
#[derive(Default)]
struct Settings {
    maximum_fraction_digits: Option<usize>,
    minimum_fraction_digits: Option<usize>,
    uses_grouping_separator: Option<bool>,
    grouping_size: Option<usize>,
    decimal_separator: Option<String>,
    grouping_separator: Option<String>,
    generates_decimal_numbers: bool,
}

impl NumberFormat {
    fn is_currency(&self) -> bool {
        use number_style as s;
        matches!(
            self.style,
            s::CURRENCY | s::CURRENCY_ISO_CODE | s::CURRENCY_PLURAL | s::CURRENCY_ACCOUNTING
        )
    }

    /// `value`, as this formatter's style writes it.
    fn format(&self, value: f64, integral: bool) -> String {
        use number_style as s;
        match self.style {
            // Scientific is the one style whose shape is not "digits with
            // separators", so it is written straight out.
            s::SCIENTIFIC => format!("{value:E}"),
            s::PERCENT => format!("{}%", grouped(value * 100.0, Some(0))),
            _ if self.is_currency() => {
                let symbol = self.currency_symbol.as_deref().unwrap_or("$");
                let amount = grouped(value.abs(), Some(2));
                match value < 0.0 {
                    // Apple writes a negative price as `-$1.23`, and accounting
                    // style as `($1.23)`.
                    true if self.style == s::CURRENCY_ACCOUNTING => {
                        format!("({symbol}{amount})")
                    }
                    true => format!("-{symbol}{amount}"),
                    false => format!("{symbol}{amount}"),
                }
            }
            // `NoStyle` is the unadorned number - no grouping separator, which
            // is what makes it the style a caller uses to ask "what is the digit
            // zero in this locale".
            s::NONE => match integral {
                true => format!("{}", value as i64),
                false => trimmed(value),
            },
            // Decimal, and anything this tree does not distinguish from it.
            s::DECIMAL => match integral {
                true => grouped(value, Some(0)),
                false => grouped(value, None),
            },
            _ => match integral {
                true => grouped(value, Some(0)),
                false => grouped(value, None),
            },
        }
    }
}

impl NumberFormat {
    /// The decimal separator this formatter writes.
    fn decimal_separator(&self) -> &str {
        self.settings.decimal_separator.as_deref().unwrap_or(".")
    }

    fn grouping_separator(&self) -> &str {
        self.settings.grouping_separator.as_deref().unwrap_or(",")
    }

    fn grouping_size(&self) -> usize {
        self.settings.grouping_size.unwrap_or(3)
    }

    /// Whether the Objective-C settings shape the digits, which they do for
    /// the plain and decimal styles; the others keep [`NumberFormat::format`].
    fn digit_style(&self) -> bool {
        matches!(self.style, number_style::NONE | number_style::DECIMAL)
    }

    /// `-stringFromNumber:` for the plain and decimal styles: `value` rounded
    /// half-even to the maximum fraction digits, padded to the minimum, and
    /// grouped when grouping is on - the defaults being the style's, as
    /// `NSNumberFormatter` documents them.
    fn format_decimal(&self, value: &crate::decimal::Decimal) -> String {
        use crate::decimal::Rounding;
        if value.is_nan() {
            return "NaN".to_string();
        }
        let decimal = self.style == number_style::DECIMAL;
        let settings = &self.settings;
        let maximum = settings
            .maximum_fraction_digits
            .unwrap_or(if decimal { 3 } else { 0 });
        let minimum = settings.minimum_fraction_digits.unwrap_or(0).min(maximum);
        let grouping = settings.uses_grouping_separator.unwrap_or(decimal);
        let size = self.grouping_size();
        let scale = i16::try_from(maximum).unwrap_or(i16::MAX - 1);
        let text = value.round(scale, Rounding::Bankers).to_string_with(".");
        let (sign, digits) = match text.strip_prefix('-') {
            Some(rest) => ("-", rest),
            None => ("", text.as_str()),
        };
        let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
        let mut fraction = fraction.to_string();
        while fraction.len() < minimum {
            fraction.push('0');
        }
        let mut out = String::from(sign);
        for (seen, ch) in whole.chars().enumerate() {
            if grouping && size > 0 && seen > 0 && (whole.len() - seen) % size == 0 {
                out.push_str(self.grouping_separator());
            }
            out.push(ch);
        }
        if !fraction.is_empty() {
            out.push_str(self.decimal_separator());
            out.push_str(&fraction);
        }
        out
    }
}

/// `value` with `,` every three integer digits, to `fraction` decimal places -
/// or to as many as it takes when `fraction` is `None`.
fn grouped(value: f64, fraction: Option<usize>) -> String {
    let plain = match fraction {
        Some(places) => format!("{:.*}", places, value),
        None => trimmed(value),
    };
    let (sign, digits) = match plain.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", plain.as_str()),
    };
    let (whole, rest) = match digits.split_once('.') {
        Some((whole, rest)) => (whole, Some(rest)),
        None => (digits, None),
    };
    let mut out = String::with_capacity(plain.len() + whole.len() / 3 + 1);
    out.push_str(sign);
    for (seen, ch) in whole.chars().enumerate() {
        if seen > 0 && (whole.len() - seen) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if let Some(rest) = rest {
        out.push('.');
        out.push_str(rest);
    }
    out
}

/// A float with no trailing zeroes and no exponent, which is how a formatter
/// with no fraction-digit setting writes one.
fn trimmed(value: f64) -> String {
    let text = format!("{value}");
    match text.contains('e') || text.contains('E') {
        // Rust's shortest form goes exponential for very large and very small
        // magnitudes; a number formatter does not.
        true => format!("{value:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string(),
        false => text,
    }
}

//
// Date formatting
//

/// `CFDateFormatterStyle`. Apple's values.
mod date_style {
    pub const NONE: i32 = 0;
    pub const SHORT: i32 = 1;
    pub const MEDIUM: i32 = 2;
    pub const LONG: i32 = 3;
    pub const FULL: i32 = 4;
}

struct DateFormat {
    date: i32,
    time: i32,
    /// `-setDateFormat:`, which wins over the two styles once set.
    pattern: Option<String>,
    /// Retained, or null for this machine's zone.
    timezone: CFTypeRef,
}

impl Drop for DateFormat {
    fn drop(&mut self) {
        objc::arc::release(self.timezone as Id);
    }
}

/// The English month names, long then abbreviated.
const MONTHS: [(&str, &str); 12] = [
    ("January", "Jan"),
    ("February", "Feb"),
    ("March", "Mar"),
    ("April", "Apr"),
    ("May", "May"),
    ("June", "Jun"),
    ("July", "Jul"),
    ("August", "Aug"),
    ("September", "Sep"),
    ("October", "Oct"),
    ("November", "Nov"),
    ("December", "Dec"),
];

/// The English weekday names, long then abbreviated, **from Sunday**.
///
/// Sunday first because that is the order a `CFDateFormatter` weekday array is
/// in, and a caller indexes it with a weekday number where Sunday is 1.
const WEEKDAYS: [(&str, &str); 7] = [
    ("Sunday", "Sun"),
    ("Monday", "Mon"),
    ("Tuesday", "Tue"),
    ("Wednesday", "Wed"),
    ("Thursday", "Thu"),
    ("Friday", "Fri"),
    ("Saturday", "Sat"),
];

impl DateFormat {
    fn styled(date: i32, time: i32) -> DateFormat {
        DateFormat {
            date,
            time,
            pattern: None,
            timezone: std::ptr::null(),
        }
    }

    /// The Unicode `LDML` pattern this pair of styles means, which is what
    /// `CFDateFormatterGetFormat` answers with.
    fn pattern(&self) -> String {
        let date = match self.date {
            date_style::SHORT => "M/d/yy",
            date_style::MEDIUM => "MMM d, y",
            date_style::LONG => "MMMM d, y",
            date_style::FULL => "EEEE, MMMM d, y",
            date_style::NONE => "",
            // A style this tree does not know is treated as none, for the reason
            // the module note gives: a caller gets no date part rather than one
            // written in a shape that was guessed at.
            _ => "",
        };
        let time = match self.time {
            date_style::SHORT => "h:mm a",
            date_style::MEDIUM => "h:mm:ss a",
            date_style::LONG => "h:mm:ss a z",
            date_style::FULL => "h:mm:ss a zzzz",
            // As above: no time part.
            _ => "",
        };
        match (date.is_empty(), time.is_empty()) {
            (false, false) => format!("{date}, {time}"),
            (false, true) => date.to_string(),
            (true, false) => time.to_string(),
            (true, true) => String::new(),
        }
    }

    /// `when`, a Unix timestamp, written out in this pair of styles.
    fn format(&self, when: f64) -> String {
        let tm = civil(when);
        let (month_long, month_short) = MONTHS[(tm.tm_mon as usize).min(11)];
        let (day_long, _) = WEEKDAYS[(tm.tm_wday as usize).min(6)];
        let year = tm.tm_year + 1900;
        let date = match self.date {
            date_style::SHORT => format!("{}/{}/{:02}", tm.tm_mon + 1, tm.tm_mday, year % 100),
            date_style::MEDIUM => format!("{month_short} {}, {year}", tm.tm_mday),
            date_style::LONG => format!("{month_long} {}, {year}", tm.tm_mday),
            date_style::FULL => format!("{day_long}, {month_long} {}, {year}", tm.tm_mday),
            date_style::NONE => String::new(),
            // As in `pattern`: an unknown style is none.
            _ => String::new(),
        };
        // Twelve-hour, because every English style is: midnight is `12 AM` and
        // noon is `12 PM`, not `0` and `12`.
        let hour = match tm.tm_hour % 12 {
            0 => 12,
            other => other,
        };
        let half = match tm.tm_hour < 12 {
            true => "AM",
            false => "PM",
        };
        let time = match self.time {
            date_style::SHORT => format!("{hour}:{:02} {half}", tm.tm_min),
            date_style::MEDIUM | date_style::LONG | date_style::FULL => {
                format!("{hour}:{:02}:{:02} {half}", tm.tm_min, tm.tm_sec)
            }
            // As above: no time part.
            _ => String::new(),
        };
        match (date.is_empty(), time.is_empty()) {
            (false, false) => format!("{date} at {time}"),
            (false, true) => date,
            (true, false) => time,
            (true, true) => String::new(),
        }
    }
}

/// A Unix timestamp as local civil time.
///
/// `localtime_r` rather than arithmetic here, because it is the call that knows
/// this host's time zone and its daylight-saving rules - the same answer the
/// guest's own `localtime` would give it.
fn civil(when: f64) -> libc::tm {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let seconds = when as _;
    // SAFETY: a live `time_t` and a live `tm` for the call to fill in.
    unsafe { libc::localtime_r(&seconds, &mut tm) };
    tm
}

/// `when`, a Unix timestamp, written out in an LDML pattern such as
/// `yyyy-MM-dd_HH:mm:ss.SSS`, with `offset` seconds east of UTC.
///
/// The fields are the English ones, as for the styles. A pattern letter this
/// does not know is named and written as itself rather than guessed at.
fn render_pattern(pattern: &str, when: f64, offset: i64) -> String {
    let whole = when.floor();
    let seconds = whole as i64 + offset;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let time = seconds as libc::time_t;
    // SAFETY: a live `time_t` and a live `tm` for the call to fill in.
    unsafe { libc::gmtime_r(&time, &mut tm) };
    let fraction = when - whole;
    let year = tm.tm_year + 1900;
    let hour12 = match tm.tm_hour % 12 {
        0 => 12,
        other => other,
    };

    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        if c == '\'' {
            // `''` is a quote; otherwise everything up to the next quote is literal.
            if chars.get(at + 1) == Some(&'\'') {
                out.push('\'');
                at += 2;
                continue;
            }
            at += 1;
            while at < chars.len() {
                if chars[at] == '\'' {
                    if chars.get(at + 1) == Some(&'\'') {
                        out.push('\'');
                        at += 2;
                        continue;
                    }
                    break;
                }
                out.push(chars[at]);
                at += 1;
            }
            at += 1;
            continue;
        }
        if !c.is_ascii_alphabetic() {
            out.push(c);
            at += 1;
            continue;
        }
        let run = chars[at..].iter().take_while(|next| **next == c).count();
        at += run;
        let number = |value: i64| format!("{value:0run$}");
        let text = match c {
            'G' => "AD".to_string(),
            'y' | 'u' => match run {
                2 => format!("{:02}", year.rem_euclid(100)),
                _ => number(year.into()),
            },
            'M' | 'L' => {
                let (long, short) = MONTHS[(tm.tm_mon as usize).min(11)];
                match run {
                    1 | 2 => number((tm.tm_mon + 1).into()),
                    3 => short.to_string(),
                    _ => long.to_string(),
                }
            }
            'd' => number(tm.tm_mday.into()),
            'D' => number((tm.tm_yday + 1).into()),
            'E' => {
                let (long, short) = WEEKDAYS[(tm.tm_wday as usize).min(6)];
                match run {
                    4 => long.to_string(),
                    _ => short.to_string(),
                }
            }
            'a' => match tm.tm_hour < 12 {
                true => "AM".to_string(),
                false => "PM".to_string(),
            },
            'H' => number(tm.tm_hour.into()),
            'k' => number(match tm.tm_hour {
                0 => 24,
                other => other.into(),
            }),
            'h' => number(hour12.into()),
            'K' => number((tm.tm_hour % 12).into()),
            'm' => number(tm.tm_min.into()),
            's' => number(tm.tm_sec.into()),
            // Fractional seconds: truncated to the digits asked for, as ICU does.
            'S' => {
                let digits = run.min(9) as u32;
                let scaled = (fraction * 10f64.powi(digits as i32)).floor() as i64;
                let mut text = format!("{scaled:0width$}", width = digits as usize);
                text.extend(std::iter::repeat_n('0', run - digits as usize));
                text
            }
            'Z' | 'x' | 'X' => {
                let sign = if offset < 0 { '-' } else { '+' };
                let (hours, minutes) = (offset.abs() / 3600, offset.abs() % 3600 / 60);
                match (c, run) {
                    ('X', _) if offset == 0 => "Z".to_string(),
                    ('Z', 4) => format!("GMT{sign}{hours:02}:{minutes:02}"),
                    ('Z', 5) | (_, 3) | (_, 5) => format!("{sign}{hours:02}:{minutes:02}"),
                    _ => format!("{sign}{hours:02}{minutes:02}"),
                }
            }
            _ => {
                eprintln!(
                    "rosemary: foundation: NSDateFormatter does not know the pattern letter \
                     {c:?} in {pattern:?}; writing it as itself"
                );
                std::iter::repeat_n(c, run).collect()
            }
        };
        out.push_str(&text);
    }
    out
}

/// Seconds east of UTC at `when` in `timezone`, null being this machine's zone.
///
/// # Safety
///
/// `timezone` is null or a time zone.
unsafe fn offset_at(timezone: CFTypeRef, when: f64) -> i64 {
    if timezone.is_null() {
        return civil(when).tm_gmtoff as i64;
    }
    // SAFETY: a time zone; the conversion is to CoreFoundation's epoch.
    unsafe { corefoundation::calendar::offset_seconds(timezone, when - 978_307_200.0) }.unwrap_or(0)
}

mod objc_api {
    use objc::object::Id;
    use objc::sel::Sel;

    use super::{CFTypeRef, DateFormat, contents, date_class, make};

    /// # Safety
    ///
    /// `object` is null or an object this runtime allocated.
    unsafe fn format<'a>(object: Id) -> Option<&'a mut DateFormat> {
        unsafe { contents::<DateFormat>(object as CFTypeRef, date_class()) }
    }

    fn autoreleased(text: &str) -> Id {
        let string = crate::string::from_str(text);
        // SAFETY: a fresh string.
        unsafe { corefoundation::api::CFAutorelease(string as CFTypeRef) as Id }
    }

    /// `-init` on the placeholder `+alloc` made from the abstract class, which
    /// has no room for settings: the concrete formatter replaces it, as the
    /// collection clusters do.
    unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: an object this runtime allocated.
        if unsafe { format(receiver) }.is_some() {
            return receiver;
        }
        objc::arc::release(receiver);
        make(date_class(), DateFormat::styled(0, 0)) as Id
    }

    unsafe extern "C" fn set_date_format(receiver: Id, _sel: Sel, pattern: Id) {
        // SAFETY: a formatter, and a string or nil.
        if let Some(format) = unsafe { format(receiver) } {
            format.pattern = (!pattern.is_null()).then(|| {
                String::from_utf8_lossy(&unsafe { crate::string::bytes(pattern) }).into_owned()
            });
        }
    }

    unsafe extern "C" fn date_format(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: a formatter.
        match unsafe { format(receiver) } {
            Some(format) => {
                autoreleased(&format.pattern.clone().unwrap_or_else(|| format.pattern()))
            }
            None => std::ptr::null_mut(),
        }
    }

    unsafe extern "C" fn set_time_zone(receiver: Id, _sel: Sel, timezone: Id) {
        // SAFETY: a formatter.
        if let Some(format) = unsafe { format(receiver) } {
            let old = std::mem::replace(
                &mut format.timezone,
                objc::arc::retain(timezone) as CFTypeRef,
            );
            objc::arc::release(old as Id);
        }
    }

    unsafe extern "C" fn time_zone(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: a formatter.
        unsafe { format(receiver) }.map_or(std::ptr::null_mut(), |format| format.timezone as Id)
    }

    unsafe extern "C" fn set_date_style(receiver: Id, _sel: Sel, style: usize) {
        // SAFETY: a formatter.
        if let Some(format) = unsafe { format(receiver) } {
            format.date = style as i32;
        }
    }

    unsafe extern "C" fn date_style(receiver: Id, _sel: Sel) -> usize {
        // SAFETY: a formatter.
        unsafe { format(receiver) }.map_or(0, |format| format.date as usize)
    }

    unsafe extern "C" fn set_time_style(receiver: Id, _sel: Sel, style: usize) {
        // SAFETY: a formatter.
        if let Some(format) = unsafe { format(receiver) } {
            format.time = style as i32;
        }
    }

    unsafe extern "C" fn time_style(receiver: Id, _sel: Sel) -> usize {
        // SAFETY: a formatter.
        unsafe { format(receiver) }.map_or(0, |format| format.time as usize)
    }

    /// `-setLocale:`. Every field here is written the English way already; see
    /// the module note.
    unsafe extern "C" fn set_locale(_receiver: Id, _sel: Sel, _locale: Id) {}

    unsafe extern "C" fn string_from_date(receiver: Id, _sel: Sel, date: Id) -> Id {
        // SAFETY: a formatter.
        let Some(format) = (unsafe { format(receiver) }) else {
            return std::ptr::null_mut();
        };
        let Some(when) = crate::timer::interval_since_1970(date) else {
            return std::ptr::null_mut();
        };
        let text = match &format.pattern {
            // SAFETY: the zone this formatter retained, or null.
            Some(pattern) => super::render_pattern(pattern, when, unsafe {
                super::offset_at(format.timezone, when)
            }),
            None => format.format(when),
        };
        autoreleased(&text)
    }

    pub(super) fn install() {
        let base = objc::class::lookup(c"NSDateFormatter").expect("registered by date_class");
        crate::define::methods!(base, {
            "init" => init, c"@16@0:8";
            "setDateFormat:" => set_date_format, c"v24@0:8@16";
            "dateFormat" => date_format, c"@16@0:8";
            "setTimeZone:" => set_time_zone, c"v24@0:8@16";
            "timeZone" => time_zone, c"@16@0:8";
            "setDateStyle:" => set_date_style, c"v24@0:8Q16";
            "dateStyle" => date_style, c"Q16@0:8";
            "setTimeStyle:" => set_time_style, c"v24@0:8Q16";
            "timeStyle" => time_style, c"Q16@0:8";
            "setLocale:" => set_locale, c"v24@0:8@16";
            "stringFromDate:" => string_from_date, c"@24@0:8@16";
        });
    }
}

/// `NSNumberFormatter`'s Objective-C interface over the same settings the
/// CoreFoundation functions use.
mod number_api {
    use objc::object::Id;
    use objc::sel::Sel;

    use super::{CFTypeRef, NumberFormat, contents, make, number_class, number_style};

    /// # Safety
    ///
    /// `object` is null or an object this runtime allocated.
    unsafe fn format<'a>(object: Id) -> Option<&'a mut NumberFormat> {
        unsafe { contents::<NumberFormat>(object as CFTypeRef, number_class()) }
    }

    fn autoreleased(text: &str) -> Id {
        let string = crate::string::from_str(text);
        // SAFETY: a fresh string.
        unsafe { corefoundation::api::CFAutorelease(string as CFTypeRef) as Id }
    }

    fn text_of(string: Id) -> Option<String> {
        // SAFETY: a string, or nil.
        (!string.is_null())
            .then(|| String::from_utf8_lossy(&unsafe { crate::string::bytes(string) }).into_owned())
    }

    /// `-init`, replacing the abstract class's placeholder as the date
    /// formatter's does.
    unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: an object this runtime allocated.
        if unsafe { format(receiver) }.is_some() {
            return receiver;
        }
        objc::arc::release(receiver);
        make(
            number_class(),
            NumberFormat {
                style: number_style::NONE,
                ..Default::default()
            },
        ) as Id
    }

    macro_rules! with_format {
        ($receiver:expr, $default:expr, |$f:ident| $body:expr) => {
            // SAFETY: a formatter this module made, or anything else, which
            // answers the default.
            match unsafe { format($receiver) } {
                Some($f) => $body,
                None => $default,
            }
        };
    }

    unsafe extern "C" fn set_number_style(receiver: Id, _sel: Sel, style: usize) {
        with_format!(receiver, (), |f| f.style = style as i32)
    }

    unsafe extern "C" fn number_style(receiver: Id, _sel: Sel) -> usize {
        with_format!(receiver, 0, |f| f.style as usize)
    }

    unsafe extern "C" fn set_maximum_fraction_digits(receiver: Id, _sel: Sel, digits: usize) {
        with_format!(receiver, (), |f| f.settings.maximum_fraction_digits =
            Some(digits))
    }

    unsafe extern "C" fn maximum_fraction_digits(receiver: Id, _sel: Sel) -> usize {
        with_format!(receiver, 0, |f| f
            .settings
            .maximum_fraction_digits
            .unwrap_or(if f.style == number_style::DECIMAL {
                3
            } else {
                0
            }))
    }

    unsafe extern "C" fn set_minimum_fraction_digits(receiver: Id, _sel: Sel, digits: usize) {
        with_format!(receiver, (), |f| f.settings.minimum_fraction_digits =
            Some(digits))
    }

    unsafe extern "C" fn minimum_fraction_digits(receiver: Id, _sel: Sel) -> usize {
        with_format!(receiver, 0, |f| f
            .settings
            .minimum_fraction_digits
            .unwrap_or(0))
    }

    unsafe extern "C" fn set_uses_grouping_separator(receiver: Id, _sel: Sel, on: u8) {
        with_format!(receiver, (), |f| f.settings.uses_grouping_separator =
            Some(on != 0))
    }

    unsafe extern "C" fn uses_grouping_separator(receiver: Id, _sel: Sel) -> u8 {
        with_format!(receiver, 0, |f| u8::from(
            f.settings
                .uses_grouping_separator
                .unwrap_or(f.style == number_style::DECIMAL)
        ))
    }

    unsafe extern "C" fn set_grouping_size(receiver: Id, _sel: Sel, size: usize) {
        with_format!(receiver, (), |f| f.settings.grouping_size = Some(size))
    }

    unsafe extern "C" fn grouping_size(receiver: Id, _sel: Sel) -> usize {
        with_format!(receiver, 3, |f| f.settings.grouping_size.unwrap_or(3))
    }

    unsafe extern "C" fn set_decimal_separator(receiver: Id, _sel: Sel, text: Id) {
        with_format!(receiver, (), |f| f.settings.decimal_separator =
            text_of(text))
    }

    unsafe extern "C" fn decimal_separator(receiver: Id, _sel: Sel) -> Id {
        autoreleased(&with_format!(receiver, ".".to_string(), |f| f
            .decimal_separator()
            .to_string()))
    }

    unsafe extern "C" fn set_grouping_separator(receiver: Id, _sel: Sel, text: Id) {
        with_format!(receiver, (), |f| f.settings.grouping_separator =
            text_of(text))
    }

    unsafe extern "C" fn grouping_separator(receiver: Id, _sel: Sel) -> Id {
        autoreleased(&with_format!(receiver, ",".to_string(), |f| f
            .grouping_separator()
            .to_string()))
    }

    unsafe extern "C" fn currency_symbol(receiver: Id, _sel: Sel) -> Id {
        autoreleased(&with_format!(receiver, "$".to_string(), |f| f
            .currency_symbol
            .clone()
            .unwrap_or_else(|| "$".to_string())))
    }

    unsafe extern "C" fn set_currency_symbol(receiver: Id, _sel: Sel, text: Id) {
        with_format!(receiver, (), |f| f.currency_symbol = text_of(text))
    }

    unsafe extern "C" fn set_generates_decimal_numbers(receiver: Id, _sel: Sel, on: u8) {
        with_format!(receiver, (), |f| f.settings.generates_decimal_numbers =
            on != 0)
    }

    unsafe extern "C" fn generates_decimal_numbers(receiver: Id, _sel: Sel) -> u8 {
        with_format!(receiver, 0, |f| u8::from(
            f.settings.generates_decimal_numbers
        ))
    }

    /// Settings this tree keeps no model of: the locale is the process's own,
    /// and a 10.0-style format string only ever restates the style.
    unsafe extern "C" fn ignored_object(_receiver: Id, _sel: Sel, _value: Id) {}

    unsafe extern "C" fn ignored_flag(_receiver: Id, _sel: Sel, _value: u8) {}

    unsafe extern "C" fn string_from_number(receiver: Id, _sel: Sel, number: Id) -> Id {
        // SAFETY: a number, or nil.
        let Some(value) = (unsafe { crate::decimal::value_of(number) }) else {
            return std::ptr::null_mut();
        };
        let text = with_format!(receiver, None, |f| Some(match f.digit_style() {
            true => f.format_decimal(&value),
            false => f.format(value.to_f64(), false),
        }));
        text.map_or(std::ptr::null_mut(), |text| autoreleased(&text))
    }

    /// `-numberFromString:`: the digits with this formatter's separators read
    /// back, as an `NSDecimalNumber` when it generates them. Nil for text that
    /// is not entirely a number.
    unsafe extern "C" fn number_from_string(receiver: Id, _sel: Sel, text: Id) -> Id {
        let Some(text) = text_of(text) else {
            return std::ptr::null_mut();
        };
        let parsed = with_format!(receiver, None, |f| {
            let plain = text.trim().replace(f.grouping_separator(), "");
            let value = crate::decimal::Decimal::parse(&plain, f.decimal_separator());
            let whole = plain.replace(f.decimal_separator(), ".");
            let exact = !value.is_nan()
                && whole.trim_start_matches(['+', '-']).chars().all(|c| {
                    c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '-' || c == '+'
                });
            exact.then_some((value, f.settings.generates_decimal_numbers))
        });
        let Some((value, decimal)) = parsed else {
            return std::ptr::null_mut();
        };
        match decimal {
            true => objc::arc::autorelease(crate::decimal::make(value)),
            // SAFETY: a fresh number.
            false => unsafe {
                corefoundation::api::CFAutorelease(corefoundation::collections::new_number(
                    corefoundation::collections::Number::Float(value.to_f64()),
                )) as Id
            },
        }
    }

    pub(super) fn install() {
        let base = objc::class::lookup(c"NSNumberFormatter").expect("registered by number_class");
        crate::define::methods!(base, {
            "init" => init, c"@16@0:8";
            "setNumberStyle:" => set_number_style, c"v24@0:8Q16";
            "numberStyle" => number_style, c"Q16@0:8";
            "setMaximumFractionDigits:" => set_maximum_fraction_digits, c"v24@0:8Q16";
            "maximumFractionDigits" => maximum_fraction_digits, c"Q16@0:8";
            "setMinimumFractionDigits:" => set_minimum_fraction_digits, c"v24@0:8Q16";
            "minimumFractionDigits" => minimum_fraction_digits, c"Q16@0:8";
            "setUsesGroupingSeparator:" => set_uses_grouping_separator, c"v20@0:8c16";
            "usesGroupingSeparator" => uses_grouping_separator, c"c16@0:8";
            "setGroupingSize:" => set_grouping_size, c"v24@0:8Q16";
            "groupingSize" => grouping_size, c"Q16@0:8";
            "setDecimalSeparator:" => set_decimal_separator, c"v24@0:8@16";
            "decimalSeparator" => decimal_separator, c"@16@0:8";
            "setGroupingSeparator:" => set_grouping_separator, c"v24@0:8@16";
            "groupingSeparator" => grouping_separator, c"@16@0:8";
            "setThousandSeparator:" => set_grouping_separator, c"v24@0:8@16";
            "thousandSeparator" => grouping_separator, c"@16@0:8";
            "setCurrencySymbol:" => set_currency_symbol, c"v24@0:8@16";
            "currencySymbol" => currency_symbol, c"@16@0:8";
            "setGeneratesDecimalNumbers:" => set_generates_decimal_numbers, c"v20@0:8c16";
            "generatesDecimalNumbers" => generates_decimal_numbers, c"c16@0:8";
            "setLocale:" => ignored_object, c"v24@0:8@16";
            "setFormat:" => ignored_object, c"v24@0:8@16";
            "setLocalizesFormat:" => ignored_flag, c"v20@0:8c16";
            "setHasThousandSeparators:" => set_uses_grouping_separator, c"v20@0:8c16";
            "stringFromNumber:" => string_from_number, c"@24@0:8@16";
            "stringForObjectValue:" => string_from_number, c"@24@0:8@16";
            "numberFromString:" => number_from_string, c"@24@0:8@16";
        });
    }
}

//
// Date components
//

/// `NSDateComponentUndefined`: `NSIntegerMax`, the sentinel every numeric
/// component starts as until something sets it.
const UNDEFINED: i64 = i64::MAX;

/// A `NSDateComponents`' fields, which are plain storage here - this runtime
/// does no calendrical math over them beyond what `CFCalendarCompose/
/// DecomposeAbsoluteTime` already does for [`Calendar`].
struct DateComponents {
    era: i64,
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    nanosecond: i64,
    weekday: i64,
    weekday_ordinal: i64,
    quarter: i64,
    week_of_month: i64,
    week_of_year: i64,
    year_for_week_of_year: i64,
    /// Retained, or null.
    calendar: CFTypeRef,
    /// Retained, or null.
    time_zone: CFTypeRef,
    leap_month: bool,
}

impl Default for DateComponents {
    fn default() -> Self {
        DateComponents {
            era: UNDEFINED,
            year: UNDEFINED,
            month: UNDEFINED,
            day: UNDEFINED,
            hour: UNDEFINED,
            minute: UNDEFINED,
            second: UNDEFINED,
            nanosecond: UNDEFINED,
            weekday: UNDEFINED,
            weekday_ordinal: UNDEFINED,
            quarter: UNDEFINED,
            week_of_month: UNDEFINED,
            week_of_year: UNDEFINED,
            year_for_week_of_year: UNDEFINED,
            calendar: std::ptr::null(),
            time_zone: std::ptr::null(),
            leap_month: false,
        }
    }
}

impl Drop for DateComponents {
    fn drop(&mut self) {
        objc::arc::release(self.calendar as Id);
        objc::arc::release(self.time_zone as Id);
    }
}

fn date_components_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let p = *CLASS
        .get_or_init(|| objc::class::ptr(register::<DateComponents>(c"NSDateComponents")) as usize);
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(p as *const Class).expect("a date components class") }
}

/// `-init`: `+alloc` hands back a zeroed `Body`, so this is where the boxed
/// defaults actually get allocated - once, the second call on an already
/// initialized receiver just hands it back unchanged.
unsafe extern "C" fn date_components_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a fresh or already-initialized `NSDateComponents`.
    unsafe {
        let body = receiver as *mut Body<DateComponents>;
        if (*body).contents.is_null() {
            (*body).contents = Box::into_raw(Box::new(DateComponents::default()));
        }
    }
    receiver
}

/// A signed-integer getter for one of [`DateComponents`]' plain fields.
macro_rules! component_i64_accessors {
    ($getter:ident, $setter:ident, $field:ident) => {
        unsafe extern "C" fn $getter(receiver: Id, _sel: Sel) -> i64 {
            // SAFETY: an `NSDateComponents`.
            unsafe { contents::<DateComponents>(receiver as CFTypeRef, date_components_class()) }
                .map_or(UNDEFINED, |c| c.$field)
        }
        unsafe extern "C" fn $setter(receiver: Id, _sel: Sel, value: i64) {
            // SAFETY: an `NSDateComponents`.
            if let Some(c) = unsafe {
                contents::<DateComponents>(receiver as CFTypeRef, date_components_class())
            } {
                c.$field = value;
            }
        }
    };
}

component_i64_accessors!(dc_get_era, dc_set_era, era);
component_i64_accessors!(dc_get_year, dc_set_year, year);
component_i64_accessors!(dc_get_month, dc_set_month, month);
component_i64_accessors!(dc_get_day, dc_set_day, day);
component_i64_accessors!(dc_get_hour, dc_set_hour, hour);
component_i64_accessors!(dc_get_minute, dc_set_minute, minute);
component_i64_accessors!(dc_get_second, dc_set_second, second);
component_i64_accessors!(dc_get_nanosecond, dc_set_nanosecond, nanosecond);
component_i64_accessors!(dc_get_weekday, dc_set_weekday, weekday);
component_i64_accessors!(
    dc_get_weekday_ordinal,
    dc_set_weekday_ordinal,
    weekday_ordinal
);
component_i64_accessors!(dc_get_quarter, dc_set_quarter, quarter);
component_i64_accessors!(dc_get_week_of_month, dc_set_week_of_month, week_of_month);
component_i64_accessors!(dc_get_week_of_year, dc_set_week_of_year, week_of_year);
component_i64_accessors!(
    dc_get_year_for_week_of_year,
    dc_set_year_for_week_of_year,
    year_for_week_of_year
);

unsafe extern "C" fn dc_get_leap_month(receiver: Id, _sel: Sel) -> bool {
    // SAFETY: an `NSDateComponents`.
    unsafe { contents::<DateComponents>(receiver as CFTypeRef, date_components_class()) }
        .is_some_and(|c| c.leap_month)
}

unsafe extern "C" fn dc_set_leap_month(receiver: Id, _sel: Sel, value: bool) {
    // SAFETY: an `NSDateComponents`.
    if let Some(c) =
        unsafe { contents::<DateComponents>(receiver as CFTypeRef, date_components_class()) }
    {
        c.leap_month = value;
    }
}

unsafe extern "C" fn dc_get_calendar(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSDateComponents`.
    match unsafe { contents::<DateComponents>(receiver as CFTypeRef, date_components_class()) } {
        Some(c) if !c.calendar.is_null() => {
            objc::arc::autorelease(objc::arc::retain(c.calendar as Id))
        }
        _ => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn dc_set_calendar(receiver: Id, _sel: Sel, value: Id) {
    // SAFETY: an `NSDateComponents` and a calendar, or nil.
    let Some(c) =
        (unsafe { contents::<DateComponents>(receiver as CFTypeRef, date_components_class()) })
    else {
        return;
    };
    let held = objc::arc::retain(value) as CFTypeRef;
    objc::arc::release(c.calendar as Id);
    c.calendar = held;
}

unsafe extern "C" fn dc_get_time_zone(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSDateComponents`.
    match unsafe { contents::<DateComponents>(receiver as CFTypeRef, date_components_class()) } {
        Some(c) if !c.time_zone.is_null() => {
            objc::arc::autorelease(objc::arc::retain(c.time_zone as Id))
        }
        _ => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn dc_set_time_zone(receiver: Id, _sel: Sel, value: Id) {
    // SAFETY: an `NSDateComponents` and a time zone, or nil.
    let Some(c) =
        (unsafe { contents::<DateComponents>(receiver as CFTypeRef, date_components_class()) })
    else {
        return;
    };
    let held = objc::arc::retain(value) as CFTypeRef;
    objc::arc::release(c.time_zone as Id);
    c.time_zone = held;
}

/// `+[NSDateComponents alloc]` and `+allocWithZone:`: an instance of the
/// concrete class, which is the one sized for the fields, as a class cluster's
/// placeholder answers.
unsafe extern "C" fn date_components_alloc(_class: Id, _sel: Sel) -> Id {
    // SAFETY: `Body<DateComponents>` is the layout the class was registered with.
    let body: *mut Body<DateComponents> =
        unsafe { corefoundation::base::allocate(date_components_class()) };
    if !body.is_null() {
        // SAFETY: a live instance; `-init` fills this in.
        unsafe { (*body).contents = std::ptr::null_mut() };
    }
    body as Id
}

fn install_date_components() {
    let class = date_components_class();
    let base = objc::class::lookup(c"NSDateComponents").expect("registered by its class");
    crate::define::class_methods!(base, {
        "alloc" => date_components_alloc, c"@16@0:8";
        "allocWithZone:" => date_components_alloc, c"@24@0:8^v16";
    });
    crate::define::methods!(class, {
        "init" => date_components_init, c"@16@0:8";
        "era" => dc_get_era, c"q16@0:8";
        "setEra:" => dc_set_era, c"v24@0:8q16";
        "year" => dc_get_year, c"q16@0:8";
        "setYear:" => dc_set_year, c"v24@0:8q16";
        "month" => dc_get_month, c"q16@0:8";
        "setMonth:" => dc_set_month, c"v24@0:8q16";
        "day" => dc_get_day, c"q16@0:8";
        "setDay:" => dc_set_day, c"v24@0:8q16";
        "hour" => dc_get_hour, c"q16@0:8";
        "setHour:" => dc_set_hour, c"v24@0:8q16";
        "minute" => dc_get_minute, c"q16@0:8";
        "setMinute:" => dc_set_minute, c"v24@0:8q16";
        "second" => dc_get_second, c"q16@0:8";
        "setSecond:" => dc_set_second, c"v24@0:8q16";
        "nanosecond" => dc_get_nanosecond, c"q16@0:8";
        "setNanosecond:" => dc_set_nanosecond, c"v24@0:8q16";
        "weekday" => dc_get_weekday, c"q16@0:8";
        "setWeekday:" => dc_set_weekday, c"v24@0:8q16";
        "weekdayOrdinal" => dc_get_weekday_ordinal, c"q16@0:8";
        "setWeekdayOrdinal:" => dc_set_weekday_ordinal, c"v24@0:8q16";
        "quarter" => dc_get_quarter, c"q16@0:8";
        "setQuarter:" => dc_set_quarter, c"v24@0:8q16";
        "weekOfMonth" => dc_get_week_of_month, c"q16@0:8";
        "setWeekOfMonth:" => dc_set_week_of_month, c"v24@0:8q16";
        "weekOfYear" => dc_get_week_of_year, c"q16@0:8";
        "setWeekOfYear:" => dc_set_week_of_year, c"v24@0:8q16";
        "yearForWeekOfYear" => dc_get_year_for_week_of_year, c"q16@0:8";
        "setYearForWeekOfYear:" => dc_set_year_for_week_of_year, c"v24@0:8q16";
        "isLeapMonth" => dc_get_leap_month, c"B16@0:8";
        "setLeapMonth:" => dc_set_leap_month, c"v24@0:8B16";
        "calendar" => dc_get_calendar, c"@16@0:8";
        "setCalendar:" => dc_set_calendar, c"v24@0:8@16";
        "timeZone" => dc_get_time_zone, c"@16@0:8";
        "setTimeZone:" => dc_set_time_zone, c"v24@0:8@16";
    });
}

#[cfg(test)]
mod date_components_tests {
    use super::*;

    /// `+alloc` hands back a zeroed body; `-init` is what plants the boxed
    /// defaults, and every numeric component starts undefined until set.
    #[test]
    fn fresh_components_are_all_undefined() {
        crate::bootstrap();
        // SAFETY: a real class, sent its own `-init` directly rather than
        // through a guest `objc_msgSend` - that marshaling is `tests/msgsend.rs`'s
        // business, not this module's.
        unsafe {
            let class = objc::class::ptr(date_components_class()) as Id;
            let instance = objc::object::create_instance(date_components_class(), 0);
            let sel = objc::sel::get("init");
            let _ = class; // the class pointer above is not otherwise used here
            date_components_init(instance, sel);
            assert_eq!(dc_get_year(instance, sel), UNDEFINED);
            assert_eq!(dc_get_month(instance, sel), UNDEFINED);
            assert!(!dc_get_leap_month(instance, sel));
        }
    }

    /// A set component reads back exactly, and is independent of its siblings.
    #[test]
    fn a_set_component_reads_back() {
        crate::bootstrap();
        unsafe {
            let instance = objc::object::create_instance(date_components_class(), 0);
            let sel = objc::sel::get("init");
            date_components_init(instance, sel);
            dc_set_year(instance, sel, 2024);
            dc_set_month(instance, sel, 3);
            assert_eq!(dc_get_year(instance, sel), 2024);
            assert_eq!(dc_get_month(instance, sel), 3);
            assert_eq!(dc_get_day(instance, sel), UNDEFINED);
        }
    }
}

//
// Calendars
//

struct Calendar {
    /// Which weekday a week starts on, 1 being Sunday - see [`first_weekday`].
    first_weekday: CFIndex,
    /// The identifier it was made with, retained. Null for the current
    /// calendar, which was not made with one.
    identifier: CFTypeRef,
    /// The zone its dates are placed in, retained.
    timezone: CFTypeRef,
}

impl Drop for Calendar {
    fn drop(&mut self) {
        objc::arc::release(self.identifier as Id);
        objc::arc::release(self.timezone as Id);
    }
}

/// The day a week starts on in the country this tree reports.
///
/// Derived from the country rather than guessed at, the way
/// `locale::uses_metric` is: which day a week begins on is a fact about a place,
/// not a preference this host has a store for. The listed countries are the ones
/// that start a week on Sunday; everywhere else starts on Monday, which is the
/// ISO 8601 week and the majority by a long way.
fn first_weekday(country: &str) -> CFIndex {
    const SUNDAY: &[&str] = &[
        "US", "CA", "JP", "IL", "KR", "TW", "HK", "MO", "PH", "SG", "TH", "ZA", "BR", "MX", "CO",
        "VE", "PE", "AR", "CL", "DO", "GT", "HN", "NI", "PA", "PR", "SV", "JM", "BS", "BZ", "BO",
        "EG", "SA", "AE", "KW", "QA", "BH", "OM", "JO", "IQ", "LY", "DZ", "MA", "SD", "YE", "SY",
        "IN", "PK", "BD", "NP", "LK", "MM", "KH", "LA", "VN", "ID", "MY", "BN", "CN", "MN",
    ];
    match SUNDAY.contains(&country) {
        true => 1,
        false => 2,
    }
}

//
// The classes
//

fn number_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let p = *CLASS
        .get_or_init(|| objc::class::ptr(register::<NumberFormat>(c"NSNumberFormatter")) as usize);
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(p as *const Class).expect("a number formatter class") }
}

fn date_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let p = *CLASS
        .get_or_init(|| objc::class::ptr(register::<DateFormat>(c"NSDateFormatter")) as usize);
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(p as *const Class).expect("a date formatter class") }
}

fn calendar_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let p = *CLASS.get_or_init(|| objc::class::ptr(register::<Calendar>(c"NSCalendar")) as usize);
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(p as *const Class).expect("a calendar class") }
}

//
// NSCalendar
//

/// `NSCalendarUnit` bits, as `<Foundation/NSCalendar.h>` numbers them.
mod unit {
    pub const ERA: u64 = 1 << 1;
    pub const YEAR: u64 = 1 << 2;
    pub const MONTH: u64 = 1 << 3;
    pub const DAY: u64 = 1 << 4;
    pub const HOUR: u64 = 1 << 5;
    pub const MINUTE: u64 = 1 << 6;
    pub const SECOND: u64 = 1 << 7;
    pub const WEEKDAY: u64 = 1 << 9;
    pub const NANOSECOND: u64 = 1 << 15;
    pub const CALENDAR: u64 = 1 << 20;
    pub const TIME_ZONE: u64 = 1 << 21;
}

fn send_id(receiver: Id, selector: &str) -> Id {
    // SAFETY: an `id (id, SEL)` method, sent through the dispatcher.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector))
    }
}

fn calendar_of<'a>(receiver: Id) -> Option<&'a mut Calendar> {
    // SAFETY: an object this runtime allocated, checked by class.
    unsafe { contents::<Calendar>(receiver as CFTypeRef, calendar_class()) }
}

/// A calendar, retained: the current one, or the one `identifier` names.
fn new_calendar(identifier: CFTypeRef) -> CFTypeRef {
    let timezone = unsafe { corefoundation::timezone::copy_system() };
    let identifier = match identifier.is_null() {
        true => corefoundation::string::from_bytes(b"gregorian", false),
        false => objc::arc::retain(identifier as Id) as CFTypeRef,
    };
    make(
        calendar_class(),
        Calendar {
            first_weekday: first_weekday(&crate::locale::country_code()),
            identifier,
            timezone,
        },
    )
}

/// Whether this tree computes dates in the calendar `identifier` names.
fn is_gregorian(identifier: Id) -> bool {
    // SAFETY: null or a string.
    match unsafe { corefoundation::string::as_string(identifier as CFTypeRef) } {
        Some(text) => matches!(&text.utf8()[..], b"gregorian" | b"iso8601"),
        None => false,
    }
}

/// `+currentCalendar` and `+autoupdatingCurrentCalendar`.
unsafe extern "C" fn calendar_current(_class: Id, _sel: Sel) -> Id {
    objc::arc::autorelease(new_calendar(std::ptr::null()) as Id)
}

/// `+calendarWithIdentifier:`: nil for a calendar this tree does not compute,
/// as Apple answers nil for an identifier it does not know, rather than
/// Gregorian dates that would be the wrong dates.
unsafe extern "C" fn calendar_with_identifier(_class: Id, _sel: Sel, identifier: Id) -> Id {
    if !is_gregorian(identifier) {
        return std::ptr::null_mut();
    }
    objc::arc::autorelease(new_calendar(identifier as CFTypeRef) as Id)
}

/// `-initWithCalendarIdentifier:`, which answers a calendar of this runtime's
/// in place of the receiver, the way a class cluster's initializer does.
unsafe extern "C" fn calendar_init_with_identifier(receiver: Id, _sel: Sel, identifier: Id) -> Id {
    objc::arc::release(receiver);
    if !is_gregorian(identifier) {
        return std::ptr::null_mut();
    }
    new_calendar(identifier as CFTypeRef) as Id
}

unsafe extern "C" fn calendar_identifier(receiver: Id, _sel: Sel) -> Id {
    calendar_of(receiver).map_or(std::ptr::null_mut(), |c| c.identifier as Id)
}

unsafe extern "C" fn calendar_time_zone(receiver: Id, _sel: Sel) -> Id {
    calendar_of(receiver).map_or(std::ptr::null_mut(), |c| c.timezone as Id)
}

unsafe extern "C" fn calendar_set_time_zone(receiver: Id, _sel: Sel, timezone: Id) {
    let Some(calendar) = calendar_of(receiver) else {
        return;
    };
    // A nil zone is the system's, as Apple resets it.
    let held = match timezone.is_null() {
        true => unsafe { corefoundation::timezone::copy_system() },
        false => objc::arc::retain(timezone) as CFTypeRef,
    };
    objc::arc::release(calendar.timezone as Id);
    calendar.timezone = held;
}

unsafe extern "C" fn calendar_first_weekday(receiver: Id, _sel: Sel) -> u64 {
    calendar_of(receiver).map_or(1, |c| c.first_weekday as u64)
}

unsafe extern "C" fn calendar_set_first_weekday(receiver: Id, _sel: Sel, weekday: u64) {
    if let Some(calendar) = calendar_of(receiver)
        && (1..=7).contains(&weekday)
    {
        calendar.first_weekday = weekday as CFIndex;
    }
}

// FIXME (ACCURACY): a calendar here has no locale of its own; it answers the
// current one and forgets one it is given, which only affects its symbols.
unsafe extern "C" fn calendar_locale(_receiver: Id, _sel: Sel) -> Id {
    let class = objc::class::lookup(c"NSLocale")
        .map_or(std::ptr::null_mut(), |c| objc::class::ptr(c) as Id);
    send_id(class, "currentLocale")
}

unsafe extern "C" fn calendar_set_locale(_receiver: Id, _sel: Sel, _locale: Id) {}

/// `-components:fromDate:`, the Gregorian fields `units` asks for.
unsafe extern "C" fn calendar_components_from_date(
    receiver: Id,
    _sel: Sel,
    units: u64,
    date: Id,
) -> Id {
    let Some(calendar) = calendar_of(receiver) else {
        return std::ptr::null_mut();
    };
    if date.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `-timeIntervalSinceReferenceDate` is `double (id, SEL)`.
    let at = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> f64 = std::mem::transmute(objc::msg::entry());
        f(date, objc::sel::get("timeIntervalSinceReferenceDate"))
    };
    let timezone = calendar.timezone;
    // SAFETY: null or a time zone.
    let g = unsafe { corefoundation::calendar::absolute_time_get_gregorian_date(at, timezone) };
    let mut c = DateComponents::default();
    let want = |bit: u64| units & bit != 0;
    if want(unit::ERA) {
        c.era = i64::from(g.year > 0);
    }
    if want(unit::YEAR) {
        c.year = i64::from(g.year);
    }
    if want(unit::MONTH) {
        c.month = i64::from(g.month);
    }
    if want(unit::DAY) {
        c.day = i64::from(g.day);
    }
    if want(unit::HOUR) {
        c.hour = i64::from(g.hour);
    }
    if want(unit::MINUTE) {
        c.minute = i64::from(g.minute);
    }
    if want(unit::SECOND) {
        c.second = g.second.floor() as i64;
    }
    if want(unit::NANOSECOND) {
        c.nanosecond = ((g.second - g.second.floor()) * 1e9) as i64;
    }
    if want(unit::WEEKDAY) {
        // Sunday is one here and seven in CoreFoundation's day of week.
        let day = unsafe { corefoundation::calendar::absolute_time_get_day_of_week(at, timezone) };
        c.weekday = i64::from(day % 7 + 1);
    }
    if want(unit::CALENDAR) {
        c.calendar = objc::arc::retain(receiver) as CFTypeRef;
    }
    if want(unit::TIME_ZONE) {
        c.time_zone = objc::arc::retain(timezone as Id) as CFTypeRef;
    }
    objc::arc::autorelease(make(date_components_class(), c) as Id)
}

/// `-dateFromComponents:`; an undefined field takes its smallest value, as
/// Apple fills one in.
// FIXME (ACCURACY): week-based fields (weekday, week of year, ...) are ignored.
unsafe extern "C" fn calendar_date_from_components(receiver: Id, _sel: Sel, components: Id) -> Id {
    let Some(calendar) = calendar_of(receiver) else {
        return std::ptr::null_mut();
    };
    // SAFETY: an `NSDateComponents` of this runtime's, checked by class.
    let Some(c) =
        (unsafe { contents::<DateComponents>(components as CFTypeRef, date_components_class()) })
    else {
        return std::ptr::null_mut();
    };
    let or = |value: i64, default: i64| if value == UNDEFINED { default } else { value };
    let timezone = match c.time_zone.is_null() {
        true => calendar.timezone,
        false => c.time_zone,
    };
    // Out-of-range fields carry over, as `-dateFromComponents:` lets them:
    // the month is normalized into the year and the rest are plain seconds.
    let month0 = or(c.month, 1) - 1;
    let year = or(c.year, 1) + month0.div_euclid(12);
    let base = corefoundation::calendar::GregorianDate {
        year: year as i32,
        month: (month0.rem_euclid(12) + 1) as i8,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0.0,
    };
    // SAFETY: null or a time zone.
    let start =
        unsafe { corefoundation::calendar::gregorian_date_get_absolute_time(base, timezone) };
    let offset =
        (or(c.day, 1) - 1) * 86_400 + or(c.hour, 0) * 3600 + or(c.minute, 0) * 60 + or(c.second, 0);
    let at = start + offset as f64 + or(c.nanosecond, 0) as f64 / 1e9;
    crate::timer::date(at)
}

fn install_calendar() {
    let class = objc::class::lookup(c"NSCalendar").expect("registered by calendar_class");
    crate::define::class_methods!(class, {
        "currentCalendar" => calendar_current, c"@16@0:8";
        "autoupdatingCurrentCalendar" => calendar_current, c"@16@0:8";
        "calendarWithIdentifier:" => calendar_with_identifier, c"@24@0:8@16";
    });
    crate::define::methods!(class, {
        "initWithCalendarIdentifier:" => calendar_init_with_identifier, c"@24@0:8@16";
        "calendarIdentifier" => calendar_identifier, c"@16@0:8";
        "timeZone" => calendar_time_zone, c"@16@0:8";
        "setTimeZone:" => calendar_set_time_zone, c"v24@0:8@16";
        "firstWeekday" => calendar_first_weekday, c"Q16@0:8";
        "setFirstWeekday:" => calendar_set_first_weekday, c"v24@0:8Q16";
        "locale" => calendar_locale, c"@16@0:8";
        "setLocale:" => calendar_set_locale, c"v24@0:8@16";
        "components:fromDate:" => calendar_components_from_date, c"@32@0:8Q16@24";
        "dateFromComponents:" => calendar_date_from_components, c"@24@0:8@16";
    });
}

/// Register the three classes, so that a guest binding `_OBJC_CLASS_$_NSCalendar`
/// gets the same class this module allocates from.
pub fn install() {
    number_class();
    date_class();
    calendar_class();
    install_date_components();
    install_calendar();
    objc_api::install();
    number_api::install();
}

/// The C entry points, and the property keys that go with them.
pub mod cf {
    #![allow(non_snake_case)]

    use corefoundation::base::{CFIndex, CFTypeRef};
    use objc::object::Id;

    use super::{Calendar, DateFormat, MONTHS, NumberFormat, WEEKDAYS};

    /// `CFNumberFormatterRef CFNumberFormatterCreate(CFAllocatorRef,
    /// CFLocaleRef, CFNumberFormatterStyle)`
    ///
    /// The locale is not kept: there is one locale in this process - see
    /// `crate::locale` - so a formatter made for it and a formatter made for the
    /// current one are the same formatter.
    unsafe extern "C" fn CFNumberFormatterCreate(
        _allocator: CFTypeRef,
        _locale: CFTypeRef,
        style: i32,
    ) -> CFTypeRef {
        super::make(
            super::number_class(),
            NumberFormat {
                style,
                ..Default::default()
            },
        )
    }

    /// `void CFNumberFormatterSetProperty(CFNumberFormatterRef, CFStringRef key,
    /// CFTypeRef value)`
    ///
    /// The currency symbol is the one property this honours, because it is the
    /// one a caller sets: a price in a currency other than the locale's cannot be
    /// written without it. Any other key is ignored rather than refused - a
    /// formatter setting this tree does not model is a difference in the output,
    /// not a reason to stop the guest.
    ///
    /// # Safety
    ///
    /// Each argument is null or an object this runtime allocated.
    unsafe extern "C" fn CFNumberFormatterSetProperty(
        formatter: CFTypeRef,
        key: CFTypeRef,
        value: CFTypeRef,
    ) {
        let Some(format) =
            (unsafe { super::contents::<NumberFormat>(formatter, super::number_class()) })
        else {
            return;
        };
        // SAFETY: as above - an `NSString` or nil.
        let key = unsafe { crate::string::bytes(key as Id) };
        if key.as_slice() != CURRENCY_SYMBOL.as_bytes() {
            return;
        }
        let text = unsafe { crate::string::bytes(value as Id) };
        format.currency_symbol = String::from_utf8(text).ok();
    }

    /// `CFTypeRef CFNumberFormatterCopyProperty(CFNumberFormatterRef,
    /// CFStringRef key)`
    ///
    /// The properties this formatter models; null for the rest, as for a key
    /// the formatter has no value for.
    ///
    /// # Safety
    ///
    /// Each argument is null or an object this runtime allocated.
    unsafe extern "C" fn CFNumberFormatterCopyProperty(
        formatter: CFTypeRef,
        key: CFTypeRef,
    ) -> CFTypeRef {
        let Some(format) =
            (unsafe { super::contents::<NumberFormat>(formatter, super::number_class()) })
        else {
            return std::ptr::null();
        };
        // SAFETY: as above - an `NSString` or nil.
        let key = unsafe { crate::string::bytes(key as Id) };
        match std::str::from_utf8(&key) {
            Ok(GROUPING_SIZE) => corefoundation::collections::new_number(
                corefoundation::collections::Number::Integer(format.grouping_size() as i64),
            ),
            Ok(CURRENCY_SYMBOL) => format
                .currency_symbol
                .as_deref()
                .map_or(std::ptr::null(), |symbol| {
                    crate::string::from_str(symbol) as CFTypeRef
                }),
            _ => std::ptr::null(),
        }
    }

    /// `CFStringRef CFNumberFormatterCreateStringWithNumber(CFAllocatorRef,
    /// CFNumberFormatterRef, CFNumberRef)`
    ///
    /// # Safety
    ///
    /// Each argument is null or an object this runtime allocated.
    unsafe extern "C" fn CFNumberFormatterCreateStringWithNumber(
        _allocator: CFTypeRef,
        formatter: CFTypeRef,
        number: CFTypeRef,
    ) -> CFTypeRef {
        let Some(format) =
            (unsafe { super::contents::<NumberFormat>(formatter, super::number_class()) })
        else {
            return std::ptr::null();
        };
        // SAFETY: as above - a `CFNumber` or nil.
        let Some(value) = (unsafe { corefoundation::collections::number(number) }) else {
            return std::ptr::null();
        };
        crate::string::from_str(&format.format(value.as_f64(), !value.is_float())) as CFTypeRef
    }

    /// `CFStringRef CFNumberFormatterCreateStringWithValue(CFAllocatorRef,
    /// CFNumberFormatterRef, CFNumberType, const void *valuePtr)`
    ///
    /// The value is read according to `kind`, which is the whole interface: the
    /// caller passes a pointer to a `double` or an `int` and says which.
    ///
    /// # Safety
    ///
    /// `value` points to a live value of the width `kind` names.
    unsafe extern "C" fn CFNumberFormatterCreateStringWithValue(
        _allocator: CFTypeRef,
        formatter: CFTypeRef,
        kind: i32,
        value: *const std::ffi::c_void,
    ) -> CFTypeRef {
        let Some(format) =
            (unsafe { super::contents::<NumberFormat>(formatter, super::number_class()) })
        else {
            return std::ptr::null();
        };
        let Some((width, is_float)) = corefoundation::collections::number_shape(kind) else {
            return std::ptr::null();
        };
        if value.is_null() {
            return std::ptr::null();
        }
        let mut bytes = [0u8; 8];
        // SAFETY: the caller states `value` is a live value of this width, and
        // `number_shape` answers at most eight bytes.
        unsafe { std::ptr::copy_nonoverlapping(value as *const u8, bytes.as_mut_ptr(), width) };
        let number = match (is_float, width) {
            (true, 4) => f32::from_le_bytes(bytes[..4].try_into().unwrap()) as f64,
            (true, _) => f64::from_le_bytes(bytes),
            // Sign-extended from its own width, so a negative `int` is a
            // negative number rather than a very large positive one.
            (false, _) => {
                let raw = i64::from_le_bytes(bytes);
                let shift = 64 - width * 8;
                ((raw << shift) >> shift) as f64
            }
        };
        crate::string::from_str(&format.format(number, !is_float)) as CFTypeRef
    }

    /// `CFDateFormatterRef CFDateFormatterCreate(CFAllocatorRef, CFLocaleRef,
    /// CFDateFormatterStyle dateStyle, CFDateFormatterStyle timeStyle)`
    unsafe extern "C" fn CFDateFormatterCreate(
        _allocator: CFTypeRef,
        _locale: CFTypeRef,
        date: i32,
        time: i32,
    ) -> CFTypeRef {
        super::make(super::date_class(), DateFormat::styled(date, time))
    }

    /// `CFStringRef CFDateFormatterGetFormat(CFDateFormatterRef)`
    ///
    /// A `Get`, so the caller does not own it: the pattern is autoreleased rather
    /// than handed over, because a caller that does not release it must not leak
    /// and one that does must not over-release.
    ///
    /// # Safety
    ///
    /// `formatter` is null or an object this runtime allocated.
    unsafe extern "C" fn CFDateFormatterGetFormat(formatter: CFTypeRef) -> CFTypeRef {
        let Some(format) =
            (unsafe { super::contents::<DateFormat>(formatter, super::date_class()) })
        else {
            return std::ptr::null();
        };
        let pattern = crate::string::from_str(&format.pattern()) as CFTypeRef;
        // SAFETY: a fresh object this call owns.
        unsafe { corefoundation::api::CFAutorelease(pattern) }
    }

    /// `CFStringRef CFDateFormatterCreateStringWithDate(CFAllocatorRef,
    /// CFDateFormatterRef, CFDateRef)`
    ///
    /// # Safety
    ///
    /// Each argument is null or an object this runtime allocated.
    unsafe extern "C" fn CFDateFormatterCreateStringWithDate(
        _allocator: CFTypeRef,
        formatter: CFTypeRef,
        date: CFTypeRef,
    ) -> CFTypeRef {
        let Some(format) =
            (unsafe { super::contents::<DateFormat>(formatter, super::date_class()) })
        else {
            return std::ptr::null();
        };
        let Some(when) = crate::timer::interval_since_1970(date as Id) else {
            return std::ptr::null();
        };
        crate::string::from_str(&format.format(when)) as CFTypeRef
    }

    /// `CFStringRef CFDateFormatterCreateStringWithAbsoluteTime(CFAllocatorRef,
    /// CFDateFormatterRef, CFAbsoluteTime)`
    ///
    /// # Safety
    ///
    /// `formatter` is null or an object this runtime allocated.
    unsafe extern "C" fn CFDateFormatterCreateStringWithAbsoluteTime(
        _allocator: CFTypeRef,
        formatter: CFTypeRef,
        at: f64,
    ) -> CFTypeRef {
        let Some(format) =
            (unsafe { super::contents::<DateFormat>(formatter, super::date_class()) })
        else {
            return std::ptr::null();
        };
        let when = at + crate::timer::UNIX_TO_REFERENCE;
        crate::string::from_str(&format.format(when)) as CFTypeRef
    }

    /// `void CFDateFormatterSetFormat(CFDateFormatterRef, CFStringRef)`
    ///
    /// # Safety
    ///
    /// Each argument is null or an object this runtime allocated.
    unsafe extern "C" fn CFDateFormatterSetFormat(formatter: CFTypeRef, pattern: CFTypeRef) {
        let Some(format) =
            (unsafe { super::contents::<DateFormat>(formatter, super::date_class()) })
        else {
            return;
        };
        format.pattern = (!pattern.is_null()).then(|| {
            String::from_utf8_lossy(&unsafe { crate::string::bytes(pattern as Id) }).into_owned()
        });
    }

    /// `CFTypeRef CFDateFormatterCopyProperty(CFDateFormatterRef,
    /// CFDateFormatterKey)`
    ///
    /// The month, weekday and half-day names, and nil for everything else -
    /// including *all* of them when the locale is not an English one, which is
    /// the whole point: see this module's note.
    ///
    /// # Safety
    ///
    /// Each argument is null or an object this runtime allocated.
    unsafe extern "C" fn CFDateFormatterCopyProperty(
        formatter: CFTypeRef,
        key: CFTypeRef,
    ) -> CFTypeRef {
        if unsafe { super::contents::<DateFormat>(formatter, super::date_class()) }.is_none() {
            return std::ptr::null();
        }
        if !crate::locale::is_english() {
            return std::ptr::null();
        }
        // SAFETY: as above - an `NSString` or nil.
        let key = unsafe { crate::string::bytes(key as Id) };
        let Ok(key) = std::str::from_utf8(&key) else {
            return std::ptr::null();
        };
        // The standalone symbols are the form a name is written in on its own
        // rather than inside a date. In English they are the same words, so they
        // answer from the same table.
        let names: Vec<&str> = match key {
            MONTH_SYMBOLS | STANDALONE_MONTH_SYMBOLS => {
                MONTHS.iter().map(|(long, _)| *long).collect()
            }
            SHORT_MONTH_SYMBOLS | SHORT_STANDALONE_MONTH_SYMBOLS => {
                MONTHS.iter().map(|(_, short)| *short).collect()
            }
            WEEKDAY_SYMBOLS | STANDALONE_WEEKDAY_SYMBOLS => {
                WEEKDAYS.iter().map(|(long, _)| *long).collect()
            }
            SHORT_WEEKDAY_SYMBOLS | SHORT_STANDALONE_WEEKDAY_SYMBOLS => {
                WEEKDAYS.iter().map(|(_, short)| *short).collect()
            }
            // The narrow forms, which in English are each name's first letter.
            VERY_SHORT_MONTH_SYMBOLS | VERY_SHORT_STANDALONE_MONTH_SYMBOLS => {
                MONTHS.iter().map(|(long, _)| &long[..1]).collect()
            }
            VERY_SHORT_WEEKDAY_SYMBOLS | VERY_SHORT_STANDALONE_WEEKDAY_SYMBOLS => {
                WEEKDAYS.iter().map(|(long, _)| &long[..1]).collect()
            }
            AM_SYMBOL => return crate::string::from_str("AM") as CFTypeRef,
            PM_SYMBOL => return crate::string::from_str("PM") as CFTypeRef,
            _ => return std::ptr::null(),
        };
        let strings: Vec<CFTypeRef> = names
            .iter()
            .map(|name| crate::string::from_str(name) as CFTypeRef)
            .collect();
        let array = corefoundation::collections::new_array(
            &strings,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        );
        for string in strings {
            // The array retained each; these are this call's own references.
            // SAFETY: a live object made immediately above.
            unsafe { corefoundation::api::CFRelease(string) };
        }
        array
    }

    /// `CFCalendarRef CFCalendarCopyCurrent(void)`
    unsafe extern "C" fn CFCalendarCopyCurrent() -> CFTypeRef {
        super::make(
            super::calendar_class(),
            Calendar {
                first_weekday: super::first_weekday(&crate::locale::country_code()),
                identifier: corefoundation::string::from_bytes(b"gregorian", false),
                timezone: unsafe { corefoundation::timezone::copy_system() },
            },
        )
    }

    /// `CFCalendarRef CFCalendarCreateWithIdentifier(CFAllocatorRef,
    /// CFCalendarIdentifier)`
    ///
    /// Only the Gregorian calendar, and ISO 8601 which is the same arithmetic
    /// with a different week rule. The rest are refused by name rather than
    /// served with Gregorian dates, which would be wrong dates.
    ///
    /// # Safety
    ///
    /// `identifier` is null or a string.
    unsafe extern "C" fn CFCalendarCreateWithIdentifier(
        _allocator: CFTypeRef,
        identifier: CFTypeRef,
    ) -> CFTypeRef {
        let Some(text) = (unsafe { corefoundation::string::as_string(identifier) }) else {
            return std::ptr::null();
        };
        let text = String::from_utf8_lossy(&text.utf8()).into_owned();
        if text != "gregorian" && text != "iso8601" {
            objc::fail!(
                "CFCalendarCreateWithIdentifier: {text} is a calendar this runtime does not \
                 implement, and Gregorian dates would be the wrong dates"
            );
        }
        super::make(
            super::calendar_class(),
            Calendar {
                first_weekday: super::first_weekday(&crate::locale::country_code()),
                identifier: objc::arc::retain(identifier as Id) as CFTypeRef,
                timezone: unsafe { corefoundation::timezone::copy_system() },
            },
        )
    }

    /// `CFCalendarIdentifier CFCalendarGetIdentifier(CFCalendarRef)`
    ///
    /// # Safety
    ///
    /// `calendar` is null or an object this runtime allocated.
    unsafe extern "C" fn CFCalendarGetIdentifier(calendar: CFTypeRef) -> CFTypeRef {
        match unsafe { super::contents::<Calendar>(calendar, super::calendar_class()) } {
            Some(calendar) => calendar.identifier,
            None => std::ptr::null(),
        }
    }

    /// `CFTimeZoneRef CFCalendarCopyTimeZone(CFCalendarRef)`
    ///
    /// # Safety
    ///
    /// As [`CFCalendarGetIdentifier`].
    unsafe extern "C" fn CFCalendarCopyTimeZone(calendar: CFTypeRef) -> CFTypeRef {
        match unsafe { super::contents::<Calendar>(calendar, super::calendar_class()) } {
            Some(calendar) => objc::arc::retain(calendar.timezone as Id) as CFTypeRef,
            None => std::ptr::null(),
        }
    }

    /// `void CFCalendarSetTimeZone(CFCalendarRef, CFTimeZoneRef)`
    ///
    /// # Safety
    ///
    /// As [`CFCalendarGetIdentifier`]; `timezone` is null or a time zone.
    unsafe extern "C" fn CFCalendarSetTimeZone(calendar: CFTypeRef, timezone: CFTypeRef) {
        let Some(calendar) =
            (unsafe { super::contents::<Calendar>(calendar, super::calendar_class()) })
        else {
            return;
        };
        let held = objc::arc::retain(timezone as Id) as CFTypeRef;
        objc::arc::release(calendar.timezone as Id);
        calendar.timezone = held;
    }

    /// `CFIndex CFCalendarGetFirstWeekday(CFCalendarRef)`
    ///
    /// One-based with Sunday as 1, which is the numbering the whole calendar API
    /// uses. Sunday for a calendar this tree did not make, because a zero here is
    /// not a weekday at all and a caller would use it as one.
    ///
    /// # Safety
    ///
    /// `calendar` is null or an object this runtime allocated.
    unsafe extern "C" fn CFCalendarGetFirstWeekday(calendar: CFTypeRef) -> CFIndex {
        match unsafe { super::contents::<Calendar>(calendar, super::calendar_class()) } {
            Some(calendar) => calendar.first_weekday,
            None => 1,
        }
    }

    /// How many components either of the two below will carry.
    ///
    /// The variadic half of these calls is *not* written as a C variadic
    /// function: Rust has no stable way to declare one, and the trampolines
    /// `darwin::printf_l` needs for `snprintf_l` are one architecture's
    /// assembly each. What is used instead is that a variadic call and a call
    /// to a function of the same fixed arity place their integer arguments in
    /// exactly the same places on every ABI this tree runs on that puts
    /// variadic arguments in the ordinary argument registers - so the
    /// declaration below simply names eight of them, and only the ones the
    /// component description asks for are ever read.
    ///
    /// Apple's arm64 is the ABI where that is not true: it passes every
    /// variadic argument on the stack, whatever the registers hold. These two
    /// entry points are therefore not published there, where the host's own
    /// CoreFoundation is the one a guest is using anyway.
    ///
    /// A description asking for more than this many components is refused by
    /// name rather than served from arguments the declaration did not cover.
    #[cfg(not(target_vendor = "apple"))]
    const MAX_COMPONENTS: usize = 8;

    /// The component a `CFCalendar` description character names, as a value
    /// read out of a date - or `None` for one this tree does not compute.
    ///
    /// The numbering is CoreFoundation's: a weekday is one-based with Sunday
    /// as one, which is `CFCalendarGetFirstWeekday`'s numbering and *not*
    /// `CFAbsoluteTimeGetDayOfWeek`'s.
    #[cfg(not(target_vendor = "apple"))]
    fn component(
        which: u8,
        date: &corefoundation::calendar::GregorianDate,
        at: f64,
        timezone: CFTypeRef,
    ) -> Option<i32> {
        Some(match which {
            b'G' => 1,
            b'y' => date.year,
            b'M' => i32::from(date.month),
            b'd' => i32::from(date.day),
            b'H' => i32::from(date.hour),
            b'm' => i32::from(date.minute),
            b's' => date.second as i32,
            // SAFETY: null or a time zone.
            b'E' => unsafe {
                corefoundation::calendar::absolute_time_get_day_of_week(at, timezone) % 7 + 1
            },
            b'D' => unsafe {
                corefoundation::calendar::absolute_time_get_day_of_year(at, timezone)
            },
            _ => return None,
        })
    }

    /// The zone a calendar places its dates in.
    #[cfg(not(target_vendor = "apple"))]
    unsafe fn zone_of(calendar: CFTypeRef) -> CFTypeRef {
        match unsafe { super::contents::<Calendar>(calendar, super::calendar_class()) } {
            Some(calendar) => calendar.timezone,
            None => std::ptr::null(),
        }
    }

    /// The description, as the bytes this walk understands.
    ///
    /// A `?` marks the component that follows as optional, which only matters
    /// to `CFCalendarComposeAbsoluteTime`; both walks skip it here and the
    /// compose treats a missing value as zero, which is what optional means.
    #[cfg(not(target_vendor = "apple"))]
    unsafe fn components(description: *const std::ffi::c_char) -> Option<Vec<u8>> {
        if description.is_null() {
            return None;
        }
        // SAFETY: the caller states it is a C string.
        let text = unsafe { std::ffi::CStr::from_ptr(description) };
        Some(
            text.to_bytes()
                .iter()
                .copied()
                .filter(|byte| *byte != b'?')
                .collect(),
        )
    }

    /// `Boolean CFCalendarDecomposeAbsoluteTime(CFCalendarRef, CFAbsoluteTime,
    /// const char *componentDesc, ...)`
    ///
    /// # Safety
    ///
    /// `description` is a C string, and the caller passed one writable `int *`
    /// for each component it names.
    #[cfg(not(target_vendor = "apple"))]
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn CFCalendarDecomposeAbsoluteTime(
        calendar: CFTypeRef,
        at: f64,
        description: *const std::ffi::c_char,
        out0: *mut i32,
        out1: *mut i32,
        out2: *mut i32,
        out3: *mut i32,
        out4: *mut i32,
        out5: *mut i32,
        out6: *mut i32,
        out7: *mut i32,
    ) -> u8 {
        let Some(wanted) = (unsafe { components(description) }) else {
            return 0;
        };
        if wanted.len() > MAX_COMPONENTS {
            objc::fail!(
                "CFCalendarDecomposeAbsoluteTime: {} components is more than the {MAX_COMPONENTS} \
                 this runtime reads out of a variadic call",
                wanted.len()
            );
        }
        // SAFETY: a calendar this runtime made.
        let timezone = unsafe { zone_of(calendar) };
        // SAFETY: null or a time zone this tree made.
        let date =
            unsafe { corefoundation::calendar::absolute_time_get_gregorian_date(at, timezone) };
        let outs = [out0, out1, out2, out3, out4, out5, out6, out7];
        for (which, out) in wanted.iter().zip(outs) {
            let Some(value) = component(*which, &date, at, timezone) else {
                objc::fail!(
                    "CFCalendarDecomposeAbsoluteTime: component '{}' is not one this runtime \
                     computes",
                    *which as char
                );
            };
            if out.is_null() {
                continue;
            }
            // SAFETY: the caller passed one writable `int *` per component.
            unsafe { *out = value };
        }
        1
    }

    /// `Boolean CFCalendarComposeAbsoluteTime(CFCalendarRef, CFAbsoluteTime *,
    /// const char *componentDesc, ...)`
    ///
    /// # Safety
    ///
    /// `at` is writable, `description` is a C string, and the caller passed one
    /// `int` for each component it names.
    #[cfg(not(target_vendor = "apple"))]
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn CFCalendarComposeAbsoluteTime(
        calendar: CFTypeRef,
        at: *mut f64,
        description: *const std::ffi::c_char,
        in0: i32,
        in1: i32,
        in2: i32,
        in3: i32,
        in4: i32,
        in5: i32,
        in6: i32,
        in7: i32,
    ) -> u8 {
        let Some(wanted) = (unsafe { components(description) }) else {
            return 0;
        };
        if at.is_null() {
            return 0;
        }
        if wanted.len() > MAX_COMPONENTS {
            objc::fail!(
                "CFCalendarComposeAbsoluteTime: {} components is more than the {MAX_COMPONENTS} \
                 this runtime reads out of a variadic call",
                wanted.len()
            );
        }
        let ins = [in0, in1, in2, in3, in4, in5, in6, in7];
        // The day and the month start at one, so a description that names
        // neither composes the first of January rather than a zeroth day.
        let mut date = corefoundation::calendar::GregorianDate {
            year: 2001,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0.0,
        };
        for (which, value) in wanted.iter().zip(ins) {
            match which {
                b'y' => date.year = value,
                b'M' => date.month = value as i8,
                b'd' => date.day = value as i8,
                b'H' => date.hour = value as i8,
                b'm' => date.minute = value as i8,
                b's' => date.second = f64::from(value),
                // The era, which is one for every date this tree places.
                b'G' => {}
                other => objc::fail!(
                    "CFCalendarComposeAbsoluteTime: component '{}' is not one this runtime \
                     composes from",
                    *other as char
                ),
            }
        }
        // SAFETY: a calendar this runtime made.
        let timezone = unsafe { zone_of(calendar) };
        // SAFETY: null or a time zone this tree made; `at` is writable.
        unsafe {
            *at = corefoundation::calendar::gregorian_date_get_absolute_time(date, timezone);
        }
        1
    }

    /// `CFStringRef CFLocaleCopyDisplayNameForPropertyValue(CFLocaleRef,
    /// CFLocaleKey, CFStringRef value)`
    ///
    /// Nil, always, and deliberately: a display name is the *translated* name of
    /// a language or a country - "Deutsch" for `de`, "États-Unis" for `US` - and
    /// that is CLDR data this tree does not carry. Nil is the documented answer
    /// for a value the locale cannot name, and a caller falls back to its own
    /// tables; an English name returned for a French locale would not be a
    /// fallback, it would be a wrong answer the caller cannot detect.
    unsafe extern "C" fn CFLocaleCopyDisplayNameForPropertyValue(
        _locale: CFTypeRef,
        _key: CFTypeRef,
        _value: CFTypeRef,
    ) -> CFTypeRef {
        std::ptr::null()
    }

    /// `CFNumberFormatterKey`/`CFDateFormatterKey` names. Each is its own symbol
    /// without the leading `k`, which is how CoreFoundation spells them.
    const CURRENCY_SYMBOL: &str = "kCFNumberFormatterCurrencySymbol";
    const GROUPING_SIZE: &str = "kCFNumberFormatterGroupingSize";
    const VERY_SHORT_MONTH_SYMBOLS: &str = "kCFDateFormatterVeryShortMonthSymbols";
    const VERY_SHORT_STANDALONE_MONTH_SYMBOLS: &str =
        "kCFDateFormatterVeryShortStandaloneMonthSymbols";
    const VERY_SHORT_WEEKDAY_SYMBOLS: &str = "kCFDateFormatterVeryShortWeekdaySymbols";
    const VERY_SHORT_STANDALONE_WEEKDAY_SYMBOLS: &str =
        "kCFDateFormatterVeryShortStandaloneWeekdaySymbols";
    const MONTH_SYMBOLS: &str = "kCFDateFormatterMonthSymbols";
    const SHORT_MONTH_SYMBOLS: &str = "kCFDateFormatterShortMonthSymbols";
    const STANDALONE_MONTH_SYMBOLS: &str = "kCFDateFormatterStandaloneMonthSymbols";
    const SHORT_STANDALONE_MONTH_SYMBOLS: &str = "kCFDateFormatterShortStandaloneMonthSymbols";
    const WEEKDAY_SYMBOLS: &str = "kCFDateFormatterWeekdaySymbols";
    const SHORT_WEEKDAY_SYMBOLS: &str = "kCFDateFormatterShortWeekdaySymbols";
    const STANDALONE_WEEKDAY_SYMBOLS: &str = "kCFDateFormatterStandaloneWeekdaySymbols";
    const SHORT_STANDALONE_WEEKDAY_SYMBOLS: &str = "kCFDateFormatterShortStandaloneWeekdaySymbols";
    const AM_SYMBOL: &str = "kCFDateFormatterAMSymbol";
    const PM_SYMBOL: &str = "kCFDateFormatterPMSymbol";

    /// The property-key globals, as `(Mach-O name, value)`.
    ///
    /// Every one is a `CFStringRef` **variable**, so each goes through a cell -
    /// an unresolved one binds to zero and the guest passes address zero as the
    /// key. The value is the key's own name, which is what CoreFoundation's is.
    pub const KEYS: &[(&str, &str)] = &[
        ("_kCFNumberFormatterCurrencySymbol", CURRENCY_SYMBOL),
        ("_kCFNumberFormatterGroupingSize", GROUPING_SIZE),
        (
            "_kCFDateFormatterVeryShortMonthSymbols",
            VERY_SHORT_MONTH_SYMBOLS,
        ),
        (
            "_kCFDateFormatterVeryShortStandaloneMonthSymbols",
            VERY_SHORT_STANDALONE_MONTH_SYMBOLS,
        ),
        (
            "_kCFDateFormatterVeryShortWeekdaySymbols",
            VERY_SHORT_WEEKDAY_SYMBOLS,
        ),
        (
            "_kCFDateFormatterVeryShortStandaloneWeekdaySymbols",
            VERY_SHORT_STANDALONE_WEEKDAY_SYMBOLS,
        ),
        ("_kCFDateFormatterMonthSymbols", MONTH_SYMBOLS),
        ("_kCFDateFormatterShortMonthSymbols", SHORT_MONTH_SYMBOLS),
        (
            "_kCFDateFormatterStandaloneMonthSymbols",
            STANDALONE_MONTH_SYMBOLS,
        ),
        (
            "_kCFDateFormatterShortStandaloneMonthSymbols",
            SHORT_STANDALONE_MONTH_SYMBOLS,
        ),
        ("_kCFDateFormatterWeekdaySymbols", WEEKDAY_SYMBOLS),
        (
            "_kCFDateFormatterShortWeekdaySymbols",
            SHORT_WEEKDAY_SYMBOLS,
        ),
        (
            "_kCFDateFormatterStandaloneWeekdaySymbols",
            STANDALONE_WEEKDAY_SYMBOLS,
        ),
        (
            "_kCFDateFormatterShortStandaloneWeekdaySymbols",
            SHORT_STANDALONE_WEEKDAY_SYMBOLS,
        ),
        ("_kCFDateFormatterAMSymbol", AM_SYMBOL),
        ("_kCFDateFormatterPMSymbol", PM_SYMBOL),
    ];

    exports::exports! {
        functions {
            "CFNumberFormatterCreate" => CFNumberFormatterCreate,
            "CFNumberFormatterSetProperty" => CFNumberFormatterSetProperty,
            "CFNumberFormatterCopyProperty" => CFNumberFormatterCopyProperty,
            "CFNumberFormatterCreateStringWithNumber" => CFNumberFormatterCreateStringWithNumber,
            "CFNumberFormatterCreateStringWithValue" => CFNumberFormatterCreateStringWithValue,
            "CFDateFormatterCreate" => CFDateFormatterCreate,
            "CFDateFormatterGetFormat" => CFDateFormatterGetFormat,
            "CFDateFormatterCreateStringWithDate" => CFDateFormatterCreateStringWithDate,
            "CFDateFormatterCopyProperty" => CFDateFormatterCopyProperty,
            "CFDateFormatterCreateStringWithAbsoluteTime" => CFDateFormatterCreateStringWithAbsoluteTime,
            "CFDateFormatterSetFormat" => CFDateFormatterSetFormat,
            "CFCalendarCopyCurrent" => CFCalendarCopyCurrent,
            "CFCalendarCreateWithIdentifier" => CFCalendarCreateWithIdentifier,
            "CFCalendarGetIdentifier" => CFCalendarGetIdentifier,
            "CFCalendarCopyTimeZone" => CFCalendarCopyTimeZone,
            "CFCalendarSetTimeZone" => CFCalendarSetTimeZone,
            #[cfg(not(target_vendor = "apple"))]
            "CFCalendarDecomposeAbsoluteTime" => CFCalendarDecomposeAbsoluteTime,
            #[cfg(not(target_vendor = "apple"))]
            "CFCalendarComposeAbsoluteTime" => CFCalendarComposeAbsoluteTime,
            "CFCalendarGetFirstWeekday" => CFCalendarGetFirstWeekday,
            "CFLocaleCopyDisplayNameForPropertyValue" => CFLocaleCopyDisplayNameForPropertyValue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_allocated_date_components_answers_its_setters() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSDateComponents").unwrap();
        let object = send_id(objc::class::ptr(class) as Id, "alloc");
        let object = send_id(object, "init");
        // SAFETY: `-setYear:` is `void (id, SEL, NSInteger)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, i64) = std::mem::transmute(objc::msg::entry());
            f(object, objc::sel::get("setYear:"), 2024);
        }
        let year = unsafe { dc_get_year(object, std::ptr::null()) };
        assert_eq!(year, 2024);
        objc::arc::release(object);
    }

    #[test]
    fn a_calendar_decomposes_and_recomposes_a_utc_date() {
        crate::bootstrap();
        let calendar = unsafe { calendar_current(std::ptr::null_mut(), std::ptr::null()) };
        let name = corefoundation::string::from_bytes(b"UTC", false);
        let utc = unsafe { corefoundation::timezone::create_with_name(std::ptr::null(), name, 0) };
        unsafe { calendar_set_time_zone(calendar, std::ptr::null(), utc as Id) };
        // 2024-02-29 13:14:15 UTC.
        let at = 730_905_255.0;
        let date = crate::timer::date(at);
        let units = unit::YEAR
            | unit::MONTH
            | unit::DAY
            | unit::HOUR
            | unit::MINUTE
            | unit::SECOND
            | unit::WEEKDAY;
        let components =
            unsafe { calendar_components_from_date(calendar, std::ptr::null(), units, date) };
        let c =
            unsafe { contents::<DateComponents>(components as CFTypeRef, date_components_class()) }
                .unwrap();
        assert_eq!(
            (c.year, c.month, c.day, c.hour, c.minute, c.second),
            (2024, 2, 29, 13, 14, 15)
        );
        // A Thursday, five with Sunday as one.
        assert_eq!(c.weekday, 5);
        assert_eq!(c.era, UNDEFINED);
        let back = unsafe { calendar_date_from_components(calendar, std::ptr::null(), components) };
        let interval = send_f64(back, "timeIntervalSinceReferenceDate");
        assert_eq!(interval, at);
    }

    fn send_f64(receiver: Id, selector: &str) -> f64 {
        // SAFETY: a `double (id, SEL)` method.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> f64 = std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get(selector))
        }
    }

    #[test]
    fn a_decimal_style_groups_by_thousands() {
        let format = NumberFormat {
            style: number_style::DECIMAL,
            ..Default::default()
        };
        assert_eq!(format.format(1234567.0, true), "1,234,567");
        assert_eq!(format.format(-1234.0, true), "-1,234");
        assert_eq!(format.format(999.0, true), "999");
    }

    /// The case Qt uses to ask "which character is the digit zero here": no
    /// style, the integer zero, and the answer has to be exactly one digit.
    #[test]
    fn no_style_writes_a_bare_number() {
        let format = NumberFormat {
            style: number_style::NONE,
            ..Default::default()
        };
        assert_eq!(format.format(0.0, true), "0");
        assert_eq!(format.format(1234.0, true), "1234", "no grouping separator");
    }

    #[test]
    fn a_currency_keeps_two_places_and_its_symbol() {
        let mut format = NumberFormat {
            style: number_style::CURRENCY,
            ..Default::default()
        };
        assert_eq!(format.format(1234.5, false), "$1,234.50");
        assert_eq!(format.format(-2.0, true), "-$2.00");
        format.currency_symbol = Some("\u{a3}".to_string());
        assert_eq!(format.format(3.0, true), "\u{a3}3.00");
        format.style = number_style::CURRENCY_ACCOUNTING;
        assert_eq!(format.format(-3.0, true), "(\u{a3}3.00)");
    }

    /// The patterns are what a caller parses, so their shape is the interface.
    #[test]
    fn the_patterns_name_each_style() {
        let pattern = |date, time| DateFormat::styled(date, time).pattern();
        assert_eq!(pattern(date_style::SHORT, date_style::NONE), "M/d/yy");
        assert_eq!(
            pattern(date_style::FULL, date_style::NONE),
            "EEEE, MMMM d, y"
        );
        assert_eq!(pattern(date_style::NONE, date_style::SHORT), "h:mm a");
        assert_eq!(
            pattern(date_style::MEDIUM, date_style::SHORT),
            "MMM d, y, h:mm a"
        );
        assert_eq!(pattern(date_style::NONE, date_style::NONE), "");
    }

    #[test]
    fn a_pattern_writes_each_field_in_place() {
        // 2001-02-03 04:05:06.789 UTC.
        let when = 981_173_106.789;
        assert_eq!(
            render_pattern("yyyy-MM-dd_HH:mm:ss.SSS", when, 0),
            "2001-02-03_04:05:06.789"
        );
        assert_eq!(
            render_pattern("EEE, d MMM yy h a", when, 0),
            "Sat, 3 Feb 01 4 AM"
        );
        assert_eq!(
            render_pattern("'at' HH 'o''clock' Z", when, 3600),
            "at 05 o'clock +0100"
        );
    }

    /// Midnight and noon are the two a twelve-hour clock gets wrong.
    #[test]
    fn twelve_hour_time_names_midnight_and_noon() {
        // 1970-01-01 in UTC, so the test is about the arithmetic rather than
        // about this host's time zone.
        let format = DateFormat::styled(date_style::NONE, date_style::SHORT);
        let midnight = civil(0.0);
        let hour = match midnight.tm_hour % 12 {
            0 => 12,
            other => other,
        };
        assert!((1..=12).contains(&hour), "a twelve-hour clock has no zero");
        // And the formatter never writes an hour outside that range.
        for step in 0..24 {
            let text = format.format(step as f64 * 3600.0);
            let written: i32 = text.split(':').next().unwrap().parse().unwrap();
            assert!((1..=12).contains(&written), "{text}");
            assert!(text.ends_with(" AM") || text.ends_with(" PM"), "{text}");
        }
    }

    #[test]
    fn a_week_starts_on_sunday_in_the_states_and_monday_in_europe() {
        assert_eq!(first_weekday("US"), 1);
        assert_eq!(first_weekday("DE"), 2);
        assert_eq!(first_weekday("FR"), 2);
        assert_eq!(first_weekday("JP"), 1);
    }
}
