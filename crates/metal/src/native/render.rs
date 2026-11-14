use super::*;

struct NativeRenderPipeline {
    host: HostId,
    device: Id,
}

struct NativeRenderEncoder {
    host: HostId,
    command: Id,
    width: u64,
    height: u64,
}

pub(super) fn install() {
    let pipeline =
        object::register::<NativeRenderPipeline>(c"RosemaryNativeMTLRenderPipelineState");
    methods!(pipeline, {
        "device" => pipeline_device, c"@16@0:8";
        "dealloc" => pipeline_dealloc, c"v16@0:8";
    });
    let encoder = object::register::<NativeRenderEncoder>(c"RosemaryNativeMTLRenderCommandEncoder");
    methods!(encoder, {
        "device" => encoder_device, c"@16@0:8";
        "setRenderPipelineState:" => set_pipeline, c"v24@0:8@16";
        "setDepthStencilState:" => set_depth_stencil_state, c"v24@0:8@16";
        "setStencilReferenceValue:" => set_stencil_reference_value, c"v20@0:8I16";
        "setStencilFrontReferenceValue:backReferenceValue:" => set_stencil_reference_values, c"v24@0:8I16I20";
        "setViewport:" => set_viewport, c"v64@0:8{MTLViewport=dddddd}16";
        "setScissorRect:" => set_scissor_rect, c"v48@0:8{MTLScissorRect=QQQQ}16";
        "setDepthBias:slopeScale:clamp:" => set_depth_bias, c"v28@0:8f16f20f24";
        "setBlendColorRed:green:blue:alpha:" => set_blend_color, c"v32@0:8f16f20f24f28";
        "setDepthClipMode:" => set_depth_clip_mode, c"v24@0:8Q16";
        "setCullMode:" => set_cull_mode, c"v24@0:8Q16";
        "setFrontFacingWinding:" => set_front_facing_winding, c"v24@0:8Q16";
        "setTriangleFillMode:" => set_triangle_fill_mode, c"v24@0:8Q16";
        "setVertexBuffer:offset:atIndex:" => set_vertex_buffer, c"v40@0:8@16Q24Q32";
        "setFragmentBuffer:offset:atIndex:" => set_fragment_buffer, c"v40@0:8@16Q24Q32";
        "setVertexBytes:length:atIndex:" => set_vertex_bytes, c"v40@0:8^v16Q24Q32";
        "setFragmentBytes:length:atIndex:" => set_fragment_bytes, c"v40@0:8^v16Q24Q32";
        "setVertexBufferOffset:atIndex:" => set_vertex_buffer_offset, c"v32@0:8Q16Q24";
        "setFragmentBufferOffset:atIndex:" => set_fragment_buffer_offset, c"v32@0:8Q16Q24";
        "setVertexTexture:atIndex:" => set_vertex_texture, c"v32@0:8@16Q24";
        "setFragmentTexture:atIndex:" => set_fragment_texture, c"v32@0:8@16Q24";
        "setVertexSamplerState:atIndex:" => set_vertex_sampler, c"v32@0:8@16Q24";
        "setFragmentSamplerState:atIndex:" => set_fragment_sampler, c"v32@0:8@16Q24";
        "drawPrimitives:vertexStart:vertexCount:" => draw_primitives, c"v40@0:8Q16Q24Q32";
        "drawPrimitives:vertexStart:vertexCount:instanceCount:" => draw_primitives_instanced,
            c"v48@0:8Q16Q24Q32Q40";
        "drawPrimitives:vertexStart:vertexCount:instanceCount:baseInstance:" => draw_primitives_base,
            c"v56@0:8Q16Q24Q32Q40Q48";
        "drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:" => draw_indexed_instanced,
            c"v64@0:8Q16Q24Q32@40Q48Q56";
        "drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:baseVertex:baseInstance:" => draw_indexed_base,
            c"v80@0:8Q16Q24Q32@40Q48Q56q64Q72";
        "endEncoding" => end_encoding, c"v16@0:8";
        "dealloc" => encoder_dealloc, c"v16@0:8";
    });
}

