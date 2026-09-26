mod corners;
mod present;
#[cfg(test)]
mod tests;

use present::*;

use std::collections::HashMap;
use std::sync::Mutex;

use ash::vk;
use vk::Handle;

use crate::gpu::{self, Gpu, GpuImage, ImageDescription};

/// How many frames may be recorded before one has to have finished.
///
/// Two, so that the host can record the next presentation while the device is
/// still blitting the last. A third buys nothing here: presentation is one
/// blit, and the guest's own frame is what the device is actually waiting on.
const FRAMES_IN_FLIGHT: usize = 2;

/// The per-window state presentation goes through.
struct Swapchain {
    gpu: &'static Gpu,
    surface: vk::SurfaceKHR,
    handle: vk::SwapchainKHR,
    format: vk::Format,
    /// The size the swapchain was actually created at, which is the surface's
    /// answer and not necessarily the one that was asked for.
    extent: vk::Extent2D,
    /// The window size that was last asked for, which is what a later frame
    /// is compared against to decide whether the window resized.
    ///
    /// Not [`Self::extent`]: a surface with a `currentExtent` of its own
    /// overrides the request, and comparing against the overridden value
    /// would see a difference on every frame and rebuild the swapchain on
    /// every frame.
    requested: vk::Extent2D,
    images: Vec<vk::Image>,
    /// One presentation semaphore per swapchain image, rather than one per
    /// frame in flight.
    ///
    /// A presentation holds the semaphore it waits on until that image comes
    /// back from `vkAcquireNextImageKHR`, and how many images there are is
    /// the presentation engine's business, not this module's - a swapchain
    /// with four images and two frames in flight would signal a semaphore
    /// the engine was still waiting on. Indexing by the acquired image makes
    /// the acquisition itself the proof that the last presentation of this
    /// image is done with it.
    blitted: Vec<vk::Semaphore>,
    /// Fences attached to presentations through swapchain maintenance1. One
    /// per image lets acquisition identify the fence which may be recycled.
    /// Empty on devices which use the queue-idle fallback.
    presented: Vec<PresentFence>,
    /// Where layers accumulate when the window has more than one. `None`
    /// while a single layer covers the window, which needs no accumulator.
    composite: Option<GpuImage>,
    /// The layers which have presented to this window. Its length is what
    /// decides whether the composite is needed.
    layers: Vec<usize>,
    /// For a window whose surface shows its alpha, the pass that makes its
    /// content opaque and rounds its corners. `None` presents opaque.
    corners: Option<corners::Corners>,
    pool: vk::CommandPool,
    frames: Vec<Frame>,
    frame: usize,
}

/// One frame's worth of recording and synchronization.
struct Frame {
    command_buffer: vk::CommandBuffer,
    /// Signalled when the presentation engine is done with the acquired
    /// image, waited on before anything is written into it. Per frame rather
    /// than per image, because which image an acquisition returns is not
    /// known until it has returned.
    acquired: vk::Semaphore,
    /// Signalled with the submission, so that this frame's command buffer and
    /// its acquisition semaphore are not reused while the device still has
    /// them.
    done: vk::Fence,
    submitted: bool,
    /// Retained drawable whose image this frame's command buffer reads.
    source: Option<usize>,
}

struct PresentFence {
    fence: vk::Fence,
    /// Whether this fence was attached to a presentation and has not yet been
    /// waited for. A fresh fence is unsignalled but has no operation to wait
    /// for, so the distinction is required during initial teardown.
    pending: bool,
}

static SWAPCHAINS: Mutex<Option<HashMap<usize, Swapchain>>> = Mutex::new(None);

/// Whether a swapchain could not be made for this window, so that a run does
/// not retry - and re-report - a surface the host will never give.
static REFUSED: Mutex<Option<HashMap<usize, ()>>> = Mutex::new(None);

struct SourceOwner(Option<usize>);

impl SourceOwner {
    fn take(&mut self) -> Option<usize> {
        self.0.take()
    }
}

impl Drop for SourceOwner {
    fn drop(&mut self) {
        if let Some(source) = self.0.take() {
            // The copy was not submitted, so only the temporary reference is
            // returned. The command submission still owns the drawable and
            // may send it through the readback fallback after its fence.
            objc::arc::release(source as objc::object::Id);
        }
    }
}

