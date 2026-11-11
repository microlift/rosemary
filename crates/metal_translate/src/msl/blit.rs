use super::*;

/// What a blit kernel's source says, once parsed and checked.
///
/// Parsing decides the kernel's shape and emission builds the IR for it; the
/// two phases talk through this rather than through three dozen locals.
struct BlitShape<'a> {
    /// The kernel body between the bounds guard's braces, for diagnostics.
    inner: &'a str,
    /// Indices into the kernel's parameters: the `device` buffer the blit
    /// reads, the texture it writes, and the parameter structure.
    input: usize,
    output: usize,
    /// The buffer's element type, and how many 8-bit lanes it has.
    element_type: Type,
    element_lanes: u8,
    /// Lanes in the destination coordinate: two for a 2D texture, else three.
    coordinate_lanes: u8,
    /// The thread-position built-in and the parameter structure, by the names
    /// the source gave them.
    gid_name: &'a str,
    gid: ValueId,
    params_name: &'a str,
    params_pointee: Type,
    structure: StructType,
    params: usize,
    /// The index of the size field inside that structure.
    size_field: usize,
    /// The buffer index expression, still unparsed.
    index_expression: &'a str,
}

pub(super) fn parse_blit_kernel(
    source: &str,
    name: &str,
    body: &str,
    parameters: Vec<Parameter>,
    values: Vec<Type>,
    resources: Vec<Resource>,
    builtins: HashMap<String, ValueId>,
) -> Result<Module, TranslateError> {
    let shape = blit_shape(source, body, &parameters, &builtins)?;
    emit_blit(source, name, &shape, parameters, values, resources)
}

