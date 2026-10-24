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
    use super::{MTLCopyAllDevices, MTLCreateSystemDefaultDevice};

    ::exports::exports! {
        functions {
            "MTLCreateSystemDefaultDevice" => MTLCreateSystemDefaultDevice,
            "MTLCopyAllDevices" => MTLCopyAllDevices,
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
