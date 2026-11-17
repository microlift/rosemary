use super::*;

#[test]
fn single_element_viewport_and_scissor_arrays_clip_the_rendered_pixels() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const SIZE: u64 = 8;
    let descriptor = triangle_pipeline_descriptor(device);
    let mut error = std::ptr::null_mut();
    // SAFETY: test-owned pipeline, attachment, buffers and registered methods.
    unsafe {
        let pipeline = new_render_pipeline(device, std::ptr::null(), descriptor, &mut error);
        assert!(!pipeline.is_null() && error.is_null());
        let target = resource::new_texture(
            gpu_of(device).unwrap(),
            resource::TextureDescriptor {
                texture_type: 2,
                pixel_format: 80,
                width: SIZE,
                height: SIZE,
                depth: 1,
                mipmap_level_count: 1,
                sample_count: 1,
                array_length: 1,
                storage_mode: 2,
                usage: gpu::USAGE_RENDER_TARGET,
            },
        );
        let vertices = new_buffer(device, std::ptr::null(), 48, 0);
        let readback = new_buffer(device, std::ptr::null(), (SIZE * SIZE * 4) as usize, 0);
        let queue = new_command_queue(device, std::ptr::null());
        assert!(
            !target.is_null() && !vertices.is_null() && !readback.is_null() && !queue.is_null()
        );
        let data = [
            -1.0f32, -1.0, 0.0, 1.0, 3.0, -1.0, 0.0, 1.0, -1.0, 3.0, 0.0, 1.0,
        ];
        std::ptr::copy_nonoverlapping(
            data.as_ptr(),
            object::body::<resource::Buffer>(vertices)
                .inner
                .contents
                .cast::<f32>(),
            data.len(),
        );
        let pass = object::make(
            c"MTLRenderPassDescriptor",
            encoder::RenderPassDescriptor::default(),
        );
        let attachment = element_at(send(pass, "colorAttachments"), 0);
        send_with_object(attachment, "setTexture:", target);
        send_number(attachment, "setLoadAction:", 2);
        send_number(attachment, "setStoreAction:", 1);
        let command = send(queue, "commandBuffer");
        let render = render_encoder(command, pass);
        send_with_object(render, "setRenderPipelineState:", pipeline);
        set_buffer_named(render, "setVertexBuffer:offset:atIndex:", vertices, 0, 0);
        let viewport = encoder::Viewport {
            origin_x: 2.0,
            origin_y: 1.0,
            width: 3.0,
            height: 4.0,
            znear: 0.0,
            zfar: 1.0,
        };
        let scissor = encoder::ScissorRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        let (set_viewports, sel) = method::<
            unsafe extern "C" fn(Id, Sel, *const encoder::Viewport, u64),
        >(render, "setViewports:count:");
        set_viewports(render, sel, &viewport, 1);
        let (set_scissors, sel) = method::<
            unsafe extern "C" fn(Id, Sel, *const encoder::ScissorRect, u64),
        >(render, "setScissorRects:count:");
        set_scissors(render, sel, &scissor, 1);
        draw(render, 3);
        send_void(render, "endEncoding");
        let blit = send(command, "blitCommandEncoder");
        copy_texture_to_buffer(blit, target, readback, SIZE);
        send_void(blit, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        let pixels = std::slice::from_raw_parts(
            object::body::<resource::Buffer>(readback).inner.contents,
            (SIZE * SIZE * 4) as usize,
        );
        for y in 0..SIZE {
            for x in 0..SIZE {
                let offset = ((y * SIZE + x) * 4) as usize;
                let expected = if (2..4).contains(&x) && (2..5).contains(&y) {
                    [0, 0, 255, 255]
                } else {
                    [0, 0, 0, 255]
                };
                assert_eq!(&pixels[offset..offset + 4], expected, "pixel ({x}, {y})");
            }
        }
        for value in [
            pipeline, descriptor, target, vertices, readback, queue, pass,
        ] {
            objc::arc::release(value);
        }
    }
}

