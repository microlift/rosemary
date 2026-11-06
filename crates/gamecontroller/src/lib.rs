use std::ffi::c_void;
use std::sync::Mutex;

use corefoundation::callbacks::Policy;
use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

pub const DID_CONNECT: &str = "GCControllerDidConnectNotification";
pub const DID_DISCONNECT: &str = "GCControllerDidDisconnectNotification";
pub const MOUSE_DID_CONNECT: &str = "GCMouseDidConnectNotification";
pub const MOUSE_DID_DISCONNECT: &str = "GCMouseDidDisconnectNotification";

#[repr(C)]
struct Controller {
    isa: *const Class,
    id: sdl3::JoystickId,
    gamepad: usize,
    player_index: isize,
    extended: Id,
    vendor_name: Id,
    product_category: Id,
    motion: Id,
    light: Id,
    haptics: Id,
    battery: Id,
    paused_handler: usize,
    paused_last: u8,
}

#[derive(Clone, Copy, Debug)]
enum Source {
    Axis(i32, bool),
    Trigger(i32),
    Button(i32),
    DpadX,
    DpadY,
    DirectionButton(DirectionKind, bool, bool),
}

#[repr(C)]
struct Input {
    isa: *const Class,
    controller: *mut Controller,
    source: Source,
    last: f32,
    handler: usize,
    pressed_handler: usize,
}

#[derive(Clone, Copy, Debug)]
enum DirectionKind {
    LeftStick,
    RightStick,
    Dpad,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Direction {
    isa: *const Class,
    controller: *mut Controller,
    kind: DirectionKind,
    x_axis: Id,
    y_axis: Id,
    up: Id,
    down: Id,
    left: Id,
    right: Id,
    last_x: f32,
    last_y: f32,
    handler: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ExtendedGamepad {
    isa: *const Class,
    controller: *mut Controller,
    handler: usize,
    button_a: Id,
    button_b: Id,
    button_x: Id,
    button_y: Id,
    left_shoulder: Id,
    right_shoulder: Id,
    left_trigger: Id,
    right_trigger: Id,
    dpad: Id,
    left_thumbstick: Id,
    right_thumbstick: Id,
    left_thumbstick_button: Id,
    right_thumbstick_button: Id,
    button_menu: Id,
    button_options: Id,
    elements: Id,
    buttons: Id,
}

#[repr(C)]
struct Capability {
    isa: *const Class,
}

#[derive(Clone, Copy)]
struct Record {
    id: sdl3::JoystickId,
    object: usize,
}

static CONTROLLERS: Mutex<Vec<Record>> = Mutex::new(Vec::new());

fn trace_controller(message: impl std::fmt::Display) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ENABLED.get_or_init(|| std::env::var_os("ROSEMARY_TRACE_GAMECONTROLLER").is_some()) {
        eprintln!("rosemary: GameController: {message}");
    }
}

fn sdl_ready() -> bool {
    static READY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *READY.get_or_init(|| match sdl3::init_controllers() {
        Ok(()) => true,
        Err(reason) => {
            eprintln!("rosemary: GameController: {reason}");
            false
        }
    })
}

fn register(
    name: &'static std::ffi::CStr,
    superclass: Option<&'static Class>,
    size: usize,
) -> &'static Class {
    corefoundation::base::register(name, superclass, size, false)
}

fn controller_class() -> &'static Class {
    objc::class::lookup(c"GCController").expect("GameController is bootstrapped")
}

fn input_class() -> &'static Class {
    objc::class::lookup(c"GCControllerAxisInput").expect("GameController is bootstrapped")
}

fn button_class() -> &'static Class {
    objc::class::lookup(c"GCControllerButtonInput").expect("GameController is bootstrapped")
}

fn direction_class() -> &'static Class {
    objc::class::lookup(c"GCControllerDirectionPad").expect("GameController is bootstrapped")
}

fn extended_class() -> &'static Class {
    objc::class::lookup(c"GCExtendedGamepad").expect("GameController is bootstrapped")
}

fn capability(name: &std::ffi::CStr) -> Id {
    let Some(class) = objc::class::lookup(name) else {
        return std::ptr::null_mut();
    };
    // SAFETY: all capability classes use the `Capability` layout.
    unsafe { allocate::<Capability>(class) as Id }
}

unsafe fn allocate<T>(class: &'static Class) -> *mut T {
    // SAFETY: every caller supplies the class registered for `T` below.
    unsafe { corefoundation::base::allocate(class) }
}

fn make_input(controller: *mut Controller, class: &'static Class, source: Source) -> Id {
    // SAFETY: `Input` is the registered layout for both input classes.
    let input = unsafe { allocate::<Input>(class) };
    if input.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a fresh instance whose fields after `isa` are ours.
    unsafe {
        (*input).controller = controller;
        (*input).source = source;
        (*input).last = read_source(controller, source);
        (*input).handler = 0;
        (*input).pressed_handler = 0;
    }
    input as Id
}

fn make_direction(controller: *mut Controller, kind: DirectionKind) -> Id {
    // SAFETY: `Direction` is the registered layout.
    let direction = unsafe { allocate::<Direction>(direction_class()) };
    if direction.is_null() {
        return std::ptr::null_mut();
    }
    let (x_source, y_source) = match kind {
        DirectionKind::LeftStick => (
            Source::Axis(sdl3::gamepad_axis::LEFT_X, false),
            Source::Axis(sdl3::gamepad_axis::LEFT_Y, true),
        ),
        DirectionKind::RightStick => (
            Source::Axis(sdl3::gamepad_axis::RIGHT_X, false),
            Source::Axis(sdl3::gamepad_axis::RIGHT_Y, true),
        ),
        DirectionKind::Dpad => (Source::DpadX, Source::DpadY),
    };
    let (x_axis, y_axis) = (
        make_input(controller, input_class(), x_source),
        make_input(controller, input_class(), y_source),
    );
    let (up, down, left, right) = match kind {
        DirectionKind::Dpad => (
            make_input(
                controller,
                button_class(),
                Source::Button(sdl3::gamepad_button::DPAD_UP),
            ),
            make_input(
                controller,
                button_class(),
                Source::Button(sdl3::gamepad_button::DPAD_DOWN),
            ),
            make_input(
                controller,
                button_class(),
                Source::Button(sdl3::gamepad_button::DPAD_LEFT),
            ),
            make_input(
                controller,
                button_class(),
                Source::Button(sdl3::gamepad_button::DPAD_RIGHT),
            ),
        ),
        _ => (
            make_input(
                controller,
                button_class(),
                Source::DirectionButton(kind, false, true),
            ),
            make_input(
                controller,
                button_class(),
                Source::DirectionButton(kind, false, false),
            ),
            make_input(
                controller,
                button_class(),
                Source::DirectionButton(kind, true, false),
            ),
            make_input(
                controller,
                button_class(),
                Source::DirectionButton(kind, true, true),
            ),
        ),
    };
    let (x, y) = read_direction(controller, kind);
    // SAFETY: a fresh instance.
    unsafe {
        (*direction).controller = controller;
        (*direction).kind = kind;
        (*direction).x_axis = x_axis;
        (*direction).y_axis = y_axis;
        (*direction).up = up;
        (*direction).down = down;
        (*direction).left = left;
        (*direction).right = right;
        (*direction).last_x = x;
        (*direction).last_y = y;
        (*direction).handler = 0;
    }
    direction as Id
}

