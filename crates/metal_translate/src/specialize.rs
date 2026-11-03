use std::collections::{BTreeMap, HashMap};

use crate::ir::{
    AddressSpace, Function, GlobalVariable, InstructionKind, Module, Operand, Terminator, Type,
    ValueId, instruction_name, instruction_operands, instruction_operands_mut, operand_type,
};
use crate::passes::{Substitution, fold_instruction};
use crate::sha256::Sha256;
use crate::{Diagnostic, Phase, TranslateError};

/// One value as `MTLFunctionConstantValues` holds it: an `MTLDataType` and
/// the bytes of a value of that type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConstantValue {
    pub data_type: u64,
    pub bytes: Vec<u8>,
}

/// The values of `MTLFunctionConstantValues`, by index and by name.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FunctionConstantValues {
    by_index: BTreeMap<u32, ConstantValue>,
    by_name: BTreeMap<String, ConstantValue>,
}

impl FunctionConstantValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.by_index.is_empty() && self.by_name.is_empty()
    }

    /// `-setConstantValue:type:atIndex:`. Only as many bytes as the type
    /// occupies are read; fewer is an error.
    pub fn set_index(&mut self, index: u32, data_type: u64, bytes: &[u8]) -> Result<(), String> {
        self.by_index.insert(index, value(data_type, bytes)?);
        Ok(())
    }

    /// `-setConstantValue:type:withName:`.
    pub fn set_name(&mut self, name: &str, data_type: u64, bytes: &[u8]) -> Result<(), String> {
        self.by_name
            .insert(name.to_owned(), value(data_type, bytes)?);
        Ok(())
    }

    /// A digest of every value, for the shader cache key: a library
    /// specialized with one set of values is not the library another emits.
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update_field(b"rosemary metal_translate function constants v1");
        for (index, value) in &self.by_index {
            hasher.update(&[0]);
            hasher.update(&index.to_le_bytes());
            hasher.update(&value.data_type.to_le_bytes());
            hasher.update_field(&value.bytes);
        }
        for (name, value) in &self.by_name {
            hasher.update(&[1]);
            hasher.update_field(name.as_bytes());
            hasher.update(&value.data_type.to_le_bytes());
            hasher.update_field(&value.bytes);
        }
        hasher.finish()
    }

    fn get(&self, slot_index: u32, name: &str) -> Option<&ConstantValue> {
        self.by_index
            .get(&slot_index)
            .or_else(|| self.by_name.get(name))
    }
}

fn value(data_type: u64, bytes: &[u8]) -> Result<ConstantValue, String> {
    let size = metal_data_type_size(data_type)
        .ok_or_else(|| format!("MTLDataType {data_type} cannot be a function constant"))?;
    let bytes = bytes.get(..size).ok_or_else(|| {
        format!(
            "MTLDataType {data_type} needs {size} bytes and {} were given",
            bytes.len()
        )
    })?;
    Ok(ConstantValue {
        data_type,
        bytes: bytes.to_vec(),
    })
}

/// The `MTLDataType`s a function constant can have: each scalar's first
/// value, its Itanium spelling and its size. The two-, three- and four-lane
/// vectors follow each scalar in the enumeration.
const SCALARS: [(u64, &str, usize); 11] = [
    (3, "f", 4),   // float
    (16, "Dh", 2), // half
    (29, "i", 4),  // int
    (33, "j", 4),  // uint
    (37, "s", 2),  // short
    (41, "t", 2),  // ushort
    (45, "c", 1),  // char
    (49, "h", 1),  // uchar
    (53, "b", 1),  // bool
    (81, "l", 8),  // long
    (85, "m", 8),  // ulong
];

fn layout(data_type: u64) -> Option<(&'static str, usize, usize)> {
    SCALARS.iter().find_map(|&(first, encoding, size)| {
        (first..first + 4)
            .contains(&data_type)
            .then(|| (encoding, size, (data_type - first + 1) as usize))
    })
}

