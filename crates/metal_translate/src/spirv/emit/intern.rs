use super::*;

impl Emitter {
    /// SPIR-V declares constants at module scope, so they must be created
    /// before the function body which uses them is emitted.
    pub(super) fn collect_constants(&mut self, function: &Function) -> Result<(), TranslateError> {
        for block in &function.blocks {
            for instruction in &block.instructions {
                for operand in instruction_operands(&instruction.kind) {
                    self.declare_constant(operand)?;
                }
            }
            for operand in terminator_operands(&block.terminator) {
                self.declare_constant(operand)?;
            }
        }
        Ok(())
    }

    pub(super) fn declare_constant(&mut self, operand: &Operand) -> Result<(), TranslateError> {
        if let Operand::Integer { ty, .. }
        | Operand::Float { ty, .. }
        | Operand::Composite { ty, .. }
        | Operand::Undef(ty) = operand
        {
            self.arithmetic_type(ty)?;
        }
        match operand {
            Operand::Integer { ty, value } => {
                self.constant(ty, *value)?;
            }
            Operand::Float { ty, bits } => {
                self.constant(ty, *bits)?;
            }
            Operand::Composite { elements, .. } => {
                for element in elements {
                    self.declare_constant(element)?;
                }
                self.composite_constant(operand)?;
            }
            Operand::Undef(ty) => {
                self.undef(ty)?;
            }
            Operand::Value(_) | Operand::Bool(_) | Operand::Global { .. } => {}
        }
        Ok(())
    }

    /// An `OpUndef` of one type, declared once at module scope.
    ///
    /// SPIR-V allows `OpUndef` inside a block, but a phi must be the first
    /// instruction of its block, and an undefined phi operand would otherwise
    /// be created between the phis it feeds.
    pub(super) fn undef(&mut self, ty: &Type) -> Result<u32, TranslateError> {
        if let Some(id) = self.undefs.get(ty) {
            return Ok(*id);
        }
        let ty_id = self.type_id(ty)?;
        let id = self.builder.id();
        self.builder
            .module_mut()
            .types_global_values
            .push(rspirv::dr::Instruction::new(
                rspirv::spirv::Op::Undef,
                Some(ty_id),
                Some(id),
                Vec::new(),
            ));
        self.undefs.insert(ty.clone(), id);
        Ok(id)
    }

