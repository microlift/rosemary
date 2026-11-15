use crate::RejectedEntry;
use crate::ir::{
    AddressSpace, Function, Module, Operand, instruction_operands, terminator_operands,
};

fn undefined(value: &Operand) -> bool {
    match value {
        Operand::Undef(_) => true,
        Operand::Composite { elements, .. } => elements.iter().any(undefined),
        _ => false,
    }
}

/// The first `constant` global `function` reads which has no defined value.
fn undefined_constant<'m>(module: &'m Module, function: &Function) -> Option<&'m str> {
    function.blocks.iter().find_map(|block| {
        block
            .instructions
            .iter()
            .flat_map(|instruction| instruction_operands(&instruction.kind))
            .chain(terminator_operands(&block.terminator))
            .find_map(|operand| {
                let Operand::Global { index, .. } = operand else {
                    return None;
                };
                let global = &module.globals[*index as usize];
                (global.space == AddressSpace::Constant
                    && global.initializer.as_ref().is_none_or(undefined))
                .then_some(global.name.as_str())
            })
    })
}

/// Reject each function which still reads a source constant with no value,
/// rather than emit it reading uninitialized GPU memory. The rest of the
/// library stays usable: Metal only refuses such a function once it is
/// made without the values it needs.
pub(crate) fn reject_undefined_constants(module: &mut Module) -> Vec<RejectedEntry> {
    let mut rejected = Vec::new();
    let mut kept = Vec::with_capacity(module.functions.len());
    for function in &module.functions {
        let constant = undefined_constant(module, function);
        if let Some(constant) = constant {
            rejected.push(RejectedEntry {
                name: function.name.clone(),
                reason: format!("constant {constant} has no value"),
            });
        }
        kept.push(constant.is_none());
    }
    // Entry points name their function by index, which removal shifts.
    let mut renumbered = Vec::with_capacity(kept.len());
    let mut next = 0;
    for &keep in &kept {
        renumbered.push(next);
        next += usize::from(keep);
    }
    module.entries.retain(|entry| kept[entry.function]);
    for entry in &mut module.entries {
        entry.function = renumbered[entry.function];
    }
    let mut kept = kept.into_iter();
    module.functions.retain(|_| kept.next().unwrap_or(true));
    rejected
}
