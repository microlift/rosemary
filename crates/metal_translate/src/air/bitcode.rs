use super::*;

/// Import the first production-oriented bitcode shape without depending on
/// libLLVM. More function records are added alongside the corresponding Metal
/// operation; accepting an unfamiliar record here would silently miscompile it.
pub(super) fn parse_minimal_store_bitcode(bitcode: &Bitcode) -> Result<Module, TranslateError> {
    let module_block = child_block(&bitcode.elements, 8)
        .ok_or_else(|| semantic("LLVM bitcode has no module block"))?;
    let function_blocks: Vec<_> = module_block
        .elements
        .iter()
        .filter_map(BitcodeElement::as_block)
        .filter(|block| block.id == 12)
        .collect();
    let all_function_records: Vec<_> = module_block
        .elements
        .iter()
        .filter_map(BitcodeElement::as_record)
        .filter(|record| record.id == 8)
        .collect();
    // LLVM emits STRINGTABLE_BLOCK inside the module block.  Older producer
    // versions put it at the top level, so search the complete bitstream
    // rather than tying name resolution to one container layout.
    let string_table = descendant_block(&bitcode.elements, 23)
        .and_then(|block| first_payload(block, 1))
        .and_then(payload_bytes);
    let legacy_names = legacy_function_names(module_block);
    let function_records: Vec<_> = all_function_records
        .iter()
        .enumerate()
        // LLVM's older AIR writer puts `isproto` in field 2, while the
        // string-table layout used by the initial fixture puts it in field 4.
        .filter(|(_, record)| {
            if string_table.is_some() {
                record.fields().get(4) == Some(&0)
            } else {
                record.fields().get(2) == Some(&0)
            }
        })
        .collect();
    if function_blocks.len() != function_records.len() || function_records.is_empty() {
        return Err(semantic(
            "LLVM defined-function records do not match their function bodies",
        ));
    }
    // The libraries in some applications are written by the older AIR producer.
    // Decode the tables before looking at a body: unlike the tiny LLVM 19
    // fixture below, its function type and entry-point contract are not
    // recoverable from a few strings.
    if string_table.is_none()
        && child_block(&module_block.elements, 17).is_some()
        && metadata_entry_stage(module_block).is_some()
    {
        return parse_packed_air(
            module_block,
            &function_records,
            &function_blocks,
            &legacy_names,
        );
    }
    let entry_index = function_records
        .iter()
        .position(|(ordinal, record)| {
            let value_id = function_value_id(module_block, *ordinal).unwrap_or(*ordinal);
            resolve_function_name(record, value_id, string_table.as_deref(), &legacy_names)
                .as_deref()
                == Some("main0")
        })
        .or_else(|| (function_records.len() == 1).then_some(0))
        .ok_or_else(|| semantic("AIR bitcode has no main0 entry function"))?;
    let name = resolve_function_name(
        function_records[entry_index].1,
        function_value_id(module_block, function_records[entry_index].0)
            .unwrap_or(function_records[entry_index].0),
        string_table.as_deref(),
        &legacy_names,
    )
    .ok_or_else(|| semantic("LLVM entry function has no resolvable name"))?;

    let function_block = function_blocks[entry_index];
    let records: Vec<_> = function_block
        .elements
        .iter()
        .filter_map(BitcodeElement::as_record)
        .collect();
    let declared_blocks = records
        .iter()
        .find(|record| record.id == 1)
        .and_then(|record| record.fields().first())
        .copied();
    let stores = records.iter().filter(|record| record.id == 44).count();
    let returns = records.iter().filter(|record| record.id == 10).count();
    if declared_blocks != Some(1) || stores != 1 || returns != 1 || records.len() != 3 {
        return Err(semantic(
            "unsupported LLVM function records in the initial bitcode importer",
        ));
    }
    let constants = child_block(&function_block.elements, 11)
        .ok_or_else(|| semantic("constant-store function has no constants block"))?;
    let encoded = constants
        .elements
        .iter()
        .filter_map(BitcodeElement::as_record)
        .find(|record| record.id == 4)
        .and_then(|record| record.fields().first())
        .copied()
        .ok_or_else(|| semantic("constant-store function has no integer constant"))?;
    let value = decode_sign_rotated(encoded) as u64;
    let symbol_table = child_block(&function_block.elements, 14)
        .ok_or_else(|| semantic("AIR function has no value symbol table"))?;
    let parameter_name = first_payload(symbol_table, 1)
        .and_then(payload_string)
        .ok_or_else(|| semantic("AIR buffer argument has no name"))?;

    let payload_text = collect_payload_text(&bitcode.elements);
    for marker in [
        "air.kernel",
        "air.buffer",
        "air.location_index",
        "air.arg_type_name",
    ] {
        if !payload_text.contains(marker) {
            return Err(semantic(format!(
                "AIR bitcode metadata is missing {marker}"
            )));
        }
    }
    let access = if payload_text.contains("air.read_write") {
        Access::ReadWrite
    } else if payload_text.contains("air.write") {
        Access::Write
    } else if payload_text.contains("air.read") {
        Access::Read
    } else {
        return Err(semantic("AIR buffer metadata has no access mode"));
    };
    let element = if payload_text.contains("uint") {
        Type::u32()
    } else {
        Type::i32()
    };
    let pointer = Type::Pointer {
        space: AddressSpace::Device,
        pointee: Box::new(element.clone()),
    };
    let parameter_value = ValueId(0);
    let parameter = Parameter {
        value: parameter_value,
        name: parameter_name.clone(),
        ty: pointer.clone(),
    };
    let function = Function {
        name,
        result: Type::Void,
        parameters: vec![parameter],
        values: vec![pointer.clone()],
        blocks: vec![Block {
            id: BlockId(0),
            name: "entry".to_string(),
            instructions: vec![Instruction {
                result: None,
                kind: InstructionKind::Store {
                    pointer: Operand::Value(parameter_value),
                    value: Operand::Integer { ty: element, value },
                },
            }],
            terminator: Terminator::Return(None),
        }],
    };
    Ok(Module {
        static_initializers: Vec::new(),
        functions: vec![function],
        globals: Vec::new(),
        entries: vec![EntryPoint {
            early_fragment_tests: false,
            function: 0,
            stage: Stage::Kernel,
            resources: vec![Resource {
                argument: 0,
                kind: ResourceKind::Buffer,
                index: 0,
                count: 1,
                access,
                ty: pointer,
                name: Some(parameter_name),
                inline_sampler: None,
            }],
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
    })
}

/// Decode the table-oriented AIR layout used by Metal's packed libraries.
///
/// This deliberately performs the type and metadata passes independently of
/// instruction lowering.  A body is only accepted once every record has a
/// Metal IR lowering; retaining the decoded contract here prevents the old
/// string-search path from accidentally treating a fragment entry as a
/// compute kernel.
fn parse_packed_air(
    module_block: &BitcodeBlock,
    function_records: &[(usize, &&llvm_bitcode::bitcode::Record)],
    function_blocks: &[&BitcodeBlock],
    legacy_names: &HashMap<usize, String>,
) -> Result<Module, TranslateError> {
    let types = decode_type_table(
        child_block(&module_block.elements, 17)
            .ok_or_else(|| semantic("packed AIR has no type table"))?,
    )?;
    let metadata = metadata::Metadata::decode(module_block, &types)?;
    let stage = metadata_entry_stage(module_block)
        .ok_or_else(|| semantic("packed AIR has no supported entry metadata"))?;
    let module_values = values::ValueTable::module_values(module_block, &types)?;
    let interface = metadata.stage_interface(stage, &module_values, &types)?;
    // AIR's entry metadata names the function by its module value id, and
    // module globals share that numbering with functions.
    let entry_ordinal = function_record_ordinal(module_block, interface.function)
        .ok_or_else(|| semantic("AIR entry metadata names a value which is not a function"))?;
    let entry = function_records
        .iter()
        .position(|(ordinal, _)| *ordinal == entry_ordinal)
        .ok_or_else(|| semantic("AIR entry metadata names a function without a body"))?;
    let (_, record) = function_records[entry];
    // The value-symbol table is keyed by module value id, which is what the
    // entry metadata already names. It is not the function's position among
    // the function records: module globals share that numbering, so a kernel
    // whose module declares `threadgroup` arrays was taking the name of the
    // first of them.
    let name = legacy_names
        .get(&interface.function)
        .cloned()
        .ok_or_else(|| semantic("packed AIR entry has no value-symbol name"))?;
    let signature_index = usize::try_from(
        *record
            .fields()
            .first()
            .ok_or_else(|| semantic("packed AIR function has no type"))?,
    )
    .map_err(|_| semantic("packed AIR function type does not fit in memory"))?;
    let signature = types
        .get(signature_index)
        .ok_or_else(|| semantic("packed AIR function refers to a missing type"))?;
    // Old LLVM module records name the pointer-to-function type. Newer
    // producers name the function type directly.
    let signature = match signature {
        DecodedType::Pointer { pointee, .. } => types
            .get(*pointee)
            .ok_or_else(|| semantic("packed AIR function pointer has no pointee type"))?,
        signature => signature,
    };
    let DecodedType::Function { parameters, result } = signature else {
        return Err(semantic(format!(
            "packed AIR function type {signature_index} is {}, not a function",
            signature.description()
        )));
    };
    let body = function_blocks[entry];
    let value_table = values::ValueTable::decode(module_block, body, &types, parameters)?;
    let opaque_types = interface
        .resources
        .iter()
        .filter_map(|resource| Some((resource.argument, resource.ty.clone()?)))
        .collect();
    let sections = section_names(module_block);
    let globals = value_table.global_variables(&types, legacy_names, &sections)?;
    let static_initializers = static_initializers(
        module_block,
        function_records,
        function_blocks,
        &types,
        legacy_names,
        &sections,
        entry,
    )?;
    let function = value_table.lower_body(
        body,
        &types,
        &values::Signature {
            names: legacy_names,
            name,
            parameter_types: parameters,
            result_type: *result,
            opaque_types: &opaque_types,
        },
    )?;
    let resources = interface
        .resources
        .iter()
        .map(|resource| {
            let parameter = function
                .parameters
                .get(resource.argument as usize)
                .ok_or_else(|| semantic("AIR resource names a missing argument"))?;
            Ok(Resource {
                argument: resource.argument,
                kind: resource.kind,
                index: resource.index,
                count: resource.count,
                access: resource.access,
                ty: parameter.ty.clone(),
                name: resource.name.clone(),
                inline_sampler: None,
            })
        })
        .collect::<Result<Vec<_>, TranslateError>>()?;
    // The metadata numbers outputs by position whatever the function returns;
    // an entry returning one unwrapped value has no field to name.
    let mut outputs = interface.outputs;
    if !matches!(function.result, crate::ir::Type::Struct(_)) {
        for output in &mut outputs {
            output.field = None;
        }
    }
    let module = Module {
        static_initializers,
        functions: vec![function],
        globals,
        entries: vec![EntryPoint {
            early_fragment_tests: false,
            function: 0,
            stage,
            resources,
            inputs: interface.inputs,
            outputs,
        }],
    };
    module.verify()?;
    Ok(module)
}

/// The module's section names, in the order a record's one-based section
/// field counts them.
fn section_names(module_block: &BitcodeBlock) -> Vec<String> {
    module_block
        .elements
        .iter()
        .filter_map(BitcodeElement::as_record)
        .filter(|record| record.id == 5)
        .map(|record| {
            record_values(record)
                .iter()
                .filter_map(|&character| char::from_u32(u32::try_from(character).ok()?))
                .collect()
        })
        .collect()
}

/// The section a record's one-based section field names.
pub(super) fn section_name(field: u64, sections: &[String]) -> Option<&str> {
    let index = usize::try_from(field).ok()?.checked_sub(1)?;
    sections.get(index).map(String::as_str)
}

/// Lower every function in the `air.static_init` section, in module order.
///
/// They compute a module's derived constants into its globals before an
/// entry point runs, and [`crate::specialize()`] runs them. A function
/// record's section is its seventh field in the layout Apple's AIR writer
/// uses: `[type, cc, isproto, linkage, paramattr, alignment, section, ...]`.
fn static_initializers(
    module_block: &BitcodeBlock,
    function_records: &[(usize, &&llvm_bitcode::bitcode::Record)],
    function_blocks: &[&BitcodeBlock],
    types: &[DecodedType],
    names: &HashMap<usize, String>,
    sections: &[String],
    entry: usize,
) -> Result<Vec<crate::ir::Function>, TranslateError> {
    let mut initializers = Vec::new();
    for (position, (ordinal, record)) in function_records.iter().enumerate() {
        let section = record.fields().get(6).copied().unwrap_or(0);
        if position == entry || section_name(section, sections) != Some("air.static_init") {
            continue;
        }
        let name = function_value_id(module_block, *ordinal)
            .and_then(|value| names.get(&value).cloned())
            .unwrap_or_else(|| format!("static initializer {position}"));
        let signature = record
            .fields()
            .first()
            .and_then(|index| types.get(usize::try_from(*index).ok()?));
        let signature = match signature {
            Some(DecodedType::Pointer { pointee, .. }) => types.get(*pointee),
            signature => signature,
        };
        let Some(DecodedType::Function { parameters, result }) = signature else {
            return Err(semantic(format!(
                "static initializer {name} has no function type"
            )));
        };
        if !parameters.is_empty() {
            return Err(semantic(format!(
                "static initializer {name} takes arguments"
            )));
        }
        let body = function_blocks[position];
        let table = values::ValueTable::decode(module_block, body, types, parameters)?;
        let no_opaque_types = HashMap::new();
        initializers.push(table.lower_body(
            body,
            types,
            &values::Signature {
                names,
                name,
                parameter_types: parameters,
                result_type: *result,
                opaque_types: &no_opaque_types,
            },
        )?);
    }
    Ok(initializers)
}

#[derive(Clone, Debug)]
pub(super) enum DecodedType {
    Void,
    Float(u8),
    Integer(u8),
    Vector {
        element: usize,
        lanes: u32,
    },
    Array {
        element: usize,
        count: u32,
    },
    Struct {
        fields: Vec<usize>,
        packed: bool,
    },
    Pointer {
        pointee: usize,
        address_space: u64,
    },
    Function {
        result: usize,
        parameters: Vec<usize>,
    },
    Metadata,
    /// An LLVM identified opaque type. AIR spells Metal's textures and
    /// samplers this way; the Metal shape comes from the entry point's
    /// `air.arg_type_name` metadata, not from this name.
    Opaque(String),
    /// A basic-block label type. It never types a value.
    Label,
}

impl DecodedType {
    pub(super) fn description(&self) -> String {
        match self {
            Self::Void => "void".to_string(),
            Self::Float(bits) => format!("f{bits}"),
            Self::Integer(bits) => format!("i{bits}"),
            Self::Vector { element, lanes } => format!("vector<{element}; {lanes}>"),
            Self::Array { element, count } => format!("array<{element}; {count}>"),
            Self::Struct { fields, packed } => {
                format!(
                    "{}struct[{}]",
                    if *packed { "packed " } else { "" },
                    fields.len()
                )
            }
            Self::Pointer {
                pointee,
                address_space,
            } => format!("ptr({address_space})<{pointee}>"),
            Self::Function { result, parameters } => {
                format!("fn({})->{result}", parameters.len())
            }
            Self::Metadata => "metadata".to_string(),
            Self::Opaque(name) => format!("opaque {name}"),
            Self::Label => "label".to_string(),
        }
    }
}

/// Decode only the LLVM type records represented by the first packed AIR
/// entry.  Referencing a type before it has been defined is rejected: AIR's
/// table is ordered, and accepting a forward reference here would change the
/// meaning of recursive structures.
fn decode_type_table(block: &BitcodeBlock) -> Result<Vec<DecodedType>, TranslateError> {
    let mut types = Vec::new();
    let mut pending_named_struct = None;
    for (record_ordinal, record) in block
        .elements
        .iter()
        .filter_map(BitcodeElement::as_record)
        .enumerate()
    {
        let fields = record_values(record);
        let minimum_fields = match record.id {
            7 | 8 => 1,
            11 | 12 | 21 => 2,
            _ => 0,
        };
        if fields.len() < minimum_fields {
            return Err(semantic(format!(
                "packed AIR type record {record_ordinal} (code {}) has {} field(s), expected at least {minimum_fields}",
                record.id,
                fields.len()
            )));
        }
        let type_record = match record.id {
            1 => continue, // NUMENTRY
            2 => DecodedType::Void,
            3 => DecodedType::Float(32),
            7 => DecodedType::Integer(
                u8::try_from(
                    *fields
                        .first()
                        .ok_or_else(|| semantic("integer type has no width"))?,
                )
                .map_err(|_| semantic("integer type width is too large"))?,
            ),
            10 => DecodedType::Float(16),
            11 => DecodedType::Array {
                count: u32::try_from(
                    *fields
                        .first()
                        .ok_or_else(|| semantic("array type has no count"))?,
                )
                .map_err(|_| semantic("array count is too large"))?,
                element: type_reference(fields.get(1), types.len())?,
            },
            12 => DecodedType::Vector {
                lanes: u32::try_from(
                    *fields
                        .first()
                        .ok_or_else(|| semantic("vector type has no lane count"))?,
                )
                .map_err(|_| semantic("vector lane count is too large"))?,
                element: type_reference(fields.get(1), types.len())?,
            },
            18 => DecodedType::Struct {
                packed: fields.first() == Some(&1),
                fields: fields[1..]
                    .iter()
                    .map(|field| type_reference(Some(field), types.len()))
                    .collect::<Result<_, _>>()?,
            },
            19 => {
                let name = record
                    .clone()
                    .take_payload()
                    .and_then(payload_string)
                    .or_else(|| {
                        record
                            .fields()
                            .iter()
                            .map(|byte| u8::try_from(*byte).ok())
                            .collect::<Option<Vec<_>>>()
                            .and_then(|bytes| String::from_utf8(bytes).ok())
                    })
                    .ok_or_else(|| semantic("named struct has no UTF-8 name"))?;
                // STRUCT_NAME allocates the identified type; STRUCT_NAMED or
                // OPAQUE fills that slot rather than allocating another entry.
                pending_named_struct = Some((types.len(), name));
                types.push(DecodedType::Void);
                continue;
            }
            20 => {
                let (index, _) = pending_named_struct
                    .take()
                    .ok_or_else(|| semantic("named struct has no preceding name"))?;
                let packed = fields.first() == Some(&1);
                let fields = fields[1..]
                    .iter()
                    .map(|field| type_reference(Some(field), types.len()))
                    .collect::<Result<_, _>>()?;
                types[index] = DecodedType::Struct { packed, fields };
                continue;
            }
            8 => DecodedType::Pointer {
                pointee: type_reference(fields.first(), types.len())?,
                address_space: *fields.get(1).unwrap_or(&0),
            },
            21 => DecodedType::Function {
                result: type_reference(fields.get(1), types.len())?,
                parameters: fields[2..]
                    .iter()
                    .map(|field| type_reference(Some(field), types.len()))
                    .collect::<Result<_, _>>()?,
            },
            16 => DecodedType::Metadata,
            5 => DecodedType::Label,
            6 => {
                let (index, name) = pending_named_struct
                    .take()
                    .ok_or_else(|| semantic("opaque type has no preceding name"))?;
                types[index] = DecodedType::Opaque(name);
                continue;
            }
            _ => {
                return Err(semantic(format!(
                    "unsupported packed AIR type record {}",
                    record.id
                )));
            }
        };
        types.push(type_record);
    }
    Ok(types)
}

/// Abbreviated LLVM records keep array operands in `Payload::Array` rather
/// than `Record::fields()`. They are still consecutive operands of the same
/// logical record (not an attached blob), notably for old AIR function types.
pub(super) fn record_values(record: &llvm_bitcode::bitcode::Record) -> Vec<u64> {
    let mut values = record.fields().to_vec();
    if let Some(Payload::Array(array)) = record.clone().take_payload() {
        values.extend(array);
    }
    values
}

/// The position among a module's `FUNCTION` records of the value `id` names.
///
/// LLVM numbers module-level values in record order, and a global variable
/// takes an id just as a function does, so the two orders only agree in a
/// module without globals.
fn function_record_ordinal(module: &BitcodeBlock, id: usize) -> Option<usize> {
    let mut value = 0;
    let mut functions = 0;
    for record in module.elements.iter().filter_map(BitcodeElement::as_record) {
        match record.id {
            8 => {
                if value == id {
                    return Some(functions);
                }
                functions += 1;
                value += 1;
            }
            // The other records which allocate a module-level value id.
            7 | 9 | 14 | 18 => {
                if value == id {
                    return None;
                }
                value += 1;
            }
            _ => {}
        }
    }
    None
}

/// The module value id of the `ordinal`-th `FUNCTION` record, which is the key
/// the value-symbol table uses. Module globals share LLVM's module-level value
/// numbering with functions, so the two only agree in a module with no globals.
fn function_value_id(module: &BitcodeBlock, ordinal: usize) -> Option<usize> {
    let mut value = 0;
    let mut functions = 0;
    for record in module.elements.iter().filter_map(BitcodeElement::as_record) {
        match record.id {
            8 => {
                if functions == ordinal {
                    return Some(value);
                }
                functions += 1;
                value += 1;
            }
            // The other records which allocate a module-level value id.
            7 | 9 | 14 | 18 => value += 1,
            _ => {}
        }
    }
    None
}

pub(super) fn type_reference(field: Option<&u64>, defined: usize) -> Result<usize, TranslateError> {
    let index =
        usize::try_from(*field.ok_or_else(|| semantic("type record has no type reference"))?)
            .map_err(|_| semantic("type reference does not fit in memory"))?;
    if index >= defined {
        return Err(semantic(
            "packed AIR type has a forward or missing reference",
        ));
    }
    Ok(index)
}

/// Metadata names are records, not incidental string payloads.  Pair each
/// NAME with its following NAMED_NODE and select the entry stage from that
/// relationship.
fn metadata_entry_stage(module_block: &BitcodeBlock) -> Option<Stage> {
    for block in module_block
        .elements
        .iter()
        .filter_map(BitcodeElement::as_block)
        .filter(|block| block.id == 15)
    {
        let mut name = None;
        for record in block.elements.iter().filter_map(BitcodeElement::as_record) {
            match record.id {
                4 => {
                    name = record
                        .clone()
                        .take_payload()
                        .and_then(payload_string)
                        .or_else(|| {
                            record
                                .fields()
                                .iter()
                                .map(|byte| u8::try_from(*byte).ok())
                                .collect::<Option<Vec<_>>>()
                                .and_then(|bytes| String::from_utf8(bytes).ok())
                        });
                }
                10 if !record.fields().is_empty() => match name.as_deref() {
                    Some("air.vertex") => return Some(Stage::Vertex),
                    Some("air.fragment") => return Some(Stage::Fragment),
                    Some("air.kernel") => return Some(Stage::Kernel),
                    _ => name = None,
                },
                _ => {}
            }
        }
    }
    None
}

/// Resolve both LLVM function-name encodings used by AIR. Modern bitcode puts
/// name slices in STRINGTABLE_BLOCK. Apple's older AIR writer uses a
/// VALUE_SYMTAB_BLOCK entry whose payload is the name and whose first field is
/// the module value id of its MODULE_CODE_FUNCTION record.
fn resolve_function_name(
    record: &llvm_bitcode::bitcode::Record,
    value_id: usize,
    string_table: Option<&[u8]>,
    legacy_names: &HashMap<usize, String>,
) -> Option<String> {
    if let Some(string_table) = string_table {
        let offset = usize::try_from(*record.fields().first()?).ok()?;
        let length = usize::try_from(*record.fields().get(1)?).ok()?;
        return std::str::from_utf8(string_table.get(offset..offset.checked_add(length)?)?)
            .ok()
            .map(str::to_owned);
    }
    legacy_names.get(&value_id).cloned()
}

fn legacy_function_names(module_block: &BitcodeBlock) -> HashMap<usize, String> {
    let Some(symbol_table) = child_block(&module_block.elements, 14) else {
        return HashMap::new();
    };
    symbol_table
        .elements
        .iter()
        .filter_map(BitcodeElement::as_record)
        .filter(|record| matches!(record.id, 1 | 3))
        .filter_map(|record| {
            let value = usize::try_from(*record.fields().first()?).ok()?;
            let mut record = record.clone();
            let name = record.take_payload().and_then(payload_string).or_else(|| {
                let prefix = match record.id {
                    // VST_CODE_ENTRY: value id, name bytes.
                    1 => 1,
                    // VST_CODE_FNENTRY: value id, function offset, name bytes.
                    3 => 2,
                    _ => return None,
                };
                record_values(&record)
                    .get(prefix..)?
                    .iter()
                    .map(|byte| u8::try_from(*byte).ok())
                    .collect::<Option<Vec<_>>>()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
            })?;
            Some((value, name))
        })
        .collect()
}

fn child_block(elements: &[BitcodeElement], id: u32) -> Option<&BitcodeBlock> {
    elements
        .iter()
        .filter_map(BitcodeElement::as_block)
        .find(|block| block.id == id)
}

fn descendant_block(elements: &[BitcodeElement], id: u32) -> Option<&BitcodeBlock> {
    for block in elements.iter().filter_map(BitcodeElement::as_block) {
        if block.id == id {
            return Some(block);
        }
        if let Some(found) = descendant_block(&block.elements, id) {
            return Some(found);
        }
    }
    None
}

fn first_payload(block: &BitcodeBlock, record_id: u64) -> Option<Payload> {
    let mut record = block
        .elements
        .iter()
        .filter_map(BitcodeElement::as_record)
        .find(|record| record.id == record_id)?
        .clone();
    record.take_payload()
}

fn payload_bytes(payload: Payload) -> Option<Vec<u8>> {
    match payload {
        Payload::Blob(bytes) => Some(bytes),
        Payload::Array(values) => values
            .into_iter()
            .map(|value| u8::try_from(value).ok())
            .collect(),
        Payload::Char6String(text) => Some(text.into_bytes()),
    }
}

pub(super) fn payload_string(payload: Payload) -> Option<String> {
    match payload {
        Payload::Char6String(text) => Some(text),
        Payload::Array(values) => String::from_utf8(
            values
                .into_iter()
                .map(|value| u8::try_from(value).ok())
                .collect::<Option<_>>()?,
        )
        .ok(),
        Payload::Blob(bytes) => String::from_utf8(bytes).ok(),
    }
}

fn collect_payload_text(elements: &[BitcodeElement]) -> String {
    let mut output = String::new();
    for element in elements {
        match element {
            BitcodeElement::Block(block) => output.push_str(&collect_payload_text(&block.elements)),
            BitcodeElement::Record(record) => {
                let mut record = record.clone();
                if let Some(payload) = record.take_payload()
                    && let Some(bytes) = payload_bytes(payload)
                {
                    output.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
        }
    }
    output
}

pub(super) fn decode_sign_rotated(value: u64) -> i64 {
    if value == 1 {
        return i64::MIN;
    }
    let magnitude = (value >> 1) as i64;
    if value & 1 == 0 {
        magnitude
    } else {
        -magnitude
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::air::fixtures::{bitcode_block, block, record};

    #[test]
    fn function_symbol_entry_skips_its_bitcode_offset() {
        let mut fields = vec![7, 1234];
        fields.extend(b"kernel".iter().copied().map(u64::from));
        let module = bitcode_block(block(8, vec![block(14, vec![record(3, &fields)])]));
        assert_eq!(
            legacy_function_names(&module).get(&7),
            Some(&"kernel".to_string())
        );
    }
}
