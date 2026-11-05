use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::*;
use crate::{Diagnostic, Phase};

pub(super) fn verify_function(function: &Function, diagnostics: &mut Vec<Diagnostic>) {
    let blocks: BTreeSet<_> = function.blocks.iter().map(|block| block.id).collect();
    if blocks.len() != function.blocks.len() || function.blocks.is_empty() {
        diagnostics.push(Diagnostic::new(
            Phase::IrVerification,
            format!("{} has duplicate block IDs or no blocks", function.name),
        ));
        return;
    }
    let mut predecessors: BTreeMap<BlockId, BTreeSet<BlockId>> = BTreeMap::new();
    for block in &function.blocks {
        for successor in successors(&block.terminator) {
            if !blocks.contains(&successor) {
                diagnostics.push(Diagnostic::new(
                    Phase::IrVerification,
                    format!("{} branches to a missing block", function.name),
                ));
            }
            predecessors.entry(successor).or_default().insert(block.id);
        }
    }
    let entry = function.blocks[0].id;
    let dominators = Dominators::new(&blocks, &predecessors, entry);
    let mut definitions = BTreeMap::new();
    for parameter in &function.parameters {
        if function.values.get(parameter.value.0 as usize) != Some(&parameter.ty) {
            diagnostics.push(Diagnostic::new(
                Phase::IrVerification,
                format!(
                    "{} has a parameter whose value type does not match",
                    function.name
                ),
            ));
        }
        if definitions.insert(parameter.value, None).is_some() {
            diagnostics.push(Diagnostic::new(
                Phase::IrVerification,
                format!("{} defines an SSA value more than once", function.name),
            ));
        }
    }
    for block in &function.blocks {
        let mut saw_non_phi = false;
        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            let is_phi = matches!(instruction.kind, InstructionKind::Phi { .. });
            if is_phi && saw_non_phi {
                diagnostics.push(Diagnostic::new(
                    Phase::IrVerification,
                    format!("{} has a phi after a non-phi instruction", function.name),
                ));
            }
            saw_non_phi |= !is_phi;
            if let Some(value) = instruction.result
                && value.0 as usize >= function.values.len()
            {
                diagnostics.push(Diagnostic::new(
                    Phase::IrVerification,
                    format!("{} defines an out-of-range value", function.name),
                ));
            }
            if let Some(value) = instruction.result
                && definitions
                    .insert(value, Some((block.id, instruction_index)))
                    .is_some()
            {
                diagnostics.push(Diagnostic::new(
                    Phase::IrVerification,
                    format!("{} defines an SSA value more than once", function.name),
                ));
            }
            if let InstructionKind::Phi { incomings } = &instruction.kind {
                let actual: BTreeSet<_> = incomings.iter().map(|(from, _)| *from).collect();
                let expected = predecessors.get(&block.id).cloned().unwrap_or_default();
                if actual != expected || actual.len() != incomings.len() {
                    diagnostics.push(Diagnostic::new(
                        Phase::IrVerification,
                        format!(
                            "{} has phi inputs which do not match its predecessors",
                            function.name
                        ),
                    ));
                }
            }
        }
    }
    for block in &function.blocks {
        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            verify_instruction_types(function, instruction, diagnostics);
            match &instruction.kind {
                InstructionKind::Phi { incomings } => {
                    for (predecessor, operand) in incomings {
                        verify_use(
                            function,
                            operand,
                            *predecessor,
                            usize::MAX,
                            &definitions,
                            &dominators,
                            diagnostics,
                        );
                    }
                }
                kind => {
                    for operand in instruction_operands(kind) {
                        verify_use(
                            function,
                            operand,
                            block.id,
                            instruction_index,
                            &definitions,
                            &dominators,
                            diagnostics,
                        );
                    }
                }
            }
        }
        for operand in terminator_operands(&block.terminator) {
            verify_use(
                function,
                operand,
                block.id,
                usize::MAX,
                &definitions,
                &dominators,
                diagnostics,
            );
        }
        if let Terminator::CondBranch { condition, .. } = &block.terminator
            && operand_type(function, condition) != Some(Type::Bool)
        {
            diagnostics.push(Diagnostic::new(
                Phase::IrVerification,
                format!("{} branches on a non-boolean value", function.name),
            ));
        }
        match &block.terminator {
            Terminator::Return(None) if function.result != Type::Void => {
                diagnostics.push(Diagnostic::new(
                    Phase::IrVerification,
                    format!(
                        "{} returns no value from a non-void function",
                        function.name
                    ),
                ))
            }
            Terminator::Return(Some(value))
                if operand_type(function, value) != Some(function.result.clone()) =>
            {
                diagnostics.push(Diagnostic::new(
                    Phase::IrVerification,
                    format!("{} returns a value of the wrong type", function.name),
                ));
            }
            _ => {}
        }
    }
}