fn make_extended(controller: *mut Controller, class: &'static Class) -> Id {
    // SAFETY: `ExtendedGamepad` is the registered layout.
    let profile = unsafe { allocate::<ExtendedGamepad>(class) };
    if profile.is_null() {
        return std::ptr::null_mut();
    }
    let button = |index| make_input(controller, button_class(), Source::Button(index));
    // SAFETY: a fresh instance.
    unsafe {
        (*profile).controller = controller;
        (*profile).handler = 0;
        (*profile).button_a = button(sdl3::gamepad_button::SOUTH);
        (*profile).button_b = button(sdl3::gamepad_button::EAST);
        (*profile).button_x = button(sdl3::gamepad_button::WEST);
        (*profile).button_y = button(sdl3::gamepad_button::NORTH);
        (*profile).left_shoulder = button(sdl3::gamepad_button::LEFT_SHOULDER);
        (*profile).right_shoulder = button(sdl3::gamepad_button::RIGHT_SHOULDER);
        (*profile).left_trigger = make_input(
            controller,
            button_class(),
            Source::Trigger(sdl3::gamepad_axis::LEFT_TRIGGER),
        );
        (*profile).right_trigger = make_input(
            controller,
            button_class(),
            Source::Trigger(sdl3::gamepad_axis::RIGHT_TRIGGER),
        );
        (*profile).dpad = make_direction(controller, DirectionKind::Dpad);
        (*profile).left_thumbstick = make_direction(controller, DirectionKind::LeftStick);
        (*profile).right_thumbstick = make_direction(controller, DirectionKind::RightStick);
        (*profile).left_thumbstick_button = button(sdl3::gamepad_button::LEFT_STICK);
        (*profile).right_thumbstick_button = button(sdl3::gamepad_button::RIGHT_STICK);
        (*profile).button_menu = button(sdl3::gamepad_button::START);
        (*profile).button_options = button(sdl3::gamepad_button::BACK);
        let p = &*profile;
        let named = [
            ("Button A", p.button_a),
            ("Button B", p.button_b),
            ("Button X", p.button_x),
            ("Button Y", p.button_y),
            ("Left Shoulder", p.left_shoulder),
            ("Right Shoulder", p.right_shoulder),
            ("Left Trigger", p.left_trigger),
            ("Right Trigger", p.right_trigger),
            ("Left Thumbstick Button", p.left_thumbstick_button),
            ("Right Thumbstick Button", p.right_thumbstick_button),
            ("Button Menu", p.button_menu),
            ("Button Options", p.button_options),
            ("Direction Pad Up", (*(p.dpad as *const Direction)).up),
            ("Direction Pad Down", (*(p.dpad as *const Direction)).down),
            ("Direction Pad Left", (*(p.dpad as *const Direction)).left),
            ("Direction Pad Right", (*(p.dpad as *const Direction)).right),
        ];
        let axes = [
            (
                "Left Thumbstick X Axis",
                (*(p.left_thumbstick as *const Direction)).x_axis,
            ),
            (
                "Left Thumbstick Y Axis",
                (*(p.left_thumbstick as *const Direction)).y_axis,
            ),
            (
                "Right Thumbstick X Axis",
                (*(p.right_thumbstick as *const Direction)).x_axis,
            ),
            (
                "Right Thumbstick Y Axis",
                (*(p.right_thumbstick as *const Direction)).y_axis,
            ),
            (
                "Direction Pad X Axis",
                (*(p.dpad as *const Direction)).x_axis,
            ),
            (
                "Direction Pad Y Axis",
                (*(p.dpad as *const Direction)).y_axis,
            ),
        ];
        let make_dictionary = |entries: &[(&str, Id)]| {
            let keys: Vec<Id> = entries
                .iter()
                .map(|(name, _)| foundation::string::from_str(name))
                .collect();
            let pairs: Vec<_> = keys
                .iter()
                .zip(entries)
                .map(|(key, (_, value))| (*key as _, *value as _))
                .collect();
            let dictionary = corefoundation::collections::new_dictionary(
                &pairs,
                Policy::from_key_callbacks(),
                Policy::from_type_callbacks(),
                false,
            );
            for key in keys {
                corefoundation::api::CFRelease(key as _);
            }
            dictionary as Id
        };
        let mut elements = named.to_vec();
        elements.extend_from_slice(&axes);
        (*profile).buttons = make_dictionary(&named);
        (*profile).elements = make_dictionary(&elements);
    }
    profile as Id
}

fn make_controller(id: sdl3::JoystickId, gamepad: sdl3::Gamepad) -> Id {
    // SAFETY: `Controller` is the registered layout.
    let controller = unsafe { allocate::<Controller>(controller_class()) };
    if controller.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `gamepad` was just opened and remains live in the controller.
    let name = unsafe { sdl3::gamepad_name(gamepad) };
    let properties = unsafe { sdl3::gamepad_properties(gamepad) };
    let vendor_name = foundation::string::from_str(&name);
    // SAFETY: the same live gamepad handle used for its name and properties.
    let vendor = unsafe { sdl3::gamepad_vendor(gamepad) };
    let is_xbox =
        vendor == sdl3::usb_vendor::MICROSOFT || name.to_ascii_lowercase().contains("xbox");
    trace_controller(format_args!(
        "connected id={id} name={name:?} vendor={vendor:04x} xbox={is_xbox}"
    ));
    let profile_class = if is_xbox {
        objc::class::lookup(c"GCXboxGamepad").expect("GameController is bootstrapped")
    } else {
        extended_class()
    };
    // SAFETY: a fresh instance.
    unsafe {
        (*controller).id = id;
        (*controller).gamepad = gamepad as usize;
        (*controller).player_index = -1;
        (*controller).extended = make_extended(controller, profile_class);
        (*controller).vendor_name = vendor_name;
        (*controller).product_category =
            foundation::string::from_str(if is_xbox { "Xbox One" } else { &name });
        (*controller).motion = std::ptr::null_mut();
        (*controller).light = if properties.mono_led || properties.rgb_led || properties.player_led
        {
            capability(c"GCDeviceLight")
        } else {
            std::ptr::null_mut()
        };
        (*controller).haptics = if properties.rumble || properties.trigger_rumble {
            capability(c"GCDeviceHaptics")
        } else {
            std::ptr::null_mut()
        };
        (*controller).battery = std::ptr::null_mut();
        (*controller).paused_handler = 0;
        (*controller).paused_last =
            sdl3::gamepad_button(gamepad, sdl3::gamepad_button::START) as u8;
    }
    controller as Id
}

