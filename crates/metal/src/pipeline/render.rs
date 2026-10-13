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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct DepthStencilKey {
    pub compare: u64,
    pub write: bool,
    pub stencil: bool,
    pub front: [u64; 6],
    pub back: [u64; 6],
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
    /// The pass shape this pipeline was declared against, without resolves;
    /// `variant` adds those from the key and asks the cache for the pass.
    pass: RenderPassKey,
    variants: Mutex<HashMap<RenderVariantKey, vk::Pipeline>>,
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
        let key = RenderPassKey {
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
        gpu.render_pass(&key)?;

        let mut resources = vertex.resources.clone();
        resources.extend(fragment.resources.iter().cloned());
        Ok(Self {
            gpu,
            vertex_module,
            fragment_module,
            vertex_name: std::ffi::CString::new(vertex.name.as_str())
                .map_err(|_| "entry point name contains a NUL".to_string())?,
            fragment_name: std::ffi::CString::new(fragment.name.as_str())
                .map_err(|_| "entry point name contains a NUL".to_string())?,
            layouts,
            resources,
            vertex_streams,
            bindings,
            attributes,
            divisors,
            colors,
            depth_format,
            stencil_format,
            samples,
            alpha_to_coverage: descriptor.alpha_to_coverage,
            pass: key,
            variants: Mutex::new(HashMap::new()),
            profile_label: crate::profile::pipeline_label(&[vertex, fragment]),
        })
    }

    pub fn variant(&self, key: &RenderVariantKey) -> Result<vk::Pipeline, String> {
        let mut cache = self.variants.lock().expect("render pipeline cache");
        if let Some(pipeline) = cache.get(key) {
            return Ok(*pipeline);
        }
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
        let pass = self.gpu.render_pass(&shape)?;
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
            // MTLDepthClipModeClamp is 1.
            .depth_clamp_enable(key.depth_clip == MTL_DEPTH_CLIP_MODE_CLAMP)
            .polygon_mode(match key.fill_mode {
                1 => vk::PolygonMode::LINE,
                _ => vk::PolygonMode::FILL,
            })
            .cull_mode(match key.cull_mode {
                1 => vk::CullModeFlags::FRONT,
                2 => vk::CullModeFlags::BACK,
                _ => vk::CullModeFlags::NONE,
            })
            // Metal's window origin is upper-left and Vulkan's is too once the
            // viewport is flipped, so a Metal clockwise front face is a Vulkan
            // clockwise front face.
            .front_face(match key.winding {
                1 => vk::FrontFace::COUNTER_CLOCKWISE,
                _ => vk::FrontFace::CLOCKWISE,
            })
            .depth_bias_enable(true)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(self.samples)
            .alpha_to_coverage_enable(self.alpha_to_coverage);
        let stencil_face = |face: &[u64; 6]| {
            vk::StencilOpState::default()
                .compare_op(compare_op(face[0]))
                .fail_op(stencil_op(face[1]))
                .depth_fail_op(stencil_op(face[2]))
                .pass_op(stencil_op(face[3]))
                .compare_mask(face[4] as u32)
                .write_mask(face[5] as u32)
        };
        let has_depth = self.depth_format != vk::Format::UNDEFINED
            || self.stencil_format != vk::Format::UNDEFINED;
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::default()
            // Metal has no depth-test switch: `MTLCompareFunctionAlways` with
            // writes off is how a game turns depth off.
            .depth_test_enable(has_depth)
            .depth_write_enable(has_depth && key.depth.write)
            .depth_compare_op(compare_op(key.depth.compare))
            .stencil_test_enable(has_depth && key.depth.stencil)
            .front(stencil_face(&key.depth.front))
            .back(stencil_face(&key.depth.back));
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
        let dynamic_states = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::STENCIL_REFERENCE,
            vk::DynamicState::DEPTH_BIAS,
            vk::DynamicState::BLEND_CONSTANTS,
        ];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let info = vk::GraphicsPipelineCreateInfo::default()
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
        // SAFETY: every borrowed structure lives across the call.
        let pipeline = crate::stats::pipeline(|| unsafe {
            self.gpu
                .device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
        })
        .map_err(|(_, error)| format!("vkCreateGraphicsPipelines failed with {error:?}"))?[0];
        cache.insert(*key, pipeline);
        Ok(pipeline)
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