type Definition = Option<(BlockId, usize)>;

/// Which blocks dominate which: a dominator tree over the blocks the entry
/// reaches, and dominator sets over the rest.
struct Dominators {
    reachable: Dominance,
    /// Each unreachable block's dominators by the maximal solution of the
    /// set equations: only itself where nothing branches in, and every block
    /// where only an unreachable cycle does. Typically empty.
    unreachable: BTreeMap<BlockId, BTreeSet<BlockId>>,
}

impl Dominators {
    fn new(
        blocks: &BTreeSet<BlockId>,
        predecessors: &BTreeMap<BlockId, BTreeSet<BlockId>>,
        entry: BlockId,
    ) -> Self {
        let edges: HashMap<BlockId, Vec<BlockId>> = predecessors
            .iter()
            .map(|(block, incoming)| (*block, incoming.iter().copied().collect()))
            .collect();
        let reachable = Dominance::new(&[entry], &edges);
        let mut reached = BTreeSet::from([entry]);
        let mut pending = vec![entry];
        let mut successors: BTreeMap<BlockId, Vec<BlockId>> = BTreeMap::new();
        for (block, incoming) in predecessors {
            for source in incoming {
                successors.entry(*source).or_default().push(*block);
            }
        }
        while let Some(block) = pending.pop() {
            for successor in successors.get(&block).into_iter().flatten() {
                if reached.insert(*successor) {
                    pending.push(*successor);
                }
            }
        }
        // Every predecessor of an unreachable block is itself unreachable, so
        // these equations never consult the tree.
        let unreached: Vec<_> = blocks.difference(&reached).copied().collect();
        let mut unreachable: BTreeMap<_, _> = unreached
            .iter()
            .map(|block| (*block, blocks.clone()))
            .collect();
        let mut changed = !unreached.is_empty();
        while changed {
            changed = false;
            for block in &unreached {
                let mut incoming = predecessors.get(block).into_iter().flatten();
                let mut next = match incoming.next() {
                    None => BTreeSet::new(),
                    Some(first) => incoming.fold(unreachable[first].clone(), |set, predecessor| {
                        set.intersection(&unreachable[predecessor])
                            .copied()
                            .collect()
                    }),
                };
                next.insert(*block);
                if unreachable[block] != next {
                    unreachable.insert(*block, next);
                    changed = true;
                }
            }
        }
        Self {
            reachable,
            unreachable,
        }
    }

    fn dominates(&self, dominator: BlockId, block: BlockId) -> bool {
        match self.unreachable.get(&block) {
            Some(set) => set.contains(&dominator),
            None => self.reachable.dominates(dominator, block),
        }
    }
}

fn verify_use(
    function: &Function,
    operand: &Operand,
    use_block: BlockId,
    use_index: usize,
    definitions: &BTreeMap<ValueId, Definition>,
    dominators: &Dominators,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Operand::Value(value) = operand else {
        return;
    };
    let Some(definition) = definitions.get(value) else {
        diagnostics.push(Diagnostic::new(
            Phase::IrVerification,
            format!("{} uses an undefined SSA value", function.name),
        ));
        return;
    };
    if let Some((definition_block, definition_index)) = definition {
        let dominates = if *definition_block == use_block {
            *definition_index < use_index
        } else {
            dominators.dominates(*definition_block, use_block)
        };
        if !dominates {
            diagnostics.push(Diagnostic::new(
                Phase::IrVerification,
                format!(
                    "{} has an SSA use not dominated by its definition",
                    function.name
                ),
            ));
        }
    }
}

