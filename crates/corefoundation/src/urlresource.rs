// The C spellings are the interface.
#![allow(non_snake_case)]

use std::ffi::c_void;

use objc::class::Class;
use objc::object::Id;

use crate::base::{CFIndex, CFTypeRef};

/// `Boolean`, which is a byte.
type Boolean = u8;

/// The resource-property keys, as `(Mach-O name, value)`.
///
/// Both spellings of each, because a program may import either and they are one
/// constant on a Mac. Every one is a `CFStringRef` **variable**, so each goes
/// through [`crate::base::global_cell`].
pub const KEYS: &[(&str, &str)] = &[
    ("_kCFURLNameKey", NAME),
    ("_NSURLNameKey", NAME),
    ("_kCFURLLocalizedNameKey", LOCALIZED_NAME),
    ("_NSURLLocalizedNameKey", LOCALIZED_NAME),
    ("_kCFURLIsHiddenKey", IS_HIDDEN),
    ("_NSURLIsHiddenKey", IS_HIDDEN),
    ("_kCFURLIsPackageKey", IS_PACKAGE),
    ("_NSURLIsPackageKey", IS_PACKAGE),
    ("_kCFURLIsAliasFileKey", IS_ALIAS_FILE),
    ("_NSURLIsAliasFileKey", IS_ALIAS_FILE),
    ("_kCFURLIsDirectoryKey", IS_DIRECTORY),
    ("_NSURLIsDirectoryKey", IS_DIRECTORY),
    ("_kCFURLIsRegularFileKey", IS_REGULAR_FILE),
    ("_NSURLIsRegularFileKey", IS_REGULAR_FILE),
    ("_kCFURLIsSymbolicLinkKey", IS_SYMBOLIC_LINK),
    ("_NSURLIsSymbolicLinkKey", IS_SYMBOLIC_LINK),
    ("_kCFURLFileSizeKey", FILE_SIZE),
    ("_NSURLFileSizeKey", FILE_SIZE),
    ("_kCFURLVolumeURLKey", VOLUME_URL),
    ("_NSURLVolumeURLKey", VOLUME_URL),
    ("_kCFURLVolumeTotalCapacityKey", VOLUME_TOTAL_CAPACITY),
    ("_NSURLVolumeTotalCapacityKey", VOLUME_TOTAL_CAPACITY),
    (
        "_kCFURLVolumeAvailableCapacityKey",
        VOLUME_AVAILABLE_CAPACITY,
    ),
    (
        "_NSURLVolumeAvailableCapacityKey",
        VOLUME_AVAILABLE_CAPACITY,
    ),
    ("_kCFURLVolumeIdentifierKey", VOLUME_IDENTIFIER),
    ("_NSURLVolumeIdentifierKey", VOLUME_IDENTIFIER),
    ("_kCFURLPathKey", PATH),
    ("_NSURLPathKey", PATH),
    ("_kCFURLIsVolumeKey", IS_VOLUME),
    ("_NSURLIsVolumeKey", IS_VOLUME),
    ("_kCFURLVolumeNameKey", VOLUME_NAME),
    ("_NSURLVolumeNameKey", VOLUME_NAME),
    // Dates are `CFDate`s, which Foundation's `NSDate` stands in for, so
    // Foundation answers these two; see `foundation::url`.
    (
        "_kCFURLContentModificationDateKey",
        CONTENT_MODIFICATION_DATE,
    ),
    (
        "_NSURLContentModificationDateKey",
        CONTENT_MODIFICATION_DATE,
    ),
    ("_kCFURLAddedToDirectoryDateKey", ADDED_TO_DIRECTORY_DATE),
    ("_NSURLAddedToDirectoryDateKey", ADDED_TO_DIRECTORY_DATE),
    ("_kCFURLFileResourceTypeKey", FILE_RESOURCE_TYPE),
    ("_NSURLFileResourceTypeKey", FILE_RESOURCE_TYPE),
    ("_kCFURLHasHiddenExtensionKey", HAS_HIDDEN_EXTENSION),
    ("_NSURLHasHiddenExtensionKey", HAS_HIDDEN_EXTENSION),
    ("_kCFURLIsApplicationKey", IS_APPLICATION),
    ("_NSURLIsApplicationKey", IS_APPLICATION),
    ("_kCFURLParentDirectoryURLKey", PARENT_DIRECTORY_URL),
    ("_NSURLParentDirectoryURLKey", PARENT_DIRECTORY_URL),
    ("_kCFURLTagNamesKey", TAG_NAMES),
    ("_NSURLTagNamesKey", TAG_NAMES),
    ("_kCFURLVolumeIsInternalKey", VOLUME_IS_INTERNAL),
    ("_NSURLVolumeIsInternalKey", VOLUME_IS_INTERNAL),
    ("_kCFURLVolumeIsLocalKey", VOLUME_IS_LOCAL),
    ("_NSURLVolumeIsLocalKey", VOLUME_IS_LOCAL),
    // An icon is an `NSImage`, which AppKit answers if it is loaded.
    ("_kCFURLEffectiveIconKey", EFFECTIVE_ICON),
    ("_NSURLEffectiveIconKey", EFFECTIVE_ICON),
    // The values `NSURLFileResourceTypeKey` answers with.
    ("_kCFURLFileResourceTypeNamedPipe", TYPE_NAMED_PIPE),
    ("_NSURLFileResourceTypeNamedPipe", TYPE_NAMED_PIPE),
    (
        "_kCFURLFileResourceTypeCharacterSpecial",
        TYPE_CHARACTER_SPECIAL,
    ),
    (
        "_NSURLFileResourceTypeCharacterSpecial",
        TYPE_CHARACTER_SPECIAL,
    ),
    ("_kCFURLFileResourceTypeDirectory", TYPE_DIRECTORY),
    ("_NSURLFileResourceTypeDirectory", TYPE_DIRECTORY),
    ("_kCFURLFileResourceTypeBlockSpecial", TYPE_BLOCK_SPECIAL),
    ("_NSURLFileResourceTypeBlockSpecial", TYPE_BLOCK_SPECIAL),
    ("_kCFURLFileResourceTypeRegular", TYPE_REGULAR),
    ("_NSURLFileResourceTypeRegular", TYPE_REGULAR),
    ("_kCFURLFileResourceTypeSymbolicLink", TYPE_SYMBOLIC_LINK),
    ("_NSURLFileResourceTypeSymbolicLink", TYPE_SYMBOLIC_LINK),
    ("_kCFURLFileResourceTypeSocket", TYPE_SOCKET),
    ("_NSURLFileResourceTypeSocket", TYPE_SOCKET),
    ("_kCFURLFileResourceTypeUnknown", TYPE_UNKNOWN),
    ("_NSURLFileResourceTypeUnknown", TYPE_UNKNOWN),
];

