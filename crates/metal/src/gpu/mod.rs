mod format;
mod guard;
mod memory;
mod pools;
mod submit;
mod trace;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use ash::{Device, Instance, vk};
use static_init::dynamic;

use crate::heap::{Allocation, Heap};

pub(crate) use format::sample_count;
pub use format::{aspect_of, block_extent, pixel_format, texel_size};
pub(crate) use guard::{Guard, GuardedVec};
use trace::dump_spirv;
pub use trace::{milestone, trace, trace_once};

type DescriptorLayoutKey = (Vec<(u32, vk::DescriptorType)>, vk::ShaderStageFlags, bool);
type DescriptorLayouts = HashMap<DescriptorLayoutKey, vk::DescriptorSetLayout>;

// ash 0.38 was generated before robustness2 was ratified. The KHR feature
// structures and structure types alias the EXT spellings exactly, so only the
// extension name needs spelling locally until ash carries the newer headers.
const ROBUSTNESS_2_KHR: &std::ffi::CStr = c"VK_KHR_robustness2";

#[derive(Clone, Copy)]
struct DeviceCapabilities {
    null_descriptors: bool,
    vertex_attribute_divisor: bool,
    swapchain_maintenance1: bool,
    /// Whether `VK_SAMPLER_ADDRESS_MODE_MIRROR_CLAMP_TO_EDGE` is legal, as
    /// the Vulkan 1.2 feature bit rather than the extension.
    sampler_mirror_clamp_to_edge: bool,
}

/// What `VK_KHR_depth_stencil_resolve` - core in Vulkan 1.2 - can do on this
/// device, and how a render pass which uses it is created.
pub(crate) struct DepthStencilResolve {
    /// `VK_KHR_create_renderpass2`'s entry points, where the resolve is
    /// reached through the extension rather than through Vulkan 1.2 core.
    pub renderpass2: Option<ash::khr::create_renderpass2::Device>,
    pub support: ResolveSupport,
}

/// The depth and stencil resolve modes a device reports. Sample-zero is the
/// only one every implementation supporting the feature must have; average,
/// minimum and maximum are optional, so a Metal filter is mapped only when
/// the device names it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ResolveSupport {
    pub depth_modes: vk::ResolveModeFlags,
    pub stencil_modes: vk::ResolveModeFlags,
    /// Whether one aspect may resolve while the other does not.
    pub independent_none: bool,
    /// Whether the two aspects may use different modes.
    pub independent: bool,
}

