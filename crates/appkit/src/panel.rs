use std::collections::HashMap;
use std::sync::Mutex;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// `NSModalResponseCancel`, which is also `NSFileHandlingPanelCancelButton`.
const CANCEL: i64 = 0;
/// `NSModalResponseOK`, which is also `NSFileHandlingPanelOKButton`.
const OK: i64 = 1;

struct Panel {
    save: bool,
    many: bool,
    files: bool,
    directories: bool,
    types: Vec<String>,
    directory: Option<String>,
    name: Option<String>,
    chosen: Vec<String>,
    accessory: usize,
    delegate: usize,
    flags: HashMap<String, bool>,
}

impl Panel {
    /// Apple's defaults, which a panel made with `-init` rather than
    /// `+openPanel` has too: files can be chosen, and the class decides
    /// whether it opens or saves.
    fn new(object: Id) -> Self {
        // SAFETY: a panel instance.
        let save = match (
            unsafe { objc::object::class_of(object) },
            objc::class::lookup(c"NSOpenPanel"),
        ) {
            (Some(class), Some(open)) => !class.chain().any(|c| std::ptr::eq(c, open)),
            _ => true,
        };
        Panel {
            save,
            many: false,
            files: true,
            directories: false,
            types: Vec::new(),
            directory: None,
            name: None,
            chosen: Vec::new(),
            accessory: 0,
            delegate: 0,
            flags: HashMap::new(),
        }
    }
}

/// Each panel's settings, by address; `-dealloc` removes the entry so a later
/// object at the same address starts fresh.
static PANELS: Mutex<Option<HashMap<usize, Panel>>> = Mutex::new(None);

fn with<R>(panel: Id, f: impl FnOnce(&mut Panel) -> R) -> R {
    let mut panels = PANELS.lock().unwrap_or_else(|e| e.into_inner());
    f(panels
        .get_or_insert_with(HashMap::new)
        .entry(panel as usize)
        .or_insert_with(|| Panel::new(panel)))
}

fn send(receiver: Id, selector: &str) -> Id {
    // SAFETY: a getter is `id (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector))
    }
}

fn text(string: Id) -> Option<String> {
    if string.is_null() {
        return None;
    }
    // SAFETY: an `NSString`.
    String::from_utf8(unsafe { foundation::string::bytes(string) }).ok()
}

/// `+openPanel` / `+savePanel`: an autoreleased panel of the receiving class.
///
/// The receiver is the class the message was sent to, so one function serves
/// both and an `NSOpenPanel` comes back for `+openPanel`.
unsafe extern "C" fn panel(class: Id, _sel: Sel) -> Id {
    // SAFETY: `class` is the class object the message was sent to.
    let Some(class) = (unsafe { objc::class::from_ptr(class as *const objc::class::Class) }) else {
        return std::ptr::null_mut();
    };
    let object = objc::object::create_instance(class, 0);
    with(object, |panel| *panel = Panel::new(object));
    objc::arc::autorelease(object)
}

unsafe extern "C" fn dealloc(receiver: Id, sel: Sel) {
    let mut panels = PANELS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(panels) = panels.as_mut() {
        panels.remove(&(receiver as usize));
    }
    drop(panels);
    // SAFETY: a panel is a window, and this is its last reference going.
    unsafe { crate::window::dealloc(receiver, sel) };
}

