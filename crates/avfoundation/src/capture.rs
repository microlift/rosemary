//! `AVCaptureDeviceDiscoverySession`, `AVCaptureDeviceInput`,
//! `AVCaptureSession` and `AVCaptureVideoDataOutput` - the rest of the
//! capture graph around `AVCaptureDevice` (registered in `lib.rs`).
//!
//! No camera backend exists (see `lib.rs`'s `AVCaptureDevice`), so none of
//! these ever see a real device: a session simply never has a live input to
//! deliver frames from, which is the same thing a real Mac with the camera
//! disabled or absent shows a caller.

use objc::object::Id;
use objc::sel::Sel;

use foundation::error::make as error;

use crate::define::{class_methods, methods};

/// `AVFoundationErrorDomain`.
const DOMAIN: &str = "AVFoundationErrorDomain";

/// `AVErrorDeviceNotConnected`, from AVError.h.
const ERROR_DEVICE_NOT_CONNECTED: i64 = -11814;

/// Write `error` through a guest's `NSError **`, if it passed one.
///
/// # Safety
///
/// `out` must be null or a writable `Id` slot.
unsafe fn report(out: *mut Id, domain: &str, code: i64, description: &str) {
    if out.is_null() {
        return;
    }
    // SAFETY: the caller's obligation.
    unsafe { *out = error(domain, code, description) };
}

pub fn install() {
    discovery_session();
    device_input();
    session();
    video_data_output();
}

// -- AVCaptureDeviceDiscoverySession -----------------------------------

/// `+[AVCaptureDeviceDiscoverySession discoverySessionWithDeviceTypes:mediaType:position:]`.
unsafe extern "C" fn discovery_session_with_device_types(
    class: Id,
    _sel: Sel,
    _device_types: Id,
    _media_type: Id,
    _position: i64,
) -> Id {
    // SAFETY: `class` is this method's own receiver, a registered class.
    let class = unsafe { objc::class::from_ptr(class as *const objc::class::Class) }
        .expect("a registered class");
    let object = objc::object::create_instance(class, 0);
    objc::arc::autorelease(object)
}

/// `-[AVCaptureDeviceDiscoverySession devices]`: always empty, since
/// `AVCaptureDevice` never enumerates one either.
unsafe extern "C" fn devices(_receiver: Id, _sel: Sel) -> Id {
    let array = corefoundation::collections::new_array(
        &[],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    objc::arc::autorelease(array as Id)
}

fn discovery_session() {
    let class = corefoundation::base::abstract_class(c"AVCaptureDeviceDiscoverySession", None);
    class_methods!(class, {
        "discoverySessionWithDeviceTypes:mediaType:position:"
            => discovery_session_with_device_types, c"@40@0:8@16@24q32";
    });
    methods!(class, {
        "devices" => devices, c"@16@0:8";
    });
}

// -- AVCaptureDeviceInput -----------------------------------------------

/// `+[AVCaptureDeviceInput deviceInputWithDevice:error:]`: always fails.
/// `AVCaptureDevice` never hands out a device for this to wrap, and a caller
/// that fabricates its own object does not have one this runtime can open
/// either way.
unsafe extern "C" fn device_input_with_device(
    _class: Id,
    _sel: Sel,
    _device: Id,
    error_out: *mut Id,
) -> Id {
    // SAFETY: the caller's `NSError **`, or null.
    unsafe {
        report(
            error_out,
            DOMAIN,
            ERROR_DEVICE_NOT_CONNECTED,
            "no capture device is connected",
        )
    };
    std::ptr::null_mut()
}

/// `-[AVCaptureDeviceInput initWithDevice:error:]`, the designated initializer
/// behind the convenience constructor above. Same answer, same reason.
unsafe extern "C" fn init_with_device(
    receiver: Id,
    _sel: Sel,
    _device: Id,
    error_out: *mut Id,
) -> Id {
    objc::arc::release(receiver);
    // SAFETY: as above.
    unsafe {
        report(
            error_out,
            DOMAIN,
            ERROR_DEVICE_NOT_CONNECTED,
            "no capture device is connected",
        )
    };
    std::ptr::null_mut()
}

fn device_input() {
    let class = corefoundation::base::abstract_class(c"AVCaptureDeviceInput", None);
    class_methods!(class, {
        "deviceInputWithDevice:error:" => device_input_with_device, c"@32@0:8@16^@24";
    });
    methods!(class, {
        "initWithDevice:error:" => init_with_device, c"@32@0:8@16^@24";
    });
}

// -- AVCaptureSession -----------------------------------------------------

/// A session's state: nothing more than whether it has been told to run, and
/// how many inputs/outputs it has accepted. Real `AVCaptureSession` tracks far
/// more, but with no device ever attached there is nothing for the rest to
/// act on.
#[derive(Default)]
struct Session {
    running: bool,
}

fn session_class() -> &'static objc::class::Class {
    foundation::boxed::register::<Session>(c"AVCaptureSession", None)
}