fn verify_instruction_types(
    function: &Function,
    instruction: &Instruction,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let result = instruction
        .result
        .and_then(|value| function.values.get(value.0 as usize))
        .cloned();
    let valid = match &instruction.kind {
        InstructionKind::LocalVariable { .. }
        | InstructionKind::Load { .. }
        | InstructionKind::Store { .. }
        | InstructionKind::ElementAddress { .. }
        | InstructionKind::FieldAddress { .. }
        | InstructionKind::BufferAddress { .. } => {
            memory_is_well_typed(function, &instruction.kind, result)
        }
        InstructionKind::IntegerBinary { .. }
        | InstructionKind::IntegerUnary { .. }
        | InstructionKind::IntegerClamp { .. }
        | InstructionKind::IntegerCompare { .. }
        | InstructionKind::FloatBinary { .. }
        | InstructionKind::FloatUnary { .. }
        | InstructionKind::FloatCompare { .. }
        | InstructionKind::FloatFma { .. }
        | InstructionKind::FloatDot { .. }
        | InstructionKind::FloatClamp { .. }
        | InstructionKind::FloatMix { .. } => {
            arithmetic_is_well_typed(function, &instruction.kind, result)
        }
        InstructionKind::CompositeExtract { .. }
        | InstructionKind::CompositeInsert { .. }
        | InstructionKind::VectorExtractDynamic { .. }
        | InstructionKind::VectorInsertDynamic { .. }
        | InstructionKind::VectorShuffle { .. }
        | InstructionKind::VectorSplat { .. }
        | InstructionKind::All { .. }
        | InstructionKind::Any { .. }
        | InstructionKind::Select { .. }
        | InstructionKind::Phi { .. } => {
            composite_is_well_typed(function, &instruction.kind, result)
        }
        InstructionKind::Convert { .. } => {
            convert_is_well_typed(function, &instruction.kind, result)
        }
        InstructionKind::TextureSample { .. }
        | InstructionKind::TextureRead { .. }
        | InstructionKind::TextureQuerySize { .. }
        | InstructionKind::TextureWrite { .. } => {
            texture_is_well_typed(function, &instruction.kind, result)
        }
        InstructionKind::GlobalInvocationId
        | InstructionKind::DiscardFragment
        | InstructionKind::Barrier { .. }
        | InstructionKind::Atomic { .. }
        | InstructionKind::FunctionConstantDefined { .. } => {
            sync_is_well_typed(function, &instruction.kind, result)
        }
    };
    if !valid {
        invalid(function, instruction, diagnostics);
    }
}

/// Report an instruction whose operands do not have the required shape and
/// stop checking it.
fn invalid(function: &Function, instruction: &Instruction, diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.push(Diagnostic::new(
        Phase::IrVerification,
        format!(
            "{} has an ill-typed {}",
            function.name,
            instruction_name(&instruction.kind)
        ),
    ));
}

fn optional_is(function: &Function, operand: Option<&Operand>, expected: &Type) -> bool {
    operand.is_none_or(|operand| operand_type(function, operand).as_ref() == Some(expected))
}

fn optional_integer_vector(function: &Function, operand: Option<&Operand>) -> bool {
    operand.is_none_or(|operand| {
        operand_type(function, operand)
            .as_ref()
            .is_some_and(is_integer_or_integer_vector)
    })
}

fn coordinate_shape(
    function: &Function,
    coordinate: &Operand,
    texture: &TextureType,
    element: Type,
) -> bool {
    let lanes = coordinate_lanes(texture);
    let expected = if lanes == 1 {
        element
    } else {
        Type::Vector {
            element: Box::new(element),
            lanes,
        }
    };
    operand_type(function, coordinate) == Some(expected)
}

/// Metal always returns four components from a texture access, whatever the
/// texture's storage format.
fn texel_vector(texture: &TextureType) -> Type {
    Type::Vector {
        element: texture.texel.clone(),
        lanes: 4,
    }
}

fn is_float_or_float_vector(ty: &Type) -> bool {
    matches!(ty, Type::Float { .. })
        || matches!(ty, Type::Vector { element, .. } if matches!(element.as_ref(), Type::Float { .. }))
}

fn is_integer_or_integer_vector(ty: &Type) -> bool {
    matches!(ty, Type::Int { .. })
        || matches!(ty, Type::Vector { element, .. } if matches!(element.as_ref(), Type::Int { .. }))
}