/// Copy the snapshot from Rosemary's guest vertex descriptor into real
/// Apple Metal objects. Do not pass guest collection or element pointers to
/// the host Objective-C runtime.
fn host_vertex_descriptor(guest: Id) -> Result<HostId, &'static str> {
    if guest.is_null() {
        return Ok(std::ptr::null_mut());
    }
    if !object::is_kind(guest, c"MTLVertexDescriptor") {
        return Err("expected a guest MTLVertexDescriptor");
    }
    let guest = unsafe { object::body::<resource::VertexDescriptor>(guest) };
    let class = unsafe { host_get_class(c"MTLVertexDescriptor".as_ptr()) };
    if class.is_null() {
        return Err("Apple MTLVertexDescriptor class not present");
    }
    let native = unsafe { host_send_id(class, sel(c"new")) };
    if native.is_null() {
        return Err("Apple MTLVertexDescriptor allocation failed");
    }
    let host_layouts = unsafe { host_send_id(native, sel(c"layouts")) };
    let host_attributes = unsafe { host_send_id(native, sel(c"attributes")) };
    if host_layouts.is_null() || host_attributes.is_null() {
        unsafe { host_release(native) };
        return Err("Apple MTLVertexDescriptor has no layouts or attributes");
    }
    for (index, attr) in guest.attributes.iter().enumerate() {
        if attr.format == 0 {
            continue;
        }
        if attr.buffer_index >= resource::VERTEX_SLOTS as u64 {
            unsafe { host_release(native) };
            return Err("vertex attribute buffer index outside 31 Metal slots");
        }
        let native_attr = unsafe {
            host_send_id_u64(
                host_attributes,
                sel(c"objectAtIndexedSubscript:"),
                index as u64,
            )
        };
        if native_attr.is_null() {
            unsafe { host_release(native) };
            return Err("Apple Metal did not provide a vertex attribute descriptor");
        }
        for (selector, value) in [
            (c"setFormat:".as_ref(), attr.format),
            (c"setOffset:".as_ref(), attr.offset),
            (c"setBufferIndex:".as_ref(), attr.buffer_index),
        ] {
            unsafe { host_send_void_u64(native_attr, sel(selector), value) };
        }
    }
    // Metal applies a layout only for an attribute that references its buffer.
    for (index, layout) in guest.layouts.iter().enumerate() {
        if !guest
            .attributes
            .iter()
            .any(|a| a.format != 0 && a.buffer_index == index as u64)
        {
            continue;
        }
        if layout.step_function > 4 || layout.step_rate == 0 {
            unsafe { host_release(native) };
            return Err("vertex buffer layout has an unsupported step function or zero step rate");
        }
        let native_layout = unsafe {
            host_send_id_u64(
                host_layouts,
                sel(c"objectAtIndexedSubscript:"),
                index as u64,
            )
        };
        if native_layout.is_null() {
            unsafe { host_release(native) };
            return Err("Apple Metal did not provide a vertex buffer layout");
        }
        for (selector, value) in [
            (c"setStride:".as_ref(), layout.stride),
            (c"setStepFunction:".as_ref(), layout.step_function),
            (c"setStepRate:".as_ref(), layout.step_rate),
        ] {
            unsafe { host_send_void_u64(native_layout, sel(selector), value) };
        }
    }
    Ok(native)
}

fn configure_host_color_attachment(
    host: HostId,
    described: &pipeline::ColorAttachment,
) -> Result<(), &'static str> {
    if described.write_mask > MTL_COLOR_WRITE_MASK_ALL
        || described.rgb_operation > 4
        || described.alpha_operation > 4
        || [
            described.source_rgb,
            described.destination_rgb,
            described.source_alpha,
            described.destination_alpha,
        ]
        .iter()
        .any(|&v| v > 18)
    {
        return Err("invalid Metal render color blend factor, operation, or write mask");
    }
    unsafe {
        host_send_void_u64(host, sel(c"setPixelFormat:"), described.pixel_format);
        host_send_void_bool(host, sel(c"setBlendingEnabled:"), described.blending);
        host_send_void_u64(host, sel(c"setWriteMask:"), described.write_mask);
    }
    if described.blending {
        for (selector, value) in [
            (c"setSourceRGBBlendFactor:".as_ref(), described.source_rgb),
            (
                c"setDestinationRGBBlendFactor:".as_ref(),
                described.destination_rgb,
            ),
            (c"setRgbBlendOperation:".as_ref(), described.rgb_operation),
            (
                c"setSourceAlphaBlendFactor:".as_ref(),
                described.source_alpha,
            ),
            (
                c"setDestinationAlphaBlendFactor:".as_ref(),
                described.destination_alpha,
            ),
            (
                c"setAlphaBlendOperation:".as_ref(),
                described.alpha_operation,
            ),
        ] {
            unsafe { host_send_void_u64(host, sel(selector), value) };
        }
    }
    Ok(())
}

