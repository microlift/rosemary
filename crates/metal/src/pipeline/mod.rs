mod compute;
mod compute_descriptor;
pub(crate) use compute_descriptor::{
    new_with_descriptor as new_compute_pipeline_with_descriptor,
    new_with_function_reflection as new_compute_pipeline_with_reflection,
};
mod descriptor;
mod enums;
mod link;
mod render;

use std::collections::HashMap;
use std::sync::Mutex;

use ash::vk;
use metal_translate::ir::{ResourceKind, Stage};
use metal_translate::{EntryArtifact, ResourceBinding};

use crate::gpu::{self, DepthResolve, Gpu, RenderPassKey};
use crate::object::{self, methods};
use crate::resource::{DepthStencilState, VERTEX_SLOTS, VertexAttribute, VertexLayout, compare_op};
use objc::object::Id;
use objc::sel::Sel;

use compute::*;
use descriptor::*;
use enums::*;
use render::render_pipeline_dealloc;

pub(crate) use compute::ComputePipeline;
pub(crate) use render::{
    COLOR_ATTACHMENTS, ColorAttachment, DepthStencilKey, RenderPipeline, RenderPipelineDescriptor,
    RenderVariantKey,
};

/// The four descriptor sets of the Metal resource ABI.
pub(crate) const SETS: usize = 4;

pub(crate) struct Layouts {
    pub sets: [vk::DescriptorSetLayout; SETS],
    pub pipeline: vk::PipelineLayout,
    pub push_set: Option<usize>,
    /// The binding numbers of the buffer set (0) declared as dynamic storage
    /// buffers, in ascending order - the order Vulkan takes their offsets in.
    pub dynamic: Vec<u32>,
}

/// The buffer set's index in the Metal resource ABI.
pub(crate) const BUFFER_SET: usize = 0;

impl Layouts {
    /// Give back what one pipeline state owns here.
    ///
    /// The pipeline layout only: the four set layouts live in the device's
    /// cache, shared by every pipeline with the same resource shape, and stay
    /// for the life of the device.
    pub(super) fn destroy(&self, gpu: &Gpu) {
        // SAFETY: the last reference to the pipeline state is gone, and every
        // command buffer which bound it retained it until its work retired.
        unsafe { gpu.device.destroy_pipeline_layout(self.pipeline, None) };
    }
}

/// The teardown a pipeline state's `dealloc` performs, shared by the compute
/// and render forms because the Vulkan objects are the same shape.
pub(super) fn destroy_variants(gpu: &Gpu, variants: impl IntoIterator<Item = vk::Pipeline>) {
    for pipeline in variants {
        unsafe { gpu.device.destroy_pipeline(pipeline, None) };
    }
}

pub(crate) fn descriptor_type(binding: &ResourceBinding) -> Option<vk::DescriptorType> {
    match binding.kind {
        ResourceKind::Buffer => Some(vk::DescriptorType::STORAGE_BUFFER),
        ResourceKind::Texture => Some(match (binding.texel_buffer, binding.sampled) {
            (true, true) => vk::DescriptorType::UNIFORM_TEXEL_BUFFER,
            (true, false) => vk::DescriptorType::STORAGE_TEXEL_BUFFER,
            (false, true) => vk::DescriptorType::SAMPLED_IMAGE,
            (false, false) => vk::DescriptorType::STORAGE_IMAGE,
        }),
        ResourceKind::Sampler => Some(vk::DescriptorType::SAMPLER),
        _ => None,
    }
}

fn stage_flags(stage: Stage) -> vk::ShaderStageFlags {
    match stage {
        Stage::Vertex => vk::ShaderStageFlags::VERTEX,
        Stage::Fragment => vk::ShaderStageFlags::FRAGMENT,
        _ => vk::ShaderStageFlags::COMPUTE,
    }
}

