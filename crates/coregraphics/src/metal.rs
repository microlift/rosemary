//! The Metal device a display is attached to.

#![allow(non_snake_case)]

use std::sync::OnceLock;

use objc::object::Id;

/// What the Metal backend provides: the system's default device.
pub trait Provider: Send + Sync {
    fn current_device(&self) -> Id;
}

static PROVIDER: OnceLock<Box<dyn Provider>> = OnceLock::new();

/// Install the provider the display-to-device call asks.
pub fn set_provider(provider: Box<dyn Provider>) {
    let _ = PROVIDER.set(provider);
}

/// Return a retained device for the requested display.
///
/// # Safety
///
/// Called through the CoreGraphics framework ABI.
pub unsafe extern "C" fn CGDirectDisplayCopyCurrentMetalDevice(_display: u32) -> Id {
    let Some(provider) = PROVIDER.get() else {
        return std::ptr::null_mut();
    };
    let device = provider.current_device();
    objc::arc::retain(device)
}

exports::exports! {
    functions {
        "CGDirectDisplayCopyCurrentMetalDevice" => CGDirectDisplayCopyCurrentMetalDevice,
    }
}
