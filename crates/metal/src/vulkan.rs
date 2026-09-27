use std::sync::OnceLock;

use ash::{Entry, Instance, vk};
use vk::Handle;

use crate::gpu::Gpu;

pub struct Adapter {
    pub handle: usize,
    pub name: String,
    pub recommended_working_set: u64,
    pub target: metal_translate::TargetProfile,
    /// The logical device, created the first time a Metal object needs one.
    /// Enumerating adapters must stay cheap: a guest asks for the device list
    /// long before it renders, and one of the adapters may never be used.
    gpu: OnceLock<Result<Gpu, String>>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AdapterRank {
    device_class: u8,
    device_local_memory: u64,
    max_image_dimension_2d: u32,
}

impl Adapter {
    pub fn gpu(&self) -> Result<&Gpu, &str> {
        match self.gpu.get_or_init(|| {
            let state = STATE.get().expect("adapters were enumerated first");
            let instance = &state.as_ref().expect("this adapter came from it").instance;
            Gpu::create(
                instance,
                vk::PhysicalDevice::from_raw(self.handle as u64),
                &self.target,
            )
        }) {
            Ok(gpu) => Ok(gpu),
            Err(reason) => Err(reason),
        }
    }
}

struct State {
    instance: Instance,
    /// `VK_KHR_surface`, present only where the instance could be created
    /// with the extension - a headless host has no surface extension at all,
    /// and presentation falls back to the readback path there.
    surface_api: Option<ash::khr::surface::Instance>,
    /// `VK_EXT_surface_maintenance1`, which a device's
    /// `VK_EXT_swapchain_maintenance1` requires of its instance.
    surface_maintenance1: bool,
    _entry: Entry,
    adapters: Vec<Adapter>,
}

impl Drop for State {
    fn drop(&mut self) {
        // SAFETY: the instance was created by this state and all objects which
        // depend on it are dropped before the process-wide state.
        unsafe { self.instance.destroy_instance(None) };
    }
}

static STATE: OnceLock<Result<State, String>> = OnceLock::new();

pub fn adapters() -> Result<&'static [Adapter], &'static str> {
    match STATE.get_or_init(initialize) {
        Ok(state) => Ok(&state.adapters),
        Err(reason) => Err(reason),
    }
}

fn initialize() -> Result<State, String> {
    let entry = load_entry()?;
    // A loader predating Vulkan 1.1 returns `None`; this runtime's descriptor
    // ABI cannot operate against it.
    let loader_version = unsafe { entry.try_enumerate_instance_version() }
        .map_err(|error| format!("vkEnumerateInstanceVersion failed with {error:?}"))?
        .unwrap_or(vk::API_VERSION_1_0);
    let instance_version = loader_version.min(vk::API_VERSION_1_3);
    if instance_version < vk::API_VERSION_1_1 {
        return Err("the Vulkan loader does not support Vulkan 1.1".to_string());
    }
    let application = vk::ApplicationInfo::default()
        .application_name(c"rosetta3")
        .application_version(1)
        .engine_name(c"metal_translate")
        .engine_version(1)
        .api_version(instance_version);
    let surface_extensions = surface_extensions(&entry);
    let enabled = surface_extensions
        .iter()
        .map(|name| name.as_ptr())
        .collect::<Vec<_>>();
    let create_info = vk::InstanceCreateInfo::default()
        .application_info(&application)
        .enabled_extension_names(&enabled);
    // SAFETY: the create info, its application info and the extension name
    // pointers - which are static - live through the call, and the default
    // allocator is used.
    let instance = unsafe { entry.create_instance(&create_info, None) }
        .map_err(|error| format!("vkCreateInstance failed with {error:?}"))?;
    let surface_api = surface_extensions
        .contains(&ash::khr::surface::NAME)
        .then(|| ash::khr::surface::Instance::new(&entry, &instance));
    let surface_maintenance1 = surface_extensions.contains(&ash::ext::surface_maintenance1::NAME);

    // SAFETY: `instance` is live and ash owns its loaded instance functions.
    let physical = unsafe { instance.enumerate_physical_devices() }
        .map_err(|error| format!("vkEnumeratePhysicalDevices failed with {error:?}"))?;
    let mut adapters = physical
        .into_iter()
        .filter_map(|handle| {
            // SAFETY: every handle was returned by this live instance.
            let properties = unsafe { instance.get_physical_device_properties(handle) };
            // SAFETY: the physical device belongs to this live instance.
            let features = unsafe { instance.get_physical_device_features(handle) };
            // SAFETY: the physical device belongs to this live instance.
            let memory = unsafe { instance.get_physical_device_memory_properties(handle) };
            let device_extensions =
                unsafe { instance.enumerate_device_extension_properties(handle) }
                    .unwrap_or_default();
            let supports_memory_budget =
                has_extension(&device_extensions, ash::ext::memory_budget::NAME);
            let budget = supports_memory_budget.then(|| {
                let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
                let mut properties =
                    vk::PhysicalDeviceMemoryProperties2::default().push_next(&mut budget);
                // SAFETY: the physical device belongs to this live instance,
                // and both structures live across the call.
                unsafe { instance.get_physical_device_memory_properties2(handle, &mut properties) };
                budget
            });
            let name = properties.device_name_as_c_str().map_or_else(
                |_| "Vulkan Device".to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            let effective_version = properties.api_version.min(instance_version);
            if effective_version < vk::API_VERSION_1_1 {
                trace(format!(
                    "ignoring {name}: the device does not support Vulkan 1.1"
                ));
                return None;
            }
            let target = query_target(
                &instance,
                handle,
                effective_version,
                &device_extensions,
                &features,
                &properties,
            );
            Some((
                adapter_rank(&properties, &memory),
                Adapter {
                    handle: handle.as_raw() as usize,
                    name,
                    recommended_working_set: recommended_working_set(&memory, budget.as_ref()),
                    target,
                    gpu: OnceLock::new(),
                },
            ))
        })
        .collect::<Vec<_>>();
    // Vulkan does not promise any useful enumeration order. Metal's default
    // device is the first adapter below, so prefer dedicated graphics hardware
    // and use capacity as a deterministic performance proxy within each class.
    adapters.sort_by(|(left, _), (right, _)| right.cmp(left));
    let adapters = adapters
        .into_iter()
        .map(|(_, adapter)| adapter)
        .collect::<Vec<_>>();
    if adapters.is_empty() {
        // SAFETY: no child objects were created from the instance.
        unsafe { instance.destroy_instance(None) };
        return Err("Vulkan reported no physical devices".to_string());
    }
    trace(format!(
        "created Vulkan {}.{} instance with {} adapter(s)",
        vk::api_version_major(instance_version),
        vk::api_version_minor(instance_version),
        adapters.len(),
    ));
    Ok(State {
        instance,
        surface_api,
        surface_maintenance1,
        _entry: entry,
        adapters,
    })
}

fn adapter_rank(
    properties: &vk::PhysicalDeviceProperties,
    memory: &vk::PhysicalDeviceMemoryProperties,
) -> AdapterRank {
    let device_class = match properties.device_type {
        vk::PhysicalDeviceType::DISCRETE_GPU => 5,
        vk::PhysicalDeviceType::INTEGRATED_GPU => 4,
        vk::PhysicalDeviceType::VIRTUAL_GPU => 3,
        vk::PhysicalDeviceType::OTHER => 2,
        vk::PhysicalDeviceType::CPU => 1,
        _ => 0,
    };
    let device_local_memory = memory.memory_heaps[..memory.memory_heap_count as usize]
        .iter()
        .filter(|heap| heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
        .map(|heap| heap.size)
        .sum();
    AdapterRank {
        device_class,
        device_local_memory,
        max_image_dimension_2d: properties.limits.max_image_dimension2_d,
    }
}

fn recommended_working_set(
    memory: &vk::PhysicalDeviceMemoryProperties,
    budget: Option<&vk::PhysicalDeviceMemoryBudgetPropertiesEXT<'_>>,
) -> u64 {
    let heaps = &memory.memory_heaps[..memory.memory_heap_count as usize];
    let capacity = heaps
        .iter()
        .filter(|heap| heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
        .map(|heap| heap.size)
        .sum();
    let budget = budget.map_or(0, |budget| {
        heaps
            .iter()
            .enumerate()
            .filter(|(_, heap)| heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
            .map(|(index, _)| budget.heap_budget[index])
            .sum()
    });
    if budget == 0 { capacity } else { budget }
}

/// The surface extensions to create the instance with: `VK_KHR_surface` and
/// every window-system one the loader advertises.
///
/// Presentation needs these enabled on the *instance*, and the instance is
/// created when a guest first asks for the adapter list - which is before it
/// has a window, and so before anything could ask SDL which extensions its
/// video backend wants. Asking the loader instead removes the ordering
/// question entirely: enabling an extension the loader reports is harmless,
/// the list is a superset of whatever SDL will need, and it costs nothing on
/// a host which has none of them.
fn surface_extensions(entry: &Entry) -> Vec<&'static std::ffi::CStr> {
    // SAFETY: the entry owns a loaded Vulkan library.
    let Ok(available) = (unsafe { entry.enumerate_instance_extension_properties(None) }) else {
        return Vec::new();
    };
    let has = |name: &std::ffi::CStr| {
        available.iter().any(|property| {
            property
                .extension_name_as_c_str()
                .is_ok_and(|found| found == name)
        })
    };
    if !has(ash::khr::surface::NAME) {
        return Vec::new();
    }
    let mut wanted = vec![ash::khr::surface::NAME];
    for name in [
        ash::khr::xlib_surface::NAME,
        ash::khr::xcb_surface::NAME,
        ash::khr::wayland_surface::NAME,
        ash::khr::win32_surface::NAME,
        ash::khr::android_surface::NAME,
        ash::ext::metal_surface::NAME,
        ash::mvk::macos_surface::NAME,
    ] {
        if has(name) {
            wanted.push(name);
        }
    }
    if has(ash::khr::get_surface_capabilities2::NAME) && has(ash::ext::surface_maintenance1::NAME) {
        wanted.push(ash::khr::get_surface_capabilities2::NAME);
        wanted.push(ash::ext::surface_maintenance1::NAME);
    }
    wanted
}

/// Whether the instance enabled `VK_EXT_surface_maintenance1`.
pub fn surface_maintenance1() -> bool {
    STATE
        .get()
        .and_then(|state| state.as_ref().ok())
        .is_some_and(|state| state.surface_maintenance1)
}

/// `VK_KHR_surface`'s instance functions, where the instance has it.
pub fn surface_api() -> Option<&'static ash::khr::surface::Instance> {
    STATE.get()?.as_ref().ok()?.surface_api.as_ref()
}

/// The raw `VkInstance`, for SDL to make a surface against.
pub fn instance_handle() -> Option<u64> {
    use vk::Handle;
    Some(STATE.get()?.as_ref().ok()?.instance.handle().as_raw())
}

fn load_entry() -> Result<Entry, String> {
    if let Some(path) = std::env::var_os("ROSETTA3_VULKAN_LIBRARY") {
        // SAFETY: the entry and its library remain owned by the process-wide state.
        return unsafe { Entry::load_from(&path) }.map_err(|error| {
            format!(
                "the Vulkan loader at {:?} could not be opened: {error}",
                path
            )
        });
    }
    // SAFETY: the entry and its library remain owned by the process-wide state.
    unsafe { Entry::load() }
        .map_err(|error| format!("the Vulkan loader could not be opened: {error}"))
}

fn has_extension(extensions: &[vk::ExtensionProperties], name: &std::ffi::CStr) -> bool {
    extensions.iter().any(|property| {
        property
            .extension_name_as_c_str()
            .is_ok_and(|available| available == name)
    })
}

/// Build the translator contract from features queryable through the effective
/// instance/device version. Promoted aggregate structures may only be placed
/// in the chain when that core version is available; older versions use the
/// extension structures which introduced the same feature bits.
fn query_target(
    instance: &Instance,
    physical: vk::PhysicalDevice,
    effective: u32,
    extensions: &[vk::ExtensionProperties],
    core: &vk::PhysicalDeviceFeatures,
    properties: &vk::PhysicalDeviceProperties,
) -> metal_translate::TargetProfile {
    let version = (
        vk::api_version_major(effective) as u8,
        vk::api_version_minor(effective) as u8,
    );
    let mut target = target_profile(version);
    target.max_bound_descriptor_sets = properties.limits.max_bound_descriptor_sets;
    target.shader_int16 = core.shader_int16 == vk::TRUE;
    target.shader_int64 = core.shader_int64 == vk::TRUE;
    target.shader_image_gather_extended = core.shader_image_gather_extended == vk::TRUE;
    target.shader_storage_image_multisample = core.shader_storage_image_multisample == vk::TRUE;
    target.image_cube_array = core.image_cube_array == vk::TRUE;
    target.shader_storage_image_read_without_format =
        core.shader_storage_image_read_without_format == vk::TRUE;
    target.shader_storage_image_write_without_format =
        core.shader_storage_image_write_without_format == vk::TRUE;
    target.shader_clip_distance = core.shader_clip_distance == vk::TRUE;
    target.sample_rate_shading = core.sample_rate_shading == vk::TRUE;
    target.geometry_shader = core.geometry_shader == vk::TRUE;
    target.multi_viewport = core.multi_viewport == vk::TRUE;

    let mut subgroup = vk::PhysicalDeviceSubgroupProperties::default();
    let mut properties2 = vk::PhysicalDeviceProperties2::default().push_next(&mut subgroup);
    // SAFETY: the device belongs to this instance and the chain lives here.
    unsafe { instance.get_physical_device_properties2(physical, &mut properties2) };
    target.group_non_uniform = subgroup
        .supported_stages
        .contains(vk::ShaderStageFlags::COMPUTE)
        && subgroup
            .supported_operations
            .contains(vk::SubgroupFeatureFlags::BASIC);

    if effective >= vk::API_VERSION_1_2 {
        let mut v11 = vk::PhysicalDeviceVulkan11Features::default();
        let mut v12 = vk::PhysicalDeviceVulkan12Features::default();
        if effective >= vk::API_VERSION_1_3 {
            let mut v13 = vk::PhysicalDeviceVulkan13Features::default();
            let mut features = vk::PhysicalDeviceFeatures2::default()
                .push_next(&mut v11)
                .push_next(&mut v12)
                .push_next(&mut v13);
            // SAFETY: every structure in the chain lives across the call.
            unsafe { instance.get_physical_device_features2(physical, &mut features) };
            target.demote_to_helper_invocation = v13.shader_demote_to_helper_invocation == vk::TRUE;
        } else {
            let mut features = vk::PhysicalDeviceFeatures2::default()
                .push_next(&mut v11)
                .push_next(&mut v12);
            // SAFETY: every structure in the chain lives across the call.
            unsafe { instance.get_physical_device_features2(physical, &mut features) };
            if has_extension(
                extensions,
                ash::ext::shader_demote_to_helper_invocation::NAME,
            ) {
                let mut demote =
                    vk::PhysicalDeviceShaderDemoteToHelperInvocationFeatures::default();
                let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut demote);
                // SAFETY: the extension is advertised and the chain lives here.
                unsafe { instance.get_physical_device_features2(physical, &mut features) };
                target.demote_to_helper_invocation =
                    demote.shader_demote_to_helper_invocation == vk::TRUE;
            }
        }
        target.storage_buffer16_bit_access = v11.storage_buffer16_bit_access == vk::TRUE;
        target.uniform_and_storage_buffer16_bit_access =
            v11.uniform_and_storage_buffer16_bit_access == vk::TRUE;
        target.storage_push_constant16 = v11.storage_push_constant16 == vk::TRUE;
        target.storage_input_output16 = v11.storage_input_output16 == vk::TRUE;
        target.variable_pointers_storage_buffer = v11.variable_pointers_storage_buffer == vk::TRUE;
        target.variable_pointers = v11.variable_pointers == vk::TRUE;
        target.shader_draw_parameters = v11.shader_draw_parameters == vk::TRUE;
        target.storage_buffer8_bit_access = v12.storage_buffer8_bit_access == vk::TRUE;
        target.uniform_and_storage_buffer8_bit_access =
            v12.uniform_and_storage_buffer8_bit_access == vk::TRUE;
        target.storage_push_constant8 = v12.storage_push_constant8 == vk::TRUE;
        target.shader_float16 = v12.shader_float16 == vk::TRUE;
        target.shader_int8 = v12.shader_int8 == vk::TRUE;
    } else {
        let mut storage16 = vk::PhysicalDevice16BitStorageFeatures::default();
        let mut pointers = vk::PhysicalDeviceVariablePointersFeatures::default();
        let mut draw = vk::PhysicalDeviceShaderDrawParametersFeatures::default();
        let mut features = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut storage16)
            .push_next(&mut pointers)
            .push_next(&mut draw);
        // SAFETY: these features are core in Vulkan 1.1.
        unsafe { instance.get_physical_device_features2(physical, &mut features) };
        target.storage_buffer16_bit_access = storage16.storage_buffer16_bit_access == vk::TRUE;
        target.uniform_and_storage_buffer16_bit_access =
            storage16.uniform_and_storage_buffer16_bit_access == vk::TRUE;
        target.storage_push_constant16 = storage16.storage_push_constant16 == vk::TRUE;
        target.storage_input_output16 = storage16.storage_input_output16 == vk::TRUE;
        target.variable_pointers_storage_buffer =
            pointers.variable_pointers_storage_buffer == vk::TRUE;
        target.variable_pointers = pointers.variable_pointers == vk::TRUE;
        target.shader_draw_parameters = draw.shader_draw_parameters == vk::TRUE;

        if has_extension(extensions, ash::khr::_8bit_storage::NAME) {
            let mut storage8 = vk::PhysicalDevice8BitStorageFeatures::default();
            let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut storage8);
            // SAFETY: the extension is advertised and the chain lives here.
            unsafe { instance.get_physical_device_features2(physical, &mut features) };
            target.storage_buffer8_bit_access = storage8.storage_buffer8_bit_access == vk::TRUE;
            target.uniform_and_storage_buffer8_bit_access =
                storage8.uniform_and_storage_buffer8_bit_access == vk::TRUE;
            target.storage_push_constant8 = storage8.storage_push_constant8 == vk::TRUE;
        }
        if has_extension(extensions, ash::khr::shader_float16_int8::NAME) {
            let mut small = vk::PhysicalDeviceShaderFloat16Int8Features::default();
            let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut small);
            // SAFETY: the extension is advertised and the chain lives here.
            unsafe { instance.get_physical_device_features2(physical, &mut features) };
            target.shader_float16 = small.shader_float16 == vk::TRUE;
            target.shader_int8 = small.shader_int8 == vk::TRUE;
        }
        if has_extension(
            extensions,
            ash::ext::shader_demote_to_helper_invocation::NAME,
        ) {
            let mut demote = vk::PhysicalDeviceShaderDemoteToHelperInvocationFeatures::default();
            let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut demote);
            // SAFETY: the extension is advertised and the chain lives here.
            unsafe { instance.get_physical_device_features2(physical, &mut features) };
            target.demote_to_helper_invocation =
                demote.shader_demote_to_helper_invocation == vk::TRUE;
        }
    }
    target
}

