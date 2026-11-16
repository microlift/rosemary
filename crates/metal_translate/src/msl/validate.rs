use std::collections::BTreeSet;

use crate::ir::{
    Function, InstructionKind, Module, Operand, instruction_operands, terminator_operands,
};
use crate::{Diagnostic, Phase, RejectedEntry, TranslateError};

fn undefined(value: &Operand) -> bool {
    match value {
        Operand::Undef(_) => true,
        Operand::Composite { elements, .. } => elements.iter().any(undefined),
        _ => false,
    }
}

fn undefined_globals(module: &Module, function: &Function) -> Vec<usize> {
    let mut missing = BTreeSet::new();
    for block in &function.blocks {
        let operands = block
            .instructions
            .iter()
            .flat_map(|instruction| instruction_operands(&instruction.kind))
            .chain(terminator_operands(&block.terminator));
        for operand in operands {
            let Operand::Global { index, .. } = operand else {
                continue;
            };
            let global = &module.globals[*index as usize];
            if global.space == crate::ir::AddressSpace::Constant
                && global.initializer.as_ref().is_none_or(undefined)
            {
                missing.insert(*index as usize);
            }
        }
    }
    missing.into_iter().collect()
}

fn missing_error(module: &Module, function: &Function, index: usize) -> TranslateError {
    TranslateError::one(Diagnostic::new(
        Phase::Specialization,
        format!(
            "constant {} has no defined value for function {}",
            module.globals[index].name, function.name
        ),
    ))
}

/// Remove entry points which still need required function-constant values.
/// Their functions remain in the template and can be specialized later.
pub(crate) fn function_constant_dependencies(module: &Module) -> BTreeSet<usize> {
    let mut dependent: BTreeSet<_> = module
        .globals
        .iter()
        .enumerate()
        .filter_map(|(index, global)| global.function_constant.as_ref().map(|_| index))
        .collect();
    loop {
        let mut changed = false;
        for initializer in &module.static_initializers {
            let mut targets = Vec::new();
            let mut reads_dependent = false;
            for block in &initializer.blocks {
                for instruction in &block.instructions {
                    if let InstructionKind::Store {
                        pointer: Operand::Global { index, .. },
                        ..
                    } = &instruction.kind
                    {
                        targets.push(*index as usize);
                    }
                    reads_dependent |= instruction_operands(&instruction.kind)
                        .into_iter()
                        .any(|operand| {
                            matches!(operand, Operand::Global { index, .. } if dependent.contains(&(*index as usize)))
                        });
                }
            }
            if reads_dependent {
                for target in targets {
                    changed |= dependent.insert(target);
                }
            }
        }
        if !changed {
            return dependent;
        }
    }
}

pub(crate) fn reject_undefined_entries(
    module: &mut Module,
    dependencies: &BTreeSet<usize>,
) -> Result<Vec<RejectedEntry>, TranslateError> {
    let mut rejected = Vec::new();
    let mut remove = Vec::new();
    for (position, entry) in module.entries.iter().enumerate() {
        let Some(function) = module.functions.get(entry.function) else {
            continue;
        };
        let missing = undefined_globals(module, function);
        if let Some(index) = missing
            .iter()
            .copied()
            .find(|index| !dependencies.contains(index))
        {
            return Err(missing_error(module, function, index));
        }
        if !missing.is_empty() {
            let error = missing_error(module, function, missing[0]);
            rejected.push(RejectedEntry {
                name: function.name.clone(),
                reason: error.to_string(),
            });
            remove.push(position);
        }
    }
    let mut position = 0;
    module.entries.retain(|_| {
        let keep = remove.binary_search(&position).is_err();
        position += 1;
        keep
    });
    Ok(rejected)
}
