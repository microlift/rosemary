use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// The URL a receiver holds, as text.
///
/// # Safety
///
/// `receiver` must be null or an object this runtime allocated.
pub unsafe fn text_of(receiver: Id) -> Option<String> {
    unsafe { corefoundation::url::as_url(receiver as corefoundation::base::CFTypeRef) }
        .map(|u| u.text.clone())
}

/// `+URLWithString:`, autoreleased.
///
/// nil for text that is not a URL, which is what Apple's answers and what a
/// caller checks for. "Not a URL" is only the empty string here: a scheme this
/// runtime does not recognise is still a URL, and refusing it would be this
/// module deciding what the transport is allowed to be asked for.
unsafe extern "C" fn url_with_string(_class: Id, _sel: Sel, text: Id) -> Id {
    objc::arc::autorelease(unsafe { make_url_with_string(text) })
}

unsafe fn make_url_with_string(text: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let bytes = unsafe { crate::string::bytes(text) };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return std::ptr::null_mut();
    };
    if text.is_empty() {
        return std::ptr::null_mut();
    }
    corefoundation::url::new(text) as Id
}

unsafe extern "C" fn init_with_string(receiver: Id, _sel: Sel, text: Id) -> Id {
    let url = unsafe { make_url_with_string(text) };
    objc::arc::release(receiver);
    url
}

unsafe fn make_file_url(path: Id, directory: Option<bool>) -> Id {
    let bytes = unsafe { crate::string::bytes(path) };
    let Ok(path) = std::str::from_utf8(&bytes) else {
        return std::ptr::null_mut();
    };
    make_file_url_str(path, directory)
}

fn make_file_url_str(path: &str, directory: Option<bool>) -> Id {
    let path = if std::path::Path::new(path).is_absolute() {
        path.to_string()
    } else {
        let Some(cwd) = std::env::current_dir()
            .ok()
            .and_then(|cwd| prefix::ns::to_guest(&cwd))
        else {
            return std::ptr::null_mut();
        };
        cwd.join(path).to_string_lossy().into_owned()
    };
    let directory = directory.unwrap_or_else(|| prefix::ns::existing_host_path(&path).is_dir());
    corefoundation::url::new_file_url(&path, directory) as Id
}

/// `+fileURLWithPath:` and `+fileURLWithPath:isDirectory:`.
unsafe extern "C" fn file_url_with_path(_class: Id, _sel: Sel, path: Id) -> Id {
    objc::arc::autorelease(unsafe { make_file_url(path, None) })
}

unsafe extern "C" fn file_url_with_path_is_directory(
    _class: Id,
    _sel: Sel,
    path: Id,
    directory: u8,
) -> Id {
    objc::arc::autorelease(unsafe { make_file_url(path, Some(directory != 0)) })
}

unsafe extern "C" fn init_file_url_with_path(receiver: Id, _sel: Sel, path: Id) -> Id {
    let url = unsafe { make_file_url(path, None) };
    objc::arc::release(receiver);
    url
}

unsafe extern "C" fn init_file_url_with_path_is_directory(
    receiver: Id,
    _sel: Sel,
    path: Id,
    directory: u8,
) -> Id {
    let url = unsafe { make_file_url(path, Some(directory != 0)) };
    objc::arc::release(receiver);
    url
}

/// A file URL for `path`, kept relative to `base` when `path` is relative.
///
/// The base is where a *relative* path is measured from and is ignored for an
/// absolute one, which is the rule for every one of Apple's relative-URL
/// constructors. Owned by the caller.
unsafe fn make_relative_file_url(path: Id, directory: Option<bool>, base: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let bytes = unsafe { crate::string::bytes(path) };
    let Ok(path) = std::str::from_utf8(&bytes) else {
        return std::ptr::null_mut();
    };
    // SAFETY: null or an `NSURL`.
    let base_path = unsafe { corefoundation::url::as_url(base as corefoundation::base::CFTypeRef) }
        .map(|u| u.path())
        .filter(|p| !p.is_empty());
    let Some(base_path) = base_path.filter(|_| !path.starts_with('/')) else {
        return make_file_url_str(path, directory);
    };
    let joined = format!("{}/{path}", base_path.trim_end_matches('/'));
    let absolute = make_file_url_str(&joined, directory);
    // SAFETY: a URL this function just made.
    let Some(text) = (unsafe { text_of(absolute) }) else {
        return absolute;
    };
    let mut relative = corefoundation::url::encode_path(path.trim_end_matches('/'));
    if text.ends_with('/') {
        relative.push('/');
    }
    objc::arc::release(absolute);
    corefoundation::url::new_relative(&text, &relative, base as corefoundation::base::CFTypeRef)
        as Id
}

/// `+fileURLWithPath:relativeToURL:` and the `isDirectory:` form.
unsafe extern "C" fn file_url_with_path_relative_to_url(
    _class: Id,
    _sel: Sel,
    path: Id,
    base: Id,
) -> Id {
    objc::arc::autorelease(unsafe { make_relative_file_url(path, None, base) })
}

unsafe extern "C" fn init_file_url_with_path_relative_to_url(
    receiver: Id,
    _sel: Sel,
    path: Id,
    base: Id,
) -> Id {
    let url = unsafe { make_relative_file_url(path, None, base) };
    objc::arc::release(receiver);
    url
}

