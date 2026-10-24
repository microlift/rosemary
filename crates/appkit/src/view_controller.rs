//! `NSViewController`, and `NSTitlebarAccessoryViewController` on top of it.
//!
//! A controller loads its view on first use: from the nib it was made with,
//! or from a subclass's own `-loadView`. Its state is kept as associations,
//! see [`crate::props`].

use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;
use crate::props::{self, properties};

/// Send `sel` to `receiver`, which takes and returns nothing.
fn send(receiver: Id, sel: &str) {
    // SAFETY: `void (id, SEL)`, a message every controller answers.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel));
    }
}

/// `-initWithNibName:bundle:`. A nil name is resolved when the view loads.
unsafe extern "C" fn init_with_nib_name(receiver: Id, _sel: Sel, name: Id, bundle: Id) -> Id {
    if receiver.is_null() {
        return receiver;
    }
    // SAFETY: the `copy` and strong setters installed below.
    unsafe {
        props::set_copied(receiver, objc::sel::get("setNibName:"), name);
        props::set_retained(receiver, objc::sel::get("setNibBundle:"), bundle);
    }
    receiver
}

unsafe extern "C" fn init(receiver: Id, sel: Sel) -> Id {
    unsafe { init_with_nib_name(receiver, sel, std::ptr::null_mut(), std::ptr::null_mut()) }
}

/// `-initWithCoder:`, as a controller decoded from a nib: no nib of its own
/// beyond what the archive names, which this runtime's decoder sets after.
unsafe extern "C" fn init_with_coder(receiver: Id, sel: Sel, _coder: Id) -> Id {
    unsafe { init(receiver, sel) }
}

/// `-view`: loaded on first use, followed by `-viewDidLoad`.
unsafe extern "C" fn view(receiver: Id, _sel: Sel) -> Id {
    let loaded = props::object(receiver, "view");
    if !loaded.is_null() {
        return loaded;
    }
    send(receiver, "loadView");
    let loaded = props::object(receiver, "view");
    if !loaded.is_null() {
        send(receiver, "viewDidLoad");
    }
    loaded
}

/// `-setView:`. The controller becomes the view's next responder, as AppKit
/// has made it since 10.10, so actions sent to the view reach it.
unsafe extern "C" fn set_view(receiver: Id, sel: Sel, value: Id) {
    unsafe { props::set_retained(receiver, sel, value) };
}

unsafe extern "C" fn is_view_loaded(receiver: Id, _sel: Sel) -> u8 {
    (!props::object(receiver, "view").is_null()) as u8
}

/// `-loadView`: the nib named `nibName`, or the one named for the class when
/// that is nil, loaded with the controller as its owner.
unsafe extern "C" fn load_view(receiver: Id, _sel: Sel) {
    let mut name = props::object(receiver, "nibName");
    let mut made = std::ptr::null_mut();
    if name.is_null() {
        // SAFETY: a live object.
        let class = unsafe { objc::object::class_of(receiver) };
        made = foundation::string::from_str(class.map_or("", |c| c.name_str()));
        name = made;
    }
    let mut bundle = props::object(receiver, "nibBundle");
    if bundle.is_null() {
        // SAFETY: `+[NSBundle mainBundle]`.
        bundle = unsafe {
            let class = objc::class::lookup(c"NSBundle").expect("Foundation's NSBundle");
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(objc::class::ptr(class) as Id, objc::sel::get("mainBundle"))
        };
    }
    // SAFETY: `-[NSBundle loadNibNamed:owner:topLevelObjects:]`.
    let loaded = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, *mut Id) -> u8 =
            std::mem::transmute(objc::msg::entry());
        f(
            bundle,
            objc::sel::get("loadNibNamed:owner:topLevelObjects:"),
            name,
            receiver,
            std::ptr::null_mut(),
        )
    };
    if loaded == 0 {
        let text =
            String::from_utf8_lossy(&unsafe { foundation::string::bytes(name) }).into_owned();
        eprintln!("rosemary: appkit: NSViewController could not load the nib '{text}'");
    }
    objc::arc::release(made);
}

