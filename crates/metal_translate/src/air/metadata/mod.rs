mod fields;
#[cfg(test)]
mod tests;

use super::values::ValueTable;
use super::*;
use fields::*;

/// One decoded metadata value. Only the shapes AIR emits are represented;
/// debug-information nodes are recorded as `Other` and never interpreted.
#[derive(Clone, Debug)]
pub(super) enum MetadataValue {
    String(String),
    Value(usize),
    Node(Vec<Option<MetadataOperand>>),
    Other,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum MetadataOperand {
    /// Another metadata value, by metadata index.
    Metadata(usize),
    /// A module value, by LLVM value ID. AIR uses these for the entry
    /// function and for the integer literals in interface descriptors.
    Value(usize),
}

pub(super) struct Metadata {
    values: Vec<MetadataValue>,
    named: HashMap<String, Vec<usize>>,
}

impl Metadata {
    /// Decode every module-level metadata block.
    ///
    /// AIR is written by an older LLVM, which numbers metadata values by the
    /// order of the records that create them. `NAME` and `KIND` records do not
    /// create values and must not advance that numbering.
    pub(super) fn decode(
        module_block: &BitcodeBlock,
        types: &[DecodedType],
    ) -> Result<Self, TranslateError> {
        let mut values = Vec::new();
        let mut named = HashMap::new();
        let mut pending_name: Option<String> = None;
        for block in module_block
            .elements
            .iter()
            .filter_map(BitcodeElement::as_block)
            .filter(|block| block.id == 15)
        {
            for record in block.elements.iter().filter_map(BitcodeElement::as_record) {
                match record.id {
                    // METADATA_STRING_OLD
                    1 => {
                        let text = record
                            .clone()
                            .take_payload()
                            .and_then(payload_string)
                            .or_else(|| record_ascii(record))
                            .ok_or_else(|| semantic("AIR metadata string is not UTF-8"))?;
                        values.push(MetadataValue::String(text));
                    }
                    // METADATA_NAME: names the following NAMED_NODE.
                    4 => {
                        pending_name = record
                            .clone()
                            .take_payload()
                            .and_then(payload_string)
                            .or_else(|| record_ascii(record));
                    }
                    // METADATA_NAMED_NODE: its fields are metadata indices.
                    10 => {
                        let Some(name) = pending_name.take() else {
                            return Err(semantic("AIR named metadata has no preceding name"));
                        };
                        let members = record_values(record)
                            .iter()
                            .map(|index| {
                                usize::try_from(*index).map_err(|_| {
                                    semantic("AIR named metadata index does not fit in memory")
                                })
                            })
                            .collect::<Result<_, _>>()?;
                        named.insert(name, members);
                    }
                    // METADATA_KIND declares an attachment kind, not a value.
                    6 => {}
                    // METADATA_VALUE: a module value wrapped as metadata.
                    2 => {
                        let fields = record_values(record);
                        let [_, value] = fields.as_slice() else {
                            return Err(semantic("AIR metadata value has the wrong shape"));
                        };
                        values.push(MetadataValue::Value(usize::try_from(*value).map_err(
                            |_| semantic("AIR metadata value id does not fit in memory"),
                        )?));
                    }
                    // METADATA_NODE and METADATA_DISTINCT_NODE. Their operands
                    // are one-based metadata IDs; zero is a null operand.
                    3 | 5 => {
                        let operands = record_values(record)
                            .iter()
                            .map(|id| {
                                if *id == 0 {
                                    return Ok(None);
                                }
                                usize::try_from(*id - 1)
                                    .map(|index| Some(MetadataOperand::Metadata(index)))
                                    .map_err(|_| {
                                        semantic("AIR metadata operand does not fit in memory")
                                    })
                            })
                            .collect::<Result<_, _>>()?;
                        values.push(MetadataValue::Node(operands));
                    }
                    // METADATA_OLD_NODE: (type, value) operand pairs.
                    8 => {
                        let fields = record_values(record);
                        if !fields.len().is_multiple_of(2) {
                            return Err(semantic("AIR metadata node has an odd operand count"));
                        }
                        let mut operands = Vec::new();
                        for pair in fields.as_chunks::<2>().0 {
                            let (ty, value) = (pair[0], pair[1]);
                            let index = usize::try_from(value).map_err(|_| {
                                semantic("AIR metadata operand does not fit in memory")
                            })?;
                            let ty = usize::try_from(ty).map_err(|_| {
                                semantic("AIR metadata operand type does not fit in memory")
                            })?;
                            operands.push(match types.get(ty) {
                                // A null operand is written as type 0, value 0.
                                None => None,
                                Some(DecodedType::Metadata) => {
                                    Some(MetadataOperand::Metadata(index))
                                }
                                Some(_) => Some(MetadataOperand::Value(index)),
                            });
                        }
                        values.push(MetadataValue::Node(operands));
                    }
                    // METADATA_STRINGS: all strings share one blob. The blob's
                    // prefix is a VBR6 stream of lengths and its suffix is the
                    // concatenated UTF-8 bytes.
                    35 => {
                        let fields = record_values(record);
                        let [count, offset] = fields.as_slice() else {
                            return Err(semantic("AIR metadata strings have the wrong shape"));
                        };
                        let Some(Payload::Blob(blob)) = record.clone().take_payload() else {
                            return Err(semantic("AIR metadata strings have no blob"));
                        };
                        values.extend(
                            metadata_strings(&blob, *count, *offset)?
                                .into_iter()
                                .map(MetadataValue::String),
                        );
                    }
                    _ => values.push(MetadataValue::Other),
                }
            }
        }
        Ok(Self { values, named })
    }

