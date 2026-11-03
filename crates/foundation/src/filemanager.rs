use corefoundation::base::CFTypeRef;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// The one manager, which `+defaultManager` must return every time.
static DEFAULT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

unsafe extern "C" fn default_manager(class: Id, _sel: Sel) -> Id {
    *DEFAULT.get_or_init(|| {
        // SAFETY: `class` is `NSFileManager`, which this crate registered.
        let manager = unsafe {
            objc::object::create_instance(
                objc::class::from_ptr(class as *const objc::class::Class).expect("a class"),
                0,
            )
        };
        // Retained forever: a singleton a guest can release is one it can make
        // dangle by releasing what it never owned.
        objc::arc::make_immortal(manager);
        manager as usize
    }) as Id
}

/// A path argument as a `String`, or `None` for one that is not readable text.
///
/// # Safety
///
/// `path` must be null or an `NSString`.
unsafe fn path_of(path: Id) -> Option<std::path::PathBuf> {
    let guest = String::from_utf8(unsafe { crate::string::bytes(path) }).ok()?;
    // The guest named it, so it is opened at this host's spelling. Every caller
    // below asks the filesystem something; none hands the string back, so the
    // translation belongs here rather than at eleven call sites.
    Some(prefix::ns::existing_host_path(&guest))
}

/// Write nil through an `NSError **`, which is what "no error" looks like.
///
/// # Safety
///
/// `slot` must be null or writable.
unsafe fn no_error(slot: *mut Id) {
    if !slot.is_null() {
        unsafe { *slot = std::ptr::null_mut() };
    }
}

/// YES for `Ok`; otherwise NO, with an `NSCocoaErrorDomain` error naming
/// `path` and the POSIX cause written through `slot`, as Foundation reports a
/// failed write. A caller that presents the error has something to show.
///
/// # Safety
///
/// `slot` must be null or writable.
unsafe fn report(slot: *mut Id, path: &std::path::Path, result: std::io::Result<()>) -> u8 {
    let Err(cause) = result else {
        return 1;
    };
    if slot.is_null() {
        return 0;
    }
    // `NSFileNoSuchFileError`, `NSFileWriteNoPermissionError`,
    // `NSFileWriteFileExistsError`, `NSFileWriteUnknownError`.
    let code = match cause.kind() {
        std::io::ErrorKind::NotFound => 4,
        std::io::ErrorKind::PermissionDenied => 513,
        std::io::ErrorKind::AlreadyExists => 516,
        _ => 512,
    };
    // A host path is never shown to the guest; see `prefix::ns::to_guest`.
    let shown = prefix::ns::to_guest(path).map_or_else(
        || {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        },
        |p| p.to_string_lossy().into_owned(),
    );
    let description = format!("\u{201c}{shown}\u{201d} could not be written: {cause}");
    let path_value = crate::string::from_str(&shown);
    let underlying = cause.raw_os_error().map(|errno| {
        crate::error::make(
            "NSPOSIXErrorDomain",
            i64::from(darwin::errno::from_host(errno)),
            &cause.to_string(),
        )
    });
    let mut entries = vec![("NSFilePath", path_value)];
    entries.extend(underlying.map(|e| ("NSUnderlyingError", e)));
    let made = crate::error::create("NSCocoaErrorDomain", code, &description, &entries);
    objc::arc::release(path_value);
    // SAFETY: writable, the caller's obligation.
    unsafe { *slot = objc::arc::autorelease(made) };
    0
}

unsafe extern "C" fn file_exists(_receiver: Id, _sel: Sel, path: Id) -> u8 {
    // SAFETY: an `NSString`, or nil.
    unsafe { path_of(path) }.is_some_and(|p| std::fs::metadata(p).is_ok()) as u8
}

unsafe extern "C" fn file_exists_is_directory(
    _receiver: Id,
    _sel: Sel,
    path: Id,
    is_directory: *mut u8,
) -> u8 {
    // SAFETY: an `NSString`, or nil.
    let Some(path) = (unsafe { path_of(path) }) else {
        return 0;
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        return 0;
    };
    if !is_directory.is_null() {
        // SAFETY: the caller passed a writable `BOOL *`, as the method declares.
        unsafe { *is_directory = metadata.is_dir() as u8 };
    }
    1
}

unsafe extern "C" fn create_file(
    _receiver: Id,
    _sel: Sel,
    path: Id,
    contents: Id,
    _attributes: Id,
) -> u8 {
    // SAFETY: the arguments are an NSString and an NSData, or nil.
    let Some(path) = (unsafe { path_of(path) }) else {
        return 0;
    };
    let bytes = if contents.is_null() {
        Vec::new()
    } else {
        let Some(data) = (unsafe { corefoundation::collections::data(contents as CFTypeRef) })
        else {
            return 0;
        };
        data.to_vec()
    };
    std::fs::write(path, bytes).is_ok() as u8
}

unsafe extern "C" fn create_directory(
    _receiver: Id,
    _sel: Sel,
    path: Id,
    intermediates: u8,
    _attributes: Id,
    error: *mut Id,
) -> u8 {
    // SAFETY: null or writable.
    unsafe { no_error(error) };
    // SAFETY: an `NSString`, or nil.
    let Some(path) = (unsafe { path_of(path) }) else {
        return 0;
    };
    let made = match intermediates != 0 {
        true => std::fs::create_dir_all(&path),
        false => std::fs::create_dir(&path),
    };
    // Apple's succeeds when the directory is already there and intermediates
    // were asked for, which `create_dir_all` also does - so the two agree
    // without a check here.
    // SAFETY: null or writable.
    unsafe { report(error, &path, made) }
}