pub(super) unsafe extern "C" fn new_render_pipeline(
    receiver: Id,
    _sel: Sel,
    descriptor: Id,
    error: *mut Id,
) -> Id {
    if !error.is_null() {
        unsafe { *error = std::ptr::null_mut() };
    }
    let fail = |reason: &str| compute::fail(error, "MTLRenderPipelineErrorDomain", reason);
    if !object::is_kind(descriptor, c"MTLRenderPipelineDescriptor") {
        return fail("expected a render pipeline descriptor");
    }
    let described = unsafe { object::body::<pipeline::RenderPipelineDescriptor>(descriptor) };
    if !object::is_kind(described.vertex_function, c"RosemaryNativeMTLFunction")
        || (!described.fragment_function.is_null()
            && !object::is_kind(described.fragment_function, c"RosemaryNativeMTLFunction"))
    {
        return fail(
            "native render pipeline requires a native vertex function and optional native fragment function",
        );
    }
    let vertex = unsafe { object::body::<compute::NativeFunction>(described.vertex_function) };
    let fragment = if described.fragment_function.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { object::body::<compute::NativeFunction>(described.fragment_function).host }
    };
    if unsafe { compute::function_device(described.vertex_function, std::ptr::null()) } != receiver
        || (!described.fragment_function.is_null()
            && unsafe { compute::function_device(described.fragment_function, std::ptr::null()) }
                != receiver)
    {
        return fail("native render functions belong to another device");
    }
    if !matches!(described.sample_count, 1 | 2 | 4 | 8 | 16) || described.input_topology > 3 {
        return fail("invalid Metal raster sample count or input topology");
    }
    let host_device = unsafe { object::body::<NativeDevice>(receiver) }.host;
    if !unsafe {
        host_send_bool_u64(
            host_device,
            sel(c"supportsTextureSampleCount:"),
            described.sample_count,
        )
    } {
        return fail("host Metal device does not support the requested render sample count");
    }
    let _pool = HostPool::new();
    let class = unsafe { host_get_class(c"MTLRenderPipelineDescriptor".as_ptr()) };
    if class.is_null() {
        return fail("native MTLRenderPipelineDescriptor class missing");
    }
    let host_descriptor = unsafe { host_send_id(class, sel(c"new")) };
    if host_descriptor.is_null() {
        return fail("native render descriptor allocation failed");
    }
    unsafe {
        host_send_void_id(host_descriptor, sel(c"setVertexFunction:"), vertex.host);
        host_send_void_id(host_descriptor, sel(c"setFragmentFunction:"), fragment);
    }
    let host_vertex = match host_vertex_descriptor(described.vertex_descriptor) {
        Ok(value) => value,
        Err(reason) => {
            unsafe { host_release(host_descriptor) };
            return fail(reason);
        }
    };
    if !host_vertex.is_null() {
        unsafe {
            host_send_void_id(host_descriptor, sel(c"setVertexDescriptor:"), host_vertex);
            host_release(host_vertex);
        }
    }
    for (selector, value) in [
        (
            c"setDepthAttachmentPixelFormat:".as_ref(),
            described.depth_format,
        ),
        (
            c"setStencilAttachmentPixelFormat:".as_ref(),
            described.stencil_format,
        ),
        (c"setRasterSampleCount:".as_ref(), described.sample_count),
        (
            c"setInputPrimitiveTopology:".as_ref(),
            described.input_topology,
        ),
    ] {
        unsafe { host_send_void_u64(host_descriptor, sel(selector), value) };
    }
    for (selector, value) in [
        (
            c"setAlphaToCoverageEnabled:".as_ref(),
            described.alpha_to_coverage,
        ),
        (c"setAlphaToOneEnabled:".as_ref(), described.alpha_to_one),
        (
            c"setRasterizationEnabled:".as_ref(),
            described.rasterization,
        ),
    ] {
        unsafe { host_send_void_bool(host_descriptor, sel(selector), value) };
    }
    let attachments = unsafe { host_send_id(host_descriptor, sel(c"colorAttachments")) };
    if attachments.is_null() {
        unsafe { host_release(host_descriptor) };
        return fail("native Metal render color attachments are unavailable");
    }
    for (index, color) in described.colors.iter().enumerate() {
        let host_color = unsafe {
            host_send_id_u64(attachments, sel(c"objectAtIndexedSubscript:"), index as u64)
        };
        if host_color.is_null() {
            unsafe { host_release(host_descriptor) };
            return fail("native Metal render color attachment descriptor is missing");
        }
        if let Err(reason) = configure_host_color_attachment(host_color, color) {
            unsafe { host_release(host_descriptor) };
            return fail(reason);
        }
    }
    let mut host_error = std::ptr::null_mut();
    let host = unsafe {
        host_send_id_id_ptr(
            object::body::<NativeDevice>(receiver).host,
            sel(c"newRenderPipelineStateWithDescriptor:error:"),
            host_descriptor,
            &mut host_error,
        )
    };
    unsafe { host_release(host_descriptor) };
    if host.is_null() {
        return fail(&compute::native_error(
            host_error,
            "native render pipeline compilation failed",
        ));
    }
    objc::arc::retain(receiver);
    let guest = object::make(
        c"RosemaryNativeMTLRenderPipelineState",
        NativeRenderPipeline {
            host,
            device: receiver,
        },
    );
    if guest.is_null() {
        unsafe { host_release(host) };
        objc::arc::release(receiver);
    }
    guest
}

unsafe extern "C" fn pipeline_device(receiver: Id, _sel: Sel) -> Id {
    unsafe { object::body::<NativeRenderPipeline>(receiver) }.device
}

unsafe extern "C" fn pipeline_dealloc(receiver: Id, _sel: Sel) {
    let pipeline = unsafe { object::body::<NativeRenderPipeline>(receiver) };
    unsafe { host_release(pipeline.host) };
    objc::arc::release(pipeline.device);
    objc::arc::finalize(receiver);
}