/// Drop everything held for an SDL window that is about to be destroyed.
///
/// `appkit` remakes a window when what it is changes - a hidden dialog shown
/// again as a menu - and the map here is keyed by the window's address, which
/// SDL reuses. An entry left behind would be presented to a surface that no
/// longer exists.
pub(crate) fn forget_window(window: sdl3::Window) {
    let key = window as usize;
    if let Some(all) = SWAPCHAINS.lock().expect("swapchains").as_mut() {
        all.remove(&key);
    }
    if let Some(refused) = REFUSED.lock().expect("refused windows").as_mut() {
        refused.remove(&key);
    }
}

/// Blit `source` into `window`'s swapchain image at `frame` and present it.
///
/// `Ok(false)` is "this window cannot be presented to with a swapchain",
/// which is not an error: the caller falls back to reading the frame back and
/// blitting it through SDL's window surface, and every host without
/// `VK_KHR_surface` takes that path.
///
/// `holes` are rectangles of other layers, which keep what those layers last
/// presented there: the window's store is presented around its views' layers.
pub(crate) fn present(
    gpu: &'static Gpu,
    layer: usize,
    frame: &appkit::view::Frame,
    source: &GpuImage,
    holes: &[(usize, Region)],
) -> Result<bool, String> {
    present_impl(gpu, layer, frame, source, holes, None)
}

/// Present a command buffer's drawable, consuming its retained `owner` on
/// every return path. A successful copy holds it until the frame fence wakes.
pub(crate) fn present_owned(
    gpu: &'static Gpu,
    layer: usize,
    frame: &appkit::view::Frame,
    source: &GpuImage,
    holes: &[(usize, Region)],
    owner: usize,
) -> Result<bool, String> {
    present_impl(gpu, layer, frame, source, holes, Some(owner))
}

fn present_impl(
    gpu: &'static Gpu,
    layer: usize,
    frame: &appkit::view::Frame,
    source: &GpuImage,
    holes: &[(usize, Region)],
    owner: Option<usize>,
) -> Result<bool, String> {
    let mut owner = SourceOwner(owner);
    if gpu.swapchain_api().is_none() || crate::vulkan::surface_api().is_none() {
        return Ok(false);
    }
    let window = frame.window as usize;
    if REFUSED
        .lock()
        .expect("refused windows")
        .as_ref()
        .is_some_and(|refused| refused.contains_key(&window))
    {
        return Ok(false);
    }
    let mut guard = SWAPCHAINS.lock().expect("window swapchains");
    let all = guard.get_or_insert_with(HashMap::new);
    let extent = vk::Extent2D {
        width: frame.window_width.max(1),
        height: frame.window_height.max(1),
    };

    if let std::collections::hash_map::Entry::Vacant(slot) = all.entry(window) {
        match Swapchain::create(gpu, frame.window, extent) {
            Ok(swapchain) => {
                gpu::milestone("window swapchain");
                // Without the pass the surface ignores alpha, and the
                // chrome's corners would show black where they are cut.
                if swapchain.corners.is_none() {
                    appkit::decor::presented_opaque(frame.window);
                }
                slot.insert(swapchain);
            }
            Err(reason) => {
                gpu::trace(format!(
                    "no swapchain for this window, presenting through a readback: {reason}"
                ));
                REFUSED
                    .lock()
                    .expect("refused windows")
                    .get_or_insert_with(HashMap::new)
                    .insert(window, ());
                return Ok(false);
            }
        }
    }
    let swapchain = all.get_mut(&window).expect("just created");
    let joined = !swapchain.layers.contains(&layer);
    if joined {
        swapchain.layers.push(layer);
    }
    // The window resized, or the presentation engine said the last image no
    // longer matches it. Either way every image in flight is finished with
    // before the old swapchain goes.
    if swapchain.requested != extent {
        swapchain.rebuild(extent)?;
    }
    // Only a layer that has presented here has anything of its own to keep:
    // over any other hole the source is still what the window shows.
    let holes: Vec<Region> = holes
        .iter()
        .filter(|(owner, _)| *owner != layer && swapchain.layers.contains(owner))
        .map(|(_, region)| *region)
        .collect();
    let regions = match holes.is_empty() {
        true => None,
        false => Some(subtract(
            Region {
                x: frame.x,
                y: frame.y,
                width: frame.width as i32,
                height: frame.height as i32,
            },
            &holes,
        )),
    };
    let presented = swapchain.present(frame, source, regions.as_deref(), owner.take());
    drop(guard);
    // A layer's first frame starts the window's accumulator, which holds
    // nothing of the window's store yet: the store may have gone straight to
    // the screen before there was a layer to leave room for. Have it
    // presented again, around the layer this time.
    if joined && layer != window {
        appkit::backing::present_again(frame.window);
    }
    presented
}

