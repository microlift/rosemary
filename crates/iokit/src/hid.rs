use crate::manager::ValueCallback;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use corefoundation::base::{CFIndex, CFTypeID};

static DEVICE_TYPE: AtomicU8 = AtomicU8::new(0);
static ELEMENT_TYPE: AtomicU8 = AtomicU8::new(0);

pub fn device_type_id() -> CFTypeID {
    std::ptr::addr_of!(DEVICE_TYPE) as CFTypeID
}

pub fn element_type_id() -> CFTypeID {
    std::ptr::addr_of!(ELEMENT_TYPE) as CFTypeID
}

pub type RemovalCallback = unsafe extern "C" fn(*mut c_void, i32, *mut c_void);
pub type ReportCallback =
    unsafe extern "C" fn(*mut c_void, i32, *mut c_void, u32, u32, *mut u8, CFIndex);

#[derive(Clone, Copy, Default)]
struct RegisteredReport {
    buffer: usize,
    length: usize,
    callback: Option<ReportCallback>,
    context: usize,
}

pub mod page {
    pub const GENERIC_DESKTOP: u32 = 0x01;
    pub const BUTTON: u32 = 0x09;
}

pub mod usage {
    pub const X: u32 = 0x30;
    pub const Y: u32 = 0x31;
    pub const Z: u32 = 0x32;
    pub const RX: u32 = 0x33;
    pub const RY: u32 = 0x34;
    pub const RZ: u32 = 0x35;
    pub const HATSWITCH: u32 = 0x39;
    pub const GAMEPAD: u32 = 0x05;
    pub const JOYSTICK: u32 = 0x04;
}

pub mod element_type {
    pub const INPUT_MISC: u32 = 1;
    pub const INPUT_BUTTON: u32 = 2;
    pub const INPUT_AXIS: u32 = 3;
    pub const COLLECTION: u32 = 513;
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Source {
    Axis(i32),
    Button(i32),
    GamepadAxis(i32, bool),
    GamepadButton(i32),
    Hat(i32),
    /// The device's root collection, which has a usage but no value.
    Collection,
}

#[derive(Clone, Debug)]
pub struct Element {
    pub cookie: u32,
    pub kind: u32,
    pub page: u32,
    pub usage: u32,
    pub logical_min: i64,
    pub logical_max: i64,
    pub source: Source,
}

impl Element {
    /// Bits this element occupies in one report: 16 for an axis (packed as
    /// `i16` by [`Device::input_report`]), 8 for a hat (one byte), 1 for a
    /// button, 0 for a collection, which carries no value of its own.
    pub fn report_size_bits(&self) -> u32 {
        match self.source {
            Source::Axis(_) | Source::GamepadAxis(_, _) => 16,
            Source::Hat(_) => 8,
            Source::Button(_) | Source::GamepadButton(_) => 1,
            Source::Collection => 0,
        }
    }

    /// How many `report_size_bits()`-wide fields this element has in one
    /// report. Every element here reports a single value rather than an
    /// array, so this is one wherever there is a value at all.
    pub fn report_count(&self) -> u32 {
        match self.source {
            Source::Collection => 0,
            _ => 1,
        }
    }
}

/// Axes a HID gamepad reports, in the order SDL numbers them.
const AXIS_USAGES: &[u32] = &[
    usage::X,
    usage::Y,
    usage::Z,
    usage::RX,
    usage::RY,
    usage::RZ,
];

static DEVICES: Mutex<BTreeMap<sdl3::JoystickId, Arc<Device>>> = Mutex::new(BTreeMap::new());

struct Transport {
    joystick: usize,
    gamepad: usize,
    raw: usize,
}
impl Transport {
    fn close_with(
        &mut self,
        close_raw: impl FnOnce(usize),
        close_gamepad: impl FnOnce(usize),
        close_joystick: impl FnOnce(usize),
    ) {
        let raw = std::mem::take(&mut self.raw);
        let gamepad = std::mem::take(&mut self.gamepad);
        let joystick = std::mem::take(&mut self.joystick);
        if raw != 0 {
            close_raw(raw);
        }
        if gamepad != 0 {
            close_gamepad(gamepad);
        }
        if joystick != 0 {
            close_joystick(joystick);
        }
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        self.close_with(
            |raw| unsafe {
                sdl3::close_hid(raw as sdl3::HidDevice);
            },
            |gamepad| unsafe {
                sdl3::close_gamepad(gamepad as sdl3::Gamepad);
            },
            |joystick| unsafe {
                sdl3::close(joystick as sdl3::Joystick);
            },
        );
    }
}

pub struct Device {
    pub id: sdl3::JoystickId,
    transport: Mutex<Transport>,
    raw_descriptor: Option<Vec<u8>>,
    numbered_reports: bool,
    pub(crate) schedules: crate::schedule::Schedules,
    enabled: AtomicBool,
    polling: AtomicBool,
    removed_notified: AtomicBool,
    values: Mutex<(Option<ValueCallback>, usize)>,
    pub name: String,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    pub elements: Vec<Element>,
    /// The value each element last reported, so a change can be noticed.
    last: Mutex<Vec<i64>>,
    /// A stable pointer per element, which is what an `IOHIDElementRef` is.
    ///
    /// Filled in once the device is inside its `Arc`, because each one holds the
    /// device's address and that is not known before then. See
    /// [`Device::publish`].
    handles: std::sync::OnceLock<Vec<crate::value::ElementRef>>,
    removal: Mutex<(Option<RemovalCallback>, usize)>,
    report: Mutex<RegisteredReport>,
    last_report: Mutex<Vec<u8>>,
}

// SAFETY: transport operations are serialized, SDL joystick operations permit
// any thread, and element handles point into immutable, process-lived data.
unsafe impl Send for Device {}
unsafe impl Sync for Device {}

impl Device {
    /// Open `id` and describe it as a tree of elements.
    pub fn open(id: sdl3::JoystickId) -> Option<Arc<Device>> {
        sdl3::init_controllers().ok()?;
        let mut devices = DEVICES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(device) = devices.get(&id) {
            return device.is_connected().then(|| Arc::clone(device));
        }
        let device = Self::open_new(id)?;
        devices.insert(id, Arc::clone(&device));
        Some(device)
    }

