use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// The locale name this process runs under, as `language_REGION`.
fn posix() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
        .unwrap_or_default()
}

/// A locale name split into an ISO 639 language and an ISO 3166 country.
///
/// The encoding suffix and the `@modifier` are dropped: `en_US.UTF-8` and
/// `en_US` name the same language and the same country, and nothing here asks
/// anything else. A name with no country - or none at all, which is what the
/// `C` locale and an unset environment amount to - is `en_US`, because a
/// program uses this to pick a localisation and there has to be one.
fn split(name: &str) -> (String, String) {
    let base = name.split(['.', '@']).next().unwrap_or_default();
    let mut parts = base.split(['_', '-']);
    let language = parts.next().filter(|p| !p.is_empty()).unwrap_or("en");
    let country = parts.next().filter(|p| !p.is_empty()).unwrap_or("US");
    (language.to_ascii_lowercase(), country.to_ascii_uppercase())
}

pub(crate) fn language() -> String {
    split(&posix()).0
}

fn country() -> String {
    split(&posix()).1
}

/// The ISO 3166 country this locale names, for a caller deriving a fact about
/// the place rather than reading a preference.
pub(crate) fn country_code() -> String {
    country()
}

/// Whether this locale's language is English.
///
/// The question `crate::formatter` asks before publishing an English month name:
/// a caller told "September" for a French locale has been given a wrong answer it
/// cannot detect, where one told nothing falls back to its own CLDR tables. See
/// that module's note.
pub(crate) fn is_english() -> bool {
    language() == "en"
}

/// `language-REGION`, which is how a BCP 47 tag spells what `posix` returns.
fn tag() -> String {
    format!("{}-{}", language(), country())
}

/// `language_REGION`, the identifier `-localeIdentifier` answers with.
fn identifier() -> String {
    format!("{}_{}", language(), country())
}

/// The one locale, which every spelling of "the locale" returns.
///
/// The current locale, the system locale and the autoupdating one are the same
/// object here: Apple's differ by whether the user's preferences are applied on
/// top of the raw ones, and this host has no preferences database - the
/// environment is the only answer it has, so there is nothing for them to
/// disagree about.
static CURRENT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn shared() -> Id {
    *CURRENT.get_or_init(|| {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSLocale").expect("NSLocale is registered");
        let locale = objc::object::create_instance(class, 0);
        // Retained forever: a singleton a guest can release is one it can make
        // dangle by releasing what it never owned.
        objc::arc::make_immortal(locale);
        locale as usize
    }) as Id
}

unsafe extern "C" fn current_locale(_class: Id, _sel: Sel) -> Id {
    shared()
}

/// `+preferredLanguages`, which is the one language this process has.
///
/// One element rather than a guessed-at fallback chain: a program walks the
/// list looking for a localisation it has, and an invented second entry would
/// send it to a language the user never asked for.
unsafe extern "C" fn preferred_languages(_class: Id, _sel: Sel) -> Id {
    // SAFETY: a fresh object this call owns.
    unsafe { corefoundation::api::CFAutorelease(preferred_languages_array()) as Id }
}