/// Build the four descriptor-set layouts and the pipeline layout for the
/// resources of one or two entry points.
pub(crate) fn layouts_for(gpu: &Gpu, resources: &[&[ResourceBinding]]) -> Result<Layouts, String> {
    let mut per_set: [Vec<(u32, vk::DescriptorType)>; SETS] = Default::default();
    let mut stages = vk::ShaderStageFlags::empty();
    for group in resources {
        for binding in *group {
            stages |= stage_flags(binding.stage);
            let Some(kind) = descriptor_type(binding) else {
                return Err(format!(
                    "resource kind {:?} has no Vulkan descriptor type",
                    binding.kind
                ));
            };
            let set = binding.descriptor_set as usize;
            if set >= SETS {
                return Err(format!("descriptor set {set} is outside the Metal ABI"));
            }
            per_set[set].push((binding.binding, kind));
        }
    }
    let push_set = choose_push_set(
        per_set.each_ref().map(Vec::len),
        gpu.max_push_descriptors as usize,
    );
    let dynamic = match push_set == Some(BUFFER_SET) {
        true => Vec::new(),
        false => dynamic_bindings(
            &mut per_set[BUFFER_SET],
            gpu.limits.max_descriptor_set_storage_buffers_dynamic as usize,
        ),
    };
    let mut sets = [vk::DescriptorSetLayout::null(); SETS];
    for (index, bindings) in per_set.iter().enumerate() {
        sets[index] = gpu.descriptor_set_layout(bindings, stages, push_set == Some(index))?;
    }
    let info = vk::PipelineLayoutCreateInfo::default().set_layouts(&sets);
    // SAFETY: the layouts live in the device's cache for its lifetime.
    let pipeline = unsafe { gpu.device.create_pipeline_layout(&info, None) }
        .map_err(|error| format!("vkCreatePipelineLayout failed with {error:?}"))?;
    Ok(Layouts {
        sets,
        pipeline,
        push_set,
        dynamic,
    })
}

/// Make the buffer set's storage buffers dynamic, as many as `limit` allows,
/// lowest binding first, and answer which.
///
/// A `-setBytes:` copy lands at a new offset of an upload chunk on every
/// call, so a set holding it is a fresh allocation and write on every draw.
/// As a dynamic buffer the descriptor names the chunk, which stays the same,
/// and the offset is handed over when the set is bound. A pushed set needs
/// none of this - pushing is the write - and cannot hold dynamic descriptors.
fn dynamic_bindings(bindings: &mut [(u32, vk::DescriptorType)], limit: usize) -> Vec<u32> {
    let mut dynamic = bindings
        .iter()
        .filter(|(_, kind)| *kind == vk::DescriptorType::STORAGE_BUFFER)
        .map(|(binding, _)| *binding)
        .collect::<Vec<_>>();
    dynamic.sort_unstable();
    dynamic.dedup();
    dynamic.truncate(limit);
    for (binding, kind) in bindings {
        if dynamic.binary_search(binding).is_ok() {
            *kind = vk::DescriptorType::STORAGE_BUFFER_DYNAMIC;
        }
    }
    dynamic
}

/// The descriptor type one binding has under `layouts`.
pub(crate) fn binding_type(
    binding: &ResourceBinding,
    layouts: &Layouts,
) -> Option<vk::DescriptorType> {
    let kind = descriptor_type(binding)?;
    let dynamic = binding.descriptor_set as usize == BUFFER_SET
        && layouts.dynamic.binary_search(&binding.binding).is_ok();
    Some(match dynamic {
        true => vk::DescriptorType::STORAGE_BUFFER_DYNAMIC,
        false => kind,
    })
}

/// Pick one set the implementation can push. Vulkan permits only one push
/// layout per pipeline, and Metal buffers churn most often (`-setBytes:` also
/// lands there), so prefer that set. If it is too large, use the fullest set
/// which does fit rather than giving up on push descriptors altogether.
fn choose_push_set(counts: [usize; SETS], limit: usize) -> Option<usize> {
    if counts[0] != 0 && counts[0] <= limit {
        return Some(0);
    }
    let mut selected = None;
    let mut largest = 0;
    for (index, count) in counts.into_iter().enumerate().skip(1) {
        if count > largest && count <= limit {
            selected = Some(index);
            largest = count;
        }
    }
    selected
}

impl From<&DepthStencilState> for DepthStencilKey {
    fn from(state: &DepthStencilState) -> Self {
        let face = |face: &crate::resource::StencilState| {
            [
                face.compare,
                face.fail,
                face.depth_fail,
                face.pass,
                u64::from(face.read_mask),
                u64::from(face.write_mask),
            ]
        };
        Self {
            compare: state.depth_compare,
            write: state.depth_write,
            stencil: state.stencil_enabled,
            front: face(&state.front),
            back: face(&state.back),
        }
    }
}

/// `MTLCompileOptions`.
pub(crate) struct CompileOptions {
    pub macros: Vec<(String, String)>,
}

unsafe extern "C" fn compile_options_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a freshly allocated options object.
    unsafe { object::init(receiver, CompileOptions { macros: Vec::new() }) };
    receiver
}

unsafe extern "C" fn compile_options_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: an options object `-init` established.
    unsafe {
        std::ptr::drop_in_place(object::body::<CompileOptions>(receiver) as *mut CompileOptions)
    };
    objc::arc::finalize(receiver);
}

