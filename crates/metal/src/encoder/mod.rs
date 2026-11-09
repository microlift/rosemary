mod bind;
mod blit;
mod command_buffer;
mod compute;
mod depth_bias;
mod hazards;
mod parallel;
mod pass;
mod queue;
mod render;
mod retire;
mod state;
mod target;
mod visibility;

use bind::*;
use blit::*;
use command_buffer::*;
use compute::*;
use depth_bias::*;
use hazards::*;
use parallel::*;
use pass::*;
use queue::*;
use render::*;
use retire::*;
use state::*;
use target::*;
use visibility::*;

use objc::address::AddressSet;

pub(crate) use pass::RenderPassDescriptor;
pub(crate) use queue::new_command_queue;
pub(crate) use retire::Submission;

use ash::vk;
use metal_translate::ir::{ResourceKind, Stage};
use objc::object::Id;
use objc::sel::Sel;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};

use crate::gpu::{
    self, CommandContext, DepthResolve, Gpu, GpuBuffer, RenderPassKey, ResolveSupport,
};
use crate::object::{self, methods};
use crate::pipeline::{
    BUFFER_SET, ComputePipeline, DepthStencilKey, Layouts, RenderDynamicState, RenderPipeline,
    RenderVariantKey, SETS, binding_type,
};
use crate::resource::{Buffer, DepthStencilState, SamplerState, Texture};
use crate::stats::{self, Counter};

#[cfg(test)]
pub(crate) static TEST_RECORDED_BARRIERS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Metal's per-stage binding tables. Index by `Stage::ordinal`.
const STAGES: usize = 3;
const SLOTS: usize = 32;

/// A bound buffer, resolved to its Vulkan handle when it is bound so a draw
/// does not reach through the object for it.
#[derive(Clone, Copy)]
struct BufferBinding {
    buffer: vk::Buffer,
    offset: u64,
    /// The offset past which the binding has no bytes.
    end: u64,
    /// The descriptor's range: the rest of a guest buffer, or exactly the
    /// bytes a `-setBytes:` copied.
    range: u64,
    /// Whether `buffer` is an upload chunk `-setBytes:` copied into, whose
    /// offset can be a dynamic one.
    uploaded: bool,
    /// The `MTLBuffer` bound, which the command buffer has retained, or null
    /// for `-setBytes:` copies.
    object: Id,
}

impl Default for BufferBinding {
    fn default() -> Self {
        Self {
            buffer: vk::Buffer::null(),
            offset: 0,
            end: 0,
            range: vk::WHOLE_SIZE,
            uploaded: false,
            object: std::ptr::null_mut(),
        }
    }
}

/// One descriptor write, as the encoder last recorded it for a set.
#[derive(Clone, Copy)]
struct DescriptorEntry {
    binding: u32,
    kind: vk::DescriptorType,
    /// The buffer, image view or sampler, as its raw handle.
    handle: u64,
    offset: u64,
    range: u64,
    /// For a dynamic buffer, the offset handed over when the set is bound.
    /// Not part of the descriptor, so not part of the comparison.
    dynamic_offset: u32,
}

impl PartialEq for DescriptorEntry {
    fn eq(&self, other: &Self) -> bool {
        (
            self.binding,
            self.kind,
            self.handle,
            self.offset,
            self.range,
        ) == (
            other.binding,
            other.kind,
            other.handle,
            other.offset,
            other.range,
        )
    }
}

impl Eq for DescriptorEntry {}

/// The descriptor sets a command buffer has written, in two flat lists so
/// that remembering one allocates nothing once they have grown.
#[derive(Default)]
struct WrittenSets {
    /// The newest set with each contents hash; older ones are chained.
    newest: crate::hash::FastMap<u64, u32>,
    sets: Vec<WrittenSet>,
    entries: Vec<DescriptorEntry>,
}

struct WrittenSet {
    layout: vk::DescriptorSetLayout,
    set: vk::DescriptorSet,
    /// Its entries, as a range of [`WrittenSets::entries`].
    start: u32,
    len: u32,
    /// The set written before it with the same hash, or [`NO_SET`].
    older: u32,
}

const NO_SET: u32 = u32::MAX;

impl WrittenSets {
    fn find(
        &self,
        layout: vk::DescriptorSetLayout,
        entries: &[DescriptorEntry],
    ) -> Option<vk::DescriptorSet> {
        let mut at = *self.newest.get(&contents_key(layout, entries))?;
        while at != NO_SET {
            let written = &self.sets[at as usize];
            let range = written.start as usize..(written.start + written.len) as usize;
            if written.layout == layout && self.entries[range] == *entries {
                return Some(written.set);
            }
            at = written.older;
        }
        None
    }

    fn remember(
        &mut self,
        layout: vk::DescriptorSetLayout,
        entries: &[DescriptorEntry],
        set: vk::DescriptorSet,
    ) {
        let at = self.sets.len() as u32;
        let older = self
            .newest
            .insert(contents_key(layout, entries), at)
            .unwrap_or(NO_SET);
        self.sets.push(WrittenSet {
            layout,
            set,
            start: self.entries.len() as u32,
            len: entries.len() as u32,
            older,
        });
        self.entries.extend_from_slice(entries);
    }

    fn clear(&mut self) {
        self.newest.clear();
        self.sets.clear();
        self.entries.clear();
    }
}

