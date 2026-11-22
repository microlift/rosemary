mod body;
mod convert;
mod intrinsic;
mod switch;
#[cfg(test)]
mod tests;
mod types;

use super::*;

use convert::{PointerAlias, cast_operation, narrow, reinterpretation, widen};
use intrinsic::{air_intrinsic, known_helper, typed_air_intrinsic};
use switch::{PendingSwitch, PhiFixup, block_id, expand_switch, is_terminator};
use types::{
    address_space_of, aggregate_types, boolean_type, emit, float_predicate, integer_predicate,
    ir_type, layout, member_type, zero_composite,
};

/// A `GLOBALVAR` flags bit: the record names the value type, not a pointer.
const GLOBAL_EXPLICIT_TYPE: u64 = 1 << 1;
/// Where an explicitly typed `GLOBALVAR` keeps its address space in its flags.
const GLOBAL_ADDRESS_SPACE_SHIFT: u64 = 2;
/// LLVM's `CE_CAST` opcode for `bitcast`.
const CAST_BITCAST: u64 = 11;

#[derive(Clone, Debug, PartialEq)]
enum Constant {
    /// A pointer `bitcast` of another constant.
    Bitcast(usize),
    Null,
    Undef,
    Integer(u64),
    Float(u64),
    Aggregate(Vec<usize>),
    Data(Vec<u64>),
    /// A pointer to one of the module's global variables.
    Global(u32),
}

#[derive(Clone, Debug)]
struct Value {
    ty: usize,
    constant: Option<Constant>,
}

pub(super) struct ValueTable {
    relative: bool,
    values: Vec<Value>,
    argument_start: usize,
    /// One entry per module global, in declaration order, which is the order
    /// `Constant::Global` indexes and the order the IR module keeps them in.
    globals: Vec<PendingGlobal>,
}

impl ValueTable {
    /// Allocate only the module-level values: functions and module
    /// constants. AIR's interface metadata refers to exactly these, so it can
    /// be decoded before an entry function has been selected.
    pub(super) fn module_values(
        module: &BitcodeBlock,
        types: &[DecodedType],
    ) -> Result<Self, TranslateError> {
        Self::decode_inner(module, None, types, &[])
    }

    pub(super) fn decode(
        module: &BitcodeBlock,
        body: &BitcodeBlock,
        types: &[DecodedType],
        parameters: &[usize],
    ) -> Result<Self, TranslateError> {
        Self::decode_inner(module, Some(body), types, parameters)
    }

