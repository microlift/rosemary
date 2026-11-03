//! Occlusion queries: `-setVisibilityResultMode:offset:` and the pass's
//! `visibilityResultBuffer`.
//!
//! Each run of draws with visibility on is one Vulkan occlusion query. Metal
//! adds every run in a pass which names an offset into that one result, and
//! Vulkan has no command which adds, so when the pass ends its query results
//! are copied out and a small compute shader sums each offset's into the
//! guest's buffer. Only the offsets the pass used are written.

use super::*;
use rspirv::binary::Assemble;
use rspirv::dr::{Builder, Operand};
use rspirv::spirv;

/// `MTLVisibilityResultMode`.
const MTL_VISIBILITY_RESULT_MODE_DISABLED: u64 = 0;
const MTL_VISIBILITY_RESULT_MODE_BOOLEAN: u64 = 1;
const MTL_VISIBILITY_RESULT_MODE_COUNTING: u64 = 2;

/// `MTLVisibilityResultTypeAccumulate`: a pass adds to what the buffer holds
/// rather than starting each offset it uses from zero.
const MTL_VISIBILITY_RESULT_TYPE_ACCUMULATE: u64 = 1;

/// One result is a 64-bit count, and an offset must be a multiple of it.
const RESULT_SIZE: u64 = 8;

/// Invocations per workgroup of the summation; one per offset.
const GROUP: u32 = 64;

/// The buffer a render pass writes its visibility results to.
#[derive(Clone, Copy)]
pub(super) struct VisibilityTarget {
    pub buffer: Id,
    pub accumulate: bool,
}

impl VisibilityTarget {
    pub(super) fn of(descriptor: &RenderPassDescriptor) -> Option<Self> {
        (!descriptor.visibility_result_buffer.is_null()).then_some(Self {
            buffer: descriptor.visibility_result_buffer,
            accumulate: descriptor.visibility_result_type == MTL_VISIBILITY_RESULT_TYPE_ACCUMULATE,
        })
    }
}

/// One occlusion query and the offset its count belongs to.
#[derive(Clone, Copy)]
pub(super) struct Segment {
    offset: u64,
    pool: vk::QueryPool,
    query: u32,
}

/// A command buffer's occlusion queries.
#[derive(Default)]
pub(super) struct Occlusion {
    /// Pools whose queries are all taken, and pools adopted from sub-encoders.
    spent: Vec<vk::QueryPool>,
    /// The pool queries are being taken from, and the next free one in it.
    current: Option<(vk::QueryPool, u32)>,
    active: Option<Segment>,
    /// The queries of the pass being recorded which have ended.
    segments: Vec<Segment>,
}

impl Occlusion {
    fn begin(
        &mut self,
        gpu: &Gpu,
        command_buffer: vk::CommandBuffer,
        offset: u64,
        flags: vk::QueryControlFlags,
    ) -> Result<(), String> {
        let (pool, query) = match self.current {
            Some((pool, next)) if next < gpu::OCCLUSION_QUERIES => (pool, next),
            _ => {
                let pool = gpu.acquire_occlusion_pool()?;
                self.spent.extend(self.current.map(|(pool, _)| pool));
                (pool, 0)
            }
        };
        self.current = Some((pool, query + 1));
        // SAFETY: recording inside a pass; the query was reset when its pool
        // was taken and has not been used since.
        unsafe {
            gpu.device
                .cmd_begin_query(command_buffer, pool, query, flags)
        };
        self.active = Some(Segment {
            offset,
            pool,
            query,
        });
        Ok(())
    }

    /// End the active query, if there is one. It must be ended in the
    /// command buffer and the pass it began in.
    pub(super) fn end(&mut self, device: &ash::Device, command_buffer: vk::CommandBuffer) {
        if let Some(segment) = self.active.take() {
            // SAFETY: the query began in this command buffer, in this pass.
            unsafe { device.cmd_end_query(command_buffer, segment.pool, segment.query) };
            self.segments.push(segment);
        }
    }

    /// Take over a parallel sub-encoder's pools and the queries it recorded.
    pub(super) fn adopt(&mut self, child: &mut Occlusion) {
        self.spent.append(&mut child.take_pools());
        self.segments.append(&mut child.segments);
    }

