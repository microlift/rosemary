use super::*;
use crate::constants::*;

/// The state Metal keeps on the encoder but Vulkan bakes into a pipeline.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RenderVariantKey {
    pub topology: u64,
    pub cull_mode: u64,
    pub winding: u64,
    pub fill_mode: u64,
    pub depth_clip: u64,
    pub depth: DepthStencilKey,
    /// Which of the pass's colour attachments are being resolved. A Metal
    /// pipeline state says nothing about resolving - the pass descriptor does -
    /// but Vulkan render passes are compatible only when their resolve
    /// attachment references match, so the pipeline needs one variant per
    /// resolve shape it is drawn under.
    pub resolve_mask: u32,
    /// The depth/stencil resolve of the pass, for the same reason. Only the
    /// modes matter here: the format is the pipeline's own depth format.
    pub depth_resolve: Option<DepthResolve>,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) struct DepthStencilKey {
    pub compare: u64,
    pub write: bool,
    pub stencil: bool,
    pub front: [u64; 6],
    pub back: [u64; 6],
}

/// `MTLPrimitiveType`'s point, line and triangle lists: the first of each
/// class, which a pipeline whose topology is dynamic is made with.
const PRIMITIVE_POINT: u64 = 0;
const PRIMITIVE_LINE: u64 = 1;
const PRIMITIVE_LINE_STRIP: u64 = 2;
const PRIMITIVE_TRIANGLE: u64 = 3;

/// `MTLPrimitiveTopologyClass`.
const TOPOLOGY_CLASS_POINT: u64 = 1;
const TOPOLOGY_CLASS_LINE: u64 = 2;

/// `MTLCullMode`.
const CULL_MODE_FRONT: u64 = 1;
const CULL_MODE_BACK: u64 = 2;

/// `MTLWinding`.
const WINDING_COUNTER_CLOCKWISE: u64 = 1;

/// `MTLTriangleFillMode`.
const TRIANGLE_FILL_MODE_LINES: u64 = 1;

/// One face's stencil test as Vulkan takes it: the fail, pass and depth-fail
/// operations, the comparison, and the compare and write masks.
#[derive(Clone, Copy, Eq, PartialEq)]
struct StencilFace {
    fail: vk::StencilOp,
    pass: vk::StencilOp,
    depth_fail: vk::StencilOp,
    compare: vk::CompareOp,
    compare_mask: u32,
    write_mask: u32,
}

impl StencilFace {
    /// A Metal stencil descriptor as the depth/stencil key packs it.
    fn of(face: &[u64; 6]) -> Self {
        Self {
            compare: compare_op(face[0]),
            fail: stencil_op(face[1]),
            depth_fail: stencil_op(face[2]),
            pass: stencil_op(face[3]),
            compare_mask: face[4] as u32,
            write_mask: face[5] as u32,
        }
    }

    fn state(&self) -> vk::StencilOpState {
        vk::StencilOpState::default()
            .compare_op(self.compare)
            .fail_op(self.fail)
            .depth_fail_op(self.depth_fail)
            .pass_op(self.pass)
            .compare_mask(self.compare_mask)
            .write_mask(self.write_mask)
    }
}

fn cull_mode(metal: u64) -> vk::CullModeFlags {
    match metal {
        CULL_MODE_FRONT => vk::CullModeFlags::FRONT,
        CULL_MODE_BACK => vk::CullModeFlags::BACK,
        _ => vk::CullModeFlags::NONE,
    }
}

fn polygon_mode(metal: u64) -> vk::PolygonMode {
    match metal {
        TRIANGLE_FILL_MODE_LINES => vk::PolygonMode::LINE,
        _ => vk::PolygonMode::FILL,
    }
}

/// Metal's window origin is upper-left and Vulkan's is too once the viewport
/// is flipped, so a Metal clockwise front face is a Vulkan clockwise one.
fn front_face(metal: u64) -> vk::FrontFace {
    match metal {
        WINDING_COUNTER_CLOCKWISE => vk::FrontFace::COUNTER_CLOCKWISE,
        _ => vk::FrontFace::CLOCKWISE,
    }
}

