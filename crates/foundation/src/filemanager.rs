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
        objc::arc::retain(manager);
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
    Some(prefix::ns::host_path(&guest))
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
    made.is_ok() as u8
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
    removed.is_ok() as u8
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
        Ok(_) => std::fs::copy(&from, &to).is_ok() as u8,
        Err(_) => 0,
    }
}

unsafe extern "C" fn move_item(_receiver: Id, _sel: Sel, from: Id, to: Id, error: *mut Id) -> u8 {
    // SAFETY: null or writable.
    unsafe { no_error(error) };
    // SAFETY: two `NSString`s, or nil.
    let (Some(from), Some(to)) = (unsafe { path_of(from) }, unsafe { path_of(to) }) else {
        return 0;
    };
    std::fs::rename(&from, &to).is_ok() as u8
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
    if create != 0 && std::fs::create_dir_all(&path).is_err() {
        return std::ptr::null_mut();
    }
    directory_url(&path)
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
    let Some(path) = (unsafe { path_of(path) }) else {
        return std::ptr::null_mut();
    };
    let Ok(entries) = std::fs::read_dir(&path) else {
        return std::ptr::null_mut();
    };
    // Names, not paths, and without `.` and `..` - which is what Apple's
    // shallow enumeration gives and what a caller joining them back onto the
    // directory expects.
    let mut names: Vec<String> = entries
        .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
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

unsafe extern "C" fn current_directory(_receiver: Id, _sel: Sel) -> Id {
    let path = std::env::current_dir().unwrap_or_default();
    crate::string::from_str(&path.to_string_lossy())
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

pub(crate) fn install() {
    let class =
        corefoundation::base::register(c"NSFileManager", None, std::mem::size_of::<usize>(), false);
    class_methods!(class, {
        "defaultManager" => default_manager, c"@16@0:8";
    });
    methods!(class, {
        "fileExistsAtPath:" => file_exists, c"B24@0:8@16";
        "fileExistsAtPath:isDirectory:" => file_exists_is_directory, c"B32@0:8@16^c24";
        "createDirectoryAtPath:withIntermediateDirectories:attributes:error:"
            => create_directory, c"B44@0:8@16c24@32^@40";
        "removeItemAtPath:error:" => remove_item, c"B32@0:8@16^@24";
        "copyItemAtPath:toPath:error:" => copy_item, c"B40@0:8@16@24^@32";
        "moveItemAtPath:toPath:error:" => move_item, c"B40@0:8@16@24^@32";
        "contentsOfDirectoryAtPath:error:" => contents_of_directory, c"@32@0:8@16^@24";
        "attributesOfItemAtPath:error:" => attributes_of_item, c"@32@0:8@16^@24";
        "isWritableFileAtPath:" => is_writable, c"B24@0:8@16";
        "isReadableFileAtPath:" => file_exists, c"B24@0:8@16";
        "isExecutableFileAtPath:" => is_executable, c"B24@0:8@16";
        "currentDirectoryPath" => current_directory, c"@16@0:8";
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
        let root = std::env::temp_dir().join(format!("r3-fm-{}", std::process::id()));
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

    /// One manager, which is what `+defaultManager` promises.
    #[test]
    fn the_default_manager_is_one_object() {
        assert_eq!(manager(), manager());
    }
}
