//! LaunchServices: which application handles a URL or type, and opening one.
//!
//! The only application registered here is the running one, so a question about
//! a handler is answered with it when its `Info.plist` claims the scheme, and
//! with "not found" otherwise. A URL whose handler that is goes to it, as a Mac
//! sends a running handler the URL; any other document or URL is handed to the
//! host, as `-[NSWorkspace openURL:]` is. Launching an application goes by
//! LaunchServices' rule that it runs once; see [`crate::launch`].

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

use corefoundation::base::CFTypeRef;

type OSStatus = i32;

const NO_ERR: OSStatus = 0;
const PARAM_ERR: OSStatus = -50;
/// `kLSApplicationNotFoundErr`.
const APPLICATION_NOT_FOUND: OSStatus = -10814;
/// `kLSUnknownErr`.
const UNKNOWN_ERR: OSStatus = -10810;

/// Handlers a program set with `LSSetDefault...`, by lowercased scheme or
/// content type, as bundle identifiers.
static DEFAULTS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn text(string: CFTypeRef) -> Option<String> {
    if string.is_null() {
        return None;
    }
    // SAFETY: a string, as every caller's parameter is declared.
    Some(String::from_utf8_lossy(&unsafe { foundation::string::bytes(string as _) }).into_owned())
}

fn string(text: &str) -> CFTypeRef {
    foundation::string::from_str(text) as CFTypeRef
}

/// The running application's bundle identifier.
fn main_identifier() -> Option<String> {
    foundation::bundle::main()?.identifier().map(str::to_string)
}

/// Whether the running application's `Info.plist` claims `scheme`.
fn main_claims(scheme: &str) -> bool {
    let Some(layout) = foundation::bundle::main() else {
        return false;
    };
    layout
        .info
        .get("CFBundleURLTypes")
        .and_then(|types| types.as_array())
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("CFBundleURLSchemes")?.as_array())
        .flatten()
        .filter_map(|s| s.as_str())
        .any(|claimed| claimed.eq_ignore_ascii_case(scheme))
}

/// The bundle identifier that handles `scheme`, if any.
fn handler_for(scheme: &str) -> Option<String> {
    let key = scheme.to_ascii_lowercase();
    if let Some(set) = DEFAULTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&key))
    {
        return Some(set.clone());
    }
    main_claims(scheme).then(main_identifier).flatten()
}

/// A file URL for the running application's bundle, which the caller owns.
fn main_url() -> Option<CFTypeRef> {
    let layout = foundation::bundle::main()?;
    let path = prefix::ns::to_guest(&layout.root)?;
    Some(corefoundation::url::new_file_url(
        &path.to_string_lossy(),
        true,
    ))
}

/// A file URL for the application whose bundle identifier is `identifier`,
/// which the caller owns. The running application is the only one registered.
pub(crate) fn application_url(identifier: &str) -> Option<CFTypeRef> {
    (main_identifier().as_deref() == Some(identifier))
        .then(main_url)
        .flatten()
}

/// Answer an application lookup that found the running application: through
/// the URL, since there is no `FSRef` to give.
fn found_main(app_ref: *mut c_void, app_url: *mut CFTypeRef) -> OSStatus {
    if app_url.is_null() {
        // Only an `FSRef` was asked for, and none can be made.
        return match app_ref.is_null() {
            true => NO_ERR,
            false => APPLICATION_NOT_FOUND,
        };
    }
    let Some(url) = main_url() else {
        return APPLICATION_NOT_FOUND;
    };
    // SAFETY: writable, per the caller; the URL follows the copy rule.
    unsafe { app_url.write(url) };
    NO_ERR
}

/// `CFStringRef LSCopyDefaultHandlerForURLScheme(CFStringRef)`
pub unsafe extern "C" fn LSCopyDefaultHandlerForURLScheme(scheme: CFTypeRef) -> CFTypeRef {
    text(scheme)
        .and_then(|s| handler_for(&s))
        .map_or(std::ptr::null(), |id| string(&id))
}

