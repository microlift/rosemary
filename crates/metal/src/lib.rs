#![allow(non_snake_case)]

mod capture;
mod completion;
mod constants;
mod device;
mod encoder;
mod exports;
mod gpu;
mod heap;
mod library;
mod object;
mod overlay;
mod pipeline;
mod present;
mod profile;
mod resource;
pub mod stats;
mod submission;
mod swapchain;
mod vulkan;

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use corefoundation::base::CFTypeRef;
use objc::object::Id;
use objc::sel::Sel;

use constants::*;
use device::*;
use gpu::Gpu;
use library::*;
use object::methods;

pub use exports::{
    CGDirectDisplayCopyCurrentMetalDevice, MTLCopyAllDevices, MTLCopyAllDevicesWithObserver,
    MTLCreateSystemDefaultDevice, MTLRemoveDeviceObserver, data_symbols, names, provide, symbol,
};

struct Device {
    ordinal: u64,
    name: Id,
    recommended_working_set: u64,
    target: *const metal_translate::TargetProfile,
    /// `None` when the adapter has no logical device: every resource entry
    /// point then fails through the Metal API rather than pretending.
    gpu: Option<&'static Gpu>,
}

struct Library {
    translated: metal_translate::TranslatedLibrary,
    /// The AIR modules the library was made from, kept only where one declares
    /// function constants: each set of values translates its owning module.
    air: Option<Vec<Vec<u8>>>,
    /// Owning AIR module for each translated entry point.
    entry_modules: Vec<usize>,
    /// The profile of the device which made the library.
    target: Option<&'static metal_translate::TargetProfile>,
    /// The device which made the library. Devices live as long as the
    /// process, so it is not retained.
    device: Id,
}

struct Function {
    library: Id,
    entry: usize,
    /// The entry point translated for the function constants this function
    /// was made with, in place of the library's own.
    specialized: Option<metal_translate::EntryArtifact>,
}

impl Function {
    /// The translated entry point this function runs.
    ///
    /// # Safety
    ///
    /// The function's library must be alive, which it is for as long as the
    /// function holds it.
    unsafe fn artifact(&self) -> Option<&'static metal_translate::EntryArtifact> {
        if let Some(artifact) = &self.specialized {
            // SAFETY: the artifact lives in this function's body, which
            // outlives every pipeline made from it.
            return Some(unsafe { &*(artifact as *const metal_translate::EntryArtifact) });
        }
        let library = unsafe { object::body::<Library>(self.library) };
        library.translated.entries.get(self.entry)
    }
}

/// `MTLFunctionConstantValues`: the values a guest specializes a function
/// with.
struct ConstantValues {
    values: metal_translate::FunctionConstantValues,
}

static ALLOCATED_SIZE: AtomicU64 = AtomicU64::new(0);
/// Bytes of live shared and managed `MTLBuffer`s, for `ROSEMARY_METAL_STATS`.
static SHARED_BUFFER_BYTES: AtomicU64 = AtomicU64::new(0);

pub(crate) fn add_shared_buffer_bytes(amount: u64) {
    if stats::enabled() {
        SHARED_BUFFER_BYTES.fetch_add(amount, Ordering::Relaxed);
    }
}

pub(crate) fn sub_shared_buffer_bytes(amount: u64) {
    if stats::enabled() {
        SHARED_BUFFER_BYTES.fetch_sub(amount, Ordering::Relaxed);
    }
}

/// Diagnostics [`report_library_error`] has already printed.
///
/// A rejected library is reported by the shape it was rejected for rather than
/// once per library: a game loads thousands, and the same gap in the translator
/// accounts for a run of them. Keyed by the message so that a run collapses to
/// one line and a new gap is still visible.
static REPORTED_LIBRARY_ERRORS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
static REPORTED_SOURCE_LIBRARY: AtomicBool = AtomicBool::new(false);

/// Add to, or with a negative amount subtract from, what
/// `-[MTLDevice currentAllocatedSize]` reports.
pub(crate) fn account(bytes: i64) {
    match bytes < 0 {
        true => ALLOCATED_SIZE.fetch_sub(bytes.unsigned_abs(), Ordering::Relaxed),
        false => ALLOCATED_SIZE.fetch_add(bytes as u64, Ordering::Relaxed),
    };
}

/// The number of live `MTLBuffer`s after this change, for the buffer trace.
pub(crate) fn live_buffers(delta: i64) -> i64 {
    static LIVE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    LIVE.fetch_add(delta, Ordering::Relaxed) + delta
}

pub(crate) unsafe extern "C" fn no_op(_receiver: Id, _sel: Sel) {}

pub(crate) unsafe extern "C" fn zero(_receiver: Id, _sel: Sel) -> u64 {
    0
}

/// The logical device behind an `MTLDevice`, or `None` when Vulkan gave none.
fn gpu_of(receiver: Id) -> Option<&'static Gpu> {
    // SAFETY: every caller is a method registered on `RosemaryMTLDevice`.
    unsafe { object::body::<Device>(receiver) }.gpu
}