    /// Every pool this recording took, to be released once its work is done.
    pub(super) fn take_pools(&mut self) -> Vec<vk::QueryPool> {
        let mut pools = std::mem::take(&mut self.spent);
        pools.extend(self.current.take().map(|(pool, _)| pool));
        pools
    }
}

/// `-[MTLRenderCommandEncoder setVisibilityResultMode:offset:]`.
pub(super) unsafe extern "C" fn set_visibility_result_mode(
    receiver: Id,
    _sel: Sel,
    mode: u64,
    offset: u64,
) {
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    if encoder.kind != Kind::Render || !encoder.in_render_pass {
        return;
    }
    let command = encoder.command();
    let gpu = command.gpu;
    command.occlusion.end(&gpu.device, command.command_buffer);
    let flags = match mode {
        MTL_VISIBILITY_RESULT_MODE_DISABLED => return,
        MTL_VISIBILITY_RESULT_MODE_BOOLEAN => vk::QueryControlFlags::empty(),
        MTL_VISIBILITY_RESULT_MODE_COUNTING if gpu.occlusion_precise => {
            vk::QueryControlFlags::PRECISE
        }
        MTL_VISIBILITY_RESULT_MODE_COUNTING => {
            // FIXME (ACCURACY): without occlusionQueryPrecise a count is only
            // guaranteed to be non-zero when a sample passed.
            gpu::trace_once("MTLVisibilityResultModeCounting counts imprecisely on this device");
            vk::QueryControlFlags::empty()
        }
        _ => return gpu::trace(format!("setVisibilityResultMode: unknown mode {mode}")),
    };
    let Some(target) = encoder.visibility else {
        return gpu::trace_once(
            "setVisibilityResultMode: in a pass with no visibilityResultBuffer",
        );
    };
    // SAFETY: only a buffer of this crate's is a visibility result buffer.
    let length = unsafe { object::body::<Buffer>(target.buffer) }
        .inner
        .length;
    if !offset.is_multiple_of(RESULT_SIZE) || offset.saturating_add(RESULT_SIZE) > length {
        return gpu::trace(format!(
            "setVisibilityResultMode: offset {offset} is not a result in a {length}-byte buffer"
        ));
    }
    if let Err(reason) = command
        .occlusion
        .begin(gpu, command.command_buffer, offset, flags)
    {
        gpu::trace_once(format!("visibility results are not counted: {reason}"));
    }
}

/// `-[MTLRenderPassDescriptor visibilityResultType]`.
pub(super) unsafe extern "C" fn pass_visibility_result_type(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a render pass descriptor of this crate's.
    unsafe { object::body::<RenderPassDescriptor>(receiver) }.visibility_result_type
}

/// `-[MTLRenderPassDescriptor setVisibilityResultType:]`.
pub(super) unsafe extern "C" fn set_pass_visibility_result_type(
    receiver: Id,
    _sel: Sel,
    value: u64,
) {
    // SAFETY: a render pass descriptor of this crate's.
    unsafe { object::body::<RenderPassDescriptor>(receiver) }.visibility_result_type = value;
}

/// Write the results of the pass just ended. Called outside the pass, on the
/// command buffer which recorded it.
pub(super) fn resolve(encoder: &Encoder) {
    let command = encoder.command();
    let segments = std::mem::take(&mut command.occlusion.segments);
    let Some(target) = encoder.visibility else {
        return;
    };
    if segments.is_empty() {
        return;
    }
    if let Err(reason) = record_resolve(command, target, segments) {
        gpu::trace(format!("visibility results are not written: {reason}"));
    }
}