/// A hash of what [`DescriptorEntry`]'s equality compares, under `layout`.
fn contents_key(layout: vk::DescriptorSetLayout, entries: &[DescriptorEntry]) -> u64 {
    use std::hash::Hasher;
    use vk::Handle;
    let mut hasher = crate::hash::FastHasher::default();
    hasher.write_u64(layout.as_raw());
    for entry in entries {
        hasher.write_u32(entry.binding);
        hasher.write_i32(entry.kind.as_raw());
        hasher.write_u64(entry.handle);
        hasher.write_u64(entry.offset);
        hasher.write_u64(entry.range);
    }
    hasher.finish()
}

/// The descriptor set last written for one set index, reused while the
/// resources a draw needs are the ones it already holds.
#[derive(Default)]
struct SetCache {
    layout: vk::DescriptorSetLayout,
    set: vk::DescriptorSet,
    entries: Vec<DescriptorEntry>,
}

/// The least an upload is aligned to, whatever the device asks, so that a
/// 64-bit query result or a `float4` lands at its natural alignment.
const UPLOAD_ALIGNMENT: u64 = 16;

/// What is bound at one bind point, so that a draw which changes nothing
/// records nothing.
#[derive(Default)]
struct BindPoint {
    pipeline: vk::Pipeline,
    layouts: [vk::DescriptorSetLayout; SETS],
    sets: [vk::DescriptorSet; SETS],
    /// The buffer set's dynamic offsets, bound with it.
    offsets: Vec<u32>,
}

pub(crate) struct CommandQueue {
    pub gpu: &'static Gpu,
    pub device: Id,
}

pub(crate) struct CommandBuffer {
    pub gpu: &'static Gpu,
    /// Two timestamps around the whole recording. They back `gpuStartTime`
    /// and `gpuEndTime`, and also the optional GPU-time statistic.
    timestamps: vk::QueryPool,
    pub queue: Id,
    pub pool: vk::CommandPool,
    pub command_buffer: vk::CommandBuffer,
    pub fence: vk::Fence,
    /// Recording already ended at an encoded event, each its own submission
    /// batch; the first holds the context's command buffer.
    batches: Vec<gpu::Batch>,
    /// Event values the batch now recording waits for.
    waits: Vec<(vk::Semaphore, u64)>,
    descriptor_pools: Vec<vk::DescriptorPool>,
    /// The sets written from those pools, by layout and contents. A set is
    /// never written again once a draw has used it, so a draw which needs the
    /// same descriptors binds it rather than allocating and writing another.
    written_sets: WrittenSets,
    /// Objects the recording refers to, retained until the work completes.
    /// Each is retained once however often it is bound: a retain is a global
    /// lock in the Objective-C runtime, and a frame binds the same objects
    /// thousands of times.
    retained: Vec<OwnedObjcRef>,
    retained_set: AddressSet,
    /// Chunks `-setBytes:` copies are cut from, and how far into the last
    /// one the copies have reached.
    uploads: Vec<GpuBuffer>,
    upload_used: u64,
    /// Blit sources read from host pages at commit; see [`Self::copy_source`].
    snapshots: Vec<Snapshot>,
    /// Upload memory for snapshots too large for an upload chunk.
    staging: Vec<GpuBuffer>,
    framebuffers: Vec<vk::Framebuffer>,
    /// Sub-encoders' secondary command buffers this one executes.
    secondaries: Vec<gpu::SecondaryContext>,
    /// Whether this is a parallel render sub-encoder's own command buffer,
    /// which records into a secondary command buffer and is never committed:
    /// its parallel encoder adopts everything it holds.
    secondary: bool,
    /// Whether that secondary command buffer has been ended.
    secondary_closed: bool,
    /// `ROSEMARY_PASS_OUT`: attachment readbacks written once the work is done.
    captures: Vec<crate::capture::Capture>,
    scheduled_handlers: Vec<OwnedBlock>,
    completed_handlers: Vec<OwnedBlock>,
    /// Drawables `-presentDrawable:` asked for, presented once the work is done.
    presents: Vec<OwnedObjcRef>,
    completion: Arc<CommandCompletion>,
    recording: bool,
    /// `ROSEMARY_METAL_PROFILE`: shared with its parallel sub-encoders.
    profile: Option<Arc<crate::profile::Profile>>,
    occlusion: Occlusion,
    /// What render passes since the last full barrier touched, which later
    /// work waits for only where it conflicts.
    hazards: Hazards,
}