fn render_indexed_viewports(device: Id, encode: impl FnOnce(Id)) -> Vec<u8> {
    let source = "struct Out { float4 position [[position]]; uint viewport [[viewport_array_index]]; };
        vertex Out indexed(const device float4 *positions [[buffer(0)]], uint vertex [[vertex_id]], uint instance [[instance_id]]) {
            Out result; result.position = positions[vertex]; result.viewport = instance; return result;
        }
        fragment float4 red(Out input [[stage_in]]) { float blue = float(input.viewport == 1u); return float4(1.0 - blue, 0.0, blue, 1.0); }";
    let mut error = std::ptr::null_mut();
    // SAFETY: test-owned objects and registered selectors with their declared ABI.
    unsafe {
        let library =
            library::library_from_source(device, Some(source.as_bytes()), &[], &mut error);
        assert!(!library.is_null() && error.is_null());
        let vertex = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("indexed"),
        );
        let fragment = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("red"),
        );
        assert!(!vertex.is_null() && !fragment.is_null());
        let descriptor = send(
            objc::class::ptr(objc::class::lookup(c"MTLRenderPipelineDescriptor").unwrap()) as Id,
            "new",
        );
        send_with_object(descriptor, "setVertexFunction:", vertex);
        send_with_object(descriptor, "setFragmentFunction:", fragment);
        send_number(
            element_at(send(descriptor, "colorAttachments"), 0),
            "setPixelFormat:",
            BGRA8,
        );
        let pipeline = new_render_pipeline(device, std::ptr::null(), descriptor, &mut error);
        assert!(!pipeline.is_null() && error.is_null());
        let target = resource::new_texture(
            gpu_of(device).unwrap(),
            resource::TextureDescriptor {
                texture_type: 2,
                pixel_format: BGRA8,
                width: 8,
                height: 8,
                depth: 1,
                mipmap_level_count: 1,
                sample_count: 1,
                array_length: 1,
                storage_mode: 2,
                usage: gpu::USAGE_RENDER_TARGET,
            },
        );
        let vertices = new_buffer(device, std::ptr::null(), 48, 0);
        let readback = new_buffer(device, std::ptr::null(), 256, 0);
        let queue = new_command_queue(device, std::ptr::null());
        assert!(
            !target.is_null() && !vertices.is_null() && !readback.is_null() && !queue.is_null()
        );
        let data = [
            -1.0f32, -1.0, 0.0, 1.0, 3.0, -1.0, 0.0, 1.0, -1.0, 3.0, 0.0, 1.0,
        ];
        std::ptr::copy_nonoverlapping(
            data.as_ptr(),
            object::body::<resource::Buffer>(vertices)
                .inner
                .contents
                .cast::<f32>(),
            data.len(),
        );
        let pass = object::make(
            c"MTLRenderPassDescriptor",
            encoder::RenderPassDescriptor::default(),
        );
        let attachment = element_at(send(pass, "colorAttachments"), 0);
        send_with_object(attachment, "setTexture:", target);
        send_number(attachment, "setLoadAction:", 2);
        send_number(attachment, "setStoreAction:", 1);
        let command = send(queue, "commandBuffer");
        let render = render_encoder(command, pass);
        send_with_object(render, "setRenderPipelineState:", pipeline);
        set_buffer_named(render, "setVertexBuffer:offset:atIndex:", vertices, 0, 0);
        encode(render);
        send_void(render, "endEncoding");
        let blit = send(command, "blitCommandEncoder");
        copy_texture_to_buffer(blit, target, readback, 8);
        send_void(blit, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        let pixels = std::slice::from_raw_parts(
            object::body::<resource::Buffer>(readback).inner.contents,
            256,
        )
        .to_vec();
        for value in [
            pipeline, descriptor, target, vertices, readback, queue, pass, vertex, fragment,
            library,
        ] {
            objc::arc::release(value);
        }
        pixels
    }
}

fn viewport(x: f64, y: f64, width: f64, height: f64) -> encoder::Viewport {
    encoder::Viewport {
        origin_x: x,
        origin_y: y,
        width,
        height,
        znear: 0.0,
        zfar: 1.0,
    }
}

fn assert_viewport_pixels(
    pixels: &[u8],
    red: impl Fn(u64, u64) -> bool,
    blue: impl Fn(u64, u64) -> bool,
) {
    for y in 0..8 {
        for x in 0..8 {
            let offset = ((y * 8 + x) * 4) as usize;
            let expected = if blue(x, y) {
                [255, 0, 0, 255]
            } else if red(x, y) {
                [0, 0, 255, 255]
            } else {
                [0, 0, 0, 255]
            };
            assert_eq!(&pixels[offset..offset + 4], expected, "pixel ({x}, {y})");
        }
    }
}

fn draw_instances(render: Id, instances: u64) {
    // SAFETY: the test encoder implements this selector with the declared ABI.
    let (draw, sel) = unsafe {
        method::<unsafe extern "C" fn(Id, Sel, u64, u64, u64, u64)>(
            render,
            "drawPrimitives:vertexStart:vertexCount:instanceCount:",
        )
    };
    // SAFETY: registered draw method on this test's encoder.
    unsafe {
        draw(render, sel, 3, 0, 3, instances);
    }
}

