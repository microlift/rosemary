use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Mutex;

use corefoundation::base::CFTypeRef;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// One stored cookie.
#[derive(Clone, Debug, Default, PartialEq)]
struct Cookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    secure: bool,
    /// Seconds since the epoch, or `None` for a cookie that dies with the
    /// session. A session cookie is not written to the jar.
    expires: Option<i64>,
}

impl Cookie {
    fn matches(&self, host: &str, path: &str) -> bool {
        let domain = self.domain.trim_start_matches('.');
        if domain.is_empty() {
            return false;
        }
        let host_matches = host == domain
            || (host.len() > domain.len()
                && host.ends_with(domain)
                && host.as_bytes()[host.len() - domain.len() - 1] == b'.');
        let against = match self.path.is_empty() {
            true => "/",
            false => self.path.as_str(),
        };
        host_matches && path.starts_with(against)
    }

    /// Whether this and `other` name the same cookie, which is what makes a
    /// `-setCookie:` a replacement rather than a second entry.
    fn same_as(&self, other: &Cookie) -> bool {
        self.name == other.name && self.domain == other.domain && self.path == other.path
    }
}

static JAR: Mutex<Vec<Cookie>> = Mutex::new(Vec::new());
static LOADED: std::sync::Once = std::sync::Once::new();

fn jar() -> std::sync::MutexGuard<'static, Vec<Cookie>> {
    LOADED.call_once(|| {
        *JAR.lock().unwrap_or_else(|e| e.into_inner()) = load();
    });
    JAR.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where the jar lives: one file per app, under the directory macOS keeps
/// cookies in.
///
/// macOS has not shared `+sharedHTTPCookieStorage` between apps since 10.11 -
/// each gets `~/Library/Cookies/<bundle id>.binarycookies` - and programs rely
/// on that.
///
/// Decided once, so the file a jar was loaded from is the file it is saved to
/// even if the main bundle were to change in between.
fn store_path() -> PathBuf {
    static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let identifier = crate::bundle::main().and_then(|bundle| bundle.identifier());
        crate::host_home_directory()
            .join("Library/Cookies")
            .join(store_file_name(identifier))
    })
    .clone()
}

/// The jar's file name for the app with this bundle identifier.
///
/// Not `.binarycookies`: the format is this runtime's own, and a file with
/// Apple's name and not Apple's contents is a trap for anything that reads it.
/// An identifier that could leave the directory, or none at all, takes the
/// runtime's name instead.
fn store_file_name(identifier: Option<&str>) -> String {
    let identifier = identifier
        .filter(|id| !id.is_empty() && !id.contains('/') && !id.starts_with('.'))
        .unwrap_or("rosemary");
    format!("{identifier}.cookies.tsv")
}

/// Tab-separated, one cookie per line.
///
/// No escaping, and none is needed: a tab or a newline is not legal in any of
/// the six fields, so a line that contains one is a line this runtime did not
/// write and is skipped rather than guessed at.
fn load() -> Vec<Cookie> {
    let Ok(text) = std::fs::read_to_string(store_path()) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let cookie = Cookie {
                name: f.next()?.to_string(),
                value: f.next()?.to_string(),
                domain: f.next()?.to_string(),
                path: f.next()?.to_string(),
                secure: f.next()? == "1",
                expires: f.next()?.parse().ok(),
            };
            (!cookie.name.is_empty()).then_some(cookie)
        })
        .collect()
}

/// Write the jar, dropping what a real browser would drop.
///
/// Session cookies are left out on purpose - a cookie with no expiry is one the
/// server asked to last only as long as the process - and so is anything
/// already expired, because writing it back would mean reloading it next launch
/// and expiring it again forever.
fn save(cookies: &[Cookie]) {
    let now = now();
    let mut text = String::new();
    for cookie in cookies {
        let Some(expires) = cookie.expires.filter(|e| *e > now) else {
            trace(format_args!(
                "not written: {}@{} is {}",
                cookie.name,
                cookie.domain,
                match cookie.expires {
                    Some(expires) => format!("expired ({expires} <= {now})"),
                    None => "a session cookie".to_string(),
                }
            ));
            continue;
        };
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{expires}\n",
            cookie.name,
            cookie.value,
            cookie.domain,
            cookie.path,
            u8::from(cookie.secure),
        ));
    }
    let path = store_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, text);
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// An `NSHTTPCookie`. The payload is a leaked `Box` rather than an inline
/// `Cookie`, because the object is created by the runtime's allocator and its
/// size has to be a constant.
#[repr(C)]
struct CookieObject {
    isa: *const c_void,
    cookie: *mut Cookie,
}