unsafe extern "C" fn nothing(_receiver: Id, _sel: Sel) {}

/// `NSLayoutAttribute`: where in the titlebar an accessory goes.
pub(crate) const BOTTOM: usize = 4;

unsafe extern "C" fn accessory_init(receiver: Id, sel: Sel, name: Id, bundle: Id) -> Id {
    let receiver = unsafe { init_with_nib_name(receiver, sel, name, bundle) };
    props::set_integer_of(receiver, "layoutAttribute", BOTTOM);
    receiver
}

unsafe extern "C" fn accessory_plain_init(receiver: Id, sel: Sel) -> Id {
    unsafe { accessory_init(receiver, sel, std::ptr::null_mut(), std::ptr::null_mut()) }
}

/// `-setHidden:` on an accessory: its window lays its titlebar out again.
unsafe extern "C" fn accessory_set_hidden(receiver: Id, sel: Sel, hidden: u8) {
    unsafe { props::set_bool(receiver, sel, hidden) };
    // AppKit collapses the accessory's container, so its view is not drawn
    // either; here the view sits in the frame view directly.
    let view = props::object(receiver, "view");
    if !view.is_null() {
        // SAFETY: `-setHidden:` is `void (id, SEL, BOOL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, u8) = std::mem::transmute(objc::msg::entry());
            f(view, objc::sel::get("setHidden:"), hidden);
        }
    }
    let window = accessory_window(receiver);
    if !window.is_null() {
        crate::window::fit_content_view(window);
    }
}

/// The window an accessory is in, which [`set_accessory_window`] records.
pub(crate) fn accessory_window(accessory: Id) -> Id {
    props::object(accessory, "_rosemaryWindow")
}

pub(crate) fn set_accessory_window(accessory: Id, window: Id) {
    objc::arc::set_associated(
        accessory,
        objc::sel::get("_rosemaryWindow") as *const std::ffi::c_void,
        window,
    );
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSViewController").expect("declared before its methods");
    methods!(class, {
        "init" => init, c"@16@0:8";
        "initWithNibName:bundle:" => init_with_nib_name, c"@32@0:8@16@24";
        "initWithCoder:" => init_with_coder, c"@24@0:8@16";
        "view" => view, c"@16@0:8";
        "setView:" => set_view, c"v24@0:8@16";
        "isViewLoaded" => is_view_loaded, c"c16@0:8";
        "loadView" => load_view, c"v16@0:8";
        "viewDidLoad" => nothing, c"v16@0:8";
        "viewWillAppear" => nothing, c"v16@0:8";
        "viewDidAppear" => nothing, c"v16@0:8";
        "viewWillDisappear" => nothing, c"v16@0:8";
        "viewDidDisappear" => nothing, c"v16@0:8";
        "viewWillLayout" => nothing, c"v16@0:8";
        "viewDidLayout" => nothing, c"v16@0:8";
        "updateViewConstraints" => nothing, c"v16@0:8";
    });
    properties!(class, {
        copied "nibName", "setNibName:";
        retained "nibBundle", "setNibBundle:";
        retained "representedObject", "setRepresentedObject:";
        copied "title", "setTitle:";
        copied "identifier", "setIdentifier:";
    });

    let accessory = objc::class::lookup(c"NSTitlebarAccessoryViewController")
        .expect("declared before its methods");
    methods!(accessory, {
        "init" => accessory_plain_init, c"@16@0:8";
        "initWithNibName:bundle:" => accessory_init, c"@32@0:8@16@24";
        "setHidden:" => accessory_set_hidden, c"v20@0:8c16";
    });
    properties!(accessory, {
        integer "layoutAttribute", "setLayoutAttribute:";
        double "fullScreenMinHeight", "setFullScreenMinHeight:";
        bool "automaticallyAdjustsSize", "setAutomaticallyAdjustsSize:";
    });
    methods!(accessory, {
        "isHidden" => props::get_bool, c"c16@0:8";
    });
}
