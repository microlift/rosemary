use super::*;

impl Gpu {
    /// A pool of two timestamps for one command buffer, or a null handle where
    /// the device cannot time this queue.
    pub fn timestamp_pool(&self) -> vk::QueryPool {
        if self.timestamp_period == 0.0 || self.timestamp_valid_bits == 0 {
            return vk::QueryPool::null();
        }
        let mut pools = self.timestamp_pools.lock().expect("timestamp pools");
        if let Some(pool) = pools.pop() {
            return pool;
        }
        drop(pools);
        let info = vk::QueryPoolCreateInfo::default()
            .query_type(vk::QueryType::TIMESTAMP)
            .query_count(2);
        // SAFETY: a well-formed create info on this live device.
        unsafe { self.device.create_query_pool(&info, None) }.unwrap_or(vk::QueryPool::null())
    }

    /// Hand a pool back once its command buffer's work has completed.
    pub fn release_timestamp_pool(&self, pool: vk::QueryPool) {
        if pool != vk::QueryPool::null() {
            self.timestamp_pools
                .lock()
                .expect("timestamp pools")
                .push(pool);
        }
    }

    /// A pool of `OCCLUSION_QUERIES` occlusion queries, every one reset.
    pub fn acquire_occlusion_pool(&self) -> Result<vk::QueryPool, String> {
        if !self.host_query_reset {
            return Err("occlusion queries need hostQueryReset, which this device lacks".into());
        }
        let recycled = self.occlusion_pools.lock().expect("occlusion pools").pop();
        let pool = match recycled {
            Some(pool) => pool,
            None => {
                let info = vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::OCCLUSION)
                    .query_count(OCCLUSION_QUERIES);
                // SAFETY: a well-formed create info on this live device.
                unsafe { self.device.create_query_pool(&info, None) }
                    .map_err(|error| format!("vkCreateQueryPool failed with {error:?}"))?
            }
        };
        // SAFETY: the pool is fresh or its last submission has retired, so no
        // pending work uses it; the feature was enabled with the device.
        unsafe { self.device.reset_query_pool(pool, 0, OCCLUSION_QUERIES) };
        Ok(pool)
    }

    /// Hand back an occlusion query pool no pending submission still uses.
    pub fn release_occlusion_pool(&self, pool: vk::QueryPool) {
        let mut pools = self.occlusion_pools.lock().expect("occlusion pools");
        if pools.len() < RECYCLED {
            pools.push(pool);
            return;
        }
        drop(pools);
        // SAFETY: nothing pending uses it.
        unsafe { self.device.destroy_query_pool(pool, None) };
    }

    /// A command buffer ready to begin, recycled where one has retired.
    pub fn acquire_context(&self) -> Result<CommandContext, String> {
        let mut used = self
            .context_budget
            .used
            .lock()
            .expect("command buffer budget");
        while *used >= self.context_budget.limit {
            crate::stats::count(crate::stats::Counter::ContextPressure);
            used = self
                .context_budget
                .available
                .wait(used)
                .expect("command buffer budget");
        }
        *used += 1;
        crate::stats::peak(crate::stats::Counter::InFlightContexts, *used as u64);
        drop(used);

        let acquired = self.acquire_context_unbounded();
        if acquired.is_err() {
            self.release_context_budget();
        }
        acquired
    }

    pub(super) fn acquire_context_unbounded(&self) -> Result<CommandContext, String> {
        if let Some(context) = self.contexts.lock().expect("command contexts").pop() {
            return Ok(context);
        }
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(self.queue_family)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT);
        // SAFETY: a well-formed create info on this live device.
        let pool = unsafe { self.device.create_command_pool(&pool_info, None) }
            .map_err(|error| format!("vkCreateCommandPool failed with {error:?}"))?;
        let allocate = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let destroy_pool = || {
            // SAFETY: nothing allocated from the pool is in use.
            unsafe { self.device.destroy_command_pool(pool, None) }
        };
        // SAFETY: the pool was just created by this device.
        let command_buffer = match unsafe { self.device.allocate_command_buffers(&allocate) } {
            Ok(buffers) => buffers[0],
            Err(error) => {
                destroy_pool();
                return Err(format!("vkAllocateCommandBuffers failed with {error:?}"));
            }
        };
        // SAFETY: a well-formed create info on this live device.
        let fence = match unsafe {
            self.device
                .create_fence(&vk::FenceCreateInfo::default(), None)
        } {
            Ok(fence) => fence,
            Err(error) => {
                destroy_pool();
                return Err(format!("vkCreateFence failed with {error:?}"));
            }
        };
        Ok(CommandContext {
            pool,
            command_buffer,
            fence,
        })
    }

    /// Hand back a command buffer which is not pending on the queue: its work
    /// has completed, or it was never submitted.
    pub fn release_context(&self, context: CommandContext) {
        self.recycle_context(context);
        self.release_context_budget();
    }

    /// Keep or destroy a context, outside the command-buffer budget.
    pub(super) fn recycle_context(&self, context: CommandContext) {
        // SAFETY: the caller guarantees nothing allocated from the pool is
        // pending, and the pool is not in use on any other thread.
        let reset = unsafe {
            self.device
                .reset_command_pool(context.pool, vk::CommandPoolResetFlags::empty())
        };
        let mut contexts = self.contexts.lock().expect("command contexts");
        if reset.is_ok() && contexts.len() < RECYCLED {
            contexts.push(context);
            return;
        }
        drop(contexts);
        unsafe {
            self.device.destroy_command_pool(context.pool, None);
            self.device.destroy_fence(context.fence, None);
        }
    }

    /// A secondary command buffer ready to begin, recycled where one has
    /// retired. Its pool is its own, so a sub-encoder records on any thread.
    pub fn acquire_secondary(&self) -> Result<SecondaryContext, String> {
        if let Some(context) = self.secondaries.lock().expect("secondary contexts").pop() {
            return Ok(context);
        }
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(self.queue_family)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT);
        // SAFETY: a well-formed create info on this live device.
        let pool = unsafe { self.device.create_command_pool(&pool_info, None) }
            .map_err(|error| format!("vkCreateCommandPool failed with {error:?}"))?;
        let allocate = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::SECONDARY)
            .command_buffer_count(1);
        // SAFETY: the pool was just created by this device.
        match unsafe { self.device.allocate_command_buffers(&allocate) } {
            Ok(buffers) => Ok(SecondaryContext {
                pool,
                command_buffer: buffers[0],
            }),
            Err(error) => {
                // SAFETY: nothing allocated from the pool is in use.
                unsafe { self.device.destroy_command_pool(pool, None) };
                Err(format!("vkAllocateCommandBuffers failed with {error:?}"))
            }
        }
    }

    /// Hand back a secondary command buffer whose primary is not pending.
    pub fn release_secondary(&self, context: SecondaryContext) {
        // SAFETY: the caller guarantees nothing from the pool is pending.
        let reset = unsafe {
            self.device
                .reset_command_pool(context.pool, vk::CommandPoolResetFlags::empty())
        };
        let mut secondaries = self.secondaries.lock().expect("secondary contexts");
        if reset.is_ok() && secondaries.len() < RECYCLED {
            secondaries.push(context);
            return;
        }
        drop(secondaries);
        // SAFETY: as above.
        unsafe { self.device.destroy_command_pool(context.pool, None) };
    }

    fn release_context_budget(&self) {
        let mut used = self
            .context_budget
            .used
            .lock()
            .expect("command buffer budget");
        *used = used.checked_sub(1).expect("a reserved command buffer");
        self.context_budget.available.notify_one();
    }

    /// An empty descriptor pool, recycled where one has retired.
    pub fn acquire_descriptor_pool(&self) -> Result<vk::DescriptorPool, String> {
        if let Some(pool) = self
            .descriptor_pools
            .lock()
            .expect("descriptor pools")
            .pop()
        {
            return Ok(pool);
        }
        const CAPACITY: u32 = 512;
        let sizes = [
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(CAPACITY * 4),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_BUFFER_DYNAMIC)
                .descriptor_count(CAPACITY * 4),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::SAMPLED_IMAGE)
                .descriptor_count(CAPACITY * 4),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(CAPACITY),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::SAMPLER)
                .descriptor_count(CAPACITY * 2),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::UNIFORM_TEXEL_BUFFER)
                .descriptor_count(CAPACITY),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_TEXEL_BUFFER)
                .descriptor_count(CAPACITY),
        ];
        let info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(CAPACITY)
            .pool_sizes(&sizes);
        // SAFETY: a well-formed create info on this live device.
        unsafe { self.device.create_descriptor_pool(&info, None) }
            .map_err(|error| format!("vkCreateDescriptorPool failed with {error:?}"))
    }

    /// Hand back a descriptor pool none of whose sets a pending submission
    /// still reads.
    pub fn release_descriptor_pool(&self, pool: vk::DescriptorPool) {
        // SAFETY: the caller guarantees no set from the pool is in use.
        let reset = unsafe {
            self.device
                .reset_descriptor_pool(pool, vk::DescriptorPoolResetFlags::empty())
        };
        let mut pools = self.descriptor_pools.lock().expect("descriptor pools");
        if reset.is_ok() && pools.len() < RECYCLED {
            pools.push(pool);
            return;
        }
        drop(pools);
        unsafe { self.device.destroy_descriptor_pool(pool, None) };
    }

    /// A host-visible buffer of [`UPLOAD_CHUNK`] bytes for small copies.
    pub fn acquire_upload_chunk(&self) -> Result<GpuBuffer, String> {
        if let Some(chunk) = self.upload_chunks.lock().expect("upload chunks").pop() {
            return Ok(chunk);
        }
        self.create_buffer(UPLOAD_CHUNK)
    }

    /// Hand back an upload chunk no pending submission still reads.
    pub fn release_upload_chunk(&self, chunk: GpuBuffer) {
        let mut chunks = self.upload_chunks.lock().expect("upload chunks");
        if chunks.len() < RECYCLED / 2 {
            chunks.push(chunk);
            return;
        }
        drop(chunks);
        self.destroy_buffer(chunk);
    }

    pub fn timestamp_period(&self) -> f64 {
        f64::from(self.timestamp_period)
    }

    pub fn timestamp_valid_bits(&self) -> u32 {
        self.timestamp_valid_bits
    }
}