/// How many bytes a value of an `MTLDataType` occupies, for a runtime which
/// has only a pointer to one.
pub fn metal_data_type_size(data_type: u64) -> Option<usize> {
    let (_, size, lanes) = layout(data_type)?;
    Some(size * lanes)
}

/// The `MTLDataType` of a constant AIR spells this way, as reflection
/// reports it.
pub fn metal_data_type_of_encoding(encoding: &str) -> Option<u64> {
    let (scalar, lanes) = match encoding.strip_prefix("Dv") {
        Some(vector) => {
            let (lanes, scalar) = vector.split_once('_')?;
            (scalar, lanes.parse::<u64>().ok()?)
        }
        None => (encoding, 1),
    };
    let &(first, _, _) = SCALARS
        .iter()
        .find(|&&(_, spelling, _)| spelling == scalar)?;
    (1..=4).contains(&lanes).then(|| first + lanes - 1)
}

/// The Itanium spelling AIR gives a constant of an `MTLDataType`.
fn metal_data_type_encoding(data_type: u64) -> Option<String> {
    let (encoding, _, lanes) = layout(data_type)?;
    Some(if lanes == 1 {
        encoding.to_owned()
    } else {
        format!("Dv{lanes}_{encoding}")
    })
}

fn error(message: impl Into<String>) -> TranslateError {
    TranslateError::one(Diagnostic::new(Phase::Specialization, message))
}

/// Give a module's function constants their values and run its static
/// initializers.
///
/// Every translation specializes, with no values where the guest gave none,
/// because a module with static initializers or constant queries cannot be
/// emitted otherwise.
pub fn specialize(
    module: &mut Module,
    values: &FunctionConstantValues,
) -> Result<(), TranslateError> {
    let mut defined = vec![false; module.globals.len()];
    for (index, global) in module.globals.iter_mut().enumerate() {
        let Some(slot) = &global.function_constant else {
            continue;
        };
        let Some(value) = values.get(slot.index, &slot.name) else {
            continue;
        };
        let given = metal_data_type_encoding(value.data_type).unwrap_or_default();
        if given != slot.encoding {
            return Err(error(format!(
                "function constant {} (index {}) is declared `{}` and was given MTLDataType {}",
                slot.name, slot.index, slot.encoding, value.data_type
            )));
        }
        // A `bool` is a byte, and Metal reads any nonzero byte as true; the
        // code which reads it truncates to the low bit.
        let boolean = slot.encoding.ends_with('b');
        global.initializer = Some(decode(&global.ty, &value.bytes, boolean).ok_or_else(|| {
            error(format!(
                "function constant {} has a type its value cannot be read as",
                slot.name
            ))
        })?);
        defined[index] = true;
    }

    let globals = &module.globals;
    for function in module
        .functions
        .iter_mut()
        .chain(module.static_initializers.iter_mut())
    {
        answer_queries(function, &defined, globals)?;
    }
    for initializer in std::mem::take(&mut module.static_initializers) {
        run_initializer(&initializer, &mut module.globals)?;
    }
    Ok(())
}

/// Read a value of `ty` from the little-endian bytes Metal was handed.
fn decode(ty: &Type, bytes: &[u8], boolean: bool) -> Option<Operand> {
    let raw = |size: usize| -> Option<u64> {
        let mut raw = [0u8; 8];
        raw.get_mut(..size)?.copy_from_slice(bytes.get(..size)?);
        Some(u64::from_le_bytes(raw))
    };
    match ty {
        Type::Int { bits, .. } => {
            let value = raw(usize::from(*bits).div_ceil(8))?;
            Some(Operand::Integer {
                ty: ty.clone(),
                value: if boolean {
                    u64::from(value != 0)
                } else {
                    value
                },
            })
        }
        Type::Float { bits } => Some(Operand::Float {
            ty: ty.clone(),
            bits: raw(usize::from(*bits) / 8)?,
        }),
        Type::Bool => Some(Operand::Bool(*bytes.first()? != 0)),
        Type::Vector { element, lanes } => {
            let size = match element.as_ref() {
                Type::Int { bits, .. } | Type::Float { bits } => usize::from(*bits).div_ceil(8),
                Type::Bool => 1,
                _ => return None,
            };
            let elements = (0..usize::from(*lanes))
                .map(|lane| decode(element, bytes.get(lane * size..)?, boolean))
                .collect::<Option<Vec<_>>>()?;
            Some(Operand::Composite {
                ty: ty.clone(),
                elements,
            })
        }
        _ => None,
    }
}