const NAME: &str = "NSURLNameKey";
const LOCALIZED_NAME: &str = "NSURLLocalizedNameKey";
const IS_HIDDEN: &str = "NSURLIsHiddenKey";
const IS_PACKAGE: &str = "NSURLIsPackageKey";
const IS_ALIAS_FILE: &str = "NSURLIsAliasFileKey";
const IS_DIRECTORY: &str = "NSURLIsDirectoryKey";
const IS_REGULAR_FILE: &str = "NSURLIsRegularFileKey";
const IS_SYMBOLIC_LINK: &str = "NSURLIsSymbolicLinkKey";
const FILE_SIZE: &str = "NSURLFileSizeKey";
const VOLUME_URL: &str = "NSURLVolumeURLKey";
const VOLUME_TOTAL_CAPACITY: &str = "NSURLVolumeTotalCapacityKey";
const VOLUME_AVAILABLE_CAPACITY: &str = "NSURLVolumeAvailableCapacityKey";
const VOLUME_IDENTIFIER: &str = "NSURLVolumeIdentifierKey";
const PATH: &str = "_NSURLPathKey";
const IS_VOLUME: &str = "NSURLIsVolumeKey";
const VOLUME_NAME: &str = "NSURLVolumeNameKey";
pub const CONTENT_MODIFICATION_DATE: &str = "NSURLContentModificationDateKey";
pub const ADDED_TO_DIRECTORY_DATE: &str = "NSURLAddedToDirectoryDateKey";
const FILE_RESOURCE_TYPE: &str = "NSURLFileResourceTypeKey";
const HAS_HIDDEN_EXTENSION: &str = "NSURLHasHiddenExtensionKey";
const IS_APPLICATION: &str = "NSURLIsApplicationKey";
const PARENT_DIRECTORY_URL: &str = "NSURLParentDirectoryURLKey";
const TAG_NAMES: &str = "NSURLTagNamesKey";
const VOLUME_IS_INTERNAL: &str = "NSURLVolumeIsInternalKey";
const VOLUME_IS_LOCAL: &str = "NSURLVolumeIsLocalKey";
pub const EFFECTIVE_ICON: &str = "NSURLEffectiveIconKey";
const TYPE_NAMED_PIPE: &str = "NSFileTypeNamedPipe";
const TYPE_CHARACTER_SPECIAL: &str = "NSFileTypeCharacterSpecial";
const TYPE_DIRECTORY: &str = "NSFileTypeDirectory";
const TYPE_BLOCK_SPECIAL: &str = "NSFileTypeBlockSpecial";
const TYPE_REGULAR: &str = "NSFileTypeRegular";
const TYPE_SYMBOLIC_LINK: &str = "NSFileTypeSymbolicLink";
const TYPE_SOCKET: &str = "NSFileTypeSocket";
const TYPE_UNKNOWN: &str = "NSFileTypeUnknown";

