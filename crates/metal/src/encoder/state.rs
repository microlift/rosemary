use super::*;

pub(super) unsafe extern "C" fn set_compute_pipeline(receiver: Id, _sel: Sel, pipeline: Id) {
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    encoder.command().retain(pipeline);
    encoder.compute_pipeline = pipeline;
}

pub(super) unsafe extern "C" fn set_render_pipeline(receiver: Id, _sel: Sel, pipeline: Id) {
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    encoder.command().retain(pipeline);
    encoder.render_pipeline = pipeline;
}

pub(super) unsafe extern "C" fn set_depth_stencil_state(receiver: Id, _sel: Sel, state: Id) {
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    encoder.command().retain(state);
    encoder.depth_state = state;
}

pub(super) const COMPUTE: usize = 0;

pub(super) const VERTEX: usize = 1;

pub(super) const FRAGMENT: usize = 2;

pub(super) unsafe extern "C" fn set_buffer<const STAGE: usize>(
    receiver: Id,
    _sel: Sel,
    buffer: Id,
    offset: u64,
    index: u64,
) {
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    let Some(slot) = encoder.buffers[STAGE].get_mut(index as usize) else {
        // Metal retains what an encoder binds, and a guest is entitled to
        // release its own reference the moment the call returns. The command
        // buffer's reference outlives the work the binding is recorded into.
        encoder.command().retain(buffer);
        return;
    };
    // A guest rebinds the same buffer at a new offset far more often than it
    // binds another; the command buffer retained it when it was first bound.
    if slot.object == buffer && !buffer.is_null() {
        slot.offset = offset;
        slot.uploaded = false;
        return;
    }
    *slot = if buffer.is_null() {
        BufferBinding::default()
    } else {
        // SAFETY: only a buffer of this crate's is ever bound.
        let body = unsafe { object::body::<Buffer>(buffer) };
        BufferBinding {
            buffer: body.device_buffer(),
            offset,
            end: body.inner.length,
            range: vk::WHOLE_SIZE,
            uploaded: false,
            object: buffer,
        }
    };
    encoder.command().retain(buffer);
}

/// `-setBytes:length:atIndex:` and its per-stage spellings.
///
/// Metal's small inline constants, which have no `MTLBuffer` of their own. The
/// bytes are copied into the command buffer's upload chunks, which outlive the
/// recording exactly as a bound buffer does. Metal's own limit is four
/// kilobytes; a larger one is refused rather than quietly turned into an
/// allocation the guest never asked for.
///
/// A game calls this for nearly every draw, so it must not make an object: a
/// whole `MTLBuffer` here was a heap lock, a `VkBuffer`, a memory bind and two
/// trips through the Objective-C runtime's global retain table per call.
pub(super) unsafe extern "C" fn set_bytes<const STAGE: usize>(
    receiver: Id,
    _sel: Sel,
    bytes: *const u8,
    length: u64,
    index: u64,
) {
    const LIMIT: u64 = 4096;
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    if bytes.is_null() || length == 0 || length > LIMIT {
        gpu::trace(format!("setBytes:length:atIndex: refused {length} byte(s)"));
        return;
    }
    stats::count(Counter::SetBytesCalls);
    stats::add(Counter::SetBytesBytes, length);
    // SAFETY: the guest promised `length` readable bytes.
    let source = unsafe { std::slice::from_raw_parts(bytes, length as usize) };
    let Some((buffer, offset)) = encoder.command().upload(source) else {
        return;
    };
    if let Some(slot) = encoder.buffers[STAGE].get_mut(index as usize) {
        *slot = BufferBinding {
            buffer,
            offset,
            end: offset + length,
            range: length,
            uploaded: true,
            object: std::ptr::null_mut(),
        };
    }
}