unsafe extern "C" fn init_file_url_with_path_is_directory_relative_to_url(
    receiver: Id,
    _sel: Sel,
    path: Id,
    directory: u8,
    base: Id,
) -> Id {
    let url = unsafe { make_relative_file_url(path, Some(directory != 0), base) };
    objc::arc::release(receiver);
    url
}

/// One accessor, given what it reads off the contents.
macro_rules! accessor {
    ($name:ident, $read:expr) => {
        unsafe extern "C" fn $name(receiver: Id, _sel: Sel) -> Id {
            // SAFETY: the receiver is an `NSURL`.
            let Some(url) = (unsafe {
                corefoundation::url::as_url(receiver as corefoundation::base::CFTypeRef)
            }) else {
                return std::ptr::null_mut();
            };
            let read: fn(&corefoundation::url::Contents) -> Option<String> = $read;
            match read(url) {
                Some(text) => autoreleased(&text),
                None => std::ptr::null_mut(),
            }
        }
    };
}

accessor!(absolute_string, |u| Some(u.text.clone()));
accessor!(path, |u| Some(u.path()).filter(|p| !p.is_empty()));
accessor!(scheme, |u| Some(u.scheme()).filter(|s| !s.is_empty()));
accessor!(host, |u| host_of(&u.text));
accessor!(user, |u| u.user());
accessor!(password, |u| u.password().map(str::to_string));
accessor!(query, |u| u.query().map(str::to_string));
accessor!(fragment, |u| u.fragment().map(str::to_string));
accessor!(resource_specifier, |u| Some(if u.scheme().is_empty() {
    u.text.clone()
} else {
    u.text.split_once(':').unwrap().1.to_string()
}));

unsafe extern "C" fn absolute_url(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is an `NSURL`.
    match unsafe { corefoundation::url::as_url(receiver as corefoundation::base::CFTypeRef) } {
        // A relative URL's absolute form has no base of its own.
        Some(u) if u.relative.is_some() => {
            objc::arc::autorelease(corefoundation::url::new(&u.text) as Id)
        }
        _ => objc::arc::autorelease(objc::arc::retain(receiver)),
    }
}

unsafe extern "C" fn port(receiver: Id, _sel: Sel) -> Id {
    let port = unsafe { corefoundation::api::CFURLGetPortNumber(receiver as _) };
    if port < 0 {
        return std::ptr::null_mut();
    }
    let number = corefoundation::collections::new_number(
        corefoundation::collections::Number::Integer(i64::from(port)),
    );
    unsafe { corefoundation::api::CFAutorelease(number) as Id }
}

unsafe extern "C" fn has_directory_path(receiver: Id, _sel: Sel) -> u8 {
    unsafe { corefoundation::api::CFURLHasDirectoryPath(receiver as _) }
}

unsafe extern "C" fn get_file_system_representation(
    receiver: Id,
    _sel: Sel,
    buffer: *mut std::ffi::c_char,
    length: usize,
) -> u8 {
    let Some(url) = (unsafe { corefoundation::url::as_url(receiver as _) }) else {
        return 0;
    };
    let Some(path) = url.file_path() else {
        return 0;
    };
    // Foundation counts a directory's trailing slash when checking capacity,
    // although it omits that slash from the returned file system spelling.
    let directory_marker = usize::from(path != "/" && url.encoded_path().ends_with('/'));
    if buffer.is_null()
        || path.len().saturating_add(directory_marker) >= length
        || path.contains('\0')
    {
        return 0;
    }
    // SAFETY: the caller supplies `length` writable bytes, including the NUL.
    unsafe {
        std::ptr::copy_nonoverlapping(path.as_ptr(), buffer.cast(), path.len());
        buffer.add(path.len()).write(0);
    }
    1
}

/// A URL's host: the authority without its user information or port, and an
/// IPv6 literal without its brackets. `None` for a URL with no authority.
fn host_of(text: &str) -> Option<String> {
    let (_, rest) = text.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match authority.strip_prefix('[') {
        Some(literal) => literal.split(']').next()?,
        None => authority.split(':').next()?,
    };
    (!host.is_empty()).then(|| host.to_string())
}

/// `-fileSystemRepresentation`, which is a `const char *` into storage the URL
/// keeps alive - so it is autoreleased as an `NSString` and its bytes handed
/// back, which is where the guest reads them from until the pool drains.
unsafe extern "C" fn file_system_representation(
    receiver: Id,
    _sel: Sel,
) -> *const std::ffi::c_char {
    // SAFETY: the receiver is an `NSURL`.
    let Some(url) =
        (unsafe { corefoundation::url::as_url(receiver as corefoundation::base::CFTypeRef) })
    else {
        return std::ptr::null();
    };
    let Some(path) = url.file_path() else {
        return std::ptr::null();
    };
    let string = crate::string::from_str(&path);
    // SAFETY: a fresh string, autoreleased so its bytes outlive this call by as
    // long as the pool does; `-UTF8String` is `const char * (id, SEL)`.
    unsafe {
        corefoundation::api::CFAutorelease(string as corefoundation::base::CFTypeRef);
        let sel = objc::sel::get("UTF8String");
        match objc::object::class_of(string).and_then(|c| c.method(sel)) {
            Some(method) => {
                let f: unsafe extern "C" fn(Id, Sel) -> *const std::ffi::c_char =
                    std::mem::transmute(method.implementation());
                f(string, sel)
            }
            None => std::ptr::null(),
        }
    }
}