/// The encoder state a draw sets as Vulkan dynamic state, on a device with
/// Vulkan 1.3. In a pipeline it would make one pipeline per combination, and
/// a game draws one Metal pipeline under many: each new one met mid-frame is
/// a compile, which is a stutter.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct RenderDynamicState {
    cull_mode: vk::CullModeFlags,
    front_face: vk::FrontFace,
    topology: vk::PrimitiveTopology,
    depth_test: bool,
    depth_write: bool,
    depth_compare: vk::CompareOp,
    stencil_test: bool,
    front: StencilFace,
    back: StencilFace,
    /// Set per draw only where the device has them dynamic as well.
    polygon_mode: vk::PolygonMode,
    depth_clamp: bool,
}

impl RenderDynamicState {
    /// Record what differs from `previous`, everything where nothing is known
    /// to have been recorded.
    pub fn record(&self, gpu: &Gpu, command_buffer: vk::CommandBuffer, previous: Option<&Self>) {
        let device = &gpu.device;
        // Whether `field` differs from what was last recorded.
        let differs = |field: fn(&Self) -> u64| {
            previous.is_none_or(|previous| field(previous) != field(self))
        };
        // SAFETY: recording into a command buffer whose bound pipeline
        // declares these states dynamic, on a Vulkan 1.3 device.
        unsafe {
            if differs(|state| u64::from(state.cull_mode.as_raw())) {
                device.cmd_set_cull_mode(command_buffer, self.cull_mode);
            }
            if differs(|state| state.front_face.as_raw() as u64) {
                device.cmd_set_front_face(command_buffer, self.front_face);
            }
            if differs(|state| state.topology.as_raw() as u64) {
                device.cmd_set_primitive_topology(command_buffer, self.topology);
            }
            if differs(|state| u64::from(state.depth_test)) {
                device.cmd_set_depth_test_enable(command_buffer, self.depth_test);
            }
            if differs(|state| u64::from(state.depth_write)) {
                device.cmd_set_depth_write_enable(command_buffer, self.depth_write);
            }
            if differs(|state| state.depth_compare.as_raw() as u64) {
                device.cmd_set_depth_compare_op(command_buffer, self.depth_compare);
            }
            if differs(|state| u64::from(state.stencil_test)) {
                device.cmd_set_stencil_test_enable(command_buffer, self.stencil_test);
            }
            let faces = [
                (
                    vk::StencilFaceFlags::FRONT,
                    self.front,
                    previous.map(|p| p.front),
                ),
                (
                    vk::StencilFaceFlags::BACK,
                    self.back,
                    previous.map(|p| p.back),
                ),
            ];
            for (flags, face, previous) in faces {
                let ops =
                    |face: &StencilFace| (face.fail, face.pass, face.depth_fail, face.compare);
                if previous.is_none_or(|previous| ops(&previous) != ops(&face)) {
                    device.cmd_set_stencil_op(
                        command_buffer,
                        flags,
                        face.fail,
                        face.pass,
                        face.depth_fail,
                        face.compare,
                    );
                }
                if previous.is_none_or(|previous| previous.compare_mask != face.compare_mask) {
                    device.cmd_set_stencil_compare_mask(command_buffer, flags, face.compare_mask);
                }
                if previous.is_none_or(|previous| previous.write_mask != face.write_mask) {
                    device.cmd_set_stencil_write_mask(command_buffer, flags, face.write_mask);
                }
            }
            if let Some(raster) = &gpu.dynamic_raster {
                if differs(|state| state.polygon_mode.as_raw() as u64) {
                    raster.cmd_set_polygon_mode(command_buffer, self.polygon_mode);
                }
                if differs(|state| u64::from(state.depth_clamp)) {
                    raster.cmd_set_depth_clamp_enable(command_buffer, self.depth_clamp);
                }
            }
        }
    }
}

