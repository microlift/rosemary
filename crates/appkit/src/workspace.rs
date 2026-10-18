use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

static SHARED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
static CURRENT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

pub const WILL_SLEEP: &str = "NSWorkspaceWillSleepNotification";
pub const DID_WAKE: &str = "NSWorkspaceDidWakeNotification";
pub const SCREENS_DID_SLEEP: &str = "NSWorkspaceScreensDidSleepNotification";
pub const SCREENS_DID_WAKE: &str = "NSWorkspaceScreensDidWakeNotification";

/// The `NSWorkspaceLaunchConfiguration...` keys, published as data in
/// `crate::STRING_GLOBALS`.
pub const LAUNCH_ARGUMENTS: &str = "NSWorkspaceLaunchConfigurationArguments";
pub const LAUNCH_ENVIRONMENT: &str = "NSWorkspaceLaunchConfigurationEnvironment";
pub const LAUNCH_APPLE_EVENT: &str = "NSWorkspaceLaunchConfigurationAppleEvent";
pub const LAUNCH_ARCHITECTURE: &str = "NSWorkspaceLaunchConfigurationArchitecture";

/// What an `NSRunningApplication` for a process this one launched knows about
/// it, by object address. Anything not listed here is this process.
struct Launched {
    pid: i32,
    identifier: Option<String>,
    name: Option<String>,
}

static LAUNCHED: std::sync::Mutex<Vec<(usize, Launched)>> = std::sync::Mutex::new(Vec::new());

fn launched<T>(receiver: Id, read: impl FnOnce(&Launched) -> T) -> Option<T> {
    LAUNCHED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(object, _)| *object == receiver as usize)
        .map(|(_, launched)| read(launched))
}

/// `+[NSWorkspace sharedWorkspace]`.
///
/// The workspace is a process-lifetime singleton. Its individual services are
/// installed as they acquire host equivalents; callers must still be able to
/// obtain the object before asking for one of them.
unsafe extern "C" fn shared_workspace(class: Id, _sel: Sel) -> Id {
    *SHARED.get_or_init(|| {
        // SAFETY: the receiver is the `NSWorkspace` class this crate declared.
        let workspace = unsafe {
            objc::object::create_instance(
                objc::class::from_ptr(class as *const objc::class::Class).expect("a class"),
                0,
            )
        };
        // A singleton is not transferred to the caller and must survive a
        // guest incorrectly balancing it with `release`.
        objc::arc::retain(workspace);
        workspace as usize
    }) as Id
}

/// `-URLForApplicationToOpenURL:`.
///
/// Host desktop applications are not Mach-O bundles loaded by this runtime, so
/// there is no truthful application URL to return. Cocoa callers use nil to
/// mean that Launch Services has no registered handler and are required to
/// handle that result.
unsafe extern "C" fn application_url_for_url(_receiver: Id, _sel: Sel, _url: Id) -> Id {
    std::ptr::null_mut()
}

/// `-[NSWorkspace openURL:]`: hand a URL to the host's registered handler.
unsafe extern "C" fn open_url(_receiver: Id, _sel: Sel, url: Id) -> u8 {
    let Some(url) = (unsafe { corefoundation::url::as_url(url as *const std::ffi::c_void) }) else {
        return 0;
    };
    let Some(target) = open_target(url) else {
        return 0;
    };
    u8::from(sdl3::open_url(&target))
}

unsafe extern "C" fn open_file(_receiver: Id, _sel: Sel, path: Id) -> u8 {
    if path.is_null() {
        return 0;
    }
    // SAFETY: an `NSString`.
    let bytes = unsafe { foundation::string::bytes(path) };
    let Ok(guest) = std::str::from_utf8(&bytes) else {
        return 0;
    };
    let host = prefix::ns::host_path(guest);
    let url = corefoundation::url::new_file_url(&host.to_string_lossy(), host.is_dir());
    let text = unsafe { corefoundation::url::as_url(url) }.map(|u| u.text.clone());
    unsafe { corefoundation::api::CFRelease(url) };
    text.is_some_and(|text| sdl3::open_url(&text)).into()
}