/// The boolean result of an elementwise comparison keeps the operand's shape:
/// a scalar comparison yields `bool`, a vector comparison yields `bool` lanes.
fn boolean_shape(ty: Type) -> Type {
    match ty {
        Type::Vector { lanes, .. } => Type::Vector {
            element: Box::new(Type::Bool),
            lanes,
        },
        _ => Type::Bool,
    }
}

/// Conversions keep the operand shape and only change the element type.
///
/// A bitcast is the exception: it reinterprets the same bits, so it may change
/// the lane count as long as the total width is unchanged. Metal reads a
/// `float4` back as a `half8` or a `uint2` as a `float2` this way.
fn verify_convert(op: ConvertOp, source: Option<Type>, result: Option<&Type>) -> bool {
    let (Some(source), Some(result)) = (source, result) else {
        return false;
    };
    let float2 = Type::Vector {
        element: Box::new(Type::Float { bits: 32 }),
        lanes: 2,
    };
    match op {
        ConvertOp::UnpackHalf2x16 => {
            return matches!(source, Type::Int { bits: 32, .. }) && *result == float2;
        }
        ConvertOp::PackHalf2x16 => {
            return source == float2 && matches!(result, Type::Int { bits: 32, .. });
        }
        _ => {}
    }
    if op == ConvertOp::Bitcast && reinterpretable(&source) && reinterpretable(result) {
        return total_bits(&source) == total_bits(result) && total_bits(&source).is_some();
    }
    let (source, result) = match (source, result) {
        (
            Type::Vector {
                element: source,
                lanes,
            },
            Type::Vector {
                element: result,
                lanes: result_lanes,
            },
        ) if lanes == *result_lanes => (*source, result.as_ref().clone()),
        (source @ (Type::Int { .. } | Type::Float { .. } | Type::Bool), result) => {
            (source, result.clone())
        }
        _ => return false,
    };
    match (op, &source, &result) {
        // Metal's `bool` is LLVM's `i1`, which widens and narrows like any
        // other integer even though SPIR-V keeps booleans a separate type.
        (
            ConvertOp::ZeroExtend | ConvertOp::SignExtend,
            Type::Bool,
            Type::Int { .. } | Type::Float { .. },
        )
        | (ConvertOp::UnsignedToFloat | ConvertOp::SignedToFloat, Type::Bool, Type::Float { .. })
        | (ConvertOp::IntegerTruncate, Type::Int { .. }, Type::Bool) => true,
        (
            ConvertOp::UnsignedToFloat | ConvertOp::SignedToFloat,
            Type::Int { .. },
            Type::Float { .. },
        )
        | (
            ConvertOp::FloatToUnsigned | ConvertOp::FloatToSigned,
            Type::Float { .. },
            Type::Int { .. },
        ) => true,
        (
            ConvertOp::ZeroExtend | ConvertOp::SignExtend,
            Type::Int { bits: from, .. },
            Type::Int { bits: to, .. },
        ) => to > from,
        (ConvertOp::IntegerTruncate, Type::Int { bits: from, .. }, Type::Int { bits: to, .. }) => {
            to < from
        }
        (ConvertOp::FloatResize, Type::Float { bits: from }, Type::Float { bits: to }) => {
            from != to
        }
        // A bitcast is only meaningful between equally sized representations.
        (ConvertOp::Bitcast, _, _) => scalar_bits(&source) == scalar_bits(&result),
        _ => false,
    }
}

/// Whether a type is a numeric scalar or vector, whose bits a bitcast may
/// reinterpret as another such type of the same total width.
fn reinterpretable(ty: &Type) -> bool {
    match ty {
        Type::Int { .. } | Type::Float { .. } => true,
        Type::Vector { element, .. } => {
            matches!(**element, Type::Int { .. } | Type::Float { .. })
        }
        _ => false,
    }
}

fn total_bits(ty: &Type) -> Option<u32> {
    match ty {
        Type::Int { bits, .. } | Type::Float { bits } => Some(u32::from(*bits)),
        Type::Vector { element, lanes } => Some(total_bits(element)? * u32::from(*lanes)),
        _ => None,
    }
}

fn scalar_bits(ty: &Type) -> Option<u8> {
    match ty {
        Type::Int { bits, .. } | Type::Float { bits } => Some(*bits),
        _ => None,
    }
}