/// One logical device and the Vulkan queue onto which this implementation
/// deliberately serializes every Metal command queue. Multiple Vulkan queues
/// require resource hazard tracking which this runtime does not yet have.
pub struct Gpu {
    pub device: Device,
    pub physical: vk::PhysicalDevice,
    pub queue_family: u32,
    /// Vulkan requires external synchronization on a queue, and Metal lets any
    /// thread commit a command buffer.
    queue: Mutex<vk::Queue>,
    /// Commits hold this across the guest submit and every presentation
    /// submit attached to it. Queue calls themselves keep using `queue` for
    /// Vulkan's external-synchronization rule.
    submission_boundary: Mutex<()>,
    /// In-flight work and its completion worker are device-local. Independent
    /// Vulkan devices must never head-of-line block one another.
    pub(crate) submissions: crate::submission::Tracker,
    memory: vk::PhysicalDeviceMemoryProperties,
    pub limits: vk::PhysicalDeviceLimits,
    /// One layout per distinct set of Metal binding indices, so pipelines
    /// sharing a resource shape share a layout.
    layouts: Mutex<DescriptorLayouts>,
    render_passes: Mutex<HashMap<RenderPassKey, vk::RenderPass>>,
    /// The pool and fence behind `one_shot`, which runs the small transfers
    /// and layout transitions Metal performs outside a command buffer.
    immediate: Mutex<Option<Immediate>>,
    /// The blocks Metal buffers are cut out of, made on first use because the
    /// memory type depends on what a buffer's requirements admit.
    buffers: OnceLock<Heap>,
    /// Two-timestamp query pools, one per command buffer in flight, kept for
    /// reuse by command-buffer timing and the optional statistics.
    timestamp_pools: Mutex<Vec<vk::QueryPool>>,
    timestamp_period: f32,
    timestamp_valid_bits: u32,
    /// Command pools, descriptor pools and upload chunks whose submission has
    /// retired, kept for the next command buffer. A game commits several
    /// command buffers a frame, and creating and destroying each one's pool,
    /// fence and descriptor pools every frame is driver work on the thread the
    /// guest records on.
    contexts: Mutex<Vec<CommandContext>>,
    context_budget: ContextBudget,
    descriptor_pools: Mutex<Vec<vk::DescriptorPool>>,
    upload_chunks: Mutex<Vec<GpuBuffer>>,
    /// `VK_KHR_push_descriptor`, where advertised, and the descriptor limit
    /// which decides whether one set in a pipeline can use it.
    push_descriptor_api: Option<ash::khr::push_descriptor::Device>,
    pub max_push_descriptors: u32,
    /// Whether null buffer and image-view descriptors have defined Metal-like
    /// reads and writes on this device.
    pub(crate) null_descriptors: bool,
    /// Largest per-instance vertex step rate supported through
    /// `VK_KHR_vertex_attribute_divisor`, or zero when it is unavailable.
    pub(crate) max_vertex_attribute_divisor: u32,
    /// Whether a draw using a vertex divisor may have a non-zero base instance.
    pub(crate) divisor_non_zero_first_instance: bool,
    /// Whether a sampler may use `MTLSamplerAddressModeMirrorClampToEdge`.
    /// The mode is not legal on an unextended Vulkan 1.1 device, and a
    /// sampler which asks for it there is rejected rather than given a
    /// different wrap.
    pub(crate) sampler_mirror_clamp_to_edge: bool,
    /// robustness2 does not permit a null standalone sampler descriptor.
    /// This neutral sampler keeps an otherwise-null resource table valid.
    pub(crate) null_sampler: vk::Sampler,
    /// `VK_KHR_swapchain`'s device functions, where the device has it. `None`
    /// is a device which cannot present at all, and presentation falls back
    /// to reading the frame back and blitting it through SDL's window
    /// surface.
    swapchain_api: Option<ash::khr::swapchain::Device>,
    /// Whether present operations can carry fences which retire their wait
    /// semaphores and swapchain resources without making the whole queue idle.
    swapchain_maintenance1: bool,
    /// Depth/stencil resolve, where the device has it. `None` is a device
    /// which cannot resolve a multisampled depth or stencil attachment at
    /// all, and a pass which asks for one is reported and left unresolved.
    pub(crate) depth_stencil_resolve: Option<DepthStencilResolve>,
}

struct Immediate {
    pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
}

/// One command buffer with the pool it was allocated from and the fence its
/// submission signals.
pub struct CommandContext {
    pub pool: vk::CommandPool,
    pub command_buffer: vk::CommandBuffer,
    pub fence: vk::Fence,
}

struct ContextBudget {
    used: Mutex<usize>,
    available: std::sync::Condvar,
    limit: usize,
}

impl ContextBudget {
    fn new() -> Self {
        let limit = std::env::var("ROSETTA3_METAL_MAX_COMMAND_BUFFERS")
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|limit| *limit > 0)
            .unwrap_or(64);
        Self {
            used: Mutex::new(0),
            available: std::sync::Condvar::new(),
            limit,
        }
    }
}

/// How many retired objects of each kind are kept for reuse. Enough for the
/// command buffers of a few frames in flight; a burst beyond it is destroyed
/// rather than held for the rest of the run.
const RECYCLED: usize = 32;

/// The size of one chunk `-setBytes:` copies are cut from. Metal limits one
/// copy to four kilobytes, so a chunk holds a frame's worth of them.
pub const UPLOAD_CHUNK: u64 = 256 << 10;

