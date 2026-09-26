//! The last step for a window whose surface shows its alpha: the client-side
//! chrome's rounded corners.
//!
//! A Mac window's content is opaque whatever alpha its layers leave, so every
//! row below the chrome is given full alpha, and the bottom corners are then
//! scaled by how much of each pixel lies inside the rounding - the same
//! coverage `chrome::corners` gives the store on the CPU. The top corners come
//! already cut, in the chrome's own pixels.
//!
//! This is a compute pass because nothing a transfer can do touches one
//! channel or blends. It reads the composite and writes a second image rather
//! than working in place: the composite keeps what earlier frames put there,
//! and scaling it again every frame would fade the corners away.

use ash::vk;
use rspirv::binary::Assemble;
use rspirv::dr::{Builder, Operand};
use rspirv::spirv;

use super::FRAMES_IN_FLIGHT;
use crate::gpu::{self, Gpu, GpuImage, ImageDescription};

/// The format of the composite and the output while the pass runs: storage
/// images of it are required of every device, and blits convert either way.
pub(super) const FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;

/// The shader's workgroup edge.
const GROUP: u32 = 8;

/// One frame's shape, as the shader's push constants.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Shape {
    pub width: u32,
    pub height: u32,
    /// How many rows at the top are the chrome's, whose alpha is its own.
    pub top: u32,
    /// The corners' radius in pixels; 0 keeps them square.
    pub radius: f32,
}

impl Shape {
    /// `window`'s shape at `width` by `height`, from its chrome as last
    /// painted. A window with none yet is sealed whole and square.
    pub(super) fn of(window: sdl3::Window, width: u32, height: u32) -> Self {
        let chrome = appkit::decor::pixels(window);
        Self {
            width,
            height,
            top: chrome
                .as_ref()
                .map_or(0, |chrome| chrome.height.min(height)),
            radius: chrome.as_ref().map_or(0.0, |chrome| chrome.radius.max(0.0)),
        }
    }

    fn bytes(&self) -> [u8; 16] {
        let mut bytes = [0; 16];
        for (at, word) in [self.width, self.height, self.top, self.radius.to_bits()]
            .into_iter()
            .enumerate()
        {
            bytes[at * 4..at * 4 + 4].copy_from_slice(&word.to_ne_bytes());
        }
        bytes
    }
}

pub(super) struct Corners {
    gpu: &'static Gpu,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    pool: vk::DescriptorPool,
    /// One per frame in flight, rewritten once that frame's fence is waited.
    sets: Vec<vk::DescriptorSet>,
    /// What is blitted into the swapchain image, made at the composite's size.
    output: Option<GpuImage>,
}

