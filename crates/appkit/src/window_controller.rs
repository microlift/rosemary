//! `NSWindowController`: it owns one window, made by the program or loaded
//! from a nib the first time `-window` is asked, and shows it. Documents are
//! not here beyond keeping the one a program sets.

use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

/// The association key for a controller's window, which it retains.
static WINDOW: u8 = 0;

fn key() -> *const std::ffi::c_void {
    &WINDOW as *const u8 as *const std::ffi::c_void
}

fn window(controller: Id) -> Id {
    objc::arc::associated(controller, key())
}

/// # Safety
///
/// The selector must name a method taking one object and returning nothing.
unsafe fn send_object(receiver: Id, selector: &str, argument: Id) {
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector), argument);
    }
}

fn set_window(controller: Id, value: Id) {
    let old = window(controller);
    if old == value {
        return;
    }
    objc::arc::retain(value);
    objc::arc::set_associated(controller, key(), value);
    if !value.is_null() {
        // SAFETY: an `NSWindow`, which answers it.
        unsafe { send_object(value, "setWindowController:", controller) };
    }
    if !old.is_null() {
        // SAFETY: as above; the old window no longer has this controller.
        unsafe { send_object(old, "setWindowController:", std::ptr::null_mut()) };
        objc::arc::release(old);
    }
}

unsafe extern "C" fn init_with_window(receiver: Id, _sel: Sel, value: Id) -> Id {
    set_window(receiver, value);
    receiver
}

/// `-initWithWindowNibName:`, owned by the controller itself.
unsafe extern "C" fn init_with_nib_name(receiver: Id, _sel: Sel, name: Id) -> Id {
    unsafe { init_with_nib_name_owner(receiver, _sel, name, receiver) }
}

/// `-initWithWindowNibName:owner:`. The nib is not loaded until the window
/// is first asked for, and the owner is not retained, as in AppKit.
unsafe extern "C" fn init_with_nib_name_owner(receiver: Id, _sel: Sel, name: Id, owner: Id) -> Id {
    objc::arc::set_associated_with_policy(receiver, key_of(&NIB_NAME), name, 3);
    objc::arc::set_associated(receiver, key_of(&OWNER), owner);
    receiver
}

/// `-initWithWindowNibPath:owner:`, a nib named by its path.
unsafe extern "C" fn init_with_nib_path_owner(receiver: Id, _sel: Sel, path: Id, owner: Id) -> Id {
    objc::arc::set_associated_with_policy(receiver, key_of(&NIB_PATH), path, 3);
    objc::arc::set_associated(receiver, key_of(&OWNER), owner);
    receiver
}

static NIB_NAME: u8 = 0;
static NIB_PATH: u8 = 0;
static OWNER: u8 = 0;
/// The nib's top-level objects, which the controller owns once loaded.
static TOP_LEVEL: u8 = 0;
/// Set once loading has been tried, so a nib without a window is not
/// loaded again on every `-window`.
static TRIED: u8 = 0;

fn key_of(slot: &'static u8) -> *const std::ffi::c_void {
    slot as *const u8 as *const std::ffi::c_void
}

unsafe extern "C" fn nib_name(receiver: Id, _sel: Sel) -> Id {
    let name = objc::arc::associated(receiver, key_of(&NIB_NAME));
    if !name.is_null() {
        return name;
    }
    let path = objc::arc::associated(receiver, key_of(&NIB_PATH));
    if path.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: an NSString.
    let path = String::from_utf8_lossy(&unsafe { foundation::string::bytes(path) }).into_owned();
    let stem = std::path::Path::new(&path)
        .file_stem()
        .map_or(String::new(), |s| s.to_string_lossy().into_owned());
    objc::arc::autorelease(foundation::string::from_str(&stem))
}