fn create_device(
    instance: &Instance,
    physical: vk::PhysicalDevice,
    queues: &[vk::DeviceQueueCreateInfo<'_>],
    features: &vk::PhysicalDeviceFeatures,
    extensions: &[*const std::ffi::c_char],
    target: &metal_translate::TargetProfile,
    capabilities: DeviceCapabilities,
) -> Result<Device, String> {
    let mut v11 = vk::PhysicalDeviceVulkan11Features {
        storage_buffer16_bit_access: target.storage_buffer16_bit_access.into(),
        uniform_and_storage_buffer16_bit_access: target
            .uniform_and_storage_buffer16_bit_access
            .into(),
        storage_push_constant16: target.storage_push_constant16.into(),
        storage_input_output16: target.storage_input_output16.into(),
        variable_pointers_storage_buffer: target.variable_pointers_storage_buffer.into(),
        variable_pointers: target.variable_pointers.into(),
        shader_draw_parameters: target.shader_draw_parameters.into(),
        ..Default::default()
    };
    let mut v12 = vk::PhysicalDeviceVulkan12Features {
        storage_buffer8_bit_access: target.storage_buffer8_bit_access.into(),
        uniform_and_storage_buffer8_bit_access: target
            .uniform_and_storage_buffer8_bit_access
            .into(),
        storage_push_constant8: target.storage_push_constant8.into(),
        shader_float16: target.shader_float16.into(),
        shader_int8: target.shader_int8.into(),
        sampler_mirror_clamp_to_edge: capabilities.sampler_mirror_clamp_to_edge.into(),
        ..Default::default()
    };
    let mut v13 = vk::PhysicalDeviceVulkan13Features {
        shader_demote_to_helper_invocation: target.demote_to_helper_invocation.into(),
        ..Default::default()
    };
    let mut storage16 = vk::PhysicalDevice16BitStorageFeatures {
        storage_buffer16_bit_access: target.storage_buffer16_bit_access.into(),
        uniform_and_storage_buffer16_bit_access: target
            .uniform_and_storage_buffer16_bit_access
            .into(),
        storage_push_constant16: target.storage_push_constant16.into(),
        storage_input_output16: target.storage_input_output16.into(),
        ..Default::default()
    };
    let mut pointers = vk::PhysicalDeviceVariablePointersFeatures {
        variable_pointers_storage_buffer: target.variable_pointers_storage_buffer.into(),
        variable_pointers: target.variable_pointers.into(),
        ..Default::default()
    };
    let mut draw = vk::PhysicalDeviceShaderDrawParametersFeatures {
        shader_draw_parameters: target.shader_draw_parameters.into(),
        ..Default::default()
    };
    let mut storage8 = vk::PhysicalDevice8BitStorageFeatures {
        storage_buffer8_bit_access: target.storage_buffer8_bit_access.into(),
        uniform_and_storage_buffer8_bit_access: target
            .uniform_and_storage_buffer8_bit_access
            .into(),
        storage_push_constant8: target.storage_push_constant8.into(),
        ..Default::default()
    };
    let mut small = vk::PhysicalDeviceShaderFloat16Int8Features {
        shader_float16: target.shader_float16.into(),
        shader_int8: target.shader_int8.into(),
        ..Default::default()
    };
    let mut demote = vk::PhysicalDeviceShaderDemoteToHelperInvocationFeatures {
        shader_demote_to_helper_invocation: target.demote_to_helper_invocation.into(),
        ..Default::default()
    };
    let mut robustness2 = vk::PhysicalDeviceRobustness2FeaturesEXT {
        null_descriptor: capabilities.null_descriptors.into(),
        ..Default::default()
    };
    let mut divisor = vk::PhysicalDeviceVertexAttributeDivisorFeaturesKHR {
        vertex_attribute_instance_rate_divisor: capabilities.vertex_attribute_divisor.into(),
        ..Default::default()
    };
    let mut swapchain_maintenance1 = vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT {
        swapchain_maintenance1: capabilities.swapchain_maintenance1.into(),
        ..Default::default()
    };

    let mut create_info = vk::DeviceCreateInfo::default()
        .queue_create_infos(queues)
        .enabled_features(features)
        .enabled_extension_names(extensions);
    if target.vulkan_version >= (1, 2) {
        create_info = create_info.push_next(&mut v11).push_next(&mut v12);
        if target.vulkan_version >= (1, 3) {
            create_info = create_info.push_next(&mut v13);
        } else if target.demote_to_helper_invocation {
            create_info = create_info.push_next(&mut demote);
        }
    } else {
        create_info = create_info
            .push_next(&mut storage16)
            .push_next(&mut pointers)
            .push_next(&mut draw);
        if target.storage_buffer8_bit_access
            || target.uniform_and_storage_buffer8_bit_access
            || target.storage_push_constant8
        {
            create_info = create_info.push_next(&mut storage8);
        }
        if target.shader_float16 || target.shader_int8 {
            create_info = create_info.push_next(&mut small);
        }
        if target.demote_to_helper_invocation {
            create_info = create_info.push_next(&mut demote);
        }
    }
    if capabilities.null_descriptors {
        create_info = create_info.push_next(&mut robustness2);
    }
    if capabilities.vertex_attribute_divisor {
        create_info = create_info.push_next(&mut divisor);
    }
    if capabilities.swapchain_maintenance1 {
        create_info = create_info.push_next(&mut swapchain_maintenance1);
    }
    // SAFETY: all feature structures and borrowed create-info data live
    // across the call, and every enabled bit came from the adapter query.
    unsafe { instance.create_device(physical, &create_info, None) }
        .map_err(|error| format!("vkCreateDevice failed with {error:?}"))
}

/// Which spelling of mirror-clamp sampling a device offers: the Vulkan 1.2
/// feature bit, or the extension which preceded it. Enabling both would be
/// redundant, and neither means the address mode is rejected.
fn mirror_clamp_support(core_feature: bool, extension: bool) -> (bool, bool) {
    match core_feature {
        true => (true, false),
        false => (false, extension),
    }
}

/// A render pass is keyed by the formats and load/store actions of its
/// attachments: Vulkan bakes into the pass what Metal carries on the
/// descriptor.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RenderPassKey {
    /// The pass's colour attachments, packed. See the MRT gap in
    /// [the known gaps](../../../../docs/metal.md): an unattached render-target
    /// index closes up rather than becoming a `VK_ATTACHMENT_UNUSED`
    /// reference, so a pass which skips an index misroutes the ones after it.
    pub colors: Vec<(vk::Format, vk::AttachmentLoadOp, vk::AttachmentStoreOp)>,
    /// The single-sample attachment each colour is resolved into, where the
    /// Metal store action asks for a resolve, and `None` where it does not.
    /// One entry per colour, because Vulkan's resolve list is positional.
    pub color_resolves: Vec<Option<vk::Format>>,
    pub depth: Option<(vk::Format, vk::AttachmentLoadOp, vk::AttachmentStoreOp)>,
    pub stencil: Option<(vk::AttachmentLoadOp, vk::AttachmentStoreOp)>,
    /// The single-sample attachment the depth/stencil attachment resolves
    /// into. Vulkan carries one for the whole subpass rather than one per
    /// aspect, which is why Metal's two resolve destinations must be the same
    /// texture.
    pub depth_resolve: Option<DepthResolve>,
    pub samples: vk::SampleCountFlags,
}