/// `-runModal`: the desktop's file chooser, through SDL, with the main loop
/// turning in the modal panel mode until it answers.
unsafe extern "C" fn run_modal(receiver: Id, _sel: Sel) -> i64 {
    let (kind, filters, start) = with(receiver, |panel| {
        let kind = match (panel.save, panel.directories && !panel.files) {
            (true, _) => sdl3::dialog::Kind::Save,
            (false, true) => sdl3::dialog::Kind::Folder { many: panel.many },
            (false, false) => sdl3::dialog::Kind::Open { many: panel.many },
        };
        let filters = match panel.types.is_empty() {
            true => Vec::new(),
            false => vec![("Allowed files".to_string(), panel.types.join(";"))],
        };
        let mut start = panel.directory.as_deref().map(|guest| {
            prefix::ns::host_path(guest)
                .into_os_string()
                .into_encoded_bytes()
        });
        if let (Some(dir), Some(name)) = (&mut start, &panel.name)
            && panel.save
        {
            dir.push(b'/');
            dir.extend_from_slice(name.as_bytes());
        }
        (kind, filters, start)
    });
    let window = crate::window::sdl_windows()
        .first()
        .copied()
        .unwrap_or(std::ptr::null_mut());
    let Some(answer) = sdl3::dialog::show(kind, window, &filters, start.as_deref()) else {
        eprintln!(
            "rosemary: appkit: no file chooser is available ({}); the panel is answered as cancelled",
            sdl3::error()
        );
        return CANCEL;
    };
    let app = crate::application::shared();
    let outcome = loop {
        if let Some(outcome) = answer.lock().unwrap_or_else(|e| e.into_inner()).take() {
            break outcome;
        }
        // SAFETY: the main thread, in a modal call as `-runModal` is.
        unsafe { crate::application::turn(app, crate::application::MODAL_PANEL_MODE) };
    };
    let paths = match outcome {
        sdl3::dialog::Outcome::Chosen(paths) => paths,
        sdl3::dialog::Outcome::Cancelled => Vec::new(),
        sdl3::dialog::Outcome::Failed(error) => {
            eprintln!(
                "rosemary: appkit: the file chooser failed ({error}); the panel is answered as cancelled"
            );
            Vec::new()
        }
    };
    let chosen: Vec<String> = paths
        .iter()
        .filter_map(|host| {
            use std::os::unix::ffi::OsStrExt;
            let host = std::path::Path::new(std::ffi::OsStr::from_bytes(host));
            let guest = prefix::ns::to_guest(host).and_then(|g| g.to_str().map(str::to_owned));
            if guest.is_none() {
                eprintln!(
                    "rosemary: appkit: {} has no path inside the prefix; it is left out of the panel's answer",
                    host.display()
                );
            }
            guest
        })
        .collect();
    let response = if chosen.is_empty() { CANCEL } else { OK };
    with(receiver, |panel| panel.chosen = chosen);
    response
}

// FIXME (ACCURACY): the chooser runs modally here and the handler is called
// before this returns; Apple returns at once and calls it later.
unsafe extern "C" fn begin(receiver: Id, sel: Sel, handler: Id) {
    // SAFETY: as `-runModal`.
    let response = unsafe { run_modal(receiver, sel) };
    // SAFETY: `void (^)(NSModalResponse)`, as the selector declares.
    if let Some(f) = unsafe { crate::animation::invoke(handler) } {
        unsafe {
            let f: unsafe extern "C" fn(Id, i64) = std::mem::transmute(f);
            f(handler, response);
        }
    }
}

unsafe extern "C" fn begin_sheet(receiver: Id, sel: Sel, _window: Id, handler: Id) {
    unsafe { begin(receiver, sel, handler) };
}

fn file_url(path: &str) -> Id {
    let string = foundation::string::from_str(path);
    let class = objc::class::lookup(c"NSURL").expect("Foundation is installed") as *const _ as Id;
    // SAFETY: `+fileURLWithPath:` is `id (id, SEL, id)`, autoreleased.
    let url = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        f(class, objc::sel::get("fileURLWithPath:"), string)
    };
    objc::arc::release(string);
    url
}