/// What a Mac calls its boot volume, and so what the guest's `/` is called.
const ROOT_VOLUME_NAME: &str = "Macintosh HD";

/// The guest path of the volume `guest` is on, by a Mac's layout rather than
/// this host's mounts: `/Volumes/<name>` is a volume and everything else is on
/// `/`. A prefix's root is a directory here, not a mount, so `st_dev` would
/// answer about the host instead.
fn guest_volume(guest: &str) -> &str {
    let Some(rest) = guest.strip_prefix("/Volumes/") else {
        return "/";
    };
    let name = rest.split('/').next().unwrap_or("");
    match name.is_empty() {
        true => "/",
        false => &guest[.."/Volumes/".len() + name.len()],
    }
}

/// The extensions that make a *directory* a package - a bundle the user sees as
/// one file rather than as a folder.
///
/// A list rather than a type query, because the type query is Launch Services
/// asking whether a UTI conforms to `com.apple.package`, and there is none here.
/// These are the extensions that conform to it in practice, and the ones a file
/// dialog has to get right: an `.app` shown as a folder is a file dialog the user
/// cannot open an application from.
const PACKAGE_EXTENSIONS: &[&str] = &[
    "app",
    "bundle",
    "framework",
    "plugin",
    "kext",
    "pkg",
    "mpkg",
    "docset",
    "xcodeproj",
    "xcworkspace",
    "playground",
    "rtfd",
    "scptd",
    "download",
    "photoslibrary",
    "fcpbundle",
    "logicx",
    "band",
];

/// Whether `path` names a package: a directory whose extension says the user
/// should see it as one file.
fn is_package(path: &std::path::Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| PACKAGE_EXTENSIONS.iter().any(|p| e.eq_ignore_ascii_case(p)))
}

/// Whether `path` is hidden.
///
/// A leading dot, which is what "hidden" means on every filesystem this host has.
/// Apple also honours a per-file Finder flag in the volume's metadata; ext4 has
/// nowhere to keep one, so a file hidden *only* that way on a Mac is not hidden
/// here - and there is no way for it to be.
fn is_hidden(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.') && n != "." && n != "..")
}

/// The mount point `path` is on, found by walking up until the device changes.
///
/// The device number rather than a mount table, because that is the question:
/// the volume is the outermost directory still on the same filesystem, and
/// `st_dev` is what says so on either host.
fn volume_of(path: &std::path::Path) -> std::path::PathBuf {
    let Some(device) = device_of(path) else {
        return std::path::PathBuf::from("/");
    };
    let mut at = path.to_path_buf();
    while let Some(parent) = at.parent() {
        if device_of(parent) != Some(device) {
            break;
        }
        at = parent.to_path_buf();
    }
    at
}

fn device_of(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.dev())
}

/// The total and available bytes of the filesystem holding `path`.
fn capacity(path: &std::path::Path) -> Option<(u64, u64)> {
    let text = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: a NUL-terminated path and a `statvfs` for the call to fill in.
    let stat = unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        (libc::statvfs(text.as_ptr(), &mut stat) == 0).then_some(stat)?
    };
    let unit = match stat.f_frsize {
        0 => stat.f_bsize,
        frsize => frsize,
    } as u64;
    // `f_bavail` rather than `f_bfree`: the blocks *this user* may fill, which is
    // what a program checking whether a save will fit needs.
    Some((stat.f_blocks as u64 * unit, stat.f_bavail as u64 * unit))
}