fn target_profile(version: (u8, u8)) -> metal_translate::TargetProfile {
    let spirv_version = match version {
        (major, minor) if major > 1 || minor >= 3 => (1, 6),
        (1, 2) => (1, 5),
        _ => (1, 3),
    };
    metal_translate::TargetProfile {
        vulkan_version: version,
        spirv_version,
        max_bound_descriptor_sets: 4,
        shader: true,
        shader_int8: false,
        shader_int16: false,
        shader_int64: false,
        shader_float16: false,
        storage_buffer8_bit_access: false,
        uniform_and_storage_buffer8_bit_access: false,
        storage_push_constant8: false,
        storage_buffer16_bit_access: false,
        uniform_and_storage_buffer16_bit_access: false,
        storage_push_constant16: false,
        storage_input_output16: false,
        shader_image_gather_extended: false,
        shader_storage_image_multisample: false,
        image_cube_array: false,
        shader_storage_image_read_without_format: false,
        shader_storage_image_write_without_format: false,
        shader_clip_distance: false,
        shader_draw_parameters: true,
        sample_rate_shading: false,
        geometry_shader: false,
        multi_viewport: false,
        demote_to_helper_invocation: false,
        group_non_uniform: false,
        image_1d: true,
        image_query: true,
        variable_pointers_storage_buffer: false,
        variable_pointers: false,
    }
}

