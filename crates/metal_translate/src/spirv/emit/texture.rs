use super::*;
use crate::ir::TextureType;

/// The IR's signless result a signed image access is bitcast into.
struct Unsigned {
    ty: u32,
    result_id: Option<u32>,
}

impl Emitter {
    /// Emit one such instruction.
    pub(super) fn emit_texture(
        &mut self,
        function: &Function,
        instruction: &crate::ir::Instruction,
        values: &HashMap<ValueId, u32>,
        result_id: Option<u32>,
    ) -> Result<(), TranslateError> {
        match &instruction.kind {
            InstructionKind::TextureWrite {
                texture,
                coordinate,
                array_index,
                value,
            } => {
                let Some(Type::Texture(image_type)) = operand_type(function, texture) else {
                    return Err(emission("texture write has no texture operand"));
                };
                let written = operand_type(function, value)
                    .ok_or_else(|| emission("texture write has no value type"))?;
                let image = self.operand(texture, values)?;
                let coordinate =
                    self.texture_coordinate(function, coordinate, array_index.as_ref(), values)?;
                let mut value = self.operand(value, values)?;
                if let Some(signed) = self.signed_texels(&image_type, &written)? {
                    value = self
                        .builder
                        .bitcast(signed, None, value)
                        .map_err(build_error)?;
                }
                self.builder
                    .image_write(image, coordinate, value, None, [])
                    .map_err(build_error)?;
            }
            InstructionKind::TextureSample {
                texture,
                sampler,
                coordinate,
                array_index,
                offset,
                lod,
                gradient,
                gather,
            } => {
                let result = instruction
                    .result
                    .ok_or_else(|| emission("texture sample has no result"))?;
                let Some(Type::Texture(image_type)) = operand_type(function, texture) else {
                    return Err(emission("texture sample has no texture operand"));
                };
                let (ty, result_id, signed) =
                    self.texel_result(&image_type, &function.values[result.0 as usize], result_id)?;
                let image_id = self.type_id(&Type::Texture(image_type))?;
                let sampled_type = self.builder.type_sampled_image(image_id);
                let image = self.operand(texture, values)?;
                let sampler = self.operand(sampler, values)?;
                let sampled = self
                    .builder
                    .sampled_image(sampled_type, None, image, sampler)
                    .map_err(build_error)?;
                let coordinate =
                    self.texture_coordinate(function, coordinate, array_index.as_ref(), values)?;
                let mut mask = ImageOperands::NONE;
                let mut extra = Vec::new();
                if let Some(lod) = lod {
                    mask |= ImageOperands::LOD;
                    extra.push(SpirvOperand::IdRef(self.operand(lod, values)?));
                }
                if let Some(gradient) = gradient.as_deref() {
                    // SPIR-V's Grad takes the derivatives of the coordinate
                    // without its array layer, which is the shape AIR already
                    // passes them in.
                    mask |= ImageOperands::GRAD;
                    extra.push(SpirvOperand::IdRef(self.operand(&gradient.0, values)?));
                    extra.push(SpirvOperand::IdRef(self.operand(&gradient.1, values)?));
                }
                if let Some(offset) = offset {
                    // Metal requires a sampling offset to be a compile-time
                    // constant, which is also the only form Vulkan accepts
                    // outside a gather.
                    if !matches!(offset, Operand::Composite { .. } | Operand::Integer { .. }) {
                        return Err(emission(
                            "a sampling offset must be a constant to be emitted",
                        ));
                    }
                    mask |= if gather.is_some() {
                        self.capability(Capability::ImageGatherExtended)?;
                        ImageOperands::OFFSET
                    } else {
                        ImageOperands::CONST_OFFSET
                    };
                    extra.push(SpirvOperand::IdRef(self.operand(offset, values)?));
                }
                match gather {
                    Some(component) => {
                        let component = self.constant(&Type::u32(), u64::from(*component))?;
                        self.builder
                            .image_gather(
                                ty,
                                result_id,
                                sampled,
                                coordinate,
                                component,
                                Some(mask),
                                extra,
                            )
                            .map_err(build_error)?;
                    }
                    // An implicit level of detail needs the fragment stage's
                    // derivatives; AIR requests an explicit level otherwise.
                    None if lod.is_some() || gradient.is_some() => {
                        self.builder
                            .image_sample_explicit_lod(
                                ty, result_id, sampled, coordinate, mask, extra,
                            )
                            .map_err(build_error)?;
                    }
                    None => {
                        self.builder
                            .image_sample_implicit_lod(
                                ty,
                                result_id,
                                sampled,
                                coordinate,
                                Some(mask),
                                extra,
                            )
                            .map_err(build_error)?;
                    }
                }
                self.unsign_texels(signed, result_id)?;
            }
            InstructionKind::TextureRead {
                texture,
                coordinate,
                array_index,
                lod,
                sample,
            } => {
                let result = instruction
                    .result
                    .ok_or_else(|| emission("texture read has no result"))?;
                let Some(Type::Texture(image_type)) = operand_type(function, texture) else {
                    return Err(emission("texture read has no texture operand"));
                };
                let (ty, result_id, signed) =
                    self.texel_result(&image_type, &function.values[result.0 as usize], result_id)?;
                let image = self.operand(texture, values)?;
                let coordinate =
                    self.texture_coordinate(function, coordinate, array_index.as_ref(), values)?;
                let mut mask = ImageOperands::NONE;
                let mut extra = Vec::new();
                if let Some(lod) = lod {
                    mask |= ImageOperands::LOD;
                    extra.push(SpirvOperand::IdRef(self.operand(lod, values)?));
                }
                if let Some(sample) = sample {
                    mask |= ImageOperands::SAMPLE;
                    extra.push(SpirvOperand::IdRef(self.operand(sample, values)?));
                }
                // A sampled image is fetched; a storage image is read.
                if image_type.sampled_image() {
                    self.builder
                        .image_fetch(ty, result_id, image, coordinate, Some(mask), extra)
                        .map_err(build_error)?;
                } else {
                    self.builder
                        .image_read(ty, result_id, image, coordinate, Some(mask), extra)
                        .map_err(build_error)?;
                }
                self.unsign_texels(signed, result_id)?;
            }
            InstructionKind::TextureQuerySize {
                texture,
                lod,
                component,
            } => {
                let result = instruction
                    .result
                    .ok_or_else(|| emission("texture query has no result"))?;
                let ty = self.type_id(&function.values[result.0 as usize])?;
                let Some(Type::Texture(image_type)) = operand_type(function, texture) else {
                    return Err(emission("texture query has no texture operand"));
                };
                self.capability(Capability::ImageQuery)?;
                let lanes = coordinate_lanes(&image_type) + u8::from(image_type.array);
                let size_type = self.type_id(&if lanes == 1 {
                    Type::u32()
                } else {
                    Type::Vector {
                        element: Box::new(Type::u32()),
                        lanes,
                    }
                })?;
                let image = self.operand(texture, values)?;
                // `OpImageQuerySize` covers a storage image and a multisampled
                // one; every other sampled image needs the level named, and
                // Metal's `get_width()` on a texture it only reads names none.
                let level = match lod {
                    Some(lod) => Some(self.operand(lod, values)?),
                    None if image_type.sampled_image() && !image_type.multisampled => {
                        Some(self.constant(&Type::u32(), 0)?)
                    }
                    None => None,
                };
                let size = match level {
                    Some(lod) => self
                        .builder
                        .image_query_size_lod(size_type, None, image, lod)
                        .map_err(build_error)?,
                    None => self
                        .builder
                        .image_query_size(size_type, None, image)
                        .map_err(build_error)?,
                };
                if lanes == 1 {
                    self.builder
                        .copy_object(ty, result_id, size)
                        .map_err(build_error)?;
                } else {
                    self.builder
                        .composite_extract(ty, result_id, size, [*component])
                        .map_err(build_error)?;
                }
            }
            _ => unreachable!("not a texture instruction"),
        }
        Ok(())
    }