/// The value of one property of `path`, or `None` for a key this host cannot
/// answer - which is not the same as a false answer. See the module note.
///
/// The returned object is owned by the caller.
pub fn property(guest: &str, key: &str) -> Option<CFTypeRef> {
    let host = prefix::ns::host_path(guest);
    let path = host.as_path();
    let number = |value: u64| {
        crate::collections::new_number(crate::collections::Number::Integer(value as i64))
    };
    // `symlink_metadata`, because every question here is about the name the URL
    // holds rather than about what a symlink points at.
    let metadata = || std::fs::symlink_metadata(path).ok();
    Some(match key {
        NAME | LOCALIZED_NAME => {
            // No localised display name: a `.localized` folder's strings are
            // Finder data this tree does not read, so the name is the name.
            let name = path.file_name()?.to_str()?;
            crate::string::from_bytes(name.as_bytes(), false)
        }
        IS_HIDDEN => crate::collections::boolean(is_hidden(path)),
        IS_PACKAGE => crate::collections::boolean(is_package(path)),
        // A Finder alias is a file whose *contents* are a bookmark, and nothing
        // on this host writes one - so this is false rather than absent: the
        // answer is known, and it is no.
        IS_ALIAS_FILE => crate::collections::boolean(false),
        IS_DIRECTORY => crate::collections::boolean(metadata()?.is_dir()),
        IS_REGULAR_FILE => crate::collections::boolean(metadata()?.is_file()),
        IS_SYMBOLIC_LINK => crate::collections::boolean(metadata()?.is_symlink()),
        FILE_SIZE => number(metadata()?.len()),
        VOLUME_URL => crate::url::new_file_url(&volume_of(path).to_string_lossy(), true),
        VOLUME_TOTAL_CAPACITY => number(capacity(path)?.0),
        VOLUME_AVAILABLE_CAPACITY => number(capacity(path)?.1),
        // The volume's device number, which is exactly what the key is for:
        // telling whether two URLs are on the same volume.
        VOLUME_IDENTIFIER => number(device_of(path)?),
        PATH => crate::string::from_bytes(guest.as_bytes(), false),
        IS_VOLUME => {
            let trimmed = match guest.trim_end_matches('/') {
                "" => "/",
                other => other,
            };
            crate::collections::boolean(guest_volume(trimmed) == trimmed)
        }
        VOLUME_NAME => {
            let name = match guest_volume(guest) {
                "/" => ROOT_VOLUME_NAME,
                volume => volume.rsplit('/').next().unwrap_or(ROOT_VOLUME_NAME),
            };
            crate::string::from_bytes(name.as_bytes(), false)
        }
        FILE_RESOURCE_TYPE => {
            use std::os::unix::fs::FileTypeExt;
            let kind = metadata()?.file_type();
            let name = if kind.is_dir() {
                TYPE_DIRECTORY
            } else if kind.is_file() {
                TYPE_REGULAR
            } else if kind.is_symlink() {
                TYPE_SYMBOLIC_LINK
            } else if kind.is_fifo() {
                TYPE_NAMED_PIPE
            } else if kind.is_char_device() {
                TYPE_CHARACTER_SPECIAL
            } else if kind.is_block_device() {
                TYPE_BLOCK_SPECIAL
            } else if kind.is_socket() {
                TYPE_SOCKET
            } else {
                TYPE_UNKNOWN
            };
            crate::base::interned_name(name) as CFTypeRef
        }
        // The Finder flag that hides an extension has nowhere to live here.
        HAS_HIDDEN_EXTENSION => crate::collections::boolean(false),
        IS_APPLICATION => crate::collections::boolean(
            is_package(path)
                && path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("app")),
        ),
        PARENT_DIRECTORY_URL => {
            let trimmed = guest.trim_end_matches('/');
            let parent = match trimmed.rfind('/') {
                None => return None,
                Some(0) => "/",
                Some(at) => &trimmed[..at],
            };
            if trimmed.is_empty() {
                return None;
            }
            crate::url::new_file_url(parent, true)
        }
        // Finder tags are an extended attribute nothing on this host writes.
        TAG_NAMES => crate::collections::new_array(
            &[],
            crate::callbacks::Policy::from_type_callbacks(),
            false,
        ),
        VOLUME_IS_INTERNAL | VOLUME_IS_LOCAL => crate::collections::boolean(true),
        _ => return None,
    })
}

