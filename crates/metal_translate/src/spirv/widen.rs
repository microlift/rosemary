//! Compute `half` as `float` on targets without `shaderFloat16`, and a
//! `short` texture's texels as `int` on targets without `shaderInt16`.
//! Every half-producing operation is rounded through an integer IEEE-754
//! binary16 conversion, and every short producer is wrapped to its low 16
//! bits; signed short consumers sign-extend those bits before operating.
//!
//! A narrow type in a buffer keeps the guest's layout, so a module with one is
//! left unchanged and still needs the capability.

use super::*;
use crate::ir::{Instruction, StructField, StructType, TextureType};
use crate::passes::Substitution;

mod half;

/// The narrow types a target cannot compute with.
#[derive(Clone, Copy)]
struct Narrow {
    half: bool,
    short: bool,
}

/// The module with each narrow type `target` lacks widened, or `None` if
/// there is nothing to change or it can't be changed.
pub(super) fn widen(module: &Module, target: &TargetProfile) -> Option<Module> {
    let narrow = Narrow {
        half: !target.shader_float16,
        short: !target.shader_int16,
    };
    let any = module.globals.iter().any(|global| narrow.has(&global.ty))
        || module.functions.iter().any(|function| {
            narrow.has(&function.result) || function.values.iter().any(|ty| narrow.has(ty))
        });
    if !any {
        return None;
    }
    let mut module = module.clone();
    for global in &mut module.globals {
        if global.space == AddressSpace::Constant && narrow.has(&global.ty) {
            return None;
        }
        global.ty = narrow.widen_type(&global.ty)?;
        if let Some(initializer) = &mut global.initializer {
            narrow.widen_operand(initializer)?;
        }
    }
    for function in &mut module.functions {
        narrow.widen_function(function)?;
    }
    Some(module)
}

