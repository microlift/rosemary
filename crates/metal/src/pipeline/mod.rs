mod buffer_descriptor;
mod compute;
mod compute_descriptor;
pub(crate) use compute_descriptor::{
    descriptor_function as compute_descriptor_function,
    new_with_descriptor as new_compute_pipeline_with_descriptor,
    new_with_function_reflection as new_compute_pipeline_with_reflection,
};
mod descriptor;
mod enums;
mod link;
mod render;

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
pub(crate) use descriptor::copy_render_pipeline_descriptor;
use descriptor::*;
use enums::*;
use render::render_pipeline_dealloc;

pub(crate) use compute::ComputePipeline;
pub(crate) use render::{
    COLOR_ATTACHMENTS, ColorAttachment, DepthStencilKey, RenderDynamicState, RenderPipeline,
    RenderPipelineDescriptor, RenderVariantKey,
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
    /// The binding slots each set's resources are read from.
    pub reads: [SlotSet; SETS],
}

/// Metal's per-stage binding tables: compute, vertex and fragment.
pub(crate) const STAGES: usize = 3;

/// The index of `stage`'s binding tables.
pub(crate) fn stage_slot(stage: Stage) -> usize {
    match stage {
        Stage::Vertex => 1,
        Stage::Fragment => 2,
        _ => 0,
    }
}

/// One of a stage's three binding tables.
#[derive(Clone, Copy)]
pub(crate) enum Table {
    Buffers,
    Textures,
    Samplers,
}

const TABLES: usize = 3;

/// A set of binding slots across every stage's tables, one bit a slot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SlotSet {
    slots: [[u32; STAGES]; TABLES],
}

impl SlotSet {
    /// The most slots of one table a set can name.
    pub(crate) const CAPACITY: usize = u32::BITS as usize;

    /// Add a slot. One past [`Self::CAPACITY`] is not a slot an encoder has.
    pub(crate) fn insert(&mut self, table: Table, stage: usize, index: usize) {
        if index < Self::CAPACITY {
            self.slots[table as usize][stage] |= 1 << index;
        }
    }

    pub(crate) fn intersects(&self, other: &Self) -> bool {
        self.slots
            .as_flattened()
            .iter()
            .zip(other.slots.as_flattened())
            .any(|(mine, theirs)| mine & theirs != 0)
    }
}

/// The binding slots the resources of each descriptor set are read from. A
/// `constexpr sampler` is made with the pipeline and reads none.
fn slot_reads(resources: &[&[ResourceBinding]]) -> [SlotSet; SETS] {
    let mut reads = [SlotSet::default(); SETS];
    for binding in resources.iter().copied().flatten() {
        let table = match binding.kind {
            ResourceKind::Buffer => Table::Buffers,
            ResourceKind::Texture => Table::Textures,
            ResourceKind::Sampler if binding.inline_sampler.is_none() => Table::Samplers,
            _ => continue,
        };
        if let Some(set) = reads.get_mut(binding.descriptor_set as usize) {
            set.insert(
                table,
                stage_slot(binding.stage),
                binding.metal_index as usize,
            );
        }
    }
    reads
}

/// The sampler each resource's `constexpr sampler` describes, null for every
/// other resource, made with the pipeline rather than looked up per draw.
pub(crate) fn inline_samplers(gpu: &Gpu, resources: &[ResourceBinding]) -> Vec<vk::Sampler> {
    resources
        .iter()
        .map(|binding| match &binding.inline_sampler {
            Some(sampler) => crate::resource::sampler::inline(gpu, sampler),
            None => vk::Sampler::null(),
        })
        .collect()
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
        reads: slot_reads(resources),
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
    // The Vulkan translator only needs macros; native Metal also needs the
    // values the guest explicitly configured. None means host defaults.
    pub fast_math: Option<bool>,
    pub preserve_invariance: Option<bool>,
    pub language_version: Option<u64>,
}

unsafe extern "C" fn compile_options_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a freshly allocated options object.
    unsafe {
        object::init(
            receiver,
            CompileOptions {
                macros: Vec::new(),
                fast_math: None,
                preserve_invariance: None,
                language_version: None,
            },
        )
    };
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

unsafe extern "C" fn set_compile_options_flag(receiver: Id, sel: Sel, value: u8) {
    let options = unsafe { object::body::<CompileOptions>(receiver) };
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"setFastMathEnabled:" => options.fast_math = Some(value != 0),
        b"setPreserveInvariance:" => options.preserve_invariance = Some(value != 0),
        _ => unreachable!("only compile option flag setters use this callback"),
    }
}