/// The array [`preferred_languages`] answers, with one reference the caller owns.
fn preferred_languages_array() -> corefoundation::base::CFTypeRef {
    let tag = crate::string::from_str(&tag());
    let array = corefoundation::collections::new_array(
        &[tag as corefoundation::base::CFTypeRef],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh object this call owns, now held by the array.
    unsafe { corefoundation::api::CFRelease(tag as corefoundation::base::CFTypeRef) };
    array
}

/// Whether this locale measures in metres.
///
/// Every country but three uses the metric system, and the United States is the
/// one of those a `LANG` is likely to name. Derived rather than guessed: it is a
/// fact about the country the locale already reports, not a preference this host
/// has no store for.
fn uses_metric() -> bool {
    !matches!(country().as_str(), "US" | "LR" | "MM")
}

/// The value of one locale component, or `None` for a key this host cannot
/// answer from the environment.
///
/// The separators, the currency and the calendar are deliberately absent: they
/// are locale *data*, which lives in CLDR, and this tree carries none of it. A
/// decimal separator invented as `.` would be wrong for every locale that uses
/// `,` and a caller could not tell - where a nil is the documented answer for a
/// component a locale has no value for, and the branch a caller already has.
fn component(key: &[u8]) -> Option<String> {
    Some(match key {
        b"kCFLocaleIdentifier" => identifier(),
        b"kCFLocaleLanguageCode" => language(),
        b"kCFLocaleCountryCode" => country(),
        b"kCFLocaleMeasurementSystem" => match uses_metric() {
            true => "Metric".to_string(),
            false => "U.S.".to_string(),
        },
        _ => return None,
    })
}

/// The locale components, by the key the constants below name.
///
/// A key this does not know answers nil, which is what Apple's locale does for
/// a component it has no value for.
unsafe extern "C" fn object_for_key(_receiver: Id, _sel: Sel, key: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let key = unsafe { crate::string::bytes(key) };
    // `kCFLocaleUsesMetricSystem` is a `CFBoolean`, not a string, so it is
    // answered before the ones that are.
    if key.as_slice() == b"kCFLocaleUsesMetricSystem" {
        return corefoundation::collections::boolean(uses_metric()) as Id;
    }
    match component(key.as_slice()) {
        Some(value) => autoreleased(&value),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn locale_identifier(_receiver: Id, _sel: Sel) -> Id {
    autoreleased(&identifier())
}

fn autoreleased(text: &str) -> Id {
    let string = crate::string::from_str(text);
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(string as corefoundation::base::CFTypeRef) as Id }
}

/// The `NSString *` globals naming a locale component, and their documented
/// values - which are the CoreFoundation spellings, because Foundation's
/// constants *are* CoreFoundation's.
///
/// Published whether or not [`component`] answers for them, and that is the
/// point: an unresolved *data* import binds to zero, so a guest asking for the
/// currency code with a key nothing defines passes address zero rather than
/// getting the nil this answers.
pub const KEYS: &[(&str, &str)] = &[
    ("_NSLocaleIdentifier", "kCFLocaleIdentifier"),
    ("_NSLocaleLanguageCode", "kCFLocaleLanguageCode"),
    ("_NSLocaleCountryCode", "kCFLocaleCountryCode"),
    ("_NSLocaleScriptCode", "kCFLocaleScriptCode"),
    ("_NSLocaleVariantCode", "kCFLocaleVariantCode"),
    ("_NSLocaleUsesMetricSystem", "kCFLocaleUsesMetricSystem"),
    ("_NSLocaleMeasurementSystem", "kCFLocaleMeasurementSystem"),
    ("_NSLocaleDecimalSeparator", "kCFLocaleDecimalSeparator"),
    ("_NSLocaleGroupingSeparator", "kCFLocaleGroupingSeparator"),
    ("_NSLocaleCurrencySymbol", "kCFLocaleCurrencySymbol"),
    ("_NSLocaleCurrencyCode", "kCFLocaleCurrencyCode"),
    ("_NSLocaleCalendarIdentifier", "kCFLocaleCalendarIdentifier"),
    (
        "_NSLocaleCollationIdentifier",
        "kCFLocaleCollationIdentifier",
    ),
    // The quotation delimiters, published so the symbol resolves. `component`
    // answers nil for them, which is the point: which quotation marks a language
    // uses is CLDR data this tree does not carry, and a caller told nothing falls
    // back to its own - see `crate::formatter`.
    (
        "_kCFLocaleQuotationBeginDelimiterKey",
        "kCFLocaleQuotationBeginDelimiterKey",
    ),
    (
        "_kCFLocaleQuotationEndDelimiterKey",
        "kCFLocaleQuotationEndDelimiterKey",
    ),
    (
        "_kCFLocaleAlternateQuotationBeginDelimiterKey",
        "kCFLocaleAlternateQuotationBeginDelimiterKey",
    ),
    (
        "_kCFLocaleAlternateQuotationEndDelimiterKey",
        "kCFLocaleAlternateQuotationEndDelimiterKey",
    ),
    ("_kCFLocaleIdentifier", "kCFLocaleIdentifier"),
    ("_kCFLocaleLanguageCode", "kCFLocaleLanguageCode"),
    ("_kCFLocaleCountryCode", "kCFLocaleCountryCode"),
    ("_kCFLocaleScriptCode", "kCFLocaleScriptCode"),
    ("_kCFLocaleVariantCode", "kCFLocaleVariantCode"),
    ("_kCFLocaleUsesMetricSystem", "kCFLocaleUsesMetricSystem"),
    ("_kCFLocaleMeasurementSystem", "kCFLocaleMeasurementSystem"),
    ("_kCFLocaleDecimalSeparator", "kCFLocaleDecimalSeparator"),
    ("_kCFLocaleGroupingSeparator", "kCFLocaleGroupingSeparator"),
    ("_kCFLocaleCurrencySymbol", "kCFLocaleCurrencySymbol"),
    ("_kCFLocaleCurrencyCode", "kCFLocaleCurrencyCode"),
    (
        "_kCFLocaleCalendarIdentifier",
        "kCFLocaleCalendarIdentifier",
    ),
    (
        "_kCFLocaleCollationIdentifier",
        "kCFLocaleCollationIdentifier",
    ),
];

/// The CoreFoundation spellings, which name the same locale as the class above.
pub mod cf {
    // The C spellings are the interface; renaming them would only hide it.
    #![allow(non_snake_case)]

    use objc::object::Id;

    /// `CFLocaleRef CFLocaleCopyCurrent(void)`.
    ///
    /// A `Copy`, so the caller owns a reference and will release it - which is
    /// the only difference between this and [`CFLocaleGetSystem`] here.
    unsafe extern "C" fn CFLocaleCopyCurrent() -> Id {
        let locale = super::shared();
        objc::arc::retain(locale);
        locale
    }

    /// `CFStringRef CFLocaleGetIdentifier(CFLocaleRef)`.
    ///
    /// A `Get`, so the string has to outlive the call without the caller holding
    /// a reference: it is interned rather than made fresh.
    unsafe extern "C" fn CFLocaleGetIdentifier(_locale: Id) -> Id {
        interned(&super::identifier())
    }

    /// `CFTypeRef CFLocaleGetValue(CFLocaleRef, CFLocaleKey key)`.
    ///
    /// The same components `-objectForKey:` answers, by the same key text - a
    /// guest that reads `kCFLocaleLanguageCode` and one that reads
    /// `NSLocaleLanguageCode` are naming one string, so they cannot disagree.
    /// A `Get` again, so the value is interned.
    unsafe extern "C" fn CFLocaleGetValue(_locale: Id, key: Id) -> Id {
        // SAFETY: an `NSString`, or nil.
        let key = unsafe { crate::string::bytes(key) };
        if key.as_slice() == b"kCFLocaleUsesMetricSystem" {
            return corefoundation::collections::boolean(super::uses_metric()) as Id;
        }
        match super::component(key.as_slice()) {
            Some(value) => interned(&value),
            None => std::ptr::null_mut(),
        }
    }

    /// A string that outlives the call, one object per distinct text.
    ///
    /// Retained forever, which is what a `Get` requires and what makes handing
    /// the same object out twice safe: a guest that released one would make
    /// every later read of it dangle.
    fn interned(text: &str) -> Id {
        static POOL: std::sync::Mutex<Vec<(String, usize)>> = std::sync::Mutex::new(Vec::new());
        let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, object)) = pool.iter().find(|(t, _)| t == text) {
            return *object as Id;
        }
        let object = crate::string::from_str(text);
        objc::arc::make_immortal(object);
        pool.push((text.to_string(), object as usize));
        object
    }

    /// `CFLocaleRef CFLocaleGetSystem(void)`.
    ///
    /// A `Get`, so the caller does not own it and will not release it - which
    /// the singleton already is: retained forever, and handed out without a
    /// reference. It is the same object `+[NSLocale systemLocale]` answers, for
    /// the reason [`super::CURRENT`] records.
    unsafe extern "C" fn CFLocaleGetSystem() -> Id {
        super::shared()
    }

    /// `CFLocaleRef CFLocaleCreate(CFAllocatorRef, CFStringRef identifier)`.
    ///
    /// FIXME (ACCURACY): there is one locale object, so every identifier is
    /// answered with it rather than with the locale it names.
    unsafe extern "C" fn CFLocaleCreate(_allocator: Id, identifier: Id) -> Id {
        if identifier.is_null() {
            return std::ptr::null_mut();
        }
        unsafe { CFLocaleCopyCurrent() }
    }

    /// `CFArrayRef CFLocaleCopyPreferredLanguages(void)`.
    unsafe extern "C" fn CFLocaleCopyPreferredLanguages() -> Id {
        super::preferred_languages_array() as Id
    }

    /// `CFStringRef CFLocaleCreateCanonicalLanguageIdentifierFromString(
    /// CFAllocatorRef, CFStringRef localeIdentifier)`: BCP 47 casing and
    /// hyphens in place of underscores, as `en_us` -> `en-US` and
    /// `zh_hans_cn` -> `zh-Hans-CN`.
    ///
    /// FIXME (ACCURACY): a three-letter ISO 639-2 language code (`eng`) is kept
    /// as-is rather than mapped to its two-letter ISO 639-1 equivalent (`en`),
    /// which is the one case Apple's own canonicalization also changes the
    /// length of.
    unsafe extern "C" fn CFLocaleCreateCanonicalLanguageIdentifierFromString(
        _allocator: Id,
        identifier: Id,
    ) -> Id {
        // SAFETY: an `NSString`, or nil.
        let text = unsafe { crate::string::bytes(identifier) };
        let Ok(text) = std::str::from_utf8(&text) else {
            return std::ptr::null_mut();
        };
        if text.is_empty() {
            return std::ptr::null_mut();
        }
        let canonical: Vec<String> = text
            .split(['-', '_'])
            .enumerate()
            .map(
                |(i, part)| match (i, part.len(), part.chars().all(|c| c.is_ascii_alphabetic())) {
                    (0, _, _) => part.to_ascii_lowercase(),
                    (_, 4, true) => {
                        let mut chars = part.chars();
                        let Some(first) = chars.next() else {
                            return String::new();
                        };
                        first.to_ascii_uppercase().to_string()
                            + &chars.as_str().to_ascii_lowercase()
                    }
                    (_, 2, true) => part.to_ascii_uppercase(),
                    (_, 3, _) if part.chars().all(|c| c.is_ascii_digit()) => part.to_string(),
                    _ => part.to_ascii_lowercase(),
                },
            )
            .collect();
        crate::string::from_str(&canonical.join("-"))
    }

    exports::exports! {
        functions {
            "CFLocaleCreate" => CFLocaleCreate,
            "CFLocaleCopyPreferredLanguages" => CFLocaleCopyPreferredLanguages,
            "CFLocaleGetSystem" => CFLocaleGetSystem,
            "CFLocaleCopyCurrent" => CFLocaleCopyCurrent,
            "CFLocaleGetIdentifier" => CFLocaleGetIdentifier,
            "CFLocaleGetValue" => CFLocaleGetValue,
            "CFLocaleCreateCanonicalLanguageIdentifierFromString"
                => CFLocaleCreateCanonicalLanguageIdentifierFromString,
        }
    }
}

