use super::*;

pub(super) fn parse_resource_attribute(
    attribute: &str,
    declaration: &str,
    ty: &Type,
) -> Option<(ResourceKind, u32, Access)> {
    let (kind, index) = if let Some(index) = attribute.strip_prefix("buffer(") {
        (ResourceKind::Buffer, index.strip_suffix(')')?)
    } else {
        let index = attribute.strip_prefix("texture(")?;
        (ResourceKind::Texture, index.strip_suffix(')')?)
    };
    let index = index.trim().parse().ok()?;
    let access = match ty {
        Type::Pointer {
            space: AddressSpace::Constant,
            ..
        } => Access::Read,
        Type::Texture(_) if declaration.contains("access::write") => Access::Write,
        Type::Texture(_) => Access::Read,
        _ => Access::ReadWrite,
    };
    Some((kind, index, access))
}

/// The texture shapes this source-MSL frontend accepts: a writable,
/// non-arrayed float texture of two or three dimensions, which is what the
/// runtime's own upload blits write into.
pub(super) fn writable_float_texture(dimension: TextureDimension) -> Type {
    Type::Texture(TextureType {
        dimension,
        array: false,
        multisampled: false,
        depth: false,
        texel: Box::new(Type::Float { bits: 32 }),
        signed: false,
        access: Access::Write,
    })
}

pub(super) fn parse_parameter_type(
    declaration: &str,
    name: &str,
    structs: &HashMap<String, Type>,
) -> Result<Type, String> {
    let ty = declaration
        .strip_suffix(name)
        .ok_or_else(|| "MSL parameter declaration is malformed".to_string())?
        .trim();
    if let Some(pointee) = ty
        .strip_prefix("device ")
        .and_then(|ty| ty.strip_suffix('*'))
    {
        return Ok(Type::Pointer {
            space: AddressSpace::Device,
            pointee: Box::new(parse_value_type(pointee.trim(), structs)?),
        });
    }
    if let Some(pointee) = ty
        .strip_prefix("constant ")
        .and_then(|ty| ty.strip_suffix('&'))
    {
        return Ok(Type::Pointer {
            space: AddressSpace::Constant,
            pointee: Box::new(parse_value_type(pointee.trim(), structs)?),
        });
    }
    if ty.contains("access::write") {
        if ty.starts_with("texture3d<float,") {
            return Ok(writable_float_texture(TextureDimension::D3));
        }
        if ty.starts_with("texture2d<float,") {
            return Ok(writable_float_texture(TextureDimension::D2));
        }
    }
    if ty.starts_with("texture") {
        return Err("unsupported MSL texture type".to_string());
    }
    parse_value_type(ty, structs)
}

pub(super) fn parse_structs(source: &str) -> Result<HashMap<String, Type>, TranslateError> {
    let mut structs = HashMap::new();
    let mut remaining = source;
    while let Some(start) = remaining.find("struct ") {
        remaining = &remaining[start + "struct ".len()..];
        let location = source.len() - remaining.len();
        let name_end = remaining
            .find(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .ok_or_else(|| syntax_at(source, location, "MSL struct has no body"))?;
        let name = remaining[..name_end].trim();
        let open = remaining[name_end..]
            .find('{')
            .map(|offset| name_end + offset)
            .ok_or_else(|| syntax_at(source, location, "MSL struct has no body"))?;
        let close = matching(remaining, open, '{', '}').ok_or_else(|| {
            syntax_at(
                source,
                location + open,
                "MSL struct has an unterminated body",
            )
        })?;
        let mut fields = Vec::new();
        let mut offset = 0;
        let mut alignment = 1;
        for field in remaining[open + 1..close]
            .split(';')
            .map(str::trim)
            .filter(|field| !field.is_empty())
        {
            let mut tokens = field.split_whitespace();
            let ty = tokens.next().ok_or_else(|| {
                syntax_at(source, location + open, "MSL struct field has no type")
            })?;
            let field_name = tokens.next().ok_or_else(|| {
                syntax_at(source, location + open, "MSL struct field has no name")
            })?;
            if tokens.next().is_some() {
                return Err(syntax_at(
                    source,
                    location + open,
                    "unsupported MSL struct field declaration",
                ));
            }
            let ty = parse_value_type(ty, &structs)
                .map_err(|message| syntax_at(source, location + open, message))?;
            let (size, field_alignment) = type_layout(&ty).ok_or_else(|| {
                syntax_at(source, location + open, "unsupported MSL struct field type")
            })?;
            offset = align_to(offset, field_alignment);
            alignment = alignment.max(field_alignment);
            fields.push(StructField {
                name: Some(field_name.to_string()),
                ty,
                offset,
            });
            offset += size;
        }
        let size = align_to(offset, alignment);
        structs.insert(
            name.to_string(),
            Type::Struct(StructType {
                fields,
                size,
                alignment,
                packed: false,
            }),
        );
        remaining = &remaining[close + 1..];
    }
    Ok(structs)
}

pub(super) fn parse_value_type(
    name: &str,
    structs: &HashMap<String, Type>,
) -> Result<Type, String> {
    if let Some(ty) = structs.get(name) {
        return Ok(ty.clone());
    }
    let base_end = name
        .trim_end_matches(|character: char| character.is_ascii_digit())
        .len();
    let (base, suffix) = name.split_at(base_end);
    let lanes = (!suffix.is_empty())
        .then(|| suffix.parse::<u8>())
        .transpose()
        .map_err(|_| "invalid MSL vector width")?;
    let scalar = match base {
        "uint" => Type::u32(),
        "int" => Type::i32(),
        "uchar" => Type::Int {
            bits: 8,
            signed: false,
        },
        "float" => Type::Float { bits: 32 },
        _ => return Err("unsupported MSL parameter type".to_string()),
    };
    Ok(lanes.map_or(scalar.clone(), |lanes| Type::Vector {
        element: Box::new(scalar),
        lanes,
    }))
}

pub(super) fn type_layout(ty: &Type) -> Option<(u32, u32)> {
    match ty {
        Type::Int { bits, .. } | Type::Float { bits } => {
            Some((u32::from(*bits) / 8, u32::from(*bits) / 8))
        }
        Type::Vector { element, lanes } => {
            let (size, alignment) = type_layout(element)?;
            Some((
                size * u32::from(*lanes),
                if *lanes == 3 {
                    alignment * 4
                } else {
                    alignment * u32::from(*lanes)
                },
            ))
        }
        _ => None,
    }
}

pub(super) fn align_to(value: u32, alignment: u32) -> u32 {
    value.next_multiple_of(alignment)
}

pub(super) fn split_parameters(parameters: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut angle_depth = 0_u32;
    for (offset, character) in parameters.char_indices() {
        match character {
            '<' => angle_depth += 1,
            '>' => angle_depth = angle_depth.saturating_sub(1),
            ',' if angle_depth == 0 => {
                let part = parameters[start..offset].trim();
                if !part.is_empty() {
                    parts.push(part);
                }
                start = offset + character.len_utf8();
            }
            _ => {}
        }
    }
    let part = parameters[start..].trim();
    if !part.is_empty() {
        parts.push(part);
    }
    parts
}