    pub(super) fn composite_constant(&mut self, operand: &Operand) -> Result<u32, TranslateError> {
        let Operand::Composite { ty, elements } = operand else {
            return Err(emission("composite constant has no elements"));
        };
        if let Some(id) = self.composites.get(operand) {
            return Ok(*id);
        }
        let ty_id = self.type_id(ty)?;
        let members = elements
            .iter()
            .map(|element| match element {
                Operand::Composite { .. } => self.composite_constant(element),
                Operand::Integer { ty, value } => self.constant(ty, *value),
                Operand::Float { ty, bits } => self.constant(ty, *bits),
                Operand::Bool(value) => {
                    let ty = self.type_id(&Type::Bool)?;
                    Ok(if *value {
                        self.builder.constant_true(ty)
                    } else {
                        self.builder.constant_false(ty)
                    })
                }
                Operand::Undef(ty) => self.undef(ty),
                Operand::Global { .. } => {
                    Err(emission("a global variable is not a constant element"))
                }
                Operand::Value(_) => Err(emission("composite constant has a non-constant element")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let id = self.builder.constant_composite(ty_id, members);
        self.composites.insert(operand.clone(), id);
        Ok(id)
    }

    pub(super) fn constant(&mut self, ty: &Type, value: u64) -> Result<u32, TranslateError> {
        let key = (ty.clone(), value);
        if let Some(id) = self.constants.get(&key) {
            return Ok(*id);
        }
        let ty_id = self.type_id(ty)?;
        let id = match ty {
            Type::Int { bits: 1..=32, .. } | Type::Float { bits: 32 } => {
                self.builder.constant_bit32(ty_id, value as u32)
            }
            Type::Int { bits: 33..=64, .. } => self.builder.constant_bit64(ty_id, value),
            _ => return Err(emission("unsupported SPIR-V constant type")),
        };
        self.constants.insert(key, id);
        Ok(id)
    }

    /// One constant operand, as the initializer of a global variable.
    pub(super) fn constant_operand(&mut self, operand: &Operand) -> Result<u32, TranslateError> {
        match operand {
            Operand::Integer { ty, value } => self.constant(ty, *value),
            Operand::Float { ty, bits } => self.constant(ty, *bits),
            Operand::Bool(value) => {
                let ty = self.type_id(&Type::Bool)?;
                Ok(if *value {
                    self.builder.constant_true(ty)
                } else {
                    self.builder.constant_false(ty)
                })
            }
            Operand::Composite { ty, elements } => {
                if let Some(id) = self.unlaid_composites.get(operand) {
                    return Ok(*id);
                }
                let ty_id = self.unlaid_type_id(ty)?;
                let members = elements
                    .iter()
                    .map(|element| self.constant_operand(element))
                    .collect::<Result<Vec<_>, _>>()?;
                let id = self.builder.constant_composite(ty_id, members);
                self.unlaid_composites.insert(operand.clone(), id);
                Ok(id)
            }
            Operand::Undef(ty) => self.undef(ty),
            Operand::Value(_) | Operand::Global { .. } => Err(emission(
                "a global variable's initializer is not a constant",
            )),
        }
    }

    /// The type of a value which does not live in a buffer.
    ///
    /// Vulkan forbids `ArrayStride` and `Offset` outside the storage classes
    /// which have an explicit layout, so a module global and a constant
    /// composite need their own aggregate types.
    pub(super) fn unlaid_type_id(&mut self, ty: &Type) -> Result<u32, TranslateError> {
        if let Some(id) = self.unlaid_types.get(ty) {
            return Ok(*id);
        }
        let id = match ty {
            Type::Array { element, count } => {
                let element_id = self.unlaid_type_id(element)?;
                let count = self.constant(&Type::u32(), u64::from(*count))?;
                let id = self.builder.id();
                self.builder.type_array_id(Some(id), element_id, count);
                id
            }
            Type::Struct(structure) => {
                let fields = structure
                    .fields
                    .iter()
                    .map(|field| self.unlaid_type_id(&field.ty))
                    .collect::<Result<Vec<_>, _>>()?;
                let id = self.builder.id();
                self.builder.type_struct_id(Some(id), fields);
                id
            }
            _ => return self.type_id(ty),
        };
        self.unlaid_types.insert(ty.clone(), id);
        Ok(id)
    }

    pub(super) fn type_id(&mut self, ty: &Type) -> Result<u32, TranslateError> {
        if let Some(id) = self.types.get(ty) {
            return Ok(*id);
        }
        let id = match ty {
            Type::Void => self.builder.type_void(),
            Type::Bool => self.builder.type_bool(),
            Type::Int { bits, signed } => {
                if *bits == 64 && !self.uses_int64 {
                    self.capability(Capability::Int64)?;
                    self.uses_int64 = true;
                }
                self.builder.type_int(u32::from(*bits), u32::from(*signed))
            }
            Type::Float { bits } => self.builder.type_float(u32::from(*bits), None),
            Type::Vector { element, lanes } => {
                let element = self.type_id(element)?;
                self.builder.type_vector(element, u32::from(*lanes))
            }
            Type::Struct(structure) => {
                let fields = structure
                    .fields
                    .iter()
                    .map(|field| self.type_id(&field.ty))
                    .collect::<Result<Vec<_>, _>>()?;
                // Distinct Metal structures can have identical member types
                // and different offsets. rspirv would reuse one SPIR-V type
                // for both and decorate it twice, so force a fresh id.
                let id = self.builder.id();
                self.builder.type_struct_id(Some(id), fields);
                for (index, field) in structure.fields.iter().enumerate() {
                    self.builder.member_decorate(
                        id,
                        index as u32,
                        Decoration::Offset,
                        [SpirvOperand::LiteralBit32(field.offset)],
                    );
                }
                id
            }
            Type::Array { element, count } => {
                let element_id = self.type_id(element)?;
                let count = self.constant(&Type::u32(), u64::from(*count))?;
                // As for a structure: a decorated type must not be shared with
                // the undecorated one a module global needs.
                let id = self.builder.id();
                self.builder.type_array_id(Some(id), element_id, count);
                self.builder.decorate(
                    id,
                    Decoration::ArrayStride,
                    [SpirvOperand::LiteralBit32(type_stride(element)?)],
                );
                id
            }
            Type::Texture(texture) => {
                let texture = texture.clone();
                let scalar = texture.sampled_scalar();
                self.arithmetic_type(&scalar)?;
                let scalar = self.type_id(&scalar)?;
                let dimension = match texture.dimension {
                    TextureDimension::D1 => Dim::Dim1D,
                    TextureDimension::D2 => Dim::Dim2D,
                    TextureDimension::D3 => Dim::Dim3D,
                    TextureDimension::Cube => Dim::DimCube,
                };
                // `Sampled` distinguishes an image a sampler or a fetch reads
                // from a storage image; only a texture a shader writes is the
                // latter. See `TextureType::sampled_image`.
                let sampled = if texture.sampled_image() { 1 } else { 2 };
                if texture.dimension == TextureDimension::D1 && sampled == 2 {
                    self.capability(Capability::Image1D)?;
                }
                if texture.multisampled && sampled == 2 {
                    self.capability(Capability::StorageImageMultisample)?;
                }
                if texture.dimension == TextureDimension::Cube && texture.array {
                    self.capability(Capability::SampledCubeArray)?;
                }
                // A storage image with an unknown format needs an explicit
                // capability for each direction it is accessed in.
                if sampled == 2 {
                    match texture.access {
                        Access::Read | Access::ReadWrite => {
                            self.capability(Capability::StorageImageReadWithoutFormat)?;
                        }
                        _ => {}
                    }
                    match texture.access {
                        Access::Write | Access::ReadWrite => {
                            self.capability(Capability::StorageImageWriteWithoutFormat)?;
                        }
                        _ => {}
                    }
                }
                self.builder.type_image(
                    scalar,
                    dimension,
                    u32::from(texture.depth),
                    u32::from(texture.array),
                    u32::from(texture.multisampled),
                    sampled,
                    ImageFormat::Unknown,
                    None,
                )
            }
            Type::Sampler => self.builder.type_sampler(),
            // A pointer only needs a type of its own where it is a value:
            // an access chain builds its own result type from the storage
            // class it started in, but a phi of two pointers into a buffer has
            // to name one. Metal's address space says the storage class, and
            // only the spaces a *pointer value* can be in are mapped: a
            // `constant` or `thread` pointer may address either a module global
            // or a buffer, which are different storage classes, and nothing in
            // the type says which.
            Type::Pointer { space, pointee } => {
                let (storage, pointee) = match space {
                    AddressSpace::Device => (StorageClass::StorageBuffer, self.type_id(pointee)?),
                    AddressSpace::Threadgroup | AddressSpace::ThreadgroupImageblock => {
                        (StorageClass::Workgroup, self.unlaid_type_id(pointee)?)
                    }
                    _ => {
                        return Err(emission(format!(
                            "a pointer value in the {space:?} address space has no one Vulkan \
                             storage class"
                        )));
                    }
                };
                self.builder.type_pointer(None, storage, pointee)
            }
            _ => return Err(emission(format!("unsupported SPIR-V value type {ty:?}"))),
        };
        self.types.insert(ty.clone(), id);
        Ok(id)
    }
}