/// Whether pointers, loads, stores and address computation has the operand and result types Metal gives it.
fn memory_is_well_typed(function: &Function, kind: &InstructionKind, result: Option<Type>) -> bool {
    match kind {
        InstructionKind::LocalVariable { ty } => matches!(
            &result,
            Some(Type::Pointer { space, pointee })
                if *space == AddressSpace::Thread && pointee.as_ref() == ty
        ),
        InstructionKind::Load { pointer } => match operand_type(function, pointer) {
            Some(Type::Pointer { pointee, .. }) => result.as_ref() == Some(pointee.as_ref()),
            _ => false,
        },
        InstructionKind::Store { pointer, value } => match operand_type(function, pointer) {
            Some(Type::Pointer { pointee, .. }) => {
                Some(pointee.as_ref().clone()) == operand_type(function, value)
            }
            _ => false,
        },
        InstructionKind::ElementAddress { pointer, index } => {
            matches!(result, Some(Type::Pointer { .. }))
                && matches!(operand_type(function, pointer), Some(Type::Pointer { .. }))
                && operand_type(function, index).is_some_and(|ty| matches!(ty, Type::Int { .. }))
        }
        InstructionKind::FieldAddress { pointer, .. } => {
            matches!(result, Some(Type::Pointer { .. }))
                && matches!(operand_type(function, pointer), Some(Type::Pointer { .. }))
        }
        InstructionKind::BufferAddress { pointer, indices } => (|| {
            let Some(Type::Pointer { space, pointee }) = operand_type(function, pointer) else {
                return false;
            };
            let mut ty = *pointee;
            let mut valid = !indices.is_empty();
            for (position, index) in indices.iter().enumerate() {
                valid &= matches!(operand_type(function, index), Some(Type::Int { .. }));
                if position == 0 {
                    continue;
                }
                ty = match ty {
                    Type::Struct(ref structure) => {
                        if let Operand::Integer { value, .. } = index {
                            if let Some(field) = structure.fields.get(*value as usize) {
                                field.ty.clone()
                            } else {
                                valid = false;
                                Type::Void
                            }
                        } else {
                            valid = false;
                            Type::Void
                        }
                    }
                    Type::Array { element, .. } | Type::Vector { element, .. } => *element,
                    _ => {
                        valid = false;
                        Type::Void
                    }
                };
            }
            valid
                && result
                    == Some(Type::Pointer {
                        space,
                        pointee: Box::new(ty),
                    })
        })(),
        _ => unreachable!("not a memory instruction"),
    }
}

/// Whether integer and floating-point arithmetic and comparison has the operand and result types Metal gives it.
fn arithmetic_is_well_typed(
    function: &Function,
    kind: &InstructionKind,
    result: Option<Type>,
) -> bool {
    match kind {
        InstructionKind::IntegerBinary { op, lhs, rhs } => {
            // LLVM types a Metal `bool` as `i1`, so bitwise operations stand
            // in for logical ones.
            let boolean = matches!(
                op,
                IntegerBinaryOp::And | IntegerBinaryOp::Or | IntegerBinaryOp::Xor
            ) && result.as_ref().is_some_and(is_boolean_or_boolean_vector);
            (boolean || result.as_ref().is_some_and(is_integer_or_integer_vector))
                && result == operand_type(function, lhs)
                && result == operand_type(function, rhs)
        }
        InstructionKind::IntegerUnary { value, .. } => {
            result == operand_type(function, value)
                && result.as_ref().is_some_and(is_integer_or_integer_vector)
        }
        InstructionKind::IntegerClamp {
            value, min, max, ..
        } => {
            result == operand_type(function, value)
                && result == operand_type(function, min)
                && result == operand_type(function, max)
                && result.as_ref().is_some_and(is_integer_or_integer_vector)
        }
        InstructionKind::IntegerCompare { lhs, rhs, .. } => {
            operand_type(function, lhs) == operand_type(function, rhs)
                && operand_type(function, lhs)
                    .as_ref()
                    .is_some_and(is_integer_or_integer_vector)
                && result == operand_type(function, lhs).map(boolean_shape)
        }
        InstructionKind::FloatBinary { lhs, rhs, .. } => {
            result == operand_type(function, lhs)
                && result == operand_type(function, rhs)
                && result.as_ref().is_some_and(is_float_or_float_vector)
        }
        InstructionKind::FloatUnary { value, .. } => {
            result == operand_type(function, value)
                && result.as_ref().is_some_and(is_float_or_float_vector)
        }
        InstructionKind::FloatCompare { lhs, rhs, .. } => {
            operand_type(function, lhs) == operand_type(function, rhs)
                && operand_type(function, lhs)
                    .as_ref()
                    .is_some_and(is_float_or_float_vector)
                && result == operand_type(function, lhs).map(boolean_shape)
        }
        InstructionKind::FloatFma {
            first,
            second,
            addend,
        } => {
            result == operand_type(function, first)
                && result == operand_type(function, second)
                && result == operand_type(function, addend)
                && result.as_ref().is_some_and(is_float_or_float_vector)
        }
        InstructionKind::FloatDot { lhs, rhs } => {
            operand_type(function, lhs) == operand_type(function, rhs)
                && matches!(operand_type(function, lhs), Some(Type::Vector { element, .. }) if matches!(element.as_ref(), Type::Float { .. }))
                && result == Some(Type::Float { bits: 32 })
        }
        InstructionKind::FloatClamp { value, min, max } => {
            result == operand_type(function, value)
                && result == operand_type(function, min)
                && result == operand_type(function, max)
                && result.as_ref().is_some_and(is_float_or_float_vector)
        }
        InstructionKind::FloatMix {
            first,
            second,
            weight,
        } => {
            result == operand_type(function, first)
                && result == operand_type(function, second)
                && result == operand_type(function, weight)
                && result.as_ref().is_some_and(is_float_or_float_vector)
        }
        _ => unreachable!("not a arithmetic instruction"),
    }
}