fn gamepad(controller: *mut Controller) -> Option<sdl3::Gamepad> {
    // SAFETY: all callers pass a live controller.
    let raw = unsafe { (*controller).gamepad };
    (raw != 0).then_some(raw as sdl3::Gamepad)
}

fn read_source(controller: *mut Controller, source: Source) -> f32 {
    let Some(gamepad) = gamepad(controller) else {
        return 0.0;
    };
    // SAFETY: the controller owns this open handle.
    unsafe {
        match source {
            Source::Axis(axis, invert) => {
                let raw = sdl3::gamepad_axis(gamepad, axis);
                let value = if raw < 0 {
                    raw as f32 / 32768.0
                } else {
                    raw as f32 / 32767.0
                };
                if invert { -value } else { value }
            }
            Source::Trigger(axis) => {
                (sdl3::gamepad_axis(gamepad, axis).max(0) as f32 / 32767.0).min(1.0)
            }
            Source::Button(button) => sdl3::gamepad_button(gamepad, button) as u8 as f32,
            Source::DpadX => {
                sdl3::gamepad_button(gamepad, sdl3::gamepad_button::DPAD_RIGHT) as u8 as f32
                    - sdl3::gamepad_button(gamepad, sdl3::gamepad_button::DPAD_LEFT) as u8 as f32
            }
            Source::DirectionButton(kind, x_axis, positive) => {
                let (x, y) = read_direction(controller, kind);
                let value = if x_axis { x } else { y };
                if positive {
                    value.max(0.0)
                } else {
                    (-value).max(0.0)
                }
            }
            Source::DpadY => {
                sdl3::gamepad_button(gamepad, sdl3::gamepad_button::DPAD_UP) as u8 as f32
                    - sdl3::gamepad_button(gamepad, sdl3::gamepad_button::DPAD_DOWN) as u8 as f32
            }
        }
    }
}

fn read_direction(controller: *mut Controller, kind: DirectionKind) -> (f32, f32) {
    match kind {
        DirectionKind::LeftStick => (
            read_source(controller, Source::Axis(sdl3::gamepad_axis::LEFT_X, false)),
            read_source(controller, Source::Axis(sdl3::gamepad_axis::LEFT_Y, true)),
        ),
        DirectionKind::RightStick => (
            read_source(controller, Source::Axis(sdl3::gamepad_axis::RIGHT_X, false)),
            read_source(controller, Source::Axis(sdl3::gamepad_axis::RIGHT_Y, true)),
        ),
        DirectionKind::Dpad => (
            read_source(controller, Source::DpadX),
            read_source(controller, Source::DpadY),
        ),
    }
}

fn replace_block(slot: &mut usize, block: Id) {
    let copied = objc::block::copy(block) as usize;
    if *slot != 0 {
        objc::block::release(*slot as Id);
    }
    *slot = copied;
}

unsafe extern "C" fn controllers(_class: Id, _sel: Sel) -> Id {
    // Existing devices do not generate connection events for new observers.
    sync_devices(false);
    let values: Vec<_> = CONTROLLERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|r| r.object as *const c_void)
        .collect();
    trace_controller(format_args!(
        "GCController.controllers -> {} controllers",
        values.len()
    ));
    let array =
        corefoundation::collections::new_array(&values, Policy::from_type_callbacks(), false);
    // SAFETY: a fresh array follows Foundation's convenience-constructor rule.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn supports_hid_device(
    _class: Id,
    _sel: Sel,
    device: corefoundation::base::CFTypeRef,
) -> bool {
    if corefoundation::base::opaque_type_id(device) != Some(iokit::hid::device_type_id()) {
        return false;
    }
    // SAFETY: the type registration above identifies one of IOKit's Device
    // handles, which lives for the lifetime of the process.
    if !sdl_ready() {
        return false;
    }
    sdl3::update_joysticks();
    let id = unsafe { (*(device as *const iokit::hid::Device)).id };
    let supported = sdl3::gamepads().contains(&id);
    trace_controller(format_args!(
        "GCController.supportsHIDDevice id={id} -> {supported}"
    ));
    supported
}

unsafe extern "C" fn extended_gamepad(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a controller.
    unsafe { (*(receiver as *mut Controller)).extended }
}

unsafe extern "C" fn controller_feature(receiver: Id, sel: Sel) -> Id {
    // SAFETY: a controller and a live selector.
    let controller = unsafe { &*(receiver as *const Controller) };
    // SAFETY: Objective-C selectors are process-lifetime C strings.
    match unsafe { std::ffi::CStr::from_ptr(objc::sel::name(sel)) }.to_bytes() {
        b"motion" => controller.motion,
        b"light" => controller.light,
        b"haptics" => controller.haptics,
        b"battery" => controller.battery,
        _ => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn player_index(receiver: Id, _sel: Sel) -> isize {
    // SAFETY: a controller.
    unsafe { (*(receiver as *mut Controller)).player_index }
}

unsafe extern "C" fn set_player_index(receiver: Id, _sel: Sel, value: isize) {
    // SAFETY: a controller.
    unsafe { (*(receiver as *mut Controller)).player_index = value };
}

unsafe extern "C" fn vendor_name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a controller.
    unsafe { (*(receiver as *mut Controller)).vendor_name }
}

unsafe extern "C" fn product_category(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a controller.
    unsafe { (*(receiver as *mut Controller)).product_category }
}

unsafe extern "C" fn physical_input_profile(receiver: Id, _sel: Sel) -> Id {
    // GCExtendedGamepad is a GCPhysicalInputProfile subclass on macOS.
    unsafe { (*(receiver as *mut Controller)).extended }
}

unsafe extern "C" fn profile_dictionary(receiver: Id, sel: Sel) -> Id {
    let profile = unsafe { &*(receiver as *const ExtendedGamepad) };
    let name = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(sel)) }.to_bytes();
    match name {
        b"elements" => profile.elements,
        b"buttons" => profile.buttons,
        _ => std::ptr::null_mut(),
    }
}

static MONITOR_BACKGROUND_EVENTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

unsafe extern "C" fn should_monitor_background_events(_class: Id, _sel: Sel) -> bool {
    MONITOR_BACKGROUND_EVENTS.load(std::sync::atomic::Ordering::Relaxed)
}

unsafe extern "C" fn set_should_monitor_background_events(_class: Id, _sel: Sel, value: bool) {
    MONITOR_BACKGROUND_EVENTS.store(value, std::sync::atomic::Ordering::Relaxed);
}

unsafe extern "C" fn supported_localities(_receiver: Id, _sel: Sel) -> Id {
    let set = corefoundation::collections::new_set(&[], Policy::from_type_callbacks(), false);
    unsafe { corefoundation::api::CFAutorelease(set) as Id }
}