unsafe extern "C" fn open_file_with(receiver: Id, sel: Sel, path: Id, _application: Id) -> u8 {
    unsafe { open_file(receiver, sel, path) }
}

fn open_target(url: &corefoundation::url::Contents) -> Option<String> {
    if url.scheme().is_empty() {
        return None;
    }
    let Some(path) = url.file_path() else {
        return Some(url.text.clone());
    };
    let host = prefix::ns::host_path(&path);
    let file_url =
        corefoundation::url::new_file_url(&host.to_string_lossy(), url.text.ends_with('/'));
    let text = unsafe { corefoundation::url::as_url(file_url) }?
        .text
        .clone();
    unsafe { corefoundation::api::CFRelease(file_url) };
    Some(text)
}

/// `-[NSWorkspace notificationCenter]`.
///
/// Workspace notifications share Foundation's process-wide center in this
/// runtime. It provides the same observer semantics while host sleep and wake
/// events remain unavailable.
unsafe extern "C" fn notification_center(_receiver: Id, _sel: Sel) -> Id {
    foundation::notification::default_center_object()
}

/// `-[NSWorkspace frontmostApplication]`: this process.
///
/// The same answer as `-[NSApplication isActive]`, which never resigns because
/// no other application here can be in front. A program comparing this against
/// its own process to decide whether it has focus must not find a stranger.
unsafe extern "C" fn frontmost_application(_receiver: Id, _sel: Sel) -> Id {
    current()
}

