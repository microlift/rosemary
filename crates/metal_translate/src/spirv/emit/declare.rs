use super::*;

impl Emitter {
    pub(in crate::spirv) fn declare_resources(
        &mut self,
        entry: &EntryPoint,
        function: &Function,
    ) -> Result<Vec<ResourceBinding>, TranslateError> {
        let mut reflected = Vec::new();
        for resource in &entry.resources {
            let parameter = function
                .parameters
                .get(resource.argument as usize)
                .ok_or_else(|| emission("resource refers to a missing function parameter"))?;
            let (variable, storage_class) = match (&resource.kind, &parameter.ty) {
                (ResourceKind::Buffer, Type::Pointer { pointee, .. }) => {
                    let (_block_type, pointer_type) = self.buffer_type(pointee)?;
                    (
                        self.builder.variable(
                            pointer_type,
                            None,
                            StorageClass::StorageBuffer,
                            None,
                        ),
                        StorageClass::StorageBuffer,
                    )
                }
                (ResourceKind::Texture, resource @ Type::Texture(_))
                | (ResourceKind::Sampler, resource @ Type::Sampler) => {
                    let image = self.type_id(resource)?;
                    let pointer =
                        self.builder
                            .type_pointer(None, StorageClass::UniformConstant, image);
                    (
                        self.builder
                            .variable(pointer, None, StorageClass::UniformConstant, None),
                        StorageClass::UniformConstant,
                    )
                }
                (ResourceKind::Buffer, _) => {
                    return Err(emission("buffer resource parameter is not a pointer"));
                }
                (ResourceKind::Texture, _) => {
                    return Err(emission("texture resource parameter is not a texture"));
                }
                (ResourceKind::Sampler, _) => {
                    return Err(emission("sampler resource parameter is not a sampler"));
                }
                _ => {
                    return Err(emission(
                        "resource kind is not implemented by the SPIR-V emitter",
                    ));
                }
            };
            let descriptor_set = descriptor_set(resource.kind);
            let binding = entry.stage.ordinal() * 256 + resource.index;
            self.builder.decorate(
                variable,
                Decoration::DescriptorSet,
                [SpirvOperand::LiteralBit32(descriptor_set)],
            );
            self.builder.decorate(
                variable,
                Decoration::Binding,
                [SpirvOperand::LiteralBit32(binding)],
            );
            match (storage_class, resource.access) {
                // SPIR-V only permits these decorations on memory objects.
                (StorageClass::StorageBuffer, Access::Read) => self.builder.decorate(
                    variable,
                    Decoration::NonWritable,
                    std::iter::empty::<SpirvOperand>(),
                ),
                (StorageClass::StorageBuffer, Access::Write) => self.builder.decorate(
                    variable,
                    Decoration::NonReadable,
                    std::iter::empty::<SpirvOperand>(),
                ),
                _ => (),
            }
            // Before SPIR-V 1.4 the entry-point interface may name only Input
            // and Output variables. Descriptors remain visible to the entry
            // point without appearing in this list.
            if self.spirv_version >= (1, 4) {
                self.interface.push(variable);
            }
            // The parameter pointer is materialized in the entry block after the
            // function has begun. Temporarily retain the descriptor variable.
            self.parameter_pointers.insert(parameter.value, variable);
            reflected.push(ResourceBinding {
                stage: entry.stage,
                kind: resource.kind,
                metal_index: resource.index,
                descriptor_set,
                binding,
                count: resource.count,
                access: resource.access,
                sampled: match &parameter.ty {
                    Type::Texture(texture) => texture.sampled_image(),
                    _ => false,
                },
            });
        }
        Ok(reflected)
    }