fn cookie_class() -> &'static objc::class::Class {
    corefoundation::base::register(c"NSHTTPCookie", None, size_of::<CookieObject>(), false)
}

/// # Safety
///
/// `object` must be null or an `NSHTTPCookie` this module made.
unsafe fn cookie_of<'a>(object: Id) -> Option<&'a Cookie> {
    if object.is_null() {
        return None;
    }
    let payload = unsafe { (*(object as *const CookieObject)).cookie };
    (!payload.is_null()).then(|| unsafe { &*payload })
}

fn make_cookie(cookie: Cookie) -> Id {
    let object = objc::object::create_instance(cookie_class(), 0);
    // SAFETY: a fresh instance of this module's own class.
    unsafe {
        (*(object as *mut CookieObject)).cookie = Box::into_raw(Box::new(cookie));
    }
    // SAFETY: a fresh object the caller does not own.
    unsafe { corefoundation::api::CFAutorelease(object as CFTypeRef) as Id }
}

/// The text of an `NSString`, or the empty string for nil.
///
/// # Safety
///
/// `object` must be null or an `NSString`.
unsafe fn text(object: Id) -> String {
    String::from_utf8_lossy(&unsafe { crate::string::bytes(object) }).into_owned()
}

/// The host and path of an `NSURL`, from its `-absoluteString`.
///
/// Asked by message rather than read out of the object, because `NSURL` is
/// another module's class and the parse it wants - host and path, no scheme, no
/// query - is this one's business.
///
/// # Safety
///
/// `url` must be null or an object that answers `-absoluteString`.
unsafe fn host_and_path(url: Id) -> (String, String) {
    if url.is_null() {
        return (String::new(), "/".to_string());
    }
    // SAFETY: `-absoluteString` is `id (id, SEL)`.
    let string = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(url, objc::sel::get("absoluteString"))
    };
    // SAFETY: an `NSString`, or nil.
    split_url(&unsafe { text(string) })
}

fn split_url(absolute: &str) -> (String, String) {
    let rest = absolute
        .split_once("://")
        .map_or(absolute, |(_, rest)| rest)
        .split(['?', '#'])
        .next()
        .unwrap_or_default();
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    // The userinfo and the port are not part of the match.
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
        .split(':')
        .next()
        .unwrap_or_default();
    (host.to_ascii_lowercase(), path)
}

/// `+[NSHTTPCookie cookieWithProperties:]`.
///
/// Both spellings of every key: Apple's `NSHTTPCookieName` constants and the
/// bare `Name`/`name` a caller building the dictionary by hand uses. A
/// dictionary with no name or no value is not a cookie and answers nil, which
/// is what Apple's does.
unsafe extern "C" fn cookie_with_properties(_class: Id, _sel: Sel, properties: Id) -> Id {
    let object = |keys: &[&str]| -> Id {
        for key in keys {
            let name = crate::string::from_str(key);
            // SAFETY: `properties` is null or a dictionary, `name` a live string.
            let value = unsafe {
                corefoundation::api::CFDictionaryGetValue(
                    properties as CFTypeRef,
                    name as CFTypeRef,
                )
            };
            // SAFETY: a fresh string this call owns.
            unsafe { corefoundation::api::CFRelease(name as CFTypeRef) };
            if !value.is_null() {
                return value as Id;
            }
        }
        std::ptr::null_mut()
    };
    // SAFETY: whatever the caller put there; read as a string, which answers
    // empty for an object that is not one.
    let get = |keys: &[&str]| -> String { unsafe { text(object(keys)) } };
    let name = get(&["NSHTTPCookieName", "Name", "name"]);
    let value = get(&["NSHTTPCookieValue", "Value", "value"]);
    if name.is_empty() {
        return std::ptr::null_mut();
    }
    // Normalised as a header's is. With no domain of its own the cookie takes
    // its origin URL's host, as one from a `Set-Cookie` header takes the host
    // it came from.
    let mut domain = get(&["NSHTTPCookieDomain", "Domain", "domain"])
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if domain.is_empty() {
        // SAFETY: null or an object this runtime allocated.
        domain =
            unsafe { origin_host(object(&["NSHTTPCookieOriginURL", "OriginURL", "originURL"])) };
    }
    let path = get(&["NSHTTPCookiePath", "Path", "path"]);
    let secure = !get(&["NSHTTPCookieSecure", "Secure", "secure"]).is_empty();
    // A discarded cookie is a session cookie whatever its expiry says.
    // SAFETY: null or an object this runtime allocated.
    let discard = unsafe { flag(object(&["NSHTTPCookieDiscard", "Discard", "discard"])) };
    let expires = match discard {
        true => None,
        false => unsafe { expiry(object(&["NSHTTPCookieExpires", "Expires", "expires"])) },
    };
    make_cookie(Cookie {
        name,
        value,
        domain,
        path: match path.is_empty() {
            true => "/".to_string(),
            false => path,
        },
        secure,
        expires,
    })
}