    /// The signed form of `ty` where `texture` holds signed integers and `ty`
    /// is an integer value, which is the type Vulkan requires an image access
    /// to use; `None` where the IR's own signless type already matches.
    fn signed_texels(
        &mut self,
        texture: &TextureType,
        ty: &Type,
    ) -> Result<Option<u32>, TranslateError> {
        match ty.signed_integer().filter(|_| texture.signed) {
            Some(signed) => self.type_id(&signed).map(Some),
            None => Ok(None),
        }
    }

    /// The result type and id an image access is emitted with. A signed
    /// access gets a fresh id, and `unsign_texels` later bitcasts it into the
    /// IR's signless result under the original id.
    fn texel_result(
        &mut self,
        texture: &TextureType,
        result: &Type,
        result_id: Option<u32>,
    ) -> Result<(u32, Option<u32>, Option<Unsigned>), TranslateError> {
        let ty = self.type_id(result)?;
        Ok(match self.signed_texels(texture, result)? {
            Some(signed) => (
                signed,
                Some(self.builder.id()),
                Some(Unsigned { ty, result_id }),
            ),
            None => (ty, result_id, None),
        })
    }

    fn unsign_texels(
        &mut self,
        unsigned: Option<Unsigned>,
        value: Option<u32>,
    ) -> Result<(), TranslateError> {
        let (Some(Unsigned { ty, result_id }), Some(value)) = (unsigned, value) else {
            return Ok(());
        };
        self.builder
            .bitcast(ty, result_id, value)
            .map_err(build_error)?;
        Ok(())
    }
}