fn finish_source(source: Option<usize>) {
    if let Some(source) = source {
        crate::present::presentation_complete(source);
    }
}

/// A rectangle of window pixels, top-left origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// `whole` less every rectangle in `holes`, as rectangles that do not overlap.
pub(crate) fn subtract(whole: Region, holes: &[Region]) -> Vec<Region> {
    let mut pieces = vec![whole];
    for hole in holes {
        let mut next = Vec::with_capacity(pieces.len() + 4);
        for piece in pieces {
            let left = piece.x.max(hole.x);
            let top = piece.y.max(hole.y);
            let right = (piece.x + piece.width).min(hole.x + hole.width);
            let bottom = (piece.y + piece.height).min(hole.y + hole.height);
            if left >= right || top >= bottom {
                next.push(piece);
                continue;
            }
            let (piece_right, piece_bottom) = (piece.x + piece.width, piece.y + piece.height);
            let bands = [
                (piece.x, piece.y, piece.width, top - piece.y),
                (piece.x, bottom, piece.width, piece_bottom - bottom),
                (piece.x, top, left - piece.x, bottom - top),
                (right, top, piece_right - right, bottom - top),
            ];
            next.extend(
                bands
                    .into_iter()
                    .filter(|&(_, _, width, height)| width > 0 && height > 0)
                    .map(|(x, y, width, height)| Region {
                        x,
                        y,
                        width,
                        height,
                    }),
            );
        }
        pieces = next;
    }
    pieces
}