/// A depth/stencil resolve destination and the mode each aspect resolves
/// with. `NONE` on an aspect is an aspect which is not resolved.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DepthResolve {
    pub format: vk::Format,
    pub depth_mode: vk::ResolveModeFlags,
    pub stencil_mode: vk::ResolveModeFlags,
}

impl Gpu {
    pub fn create(
        instance: &Instance,
        physical: vk::PhysicalDevice,
        target: &metal_translate::TargetProfile,
    ) -> Result<Self, String> {
        // SAFETY: the physical device came from this live instance.
        let families = unsafe { instance.get_physical_device_queue_family_properties(physical) };
        let queue_family = families
            .iter()
            .position(|family| {
                family
                    .queue_flags
                    .contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE)
            })
            .ok_or("the Vulkan device has no graphics and compute queue family")?
            as u32;

        // Vulkan accepts every feature the device reports, and Metal's own
        // feature set is wide: a translated shader may need any of Int64,
        // format-less storage images, cube arrays or BC compression. Enabling
        // what the device already advertises costs nothing and avoids a
        // per-feature list that would drift from the emitter.
        // SAFETY: the physical device belongs to this live instance.
        let features = unsafe { instance.get_physical_device_features(physical) };

        // SAFETY: the physical device belongs to this live instance.
        let extensions = unsafe { instance.enumerate_device_extension_properties(physical) }
            .map_err(|error| format!("vkEnumerateDeviceExtensionProperties failed: {error:?}"))?;
        let has = |name: &std::ffi::CStr| {
            extensions.iter().any(|property| {
                property
                    .extension_name_as_c_str()
                    .is_ok_and(|available| available == name)
            })
        };
        let mut enabled: Vec<*const std::ffi::c_char> = Vec::new();
        let presents = has(ash::khr::swapchain::NAME);
        if presents {
            enabled.push(ash::khr::swapchain::NAME.as_ptr());
        }
        // The device extension requires its surface counterpart on the instance.
        let swapchain_maintenance1 = presents
            && crate::vulkan::surface_maintenance1()
            && has(ash::ext::swapchain_maintenance1::NAME)
            && {
                let mut maintenance = vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT::default();
                let mut features =
                    vk::PhysicalDeviceFeatures2::default().push_next(&mut maintenance);
                // SAFETY: the extension is advertised and both structures
                // live across the query.
                unsafe { instance.get_physical_device_features2(physical, &mut features) };
                maintenance.swapchain_maintenance1 == vk::TRUE
            };
        if swapchain_maintenance1 {
            enabled.push(ash::ext::swapchain_maintenance1::NAME.as_ptr());
        }
        let pushes = has(ash::khr::push_descriptor::NAME);
        if pushes {
            enabled.push(ash::khr::push_descriptor::NAME.as_ptr());
        }
        if target.vulkan_version < (1, 2) {
            if target.storage_buffer8_bit_access
                || target.uniform_and_storage_buffer8_bit_access
                || target.storage_push_constant8
            {
                enabled.push(ash::khr::_8bit_storage::NAME.as_ptr());
            }
            if target.shader_float16 || target.shader_int8 {
                enabled.push(ash::khr::shader_float16_int8::NAME.as_ptr());
            }
        }
        if target.vulkan_version < (1, 3) && target.demote_to_helper_invocation {
            enabled.push(ash::ext::shader_demote_to_helper_invocation::NAME.as_ptr());
        }