    pub fn discover() -> Vec<Arc<Device>> {
        if sdl3::init_controllers().is_err() {
            return Vec::new();
        }
        sdl3::update_joysticks();
        let attached = sdl3::joysticks();
        for id in &attached {
            let _ = Self::open(*id);
        }
        let devices: Vec<_> = DEVICES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for device in &devices {
            if !attached.contains(&device.id) {
                device.disconnect();
            }
        }
        devices.into_iter().filter(|d| d.is_connected()).collect()
    }

    fn open_new(id: sdl3::JoystickId) -> Option<Arc<Device>> {
        let joystick = sdl3::open(id)?;
        // SAFETY: a joystick just opened and not yet closed.
        let (name, physical_vendor, physical_product, physical_version) = unsafe {
            (
                sdl3::name(joystick),
                sdl3::vendor(joystick),
                sdl3::product(joystick),
                sdl3::version(joystick),
            )
        };
        let (axes, buttons, hats) = unsafe {
            (
                sdl3::axes(joystick),
                sdl3::buttons(joystick),
                sdl3::hats(joystick),
            )
        };
        let gamepad = sdl3::open_gamepad(id).map_or(0, |g| g as usize);
        let mut raw = unsafe { sdl3::open_hid(joystick) }.map_or(0, |d| d as usize);
        let mut raw_descriptor = if raw != 0 {
            sdl3::hid_report_descriptor(raw as sdl3::HidDevice).filter(|d| !d.is_empty())
        } else {
            None
        };
        let compatibility = gamepad != 0
            && (physical_vendor == sdl3::usb_vendor::MICROSOFT || raw_descriptor.is_none());
        if raw != 0 && (compatibility || raw_descriptor.is_none()) {
            unsafe {
                sdl3::close_hid(raw as sdl3::HidDevice);
            }
            raw = 0;
            raw_descriptor = None;
        }
        let numbered_reports = raw_descriptor.as_deref().is_some_and(has_report_ids);
        let (vendor, product, version, mut elements) = if compatibility {
            // Legacy macOS games recognize this identity and semantic layout.
            (
                sdl3::usb_vendor::MICROSOFT,
                sdl3::usb_product::XBOX_360_CONTROLLER,
                0,
                describe_gamepad(buttons),
            )
        } else {
            (
                physical_vendor,
                physical_product,
                physical_version,
                describe(axes, buttons, hats),
            )
        };
        if gamepad == 0 {
            elements[0].usage = usage::JOYSTICK;
        }
        crate::trace_hid(format_args!(
            "device id={id} name={name:?} SDL={physical_vendor:04x}:{physical_product:04x}:{physical_version:04x} \
             published={published_vendor:04x}:{published_product:04x}:{published_version:04x} \
             gamepad={} raw_hid={} elements={}",
            gamepad != 0,
            raw != 0,
            elements.len(),
            published_vendor = vendor,
            published_product = product,
            published_version = version,
        ));
        let last = Mutex::new(vec![i64::MIN; elements.len()]);
        let device = std::sync::Arc::new(Device {
            id,
            transport: Mutex::new(Transport {
                joystick: joystick as usize,
                gamepad,
                raw,
            }),
            raw_descriptor,
            numbered_reports,
            schedules: Default::default(),
            enabled: AtomicBool::new(true),
            polling: AtomicBool::new(false),
            removed_notified: AtomicBool::new(false),
            values: Mutex::new((None, 0)),
            name,
            vendor,
            product,
            version,
            elements,
            last,
            handles: std::sync::OnceLock::new(),
            removal: Mutex::new((None, 0)),
            report: Mutex::new(RegisteredReport::default()),
            last_report: Mutex::new(Vec::new()),
        });
        device.publish();
        Some(device)
    }