impl Corners {
    pub(super) fn new(gpu: &'static Gpu) -> Result<Self, String> {
        let device = &gpu.device;
        // Shared through the device's cache, which owns it.
        let set_layout = gpu.descriptor_set_layout(
            &[
                (0, vk::DescriptorType::STORAGE_IMAGE),
                (1, vk::DescriptorType::STORAGE_IMAGE),
            ],
            vk::ShaderStageFlags::COMPUTE,
            false,
        )?;
        let push = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
            .size(std::mem::size_of::<Shape>() as u32)];
        let set_layouts = [set_layout];
        let info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&push);
        // SAFETY: a well-formed create info on this live device.
        let pipeline_layout = unsafe { device.create_pipeline_layout(&info, None) }
            .map_err(|error| format!("vkCreatePipelineLayout failed with {error:?}"))?;
        let pipeline_layout = gpu::Guard::new(pipeline_layout, |layout| {
            // SAFETY: a layout of this device's that nothing uses yet.
            unsafe { device.destroy_pipeline_layout(layout, None) };
        });

        let words = shader();
        let module = gpu.create_shader_module(&words, "rosetta3_window_corners")?;
        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(module)
            .name(c"main");
        let info = vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(*pipeline_layout);
        // SAFETY: every borrowed structure lives across the call.
        let pipeline =
            unsafe { device.create_compute_pipelines(vk::PipelineCache::null(), &[info], None) };
        // SAFETY: a pipeline keeps nothing of the module it was made from.
        unsafe { device.destroy_shader_module(module, None) };
        let pipeline = pipeline
            .map_err(|(_, error)| format!("vkCreateComputePipelines failed with {error:?}"))?[0];
        let pipeline = gpu::Guard::new(pipeline, |pipeline| {
            // SAFETY: a pipeline of this device's, never bound.
            unsafe { device.destroy_pipeline(pipeline, None) };
        });

        let sizes = [vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(2 * FRAMES_IN_FLIGHT as u32)];
        let info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(FRAMES_IN_FLIGHT as u32)
            .pool_sizes(&sizes);
        // SAFETY: a well-formed create info on this live device.
        let pool = unsafe { device.create_descriptor_pool(&info, None) }
            .map_err(|error| format!("vkCreateDescriptorPool failed with {error:?}"))?;
        let pool = gpu::Guard::new(pool, |pool| {
            // SAFETY: a pool of this device's; its sets go with it.
            unsafe { device.destroy_descriptor_pool(pool, None) };
        });
        let layouts = [set_layout; FRAMES_IN_FLIGHT];
        let allocate = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(*pool)
            .set_layouts(&layouts);
        // SAFETY: the pool was sized for exactly these sets.
        let sets = unsafe { device.allocate_descriptor_sets(&allocate) }
            .map_err(|error| format!("vkAllocateDescriptorSets failed with {error:?}"))?;

        Ok(Self {
            gpu,
            pipeline_layout: pipeline_layout.take(),
            pipeline: pipeline.take(),
            pool: pool.take(),
            sets,
            output: None,
        })
    }

    /// Make the image the pass writes at `width` by `height`, if it is not
    /// that size already. Outside any recording: making an image submits.
    ///
    /// The caller has finished with every frame that could still be using an
    /// image this replaces.
    pub(super) fn prepare(&mut self, width: u32, height: u32) -> Result<(), String> {
        if self
            .output
            .as_ref()
            .is_some_and(|output| (output.width, output.height) != (width, height))
        {
            self.gpu
                .destroy_image(self.output.take().expect("checked above"));
        }
        if self.output.is_none() {
            self.output = Some(self.gpu.create_image(&storage(width, height))?);
        }
        Ok(())
    }

    /// Drop the output, for a swapchain whose images - and composite - are
    /// being replaced. The caller has waited for every frame in flight.
    pub(super) fn forget_output(&mut self) {
        if let Some(output) = self.output.take() {
            self.gpu.destroy_image(output);
        }
    }

    /// Record the pass from `composite` into the output [`Self::prepare`]
    /// made, and return the output, left ready for a transfer read.
    ///
    /// `slot` is a frame in flight whose fence has been waited, which is what
    /// makes rewriting its descriptor set safe.
    ///
    /// # Safety
    ///
    /// Recording into `command_buffer`, with `composite` a [`storage`] image
    /// of `shape`'s size in `GENERAL`, written by transfers earlier in it.
    pub(super) unsafe fn record(
        &self,
        command_buffer: vk::CommandBuffer,
        slot: usize,
        composite: &GpuImage,
        shape: Shape,
    ) -> vk::Image {
        let prepared = self.output.as_ref().expect("an output made by prepare");
        debug_assert_eq!(
            (prepared.width, prepared.height),
            (shape.width, shape.height)
        );
        let (output, output_view) = (prepared.image, prepared.view);
        let set = self.sets[slot % self.sets.len()];
        let source = [vk::DescriptorImageInfo::default()
            .image_view(composite.view)
            .image_layout(vk::ImageLayout::GENERAL)];
        let target = [vk::DescriptorImageInfo::default()
            .image_view(output_view)
            .image_layout(vk::ImageLayout::GENERAL)];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&source),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&target),
        ];
        let device = &self.gpu.device;
        // SAFETY: the set belongs to a frame whose submission has retired,
        // and the caller's contract covers the recording.
        unsafe {
            device.update_descriptor_sets(&writes, &[]);
            // The composite's blits land before the shader reads it; the
            // last frame's blit out of the output is done before this one
            // overwrites it.
            barriers(
                device,
                command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                &[
                    (
                        composite.image,
                        vk::AccessFlags::TRANSFER_WRITE,
                        vk::AccessFlags::SHADER_READ,
                    ),
                    (
                        output,
                        vk::AccessFlags::empty(),
                        vk::AccessFlags::SHADER_WRITE,
                    ),
                ],
            );
            device.cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline,
            );
            device.cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline_layout,
                0,
                &[set],
                &[],
            );
            device.cmd_push_constants(
                command_buffer,
                self.pipeline_layout,
                vk::ShaderStageFlags::COMPUTE,
                0,
                &shape.bytes(),
            );
            device.cmd_dispatch(
                command_buffer,
                shape.width.div_ceil(GROUP),
                shape.height.div_ceil(GROUP),
                1,
            );
            // The output is blitted next; the composite is written by the
            // next frame's blits, which must wait for this read.
            barriers(
                device,
                command_buffer,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::TRANSFER,
                &[
                    (
                        output,
                        vk::AccessFlags::SHADER_WRITE,
                        vk::AccessFlags::TRANSFER_READ,
                    ),
                    (
                        composite.image,
                        vk::AccessFlags::empty(),
                        vk::AccessFlags::TRANSFER_WRITE,
                    ),
                ],
            );
        }
        output
    }
}

