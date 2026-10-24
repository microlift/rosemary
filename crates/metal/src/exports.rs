use super::*;

pub(super) fn devices() -> &'static [usize] {
    static DEVICES: OnceLock<Vec<usize>> = OnceLock::new();
    DEVICES.get_or_init(|| {
        bootstrap();
        let adapters = match vulkan::adapters() {
            Ok(adapters) => adapters,
            Err(reason) => {
                eprintln!("rosemary: Metal: {reason}");
                return Vec::new();
            }
        };
        adapters
            .iter()
            .enumerate()
            .map(|(ordinal, adapter)| {
                let gpu = match adapter.gpu() {
                    Ok(gpu) => Some(gpu),
                    Err(reason) => {
                        eprintln!(
                            "rosemary: Metal: {} has no logical device: {reason}",
                            adapter.name
                        );
                        None
                    }
                };
                object::make(
                    c"RosemaryMTLDevice",
                    Device {
                        ordinal: ordinal as u64 + 1,
                        name: foundation::string::from_str(&adapter.name),
                        recommended_working_set: adapter.recommended_working_set,
                        target: &adapter.target,
                        gpu,
                    },
                ) as usize
            })
            .filter(|device| *device != 0)
            .collect()
    })
}

/// Return the first Vulkan-backed Metal device, or nil when Vulkan is unavailable.
///
/// # Safety
///
/// Called through the Metal framework ABI.
pub unsafe extern "C" fn MTLCreateSystemDefaultDevice() -> Id {
    devices()
        .first()
        .copied()
        .map_or(std::ptr::null_mut(), |device| device as Id)
}

/// Return a retained array containing every Vulkan-backed Metal device.
///
/// # Safety
///
/// Called through the Metal framework ABI.
pub unsafe extern "C" fn MTLCopyAllDevices() -> Id {
    bootstrap();
    let values = devices()
        .iter()
        .map(|device| *device as CFTypeRef)
        .collect::<Vec<_>>();
    corefoundation::collections::new_array(
        &values,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    ) as Id
}

/// Every observer `MTLCopyAllDevicesWithObserver` made and
/// `MTLRemoveDeviceObserver` has not yet removed, each retained here as the
/// notification center retains the observers it hands out.
static OBSERVERS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// Return the devices, as `MTLCopyAllDevices` does, and in `observer` an
/// autoreleased object holding `handler` until `MTLRemoveDeviceObserver`.
///
/// The handler is never called: the devices are the Vulkan adapters found
/// once per process, so none is added to that list or removed from it.
///
/// # Safety
///
/// Called through the Metal framework ABI.
pub unsafe extern "C" fn MTLCopyAllDevicesWithObserver(observer: *mut Id, handler: Id) -> Id {
    bootstrap();
    if !observer.is_null() {
        let handler = match handler.is_null() {
            true => std::ptr::null_mut(),
            false => objc::block::copy(handler),
        };
        let made = object::make(c"RosemaryMTLDeviceObserver", handler);
        OBSERVERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(objc::arc::retain(made) as usize);
        // SAFETY: the caller's out-parameter.
        unsafe { *observer = objc::arc::autorelease(made) };
    }
    // SAFETY: called through the framework ABI.
    unsafe { MTLCopyAllDevices() }
}

/// Stop observing: drop the reference `MTLCopyAllDevicesWithObserver` kept.
/// An object it did not make, or one already removed, is ignored.
///
/// # Safety
///
/// Called through the Metal framework ABI.
pub unsafe extern "C" fn MTLRemoveDeviceObserver(observer: Id) {
    let removed = {
        let mut observers = OBSERVERS.lock().unwrap_or_else(|e| e.into_inner());
        let at = observers.iter().position(|o| *o == observer as usize);
        at.map(|at| observers.swap_remove(at))
    };
    if let Some(observer) = removed {
        objc::arc::release(observer as Id);
    }
}

pub(super) unsafe extern "C" fn observer_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: an observer of this module's; its handler was copied when made.
    let handler = *unsafe { object::body::<Id>(receiver) };
    if !handler.is_null() {
        objc::block::release(handler);
    }
    objc::arc::finalize(receiver);
}

/// Return a retained device for the requested display.
///
/// # Safety
///
/// Called through the CoreGraphics framework ABI.
pub unsafe extern "C" fn CGDirectDisplayCopyCurrentMetalDevice(_display: u32) -> Id {
    // SAFETY: called through the framework ABI.
    let device = unsafe { MTLCreateSystemDefaultDevice() };
    objc::arc::retain(device)
}

pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    [
        "MTLTextureDescriptor",
        "MTLVertexDescriptor",
        "MTLSamplerDescriptor",
        "MTLHeapDescriptor",
        "MTLDepthStencilDescriptor",
        "MTLStencilDescriptor",
        "MTLRenderPipelineDescriptor",
        "MTLRenderPassDescriptor",
        "MTLCompileOptions",
    ]
    .into_iter()
    .filter_map(|name| {
        let name_c = std::ffi::CString::new(name).ok()?;
        let class = objc::class::lookup(&name_c)?;
        Some((
            format!("_OBJC_CLASS_$_{name}"),
            objc::class::ptr(class) as usize as u64,
        ))
    })
    .collect()
}

/// Metal's entry points.
///
/// The crate has a module of this name too, so the macro is spelled absolutely.
mod entries {
    use super::{
        CGDirectDisplayCopyCurrentMetalDevice, MTLCopyAllDevices, MTLCopyAllDevicesWithObserver,
        MTLCreateSystemDefaultDevice, MTLRemoveDeviceObserver,
    };

    ::exports::exports! {
        functions {
            "MTLCreateSystemDefaultDevice" => MTLCreateSystemDefaultDevice,
            "MTLCopyAllDevices" => MTLCopyAllDevices,
            "MTLCopyAllDevicesWithObserver" => MTLCopyAllDevicesWithObserver,
            "MTLRemoveDeviceObserver" => MTLRemoveDeviceObserver,
            "CGDirectDisplayCopyCurrentMetalDevice" => CGDirectDisplayCopyCurrentMetalDevice,
        }
    }
}

#[allow(unused_imports)]
pub use entries::{MODULE, names, provide};

/// The address `name` has here, in either spelling.
///
/// Both, unlike every other crate's: a caller that has not stripped the Mach-O
/// underscore gets the same answer as one that has.
pub fn symbol(name: &str) -> Option<u64> {
    entries::symbol(name.trim_start_matches('_'))
}