    fn decode_inner(
        module: &BitcodeBlock,
        body: Option<&BitcodeBlock>,
        types: &[DecodedType],
        parameters: &[usize],
    ) -> Result<Self, TranslateError> {
        let version = module
            .elements
            .iter()
            .filter_map(BitcodeElement::as_record)
            .find(|r| r.id == 1)
            .map(record_values)
            .ok_or_else(|| semantic("LLVM module has no version"))?;
        let [version @ 0..=2] = version.as_slice() else {
            return Err(semantic("unsupported LLVM module version"));
        };
        let mut table = Self {
            relative: *version >= 1,
            values: Vec::new(),
            argument_start: 0,
            globals: Vec::new(),
        };
        for record in module.elements.iter().filter_map(BitcodeElement::as_record) {
            match record.id {
                8 => {
                    let fields = record_values(record);
                    let ty =
                        type_reference(fields.get(if *version == 2 { 2 } else { 0 }), types.len())?;
                    table.values.push(Value { ty, constant: None });
                }
                // A module global: Metal's constant lookup tables and its
                // `threadgroup` arrays live here.
                7 => {
                    let fields = record_values(record);
                    let index = if *version == 2 { 2 } else { 0 };
                    let declared = type_reference(fields.get(index), types.len())?;
                    let flags = fields.get(index + 1).copied().unwrap_or(0);
                    // With the explicit-type bit the record names the value
                    // type and its own address space; without it the type
                    // field is the pointer.
                    let pointer = match flags & GLOBAL_EXPLICIT_TYPE {
                        0 => declared,
                        _ => {
                            let space = flags >> GLOBAL_ADDRESS_SPACE_SHIFT;
                            types
                                .iter()
                                .position(|ty| {
                                    matches!(ty, DecodedType::Pointer { pointee, address_space }
                                        if *pointee == declared && *address_space == space)
                                })
                                .ok_or_else(|| {
                                    semantic("LLVM global variable's pointer type is not in the type table")
                                })?
                        }
                    };
                    let DecodedType::Pointer {
                        pointee,
                        address_space,
                    } = types[pointer]
                    else {
                        return Err(semantic("LLVM global variable is not a pointer"));
                    };
                    let space = address_space_of(address_space)?;
                    let initializer = match fields.get(index + 2).copied().unwrap_or(0) {
                        0 => None,
                        id => Some(usize::try_from(id - 1).map_err(|_| {
                            semantic("LLVM global initializer does not fit in memory")
                        })?),
                    };
                    table.values.push(Value {
                        ty: pointer,
                        constant: Some(Constant::Global(table.globals.len() as u32)),
                    });
                    table.globals.push(PendingGlobal {
                        space,
                        pointee,
                        initializer,
                        value_id: table.values.len() - 1,
                        // `[type, isconst, initid, linkage, alignment, section, ...]`
                        section: fields.get(index + 5).copied().unwrap_or(0),
                    });
                }
                // These also allocate value IDs. Reject until their pointer
                // types and initializers can be represented, never skip them.
                9 | 14 | 18 => {
                    return Err(semantic(format!(
                        "unsupported LLVM module value record {}",
                        record.id
                    )));
                }
                _ => {}
            }
        }
        for block in module
            .elements
            .iter()
            .filter_map(BitcodeElement::as_block)
            .filter(|b| b.id == 11)
        {
            table.constants(block, types)?;
        }
        table.argument_start = table.values.len();
        for &ty in parameters {
            if ty >= types.len() {
                return Err(semantic("LLVM argument has missing type"));
            }
            table.values.push(Value { ty, constant: None });
        }
        for block in body
            .into_iter()
            .flat_map(|body| body.elements.iter())
            .filter_map(BitcodeElement::as_block)
            .filter(|b| b.id == 11)
        {
            table.constants(block, types)?;
        }
        Ok(table)
    }