    /// Declare `gl_WorkGroupSize` as a specialization constant vector.
    ///
    /// The decoration supersedes the `LocalSize` execution mode, which stays
    /// in the module because a `GLCompute` entry point must declare one.
    pub(in crate::spirv) fn declare_workgroup_size(&mut self) -> Result<(), TranslateError> {
        let uint = self.type_id(&Type::Int {
            bits: 32,
            signed: false,
        })?;
        let vector = self.builder.type_vector(uint, 3);
        let lanes = WORKGROUP_SIZE_SPEC_IDS.map(|spec_id| {
            let lane = self.builder.spec_constant_bit32(uint, 1);
            self.builder.decorate(
                lane,
                Decoration::SpecId,
                [SpirvOperand::LiteralBit32(spec_id)],
            );
            lane
        });
        let size = self.builder.spec_constant_composite(vector, lanes);
        self.builder.decorate(
            size,
            Decoration::BuiltIn,
            [SpirvOperand::BuiltIn(BuiltIn::WorkgroupSize)],
        );
        Ok(())
    }

    pub(super) fn buffer_type(&mut self, pointee: &Type) -> Result<(u32, u32), TranslateError> {
        if let Some(ids) = self.buffer_types.get(pointee) {
            return Ok(*ids);
        }
        self.storage_type(pointee, StorageClass::StorageBuffer)?;
        let element_type = self.type_id(pointee)?;
        let array_type = self.builder.type_runtime_array(element_type);
        self.builder.decorate(
            array_type,
            Decoration::ArrayStride,
            [SpirvOperand::LiteralBit32(type_stride(pointee)?)],
        );
        let block_type = self.builder.id();
        self.builder.type_struct_id(Some(block_type), [array_type]);
        self.builder.decorate(
            block_type,
            Decoration::Block,
            std::iter::empty::<SpirvOperand>(),
        );
        self.builder.member_decorate(
            block_type,
            0,
            Decoration::Offset,
            [SpirvOperand::LiteralBit32(0)],
        );
        let pointer_type = self
            .builder
            .type_pointer(None, StorageClass::StorageBuffer, block_type);
        self.buffer_types
            .insert(pointee.clone(), (block_type, pointer_type));
        Ok((block_type, pointer_type))
    }

    /// Declare Vulkan input and output variables for the entry point's Metal
    /// stage interface. Every binding comes from the entry point; nothing is
    /// inferred from parameter names.
    pub(in crate::spirv) fn declare_stage_io(
        &mut self,
        entry: &EntryPoint,
        function: &Function,
    ) -> Result<(), TranslateError> {
        for input in &entry.inputs {
            let parameter = function
                .parameters
                .get(input.argument as usize)
                .ok_or_else(|| emission("stage input names a missing argument"))?;
            self.storage_type(&parameter.ty, StorageClass::Input)?;
            let ty = match parameter.ty.signed_integer().filter(|_| input.signed) {
                Some(signed) => {
                    let signed = self.type_id(&signed)?;
                    self.signed_inputs.insert(parameter.value, signed);
                    signed
                }
                None => self.type_id(&parameter.ty)?,
            };
            let pointer = self.builder.type_pointer(None, StorageClass::Input, ty);
            let variable = self
                .builder
                .variable(pointer, None, StorageClass::Input, None);
            self.decorate_binding(variable, entry.stage, input.binding)?;
            self.interface.push(variable);
            self.stage_inputs.insert(parameter.value, variable);
            // A kernel reaching the thread position through both a parameter
            // and the built-in instruction must share one Vulkan variable.
            if input.binding == StageBinding::Builtin(Builtin::ThreadPositionInGrid) {
                self.global_invocation_id = Some(variable);
            }
        }
        if !entry.outputs.is_empty() {
            self.return_type = Some(function.result.clone());
        }
        for (position, output) in entry.outputs.iter().enumerate() {
            let ty = output_type(function, output, position)?;
            self.storage_type(&ty, StorageClass::Output)?;
            let ty_id = self.type_id(&ty)?;
            let pointer = self.builder.type_pointer(None, StorageClass::Output, ty_id);
            let variable = self
                .builder
                .variable(pointer, None, StorageClass::Output, None);
            self.decorate_binding(variable, entry.stage, output.binding)?;
            self.interface.push(variable);
            self.stage_outputs.push(variable);
        }
        Ok(())
    }