/// The path a URL names, for a caller that has the URL.
///
/// # Safety
///
/// `url` must be null or an object this runtime allocated.
unsafe fn path_of(url: CFTypeRef) -> Option<String> {
    // The guest's spelling: [`property`] puts filesystem questions to the
    // host's, and answers the path and volume ones with this.
    unsafe { crate::url::as_url(url) }?.file_path()
}

/// The text of a `CFStringRef`.
///
/// # Safety
///
/// As [`path_of`].
unsafe fn text_of(value: CFTypeRef) -> Option<String> {
    let bytes = unsafe { crate::string::as_string(value) }?.utf8();
    String::from_utf8(bytes).ok()
}

/// `Boolean CFURLCopyResourcePropertyForKey(CFURLRef, CFStringRef key, void
/// *propertyValueTypeRefPtr, CFErrorRef *error)`
///
/// True with the value written through `out` when the property is known, false
/// with `*out` left nil when it is not. `error` is left alone: a property this
/// host cannot answer is not a failure to report, and a caller that passed a
/// pointer for one is entitled to find no error in it.
///
/// # Safety
///
/// `url` and `key` are null or objects this runtime allocated; `out` is null or
/// writable for one pointer, and `error` is null or writable for one pointer.
pub unsafe extern "C" fn CFURLCopyResourcePropertyForKey(
    url: CFTypeRef,
    key: CFTypeRef,
    out: *mut CFTypeRef,
    _error: *mut CFTypeRef,
) -> Boolean {
    if !out.is_null() {
        // Cleared first, so a caller that reads it after a false return finds
        // nil rather than whatever was on its stack.
        // SAFETY: the caller states it is writable.
        unsafe { *out = std::ptr::null() };
    }
    let (Some(path), Some(key)) = (unsafe { path_of(url) }, unsafe { text_of(key) }) else {
        return 0;
    };
    let Some(value) = property(&path, &key) else {
        return 0;
    };
    match out.is_null() {
        // A caller asking only whether the property exists: the value it would
        // have been given has to be released rather than leaked.
        // SAFETY: an object this function owns the only reference to.
        true => unsafe { crate::api::CFRelease(value) },
        // SAFETY: the caller states it is writable, and takes the reference.
        false => unsafe { *out = value },
    }
    1
}