impl Narrow {
    fn widen_function(self, function: &mut Function) -> Option<()> {
        let original_values = function.values.clone();
        let mut parameter_wraps = Vec::new();
        for parameter in &mut function.parameters {
            let ty = original_values.get(parameter.value.0 as usize)?;
            if self.has(ty)
                && matches!(
                    ty,
                    Type::Array { .. } | Type::Matrix { .. } | Type::Struct(_)
                )
            {
                // A scalar/vector parameter can be normalized at function
                // entry. A composite needs per-field interface lowering.
                return None;
            }
            if !self.short_value(ty) && !self.half_value(ty) {
                continue;
            }
            let result = parameter.value;
            let raw = ValueId(function.values.len() as u32);
            function.values.push(ty.clone());
            parameter.value = raw;
            if self.short_value(ty) {
                self.wrap_short(
                    Operand::Value(raw),
                    ty,
                    result,
                    &mut function.values,
                    &mut parameter_wraps,
                )?;
            } else {
                half::quantize(
                    Operand::Value(raw),
                    ty,
                    result,
                    &mut function.values,
                    &mut parameter_wraps,
                )?;
            }
        }

        let mut substitution = Substitution::default();
        for block in &mut function.blocks {
            let mut phis = Vec::new();
            let mut phi_wraps = Vec::new();
            let mut kept = Vec::with_capacity(block.instructions.len());
            for mut instruction in std::mem::take(&mut block.instructions) {
                let result = instruction.result;
                let result_type = result
                    .and_then(|result| original_values.get(result.0 as usize))
                    .cloned();
                let result_is_short = result_type.as_ref().is_some_and(|ty| self.short_value(ty));
                let result_is_half = result_type.as_ref().is_some_and(|ty| self.half_value(ty));
                self.prepare_signed_short_operands(
                    &mut instruction.kind,
                    &original_values,
                    &mut function.values,
                    &mut kept,
                )?;
                if let InstructionKind::Convert { op, value } = &mut instruction.kind {
                    let source = type_of(&function.values, value)?;
                    let result = result_type.clone()?;
                    if *op == ConvertOp::FloatResize
                        && result_is_half
                        && self.widen_type(&source)? == self.widen_type(&result)?
                    {
                        half::quantize(
                            value.clone(),
                            &result,
                            instruction.result?,
                            &mut function.values,
                            &mut kept,
                        )?;
                        continue;
                    }
                    match op {
                        ConvertOp::Bitcast if self.has(&source) || self.has(&result) => {
                            *op = match (&source, &result) {
                                (Type::Int { bits: 32, .. }, half2)
                                    if self.half && is_half2(half2) =>
                                {
                                    ConvertOp::UnpackHalf2x16
                                }
                                (half2, Type::Int { bits: 32, .. })
                                    if self.half && is_half2(half2) =>
                                {
                                    ConvertOp::PackHalf2x16
                                }
                                _ => return None,
                            };
                        }
                        // Once both sides are the wide type the conversion does
                        // nothing.
                        ConvertOp::FloatResize
                        | ConvertOp::ZeroExtend
                        | ConvertOp::SignExtend
                        | ConvertOp::IntegerTruncate
                            if self.widen_type(&source)? == self.widen_type(&result)? =>
                        {
                            let id = instruction.result?;
                            if result_is_short {
                                self.wrap_short(
                                    value.clone(),
                                    &result,
                                    id,
                                    &mut function.values,
                                    &mut kept,
                                )?;
                            } else {
                                substitution.insert(id, value.clone());
                            }
                            continue;
                        }
                        _ => {}
                    }
                }
                let is_phi = matches!(instruction.kind, InstructionKind::Phi { .. });
                let rounds_half = result_is_half && self.half_result_needs_round(&instruction.kind);
                if rounds_half || result_is_short {
                    let result = result?;
                    let ty = result_type.as_ref()?;
                    let raw = ValueId(function.values.len() as u32);
                    function.values.push(ty.clone());
                    instruction.result = Some(raw);
                    if is_phi {
                        phis.push(instruction);
                        if rounds_half {
                            half::quantize(
                                Operand::Value(raw),
                                ty,
                                result,
                                &mut function.values,
                                &mut phi_wraps,
                            )?;
                        } else {
                            self.wrap_short(
                                Operand::Value(raw),
                                ty,
                                result,
                                &mut function.values,
                                &mut phi_wraps,
                            )?;
                        }
                    } else {
                        kept.push(instruction);
                        if rounds_half {
                            half::quantize(
                                Operand::Value(raw),
                                ty,
                                result,
                                &mut function.values,
                                &mut kept,
                            )?;
                        } else {
                            self.wrap_short(
                                Operand::Value(raw),
                                ty,
                                result,
                                &mut function.values,
                                &mut kept,
                            )?;
                        }
                    }
                } else if is_phi {
                    phis.push(instruction);
                } else {
                    kept.push(instruction);
                }
            }
            phis.append(&mut phi_wraps);
            phis.append(&mut kept);
            block.instructions = phis;
        }
        if !parameter_wraps.is_empty() {
            let entry = function.blocks.first_mut()?;
            let after_phis = entry
                .instructions
                .iter()
                .position(|instruction| !matches!(instruction.kind, InstructionKind::Phi { .. }))
                .unwrap_or(entry.instructions.len());
            entry
                .instructions
                .splice(after_phis..after_phis, parameter_wraps);
        }
        substitution.apply(function);

        function.result = self.widen_type(&function.result)?;
        for parameter in &mut function.parameters {
            parameter.ty = self.widen_type(&parameter.ty)?;
        }
        for ty in &mut function.values {
            *ty = self.widen_type(ty)?;
        }
        for block in &mut function.blocks {
            for instruction in &mut block.instructions {
                if let InstructionKind::LocalVariable { ty } = &mut instruction.kind {
                    *ty = self.widen_type(ty)?;
                }
                for operand in instruction_operands_mut(&mut instruction.kind) {
                    self.widen_operand(operand)?;
                }
            }
            match &mut block.terminator {
                Terminator::CondBranch { condition, .. } => self.widen_operand(condition)?,
                Terminator::Return(Some(value)) => self.widen_operand(value)?,
                _ => {}
            }
        }
        Some(())
    }