unsafe extern "C" fn is_file_url(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: the receiver is an `NSURL`.
    let is_file =
        unsafe { corefoundation::url::as_url(receiver as corefoundation::base::CFTypeRef) }
            .is_some_and(|u| u.is_file_url());
    u8::from(is_file)
}

fn autoreleased(text: &str) -> Id {
    let string = crate::string::from_str(text);
    // SAFETY: a string this call just created.
    unsafe { corefoundation::api::CFAutorelease(string as corefoundation::base::CFTypeRef) as Id }
}

/// The file path a URL stands for, for the path arithmetic below.
///
/// `None` for a URL that is not a file URL: appending a path component to an
/// `https:` URL is a different operation - the query and fragment come after
/// the path and would end up inside it - and nothing here needs it, so it is
/// refused rather than approximated.
///
/// # Safety
///
/// `receiver` is null or an `NSURL`.
unsafe fn file_path_of(receiver: Id) -> Option<String> {
    unsafe { corefoundation::url::as_url(receiver as corefoundation::base::CFTypeRef) }?.file_path()
}

fn autoreleased_file_url(path: &str, directory: bool) -> Id {
    let url = corefoundation::url::new_file_url(path, directory);
    // SAFETY: a URL this call just made.
    unsafe { corefoundation::api::CFAutorelease(url) as Id }
}

/// Collapse the path-only components Foundation standardizes without consulting
/// the host filesystem. Following symlinks here would make `..` mean something
/// different from the spelling in the URL.
pub(crate) fn standardized_file_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|part| *part != "..") => {
                parts.pop();
            }
            ".." if absolute => {}
            other => parts.push(other),
        }
    }

    let mut standardized = parts.join("/");
    if absolute {
        standardized.insert(0, '/');
    }
    if standardized.is_empty() {
        standardized.push(if absolute { '/' } else { '.' });
    }
    standardized
}

/// `-URLByStandardizingPath` for file URLs.
unsafe extern "C" fn url_by_standardizing_path(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is an `NSURL`.
    let Some(url) =
        (unsafe { corefoundation::url::as_url(receiver as corefoundation::base::CFTypeRef) })
    else {
        return std::ptr::null_mut();
    };
    let Some(path) = url.file_path() else {
        // Path standardization is meaningful for file URLs. Preserve another
        // URL unchanged rather than corrupting its authority, query or fragment.
        return unsafe {
            corefoundation::api::CFAutorelease(corefoundation::api::CFRetain(
                receiver as corefoundation::base::CFTypeRef,
            )) as Id
        };
    };
    let directory = url.text.ends_with('/');
    autoreleased_file_url(&standardized_file_path(&path), directory)
}

/// `-URLByAppendingPathComponent:`, and the form that says what it is making.
///
/// The single-argument form asks the file system whether the result is a
/// directory, as Apple's does - the trailing slash is the only thing that
/// distinguishes the two URLs, and a caller comparing one against a URL it got
/// from `-URLsForDirectory:` needs them to agree.
unsafe extern "C" fn url_by_appending_path_component(receiver: Id, _sel: Sel, name: Id) -> Id {
    // SAFETY: the receiver is an `NSURL`, and `name` null or an `NSString`.
    let (Some(base), bytes) = (unsafe { file_path_of(receiver) }, unsafe {
        crate::string::bytes(name)
    }) else {
        return std::ptr::null_mut();
    };
    let Ok(name) = String::from_utf8(bytes) else {
        return std::ptr::null_mut();
    };
    let joined = format!("{}/{name}", base.trim_end_matches('/'));
    let directory = std::path::Path::new(&joined).is_dir();
    autoreleased_file_url(&joined, directory)
}

unsafe extern "C" fn url_by_appending_path_component_is_directory(
    receiver: Id,
    _sel: Sel,
    name: Id,
    directory: u8,
) -> Id {
    let (Some(base), bytes) = (unsafe { file_path_of(receiver) }, unsafe {
        crate::string::bytes(name)
    }) else {
        return std::ptr::null_mut();
    };
    let Ok(name) = String::from_utf8(bytes) else {
        return std::ptr::null_mut();
    };
    autoreleased_file_url(
        &format!("{}/{name}", base.trim_end_matches('/')),
        directory != 0,
    )
}

unsafe extern "C" fn url_by_appending_path_extension(receiver: Id, _sel: Sel, extension: Id) -> Id {
    let (Some(base), bytes) = (unsafe { file_path_of(receiver) }, unsafe {
        crate::string::bytes(extension)
    }) else {
        return std::ptr::null_mut();
    };
    let Ok(extension) = String::from_utf8(bytes) else {
        return std::ptr::null_mut();
    };
    autoreleased_file_url(&format!("{base}.{extension}"), false)
}