    /// Give every element a stable handle, now that the device has an address.
    fn publish(self: &std::sync::Arc<Device>) {
        let base = std::sync::Arc::as_ptr(self);
        let handles: Vec<crate::value::ElementRef> = self
            .elements
            .iter()
            .map(|e| crate::value::ElementRef {
                device: base,
                cookie: e.cookie,
            })
            .collect();
        // The device and every element handle are things a guest may `CFRetain`
        // - see `corefoundation::base::note_opaque` - and both live as long as
        // the device does, which is as long as the process.
        corefoundation::base::note_typed_opaque(base as *const std::ffi::c_void, device_type_id());
        let _ = self.handles.set(handles);
        for handle in self.handles.get().into_iter().flatten() {
            corefoundation::base::note_typed_opaque(
                std::ptr::from_ref(handle) as *const std::ffi::c_void,
                element_type_id(),
            );
        }
    }

    /// The `IOHIDElementRef` for one element.
    pub fn handle(&self, cookie: u32) -> *const std::ffi::c_void {
        self.handles
            .get()
            .and_then(|h| h.get(cookie as usize))
            .map_or(std::ptr::null(), |r| {
                r as *const _ as *const std::ffi::c_void
            })
    }

    pub fn element(&self, cookie: u32) -> Option<&Element> {
        self.elements.get(cookie as usize)
    }

    pub fn set_removal_callback(&self, callback: Option<RemovalCallback>, context: usize) {
        *self.removal.lock().unwrap_or_else(|e| e.into_inner()) = (callback, context);
    }

    pub fn notify_removed(&self) {
        if self.removed_notified.swap(true, Ordering::AcqRel) {
            return;
        }
        let (callback, context) = *self.removal.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(callback) = callback {
            // SAFETY: the registration supplies an IOHIDCallback and its
            // untouched context. The device remains alive for this call.
            unsafe {
                callback(
                    context as *mut c_void,
                    0,
                    std::ptr::from_ref(self).cast_mut().cast(),
                )
            };
        }
    }

    pub fn set_report_callback(
        &self,
        buffer: *mut u8,
        length: usize,
        callback: Option<ReportCallback>,
        context: usize,
    ) {
        *self.report.lock().unwrap_or_else(|e| e.into_inner()) = RegisteredReport {
            buffer: buffer as usize,
            length,
            callback,
            context,
        };
        self.last_report
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// The decoded SDL state encoded as the descriptor returned for this
    /// device: little-endian axes, byte-wide hats, then packed buttons.
    pub fn input_report(&self) -> Vec<u8> {
        let mut report = Vec::with_capacity(self.max_input_report_size());
        for element in self
            .elements
            .iter()
            .filter(|e| matches!(e.source, Source::Axis(_) | Source::GamepadAxis(_, _)))
        {
            report.extend_from_slice(&(self.value(element) as i16).to_le_bytes());
        }
        for element in self
            .elements
            .iter()
            .filter(|e| matches!(e.source, Source::Hat(_)))
        {
            report.push(self.value(element) as u8);
        }
        let buttons: Vec<&Element> = self
            .elements
            .iter()
            .filter(|e| matches!(e.source, Source::Button(_) | Source::GamepadButton(_)))
            .collect();
        for chunk in buttons.chunks(8) {
            let mut byte = 0;
            for (bit, element) in chunk.iter().enumerate() {
                byte |= ((self.value(element) != 0) as u8) << bit;
            }
            report.push(byte);
        }
        report
    }

    pub fn max_input_report_size(&self) -> usize {
        if self.raw_descriptor.is_some() {
            // SDL recommends 4096 bytes for raw HID descriptors. The same
            // bound is ample for an interrupt input report and is what HIDAPI
            // uses for its reusable callback buffer.
            return 4096;
        }
        let axes = self
            .elements
            .iter()
            .filter(|e| matches!(e.source, Source::Axis(_) | Source::GamepadAxis(_, _)))
            .count();
        let hats = self
            .elements
            .iter()
            .filter(|e| matches!(e.source, Source::Hat(_)))
            .count();
        let buttons = self
            .elements
            .iter()
            .filter(|e| matches!(e.source, Source::Button(_) | Source::GamepadButton(_)))
            .count();
        axes * 2 + hats + buttons.div_ceil(8)
    }

    /// A HID descriptor for the bytes produced by [`Device::input_report`].
    pub fn report_descriptor(&self) -> Vec<u8> {
        self.raw_descriptor
            .clone()
            .unwrap_or_else(|| descriptor(&self.elements))
    }

    fn notify_report(&self, inherited: bool, active: &impl Fn() -> bool) {
        let registered = *self.report.lock().unwrap_or_else(|e| e.into_inner());
        let Some(callback) = registered.callback else {
            return;
        };
        let raw = self.transport.lock().unwrap_or_else(|e| e.into_inner());
        if raw.raw != 0 {
            if registered.length == 0 || registered.buffer == 0 {
                return;
            }
            drop(raw);
            drain_raw_reports(
                registered.length,
                self.numbered_reports,
                |buffer| {
                    if !active()
                        || !self.enabled.load(Ordering::Acquire)
                        || !self.is_connected()
                        || (!inherited && !self.schedules.current())
                    {
                        return None;
                    }
                    let current = *self.report.lock().unwrap_or_else(|e| e.into_inner());
                    if current.buffer != registered.buffer
                        || current.length != registered.length
                        || current.callback.map(|c| c as usize) != Some(callback as usize)
                        || current.context != registered.context
                    {
                        return None;
                    }
                    let raw = self.transport.lock().unwrap_or_else(|e| e.into_inner());
                    if raw.raw == 0 {
                        return None;
                    }
                    sdl3::hid_read(raw.raw as sdl3::HidDevice, buffer)
                },
                |report_id, bytes| {
                    // The guest may unregister or close from its callback. Keep no
                    // borrowed guest buffer or transport lock across that call.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            bytes.as_ptr(),
                            registered.buffer as *mut u8,
                            bytes.len(),
                        );
                        callback(
                            registered.context as *mut c_void,
                            0,
                            std::ptr::from_ref(self).cast_mut().cast(),
                            0,
                            report_id,
                            registered.buffer as *mut u8,
                            bytes.len() as CFIndex,
                        );
                    }
                },
            );
            return;
        }
        drop(raw);
        let report = self.input_report();
        {
            let mut last = self.last_report.lock().unwrap_or_else(|e| e.into_inner());
            if *last == report {
                return;
            }
            last.clone_from(&report);
        }
        let length = report.len().min(registered.length);
        if length != 0 && registered.buffer == 0 {
            return;
        }
        // SAFETY: the registration promises a writable buffer of its stated
        // size, and the callback is invoked only while the device is alive.
        unsafe {
            if length != 0 {
                std::ptr::copy_nonoverlapping(
                    report.as_ptr(),
                    registered.buffer as *mut u8,
                    length,
                );
            }
            callback(
                registered.context as *mut c_void,
                0,
                std::ptr::from_ref(self).cast_mut().cast(),
                0,
                0,
                registered.buffer as *mut u8,
                length as CFIndex,
            );
        }
        crate::trace_hid(format_args!(
            "input report id={} synthetic length={length}",
            self.id
        ));
    }