        // `MTLSamplerAddressModeMirrorClampToEdge` maps onto an address mode
        // which an unextended Vulkan 1.1 device does not accept. Vulkan 1.2
        // carries it as an optional feature bit, and the extension which
        // preceded it needs no bit at all - only enabling. Prefer the bit,
        // and fall back to the extension where the device reports it instead.
        let mirror_clamp_feature = target.vulkan_version >= (1, 2) && {
            let mut v12 = vk::PhysicalDeviceVulkan12Features::default();
            let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut v12);
            // SAFETY: the device reports Vulkan 1.2 and both structures live
            // across the query.
            unsafe { instance.get_physical_device_features2(physical, &mut features) };
            v12.sampler_mirror_clamp_to_edge == vk::TRUE
        };
        let (mirror_clamp_feature, mirror_clamp_extension) = mirror_clamp_support(
            mirror_clamp_feature,
            has(ash::khr::sampler_mirror_clamp_to_edge::NAME),
        );
        if mirror_clamp_extension {
            enabled.push(ash::khr::sampler_mirror_clamp_to_edge::NAME.as_ptr());
        }

        // Prefer the ratified spelling. The feature structure is an exact KHR
        // alias of the older EXT structure represented by this ash release.
        let robustness2 = if has(ROBUSTNESS_2_KHR) {
            Some(ROBUSTNESS_2_KHR)
        } else if has(ash::ext::robustness2::NAME) {
            Some(ash::ext::robustness2::NAME)
        } else {
            None
        };
        let null_descriptors = robustness2.is_some_and(|_| {
            let mut robustness2 = vk::PhysicalDeviceRobustness2FeaturesEXT::default();
            let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut robustness2);
            // SAFETY: robustness2 is advertised and both structures live
            // across the query.
            unsafe { instance.get_physical_device_features2(physical, &mut features) };
            robustness2.null_descriptor == vk::TRUE
        });
        if null_descriptors {
            enabled.push(
                robustness2
                    .expect("a queried robustness2 extension")
                    .as_ptr(),
            );
        }

        let (max_vertex_attribute_divisor, divisor_non_zero_first_instance) =
            if has(ash::khr::vertex_attribute_divisor::NAME) {
                let mut divisor = vk::PhysicalDeviceVertexAttributeDivisorFeaturesKHR::default();
                let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut divisor);
                // SAFETY: the extension is advertised and both structures live
                // across the query.
                unsafe { instance.get_physical_device_features2(physical, &mut features) };
                if divisor.vertex_attribute_instance_rate_divisor == vk::TRUE {
                    let mut divisor =
                        vk::PhysicalDeviceVertexAttributeDivisorPropertiesKHR::default();
                    let mut properties =
                        vk::PhysicalDeviceProperties2::default().push_next(&mut divisor);
                    // SAFETY: the extension is advertised and both structures
                    // live across the query.
                    unsafe { instance.get_physical_device_properties2(physical, &mut properties) };
                    enabled.push(ash::khr::vertex_attribute_divisor::NAME.as_ptr());
                    (
                        divisor.max_vertex_attrib_divisor,
                        divisor.supports_non_zero_first_instance == vk::TRUE,
                    )
                } else {
                    (0, false)
                }
            } else {
                (0, false)
            };

        // Depth/stencil resolve is properties-only: there is no feature bit to
        // enable, but the render pass which uses it must be created through
        // `vkCreateRenderPass2`, which is core in Vulkan 1.2 and an extension
        // of its own before that.
        let depth_stencil_resolve_core = target.vulkan_version >= (1, 2);
        let depth_stencil_resolve_extension = !depth_stencil_resolve_core
            && has(ash::khr::depth_stencil_resolve::NAME)
            && has(ash::khr::create_renderpass2::NAME);
        let resolve_support = (depth_stencil_resolve_core || depth_stencil_resolve_extension)
            .then(|| {
                if depth_stencil_resolve_extension {
                    enabled.push(ash::khr::create_renderpass2::NAME.as_ptr());
                    enabled.push(ash::khr::depth_stencil_resolve::NAME.as_ptr());
                }
                let mut resolve = vk::PhysicalDeviceDepthStencilResolveProperties::default();
                let mut properties =
                    vk::PhysicalDeviceProperties2::default().push_next(&mut resolve);
                // SAFETY: the properties are core or the extension is
                // advertised, and both structures live across the query.
                unsafe { instance.get_physical_device_properties2(physical, &mut properties) };
                ResolveSupport {
                    depth_modes: resolve.supported_depth_resolve_modes,
                    stencil_modes: resolve.supported_stencil_resolve_modes,
                    independent_none: resolve.independent_resolve_none == vk::TRUE,
                    independent: resolve.independent_resolve == vk::TRUE,
                }
            })
            // A device which reports no mode for either aspect can resolve
            // nothing, and is the same as one without the feature at all.
            .filter(|support| !(support.depth_modes | support.stencil_modes).is_empty());

        let priorities = [1.0f32];
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priorities)];
        let device = create_device(
            instance,
            physical,
            &queues,
            &features,
            &enabled,
            target,
            DeviceCapabilities {
                null_descriptors,
                vertex_attribute_divisor: max_vertex_attribute_divisor != 0,
                swapchain_maintenance1,
                sampler_mirror_clamp_to_edge: mirror_clamp_feature,
            },
        )?;
        // SAFETY: the family was requested above with one queue.
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        // SAFETY: the physical device belongs to this live instance.
        let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
        let properties = unsafe { instance.get_physical_device_properties(physical) };

        let depth_stencil_resolve = resolve_support.map(|support| DepthStencilResolve {
            renderpass2: depth_stencil_resolve_extension
                .then(|| ash::khr::create_renderpass2::Device::new(instance, &device)),
            support,
        });
        let swapchain_api = presents.then(|| ash::khr::swapchain::Device::new(instance, &device));
        let push_descriptor_api =
            pushes.then(|| ash::khr::push_descriptor::Device::new(instance, &device));
        let max_push_descriptors = if pushes {
            let mut push = vk::PhysicalDevicePushDescriptorPropertiesKHR::default();
            let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut push);
            // SAFETY: the physical device belongs to this live instance, and
            // both structures live across the call.
            unsafe { instance.get_physical_device_properties2(physical, &mut properties) };
            push.max_push_descriptors
        } else {
            0
        };
        let null_sampler = if null_descriptors {
            let info = vk::SamplerCreateInfo::default();
            // SAFETY: the default sampler description is valid on every
            // Vulkan device.
            unsafe { device.create_sampler(&info, None) }
                .map_err(|error| format!("vkCreateSampler for nil resources failed: {error:?}"))?
        } else {
            vk::Sampler::null()
        };

        Ok(Self {
            device,
            physical,
            queue_family,
            queue: Mutex::new(queue),
            submission_boundary: Mutex::new(()),
            submissions: crate::submission::Tracker::new(),
            memory,
            limits: properties.limits,
            layouts: Mutex::new(HashMap::new()),
            render_passes: Mutex::new(HashMap::new()),
            immediate: Mutex::new(None),
            buffers: OnceLock::new(),
            timestamp_pools: Mutex::new(Vec::new()),
            timestamp_period: properties.limits.timestamp_period,
            timestamp_valid_bits: families[queue_family as usize].timestamp_valid_bits,
            contexts: Mutex::new(Vec::new()),
            context_budget: ContextBudget::new(),
            descriptor_pools: Mutex::new(Vec::new()),
            upload_chunks: Mutex::new(Vec::new()),
            push_descriptor_api,
            max_push_descriptors,
            null_descriptors,
            sampler_mirror_clamp_to_edge: mirror_clamp_feature || mirror_clamp_extension,
            max_vertex_attribute_divisor,
            divisor_non_zero_first_instance,
            null_sampler,
            swapchain_api,
            swapchain_maintenance1,
            depth_stencil_resolve,
        })
    }
}