/// Copy one Rosemary render attachment to Apple's descriptor. A pass can
/// have multiple colors, a depth/stencil-only target, or a multisample resolve;
/// copying the whole snapshot is necessary for genuine Metal rendering.
/// The result is the mip-level's width/height, if the attachment is present.
fn host_render_attachment(
    host_attachment: HostId,
    guest: &encoder::AttachmentDescriptor,
    device: Id,
    kind: u8, // 0 color, 1 depth, 2 stencil
) -> Result<Option<(u64, u64)>, &'static str> {
    if guest.texture.is_null() {
        if !guest.resolve_texture.is_null() {
            return Err("a render pass resolve has no source attachment");
        }
        return Ok(None);
    }
    if !object::is_kind(guest.texture, c"RosemaryNativeMTLTexture") {
        return Err("a render pass needs native Metal attachment textures");
    }
    let texture = unsafe { object::body::<texture::NativeTexture>(guest.texture) };
    if texture.device != device {
        return Err("a render attachment belongs to another Metal device");
    }
    if !matches!(guest.load_action, 0..=2) || guest.store_action > 5 {
        return Err("a render pass has an invalid load or store action");
    }
    let _pool = HostPool::new();
    let query = |host: HostId, selector: &CStr| unsafe { host_send_u64(host, sel(selector)) };
    let level_extent =
        |host: HostId, level: u64, selector: &CStr| (query(host, selector) >> level.min(63)).max(1);
    let check_texture = |host: HostId, level: u64, slice: u64| {
        if level >= query(host, c"mipmapLevelCount") {
            return false;
        }
        let kind = query(host, c"textureType");
        let slices = match kind {
            2 | 4 | 7 => 1,
            3 | 8 => query(host, c"arrayLength"),
            5 => 6,
            6 => query(host, c"arrayLength").saturating_mul(6),
            _ => 0,
        };
        slice < slices
    };
    if !check_texture(texture.host, guest.level, guest.slice) {
        return Err("a render texture mip level or slice is out of range");
    }
    if !guest.clear_color.iter().all(|x| x.is_finite()) || !guest.clear_depth.is_finite() {
        return Err("a render pass has an invalid clear value");
    }
    unsafe {
        host_send_void_id(host_attachment, sel(c"setTexture:"), texture.host);
        host_send_void_u64(host_attachment, sel(c"setLevel:"), guest.level);
        host_send_void_u64(host_attachment, sel(c"setSlice:"), guest.slice);
        host_send_void_u64(host_attachment, sel(c"setLoadAction:"), guest.load_action);
        host_send_void_u64(host_attachment, sel(c"setStoreAction:"), guest.store_action);
    }
    if !guest.resolve_texture.is_null() {
        if !object::is_kind(guest.resolve_texture, c"RosemaryNativeMTLTexture") {
            return Err("a render pass resolve needs a native Metal texture");
        }
        let resolve = unsafe { object::body::<texture::NativeTexture>(guest.resolve_texture) };
        if resolve.device != device
            || !check_texture(resolve.host, guest.resolve_level, guest.resolve_slice)
        {
            return Err("a render pass resolve targets another device or invalid slice");
        }
        unsafe {
            host_send_void_id(host_attachment, sel(c"setResolveTexture:"), resolve.host);
            host_send_void_u64(
                host_attachment,
                sel(c"setResolveLevel:"),
                guest.resolve_level,
            );
            host_send_void_u64(
                host_attachment,
                sel(c"setResolveSlice:"),
                guest.resolve_slice,
            );
        }
    }
    match kind {
        0 => unsafe {
            host_send_void_clear_color(
                host_attachment,
                sel(c"setClearColor:"),
                encoder::ClearColor {
                    red: guest.clear_color[0],
                    green: guest.clear_color[1],
                    blue: guest.clear_color[2],
                    alpha: guest.clear_color[3],
                },
            )
        },
        1 => {
            unsafe {
                host_send_void_f64(host_attachment, sel(c"setClearDepth:"), guest.clear_depth)
            };
            if guest.resolve_filter != 0 {
                if guest.resolve_filter > 2 {
                    return Err("invalid Metal depth resolve filter");
                }
                unsafe {
                    host_send_void_u64(
                        host_attachment,
                        sel(c"setDepthResolveFilter:"),
                        guest.resolve_filter,
                    )
                };
            }
        }
        2 => {
            unsafe {
                host_send_void_u64(
                    host_attachment,
                    sel(c"setClearStencil:"),
                    guest.clear_stencil as u64,
                )
            };
            if guest.resolve_filter != 0 {
                if guest.resolve_filter > 1 {
                    return Err("invalid Metal stencil resolve filter");
                }
                unsafe {
                    host_send_void_u64(
                        host_attachment,
                        sel(c"setStencilResolveFilter:"),
                        guest.resolve_filter,
                    )
                };
            }
        }
        _ => unreachable!(),
    }
    Ok(Some((
        level_extent(texture.host, guest.level, c"width"),
        level_extent(texture.host, guest.level, c"height"),
    )))
}

