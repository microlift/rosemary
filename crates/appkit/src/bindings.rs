//! Cocoa Bindings: `-bind:toObject:withKeyPath:options:` and the controller
//! that most bindings go through, `NSUserDefaultsController`.
//!
//! A binding is a KVO observer (a binder) that the bound object keeps as an
//! association under the binding's name. Each change of the observed key path
//! is transformed by the binding's options and handed to the bound object's
//! setter for that binding. A control the user changes pushes its value back
//! through its `value` binding.

use std::ffi::{CStr, c_void};

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// `NSKeyValueObservingOptionNew | NSKeyValueObservingOptionInitial`.
const OBSERVING: usize = 0x01 | 0x04;

#[repr(C)]
struct Binder {
    isa: *const c_void,
    /// The bound object, which owns the binder; not retained.
    owner: Id,
    name: Id,
    /// Retained, as AppKit's binders retain what they observe: unbinding sends
    /// it a message.
    observed: Id,
    key_path: Id,
    options: Id,
}

fn binder_class() -> &'static objc::class::Class {
    objc::class::lookup(c"_RosemaryBinder").expect("installed with the bindings")
}

fn string(text: &str) -> Id {
    foundation::string::from_str(text)
}

fn release_string(string: Id) {
    // SAFETY: a string `string` made.
    unsafe { corefoundation::api::CFRelease(string as corefoundation::base::CFTypeRef) };
}

fn text(string: Id) -> String {
    // SAFETY: an `NSString` or nil.
    String::from_utf8_lossy(&unsafe { foundation::string::bytes(string) }).into_owned()
}

fn send_id(receiver: Id, sel: &str) -> Id {
    // SAFETY: `id (id, SEL)`, as each selector this is used with is.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel))
    }
}

fn send_id_id(receiver: Id, sel: &str, argument: Id) -> Id {
    // SAFETY: `id (id, SEL, id)`, as each selector this is used with is.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel), argument)
    }
}

fn responds(object: Id, sel: &str) -> bool {
    // SAFETY: a live object.
    unsafe { objc::object::class_of(object) }
        .is_some_and(|c| c.method(objc::sel::get(sel)).is_some())
}

fn is_kind(object: Id, name: &CStr) -> bool {
    let Some(class) = objc::class::lookup(name) else {
        return false;
    };
    // SAFETY: a live object.
    unsafe { objc::object::class_of(object) }
        .is_some_and(|c| c.chain().any(|c| std::ptr::eq(c, class)))
}

/// The association key a binding of this name is kept under.
fn key(name: &str) -> *const c_void {
    objc::sel::get(&format!("rosemary binding {name}")) as *const c_void
}

fn binder_of(owner: Id, name: &str) -> Option<&'static mut Binder> {
    let binder = objc::arc::associated(owner, key(name));
    // SAFETY: only binders are stored under these keys.
    (!binder.is_null()).then(|| unsafe { &mut *(binder as *mut Binder) })
}

/// The option named `name`, or nil.
fn option(binder: &Binder, name: &str) -> Id {
    if binder.options.is_null() {
        return std::ptr::null_mut();
    }
    let key = string(name);
    let value = send_id_id(binder.options, "objectForKey:", key);
    release_string(key);
    value
}

/// The binding's value transformer, given by object or by name.
fn transformer(binder: &Binder) -> Id {
    let given = option(binder, "NSValueTransformer");
    if !given.is_null() {
        return given;
    }
    let name = option(binder, "NSValueTransformerName");
    if name.is_null() {
        return std::ptr::null_mut();
    }
    let class = objc::class::lookup(c"NSValueTransformer").expect("Foundation registered it");
    send_id_id(
        objc::class::ptr(class) as Id,
        "valueTransformerForName:",
        name,
    )
}

fn is_null_object(value: Id) -> bool {
    !value.is_null() && is_kind(value, c"NSNull")
}

/// The observed value as the bound object should see it.
fn presented(binder: &Binder) -> Id {
    let mut value = send_id_id(binder.observed, "valueForKeyPath:", binder.key_path);
    let transformer = transformer(binder);
    if !transformer.is_null() {
        value = send_id_id(transformer, "transformedValue:", value);
    }
    if is_null_object(value) {
        value = std::ptr::null_mut();
    }
    if value.is_null() {
        value = option(binder, "NSNullPlaceholder");
    }
    value
}

