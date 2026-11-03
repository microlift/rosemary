use super::*;

/// The instruction's kind, for diagnostics. `Debug` would print every
/// operand, which is far more than a reader of a diagnostic needs.
pub(crate) fn instruction_name(kind: &InstructionKind) -> &'static str {
    match kind {
        InstructionKind::GlobalInvocationId => "global invocation ID",
        InstructionKind::LocalVariable { .. } => "local variable",
        InstructionKind::Load { .. } => "load",
        InstructionKind::Store { .. } => "store",
        InstructionKind::IntegerBinary { .. } => "integer arithmetic",
        InstructionKind::IntegerCompare { .. } => "integer comparison",
        InstructionKind::FloatCompare { .. } => "floating comparison",
        InstructionKind::ElementAddress { .. } => "element address",
        InstructionKind::FieldAddress { .. } => "field address",
        InstructionKind::BufferAddress { .. } => "buffer address",
        InstructionKind::CompositeExtract { .. } => "composite extraction",
        InstructionKind::VectorExtractDynamic { .. } => "dynamic vector extraction",
        InstructionKind::VectorInsertDynamic { .. } => "dynamic vector insertion",
        InstructionKind::CompositeInsert { .. } => "composite insertion",
        InstructionKind::All { .. } => "all",
        InstructionKind::Any { .. } => "any",
        InstructionKind::Convert { .. } => "conversion",
        InstructionKind::Select { .. } => "select",
        InstructionKind::FloatBinary { .. } => "floating arithmetic",
        InstructionKind::FloatUnary { .. } => "floating math call",
        InstructionKind::FloatFma { .. } => "fused multiply-add",
        InstructionKind::VectorShuffle { .. } => "vector shuffle",
        InstructionKind::FloatClamp { .. } => "clamp",
        InstructionKind::IntegerClamp { .. } => "integer clamp",
        InstructionKind::IntegerUnary { .. } => "integer math call",
        InstructionKind::FloatMix { .. } => "mix",
        InstructionKind::FloatDot { .. } => "dot product",
        InstructionKind::VectorSplat { .. } => "vector splat",
        InstructionKind::TextureSample { .. } => "texture sample",
        InstructionKind::TextureRead { .. } => "texture read",
        InstructionKind::TextureWrite { .. } => "texture write",
        InstructionKind::TextureQuerySize { .. } => "texture size query",
        InstructionKind::DiscardFragment => "fragment discard",
        InstructionKind::Barrier { .. } => "barrier",
        InstructionKind::Atomic { .. } => "atomic",
        InstructionKind::Phi { .. } => "phi",
        InstructionKind::FunctionConstantDefined { .. } => "function constant query",
    }
}

/// The number of coordinate components a texture dimension takes, excluding
/// the array slice, which Metal passes separately.
pub(crate) fn coordinate_lanes(texture: &TextureType) -> u8 {
    match texture.dimension {
        TextureDimension::D1 | TextureDimension::Buffer => 1,
        TextureDimension::D2 => 2,
        TextureDimension::D3 | TextureDimension::Cube => 3,
    }
}

pub(crate) fn operand_type(function: &Function, operand: &Operand) -> Option<Type> {
    match operand {
        Operand::Composite { ty, .. } => Some(ty.clone()),
        Operand::Value(value) => function.values.get(value.0 as usize).cloned(),
        Operand::Integer { ty, .. } => Some(ty.clone()),
        Operand::Float { ty, .. } => Some(ty.clone()),
        Operand::Global { ty, .. } => Some(ty.clone()),
        Operand::Undef(ty) => Some(ty.clone()),
        Operand::Bool(_) => Some(Type::Bool),
    }
}