/// The host in an `NSHTTPCookieOriginURL`, which Apple's takes as either an
/// `NSURL` or an `NSString`; empty for anything else.
///
/// # Safety
///
/// `value` must be null or an object this runtime allocated.
unsafe fn origin_host(value: Id) -> String {
    if value.is_null() {
        return String::new();
    }
    if unsafe { corefoundation::string::as_string(value as CFTypeRef) }.is_some() {
        // SAFETY: an `NSString`.
        return split_url(&unsafe { text(value) }).0;
    }
    if unsafe { corefoundation::url::as_url(value as CFTypeRef) }.is_some() {
        // SAFETY: an `NSURL`, which answers `-absoluteString`.
        return unsafe { host_and_path(value) }.0;
    }
    String::new()
}

/// A boolean property such as `NSHTTPCookieDiscard`: the string `"TRUE"` as
/// Apple's constants spell it, or an `NSNumber`/`CFBoolean` a caller put there
/// by hand.
///
/// # Safety
///
/// `value` must be null or an object this runtime allocated.
unsafe fn flag(value: Id) -> bool {
    if value.is_null() {
        return false;
    }
    if value as CFTypeRef == corefoundation::collections::boolean(true) {
        return true;
    }
    if let Some(number) = unsafe { corefoundation::collections::number(value as CFTypeRef) } {
        return number.as_i64() != 0;
    }
    // SAFETY: the caller's obligation; empty for anything but a string.
    let text = unsafe { text(value) };
    ["true", "yes", "1"]
        .iter()
        .any(|t| text.trim().eq_ignore_ascii_case(t))
}

/// The expiry in an `NSHTTPCookieExpires`.
///
/// # Safety
///
/// `value` must be null or an object this runtime allocated.
unsafe fn expiry(value: Id) -> Option<i64> {
    if value.is_null() {
        return None;
    }
    if let Some(seconds) = crate::timer::interval_since_1970(value) {
        return Some(seconds as i64);
    }
    // SAFETY: the caller's obligation; empty for anything but a string.
    let text = unsafe { text(value) };
    parse_expires(text.trim()).or_else(|| text.trim().parse().ok())
}