/// One `MTLRenderPipelineColorAttachmentDescriptor`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ColorAttachment {
    pub pixel_format: u64,
    pub blending: bool,
    pub source_rgb: u64,
    pub destination_rgb: u64,
    pub rgb_operation: u64,
    pub source_alpha: u64,
    pub destination_alpha: u64,
    pub alpha_operation: u64,
    pub write_mask: u64,
}

impl ColorAttachment {
    pub fn fresh() -> Self {
        Self {
            pixel_format: MTL_PIXEL_FORMAT_INVALID,
            blending: false,
            source_rgb: 1,      // One
            destination_rgb: 0, // Zero
            rgb_operation: 0,   // Add
            source_alpha: 1,
            destination_alpha: 0,
            alpha_operation: 0,
            write_mask: MTL_COLOR_WRITE_MASK_ALL,
        }
    }
}

pub(crate) const COLOR_ATTACHMENTS: usize = 8;

pub(crate) struct RenderPipelineDescriptor {
    pub vertex_function: objc::object::Id,
    pub fragment_function: objc::object::Id,
    pub vertex_descriptor: objc::object::Id,
    pub colors: [ColorAttachment; COLOR_ATTACHMENTS],
    pub depth_format: u64,
    pub stencil_format: u64,
    pub sample_count: u64,
    pub alpha_to_coverage: bool,
    pub alpha_to_one: bool,
    /// `NO` runs only the vertex function: nothing is rasterized.
    pub rasterization: bool,
    /// The `MTLPrimitiveTopologyClass` the pipeline draws, or unspecified.
    pub input_topology: u64,
    /// The `-colorAttachments` collection, made on first use.
    pub color_objects: objc::object::Id,
}

pub(crate) struct RenderPipeline {
    pub gpu: &'static Gpu,
    pub vertex_module: vk::ShaderModule,
    pub fragment_module: vk::ShaderModule,
    pub vertex_name: std::ffi::CString,
    pub fragment_name: std::ffi::CString,
    pub layouts: Layouts,
    pub resources: Vec<ResourceBinding>,
    /// Whether a shader writes a buffer or texture it is bound, rather than
    /// only the pass's attachments.
    pub writes_resources: bool,
    /// Parallel to `resources`.
    pub inline_samplers: Vec<vk::Sampler>,
    /// Metal buffer indices which feed vertex attributes rather than a
    /// descriptor, taken from the pipeline's vertex descriptor.
    pub vertex_streams: Vec<u32>,
    bindings: Vec<vk::VertexInputBindingDescription>,
    attributes: Vec<vk::VertexInputAttributeDescription>,
    divisors: Vec<vk::VertexInputBindingDivisorDescriptionKHR>,
    colors: Vec<ColorAttachment>,
    pub depth_format: vk::Format,
    pub stencil_format: vk::Format,
    pub samples: vk::SampleCountFlags,
    alpha_to_coverage: bool,
    alpha_to_one: bool,
    rasterization: bool,
    /// The pass shape this pipeline was declared against, without resolves;
    /// `variant` adds those from the key and asks the cache for the pass.
    pass: RenderPassKey,
    variants: Mutex<crate::hash::FastMap<RenderVariantKey, vk::Pipeline>>,
    /// `ROSEMARY_METAL_PROFILE`: what its draws are totalled under.
    pub profile_label: Option<std::sync::Arc<str>>,
}