/// `CFArrayRef LSCopyAllHandlersForURLScheme(CFStringRef)`
pub unsafe extern "C" fn LSCopyAllHandlersForURLScheme(scheme: CFTypeRef) -> CFTypeRef {
    let Some(id) = text(scheme).and_then(|s| handler_for(&s)) else {
        return std::ptr::null();
    };
    let id = string(&id);
    let array = corefoundation::collections::new_array(
        &[id],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh string, now held by the array.
    unsafe { corefoundation::api::CFRelease(id) };
    array
}

/// `OSStatus LSSetDefaultHandlerForURLScheme(CFStringRef, CFStringRef)`
///
/// Kept for this process; there is no LaunchServices database to persist it in.
pub unsafe extern "C" fn LSSetDefaultHandlerForURLScheme(
    scheme: CFTypeRef,
    bundle: CFTypeRef,
) -> OSStatus {
    let (Some(scheme), Some(bundle)) = (text(scheme), text(bundle)) else {
        return PARAM_ERR;
    };
    DEFAULTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(scheme.to_ascii_lowercase(), bundle);
    NO_ERR
}

/// `OSStatus LSSetDefaultRoleHandlerForContentType(CFStringRef, LSRolesMask,
/// CFStringRef)`, kept as [`LSSetDefaultHandlerForURLScheme`] keeps a scheme.
pub unsafe extern "C" fn LSSetDefaultRoleHandlerForContentType(
    content_type: CFTypeRef,
    _roles: u32,
    bundle: CFTypeRef,
) -> OSStatus {
    // SAFETY: the same arguments, keyed the same way.
    unsafe { LSSetDefaultHandlerForURLScheme(content_type, bundle) }
}

/// `OSStatus LSGetApplicationForURL(CFURLRef, LSRolesMask, FSRef *, CFURLRef *)`
pub unsafe extern "C" fn LSGetApplicationForURL(
    url: CFTypeRef,
    _roles: u32,
    app_ref: *mut c_void,
    app_url: *mut CFTypeRef,
) -> OSStatus {
    // SAFETY: a `CFURL` or null.
    let Some(url) = (unsafe { corefoundation::url::as_url(url) }) else {
        return PARAM_ERR;
    };
    let scheme = url.scheme().to_string();
    match handler_for(&scheme).is_some_and(|id| Some(id) == main_identifier()) {
        true => found_main(app_ref, app_url),
        false => APPLICATION_NOT_FOUND,
    }
}

/// `OSStatus LSFindApplicationForInfo(OSType creator, CFStringRef bundleID,
/// CFStringRef name, FSRef *, CFURLRef *)`
pub unsafe extern "C" fn LSFindApplicationForInfo(
    _creator: u32,
    bundle: CFTypeRef,
    name: CFTypeRef,
    app_ref: *mut c_void,
    app_url: *mut CFTypeRef,
) -> OSStatus {
    let wanted_id = text(bundle);
    let by_id = wanted_id.is_some() && wanted_id == main_identifier();
    let by_name = text(name).is_some_and(|name| {
        foundation::bundle::main().is_some_and(|layout| {
            layout
                .root
                .file_name()
                .is_some_and(|leaf| leaf.to_string_lossy().eq_ignore_ascii_case(&name))
        })
    });
    match by_id || by_name {
        true => found_main(app_ref, app_url),
        false => APPLICATION_NOT_FOUND,
    }
}

/// `OSStatus LSGetApplicationForInfo(OSType, OSType, CFStringRef extension,
/// LSRolesMask, FSRef *, CFURLRef *)`: no application here claims a document
/// type for another to find.
pub unsafe extern "C" fn LSGetApplicationForInfo(
    _kind: u32,
    _creator: u32,
    _extension: CFTypeRef,
    _roles: u32,
    _app_ref: *mut c_void,
    _app_url: *mut CFTypeRef,
) -> OSStatus {
    APPLICATION_NOT_FOUND
}

/// `OSStatus LSCopyApplicationForMIMEType(CFStringRef, LSRolesMask, CFURLRef *)`
pub unsafe extern "C" fn LSCopyApplicationForMIMEType(
    _mime: CFTypeRef,
    _roles: u32,
    _app_url: *mut CFTypeRef,
) -> OSStatus {
    APPLICATION_NOT_FOUND
}

/// Open `url` in this application if it is the handler LaunchServices would
/// pick, which then receives it as a `GURL` event. True when it was sent.
pub(crate) fn handled_here(url: &corefoundation::url::Contents) -> bool {
    let scheme = url.scheme();
    if scheme.eq_ignore_ascii_case("file") {
        return false;
    }
    let ours = handler_for(&scheme).is_some_and(|id| Some(id) == main_identifier());
    ours && crate::launch::open_in_self(std::slice::from_ref(&url.text))
}

/// Hand a URL to its handler, as `-[NSWorkspace openURL:]` does.
fn open(url: CFTypeRef) -> bool {
    // SAFETY: a `CFURL` or null.
    let Some(contents) = (unsafe { corefoundation::url::as_url(url) }) else {
        return false;
    };
    if handled_here(contents) {
        return true;
    }
    crate::workspace::open_target(contents).is_some_and(|target| sdl3::open_url(&target))
}

/// `OSStatus LSOpenCFURLRef(CFURLRef, CFURLRef *launchedURL)`
pub unsafe extern "C" fn LSOpenCFURLRef(url: CFTypeRef, launched: *mut CFTypeRef) -> OSStatus {
    if !launched.is_null() {
        // SAFETY: writable, per the caller. The host's handler has no URL here.
        unsafe { launched.write(std::ptr::null()) };
    }
    match open(url) {
        true => NO_ERR,
        false => UNKNOWN_ERR,
    }
}

/// `LSLaunchURLSpec`.
#[repr(C)]
pub struct LaunchUrlSpec {
    app_url: CFTypeRef,
    item_urls: CFTypeRef,
    pass_thru_params: *const c_void,
    launch_flags: u32,
    async_ref_con: *mut c_void,
}

/// `kLSLaunchNewInstance`.
const LAUNCH_NEW_INSTANCE: u32 = 0x0008_0000;

/// Launch the application at `app_url` with `items`, by LaunchServices' rule
/// that an instance already running takes the launch.
fn launch_application(app_url: CFTypeRef, items: &[String], flags: u32) -> OSStatus {
    // SAFETY: a `CFURL` or null.
    let Some(path) = (unsafe { corefoundation::url::as_url(app_url) }).and_then(|u| u.file_path())
    else {
        return PARAM_ERR;
    };
    let host = prefix::ns::host_path(&path);
    if foundation::bundle::Layout::at(&host).is_none() {
        return APPLICATION_NOT_FOUND;
    }
    let new_instance = flags & LAUNCH_NEW_INSTANCE != 0;
    let mode = match new_instance {
        true => crate::launch::Launch::NewInstance,
        false => crate::launch::Launch::Open,
    };
    if let Some(running) = crate::launch::running_instance(&host, mode) {
        return match crate::launch::hand_over(running.pid, items) {
            true => NO_ERR,
            false => UNKNOWN_ERR,
        };
    }
    match crate::workspace::spawn_application(&path, items, new_instance) {
        Ok(_) => NO_ERR,
        Err(_) => UNKNOWN_ERR,
    }
}

/// `OSStatus LSOpenFromURLSpec(const LSLaunchURLSpec *, CFURLRef *)`
///
/// A spec naming an application launches it with the items, or hands them to
/// the instance already running. Without one, each item is opened by its
/// handler.
pub unsafe extern "C" fn LSOpenFromURLSpec(
    spec: *const LaunchUrlSpec,
    launched: *mut CFTypeRef,
) -> OSStatus {
    if spec.is_null() {
        return PARAM_ERR;
    }
    if !launched.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { launched.write(std::ptr::null()) };
    }
    // SAFETY: the caller's spec.
    let (app_url, items, flags) =
        unsafe { ((*spec).app_url, (*spec).item_urls, (*spec).launch_flags) };
    // SAFETY: null or an array of URLs.
    let list = unsafe { corefoundation::collections::array(items) };
    if !app_url.is_null() {
        let texts: Vec<String> = list
            .map(|list| list.values.clone())
            .unwrap_or_default()
            .into_iter()
            // SAFETY: the spec's items are URLs.
            .filter_map(|url| unsafe { corefoundation::url::as_url(url) }.map(|u| u.text.clone()))
            .collect();
        return launch_application(app_url, &texts, flags);
    }
    let Some(list) = list else {
        return APPLICATION_NOT_FOUND;
    };
    let urls: Vec<CFTypeRef> = list.values.clone();
    match urls.into_iter().all(open) {
        true => NO_ERR,
        false => UNKNOWN_ERR,
    }
}