pub(super) unsafe extern "C" fn set_buffer_offset<const STAGE: usize>(
    receiver: Id,
    _sel: Sel,
    offset: u64,
    index: u64,
) {
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    if let Some(slot) = encoder.buffers[STAGE].get_mut(index as usize) {
        slot.offset = offset;
        // Metal allows this only on a buffer set with `-setBuffer:`. On a
        // `-setBytes:` copy it would move a dynamic offset past the bytes the
        // chunk was written with, so it stays an ordinary descriptor.
        slot.uploaded = false;
    }
}

pub(super) unsafe extern "C" fn set_texture<const STAGE: usize>(
    receiver: Id,
    _sel: Sel,
    texture: Id,
    index: u64,
) {
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    if let Some(slot) = encoder.textures[STAGE].get_mut(index as usize) {
        // Retained when it was bound here before.
        if *slot == texture {
            return;
        }
        *slot = texture;
    }
    encoder.command().retain(texture);
    encoder.note_read(texture);
}

/// `-waitForFence:`, `-useResource:` and `-useHeap:` and their forms with
/// stages: a render pass reads what an earlier pass wrote through a resource
/// it does not bind, so it waits for every pass before it. A compute or blit
/// encoder already does.
pub(super) unsafe extern "C" fn order_after_earlier_passes(receiver: Id, _sel: Sel) {
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    encoder.writes_elsewhere = true;
}

pub(super) unsafe extern "C" fn set_sampler<const STAGE: usize>(
    receiver: Id,
    _sel: Sel,
    sampler: Id,
    index: u64,
) {
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    if let Some(slot) = encoder.samplers[STAGE].get_mut(index as usize) {
        // Retained when it was bound here before.
        if *slot == sampler {
            return;
        }
        *slot = sampler;
    }
    encoder.command().retain(sampler);
}

pub(super) unsafe extern "C" fn set_stencil_reference(receiver: Id, _sel: Sel, value: u32) {
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    encoder.stencil_reference = value;
    let command = encoder.command();
    // SAFETY: recording into this command buffer.
    unsafe {
        command.gpu.device.cmd_set_stencil_reference(
            command.command_buffer,
            vk::StencilFaceFlags::FRONT_AND_BACK,
            value,
        )
    };
}

/// The raster states a draw's pipeline variant is chosen by. One function
/// each rather than one which reads the selector's name: a game sets them
/// for nearly every draw.
macro_rules! raster_setter {
    ($name:ident, $field:ident) => {
        pub(super) unsafe extern "C" fn $name(receiver: Id, _sel: Sel, value: u64) {
            // SAFETY: an encoder of this crate's.
            unsafe { object::body::<Encoder>(receiver) }.$field = value;
        }
    };
}

raster_setter!(set_cull_mode, cull_mode);
raster_setter!(set_front_facing_winding, winding);
raster_setter!(set_triangle_fill_mode, fill_mode);
raster_setter!(set_depth_clip_mode, depth_clip);

pub(super) unsafe extern "C" fn set_viewport(receiver: Id, _sel: Sel, viewport: Viewport) {
    // SAFETY: an encoder of this crate's.
    let command = unsafe { object::body::<Encoder>(receiver) }.command();
    // Metal's clip space has Y up and its framebuffer origin is top left;
    // Vulkan's clip space has Y down. A negative viewport height, core since
    // Vulkan 1.1, is the exact correction.
    let flipped = vk::Viewport {
        x: viewport.origin_x as f32,
        y: (viewport.origin_y + viewport.height) as f32,
        width: viewport.width as f32,
        height: -(viewport.height as f32),
        min_depth: viewport.znear as f32,
        max_depth: viewport.zfar as f32,
    };
    // SAFETY: recording into this command buffer.
    unsafe {
        command
            .gpu
            .device
            .cmd_set_viewport(command.command_buffer, 0, &[flipped])
    };
}