/// `+[NSHTTPCookie cookiesWithResponseHeaderFields:forURL:]`.
///
/// Only `Set-Cookie`, and only the attributes that decide where a cookie goes
/// back to: `Domain`, `Path`, `Secure` and `Max-Age`. `Expires` is a date in
/// one of three formats and `Max-Age` is a number that means the same thing, so
/// the number is the one parsed and an `Expires`-only cookie is kept for the
/// session rather than given an invented lifetime.
unsafe extern "C" fn cookies_with_response_header_fields(
    _class: Id,
    _sel: Sel,
    fields: Id,
    url: Id,
) -> Id {
    // SAFETY: null or an `NSURL`.
    let (host, _) = unsafe { host_and_path(url) };
    let mut made: Vec<CFTypeRef> = Vec::new();
    for key in ["Set-Cookie", "set-cookie"] {
        let name = crate::string::from_str(key);
        // SAFETY: `fields` is null or a dictionary.
        let value = unsafe {
            corefoundation::api::CFDictionaryGetValue(fields as CFTypeRef, name as CFTypeRef)
        };
        // SAFETY: a fresh string this call owns.
        unsafe { corefoundation::api::CFRelease(name as CFTypeRef) };
        if value.is_null() {
            continue;
        }
        // SAFETY: an `NSString`.
        let header = unsafe { text(value as Id) };
        let parsed = parse_set_cookie(&header, &host);
        trace(format_args!(
            "+cookiesWithResponseHeaderFields: {host}: {} -> {}",
            attributes(&header),
            match &parsed {
                Some(cookie) => format!("{}@{} {}", cookie.name, cookie.domain, life(cookie)),
                None => "nothing".to_string(),
            }
        ));
        if let Some(cookie) = parsed {
            made.push(make_cookie(cookie) as CFTypeRef);
        }
    }
    let array = corefoundation::collections::new_array(
        &made,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh array this call owns.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

fn parse_set_cookie(header: &str, host: &str) -> Option<Cookie> {
    let mut parts = header.split(';');
    let (name, value) = parts.next()?.trim().split_once('=')?;
    let mut cookie = Cookie {
        name: name.trim().to_string(),
        value: value.trim().to_string(),
        domain: host.to_string(),
        path: "/".to_string(),
        secure: false,
        expires: None,
    };
    if cookie.name.is_empty() {
        return None;
    }
    for attribute in parts {
        let attribute = attribute.trim();
        let (key, value) = attribute.split_once('=').unwrap_or((attribute, ""));
        match key.to_ascii_lowercase().as_str() {
            "domain" => cookie.domain = value.trim().trim_start_matches('.').to_ascii_lowercase(),
            "path" if !value.is_empty() => cookie.path = value.trim().to_string(),
            "secure" => cookie.secure = true,
            // `Max-Age` wins over `Expires` where both are present, which is
            // RFC 6265's rule.
            "max-age" => cookie.expires = value.trim().parse::<i64>().ok().map(|s| now() + s),
            "expires" if cookie.expires.is_none() => cookie.expires = parse_expires(value.trim()),
            _ => {}
        }
    }
    Some(cookie)
}

/// `Expires=Wed, 21 Oct 2015 07:28:00 GMT` as seconds since the epoch.
fn parse_expires(value: &str) -> Option<i64> {
    let (mut time, mut day, mut month, mut year) = (None, None, None, None);
    for token in value.split(|c: char| !c.is_ascii_alphanumeric() && c != ':') {
        if time.is_none() {
            if let Some(clock) = parse_time(token) {
                time = Some(clock);
                continue;
            }
        }
        if day.is_none() {
            if let Some(number) = parse_number(token, 1..=2) {
                day = Some(number);
                continue;
            }
        }
        if month.is_none() {
            if let Some(number) = parse_month(token) {
                month = Some(number);
                continue;
            }
        }
        if year.is_none() {
            if let Some(number) = parse_number(token, 2..=4) {
                year = Some(number);
            }
        }
    }
    let ((hour, minute, second), day, month, year) = (time?, day?, month?, year?);
    // A two-digit year, in the century the RFC names rather than one guessed at.
    let year = match year {
        70..=99 => year + 1900,
        0..=69 => year + 2000,
        _ => year,
    };
    let sane = (1..=31).contains(&day) && hour < 24 && minute < 60 && second < 60 && year >= 1601;
    sane.then(|| days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

fn parse_number(token: &str, digits: std::ops::RangeInclusive<usize>) -> Option<i64> {
    digits
        .contains(&token.len())
        .then(|| token.parse().ok())
        .flatten()
}

fn parse_time(token: &str) -> Option<(i64, i64, i64)> {
    let mut fields = token.split(':');
    let mut next = || parse_number(fields.next()?, 1..=2);
    let clock = (next()?, next()?, next()?);
    fields.next().is_none().then_some(clock)
}

fn parse_month(token: &str) -> Option<i64> {
    let name = token.get(..3)?.to_ascii_lowercase();
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    months.iter().position(|m| *m == name).map(|i| i as i64 + 1)
}

/// Howard Hinnant's `days_from_civil`: days since 1970-01-01 for a proleptic
/// Gregorian date.
///
/// Written out rather than pulled in, because the whole of what this module
/// needs from a date library is this one function, and it is exact for every
/// year - no leap-year table to get wrong at a century boundary.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

unsafe extern "C" fn cookie_name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSHTTPCookie` this module made.
    autoreleased(unsafe { cookie_of(receiver) }.map_or("", |c| c.name.as_str()))
}

unsafe extern "C" fn cookie_value(receiver: Id, _sel: Sel) -> Id {
    autoreleased(unsafe { cookie_of(receiver) }.map_or("", |c| c.value.as_str()))
}

unsafe extern "C" fn cookie_domain(receiver: Id, _sel: Sel) -> Id {
    autoreleased(unsafe { cookie_of(receiver) }.map_or("", |c| c.domain.as_str()))
}

unsafe extern "C" fn cookie_path(receiver: Id, _sel: Sel) -> Id {
    autoreleased(unsafe { cookie_of(receiver) }.map_or("/", |c| c.path.as_str()))
}

unsafe extern "C" fn cookie_is_secure(receiver: Id, _sel: Sel) -> u8 {
    u8::from(unsafe { cookie_of(receiver) }.is_some_and(|c| c.secure))
}

/// `-expiresDate`, nil for a session cookie, as Apple's is.
unsafe extern "C" fn cookie_expires_date(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSHTTPCookie` this module made.
    match unsafe { cookie_of(receiver) }.and_then(|c| c.expires) {
        Some(expires) => crate::timer::date_since_1970(expires as f64),
        None => std::ptr::null_mut(),
    }
}

/// `-isSessionOnly`, which is what "has no expiry" means to a caller deciding
/// whether the cookie is worth keeping.
unsafe extern "C" fn cookie_is_session_only(receiver: Id, _sel: Sel) -> u8 {
    u8::from(unsafe { cookie_of(receiver) }.is_none_or(|c| c.expires.is_none()))
}

/// `-properties`, as the dictionary `+cookieWithProperties:` would take back.
unsafe extern "C" fn cookie_properties(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSHTTPCookie` this module made.
    let Some(cookie) = (unsafe { cookie_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    let mut pairs: Vec<(CFTypeRef, CFTypeRef)> = [
        ("NSHTTPCookieName", cookie.name.as_str()),
        ("NSHTTPCookieValue", cookie.value.as_str()),
        ("NSHTTPCookieDomain", cookie.domain.as_str()),
        ("NSHTTPCookiePath", cookie.path.as_str()),
        ("NSHTTPCookieVersion", "0"),
    ]
    .into_iter()
    .chain(cookie.secure.then_some(("NSHTTPCookieSecure", "TRUE")))
    .chain(
        cookie
            .expires
            .is_none()
            .then_some(("NSHTTPCookieDiscard", "TRUE")),
    )
    .map(|(key, value)| {
        (
            crate::string::from_str(key) as CFTypeRef,
            crate::string::from_str(value) as CFTypeRef,
        )
    })
    .collect();
    if let Some(expires) = cookie.expires {
        let date = crate::timer::date_since_1970(expires as f64);
        // Retained, because the loop below releases every value and this one is
        // autoreleased rather than owned by this call.
        objc::arc::retain(date);
        pairs.push((
            crate::string::from_str("NSHTTPCookieExpires") as CFTypeRef,
            date as CFTypeRef,
        ));
    }
    let dictionary = corefoundation::collections::new_dictionary(
        &pairs,
        corefoundation::callbacks::Policy::from_key_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for (key, value) in pairs {
        // SAFETY: the dictionary retained both; these are this call's own
        // references.
        unsafe {
            corefoundation::api::CFRelease(key);
            corefoundation::api::CFRelease(value);
        }
    }
    // SAFETY: a fresh dictionary this call owns.
    unsafe { corefoundation::api::CFAutorelease(dictionary) as Id }
}

/// `ROSEMARY_TRACE_COOKIES=1`: report each call and what it answered.
///
/// A cookie jar is invisible from both ends: a client that asks for a cookie it
/// has never been given and one that is never asked look the same from outside,
/// and the session state that depends on the answer is three HTTP round trips
/// away. This is what tells them apart.
fn trace(what: std::fmt::Arguments) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

    if *ON.get_or_init(|| std::env::var_os("ROSEMARY_TRACE_COOKIES").is_some()) {
        eprintln!("rosemary: cookies: {what}");
    }
}

/// A cookie's lifetime, which is what decides whether it reaches the jar file.
fn life(cookie: &Cookie) -> String {
    match cookie.expires {
        Some(expires) => format!("expires {expires}"),
        None => "session only".to_string(),
    }
}

/// A `Set-Cookie` header with its value elided, for [`trace`].
///
/// The name and the attributes are the whole of what decides where the cookie
/// goes and how long it lives; the value is the secret and is not printed.
fn attributes(header: &str) -> String {
    let mut parts = header.split(';');
    let first = parts.next().unwrap_or_default();
    let (name, value) = first.split_once('=').unwrap_or((first, ""));
    std::iter::once(format!("{}=<{} bytes>", name.trim(), value.len()))
        .chain(parts.map(|a| a.trim().to_string()))
        .collect::<Vec<_>>()
        .join("; ")
}

fn autoreleased(text: &str) -> Id {
    let string = crate::string::from_str(text);
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(string as CFTypeRef) as Id }
}

/// The one storage. Retained forever, for the reason `NSLocale`'s singleton is:
/// a shared object a guest can release is one it can make dangle.
static SHARED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

unsafe extern "C" fn shared_storage(class: Id, _sel: Sel) -> Id {
    *SHARED.get_or_init(|| {
        // SAFETY: `class` is `NSHTTPCookieStorage`, which this module registered.
        let storage = unsafe {
            objc::object::create_instance(
                objc::class::from_ptr(class as *const objc::class::Class).expect("a class"),
                0,
            )
        };
        objc::arc::make_immortal(storage);
        storage as usize
    }) as Id
}

fn cookie_array(cookies: Vec<Cookie>) -> Id {
    let objects: Vec<CFTypeRef> = cookies
        .into_iter()
        .map(|c| make_cookie(c) as CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &objects,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh array this call owns.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn storage_cookies(_receiver: Id, _sel: Sel) -> Id {
    let now = now();
    let all: Vec<Cookie> = jar()
        .iter()
        .filter(|c| c.expires.is_none_or(|e| e > now))
        .cloned()
        .collect();
    trace(format_args!("-cookies -> {} ({})", all.len(), names(&all)));
    cookie_array(all)
}

/// The names in a cookie list, for [`trace`].
fn names(cookies: &[Cookie]) -> String {
    cookies
        .iter()
        .map(|c| format!("{}@{}", c.name, c.domain))
        .collect::<Vec<_>>()
        .join(", ")
}

unsafe extern "C" fn cookies_for_url(_receiver: Id, _sel: Sel, url: Id) -> Id {
    // SAFETY: null or an `NSURL`.
    let (host, path) = unsafe { host_and_path(url) };
    let now = now();
    let matching: Vec<Cookie> = jar()
        .iter()
        .filter(|c| c.matches(&host, &path) && c.expires.is_none_or(|e| e > now))
        .cloned()
        .collect();
    trace(format_args!(
        "-cookiesForURL: {host}{path} -> {} ({})",
        matching.len(),
        names(&matching)
    ));
    cookie_array(matching)
}

unsafe extern "C" fn set_cookie(_receiver: Id, _sel: Sel, cookie: Id) {
    // SAFETY: null or an `NSHTTPCookie` this module made.
    let Some(cookie) = (unsafe { cookie_of(cookie) }) else {
        return;
    };
    trace(format_args!(
        "-setCookie: {}@{}{} = {} bytes, {}",
        cookie.name,
        cookie.domain,
        cookie.path,
        cookie.value.len(),
        life(cookie)
    ));
    let mut jar = jar();
    jar.retain(|existing| !existing.same_as(cookie));
    jar.push(cookie.clone());
    save(&jar);
}

unsafe extern "C" fn delete_cookie(_receiver: Id, _sel: Sel, cookie: Id) {
    let Some(cookie) = (unsafe { cookie_of(cookie) }) else {
        return;
    };
    trace(format_args!(
        "-deleteCookie: {}@{}{}",
        cookie.name, cookie.domain, cookie.path
    ));
    let mut jar = jar();
    jar.retain(|existing| !existing.same_as(cookie));
    save(&jar);
}

unsafe extern "C" fn set_cookies_for_url(
    receiver: Id,
    sel: Sel,
    cookies: Id,
    _url: Id,
    _main_document: Id,
) {
    // SAFETY: null or an `NSArray`.
    let count = unsafe { corefoundation::api::CFArrayGetCount(cookies as CFTypeRef) };
    for i in 0..count {
        // SAFETY: an index the count admits.
        let cookie =
            unsafe { corefoundation::api::CFArrayGetValueAtIndex(cookies as CFTypeRef, i) as Id };
        // SAFETY: `-setCookie:` takes exactly this.
        unsafe { set_cookie(receiver, sel, cookie) };
    }
}

/// `+[NSHTTPCookie requestHeaderFieldsWithCookies:]`, the inverse of
/// `+cookiesWithResponseHeaderFields:forURL:`.
unsafe extern "C" fn request_header_fields(_class: Id, _sel: Sel, cookies: Id) -> Id {
    // SAFETY: null or an `NSArray`.
    let count = unsafe { corefoundation::api::CFArrayGetCount(cookies as CFTypeRef) };
    let mut header = String::new();
    for i in 0..count {
        // SAFETY: an index the count admits.
        let object =
            unsafe { corefoundation::api::CFArrayGetValueAtIndex(cookies as CFTypeRef, i) as Id };
        // SAFETY: an `NSHTTPCookie` this module made.
        let Some(cookie) = (unsafe { cookie_of(object) }) else {
            continue;
        };
        if !header.is_empty() {
            header.push_str("; ");
        }
        header.push_str(&format!("{}={}", cookie.name, cookie.value));
    }
    let key = crate::string::from_str("Cookie");
    let value = crate::string::from_str(&header);
    let dictionary = corefoundation::collections::new_dictionary(
        &[(key as CFTypeRef, value as CFTypeRef)],
        corefoundation::callbacks::Policy::from_key_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: the dictionary retained both; these are this call's references.
    unsafe {
        corefoundation::api::CFRelease(key as CFTypeRef);
        corefoundation::api::CFRelease(value as CFTypeRef);
        corefoundation::api::CFAutorelease(dictionary) as Id
    }
}

/// `-setCookieAcceptPolicy:` and its getter.
///
/// Recorded and not honoured: this storage accepts every cookie it is given.
/// The policy that would change that - "only from the main document domain" -
/// needs the main document URL, which `-setCookies:forURL:mainDocumentURL:`
/// carries and nothing else does, and a program that sets the policy and then
/// reads it back is entitled to see what it set.
static ACCEPT_POLICY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "C" fn set_accept_policy(_receiver: Id, _sel: Sel, policy: usize) {
    ACCEPT_POLICY.store(policy, std::sync::atomic::Ordering::Relaxed);
}

unsafe extern "C" fn accept_policy(_receiver: Id, _sel: Sel) -> usize {
    ACCEPT_POLICY.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn install() {
    let cookie = cookie_class();
    class_methods!(cookie, {
        "cookieWithProperties:" => cookie_with_properties, c"@24@0:8@16";
        "cookiesWithResponseHeaderFields:forURL:" => cookies_with_response_header_fields,
            c"@32@0:8@16@24";
        "requestHeaderFieldsWithCookies:" => request_header_fields, c"@24@0:8@16";
    });
    methods!(cookie, {
        "name" => cookie_name, c"@16@0:8";
        "value" => cookie_value, c"@16@0:8";
        "domain" => cookie_domain, c"@16@0:8";
        "path" => cookie_path, c"@16@0:8";
        "isSecure" => cookie_is_secure, c"c16@0:8";
        "expiresDate" => cookie_expires_date, c"@16@0:8";
        "isSessionOnly" => cookie_is_session_only, c"c16@0:8";
        "properties" => cookie_properties, c"@16@0:8";
    });

    let storage =
        corefoundation::base::register(c"NSHTTPCookieStorage", None, size_of::<usize>(), false);
    class_methods!(storage, {
        "sharedHTTPCookieStorage" => shared_storage, c"@16@0:8";
    });
    methods!(storage, {
        "cookies" => storage_cookies, c"@16@0:8";
        "cookiesForURL:" => cookies_for_url, c"@24@0:8@16";
        "setCookie:" => set_cookie, c"v24@0:8@16";
        "deleteCookie:" => delete_cookie, c"v24@0:8@16";
        "setCookies:forURL:mainDocumentURL:" => set_cookies_for_url, c"v40@0:8@16@24@32";
        "setCookieAcceptPolicy:" => set_accept_policy, c"v24@0:8Q16";
        "cookieAcceptPolicy" => accept_policy, c"Q16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_app_keeps_its_own_jar() {
        assert_eq!(
            store_file_name(Some("com.test.app1")),
            "com.test.app1.cookies.tsv"
        );
        assert_ne!(
            store_file_name(Some("com.test.app1")),
            store_file_name(Some("com.test.app2"))
        );
        assert_eq!(store_file_name(None), "rosemary.cookies.tsv");
        assert_eq!(store_file_name(Some("")), "rosemary.cookies.tsv");
        assert_eq!(store_file_name(Some("../evil")), "rosemary.cookies.tsv");
        assert_eq!(store_file_name(Some("a/b")), "rosemary.cookies.tsv");
    }

    #[test]
    fn a_url_splits_into_a_host_and_a_path() {
        assert_eq!(
            split_url("https://www.example.com/home?a=1"),
            ("www.example.com".to_string(), "/home".to_string())
        );
        assert_eq!(
            split_url("https://example.com"),
            ("example.com".to_string(), "/".to_string())
        );
        // The port and the userinfo are not part of the match.
        assert_eq!(
            split_url("http://user@Example.COM:8080/a/b"),
            ("example.com".to_string(), "/a/b".to_string())
        );
    }

    /// The rule that decides whether the session cookie goes back out, and the
    /// one worth a test: a cookie set for `.example.com` must reach
    /// `www.example.com` and must not reach `notexample.com`.
    #[test]
    fn a_domain_cookie_matches_subdomains_and_nothing_else() {
        let cookie = Cookie {
            name: "SESSIONID".into(),
            domain: ".example.com".into(),
            path: "/".into(),
            ..Cookie::default()
        };
        assert!(cookie.matches("example.com", "/"));
        assert!(cookie.matches("www.example.com", "/home"));
        assert!(!cookie.matches("notexample.com", "/"));
        assert!(!cookie.matches("example.com.evil.test", "/"));
    }

    #[test]
    fn a_set_cookie_header_carries_its_attributes() {
        let cookie = parse_set_cookie(
            "SESSIONID=TOKEN; Domain=.example.com; Path=/; Secure; Max-Age=3600",
            "www.example.com",
        )
        .expect("a cookie");
        assert_eq!(cookie.name, "SESSIONID");
        assert_eq!(cookie.value, "TOKEN");
        assert_eq!(cookie.domain, "example.com");
        assert!(cookie.secure);
        assert!(cookie.expires.is_some());
    }

    /// The date arithmetic, against three dates whose epoch seconds are known.
    #[test]
    fn a_civil_date_converts_to_the_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(
            parse_expires("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(1_445_412_480)
        );
    }

    /// A persistent session cookie must retain its expiry date.
    #[test]
    fn an_expires_attribute_is_what_persists_the_session_cookie() {
        let cookie = parse_set_cookie(
            "SESSIONID=TOKEN; Domain=.example.com; Path=/; Expires=Fri, 01 Jan 2100 00:00:00 GMT; Secure",
            "www.example.com",
        )
        .expect("a cookie");
        assert_eq!(cookie.expires, Some(4_102_444_800));
    }

    /// The trace elides the value, which is the secret, and keeps the
    /// attributes, which are what decides whether the cookie is written.
    #[test]
    fn a_traced_header_shows_its_attributes_and_not_its_value() {
        assert_eq!(
            attributes(
                "SESSIONID=SECRET; domain=.example.com; expires=Fri, 01 Jan 2100 00:00:00 GMT"
            ),
            "SESSIONID=<6 bytes>; domain=.example.com; expires=Fri, 01 Jan 2100 00:00:00 GMT"
        );
    }

    /// The round trip through `-properties` and `+cookieWithProperties:` is
    /// what a client does at startup, and an expiry lost across it is the
    /// session cookie cleared from the jar on every relaunch.
    #[test]
    fn a_property_expiry_survives_both_spellings() {
        assert_eq!(
            parse_expires("Fri, 01 Jan 2100 00:00:00 GMT"),
            Some(4_102_444_800)
        );
        // What `-properties` hands back for a cookie with no expiry, and what
        // `+cookieWithProperties:` must not invent one from.
        assert_eq!(parse_expires(""), None);
    }

    /// The three spellings RFC 6265 admits, all of the same instant. The dashed
    /// one is a valid dashed date spelling.
    #[test]
    fn every_spelling_of_a_cookie_date_reads_the_same() {
        let expected = Some(2_734_534_036);
        assert_eq!(parse_expires("Sat, 26-Aug-2056 16:47:16 GMT"), expected);
        assert_eq!(parse_expires("Sat, 26 Aug 2056 16:47:16 GMT"), expected);
        assert_eq!(parse_expires("Sat Aug 26 16:47:16 2056"), expected);
    }

    /// A two-digit year takes the century RFC 6265 names, not one guessed at.
    #[test]
    fn a_two_digit_year_takes_the_rfcs_century() {
        assert_eq!(
            parse_expires("Wed, 21-Oct-15 07:28:00 GMT"),
            parse_expires("Wed, 21 Oct 2015 07:28:00 GMT")
        );
        assert_eq!(
            parse_expires("Thu, 01-Jan-70 00:00:00 GMT"),
            parse_expires("Thu, 01 Jan 1970 00:00:00 GMT")
        );
    }

    /// A date with a field missing is a *session* cookie, not a cookie with an
    /// invented date.
    #[test]
    fn an_unreadable_date_is_refused_rather_than_guessed_at() {
        assert_eq!(parse_expires("nonsense"), None);
        assert_eq!(parse_expires("Sat, 26 Aug 2056 GMT"), None);
        assert_eq!(parse_expires("16:47:16"), None);
        // Out of range rather than wrapped into the next month.
        assert_eq!(parse_expires("Sat, 32 Aug 2056 16:47:16 GMT"), None);
        assert_eq!(parse_expires("Sat, 26 Aug 2056 25:47:16 GMT"), None);
    }

    /// A header with no `Domain` belongs to the host that sent it.
    #[test]
    fn a_set_cookie_without_a_domain_takes_the_hosts() {
        let cookie = parse_set_cookie("a=b", "www.example.com").expect("a cookie");
        assert_eq!(cookie.domain, "www.example.com");
        assert_eq!(cookie.path, "/");
        assert_eq!(cookie.expires, None);
    }
}