fn record_resolve(
    command: &mut CommandBuffer,
    target: VisibilityTarget,
    mut segments: Vec<Segment>,
) -> Result<(), String> {
    let gpu = command.gpu;
    let device = &gpu.device;
    let summation = summation(gpu)?;
    // Each offset's queries side by side, so the shader sums a range.
    segments.sort_by_key(|segment| segment.offset);
    let alignment = gpu.limits.min_storage_buffer_offset_alignment.max(1);
    let base = segments[0].offset / alignment * alignment;
    let end = segments[segments.len() - 1].offset + RESULT_SIZE;

    // Per offset: its first word relative to `base`, its first query and how
    // many queries it has.
    let mut plan: Vec<[u32; 3]> = Vec::new();
    let mut previous = None;
    for (at, segment) in segments.iter().enumerate() {
        match plan.last_mut() {
            Some(entry) if previous == Some(segment.offset) => entry[2] += 1,
            _ => plan.push([((segment.offset - base) / 4) as u32, at as u32, 1]),
        }
        previous = Some(segment.offset);
    }
    let entries = plan.len() as u32;
    let plan: Vec<u8> = plan
        .iter()
        .flatten()
        .flat_map(|word| word.to_ne_bytes())
        .collect();
    let results_size = segments.len() as u64 * RESULT_SIZE;
    if results_size.max(plan.len() as u64) > gpu::UPLOAD_CHUNK {
        return Err(format!("{} queries in one pass", segments.len()));
    }
    let (results, results_offset) = command
        .upload(&vec![0; results_size as usize])
        .ok_or("no upload space for the query results")?;
    let (plan_buffer, plan_offset) = command
        .upload(&plan)
        .ok_or("no upload space for the summation plan")?;
    let set = command
        .allocate_set(summation.set_layout)
        .ok_or("no descriptor set for the summation")?;
    // SAFETY: a buffer of this crate's, retained when the pass began.
    let visibility = unsafe { object::body::<Buffer>(target.buffer) }.device_buffer();

    let infos = [
        [vk::DescriptorBufferInfo::default()
            .buffer(results)
            .offset(results_offset)
            .range(results_size)],
        [vk::DescriptorBufferInfo::default()
            .buffer(plan_buffer)
            .offset(plan_offset)
            .range(plan.len() as u64)],
        [vk::DescriptorBufferInfo::default()
            .buffer(visibility)
            .offset(base)
            .range(end - base)],
    ];
    let writes: Vec<_> = infos
        .iter()
        .enumerate()
        .map(|(binding, info)| {
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(binding as u32)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(info)
        })
        .collect();
    let constants: Vec<u8> = [entries, u32::from(target.accumulate)]
        .iter()
        .flat_map(|word| word.to_ne_bytes())
        .collect();
    let command_buffer = command.command_buffer;
    // SAFETY: recording into this command buffer outside any pass; every
    // query has ended, and every handle belongs to this device.
    unsafe {
        device.update_descriptor_sets(&writes, &[]);
        for (at, segment) in segments.iter().enumerate() {
            device.cmd_copy_query_pool_results(
                command_buffer,
                segment.pool,
                segment.query,
                1,
                results,
                results_offset + at as u64 * RESULT_SIZE,
                RESULT_SIZE,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
            );
        }
        let copied = vk::MemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ);
        device.cmd_pipeline_barrier(
            command_buffer,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::DependencyFlags::empty(),
            &[copied],
            &[],
            &[],
        );
        device.cmd_bind_pipeline(
            command_buffer,
            vk::PipelineBindPoint::COMPUTE,
            summation.pipeline,
        );
        device.cmd_bind_descriptor_sets(
            command_buffer,
            vk::PipelineBindPoint::COMPUTE,
            summation.layout,
            0,
            &[set],
            &[],
        );
        device.cmd_push_constants(
            command_buffer,
            summation.layout,
            vk::ShaderStageFlags::COMPUTE,
            0,
            &constants,
        );
        device.cmd_dispatch(command_buffer, entries.div_ceil(GROUP), 1, 1);
    }
    Ok(())
}