pub(super) unsafe extern "C" fn new_render_encoder(receiver: Id, _sel: Sel, descriptor: Id) -> Id {
    if !object::is_kind(descriptor, c"MTLRenderPassDescriptor") {
        return std::ptr::null_mut();
    }
    let pass = unsafe { object::body::<encoder::RenderPassDescriptor>(descriptor) };
    let command = unsafe { object::body::<NativeCommandBuffer>(receiver) };
    let guest_device = unsafe { command_device(receiver, std::ptr::null()) };
    let _pool = HostPool::new();
    let class = unsafe { host_get_class(c"MTLRenderPassDescriptor".as_ptr()) };
    if class.is_null() {
        return std::ptr::null_mut();
    }
    let host_descriptor = unsafe { host_send_id(class, sel(c"new")) };
    if host_descriptor.is_null() {
        return std::ptr::null_mut();
    }
    let make_pass = || -> Result<(u64, u64), &'static str> {
        let mut extent = None;
        let colors = unsafe { host_send_id(host_descriptor, sel(c"colorAttachments")) };
        if colors.is_null() {
            return Err("host Metal render color attachments are missing");
        }
        for (index, guest) in pass.colors.iter().enumerate() {
            if guest.texture.is_null() && guest.resolve_texture.is_null() {
                continue;
            }
            let host_attachment = unsafe {
                host_send_id_u64(colors, sel(c"objectAtIndexedSubscript:"), index as u64)
            };
            if host_attachment.is_null() {
                return Err("host Metal render color slot is missing");
            }
            if let Some(size) = host_render_attachment(host_attachment, guest, guest_device, 0)? {
                if extent.is_some_and(|old| old != size) {
                    return Err("render attachments disagree on their dimensions");
                }
                extent = Some(size);
            }
        }
        for (selector, guest, kind) in [
            (c"depthAttachment".as_ref(), &pass.depth, 1),
            (c"stencilAttachment".as_ref(), &pass.stencil, 2),
        ] {
            if guest.texture.is_null() && guest.resolve_texture.is_null() {
                continue;
            }
            let attachment = unsafe { host_send_id(host_descriptor, sel(selector)) };
            if attachment.is_null() {
                return Err("host Metal depth/stencil attachment is missing");
            }
            if let Some(size) = host_render_attachment(attachment, guest, guest_device, kind)? {
                if extent.is_some_and(|old| old != size) {
                    return Err("render attachments disagree on their dimensions");
                }
                extent = Some(size);
            }
        }
        if !pass.visibility_result_buffer.is_null() {
            if !object::is_kind(pass.visibility_result_buffer, c"RosemaryNativeMTLBuffer") {
                return Err("native render visibility result requires a native Metal buffer");
            }
            let buffer = unsafe { object::body::<NativeBuffer>(pass.visibility_result_buffer) };
            if buffer.device != guest_device {
                return Err("render visibility buffer belongs to another device");
            }
            unsafe {
                host_send_void_id(
                    host_descriptor,
                    sel(c"setVisibilityResultBuffer:"),
                    buffer.host,
                )
            };
            if pass.visibility_result_type != 0 {
                let selector = sel(c"setVisibilityResultType:");
                if !unsafe {
                    host_send_bool_sel(host_descriptor, sel(c"respondsToSelector:"), selector)
                } {
                    return Err("host Metal render pass cannot set visibility result type");
                }
                unsafe {
                    host_send_void_u64(host_descriptor, selector, pass.visibility_result_type)
                };
            }
        }
        if pass.render_target_array_length > 1 {
            unsafe {
                host_send_void_u64(
                    host_descriptor,
                    sel(c"setRenderTargetArrayLength:"),
                    pass.render_target_array_length,
                )
            };
        }
        extent.ok_or("render pass has no native attachment")
    };
    let extent = match make_pass() {
        Ok(size) => size,
        Err(reason) => {
            gpu::trace_once(format!("native render pass: {reason}"));
            unsafe { host_release(host_descriptor) };
            return std::ptr::null_mut();
        }
    };
    let host = unsafe {
        host_send_id_id(
            command.host,
            sel(c"renderCommandEncoderWithDescriptor:"),
            host_descriptor,
        )
    };
    unsafe { host_release(host_descriptor) };
    if host.is_null() {
        gpu::trace_once("native host Metal render encoder returned nil");
        return std::ptr::null_mut();
    }
    let host = unsafe { host_retain(host) };
    objc::arc::retain(receiver);
    let guest = object::make(
        c"RosemaryNativeMTLRenderCommandEncoder",
        NativeRenderEncoder {
            host,
            command: receiver,
            width: extent.0,
            height: extent.1,
        },
    );
    if guest.is_null() {
        unsafe { host_release(host) };
        objc::arc::release(receiver);
    }
    objc::arc::autorelease(guest)
}

unsafe extern "C" fn encoder_device(receiver: Id, _sel: Sel) -> Id {
    let command = unsafe { object::body::<NativeRenderEncoder>(receiver) }.command;
    unsafe { command_device(command, std::ptr::null()) }
}