/// Parse and check one blit kernel's source, without building any IR.
fn blit_shape<'a>(
    source: &str,
    body: &'a str,
    parameters: &[Parameter],
    builtins: &HashMap<String, ValueId>,
) -> Result<BlitShape<'a>, TranslateError> {
    let guard = body.trim_start();
    let open = guard.find('(').ok_or_else(|| {
        syntax_at(
            source,
            source.find(body).unwrap_or(0),
            "MSL blit guard is malformed",
        )
    })?;
    let close = matching(guard, open, '(', ')').ok_or_else(|| {
        syntax_at(
            source,
            source.find(guard).unwrap_or(0),
            "MSL blit guard is malformed",
        )
    })?;
    let condition = guard[open + 1..close].trim();
    let (gid_name, size_expression) = condition
        .strip_prefix("all(")
        .and_then(|value| value.strip_suffix(')'))
        .and_then(|value| value.split_once('<'))
        .ok_or_else(|| {
            syntax_at(
                source,
                source.find(guard).unwrap_or(0),
                "unsupported MSL conditional expression",
            )
        })?;
    let gid = builtins.get(gid_name.trim()).copied().ok_or_else(|| {
        syntax_at(
            source,
            source.find(guard).unwrap_or(0),
            "blit guard must use thread_position_in_grid",
        )
    })?;
    let (params_name, field_name) = size_expression.trim().split_once('.').ok_or_else(|| {
        syntax_at(
            source,
            source.find(guard).unwrap_or(0),
            "blit guard must compare against a parameter field",
        )
    })?;
    let params_index = parameters
        .iter()
        .position(|parameter| parameter.name == params_name.trim())
        .ok_or_else(|| {
            syntax_at(
                source,
                source.find(guard).unwrap_or(0),
                "blit guard parameter is missing",
            )
        })?;
    let Type::Pointer {
        pointee: params_pointee,
        ..
    } = &parameters[params_index].ty
    else {
        return Err(syntax_at(
            source,
            source.find(guard).unwrap_or(0),
            "blit guard parameter is not a buffer",
        ));
    };
    let Type::Struct(structure) = params_pointee.as_ref() else {
        return Err(syntax_at(
            source,
            source.find(guard).unwrap_or(0),
            "blit guard parameter is not a struct",
        ));
    };
    let field = structure
        .fields
        .iter()
        .enumerate()
        .find(|(_, field)| field.name.as_deref() == Some(field_name.trim()))
        .ok_or_else(|| {
            syntax_at(
                source,
                source.find(guard).unwrap_or(0),
                "blit size field is missing",
            )
        })?;
    if field.1.ty
        != (Type::Vector {
            element: Box::new(Type::u32()),
            lanes: 3,
        })
    {
        return Err(syntax_at(
            source,
            source.find(guard).unwrap_or(0),
            "blit size field must be uint3",
        ));
    }
    let guarded = guard[close + 1..].trim_start();
    let body_open = guarded.find('{').ok_or_else(|| {
        syntax_at(
            source,
            source.find(guard).unwrap_or(0),
            "MSL blit guard has no body",
        )
    })?;
    let body_close = matching(guarded, body_open, '{', '}').ok_or_else(|| {
        syntax_at(
            source,
            source.find(guard).unwrap_or(0),
            "MSL blit guard has no body",
        )
    })?;
    let inner = &guarded[body_open + 1..body_close];
    let load = inner.split(';').next().unwrap_or("").trim();
    let (_, index_expression) = load
        .split_once('[')
        .and_then(|(left, right)| right.strip_suffix(']').map(|right| (left, right)))
        .ok_or_else(|| {
            syntax_at(
                source,
                source.find(inner).unwrap_or(0),
                format!(
                    "unsupported MSL buffer element load ({})",
                    blit_statement_id(load)
                ),
            )
        })?;
    let input_name = load
        .split_once('=')
        .map(|(_, right)| right.trim().split('[').next().unwrap_or("").trim())
        .unwrap_or("");
    let element_name = load
        .split_once('=')
        .and_then(|(left, _)| left.split_whitespace().last())
        .unwrap_or("");
    let input_index = parameters
        .iter()
        .position(|parameter| parameter.name == input_name)
        .ok_or_else(|| {
            syntax_at(
                source,
                source.find(inner).unwrap_or(0),
                "blit input buffer is missing",
            )
        })?;
    let Type::Pointer {
        space: AddressSpace::Device,
        pointee: input_pointee,
    } = &parameters[input_index].ty
    else {
        return Err(syntax_at(
            source,
            source.find(inner).unwrap_or(0),
            "blit input must be a device buffer",
        ));
    };
    let element_type = input_pointee.as_ref().clone();
    let uchar = Type::Int {
        bits: 8,
        signed: false,
    };
    // The upload blits read an unsigned 8-bit element of one, two or four
    // lanes. How many it has fixes which write form is correct, so the two
    // are checked against each other below rather than independently.
    let element_lanes = match &element_type {
        ty if *ty == uchar => Some(1),
        Type::Vector { element, lanes } if **element == uchar && matches!(*lanes, 2 | 4) => {
            Some(*lanes)
        }
        _ => None,
    }
    .ok_or_else(|| {
        syntax_at(
            source,
            source.find(inner).unwrap_or(0),
            format!(
                "unsupported MSL buffer element load ({})",
                blit_statement_id(load)
            ),
        )
    })?;
    let write = inner.split(';').nth(1).unwrap_or("").trim();
    let (output_name, write_value, write_coordinate) =
        parse_blit_write(write).ok_or_else(|| {
            syntax_at(
                source,
                source.find(inner).unwrap_or(0),
                format!(
                    "unsupported MSL blit output operation ({})",
                    blit_statement_id(write)
                ),
            )
        })?;
    let output_index = parameters
        .iter()
        .position(|parameter| parameter.name == output_name)
        .ok_or_else(|| {
            syntax_at(
                source,
                source.find(inner).unwrap_or(0),
                "blit output texture is missing",
            )
        })?;
    let output = &parameters[output_index];
    let dimension = if output.ty == writable_float_texture(TextureDimension::D3) {
        Some(TextureDimension::D3)
    } else if output.ty == writable_float_texture(TextureDimension::D2) {
        Some(TextureDimension::D2)
    } else {
        None
    };
    // A two-dimensional destination takes a two-lane coordinate, which is
    // what Metal's `gid.xy + params.offset.xy` spells and what the verifier
    // requires of an image write.
    let coordinate_lanes: u8 = match &dimension {
        Some(TextureDimension::D2) => 2,
        _ => 3,
    };
    if dimension.is_none()
        || normalized_write(write_value, element_name) != Some(element_lanes)
        || !is_offset_coordinate(
            write_coordinate,
            gid_name.trim(),
            params_name.trim(),
            coordinate_lanes,
        )
    {
        return Err(syntax_at(
            source,
            source.find(inner).unwrap_or(0),
            format!(
                "unsupported MSL blit output operation ({})",
                blit_statement_id(write)
            ),
        ));
    }
    Ok(BlitShape {
        inner,
        input: input_index,
        output: output_index,
        element_type,
        element_lanes,
        coordinate_lanes,
        gid_name: gid_name.trim(),
        gid,
        params_name: params_name.trim(),
        params_pointee: params_pointee.as_ref().clone(),
        structure: structure.clone(),
        params: params_index,
        size_field: field.0,
        index_expression,
    })
}

