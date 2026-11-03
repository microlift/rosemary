//! Compute `half` as `float` on targets without `shaderFloat16`, and a
//! `short` texture's texels as `int` on targets without `shaderInt16`.
//!
//! FIXME (ACCURACY): the wider type keeps the precision and range the narrow
//! one would round or wrap away.
//!
//! A narrow type in a buffer keeps the guest's layout, so a module with one is
//! left unchanged and still needs the capability.

use super::*;
use crate::ir::{StructField, StructType, TextureType};
use crate::passes::Substitution;

/// The narrow types a target cannot compute with.
#[derive(Clone, Copy)]
struct Narrow {
    half: bool,
    short: bool,
}

/// The module with each narrow type `target` lacks widened, or `None` if
/// there is nothing to change or it can't be changed.
pub(super) fn widen(module: &Module, target: &TargetProfile) -> Option<Module> {
    // Widening changes a `short`'s arithmetic, so it is kept to modules
    // reading a `short` texture, which have no other way to run there.
    let short_texture = |ty: &Type| match ty {
        Type::Texture(texture) => matches!(*texture.texel, Type::Int { bits: 16, .. }),
        _ => false,
    };
    let narrow = Narrow {
        half: !target.shader_float16,
        short: !target.shader_int16
            && module.functions.iter().any(|function| {
                function
                    .parameters
                    .iter()
                    .any(|parameter| short_texture(&parameter.ty))
            }),
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
        let mut substitution = Substitution::default();
        let values = &function.values;
        for block in &mut function.blocks {
            let mut kept = Vec::with_capacity(block.instructions.len());
            for mut instruction in std::mem::take(&mut block.instructions) {
                if let InstructionKind::Convert { op, value } = &mut instruction.kind {
                    let source = type_of(values, value)?;
                    let result = values.get(instruction.result?.0 as usize)?.clone();
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
                            substitution.insert(instruction.result?, value.clone());
                            continue;
                        }
                        _ => {}
                    }
                }
                kept.push(instruction);
            }
            block.instructions = kept;
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