    pub fn raw_get_report(&self, feature: bool, report_id: u8, capacity: usize) -> Option<Vec<u8>> {
        let raw = self.transport.lock().unwrap_or_else(|e| e.into_inner());
        if raw.raw == 0 {
            return None;
        }
        let mut bytes = vec![0; capacity];
        if bytes.is_empty() {
            return Some(bytes);
        }
        bytes[0] = report_id;
        let length = sdl3::hid_get_report(raw.raw as sdl3::HidDevice, feature, &mut bytes)?;
        bytes.truncate(length.min(bytes.len()));
        Some(bytes)
    }

    pub fn raw_set_report(&self, feature: bool, bytes: &[u8]) -> Option<bool> {
        let raw = self.transport.lock().unwrap_or_else(|e| e.into_inner());
        (raw.raw != 0).then(|| sdl3::hid_write(raw.raw as sdl3::HidDevice, feature, bytes))
    }

    pub fn has_raw_reports(&self) -> bool {
        self.transport.lock().unwrap_or_else(|e| e.into_inner()).raw != 0
    }

    /// What an element reads right now.
    pub fn value(&self, element: &Element) -> i64 {
        let transport = self.transport.lock().unwrap_or_else(|e| e.into_inner());
        if transport.joystick == 0 {
            return 0;
        }
        let joystick = transport.joystick as sdl3::Joystick;
        let gamepad = transport.gamepad as sdl3::Gamepad;
        match element.source {
            Source::Axis(i) => unsafe { sdl3::axis(joystick, i) as i64 },
            Source::Button(i) => unsafe { sdl3::button(joystick, i) as i64 },
            Source::GamepadAxis(i, invert) => unsafe {
                let value = sdl3::gamepad_axis(gamepad, i);
                if invert {
                    value.saturating_neg() as i64
                } else {
                    value as i64
                }
            },
            Source::GamepadButton(i) => unsafe { sdl3::gamepad_button(gamepad, i) as i64 },
            Source::Hat(i) => hat_to_direction(unsafe { sdl3::hat(joystick, i) }),
            Source::Collection => 0,
        }
    }

    pub fn is_connected(&self) -> bool {
        let t = self.transport.lock().unwrap_or_else(|e| e.into_inner());
        t.joystick != 0 && unsafe { sdl3::joystick_connected(t.joystick as sdl3::Joystick) }
    }

    fn disconnect(&self) {
        let old = std::mem::replace(
            &mut *self.transport.lock().unwrap_or_else(|e| e.into_inner()),
            Transport {
                joystick: 0,
                gamepad: 0,
                raw: 0,
            },
        );
        drop(old);
    }

    pub fn open_input(&self) -> bool {
        if !self.is_connected() {
            return false;
        }
        self.enabled.store(true, Ordering::Release);
        true
    }

