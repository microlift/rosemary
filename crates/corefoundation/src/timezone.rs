use std::ffi::c_void;

use objc::class::Class;
use objc::object::Id;

use crate::base::CFTypeRef;

#[repr(C)]
struct Body {
    isa: *const Class,
    name: CFTypeRef,
}

fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        let base = crate::base::abstract_class(c"NSTimeZone", None);
        crate::base::note_type_class(base);
        let class = crate::base::register(c"__NSCFTimeZone", Some(base), size_of::<Body>(), false);
        class.replace_method(
            objc::sel::get("dealloc"),
            dealloc as *const c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("a timezone class") }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: objc::sel::Sel) {
    // SAFETY: this class's object layout and its owned name.
    unsafe {
        let body = receiver as *mut Body;
        objc::arc::release((*body).name as Id);
        objc::arc::finalize(receiver);
    }
}

fn new(name: &str) -> CFTypeRef {
    // SAFETY: `Body` is the registered layout.
    let body = unsafe { crate::base::allocate::<Body>(class()) };
    if body.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a freshly allocated body.
    unsafe { (*body).name = crate::string::from_bytes(name.as_bytes(), false) };
    body as CFTypeRef
}

/// The directories a zoneinfo database is found in, in the order `tzset` itself
/// looks: `TZDIR` overrides, and the rest are where the distributions put it -
/// NixOS uses `/etc/zoneinfo`, most others `/usr/share/zoneinfo`.
pub fn database() -> Option<std::path::PathBuf> {
    std::env::var("TZDIR")
        .ok()
        .filter(|dir| !dir.is_empty())
        .map(std::path::PathBuf::from)
        .into_iter()
        .chain(
            ["/usr/share/zoneinfo", "/etc/zoneinfo", "/usr/lib/zoneinfo"]
                .into_iter()
                .map(std::path::PathBuf::from),
        )
        .find(|dir| dir.is_dir())
}

/// The zone called `name`, read from the host's database once and kept, or
/// `None` for a name the host does not have.
///
/// The system zone falls back to `/etc/localtime`, which is where the host's
/// zone lives even when its name is not in a database this can find; UTC and
/// GMT are answered without one.
pub fn zone(name: &str) -> Option<std::sync::Arc<crate::tzif::Zone>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    static ZONES: Mutex<Option<HashMap<String, Option<Arc<crate::tzif::Zone>>>>> = Mutex::new(None);

    let mut zones = ZONES.lock().unwrap_or_else(|e| e.into_inner());
    let zones = zones.get_or_insert_default();
    if let Some(found) = zones.get(name) {
        return found.clone();
    }
    // A name is a relative path inside the database and nothing else.
    let valid = !name.is_empty()
        && !name.starts_with('/')
        && !name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..");
    let read = |path: std::path::PathBuf| {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| crate::tzif::Zone::parse(&bytes))
    };
    let found = valid
        .then(|| database().and_then(|root| read(root.join(name))))
        .flatten()
        .or_else(|| {
            (name == system_name())
                .then(|| read("/etc/localtime".into()))
                .flatten()
        })
        .or_else(|| match name {
            "UTC" | "GMT" | "Etc/UTC" | "Etc/GMT" => Some(crate::tzif::Zone::fixed(0, name)),
            _ => offset_of_fixed_name(name).map(|offset| crate::tzif::Zone::fixed(offset, name)),
        })
        .map(Arc::new);
    zones.insert(name.to_owned(), found.clone());
    found
}

/// The largest offset CFTimeZone accepts for a fixed zone, either side of GMT.
const MAX_FIXED_OFFSET: i64 = 18 * 3600;

/// The name CFTimeZone gives a fixed-offset zone: `GMT`, or `GMT+HHMM` with
/// `SS` appended only when the offset has seconds.
fn fixed_name(offset: i64) -> String {
    if offset == 0 {
        return "GMT".to_owned();
    }
    let sign = if offset < 0 { '-' } else { '+' };
    let magnitude = offset.unsigned_abs();
    let (hours, minutes, seconds) = (magnitude / 3600, magnitude / 60 % 60, magnitude % 60);
    match seconds {
        0 => format!("GMT{sign}{hours:02}{minutes:02}"),
        _ => format!("GMT{sign}{hours:02}{minutes:02}{seconds:02}"),
    }
}

/// The offset a [`fixed_name`] stands for, so such a zone resolves by name.
fn offset_of_fixed_name(name: &str) -> Option<i64> {
    let rest = name.strip_prefix("GMT")?;
    let (sign, digits) = match rest.as_bytes().first()? {
        b'+' => (1, &rest[1..]),
        b'-' => (-1, &rest[1..]),
        _ => return None,
    };
    if !matches!(digits.len(), 4 | 6) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let field = |range: std::ops::Range<usize>| digits.get(range)?.parse::<i64>().ok();
    let (hours, minutes) = (field(0..2)?, field(2..4)?);
    let seconds = if digits.len() == 6 { field(4..6)? } else { 0 };
    if minutes >= 60 || seconds >= 60 {
        return None;
    }
    let offset = sign * (hours * 3600 + minutes * 60 + seconds);
    (offset.abs() <= MAX_FIXED_OFFSET && fixed_name(offset) == name).then_some(offset)
}