pub(crate) fn install() {
    let class =
        corefoundation::base::register(c"NSLocale", None, std::mem::size_of::<usize>(), false);
    class_methods!(class, {
        "currentLocale" => current_locale, c"@16@0:8";
        "autoupdatingCurrentLocale" => current_locale, c"@16@0:8";
        "systemLocale" => current_locale, c"@16@0:8";
        "preferredLanguages" => preferred_languages, c"@16@0:8";
    });
    methods!(class, {
        "objectForKey:" => object_for_key, c"@24@0:8@16";
        "localeIdentifier" => locale_identifier, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Get` hands back the same object every time and does not hand over a
    /// reference with it - so the one thing that must be true is that it is the
    /// singleton, and not a fresh locale the caller would be expected to free.
    #[test]
    fn the_system_locale_is_the_one_locale() {
        crate::bootstrap();
        let address = cf::symbol("CFLocaleGetSystem").expect("the name has to resolve");
        // SAFETY: the address of `CFLocaleGetSystem`, which takes no argument
        // and returns an object.
        let system: unsafe extern "C" fn() -> Id = unsafe { std::mem::transmute(address as usize) };
        let (first, second) = unsafe { (system(), system()) };
        assert!(!first.is_null());
        assert_eq!(first, second, "a Get answers with one object");
        assert_eq!(
            first,
            shared(),
            "and it is the locale every other call gets"
        );
    }

    /// `en_us` canonicalizes to `en-US`, and a script subtag is title-cased.
    #[test]
    fn canonical_language_identifier_fixes_hyphens_and_casing() {
        crate::bootstrap();
        let address = cf::symbol("CFLocaleCreateCanonicalLanguageIdentifierFromString")
            .expect("the name has to resolve");
        // SAFETY: the address of the function above, which takes two objects
        // and returns one the caller owns.
        let canonicalize: unsafe extern "C" fn(Id, Id) -> Id =
            unsafe { std::mem::transmute(address as usize) };
        let check = |input: &str, want: &str| {
            let text = crate::string::from_str(input);
            // SAFETY: a live `NSString`.
            let result = unsafe { canonicalize(std::ptr::null_mut(), text) };
            assert!(!result.is_null());
            // SAFETY: a live `NSString` this call owns.
            let got = String::from_utf8(unsafe { crate::string::bytes(result) }).unwrap();
            assert_eq!(got, want, "canonicalizing {input:?}");
            objc::arc::release(text);
            objc::arc::release(result);
        };
        check("en_us", "en-US");
        check("zh_hans_cn", "zh-Hans-CN");
        check("EN", "en");
    }

    /// The components a caller reads through the CoreFoundation spelling are
    /// the ones the class answers, and a component this host has no data for is
    /// nil rather than an invented one.
    #[test]
    fn the_locale_answers_the_components_it_can_derive() {
        crate::bootstrap();
        let address = cf::symbol("CFLocaleGetValue").expect("the name has to resolve");
        // SAFETY: the address of `CFLocaleGetValue`, which takes a locale and a
        // key and returns an object.
        let value: unsafe extern "C" fn(Id, Id) -> Id =
            unsafe { std::mem::transmute(address as usize) };
        let ask = |key: &str| {
            let key = crate::string::from_str(key);
            // SAFETY: a live locale and a string this test made.
            unsafe { value(shared(), key) }
        };
        let text = |object: Id| {
            // SAFETY: an `NSString` this runtime made, or nil.
            String::from_utf8(unsafe { crate::string::bytes(object) }).expect("utf-8")
        };
        assert_eq!(text(ask("kCFLocaleIdentifier")), identifier());
        assert_eq!(text(ask("kCFLocaleLanguageCode")), language());
        assert_eq!(text(ask("kCFLocaleCountryCode")), country());
        assert!(
            ask("kCFLocaleCurrencyCode").is_null(),
            "a currency is locale data this tree does not carry"
        );
        // A `Get`, so the same object each time rather than one per call.
        assert_eq!(ask("kCFLocaleIdentifier"), ask("kCFLocaleIdentifier"));
    }

    /// Whether the country measures in metres is a fact about the country, and
    /// the answer is a boolean object rather than a string.
    /// Whether the country measures in metres is a fact about the country, and
    /// the two answers derived from it cannot disagree.
    #[test]
    fn the_measurement_system_follows_the_country() {
        let system = component(b"kCFLocaleMeasurementSystem").expect("every country has one");
        assert_eq!(system == "Metric", uses_metric());
        assert_eq!(system == "U.S.", country() == "US");
        assert!(
            component(b"kCFLocaleDecimalSeparator").is_none(),
            "a separator is locale data this tree does not carry"
        );
    }

    #[test]
    fn a_posix_name_splits_into_a_language_and_a_country() {
        assert_eq!(split("fr_CA.UTF-8"), ("fr".into(), "CA".into()));
        assert_eq!(split("en_GB@euro"), ("en".into(), "GB".into()));
        assert_eq!(split("de"), ("de".into(), "US".into()));
        assert_eq!(split(""), ("en".into(), "US".into()));
    }
}