    /// Whether this is a scalar or vector integer whose 16-bit arithmetic is
    /// being emulated at 32 bits. Other composite shapes are rejected rather
    /// than partly wrapped.
    fn short_value(self, ty: &Type) -> bool {
        if !self.short {
            return false;
        }
        match ty {
            Type::Int { bits: 16, .. } => true,
            Type::Vector { element, .. } => matches!(element.as_ref(), Type::Int { bits: 16, .. }),
            _ => false,
        }
    }

    fn half_value(self, ty: &Type) -> bool {
        if !self.half {
            return false;
        }
        match ty {
            Type::Float { bits: 16 } => true,
            Type::Vector { element, .. } => **element == Type::Float { bits: 16 },
            _ => false,
        }
    }

    fn half_result_needs_round(self, kind: &InstructionKind) -> bool {
        matches!(
            kind,
            InstructionKind::FloatBinary { .. }
                | InstructionKind::FloatUnary { .. }
                | InstructionKind::FloatClamp { .. }
                | InstructionKind::FloatMix { .. }
                | InstructionKind::FloatFma { .. }
                | InstructionKind::FloatDot { .. }
                | InstructionKind::FloatRefract { .. }
                | InstructionKind::TextureSample { .. }
                | InstructionKind::TextureRead { .. }
        )
    }

    /// A widened short is kept as its low 16 bits. Operations which interpret
    /// those bits as signed receive a temporary sign extension; unsigned and
    /// bitwise operations consume the canonical value directly.
    fn prepare_signed_short_operands(
        self,
        kind: &mut InstructionKind,
        original_values: &[Type],
        values: &mut Vec<Type>,
        instructions: &mut Vec<Instruction>,
    ) -> Option<()> {
        let mut signed = |operand: &mut Operand| {
            self.sign_extend_short(operand, original_values, values, instructions)
        };
        match kind {
            InstructionKind::IntegerBinary { op, lhs, rhs } => match op {
                IntegerBinaryOp::DivideSigned
                | IntegerBinaryOp::RemainderSigned
                | IntegerBinaryOp::MinimumSigned
                | IntegerBinaryOp::MaximumSigned => {
                    signed(lhs)?;
                    signed(rhs)?;
                }
                IntegerBinaryOp::ShiftRightArithmetic => signed(lhs)?,
                _ => {}
            },
            InstructionKind::IntegerCompare {
                predicate:
                    IntegerPredicate::SignedLessThan
                    | IntegerPredicate::SignedLessEqual
                    | IntegerPredicate::SignedGreaterThan
                    | IntegerPredicate::SignedGreaterEqual,
                lhs,
                rhs,
            } => {
                signed(lhs)?;
                signed(rhs)?;
            }
            InstructionKind::IntegerUnary {
                op: IntegerUnaryOp::AbsoluteSigned | IntegerUnaryOp::SignSigned,
                value,
            } => signed(value)?,
            InstructionKind::IntegerClamp {
                signed: true,
                value,
                min,
                max,
            } => {
                signed(value)?;
                signed(min)?;
                signed(max)?;
            }
            InstructionKind::Convert {
                op: ConvertOp::SignExtend | ConvertOp::SignedToFloat,
                value,
            } => signed(value)?,
            // Widening the pointed-to storage would make a 16-bit atomic a
            // different atomic object. A compare-exchange emulation is needed
            // before that can be represented exactly.
            InstructionKind::Atomic { pointer, value, .. } => {
                let narrow_pointer =
                    type_of(original_values, pointer).is_some_and(|ty| self.has(&ty));
                let narrow_value = type_of(original_values, value).is_some_and(|ty| self.has(&ty));
                if narrow_pointer || narrow_value {
                    return None;
                }
            }
            _ => {}
        }
        Some(())
    }