unsafe extern "C" fn nib_path(receiver: Id, _sel: Sel) -> Id {
    let path = objc::arc::associated(receiver, key_of(&NIB_PATH));
    if !path.is_null() {
        return path;
    }
    match nib_file(receiver).and_then(|found| prefix::ns::to_guest(&found)) {
        Some(found) => {
            objc::arc::autorelease(foundation::string::from_str(&found.to_string_lossy()))
        }
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn owner(receiver: Id, _sel: Sel) -> Id {
    objc::arc::associated(receiver, key_of(&OWNER))
}

/// The nib file on the host: the path given, or the name looked up in the
/// owner's class's bundle and then the main bundle.
fn nib_file(receiver: Id) -> Option<std::path::PathBuf> {
    let path = objc::arc::associated(receiver, key_of(&NIB_PATH));
    if !path.is_null() {
        // SAFETY: an NSString.
        let path =
            String::from_utf8_lossy(&unsafe { foundation::string::bytes(path) }).into_owned();
        return Some(prefix::ns::host_path(&path));
    }
    let name = objc::arc::associated(receiver, key_of(&NIB_NAME));
    if name.is_null() {
        return None;
    }
    // SAFETY: an NSString.
    let name = String::from_utf8_lossy(&unsafe { foundation::string::bytes(name) }).into_owned();
    let owner = objc::arc::associated(receiver, key_of(&OWNER));
    // SAFETY: `+bundleForClass:` is `id (Class, SEL, Class)`.
    let bundle = unsafe {
        let class = objc::class::lookup(c"NSBundle").expect("Foundation registered NSBundle");
        let owner_class = objc::object::class_of(owner)
            .map_or(std::ptr::null_mut(), |c| objc::class::ptr(c) as Id);
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        f(
            objc::class::ptr(class) as Id,
            objc::sel::get("bundleForClass:"),
            owner_class,
        )
    };
    // SAFETY: a bundle object or nil.
    unsafe { foundation::bundle::resource_for_object(bundle, &name, "nib") }
        .or_else(|| foundation::bundle::main()?.resource(&name, "nib"))
}

/// `-loadWindow`: load the nib with the owner, which connects the window
/// outlet; the controller keeps the nib's top-level objects.
unsafe extern "C" fn load_window(receiver: Id, _sel: Sel) {
    objc::arc::set_associated_word(receiver, key_of(&TRIED), 1);
    let Some(path) = nib_file(receiver) else {
        eprintln!("rosemary: appkit: NSWindowController found no nib to load its window from");
        return;
    };
    let owner = objc::arc::associated(receiver, key_of(&OWNER));
    let loaded = match crate::nib::load(&path, owner) {
        Ok(loaded) => loaded,
        Err(why) => {
            eprintln!("rosemary: appkit: {}: {why}", path.display());
            return;
        }
    };
    loaded.report(&path);
    let objects: Vec<_> = loaded
        .objects
        .values()
        .filter(|object| !object.is_null() && **object != owner)
        .map(|object| *object as corefoundation::base::CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &objects,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // The array holds them now; the load's own references go.
    for object in &objects {
        objc::arc::release(*object as Id);
    }
    objc::arc::set_associated_with_policy(receiver, key_of(&TOP_LEVEL), array as Id, 1);
    objc::arc::release(array as Id);
}

/// `-window`, loading it from the nib the first time when there is one.
unsafe extern "C" fn window_of(receiver: Id, _sel: Sel) -> Id {
    let current = window(receiver);
    let has_nib = !objc::arc::associated(receiver, key_of(&NIB_NAME)).is_null()
        || !objc::arc::associated(receiver, key_of(&NIB_PATH)).is_null();
    let tried = !objc::arc::associated(receiver, key_of(&TRIED)).is_null();
    if !current.is_null() || !has_nib || tried {
        return current;
    }
    // Sent, so a subclass's overrides run, in AppKit's order.
    for selector in ["windowWillLoad", "loadWindow"] {
        // SAFETY: both are `void (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get(selector));
        }
    }
    objc::arc::set_associated_word(receiver, key_of(&TRIED), 1);
    let loaded = window(receiver);
    if !loaded.is_null() {
        // SAFETY: `-windowDidLoad` is `void (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get("windowDidLoad"));
        }
    }
    loaded
}

unsafe extern "C" fn set_window_of(receiver: Id, _sel: Sel, value: Id) {
    set_window(receiver, value);
}

/// The window, through `-window` so a nib is loaded first.
fn loaded_window(receiver: Id) -> Id {
    // SAFETY: `-window` is `id (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("window"))
    }
}