/// `CFTimeZoneRef CFTimeZoneCreateWithTimeIntervalFromGMT(CFAllocatorRef, CFTimeInterval)`:
/// a zone always `interval` seconds from GMT, or NULL beyond +-18 hours.
pub unsafe extern "C" fn create_with_time_interval_from_gmt(
    _allocator: CFTypeRef,
    interval: f64,
) -> CFTypeRef {
    let bound = MAX_FIXED_OFFSET as f64;
    if !(-bound..=bound).contains(&interval) {
        return std::ptr::null();
    }
    // CF drops the fraction of a second, toward zero.
    new(&fixed_name(interval.trunc() as i64))
}

/// The zone this host is set to, as an IANA name.
pub fn system_name() -> String {
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|path| {
            let text = path.to_string_lossy();
            text.split("/zoneinfo/").nth(1).map(str::to_owned)
        })
        .or_else(|| std::env::var("TZ").ok().filter(|name| !name.is_empty()))
        .unwrap_or_else(|| "UTC".to_string())
}

pub unsafe extern "C" fn copy_default() -> CFTypeRef {
    new(&system_name())
}

pub unsafe extern "C" fn copy_system() -> CFTypeRef {
    new(&system_name())
}

/// CFTimeZone's default abbreviation dictionary, which is what
/// `+[NSTimeZone abbreviationDictionary]` starts as on a Mac.
const ABBREVIATIONS: &[(&str, &str)] = &[
    ("ADT", "America/Halifax"),
    ("AKDT", "America/Juneau"),
    ("AKST", "America/Juneau"),
    ("ART", "America/Argentina/Buenos_Aires"),
    ("AST", "America/Halifax"),
    ("BDT", "Asia/Dhaka"),
    ("BRST", "America/Sao_Paulo"),
    ("BRT", "America/Sao_Paulo"),
    ("BST", "Europe/London"),
    ("CAT", "Africa/Harare"),
    ("CDT", "America/Chicago"),
    ("CEST", "Europe/Paris"),
    ("CET", "Europe/Paris"),
    ("CLST", "America/Santiago"),
    ("CLT", "America/Santiago"),
    ("COT", "America/Bogota"),
    ("CST", "America/Chicago"),
    ("EAT", "Africa/Addis_Ababa"),
    ("EDT", "America/New_York"),
    ("EEST", "Europe/Athens"),
    ("EET", "Europe/Athens"),
    ("EST", "America/New_York"),
    ("GMT", "GMT"),
    ("GST", "Asia/Dubai"),
    ("HKT", "Asia/Hong_Kong"),
    ("HST", "Pacific/Honolulu"),
    ("ICT", "Asia/Bangkok"),
    ("IRST", "Asia/Tehran"),
    ("IST", "Asia/Kolkata"),
    ("JST", "Asia/Tokyo"),
    ("KST", "Asia/Seoul"),
    ("MDT", "America/Denver"),
    ("MSD", "Europe/Moscow"),
    ("MSK", "Europe/Moscow"),
    ("MST", "America/Phoenix"),
    ("NZDT", "Pacific/Auckland"),
    ("NZST", "Pacific/Auckland"),
    ("PDT", "America/Los_Angeles"),
    ("PET", "America/Lima"),
    ("PHT", "Asia/Manila"),
    ("PKT", "Asia/Karachi"),
    ("PST", "America/Los_Angeles"),
    ("SGT", "Asia/Singapore"),
    ("TRT", "Europe/Istanbul"),
    ("UTC", "UTC"),
    ("WAT", "Africa/Lagos"),
    ("WEST", "Europe/Lisbon"),
    ("WET", "Europe/Lisbon"),
    ("WIT", "Asia/Jakarta"),
];

/// The zone name an abbreviation stands for, per [`ABBREVIATIONS`].
pub fn name_for_abbreviation(abbreviation: &str) -> Option<&'static str> {
    ABBREVIATIONS
        .iter()
        .find(|(short, _)| *short == abbreviation)
        .map(|(_, name)| *name)
}

pub unsafe extern "C" fn create_with_name(
    _allocator: CFTypeRef,
    name: CFTypeRef,
    try_abbreviation: u8,
) -> CFTypeRef {
    // SAFETY: a string or null.
    let Some(name) = (unsafe { crate::string::as_string(name) }) else {
        return std::ptr::null();
    };
    let mut name = String::from_utf8_lossy(&name.utf8()).into_owned();
    if try_abbreviation != 0
        && let Some(full) = name_for_abbreviation(&name)
    {
        name = full.to_owned();
    }
    // A name the host has no zone for is NULL, as it is on a Mac, rather than
    // an object every later question about would have to refuse.
    match zone(&name) {
        Some(_) => new(&name),
        None => std::ptr::null(),
    }
}

/// The zone's name as Rust text, or `None` for anything that is not one of
/// this tree's time zones - a null included, which the calendar reads as UTC.
///
/// # Safety
///
/// `timezone` is null or an object.
pub unsafe fn name_of(timezone: CFTypeRef) -> Option<String> {
    if !unsafe { crate::base::is_kind_of(timezone, class()) } {
        return None;
    }
    // SAFETY: an instance of `class()` has this layout.
    let name = unsafe { (*(timezone as *const Body)).name };
    // SAFETY: the name is a string this module made.
    let text = unsafe { crate::string::as_string(name) }?;
    Some(String::from_utf8_lossy(&text.utf8()).into_owned())
}

pub unsafe extern "C" fn get_name(timezone: CFTypeRef) -> CFTypeRef {
    // SAFETY: a timezone from this runtime.
    if !unsafe { crate::base::is_kind_of(timezone, class()) } {
        return std::ptr::null();
    }
    // SAFETY: an instance of `class()` has this layout.
    unsafe { (*(timezone as *const Body)).name }
}