fn as_bool(value: Id) -> bool {
    if value.is_null() || is_null_object(value) {
        return false;
    }
    if !responds(value, "boolValue") {
        return true;
    }
    // SAFETY: `BOOL (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        f(value, objc::sel::get("boolValue")) != 0
    }
}

fn as_integer(value: Id) -> isize {
    if value.is_null() || !responds(value, "integerValue") {
        return 0;
    }
    // SAFETY: `NSInteger (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> isize = std::mem::transmute(objc::msg::entry());
        f(value, objc::sel::get("integerValue"))
    }
}

/// How a binding's value reaches the bound object.
enum Setter {
    Object(&'static str),
    Bool(&'static str),
    Integer(&'static str),
}

/// The setter AppKit's own classes expose a binding through.
fn setter_for(owner: Id, name: &str) -> Option<Setter> {
    Some(match name {
        "value" if is_kind(owner, c"NSButton") || is_kind(owner, c"NSMenuItem") => {
            Setter::Integer("setState:")
        }
        "value" => Setter::Object("setObjectValue:"),
        "enabled" => Setter::Bool("setEnabled:"),
        "hidden" => Setter::Bool("setHidden:"),
        "editable" => Setter::Bool("setEditable:"),
        "documentEdited" => Setter::Bool("setDocumentEdited:"),
        "title" => Setter::Object("setTitle:"),
        "toolTip" => Setter::Object("setToolTip:"),
        "image" => Setter::Object("setImage:"),
        "selectedTag" => Setter::Integer("selectItemWithTag:"),
        "selectedIndex" => Setter::Integer("selectItemAtIndex:"),
        "selectedValue" => Setter::Object("selectItemWithTitle:"),
        "selectionIndexes" => Setter::Object("setSelectionIndexes:"),
        _ => return None,
    })
}

/// Hand the observed value to the bound object.
fn apply(binder: &Binder) {
    let value = presented(binder);
    let name = text(binder.name);
    let owner = binder.owner;
    match setter_for(owner, &name) {
        Some(Setter::Object(sel)) if responds(owner, sel) => {
            send_id_id(owner, sel, value);
        }
        // SAFETY: `void (id, SEL, BOOL)`.
        Some(Setter::Bool(sel)) if responds(owner, sel) => unsafe {
            let f: unsafe extern "C" fn(Id, Sel, u8) = std::mem::transmute(objc::msg::entry());
            f(owner, objc::sel::get(sel), as_bool(value) as u8);
        },
        // SAFETY: `void (id, SEL, NSInteger)`.
        Some(Setter::Integer(sel)) if responds(owner, sel) => unsafe {
            let f: unsafe extern "C" fn(Id, Sel, isize) = std::mem::transmute(objc::msg::entry());
            f(owner, objc::sel::get(sel), as_integer(value));
        },
        // Anything else is the plain `NSObject` binding: the binding's name is
        // a key on the bound object.
        _ => {
            // SAFETY: a live object.
            let Some(class) = (unsafe { objc::object::class_of(owner) }) else {
                return;
            };
            if !foundation::kvc::accepts(class, &name) {
                eprintln!(
                    "rosemary: bindings: {} has no {name:?} binding; ignoring it",
                    class.name_str()
                );
                return;
            }
            // AppKit's binder passes nil on to `-setNilValueForKey:` only when
            // a class has made that mean something; a key bound through a nil
            // object on the way is otherwise left as it is.
            // FIXME (ACCURACY): which of the two AppKit does here is inferred
            // from TextMate binding scalars through a not-yet-set document.
            if value.is_null() && foundation::kvc::rejects_nil(class, &name) {
                return;
            }
            if std::env::var_os("ROSEMARY_TRACE_BINDINGS").is_some() {
                // SAFETY: a live object.
                let observed = unsafe { objc::object::class_of(binder.observed) }
                    .map_or("nil", |c| c.name_str());
                eprintln!(
                    "rosemary: bindings: {}.{name} <- {observed}.{} = {value:p}",
                    class.name_str(),
                    text(binder.key_path)
                );
            }
            let key = string(&name);
            // SAFETY: `void (id, SEL, id, NSString *)`.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Id, Id) =
                    std::mem::transmute(objc::msg::entry());
                f(owner, objc::sel::get("setValue:forKey:"), value, key);
            }
            release_string(key);
        }
    }
}

unsafe extern "C" fn binder_observe(
    receiver: Id,
    _sel: Sel,
    _key_path: Id,
    _object: Id,
    _change: Id,
    _context: usize,
) {
    // SAFETY: the binder this class makes.
    apply(unsafe { &*(receiver as *const Binder) });
}

unsafe extern "C" fn binder_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: the binder this class makes.
    let binder = unsafe { &mut *(receiver as *mut Binder) };
    // SAFETY: `void (id, SEL, id, NSString *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) = std::mem::transmute(objc::msg::entry());
        f(
            binder.observed,
            objc::sel::get("removeObserver:forKeyPath:"),
            receiver,
            binder.key_path,
        );
    }
    for field in [
        binder.name,
        binder.observed,
        binder.key_path,
        binder.options,
    ] {
        objc::arc::release(field);
    }
    objc::arc::finalize(receiver);
}