/// Replace every `is_function_constant_defined` with its answer.
fn answer_queries(
    function: &mut Function,
    defined: &[bool],
    globals: &[GlobalVariable],
) -> Result<(), TranslateError> {
    let mut substitution = Substitution::default();
    let mut failed = false;
    for block in &mut function.blocks {
        block.instructions.retain(|instruction| {
            let InstructionKind::FunctionConstantDefined { constant } = &instruction.kind else {
                return true;
            };
            match (instruction.result, constant) {
                (Some(result), Operand::Global { index, .. })
                    if globals
                        .get(*index as usize)
                        .is_some_and(|global| global.function_constant.is_some()) =>
                {
                    substitution.insert(result, Operand::Bool(defined[*index as usize]));
                    false
                }
                _ => {
                    failed = true;
                    true
                }
            }
        });
    }
    if failed {
        return Err(error(format!(
            "{} asks whether something other than a function constant is defined",
            function.name
        )));
    }
    substitution.apply(function);
    Ok(())
}

/// Run a static initializer once, writing what it computes into the
/// initializers of the globals it stores to.
///
/// It is executed rather than translated: what it computes is a constant
/// once the function constants are known, and a Metal pipeline has no stage
/// to run it in. Anything it does besides computing constants and storing
/// them into `constant` globals is rejected rather than guessed at. A value
/// computed from an undefined function constant is itself undefined.
fn run_initializer(
    function: &Function,
    globals: &mut [GlobalVariable],
) -> Result<(), TranslateError> {
    let fail = |what: String| error(format!("static initializer {} {what}", function.name));
    let blocks: HashMap<_, _> = function
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect();
    let Some(mut current) = function.blocks.first().map(|block| block.id) else {
        return Ok(());
    };
    let mut previous = None;
    let mut known: HashMap<ValueId, Operand> = HashMap::new();
    let resolve = |known: &HashMap<ValueId, Operand>, operand: &Operand| match operand {
        Operand::Value(value) => known.get(value).cloned().unwrap_or_else(|| operand.clone()),
        other => other.clone(),
    };

    // Enough for any straight-line initializer many times over; a loop which
    // runs longer is not one this evaluator should be running.
    for _ in 0..(1 << 16) {
        let block = blocks
            .get(&current)
            .ok_or_else(|| fail("branches to a missing block".to_owned()))?;
        for instruction in &block.instructions {
            let mut kind = instruction.kind.clone();
            if let InstructionKind::Phi { incomings } = &kind {
                let from =
                    previous.ok_or_else(|| fail("has a phi in its entry block".to_owned()))?;
                let (_, value) = incomings
                    .iter()
                    .find(|(block, _)| *block == from)
                    .ok_or_else(|| fail("has a phi without an input for its edge".to_owned()))?;
                if let Some(result) = instruction.result {
                    known.insert(result, resolve(&known, value));
                }
                continue;
            }
            for operand in instruction_operands_mut(&mut kind) {
                *operand = resolve(&known, operand);
            }
            match &kind {
                InstructionKind::Load {
                    pointer: Operand::Global { index, .. },
                } => {
                    let global = &globals[*index as usize];
                    let value = global
                        .initializer
                        .clone()
                        .unwrap_or_else(|| Operand::Undef(global.ty.clone()));
                    let result = instruction
                        .result
                        .ok_or_else(|| fail("loads into nothing".to_owned()))?;
                    if function.values.get(result.0 as usize) != Some(&global.ty) {
                        return Err(fail("reads part of a global".to_owned()));
                    }
                    known.insert(result, value);
                }
                InstructionKind::Store {
                    pointer: Operand::Global { index, .. },
                    value,
                } => {
                    let global = &mut globals[*index as usize];
                    if global.space != AddressSpace::Constant || global.function_constant.is_some()
                    {
                        return Err(fail(
                            "writes a global other than a derived constant".to_owned(),
                        ));
                    }
                    if matches!(value, Operand::Value(_))
                        || operand_type(function, value).as_ref() != Some(&global.ty)
                    {
                        return Err(fail("stores a value it could not compute".to_owned()));
                    }
                    global.initializer = Some(value.clone());
                }
                _ => {
                    let result = instruction.result.ok_or_else(|| {
                        fail(format!("has a {} it cannot run", instruction_name(&kind)))
                    })?;
                    let ty = &function.values[result.0 as usize];
                    let value = match fold_instruction(&kind, ty, globals) {
                        Some(value) if !matches!(value, Operand::Value(_)) => value,
                        _ if instruction_operands(&kind)
                            .iter()
                            .any(|operand| matches!(operand, Operand::Undef(_))) =>
                        {
                            Operand::Undef(ty.clone())
                        }
                        _ => {
                            return Err(fail(format!(
                                "computes a {} it cannot evaluate",
                                instruction_name(&kind)
                            )));
                        }
                    };
                    known.insert(result, value);
                }
            }
        }
        previous = Some(current);
        current = match &block.terminator {
            Terminator::Return(_) => return Ok(()),
            Terminator::Branch(target) => *target,
            Terminator::CondBranch {
                condition,
                true_block,
                false_block,
            } => match resolve(&known, condition) {
                Operand::Bool(true) => *true_block,
                Operand::Bool(false) => *false_block,
                _ => return Err(fail("branches on a value it cannot compute".to_owned())),
            },
            Terminator::Unreachable => return Err(fail("reaches `unreachable`".to_owned())),
        };
    }
    Err(fail("does not finish".to_owned()))
}