/// `LSRolesMask`, and the `CFBundleTypeRole` each bit is.
const ROLES: &[(u32, &str)] = &[
    (0x1, "None"),
    (0x2, "Viewer"),
    (0x4, "Editor"),
    (0x8, "Shell"),
];

/// Whether the running application's `Info.plist` claims documents of
/// `content_type` in one of the roles in `roles`.
fn main_claims_type(content_type: &str, roles: u32) -> bool {
    let Some(layout) = foundation::bundle::main() else {
        return false;
    };
    let in_role = |entry: &foundation::plist::Value| {
        let role = entry
            .get("CFBundleTypeRole")
            .and_then(|r| r.as_str())
            .unwrap_or("Editor");
        ROLES
            .iter()
            .any(|(bit, name)| roles & bit != 0 && role.eq_ignore_ascii_case(name))
    };
    layout
        .info
        .get("CFBundleDocumentTypes")
        .and_then(|types| types.as_array())
        .into_iter()
        .flatten()
        .filter(|entry| in_role(entry))
        .filter_map(|entry| entry.get("LSItemContentTypes")?.as_array())
        .flatten()
        .filter_map(|t| t.as_str())
        .any(|claimed| claimed.eq_ignore_ascii_case(content_type))
}

/// A one-element array of `object`, which it takes the caller's reference to.
fn sole(object: CFTypeRef) -> CFTypeRef {
    let array = corefoundation::collections::new_array(
        &[object],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: the caller's reference, now held by the array.
    unsafe { corefoundation::api::CFRelease(object) };
    array
}

/// `CFArrayRef LSCopyAllRoleHandlersForContentType(CFStringRef, LSRolesMask)`:
/// the running application, the only one registered, if it claims the type.
unsafe extern "C" fn LSCopyAllRoleHandlersForContentType(
    content_type: CFTypeRef,
    roles: u32,
) -> CFTypeRef {
    let claimed = text(content_type).is_some_and(|t| main_claims_type(&t, roles));
    match claimed.then(main_identifier).flatten() {
        Some(identifier) => sole(string(&identifier)),
        None => std::ptr::null(),
    }
}

/// `CFArrayRef LSCopyApplicationURLsForURL(CFURLRef, LSRolesMask)`: the
/// running application, if it claims the URL's scheme or, for a file, its
/// type.
unsafe extern "C" fn LSCopyApplicationURLsForURL(url: CFTypeRef, roles: u32) -> CFTypeRef {
    // SAFETY: a URL or null.
    let Some(url) = (unsafe { corefoundation::url::as_url(url) }) else {
        return std::ptr::null();
    };
    let claimed = match url.file_path() {
        Some(path) => std::path::Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .and_then(|e| {
                corefoundation::uti::identifier_for(corefoundation::uti::FILENAME_EXTENSION, e)
            })
            .is_some_and(|t| main_claims_type(&t, roles)),
        None => main_claims(&url.scheme()),
    };
    match claimed.then(main_url).flatten() {
        Some(application) => sole(application),
        None => std::ptr::null(),
    }
}

/// `OSStatus LSRegisterURL(CFURLRef, Boolean update)`.
///
/// FIXME (ACCURACY): nothing is recorded: the only application registered is
/// the running one, so a bundle registered here is never answered for.
unsafe extern "C" fn LSRegisterURL(url: CFTypeRef, _update: u8) -> OSStatus {
    match url.is_null() {
        true => PARAM_ERR,
        false => NO_ERR,
    }
}

/// `kLSSharedFileListSessionLoginItems`'s value.
pub const SESSION_LOGIN_ITEMS: &str = "com.apple.LSSharedFileList.SessionLoginItems";

fn shared_file_list_class() -> &'static objc::class::Class {
    corefoundation::base::register(c"LSSharedFileList", None, size_of::<usize>(), true)
}

