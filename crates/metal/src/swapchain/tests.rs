use super::*;

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn test_opaque_metal_placement(id: usize, frame: DirectFrame) -> ScenePlacement {
    ScenePlacement {
        opacity_groups: Vec::new(),
        id,
        frame,
        opacity: 1.0,
        opaque: true,
        clip: None,
        rounded: Vec::new(),
        occluded: Some(Vec::new()),
        native: None,
    }
}

#[test]
#[cfg(not(target_os = "macos"))]
fn many_sibling_nested_opacity_groups_plan_with_indexed_membership() {
    if isolate_vulkan_graphical_test(
        "many_sibling_nested_opacity_groups_plan_with_indexed_membership",
    ) {
        return;
    }
    let Some((window, gpu, mut state, image, full)) = v10_fifo_fixture() else {
        return;
    };
    let mut timings = Vec::new();
    for groups in [128usize, 512, 2048] {
        let mut placements = Vec::with_capacity(groups * 2);
        for g in 0..groups {
            for child in 0..2 {
                let mut source = test_opaque_metal_placement(
                    0x10_0000 + g * 2 + child,
                    DirectFrame::from_frame(&full),
                );
                source.opacity_groups = vec![
                    appkit::view::OpacityGroup {
                        id: 0x20000 + g,
                        opacity: 0.8,
                    },
                    appkit::view::OpacityGroup {
                        id: 0x40000 + g,
                        opacity: 0.5,
                    },
                ];
                placements.push(source);
            }
        }
        state.layers = placements.iter().map(|placement| placement.id).collect();
        state.scene_placements = placements;
        let mut fastest = std::time::Duration::MAX;
        for _ in 0..3 {
            let start = std::time::Instant::now();
            let normalized = state.normalized_opacity_paths();
            let regions = state.opacity_group_regions(&normalized);
            fastest = fastest.min(start.elapsed());
            assert_eq!(normalized.len(), groups * 2);
            assert_eq!(regions.len(), groups);
            for (_, path, inherited) in normalized.iter().take(4) {
                assert_eq!(path.len(), 1);
                assert!((path[0].opacity - 0.4).abs() < 1e-6);
                assert_eq!(*inherited, 1.0);
            }
        }
        timings.push(fastest.as_secs_f64() * 1e6);
        eprintln!(
            "indexed CA opacity planner: {groups} sibling groups, {} sources, {:.1}us ({:.3}us/group)",
            groups * 2,
            timings.last().unwrap(),
            timings.last().unwrap() / groups as f64
        );
    }
    // An order-of-magnitude input growth must not exhibit old all-pairs
    // group scanning. Be generous about noisy/shared CI hardware.
    assert!(
        timings[2] <= timings[0] * 80.0 + 1000.0,
        "quadratic group-planning regression: {timings:?}"
    );
    drop(state);
    gpu.destroy_image(image);
    sdl3::video::destroy_window(window);
}

fn test_full_frame(window: sdl3::Window, width: u32, height: u32) -> appkit::view::Frame {
    appkit::view::Frame {
        window,
        x: 0,
        y: 0,
        width,
        height,
        window_width: width,
        window_height: height,
    }
}

// All test colors are applied through an identical full-image color range.
// Keep submission and fence ownership inside each actual regression.
unsafe fn cmd_test_clear_color(
    device: &ash::Device,
    cb: vk::CommandBuffer,
    image: vk::Image,
    rgba: [f32; 4],
) {
    unsafe {
        device.cmd_clear_color_image(
            cb,
            image,
            vk::ImageLayout::GENERAL,
            &vk::ClearColorValue { float32: rgba },
            &[vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            }],
        );
    }
}

// Most graphical regressions use the same four-channel render target. Keep
// nonstandard formats and memory-usage cases explicit in their tests.
fn test_render_target(gpu: &'static Gpu, width: u32, height: u32) -> Result<GpuImage, String> {
    gpu.create_image(&ImageDescription {
        image_type: vk::ImageType::TYPE_2D,
        view_type: vk::ImageViewType::TYPE_2D,
        format: vk::Format::B8G8R8A8_UNORM,
        width,
        height,
        depth: 1,
        levels: 1,
        layers: 1,
        samples: 1,
        usage: gpu::USAGE_RENDER_TARGET,
        cube: false,
        components: vk::ComponentMapping::default(),
    })
}

#[test]
fn a_scene_retry_never_overwrites_a_more_recent_view_snapshot() {
    let frame = DirectFrame {
        window: 0,
        x: 0,
        y: 0,
        width: 1,
        height: 1,
        window_width: 1,
        window_height: 1,
    };
    let scene = |id| {
        vec![ScenePlacement {
            opacity_groups: Vec::new(),
            id,
            frame,
            opacity: 1.0,
            opaque: true,
            clip: None,
            rounded: Vec::new(),
            occluded: None,
            native: None,
        }]
    };
    let pending = Mutex::new(None);
    requeue_scene(&pending, scene(1));
    assert_eq!(pending.lock().unwrap().as_ref().unwrap()[0].id, 1);
    *pending.lock().unwrap() = Some(scene(2));
    requeue_scene(&pending, scene(1));
    assert_eq!(
        pending.lock().unwrap().as_ref().unwrap()[0].id,
        2,
        "a stale GPU retry replaced the latest AppKit visibility update"
    );
}

/// Vulkan graphical tests share SDL video ownership and a singleton GPU;
/// synthetic failures can poison them across Rust's serial test threads.
/// Give each scenario its own SDL/Vulkan process while leaving pure tests in
/// this process. Exact selection prevents recursive execution of the suite.
pub(crate) fn isolate_vulkan_graphical_test(name: &str) -> bool {
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none()
        || std::env::var_os("ROSEMARY_VULKAN_GRAPHICS_CHILD").is_some()
    {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .arg("--exact")
        .arg(if name.contains("::") {
            name.to_owned()
        } else {
            format!("swapchain::tests::{name}")
        })
        .arg("--nocapture")
        .env("ROSEMARY_VULKAN_GRAPHICS_CHILD", "1")
        .output()
        .expect("start isolated Vulkan graphical test");
    assert!(
        output.status.success(),
        "isolated {name} failed ({:?}):\n{}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "isolated {name} did not execute: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    true
}

/// Tests run without the background compositor worker, so they can prove several
/// independent layer copies are coalesced into one actual WSI presentation.
fn flush_if_composed(window: sdl3::Window) {
    let entry = SWAPCHAINS
        .lock()
        .expect("window swapchains")
        .as_ref()
        .and_then(|all| all.get(&(window as usize)).cloned())
        .expect("swapchain for test window");
    let _presenting = entry.presenting.lock().expect("window presentation");
    let mut swapchain = entry.state.lock().expect("window swapchain");
    if !uses_layer_compositor(swapchain.present_mode) {
        return;
    }
    assert!(
        swapchain.composite_dirty,
        "independent layer updates were not staged"
    );
    let before = swapchain.frame;
    swapchain
        .flush_composite(window)
        .expect("one coalesced window presentation");
    assert!(
        !swapchain.composite_dirty,
        "window presentation did not drain the accumulator"
    );
    assert_eq!(
        swapchain.frame,
        before + 1,
        "the flush submitted multiple WSI frames"
    );
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "SDL main thread required")]
fn vulkan_graphical_test_prerequisites_are_available() {
    if isolate_vulkan_graphical_test("vulkan_graphical_test_prerequisites_are_available") {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    assert!(sdl3::video::init_video(), "SDL video subsystem unavailable");
    assert!(
        sdl3::may_pump(),
        "Vulkan tests must run on SDL's video owner thread"
    );
    let adapters = crate::vulkan::adapters().expect("Vulkan adapters unavailable");
    let gpu = adapters
        .first()
        .expect("no Vulkan adapter")
        .gpu()
        .expect("Vulkan logical device unavailable");
    let window = sdl3::video::create_window(c"Vulkan test prerequisites", 80, 64, 0)
        .expect("SDL video window unavailable");
    sdl3::video::presentation_window(window)
        .expect("presentation metadata unavailable on SDL owner thread");
    let instance = crate::vulkan::instance_handle().expect("Vulkan instance unavailable");
    let surface = sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library())
        .expect("SDL Vulkan surface creation failed");
    assert!(
        gpu.can_present(vk::SurfaceKHR::from_raw(surface)),
        "the Vulkan device queue cannot present to this window"
    );
    assert!(
        sdl3::video::vulkan::owns_window(window),
        "SDL conversion not registered"
    );
    sdl3::video::vulkan::destroy_surface(instance, surface);
    sdl3::video::destroy_window(window);
}

#[test]
fn present_fences_are_not_awaited_after_unqueued_errors() {
    use vk::Result as R;
    for gamescope in [false, true] {
        for error in [
            R::ERROR_DEVICE_LOST,
            R::ERROR_OUT_OF_HOST_MEMORY,
            R::ERROR_OUT_OF_DEVICE_MEMORY,
        ] {
            assert!(!present_fence_will_signal(&Err(error), gamescope));
        }
        assert!(present_fence_will_signal(&Ok(false), gamescope));
        assert!(present_fence_will_signal(&Ok(true), gamescope));
    }
    // The specification queues semaphore/fence work for these results, but
    // Gamescope's alternate WSI path has a known missing-signal behavior.
    for error in [R::ERROR_OUT_OF_DATE_KHR, R::ERROR_SURFACE_LOST_KHR] {
        assert!(present_fence_will_signal(&Err(error), false));
        assert!(!present_fence_will_signal(&Err(error), true));
    }
}

#[test]
fn fifo_limits_wsi_images_without_changing_mailbox_capacity() {
    assert_eq!(desired_image_count(vk::PresentModeKHR::FIFO, 3, 8), 4);
    assert_eq!(desired_image_count(vk::PresentModeKHR::FIFO, 3, 0), 4);
    assert_eq!(desired_image_count(vk::PresentModeKHR::FIFO, 3, 3), 3);
    assert_eq!(desired_image_count(vk::PresentModeKHR::MAILBOX, 3, 8), 4);
    assert_eq!(desired_image_count(vk::PresentModeKHR::IMMEDIATE, 2, 0), 2);
    assert_eq!(desired_image_count(vk::PresentModeKHR::IMMEDIATE, 3, 3), 3);
    assert_eq!(desired_image_count(vk::PresentModeKHR::IMMEDIATE, 2, 2), 2);
    assert_eq!(
        desired_image_count(vk::PresentModeKHR::FIFO_RELAXED, 3, 8),
        4
    );
}

/// Two independent windows may perform immediate Vulkan recording without
/// holding up one another's command pool for the duration of a GPU wait.
/// Both command lists own different handles and retire independently.
#[test]
fn overlapping_immediate_gpu_recorders_have_independent_fence_owners() {
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(adapter) = adapters.first() else {
        return;
    };
    let Ok(gpu) = adapter.gpu() else {
        return;
    };
    let (first_started, started) = std::sync::mpsc::sync_channel(1);
    let (release_first, release) = std::sync::mpsc::sync_channel::<()>(1);
    let first = std::thread::spawn(move || {
        gpu.one_shot(|_, cb| {
            first_started
                .send(cb)
                .expect("notify competing immediate recorder");
            release.recv().expect("first command recorder released");
        })
    });
    let first_cb = started
        .recv_timeout(std::time::Duration::from_secs(4))
        .expect("first Vulkan command recorder never began");
    let (second_done, second_result) = std::sync::mpsc::sync_channel(1);
    let second = std::thread::spawn(move || {
        let mut handle = None;
        let result = gpu.one_shot(|_, cb| {
            handle = Some(cb);
        });
        second_done
            .send(result.map(|()| handle.expect("recorded Vulkan command buffer")))
            .expect("second recorder completed");
    });
    // A single global immediate pool lock would block here until the first
    // recorder is released. Do not leave a hung thread behind on failure.
    let concurrent = second_result.recv_timeout(std::time::Duration::from_secs(4));
    release_first.send(()).expect("release first recorder");
    assert!(first.join().unwrap().is_ok());
    second.join().unwrap();
    let second_cb = concurrent
        .expect("unrelated Vulkan recorder blocked by first recorder")
        .expect("second Vulkan submission failed");
    assert_ne!(
        first_cb, second_cb,
        "two simultaneous GPU immediate recordings aliased a live VkCommandBuffer"
    );
}

#[test]
fn all_present_modes_compose_layers_before_wsi() {
    for mode in [
        vk::PresentModeKHR::FIFO,
        vk::PresentModeKHR::FIFO_RELAXED,
        vk::PresentModeKHR::MAILBOX,
        vk::PresentModeKHR::IMMEDIATE,
    ] {
        assert!(uses_layer_compositor(mode));
        assert_eq!(frame_slots(mode), FRAMES_IN_FLIGHT);
    }
}

#[test]
fn layer_compositing_batch_delay_distinguishes_wsi_modes() {
    for rate in [30.0, 60.0, 165.0, 240.0] {
        let immediate = compositor_batch_delay(vk::PresentModeKHR::IMMEDIATE, rate);
        let mailbox = compositor_batch_delay(vk::PresentModeKHR::MAILBOX, rate);
        let fifo = compositor_batch_delay(vk::PresentModeKHR::FIFO, rate);
        assert_eq!(mailbox, immediate);
        assert_eq!(
            compositor_batch_delay(vk::PresentModeKHR::FIFO_RELAXED, rate),
            fifo
        );
        assert!(immediate < fifo);
        assert!(immediate <= Duration::from_millis(2));
        assert!(fifo <= Duration::from_millis(4));
    }
}

#[test]
fn single_layer_fast_path_requires_an_owned_opaque_full_window() {
    let frame = test_full_frame(std::ptr::null_mut(), 320, 240);
    for mode in [vk::PresentModeKHR::IMMEDIATE, vk::PresentModeKHR::MAILBOX] {
        assert!(fast_direct_layer(mode, 1, &frame, None, true));
        assert!(!fast_direct_layer(mode, 1, &frame, None, false));
        assert!(!fast_direct_layer(mode, 2, &frame, None, true));
        assert!(!fast_direct_layer(
            mode,
            1,
            &frame,
            Some(&[Region {
                x: 0,
                y: 0,
                width: 32,
                height: 32
            }]),
            true,
        ));
        let shifted = appkit::view::Frame { x: 1, ..frame };
        assert!(!fast_direct_layer(mode, 1, &shifted, None, true));
    }
    for mode in [vk::PresentModeKHR::FIFO, vk::PresentModeKHR::FIFO_RELAXED] {
        assert!(!fast_direct_layer(mode, 1, &frame, None, true));
    }
}

#[test]
fn mailbox_presentation_is_preferred_when_available() {
    assert_eq!(
        choose_present_mode(&[vk::PresentModeKHR::FIFO, vk::PresentModeKHR::MAILBOX]),
        vk::PresentModeKHR::MAILBOX
    );
    assert_eq!(
        choose_present_mode(&[vk::PresentModeKHR::IMMEDIATE, vk::PresentModeKHR::FIFO]),
        vk::PresentModeKHR::FIFO
    );
}

/// A window, a swapchain and a frame that reaches it.
///
/// Everything this module does is a call into a driver and a window
/// system, so there is nothing here a unit test can stand in for: the
/// test opens a real window on the host it runs on, presents into it, and
/// is skipped where there is no such host. `ROSEMARY_TEST_VULKAN=1` asks
/// for it, the same switch the adapter test uses.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "Cocoa needs the main thread; use rosemary's Metal guest tests"
)]
fn a_window_is_presented_to_through_its_swapchain() {
    if isolate_vulkan_graphical_test("a_window_is_presented_to_through_its_swapchain") {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|adapter| adapter.gpu().ok()) else {
        return;
    };
    assert!(
        crate::vulkan::surface_api().is_some(),
        "the instance was created without VK_KHR_surface"
    );
    assert!(
        gpu.swapchain_api().is_some(),
        "the device was created without VK_KHR_swapchain"
    );

    // The flags a window of this runtime is actually made with: no
    // graphics API at all, so that this is the call that decides. A test
    // which asked for `VULKAN` here would not be testing the conversion
    // every real window goes through.
    let flags = sdl3::video::flags::HIGH_PIXEL_DENSITY;
    let Some(window) = sdl3::video::create_window(c"rosemary swapchain", 320, 240, flags) else {
        return;
    };
    sdl3::video::show_window(window);
    let (width, height) =
        sdl3::video::window_size_in_pixels(window).expect("a window SDL just made");
    let frame = test_full_frame(window, width as u32, height as u32);
    assert!(frame.covers_window());
    // The window is converted by the first presentation and not before.
    assert_eq!(
        sdl3::video::window_flags(window) & sdl3::video::flags::VULKAN,
        0,
        "the window was a Vulkan window before anything presented to it"
    );

    let source = test_render_target(gpu, frame.width, frame.height).expect("a colour image");

    // More frames than the swapchain has images times the frames in
    // flight, so that every fence, every acquisition semaphore and every
    // per-image presentation semaphore is reused several times over.
    // Reuse is where a synchronization mistake shows up; the first frame
    // never does.
    let handle = || {
        SWAPCHAINS
            .lock()
            .expect("window swapchains")
            .as_ref()
            .and_then(|all| all.get(&(window as usize)))
            .map(|window| window.state.lock().expect("window swapchain").handle)
    };
    for _ in 0..12 {
        assert_eq!(
            present(gpu, 1, &frame, &source, &[]),
            Ok(true),
            "the window was not presented to through a swapchain"
        );
    }
    flush_if_composed(window);
    // ...and is one afterwards, having been converted in place from a
    // window with no graphics API - the case a real guest's window is
    // in, whatever order it created its `MTLDevice` in.
    assert_ne!(
        sdl3::video::window_flags(window) & sdl3::video::flags::VULKAN,
        0,
        "presenting did not make the window a Vulkan window"
    );
    let steady = handle().expect("a swapchain for the window");
    for _ in 0..12 {
        assert_eq!(present(gpu, 1, &frame, &source, &[]), Ok(true));
    }
    flush_if_composed(window);
    // A window that has not resized keeps the swapchain it has. A
    // surface with a `currentExtent` of its own answers a size that is
    // not the one that was asked for, and comparing against *that* would
    // rebuild the swapchain on every single frame.
    assert_eq!(
        handle(),
        Some(steady),
        "the swapchain was rebuilt without the window resizing"
    );

    // Presentation survives the window changing size, which is a
    // different swapchain and a different set of images.
    sdl3::video::set_window_size(window, 400, 300);
    let (width, height) = sdl3::video::window_size_in_pixels(window).expect("still a window");
    let resized = appkit::view::Frame {
        width: width as u32,
        height: height as u32,
        window_width: width as u32,
        window_height: height as u32,
        ..frame
    };
    assert_eq!(present(gpu, 1, &resized, &source, &[]), Ok(true));
    flush_if_composed(window);

    // Take the swapchain out of the map so that its `Drop` runs the
    // teardown while the window it presented to is still alive.
    SWAPCHAINS
        .lock()
        .expect("window swapchains")
        .as_mut()
        .and_then(|all| all.remove(&(window as usize)));
    gpu.destroy_image(source);
    sdl3::video::destroy_window(window);
}

/// Exercise the real asynchronous scheduler, which the deterministic
/// coalescing tests intentionally keep disabled. Run separately with
/// ROSEMARY_TEST_VULKAN=1 and a selected WSI mode (IMMEDIATE or FIFO).
#[test]
#[cfg_attr(target_os = "macos", ignore = "Cocoa needs the main thread")]
fn two_composited_windows_present_on_independent_workers() {
    if isolate_vulkan_graphical_test("two_composited_windows_present_on_independent_workers") {
        return;
    }
    let _retire_guard = AwaitTestWindowRetirements;
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|adapter| adapter.gpu().ok()) else {
        return;
    };
    let mut windows = Vec::new();
    for _ in 0..4 {
        let window = sdl3::video::create_window(
            c"rosemary fifo independent window",
            320,
            240,
            sdl3::video::flags::HIGH_PIXEL_DENSITY,
        )
        .expect("X11 test window");
        sdl3::video::show_window(window);
        let (w, h) = sdl3::video::window_size_in_pixels(window).expect("window size");
        let source = test_render_target(gpu, w as u32, h as u32).expect("source image");
        let frame = test_full_frame(window, w as u32, h as u32);
        assert_eq!(
            present(gpu, window as usize, &frame, &source, &[]),
            Ok(true)
        );
        let entry = SWAPCHAINS
            .lock()
            .expect("swapchains")
            .as_ref()
            .and_then(|all| all.get(&(window as usize)).cloned())
            .expect("test window swapchain");
        assert!(uses_layer_compositor(
            entry.state.lock().expect("state").present_mode
        ));
        ensure_layer_compositor_worker(window as usize, &entry);
        windows.push((window, source, entry));
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    for (_, _, entry) in &windows {
        loop {
            let state = entry.state.lock().expect("window state");
            if !state.composite_dirty && state.frame == 2 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "FIFO worker did not drain its window"
            );
            drop(state);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    // Deliberately hold window A's acquisition/presentation lock. The
    // global drawable dispatcher must defer A immediately and still stage B.
    let first = &windows[0];
    let second = &windows[1];
    let make_frame = |window: sdl3::Window| {
        let (w, h) = sdl3::video::window_size_in_pixels(window).expect("window size");
        test_full_frame(window, w as u32, h as u32)
    };
    let window_key = first.0 as usize;
    let snapshot = sdl3::video::presentation_window(first.0).expect("main-thread video snapshot");
    let from_worker =
        std::thread::spawn(move || sdl3::video::presentation_window(window_key as sdl3::Window))
            .join()
            .expect("worker snapshot");
    assert_eq!(
        from_worker,
        Some(snapshot),
        "worker must use main-thread snapshot"
    );
    let window_id = sdl3::video::window_id(first.0);
    sdl3::video::invalidate_presentation_window(window_id, &[first.0]);
    let uninitialized =
        std::thread::spawn(move || sdl3::video::presentation_window(window_key as sdl3::Window))
            .join()
            .expect("worker query");
    assert_eq!(
        uninitialized, None,
        "worker may not call SDL for uncached geometry"
    );
    assert!(
        sdl3::video::presentation_window(first.0).is_some(),
        "main thread repairs cache"
    );
    let first_guard = first
        .2
        .presenting
        .lock()
        .expect("deliberate WSI contention");
    assert!(
        window_presenting_busy(first.0),
        "AppKit must defer busy window A"
    );
    assert!(
        !window_presenting_busy(second.0),
        "window B remains available"
    );
    assert_eq!(
        present_nonblocking(gpu, first.0 as usize, &make_frame(first.0), &first.1, &[]),
        Ok(None),
        "backing-store staging must not block AppKit on A"
    );
    assert_eq!(
        present_nonblocking(
            gpu,
            second.0 as usize,
            &make_frame(second.0),
            &second.1,
            &[]
        ),
        Ok(Some(true)),
        "backing-store staging must still progress on B"
    );
    let before = Instant::now();
    assert_eq!(
        present_owned(
            gpu,
            first.0 as usize,
            &make_frame(first.0),
            &first.1,
            &[],
            0
        ),
        Ok(None),
        "contended window must defer rather than block the dispatcher"
    );
    assert_eq!(
        present_owned(
            gpu,
            second.0 as usize,
            &make_frame(second.0),
            &second.1,
            &[],
            0
        ),
        Ok(Some(true)),
        "uncontended window must still stage"
    );
    assert!(
        before.elapsed() < Duration::from_millis(200),
        "window B stalled on A"
    );
    drop(first_guard);

    // Surface loss is window-local. Another window must continue.
    *first.2.injected_acquire_error.lock().expect("injection") =
        Some(vk::Result::ERROR_SURFACE_LOST_KHR);
    assert_eq!(
        present(gpu, first.0 as usize, &make_frame(first.0), &first.1, &[]),
        Ok(true)
    );
    assert_eq!(
        present(
            gpu,
            second.0 as usize,
            &make_frame(second.0),
            &second.1,
            &[]
        ),
        Ok(true)
    );
    let until = Instant::now() + Duration::from_secs(3);
    while first.2.present_failure.load(Ordering::Acquire) != 1 {
        assert!(
            Instant::now() < until,
            "terminal WSI error must stop the worker"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        first
            .2
            .injected_acquire_error
            .lock()
            .expect("injection")
            .is_none()
    );
    assert_eq!(
        present_owned(
            gpu,
            first.0 as usize,
            &make_frame(first.0),
            &first.1,
            &[],
            0
        ),
        Ok(Some(true)),
        "surface-lost window must not transition to incompatible SDL window surfaces"
    );
    let until = Instant::now() + Duration::from_secs(3);
    while second.2.state.lock().expect("state").composite_dirty {
        assert!(
            Instant::now() < until,
            "healthy window B cannot be stalled by A"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    // SURFACE_LOST terminates its worker too, but chooses the SDL readback
    // fallback instead of submitting more work to the invalid WSI surface.
    *second.2.injected_acquire_error.lock().expect("injection") =
        Some(vk::Result::ERROR_SURFACE_LOST_KHR);
    assert_eq!(
        present(
            gpu,
            second.0 as usize,
            &make_frame(second.0),
            &second.1,
            &[]
        ),
        Ok(true)
    );
    let until = Instant::now() + Duration::from_secs(3);
    while second.2.present_failure.load(Ordering::Acquire) != 1 {
        assert!(Instant::now() < until, "surface-lost worker must exit");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        present_owned(
            gpu,
            second.0 as usize,
            &make_frame(second.0),
            &second.1,
            &[],
            0
        ),
        Ok(Some(true)),
        "surface-lost window must be disabled until re-creation"
    );
    // Repeat terminal failure injection *at vkQueuePresentKHR*, after a
    // successful WSI image acquisition, not at vkAcquireNextImageKHR.
    for (at, error, expected, should_fallback) in [
        (2usize, vk::Result::ERROR_SURFACE_LOST_KHR, 1, true),
        (3usize, vk::Result::ERROR_DEVICE_LOST, 2, true),
    ] {
        let (window, source, entry) = &windows[at];
        entry
            .state
            .lock()
            .expect("swapchain state")
            .injected_queue_present_error = Some(error);
        assert_eq!(
            present(gpu, *window as usize, &make_frame(*window), source, &[]),
            Ok(true)
        );
        let until = Instant::now() + Duration::from_secs(3);
        while entry.present_failure.load(Ordering::Acquire) != expected {
            assert!(
                Instant::now() < until,
                "queue-present terminal error must stop worker: {error:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let state = entry.state.lock().expect("state");
        assert!(
            state.composite_dirty,
            "failed presentation cannot clear dirty accumulator"
        );
        assert!(
            state.injected_queue_present_error.is_none(),
            "injected fault not consumed"
        );
        drop(state);
        assert_eq!(
            present_nonblocking(gpu, *window as usize, &make_frame(*window), source, &[]),
            Ok(Some(should_fallback)),
            "wrong policy after queue-present error: {error:?}"
        );
    }
    assert!(
        gpu.lost.load(Ordering::Acquire),
        "device loss must mark the logical device"
    );
    for (window, _, entry) in &windows {
        assert_eq!(
            entry.present_failure.load(Ordering::Acquire),
            2,
            "device loss must stop every window of this GPU: {window:p}"
        );
    }
    for (window, source, entry) in windows {
        forget_window(window);
        drop(entry);
        gpu.destroy_image(source);
        sdl3::video::destroy_window(window);
    }
    // The fault was injected after successful real Vulkan presentation.
    // The host device remains operational for independent unit tests.
    gpu.lost.store(false, Ordering::Release);
}

/// SDL creates the surface on its video thread; the Vulkan initializer must
/// not serialize another window behind its (deliberately slow) driver call.
#[test]
#[cfg_attr(target_os = "macos", ignore = "SDL/Cocoa main thread integration")]
fn slow_window_initialization_does_not_block_other_windows_or_teardown() {
    if isolate_vulkan_graphical_test(
        "slow_window_initialization_does_not_block_other_windows_or_teardown",
    ) {
        return;
    }
    let _retire_guard = AwaitTestWindowRetirements;
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|adapter| adapter.gpu().ok()) else {
        return;
    };
    let mut created = Vec::new();
    for _ in 0..2 {
        let window = sdl3::video::create_window(c"independent initialization", 240, 180, 0)
            .expect("host SDL window");
        sdl3::video::show_window(window);
        let _ = sdl3::video::presentation_window(window).expect("video snapshot");
        let source = test_render_target(gpu, 240, 180).expect("source");
        let frame = test_full_frame(window, 240, 180);
        created.push((window, source, frame));
    }
    let a = &created[0];
    let b = &created[1];
    TEST_SLOW_INITIALIZER_STARTED.store(false, Ordering::Release);
    TEST_SLOW_INITIALIZER.store(a.0 as usize, Ordering::Release);
    assert_eq!(
        present_impl(gpu, a.0 as usize, &a.2, &a.1, &[], None, None, true),
        Ok(None)
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while !TEST_SLOW_INITIALIZER_STARTED.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "slow initializer did not start");
        std::thread::sleep(Duration::from_millis(1));
    }
    let start = Instant::now();
    assert_eq!(present(gpu, b.0 as usize, &b.2, &b.1, &[]), Ok(true));
    assert!(
        start.elapsed() < Duration::from_millis(400),
        "window B initialization stalled behind window A's 600ms Vulkan initializer"
    );
    forget_window(a.0);
    sdl3::video::destroy_window(a.0);
    // SDL_Window destruction may be deferred until A's cancellation guard
    // retires; the pump completes all outstanding surface cleanup.
    std::thread::sleep(Duration::from_millis(650));
    sdl3::video::vulkan::service_surface_requests();
    TEST_SLOW_INITIALIZER.store(0, Ordering::Release);
    forget_window(b.0);
    sdl3::video::destroy_window(b.0);
    for (_, source, _) in created {
        gpu.destroy_image(source);
    }
}

/// A clean layer compositor worker must terminate when its Vulkan device is lost,
/// without needing another layer frame or SDL window destruction to wake it.
#[test]
#[cfg_attr(target_os = "macos", ignore = "Cocoa needs the main thread")]
fn idle_compositor_worker_exits_on_device_loss_without_new_frames() {
    if isolate_vulkan_graphical_test(
        "idle_compositor_worker_exits_on_device_loss_without_new_frames",
    ) {
        return;
    }
    let _retire_guard = AwaitTestWindowRetirements;
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let adapters = crate::vulkan::adapters().expect("Vulkan adapter discovery");
    let gpu = adapters
        .first()
        .expect("Vulkan adapter")
        .gpu()
        .expect("Vulkan GPU");
    let window =
        sdl3::video::create_window(c"idle FIFO worker loss", 160, 120, 0).expect("SDL window");
    sdl3::video::show_window(window);
    let _ = sdl3::video::presentation_window(window).expect("SDL owner snapshot");
    let source = test_render_target(gpu, 160, 120).expect("layer source");
    let frame = test_full_frame(window, 160, 120);
    assert_eq!(
        present(gpu, window as usize, &frame, &source, &[]),
        Ok(true)
    );
    let entry = SWAPCHAINS
        .lock()
        .expect("swapchains")
        .as_ref()
        .unwrap()
        .get(&(window as usize))
        .expect("FIFO swapchain")
        .clone();
    assert!(uses_layer_compositor(
        entry.state.lock().unwrap().present_mode
    ));
    TEST_COMPOSITOR_IDLE_PARKED.store(0, Ordering::Release);
    ensure_layer_compositor_worker(window as usize, &entry);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let state = entry.state.lock().unwrap();
        if !state.composite_dirty
            && TEST_COMPOSITOR_IDLE_PARKED.load(Ordering::Acquire) == window as usize
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "FIFO worker did not reach idle sleep"
        );
        drop(state);
        std::thread::sleep(Duration::from_millis(2));
    }
    // Notifying without a new frame must wake the sleeping worker.
    record_device_loss(gpu);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let finished = entry
            .worker
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|worker| worker.is_finished());
        if finished {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "idle FIFO worker retained after device loss"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(entry.present_failure.load(Ordering::Acquire), 2);
    assert!(!entry.state.lock().unwrap().composite_dirty);
    forget_window(window);
    drop(entry);
    sdl3::video::destroy_window(window);
    gpu.lost.store(false, Ordering::Release); // synthetic loss only
    gpu.destroy_image(source);
}

#[test]
fn initializer_scope_guard_cannot_clear_replacement_token() {
    let key = 0x7f34_9125usize;
    let first = Arc::new(AtomicBool::new(false));
    let replacement = Arc::new(AtomicBool::new(false));
    let first_guard = InitializerRegistration {
        window: key,
        token: Arc::clone(&first),
    };
    INITIALIZING
        .lock()
        .expect("initializers")
        .get_or_insert_with(HashMap::new)
        .insert(key, Arc::clone(&first));
    let replacement_guard = InitializerRegistration {
        window: key,
        token: Arc::clone(&replacement),
    };
    INITIALIZING
        .lock()
        .expect("initializers")
        .as_mut()
        .unwrap()
        .insert(key, Arc::clone(&replacement));
    drop(first_guard);
    assert!(
        INITIALIZING
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &replacement)),
        "old initializer cleared the next generation"
    );
    drop(replacement_guard);
    assert!(
        !INITIALIZING
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .contains_key(&key),
        "initializer scope guard did not clear its own generation"
    );
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "SDL/Cocoa main-thread integration")]
fn device_loss_before_swapchain_create_cleans_pending_initializer() {
    if isolate_vulkan_graphical_test(
        "device_loss_before_swapchain_create_cleans_pending_initializer",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let adapters = crate::vulkan::adapters().expect("real Vulkan adapters required");
    let gpu = adapters
        .first()
        .expect("Vulkan device")
        .gpu()
        .expect("usable GPU");
    let window =
        sdl3::video::create_window(c"early device loss", 170, 120, 0).expect("SDL video window");
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).expect("SDL video-thread metadata");
    let key = window as usize;
    let frame = test_full_frame(window, 170, 120);
    let source = test_render_target(gpu, 170, 120).expect("test GPU image");
    TEST_SLOW_INITIALIZER_STARTED.store(false, Ordering::Release);
    TEST_SLOW_INITIALIZER.store(key, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !TEST_SLOW_INITIALIZER_STARTED.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "initializer did not reach pre-create checkpoint"
        );
        let _ = present_impl(gpu, key, &frame, &source, &[], None, None, true);
        sdl3::video::vulkan::service_surface_requests();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        sdl3::video::vulkan::owns_window(window),
        "SDL window must be converted"
    );
    assert!(
        INITIALIZING
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .contains_key(&key)
    );
    gpu.lost.store(true, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(3);
    while INITIALIZING
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|all| all.contains_key(&key))
    {
        assert!(
            Instant::now() < deadline,
            "orphaned initializer entry after device loss"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        !sdl3::video::vulkan::surface_request_pending(window),
        "aborted converted surface remained taken"
    );
    assert!(
        !window_initialization_pending(window),
        "backing store permanently deferred"
    );
    assert_eq!(
        crate::present::present_store(window, &appkit::backing::Store::new(170, 120), &[], &[]),
        appkit::backing::PresentOutcome::Displayed,
        "Vulkan-converted window cannot use SDL CPU fallback after loss"
    );
    TEST_SLOW_INITIALIZER.store(0, Ordering::Release);
    gpu.lost.store(false, Ordering::Release); // synthetic flag; real device works
    forget_window(window);
    sdl3::video::vulkan::service_surface_requests();
    sdl3::video::destroy_window(window);
    gpu.destroy_image(source);
}

#[test]
fn a_transferred_surface_is_retired_once_when_metadata_disappears() {
    if isolate_vulkan_graphical_test(
        "a_transferred_surface_is_retired_once_when_metadata_disappears",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|a| a.gpu().ok()) else {
        return;
    };
    let window = sdl3::video::create_window(c"surface ownership", 160, 100, 0).unwrap();
    let _ = sdl3::video::presentation_window(window).unwrap();
    let id = sdl3::video::window_id(window);
    sdl3::video::invalidate_presentation_window(id, &[window]);
    TEST_SURFACE_GUARD_RELEASES.store(0, Ordering::Release);
    let key = window as usize;
    let instance = crate::vulkan::instance_handle().unwrap();
    let result = std::thread::spawn(move || {
        Swapchain::create(
            gpu,
            key as sdl3::Window,
            vk::Extent2D {
                width: 160,
                height: 100,
            },
            u64::MAX,
            instance,
        )
    })
    .join()
    .expect("initializer");
    assert!(
        result.is_err(),
        "missing SDL snapshot must fail before accessing fake VkSurface"
    );
    assert_eq!(
        TEST_SURFACE_GUARD_RELEASES.load(Ordering::Acquire),
        1,
        "VkSurface ownership must be retired exactly once on every early failure"
    );
    sdl3::video::destroy_window(window);
}

#[test]
fn minimized_window_retries_transient_initialization_after_restore() {
    if isolate_vulkan_graphical_test(
        "minimized_window_retries_transient_initialization_after_restore",
    ) {
        return;
    }
    let _retire_guard = AwaitTestWindowRetirements;
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|a| a.gpu().ok()) else {
        return;
    };
    let window = sdl3::video::create_window(c"minimized Vulkan recovery", 240, 180, 0).unwrap();
    sdl3::video::show_window(window);
    let _ = sdl3::video::presentation_window(window).unwrap();
    let frame = test_full_frame(window, 240, 180);
    let source = test_render_target(gpu, 240, 180).unwrap();
    sdl3::video::minimize_window(window);
    TEST_FAIL_FIRST_CREATE_WINDOW.store(window as usize, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !INIT_RETRIES
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|all| all.contains_key(&(window as usize)))
    {
        assert!(
            Instant::now() < deadline,
            "transient failure did not schedule retry"
        );
        let _ = present_impl(gpu, 42, &frame, &source, &[], None, None, true);
        sdl3::video::vulkan::service_surface_requests();
        std::thread::sleep(Duration::from_millis(2));
    }
    // Simulate >8 transient retries while minimized without waiting for
    // twelve real backoff intervals.
    for _ in 0..12 {
        assert!(schedule_retry(window as usize, false));
    }
    assert_eq!(
        INIT_RETRIES
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .get(&(window as usize))
            .unwrap()
            .delay,
        Duration::from_secs(2),
    );
    assert!(
        !REFUSED
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|r| r.contains_key(&(window as usize))),
        "temporary initialization failures may not permanently refuse a window"
    );
    INIT_RETRIES
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .get_mut(&(window as usize))
        .unwrap()
        .after = Instant::now();
    sdl3::video::restore_window(window);
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if SWAPCHAINS
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|all| all.contains_key(&(window as usize)))
        {
            break;
        }
        assert!(
            Instant::now() < until,
            "restored window never recovered Vulkan presentation"
        );
        let _ = present_impl(gpu, 42, &frame, &source, &[], None, None, true);
        sdl3::video::vulkan::service_surface_requests();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(present(gpu, 42, &frame, &source, &[]), Ok(true));
    flush_if_composed(window);
    assert!(sdl3::video::vulkan::owns_window(window));
    forget_window(window);
    sdl3::video::destroy_window(window);
    gpu.destroy_image(source);
}

#[test]
fn backing_store_preflight_defers_until_retry_deadline() {
    let key = 0x794514usize;
    reset_retry(key);
    assert!(!window_initialization_pending(key as sdl3::Window));
    assert!(schedule_retry(key, false));
    assert!(window_initialization_pending(key as sdl3::Window));
    reset_retry(key);
    assert!(!window_initialization_pending(key as sdl3::Window));
}

/// Emulate the failed vkQueueSubmit path: its fence was reset, but no command
/// was queued to signal it. The next FIFO stage must not wait on that fence.
#[test]
#[cfg_attr(target_os = "macos", ignore = "SDL/Cocoa main thread integration")]
fn unsubmitted_unsignaled_fifo_fence_does_not_stall_next_frame() {
    if isolate_vulkan_graphical_test("unsubmitted_unsignaled_fifo_fence_does_not_stall_next_frame")
    {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|a| a.gpu().ok()) else {
        return;
    };
    let Some(window) = sdl3::video::create_window(c"failed FIFO fence", 160, 120, 0) else {
        return;
    };
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).expect("SDL video snapshot");
    let instance = crate::vulkan::instance_handle().expect("Vulkan instance");
    let surface = sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library())
        .expect("SDL Vulkan surface");
    let mut swapchain = Swapchain::create(
        gpu,
        window,
        vk::Extent2D {
            width: 160,
            height: 120,
        },
        surface,
        instance,
    )
    .expect("Vulkan swapchain");
    let source = test_render_target(gpu, 160, 120).expect("source image");
    let frame = test_full_frame(window, 160, 120);
    let slot = 0;
    // SAFETY: the new frame's fence is unused and belongs to this GPU.
    unsafe { gpu.device.reset_fences(&[swapchain.frames[slot].done]) }
        .expect("simulate fence reset before failed submission");
    assert_eq!(
        unsafe { gpu.device.get_fence_status(swapchain.frames[slot].done) },
        Ok(false)
    );
    // An actually submitted but unsignaled slot must defer without waiting.
    swapchain.frames[slot].submitted = true;
    assert_eq!(
        swapchain.stage_composite(&frame, &source, None, &mut SourceOwner(None), true),
        Ok(None)
    );
    // A failed queue submit never marked the frame as submitted. Despite the
    // unsignaled fence, it is immediately safe to reuse the command buffer.
    swapchain.frames[slot].submitted = false;
    assert_eq!(
        swapchain.stage_composite(&frame, &source, None, &mut SourceOwner(None), true),
        Ok(Some(true))
    );
    drop(swapchain);
    gpu.destroy_image(source);
    sdl3::video::destroy_window(window);
}

#[test]
fn injected_fence_status_and_wait_device_loss_propagate() {
    if isolate_vulkan_graphical_test("injected_fence_status_and_wait_device_loss_propagate") {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|a| a.gpu().ok()) else {
        return;
    };
    for operation in ["vkGetFenceStatus", "vkWaitForFences"] {
        gpu.lost.store(false, Ordering::Release);
        let result: Result<bool, String> =
            gpu.fence_result(Err(vk::Result::ERROR_DEVICE_LOST), operation);
        assert!(result.unwrap_err().contains("ERROR_DEVICE_LOST"));
        assert!(
            gpu.lost.load(Ordering::Acquire),
            "{operation} did not mark device lost"
        );
    }
    gpu.lost.store(false, Ordering::Release);
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "SDL main thread")]
fn fifo_acquire_submit_failure_consumes_semaphore_and_retires_image() {
    if isolate_vulkan_graphical_test(
        "fifo_acquire_submit_failure_consumes_semaphore_and_retires_image",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|adapter| adapter.gpu().ok()) else {
        return;
    };
    let Some(window) = sdl3::video::create_window(c"FIFO acquire failure", 160, 120, 0) else {
        return;
    };
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).expect("SDL window snapshot");
    let instance = crate::vulkan::instance_handle().expect("Vulkan instance");
    let surface = sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library())
        .expect("Vulkan WSI surface");
    let mut state = Swapchain::create(
        gpu,
        window,
        vk::Extent2D {
            width: 160,
            height: 120,
        },
        surface,
        instance,
    )
    .expect("swapchain");
    if !uses_layer_compositor(state.present_mode) {
        drop(state);
        sdl3::video::destroy_window(window);
        return;
    }
    let source = test_render_target(gpu, 160, 120).expect("GPU image");
    let frame = test_full_frame(window, 160, 120);
    assert_eq!(
        state.stage_composite(&frame, &source, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    let original = state.handle;
    state.injected_queue_submit_error = Some(vk::Result::ERROR_OUT_OF_HOST_MEMORY);
    assert!(
        state.flush_composite(window).is_ok(),
        "failed FIFO submission must recover"
    );
    assert!(
        state.acquired_unsubmitted.is_none(),
        "FIFO failed submit left the acquire semaphore unconsumed"
    );
    assert_ne!(state.handle, original, "acquired image was not retired");
    assert!(
        state.composite_dirty,
        "cached Metal layers must be replayed"
    );
    state
        .flush_composite(window)
        .expect("replay preserved FIFO snapshot");
    assert!(!state.composite_dirty);
    assert_eq!(
        state.stage_composite(&frame, &source, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state
        .flush_composite(window)
        .expect("next FIFO frame must present normally");
    drop(state);
    gpu.destroy_image(source);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "SDL main thread")]
fn direct_vulkan_surface_creation_registers_cpu_surface_exclusion() {
    if isolate_vulkan_graphical_test(
        "direct_vulkan_surface_creation_registers_cpu_surface_exclusion",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(_gpu) = adapters.first().and_then(|adapter| adapter.gpu().ok()) else {
        return;
    };
    let Some(window) = sdl3::video::create_window(c"Direct Vulkan surface", 80, 64, 0) else {
        return;
    };
    assert!(!sdl3::video::vulkan::owns_window(window));
    let instance = crate::vulkan::instance_handle().expect("Vulkan instance");
    let surface = sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library())
        .expect("direct Vulkan surface");
    assert!(sdl3::video::vulkan::owns_window(window));
    assert!(
        !sdl3::video::present_window(window, |_| true),
        "Vulkan forbids SDL surface"
    );
    sdl3::video::vulkan::destroy_surface(instance, surface);
    assert!(
        sdl3::video::vulkan::owns_window(window),
        "destruction must not undo conversion"
    );
    sdl3::video::destroy_window(window);
}

/// Independent FIFO WSI scenarios share a real SDL surface and VkSwapchainKHR.
#[cfg(not(target_os = "macos"))]
fn v10_fifo_fixture() -> Option<(
    sdl3::Window,
    &'static Gpu,
    Swapchain,
    GpuImage,
    appkit::view::Frame,
)> {
    std::env::var_os("ROSEMARY_TEST_VULKAN")?;
    let gpu = crate::vulkan::adapters().ok()?.first()?.gpu().ok()?;
    let window = sdl3::video::create_window(c"FIFO persistent layers", 160, 120, 0)?;
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window)?;
    let instance = crate::vulkan::instance_handle()?;
    let surface = sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library())?;
    let state = Swapchain::create(
        gpu,
        window,
        vk::Extent2D {
            width: 160,
            height: 120,
        },
        surface,
        instance,
    )
    .expect("WSI swapchain");
    if !uses_layer_compositor(state.present_mode) {
        drop(state);
        sdl3::video::destroy_window(window);
        return None;
    }
    let image = test_render_target(gpu, 160, 120).expect("Metal content source");
    let frame = test_full_frame(window, 160, 120);
    Some((window, gpu, state, image, frame))
}

/// Exercises actual Vulkan acquisition and WSI presentation on the direct
/// worker path, including the lazy transition to multi-layer composition.
#[test]
#[cfg(not(target_os = "macos"))]
fn single_opaque_layer_uses_one_wsi_copy_then_snapshots_on_second_layer() {
    if isolate_vulkan_graphical_test(
        "single_opaque_layer_uses_one_wsi_copy_then_snapshots_on_second_layer",
    ) {
        return;
    }
    let expected_mode = match std::env::var("ROSEMARY_METAL_PRESENTATION_MODE").as_deref() {
        Ok("immediate") => vk::PresentModeKHR::IMMEDIATE,
        Ok("mailbox") => vk::PresentModeKHR::MAILBOX,
        _ => return,
    };
    let Some((window, gpu, mut state, image, frame)) = v10_fifo_fixture() else {
        return;
    };
    // A requested unavailable WSI mode is not real validation of that mode.
    assert_eq!(
        state.present_mode, expected_mode,
        "the host did not select the requested WSI mode"
    );
    state.layers.push(8);
    // Reuse every WSI image, acquisition semaphore and presentation fence
    // repeatedly; the first few images alone do not exercise retirement.
    for _ in 0..(FRAMES_IN_FLIGHT * 3 + 2) {
        state.queue_direct(
            8,
            &frame,
            PresentSource::from(&image),
            &mut SourceOwner(None),
            None,
        );
        assert_eq!(pending_batch_delay(&state), Duration::ZERO);
        assert!(
            state.composite.is_none(),
            "direct path allocated an intermediate image"
        );
        state
            .flush_composite(window)
            .expect("single-layer direct WSI presentation");
        assert!(
            state.composite.is_none(),
            "direct path copied into an accumulator"
        );
        assert!(!state.composite_dirty);
    }
    let prev_slot = state.direct.as_ref().unwrap().submitted_slot;
    assert!(prev_slot.is_some());
    state.layers.push(9);
    assert_eq!(state.stage_direct_snapshot(false), Ok(Some(true)));
    assert!(state.direct.is_none());
    assert!(
        state.composite.is_some(),
        "second layer must retain prior full-window content"
    );
    assert_eq!(
        state.stage_composite(
            &appkit::view::Frame {
                x: 12,
                y: 15,
                width: 80,
                height: 60,
                ..frame
            },
            &image,
            None,
            &mut SourceOwner(None),
            false,
        ),
        Ok(Some(true)),
    );
    state.flush_composite(window).expect("two-layer WSI frame");
    assert!(!state.composite_dirty);
    state
        .rebuild(state.requested)
        .expect("recreate multi-layer WSI");
    assert!(state.composite_dirty);
    state
        .flush_composite(window)
        .expect("static multi-layer replay");
    drop(state);
    gpu.destroy_image(image);
    sdl3::video::destroy_window(window);
}

/// An acquired WSI image must never force the worker to wait forever on
/// the previous maintenance1 presentation fence.
#[test]
#[cfg(not(target_os = "macos"))]
fn unsignaled_prior_present_fence_does_not_block_acquired_image() {
    if isolate_vulkan_graphical_test("unsignaled_prior_present_fence_does_not_block_acquired_image")
    {
        return;
    }
    let Some((window, gpu, mut state, image, frame)) = v10_fifo_fixture() else {
        return;
    };
    if state.presented.is_empty() {
        drop(state);
        gpu.destroy_image(image);
        sdl3::video::destroy_window(window);
        return;
    }
    assert_eq!(
        state.stage_composite(&frame, &image, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.flush_composite(window).expect("initial presentation");
    // Ensure the fault applies to whichever image this MAILBOX or IMMEDIATE
    // driver acquires next, including MAILBOX implementations that skip images.
    for fence in &mut state.presented {
        fence.pending = true;
    }
    state.injected_unsignaled_present_fence = true;
    let before = state.frame;
    // Acquire the same image on repeated presents to exercise its prior fence.
    for _ in 0..(state.images.len() * 3) {
        assert_eq!(
            state.stage_composite(&frame, &image, None, &mut SourceOwner(None), false),
            Ok(Some(true))
        );
        let start = Instant::now();
        state
            .flush_composite(window)
            .expect("present without waiting for stalled fence");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "previous presentation fence blocked worker"
        );
        if state.skipped_stalled_present_fence {
            break;
        }
    }
    assert!(
        state.skipped_stalled_present_fence,
        "failed to reacquire a previously presented image"
    );
    assert!(state.frame > before);
    assert!(
        state.acquired_unsubmitted.is_none(),
        "acquired semaphore was not consumed"
    );
    assert!(!state.composite_dirty);
    // After a present omits a maintenance fence, even a later signal of the
    // original fence cannot cover the new present. Retire via queue idle.
    assert!(state.stalled_present_fence);
    state.idle();
    assert!(!state.stalled_present_fence);
    assert!(state.presented.iter().all(|fence| !fence.pending));
    drop(state);
    gpu.destroy_image(image);
    sdl3::video::destroy_window(window);
}

#[cfg(not(target_os = "macos"))]
fn direct_layer_resize_preserves_unscaled_static_window_image_impl(wsi_only: bool) {
    let expected_mode = match std::env::var("ROSEMARY_METAL_PRESENTATION_MODE").as_deref() {
        Ok("immediate") => vk::PresentModeKHR::IMMEDIATE,
        Ok("mailbox") => vk::PresentModeKHR::MAILBOX,
        _ => return,
    };
    let Some((window, gpu, mut state, image, frame)) = v10_fifo_fixture() else {
        return;
    };
    assert_eq!(state.present_mode, expected_mode);
    state.layers.push(8);
    state.queue_direct(
        8,
        &frame,
        PresentSource::from(&image),
        &mut SourceOwner(None),
        None,
    );
    state
        .flush_composite(window)
        .expect("first direct WSI presentation");
    assert!(state.composite.is_none());
    assert!(sdl3::video::set_window_size(window, 208, 144));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        sdl3::pump();
        let caps = unsafe {
            crate::vulkan::surface_api()
                .expect("WSI surface API")
                .get_physical_device_surface_capabilities(gpu.physical, state.surface)
        }
        .expect("WSI capabilities");
        if caps.current_extent.width == u32::MAX && wsi_only {
            // This host uses the caller's requested extent, not WSI's:
            // cannot force a new effective extent without a new request.
            drop(state);
            gpu.destroy_image(image);
            sdl3::video::destroy_window(window);
            return;
        }
        if caps.current_extent.width == u32::MAX
            || (caps.current_extent.width, caps.current_extent.height) == (208, 144)
        {
            break;
        }
        assert!(Instant::now() < deadline, "Vulkan surface did not resize");
        std::thread::sleep(Duration::from_millis(5));
    }
    let requested_before = state.requested;
    let rebuild_request = if wsi_only {
        requested_before
    } else {
        vk::Extent2D {
            width: 208,
            height: 144,
        }
    };
    state.rebuild(rebuild_request).expect("direct WSI resize");
    if wsi_only {
        assert_eq!(state.requested, requested_before, "rebuild request changed");
        assert_ne!(
            (state.extent.width, state.extent.height),
            (frame.width, frame.height),
            "WSI extent did not change"
        );
    }
    assert!(
        state.direct.is_none(),
        "old geometry must not be stretched as a direct frame"
    );
    let saved = state
        .composite
        .as_ref()
        .expect("resized snapshot of static layer");
    assert_eq!(
        (saved.width, saved.height),
        (state.extent.width, state.extent.height)
    );
    assert!(state.composite_dirty);
    state
        .flush_composite(window)
        .expect("static layer replay without guest redraw");
    assert!(!state.composite_dirty);
    drop(state);
    gpu.destroy_image(image);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn direct_layer_resize_preserves_unscaled_static_window_image() {
    direct_layer_resize_preserves_unscaled_static_window_image_impl(false);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn wsi_effective_extent_change_preserves_direct_layer_without_new_request() {
    direct_layer_resize_preserves_unscaled_static_window_image_impl(true);
}

/// Exercise the actual AppKit CPU store upload worker's Vulkan path. Its
/// image cache must rotate while an earlier frame is still retained, and the
/// compositor must select direct WSI instead of an intermediate accumulator.
#[test]
#[cfg(not(target_os = "macos"))]
fn backing_store_uploads_select_direct_wsi_without_overwriting_inflight_source() {
    if isolate_vulkan_graphical_test(
        "backing_store_uploads_select_direct_wsi_without_overwriting_inflight_source",
    ) {
        return;
    }
    let expected = match std::env::var("ROSEMARY_METAL_PRESENTATION_MODE").as_deref() {
        Ok("immediate") => vk::PresentModeKHR::IMMEDIATE,
        Ok("mailbox") => vk::PresentModeKHR::MAILBOX,
        _ => return,
    };
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Some(window) = sdl3::video::create_window(c"direct backing store", 160, 120, 0) else {
        return;
    };
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).expect("SDL metadata");
    let mut store = appkit::backing::Store::new(160, 120);
    store.pixels.fill(43);
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        match crate::present::present_store_synchronous(window, &store, &[], &[]) {
            appkit::backing::PresentOutcome::Displayed => break,
            appkit::backing::PresentOutcome::Deferred => {
                assert!(
                    Instant::now() < deadline,
                    "store upload never reached Vulkan"
                );
                sdl3::video::vulkan::service_surface_requests();
                std::thread::sleep(Duration::from_millis(2));
            }
            outcome => panic!("store upload failed: {outcome:?}"),
        }
    }
    let entry = SWAPCHAINS
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get(&(window as usize))
        .unwrap()
        .clone();
    {
        let _presenting = entry.presenting.lock().unwrap();
        let mut state = entry.state.lock().unwrap();
        assert_eq!(state.present_mode, expected);
        assert!(
            state.composite.is_none(),
            "backing store took extra GPU accumulator path"
        );
        assert!(
            state.direct.as_ref().unwrap().backing.is_some(),
            "missing retained backing image"
        );
        state.flush_composite(window).expect("direct store WSI");
    }
    let (old, _) = crate::present::backing_store_image_for_test(window).unwrap();
    store.pixels.fill(77);
    assert_eq!(
        crate::present::present_store_synchronous(window, &store, &[], &[]),
        appkit::backing::PresentOutcome::Displayed
    );
    {
        let (current, spare) = crate::present::backing_store_image_for_test(window).unwrap();
        assert!(
            !Arc::ptr_eq(&old, &current),
            "overwrote an image retained by WSI"
        );
        assert_eq!(spare, 1, "source rotation did not retain old image");
    }
    {
        let _presenting = entry.presenting.lock().unwrap();
        let mut state = entry.state.lock().unwrap();
        assert!(state.composite.is_none());
        state
            .flush_composite(window)
            .expect("new backing-store direct WSI");
        state.direct.as_mut().unwrap().queued_at = Instant::now() - DIRECT_IDLE_SNAPSHOT_DELAY;
        assert!(state.snapshot_idle_direct().expect("idle store snapshot"));
        state.layers.push(0x777); // The next backing-store update joins a second Metal layer.
    }
    store.pixels.fill(91);
    assert_eq!(
        crate::present::present_store_synchronous(window, &store, &[], &[]),
        appkit::backing::PresentOutcome::Displayed,
        "multi-layer backing store should enter the compositor"
    );
    {
        let _presenting = entry.presenting.lock().unwrap();
        let mut state = entry.state.lock().unwrap();
        assert!(state.composite.is_some());
        assert!(
            state
                .frames
                .iter()
                .any(|frame| frame.backing.is_some() && frame.submitted),
            "staged backing source lost its lease before the GPU-copy fence"
        );
        state
            .flush_composite(window)
            .expect("multi-layer backing-store present");
        state.idle();
        assert!(state.frames.iter().all(|frame| frame.backing.is_none()));
    }
    drop(old);
    drop(entry); // Teardown waits for every external WindowSwapchain Arc.
    crate::present::forget_backing_store_for_test(window);
    forget_window(window);
    sdl3::video::destroy_window(window);
    wait_for_test_retirements();
}

/// The real AppKit overlay-present route must retain its blended GPU image
/// and perform one WSI copy. The deliberately invalid DMA-BUF exercises the
/// overlay fallback copy and refusal contract without requiring an exporter;
/// importing an actual foreign buffer needs separate supported-host coverage.
#[test]
#[cfg(target_os = "linux")]
fn backing_store_with_gpu_overlay_uses_direct_wsi_without_extra_composite() {
    if isolate_vulkan_graphical_test(
        "backing_store_with_gpu_overlay_uses_direct_wsi_without_extra_composite",
    ) {
        return;
    }
    let expected = match std::env::var("ROSEMARY_METAL_PRESENTATION_MODE").as_deref() {
        Ok("immediate") => vk::PresentModeKHR::IMMEDIATE,
        Ok("mailbox") => vk::PresentModeKHR::MAILBOX,
        _ => return,
    };
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none()
        || std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none()
    {
        return;
    }
    let window = sdl3::video::create_window(c"direct GPU overlay store", 160, 120, 0)
        .expect("SDL overlay test window");
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).expect("SDL overlay metadata");
    let mut store = appkit::backing::Store::new(160, 120);
    store.pixels.fill(42);
    let overlays = [appkit::backing::GpuOverlay {
        view: 0,
        picture: real_gbm_picture_with_pixels(160, 120, |_, _| [0, 0, 255, 255]),
        rect: appkit::backing::Rect {
            x: 0,
            y: 0,
            width: 160,
            height: 120,
        },
    }];
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        match crate::present::present_store_synchronous(window, &store, &[], &overlays) {
            appkit::backing::PresentOutcome::Displayed => break,
            appkit::backing::PresentOutcome::Deferred => {
                assert!(
                    Instant::now() < deadline,
                    "overlay backing store did not reach Vulkan"
                );
                sdl3::video::vulkan::service_surface_requests();
                std::thread::sleep(Duration::from_millis(2));
            }
            other => panic!("unexpected overlay backing outcome: {other:?}"),
        }
    }
    let entry = SWAPCHAINS
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get(&(window as usize))
        .unwrap()
        .clone();
    let old = {
        let _presenting = entry.presenting.lock().unwrap();
        let mut state = entry.state.lock().unwrap();
        assert_eq!(state.present_mode, expected);
        assert!(
            state.composite.is_none(),
            "overlay unnecessarily staged into accumulator"
        );
        let direct = state
            .direct
            .as_ref()
            .expect("eligible overlay must queue directly");
        let lease = direct
            .backing
            .as_ref()
            .expect("blended image lacks GPU-copy lease");
        assert_eq!(
            lease.image.format,
            vk::Format::R8G8B8A8_UNORM,
            "overlay route did not select actual blended target"
        );
        let previous = Arc::clone(lease);
        state
            .flush_composite(window)
            .expect("direct overlay WSI frame");
        assert!(
            state.composite.is_none(),
            "direct overlay frame created full window composite"
        );
        previous
    };
    store.pixels.fill(87);
    assert_eq!(
        crate::present::present_store_synchronous(window, &store, &[], &overlays),
        appkit::backing::PresentOutcome::Displayed
    );
    {
        let _presenting = entry.presenting.lock().unwrap();
        let mut state = entry.state.lock().unwrap();
        let direct = state.direct.as_ref().expect("updated overlay direct frame");
        let fresh = direct.backing.as_ref().expect("overlay source lease");
        assert!(
            !Arc::ptr_eq(&old, fresh),
            "blended image overwritten while WSI retained it"
        );
        assert!(state.composite.is_none());
        state
            .flush_composite(window)
            .expect("second direct overlay WSI");
        state.direct.as_mut().unwrap().queued_at = Instant::now() - DIRECT_IDLE_SNAPSHOT_DELAY;
        assert!(state.snapshot_idle_direct().expect("idle overlay snapshot"));
        state.idle();
    }
    // All eight blended allocations are now retained as though the WSI
    // presenter were slow to return them. Retries must defer before issuing
    // another backing upload, not spin on a costly GPU copy every millisecond.
    let retained_blends = crate::present::hold_every_blended_source_for_test(window);
    let submitted = crate::present::backing_upload_submissions_for_test();
    for _ in 0..5 {
        assert_eq!(
            crate::present::present_store_synchronous(window, &store, &[], &overlays),
            appkit::backing::PresentOutcome::Deferred
        );
        assert_eq!(
            crate::present::backing_upload_submissions_for_test(),
            submitted,
            "pool exhaustion submitted a redundant full-frame GPU upload"
        );
    }
    drop(retained_blends);
    assert_eq!(
        crate::present::present_store_synchronous(window, &store, &[], &overlays),
        appkit::backing::PresentOutcome::Displayed,
        "source lease release must permit upload and presentation"
    );
    assert_eq!(
        crate::present::backing_upload_submissions_for_test(),
        submitted + 1
    );
    drop(old);
    drop(entry);
    crate::present::forget_backing_store_for_test(window);
    appkit::backing::forget_window(window); // Clear the deliberate DMA-BUF refusal.
    forget_window(window);
    sdl3::video::destroy_window(window);
    wait_for_test_retirements();
}

/// A failed DMA blend cannot publish the raw CPU backing as a complete frame.
#[test]
#[cfg(target_os = "linux")]
fn failed_dma_blend_defers_or_requests_cpu_without_accepting_raw_store() {
    if isolate_vulkan_graphical_test(
        "failed_dma_blend_defers_or_requests_cpu_without_accepting_raw_store",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none()
        || std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none()
    {
        return;
    }
    let window = sdl3::video::create_window(c"DMA blend rollback", 80, 64, 0)
        .expect("SDL Vulkan overlay fixture");
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).expect("SDL snapshot");
    let mut store = appkit::backing::Store::new(80, 64);
    for pixel in store.pixels.as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&[0, 255, 0, 255]);
    }
    let picture = real_gbm_picture_with_pixels(80, 64, |_, _| [0, 0, 255, 255]);
    let overlays = [appkit::backing::GpuOverlay {
        picture: Arc::clone(&picture),
        view: 0x3157,
        rect: appkit::backing::Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 64,
        },
    }];
    let retry = |overlays: &[appkit::backing::GpuOverlay]| {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            match crate::present::present_store_synchronous(window, &store, &[], overlays) {
                appkit::backing::PresentOutcome::Displayed => break,
                appkit::backing::PresentOutcome::Deferred => {
                    assert!(Instant::now() < deadline, "Vulkan store never admitted");
                    sdl3::video::vulkan::service_surface_requests();
                    std::thread::sleep(Duration::from_millis(2));
                }
                other => panic!("unexpected outcome: {other:?}"),
            }
        }
    };
    retry(&[]);
    for fault in [1, 2] {
        crate::present::inject_blend_failure_for_test(fault);
        let deadline = Instant::now() + Duration::from_secs(6);
        while crate::present::blend_failure_pending_for_test() != 0 {
            assert!(Instant::now() < deadline, "blend fault never reached");
            assert_ne!(
                crate::present::present_store_synchronous(window, &store, &[], &overlays),
                appkit::backing::PresentOutcome::Displayed,
                "incomplete DMA overlay acknowledged as displayed"
            );
            sdl3::video::vulkan::service_surface_requests();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            !appkit::backing::dmabuf_import_refused(window),
            "temporary failure permanently disabled DMA-BUF capability"
        );
        retry(&overlays);
    }
    // The asynchronous worker must not cache a failed submission as the
    // accepted overlay snapshot. A subsequent identical job must retry.
    let accepted_before = crate::present::accepted_dma_blends_for_test();
    crate::present::inject_blend_failure_for_test(2);
    let deadline = Instant::now() + Duration::from_secs(6);
    while crate::present::blend_failure_pending_for_test() != 0 {
        assert!(
            Instant::now() < deadline,
            "async DMA fault was never exercised"
        );
        assert_ne!(
            crate::present::present_store(window, &store, &[], &overlays),
            appkit::backing::PresentOutcome::Displayed
        );
        sdl3::video::vulkan::service_surface_requests();
        std::thread::sleep(Duration::from_millis(2));
    }
    while crate::present::store_upload_completed_outcome_for_test(window).is_none() {
        assert!(
            Instant::now() < deadline,
            "async store worker never completed"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    // The store worker retries a transient Vulkan deferral internally for
    // up to 250ms. A Displayed completion is valid only after a later full
    // overlay blend actually succeeds; the failed attempt alone cannot count.
    assert_eq!(
        crate::present::store_upload_completed_outcome_for_test(window),
        Some(appkit::backing::PresentOutcome::Displayed)
    );
    assert!(
        crate::present::accepted_dma_blends_for_test() > accepted_before,
        "worker accepted the raw store without completing any DMA blend"
    );
    // No change to CPU generation, producer allocation or overlay placement.
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        match crate::present::present_store(window, &store, &[], &overlays) {
            appkit::backing::PresentOutcome::Displayed => break,
            appkit::backing::PresentOutcome::Deferred
            | appkit::backing::PresentOutcome::DisplayedEarlier => {
                assert!(
                    Instant::now() < deadline,
                    "unchanged DMA source never retried"
                );
                sdl3::video::vulkan::service_surface_requests();
                std::thread::sleep(Duration::from_millis(2));
            }
            other => panic!("DMA retry worker returned {other:?}"),
        }
    }
    crate::present::inject_blend_failure_for_test(3);
    let deadline = Instant::now() + Duration::from_secs(6);
    while crate::present::blend_failure_pending_for_test() != 0 {
        assert!(
            Instant::now() < deadline,
            "permanent refusal never reached importer"
        );
        assert_ne!(
            crate::present::present_store_synchronous(window, &store, &[], &overlays),
            appkit::backing::PresentOutcome::Displayed,
            "permanent DMA refusal acknowledged an incomplete frame"
        );
        sdl3::video::vulkan::service_surface_requests();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        appkit::backing::dmabuf_producer_refused(window, overlays[0].view, &picture),
        "permanent import refusal did not request this producer's CPU pixels"
    );
    assert!(
        !appkit::backing::dmabuf_import_refused(window),
        "a producer-specific refusal must not blacklist the window"
    );
    retry(&[]);
    // The same SDL window may still import a different immutable producer,
    // including one with no readable CPU fallback.
    let valid_picture = real_gbm_picture_with_pixels(80, 64, |_, _| [255, 0, 0, 255]);
    let independent = [appkit::backing::GpuOverlay {
        view: 0x3158,
        picture: Arc::clone(&valid_picture),
        rect: overlays[0].rect,
    }];
    assert!(!appkit::backing::dmabuf_producer_refused(
        window,
        independent[0].view,
        &valid_picture,
    ));
    retry(&independent);
    let accepted = crate::present::backing_store_blended_for_test(window)
        .expect("accepted independent DMA image");
    assert_eq!(
        read_scene_pixel(accepted.gpu, accepted.image.image, 80, 64, 40, 30),
        [0, 0, 255, 255],
        "different DMA producer did not reach the accepted GPU image"
    );
    drop(accepted);
    crate::present::forget_backing_store_for_test(window);
    appkit::backing::forget_window(window);
    forget_window(window);
    sdl3::video::destroy_window(window);
    wait_for_test_retirements();
}

/// The ninth overlay must not disappear just because one pass has eight sets.
#[test]
#[cfg(target_os = "linux")]
fn ninth_dma_overlay_cannot_be_accepted_without_its_last_pixel() {
    if isolate_vulkan_graphical_test("ninth_dma_overlay_cannot_be_accepted_without_its_last_pixel")
    {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none()
        || std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none()
    {
        return;
    }
    let window = sdl3::video::create_window(c"ninth DMA overlay", 80, 64, 0).unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let mut store = appkit::backing::Store::new(80, 64);
    store.pixels = [0, 255, 0, 255].repeat(80 * 64);
    let red = real_gbm_picture_with_pixels(80, 64, |_, _| [0, 0, 255, 255]);
    let blue = real_gbm_picture_with_pixels(80, 64, |_, _| [255, 0, 0, 255]);
    let mut overlays = (0..8)
        .map(|view| appkit::backing::GpuOverlay {
            view,
            picture: Arc::clone(&red),
            rect: appkit::backing::Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 64,
            },
        })
        .collect::<Vec<_>>();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match crate::present::present_store_synchronous(window, &store, &[], &overlays) {
            appkit::backing::PresentOutcome::Displayed => break,
            appkit::backing::PresentOutcome::Deferred => {
                assert!(
                    Instant::now() < deadline,
                    "eight DMA overlays never displayed"
                );
                sdl3::video::vulkan::service_surface_requests();
                std::thread::sleep(Duration::from_millis(2));
            }
            other => panic!("unexpected eight-overlay result: {other:?}"),
        }
    }
    let old = crate::present::backing_store_blended_for_test(window)
        .expect("eight completed DMA overlays");
    assert_eq!(
        read_scene_pixel(old.gpu, old.image.image, 80, 64, 40, 30),
        [255, 0, 0, 255]
    );
    overlays.push(appkit::backing::GpuOverlay {
        view: 8,
        picture: blue,
        rect: overlays[0].rect,
    });
    let old_pixel = read_scene_pixel(old.gpu, old.image.image, 80, 64, 40, 30);
    assert_eq!(old_pixel, [255, 0, 0, 255]);
    // Nine overlays must reach the GPU as two ordered descriptor passes,
    // not defer forever or acknowledge only the first eight sources.
    loop {
        match crate::present::present_store_synchronous(window, &store, &[], &overlays) {
            appkit::backing::PresentOutcome::Displayed => break,
            appkit::backing::PresentOutcome::Deferred => {
                assert!(
                    Instant::now() < deadline,
                    "nine DMA overlays never displayed"
                );
                sdl3::video::vulkan::service_surface_requests();
                std::thread::sleep(Duration::from_millis(2));
            }
            other => panic!("unexpected nine-overlay result: {other:?}"),
        }
    }
    let completed = crate::present::backing_store_blended_for_test(window)
        .expect("completed nine-source target");
    assert_eq!(
        read_scene_pixel(completed.gpu, completed.image.image, 80, 64, 40, 30),
        [0, 0, 255, 255],
        "ninth source was not included in final GPU pixels"
    );
    assert_eq!(
        read_scene_pixel(old.gpu, old.image.image, 80, 64, 40, 30),
        old_pixel,
        "a previously accepted GPU image was modified in place"
    );
    drop(completed);
    // Returning below the eight-descriptor boundary must also make progress.
    overlays.truncate(8);
    loop {
        match crate::present::present_store_synchronous(window, &store, &[], &overlays) {
            appkit::backing::PresentOutcome::Displayed => break,
            appkit::backing::PresentOutcome::Deferred => {
                assert!(
                    Instant::now() < deadline,
                    "eight-overlay recovery never displayed"
                );
                sdl3::video::vulkan::service_surface_requests();
                std::thread::sleep(Duration::from_millis(2));
            }
            other => panic!("unexpected recovery: {other:?}"),
        }
    }
    let recovered = crate::present::backing_store_blended_for_test(window).unwrap();
    assert_eq!(
        read_scene_pixel(recovered.gpu, recovered.image.image, 80, 64, 40, 30),
        [255, 0, 0, 255]
    );
    drop(recovered);
    assert!(!appkit::backing::dmabuf_import_refused(window));
    drop(old);
    crate::present::forget_backing_store_for_test(window);
    appkit::backing::forget_window(window);
    forget_window(window);
    sdl3::video::destroy_window(window);
    wait_for_test_retirements();
}

/// A full-window store upload supplies a retained GPU allocation, not a
/// CAMetalDrawable. The WSI worker can copy it directly without an offscreen
/// accumulator or reading a source the next upload will overwrite.
#[test]
#[cfg(not(target_os = "macos"))]
fn retained_backing_source_uses_direct_wsi_and_releases_after_gpu_copy() {
    if isolate_vulkan_graphical_test(
        "retained_backing_source_uses_direct_wsi_and_releases_after_gpu_copy",
    ) {
        return;
    }
    let expected = match std::env::var("ROSEMARY_METAL_PRESENTATION_MODE").as_deref() {
        Ok("immediate") => vk::PresentModeKHR::IMMEDIATE,
        Ok("mailbox") => vk::PresentModeKHR::MAILBOX,
        _ => return,
    };
    let Some((window, gpu, mut state, image, frame)) = v10_fifo_fixture() else {
        return;
    };
    assert_eq!(state.present_mode, expected, "real WSI mode required");
    let lease = BackingImageLease::new(gpu, image);
    state.layers.push(window as usize);
    assert!(fast_direct_layer(state.present_mode, 1, &frame, None, true));
    for _ in 0..FRAMES_IN_FLIGHT * 2 {
        state.queue_direct(
            8,
            &frame,
            PresentSource::from(&lease.image),
            &mut SourceOwner(None),
            Some(Arc::clone(&lease)),
        );
        assert_eq!(pending_batch_delay(&state), Duration::ZERO);
        assert!(state.composite.is_none());
        state
            .flush_composite(window)
            .expect("backing image direct WSI copy");
        assert!(
            state.composite.is_none(),
            "must not allocate offscreen full-frame accumulator"
        );
        assert!(
            Arc::strong_count(&lease) > 1,
            "direct WSI image lost retained lifetime"
        );
    }
    // After idle, a single snapshot is enough to preserve static contents
    // and the last backing-store copy is no longer WSI-owned.
    state.direct.as_mut().unwrap().queued_at = Instant::now() - DIRECT_IDLE_SNAPSHOT_DELAY;
    assert!(state.snapshot_idle_direct().expect("idle source snapshot"));
    state.idle();
    assert_eq!(
        Arc::strong_count(&lease),
        1,
        "GPU fence failed to release backing store"
    );
    assert!(!state.composite_dirty);
    drop(state);
    drop(lease);
    sdl3::video::destroy_window(window);
}

/// Idle direct frames are snapshotted only after they are on screen, then
/// their drawable can be returned without queuing another WSI presentation.
#[test]
#[cfg(not(target_os = "macos"))]
fn idle_direct_frame_snapshots_once_and_releases_the_source_fence() {
    if isolate_vulkan_graphical_test(
        "idle_direct_frame_snapshots_once_and_releases_the_source_fence",
    ) {
        return;
    }
    let expected_mode = match std::env::var("ROSEMARY_METAL_PRESENTATION_MODE").as_deref() {
        Ok("immediate") => vk::PresentModeKHR::IMMEDIATE,
        Ok("mailbox") => vk::PresentModeKHR::MAILBOX,
        _ => return,
    };
    let Some((window, gpu, mut state, image, frame)) = v10_fifo_fixture() else {
        return;
    };
    assert_eq!(state.present_mode, expected_mode);
    state.layers.push(8);
    state.queue_direct(
        8,
        &frame,
        PresentSource::from(&image),
        &mut SourceOwner(None),
        None,
    );
    assert!(
        !state
            .snapshot_idle_direct()
            .expect("dirty direct cannot snapshot")
    );
    state.flush_composite(window).expect("direct presentation");
    assert!(state.direct.is_some());
    assert!(
        !state
            .snapshot_idle_direct()
            .expect("recent direct cannot snapshot")
    );
    state.direct.as_mut().unwrap().queued_at = Instant::now() - DIRECT_IDLE_SNAPSHOT_DELAY;
    let before_stage = state.frame;
    assert!(state.snapshot_idle_direct().expect("snapshot idle direct"));
    assert_eq!(state.frame, before_stage + 1, "one GPU staging copy");
    assert!(state.direct.is_none());
    assert!(state.composite.is_some());
    assert!(
        !state.composite_dirty,
        "already-presented image must not re-enter WSI"
    );
    let snapshot_slot = before_stage % state.frames.len();
    assert!(!state.frames[snapshot_slot].submitted);
    assert!(state.frames[snapshot_slot].source.is_none());
    assert!(
        !state
            .snapshot_idle_direct()
            .expect("only one idle snapshot")
    );
    state.layers.push(9);
    assert_eq!(
        state.stage_composite(
            &appkit::view::Frame {
                x: 4,
                y: 3,
                width: 20,
                height: 40,
                ..frame
            },
            &image,
            None,
            &mut SourceOwner(None),
            false,
        ),
        Ok(Some(true)),
    );
    state
        .flush_composite(window)
        .expect("present new layer atop preserved idle snapshot");
    drop(state);
    gpu.destroy_image(image);
    sdl3::video::destroy_window(window);
}

/// Acquired-image recovery must preserve the direct source, not silently
/// consume its drawable on an out-of-date or failed submission.
#[test]
#[cfg(not(target_os = "macos"))]
fn direct_single_layer_replays_after_failed_submit() {
    if isolate_vulkan_graphical_test("direct_single_layer_replays_after_failed_submit") {
        return;
    }
    let expected_mode = match std::env::var("ROSEMARY_METAL_PRESENTATION_MODE").as_deref() {
        Ok("immediate") => vk::PresentModeKHR::IMMEDIATE,
        Ok("mailbox") => vk::PresentModeKHR::MAILBOX,
        _ => return,
    };
    let Some((window, gpu, mut state, image, frame)) = v10_fifo_fixture() else {
        return;
    };
    assert_eq!(state.present_mode, expected_mode);
    state.layers.push(8);
    state.queue_direct(
        8,
        &frame,
        PresentSource::from(&image),
        &mut SourceOwner(None),
        None,
    );
    state.injected_queue_submit_error = Some(vk::Result::ERROR_OUT_OF_HOST_MEMORY);
    state
        .flush_composite(window)
        .expect("recover direct acquire after failed submit");
    assert!(state.composite_dirty);
    assert!(state.direct.is_some());
    state
        .flush_composite(window)
        .expect("replay direct without new drawable");
    assert!(!state.composite_dirty);
    assert!(state.composite.is_none());
    drop(state);
    gpu.destroy_image(image);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn fifo_static_layer_survives_out_of_date_recreation_without_guest_redraw() {
    if isolate_vulkan_graphical_test(
        "fifo_static_layer_survives_out_of_date_recreation_without_guest_redraw",
    ) {
        return;
    }
    let Some((window, gpu, mut state, image, frame)) = v10_fifo_fixture() else {
        return;
    };
    assert_eq!(
        state.stage_composite(&frame, &image, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state
        .flush_composite(window)
        .expect("first FIFO presentation");
    let previous = state.composite.as_ref().unwrap().image;
    let old_handle = state.handle;
    assert!(!state.composite_dirty);
    state.rebuild(state.requested).expect("WSI rebuild");
    assert_ne!(state.handle, old_handle);
    assert_eq!(
        state.composite.as_ref().unwrap().image,
        previous,
        "static layer GPU image should survive same-format recreation"
    );
    assert!(
        state.composite_dirty,
        "static layer must be queued for WSI replay"
    );
    // This is another queue presentation only: no stage_composite/guest frame.
    state
        .flush_composite(window)
        .expect("replay last layer frame after recreate");
    assert!(!state.composite_dirty);
    drop(state);
    gpu.destroy_image(image);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn fifo_out_of_date_acquire_replays_static_layers_without_guest_submit() {
    if isolate_vulkan_graphical_test(
        "fifo_out_of_date_acquire_replays_static_layers_without_guest_submit",
    ) {
        return;
    }
    let Some((window, gpu, mut state, image, frame)) = v10_fifo_fixture() else {
        return;
    };
    assert_eq!(
        state.stage_composite(&frame, &image, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.flush_composite(window).expect("initial present");
    let previous = state.composite.as_ref().unwrap().image;
    assert_eq!(
        state.stage_composite(&frame, &image, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    let before = state.handle;
    let ready = state
        .prepare_composite(window)
        .expect("reserve WSI acquire");
    // WSI reports OUT_OF_DATE before assigning a new image, so the layer
    // cache must remain presentable without another guest drawable.
    assert_eq!(state.complete_composite(ready, Ok(None)), Ok(None));
    assert_ne!(state.handle, before);
    assert!(state.composite_dirty);
    assert_eq!(state.composite.as_ref().unwrap().image, previous);
    state
        .flush_composite(window)
        .expect("present cached layer after OUT_OF_DATE");
    assert!(!state.composite_dirty);
    drop(state);
    gpu.destroy_image(image);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn fifo_out_of_date_queue_present_replays_static_layers() {
    if isolate_vulkan_graphical_test("fifo_out_of_date_queue_present_replays_static_layers") {
        return;
    }
    let Some((window, gpu, mut state, image, frame)) = v10_fifo_fixture() else {
        return;
    };
    assert_eq!(
        state.stage_composite(&frame, &image, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.injected_queue_present_error = Some(vk::Result::ERROR_OUT_OF_DATE_KHR);
    let before = state.handle;
    state
        .flush_composite(window)
        .expect("WSI out-of-date must rebuild");
    assert_ne!(state.handle, before);
    assert!(state.composite_dirty);
    let saved = state.composite.as_ref().unwrap().image;
    state
        .flush_composite(window)
        .expect("present cached layer after WSI error");
    assert!(!state.composite_dirty);
    assert_eq!(state.composite.as_ref().unwrap().image, saved);
    drop(state);
    gpu.destroy_image(image);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn fifo_window_resize_keeps_snapshot_without_new_drawable() {
    if isolate_vulkan_graphical_test("fifo_window_resize_keeps_snapshot_without_new_drawable") {
        return;
    }
    let Some((window, gpu, mut state, image, frame)) = v10_fifo_fixture() else {
        return;
    };
    assert_eq!(
        state.stage_composite(&frame, &image, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.flush_composite(window).expect("original present");
    let previous = state.composite.as_ref().unwrap().image;
    assert!(sdl3::video::set_window_size(window, 208, 144));
    // X11 applies resize asynchronously. Do not accidentally test only a
    // logical requested extent while the actual Vulkan surface stays old.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        sdl3::pump();
        let caps = unsafe {
            crate::vulkan::surface_api()
                .expect("WSI surface API")
                .get_physical_device_surface_capabilities(gpu.physical, state.surface)
        }
        .expect("WSI capabilities");
        if caps.current_extent.width == u32::MAX
            || (caps.current_extent.width, caps.current_extent.height) == (208, 144)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native Vulkan surface never resized"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    state
        .rebuild(vk::Extent2D {
            width: 208,
            height: 144,
        })
        .expect("WSI resize");
    assert!(state.composite_dirty);
    let retained = state.composite.as_ref().unwrap();
    assert_eq!(
        (retained.width, retained.height),
        (state.extent.width, state.extent.height)
    );
    assert_ne!(
        (state.extent.width, state.extent.height),
        (160, 120),
        "fixture did not resize the actual Vulkan surface"
    );
    assert_ne!(retained.image, previous, "resized snapshot must be copied");
    let resized = retained.image;
    state
        .flush_composite(window)
        .expect("replay resized snapshot without guest frame");
    let extent = state.extent;
    state.rebuild(extent).expect("same-size WSI rebuild");
    assert_eq!(
        state.composite.as_ref().unwrap().image,
        resized,
        "a rebuild that changes nothing must keep the snapshot it has"
    );
    drop(state);
    gpu.destroy_image(image);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn secondary_acquire_recovery_failure_waits_wsi_fence_on_teardown() {
    if isolate_vulkan_graphical_test(
        "secondary_acquire_recovery_failure_waits_wsi_fence_on_teardown",
    ) {
        return;
    }
    let Some((window, gpu, mut state, image, frame)) = v10_fifo_fixture() else {
        return;
    };
    assert_eq!(
        state.stage_composite(&frame, &image, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.injected_queue_submit_error = Some(vk::Result::ERROR_OUT_OF_HOST_MEMORY);
    state.injected_recovery_error = Some(vk::Result::ERROR_OUT_OF_HOST_MEMORY);
    let result = state.flush_composite(window);
    assert!(
        result.is_err(),
        "double failed queue submissions must disable FIFO"
    );
    let (acquired, _) = state
        .acquired_unsubmitted
        .expect("unconsumed WSI semaphore must remain tracked");
    let acquire_fence = state
        .frames
        .iter()
        .find(|f| f.acquired == acquired)
        .unwrap()
        .acquire_done;
    gpu.wait(acquire_fence)
        .expect("acquire fence proves WSI signal finished");
    drop(state); // Destructor independently waits before retiring semaphore.
    gpu.destroy_image(image);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn failed_recreation_never_reuses_retired_old_swapchain() {
    if isolate_vulkan_graphical_test("failed_recreation_never_reuses_retired_old_swapchain") {
        return;
    }
    let Some((window, gpu, mut state, image, frame)) = v10_fifo_fixture() else {
        return;
    };
    assert_eq!(
        state.stage_composite(&frame, &image, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state
        .flush_composite(window)
        .expect("first FIFO presentation");
    let old_handle = state.handle;
    let saved = state.composite.as_ref().unwrap().image;
    state.injected_build_failure = true;
    assert!(
        state.rebuild(state.requested).is_err(),
        "inject post-create failure after Vulkan retires oldSwapchain"
    );
    assert!(state.retired);
    assert_eq!(state.handle, old_handle, "still owned but unusable");
    state
        .rebuild(state.requested)
        .expect("recreate without retired oldSwapchain");
    assert!(!state.retired);
    assert_eq!(
        state.last_create_old_swapchain,
        vk::SwapchainKHR::null(),
        "retired oldSwapchain cannot be supplied to vkCreateSwapchainKHR twice"
    );
    // Native Vulkan handle values may be reused after destruction.
    assert_eq!(state.composite.as_ref().unwrap().image, saved);
    state
        .flush_composite(window)
        .expect("replay preserved layer after failed recreate");
    drop(state);
    gpu.destroy_image(image);
    sdl3::video::destroy_window(window);
}

#[test]
fn initialization_retry_frequency_is_bounded_without_blacklisting() {
    let key = 0x794513usize;
    reset_retry(key);
    for _ in 0..16 {
        assert!(schedule_retry(key, false));
    }
    let retries = INIT_RETRIES.lock().expect("retries");
    let retry = retries.as_ref().unwrap().get(&key).unwrap();
    assert_eq!(retry.delay, Duration::from_secs(2), "retry must be capped");
    drop(retries);
    assert!(!retry_ready(key));
    reset_retry(key);
    assert!(retry_ready(key));
}

#[test]
fn device_loss_during_swapchain_initialization_does_not_relock_registry() {
    if isolate_vulkan_graphical_test(
        "device_loss_during_swapchain_initialization_does_not_relock_registry",
    ) {
        return;
    }
    if isolate_vulkan_graphical_test(
        "device_loss_during_swapchain_initialization_does_not_relock_registry",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|a| a.gpu().ok()) else {
        return;
    };
    let window = sdl3::video::create_window(c"device lost in initializer", 190, 130, 0).unwrap();
    sdl3::video::show_window(window);
    let _ = sdl3::video::presentation_window(window).unwrap();
    let frame = test_full_frame(window, 190, 130);
    let source = test_render_target(gpu, 190, 130).unwrap();
    TEST_FAIL_DEVICE_CREATE_WINDOW.store(window as usize, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !gpu.lost.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "initializer device loss deadlocked the registry"
        );
        let _ = present_impl(gpu, window as usize, &frame, &source, &[], None, None, true);
        sdl3::video::vulkan::service_surface_requests();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        SWAPCHAINS.try_lock().is_ok(),
        "device-loss handling must release swapchain mutex"
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while INITIALIZING
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|all| all.contains_key(&(window as usize)))
    {
        assert!(
            Instant::now() < deadline,
            "device-loss initializer not cleared"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        sdl3::video::vulkan::owns_window(window),
        "SDL already converted this window before Swapchain::create"
    );
    assert!(
        !sdl3::video::vulkan::surface_request_pending(window),
        "device-loss during Swapchain::create left SDL State::Taken"
    );
    assert!(
        !window_initialization_pending(window),
        "converted backing store permanently deferred on device loss"
    );
    assert_eq!(
        crate::present::present_store(window, &appkit::backing::Store::new(190, 130), &[], &[],),
        appkit::backing::PresentOutcome::Displayed,
        "converted terminal window must not keep reporting Deferred or use SDL fallback"
    );
    forget_window(window);
    std::thread::sleep(Duration::from_millis(10));
    sdl3::video::destroy_window(window);
    sdl3::video::vulkan::service_surface_requests();
    gpu.lost.store(false, Ordering::Release); // synthetic result, real device healthy
    gpu.destroy_image(source);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn native_gpu_escrow_restores_exact_bytes() {
    if isolate_vulkan_graphical_test("native_gpu_escrow_restores_exact_bytes") {
        return;
    }
    let Some((window, gpu, mut state, metal, _)) = v10_fifo_fixture() else {
        return;
    };
    let desc = ImageDescription {
        image_type: vk::ImageType::TYPE_2D,
        view_type: vk::ImageViewType::TYPE_2D,
        format: vk::Format::B8G8R8A8_UNORM,
        width: 2048,
        height: 2048,
        depth: 1,
        levels: 1,
        layers: 1,
        samples: 1,
        usage: gpu::USAGE_RENDER_TARGET | gpu::USAGE_SHADER_READ,
        cube: false,
        components: vk::ComponentMapping::default(),
    };
    let old = BackingImageLease::create_for_window(gpu, window as usize, &desc).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, old.image.image, [1.0, 0.0, 0.0, 1.0]);
    })
    .unwrap();
    let escrow = scene::NativeGpuEscrow::capture(&mut state, &old)
        .unwrap()
        .unwrap();
    assert_eq!(escrow.pixels.len(), 2048 * 2048 * 4);
    assert_eq!(&escrow.pixels[..4], &[0, 0, 255, 255]);
    drop(old);
    let recovered = escrow.restore(gpu, window as usize).unwrap();
    assert_eq!(
        read_scene_pixel(gpu, recovered.image.image, 2048, 2048, 100, 120),
        [0, 0, 255, 255]
    );
    drop(recovered);
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn native_gpu_escrow_never_reads_busy_frame_or_unsupported_format() {
    if isolate_vulkan_graphical_test(
        "native_gpu_escrow_never_reads_busy_frame_or_unsupported_format",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, _)) = v10_fifo_fixture() else {
        return;
    };
    let desc = ImageDescription {
        image_type: vk::ImageType::TYPE_2D,
        view_type: vk::ImageViewType::TYPE_2D,
        format: vk::Format::B8G8R8A8_UNORM,
        width: 8,
        height: 8,
        depth: 1,
        levels: 1,
        layers: 1,
        samples: 1,
        usage: gpu::USAGE_RENDER_TARGET,
        cube: false,
        components: vk::ComponentMapping::default(),
    };
    let snapshot = BackingImageLease::create_for_window(gpu, window as usize, &desc).unwrap();
    state.idle();
    unsafe { gpu.device.reset_fences(&[state.frames[0].done]) }.unwrap();
    state.frames[0].scene_reads.push(Arc::clone(&snapshot));
    state.frames[0].submitted = true;
    assert!(
        scene::NativeGpuEscrow::capture(&mut state, &snapshot)
            .unwrap()
            .is_none()
    );
    state.frames[0].submitted = false;
    state.frames[0].scene_reads.clear();
    let non_native = gpu
        .create_image(&ImageDescription {
            format: vk::Format::R16G16B16A16_SFLOAT,
            ..desc
        })
        .unwrap();
    assert!(scene::NativeGpuEscrow::extent(&non_native).is_err());
    gpu.destroy_image(non_native);
    drop(snapshot);
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// Read a window accumulator only after the staging queue has completed.
/// This inspects raw RGBA8 compositor texels, not a WSI screenshot.
#[cfg(not(target_os = "macos"))]
fn read_scene_pixel(
    gpu: &'static Gpu,
    image: vk::Image,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
) -> Vec<u8> {
    let buffer = gpu
        .create_buffer(u64::from(width) * u64::from(height) * 4)
        .expect("GPU readback staging");
    gpu.one_shot(|device, cb| unsafe {
        let sync = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        device.cmd_pipeline_barrier(
            cb,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[sync],
        );
        device.cmd_copy_image_to_buffer(
            cb,
            image,
            vk::ImageLayout::GENERAL,
            buffer.buffer,
            &[vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })],
        );
    })
    .expect("GPU readback");
    let offset = (y as usize * width as usize + x as usize) * 4;
    let pixel = unsafe { std::slice::from_raw_parts(buffer.contents.add(offset), 4).to_vec() };
    gpu.destroy_buffer(buffer);
    pixel
}

#[test]
#[cfg(not(target_os = "macos"))]
fn opacity_group_composites_metal_drawable_before_applying_root_alpha() {
    if isolate_vulkan_graphical_test(
        "opacity_group_composites_metal_drawable_before_applying_root_alpha",
    ) {
        return;
    }
    let Some((window, gpu, mut state, underlay, frame)) = v10_fifo_fixture() else {
        return;
    };
    let drawable = test_render_target(gpu, frame.width, frame.height).unwrap();
    let ca = test_render_target(gpu, frame.width, frame.height).unwrap();
    for (image, channels) in [
        (&underlay, [1., 0., 0., 1.]),
        (&drawable, [0., 1., 0., 1.]),
        (&ca, [0., 0., 1., 1.]),
    ] {
        gpu.one_shot(|device, cb| unsafe {
            cmd_test_clear_color(device, cb, image.image, channels);
        })
        .unwrap();
    }
    let blue = test_opaque_metal_placement(0x200, DirectFrame::from_frame(&frame));
    let group = |id| ScenePlacement {
        opacity_groups: vec![appkit::view::OpacityGroup {
            id: 0x201,
            opacity: 0.5,
        }],
        id,
        frame: DirectFrame::from_frame(&frame),
        opacity: 1.0,
        opaque: true,
        clip: None,
        rounded: Vec::new(),
        occluded: Some(Vec::new()),
        native: None,
    };
    let root = group(0x202);
    let red = group(0x203);
    state.layers.extend([blue.id, root.id, red.id]);
    state.synchronize_scene(window as usize, &[blue.clone(), root.clone(), red.clone()]);
    for (id, image) in [(blue.id, &underlay), (root.id, &drawable), (red.id, &ca)] {
        assert_eq!(
            state.stage_composite_layer(id, &frame, image, None, &mut SourceOwner(None), false),
            Ok(Some(true))
        );
    }
    state.idle();
    let pixel = |state: &Swapchain| {
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            frame.width,
            frame.height,
            40,
            30,
        )
    };
    // Metal source-over rounds at a different intermediate step from Apple's
    // offscreen CARenderer; allow one 8-bit unit, but never a leaked child.
    let native_pixel = |actual: Vec<u8>, expected: [u8; 4]| {
        assert!(
            actual.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1),
            "native macOS CA reference {expected:?}, Rosemary GPU {actual:?}"
        );
    };
    native_pixel(pixel(&state), [127, 0, 128, 255]);

    // The inner group covers an opaque red child. Its alpha must first
    // composite onto the green drawable in the outer half-opacity group.
    let mut nested_red = red.clone();
    nested_red.opacity_groups.push(appkit::view::OpacityGroup {
        id: 0x204,
        opacity: 0.5,
    });
    assert!(state.synchronize_scene(window as usize, &[blue.clone(), root.clone(), nested_red]));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    native_pixel(pixel(&state), [127, 64, 64, 255]);
    assert!(state.committed_scene_bytes() <= 512 * 1024 * 1024);
    // A unary inner boundary folds into its parent group; only the
    // actual two-child branch needs a separate GPU isolation target.
    assert_eq!(state.group_targets.iter().map(Vec::len).max(), Some(1));

    let mut quarter_root = root.clone();
    let mut quarter_red = red.clone();
    quarter_root.opacity_groups[0].opacity = 0.25;
    quarter_red.opacity_groups[0].opacity = 0.25;
    assert!(state.synchronize_scene(
        window as usize,
        &[blue.clone(), quarter_root.clone(), quarter_red.clone()]
    ));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    native_pixel(pixel(&state), [191, 0, 64, 255]);

    assert!(state.synchronize_scene(window as usize, &[blue, root]));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    native_pixel(pixel(&state), [127, 128, 0, 255]);
    // End of grouped rendering eventually returns per-frame temporary image
    // allocations to the physical window ledger without destroying the window.
    assert!(state.synchronize_scene(
        window as usize,
        &[test_opaque_metal_placement(
            0x200,
            DirectFrame::from_frame(&frame)
        )]
    ));
    state.idle();
    state.retire_copied();
    assert!(state.group_targets.iter().all(Vec::is_empty));

    drop(state);
    for image in [underlay, drawable, ca] {
        gpu.destroy_image(image);
    }
    sdl3::video::destroy_window(window);
}

/// A large grouped window must not allocate a full-window scratch image for
/// all eight frame slots. Every image is a real Vulkan/WSI-backed allocation.
#[cfg(not(target_os = "macos"))]
#[expect(
    clippy::too_many_arguments,
    reason = "Test matrix cases select independent window, scene and opacity parameters"
)]
fn long_lived_large_group_reuses_completed_scratch(
    width: u32,
    height: u32,
    depth: usize,
    idle_each: bool,
    origin: (i32, i32),
    clipped: bool,
    full_source: bool,
    ancestor_alpha: f32,
    overlapping_sources: bool,
) {
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let window = sdl3::video::create_window(
        c"high-resolution CA scratch rotation",
        width as i32,
        height as i32,
        0,
    )
    .unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        window,
        vk::Extent2D { width, height },
        surface,
        instance,
    )
    .unwrap();
    let background = test_render_target(gpu, width, height).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, background.image, [0., 1., 0., 1.]);
    })
    .unwrap();
    let source_extent = if full_source {
        (width, height)
    } else {
        (16, 16)
    };
    let image = test_render_target(gpu, source_extent.0, source_extent.1).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, image.image, [0., 0., 1., 1.]);
    })
    .unwrap();
    let second = if overlapping_sources {
        let source = test_render_target(gpu, source_extent.0, source_extent.1).unwrap();
        gpu.one_shot(|device, cb| unsafe {
            cmd_test_clear_color(device, cb, source.image, [1., 0., 0., 1.]);
        })
        .unwrap();
        Some(source)
    } else {
        None
    };
    let frame = appkit::view::Frame {
        x: origin.0,
        y: origin.1,
        width: source_extent.0,
        height: source_extent.1,
        ..test_full_frame(window, width, height)
    };
    let background_id = 0x6529usize;
    let full = test_full_frame(window, width, height);
    let base = test_opaque_metal_placement(background_id, DirectFrame::from_frame(&full));
    let layer = 0x652ausize;
    let mut placement = ScenePlacement {
        opacity_groups: (0..depth)
            .map(|n| appkit::view::OpacityGroup {
                id: 0x6000 + n,
                opacity: ancestor_alpha,
            })
            .collect(),
        id: layer,
        frame: DirectFrame::from_frame(&frame),
        opacity: 1.0,
        opaque: true,
        clip: clipped.then(|| {
            DirectFrame::from_frame(&appkit::view::Frame {
                x: frame.x + 4,
                y: frame.y + 4,
                width: 8,
                height: 8,
                ..frame
            })
        }),
        rounded: vec![],
        occluded: Some(vec![]),
        native: None,
    };
    let under_id = layer + 1;
    let live = |current: &ScenePlacement| {
        let mut items = vec![base.clone()];
        if overlapping_sources {
            let mut underneath = current.clone();
            underneath.id = under_id;
            items.push(underneath);
        }
        items.push(current.clone());
        items
    };
    state.layers.push(background_id);
    if overlapping_sources {
        state.layers.push(under_id);
    }
    state.layers.push(layer);
    state.synchronize_scene(window as usize, &live(&placement));
    assert_eq!(
        state.stage_composite_source(
            background_id,
            &full,
            PresentSource::from(&background),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    if let Some(second) = &second {
        assert_eq!(
            state.stage_composite_source(
                under_id,
                &frame,
                PresentSource::from(second),
                None,
                &mut SourceOwner(None),
                false,
                true
            ),
            Ok(Some(true))
        );
    }
    assert_eq!(
        state.stage_composite_source(
            layer,
            &frame,
            PresentSource::from(&image),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    let before = state.frame;
    let mut samples = Vec::<f64>::new();
    let mut overlapping_slots = 0;
    for iteration in 0..(FRAMES_IN_FLIGHT * 3 + 3) {
        let opacity = if iteration % 2 == 0 { 0.5 } else { 0.6 };
        placement.opacity_groups[depth - 1].opacity = opacity;
        state.synchronize_scene(window as usize, &live(&placement));
        let started = Instant::now();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match state.redisplay_scene(window as usize) {
                Ok(true) => break,
                Ok(false) => {
                    assert!(
                        Instant::now() < deadline,
                        "frame {iteration} of {width}x{height}, depth {depth} never advanced"
                    );
                    std::thread::sleep(Duration::from_micros(100));
                }
                Err(reason) => {
                    panic!("frame {iteration} of {width}x{height}, depth {depth}: {reason}")
                }
            }
        }
        // Test a completed producer/consumer fence before revisiting the
        // oldest slot, without destroying the window or its retained source.
        if idle_each {
            state.idle();
        } else {
            let busy = state
                .frames
                .iter()
                .filter(|frame| frame.submitted && !gpu.fence_signaled(frame.done).unwrap_or(true))
                .count();
            overlapping_slots = overlapping_slots.max(busy);
        }
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        assert!(
            state.scene_gpu_bytes() <= 512 * 1024 * 1024,
            "scene images exceeded committed 512 MiB budget"
        );
    }
    assert!(state.frame >= before + (FRAMES_IN_FLIGHT * 3 + 3));
    if !idle_each {
        println!("overlapping unretired Vulkan frame slots at peak: {overlapping_slots}");
        state.idle();
        assert!(
            overlapping_slots >= 2,
            "test never exercised overlapping GPU scene submissions"
        );
    }
    for slot in &state.group_targets {
        if full_source && !overlapping_sources {
            assert!(
                slot.is_empty(),
                "unary nested group retained unnecessary full-window scratch"
            );
        }
        if full_source && overlapping_sources {
            assert!(
                slot.len() <= 1,
                "unary ancestor isolation was not coalesced"
            );
        }
        for target in slot {
            assert!(
                (full_source || target.image.width <= if clipped { 8 } else { 16 })
                    && (full_source || target.image.height <= if clipped { 8 } else { 16 }),
                "small grouped element allocated a full-window scratch image"
            );
        }
    }
    samples.sort_by(f64::total_cmp);
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)];
    println!(
        "group metadata repaint {}x{} depth={} static Metal background, 27 updates: mean={mean:.3}ms p95={p95:.3}ms; fence-completed producer-side only",
        width, height, depth
    );
    let pixel = read_scene_pixel(
        gpu,
        state.composite.as_ref().unwrap().image,
        width,
        height,
        (frame.x + 8) as u32,
        (frame.y + 8) as u32,
    );
    if clipped {
        let outside = read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            width,
            height,
            (frame.x + 2) as u32,
            (frame.y + 2) as u32,
        );
        assert_eq!(
            outside,
            [0, 255, 0, 255],
            "ancestor clip did not preserve static Metal background"
        );
    }
    let expected_alpha = (255.0 * 0.5f32 * ancestor_alpha.powi(depth as i32 - 1)).round() as u8;
    assert!(pixel[0] <= 1 && pixel[3] == 255);
    assert!(
        pixel[1].abs_diff(255 - expected_alpha) <= 2,
        "static opaque green Metal background did not survive group opacity"
    );
    assert!(
        pixel[2].abs_diff(expected_alpha) <= 2,
        "wrong final group pixel: {pixel:?}"
    );
    drop(state);
    gpu.destroy_image(image);
    if let Some(second) = second {
        gpu.destroy_image(second);
    }
    gpu.destroy_image(background);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn six_k_group_progresses_through_more_than_eight_submitted_frames() {
    if isolate_vulkan_graphical_test(
        "six_k_group_progresses_through_more_than_eight_submitted_frames",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    long_lived_large_group_reuses_completed_scratch(
        6016,
        3384,
        1,
        true,
        (0, 0),
        false,
        false,
        0.5,
        false,
    );
}

#[test]
#[cfg(not(target_os = "macos"))]
fn six_k_six_nested_small_groups_retain_pixels_during_overlapping_submissions() {
    if isolate_vulkan_graphical_test(
        "six_k_six_nested_small_groups_retain_pixels_during_overlapping_submissions",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    long_lived_large_group_reuses_completed_scratch(
        6016,
        3384,
        6,
        false,
        (0, 0),
        false,
        false,
        0.5,
        false,
    );
}

#[test]
#[cfg(not(target_os = "macos"))]
fn twenty_four_nested_groups_fit_in_small_window_without_depth_refusal() {
    if isolate_vulkan_graphical_test(
        "twenty_four_nested_groups_fit_in_small_window_without_depth_refusal",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    long_lived_large_group_reuses_completed_scratch(
        128,
        96,
        24,
        true, // A tiny GPU workload need not overlap with its predecessor.
        (0, 0),
        false,
        false,
        0.5,
        false,
    );
}

#[test]
#[cfg(not(target_os = "macos"))]
fn clipped_offset_group_at_six_k_retains_static_background_and_forward_progress() {
    if isolate_vulkan_graphical_test(
        "clipped_offset_group_at_six_k_retains_static_background_and_forward_progress",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    long_lived_large_group_reuses_completed_scratch(
        6016,
        3384,
        5,
        false,
        (719, 433),
        true,
        false,
        0.5,
        false,
    );
}

#[test]
#[cfg(not(target_os = "macos"))]
fn full_six_k_unary_twenty_four_level_ca_chain_makes_progress_without_scratch() {
    if isolate_vulkan_graphical_test(
        "full_six_k_unary_twenty_four_level_ca_chain_makes_progress_without_scratch",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    long_lived_large_group_reuses_completed_scratch(
        6016,
        3384,
        24,
        false,
        (0, 0),
        false,
        true,
        0.95,
        false,
    );
}

#[test]
#[cfg(not(target_os = "macos"))]
fn offscreen_previously_admitted_six_k_source_does_not_force_nested_isolation() {
    if isolate_vulkan_graphical_test(
        "offscreen_previously_admitted_six_k_source_does_not_force_nested_isolation",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let (width, height) = (6016u32, 3384u32);
    let window =
        sdl3::video::create_window(c"offscreen nested CA", width as i32, height as i32, 0).unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        window,
        vk::Extent2D { width, height },
        surface,
        instance,
    )
    .unwrap();
    let full = test_full_frame(window, width, height);
    let green = test_render_target(gpu, width, height).unwrap();
    let red = test_render_target(gpu, width, height).unwrap();
    let blue = test_render_target(gpu, width, height).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, green.image, [0., 1., 0., 1.]);
        cmd_test_clear_color(device, cb, red.image, [0., 0., 1., 1.]);
        cmd_test_clear_color(device, cb, blue.image, [1., 0., 0., 1.]);
    })
    .unwrap();
    let base = test_opaque_metal_placement(0x8310, DirectFrame::from_frame(&full));
    let groups = (0..24)
        .map(|n| appkit::view::OpacityGroup {
            id: 0x9000 + n,
            opacity: 0.98,
        })
        .collect::<Vec<_>>();
    let mut first = test_opaque_metal_placement(0x8311, DirectFrame::from_frame(&full));
    first.opacity_groups = groups.clone();
    let mut second = test_opaque_metal_placement(0x8312, DirectFrame::from_frame(&full));
    second.opacity_groups = groups;
    state.layers.extend([base.id, first.id, second.id]);
    state.synchronize_scene(
        window as usize,
        &[base.clone(), first.clone(), second.clone()],
    );
    assert_eq!(
        state.stage_composite_source(
            base.id,
            &full,
            PresentSource::from(&green),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    assert_eq!(
        state.stage_composite_source(
            first.id,
            &full,
            PresentSource::from(&red),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    assert_eq!(
        state.stage_composite_source(
            second.id,
            &full,
            PresentSource::from(&blue),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    state.idle();
    assert!(
        state
            .scene_layers
            .iter()
            .any(|s| s.id == first.id && s.ready)
    );
    assert!(
        state
            .scene_layers
            .iter()
            .any(|s| s.id == second.id && s.ready)
    );
    // All 24 ancestors contain two visible sources initially, but only one
    // isolation scratch depth is required after unary folding.
    assert_eq!(
        state
            .normalized_opacity_paths()
            .iter()
            .filter(|(id, _, _)| *id == first.id || *id == second.id)
            .count(),
        2
    );
    let mut moved = second.clone();
    moved.frame = DirectFrame::from_frame(&appkit::view::Frame {
        x: width as i32 + 10,
        ..full
    });
    assert!(state.synchronize_scene(
        window as usize,
        &[base.clone(), first.clone(), moved.clone()]
    ));
    let normalized = state.normalized_opacity_paths();
    assert_eq!(
        normalized
            .iter()
            .filter(|(id, _, _)| *id == second.id)
            .count(),
        0
    );
    let visible = normalized
        .iter()
        .find(|(id, _, _)| *id == first.id)
        .unwrap();
    assert!(
        visible.1.is_empty(),
        "offscreen source kept 24 isolation boundaries"
    );
    state.prepare_opacity_group_targets(0).unwrap();
    assert!(
        state.group_targets[0].is_empty(),
        "offscreen source allocated group scratch"
    );
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let sample = read_scene_pixel(
        gpu,
        state.composite.as_ref().unwrap().image,
        width,
        height,
        500,
        400,
    );
    assert!(
        sample[0] <= 1 && sample[2] > 100 && sample[1] > 30,
        "visible red source did not recompose over green: {sample:?}"
    );
    // The same is true if the source is on-screen but its clipping boundary
    // is entirely outside the viewport, without new drawable submission.
    let mut clipped = second;
    clipped.clip = Some(DirectFrame::from_frame(&appkit::view::Frame {
        x: width as i32 + 20,
        y: 0,
        width: 32,
        height: 32,
        ..full
    }));
    assert!(state.synchronize_scene(window as usize, &[base, first, clipped]));
    assert!(
        state
            .normalized_opacity_paths()
            .iter()
            .all(|(id, _, _)| *id != moved.id)
    );
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert!(state.scene_gpu_bytes() <= 512 * 1024 * 1024);
    drop(state);
    for image in [green, red, blue] {
        gpu.destroy_image(image);
    }
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn omitted_invisible_sibling_preserves_shared_opacity_group_pixels() {
    if isolate_vulkan_graphical_test(
        "omitted_invisible_sibling_preserves_shared_opacity_group_pixels",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Some((window, gpu, mut state, underlay, full)) = v10_fifo_fixture() else {
        return;
    };
    let tile = appkit::view::Frame {
        x: 20,
        y: 20,
        width: 20,
        height: 20,
        ..full
    };
    let red = test_render_target(gpu, 20, 20).unwrap();
    let blue = test_render_target(gpu, 20, 20).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, underlay.image, [0., 1., 0., 1.]);
        cmd_test_clear_color(device, cb, red.image, [0., 0., 1., 1.]);
        cmd_test_clear_color(device, cb, blue.image, [1., 0., 0., 1.]);
    })
    .unwrap();
    let base = test_opaque_metal_placement(0xa100, DirectFrame::from_frame(&full));
    let mut front = test_opaque_metal_placement(0xa101, DirectFrame::from_frame(&tile));
    let mut hidden = test_opaque_metal_placement(0xa102, DirectFrame::from_frame(&tile));
    let mut last = test_opaque_metal_placement(0xa103, DirectFrame::from_frame(&tile));
    for nested in [false, true] {
        let groups = if nested {
            vec![
                appkit::view::OpacityGroup {
                    id: 0xa110,
                    opacity: 0.75,
                },
                appkit::view::OpacityGroup {
                    id: 0xa111,
                    opacity: 0.5,
                },
            ]
        } else {
            vec![appkit::view::OpacityGroup {
                id: 0xa110,
                opacity: 0.5,
            }]
        };
        for item in [&mut front, &mut hidden, &mut last] {
            item.opacity_groups = groups.clone();
        }
        state.layers = vec![base.id, front.id, last.id];
        state.synchronize_scene(
            window as usize,
            &[base.clone(), front.clone(), last.clone()],
        );
        assert_eq!(
            state.stage_composite_source(
                base.id,
                &full,
                PresentSource::from(&underlay),
                None,
                &mut SourceOwner(None),
                false,
                true
            ),
            Ok(Some(true))
        );
        assert_eq!(
            state.stage_composite_source(
                front.id,
                &tile,
                PresentSource::from(&red),
                None,
                &mut SourceOwner(None),
                false,
                true
            ),
            Ok(Some(true))
        );
        assert_eq!(
            state.stage_composite_source(
                last.id,
                &tile,
                PresentSource::from(&blue),
                None,
                &mut SourceOwner(None),
                false,
                true
            ),
            Ok(Some(true))
        );
        state.idle();
        let before = read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            full.window_width,
            full.window_height,
            28,
            28,
        );
        assert!(
            before[0] > 10 && before[1] > 10 && before[2] < 20,
            "grouped blue source didn't cover red: {before:?}"
        );
        for offscreen in [false, true] {
            hidden.opacity = if offscreen { 1.0 } else { 0.0 };
            hidden.frame = if offscreen {
                DirectFrame::from_frame(&appkit::view::Frame {
                    x: full.width as i32 + 20,
                    ..tile
                })
            } else {
                DirectFrame::from_frame(&tile)
            };
            state.layers = vec![base.id, front.id, hidden.id, last.id];
            state.synchronize_scene(
                window as usize,
                &[base.clone(), front.clone(), hidden.clone(), last.clone()],
            );
            let normalized = state.normalized_opacity_paths();
            assert!(!normalized.iter().any(|(id, _, _)| *id == hidden.id));
            assert!(
                !normalized
                    .iter()
                    .find(|(id, _, _)| *id == front.id)
                    .unwrap()
                    .1
                    .is_empty()
            );
            assert_eq!(state.redisplay_scene(window as usize), Ok(true));
            state.idle();
            let after = read_scene_pixel(
                gpu,
                state.composite.as_ref().unwrap().image,
                full.window_width,
                full.window_height,
                28,
                28,
            );
            assert_eq!(
                after, before,
                "excluded sibling split opacity group: nested={nested} offscreen={offscreen}"
            );
        }
    }
    drop(state);
    gpu.destroy_image(red);
    gpu.destroy_image(blue);
    gpu.destroy_image(underlay);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn offscreen_opacity_metadata_preserves_incremental_gpu_repaint() {
    if isolate_vulkan_graphical_test("offscreen_opacity_metadata_preserves_incremental_gpu_repaint")
    {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Some((window, gpu, mut state, green, full)) = v10_fifo_fixture() else {
        return;
    };
    let small = appkit::view::Frame {
        x: 16,
        y: 16,
        width: 12,
        height: 12,
        ..full
    };
    let off = appkit::view::Frame {
        x: full.width as i32 + 200,
        y: 20,
        width: 16,
        height: 16,
        ..full
    };
    let red = test_render_target(gpu, 12, 12).unwrap();
    let blue = test_render_target(gpu, 12, 12).unwrap();
    let offscreen = test_render_target(gpu, 16, 16).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, green.image, [0., 1., 0., 1.]);
        cmd_test_clear_color(device, cb, red.image, [0., 0., 1., 1.]);
        cmd_test_clear_color(device, cb, blue.image, [1., 0., 0., 1.]);
        cmd_test_clear_color(device, cb, offscreen.image, [0., 0., 1., 1.]);
    })
    .unwrap();
    let base = test_opaque_metal_placement(0xc200, DirectFrame::from_frame(&full));
    let front = test_opaque_metal_placement(0xc201, DirectFrame::from_frame(&small));
    let mut idle = test_opaque_metal_placement(0xc202, DirectFrame::from_frame(&off));
    idle.opacity_groups = vec![appkit::view::OpacityGroup {
        id: 0xc300,
        opacity: 0.5,
    }];
    state.layers = vec![base.id, front.id, idle.id];
    state.synchronize_scene(
        window as usize,
        &[base.clone(), front.clone(), idle.clone()],
    );
    assert_eq!(
        state.stage_composite_source(
            base.id,
            &full,
            PresentSource::from(&green),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    assert_eq!(
        state.stage_composite_source(
            front.id,
            &small,
            PresentSource::from(&red),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    assert_eq!(
        state.stage_composite_source(
            idle.id,
            &off,
            PresentSource::from(&offscreen),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    state.idle();
    assert!(
        !state
            .normalized_opacity_paths()
            .iter()
            .any(|(_, path, _)| !path.is_empty())
    );
    assert_eq!(
        state.stage_composite_source(
            front.id,
            &small,
            PresentSource::from(&blue),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    state.idle();
    let repainted = state.last_scene_repaint_pixels.load(Ordering::Relaxed);
    assert!(
        repainted <= small.width as u64 * small.height as u64,
        "invisible group disabled incremental damage: {repainted} pixels"
    );
    let pixel = read_scene_pixel(
        gpu,
        state.composite.as_ref().unwrap().image,
        full.window_width,
        full.window_height,
        20,
        20,
    );
    assert!(
        pixel[0] > 240 && pixel[2] < 10,
        "changed source not displayed {pixel:?}"
    );
    drop(state);
    gpu.destroy_image(green);
    gpu.destroy_image(red);
    gpu.destroy_image(blue);
    gpu.destroy_image(offscreen);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn sparse_seven_level_six_k_visible_groups_render_with_bounded_tiles() {
    if isolate_vulkan_graphical_test(
        "sparse_seven_level_six_k_visible_groups_render_with_bounded_tiles",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let (width, height) = (6016u32, 3384u32);
    let window =
        sdl3::video::create_window(c"sparse 6K seven depth CA", width as i32, height as i32, 0)
            .unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        window,
        vk::Extent2D { width, height },
        surface,
        instance,
    )
    .unwrap();
    let full = test_full_frame(window, width, height);
    let green = test_render_target(gpu, width, height).unwrap();
    let red = test_render_target(gpu, 16, 16).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, green.image, [0., 1., 0., 1.]);
        cmd_test_clear_color(device, cb, red.image, [0., 0., 1., 1.]);
    })
    .unwrap();
    let base = test_opaque_metal_placement(0xb100, DirectFrame::from_frame(&full));
    let mut placements = vec![base.clone()];
    let mut tiles = Vec::new();
    for depth in 0..8usize {
        let (x, y) = if depth % 2 == 0 {
            (20 + (depth / 2) * 40, 20)
        } else {
            (
                width as usize - 200 + (depth / 2) * 40,
                height as usize - 200,
            )
        };
        let rect = appkit::view::Frame {
            x: x as i32,
            y: y as i32,
            width: 16,
            height: 16,
            ..full
        };
        let mut source =
            test_opaque_metal_placement(0xb110 + depth, DirectFrame::from_frame(&rect));
        source.opacity_groups = (0..=depth)
            .map(|n| appkit::view::OpacityGroup {
                id: 0xb200 + n,
                opacity: 0.5,
            })
            .collect();
        placements.push(source);
        tiles.push(rect);
    }
    state.layers = placements.iter().map(|p| p.id).collect();
    state.synchronize_scene(window as usize, &placements);
    assert_eq!(
        state.stage_composite_source(
            base.id,
            &full,
            PresentSource::from(&green),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    for (source, frame) in placements[1..].iter().zip(&tiles) {
        assert_eq!(
            state.stage_composite_source(
                source.id,
                frame,
                PresentSource::from(&red),
                None,
                &mut SourceOwner(None),
                false,
                true
            ),
            Ok(Some(true))
        );
    }
    assert!(
        state.scene_tile_edge > 0,
        "sparse 6K scene was not tiled to avoid false terminal budget"
    );
    assert!(
        state.group_targets[0].len() <= 7,
        "unexpected isolation depth"
    );
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let at = |state: &Swapchain, x: u32, y: u32| {
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            width,
            height,
            x,
            y,
        )
    };
    assert_eq!(
        at(&state, width / 2, height / 2),
        [0, 255, 0, 255],
        "untouched green underlay lost during tiled rendering"
    );
    for (n, frame) in tiles.iter().enumerate() {
        let pixel = at(&state, frame.x as u32 + 5, frame.y as u32 + 5);
        let opacity = 0.5f32.powi(n as i32 + 1);
        assert!(
            (pixel[2] as f32 - 255. * opacity).abs() <= 3.0,
            "tile source {n} expected alpha {opacity}, got {pixel:?}"
        );
    }
    // An aborted GPU recording cannot poison the previous tiled scene's
    // isolation plan, image generations or retained pixels.
    let old_edge = state.scene_tile_edge;
    state.injected_post_scene_record_error = Some(vk::Result::ERROR_OUT_OF_HOST_MEMORY);
    let failed = state.stage_composite_source(
        placements[1].id,
        &tiles[0],
        PresentSource::from(&red),
        None,
        &mut SourceOwner(None),
        false,
        true,
    );
    assert!(
        failed
            .unwrap_err()
            .contains("injected post-scene recording failure")
    );
    assert_eq!(
        state.scene_tile_edge, old_edge,
        "rollback lost tiled scene plan"
    );
    assert!(
        state
            .scene_layers
            .iter()
            .find(|s| s.id == placements[1].id)
            .unwrap()
            .ready
    );
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert!((at(&state, 25, 25)[2] as f32 - 127.5).abs() <= 3.0);

    // Source geometry revision with no new source memory must still advance.
    let mut changed = placements.clone();
    changed[1].frame = DirectFrame::from_frame(&appkit::view::Frame {
        x: 300,
        y: 100,
        ..tiles[0]
    });
    assert!(state.synchronize_scene(window as usize, &changed));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(at(&state, 25, 25), [0, 255, 0, 255]);
    assert!(at(&state, 305, 105)[2] > 100);
    assert!(state.scene_gpu_bytes() <= 512 * 1024 * 1024);
    drop(state);
    gpu.destroy_image(green);
    gpu.destroy_image(red);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn six_k_group_disjoint_damage_keeps_incremental_pixels_and_dispatches() {
    if isolate_vulkan_graphical_test(
        "six_k_group_disjoint_damage_keeps_incremental_pixels_and_dispatches",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let (width, height) = (6016u32, 3384u32);
    let window =
        sdl3::video::create_window(c"6k group disjoint damage", width as i32, height as i32, 0)
            .unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        window,
        vk::Extent2D { width, height },
        surface,
        instance,
    )
    .unwrap();
    let full = test_full_frame(window, width, height);
    let group = appkit::view::Frame {
        x: 20,
        y: 20,
        width: 20,
        height: 20,
        ..full
    };
    let far = appkit::view::Frame {
        x: 5900,
        y: 3200,
        width: 12,
        height: 12,
        ..full
    };
    let green = test_render_target(gpu, width, height).unwrap();
    let red = test_render_target(gpu, 20, 20).unwrap();
    let blue = test_render_target(gpu, 20, 20).unwrap();
    let smallred = test_render_target(gpu, 12, 12).unwrap();
    let smallblue = test_render_target(gpu, 12, 12).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, green.image, [0., 1., 0., 1.]);
        cmd_test_clear_color(device, cb, red.image, [0., 0., 1., 1.]);
        cmd_test_clear_color(device, cb, blue.image, [1., 0., 0., 1.]);
        cmd_test_clear_color(device, cb, smallred.image, [0., 0., 1., 1.]);
        cmd_test_clear_color(device, cb, smallblue.image, [1., 0., 0., 1.]);
    })
    .unwrap();
    let base = test_opaque_metal_placement(0xf000, DirectFrame::from_frame(&full));
    let mut one = test_opaque_metal_placement(0xf001, DirectFrame::from_frame(&group));
    let mut two = test_opaque_metal_placement(0xf002, DirectFrame::from_frame(&group));
    for item in [&mut one, &mut two] {
        item.opacity_groups = vec![appkit::view::OpacityGroup {
            id: 0xf100,
            opacity: 0.5,
        }];
    }
    let distant = test_opaque_metal_placement(0xf003, DirectFrame::from_frame(&far));
    let live = vec![base.clone(), one.clone(), two.clone(), distant.clone()];
    state.layers = live.iter().map(|p| p.id).collect();
    state.synchronize_scene(window as usize, &live);
    for (entry, rect, source) in [
        (&base, &full, PresentSource::from(&green)),
        (&one, &group, PresentSource::from(&red)),
        (&two, &group, PresentSource::from(&blue)),
        (&distant, &far, PresentSource::from(&smallred)),
    ] {
        assert_eq!(
            state.stage_composite_source(
                entry.id,
                rect,
                source,
                None,
                &mut SourceOwner(None),
                false,
                true
            ),
            Ok(Some(true))
        );
    }
    state.idle();
    let sample = |state: &Swapchain, x: u32, y: u32| {
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            width,
            height,
            x,
            y,
        )
    };
    let frozen_group = sample(&state, 25, 25);
    let mut part_calls = 0u64;
    let mut full_calls = 0u64;
    let mut part_ns = 0u128;
    let mut full_ns = 0u128;
    for n in 0..6 {
        let incoming = if n % 2 == 0 { &smallblue } else { &smallred };
        let start = Instant::now();
        assert_eq!(
            state.stage_composite_source(
                distant.id,
                &far,
                PresentSource::from(incoming),
                None,
                &mut SourceOwner(None),
                false,
                true
            ),
            Ok(Some(true))
        );
        state.idle();
        part_ns += start.elapsed().as_nanos();
        let area = state.last_scene_repaint_pixels.load(Ordering::Relaxed);
        assert_eq!(area, 144, "unrelated group forced entire 6K repaint");
        part_calls += state.last_scene_dispatches.load(Ordering::Relaxed);
        let partial_group = sample(&state, 25, 25);
        let partial_far = sample(&state, 5905, 3205);
        assert_eq!(
            partial_group, frozen_group,
            "group not overlapping damage was modified during incremental repaint"
        );
        let start = Instant::now();
        assert_eq!(state.redisplay_scene(window as usize), Ok(true));
        state.idle();
        full_ns += start.elapsed().as_nanos();
        full_calls += state.last_scene_dispatches.load(Ordering::Relaxed);
        assert_eq!(
            state.last_scene_repaint_pixels.load(Ordering::Relaxed),
            width as u64 * height as u64
        );
        assert_eq!(
            sample(&state, 25, 25),
            partial_group,
            "group full-scene reference differs"
        );
        assert_eq!(
            sample(&state, 5905, 3205),
            partial_far,
            "updated source full-scene reference differs"
        );
    }
    assert!(
        part_calls < full_calls,
        "partial repaint failed to save GPU draw dispatches: {part_calls} vs {full_calls}"
    );
    eprintln!(
        "6K grouped remote updates: incremental dispatch={part_calls}, full={full_calls}, incremental mean={}us, full mean={}us",
        part_ns / 6000,
        full_ns / 6000
    );
    drop(state);
    for image in [green, red, blue, smallred, smallblue] {
        gpu.destroy_image(image);
    }
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn dense_six_k_seven_branching_groups_sample_small_textures_and_revise_pixels() {
    if isolate_vulkan_graphical_test(
        "dense_six_k_seven_branching_groups_sample_small_textures_and_revise_pixels",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let (width, height) = (6016u32, 3384u32);
    let window = sdl3::video::create_window(
        c"dense six K tiny GPU sources",
        width as i32,
        height as i32,
        0,
    )
    .unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        window,
        vk::Extent2D { width, height },
        surface,
        instance,
    )
    .unwrap();
    let full = test_full_frame(window, width, height);
    let green = test_render_target(gpu, width, height).unwrap();
    let red = test_render_target(gpu, 16, 16).unwrap();
    let blue = test_render_target(gpu, 16, 16).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, green.image, [0., 1., 0., 1.]);
        cmd_test_clear_color(device, cb, red.image, [0., 0., 1., 1.]);
        cmd_test_clear_color(device, cb, blue.image, [1., 0., 0., 1.]);
    })
    .unwrap();
    let base = test_opaque_metal_placement(0xd000, DirectFrame::from_frame(&full));
    let mut live = vec![base.clone()];
    for depth in 0..8 {
        let mut item = test_opaque_metal_placement(0xd100 + depth, DirectFrame::from_frame(&full));
        item.opacity_groups = (0..=depth)
            .map(|n| appkit::view::OpacityGroup {
                id: 0xd200 + n,
                opacity: 0.5,
            })
            .collect();
        live.push(item);
    }
    state.layers = live.iter().map(|p| p.id).collect();
    state.synchronize_scene(window as usize, &live);
    assert_eq!(
        state.stage_composite_source(
            base.id,
            &full,
            PresentSource::from(&green),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    for item in &live[1..] {
        assert_eq!(
            state.stage_composite_source(
                item.id,
                &full,
                PresentSource::from(&red),
                None,
                &mut SourceOwner(None),
                false,
                true
            ),
            Ok(Some(true))
        );
    }
    state.idle();
    assert_eq!(
        state.group_targets[0].len(),
        7,
        "nested branching target count"
    );
    assert!(
        state.scene_tile_edge > 0,
        "full-viewport frames must not be terminal"
    );
    let sample = |state: &Swapchain, x: u32, y: u32| {
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            width,
            height,
            x,
            y,
        )
    };
    for (x, y) in [(8, 8), (width / 2, height / 2), (width - 10, height - 10)] {
        let p = sample(&state, x, y);
        assert!(
            p[0] <= 2 && (126..=129).contains(&p[1]) && (126..=129).contains(&p[2]) && p[3] == 255,
            "full-window placement did not sample 16x16 red image: {x},{y} {p:?}"
        );
    }
    assert_eq!(
        state.stage_composite_source(
            live[1].id,
            &full,
            PresentSource::from(&blue),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    state.idle();
    let updated = sample(&state, width / 2, height / 2);
    assert!(
        updated[0] > 20 && updated[2] < 120 && updated[3] == 255,
        "changed 16x16 source did not advance full-frame pixels: {updated:?}"
    );
    assert!(state.scene_gpu_bytes() <= 512 * 1024 * 1024);

    // Hundreds of additional distinct visible views occupy one 512px
    // tile, yet descriptor reservation must not multiply them by all 84
    // screen tiles. Each shares the same immutable GPU-owned backing lease.
    let reference = state
        .scene_layers
        .iter()
        .find(|l| l.id == live[2].id)
        .unwrap()
        .clone();
    let mut crowded = live.clone();
    for n in 0..256usize {
        let rect = appkit::view::Frame {
            x: 16 + (n % 16) as i32 * 16,
            y: 16 + (n / 16) as i32 * 16,
            width: 16,
            height: 16,
            ..full
        };
        let mut placement = test_opaque_metal_placement(0xe000 + n, DirectFrame::from_frame(&rect));
        placement.opacity_groups = (0..7)
            .map(|i| appkit::view::OpacityGroup {
                id: 0xd200 + i,
                opacity: 0.5,
            })
            .collect();
        let mut snapshot = reference.clone();
        snapshot.id = placement.id;
        snapshot.frame = DirectFrame::from_frame(&rect);
        snapshot.ready = true;
        state.scene_layers.push(snapshot);
        crowded.push(placement);
    }
    state
        .layers
        .extend(crowded.iter().skip(live.len()).map(|p| p.id));
    state.synchronize_scene(window as usize, &crowded);
    let planned = state.opacity_group_descriptor_count(512).unwrap();
    assert!(
        planned < 12000,
        "tile-local visibility inflated planned descriptors: {planned}"
    );
    state
        .prepare_opacity_group_targets(state.frame % state.frames.len())
        .unwrap();
    let actual = state.scene_blender.as_ref().unwrap().sets[0].len();
    assert!(
        actual <= planned.saturating_mul(2).max(32),
        "descriptor pool grew beyond geometric bound: {actual} vs {planned}"
    );
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert!(sample(&state, width - 10, height - 10)[3] == 255);
    assert!(state.scene_gpu_bytes() <= 512 * 1024 * 1024);
    eprintln!(
        "6K dense 256 crowded views: planned_descriptors={planned}, reserved_per_slot={actual}, scene_gpu_bytes={}",
        state.scene_gpu_bytes()
    );
    drop(state);
    gpu.destroy_image(green);
    gpu.destroy_image(red);
    gpu.destroy_image(blue);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn dense_six_k_group_scene_admits_tiled_plan_without_terminal_refusal() {
    if isolate_vulkan_graphical_test(
        "dense_six_k_group_scene_admits_tiled_plan_without_terminal_refusal",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let width = 6016u32;
    let height = 3384u32;
    let window =
        sdl3::video::create_window(c"irreducible CA scratch", width as i32, height as i32, 0)
            .unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        window,
        vk::Extent2D { width, height },
        surface,
        instance,
    )
    .unwrap();
    let frame = DirectFrame::from_frame(&test_full_frame(window, width, height));
    let mut live = Vec::new();
    // This used to be misclassified as terminal because seven whole-window
    // isolation targets exceed the budget. Its per-pixel work is renderable
    // with bounded tiles regardless of how dense the frame rectangles are.
    for depth in 0..8 {
        live.push(ScenePlacement {
            id: 0x7200 + depth,
            opacity_groups: (0..=depth)
                .map(|n| appkit::view::OpacityGroup {
                    id: 0x6200 + n,
                    opacity: 0.5,
                })
                .collect(),
            frame,
            opacity: 1.0,
            opaque: true,
            clip: None,
            rounded: vec![],
            occluded: Some(vec![]),
            native: None,
        });
    }
    state.layers.extend(live.iter().map(|item| item.id));
    state.scene_placements = live;
    state.scene_blender = Some(SceneBlender::new(gpu, state.window).unwrap());
    state.prepare_opacity_group_targets(0).unwrap();
    assert!(
        state.scene_tile_edge > 0,
        "dense group hierarchy must be tile-admitted"
    );
    assert_eq!(state.group_targets[0].len(), 7);
    assert!(state.scene_gpu_bytes() <= 512 * 1024 * 1024);
    // Collapsing to a unary group permits a non-tiled plan again.
    state.scene_placements.truncate(1);
    state.layers.truncate(1);
    state.prepare_opacity_group_targets(0).unwrap();
    assert!(state.group_targets.iter().all(Vec::is_empty));
    assert_eq!(state.scene_tile_edge, 0);
    drop(state);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn zero_alpha_sparse_six_k_groups_never_block_visible_metal_changes() {
    if isolate_vulkan_graphical_test(
        "zero_alpha_sparse_six_k_groups_never_block_visible_metal_changes",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let (width, height) = (6016u32, 3384u32);
    let window = sdl3::video::create_window(
        c"zero alpha sparse CA group",
        width as i32,
        height as i32,
        0,
    )
    .unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        window,
        vk::Extent2D { width, height },
        surface,
        instance,
    )
    .unwrap();
    let full = test_full_frame(window, width, height);
    let tile = appkit::view::Frame {
        x: 320,
        y: 320,
        width: 16,
        height: 16,
        ..full
    };
    let background = test_render_target(gpu, width, height).unwrap();
    let colored = test_render_target(gpu, 16, 16).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, background.image, [0., 1., 0., 1.]);
        cmd_test_clear_color(device, cb, colored.image, [1., 0., 0., 1.]);
    })
    .unwrap();

    let base = test_opaque_metal_placement(0x9000, DirectFrame::from_frame(&full));
    let mut visible = test_opaque_metal_placement(0x9001, DirectFrame::from_frame(&tile));
    let mut placements = vec![base.clone(), visible.clone()];
    for depth in 0..8 {
        let spot = appkit::view::Frame {
            x: if depth % 2 == 0 { 0 } else { 6000 },
            y: if depth % 2 == 0 { 0 } else { 3368 },
            width: 16,
            height: 16,
            ..full
        };
        placements.push(ScenePlacement {
            id: 0x9100 + depth,
            opacity_groups: (0..=depth)
                .map(|n| appkit::view::OpacityGroup {
                    id: 0x9200 + n,
                    opacity: if n == 0 { 0.0 } else { 0.5 },
                })
                .collect(),
            frame: DirectFrame::from_frame(&spot),
            opacity: 1.0,
            opaque: true,
            clip: None,
            rounded: vec![],
            occluded: Some(vec![]),
            native: None,
        });
    }
    state.layers.extend(placements.iter().map(|p| p.id));
    state.synchronize_scene(window as usize, &placements);
    state.prepare_opacity_group_targets(0).unwrap();
    assert!(state.group_targets.iter().all(Vec::is_empty));
    assert_eq!(
        state.stage_composite_source(
            base.id,
            &full,
            PresentSource::from(&background),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    assert_eq!(
        state.stage_composite_source(
            visible.id,
            &tile,
            PresentSource::from(&colored),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    for placement in placements.iter().skip(2) {
        assert_eq!(
            state.stage_composite_source(
                placement.id,
                &placement.frame.frame(),
                PresentSource::from(&colored),
                None,
                &mut SourceOwner(None),
                false,
                true
            ),
            Ok(Some(true))
        );
    }
    state.idle();
    let read = |state: &Swapchain, x, y| {
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            width,
            height,
            x,
            y,
        )
    };
    assert_eq!(read(&state, 8, 8), [0, 255, 0, 255]);
    assert_eq!(read(&state, 6008, 3376), [0, 255, 0, 255]);
    assert_eq!(read(&state, 328, 328), [255, 0, 0, 255]);
    visible.opacity = 0.5;
    placements[1] = visible;
    assert!(state.synchronize_scene(window as usize, &placements));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let actual = read(&state, 328, 328);
    assert!(
        actual[0].abs_diff(127) <= 1
            && actual[1].abs_diff(127) <= 1
            && actual[2] == 0
            && actual[3] == 255,
        "unrelated visible Metal change was blocked by zero-alpha scene: {actual:?}"
    );
    assert!(state.group_targets.iter().all(Vec::is_empty));
    assert!(state.scene_gpu_bytes() <= 512 * 1024 * 1024);

    drop(state);
    gpu.destroy_image(colored);
    gpu.destroy_image(background);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn two_overlapping_full_six_k_sources_inside_twenty_four_ancestors_progress() {
    if isolate_vulkan_graphical_test(
        "two_overlapping_full_six_k_sources_inside_twenty_four_ancestors_progress",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    long_lived_large_group_reuses_completed_scratch(
        6016,
        3384,
        24,
        false,
        (0, 0),
        false,
        true,
        0.95,
        true,
    );
}

#[test]
#[cfg(not(target_os = "macos"))]
fn four_k_nested_groups_progress_through_more_than_eight_submitted_frames() {
    if isolate_vulkan_graphical_test(
        "four_k_nested_groups_progress_through_more_than_eight_submitted_frames",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    long_lived_large_group_reuses_completed_scratch(
        3840,
        2160,
        2,
        true,
        (0, 0),
        false,
        false,
        0.5,
        false,
    );
}

/// The source order comes from a real AppKit hierarchy rather than a
/// hand-constructed group. Parent NSView alphaValue must attenuate the
/// opaque blue Metal child *after* it hides the opaque red Metal child.
#[test]
#[cfg(not(target_os = "macos"))]
fn nsview_parent_alpha_groups_two_opaque_metal_children() {
    if isolate_vulkan_graphical_test("nsview_parent_alpha_groups_two_opaque_metal_children") {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    crate::bootstrap();
    use foundation::geometry::{NSPoint, NSRect, NSSize};
    use objc::object::Id;
    let rect = |width, height| NSRect {
        origin: NSPoint { x: 0., y: 0. },
        size: NSSize { width, height },
    };
    let window = objc::object::create_instance(
        objc::class::lookup(c"NSWindow").unwrap(),
        appkit::window::INSTANCE_SIZE,
    );
    let initialize: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect, usize, usize, u8) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    assert_eq!(
        unsafe {
            initialize(
                window,
                objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
                rect(80., 60.),
                0,
                0,
                0,
            )
        },
        window
    );
    let make_view = || {
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let init: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { init(view, objc::sel::get("initWithFrame:"), rect(80., 60.)) }
    };
    let make_layer = |class: &str| {
        let class = objc::class::lookup(std::ffi::CString::new(class).unwrap().as_c_str()).unwrap();
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { make(class as *const _ as Id, objc::sel::get("layer")) }
    };
    let set: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let set_alpha: unsafe extern "C" fn(Id, objc::sel::Sel, f64) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let parent = make_view();
    let lower_view = make_view();
    let upper_view = make_view();
    let lower = make_layer("CAMetalLayer");
    let upper = make_layer("CAMetalLayer");
    unsafe {
        set(window, objc::sel::get("setContentView:"), parent);
        set(lower_view, objc::sel::get("setLayer:"), lower);
        set(upper_view, objc::sel::get("setLayer:"), upper);
        set(parent, objc::sel::get("addSubview:"), lower_view);
        set(parent, objc::sel::get("addSubview:"), upper_view);
        set_alpha(parent, objc::sel::get("setAlphaValue:"), 0.5);
    }
    let sdl = unsafe { appkit::window::sdl_window(window) };
    appkit::event::pump(std::ptr::null_mut());
    let observed = appkit::view::metal_scene(sdl).unwrap();
    assert_eq!(
        observed
            .iter()
            .map(|source| source.layer)
            .collect::<Vec<_>>(),
        [lower as usize, upper as usize]
    );
    assert!(observed.iter().all(|source| source.opacity == 1.0
        && source.opacity_groups
            == [appkit::view::OpacityGroup {
                id: parent as usize,
                opacity: 0.5
            }]));
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    sdl3::video::presentation_window(sdl).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(sdl, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        sdl,
        vk::Extent2D {
            width: 80,
            height: 60,
        },
        surface,
        instance,
    )
    .unwrap();
    let red = test_render_target(gpu, 80, 60).unwrap();
    let blue = test_render_target(gpu, 80, 60).unwrap();
    for (image, rgba) in [(&red, [0., 0., 1., 1.]), (&blue, [1., 0., 0., 1.])] {
        gpu.one_shot(|device, cb| unsafe {
            cmd_test_clear_color(device, cb, image.image, rgba);
        })
        .unwrap();
    }
    let proposal = |items: Vec<appkit::view::MetalSceneLayer>| {
        items
            .into_iter()
            .map(|item| ScenePlacement {
                id: item.layer,
                frame: DirectFrame::from_frame(&item.frame),
                opacity_groups: item.opacity_groups,
                opacity: item.opacity,
                opaque: item.opaque,
                clip: item.clip.map(|clip| DirectFrame::from_frame(&clip)),
                rounded: vec![],
                occluded: Some(vec![]),
                native: native_scene_source(
                    item.native_pixels,
                    item.native_dmabuf,
                    item.native_revision,
                    item.frame.width,
                    item.frame.height,
                    item.native_cpu_capture,
                    item.native_dma_sample,
                ),
            })
            .collect::<Vec<_>>()
    };
    let initial = proposal(observed);
    state.layers.extend([lower as usize, upper as usize]);
    state.synchronize_scene(sdl as usize, &initial);
    let whole = test_full_frame(sdl, 80, 60);
    for (id, image) in [(lower as usize, &red), (upper as usize, &blue)] {
        assert_eq!(
            state.stage_composite_layer(id, &whole, image, None, &mut SourceOwner(None), false),
            Ok(Some(true))
        );
    }
    state.synchronize_scene(sdl as usize, &initial);
    assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
    state.idle();
    let sample = |state: &Swapchain| {
        read_scene_pixel(gpu, state.composite.as_ref().unwrap().image, 80, 60, 40, 30)
    };
    assert!(
        sample(&state)
            .iter()
            .zip([128u8, 0, 0, 128])
            .all(|(a, b)| a.abs_diff(b) <= 1),
        "lower opaque red Metal child bled through blue upper child: {:?}",
        sample(&state)
    );
    unsafe {
        set_alpha(parent, objc::sel::get("setAlphaValue:"), 0.25);
    }
    appkit::event::pump(std::ptr::null_mut());
    let quarter = proposal(appkit::view::metal_scene(sdl).unwrap());
    assert!(state.synchronize_scene(sdl as usize, &quarter));
    assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
    state.idle();
    assert!(
        sample(&state)
            .iter()
            .zip([64u8, 0, 0, 64])
            .all(|(a, b)| a.abs_diff(b) <= 1),
        "metadata-only NSView alpha update failed to update static Metal sources"
    );
    // A zero-opacity ancestor still closes an isolated subtree; children
    // must not render simply because a 0% alpha failed the fractional check.
    unsafe {
        set_alpha(parent, objc::sel::get("setAlphaValue:"), 0.0);
    }
    appkit::event::pump(std::ptr::null_mut());
    let zero = proposal(appkit::view::metal_scene(sdl).unwrap());
    assert!(zero.iter().all(|source| source.opacity_groups
        == [appkit::view::OpacityGroup {
            id: parent as usize,
            opacity: 0.0
        }]));
    assert!(state.synchronize_scene(sdl as usize, &zero));
    assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
    state.idle();
    assert_eq!(sample(&state), [0, 0, 0, 0]);
    unsafe {
        set_alpha(parent, objc::sel::get("setAlphaValue:"), 1.0);
    }
    appkit::event::pump(std::ptr::null_mut());
    let opaque = proposal(appkit::view::metal_scene(sdl).unwrap());
    assert!(opaque.iter().all(|source| source.opacity_groups.is_empty()));
    assert!(state.synchronize_scene(sdl as usize, &opaque));
    assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
    state.idle();
    assert_eq!(sample(&state), [255, 0, 0, 255]);

    drop(state);
    gpu.destroy_image(red);
    gpu.destroy_image(blue);
    objc::arc::release(window);
}

/// A single *partial* CAMetalLayer cannot use the FIFO flattened path:
/// after a move or resize it must sample its original image-space source.
/// Adding a second layer must not reinterpret that snapshot as a full-window
/// source or lose its original image dimensions.
#[test]
#[cfg(not(target_os = "macos"))]
fn partial_single_layer_moves_resizes_and_gains_sibling_without_new_drawable() {
    if isolate_vulkan_graphical_test(
        "partial_single_layer_moves_resizes_and_gains_sibling_without_new_drawable",
    ) {
        return;
    }
    let Some((window, gpu, mut state, background, whole)) = v10_fifo_fixture() else {
        return;
    };
    let foreground = test_render_target(gpu, 40, 30).expect("40x30 source image");
    for (image, rgba) in [
        (&background, [0.0, 0.0, 1.0, 1.0]),
        (&foreground, [0.0, 1.0, 0.0, 1.0]),
    ] {
        gpu.one_shot(|device, cb| unsafe {
            cmd_test_clear_color(device, cb, image.image, rgba);
        })
        .expect("paint source image");
    }
    let id = 10usize;
    let sibling = 11usize;
    let initial = appkit::view::Frame {
        x: 10,
        y: 20,
        width: 40,
        height: 30,
        ..whole
    };
    state.layers.push(id);
    state.synchronize_scene(
        window as usize,
        &[ScenePlacement {
            opacity_groups: Vec::new(),
            id,
            frame: DirectFrame::from_frame(&initial),
            opacity: 1.0,
            opaque: true,
            clip: None,
            rounded: Vec::new(),
            occluded: None,
            native: None,
        }],
    );
    assert_eq!(
        state.stage_composite_layer(
            id,
            &initial,
            &foreground,
            None,
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.note_scene_frame(id, &initial);
    assert_eq!(
        state.scene_layers.len(),
        1,
        "a partial FIFO layer was flattened"
    );
    assert_eq!(
        (
            state.scene_layers[0].snapshot.image.width,
            state.scene_layers[0].snapshot.image.height
        ),
        (40, 30)
    );
    state.idle();
    let pixel = |state: &Swapchain, x, y| {
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            whole.window_width,
            whole.window_height,
            x,
            y,
        )
    };
    assert_eq!(pixel(&state, 15, 25), [0, 255, 0, 255]);
    assert_eq!(pixel(&state, 75, 25), [0, 0, 0, 0]);
    let moved = appkit::view::Frame {
        x: 65,
        y: 45,
        ..initial
    };
    assert!(state.synchronize_scene(
        window as usize,
        &[ScenePlacement {
            opacity_groups: Vec::new(),
            id,
            frame: DirectFrame::from_frame(&moved),
            opacity: 1.0,
            opaque: true,
            clip: None,
            rounded: Vec::new(),
            occluded: None,
            native: None,
        }]
    ));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        pixel(&state, 15, 25),
        [0, 0, 0, 0],
        "old placement left a ghost"
    );
    assert_eq!(
        pixel(&state, 75, 55),
        [0, 255, 0, 255],
        "moved source missing"
    );
    let resized = appkit::view::Frame {
        x: 65,
        y: 45,
        width: 60,
        height: 40,
        ..initial
    };
    assert!(state.synchronize_scene(
        window as usize,
        &[ScenePlacement {
            opacity_groups: Vec::new(),
            id,
            frame: DirectFrame::from_frame(&resized),
            opacity: 1.0,
            opaque: true,
            clip: None,
            rounded: Vec::new(),
            occluded: None,
            native: None,
        }]
    ));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        pixel(&state, 119, 79),
        [0, 255, 0, 255],
        "resize did not sample source"
    );
    assert_eq!(pixel(&state, 126, 85), [0, 0, 0, 0]);
    // A new blue background arrives underneath, while the moved foreground
    // does not submit another drawable. It must remain sharp and correctly
    // positioned rather than become a stretched 160x120 flattened image.
    state.layers.push(sibling);
    assert!(state.synchronize_scene(
        window as usize,
        &[
            ScenePlacement {
                opacity_groups: Vec::new(),
                id: sibling,
                frame: DirectFrame::from_frame(&whole),
                opacity: 1.0,
                opaque: true,
                clip: None,
                rounded: Vec::new(),
                occluded: None,
                native: None,
            },
            ScenePlacement {
                opacity_groups: Vec::new(),
                id,
                frame: DirectFrame::from_frame(&resized),
                opacity: 1.0,
                opaque: true,
                clip: None,
                rounded: Vec::new(),
                occluded: None,
                native: None,
            },
        ]
    ));
    assert_eq!(
        state.stage_composite_layer(
            sibling,
            &whole,
            &background,
            None,
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.note_scene_frame(sibling, &whole);
    state.idle();
    assert_eq!(pixel(&state, 5, 5), [0, 0, 255, 255]);
    assert_eq!(pixel(&state, 75, 55), [0, 255, 0, 255]);
    assert_eq!(pixel(&state, 119, 79), [0, 255, 0, 255]);
    assert_eq!(
        (
            state
                .scene_layers
                .iter()
                .find(|l| l.id == id)
                .unwrap()
                .snapshot
                .image
                .width,
            state
                .scene_layers
                .iter()
                .find(|l| l.id == id)
                .unwrap()
                .snapshot
                .image
                .height
        ),
        (40, 30)
    );
    drop(state);
    gpu.destroy_image(background);
    gpu.destroy_image(foreground);
    sdl3::video::destroy_window(window);
}

/// Descriptor growth is per-frame and must survive 33+ independent source
/// snapshots without recycling descriptor sets still in Vulkan submissions.
#[test]
#[cfg(not(target_os = "macos"))]
fn more_than_32_retained_layers_preserve_topmost_pixels() {
    if isolate_vulkan_graphical_test("more_than_32_retained_layers_preserve_topmost_pixels") {
        return;
    }
    let Some((window, gpu, mut state, blue, whole)) = v10_fifo_fixture() else {
        return;
    };
    let small = test_render_target(gpu, 8, 8).expect("test overlay image");
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, blue.image, [0.0, 0.0, 1.0, 1.0]);
        cmd_test_clear_color(device, cb, small.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .expect("seed two colors");
    state.layers.push(1);
    assert_eq!(
        state.stage_composite_layer(1, &whole, &blue, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.note_scene_frame(1, &whole);
    // 33 small independently snapshotted Metal views plus a CPU-like full
    // background: exactly the boundary where the old fixed pool failed.
    for i in 0..33usize {
        let id = 2 + i;
        let small_frame = appkit::view::Frame {
            x: (i as i32 % 11) * 12,
            y: 10 + (i as i32 / 11) * 18,
            width: 8,
            height: 8,
            ..whole
        };
        state.layers.push(id);
        assert_eq!(
            state.stage_composite_layer(
                id,
                &small_frame,
                &small,
                None,
                &mut SourceOwner(None),
                false
            ),
            Ok(Some(true)),
            "layer id {id}"
        );
        state.note_scene_frame(id, &small_frame);
    }
    state.idle();
    assert_eq!(state.layers.len(), 34);
    assert!(
        state
            .scene_blender
            .as_ref()
            .unwrap()
            .sets
            .iter()
            .all(|sets| sets.len() >= 34)
    );
    let image = state.composite.as_ref().unwrap().image;
    assert_eq!(
        read_scene_pixel(gpu, image, whole.window_width, whole.window_height, 7, 7),
        [0, 0, 255, 255]
    );
    assert_eq!(
        read_scene_pixel(gpu, image, whole.window_width, whole.window_height, 4, 14),
        [0, 255, 0, 255]
    );
    assert_eq!(
        read_scene_pixel(gpu, image, whole.window_width, whole.window_height, 124, 50),
        [0, 255, 0, 255]
    );
    // Updating only the first 8x8 foreground exercises damaged-rectangle
    // clearing/composition with >32 layer descriptors. Other snapshots and
    // the full-window backing image must retain their previous pixels.
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, small.image, [1.0, 0.0, 0.0, 1.0]);
    })
    .expect("repaint small foreground");
    let affected = appkit::view::Frame {
        x: 0,
        y: 10,
        width: 8,
        height: 8,
        ..whole
    };
    assert_eq!(
        state.stage_composite_layer(2, &affected, &small, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.idle();
    assert_eq!(
        read_scene_pixel(gpu, image, whole.window_width, whole.window_height, 4, 14),
        [255, 0, 0, 255],
        "damaged overlay was not repainted"
    );
    assert_eq!(
        read_scene_pixel(gpu, image, whole.window_width, whole.window_height, 16, 14),
        [0, 255, 0, 255],
        "unmodified neighboring overlay was lost"
    );
    assert_eq!(
        read_scene_pixel(gpu, image, whole.window_width, whole.window_height, 7, 7),
        [0, 0, 255, 255],
        "unchanged background outside damage was modified"
    );
    drop(state);
    gpu.destroy_image(blue);
    gpu.destroy_image(small);
    sdl3::video::destroy_window(window);
}

/// FIFO previously flattened an opaque full-window source. A CA-only mask
/// change must capture that source and rebuild the retained compositor too.
#[test]
#[cfg(not(target_os = "macos"))]
fn fifo_flattened_static_drawable_recomposes_after_ca_mask_change() {
    if isolate_vulkan_graphical_test(
        "fifo_flattened_static_drawable_recomposes_after_ca_mask_change",
    ) {
        return;
    }
    let Some((window, gpu, mut state, source, frame)) = v10_fifo_fixture() else {
        return;
    };
    if !matches!(
        state.present_mode,
        vk::PresentModeKHR::FIFO | vk::PresentModeKHR::FIFO_RELAXED
    ) {
        gpu.destroy_image(source);
        drop(state);
        sdl3::video::destroy_window(window);
        return;
    }
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, source.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .expect("green FIFO source");
    let id = 613;
    let base = ScenePlacement {
        opacity_groups: Vec::new(),
        id,
        frame: DirectFrame::from_frame(&frame),
        opacity: 1.0,
        opaque: true,
        clip: None,
        rounded: vec![],
        occluded: None,
        native: None,
    };
    state.layers.push(id);
    state.synchronize_scene(window as usize, std::slice::from_ref(&base));
    assert_eq!(
        state.stage_composite_layer(id, &frame, &source, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.note_scene_frame(id, &frame);
    state.idle();
    assert!(
        state.scene_layers.is_empty(),
        "FIFO fixture did not flatten"
    );
    let masked = ScenePlacement {
        opacity_groups: Vec::new(),
        rounded: vec![scene::RoundedClip {
            frame: base.frame,
            radius: 32.0,
        }],
        ..base
    };
    assert!(state.synchronize_scene(window as usize, &[masked]));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            frame.width,
            frame.height,
            0,
            0
        ),
        [0, 0, 0, 0]
    );
    assert_eq!(
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            frame.width,
            frame.height,
            80,
            60
        ),
        [0, 255, 0, 255]
    );
    drop(state);
    gpu.destroy_image(source);
    sdl3::video::destroy_window(window);
}

/// The sRGB view's sampler decodes RGB to linear floats. Retained scene
/// composition uses display-referred RGBA8 bytes; it must encode the sampled
/// sRGB source back before source-over blending into a UNORM accumulator.
#[test]
#[cfg(not(target_os = "macos"))]
fn srgb_midtone_and_premultiplied_alpha_retain_encoded_scene_colors() {
    if isolate_vulkan_graphical_test(
        "srgb_midtone_and_premultiplied_alpha_retain_encoded_scene_colors",
    ) {
        return;
    }
    let Some((window, gpu, mut state, bottom, whole)) = v10_fifo_fixture() else {
        return;
    };
    let top = gpu
        .create_image(&ImageDescription {
            image_type: vk::ImageType::TYPE_2D,
            view_type: vk::ImageViewType::TYPE_2D,
            format: vk::Format::B8G8R8A8_SRGB,
            width: whole.width,
            height: whole.height,
            depth: 1,
            levels: 1,
            layers: 1,
            samples: 1,
            usage: gpu::USAGE_RENDER_TARGET | gpu::USAGE_SHADER_READ,
            cube: false,
            components: vk::ComponentMapping::default(),
        })
        .expect("sRGB Metal source");
    let bytes = u64::from(whole.width) * u64::from(whole.height) * 4;
    let upload = gpu.create_buffer(bytes).expect("raw sRGB texels");
    // B8G8R8A8: premultiplied source RGB=(96,64,32), alpha=128.
    // This nontrivial encoded midtone must not be mistaken for the sampler's
    // decoded linear components.
    for i in 0..(bytes as usize / 4) {
        unsafe {
            std::ptr::copy_nonoverlapping(
                [32u8, 64, 96, 128].as_ptr(),
                upload.contents.add(i * 4),
                4,
            )
        };
    }
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(
            device,
            cb,
            bottom.image,
            [10.0 / 255.0, 50.0 / 255.0, 180.0 / 255.0, 1.0],
        );
        device.cmd_copy_buffer_to_image(
            cb,
            upload.buffer,
            top.image,
            vk::ImageLayout::GENERAL,
            &[vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D {
                    width: whole.width,
                    height: whole.height,
                    depth: 1,
                })],
        );
    })
    .expect("upload midtone SRGB image");
    gpu.destroy_buffer(upload);
    let background = 611;
    let foreground = 612;
    state.layers = vec![background, foreground];
    state.synchronize_scene(
        window as usize,
        &[
            ScenePlacement {
                opacity_groups: Vec::new(),
                id: background,
                frame: DirectFrame::from_frame(&whole),
                opacity: 1.0,
                opaque: true,
                clip: None,
                rounded: vec![],
                occluded: None,
                native: None,
            },
            ScenePlacement {
                opacity_groups: Vec::new(),
                id: foreground,
                frame: DirectFrame::from_frame(&whole),
                opacity: 1.0,
                opaque: false,
                clip: None,
                rounded: vec![],
                occluded: None,
                native: None,
            },
        ],
    );
    assert_eq!(
        state.stage_composite_layer(
            background,
            &whole,
            &bottom,
            None,
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.note_scene_frame(background, &whole);
    assert_eq!(
        state.stage_composite_layer(
            foreground,
            &whole,
            &top,
            None,
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.note_scene_frame(foreground, &whole);
    state.idle();
    let pixel = read_scene_pixel(
        gpu,
        state.composite.as_ref().unwrap().image,
        whole.width,
        whole.height,
        80,
        60,
    );
    // 96 + 10*(127/255), 64 + 50*(127/255), 32 + 180*(127/255).
    for (actual, expected) in pixel.iter().zip([101u8, 89, 122, 255]) {
        assert!(
            (i32::from(*actual) - i32::from(expected)).abs() <= 2,
            "sRGB source-over mismatch: pixel={pixel:?}"
        );
    }
    drop(state);
    gpu.destroy_image(bottom);
    gpu.destroy_image(top);
    sdl3::video::destroy_window(window);
}

/// Independent native CPU pixels must be composited *above* or *below*
/// a static Metal drawable according to AppKit order, including alpha blend
/// and later geometry removal without another Metal drawable submission.
#[test]
#[cfg(not(target_os = "macos"))]
fn independent_translucent_native_scene_respects_metal_order_and_motion() {
    if isolate_vulkan_graphical_test(
        "independent_translucent_native_scene_respects_metal_order_and_motion",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, metal.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .unwrap();
    let metal_id = 900usize;
    let native_id = (1usize << (usize::BITS - 1)) | 1234;
    let native_frame = appkit::view::Frame {
        x: 40,
        y: 35,
        width: 30,
        height: 25,
        ..whole
    };
    let red: Vec<u8> =
        [0u8, 0, 128, 128].repeat((native_frame.width * native_frame.height) as usize);
    let native_source = scene::NativeCpuSceneSource {
        cpu_capture: None,
        bytes: Arc::new(red),
        revision: 1,
        dmabuf: None,
        dma_sample: None,
    };
    let metal_placement = test_opaque_metal_placement(metal_id, DirectFrame::from_frame(&whole));
    let native_placement = ScenePlacement {
        opacity_groups: Vec::new(),
        id: native_id,
        frame: DirectFrame::from_frame(&native_frame),
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(native_source),
    };
    state.layers.push(metal_id);
    state.synchronize_scene(
        window as usize,
        &[metal_placement.clone(), native_placement.clone()],
    );
    assert_eq!(
        state.stage_composite_layer(
            metal_id,
            &whole,
            &metal,
            None,
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.note_scene_frame(metal_id, &whole);
    assert_eq!(
        stage_native_scene_source(&mut state, &native_placement),
        Ok(true)
    );
    state.synchronize_scene(
        window as usize,
        &[metal_placement.clone(), native_placement.clone()],
    );
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let pixel = |state: &Swapchain, x, y| {
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            whole.width,
            whole.height,
            x,
            y,
        )
    };
    let above = pixel(&state, 45, 40);
    for (actual, expected) in above.iter().zip([128u8, 127, 0, 255]) {
        assert!(
            (i32::from(*actual) - i32::from(expected)).abs() <= 2,
            "native alpha was not blended over metal: {above:?}"
        );
    }
    assert_eq!(pixel(&state, 90, 50), [0, 255, 0, 255]);
    // The very same native source *below* an opaque Metal drawable is hidden.
    assert!(state.synchronize_scene(
        window as usize,
        &[native_placement.clone(), metal_placement.clone()]
    ));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(pixel(&state, 45, 40), [0, 255, 0, 255]);
    // Return to above, then move the native view without changing its bytes.
    assert!(state.synchronize_scene(
        window as usize,
        &[metal_placement.clone(), native_placement.clone()]
    ));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let moved_frame = appkit::view::Frame {
        x: 80,
        y: 60,
        ..native_frame
    };
    let moved = ScenePlacement {
        opacity_groups: Vec::new(),
        frame: DirectFrame::from_frame(&moved_frame),
        ..native_placement.clone()
    };
    assert!(state.synchronize_scene(window as usize, &[metal_placement.clone(), moved]));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(pixel(&state, 45, 40), [0, 255, 0, 255]);
    let relocated = pixel(&state, 90, 70);
    assert!(
        (i32::from(relocated[0]) - 128).abs() <= 2 && (i32::from(relocated[1]) - 127).abs() <= 2
    );
    // A DMA-BUF's revision can remain unchanged while AppKit resizes its
    // native placement. Source dimensions must still be reuploaded.
    let resized_frame = appkit::view::Frame {
        width: 20,
        height: 18,
        ..moved_frame
    };
    let resized = ScenePlacement {
        opacity_groups: Vec::new(),
        frame: DirectFrame::from_frame(&resized_frame),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new(
                [0u8, 0, 128, 128].repeat((resized_frame.width * resized_frame.height) as usize),
            ),
            revision: 1,
            dmabuf: None,
            dma_sample: None,
        }),
        ..native_placement
    };
    state.synchronize_scene(window as usize, &[metal_placement.clone(), resized.clone()]);
    assert_eq!(stage_native_scene_source(&mut state, &resized), Ok(true));
    state.synchronize_scene(window as usize, &[metal_placement.clone(), resized.clone()]);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let image = &state
        .scene_layers
        .iter()
        .find(|s| s.id == native_id)
        .unwrap()
        .snapshot
        .image;
    assert_eq!(
        (image.width, image.height),
        (resized_frame.width, resized_frame.height)
    );
    assert!(i32::from(pixel(&state, 90, 70)[0]).abs_diff(128) <= 2);
    assert!(state.synchronize_scene(window as usize, &[metal_placement]));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    // With only one opaque Metal source left, IMMEDIATE/MAILBOX may restore
    // direct WSI instead of rewriting the now-unreferenced accumulator.
    let displayed = state
        .direct
        .as_ref()
        .map_or(state.composite.as_ref().unwrap().image, |direct| {
            direct.source.image
        });
    assert_eq!(
        read_scene_pixel(gpu, displayed, whole.width, whole.height, 90, 70),
        [0, 255, 0, 255]
    );
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// An intermediate CALayer above a static CAMetalLayer animates its own
/// position and bounds. The AppKit event pump publishes changing CA clips
/// even though Metal submits no new drawable; Vulkan final pixels track them.
#[test]
#[cfg(not(target_os = "macos"))]
fn animated_geometry_mask_recomposes_static_metal_gpu_pixels() {
    if isolate_vulkan_graphical_test("animated_geometry_mask_recomposes_static_metal_gpu_pixels") {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    appkit::bootstrap();
    use foundation::geometry::{NSPoint, NSRect, NSSize};
    use objc::object::Id;
    let rect = |x, y, width, height| NSRect {
        origin: NSPoint { x, y },
        size: NSSize { width, height },
    };
    let window = objc::object::create_instance(
        objc::class::lookup(c"NSWindow").unwrap(),
        appkit::window::INSTANCE_SIZE,
    );
    let init: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect, usize, usize, u8) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    assert_eq!(
        unsafe {
            init(
                window,
                objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
                rect(0.0, 0.0, 120.0, 90.0),
                0,
                0,
                0,
            )
        },
        window
    );
    let sdl = unsafe { appkit::window::sdl_window(window) };
    assert!(!sdl.is_null());
    let mk_view = |width, height| {
        let v = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let initialize: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            initialize(
                v,
                objc::sel::get("initWithFrame:"),
                rect(0.0, 0.0, width, height),
            )
        }
    };
    let content = mk_view(120.0, 90.0);
    let metal_view = mk_view(120.0, 90.0);
    let set: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let layer = |class: &str| {
        let cls = objc::class::lookup(std::ffi::CString::new(class).unwrap().as_c_str()).unwrap();
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { make(cls as *const _ as Id, objc::sel::get("layer")) }
    };
    let root = layer("CALayer");
    let mask = layer("CALayer");
    let metal_layer = layer("CAMetalLayer");
    let set_frame: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let set_bool: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe {
        set(window, objc::sel::get("setContentView:"), content);
        set(content, objc::sel::get("setLayer:"), root);
        set(metal_view, objc::sel::get("setLayer:"), metal_layer);
        set(content, objc::sel::get("addSubview:"), metal_view);
        set_frame(
            mask,
            objc::sel::get("setFrame:"),
            rect(9.0, 8.0, 38.0, 25.0),
        );
        set(root, objc::sel::get("addSublayer:"), mask);
        set(mask, objc::sel::get("addSublayer:"), metal_layer);
        set_bool(mask, objc::sel::get("setMasksToBounds:"), 1);
    }
    appkit::event::pump(std::ptr::null_mut());
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    sdl3::video::presentation_window(sdl).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(sdl, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        sdl,
        vk::Extent2D {
            width: 120,
            height: 90,
        },
        surface,
        instance,
    )
    .unwrap();
    let source = test_render_target(gpu, 120, 90).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, source.image, [0.0, 1.0, 0.0, 1.0])
    })
    .unwrap();
    let initial = appkit::view::metal_scene(sdl).expect("static Metal published");
    let metal_id = metal_layer as usize;
    let physical = test_full_frame(sdl, 120, 90);
    let scene_from_ca = || {
        let entry = appkit::view::metal_scene(sdl)
            .unwrap()
            .into_iter()
            .find(|s| s.layer == metal_id)
            .expect("static CAMetalLayer in AppKit tree");
        let clip = entry
            .clip
            .expect("intermediate CA mask clip projected onto Metal");
        (
            ScenePlacement {
                opacity_groups: Vec::new(),
                id: metal_id,
                frame: DirectFrame::from_frame(&entry.frame),
                opacity: entry.opacity,
                opaque: true,
                clip: Some(DirectFrame::from_frame(&clip)),
                rounded: vec![],
                occluded: Some(vec![]),
                native: None,
            },
            clip,
        )
    };
    assert!(initial.iter().any(|s| s.layer == metal_id));
    let (initial_p, initial_clip) = scene_from_ca();
    state.layers.push(metal_id);
    state.synchronize_scene(sdl as usize, std::slice::from_ref(&initial_p));
    assert_eq!(
        state.stage_composite_source(
            metal_id,
            &physical,
            PresentSource::from(&source),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    state.synchronize_scene(sdl as usize, &[initial_p]);
    assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
    state.idle();
    let comp = state.composite.as_ref().unwrap().image;
    let check_clip = |state: &Swapchain, clip: &appkit::view::Frame| {
        let image = state.composite.as_ref().unwrap().image;
        let cx = (clip.x + clip.width as i32 / 2).clamp(0, 119) as u32;
        let cy = (clip.y + clip.height as i32 / 2).clamp(0, 89) as u32;
        assert_eq!(
            read_scene_pixel(gpu, image, 120, 90, cx, cy),
            [0, 255, 0, 255],
            "static Metal pixels missing inside animated CA mask"
        );
        assert_eq!(
            read_scene_pixel(gpu, image, 120, 90, 0, 0),
            [0, 0, 0, 0],
            "static Metal appeared outside intermediate ancestor clip"
        );
    };
    check_clip(&state, &initial_clip);
    let anim_cls = objc::class::lookup(c"CABasicAnimation").unwrap();
    let mk: unsafe extern "C" fn(Id, objc::sel::Sel, Id) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let set_seconds: unsafe extern "C" fn(Id, objc::sel::Sel, f64) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let add: unsafe extern "C" fn(Id, objc::sel::Sel, Id, Id) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let animate = |path: &str, start: f64, end: f64| {
        let key = foundation::string::from_str(path);
        let obj = unsafe {
            mk(
                anim_cls as *const _ as Id,
                objc::sel::get("animationWithKeyPath:"),
                key,
            )
        };
        let number = |x: f64| {
            corefoundation::collections::new_number(corefoundation::collections::Number::Float(x))
                as Id
        };
        let from = number(start);
        let to = number(end);
        unsafe {
            set(obj, objc::sel::get("setFromValue:"), from);
            set(obj, objc::sel::get("setToValue:"), to);
            set_seconds(obj, objc::sel::get("setDuration:"), 0.30);
            set_seconds(
                obj,
                objc::sel::get("setBeginTime:"),
                darwin::mach::media_time() - 0.15,
            );
            add(mask, objc::sel::get("addAnimation:forKey:"), obj, key);
        }
        objc::arc::release(from);
        objc::arc::release(to);
        objc::arc::release(key);
    };
    animate("position.x", 30.0, 65.0);
    animate("bounds.size.width", 32.0, 75.0);
    appkit::event::pump(std::ptr::null_mut());
    let (middle_p, middle_clip) = scene_from_ca();
    assert!(middle_clip.x != initial_clip.x || middle_clip.width != initial_clip.width);
    state.synchronize_scene(sdl as usize, &[middle_p]);
    assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
    state.idle();
    check_clip(&state, &middle_clip);
    std::thread::sleep(Duration::from_millis(30));
    appkit::event::pump(std::ptr::null_mut());
    let (later_p, later_clip) = scene_from_ca();
    assert!(later_clip.x != middle_clip.x || later_clip.width != middle_clip.width);
    state.synchronize_scene(sdl as usize, &[later_p]);
    assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
    state.idle();
    check_clip(&state, &later_clip);
    std::thread::sleep(Duration::from_millis(160));
    appkit::event::pump(std::ptr::null_mut());
    let (final_p, final_clip) = scene_from_ca();
    assert_eq!(
        (
            final_clip.x,
            final_clip.y,
            final_clip.width,
            final_clip.height
        ),
        (
            initial_clip.x,
            initial_clip.y,
            initial_clip.width,
            initial_clip.height
        ),
        "finished geometry animation failed to restore static drawable's model-state clipping"
    );
    state.synchronize_scene(sdl as usize, &[final_p]);
    assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
    state.idle();
    check_clip(&state, &final_clip);
    assert_eq!(
        state.composite.as_ref().unwrap().image,
        comp,
        "CA clipping unexpectedly replaced whole static GPU accumulator"
    );
    drop(state);
    gpu.destroy_image(source);
    objc::arc::release(window);
}

/// A real 6016x3384 opaque Metal window remains visible beneath the entire
/// CPU-backed native foreground. Each source allocation stays <=64MiB; all
/// final pixels are read from the actual Vulkan window accumulator, including
/// seams and later static-Metal membership changes.
#[test]
#[cfg(not(target_os = "macos"))]
fn fully_visible_six_k_cpu_native_tiles_overlay_static_opaque_metal() {
    if isolate_vulkan_graphical_test(
        "fully_visible_six_k_cpu_native_tiles_overlay_static_opaque_metal",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    const WIDTH: u32 = 6016;
    const HEIGHT: u32 = 3384;
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let window = sdl3::video::create_window(
        c"six-k tiled native above Metal",
        WIDTH as i32,
        HEIGHT as i32,
        0,
    )
    .unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        window,
        vk::Extent2D {
            width: WIDTH,
            height: HEIGHT,
        },
        surface,
        instance,
    )
    .expect("real 6K Vulkan WSI surface");
    assert!(uses_layer_compositor(state.present_mode));
    let source = test_render_target(gpu, WIDTH, HEIGHT).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, source.image, [0.0, 1.0, 0.0, 1.0])
    })
    .unwrap();
    let whole = test_full_frame(window, WIDTH, HEIGHT);
    let metal_id = 301usize;
    let base = test_opaque_metal_placement(metal_id, DirectFrame::from_frame(&whole));
    state.layers.push(metal_id);
    state.synchronize_scene(window as usize, std::slice::from_ref(&base));
    assert_eq!(
        state.stage_composite_source(
            metal_id,
            &whole,
            PresentSource::from(&source),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    state.idle();
    assert!(
        state
            .scene_layers
            .iter()
            .any(|layer| layer.id == metal_id && layer.ready),
        "static Metal source not retained ahead of native tile staging"
    );
    let mut placements = vec![base.clone()];
    let mut tile_no = 0usize;
    for y in (0..HEIGHT).step_by(2048) {
        for x in (0..WIDTH).step_by(2048) {
            let tw = (WIDTH - x).min(2048);
            let th = (HEIGHT - y).min(2048);
            let tile = ScenePlacement {
                opacity_groups: Vec::new(),
                id: (3usize << (usize::BITS - 2)) | (tile_no + 1),
                frame: DirectFrame::from_frame(&appkit::view::Frame {
                    x: x as i32,
                    y: y as i32,
                    width: tw,
                    height: th,
                    ..whole
                }),
                opacity: 1.0,
                opaque: false,
                clip: None,
                rounded: vec![],
                occluded: Some(vec![]),
                native: Some(scene::NativeCpuSceneSource {
                    cpu_capture: None,
                    bytes: Arc::new([0u8, 0, 128, 128].repeat(tw as usize * th as usize)),
                    revision: 91 + tile_no as u64,
                    dmabuf: None,
                    dma_sample: None,
                }),
            };
            assert!(tile.native.as_ref().unwrap().bytes.len() <= 64 * 1024 * 1024);
            assert_eq!(stage_native_scene_source(&mut state, &tile), Ok(true));
            placements.push(tile);
            tile_no += 1;
        }
    }
    assert!(tile_no >= 6);
    state.synchronize_scene(window as usize, &placements);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let image = state.composite.as_ref().unwrap().image;
    for (x, y) in [
        (0, 0),
        (2047, 2047),
        (2048, 2048),
        (4095, 2047),
        (4096, 2048),
        (6015, 3383),
    ] {
        let rgba = read_scene_pixel(gpu, image, WIDTH, HEIGHT, x, y);
        assert!(
            (i32::from(rgba[0]) - 128).abs() <= 2
                && (i32::from(rgba[1]) - 127).abs() <= 2
                && rgba[3] == 255,
            "tile seam or foreground at ({x},{y}) fell behind opaque Metal: {rgba:?}"
        );
    }
    // Drop the bottom-right tile, leaving the last Metal drawable static.
    // A fresh compositing pass must reveal the original green pixel there,
    // never retain stale native coverage from an old scene.
    let removed = placements.pop().unwrap();
    assert_eq!(removed.frame.x, 4096);
    assert_eq!(removed.frame.y, 2048);
    state.synchronize_scene(window as usize, &placements);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        read_scene_pixel(gpu, image, WIDTH, HEIGHT, 5000, 3000),
        [0, 255, 0, 255],
        "removing final tile erased or corrupted the static opaque Metal source"
    );
    drop(state);
    gpu.destroy_image(source);
    sdl3::video::destroy_window(window);
}

/// An atomic replacement of twelve independently retained native images
/// must make forward progress with both prior and new generations alive.
/// These old sources exceed the number of GPU submission frame slots.
#[test]
#[cfg(not(target_os = "macos"))]
fn twelve_retained_native_replacements_commit_with_old_generations_pinned() {
    if isolate_vulkan_graphical_test(
        "twelve_retained_native_replacements_commit_with_old_generations_pinned",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, metal.image, [0.0, 1.0, 0.0, 1.0])
    })
    .unwrap();
    let metal_id = 905usize;
    let background = test_opaque_metal_placement(metal_id, DirectFrame::from_frame(&whole));
    state.layers.push(metal_id);
    state.synchronize_scene(window as usize, std::slice::from_ref(&background));
    assert_eq!(
        state.stage_composite_source(
            metal_id,
            &whole,
            PresentSource::from(&metal),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    let mut old = vec![background.clone()];
    for n in 0..12u64 {
        let x = 5 + (n % 4) * 33;
        let y = 6 + (n / 4) * 31;
        let red = ScenePlacement {
            opacity_groups: Vec::new(),
            id: (1usize << (usize::BITS - 1)) | (n as usize + 0x400),
            frame: DirectFrame::from_frame(&appkit::view::Frame {
                x: x as i32,
                y: y as i32,
                width: 20,
                height: 18,
                ..whole
            }),
            opacity: 1.0,
            opaque: false,
            clip: None,
            rounded: vec![],
            occluded: Some(vec![]),
            native: Some(scene::NativeCpuSceneSource {
                cpu_capture: None,
                bytes: Arc::new([0u8, 0, 255, 255].repeat(20 * 18)),
                revision: 1,
                dmabuf: None,
                dma_sample: None,
            }),
        };
        assert_eq!(stage_native_scene_source(&mut state, &red), Ok(true));
        old.push(red);
    }
    state.synchronize_scene(window as usize, &old);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert!(state.scene_layers.len() > state.frames.len());
    let before = state.scene_gpu_bytes();
    state.transaction_pinned = state
        .scene_layers
        .iter()
        .map(|layer| Arc::clone(&layer.snapshot))
        .collect();
    let mut updated = old.clone();
    for entry in &mut updated[1..] {
        let changed = entry.native.as_mut().unwrap();
        changed.bytes = Arc::new([255u8, 0, 0, 255].repeat(20 * 18));
        changed.revision = 2;
        assert_eq!(stage_native_scene_source(&mut state, entry), Ok(true));
    }
    assert!(
        state.scene_gpu_bytes() > before,
        "replaced source generations pinned solely for rollback disappeared from accounting"
    );
    state.synchronize_scene(window as usize, &updated);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    for entry in &updated[1..] {
        let x = (entry.frame.x + 5) as u32;
        let y = (entry.frame.y + 5) as u32;
        assert_eq!(
            read_scene_pixel(
                gpu,
                state.composite.as_ref().unwrap().image,
                whole.width,
                whole.height,
                x,
                y
            ),
            [0, 0, 255, 255],
            "one of twelve new native GPU generations failed to commit"
        );
    }
    let pinned_footprint = state.scene_gpu_bytes();
    state.transaction_pinned.clear();
    state.idle();
    state.shed_scene_spares();
    let after = state.scene_gpu_bytes();
    assert!(
        after < pinned_footprint,
        "commit did not release transaction-only GPU generations"
    );
    assert!(
        after <= before.saturating_add(12 * 20 * 18 * 4),
        "committed 12 native replacements retained too many old GPU generations"
    );
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// Twelve replaced scene generations may outlive the eight frame slots
/// solely through transaction rollback pins. They must remain budgeted until
/// the entire batch commits/rolls back, then capacity must recover.
#[test]
#[cfg(not(target_os = "macos"))]
fn twelve_transaction_pinned_gpu_generations_count_toward_scene_budget() {
    if isolate_vulkan_graphical_test(
        "twelve_transaction_pinned_gpu_generations_count_toward_scene_budget",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let before = state.scene_gpu_bytes();
    let dimensions = (2048u32, 2048u32);
    let one = u64::from(dimensions.0) * u64::from(dimensions.1) * 4;
    let desc = ImageDescription {
        image_type: vk::ImageType::TYPE_2D,
        view_type: vk::ImageViewType::TYPE_2D,
        format: vk::Format::B8G8R8A8_UNORM,
        width: dimensions.0,
        height: dimensions.1,
        depth: 1,
        levels: 1,
        layers: 1,
        samples: 1,
        usage: gpu::USAGE_RENDER_TARGET,
        cube: false,
        components: vk::ComponentMapping::default(),
    };
    for _ in 0..12 {
        let pinned = BackingImageLease::create_for_window(gpu, window as usize, &desc)
            .expect("twelve distinct committed old generations");
        state.transaction_pinned.push(pinned);
    }
    assert!(state.transaction_pinned.len() > state.frames.len());
    assert_eq!(
        state.scene_gpu_bytes(),
        before + 12 * one,
        "rollback-only GPU snapshots vanished from the 512MiB budget"
    );
    // Identity-based dedup: an old scene and its transaction pin are one
    // backing allocation, never twice the physical bytes.
    state
        .transaction_pinned
        .push(Arc::clone(&state.transaction_pinned[0]));
    assert_eq!(state.scene_gpu_bytes(), before + 12 * one);
    let future = 350 * 1024 * 1024u64;
    assert!(
        !scene::fits_scene_budget(state.scene_gpu_bytes(), future),
        "a batch overcommitted the 512MiB retained-scene budget"
    );
    state.transaction_pinned.clear();
    assert_eq!(state.scene_gpu_bytes(), before);
    assert!(
        scene::fits_scene_budget(state.scene_gpu_bytes(), future),
        "after old transaction leases retire, a valid replacement must fit"
    );
    // Progress also requires the GPU to accept new pixels, not merely a
    // successful arithmetic budget check.
    let placement = ScenePlacement {
        opacity_groups: Vec::new(),
        id: (1usize << (usize::BITS - 1)) | 0x201,
        frame: DirectFrame::from_frame(&appkit::view::Frame {
            x: 16,
            y: 14,
            width: 32,
            height: 24,
            ..whole
        }),
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new([0u8, 0, 255, 255].repeat(32 * 24)),
            revision: 1,
            dmabuf: None,
            dma_sample: None,
        }),
    };
    assert_eq!(stage_native_scene_source(&mut state, &placement), Ok(true));
    assert_eq!(
        state
            .scene_layers
            .iter()
            .find(|s| s.id == placement.id)
            .unwrap()
            .native_revision,
        Some(1)
    );
    state.idle();
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// A native UI layer appearing after a static *direct WSI* Metal frame must
/// force a retained snapshot of that direct source before uploading the UI.
#[test]
#[cfg(not(target_os = "macos"))]
fn native_overlay_joins_last_direct_wsi_image_without_new_metal_drawable() {
    if isolate_vulkan_graphical_test(
        "native_overlay_joins_last_direct_wsi_image_without_new_metal_drawable",
    ) {
        return;
    }
    let Some((window, gpu, mut state, green, whole)) = v10_fifo_fixture() else {
        return;
    };
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, green.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .unwrap();
    let metal_id = 991usize;
    let metal_placement = test_opaque_metal_placement(metal_id, DirectFrame::from_frame(&whole));
    state.layers.push(metal_id);
    state.synchronize_scene(window as usize, std::slice::from_ref(&metal_placement));
    state.queue_direct(
        metal_id,
        &whole,
        PresentSource::from(&green),
        &mut SourceOwner(None),
        None,
    );
    assert!(state.direct.is_some());
    // The production present path also records the original Metal frame.
    state.note_scene_frame(metal_id, &whole);
    let child_frame = appkit::view::Frame {
        x: 40,
        y: 35,
        width: 30,
        height: 25,
        ..whole
    };
    let native = ScenePlacement {
        opacity_groups: Vec::new(),
        id: (1usize << (usize::BITS - 1)) | 992,
        frame: DirectFrame::from_frame(&child_frame),
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new(
                [0u8, 0, 128, 128].repeat((child_frame.width * child_frame.height) as usize),
            ),
            revision: 1,
            dmabuf: None,
            dma_sample: None,
        }),
    };
    state.synchronize_scene(window as usize, &[metal_placement, native.clone()]);
    assert_eq!(stage_native_scene_source(&mut state, &native), Ok(true));
    state.synchronize_scene(
        window as usize,
        &[
            test_opaque_metal_placement(metal_id, DirectFrame::from_frame(&whole)),
            native,
        ],
    );
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let composite = state.composite.as_ref().unwrap();
    assert_eq!(
        read_scene_pixel(gpu, composite.image, whole.width, whole.height, 90, 50),
        [0, 255, 0, 255],
        "native source discarded the static WSI drawable during transition"
    );
    let native_pixel = read_scene_pixel(gpu, composite.image, whole.width, whole.height, 45, 40);
    assert!(
        (i32::from(native_pixel[0]) - 128).abs() <= 2
            && (i32::from(native_pixel[1]) - 127).abs() <= 2,
        "native overlay was not source-over green: {native_pixel:?}"
    );
    drop(state);
    gpu.destroy_image(green);
    sdl3::video::destroy_window(window);
}

/// Rounded native view clips must not turn the region behind it into a hole
/// in the static opaque Metal drawable. The layer shader must draw the native
/// source only within its rounded mask, revealing green Metal at the corners.
#[test]
#[cfg(not(target_os = "macos"))]
fn rounded_native_opaque_overlay_preserves_metal_corners() {
    if isolate_vulkan_graphical_test("rounded_native_opaque_overlay_preserves_metal_corners") {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, metal.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .expect("static green metal source");
    let metal_layer = 0x313;
    let native_layer = (1usize << (usize::BITS - 1)) | 0x314;
    let bottom = test_opaque_metal_placement(metal_layer, DirectFrame::from_frame(&whole));
    let native = ScenePlacement {
        opacity_groups: Vec::new(),
        id: native_layer,
        frame: DirectFrame::from_frame(&whole),
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![scene::RoundedClip {
            frame: DirectFrame::from_frame(&whole),
            radius: 35.0,
        }],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new([0u8, 0, 255, 255].repeat((whole.width * whole.height) as usize)),
            revision: 1,
            dmabuf: None,
            dma_sample: None,
        }),
    };
    state.layers.push(metal_layer);
    state.synchronize_scene(window as usize, &[bottom.clone(), native.clone()]);
    assert_eq!(
        state.stage_composite_layer(
            metal_layer,
            &whole,
            &metal,
            None,
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.note_scene_frame(metal_layer, &whole);
    assert_eq!(stage_native_scene_source(&mut state, &native), Ok(true));
    state.synchronize_scene(window as usize, &[bottom, native]);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let image = state.composite.as_ref().unwrap().image;
    assert_eq!(
        read_scene_pixel(gpu, image, whole.width, whole.height, 0, 0),
        [0, 255, 0, 255],
        "rounded native corner incorrectly erased underlying metal"
    );
    assert_eq!(
        read_scene_pixel(gpu, image, whole.width, whole.height, 80, 60),
        [255, 0, 0, 255],
        "opaque native center should cover Metal"
    );
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

#[test]
fn large_native_dma_source_requires_no_event_thread_pixel_allocation() {
    let picture = Arc::new(appkit::backing::Dmabuf {
        width: 6016,
        height: 3384,
        fourcc: 0,
        modifier: 0,
        planes: vec![],
    });
    let admitted = native_scene_source(None, Some(Arc::clone(&picture)), 7, 6016, 3384, None, None)
        .expect("6K DMA-BUF-only sources must bypass the 64 MiB CPU cap");
    assert!(admitted.bytes.is_empty());
    assert!(Arc::ptr_eq(admitted.dmabuf.as_ref().unwrap(), &picture));
    assert_eq!(admitted.revision, 7);
    assert!(native_scene_source(None, Some(picture), 7, 9000, 9000, None, None).is_none());
}

#[cfg(target_os = "linux")]
fn real_gbm_picture_with_pixels(
    width: u32,
    height: u32,
    color: impl Fn(u32, u32) -> [u8; 4],
) -> Arc<appkit::backing::Dmabuf> {
    let Some(importer) = crate::overlay::importer() else {
        panic!("the Vulkan adapter cannot import DMA-BUFs");
    };
    // Pick the exact render node used by the presenter, not a buffer owned by
    // another physical GPU which would fail cross-device import.
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::MetadataExt;
    let node = ["/dev/dri/renderD128", "/dev/dri/renderD129"]
        .into_iter()
        .find(|path| std::fs::metadata(path).is_ok_and(|meta| meta.rdev() == importer.device))
        .expect("no matching DRM render node for the Vulkan importer");
    // Resolve GBM dynamically to keep portable builds independent of the
    // distribution's libgbm development package.
    unsafe {
        use std::ffi::{c_int, c_void};
        let handle = libc::dlopen(c"libgbm.so.1".as_ptr(), libc::RTLD_NOW);
        assert!(!handle.is_null(), "libgbm runtime unavailable");
        unsafe fn symbol<T>(handle: *mut c_void, name: &std::ffi::CStr) -> T {
            let raw = unsafe { libc::dlsym(handle, name.as_ptr()) };
            assert!(!raw.is_null(), "GBM missing symbol {name:?}");
            unsafe { std::mem::transmute_copy(&raw) }
        }
        let create_device: unsafe extern "C" fn(c_int) -> *mut c_void =
            symbol(handle, c"gbm_create_device");
        let destroy_device: unsafe extern "C" fn(*mut c_void) =
            symbol(handle, c"gbm_device_destroy");
        let create_bo: unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32) -> *mut c_void =
            symbol(handle, c"gbm_bo_create");
        let destroy_bo: unsafe extern "C" fn(*mut c_void) = symbol(handle, c"gbm_bo_destroy");
        let bo_fd: unsafe extern "C" fn(*mut c_void) -> c_int = symbol(handle, c"gbm_bo_get_fd");
        let bo_modifier: unsafe extern "C" fn(*mut c_void) -> u64 =
            symbol(handle, c"gbm_bo_get_modifier");
        let bo_map: unsafe extern "C" fn(
            *mut c_void,
            u32,
            u32,
            u32,
            u32,
            u32,
            *mut u32,
            *mut *mut c_void,
        ) -> *mut u8 = symbol(handle, c"gbm_bo_map");
        let bo_unmap: unsafe extern "C" fn(*mut c_void, *mut c_void) =
            symbol(handle, c"gbm_bo_unmap");
        let fd = libc::open(
            std::ffi::CString::new(node).unwrap().as_ptr(),
            libc::O_RDWR | libc::O_CLOEXEC,
        );
        assert!(fd >= 0, "could not open render node {node}");
        let device = create_device(fd);
        assert!(!device.is_null(), "GBM device creation failed");
        // Linear AR24 is the portable format for this pixel oracle. No CPU
        // zeroed 6K image is ever handed to AppKit or the Metal worker.
        let bo = create_bo(device, width, height, u32::from_le_bytes(*b"AR24"), 16);
        assert!(!bo.is_null(), "GBM rejected 6K linear AR24");
        let modifier = bo_modifier(bo);
        assert!(
            importer
                .formats
                .contains(&(u32::from_le_bytes(*b"AR24"), modifier)),
            "Vulkan importer does not support GBM modifier {modifier:#x}"
        );
        let mut stride = 0u32;
        let mut cookie = std::ptr::null_mut();
        let pixels = bo_map(bo, 0, 0, width, height, 2, &mut stride, &mut cookie);
        assert!(
            !pixels.is_null() && stride >= width * 4,
            "GBM 6K buffer not CPU mappable"
        );
        let bytes = std::slice::from_raw_parts_mut(pixels, stride as usize * height as usize);
        for (y, row) in bytes.chunks_exact_mut(stride as usize).enumerate() {
            for (x, pixel) in row
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .take(width as usize)
                .enumerate()
            {
                pixel.copy_from_slice(&color(x as u32, y as u32));
            }
        }
        bo_unmap(bo, cookie);
        let exported = bo_fd(bo);
        assert!(exported >= 0, "GBM failed to export real DMA-BUF");
        // The returned fd independently pins the image after GBM teardown.
        destroy_bo(bo);
        destroy_device(device);
        libc::close(fd);
        libc::dlclose(handle);

        Arc::new(appkit::backing::Dmabuf {
            width,
            height,
            fourcc: u32::from_le_bytes(*b"AR24"),
            modifier,
            planes: vec![appkit::backing::DmabufPlane {
                fd: std::os::fd::OwnedFd::from_raw_fd(exported),
                offset: 0,
                stride,
            }],
        })
    }
}

/// Six fully visible CPU-backed tiles and a translucent 6K real DMA-BUF
/// must composite in source order. Transparent producer texels reveal CPU,
/// never the opaque Metal image behind that native foreground.
#[test]
#[cfg(target_os = "linux")]
fn six_k_mixed_cpu_and_translucent_gbm_dma_preserves_native_background() {
    if isolate_vulkan_graphical_test(
        "six_k_mixed_cpu_and_translucent_gbm_dma_preserves_native_background",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    const WIDTH: u32 = 6016;
    const HEIGHT: u32 = 3384;
    let producer_damage = std::env::var("ROSEMARY_BENCH_NATIVE_MIXED_6K_PRODUCER_DAMAGE")
        .is_ok_and(|value| value == "1");
    let picture = real_gbm_picture_with_pixels(WIDTH, HEIGHT, |x, _| {
        if x < WIDTH / 2 {
            [0, 0, 128, 128]
        } else {
            [0, 0, 0, 0]
        }
    });
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let window =
        sdl3::video::create_window(c"6K mixed CPU DMA", WIDTH as i32, HEIGHT as i32, 0).unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        window,
        vk::Extent2D {
            width: WIDTH,
            height: HEIGHT,
        },
        surface,
        instance,
    )
    .unwrap();
    let source = test_render_target(gpu, WIDTH, HEIGHT).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, source.image, [0.0, 1.0, 0.0, 1.0])
    })
    .unwrap();
    let whole = test_full_frame(window, WIDTH, HEIGHT);
    let bottom = test_opaque_metal_placement(0x2458, DirectFrame::from_frame(&whole));
    state.layers.push(bottom.id);
    state.synchronize_scene(window as usize, std::slice::from_ref(&bottom));
    assert_eq!(
        state.stage_composite_source(
            bottom.id,
            &whole,
            PresentSource::from(&source),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    let mut scene = vec![bottom.clone()];
    let before_native = state.frame;
    let admission_start = std::time::Instant::now();
    let mut count = 0usize;
    for y in (0..HEIGHT).step_by(2048) {
        for x in (0..WIDTH).step_by(2048) {
            let tw = (WIDTH - x).min(2048);
            let th = (HEIGHT - y).min(2048);
            let tile = ScenePlacement {
                opacity_groups: Vec::new(),
                id: (3usize << (usize::BITS - 2)) | (count + 1),
                frame: DirectFrame::from_frame(&appkit::view::Frame {
                    x: x as i32,
                    y: y as i32,
                    width: tw,
                    height: th,
                    ..whole
                }),
                opacity: 1.0,
                opaque: false,
                clip: None,
                rounded: vec![],
                occluded: Some(vec![]),
                native: Some(scene::NativeCpuSceneSource {
                    cpu_capture: producer_damage.then_some(appkit::backing::NativeCpuCapture {
                        revision: 7 + count as u64,
                        damage: None,
                    }),
                    bytes: Arc::new([255, 0, 0, 255].repeat(tw as usize * th as usize)),
                    revision: 7 + count as u64,
                    dmabuf: Some(Arc::clone(&picture)),
                    dma_sample: Some(scene::DmaSample {
                        full_width: WIDTH,
                        full_height: HEIGHT,
                        crop_x: x,
                        crop_y: y,
                    }),
                }),
            };
            assert!(tile.native.as_ref().unwrap().bytes.len() <= 64 * 1024 * 1024);
            assert_eq!(
                stage_native_scene_source(&mut state, &tile),
                Ok(true),
                "mixed tile failed import or scene staging"
            );
            scene.push(tile);
            count += 1;
        }
    }
    assert_eq!(count, 6);
    assert_eq!(
        state.frame, before_native,
        "six native tiles unexpectedly submitted six independent retained-scene snapshots"
    );
    let first_six_tiles_ms = admission_start.elapsed().as_secs_f64() * 1000.0;
    state.synchronize_scene(window as usize, &scene);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let initial_wsi_start = std::time::Instant::now();
    state
        .flush_composite(window)
        .expect("real initial mixed 6K WSI presentation");
    let initial_wsi_ms = initial_wsi_start.elapsed().as_secs_f64() * 1000.0;
    if std::env::var_os("ROSEMARY_BENCH_NATIVE_MIXED_6K").is_some() {
        eprintln!(
            "6K mixed real GPU mode={:?} initial_six_tiles_ms={first_six_tiles_ms:.3} first_wsi_ms={initial_wsi_ms:.3}",
            state.present_mode
        );
        let repetitions = std::env::var("ROSEMARY_BENCH_NATIVE_MIXED_6K_REVISIONS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(3)
            .clamp(3, 64);
        let pace = std::env::var("ROSEMARY_BENCH_NATIVE_MIXED_6K_PACE_HZ")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|hz| (1..=1000).contains(hz))
            .map(|hz| std::time::Duration::from_secs_f64(1. / f64::from(hz)));
        let change_cpu = std::env::var("ROSEMARY_BENCH_NATIVE_MIXED_6K_CHANGE_CPU")
            .is_ok_and(|value| value == "1");
        let cadence_start = std::time::Instant::now();
        let mut missed_deadlines = 0usize;
        let mut stage_times = Vec::with_capacity(repetitions);
        let mut cpu_rebuild_times = Vec::with_capacity(repetitions);
        let mut native_source_times = Vec::with_capacity(repetitions);
        let mut complete_times = Vec::with_capacity(repetitions);
        for n in 0..repetitions {
            if let Some(interval) = pace {
                let due = cadence_start + interval.mul_f64(n as f64);
                let now = std::time::Instant::now();
                if now < due {
                    std::thread::sleep(due - now);
                } else if n != 0 {
                    missed_deadlines += 1;
                }
            }
            let animation_start = std::time::Instant::now();
            // Worker discards expendable scene snapshot spares before every
            // transaction; immutable CPU backing caches must survive.
            state.shed_scene_snapshot_spares();
            let mut cpu_ms = 0.;
            let mut native_ms = 0.;
            for tile in &mut scene[1..] {
                let cpu_start = std::time::Instant::now();
                let native = tile.native.as_mut().unwrap();
                native.revision += 1;
                if change_cpu {
                    let mut pixels = (*native.bytes).clone();
                    // Source backing truly changes on each update. Alter a
                    // center texel, not a tile boundary sampled by the test.
                    let w = tile.frame.width as usize;
                    let h = tile.frame.height as usize;
                    let at = ((h / 2) * w + (w / 2)) * 4;
                    pixels[at] = (n as u8) ^ 1;
                    native.bytes = Arc::new(pixels);
                    if producer_damage {
                        let prev = native.cpu_capture.unwrap().revision;
                        native.cpu_capture = Some(appkit::backing::NativeCpuCapture {
                            revision: prev + 1,
                            damage: Some(appkit::backing::NativeCpuDamage {
                                from_revision: prev,
                                rect: appkit::backing::Rect {
                                    x: (w / 2) as i64,
                                    y: (h / 2) as i64,
                                    width: 1,
                                    height: 1,
                                },
                            }),
                        });
                    }
                }
                cpu_ms += cpu_start.elapsed().as_secs_f64() * 1000.0;
                let native_start = std::time::Instant::now();
                assert_eq!(stage_native_scene_source(&mut state, tile), Ok(true));
                native_ms += native_start.elapsed().as_secs_f64() * 1000.0;
            }
            let six_tiles_ms = animation_start.elapsed().as_secs_f64() * 1000.0;
            stage_times.push(six_tiles_ms);
            cpu_rebuild_times.push(cpu_ms);
            native_source_times.push(native_ms);
            state.synchronize_scene(window as usize, &scene);
            assert_eq!(state.redisplay_scene(window as usize), Ok(true));
            state.idle();
            let wsi_start = std::time::Instant::now();
            // Diagnostic: omit repeated FIFO presentations to isolate native
            // staging from a full present queue. Never treat that timing as
            // end-to-end WSI latency or a guest frame-rate measurement.
            if std::env::var_os("ROSEMARY_BENCH_NATIVE_MIXED_6K_SKIP_WSI").is_none() {
                state
                    .flush_composite(window)
                    .expect("animated mixed 6K WSI publication");
            }
            let wsi_ms = wsi_start.elapsed().as_secs_f64() * 1000.0;
            complete_times.push(animation_start.elapsed().as_secs_f64() * 1000.0);
            eprintln!(
                "6K mixed real GPU mode={:?} revision={} six_tiles_ms={six_tiles_ms:.3} first_wsi_ms={wsi_ms:.3} total_ms={:.3}",
                state.present_mode,
                n + 1,
                animation_start.elapsed().as_secs_f64() * 1000.0
            );
        }
        stage_times.sort_by(f64::total_cmp);
        let p95 = stage_times[(stage_times.len() * 95 / 100).min(stage_times.len() - 1)];
        let avg = stage_times.iter().sum::<f64>() / stage_times.len() as f64;
        cpu_rebuild_times.sort_by(f64::total_cmp);
        native_source_times.sort_by(f64::total_cmp);
        let cpu_mean = cpu_rebuild_times.iter().sum::<f64>() / repetitions as f64;
        let native_mean = native_source_times.iter().sum::<f64>() / repetitions as f64;
        let cpu_p95 = cpu_rebuild_times[(repetitions * 95 / 100).min(repetitions - 1)];
        let native_p95 = native_source_times[(repetitions * 95 / 100).min(repetitions - 1)];
        complete_times.sort_by(f64::total_cmp);
        let end_to_end_p95 =
            complete_times[(complete_times.len() * 95 / 100).min(complete_times.len() - 1)];
        eprintln!(
            "6K mixed real GPU mode={:?} revisions={} paced={} cpu_changed={} producer_damage={} missed_deadlines={} staging_mean_ms={avg:.3} staging_p95_ms={p95:.3} staging_max_ms={:.3} stage_compose_wsi_p95_ms={end_to_end_p95:.3} cpu_source_mean_ms={cpu_mean:.3} cpu_source_p95_ms={cpu_p95:.3} native_stage_mean_ms={native_mean:.3} native_stage_p95_ms={native_p95:.3}",
            state.present_mode,
            repetitions,
            pace.is_some(),
            change_cpu,
            producer_damage,
            missed_deadlines,
            stage_times.last().unwrap()
        );
    }
    let image = state.composite.as_ref().unwrap().image;
    if std::env::var("ROSEMARY_BENCH_NATIVE_MIXED_6K_PRODUCER_DAMAGE")
        .is_ok_and(|value| value == "1")
        && std::env::var("ROSEMARY_BENCH_NATIVE_MIXED_6K_CHANGE_CPU")
            .is_ok_and(|value| value == "1")
    {
        let count = std::env::var("ROSEMARY_BENCH_NATIVE_MIXED_6K_REVISIONS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(3)
            .clamp(3, 64);
        let latest_blue = ((count - 1) as u8) ^ 1;
        let over = read_scene_pixel(gpu, image, WIDTH, HEIGHT, 1024, 1024);
        let opaque = read_scene_pixel(gpu, image, WIDTH, HEIGHT, 5056, 2716);
        assert!(
            (i16::from(over[2]) - i16::from(latest_blue / 2)).abs() <= 2,
            "partial CPU upload did not change translucent DMA texel: {over:?}"
        );
        assert!(
            (i16::from(opaque[2]) - i16::from(latest_blue)).abs() <= 2,
            "partial CPU upload did not change transparent DMA texel: {opaque:?}"
        );
        assert_eq!(opaque[3], 255);
    }
    // DMA: premultiplied half-red over opaque CPU blue => purple. Fully
    // transparent texels preserve CPU blue rather than exposing Metal green.
    for (x, y, expected) in [
        (5, 5, [128, 0, 127, 255]),
        (2047, 2047, [128, 0, 127, 255]),
        (2048, 2048, [128, 0, 127, 255]),
        (3007, 3383, [128, 0, 127, 255]),
        (3008, 0, [0, 0, 255, 255]),
        (4096, 2048, [0, 0, 255, 255]),
        (6015, 3383, [0, 0, 255, 255]),
    ] {
        let pixel = read_scene_pixel(gpu, image, WIDTH, HEIGHT, x, y);
        for channel in 0..4 {
            assert!(
                (i32::from(pixel[channel]) - expected[channel]).abs() <= 2,
                "mixed DMA/native/Metal sampling at ({x},{y}) pixel={pixel:?} expected={expected:?}"
            );
        }
    }
    // A new CPU Arc has different authoritative contents even when the
    // DMA producer and layer id are unchanged. Do not reuse its old cached
    // GPU base; transparent DMA texels must reveal the new exact CPU color.
    let last_id = scene.last().unwrap().id;
    assert_eq!(
        state.native_cpu_bases.len(),
        6,
        "normal scene spare cleanup wrongly discarded immutable CPU bases"
    );
    let old_base = state
        .native_cpu_bases
        .iter()
        .find(|base| base.id == last_id)
        .map(|base| Arc::clone(&base.image));
    let last = scene.last_mut().unwrap();
    last.native.as_mut().unwrap().bytes =
        Arc::new([0u8, 255, 0, 255].repeat((last.frame.width * last.frame.height) as usize));
    last.native.as_mut().unwrap().revision += 1;
    // Force the existing physical-budget eviction path without actually
    // allocating hundreds of MiB. A stale cached base is optional; all
    // accepted scene/producer images and their real charges remain intact.
    let existing = WINDOW_GPU_BUDGETS
        .lock()
        .unwrap()
        .get(&(window as usize))
        .and_then(std::sync::Weak::upgrade)
        .unwrap();
    let held = existing.held.load(Ordering::Acquire);
    let headroom = 10 * 1024 * 1024u64;
    assert!(WINDOW_GPU_PAYLOAD_BUDGET > held + headroom);
    let pressure =
        GpuCharge::reserve(window as usize, WINDOW_GPU_PAYLOAD_BUDGET - held - headroom).unwrap();
    assert!(!window_gpu_capacity_available(
        window as usize,
        3 * u64::from(last.frame.width) * u64::from(last.frame.height) * 4
    ));
    assert_eq!(stage_native_scene_source(&mut state, last), Ok(true));
    drop(pressure);
    assert!(
        state.native_cpu_bases.len() < 6,
        "native CPU base cache must shed optional images on physical pressure"
    );
    if let (Some(previous), Some(new)) = (
        old_base.as_ref(),
        state
            .native_cpu_bases
            .iter()
            .find(|base| base.id == last_id),
    ) {
        assert!(
            !Arc::ptr_eq(previous, &new.image),
            "new immutable CPU source incorrectly reused the old GPU base"
        );
    }
    state.synchronize_scene(window as usize, &scene);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        read_scene_pixel(gpu, image, WIDTH, HEIGHT, 5200, 3000),
        [0, 255, 0, 255],
        "changed native CPU pixels failed to invalidate base cache"
    );
    drop(old_base);
    let removed = scene.pop().unwrap();
    assert_eq!(removed.frame.x, 4096);
    state.synchronize_scene(window as usize, &scene);
    assert!(
        state
            .native_cpu_bases
            .iter()
            .all(|base| base.id != removed.id),
        "removed native layer left an obsolete GPU CPU-base cache entry"
    );
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        read_scene_pixel(gpu, image, WIDTH, HEIGHT, 5200, 3000),
        [0, 255, 0, 255],
        "removed mixed tile failed to reveal retained static Metal"
    );
    drop(state);
    gpu.destroy_image(source);
    sdl3::video::destroy_window(window);
}

/// Several mostly-offscreen 6K producer DMA-BUFs must allocate only clipped
/// destination images; UVs sample their original producer offset correctly.
#[test]
#[cfg(target_os = "linux")]
fn many_offscreen_six_k_dma_views_stage_in_bounded_visible_gpu_budget() {
    if isolate_vulkan_graphical_test(
        "many_offscreen_six_k_dma_views_stage_in_bounded_visible_gpu_budget",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    let picture = real_gbm_picture_with_pixels(6016, 3384, |x, y| {
        if x >= 5800 && y >= 3200 {
            [0, 0, 255, 255]
        } else {
            [255, 0, 0, 255]
        }
    });
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        panic!("no WSI");
    };
    let full = appkit::view::Frame {
        x: -5800,
        y: -3280,
        width: 6016,
        height: 3384,
        ..whole
    };
    let mut plans = Vec::new();
    for n in 0..12usize {
        let placement = ScenePlacement {
            opacity_groups: Vec::new(),
            id: (1usize << (usize::BITS - 1)) | (n + 0x6010),
            frame: DirectFrame::from_frame(&full),
            opacity: 1.0,
            opaque: false,
            clip: None,
            rounded: vec![],
            occluded: Some(vec![]),
            native: Some(scene::NativeCpuSceneSource {
                cpu_capture: None,
                bytes: Arc::new(Vec::new()),
                revision: n as u64 + 1,
                dmabuf: Some(Arc::clone(&picture)),
                dma_sample: None,
            }),
        };
        let cropped = crop_native_to_window(placement).expect("6K DMA visibility");
        assert_eq!((cropped.frame.width, cropped.frame.height), (160, 104));
        assert_eq!((cropped.frame.x, cropped.frame.y), (0, 0));
        let sample = cropped.native.as_ref().unwrap().dma_sample.unwrap();
        assert_eq!(
            (
                sample.crop_x,
                sample.crop_y,
                sample.full_width,
                sample.full_height
            ),
            (5800, 3280, 6016, 3384)
        );
        plans.push(cropped);
    }
    let before = state.scene_gpu_bytes();
    for proposal in &plans {
        assert_eq!(
            stage_native_scene_source(&mut state, proposal),
            Ok(true),
            "one of 12 otherwise representable tiny DMA viewports exhausted GPU budget"
        );
    }
    let bytes = state.scene_gpu_bytes().saturating_sub(before);
    assert!(
        bytes < 32 * 1024 * 1024,
        "12 clipped DMA sources retained {bytes} GPU bytes"
    );
    state.synchronize_scene(window as usize, &plans);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let image = state.composite.as_ref().unwrap().image;
    assert_eq!(
        read_scene_pixel(gpu, image, whole.width, whole.height, 55, 42),
        [255, 0, 0, 255],
        "cropped DMA mapped the wrong far-right/bottom producer texel"
    );
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// Exercise the complete 6K external producer -> DMA import -> retained
/// native scene -> window composite -> GPU pixel readback. A real DMA-BUF
/// exporter is opt-in because not every CI host has GBM or enough GPU memory.
#[test]
#[cfg(target_os = "linux")]
fn oversized_external_dma_buff_composes_actual_source_pixels() {
    if isolate_vulkan_graphical_test("oversized_external_dma_buff_composes_actual_source_pixels") {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        panic!("could not create real Vulkan WSI fixture for DMA-BUF pixels");
    };
    let (width, height) = (6016u32, 3384u32);
    assert!(!appkit::backing::native_scene_extent_supported(
        width, height
    ));
    assert!(appkit::backing::native_scene_dma_extent_supported(
        width, height
    ));
    let picture = real_gbm_picture_with_pixels(width, height, |_, _| [0, 0, 255, 255]);
    unsafe {
        let native_source = native_scene_source(None, Some(picture), 1, width, height, None, None)
            .expect("a 6K DMA-only layer must enter retained composition");
        assert!(native_source.bytes.is_empty());
        gpu.one_shot(|device, cb| {
            cmd_test_clear_color(device, cb, metal.image, [0.0, 1.0, 0.0, 1.0]);
        })
        .expect("clear static Metal background");
        let metal_id = 0x6b1usize;
        let native_id = (1usize << (usize::BITS - 1)) | 0x6b2;
        let bottom = test_opaque_metal_placement(metal_id, DirectFrame::from_frame(&whole));
        let huge_frame = appkit::view::Frame {
            x: 40,
            y: 35,
            width,
            height,
            ..whole
        };
        let native = ScenePlacement {
            opacity_groups: Vec::new(),
            id: native_id,
            frame: DirectFrame::from_frame(&huge_frame),
            opacity: 1.0,
            opaque: false,
            clip: None,
            rounded: vec![],
            occluded: Some(vec![]),
            native: Some(native_source),
        };
        state.layers.push(metal_id);
        state.synchronize_scene(window as usize, &[bottom.clone(), native.clone()]);
        assert_eq!(
            state.stage_composite_layer(
                metal_id,
                &whole,
                &metal,
                None,
                &mut SourceOwner(None),
                false
            ),
            Ok(Some(true))
        );
        state.note_scene_frame(metal_id, &whole);
        assert_eq!(
            stage_native_scene_source(&mut state, &native),
            Ok(true),
            "a genuine 6K DMA-BUF did not import as a retained native layer"
        );
        state.synchronize_scene(window as usize, &[bottom, native]);
        assert_eq!(state.redisplay_scene(window as usize), Ok(true));
        state.idle();
        let image = state
            .composite
            .as_ref()
            .expect("completed window composite")
            .image;
        assert_eq!(
            read_scene_pixel(gpu, image, whole.width, whole.height, 80, 60),
            [255, 0, 0, 255],
            "6K native DMA foreground disappeared or changed colour"
        );
        assert_eq!(
            read_scene_pixel(gpu, image, whole.width, whole.height, 20, 20),
            [0, 255, 0, 255],
            "6K DMA overlay erased the static Metal background outside its view"
        );
        drop(state);
        gpu.destroy_image(metal);
        sdl3::video::destroy_window(window);
    }
}

/// An invalid native DMA-BUF is a permanent capability failure, not an
/// unfinished WSI fence. Verify a later software replacement can still draw
/// over the previously successful Metal layer after the failed import.
#[test]
#[cfg(target_os = "linux")]
fn linear_dma_fallback_maps_only_far_visible_rows_and_keeps_texel_offsets() {
    use std::os::fd::FromRawFd;
    let fd = unsafe {
        libc::memfd_create(
            c"rosemary-linear-crop".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    assert!(fd >= 0);
    let width = 1024usize;
    let bytes = width * width * 4;
    assert_eq!(unsafe { libc::ftruncate(fd, bytes as libc::off_t) }, 0);
    let src = (512 * width + 512) * 4;
    let red = [0u8, 0, 255, 255];
    assert_eq!(
        unsafe { libc::pwrite(fd, red.as_ptr().cast(), 4, src as libc::off_t) },
        4
    );
    let second = (513 * width + 513) * 4;
    let blue = [255u8, 0, 0, 255];
    assert_eq!(
        unsafe { libc::pwrite(fd, blue.as_ptr().cast(), 4, second as libc::off_t) },
        4
    );
    assert_eq!(
        unsafe {
            libc::fcntl(
                fd,
                libc::F_ADD_SEALS,
                libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE,
            )
        },
        0
    );
    let scene = ScenePlacement {
        opacity_groups: Vec::new(),
        id: (1usize << (usize::BITS - 1)) | 123,
        frame: DirectFrame {
            window: 1,
            x: -512,
            y: -512,
            width: 1024,
            height: 1024,
            window_width: 32,
            window_height: 32,
        },
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: None,
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new(Vec::new()),
            revision: 1,
            dmabuf: Some(Arc::new(appkit::backing::Dmabuf {
                width: 1024,
                height: 1024,
                fourcc: u32::from_le_bytes(*b"AR24"),
                modifier: 0,
                planes: vec![appkit::backing::DmabufPlane {
                    fd: unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) },
                    offset: 0,
                    stride: 4096,
                }],
            })),
            dma_sample: None,
        }),
    };
    let mapped = linear_dmabuf_cpu_fallback(&scene).expect("bounded nonzero-map-offset source");
    assert_eq!(
        (
            mapped.frame.x,
            mapped.frame.y,
            mapped.frame.width,
            mapped.frame.height
        ),
        (0, 0, 32, 32)
    );
    let out = mapped.native.as_ref().unwrap();
    assert_eq!(out.bytes.len(), 32 * 32 * 4);
    assert_eq!(&out.bytes[..4], &red);
    assert_eq!(&out.bytes[(32 + 1) * 4..(32 + 2) * 4], &blue);
    assert!(out.dmabuf.is_none());
    // The real mixed-source path already cropped the CPU base to 32x32.
    // An importer failure must sample the far-offset original DMA producer,
    // then blend its premultiplied alpha over the cropped CPU background.
    let mut cropped = scene.clone();
    cropped.frame.x = 0;
    cropped.frame.y = 0;
    cropped.frame.width = 32;
    cropped.frame.height = 32;
    let source = cropped.native.as_mut().unwrap();
    source.bytes = Arc::new([0u8, 255, 0, 255].repeat(32 * 32));
    source.dma_sample = Some(scene::DmaSample {
        full_width: 1024,
        full_height: 1024,
        crop_x: 512,
        crop_y: 512,
    });
    let recovered = linear_dmabuf_cpu_fallback(&cropped)
        .expect("cropped source with producer's original far-offset DMA image");
    let pixels = &recovered.native.as_ref().unwrap().bytes;
    assert_eq!(&pixels[..4], &red);
    assert_eq!(&pixels[(32 + 1) * 4..(32 + 2) * 4], &blue);
    assert_eq!(
        &pixels[(32 * 2 + 2) * 4..(32 * 2 + 3) * 4],
        &[0, 255, 0, 255],
        "transparent producer pixel erased CPU content below it"
    );
    assert!(recovered.native.as_ref().unwrap().dmabuf.is_none());
    let mut unsupported = scene;
    let clone = unsupported.native.as_mut().unwrap();
    let original = clone.dmabuf.take().unwrap();
    // An unsupported modifier is not secretly treated as linear.
    clone.dmabuf = Some(Arc::new(appkit::backing::Dmabuf {
        width: 1024,
        height: 1024,
        fourcc: u32::from_le_bytes(*b"AR24"),
        modifier: 123,
        planes: vec![],
    }));
    assert!(linear_dmabuf_cpu_fallback(&unsupported).is_none());
    drop(original);
}

/// Vulkan import refuses the image, but the *actual* producer allocation is
/// linear, CPU-mappable AR24. Import refusal must not erase a never-accepted
/// foreground: copy and synchronize the producer pixels for CPU staging.
#[test]
#[cfg(target_os = "linux")]
fn rejected_linear_dma_without_prior_native_image_stays_in_final_pixels() {
    if isolate_vulkan_graphical_test(
        "rejected_linear_dma_without_prior_native_image_stays_in_final_pixels",
    ) {
        return;
    }
    use std::os::fd::FromRawFd;
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    // Synthetic linear-DMA file backing: sealed memfd supports the same
    // stride/offset pixel contract but intentionally cannot be Vulkan DMA-BUF
    // imported. Production real DMA buffers require DMA_BUF_IOCTL_SYNC.
    let fd = unsafe {
        libc::memfd_create(
            c"rosemary-linear-dma-fallback".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    assert!(fd >= 0);
    let width = 30u32;
    let height = 25u32;
    let stride = width * 4 + 16;
    let mut image = vec![0u8; (stride * height) as usize];
    for row in image.chunks_exact_mut(stride as usize) {
        for texel in row.as_chunks_mut::<4>().0.iter_mut().take(width as usize) {
            texel.copy_from_slice(&[0, 0, 255, 255]);
        }
    }
    assert_eq!(
        unsafe { libc::ftruncate(fd, image.len() as libc::off_t) },
        0
    );
    assert_eq!(
        unsafe { libc::pwrite(fd, image.as_ptr().cast(), image.len(), 0) },
        image.len() as isize
    );
    assert_eq!(
        unsafe {
            libc::fcntl(
                fd,
                libc::F_ADD_SEALS,
                libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE,
            )
        },
        0
    );
    let picture = Arc::new(appkit::backing::Dmabuf {
        width,
        height,
        fourcc: u32::from_le_bytes(*b"AR24"),
        modifier: 0,
        planes: vec![appkit::backing::DmabufPlane {
            fd: unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) },
            offset: 0,
            stride,
        }],
    });
    let native_frame = appkit::view::Frame {
        x: 40,
        y: 35,
        width,
        height,
        ..whole
    };
    let id = (1usize << (usize::BITS - 1)) | 0xa017usize;
    let native = ScenePlacement {
        opacity_groups: Vec::new(),
        id,
        frame: DirectFrame::from_frame(&native_frame),
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new(Vec::new()),
            revision: 1,
            dmabuf: Some(picture),
            dma_sample: None,
        }),
    };
    let bottom = test_opaque_metal_placement(0xa018usize, DirectFrame::from_frame(&whole));
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, metal.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .unwrap();
    state.layers.push(bottom.id);
    state.synchronize_scene(window as usize, &[bottom.clone(), native.clone()]);
    assert_eq!(
        state.stage_composite_layer(
            bottom.id,
            &whole,
            &metal,
            None,
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.note_scene_frame(bottom.id, &whole);
    assert!(
        state
            .scene_layers
            .iter()
            .all(|source| source.id != native.id)
    );
    // Force rejection with the same capability latch used by the real
    // Vulkan importer's invalid-modifier/format/FD path.
    state.unsupported_native_dma.push(UnsupportedNativeDma {
        id: native.id,
        revision: native.native.as_ref().unwrap().revision,
        producer: Arc::downgrade(native.native.as_ref().unwrap().dmabuf.as_ref().unwrap()),
    });
    let before = NATIVE_IMAGE_UPLOAD_ATTEMPTS.load(Ordering::Relaxed);
    assert_eq!(stage_native_scene_source(&mut state, &native), Ok(true));
    assert!(NATIVE_IMAGE_UPLOAD_ATTEMPTS.load(Ordering::Relaxed) > before);
    assert!(
        state
            .scene_layers
            .iter()
            .any(|source| source.id == native.id && source.ready)
    );
    state.synchronize_scene(window as usize, &[bottom, native]);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let pixel = |x, y| {
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            whole.width,
            whole.height,
            x,
            y,
        )
    };
    assert_eq!(
        pixel(48, 42),
        [255, 0, 0, 255],
        "unimportable DMA foreground disappeared"
    );
    assert_eq!(
        pixel(10, 10),
        [0, 255, 0, 255],
        "fallback overwrote lower Metal outside clip"
    );
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn refused_native_dma_buff_does_not_block_later_cpu_scene() {
    if isolate_vulkan_graphical_test("refused_native_dma_buff_does_not_block_later_cpu_scene") {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, metal.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .unwrap();
    let id = 7077usize;
    let native_id = (1usize << (usize::BITS - 1)) | 7078;
    let bottom = test_opaque_metal_placement(id, DirectFrame::from_frame(&whole));
    state.layers.push(id);
    state.synchronize_scene(window as usize, std::slice::from_ref(&bottom));
    assert_eq!(
        state.stage_composite_layer(id, &whole, &metal, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.note_scene_frame(id, &whole);
    let native_frame = appkit::view::Frame {
        x: 40,
        y: 35,
        width: 30,
        height: 25,
        ..whole
    };
    let invalid = ScenePlacement {
        opacity_groups: Vec::new(),
        id: native_id,
        frame: DirectFrame::from_frame(&native_frame),
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new(Vec::new()),
            revision: 11,
            dmabuf: Some(Arc::new(appkit::backing::Dmabuf {
                width: 30,
                height: 25,
                fourcc: 0,
                modifier: 0,
                planes: vec![],
            })),
            dma_sample: None,
        }),
    };
    // Mirror the worker's transactional order: do not mutate scene
    // membership to the proposed native source before verifying the import.
    let before_layers = state.layers.clone();
    let before_placements: Vec<_> = state.scene_placements.iter().map(|p| p.id).collect();
    let previous_image = state.scene_layers.iter().find(|p| p.id == id).map_or_else(
        || state.composite.as_ref().unwrap().image,
        |layer| layer.snapshot.image.image,
    );
    assert_eq!(
        stage_native_scene_source(&mut state, &invalid),
        Err("native-dmabuf-import-refused".to_string())
    );
    assert!(
        !appkit::backing::dmabuf_import_refused(window),
        "one unsupported producer must not disable future DMA-BUF imports"
    );
    let upload_count = NATIVE_IMAGE_UPLOAD_ATTEMPTS.load(Ordering::Relaxed);
    for _ in 0..16 {
        assert_eq!(
            stage_native_scene_source(&mut state, &invalid),
            Err("native-dmabuf-import-refused".to_string())
        );
    }
    assert_eq!(
        NATIVE_IMAGE_UPLOAD_ATTEMPTS.load(Ordering::Relaxed),
        upload_count,
        "unchanged statically unsupported DMA producer retried GPU allocations"
    );
    assert!(
        state.scene_layers.iter().all(|layer| layer.id != native_id),
        "failed native import became a ready layer"
    );
    assert_eq!(
        state.layers, before_layers,
        "permanent import failure removed a previously retained layer"
    );
    assert_eq!(
        state
            .scene_placements
            .iter()
            .map(|p| p.id)
            .collect::<Vec<_>>(),
        before_placements,
        "permanent failure committed new CA placements"
    );
    let remaining_image = state.scene_layers.iter().find(|p| p.id == id).map_or_else(
        || state.composite.as_ref().unwrap().image,
        |layer| layer.snapshot.image.image,
    );
    assert_eq!(
        remaining_image, previous_image,
        "permanent failure released the previous complete Metal image"
    );
    // The rejected overlay must not hold other CA changes hostage. The last
    // accepted Metal frame can be repainted at a new opacity while the
    // unsupported native picture is excluded awaiting a software redraw.
    let half_metal = ScenePlacement {
        opacity_groups: Vec::new(),
        opacity: 0.5,
        ..bottom.clone()
    };
    let cleaned = preserve_refused_native_sources(
        &state,
        vec![half_metal.clone(), invalid.clone()],
        &[native_id],
    );
    assert_eq!(cleaned.len(), 1, "new rejected source should be absent");
    state.synchronize_scene(window as usize, &cleaned);
    assert!(state.scene_repaint_needed);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let unaffected = read_scene_pixel(
        gpu,
        state.composite.as_ref().unwrap().image,
        whole.width,
        whole.height,
        80,
        60,
    );
    assert!(
        (i32::from(unaffected[1]) - 128).abs() <= 2,
        "an unrelated Metal opacity update stalled behind failed DMA: {unaffected:?}"
    );
    let cpu = ScenePlacement {
        opacity_groups: Vec::new(),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new([0u8, 0, 128, 128].repeat(30 * 25)),
            revision: 12,
            dmabuf: None,
            dma_sample: None,
        }),
        ..invalid
    };
    state.synchronize_scene(window as usize, &[bottom.clone(), cpu.clone()]);
    assert_eq!(stage_native_scene_source(&mut state, &cpu), Ok(true));
    // Once a CPU replacement has been accepted, a later refused DMA revision
    // must retain those last valid native pixels, not erase them.
    let second_invalid = ScenePlacement {
        opacity_groups: Vec::new(),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new(Vec::new()),
            revision: 13,
            dmabuf: invalid.native.as_ref().unwrap().dmabuf.clone(),
            dma_sample: None,
        }),
        ..cpu.clone()
    };
    let mut second_invalid = second_invalid;
    second_invalid.frame.x += 17;
    second_invalid.frame.y += 9;
    second_invalid.frame.width = 44;
    second_invalid.frame.height = 33;
    second_invalid.opacity = 0.25;
    second_invalid.opaque = true;
    second_invalid.clip = Some(DirectFrame::from_frame(&appkit::view::Frame {
        x: 10,
        y: 10,
        width: 12,
        height: 12,
        ..whole
    }));
    second_invalid.rounded.push(scene::RoundedClip {
        frame: second_invalid.frame,
        radius: 8.0,
    });
    second_invalid.occluded = Some(vec![Region {
        x: 40,
        y: 35,
        width: 2,
        height: 2,
    }]);
    let accepted =
        preserve_refused_native_sources(&state, vec![bottom.clone(), second_invalid], &[native_id]);
    assert!(
        accepted[1].frame == DirectFrame::from_frame(&cpu.frame.frame()),
        "unimported revision moved/resized old GPU image"
    );
    assert_eq!(accepted[1].opacity, cpu.opacity);
    assert_eq!(accepted[1].opaque, cpu.opaque);
    assert!(accepted[1].clip == cpu.clip);
    assert!(accepted[1].rounded == cpu.rounded);
    assert_eq!(accepted[1].occluded, cpu.occluded);
    assert_eq!(accepted.len(), 2);
    assert_eq!(
        accepted[1].native.as_ref().unwrap().revision,
        12,
        "previously accepted native pixels were dropped on reimport failure"
    );
    // Composing the rejected resize/move/opacity/mask proposal must not
    // relocate or distort the previously admitted native GPU texture.
    state.synchronize_scene(window as usize, &accepted);
    state.scene_repaint_needed = true;
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let kept = read_scene_pixel(
        gpu,
        state.composite.as_ref().unwrap().image,
        whole.width,
        whole.height,
        45,
        40,
    );
    assert!(
        (i32::from(kept[0]) - 128).abs() <= 2 && (i32::from(kept[1]) - 127).abs() <= 2,
        "rejected new masks/position hid accepted native pixels: {kept:?}"
    );
    assert_eq!(
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            whole.width,
            whole.height,
            85,
            70
        ),
        [0, 255, 0, 255],
        "unadmitted larger proposed bounds made old GPU texture appear elsewhere"
    );
    state.synchronize_scene(
        window as usize,
        &[
            test_opaque_metal_placement(id, DirectFrame::from_frame(&whole)),
            cpu,
        ],
    );
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let pixel = read_scene_pixel(
        gpu,
        state.composite.as_ref().unwrap().image,
        whole.width,
        whole.height,
        45,
        40,
    );
    assert!(
        (i32::from(pixel[0]) - 128).abs() <= 2 && (i32::from(pixel[1]) - 127).abs() <= 2,
        "CPU replacement did not advance after failed DMA import: {pixel:?}"
    );
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// The last guest Metal image initially has only a direct WSI owner: no
/// retained scene image exists. Rejected first native admission must not
/// consume that owner and replace the visible window with transparent pixels.
#[cfg(not(target_os = "macos"))]
fn exercise_direct_first_native_rejection(busy_restore: bool) {
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let _retire = AwaitTestWindowRetirements;
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let window = sdl3::video::create_window(c"direct rejected native scene", 160, 120, 0).unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let green = test_render_target(gpu, 160, 120).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, green.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .unwrap();
    let whole = test_full_frame(window, 160, 120);
    let metal_id = window as usize;
    assert_eq!(present(gpu, metal_id, &whole, &green, &[]), Ok(true));
    let entry = SWAPCHAINS
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get(&metal_id)
        .unwrap()
        .clone();
    let bottom = test_opaque_metal_placement(metal_id, DirectFrame::from_frame(&whole));
    // Seed the exact direct presentation state with its GPU source still
    // alive. The preceding public present() creates a genuine WSI window;
    // queue_direct models the last completed full-window guest drawable.
    {
        let _presenting = entry.presenting.lock().unwrap();
        let mut state = entry.state.lock().unwrap();
        state.scene_layers.clear();
        state.scene_placements = vec![bottom.clone()];
        state.layers = vec![metal_id];
        state.scene_frames.clear();
        state.note_scene_frame(metal_id, &whole);
        state.queue_direct(
            metal_id,
            &whole,
            PresentSource::from(&green),
            &mut SourceOwner(None),
            None,
        );
        assert!(state.direct.is_some());
        assert!(state.scene_layers.is_empty());
    }
    let bframe = appkit::view::Frame {
        x: 24,
        y: 20,
        width: 32,
        height: 24,
        ..whole
    };
    let missing_id = (1usize << (usize::BITS - 1)) | 0x55bbu64 as usize;
    let missing = ScenePlacement {
        opacity_groups: Vec::new(),
        id: missing_id,
        frame: DirectFrame::from_frame(&bframe),
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new(Vec::new()),
            revision: 71,
            dmabuf: Some(Arc::new(appkit::backing::Dmabuf {
                width: 32,
                height: 24,
                fourcc: 0,
                modifier: u64::MAX,
                planes: vec![],
            })),
            dma_sample: None,
        }),
    };
    let mut proposal = vec![bottom.clone()];
    if busy_restore {
        // A previously absent stage writes the accumulator before rejecting B.
        proposal.push(ScenePlacement {
            opacity_groups: Vec::new(),
            id: (1usize << (usize::BITS - 1)) | 0x55aa,
            frame: DirectFrame::from_frame(&appkit::view::Frame {
                x: 70,
                y: 45,
                ..bframe
            }),
            native: Some(scene::NativeCpuSceneSource {
                cpu_capture: None,
                bytes: Arc::new([255u8, 0, 0, 255].repeat(32 * 24)),
                revision: 72,
                dmabuf: None,
                dma_sample: None,
            }),
            ..missing.clone()
        });
    }
    proposal.push(missing);
    // First direct WSI presentation may finish before worker admission;
    // subsequent publication while restore_pending is never permitted.
    {
        let mut state = entry.state.lock().unwrap();
        if busy_restore {
            state.injected_restore_busy_cycles = 120;
        }
    }
    *entry.pending_scene.lock().unwrap() = Some(proposal);
    ensure_layer_compositor_worker(window as usize, &entry);
    wake_compositor_worker(&entry);
    let deadline = Instant::now() + Duration::from_secs(10);
    if busy_restore {
        loop {
            let state = entry.state.lock().unwrap();
            if state.restore_busy_cycles_seen >= 3 {
                assert!(
                    state.scene_restore_pending,
                    "partial GPU stage was incorrectly marked committed"
                );
                let baseline = state.wsi_publishes;
                drop(state);
                std::thread::sleep(Duration::from_millis(20));
                let state = entry.state.lock().unwrap();
                assert!(
                    state.scene_restore_pending,
                    "worker published a partial scene while restoration busy"
                );
                assert_eq!(
                    state.wsi_publishes, baseline,
                    "WSI advanced while partial accumulator awaited rollback"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker never encountered the injected busy restoration slot"
            );
            drop(state);
            std::thread::sleep(Duration::from_millis(2));
        }
        entry.state.lock().unwrap().injected_restore_busy_cycles = 0;
        wake_compositor_worker(&entry);
    }
    loop {
        let state = entry.state.lock().unwrap();
        let pending = entry.pending_scene.lock().unwrap().is_none();
        if pending
            && !state.scene_restore_pending
            && !state.scene_repaint_needed
            && state.direct.is_none()
            && state
                .scene_layers
                .iter()
                .any(|source| source.id == metal_id && source.ready)
        {
            let color = read_scene_pixel(
                gpu,
                state.composite.as_ref().unwrap().image,
                whole.width,
                whole.height,
                80,
                50,
            );
            assert_eq!(
                color,
                [0, 255, 0, 255],
                "failed native first admission erased last direct Metal source"
            );
            let other = read_scene_pixel(
                gpu,
                state.composite.as_ref().unwrap().image,
                whole.width,
                whole.height,
                28,
                25,
            );
            assert_eq!(
                other,
                [0, 255, 0, 255],
                "failed DMA scene left partial foreground over direct source"
            );
            assert!(
                state.direct.is_none(),
                "direct source not materialized before batch"
            );
            assert!(
                state
                    .scene_placements
                    .iter()
                    .all(|layer| layer.id != missing_id)
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "GPU restoration remained incomplete without guest drawables"
        );
        drop(state);
        std::thread::sleep(Duration::from_millis(2));
    }
    forget_window(window);
    drop(entry);
    gpu.destroy_image(green);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn rejected_first_native_dma_preserves_last_direct_metal_pixels() {
    if isolate_vulkan_graphical_test("rejected_first_native_dma_preserves_last_direct_metal_pixels")
    {
        return;
    }
    exercise_direct_first_native_rejection(false);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn busy_rollback_never_presents_partial_native_scene() {
    if isolate_vulkan_graphical_test("busy_rollback_never_presents_partial_native_scene") {
        return;
    }
    exercise_direct_first_native_rejection(true);
}

/// A newly published DMA foreground has neither a retained GPU layer nor
/// readable producer pixels. Its rejection cannot commit a CA scene which
/// drops it. Retain the previous complete Metal frame, avoid 10-ms retries,
/// and admit an actual later CPU replacement without another Metal submit.
#[test]
#[cfg(not(target_os = "macos"))]
fn unreadable_dma_rejection_keeps_last_complete_scene_until_cpu_replacement() {
    if isolate_vulkan_graphical_test(
        "unreadable_dma_rejection_keeps_last_complete_scene_until_cpu_replacement",
    ) {
        return;
    }
    let _guard = AwaitTestWindowRetirements;
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let window = sdl3::video::create_window(c"DMA admission fallback WSI", 160, 120, 0).unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let metal = test_render_target(gpu, 160, 120).unwrap();
    let whole = test_full_frame(window, 160, 120);
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, metal.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .unwrap();
    let metal_id = window as usize;
    assert_eq!(present(gpu, metal_id, &whole, &metal, &[]), Ok(true));
    let entry = SWAPCHAINS
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get(&metal_id)
        .unwrap()
        .clone();
    let native_frame = appkit::view::Frame {
        x: 24,
        y: 20,
        width: 32,
        height: 24,
        ..whole
    };
    let native_id = (1usize << (usize::BITS - 1)) | 0x809123usize;
    let bottom = test_opaque_metal_placement(metal_id, DirectFrame::from_frame(&whole));
    let unknown = ScenePlacement {
        opacity_groups: Vec::new(),
        id: native_id,
        frame: DirectFrame::from_frame(&native_frame),
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new(Vec::new()),
            revision: 11,
            dmabuf: Some(Arc::new(appkit::backing::Dmabuf {
                width: 32,
                height: 24,
                fourcc: 0,
                modifier: u64::MAX,
                planes: vec![],
            })),
            dma_sample: None,
        }),
    };
    // A has already been displayed at revision 9. Stage its revision 10
    // replacement before an unreadable newly introduced B. The batch must
    // restore the prior GPU allocation AND final framebuffer, not only order.
    let earlier_id = (1usize << (usize::BITS - 1)) | 0x809124usize;
    let earlier_frame = appkit::view::Frame {
        x: 75,
        y: 50,
        ..native_frame
    };
    let previously_accepted_a = ScenePlacement {
        opacity_groups: Vec::new(),
        id: earlier_id,
        frame: DirectFrame::from_frame(&earlier_frame),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new([0u8, 0, 255, 255].repeat(32 * 24)),
            revision: 9,
            dmabuf: None,
            dma_sample: None,
        }),
        ..unknown.clone()
    };
    let previous_gpu_a = {
        let mut state = entry.state.lock().unwrap();
        assert_eq!(
            stage_native_scene_source(&mut state, &previously_accepted_a),
            Ok(true)
        );
        state.synchronize_scene(
            window as usize,
            &[bottom.clone(), previously_accepted_a.clone()],
        );
        assert_eq!(state.redisplay_scene(window as usize), Ok(true));
        state.idle();
        let image = state
            .scene_layers
            .iter_mut()
            .find(|p| p.id == earlier_id)
            .unwrap();
        assert_eq!(image.native_revision, Some(9));
        image.native_published_revision = Some(9);
        let original_snapshot = Arc::clone(&image.snapshot);
        assert_eq!(
            read_scene_pixel(
                gpu,
                state.composite.as_ref().unwrap().image,
                whole.width,
                whole.height,
                80,
                55
            ),
            [255, 0, 0, 255]
        );
        original_snapshot
    };
    let earlier = ScenePlacement {
        opacity_groups: Vec::new(),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new([255u8, 0, 0, 255].repeat(32 * 24)),
            revision: 10,
            dmabuf: None,
            dma_sample: None,
        }),
        ..previously_accepted_a.clone()
    };
    // A refused replacement of A itself can never acknowledge revision 10
    // using A's existing ready revision 9 GPU image.
    {
        let state = entry.state.lock().unwrap();
        let publication = HashMap::from([(earlier_id, 10u64)]);
        let staged = HashMap::from([(
            earlier_id,
            StagedNativeIdentity {
                revision: 9,
                dma_sample: None,
            },
        )]);
        let previous = vec![previously_accepted_a.clone()];
        assert!(
            committed_native_gpu_revisions(&state, &previous, &publication, &staged, &[earlier_id])
                .is_empty(),
            "refused revision 10 must not claim previously accepted revision 9"
        );
        assert!(
            committed_native_gpu_revisions(&state, &previous, &publication, &staged, &[])
                .is_empty(),
            "old GPU image cannot acknowledge a different AppKit publication"
        );
        let original = HashMap::from([(earlier_id, 9u64)]);
        assert_eq!(
            committed_native_gpu_revisions(&state, &previous, &original, &staged, &[]),
            vec![(earlier_id, 9)]
        );
    }
    let prior_placements = entry.state.lock().unwrap().scene_placements.clone();
    *entry.pending_scene.lock().unwrap() =
        Some(vec![bottom.clone(), earlier.clone(), unknown.clone()]);
    ensure_layer_compositor_worker(window as usize, &entry);
    wake_compositor_worker(&entry);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let pending = entry.pending_scene.lock().unwrap().is_none();
        if pending {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "worker indefinitely retried DMA import refusal"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(120));
    assert!(
        entry.pending_scene.lock().unwrap().is_none(),
        "a permanently unimportable DMA revision was requeued"
    );
    {
        let state = entry.state.lock().unwrap();
        assert_eq!(
            state.scene_placements.len(),
            prior_placements.len(),
            "worker committed an incomplete replacement scene"
        );
        assert!(
            state
                .scene_placements
                .iter()
                .all(|placement| placement.id != native_id)
        );
        let accepted_a = state
            .scene_layers
            .iter()
            .find(|layer| layer.id == earlier_id)
            .unwrap();
        assert_eq!(
            accepted_a.native_published_revision,
            Some(9),
            "failed transaction advanced the original AppKit publication"
        );
        assert_eq!(
            accepted_a.native_revision,
            Some(9),
            "restored placements still point at changed GPU native revision"
        );
        assert!(
            Arc::ptr_eq(&accepted_a.snapshot, &previous_gpu_a),
            "failed transaction did not retain pre-batch GPU image lease"
        );
        assert_eq!(
            read_scene_pixel(
                gpu,
                state.composite.as_ref().unwrap().image,
                whole.width,
                whole.height,
                80,
                55
            ),
            [255, 0, 0, 255],
            "failed B import left new A pixels in supposedly restored composite"
        );
        let source = state
            .composite
            .as_ref()
            .expect("previous Metal accumulator");
        assert_eq!(
            read_scene_pixel(gpu, source.image, whole.width, whole.height, 28, 24),
            [0, 255, 0, 255],
            "unreadable DMA failure corrupted last complete image"
        );
    }
    assert_eq!(entry.present_failure.load(Ordering::Relaxed), 0);
    let real = ScenePlacement {
        opacity_groups: Vec::new(),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new([0u8, 0, 255, 255].repeat(32 * 24)),
            revision: 12,
            dmabuf: None,
            dma_sample: None,
        }),
        ..unknown
    };
    *entry.pending_scene.lock().unwrap() = Some(vec![bottom, earlier, real]);
    wake_compositor_worker(&entry);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = entry.state.lock().unwrap();
        let settled = entry.pending_scene.lock().unwrap().is_none();
        let accepted = state
            .scene_layers
            .iter()
            .any(|p| p.id == native_id && p.ready);
        if settled && accepted && !state.composite_dirty {
            let source = state.composite.as_ref().unwrap();
            assert_eq!(
                read_scene_pixel(gpu, source.image, whole.width, whole.height, 28, 24),
                [255, 0, 0, 255],
                "actual producer CPU replacement was not shown above Metal"
            );
            assert_eq!(
                read_scene_pixel(gpu, source.image, whole.width, whole.height, 80, 55),
                [0, 0, 255, 255],
                "successful later batch did not commit revised A pixels"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "producer CPU fallback did not resume compositing"
        );
        drop(state);
        std::thread::sleep(Duration::from_millis(5));
    }
    forget_window(window);
    drop(entry);
    sdl3::video::destroy_window(window);
    gpu.destroy_image(metal);
}

/// A newly proposed native DMA geometry revision must never overwrite the
/// last GPU-admitted source simply because another Metal drawable arrived
/// first. Also preserve still-pending native removals until the worker commits.
#[test]
#[cfg(not(target_os = "macos"))]
fn guest_metal_present_cannot_commit_unadmitted_native_geometry_before_dma_refusal() {
    if isolate_vulkan_graphical_test(
        "guest_metal_present_cannot_commit_unadmitted_native_geometry_before_dma_refusal",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, metal.image, [0.0, 1.0, 0.0, 1.0])
    })
    .unwrap();
    let background = test_opaque_metal_placement(0xb010, DirectFrame::from_frame(&whole));
    let old_frame = appkit::view::Frame {
        x: 20,
        y: 16,
        width: 32,
        height: 24,
        ..whole
    };
    let old = ScenePlacement {
        opacity_groups: Vec::new(),
        id: (1usize << (usize::BITS - 1)) | 0xb011,
        frame: DirectFrame::from_frame(&old_frame),
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new([0u8, 0, 255, 255].repeat(32 * 24)),
            revision: 7,
            dmabuf: None,
            dma_sample: None,
        }),
    };
    state.layers.push(background.id);
    state.synchronize_scene(window as usize, &[background.clone(), old.clone()]);
    assert_eq!(
        state.stage_composite_source(
            background.id,
            &whole,
            PresentSource::from(&metal),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    assert_eq!(stage_native_scene_source(&mut state, &old), Ok(true));
    state.synchronize_scene(window as usize, &[background.clone(), old.clone()]);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let image = state.composite.as_ref().unwrap().image;
    assert_eq!(
        read_scene_pixel(gpu, image, whole.width, whole.height, 24, 20),
        [255, 0, 0, 255]
    );
    let proposed = ScenePlacement {
        opacity_groups: Vec::new(),
        frame: DirectFrame::from_frame(&appkit::view::Frame {
            x: 75,
            y: 55,
            width: 48,
            height: 30,
            ..whole
        }),
        opacity: 0.4,
        opaque: false,
        clip: Some(DirectFrame::from_frame(&appkit::view::Frame {
            x: 82,
            y: 60,
            width: 24,
            height: 16,
            ..whole
        })),
        rounded: vec![scene::RoundedClip {
            frame: DirectFrame::from_frame(&old_frame),
            radius: 9.0,
        }],
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new(Vec::new()),
            revision: 8,
            dmabuf: Some(Arc::new(appkit::backing::Dmabuf {
                width: 48,
                height: 30,
                fourcc: 0,
                modifier: 0,
                planes: vec![],
            })),
            dma_sample: None,
        }),
        ..old.clone()
    };
    let proposed_scene = vec![background.clone(), proposed.clone()];
    // Same translation as the competing present_impl() path. The worker has
    // not yet staged the published revision. Guest drawable admission may
    // update Metal pixels but not metadata of the proposed native image.
    let safe = state.guest_present_scene(&proposed_scene);
    assert_eq!(safe.len(), 2);
    assert_eq!(
        safe.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![background.id, old.id]
    );
    let reorder = state.guest_present_scene(&[proposed.clone(), background.clone()]);
    assert_eq!(
        reorder.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![background.id, old.id],
        "guest draw unexpectedly accepted native stacking/reorder before DMA admission"
    );
    assert!(safe[1].frame == old.frame);
    assert_eq!(safe[1].opacity, old.opacity);
    assert!(safe[1].clip == old.clip);
    assert!(safe[1].rounded == old.rounded);
    assert_eq!(safe[1].native.as_ref().unwrap().revision, 7);
    state.synchronize_scene(window as usize, &safe);
    let retained = state
        .scene_placements
        .iter()
        .find(|s| s.id == old.id)
        .unwrap();
    assert!(retained.frame == old.frame);
    assert_eq!(retained.opacity, 1.0);
    assert!(retained.clip.is_none());
    assert_eq!(retained.native.as_ref().unwrap().revision, 7);
    assert_eq!(
        stage_native_scene_source(&mut state, &proposed),
        Err("native-dmabuf-import-refused".to_owned())
    );
    let recovered = preserve_refused_native_sources(&state, proposed_scene, &[old.id]);
    assert!(recovered[1].frame == old.frame);
    assert_eq!(recovered[1].opacity, old.opacity);
    assert!(recovered[1].clip == old.clip);
    state.synchronize_scene(window as usize, &recovered);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        read_scene_pixel(gpu, image, whole.width, whole.height, 24, 20),
        [255, 0, 0, 255],
        "failed revision lost original foreground pixels after competing guest present"
    );
    assert_eq!(
        read_scene_pixel(gpu, image, whole.width, whole.height, 88, 66),
        [0, 255, 0, 255],
        "unadmitted new placement incorrectly moved old CPU pixels"
    );
    let removal = state.guest_present_scene(std::slice::from_ref(&background));
    assert_eq!(
        removal.len(),
        2,
        "uncommitted native removal was applied by guest present"
    );
    assert!(removal[1].frame == old.frame);
    let introduction = ScenePlacement {
        opacity_groups: Vec::new(),
        id: old.id + 1,
        ..proposed
    };
    let safe = state.guest_present_scene(&[background, introduction]);
    assert!(
        !safe.iter().any(|p| p.id == old.id + 1),
        "unadmitted new native became guest-owned"
    );
    assert!(
        safe.iter().any(|p| p.id == old.id),
        "old native was pruned before worker commit"
    );
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// The 512 MiB scene budget is stricter than the 768 MiB window allocation
/// ledger. Fence/transaction-pinned old images above that scene threshold
/// must defer DMA import without poisoning the window's importer capability.
#[test]
#[cfg(target_os = "linux")]
fn pinned_scene_generation_budget_defers_dma_and_recovers_after_retirement() {
    if isolate_vulkan_graphical_test(
        "pinned_scene_generation_budget_defers_dma_and_recovers_after_retirement",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let picture = real_gbm_picture_with_pixels(48, 44, |_, _| [0, 0, 255, 255]);
    let view = appkit::view::Frame {
        x: 12,
        y: 16,
        width: 48,
        height: 44,
        ..whole
    };
    let placement = ScenePlacement {
        opacity_groups: Vec::new(),
        id: (1usize << (usize::BITS - 1)) | 0xb015,
        frame: DirectFrame::from_frame(&view),
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new(Vec::new()),
            revision: 11,
            dmabuf: Some(picture),
            dma_sample: None,
        }),
    };
    assert!(!appkit::backing::dmabuf_import_refused(window));
    let desc = ImageDescription {
        image_type: vk::ImageType::TYPE_2D,
        view_type: vk::ImageViewType::TYPE_2D,
        format: vk::Format::B8G8R8A8_UNORM,
        width: 4096,
        height: 4096,
        depth: 1,
        levels: 1,
        layers: 1,
        samples: 1,
        usage: gpu::USAGE_RENDER_TARGET,
        cube: false,
        components: vk::ComponentMapping::default(),
    };
    for _ in 0..9 {
        state.transaction_pinned.push(
            BackingImageLease::create_for_window(gpu, window as usize, &desc)
                .expect("9 x 64MiB old snapshot leases under the 768MiB aggregate window budget"),
        );
    }
    assert!(state.scene_gpu_bytes() > 512 * 1024 * 1024);
    assert!(state.committed_scene_bytes() < 512 * 1024 * 1024);
    let before = NATIVE_IMAGE_UPLOAD_ATTEMPTS.load(Ordering::Relaxed);
    let denied = NATIVE_HARD_BUDGET_REFUSALS.load(Ordering::Relaxed);
    assert_eq!(
        stage_native_scene_source(&mut state, &placement),
        Ok(false),
        "temporary scene lease pressure was classified as a permanent failure"
    );
    assert!(
        !appkit::backing::dmabuf_import_refused(window),
        "temporary 512MiB scene pressure permanently poisoned DMA-BUF import"
    );
    assert_eq!(NATIVE_IMAGE_UPLOAD_ATTEMPTS.load(Ordering::Relaxed), before);
    assert_eq!(NATIVE_HARD_BUDGET_REFUSALS.load(Ordering::Relaxed), denied);
    state.transaction_pinned.clear();
    state.retire_copied();
    assert_eq!(
        stage_native_scene_source(&mut state, &placement),
        Ok(true),
        "valid DMA producer was not retried when old GPU leases retired"
    );
    assert!(!appkit::backing::dmabuf_import_refused(window));
    state.synchronize_scene(window as usize, &[placement]);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            whole.width,
            whole.height,
            25,
            30
        ),
        [255, 0, 0, 255]
    );
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// Full AppKit publication to Vulkan compositor: move a real 6K native
/// NSView that owns one unchanged DMA producer behind the same small window.
/// Both AppKit's change identity and retained GPU pixels must follow the crop.
#[test]
#[cfg(target_os = "linux")]
fn appkit_moving_six_k_dma_source_updates_static_metal_window_pixels() {
    if isolate_vulkan_graphical_test(
        "appkit_moving_six_k_dma_source_updates_static_metal_window_pixels",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    appkit::bootstrap();
    use foundation::geometry::{NSPoint, NSRect, NSSize};
    use objc::object::Id;
    let rect = |x, y, width, height| NSRect {
        origin: NSPoint { x, y },
        size: NSSize { width, height },
    };
    let window = objc::object::create_instance(
        objc::class::lookup(c"NSWindow").unwrap(),
        appkit::window::INSTANCE_SIZE,
    );
    let init: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect, usize, usize, u8) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    assert_eq!(
        unsafe {
            init(
                window,
                objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
                rect(0.0, 0.0, 160.0, 120.0),
                0,
                0,
                0,
            )
        },
        window
    );
    let sdl = unsafe { appkit::window::sdl_window(window) };
    assert!(!sdl.is_null());
    let mk_view = |x, y, width, height| {
        let v = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let f: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            f(
                v,
                objc::sel::get("initWithFrame:"),
                rect(x, y, width, height),
            )
        }
    };
    let content = mk_view(0.0, 0.0, 160.0, 120.0);
    let metal_view = mk_view(0.0, 0.0, 160.0, 120.0);
    let native = mk_view(-5800.0, -3200.0, 6016.0, 3384.0);
    let set: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let metal_cls = objc::class::lookup(c"CAMetalLayer").unwrap();
    let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let metal_layer = unsafe { make(metal_cls as *const _ as Id, objc::sel::get("layer")) };
    unsafe {
        set(window, objc::sel::get("setContentView:"), content);
        set(metal_view, objc::sel::get("setLayer:"), metal_layer);
        set(content, objc::sel::get("addSubview:"), metal_view);
        set(content, objc::sel::get("addSubview:"), native);
    }
    let producer = real_gbm_picture_with_pixels(6016, 3384, |x, _| {
        if x >= 5700 {
            [0, 0, 255, 255]
        } else {
            [255, 0, 0, 255]
        }
    });
    appkit::backing::set_overlay(
        native,
        appkit::backing::Overlay {
            width: 6016,
            height: 3384,
            picture: appkit::backing::Picture::Dmabuf(Arc::clone(&producer)),
        },
    );
    appkit::event::pump(std::ptr::null_mut());
    let capture = || {
        appkit::view::metal_scene(sdl)
            .unwrap()
            .into_iter()
            .find(|p| p.native_owner == native as usize)
            .expect("native foreground published above Metal")
    };
    let initial = capture();
    assert_eq!(
        (
            initial.frame.x,
            initial.frame.y,
            initial.frame.width,
            initial.frame.height
        ),
        (0, 0, 160, 120)
    );
    assert_eq!(initial.native_dma_sample, Some((6016, 3384, 5800, 64)));
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    sdl3::video::presentation_window(sdl).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(sdl, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        sdl,
        vk::Extent2D {
            width: 160,
            height: 120,
        },
        surface,
        instance,
    )
    .unwrap();
    let to_placement = |item: appkit::view::MetalSceneLayer| ScenePlacement {
        opacity_groups: Vec::new(),
        id: item.layer,
        frame: DirectFrame::from_frame(&item.frame),
        opacity: item.opacity,
        opaque: item.opaque,
        clip: item.clip.map(|c| DirectFrame::from_frame(&c)),
        rounded: vec![],
        occluded: Some(vec![]),
        native: native_scene_source(
            item.native_pixels,
            item.native_dmabuf,
            item.native_revision,
            item.frame.width,
            item.frame.height,
            item.native_cpu_capture,
            item.native_dma_sample,
        ),
    };
    let old = to_placement(initial.clone());
    assert_eq!(stage_native_scene_source(&mut state, &old), Ok(true));
    state.synchronize_scene(sdl as usize, &[old]);
    assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
    state.idle();
    let image = state.composite.as_ref().unwrap().image;
    assert_eq!(
        read_scene_pixel(gpu, image, 160, 120, 40, 30),
        [255, 0, 0, 255]
    );
    let set_origin: unsafe extern "C" fn(Id, objc::sel::Sel, NSPoint) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe {
        set_origin(
            native,
            objc::sel::get("setFrameOrigin:"),
            NSPoint {
                x: -5500.0,
                y: -3200.0,
            },
        )
    };
    appkit::event::pump(std::ptr::null_mut());
    let shifted = capture();
    assert_eq!(
        (
            shifted.frame.x,
            shifted.frame.y,
            shifted.frame.width,
            shifted.frame.height
        ),
        (0, 0, 160, 120)
    );
    assert_eq!(shifted.native_dma_sample, Some((6016, 3384, 5500, 64)));
    assert_ne!(shifted.native_revision, initial.native_revision);
    let new = to_placement(shifted);
    assert_eq!(stage_native_scene_source(&mut state, &new), Ok(true));
    state.synchronize_scene(sdl as usize, &[new]);
    assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
    state.idle();
    assert_eq!(
        read_scene_pixel(gpu, image, 160, 120, 40, 30),
        [0, 0, 255, 255],
        "moved AppKit foreground retained old DMA sample pixels in GPU window"
    );
    drop(state);
    appkit::backing::remove_overlay(native);
    objc::arc::release(window);
}

/// Moving a static 6K DMA producer behind an unchanged 160x120 viewport
/// must replace the retained GPU sampling image even when its published
/// setter revision, view ID and visible placement dimensions are unchanged.
#[test]
#[cfg(target_os = "linux")]
fn moving_static_large_dma_source_changes_retained_gpu_pixels() {
    if isolate_vulkan_graphical_test("moving_static_large_dma_source_changes_retained_gpu_pixels") {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    let picture = real_gbm_picture_with_pixels(6016, 3384, |x, _| {
        if x >= 5700 {
            [0, 0, 255, 255]
        } else {
            [255, 0, 0, 255]
        }
    });
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let id = (1usize << (usize::BITS - 1)) | 0x9911;
    let make = |crop_x| ScenePlacement {
        opacity_groups: Vec::new(),
        id,
        frame: DirectFrame::from_frame(&whole),
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new(Vec::new()),
            revision: 42,
            dmabuf: Some(Arc::clone(&picture)),
            dma_sample: Some(scene::DmaSample {
                full_width: 6016,
                full_height: 3384,
                crop_x,
                crop_y: 3200,
            }),
        }),
    };
    let first = make(5800);
    assert_eq!(stage_native_scene_source(&mut state, &first), Ok(true));
    state.synchronize_scene(window as usize, &[first]);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let image = state.composite.as_ref().unwrap().image;
    assert_eq!(
        read_scene_pixel(gpu, image, whole.width, whole.height, 40, 30),
        [255, 0, 0, 255]
    );
    let changed = make(5500);
    assert_eq!(stage_native_scene_source(&mut state, &changed), Ok(true));
    assert_eq!(
        state
            .scene_layers
            .iter()
            .find(|s| s.id == id)
            .unwrap()
            .native_dma_sample,
        changed.native.as_ref().unwrap().dma_sample
    );
    state.synchronize_scene(window as usize, &[changed]);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        read_scene_pixel(gpu, image, whole.width, whole.height, 40, 30),
        [0, 0, 255, 255],
        "moved DMA source reused an old crop at an unchanged output position"
    );
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// A 512MiB final-scene budget must not permanently block replacing seven
/// obsolete 4096-square native images with one supported 2048-square mixed
/// source. Old generations are still physically pinned for rollback, and
/// every allocation remains charged under the 768MiB window-wide ceiling.
#[test]
#[cfg(target_os = "linux")]
fn replacing_obsolete_native_gpu_generations_makes_forward_progress() {
    if isolate_vulkan_graphical_test(
        "replacing_obsolete_native_gpu_generations_makes_forward_progress",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    let picture = real_gbm_picture_with_pixels(2048, 2048, |_, _| [0, 0, 255, 255]);
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let window =
        sdl3::video::create_window(c"native membership replacement", 2048, 2048, 0).unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        window,
        vk::Extent2D {
            width: 2048,
            height: 2048,
        },
        surface,
        instance,
    )
    .unwrap();
    let whole = test_full_frame(window, 2048, 2048);
    let old_desc = ImageDescription {
        image_type: vk::ImageType::TYPE_2D,
        view_type: vk::ImageViewType::TYPE_2D,
        format: vk::Format::B8G8R8A8_UNORM,
        width: 4096,
        height: 4096,
        depth: 1,
        levels: 1,
        layers: 1,
        samples: 1,
        usage: gpu::USAGE_RENDER_TARGET,
        cube: false,
        components: vk::ComponentMapping::default(),
    };
    for n in 0..8usize {
        let lease = BackingImageLease::create_for_window(gpu, window as usize, &old_desc)
            .expect("old 64MiB accepted image");
        state.seed_accepted_native_for_budget_test(
            (1usize << (usize::BITS - 1)) | 0xc100 | n,
            DirectFrame::from_frame(&whole),
            lease,
        );
    }
    assert!(state.scene_gpu_bytes() >= 512 * 1024 * 1024);
    let new_id = (1usize << (usize::BITS - 1)) | 0xc200;
    let replacement = ScenePlacement {
        opacity_groups: Vec::new(),
        id: new_id,
        frame: DirectFrame::from_frame(&whole),
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new([255u8, 0, 0, 255].repeat(2048 * 2048)),
            revision: 2,
            dmabuf: Some(picture),
            dma_sample: None,
        }),
    };
    let original = state.scene_layers.clone();
    state.transaction_pinned = original.iter().map(|l| Arc::clone(&l.snapshot)).collect();
    state.transaction_retiring_bytes = original
        .iter()
        .map(|l| scene::scene_image_bytes(PresentSource::from(&l.snapshot.image)))
        .sum();
    assert!(
        !scene::fits_scene_budget(state.scene_gpu_bytes(), 4 * 2048 * 2048 * 4),
        "fixture did not trigger obsolete-image 512MiB contention"
    );
    assert!(scene::fits_scene_budget(
        state.budgeted_scene_bytes(),
        4 * 2048 * 2048 * 4
    ));
    assert_eq!(
        stage_native_scene_source(&mut state, &replacement),
        Ok(true)
    );
    assert!(!appkit::backing::dmabuf_import_refused(window));
    state.synchronize_scene(window as usize, &[replacement]);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let image = state.composite.as_ref().unwrap().image;
    assert_eq!(
        read_scene_pixel(gpu, image, 2048, 2048, 60, 40),
        [255, 0, 0, 255],
        "new mixed source failed to replace obsolete native images"
    );
    state.transaction_pinned.clear();
    state.transaction_retiring_bytes = 0;
    assert_eq!(state.scene_layers.iter().filter(|s| s.ready).count(), 1);
    assert!(state.scene_gpu_bytes() < 512 * 1024 * 1024);
    drop(state);
    sdl3::video::destroy_window(window);
}

/// A high-occupancy native scene can replace an accepted GPU generation
/// *without changing its layer ID*. The 512MiB projected scene budget must
/// credit the old pixels it supersedes, not just views removed by membership.
#[test]
#[cfg(target_os = "linux")]
fn same_native_id_near_scene_budget_replaces_real_dma_pixels() {
    if isolate_vulkan_graphical_test("same_native_id_near_scene_budget_replaces_real_dma_pixels") {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    let picture = real_gbm_picture_with_pixels(2048, 2048, |_, _| [0, 0, 255, 255]);
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let window = sdl3::video::create_window(c"same id native 512MiB", 2048, 2048, 0).unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        window,
        vk::Extent2D {
            width: 2048,
            height: 2048,
        },
        surface,
        instance,
    )
    .unwrap();
    let whole = test_full_frame(window, 2048, 2048);
    let frame = DirectFrame::from_frame(&whole);
    let desc = ImageDescription {
        image_type: vk::ImageType::TYPE_2D,
        view_type: vk::ImageViewType::TYPE_2D,
        format: vk::Format::B8G8R8A8_UNORM,
        width: 2048,
        height: 2048,
        depth: 1,
        levels: 1,
        layers: 1,
        samples: 1,
        usage: gpu::USAGE_RENDER_TARGET,
        cube: false,
        components: vk::ComponentMapping::default(),
    };
    let ids: Vec<_> = (0..28usize)
        .map(|n| (1usize << (usize::BITS - 1)) | 0xca00 | n)
        .collect();
    for id in &ids {
        let lease = BackingImageLease::create_for_window(gpu, window as usize, &desc)
            .expect("accepted native 16MiB image under physical window ledger");
        state.seed_accepted_native_for_budget_test(*id, frame, lease);
    }
    // Initial accepted image generation is blue, later DMA revision red.
    let id = *ids.last().unwrap();
    let base = state
        .scene_layers
        .iter()
        .find(|s| s.id == id)
        .unwrap()
        .snapshot
        .image
        .image;
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, base, [1.0, 0.0, 0.0, 1.0]);
    })
    .unwrap();
    let old_scene = state.scene_placements.clone();
    state.synchronize_scene(window as usize, &old_scene);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert!(state.scene_gpu_bytes() >= 464 * 1024 * 1024);
    let output = state.composite.as_ref().unwrap().image;
    // Synthetic preaccepted snapshots validate the GPU lease and budget
    // transition; their pixels are not an AppKit producer publication.
    assert_eq!(state.scene_layers.iter().filter(|s| s.ready).count(), 28);
    let changed = ScenePlacement {
        opacity_groups: Vec::new(),
        id,
        frame,
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new([255, 0, 0, 255].repeat(2048 * 2048)),
            revision: 2,
            dmabuf: Some(picture),
            dma_sample: None,
        }),
    };
    let mut proposal = old_scene;
    *proposal.last_mut().unwrap() = changed.clone();
    let previous = state.scene_layers.clone();
    state.transaction_pinned = previous
        .iter()
        .map(|image| Arc::clone(&image.snapshot))
        .collect();
    state.transaction_retiring_bytes = previous
        .iter()
        .filter(|image| {
            image.native_revision.is_some()
                && !proposal
                    .iter()
                    .any(|p| native_image_matches_placement(image, p))
        })
        .map(|image| scene::scene_image_bytes(PresentSource::from(&image.snapshot.image)))
        .sum();
    assert_eq!(state.transaction_retiring_bytes, 16 * 1024 * 1024);
    let staged_scratch = 4 * 2048 * 2048 * 4;
    assert!(!scene::fits_scene_budget(
        state.scene_gpu_bytes(),
        staged_scratch
    ));
    assert!(scene::fits_scene_budget(
        state.budgeted_scene_bytes(),
        staged_scratch
    ));
    assert!(scene::fits_scene_budget(
        state.projected_committed_scene_bytes(),
        staged_scratch
    ));
    assert_eq!(
        stage_native_scene_source(&mut state, &changed),
        Ok(true),
        "same-ID native replacement was permanently refused at 512MiB"
    );
    state.synchronize_scene(window as usize, &proposal);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        read_scene_pixel(gpu, output, 2048, 2048, 60, 40),
        [255, 0, 0, 255],
        "accepted image content stayed stale even after same-ID admission"
    );
    assert!(!appkit::backing::dmabuf_import_refused(window));
    state.transaction_pinned.clear();
    state.transaction_retiring_bytes = 0;
    drop(previous);
    assert!(state.scene_gpu_bytes() <= 512 * 1024 * 1024);
    drop(state);
    sdl3::video::destroy_window(window);
}

/// Staging a second large mixed native tile while preserving old and first-
/// replacement generations for rollback can exceed the 768MiB physical cap,
/// even though the final projected retained scene fits under 512MiB. Classify
/// this as a nonprogressing atomic peak, not a fence-held temporary retry or
/// an importer failure. No GPU submission or rejected CPU pixel fallback.
#[test]
#[cfg(not(target_os = "macos"))]
fn atomic_scene_replacement_peak_reports_nonprogressing_physical_pressure() {
    if isolate_vulkan_graphical_test(
        "atomic_scene_replacement_peak_reports_nonprogressing_physical_pressure",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let desc = ImageDescription {
        image_type: vk::ImageType::TYPE_2D,
        view_type: vk::ImageViewType::TYPE_2D,
        format: vk::Format::B8G8R8A8_UNORM,
        width: 2048,
        height: 2048,
        depth: 1,
        levels: 1,
        layers: 1,
        samples: 1,
        usage: gpu::USAGE_RENDER_TARGET,
        cube: false,
        components: vk::ComponentMapping::default(),
    };
    let mut old = Vec::new();
    for n in 0..28usize {
        let lease = BackingImageLease::create_for_window(gpu, window as usize, &desc).unwrap();
        old.push(Arc::clone(&lease));
        state.seed_accepted_native_for_budget_test(
            (1usize << (usize::BITS - 1)) | 0xdd00 | n,
            DirectFrame::from_frame(&whole),
            lease,
        );
    }
    // Nineteen tiles were successfully staged in this atomic replacement,
    // but the previous 28 old generations must remain pinned for rollback.
    // The 47 actual 16MiB leases occupy 752MiB before one more tile begins.
    state.transaction_pinned = old.clone();
    for i in 0..19 {
        let fresh = BackingImageLease::create_for_window(gpu, window as usize, &desc).unwrap();
        state.scene_layers[i].snapshot = fresh;
        state.scene_layers[i].native_revision = Some(2);
    }
    state.transaction_retiring_bytes = 19 * 16 * 1024 * 1024;
    let incoming = 4 * 16 * 1024 * 1024u64;
    assert_eq!(state.atomic_transition_gpu_floor(), 47 * 16 * 1024 * 1024);
    assert_eq!(state.budgeted_scene_bytes(), 28 * 16 * 1024 * 1024);
    assert!(scene::fits_scene_budget(
        state.budgeted_scene_bytes(),
        incoming
    ));
    assert!(state.atomic_transition_gpu_floor() + incoming > WINDOW_GPU_PAYLOAD_BUDGET);
    assert!(!window_gpu_capacity_available(window as usize, incoming));
    let pending = ScenePlacement {
        opacity_groups: Vec::new(),
        id: (1usize << (usize::BITS - 1)) | 0xee00,
        frame: DirectFrame {
            window: window as usize,
            x: 0,
            y: 0,
            width: 2048,
            height: 2048,
            window_width: whole.width,
            window_height: whole.height,
        },
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new(vec![0u8; 16 * 1024 * 1024]),
            revision: 2,
            dmabuf: Some(Arc::new(appkit::backing::Dmabuf {
                width: 2048,
                height: 2048,
                fourcc: 0,
                modifier: 0,
                planes: vec![],
            })),
            dma_sample: None,
        }),
    };
    let before = NATIVE_IMAGE_UPLOAD_ATTEMPTS.load(Ordering::Relaxed);
    assert_eq!(
        stage_native_scene_source(&mut state, &pending),
        Err("native AppKit atomic GPU transition peak exhausted".to_owned())
    );
    assert_eq!(NATIVE_IMAGE_UPLOAD_ATTEMPTS.load(Ordering::Relaxed), before);
    assert!(!appkit::backing::dmabuf_import_refused(window));
    state.transaction_pinned.clear();
    state.transaction_retiring_bytes = 0;
    drop(old);
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// External backing pool reservations or fence-held leases are not a hard
/// scene-size impossibility. Deferred native staging must avoid a wasted
/// Vulkan upload and resume when unrelated GPU image ownership retires.
#[test]
#[cfg(not(target_os = "macos"))]
fn temporary_window_gpu_budget_pressure_defers_native_without_hard_refusal() {
    if isolate_vulkan_graphical_test(
        "temporary_window_gpu_budget_pressure_defers_native_without_hard_refusal",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let bytes = 48u32 * 44 * 4;
    let pressured = GpuCharge::reserve(
        window as usize,
        WINDOW_GPU_PAYLOAD_BUDGET - u64::from(bytes),
    )
    .unwrap();
    let view = appkit::view::Frame {
        x: 12,
        y: 16,
        width: 48,
        height: 44,
        ..whole
    };
    let native = ScenePlacement {
        opacity_groups: Vec::new(),
        id: (1usize << (usize::BITS - 1)) | 0x91ab,
        frame: DirectFrame::from_frame(&view),
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new([0, 0, 255, 255].repeat((48 * 44) as usize)),
            revision: 1,
            dmabuf: None,
            dma_sample: None,
        }),
    };
    let before = NATIVE_IMAGE_UPLOAD_ATTEMPTS.load(Ordering::Relaxed);
    let rejected = NATIVE_HARD_BUDGET_REFUSALS.load(Ordering::Relaxed);
    assert_eq!(
        stage_native_scene_source(&mut state, &native),
        Ok(false),
        "temporary ledger ownership pressure was mistaken for an impossible source"
    );
    assert_eq!(NATIVE_IMAGE_UPLOAD_ATTEMPTS.load(Ordering::Relaxed), before);
    assert_eq!(
        NATIVE_HARD_BUDGET_REFUSALS.load(Ordering::Relaxed),
        rejected
    );
    drop(pressured);
    assert_eq!(stage_native_scene_source(&mut state, &native), Ok(true));
    state.synchronize_scene(window as usize, &[native]);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            whole.width,
            whole.height,
            24,
            30
        ),
        [255, 0, 0, 255]
    );
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// Real Vulkan/WSI regression: six 4096² native CPU images overlap a
/// 160×120 Metal window. Each retains only its exact visible crop, so no
/// valid topmost source is dropped by the scene budget, and WSI can repaint.
#[test]
#[cfg(not(target_os = "macos"))]
fn large_cpu_native_views_crop_to_visible_window_and_keep_wsi_progress() {
    if isolate_vulkan_graphical_test(
        "large_cpu_native_views_crop_to_visible_window_and_keep_wsi_progress",
    ) {
        return;
    }
    let _retire_guard = AwaitTestWindowRetirements;
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let window = sdl3::video::create_window(c"CPU hard-budget WSI", 160, 120, 0).unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).expect("owner SDL snapshot");
    let metal = test_render_target(gpu, 160, 120).unwrap();
    let whole = test_full_frame(window, 160, 120);
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, metal.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .unwrap();
    let metal_id = window as usize;
    assert_eq!(present(gpu, metal_id, &whole, &metal, &[]), Ok(true));
    let entry = SWAPCHAINS
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get(&metal_id)
        .unwrap()
        .clone();
    let base = test_opaque_metal_placement(metal_id, DirectFrame::from_frame(&whole));
    let huge = appkit::view::Frame {
        width: 4096,
        height: 4096,
        ..whole
    };
    let mut scene = vec![base.clone()];
    for index in 0..6usize {
        let id = (1usize << (usize::BITS - 1)) | (0x41000 + index);
        scene.push(ScenePlacement {
            opacity_groups: Vec::new(),
            id,
            frame: DirectFrame::from_frame(&huge),
            opacity: 1.0,
            opaque: false,
            clip: None,
            rounded: vec![],
            occluded: Some(vec![]),
            native: Some(scene::NativeCpuSceneSource {
                cpu_capture: None,
                bytes: Arc::new([index as u8, 0, 255, 255].repeat(4096 * 4096)),
                revision: index as u64 + 1,
                dmabuf: None,
                dma_sample: None,
            }),
        });
    }
    let before_refused = NATIVE_HARD_BUDGET_REFUSALS.load(Ordering::Relaxed);
    let before_upload = NATIVE_IMAGE_UPLOAD_ATTEMPTS.load(Ordering::Relaxed);
    *entry.pending_scene.lock().unwrap() = Some(scene.clone());
    ensure_layer_compositor_worker(window as usize, &entry);
    wake_compositor_worker(&entry);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let refused = NATIVE_HARD_BUDGET_REFUSALS.load(Ordering::Relaxed);
        let settled = entry.pending_scene.lock().unwrap().is_none();
        let state = entry.state.lock().unwrap();
        let ready = state
            .scene_layers
            .iter()
            .filter(|layer| layer.native_revision.is_some())
            .count();
        if refused == before_refused && settled && ready == 6 && !state.composite_dirty {
            assert_eq!(entry.present_failure.load(Ordering::Relaxed), 0);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "WSI worker stalled with window-clipped native sources: ready={ready}, refused={refused}, settled={settled}"
        );
        drop(state);
        std::thread::sleep(Duration::from_millis(10));
    }
    let initial_frame = entry.state.lock().unwrap().frame;
    let actual = {
        let state = entry.state.lock().unwrap();
        let composite = state.composite.as_ref().expect("composed WSI GPU target");
        read_scene_pixel(gpu, composite.image, whole.width, whole.height, 40, 40)
    };
    assert_eq!(
        actual,
        [255, 0, 5, 255],
        "cropped last native foreground must win over earlier sources"
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        NATIVE_HARD_BUDGET_REFUSALS.load(Ordering::Relaxed),
        before_refused,
        "fully offscreen native texels should not trigger GPU budget refusal"
    );
    let uploaded = NATIVE_IMAGE_UPLOAD_ATTEMPTS.load(Ordering::Relaxed);
    assert!(uploaded > before_upload);
    // Recompose static metal after changing only its alpha, with the identical
    // rejected CPU revision still supplied. No repeated failed large upload.
    let changed = ScenePlacement {
        opacity_groups: Vec::new(),
        opacity: 0.6,
        ..base
    };
    scene[0] = changed;
    *entry.pending_scene.lock().unwrap() = Some(scene);
    wake_compositor_worker(&entry);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = entry.state.lock().unwrap();
        if entry.pending_scene.lock().unwrap().is_none()
            && state
                .scene_placements
                .iter()
                .find(|p| p.id == metal_id)
                .is_some_and(|p| p.opacity == 0.6)
            && !state.composite_dirty
            && state.frame > initial_frame
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "unchanged rejected CPU source starved subsequent WSI scene repaint"
        );
        drop(state);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        NATIVE_HARD_BUDGET_REFUSALS.load(Ordering::Relaxed),
        before_refused
    );
    assert_eq!(
        NATIVE_IMAGE_UPLOAD_ATTEMPTS.load(Ordering::Relaxed),
        uploaded
    );
    forget_window(window);
    drop(entry);
    sdl3::video::destroy_window(window);
    gpu.destroy_image(metal);
}

/// A native opaque view is an authoritative occlusion source even before
/// a CPU backing store has arrived. Moving/removing it must repaint a static
/// retained Metal image without a new Metal drawable submission.
#[test]
#[cfg(not(target_os = "macos"))]
fn live_native_occlusion_updates_without_new_drawable_or_initial_backing() {
    if isolate_vulkan_graphical_test(
        "live_native_occlusion_updates_without_new_drawable_or_initial_backing",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, metal.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .unwrap();
    let id = 707usize;
    let blocked = Region {
        x: 12,
        y: 17,
        width: 35,
        height: 30,
    };
    let placement = ScenePlacement {
        opacity_groups: Vec::new(),
        id,
        frame: DirectFrame::from_frame(&whole),
        opacity: 1.0,
        opaque: true,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![blocked]),
        native: None,
    };
    state.layers.push(id);
    state.synchronize_scene(window as usize, std::slice::from_ref(&placement));
    assert!(state.live_scene_regions(id, &whole).is_some());
    if matches!(
        state.present_mode,
        vk::PresentModeKHR::IMMEDIATE | vk::PresentModeKHR::MAILBOX
    ) {
        assert!(
            !fast_direct_scene(
                &state,
                id,
                &whole,
                state.live_scene_regions(id, &whole).as_deref(),
                true
            ),
            "native covering view absent from fast WSI eligibility before any backing upload"
        );
    }
    assert!(
        !state.layers.contains(&(window as usize)),
        "test backing store had registered too early"
    );
    let regions = state.live_scene_regions(id, &whole);
    assert_eq!(
        state.stage_composite_layer(
            id,
            &whole,
            &metal,
            regions.as_deref(),
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.note_scene_frame(id, &whole);
    state.idle();
    let pixel = |state: &Swapchain, x, y| {
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            whole.width,
            whole.height,
            x,
            y,
        )
    };
    assert_eq!(pixel(&state, 20, 20), [0, 0, 0, 0]);
    assert_eq!(pixel(&state, 90, 50), [0, 255, 0, 255]);
    let moved = ScenePlacement {
        opacity_groups: Vec::new(),
        occluded: Some(vec![Region {
            x: 85,
            y: 40,
            width: 30,
            height: 35,
        }]),
        ..placement.clone()
    };
    assert!(state.synchronize_scene(window as usize, std::slice::from_ref(&moved)));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        pixel(&state, 20, 20),
        [0, 255, 0, 255],
        "moved native view left a stale hole"
    );
    assert_eq!(
        pixel(&state, 90, 50),
        [0, 0, 0, 0],
        "new opaque native view area reveals Metal"
    );
    let removed = ScenePlacement {
        opacity_groups: Vec::new(),
        occluded: Some(vec![]),
        ..moved
    };
    assert!(state.synchronize_scene(window as usize, &[removed]));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    // Unoccluded IMMEDIATE/MAILBOX may restore direct WSI rather than keep a
    // freshly composited accumulator. Check content only if still retained.
    if state.direct.is_none() {
        assert_eq!(pixel(&state, 90, 50), [0, 255, 0, 255]);
    }
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// A large CPU-backed NSView is clipped to a small visible slice without
/// losing view-local texture coordinates or falling underneath a static Metal
/// drawable. Verify the real AppKit capture bytes in the *final* Vulkan image.
#[test]
#[cfg(not(target_os = "macos"))]
fn oversized_cpu_native_view_keeps_foreground_in_final_vulkan_framebuffer() {
    if isolate_vulkan_graphical_test(
        "oversized_cpu_native_view_keeps_foreground_in_final_vulkan_framebuffer",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    appkit::bootstrap();
    let cls = objc::class::lookup(c"NSView").expect("native NSView");
    let native_view = objc::object::create_instance(cls, 0);
    // Source width 6016 x height 3384 exceeds the 64 MiB AppKit capture cap.
    let mut pixels = [0u8, 0, 255, 255].repeat(6016);
    for (index, texel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        if index >= 3000 {
            texel.copy_from_slice(&[255, 0, 0, 255]); // blue in BGRA
        }
    }
    appkit::backing::set_overlay(
        native_view,
        appkit::backing::Overlay {
            width: 6016,
            height: 1,
            picture: appkit::backing::Picture::Pixels(pixels),
        },
    );
    let capture = |left| {
        appkit::backing::native_scene_pixels_cropped(
            native_view,
            6016,
            3384,
            appkit::backing::Rect {
                x: left,
                y: 40,
                width: 80,
                height: 60,
            },
        )
        .expect("window-visible native crop")
    };
    let first = capture(3000);
    assert_eq!(first.0.len(), 80 * 60 * 4);
    assert_eq!(&first.0[..4], &[255, 0, 0, 255]);
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, metal.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .unwrap();
    let metal_id = 0x6d01usize;
    let native_id = (1usize << (usize::BITS - 1)) | (native_view as usize);
    let bottom = test_opaque_metal_placement(metal_id, DirectFrame::from_frame(&whole));
    let native_frame = appkit::view::Frame {
        x: 35,
        y: 20,
        width: 80,
        height: 60,
        ..whole
    };
    let foreground = |capture: (Arc<Vec<u8>>, u64)| ScenePlacement {
        opacity_groups: Vec::new(),
        id: native_id,
        frame: DirectFrame::from_frame(&native_frame),
        opacity: 1.0,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: capture.0,
            revision: capture.1,
            dmabuf: None,
            dma_sample: None,
        }),
    };
    let native = foreground(first);
    state.layers.push(metal_id);
    state.synchronize_scene(window as usize, &[bottom.clone(), native.clone()]);
    assert_eq!(
        state.stage_composite_layer(
            metal_id,
            &whole,
            &metal,
            None,
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.note_scene_frame(metal_id, &whole);
    assert_eq!(stage_native_scene_source(&mut state, &native), Ok(true));
    state.synchronize_scene(window as usize, &[bottom.clone(), native]);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let read = |s: &Swapchain, x, y| {
        read_scene_pixel(
            gpu,
            s.composite.as_ref().unwrap().image,
            whole.width,
            whole.height,
            x,
            y,
        )
    };
    assert_eq!(read(&state, 40, 30), [0, 0, 255, 255]);
    assert_eq!(
        read(&state, 10, 10),
        [0, 255, 0, 255],
        "oversize native was flattened beneath Metal or punched a hole"
    );
    // Moving the giant view changes the crop but not the Metal drawable.
    let changed = foreground(capture(0));
    assert_eq!(stage_native_scene_source(&mut state, &changed), Ok(true));
    state.synchronize_scene(window as usize, &[bottom, changed]);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(read(&state, 40, 30), [255, 0, 0, 255]);
    assert_eq!(read(&state, 10, 10), [0, 255, 0, 255]);
    appkit::backing::remove_overlay(native_view);
    objc::arc::release(native_view);
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// Final Vulkan pixels for an opaque native source with a *smaller rectangular*
/// intermediate CALayer clip. AppKit independently verifies that it projects
/// that exact rectangle into this scene metadata. Metal outside the mask must
/// remain visible, including after the CA rectangle changes without a drawable.
#[test]
#[cfg(not(target_os = "macos"))]
fn rectangular_intermediate_ca_clip_does_not_erase_lower_metal_pixels() {
    if isolate_vulkan_graphical_test(
        "rectangular_intermediate_ca_clip_does_not_erase_lower_metal_pixels",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, metal.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .unwrap();
    let metal_id = 0x6c01usize;
    let native_id = (1usize << (usize::BITS - 1)) | 0x6c02;
    let frame = appkit::view::Frame {
        x: 10,
        y: 10,
        width: 60,
        height: 45,
        ..whole
    };
    let initial_clip = appkit::view::Frame {
        x: 20,
        y: 20,
        width: 15,
        height: 12,
        ..whole
    };
    let clip_region = |clip: appkit::view::Frame| Region {
        x: clip.x,
        y: clip.y,
        width: clip.width as i32,
        height: clip.height as i32,
    };
    let bottom = |clip| ScenePlacement {
        opacity_groups: Vec::new(),
        id: metal_id,
        frame: DirectFrame::from_frame(&whole),
        opacity: 1.0,
        opaque: true,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![clip_region(clip)]),
        native: None,
    };
    let native_source = scene::NativeCpuSceneSource {
        cpu_capture: None,
        bytes: Arc::new([0u8, 0, 255, 255].repeat((frame.width * frame.height) as usize)),
        revision: 1,
        dmabuf: None,
        dma_sample: None,
    };
    let native = |clip| ScenePlacement {
        opacity_groups: Vec::new(),
        id: native_id,
        frame: DirectFrame::from_frame(&frame),
        opacity: 1.0,
        opaque: false,
        clip: Some(DirectFrame::from_frame(&clip)),
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(native_source.clone()),
    };
    state.layers.push(metal_id);
    state.synchronize_scene(
        window as usize,
        &[bottom(initial_clip), native(initial_clip)],
    );
    let regions = state.live_scene_regions(metal_id, &whole);
    assert_eq!(
        state.stage_composite_layer(
            metal_id,
            &whole,
            &metal,
            regions.as_deref(),
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true)),
    );
    state.note_scene_frame(metal_id, &whole);
    assert_eq!(
        stage_native_scene_source(&mut state, &native(initial_clip)),
        Ok(true)
    );
    state.synchronize_scene(
        window as usize,
        &[bottom(initial_clip), native(initial_clip)],
    );
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let pixel = |state: &Swapchain, x, y| {
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            whole.width,
            whole.height,
            x,
            y,
        )
    };
    assert_eq!(pixel(&state, 22, 22), [255, 0, 0, 255]);
    assert_eq!(pixel(&state, 15, 15), [0, 255, 0, 255]);
    assert_eq!(
        pixel(&state, 38, 22),
        [0, 255, 0, 255],
        "native NSView rectangle punched a hole outside its intermediate CA mask"
    );
    let changed_clip = appkit::view::Frame {
        x: 20,
        y: 20,
        width: 25,
        height: 18,
        ..whole
    };
    assert!(state.synchronize_scene(
        window as usize,
        &[bottom(changed_clip), native(changed_clip)]
    ));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(pixel(&state, 38, 22), [255, 0, 0, 255]);
    assert_eq!(pixel(&state, 46, 22), [0, 255, 0, 255]);
    assert_eq!(pixel(&state, 15, 15), [0, 255, 0, 255]);
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// A CA-only property update must withdraw an already-published direct WSI
/// image and recompose its *retained* last drawable with the new mask.
#[test]
#[cfg(not(target_os = "macos"))]
fn direct_wsi_static_drawable_recomposes_after_ca_mask_changes() {
    if isolate_vulkan_graphical_test("direct_wsi_static_drawable_recomposes_after_ca_mask_changes")
    {
        return;
    }
    let Some((window, gpu, mut state, green, whole)) = v10_fifo_fixture() else {
        return;
    };
    if !matches!(
        state.present_mode,
        vk::PresentModeKHR::IMMEDIATE | vk::PresentModeKHR::MAILBOX
    ) {
        gpu.destroy_image(green);
        drop(state);
        sdl3::video::destroy_window(window);
        return;
    }
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, green.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .expect("green source");
    let id = 404;
    let base = ScenePlacement {
        opacity_groups: Vec::new(),
        id,
        frame: DirectFrame::from_frame(&whole),
        opacity: 1.0,
        opaque: true,
        clip: None,
        rounded: vec![],
        occluded: None,
        native: None,
    };
    state.layers.push(id);
    state.synchronize_scene(window as usize, std::slice::from_ref(&base));
    assert!(fast_direct_scene(&state, id, &whole, None, true));
    state.queue_direct(
        id,
        &whole,
        PresentSource::from(&green),
        &mut SourceOwner(None),
        None,
    );
    state
        .flush_composite(window)
        .expect("actual direct WSI present");
    assert!(
        state.direct.is_some(),
        "fixture did not enter actual direct path"
    );
    assert!(state.composite.is_none(), "unexpected early accumulator");
    // A CA opacity group cannot bypass the group shader just because its
    // individual retained drawable remains opaque. Replay the same direct
    // WSI source into a half-opacity group without asking the guest to draw.
    let grouped = ScenePlacement {
        opacity_groups: vec![appkit::view::OpacityGroup {
            id: 0x4040,
            opacity: 0.5,
        }],
        ..base.clone()
    };
    assert!(state.synchronize_scene(window as usize, &[grouped]));
    assert!(!fast_direct_scene(&state, id, &whole, None, true));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert!(state.direct.is_none());
    let grouped_pixel = read_scene_pixel(
        gpu,
        state.composite.as_ref().unwrap().image,
        whole.width,
        whole.height,
        80,
        60,
    );
    assert!(
        grouped_pixel
            .iter()
            .zip([0u8, 128, 0, 128])
            .all(|(a, b)| a.abs_diff(b) <= 1),
        "a direct WSI image escaped newly added Core Animation opacity: {grouped_pixel:?}"
    );
    assert!(state.synchronize_scene(window as usize, std::slice::from_ref(&base)));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert!(
        state.direct.is_some(),
        "restoring opaque CA state did not restore direct WSI"
    );

    let rounded = scene::RoundedClip {
        frame: DirectFrame::from_frame(&whole),
        radius: 35.0,
    };
    let new = ScenePlacement {
        opacity_groups: Vec::new(),
        rounded: vec![rounded],
        ..base.clone()
    };
    assert!(state.synchronize_scene(window as usize, std::slice::from_ref(&new)));
    assert!(state.scene_repaint_needed);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert!(
        state.direct.is_none(),
        "masked scene was left in direct mode"
    );
    let pixel = |state: &Swapchain, x, y| {
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            whole.window_width,
            whole.window_height,
            x,
            y,
        )
    };
    assert_eq!(
        pixel(&state, 0, 0),
        [0, 0, 0, 0],
        "rounded corner leaked direct pixels"
    );
    assert_eq!(pixel(&state, 80, 60), [0, 255, 0, 255]);
    // Removing the rounded mask, then restricting the rectangular ancestor,
    // still updates the same preserved green drawable without new submissions.
    let unrounded = ScenePlacement {
        opacity_groups: Vec::new(),
        rounded: vec![],
        ..new
    };
    assert!(state.synchronize_scene(window as usize, std::slice::from_ref(&unrounded)));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert!(
        state.direct.is_some(),
        "unmasked layer did not restore direct WSI"
    );
    let rect = DirectFrame::from_frame(&appkit::view::Frame {
        x: 20,
        y: 15,
        width: 110,
        height: 75,
        ..whole
    });
    assert!(state.synchronize_scene(
        window as usize,
        &[ScenePlacement {
            opacity_groups: Vec::new(),
            clip: Some(rect),
            ..unrounded
        }]
    ));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert!(
        state.direct.is_none(),
        "rectangular mask left the old direct WSI image"
    );
    assert_eq!(pixel(&state, 5, 5), [0, 0, 0, 0]);
    assert_eq!(pixel(&state, 80, 60), [0, 255, 0, 255]);
    drop(state);
    gpu.destroy_image(green);
    sdl3::video::destroy_window(window);
}

/// A window's CPU store that presented before its Metal layer lies below that
/// layer, so it must not force a full-window opaque drawable through the
/// accumulator. The store itself never goes direct over the drawable.
#[test]
#[cfg(not(target_os = "macos"))]
fn occluded_window_store_keeps_full_window_drawable_direct() {
    if isolate_vulkan_graphical_test("occluded_window_store_keeps_full_window_drawable_direct") {
        return;
    }
    let Some((window, gpu, mut state, image, whole)) = v10_fifo_fixture() else {
        return;
    };
    if matches!(
        state.present_mode,
        vk::PresentModeKHR::IMMEDIATE | vk::PresentModeKHR::MAILBOX
    ) {
        let id = 405;
        state.layers.push(window as usize);
        state.layers.push(id);
        state.synchronize_scene(
            window as usize,
            &[ScenePlacement {
                opacity_groups: Vec::new(),
                id,
                frame: DirectFrame::from_frame(&whole),
                opacity: 1.0,
                opaque: true,
                clip: None,
                rounded: vec![],
                occluded: None,
                native: None,
            }],
        );
        assert!(fast_direct_scene(&state, id, &whole, None, true));
        assert!(!fast_direct_scene(
            &state,
            window as usize,
            &whole,
            None,
            true
        ));
    }
    drop(state);
    gpu.destroy_image(image);
    sdl3::video::destroy_window(window);
}

/// Rounded masks from one or more ancestor CALayers multiply the Metal
/// drawable's alpha, including its corners. No CPU surface readback or
/// second Metal drawable is required when the CA geometry changes.
#[test]
#[cfg(not(target_os = "macos"))]
fn opaque_metal_content_respects_nested_rounded_ca_clips() {
    if isolate_vulkan_graphical_test("opaque_metal_content_respects_nested_rounded_ca_clips") {
        return;
    }
    let Some((window, gpu, mut state, green, whole)) = v10_fifo_fixture() else {
        return;
    };
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, green.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .expect("green full-window source");
    let first = DirectFrame::from_frame(&appkit::view::Frame {
        x: 10,
        y: 10,
        width: 120,
        height: 100,
        ..whole
    });
    let second = DirectFrame::from_frame(&appkit::view::Frame {
        x: 30,
        y: 25,
        width: 80,
        height: 65,
        ..whole
    });
    let id = 22;
    state.layers.push(id);
    assert!(matches!(
        state.present_mode,
        vk::PresentModeKHR::IMMEDIATE
            | vk::PresentModeKHR::MAILBOX
            | vk::PresentModeKHR::FIFO
            | vk::PresentModeKHR::FIFO_RELAXED
    ));
    // This is the exact eligibility predicate used by the live present path;
    // in IMMEDIATE/MAILBOX, the otherwise eligible full-window opaque source
    // must stop being direct as soon as a rounded ancestor mask appears.
    if matches!(
        state.present_mode,
        vk::PresentModeKHR::IMMEDIATE | vk::PresentModeKHR::MAILBOX
    ) {
        assert!(fast_direct_scene(&state, id, &whole, None, true));
    }
    state.synchronize_scene(
        window as usize,
        &[ScenePlacement {
            opacity_groups: Vec::new(),
            id,
            frame: DirectFrame::from_frame(&whole),
            opacity: 1.0,
            opaque: true,
            clip: Some(first),
            rounded: vec![scene::RoundedClip {
                frame: first,
                radius: 30.0,
            }],
            occluded: None,
            native: None,
        }],
    );
    assert!(
        !fast_direct_scene(&state, id, &whole, None, true),
        "rounded CA mask erroneously permits WSI direct presentation"
    );
    assert_eq!(
        state.stage_composite_layer(id, &whole, &green, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.note_scene_frame(id, &whole);
    state.idle();
    let pixel = |state: &Swapchain, x, y| {
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            whole.window_width,
            whole.window_height,
            x,
            y,
        )
    };
    assert_eq!(
        pixel(&state, 10, 10),
        [0, 0, 0, 0],
        "round corner not removed"
    );
    assert_eq!(
        pixel(&state, 70, 10),
        [0, 255, 0, 255],
        "straight upper edge missing"
    );
    assert_eq!(pixel(&state, 70, 60), [0, 255, 0, 255]);
    assert_eq!(
        pixel(&state, 132, 60),
        [0, 0, 0, 0],
        "rectangular clip failed"
    );
    // Introduce a nested rounded ancestor without any new Metal drawable.
    assert!(state.synchronize_scene(
        window as usize,
        &[ScenePlacement {
            opacity_groups: Vec::new(),
            id,
            frame: DirectFrame::from_frame(&whole),
            opacity: 1.0,
            opaque: true,
            clip: Some(second),
            rounded: vec![
                scene::RoundedClip {
                    frame: first,
                    radius: 30.0
                },
                scene::RoundedClip {
                    frame: second,
                    radius: 20.0
                },
            ],
            occluded: None,
            native: None,
        }]
    ));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        pixel(&state, 30, 25),
        [0, 0, 0, 0],
        "nested corner not clipped"
    );
    assert_eq!(
        pixel(&state, 70, 25),
        [0, 255, 0, 255],
        "nested straight edge missing"
    );
    assert_eq!(pixel(&state, 70, 60), [0, 255, 0, 255]);
    assert_eq!(pixel(&state, 20, 50), [0, 0, 0, 0]);
    // Five nested masks also work: no fixed-size push-constant clipping cap.
    let nested = vec![
        scene::RoundedClip {
            frame: second,
            radius: 20.0,
        };
        5
    ];
    assert!(state.synchronize_scene(
        window as usize,
        &[ScenePlacement {
            opacity_groups: Vec::new(),
            id,
            frame: DirectFrame::from_frame(&whole),
            opacity: 1.0,
            opaque: true,
            clip: Some(second),
            rounded: nested,
            occluded: None,
            native: None,
        }]
    ));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(pixel(&state, 30, 25), [0, 0, 0, 0]);
    assert_eq!(pixel(&state, 70, 60), [0, 255, 0, 255]);
    drop(state);
    gpu.destroy_image(green);
    sdl3::video::destroy_window(window);
}

/// Exact retained-GPU output from an oversize-radius CA mask, including a
/// nested intermediary mask, must match native M2 CARenderer edge samples.
#[test]
#[cfg(not(target_os = "macos"))]
fn gpu_oversized_ca_radius_preserves_native_rounded_pixels() {
    if isolate_vulkan_graphical_test("gpu_oversized_ca_radius_preserves_native_rounded_pixels") {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Some((window, gpu, mut state, underlay, full)) = v10_fifo_fixture() else {
        return;
    };
    let tile = appkit::view::Frame {
        x: 30,
        y: 24,
        width: 12,
        height: 12,
        ..full
    };
    let red = test_render_target(gpu, 12, 12).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, underlay.image, [0., 1., 0., 1.]);
        cmd_test_clear_color(device, cb, red.image, [0., 0., 1., 1.]);
    })
    .unwrap();
    let green = test_opaque_metal_placement(0x400, DirectFrame::from_frame(&full));
    let mask = scene::RoundedClip {
        frame: DirectFrame::from_frame(&tile),
        radius: 8.0,
    };
    let foreground = ScenePlacement {
        opacity_groups: vec![],
        id: 0x401,
        frame: DirectFrame::from_frame(&tile),
        opacity: 1.0,
        opaque: true,
        clip: Some(DirectFrame::from_frame(&tile)),
        rounded: vec![mask],
        occluded: Some(vec![]),
        native: None,
    };
    state.layers.extend([green.id, foreground.id]);
    state.synchronize_scene(window as usize, &[green.clone(), foreground.clone()]);
    assert_eq!(
        state.stage_composite_source(
            green.id,
            &full,
            PresentSource::from(&underlay),
            None,
            &mut SourceOwner(None),
            false,
            true,
        ),
        Ok(Some(true))
    );
    assert_eq!(
        state.stage_composite_source(
            foreground.id,
            &tile,
            PresentSource::from(&red),
            None,
            &mut SourceOwner(None),
            false,
            true,
        ),
        Ok(Some(true))
    );
    state.idle();
    let sample = |state: &Swapchain, x, y| {
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            full.window_width,
            full.window_height,
            x,
            y,
        )
    };
    assert_eq!(sample(&state, 30, 24), [0, 255, 0, 255]);
    assert_eq!(sample(&state, 36, 30), [0, 0, 255, 255]);
    // Apple M2/macOS 26.5 native r8 CALayer on 12x12 above green Metal:
    // (local 4,0) BGRA [0,183,72,255], not the old radius-clamped ~204-red pixel.
    let apple = [0u8, 183, 72, 255];
    let actual = sample(&state, 34, 24);
    assert!(
        actual.iter().zip(apple).all(|(a, b)| a.abs_diff(b) <= 18),
        "GPU radius8 final pixel {actual:?}, native reference {apple:?}"
    );

    let square = ScenePlacement {
        rounded: vec![],
        ..foreground.clone()
    };
    assert!(state.synchronize_scene(window as usize, &[green.clone(), square]));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        sample(&state, 34, 24),
        [0, 0, 255, 255],
        "mask metadata change required a new drawable"
    );

    // Retain two distinct rounded ancestors: the intermediate ordinary CA
    // mask and the NSView-backed layer's own mask share source-space pixels.
    let nested = ScenePlacement {
        rounded: vec![mask, mask],
        ..foreground
    };
    assert!(state.synchronize_scene(window as usize, &[green.clone(), nested]));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let edge = sample(&state, 34, 24);
    assert!(
        edge[2] < actual[2] && edge[1] > actual[1],
        "nested rounded masks did not multiply GPU edge coverage: {edge:?}"
    );
    assert_eq!(sample(&state, 36, 30), [0, 0, 255, 255]);
    drop(state);
    gpu.destroy_image(red);
    gpu.destroy_image(underlay);
    sdl3::video::destroy_window(window);
}

/// The CPU backing image, including its pixels covered by Metal, is the
/// bottom scene source. This is not the opaque-AppKit-over-Metal mask.
#[test]
#[cfg(not(target_os = "macos"))]
fn translucent_metal_composites_over_complete_cpu_store() {
    if isolate_vulkan_graphical_test("translucent_metal_composites_over_complete_cpu_store") {
        return;
    }
    let Some((window, gpu, mut state, background, whole)) = v10_fifo_fixture() else {
        return;
    };
    let overlay = test_render_target(gpu, 60, 40).expect("Metal foreground image");
    for (img, color) in [
        (&background, [0.0, 0.0, 1.0, 1.0]),
        (&overlay, [0.0, 1.0, 0.0, 1.0]),
    ] {
        gpu.one_shot(|device, cb| unsafe {
            cmd_test_clear_color(device, cb, img.image, color);
        })
        .expect("fill source");
    }
    let foreground = appkit::view::Frame {
        x: 30,
        y: 20,
        width: 60,
        height: 40,
        ..whole
    };
    let metal_coverage = Region {
        x: 30,
        y: 20,
        width: 60,
        height: 40,
    };
    assert!(
        scene_regions(
            window as usize,
            window as usize,
            true,
            &whole,
            &[metal_coverage]
        )
        .is_none(),
        "Metal coverage wrongly excised CPU pixels needed under translucent content"
    );
    assert!(
        scene_regions(
            window as usize,
            window as usize,
            false,
            &whole,
            &[metal_coverage]
        )
        .is_some(),
        "non-scene legacy occlusion contract unexpectedly changed"
    );
    let metal_id = 100usize;
    let cpu_id = window as usize;
    state.layers.push(cpu_id);
    state.note_scene_frame(cpu_id, &whole);
    assert_eq!(
        state.stage_composite_layer(
            cpu_id,
            &whole,
            &background,
            None,
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.layers.push(metal_id);
    state.synchronize_scene(
        cpu_id,
        &[ScenePlacement {
            opacity_groups: Vec::new(),
            id: metal_id,
            frame: DirectFrame::from_frame(&foreground),
            opacity: 0.5,
            opaque: true,
            clip: None,
            rounded: Vec::new(),
            occluded: None,
            native: None,
        }],
    );
    assert_eq!(
        state.stage_composite_layer(
            metal_id,
            &foreground,
            &overlay,
            None,
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.idle();
    let image = state.composite.as_ref().unwrap().image;
    assert_eq!(
        read_scene_pixel(gpu, image, whole.window_width, whole.window_height, 5, 5),
        [0, 0, 255, 255]
    );
    let half = read_scene_pixel(gpu, image, whole.window_width, whole.window_height, 55, 35);
    assert_eq!(half[0], 0);
    assert!((126..=129).contains(&half[1]) && (126..=129).contains(&half[2]));
    assert_eq!(half[3], 255);
    // The foreground drawable remains static. Only its CA opacity changes.
    assert!(state.synchronize_scene(
        cpu_id,
        &[ScenePlacement {
            opacity_groups: Vec::new(),
            id: metal_id,
            frame: DirectFrame::from_frame(&foreground),
            opacity: 0.25,
            opaque: true,
            clip: None,
            rounded: Vec::new(),
            occluded: None,
            native: None,
        }]
    ));
    assert_eq!(state.redisplay_scene(cpu_id), Ok(true));
    state.idle();
    let quarter = read_scene_pixel(gpu, image, whole.window_width, whole.window_height, 55, 35);
    assert_eq!(quarter[0], 0);
    assert!(
        (62..=65).contains(&quarter[1]),
        "lost green over blue: {quarter:?}"
    );
    assert!(
        (190..=193).contains(&quarter[2]),
        "lost CPU blue behind Metal: {quarter:?}"
    );
    assert_eq!(quarter[3], 255);
    // Genuine opaque upper AppKit regions continue to mask a Metal drawable.
    assert!(scene_regions(metal_id, cpu_id, true, &foreground, &[metal_coverage]).is_some());
    drop(state);
    gpu.destroy_image(background);
    gpu.destroy_image(overlay);
    sdl3::video::destroy_window(window);
}

/// Switching a lone full-window drawable to 50% CA alpha must recompose its
/// last frame even without a new Metal present: both direct and FIFO paths.
#[test]
#[cfg(not(target_os = "macos"))]
fn lone_metal_alpha_change_invalidates_fifo_and_direct_wsi_fast_paths() {
    if isolate_vulkan_graphical_test(
        "lone_metal_alpha_change_invalidates_fifo_and_direct_wsi_fast_paths",
    ) {
        return;
    }
    let Some((window, gpu, mut state, blue, whole)) = v10_fifo_fixture() else {
        return;
    };
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, blue.image, [0.0, 0.0, 1.0, 1.0]);
    })
    .expect("full-window blue");
    let id = 101usize;
    state.layers.push(id);
    state.note_scene_frame(id, &whole);
    if matches!(
        state.present_mode,
        vk::PresentModeKHR::FIFO | vk::PresentModeKHR::FIFO_RELAXED
    ) {
        assert_eq!(
            state.stage_composite_layer(id, &whole, &blue, None, &mut SourceOwner(None), false),
            Ok(Some(true))
        );
        assert!(
            state.scene_layers.is_empty(),
            "FIFO fast path unexpectedly allocated a retained source"
        );
    } else {
        state.queue_direct(
            id,
            &whole,
            PresentSource::from(&blue),
            &mut SourceOwner(None),
            None,
        );
        assert!(state.direct.is_some(), "test did not exercise direct WSI");
    }
    assert!(state.synchronize_scene(
        window as usize,
        &[ScenePlacement {
            opacity_groups: Vec::new(),
            id,
            frame: DirectFrame::from_frame(&whole),
            opacity: 0.5,
            opaque: true,
            clip: None,
            rounded: Vec::new(),
            occluded: None,
            native: None,
        }]
    ));
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert!(
        state.direct.is_none(),
        "translucent scene incorrectly remains direct WSI"
    );
    let image = state.composite.as_ref().expect("CA blend compositor").image;
    let half = read_scene_pixel(gpu, image, whole.window_width, whole.window_height, 40, 40);
    assert_eq!(half[0], 0);
    assert_eq!(half[1], 0);
    assert!(
        (126..=129).contains(&half[2]),
        "CA alpha bypassed shader: {half:?}"
    );
    assert!(
        (126..=129).contains(&half[3]),
        "alpha not premultiplied: {half:?}"
    );
    // -setOpaque:NO is independent from opacity=1. A drawable containing
    // half-transparent premultiplied pixels must still use the scene shader.
    assert!(state.synchronize_scene(
        window as usize,
        &[ScenePlacement {
            opacity_groups: Vec::new(),
            id,
            frame: DirectFrame::from_frame(&whole),
            opacity: 1.0,
            opaque: false,
            clip: None,
            rounded: Vec::new(),
            occluded: None,
            native: None,
        }]
    ));
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, blue.image, [0.0, 0.0, 0.5, 0.5]);
    })
    .expect("half-transparent drawable");
    assert_eq!(
        state.stage_composite_layer(id, &whole, &blue, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.idle();
    assert!(
        state.direct.is_none(),
        "non-opaque layer bypassed the compositor"
    );
    let half_alpha = read_scene_pixel(gpu, image, whole.window_width, whole.window_height, 40, 40);
    assert_eq!(half_alpha[0], 0);
    assert_eq!(half_alpha[1], 0);
    assert!(
        (126..=129).contains(&half_alpha[2]),
        "source alpha lost: {half_alpha:?}"
    );
    assert!(
        (126..=129).contains(&half_alpha[3]),
        "source alpha lost: {half_alpha:?}"
    );
    drop(state);
    gpu.destroy_image(blue);
    sdl3::video::destroy_window(window);
}

/// Manual release-only 4K GPU staging benchmark. The control mirrors the
/// old incremental subrectangle blit, not an installed upstream binary. No
/// timing assertion: these numbers are hardware and driver specific.
#[test]
#[cfg(not(target_os = "macos"))]
fn benchmark_4k_static_background_with_animated_128px_foreground() {
    if std::env::var_os("ROSEMARY_BENCH_SCENE_4K").is_none() {
        return;
    }
    if isolate_vulkan_graphical_test(
        "benchmark_4k_static_background_with_animated_128px_foreground",
    ) {
        return;
    }
    let Some((window, gpu, mut state, unused_source, mut frame)) = v10_fifo_fixture() else {
        return;
    };
    gpu.destroy_image(unused_source);
    let (width, height) = (3840u32, 2160u32);
    state.extent = vk::Extent2D { width, height };
    state.requested = state.extent;
    frame.width = width;
    frame.height = height;
    frame.window_width = width;
    frame.window_height = height;
    let foreground = appkit::view::Frame {
        x: 1900,
        y: 1000,
        width: 128,
        height: 128,
        ..frame
    };
    let make_image =
        |w: u32, h: u32| test_render_target(gpu, w, h).expect("benchmark Vulkan image");
    let background = make_image(width, height);
    let overlay = make_image(128, 128);
    let control = make_image(width, height);
    for (image, color) in [
        (&background, [0.0, 0.0, 1.0, 1.0]),
        (&overlay, [0.0, 1.0, 0.0, 1.0]),
    ] {
        gpu.one_shot(|device, cb| unsafe {
            cmd_test_clear_color(device, cb, image.image, color);
        })
        .expect("initialize benchmark source");
    }
    let background_id = 1usize;
    let foreground_id = 2usize;
    state.layers.push(background_id);
    state.note_scene_frame(background_id, &frame);
    assert_eq!(
        state.stage_composite_layer(
            background_id,
            &frame,
            &background,
            None,
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.layers.push(foreground_id);
    assert_eq!(
        state.stage_composite_layer(
            foreground_id,
            &foreground,
            &overlay,
            None,
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.note_scene_frame(foreground_id, &foreground);
    state.idle();
    let control_update = || {
        gpu.one_shot(|device, cb| unsafe {
            let sync = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(control.image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            device.cmd_pipeline_barrier(
                cb,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[sync],
            );
            blit(
                device,
                cb,
                overlay.image,
                [
                    vk::Offset3D::default(),
                    vk::Offset3D {
                        x: 128,
                        y: 128,
                        z: 1,
                    },
                ],
                control.image,
                vk::ImageLayout::GENERAL,
                [
                    vk::Offset3D {
                        x: foreground.x,
                        y: foreground.y,
                        z: 0,
                    },
                    vk::Offset3D {
                        x: foreground.x + 128,
                        y: foreground.y + 128,
                        z: 1,
                    },
                ],
            );
        })
        .expect("control subrectangle GPU blit");
    };
    let mut retained_update = || {
        assert_eq!(
            state.stage_composite_layer(
                foreground_id,
                &foreground,
                &overlay,
                None,
                &mut SourceOwner(None),
                false
            ),
            Ok(Some(true))
        );
        state.idle();
    };
    // The work is repeated after full warmup to remove one-time allocation
    // and shader pipeline compilation from the per-frame latency figure.
    for _ in 0..6 {
        control_update();
        retained_update();
    }
    let iterations = 40;
    let mut control_us = Vec::new();
    let mut retained_us = Vec::new();
    for _ in 0..iterations {
        let start = Instant::now();
        control_update();
        control_us.push(start.elapsed().as_secs_f64() * 1e6);
        let start = Instant::now();
        retained_update();
        retained_us.push(start.elapsed().as_secs_f64() * 1e6);
    }
    let average = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;
    let mut control_p95 = control_us.clone();
    let mut retained_p95 = retained_us.clone();
    control_p95.sort_by(|a, b| a.total_cmp(b));
    retained_p95.sort_by(|a, b| a.total_cmp(b));
    eprintln!(
        "4K small-foreground benchmark: {iterations} serialized warmed-up samples, window={}x{}, overlay=128x128, WSI mode={:?}",
        width, height, state.present_mode
    );
    eprintln!(
        "4K upstream-style incremental blit proxy (NOT actual upstream binary): mean_us={:.1}, p95_us={:.1}",
        average(&control_us),
        control_p95[(iterations * 95 / 100) as usize]
    );
    eprintln!(
        "4K retained scene damage-aware redraw: mean_us={:.1}, p95_us={:.1}, ratio={:.2}x",
        average(&retained_us),
        retained_p95[(iterations * 95 / 100) as usize],
        average(&retained_us) / average(&control_us)
    );
    eprintln!(
        "4K static background footprint: {} bytes; overlay footprint: {} bytes",
        u64::from(width) * u64::from(height) * 4,
        128 * 128 * 4
    );
    drop(state);
    gpu.destroy_image(background);
    gpu.destroy_image(overlay);
    gpu.destroy_image(control);
    sdl3::video::destroy_window(window);
}

/// Clipping a single full-window Metal layer must not route it through the
/// unmasked legacy blit or the direct WSI fast path. Outside its ancestor's
/// rectangle the window accumulator must stay transparent.
#[test]
#[cfg(not(target_os = "macos"))]
fn one_layer_with_clipping_parent_keeps_outside_pixels_transparent() {
    if isolate_vulkan_graphical_test(
        "one_layer_with_clipping_parent_keeps_outside_pixels_transparent",
    ) {
        return;
    }
    let Some((window, gpu, mut state, image, whole)) = v10_fifo_fixture() else {
        return;
    };
    match std::env::var("ROSEMARY_METAL_PRESENTATION_MODE").as_deref() {
        Ok("immediate") => assert_eq!(state.present_mode, vk::PresentModeKHR::IMMEDIATE),
        Ok("mailbox") => assert_eq!(state.present_mode, vk::PresentModeKHR::MAILBOX),
        _ => {}
    }
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, image.image, [0.0, 0.0, 1.0, 1.0]);
    })
    .expect("blue guest drawable");
    let clip = appkit::view::Frame {
        x: 20,
        y: 15,
        width: 45,
        height: 35,
        ..whole
    };
    state.layers.push(1);
    state.synchronize_scene(
        window as usize,
        &[ScenePlacement {
            opacity_groups: Vec::new(),
            id: 1,
            frame: DirectFrame::from_frame(&whole),
            opacity: 1.0,
            opaque: true,
            clip: Some(DirectFrame::from_frame(&clip)),
            rounded: Vec::new(),
            occluded: None,
            native: None,
        }],
    );
    assert_eq!(
        state.stage_composite_layer(1, &whole, &image, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.idle();
    assert_eq!(
        state.scene_layers.len(),
        1,
        "single clipped layer bypassed the scene compositor"
    );
    let composite = state
        .composite
        .as_ref()
        .expect("clipped window image")
        .image;
    let buffer = gpu
        .create_buffer(u64::from(whole.window_width) * u64::from(whole.window_height) * 4)
        .expect("GPU readback");
    gpu.one_shot(|device, cb| unsafe {
        let sync = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(composite)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        device.cmd_pipeline_barrier(
            cb,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[sync],
        );
        device.cmd_copy_image_to_buffer(
            cb,
            composite,
            vk::ImageLayout::GENERAL,
            buffer.buffer,
            &[vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D {
                    width: whole.window_width,
                    height: whole.window_height,
                    depth: 1,
                })],
        );
    })
    .expect("read back clipped window");
    let pixel = |x: usize, y: usize| {
        let at = (y * whole.window_width as usize + x) * 4;
        unsafe { std::slice::from_raw_parts(buffer.contents.add(at), 4) }.to_vec()
    };
    assert_eq!(
        pixel(4, 4),
        [0, 0, 0, 0],
        "out-of-clip pixels were incorrectly composited"
    );
    assert_eq!(
        pixel(30, 25),
        [0, 0, 255, 255],
        "inside-clipped blue layer disappeared"
    );
    gpu.destroy_buffer(buffer);
    drop(state);
    gpu.destroy_image(image);
    sdl3::video::destroy_window(window);
}

/// A background can submit another drawable after an opaque foreground
/// overlay stops submitting. The *pixels*, not just layer-count metadata,
/// must still contain the static foreground after the background update.
#[test]
#[cfg(not(target_os = "macos"))]
fn static_foreground_survives_repeated_background_submissions() {
    if isolate_vulkan_graphical_test("static_foreground_survives_repeated_background_submissions") {
        return;
    }
    let Some((window, gpu, mut state, background, whole)) = v10_fifo_fixture() else {
        return;
    };
    let overlay = test_render_target(gpu, 60, 40).expect("overlay image");
    let paint = |image: &GpuImage, rgba: [f32; 4]| {
        gpu.one_shot(|device, command_buffer| unsafe {
            cmd_test_clear_color(device, command_buffer, image.image, rgba);
        })
        .expect("fill test image")
    };
    paint(&background, [1.0, 0.0, 0.0, 1.0]);
    paint(&overlay, [0.0, 1.0, 0.0, 1.0]);
    let foreground = appkit::view::Frame {
        x: 30,
        y: 20,
        width: 60,
        height: 40,
        ..whole
    };
    state.layers.push(1);
    state.note_scene_frame(1, &whole);
    assert_eq!(
        state.stage_composite_layer(1, &whole, &background, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.layers.push(2);
    assert_eq!(
        state.stage_composite_layer(
            2,
            &foreground,
            &overlay,
            None,
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.note_scene_frame(2, &foreground);
    // B has now stopped updating. Replacing A's drawable cannot erase it.
    paint(&background, [0.0, 0.0, 1.0, 1.0]);
    for _ in 0..4 {
        assert_eq!(
            state.stage_composite_layer(
                1,
                &whole,
                &background,
                None,
                &mut SourceOwner(None),
                false
            ),
            Ok(Some(true))
        );
    }
    state.idle();
    let read = gpu
        .create_buffer(u64::from(whole.window_width) * u64::from(whole.window_height) * 4)
        .expect("readback buffer");
    let composite_image = state.composite.as_ref().expect("window accumulator").image;
    let fetch = || {
        gpu.one_shot(|device, cb| unsafe {
            let read_barrier = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(composite_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            device.cmd_pipeline_barrier(
                cb,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[read_barrier],
            );
            device.cmd_copy_image_to_buffer(
                cb,
                composite_image,
                vk::ImageLayout::GENERAL,
                read.buffer,
                &[vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width: whole.window_width,
                        height: whole.window_height,
                        depth: 1,
                    })],
            );
        })
        .expect("GPU compositor readback")
    };
    fetch();
    let pixel = |x: usize, y: usize| {
        let offset = (y * whole.window_width as usize + x) * 4;
        unsafe { std::slice::from_raw_parts(read.contents.add(offset), 4).to_vec() }
    };
    assert_eq!(
        pixel(55, 35),
        [0, 255, 0, 255],
        "static green overlay was overwritten"
    );
    assert_eq!(
        pixel(5, 5),
        [0, 0, 255, 255],
        "new blue background is missing"
    );
    // Parent/view opacity changes are part of the retained scene state, not
    // an instruction for the static foreground to submit another drawable.
    // A 50% green layer over solid blue is ~[0,128,128,255] in RGBA.
    assert!(state.synchronize_scene(
        window as usize,
        &[
            ScenePlacement {
                opacity_groups: Vec::new(),
                id: 1,
                frame: DirectFrame::from_frame(&whole),
                opacity: 1.0,
                opaque: true,
                clip: None,
                rounded: Vec::new(),
                occluded: None,
                native: None,
            },
            ScenePlacement {
                opacity_groups: Vec::new(),
                id: 2,
                frame: DirectFrame::from_frame(&foreground),
                opacity: 0.5,
                opaque: true,
                clip: None,
                rounded: Vec::new(),
                occluded: None,
                native: None,
            },
        ]
    ));
    let before_repaint = state.frame;
    state.injected_post_scene_record_error = Some(vk::Result::ERROR_OUT_OF_HOST_MEMORY);
    let error = state
        .redisplay_scene(window as usize)
        .expect_err("first scene-only repaint must fail after recording");
    assert!(error.contains("injected post-scene recording failure"));
    assert_eq!(state.frame, before_repaint);
    assert!(
        state.scene_repaint_needed,
        "failed repaint must retain damage"
    );
    assert!(
        state
            .redisplay_scene(window as usize)
            .expect("scene-only repaint must recover without another drawable")
    );
    assert_eq!(state.frame, before_repaint + 1);
    state.scene_repaint_needed = false;
    state.idle();
    fetch();
    let translucent = pixel(55, 35);
    assert_eq!(
        translucent[0], 0,
        "red leaked into green-over-blue composition"
    );
    assert!(
        (127..=129).contains(&translucent[1]),
        "opacity did not halve the static green foreground: {translucent:?}"
    );
    assert!(
        (126..=129).contains(&translucent[2]),
        "blue was erased by the half-transparent green foreground: {translucent:?}"
    );
    assert_eq!(
        translucent[3], 255,
        "premultiplied source-over must end opaque"
    );
    // A clipping ancestor changes visibility without a new B drawable.
    // Recompose just from retained GPU snapshots using B's new clip.
    let restricted = DirectFrame::from_frame(&appkit::view::Frame {
        x: foreground.x + 14,
        y: foreground.y + 10,
        width: 30,
        height: 20,
        ..foreground
    });
    assert!(state.synchronize_scene(
        window as usize,
        &[
            ScenePlacement {
                opacity_groups: Vec::new(),
                id: 1,
                frame: DirectFrame::from_frame(&whole),
                opacity: 1.0,
                opaque: true,
                clip: None,
                rounded: Vec::new(),
                occluded: None,
                native: None,
            },
            ScenePlacement {
                opacity_groups: Vec::new(),
                id: 2,
                frame: DirectFrame::from_frame(&foreground),
                opacity: 0.5,
                opaque: true,
                clip: Some(restricted),
                rounded: Vec::new(),
                occluded: None,
                native: None,
            },
        ]
    ));
    assert!(
        state
            .redisplay_scene(window as usize)
            .expect("clip changed without new drawable")
    );
    state.idle();
    fetch();
    assert_eq!(
        pixel(35, 35),
        [0, 0, 255, 255],
        "pixels outside ancestor bounds leaked from static foreground"
    );
    let inside = pixel(55, 35);
    assert_eq!(inside[0], 0);
    assert!((126..=129).contains(&inside[1]));
    assert!((126..=129).contains(&inside[2]));
    assert_eq!(inside[3], 255);
    gpu.destroy_buffer(read);
    drop(state);
    gpu.destroy_image(background);
    gpu.destroy_image(overlay);
    sdl3::video::destroy_window(window);
}

/// A Vulkan recording/queue failure must never replace the last composited
/// layer with an unwritten GPU image, even when a new scene generation was
/// allocated and the old one is still retained by another GPU frame.
#[test]
#[cfg(not(target_os = "macos"))]
fn failed_scene_submission_restores_last_complete_layer_snapshot() {
    if isolate_vulkan_graphical_test(
        "failed_scene_submission_restores_last_complete_layer_snapshot",
    ) {
        return;
    }
    let Some((window, gpu, mut state, background, whole)) = v10_fifo_fixture() else {
        return;
    };
    let overlay = test_render_target(gpu, 60, 40).expect("overlay source");
    let child = appkit::view::Frame {
        x: 30,
        y: 10,
        width: 60,
        height: 40,
        ..whole
    };
    state.layers.push(1);
    state.note_scene_frame(1, &whole);
    assert_eq!(
        state.stage_composite_layer(1, &whole, &background, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.layers.push(2);
    assert_eq!(
        state.stage_composite_layer(2, &child, &overlay, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.note_scene_frame(2, &child);
    let previous = Arc::clone(
        &state
            .scene_layers
            .iter()
            .find(|scene| scene.id == 2)
            .unwrap()
            .snapshot,
    );
    assert!(
        state
            .scene_layers
            .iter()
            .find(|scene| scene.id == 2)
            .unwrap()
            .ready
    );
    let frame_before = state.frame;
    state.injected_post_scene_record_error = Some(vk::Result::ERROR_OUT_OF_HOST_MEMORY);
    let failure =
        state.stage_composite_layer(2, &child, &overlay, None, &mut SourceOwner(None), false);
    assert!(
        failure
            .unwrap_err()
            .contains("injected post-scene recording failure")
    );
    assert_eq!(state.frame, frame_before);
    let retained = state
        .scene_layers
        .iter()
        .find(|scene| scene.id == 2)
        .unwrap();
    assert!(
        retained.ready,
        "failed submit left the previous layer marked unreadable"
    );
    assert!(
        Arc::ptr_eq(&previous, &retained.snapshot),
        "failed submit replaced the last visible image"
    );
    assert_eq!(state.layers, [1, 2]);
    assert_eq!(
        state.stage_composite_layer(2, &child, &overlay, None, &mut SourceOwner(None), false),
        Ok(Some(true)),
        "retry should stage a complete new scene"
    );
    state.idle();
    drop(state);
    drop(previous);
    gpu.destroy_image(background);
    gpu.destroy_image(overlay);
    sdl3::video::destroy_window(window);
}

/// Every image the compositor has submitted remains immutable until the GPU
/// fence has signaled. Once the per-layer source pool fills with retained
/// generations, a new frame must defer without overwriting an older image.
#[test]
#[cfg(not(target_os = "macos"))]
fn retained_scene_sources_rotate_and_defer_when_all_generations_are_busy() {
    if isolate_vulkan_graphical_test(
        "retained_scene_sources_rotate_and_defer_when_all_generations_are_busy",
    ) {
        return;
    }
    let Some((window, gpu, mut state, background, whole)) = v10_fifo_fixture() else {
        return;
    };
    let overlay = test_render_target(gpu, 60, 40).expect("scene overlay");
    let child = appkit::view::Frame {
        x: 15,
        y: 20,
        width: 60,
        height: 40,
        ..whole
    };
    state.layers.push(1);
    state.note_scene_frame(1, &whole);
    assert_eq!(
        state.stage_composite_layer(1, &whole, &background, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.layers.push(2);
    assert_eq!(
        state.stage_composite_layer(2, &child, &overlay, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.note_scene_frame(2, &child);
    // The first background update establishes a source matching the guest
    // BGRA image; its initial fallback snapshot was in RGBA compositor format.
    assert_eq!(
        state.stage_composite_layer(1, &whole, &background, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    let mut retained = Vec::new();
    for index in 0..super::scene::MAX_SCENE_IMAGES_PER_LAYER {
        let source = state
            .scene_layers
            .iter()
            .find(|scene| scene.id == 1)
            .expect("background in retained scene");
        retained.push(Arc::clone(&source.snapshot));
        let before = state.frame;
        let next = state
            .stage_composite_layer(1, &whole, &background, None, &mut SourceOwner(None), false)
            .expect("scene source rotation");
        if index + 1 == super::scene::MAX_SCENE_IMAGES_PER_LAYER {
            assert_eq!(
                next, None,
                "pool must defer rather than overwrite an in-flight scene source"
            );
            assert_eq!(
                state.frame, before,
                "deferred source caused a GPU staging submission"
            );
        } else {
            assert_eq!(
                next,
                Some(true),
                "unexpected exhaustion at generation {index}"
            );
            assert!(
                !Arc::ptr_eq(
                    retained.last().unwrap(),
                    &state
                        .scene_layers
                        .iter()
                        .find(|scene| scene.id == 1)
                        .unwrap()
                        .snapshot
                ),
                "updated the same source allocation while its previous generation was retained"
            );
        }
    }
    state.idle();
    drop(retained);
    assert_eq!(
        state.stage_composite_layer(1, &whole, &background, None, &mut SourceOwner(None), false),
        Ok(Some(true)),
        "a retired scene source must be reusable without another allocation"
    );
    state.idle();
    drop(state);
    gpu.destroy_image(background);
    gpu.destroy_image(overlay);
    sdl3::video::destroy_window(window);
}

/// A real WSI direct frame becomes a multi-layer scene, then regains direct
/// WSI after the foreground view leaves the live AppKit hierarchy. Repaint
/// only from GPU-retained layer contents: no new guest drawable is submitted.
#[test]
#[cfg(not(target_os = "macos"))]
fn removed_foreground_recomposes_and_restores_direct_wsi() {
    if isolate_vulkan_graphical_test("removed_foreground_recomposes_and_restores_direct_wsi") {
        return;
    }
    let expected = match std::env::var("ROSEMARY_METAL_PRESENTATION_MODE").as_deref() {
        Ok("immediate") => vk::PresentModeKHR::IMMEDIATE,
        Ok("mailbox") => vk::PresentModeKHR::MAILBOX,
        _ => return,
    };
    let Some((window, gpu, mut state, background, whole)) = v10_fifo_fixture() else {
        return;
    };
    assert_eq!(
        state.present_mode, expected,
        "test did not select actual requested WSI mode"
    );
    let overlay = test_render_target(gpu, 60, 40).expect("foreground image");
    let fill = |image: &GpuImage, color| {
        gpu.one_shot(|device, cb| unsafe {
            cmd_test_clear_color(device, cb, image.image, color);
        })
        .expect("fill scene source")
    };
    fill(&background, [0.0, 0.0, 1.0, 1.0]);
    fill(&overlay, [0.0, 1.0, 0.0, 1.0]);
    state.layers.push(1);
    state.note_scene_frame(1, &whole);
    state.queue_direct(
        1,
        &whole,
        PresentSource::from(&background),
        &mut SourceOwner(None),
        None,
    );
    state
        .flush_composite(window)
        .expect("single-layer direct WSI");
    assert!(state.direct.is_some());
    assert!(state.composite.is_none());
    let foreground = appkit::view::Frame {
        x: 30,
        y: 20,
        width: 60,
        height: 40,
        ..whole
    };
    state.layers.push(2);
    assert_eq!(state.stage_direct_snapshot(false), Ok(Some(true)));
    assert_eq!(
        state.stage_composite_layer(
            2,
            &foreground,
            &overlay,
            None,
            &mut SourceOwner(None),
            false
        ),
        Ok(Some(true))
    );
    state.note_scene_frame(2, &foreground);
    state
        .flush_composite(window)
        .expect("two-layer accumulator WSI");
    assert!(state.direct.is_none());
    assert_eq!(state.scene_layers.len(), 2);
    assert!(state.synchronize_scene(
        window as usize,
        &[ScenePlacement {
            opacity_groups: Vec::new(),
            id: 1,
            frame: DirectFrame::from_frame(&whole),
            opacity: 1.0,
            opaque: true,
            clip: None,
            rounded: Vec::new(),
            occluded: None,
            native: None,
        }]
    ));
    assert_eq!(state.layers, [1]);
    let gpu_frame = state.frame;
    assert!(
        state
            .redisplay_scene(window as usize)
            .expect("retained scene repaint")
    );
    assert_eq!(
        state.frame, gpu_frame,
        "restored direct WSI paid an extra accumulator submission"
    );
    let direct = state
        .direct
        .as_ref()
        .expect("single-layer fast path not restored");
    assert_eq!(direct.layer, 1);
    assert!(
        direct.backing.is_some(),
        "new direct frame has no GPU source lease"
    );
    assert!(
        state.scene_layers.iter().all(|scene| scene.id != 2),
        "removed overlay source remains in live scene"
    );
    // Check source pixels, not just membership and WSI path selection: the
    // old overlay's green pixels must be replaced by the blue background
    // without requiring any new guest command buffer or drawable.
    let retained = &direct.backing.as_ref().expect("retained background").image;
    let source_image = retained.image;
    let expected_blue = match retained.format {
        vk::Format::B8G8R8A8_UNORM => [255, 0, 0, 255],
        vk::Format::R8G8B8A8_UNORM => [0, 0, 255, 255],
        other => panic!("unexpected retained scene source format: {other:?}"),
    };
    let read = gpu
        .create_buffer(u64::from(whole.window_width) * u64::from(whole.window_height) * 4)
        .expect("source pixel readback");
    gpu.one_shot(|device, cb| unsafe {
        let sync = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(source_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        device.cmd_pipeline_barrier(
            cb,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[sync],
        );
        device.cmd_copy_image_to_buffer(
            cb,
            source_image,
            vk::ImageLayout::GENERAL,
            read.buffer,
            &[vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D {
                    width: whole.window_width,
                    height: whole.window_height,
                    depth: 1,
                })],
        );
    })
    .expect("retained direct-image readback");
    let at_overlay = ((35u64 * u64::from(whole.window_width) + 55) * 4) as usize;
    let pixel = unsafe { std::slice::from_raw_parts(read.contents.add(at_overlay), 4) };
    assert_eq!(
        pixel, expected_blue,
        "a removed foreground left obsolete pixels in restored WSI direct image"
    );
    gpu.destroy_buffer(read);
    state
        .flush_composite(window)
        .expect("direct WSI after foreground removal");
    state.idle();
    drop(state);
    gpu.destroy_image(background);
    gpu.destroy_image(overlay);
    sdl3::video::destroy_window(window);
}

/// Two layers in one window, the second of them a rectangle inside it,
/// which is the path that needs the composite image.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "Cocoa needs the main thread; use rosemary's Metal guest tests"
)]
fn two_layers_in_a_window_are_composited_into_one_image() {
    if isolate_vulkan_graphical_test("two_layers_in_a_window_are_composited_into_one_image") {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|adapter| adapter.gpu().ok()) else {
        return;
    };
    let flags = sdl3::video::flags::HIGH_PIXEL_DENSITY;
    let Some(window) = sdl3::video::create_window(c"rosemary compositing", 320, 240, flags) else {
        return;
    };
    sdl3::video::show_window(window);
    let (width, height) =
        sdl3::video::window_size_in_pixels(window).expect("a window SDL just made");
    let (width, height) = (width as u32, height as u32);

    let image = |w: u32, h: u32| test_render_target(gpu, w, h).expect("a colour image");
    let background = image(width, height);
    let overlay = image(width / 2, height / 2);

    let whole = appkit::view::Frame {
        window,
        x: 0,
        y: 0,
        width,
        height,
        window_width: width,
        window_height: height,
    };
    // Deliberately overhanging the bottom right corner, so the clipping
    // in `clamp` is what keeps the blit inside the image.
    let corner = appkit::view::Frame {
        x: (width / 2) as i32,
        y: (height / 2) as i32,
        width,
        height,
        ..whole
    };
    assert!(!corner.covers_window());

    // The first layer alone takes the direct path; the second brings the
    // composite into existence, and both are presented after that.
    assert_eq!(present(gpu, 1, &whole, &background, &[]), Ok(true));
    assert_eq!(present(gpu, 2, &corner, &overlay, &[]), Ok(true));
    assert_eq!(present(gpu, 1, &whole, &background, &[]), Ok(true));
    assert_eq!(present(gpu, 2, &corner, &overlay, &[]), Ok(true));
    flush_if_composed(window);

    let composited = SWAPCHAINS
        .lock()
        .expect("window swapchains")
        .as_ref()
        .and_then(|all| all.get(&(window as usize)))
        .is_some_and(|window| {
            window
                .state
                .lock()
                .expect("window swapchain")
                .composite
                .is_some()
        });
    assert!(
        composited,
        "a window with two layers presented without a composite image"
    );

    SWAPCHAINS
        .lock()
        .expect("window swapchains")
        .as_mut()
        .and_then(|all| all.remove(&(window as usize)));
    gpu.destroy_image(background);
    gpu.destroy_image(overlay);
    sdl3::video::destroy_window(window);
}

/// A window made with room for rounded corners is presented through the
/// corners pass, on a surface that shows its alpha, whatever size it is.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "Cocoa needs the main thread; use rosemary's Metal guest tests"
)]
fn a_transparent_window_is_presented_through_the_corners_pass() {
    if isolate_vulkan_graphical_test("a_transparent_window_is_presented_through_the_corners_pass") {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|adapter| adapter.gpu().ok()) else {
        return;
    };
    let flags = sdl3::video::flags::HIGH_PIXEL_DENSITY | sdl3::video::flags::TRANSPARENT;
    let Some(window) = sdl3::video::create_window(c"rosemary corners", 320, 240, flags) else {
        return;
    };
    sdl3::video::show_window(window);
    let frame_of = |window| {
        let (width, height) =
            sdl3::video::window_size_in_pixels(window).expect("a window SDL made");
        test_full_frame(window, width as u32, height as u32)
    };
    let frame = frame_of(window);
    let source = test_render_target(gpu, frame.width, frame.height).expect("a colour image");
    // Enough frames to reuse every frame slot's descriptor set.
    for _ in 0..8 {
        assert_eq!(present(gpu, 1, &frame, &source, &[]), Ok(true));
    }
    flush_if_composed(window);
    let sealed = |check: fn(&Swapchain) -> bool| {
        SWAPCHAINS
            .lock()
            .expect("window swapchains")
            .as_ref()
            .and_then(|all| all.get(&(window as usize)))
            .is_some_and(|window| check(&window.state.lock().expect("window swapchain")))
    };
    // A surface that cannot show premultiplied alpha is presented opaque
    // instead, which is not this test's subject.
    if sealed(|swapchain| swapchain.corners.is_some()) {
        assert!(
            sealed(|swapchain| swapchain.composite.is_some()),
            "the corners pass ran without a composite to read"
        );
        sdl3::video::set_window_size(window, 400, 300);
        let resized = frame_of(window);
        for _ in 0..4 {
            assert_eq!(present(gpu, 1, &resized, &source, &[]), Ok(true));
        }
        flush_if_composed(window);
        assert!(sealed(|swapchain| swapchain.corners.is_some()));
    }

    SWAPCHAINS
        .lock()
        .expect("window swapchains")
        .as_mut()
        .and_then(|all| all.remove(&(window as usize)));
    gpu.destroy_image(source);
    sdl3::video::destroy_window(window);
}

/// An OpenGL window is never converted into a Vulkan one.
///
/// SDL will do it, and on Wayland the result kills the process: the
/// `EGLSurface` SDL made with the window stays alive on the
/// `wl_surface`, the Vulkan driver claims the same surface, and the
/// compositer answers `wp_linux_drm_syncobj_manager_v1: surface already
/// exists` and closes the connection. Presentation must refuse such a
/// window and fall back to the readback rather than convert it.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "Cocoa needs the main thread; use rosemary's Metal guest tests"
)]
fn an_opengl_window_is_refused_rather_than_converted() {
    if isolate_vulkan_graphical_test("an_opengl_window_is_refused_rather_than_converted") {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|adapter| adapter.gpu().ok()) else {
        return;
    };
    let flags = sdl3::video::flags::OPENGL | sdl3::video::flags::HIGH_PIXEL_DENSITY;
    let Some(window) = sdl3::video::create_window(c"rosemary opengl", 320, 240, flags) else {
        return;
    };
    sdl3::video::show_window(window);
    let (w, h) = sdl3::video::window_size_in_pixels(window).expect("a window");
    let frame = test_full_frame(window, w as u32, h as u32);
    let source = test_render_target(gpu, frame.width, frame.height).expect("a colour image");

    assert_eq!(
        present(gpu, 1, &frame, &source, &[]),
        Ok(false),
        "an OpenGL window was given a Vulkan surface"
    );
    // Still an OpenGL window afterwards: nothing converted it.
    assert_eq!(
        sdl3::video::window_flags(window) & sdl3::video::flags::VULKAN,
        0,
        "the window was converted to a Vulkan window anyway"
    );
    assert_ne!(
        sdl3::video::window_flags(window) & sdl3::video::flags::OPENGL,
        0,
        "the window lost its OpenGL flag"
    );
    // And it is remembered, so a run does not retry it every frame.
    assert_eq!(present(gpu, 1, &frame, &source, &[]), Ok(false));

    REFUSED
        .lock()
        .expect("refused windows")
        .as_mut()
        .and_then(|refused| refused.remove(&(window as usize)));
    gpu.destroy_image(source);
    sdl3::video::destroy_window(window);
}

/// The whole path a guest actually takes: an `NSWindow` made through
/// `-initWithContentRect:`, a view in it, a `CAMetalLayer` on the view,
/// and a drawable presented through the layer.
///
/// The tests above build the SDL window themselves, so they cannot see a
/// mistake in the flags `-initWithContentRect:` chooses - and that is
/// exactly what once left every real window unable to have a swapchain.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "Cocoa needs the main thread; use rosemary's Metal guest tests"
)]
fn a_layer_on_a_guests_own_window_is_presented_through_a_swapchain() {
    if isolate_vulkan_graphical_test(
        "a_layer_on_a_guests_own_window_is_presented_through_a_swapchain",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|adapter| adapter.gpu().ok()) else {
        return;
    };
    crate::bootstrap();

    let rect = foundation::geometry::NSRect {
        origin: foundation::geometry::NSPoint { x: 0.0, y: 0.0 },
        size: foundation::geometry::NSSize {
            width: 320.0,
            height: 240.0,
        },
    };
    let class = objc::class::lookup(c"NSWindow").expect("NSWindow");
    let window = objc::object::create_instance(class, appkit::window::INSTANCE_SIZE);
    // SAFETY: `-initWithContentRect:styleMask:backing:defer:` is
    // `id (id, SEL, NSRect, NSUInteger, NSUInteger, BOOL)` on a fresh
    // window, which is how a guest makes one.
    unsafe {
        let send: unsafe extern "C" fn(
            objc::object::Id,
            objc::sel::Sel,
            foundation::geometry::NSRect,
            usize,
            usize,
            u8,
        ) = std::mem::transmute(objc::msg::entry());
        send(
            window,
            objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
            rect,
            0, // Borderless: a titled X11 window has host titlebar insets.
            0,
            0,
        );
    }
    // SAFETY: a window this test just made.
    let sdl = unsafe { appkit::window::sdl_window(window) };
    if sdl.is_null() {
        return; // no display on this host
    }
    assert_eq!(
        sdl3::video::window_flags(sdl) & sdl3::video::flags::OPENGL,
        0,
        "a guest's window claimed OpenGL, so it can never have a swapchain"
    );

    let view_class = objc::class::lookup(c"NSView").expect("NSView");
    let view = objc::object::create_instance(view_class, 0);
    // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)`.
    unsafe {
        let send: unsafe extern "C" fn(
            objc::object::Id,
            objc::sel::Sel,
            foundation::geometry::NSRect,
        ) -> objc::object::Id = std::mem::transmute(objc::msg::entry());
        send(view, objc::sel::get("initWithFrame:"), rect);
    }
    // SAFETY: `-setContentView:` is `void (id, SEL, id)`.
    unsafe {
        let send: unsafe extern "C" fn(objc::object::Id, objc::sel::Sel, objc::object::Id) =
            std::mem::transmute(objc::msg::entry());
        send(window, objc::sel::get("setContentView:"), view);
    }

    let layer_class = objc::class::lookup(c"CAMetalLayer").expect("CAMetalLayer");
    // SAFETY: `+layer` is `id (Class, SEL)`.
    let layer = unsafe {
        let send: unsafe extern "C" fn(objc::object::Id, objc::sel::Sel) -> objc::object::Id =
            std::mem::transmute(objc::msg::entry());
        send(
            layer_class as *const objc::class::Class as objc::object::Id,
            objc::sel::get("layer"),
        )
    };
    assert!(!layer.is_null());
    // SAFETY: `-setLayer:` is `void (id, SEL, id)`, and this is what
    // records the layer's view so presentation can find its window.
    unsafe {
        let send: unsafe extern "C" fn(objc::object::Id, objc::sel::Sel, objc::object::Id) =
            std::mem::transmute(objc::msg::entry());
        send(view, objc::sel::get("setLayer:"), layer);
    }

    let frame = crate::present::layer_frame(layer)
        .expect("the layer's view is in a window this runtime made");
    assert_eq!(frame.window, sdl);
    assert!(
        frame.covers_window(),
        "a content view the size of its window did not cover it"
    );

    let source =
        test_render_target(gpu, frame.width.max(1), frame.height.max(1)).expect("a colour image");
    assert_eq!(
        present(gpu, layer as usize, &frame, &source, &[]),
        Ok(true),
        "a guest's own window fell back to the readback"
    );

    // Regression: an SDL resize/event invalidates the worker-readable
    // snapshot after this drawable was submitted but before its dispatch.
    // The drawable must remain in the Submitted ring until a real Vulkan
    // presentation occurs; CPU readback is forbidden on the converted SDL window.
    let drawable = unsafe { crate::present::next_drawable(layer, objc::sel::get("nextDrawable")) };
    assert!(!drawable.is_null(), "an actual Metal drawable");
    assert!(crate::present::submit(drawable));
    let baseline = {
        let all = SWAPCHAINS.lock().unwrap();
        let entry = all.as_ref().unwrap().get(&(sdl as usize)).unwrap();
        entry.state.lock().unwrap().frame
    };
    let identity = sdl3::video::window_id(sdl);
    sdl3::video::invalidate_presentation_window(identity, &[sdl]);
    // Only the non-SDL owner thread sees the transient cache hole. Calling
    // from the owner would immediately resample SDL and mask the race.
    let address = layer as usize;
    assert!(
        std::thread::spawn(move || {
            crate::present::layer_frame(address as objc::object::Id).is_none()
                && crate::present::layer_waits_for_window_metadata(address as objc::object::Id)
        })
        .join()
        .unwrap()
    );
    let submitted = drawable as usize;
    assert_eq!(
        std::thread::spawn(move || {
            crate::present::enqueue_swapchain(submitted as objc::object::Id)
        })
        .join()
        .unwrap(),
        crate::present::PresentAttempt::DeferredBusy,
        "metadata invalidation must not select CPU readback"
    );
    assert!(
        crate::present::drawable_is_submitted_for_test(drawable),
        "deferred frame was silently completed"
    );
    sdl3::video::publish_presentation_windows(&[sdl]);
    assert!(crate::present::layer_frame(layer).is_some());
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let submitted = drawable as usize;
        match std::thread::spawn(move || {
            crate::present::enqueue_swapchain(submitted as objc::object::Id)
        })
        .join()
        .unwrap()
        {
            crate::present::PresentAttempt::Handled => break,
            crate::present::PresentAttempt::Deferred
            | crate::present::PresentAttempt::DeferredBusy => {
                assert!(
                    Instant::now() < deadline,
                    "deferred drawable never presented"
                );
                sdl3::video::vulkan::service_surface_requests();
                std::thread::sleep(Duration::from_millis(2));
            }
            crate::present::PresentAttempt::Readback => {
                panic!("the converted window incorrectly selected CPU fallback")
            }
        }
    }
    flush_if_composed(sdl);
    let after = {
        let all = SWAPCHAINS.lock().unwrap();
        let entry = all.as_ref().unwrap().get(&(sdl as usize)).unwrap();
        entry.state.lock().unwrap().frame
    };
    assert!(
        after > baseline,
        "the deferred drawable never reached Vulkan"
    );
    // This harness omits the asynchronous production compositor worker.
    // Its direct single-layer source remains retained after WSI copy until
    // the worker's idle snapshot releases the last CAMetalDrawable. Exercise
    // that exact completion contract synchronously rather than timing out.
    {
        let entry = SWAPCHAINS
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .get(&(sdl as usize))
            .unwrap()
            .clone();
        let _presenting = entry.presenting.lock().unwrap();
        let mut state = entry.state.lock().unwrap();
        if state.direct.is_some() {
            assert!(
                crate::present::drawable_is_submitted_for_test(drawable),
                "WSI copy must not finish a drawable before idle snapshot"
            );
            state.direct.as_mut().unwrap().queued_at = Instant::now() - DIRECT_IDLE_SNAPSHOT_DELAY;
            assert!(
                state
                    .snapshot_idle_direct()
                    .expect("idle snapshot after direct WSI")
            );
        }
    }
    // Other backing-store recomposites can create more swapchain frames.
    // This drawable itself was handled once; wait for its real copy fence.
    let deadline = Instant::now() + Duration::from_secs(3);
    while crate::present::drawable_is_submitted_for_test(drawable) {
        crate::swapchain::retire_copied();
        assert!(
            Instant::now() < deadline,
            "the submitted drawable never completed"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    // A1 loses its cached geometry while the presenter is scanning a batch.
    // A2 belongs to the same window but SDL republishes its metadata between
    // those two dispatch decisions. Neither A2 nor a later batch may overtake
    // the older submitted drawable.
    let first = unsafe { crate::present::next_drawable(layer, objc::sel::get("nextDrawable")) };
    let second = unsafe { crate::present::next_drawable(layer, objc::sel::get("nextDrawable")) };
    assert!(!first.is_null() && !second.is_null() && first != second);
    assert!(crate::present::submit(first));
    assert!(crate::present::submit(second));
    let first_addr = first as usize;
    let second_addr = second as usize;
    sdl3::video::invalidate_presentation_window(identity, &[sdl]);
    let (reached, at_first) = std::sync::mpsc::channel();
    let (resume, after_publish) = std::sync::mpsc::channel();
    let batch = std::thread::spawn(move || {
        let mut attempted = Vec::new();
        let deferred = crate::present::dispatch_batch(
            std::collections::VecDeque::from([first_addr, second_addr]),
            |drawable| crate::present::drawable_window(drawable as objc::object::Id),
            |drawable| {
                attempted.push(drawable);
                let outcome = crate::present::enqueue_swapchain(drawable as objc::object::Id);
                if drawable == first_addr {
                    reached.send(outcome).expect("notify video thread");
                    after_publish.recv().expect("metadata publication");
                }
                outcome
            },
            |_drawable, _outcome| {},
        );
        (deferred, attempted)
    });
    assert_eq!(
        at_first
            .recv_timeout(Duration::from_secs(4))
            .expect("first drawable was examined"),
        crate::present::PresentAttempt::DeferredBusy,
    );
    sdl3::video::publish_presentation_windows(&[sdl]);
    assert!(crate::present::layer_frame(layer).is_some());
    resume.send(()).expect("resume the same batch");
    let (mut deferred, attempted) = batch.join().expect("presenter batch");
    assert_eq!(
        attempted,
        [first_addr],
        "newer A2 overtook metadata-starved A1"
    );
    assert_eq!(
        deferred,
        std::collections::VecDeque::from([first_addr, second_addr]),
        "the original order must survive the cache-publication transition"
    );
    assert!(
        crate::present::drawable_is_submitted_for_test(first)
            && crate::present::drawable_is_submitted_for_test(second),
        "the dispatcher consumed a queued drawable during invalidation"
    );
    // Replay through the same real batch dispatcher, and check the order
    // in which the two distinct Metal drawables actually reach the swapchain.
    let mut handled = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !deferred.is_empty() {
        let (retry, newly_handled) = std::thread::spawn(move || {
            let mut completed = Vec::new();
            let retry = crate::present::dispatch_batch(
                deferred,
                |drawable| crate::present::drawable_window(drawable as objc::object::Id),
                |drawable| crate::present::enqueue_swapchain(drawable as objc::object::Id),
                |drawable, result| {
                    assert_eq!(result, crate::present::PresentAttempt::Handled);
                    completed.push(drawable);
                },
            );
            (retry, completed)
        })
        .join()
        .expect("presenter retry");
        deferred = retry;
        handled.extend(newly_handled);
        if !deferred.is_empty() {
            assert!(Instant::now() < deadline, "queued pair never presented");
            sdl3::video::vulkan::service_surface_requests();
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    assert_eq!(
        handled,
        [first_addr, second_addr],
        "the frames must not reorder"
    );
    flush_if_composed(sdl);
    {
        let entry = SWAPCHAINS
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .get(&(sdl as usize))
            .unwrap()
            .clone();
        let _presenting = entry.presenting.lock().unwrap();
        let mut state = entry.state.lock().unwrap();
        if state.direct.is_some() {
            assert!(
                crate::present::drawable_is_submitted_for_test(second),
                "the last direct drawable remains submitted until GPU idle snapshot"
            );
            state.direct.as_mut().unwrap().queued_at = Instant::now() - DIRECT_IDLE_SNAPSHOT_DELAY;
            assert!(
                state
                    .snapshot_idle_direct()
                    .expect("idle snapshot for deferred pair")
            );
        }
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while crate::present::drawable_is_submitted_for_test(first)
        || crate::present::drawable_is_submitted_for_test(second)
    {
        crate::swapchain::retire_copied();
        assert!(Instant::now() < deadline, "deferred pair never retired");
        std::thread::sleep(Duration::from_millis(2));
    }

    SWAPCHAINS
        .lock()
        .expect("window swapchains")
        .as_mut()
        .and_then(|all| all.remove(&(sdl as usize)));
    gpu.destroy_image(source);
}

#[test]
fn a_frame_off_the_edge_of_its_window_is_clipped_to_it() {
    let extent = vk::Extent2D {
        width: 100,
        height: 80,
    };
    let frame = |x, y, width, height| appkit::view::Frame {
        window: std::ptr::null_mut(),
        x,
        y,
        width,
        height,
        window_width: extent.width,
        window_height: extent.height,
    };
    // Wholly inside, which is left alone.
    let [from, to] = clamp(&frame(10, 20, 30, 40), extent).expect("inside the window");
    assert_eq!((from.x, from.y, to.x, to.y), (10, 20, 40, 60));
    // Hanging off the far corner, clipped to the image.
    let [from, to] = clamp(&frame(80, 60, 100, 100), extent).expect("partly inside");
    assert_eq!((from.x, from.y, to.x, to.y), (80, 60, 100, 80));
    // Off the near corner, which a negative origin reaches.
    let [from, to] = clamp(&frame(-20, -30, 50, 50), extent).expect("partly inside");
    assert_eq!((from.x, from.y, to.x, to.y), (0, 0, 30, 20));
    // Entirely outside, which is nothing to blit rather than an empty
    // rectangle Vulkan would reject.
    assert!(clamp(&frame(500, 500, 10, 10), extent).is_none());
    assert!(clamp(&frame(-50, 0, 10, 10), extent).is_none());
    // A frame with no area of its own is the same answer.
    assert!(clamp(&frame(10, 10, 0, 40), extent).is_none());
}

/// A swapchain whose extent lags its window's size in pixels, as one does
/// between a resize and the rebuild, is still filled by a layer covering the
/// window: scaled, as before logical clipping, not cropped or left unwritten.
#[test]
fn a_covering_layer_fills_a_swapchain_extent_that_lags_its_window() {
    let frame = test_full_frame(std::ptr::null_mut(), 320, 240);
    for extent in [(330, 250), (300, 200)] {
        let extent = vk::Extent2D {
            width: extent.0,
            height: extent.1,
        };
        let (from, into) = direct_blit_to(&frame, extent, None, (320, 240)).unwrap();
        let whole = |w: u32, h: u32| {
            [
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D {
                    x: w as i32,
                    y: h as i32,
                    z: 1,
                },
            ]
        };
        assert_eq!(into, whole(extent.width, extent.height), "{extent:?}");
        assert_eq!(from, whole(320, 240), "{extent:?}");
    }
    // A logical clip in window pixels scales with the frame.
    let allowed = sdl3::Rect {
        x: 0,
        y: 24,
        w: 320,
        h: 216,
    };
    let extent = vk::Extent2D {
        width: 640,
        height: 480,
    };
    let (from, into) = direct_blit_to(&frame, extent, Some(allowed), (320, 240)).unwrap();
    assert_eq!((into[0].y, into[1].y), (48, 480));
    assert_eq!((from[0].y, from[1].y), (24, 240));
}

/// Actual WSI presentation with a shortened AppKit NSWindow inside a full-
/// display SDL container. A pure clipping unit cannot validate Vulkan image
/// layout changes needed for the new black-band clear and cropped transfer.
#[test]
#[cfg_attr(target_os = "macos", ignore = "requires a Fedora X11 Vulkan window")]
fn a_shrunken_guest_fullscreen_is_presented_through_the_vulkan_swapchain() {
    if isolate_vulkan_graphical_test(
        "a_shrunken_guest_fullscreen_is_presented_through_the_vulkan_swapchain",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|adapter| adapter.gpu().ok()) else {
        return;
    };
    crate::bootstrap();
    let pool = objc::arc::pool_push();
    let class = objc::class::lookup(c"NSWindow").unwrap();
    let object = objc::object::create_instance(class, appkit::window::INSTANCE_SIZE);
    let mut frame = foundation::geometry::NSRect {
        origin: foundation::geometry::NSPoint { x: 80.0, y: 90.0 },
        size: foundation::geometry::NSSize {
            width: 400.0,
            height: 300.0,
        },
    };
    let init: unsafe extern "C" fn(
        objc::object::Id,
        objc::sel::Sel,
        foundation::geometry::NSRect,
        usize,
        usize,
        u8,
    ) -> objc::object::Id = unsafe { std::mem::transmute(objc::msg::entry()) };
    assert_eq!(
        unsafe {
            init(
                object,
                objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
                frame,
                9,
                0,
                0,
            )
        },
        object
    );
    let sdl = unsafe { appkit::window::sdl_window(object) };
    assert!(!sdl.is_null());
    sdl3::video::show_window(sdl);
    let toggle: unsafe extern "C" fn(objc::object::Id, objc::sel::Sel, objc::object::Id) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe {
        toggle(
            object,
            objc::sel::get("toggleFullScreen:"),
            std::ptr::null_mut(),
        )
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while sdl3::video::window_flags(sdl) & sdl3::video::flags::FULLSCREEN == 0 {
        appkit::event::pump(std::ptr::null_mut());
        assert!(
            std::time::Instant::now() < deadline,
            "fullscreen did not settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    appkit::event::pump(std::ptr::null_mut());
    let get: unsafe extern "C" fn(
        objc::object::Id,
        objc::sel::Sel,
    ) -> foundation::geometry::NSRect = unsafe { std::mem::transmute(objc::msg::entry_stret()) };
    frame = unsafe { get(object, objc::sel::get("frame")) };
    let host_frame = frame;
    frame.origin.y += 31.0;
    frame.size.height -= 31.0;
    let set: unsafe extern "C" fn(objc::object::Id, objc::sel::Sel, foundation::geometry::NSRect) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe { set(object, objc::sel::get("setFrame:"), frame) };
    let bounds = appkit::window::presentation_clip(sdl).unwrap();
    let (w, h) = sdl3::video::window_size_in_pixels(sdl).unwrap();
    assert!(
        bounds.h < h && bounds.h > 0,
        "logical window did not shrink"
    );
    let full = test_full_frame(sdl, w as u32, h as u32);
    let accepted = clamp(
        &full,
        vk::Extent2D {
            width: w as u32,
            height: h as u32,
        },
    )
    .unwrap();
    assert_eq!(accepted[1].y - accepted[0].y, bounds.h);
    let source = test_render_target(gpu, full.width, full.height).unwrap();
    // Give the oversized layer an unmistakable red drawable: pixel-level
    // verification must not pass merely because an uninitialised source image
    // happened to be black.
    gpu.one_shot(|device, commands| unsafe {
        cmd_test_clear_color(device, commands, source.image, [1.0, 0.0, 0.0, 1.0]);
    })
    .expect("red source cleared through Vulkan");
    for _ in 0..5 {
        assert_eq!(present(gpu, 333, &full, &source, &[]), Ok(true));
        // The second layer forces the accumulator/compositor path that GPU
        // DMA-BUF overlays share, rather than only direct layer presentation.
        assert_eq!(present(gpu, 334, &full, &source, &[]), Ok(true));
    }
    assert!(
        SWAPCHAINS
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|all| all.get(&(sdl as usize)))
            .is_some_and(|chain| chain.state.lock().unwrap().composite.is_some())
    );
    // Optional pixel oracle for real X11 Vulkan hosts with ImageMagick.
    // The test otherwise still executes Vulkan's recorded clear/blit/present.
    if std::env::var_os("ROSEMARY_TEST_VULKAN_PIXELS").is_some() {
        std::thread::sleep(std::time::Duration::from_millis(220));
        let image =
            std::env::temp_dir().join(format!("rosemary-vulkan-clip-{}.png", std::process::id()));
        let photographed = std::process::Command::new("import")
            .args(["-window", "root"])
            .arg(&image)
            .status()
            .unwrap();
        assert!(
            photographed.success(),
            "X11 screenshot could not be captured"
        );
        let rgb = std::process::Command::new("magick")
            .arg(&image)
            .args(["-depth", "8", "RGB:-"])
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&image);
        assert!(rgb.status.success());
        assert_eq!(rgb.stdout.len(), w as usize * h as usize * 3);
        let sample = |x: usize, y: usize| {
            let index = (y * w as usize + x) * 3;
            &rgb.stdout[index..index + 3]
        };
        let x = w as usize / 2;
        assert_eq!(
            sample(x, h as usize - 5),
            &[0, 0, 0],
            "pixels beyond logical frame escaped"
        );
        assert_eq!(
            sample(x, h as usize - 60),
            &[255, 0, 0],
            "valid Metal layer pixel not presented"
        );
    }
    // Restoring the full logical NSWindow must reveal the source pixels in
    // the previously excluded region, not leave a permanent black mask.
    unsafe { set(object, objc::sel::get("setFrame:"), host_frame) };
    let restored = appkit::window::presentation_clip(sdl).unwrap();
    assert_eq!(
        (restored.x, restored.y, restored.w, restored.h),
        (0, 0, w, h)
    );
    for _ in 0..3 {
        assert_eq!(present(gpu, 333, &full, &source, &[]), Ok(true));
        assert_eq!(present(gpu, 334, &full, &source, &[]), Ok(true));
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN_PIXELS").is_some() {
        std::thread::sleep(std::time::Duration::from_millis(220));
        let image = std::env::temp_dir().join(format!(
            "rosemary-vulkan-restored-{}.png",
            std::process::id()
        ));
        let screenshot = std::process::Command::new("import")
            .args(["-window", "root"])
            .arg(&image)
            .status()
            .unwrap();
        assert!(screenshot.success());
        let rgb = std::process::Command::new("magick")
            .arg(&image)
            .args(["-depth", "8", "RGB:-"])
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&image);
        assert!(rgb.status.success());
        let offset = ((h as usize - 5) * w as usize + w as usize / 2) * 3;
        assert_eq!(
            &rgb.stdout[offset..offset + 3],
            &[255, 0, 0],
            "restoring the logical frame did not reveal its lower pixels"
        );
    }
    SWAPCHAINS
        .lock()
        .unwrap()
        .as_mut()
        .and_then(|all| all.remove(&(sdl as usize)));
    gpu.destroy_image(source);
    objc::arc::release(object);
    objc::arc::pool_pop(pool);
}

/// A real fractional AppKit view with a CAMetalLayer must use the same
/// half-open pixel edges as the logical window boundary. Validate the
/// destination-to-source crop rather than merely masking the last pixel.
#[test]
#[cfg_attr(target_os = "macos", ignore = "requires a non-Cocoa SDL host")]
fn a_fractional_metal_layer_samples_exactly_its_rounded_destination_edges() {
    if isolate_vulkan_graphical_test(
        "a_fractional_metal_layer_samples_exactly_its_rounded_destination_edges",
    ) {
        return;
    }
    crate::bootstrap();
    let pool = objc::arc::pool_push();
    let class = objc::class::lookup(c"NSWindow").unwrap();
    let window = objc::object::create_instance(class, appkit::window::INSTANCE_SIZE);
    let rect = foundation::geometry::NSRect {
        origin: foundation::geometry::NSPoint { x: 0.0, y: 0.0 },
        size: foundation::geometry::NSSize {
            width: 320.0,
            height: 240.0,
        },
    };
    let init: unsafe extern "C" fn(
        objc::object::Id,
        objc::sel::Sel,
        foundation::geometry::NSRect,
        usize,
        usize,
        u8,
    ) -> objc::object::Id = unsafe { std::mem::transmute(objc::msg::entry()) };
    if unsafe {
        init(
            window,
            objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
            rect,
            0,
            0,
            0,
        )
    } != window
    {
        return;
    }
    // Only the SDL video thread can build the presentation cache.
    if !sdl3::video::is_video_thread() {
        objc::arc::pool_pop(pool);
        return;
    }
    let sdl = unsafe { appkit::window::sdl_window(window) };
    let points = sdl3::video::window_size(sdl).unwrap();
    let physical = sdl3::video::window_size_in_pixels(sdl).unwrap();
    if points != physical {
        // This fixture explicitly tests 1x; 2x is tested in AppKit.
        objc::arc::release(window);
        objc::arc::pool_pop(pool);
        return;
    }
    assert!(points.0 >= 100 && points.1 >= 100);
    let content: unsafe extern "C" fn(objc::object::Id, objc::sel::Sel) -> objc::object::Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let parent = unsafe { content(window, objc::sel::get("contentView")) };
    assert!(!parent.is_null());
    let child_class = objc::class::lookup(c"NSView").unwrap();
    let child = objc::object::create_instance(child_class, 0);
    let w = f64::from(points.0);
    let h = f64::from(points.1);
    let frame = foundation::geometry::NSRect {
        origin: foundation::geometry::NSPoint { x: 0.5, y: 1.0 },
        size: foundation::geometry::NSSize {
            width: w - 1.5,
            height: h - 1.5,
        },
    };
    let set_frame: unsafe extern "C" fn(
        objc::object::Id,
        objc::sel::Sel,
        foundation::geometry::NSRect,
    ) -> objc::object::Id = unsafe { std::mem::transmute(objc::msg::entry()) };
    assert_eq!(
        unsafe { set_frame(child, objc::sel::get("initWithFrame:"), frame) },
        child
    );
    let add: unsafe extern "C" fn(objc::object::Id, objc::sel::Sel, objc::object::Id) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe { add(parent, objc::sel::get("addSubview:"), child) };
    let layer_class = objc::class::lookup(c"CAMetalLayer").unwrap();
    let layer: unsafe extern "C" fn(objc::object::Id, objc::sel::Sel) -> objc::object::Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let drawable = unsafe {
        layer(
            layer_class as *const objc::class::Class as objc::object::Id,
            objc::sel::get("layer"),
        )
    };
    assert!(!drawable.is_null());
    unsafe { add(child, objc::sel::get("setLayer:"), drawable) };
    let mapped = crate::present::layer_frame(drawable).expect("fractional CAMetalLayer placement");
    // The view starts at half a point and ends at (host_width - 1). These
    // independently rounded endpoints form [1,host_width-1), not [1,host_width).
    assert_eq!(
        (mapped.x, mapped.y, mapped.width, mapped.height),
        (1, 1, (points.0 - 2) as u32, (points.1 - 2) as u32)
    );
    let extent = vk::Extent2D {
        width: physical.0 as u32,
        height: physical.1 as u32,
    };
    let destination = clamp(&mapped, extent).expect("fractional view intersects its host");
    assert_eq!(
        (
            destination[0].x,
            destination[0].y,
            destination[1].x,
            destination[1].y
        ),
        (1, 1, points.0 - 1, points.1 - 1)
    );
    let crop = clamp_to(
        &mapped,
        extent,
        Some(sdl3::Rect {
            x: 2,
            y: 2,
            w: points.0 - 3,
            h: points.1 - 3,
        }),
    )
    .expect("fractional clipped Metal layer");
    let source = source_of(crop, &mapped, (mapped.width * 2, mapped.height * 2));
    assert_eq!(
        (source[0].x, source[0].y, source[1].x, source[1].y),
        (2, 2, (mapped.width * 2) as i32, (mapped.height * 2) as i32)
    );
    // Earlier origin+width rounding returned 319 rather than 318 for the
    // 320-point host, incorrectly mapping the final crop to source x=636.
    objc::arc::release(child);
    objc::arc::release(window);
    objc::arc::pool_pop(pool);
}

#[test]
fn fullscreen_logical_clip_preserves_metal_blit_source_coordinates() {
    let extent = vk::Extent2D {
        width: 100,
        height: 80,
    };
    let frame = appkit::view::Frame {
        window: std::ptr::null_mut(),
        x: -10,
        y: -5,
        width: 120,
        height: 90,
        window_width: 100,
        window_height: 80,
    };
    let allowed = sdl3::Rect {
        x: 0,
        y: 0,
        w: 100,
        h: 49,
    };
    let clipped =
        clamp_to(&frame, extent, Some(allowed)).expect("oversized layer intersects logical window");
    assert_eq!(
        (clipped[0].x, clipped[0].y, clipped[1].x, clipped[1].y),
        (0, 0, 100, 49)
    );
    let source = source_of(clipped, &frame, (240, 180));
    assert_eq!(
        (source[0].x, source[0].y, source[1].x, source[1].y),
        (20, 10, 220, 108)
    );
    assert!(
        clamp_to(
            &appkit::view::Frame {
                y: 51,
                height: 25,
                ..frame
            },
            extent,
            Some(allowed)
        )
        .is_none()
    );
    // The clipping region is computed in the destination coordinate space,
    // not by moving or shortening Frame before deriving source coordinates.
    assert_eq!(clamp_to(&frame, extent, None).unwrap()[1].y, 80);
}

/// The window's store is presented around the layers over it: a rectangle a
/// `CAMetalLayer` owns keeps what that layer last presented there, and the
/// rest of the store still reaches the window.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "Cocoa needs the main thread; use rosemary's Metal guest tests"
)]
fn the_store_is_presented_around_a_layers_rectangle() {
    let whole = Region {
        x: 0,
        y: 0,
        width: 100,
        height: 100,
    };
    let hole = Region {
        x: 20,
        y: 30,
        width: 40,
        height: 10,
    };
    let pieces = subtract(whole, &[hole]);
    // The bands above, below, left and right of the hole, and nothing else.
    assert_eq!(
        pieces,
        vec![
            Region {
                x: 0,
                y: 0,
                width: 100,
                height: 30
            },
            Region {
                x: 0,
                y: 40,
                width: 100,
                height: 60
            },
            Region {
                x: 0,
                y: 30,
                width: 20,
                height: 10
            },
            Region {
                x: 60,
                y: 30,
                width: 40,
                height: 10
            },
        ]
    );
    // Every piece is inside the whole and outside the hole, and together they
    // are the whole less the hole.
    let area: i32 = pieces.iter().map(|p| p.width * p.height).sum();
    assert_eq!(area, 100 * 100 - 40 * 10);

    // A layer covering the window leaves nothing of the store, and a hole
    // outside it takes nothing away.
    assert!(subtract(whole, &[whole]).is_empty());
    assert_eq!(
        subtract(
            whole,
            &[Region {
                x: 200,
                y: 0,
                width: 10,
                height: 10
            }]
        ),
        vec![whole]
    );
}

#[test]
fn a_region_of_a_layer_is_blitted_from_the_matching_part_of_its_drawable() {
    let frame = appkit::view::Frame {
        window: std::ptr::null_mut(),
        x: 10,
        y: 20,
        width: 100,
        height: 50,
        window_width: 400,
        window_height: 300,
    };
    let at = |x, y| vk::Offset3D { x, y, z: 0 };
    let to = |x, y| vk::Offset3D { x, y, z: 1 };
    // A drawable at twice the view's size: the lower half of the view comes
    // from the lower half of the drawable, not from the window's coordinates.
    assert_eq!(
        source_of([at(10, 45), to(110, 70)], &frame, (200, 100)),
        [at(0, 50), to(200, 100)]
    );
    // The window's own store is its size, so a region maps to itself.
    let store = appkit::view::Frame {
        x: 0,
        y: 0,
        width: 400,
        height: 300,
        ..frame
    };
    assert_eq!(
        source_of([at(10, 45), to(110, 70)], &store, (400, 300)),
        [at(10, 45), to(110, 70)]
    );
}

#[test]
fn fifo_wsi_error_classes_do_not_retry_terminal_acquisition() {
    assert!(matches!(
        classify_compositor_acquire(Err(vk::Result::TIMEOUT)),
        CompositorAcquire::TimedOut
    ));
    assert!(matches!(
        classify_compositor_acquire(Err(vk::Result::ERROR_OUT_OF_DATE_KHR)),
        CompositorAcquire::Ready(None)
    ));
    assert!(matches!(
        classify_compositor_acquire(Err(vk::Result::ERROR_SURFACE_LOST_KHR)),
        CompositorAcquire::Terminal(vk::Result::ERROR_SURFACE_LOST_KHR)
    ));
    assert_eq!(
        terminal_present_failure(vk::Result::ERROR_SURFACE_LOST_KHR),
        1
    );
    assert_eq!(terminal_present_failure(vk::Result::ERROR_DEVICE_LOST), 2);
}

/// Opt-in, same-host WSI stress proxy. This is not Subnautica and does
/// not establish end-to-end guest frame latency.
#[test]
#[cfg(not(target_os = "macos"))]
fn benchmark_live_immediate_acquisition_policy() {
    if std::env::var_os("ROSEMARY_BENCH_COMPOSITOR_WSI").is_none() {
        return;
    }
    if isolate_vulkan_graphical_test("benchmark_live_immediate_acquisition_policy") {
        return;
    }
    let _retire_guard = AwaitTestWindowRetirements;
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let (width, height) = (640u32, 360u32);
    let window = sdl3::video::create_window(
        c"WSI acquisition microprofile",
        width as i32,
        height as i32,
        0,
    )
    .unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let images = [
        test_render_target(gpu, width, height).unwrap(),
        test_render_target(gpu, width, height).unwrap(),
    ];
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, images[0].image, [0., 0., 1., 1.]);
        cmd_test_clear_color(device, cb, images[1].image, [0., 1., 0., 1.]);
    })
    .unwrap();
    let frame = test_full_frame(window, width, height);
    crate::stats::tick();
    let started = Instant::now();
    let updates = 300;
    let mut deferred = 0;
    for n in 0..updates {
        let due = started + Duration::from_micros(n as u64 * 4_000);
        if Instant::now() < due {
            std::thread::sleep(due - Instant::now());
        }
        if !present(gpu, window as usize, &frame, &images[n % 2], &[]).unwrap() {
            deferred += 1;
        }
        if n == 0 {
            let entry = SWAPCHAINS
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .get(&(window as usize))
                .unwrap()
                .clone();
            ensure_layer_compositor_worker(window as usize, &entry);
        }
    }
    std::thread::sleep(Duration::from_millis(250));
    let elapsed = started.elapsed();
    let entry = SWAPCHAINS
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get(&(window as usize))
        .unwrap()
        .clone();
    let state = entry.state.lock().unwrap();
    let submitted = state.wsi_publishes;
    let mode = state.present_mode;
    let dirty = state.composite_dirty;
    drop(state);
    eprintln!(
        "LIVE_WSI_BENCH selected_mode={mode:?} guest_updates={updates} deferred={deferred} wsi_publishes={submitted} elapsed_ms={:.2} dirty={dirty}",
        elapsed.as_secs_f64() * 1000.
    );
    crate::stats::tick();
    forget_window(window);
    drop(entry);
    for image in images {
        gpu.destroy_image(image);
    }
    sdl3::video::destroy_window(window);
}

#[test]
fn a_fifo_window_that_keeps_timing_out_backs_off_to_a_bound() {
    let waits: Vec<u64> = (1..=8)
        .map(|timeouts| compositor_acquire_backoff(timeouts).as_millis() as u64)
        .collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 32, 32, 32]);
}

/// Minimize a live FIFO window while its WSI reports no presentable area.
/// Injected zero extent ensures deterministic behavior across X11/Wayland
/// compositors, some of which retain the prior dimensions while minimized.
#[test]
#[cfg_attr(target_os = "macos", ignore = "Cocoa needs the main thread")]
fn minimized_compositor_recovers_after_repeated_zero_extent_rebuilds() {
    if isolate_vulkan_graphical_test(
        "minimized_compositor_recovers_after_repeated_zero_extent_rebuilds",
    ) {
        return;
    }
    let _retire_guard = AwaitTestWindowRetirements;
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let adapters = crate::vulkan::adapters().expect("Vulkan adapters");
    let gpu = adapters.first().expect("adapter").gpu().expect("GPU");
    let window = sdl3::video::create_window(c"FIFO minimized recovery", 220, 160, 0)
        .expect("SDL test window");
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).expect("SDL snapshot");
    let source = test_render_target(gpu, 220, 160).expect("source image");
    let frame = test_full_frame(window, 220, 160);
    assert_eq!(
        present(gpu, window as usize, &frame, &source, &[]),
        Ok(true)
    );
    let entry = SWAPCHAINS
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get(&(window as usize))
        .expect("installed swapchain")
        .clone();
    assert!(uses_layer_compositor(
        entry.state.lock().unwrap().present_mode
    ));
    let before = {
        let _presenting = entry.presenting.lock().unwrap();
        let mut state = entry.state.lock().unwrap();
        assert!(state.composite_dirty);
        state.injected_unavailable_extent = true;
        state.retired = true;
        let before = state.rebuild_attempts;
        ensure_layer_compositor_worker(window as usize, &entry);
        before
    };
    sdl3::video::minimize_window(window);
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let attempts = entry.state.lock().unwrap().rebuild_attempts;
        if attempts >= before + 4 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "FIFO worker did not retry zero extent"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(entry.present_failure.load(Ordering::Acquire), 0);
    assert!(
        entry.state.lock().unwrap().composite_dirty,
        "snapshot lost while minimized"
    );
    sdl3::video::restore_window(window);
    {
        let _presenting = entry.presenting.lock().unwrap();
        entry.state.lock().unwrap().injected_unavailable_extent = false;
    }
    wake_compositor_worker(&entry);
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let state = entry.state.lock().unwrap();
        if !state.composite_dirty {
            assert!(!state.retired, "restoration never rebuilt WSI");
            break;
        }
        assert_eq!(
            entry.present_failure.load(Ordering::Acquire),
            0,
            "temporary zero area permanently disabled FIFO"
        );
        assert!(
            Instant::now() < deadline,
            "restored FIFO window never presented snapshot"
        );
        drop(state);
        std::thread::sleep(Duration::from_millis(5));
    }
    forget_window(window);
    drop(entry);
    sdl3::video::destroy_window(window);
    gpu.destroy_image(source);
}

#[test]
fn unsupported_sdl_surface_falls_back_without_permanent_transient_blacklisting() {
    if isolate_vulkan_graphical_test(
        "unsupported_sdl_surface_falls_back_without_permanent_transient_blacklisting",
    ) {
        return;
    }
    let permanent = sdl3::video::vulkan::SurfaceFailure {
        reason: "SDL Vulkan not supported on window surface".into(),
        transient: false,
    };
    assert!(permanent_sdl_surface_failure(&permanent));
    let temporary = sdl3::video::vulkan::SurfaceFailure {
        reason: "SDL surface not available while minimized".into(),
        transient: true,
    };
    assert!(!permanent_sdl_surface_failure(&temporary));
    let unknown = sdl3::video::vulkan::SurfaceFailure {
        reason: "unknown SDL Vulkan surface error".into(),
        transient: false,
    };
    assert!(!permanent_sdl_surface_failure(&unknown));
    let key = 0x7947a1usize;
    reset_retry(key);
    for _ in 1..MAX_UNCLASSIFIED_SURFACE_FAILURES {
        assert!(schedule_retry(key, true));
    }
    assert!(
        !schedule_retry(key, true),
        "permanently unsupported surface retries forever"
    );
    reset_retry(key);
    for _ in 0..20 {
        assert!(
            schedule_retry(key, false),
            "minimized window must never exhaust a lifetime retry budget"
        );
    }
    reset_retry(key);
    assert!(retry_ready(key));
}

/// A new layer must not join the occlusion set until its first frame can
/// actually be staged after a deferred WSI rebuild.
#[test]
#[cfg_attr(target_os = "macos", ignore = "SDL/Cocoa main thread integration")]
fn failed_async_rebuild_does_not_register_unpresented_layer() {
    if isolate_vulkan_graphical_test("failed_async_rebuild_does_not_register_unpresented_layer") {
        return;
    }
    let _retire_guard = AwaitTestWindowRetirements;
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let window = sdl3::video::create_window(c"deferred layer rebuild", 200, 150, 0).unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let source = test_render_target(gpu, 200, 150).unwrap();
    let frame = test_full_frame(window, 200, 150);
    assert_eq!(
        present(gpu, window as usize, &frame, &source, &[]),
        Ok(true)
    );
    let entry = SWAPCHAINS
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get(&(window as usize))
        .unwrap()
        .clone();
    let layer = window as usize + 0x40;
    sdl3::video::minimize_window(window);
    {
        let mut state = entry.state.lock().unwrap();
        state.injected_unavailable_extent = true;
        state.retired = true;
    }
    let started = Instant::now();
    assert_eq!(
        present_nonblocking(gpu, layer, &frame, &source, &[]),
        Ok(None)
    );
    assert!(
        started.elapsed() < Duration::from_millis(150),
        "nonblocking resize waited for WSI"
    );
    {
        let state = entry.state.lock().unwrap();
        assert!(
            !state.layers.contains(&layer),
            "failed rebuild registered a never-presented layer"
        );
        assert_eq!(
            state.pending_rebuild,
            Some(vk::Extent2D {
                width: 200,
                height: 150
            })
        );
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if entry.state.lock().unwrap().rebuild_attempts >= 4 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "worker did not retry deferred rebuild"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!entry.state.lock().unwrap().layers.contains(&layer));
    sdl3::video::restore_window(window);
    {
        let _presenting = entry.presenting.lock().unwrap();
        entry.state.lock().unwrap().injected_unavailable_extent = false;
    }
    wake_resize_worker(&entry);
    let deadline = Instant::now() + Duration::from_secs(5);
    while entry.state.lock().unwrap().pending_rebuild.is_some() {
        assert!(
            Instant::now() < deadline,
            "WSI did not recover after restore"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(present(gpu, layer, &frame, &source, &[]), Ok(true));
    assert!(entry.state.lock().unwrap().layers.contains(&layer));
    forget_window(window);
    drop(entry);
    sdl3::video::destroy_window(window);
    gpu.destroy_image(source);
}

fn v17_async_window_fixture() -> Option<(
    sdl3::Window,
    &'static Gpu,
    GpuImage,
    appkit::view::Frame,
    Arc<WindowSwapchain>,
)> {
    // FIXME (TESTS): this was turned off on macOS, so i'm leaving it stubbed
    if cfg!(target_os = "macos") {
        return None;
    }

    std::env::var_os("ROSEMARY_TEST_VULKAN")?;
    let gpu = crate::vulkan::adapters().ok()?.first()?.gpu().ok()?;
    let window = sdl3::video::create_window(c"WSI asynchronous ownership", 240, 180, 0)?;
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window)?;
    let frame = test_full_frame(window, 240, 180);
    let source = test_render_target(gpu, 240, 180).ok()?;
    assert_eq!(
        present(gpu, window as usize, &frame, &source, &[]),
        Ok(true)
    );
    flush_if_composed(window);
    let entry = SWAPCHAINS
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get(&(window as usize))
        .unwrap()
        .clone();
    Some((window, gpu, source, frame, entry))
}

#[test]
#[cfg(not(target_os = "macos"))]
fn stalled_resize_cannot_hold_appkit_window_destruction_or_destroy_sdl_early() {
    if isolate_vulkan_graphical_test(
        "stalled_resize_cannot_hold_appkit_window_destruction_or_destroy_sdl_early",
    ) {
        return;
    }
    let Some((window, gpu, source, frame, entry)) = v17_async_window_fixture() else {
        return;
    };
    let key = window as usize;
    let identity = sdl3::video::window_id(window);
    assert_ne!(identity, 0);
    TEST_WINDOW_RETIRED.store(0, Ordering::Release);
    TEST_BLOCK_RESIZE_WORKER.store(key, Ordering::Release);
    let resized = appkit::view::Frame {
        window_width: frame.window_width + 1,
        ..frame
    };
    assert_eq!(
        present_nonblocking(gpu, key, &resized, &source, &[]),
        Ok(None),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while TEST_RESIZE_WORKER_PAUSED.load(Ordering::Acquire) != key {
        assert!(
            Instant::now() < deadline,
            "worker never held the WSI presentation lock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let start = Instant::now();
    forget_window(window);
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "AppKit destruction waited for a worker stalled in Vulkan"
    );
    let start = Instant::now();
    sdl3::video::destroy_window(window);
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "SDL window destroy waited for the worker"
    );
    sdl3::video::vulkan::service_surface_requests();
    assert_eq!(
        sdl3::video::window_id(window),
        identity,
        "SDL destroyed the native window while Vulkan still owned its surface"
    );
    assert!(
        SWAPCHAINS
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .get(&key)
            .is_none()
    );
    TEST_BLOCK_RESIZE_WORKER.store(0, Ordering::Release);
    drop(entry);
    let deadline = Instant::now() + Duration::from_secs(5);
    while TEST_WINDOW_RETIRED.load(Ordering::Acquire) != key {
        assert!(
            Instant::now() < deadline,
            "async WSI retirement never completed"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    sdl3::video::vulkan::service_surface_requests();
    gpu.destroy_image(source);
}

// Isolated Vulkan tests must not exit while a detached swapchain drop
// still calls the host ICD. Production teardown remains asynchronous.
struct AwaitTestWindowRetirements;
impl Drop for AwaitTestWindowRetirements {
    fn drop(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(6);
        while TEST_ACTIVE_RETIREMENTS.load(Ordering::Acquire) != 0 {
            assert!(
                Instant::now() < deadline,
                "test WSI retirement did not finish"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        sdl3::video::vulkan::service_surface_requests();
    }
}

/// SDL recursively deletes native popups. Recreating an AppKit parent must
/// rehome the guest's popup before retiring the child's old GPU swapchain.
#[test]
fn appkit_parent_remake_retires_gpu_backed_popup_without_dangling_nswindow() {
    if isolate_vulkan_graphical_test(
        "appkit_parent_remake_retires_gpu_backed_popup_without_dangling_nswindow",
    ) {
        return;
    }
    let _retire_guard = AwaitTestWindowRetirements;
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|adapter| adapter.gpu().ok()) else {
        return;
    };
    crate::bootstrap();
    let rect = foundation::geometry::NSRect {
        origin: foundation::geometry::NSPoint { x: 0.0, y: 0.0 },
        size: foundation::geometry::NSSize {
            width: 180.0,
            height: 120.0,
        },
    };
    let class = objc::class::lookup(c"NSWindow").unwrap();
    let make = || {
        let object = objc::object::create_instance(class, appkit::window::INSTANCE_SIZE);
        unsafe {
            let init: unsafe extern "C" fn(
                objc::object::Id,
                objc::sel::Sel,
                foundation::geometry::NSRect,
                usize,
                usize,
                u8,
            ) = std::mem::transmute(objc::msg::entry());
            init(
                object,
                objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
                rect,
                0,
                0,
                0,
            );
        }
        object
    };
    let parent = make();
    let child = make();
    // SAFETY: AppKit instance methods with their registered Objective-C ABI.
    unsafe {
        let order: unsafe extern "C" fn(objc::object::Id, objc::sel::Sel, objc::object::Id) =
            std::mem::transmute(objc::msg::entry());
        order(parent, objc::sel::get("orderFront:"), std::ptr::null_mut());
        let level: unsafe extern "C" fn(objc::object::Id, objc::sel::Sel, isize) =
            std::mem::transmute(objc::msg::entry());
        level(child, objc::sel::get("setLevel:"), 101);
        order(child, objc::sel::get("orderFront:"), std::ptr::null_mut());
    }
    let old_parent = unsafe { appkit::window::sdl_window(parent) };
    let old_child = unsafe { appkit::window::sdl_window(child) };
    assert!(!old_parent.is_null() && !old_child.is_null());
    assert_eq!(sdl3::video::window_parent(old_child), old_parent);
    let old_child_id = sdl3::video::live_window_id(old_child).unwrap();
    sdl3::video::presentation_window(old_child).expect("child snapshot");
    let child_frame = test_full_frame(old_child, 180, 120);
    let source = test_render_target(gpu, 180, 120).expect("GPU image for child");
    assert_eq!(
        present(gpu, 0x450_aaa, &child_frame, &source, &[]),
        Ok(true),
        "the child must actually own a GPU-backed SDL/Vulkan presentation"
    );
    assert!(
        SWAPCHAINS
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .contains_key(&(old_child as usize)),
        "no GPU swapchain created for child"
    );
    // A GPU-converted child may also have a parked CPU backing-store uploader.
    // Its worker must receive the same descendant destroy notification as WSI.
    let mut store = appkit::backing::Store::new(180, 120);
    store.pixels.fill(91);
    let _ = crate::present::present_store(old_child, &store, &[], &[]);
    assert!(
        crate::present::test_has_store_upload_worker(old_child),
        "GPU-backed child did not create its store upload worker"
    );
    assert!(appkit::window::rebuild_for_opengl(parent), "parent remake");
    let new_parent = unsafe { appkit::window::sdl_window(parent) };
    let new_child = unsafe { appkit::window::sdl_window(child) };
    assert!(!new_parent.is_null() && new_parent != old_parent);
    assert!(!new_child.is_null() && new_child != old_child);
    assert_eq!(sdl3::video::window_parent(new_child), new_parent);
    assert_eq!(appkit::window::window_for(new_child), child);
    assert!(appkit::window::window_for(old_child).is_null());
    assert!(sdl3::video::live_window_id(old_child).is_none());
    // Native deletion waits for the GPU-backed child's retirement token.
    assert!(
        !SWAPCHAINS
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .contains_key(&(old_child as usize)),
        "old popup's Metal state survived recursive native destruction"
    );
    assert!(
        !crate::present::test_has_store_upload_worker(old_child),
        "recursive native teardown left the child backing-store worker parked"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !sdl3::video::window_from_id(old_child_id).is_null() {
        sdl3::video::vulkan::service_surface_requests();
        assert!(Instant::now() < deadline, "GPU-backed popup never retired");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(appkit::window::window_for(old_parent).is_null());
    assert_eq!(appkit::window::window_for(new_child), child);
    // A remade parent also owns this fresh popup; AppKit's normal recursive
    // cleanup must release it before the replacement parent is destroyed.
    // A replacement OpenGL parent cannot itself be used for this Vulkan test.
    gpu.destroy_image(source);
}

/// A converted SDL window cannot fall back to CPU surface readback. A
/// transient stage failure keeps the drawable queued, and a terminal OOM
/// reports completion without attempting an invalid fallback.
#[test]
fn converted_fifo_staging_failure_never_uses_cpu_fallback_or_dirties_first_layer() {
    if isolate_vulkan_graphical_test(
        "converted_fifo_staging_failure_never_uses_cpu_fallback_or_dirties_first_layer",
    ) {
        return;
    }
    assert!(!composite_first_stage_needs_store(true, &Ok(None), 42, 7));
    assert!(!composite_first_stage_needs_store(
        true,
        &Err("staging failed".into()),
        42,
        7
    ));
    assert!(composite_first_stage_needs_store(
        true,
        &Ok(Some(true)),
        42,
        7
    ));
    assert!(!composite_first_stage_needs_store(
        false,
        &Ok(Some(true)),
        42,
        7
    ));
    assert!(!composite_first_stage_needs_store(
        true,
        &Ok(Some(true)),
        7,
        7
    ));

    let _retire_guard = AwaitTestWindowRetirements;
    let Some((window, gpu, source, frame, entry)) = v17_async_window_fixture() else {
        return;
    };
    let child_layer = (window as usize).wrapping_add(0x129);
    {
        let mut state = entry.state.lock().unwrap();
        state.present_mode = vk::PresentModeKHR::FIFO;
        // Prove an error does not transfer the presenter's drawable retain
        // into a GPU frame or complete the ring slot during staging.
        let mut retained = SourceOwner(Some(0xfeed_f001));
        state.injected_stage_vk_error = Some(vk::Result::ERROR_UNKNOWN);
        assert!(
            state
                .stage_composite(&frame, &source, None, &mut retained, true)
                .is_err()
        );
        assert_eq!(retained.0, Some(0xfeed_f001));
        retained.0 = None; // dummy identity, not an Objective-C allocation
        state.injected_stage_vk_error = Some(vk::Result::ERROR_UNKNOWN);
    }
    assert!(sdl3::video::vulkan::owns_window(window));
    assert_eq!(
        present_nonblocking(gpu, child_layer, &frame, &source, &[]),
        Ok(None),
        "retryable FIFO staging failure must not consume a Metal drawable"
    );
    assert!(
        !entry.state.lock().unwrap().layers.contains(&child_layer),
        "failed first stage must not register a layer"
    );
    assert_eq!(entry.present_failure.load(Ordering::Acquire), 0);
    // A source which has not been staged needs no new CPU store damage.
    assert!(!composite_first_stage_needs_store(
        true,
        &Ok(None),
        child_layer,
        window as usize
    ));

    // The terminal error must also wake a clean, parked FIFO worker so it
    // can retire instead of retaining the window indefinitely.
    ensure_layer_compositor_worker(window as usize, &entry);
    {
        let mut state = entry.state.lock().unwrap();
        state.injected_stage_vk_error = Some(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY);
    }
    assert_eq!(
        present_nonblocking(gpu, child_layer, &frame, &source, &[]),
        Ok(Some(true)),
        "terminal FIFO error must not be routed to unusable SDL readback"
    );
    assert_eq!(entry.present_failure.load(Ordering::Acquire), 2);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !entry
        .worker
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|h| h.is_finished())
    {
        assert!(
            Instant::now() < deadline,
            "terminal staging left FIFO worker asleep"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    forget_window(window);
    drop(entry);
    sdl3::video::destroy_window(window);
    gpu.destroy_image(source);
}

#[test]
fn asynchronous_resize_device_loss_notifies_other_windows() {
    if isolate_vulkan_graphical_test("asynchronous_resize_device_loss_notifies_other_windows") {
        return;
    }
    let _retire_guard = AwaitTestWindowRetirements;
    let Some((first, gpu, first_source, _, first_entry)) = v17_async_window_fixture() else {
        return;
    };
    let Some((second, _, second_source, _, second_entry)) = v17_async_window_fixture() else {
        forget_window(first);
        drop(first_entry);
        sdl3::video::destroy_window(first);
        gpu.destroy_image(first_source);
        return;
    };
    assert!(!gpu.lost.load(Ordering::Acquire));
    {
        let mut state = first_entry.state.lock().unwrap();
        state.injected_build_vk_error = Some(vk::Result::ERROR_DEVICE_LOST);
        state.pending_rebuild = Some(state.requested);
    }
    ensure_resize_worker(first as usize, &first_entry);
    wake_resize_worker(&first_entry);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !gpu.lost.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "async Vulkan loss remained window-local"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(first_entry.present_failure.load(Ordering::Acquire), 2);
    assert_eq!(
        second_entry.present_failure.load(Ordering::Acquire),
        2,
        "other idle window did not receive device-wide loss notification"
    );
    forget_window(first);
    forget_window(second);
    drop(first_entry);
    drop(second_entry);
    sdl3::video::destroy_window(first);
    sdl3::video::destroy_window(second);
    // Injected loss, not a real dead device; allow test fixture cleanup.
    gpu.lost.store(false, Ordering::Release);
    gpu.destroy_image(first_source);
    gpu.destroy_image(second_source);
}

#[test]
fn compositor_preparation_device_loss_wakes_other_window_workers() {
    if isolate_vulkan_graphical_test(
        "compositor_preparation_device_loss_wakes_other_window_workers",
    ) {
        return;
    }
    let _retire_guard = AwaitTestWindowRetirements;
    let Some((first, gpu, first_source, _, first_entry)) = v17_async_window_fixture() else {
        return;
    };
    let Some((second, _, second_source, _, second_entry)) = v17_async_window_fixture() else {
        forget_window(first);
        drop(first_entry);
        sdl3::video::destroy_window(first);
        gpu.destroy_image(first_source);
        return;
    };
    assert!(!gpu.lost.load(Ordering::Acquire));
    {
        let mut state = first_entry.state.lock().unwrap();
        assert!(uses_layer_compositor(state.present_mode));
        state.retired = true;
        state.composite_dirty = true;
        state.composite_dirty_since = None;
        state.injected_build_vk_error = Some(vk::Result::ERROR_DEVICE_LOST);
    }
    ensure_layer_compositor_worker(first as usize, &first_entry);
    wake_compositor_worker(&first_entry);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !gpu.lost.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "FIFO preparation loss remained local"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(first_entry.present_failure.load(Ordering::Acquire), 2);
    assert_eq!(second_entry.present_failure.load(Ordering::Acquire), 2);
    forget_window(first);
    forget_window(second);
    drop(first_entry);
    drop(second_entry);
    sdl3::video::destroy_window(first);
    sdl3::video::destroy_window(second);
    gpu.lost.store(false, Ordering::Release); // injected, device remains healthy
    gpu.destroy_image(first_source);
    gpu.destroy_image(second_source);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn native_scene_rollback_metadata_survives_gpu_escrow() {
    if isolate_vulkan_graphical_test("native_scene_rollback_metadata_survives_gpu_escrow") {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let desc = ImageDescription {
        image_type: vk::ImageType::TYPE_2D,
        view_type: vk::ImageViewType::TYPE_2D,
        format: vk::Format::B8G8R8A8_UNORM,
        width: 8,
        height: 8,
        depth: 1,
        levels: 1,
        layers: 1,
        samples: 1,
        usage: gpu::USAGE_RENDER_TARGET,
        cube: false,
        components: vk::ComponentMapping::default(),
    };
    let old = BackingImageLease::create_for_window(gpu, window as usize, &desc).unwrap();
    gpu.one_shot(|d, cb| unsafe {
        cmd_test_clear_color(d, cb, old.image.image, [0.0, 1.0, 0.0, 1.0]);
    })
    .unwrap();
    let id = (1usize << (usize::BITS - 1)) | 0xec01;
    state.seed_accepted_native_for_budget_test(id, DirectFrame::from_frame(&whole), old);
    let layer = state.scene_layers.pop().unwrap();
    let old_revision = layer.native_revision;
    let saved = scene::NativeGpuEscrow::capture(&mut state, &layer.snapshot)
        .unwrap()
        .unwrap();
    let original_budget = Arc::clone(&layer.snapshot._charge.as_ref().unwrap().budget);
    let used = || original_budget.held.load(Ordering::Acquire);
    let before = used();
    let mut backup = scene::SceneRollbackLayer::from_gpu(layer);
    backup.replace_with_escrow(saved);
    assert_eq!(
        before - used(),
        8 * 8 * 4,
        "the original GPU allocation must retire"
    );
    assert_eq!(backup.escrow_bytes(), 8 * 8 * 4);
    assert!(backup.resident().is_none());
    let restored = backup.materialize(gpu, window as usize).unwrap();
    assert_eq!(restored.id, id);
    assert_eq!(restored.native_revision, old_revision);
    assert_eq!(
        read_scene_pixel(gpu, restored.snapshot.image.image, 8, 8, 4, 4),
        [0, 255, 0, 255]
    );
    drop(restored);
    drop(backup);
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// Physical old+new scene leases exceed 768MiB but final accepted scene fits
/// under 512MiB. Escrowing obsolete accepted GPU generations must really
/// retire their charges, and the inverse transaction must recover their pixels.
#[test]
#[cfg(not(target_os = "macos"))]
fn atomic_768mib_native_rollback_escrow_reclaims_real_gpu_images() {
    if isolate_vulkan_graphical_test(
        "atomic_768mib_native_rollback_escrow_reclaims_real_gpu_images",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let desc = ImageDescription {
        image_type: vk::ImageType::TYPE_2D,
        view_type: vk::ImageViewType::TYPE_2D,
        format: vk::Format::B8G8R8A8_UNORM,
        width: 2048,
        height: 2048,
        depth: 1,
        levels: 1,
        layers: 1,
        samples: 1,
        usage: gpu::USAGE_RENDER_TARGET,
        cube: false,
        components: vk::ComponentMapping::default(),
    };
    for n in 0..28usize {
        let lease = BackingImageLease::create_for_window(gpu, window as usize, &desc).unwrap();
        state.seed_accepted_native_for_budget_test(
            (1usize << (usize::BITS - 1)) | 0xfb00 | n,
            DirectFrame::from_frame(&whole),
            lease,
        );
    }
    let old_images: Vec<_> = state
        .scene_layers
        .iter()
        .map(|layer| layer.snapshot.image.image)
        .collect();
    gpu.one_shot(|device, cb| unsafe {
        for &source in &old_images {
            cmd_test_clear_color(device, cb, source, [1.0, 0.0, 0.0, 1.0]);
        }
    })
    .unwrap();
    let mut prior: Vec<_> = state
        .scene_layers
        .iter()
        .cloned()
        .map(scene::SceneRollbackLayer::from_gpu)
        .collect();
    state.transaction_pinned = state
        .scene_layers
        .iter()
        .map(|layer| Arc::clone(&layer.snapshot))
        .collect();
    let mut proposal = state.scene_placements.clone();
    for (index, proposed) in proposal.iter_mut().enumerate().take(19) {
        proposed.native.as_mut().unwrap().revision = 2;
        let replacement =
            BackingImageLease::create_for_window(gpu, window as usize, &desc).unwrap();
        state.scene_layers[index].snapshot = replacement;
        state.scene_layers[index].native_revision = Some(2);
    }
    state.transaction_retiring_bytes = 19 * 16 * 1024 * 1024;
    let scratch = 4 * 16 * 1024 * 1024u64;
    assert_eq!(state.atomic_transition_gpu_floor(), 47 * 16 * 1024 * 1024);
    assert!(!window_gpu_capacity_available(window as usize, scratch));
    for _ in 0..4 {
        assert!(matches!(
            escrow_one_retired_native(&mut state, &mut prior, &proposal).unwrap(),
            NativeEscrowProgress::Released
        ));
    }
    assert!(window_gpu_capacity_available(window as usize, scratch));
    assert_eq!(
        prior
            .iter()
            .map(scene::SceneRollbackLayer::escrow_bytes)
            .sum::<u64>(),
        scratch
    );
    state.scene_layers.clear();
    state.transaction_pinned.clear();
    state.transaction_retiring_bytes = 0;
    state.pending_scene_rollback = Some(prior);
    // Rebuilding an old image needs both a charged image and a charged upload
    // buffer. Exhaustion after partial progress cannot publish a partial old
    // scene or consume the authoritative CPU rollback pixels.
    let blocker = GpuCharge::reserve(window as usize, 350 * 1024 * 1024).unwrap();
    assert!(restore_native_rollback(&mut state).is_err());
    assert!(state.scene_layers.is_empty());
    assert!(state.pending_scene_rollback.is_some());
    drop(blocker);
    restore_native_rollback(&mut state).expect("restore all old accepted generations");
    assert_eq!(state.scene_layers.len(), 28);
    for index in 0..4 {
        assert_eq!(state.scene_layers[index].native_revision, Some(1));
        assert_eq!(
            read_scene_pixel(
                gpu,
                state.scene_layers[index].snapshot.image.image,
                2048,
                2048,
                16,
                16
            ),
            [0, 0, 255, 255]
        );
    }
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn native_atomic_escrow_defers_busy_scene_reader_without_refusal() {
    if isolate_vulkan_graphical_test(
        "native_atomic_escrow_defers_busy_scene_reader_without_refusal",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let desc = ImageDescription {
        image_type: vk::ImageType::TYPE_2D,
        view_type: vk::ImageViewType::TYPE_2D,
        format: vk::Format::B8G8R8A8_UNORM,
        width: 8,
        height: 8,
        depth: 1,
        levels: 1,
        layers: 1,
        samples: 1,
        usage: gpu::USAGE_RENDER_TARGET,
        cube: false,
        components: vk::ComponentMapping::default(),
    };
    let accepted = BackingImageLease::create_for_window(gpu, window as usize, &desc).unwrap();
    let id = (1usize << (usize::BITS - 1)) | 0xab01;
    state.seed_accepted_native_for_budget_test(id, DirectFrame::from_frame(&whole), accepted);
    let mut rollback = state
        .scene_layers
        .iter()
        .cloned()
        .map(scene::SceneRollbackLayer::from_gpu)
        .collect::<Vec<_>>();
    state
        .transaction_pinned
        .push(Arc::clone(&state.scene_layers[0].snapshot));
    state.transaction_retiring_bytes = 8 * 8 * 4;
    let mut proposal = state.scene_placements.clone();
    proposal[0].native.as_mut().unwrap().revision = 2;
    state.idle();
    unsafe { gpu.device.reset_fences(&[state.frames[0].done]) }.unwrap();
    state.frames[0]
        .scene_reads
        .push(Arc::clone(&state.scene_layers[0].snapshot));
    state.frames[0].submitted = true;
    assert!(matches!(
        escrow_one_retired_native(&mut state, &mut rollback, &proposal).unwrap(),
        NativeEscrowProgress::WaitingForReaders
    ));
    assert!(rollback[0].resident().is_some());
    assert_eq!(state.scene_layers.len(), 1);
    state.frames[0].submitted = false;
    state.frames[0].scene_reads.clear();
    assert!(matches!(
        escrow_one_retired_native(&mut state, &mut rollback, &proposal).unwrap(),
        NativeEscrowProgress::Released
    ));
    assert!(rollback[0].resident().is_none());
    state.transaction_pinned.clear();
    state.pending_scene_rollback = Some(rollback);
    restore_native_rollback(&mut state).unwrap();
    assert_eq!(state.scene_layers.len(), 1);
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// Native same-ID replacement must *finish* a real retained Vulkan scene,
/// not merely free synthetic budget leases in an isolated accounting test.
#[test]
#[cfg(not(target_os = "macos"))]
fn atomic_native_replacement_commits_after_768mib_gpu_peak() {
    if isolate_vulkan_graphical_test("atomic_native_replacement_commits_after_768mib_gpu_peak") {
        return;
    }
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let window = sdl3::video::create_window(c"native atomic 768MB commit", 2048, 2048, 0).unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        window,
        vk::Extent2D {
            width: 2048,
            height: 2048,
        },
        surface,
        instance,
    )
    .unwrap();
    let full = test_full_frame(window, 2048, 2048);
    let desc = ImageDescription {
        image_type: vk::ImageType::TYPE_2D,
        view_type: vk::ImageViewType::TYPE_2D,
        format: vk::Format::B8G8R8A8_UNORM,
        width: 2048,
        height: 2048,
        depth: 1,
        levels: 1,
        layers: 1,
        samples: 1,
        usage: gpu::USAGE_RENDER_TARGET | gpu::USAGE_SHADER_READ,
        cube: false,
        components: vk::ComponentMapping::default(),
    };
    for index in 0..28usize {
        let lease = BackingImageLease::create_for_window(gpu, window as usize, &desc).unwrap();
        state.seed_accepted_native_for_budget_test(
            (1usize << (usize::BITS - 1)) | 0xfe00 | index,
            DirectFrame::from_frame(&full),
            lease,
        );
    }
    let original = state.scene_placements.clone();
    let images: Vec<_> = state
        .scene_layers
        .iter()
        .map(|l| l.snapshot.image.image)
        .collect();
    gpu.one_shot(|d, cb| unsafe {
        for &image in &images {
            cmd_test_clear_color(d, cb, image, [1.0, 0.0, 0.0, 1.0]);
        }
    })
    .unwrap();
    state.synchronize_scene(window as usize, &original);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    let old_pixel = read_scene_pixel(
        gpu,
        state.composite.as_ref().unwrap().image,
        2048,
        2048,
        60,
        40,
    );
    assert_eq!(old_pixel, [255, 0, 0, 255]);
    let mut rollback = state
        .scene_layers
        .iter()
        .cloned()
        .map(scene::SceneRollbackLayer::from_gpu)
        .collect::<Vec<_>>();
    state.transaction_pinned = state
        .scene_layers
        .iter()
        .map(|l| Arc::clone(&l.snapshot))
        .collect();
    state.transaction_retiring_bytes = 19 * 16 * 1024 * 1024;
    let mut proposed = original;
    let new_bytes = Arc::new([255u8, 0, 0, 255].repeat(2048 * 2048));
    for entry in proposed.iter_mut().skip(9) {
        let native = entry.native.as_mut().unwrap();
        native.revision = 2;
        native.bytes = Arc::clone(&new_bytes);
    }
    for layer in &proposed {
        let result = stage_native_transaction_source(&mut state, layer, &mut rollback, &proposed);
        assert_eq!(
            result,
            Ok(StagedNativeIdentity::from_placement(layer)),
            "native image id={} scene={} floor={} retiring={} physical_free={} gpu_held={}",
            layer.id,
            state.scene_gpu_bytes(),
            state.atomic_transition_gpu_floor(),
            state.transaction_retiring_bytes,
            window_gpu_capacity_available(window as usize, 2 * 16 * 1024 * 1024),
            state.scene_gpu_bytes()
        );
    }
    let escrowed: u64 = rollback
        .iter()
        .map(scene::SceneRollbackLayer::escrow_bytes)
        .sum();
    assert!(
        escrowed > 0,
        "fixture never exercised CPU rollback under peak pressure"
    );
    state.synchronize_scene(window as usize, &proposed);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    state
        .flush_composite(window)
        .expect("committed native scene WSI present");
    state.idle();
    assert_eq!(
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            2048,
            2048,
            60,
            40
        ),
        [0, 0, 255, 255]
    );
    state.transaction_pinned.clear();
    state.transaction_retiring_bytes = 0;
    drop(rollback);
    assert!(state.scene_gpu_bytes() <= 512 * 1024 * 1024);
    drop(state);
    sdl3::video::destroy_window(window);
}

/// CPU-readable linear DMA fallback must complete the *worker* transaction,
/// not just the direct uploader: its GPU revision is not the producer's
/// publication revision, but only the latter may be acknowledged to AppKit.
#[test]
#[cfg(target_os = "linux")]
fn rejected_linear_dma_worker_commits_original_publication() {
    if isolate_vulkan_graphical_test("rejected_linear_dma_worker_commits_original_publication") {
        return;
    }
    use std::os::fd::FromRawFd;
    let retirement = AwaitTestWindowRetirements;
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let window = sdl3::video::create_window(c"linear DMA worker admission", 160, 120, 0).unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let whole = test_full_frame(window, 160, 120);
    let metal = test_render_target(gpu, 160, 120).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, metal.image, [0., 1., 0., 1.]);
    })
    .unwrap();
    let window_id = window as usize;
    assert_eq!(present(gpu, window_id, &whole, &metal, &[]), Ok(true));
    let entry = SWAPCHAINS
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get(&window_id)
        .unwrap()
        .clone();
    let fd = unsafe {
        libc::memfd_create(
            c"worker-linear-fallback".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    assert!(fd >= 0);
    let (width, height, stride) = (30u32, 25u32, 30u32 * 4 + 16);
    let mut image = vec![0u8; (stride * height) as usize];
    for row in image.chunks_exact_mut(stride as usize) {
        for pixel in row.as_chunks_mut::<4>().0.iter_mut().take(width as usize) {
            pixel.copy_from_slice(&[0, 0, 255, 255]);
        }
    }
    assert_eq!(
        unsafe { libc::ftruncate(fd, image.len() as libc::off_t) },
        0
    );
    assert_eq!(
        unsafe { libc::pwrite(fd, image.as_ptr().cast(), image.len(), 0) },
        image.len() as isize
    );
    assert_eq!(
        unsafe {
            libc::fcntl(
                fd,
                libc::F_ADD_SEALS,
                libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE,
            )
        },
        0
    );
    let picture = Arc::new(appkit::backing::Dmabuf {
        width,
        height,
        fourcc: u32::from_le_bytes(*b"AR24"),
        modifier: 0,
        planes: vec![appkit::backing::DmabufPlane {
            fd: unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) },
            offset: 0,
            stride,
        }],
    });
    let id = (1usize << (usize::BITS - 1)) | 0xa021usize;
    let native = ScenePlacement {
        opacity_groups: Vec::new(),
        id,
        frame: DirectFrame::from_frame(&appkit::view::Frame {
            x: 40,
            y: 35,
            width,
            height,
            ..whole
        }),
        opacity: 1.,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new(Vec::new()),
            revision: 17,
            dmabuf: Some(picture),
            dma_sample: None,
        }),
    };
    let base = test_opaque_metal_placement(window_id, DirectFrame::from_frame(&whole));
    entry
        .state
        .lock()
        .unwrap()
        .unsupported_native_dma
        .push(UnsupportedNativeDma {
            id: native.id,
            revision: native.native.as_ref().unwrap().revision,
            producer: Arc::downgrade(native.native.as_ref().unwrap().dmabuf.as_ref().unwrap()),
        });
    *entry.pending_scene.lock().unwrap() = Some(vec![base, native]);
    ensure_layer_compositor_worker(window_id, &entry);
    wake_compositor_worker(&entry);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = entry.state.lock().unwrap();
        let accepted = state.scene_layers.iter().any(|layer| {
            layer.id == id
                && layer.ready
                && layer.native_published_revision == Some(17)
                && layer.native_revision != Some(17)
        });
        if accepted
            && !state.composite_dirty
            && !state.scene_restore_pending
            && entry.pending_scene.lock().unwrap().is_none()
        {
            assert_eq!(
                read_scene_pixel(
                    gpu,
                    state.composite.as_ref().unwrap().image,
                    160,
                    120,
                    48,
                    42
                ),
                [255, 0, 0, 255]
            );
            assert!(
                state.wsi_publishes > 0,
                "worker did not publish fallback WSI image"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "linear fallback worker never acknowledged publication: layers={} restore={} pending={}",
            state.scene_layers.len(),
            state.scene_restore_pending,
            entry.pending_scene.lock().unwrap().is_some()
        );
        drop(state);
        std::thread::sleep(Duration::from_millis(10));
    }
    forget_window(window);
    drop(entry);
    drop(retirement);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// A successful submit followed by a synthetic wait error does not establish
/// completion. The staging heap and image leases remain physically charged
/// until a later query proves the fence complete.
#[test]
#[cfg(not(target_os = "macos"))]
fn native_upload_failed_fence_wait_retains_gpu_resources_until_completion() {
    if isolate_vulkan_graphical_test(
        "native_upload_failed_fence_wait_retains_gpu_resources_until_completion",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let account = GpuCharge::reserve(window as usize, 1).unwrap();
    let before = WINDOW_GPU_BUDGETS
        .lock()
        .unwrap()
        .get(&(window as usize))
        .and_then(std::sync::Weak::upgrade)
        .unwrap();
    let baseline = before.held.load(Ordering::Acquire);
    let native = ScenePlacement {
        opacity_groups: Vec::new(),
        id: (1usize << (usize::BITS - 1)) | 0xd21,
        frame: DirectFrame::from_frame(&appkit::view::Frame {
            x: 5,
            y: 6,
            width: 32,
            height: 32,
            ..whole
        }),
        opacity: 1.,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new([0u8, 0, 255, 255].repeat(32 * 32)),
            revision: 1,
            dmabuf: None,
            dma_sample: None,
        }),
    };
    gpu.inject_immediate_wait_failure_for_test();
    let error = stage_native_scene_source(&mut state, &native).unwrap_err();
    assert!(error.contains("injected immediate fence wait error"));
    assert!(gpu.immediate_quarantine_count_for_test() > 0);
    assert!(
        before.held.load(Ordering::Acquire) > baseline,
        "failed wait prematurely freed charged native input and output"
    );
    assert!(state.scene_layers.is_empty(), "failed upload was admitted");
    gpu.wait_first_quarantined_for_test()
        .expect("actual completion fence");
    gpu.one_shot(|_, _| {}).unwrap();
    assert_eq!(gpu.immediate_quarantine_count_for_test(), 0);
    assert_eq!(
        before.held.load(Ordering::Acquire),
        baseline,
        "completed native upload leaked its charged staging resources"
    );
    drop(state);
    drop(account);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// Native CPU/DMA blending must quarantine the descriptor pass and imported
/// picture along with the staging buffer, source image and destination image.
#[test]
#[cfg(target_os = "linux")]
fn native_dma_failed_fence_wait_keeps_descriptor_and_import_alive() {
    if isolate_vulkan_graphical_test(
        "native_dma_failed_fence_wait_keeps_descriptor_and_import_alive",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let picture = real_gbm_picture_with_pixels(32, 32, |_, _| [128, 0, 127, 255]);
    let before_pictures = Arc::strong_count(&picture);
    let account = GpuCharge::reserve(window as usize, 1).unwrap();
    let budget = WINDOW_GPU_BUDGETS
        .lock()
        .unwrap()
        .get(&(window as usize))
        .and_then(std::sync::Weak::upgrade)
        .unwrap();
    let baseline = budget.held.load(Ordering::Acquire);
    let available_passes = crate::overlay::available_passes_for_test(gpu);
    let native = ScenePlacement {
        opacity_groups: Vec::new(),
        id: (1usize << (usize::BITS - 1)) | 0xd22,
        frame: DirectFrame::from_frame(&appkit::view::Frame {
            x: 5,
            y: 6,
            width: 32,
            height: 32,
            ..whole
        }),
        opacity: 1.,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            cpu_capture: None,
            bytes: Arc::new([255u8, 0, 0, 255].repeat(32 * 32)),
            revision: 1,
            dmabuf: Some(Arc::clone(&picture)),
            dma_sample: None,
        }),
    };
    gpu.inject_immediate_wait_failure_for_test();
    let error = stage_native_scene_source(&mut state, &native).unwrap_err();
    assert!(error.contains("injected immediate fence wait error"));
    assert!(gpu.immediate_quarantine_count_for_test() > 0);
    assert!(budget.held.load(Ordering::Acquire) > baseline);
    assert!(
        Arc::strong_count(&picture) > before_pictures + 1,
        "submitted DMA importer lost strong producer ownership"
    );
    assert_eq!(
        crate::overlay::available_passes_for_test(gpu),
        available_passes.saturating_sub(1),
        "descriptor pass was recycled before completion"
    );
    gpu.wait_first_quarantined_for_test()
        .expect("actual overlay completion fence");
    gpu.one_shot(|_, _| {}).unwrap();
    assert_eq!(gpu.immediate_quarantine_count_for_test(), 0);
    assert_eq!(budget.held.load(Ordering::Acquire), baseline);
    assert_eq!(
        Arc::strong_count(&picture),
        before_pictures + 1,
        "completed blend still retains DMA picture"
    );
    assert_eq!(
        crate::overlay::available_passes_for_test(gpu),
        available_passes.max(1),
        "descriptor pass never returned after completion"
    );
    drop(state);
    drop(account);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn immediate_quarantine_recycles_completed_contexts_across_repeated_faults() {
    if isolate_vulkan_graphical_test(
        "immediate_quarantine_recycles_completed_contexts_across_repeated_faults",
    ) {
        return;
    }
    let Some((window, gpu, state, metal, _)) = v10_fifo_fixture() else {
        return;
    };
    struct CompletionWitness(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for CompletionWitness {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }
    let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // More failures than the pool's eight contexts. Each fence eventually
    // completes, so the recycler must regain contexts instead of exhausting.
    for index in 0..12 {
        gpu.inject_immediate_wait_failure_for_test();
        assert!(
            gpu.one_shot_owned(CompletionWitness(Arc::clone(&completed)), |_, _, _| {})
                .unwrap_err()
                .contains("injected immediate fence wait error")
        );
        assert_eq!(completed.load(Ordering::Acquire), index);
        assert!(gpu.immediate_quarantine_count_for_test() > 0);
        gpu.wait_first_quarantined_for_test().unwrap();
        gpu.one_shot(|_, _| {}).unwrap();
        assert_eq!(completed.load(Ordering::Acquire), index + 1);
        assert_eq!(gpu.immediate_quarantine_count_for_test(), 0);
    }
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn native_escrow_failed_wait_retains_exact_gpu_readback_and_restore_resources() {
    if isolate_vulkan_graphical_test(
        "native_escrow_failed_wait_retains_exact_gpu_readback_and_restore_resources",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, _)) = v10_fifo_fixture() else {
        return;
    };
    let source = BackingImageLease::create_for_window(
        gpu,
        window as usize,
        &ImageDescription {
            image_type: vk::ImageType::TYPE_2D,
            view_type: vk::ImageViewType::TYPE_2D,
            format: vk::Format::B8G8R8A8_UNORM,
            width: 8,
            height: 8,
            depth: 1,
            levels: 1,
            layers: 1,
            samples: 1,
            usage: gpu::USAGE_RENDER_TARGET,
            cube: false,
            components: vk::ComponentMapping::default(),
        },
    )
    .unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, source.image.image, [0., 1., 0., 1.]);
    })
    .unwrap();
    let budget = Arc::clone(&source._charge.as_ref().unwrap().budget);
    let baseline = budget.held.load(Ordering::Acquire);
    gpu.inject_immediate_wait_failure_for_test();
    assert!(
        scene::NativeGpuEscrow::capture(&mut state, &source)
            .err()
            .expect("injected wait failure")
            .contains("injected immediate fence wait error")
    );
    assert_eq!(gpu.immediate_quarantine_count_for_test(), 1);
    assert!(
        budget.held.load(Ordering::Acquire) > baseline,
        "uncertain escrow GPU readback released its allocation"
    );
    gpu.wait_first_quarantined_for_test().unwrap();
    gpu.one_shot(|_, _| {}).unwrap();
    assert_eq!(budget.held.load(Ordering::Acquire), baseline);
    let saved = scene::NativeGpuEscrow::capture(&mut state, &source)
        .unwrap()
        .unwrap();
    gpu.inject_immediate_wait_failure_for_test();
    assert!(
        saved
            .restore(gpu, window as usize)
            .err()
            .expect("injected wait failure")
            .contains("injected immediate fence wait error")
    );
    assert_eq!(gpu.immediate_quarantine_count_for_test(), 1);
    assert!(
        budget.held.load(Ordering::Acquire) > baseline,
        "uncertain escrow GPU restore released its allocation"
    );
    gpu.wait_first_quarantined_for_test().unwrap();
    gpu.one_shot(|_, _| {}).unwrap();
    assert_eq!(budget.held.load(Ordering::Acquire), baseline);
    let recovered = saved.restore(gpu, window as usize).unwrap();
    assert_eq!(
        read_scene_pixel(gpu, recovered.image.image, 8, 8, 4, 4),
        [0, 255, 0, 255]
    );
    drop(recovered);
    drop(source);
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// A changed immutable CPU Arc may patch a GPU base only when AppKit names
/// the exact prior CPU capture revision and rectangle. Old DMA-blended scene
/// pixels stay independently committed through failed/uncertain submissions.
#[test]
#[cfg(target_os = "linux")]
fn native_cpu_producer_damage_reuses_only_exact_gpu_base_and_rolls_back_on_failed_wait() {
    if isolate_vulkan_graphical_test(
        "native_cpu_producer_damage_reuses_only_exact_gpu_base_and_rolls_back_on_failed_wait",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let picture = real_gbm_picture_with_pixels(32, 32, |_, _| [0, 0, 0, 0]);
    let id = (1usize << (usize::BITS - 1)) | 0xac41;
    let frame = DirectFrame::from_frame(&appkit::view::Frame {
        x: 5,
        y: 6,
        width: 32,
        height: 32,
        ..whole
    });
    let old_pixels = [255u8, 0, 0, 255].repeat(32 * 32);
    let mut layer = ScenePlacement {
        opacity_groups: Vec::new(),
        id,
        frame,
        opacity: 1.,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            bytes: Arc::new(old_pixels.clone()),
            revision: 1,
            cpu_capture: Some(appkit::backing::NativeCpuCapture {
                revision: 101,
                damage: None,
            }),
            dmabuf: Some(Arc::clone(&picture)),
            dma_sample: None,
        }),
    };
    assert_eq!(stage_native_scene_source(&mut state, &layer), Ok(true));
    let base_before = Arc::clone(
        &state
            .native_cpu_bases
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .image,
    );
    let accepted_before = Arc::clone(
        &state
            .scene_layers
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .snapshot,
    );
    assert_eq!(
        read_scene_pixel(gpu, accepted_before.image.image, 32, 32, 16, 16),
        [0, 0, 255, 255]
    );
    let mut changed = old_pixels.clone();
    let offset = (16 * 32 + 16) * 4;
    changed[offset..offset + 4].copy_from_slice(&[0, 255, 0, 255]);
    let native = layer.native.as_mut().unwrap();
    native.revision = 2;
    native.bytes = Arc::new(changed.clone());
    native.cpu_capture = Some(appkit::backing::NativeCpuCapture {
        revision: 102,
        damage: Some(appkit::backing::NativeCpuDamage {
            from_revision: 101,
            rect: appkit::backing::Rect {
                x: 16,
                y: 16,
                width: 1,
                height: 1,
            },
        }),
    });
    // The GPU source can be updated exclusively; this old image Arc clone
    // itself is held by the test, so release it before validating exclusivity.
    drop(base_before);
    assert_eq!(stage_native_scene_source(&mut state, &layer), Ok(true));
    let base_after = Arc::clone(
        &state
            .native_cpu_bases
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .image,
    );
    assert_eq!(
        read_scene_pixel(gpu, accepted_before.image.image, 32, 32, 16, 16),
        [0, 0, 255, 255]
    );
    let accepted_after = Arc::clone(
        &state
            .scene_layers
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .snapshot,
    );
    assert_eq!(
        read_scene_pixel(gpu, accepted_after.image.image, 32, 32, 16, 16),
        [0, 255, 0, 255]
    );
    assert_eq!(
        read_scene_pixel(gpu, accepted_after.image.image, 32, 32, 0, 0),
        [0, 0, 255, 255]
    );
    drop(base_after);
    // A stale producer predecessor cannot authenticate the patched base.
    changed[offset..offset + 4].copy_from_slice(&[128, 64, 0, 255]);
    let native = layer.native.as_mut().unwrap();
    native.revision = 3;
    native.bytes = Arc::new(changed.clone());
    native.cpu_capture = Some(appkit::backing::NativeCpuCapture {
        revision: 103,
        damage: Some(appkit::backing::NativeCpuDamage {
            from_revision: 99,
            rect: appkit::backing::Rect {
                x: 16,
                y: 16,
                width: 1,
                height: 1,
            },
        }),
    });
    assert_eq!(stage_native_scene_source(&mut state, &layer), Ok(true));
    let accepted_third = Arc::clone(
        &state
            .scene_layers
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .snapshot,
    );
    assert_eq!(
        read_scene_pixel(gpu, accepted_third.image.image, 32, 32, 16, 16),
        [0, 64, 128, 255]
    );
    // Correct next-generation damage but an unsuccessful GPU wait cannot
    // expose new scene pixels or keep using the partially modified base.
    changed[offset..offset + 4].copy_from_slice(&[0, 0, 255, 255]);
    let native = layer.native.as_mut().unwrap();
    native.revision = 4;
    native.bytes = Arc::new(changed);
    native.cpu_capture = Some(appkit::backing::NativeCpuCapture {
        revision: 104,
        damage: Some(appkit::backing::NativeCpuDamage {
            from_revision: 103,
            rect: appkit::backing::Rect {
                x: 16,
                y: 16,
                width: 1,
                height: 1,
            },
        }),
    });
    gpu.inject_immediate_wait_failure_for_test();
    let err = stage_native_scene_source(&mut state, &layer).unwrap_err();
    assert!(err.contains("injected immediate fence wait error"));
    assert!(!state.native_cpu_bases.iter().any(|base| base.id == id));
    assert_eq!(
        read_scene_pixel(gpu, accepted_third.image.image, 32, 32, 16, 16),
        [0, 64, 128, 255]
    );
    gpu.wait_first_quarantined_for_test().unwrap();
    gpu.one_shot(|_, _| {}).unwrap();
    assert_eq!(stage_native_scene_source(&mut state, &layer), Ok(true));
    let recovered = &state
        .scene_layers
        .iter()
        .find(|c| c.id == id)
        .unwrap()
        .snapshot;
    assert_eq!(
        read_scene_pixel(gpu, recovered.image.image, 32, 32, 16, 16),
        [255, 0, 0, 255]
    );
    drop(accepted_before);
    drop(accepted_after);
    drop(accepted_third);
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(target_os = "linux")]
fn extra_native_window_crop_invalidates_damage_before_mixed_dma_gpu_upload() {
    if isolate_vulkan_graphical_test(
        "extra_native_window_crop_invalidates_damage_before_mixed_dma_gpu_upload",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let picture = real_gbm_picture_with_pixels(30, 16, |_, _| [0, 0, 0, 0]);
    let mut pixels = [200u8, 0, 0, 255].repeat(30 * 16);
    let mut placement = ScenePlacement {
        opacity_groups: Vec::new(),
        id: (1usize << (usize::BITS - 1)) | 0xabce,
        frame: DirectFrame::from_frame(&appkit::view::Frame {
            x: -5,
            y: 10,
            width: 30,
            height: 16,
            ..whole
        }),
        opacity: 1.,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            bytes: Arc::new(pixels.clone()),
            revision: 7,
            cpu_capture: Some(appkit::backing::NativeCpuCapture {
                revision: 77,
                damage: None,
            }),
            dmabuf: Some(Arc::clone(&picture)),
            dma_sample: None,
        }),
    };
    let first = crop_native_to_window(placement.clone()).unwrap();
    assert_eq!((first.frame.x, first.frame.width), (0, 25));
    assert_eq!(stage_native_scene_source(&mut state, &first), Ok(true));
    let old = Arc::clone(
        &state
            .scene_layers
            .iter()
            .find(|item| item.id == first.id)
            .unwrap()
            .snapshot,
    );
    assert_eq!(
        read_scene_pixel(gpu, old.image.image, 25, 16, 1, 4),
        [0, 0, 200, 255]
    );
    // Original source x=6 becomes cropped-source x=1. x=6 would be *inside*
    // the new bounds too, so retaining its old hint would silently repaint
    // a valid yet incorrect texel.
    let changed_index = (4 * 30 + 6) * 4;
    pixels[changed_index..changed_index + 4].copy_from_slice(&[0, 255, 0, 255]);
    let native = placement.native.as_mut().unwrap();
    native.bytes = Arc::new(pixels);
    native.revision = 8;
    native.cpu_capture = Some(appkit::backing::NativeCpuCapture {
        revision: 78,
        damage: Some(appkit::backing::NativeCpuDamage {
            from_revision: 77,
            rect: appkit::backing::Rect {
                x: 6,
                y: 4,
                width: 1,
                height: 1,
            },
        }),
    });
    let next = crop_native_to_window(placement).unwrap();
    assert_eq!((next.frame.x, next.frame.width), (0, 25));
    assert!(
        next.native
            .as_ref()
            .unwrap()
            .cpu_capture
            .unwrap()
            .damage
            .is_none(),
        "an additional worker crop cannot inherit uncropped damage coordinates"
    );
    assert_eq!(stage_native_scene_source(&mut state, &next), Ok(true));
    let updated = &state
        .scene_layers
        .iter()
        .find(|item| item.id == next.id)
        .unwrap()
        .snapshot;
    assert_eq!(
        read_scene_pixel(gpu, updated.image.image, 25, 16, 1, 4),
        [0, 255, 0, 255]
    );
    assert_eq!(
        read_scene_pixel(gpu, updated.image.image, 25, 16, 6, 4),
        [0, 0, 200, 255]
    );
    assert_eq!(
        read_scene_pixel(gpu, old.image.image, 25, 16, 1, 4),
        [0, 0, 200, 255]
    );
    drop(old);
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// Temporary failure importing one GPU-only producer must not downgrade the
/// entire window: a later, different producer must retain its exact texels.
#[test]
#[cfg(target_os = "linux")]
fn transient_native_dma_import_retries_new_gpu_only_producer_without_window_blacklist() {
    if isolate_vulkan_graphical_test(
        "transient_native_dma_import_retries_new_gpu_only_producer_without_window_blacklist",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let first = real_gbm_picture_with_pixels(32, 32, |_, _| [0, 0, 255, 255]);
    let second = real_gbm_picture_with_pixels(32, 32, |_, _| [40, 60, 80, 255]);
    let id = (1usize << (usize::BITS - 1)) | 0xdab1;
    let mut source = ScenePlacement {
        opacity_groups: Vec::new(),
        id,
        frame: DirectFrame::from_frame(&appkit::view::Frame {
            x: 8,
            y: 9,
            width: 32,
            height: 32,
            ..whole
        }),
        opacity: 1.,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            bytes: Arc::new(Vec::new()),
            revision: 17,
            cpu_capture: None,
            dmabuf: Some(Arc::clone(&first)),
            dma_sample: None,
        }),
    };
    crate::overlay::fail_next_dma_import_for_test();
    // The first linear producer may recover by uploading exact CPU pixels;
    // the failed GPU import must nevertheless be limited to this attempt.
    assert_eq!(stage_native_scene_source(&mut state, &source), Ok(true));
    assert_ne!(
        state
            .scene_layers
            .iter()
            .find(|layer| layer.id == id)
            .unwrap()
            .native_revision,
        Some(17),
        "injected failure must exercise CPU fallback"
    );
    assert!(
        !appkit::backing::dmabuf_import_refused(window),
        "a temporary per-buffer importer failure poisoned the entire window"
    );
    let native = source.native.as_mut().unwrap();
    native.revision = 18;
    native.dmabuf = Some(Arc::clone(&second));
    assert_eq!(stage_native_scene_source(&mut state, &source), Ok(true));
    let accepted = state
        .scene_layers
        .iter()
        .find(|layer| layer.id == id && layer.ready)
        .expect("next GPU-only DMA producer was not admitted");
    assert_eq!(accepted.native_revision, Some(18));
    assert_eq!(
        read_scene_pixel(gpu, accepted.snapshot.image.image, 32, 32, 4, 4),
        [80, 60, 40, 255]
    );
    assert!(!appkit::backing::dmabuf_import_refused(window));
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// Immediate accumulation and snapshot copies must never destroy GPU-owned
/// images after a successful queue submit with an uncertain fence wait.
#[test]
#[cfg(not(target_os = "macos"))]
fn immediate_composite_clear_and_scene_snapshot_quarantine_gpu_image_charges() {
    if isolate_vulkan_graphical_test(
        "immediate_composite_clear_and_scene_snapshot_quarantine_gpu_image_charges",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, _whole)) = v10_fifo_fixture() else {
        return;
    };
    let account = GpuCharge::reserve(window as usize, 1).unwrap();
    let ledger = WINDOW_GPU_BUDGETS
        .lock()
        .unwrap()
        .get(&(window as usize))
        .and_then(std::sync::Weak::upgrade)
        .unwrap();
    let starting = ledger.held.load(Ordering::Acquire);

    gpu.inject_immediate_wait_failure_for_test();
    let err = state.composite().unwrap_err();
    assert!(err.contains("injected immediate fence wait error"));
    assert!(state.composite.is_none());
    assert!(
        ledger.held.load(Ordering::Acquire) > starting,
        "failed accumulator clear destroyed an in-flight Vulkan image"
    );
    gpu.wait_first_quarantined_for_test().unwrap();
    gpu.one_shot(|_, _| {}).unwrap();
    assert_eq!(
        ledger.held.load(Ordering::Acquire),
        starting,
        "completed accumulator clear retained an obsolete image/charge"
    );

    let composite = state.composite().unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, composite, [0.0, 1.0, 0.0, 1.0]);
    })
    .unwrap();
    state.layers.push(7);
    let before = ledger.held.load(Ordering::Acquire);
    let accepted = Arc::clone(state.composite.as_ref().unwrap());
    gpu.inject_immediate_wait_failure_for_test();
    let err = state.capture_single_scene_from_composite().unwrap_err();
    assert!(err.contains("injected immediate fence wait error"));
    assert!(Arc::ptr_eq(&accepted, state.composite.as_ref().unwrap()));
    assert!(state.scene_layers.is_empty());
    assert!(
        ledger.held.load(Ordering::Acquire) > before,
        "failed snapshot destroyed its in-flight destination"
    );
    gpu.wait_first_quarantined_for_test().unwrap();
    gpu.one_shot(|_, _| {}).unwrap();
    assert_eq!(ledger.held.load(Ordering::Acquire), before);
    state.capture_single_scene_from_composite().unwrap();
    let copied = state.scene_layers.last().unwrap();
    assert_eq!(
        read_scene_pixel(gpu, copied.snapshot.image.image, 160, 120, 3, 4),
        [0, 255, 0, 255],
        "confirmed snapshot lost its source texels"
    );
    drop(accepted);
    drop(state);
    drop(account);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn immediate_resize_copy_retains_old_and_new_accumulators_after_failed_wait() {
    if isolate_vulkan_graphical_test(
        "immediate_resize_copy_retains_old_and_new_accumulators_after_failed_wait",
    ) {
        return;
    }
    let Some((window, gpu, mut state, metal, _whole)) = v10_fifo_fixture() else {
        return;
    };
    state.composite().unwrap();
    let original = Arc::clone(state.composite.as_ref().unwrap());
    let account = GpuCharge::reserve(window as usize, 1).unwrap();
    let ledger = WINDOW_GPU_BUDGETS
        .lock()
        .unwrap()
        .get(&(window as usize))
        .and_then(std::sync::Weak::upgrade)
        .unwrap();
    let baseline = ledger.held.load(Ordering::Acquire);
    assert!(sdl3::video::set_window_size(window, 208, 144));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        sdl3::pump();
        let caps = unsafe {
            crate::vulkan::surface_api()
                .unwrap()
                .get_physical_device_surface_capabilities(gpu.physical, state.surface)
        }
        .unwrap();
        if caps.current_extent.width == u32::MAX
            || (caps.current_extent.width, caps.current_extent.height) == (208, 144)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "surface resize not reflected in Vulkan"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    gpu.inject_immediate_wait_failure_for_test();
    let error = state
        .rebuild(vk::Extent2D {
            width: 208,
            height: 144,
        })
        .unwrap_err();
    assert!(
        error.contains("injected immediate fence wait error"),
        "{error}"
    );
    assert!(
        Arc::ptr_eq(&original, state.composite.as_ref().unwrap()),
        "rejected resize replaced the accepted old source"
    );
    assert!(
        ledger.held.load(Ordering::Acquire) > baseline,
        "failed resize destroyed in-flight destination or its charge"
    );
    gpu.wait_first_quarantined_for_test().unwrap();
    gpu.one_shot(|_, _| {}).unwrap();
    assert_eq!(
        ledger.held.load(Ordering::Acquire),
        baseline,
        "retired resize left its failed destination allocated"
    );
    drop(original);
    drop(state);
    drop(account);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// Retryable GPU-only import failures may not enter refused_revisions: the
/// *same* immutable producer must be accepted after the temporary error ends.
#[test]
#[cfg(target_os = "linux")]
fn compositor_worker_retries_same_dma_revision_after_temporary_import_failure() {
    if isolate_vulkan_graphical_test(
        "compositor_worker_retries_same_dma_revision_after_temporary_import_failure",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    let retirement = AwaitTestWindowRetirements;
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let window = sdl3::video::create_window(c"DMA transient worker", 160, 120, 0).unwrap();
    sdl3::video::show_window(window);
    sdl3::video::presentation_window(window).unwrap();
    let whole = test_full_frame(window, 160, 120);
    let metal = test_render_target(gpu, 160, 120).unwrap();
    assert_eq!(present(gpu, window as usize, &whole, &metal, &[]), Ok(true));
    let entry = SWAPCHAINS
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get(&(window as usize))
        .unwrap()
        .clone();
    let picture = real_gbm_picture_with_pixels(32, 32, |_, _| [15, 45, 175, 255]);
    let native_id = (1usize << (usize::BITS - 1)) | 0xda44;
    let native = ScenePlacement {
        opacity_groups: Vec::new(),
        id: native_id,
        frame: DirectFrame::from_frame(&appkit::view::Frame {
            x: 26,
            y: 19,
            width: 32,
            height: 32,
            ..whole
        }),
        opacity: 1.,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            bytes: Arc::new(Vec::new()),
            revision: 63,
            cpu_capture: None,
            dmabuf: Some(Arc::clone(&picture)),
            dma_sample: None,
        }),
    };
    let base = test_opaque_metal_placement(window as usize, DirectFrame::from_frame(&whole));
    let failures = TEST_NATIVE_RETRYABLE_FAILURES.load(Ordering::Acquire);
    TEST_BLOCK_LINEAR_DMA_FALLBACK.store(true, Ordering::Release);
    crate::overlay::fail_next_dma_import_for_test();
    *entry.pending_scene.lock().unwrap() = Some(vec![base, native]);
    ensure_layer_compositor_worker(window as usize, &entry);
    wake_compositor_worker(&entry);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = entry.state.lock().unwrap();
        let accepted = state.scene_layers.iter().any(|layer| {
            layer.id == native_id
                && layer.ready
                && layer.native_revision == Some(63)
                && layer.native_published_revision == Some(63)
        });
        if accepted
            && !state.composite_dirty
            && !state.scene_restore_pending
            && entry.pending_scene.lock().unwrap().is_none()
        {
            assert!(
                TEST_NATIVE_RETRYABLE_FAILURES.load(Ordering::Acquire) > failures,
                "worker did not observe retryable DMA rejection"
            );
            assert_eq!(
                read_scene_pixel(
                    gpu,
                    state.composite.as_ref().unwrap().image,
                    160,
                    120,
                    29,
                    23
                ),
                [175, 45, 15, 255]
            );
            assert!(state.wsi_publishes > 0);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "worker never retried unchanged rejected DMA source: pending={} restore={} accepted={}",
            entry.pending_scene.lock().unwrap().is_some(),
            state.scene_restore_pending,
            accepted
        );
        drop(state);
        std::thread::sleep(Duration::from_millis(10));
    }
    TEST_BLOCK_LINEAR_DMA_FALLBACK.store(false, Ordering::Release);
    forget_window(window);
    drop(entry);
    drop(retirement);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// A cached unsupported GPU import must still recompute the exact linear
/// fallback on subsequent moves/opacity updates of the same DMA revision.
#[test]
#[cfg(target_os = "linux")]
fn cached_unsupported_dma_uses_cpu_fallback_on_metadata_only_scene_change() {
    if isolate_vulkan_graphical_test(
        "cached_unsupported_dma_uses_cpu_fallback_on_metadata_only_scene_change",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let picture = real_gbm_picture_with_pixels(22, 18, |_, _| [9, 81, 202, 255]);
    assert_eq!(picture.modifier, 0, "CPU fallback fixture must be linear");
    let id = (1usize << (usize::BITS - 1)) | 0xda45;
    let mut placement = ScenePlacement {
        opacity_groups: Vec::new(),
        id,
        frame: DirectFrame::from_frame(&appkit::view::Frame {
            x: 12,
            y: 11,
            width: 22,
            height: 18,
            ..whole
        }),
        opacity: 1.,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            bytes: Arc::new(Vec::new()),
            revision: 89,
            cpu_capture: None,
            dmabuf: Some(Arc::clone(&picture)),
            dma_sample: None,
        }),
    };
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, metal.image, [0., 1., 0., 1.]);
    })
    .unwrap();
    let base = test_opaque_metal_placement(0xda46, DirectFrame::from_frame(&whole));
    state.layers.push(base.id);
    state.synchronize_scene(window as usize, &[base.clone(), placement.clone()]);
    assert_eq!(
        state.stage_composite_layer(base.id, &whole, &metal, None, &mut SourceOwner(None), false),
        Ok(Some(true))
    );
    state.note_scene_frame(base.id, &whole);
    crate::overlay::fail_next_unsupported_dma_import_for_test();
    assert_eq!(stage_native_scene_source(&mut state, &placement), Ok(true));
    assert_eq!(state.unsupported_native_dma.len(), 1);
    let original = state.scene_layers.iter().find(|l| l.id == id).unwrap();
    assert_eq!(
        read_scene_pixel(gpu, original.snapshot.image.image, 22, 18, 3, 4),
        [9, 81, 202, 255]
    );
    state.synchronize_scene(window as usize, &[base.clone(), placement.clone()]);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            160,
            120,
            15,
            15
        ),
        [202, 81, 9, 255]
    );
    // Exact same producer and revision, only compositor geometry and opacity.
    placement.frame.x = 57;
    placement.frame.y = 33;
    placement.opacity = 0.5;
    assert_eq!(stage_native_scene_source(&mut state, &placement), Ok(true));
    let updated = state.scene_layers.iter().find(|l| l.id == id).unwrap();
    assert_eq!(updated.native_published_revision, None);
    assert_eq!(
        read_scene_pixel(gpu, updated.snapshot.image.image, 22, 18, 3, 4),
        [9, 81, 202, 255]
    );
    assert_eq!(
        state.unsupported_native_dma.len(),
        1,
        "same producer should not retry unsupported GPU import"
    );
    state.synchronize_scene(window as usize, &[base, placement]);
    assert_eq!(state.redisplay_scene(window as usize), Ok(true));
    state.idle();
    assert_eq!(
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            160,
            120,
            15,
            15
        ),
        [0, 255, 0, 255],
        "moved native view remained visible at the previous location"
    );
    let moved = read_scene_pixel(
        gpu,
        state.composite.as_ref().unwrap().image,
        160,
        120,
        60,
        38,
    );
    assert_ne!(
        moved,
        [0, 255, 0, 255],
        "CPU fallback was skipped when only native geometry and opacity changed"
    );
    assert_eq!(moved[3], 255);
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// Real Vulkan device reports support for a FourCC but not an arbitrary
/// non-linear modifier: reject once, never retry as an allocation failure.
#[test]
#[cfg(target_os = "linux")]
fn unsupported_device_modifier_is_hard_refusal_not_endless_dma_retry() {
    if isolate_vulkan_graphical_test(
        "unsupported_device_modifier_is_hard_refusal_not_endless_dma_retry",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let valid = real_gbm_picture_with_pixels(32, 32, |_, _| [0, 0, 220, 255]);
    let mut invalid_modifier = u64::MAX;
    let importer = crate::overlay::importer().expect("Vulkan DMA importer");
    assert!(!importer.formats.contains(&(valid.fourcc, invalid_modifier)));
    if invalid_modifier == valid.modifier {
        invalid_modifier -= 1;
    }
    let unsupported = Arc::new(appkit::backing::Dmabuf {
        width: valid.width,
        height: valid.height,
        fourcc: valid.fourcc,
        modifier: invalid_modifier,
        planes: valid
            .planes
            .iter()
            .map(|p| appkit::backing::DmabufPlane {
                fd: p.fd.try_clone().unwrap(),
                offset: p.offset,
                stride: p.stride,
            })
            .collect(),
    });
    let native = ScenePlacement {
        opacity_groups: Vec::new(),
        id: (1usize << (usize::BITS - 1)) | 0xdaf0,
        frame: DirectFrame::from_frame(&appkit::view::Frame {
            x: 8,
            y: 8,
            width: 32,
            height: 32,
            ..whole
        }),
        opacity: 1.,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            bytes: Arc::new(Vec::new()),
            revision: 92,
            cpu_capture: None,
            dmabuf: Some(Arc::clone(&unsupported)),
            dma_sample: None,
        }),
    };
    let before = crate::overlay::import_attempts_for_test();
    assert_eq!(
        stage_native_scene_source(&mut state, &native),
        Err("native-dmabuf-import-refused".into())
    );
    assert_eq!(state.unsupported_native_dma.len(), 1);
    assert_eq!(crate::overlay::import_attempts_for_test(), before + 1);
    for _ in 0..16 {
        assert_eq!(
            stage_native_scene_source(&mut state, &native),
            Err("native-dmabuf-import-refused".into())
        );
    }
    assert_eq!(
        crate::overlay::import_attempts_for_test(),
        before + 1,
        "unsupported non-linear modifier kept retrying GPU allocations"
    );
    assert!(
        !appkit::backing::dmabuf_import_refused(window),
        "one unsupported modifier cannot poison the window"
    );
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// Structural DMA producer errors are hard refusals even when the GPU can
/// import the exact FourCC and modifier from a correctly formed producer.
#[test]
#[cfg(target_os = "linux")]
fn invalid_supported_dma_plane_layout_is_not_retried() {
    if isolate_vulkan_graphical_test("invalid_supported_dma_plane_layout_is_not_retried") {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_GBM_DMA").is_none() {
        return;
    }
    let Some((window, gpu, mut state, metal, whole)) = v10_fifo_fixture() else {
        return;
    };
    let picture = real_gbm_picture_with_pixels(32, 32, |_, _| [1, 2, 3, 255]);
    let malformed = Arc::new(appkit::backing::Dmabuf {
        width: picture.width,
        height: picture.height,
        fourcc: picture.fourcc,
        modifier: picture.modifier,
        planes: picture
            .planes
            .iter()
            .map(|p| appkit::backing::DmabufPlane {
                fd: p.fd.try_clone().unwrap(),
                offset: p.offset,
                stride: 0,
            })
            .collect(),
    });
    let native = ScenePlacement {
        opacity_groups: Vec::new(),
        id: (1usize << (usize::BITS - 1)) | 0xda91,
        frame: DirectFrame::from_frame(&appkit::view::Frame {
            x: 8,
            y: 8,
            width: 32,
            height: 32,
            ..whole
        }),
        opacity: 1.,
        opaque: false,
        clip: None,
        rounded: vec![],
        occluded: Some(vec![]),
        native: Some(scene::NativeCpuSceneSource {
            bytes: Arc::new(Vec::new()),
            revision: 18,
            cpu_capture: None,
            dmabuf: Some(Arc::clone(&malformed)),
            dma_sample: None,
        }),
    };
    let before = crate::overlay::import_attempts_for_test();
    assert_eq!(
        stage_native_scene_source(&mut state, &native),
        Err("native-dmabuf-import-refused".into())
    );
    assert_eq!(state.unsupported_native_dma.len(), 1);
    assert_eq!(crate::overlay::import_attempts_for_test(), before + 1);
    for _ in 0..16 {
        assert_eq!(
            stage_native_scene_source(&mut state, &native),
            Err("native-dmabuf-import-refused".into())
        );
    }
    assert_eq!(crate::overlay::import_attempts_for_test(), before + 1);
    // A different valid producer still reaches the Vulkan importer.
    let other = ScenePlacement {
        opacity_groups: Vec::new(),
        id: native.id + 1,
        native: Some(scene::NativeCpuSceneSource {
            bytes: Arc::new(Vec::new()),
            revision: 19,
            cpu_capture: None,
            dmabuf: Some(Arc::clone(&picture)),
            dma_sample: None,
        }),
        ..native
    };
    assert_eq!(stage_native_scene_source(&mut state, &other), Ok(true));
    assert!(crate::overlay::import_attempts_for_test() > before + 1);
    drop(state);
    gpu.destroy_image(metal);
    sdl3::video::destroy_window(window);
}

/// A non-NSView CALayer descendant must paint above the retained Metal
/// drawable and move without any subsequent guest Metal frame submission.
#[test]
#[cfg(not(target_os = "macos"))]
fn static_native_foreground_published_before_swapchain_init_replays_on_first_present() {
    if isolate_vulkan_graphical_test(
        "static_native_foreground_published_before_swapchain_init_replays_on_first_present",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let _retire_guard = AwaitTestWindowRetirements;
    crate::bootstrap();
    let pool = objc::arc::pool_push();
    use foundation::geometry::{NSPoint, NSRect, NSSize};
    use objc::object::Id;
    let rect = |x, y, width, height| NSRect {
        origin: NSPoint { x, y },
        size: NSSize { width, height },
    };
    let window = objc::object::create_instance(
        objc::class::lookup(c"NSWindow").unwrap(),
        appkit::window::INSTANCE_SIZE,
    );
    let initialize: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect, usize, usize, u8) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    assert_eq!(
        unsafe {
            initialize(
                window,
                objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
                rect(0., 0., 120., 90.),
                0,
                0,
                0,
            )
        },
        window
    );
    let make_view = |frame: NSRect| {
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let init: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { init(view, objc::sel::get("initWithFrame:"), frame) }
    };
    let make_layer = |class: &str| {
        let cls = objc::class::lookup(std::ffi::CString::new(class).unwrap().as_c_str()).unwrap();
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { make(cls as *const _ as Id, objc::sel::get("layer")) }
    };
    let content = make_view(rect(0., 0., 120., 90.));
    let metal_view = make_view(rect(0., 0., 120., 90.));
    let native = make_view(rect(13., 12., 24., 20.));
    let metal_layer = make_layer("CAMetalLayer");
    let native_layer = make_layer("CALayer");
    let set: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let red = unsafe { appkit::cgcontext::CGColorCreateGenericRGB(1., 0., 0., 1.) };
    unsafe {
        set(window, objc::sel::get("setContentView:"), content);
        set(metal_view, objc::sel::get("setLayer:"), metal_layer);
        set(native, objc::sel::get("setLayer:"), native_layer);
        set(
            native_layer,
            objc::sel::get("setBackgroundColor:"),
            red as Id,
        );
        set(content, objc::sel::get("addSubview:"), metal_view);
        set(content, objc::sel::get("addSubview:"), native);
    }
    let sdl = unsafe { appkit::window::sdl_window(window) };
    appkit::event::pump(std::ptr::null_mut());
    let published = appkit::view::metal_scene(sdl).unwrap();
    let native_item = published
        .iter()
        .find(|item| item.native_owner == native as usize)
        .expect("static native foreground must be published before Vulkan init");
    assert!(native_item.native_pixels.is_some());
    assert!(
        SWAPCHAINS
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|registry| !registry.contains_key(&(sdl as usize)))
    );
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    let source = test_render_target(gpu, 120, 90).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, source.image, [0., 1., 0., 1.]);
    })
    .unwrap();
    let physical = test_full_frame(sdl, 120, 90);
    // The initializer runs independently. No setters or calls to publish
    // the foreground follow this first guest drawable.
    let _ = present(gpu, metal_layer as usize, &physical, &source, &[]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let entry = loop {
        appkit::event::pump(std::ptr::null_mut());
        if let Some(entry) = SWAPCHAINS
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|all| all.get(&(sdl as usize)).cloned())
        {
            break entry;
        }
        assert!(Instant::now() < deadline, "swapchain never initialized");
        std::thread::sleep(Duration::from_millis(2));
    };
    // The owner thread consumes the initializer's targeted replay request.
    for _ in 0..4 {
        appkit::event::pump(std::ptr::null_mut());
    }
    let _ = present(gpu, metal_layer as usize, &physical, &source, &[]);
    ensure_layer_compositor_worker(sdl as usize, &entry);
    wake_compositor_worker(&entry);
    loop {
        appkit::event::pump(std::ptr::null_mut());
        {
            let mut state = entry.state.lock().unwrap();
            let ready = state.scene_layers.iter().any(|layer| {
                layer.id == native_item.layer
                    && layer.ready
                    && layer.native_published_revision.is_some()
            });
            if ready
                && entry.pending_scene.lock().unwrap().is_none()
                && state.composite.is_some()
                && !state.scene_restore_pending
            {
                state.idle();
                let image = state.composite.as_ref().unwrap().image;
                // Readback returns raw texel bytes; the negotiated target
                // may store red first (RGBA) or blue first (BGRA).
                let red_pixel = match state.composite.as_ref().unwrap().format {
                    vk::Format::R8G8B8A8_UNORM | vk::Format::R8G8B8A8_SRGB => [255, 0, 0, 255],
                    vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB => [0, 0, 255, 255],
                    other => panic!("unexpected compositor format {other:?}"),
                };
                let red = read_scene_pixel(gpu, image, 120, 90, 20, 65);
                let green = read_scene_pixel(gpu, image, 120, 90, 70, 30);
                assert_eq!(
                    red, red_pixel,
                    "pre-initialization static native view remained missing"
                );
                assert_eq!(
                    green,
                    [0, 255, 0, 255],
                    "native replay displaced the underlying Metal drawable"
                );
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "native foreground never staged from pre-swapchain publication"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    TEST_WINDOW_RETIRED.store(0, Ordering::Release);
    forget_window(sdl);
    drop(entry);
    sdl3::video::destroy_window(sdl);
    let teardown_deadline = Instant::now() + Duration::from_secs(5);
    while TEST_WINDOW_RETIRED.load(Ordering::Acquire) != sdl as usize {
        assert!(
            Instant::now() < teardown_deadline,
            "retired WSI workers must stop"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    sdl3::video::vulkan::service_surface_requests();
    gpu.destroy_image(source);
    objc::arc::pool_pop(pool);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn metal_root_ca_only_child_recomposes_actual_vulkan_pixels_without_new_drawable() {
    if isolate_vulkan_graphical_test(
        "metal_root_ca_only_child_recomposes_actual_vulkan_pixels_without_new_drawable",
    ) {
        return;
    }
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    appkit::bootstrap();
    use foundation::geometry::{NSPoint, NSRect, NSSize};
    use objc::object::Id;
    let rect = |x, y, width, height| NSRect {
        origin: NSPoint { x, y },
        size: NSSize { width, height },
    };
    let window = objc::object::create_instance(
        objc::class::lookup(c"NSWindow").unwrap(),
        appkit::window::INSTANCE_SIZE,
    );
    let initialize: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect, usize, usize, u8) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    assert_eq!(
        unsafe {
            initialize(
                window,
                objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
                rect(0., 0., 120., 90.),
                0,
                0,
                0,
            )
        },
        window
    );
    let sdl = unsafe { appkit::window::sdl_window(window) };
    assert!(!sdl.is_null());
    let make_view = || {
        let v = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let init: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { init(v, objc::sel::get("initWithFrame:"), rect(0., 0., 120., 90.)) }
    };
    let content = make_view();
    let metal_view = make_view();
    let make_layer = |class: &str| {
        let c = objc::class::lookup(std::ffi::CString::new(class).unwrap().as_c_str()).unwrap();
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { make(c as *const _ as Id, objc::sel::get("layer")) }
    };
    let metal_layer = make_layer("CAMetalLayer");
    let child = make_layer("CALayer");
    let set: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let set_frame: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let red = unsafe { appkit::cgcontext::CGColorCreateGenericRGB(1., 0., 0., 1.) };
    unsafe {
        set(window, objc::sel::get("setContentView:"), content);
        set(metal_view, objc::sel::get("setLayer:"), metal_layer);
        set(content, objc::sel::get("addSubview:"), metal_view);
        set_frame(child, objc::sel::get("setFrame:"), rect(13., 12., 24., 20.));
        set(child, objc::sel::get("setBackgroundColor:"), red as Id);
        set(metal_layer, objc::sel::get("addSublayer:"), child);
    }
    appkit::event::pump(std::ptr::null_mut());
    let scene_before = appkit::view::metal_scene(sdl).unwrap();
    assert_eq!(scene_before[0].layer, metal_layer as usize);
    let overlay_before = scene_before
        .iter()
        .find(|part| part.native_pixels.is_some())
        .expect("Metal root CA child must be separately composited");
    let old_pixels = overlay_before.native_pixels.as_ref().unwrap();
    let first_red = old_pixels
        .as_chunks::<4>()
        .0
        .iter()
        .position(|p| p[3] != 0)
        .unwrap();
    let old_point = (
        overlay_before.frame.x as u32 + (first_red % overlay_before.frame.width as usize) as u32,
        overlay_before.frame.y as u32 + (first_red / overlay_before.frame.width as usize) as u32,
    );
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    sdl3::video::presentation_window(sdl).unwrap();
    let instance = crate::vulkan::instance_handle().unwrap();
    let surface =
        sdl3::video::vulkan::create_surface(sdl, instance, crate::vulkan::library()).unwrap();
    let mut state = Swapchain::create(
        gpu,
        sdl,
        vk::Extent2D {
            width: 120,
            height: 90,
        },
        surface,
        instance,
    )
    .unwrap();
    let source = test_render_target(gpu, 120, 90).unwrap();
    gpu.one_shot(|device, cb| unsafe {
        cmd_test_clear_color(device, cb, source.image, [0., 1., 0., 1.]);
    })
    .unwrap();
    let physical = test_full_frame(sdl, 120, 90);
    let make_scene = |items: Vec<appkit::view::MetalSceneLayer>| -> Vec<ScenePlacement> {
        items
            .into_iter()
            .map(|item| ScenePlacement {
                opacity_groups: item.opacity_groups,
                id: item.layer,
                frame: DirectFrame::from_frame(&item.frame),
                opacity: item.opacity,
                opaque: item.opaque,
                clip: item.clip.map(|clip| DirectFrame::from_frame(&clip)),
                rounded: item
                    .rounded
                    .into_iter()
                    .map(|r| scene::RoundedClip {
                        frame: DirectFrame::from_frame(&r.frame),
                        radius: r.radius,
                    })
                    .collect(),
                occluded: Some(vec![]),
                native: native_scene_source(
                    item.native_pixels,
                    item.native_dmabuf,
                    item.native_revision,
                    item.frame.width,
                    item.frame.height,
                    item.native_cpu_capture,
                    item.native_dma_sample,
                ),
            })
            .collect()
    };
    let initial = make_scene(scene_before.clone());
    state.layers.push(metal_layer as usize);
    state.synchronize_scene(sdl as usize, &initial);
    assert_eq!(
        state.stage_composite_source(
            metal_layer as usize,
            &physical,
            PresentSource::from(&source),
            None,
            &mut SourceOwner(None),
            false,
            true
        ),
        Ok(Some(true))
    );
    for native in initial.iter().filter(|p| p.native.is_some()) {
        assert_eq!(stage_native_scene_source(&mut state, native), Ok(true));
    }
    state.synchronize_scene(sdl as usize, &initial);
    assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
    state.idle();
    let accumulated = state.composite.as_ref().unwrap().image;
    assert_eq!(
        read_scene_pixel(gpu, accumulated, 120, 90, old_point.0, old_point.1),
        [255, 0, 0, 255]
    );
    // The original drawable stays unchanged. Changing only root CA opacity
    // must group its old Metal pixels with the native child before fading.
    let change_root_opacity = |state: &mut Swapchain, alpha: f32| {
        let set_alpha: unsafe extern "C" fn(Id, objc::sel::Sel, f32) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { set_alpha(metal_layer, objc::sel::get("setOpacity:"), alpha) };
        appkit::event::pump(std::ptr::null_mut());
        let proposal = make_scene(appkit::view::metal_scene(sdl).unwrap());
        if alpha < 1.0 {
            assert!(proposal.iter().all(|source| source.opacity_groups
                == [appkit::view::OpacityGroup {
                    id: metal_layer as usize,
                    opacity: alpha,
                }]));
        }
        for native in proposal.iter().filter(|p| p.native.is_some()) {
            assert_eq!(stage_native_scene_source(state, native), Ok(true));
        }
        state.synchronize_scene(sdl as usize, &proposal);
        assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
        state.idle();
    };
    change_root_opacity(&mut state, 0.5);
    let half = read_scene_pixel(
        gpu,
        state.composite.as_ref().unwrap().image,
        120,
        90,
        old_point.0,
        old_point.1,
    );
    assert!(
        half.iter()
            .zip([128u8, 0, 0, 128])
            .all(|(a, b)| a.abs_diff(b) <= 1),
        "root CA opacity leaked retained drawable through opaque native child: {half:?}"
    );
    change_root_opacity(&mut state, 0.25);
    let quarter = read_scene_pixel(
        gpu,
        state.composite.as_ref().unwrap().image,
        120,
        90,
        old_point.0,
        old_point.1,
    );
    assert!(
        quarter
            .iter()
            .zip([64u8, 0, 0, 64])
            .all(|(a, b)| a.abs_diff(b) <= 1),
        "metadata-only opacity did not update completed retained group: {quarter:?}"
    );
    change_root_opacity(&mut state, 1.0);
    assert_eq!(
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            120,
            90,
            old_point.0,
            old_point.1
        ),
        [255, 0, 0, 255]
    );

    // A partially transparent NSView may own one Metal drawable plus
    // ordinary CA descendants even with no subviews. Its alphaValue must
    // still fade the *completed* group once, including exact-zero opacity.
    let alpha_of_metal_view: unsafe extern "C" fn(Id, objc::sel::Sel, f64) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    for (alpha, expected) in [
        (0.5, [128u8, 0, 0, 128]),
        (0.0, [0u8, 0, 0, 0]),
        (1.0, [255u8, 0, 0, 255]),
    ] {
        unsafe {
            alpha_of_metal_view(metal_view, objc::sel::get("setAlphaValue:"), alpha);
        }
        appkit::event::pump(std::ptr::null_mut());
        let proposed = make_scene(appkit::view::metal_scene(sdl).unwrap());
        if alpha < 1.0 {
            assert!(proposed.iter().all(|source| source.opacity_groups
                == [appkit::view::OpacityGroup {
                    id: metal_view as usize,
                    opacity: alpha as f32
                }]));
        }
        for native in proposed.iter().filter(|source| source.native.is_some()) {
            assert_eq!(stage_native_scene_source(&mut state, native), Ok(true));
        }
        state.synchronize_scene(sdl as usize, &proposed);
        assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
        state.idle();
        let actual = read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            120,
            90,
            old_point.0,
            old_point.1,
        );
        assert!(
            actual.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1),
            "NSView alpha {alpha} failed over Metal + CALayer with no child NSViews: {actual:?}"
        );
    }

    // Move a CALayer-only overlay after the Metal image was last submitted.
    unsafe {
        set_frame(child, objc::sel::get("setFrame:"), rect(64., 40., 24., 20.));
    }
    appkit::event::pump(std::ptr::null_mut());
    let changed = appkit::view::metal_scene(sdl).unwrap();
    let updated = changed
        .iter()
        .find(|part| part.native_pixels.is_some())
        .unwrap();
    let new_pixels = updated.native_pixels.as_ref().unwrap();
    let new_red = new_pixels
        .as_chunks::<4>()
        .0
        .iter()
        .position(|p| p[3] != 0)
        .unwrap();
    let new_point = (
        updated.frame.x as u32 + (new_red % updated.frame.width as usize) as u32,
        updated.frame.y as u32 + (new_red / updated.frame.width as usize) as u32,
    );
    assert_ne!(old_point, new_point);
    let next = make_scene(changed);
    for native in next.iter().filter(|p| p.native.is_some()) {
        assert_eq!(stage_native_scene_source(&mut state, native), Ok(true));
    }
    state.synchronize_scene(sdl as usize, &next);
    assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
    state.idle();
    let output = state.composite.as_ref().unwrap().image;
    assert_eq!(
        read_scene_pixel(gpu, output, 120, 90, old_point.0, old_point.1),
        [0, 255, 0, 255],
        "old CALayer-only foreground remained after move"
    );
    assert_eq!(
        read_scene_pixel(gpu, output, 120, 90, new_point.0, new_point.1),
        [255, 0, 0, 255],
        "CALayer-only foreground was not composited above Metal"
    );
    // Interleave an NSView-backed blue CALayer between two ordinary CA
    // descendants. All three overlap over a static Metal drawable.
    let blue_view = make_view();
    let b = make_layer("CALayer");
    let c = make_layer("CALayer");
    let blue = unsafe { appkit::cgcontext::CGColorCreateGenericRGB(0., 0., 1., 1.) };
    let green = unsafe { appkit::cgcontext::CGColorCreateGenericRGB(0., 1., 0., 1.) };
    unsafe {
        set(blue_view, objc::sel::get("setLayer:"), b);
        set(metal_view, objc::sel::get("addSubview:"), blue_view);
        set_frame(b, objc::sel::get("setFrame:"), rect(0., 0., 120., 90.));
        set_frame(c, objc::sel::get("setFrame:"), rect(64., 40., 24., 20.));
        set(b, objc::sel::get("setBackgroundColor:"), blue as Id);
        set(c, objc::sel::get("setBackgroundColor:"), green as Id);
        set(metal_layer, objc::sel::get("addSublayer:"), b);
        set(metal_layer, objc::sel::get("addSublayer:"), c);
    }
    let repaint = |state: &mut Swapchain| {
        appkit::event::pump(std::ptr::null_mut());
        let current = make_scene(appkit::view::metal_scene(sdl).unwrap());
        for native in current.iter().filter(|p| p.native.is_some()) {
            assert_eq!(stage_native_scene_source(state, native), Ok(true));
        }
        state.synchronize_scene(sdl as usize, &current);
        assert_eq!(state.redisplay_scene(sdl as usize), Ok(true));
        state.idle();
    };
    repaint(&mut state);
    let sample = |state: &Swapchain| {
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            120,
            90,
            new_point.0,
            new_point.1,
        )
    };
    assert_eq!(
        sample(&state),
        [0, 255, 0, 255],
        "ordinary CALayer above NSView-backed layer was not topmost"
    );
    // A parent NSView's alphaValue isolates its retained Metal drawable,
    // ordinary CPU CALayer, and independently backed child. The foremost
    // opaque CPU layer covers both lower sources before group attenuation.
    let set_view_alpha: unsafe extern "C" fn(Id, objc::sel::Sel, f64) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let hide_child: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe {
        hide_child(c, objc::sel::get("setHidden:"), 1);
        set_view_alpha(metal_view, objc::sel::get("setAlphaValue:"), 0.5);
    }
    repaint(&mut state);
    let alpha_sources = appkit::view::metal_scene(sdl).unwrap();
    assert!(alpha_sources.iter().all(|source| source.opacity_groups
        == [appkit::view::OpacityGroup {
            id: metal_view as usize,
            opacity: 0.5
        }]
        && source.opacity == 1.0));
    let half_view = sample(&state);
    assert!(
        half_view
            .iter()
            .zip([0u8, 0, 128, 128])
            .all(|(a, b)| a.abs_diff(b) <= 1),
        "NSView ancestor alpha leaked opaque underlay through child: {half_view:?}"
    );
    unsafe {
        set_view_alpha(metal_view, objc::sel::get("setAlphaValue:"), 0.25);
    }
    repaint(&mut state);
    let quarter_view = sample(&state);
    assert!(
        quarter_view
            .iter()
            .zip([0u8, 0, 64, 64])
            .all(|(a, b)| a.abs_diff(b) <= 1),
        "NSView metadata-only alpha did not recomposite child: {quarter_view:?}"
    );
    unsafe {
        set_view_alpha(metal_view, objc::sel::get("setAlphaValue:"), 1.0);
        hide_child(c, objc::sel::get("setHidden:"), 0);
    }
    repaint(&mut state);
    assert_eq!(sample(&state), [0, 255, 0, 255]);

    // The same retained frame also contains a mixed CA/NSView group:
    // native red A, NSView-backed opaque blue B, and ordinary green C.
    // Their enclosing alpha must not be applied to each source separately.
    let group_wrap = make_layer("CALayer");
    let set_alpha: unsafe extern "C" fn(Id, objc::sel::Sel, f32) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let hidden: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe {
        set_frame(
            group_wrap,
            objc::sel::get("setFrame:"),
            rect(0., 0., 120., 90.),
        );
        set_alpha(group_wrap, objc::sel::get("setOpacity:"), 0.5);
        set(metal_layer, objc::sel::get("addSublayer:"), group_wrap);
        for member in [child, b, c] {
            set(group_wrap, objc::sel::get("addSublayer:"), member);
        }
    }
    repaint(&mut state);
    let wrapped = appkit::view::metal_scene(sdl).unwrap();
    assert!(wrapped.iter().skip(1).all(|part| part.opacity_groups
        == [appkit::view::OpacityGroup {
            id: group_wrap as usize,
            opacity: 0.5
        }]));
    unsafe { hidden(c, objc::sel::get("setHidden:"), 1) };
    repaint(&mut state);
    let mixed_blue = sample(&state);
    assert!(
        mixed_blue
            .iter()
            .zip([0u8, 127, 128, 255])
            .all(|(a, b)| a.abs_diff(b) <= 1),
        "interleaved NSView B leaked opaque CA child A under group alpha: {mixed_blue:?}"
    );
    unsafe { set_alpha(group_wrap, objc::sel::get("setOpacity:"), 0.25) };
    repaint(&mut state);
    let quarter_blue = sample(&state);
    assert!(
        quarter_blue
            .iter()
            .zip([0u8, 191, 64, 255])
            .all(|(a, b)| a.abs_diff(b) <= 1),
        "metadata-only mixed-group opacity did not repaint: {quarter_blue:?}"
    );
    // masksToBounds is applied before the completed mixed group is faded.
    // A clipped independently backed blue child may not escape the group.
    let set_mask: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe {
        set_frame(
            group_wrap,
            objc::sel::get("setFrame:"),
            rect(0., 0., 66., 90.),
        );
        set_mask(group_wrap, objc::sel::get("setMasksToBounds:"), 1);
    }
    repaint(&mut state);
    assert_eq!(
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            120,
            90,
            75,
            45
        ),
        [0, 255, 0, 255],
        "independent NSView-backed child escaped opacity group's clipping mask"
    );
    let within_clip = read_scene_pixel(
        gpu,
        state.composite.as_ref().unwrap().image,
        120,
        90,
        65,
        45,
    );
    assert!(
        within_clip
            .iter()
            .zip([0u8, 191, 64, 255])
            .all(|(a, b)| a.abs_diff(b) <= 1),
        "opacity group mask clipped valid source pixels: {within_clip:?}"
    );
    unsafe {
        set_mask(group_wrap, objc::sel::get("setMasksToBounds:"), 0);
        set_frame(
            group_wrap,
            objc::sel::get("setFrame:"),
            rect(0., 0., 120., 90.),
        );
    }
    repaint(&mut state);
    assert!(
        sample(&state)
            .iter()
            .zip([0u8, 191, 64, 255])
            .all(|(a, b)| a.abs_diff(b) <= 1)
    );

    // Restore the original explicit interleaving regression below.
    unsafe {
        hidden(c, objc::sel::get("setHidden:"), 0);
        for member in [child, b, c] {
            set(metal_layer, objc::sel::get("addSublayer:"), member);
        }
    }
    repaint(&mut state);
    assert_eq!(sample(&state), [0, 255, 0, 255]);

    let insert: unsafe extern "C" fn(Id, objc::sel::Sel, Id, u32) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe {
        insert(metal_layer, objc::sel::get("insertSublayer:atIndex:"), c, 0);
    }
    repaint(&mut state);
    assert_eq!(
        sample(&state),
        [0, 0, 255, 255],
        "view-backed layer must paint after coalesced ordinary CA segment"
    );
    let hidden: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe {
        hidden(b, objc::sel::get("setHidden:"), 1);
    }
    repaint(&mut state);
    assert_eq!(
        sample(&state),
        [255, 0, 0, 255],
        "hiding the view-backed layer must reveal ordinary CA pixels"
    );
    let remove: unsafe extern "C" fn(Id, objc::sel::Sel) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe {
        remove(child, objc::sel::get("removeFromSuperlayer"));
    }
    repaint(&mut state);
    assert_eq!(
        sample(&state),
        [0, 255, 0, 255],
        "removed foreground source remained in the Vulkan scene"
    );
    // A producer can update the pixels of a child IOSurface in place, without
    // setContents: or another drawable. The AppKit poll must invalidate the
    // native source and the real Vulkan compositor must show the new texels.
    unsafe {
        remove(c, objc::sel::get("removeFromSuperlayer"));
    }
    let surface_child = make_layer("CALayer");
    let number = |value: i64| {
        corefoundation::collections::new_number(corefoundation::collections::Number::Integer(value))
    };
    let key = |name: &str| {
        corefoundation::base::interned_name(name) as usize as corefoundation::base::CFTypeRef
    };
    let properties = corefoundation::collections::new_dictionary(
        &[
            (key("IOSurfaceWidth"), number(2)),
            (key("IOSurfaceHeight"), number(1)),
            (key("IOSurfaceBytesPerElement"), number(4)),
            (
                key("IOSurfacePixelFormat"),
                number(i64::from(iosurface::PIXEL_FORMAT_BGRA)),
            ),
        ],
        corefoundation::callbacks::Policy::from_key_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    let iosurface = unsafe { iosurface::IOSurfaceCreate(properties) };
    assert!(!iosurface.is_null());
    let publish_surface = |pixels: [u8; 8]| unsafe {
        assert_eq!(
            iosurface::IOSurfaceLock(iosurface, 0, std::ptr::null_mut()),
            0
        );
        std::ptr::copy_nonoverlapping(
            pixels.as_ptr(),
            iosurface::IOSurfaceGetBaseAddress(iosurface) as *mut u8,
            8,
        );
        assert_eq!(
            iosurface::IOSurfaceUnlock(iosurface, 0, std::ptr::null_mut()),
            0
        );
    };
    publish_surface([255, 0, 0, 255, 255, 0, 0, 255]);
    unsafe {
        set_frame(
            surface_child,
            objc::sel::get("setFrame:"),
            rect(64., 40., 2., 1.),
        );
        set(
            surface_child,
            objc::sel::get("setContents:"),
            iosurface as Id,
        );
        set(metal_layer, objc::sel::get("addSublayer:"), surface_child);
    }
    repaint(&mut state);
    let published = appkit::view::metal_scene(sdl).unwrap();
    let image = published
        .iter()
        .find(|s| s.native_pixels.is_some())
        .unwrap();
    let surface_point = (image.frame.x as u32, image.frame.y as u32);
    let revision = image.native_revision;
    assert_eq!(
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            120,
            90,
            surface_point.0,
            surface_point.1
        ),
        [0, 0, 255, 255]
    );
    publish_surface([0, 0, 255, 255, 0, 0, 255, 255]);
    repaint(&mut state);
    let republished = appkit::view::metal_scene(sdl).unwrap();
    let image2 = republished
        .iter()
        .find(|s| s.native_pixels.is_some())
        .unwrap();
    assert_ne!(image2.native_revision, revision);
    assert_eq!(
        read_scene_pixel(
            gpu,
            state.composite.as_ref().unwrap().image,
            120,
            90,
            surface_point.0,
            surface_point.1
        ),
        [255, 0, 0, 255],
        "in-place IOSurface publication under a static CAMetalLayer remained stale"
    );
    // Native Apple M2 CA oracle: a half-opacity CALayer *group* must
    // compose overlapping red/blue children before applying group alpha.
    // Multiplying both child alphas first leaks red through the blue top.
    let group = make_layer("CALayer");
    let group_red = make_layer("CALayer");
    let group_blue = make_layer("CALayer");
    let red_group_color = unsafe { appkit::cgcontext::CGColorCreateGenericRGB(1., 0., 0., 1.) };
    let blue_group_color = unsafe { appkit::cgcontext::CGColorCreateGenericRGB(0., 0., 1., 1.) };
    let set_opacity: unsafe extern "C" fn(Id, objc::sel::Sel, f32) =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe {
        set_frame(group, objc::sel::get("setFrame:"), rect(64., 40., 24., 20.));
        for child in [group_red, group_blue] {
            set_frame(child, objc::sel::get("setFrame:"), rect(0., 0., 24., 20.));
            set(group, objc::sel::get("addSublayer:"), child);
        }
        set(
            group_red,
            objc::sel::get("setBackgroundColor:"),
            red_group_color as Id,
        );
        set(
            group_blue,
            objc::sel::get("setBackgroundColor:"),
            blue_group_color as Id,
        );
        set_opacity(group, objc::sel::get("setOpacity:"), 0.5);
        set(metal_layer, objc::sel::get("addSublayer:"), group);
    }
    repaint(&mut state);
    let grouped = read_scene_pixel(
        gpu,
        state.composite.as_ref().unwrap().image,
        120,
        90,
        70,
        40,
    );
    assert_eq!(grouped[0], 0);
    assert!((127..=128).contains(&grouped[1]));
    assert_eq!(grouped[2], 128);
    assert_eq!(grouped[3], 255);
    unsafe { corefoundation::api::CFRelease(iosurface) };
    drop(state);
    gpu.destroy_image(source);
    objc::arc::release(window);
}

/// Explicitly opt-in real Vulkan measurement. Only the position/opacity of
/// one small native overlay changes while all retained GPU content is static.
/// Records producer-side scene recomposition, not physical monitor scanout.
#[test]
#[cfg(not(target_os = "macos"))]
fn metadata_only_animation_over_static_metal_recomposition_benchmark() {
    if std::env::var_os("ROSEMARY_BENCH_SCENE_METADATA").is_none() {
        return;
    }
    if isolate_vulkan_graphical_test(
        "metadata_only_animation_over_static_metal_recomposition_benchmark",
    ) {
        return;
    }
    let gpu = crate::vulkan::adapters()
        .unwrap()
        .first()
        .unwrap()
        .gpu()
        .unwrap();
    for (width, height, count) in [(1280u32, 720u32, 8usize), (1920, 1080, 24)] {
        let window =
            sdl3::video::create_window(c"metadata scene benchmark", width as i32, height as i32, 0)
                .unwrap();
        sdl3::video::show_window(window);
        sdl3::video::presentation_window(window).unwrap();
        let instance = crate::vulkan::instance_handle().unwrap();
        let surface =
            sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library())
                .unwrap();
        let mut state = Swapchain::create(
            gpu,
            window,
            vk::Extent2D { width, height },
            surface,
            instance,
        )
        .unwrap();
        if !uses_layer_compositor(state.present_mode) {
            drop(state);
            sdl3::video::destroy_window(window);
            continue;
        }
        let metal = gpu
            .create_image(&ImageDescription {
                image_type: vk::ImageType::TYPE_2D,
                view_type: vk::ImageViewType::TYPE_2D,
                format: vk::Format::B8G8R8A8_UNORM,
                width,
                height,
                depth: 1,
                levels: 1,
                layers: 1,
                samples: 1,
                usage: gpu::USAGE_RENDER_TARGET,
                cube: false,
                components: vk::ComponentMapping::default(),
            })
            .unwrap();
        gpu.one_shot(|device, cb| unsafe {
            cmd_test_clear_color(device, cb, metal.image, [0., 1., 0., 1.]);
        })
        .unwrap();
        let frame = appkit::view::Frame {
            window,
            x: 0,
            y: 0,
            width,
            height,
            window_width: width,
            window_height: height,
        };
        let base_id = window as usize;
        let mut placements = vec![test_opaque_metal_placement(
            base_id,
            DirectFrame::from_frame(&frame),
        )];
        let pixel = Arc::new([0u8, 0u8, 255u8, 255u8].repeat(32 * 32));
        for i in 0..count {
            let at = appkit::view::Frame {
                x: ((i * 41) % (width as usize - 64)) as i32,
                y: ((i * 37) % (height as usize - 64)) as i32,
                width: 32,
                height: 32,
                ..frame
            };
            placements.push(ScenePlacement {
                opacity_groups: Vec::new(),
                id: (1usize << (usize::BITS - 1)) | (0x6f00 + i),
                frame: DirectFrame::from_frame(&at),
                opacity: 1.,
                opaque: false,
                clip: None,
                rounded: vec![],
                occluded: Some(vec![]),
                native: Some(scene::NativeCpuSceneSource {
                    bytes: Arc::clone(&pixel),
                    revision: 7,
                    cpu_capture: None,
                    dmabuf: None,
                    dma_sample: None,
                }),
            });
        }
        state.layers.push(base_id);
        state.synchronize_scene(window as usize, &placements);
        assert_eq!(
            state.stage_composite_source(
                base_id,
                &frame,
                PresentSource::from(&metal),
                None,
                &mut SourceOwner(None),
                false,
                true
            ),
            Ok(Some(true))
        );
        for source in placements.iter().filter(|source| source.native.is_some()) {
            assert_eq!(stage_native_scene_source(&mut state, source), Ok(true));
        }
        state.synchronize_scene(window as usize, &placements);
        assert_eq!(state.redisplay_scene(window as usize), Ok(true));
        state.idle();
        let mut samples = Vec::with_capacity(24);
        for iteration in 0..24 {
            placements[count].frame.x = 16 + iteration * 5;
            placements[count].opacity = if iteration % 2 == 0 { 1.0 } else { 0.5 };
            let now = Instant::now();
            state.synchronize_scene(window as usize, &placements);
            assert_eq!(state.redisplay_scene(window as usize), Ok(true));
            state.idle();
            samples.push(now.elapsed().as_secs_f64() * 1000.0);
        }
        samples.sort_by(f64::total_cmp);
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)];
        println!(
            "metadata-only full-scene Vulkan {}x{} {} retained layers: mean={mean:.3}ms p95={p95:.3}ms",
            width,
            height,
            count + 1
        );
        // Final pixels: this overlay remains visible without a new Metal draw.
        let at = &placements[count].frame;
        assert_ne!(
            read_scene_pixel(
                gpu,
                state.composite.as_ref().unwrap().image,
                width,
                height,
                at.x as u32 + 4,
                at.y as u32 + 4
            ),
            [0, 255, 0, 255]
        );
        drop(state);
        gpu.destroy_image(metal);
        sdl3::video::destroy_window(window);
    }
}
