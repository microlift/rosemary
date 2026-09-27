mod arithmetic;
mod composite;
mod convert;
mod declare;
mod intern;
mod memory;
mod sync;
mod texture;

use super::*;

pub(super) struct Emitter {
    pub(super) target: TargetProfile,
    pub(super) variable_pointers_storage_buffer: bool,
    pub(super) variable_pointers: bool,
    pub(super) spirv_version: (u8, u8),
    pub(super) uses_int64: bool,
    /// Whether a pointer has been a phi or a select operand, and in which
    /// storage class: `VariablePointers` is the wider of the two capabilities
    /// and the only one `Workgroup` may use.
    pub(super) uses_variable_pointers: Option<Capability>,
    pub(super) builder: Builder,
    pub(super) types: HashMap<Type, u32>,
    pub(super) constants: HashMap<(Type, u64), u32>,
    pub(super) composites: HashMap<Operand, u32>,
    pub(super) buffer_types: HashMap<Type, (u32, u32)>,
    pub(super) interface: Vec<u32>,
    pub(super) parameter_pointers: HashMap<ValueId, u32>,
    /// Each module global's variable and the storage class it lives in.
    pub(super) globals: HashMap<u32, (u32, StorageClass)>,
    /// The storage class of every pointer an access chain produced, so a
    /// chained access chain keeps the class of the variable it started from.
    pub(super) pointer_storage: HashMap<ValueId, StorageClass>,
    /// Array and structure types outside a buffer. Vulkan forbids explicit
    /// layout decorations there, so those types cannot share the ids the
    /// laid-out buffer types use.
    pub(super) unlaid_types: HashMap<Type, u32>,
    pub(super) unlaid_composites: HashMap<Operand, u32>,
    pub(super) undefs: HashMap<Type, u32>,
    pub(super) global_invocation_id: Option<u32>,
    pub(super) glsl_std_450: u32,
    pub(super) stage_inputs: HashMap<ValueId, u32>,
    /// The signed type a stage input is declared with, where it differs from
    /// its signless parameter.
    pub(super) signed_inputs: HashMap<ValueId, u32>,
    /// Output variables in the order of the entry point's output list; each
    /// receives one component of the returned value.
    pub(super) stage_outputs: Vec<u32>,
    /// The Metal result type of the entry function, needed to decompose a
    /// returned structure into stage outputs.
    pub(super) return_type: Option<Type>,
    /// Vulkan requires a fragment shader which writes depth to say so.
    pub(super) writes_depth: bool,
}

impl Emitter {
    pub(super) fn new(target: &TargetProfile) -> Self {
        let mut builder = Builder::new();
        builder.set_version(target.spirv_version.0, target.spirv_version.1);
        builder.capability(Capability::Shader);
        builder.memory_model(AddressingModel::Logical, MemoryModel::GLSL450);
        let glsl_std_450 = builder.ext_inst_import("GLSL.std.450");
        Self {
            target: target.clone(),
            variable_pointers_storage_buffer: target.variable_pointers_storage_buffer,
            variable_pointers: target.variable_pointers,
            spirv_version: target.spirv_version,
            uses_int64: false,
            uses_variable_pointers: None,
            builder,
            types: HashMap::new(),
            constants: HashMap::new(),
            composites: HashMap::new(),
            buffer_types: HashMap::new(),
            interface: Vec::new(),
            parameter_pointers: HashMap::new(),
            globals: HashMap::new(),
            pointer_storage: HashMap::new(),
            unlaid_types: HashMap::new(),
            unlaid_composites: HashMap::new(),
            undefs: HashMap::new(),
            global_invocation_id: None,
            glsl_std_450,
            stage_inputs: HashMap::new(),
            signed_inputs: HashMap::new(),
            stage_outputs: Vec::new(),
            return_type: None,
            writes_depth: false,
        }
    }

    /// Declare a capability only when the negotiated Vulkan target admits it.
    pub(super) fn capability(&mut self, capability: Capability) -> Result<(), TranslateError> {
        if !self.target.supports_capability(&format!("{capability:?}")) {
            return Err(emission(format!(
                "target profile does not support {capability:?}"
            )));
        }
        self.builder.capability(capability);
        Ok(())
    }