fn trace(message: impl std::fmt::Display) {
    if std::env::var_os("ROSETTA3_TRACE_METAL").is_some() {
        eprintln!("rosetta3: Metal: {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ash_vulkan_version_is_exact() {
        assert_eq!(vk::API_VERSION_1_1, 0x0040_1000);
    }

    #[test]
    fn translator_profile_tracks_the_vulkan_version() {
        assert_eq!(target_profile((1, 1)).spirv_version, (1, 3));
        assert_eq!(target_profile((1, 2)).spirv_version, (1, 5));
        assert_eq!(target_profile((1, 3)).spirv_version, (1, 6));
    }

    #[test]
    fn adapter_rank_prefers_discrete_graphics_over_more_shared_memory() {
        let discrete = properties(vk::PhysicalDeviceType::DISCRETE_GPU, 8192);
        let integrated = properties(vk::PhysicalDeviceType::INTEGRATED_GPU, 16384);
        let discrete_memory = memory(4 * 1024 * 1024 * 1024);
        let integrated_memory = memory(64 * 1024 * 1024 * 1024);

        assert!(
            adapter_rank(&discrete, &discrete_memory)
                > adapter_rank(&integrated, &integrated_memory)
        );
    }

    #[test]
    fn adapter_rank_uses_device_local_memory_within_a_device_class() {
        let properties = properties(vk::PhysicalDeviceType::DISCRETE_GPU, 8192);

        assert!(
            adapter_rank(&properties, &memory(16 * 1024 * 1024 * 1024))
                > adapter_rank(&properties, &memory(8 * 1024 * 1024 * 1024))
        );
    }

    #[test]
    fn working_set_uses_the_device_local_budget() {
        let memory = memory(8 * 1024 * 1024 * 1024);
        let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
        budget.heap_budget[0] = 6 * 1024 * 1024 * 1024;

        assert_eq!(
            recommended_working_set(&memory, Some(&budget)),
            6 * 1024 * 1024 * 1024
        );
    }

    #[test]
    fn working_set_falls_back_to_device_local_capacity() {
        let memory = memory(8 * 1024 * 1024 * 1024);

        assert_eq!(
            recommended_working_set(&memory, None),
            8 * 1024 * 1024 * 1024
        );
    }

    fn properties(
        device_type: vk::PhysicalDeviceType,
        image_dimension: u32,
    ) -> vk::PhysicalDeviceProperties {
        let mut properties = vk::PhysicalDeviceProperties::default();
        properties.device_type = device_type;
        properties.limits.max_image_dimension2_d = image_dimension;
        properties
    }

    fn memory(device_local: u64) -> vk::PhysicalDeviceMemoryProperties {
        let mut memory = vk::PhysicalDeviceMemoryProperties::default();
        memory.memory_heap_count = 1;
        memory.memory_heaps[0].size = device_local;
        memory.memory_heaps[0].flags = vk::MemoryHeapFlags::DEVICE_LOCAL;
        memory
    }

    #[test]
    fn configured_vulkan_runtime_enumerates_an_adapter() {
        if std::env::var_os("ROSETTA3_TEST_VULKAN").is_none() {
            return;
        }
        let adapters = adapters().expect("configured Vulkan runtime");
        assert!(!adapters.is_empty());
        assert!(adapters[0].target.vulkan_version >= (1, 1));
        assert!(adapters[0].target.max_bound_descriptor_sets >= 4);
    }
}