/// As `gpu_of`, for a caller holding an object which may not be a device.
pub(crate) fn gpu_of_device(device: Id) -> Option<&'static Gpu> {
    object::is_kind(device, c"RosemaryMTLDevice").then(|| gpu_of(device))?
}

pub fn bootstrap() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        foundation::bootstrap();
        let class = object::register::<Device>(c"RosemaryMTLDevice");
        let library_class = object::register::<Library>(c"RosemaryMTLLibrary");
        let function_class = object::register::<Function>(c"RosemaryMTLFunction");
        let constant_values_class =
            object::register::<ConstantValues>(c"MTLFunctionConstantValues");
        object::register::<pipeline::ComputePipeline>(c"RosemaryMTLComputePipelineState");
        object::register::<pipeline::RenderPipeline>(c"RosemaryMTLRenderPipelineState");

        methods!(class, {
            "name" => name, c"@16@0:8";
            "registryID" => registry_id, c"Q16@0:8";
            "isLowPower" => no, c"B16@0:8";
            "isHeadless" => no, c"B16@0:8";
            "isRemovable" => no, c"B16@0:8";
            "hasUnifiedMemory" => no, c"B16@0:8";
            "isDepth24Stencil8PixelFormatSupported" => no, c"B16@0:8";
            "recommendedMaxWorkingSetSize" => recommended_working_set, c"Q16@0:8";
            "maxBufferLength" => max_buffer_length, c"Q16@0:8";
            "maxThreadgroupMemoryLength" => max_threadgroup_memory, c"Q16@0:8";
            "supportsFeatureSet:" => supports_feature_set, c"B24@0:8Q16";
            "supportsFamily:" => supports_family, c"B24@0:8Q16";
            "argumentBuffersSupport" => argument_buffers_tier, c"Q16@0:8";
            "argumentBuffersTier" => argument_buffers_tier, c"Q16@0:8";
            "readWriteTextureSupport" => read_write_texture_tier, c"Q16@0:8";
            "supportsShaderBarycentricCoordinates" => no, c"B16@0:8";
            "areBarycentricCoordsSupported" => no, c"B16@0:8";
            "supportsRaytracing" => no, c"B16@0:8";
            "supportsFunctionPointers" => no, c"B16@0:8";
            "supportsDynamicLibraries" => no, c"B16@0:8";
            "supports32BitFloatFiltering" => yes, c"B16@0:8";
            "supportsBCTextureCompression" => supports_bc, c"B16@0:8";
            "supportsPullModelInterpolation" => no, c"B16@0:8";
            "supportsTextureSampleCount:" => supports_sample_count, c"B24@0:8Q16";
            "newCommandQueue" => new_command_queue, c"@16@0:8";
            "newCommandQueueWithMaxCommandBufferCount:" => new_command_queue_with_count,
                c"@24@0:8Q16";
            "newBufferWithLength:options:" => new_buffer, c"@32@0:8Q16Q24";
            "newBufferWithBytes:length:options:" => new_buffer_with_bytes, c"@40@0:8^v16Q24Q32";
            "newTextureWithDescriptor:" => new_texture, c"@24@0:8@16";
            "newSamplerStateWithDescriptor:" => new_sampler_state, c"@24@0:8@16";
            "newDepthStencilStateWithDescriptor:" => new_depth_stencil_state, c"@24@0:8@16";
            "newDefaultLibrary" => new_default_library, c"@16@0:8";
            "newLibraryWithData:error:" => new_library_with_data, c"@32@0:8@16^@24";
            "newLibraryWithSource:options:error:" => new_library_with_source, c"@40@0:8@16@24^@32";
            "newComputePipelineStateWithDescriptor:options:reflection:error:"
                => pipeline::new_compute_pipeline_with_descriptor, c"@48@0:8@16Q24^@32^@40";
            "newComputePipelineStateWithFunction:options:reflection:error:"
                => pipeline::new_compute_pipeline_with_reflection, c"@48@0:8@16Q24^@32^@40";
            "newComputePipelineStateWithFunction:error:" => new_compute_pipeline,
                c"@32@0:8@16^@24";
            "newRenderPipelineStateWithDescriptor:error:" => new_render_pipeline,
                c"@32@0:8@16^@24";
            "currentAllocatedSize" => current_allocated_size, c"Q16@0:8";
            "newFence" => new_fence, c"@16@0:8";
            "newHeapWithDescriptor:" => new_heap, c"@24@0:8@16";
            "heapBufferSizeAndAlignWithLength:options:" => resource::heap_buffer_size_and_align,
                c"{MTLSizeAndAlign=QQ}32@0:8Q16Q24";
            "heapTextureSizeAndAlignWithDescriptor:" => resource::heap_texture_size_and_align,
                c"{MTLSizeAndAlign=QQ}24@0:8@16";
        });
        // Encoders on one device run in order behind full barriers, so a fence
        // has nothing to carry: it is an object to pass back and nothing more.
        let observer = object::register::<Id>(c"RosemaryMTLDeviceObserver");
        methods!(observer, {
            "dealloc" => exports::observer_dealloc, c"v16@0:8";
        });
        let fence = object::register::<Id>(c"RosemaryMTLFence");
        methods!(fence, {
            "device" => fence_device, c"@16@0:8";
            "dealloc" => fence_dealloc, c"v16@0:8";
        });
        methods!(library_class, {
            "dealloc" => library_dealloc, c"v16@0:8";
            "newFunctionWithName:" => new_function_with_name, c"@24@0:8@16";
            "newFunctionWithName:constantValues:error:" => new_function_with_constants,
                c"@40@0:8@16@24^@32";
            "functionNames" => library_function_names, c"@16@0:8";
            "device" => library_device, c"@16@0:8";
            "setLabel:" => no_op, c"v24@0:8@16";
        });
        let attribute =
            object::register::<VertexAttributeReflection>(c"RosemaryMTLVertexAttribute");
        methods!(attribute, {
            "name" => attribute_name, c"@16@0:8";
            "attributeIndex" => attribute_index, c"Q16@0:8";
            "attributeType" => attribute_type, c"Q16@0:8";
            "isActive" => yes, c"B16@0:8";
            "isPatchData" => no, c"B16@0:8";
            "isPatchControlPointData" => no, c"B16@0:8";
            "dealloc" => attribute_dealloc, c"v16@0:8";
        });
        methods!(function_class, {
            "dealloc" => function_dealloc, c"v16@0:8";
            "name" => function_name, c"@16@0:8";
            "functionType" => function_type, c"Q16@0:8";
            "vertexAttributes" => function_vertex_attributes, c"@16@0:8";
            "stageInputAttributes" => function_vertex_attributes, c"@16@0:8";
            "patchType" => zero, c"Q16@0:8";
            "patchControlPointCount" => zero, c"q16@0:8";
            "device" => function_device, c"@16@0:8";
            "setLabel:" => no_op, c"v24@0:8@16";
        });
        methods!(constant_values_class, {
            "init" => constant_values_init, c"@16@0:8";
            "reset" => constant_values_reset, c"v16@0:8";
            "dealloc" => constant_values_dealloc, c"v16@0:8";
            "setConstantValue:type:atIndex:" => set_constant_at_index, c"v40@0:8r^v16Q24Q32";
            "setConstantValue:type:withName:" => set_constant_with_name, c"v40@0:8r^v16Q24@32";
            "setConstantValues:type:withRange:" => set_constants_in_range,
                c"v48@0:8r^v16Q24{_NSRange=QQ}32";
        });

        resource::install();
        encoder::install();
        pipeline::install();
        present::install();
        install_labels();
    });
}