    pub fn close_input(&self) {
        self.enabled.store(false, Ordering::Release);
        self.schedules.clear();
    }

    pub fn send_effect(&self, bytes: &[u8]) -> bool {
        let t = self.transport.lock().unwrap_or_else(|e| e.into_inner());
        t.joystick != 0 && unsafe { sdl3::send_effect(t.joystick as sdl3::Joystick, bytes) }
    }

    pub fn set_values(&self, callback: Option<ValueCallback>, context: usize) {
        *self.values.lock().unwrap_or_else(|e| e.into_inner()) = (callback, context);
        self.last
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .fill(i64::MIN);
    }

    pub(crate) fn poll(&self, inherited: bool, active: impl Fn() -> bool) {
        if (inherited && self.schedules.any())
            || (!inherited && !self.schedules.current())
            || !self.enabled.load(Ordering::Acquire)
            || self.polling.swap(true, Ordering::AcqRel)
        {
            return;
        }
        struct Guard<'a>(&'a AtomicBool);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = Guard(&self.polling);
        Self::discover();
        if !self.is_connected() {
            self.notify_removed();
            return;
        }
        self.notify_report(inherited, &active);
        for (cookie, integer) in self.changed() {
            if !active()
                || !self.enabled.load(Ordering::Acquire)
                || (!inherited && !self.schedules.current())
            {
                break;
            }
            let (callback, context) = *self.values.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(callback) = callback {
                let value = crate::value::new(self, cookie, integer);
                unsafe {
                    callback(
                        context as *mut c_void,
                        0,
                        std::ptr::from_ref(self).cast_mut().cast(),
                        value,
                    );
                }
                crate::value::release(value);
            }
        }
    }

    /// Every element whose value changed since the last time this was asked.
    ///
    /// The device is polled and the changes reported, rather than SDL's events
    /// being read: a HID input-value callback is *per element*, and an SDL event
    /// is per axis or per button with no element behind it. Polling the elements
    /// is what turns one into the other, and it is also what makes a device that
    /// was already moved when the guest registered its callback report its
    /// current state rather than nothing.
    pub fn changed(&self) -> Vec<(u32, i64)> {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        for element in &self.elements {
            if element.source == Source::Collection {
                continue;
            }
            let now = self.value(element);
            let slot = &mut last[element.cookie as usize];
            if *slot != now {
                *slot = now;
                out.push((element.cookie, now));
            }
        }
        out
    }
}

fn drain_raw_reports(
    capacity: usize,
    numbered: bool,
    mut read: impl FnMut(&mut [u8]) -> Option<usize>,
    mut deliver: impl FnMut(u32, &[u8]),
) {
    let mut buffer = vec![0; capacity];
    // Drain queued input while bounding the work of noisy devices.
    for _ in 0..64 {
        let Some(length) = read(&mut buffer) else {
            break;
        };
        if length == 0 || length > capacity {
            break;
        }
        let report_id = if numbered { buffer[0] as u32 } else { 0 };
        deliver(report_id, &buffer[..length]);
    }
}

fn descriptor(elements: &[Element]) -> Vec<u8> {
    let mut out = vec![0x05, 0x01, 0x09, elements[0].usage as u8, 0xa1, 0x01];
    for element in elements.iter().filter(|e| {
        matches!(
            e.source,
            Source::Axis(_) | Source::GamepadAxis(_, _) | Source::Hat(_)
        )
    }) {
        // Each item resets its globals, including unsigned triggers and hats.
        out.extend_from_slice(&[0x05, element.page as u8, 0x09, element.usage as u8, 0x16]);
        out.extend_from_slice(&(element.logical_min as i16).to_le_bytes());
        out.push(0x26);
        out.extend_from_slice(&(element.logical_max as i16).to_le_bytes());
        out.extend_from_slice(&[
            0x75,
            if matches!(element.source, Source::Hat(_)) {
                8
            } else {
                16
            },
            0x95,
            1,
            0x81,
            if matches!(element.source, Source::Hat(_)) {
                0x42
            } else {
                0x02
            },
        ]);
    }
    let buttons: Vec<_> = elements
        .iter()
        .filter(|e| matches!(e.source, Source::Button(_) | Source::GamepadButton(_)))
        .collect();
    for element in &buttons {
        out.extend_from_slice(&[0x05, 0x09, 0x0a]);
        out.extend_from_slice(&(element.usage as u16).to_le_bytes());
        out.extend_from_slice(&[0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x81, 2]);
    }
    let padding = (8 - buttons.len() % 8) % 8;
    if padding != 0 {
        out.extend_from_slice(&[0x75, 1, 0x95, padding as u8, 0x81, 3]);
    }
    out.push(0xc0);
    out
}

