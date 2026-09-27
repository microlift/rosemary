use super::*;

pub(super) fn resource_kind(tag: &str) -> Option<ResourceKind> {
    match tag {
        "air.buffer" => Some(ResourceKind::Buffer),
        "air.indirect_buffer" | "air.argument_buffer" => Some(ResourceKind::ArgumentBuffer),
        "air.texture" | "air.depth_texture" => Some(ResourceKind::Texture),
        "air.sampler" => Some(ResourceKind::Sampler),
        _ => None,
    }
}

/// `air.location_index` is followed by the Metal resource index and the
/// number of consecutive indices the argument occupies.
pub(super) fn location_index(items: &[Item<'_>]) -> Result<u32, TranslateError> {
    match keyed(items, "air.location_index") {
        [Item::Number(index), ..] => u32::try_from(*index)
            .map_err(|_| semantic("AIR location index does not fit in 32 bits")),
        _ => Err(semantic("AIR resource has no location index")),
    }
}

pub(super) fn location_count(items: &[Item<'_>]) -> Result<u32, TranslateError> {
    match keyed(items, "air.location_index") {
        [_, Item::Number(count), ..] => u32::try_from(*count)
            .map_err(|_| semantic("AIR location count does not fit in 32 bits")),
        _ => Err(semantic("AIR resource has no location count")),
    }
}

pub(super) fn access(items: &[Item<'_>]) -> Access {
    for item in items {
        match item {
            Item::Text("air.sample") => return Access::Sample,
            Item::Text("air.write") => return Access::Write,
            Item::Text("air.read_write") => return Access::ReadWrite,
            _ => {}
        }
    }
    Access::Read
}

/// Build a Metal texture type from `air.arg_type_name`, which spells the
/// complete Metal type, for instance `texture2d_array<half>` or
/// `depthcube<float>`. The LLVM type of the argument is opaque and carries
/// none of this.
pub(super) fn texture_type(items: &[Item<'_>], access: Access) -> Result<Type, TranslateError> {
    let Some(Item::Text(name)) = keyed(items, "air.arg_type_name").first() else {
        return Err(semantic("AIR texture has no argument type name"));
    };
    let (kind, texel) = match name.split_once('<') {
        Some((kind, rest)) => (
            kind,
            rest.split(',')
                .next()
                .and_then(|texel| texel.strip_suffix('>').or(Some(texel)))
                .unwrap_or("float")
                .trim(),
        ),
        None => (*name, "float"),
    };
    // Metal spells a texture's element signedness in its type, but the AIR
    // body reads texels as signless LLVM integers, so keep the storage width
    // here and record the signedness beside it.
    let signed = matches!(texel, "int" | "short");
    let texel = match texel {
        "float" => Type::Float { bits: 32 },
        "half" => Type::Float { bits: 16 },
        "int" | "uint" => Type::u32(),
        "short" | "ushort" => Type::Int {
            bits: 16,
            signed: false,
        },
        texel => {
            return Err(semantic(format!(
                "AIR texture element type {texel} needs Metal IR lowering"
            )));
        }
    };
    let depth = kind.starts_with("depth");
    let body = kind
        .strip_prefix("texture")
        .or_else(|| kind.strip_prefix("depth"))
        .ok_or_else(|| semantic(format!("AIR texture type {name} needs Metal IR lowering")))?;
    let (body, array) = match body.strip_suffix("_array") {
        Some(body) => (body, true),
        None => (body, false),
    };
    let (body, multisampled) = match body.strip_suffix("_ms") {
        Some(body) => (body, true),
        None => (body, false),
    };
    let dimension = match body {
        "1d" => TextureDimension::D1,
        "2d" => TextureDimension::D2,
        "3d" => TextureDimension::D3,
        "cube" => TextureDimension::Cube,
        _ => {
            return Err(semantic(format!(
                "AIR texture type {name} needs Metal IR lowering"
            )));
        }
    };
    Ok(Type::Texture(TextureType {
        dimension,
        array,
        multisampled,
        depth,
        texel: Box::new(texel),
        signed,
        access,
    }))
}

pub(super) fn argument_name(items: &[Item<'_>]) -> Option<String> {
    match keyed(items, "air.arg_name") {
        [Item::Text(name), ..] => Some((*name).to_string()),
        _ => None,
    }
}

/// Whether `air.arg_type_name` spells a signed integer scalar or vector,
/// such as `int`, `short2` or `char4`.
pub(super) fn signed_integer_argument(items: &[Item<'_>]) -> bool {
    let [Item::Text(name), ..] = keyed(items, "air.arg_type_name") else {
        return false;
    };
    let name = name.strip_prefix("packed_").unwrap_or(name);
    matches!(
        name.trim_end_matches(|c: char| c.is_ascii_digit()),
        "int" | "short" | "char" | "long"
    )
}

/// The items following a string key, up to the end of the descriptor.
fn keyed<'a, 'b>(items: &'a [Item<'b>], key: &str) -> &'a [Item<'b>] {
    items
        .iter()
        .position(|item| matches!(item, Item::Text(text) if *text == key))
        .map(|position| &items[position + 1..])
        .unwrap_or_default()
}

/// `user(locnN)` names a Metal user attribute location.
pub(super) fn user_location(items: &[Item<'_>]) -> Option<u32> {
    items.iter().find_map(|item| match item {
        Item::Text(text) => text
            .strip_prefix("user(locn")
            .and_then(|rest| rest.strip_suffix(')'))
            .and_then(|digits| digits.parse().ok()),
        Item::Number(_) => None,
    })
}

pub(super) fn interpolation(items: &[Item<'_>]) -> Option<Interpolation> {
    let mut sampling = None;
    let mut perspective = None;
    let mut flat = false;
    for item in items {
        match item {
            Item::Text("air.center") => sampling = Some(Sampling::Center),
            Item::Text("air.centroid") => sampling = Some(Sampling::Centroid),
            Item::Text("air.sample") => sampling = Some(Sampling::Sample),
            Item::Text("air.perspective") => perspective = Some(true),
            Item::Text("air.no_perspective") => perspective = Some(false),
            Item::Text("air.flat") => flat = true,
            _ => {}
        }
    }
    (sampling.is_some() || perspective.is_some() || flat).then(|| Interpolation {
        sampling: sampling.unwrap_or(Sampling::Center),
        perspective: perspective.unwrap_or(true),
        flat,
    })
}

pub(super) fn builtin(tag: &str) -> Option<Builtin> {
    Some(match tag {
        "air.position" => Builtin::Position,
        "air.point_size" => Builtin::PointSize,
        "air.clip_distance" => Builtin::ClipDistance,
        "air.vertex_id" => Builtin::VertexId,
        "air.instance_id" => Builtin::InstanceId,
        "air.base_vertex" => Builtin::BaseVertex,
        "air.base_instance" => Builtin::BaseInstance,
        "air.front_facing" => Builtin::FrontFacing,
        "air.point_coord" => Builtin::PointCoord,
        "air.sample_id" => Builtin::SampleId,
        "air.sample_mask" => Builtin::SampleMask,
        "air.depth" => Builtin::Depth,
        "air.render_target_array_index" => Builtin::RenderTargetArrayIndex,
        "air.viewport_array_index" => Builtin::ViewportArrayIndex,
        "air.thread_position_in_grid" => Builtin::ThreadPositionInGrid,
        "air.thread_position_in_threadgroup" => Builtin::ThreadPositionInThreadgroup,
        "air.threadgroup_position_in_grid" => Builtin::ThreadgroupPositionInGrid,
        "air.threadgroups_per_grid" => Builtin::ThreadgroupsPerGrid,
        "air.thread_index_in_threadgroup" => Builtin::ThreadIndexInThreadgroup,
        "air.threads_per_threadgroup" => Builtin::ThreadsPerThreadgroup,
        _ => return None,
    })
}

pub(super) fn input_binding(tag: &str, rest: &[Item<'_>]) -> Result<StageBinding, TranslateError> {
    match tag {
        "air.vertex_input" => Ok(StageBinding::Location {
            location: location_index(rest)?,
            interpolation: None,
        }),
        "air.fragment_input" => Ok(StageBinding::Location {
            location: user_location(rest)
                .ok_or_else(|| semantic("AIR fragment input has no user location"))?,
            interpolation: interpolation(rest),
        }),
        tag => builtin(tag)
            .map(StageBinding::Builtin)
            .ok_or_else(|| semantic(format!("AIR stage input {tag} needs Metal IR lowering"))),
    }
}

pub(super) fn output_binding(items: &[Item<'_>]) -> Result<StageBinding, TranslateError> {
    let Some(Item::Text(tag)) = items.first() else {
        return Err(semantic("AIR stage output has no kind"));
    };
    match *tag {
        "air.render_target" => match items.get(1) {
            Some(Item::Number(index)) => Ok(StageBinding::RenderTarget {
                index: u32::try_from(*index)
                    .map_err(|_| semantic("AIR render target index does not fit in 32 bits"))?,
            }),
            _ => Err(semantic("AIR render target has no index")),
        },
        "air.vertex_output" => Ok(StageBinding::Location {
            location: user_location(&items[1..])
                .ok_or_else(|| semantic("AIR vertex output has no user location"))?,
            interpolation: None,
        }),
        tag => builtin(tag)
            .map(StageBinding::Builtin)
            .ok_or_else(|| semantic(format!("AIR stage output {tag} needs Metal IR lowering"))),
    }
}