/// `-setPreprocessorMacros:`: a dictionary of names to strings or numbers,
/// kept as the text each defines.
unsafe extern "C" fn set_preprocessor_macros(receiver: Id, _sel: Sel, dictionary: Id) {
    use corefoundation::api::*;
    use corefoundation::base::CFTypeRef;
    let mut macros = Vec::new();
    if !dictionary.is_null() {
        // SAFETY: a guest dictionary.
        let count = unsafe { CFDictionaryGetCount(dictionary as CFTypeRef) }.max(0) as usize;
        let mut keys: Vec<CFTypeRef> = vec![std::ptr::null(); count];
        let mut values: Vec<CFTypeRef> = vec![std::ptr::null(); count];
        // SAFETY: both arrays hold `count` entries.
        unsafe {
            CFDictionaryGetKeysAndValues(
                dictionary as CFTypeRef,
                keys.as_mut_ptr(),
                values.as_mut_ptr(),
            )
        };
        for (key, value) in keys.into_iter().zip(values) {
            // SAFETY: a key of the guest's dictionary.
            let name = String::from_utf8_lossy(&unsafe { foundation::string::bytes(key as Id) })
                .into_owned();
            // SAFETY: a value of the guest's dictionary.
            let text = if unsafe { CFGetTypeID(value) == CFNumberGetTypeID() } {
                let mut number = 0f64;
                // SAFETY: a number, read as a double (kCFNumberDoubleType).
                unsafe { CFNumberGetValue(value, 13, (&raw mut number).cast()) };
                if number.fract() == 0.0 && number.abs() < 1e15 {
                    format!("{}", number as i64)
                } else {
                    format!("{number}")
                }
            } else {
                String::from_utf8_lossy(&unsafe { foundation::string::bytes(value as Id) })
                    .into_owned()
            };
            macros.push((name, text));
        }
    }
    // Sorted, so one set of macros is one shader cache key whatever order
    // the dictionary enumerates them in.
    macros.sort();
    // SAFETY: an options object `-init` established.
    unsafe { object::body::<CompileOptions>(receiver) }.macros = macros;
}

