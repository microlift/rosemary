use super::*;

pub(super) fn encode(key: &[u8; 32], library: &TranslatedLibrary) -> Vec<u8> {
    let mut payload = Vec::new();
    write_u32(&mut payload, library.entries.len() as u32);
    for entry in &library.entries {
        write_string(&mut payload, &entry.name);
        payload.push(stage_tag(entry.stage));
        write_u32(&mut payload, entry.words.len() as u32);
        for word in &entry.words {
            write_u32(&mut payload, *word);
        }
        write_u32(&mut payload, entry.resources.len() as u32);
        for resource in &entry.resources {
            payload.push(stage_tag(resource.stage));
            payload.push(resource_kind_tag(resource.kind));
            write_u32(&mut payload, resource.metal_index);
            write_u32(&mut payload, resource.descriptor_set);
            write_u32(&mut payload, resource.binding);
            write_u32(&mut payload, resource.count);
            payload.push(access_tag(resource.access));
            payload.push(u8::from(resource.sampled));
            payload.push(u8::from(resource.texel_buffer));
            write_inline_sampler(&mut payload, resource.inline_sampler.as_ref());
        }
        write_strings(&mut payload, &entry.required.capabilities);
        write_strings(&mut payload, &entry.required.extensions);
        for variables in [&entry.vertex_inputs, &entry.varyings] {
            write_u32(&mut payload, variables.len() as u32);
            for variable in variables {
                write_string(&mut payload, &variable.name);
                write_u32(&mut payload, variable.location);
                payload.extend_from_slice(&variable.data_type.to_le_bytes());
            }
        }
    }
    write_u32(&mut payload, library.function_constants.len() as u32);
    for constant in &library.function_constants {
        write_u32(&mut payload, constant.index);
        write_string(&mut payload, &constant.name);
        write_string(&mut payload, &constant.encoding);
    }
    write_u32(&mut payload, library.rejected.len() as u32);
    for rejected in &library.rejected {
        write_string(&mut payload, &rejected.name);
        write_string(&mut payload, &rejected.reason);
    }

    let mut bytes = Vec::with_capacity(payload.len() + 112);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&sha256::digest(CACHE_VERSION.as_bytes()));
    bytes.extend_from_slice(key);
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&sha256::digest(&payload));
    bytes.extend_from_slice(&payload);
    bytes
}

pub(super) fn decode(bytes: &[u8], key: &[u8; 32]) -> Option<TranslatedLibrary> {
    let mut reader = Reader::new(bytes);
    if reader.take(MAGIC.len())? != MAGIC {
        return None;
    }
    // The key already covers the crate version, so this only catches a file
    // moved between version directories by hand.
    if reader.take(32)? != sha256::digest(CACHE_VERSION.as_bytes()) {
        return None;
    }
    if reader.take(32)? != key {
        return None;
    }
    let length = usize::try_from(reader.u64()?).ok()?;
    let checksum = reader.take(32)?.to_vec();
    let payload = reader.take(length)?;
    if !reader.at_end() || sha256::digest(payload) != checksum.as_slice() {
        return None;
    }

    let mut reader = Reader::new(payload);
    let count = reader.u32()? as usize;
    let mut entries = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let name = reader.string()?;
        let stage = stage_of_tag(reader.u8()?)?;
        let word_count = reader.u32()? as usize;
        let mut words = Vec::with_capacity(word_count.min(1 << 16));
        for _ in 0..word_count {
            words.push(reader.u32()?);
        }
        let resource_count = reader.u32()? as usize;
        let mut resources = Vec::with_capacity(resource_count.min(1024));
        for _ in 0..resource_count {
            resources.push(ResourceBinding {
                stage: stage_of_tag(reader.u8()?)?,
                kind: resource_kind_of_tag(reader.u8()?)?,
                metal_index: reader.u32()?,
                descriptor_set: reader.u32()?,
                binding: reader.u32()?,
                count: reader.u32()?,
                access: access_of_tag(reader.u8()?)?,
                sampled: reader.boolean()?,
                texel_buffer: reader.boolean()?,
                inline_sampler: read_inline_sampler(&mut reader)?,
            });
        }
        let required = RequiredFeatures {
            capabilities: reader.strings()?,
            extensions: reader.strings()?,
        };
        let mut lists = [Vec::new(), Vec::new()];
        for list in &mut lists {
            let count = reader.u32()? as usize;
            for _ in 0..count.min(4096) {
                list.push(StageVariable {
                    name: reader.string()?,
                    location: reader.u32()?,
                    data_type: reader.u64()?,
                });
            }
        }
        let [vertex_inputs, varyings] = lists;
        entries.push(EntryArtifact {
            name,
            stage,
            words,
            resources,
            required,
            vertex_inputs,
            varyings,
        });
    }
    let count = reader.u32()? as usize;
    let mut function_constants = Vec::with_capacity(count.min(256));
    for _ in 0..count {
        function_constants.push(FunctionConstantInfo {
            index: reader.u32()?,
            name: reader.string()?,
            encoding: reader.string()?,
        });
    }
    let count = reader.u32()? as usize;
    let mut rejected = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        rejected.push(crate::RejectedEntry {
            name: reader.string()?,
            reason: reader.string()?,
        });
    }
    reader.at_end().then_some(TranslatedLibrary {
        entries,
        function_constants,
        rejected,
    })
}