/// `CFDictionaryRef CFURLCopyResourcePropertiesForKeys(CFURLRef, CFArrayRef
/// keys, CFErrorRef *error)`
///
/// A key with no answer is left *out* of the dictionary rather than entered as
/// nil, which is Apple's behaviour and the one a caller can act on: a dictionary
/// that holds the key says the filesystem answered.
///
/// # Safety
///
/// As [`CFURLCopyResourcePropertyForKey`], with `keys` a `CFArray` or null.
pub unsafe extern "C" fn CFURLCopyResourcePropertiesForKeys(
    url: CFTypeRef,
    keys: CFTypeRef,
    _error: *mut CFTypeRef,
) -> CFTypeRef {
    let Some(path) = (unsafe { path_of(url) }) else {
        return std::ptr::null();
    };
    // SAFETY: as above - a `CFArray` or null.
    let Some(keys) = (unsafe { crate::collections::array(keys) }) else {
        return std::ptr::null();
    };
    let mut pairs: Vec<(CFTypeRef, CFTypeRef)> = Vec::new();
    for key in keys.values.clone() {
        // SAFETY: a value from the caller's array.
        let Some(name) = (unsafe { text_of(key) }) else {
            continue;
        };
        if let Some(value) = property(&path, &name) {
            pairs.push((key, value));
        }
    }
    let dictionary = crate::collections::new_dictionary(
        &pairs,
        crate::callbacks::Policy::from_key_callbacks(),
        crate::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for (_, value) in pairs {
        // The dictionary retained each value; these are this call's references.
        // SAFETY: a live object `property` handed over.
        unsafe { crate::api::CFRelease(value) };
    }
    dictionary
}

/// `CFURLRef CFURLCreateFromFileSystemRepresentation(CFAllocatorRef, const UInt8
/// *buffer, CFIndex length, Boolean isDirectory)`
///
/// # Safety
///
/// `buffer` points to at least `length` readable bytes.
pub unsafe extern "C" fn CFURLCreateFromFileSystemRepresentation(
    _allocator: CFTypeRef,
    buffer: *const u8,
    length: CFIndex,
    is_directory: Boolean,
) -> CFTypeRef {
    if buffer.is_null() || length < 0 {
        return std::ptr::null();
    }
    // SAFETY: the caller states the bytes are readable.
    let bytes = unsafe { std::slice::from_raw_parts(buffer, length as usize) };
    // A path is bytes, not text: anything not UTF-8 is kept as it is rather than
    // being refused, because it is a name that exists on disk.
    let path = String::from_utf8_lossy(bytes);
    crate::url::new_file_url(&path, is_directory != 0)
}

/// `CFURLRef CFURLCreateFromFileSystemRepresentationRelativeToBase(CFAllocatorRef,
/// const UInt8 *, CFIndex, Boolean isDirectory, CFURLRef baseURL)`
///
/// # Safety
///
/// As [`CFURLCreateFromFileSystemRepresentation`]; `base` is null or a `CFURL`.
pub unsafe extern "C" fn CFURLCreateFromFileSystemRepresentationRelativeToBase(
    allocator: CFTypeRef,
    buffer: *const u8,
    length: CFIndex,
    is_directory: Boolean,
    base: CFTypeRef,
) -> CFTypeRef {
    if buffer.is_null() || length < 0 {
        return std::ptr::null();
    }
    // SAFETY: the caller states the bytes are readable.
    let bytes = unsafe { std::slice::from_raw_parts(buffer, length as usize) };
    let path = String::from_utf8_lossy(bytes);
    let base_path = (!std::path::Path::new(&*path).is_absolute())
        .then(|| unsafe { crate::url::as_url(base) }.and_then(|b| b.file_path()))
        .flatten();
    match base_path {
        Some(base_path) => {
            let joined = std::path::Path::new(&base_path).join(&*path);
            crate::url::new_file_url(&joined.to_string_lossy(), is_directory != 0)
        }
        // SAFETY: the caller's buffer, as above.
        None => unsafe {
            CFURLCreateFromFileSystemRepresentation(allocator, buffer, length, is_directory)
        },
    }
}

/// `Boolean CFURLCreateDataAndPropertiesFromResource(CFAllocatorRef, CFURLRef,
/// CFDataRef *resourceData, CFDictionaryRef *properties, CFArrayRef
/// desiredProperties, SInt32 *errorCode)`
///
/// The deprecated way to read a file. Only the data is answered; a caller
/// asking for properties gets NULL, which the documentation permits for a
/// property the scheme does not support.
///
/// # Safety
///
/// `url` is null or a `CFURL`; each out-pointer is null or writable.
pub unsafe extern "C" fn CFURLCreateDataAndPropertiesFromResource(
    _allocator: CFTypeRef,
    url: CFTypeRef,
    data: *mut CFTypeRef,
    properties: *mut CFTypeRef,
    _desired: CFTypeRef,
    error: *mut i32,
) -> Boolean {
    /// `kCFURLUnknownError` and `kCFURLResourceNotFoundError`.
    const UNKNOWN: i32 = -10;
    const NOT_FOUND: i32 = -12;
    let set_error = |code: i32| {
        if !error.is_null() {
            // SAFETY: writable, per the caller.
            unsafe { error.write(code) };
        }
    };
    if !properties.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { properties.write(std::ptr::null()) };
    }
    if !data.is_null() {
        // SAFETY: as above.
        unsafe { data.write(std::ptr::null()) };
    }
    let Some(path) = (unsafe { crate::url::as_url(url) }).and_then(|u| u.file_path()) else {
        set_error(UNKNOWN);
        return 0;
    };
    if data.is_null() {
        let exists = std::fs::metadata(prefix::ns::host_path(&path)).is_ok();
        set_error(if exists { 0 } else { NOT_FOUND });
        return Boolean::from(exists);
    }
    let Ok(bytes) = std::fs::read(prefix::ns::host_path(&path)) else {
        set_error(NOT_FOUND);
        return 0;
    };
    // SAFETY: a buffer described by its own length.
    let object = unsafe {
        crate::api::CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as CFIndex)
    };
    // SAFETY: writable, per the caller.
    unsafe { data.write(object) };
    set_error(0);
    1
}