/// `LSSharedFileListRef LSSharedFileListCreate(CFAllocatorRef, CFStringRef
/// listType, CFTypeRef listOptions)`
///
/// A list of what this host has registered, which is nothing: no login item
/// or favorite is kept here, so every list is empty.
unsafe extern "C" fn LSSharedFileListCreate(
    _allocator: CFTypeRef,
    list_type: CFTypeRef,
    _options: CFTypeRef,
) -> CFTypeRef {
    if list_type.is_null() {
        return std::ptr::null();
    }
    objc::object::create_instance(shared_file_list_class(), 0) as CFTypeRef
}

/// The seed a snapshot reports, which changes when a list does; these never do.
const SNAPSHOT_SEED: u32 = 1;

/// `CFArrayRef LSSharedFileListCopySnapshot(LSSharedFileListRef, UInt32 *seed)`
unsafe extern "C" fn LSSharedFileListCopySnapshot(list: CFTypeRef, seed: *mut u32) -> CFTypeRef {
    if list.is_null() {
        return std::ptr::null();
    }
    if !seed.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { seed.write(SNAPSHOT_SEED) };
    }
    corefoundation::collections::new_array(
        &[],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    )
}

/// `OSStatus LSSharedFileListItemRemove(LSSharedFileListRef,
/// LSSharedFileListItemRef)`: no item is ever handed out, so none is one.
unsafe extern "C" fn LSSharedFileListItemRemove(_list: CFTypeRef, _item: CFTypeRef) -> OSStatus {
    PARAM_ERR
}