    pub(super) fn named(&self, name: &str) -> Option<&[usize]> {
        self.named.get(name).map(Vec::as_slice)
    }

    pub(super) fn node(&self, index: usize) -> Option<&[Option<MetadataOperand>]> {
        match self.values.get(index)? {
            MetadataValue::Node(operands) => Some(operands),
            _ => None,
        }
    }

    pub(super) fn string(&self, index: usize) -> Option<&str> {
        match self.values.get(index)? {
            MetadataValue::String(text) => Some(text),
            _ => None,
        }
    }
}

fn metadata_strings(blob: &[u8], count: u64, offset: u64) -> Result<Vec<String>, TranslateError> {
    let count = usize::try_from(count)
        .map_err(|_| semantic("AIR metadata string count does not fit in memory"))?;
    let offset = usize::try_from(offset)
        .map_err(|_| semantic("AIR metadata string offset does not fit in memory"))?;
    if count == 0 || offset > blob.len() {
        return Err(semantic("AIR metadata strings have an invalid layout"));
    }
    let lengths = &blob[..offset];
    let mut strings = &blob[offset..];
    let mut bit = 0usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let length = usize::try_from(read_vbr6(lengths, &mut bit)?)
            .map_err(|_| semantic("AIR metadata string length does not fit in memory"))?;
        let Some((text, rest)) = strings.split_at_checked(length) else {
            return Err(semantic("AIR metadata strings are truncated"));
        };
        out.push(
            std::str::from_utf8(text)
                .map_err(|_| semantic("AIR metadata string is not UTF-8"))?
                .to_string(),
        );
        strings = rest;
    }
    Ok(out)
}

/// One LLVM bitstream VBR value with six-bit groups and five payload bits.
fn read_vbr6(bytes: &[u8], bit: &mut usize) -> Result<u64, TranslateError> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let mut group = 0u8;
        for position in 0..6 {
            let Some(byte) = bytes.get(*bit / 8) else {
                return Err(semantic("AIR metadata string lengths are truncated"));
            };
            group |= ((byte >> (*bit % 8)) & 1) << position;
            *bit += 1;
        }
        value |= u64::from(group & 0x1f)
            .checked_shl(shift)
            .ok_or_else(|| semantic("AIR metadata string length overflows"))?;
        if group & 0x20 == 0 {
            return Ok(value);
        }
        shift = shift
            .checked_add(5)
            .filter(|shift| *shift < 64)
            .ok_or_else(|| semantic("AIR metadata string length overflows"))?;
    }
}

fn record_ascii(record: &llvm_bitcode::bitcode::Record) -> Option<String> {
    String::from_utf8(
        record
            .fields()
            .iter()
            .map(|byte| u8::try_from(*byte).ok())
            .collect::<Option<Vec<_>>>()?,
    )
    .ok()
}

/// One operand of an interface descriptor node, resolved to the two shapes AIR
/// uses: a literal string key or an integer constant.
#[derive(Clone, Copy, Debug)]
pub(super) enum Item<'a> {
    Text(&'a str),
    Number(u64),
}

impl Metadata {
    /// Flatten a descriptor node into strings and integers, resolving module
    /// constants through the value table.
    pub(super) fn items<'a>(
        &'a self,
        node: usize,
        values: &ValueTable,
        types: &[DecodedType],
    ) -> Result<Vec<Item<'a>>, TranslateError> {
        let operands = self
            .node(node)
            .ok_or_else(|| semantic("AIR interface descriptor is not a metadata node"))?;
        let mut items = Vec::new();
        for operand in operands {
            match operand {
                // A nested node, such as `air.struct_type_info`, carries
                // reflection this translator does not need.
                Some(MetadataOperand::Metadata(index)) => {
                    if let Some(text) = self.string(*index) {
                        items.push(Item::Text(text));
                    } else if let Some(MetadataValue::Value(id)) = self.values.get(*index) {
                        items.push(Item::Number(values.integer(*id, types)?));
                    }
                }
                Some(MetadataOperand::Value(id)) => {
                    items.push(Item::Number(values.integer(*id, types)?));
                }
                None => {}
            }
        }
        Ok(items)
    }
}

/// A resource binding described by AIR metadata, before its Metal IR type is
/// known. The type comes from the entry function's parameter list.
#[derive(Clone, Debug)]
pub(super) struct MetadataResource {
    pub argument: u32,
    pub kind: ResourceKind,
    pub index: u32,
    pub count: u32,
    pub access: Access,
    pub name: Option<String>,
    /// The Metal type spelled by `air.arg_type_name`, for arguments whose
    /// LLVM type is opaque. A buffer's type comes from LLVM instead.
    pub ty: Option<Type>,
}