/// Build the Metal IR for a blit kernel of the given shape.
fn emit_blit(
    source: &str,
    name: &str,
    shape: &BlitShape<'_>,
    parameters: Vec<Parameter>,
    mut values: Vec<Type>,
    resources: Vec<Resource>,
) -> Result<Module, TranslateError> {
    let &BlitShape {
        inner,
        input,
        output,
        ref element_type,
        element_lanes,
        coordinate_lanes,
        gid_name,
        gid,
        params_name,
        ref params_pointee,
        ref structure,
        params,
        size_field,
        index_expression,
    } = shape;
    let element_type = element_type.clone();
    let params = &parameters[params];
    let input = &parameters[input];
    let output = &parameters[output];
    let add = |values: &mut Vec<Type>, ty: Type| {
        let value = ValueId(values.len() as u32);
        values.push(ty);
        value
    };
    let uint3 = Type::Vector {
        element: Box::new(Type::u32()),
        lanes: 3,
    };
    let bool3 = Type::Vector {
        element: Box::new(Type::Bool),
        lanes: 3,
    };
    let element_ty = element_type;
    let float = Type::Float { bits: 32 };
    let float_n = if element_lanes == 1 {
        float.clone()
    } else {
        Type::Vector {
            element: Box::new(float.clone()),
            lanes: element_lanes,
        }
    };
    let float4 = Type::Vector {
        element: Box::new(float.clone()),
        lanes: 4,
    };
    let uint2 = Type::Vector {
        element: Box::new(Type::u32()),
        lanes: 2,
    };
    let params_value = add(&mut values, params_pointee.clone());
    let size = add(&mut values, uint3.clone());
    let in_bounds = add(&mut values, bool3);
    let all = add(&mut values, Type::Bool);
    let index = lower_blit_index(
        index_expression.trim(),
        IndexBindings {
            gid_name: gid_name.trim(),
            gid,
            params_name: params_name.trim(),
            params: params_value,
            structure,
        },
        &mut values,
        source,
        source.find(inner).unwrap_or(0),
    )?;
    let element_pointer = add(
        &mut values,
        Type::Pointer {
            space: AddressSpace::Device,
            pointee: Box::new(element_ty.clone()),
        },
    );
    let element = add(&mut values, element_ty);
    let converted = add(&mut values, float_n.clone());
    // A scalar element divides by a scalar; only a vector needs the splat.
    let divisor = (element_lanes > 1).then(|| add(&mut values, float_n.clone()));
    let normalized = add(&mut values, float_n);
    let zero_pad = (element_lanes == 2).then(|| {
        add(
            &mut values,
            Type::Vector {
                element: Box::new(float.clone()),
                lanes: 2,
            },
        )
    });
    // Metal writes a four-lane texel whatever the element's width.
    let texel = if element_lanes == 4 {
        normalized
    } else {
        add(&mut values, float4)
    };
    let offset = add(&mut values, uint3.clone());
    let gid_coordinate = (coordinate_lanes == 2).then(|| add(&mut values, uint2.clone()));
    let offset_coordinate = (coordinate_lanes == 2).then(|| add(&mut values, uint2.clone()));
    let coordinate = add(
        &mut values,
        if coordinate_lanes == 2 {
            uint2
        } else {
            uint3.clone()
        },
    );
    let offset_field = structure
        .fields
        .iter()
        .enumerate()
        .find(|(_, field)| field.name.as_deref() == Some("offset"))
        .ok_or_else(|| {
            syntax_at(
                source,
                source.find(inner).unwrap_or(0),
                "blit offset field is missing",
            )
        })?;
    if offset_field.1.ty != uint3 {
        return Err(syntax_at(
            source,
            source.find(inner).unwrap_or(0),
            "blit offset field must be uint3",
        ));
    }
    let entry = Block {
        id: BlockId(0),
        name: "entry".to_string(),
        instructions: vec![
            Instruction {
                result: Some(gid),
                kind: InstructionKind::GlobalInvocationId,
            },
            Instruction {
                result: Some(params_value),
                kind: InstructionKind::Load {
                    pointer: Operand::Value(params.value),
                },
            },
            Instruction {
                result: Some(size),
                kind: InstructionKind::CompositeExtract {
                    composite: Operand::Value(params_value),
                    index: size_field as u32,
                },
            },
            Instruction {
                result: Some(in_bounds),
                kind: InstructionKind::IntegerCompare {
                    predicate: IntegerPredicate::UnsignedLessThan,
                    lhs: Operand::Value(gid),
                    rhs: Operand::Value(size),
                },
            },
            Instruction {
                result: Some(all),
                kind: InstructionKind::All {
                    vector: Operand::Value(in_bounds),
                },
            },
        ],
        terminator: Terminator::CondBranch {
            condition: Operand::Value(all),
            true_block: BlockId(1),
            false_block: BlockId(2),
        },
    };
    let write_block = Block {
        id: BlockId(1),
        name: "write".to_string(),
        instructions: {
            let mut instructions = index.instructions;
            instructions.extend([
                Instruction {
                    result: Some(element_pointer),
                    kind: InstructionKind::ElementAddress {
                        pointer: Operand::Value(input.value),
                        index: Operand::Value(index.value),
                    },
                },
                Instruction {
                    result: Some(element),
                    kind: InstructionKind::Load {
                        pointer: Operand::Value(element_pointer),
                    },
                },
                Instruction {
                    result: Some(converted),
                    kind: InstructionKind::Convert {
                        op: ConvertOp::UnsignedToFloat,
                        value: Operand::Value(element),
                    },
                },
            ]);
            if let Some(divisor) = divisor {
                instructions.push(Instruction {
                    result: Some(divisor),
                    kind: InstructionKind::VectorSplat {
                        value: Operand::f32(255.0),
                        lanes: element_lanes,
                    },
                });
            }
            instructions.push(Instruction {
                result: Some(normalized),
                kind: InstructionKind::FloatBinary {
                    op: FloatBinaryOp::Divide,
                    lhs: Operand::Value(converted),
                    rhs: divisor.map_or(Operand::f32(255.0), Operand::Value),
                },
            });
            // A scalar is broadcast into the texel, which is what Metal's
            // implicit scalar-to-vector conversion does; a two-lane element
            // is padded with the zeroes the source itself names.
            if element_lanes == 1 {
                instructions.push(Instruction {
                    result: Some(texel),
                    kind: InstructionKind::VectorSplat {
                        value: Operand::Value(normalized),
                        lanes: 4,
                    },
                });
            } else if let Some(zero) = zero_pad {
                instructions.extend([
                    Instruction {
                        result: Some(zero),
                        kind: InstructionKind::VectorSplat {
                            value: Operand::f32(0.0),
                            lanes: 2,
                        },
                    },
                    Instruction {
                        result: Some(texel),
                        kind: InstructionKind::VectorShuffle {
                            first: Operand::Value(normalized),
                            second: Operand::Value(zero),
                            components: vec![0, 1, 2, 3],
                        },
                    },
                ]);
            }
            instructions.push(Instruction {
                result: Some(offset),
                kind: InstructionKind::CompositeExtract {
                    composite: Operand::Value(params_value),
                    index: offset_field.0 as u32,
                },
            });
            match (gid_coordinate, offset_coordinate) {
                (Some(gid_xy), Some(offset_xy)) => instructions.extend([
                    Instruction {
                        result: Some(gid_xy),
                        kind: InstructionKind::VectorShuffle {
                            first: Operand::Value(gid),
                            second: Operand::Value(gid),
                            components: vec![0, 1],
                        },
                    },
                    Instruction {
                        result: Some(offset_xy),
                        kind: InstructionKind::VectorShuffle {
                            first: Operand::Value(offset),
                            second: Operand::Value(offset),
                            components: vec![0, 1],
                        },
                    },
                    Instruction {
                        result: Some(coordinate),
                        kind: InstructionKind::IntegerBinary {
                            op: IntegerBinaryOp::Add,
                            lhs: Operand::Value(gid_xy),
                            rhs: Operand::Value(offset_xy),
                        },
                    },
                ]),
                _ => instructions.push(Instruction {
                    result: Some(coordinate),
                    kind: InstructionKind::IntegerBinary {
                        op: IntegerBinaryOp::Add,
                        lhs: Operand::Value(gid),
                        rhs: Operand::Value(offset),
                    },
                }),
            }
            instructions.push(Instruction {
                result: None,
                kind: InstructionKind::TextureWrite {
                    array_index: None,
                    texture: Operand::Value(output.value),
                    coordinate: Operand::Value(coordinate),
                    value: Operand::Value(texel),
                },
            });
            instructions
        },
        terminator: Terminator::Return(None),
    };
    let module = Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![Function {
            name: name.to_string(),
            result: Type::Void,
            parameters,
            values,
            blocks: vec![
                entry,
                write_block,
                Block {
                    id: BlockId(2),
                    name: "out_of_bounds".to_string(),
                    instructions: vec![],
                    terminator: Terminator::Return(None),
                },
            ],
        }],
        entries: vec![EntryPoint {
            early_fragment_tests: false,
            function: 0,
            stage: Stage::Kernel,
            resources,
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
    };
    module.verify()?;
    Ok(module)
}

/// Split the exact two-argument image write form used by the uploaded blit
/// kernel. Nested constructors make a plain comma split incorrect.
fn parse_blit_write(statement: &str) -> Option<(&str, &str, &str)> {
    let (output, arguments) = statement.split_once(".write(")?;
    let arguments = arguments.strip_suffix(')')?;
    let comma = top_level_comma(arguments)?;
    Some((
        output.trim(),
        arguments[..comma].trim(),
        arguments[comma + 1..].trim(),
    ))
}

fn top_level_comma(arguments: &str) -> Option<usize> {
    let mut depth = 0_u32;
    for (offset, character) in arguments.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => return Some(offset),
            _ => {}
        }
    }
    None
}