/// There is no CoreHaptics engine to hand out, and `supportedLocalities` is
/// empty, so every locality is unsupported and the documented answer is nil.
unsafe extern "C" fn create_engine_with_locality(_receiver: Id, _sel: Sel, locality: Id) -> Id {
    trace_controller(format_args!(
        "createEngineWithLocality: {locality:p} unsupported"
    ));
    std::ptr::null_mut()
}

unsafe extern "C" fn set_paused_handler(receiver: Id, _sel: Sel, block: Id) {
    // SAFETY: a controller.
    replace_block(
        unsafe { &mut (*(receiver as *mut Controller)).paused_handler },
        block,
    );
}

unsafe extern "C" fn input_value(receiver: Id, _sel: Sel) -> f32 {
    sdl3::update_joysticks();
    // SAFETY: an axis or button input, both with `Input` layout.
    let input = unsafe { &*(receiver as *const Input) };
    read_source(input.controller, input.source)
}

unsafe extern "C" fn input_pressed(receiver: Id, sel: Sel) -> u8 {
    // SAFETY: this receiver is also an input.
    (unsafe { input_value(receiver, sel) } > 0.0) as u8
}

/// Nothing here is claimed by the system (Home, screenshot, etc.), so no
/// button is ever bound to a system gesture and the app always receives it.
unsafe extern "C" fn input_bound_to_system_gesture(_receiver: Id, _sel: Sel) -> bool {
    false
}

unsafe extern "C" fn input_is_analog(receiver: Id, _sel: Sel) -> bool {
    // Axes and triggers have a continuous value. SDL's standardized digital
    // buttons do not, even though GCControllerButtonInput inherits the axis API.
    matches!(
        unsafe { (*(receiver as *const Input)).source },
        Source::Axis(..) | Source::Trigger(_) | Source::DirectionButton(..)
    )
}

unsafe extern "C" fn set_input_handler(receiver: Id, _sel: Sel, block: Id) {
    // SAFETY: an axis or button input.
    let input = unsafe { &mut *(receiver as *mut Input) };
    trace_controller(format_args!(
        "setValueChangedHandler source={:?} installed={}",
        input.source,
        !block.is_null()
    ));
    replace_block(&mut input.handler, block);
}

unsafe extern "C" fn direction_element(receiver: Id, sel: Sel) -> Id {
    // SAFETY: a direction pad.
    let direction = unsafe { &*(receiver as *const Direction) };
    // SAFETY: a live selector has a process-lifetime NUL-terminated name.
    let name = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(sel)) }.to_bytes();
    match name {
        b"xAxis" => direction.x_axis,
        b"yAxis" => direction.y_axis,
        b"up" => direction.up,
        b"down" => direction.down,
        b"left" => direction.left,
        b"right" => direction.right,
        _ => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn set_direction_handler(receiver: Id, _sel: Sel, block: Id) {
    // SAFETY: a direction pad.
    let direction = unsafe { &mut *(receiver as *mut Direction) };
    trace_controller(format_args!(
        "setValueChangedHandler direction={:?} installed={}",
        direction.kind,
        !block.is_null()
    ));
    replace_block(&mut direction.handler, block);
}

unsafe extern "C" fn extended_element(receiver: Id, sel: Sel) -> Id {
    // SAFETY: an extended gamepad profile.
    let profile = unsafe { &*(receiver as *const ExtendedGamepad) };
    // SAFETY: a live selector has a process-lifetime NUL-terminated name.
    let name = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(sel)) }.to_bytes();
    match name {
        b"buttonA" => profile.button_a,
        b"buttonB" => profile.button_b,
        b"buttonX" => profile.button_x,
        b"buttonY" => profile.button_y,
        b"leftShoulder" => profile.left_shoulder,
        b"rightShoulder" => profile.right_shoulder,
        b"leftTrigger" => profile.left_trigger,
        b"rightTrigger" => profile.right_trigger,
        b"dpad" => profile.dpad,
        b"leftThumbstick" => profile.left_thumbstick,
        b"rightThumbstick" => profile.right_thumbstick,
        b"leftThumbstickButton" => profile.left_thumbstick_button,
        b"rightThumbstickButton" => profile.right_thumbstick_button,
        b"buttonMenu" => profile.button_menu,
        b"buttonOptions" => profile.button_options,
        _ => std::ptr::null_mut(),
    }
}

fn pressed(value: f32) -> bool {
    value > 0.0
}

fn call_input(input: Id) -> bool {
    if input.is_null() {
        return false;
    }
    let (source, controller, old) = unsafe {
        let i = &*(input as *const Input);
        (i.source, i.controller, i.last)
    };
    let value = read_source(controller, source);
    if value == old {
        return false;
    }
    unsafe {
        (*(input as *mut Input)).last = value;
    }
    let block = objc::block::copy(unsafe { (*(input as *const Input)).handler as Id });
    unsafe {
        if let Some(invoke) = objc::block::invoke(block) {
            if matches!(source, Source::Axis(..) | Source::DpadX | Source::DpadY) {
                let f: unsafe extern "C" fn(Id, Id, f32) = std::mem::transmute(invoke);
                f(block, input, value);
            } else {
                let f: unsafe extern "C" fn(Id, Id, f32, u8) = std::mem::transmute(invoke);
                f(block, input, value, pressed(value) as u8);
            }
        }
    }
    objc::block::release(block);
    if pressed(value) != pressed(old) {
        let block = objc::block::copy(unsafe { (*(input as *const Input)).pressed_handler as Id });
        unsafe {
            if let Some(invoke) = objc::block::invoke(block) {
                let f: unsafe extern "C" fn(Id, Id, f32, u8) = std::mem::transmute(invoke);
                f(block, input, value, pressed(value) as u8);
            }
        }
        objc::block::release(block);
    }
    true
}

fn call_direction(direction: Id) -> bool {
    if direction.is_null() {
        return false;
    }
    let d = unsafe { *(direction as *const Direction) };
    let (x, y) = read_direction(d.controller, d.kind);
    if x == d.last_x && y == d.last_y {
        return false;
    }
    unsafe {
        (*(direction as *mut Direction)).last_x = x;
        (*(direction as *mut Direction)).last_y = y;
    }
    for input in [d.x_axis, d.y_axis, d.up, d.down, d.left, d.right] {
        call_input(input);
    }
    let block = objc::block::copy(unsafe { (*(direction as *const Direction)).handler as Id });
    unsafe {
        if let Some(invoke) = objc::block::invoke(block) {
            let f: unsafe extern "C" fn(Id, Id, f32, f32) = std::mem::transmute(invoke);
            f(block, direction, x, y);
        }
    }
    objc::block::release(block);
    true
}