/// Whether vector and aggregate construction, extraction and selection has the operand and result types Metal gives it.
fn composite_is_well_typed(
    function: &Function,
    kind: &InstructionKind,
    result: Option<Type>,
) -> bool {
    match kind {
        InstructionKind::CompositeExtract { composite, index } => {
            match operand_type(function, composite) {
                Some(Type::Struct(structure)) => {
                    structure
                        .fields
                        .get(*index as usize)
                        .map(|field| field.ty.clone())
                        == result
                }
                Some(Type::Vector { element, lanes }) => {
                    *index < u32::from(lanes) && result.as_ref() == Some(element.as_ref())
                }
                Some(Type::Array { element, count }) => {
                    *index < count && result.as_ref() == Some(element.as_ref())
                }
                _ => false,
            }
        }
        InstructionKind::CompositeInsert {
            composite,
            object,
            index,
        } => match operand_type(function, composite) {
            Some(Type::Vector { element, lanes }) => {
                *index < u32::from(lanes)
                    && operand_type(function, object) == Some(*element)
                    && result == operand_type(function, composite)
            }
            Some(Type::Array { element, count }) => {
                *index < count
                    && operand_type(function, object) == Some(*element)
                    && result == operand_type(function, composite)
            }
            Some(Type::Struct(structure)) => {
                structure
                    .fields
                    .get(*index as usize)
                    .map(|field| field.ty.clone())
                    == operand_type(function, object)
                    && result == operand_type(function, composite)
            }
            _ => false,
        },
        InstructionKind::VectorExtractDynamic { vector, index } => {
            matches!(operand_type(function, index), Some(Type::Int { .. }))
                && matches!(operand_type(function, vector),
                    Some(Type::Vector { element, .. }) if result.as_ref() == Some(element.as_ref()))
        }
        InstructionKind::VectorInsertDynamic {
            vector,
            object,
            index,
        } => {
            matches!(operand_type(function, index), Some(Type::Int { .. }))
                && matches!(operand_type(function, vector),
                    Some(Type::Vector { ref element, .. }) if operand_type(function, object).as_ref() == Some(element.as_ref()))
                && result == operand_type(function, vector)
        }
        InstructionKind::VectorShuffle {
            first,
            second,
            components,
        } => match operand_type(function, first) {
            Some(Type::Vector { element, lanes }) => {
                operand_type(function, second)
                    == Some(Type::Vector {
                        element: element.clone(),
                        lanes,
                    })
                    && result
                        == Some(Type::Vector {
                            element,
                            lanes: components.len() as u8,
                        })
            }
            _ => false,
        },
        InstructionKind::VectorSplat { value, lanes } => {
            result
                == operand_type(function, value).map(|element| Type::Vector {
                    element: Box::new(element),
                    lanes: *lanes,
                })
        }
        InstructionKind::All { vector } | InstructionKind::Any { vector } => {
            matches!(operand_type(function, vector), Some(Type::Vector { element, .. }) if *element == Type::Bool)
                && result == Some(Type::Bool)
        }
        InstructionKind::Select {
            condition,
            on_true,
            on_false,
        } => {
            result == operand_type(function, on_true)
                && result == operand_type(function, on_false)
                && (operand_type(function, condition) == Some(Type::Bool)
                    || matches!((&result, operand_type(function, condition)), (Some(Type::Vector { lanes, .. }), Some(Type::Vector { element, lanes: n })) if *element == Type::Bool && *lanes == n))
        }
        InstructionKind::Phi { incomings } => {
            result.is_some()
                && incomings
                    .iter()
                    .all(|(_, value)| operand_type(function, value) == result)
        }
        _ => unreachable!("not a composite instruction"),
    }
}