unsafe extern "C" fn set_viewport(receiver: Id, _sel: Sel, viewport: encoder::Viewport) {
    if ![
        viewport.origin_x,
        viewport.origin_y,
        viewport.width,
        viewport.height,
        viewport.znear,
        viewport.zfar,
    ]
    .iter()
    .all(|n| n.is_finite())
        || viewport.width <= 0.0
        || viewport.height <= 0.0
        || !(0.0..=1.0).contains(&viewport.znear)
        || !(0.0..=1.0).contains(&viewport.zfar)
    {
        gpu::trace("native Metal rejected invalid render viewport".to_string());
        return;
    }
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe { host_send_void_viewport(host, sel(c"setViewport:"), viewport) };
}

unsafe extern "C" fn set_depth_bias(receiver: Id, _sel: Sel, bias: f32, slope: f32, clamp: f32) {
    if ![bias, slope, clamp].iter().all(|x| x.is_finite()) {
        gpu::trace("native Metal rejected non-finite depth bias".to_string());
        return;
    }
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe {
        host_send_void_f32_f32_f32(
            host,
            sel(c"setDepthBias:slopeScale:clamp:"),
            bias,
            slope,
            clamp,
        )
    };
}

unsafe extern "C" fn set_blend_color(
    receiver: Id,
    _sel: Sel,
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
) {
    if ![red, green, blue, alpha].iter().all(|x| x.is_finite()) {
        gpu::trace("native Metal rejected non-finite blend color".to_string());
        return;
    }
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe {
        host_send_void_f32_f32_f32_f32(
            host,
            sel(c"setBlendColorRed:green:blue:alpha:"),
            red,
            green,
            blue,
            alpha,
        )
    };
}

unsafe extern "C" fn set_depth_clip_mode(receiver: Id, _sel: Sel, mode: u64) {
    if mode > 1 {
        gpu::trace("native Metal rejected invalid depth clip mode".to_string());
        return;
    }
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe { host_send_void_u64(host, sel(c"setDepthClipMode:"), mode) };
}

unsafe extern "C" fn set_scissor_rect(receiver: Id, _sel: Sel, rect: encoder::ScissorRect) {
    let render = unsafe { object::body::<NativeRenderEncoder>(receiver) };
    // Empty Metal scissors are valid and discard all fragments. DOORS uses
    // zero-height rectangles to hide UI elements; Apple Metal accepts them.
    // Reject only rectangles whose endpoints lie outside the render target.
    if !rect
        .x
        .checked_add(rect.width)
        .is_some_and(|right| right <= render.width)
        || !rect
            .y
            .checked_add(rect.height)
            .is_some_and(|bottom| bottom <= render.height)
    {
        gpu::trace("native Metal rejected out-of-bounds render scissor".to_string());
        return;
    }
    let _pool = HostPool::new();
    unsafe { host_send_void_scissor(render.host, sel(c"setScissorRect:"), rect) };
}

unsafe extern "C" fn set_pipeline(receiver: Id, _sel: Sel, pipeline: Id) {
    if !object::is_kind(pipeline, c"RosemaryNativeMTLRenderPipelineState")
        || unsafe { pipeline_device(pipeline, std::ptr::null()) }
            != unsafe { encoder_device(receiver, std::ptr::null()) }
    {
        gpu::trace("native render encoder requires a pipeline on this device".to_string());
        return;
    }
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    let pipeline = unsafe { object::body::<NativeRenderPipeline>(pipeline) }.host;
    unsafe { host_send_void_id(host, sel(c"setRenderPipelineState:"), pipeline) };
}

unsafe extern "C" fn set_depth_stencil_state(receiver: Id, _sel: Sel, state: Id) {
    let host_state = if state.is_null() {
        std::ptr::null_mut()
    } else if object::is_kind(state, c"RosemaryNativeMTLDepthStencilState") {
        let body = unsafe { object::body::<depth::NativeDepthStencilState>(state) };
        if body.device != unsafe { encoder_device(receiver, std::ptr::null()) } {
            gpu::trace(
                "native Metal render depth/stencil state belongs to another device".to_string(),
            );
            return;
        }
        body.host
    } else {
        gpu::trace("native Metal render encoder requires a native depth/stencil state".to_string());
        return;
    };
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe { host_send_void_id(host, sel(c"setDepthStencilState:"), host_state) };
}

unsafe extern "C" fn set_stencil_reference_value(receiver: Id, _sel: Sel, value: u32) {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe { host_send_void_u64(host, sel(c"setStencilReferenceValue:"), value as u64) };
}

unsafe extern "C" fn set_stencil_reference_values(receiver: Id, _sel: Sel, front: u32, back: u32) {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe {
        host_send_void_u64_u64(
            host,
            sel(c"setStencilFrontReferenceValue:backReferenceValue:"),
            front as u64,
            back as u64,
        )
    };
}

fn set_render_enum(receiver: Id, selector: &CStr, value: u64, max: u64) {
    // Metal raises an Objective-C exception for invalid rasterization enums.
    if value > max {
        gpu::trace(format!(
            "native Metal rejected invalid rasterization enum for {}",
            selector.to_string_lossy()
        ));
        return;
    }
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe { host_send_void_u64(host, sel(selector), value) };
}

