mod cfg;
mod emit;
mod structurize;
mod widen;

use cfg::{Merge, control_flow_merges};
use emit::Emitter;
use structurize::{
    hoist_phis, isolate_loop_merges, isolate_shared_selection_paths, limit, normalize_loop_headers,
    reverse_post_order, separate_loop_latches, single_exit, split_shared_merges,
};

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rspirv::binary::Assemble;
use rspirv::dr::{Builder, InsertPoint, Operand as SpirvOperand};
use rspirv::spirv::{
    AddressingModel, BuiltIn, Capability, Decoration, Dim, ExecutionMode, ExecutionModel,
    FunctionControl, GlslStd450Op, ImageFormat, ImageOperands, LoopControl, MemoryModel,
    MemorySemantics, SelectionControl, StorageClass,
};

use crate::ir::{
    Access, AddressSpace, AtomicOp, BarrierMemory, BarrierScope, BlockId, Builtin, ConvertOp,
    EntryPoint, FloatBinaryOp, FloatPredicate, FloatUnaryOp, Function, InstructionKind,
    IntegerBinaryOp, IntegerPredicate, IntegerUnaryOp, MemoryOrder, Module, Operand, ResourceKind,
    Sampling, Stage, StageBinding, Terminator, TextureDimension, Type, ValueId, coordinate_lanes,
    instruction_operands, instruction_operands_mut, is_boolean_or_boolean_vector, operand_type,
    terminator_operands,
};
use crate::{Diagnostic, Phase, TranslateError};

/// Vulkan and SPIR-V capabilities available to an emitter invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetProfile {
    pub vulkan_version: (u8, u8),
    pub spirv_version: (u8, u8),
    pub max_bound_descriptor_sets: u32,
    pub shader: bool,
    pub shader_int8: bool,
    pub shader_int16: bool,
    pub shader_int64: bool,
    pub shader_float16: bool,
    pub storage_buffer8_bit_access: bool,
    pub uniform_and_storage_buffer8_bit_access: bool,
    pub storage_push_constant8: bool,
    pub storage_buffer16_bit_access: bool,
    pub uniform_and_storage_buffer16_bit_access: bool,
    pub storage_push_constant16: bool,
    pub storage_input_output16: bool,
    pub shader_image_gather_extended: bool,
    pub shader_storage_image_multisample: bool,
    pub image_cube_array: bool,
    pub shader_storage_image_read_without_format: bool,
    pub shader_storage_image_write_without_format: bool,
    pub shader_clip_distance: bool,
    pub shader_draw_parameters: bool,
    pub sample_rate_shading: bool,
    pub geometry_shader: bool,
    pub multi_viewport: bool,
    pub demote_to_helper_invocation: bool,
    /// Vulkan 1.1 subgroup operations support `GroupNonUniform` when the
    /// physical device reports the basic operation for the shader stage.
    pub group_non_uniform: bool,
    /// Core Vulkan shader capabilities which have no optional feature bit.
    pub image_1d: bool,
    pub image_query: bool,
    /// `VkPhysicalDeviceVariablePointersFeatures::variablePointersStorageBuffer`,
    /// which a pointer into a buffer needs before it can be a phi or a select.
    pub variable_pointers_storage_buffer: bool,
    /// `variablePointers`, which widens the above to `Workgroup` memory - a
    /// pointer into a Metal `threadgroup` array.
    pub variable_pointers: bool,
}

impl TargetProfile {
    pub fn vulkan_1_3() -> Self {
        Self {
            vulkan_version: (1, 3),
            spirv_version: (1, 6),
            max_bound_descriptor_sets: 4,
            shader: true,
            shader_int8: false,
            shader_int16: false,
            shader_int64: false,
            shader_float16: false,
            storage_buffer8_bit_access: false,
            uniform_and_storage_buffer8_bit_access: false,
            storage_push_constant8: false,
            storage_buffer16_bit_access: false,
            uniform_and_storage_buffer16_bit_access: false,
            storage_push_constant16: false,
            storage_input_output16: false,
            shader_image_gather_extended: false,
            shader_storage_image_multisample: false,
            image_cube_array: false,
            shader_storage_image_read_without_format: false,
            shader_storage_image_write_without_format: false,
            shader_clip_distance: false,
            shader_draw_parameters: false,
            sample_rate_shading: false,
            geometry_shader: false,
            multi_viewport: false,
            demote_to_helper_invocation: false,
            group_non_uniform: false,
            image_1d: true,
            image_query: true,
            variable_pointers_storage_buffer: false,
            variable_pointers: false,
        }
    }