/// `-bind:toObject:withKeyPath:options:`.
unsafe extern "C" fn bind(
    receiver: Id,
    _sel: Sel,
    name: Id,
    observed: Id,
    key_path: Id,
    options: Id,
) {
    let binding = text(name);
    // Binding a name again replaces the old binding, as AppKit does.
    objc::arc::set_associated_with_policy(receiver, key(&binding), std::ptr::null_mut(), 1);
    if observed.is_null() || key_path.is_null() {
        return;
    }
    let binder = objc::object::create_instance(binder_class(), 0);
    // SAFETY: a fresh instance of the binder class.
    unsafe {
        *(binder as *mut Binder) = Binder {
            isa: *(binder as *const *const c_void),
            owner: receiver,
            name: send_id(name, "copy"),
            observed: objc::arc::retain(observed),
            key_path: send_id(key_path, "copy"),
            options: send_id(options, "copy"),
        };
    }
    objc::arc::set_associated_with_policy(receiver, key(&binding), binder, 1);
    objc::arc::release(binder);
    // SAFETY: `void (id, SEL, id, NSString *, NSKeyValueObservingOptions, void *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, usize, usize) =
            std::mem::transmute(objc::msg::entry());
        f(
            observed,
            objc::sel::get("addObserver:forKeyPath:options:context:"),
            binder,
            key_path,
            OBSERVING,
            0,
        );
    }
}

unsafe extern "C" fn unbind(receiver: Id, _sel: Sel, name: Id) {
    objc::arc::set_associated_with_policy(receiver, key(&text(name)), std::ptr::null_mut(), 1);
}

/// `-infoForBinding:`: the observed object, key path and options, or nil.
unsafe extern "C" fn info_for_binding(receiver: Id, _sel: Sel, name: Id) -> Id {
    let Some(binder) = binder_of(receiver, &text(name)) else {
        return std::ptr::null_mut();
    };
    let options = match binder.options.is_null() {
        true => {
            let class = objc::class::lookup(c"NSDictionary").expect("NSDictionary");
            send_id(objc::class::ptr(class) as Id, "dictionary")
        }
        false => binder.options,
    };
    let keys = [
        string("NSObservedObject"),
        string("NSObservedKeyPath"),
        string("NSOptions"),
    ];
    let values = [binder.observed, binder.key_path, options];
    let class = objc::class::lookup(c"NSDictionary").expect("NSDictionary");
    // SAFETY: `id (Class, SEL, const id *, const id *, NSUInteger)`.
    let info = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, *const Id, *const Id, usize) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            objc::class::ptr(class) as Id,
            objc::sel::get("dictionaryWithObjects:forKeys:count:"),
            values.as_ptr(),
            keys.as_ptr(),
            3,
        )
    };
    for key in keys {
        release_string(key);
    }
    info
}

unsafe extern "C" fn exposed_bindings(_receiver: Id, _sel: Sel) -> Id {
    let class = objc::class::lookup(c"NSArray").expect("NSArray");
    send_id(objc::class::ptr(class) as Id, "array")
}

unsafe extern "C" fn expose_binding(_class: Id, _sel: Sel, _name: Id) {}