unsafe extern "C" fn set_cull_mode(receiver: Id, _sel: Sel, value: u64) {
    set_render_enum(receiver, c"setCullMode:", value, 2);
}
unsafe extern "C" fn set_front_facing_winding(receiver: Id, _sel: Sel, value: u64) {
    set_render_enum(receiver, c"setFrontFacingWinding:", value, 1);
}
unsafe extern "C" fn set_triangle_fill_mode(receiver: Id, _sel: Sel, value: u64) {
    set_render_enum(receiver, c"setTriangleFillMode:", value, 1);
}

fn set_buffer_offset(receiver: Id, selector: &CStr, offset: u64, index: u64) {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe { host_send_void_u64_u64(host, sel(selector), offset, index) };
}
fn set_inline_bytes(receiver: Id, selector: &CStr, bytes: *const c_void, length: u64, index: u64) {
    if (length > 0 && bytes.is_null()) || length > 4096 {
        gpu::trace("native Metal render inline bytes invalid or over 4 KiB".to_string());
        return;
    }
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe { host_send_void_ptr_u64_u64(host, sel(selector), bytes, length, index) };
}

unsafe extern "C" fn set_vertex_bytes(
    receiver: Id,
    _sel: Sel,
    bytes: *const c_void,
    length: u64,
    index: u64,
) {
    set_inline_bytes(
        receiver,
        c"setVertexBytes:length:atIndex:",
        bytes,
        length,
        index,
    );
}
unsafe extern "C" fn set_fragment_bytes(
    receiver: Id,
    _sel: Sel,
    bytes: *const c_void,
    length: u64,
    index: u64,
) {
    set_inline_bytes(
        receiver,
        c"setFragmentBytes:length:atIndex:",
        bytes,
        length,
        index,
    );
}

unsafe extern "C" fn set_vertex_buffer_offset(receiver: Id, _sel: Sel, offset: u64, index: u64) {
    set_buffer_offset(receiver, c"setVertexBufferOffset:atIndex:", offset, index);
}
unsafe extern "C" fn set_fragment_buffer_offset(receiver: Id, _sel: Sel, offset: u64, index: u64) {
    set_buffer_offset(receiver, c"setFragmentBufferOffset:atIndex:", offset, index);
}

fn set_buffer(receiver: Id, selector: &CStr, buffer: Id, offset: u64, index: u64) {
    let native_buffer = if buffer.is_null() {
        // Passing nil unbinds the vertex/fragment slot in Apple Metal.
        std::ptr::null_mut()
    } else if object::is_kind(buffer, c"RosemaryNativeMTLBuffer") {
        let guest = unsafe { object::body::<NativeBuffer>(buffer) };
        if guest.device != unsafe { encoder_device(receiver, std::ptr::null()) } {
            gpu::trace("native render buffer belongs to another device".to_string());
            return;
        }
        guest.host
    } else {
        gpu::trace("native render encoder requires a native buffer".to_string());
        return;
    };
    let _pool = HostPool::new();
    if !native_buffer.is_null() && offset > unsafe { host_send_u64(native_buffer, sel(c"length")) }
    {
        gpu::trace("native render buffer offset out of bounds".to_string());
        return;
    }
    let encoder = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe { host_send_void_id_u64_u64(encoder, sel(selector), native_buffer, offset, index) };
}

unsafe extern "C" fn set_vertex_buffer(
    receiver: Id,
    _sel: Sel,
    buffer: Id,
    offset: u64,
    index: u64,
) {
    set_buffer(
        receiver,
        c"setVertexBuffer:offset:atIndex:",
        buffer,
        offset,
        index,
    );
}

unsafe extern "C" fn set_fragment_buffer(
    receiver: Id,
    _sel: Sel,
    buffer: Id,
    offset: u64,
    index: u64,
) {
    set_buffer(
        receiver,
        c"setFragmentBuffer:offset:atIndex:",
        buffer,
        offset,
        index,
    );
}

fn set_texture(receiver: Id, selector: &CStr, texture: Id, index: u64) {
    let native = if texture.is_null() {
        std::ptr::null_mut()
    } else if object::is_kind(texture, c"RosemaryNativeMTLTexture") {
        let body = unsafe { object::body::<texture::NativeTexture>(texture) };
        if body.device != unsafe { encoder_device(receiver, std::ptr::null()) } {
            gpu::trace("native render texture belongs to a different device".to_string());
            return;
        }
        body.host
    } else {
        gpu::trace("native render encoder requires a native Metal texture".to_string());
        return;
    };
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe { host_send_void_id_u64(host, sel(selector), native, index) };
}

fn set_sampler(receiver: Id, selector: &CStr, sampler: Id, index: u64) {
    let native = if sampler.is_null() {
        std::ptr::null_mut()
    } else if object::is_kind(sampler, c"RosemaryNativeMTLSamplerState") {
        let body = unsafe { object::body::<sampler::NativeSampler>(sampler) };
        if body.device != unsafe { encoder_device(receiver, std::ptr::null()) } {
            gpu::trace("native render sampler belongs to a different device".to_string());
            return;
        }
        body.host
    } else {
        gpu::trace("native render encoder requires a native Metal sampler".to_string());
        return;
    };
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe { host_send_void_id_u64(host, sel(selector), native, index) };
}