/// `CFURLRef CFURLCreateWithBytes(CFAllocatorRef, const UInt8 *, CFIndex,
/// CFStringEncoding, CFURLRef baseURL)`
///
/// The encoding is ignored for the reason every other text-from-bytes call here
/// ignores it: a URL is ASCII with everything else percent-escaped, so the bytes
/// are already the URL whatever encoding they were declared as.
///
/// # Safety
///
/// As [`CFURLCreateFromFileSystemRepresentation`]; `base` is null or a `CFURL`.
pub unsafe extern "C" fn CFURLCreateWithBytes(
    _allocator: CFTypeRef,
    buffer: *const u8,
    length: CFIndex,
    _encoding: u32,
    base: CFTypeRef,
) -> CFTypeRef {
    if buffer.is_null() || length < 0 {
        return std::ptr::null();
    }
    // SAFETY: the caller states the bytes are readable.
    let bytes = unsafe { std::slice::from_raw_parts(buffer, length as usize) };
    let text = String::from_utf8_lossy(bytes).into_owned();
    // SAFETY: a `CFURL` or null.
    let base = unsafe { crate::url::as_url(base) };
    match base {
        // Relative to a base only when it is relative: a URL with its own scheme
        // is absolute and the base says nothing about it.
        Some(base) if !text.contains("://") && !text.starts_with('/') => {
            let mut joined = base.text.clone();
            if !joined.ends_with('/') {
                joined.push('/');
            }
            joined.push_str(&text);
            crate::url::new(&joined)
        }
        _ => crate::url::new(&text),
    }
}

#[repr(C)]
struct EnumeratorBody {
    isa: *const Class,
    contents: *mut Enumerator,
}

struct Enumerator {
    root: CFTypeRef,
    yielded: bool,
}

impl Drop for Enumerator {
    fn drop(&mut self) {
        // SAFETY: the enumerator owns the create-rule reference to its URL.
        unsafe { crate::api::CFRelease(self.root) };
    }
}

fn enumerator_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        let class = crate::base::register(
            c"__NSCFURLEnumerator",
            None,
            size_of::<EnumeratorBody>(),
            true,
        );
        unsafe extern "C" fn dealloc(receiver: Id, _sel: objc::sel::Sel) {
            // SAFETY: this class stores a boxed `Enumerator` in its second word.
            unsafe {
                let body = receiver as *mut EnumeratorBody;
                if !(*body).contents.is_null() {
                    drop(Box::from_raw((*body).contents));
                    (*body).contents = std::ptr::null_mut();
                }
                objc::arc::finalize(receiver);
            }
        }
        class.replace_method(
            objc::sel::get("dealloc"),
            dealloc as *const c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: the cell contains a registered, process-lifetime class.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("a class") }
}

/// Enumerate the host's mounted volumes. The root filesystem is the portable
/// volume every supported host has and is enough for Qt's storage accounting.
pub unsafe extern "C" fn CFURLEnumeratorCreateForMountedVolumes(
    _allocator: CFTypeRef,
    _options: u64,
    _property_keys: CFTypeRef,
) -> CFTypeRef {
    let root = crate::url::new_file_url("/", true);
    // SAFETY: the body begins with `isa` and matches the registered size.
    let body: *mut EnumeratorBody = unsafe { crate::base::allocate(enumerator_class()) };
    if body.is_null() {
        // SAFETY: `root` is this call's create-rule reference.
        unsafe { crate::api::CFRelease(root) };
        return std::ptr::null();
    }
    // SAFETY: `body` is a live instance with a writable contents word.
    unsafe {
        (*body).contents = Box::into_raw(Box::new(Enumerator {
            root,
            yielded: false,
        }))
    };
    body as CFTypeRef
}

/// Return the root volume once, then `kCFURLEnumeratorEnd`.
pub unsafe extern "C" fn CFURLEnumeratorGetNextURL(
    enumerator: CFTypeRef,
    url: *mut CFTypeRef,
    error: *mut CFTypeRef,
) -> CFIndex {
    if !url.is_null() {
        // SAFETY: the caller supplied the result word.
        unsafe { url.write(std::ptr::null()) };
    }
    if !error.is_null() {
        // SAFETY: the caller supplied the result word.
        unsafe { error.write(std::ptr::null()) };
    }
    // SAFETY: the caller's handle must be null or a runtime object.
    if !unsafe { crate::base::is_kind_of(enumerator, enumerator_class()) } {
        return 2;
    }
    let body = enumerator as *mut EnumeratorBody;
    // SAFETY: an instance of this class has the body above.
    let state = unsafe { (*body).contents.as_mut() };
    let Some(state) = state else {
        return 2;
    };
    if state.yielded {
        return 2;
    }
    state.yielded = true;
    if !url.is_null() {
        // Borrowed for as long as the enumerator remains alive.
        unsafe { url.write(state.root) };
    }
    1
}