    pub fn supports_capability(&self, capability: &str) -> bool {
        match capability {
            "Shader" => self.shader,
            "Int8" => self.shader_int8,
            "Int16" => self.shader_int16,
            "Int64" => self.shader_int64,
            "Float16" => self.shader_float16,
            "StorageBuffer8BitAccess" => self.storage_buffer8_bit_access,
            "UniformAndStorageBuffer8BitAccess" => self.uniform_and_storage_buffer8_bit_access,
            "StoragePushConstant8" => self.storage_push_constant8,
            "StorageBuffer16BitAccess" => self.storage_buffer16_bit_access,
            "UniformAndStorageBuffer16BitAccess" => self.uniform_and_storage_buffer16_bit_access,
            "StoragePushConstant16" => self.storage_push_constant16,
            "StorageInputOutput16" => self.storage_input_output16,
            "ImageGatherExtended" => self.shader_image_gather_extended,
            "StorageImageMultisample" => self.shader_storage_image_multisample,
            "SampledCubeArray" => self.image_cube_array,
            "StorageImageReadWithoutFormat" => self.shader_storage_image_read_without_format,
            "StorageImageWriteWithoutFormat" => self.shader_storage_image_write_without_format,
            "ClipDistance" => self.shader_clip_distance,
            "DrawParameters" => self.shader_draw_parameters,
            "SampleRateShading" => self.sample_rate_shading,
            "Geometry" => self.geometry_shader,
            "MultiViewport" => self.multi_viewport,
            "DemoteToHelperInvocation" => self.demote_to_helper_invocation,
            "GroupNonUniform" => self.group_non_uniform,
            "Image1D" => self.image_1d,
            // Every Vulkan implementation supports texel buffers.
            "SampledBuffer" | "ImageBuffer" => self.shader,
            "ImageQuery" => self.image_query,
            "VariablePointersStorageBuffer" => {
                self.variable_pointers_storage_buffer || self.variable_pointers
            }
            "VariablePointers" => self.variable_pointers,
            _ => false,
        }
    }

    pub fn supports_extension(&self, extension: &str) -> bool {
        match extension {
            "SPV_KHR_variable_pointers" => {
                self.variable_pointers_storage_buffer || self.variable_pointers
            }
            "SPV_EXT_demote_to_helper_invocation" => self.demote_to_helper_invocation,
            "SPV_KHR_8bit_storage" => {
                self.storage_buffer8_bit_access
                    || self.uniform_and_storage_buffer8_bit_access
                    || self.storage_push_constant8
            }
            _ => false,
        }
    }

    pub fn unsupported_requirement<'a>(&self, required: &'a RequiredFeatures) -> Option<&'a str> {
        required
            .capabilities
            .iter()
            .find(|capability| !self.supports_capability(capability))
            .map(String::as_str)
            .or_else(|| {
                required
                    .extensions
                    .iter()
                    .find(|extension| !self.supports_extension(extension))
                    .map(String::as_str)
            })
    }
}

impl Default for TargetProfile {
    fn default() -> Self {
        Self::vulkan_1_3()
    }
}