/// Whether conversion between scalar and vector types has the operand and result types Metal gives it.
fn convert_is_well_typed(
    function: &Function,
    kind: &InstructionKind,
    result: Option<Type>,
) -> bool {
    match kind {
        InstructionKind::Convert { op, value } => {
            verify_convert(*op, operand_type(function, value), result.as_ref())
        }
        _ => unreachable!("not a convert instruction"),
    }
}

/// Whether texture sampling, reads, writes and queries has the operand and result types Metal gives it.
fn texture_is_well_typed(
    function: &Function,
    kind: &InstructionKind,
    result: Option<Type>,
) -> bool {
    match kind {
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
            gather,
        } => {
            let Some(Type::Texture(texture)) = operand_type(function, texture) else {
                return false;
            };
            // A comparison yields one value, only from a depth texture, and
            // never alongside a gather.
            let compare_agrees = match compare {
                None => result == Some(texel_vector(&texture)),
                Some(reference) => {
                    texture.depth
                        && gather.is_none()
                        && operand_type(function, reference) == Some(Type::Float { bits: 32 })
                        && result == Some(Type::Float { bits: 32 })
                }
            };
            // A gradient names the level of detail, so a sample carries one or
            // the other and never both.
            let gradients_agree = match gradient.as_deref() {
                None => true,
                Some((dx, dy)) => {
                    lod.is_none()
                        && operand_type(function, dx).is_some()
                        && operand_type(function, dx) == operand_type(function, dy)
                }
            };
            // A bias adjusts the implicit level, so it excludes the other two.
            let bias_agrees = bias.is_none() || (lod.is_none() && gradient.is_none());
            gradients_agree
                && bias_agrees
                && operand_type(function, sampler) == Some(Type::Sampler)
                && texture.access == Access::Sample
                && coordinate_shape(function, coordinate, &texture, Type::Float { bits: 32 })
                && optional_is(function, array_index.as_ref(), &Type::u32())
                && optional_integer_vector(function, offset.as_ref())
                && optional_is(function, lod.as_ref(), &Type::Float { bits: 32 })
                && optional_is(function, bias.as_ref(), &Type::Float { bits: 32 })
                && compare_agrees
        }
        InstructionKind::TextureRead {
            texture,
            coordinate,
            array_index,
            lod,
            sample,
        } => {
            let Some(Type::Texture(texture)) = operand_type(function, texture) else {
                return false;
            };
            coordinate_shape(function, coordinate, &texture, Type::u32())
                && optional_is(function, array_index.as_ref(), &Type::u32())
                && optional_is(function, lod.as_ref(), &Type::u32())
                && optional_is(function, sample.as_ref(), &Type::u32())
                && result == Some(texel_vector(&texture))
        }
        InstructionKind::TextureQuerySize { texture, lod, .. } => {
            matches!(operand_type(function, texture), Some(Type::Texture(_)))
                && optional_is(function, lod.as_ref(), &Type::u32())
                && result == Some(Type::u32())
        }
        InstructionKind::TextureWrite {
            texture,
            coordinate,
            array_index,
            value,
        } => {
            let Some(Type::Texture(texture)) = operand_type(function, texture) else {
                return false;
            };
            result.is_none()
                && texture.access != Access::Sample
                && coordinate_shape(function, coordinate, &texture, Type::u32())
                && optional_is(function, array_index.as_ref(), &Type::u32())
                && operand_type(function, value) == Some(texel_vector(&texture))
        }
        _ => unreachable!("not a texture instruction"),
    }
}