/// Metal's `MTLTextureUsage`.
pub const USAGE_SHADER_READ: u64 = 1;
pub const USAGE_SHADER_WRITE: u64 = 2;
pub const USAGE_RENDER_TARGET: u64 = 4;

pub struct GpuBuffer {
    pub buffer: vk::Buffer,
    pub allocation: Allocation,
    pub contents: *mut u8,
    pub length: u64,
}

// SAFETY: `contents` is a permanent mapping of device memory, valid on every
// thread; see `Allocation`.
unsafe impl Send for GpuBuffer {}

pub struct GpuImage {
    pub image: vk::Image,
    pub memory: Option<vk::DeviceMemory>,
    pub view: vk::ImageView,
    pub format: vk::Format,
    pub aspect: vk::ImageAspectFlags,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub levels: u32,
    pub layers: u32,
    pub samples: vk::SampleCountFlags,
}

impl GpuBuffer {
    /// The buffer left behind when a real one is destroyed: it is never bound,
    /// because the object holding it is being deallocated.
    pub fn none() -> Self {
        Self {
            buffer: vk::Buffer::null(),
            allocation: Allocation::none(),
            contents: std::ptr::null_mut(),
            length: 0,
        }
    }
}

/// One subresource region of an image, as a Metal copy names it.
///
/// Every field is a guest `NSUInteger`, so none of them is known to fit a
/// Vulkan `uint32_t` - let alone to name texels the image has - until
/// [`GpuImage::region`] has said so.
#[derive(Clone, Copy)]
pub struct Region {
    pub level: u64,
    pub slice: u64,
    pub origin: [u64; 3],
    pub size: [u64; 3],
}