/// Every Metal object and descriptor answers `-label` and `-setLabel:`. A
/// label is a debugging aid nothing here displays, so it is not kept.
fn install_labels() {
    let names: [&std::ffi::CStr; 25] = [
        c"RosemaryMTLFence",
        c"MTLHeapDescriptor",
        c"RosemaryMTLHeap",
        c"MTLDepthStencilDescriptor",
        c"MTLRenderPassDescriptor",
        c"MTLRenderPipelineDescriptor",
        c"MTLSamplerDescriptor",
        c"MTLStencilDescriptor",
        c"MTLTextureDescriptor",
        c"MTLVertexDescriptor",
        c"RosemaryMTLBuffer",
        c"RosemaryMTLColorAttachmentDescriptor",
        c"RosemaryMTLCommandBuffer",
        c"RosemaryMTLCommandEncoder",
        c"RosemaryMTLCommandQueue",
        c"RosemaryMTLComputePipelineState",
        c"RosemaryMTLDepthStencilState",
        c"RosemaryMTLDevice",
        c"RosemaryMTLFunction",
        c"RosemaryMTLLibrary",
        c"RosemaryMTLRenderPassAttachmentDescriptor",
        c"RosemaryMTLRenderPipelineState",
        c"RosemaryMTLSamplerState",
        c"RosemaryMTLTexture",
        c"RosemaryMTLVertexElement",
    ];
    for name in names {
        let Some(class) = objc::class::lookup(name) else {
            continue;
        };
        methods!(class, {
            "label" => nil, c"@16@0:8";
            "setLabel:" => no_op, c"v24@0:8@16";
        });
    }
}

unsafe extern "C" fn new_fence(receiver: Id, _sel: Sel) -> Id {
    object::make(c"RosemaryMTLFence", objc::arc::retain(receiver))
}

unsafe extern "C" fn fence_device(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a fence of this crate's.
    *unsafe { object::body::<Id>(receiver) }
}

unsafe extern "C" fn fence_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a fence of this crate's; its device was retained when it was made.
    objc::arc::release(*unsafe { object::body::<Id>(receiver) });
    objc::arc::finalize(receiver);
}

pub(crate) unsafe extern "C" fn nil(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

#[cfg(test)]
mod tests;