/// The summation's pipeline on one device.
#[derive(Clone, Copy)]
struct Summation {
    gpu: &'static Gpu,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}

// SAFETY: Vulkan handles, used under `SUMMATIONS`.
unsafe impl Send for Summation {}

/// One per device, made on first use and kept for the run.
static SUMMATIONS: Mutex<Vec<Summation>> = Mutex::new(Vec::new());

fn summation(gpu: &'static Gpu) -> Result<Summation, String> {
    let mut summations = SUMMATIONS.lock().expect("visibility summations");
    if let Some(summation) = summations
        .iter()
        .find(|summation| std::ptr::eq(summation.gpu, gpu))
    {
        return Ok(*summation);
    }
    let summation = Summation::new(gpu)?;
    summations.push(summation);
    Ok(summation)
}

impl Summation {
    fn new(gpu: &'static Gpu) -> Result<Self, String> {
        let device = &gpu.device;
        let set_layout = gpu.descriptor_set_layout(
            &[
                (0, vk::DescriptorType::STORAGE_BUFFER),
                (1, vk::DescriptorType::STORAGE_BUFFER),
                (2, vk::DescriptorType::STORAGE_BUFFER),
            ],
            vk::ShaderStageFlags::COMPUTE,
            false,
        )?;
        let push = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
            .size(8)];
        let set_layouts = [set_layout];
        let info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&push);
        // SAFETY: a well-formed create info on this live device.
        let layout = unsafe { device.create_pipeline_layout(&info, None) }
            .map_err(|error| format!("vkCreatePipelineLayout failed with {error:?}"))?;
        let layout = gpu::Guard::new(layout, |layout| {
            // SAFETY: a layout of this device's that nothing uses yet.
            unsafe { device.destroy_pipeline_layout(layout, None) };
        });
        let module = gpu.create_shader_module(&shader(), "rosemary_visibility_sum")?;
        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(module)
            .name(c"main");
        let info = vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(*layout);
        // SAFETY: every borrowed structure lives across the call.
        let pipeline =
            unsafe { device.create_compute_pipelines(vk::PipelineCache::null(), &[info], None) };
        // SAFETY: a pipeline keeps nothing of the module it was made from.
        unsafe { device.destroy_shader_module(module, None) };
        let pipeline = pipeline
            .map_err(|(_, error)| format!("vkCreateComputePipelines failed with {error:?}"))?[0];
        Ok(Self {
            gpu,
            set_layout,
            layout: layout.take(),
            pipeline,
        })
    }
}

fn shader() -> Vec<u32> {
    build_shader().expect("the visibility summation shader is well formed")
}