/// The complete stage interface of one AIR entry point.
#[derive(Debug)]
pub(super) struct StageInterface {
    /// LLVM value ID of the entry function.
    pub function: usize,
    pub resources: Vec<MetadataResource>,
    pub inputs: Vec<StageInput>,
    pub outputs: Vec<StageOutput>,
}

impl Metadata {
    /// Decode the entry point declared by `air.vertex`, `air.fragment` or
    /// `air.kernel`.
    pub(super) fn stage_interface(
        &self,
        stage: Stage,
        values: &ValueTable,
        types: &[DecodedType],
    ) -> Result<StageInterface, TranslateError> {
        let name = match stage {
            Stage::Vertex => "air.vertex",
            Stage::Fragment => "air.fragment",
            Stage::Kernel => "air.kernel",
            _ => return Err(semantic("AIR entry stage has no metadata name")),
        };
        let [entry] = self
            .named(name)
            .ok_or_else(|| semantic("AIR has no entry-point metadata"))?
        else {
            return Err(semantic("AIR declares more than one entry point"));
        };
        let [function, outputs, inputs] = self
            .node(*entry)
            .ok_or_else(|| semantic("AIR entry metadata is not a node"))?
        else {
            return Err(semantic(
                "AIR entry metadata does not have a function, output and input list",
            ));
        };
        let function = match function {
            Some(MetadataOperand::Value(function)) => *function,
            Some(MetadataOperand::Metadata(index)) => match self.values.get(*index) {
                Some(MetadataValue::Value(function)) => *function,
                _ => return Err(semantic("AIR entry metadata has no function")),
            },
            None => return Err(semantic("AIR entry metadata has no function")),
        };
        let outputs = match outputs {
            Some(MetadataOperand::Metadata(index)) => self.list(*index)?,
            // A kernel has no returned value and no output list.
            None => Vec::new(),
            _ => return Err(semantic("AIR entry output list is not metadata")),
        };
        let inputs = match inputs {
            Some(MetadataOperand::Metadata(index)) => self.list(*index)?,
            None => Vec::new(),
            _ => return Err(semantic("AIR entry input list is not metadata")),
        };
        let mut interface = StageInterface {
            function,
            resources: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        };
        // An argument or output Metal declares only while a function constant
        // holds names that constant here in place of its kind. Which of them
        // exist is a question for specialization, and until it answers one a
        // gated binding would be bound whether it exists or not.
        let gated = |items: &[Item<'_>]| {
            items
                .iter()
                .any(|item| matches!(item, Item::Text("air.function_constant")))
        };
        for (position, node) in outputs.into_iter().enumerate() {
            let items = self.items(node, values, types)?;
            if gated(&items) {
                return Err(semantic(format!(
                    "AIR output {position} exists only under a function constant, which specialization does not yet gate"
                )));
            }
            interface.outputs.push(StageOutput {
                field: Some(position as u32),
                binding: output_binding(&items)?,
                name: argument_name(&items),
            });
        }
        for node in inputs {
            let items = self.items(node, values, types)?;
            let (&Item::Number(argument), rest) = items
                .split_first()
                .ok_or_else(|| semantic("AIR interface descriptor is empty"))?
            else {
                return Err(semantic("AIR interface descriptor has no argument index"));
            };
            let argument = u32::try_from(argument)
                .map_err(|_| semantic("AIR argument index does not fit in 32 bits"))?;
            let Some(Item::Text(tag)) = rest.first() else {
                return Err(semantic("AIR interface descriptor has no kind"));
            };
            if gated(&items) {
                return Err(semantic(format!(
                    "AIR argument {argument} exists only under a function constant, which specialization does not yet gate"
                )));
            }
            match resource_kind(tag) {
                Some(kind) => {
                    let access = access(rest);
                    interface.resources.push(MetadataResource {
                        argument,
                        kind,
                        index: location_index(rest)?,
                        count: location_count(rest)?,
                        access,
                        name: argument_name(&items),
                        ty: match kind {
                            ResourceKind::Texture => Some(texture_type(&items, access)?),
                            ResourceKind::Sampler => Some(Type::Sampler),
                            _ => None,
                        },
                    });
                }
                None => interface.inputs.push(StageInput {
                    argument,
                    binding: input_binding(tag, rest)?,
                    name: argument_name(&items),
                    signed: *tag == "air.vertex_input" && signed_integer_argument(&items),
                }),
            }
        }
        Ok(interface)
    }

    /// Read a metadata node whose operands are all metadata references.
    fn list(&self, index: usize) -> Result<Vec<usize>, TranslateError> {
        self.node(index)
            .ok_or_else(|| semantic("AIR interface list is not a metadata node"))?
            .iter()
            .map(|operand| match operand {
                Some(MetadataOperand::Metadata(index)) => Ok(*index),
                _ => Err(semantic("AIR interface list has a non-metadata member")),
            })
            .collect()
    }
}