/// Parse AIR's name for a function constant's global,
/// `<name>.MTL_FC_INIT_<index>_<encoding>`.
pub(crate) fn parse_slot_name(name: &str) -> Option<crate::ir::FunctionConstantSlot> {
    let (base, rest) = name.split_once(".MTL_FC_INIT_")?;
    let (index, encoding) = rest.split_once('_')?;
    Some(crate::ir::FunctionConstantSlot {
        index: index.parse().ok()?,
        name: base.to_owned(),
        encoding: encoding.to_owned(),
    })
}

/// Whether a global's Metal IR type is what its Itanium spelling says.
pub(crate) fn encoding_matches(ty: &Type, encoding: &str) -> bool {
    if let Some(vector) = encoding.strip_prefix("Dv") {
        let Some((lanes, scalar)) = vector.split_once('_') else {
            return false;
        };
        return matches!(ty, Type::Vector { element, lanes: count }
            if lanes.parse() == Ok(*count) && encoding_matches(element, scalar));
    }
    matches!(
        (encoding, ty),
        ("b" | "c" | "a" | "h", Type::Int { bits: 8, .. })
            | ("s" | "t", Type::Int { bits: 16, .. })
            | ("i" | "j", Type::Int { bits: 32, .. })
            | ("l" | "m" | "x" | "y", Type::Int { bits: 64, .. })
            | ("Dh", Type::Float { bits: 16 })
            | ("f", Type::Float { bits: 32 })
    )
}

#[cfg(test)]
mod tests;