unsafe extern "C" fn remove_item(_receiver: Id, _sel: Sel, path: Id, error: *mut Id) -> u8 {
    // SAFETY: null or writable.
    unsafe { no_error(error) };
    // SAFETY: an `NSString`, or nil.
    let Some(path) = (unsafe { path_of(path) }) else {
        return 0;
    };
    let Ok(metadata) = std::fs::symlink_metadata(&path) else {
        return 0;
    };
    let removed = match metadata.is_dir() {
        true => std::fs::remove_dir_all(&path),
        false => std::fs::remove_file(&path),
    };
    // SAFETY: null or writable.
    unsafe { report(error, &path, removed) }
}

unsafe extern "C" fn copy_item(_receiver: Id, _sel: Sel, from: Id, to: Id, error: *mut Id) -> u8 {
    // SAFETY: null or writable.
    unsafe { no_error(error) };
    // SAFETY: two `NSString`s, or nil.
    let (Some(from), Some(to)) = (unsafe { path_of(from) }, unsafe { path_of(to) }) else {
        return 0;
    };
    // Files only. Copying a directory tree is a walk with its own failure modes,
    // and answering "yes" to one this did not copy would leave the guest
    // reading files that are not there.
    match std::fs::symlink_metadata(&from) {
        Ok(metadata) if metadata.is_dir() => objc::fail!(
            "-[NSFileManager copyItemAtPath:toPath:error:]: this runtime copies files, not \
             directory trees"
        ),
        // SAFETY: null or writable.
        Ok(_) => unsafe { report(error, &to, std::fs::copy(&from, &to).map(drop)) },
        Err(cause) => unsafe { report(error, &from, Err(cause)) },
    }
}

unsafe extern "C" fn move_item(_receiver: Id, _sel: Sel, from: Id, to: Id, error: *mut Id) -> u8 {
    // SAFETY: null or writable.
    unsafe { no_error(error) };
    // SAFETY: two `NSString`s, or nil.
    let (Some(from), Some(to)) = (unsafe { path_of(from) }, unsafe { path_of(to) }) else {
        return 0;
    };
    // SAFETY: null or writable.
    unsafe { report(error, &to, std::fs::rename(&from, &to)) }
}

/// Run `with` on the path a file URL names, spelled as the `NSString` the
/// path-taking methods above already take.
///
/// The URL forms are the same operations - Apple's documentation for each pair
/// says so - and a second implementation of "create this directory" would be a
/// second set of failure cases. `absent` is the answer for a receiver that is
/// not a file URL, which is what Apple's returns for one.
///
/// # Safety
///
/// `url` is null or an `NSURL`.
unsafe fn at_url<T>(url: Id, absent: T, with: impl FnOnce(Id) -> T) -> T {
    let Some(path) =
        (unsafe { corefoundation::url::as_url(url as CFTypeRef) }).and_then(|u| u.file_path())
    else {
        return absent;
    };
    let string = crate::string::from_str(&path) as Id;
    let answer = with(string);
    // SAFETY: a string this function made and nothing else holds.
    unsafe { corefoundation::api::CFRelease(string as CFTypeRef) };
    answer
}

unsafe extern "C" fn create_directory_at_url(
    receiver: Id,
    sel: Sel,
    url: Id,
    intermediates: u8,
    attributes: Id,
    error: *mut Id,
) -> u8 {
    // SAFETY: an `NSURL`; the inner call is the path form of this method.
    unsafe {
        at_url(url, 0, |path| {
            create_directory(receiver, sel, path, intermediates, attributes, error)
        })
    }
}

unsafe extern "C" fn remove_item_at_url(receiver: Id, sel: Sel, url: Id, error: *mut Id) -> u8 {
    unsafe { at_url(url, 0, |path| remove_item(receiver, sel, path, error)) }
}

unsafe extern "C" fn move_item_at_url(
    receiver: Id,
    sel: Sel,
    from: Id,
    to: Id,
    error: *mut Id,
) -> u8 {
    // SAFETY: as above, for both ends.
    unsafe {
        at_url(from, 0, |from| {
            at_url(to, 0, |to| move_item(receiver, sel, from, to, error))
        })
    }
}

unsafe extern "C" fn copy_item_at_url(
    receiver: Id,
    sel: Sel,
    from: Id,
    to: Id,
    error: *mut Id,
) -> u8 {
    unsafe {
        at_url(from, 0, |from| {
            at_url(to, 0, |to| copy_item(receiver, sel, from, to, error))
        })
    }
}

/// The `NSURLRelationship` between a directory and an item, using Apple's
/// declaration order: contains, same, other.
fn url_relationship(directory: &str, item: &str) -> isize {
    const CONTAINS: isize = 0;
    const SAME: isize = 1;
    const OTHER: isize = 2;

    let directory = crate::url::standardized_file_path(directory);
    let item = crate::url::standardized_file_path(item);
    if directory == item {
        return SAME;
    }
    let prefix = match directory.as_str() {
        "/" => "/".to_string(),
        _ => format!("{}/", directory.trim_end_matches('/')),
    };
    if item.starts_with(&prefix) {
        CONTAINS
    } else {
        OTHER
    }
}

/// `-getRelationship:ofDirectoryAtURL:toItemAtURL:error:`.
///
/// # Safety
///
/// The URLs are null or `NSURL`s, and the two out-parameters are null or
/// writable values of the types declared by the method.
unsafe extern "C" fn get_relationship(
    _receiver: Id,
    _sel: Sel,
    relationship: *mut isize,
    directory: Id,
    item: Id,
    error: *mut Id,
) -> u8 {
    unsafe { no_error(error) };
    if relationship.is_null() {
        return 0;
    }
    // SAFETY: the arguments are `NSURL`s.
    let Some(directory) = (unsafe { corefoundation::url::as_url(directory as CFTypeRef) })
        .and_then(|u| u.file_path())
    else {
        return 0;
    };
    let Some(item) =
        (unsafe { corefoundation::url::as_url(item as CFTypeRef) }).and_then(|u| u.file_path())
    else {
        return 0;
    };
    // SAFETY: checked non-null and declared writable by the caller.
    unsafe { *relationship = url_relationship(&directory, &item) };
    1
}