fn has_report_ids(descriptor: &[u8]) -> bool {
    let mut offset = 0;
    while let Some(&prefix) = descriptor.get(offset) {
        if prefix == 0xfe {
            let Some(&length) = descriptor.get(offset + 1) else {
                break;
            };
            offset += 3 + length as usize;
        } else {
            let size = match prefix & 3 {
                3 => 4,
                n => n as usize,
            };
            if descriptor.get(offset + 1..offset + 1 + size).is_none() {
                break;
            }
            if prefix & 0xfc == 0x84 {
                return true;
            }
            offset += 1 + size;
        }
    }
    false
}

fn hat_to_direction(bits: u8) -> i64 {
    const UP: u8 = 0x01;
    const RIGHT: u8 = 0x02;
    const DOWN: u8 = 0x04;
    const LEFT: u8 = 0x08;
    match bits {
        UP => 0,
        b if b == UP | RIGHT => 1,
        RIGHT => 2,
        b if b == DOWN | RIGHT => 3,
        DOWN => 4,
        b if b == DOWN | LEFT => 5,
        LEFT => 6,
        b if b == UP | LEFT => 7,
        _ => 8,
    }
}

/// The element tree for a device with this many axes, buttons and hats.
///
/// The root collection is element zero, so a guest walking children from the
/// top finds everything - which is what `IOHIDElementGetChildren` is for.
pub fn describe(axes: i32, buttons: i32, hats: i32) -> Vec<Element> {
    let mut out = vec![Element {
        cookie: 0,
        kind: element_type::COLLECTION,
        page: page::GENERIC_DESKTOP,
        usage: usage::GAMEPAD,
        logical_min: 0,
        logical_max: 0,
        source: Source::Collection,
    }];
    for i in 0..axes.max(0) {
        out.push(Element {
            cookie: out.len() as u32,
            kind: element_type::INPUT_AXIS,
            page: page::GENERIC_DESKTOP,
            // Past the six named axes a device is reporting something this
            // runtime has no HID usage for, so it gets none rather than a
            // neighbour's - a guest matching on usage will skip it, which is
            // better than it reading the wrong stick.
            usage: AXIS_USAGES.get(i as usize).copied().unwrap_or(0),
            logical_min: i16::MIN as i64,
            logical_max: i16::MAX as i64,
            source: Source::Axis(i),
        });
    }
    for i in 0..hats.max(0) {
        out.push(Element {
            cookie: out.len() as u32,
            kind: element_type::INPUT_MISC,
            page: page::GENERIC_DESKTOP,
            usage: usage::HATSWITCH,
            logical_min: 0,
            // Seven directions, and the eighth value is "centred".
            logical_max: 7,
            source: Source::Hat(i),
        });
    }
    for i in 0..buttons.max(0) {
        out.push(Element {
            cookie: out.len() as u32,
            kind: element_type::INPUT_BUTTON,
            page: page::BUTTON,
            // HID numbers buttons from one.
            usage: i as u32 + 1,
            logical_min: 0,
            logical_max: 1,
            source: Source::Button(i),
        });
    }
    out
}