unsafe extern "C" fn urls(receiver: Id, sel: Sel) -> Id {
    // SAFETY: a selector the runtime owns.
    let names = unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() == b"filenames";
    let chosen = with(receiver, |panel| panel.chosen.clone());
    let items: Vec<Id> = chosen
        .iter()
        .map(|path| match names {
            true => objc::arc::autorelease(foundation::string::from_str(path)),
            false => file_url(path),
        })
        .collect();
    let values: Vec<_> = items.iter().map(|item| *item as _).collect();
    let array = corefoundation::collections::new_array(
        &values,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh array this call made.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn url(receiver: Id, sel: Sel) -> Id {
    // SAFETY: a selector the runtime owns.
    let name = unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() == b"filename";
    match with(receiver, |panel| panel.chosen.first().cloned()) {
        Some(path) if name => objc::arc::autorelease(foundation::string::from_str(&path)),
        Some(path) => file_url(&path),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn directory_url(receiver: Id, _sel: Sel) -> Id {
    with(receiver, |panel| panel.directory.clone()).map_or(std::ptr::null_mut(), |d| file_url(&d))
}

unsafe extern "C" fn set_directory_url(receiver: Id, _sel: Sel, url: Id) {
    let path = (!url.is_null()).then(|| text(send(url, "path"))).flatten();
    with(receiver, |panel| panel.directory = path);
}

unsafe extern "C" fn set_name(receiver: Id, _sel: Sel, name: Id) {
    let name = text(name);
    with(receiver, |panel| panel.name = name);
}

unsafe extern "C" fn name_field(receiver: Id, _sel: Sel) -> Id {
    match with(receiver, |panel| panel.name.clone()) {
        Some(name) => objc::arc::autorelease(foundation::string::from_str(&name)),
        None => objc::arc::autorelease(foundation::string::from_str("")),
    }
}

unsafe extern "C" fn set_allowed_file_types(receiver: Id, _sel: Sel, types: Id) {
    let mut found = Vec::new();
    if !types.is_null() {
        // SAFETY: an `NSArray` of `NSString`s.
        let count = unsafe { corefoundation::api::CFArrayGetCount(types as _) };
        for index in 0..count {
            let item = unsafe { corefoundation::api::CFArrayGetValueAtIndex(types as _, index) };
            if let Some(kind) = text(item as Id) {
                found.push(kind.trim_start_matches('.').to_string());
            }
        }
    }
    with(receiver, |panel| panel.types = found);
}

unsafe extern "C" fn set_many(receiver: Id, _sel: Sel, flag: u8) {
    with(receiver, |panel| panel.many = flag != 0);
}

unsafe extern "C" fn set_files(receiver: Id, _sel: Sel, flag: u8) {
    with(receiver, |panel| panel.files = flag != 0);
}

unsafe extern "C" fn set_directories(receiver: Id, _sel: Sel, flag: u8) {
    with(receiver, |panel| panel.directories = flag != 0);
}

unsafe extern "C" fn set_accessory(receiver: Id, _sel: Sel, view: Id) {
    with(receiver, |panel| panel.accessory = view as usize);
}

unsafe extern "C" fn accessory(receiver: Id, _sel: Sel) -> Id {
    with(receiver, |panel| panel.accessory) as Id
}

unsafe extern "C" fn set_delegate(receiver: Id, _sel: Sel, delegate: Id) {
    with(receiver, |panel| panel.delegate = delegate as usize);
}

unsafe extern "C" fn delegate(receiver: Id, _sel: Sel) -> Id {
    with(receiver, |panel| panel.delegate) as Id
}

fn flag_name(sel: Sel) -> String {
    // SAFETY: a selector the runtime owns, which lives for the process.
    let name = unsafe { std::ffi::CStr::from_ptr(sel) }
        .to_str()
        .unwrap_or("");
    let name = name
        .strip_prefix("set")
        .unwrap_or(name)
        .trim_end_matches(':');
    let name = name.strip_prefix("is").unwrap_or(name);
    let mut lower = name.to_string();
    if let Some(first) = lower.get_mut(0..1) {
        first.make_ascii_lowercase();
    }
    lower
}

unsafe extern "C" fn set_flag(receiver: Id, sel: Sel, flag: u8) {
    let name = flag_name(sel);
    with(receiver, |panel| panel.flags.insert(name, flag != 0));
}

unsafe extern "C" fn flag(receiver: Id, sel: Sel) -> u8 {
    let name = flag_name(sel);
    u8::from(with(receiver, |panel| {
        panel.flags.get(&name).copied().unwrap_or(false)
    }))
}

unsafe extern "C" fn many(receiver: Id, _sel: Sel) -> u8 {
    u8::from(with(receiver, |panel| panel.many))
}

unsafe extern "C" fn files(receiver: Id, _sel: Sel) -> u8 {
    u8::from(with(receiver, |panel| panel.files))
}

unsafe extern "C" fn directories(receiver: Id, _sel: Sel) -> u8 {
    u8::from(with(receiver, |panel| panel.directories))
}

unsafe extern "C" fn allowed_file_types(receiver: Id, _sel: Sel) -> Id {
    let types = with(receiver, |panel| panel.types.clone());
    if types.is_empty() {
        return std::ptr::null_mut();
    }
    let items: Vec<Id> = types
        .iter()
        .map(|t| foundation::string::from_str(t))
        .collect();
    let values: Vec<_> = items.iter().map(|item| *item as _).collect();
    let array = corefoundation::collections::new_array(
        &values,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for item in items {
        objc::arc::release(item);
    }
    // SAFETY: a fresh array this call made.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn ignored(_receiver: Id, _sel: Sel) {}

unsafe extern "C" fn ignored_object(_receiver: Id, _sel: Sel, _value: Id) {}

unsafe extern "C" fn no(_receiver: Id, _sel: Sel) -> u8 {
    0
}

pub(crate) fn install() {
    for name in [c"NSOpenPanel", c"NSSavePanel"] {
        let class = objc::class::lookup(name).expect("declared in `classes`");
        class_methods!(class, {
            "openPanel" => panel, c"@16@0:8";
            "savePanel" => panel, c"@16@0:8";
        });
        methods!(class, {
            "dealloc" => dealloc, c"v16@0:8";
            "runModal" => run_modal, c"q16@0:8";
            "beginWithCompletionHandler:" => begin, c"v24@0:8@?16";
            "beginSheetModalForWindow:completionHandler:" => begin_sheet, c"v32@0:8@16@?24";
            "URLs" => urls, c"@16@0:8";
            "filenames" => urls, c"@16@0:8";
            "URL" => url, c"@16@0:8";
            "filename" => url, c"@16@0:8";
            "directoryURL" => directory_url, c"@16@0:8";
            "setDirectoryURL:" => set_directory_url, c"v24@0:8@16";
            "setAllowedFileTypes:" => set_allowed_file_types, c"v24@0:8@16";
            "setNameFieldStringValue:" => set_name, c"v24@0:8@16";
            "nameFieldStringValue" => name_field, c"@16@0:8";
            "setAccessoryView:" => set_accessory, c"v24@0:8@16";
            "accessoryView" => accessory, c"@16@0:8";
            "setDelegate:" => set_delegate, c"v24@0:8@16";
            "delegate" => delegate, c"@16@0:8";
            "setTitle:" => ignored_object, c"v24@0:8@16";
            "setMessage:" => ignored_object, c"v24@0:8@16";
            "setPrompt:" => ignored_object, c"v24@0:8@16";
            "setNameFieldLabel:" => ignored_object, c"v24@0:8@16";
            "setCanChooseFiles:" => set_files, c"v20@0:8c16";
            "setCanChooseDirectories:" => set_directories, c"v20@0:8c16";
            "setAllowsMultipleSelection:" => set_many, c"v20@0:8c16";
            "canChooseFiles" => files, c"c16@0:8";
            "canChooseDirectories" => directories, c"c16@0:8";
            "allowsMultipleSelection" => many, c"c16@0:8";
            "allowedFileTypes" => allowed_file_types, c"@16@0:8";
            "setCanCreateDirectories:" => set_flag, c"v20@0:8c16";
            "canCreateDirectories" => flag, c"c16@0:8";
            "setResolvesAliases:" => set_flag, c"v20@0:8c16";
            "resolvesAliases" => flag, c"c16@0:8";
            "setShowsHiddenFiles:" => set_flag, c"v20@0:8c16";
            "showsHiddenFiles" => flag, c"c16@0:8";
            "setTreatsFilePackagesAsDirectories:" => set_flag, c"v20@0:8c16";
            "treatsFilePackagesAsDirectories" => flag, c"c16@0:8";
            "setExtensionHidden:" => set_flag, c"v20@0:8c16";
            "isExtensionHidden" => flag, c"c16@0:8";
            "setCanSelectHiddenExtension:" => set_flag, c"v20@0:8c16";
            "canSelectHiddenExtension" => flag, c"c16@0:8";
            "setAllowsOtherFileTypes:" => set_flag, c"v20@0:8c16";
            "allowsOtherFileTypes" => flag, c"c16@0:8";
            "setAccessoryViewDisclosed:" => set_flag, c"v20@0:8c16";
            "isAccessoryViewDisclosed" => flag, c"c16@0:8";
            "setShowsTagField:" => set_flag, c"v20@0:8c16";
            "showsTagField" => flag, c"c16@0:8";
            "validateVisibleColumns" => ignored, c"v16@0:8";
            "isVisible" => no, c"c16@0:8";
            "isSheet" => no, c"c16@0:8";
            "close" => ignored, c"v16@0:8";
            "orderOut:" => ignored_object, c"v24@0:8@16";
            "cancel:" => ignored_object, c"v24@0:8@16";
            "ok:" => ignored_object, c"v24@0:8@16";
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_panel_keeps_what_it_is_told() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSOpenPanel").expect("NSOpenPanel") as *const _ as Id;
        let pool = objc::arc::pool_push();
        // SAFETY: `+openPanel` is `id (id, SEL)`.
        let panel = unsafe { panel(class, objc::sel::get("openPanel")) };
        let name = foundation::string::from_str("model.fbx");
        unsafe {
            set_name(panel, objc::sel::get("setNameFieldStringValue:"), name);
            set_many(panel, objc::sel::get("setAllowsMultipleSelection:"), 1);
        }
        objc::arc::release(name);
        unsafe {
            set_flag(
                panel,
                objc::sel::get("setTreatsFilePackagesAsDirectories:"),
                1,
            );
            assert_eq!(
                flag(panel, objc::sel::get("treatsFilePackagesAsDirectories")),
                1
            );
            assert_eq!(flag(panel, objc::sel::get("isExtensionHidden")), 0);
            set_flag(panel, objc::sel::get("setExtensionHidden:"), 1);
            assert_eq!(flag(panel, objc::sel::get("isExtensionHidden")), 1);
        }
        with(panel, |p| {
            assert!(!p.save);
            assert!(p.many && p.files);
            assert_eq!(p.name.as_deref(), Some("model.fbx"));
            p.chosen = vec!["/Users/me/model.fbx".to_string()];
        });
        // SAFETY: `-filenames` is `id (id, SEL)`.
        let names = unsafe { urls(panel, objc::sel::get("filenames")) };
        assert_eq!(
            unsafe { corefoundation::api::CFArrayGetCount(names as _) },
            1
        );
        objc::arc::pool_pop(pool);
    }
}