fn write_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_string(bytes: &mut Vec<u8>, text: &str) {
    write_u32(bytes, text.len() as u32);
    bytes.extend_from_slice(text.as_bytes());
}

fn write_strings(bytes: &mut Vec<u8>, texts: &[String]) {
    write_u32(bytes, texts.len() as u32);
    for text in texts {
        write_string(bytes, text);
    }
}

/// A bounds-checked reader: a corrupt entry answers `None` and is discarded,
/// and no length in the file is ever trusted enough to index with.
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(length)?;
        let slice = self.bytes.get(self.offset..end)?;
        self.offset = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn boolean(&mut self) -> Option<bool> {
        match self.u8()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    fn string(&mut self) -> Option<String> {
        let length = self.u32()? as usize;
        std::str::from_utf8(self.take(length)?)
            .ok()
            .map(str::to_owned)
    }

    fn strings(&mut self) -> Option<Vec<String>> {
        let count = self.u32()? as usize;
        let mut texts = Vec::with_capacity(count.min(256));
        for _ in 0..count {
            texts.push(self.string()?);
        }
        Some(texts)
    }

    fn at_end(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn stage_tag(stage: Stage) -> u8 {
    match stage {
        Stage::Vertex => 0,
        Stage::Fragment => 1,
        Stage::Kernel => 2,
        Stage::Visible => 3,
        Stage::Intersection => 4,
        Stage::Mesh => 5,
        Stage::Object => 6,
        Stage::Tile => 7,
    }
}

fn stage_of_tag(tag: u8) -> Option<Stage> {
    Some(match tag {
        0 => Stage::Vertex,
        1 => Stage::Fragment,
        2 => Stage::Kernel,
        3 => Stage::Visible,
        4 => Stage::Intersection,
        5 => Stage::Mesh,
        6 => Stage::Object,
        7 => Stage::Tile,
        _ => return None,
    })
}

fn resource_kind_tag(kind: ResourceKind) -> u8 {
    match kind {
        ResourceKind::Buffer => 0,
        ResourceKind::Texture => 1,
        ResourceKind::Sampler => 2,
        ResourceKind::AccelerationStructure => 3,
        ResourceKind::VisibleFunctionTable => 4,
        ResourceKind::IntersectionFunctionTable => 5,
        ResourceKind::ArgumentBuffer => 6,
        ResourceKind::Imageblock => 7,
    }
}

fn resource_kind_of_tag(tag: u8) -> Option<ResourceKind> {
    Some(match tag {
        0 => ResourceKind::Buffer,
        1 => ResourceKind::Texture,
        2 => ResourceKind::Sampler,
        3 => ResourceKind::AccelerationStructure,
        4 => ResourceKind::VisibleFunctionTable,
        5 => ResourceKind::IntersectionFunctionTable,
        6 => ResourceKind::ArgumentBuffer,
        7 => ResourceKind::Imageblock,
        _ => return None,
    })
}

fn access_tag(access: Access) -> u8 {
    match access {
        Access::Sample => 0,
        Access::Read => 1,
        Access::Write => 2,
        Access::ReadWrite => 3,
    }
}

fn access_of_tag(tag: u8) -> Option<Access> {
    Some(match tag {
        0 => Access::Sample,
        1 => Access::Read,
        2 => Access::Write,
        3 => Access::ReadWrite,
        _ => return None,
    })
}

fn write_inline_sampler(payload: &mut Vec<u8>, sampler: Option<&crate::ir::InlineSampler>) {
    let Some(sampler) = sampler else {
        payload.push(0);
        return;
    };
    payload.push(1);
    payload.extend_from_slice(&[
        sampler.min_filter,
        sampler.mag_filter,
        sampler.mip_filter,
        sampler.address[0],
        sampler.address[1],
        sampler.address[2],
        sampler.max_anisotropy,
        sampler.compare_function,
        sampler.border_color,
        u8::from(sampler.normalized_coordinates),
    ]);
    write_u32(payload, sampler.lod_min_clamp);
    write_u32(payload, sampler.lod_max_clamp);
}

fn read_inline_sampler(reader: &mut Reader<'_>) -> Option<Option<crate::ir::InlineSampler>> {
    if !reader.boolean()? {
        return Some(None);
    }
    let b = reader.take(10)?;
    Some(Some(crate::ir::InlineSampler {
        min_filter: b[0],
        mag_filter: b[1],
        mip_filter: b[2],
        address: [b[3], b[4], b[5]],
        max_anisotropy: b[6],
        compare_function: b[7],
        border_color: b[8],
        normalized_coordinates: b[9] != 0,
        lod_min_clamp: reader.u32()?,
        lod_max_clamp: reader.u32()?,
    }))
}