unsafe extern "C" fn show_window(receiver: Id, _sel: Sel, _sender: Id) {
    let window = loaded_window(receiver);
    if !window.is_null() {
        // SAFETY: an `NSWindow`.
        unsafe { send_object(window, "makeKeyAndOrderFront:", receiver) };
    }
}

unsafe extern "C" fn close(receiver: Id, _sel: Sel) {
    let window = window(receiver);
    if !window.is_null() {
        // SAFETY: `-close` is `void (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
            f(window, objc::sel::get("close"));
        }
    }
}

unsafe extern "C" fn is_window_loaded(receiver: Id, _sel: Sel) -> u8 {
    u8::from(!window(receiver).is_null())
}

unsafe extern "C" fn nothing(_receiver: Id, _sel: Sel) {}

/// `-windowTitleForDocumentDisplayName:`: the name as it is.
unsafe extern "C" fn title_for_display_name(_receiver: Id, _sel: Sel, name: Id) -> Id {
    name
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    set_window(receiver, std::ptr::null_mut());
    objc::arc::finalize(receiver);
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSWindowController").expect("declared before its methods");
    methods!(class, {
        "initWithWindow:" => init_with_window, c"@24@0:8@16";
        "initWithWindowNibName:" => init_with_nib_name, c"@24@0:8@16";
        "initWithWindowNibName:owner:" => init_with_nib_name_owner, c"@32@0:8@16@24";
        "initWithWindowNibPath:owner:" => init_with_nib_path_owner, c"@32@0:8@16@24";
        "windowNibName" => nib_name, c"@16@0:8";
        "windowNibPath" => nib_path, c"@16@0:8";
        "owner" => owner, c"@16@0:8";
        "loadWindow" => load_window, c"v16@0:8";
        "windowTitleForDocumentDisplayName:" => title_for_display_name, c"@24@0:8@16";
        "synchronizeWindowTitleWithDocumentName" => nothing, c"v16@0:8";
        "window" => window_of, c"@16@0:8";
        "setWindow:" => set_window_of, c"v24@0:8@16";
        "showWindow:" => show_window, c"v24@0:8@16";
        "close" => close, c"v16@0:8";
        "isWindowLoaded" => is_window_loaded, c"c16@0:8";
        "windowDidLoad" => nothing, c"v16@0:8";
        "windowWillLoad" => nothing, c"v16@0:8";
        "dealloc" => dealloc, c"v16@0:8";
    });
    crate::props::properties!(class, {
        assigned "document", "setDocument:";
        copied "windowFrameAutosaveName", "setWindowFrameAutosaveName:";
        bool "shouldCloseDocument", "setShouldCloseDocument:";
    });
    // AppKit's default is to cascade.
    methods!(class, {
        "shouldCascadeWindows" => cascades, c"c16@0:8";
        "setShouldCascadeWindows:" => set_cascades, c"v20@0:8c16";
    });
}

static NO_CASCADE: u8 = 0;

unsafe extern "C" fn cascades(receiver: Id, _sel: Sel) -> u8 {
    u8::from(objc::arc::associated(receiver, key_of(&NO_CASCADE)).is_null())
}

unsafe extern "C" fn set_cascades(receiver: Id, _sel: Sel, flag: u8) {
    objc::arc::set_associated_word(receiver, key_of(&NO_CASCADE), usize::from(flag == 0));
}