impl RenderPipeline {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gpu: &'static Gpu,
        vertex: &EntryArtifact,
        fragment: &EntryArtifact,
        descriptor: &RenderPipelineDescriptor,
        layouts_in: &[VertexLayout; VERTEX_SLOTS],
        attributes_in: &[VertexAttribute; VERTEX_SLOTS],
    ) -> Result<Self, String> {
        let samples = gpu::sample_count(descriptor.sample_count).ok_or_else(|| {
            format!(
                "render pipeline has invalid Metal sample count {}",
                descriptor.sample_count
            )
        })?;
        if descriptor.alpha_to_one && !gpu.alpha_to_one {
            return Err("alphaToOneEnabled needs the Vulkan alphaToOne feature".into());
        }
        let vertex_module = gpu.create_shader_module(&vertex.words, &vertex.name)?;
        let linked =
            super::link::link_varyings(&vertex.varyings, &fragment.varyings, &fragment.words);
        let fragment_words = linked.as_deref().unwrap_or(&fragment.words);
        let fragment_module = gpu.create_shader_module(fragment_words, &fragment.name)?;
        let layouts = layouts_for(gpu, &[&vertex.resources, &fragment.resources])?;

        let mut bindings = Vec::new();
        let mut attributes = Vec::new();
        let mut divisors = Vec::new();
        let mut vertex_streams = Vec::new();
        for (index, attribute) in attributes_in.iter().enumerate() {
            if attribute.format == 0 {
                continue;
            }
            let format = vertex_format(attribute.format).ok_or_else(|| {
                format!("MTLVertexFormat {} has no Vulkan form", attribute.format)
            })?;
            // `-setBufferIndex:` takes any `NSUInteger`, and the layouts below
            // are the 31 a `MTLVertexDescriptor` has.
            if attribute.buffer_index as usize >= VERTEX_SLOTS {
                return Err(format!(
                    "vertex attribute {index} names buffer index {}, which is outside the {} a \
                     MTLVertexDescriptor has",
                    attribute.buffer_index, VERTEX_SLOTS
                ));
            }
            if u32::try_from(attribute.offset).is_err() {
                return Err(format!(
                    "vertex attribute {index} has offset {}, which Vulkan cannot express",
                    attribute.offset
                ));
            }
            attributes.push(
                vk::VertexInputAttributeDescription::default()
                    .location(index as u32)
                    .binding(attribute.buffer_index as u32)
                    .format(format)
                    .offset(attribute.offset as u32),
            );
            if !vertex_streams.contains(&(attribute.buffer_index as u32)) {
                vertex_streams.push(attribute.buffer_index as u32);
            }
        }
        for stream in &vertex_streams {
            // Bounded by the check on every attribute above, which is where
            // `vertex_streams` comes from.
            let layout = &layouts_in[*stream as usize];
            let stride = binding_stride(*stream, layout)?;
            let (input_rate, divisor) =
                vertex_input_rate(*stream, layout, gpu.max_vertex_attribute_divisor)?;
            bindings.push(
                vk::VertexInputBindingDescription::default()
                    .binding(*stream)
                    .stride(stride)
                    .input_rate(input_rate),
            );
            if let Some(divisor) = divisor {
                divisors.push(divisor);
            }
        }

        // FIXME (ACCURACY): the colour list stops at the first render-target
        // index with no format, matching the way a render pass closes a gap
        // up rather than keeping it. A pipeline which declares targets 0 and 2
        // therefore loses target 2. See the MRT gap in the known gaps.
        if descriptor
            .colors
            .iter()
            .skip_while(|color| color.pixel_format != 0)
            .any(|color| color.pixel_format != 0)
        {
            gpu::trace_once(
                "a render pipeline leaves a gap in its colour attachments, which this runtime \
                 closes up: the targets after the gap are dropped",
            );
        }
        let colors = descriptor
            .colors
            .iter()
            .take_while(|color| color.pixel_format != 0)
            .copied()
            .collect::<Vec<_>>();
        let mut color_formats = Vec::new();
        for color in &colors {
            let format = gpu::pixel_format(color.pixel_format);
            if format == vk::Format::UNDEFINED {
                return Err(format!(
                    "colour attachment has no Vulkan form for MTLPixelFormat {}",
                    color.pixel_format
                ));
            }
            color_formats.push((
                format,
                vk::AttachmentLoadOp::DONT_CARE,
                vk::AttachmentStoreOp::STORE,
            ));
        }
        let depth_format = gpu::pixel_format(descriptor.depth_format);
        let stencil_format = gpu::pixel_format(descriptor.stencil_format);
        let attachment_depth = if depth_format != vk::Format::UNDEFINED {
            Some(depth_format)
        } else if stencil_format != vk::Format::UNDEFINED {
            Some(stencil_format)
        } else {
            None
        };
        let pass = RenderPassKey {
            color_resolves: vec![None; color_formats.len()],
            depth_resolve: None,
            colors: color_formats,
            depth: attachment_depth.map(|format| {
                (
                    format,
                    vk::AttachmentLoadOp::DONT_CARE,
                    vk::AttachmentStoreOp::STORE,
                )
            }),
            stencil: (stencil_format != vk::Format::UNDEFINED).then_some((
                vk::AttachmentLoadOp::DONT_CARE,
                vk::AttachmentStoreOp::STORE,
            )),
            samples,
        };
        if !gpu.dynamic_rendering {
            gpu.render_pass(&pass)?;
        }

        let mut resources = vertex.resources.clone();
        resources.extend(fragment.resources.iter().cloned());
        let inline_samplers = inline_samplers(gpu, &resources);
        let pipeline = Self {
            gpu,
            vertex_module,
            fragment_module,
            vertex_name: std::ffi::CString::new(vertex.name.as_str())
                .map_err(|_| "entry point name contains a NUL".to_string())?,
            fragment_name: std::ffi::CString::new(fragment.name.as_str())
                .map_err(|_| "entry point name contains a NUL".to_string())?,
            layouts,
            writes_resources: resources.iter().any(|binding| {
                matches!(
                    binding.access,
                    metal_translate::ir::Access::Write | metal_translate::ir::Access::ReadWrite
                )
            }),
            resources,
            inline_samplers,
            vertex_streams,
            bindings,
            attributes,
            divisors,
            colors,
            depth_format,
            stencil_format,
            samples,
            alpha_to_coverage: descriptor.alpha_to_coverage,
            alpha_to_one: descriptor.alpha_to_one,
            rasterization: descriptor.rasterization,
            pass,
            variants: Mutex::new(Default::default()),
            profile_label: crate::profile::pipeline_label(&[vertex, fragment]),
        };
        if gpu.dynamic_render_state {
            // Metal compiles a pipeline state when it is made, so a game makes
            // them where a compile is affordable. With the draw state dynamic
            // nearly every draw uses this one variant, so compiling it now
            // keeps the compile out of the frame which first draws with it.
            let topology = match descriptor.input_topology {
                TOPOLOGY_CLASS_POINT => PRIMITIVE_POINT,
                TOPOLOGY_CLASS_LINE => PRIMITIVE_LINE,
                _ => PRIMITIVE_TRIANGLE,
            };
            let expected = RenderVariantKey {
                topology,
                cull_mode: 0,
                winding: 0,
                fill_mode: 0,
                depth_clip: 0,
                depth: DepthStencilKey::default(),
                resolve_mask: 0,
                depth_resolve: None,
            };
            if let Err(reason) = pipeline.variant(&pipeline.pipeline_key(&expected)) {
                gpu::trace(format!("render pipeline creation failed: {reason}"));
            }
        }
        Ok(pipeline)
    }

    pub fn variant(&self, key: &RenderVariantKey) -> Result<vk::Pipeline, String> {
        // Compiled without the lock held: a driver can take tens of
        // milliseconds, and other encoders drawing with this state must not
        // wait on a variant they do not need.
        if let Some(pipeline) = self
            .variants
            .lock()
            .expect("render pipeline cache")
            .get(key)
        {
            return Ok(*pipeline);
        }
        let pass = if self.gpu.dynamic_rendering {
            vk::RenderPass::null()
        } else {
            self.render_pass(key)?
        };
        let color_formats = self
            .pass
            .colors
            .iter()
            .map(|color| color.0)
            .collect::<Vec<_>>();
        let (depth_format, stencil_format) = self.pass.depth_stencil_formats();
        let mut rendering = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&color_formats)
            .depth_attachment_format(depth_format)
            .stencil_attachment_format(stencil_format);
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(self.vertex_module)
                .name(&self.vertex_name),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(self.fragment_module)
                .name(&self.fragment_name),
        ];
        let mut divisor_state = vk::PipelineVertexInputDivisorStateCreateInfoKHR::default()
            .vertex_binding_divisors(&self.divisors);
        let mut vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&self.bindings)
            .vertex_attribute_descriptions(&self.attributes);
        if !self.divisors.is_empty() {
            vertex_input = vertex_input.push_next(&mut divisor_state);
        }
        let assembly =
            vk::PipelineInputAssemblyStateCreateInfo::default().topology(topology(key.topology));
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .depth_clamp_enable(key.depth_clip == MTL_DEPTH_CLIP_MODE_CLAMP)
            .polygon_mode(polygon_mode(key.fill_mode))
            .cull_mode(cull_mode(key.cull_mode))
            .front_face(front_face(key.winding))
            .depth_bias_enable(true)
            .rasterizer_discard_enable(!self.rasterization)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(self.samples)
            .alpha_to_coverage_enable(self.alpha_to_coverage)
            .alpha_to_one_enable(self.alpha_to_one);
        let depth = self.dynamic_state(key);
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(depth.depth_test)
            .depth_write_enable(depth.depth_write)
            .depth_compare_op(depth.depth_compare)
            .stencil_test_enable(depth.stencil_test)
            .front(depth.front.state())
            .back(depth.back.state());
        let blends = self
            .colors
            .iter()
            .map(|color| {
                vk::PipelineColorBlendAttachmentState::default()
                    .blend_enable(color.blending)
                    .src_color_blend_factor(blend_factor(color.source_rgb))
                    .dst_color_blend_factor(blend_factor(color.destination_rgb))
                    .color_blend_op(blend_op(color.rgb_operation))
                    .src_alpha_blend_factor(blend_factor(color.source_alpha))
                    .dst_alpha_blend_factor(blend_factor(color.destination_alpha))
                    .alpha_blend_op(blend_op(color.alpha_operation))
                    .color_write_mask(write_mask(color.write_mask))
            })
            .collect::<Vec<_>>();
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blends);
        let mut dynamic_states = vec![
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::STENCIL_REFERENCE,
            vk::DynamicState::DEPTH_BIAS,
            vk::DynamicState::BLEND_CONSTANTS,
        ];
        if self.gpu.dynamic_render_state {
            dynamic_states.extend([
                vk::DynamicState::CULL_MODE,
                vk::DynamicState::FRONT_FACE,
                vk::DynamicState::PRIMITIVE_TOPOLOGY,
                vk::DynamicState::DEPTH_TEST_ENABLE,
                vk::DynamicState::DEPTH_WRITE_ENABLE,
                vk::DynamicState::DEPTH_COMPARE_OP,
                vk::DynamicState::STENCIL_TEST_ENABLE,
                vk::DynamicState::STENCIL_OP,
                vk::DynamicState::STENCIL_COMPARE_MASK,
                vk::DynamicState::STENCIL_WRITE_MASK,
            ]);
        }
        if self.gpu.dynamic_raster.is_some() {
            dynamic_states.extend([
                vk::DynamicState::POLYGON_MODE_EXT,
                vk::DynamicState::DEPTH_CLAMP_ENABLE_EXT,
            ]);
        }
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let mut info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(self.layouts.pipeline)
            .render_pass(pass)
            .subpass(0);
        if self.gpu.dynamic_rendering {
            info = info.push_next(&mut rendering);
        }
        // SAFETY: every borrowed structure lives across the call.
        let pipeline = crate::stats::pipeline(|| unsafe {
            self.gpu
                .device
                .create_graphics_pipelines(self.gpu.pipeline_cache(), &[info], None)
        })
        .map_err(|(_, error)| format!("vkCreateGraphicsPipelines failed with {error:?}"))?[0];
        self.gpu.pipeline_created();
        let mut cache = self.variants.lock().expect("render pipeline cache");
        match cache.entry(*key) {
            std::collections::hash_map::Entry::Occupied(made) => {
                // Another thread compiled it first; this one was never recorded.
                // SAFETY: a pipeline of this device's which nothing refers to.
                unsafe { self.gpu.device.destroy_pipeline(pipeline, None) };
                Ok(*made.get())
            }
            std::collections::hash_map::Entry::Vacant(slot) => Ok(*slot.insert(pipeline)),
        }
    }

    /// The render pass a pipeline drawn under `key` is made against, where
    /// the device has no dynamic rendering.
    fn render_pass(&self, key: &RenderVariantKey) -> Result<vk::RenderPass, String> {
        let mut shape = self.pass.clone();
        for (index, resolve) in shape.color_resolves.iter_mut().enumerate() {
            if key.resolve_mask & (1 << index) != 0 {
                *resolve = Some(self.pass.colors[index].0);
            }
        }
        // The resolve destination has the attachment's format, which the
        // pipeline declared; the modes come from the pass being recorded.
        shape.depth_resolve = match (key.depth_resolve, shape.depth) {
            (Some(resolve), Some((format, _, _))) => Some(DepthResolve { format, ..resolve }),
            _ => None,
        };
        self.gpu.render_pass(&shape)
    }

    /// The key of the Vulkan pipeline a draw under `key` uses. State the
    /// device sets per draw, or takes from the pass as it begins, is left
    /// out, so draws differing only in it share one pipeline.
    pub fn pipeline_key(&self, key: &RenderVariantKey) -> RenderVariantKey {
        let mut shared = *key;
        if self.gpu.dynamic_render_state {
            shared.topology = match key.topology {
                PRIMITIVE_POINT => PRIMITIVE_POINT,
                PRIMITIVE_LINE | PRIMITIVE_LINE_STRIP => PRIMITIVE_LINE,
                _ => PRIMITIVE_TRIANGLE,
            };
            shared.cull_mode = 0;
            shared.winding = 0;
            shared.depth = DepthStencilKey::default();
        }
        if self.gpu.dynamic_raster.is_some() {
            shared.fill_mode = 0;
            shared.depth_clip = 0;
        }
        if self.gpu.dynamic_rendering {
            shared.resolve_mask = 0;
            shared.depth_resolve = None;
        }
        shared
    }

    /// The state a draw under `key` sets per draw on a device which can.
    pub fn dynamic_state(&self, key: &RenderVariantKey) -> RenderDynamicState {
        let has_depth = self.depth_format != vk::Format::UNDEFINED
            || self.stencil_format != vk::Format::UNDEFINED;
        RenderDynamicState {
            cull_mode: cull_mode(key.cull_mode),
            front_face: front_face(key.winding),
            topology: topology(key.topology),
            // Metal has no depth-test switch: `MTLCompareFunctionAlways` with
            // writes off is how a game turns depth off.
            depth_test: has_depth,
            depth_write: has_depth && key.depth.write,
            depth_compare: compare_op(key.depth.compare),
            stencil_test: has_depth && key.depth.stencil,
            front: StencilFace::of(&key.depth.front),
            back: StencilFace::of(&key.depth.back),
            polygon_mode: polygon_mode(key.fill_mode),
            depth_clamp: key.depth_clip == MTL_DEPTH_CLIP_MODE_CLAMP,
        }
    }

    /// How many Vulkan pipelines this one has compiled.
    #[cfg(test)]
    pub fn variant_count(&self) -> usize {
        self.variants.lock().expect("render pipeline cache").len()
    }

    pub fn supports_base_instance(&self, base_instance: u64) -> bool {
        base_instance == 0 || self.divisors.is_empty() || self.gpu.divisor_non_zero_first_instance
    }
}

