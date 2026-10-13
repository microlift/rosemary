use super::*;

pub(crate) struct ComputePipeline {
    pub gpu: &'static Gpu,
    pub device: objc::object::Id,
    pub module: vk::ShaderModule,
    pub name: std::ffi::CString,
    pub layouts: Layouts,
    pub resources: Vec<ResourceBinding>,
    /// One Vulkan pipeline per threadgroup size the guest has dispatched.
    variants: Mutex<HashMap<[u32; 3], vk::Pipeline>>,
    /// `ROSEMARY_METAL_PROFILE`: what its dispatches are totalled under.
    pub profile_label: Option<std::sync::Arc<str>>,
}

impl ComputePipeline {
    pub fn new(
        gpu: &'static Gpu,
        device: objc::object::Id,
        entry: &EntryArtifact,
    ) -> Result<Self, String> {
        let module = gpu.create_shader_module(&entry.words, &entry.name)?;
        let layouts = layouts_for(gpu, &[&entry.resources])?;
        Ok(Self {
            gpu,
            device,
            module,
            name: std::ffi::CString::new(entry.name.as_str())
                .map_err(|_| "entry point name contains a NUL".to_string())?,
            layouts,
            resources: entry.resources.clone(),
            variants: Mutex::new(HashMap::new()),
            profile_label: crate::profile::pipeline_label(&[entry]),
        })
    }

    /// The Vulkan pipeline for one Metal threadgroup size.
    pub fn variant(&self, size: [u32; 3]) -> Result<vk::Pipeline, String> {
        let mut cache = self.variants.lock().expect("compute pipeline cache");
        if let Some(pipeline) = cache.get(&size) {
            return Ok(*pipeline);
        }
        let entries = metal_translate::WORKGROUP_SIZE_SPEC_IDS
            .iter()
            .enumerate()
            .map(|(index, id)| {
                vk::SpecializationMapEntry::default()
                    .constant_id(*id)
                    .offset((index * size_of::<u32>()) as u32)
                    .size(size_of::<u32>())
            })
            .collect::<Vec<_>>();
        let data: [u8; 12] = unsafe { std::mem::transmute(size) };
        let specialization = vk::SpecializationInfo::default()
            .map_entries(&entries)
            .data(&data);
        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(self.module)
            .name(&self.name)
            .specialization_info(&specialization);
        let info = vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(self.layouts.pipeline);
        // Said before the call, not after: a driver's shader compiler is where a
        // bad module is met, and one that dies there leaves nothing to say it.
        gpu::trace(format!(
            "compiling compute pipeline {} for threadgroup {size:?}",
            self.name.to_string_lossy()
        ));
        // SAFETY: every borrowed structure lives across the call.
        let pipeline = crate::stats::pipeline(|| unsafe {
            self.gpu
                .device
                .create_compute_pipelines(vk::PipelineCache::null(), &[info], None)
        })
        .map_err(|(_, error)| format!("vkCreateComputePipelines failed with {error:?}"))?[0];
        cache.insert(size, pipeline);
        Ok(pipeline)
    }
}

pub(super) unsafe extern "C" fn compute_pipeline_device(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a compute pipeline state of this crate's.
    unsafe { object::body::<ComputePipeline>(receiver) }.device
}

pub(super) unsafe extern "C" fn compute_pipeline_thread_width(receiver: Id, _sel: Sel) -> u64 {
    let state = unsafe { object::body::<ComputePipeline>(receiver) };
    u64::from(state.gpu.limits.max_compute_work_group_invocations.min(32))
}

pub(super) unsafe extern "C" fn compute_pipeline_max_threads(receiver: Id, _sel: Sel) -> u64 {
    let state = unsafe { object::body::<ComputePipeline>(receiver) };
    u64::from(state.gpu.limits.max_compute_work_group_invocations)
}

/// `-[MTLComputePipelineState dealloc]`.
///
/// Every command buffer which bound this state retained it, and released it
/// when its submission retired, so the last reference to go is never one the
/// device is still running.
pub(super) unsafe extern "C" fn compute_pipeline_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a compute pipeline state of this crate's.
    let state = unsafe { object::body::<ComputePipeline>(receiver) };
    if !crate::resource::leaking() {
        let variants = std::mem::take(&mut *state.variants.lock().expect("compute pipeline cache"));
        destroy_variants(state.gpu, variants.into_values());
        state.layouts.destroy(state.gpu);
        unsafe {
            state.gpu.device.destroy_shader_module(
                std::mem::replace(&mut state.module, vk::ShaderModule::null()),
                None,
            )
        };
    }
    objc::arc::release(std::mem::replace(&mut state.device, std::ptr::null_mut()));
    // SAFETY: the body was established by `object::make` and is not read again.
    unsafe { std::ptr::drop_in_place(state as *mut ComputePipeline) };
    objc::arc::finalize(receiver);
}
