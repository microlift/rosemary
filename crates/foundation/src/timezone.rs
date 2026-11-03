use corefoundation::timezone::database;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// Whether a path relative to the database root names a zone.
///
/// A zone is `Area/Location`, which is what having a directory component says.
/// The database also holds the compiled-rule subtrees, the `.tab` and `.zi`
/// sources and a handful of generated files, none of which are zones a guest
/// should show a user; `UTC` and `GMT` are the two zones that have no area, and
/// Apple lists both.
fn is_zone(name: &str) -> bool {
    if name.contains('.') || name.starts_with("posix/") || name.starts_with("right/") {
        return false;
    }
    match name.split('/').next() {
        Some("Factory") | Some("localtime") | Some("posixrules") => false,
        _ => name.contains('/') || matches!(name, "UTC" | "GMT"),
    }
}

/// Every zone the host's database names, sorted, or empty when it has none.
///
/// Empty rather than a guessed list: an empty array says "this host publishes no
/// zones", which a guest can present, where invented names would be offered to
/// the user and then fail to resolve.
fn known_names() -> Vec<String> {
    let Some(root) = database() else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(&root) else {
                continue;
            };
            let Some(name) = relative.to_str().map(str::to_owned) else {
                continue;
            };
            // `is_dir` follows the link, which is what is wanted: a linked zone
            // - `US/Eastern` - is a name the database resolves, and a linked
            // *directory* is one to descend into.
            match path.is_dir() {
                true if !name.starts_with("posix") && !name.starts_with("right") => {
                    pending.push(path)
                }
                true => {}
                false if is_zone(&name) => names.push(name),
                false => {}
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// `+[NSTimeZone systemTimeZone]`, autoreleased as a `+` method's result is.
unsafe extern "C" fn system_time_zone(_class: Id, _sel: Sel) -> Id {
    let zone = unsafe { corefoundation::timezone::copy_system() };
    // SAFETY: the reference `copy_system` handed over.
    unsafe { corefoundation::api::CFAutorelease(zone) as Id }
}

unsafe extern "C" fn known_time_zone_names(_class: Id, _sel: Sel) -> Id {
    let names = known_names()
        .iter()
        .map(|name| crate::string::from_str(name) as *const std::ffi::c_void)
        .collect::<Vec<_>>();
    let array = corefoundation::collections::new_array(
        &names,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for name in names {
        // The array retained each string and now owns it.
        // SAFETY: each is the live object made immediately above.
        unsafe { corefoundation::api::CFRelease(name) };
    }
    // SAFETY: the fresh array this call owns.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

/// `+[NSTimeZone timeZoneWithName:]`: the zone, autoreleased, or nil for a
/// name the host does not know - which is Apple's answer too, and one a program
/// checks for, where a zone that silently behaved as UTC would not be.
unsafe extern "C" fn time_zone_with_name(_class: Id, _sel: Sel, name: Id) -> Id {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a string; `create_with_name` answers NULL for an unknown name
    // and otherwise hands over a reference.
    unsafe {
        let zone = corefoundation::timezone::create_with_name(std::ptr::null(), name as _, 0);
        match zone.is_null() {
            true => std::ptr::null_mut(),
            false => corefoundation::api::CFAutorelease(zone) as Id,
        }
    }
}

/// `+[NSTimeZone timeZoneForSecondsFromGMT:]`: a fixed-offset zone named
/// `GMT+HHMM`, autoreleased, or nil beyond +-18 hours.
unsafe extern "C" fn time_zone_for_seconds_from_gmt(_class: Id, _sel: Sel, seconds: isize) -> Id {
    // SAFETY: `create_with_time_interval_from_gmt` answers NULL or a new reference.
    unsafe {
        let zone = corefoundation::timezone::create_with_time_interval_from_gmt(
            std::ptr::null(),
            seconds as f64,
        );
        match zone.is_null() {
            true => std::ptr::null_mut(),
            false => corefoundation::api::CFAutorelease(zone) as Id,
        }
    }
}

/// `+[NSTimeZone timeZoneWithAbbreviation:]`: nil unless the abbreviation is
/// in the default dictionary, as on a Mac.
unsafe extern "C" fn time_zone_with_abbreviation(_class: Id, sel: Sel, abbreviation: Id) -> Id {
    // SAFETY: a string, or nil.
    let text = unsafe { crate::string::bytes(abbreviation) };
    let Some(name) = std::str::from_utf8(&text)
        .ok()
        .and_then(corefoundation::timezone::name_for_abbreviation)
    else {
        return std::ptr::null_mut();
    };
    let name = crate::string::from_str(name);
    // SAFETY: a fresh string, released once the zone is made from it.
    unsafe {
        let zone = time_zone_with_name(std::ptr::null_mut(), sel, name);
        corefoundation::api::CFRelease(name as _);
        zone
    }
}

/// The receiver's zone data. Every zone object is made from a name the
/// database resolved, so a miss here is a broken invariant, not an answer.
fn zone_of(receiver: Id) -> std::sync::Arc<corefoundation::tzif::Zone> {
    // SAFETY: the receiver is a time zone object.
    let name = unsafe { corefoundation::timezone::name_of(receiver as _) };
    match name.as_deref().and_then(corefoundation::timezone::zone) {
        Some(zone) => zone,
        None => objc::fail!("NSTimeZone: no zone data for {name:?}"),
    }
}

/// The Unix second a date names, by asking it - so any `NSDate` answers, and
/// nil is the reference date, as a message to nil would make it.
fn unix_of(date: Id) -> i64 {
    // SAFETY: `-timeIntervalSinceReferenceDate` is `double (id, SEL)`.
    let interval = unsafe {
        let send: unsafe extern "C" fn(Id, Sel) -> f64 = std::mem::transmute(objc::msg::entry());
        send(date, objc::sel::get("timeIntervalSinceReferenceDate"))
    };
    (interval + UNIX_TO_REFERENCE).floor() as i64
}

/// Seconds from 1970 to 2001-01-01, the reference date.
const UNIX_TO_REFERENCE: f64 = 978_307_200.0;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

unsafe extern "C" fn seconds_from_gmt_for_date(receiver: Id, _sel: Sel, date: Id) -> isize {
    zone_of(receiver).at(unix_of(date)).offset as isize
}

unsafe extern "C" fn seconds_from_gmt(receiver: Id, _sel: Sel) -> isize {
    zone_of(receiver).at(now()).offset as isize
}

fn abbreviation_at(receiver: Id, unix: i64) -> Id {
    let kind = zone_of(receiver).at(unix);
    objc::arc::autorelease(crate::string::from_str(&kind.abbreviation))
}

unsafe extern "C" fn abbreviation_for_date(receiver: Id, _sel: Sel, date: Id) -> Id {
    abbreviation_at(receiver, unix_of(date))
}

unsafe extern "C" fn abbreviation(receiver: Id, _sel: Sel) -> Id {
    abbreviation_at(receiver, now())
}

unsafe extern "C" fn is_daylight_saving_time_for_date(receiver: Id, _sel: Sel, date: Id) -> u8 {
    u8::from(zone_of(receiver).at(unix_of(date)).dst)
}

unsafe extern "C" fn is_daylight_saving_time(receiver: Id, _sel: Sel) -> u8 {
    u8::from(zone_of(receiver).at(now()).dst)
}

/// How far daylight time moves the clock at `unix`: zero in standard time.
fn daylight_offset_at(receiver: Id, unix: i64) -> f64 {
    let zone = zone_of(receiver);
    (zone.at(unix).offset - zone.standard_offset(unix)) as f64
}

unsafe extern "C" fn daylight_saving_time_offset_for_date(
    receiver: Id,
    _sel: Sel,
    date: Id,
) -> f64 {
    daylight_offset_at(receiver, unix_of(date))
}

unsafe extern "C" fn daylight_saving_time_offset(receiver: Id, _sel: Sel) -> f64 {
    daylight_offset_at(receiver, now())
}

/// The next change of offset after `unix`, as a date, or nil for a zone that
/// has no more.
fn next_transition_after(receiver: Id, unix: i64) -> Id {
    match zone_of(receiver).next_transition(unix) {
        Some(at) => crate::timer::date(at as f64 - UNIX_TO_REFERENCE),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn next_daylight_saving_time_transition_after_date(
    receiver: Id,
    _sel: Sel,
    date: Id,
) -> Id {
    next_transition_after(receiver, unix_of(date))
}

unsafe extern "C" fn next_daylight_saving_time_transition(receiver: Id, _sel: Sel) -> Id {
    next_transition_after(receiver, now())
}

/// A time zone is immutable, so a copy is the same object.
unsafe extern "C" fn copy_with_zone(receiver: Id, _sel: Sel, _zone: *mut std::ffi::c_void) -> Id {
    objc::arc::retain(receiver)
}

/// `-[NSTimeZone name]`, the zone's IANA name, owned by the zone.
unsafe extern "C" fn name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a timezone object.
    unsafe { corefoundation::timezone::get_name(receiver as _) as Id }
}

pub(crate) fn install() {
    let class = corefoundation::base::abstract_class(c"NSTimeZone", None);
    class_methods!(class, {
        "systemTimeZone" => system_time_zone, c"@16@0:8";
        "localTimeZone" => system_time_zone, c"@16@0:8";
        "defaultTimeZone" => system_time_zone, c"@16@0:8";
        "knownTimeZoneNames" => known_time_zone_names, c"@16@0:8";
        "timeZoneWithName:" => time_zone_with_name, c"@24@0:8@16";
        "timeZoneWithAbbreviation:" => time_zone_with_abbreviation, c"@24@0:8@16";
        "timeZoneForSecondsFromGMT:" => time_zone_for_seconds_from_gmt, c"@24@0:8q16";
    });
    methods!(class, {
        "name" => name, c"@16@0:8";
        "secondsFromGMTForDate:" => seconds_from_gmt_for_date, c"q24@0:8@16";
        "secondsFromGMT" => seconds_from_gmt, c"q16@0:8";
        "abbreviationForDate:" => abbreviation_for_date, c"@24@0:8@16";
        "abbreviation" => abbreviation, c"@16@0:8";
        "isDaylightSavingTimeForDate:" => is_daylight_saving_time_for_date, c"c24@0:8@16";
        "isDaylightSavingTime" => is_daylight_saving_time, c"c16@0:8";
        "daylightSavingTimeOffsetForDate:" => daylight_saving_time_offset_for_date, c"d24@0:8@16";
        "daylightSavingTimeOffset" => daylight_saving_time_offset, c"d16@0:8";
        "nextDaylightSavingTimeTransitionAfterDate:" => next_daylight_saving_time_transition_after_date, c"@24@0:8@16";
        "nextDaylightSavingTimeTransition" => next_daylight_saving_time_transition, c"@16@0:8";
        "copyWithZone:" => copy_with_zone, c"@24@0:8^v16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The structural rule, which is what keeps `zone.tab` and the compiled
    /// `posix/` tree out of a list a guest shows a user.
    #[test]
    fn a_zone_is_an_area_and_a_location() {
        assert!(is_zone("America/New_York"));
        assert!(is_zone("America/Argentina/Buenos_Aires"));
        assert!(is_zone("UTC"));
        assert!(!is_zone("zone.tab"));
        assert!(!is_zone("tzdata.zi"));
        assert!(!is_zone("leapseconds"));
        assert!(!is_zone("posix/America/New_York"));
        assert!(!is_zone("right/UTC"));
        assert!(!is_zone("Factory"));
    }

    /// Whatever the host publishes, every name in the answer is one
    /// `+timeZoneWithName:` resolves - which is the only promise the list makes.
    #[test]
    fn every_published_name_is_a_zone_the_host_has() {
        let Some(root) = database() else {
            return;
        };
        let names = known_names();
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]), "sorted");
        for name in &names {
            assert!(root.join(name).exists(), "{name} is not in the database");
        }
        if names.iter().any(|name| name.starts_with("America/")) {
            assert!(
                names.contains(&"America/New_York".to_string()),
                "a database with an America area has its best-known zone"
            );
        }
    }

    /// The system zone is an object, and it is the one the CoreFoundation
    /// spelling answers with - not a second, unrelated timezone class.
    #[test]
    fn the_system_zone_is_a_timezone() {
        crate::bootstrap();
        // SAFETY: `+systemTimeZone` is `id (Class, SEL)`.
        let zone =
            unsafe { system_time_zone(std::ptr::null_mut(), objc::sel::get("systemTimeZone")) };
        assert!(!zone.is_null());
        let class = objc::class::lookup(c"NSTimeZone").expect("NSTimeZone");
        // SAFETY: the object made just above.
        assert!(
            unsafe { objc::object::class_of(zone) }
                .is_some_and(|c| c.chain().any(|x| std::ptr::eq(x, class))),
            "the system zone is an NSTimeZone"
        );
        // SAFETY: `-name` is `id (id, SEL)`.
        let named = unsafe { name(zone, objc::sel::get("name")) };
        assert_eq!(
            unsafe { crate::string::bytes(named) },
            corefoundation::timezone::system_name().as_bytes()
        );
    }

    /// A zone is found by the name it reports, and a name the host has never
    /// heard of is nil rather than a stand-in.
    #[test]
    fn a_zone_is_found_by_name() {
        crate::bootstrap();
        let sel = objc::sel::get("timeZoneWithName:");
        let utc = crate::string::from_str("UTC");
        // SAFETY: `+timeZoneWithName:` is `id (Class, SEL, id)`.
        let zone = unsafe { time_zone_with_name(std::ptr::null_mut(), sel, utc) };
        assert!(!zone.is_null());
        // SAFETY: the zone made just above.
        let named = unsafe { name(zone, objc::sel::get("name")) };
        assert_eq!(unsafe { crate::string::bytes(named) }, b"UTC");
        for bogus in ["Nowhere/Atlantis", "../etc/passwd", ""] {
            let text = crate::string::from_str(bogus);
            // SAFETY: as above.
            let zone = unsafe { time_zone_with_name(std::ptr::null_mut(), sel, text) };
            assert!(zone.is_null(), "{bogus:?} resolved");
        }
    }

    /// A fixed offset is a zone named for it, which resolves by that name again,
    /// and an offset past 18 hours is nil.
    #[test]
    fn a_zone_is_made_for_a_fixed_offset() {
        crate::bootstrap();
        let sel = objc::sel::get("timeZoneForSecondsFromGMT:");
        let cases: [(isize, &[u8]); 5] = [
            (0, b"GMT"),
            (5400, b"GMT+0130"),
            (-18000, b"GMT-0500"),
            (3723, b"GMT+010203"),
            (18 * 3600, b"GMT+1800"),
        ];
        for (seconds, expected) in cases {
            // SAFETY: `+timeZoneForSecondsFromGMT:` is `id (Class, SEL, NSInteger)`.
            let zone =
                unsafe { time_zone_for_seconds_from_gmt(std::ptr::null_mut(), sel, seconds) };
            assert!(!zone.is_null(), "{seconds} made no zone");
            // SAFETY: the zone made just above.
            unsafe {
                let named = name(zone, objc::sel::get("name"));
                assert_eq!(crate::string::bytes(named), expected);
                assert_eq!(
                    seconds_from_gmt(zone, objc::sel::get("secondsFromGMT")),
                    seconds
                );
                let again = time_zone_with_name(std::ptr::null_mut(), sel, named);
                assert!(!again.is_null(), "{seconds}: its own name does not resolve");
            }
        }
        for seconds in [18 * 3600 + 1, -18 * 3600 - 60] {
            // SAFETY: as above.
            let zone =
                unsafe { time_zone_for_seconds_from_gmt(std::ptr::null_mut(), sel, seconds) };
            assert!(zone.is_null(), "{seconds} made a zone");
        }
    }

    /// An abbreviation resolves through the default dictionary, and one that
    /// is not in it is nil.
    #[test]
    fn a_zone_is_found_by_abbreviation() {
        crate::bootstrap();
        if database().is_none() {
            return;
        }
        let sel = objc::sel::get("timeZoneWithAbbreviation:");
        for (short, full) in [("UTC", &b"UTC"[..]), ("PST", b"America/Los_Angeles")] {
            let text = crate::string::from_str(short);
            // SAFETY: `+timeZoneWithAbbreviation:` is `id (Class, SEL, id)`.
            let zone = unsafe { time_zone_with_abbreviation(std::ptr::null_mut(), sel, text) };
            assert!(!zone.is_null(), "{short} did not resolve");
            // SAFETY: the zone made just above.
            let named = unsafe { name(zone, objc::sel::get("name")) };
            assert_eq!(unsafe { crate::string::bytes(named) }, full);
        }
        let text = crate::string::from_str("XYZ");
        // SAFETY: as above.
        let zone = unsafe { time_zone_with_abbreviation(std::ptr::null_mut(), sel, text) };
        assert!(zone.is_null());
    }

    /// A named zone answers for the date it is asked about, not for now and
    /// not with this machine's offset.
    #[test]
    fn a_zone_answers_for_the_date_asked_about() {
        crate::bootstrap();
        let new_york = crate::string::from_str("America/New_York");
        // SAFETY: `+timeZoneWithName:` is `id (Class, SEL, id)`.
        let zone = unsafe {
            time_zone_with_name(
                std::ptr::null_mut(),
                objc::sel::get("timeZoneWithName:"),
                new_york,
            )
        };
        if zone.is_null() {
            // No zone database on this host.
            return;
        }
        // 2026-07-01T12:00Z and 2026-01-01T12:00Z.
        let summer = crate::timer::date_since_1970(1_782_907_200.0);
        let winter = crate::timer::date_since_1970(1_767_268_800.0);
        let sel = objc::sel::get("");
        // SAFETY: the zone and dates made above.
        unsafe {
            assert_eq!(seconds_from_gmt_for_date(zone, sel, summer), -4 * 3600);
            assert_eq!(seconds_from_gmt_for_date(zone, sel, winter), -5 * 3600);
            assert_eq!(
                daylight_saving_time_offset_for_date(zone, sel, summer),
                3600.0
            );
            assert_eq!(daylight_saving_time_offset_for_date(zone, sel, winter), 0.0);
            let abbreviation = abbreviation_for_date(zone, sel, summer);
            assert_eq!(crate::string::bytes(abbreviation), b"EDT");
            // 2026-03-08T07:00Z.
            let next = next_daylight_saving_time_transition_after_date(zone, sel, winter);
            assert_eq!(
                crate::timer::interval_since_1970(next),
                Some(1_772_953_200.0)
            );
        }
    }
}