pub(crate) struct Encoder {
    pub command_buffer: Id,
    pub kind: Kind,
    /// `MTLDispatchTypeConcurrent`: dispatches may overlap, and the guest
    /// orders them with memory barriers. Every other compute encoder is serial.
    concurrent: bool,
    /// Whether a full memory barrier follows everything this encoder recorded.
    barrier_is_last: std::cell::Cell<bool>,
    compute_pipeline: Id,
    render_pipeline: Id,
    depth_state: Id,
    buffers: [[BufferBinding; SLOTS]; STAGES],
    textures: [[Id; SLOTS]; STAGES],
    samplers: [[Id; SLOTS]; STAGES],
    stencil_reference: u32,
    cull_mode: u64,
    winding: u64,
    fill_mode: u64,
    depth_clip: u64,
    depth_bias: RecordedDepthBias,
    in_render_pass: bool,
    /// A render pass whose Vulkan pass begins at its first draw and is
    /// ordered against earlier passes by what it touches; `None` for every
    /// other encoder, which is ordered by full barriers.
    lazy: Option<LazyPass>,
    /// Whether this is a parallel render sub-encoder, which records into a
    /// secondary command buffer inside a pass its parallel encoder began.
    sub_encoder: bool,
    /// The pass a parallel encoder began and its area, which its
    /// sub-encoders' command buffers inherit.
    inheritance: Option<(Inheritance, vk::Rect2D)>,
    /// Which colour attachments of the pass being recorded are resolved, for
    /// the pipeline variant a draw needs. See [`RenderVariantKey`].
    resolve_mask: u32,
    /// The depth/stencil resolve of the pass being recorded, for the same
    /// reason: a Vulkan render pass carries it and a Metal pipeline does not.
    depth_resolve: Option<DepthResolve>,
    /// Resolves a subpass cannot perform because the attachment has one sample,
    /// recorded as image copies once the pass has ended.
    pending_copies: Vec<PendingCopy>,
    /// `ROSEMARY_PASS_OUT`: the attachments to read back when this pass ends,
    /// and the ordinal the capture of this pass is named by.
    capture_targets: Vec<crate::capture::Target>,
    capture_pass: Option<u64>,
    /// Where the pass being recorded writes its visibility results.
    visibility: Option<VisibilityTarget>,
    /// The pipeline variant the last draw or dispatch used, with the state
    /// which chose it, so a run of draws with one state asks no cache.
    render_variant: Option<(Id, RenderVariantKey, vk::Pipeline, RenderDynamicState)>,
    /// The per-draw state this encoder last recorded, where the device sets
    /// it per draw; `None` until the first draw, as nothing is inherited.
    render_dynamic: Option<RenderDynamicState>,
    compute_variant: Option<(Id, [u32; 3], vk::Pipeline)>,
    /// A render encoder's attachment images, all of them written.
    attachments: Vec<(vk::Image, vk::ImageAspectFlags)>,
    /// Whether this encoder may write anything but its attachments: a
    /// resource a shader writes, a resolve copy, query results, an attachment
    /// whose memory another resource can share. It then waits for every
    /// earlier pass and ends with a barrier over all memory.
    writes_elsewhere: bool,
    /// Index by `bind_point_slot`.
    bound: [BindPoint; 2],
    /// Per set index, the last set bound or pushed there; the push path
    /// reads it to know whether the bind point still holds its push.
    set_cache: [SetCache; SETS],
    /// Scratch for the entries a draw needs, kept to avoid an allocation.
    wanted: [Vec<DescriptorEntry>; SETS],
    /// The buffer set's dynamic offsets for the draw, in binding order.
    dynamic_offsets: Vec<u32>,
    vertex_buffers: [(vk::Buffer, u64); SLOTS],
    index_buffer: (vk::Buffer, u64, vk::IndexType),
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Kind {
    Compute,
    Blit,
    Render,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct MtlSize {
    pub width: u64,
    pub height: u64,
    pub depth: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct NsRange {
    pub location: u64,
    pub length: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct MtlOrigin {
    pub x: u64,
    pub y: u64,
    pub z: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Viewport {
    pub origin_x: f64,
    pub origin_y: f64,
    pub width: f64,
    pub height: f64,
    pub znear: f64,
    pub zfar: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ScissorRect {
    pub x: u64,
    pub y: u64,
    pub width: u64,
    pub height: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ClearColor {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

impl CommandBuffer {
    pub(super) fn retain(&mut self, object: Id) {
        if !object.is_null() && self.retained_set.insert(object as usize) {
            self.retained.push(OwnedObjcRef::retain(object));
        }
    }

    /// A set this command buffer already wrote with exactly `entries`.
    fn written_set(
        &self,
        layout: vk::DescriptorSetLayout,
        entries: &[DescriptorEntry],
    ) -> Option<vk::DescriptorSet> {
        self.written_sets.find(layout, entries)
    }

    fn remember_set(
        &mut self,
        layout: vk::DescriptorSetLayout,
        entries: &[DescriptorEntry],
        set: vk::DescriptorSet,
    ) {
        self.written_sets.remember(layout, entries, set);
    }

    /// A descriptor set for `layout`, from a pool this command buffer owns.
    pub(super) fn allocate_set(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<vk::DescriptorSet> {
        for attempt in 0..2 {
            if let Some(pool) = self.descriptor_pools.last().copied() {
                let layouts = [layout];
                let info = vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(pool)
                    .set_layouts(&layouts);
                // SAFETY: the pool and layout belong to this live device.
                match unsafe { self.gpu.device.allocate_descriptor_sets(&info) } {
                    Ok(sets) => {
                        stats::count(Counter::DescriptorSets);
                        return sets.first().copied();
                    }
                    // A full pool is expected: take another and retry.
                    Err(_) if attempt == 0 => {}
                    Err(error) => {
                        gpu::trace(format!("vkAllocateDescriptorSets failed with {error:?}"));
                        return None;
                    }
                }
            }
            match self.gpu.acquire_descriptor_pool() {
                Ok(pool) => self.descriptor_pools.push(pool),
                Err(reason) => {
                    gpu::trace(reason);
                    return None;
                }
            }
        }
        None
    }

    /// Copy `bytes` into this command buffer's upload chunks, answering the
    /// buffer and offset they landed at. The copy lives until the work does.
    pub(super) fn upload(&mut self, bytes: &[u8]) -> Option<(vk::Buffer, u64)> {
        let (buffer, offset, contents) = self.reserve(bytes.len() as u64)?;
        // SAFETY: `reserve` handed out these `bytes.len()` mapped bytes once.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), contents, bytes.len()) };
        Some((buffer, offset))
    }

    /// Set aside `length` bytes of the upload chunks for the device to write,
    /// answering where they are and the mapping of their first byte.
    pub(super) fn reserve(&mut self, length: u64) -> Option<(vk::Buffer, u64, *mut u8)> {
        // Every upload is read through a storage descriptor, a vertex or
        // index binding, or a copy; none is a uniform buffer.
        let alignment = self
            .gpu
            .limits
            .min_storage_buffer_offset_alignment
            .max(UPLOAD_ALIGNMENT);
        let offset = self.upload_used.next_multiple_of(alignment);
        let offset = match self.uploads.last() {
            Some(_) if offset + length <= gpu::UPLOAD_CHUNK => offset,
            _ => {
                match self.gpu.acquire_upload_chunk() {
                    Ok(chunk) => self.uploads.push(chunk),
                    Err(reason) => {
                        gpu::trace(format!("setBytes: upload chunk failed: {reason}"));
                        return None;
                    }
                }
                0
            }
        };
        let chunk = self.uploads.last().expect("a chunk was just ensured");
        self.upload_used = offset + length;
        // SAFETY: the chunk is mapped over `UPLOAD_CHUNK` bytes, and this
        // offset is inside it.
        Some((chunk.buffer, offset, unsafe {
            chunk.contents.add(offset as usize)
        }))
    }

    /// The buffer and offset a blit reads `offset..offset + length` of
    /// `source` through. The caller has checked the range is inside.
    ///
    /// Host pages the device has never been given cannot have been written by
    /// it, so their bytes at commit are what the copy would read, and Metal
    /// requires the CPU's writes be done by then. Those are copied into upload
    /// memory at commit instead of being imported. Behind an event wait the
    /// CPU may still write after commit, so the pages are imported as before.
    pub(super) fn copy_source(
        &mut self,
        source: Id,
        offset: u64,
        length: u64,
    ) -> (vk::Buffer, u64) {
        // SAFETY: only buffers of this crate's reach a blit encoder.
        let body = unsafe { object::body::<Buffer>(source) };
        let snapshot = body.host_pages().is_some_and(|pages| !pages.imported())
            && !self.waits_for_event()
            && self.gpu.event_waits_in_flight.load(Ordering::Acquire) == 0;
        if snapshot && let Some((buffer, at, target)) = self.reserve_snapshot(length) {
            self.snapshots.push(Snapshot {
                source,
                offset,
                length,
                target,
                buffer,
                at,
            });
            stats::count(Counter::CopySnapshots);
            return (buffer, at);
        }
        body.device_range(offset, length)
    }

    fn reserve_snapshot(&mut self, length: u64) -> Option<(vk::Buffer, u64, *mut u8)> {
        if length <= gpu::UPLOAD_CHUNK {
            return self.reserve(length);
        }
        match self.gpu.create_staging_buffer(length) {
            Ok(staging) => {
                let found = (staging.buffer, 0, staging.contents);
                self.staging.push(staging);
                Some(found)
            }
            Err(reason) => {
                gpu::trace(format!("blit snapshot staging failed: {reason}"));
                None
            }
        }
    }

    /// Whether any batch recorded so far, or the one recording, waits for an
    /// event.
    fn waits_for_event(&self) -> bool {
        !self.waits.is_empty() || self.batches.iter().any(|batch| !batch.waits.is_empty())
    }
}

/// Bytes a blit reads from host pages, copied into upload memory at commit.
struct Snapshot {
    /// The `MTLBuffer`, retained by the command buffer with the copy.
    source: Id,
    offset: u64,
    length: u64,
    target: *mut u8,
    buffer: vk::Buffer,
    at: u64,
}

impl Encoder {}

// A pipeline or depth/stencil state is read at every draw after it is set, so
// the command buffer holds it from the moment it is bound, exactly as it holds
// a bound buffer: a guest may release its own reference straight away.

// The binding setters are registered once per stage, with the stage as a
// const parameter: the generic, vertex and fragment spellings share a body.

impl Encoder {}

impl Default for AttachmentDescriptor {
    fn default() -> Self {
        Self {
            texture: std::ptr::null_mut(),
            level: 0,
            slice: 0,
            load_action: 0,
            store_action: 0,
            clear_color: [0.0; 4],
            clear_depth: 1.0,
            clear_stencil: 0,
            resolve_texture: std::ptr::null_mut(),
            resolve_level: 0,
            resolve_slice: 0,
            resolve_filter: 0,
        }
    }
}

impl Default for RenderPassDescriptor {
    fn default() -> Self {
        Self {
            colors: [AttachmentDescriptor::default(); RENDER_TARGETS],
            depth: AttachmentDescriptor::default(),
            stencil: AttachmentDescriptor::default(),
            color_objects: std::ptr::null_mut(),
            depth_object: std::ptr::null_mut(),
            stencil_object: std::ptr::null_mut(),
            visibility_result_buffer: std::ptr::null_mut(),
            visibility_result_type: 0,
            render_target_array_length: 0,
        }
    }
}

fn install_attachment_methods(attachment: &'static objc::class::Class) {
    methods!(attachment, {
        "setClearColor:" => set_attachment_clear_color, c"v48@0:8{MTLClearColor=dddd}16";
        "setClearDepth:" => set_attachment_clear_depth, c"v24@0:8d16";
        "clearColor" => attachment_clear_color, c"{MTLClearColor=dddd}16@0:8";
        "clearDepth" => attachment_clear_depth, c"d16@0:8";
        "dealloc" => attachment_dealloc, c"v16@0:8";
    });
    for selector in ["setTexture:", "setResolveTexture:"] {
        attachment.replace_method(
            objc::sel::get(selector),
            set_attachment_object as *const std::ffi::c_void,
            c"v24@0:8@16",
        );
    }
    for selector in ["texture", "resolveTexture"] {
        attachment.replace_method(
            objc::sel::get(selector),
            attachment_object as *const std::ffi::c_void,
            c"@16@0:8",
        );
    }
    for selector in [
        "setLevel:",
        "setSlice:",
        "setDepthPlane:",
        "setLoadAction:",
        "setStoreAction:",
        "setClearStencil:",
        "setResolveLevel:",
        "setResolveSlice:",
        "setDepthResolveFilter:",
        "setStencilResolveFilter:",
    ] {
        attachment.replace_method(
            objc::sel::get(selector),
            set_attachment_number as *const std::ffi::c_void,
            c"v24@0:8Q16",
        );
    }
    for selector in [
        "level",
        "slice",
        "depthPlane",
        "loadAction",
        "storeAction",
        "clearStencil",
        "resolveLevel",
        "resolveSlice",
        "depthResolveFilter",
        "stencilResolveFilter",
    ] {
        attachment.replace_method(
            objc::sel::get(selector),
            attachment_number as *const std::ffi::c_void,
            c"Q16@0:8",
        );
    }
}

pub(crate) fn install() {
    parallel::install();
    object::register::<CommandQueue>(c"RosemaryMTLCommandQueue");
    object::register::<CommandBuffer>(c"RosemaryMTLCommandBuffer");
    object::register::<Encoder>(c"RosemaryMTLCommandEncoder");
    let descriptor =
        object::register::<queue::CommandBufferDescriptor>(c"MTLCommandBufferDescriptor");
    methods!(descriptor, {
        "init" => queue::command_buffer_descriptor_init, c"@16@0:8";
        "retainedReferences" => queue::retained_references, c"B16@0:8";
        "setRetainedReferences:" => queue::set_retained_references, c"v20@0:8B16";
        "errorOptions" => queue::error_options, c"Q16@0:8";
        "setErrorOptions:" => queue::set_error_options, c"v24@0:8Q16";
    });

    let queue = objc::class::lookup(c"RosemaryMTLCommandQueue").expect("just registered");
    methods!(queue, {
        "device" => queue_device, c"@16@0:8";
        "commandBuffer" => new_command_buffer, c"@16@0:8";
        "dealloc" => queue_dealloc, c"v16@0:8";
        "commandBufferWithUnretainedReferences" => new_command_buffer, c"@16@0:8";
        "commandBufferWithDescriptor:" => new_command_buffer_with_descriptor, c"@24@0:8@16";
        "setLabel:" => crate::no_op, c"v24@0:8@16";
    });

    let command = objc::class::lookup(c"RosemaryMTLCommandBuffer").expect("just registered");
    methods!(command, {
        "commandQueue" => command_buffer_queue, c"@16@0:8";
        "status" => command_buffer_status, c"Q16@0:8";
        "GPUStartTime" => command_buffer_gpu_start_time, c"d16@0:8";
        "GPUEndTime" => command_buffer_gpu_end_time, c"d16@0:8";
        "addScheduledHandler:" => add_handler, c"v24@0:8@?16";
        "addCompletedHandler:" => add_handler, c"v24@0:8@?16";
        "commit" => commit, c"v16@0:8";
        "encodeSignalEvent:value:" => encode_signal_event, c"v32@0:8@16Q24";
        "encodeWaitForEvent:value:" => encode_wait_for_event, c"v32@0:8@16Q24";
        "dealloc" => command_buffer_dealloc, c"v16@0:8";
        "waitUntilCompleted" => wait_until_completed, c"v16@0:8";
        "waitUntilScheduled" => wait_until_scheduled, c"v16@0:8";
        "enqueue" => crate::no_op, c"v16@0:8";
        "presentDrawable:" => present_drawable, c"v24@0:8@16";
        "presentDrawable:atTime:" => present_drawable_at_time, c"v32@0:8@16d24";
        "setLabel:" => crate::no_op, c"v24@0:8@16";
        "pushDebugGroup:" => crate::no_op, c"v24@0:8@16";
        "popDebugGroup" => crate::no_op, c"v16@0:8";
    });

    let encoder = objc::class::lookup(c"RosemaryMTLCommandEncoder").expect("just registered");
    methods!(encoder, {
        "endEncoding" => end_encoding, c"v16@0:8";
        "dealloc" => encoder_dealloc, c"v16@0:8";
        "setComputePipelineState:" => set_compute_pipeline, c"v24@0:8@16";
        "setRenderPipelineState:" => set_render_pipeline, c"v24@0:8@16";
        "setDepthStencilState:" => set_depth_stencil_state, c"v24@0:8@16";
        "setStencilReferenceValue:" => set_stencil_reference, c"v20@0:8I16";
        "setViewport:" => set_viewport, c"v64@0:8{MTLViewport=dddddd}16";
        "setScissorRect:" => set_scissor, c"v48@0:8{MTLScissorRect=QQQQ}16";
        "setDepthBias:slopeScale:clamp:" => set_depth_bias, c"v28@0:8f16f20f24";
        "setBlendColorRed:green:blue:alpha:" => set_blend_color, c"v32@0:8f16f20f24f28";
        "dispatchThreadgroups:threadsPerThreadgroup:" => dispatch_threadgroups,
            c"v64@0:8{MTLSize=QQQ}16{MTLSize=QQQ}40";
        "dispatchThreads:threadsPerThreadgroup:" => dispatch_threadgroups,
            c"v64@0:8{MTLSize=QQQ}16{MTLSize=QQQ}40";
        "drawPrimitives:vertexStart:vertexCount:instanceCount:" => draw_primitives,
            c"v48@0:8Q16Q24Q32Q40";
        "drawPrimitives:vertexStart:vertexCount:instanceCount:baseInstance:"
            => draw_primitives_base, c"v56@0:8Q16Q24Q32Q40Q48";
        "drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:"
            => draw_indexed_primitives, c"v64@0:8Q16Q24Q32@40Q48Q56";
        "drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:baseVertex:baseInstance:"
            => draw_indexed_primitives_base, c"v80@0:8Q16Q24Q32@40Q48Q56q64Q72";
        "copyFromBuffer:sourceOffset:toBuffer:destinationOffset:size:"
            => copy_buffer_to_buffer, c"v56@0:8@16Q24@32Q40Q48";
        "copyFromBuffer:sourceOffset:sourceBytesPerRow:sourceBytesPerImage:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:"
            => copy_buffer_to_texture, c"v120@0:8@16Q24Q32Q40{MTLSize=QQQ}48@72Q80Q88{MTLOrigin=QQQ}96";
        "copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:"
            => copy_texture_to_buffer, c"v120@0:8@16Q24Q32{MTLOrigin=QQQ}40{MTLSize=QQQ}64@88Q96Q104Q112";
        "copyFromBuffer:sourceOffset:sourceBytesPerRow:sourceBytesPerImage:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:options:"
            => copy_buffer_to_texture_options, c"v128@0:8@16Q24Q32Q40{MTLSize=QQQ}48@72Q80Q88{MTLOrigin=QQQ}96Q120";
        "copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:options:"
            => copy_texture_to_buffer_options, c"v128@0:8@16Q24Q32{MTLOrigin=QQQ}40{MTLSize=QQQ}64@88Q96Q104Q112Q120";
        "drawPrimitives:vertexStart:vertexCount:" => draw_primitives_once, c"v40@0:8Q16Q24Q32";
        "drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:"
            => draw_indexed_primitives_once, c"v56@0:8Q16Q24Q32@40Q48";
        "drawPrimitives:indirectBuffer:indirectBufferOffset:" => draw_primitives_indirect,
            c"v40@0:8Q16@24Q32";
        "drawIndexedPrimitives:indexType:indexBuffer:indexBufferOffset:indirectBuffer:indirectBufferOffset:"
            => draw_indexed_primitives_indirect, c"v64@0:8Q16Q24@32Q40@48Q56";
        // Every attachment is stored whatever the pass named (see
        // `pass::store_op`), so a store action decided late changes nothing.
        "setColorStoreAction:atIndex:" => crate::no_op, c"v32@0:8Q16Q24";
        "setDepthStoreAction:" => crate::no_op, c"v24@0:8Q16";
        "setStencilStoreAction:" => crate::no_op, c"v24@0:8Q16";
        // A pass which updates a fence is ordered against later passes by
        // what it writes; one which waits for a fence waits for them all.
        "updateFence:afterStages:" => crate::no_op, c"v32@0:8@16Q24";
        "waitForFence:beforeStages:" => order_after_earlier_passes, c"v32@0:8@16Q24";
        "memoryBarrierWithScope:" => memory_barrier, c"v24@0:8Q16";
        "memoryBarrierWithScope:afterStages:beforeStages:" => memory_barrier, c"v40@0:8Q16Q24Q32";
        "memoryBarrierWithResources:count:" => memory_barrier, c"v32@0:8^@16Q24";
        "memoryBarrierWithResources:count:afterStages:beforeStages:" => memory_barrier,
            c"v48@0:8^@16Q24Q32Q40";
        "useResource:usage:stages:" => order_after_earlier_passes, c"v40@0:8@16Q24Q32";
        "useHeap:" => order_after_earlier_passes, c"v24@0:8@16";
        "setStencilFrontReferenceValue:backReferenceValue:" => set_stencil_references,
            c"v24@0:8I16I20";
        "copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:"
            => copy_texture_to_texture, c"v144@0:8@16Q24Q32{MTLOrigin=QQQ}40{MTLSize=QQQ}64@88Q96Q104{MTLOrigin=QQQ}112";
        "generateMipmapsForTexture:" => generate_mipmaps, c"v24@0:8@16";
        "synchronizeResource:" => crate::no_op, c"v24@0:8@16";
        "synchronizeTexture:slice:level:" => crate::no_op, c"v40@0:8@16Q24Q32";
        // Layout hints for a GPU's compressed texture storage; Vulkan picks
        // its own layouts, so there is nothing to change.
        "optimizeContentsForGPUAccess:" => crate::no_op, c"v24@0:8@16";
        "optimizeContentsForGPUAccess:slice:level:" => crate::no_op, c"v40@0:8@16Q24Q32";
        "optimizeContentsForCPUAccess:" => crate::no_op, c"v24@0:8@16";
        "optimizeContentsForCPUAccess:slice:level:" => crate::no_op, c"v40@0:8@16Q24Q32";
        "updateFence:" => crate::no_op, c"v24@0:8@16";
        "waitForFence:" => order_after_earlier_passes, c"v24@0:8@16";
        "pushDebugGroup:" => crate::no_op, c"v24@0:8@16";
        "popDebugGroup" => crate::no_op, c"v16@0:8";
        "insertDebugSignpost:" => crate::no_op, c"v24@0:8@16";
        "setLabel:" => crate::no_op, c"v24@0:8@16";
        "useResource:usage:" => order_after_earlier_passes, c"v32@0:8@16Q24";
        "setVisibilityResultMode:offset:" => set_visibility_result_mode, c"v32@0:8Q16Q24";
    });

    let pass = object::register::<RenderPassDescriptor>(c"MTLRenderPassDescriptor");
    let attachment =
        object::register::<AttachmentElement>(c"RosemaryMTLRenderPassAttachmentDescriptor");
    let metaclass = pass.metaclass();
    metaclass.replace_method(
        objc::sel::get("renderPassDescriptor"),
        new_render_pass_descriptor as *const std::ffi::c_void,
        c"@16@0:8",
    );
    methods!(pass, {
        "init" => render_pass_descriptor_init, c"@16@0:8";
        "dealloc" => render_pass_descriptor_dealloc, c"v16@0:8";
        "colorAttachments" => pass_color_attachments, c"@16@0:8";
        "depthAttachment" => pass_single_attachment, c"@16@0:8";
        "stencilAttachment" => pass_single_attachment, c"@16@0:8";
        "visibilityResultBuffer" => pass_visibility_result_buffer, c"@16@0:8";
        "setVisibilityResultBuffer:" => set_pass_visibility_result_buffer, c"v24@0:8@16";
        "visibilityResultType" => pass_visibility_result_type, c"Q16@0:8";
        "setVisibilityResultType:" => set_pass_visibility_result_type, c"v24@0:8Q16";
        "setRenderTargetWidth:" => crate::no_op, c"v24@0:8Q16";
        "setRenderTargetHeight:" => crate::no_op, c"v24@0:8Q16";
        "setDefaultRasterSampleCount:" => crate::no_op, c"v24@0:8Q16";
        "setRenderTargetArrayLength:" => set_render_target_array_length, c"v24@0:8Q16";
        "renderTargetArrayLength" => render_target_array_length, c"Q16@0:8";
    });
    methods!(pass, {
        "setDepthAttachment:" => set_pass_single_attachment, c"v24@0:8@16";
        "setStencilAttachment:" => set_pass_single_attachment, c"v24@0:8@16";
    });
    let _ = attachment;
    for name in ATTACHMENT_CLASSES {
        let class = object::register::<AttachmentElement>(name);
        install_attachment_methods(class);
        // The vended elements are made with their owner; only a standalone
        // descriptor a game makes with `+new` is initialized here.
        if name != c"RosemaryMTLRenderPassAttachmentDescriptor" {
            methods!(class, { "init" => attachment_init, c"@16@0:8"; });
        }
    }

    methods!(command, {
        "computeCommandEncoder" => new_compute_encoder, c"@16@0:8";
        "computeCommandEncoderWithDispatchType:" => new_compute_encoder_with_dispatch_type,
            c"@24@0:8Q16";
        "blitCommandEncoder" => new_blit_encoder, c"@16@0:8";
        "renderCommandEncoderWithDescriptor:" => new_render_encoder, c"@24@0:8@16";
        "parallelRenderCommandEncoderWithDescriptor:" => new_parallel_encoder,
            c"@24@0:8@16";
    });
    let per_stage: [(&str, *const std::ffi::c_void, &std::ffi::CStr); 15] = [
        (
            "setBuffer:offset:atIndex:",
            set_buffer::<COMPUTE> as *const std::ffi::c_void,
            c"v40@0:8@16Q24Q32",
        ),
        (
            "setVertexBuffer:offset:atIndex:",
            set_buffer::<VERTEX> as *const std::ffi::c_void,
            c"v40@0:8@16Q24Q32",
        ),
        (
            "setFragmentBuffer:offset:atIndex:",
            set_buffer::<FRAGMENT> as *const std::ffi::c_void,
            c"v40@0:8@16Q24Q32",
        ),
        (
            "setBytes:length:atIndex:",
            set_bytes::<COMPUTE> as *const std::ffi::c_void,
            c"v40@0:8r^v16Q24Q32",
        ),
        (
            "setVertexBytes:length:atIndex:",
            set_bytes::<VERTEX> as *const std::ffi::c_void,
            c"v40@0:8r^v16Q24Q32",
        ),
        (
            "setFragmentBytes:length:atIndex:",
            set_bytes::<FRAGMENT> as *const std::ffi::c_void,
            c"v40@0:8r^v16Q24Q32",
        ),
        (
            "setBufferOffset:atIndex:",
            set_buffer_offset::<COMPUTE> as *const std::ffi::c_void,
            c"v32@0:8Q16Q24",
        ),
        (
            "setVertexBufferOffset:atIndex:",
            set_buffer_offset::<VERTEX> as *const std::ffi::c_void,
            c"v32@0:8Q16Q24",
        ),
        (
            "setFragmentBufferOffset:atIndex:",
            set_buffer_offset::<FRAGMENT> as *const std::ffi::c_void,
            c"v32@0:8Q16Q24",
        ),
        (
            "setTexture:atIndex:",
            set_texture::<COMPUTE> as *const std::ffi::c_void,
            c"v32@0:8@16Q24",
        ),
        (
            "setVertexTexture:atIndex:",
            set_texture::<VERTEX> as *const std::ffi::c_void,
            c"v32@0:8@16Q24",
        ),
        (
            "setFragmentTexture:atIndex:",
            set_texture::<FRAGMENT> as *const std::ffi::c_void,
            c"v32@0:8@16Q24",
        ),
        (
            "setSamplerState:atIndex:",
            set_sampler::<COMPUTE> as *const std::ffi::c_void,
            c"v32@0:8@16Q24",
        ),
        (
            "setVertexSamplerState:atIndex:",
            set_sampler::<VERTEX> as *const std::ffi::c_void,
            c"v32@0:8@16Q24",
        ),
        (
            "setFragmentSamplerState:atIndex:",
            set_sampler::<FRAGMENT> as *const std::ffi::c_void,
            c"v32@0:8@16Q24",
        ),
    ];
    let ranged: [(&str, *const std::ffi::c_void, &std::ffi::CStr); 9] = [
        (
            "setBuffers:offsets:withRange:",
            set_buffers::<COMPUTE> as *const std::ffi::c_void,
            c"v48@0:8r^@16r^Q24{_NSRange=QQ}32",
        ),
        (
            "setVertexBuffers:offsets:withRange:",
            set_buffers::<VERTEX> as *const std::ffi::c_void,
            c"v48@0:8r^@16r^Q24{_NSRange=QQ}32",
        ),
        (
            "setFragmentBuffers:offsets:withRange:",
            set_buffers::<FRAGMENT> as *const std::ffi::c_void,
            c"v48@0:8r^@16r^Q24{_NSRange=QQ}32",
        ),
        (
            "setTextures:withRange:",
            set_textures::<COMPUTE> as *const std::ffi::c_void,
            c"v40@0:8r^@16{_NSRange=QQ}24",
        ),
        (
            "setVertexTextures:withRange:",
            set_textures::<VERTEX> as *const std::ffi::c_void,
            c"v40@0:8r^@16{_NSRange=QQ}24",
        ),
        (
            "setFragmentTextures:withRange:",
            set_textures::<FRAGMENT> as *const std::ffi::c_void,
            c"v40@0:8r^@16{_NSRange=QQ}24",
        ),
        (
            "setSamplerStates:withRange:",
            set_samplers::<COMPUTE> as *const std::ffi::c_void,
            c"v40@0:8r^@16{_NSRange=QQ}24",
        ),
        (
            "setVertexSamplerStates:withRange:",
            set_samplers::<VERTEX> as *const std::ffi::c_void,
            c"v40@0:8r^@16{_NSRange=QQ}24",
        ),
        (
            "setFragmentSamplerStates:withRange:",
            set_samplers::<FRAGMENT> as *const std::ffi::c_void,
            c"v40@0:8r^@16{_NSRange=QQ}24",
        ),
    ];
    for (selector, implementation, types) in per_stage.into_iter().chain(ranged) {
        encoder.replace_method(objc::sel::get(selector), implementation, types);
    }
    let raster: [(&str, unsafe extern "C" fn(Id, Sel, u64)); 4] = [
        ("setCullMode:", set_cull_mode),
        ("setFrontFacingWinding:", set_front_facing_winding),
        ("setTriangleFillMode:", set_triangle_fill_mode),
        ("setDepthClipMode:", set_depth_clip_mode),
    ];
    for (selector, implementation) in raster {
        encoder.replace_method(
            objc::sel::get(selector),
            implementation as *const std::ffi::c_void,
            c"v24@0:8Q16",
        );
    }
}

#[cfg(test)]
mod written_sets_tests {
    use super::*;
    use vk::Handle;

    fn entries(handle: u64) -> Vec<DescriptorEntry> {
        vec![DescriptorEntry {
            binding: 0,
            kind: vk::DescriptorType::STORAGE_BUFFER,
            handle,
            offset: 0,
            range: vk::WHOLE_SIZE,
            dynamic_offset: 0,
        }]
    }

    fn layout(raw: u64) -> vk::DescriptorSetLayout {
        vk::DescriptorSetLayout::from_raw(raw)
    }

    fn set(raw: u64) -> vk::DescriptorSet {
        vk::DescriptorSet::from_raw(raw)
    }

    #[test]
    fn finds_any_remembered_set_with_the_same_layout_and_entries() {
        let mut written = WrittenSets::default();
        written.remember(layout(1), &entries(100), set(10));
        written.remember(layout(1), &entries(200), set(20));
        assert_eq!(written.find(layout(1), &entries(100)), Some(set(10)));
        assert_eq!(written.find(layout(1), &entries(200)), Some(set(20)));
        assert_eq!(written.find(layout(2), &entries(100)), None);
        assert_eq!(written.find(layout(1), &entries(300)), None);
    }

    #[test]
    fn ignores_dynamic_offsets_when_matching() {
        let mut written = WrittenSets::default();
        written.remember(layout(1), &entries(100), set(10));
        let mut moved = entries(100);
        moved[0].dynamic_offset = 256;
        assert_eq!(written.find(layout(1), &moved), Some(set(10)));
    }

    #[test]
    fn remembers_every_set_a_frame_writes() {
        let mut written = WrittenSets::default();
        for index in 0..64 {
            written.remember(layout(1), &entries(index), set(index + 1));
        }
        assert_eq!(written.find(layout(1), &entries(0)), Some(set(1)));
        assert_eq!(written.find(layout(1), &entries(63)), Some(set(64)));
    }

    #[test]
    fn forgets_everything_when_cleared() {
        let mut written = WrittenSets::default();
        written.remember(layout(1), &entries(100), set(10));
        written.clear();
        assert_eq!(written.find(layout(1), &entries(100)), None);
    }
}