fn call_profile(profile: Id, element: Id) {
    let block = objc::block::copy(unsafe { (*(profile as *const ExtendedGamepad)).handler as Id });
    unsafe {
        if let Some(invoke) = objc::block::invoke(block) {
            let f: unsafe extern "C" fn(Id, Id, Id) = std::mem::transmute(invoke);
            f(block, profile, element);
        }
    }
    objc::block::release(block);
}

fn poll_controller(controller: Id) {
    let profile_id = unsafe { (*(controller as *const Controller)).extended };
    if profile_id.is_null() {
        return;
    }
    let profile = unsafe { *(profile_id as *const ExtendedGamepad) };
    for input in [
        profile.button_a,
        profile.button_b,
        profile.button_x,
        profile.button_y,
        profile.left_shoulder,
        profile.right_shoulder,
        profile.left_trigger,
        profile.right_trigger,
        profile.left_thumbstick_button,
        profile.right_thumbstick_button,
        profile.button_menu,
        profile.button_options,
    ] {
        if call_input(input) {
            call_profile(profile_id, input);
        }
    }
    for direction in [
        profile.dpad,
        profile.left_thumbstick,
        profile.right_thumbstick,
    ] {
        if call_direction(direction) {
            call_profile(profile_id, direction);
        }
    }
    let now = gamepad(controller as *mut Controller)
        .is_some_and(|g| unsafe { sdl3::gamepad_button(g, sdl3::gamepad_button::START) });
    let old = unsafe { (*(controller as *const Controller)).paused_last };
    unsafe {
        (*(controller as *mut Controller)).paused_last = now as u8;
    }
    if now && old == 0 {
        let block =
            objc::block::copy(unsafe { (*(controller as *const Controller)).paused_handler as Id });
        unsafe {
            if let Some(invoke) = objc::block::invoke(block) {
                let f: unsafe extern "C" fn(Id, Id) = std::mem::transmute(invoke);
                f(block, controller);
            }
        }
        objc::block::release(block);
    }
}

unsafe extern "C" fn get_input_handler(receiver: Id, _sel: Sel) -> Id {
    unsafe { (*(receiver as *const Input)).handler as Id }
}
unsafe extern "C" fn get_pressed_handler(receiver: Id, _sel: Sel) -> Id {
    unsafe { (*(receiver as *const Input)).pressed_handler as Id }
}
unsafe extern "C" fn set_pressed_handler(receiver: Id, _sel: Sel, block: Id) {
    replace_block(
        unsafe { &mut (*(receiver as *mut Input)).pressed_handler },
        block,
    );
}
unsafe extern "C" fn get_direction_handler(receiver: Id, _sel: Sel) -> Id {
    unsafe { (*(receiver as *const Direction)).handler as Id }
}
unsafe extern "C" fn get_profile_handler(receiver: Id, _sel: Sel) -> Id {
    unsafe { (*(receiver as *const ExtendedGamepad)).handler as Id }
}
unsafe extern "C" fn set_profile_handler(receiver: Id, _sel: Sel, block: Id) {
    replace_block(
        unsafe { &mut (*(receiver as *mut ExtendedGamepad)).handler },
        block,
    );
}
unsafe extern "C" fn get_paused_handler(receiver: Id, _sel: Sel) -> Id {
    unsafe { (*(receiver as *const Controller)).paused_handler as Id }
}
unsafe extern "C" fn profile_controller(receiver: Id, _sel: Sel) -> Id {
    unsafe { (*(receiver as *const ExtendedGamepad)).controller as Id }
}

fn sync_devices(post_notifications: bool) {
    if !sdl_ready() {
        return;
    }
    sdl3::update_joysticks();
    let attached = sdl3::gamepads();
    let mut connected = Vec::new();
    let mut disconnected = Vec::new();
    {
        let mut current = CONTROLLERS.lock().unwrap_or_else(|e| e.into_inner());
        let mut i = 0;
        while i < current.len() {
            if attached.contains(&current[i].id) {
                i += 1;
                continue;
            }
            let gone = current.remove(i);
            let object = gone.object as Id;
            // SAFETY: the record owns an open handle.
            unsafe {
                let controller = &mut *(object as *mut Controller);
                if controller.gamepad != 0 {
                    sdl3::close_gamepad(controller.gamepad as sdl3::Gamepad);
                    controller.gamepad = 0;
                }
            }
            disconnected.push(object);
        }
        for id in attached {
            if current.iter().any(|r| r.id == id) {
                continue;
            }
            let Some(gamepad) = sdl3::open_gamepad(id) else {
                continue;
            };
            let object = make_controller(id, gamepad);
            if object.is_null() {
                // SAFETY: ownership was not transferred.
                unsafe { sdl3::close_gamepad(gamepad) };
                continue;
            }
            current.push(Record {
                id,
                object: object as usize,
            });
            connected.push(object);
        }
    }
    if post_notifications {
        for object in disconnected {
            foundation::notification::post(DID_DISCONNECT, object, std::ptr::null_mut());
        }
        for object in connected {
            foundation::notification::post(DID_CONNECT, object, std::ptr::null_mut());
        }
    }
}

/// Refresh attachment and input state. Called by AppKit's event pump.
pub fn pump() {
    static POLLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    use std::sync::atomic::Ordering;
    if POLLING.swap(true, Ordering::AcqRel) {
        return;
    }
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            POLLING.store(false, Ordering::Release);
        }
    }
    let _guard = Guard;
    bootstrap();
    sync_devices(true);
    let controllers: Vec<usize> = CONTROLLERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|r| r.object)
        .collect();
    for controller in controllers {
        poll_controller(controller as Id);
    }
}