#[test]
fn viewport_indices_select_matching_scissors_in_either_setter_order() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    for scissors_first in [true, false] {
        let pixels = render_indexed_viewports(device, |render| {
            let viewports = [viewport(0.0, 0.0, 4.0, 8.0), viewport(4.0, 0.0, 4.0, 8.0)];
            let scissors = [
                encoder::ScissorRect {
                    x: 0,
                    y: 1,
                    width: 3,
                    height: 2,
                },
                encoder::ScissorRect {
                    x: 5,
                    y: 4,
                    width: 3,
                    height: 3,
                },
            ];
            // SAFETY: the test encoder implements this selector with the declared ABI.
            let (set_viewports, vsel) = unsafe {
                method::<unsafe extern "C" fn(Id, Sel, *const encoder::Viewport, u64)>(
                    render,
                    "setViewports:count:",
                )
            };
            // SAFETY: the test encoder implements this selector with the declared ABI.
            let (set_scissors, ssel) = unsafe {
                method::<unsafe extern "C" fn(Id, Sel, *const encoder::ScissorRect, u64)>(
                    render,
                    "setScissorRects:count:",
                )
            };
            // SAFETY: the arrays remain readable for both setters.
            unsafe {
                if scissors_first {
                    set_scissors(render, ssel, scissors.as_ptr(), 2);
                }
                set_viewports(render, vsel, viewports.as_ptr(), 2);
                if !scissors_first {
                    set_scissors(render, ssel, scissors.as_ptr(), 2);
                }
            }
            draw_instances(render, 2);
        });
        assert_viewport_pixels(
            &pixels,
            |x, y| x < 3 && (1..3).contains(&y),
            |x, y| x >= 5 && (4..7).contains(&y),
        );
    }
}

#[test]
fn single_scissor_broadcasts_and_single_viewport_replaces_the_array() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let pixels = render_indexed_viewports(device, |render| {
        let viewports = [viewport(0.0, 0.0, 3.0, 8.0), viewport(5.0, 0.0, 3.0, 8.0)];
        // SAFETY: the test encoder implements this selector with the declared ABI.
        let (set_viewports, vsel) = unsafe {
            method::<unsafe extern "C" fn(Id, Sel, *const encoder::Viewport, u64)>(
                render,
                "setViewports:count:",
            )
        };
        // SAFETY: the test encoder implements this selector with the declared ABI.
        let (set_scissor, ssel) = unsafe {
            method::<unsafe extern "C" fn(Id, Sel, encoder::ScissorRect)>(render, "setScissorRect:")
        };
        // SAFETY: the test encoder implements this selector with the declared ABI.
        let (set_viewport, single) = unsafe {
            method::<unsafe extern "C" fn(Id, Sel, encoder::Viewport)>(render, "setViewport:")
        };
        // SAFETY: test-owned encoder and copied values.
        unsafe {
            set_scissor(
                render,
                ssel,
                encoder::ScissorRect {
                    x: 0,
                    y: 2,
                    width: 8,
                    height: 3,
                },
            );
            set_viewports(render, vsel, viewports.as_ptr(), 2);
        }
        draw_instances(render, 2);
        unsafe {
            set_viewport(render, single, viewport(3.0, 6.0, 2.0, 2.0));
            set_scissor(
                render,
                ssel,
                encoder::ScissorRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
            );
        }
        draw_instances(render, 1);
    });
    assert_viewport_pixels(
        &pixels,
        |x, y| (x < 3 && (2..5).contains(&y)) || ((3..5).contains(&x) && y >= 6),
        |x, y| x >= 5 && (2..5).contains(&y),
    );
}

#[test]
fn a_single_scissor_rect_array_applies_to_all_viewports() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let pixels = render_indexed_viewports(device, |render| {
        let viewports = [viewport(0.0, 0.0, 4.0, 8.0), viewport(4.0, 0.0, 4.0, 8.0)];
        let rectangle = encoder::ScissorRect {
            x: 1,
            y: 2,
            width: 6,
            height: 3,
        };
        // SAFETY: test-owned encoder and arrays matching the selectors' ABI.
        unsafe {
            let (set_viewports, vsel) = method::<
                unsafe extern "C" fn(Id, Sel, *const encoder::Viewport, u64),
            >(render, "setViewports:count:");
            let (set_scissors, ssel) = method::<
                unsafe extern "C" fn(Id, Sel, *const encoder::ScissorRect, u64),
            >(render, "setScissorRects:count:");
            set_viewports(render, vsel, viewports.as_ptr(), 2);
            set_scissors(render, ssel, &rectangle, 1);
        }
        draw_instances(render, 2);
    });
    assert_viewport_pixels(
        &pixels,
        |x, y| (1..4).contains(&x) && (2..5).contains(&y),
        |x, y| (4..7).contains(&x) && (2..5).contains(&y),
    );
}

#[test]
fn device_limit_viewports_share_the_default_scissor() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let count = gpu_of(device).unwrap().max_viewports.min(16);
    let pixels = render_indexed_viewports(device, |render| {
        let viewports = (0..count)
            .map(|index| viewport(f64::from(index % 4 * 2), f64::from(index / 4 * 2), 1.0, 2.0))
            .collect::<Vec<_>>();
        // SAFETY: a live encoder and count copied viewport structures.
        unsafe {
            let (set, sel) = method::<unsafe extern "C" fn(Id, Sel, *const encoder::Viewport, u64)>(
                render,
                "setViewports:count:",
            );
            set(render, sel, viewports.as_ptr(), u64::from(count));
        }
        draw_instances(render, u64::from(count));
    });
    assert_viewport_pixels(
        &pixels,
        |x, y| x % 2 == 0 && y / 2 * 4 + x / 2 < u64::from(count) && !(x == 2 && y < 2),
        |x, y| count > 1 && x == 2 && y < 2,
    );
}