/// The Vulkan stride of a vertex buffer. Metal ignores the stride of constant
/// input, which Unity sets anyway; a zero stride makes every vertex read the
/// first element.
fn binding_stride(stream: u32, layout: &VertexLayout) -> Result<u32, String> {
    let stride = if layout.step_function == 0 {
        0
    } else {
        layout.stride
    };
    u32::try_from(stride).map_err(|_| {
        format!(
            "vertex buffer {stream} has stride {}, which Vulkan cannot express",
            layout.stride
        )
    })
}

fn vertex_input_rate(
    stream: u32,
    layout: &VertexLayout,
    max_divisor: u32,
) -> Result<
    (
        vk::VertexInputRate,
        Option<vk::VertexInputBindingDivisorDescriptionKHR>,
    ),
    String,
> {
    match layout.step_function {
        // Constant input is a per-vertex binding whose zero stride always
        // addresses the same element; the caller zeroes the stride.
        0 => Ok((vk::VertexInputRate::VERTEX, None)),
        1 => Ok((vk::VertexInputRate::VERTEX, None)),
        2 if layout.step_rate == 0 => Err(format!(
            "vertex buffer {stream} has a per-instance step rate of zero"
        )),
        2 if layout.step_rate == 1 => Ok((vk::VertexInputRate::INSTANCE, None)),
        2 => {
            let divisor = u32::try_from(layout.step_rate).map_err(|_| {
                format!(
                    "vertex buffer {stream} has step rate {}, which Vulkan cannot express",
                    layout.step_rate
                )
            })?;
            if divisor > max_divisor {
                return Err(format!(
                    "vertex buffer {stream} has step rate {divisor}, but this Vulkan device supports at most {max_divisor}"
                ));
            }
            Ok((
                vk::VertexInputRate::INSTANCE,
                Some(
                    vk::VertexInputBindingDivisorDescriptionKHR::default()
                        .binding(stream)
                        .divisor(divisor),
                ),
            ))
        }
        function => Err(format!(
            "vertex buffer {stream} uses unsupported MTLVertexStepFunction {function}"
        )),
    }
}