    fn sign_extend_short(
        self,
        operand: &mut Operand,
        original_values: &[Type],
        values: &mut Vec<Type>,
        instructions: &mut Vec<Instruction>,
    ) -> Option<()> {
        let ty = type_of(original_values, operand)?;
        if !self.short_value(&ty) {
            return Some(());
        }
        let shifted = ValueId(values.len() as u32);
        values.push(ty.clone());
        let extended = ValueId(values.len() as u32);
        values.push(ty.clone());
        let amount = integer_constant(&ty, 16)?;
        instructions.push(Instruction {
            result: Some(shifted),
            kind: InstructionKind::IntegerBinary {
                op: IntegerBinaryOp::ShiftLeft,
                lhs: operand.clone(),
                rhs: amount.clone(),
            },
        });
        instructions.push(Instruction {
            result: Some(extended),
            kind: InstructionKind::IntegerBinary {
                op: IntegerBinaryOp::ShiftRightArithmetic,
                lhs: Operand::Value(shifted),
                rhs: amount,
            },
        });
        *operand = Operand::Value(extended);
        Some(())
    }

    /// Wrap a widened integer back to Metal's canonical low 16 bits. The final
    /// instruction deliberately keeps `result`, so every original use sees
    /// the wrapped value while `source` remains the raw widened operation.
    fn wrap_short(
        self,
        source: Operand,
        ty: &Type,
        result: ValueId,
        _values: &mut Vec<Type>,
        instructions: &mut Vec<Instruction>,
    ) -> Option<()> {
        if !self.short_value(ty) {
            return None;
        }
        instructions.push(Instruction {
            result: Some(result),
            kind: InstructionKind::IntegerBinary {
                op: IntegerBinaryOp::And,
                lhs: source,
                rhs: integer_constant(ty, u16::MAX.into())?,
            },
        });
        Some(())
    }

    fn widen_operand(self, operand: &mut Operand) -> Option<()> {
        match operand {
            Operand::Float { ty, bits } if self.half && *ty == Type::Float { bits: 16 } => {
                *bits = u64::from(half_to_float_bits(*bits as u16));
                *ty = Type::Float { bits: 32 };
            }
            Operand::Integer { ty, value } if self.has(ty) => {
                if let Type::Int { signed: true, .. } = ty {
                    *value = u64::from(*value as u16 as i16 as i32 as u32);
                }
                *ty = self.widen_type(ty)?;
            }
            Operand::Composite { ty, elements } => {
                *ty = self.widen_type(ty)?;
                for element in elements {
                    self.widen_operand(element)?;
                }
            }
            Operand::Global { ty, .. } | Operand::Undef(ty) => *ty = self.widen_type(ty)?,
            _ => {}
        }
        Some(())
    }

    /// `None` when a narrow type is in buffer memory.
    fn widen_type(self, ty: &Type) -> Option<Type> {
        if !self.has(ty) {
            return Some(ty.clone());
        }
        Some(match ty {
            Type::Float { bits: 16 } => Type::Float { bits: 32 },
            Type::Int { bits: 16, signed } => Type::Int {
                bits: 32,
                signed: *signed,
            },
            Type::Vector { element, lanes } => Type::Vector {
                element: Box::new(self.widen_type(element)?),
                lanes: *lanes,
            },
            Type::Matrix { column, columns } => Type::Matrix {
                column: Box::new(self.widen_type(column)?),
                columns: *columns,
            },
            Type::Array { element, count } => Type::Array {
                element: Box::new(self.widen_type(element)?),
                count: *count,
            },
            Type::Struct(structure) => Type::Struct(self.widen_struct(structure)?),
            Type::Pointer { space, pointee } => match space {
                AddressSpace::Device | AddressSpace::Constant => return None,
                _ => Type::Pointer {
                    space: *space,
                    pointee: Box::new(self.widen_type(pointee)?),
                },
            },
            Type::Texture(texture) => Type::Texture(TextureType {
                texel: Box::new(self.widen_type(&texture.texel)?),
                ..texture.clone()
            }),
            _ => return None,
        })
    }