pub(crate) fn install() {
    compute_descriptor::install();
    let descriptor = object::register::<RenderPipelineDescriptor>(c"MTLRenderPipelineDescriptor");
    let color = object::register::<ColorElement>(c"RosemaryMTLColorAttachmentDescriptor");
    // Of `MTLCompileOptions` only the preprocessor macros change what a
    // library says. The frontend accepts one dialect whatever language
    // version is asked for, and fast math is the driver's choice.
    // `[[invariant]]` is always honored, which preserving invariance asks for
    // and not preserving it permits.
    let options = object::register::<CompileOptions>(c"MTLCompileOptions");
    methods!(options, {
        "init" => compile_options_init, c"@16@0:8";
        "dealloc" => compile_options_dealloc, c"v16@0:8";
        "setFastMathEnabled:" => crate::no_op, c"v20@0:8c16";
        "setPreserveInvariance:" => crate::no_op, c"v20@0:8c16";
        "setLanguageVersion:" => crate::no_op, c"v24@0:8Q16";
        "setPreprocessorMacros:" => set_preprocessor_macros, c"v24@0:8@16";
    });
    methods!(descriptor, {
        // Vulkan takes the topology from each draw, and no function here is
        // ever a post-tessellation vertex function, so tessellation state has
        // nothing to configure: a patch function is refused when it is made.
        "setInputPrimitiveTopology:" => crate::no_op, c"v24@0:8Q16";
        "setMaxTessellationFactor:" => crate::no_op, c"v24@0:8Q16";
        "setTessellationControlPointIndexType:" => crate::no_op, c"v24@0:8Q16";
        "setTessellationFactorFormat:" => crate::no_op, c"v24@0:8Q16";
        "setTessellationFactorScaleEnabled:" => crate::no_op, c"v20@0:8c16";
        "setTessellationFactorStepFunction:" => crate::no_op, c"v24@0:8Q16";
        "setTessellationOutputWindingOrder:" => crate::no_op, c"v24@0:8Q16";
        "setTessellationPartitionMode:" => crate::no_op, c"v24@0:8Q16";
        "vertexDescriptor" => descriptor_object, c"@16@0:8";
        "vertexFunction" => descriptor_object, c"@16@0:8";
        "fragmentFunction" => descriptor_object, c"@16@0:8";
        "depthAttachmentPixelFormat" => descriptor_number, c"Q16@0:8";
        "stencilAttachmentPixelFormat" => descriptor_number, c"Q16@0:8";
        "sampleCount" => descriptor_number, c"Q16@0:8";
        "rasterSampleCount" => descriptor_number, c"Q16@0:8";
        "isAlphaToCoverageEnabled" => descriptor_number, c"Q16@0:8";
        "isAlphaToOneEnabled" => descriptor_number, c"Q16@0:8";
        "isRasterizationEnabled" => descriptor_number, c"Q16@0:8";
        "init" => descriptor_init, c"@16@0:8";
        "colorAttachments" => color_attachments, c"@16@0:8";
        "dealloc" => descriptor_dealloc, c"v16@0:8";
        "setLabel:" => crate::no_op, c"v24@0:8@16";
        "reset" => descriptor_init, c"@16@0:8";
    });
    for selector in [
        "setVertexFunction:",
        "setFragmentFunction:",
        "setVertexDescriptor:",
    ] {
        descriptor.replace_method(
            objc::sel::get(selector),
            set_descriptor_object as *const std::ffi::c_void,
            c"v24@0:8@16",
        );
    }
    for selector in [
        "setDepthAttachmentPixelFormat:",
        "setStencilAttachmentPixelFormat:",
        "setSampleCount:",
        "setRasterSampleCount:",
        "setAlphaToCoverageEnabled:",
        "setAlphaToOneEnabled:",
        "setRasterizationEnabled:",
    ] {
        descriptor.replace_method(
            objc::sel::get(selector),
            set_descriptor_number as *const std::ffi::c_void,
            c"v24@0:8Q16",
        );
    }
    let _ = color;
    for name in descriptor::COLOR_CLASSES {
        let color = object::register::<ColorElement>(name);
        if name != c"RosemaryMTLColorAttachmentDescriptor" {
            methods!(color, { "init" => descriptor::color_init, c"@16@0:8"; });
        }
        for selector in [
            "setPixelFormat:",
            "setBlendingEnabled:",
            "setSourceRGBBlendFactor:",
            "setDestinationRGBBlendFactor:",
            "setRgbBlendOperation:",
            "setSourceAlphaBlendFactor:",
            "setDestinationAlphaBlendFactor:",
            "setAlphaBlendOperation:",
            "setWriteMask:",
        ] {
            color.replace_method(
                objc::sel::get(selector),
                set_color_value as *const std::ffi::c_void,
                c"v24@0:8Q16",
            );
        }
        for selector in [
            "pixelFormat",
            "isBlendingEnabled",
            "sourceRGBBlendFactor",
            "destinationRGBBlendFactor",
            "rgbBlendOperation",
            "sourceAlphaBlendFactor",
            "destinationAlphaBlendFactor",
            "alphaBlendOperation",
            "writeMask",
        ] {
            color.replace_method(
                objc::sel::get(selector),
                descriptor::color_value as *const std::ffi::c_void,
                c"Q16@0:8",
            );
        }
    }

    let compute =
        objc::class::lookup(c"RosemaryMTLComputePipelineState").expect("registered first");
    methods!(compute, {
        "dealloc" => compute_pipeline_dealloc, c"v16@0:8";
        "device" => compute_pipeline_device, c"@16@0:8";
        "threadExecutionWidth" => compute_pipeline_thread_width, c"Q16@0:8";
        "maxTotalThreadsPerThreadgroup" => compute_pipeline_max_threads, c"Q16@0:8";
        "setLabel:" => crate::no_op, c"v24@0:8@16";
    });
    let render = objc::class::lookup(c"RosemaryMTLRenderPipelineState").expect("registered first");
    methods!(render, {
        "dealloc" => render_pipeline_dealloc, c"v16@0:8";
        "setLabel:" => crate::no_op, c"v24@0:8@16";
    });
}

#[cfg(test)]
mod tests {
    use super::{choose_push_set, dynamic_bindings};
    use ash::vk;

    #[test]
    fn the_lowest_buffer_bindings_become_dynamic_up_to_the_limit() {
        let buffer = vk::DescriptorType::STORAGE_BUFFER;
        let dynamic = vk::DescriptorType::STORAGE_BUFFER_DYNAMIC;
        let mut bindings = vec![(5, buffer), (1, buffer), (3, buffer), (1, buffer)];
        assert_eq!(dynamic_bindings(&mut bindings, 2), [1, 3]);
        assert_eq!(
            bindings,
            [(5, buffer), (1, dynamic), (3, dynamic), (1, dynamic)]
        );
        assert!(dynamic_bindings(&mut [(0, buffer)], 0).is_empty());
    }

    #[test]
    fn push_set_prefers_buffers_then_the_largest_one_that_fits() {
        assert_eq!(choose_push_set([12, 8, 4, 0], 8), Some(1));
        assert_eq!(choose_push_set([4, 8, 2, 0], 8), Some(0));
        assert_eq!(choose_push_set([1, 0, 0, 0], 0), None);
    }
}