/// `CFStringRef LSSharedFileListItemCopyDisplayName(LSSharedFileListItemRef)`
unsafe extern "C" fn LSSharedFileListItemCopyDisplayName(_item: CFTypeRef) -> CFTypeRef {
    std::ptr::null()
}

/// `CFURLRef LSSharedFileListItemCopyResolvedURL(LSSharedFileListItemRef,
/// LSSharedFileListResolutionFlags, CFErrorRef *)`
unsafe extern "C" fn LSSharedFileListItemCopyResolvedURL(
    _item: CFTypeRef,
    _flags: u32,
    error: *mut CFTypeRef,
) -> CFTypeRef {
    if !error.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { error.write(std::ptr::null()) };
    }
    std::ptr::null()
}

exports::exports! {
    functions {
        "LSCopyAllRoleHandlersForContentType" => LSCopyAllRoleHandlersForContentType,
        "LSCopyApplicationURLsForURL" => LSCopyApplicationURLsForURL,
        "LSRegisterURL" => LSRegisterURL,
        "LSSharedFileListCreate" => LSSharedFileListCreate,
        "LSSharedFileListCopySnapshot" => LSSharedFileListCopySnapshot,
        "LSSharedFileListItemRemove" => LSSharedFileListItemRemove,
        "LSSharedFileListItemCopyDisplayName" => LSSharedFileListItemCopyDisplayName,
        "LSSharedFileListItemCopyResolvedURL" => LSSharedFileListItemCopyResolvedURL,
        "LSCopyDefaultHandlerForURLScheme" => LSCopyDefaultHandlerForURLScheme,
        "LSCopyAllHandlersForURLScheme" => LSCopyAllHandlersForURLScheme,
        "LSSetDefaultHandlerForURLScheme" => LSSetDefaultHandlerForURLScheme,
        "LSSetDefaultRoleHandlerForContentType" => LSSetDefaultRoleHandlerForContentType,
        "LSGetApplicationForURL" => LSGetApplicationForURL,
        "LSFindApplicationForInfo" => LSFindApplicationForInfo,
        "LSGetApplicationForInfo" => LSGetApplicationForInfo,
        "LSCopyApplicationForMIMEType" => LSCopyApplicationForMIMEType,
        "LSOpenCFURLRef" => LSOpenCFURLRef,
        "LSOpenFromURLSpec" => LSOpenFromURLSpec,
    }
}