/// Install GameController's classes and methods. Idempotent.
pub fn bootstrap() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        foundation::bootstrap();
        let element = register(c"GCControllerElement", None, size_of::<usize>());
        let axis = register(c"GCControllerAxisInput", Some(element), size_of::<Input>());
        let button = register(c"GCControllerButtonInput", Some(axis), size_of::<Input>());
        let direction = register(
            c"GCControllerDirectionPad",
            Some(element),
            size_of::<Direction>(),
        );
        let profile = register(c"GCPhysicalInputProfile", None, size_of::<usize>());
        let extended = register(
            c"GCExtendedGamepad",
            Some(profile),
            size_of::<ExtendedGamepad>(),
        );
        for name in [
            c"GCDualSenseGamepad",
            c"GCDualShockGamepad",
            c"GCXboxGamepad",
        ] {
            register(name, Some(extended), size_of::<ExtendedGamepad>());
        }
        register(c"GCColor", None, size_of::<usize>());
        for name in [
            c"GCMotion",
            c"GCDeviceLight",
            c"GCDeviceHaptics",
            c"GCDeviceBattery",
        ] {
            register(name, None, size_of::<Capability>());
        }
        let controller = register(c"GCController", None, size_of::<Controller>());

        controller.metaclass().replace_method(
            objc::sel::get("controllers"),
            controllers as *const c_void,
            c"@16@0:8",
        );
        controller.metaclass().replace_method(
            objc::sel::get("shouldMonitorBackgroundEvents"),
            should_monitor_background_events as *const c_void,
            c"B16@0:8",
        );
        controller.metaclass().replace_method(
            objc::sel::get("setShouldMonitorBackgroundEvents:"),
            set_should_monitor_background_events as *const c_void,
            c"v20@0:8B16",
        );
        controller.metaclass().replace_method(
            objc::sel::get("supportsHIDDevice:"),
            supports_hid_device as *const c_void,
            c"B24@0:8^{__IOHIDDevice=}16",
        );
        for (name, imp, types) in [
            (
                "extendedGamepad",
                extended_gamepad as *const c_void,
                c"@16@0:8",
            ),
            ("playerIndex", player_index as *const c_void, c"q16@0:8"),
            (
                "setPlayerIndex:",
                set_player_index as *const c_void,
                c"v24@0:8q16",
            ),
            ("vendorName", vendor_name as *const c_void, c"@16@0:8"),
            (
                "productCategory",
                product_category as *const c_void,
                c"@16@0:8",
            ),
            (
                "physicalInputProfile",
                physical_input_profile as *const c_void,
                c"@16@0:8",
            ),
            (
                "setControllerPausedHandler:",
                set_paused_handler as *const c_void,
                c"v24@0:8@?16",
            ),
        ] {
            controller.replace_method(objc::sel::get(name), imp, types);
        }
        controller.replace_method(
            objc::sel::get("controllerPausedHandler"),
            get_paused_handler as *const c_void,
            c"@?16@0:8",
        );
        for (class, get) in [
            (axis, get_input_handler as *const c_void),
            (button, get_input_handler as *const c_void),
            (direction, get_direction_handler as *const c_void),
            (extended, get_profile_handler as *const c_void),
        ] {
            class.replace_method(objc::sel::get("valueChangedHandler"), get, c"@?16@0:8");
        }
        button.replace_method(
            objc::sel::get("pressedChangedHandler"),
            get_pressed_handler as *const c_void,
            c"@?16@0:8",
        );
        button.replace_method(
            objc::sel::get("setPressedChangedHandler:"),
            set_pressed_handler as *const c_void,
            c"v24@0:8@?16",
        );
        extended.replace_method(
            objc::sel::get("setValueChangedHandler:"),
            set_profile_handler as *const c_void,
            c"v24@0:8@?16",
        );
        extended.replace_method(
            objc::sel::get("controller"),
            profile_controller as *const c_void,
            c"@16@0:8",
        );
        for name in ["motion", "light", "haptics", "battery"] {
            controller.replace_method(
                objc::sel::get(name),
                controller_feature as *const c_void,
                c"@16@0:8",
            );
        }
        for class in [axis, button] {
            class.replace_method(
                objc::sel::get("value"),
                input_value as *const c_void,
                c"f16@0:8",
            );
            class.replace_method(
                objc::sel::get("setValueChangedHandler:"),
                set_input_handler as *const c_void,
                c"v24@0:8@?16",
            );
            class.replace_method(
                objc::sel::get("isAnalog"),
                input_is_analog as *const c_void,
                c"B16@0:8",
            );
        }
        button.replace_method(
            objc::sel::get("isPressed"),
            input_pressed as *const c_void,
            c"B16@0:8",
        );
        button.replace_method(
            objc::sel::get("isBoundToSystemGesture"),
            input_bound_to_system_gesture as *const c_void,
            c"B16@0:8",
        );
        for name in ["xAxis", "yAxis", "up", "down", "left", "right"] {
            direction.replace_method(
                objc::sel::get(name),
                direction_element as *const c_void,
                c"@16@0:8",
            );
        }
        direction.replace_method(
            objc::sel::get("setValueChangedHandler:"),
            set_direction_handler as *const c_void,
            c"v24@0:8@?16",
        );
        for name in [
            "buttonA",
            "buttonB",
            "buttonX",
            "buttonY",
            "leftShoulder",
            "rightShoulder",
            "leftTrigger",
            "rightTrigger",
            "dpad",
            "leftThumbstick",
            "rightThumbstick",
            "leftThumbstickButton",
            "rightThumbstickButton",
            "buttonMenu",
            "buttonOptions",
        ] {
            extended.replace_method(
                objc::sel::get(name),
                extended_element as *const c_void,
                c"@16@0:8",
            );
        }
        for name in ["elements", "buttons"] {
            profile.replace_method(
                objc::sel::get(name),
                profile_dictionary as *const c_void,
                c"@16@0:8",
            );
        }
        let haptics = objc::class::lookup(c"GCDeviceHaptics").expect("GCDeviceHaptics");
        haptics.replace_method(
            objc::sel::get("supportedLocalities"),
            supported_localities as *const c_void,
            c"@16@0:8",
        );
        haptics.replace_method(
            objc::sel::get("createEngineWithLocality:"),
            create_engine_with_locality as *const c_void,
            c"@24@0:8@16",
        );
    });
}

/// Objective-C class symbols exported by GameController.framework.
pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    let mut out = Vec::new();
    for name in [
        "GCController",
        "GCControllerElement",
        "GCControllerAxisInput",
        "GCControllerButtonInput",
        "GCControllerDirectionPad",
        "GCPhysicalInputProfile",
        "GCExtendedGamepad",
        "GCDualSenseGamepad",
        "GCDualShockGamepad",
        "GCXboxGamepad",
        "GCColor",
        "GCMotion",
        "GCDeviceLight",
        "GCDeviceHaptics",
        "GCDeviceBattery",
    ] {
        let c = std::ffi::CString::new(name).expect("a class name");
        let class = objc::class::lookup(&c).expect("a registered GameController class");
        out.push((
            format!("_OBJC_CLASS_$_{name}"),
            objc::class::ptr(class) as usize as u64,
        ));
        out.push((
            format!("_OBJC_METACLASS_$_{name}"),
            objc::class::ptr(class.metaclass()) as usize as u64,
        ));
    }
    out.extend(corefoundation::base::string_globals(STRING_GLOBALS));
    out.push((
        "_GCHapticDurationInfinite".to_string(),
        &HAPTIC_DURATION_INFINITE as *const f32 as usize as u64,
    ));
    out
}

/// A `const float`, so the import slot holds this storage's address, not a string.
static HAPTIC_DURATION_INFINITE: f32 = f32::INFINITY;

/// Resolve a GameController data symbol, accepting Mach-O's leading underscore
/// or the bare spelling used by `dlsym`.
pub fn symbol(name: &str) -> Option<u64> {
    let wanted = name.trim_start_matches('_');
    data_symbols()
        .into_iter()
        .find(|(symbol, _)| symbol.trim_start_matches('_') == wanted)
        .map(|(_, address)| address)
}