    /// Attach the Vulkan decoration for one Metal stage binding.
    pub(super) fn decorate_binding(
        &mut self,
        variable: u32,
        stage: Stage,
        binding: StageBinding,
    ) -> Result<(), TranslateError> {
        match binding {
            StageBinding::Location {
                location,
                interpolation,
            } => {
                self.builder.decorate(
                    variable,
                    Decoration::Location,
                    [SpirvOperand::LiteralBit32(location)],
                );
                if let Some(interpolation) = interpolation {
                    if interpolation.flat {
                        self.builder.decorate(variable, Decoration::Flat, []);
                    } else if !interpolation.perspective {
                        self.builder
                            .decorate(variable, Decoration::NoPerspective, []);
                    }
                    match interpolation.sampling {
                        Sampling::Center => {}
                        Sampling::Centroid => {
                            self.builder.decorate(variable, Decoration::Centroid, []);
                        }
                        Sampling::Sample => {
                            self.capability(Capability::SampleRateShading)?;
                            self.builder.decorate(variable, Decoration::Sample, []);
                        }
                    }
                }
            }
            StageBinding::RenderTarget { index } => {
                self.builder.decorate(
                    variable,
                    Decoration::Location,
                    [SpirvOperand::LiteralBit32(index)],
                );
            }
            StageBinding::Builtin(builtin) => {
                let builtin = self.spirv_builtin(builtin, stage)?;
                self.builder.decorate(
                    variable,
                    Decoration::BuiltIn,
                    [SpirvOperand::BuiltIn(builtin)],
                );
            }
        }
        Ok(())
    }

    /// Metal built-ins name a semantic; the SPIR-V built-in depends on the
    /// stage and on whether it is read or written. `[[position]]` is Vulkan's
    /// `Position` output in a vertex shader and `FragCoord` input in a
    /// fragment shader.
    pub(super) fn spirv_builtin(
        &mut self,
        builtin: Builtin,
        stage: Stage,
    ) -> Result<BuiltIn, TranslateError> {
        Ok(match (builtin, stage) {
            (Builtin::Position, Stage::Fragment) => BuiltIn::FragCoord,
            (Builtin::Position, _) => BuiltIn::Position,
            (Builtin::PointSize, _) => BuiltIn::PointSize,
            (Builtin::ClipDistance, _) => {
                self.capability(Capability::ClipDistance)?;
                BuiltIn::ClipDistance
            }
            (Builtin::VertexId, _) => BuiltIn::VertexIndex,
            (Builtin::InstanceId, _) => BuiltIn::InstanceIndex,
            (Builtin::BaseVertex, _) => {
                self.capability(Capability::DrawParameters)?;
                BuiltIn::BaseVertex
            }
            (Builtin::BaseInstance, _) => {
                self.capability(Capability::DrawParameters)?;
                BuiltIn::BaseInstance
            }
            (Builtin::FrontFacing, _) => BuiltIn::FrontFacing,
            (Builtin::PointCoord, _) => BuiltIn::PointCoord,
            (Builtin::SampleId, _) => {
                self.capability(Capability::SampleRateShading)?;
                BuiltIn::SampleId
            }
            (Builtin::SampleMask, _) => BuiltIn::SampleMask,
            (Builtin::Depth, _) => {
                self.writes_depth = true;
                BuiltIn::FragDepth
            }
            (Builtin::RenderTargetArrayIndex, _) => {
                self.capability(Capability::Geometry)?;
                BuiltIn::Layer
            }
            (Builtin::ViewportArrayIndex, _) => {
                self.capability(Capability::MultiViewport)?;
                BuiltIn::ViewportIndex
            }
            (Builtin::ThreadPositionInGrid, _) => BuiltIn::GlobalInvocationId,
            (Builtin::ThreadPositionInThreadgroup, _) => BuiltIn::LocalInvocationId,
            (Builtin::ThreadgroupPositionInGrid, _) => BuiltIn::WorkgroupId,
            (Builtin::ThreadgroupsPerGrid, _) => BuiltIn::NumWorkgroups,
            (Builtin::ThreadIndexInThreadgroup, _) => BuiltIn::LocalInvocationIndex,
            (Builtin::ThreadsPerThreadgroup, _) => BuiltIn::WorkgroupSize,
        })
    }