    /// Admit a small scalar used for calculations rather than only as the
    /// element of an interface storage object.
    pub(super) fn arithmetic_type(&mut self, ty: &Type) -> Result<(), TranslateError> {
        match ty {
            Type::Int { bits: 8, .. } => self.capability(Capability::Int8),
            Type::Int { bits: 16, .. } => self.capability(Capability::Int16),
            Type::Float { bits: 16 } => self.capability(Capability::Float16),
            Type::Vector { element, .. } | Type::Array { element, .. } => {
                self.arithmetic_type(element)
            }
            Type::Struct(structure) => {
                for field in &structure.fields {
                    self.arithmetic_type(&field.ty)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Declare the capability which permits a small type in an interface.
    /// These capabilities deliberately do not imply small-width arithmetic.
    pub(super) fn storage_type(
        &mut self,
        ty: &Type,
        storage: StorageClass,
    ) -> Result<(), TranslateError> {
        let mut bits = BTreeSet::new();
        small_storage_widths(ty, &mut bits);
        for bits in bits {
            let capability = match (bits, storage) {
                (8, StorageClass::StorageBuffer) => Capability::StorageBuffer8BitAccess,
                (8, StorageClass::Uniform) => Capability::UniformAndStorageBuffer8BitAccess,
                (8, StorageClass::PushConstant) => Capability::StoragePushConstant8,
                (8, _) => {
                    return Err(emission(format!(
                        "8-bit interface values are not supported in {storage:?} storage"
                    )));
                }
                (16, StorageClass::StorageBuffer) => Capability::StorageBuffer16BitAccess,
                (16, StorageClass::Uniform) => Capability::UniformAndStorageBuffer16BitAccess,
                (16, StorageClass::PushConstant) => Capability::StoragePushConstant16,
                (16, StorageClass::Input | StorageClass::Output) => {
                    Capability::StorageInputOutput16
                }
                _ => continue,
            };
            self.capability(capability)?;
            if bits == 8 && self.spirv_version < (1, 5) {
                self.builder.extension("SPV_KHR_8bit_storage");
            }
        }
        Ok(())
    }

    pub(super) fn emit_instruction(
        &mut self,
        function: &Function,
        instruction: &crate::ir::Instruction,
        blocks: &HashMap<BlockId, u32>,
        values: &HashMap<ValueId, u32>,
    ) -> Result<(), TranslateError> {
        let result_id = instruction.result.map(|result| values[&result]);
        if !matches!(
            instruction.kind,
            InstructionKind::LocalVariable { .. }
                | InstructionKind::Load { .. }
                | InstructionKind::Store { .. }
                | InstructionKind::ElementAddress { .. }
                | InstructionKind::BufferAddress { .. }
                | InstructionKind::FieldAddress { .. }
                | InstructionKind::Convert { .. }
        ) {
            if let Some(result) = instruction.result {
                self.arithmetic_type(&function.values[result.0 as usize])?;
            }
            for operand in instruction_operands(&instruction.kind) {
                if let Some(ty) = operand_type(function, operand) {
                    self.arithmetic_type(&ty)?;
                }
            }
        }
        match &instruction.kind {
            InstructionKind::LocalVariable { .. }
            | InstructionKind::Load { .. }
            | InstructionKind::Store { .. }
            | InstructionKind::ElementAddress { .. }
            | InstructionKind::BufferAddress { .. }
            | InstructionKind::FieldAddress { .. } => {
                self.emit_memory(function, instruction, values, result_id)
            }
            InstructionKind::IntegerBinary { .. }
            | InstructionKind::IntegerClamp { .. }
            | InstructionKind::IntegerUnary { .. }
            | InstructionKind::IntegerCompare { .. }
            | InstructionKind::FloatCompare { .. }
            | InstructionKind::FloatBinary { .. }
            | InstructionKind::FloatUnary { .. }
            | InstructionKind::FloatFma { .. }
            | InstructionKind::FloatDot { .. }
            | InstructionKind::FloatClamp { .. }
            | InstructionKind::FloatMix { .. } => {
                self.emit_arithmetic(function, instruction, values, result_id)
            }
            InstructionKind::Phi { .. }
            | InstructionKind::CompositeExtract { .. }
            | InstructionKind::CompositeInsert { .. }
            | InstructionKind::VectorExtractDynamic { .. }
            | InstructionKind::VectorInsertDynamic { .. }
            | InstructionKind::VectorShuffle { .. }
            | InstructionKind::VectorSplat { .. }
            | InstructionKind::All { .. }
            | InstructionKind::Any { .. }
            | InstructionKind::Select { .. } => {
                self.emit_composite(function, instruction, blocks, values, result_id)
            }
            InstructionKind::Convert { .. } => {
                self.emit_convert(function, instruction, values, result_id)
            }
            InstructionKind::TextureWrite { .. }
            | InstructionKind::TextureSample { .. }
            | InstructionKind::TextureRead { .. }
            | InstructionKind::TextureQuerySize { .. } => {
                self.emit_texture(function, instruction, values, result_id)
            }
            InstructionKind::GlobalInvocationId
            | InstructionKind::DiscardFragment
            | InstructionKind::Barrier { .. }
            | InstructionKind::Atomic { .. }
            | InstructionKind::FunctionConstantDefined { .. } => {
                self.emit_sync(function, instruction, values, result_id)
            }
        }
    }

    /// Build an SSA value's type, resolving an ambiguous Metal pointer address
    /// space from the values which reach it. A `constant` pointer can address
    /// either a buffer or a module-private lookup table, while SPIR-V includes
    /// that distinction in the pointer type itself.
    fn value_type_id<'a>(
        &mut self,
        function: &Function,
        result: ValueId,
        operands: impl Iterator<Item = &'a Operand>,
        what: &str,
    ) -> Result<u32, TranslateError> {
        let ty = &function.values[result.0 as usize];
        let Type::Pointer { space, pointee } = ty else {
            return self.type_id(ty);
        };
        let mut storage = match space {
            AddressSpace::Device => Some(StorageClass::StorageBuffer),
            AddressSpace::Threadgroup | AddressSpace::ThreadgroupImageblock => {
                Some(StorageClass::Workgroup)
            }
            AddressSpace::Thread => Some(StorageClass::Function),
            AddressSpace::Constant => None,
        };
        for operand in operands {
            let candidate = self.pointer_operand_storage(operand)?;
            if storage.is_some_and(|existing| existing != candidate) {
                return Err(emission(format!(
                    "a {what} combines pointers from different Vulkan storage classes"
                )));
            }
            storage = Some(candidate);
        }
        let storage = storage.ok_or_else(|| {
            emission(format!(
                "a pointer {what} in the {space:?} address space has no operands to determine its Vulkan storage class"
            ))
        })?;
        self.variable_pointer(storage, what)?;
        self.pointer_storage.insert(result, storage);
        let pointee = match storage {
            StorageClass::StorageBuffer => self.type_id(pointee)?,
            _ => self.unlaid_type_id(pointee)?,
        };
        Ok(self.builder.type_pointer(None, storage, pointee))
    }

    pub(super) fn pointer_operand_storage(
        &self,
        operand: &Operand,
    ) -> Result<StorageClass, TranslateError> {
        match operand {
            Operand::Global { index, .. } => self
                .globals
                .get(index)
                .map(|(_, storage)| *storage)
                .ok_or_else(|| emission("pointer value refers to a missing global variable")),
            Operand::Value(value) if self.parameter_pointers.contains_key(value) => {
                Ok(StorageClass::StorageBuffer)
            }
            Operand::Value(value) => self.pointer_storage.get(value).copied().ok_or_else(|| {
                emission("pointer value's Vulkan storage class has not been established")
            }),
            _ => Err(emission(
                "pointer phi or select operand is not a pointer value",
            )),
        }
    }

    /// Note that a pointer is about to be a phi or a select operand.
    ///
    /// SPIR-V's logical addressing model has no pointer values by default;
    /// `VariablePointers` is what admits them, and it is an optional Vulkan
    /// feature. Rejecting here rather than emitting a module the device cannot
    /// accept keeps the failure on the translation side, where it is named.
    pub(super) fn variable_pointer(
        &mut self,
        storage: StorageClass,
        what: &str,
    ) -> Result<(), TranslateError> {
        // `VariablePointersStorageBuffer` covers `StorageBuffer` alone;
        // anything else, which for Metal means a `threadgroup` pointer, needs
        // the wider capability.
        // Prefer the full capability when the device has it. The restricted
        // StorageBuffer capability only permits pointers derived from the
        // same buffer structure; Metal may phi pointers from two arguments.
        let wide = storage != StorageClass::StorageBuffer || self.variable_pointers;
        let capability = match wide {
            true => Capability::VariablePointers,
            false => Capability::VariablePointersStorageBuffer,
        };
        let available = match wide {
            true => self.variable_pointers,
            false => self.variable_pointers || self.variable_pointers_storage_buffer,
        };
        if !available {
            return Err(emission(format!(
                "a {what} of pointers needs {capability:?}, which the target profile does not \
                 report"
            )));
        }
        // The wider capability implies the narrower one, so a module which
        // needs both declares only the wider.
        if self.uses_variable_pointers != Some(Capability::VariablePointers) {
            self.uses_variable_pointers = Some(capability);
            self.capability(capability)?;
            if self.spirv_version < (1, 3) {
                self.builder.extension("SPV_KHR_variable_pointers");
            }
        }
        Ok(())
    }

    /// The constant a SPIR-V execution or memory scope operand is spelled as.
    pub(super) fn scope_id(&mut self, scope: BarrierScope) -> Result<u32, TranslateError> {
        let value = match scope {
            BarrierScope::Device => rspirv::spirv::Scope::Device,
            BarrierScope::Threadgroup => rspirv::spirv::Scope::Workgroup,
            BarrierScope::Simdgroup => rspirv::spirv::Scope::Subgroup,
        };
        self.constant(&Type::u32(), value as u64)
    }

    /// Vulkan carries the array slice as an extra coordinate component;
    /// Metal passes it separately.
    pub(super) fn texture_coordinate(
        &mut self,
        function: &Function,
        coordinate: &Operand,
        array_index: Option<&Operand>,
        values: &HashMap<ValueId, u32>,
    ) -> Result<u32, TranslateError> {
        let base = self.operand(coordinate, values)?;
        let Some(array_index) = array_index else {
            return Ok(base);
        };
        let element = match operand_type(function, coordinate) {
            Some(Type::Vector { element, .. }) => *element,
            Some(scalar) => scalar,
            None => return Err(emission("texture coordinate has no type")),
        };
        let lanes = match operand_type(function, coordinate) {
            Some(Type::Vector { lanes, .. }) => lanes,
            _ => 1,
        };
        // The slice joins the coordinate in the coordinate's own element
        // type, which is integer for reads and floating for samples.
        let index = self.operand(array_index, values)?;
        let index = match &element {
            Type::Float { .. } => {
                let float = self.type_id(&element)?;
                self.builder
                    .convert_u_to_f(float, None, index)
                    .map_err(build_error)?
            }
            _ => index,
        };
        let combined = self.type_id(&Type::Vector {
            element: Box::new(element.clone()),
            lanes: lanes + 1,
        })?;
        let scalar = self.type_id(&element)?;
        let mut components = Vec::new();
        for lane in 0..lanes {
            components.push(if lanes == 1 {
                base
            } else {
                self.builder
                    .composite_extract(scalar, None, base, [u32::from(lane)])
                    .map_err(build_error)?
            });
        }
        components.push(index);
        self.builder
            .composite_construct(combined, None, components)
            .map_err(build_error)
    }

    fn emit_binary<F>(
        &mut self,
        function: &Function,
        instruction: &crate::ir::Instruction,
        lhs: &Operand,
        rhs: &Operand,
        values: &HashMap<ValueId, u32>,
        emit: F,
    ) -> Result<(), TranslateError>
    where
        F: FnOnce(&mut Builder, u32, Option<u32>, u32, u32) -> Result<u32, rspirv::dr::Error>,
    {
        let result = instruction
            .result
            .ok_or_else(|| emission("binary instruction has no result"))?;
        let ty = self.type_id(&function.values[result.0 as usize])?;
        let lhs = self.operand(lhs, values)?;
        let rhs = self.operand(rhs, values)?;
        emit(&mut self.builder, ty, Some(values[&result]), lhs, rhs).map_err(build_error)?;
        Ok(())
    }

    fn emit_ext_inst<const N: usize>(
        &mut self,
        function: &Function,
        instruction: &crate::ir::Instruction,
        values: &HashMap<ValueId, u32>,
        operation: GlslStd450Op,
        operands: [&Operand; N],
    ) -> Result<(), TranslateError> {
        let result = instruction
            .result
            .ok_or_else(|| emission("extended instruction has no result"))?;
        let ty = self.type_id(&function.values[result.0 as usize])?;
        let operands = operands
            .into_iter()
            .map(|operand| self.operand(operand, values).map(SpirvOperand::IdRef))
            .collect::<Result<Vec<_>, _>>()?;
        self.builder
            .ext_inst(
                ty,
                Some(values[&result]),
                self.glsl_std_450,
                operation as u32,
                operands,
            )
            .map_err(build_error)?;
        Ok(())
    }

    pub(super) fn emit_terminator(
        &mut self,
        terminator: &Terminator,
        blocks: &HashMap<BlockId, u32>,
        values: &HashMap<ValueId, u32>,
    ) -> Result<(), TranslateError> {
        match terminator {
            Terminator::Branch(target) => self.builder.branch(blocks[target]).map_err(build_error),
            Terminator::CondBranch {
                condition,
                true_block,
                false_block,
            } => {
                let condition = self.operand(condition, values)?;
                self.builder
                    .branch_conditional(
                        condition,
                        blocks[true_block],
                        blocks[false_block],
                        std::iter::empty(),
                    )
                    .map_err(build_error)
            }
            Terminator::Return(None) => self.builder.ret().map_err(build_error),
            // A SPIR-V entry point returns void. Metal returns the stage's
            // outputs as a value, so decompose it into the output variables.
            Terminator::Return(Some(value)) => {
                let operand_type = self.return_type.clone();
                let value = self.operand(value, values)?;
                if self.stage_outputs.is_empty() {
                    return self.builder.ret_value(value).map_err(build_error);
                }
                for (position, variable) in self.stage_outputs.clone().into_iter().enumerate() {
                    let component = match &operand_type {
                        Some(Type::Struct(structure)) => {
                            let field = structure.fields.get(position).ok_or_else(|| {
                                emission("stage output has no matching returned field")
                            })?;
                            let ty = self.type_id(&field.ty)?;
                            self.builder
                                .composite_extract(ty, None, value, [position as u32])
                                .map_err(build_error)?
                        }
                        _ => value,
                    };
                    self.builder
                        .store(variable, component, None, [])
                        .map_err(build_error)?;
                }
                self.builder.ret().map_err(build_error)
            }
            Terminator::Unreachable => self.builder.unreachable().map_err(build_error),
        }
    }

    pub(super) fn operand(
        &mut self,
        operand: &Operand,
        values: &HashMap<ValueId, u32>,
    ) -> Result<u32, TranslateError> {
        match operand {
            Operand::Bool(value) => {
                let ty = self.type_id(&Type::Bool)?;
                Ok(if *value {
                    self.builder.constant_true(ty)
                } else {
                    self.builder.constant_false(ty)
                })
            }
            Operand::Value(value) => values
                .get(value)
                .copied()
                .ok_or_else(|| emission("operand refers to a missing value")),
            Operand::Integer { ty, value } => self.constant(ty, *value),
            Operand::Float { ty, bits } => self.constant(ty, *bits),
            Operand::Composite { .. } => self.composite_constant(operand),
            Operand::Global { index, .. } => self
                .globals
                .get(index)
                .map(|(variable, _)| *variable)
                .ok_or_else(|| emission("operand refers to a missing global variable")),
            Operand::Undef(ty) => self.undef(ty),
        }
    }

    /// The variable an access chain starts from, and the storage class its
    /// result pointer lives in.
    pub(super) fn pointer_base(
        &mut self,
        pointer: &Operand,
        values: &HashMap<ValueId, u32>,
    ) -> Result<(u32, StorageClass, bool), TranslateError> {
        match pointer {
            Operand::Global { index, .. } => self
                .globals
                .get(index)
                // A global's variable points straight at what it holds, so an
                // access chain into it needs no leading member index.
                .map(|(variable, storage)| (*variable, *storage, false))
                .ok_or_else(|| emission("access chain base is a missing global variable")),
            Operand::Value(value) => {
                if let Some(variable) = self.parameter_pointers.get(value).copied() {
                    // A buffer resource is a block with the buffer as its one
                    // member, so every chain into it starts with that member.
                    return Ok((variable, StorageClass::StorageBuffer, true));
                }
                let storage =
                    self.pointer_storage.get(value).copied().ok_or_else(|| {
                        emission("access chain base is not a resource or a global")
                    })?;
                let id = values
                    .get(value)
                    .copied()
                    .ok_or_else(|| emission("access chain base refers to a missing value"))?;
                Ok((id, storage, false))
            }
            _ => Err(emission("access chain base is not a pointer")),
        }
    }
}

fn small_storage_widths(ty: &Type, widths: &mut BTreeSet<u8>) {
    match ty {
        Type::Int { bits: 8 | 16, .. } | Type::Float { bits: 16 } => {
            let bits = match ty {
                Type::Int { bits, .. } | Type::Float { bits } => *bits,
                _ => unreachable!(),
            };
            widths.insert(bits);
        }
        Type::Vector { element, .. } | Type::Array { element, .. } => {
            small_storage_widths(element, widths)
        }
        Type::Struct(structure) => {
            for field in &structure.fields {
                small_storage_widths(&field.ty, widths);
            }
        }
        _ => {}
    }
}
