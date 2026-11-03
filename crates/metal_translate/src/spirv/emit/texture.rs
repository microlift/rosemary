use super::*;
use crate::ir::TextureType;

/// How the texels an image access carries become the IR's texels.
#[derive(Clone, Copy)]
enum Resize {
    /// A signed image's texels against the IR's signless ones.
    Bitcast,
    /// A `half` texel against Vulkan's 32-bit sampled type.
    Float,
    /// A `short` texel against Vulkan's 32-bit sampled type.
    Integer { signed: bool },
}

/// The IR's result an image access is converted into.
struct Narrowed {
    ty: u32,
    result_id: Option<u32>,
    resize: Resize,
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
                if let Some((access, resize)) = self.image_texels(&image_type, &written)? {
                    let builder = &mut self.builder;
                    value = match resize {
                        Resize::Bitcast => builder.bitcast(access, None, value),
                        Resize::Float => builder.f_convert(access, None, value),
                        Resize::Integer { signed: true } => builder.s_convert(access, None, value),
                        Resize::Integer { signed: false } => builder.u_convert(access, None, value),
                    }
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
                bias,
                gather,
                compare,
            } => {
                let result = instruction
                    .result
                    .ok_or_else(|| emission("texture sample has no result"))?;
                let Some(Type::Texture(image_type)) = operand_type(function, texture) else {
                    return Err(emission("texture sample has no texture operand"));
                };
                let (ty, result_id, narrowed) =
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
                // Image operands follow their mask bits in order: Bias first.
                let mut mask = ImageOperands::NONE;
                let mut extra = Vec::new();
                if let Some(bias) = bias {
                    mask |= ImageOperands::BIAS;
                    extra.push(SpirvOperand::IdRef(self.operand(bias, values)?));
                }
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
                // Metal requires a sampling offset to be a compile-time
                // constant, which is also the only form Vulkan accepts outside
                // a gather.
                if let Some(offset) = offset.as_ref().map(sampling_offset).transpose()?.flatten() {
                    mask |= if gather.is_some() {
                        self.capability(Capability::ImageGatherExtended)?;
                        ImageOperands::OFFSET
                    } else {
                        ImageOperands::CONST_OFFSET
                    };
                    extra.push(SpirvOperand::IdRef(self.operand(&offset, values)?));
                }
                if let Some(reference) = compare {
                    let reference = self.operand(reference, values)?;
                    if lod.is_some() || gradient.is_some() {
                        self.builder
                            .image_sample_dref_explicit_lod(
                                ty, result_id, sampled, coordinate, reference, mask, extra,
                            )
                            .map_err(build_error)?;
                    } else {
                        self.builder
                            .image_sample_dref_implicit_lod(
                                ty,
                                result_id,
                                sampled,
                                coordinate,
                                reference,
                                Some(mask),
                                extra,
                            )
                            .map_err(build_error)?;
                    }
                    return Ok(());
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
                self.narrow_texels(narrowed, result_id)?;
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
                let (ty, result_id, narrowed) =
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
                self.narrow_texels(narrowed, result_id)?;
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
                // `OpImageQuerySize` covers a storage image, a multisampled
                // one and a buffer; every other sampled image needs the level
                // named, and
                // Metal's `get_width()` on a texture it only reads names none.
                let level = match lod {
                    Some(lod) => Some(self.operand(lod, values)?),
                    None if image_type.sampled_image()
                        && !image_type.multisampled
                        && image_type.dimension != TextureDimension::Buffer =>
                    {
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

    /// The type an image access of `ty` texels is emitted with, and how its
    /// texels convert to `ty`'s; `None` where `ty` is already that type.
    fn image_texels(
        &mut self,
        texture: &TextureType,
        ty: &Type,
    ) -> Result<Option<(u32, Resize)>, TranslateError> {
        let (element, lanes) = match ty {
            Type::Vector { element, lanes } => (element.as_ref(), Some(*lanes)),
            scalar => (scalar, None),
        };
        let sampled = texture.sampled_scalar();
        if *element == sampled {
            return Ok(None);
        }
        let resize = match (element, &sampled) {
            (Type::Float { .. }, Type::Float { .. }) => Resize::Float,
            (Type::Int { bits: 32, .. }, Type::Int { bits: 32, .. }) => Resize::Bitcast,
            (Type::Int { .. }, Type::Int { signed, .. }) => Resize::Integer { signed: *signed },
            _ => return Err(emission("an image access of the wrong texel kind")),
        };
        let access = match lanes {
            Some(lanes) => Type::Vector {
                element: Box::new(sampled),
                lanes,
            },
            None => sampled,
        };
        Ok(Some((self.type_id(&access)?, resize)))
    }

    /// The result type and id an image access is emitted with. An access
    /// whose texels differ from the IR's gets a fresh id, and `narrow_texels`
    /// later converts it into the IR's result under the original id.
    fn texel_result(
        &mut self,
        texture: &TextureType,
        result: &Type,
        result_id: Option<u32>,
    ) -> Result<(u32, Option<u32>, Option<Narrowed>), TranslateError> {
        let ty = self.type_id(result)?;
        Ok(match self.image_texels(texture, result)? {
            Some((access, resize)) => (
                access,
                Some(self.builder.id()),
                Some(Narrowed {
                    ty,
                    result_id,
                    resize,
                }),
            ),
            None => (ty, result_id, None),
        })
    }

    fn narrow_texels(
        &mut self,
        narrowed: Option<Narrowed>,
        value: Option<u32>,
    ) -> Result<(), TranslateError> {
        let (
            Some(Narrowed {
                ty,
                result_id,
                resize,
            }),
            Some(value),
        ) = (narrowed, value)
        else {
            return Ok(());
        };
        match resize {
            Resize::Bitcast => self.builder.bitcast(ty, result_id, value),
            Resize::Float => self.builder.f_convert(ty, result_id, value),
            // Narrowing keeps the low bits whatever the image's signedness.
            Resize::Integer { .. } => self.builder.u_convert(ty, result_id, value),
        }
        .map_err(build_error)?;
        Ok(())
    }
}

/// The image operand for a constant sampling offset, or `None` where the
/// offset is zero and so needs no operand at all. AIR passes an offset with
/// nearly every sample, almost always `int2(0)`.
fn sampling_offset(offset: &Operand) -> Result<Option<Operand>, TranslateError> {
    let offset = signed_offset(offset)?;
    Ok((!is_zero(&offset)).then_some(offset))
}

/// A constant offset retyped as Metal's signed `int2`/`int3`; the bits are
/// unchanged. The IR is signless, and SPIRV-Cross spells an unsigned offset
/// as a `uint2`, which MSL's `sample` does not accept.
fn signed_offset(offset: &Operand) -> Result<Operand, TranslateError> {
    let signed = |ty: &Type| {
        ty.signed_integer()
            .ok_or_else(|| emission("a sampling offset must be an integer constant"))
    };
    Ok(match offset {
        Operand::Integer { ty, value } => Operand::Integer {
            ty: signed(ty)?,
            value: *value,
        },
        Operand::Composite { ty, elements } => Operand::Composite {
            ty: signed(ty)?,
            elements: elements
                .iter()
                .map(signed_offset)
                .collect::<Result<_, _>>()?,
        },
        // An undefined lane may be given any value, and zero leaves the
        // sample unchanged.
        Operand::Undef(Type::Vector { element, lanes }) => signed_offset(&Operand::Composite {
            ty: Type::Vector {
                element: element.clone(),
                lanes: *lanes,
            },
            elements: vec![Operand::Undef((**element).clone()); usize::from(*lanes)],
        })?,
        Operand::Undef(ty) => Operand::Integer {
            ty: signed(ty)?,
            value: 0,
        },
        _ => {
            return Err(emission(
                "a sampling offset must be a constant to be emitted",
            ));
        }
    })
}

fn is_zero(offset: &Operand) -> bool {
    match offset {
        Operand::Integer { value, .. } => *value == 0,
        Operand::Composite { elements, .. } => elements.iter().all(is_zero),
        _ => false,
    }
}