    pub(in crate::spirv) fn emit_function(
        &mut self,
        entry: &EntryPoint,
        function: &Function,
    ) -> Result<u32, TranslateError> {
        self.collect_constants(function)?;
        let merges = control_flow_merges(function)?;
        if entry.stage == Stage::Kernel {
            self.declare_global_invocation_id()?;
        }
        let void = self.type_id(&Type::Void)?;
        let function_type = self.builder.type_function(void, []);
        let function_id = self.builder.id();
        let block_ids: HashMap<_, _> = function
            .blocks
            .iter()
            .map(|block| (block.id, self.builder.id()))
            .collect();
        let value_ids: HashMap<_, _> = function
            .values
            .iter()
            .enumerate()
            .map(|(index, _)| (ValueId(index as u32), self.builder.id()))
            .collect();
        self.builder
            .begin_function(
                void,
                Some(function_id),
                FunctionControl::NONE,
                function_type,
            )
            .map_err(build_error)?;

        for (block_index, block) in function.blocks.iter().enumerate() {
            self.builder
                .begin_block(Some(block_ids[&block.id]))
                .map_err(build_error)?;
            if block_index == 0 {
                self.materialize_local_variables(function, &value_ids)?;
                self.materialize_parameter_pointers(function, &value_ids)?;
                self.materialize_stage_inputs(function, &value_ids)?;
            }
            for instruction in &block.instructions {
                self.emit_instruction(function, instruction, &block_ids, &value_ids)?;
            }
            match merges.get(&block.id) {
                Some(Merge::Loop {
                    merge,
                    continue_target,
                }) => self
                    .builder
                    .loop_merge(
                        block_ids[merge],
                        block_ids[continue_target],
                        LoopControl::NONE,
                        std::iter::empty::<SpirvOperand>(),
                    )
                    .map_err(build_error)?,
                Some(Merge::Selection { merge }) => self
                    .builder
                    .selection_merge(block_ids[merge], SelectionControl::NONE)
                    .map_err(build_error)?,
                None => {}
            }
            self.emit_terminator(&block.terminator, &block_ids, &value_ids)?;
        }
        self.builder.end_function().map_err(build_error)?;
        Ok(function_id)
    }

    /// Declare every `alloca` in the entry block.
    ///
    /// SPIR-V requires a function's `Function`-storage variables to be in its
    /// first block, whatever block LLVM wrote the `alloca` in.
    pub(super) fn materialize_local_variables(
        &mut self,
        function: &Function,
        values: &HashMap<ValueId, u32>,
    ) -> Result<(), TranslateError> {
        for block in &function.blocks {
            for instruction in &block.instructions {
                let InstructionKind::LocalVariable { ty } = &instruction.kind else {
                    continue;
                };
                let result = instruction
                    .result
                    .ok_or_else(|| emission("a local variable has no result"))?;
                self.arithmetic_type(ty)?;
                let pointee = self.unlaid_type_id(ty)?;
                let pointer = self
                    .builder
                    .type_pointer(None, StorageClass::Function, pointee);
                self.builder
                    .variable(pointer, Some(values[&result]), StorageClass::Function, None);
                self.pointer_storage.insert(result, StorageClass::Function);
            }
        }
        Ok(())
    }