unsafe extern "C" fn set_compile_language_version(receiver: Id, _sel: Sel, value: u64) {
    unsafe { object::body::<CompileOptions>(receiver) }.language_version = Some(value);
}

pub(crate) fn install() {
    buffer_descriptor::install();
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
        "setFastMathEnabled:" => set_compile_options_flag, c"v20@0:8c16";
        "setPreserveInvariance:" => set_compile_options_flag, c"v20@0:8c16";
        "setLanguageVersion:" => set_compile_language_version, c"v24@0:8Q16";
        "setPreprocessorMacros:" => set_preprocessor_macros, c"v24@0:8@16";
    });
    methods!(descriptor, {
        // No function here is ever a post-tessellation vertex function, so
        // tessellation state has nothing to configure: a patch function is
        // refused when it is made.
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
        "vertexBuffers" => descriptor_buffers, c"@16@0:8";
        "fragmentBuffers" => descriptor_buffers, c"@16@0:8";
        "depthAttachmentPixelFormat" => descriptor_number, c"Q16@0:8";
        "stencilAttachmentPixelFormat" => descriptor_number, c"Q16@0:8";
        "sampleCount" => descriptor_number, c"Q16@0:8";
        "rasterSampleCount" => descriptor_number, c"Q16@0:8";
        "isAlphaToCoverageEnabled" => descriptor_number, c"Q16@0:8";
        "isAlphaToOneEnabled" => descriptor_number, c"Q16@0:8";
        "isRasterizationEnabled" => descriptor_number, c"Q16@0:8";
        "inputPrimitiveTopology" => descriptor_number, c"Q16@0:8";
        "init" => descriptor_init, c"@16@0:8";
        "colorAttachments" => color_attachments, c"@16@0:8";
        "dealloc" => descriptor_dealloc, c"v16@0:8";
        "setLabel:" => crate::no_op, c"v24@0:8@16";
        "reset" => descriptor_reset, c"v16@0:8";
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
        "setInputPrimitiveTopology:",
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
    use super::{
        ResourceBinding, ResourceKind, SETS, SlotSet, Stage, Table, choose_push_set,
        dynamic_bindings, slot_reads, stage_slot,
    };
    use ash::vk;

    fn resource(stage: Stage, kind: ResourceKind, metal_index: u32, set: u32) -> ResourceBinding {
        ResourceBinding {
            stage,
            kind,
            metal_index,
            descriptor_set: set,
            binding: metal_index,
            count: 1,
            access: metal_translate::ir::Access::Read,
            sampled: true,
            texel_buffer: false,
            inline_sampler: None,
        }
    }

    #[test]
    fn each_set_reads_the_slots_of_its_own_resources() {
        let vertex = [resource(Stage::Vertex, ResourceKind::Buffer, 3, 0)];
        let fragment = [
            resource(Stage::Fragment, ResourceKind::Texture, 3, 1),
            resource(Stage::Fragment, ResourceKind::Sampler, 0, 2),
            // Past the slots an encoder has, so no slot at all.
            resource(Stage::Fragment, ResourceKind::Buffer, 40, 0),
        ];
        let reads = slot_reads(&[&vertex, &fragment]);
        let slot = |table, stage, index| {
            let mut set = SlotSet::default();
            set.insert(table, stage_slot(stage), index);
            set
        };
        let which = |changed: SlotSet| reads.map(|set| set.intersects(&changed));
        assert_eq!(
            which(slot(Table::Buffers, Stage::Vertex, 3)),
            [true, false, false, false]
        );
        assert_eq!(
            which(slot(Table::Textures, Stage::Fragment, 3)),
            [false, true, false, false]
        );
        assert_eq!(
            which(slot(Table::Samplers, Stage::Fragment, 0)),
            [false, false, true, false]
        );
        // The same index of another stage's table, or of another table.
        assert_eq!(
            which(slot(Table::Buffers, Stage::Fragment, 3)),
            [false; SETS]
        );
        assert_eq!(
            which(slot(Table::Samplers, Stage::Fragment, 3)),
            [false; SETS]
        );
        assert_eq!(
            which(slot(Table::Buffers, Stage::Fragment, 40)),
            [false; SETS]
        );
    }

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