    fn constants(
        &mut self,
        block: &BitcodeBlock,
        types: &[DecodedType],
    ) -> Result<(), TranslateError> {
        let start = self.values.len();
        let mut current_type = None;
        for record in block.elements.iter().filter_map(BitcodeElement::as_record) {
            let fields = record_values(record);
            if record.id == 1 {
                if fields.len() != 1 {
                    return Err(semantic("malformed LLVM constant SETTYPE"));
                }
                current_type = Some(type_reference(fields.first(), types.len())?);
                continue;
            }
            let ty = current_type.ok_or_else(|| semantic("LLVM constant precedes SETTYPE"))?;
            let constant = decode_constant(record.id, &fields, &types[ty])?;
            self.values.push(Value {
                ty,
                constant: Some(constant),
            });
        }
        // Constants can refer to later constants in the same block. Validate
        // after allocating every ID, without recursively expanding the graph.
        for value in &self.values[start..] {
            if let Some(Constant::Aggregate(ids)) = &value.constant {
                let expected = aggregate_types(&types[value.ty])?;
                if ids.len() != expected.len() {
                    return Err(semantic(
                        "LLVM aggregate constant has incorrect element count",
                    ));
                }
                for (&id, ty) in ids.iter().zip(expected) {
                    let member = self.values.get(id).ok_or_else(|| {
                        semantic("LLVM aggregate constant refers to a missing value")
                    })?;
                    if member.ty != ty || member.constant.is_none() {
                        return Err(semantic(
                            "LLVM aggregate constant has an invalid element type or nonconstant value",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn operand_id(&self, encoded: u64) -> Result<usize, TranslateError> {
        let id = self.reference(encoded)?;
        if id >= self.values.len() {
            return Err(semantic(format!(
                "LLVM value reference resolves to undefined value {id} at {}",
                self.values.len()
            )));
        }
        Ok(id)
    }

    /// Resolve a value reference without requiring it to be defined yet.
    ///
    /// LLVM lays a function's blocks out in an order which need not follow
    /// dominance, so an operand can name an instruction which appears later
    /// in the record stream. Those references carry their type inline.
    fn reference(&self, encoded: u64) -> Result<usize, TranslateError> {
        let encoded =
            u32::try_from(encoded).map_err(|_| semantic("LLVM value reference exceeds 32 bits"))?;
        let next = u32::try_from(self.values.len())
            .map_err(|_| semantic("LLVM value table exceeds 32 bits"))?;
        Ok(if self.relative {
            next.wrapping_sub(encoded)
        } else {
            encoded
        } as usize)
    }

    /// The constant initializing the global module value `value` names.
    pub(super) fn global_initializer(&self, value: usize) -> Option<usize> {
        self.globals
            .iter()
            .find(|global| global.value_id == value)?
            .initializer
    }

    pub(super) fn integer(&self, id: usize, types: &[DecodedType]) -> Result<u64, TranslateError> {
        if !self
            .values
            .get(id)
            .is_some_and(|value| matches!(types[value.ty], DecodedType::Integer(_)))
        {
            return Err(semantic("LLVM index has a noninteger type"));
        }
        match self.values.get(id).and_then(|v| v.constant.as_ref()) {
            Some(Constant::Integer(value)) => Ok(*value),
            Some(Constant::Null) => Ok(0),
            _ => Err(semantic("LLVM index requires a constant integer")),
        }
    }

    fn shuffle_mask(&self, id: usize, types: &[DecodedType]) -> Result<Vec<u32>, TranslateError> {
        let value = &self.values[id];
        let DecodedType::Vector { element, lanes } = types[value.ty] else {
            return Err(semantic("LLVM shuffle mask is not a vector"));
        };
        if !matches!(types[element], DecodedType::Integer(32)) {
            return Err(semantic("LLVM shuffle mask is not i32"));
        }
        match value.constant.as_ref() {
            Some(Constant::Null) => Ok(vec![0; lanes as usize]),
            Some(Constant::Undef) => Ok(vec![u32::MAX; lanes as usize]),
            Some(Constant::Data(data)) => data
                .iter()
                .map(|&value| {
                    u32::try_from(value).map_err(|_| semantic("LLVM shuffle mask exceeds i32"))
                })
                .collect(),
            Some(Constant::Aggregate(ids)) => ids
                .iter()
                .map(|&id| {
                    if self.values[id].constant == Some(Constant::Undef) {
                        Ok(u32::MAX)
                    } else {
                        u32::try_from(self.integer(id, types)?)
                            .map_err(|_| semantic("LLVM shuffle lane exceeds i32"))
                    }
                })
                .collect(),
            _ => Err(semantic("LLVM shuffle mask is not constant")),
        }
    }

    /// The module's global variables, with their initializers materialized.
    ///
    /// A global's initializer is a constant which the module's constant block
    /// defines after the record which names it, so this runs once the whole
    /// value table exists.
    ///
    /// A global in the `air.fc_initializer` section is a function constant,
    /// and its name, `<name>.MTL_FC_INIT_<index>_<type>`, is the only place
    /// AIR records which one.
    pub(super) fn global_variables(
        &self,
        types: &[DecodedType],
        names: &HashMap<usize, String>,
        sections: &[String],
    ) -> Result<Vec<crate::ir::GlobalVariable>, TranslateError> {
        let mut operands = HashMap::new();
        self.globals
            .iter()
            .map(|global| {
                let initializer = global
                    .initializer
                    .map(|id| self.materialize(id, types, &mut operands, &mut Vec::new()))
                    .transpose()?;
                let ty = ir_type(global.pointee, types)?;
                let name = names.get(&global.value_id).cloned().unwrap_or_default();
                let function_constant = match super::section_name(global.section, sections) {
                    Some("air.fc_initializer") => {
                        let slot = crate::specialize::parse_slot_name(&name).ok_or_else(|| {
                            semantic(format!(
                                "AIR function constant global `{name}` has no MTL_FC_INIT name"
                            ))
                        })?;
                        if global.space != AddressSpace::Constant
                            || !crate::specialize::encoding_matches(&ty, &slot.encoding)
                        {
                            return Err(semantic(format!(
                                "AIR function constant {} is not a `constant` of type `{}`",
                                slot.name, slot.encoding
                            )));
                        }
                        Some(slot)
                    }
                    _ => None,
                };
                Ok(crate::ir::GlobalVariable {
                    name,
                    space: global.space,
                    ty,
                    initializer,
                    function_constant,
                })
            })
            .collect()
    }

    /// Resolve an LLVM value reference to a Metal IR operand, expanding a
    /// constant into a value rather than into instructions.
    fn materialize(
        &self,
        id: usize,
        types: &[DecodedType],
        operands: &mut HashMap<usize, Operand>,
        active: &mut Vec<usize>,
    ) -> Result<Operand, TranslateError> {
        if let Some(value) = operands.get(&id) {
            return Ok(value.clone());
        }
        if active.contains(&id) {
            return Err(semantic("cyclic LLVM aggregate constant"));
        }
        let value = self
            .values
            .get(id)
            .ok_or_else(|| semantic("missing LLVM constant"))?;
        let constant = value
            .constant
            .as_ref()
            .ok_or_else(|| semantic("LLVM value has no lowered operand"))?;
        // A cast to the opaque sampler pointer AIR passes samplers as is only
        // made of an inline sampler's state, which the entry binds first.
        if let Constant::Bitcast(source) = constant {
            return operands
                .get(source)
                .cloned()
                .ok_or_else(|| semantic("LLVM constant pointer bitcast needs lowering"));
        }
        let ty = ir_type(value.ty, types)?;
        active.push(id);
        let result = match constant {
            Constant::Integer(value) if ty == Type::Bool => Operand::Bool(*value != 0),
            Constant::Integer(value) => Operand::Integer { ty, value: *value },
            Constant::Float(bits) => Operand::Float { ty, bits: *bits },
            Constant::Undef => Operand::Undef(ty),
            Constant::Null if ty == Type::Bool => Operand::Bool(false),
            Constant::Null if matches!(ty, Type::Int { .. }) => Operand::Integer { ty, value: 0 },
            Constant::Null if matches!(ty, Type::Float { .. }) => Operand::Float { ty, bits: 0 },
            Constant::Aggregate(ids) => {
                let mut elements = Vec::new();
                for &id in ids {
                    elements.push(self.materialize(id, types, operands, active)?);
                }
                Operand::Composite { ty, elements }
            }
            Constant::Data(data) => {
                let member_types = aggregate_types(&types[value.ty])?;
                let mut elements = Vec::new();
                for (&bits, member) in data.iter().zip(member_types) {
                    let member = ir_type(member, types)?;
                    elements.push(match member {
                        Type::Int { .. } => Operand::Integer {
                            ty: member,
                            value: bits,
                        },
                        Type::Float { .. } => Operand::Float { ty: member, bits },
                        _ => return Err(semantic("LLVM DATA requires numeric scalar elements")),
                    });
                }
                Operand::Composite { ty, elements }
            }
            Constant::Null => zero_composite(&ty)?,
            Constant::Global(index) => Operand::Global { index: *index, ty },
            Constant::Bitcast(_) => unreachable!("answered before its type is needed"),
        };
        active.pop();
        operands.insert(id, result.clone());
        Ok(result)
    }
}

/// What a lowered function body needs beyond its own records: its LLVM
/// signature, the names its calls resolve to, and the Metal types of any
/// opaque arguments.
pub(super) struct Signature<'a> {
    pub names: &'a HashMap<usize, String>,
    pub name: String,
    pub parameter_types: &'a [usize],
    pub result_type: usize,
    pub opaque_types: &'a HashMap<u32, Type>,
    /// The module's own functions, which a call is inlined from. Without
    /// them, a call to one is rejected.
    pub callees: Option<&'a super::bitcode::Callees<'a>>,
    /// The module values of inline sampler states, each passed as a sampler
    /// parameter after the LLVM ones.
    pub inline_samplers: &'a [usize],
}

/// Record the Metal IR operand an LLVM value lowered to.
///
/// A value used before the record stream defined it already has a reserved
/// Metal IR value, because the use needed a type. Point the defining
/// instruction at that reserved value so both refer to one SSA definition.
fn define(
    forward: &mut HashMap<usize, (ValueId, usize)>,
    id: usize,
    value: Operand,
    operands: &mut HashMap<usize, Operand>,
    function: &Function,
    instructions: &mut [Instruction],
) -> Result<(), TranslateError> {
    let Some((reserved, _)) = forward.remove(&id) else {
        operands.insert(id, value);
        return Ok(());
    };
    let Operand::Value(defined) = value else {
        return Err(semantic(
            "LLVM forward reference names a value with no instruction",
        ));
    };
    if function.values.get(reserved.0 as usize) != function.values.get(defined.0 as usize) {
        return Err(semantic(
            "LLVM forward reference declares the wrong result type",
        ));
    }
    let instruction = instructions
        .iter_mut()
        .rev()
        .find(|instruction| instruction.result == Some(defined))
        .ok_or_else(|| semantic("LLVM forward reference has no defining instruction"))?;
    instruction.result = Some(reserved);
    operands.insert(id, Operand::Value(reserved));
    Ok(())
}

/// A decoded `GLOBALVAR` record, before its initializer has been materialized.
struct PendingGlobal {
    space: AddressSpace,
    /// The type index of what the variable holds, not of the pointer to it.
    pointee: usize,
    /// The value id of the initializer, if the record named one.
    initializer: Option<usize>,
    /// The variable's own module value id, which the value-symbol table
    /// names it by.
    value_id: usize,
    /// The one-based index of its section name, or zero for none.
    section: u64,
}

fn decode_constant(
    code: u64,
    fields: &[u64],
    ty: &DecodedType,
) -> Result<Constant, TranslateError> {
    match (code, fields, ty) {
        // CE_CAST: opcode, source type, source.
        (11, [CAST_BITCAST, _, source], DecodedType::Pointer { .. }) => Ok(Constant::Bitcast(
            usize::try_from(*source).map_err(|_| semantic("LLVM constant reference overflows"))?,
        )),
        (2 | 3, [], DecodedType::Void | DecodedType::Function { .. } | DecodedType::Metadata) => {
            Err(semantic("LLVM constant has a nonvalue type"))
        }
        (2, [], _) => Ok(Constant::Null),
        (3, [], _) => Ok(Constant::Undef),
        (4, [encoded], DecodedType::Integer(bits @ 1..=64)) => {
            let value = decode_sign_rotated(*encoded) as u64;
            Ok(Constant::Integer(if *bits == 64 {
                value
            } else {
                value & ((1u64 << bits) - 1)
            }))
        }
        (6, [bits], DecodedType::Float(width @ (16 | 32 | 64)))
            if *width == 64 || *bits < (1u64 << width) =>
        {
            Ok(Constant::Float(*bits))
        }
        (7, ids, _) => {
            aggregate_types(ty)?;
            Ok(Constant::Aggregate(
                ids.iter()
                    .map(|id| {
                        usize::try_from(*id)
                            .map_err(|_| semantic("LLVM constant reference overflows"))
                    })
                    .collect::<Result<_, _>>()?,
            ))
        }
        (22, data, DecodedType::Vector { lanes: count, .. } | DecodedType::Array { count, .. })
            if data.len() == *count as usize =>
        {
            Ok(Constant::Data(data.to_vec()))
        }
        _ => Err(semantic(format!(
            "unsupported or malformed LLVM constant record {code} for {}",
            ty.description()
        ))),
    }
}