pub(crate) fn instruction_operands(kind: &InstructionKind) -> Vec<&Operand> {
    match kind {
        InstructionKind::LocalVariable { .. } => Vec::new(),
        InstructionKind::Load { pointer } => vec![pointer],
        InstructionKind::Store { pointer, value } => vec![pointer, value],
        InstructionKind::IntegerBinary { lhs, rhs, .. }
        | InstructionKind::IntegerCompare { lhs, rhs, .. }
        | InstructionKind::FloatCompare { lhs, rhs, .. } => vec![lhs, rhs],
        InstructionKind::ElementAddress { pointer, index } => vec![pointer, index],
        InstructionKind::FieldAddress { pointer, .. } => vec![pointer],
        InstructionKind::CompositeExtract { composite, .. } => vec![composite],
        InstructionKind::VectorExtractDynamic { vector, index } => vec![vector, index],
        InstructionKind::VectorInsertDynamic {
            vector,
            object,
            index,
        } => vec![vector, object, index],
        InstructionKind::BufferAddress { pointer, indices } => {
            std::iter::once(pointer).chain(indices).collect()
        }
        InstructionKind::FloatBinary { lhs, rhs, .. } | InstructionKind::FloatDot { lhs, rhs } => {
            vec![lhs, rhs]
        }
        InstructionKind::FloatFma {
            first,
            second,
            addend,
        } => vec![first, second, addend],
        InstructionKind::Select {
            condition,
            on_true,
            on_false,
        } => vec![condition, on_true, on_false],
        InstructionKind::VectorShuffle { first, second, .. } => vec![first, second],
        InstructionKind::CompositeInsert {
            composite, object, ..
        } => vec![composite, object],
        InstructionKind::FloatClamp { value, min, max }
        | InstructionKind::IntegerClamp {
            value, min, max, ..
        } => vec![value, min, max],
        InstructionKind::FloatMix {
            first,
            second,
            weight,
        } => vec![first, second, weight],
        InstructionKind::All { vector }
        | InstructionKind::Any { vector }
        | InstructionKind::Convert { value: vector, .. }
        | InstructionKind::FloatUnary { value: vector, .. }
        | InstructionKind::IntegerUnary { value: vector, .. } => {
            vec![vector]
        }
        InstructionKind::VectorSplat { value, .. } => vec![value],
        InstructionKind::GlobalInvocationId => vec![],
        InstructionKind::TextureWrite {
            texture,
            coordinate,
            array_index,
            value,
        } => [texture, coordinate, value]
            .into_iter()
            .chain(array_index)
            .collect(),
        InstructionKind::TextureSample {
            texture,
            sampler,
            coordinate,
            array_index,
            offset,
            lod,
            gradient,
            bias,
            compare,
            ..
        } => [texture, sampler, coordinate]
            .into_iter()
            .chain(array_index)
            .chain(offset)
            .chain(lod)
            .chain(bias)
            .chain(compare)
            .chain(gradient.as_deref().map(|(dx, _)| dx))
            .chain(gradient.as_deref().map(|(_, dy)| dy))
            .collect(),
        InstructionKind::TextureRead {
            texture,
            coordinate,
            array_index,
            lod,
            sample,
        } => [texture, coordinate]
            .into_iter()
            .chain(array_index)
            .chain(lod)
            .chain(sample)
            .collect(),
        InstructionKind::TextureQuerySize { texture, lod, .. } => {
            [texture].into_iter().chain(lod).collect()
        }
        InstructionKind::DiscardFragment | InstructionKind::Barrier { .. } => Vec::new(),
        InstructionKind::Atomic { pointer, value, .. } => vec![pointer, value],
        InstructionKind::FunctionConstantDefined { constant } => vec![constant],
        InstructionKind::Phi { incomings } => incomings.iter().map(|(_, value)| value).collect(),
    }
}