/// A control the user changed: its `value` binding, if any, takes the new
/// value back to the observed object.
pub(crate) fn control_changed(control: Id) {
    let Some(binder) = binder_of(control, "value") else {
        return;
    };
    let mut value = match is_kind(control, c"NSButton") {
        true => {
            // SAFETY: `NSInteger (id, SEL)` and `id (Class, SEL, NSInteger)`.
            unsafe {
                let state: unsafe extern "C" fn(Id, Sel) -> isize =
                    std::mem::transmute(objc::msg::entry());
                let number: unsafe extern "C" fn(Id, Sel, isize) -> Id =
                    std::mem::transmute(objc::msg::entry());
                let class = objc::class::lookup(c"NSNumber").expect("NSNumber");
                number(
                    objc::class::ptr(class) as Id,
                    objc::sel::get("numberWithInteger:"),
                    state(control, objc::sel::get("state")),
                )
            }
        }
        false => send_id(control, "objectValue"),
    };
    let transformer = transformer(binder);
    if !transformer.is_null() {
        value = send_id_id(transformer, "reverseTransformedValue:", value);
    }
    // SAFETY: `void (id, SEL, id, NSString *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) = std::mem::transmute(objc::msg::entry());
        f(
            binder.observed,
            objc::sel::get("setValue:forKeyPath:"),
            value,
            binder.key_path,
        );
    }
}

// NSUserDefaultsController.

pub(crate) const DEFAULTS_CONTROLLER_INSTANCE_SIZE: usize = size_of::<DefaultsController>();

#[repr(C)]
struct DefaultsController {
    isa: *const c_void,
    defaults: Id,
    initial_values: Id,
    values: Id,
    applies_immediately: u8,
}

#[repr(C)]
struct Values {
    isa: *const c_void,
    /// The controller, which owns this proxy; not retained.
    controller: Id,
}

fn controller(values: Id) -> &'static DefaultsController {
    // SAFETY: a values proxy this module made, whose controller outlives it.
    unsafe { &*((*(values as *const Values)).controller as *const DefaultsController) }
}

/// `-[values valueForKey:]`: the defaults' value, or the initial value.
unsafe extern "C" fn values_value_for_key(receiver: Id, _sel: Sel, key: Id) -> Id {
    let controller = controller(receiver);
    let value = send_id_id(controller.defaults, "objectForKey:", key);
    if value.is_null() && !controller.initial_values.is_null() {
        return send_id_id(controller.initial_values, "objectForKey:", key);
    }
    value
}

/// `-[values setValue:forKey:]`: a write to the defaults, which tells the
/// proxy's observers through the defaults' mirror.
unsafe extern "C" fn values_set_value_for_key(receiver: Id, _sel: Sel, value: Id, key: Id) {
    let controller = controller(receiver);
    // SAFETY: `void (id, SEL, id, NSString *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) = std::mem::transmute(objc::msg::entry());
        f(
            controller.defaults,
            objc::sel::get("setObject:forKey:"),
            value,
            key,
        );
    }
}

unsafe extern "C" fn controller_init_with_defaults(
    receiver: Id,
    _sel: Sel,
    defaults: Id,
    initial_values: Id,
) -> Id {
    if receiver.is_null() {
        return receiver;
    }
    let defaults = match defaults.is_null() {
        true => {
            let class = objc::class::lookup(c"NSUserDefaults").expect("NSUserDefaults");
            send_id(objc::class::ptr(class) as Id, "standardUserDefaults")
        }
        false => defaults,
    };
    let values_class = objc::class::lookup(c"_NSUserDefaultsControllerValues").expect("installed");
    let values = objc::object::create_instance(values_class, 0);
    // SAFETY: fresh instances of this module's classes.
    unsafe {
        (*(values as *mut Values)).controller = receiver;
        let controller = &mut *(receiver as *mut DefaultsController);
        controller.defaults = objc::arc::retain(defaults);
        controller.initial_values = send_id(initial_values, "copy");
        controller.values = values;
        controller.applies_immediately = 1;
    }
    let standard = {
        let class = objc::class::lookup(c"NSUserDefaults").expect("NSUserDefaults");
        send_id(objc::class::ptr(class) as Id, "standardUserDefaults")
    };
    if defaults == standard {
        foundation::defaults::mirror_changes_to(values);
    }
    receiver
}

unsafe extern "C" fn controller_init(receiver: Id, sel: Sel) -> Id {
    // SAFETY: as `-initWithDefaults:initialValues:`.
    unsafe {
        controller_init_with_defaults(receiver, sel, std::ptr::null_mut(), std::ptr::null_mut())
    }
}