/// A region checked against the image it names.
#[derive(Clone, Copy)]
pub struct CheckedRegion {
    pub level: u32,
    pub slice: u32,
    pub origin: [u32; 3],
    pub size: [u32; 3],
}

impl GpuImage {
    /// The extent of one mip level, which is the base extent halved `level`
    /// times and never below one texel.
    pub fn level_extent(&self, level: u32) -> [u32; 3] {
        [self.width, self.height, self.depth].map(|base| (base >> level.min(31)).max(1))
    }

    /// Check one Metal copy region against this image.
    ///
    /// `Err` names what is wrong, for a caller which turns it into a Metal
    /// diagnostic. Nothing below it reaches an `ash` call: a mip level, an
    /// array slice or an extent the image does not have is undefined
    /// behaviour in Vulkan, and every one of them is guest-controlled.
    pub fn region(&self, region: Region) -> Result<CheckedRegion, String> {
        let Region {
            level,
            slice,
            origin,
            size,
        } = region;
        if level >= u64::from(self.levels) {
            return Err(format!(
                "mipmap level {level} of a texture with {} levels",
                self.levels
            ));
        }
        if slice >= u64::from(self.layers) {
            return Err(format!(
                "slice {slice} of a texture with {} slices",
                self.layers
            ));
        }
        let level = level as u32;
        // The bound is the level rounded up to whole blocks, not the level.
        // A compressed level below one block - the 2x2 and 1x1 tail of any BC
        // mip chain - still occupies a full 4x4 block, and both Metal and
        // Vulkan let a copy that reaches the edge of the level cover it - Metal
        // by naming the whole block, Vulkan by stopping at the edge. A
        // check against the level itself refuses every BC mip tail a game
        // uploads, which is most of its textures.
        let (block_width, block_height) = block_extent(self.format);
        let blocks = [block_width, block_height, 1];
        let extent = self.level_extent(level);
        let bound: [u32; 3] =
            std::array::from_fn(|axis| extent[axis].next_multiple_of(blocks[axis]));
        let mut checked = CheckedRegion {
            level,
            slice: slice as u32,
            origin: [0; 3],
            size: [1; 3],
        };
        for axis in 0..3 {
            let start = origin[axis];
            // A Metal size of zero on an axis means one texel of it, which is
            // how a 2D copy spells its depth.
            let span = size[axis].max(1);
            let end = start.checked_add(span).ok_or("a region which overflows")?;
            if end > u64::from(bound[axis]) {
                return Err(format!(
                    "a region ending at {end} on axis {axis}, where level {level} is {} texels \
                     and {} with its blocks",
                    extent[axis], bound[axis]
                ));
            }
            // Vulkan covers a partial block by reaching the level's edge, not
            // by overhanging it, so the extent stops at the level.
            let inside = u64::from(extent[axis]).saturating_sub(start);
            if inside == 0 {
                return Err(format!(
                    "a region starting at {start} on axis {axis}, past level {level}'s {} texels",
                    extent[axis]
                ));
            }
            checked.origin[axis] = start as u32;
            checked.size[axis] = span.min(inside) as u32;
        }
        Ok(checked)
    }