/// How many lanes the normalized write form names, or `None` for a form this
/// frontend does not implement.
///
/// The uploaded blits spell one operation three ways, one per element width:
/// a four-lane element fills the texel, a scalar is broadcast into it by
/// Metal's implicit conversion, and a two-lane element names the zeroes which
/// fill the rest.
fn normalized_write(value: &str, element: &str) -> Option<u8> {
    let compact: String = value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    for divisor in ["255.f", "255.0f", "255.0"] {
        if compact == format!("float4({element})/{divisor}") {
            return Some(4);
        }
        if compact == format!("float({element})/{divisor}") {
            return Some(1);
        }
        for zero in ["0.f", "0.0f", "0.0"] {
            if compact == format!("float4(float2({element})/{divisor},{zero},{zero})") {
                return Some(2);
            }
        }
    }
    None
}

/// Whether the coordinate is `gid + params.offset`, in the lane count the
/// destination texture takes.
fn is_offset_coordinate(coordinate: &str, gid: &str, params: &str, lanes: u8) -> bool {
    let compact: String = coordinate
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    match lanes {
        2 => compact == format!("{gid}.xy+{params}.offset.xy"),
        _ => compact == format!("{gid}+{params}.offset"),
    }
}

/// A bounded diagnostic identifier for a rejected source statement. Source
/// text is intentionally never included in diagnostics or tracing.
fn blit_statement_id(statement: &str) -> String {
    let hash = statement
        .bytes()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    format!("{} bytes, fnv1a64={hash:016x}", statement.len())
}