    fn widen_struct(self, structure: &StructType) -> Option<StructType> {
        if structure.packed {
            return None;
        }
        let mut fields = Vec::with_capacity(structure.fields.len());
        let mut end = 0u32;
        let mut alignment = 1u32;
        for field in &structure.fields {
            let ty = self.widen_type(&field.ty)?;
            let (size, align) = size_and_alignment(&ty)?;
            let offset = end.next_multiple_of(align);
            end = offset + size;
            alignment = alignment.max(align);
            fields.push(StructField {
                name: field.name.clone(),
                ty,
                offset,
            });
        }
        Some(StructType {
            fields,
            size: end.next_multiple_of(alignment),
            alignment,
            packed: false,
        })
    }

    fn has(self, ty: &Type) -> bool {
        match ty {
            Type::Float { bits: 16 } => self.half,
            Type::Int { bits: 16, .. } => self.short,
            Type::Vector { element, .. } | Type::Array { element, .. } => self.has(element),
            Type::Matrix { column, .. } => self.has(column),
            Type::Struct(structure) => structure.fields.iter().any(|field| self.has(&field.ty)),
            Type::Pointer { pointee, .. } => self.has(pointee),
            Type::Texture(texture) => self.has(&texture.texel),
            _ => false,
        }
    }
}

fn integer_constant(ty: &Type, value: u64) -> Option<Operand> {
    match ty {
        Type::Int { .. } => Some(Operand::Integer {
            ty: ty.clone(),
            value,
        }),
        Type::Vector { element, lanes } if matches!(element.as_ref(), Type::Int { .. }) => {
            Some(Operand::Composite {
                ty: ty.clone(),
                elements: (0..*lanes)
                    .map(|_| Operand::Integer {
                        ty: element.as_ref().clone(),
                        value,
                    })
                    .collect(),
            })
        }
        _ => None,
    }
}

fn size_and_alignment(ty: &Type) -> Option<(u32, u32)> {
    Some(match ty {
        Type::Bool => (1, 1),
        Type::Int { bits, .. } | Type::Float { bits } => {
            let bytes = u32::from(*bits).div_ceil(8);
            (bytes, bytes)
        }
        Type::Vector { element, lanes } => {
            let (element, _) = size_and_alignment(element)?;
            let size = element * u32::from(*lanes).next_power_of_two();
            (size, size)
        }
        Type::Matrix { column, columns } => {
            let (column, align) = size_and_alignment(column)?;
            (column * u32::from(*columns), align)
        }
        Type::Array { element, count } => {
            let (size, align) = size_and_alignment(element)?;
            (size.next_multiple_of(align) * count, align)
        }
        Type::Struct(structure) => (structure.size, structure.alignment),
        _ => return None,
    })
}

fn type_of(values: &[Type], operand: &Operand) -> Option<Type> {
    match operand {
        Operand::Value(value) => values.get(value.0 as usize).cloned(),
        Operand::Bool(_) => Some(Type::Bool),
        Operand::Integer { ty, .. }
        | Operand::Float { ty, .. }
        | Operand::Composite { ty, .. }
        | Operand::Global { ty, .. }
        | Operand::Undef(ty) => Some(ty.clone()),
    }
}

fn is_half2(ty: &Type) -> bool {
    matches!(ty, Type::Vector { element, lanes: 2 } if **element == Type::Float { bits: 16 })
}

fn half_to_float_bits(half: u16) -> u32 {
    let sign = u32::from(half >> 15) << 31;
    let exponent = u32::from((half >> 10) & 0x1f);
    let mantissa = u32::from(half & 0x3ff);
    match (exponent, mantissa) {
        (0, 0) => sign,
        // Subnormal: normalise it.
        (0, _) => {
            let shift = mantissa.leading_zeros() - 21;
            let mantissa = (mantissa << shift) & 0x3ff;
            sign | ((113 - shift) << 23) | (mantissa << 13)
        }
        (0x1f, _) => sign | 0x7f80_0000 | (mantissa << 13),
        _ => sign | ((exponent + 112) << 23) | (mantissa << 13),
    }
}
