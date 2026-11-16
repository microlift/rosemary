use super::*;

#[test]
fn multisampled_depth_and_stencil_can_use_aliases_of_one_texture() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let gpu = gpu_of(device).unwrap();
    let supported = gpu.limits.framebuffer_color_sample_counts
        & gpu.limits.framebuffer_depth_sample_counts
        & gpu.limits.framebuffer_stencil_sample_counts;
    if !supported.contains(ash::vk::SampleCountFlags::TYPE_4) {
        return;
    }

    const SIZE: u64 = 8;
    const MTL_PIXEL_FORMAT_DEPTH32_FLOAT_STENCIL8: u64 = 260;
    const MTL_TEXTURE_TYPE_2D_MULTISAMPLE: u64 = 4;
    let texture = |format, samples, usage| {
        resource::new_texture(
            gpu,
            resource::TextureDescriptor {
                texture_type: if samples == 1 {
                    MTL_TEXTURE_TYPE_2D
                } else {
                    MTL_TEXTURE_TYPE_2D_MULTISAMPLE
                },
                pixel_format: format,
                width: SIZE,
                height: SIZE,
                depth: 1,
                mipmap_level_count: 1,
                sample_count: samples,
                array_length: 1,
                storage_mode: MTL_STORAGE_MODE_PRIVATE,
                usage,
            },
        )
    };
    let depth = texture(
        MTL_PIXEL_FORMAT_DEPTH32_FLOAT_STENCIL8,
        4,
        gpu::USAGE_RENDER_TARGET | gpu::USAGE_PIXEL_FORMAT_VIEW,
    );
    let color = texture(MTL_PIXEL_FORMAT_BGRA8_UNORM, 4, gpu::USAGE_RENDER_TARGET);
    let resolved = texture(MTL_PIXEL_FORMAT_BGRA8_UNORM, 1, gpu::USAGE_RENDER_TARGET);
    assert!(!depth.is_null() && !color.is_null() && !resolved.is_null());
    // SAFETY: test-owned Metal objects and the selectors they implement.
    unsafe {
        let queue = new_command_queue(device, std::ptr::null());
        let readback = new_buffer(device, std::ptr::null(), (SIZE * SIZE * 4) as usize, 0);
        let new_view: unsafe extern "C" fn(Id, Sel, u64) -> Id =
            std::mem::transmute(objc::msg::entry());
        let stencil = new_view(
            depth,
            objc::sel::get("newTextureViewWithPixelFormat:"),
            MTL_PIXEL_FORMAT_DEPTH32_FLOAT_STENCIL8,
        );
        assert!(!stencil.is_null());
        assert_ne!(depth, stencil);

        let pass = object::make(
            c"MTLRenderPassDescriptor",
            encoder::RenderPassDescriptor::default(),
        );
        let a = element_at(send(pass, "colorAttachments"), 0);
        send_with_object(a, "setTexture:", color);
        send_number(a, "setLoadAction:", 2);
        send_number(a, "setStoreAction:", 2);
        send_with_object(a, "setResolveTexture:", resolved);
        object::body::<encoder::RenderPassDescriptor>(pass).colors[0].clear_color =
            [1.0, 0.0, 0.0, 1.0];
        for (selector, texture) in [("depthAttachment", depth), ("stencilAttachment", stencil)] {
            let a = send(pass, selector);
            send_with_object(a, "setTexture:", texture);
            send_number(a, "setLoadAction:", 2);
            send_number(a, "setStoreAction:", 1);
        }
        let command = send(queue, "commandBuffer");
        send_void(render_encoder(command, pass), "endEncoding");
        let blit = send(command, "blitCommandEncoder");
        copy_texture_to_buffer(blit, resolved, readback, SIZE);
        send_void(blit, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        assert_eq!(send_number_result(command, "status"), 4);
        let pixel = object::body::<resource::Buffer>(readback)
            .inner
            .contents
            .cast::<[u8; 4]>()
            .read();
        assert_eq!(pixel, [0, 0, 255, 255]);
    }
}