impl Drop for Corners {
    /// The owning swapchain has waited for every frame in flight.
    fn drop(&mut self) {
        self.forget_output();
        let device = &self.gpu.device;
        // SAFETY: objects of this device's that no pending work refers to.
        unsafe {
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            device.destroy_descriptor_pool(self.pool, None);
        }
    }
}

/// An image the pass reads or writes: see [`FORMAT`].
pub(super) fn storage(width: u32, height: u32) -> ImageDescription {
    ImageDescription {
        image_type: vk::ImageType::TYPE_2D,
        view_type: vk::ImageViewType::TYPE_2D,
        format: FORMAT,
        width,
        height,
        depth: 1,
        levels: 1,
        layers: 1,
        samples: 1,
        usage: gpu::USAGE_SHADER_WRITE,
        cube: false,
    }
}

/// # Safety
///
/// Recording into `command_buffer`; every image belongs to `device` and is
/// in `GENERAL`.
unsafe fn barriers(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    from: vk::PipelineStageFlags,
    to: vk::PipelineStageFlags,
    images: &[(vk::Image, vk::AccessFlags, vk::AccessFlags)],
) {
    let barriers: Vec<_> = images
        .iter()
        .map(|&(image, source, target)| {
            vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_access_mask(source)
                .dst_access_mask(target)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
        })
        .collect();
    // SAFETY: as the caller promises.
    unsafe {
        device.cmd_pipeline_barrier(
            command_buffer,
            from,
            to,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &barriers,
        )
    };
}

/// GLSL.std.450 instruction numbers.
const UMIN: u32 = 38;
const FCLAMP: u32 = 43;
const LENGTH: u32 = 66;

/// The pass, as SPIR-V. In GLSL:
///
/// ```glsl
/// layout(local_size_x = 8, local_size_y = 8) in;
/// layout(binding = 0, rgba8) readonly uniform image2D source;
/// layout(binding = 1, rgba8) writeonly uniform image2D target;
/// layout(push_constant) uniform Shape { uint width, height, top; float radius; };
/// void main() {
///     uvec2 id = gl_GlobalInvocationID.xy;
///     if (id.x < width && id.y < height) {
///         vec4 pixel = imageLoad(source, ivec2(id));
///         if (id.y >= top) pixel.a = 1.0;
///         vec2 from = vec2(min(id.x, width - 1 - id.x), height - 1 - id.y) + 0.5;
///         if (from.x < radius && from.y < radius)
///             pixel *= clamp(radius - length(radius - from) + 0.5, 0.0, 1.0);
///         imageStore(target, ivec2(id), pixel);
///     }
/// }
/// ```
///
/// Built here rather than compiled, so that the build needs no shader
/// toolchain; it cannot fail on its own input.
fn shader() -> Vec<u32> {
    build_shader().expect("the corners shader is well formed")
}