unsafe fn session_of<'a>(receiver: Id) -> Option<&'a mut Session> {
    unsafe { foundation::boxed::contents(receiver, session_class()) }
}

/// `-[AVCaptureSession init]`: a guest subclass reaches the state through
/// `[super init]`, as `NSOperation`'s does in `foundation`.
unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an instance of a class registered for `Session`.
    unsafe { foundation::boxed::fill(receiver, Session::default()) };
    receiver
}

/// `-canAddInput:`/`-canAddOutput:`: always yes. The only reason a real
/// session refuses one is a conflict with hardware already in use, and there
/// is none to conflict with.
unsafe extern "C" fn can_add(_receiver: Id, _sel: Sel, _port: Id) -> u8 {
    1
}

unsafe extern "C" fn add_input(_receiver: Id, _sel: Sel, _input: Id) {}
unsafe extern "C" fn add_output(_receiver: Id, _sel: Sel, _output: Id) {}
unsafe extern "C" fn remove_input(_receiver: Id, _sel: Sel, _input: Id) {}
unsafe extern "C" fn remove_output(_receiver: Id, _sel: Sel, _output: Id) {}
unsafe extern "C" fn begin_configuration(_receiver: Id, _sel: Sel) {}
unsafe extern "C" fn commit_configuration(_receiver: Id, _sel: Sel) {}

/// `-startRunning`: a session with no live input runs and delivers nothing,
/// which is exactly this runtime's situation, not a special case of it.
unsafe extern "C" fn start_running(receiver: Id, _sel: Sel) {
    // SAFETY: a live session.
    if let Some(session) = unsafe { session_of(receiver) } {
        session.running = true;
    }
}

unsafe extern "C" fn stop_running(receiver: Id, _sel: Sel) {
    // SAFETY: a live session.
    if let Some(session) = unsafe { session_of(receiver) } {
        session.running = false;
    }
}

unsafe extern "C" fn is_running(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a live session.
    unsafe { session_of(receiver) }.is_some_and(|s| s.running) as u8
}

fn session() {
    let class = session_class();
    methods!(class, {
        "init" => init, c"@16@0:8";
        "canAddInput:" => can_add, c"c24@0:8@16";
        "canAddOutput:" => can_add, c"c24@0:8@16";
        "addInput:" => add_input, c"v24@0:8@16";
        "addOutput:" => add_output, c"v24@0:8@16";
        "removeInput:" => remove_input, c"v24@0:8@16";
        "removeOutput:" => remove_output, c"v24@0:8@16";
        "beginConfiguration" => begin_configuration, c"v16@0:8";
        "commitConfiguration" => commit_configuration, c"v16@0:8";
        "startRunning" => start_running, c"v16@0:8";
        "stopRunning" => stop_running, c"v16@0:8";
        "isRunning" => is_running, c"c16@0:8";
    });
}

// -- AVCaptureVideoDataOutput ---------------------------------------------

/// A video data output's state: a delegate it will in fact never call back,
/// since no session it is attached to ever has a running input. Retained the
/// way a real output retains it, so `-sampleBufferDelegate` answers what was
/// set.
#[derive(Default)]
struct VideoDataOutput {
    delegate: Id,
}

impl Drop for VideoDataOutput {
    fn drop(&mut self) {
        objc::arc::release(self.delegate);
    }
}

fn video_data_output_class() -> &'static objc::class::Class {
    foundation::boxed::register::<VideoDataOutput>(c"AVCaptureVideoDataOutput", None)
}

unsafe fn video_data_output_of<'a>(receiver: Id) -> Option<&'a mut VideoDataOutput> {
    unsafe { foundation::boxed::contents(receiver, video_data_output_class()) }
}

unsafe extern "C" fn video_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an instance of a class registered for `VideoDataOutput`.
    unsafe { foundation::boxed::fill(receiver, VideoDataOutput::default()) };
    receiver
}