pub const STRING_GLOBALS: &[(&str, &str)] = &[
    ("_GCControllerDidConnectNotification", DID_CONNECT),
    ("_GCControllerDidDisconnectNotification", DID_DISCONNECT),
    ("_GCMouseDidConnectNotification", MOUSE_DID_CONNECT),
    ("_GCMouseDidDisconnectNotification", MOUSE_DID_DISCONNECT),
    ("_GCInputButtonA", "Button A"),
    ("_GCInputButtonB", "Button B"),
    ("_GCInputButtonX", "Button X"),
    ("_GCInputButtonY", "Button Y"),
    ("_GCInputButtonMenu", "Button Menu"),
    ("_GCInputButtonOptions", "Button Options"),
    ("_GCInputDirectionPad", "Direction Pad"),
    ("_GCInputLeftShoulder", "Left Shoulder"),
    ("_GCInputRightShoulder", "Right Shoulder"),
    ("_GCInputLeftTrigger", "Left Trigger"),
    ("_GCInputRightTrigger", "Right Trigger"),
    ("_GCInputLeftThumbstick", "Left Thumbstick"),
    ("_GCInputRightThumbstick", "Right Thumbstick"),
    ("_GCInputLeftThumbstickButton", "Left Thumbstick Button"),
    ("_GCInputRightThumbstickButton", "Right Thumbstick Button"),
    ("_GCInputDualShockTouchpadButton", "Touchpad Button"),
    ("_GCInputDualShockTouchpadOne", "Touchpad 1"),
    ("_GCInputDualShockTouchpadTwo", "Touchpad 2"),
    ("_GCInputXboxPaddleOne", "Paddle 1"),
    ("_GCInputXboxPaddleTwo", "Paddle 2"),
    ("_GCInputXboxPaddleThree", "Paddle 3"),
    ("_GCInputXboxPaddleFour", "Paddle 4"),
    ("_GCHapticsLocalityDefault", "Default"),
    ("_GCHapticsLocalityAll", "All"),
    ("_GCHapticsLocalityHandles", "Handles"),
    ("_GCHapticsLocalityLeftHandle", "LeftHandle"),
    ("_GCHapticsLocalityRightHandle", "RightHandle"),
    ("_GCHapticsLocalityTriggers", "Triggers"),
    ("_GCHapticsLocalityLeftTrigger", "LeftTrigger"),
    ("_GCHapticsLocalityRightTrigger", "RightTrigger"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_and_selectors_exist() {
        bootstrap();
        let controller = controller_class();
        assert!(
            controller
                .metaclass()
                .method(objc::sel::get("controllers"))
                .is_some()
        );
        assert!(
            controller
                .metaclass()
                .method(objc::sel::get("supportsHIDDevice:"))
                .is_some()
        );
        assert!(
            controller
                .method(objc::sel::get("extendedGamepad"))
                .is_some()
        );
        for selector in ["motion", "light", "haptics", "battery"] {
            assert!(controller.method(objc::sel::get(selector)).is_some());
        }
        let haptics = objc::class::lookup(c"GCDeviceHaptics").expect("GCDeviceHaptics");
        assert!(
            haptics
                .method(objc::sel::get("createEngineWithLocality:"))
                .is_some()
        );
        assert!(
            button_class()
                .method(objc::sel::get("setValueChangedHandler:"))
                .is_some()
        );
        assert!(
            direction_class()
                .method(objc::sel::get("setValueChangedHandler:"))
                .is_some()
        );
    }

    #[test]
    fn every_exported_symbol_is_nonzero_and_stable() {
        assert_eq!(data_symbols(), data_symbols());
        assert!(data_symbols().iter().all(|(_, address)| *address != 0));
        assert_eq!(
            symbol("GCControllerDidConnectNotification"),
            symbol("_GCControllerDidConnectNotification")
        );
    }

    #[derive(Default)]
    struct CallbackEvents {
        axes: Vec<(usize, f32)>,
        buttons: Vec<(usize, f32, u8)>,
        pressed: Vec<(usize, u8)>,
        directions: Vec<(usize, f32, f32)>,
        profiles: Vec<usize>,
    }
    #[repr(C)]
    struct TestDescriptor {
        reserved: usize,
        size: usize,
    }
    #[repr(C)]
    struct TestBlock {
        isa: usize,
        flags: i32,
        reserved: i32,
        invoke: usize,
        descriptor: *const TestDescriptor,
        context: *const Mutex<CallbackEvents>,
    }
    unsafe fn callback_events<'a>(block: Id) -> &'a Mutex<CallbackEvents> {
        unsafe { &*(*(block as *const TestBlock)).context }
    }
    unsafe extern "C" fn axis_changed(block: Id, axis: Id, value: f32) {
        unsafe { callback_events(block) }
            .lock()
            .unwrap()
            .axes
            .push((axis as usize, value));
        // Replacing this block and reentering the pump must not free an
        // executing block or deliver duplicate notifications.
        unsafe {
            set_input_handler(
                axis,
                objc::sel::get("setValueChangedHandler:"),
                std::ptr::null_mut(),
            );
        }
        pump();
    }
    unsafe extern "C" fn button_changed(block: Id, button: Id, value: f32, pressed: u8) {
        unsafe { callback_events(block) }
            .lock()
            .unwrap()
            .buttons
            .push((button as usize, value, pressed));
    }
    unsafe extern "C" fn pressed_changed(block: Id, button: Id, _value: f32, pressed: u8) {
        unsafe { callback_events(block) }
            .lock()
            .unwrap()
            .pressed
            .push((button as usize, pressed));
    }
    unsafe extern "C" fn direction_changed(block: Id, direction: Id, x: f32, y: f32) {
        unsafe { callback_events(block) }
            .lock()
            .unwrap()
            .directions
            .push((direction as usize, x, y));
    }
    unsafe extern "C" fn profile_changed(block: Id, _profile: Id, element: Id) {
        unsafe { callback_events(block) }
            .lock()
            .unwrap()
            .profiles
            .push(element as usize);
    }

    #[test]
    fn a_virtual_sdl_gamepad_populates_the_extended_profile() {
        if !sdl3::available() || !sdl3::virtual_joystick::available() {
            return;
        }
        unsafe extern "C" fn rumble(_data: *mut c_void, _low: u16, _high: u16) -> bool {
            true
        }
        unsafe extern "C" fn set_led(_data: *mut c_void, _r: u8, _g: u8, _b: u8) -> bool {
            true
        }
        let desc = sdl3::virtual_joystick::Desc {
            kind: 1,
            vendor_id: sdl3::usb_vendor::MICROSOFT,
            product_id: sdl3::usb_product::XBOX_SERIES_CONTROLLER,
            naxes: 6,
            nbuttons: 15,
            rumble: Some(rumble),
            set_led: Some(set_led),
            ..Default::default()
        };
        assert!(sdl_ready());
        let id = sdl3::virtual_joystick::attach(&desc).expect("virtual gamepad attachment");
        let hid = iokit::hid::Device::open(id).expect("the virtual HID device opens");
        bootstrap();
        // SAFETY: `hid` is a live IOHIDDeviceRef for this call.
        assert!(unsafe {
            supports_hid_device(
                std::ptr::null_mut(),
                objc::sel::get("supportsHIDDevice:"),
                std::sync::Arc::as_ptr(&hid).cast_mut().cast(),
            )
        });
        sync_devices(false);
        let object = CONTROLLERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|record| record.id == id)
            .map(|record| record.object as Id)
            .expect("the virtual gamepad is a GCController");
        let joystick = sdl3::open(id).expect("the virtual joystick opens");
        // SAFETY: `joystick` remains open through these operations.
        unsafe {
            assert!(sdl3::virtual_joystick::set_button(
                joystick,
                sdl3::gamepad_button::SOUTH,
                true,
            ));
            assert!(sdl3::virtual_joystick::set_axis(
                joystick,
                sdl3::gamepad_axis::LEFT_X,
                16_384,
            ));
            assert!(sdl3::virtual_joystick::set_axis(
                joystick,
                sdl3::gamepad_axis::LEFT_Y,
                -16_384,
            ));
        }
        sdl3::pump();
        // SAFETY: `object` is the live controller just found.
        let profile =
            unsafe { *((*(object as *mut Controller)).extended as *const ExtendedGamepad) };
        // SAFETY: `object` is the controller allocated above.
        let controller = unsafe { &*(object as *const Controller) };
        // SAFETY: the extended profile is a live Objective-C object.
        assert_eq!(
            unsafe { objc::object::class_of(controller.extended) }
                .expect("the profile has a class")
                .name_str(),
            "GCXboxGamepad"
        );
        assert!(!controller.light.is_null());
        assert!(!controller.haptics.is_null());
        assert!(controller.motion.is_null());
        assert!(controller.battery.is_null());
        // SAFETY: the profile owns inputs with the layouts being read.
        unsafe {
            assert_eq!(input_value(profile.button_a, objc::sel::get("value")), 1.0);
            let stick = &*(profile.left_thumbstick as *const Direction);
            let (x, y) = read_direction(stick.controller, stick.kind);
            assert!((x - 0.5).abs() < 0.001);
            assert!((y - 0.5).abs() < 0.001);
        }
        pump();
        let profile_id = controller.extended;
        let direction = unsafe { *(profile.left_thumbstick as *const Direction) };
        let events = Mutex::new(CallbackEvents::default());
        let descriptor = TestDescriptor {
            reserved: 0,
            size: size_of::<TestBlock>(),
        };
        let isa = objc::block::data_symbols()
            .iter()
            .find(|(name, _)| name.ends_with("_NSConcreteStackBlock"))
            .unwrap()
            .1 as usize;
        let block = |invoke| TestBlock {
            isa,
            flags: 0,
            reserved: 0,
            invoke,
            descriptor: &descriptor,
            context: &events,
        };
        let axis_block = block(axis_changed as *const c_void as usize);
        let button_block = block(button_changed as *const c_void as usize);
        let pressed_block = block(pressed_changed as *const c_void as usize);
        let direction_block = block(direction_changed as *const c_void as usize);
        let profile_block = block(profile_changed as *const c_void as usize);
        unsafe {
            set_input_handler(
                direction.x_axis,
                objc::sel::get("setValueChangedHandler:"),
                std::ptr::from_ref(&axis_block).cast_mut().cast(),
            );
            set_input_handler(
                profile.button_a,
                objc::sel::get("setValueChangedHandler:"),
                std::ptr::from_ref(&button_block).cast_mut().cast(),
            );
            set_pressed_handler(
                profile.button_a,
                objc::sel::get("setPressedChangedHandler:"),
                std::ptr::from_ref(&pressed_block).cast_mut().cast(),
            );
            set_direction_handler(
                profile.left_thumbstick,
                objc::sel::get("setValueChangedHandler:"),
                std::ptr::from_ref(&direction_block).cast_mut().cast(),
            );
            set_profile_handler(
                profile_id,
                objc::sel::get("setValueChangedHandler:"),
                std::ptr::from_ref(&profile_block).cast_mut().cast(),
            );
            assert!(
                !get_profile_handler(profile_id, objc::sel::get("valueChangedHandler")).is_null()
            );
            sdl3::virtual_joystick::set_axis(joystick, sdl3::gamepad_axis::LEFT_X, i16::MIN);
            sdl3::virtual_joystick::set_axis(joystick, sdl3::gamepad_axis::LEFT_Y, 0);
            sdl3::virtual_joystick::set_button(joystick, sdl3::gamepad_button::SOUTH, false);
        }
        pump();
        {
            let events = events.lock().unwrap();
            assert_eq!(events.axes, [(direction.x_axis as usize, -1.0)]);
            assert_eq!(events.buttons, [(profile.button_a as usize, 0.0, 0)]);
            assert_eq!(events.pressed, [(profile.button_a as usize, 0)]);
            assert_eq!(
                events.directions,
                [(profile.left_thumbstick as usize, -1.0, 0.0)]
            );
            assert_eq!(
                events.profiles,
                [profile.button_a as usize, profile.left_thumbstick as usize],
                "profiles report containing elements once"
            );
        }
        unsafe {
            assert_eq!(input_value(direction.left, objc::sel::get("value")), 1.0);
            assert_eq!(input_value(direction.right, objc::sel::get("value")), 0.0);
            assert!(
                get_input_handler(direction.x_axis, objc::sel::get("valueChangedHandler"))
                    .is_null()
            );
        }
        pump();
        assert_eq!(
            events.lock().unwrap().profiles.len(),
            2,
            "unchanged values produce no callbacks"
        );
        unsafe {
            sdl3::virtual_joystick::set_button(joystick, sdl3::gamepad_button::SOUTH, true);
        }
        pump();
        assert_eq!(
            events.lock().unwrap().pressed,
            [
                (profile.button_a as usize, 0),
                (profile.button_a as usize, 1)
            ]
        );
        unsafe {
            set_input_handler(
                profile.button_a,
                objc::sel::get("setValueChangedHandler:"),
                std::ptr::null_mut(),
            );
            set_pressed_handler(
                profile.button_a,
                objc::sel::get("setPressedChangedHandler:"),
                std::ptr::null_mut(),
            );
            set_direction_handler(
                profile.left_thumbstick,
                objc::sel::get("setValueChangedHandler:"),
                std::ptr::null_mut(),
            );
            set_profile_handler(
                profile_id,
                objc::sel::get("setValueChangedHandler:"),
                std::ptr::null_mut(),
            );
            sdl3::close(joystick);
        }
        assert!(sdl3::virtual_joystick::detach(id));
        sync_devices(false);
        unsafe {
            assert_eq!(input_value(profile.button_a, objc::sel::get("value")), 0.0);
        }
    }
}