fn build_shader() -> Result<Vec<u32>, rspirv::dr::Error> {
    use spirv::{Decoration, StorageClass};
    let mut b = Builder::new();
    b.set_version(1, 0);
    b.capability(spirv::Capability::Shader);
    let glsl = b.ext_inst_import("GLSL.std.450");
    b.memory_model(spirv::AddressingModel::Logical, spirv::MemoryModel::GLSL450);

    let void = b.type_void();
    let boolean = b.type_bool();
    let uint = b.type_int(32, 0);
    let int = b.type_int(32, 1);
    let float = b.type_float(32, None);
    let uvec3 = b.type_vector(uint, 3);
    let uvec2 = b.type_vector(uint, 2);
    let ivec2 = b.type_vector(int, 2);
    let vec2 = b.type_vector(float, 2);
    let vec4 = b.type_vector(float, 4);
    let image = b.type_image(
        float,
        spirv::Dim::Dim2D,
        0,
        0,
        0,
        2,
        spirv::ImageFormat::Rgba8,
        None,
    );
    let image_pointer = b.type_pointer(None, StorageClass::UniformConstant, image);
    let source = b.variable(image_pointer, None, StorageClass::UniformConstant, None);
    let target = b.variable(image_pointer, None, StorageClass::UniformConstant, None);
    for (variable, binding, access) in [
        (source, 0, Decoration::NonWritable),
        (target, 1, Decoration::NonReadable),
    ] {
        b.decorate(
            variable,
            Decoration::DescriptorSet,
            [Operand::LiteralBit32(0)],
        );
        b.decorate(
            variable,
            Decoration::Binding,
            [Operand::LiteralBit32(binding)],
        );
        b.decorate(variable, access, []);
    }

    let shape = b.type_struct([uint, uint, uint, float]);
    b.decorate(shape, Decoration::Block, []);
    for member in 0..4 {
        b.member_decorate(
            shape,
            member,
            Decoration::Offset,
            [Operand::LiteralBit32(member * 4)],
        );
    }
    let shape_pointer = b.type_pointer(None, StorageClass::PushConstant, shape);
    let shape = b.variable(shape_pointer, None, StorageClass::PushConstant, None);
    let uint_constant_pointer = b.type_pointer(None, StorageClass::PushConstant, uint);
    let float_constant_pointer = b.type_pointer(None, StorageClass::PushConstant, float);

    let id_pointer = b.type_pointer(None, StorageClass::Input, uvec3);
    let id = b.variable(id_pointer, None, StorageClass::Input, None);
    b.decorate(
        id,
        Decoration::BuiltIn,
        [Operand::BuiltIn(spirv::BuiltIn::GlobalInvocationId)],
    );

    let index = |b: &mut Builder, value: u32| b.constant_bit32(int, value);
    let one_uint = b.constant_bit32(uint, 1);
    let zero = b.constant_bit32(float, 0f32.to_bits());
    let half = b.constant_bit32(float, 0.5f32.to_bits());
    let one = b.constant_bit32(float, 1f32.to_bits());

    let function = b.type_function(void, []);
    let main = b.begin_function(void, None, spirv::FunctionControl::NONE, function)?;
    b.begin_block(None)?;
    let member = |b: &mut Builder, at: u32, pointer: u32, ty: u32| {
        let at = index(b, at);
        let address = b.access_chain(pointer, None, shape, [at])?;
        b.load(ty, None, address, None, [])
    };
    let width = member(&mut b, 0, uint_constant_pointer, uint)?;
    let height = member(&mut b, 1, uint_constant_pointer, uint)?;
    let top = member(&mut b, 2, uint_constant_pointer, uint)?;
    let radius = member(&mut b, 3, float_constant_pointer, float)?;
    let global = b.load(uvec3, None, id, None, [])?;
    let x = b.composite_extract(uint, None, global, [0])?;
    let y = b.composite_extract(uint, None, global, [1])?;
    let inside_x = b.u_less_than(boolean, None, x, width)?;
    let inside_y = b.u_less_than(boolean, None, y, height)?;
    let inside = b.logical_and(boolean, None, inside_x, inside_y)?;
    let body = b.id();
    let merge = b.id();
    b.selection_merge(merge, spirv::SelectionControl::NONE)?;
    b.branch_conditional(inside, body, merge, [])?;

    b.begin_block(Some(body))?;
    let at = b.composite_construct(uvec2, None, [x, y])?;
    let at = b.bitcast(ivec2, None, at)?;
    let loaded = b.load(image, None, source, None, [])?;
    let pixel = b.image_read(vec4, None, loaded, at, None, [])?;
    // Content rows are opaque. Premultiplied, that is the pixel as if drawn
    // over black - which is what an opaque window shows for it too.
    let content = b.u_greater_than_equal(boolean, None, y, top)?;
    let alpha = b.composite_extract(float, None, pixel, [3])?;
    let alpha = b.select(float, None, content, one, alpha)?;
    let pixel = b.composite_insert(vec4, None, alpha, pixel, [3])?;

    let last_column = b.i_sub(uint, None, width, one_uint)?;
    let last_row = b.i_sub(uint, None, height, one_uint)?;
    let from_right = b.i_sub(uint, None, last_column, x)?;
    let column = b.ext_inst(
        uint,
        None,
        glsl,
        UMIN,
        [Operand::IdRef(x), Operand::IdRef(from_right)],
    )?;
    let row = b.i_sub(uint, None, last_row, y)?;
    let column = b.convert_u_to_f(float, None, column)?;
    let row = b.convert_u_to_f(float, None, row)?;
    let column = b.f_add(float, None, column, half)?;
    let row = b.f_add(float, None, row, half)?;
    let near_x = b.f_ord_less_than(boolean, None, column, radius)?;
    let near_y = b.f_ord_less_than(boolean, None, row, radius)?;
    let corner = b.logical_and(boolean, None, near_x, near_y)?;
    let across = b.f_sub(float, None, radius, column)?;
    let down = b.f_sub(float, None, radius, row)?;
    let offset = b.composite_construct(vec2, None, [across, down])?;
    let distance = b.ext_inst(float, None, glsl, LENGTH, [Operand::IdRef(offset)])?;
    let coverage = b.f_sub(float, None, radius, distance)?;
    let coverage = b.f_add(float, None, coverage, half)?;
    let coverage = b.ext_inst(
        float,
        None,
        glsl,
        FCLAMP,
        [
            Operand::IdRef(coverage),
            Operand::IdRef(zero),
            Operand::IdRef(one),
        ],
    )?;
    let coverage = b.select(float, None, corner, coverage, one)?;
    let pixel = b.vector_times_scalar(vec4, None, pixel, coverage)?;
    let stored = b.load(image, None, target, None, [])?;
    b.image_write(stored, at, pixel, None, [])?;
    b.branch(merge)?;

    b.begin_block(Some(merge))?;
    b.ret()?;
    b.end_function()?;
    b.entry_point(spirv::ExecutionModel::GLCompute, main, "main", [id]);
    b.execution_mode(main, spirv::ExecutionMode::LocalSize, [GROUP, GROUP, 1]);
    Ok(b.module().assemble())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_corners_shader_is_one_compute_entry_point() {
        let words = shader();
        let module = rspirv::dr::load_words(&words).expect("SPIR-V that loads back");
        assert_eq!(module.entry_points.len(), 1);
        assert_eq!(module.functions.len(), 1);
        assert_eq!(words[0], spirv::MAGIC_NUMBER);
    }

    /// The pass run on a real device, over a composite whose every pixel has
    /// the colour a guest might leave: some, with no alpha at all.
    /// `ROSETTA3_TEST_VULKAN=1` asks for it.
    #[test]
    fn content_is_sealed_and_the_bottom_corners_cut() {
        if std::env::var_os("ROSETTA3_TEST_VULKAN").is_none() {
            return;
        }
        let Ok(adapters) = crate::vulkan::adapters() else {
            return;
        };
        let Some(gpu) = adapters.first().and_then(|adapter| adapter.gpu().ok()) else {
            return;
        };
        let shape = Shape {
            width: 64,
            height: 48,
            top: 10,
            radius: 8.0,
        };
        let composite = gpu
            .create_image(&storage(shape.width, shape.height))
            .expect("a storage image");
        let readback = gpu
            .create_buffer(u64::from(shape.width * shape.height * 4))
            .expect("a readback buffer");
        let mut corners = Corners::new(gpu).expect("the corners pipeline");
        corners
            .prepare(shape.width, shape.height)
            .expect("an output image");
        gpu.one_shot(|device, command_buffer| {
            let clear = vk::ClearColorValue {
                float32: [0.2, 0.4, 0.6, 0.0],
            };
            let range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };
            let copy = vk::BufferImageCopy::default()
                .image_subresource(super::super::present::whole_color())
                .image_extent(vk::Extent3D {
                    width: shape.width,
                    height: shape.height,
                    depth: 1,
                });
            // SAFETY: recording into the call's command buffer; every image
            // is in GENERAL from creation.
            unsafe {
                device.cmd_clear_color_image(
                    command_buffer,
                    composite.image,
                    vk::ImageLayout::GENERAL,
                    &clear,
                    &[range],
                );
                let output = corners.record(command_buffer, 0, &composite, shape);
                device.cmd_copy_image_to_buffer(
                    command_buffer,
                    output,
                    vk::ImageLayout::GENERAL,
                    readback.buffer,
                    &[copy],
                );
            }
        })
        .expect("the pass ran");
        // SAFETY: the copy has finished, and the buffer is mapped whole.
        let pixels =
            unsafe { std::slice::from_raw_parts(readback.contents, readback.length as usize) };
        let at = |x: u32, y: u32| {
            let start = ((y * shape.width + x) * 4) as usize;
            [
                pixels[start],
                pixels[start + 1],
                pixels[start + 2],
                pixels[start + 3],
            ]
        };
        let bottom = shape.height - 1;
        let right = shape.width - 1;
        // The chrome's rows keep their own alpha, top corners and all.
        assert_eq!(at(0, 0), [51, 102, 153, 0]);
        assert_eq!(at(32, 5), [51, 102, 153, 0]);
        // Content is opaque, as if drawn over black.
        assert_eq!(at(32, 30), [51, 102, 153, 255]);
        assert_eq!(at(32, bottom), [51, 102, 153, 255]);
        assert_eq!(at(0, 30), [51, 102, 153, 255]);
        // The bottom corners are cut, and blended where the edge crosses.
        assert_eq!(at(0, bottom), [0; 4]);
        assert_eq!(at(right, bottom), [0; 4]);
        let edge = at(2, bottom - 2)[3];
        assert!(
            (170..200).contains(&edge),
            "an edge pixel's alpha was {edge}"
        );
        assert_eq!(at(right - 2, bottom - 2)[3], edge);

        drop(corners);
        gpu.destroy_buffer(readback);
        gpu.destroy_image(composite);
    }

    #[test]
    fn a_shape_is_laid_out_as_the_shader_reads_it() {
        let shape = Shape {
            width: 1,
            height: 2,
            top: 3,
            radius: 4.0,
        };
        let bytes = shape.bytes();
        assert_eq!(&bytes[0..4], &1u32.to_ne_bytes());
        assert_eq!(&bytes[8..12], &3u32.to_ne_bytes());
        assert_eq!(&bytes[12..16], &4f32.to_bits().to_ne_bytes());
    }
}