/// `-setSampleBufferDelegate:queue:`. The queue is not kept: nothing is ever
/// scheduled onto it, since no sample buffer is ever produced to hand the
/// delegate.
unsafe extern "C" fn set_sample_buffer_delegate(receiver: Id, _sel: Sel, delegate: Id, _queue: Id) {
    // SAFETY: a live output.
    let Some(output) = (unsafe { video_data_output_of(receiver) }) else {
        return;
    };
    objc::arc::release(output.delegate);
    output.delegate = objc::arc::retain(delegate);
}

unsafe extern "C" fn sample_buffer_delegate(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a live output.
    unsafe { video_data_output_of(receiver) }.map_or(std::ptr::null_mut(), |o| o.delegate)
}

/// `-availableVideoCVPixelFormatTypes`: empty, since no format this runtime
/// can actually hand frames in is ever available with no camera behind it.
unsafe extern "C" fn available_pixel_format_types(_receiver: Id, _sel: Sel) -> Id {
    let array = corefoundation::collections::new_array(
        &[],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    objc::arc::autorelease(array as Id)
}

fn video_data_output() {
    let class = video_data_output_class();
    methods!(class, {
        "init" => video_init, c"@16@0:8";
        "setSampleBufferDelegate:queue:" => set_sample_buffer_delegate, c"v32@0:8@16@24";
        "sampleBufferDelegate" => sample_buffer_delegate, c"@16@0:8";
        "availableVideoCVPixelFormatTypes" => available_pixel_format_types, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_session_reports_no_devices() {
        crate::bootstrap();
        let class = objc::class::lookup(c"AVCaptureDeviceDiscoverySession").expect("registered");
        let implementation: unsafe extern "C" fn(Id, Sel) -> Id = unsafe {
            std::mem::transmute(
                class
                    .method(objc::sel::get("devices"))
                    .unwrap()
                    .implementation(),
            )
        };
        let pool = objc::arc::pool_push();
        let array = unsafe { implementation(std::ptr::null_mut(), objc::sel::get("devices")) };
        let contents = unsafe { corefoundation::collections::array(array as _) }.expect("array");
        assert!(contents.values.is_empty());
        objc::arc::pool_pop(pool);
    }

    #[test]
    fn device_input_always_fails() {
        crate::bootstrap();
        let class = objc::class::lookup(c"AVCaptureDeviceInput").expect("registered");
        let implementation: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id = unsafe {
            std::mem::transmute(
                class
                    .metaclass()
                    .method(objc::sel::get("deviceInputWithDevice:error:"))
                    .unwrap()
                    .implementation(),
            )
        };
        let mut error: Id = std::ptr::null_mut();
        let pool = objc::arc::pool_push();
        let result = unsafe {
            implementation(
                std::ptr::null_mut(),
                objc::sel::get("deviceInputWithDevice:error:"),
                std::ptr::null_mut(),
                &mut error,
            )
        };
        assert!(result.is_null());
        assert!(!error.is_null(), "a failure reports an NSError");
        objc::arc::pool_pop(pool);
    }

    #[test]
    fn a_session_with_no_input_runs_harmlessly() {
        crate::bootstrap();
        let class = session_class();
        let object: Id = objc::object::create_instance(class, 0);
        unsafe { foundation::boxed::fill(object, Session::default()) };

        let can_add_input: unsafe extern "C" fn(Id, Sel, Id) -> u8 = unsafe {
            std::mem::transmute(
                class
                    .method(objc::sel::get("canAddInput:"))
                    .unwrap()
                    .implementation(),
            )
        };
        assert_eq!(
            unsafe { can_add_input(object, objc::sel::get("canAddInput:"), std::ptr::null_mut()) },
            1
        );

        let start: unsafe extern "C" fn(Id, Sel) = unsafe {
            std::mem::transmute(
                class
                    .method(objc::sel::get("startRunning"))
                    .unwrap()
                    .implementation(),
            )
        };
        let running: unsafe extern "C" fn(Id, Sel) -> u8 = unsafe {
            std::mem::transmute(
                class
                    .method(objc::sel::get("isRunning"))
                    .unwrap()
                    .implementation(),
            )
        };
        unsafe { start(object, objc::sel::get("startRunning")) };
        assert_eq!(unsafe { running(object, objc::sel::get("isRunning")) }, 1);

        objc::arc::release(object);
    }
}