/// Whether built-ins, barriers, atomics and fragment discard has the operand and result types Metal gives it.
fn sync_is_well_typed(function: &Function, kind: &InstructionKind, result: Option<Type>) -> bool {
    match kind {
        InstructionKind::GlobalInvocationId => {
            result
                == Some(Type::Vector {
                    element: Box::new(Type::u32()),
                    lanes: 3,
                })
        }
        InstructionKind::DiscardFragment => result.is_none(),
        InstructionKind::Barrier { .. } => result.is_none(),
        InstructionKind::Atomic { pointer, value, .. } => {
            // Metal's atomics are on 32-bit integers in `device` or
            // `threadgroup` memory, and the result is the value that was
            // there.
            let Some(Type::Pointer { space, pointee }) = operand_type(function, pointer) else {
                return false;
            };
            matches!(space, AddressSpace::Device | AddressSpace::Threadgroup)
                && matches!(*pointee, Type::Int { bits: 32, .. })
                && operand_type(function, value).as_ref() == Some(&*pointee)
                && result == Some(*pointee)
        }
        InstructionKind::FunctionConstantDefined { constant } => {
            result == Some(Type::Bool) && matches!(constant, Operand::Global { .. })
        }
        _ => unreachable!("not a sync instruction"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(result: u32, lhs: Operand) -> Instruction {
        Instruction {
            result: Some(ValueId(result)),
            kind: InstructionKind::IntegerBinary {
                op: IntegerBinaryOp::Add,
                lhs,
                rhs: Operand::u32(1),
            },
        }
    }

    /// `%0 = 1 + 1` in block `defines` and `%1 = %0 + 1` in block `uses`.
    fn function(blocks: Vec<(u32, Terminator)>, defines: u32, uses: u32) -> Function {
        Function {
            name: "f".to_string(),
            result: Type::Void,
            parameters: Vec::new(),
            values: vec![Type::u32(), Type::u32()],
            blocks: blocks
                .into_iter()
                .map(|(id, terminator)| {
                    let mut instructions = Vec::new();
                    if id == defines {
                        instructions.push(add(0, Operand::u32(1)));
                    }
                    if id == uses {
                        instructions.push(add(1, Operand::Value(ValueId(0))));
                    }
                    Block {
                        id: BlockId(id),
                        name: String::new(),
                        instructions,
                        terminator,
                    }
                })
                .collect(),
        }
    }

    fn branch(to: u32) -> Terminator {
        Terminator::Branch(BlockId(to))
    }

    fn either(a: u32, b: u32) -> Terminator {
        Terminator::CondBranch {
            condition: Operand::Bool(true),
            true_block: BlockId(a),
            false_block: BlockId(b),
        }
    }

    const END: Terminator = Terminator::Return(None);

    #[test]
    fn accepts_a_use_dominated_through_a_long_chain() {
        const LENGTH: u32 = 5000;
        let blocks = (0..LENGTH)
            .map(|id| {
                (
                    id,
                    if id + 1 == LENGTH {
                        END
                    } else {
                        branch(id + 1)
                    },
                )
            })
            .collect();
        assert!(function(blocks, 0, LENGTH - 1).verify().is_ok());
    }

    #[test]
    fn rejects_a_use_after_a_join_defined_on_one_arm() {
        let blocks = vec![(0, either(1, 2)), (1, branch(3)), (2, branch(3)), (3, END)];
        assert!(function(blocks, 1, 3).verify().is_err());
    }

    #[test]
    fn ignores_an_unreachable_predecessor_of_a_reachable_block() {
        let blocks = vec![(0, branch(2)), (1, branch(2)), (2, END)];
        assert!(function(blocks, 0, 2).verify().is_ok());
    }

    #[test]
    fn rejects_a_use_in_an_unreachable_block_nothing_branches_to() {
        let blocks = vec![(0, END), (1, END)];
        assert!(function(blocks, 0, 1).verify().is_err());
    }

    #[test]
    fn accepts_a_use_in_an_unreachable_cycle() {
        let blocks = vec![(0, END), (1, branch(2)), (2, branch(1))];
        assert!(function(blocks, 0, 2).verify().is_ok());
    }
}
