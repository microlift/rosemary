use crate::ir::{Module, Operand, instruction_operands, terminator_operands};
use crate::{Diagnostic, Phase, TranslateError};

fn undefined(value: &Operand) -> bool {
    match value {
        Operand::Undef(_) => true,
        Operand::Composite { elements, .. } => elements.iter().any(undefined),
        _ => false,
    }
}

/// A source constant which still has no value must not become uninitialized GPU memory.
pub(crate) fn validate_specialized(module: &Module) -> Result<(), TranslateError> {
    for function in &module.functions {
        for block in &function.blocks {
            let operands = block
                .instructions
                .iter()
                .flat_map(|i| instruction_operands(&i.kind))
                .chain(terminator_operands(&block.terminator));
            for operand in operands {
                let Operand::Global { index, .. } = operand else {
                    continue;
                };
                let global = &module.globals[*index as usize];
                if global.space == crate::ir::AddressSpace::Constant
                    && global.initializer.as_ref().is_none_or(undefined)
                {
                    return Err(TranslateError::one(Diagnostic::new(
                        Phase::Specialization,
                        format!(
                            "constant {} has no defined value for function {}",
                            global.name, function.name
                        ),
                    )));
                }
            }
        }
    }
    Ok(())
}