/// One invocation per plan entry: `visibility[destination]`, a 64-bit count
/// as two words, becomes the sum of `count` results from `first` on, plus
/// what it held when the pass accumulates. 64-bit integers are optional in
/// Vulkan, so the carry is propagated by hand.
fn build_shader() -> Result<Vec<u32>, rspirv::dr::Error> {
    use spirv::{Decoration, StorageClass};
    let mut b = Builder::new();
    b.set_version(1, 3);
    b.capability(spirv::Capability::Shader);
    b.memory_model(spirv::AddressingModel::Logical, spirv::MemoryModel::GLSL450);

    let void = b.type_void();
    let boolean = b.type_bool();
    let uint = b.type_int(32, 0);
    let uvec3 = b.type_vector(uint, 3);
    let words = b.type_runtime_array(uint);
    b.decorate(words, Decoration::ArrayStride, [Operand::LiteralBit32(4)]);
    let block = b.type_struct([words]);
    b.decorate(block, Decoration::Block, []);
    b.member_decorate(block, 0, Decoration::Offset, [Operand::LiteralBit32(0)]);
    let block_pointer = b.type_pointer(None, StorageClass::StorageBuffer, block);
    let word_pointer = b.type_pointer(None, StorageClass::StorageBuffer, uint);
    let [results, plan, visibility] = [0, 1, 2].map(|binding| {
        let variable = b.variable(block_pointer, None, StorageClass::StorageBuffer, None);
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
        variable
    });

    // The number of plan entries, and whether the pass accumulates.
    let constants = b.type_struct([uint, uint]);
    b.decorate(constants, Decoration::Block, []);
    for member in 0..2 {
        b.member_decorate(
            constants,
            member,
            Decoration::Offset,
            [Operand::LiteralBit32(member * 4)],
        );
    }
    let constants_pointer = b.type_pointer(None, StorageClass::PushConstant, constants);
    let constants = b.variable(constants_pointer, None, StorageClass::PushConstant, None);
    let constant_pointer = b.type_pointer(None, StorageClass::PushConstant, uint);

    let id_pointer = b.type_pointer(None, StorageClass::Input, uvec3);
    let id = b.variable(id_pointer, None, StorageClass::Input, None);
    b.decorate(
        id,
        Decoration::BuiltIn,
        [Operand::BuiltIn(spirv::BuiltIn::GlobalInvocationId)],
    );

    let zero = b.constant_bit32(uint, 0);
    let one = b.constant_bit32(uint, 1);
    let two = b.constant_bit32(uint, 2);
    let three = b.constant_bit32(uint, 3);

    let function = b.type_function(void, []);
    let main = b.begin_function(void, None, spirv::FunctionControl::NONE, function)?;
    b.begin_block(None)?;
    let word = |b: &mut Builder, buffer: u32, at: u32| {
        let address = b.access_chain(word_pointer, None, buffer, [zero, at])?;
        b.load(uint, None, address, None, [])
    };
    let global = b.load(uvec3, None, id, None, [])?;
    let entry = b.composite_extract(uint, None, global, [0])?;
    let address = b.access_chain(constant_pointer, None, constants, [zero])?;
    let entries = b.load(uint, None, address, None, [])?;
    let address = b.access_chain(constant_pointer, None, constants, [one])?;
    let accumulate = b.load(uint, None, address, None, [])?;
    let inside = b.u_less_than(boolean, None, entry, entries)?;
    let body = b.id();
    let done = b.id();
    b.selection_merge(done, spirv::SelectionControl::NONE)?;
    b.branch_conditional(inside, body, done, [])?;

    b.begin_block(Some(body))?;
    let at = b.i_mul(uint, None, entry, three)?;
    let destination = word(&mut b, plan, at)?;
    let at = b.i_add(uint, None, at, one)?;
    let first = word(&mut b, plan, at)?;
    let at = b.i_add(uint, None, at, one)?;
    let count = word(&mut b, plan, at)?;
    let destination_high = b.i_add(uint, None, destination, one)?;
    let held_low = word(&mut b, visibility, destination)?;
    let held_high = word(&mut b, visibility, destination_high)?;
    let keep = b.i_not_equal(boolean, None, accumulate, zero)?;
    let start_low = b.select(uint, None, keep, held_low, zero)?;
    let start_high = b.select(uint, None, keep, held_high, zero)?;
    let header = b.id();
    let add = b.id();
    let latch = b.id();
    let merge = b.id();
    b.branch(header)?;

    b.begin_block(Some(header))?;
    let next_index = b.id();
    let next_low = b.id();
    let next_high = b.id();
    let index = b.phi(uint, None, [(zero, body), (next_index, latch)])?;
    let low = b.phi(uint, None, [(start_low, body), (next_low, latch)])?;
    let high = b.phi(uint, None, [(start_high, body), (next_high, latch)])?;
    let more = b.u_less_than(boolean, None, index, count)?;
    b.loop_merge(merge, latch, spirv::LoopControl::NONE, [])?;
    b.branch_conditional(more, add, merge, [])?;

    b.begin_block(Some(add))?;
    let query = b.i_add(uint, None, first, index)?;
    let at = b.i_mul(uint, None, query, two)?;
    let add_low = word(&mut b, results, at)?;
    let at = b.i_add(uint, None, at, one)?;
    let add_high = word(&mut b, results, at)?;
    b.i_add(uint, Some(next_low), low, add_low)?;
    let wrapped = b.u_less_than(boolean, None, next_low, low)?;
    let carry = b.select(uint, None, wrapped, one, zero)?;
    let high_sum = b.i_add(uint, None, high, add_high)?;
    b.i_add(uint, Some(next_high), high_sum, carry)?;
    b.i_add(uint, Some(next_index), index, one)?;
    b.branch(latch)?;

    b.begin_block(Some(latch))?;
    b.branch(header)?;

    b.begin_block(Some(merge))?;
    for (at, value) in [(destination, low), (destination_high, high)] {
        let address = b.access_chain(word_pointer, None, visibility, [zero, at])?;
        b.store(address, value, None, [])?;
    }
    b.branch(done)?;

    b.begin_block(Some(done))?;
    b.ret()?;
    b.end_function()?;
    b.entry_point(spirv::ExecutionModel::GLCompute, main, "main", [id]);
    b.execution_mode(main, spirv::ExecutionMode::LocalSize, [GROUP, 1, 1]);
    Ok(b.module().assemble())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summation_shader_is_one_compute_entry_point() {
        let words = shader();
        let module = rspirv::dr::load_words(&words).expect("SPIR-V that loads back");
        assert_eq!(module.entry_points.len(), 1);
        assert_eq!(words[0], spirv::MAGIC_NUMBER);
    }
}