pub(crate) fn instruction_operands_mut(kind: &mut InstructionKind) -> Vec<&mut Operand> {
    match kind {
        InstructionKind::LocalVariable { .. }
        | InstructionKind::GlobalInvocationId
        | InstructionKind::DiscardFragment
        | InstructionKind::Barrier { .. } => Vec::new(),
        InstructionKind::Load { pointer } => vec![pointer],
        InstructionKind::Store { pointer, value }
        | InstructionKind::Atomic { pointer, value, .. } => vec![pointer, value],
        InstructionKind::IntegerBinary { lhs, rhs, .. }
        | InstructionKind::IntegerCompare { lhs, rhs, .. }
        | InstructionKind::FloatCompare { lhs, rhs, .. }
        | InstructionKind::FloatBinary { lhs, rhs, .. }
        | InstructionKind::FloatDot { lhs, rhs } => vec![lhs, rhs],
        InstructionKind::ElementAddress { pointer, index } => vec![pointer, index],
        InstructionKind::FieldAddress { pointer, .. } => vec![pointer],
        InstructionKind::CompositeExtract { composite, .. } => vec![composite],
        InstructionKind::VectorExtractDynamic { vector, index } => vec![vector, index],
        InstructionKind::VectorInsertDynamic {
            vector,
            object,
            index,
        } => vec![vector, object, index],
        InstructionKind::BufferAddress { pointer, indices } => {
            std::iter::once(pointer).chain(indices).collect()
        }
        InstructionKind::FloatFma {
            first,
            second,
            addend,
        } => vec![first, second, addend],
        InstructionKind::Select {
            condition,
            on_true,
            on_false,
        } => vec![condition, on_true, on_false],
        InstructionKind::VectorShuffle { first, second, .. } => vec![first, second],
        InstructionKind::CompositeInsert {
            composite, object, ..
        } => vec![composite, object],
        InstructionKind::FloatClamp { value, min, max }
        | InstructionKind::IntegerClamp {
            value, min, max, ..
        } => vec![value, min, max],
        InstructionKind::FloatMix {
            first,
            second,
            weight,
        } => vec![first, second, weight],
        InstructionKind::All { vector }
        | InstructionKind::Any { vector }
        | InstructionKind::Convert { value: vector, .. }
        | InstructionKind::FloatUnary { value: vector, .. }
        | InstructionKind::IntegerUnary { value: vector, .. }
        | InstructionKind::VectorSplat { value: vector, .. } => vec![vector],
        InstructionKind::TextureWrite {
            texture,
            coordinate,
            array_index,
            value,
        } => {
            let mut operands = vec![texture, coordinate, value];
            operands.extend(array_index);
            operands
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
            compare,
            ..
        } => {
            let mut operands = vec![texture, sampler, coordinate];
            operands.extend(array_index);
            operands.extend(offset);
            operands.extend(lod);
            operands.extend(bias);
            operands.extend(compare);
            if let Some(gradient) = gradient {
                operands.push(&mut gradient.0);
                operands.push(&mut gradient.1);
            }
            operands
        }
        InstructionKind::TextureRead {
            texture,
            coordinate,
            array_index,
            lod,
            sample,
        } => {
            let mut operands = vec![texture, coordinate];
            operands.extend(array_index);
            operands.extend(lod);
            operands.extend(sample);
            operands
        }
        InstructionKind::TextureQuerySize { texture, lod, .. } => {
            let mut operands = vec![texture];
            operands.extend(lod);
            operands
        }
        InstructionKind::FunctionConstantDefined { constant } => vec![constant],
        InstructionKind::Phi { incomings } => {
            incomings.iter_mut().map(|(_, value)| value).collect()
        }
    }
}

pub(crate) fn is_boolean_or_boolean_vector(ty: &Type) -> bool {
    matches!(ty, Type::Bool)
        || matches!(ty, Type::Vector { element, .. } if matches!(element.as_ref(), Type::Bool))
}

pub(crate) fn terminator_operands(terminator: &Terminator) -> Vec<&Operand> {
    match terminator {
        Terminator::CondBranch { condition, .. } => vec![condition],
        Terminator::Return(Some(value)) => vec![value],
        _ => Vec::new(),
    }
}

pub(super) fn resource_namespace(kind: ResourceKind) -> u8 {
    match kind {
        ResourceKind::Buffer
        | ResourceKind::AccelerationStructure
        | ResourceKind::VisibleFunctionTable
        | ResourceKind::IntersectionFunctionTable
        | ResourceKind::ArgumentBuffer => 0,
        ResourceKind::Texture => 1,
        ResourceKind::Sampler => 2,
        ResourceKind::Imageblock => 3,
    }
}

pub(crate) fn successors(terminator: &Terminator) -> Vec<BlockId> {
    match terminator {
        Terminator::Branch(target) => vec![*target],
        Terminator::CondBranch {
            true_block,
            false_block,
            ..
        } => vec![*true_block, *false_block],
        Terminator::Return(_) | Terminator::Unreachable => Vec::new(),
    }
}