exports::exports! {
    functions {
        "CFURLCopyResourcePropertyForKey" => CFURLCopyResourcePropertyForKey,
        "CFURLCopyResourcePropertiesForKeys" => CFURLCopyResourcePropertiesForKeys,
        "CFURLCreateFromFileSystemRepresentation" => CFURLCreateFromFileSystemRepresentation,
        "CFURLCreateFromFileSystemRepresentationRelativeToBase" => CFURLCreateFromFileSystemRepresentationRelativeToBase,
        "CFURLCreateDataAndPropertiesFromResource" => CFURLCreateDataAndPropertiesFromResource,
        "CFURLCreateWithBytes" => CFURLCreateWithBytes,
        "CFURLEnumeratorCreateForMountedVolumes" => CFURLEnumeratorCreateForMountedVolumes,
        "CFURLEnumeratorGetNextURL" => CFURLEnumeratorGetNextURL,
    }
}

/// The data symbols this module publishes, for [`crate::data_symbols`].
pub fn data_symbols() -> Vec<(String, u64)> {
    crate::bootstrap();
    KEYS.iter()
        .map(|(symbol, text)| {
            (
                (*symbol).to_string(),
                crate::base::global_cell(
                    symbol,
                    crate::base::interned_name(text) as CFTypeRef as usize as u64,
                ),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both spellings of a key have to be the same object, or a dictionary built
    /// with the CoreFoundation key cannot be read with the Foundation one.
    #[test]
    fn the_two_spellings_of_a_key_are_one_string() {
        crate::bootstrap();
        let symbols = data_symbols();
        let at = |name: &str| {
            let (_, cell) = symbols.iter().find(|(s, _)| s == name).expect(name);
            // SAFETY: a cell this crate leaked, holding one pointer.
            unsafe { *(*cell as *const u64) }
        };
        assert_eq!(at("_kCFURLIsHiddenKey"), at("_NSURLIsHiddenKey"));
        assert_ne!(at("_kCFURLIsHiddenKey"), at("_kCFURLIsPackageKey"));
    }

    #[test]
    fn a_dotfile_is_hidden_and_an_app_is_a_package() {
        let dir = std::env::temp_dir().join("rosemary-urlresource");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Thing.app")).expect("a package to look at");
        std::fs::write(dir.join(".secret"), b"x").expect("a dotfile to look at");
        std::fs::write(dir.join("plain.txt"), b"x").expect("a plain file to look at");

        assert!(is_hidden(&dir.join(".secret")));
        assert!(!is_hidden(&dir.join("plain.txt")));
        assert!(is_package(&dir.join("Thing.app")));
        // A *file* called `.app` is not a package, however it is spelled: the
        // rule is about directories the user sees as one file.
        assert!(!is_package(&dir.join("plain.txt")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The volume of any path under `/` is a directory that exists, and the
    /// capacity of it is not zero - the two answers `QStorageInfo` is made of.
    #[test]
    fn a_path_has_a_volume_with_a_capacity() {
        let volume = volume_of(&std::env::temp_dir());
        assert!(volume.is_dir(), "{volume:?}");
        let (total, available) = capacity(&volume).expect("a mounted filesystem has a size");
        assert!(total > 0, "a volume with no bytes at all");
        assert!(available <= total);
    }

    #[test]
    fn mounted_volume_enumerator_yields_root_then_ends() {
        // SAFETY: null means the default allocator and no property keys.
        let enumerator = unsafe {
            CFURLEnumeratorCreateForMountedVolumes(std::ptr::null(), 0, std::ptr::null())
        };
        let mut url = std::ptr::null();
        let mut error = std::ptr::null();
        // SAFETY: both output words are writable and the enumerator is live.
        assert_eq!(
            unsafe { CFURLEnumeratorGetNextURL(enumerator, &mut url, &mut error) },
            1
        );
        // SAFETY: the returned URL is borrowed from the live enumerator.
        assert_eq!(
            unsafe { crate::url::as_url(url) }.and_then(|u| u.file_path()),
            Some("/".to_string())
        );
        assert!(error.is_null());
        assert_eq!(
            unsafe { CFURLEnumeratorGetNextURL(enumerator, &mut url, &mut error) },
            2
        );
        assert!(url.is_null());
        // SAFETY: balances the create rule.
        unsafe { crate::api::CFRelease(enumerator) };
    }
}