impl Swapchain {
    fn create(
        gpu: &'static Gpu,
        window: sdl3::Window,
        extent: vk::Extent2D,
    ) -> Result<Self, String> {
        // Never an OpenGL window. SDL will convert one in place, but it
        // converts the flags and the loaded libraries and not the backend's
        // own state: on Wayland the `EGLSurface` SDL created with the window
        // stays alive on the `wl_surface`, and when the Vulkan driver claims
        // the same surface the compositor answers
        // `wp_linux_drm_syncobj_manager_v1: surface already exists` and
        // closes the connection.
        //
        // A window with no graphics API - which is how this runtime makes
        // them, exactly so that this is possible - has no `EGLSurface` and
        // is converted safely, so only a guest that has actually asked for
        // an `NSOpenGLContext` ends up here.
        if sdl3::video::window_flags(window) & sdl3::video::flags::OPENGL != 0 {
            return Err("the window belongs to OpenGL, which cannot share it".into());
        }
        let instance = crate::vulkan::instance_handle().ok_or("the Vulkan instance is gone")?;
        let surface = sdl3::video::vulkan::create_surface(window, instance)
            .ok_or_else(|| format!("SDL_Vulkan_CreateSurface failed: {}", sdl3::error()))?;
        let surface = vk::SurfaceKHR::from_raw(surface);
        // Until the `Swapchain` exists there is no `Drop` to undo any of this,
        // so each piece is guarded and the guards are taken together once the
        // last fallible step has succeeded.
        let surface = gpu::Guard::new(surface, |surface| {
            sdl3::video::vulkan::destroy_surface(instance, surface.as_raw());
        });
        if !gpu.can_present(*surface) {
            return Err("the Vulkan queue Metal submits to cannot present to this window".into());
        }
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(gpu.queue_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        // SAFETY: a well-formed create info on this live device.
        let pool = unsafe { gpu.device.create_command_pool(&pool_info, None) }
            .map_err(|error| format!("vkCreateCommandPool failed with {error:?}"))?;
        // The command buffers are the pool's, so they go with it.
        let pool = gpu::Guard::new(pool, |pool| {
            // SAFETY: a pool of this device's, nothing allocated from it
            // having been submitted.
            unsafe { gpu.device.destroy_command_pool(pool, None) };
        });
        let allocate = vk::CommandBufferAllocateInfo::default()
            .command_pool(*pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(FRAMES_IN_FLIGHT as u32);
        // SAFETY: the pool was just created by this device.
        let command_buffers = unsafe { gpu.device.allocate_command_buffers(&allocate) }
            .map_err(|error| format!("vkAllocateCommandBuffers failed with {error:?}"))?;
        let mut frames = gpu::GuardedVec::new(|frame: &Frame| {
            // SAFETY: objects of this device's, never submitted.
            unsafe {
                gpu.device.destroy_semaphore(frame.acquired, None);
                gpu.device.destroy_fence(frame.done, None);
            }
        });
        for command_buffer in command_buffers {
            // A fence created signalled, because the first use of every frame
            // waits on it before it has ever been submitted.
            let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
            // SAFETY: a well-formed create info on this live device.
            let acquired = unsafe {
                gpu.device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
            }
            .map_err(|error| format!("vkCreateSemaphore failed with {error:?}"))?;
            let acquired = gpu::Guard::new(acquired, |acquired| {
                // SAFETY: a semaphore of this device's, never submitted.
                unsafe { gpu.device.destroy_semaphore(acquired, None) };
            });
            // SAFETY: a well-formed create info on this live device.
            let done = unsafe { gpu.device.create_fence(&fence_info, None) }
                .map_err(|error| format!("vkCreateFence failed with {error:?}"))?;
            frames.push(Frame {
                command_buffer,
                acquired: acquired.take(),
                done,
                submitted: false,
                source: None,
            });
        }
        // Only a window made with room for rounded corners has a surface
        // that shows alpha; see `appkit::window`.
        let transparent = sdl3::video::window_flags(window) & sdl3::video::flags::TRANSPARENT != 0;
        let corners = match transparent {
            true => corners::Corners::new(gpu)
                .inspect_err(|reason| {
                    gpu::trace(format!(
                        "window corners unavailable, presenting opaque: {reason}"
                    ))
                })
                .ok(),
            false => None,
        };
        let mut swapchain = Self {
            gpu,
            surface: surface.take(),
            handle: vk::SwapchainKHR::null(),
            format: vk::Format::UNDEFINED,
            extent,
            requested: extent,
            images: Vec::new(),
            blitted: Vec::new(),
            presented: Vec::new(),
            composite: None,
            layers: Vec::new(),
            corners,
            pool: pool.take(),
            frames: frames.take(),
            frame: 0,
        };
        // From here the `Drop` above owns everything, including whatever
        // `build` leaves behind when it fails.
        swapchain.build(extent)?;
        Ok(swapchain)
    }

    /// Create the `VkSwapchainKHR` itself, replacing whatever is there.
    ///
    /// Every replacement is built whole before any of it is installed: a
    /// failure part-way leaves the swapchain exactly as it was, rather than a
    /// `self.handle` naming a swapchain this call has already destroyed.
    fn build(&mut self, requested: vk::Extent2D) -> Result<(), String> {
        let surface_api = crate::vulkan::surface_api().ok_or("VK_KHR_surface is gone")?;
        let api = self.gpu.swapchain_api().ok_or("VK_KHR_swapchain is gone")?;
        // SAFETY: a physical device and a surface of this instance's.
        let capabilities = unsafe {
            surface_api.get_physical_device_surface_capabilities(self.gpu.physical, self.surface)
        }
        .map_err(|error| format!("vkGetPhysicalDeviceSurfaceCapabilitiesKHR failed: {error:?}"))?;
        let formats = unsafe {
            surface_api.get_physical_device_surface_formats(self.gpu.physical, self.surface)
        }
        .map_err(|error| format!("vkGetPhysicalDeviceSurfaceFormatsKHR failed: {error:?}"))?;
        let chosen = choose_format(&formats).ok_or("the surface reports no format")?;
        let present_modes = unsafe {
            surface_api.get_physical_device_surface_present_modes(self.gpu.physical, self.surface)
        }
        .map_err(|error| format!("vkGetPhysicalDeviceSurfacePresentModesKHR failed: {error:?}"))?;
        let present_mode = choose_present_mode(&present_modes);
        let alpha = composite_alpha(&capabilities, self.corners.is_some());

        // `0xffff_ffff` in either direction is a surface which takes its size
        // from the swapchain rather than the other way round.
        let extent = match capabilities.current_extent.width {
            u32::MAX => vk::Extent2D {
                width: requested.width.clamp(
                    capabilities.min_image_extent.width,
                    capabilities.max_image_extent.width,
                ),
                height: requested.height.clamp(
                    capabilities.min_image_extent.height,
                    capabilities.max_image_extent.height,
                ),
            },
            _ => capabilities.current_extent,
        };
        if extent.width == 0 || extent.height == 0 {
            return Err("the window has no area to present into".into());
        }
        // One more than the minimum, so that the host is not blocked in
        // `vkAcquireNextImageKHR` for as long as the engine holds one.
        let count = match capabilities.max_image_count {
            0 => capabilities.min_image_count + 1,
            most => (capabilities.min_image_count + 1).min(most),
        };
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(self.surface)
            .min_image_count(count)
            .image_format(chosen.format)
            .image_color_space(chosen.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            // The frame arrives as a blit, never as an attachment: the guest
            // rendered into its own drawable long before this.
            .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(capabilities.current_transform)
            .composite_alpha(alpha)
            .present_mode(present_mode)
            .clipped(true)
            .old_swapchain(self.handle);
        // SAFETY: a well-formed create info; the old swapchain is retired by
        // this call and destroyed once the replacement is whole.
        let handle = unsafe { api.create_swapchain(&info, None) }
            .map_err(|error| format!("vkCreateSwapchainKHR failed with {error:?}"))?;
        let handle = gpu::Guard::new(handle, |handle| {
            // SAFETY: a swapchain of this device's which was never installed,
            // so nothing has acquired an image from it.
            unsafe { api.destroy_swapchain(handle, None) };
        });
        // SAFETY: the swapchain was just created by this device.
        let images = unsafe { api.get_swapchain_images(*handle) }
            .map_err(|error| format!("vkGetSwapchainImagesKHR failed with {error:?}"))?;
        // One semaphore per image, made before anything is installed so that a
        // failure half way through leaves neither these nor the swapchain.
        let mut blitted = gpu::GuardedVec::new(|semaphore: &vk::Semaphore| {
            // SAFETY: a semaphore of this device's, never submitted.
            unsafe { self.gpu.device.destroy_semaphore(*semaphore, None) };
        });
        for _ in &images {
            // SAFETY: a well-formed create info on this live device.
            blitted.push(
                unsafe {
                    self.gpu
                        .device
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                }
                .map_err(|error| format!("vkCreateSemaphore failed with {error:?}"))?,
            );
        }
        let mut presented = gpu::GuardedVec::new(|present: &PresentFence| {
            // SAFETY: a fence of this device's, never submitted.
            unsafe { self.gpu.device.destroy_fence(present.fence, None) };
        });
        if self.gpu.has_present_fences() {
            for _ in &images {
                // Present fences must be unsignalled when passed to
                // vkQueuePresentKHR.
                let fence = unsafe {
                    self.gpu
                        .device
                        .create_fence(&vk::FenceCreateInfo::default(), None)
                }
                .map_err(|error| format!("vkCreateFence failed with {error:?}"))?;
                presented.push(PresentFence {
                    fence,
                    pending: false,
                });
            }
        }

        // Nothing below can fail, so the replacement is installed and the old
        // state destroyed as one step.
        let blitted = blitted.take();
        let presented = presented.take();
        let old = std::mem::replace(&mut self.handle, handle.take());
        if old != vk::SwapchainKHR::null() {
            // SAFETY: retired by the creation above, and every submission
            // touching its images has been waited for by the caller.
            unsafe { api.destroy_swapchain(old, None) };
        }
        // The old swapchain's images are gone with it, and so is the meaning
        // of a semaphore indexed by one.
        for semaphore in std::mem::replace(&mut self.blitted, blitted) {
            // SAFETY: `idle` waited either the presentation fences or the
            // queue before the swapchain these belonged to was destroyed.
            unsafe { self.gpu.device.destroy_semaphore(semaphore, None) };
        }
        for present in std::mem::replace(&mut self.presented, presented) {
            // SAFETY: `idle` waited every pending use of this fence.
            unsafe { self.gpu.device.destroy_fence(present.fence, None) };
        }
        self.requested = requested;
        self.format = chosen.format;
        self.extent = extent;
        self.images = images;
        if let Some(composite) = self.composite.take() {
            self.gpu.destroy_image(composite);
        }
        // A surface that cannot show premultiplied alpha is presented
        // opaque, which the pass would only get wrong.
        if alpha != vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED {
            self.corners = None;
        }
        if let Some(corners) = self.corners.as_mut() {
            corners.forget_output();
        }
        Ok(())
    }

    /// Wait for every frame in flight and rebuild at `extent`.
    fn rebuild(&mut self, extent: vk::Extent2D) -> Result<(), String> {
        self.idle();
        self.build(extent)
    }

    /// Block until nothing this swapchain gave the queue is still running.
    ///
    /// Presentation fences cover the presentation engine's access to the
    /// image and wait semaphore. On an older device, only making the queue
    /// idle supplies that guarantee.
    fn idle(&mut self) {
        if !self.gpu.has_present_fences() {
            self.gpu.wait_queue_idle();
            for frame in &mut self.frames {
                frame.submitted = false;
                finish_source(frame.source.take());
            }
            return;
        }
        let submitted = self
            .frames
            .iter()
            .filter(|frame| frame.submitted)
            .map(|frame| frame.done);
        let presented = self
            .presented
            .iter()
            .filter(|present| present.pending)
            .map(|present| present.fence);
        let fences = submitted.chain(presented).collect::<Vec<_>>();
        if !fences.is_empty() {
            // SAFETY: each fence belongs to this device and is associated
            // with an operation which was successfully queued.
            let _ = unsafe { self.gpu.device.wait_for_fences(&fences, true, u64::MAX) };
        }
        for frame in &mut self.frames {
            frame.submitted = false;
            finish_source(frame.source.take());
        }
        for present in &mut self.presented {
            present.pending = false;
        }
    }

    /// The image layers accumulate into, made on the first frame that needs
    /// one and kept in `GENERAL` for the same reason every other image in
    /// this crate is.
    ///
    /// With the corners pass, the composite is one the pass can read.
    fn composite(&mut self) -> Result<vk::Image, String> {
        if self.composite.is_none() {
            let description = match self.corners {
                Some(_) => corners::storage(self.extent.width, self.extent.height),
                None => ImageDescription {
                    image_type: vk::ImageType::TYPE_2D,
                    view_type: vk::ImageViewType::TYPE_2D,
                    format: self.format,
                    width: self.extent.width,
                    height: self.extent.height,
                    depth: 1,
                    levels: 1,
                    layers: 1,
                    samples: 1,
                    usage: gpu::USAGE_RENDER_TARGET,
                    cube: false,
                },
            };
            let image = self.gpu.create_image(&description)?;
            // A fresh image's contents are undefined, and the frame that
            // brings the composite into existence has only the one layer
            // that asked for it - so everywhere the other layers have not
            // reached yet would be whatever the allocation held.
            let clear = vk::ClearColorValue { float32: [0.0; 4] };
            let range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };
            self.gpu.one_shot(|device, command_buffer| {
                // SAFETY: recording into a command buffer the call owns; the
                // image is in GENERAL from creation.
                unsafe {
                    device.cmd_clear_color_image(
                        command_buffer,
                        image.image,
                        vk::ImageLayout::GENERAL,
                        &clear,
                        &[range],
                    )
                };
            })?;
            self.composite = Some(image);
        }
        Ok(self.composite.as_ref().expect("just made").image)
    }

    /// `regions`, where given, are the parts of `frame` to blit, each from
    /// the same rectangle of `source`: the source is the window's size.
    fn present(
        &mut self,
        frame: &appkit::view::Frame,
        source: &GpuImage,
        regions: Option<&[Region]>,
        owner: Option<usize>,
    ) -> Result<bool, String> {
        let mut owner = SourceOwner(owner);
        // One layer covering the window needs no accumulator: what it draws
        // is the whole of what the window shows, so it goes straight into the
        // image the presentation engine hands back.
        // The corners pass reads the composite, so a window with it always
        // has one.
        let direct = self.layers.len() <= 1
            && frame.covers_window()
            && regions.is_none()
            && self.corners.is_none();
        let composite = match direct {
            true => None,
            false => Some(self.composite()?),
        };
        if let Some(corners) = self.corners.as_mut() {
            corners.prepare(self.extent.width, self.extent.height)?;
        }

        let slot = self.frame % self.frames.len().max(1);
        let this = &self.frames[slot];
        let (command_buffer, acquired, done) = (this.command_buffer, this.acquired, this.done);
        // SAFETY: a fence of this device's, signalled at creation or by the
        // submission this slot last made.
        unsafe { self.gpu.device.wait_for_fences(&[done], true, u64::MAX) }
            .map_err(|error| format!("vkWaitForFences failed with {error:?}"))?;
        self.frames[slot].submitted = false;
        finish_source(self.frames[slot].source.take());

        let Some(index) = crate::stats::timed(crate::stats::Counter::PresentAcquireMicros, || {
            self.acquire(acquired)
        })?
        else {
            // The swapchain was rebuilt underneath the acquisition. The frame
            // the guest drew is gone, which is what a resize costs; the next
            // one lands in the new swapchain.
            finish_source(owner.take());
            return Ok(true);
        };
        let target = self.images[index as usize];
        // Safe to signal: this image has just come back from the
        // presentation engine, so the presentation which last waited on this
        // semaphore is finished.
        let blitted = self.blitted[index as usize];
        let present_fence = if let Some(present) = self.presented.get_mut(index as usize) {
            if present.pending {
                // SAFETY: this fence was attached to the last presentation
                // of the image which acquisition just returned.
                unsafe {
                    self.gpu
                        .device
                        .wait_for_fences(&[present.fence], true, u64::MAX)
                }
                .map_err(|error| format!("vkWaitForFences failed with {error:?}"))?;
                present.pending = false;
            }
            // SAFETY: the fence is either fresh or was waited above, and is
            // not associated with an outstanding queue operation.
            unsafe { self.gpu.device.reset_fences(&[present.fence]) }
                .map_err(|error| format!("vkResetFences failed with {error:?}"))?;
            Some(present.fence)
        } else {
            None
        };

        let record_timer = crate::stats::timer();
        let device = &self.gpu.device;
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: this slot's fence was waited for above, so nothing is
        // reading the command buffer.
        unsafe {
            device
                .reset_command_buffer(command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|error| format!("vkResetCommandBuffer failed with {error:?}"))?;
            device
                .begin_command_buffer(command_buffer, &begin)
                .map_err(|error| format!("vkBeginCommandBuffer failed with {error:?}"))?;
        }

        // SAFETY: recording into a command buffer this call owns; every image
        // named below belongs to this device.
        unsafe {
            prepare_source(device, command_buffer, source.image);
            let whole = [
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D {
                    x: self.extent.width as i32,
                    y: self.extent.height as i32,
                    z: 1,
                },
            ];
            match composite {
                // The layer's own rectangle of the accumulator, then the
                // whole accumulator into the image being presented.
                Some(composite) => {
                    // A layer wholly off the edge of its window contributes
                    // nothing; what the composite already holds is still
                    // what the window shows, so it is presented unchanged.
                    match regions {
                        Some(regions) => {
                            for region in regions {
                                let part = appkit::view::Frame {
                                    x: region.x,
                                    y: region.y,
                                    width: region.width.max(0) as u32,
                                    height: region.height.max(0) as u32,
                                    ..*frame
                                };
                                if let Some(into) = clamp(&part, self.extent) {
                                    blit(
                                        device,
                                        command_buffer,
                                        source.image,
                                        into,
                                        composite,
                                        vk::ImageLayout::GENERAL,
                                        into,
                                    );
                                }
                            }
                        }
                        None => {
                            if let Some(into) = clamp(frame, self.extent) {
                                blit(
                                    device,
                                    command_buffer,
                                    source.image,
                                    extent_of(source),
                                    composite,
                                    vk::ImageLayout::GENERAL,
                                    into,
                                );
                            }
                        }
                    }
                    let shown = match &self.corners {
                        Some(corners) => {
                            let shape = corners::Shape::of(
                                frame.window,
                                self.extent.width,
                                self.extent.height,
                            );
                            let image = self.composite.as_ref().expect("made above");
                            corners.record(command_buffer, slot, image, shape)
                        }
                        None => {
                            // The blit above must land before the one below
                            // reads it.
                            barrier(
                                device,
                                command_buffer,
                                composite,
                                vk::ImageLayout::GENERAL,
                                vk::ImageLayout::GENERAL,
                                vk::AccessFlags::TRANSFER_WRITE,
                                vk::AccessFlags::TRANSFER_READ,
                            );
                            composite
                        }
                    };
                    transition_to_transfer_dst(device, command_buffer, target);
                    blit(
                        device,
                        command_buffer,
                        shown,
                        whole,
                        target,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        whole,
                    );
                }
                None => {
                    transition_to_transfer_dst(device, command_buffer, target);
                    blit(
                        device,
                        command_buffer,
                        source.image,
                        extent_of(source),
                        target,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        whole,
                    );
                }
            }
            transition_to_present(device, command_buffer, target);
            device
                .end_command_buffer(command_buffer)
                .map_err(|error| format!("vkEndCommandBuffer failed with {error:?}"))?;
        }
        crate::stats::finish_timer(crate::stats::Counter::PresentRecordMicros, record_timer);

        crate::stats::timed(crate::stats::Counter::PresentSubmitMicros, || {
            self.gpu.submit_synchronized(
                command_buffer,
                done,
                &[(acquired, vk::PipelineStageFlags::TRANSFER)],
                &[blitted],
            )
        })?;
        self.frames[slot].submitted = true;
        self.frames[slot].source = owner.take();
        self.frame = self.frame.wrapping_add(1);

        let result = crate::stats::timed(crate::stats::Counter::QueuePresentMicros, || {
            self.gpu.present(self.handle, index, blitted, present_fence)
        });
        // OUT_OF_DATE and the other ordinary WSI errors still enqueue the
        // semaphore wait. Only allocation failure is specified to leave all
        // synchronization objects untouched.
        if present_fence.is_some()
            && !matches!(
                result,
                Err(vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)
            )
        {
            self.presented[index as usize].pending = true;
        }
        match result {
            Ok(suboptimal) => {
                if suboptimal {
                    // The image still reached the window; the next frame is
                    // the one that gets a swapchain matching it.
                    self.rebuild(self.requested)?;
                }
                Ok(true)
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.rebuild(self.requested)?;
                Ok(true)
            }
            Err(error) => {
                // The blit submission succeeded and owns the drawable until
                // its frame fence. A WSI failure must not make the caller
                // schedule a second consumer or release the slot early.
                gpu::trace(format!("vkQueuePresentKHR failed with {error:?}"));
                Ok(true)
            }
        }
    }

    /// The next image to write into, or `None` where the swapchain had to be
    /// rebuilt and this frame is lost with it.
    fn acquire(&mut self, signal: vk::Semaphore) -> Result<Option<u32>, String> {
        let api = self.gpu.swapchain_api().ok_or("VK_KHR_swapchain is gone")?;
        // SAFETY: a swapchain and a semaphore of this device's; no fence, so
        // the semaphore is the only signal.
        let acquired =
            unsafe { api.acquire_next_image(self.handle, u64::MAX, signal, vk::Fence::null()) };
        match acquired {
            Ok((index, _suboptimal)) => Ok(Some(index)),
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.rebuild(self.requested)?;
                Ok(None)
            }
            Err(error) => Err(format!("vkAcquireNextImageKHR failed with {error:?}")),
        }
    }
}

/// Nothing in this runtime destroys an `SDL_Window` today - `-[NSWindow
/// close]` orders it out and the window stays in the list - so a swapchain is
/// only dropped if one is taken out of the map. Teardown lives here anyway
/// rather than in a function nothing calls: whichever way a swapchain stops
/// being reachable, the surface, the semaphores and the pool go with it, and
/// the order below is the one Vulkan requires.
impl Drop for Swapchain {
    fn drop(&mut self) {
        self.idle();
        let device = &self.gpu.device;
        // The swapchain first: its images are what the semaphores below were
        // signalled for, and a surface outlives the swapchain made on it.
        if let Some(api) = self.gpu.swapchain_api() {
            // SAFETY: the wait above retired every submission and presentation
            // which refers to this swapchain.
            unsafe { api.destroy_swapchain(self.handle, None) };
        }
        for frame in &self.frames {
            // SAFETY: objects of this device's, with nothing in flight after
            // the wait above.
            unsafe {
                device.destroy_semaphore(frame.acquired, None);
                device.destroy_fence(frame.done, None);
            }
        }
        for semaphore in std::mem::take(&mut self.blitted) {
            // SAFETY: as above, and the swapchain whose presentations waited
            // on these is destroyed first.
            unsafe { device.destroy_semaphore(semaphore, None) };
        }
        for present in std::mem::take(&mut self.presented) {
            // SAFETY: the wait above retired every pending presentation.
            unsafe { device.destroy_fence(present.fence, None) };
        }
        // SAFETY: the pool owns the command buffers, which are freed with it.
        unsafe { device.destroy_command_pool(self.pool, None) };
        if let Some(composite) = self.composite.take() {
            self.gpu.destroy_image(composite);
        }
        if let Some(instance) = crate::vulkan::instance_handle() {
            sdl3::video::vulkan::destroy_surface(instance, self.surface.as_raw());
        }
    }
}