unsafe extern "C" fn shared_controller(class: Id, _sel: Sel) -> Id {
    static SHARED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *SHARED.get_or_init(|| {
        let controller = send_id(send_id(class, "alloc"), "init");
        objc::arc::make_immortal(controller);
        controller as usize
    }) as Id
}

unsafe extern "C" fn controller_values(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a controller this module made.
    unsafe { (*(receiver as *const DefaultsController)).values }
}

unsafe extern "C" fn controller_defaults(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a controller this module made.
    unsafe { (*(receiver as *const DefaultsController)).defaults }
}

unsafe extern "C" fn controller_initial_values(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a controller this module made.
    unsafe { (*(receiver as *const DefaultsController)).initial_values }
}

unsafe extern "C" fn controller_set_initial_values(receiver: Id, _sel: Sel, values: Id) {
    // SAFETY: a controller this module made.
    let controller = unsafe { &mut *(receiver as *mut DefaultsController) };
    let old = std::mem::replace(&mut controller.initial_values, send_id(values, "copy"));
    objc::arc::release(old);
}

unsafe extern "C" fn controller_applies_immediately(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a controller this module made.
    unsafe { (*(receiver as *const DefaultsController)).applies_immediately }
}

/// FIXME (ACCURACY): writes always apply immediately; this is only recorded.
unsafe extern "C" fn controller_set_applies_immediately(receiver: Id, _sel: Sel, flag: u8) {
    // SAFETY: a controller this module made.
    unsafe { (*(receiver as *mut DefaultsController)).applies_immediately = flag };
}

unsafe extern "C" fn controller_has_unapplied_changes(_receiver: Id, _sel: Sel) -> u8 {
    0
}

unsafe extern "C" fn controller_action(_receiver: Id, _sel: Sel, _sender: Id) {}

pub(crate) fn install() {
    let nsobject = objc::class::lookup(c"NSObject").expect("NSObject");
    methods!(nsobject, {
        "bind:toObject:withKeyPath:options:" => bind, c"v48@0:8@16@24@32@40";
        "unbind:" => unbind, c"v24@0:8@16";
        "infoForBinding:" => info_for_binding, c"@24@0:8@16";
        "exposedBindings" => exposed_bindings, c"@16@0:8";
    });
    class_methods!(nsobject, {
        "exposeBinding:" => expose_binding, c"v24@0:8@16";
    });

    let binder =
        corefoundation::base::register(c"_RosemaryBinder", None, size_of::<Binder>(), false);
    methods!(binder, {
        "observeValueForKeyPath:ofObject:change:context:" => binder_observe,
        c"v48@0:8@16@24@32^v40";
        "dealloc" => binder_dealloc, c"v16@0:8";
    });

    let controller = corefoundation::base::abstract_class(c"NSController", None);
    let defaults_controller = corefoundation::base::register(
        c"NSUserDefaultsController",
        Some(controller),
        size_of::<DefaultsController>(),
        false,
    );
    methods!(defaults_controller, {
        "init" => controller_init, c"@16@0:8";
        "initWithDefaults:initialValues:" => controller_init_with_defaults, c"@32@0:8@16@24";
        "values" => controller_values, c"@16@0:8";
        "defaults" => controller_defaults, c"@16@0:8";
        "initialValues" => controller_initial_values, c"@16@0:8";
        "setInitialValues:" => controller_set_initial_values, c"v24@0:8@16";
        "appliesImmediately" => controller_applies_immediately, c"c16@0:8";
        "setAppliesImmediately:" => controller_set_applies_immediately, c"v20@0:8c16";
        "hasUnappliedChanges" => controller_has_unapplied_changes, c"c16@0:8";
        "save:" => controller_action, c"v24@0:8@16";
        "revert:" => controller_action, c"v24@0:8@16";
        "revertToInitialValues:" => controller_action, c"v24@0:8@16";
    });
    class_methods!(defaults_controller, {
        "sharedUserDefaultsController" => shared_controller, c"@16@0:8";
    });

    let values = corefoundation::base::register(
        c"_NSUserDefaultsControllerValues",
        None,
        size_of::<Values>(),
        false,
    );
    methods!(values, {
        "valueForKey:" => values_value_for_key, c"@24@0:8@16";
        "setValue:forKey:" => values_set_value_for_key, c"v32@0:8@16@24";
    });
}