    /// The image left behind when a drawable's own image is destroyed. It is
    /// never bound: the texture it belongs to is unreachable by then.
    pub fn none() -> Self {
        Self {
            image: vk::Image::null(),
            memory: None,
            view: vk::ImageView::null(),
            format: vk::Format::UNDEFINED,
            aspect: vk::ImageAspectFlags::COLOR,
            width: 0,
            height: 0,
            depth: 1,
            levels: 1,
            layers: 1,
            samples: vk::SampleCountFlags::TYPE_1,
        }
    }
}

pub struct ImageDescription {
    pub image_type: vk::ImageType,
    pub view_type: vk::ImageViewType,
    pub format: vk::Format,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub levels: u32,
    pub layers: u32,
    pub samples: u64,
    pub usage: u64,
    pub cube: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, levels: u32, layers: u32) -> GpuImage {
        GpuImage {
            width,
            height,
            levels,
            layers,
            ..GpuImage::none()
        }
    }

    fn image2d(width: u32, height: u32, levels: u32) -> GpuImage {
        image(width, height, levels, 1)
    }

    fn region(level: u64, slice: u64, origin: [u64; 3], size: [u64; 3]) -> Region {
        Region {
            level,
            slice,
            origin,
            size,
        }
    }

    #[test]
    fn mirror_clamp_prefers_the_core_feature_over_the_extension() {
        assert_eq!(mirror_clamp_support(true, true), (true, false));
        assert_eq!(mirror_clamp_support(true, false), (true, false));
    }

    #[test]
    fn mirror_clamp_falls_back_to_the_extension() {
        assert_eq!(mirror_clamp_support(false, true), (false, true));
    }

    #[test]
    fn mirror_clamp_is_unsupported_without_either_spelling() {
        assert_eq!(mirror_clamp_support(false, false), (false, false));
    }

    #[test]
    fn a_region_inside_its_level_is_accepted_and_narrowed() {
        let image = image(64, 32, 4, 6);
        let checked = image
            .region(region(2, 5, [4, 2, 0], [8, 4, 0]))
            .expect("inside level two");
        assert_eq!(checked.level, 2);
        assert_eq!(checked.slice, 5);
        assert_eq!(checked.origin, [4, 2, 0]);
        // A Metal size of zero on an axis is one texel of it.
        assert_eq!(checked.size, [8, 4, 1]);
    }

    #[test]
    fn a_region_a_texture_cannot_hold_is_refused_rather_than_truncated() {
        let image = image(64, 32, 4, 6);
        // Level two of a 64x32 texture is 16x8.
        assert!(image.region(region(2, 0, [0, 0, 0], [17, 8, 1])).is_err());
        assert!(image.region(region(4, 0, [0, 0, 0], [1, 1, 1])).is_err());
        assert!(image.region(region(0, 6, [0, 0, 0], [1, 1, 1])).is_err());
        assert!(image.region(region(0, 0, [60, 0, 0], [8, 1, 1])).is_err());
        // An origin and a size which overflow rather than exceed.
        assert!(
            image
                .region(region(0, 0, [u64::MAX, 0, 0], [2, 1, 1]))
                .is_err()
        );
    }

    #[test]
    fn a_compressed_mip_tail_may_cover_its_whole_block() {
        let mut image = image(16, 16, 5, 1);
        image.format = vk::Format::BC3_UNORM_BLOCK;
        // Level three is 2x2 texels and level four 1x1, and a BC3 upload
        // names the 4x4 block each of them sits in.
        // Vulkan is handed the level's own extent, which reaches its edge.
        for (level, texels) in [(3, 2), (4, 1)] {
            let checked = image
                .region(region(level, 0, [0, 0, 0], [4, 4, 1]))
                .expect("a whole block of the mip tail");
            assert_eq!(checked.size, [texels, texels, 1]);
        }
        // One block, and no further.
        assert!(image.region(region(4, 0, [0, 0, 0], [8, 4, 1])).is_err());
        // An uncompressed texture of the same shape is bounded by the level.
        assert!(
            image2d(16, 16, 5)
                .region(region(4, 0, [0, 0, 0], [4, 4, 1]))
                .is_err()
        );
    }

    #[test]
    fn a_mip_level_is_never_narrower_than_one_texel() {
        assert_eq!(image(8, 1, 8, 1).level_extent(7), [1, 1, 1]);
    }
}
