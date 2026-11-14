//! Manual end-to-end smoke test for native Metal on the SDL/Cocoa video thread.
//! Run on macOS with ROSEMARY_METAL_BACKEND=native. Does not launch Roblox.
//! The program creates a throwaway Rosemary guest NSWindow and presents a
//! solid-color native Metal drawable into its actual SDL-owned host window.

#[cfg(target_os = "macos")]
fn main() {
    use foundation::geometry::{NSPoint, NSRect, NSSize};
    use objc::object::Id;
    use objc::sel::Sel;
    use std::ptr;

    assert_eq!(
        std::env::var("ROSEMARY_METAL_BACKEND").as_deref(),
        Ok("native")
    );
    metal::bootstrap();
    let pool = objc::arc::pool_push();

    let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_id_arg: unsafe extern "C" fn(Id, Sel, Id) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_id_rect: unsafe extern "C" fn(Id, Sel, NSRect) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_window_init: unsafe extern "C" fn(Id, Sel, NSRect, usize, usize, u8) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_void: unsafe extern "C" fn(Id, Sel) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_void_id: unsafe extern "C" fn(Id, Sel, Id) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_void_size: unsafe extern "C" fn(Id, Sel, NSSize) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_void_u64: unsafe extern "C" fn(Id, Sel, u64) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ClearColor {
        red: f64,
        green: f64,
        blue: f64,
        alpha: f64,
    }
    let send_clear: unsafe extern "C" fn(Id, Sel, ClearColor) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_index: unsafe extern "C" fn(Id, Sel, u64) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_library: unsafe extern "C" fn(Id, Sel, Id, Id, *mut Id) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_pipeline: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_draw: unsafe extern "C" fn(Id, Sel, u64, u64, u64) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Viewport {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        znear: f64,
        zfar: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Scissor {
        x: u64,
        y: u64,
        width: u64,
        height: u64,
    }
    let send_viewport: unsafe extern "C" fn(Id, Sel, Viewport) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_scissor: unsafe extern "C" fn(Id, Sel, Scissor) =
        unsafe { std::mem::transmute(objc::msg::entry()) };

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MtlOrigin {
        x: u64,
        y: u64,
        z: u64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MtlSize {
        width: u64,
        height: u64,
        depth: u64,
    }
    let send_texture_readback: unsafe extern "C" fn(
        Id,
        Sel,
        Id,
        u64,
        u64,
        MtlOrigin,
        MtlSize,
        Id,
        u64,
        u64,
        u64,
    ) = unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_new_buffer: unsafe extern "C" fn(Id, Sel, u64, u64) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_contents: unsafe extern "C" fn(Id, Sel) -> *const u8 =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let send_void_bool: unsafe extern "C" fn(Id, Sel, u8) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let rect = NSRect {
        origin: NSPoint { x: 80.0, y: 80.0 },
        size: NSSize {
            width: 320.0,
            height: 240.0,
        },
    };

    let window_cls = objc::class::lookup(c"NSWindow").expect("guest NSWindow class");
    let window = objc::object::create_instance(window_cls, appkit::window::INSTANCE_SIZE);
    let window = unsafe {
        send_window_init(
            window,
            objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
            rect,
            0,
            0,
            0,
        )
    };
    assert!(!window.is_null(), "guest NSWindow failed to initialize");
    let sdl = unsafe { appkit::window::sdl_window(window) };
    assert!(
        !sdl.is_null(),
        "guest NSWindow failed to create an SDL window"
    );
    let window_id = sdl3::video::window_id(sdl);
    println!("SDL window created: {window_id}");

    let view_cls = objc::class::lookup(c"NSView").expect("guest NSView class");
    let view = objc::object::create_instance(view_cls, 0);
    let view_frame = NSRect {
        origin: NSPoint { x: 0.0, y: 0.0 },
        size: rect.size,
    };
    let view = unsafe { send_id_rect(view, objc::sel::get("initWithFrame:"), view_frame) };
    assert!(!view.is_null());
    unsafe { send_void_id(window, objc::sel::get("setContentView:"), view) };
    let layer_cls = objc::class::lookup(c"CAMetalLayer").expect("guest CAMetalLayer class");
    let layer = unsafe { send_id(objc::class::ptr(layer_cls) as Id, objc::sel::get("layer")) };
    assert!(!layer.is_null());
    unsafe { send_void_id(view, objc::sel::get("setLayer:"), layer) };
    let device = unsafe { metal::MTLCreateSystemDefaultDevice() };
    assert!(!device.is_null());
    unsafe {
        send_void_id(layer, objc::sel::get("setDevice:"), device);
        // A native drawable with framebufferOnly=false can be copied by a
        // blit encoder into CPU-visible shared memory for pixel assertions.
        send_void_bool(layer, objc::sel::get("setFramebufferOnly:"), 0);
        send_void_size(
            layer,
            objc::sel::get("setDrawableSize:"),
            NSSize {
                width: 320.0,
                height: 240.0,
            },
        );
    }
    // One single initial drawable must already come from SDL's attached
    // CAMetalLayer, without a discarded offscreen warm-up drawable or pump.
    let drawable = unsafe { send_id(layer, objc::sel::get("nextDrawable")) };
    assert!(!drawable.is_null(), "first on-screen drawable unavailable");
    assert!(
        sdl3::video::window_flags(sdl) & sdl3::video::flags::METAL != 0,
        "first drawable did not synchronously attach the SDL Metal view"
    );
    println!("First drawable already attached to SDL Metal window");

    let texture = unsafe { send_id(drawable, objc::sel::get("texture")) };
    assert!(!texture.is_null());
    let width: unsafe extern "C" fn(Id, Sel) -> u64 =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    assert_eq!(unsafe { width(texture, objc::sel::get("width")) }, 320);
    assert_eq!(unsafe { width(texture, objc::sel::get("height")) }, 240);

    let pass_cls = objc::class::lookup(c"MTLRenderPassDescriptor")
        .expect("guest MTLRenderPassDescriptor class");
    let pass = unsafe {
        send_id(
            objc::class::ptr(pass_cls) as Id,
            objc::sel::get("renderPassDescriptor"),
        )
    };
    assert!(!pass.is_null());
    let attachments = unsafe { send_id(pass, objc::sel::get("colorAttachments")) };
    assert!(!attachments.is_null());
    let color = unsafe { send_index(attachments, objc::sel::get("objectAtIndexedSubscript:"), 0) };
    assert!(!color.is_null());
    unsafe {
        send_void_id(color, objc::sel::get("setTexture:"), texture);
        send_void_u64(color, objc::sel::get("setLoadAction:"), 2); // MTLLoadActionClear
        send_void_u64(color, objc::sel::get("setStoreAction:"), 1); // MTLStoreActionStore
        send_clear(
            color,
            objc::sel::get("setClearColor:"),
            ClearColor {
                red: 0.09,
                green: 0.75,
                blue: 0.36,
                alpha: 1.0,
            },
        );
    }
    // Compile and bind a native Metal vertex/fragment pipeline, but use
    // guest-facing NSStrings, descriptors, and objc_msgSend for every step.
    let source = foundation::string::from_str(
        "#include <metal_stdlib>\nusing namespace metal;\n\
         struct VertexOut { float4 position [[position]]; };\n\
         vertex VertexOut passthrough_vertex(uint i [[vertex_id]]) {\n\
             float2 positions[3] = { float2(-0.75,-0.75), float2(0.75,-0.75), float2(0.0,0.75) };\n\
             VertexOut output; output.position = float4(positions[i],0.0,1.0); return output;\n\
         }\n\
         fragment float4 constant_fragment() { return float4(0.95, 0.10, 0.65, 1.0); }",
    );
    let vertex_name = foundation::string::from_str("passthrough_vertex");
    let fragment_name = foundation::string::from_str("constant_fragment");
    let mut error = ptr::null_mut();
    let library = unsafe {
        send_library(
            device,
            objc::sel::get("newLibraryWithSource:options:error:"),
            source,
            ptr::null_mut(),
            &mut error,
        )
    };
    assert!(
        !library.is_null(),
        "guest library compilation failed: {error:p}"
    );
    let vertex =
        unsafe { send_id_arg(library, objc::sel::get("newFunctionWithName:"), vertex_name) };
    let fragment = unsafe {
        send_id_arg(
            library,
            objc::sel::get("newFunctionWithName:"),
            fragment_name,
        )
    };
    assert!(!vertex.is_null() && !fragment.is_null());
    let pipeline_cls = objc::class::lookup(c"MTLRenderPipelineDescriptor")
        .expect("guest MTLRenderPipelineDescriptor class");
    let descriptor =
        unsafe { send_id(objc::class::ptr(pipeline_cls) as Id, objc::sel::get("new")) };
    assert!(!descriptor.is_null());
    unsafe {
        send_void_id(descriptor, objc::sel::get("setVertexFunction:"), vertex);
        send_void_id(descriptor, objc::sel::get("setFragmentFunction:"), fragment);
    }
    let colors = unsafe { send_id(descriptor, objc::sel::get("colorAttachments")) };
    let pipeline_color =
        unsafe { send_index(colors, objc::sel::get("objectAtIndexedSubscript:"), 0) };
    assert!(!pipeline_color.is_null());
    unsafe { send_void_u64(pipeline_color, objc::sel::get("setPixelFormat:"), 80) }; // BGRA8Unorm
    let pipeline = unsafe {
        send_pipeline(
            device,
            objc::sel::get("newRenderPipelineStateWithDescriptor:error:"),
            descriptor,
            &mut error,
        )
    };
    assert!(
        !pipeline.is_null(),
        "guest render pipeline compilation failed: {error:p}"
    );
    println!("Native MSL render pipeline created through guest Objective-C descriptors");

    const TARGET_WIDTH: usize = 320;
    const TARGET_HEIGHT: usize = 240;
    let row_bytes = TARGET_WIDTH * 4;
    let output_bytes = row_bytes * TARGET_HEIGHT;
    let readback = unsafe {
        send_new_buffer(
            device,
            objc::sel::get("newBufferWithLength:options:"),
            output_bytes as u64,
            0,
        )
    };
    assert!(
        !readback.is_null(),
        "could not allocate shared native Metal readback buffer"
    );
    let queue = unsafe { send_id(device, objc::sel::get("newCommandQueue")) };
    let command = unsafe { send_id(queue, objc::sel::get("commandBuffer")) };
    let encoder = unsafe {
        send_id_arg(
            command,
            objc::sel::get("renderCommandEncoderWithDescriptor:"),
            pass,
        )
    };
    assert!(!encoder.is_null());
    unsafe {
        send_void_id(encoder, objc::sel::get("setRenderPipelineState:"), pipeline);
        send_viewport(
            encoder,
            objc::sel::get("setViewport:"),
            Viewport {
                x: 0.0,
                y: 0.0,
                width: 320.0,
                height: 240.0,
                znear: 0.0,
                zfar: 1.0,
            },
        );
        send_scissor(
            encoder,
            objc::sel::get("setScissorRect:"),
            Scissor {
                x: 142,
                y: 95,
                width: 36,
                height: 50,
            },
        );
        send_draw(
            encoder,
            objc::sel::get("drawPrimitives:vertexStart:vertexCount:"),
            3,
            0,
            3,
        );
        send_void(encoder, objc::sel::get("endEncoding"));
        let blit = send_id(command, objc::sel::get("blitCommandEncoder"));
        assert!(!blit.is_null());
        send_texture_readback(
            blit,
            objc::sel::get(
                "copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:",
            ),
            texture,
            0,
            0,
            MtlOrigin { x: 0, y: 0, z: 0 },
            MtlSize {
                width: TARGET_WIDTH as u64,
                height: TARGET_HEIGHT as u64,
                depth: 1,
            },
            readback,
            0,
            row_bytes as u64,
            output_bytes as u64,
        );
        send_void(blit, objc::sel::get("endEncoding"));
        send_void_id(command, objc::sel::get("presentDrawable:"), drawable);
        send_void(command, objc::sel::get("commit"));
        send_void(command, objc::sel::get("waitUntilCompleted"));
    }
    let pixels = unsafe { send_contents(readback, objc::sel::get("contents")) };
    assert!(!pixels.is_null());
    let read_pixel = |x: usize, y: usize| {
        let address = (y * TARGET_WIDTH + x) * 4;
        let bytes = unsafe { std::slice::from_raw_parts(pixels.add(address), 4) };
        [bytes[0], bytes[1], bytes[2], bytes[3]]
    };
    let corner = read_pixel(8, 8);
    let center = read_pixel(TARGET_WIDTH / 2, TARGET_HEIGHT / 2);
    let clipped = read_pixel(125, TARGET_HEIGHT / 2);
    println!("Native drawable GPU readback: corner BGRA={corner:?} center BGRA={center:?}");
    assert!(
        (75..110).contains(&corner[0])
            && (170..215).contains(&corner[1])
            && (10..38).contains(&corner[2])
            && corner[3] == 255,
        "background pixels do not match requested clear color"
    );
    assert!(
        (150..190).contains(&center[0])
            && (10..42).contains(&center[1])
            && (220..255).contains(&center[2])
            && center[3] == 255,
        "triangle pixels do not match native Metal fragment shader"
    );
    assert!(
        (75..110).contains(&clipped[0])
            && (170..215).contains(&clipped[1])
            && (10..38).contains(&clipped[2])
            && clipped[3] == 255,
        "scissor failed to clip triangle fragment at x=125, y=120: {clipped:?}"
    );
    println!("Native Metal viewport and scissor clipped triangle correctly: BGRA={clipped:?}");
    println!("Native Metal render pass presented to SDL window with pixel verification");
    if let Ok(path) = std::env::var("ROSEMARY_METAL_SMOKE_SCREENSHOT") {
        appkit::event::pump(ptr::null_mut());
        std::thread::sleep(std::time::Duration::from_millis(250));
        match std::process::Command::new("/usr/sbin/screencapture")
            .args(["-x", &path])
            .status()
        {
            Ok(result) => println!("Screen capture: {} ({result})", path),
            Err(reason) => eprintln!("Screen capture failed: {reason}"),
        }
    }
    for _ in 0..90 {
        appkit::event::pump(ptr::null_mut());
        std::thread::sleep(std::time::Duration::from_millis(33));
    }
    // Retain the original live CAMetalLayer *beyond* setLayer:B. Its still
    // valid identity must not occupy the window after its view is detached.
    objc::arc::retain(layer);
    assert_eq!(sdl3::video::window_id(sdl), window_id);
    let next_pool = objc::arc::pool_push();
    let new_layer = unsafe { send_id(objc::class::ptr(layer_cls) as Id, objc::sel::get("layer")) };
    assert!(!new_layer.is_null());
    unsafe {
        send_void_id(view, objc::sel::get("setLayer:"), new_layer);
        send_void_id(new_layer, objc::sel::get("setDevice:"), device);
        send_void_size(
            new_layer,
            objc::sel::get("setDrawableSize:"),
            NSSize {
                width: 320.0,
                height: 240.0,
            },
        );
    }
    let replacement = unsafe { send_id(new_layer, objc::sel::get("nextDrawable")) };
    assert!(!replacement.is_null());
    // Layer A remains alive: this is not a deallocation-based cleanup test.
    assert!(!layer.is_null());
    assert!(
        sdl3::video::window_flags(sdl) & sdl3::video::flags::METAL != 0,
        "replacement CAMetalLayer failed to acquire SDL Metal view"
    );
    println!("Replacement guest CAMetalLayer attached while old layer remains retained");
    unsafe { send_void_id(view, objc::sel::get("setLayer:"), ptr::null_mut()) };

    // Removing a retained child NSView must retire its SDL Metal view without
    // changing the child's CAMetalLayer back-reference or deallocating either.
    let detached = unsafe {
        send_id_rect(
            objc::object::create_instance(view_cls, 0),
            objc::sel::get("initWithFrame:"),
            view_frame,
        )
    };
    let detached_layer =
        unsafe { send_id(objc::class::ptr(layer_cls) as Id, objc::sel::get("layer")) };
    assert!(!detached.is_null() && !detached_layer.is_null());
    unsafe {
        send_void_id(detached, objc::sel::get("setLayer:"), detached_layer);
        send_void_id(view, objc::sel::get("addSubview:"), detached);
        send_void_id(detached_layer, objc::sel::get("setDevice:"), device);
        send_void_size(
            detached_layer,
            objc::sel::get("setDrawableSize:"),
            view_frame.size,
        );
    }
    assert!(
        !unsafe { send_id(detached_layer, objc::sel::get("nextDrawable")) }.is_null(),
        "detachable view did not obtain its first drawable",
    );
    objc::arc::retain(detached);
    unsafe { send_void(detached, objc::sel::get("removeFromSuperview")) };
    appkit::event::pump(ptr::null_mut());
    assert_eq!(
        unsafe { send_id(detached, objc::sel::get("layer")) },
        detached_layer,
        "detached NSView did not retain its CAMetalLayer",
    );
    assert_eq!(sdl3::video::window_id(sdl), window_id);
    assert!(
        !unsafe { send_id(detached_layer, objc::sel::get("nextDrawable")) }.is_null(),
        "detached CAMetalLayer should still provide offscreen drawables",
    );
    // SDL_WINDOW_METAL can remain set after SDL_Metal_DestroyView; the
    // replacement's first drawable is the actual ownership check.
    let successor = unsafe {
        send_id_rect(
            objc::object::create_instance(view_cls, 0),
            objc::sel::get("initWithFrame:"),
            view_frame,
        )
    };
    let successor_layer =
        unsafe { send_id(objc::class::ptr(layer_cls) as Id, objc::sel::get("layer")) };
    assert!(!successor.is_null() && !successor_layer.is_null());
    unsafe {
        send_void_id(successor, objc::sel::get("setLayer:"), successor_layer);
        send_void_id(view, objc::sel::get("addSubview:"), successor);
        send_void_id(successor_layer, objc::sel::get("setDevice:"), device);
        send_void_size(
            successor_layer,
            objc::sel::get("setDrawableSize:"),
            view_frame.size,
        );
    }
    assert!(
        !unsafe { send_id(successor_layer, objc::sel::get("nextDrawable")) }.is_null(),
        "replacement view failed to acquire SDL Metal view after removal",
    );
    println!("Retained detached NSView released its SDL Metal slot for a new view");
    unsafe {
        send_void_id(detached, objc::sel::get("setLayer:"), ptr::null_mut());
        send_void_id(successor, objc::sel::get("setLayer:"), ptr::null_mut());
    }
    objc::arc::release(detached);
    objc::arc::release(detached);
    objc::arc::release(successor);
    objc::arc::release(layer);
    objc::arc::pool_pop(next_pool);
    objc::arc::pool_pop(pool);
    appkit::event::pump(ptr::null_mut());
    println!("Native Metal SDL window smoke test and layer replacement complete");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Native Metal SDL window test requires macOS");
}