unsafe extern "C" fn set_vertex_texture(receiver: Id, _sel: Sel, texture: Id, index: u64) {
    set_texture(receiver, c"setVertexTexture:atIndex:", texture, index);
}
unsafe extern "C" fn set_fragment_texture(receiver: Id, _sel: Sel, texture: Id, index: u64) {
    set_texture(receiver, c"setFragmentTexture:atIndex:", texture, index);
}
unsafe extern "C" fn set_vertex_sampler(receiver: Id, _sel: Sel, sampler: Id, index: u64) {
    set_sampler(receiver, c"setVertexSamplerState:atIndex:", sampler, index);
}
unsafe extern "C" fn set_fragment_sampler(receiver: Id, _sel: Sel, sampler: Id, index: u64) {
    set_sampler(
        receiver,
        c"setFragmentSamplerState:atIndex:",
        sampler,
        index,
    );
}

unsafe extern "C" fn draw_primitives(
    receiver: Id,
    _sel: Sel,
    primitive: u64,
    start: u64,
    count: u64,
) {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe {
        host_send_void_u64_u64_u64(
            host,
            sel(c"drawPrimitives:vertexStart:vertexCount:"),
            primitive,
            start,
            count,
        )
    };
}

unsafe extern "C" fn draw_primitives_instanced(
    receiver: Id,
    _sel: Sel,
    primitive: u64,
    start: u64,
    count: u64,
    instances: u64,
) {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe {
        host_send_void_u64_u64_u64_u64(
            host,
            sel(c"drawPrimitives:vertexStart:vertexCount:instanceCount:"),
            primitive,
            start,
            count,
            instances,
        )
    };
}

unsafe extern "C" fn draw_primitives_base(
    receiver: Id,
    _sel: Sel,
    primitive: u64,
    start: u64,
    count: u64,
    instances: u64,
    base: u64,
) {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe {
        host_send_void_u64_u64_u64_u64_u64(
            host,
            sel(c"drawPrimitives:vertexStart:vertexCount:instanceCount:baseInstance:"),
            primitive,
            start,
            count,
            instances,
            base,
        )
    };
}

fn native_index_buffer(
    receiver: Id,
    buffer: Id,
    offset: u64,
    count: u64,
    index_type: u64,
) -> Option<HostId> {
    if index_type > 1 || !object::is_kind(buffer, c"RosemaryNativeMTLBuffer") {
        gpu::trace(
            "native indexed draw requires UInt16/UInt32 indices and a native buffer".to_string(),
        );
        return None;
    }
    let native = unsafe { object::body::<NativeBuffer>(buffer) };
    if native.device != unsafe { encoder_device(receiver, std::ptr::null()) } {
        gpu::trace("native index buffer belongs to another Metal device".to_string());
        return None;
    }
    let stride = if index_type == 0 { 2u64 } else { 4u64 };
    let _pool = HostPool::new();
    let capacity = unsafe { host_send_u64(native.host, sel(c"length")) };
    if offset % stride != 0
        || !count
            .checked_mul(stride)
            .and_then(|bytes| offset.checked_add(bytes))
            .is_some_and(|end| end <= capacity)
    {
        gpu::trace("native indexed draw's index buffer range is invalid".to_string());
        return None;
    }
    Some(native.host)
}

unsafe extern "C" fn draw_indexed_instanced(
    receiver: Id,
    _sel: Sel,
    primitive: u64,
    count: u64,
    index_type: u64,
    buffer: Id,
    offset: u64,
    instances: u64,
) {
    let Some(host_buffer) = native_index_buffer(receiver, buffer, offset, count, index_type) else {
        return;
    };
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe {
        host_send_void_indexed(
        host, sel(c"drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:"),
        primitive, count, index_type, host_buffer, offset, instances,
    )
    };
}

unsafe extern "C" fn draw_indexed_base(
    receiver: Id,
    _sel: Sel,
    primitive: u64,
    count: u64,
    index_type: u64,
    buffer: Id,
    offset: u64,
    instances: u64,
    base_vertex: i64,
    base_instance: u64,
) {
    let Some(host_buffer) = native_index_buffer(receiver, buffer, offset, count, index_type) else {
        return;
    };
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe {
        host_send_void_indexed_base(
        host, sel(c"drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:baseVertex:baseInstance:"),
        primitive, count, index_type, host_buffer, offset, instances, base_vertex, base_instance,
    )
    };
}

unsafe extern "C" fn end_encoding(receiver: Id, _sel: Sel) {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeRenderEncoder>(receiver) }.host;
    unsafe { host_send_void(host, sel(c"endEncoding")) };
}

unsafe extern "C" fn encoder_dealloc(receiver: Id, _sel: Sel) {
    let encoder = unsafe { object::body::<NativeRenderEncoder>(receiver) };
    unsafe { host_release(encoder.host) };
    objc::arc::release(encoder.command);
    objc::arc::finalize(receiver);
}