pub(super) unsafe extern "C" fn set_scissor(receiver: Id, _sel: Sel, rect: ScissorRect) {
    // SAFETY: an encoder of this crate's.
    let command = unsafe { object::body::<Encoder>(receiver) }.command();
    let scissor = vk::Rect2D {
        offset: vk::Offset2D {
            x: rect.x as i32,
            y: rect.y as i32,
        },
        extent: vk::Extent2D {
            width: rect.width as u32,
            height: rect.height as u32,
        },
    };
    // SAFETY: recording into this command buffer.
    unsafe {
        command
            .gpu
            .device
            .cmd_set_scissor(command.command_buffer, 0, &[scissor])
    };
}

pub(super) unsafe extern "C" fn set_depth_bias(
    receiver: Id,
    _sel: Sel,
    bias: f32,
    slope_scale: f32,
    clamp: f32,
) {
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    let command = encoder.command();
    encoder.depth_bias.record(
        &command.gpu.device,
        command.command_buffer,
        DepthBias {
            bias,
            slope_scale,
            clamp,
        },
    );
}

pub(super) unsafe extern "C" fn set_blend_color(
    receiver: Id,
    _sel: Sel,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
) {
    // SAFETY: an encoder of this crate's.
    let command = unsafe { object::body::<Encoder>(receiver) }.command();
    // SAFETY: recording into this command buffer.
    unsafe {
        command
            .gpu
            .device
            .cmd_set_blend_constants(command.command_buffer, &[red, green, blue, alpha])
    };
}

/// `-setBuffers:offsets:withRange:` and its per-stage spellings: one
/// `-setBuffer:offset:atIndex:` per element of the range.
pub(super) unsafe extern "C" fn set_buffers<const STAGE: usize>(
    receiver: Id,
    sel: Sel,
    buffers: *const Id,
    offsets: *const u64,
    range: NsRange,
) {
    if buffers.is_null() || offsets.is_null() {
        return;
    }
    for at in 0..range.length as usize {
        // SAFETY: the guest passed `range.length` buffers and offsets.
        let (buffer, offset) = unsafe { (*buffers.add(at), *offsets.add(at)) };
        unsafe { set_buffer::<STAGE>(receiver, sel, buffer, offset, range.location + at as u64) };
    }
}

/// `-setTextures:withRange:` and its per-stage spellings.
pub(super) unsafe extern "C" fn set_textures<const STAGE: usize>(
    receiver: Id,
    sel: Sel,
    textures: *const Id,
    range: NsRange,
) {
    if textures.is_null() {
        return;
    }
    for at in 0..range.length as usize {
        // SAFETY: the guest passed `range.length` textures.
        let texture = unsafe { *textures.add(at) };
        unsafe { set_texture::<STAGE>(receiver, sel, texture, range.location + at as u64) };
    }
}

/// `-setSamplerStates:withRange:` and its per-stage spellings.
pub(super) unsafe extern "C" fn set_samplers<const STAGE: usize>(
    receiver: Id,
    sel: Sel,
    samplers: *const Id,
    range: NsRange,
) {
    if samplers.is_null() {
        return;
    }
    for at in 0..range.length as usize {
        // SAFETY: the guest passed `range.length` samplers.
        let sampler = unsafe { *samplers.add(at) };
        unsafe { set_sampler::<STAGE>(receiver, sel, sampler, range.location + at as u64) };
    }
}

/// `-setStencilFrontReferenceValue:backReferenceValue:`.
pub(super) unsafe extern "C" fn set_stencil_references(
    receiver: Id,
    _sel: Sel,
    front: u32,
    back: u32,
) {
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    encoder.stencil_reference = front;
    let command = encoder.command();
    // SAFETY: recording into this command buffer.
    unsafe {
        let device = &command.gpu.device;
        device.cmd_set_stencil_reference(
            command.command_buffer,
            vk::StencilFaceFlags::FRONT,
            front,
        );
        device.cmd_set_stencil_reference(command.command_buffer, vk::StencilFaceFlags::BACK, back);
    };
}