/// `-[MTLRenderPipelineState dealloc]`.
///
/// As the compute form: a command buffer retains the state it binds until its
/// work has retired, so nothing here can still be in flight. The render passes
/// the variants were made against are the device's cache and stay.
pub(super) unsafe extern "C" fn render_pipeline_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a render pipeline state of this crate's.
    let state = unsafe { object::body::<RenderPipeline>(receiver) };
    if !crate::resource::leaking() {
        let variants = std::mem::take(&mut *state.variants.lock().expect("render pipeline cache"));
        destroy_variants(state.gpu, variants.into_values());
        state.layouts.destroy(state.gpu);
        for module in [
            std::mem::replace(&mut state.vertex_module, vk::ShaderModule::null()),
            std::mem::replace(&mut state.fragment_module, vk::ShaderModule::null()),
        ] {
            unsafe { state.gpu.device.destroy_shader_module(module, None) };
        }
    }
    // SAFETY: the body was established by `object::make` and is not read again.
    unsafe { std::ptr::drop_in_place(state as *mut RenderPipeline) };
    objc::arc::finalize(receiver);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_instance_step_rate_becomes_a_vertex_divisor() {
        let layout = VertexLayout {
            stride: 16,
            step_function: 2,
            step_rate: 4,
        };
        let (rate, divisor) = vertex_input_rate(7, &layout, 16).expect("supported divisor");
        let divisor = divisor.expect("a divisor");
        assert_eq!(rate, vk::VertexInputRate::INSTANCE);
        assert_eq!(divisor.binding, 7);
        assert_eq!(divisor.divisor, 4);
    }

    #[test]
    fn per_instance_step_rate_one_needs_no_extension_state() {
        let layout = VertexLayout {
            stride: 16,
            step_function: 2,
            step_rate: 1,
        };
        let (rate, divisor) = vertex_input_rate(0, &layout, 0).expect("core instance rate");
        assert_eq!(rate, vk::VertexInputRate::INSTANCE);
        assert!(divisor.is_none());
    }

    #[test]
    fn constant_input_ignores_its_declared_stride() {
        let layout = VertexLayout {
            stride: 8,
            step_function: 0,
            step_rate: 0,
        };
        assert_eq!(binding_stride(2, &layout), Ok(0));
        let (rate, divisor) = vertex_input_rate(2, &layout, 0).expect("constant input");
        assert_eq!(rate, vk::VertexInputRate::VERTEX);
        assert!(divisor.is_none());
    }

    #[test]
    fn unsupported_vertex_divisor_is_rejected() {
        let layout = VertexLayout {
            stride: 16,
            step_function: 2,
            step_rate: 4,
        };
        let error = vertex_input_rate(3, &layout, 2).expect_err("divisor exceeds the limit");
        assert!(error.contains("supports at most 2"));
    }
}