/// `-[NSWorkspace runningApplications]`: this process alone, for the reason
/// given at [`running_applications`].
unsafe extern "C" fn workspace_running_applications(_receiver: Id, _sel: Sel) -> Id {
    let array = corefoundation::collections::new_array(
        &[current() as *const std::ffi::c_void],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh CFArray, toll-free bridged to NSArray.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

/// `-[NSWorkspace iconForFileType:]` and `-iconForFile:`: a blank 32x32 image.
///
/// Apple never answers nil - a type it knows nothing about gets the generic
/// document icon - and a program puts the result straight into a cell or menu
/// item without checking. There is no icon theme to take artwork from here, so
/// the image is the right size with nothing in it, which draws as nothing.
unsafe extern "C" fn icon_for_file_type(_receiver: Id, _sel: Sel, _type: Id) -> Id {
    let image = crate::image::empty(foundation::geometry::NSSize {
        width: 32.0,
        height: 32.0,
    });
    objc::arc::autorelease(image)
}

/// The `NSRunningApplication` for this process, a process-lifetime singleton.
fn current() -> Id {
    *CURRENT.get_or_init(|| {
        let class =
            objc::class::lookup(c"NSRunningApplication").expect("declared before its methods");
        let object = objc::object::create_instance(class, 0);
        objc::arc::retain(object);
        object as usize
    }) as Id
}

/// `+[NSRunningApplication currentApplication]`.
unsafe extern "C" fn current_application(_class: Id, _sel: Sel) -> Id {
    current()
}

fn autoreleased(text: &str) -> Id {
    let string = foundation::string::from_str(text);
    // SAFETY: a fresh CFString, toll-free bridged to NSString.
    unsafe { corefoundation::api::CFAutorelease(string as _) as Id }
}

/// `-processIdentifier`: this process, or one it launched.
unsafe extern "C" fn process_identifier(receiver: Id, _sel: Sel) -> i32 {
    launched(receiver, |app| app.pid).unwrap_or(std::process::id() as i32)
}

/// `-bundleIdentifier`: nil for an executable outside a bundle, as on macOS.
unsafe extern "C" fn bundle_identifier(receiver: Id, _sel: Sel) -> Id {
    if let Some(identifier) = launched(receiver, |app| app.identifier.clone()) {
        return identifier.map_or(std::ptr::null_mut(), |id| autoreleased(&id));
    }
    match foundation::bundle::main().and_then(|b| b.identifier()) {
        Some(identifier) => autoreleased(identifier),
        None => std::ptr::null_mut(),
    }
}

/// `-localizedName`: the bundle's display name, else the executable's name.
unsafe extern "C" fn localized_name(receiver: Id, _sel: Sel) -> Id {
    if let Some(name) = launched(receiver, |app| app.name.clone()) {
        return name.map_or(std::ptr::null_mut(), |name| autoreleased(&name));
    }
    if let Some(name) = foundation::bundle::main().and_then(|b| b.display_name()) {
        return autoreleased(name);
    }
    let name = std::env::args_os().next().and_then(|arg| {
        std::path::Path::new(&arg)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    });
    match name {
        Some(name) => autoreleased(&name),
        None => std::ptr::null_mut(),
    }
}

/// `-isActive`: always for this process, for the reason given at
/// [`frontmost_application`]; never for one it launched, which has its own
/// windows this process knows nothing of.
unsafe extern "C" fn running_is_active(receiver: Id, _sel: Sel) -> u8 {
    u8::from(launched(receiver, |_| ()).is_none())
}

/// `-activateWithOptions:`: already active, which Apple reports as success.
unsafe extern "C" fn activate_with_options(_receiver: Id, _sel: Sel, _options: usize) -> u8 {
    1
}

/// The strings of an `NSArray`, or nothing for nil.
///
/// # Safety
///
/// `list` is null or an array of strings.
unsafe fn strings_of(list: Id) -> Vec<String> {
    // SAFETY: the caller's array, or nil.
    unsafe { corefoundation::collections::array(list as *const std::ffi::c_void) }.map_or_else(
        Vec::new,
        |array| {
            array
                .values
                .iter()
                .map(|value| unsafe { text(*value as Id) })
                .collect()
        },
    )
}

/// # Safety
///
/// `string` is null or an `NSString`.
unsafe fn text(string: Id) -> String {
    match string.is_null() {
        true => String::new(),
        false => {
            String::from_utf8_lossy(&unsafe { foundation::string::bytes(string) }).into_owned()
        }
    }
}

fn launch_error(error: *mut Id, message: &str) -> Id {
    if !error.is_null() {
        // SAFETY: the caller's writable `NSError **`.
        unsafe { *error = foundation::error::make("NSCocoaErrorDomain", 4, message) };
    }
    std::ptr::null_mut()
}

/// `-launchApplicationAtURL:options:configuration:error:`: start the bundle's
/// executable as the guest's own `posix_spawn` would, so a Mach-O runs through
/// this runtime, and answer an `NSRunningApplication` for it.
///
/// The launch options are about activation and Dock behaviour, which have no
/// meaning here. Of the configuration only the arguments are applied; another
/// key is named, not silently dropped.
unsafe extern "C" fn launch_application(
    _receiver: Id,
    _sel: Sel,
    url: Id,
    _options: usize,
    configuration: Id,
    error: *mut Id,
) -> Id {
    // SAFETY: an `NSURL`, or nil.
    let Some(path) = (unsafe { corefoundation::url::as_url(url as *const std::ffi::c_void) })
        .and_then(|url| url.file_path())
    else {
        return launch_error(error, "the application URL is not a file URL");
    };
    let Some(bundle) = foundation::bundle::Layout::at(&prefix::ns::host_path(&path)) else {
        return launch_error(error, &format!("{path} is not an application bundle"));
    };
    let Some(executable) = prefix::ns::to_guest(&bundle.executable) else {
        return launch_error(error, &format!("{path} has no executable"));
    };

    let mut arguments = Vec::new();
    // SAFETY: an `NSDictionary`, or nil.
    if let Some(dictionary) =
        unsafe { corefoundation::collections::dictionary(configuration as *const std::ffi::c_void) }
    {
        for (key, value) in &dictionary.entries {
            // SAFETY: the keys are strings.
            match unsafe { text(*key as Id) }.as_str() {
                LAUNCH_ARGUMENTS => arguments = unsafe { strings_of(*value as Id) },
                other => eprintln!(
                    "rosemary: appkit: -launchApplicationAtURL: does not apply {other}; \
                     launching without it"
                ),
            }
        }
    }
    let cstring = |text: &str| std::ffi::CString::new(text).unwrap_or_default();
    let program = cstring(&executable.to_string_lossy());
    let argv: Vec<std::ffi::CString> = std::iter::once(program.clone())
        .chain(arguments.iter().map(|argument| cstring(argument)))
        .collect();
    let pid = match darwin::spawn::spawn_child(&program, &argv, None, None, [None; 3]) {
        Ok(pid) => pid,
        Err(errno) => {
            let why = std::io::Error::from_raw_os_error(errno);
            return launch_error(error, &format!("could not launch {path}: {why}"));
        }
    };
    // An application is not the launcher's to wait for, but its exit status
    // still has to be collected by somebody.
    std::thread::spawn(move || darwin::spawn::wait_child(pid, true));

    let class = objc::class::lookup(c"NSRunningApplication").expect("declared before its methods");
    let object = objc::object::create_instance(class, 0);
    LAUNCHED.lock().unwrap_or_else(|e| e.into_inner()).push((
        object as usize,
        Launched {
            pid,
            identifier: bundle.identifier().map(str::to_owned),
            name: bundle
                .display_name()
                .map(str::to_owned)
                .or_else(|| Some(bundle.root.file_stem()?.to_string_lossy().into_owned())),
        },
    ));
    // The table outlives the object's address only as long as the object is
    // kept, which is for the process: applications launched here are few.
    objc::arc::retain(object);
    objc::arc::autorelease(object)
}

/// `+[NSRunningApplication runningApplicationsWithBundleIdentifier:]`.
///
/// Host processes are not registered applications in this Darwin workspace.
unsafe extern "C" fn running_applications(_class: Id, _sel: Sel, _identifier: Id) -> Id {
    let array = corefoundation::collections::new_array(
        &[],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh CFArray, toll-free bridged to NSArray.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn is_file_package(_receiver: Id, _sel: Sel, path: Id) -> u8 {
    const PACKAGES: &[&str] = &[
        "app",
        "bundle",
        "framework",
        "plugin",
        "kext",
        "pkg",
        "rtfd",
    ];
    if path.is_null() {
        return 0;
    }
    // SAFETY: an `NSString`.
    let bytes = unsafe { foundation::string::bytes(path) };
    let Ok(guest) = std::str::from_utf8(&bytes) else {
        return 0;
    };
    let host = prefix::ns::host_path(guest);
    let package = host
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| PACKAGES.iter().any(|p| p.eq_ignore_ascii_case(e)));
    u8::from(package && host.is_dir())
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSWorkspace").expect("declared before its methods");
    class_methods!(class, {
        "sharedWorkspace" => shared_workspace, c"@16@0:8";
    });
    methods!(class, {
        "URLForApplicationToOpenURL:" => application_url_for_url, c"@24@0:8@16";
        "openURL:" => open_url, c"c24@0:8@16";
        "openFile:" => open_file, c"c24@0:8@16";
        "openFile:withApplication:" => open_file_with, c"c32@0:8@16@24";
        "notificationCenter" => notification_center, c"@16@0:8";
        "frontmostApplication" => frontmost_application, c"@16@0:8";
        "runningApplications" => workspace_running_applications, c"@16@0:8";
        "launchApplicationAtURL:options:configuration:error:" => launch_application,
            c"@48@0:8@16Q24@32^@40";
        "iconForFileType:" => icon_for_file_type, c"@24@0:8@16";
        "iconForFile:" => icon_for_file_type, c"@24@0:8@16";
        "isFilePackageAtPath:" => is_file_package, c"c24@0:8@16";
    });

    let running =
        objc::class::lookup(c"NSRunningApplication").expect("declared before its methods");
    class_methods!(running, {
        "runningApplicationsWithBundleIdentifier:" => running_applications, c"@24@0:8@16";
        "currentApplication" => current_application, c"@16@0:8";
    });
    methods!(running, {
        "processIdentifier" => process_identifier, c"i16@0:8";
        "bundleIdentifier" => bundle_identifier, c"@16@0:8";
        "localizedName" => localized_name, c"@16@0:8";
        "isActive" => running_is_active, c"c16@0:8";
        "activateWithOptions:" => activate_with_options, c"c24@0:8Q16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_url_rejects_nil_and_text_without_a_scheme() {
        crate::bootstrap();
        let result = unsafe {
            open_url(
                std::ptr::null_mut(),
                objc::sel::get("openURL:"),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, 0);

        let url = corefoundation::url::new("not a URL");
        let contents = unsafe { corefoundation::url::as_url(url) }.unwrap();
        assert!(open_target(contents).is_none());
        unsafe { corefoundation::api::CFRelease(url) };
    }

    #[test]
    fn open_url_passes_web_urls_and_maps_file_urls() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSWorkspace").unwrap();
        assert!(class.method(objc::sel::get("openURL:")).is_some());

        let web = corefoundation::url::new("https://example.com/a?b=c");
        let contents = unsafe { corefoundation::url::as_url(web) }.unwrap();
        assert_eq!(
            open_target(contents),
            Some("https://example.com/a?b=c".to_string())
        );
        unsafe { corefoundation::api::CFRelease(web) };

        let file = corefoundation::url::new_file_url("/tmp/a file.txt", false);
        let contents = unsafe { corefoundation::url::as_url(file) }.unwrap();
        assert_eq!(
            open_target(contents),
            Some("file:///tmp/a%20file.txt".to_string())
        );
        unsafe { corefoundation::api::CFRelease(file) };
    }

    #[test]
    fn shared_workspace_is_one_process_lifetime_object() {
        crate::bootstrap();
        let class =
            objc::class::ptr(objc::class::lookup(c"NSWorkspace").expect("NSWorkspace")) as Id;
        // SAFETY: `+sharedWorkspace` is `id (Class, SEL)`.
        let first = unsafe { shared_workspace(class, objc::sel::get("sharedWorkspace")) };
        let second = unsafe { shared_workspace(class, objc::sel::get("sharedWorkspace")) };
        assert!(!first.is_null());
        assert_eq!(first, second);
        // SAFETY: `-notificationCenter` takes no explicit arguments.
        let center = unsafe { notification_center(first, objc::sel::get("notificationCenter")) };
        assert!(!center.is_null());
        assert_eq!(center, foundation::notification::default_center_object());
    }

    #[test]
    fn a_file_type_icon_is_an_image_even_for_an_unknown_type() {
        crate::bootstrap();
        let kind = foundation::string::from_str("rosemary-no-such-type");
        // SAFETY: `-iconForFileType:` is `id (id, SEL, id)`.
        let icon = unsafe {
            icon_for_file_type(
                std::ptr::null_mut(),
                objc::sel::get("iconForFileType:"),
                kind,
            )
        };
        assert!(!icon.is_null());
        assert_eq!(crate::image::declared_size(icon).width, 32.0);
    }

    #[test]
    fn frontmost_application_is_this_process() {
        crate::bootstrap();
        // SAFETY: both take no explicit arguments.
        let frontmost = unsafe {
            frontmost_application(std::ptr::null_mut(), objc::sel::get("frontmostApplication"))
        };
        let current = unsafe {
            current_application(std::ptr::null_mut(), objc::sel::get("currentApplication"))
        };
        assert!(!frontmost.is_null());
        assert_eq!(frontmost, current);
        // SAFETY: `-processIdentifier` takes no explicit arguments.
        let pid = unsafe { process_identifier(frontmost, objc::sel::get("processIdentifier")) };
        assert_eq!(pid, std::process::id() as i32);
    }

    #[test]
    fn running_applications_lists_this_process() {
        crate::bootstrap();
        // SAFETY: both take no explicit arguments.
        let list = unsafe {
            workspace_running_applications(
                std::ptr::null_mut(),
                objc::sel::get("runningApplications"),
            )
        };
        let current = unsafe {
            current_application(std::ptr::null_mut(), objc::sel::get("currentApplication"))
        };
        // SAFETY: the answer is a CFArray.
        let array = unsafe { corefoundation::collections::array(list as *const _) }.unwrap();
        assert_eq!(
            array.values,
            vec![current as corefoundation::base::CFTypeRef]
        );
    }
}