    pub(super) fn materialize_parameter_pointers(
        &mut self,
        function: &Function,
        values: &HashMap<ValueId, u32>,
    ) -> Result<(), TranslateError> {
        let zero = self.constant(&Type::u32(), 0)?;
        for parameter in &function.parameters {
            let Some(variable) = self.parameter_pointers.get(&parameter.value).copied() else {
                continue;
            };
            // A texture or sampler is a handle, not addressable memory: it
            // is loaded from its descriptor rather than indexed into.
            if matches!(parameter.ty, Type::Texture(_) | Type::Sampler) {
                let handle = self.type_id(&parameter.ty)?;
                self.builder
                    .load(handle, Some(values[&parameter.value]), variable, None, [])
                    .map_err(build_error)?;
                continue;
            }
            let Type::Pointer { pointee, .. } = &parameter.ty else {
                return Err(emission("resource parameter is not a pointer"));
            };
            let pointee_id = self.type_id(pointee)?;
            let pointer_type =
                self.builder
                    .type_pointer(None, StorageClass::StorageBuffer, pointee_id);
            self.builder
                .access_chain(
                    pointer_type,
                    Some(values[&parameter.value]),
                    variable,
                    [zero, zero],
                )
                .map_err(build_error)?;
        }
        Ok(())
    }

    pub(super) fn materialize_stage_inputs(
        &mut self,
        function: &Function,
        values: &HashMap<ValueId, u32>,
    ) -> Result<(), TranslateError> {
        for parameter in &function.parameters {
            let Some(variable) = self.stage_inputs.get(&parameter.value).copied() else {
                continue;
            };
            let ty = self.type_id(&parameter.ty)?;
            let result = values[&parameter.value];
            match self.signed_inputs.get(&parameter.value).copied() {
                // The body reads the signless parameter it was written against.
                Some(signed) => {
                    let loaded = self
                        .builder
                        .load(signed, None, variable, None, [])
                        .map_err(build_error)?;
                    self.builder
                        .bitcast(ty, Some(result), loaded)
                        .map_err(build_error)?;
                }
                None => {
                    self.builder
                        .load(ty, Some(result), variable, None, [])
                        .map_err(build_error)?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn declare_global_invocation_id(&mut self) -> Result<(), TranslateError> {
        if self.global_invocation_id.is_some() {
            return Ok(());
        }
        let vector = self.type_id(&Type::Vector {
            element: Box::new(Type::u32()),
            lanes: 3,
        })?;
        let pointer = self.builder.type_pointer(None, StorageClass::Input, vector);
        let variable = self
            .builder
            .variable(pointer, None, StorageClass::Input, None);
        self.builder.decorate(
            variable,
            Decoration::BuiltIn,
            [SpirvOperand::BuiltIn(BuiltIn::GlobalInvocationId)],
        );
        self.interface.push(variable);
        self.global_invocation_id = Some(variable);
        Ok(())
    }

    /// Declare the module's global variables.
    ///
    /// Metal's `constant` tables become `Private` variables with their
    /// initializer, and its `threadgroup` arrays become `Workgroup` variables,
    /// which Vulkan does not let an initializer reach.
    pub(in crate::spirv) fn declare_globals(
        &mut self,
        module: &Module,
    ) -> Result<(), TranslateError> {
        for (index, global) in module.globals.iter().enumerate() {
            let storage = match global.space {
                crate::ir::AddressSpace::Constant | crate::ir::AddressSpace::Thread => {
                    StorageClass::Private
                }
                crate::ir::AddressSpace::Threadgroup
                | crate::ir::AddressSpace::ThreadgroupImageblock => StorageClass::Workgroup,
                crate::ir::AddressSpace::Device => {
                    return Err(emission(
                        "a module global in the device address space has no Vulkan form",
                    ));
                }
            };
            self.arithmetic_type(&global.ty)?;
            let pointee = self.unlaid_type_id(&global.ty)?;
            let pointer = self.builder.type_pointer(None, storage, pointee);
            let initializer = match &global.initializer {
                Some(operand) if storage == StorageClass::Private => {
                    Some(self.constant_operand(operand)?)
                }
                // A `threadgroup` variable is uninitialized in Metal too.
                _ => None,
            };
            let variable = self.builder.variable(pointer, None, storage, initializer);
            if !global.name.is_empty() {
                self.builder.name(variable, global.name.clone());
            }
            // SPIR-V 1.4 widened an entry point's interface from its stage
            // variables to every global one it can reach.
            if self.spirv_version >= (1, 4) {
                self.interface.push(variable);
            }
            self.globals.insert(index as u32, (variable, storage));
        }
        Ok(())
    }
}
