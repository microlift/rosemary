use objc::object::Id;
use objc::sel::Sel;

use crate::define::class_methods;

/// The directories a zoneinfo database is found in, in the order `tzset` itself
/// looks: `TZDIR` overrides, and the rest are where the distributions put it -
/// NixOS uses `/etc/zoneinfo`, most others `/usr/share/zoneinfo`.
fn database() -> Option<std::path::PathBuf> {
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

/// Whether a path relative to the database root is a zone *file*.
///
/// The same exclusions as [`is_zone`] plus the no-area condition: an
/// abbreviation is a bare word, and a bare word that names something in the
/// database is a zone whose standard abbreviation is that word. `UTC` and `GMT`
/// qualify, `CET` and `EST` do, `Factory` and `zone.tab` do not.
///
/// **The shape is not the test; the database is.** A bare word with no `.` and
/// no `/` passes the shape check, and `/usr/share/zoneinfo` really does contain
/// such files that are not zones at all - `leapseconds` and `posixrules` among
/// them. Answering a guest that asked for `timeZoneWithAbbreviation:` with
/// `leapseconds` would be handing back a file that is not a time zone, so the
/// file has to exist *and* not be one of the database's own housekeeping
/// entries.
fn is_zone_file(name: &str) -> bool {
    // An abbreviation is a **bare word**. A name with an area in it
    // (`America/New_York`) is a zone, and a perfectly good one, but it is not
    // what `timeZoneWithAbbreviation:` is being asked for - and accepting it
    // would mean the guest got a zone it never named. So the no-area condition
    // is a hard gate, not a fallback.
    if name.is_empty() || name.contains('.') || name.contains('/') {
        return false;
    }
    // The housekeeping files the tz database keeps beside the zones. These are
    // named here rather than pattern-matched because the list is short, fixed,
    // and part of the database's own contract - a new pattern would be a guess
    // about files this host may not even have.
    const NOT_ZONES: &[&str] = &[
        "leapseconds",
        "leap-seconds.list",
        "posixrules",
        "Factory",
        "localtime",
        "tzdata.zi",
        "iso3166.tab",
        "zone.tab",
        "zone1970.tab",
        "zonenow.tab",
    ];
    if NOT_ZONES.contains(&name) {
        return false;
    }
    let Some(root) = database() else {
        // No database to ask. The shape is then all there is, and saying so is
        // better than inventing an answer for a host that has no zones at all.
        return false;
    };
    root.join(name).is_file()
}

/// The zone the host's database names by `abbreviation`, or `None`.
///
/// **This is a lookup in the host's zoneinfo tree, not a table invented here,
/// and that is the only reason it can be answered at all.** A name under `root`
/// is a file the host really has, and the guest's argument is accepted only if
/// the host publishes a zone of that name - so what comes back is a zone this
/// host can genuinely resolve, not a plausible-looking guess.
///
/// What this **cannot** do is resolve an abbreviation in general, and the
/// distinction matters because a wrong zone is a silent wrong answer: the guest
/// hands the result straight to `-setTimeZone:` on a date formatter
/// (`0xa58e928`) and then formats timestamps with it, so an invented zone would
/// be indistinguishable from a right one everywhere the guest can check.
/// Resolving `PST` to *Pacific* needs an abbreviation table, and the host's
/// database does not carry one:
///
/// * `zone.tab`, `zone1970.tab` and `iso3166.tab` carry ISO country codes,
///   coordinates and comments - no abbreviations. Read on this host, not assumed.
/// * `tzdata.zi` here is version 2026d in the **slim** format `zic -b slim`
///   produces: a `Z` line reads `Z <name> <offset> - <abbrev> <until>` and the
///   abbreviation column is `-` for the zones that would matter. Read on this
///   host, not assumed.
///
/// So an abbreviation the database does not name is refused by name below, and
/// the refusal says which files it could not answer from. Answering the system
/// zone, or nil, would both be worse: the first is a zone nobody asked for, and
/// the second is indistinguishable from "macOS has no such zone".
fn zone_named(abbreviation: &str) -> Option<String> {
    let root = database()?;
    let candidate = abbreviation.trim();
    if candidate.is_empty() {
        return None;
    }
    // Two shapes are acceptable, and the distinction is the difference between
    // naming a zone and naming one of the database's own bookkeeping files.
    //
    // A **bare word** has to clear [`is_zone_file`], which consults the database
    // and excludes `leapseconds`, `posixrules` and the rest. A name **with an
    // area in it** (`Europe/London`) is already unambiguous - there is nothing
    // else in the tree that could be meant by it - so it only has to be a real
    // zone file, which is what [`is_zone`] checks.
    //
    // Anything containing a `.` is refused outright: those are `zone.tab` and
    // friends, which are tables rather than zones.
    if candidate.contains('.') {
        return None;
    }
    let shape_ok = if candidate.contains('/') {
        is_zone(candidate)
    } else {
        is_zone_file(candidate)
    };
    if !shape_ok {
        return None;
    }
    let path = root.join(candidate);
    // `is_file` follows the link, which is what is wanted: `US/Eastern` and
    // `EST` are both links to a real zone, and a link to a *directory* is one
    // this must not answer for a bare abbreviation.
    path.is_file().then(|| candidate.to_string())
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

/// `+[NSTimeZone timeZoneWithAbbreviation:]`.
///
/// This is the one that cannot be answered from a name, so the work is all in
/// saying so - see [`zone_named`], which is where the database is consulted and
/// where the refusal is justified.
///
/// Every call site in this guest passes the same argument: a `__DATA_CONST`
/// `__cfstring` reading `UTC` (`0xe678b30`, `{isa, flags, chars, length = 3}`),
/// which is a name the host's database has. So the case that is reachable is
/// answered from the database, and the case that would need a table refuses.
///
/// Autoreleased, as a `+` method's result is.
unsafe extern "C" fn time_zone_with_abbreviation(_class: Id, _sel: Sel, abbreviation: Id) -> Id {
    if abbreviation.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller's `NSString`.
    let text = match String::from_utf8(crate::string::bytes(abbreviation)) {
        Ok(text) if !text.is_empty() => text,
        // Not a string, or an empty one: there is no zone name here to look up,
        // and nil says that without claiming anything about macOS.
        _ => return std::ptr::null_mut(),
    };
    let Some(name) = zone_named(&text) else {
        objc::fail!(
            "+[NSTimeZone timeZoneWithAbbreviation:]: {text:?} is not a zone this host's \
             zoneinfo database names, and resolving a bare abbreviation like it to a zone needs a \
             table this host does not publish - /usr/share/zoneinfo carries zone names and \
             offsets, and its zone*.tab and tzdata.zi carry no abbreviations. Returning a \
             plausible zone here would be a wrong answer the guest formats timestamps with."
        )
    };
    let name_string = crate::string::from_str(&name);
    // SAFETY: a zone object this runtime's own `__NSCFTimeZone` answers, which
    // retains the name it is given.
    let zone = unsafe {
        corefoundation::timezone::create_with_name(
            std::ptr::null(),
            name_string as corefoundation::base::CFTypeRef,
            0,
        )
    };
    // SAFETY: the name this call made, now held by the zone.
    unsafe { corefoundation::api::CFRelease(name_string as corefoundation::base::CFTypeRef) };
    if zone.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the reference `create_with_name` handed over.
    unsafe { corefoundation::api::CFAutorelease(zone) as Id }
}

pub(crate) fn install() {
    let class = corefoundation::base::abstract_class(c"NSTimeZone", None);
    class_methods!(class, {
        "systemTimeZone" => system_time_zone, c"@16@0:8";
        "localTimeZone" => system_time_zone, c"@16@0:8";
        "defaultTimeZone" => system_time_zone, c"@16@0:8";
        "knownTimeZoneNames" => known_time_zone_names, c"@16@0:8";
        "timeZoneWithAbbreviation:" => time_zone_with_abbreviation, c"@24@0:8@16";
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

    /// An abbreviation is a bare word, so the rule that admits it is the
    /// no-area one - and it must not become a hole through which `zone.tab` and
    /// the generated files come out as zones.
    #[test]
    fn an_abbreviation_is_a_bare_word_that_names_something_in_the_database() {
        assert!(is_zone_file("UTC"));
        assert!(is_zone_file("CET"));
        assert!(is_zone_file("EST"));
        assert!(!is_zone_file("zone.tab"));
        assert!(!is_zone_file("tzdata.zi"));
        assert!(!is_zone_file("leapseconds"));
        assert!(!is_zone_file("Factory"));
        assert!(!is_zone_file("posixrules"));
        assert!(!is_zone_file("localtime"));
        assert!(!is_zone_file("America/New_York"), "not an abbreviation");
    }

    /// **The load-bearing test for `timeZoneWithAbbreviation:`**: whatever it
    /// answers is a zone the host's database really has.
    ///
    /// The negative half matters as much as the positive one. The guest hands
    /// the answer to `-setTimeZone:` and formats timestamps with it, so a
    /// plausible-but-invented zone would be indistinguishable from a right one
    /// everywhere the guest can look. So this asserts the promise the whole
    /// implementation rests on - a name that came back is a file under the
    /// database root - and separately that nothing is invented for a name the
    /// database does not have.
    #[test]
    fn an_abbreviation_resolves_only_to_a_zone_the_database_really_has() {
        let Some(root) = database() else {
            // A host with no database has no answer for anything, which is the
            // honest state; the refusal path covers it.
            assert!(zone_named("UTC").is_none());
            return;
        };
        for abbreviation in ["UTC", "GMT", "CET", "EST", "PST", "Europe/London"] {
            match zone_named(abbreviation) {
                // A zone that came back must be a file this host has.
                Some(name) => assert!(
                    root.join(&name).is_file(),
                    "{abbreviation} resolved to {name}, which is not in the database"
                ),
                // Nothing invented for a name the database does not have. The
                // caller turns this into a refusal by name rather than a guess.
                None => assert!(
                    !root.join(abbreviation).exists(),
                    "{abbreviation} was refused but the database does have it"
                ),
            }
        }
        // The two shapes that are not zone names at all, refused for a
        // structural reason rather than for want of a file.
        assert!(zone_named("").is_none());
        assert!(zone_named("  ").is_none());
        assert!(zone_named("zone.tab").is_none());
        assert!(zone_named("posixrules").is_none());
    }

    /// `+[NSTimeZone timeZoneWithAbbreviation:@"UTC"]` - the only call this guest
    /// makes, at all eleven of its sites - answers a real zone object.
    ///
    /// Asserted as an object *and* as one whose name the host agrees with, so
    /// this cannot pass by returning the system zone or an empty placeholder.
    #[test]
    fn the_abbreviation_the_guest_asks_for_answers_a_zone_of_that_name() {
        crate::bootstrap();
        // The guest's argument at every site: a constant `__cfstring` reading
        // `UTC`, read out of `Framework+0xe678b30`.
        let abbreviation = crate::string::from_str("UTC");
        // SAFETY: `+timeZoneWithAbbreviation:` is `id (Class, SEL, NSString *)`.
        let zone = unsafe {
            let f: unsafe extern "C" fn(Id, objc::sel::Sel, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                objc::class::ptr(objc::class::lookup(c"NSTimeZone").expect("NSTimeZone")) as Id,
                objc::sel::get("timeZoneWithAbbreviation:"),
                abbreviation,
            )
        };
        if database().is_none() {
            // Nothing to be right about, so the honest answer is the refusal
            // path, which aborts. There is nothing to assert here.
            return;
        }
        assert!(!zone.is_null(), "a zone this host has was refused");
        // SAFETY: the zone is live and this runtime's own layout.
        let name = unsafe { corefoundation::timezone::name_of(zone.cast()) };
        assert_eq!(name.as_deref(), Some("UTC"));
    }

    /// The selector is a class method with the encoding its one argument
    /// implies.
    #[test]
    fn the_abbreviation_spelling_is_installed_on_the_metaclass() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSTimeZone").expect("NSTimeZone");
        let sel = objc::sel::get("timeZoneWithAbbreviation:");
        assert!(class.method(sel).is_none(), "not an instance method");
        let method = class
            .metaclass()
            .method(sel)
            .expect("a + method on NSTimeZone");
        // SAFETY: a registered method's types are a NUL-terminated string.
        let types = unsafe { std::ffi::CStr::from_ptr(method.types()) };
        assert_eq!(types, c"@24@0:8@16");
        assert!(!method.implementation().is_null());
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
    }
}