/// A file URL for a directory, autoreleased.
fn directory_url(path: &str) -> Id {
    // SAFETY: a URL this call just made.
    let url = corefoundation::url::new_file_url(path, true);
    // SAFETY: a URL this call just made.
    unsafe { corefoundation::api::CFAutorelease(url) as Id }
}

/// `-URLsForDirectory:inDomains:`, the `NSURL` form of
/// `NSSearchPathForDirectoriesInDomains`.
///
/// The paths come from [`crate::paths`] rather than from a second table here:
/// two lists of where `NSApplicationSupportDirectory` is would be two answers
/// for a program that asks both ways, which programs do.
unsafe extern "C" fn urls_for_directory(_receiver: Id, _sel: Sel, directory: u64, mask: u64) -> Id {
    let objects: Vec<CFTypeRef> = crate::paths::directory_paths(directory, mask)
        .iter()
        .map(|path| corefoundation::url::new_file_url(path, true))
        .collect();
    let array = corefoundation::collections::new_array(
        &objects,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for object in objects {
        // SAFETY: a URL this function made and the array has retained.
        unsafe { corefoundation::api::CFRelease(object) };
    }
    // SAFETY: a fresh array.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

/// `-URLForDirectory:inDomain:appropriateForURL:create:error:`
///
/// The first URL of the domain, made on disk when `create` asks for it - which
/// is the difference between this and the plural form above, and the reason a
/// program calls it: it wants a directory it can write to *now*.
///
/// `appropriateForURL:` is ignored. It only means anything for
/// `NSItemReplacementDirectory`, which asks for a scratch directory on the same
/// volume as the URL - and this tree does not place that constant at all, so
/// the answer for it is nil rather than a directory on the wrong volume.
///
/// # Safety
///
/// `error` is null or a writable `NSError *`.
unsafe extern "C" fn url_for_directory(
    _receiver: Id,
    _sel: Sel,
    directory: u64,
    domain: u64,
    _appropriate_for: Id,
    create: u8,
    error: *mut Id,
) -> Id {
    unsafe { no_error(error) };
    let Some(path) = crate::paths::directory_paths(directory, domain)
        .into_iter()
        .next()
    else {
        return std::ptr::null_mut();
    };
    // The path is the guest's; it is created at the host's spelling.
    if create != 0 && std::fs::create_dir_all(prefix::ns::existing_host_path(&path)).is_err() {
        return std::ptr::null_mut();
    }
    directory_url(&path)
}

/// The names in the host directory `path`, sorted, as the guest sees them.
fn directory_names(path: &std::path::Path) -> Option<Vec<String>> {
    let entries = std::fs::read_dir(path).ok()?;
    // Names, not paths, and without `.` and `..` - which is what Apple's
    // shallow enumeration gives and what a caller joining them back onto the
    // directory expects.
    // The prefix's mount points are not in the host directory, so the guest's
    // view of it is patched in: hidden ones left out, the rest shown.
    let listing = prefix::ns::listing_of(path);
    let mut names: Vec<String> = entries
        .filter_map(|e| {
            let name = e.ok()?.file_name();
            let hidden = listing
                .as_ref()
                .is_some_and(|l| l.hides(name.as_encoded_bytes()));
            (!hidden).then(|| name.to_string_lossy().into_owned())
        })
        .collect();
    if let Some(listing) = &listing {
        names.extend(
            listing
                .shown()
                .iter()
                .map(|point| point.name.to_string_lossy().into_owned()),
        );
    }
    names.sort();
    Some(names)
}

/// `-contentsOfDirectoryAtURL:includingPropertiesForKeys:options:error:`
///
/// The property keys are a prefetching hint, and `-getResourceValue:...`
/// answers them as cheaply afterwards. Of the options only skipping hidden
/// files changes what is listed; the others are about descending, which this
/// shallow listing never does.
///
/// # Safety
///
/// `url` is null or an `NSURL`, and `error` null or writable.
unsafe extern "C" fn contents_of_directory_at_url(
    _receiver: Id,
    _sel: Sel,
    url: Id,
    _keys: Id,
    options: u64,
    error: *mut Id,
) -> Id {
    const SKIPS_HIDDEN_FILES: u64 = 1 << 2;
    // SAFETY: null or writable.
    unsafe { no_error(error) };
    // SAFETY: null or an `NSURL`.
    let Some(directory) =
        (unsafe { corefoundation::url::as_url(url as CFTypeRef) }).and_then(|u| u.file_path())
    else {
        return std::ptr::null_mut();
    };
    let host = prefix::ns::existing_host_path(&directory);
    let Some(names) = directory_names(&host) else {
        return std::ptr::null_mut();
    };
    let base = directory.trim_end_matches('/');
    let objects: Vec<CFTypeRef> = names
        .iter()
        .filter(|name| options & SKIPS_HIDDEN_FILES == 0 || !name.starts_with('.'))
        .map(|name| {
            // Through the namespace again: a mount point listed here is not
            // in the host directory.
            let child = format!("{base}/{name}");
            let is_directory = prefix::ns::existing_host_path(&child).is_dir();
            corefoundation::url::new_file_url(&child, is_directory)
        })
        .collect();
    let array = corefoundation::collections::new_array(
        &objects,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for object in objects {
        // SAFETY: a URL this function made and the array has retained.
        unsafe { corefoundation::api::CFRelease(object) };
    }
    // SAFETY: a fresh array.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn contents_of_directory(
    _receiver: Id,
    _sel: Sel,
    path: Id,
    error: *mut Id,
) -> Id {
    // SAFETY: null or writable.
    unsafe { no_error(error) };
    // SAFETY: an `NSString`, or nil.
    let Some(names) = (unsafe { path_of(path) }).and_then(|path| directory_names(&path)) else {
        return std::ptr::null_mut();
    };
    let objects: Vec<corefoundation::base::CFTypeRef> = names
        .iter()
        .map(|n| corefoundation::string::from_bytes(n.as_bytes(), false))
        .collect();
    let array = corefoundation::collections::new_array(
        &objects,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for object in objects {
        // SAFETY: a string this function made and the array has retained.
        unsafe { corefoundation::api::CFRelease(object) };
    }
    // SAFETY: a fresh array.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

/// `-attributesOfItemAtPath:error:`, with the keys a program actually reads.
///
/// Size, type and the two timestamps. A dictionary with only some of an item's
/// attributes is the shape this tree refused `-[NSBundle infoDictionary]` over,
/// and the reason it is allowed here is that these are *this runtime's* answers
/// to a fixed question rather than a file's own contents: a key that is absent
/// is one nothing in this tree can answer, and adding one later cannot change
/// what the present ones mean.
unsafe extern "C" fn attributes_of_item(_receiver: Id, _sel: Sel, path: Id, error: *mut Id) -> Id {
    // SAFETY: null or writable.
    unsafe { no_error(error) };
    // SAFETY: an `NSString`, or nil.
    let Some(path) = (unsafe { path_of(path) }) else {
        return std::ptr::null_mut();
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        return std::ptr::null_mut();
    };
    let kind = match metadata.is_dir() {
        true => "NSFileTypeDirectory",
        false => "NSFileTypeRegular",
    };
    // The bool is whether this function owns the value: a date is autoreleased,
    // so releasing it here would be releasing what this function never held.
    let mut values: Vec<(&str, CFTypeRef, bool)> = vec![
        (
            "NSFileSize",
            corefoundation::collections::new_number(corefoundation::collections::Number::Integer(
                metadata.len() as i64,
            )),
            true,
        ),
        (
            "NSFileType",
            corefoundation::string::from_bytes(kind.as_bytes(), false),
            true,
        ),
    ];
    let timestamps = [
        ("NSFileModificationDate", metadata.modified()),
        ("NSFileCreationDate", metadata.created()),
    ];
    for (key, time) in timestamps {
        if let Some(seconds) = time.ok().map(since_1970) {
            values.push((
                key,
                crate::timer::date_since_1970(seconds) as CFTypeRef,
                false,
            ));
        }
    }

    let pairs: Vec<(CFTypeRef, CFTypeRef)> = values
        .iter()
        .map(|(key, value, _)| {
            (
                corefoundation::string::from_bytes(key.as_bytes(), false),
                *value,
            )
        })
        .collect();
    let dictionary = corefoundation::collections::new_dictionary(
        &pairs,
        corefoundation::callbacks::Policy::from_key_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for (key, _) in &pairs {
        // SAFETY: a string this function made and the dictionary has retained.
        unsafe { corefoundation::api::CFRelease(*key) };
    }
    for (_, value, owned) in &values {
        if *owned {
            // SAFETY: an object this function made and the dictionary has retained.
            unsafe { corefoundation::api::CFRelease(*value) };
        }
    }
    // SAFETY: a fresh dictionary.
    unsafe { corefoundation::api::CFAutorelease(dictionary) as Id }
}

unsafe extern "C" fn attributes_of_file_system(
    _receiver: Id,
    _sel: Sel,
    path: Id,
    error: *mut Id,
) -> Id {
    // SAFETY: null or writable.
    unsafe { no_error(error) };
    // SAFETY: an NSString, or nil.
    let Some(path) = (unsafe { path_of(path) }) else {
        return std::ptr::null_mut();
    };
    let Ok(path) = std::ffi::CString::new(path.to_string_lossy().as_bytes()) else {
        return std::ptr::null_mut();
    };
    // SAFETY: statvfs initializes the output on success.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return std::ptr::null_mut();
    }
    // macOS uses 32-bit counters; widen before computing byte sizes.
    let frsize = stat.f_frsize as u64;
    let entries = [
        (
            "NSFileSystemSize",
            (stat.f_blocks as u64).saturating_mul(frsize),
        ),
        (
            "NSFileSystemFreeSize",
            (stat.f_bavail as u64).saturating_mul(frsize),
        ),
        ("NSFileSystemNodes", stat.f_files as u64),
        ("NSFileSystemFreeNodes", stat.f_favail as u64),
        // The device number, which is what Apple puts here: a caller compares
        // two of them to tell whether paths share a filesystem.
        (
            "NSFileSystemNumber",
            std::fs::metadata(path.to_str().unwrap_or_default())
                .map_or(0, |m| std::os::unix::fs::MetadataExt::dev(&m)),
        ),
    ];
    let pairs: Vec<(CFTypeRef, CFTypeRef)> = entries
        .iter()
        .map(|(key, value)| {
            (
                corefoundation::string::from_bytes(key.as_bytes(), false),
                corefoundation::collections::new_number(
                    corefoundation::collections::Number::Integer(
                        (*value).min(i64::MAX as u64) as i64
                    ),
                ),
            )
        })
        .collect();
    let dictionary = corefoundation::collections::new_dictionary(
        &pairs,
        corefoundation::callbacks::Policy::from_key_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for (key, value) in pairs {
        // SAFETY: the dictionary retained both objects.
        unsafe {
            corefoundation::api::CFRelease(key);
            corefoundation::api::CFRelease(value);
        }
    }
    // SAFETY: a fresh dictionary.
    unsafe { corefoundation::api::CFAutorelease(dictionary) as Id }
}

/// Seconds since the Unix epoch, negative for a file dated before it.
fn since_1970(time: std::time::SystemTime) -> f64 {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(forward) => forward.as_secs_f64(),
        Err(back) => -back.duration().as_secs_f64(),
    }
}

/// One key of an attributes dictionary, or nil.
///
/// # Safety
///
/// `receiver` must be an `NSDictionary`.
unsafe fn attribute(receiver: Id, key: &str) -> Id {
    let key = corefoundation::string::from_bytes(key.as_bytes(), false);
    // SAFETY: a dictionary and a string key.
    let value = unsafe { corefoundation::api::CFDictionaryGetValue(receiver as CFTypeRef, key) };
    // SAFETY: a string this function made and holds the only reference to.
    unsafe { corefoundation::api::CFRelease(key) };
    value as Id
}

unsafe extern "C" fn file_size(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a dictionary.
    let value = unsafe { attribute(receiver, "NSFileSize") };
    // SAFETY: a number, or nil.
    unsafe { corefoundation::collections::number(value as CFTypeRef) }
        .map_or(0, |n| n.as_i64() as u64)
}

unsafe extern "C" fn file_type(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a dictionary.
    unsafe { attribute(receiver, "NSFileType") }
}

unsafe extern "C" fn file_modification_date(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a dictionary.
    unsafe { attribute(receiver, "NSFileModificationDate") }
}

unsafe extern "C" fn file_creation_date(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a dictionary.
    unsafe { attribute(receiver, "NSFileCreationDate") }
}

/// `-stringWithFileSystemRepresentation:length:`, the path a `readdir` or a
/// `getcwd` gave read back as an `NSString`.
///
/// The bytes are a path, so they are taken as UTF-8 and kept lossily rather than
/// refused: a name this host will hand back verbatim is one the guest can still
/// open, and dropping the whole path over one byte would lose the directory too.
unsafe extern "C" fn string_with_file_system_representation(
    _receiver: Id,
    _sel: Sel,
    bytes: *const u8,
    length: usize,
) -> Id {
    if bytes.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller's `length` bytes, which is the contract of the method.
    let bytes = unsafe { std::slice::from_raw_parts(bytes, length) };
    // SAFETY: a fresh string.
    unsafe {
        corefoundation::api::CFAutorelease(corefoundation::string::from_bytes(bytes, false)) as Id
    }
}

unsafe extern "C" fn is_writable(_receiver: Id, _sel: Sel, path: Id) -> u8 {
    // SAFETY: an `NSString`, or nil.
    let Some(path) = (unsafe { path_of(path) }) else {
        return 0;
    };
    std::fs::metadata(&path).is_ok_and(|m| !m.permissions().readonly()) as u8
}

/// `-isExecutableFileAtPath:`, which is the question "may I spawn this".
///
/// The execute bit for *this* process, not merely "some execute bit is set":
/// the bundle's own helpers are what a program asks about, and one that shipped
/// non-executable is exactly the case this has to report honestly - the crash
/// handler stub in this bundle did, and the spawn failed with `EACCES`.
unsafe extern "C" fn is_executable(_receiver: Id, _sel: Sel, path: Id) -> u8 {
    // SAFETY: an `NSString`, or nil.
    let Some(path) = (unsafe { path_of(path) }) else {
        return 0;
    };
    let Ok(path) = std::ffi::CString::new(path.into_os_string().into_encoded_bytes()) else {
        return 0;
    };
    // `access(X_OK)` rather than a mode-bit test, because the answer depends on
    // which of owner, group and other this process is - and on a mount that was
    // made `noexec`, where every mode bit says yes and an `execve` says no.
    // SAFETY: a NUL-terminated path that outlives the call.
    u8::from(unsafe { libc::access(path.as_ptr(), libc::X_OK) } == 0)
}

/// `-currentDirectoryPath`, in the guest's spelling so that it round-trips
/// through `-changeCurrentDirectoryPath:`.
unsafe extern "C" fn current_directory(_receiver: Id, _sel: Sel) -> Id {
    let mut buffer = [0 as std::ffi::c_char; 1024];
    // SAFETY: `buffer` is writable for its length.
    if unsafe { darwin::fs::getcwd(buffer.as_mut_ptr(), buffer.len()) }.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `getcwd` wrote a NUL-terminated path.
    let path = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) };
    crate::string::from_str(&path.to_string_lossy())
}

/// `-changeCurrentDirectoryPath:`, which is `chdir` with the guest's path.
unsafe extern "C" fn change_current_directory(_receiver: Id, _sel: Sel, path: Id) -> u8 {
    // SAFETY: an `NSString`, or nil.
    let Ok(path) = std::ffi::CString::new(unsafe { crate::string::bytes(path) }) else {
        return 0;
    };
    // SAFETY: a NUL-terminated string.
    (unsafe { darwin::fs::chdir(path.as_ptr()) } == 0) as u8
}

/// `-URLForUbiquityContainerIdentifier:` and `-ubiquityIdentityToken`: nil,
/// which is what a Mac signed out of iCloud answers, and what tells a program
/// to keep its saves locally.
unsafe extern "C" fn no_ubiquity(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

unsafe extern "C" fn no_ubiquity_container(_receiver: Id, _sel: Sel, _identifier: Id) -> Id {
    std::ptr::null_mut()
}

/// `-contentsAtPath:`: the file's bytes as `NSData`, or nil when it cannot be
/// read - which is also what Apple answers for a directory.
unsafe extern "C" fn contents_at_path(_receiver: Id, _sel: Sel, path: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let Some(path) = (unsafe { path_of(path) }) else {
        return std::ptr::null_mut();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return std::ptr::null_mut();
    };
    let data = corefoundation::collections::new_data(bytes);
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(data) as Id }
}

/// `-contentsEqualAtPath:andPath:`: two files with the same bytes, or two
/// directories whose entries are pairwise equal. Anything unreadable is unequal.
unsafe extern "C" fn contents_equal(_receiver: Id, _sel: Sel, first: Id, second: Id) -> u8 {
    // SAFETY: each is an `NSString`, or nil.
    let (Some(a), Some(b)) = (unsafe { path_of(first) }, unsafe { path_of(second) }) else {
        return 0;
    };
    paths_equal(&a, &b) as u8
}

fn paths_equal(a: &std::path::Path, b: &std::path::Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    match (ma.is_dir(), mb.is_dir()) {
        (false, false) => {
            ma.len() == mb.len()
                && matches!((std::fs::read(a), std::fs::read(b)), (Ok(x), Ok(y)) if x == y)
        }
        (true, true) => {
            let (Some(na), Some(nb)) = (directory_names(a), directory_names(b)) else {
                return false;
            };
            na == nb && na.iter().all(|n| paths_equal(&a.join(n), &b.join(n)))
        }
        _ => false,
    }
}

/// `-isDeletableFileAtPath:`: the item exists and its parent can be written.
unsafe extern "C" fn is_deletable(_receiver: Id, _sel: Sel, path: Id) -> u8 {
    // SAFETY: an `NSString`, or nil.
    let Some(path) = (unsafe { path_of(path) }) else {
        return 0;
    };
    if std::fs::symlink_metadata(&path).is_err() {
        return 0;
    }
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => std::path::PathBuf::from("."),
    };
    let Ok(parent) = std::ffi::CString::new(parent.into_os_string().into_encoded_bytes()) else {
        return 0;
    };
    // SAFETY: a NUL-terminated path that outlives the call.
    u8::from(unsafe { libc::access(parent.as_ptr(), libc::W_OK) } == 0)
}

/// Every descendant of `root`, depth first, as paths relative to it.
fn subpaths(root: &std::path::Path) -> Option<Vec<String>> {
    fn walk(dir: &std::path::Path, prefix: &str, out: &mut Vec<String>) {
        for name in directory_names(dir).unwrap_or_default() {
            let relative = match prefix.is_empty() {
                true => name.clone(),
                false => format!("{prefix}/{name}"),
            };
            out.push(relative.clone());
            let child = dir.join(&name);
            // A symlink to a directory is listed but not entered, which is how
            // Apple avoids cycles.
            let is_real_dir = std::fs::symlink_metadata(&child).is_ok_and(|m| m.is_dir());
            if is_real_dir {
                walk(&child, &relative, out);
            }
        }
    }
    directory_names(root)?;
    let mut out = Vec::new();
    walk(root, "", &mut out);
    Some(out)
}

fn string_array(strings: &[String]) -> Id {
    let objects: Vec<CFTypeRef> = strings
        .iter()
        .map(|s| corefoundation::string::from_bytes(s.as_bytes(), false))
        .collect();
    let array = corefoundation::collections::new_array(
        &objects,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for object in objects {
        // SAFETY: a string made above, which the array has retained.
        unsafe { corefoundation::api::CFRelease(object) };
    }
    // SAFETY: a fresh array.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

/// `-subpathsOfDirectoryAtPath:error:`.
unsafe extern "C" fn subpaths_of_directory(
    _receiver: Id,
    _sel: Sel,
    path: Id,
    error: *mut Id,
) -> Id {
    // SAFETY: null or writable.
    unsafe { no_error(error) };
    // SAFETY: an `NSString`, or nil.
    match (unsafe { path_of(path) }).and_then(|p| subpaths(&p)) {
        Some(list) => string_array(&list),
        None => {
            if !error.is_null() {
                // SAFETY: writable, checked above. 260 is NSFileReadNoSuchFileError.
                unsafe {
                    *error =
                        crate::error::make("NSCocoaErrorDomain", 260, "The folder doesn't exist.")
                };
            }
            std::ptr::null_mut()
        }
    }
}

/// `-subpathsAtPath:`, the older spelling, which has no error slot.
unsafe extern "C" fn subpaths_at_path(_receiver: Id, _sel: Sel, path: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    match (unsafe { path_of(path) }).and_then(|p| subpaths(&p)) {
        Some(list) => string_array(&list),
        None => std::ptr::null_mut(),
    }
}

/// `-displayNameAtPath:`: the last component, since there is no Finder
/// localization to apply.
unsafe extern "C" fn display_name(_receiver: Id, _sel: Sel, path: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let text = String::from_utf8_lossy(&unsafe { crate::string::bytes(path) }).into_owned();
    let trimmed = text.trim_end_matches('/');
    let name = match trimmed.rsplit('/').next() {
        Some(n) if !n.is_empty() => n,
        _ => "/",
    };
    crate::string::from_str(name)
}

/// `-destinationOfSymbolicLinkAtPath:error:`.
unsafe extern "C" fn destination_of_symlink(
    _receiver: Id,
    _sel: Sel,
    path: Id,
    error: *mut Id,
) -> Id {
    // SAFETY: null or writable.
    unsafe { no_error(error) };
    // SAFETY: an `NSString`, or nil.
    let Some(path) = (unsafe { path_of(path) }) else {
        return std::ptr::null_mut();
    };
    match std::fs::read_link(path) {
        Ok(target) => crate::string::from_str(&target.to_string_lossy()),
        Err(_) => std::ptr::null_mut(),
    }
}

/// `-createSymbolicLinkAtPath:withDestinationPath:error:`. The destination is
/// stored as the guest wrote it; it is only translated when something follows it.
unsafe extern "C" fn create_symlink(
    _receiver: Id,
    _sel: Sel,
    path: Id,
    destination: Id,
    error: *mut Id,
) -> u8 {
    // SAFETY: null or writable.
    unsafe { no_error(error) };
    // SAFETY: each is an `NSString`, or nil.
    let Some(path) = (unsafe { path_of(path) }) else {
        return 0;
    };
    let Ok(target) = String::from_utf8(unsafe { crate::string::bytes(destination) }) else {
        return 0;
    };
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, path).is_ok() as u8
    }
    #[cfg(not(unix))]
    {
        let _ = (path, target);
        0
    }
}

/// `-linkItemAtPath:toPath:error:`: a hard link.
unsafe extern "C" fn link_item(_receiver: Id, _sel: Sel, from: Id, to: Id, error: *mut Id) -> u8 {
    // SAFETY: null or writable.
    unsafe { no_error(error) };
    // SAFETY: each is an `NSString`, or nil.
    let (Some(from), Some(to)) = (unsafe { path_of(from) }, unsafe { path_of(to) }) else {
        return 0;
    };
    std::fs::hard_link(from, to).is_ok() as u8
}

pub(crate) fn install() {
    let class =
        corefoundation::base::register(c"NSFileManager", None, std::mem::size_of::<usize>(), false);
    class_methods!(class, {
        "defaultManager" => default_manager, c"@16@0:8";
    });
    methods!(class, {
        "fileExistsAtPath:" => file_exists, c"B24@0:8@16";
        "fileExistsAtPath:isDirectory:" => file_exists_is_directory, c"B32@0:8@16^c24";
        "createFileAtPath:contents:attributes:" => create_file, c"B40@0:8@16@24@32";
        "createDirectoryAtPath:withIntermediateDirectories:attributes:error:"
            => create_directory, c"B44@0:8@16c24@32^@40";
        "removeItemAtPath:error:" => remove_item, c"B32@0:8@16^@24";
        "copyItemAtPath:toPath:error:" => copy_item, c"B40@0:8@16@24^@32";
        "moveItemAtPath:toPath:error:" => move_item, c"B40@0:8@16@24^@32";
        "contentsOfDirectoryAtPath:error:" => contents_of_directory, c"@32@0:8@16^@24";
        "contentsOfDirectoryAtURL:includingPropertiesForKeys:options:error:"
            => contents_of_directory_at_url, c"@48@0:8@16@24Q32^@40";
        "contentsAtPath:" => contents_at_path, c"@24@0:8@16";
        "contentsEqualAtPath:andPath:" => contents_equal, c"B32@0:8@16@24";
        "isDeletableFileAtPath:" => is_deletable, c"B24@0:8@16";
        "subpathsOfDirectoryAtPath:error:" => subpaths_of_directory, c"@32@0:8@16^@24";
        "subpathsAtPath:" => subpaths_at_path, c"@24@0:8@16";
        "displayNameAtPath:" => display_name, c"@24@0:8@16";
        "destinationOfSymbolicLinkAtPath:error:"
            => destination_of_symlink, c"@32@0:8@16^@24";
        "createSymbolicLinkAtPath:withDestinationPath:error:"
            => create_symlink, c"B40@0:8@16@24^@32";
        "linkItemAtPath:toPath:error:" => link_item, c"B40@0:8@16@24^@32";
        "attributesOfItemAtPath:error:" => attributes_of_item, c"@32@0:8@16^@24";
        "attributesOfFileSystemForPath:error:" => attributes_of_file_system, c"@32@0:8@16^@24";
        "isWritableFileAtPath:" => is_writable, c"B24@0:8@16";
        "isReadableFileAtPath:" => file_exists, c"B24@0:8@16";
        "isExecutableFileAtPath:" => is_executable, c"B24@0:8@16";
        "currentDirectoryPath" => current_directory, c"@16@0:8";
        "changeCurrentDirectoryPath:" => change_current_directory, c"B24@0:8@16";
        "stringWithFileSystemRepresentation:length:"
            => string_with_file_system_representation, c"@32@0:8*16Q24";
        "createDirectoryAtURL:withIntermediateDirectories:attributes:error:"
            => create_directory_at_url, c"B44@0:8@16c24@32^@40";
        "removeItemAtURL:error:" => remove_item_at_url, c"B32@0:8@16^@24";
        "moveItemAtURL:toURL:error:" => move_item_at_url, c"B40@0:8@16@24^@32";
        "copyItemAtURL:toURL:error:" => copy_item_at_url, c"B40@0:8@16@24^@32";
        "URLsForDirectory:inDomains:" => urls_for_directory, c"@32@0:8Q16Q24";
        "URLForDirectory:inDomain:appropriateForURL:create:error:"
            => url_for_directory, c"@52@0:8Q16Q24@32c40^@44";
        "getRelationship:ofDirectoryAtURL:toItemAtURL:error:"
            => get_relationship, c"B48@0:8^q16@24@32^@40";
        "URLForUbiquityContainerIdentifier:" => no_ubiquity_container, c"@24@0:8@16";
        "ubiquityIdentityToken" => no_ubiquity, c"@16@0:8";
    });

    // Foundation's `NSFileAttributes` category: how a program reads the result
    // of `-attributesOfItemAtPath:error:` when it does not subscript it by key.
    let dictionary =
        objc::class::lookup(c"NSDictionary").expect("CoreFoundation registered NSDictionary");
    methods!(dictionary, {
        "fileSize" => file_size, c"Q16@0:8";
        "fileType" => file_type, c"@16@0:8";
        "fileModificationDate" => file_modification_date, c"@16@0:8";
        "fileCreationDate" => file_creation_date, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_system_attributes_report_available_space() {
        let manager = manager();
        let root = std::env::temp_dir().join(format!("rosemary-fs-{}", std::process::id()));
        let path = crate::string::from_str(&root.to_string_lossy());
        // SAFETY: the manager and path are Foundation objects; no error slot is needed.
        assert_eq!(
            unsafe {
                create_directory(
                    manager,
                    objc::sel::get(
                        "createDirectoryAtPath:withIntermediateDirectories:attributes:error:",
                    ),
                    path,
                    1,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            1
        );
        let attributes = unsafe {
            attributes_of_file_system(
                manager,
                objc::sel::get("attributesOfFileSystemForPath:error:"),
                path,
                std::ptr::null_mut(),
            )
        };
        assert!(!attributes.is_null());
        let host_path = std::ffi::CString::new(root.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: a valid path and writable statvfs output.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::statvfs(host_path.as_ptr(), &mut stat) }, 0);
        let expected_size =
            (u128::from(stat.f_blocks) * u128::from(stat.f_frsize)).min(i64::MAX as u128) as i64;
        // SAFETY: the returned dictionary holds an NSNumber at this key.
        let size = unsafe { attribute(attributes, "NSFileSystemSize") };
        let size = unsafe { corefoundation::collections::number(size as CFTypeRef) }
            .expect("filesystem size number")
            .as_i64();
        assert_eq!(size, expected_size);
        // SAFETY: the returned dictionary holds an NSNumber at this key.
        let free = unsafe { attribute(attributes, "NSFileSystemFreeSize") };
        let free = unsafe { corefoundation::collections::number(free as CFTypeRef) }
            .expect("free space number")
            .as_i64();
        assert!(free > 0);
        assert_eq!(
            unsafe {
                remove_item(
                    manager,
                    objc::sel::get("removeItemAtPath:error:"),
                    path,
                    std::ptr::null_mut(),
                )
            },
            1
        );
        objc::arc::release(path);
    }

    fn manager() -> Id {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSFileManager").expect("NSFileManager");
        // SAFETY: `+defaultManager` is `id (Class, SEL)`.
        unsafe {
            default_manager(
                objc::class::ptr(class) as Id,
                objc::sel::get("defaultManager"),
            )
        }
    }

    #[test]
    fn url_relationship_observes_component_boundaries() {
        assert_eq!(url_relationship("/a/b", "/a/b"), 1);
        assert_eq!(url_relationship("/a/b", "/a/b/c/../d"), 0);
        assert_eq!(url_relationship("/a/b", "/a/bc"), 2);
        assert_eq!(url_relationship("/", "/a"), 0);
    }

    /// The three the runner uses, on a directory this test makes and removes.
    #[test]
    fn a_directory_is_created_seen_and_removed() {
        let manager = manager();
        let root = std::env::temp_dir().join(format!("rosemary-fm-{}", std::process::id()));
        let nested = root.join("a/b");
        let path = crate::string::from_str(&nested.to_string_lossy());

        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            assert_eq!(
                create_directory(
                    manager,
                    objc::sel::get("createDirectoryAtPath:"),
                    path,
                    1,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ),
                1,
                "intermediates were asked for"
            );
            assert_eq!(
                file_exists(manager, objc::sel::get("fileExistsAtPath:"), path),
                1
            );

            let mut is_directory = 0u8;
            assert_eq!(
                file_exists_is_directory(
                    manager,
                    objc::sel::get("fileExistsAtPath:isDirectory:"),
                    path,
                    &mut is_directory,
                ),
                1
            );
            assert_eq!(is_directory, 1);

            let root_path = crate::string::from_str(&root.to_string_lossy());
            assert_eq!(
                remove_item(
                    manager,
                    objc::sel::get("removeItemAtPath:error:"),
                    root_path,
                    std::ptr::null_mut(),
                ),
                1
            );
            assert_eq!(
                file_exists(manager, objc::sel::get("fileExistsAtPath:"), path),
                0
            );
        }
    }

    /// `-contentsAtPath:` and the selectors an app pairs it with.
    #[test]
    fn contents_at_path_reads_and_relatives_agree() {
        let manager = manager();
        let root = std::env::temp_dir().join(format!("rosemary-fc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("d/sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"hello").unwrap();
        std::fs::write(root.join("b.txt"), b"hello").unwrap();
        std::fs::write(root.join("c.txt"), b"world").unwrap();
        std::fs::write(root.join("d/sub/f"), b"x").unwrap();
        let s = |name: &str| crate::string::from_str(&root.join(name).to_string_lossy());
        let sel = objc::sel::get;
        let null = std::ptr::null_mut();

        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            let data = contents_at_path(manager, sel("contentsAtPath:"), s("a.txt"));
            let bytes = corefoundation::collections::data(data as CFTypeRef).unwrap();
            assert_eq!(bytes.as_slice(), b"hello");
            assert!(contents_at_path(manager, sel("contentsAtPath:"), s("nope")).is_null());
            assert!(contents_at_path(manager, sel("contentsAtPath:"), s("d")).is_null());
            assert!(contents_at_path(manager, sel("contentsAtPath:"), null).is_null());

            let eq = |a, b| contents_equal(manager, sel("contentsEqualAtPath:andPath:"), a, b);
            assert_eq!(eq(s("a.txt"), s("b.txt")), 1);
            assert_eq!(eq(s("a.txt"), s("c.txt")), 0);
            assert_eq!(eq(s("a.txt"), s("d")), 0);
            assert_eq!(eq(s("a.txt"), s("missing")), 0);

            assert_eq!(
                is_deletable(manager, sel("isDeletableFileAtPath:"), s("a.txt")),
                1
            );
            assert_eq!(
                is_deletable(manager, sel("isDeletableFileAtPath:"), s("missing")),
                0
            );

            let list = subpaths_at_path(manager, sel("subpathsAtPath:"), s("d"));
            let count = corefoundation::api::CFArrayGetCount(list as CFTypeRef);
            assert_eq!(count, 2, "sub and sub/f");
            let list = subpaths_of_directory(manager, sel("x"), s("missing"), null as *mut Id);
            assert!(list.is_null());

            assert_eq!(
                create_symlink(
                    manager,
                    sel("x"),
                    s("ln"),
                    crate::string::from_str("a.txt"),
                    null as *mut Id,
                ),
                1
            );
            let target = destination_of_symlink(manager, sel("x"), s("ln"), null as *mut Id);
            assert_eq!(crate::string::bytes(target), b"a.txt");
            assert_eq!(
                link_item(manager, sel("x"), s("a.txt"), s("hard"), null as *mut Id),
                1
            );
            assert_eq!(std::fs::read(root.join("hard")).unwrap(), b"hello");

            let name = display_name(manager, sel("displayNameAtPath:"), s("a.txt"));
            assert_eq!(crate::string::bytes(name), b"a.txt");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// One manager, which is what `+defaultManager` promises.
    #[test]
    fn the_default_manager_is_one_object() {
        assert_eq!(manager(), manager());
    }
}