/// Apple's synthetic Xbox HID layout, backed by SDL's semantic gamepad API.
fn describe_gamepad(reported_buttons: i32) -> Vec<Element> {
    let mut out = describe(
        sdl3::gamepad_axis::COUNT,
        reported_buttons.clamp(15, sdl3::gamepad_button::COUNT),
        0,
    );
    let axes = [
        (sdl3::gamepad_axis::LEFT_X, false),
        (sdl3::gamepad_axis::LEFT_Y, false),
        (sdl3::gamepad_axis::LEFT_TRIGGER, false),
        (sdl3::gamepad_axis::RIGHT_X, false),
        (sdl3::gamepad_axis::RIGHT_Y, true),
        (sdl3::gamepad_axis::RIGHT_TRIGGER, false),
    ];
    for (element, (axis, invert)) in out.iter_mut().skip(1).zip(axes) {
        element.source = Source::GamepadAxis(axis, invert);
        if axis == sdl3::gamepad_axis::LEFT_TRIGGER || axis == sdl3::gamepad_axis::RIGHT_TRIGGER {
            element.logical_min = 0;
        }
    }
    let buttons = [
        sdl3::gamepad_button::SOUTH,
        sdl3::gamepad_button::EAST,
        sdl3::gamepad_button::WEST,
        sdl3::gamepad_button::NORTH,
        sdl3::gamepad_button::LEFT_SHOULDER,
        sdl3::gamepad_button::RIGHT_SHOULDER,
        sdl3::gamepad_button::LEFT_STICK,
        sdl3::gamepad_button::RIGHT_STICK,
        sdl3::gamepad_button::BACK,
        sdl3::gamepad_button::START,
        sdl3::gamepad_button::GUIDE,
        sdl3::gamepad_button::DPAD_UP,
        sdl3::gamepad_button::DPAD_DOWN,
        sdl3::gamepad_button::DPAD_LEFT,
        sdl3::gamepad_button::DPAD_RIGHT,
        sdl3::gamepad_button::MISC1,
        sdl3::gamepad_button::RIGHT_PADDLE1,
        sdl3::gamepad_button::LEFT_PADDLE1,
        sdl3::gamepad_button::RIGHT_PADDLE2,
        sdl3::gamepad_button::LEFT_PADDLE2,
        sdl3::gamepad_button::TOUCHPAD,
        sdl3::gamepad_button::MISC2,
        sdl3::gamepad_button::MISC3,
        sdl3::gamepad_button::MISC4,
        sdl3::gamepad_button::MISC5,
        sdl3::gamepad_button::MISC6,
    ];
    for (element, button) in out
        .iter_mut()
        .skip(1 + sdl3::gamepad_axis::COUNT as usize)
        .zip(buttons)
    {
        element.source = Source::GamepadButton(button);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(source: Source) -> Element {
        Element {
            cookie: 0,
            kind: 0,
            page: 0,
            usage: 0,
            logical_min: 0,
            logical_max: 0,
            source,
        }
    }

    #[test]
    fn report_size_and_count_match_what_input_report_packs() {
        // An axis is a little-endian `i16`, a hat one byte, a button one bit -
        // the exact widths `Device::input_report` writes.
        assert_eq!(element(Source::Axis(0)).report_size_bits(), 16);
        assert_eq!(
            element(Source::GamepadAxis(0, false)).report_size_bits(),
            16
        );
        assert_eq!(element(Source::Hat(0)).report_size_bits(), 8);
        assert_eq!(element(Source::Button(0)).report_size_bits(), 1);
        assert_eq!(element(Source::GamepadButton(0)).report_size_bits(), 1);
        // The root collection has no value, so it has no size and no count.
        assert_eq!(element(Source::Collection).report_size_bits(), 0);
        assert_eq!(element(Source::Collection).report_count(), 0);
        for source in [
            Source::Axis(0),
            Source::Button(0),
            Source::Hat(0),
            Source::GamepadAxis(0, false),
            Source::GamepadButton(0),
        ] {
            assert_eq!(element(source).report_count(), 1);
        }
    }

    #[test]
    fn raw_reports_follow_descriptor_numbering_and_stop_on_read_failure() {
        assert!(!has_report_ids(&[0x05, 1, 0x09, 5, 0x81, 2]));
        assert!(has_report_ids(&[0x05, 1, 0x85, 7, 0x81, 2]));
        assert!(!has_report_ids(&[0xfe, 2, 0, 0x85, 7]));
        assert!(!has_report_ids(&[0x85]));
        for numbered in [false, true] {
            let mut reads = 0;
            let mut received = Vec::new();
            drain_raw_reports(
                8,
                numbered,
                |buffer| {
                    reads += 1;
                    if reads > 2 {
                        return None;
                    }
                    buffer[..3].copy_from_slice(&[7, reads, 42]);
                    Some(3)
                },
                |id, bytes| received.push((id, bytes.to_vec())),
            );
            assert_eq!(
                received,
                vec![
                    (if numbered { 7 } else { 0 }, vec![7, 1, 42]),
                    (if numbered { 7 } else { 0 }, vec![7, 2, 42])
                ]
            );
        }
        let mut count = 0;
        drain_raw_reports(1, false, |_| Some(1), |_, _| count += 1);
        assert_eq!(count, 64);
        drain_raw_reports(1, false, |_| Some(2), |_, _| panic!("invalid read length"));
        drain_raw_reports(1, false, |_| Some(0), |_, _| panic!("no report available"));
    }

    #[test]
    fn transport_closes_each_owned_handle_once() {
        let calls = std::cell::RefCell::new(Vec::new());
        let mut t = Transport {
            raw: 1,
            gamepad: 2,
            joystick: 3,
        };
        t.close_with(
            |h| calls.borrow_mut().push(h),
            |h| calls.borrow_mut().push(h),
            |h| calls.borrow_mut().push(h),
        );
        t.close_with(
            |_| panic!("closed twice"),
            |_| panic!("closed twice"),
            |_| panic!("closed twice"),
        );
        assert_eq!(*calls.borrow(), [1, 2, 3]);
    }

    #[test]
    fn synthetic_descriptor_preserves_ranges_usages_and_bit_counts() {
        let elements = describe_gamepad(15);
        let bytes = descriptor(&elements);
        // Decode the short items a HID reader uses to assign fields and ranges.
        let mut offset = 0;
        let (mut page, mut usage, mut min, mut max, mut size, mut count) = (0, 0, 0i32, 0i32, 0, 0);
        let mut inputs = Vec::new();
        while offset < bytes.len() {
            let prefix = bytes[offset];
            let n = match prefix & 3 {
                3 => 4,
                n => n as usize,
            };
            let mut raw = [0; 4];
            raw[..n].copy_from_slice(&bytes[offset + 1..offset + 1 + n]);
            let value = u32::from_le_bytes(raw);
            match prefix & 0xfc {
                0x04 => page = value,
                0x08 => usage = value,
                0x14 => {
                    min = if n == 2 {
                        value as i16 as i32
                    } else {
                        value as i8 as i32
                    }
                }
                0x24 => {
                    max = if min < 0 {
                        value as i16 as i32
                    } else {
                        value as i32
                    }
                }
                0x74 => size = value,
                0x94 => count = value,
                0x80 => inputs.push((page, usage, min, max, size * count, value)),
                _ => {}
            }
            offset += 1 + n;
        }
        assert_eq!(
            inputs[2],
            (page::GENERIC_DESKTOP, usage::Z, 0, 32767, 16, 2)
        );
        assert_eq!(
            inputs[3].2, -32768,
            "trigger globals must not leak into sticks"
        );
        assert_eq!(inputs.iter().map(|i| i.4).sum::<u32>(), 14 * 8);
        let extra = descriptor(&describe(8, 300, 2));
        assert!(!extra.is_empty());
        assert!(!has_report_ids(&extra));
    }

    /// The shape a guest walks: a root collection, then axes on the
    /// generic-desktop page, then a hat, then buttons numbered from one.
    #[test]
    fn a_gamepads_elements_are_the_ones_a_guest_matches_on() {
        let elements = describe(6, 15, 1);
        assert_eq!(elements.len(), 1 + 6 + 1 + 15);

        assert_eq!(elements[0].kind, element_type::COLLECTION);
        assert_eq!(elements[0].usage, usage::GAMEPAD);

        // The two sticks and the two triggers, in HID's own order.
        let axes: Vec<u32> = elements[1..7].iter().map(|e| e.usage).collect();
        assert_eq!(
            axes,
            vec![
                usage::X,
                usage::Y,
                usage::Z,
                usage::RX,
                usage::RY,
                usage::RZ
            ]
        );
        assert!(
            elements[1..7]
                .iter()
                .all(|e| e.kind == element_type::INPUT_AXIS)
        );
        assert_eq!(elements[1].logical_min, -32768);
        assert_eq!(elements[1].logical_max, 32767);

        let hat = &elements[7];
        assert_eq!(hat.usage, usage::HATSWITCH);
        assert_eq!((hat.logical_min, hat.logical_max), (0, 7));

        // Buttons on their own page, numbered from one, and each element's
        // cookie is its own index - which is what makes a stored cookie find
        // the same element later.
        assert_eq!(elements[8].page, page::BUTTON);
        assert_eq!(elements[8].usage, 1);
        assert!(
            elements
                .iter()
                .enumerate()
                .all(|(i, e)| e.cookie == i as u32)
        );
    }

    /// A hat's bitfield becomes HID's clockwise-from-north direction, and
    /// centred is the value past the logical maximum.
    #[test]
    fn a_hat_reads_as_a_direction() {
        assert_eq!(hat_to_direction(0x01), 0, "up");
        assert_eq!(hat_to_direction(0x03), 1, "up and right");
        assert_eq!(hat_to_direction(0x02), 2, "right");
        assert_eq!(hat_to_direction(0x0c), 5, "down and left");
        assert_eq!(hat_to_direction(0x08), 6, "left");
        assert_eq!(hat_to_direction(0x00), 8, "centred");
        // An impossible combination is centred rather than a direction it is
        // not: up and down at once is a device fault, not a diagonal.
        assert_eq!(hat_to_direction(0x05), 8, "up and down");
    }

    /// A device with more axes than HID has named usages leaves the extra ones
    /// unnamed rather than reusing a usage that means something else.
    #[test]
    fn an_extra_axis_gets_no_usage_rather_than_a_wrong_one() {
        let elements = describe(8, 0, 0);
        assert_eq!(elements[7].usage, 0);
        assert_eq!(elements[8].usage, 0);
    }

    #[test]
    fn an_xbox_gamepad_uses_apples_synthetic_hid_order() {
        let elements = describe_gamepad(18);
        assert_eq!(
            elements[1..7].iter().map(|e| e.source).collect::<Vec<_>>(),
            vec![
                Source::GamepadAxis(sdl3::gamepad_axis::LEFT_X, false),
                Source::GamepadAxis(sdl3::gamepad_axis::LEFT_Y, false),
                Source::GamepadAxis(sdl3::gamepad_axis::LEFT_TRIGGER, false),
                Source::GamepadAxis(sdl3::gamepad_axis::RIGHT_X, false),
                Source::GamepadAxis(sdl3::gamepad_axis::RIGHT_Y, true),
                Source::GamepadAxis(sdl3::gamepad_axis::RIGHT_TRIGGER, false),
            ]
        );
        assert_eq!(elements[3].logical_min, 0, "left trigger is unsigned");
        assert_eq!(elements[6].logical_min, 0, "right trigger is unsigned");
        assert_eq!(
            elements[7].source,
            Source::GamepadButton(sdl3::gamepad_button::SOUTH)
        );
        assert_eq!(
            elements[11].source,
            Source::GamepadButton(sdl3::gamepad_button::LEFT_SHOULDER)
        );
        assert_eq!(
            elements[13].source,
            Source::GamepadButton(sdl3::gamepad_button::LEFT_STICK)
        );
        assert_eq!(
            elements[18].source,
            Source::GamepadButton(sdl3::gamepad_button::DPAD_UP)
        );
    }
}