/// The specialization constants a compute entry point's threadgroup size uses.
///
/// The runtime sets these when it creates the Vulkan pipeline for a Metal
/// dispatch, because Metal names the threadgroup size at dispatch time.
pub const WORKGROUP_SIZE_SPEC_IDS: [u32; 3] = [0, 1, 2];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequiredFeatures {
    pub capabilities: Vec<String>,
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceBinding {
    pub stage: Stage,
    pub kind: ResourceKind,
    pub metal_index: u32,
    pub descriptor_set: u32,
    pub binding: u32,
    pub count: u32,
    pub access: Access,
    /// For a texture, whether Vulkan carries it as a sampled image rather than
    /// a storage image. See [`ir::TextureType::sampled_image`]; a buffer or a
    /// sampler leaves it false.
    pub sampled: bool,
    /// For a texture, whether it is a `texture_buffer`: a Vulkan texel
    /// buffer, uniform where `sampled` and storage otherwise.
    pub texel_buffer: bool,
    /// Set for a sampler the shader describes itself, which the runtime binds.
    pub inline_sampler: Option<crate::ir::InlineSampler>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryArtifact {
    pub name: String,
    pub stage: Stage,
    pub words: Vec<u32>,
    pub resources: Vec<ResourceBinding>,
    pub required: RequiredFeatures,
    /// A vertex function's attributes, for `-[MTLFunction vertexAttributes]`.
    pub vertex_inputs: Vec<StageVariable>,
    /// The user varyings a vertex function writes or a fragment function
    /// reads, by the `user(NAME)` Metal links them with. Only source
    /// libraries name them; the runtime renumbers a fragment function's
    /// locations to its vertex function's by these names.
    pub varyings: Vec<StageVariable>,
}

/// One stage interface variable, as reflection reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StageVariable {
    pub name: String,
    pub location: u32,
    /// Its `MTLDataType`.
    pub data_type: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranslatedLibrary {
    pub entries: Vec<EntryArtifact>,
    /// The library's `[[function_constant]]` declarations. A library with
    /// any has to be translated again for each set of values a function is
    /// made with.
    pub function_constants: Vec<FunctionConstantInfo>,
    /// Entry points the source declares which could not be translated, and
    /// why: the rest of the library is usable, and asking for one of these
    /// reports its reason.
    pub rejected: Vec<RejectedEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectedEntry {
    pub name: String,
    pub reason: String,
}

/// One function constant a library declares, as reflection reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionConstantInfo {
    pub index: u32,
    pub name: String,
    /// The Itanium spelling of its Metal type.
    pub encoding: String,
}

pub fn emit_spirv(
    module: &Module,
    target: &TargetProfile,
) -> Result<TranslatedLibrary, TranslateError> {
    module.verify()?;
    validate_target(target)?;
    if !module.static_initializers.is_empty() {
        return Err(emission(
            "the module's static initializers have not been run; specialize it first",
        ));
    }
    let widened;
    let module = match widen::widen(module, target) {
        Some(module) => {
            module.verify()?;
            widened = module;
            &widened
        }
        None => module,
    };
    let mut entries = Vec::new();
    let mut rejected = Vec::new();
    let mut first_error = None;
    for entry in &module.entries {
        match emit_entry(module, entry, target) {
            Ok(artifact) => entries.push(artifact),
            Err(error) => {
                rejected.push(RejectedEntry {
                    name: module.functions[entry.function].name.clone(),
                    reason: error.to_string(),
                });
                first_error.get_or_insert(error);
            }
        }
    }
    // A library none of whose entry points translate is a failed library.
    if entries.is_empty()
        && let Some(error) = first_error
    {
        return Err(error);
    }
    let function_constants = module
        .globals
        .iter()
        .filter_map(|global| global.function_constant.as_ref())
        .map(|slot| FunctionConstantInfo {
            index: slot.index,
            name: slot.name.clone(),
            encoding: slot.encoding.clone(),
        })
        .collect();
    Ok(TranslatedLibrary {
        entries,
        function_constants,
        rejected,
    })
}

fn validate_target(target: &TargetProfile) -> Result<(), TranslateError> {
    if !target.shader {
        return Err(emission(
            "target profile does not support the Shader capability",
        ));
    }
    if target.max_bound_descriptor_sets < 4 {
        return Err(emission(
            "Metal descriptor ABI requires four bound descriptor sets",
        ));
    }
    if target.vulkan_version < (1, 1) {
        return Err(emission(
            "the initial descriptor ABI requires at least Vulkan 1.1",
        ));
    }
    if target.spirv_version.0 != 1 || target.spirv_version.1 > 6 {
        return Err(emission("unsupported SPIR-V target version"));
    }
    if target.spirv_version < (1, 3) {
        return Err(emission(
            "the initial emitter requires SPIR-V 1.3 for StorageBuffer resources",
        ));
    }
    Ok(())
}

fn emit_entry(
    module: &Module,
    entry: &EntryPoint,
    target: &TargetProfile,
) -> Result<EntryArtifact, TranslateError> {
    if !matches!(entry.stage, Stage::Kernel | Stage::Fragment | Stage::Vertex) {
        return Err(emission(
            "the emitter only supports kernel, vertex and fragment entry points",
        ));
    }
    let function = module
        .functions
        .get(entry.function)
        .ok_or_else(|| emission("entry point refers to a missing function"))?;
    let mut function = single_exit(function.clone());
    // Each restructuring pass can expose work for the others: separating a
    // loop's merge from the code outside it leaves the header choosing between
    // two body paths, which is what normalizing headers answers, and that in
    // turn adds a block another construct may claim. Run them to a fixed
    // point rather than once each, and treat one that never settles as the
    // rejection it is.
    // Node splitting can expose several nested shared paths one at a time.
    // Leave generous linear headroom while still rejecting a pass cycle.
    let rounds = limit(&function).saturating_mul(8);
    let mut settled = false;
    for _ in 0..rounds {
        let mut changed = normalize_loop_headers(&mut function)?;
        changed |= separate_loop_latches(&mut function)?;
        changed |= isolate_loop_merges(&mut function)?;
        changed |= isolate_shared_selection_paths(&mut function)?;
        changed |= split_shared_merges(&mut function)?;
        if !changed {
            settled = true;
            break;
        }
    }
    if !settled {
        return Err(emission(
            "the control-flow restructuring passes never reached a fixed point",
        ));
    }
    hoist_phis(&mut function);
    reverse_post_order(&mut function);
    let function = &function;
    let mut emitter = Emitter::new(target);
    emitter.declare_globals(module)?;
    let resources = emitter.declare_resources(entry, function)?;
    emitter.declare_stage_io(entry, function)?;
    let function_id = emitter.emit_function(entry, function)?;
    let execution_model = match entry.stage {
        Stage::Kernel => ExecutionModel::GLCompute,
        Stage::Vertex => ExecutionModel::Vertex,
        Stage::Fragment => ExecutionModel::Fragment,
        _ => unreachable!(),
    };
    emitter.builder.entry_point(
        execution_model,
        function_id,
        &function.name,
        emitter.interface.clone(),
    );
    match entry.stage {
        Stage::Kernel => {
            // Metal has no threadgroup size in the shader: the host names it
            // at dispatch. SPIR-V needs one at compile time, so the size is
            // three specialization constants which the runtime supplies when
            // it creates the Vulkan pipeline for a dispatch.
            emitter.declare_workgroup_size()?;
            emitter
                .builder
                .execution_mode(function_id, ExecutionMode::LocalSize, [1, 1, 1])
        }
        Stage::Fragment => {
            emitter
                .builder
                .execution_mode(function_id, ExecutionMode::OriginUpperLeft, [])
        }
        // A vertex entry point declares no execution mode.
        _ => {}
    }
    if emitter.writes_depth {
        emitter
            .builder
            .execution_mode(function_id, ExecutionMode::DepthReplacing, []);
    }
    // Reflection is read from the completed module, so every declaration made
    // by any emitter path is reported without a second feature list to update.
    let module = emitter.builder.module();
    let capabilities = module
        .capabilities
        .iter()
        .filter_map(|instruction| match instruction.operands.first() {
            Some(SpirvOperand::Capability(capability)) => Some(format!("{capability:?}")),
            _ => None,
        })
        .collect();
    let extensions = module
        .extensions
        .iter()
        .filter_map(|instruction| match instruction.operands.first() {
            Some(SpirvOperand::LiteralString(extension)) => Some(extension.clone()),
            _ => None,
        })
        .collect();
    let words = module.assemble();
    rspirv::dr::load_words(&words)
        .map_err(|error| emission(format!("emitted SPIR-V could not be parsed: {error}")))?;
    let (vertex_inputs, varyings) = interface_reflection(entry, function);
    Ok(EntryArtifact {
        name: function.name.clone(),
        stage: entry.stage,
        words,
        resources,
        required: RequiredFeatures {
            capabilities,
            extensions,
        },
        vertex_inputs,
        varyings,
    })
}

/// The attributes a vertex function reads, and the user varyings either
/// stage names.
fn interface_reflection(
    entry: &EntryPoint,
    function: &Function,
) -> (Vec<StageVariable>, Vec<StageVariable>) {
    let mut vertex_inputs = Vec::new();
    let mut varyings = Vec::new();
    for input in &entry.inputs {
        let StageBinding::Location { location, .. } = input.binding else {
            continue;
        };
        let Some(parameter) = function.parameters.get(input.argument as usize) else {
            continue;
        };
        let variable = StageVariable {
            name: input.name.clone().unwrap_or_default(),
            location,
            data_type: metal_data_type(&parameter.ty, input.signed),
        };
        match entry.stage {
            Stage::Vertex => vertex_inputs.push(variable),
            _ if variable.name.starts_with("user(") => varyings.push(variable),
            _ => {}
        }
    }
    if entry.stage == Stage::Vertex {
        for output in &entry.outputs {
            if let (StageBinding::Location { location, .. }, Some(name)) =
                (output.binding, &output.name)
                && name.starts_with("user(")
            {
                varyings.push(StageVariable {
                    name: name.clone(),
                    location,
                    data_type: 0,
                });
            }
        }
    }
    (vertex_inputs, varyings)
}

/// The `MTLDataType` of a scalar or vector stage value.
fn metal_data_type(ty: &Type, signed: bool) -> u64 {
    let (element, lanes) = match ty {
        Type::Vector { element, lanes } => (element.as_ref(), u64::from(*lanes)),
        scalar => (scalar, 1),
    };
    let base = match element {
        Type::Float { bits: 32 } => 3,
        Type::Float { bits: 16 } => 16,
        Type::Int { bits: 32, .. } if signed => 29,
        Type::Int { bits: 32, .. } => 33,
        Type::Int { bits: 16, .. } if signed => 37,
        Type::Int { bits: 16, .. } => 41,
        Type::Int { bits: 8, .. } if signed => 45,
        Type::Int { bits: 8, .. } => 49,
        Type::Bool => 53,
        _ => return 0,
    };
    base + lanes - 1
}

/// The Metal type of one stage output: a member of the returned structure, or
/// the whole result when the entry point has a single unwrapped output.
fn output_type(
    function: &Function,
    output: &crate::ir::StageOutput,
    position: usize,
) -> Result<Type, TranslateError> {
    match (&function.result, output.field) {
        (Type::Struct(structure), Some(field)) => structure
            .fields
            .get(field as usize)
            .map(|field| field.ty.clone())
            .ok_or_else(|| emission("stage output names a missing returned field")),
        (result, None) if position == 0 => Ok(result.clone()),
        _ => Err(emission("stage output does not match the returned type")),
    }
}

fn descriptor_set(kind: ResourceKind) -> u32 {
    match kind {
        ResourceKind::Texture => 1,
        ResourceKind::Sampler => 2,
        ResourceKind::Imageblock => 3,
        ResourceKind::Buffer
        | ResourceKind::AccelerationStructure
        | ResourceKind::VisibleFunctionTable
        | ResourceKind::IntersectionFunctionTable
        | ResourceKind::ArgumentBuffer => 0,
    }
}

fn type_stride(ty: &Type) -> Result<u32, TranslateError> {
    match ty {
        Type::Int { bits, .. } | Type::Float { bits } => Ok(u32::from(*bits) / 8),
        Type::Vector { element, lanes } => {
            let element = type_stride(element)?;
            Ok(element * u32::from(*lanes).next_power_of_two())
        }
        Type::Struct(structure) => Ok(structure.size),
        Type::Array { element, count } => type_stride(element)?
            .checked_mul(*count)
            .ok_or_else(|| emission("array stride overflows")),
        _ => Err(emission("buffer element type has no SPIR-V array stride")),
    }
}

fn build_error(error: rspirv::dr::Error) -> TranslateError {
    emission(format!("SPIR-V builder rejected the module: {error}"))
}

fn emission(message: impl Into<String>) -> TranslateError {
    TranslateError::one(Diagnostic::new(Phase::SpirvEmission, message))
}

/// The SPIR-V memory semantics Metal's `mem_flags` name.
fn memory_semantics(memory: BarrierMemory) -> u32 {
    let mut bits = 0;
    if memory.device {
        bits |= MemorySemantics::UNIFORM_MEMORY.bits();
    }
    if memory.threadgroup {
        bits |= MemorySemantics::WORKGROUP_MEMORY.bits();
    }
    if memory.texture {
        bits |= MemorySemantics::IMAGE_MEMORY.bits();
    }
    bits
}