/// `-URLByDeletingLastPathComponent`, which always names a directory.
unsafe extern "C" fn url_by_deleting_last_path_component(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is an `NSURL`.
    let Some(base) = (unsafe { file_path_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    let parent = match base.trim_end_matches('/').rsplit_once('/') {
        // The root's parent is the root, which is what Apple answers.
        Some(("", _)) | None => "/".to_string(),
        Some((head, _)) => head.to_string(),
    };
    autoreleased_file_url(&parent, true)
}

unsafe extern "C" fn last_path_component(receiver: Id, _sel: Sel) -> Id {
    let Some(url) = (unsafe { corefoundation::url::as_url(receiver as _) }) else {
        return std::ptr::null_mut();
    };
    let base = url.path();
    let trimmed = base.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or_default();
    autoreleased(if base.starts_with('/') && last.is_empty() {
        "/"
    } else {
        last
    })
}

unsafe extern "C" fn path_extension(receiver: Id, _sel: Sel) -> Id {
    let Some(url) = (unsafe { corefoundation::url::as_url(receiver as _) }) else {
        return std::ptr::null_mut();
    };
    let path = url.path();
    let component = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let extension = component
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map_or("", |(_, ext)| ext);
    autoreleased(extension)
}

unsafe extern "C" fn path_components(receiver: Id, _sel: Sel) -> Id {
    let Some(url) = (unsafe { corefoundation::url::as_url(receiver as _) }) else {
        return std::ptr::null_mut();
    };
    let path = url.path();
    let mut parts = Vec::new();
    if path.starts_with('/') {
        parts.push(crate::string::from_str("/") as corefoundation::base::CFTypeRef);
    }
    parts.extend(
        path.split('/')
            .filter(|part| !part.is_empty())
            .map(|part| crate::string::from_str(part) as corefoundation::base::CFTypeRef),
    );
    let array = corefoundation::collections::new_array(
        &parts,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: the array retained the strings this call owns.
    unsafe {
        for part in parts {
            corefoundation::api::CFRelease(part);
        }
        corefoundation::api::CFAutorelease(array) as Id
    }
}

unsafe extern "C" fn url_by_deleting_path_extension(receiver: Id, _sel: Sel) -> Id {
    let url = unsafe {
        corefoundation::api::CFURLCreateCopyDeletingPathExtension(std::ptr::null(), receiver as _)
    };
    unsafe { corefoundation::api::CFAutorelease(url) as Id }
}

fn reachability_error_code(error: &std::io::Error) -> i64 {
    match error.raw_os_error() {
        Some(libc::ENOENT) => 260,
        Some(libc::EACCES | libc::EPERM) => 257,
        Some(libc::ENAMETOOLONG | libc::EINVAL) => 258,
        _ if error.kind() == std::io::ErrorKind::InvalidInput => 258,
        _ => 256,
    }
}

/// Reachability concerns the directory entry, including a dangling symlink.
/// On success Foundation leaves the caller's error word untouched.
unsafe extern "C" fn check_resource_is_reachable(receiver: Id, _sel: Sel, error: *mut Id) -> u8 {
    let Some(path) = (unsafe { file_path_of(receiver) }) else {
        if !error.is_null() {
            unsafe {
                *error = crate::error::make(
                    "NSCocoaErrorDomain",
                    262,
                    "The URL does not refer to a file system resource.",
                )
            };
        }
        return 0;
    };
    let Err(cause) = std::fs::symlink_metadata(prefix::ns::existing_host_path(&path)) else {
        return 1;
    };
    if error.is_null() {
        return 0;
    }
    let code = reachability_error_code(&cause);
    let description = format!("The resource at {path} could not be reached: {cause}");
    let path_value = crate::string::from_str(&path);
    let description_value = crate::string::from_str(&description);
    let mut pairs = vec![
        (crate::string::from_str("NSFilePath") as _, path_value as _),
        (crate::string::from_str("NSURL") as _, receiver as _),
        (
            crate::string::from_str("NSLocalizedDescription") as _,
            description_value as _,
        ),
    ];
    if let Some(errno) = cause.raw_os_error() {
        let underlying = crate::error::make(
            "NSPOSIXErrorDomain",
            i64::from(darwin::errno::from_host(errno)),
            &cause.to_string(),
        );
        pairs.push((
            crate::string::from_str("NSUnderlyingError") as _,
            underlying as _,
        ));
    }
    let info = corefoundation::collections::new_dictionary(
        &pairs,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: writable error output; the dictionary retained every entry.
    unsafe {
        *error =
            crate::error::make_with_user_info("NSCocoaErrorDomain", code, &description, info as Id);
        for (key, _) in &pairs {
            corefoundation::api::CFRelease(*key);
        }
        objc::arc::release(path_value);
        objc::arc::release(description_value);
        corefoundation::api::CFRelease(info);
    }
    0
}

/// The value of one `NSURLResourceKey` for `path`, or `None` for a key this
/// tree has no answer for.
///
/// Absent rather than zero, and the distinction is the whole contract: Apple
/// leaves a key it cannot answer *out* of the dictionary, and a caller that
/// reads a size of zero for a file it just wrote would conclude the write
/// failed.
fn resource_value(path: &str, key: &str) -> Option<corefoundation::base::CFTypeRef> {
    use corefoundation::urlresource::{ADDED_TO_DIRECTORY_DATE, CONTENT_MODIFICATION_DATE};
    let number = |value: i64| {
        corefoundation::collections::new_number(corefoundation::collections::Number::Integer(value))
    };
    let modified = || {
        let time = std::fs::symlink_metadata(prefix::ns::existing_host_path(path))
            .ok()?
            .modified()
            .ok()?;
        let seconds = match time.duration_since(std::time::UNIX_EPOCH) {
            Ok(forward) => forward.as_secs_f64(),
            Err(back) => -back.duration().as_secs_f64(),
        };
        Some(objc::arc::retain(crate::timer::date_since_1970(seconds))
            as corefoundation::base::CFTypeRef)
    };
    match key {
        CONTENT_MODIFICATION_DATE => modified(),
        // No Linux filesystem records when an entry arrived in its directory;
        // the modification date is the nearest thing it does record, and a
        // Finder sorting by "date added" needs some date for every entry.
        ADDED_TO_DIRECTORY_DATE => modified(),
        // Value type `UTType`, not a string - see `uttype::for_path`. Already
        // a fresh +1 reference, like every other object this function hands
        // back for a caller to release or autorelease.
        "NSURLContentTypeKey" => {
            let host = prefix::ns::existing_host_path(path);
            crate::uttype::for_path(&host).map(|ut| ut as corefoundation::base::CFTypeRef)
        }
        // There is no backup daemon here, so nothing is ever excluded from one.
        // Answered rather than omitted because a caller reads it back to
        // confirm the exclusion it just asked for, and a missing key reads as a
        // failure it would then report.
        "NSURLIsExcludedFromBackupKey" => Some(number(0)),
        _ => corefoundation::urlresource::property(path, key),
    }
}

/// `-resourceValuesForKeys:error:`
///
/// # Safety
///
/// `keys` is null or an `NSArray` of `NSString`, and `error` null or writable.
unsafe extern "C" fn resource_values_for_keys(
    receiver: Id,
    _sel: Sel,
    keys: Id,
    error: *mut Id,
) -> Id {
    if !error.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *error = std::ptr::null_mut() };
    }
    // SAFETY: the receiver is an `NSURL`.
    let Some(path) = (unsafe { file_path_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: null or an array.
    let Some(keys) =
        (unsafe { corefoundation::collections::array(keys as corefoundation::base::CFTypeRef) })
    else {
        return std::ptr::null_mut();
    };

    let mut pairs: Vec<(
        corefoundation::base::CFTypeRef,
        corefoundation::base::CFTypeRef,
    )> = Vec::new();
    for key in keys.values.clone() {
        // SAFETY: an element of a live array, which a caller of this method
        // fills with `NSString`s.
        let name =
            String::from_utf8(unsafe { crate::string::bytes(key as Id) }).unwrap_or_default();
        if let Some(value) = resource_value(&path, &name) {
            pairs.push((key, value));
        }
    }
    let dictionary = corefoundation::collections::new_dictionary(
        &pairs,
        corefoundation::callbacks::Policy::from_key_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for (_, value) in &pairs {
        // SAFETY: an object this function made and the dictionary has retained.
        // The keys are the caller's and are not released here.
        unsafe { corefoundation::api::CFRelease(*value) };
    }
    // SAFETY: a fresh dictionary.
    unsafe { corefoundation::api::CFAutorelease(dictionary) as Id }
}

/// `-getResourceValue:forKey:error:`, the one-key form.
///
/// # Safety
///
/// `out` is a writable `id`, and `error` null or writable.
unsafe extern "C" fn get_resource_value(
    receiver: Id,
    _sel: Sel,
    out: *mut Id,
    key: Id,
    error: *mut Id,
) -> u8 {
    if !error.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *error = std::ptr::null_mut() };
    }
    if out.is_null() {
        return 0;
    }
    // SAFETY: the caller states it is writable.
    unsafe { *out = std::ptr::null_mut() };
    // SAFETY: the receiver is an `NSURL`, and `key` null or an `NSString`.
    let (Some(path), name) = (unsafe { file_path_of(receiver) }, unsafe {
        crate::string::bytes(key)
    }) else {
        return 0;
    };
    let name = String::from_utf8(name).unwrap_or_default();
    // A key with no value is *success with nil*, which is what Apple documents:
    // "the property is not available" is not an error.
    if let Some(value) = resource_value(&path, &name) {
        // SAFETY: an object this function made, handed over autoreleased.
        unsafe { *out = corefoundation::api::CFAutorelease(value) as Id };
    }
    1
}

/// `-setResourceValue:forKey:error:`, which accepts and records nothing.
///
/// The only key a program sets here is `NSURLIsExcludedFromBackupKey`, and
/// there is no backup daemon on this host to exclude anything from - so the
/// request has already been granted. It succeeds rather than failing because a
/// caller treats a failure as "this directory is not usable" and gives up on
/// it.
unsafe extern "C" fn set_resource_value(
    _receiver: Id,
    _sel: Sel,
    _value: Id,
    _key: Id,
    error: *mut Id,
) -> u8 {
    if !error.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *error = std::ptr::null_mut() };
    }
    1
}

/// Resolve `reference` against an absolute `base` (RFC 3986 section 5.2).
///
/// URLs in this runtime are stored already resolved, so a "relative" URL is
/// resolved at construction and `-baseURL` is always nil.
fn resolve_reference(base: &corefoundation::url::Contents, reference: &str) -> String {
    let has_scheme = {
        let probe = corefoundation::url::Contents::probe(reference);
        !probe.is_empty()
    };
    if has_scheme {
        return reference.to_string();
    }
    let text = base.text.split('#').next().unwrap_or_default();
    let scheme = base.scheme();
    let authority_end = {
        // Everything before the path: "scheme://authority".
        let path_start = base.encoded_path().as_ptr() as usize - base.text.as_ptr() as usize;
        path_start.min(text.len())
    };
    let prefix = &text[..authority_end];
    if reference.is_empty() {
        return text.to_string();
    }
    if let Some(rest) = reference.strip_prefix("//") {
        return format!("{scheme}://{rest}");
    }
    if reference.starts_with('#') {
        return format!("{text}{reference}");
    }
    if reference.starts_with('?') {
        let end = prefix.len() + base.encoded_path().len();
        return format!("{}{reference}", &text[..end]);
    }
    let (path_part, suffix) = match reference.find(['?', '#']) {
        Some(i) => reference.split_at(i),
        None => (reference, ""),
    };
    let merged = if path_part.starts_with('/') {
        path_part.to_string()
    } else {
        let base_path = base.encoded_path();
        let dir = base_path.rfind('/').map_or("", |i| &base_path[..=i]);
        let dir = if dir.is_empty() && base.authority().is_some() {
            "/"
        } else {
            dir
        };
        format!("{dir}{path_part}")
    };
    let keeps_slash = merged.ends_with('/') || merged.ends_with("/.") || merged.ends_with("/..");
    let mut normalized = standardized_file_path(&merged);
    if keeps_slash && !normalized.ends_with('/') {
        normalized.push('/');
    }
    format!("{prefix}{normalized}{suffix}")
}

unsafe fn make_url_relative_to(text: Id, base: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let bytes = unsafe { crate::string::bytes(text) };
    let Ok(reference) = std::str::from_utf8(&bytes) else {
        return std::ptr::null_mut();
    };
    // SAFETY: null or an `NSURL`.
    match unsafe { corefoundation::url::as_url(base as corefoundation::base::CFTypeRef) } {
        Some(contents) => corefoundation::url::new_relative(
            &resolve_reference(contents, reference),
            reference,
            base as corefoundation::base::CFTypeRef,
        ) as Id,
        None => unsafe { make_url_with_string(text) },
    }
}

/// `+URLWithString:relativeToURL:`.
unsafe extern "C" fn url_with_string_relative_to_url(
    _class: Id,
    _sel: Sel,
    text: Id,
    base: Id,
) -> Id {
    objc::arc::autorelease(unsafe { make_url_relative_to(text, base) })
}

unsafe extern "C" fn init_with_string_relative_to_url(
    receiver: Id,
    _sel: Sel,
    text: Id,
    base: Id,
) -> Id {
    let url = unsafe { make_url_relative_to(text, base) };
    objc::arc::release(receiver);
    url
}

/// `+fileURLWithPath:isDirectory:relativeToURL:`.
unsafe extern "C" fn file_url_with_path_is_directory_relative_to_url(
    _class: Id,
    _sel: Sel,
    path: Id,
    directory: u8,
    base: Id,
) -> Id {
    objc::arc::autorelease(unsafe { make_relative_file_url(path, Some(directory != 0), base) })
}

/// `-relativePath`: the path of the reference this URL was made from, or the
/// whole path when it has no base.
unsafe extern "C" fn relative_path(receiver: Id, sel: Sel) -> Id {
    // SAFETY: the receiver is an `NSURL`.
    let relative =
        unsafe { corefoundation::url::as_url(receiver as corefoundation::base::CFTypeRef) }
            .and_then(|u| u.relative.clone());
    match relative {
        Some(text) => {
            let path = corefoundation::url::Contents::probe_path(&text);
            autoreleased(&path)
        }
        None => unsafe { path(receiver, sel) },
    }
}

/// `-relativeString`: the reference as written, or the whole URL.
unsafe extern "C" fn relative_string(receiver: Id, sel: Sel) -> Id {
    // SAFETY: the receiver is an `NSURL`.
    let relative =
        unsafe { corefoundation::url::as_url(receiver as corefoundation::base::CFTypeRef) }
            .and_then(|u| u.relative.clone());
    match relative {
        Some(text) => autoreleased(&text),
        None => unsafe { absolute_string(receiver, sel) },
    }
}

/// `-baseURL`: the URL a relative one was resolved against, else nil.
unsafe extern "C" fn base_url(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is an `NSURL`.
    let base = unsafe { corefoundation::url::as_url(receiver as corefoundation::base::CFTypeRef) }
        .map_or(std::ptr::null(), |u| u.base());
    if base.is_null() {
        return std::ptr::null_mut();
    }
    objc::arc::autorelease(objc::arc::retain(base as Id))
}

/// `-description`: `relative -- base` for a relative URL.
unsafe extern "C" fn url_description(receiver: Id, sel: Sel) -> Id {
    // SAFETY: the receiver is an `NSURL`.
    let parts = unsafe { corefoundation::url::as_url(receiver as corefoundation::base::CFTypeRef) }
        .and_then(|u| {
            let base = unsafe { corefoundation::url::as_url(u.base()) }?;
            Some(format!("{} -- {}", u.relative.as_deref()?, base.text))
        });
    match parts {
        Some(text) => autoreleased(&text),
        None => unsafe { absolute_string(receiver, sel) },
    }
}

/// `-standardizedURL`, which is `-URLByStandardizingPath` for file URLs.
unsafe extern "C" fn standardized_url(receiver: Id, sel: Sel) -> Id {
    unsafe { url_by_standardizing_path(receiver, sel) }
}

/// `-filePathURL`: the path form of a file URL, which for a file reference URL
/// is the URL of the file it names; nil for any other scheme.
unsafe extern "C" fn file_path_url(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is an `NSURL`.
    let Some(url) =
        (unsafe { corefoundation::url::as_url(receiver as corefoundation::base::CFTypeRef) })
    else {
        return std::ptr::null_mut();
    };
    match &url.reference_target {
        Some(target) => objc::arc::autorelease(corefoundation::url::new(target) as Id),
        None if url.is_file_url() => objc::arc::autorelease(objc::arc::retain(receiver)),
        None => std::ptr::null_mut(),
    }
}

/// `-isFileReferenceURL`.
unsafe extern "C" fn is_file_reference_url(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: the receiver is an `NSURL`.
    let is_reference =
        unsafe { corefoundation::url::as_url(receiver as corefoundation::base::CFTypeRef) }
            .is_some_and(|u| u.reference_target.is_some());
    u8::from(is_reference)
}

/// `-fileReferenceURL`: `file:///.file/id=<device>.<inode>`, which keeps naming
/// the file across renames. nil when the file does not exist or the URL is not
/// a file URL, as Apple answers.
unsafe extern "C" fn file_reference_url(receiver: Id, _sel: Sel) -> Id {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: the receiver is an `NSURL`.
    let Some(url) =
        (unsafe { corefoundation::url::as_url(receiver as corefoundation::base::CFTypeRef) })
    else {
        return std::ptr::null_mut();
    };
    if url.reference_target.is_some() {
        return objc::arc::autorelease(objc::arc::retain(receiver));
    }
    let Some(path) = url.file_path() else {
        return std::ptr::null_mut();
    };
    let Ok(meta) = std::fs::metadata(prefix::ns::existing_host_path(&path)) else {
        return std::ptr::null_mut();
    };
    let target = corefoundation::url::new_file_url(&path, meta.is_dir());
    // SAFETY: a URL this call just made.
    let target_text = unsafe { text_of(target as Id) }.unwrap_or_default();
    objc::arc::release(target as Id);
    let id = format!("{}.{}", meta.dev(), meta.ino());
    objc::arc::autorelease(corefoundation::url::new_file_reference(&id, &target_text) as Id)
}

/// `-URLByResolvingSymlinksInPath`: symlinks are resolved when the path exists;
/// otherwise the path is standardized lexically, as Apple does.
unsafe extern "C" fn url_by_resolving_symlinks_in_path(receiver: Id, sel: Sel) -> Id {
    // SAFETY: the receiver is an `NSURL`.
    let Some(path) = (unsafe { file_path_of(receiver) }) else {
        return objc::arc::autorelease(objc::arc::retain(receiver));
    };
    let host = prefix::ns::existing_host_path(&path);
    match std::fs::canonicalize(&host)
        .ok()
        .and_then(|resolved| prefix::ns::to_guest(&resolved))
    {
        Some(guest) => {
            let guest = guest.to_string_lossy().into_owned();
            autoreleased_file_url(&guest, host.is_dir())
        }
        None => unsafe { url_by_standardizing_path(receiver, sel) },
    }
}

pub(crate) fn install() {
    let class = corefoundation::url::class();
    // On `NSURL` rather than on the concrete class, so `+URLWithString:` reaches
    // the spelling a guest actually sends it to.
    let abstract_class = objc::class::lookup(c"NSURL").expect("registered by corefoundation");
    class_methods!(abstract_class, {
        "URLWithString:" => url_with_string, c"@24@0:8@16";
        "fileURLWithPath:" => file_url_with_path, c"@24@0:8@16";
        "fileURLWithPath:isDirectory:" => file_url_with_path_is_directory, c"@28@0:8@16c24";
        "fileURLWithPath:relativeToURL:" => file_url_with_path_relative_to_url,
            c"@32@0:8@16@24";
        "fileURLWithPath:isDirectory:relativeToURL:"
            => file_url_with_path_is_directory_relative_to_url, c"@36@0:8@16c24@28";
        "URLWithString:relativeToURL:" => url_with_string_relative_to_url, c"@32@0:8@16@24";
    });
    methods!(abstract_class, {
        "initWithString:" => init_with_string, c"@24@0:8@16";
        "initWithString:relativeToURL:" => init_with_string_relative_to_url, c"@32@0:8@16@24";
        "initFileURLWithPath:relativeToURL:" => init_file_url_with_path_relative_to_url,
            c"@32@0:8@16@24";
        "initFileURLWithPath:isDirectory:relativeToURL:"
            => init_file_url_with_path_is_directory_relative_to_url, c"@36@0:8@16c24@28";
        "initFileURLWithPath:" => init_file_url_with_path, c"@24@0:8@16";
        "initFileURLWithPath:isDirectory:" => init_file_url_with_path_is_directory, c"@28@0:8@16c24";
    });
    methods!(class, {
        "copy" => url_copy, c"@16@0:8";
        "copyWithZone:" => url_copy_with_zone, c"@24@0:8^v16";
        "absoluteString" => absolute_string, c"@16@0:8";
        "absoluteURL" => absolute_url, c"@16@0:8";
        "description" => url_description, c"@16@0:8";
        "path" => path, c"@16@0:8";
        "relativePath" => relative_path, c"@16@0:8";
        "relativeString" => relative_string, c"@16@0:8";
        "baseURL" => base_url, c"@16@0:8";
        "standardizedURL" => standardized_url, c"@16@0:8";
        "filePathURL" => file_path_url, c"@16@0:8";
        "fileReferenceURL" => file_reference_url, c"@16@0:8";
        "isFileReferenceURL" => is_file_reference_url, c"c16@0:8";
        "URLByResolvingSymlinksInPath" => url_by_resolving_symlinks_in_path, c"@16@0:8";
        "scheme" => scheme, c"@16@0:8";
        "host" => host, c"@16@0:8";
        "user" => user, c"@16@0:8";
        "password" => password, c"@16@0:8";
        "port" => port, c"@16@0:8";
        "query" => query, c"@16@0:8";
        "fragment" => fragment, c"@16@0:8";
        "resourceSpecifier" => resource_specifier, c"@16@0:8";
        "isFileURL" => is_file_url, c"c16@0:8";
        "hasDirectoryPath" => has_directory_path, c"c16@0:8";
        "fileSystemRepresentation" => file_system_representation, c"*16@0:8";
        "getFileSystemRepresentation:maxLength:" => get_file_system_representation, c"c32@0:8*16Q24";
        "checkResourceIsReachableAndReturnError:" => check_resource_is_reachable, c"c24@0:8^@16";
        "URLByAppendingPathComponent:" => url_by_appending_path_component, c"@24@0:8@16";
        "URLByAppendingPathComponent:isDirectory:"
            => url_by_appending_path_component_is_directory, c"@28@0:8@16c24";
        "URLByAppendingPathExtension:" => url_by_appending_path_extension, c"@24@0:8@16";
        "URLByDeletingLastPathComponent" => url_by_deleting_last_path_component, c"@16@0:8";
        "URLByStandardizingPath" => url_by_standardizing_path, c"@16@0:8";
        "URLByDeletingPathExtension" => url_by_deleting_path_extension, c"@16@0:8";
        "lastPathComponent" => last_path_component, c"@16@0:8";
        "pathExtension" => path_extension, c"@16@0:8";
        "pathComponents" => path_components, c"@16@0:8";
        "resourceValuesForKeys:error:" => resource_values_for_keys, c"@32@0:8@16^@24";
        "getResourceValue:forKey:error:" => get_resource_value, c"B40@0:8^@16@24^@32";
        "setResourceValue:forKey:error:" => set_resource_value, c"B40@0:8@16@24^@32";
    });
}

unsafe extern "C" fn url_copy(receiver: Id, _sel: Sel) -> Id {
    objc::arc::retain(receiver)
}

unsafe extern "C" fn url_copy_with_zone(receiver: Id, sel: Sel, _zone: *const ()) -> Id {
    unsafe { url_copy(receiver, sel) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reachability_maps_host_failures_to_cocoa_read_errors() {
        for (errno, expected) in [
            (libc::ENOENT, 260),
            (libc::EACCES, 257),
            (libc::EPERM, 257),
            (libc::ENAMETOOLONG, 258),
            (libc::EINVAL, 258),
            (libc::ENOTDIR, 256),
            (libc::EIO, 256),
        ] {
            assert_eq!(
                super::reachability_error_code(&std::io::Error::from_raw_os_error(errno)),
                expected
            );
        }
    }

    #[test]
    fn host_drops_user_information_port_and_brackets() {
        assert_eq!(
            super::host_of("https://user:pw@messages.example.test:443/message?msg0=1"),
            Some("messages.example.test".to_string())
        );
        assert_eq!(
            super::host_of("http://[::1]:8080/"),
            Some("::1".to_string())
        );
        assert_eq!(super::host_of("file:///tmp/x"), None);
        assert_eq!(super::host_of("example-studio-auth:/?code=1"), None);
    }

    use super::standardized_file_path;

    #[test]
    fn standardizes_file_path_lexically() {
        assert_eq!(standardized_file_path("/a//b/../c/."), "/a/c");
        assert_eq!(standardized_file_path("/../../a"), "/a");
        assert_eq!(standardized_file_path("a/../../b"), "../b");
        assert_eq!(standardized_file_path("/"), "/");
    }

    /// `NSURLContentTypeKey` answers a `UTType`, not a string - the one real
    /// difference from every other resource key this module computes.
    #[test]
    fn content_type_key_answers_a_uttype_matching_the_extension() {
        crate::bootstrap();
        let dir = std::env::temp_dir().join(format!("rosemary-url-ct-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("picture.png");
        std::fs::write(&file, b"x").expect("a file to look at");

        let value = super::resource_value(&file.to_string_lossy(), "NSURLContentTypeKey")
            .expect("a content type for a file that exists");
        // SAFETY: `resource_value` hands over a `UTType` for this key.
        assert_eq!(
            unsafe { crate::uttype::identifier_of(value as objc::object::Id) },
            Some("public.png".to_string())
        );
        objc::arc::release(value as objc::object::Id);

        let missing = super::resource_value(
            &dir.join("nowhere.png").to_string_lossy(),
            "NSURLContentTypeKey",
        );
        assert!(missing.is_none(), "a resource that does not exist");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
