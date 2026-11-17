pub mod air;
pub mod cache;
pub mod ir;
pub mod msl;
pub mod passes;
mod sha256;
pub mod specialize;
pub mod spirv;

use std::collections::BTreeSet;
use std::fmt;

pub use air::{AirInput, parse_air};
pub use cache::{
    CACHE_VERSION, MslSource, translate_cached, translate_cached_with, translate_msl_cached,
    translate_msl_cached_with, translate_msl_cached_with_macros, translate_msl_specialized_cached,
    translate_specialized_cached,
};
pub use msl::parse_msl;
pub use passes::{Pass, PassPipeline};
pub use specialize::{
    FunctionConstantValues, MTL_DATA_TYPE_BOOL, MTL_DATA_TYPE_CHAR, MTL_DATA_TYPE_FLOAT,
    MTL_DATA_TYPE_HALF, MTL_DATA_TYPE_INT, MTL_DATA_TYPE_LONG, MTL_DATA_TYPE_SHORT,
    MTL_DATA_TYPE_UCHAR, MTL_DATA_TYPE_UINT, MTL_DATA_TYPE_ULONG, MTL_DATA_TYPE_USHORT,
    metal_data_type_of_encoding, metal_data_type_size, specialize,
};
pub use spirv::{
    EntryArtifact, FunctionConstantInfo, FunctionReflection, RejectedEntry, RequiredFeatures,
    ResourceBinding, StageVariable, TargetProfile, TranslatedLibrary, WORKGROUP_SIZE_SPEC_IDS,
    emit_spirv,
};

/// One entry point declared by a parsed source library.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MslFunctionInfo {
    pub name: String,
    pub stage: ir::Stage,
}

/// A source library after preprocessing, parsing and semantic lowering, but
/// before function constants choose one of its executable variants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MslTemplate {
    module: ir::Module,
    rejected: Vec<RejectedEntry>,
}

impl MslTemplate {
    /// Entry points which reached the common IR, in declaration order.
    pub fn functions(&self) -> impl Iterator<Item = MslFunctionInfo> + '_ {
        self.module.entries.iter().filter_map(|entry| {
            Some(MslFunctionInfo {
                name: self.module.functions.get(entry.function)?.name.clone(),
                stage: entry.stage,
            })
        })
    }

    /// Function constants declared by the library, in source order.
    pub fn function_constants(&self) -> impl Iterator<Item = FunctionConstantInfo> + '_ {
        self.module.globals.iter().filter_map(|global| {
            let slot = global.function_constant.as_ref()?;
            Some(FunctionConstantInfo {
                index: slot.index,
                name: slot.name.clone(),
                encoding: slot.encoding.clone(),
            })
        })
    }

    /// Function constants the named entry point reads, in declaration order.
    pub fn function_constants_for(&self, name: &str) -> Vec<FunctionConstantInfo> {
        self.module
            .entries
            .iter()
            .position(|entry| {
                self.module
                    .functions
                    .get(entry.function)
                    .is_some_and(|function| function.name == name)
            })
            .and_then(|position| {
                entry_function_constants(&self.module)
                    .into_iter()
                    .nth(position)
            })
            .unwrap_or_default()
    }
}

fn operand_globals(operand: &ir::Operand, globals: &mut BTreeSet<usize>) {
    match operand {
        ir::Operand::Global { index, .. } => {
            globals.insert(*index as usize);
        }
        ir::Operand::Composite { elements, .. } => {
            for element in elements {
                operand_globals(element, globals);
            }
        }
        _ => {}
    }
}

fn function_globals(function: &ir::Function) -> BTreeSet<usize> {
    let mut globals = BTreeSet::new();
    for block in &function.blocks {
        for operand in block
            .instructions
            .iter()
            .flat_map(|instruction| ir::instruction_operands(&instruction.kind))
            .chain(ir::terminator_operands(&block.terminator))
        {
            operand_globals(operand, &mut globals);
        }
    }
    globals
}

/// Function constants used by each entry point, in entry/declaration order.
pub fn entry_function_constants(module: &ir::Module) -> Vec<Vec<FunctionConstantInfo>> {
    let mut dependencies = vec![BTreeSet::new(); module.globals.len()];
    for (index, global) in module.globals.iter().enumerate() {
        if global.function_constant.is_some() {
            dependencies[index].insert(index);
        }
    }
    loop {
        let mut changed = false;
        for initializer in &module.static_initializers {
            let reads = function_globals(initializer);
            let inherited: BTreeSet<_> = reads
                .iter()
                .flat_map(|index| dependencies[*index].iter().copied())
                .collect();
            let targets: Vec<_> = initializer
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .filter_map(|instruction| match &instruction.kind {
                    ir::InstructionKind::Store {
                        pointer: ir::Operand::Global { index, .. },
                        ..
                    } => Some(*index as usize),
                    _ => None,
                })
                .collect();
            for target in targets {
                let before = dependencies[target].len();
                dependencies[target].extend(&inherited);
                changed |= dependencies[target].len() != before;
            }
        }
        if !changed {
            break;
        }
    }
    module
        .entries
        .iter()
        .map(|entry| {
            let used: BTreeSet<_> = module
                .functions
                .get(entry.function)
                .into_iter()
                .flat_map(function_globals)
                .flat_map(|index| dependencies[index].iter().copied())
                .collect();
            module
                .globals
                .iter()
                .enumerate()
                .filter_map(|(index, global)| {
                    let slot = global.function_constant.as_ref()?;
                    used.contains(&index).then(|| FunctionConstantInfo {
                        index: slot.index,
                        name: slot.name.clone(),
                        encoding: slot.encoding.clone(),
                    })
                })
                .collect()
        })
        .collect()
}

fn reflect_functions(module: &ir::Module) -> Vec<FunctionReflection> {
    if !module
        .globals
        .iter()
        .any(|global| global.function_constant.is_some())
    {
        return Vec::new();
    }
    module
        .entries
        .iter()
        .zip(entry_function_constants(module))
        .filter_map(|(entry, constants)| {
            Some(FunctionReflection {
                name: module.functions.get(entry.function)?.name.clone(),
                stage: entry.stage,
                constants,
                entry: None,
            })
        })
        .collect()
}

fn attach_function_reflection(
    library: &mut TranslatedLibrary,
    mut reflection: Vec<FunctionReflection>,
    include_unemitted: bool,
) {
    for function in &mut reflection {
        function.entry = library
            .entries
            .iter()
            .position(|entry| entry.name == function.name && entry.stage == function.stage);
    }
    if !include_unemitted {
        reflection.retain(|function| function.entry.is_some());
    }
    library.function_reflection = reflection;
}

/// The phase which reported a translation diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    AirContainer,
    AirSyntax,
    AirSemantics,
    IrVerification,
    Specialization,
    SpirvEmission,
}

/// A contextual, recoverable translation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub phase: Phase,
    pub message: String,
    pub line: Option<usize>,
}

impl Diagnostic {
    pub(crate) fn new(phase: Phase, message: impl Into<String>) -> Self {
        Self {
            phase,
            message: message.into(),
            line: None,
        }
    }

    pub(crate) fn at_line(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }
}

/// One or more diagnostics from a failed translation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranslateError {
    pub diagnostics: Vec<Diagnostic>,
}

impl TranslateError {
    pub(crate) fn one(diagnostic: Diagnostic) -> Self {
        Self {
            diagnostics: vec![diagnostic],
        }
    }
}

impl fmt::Display for TranslateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, diagnostic) in self.diagnostics.iter().enumerate() {
            if index != 0 {
                writeln!(f)?;
            }
            write!(f, "{:?}", diagnostic.phase)?;
            if let Some(line) = diagnostic.line {
                write!(f, " at line {line}")?;
            }
            write!(f, ": {}", diagnostic.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for TranslateError {}

/// Parse, verify, optimize with the process's [`PassPipeline::from_env`], and
/// emit every entry point in one AIR module.
pub fn translate(
    input: AirInput<'_>,
    target: &TargetProfile,
) -> Result<TranslatedLibrary, TranslateError> {
    translate_with(input, target, PassPipeline::from_env())
}

/// [`translate`] with an explicit pass pipeline.
pub fn translate_with(
    input: AirInput<'_>,
    target: &TargetProfile,
    passes: &PassPipeline,
) -> Result<TranslatedLibrary, TranslateError> {
    translate_specialized(input, target, passes, &FunctionConstantValues::new())
}

/// [`translate_with`] for a function made with `MTLFunctionConstantValues`.
///
/// Specialization comes before the passes: a function constant which becomes
/// a literal is what lets folding remove the code it switches off.
pub fn translate_specialized(
    input: AirInput<'_>,
    target: &TargetProfile,
    passes: &PassPipeline,
    constants: &FunctionConstantValues,
) -> Result<TranslatedLibrary, TranslateError> {
    let mut module = parse_air(input)?;
    let reflection = reflect_functions(&module);
    specialize(&mut module, constants)?;
    passes.run(&mut module)?;
    let mut library = emit_spirv(&module, target)?;
    attach_function_reflection(&mut library, reflection, false);
    Ok(library)
}

/// Parse and translate the supported source-MSL subset.
///
/// Source compilation starts as a separate frontend rather than pretending MSL
/// is AIR.  Both frontends produce the same verified Metal IR before SPIR-V
/// emission.
pub fn translate_msl(
    source: &str,
    target: &TargetProfile,
) -> Result<TranslatedLibrary, TranslateError> {
    translate_msl_with(source, target, PassPipeline::from_env())
}

/// [`translate_msl`] with an explicit pass pipeline.
pub fn translate_msl_with(
    source: &str,
    target: &TargetProfile,
    passes: &PassPipeline,
) -> Result<TranslatedLibrary, TranslateError> {
    translate_msl_with_macros(source, &[], target, passes)
}

/// [`translate_msl_with`] with preprocessor macros defined first, as
/// `MTLCompileOptions.preprocessorMacros` defines them.
pub fn translate_msl_with_macros(
    source: &str,
    macros: &[(String, String)],
    target: &TargetProfile,
    passes: &PassPipeline,
) -> Result<TranslatedLibrary, TranslateError> {
    translate_msl_specialized(
        source,
        macros,
        target,
        passes,
        &FunctionConstantValues::new(),
    )
}

/// Parse and lower a source library without requiring values for its function
/// constants. Code generation happens from a clone when a variant is needed.
pub fn compile_msl_template(
    source: &str,
    macros: &[(String, String)],
) -> Result<MslTemplate, TranslateError> {
    let (module, rejected) = msl::parse_msl_partial(source, macros)
        .map_err(|error| with_msl_source_context(source, error))?;
    Ok(MslTemplate { module, rejected })
}

/// Translate a parsed source template with one concrete constant-value set,
/// rejecting only entries whose constants remain undefined.
pub fn translate_msl_template_specialized(
    template: &MslTemplate,
    target: &TargetProfile,
    passes: &PassPipeline,
    constants: &FunctionConstantValues,
) -> Result<TranslatedLibrary, TranslateError> {
    translate_msl_template_inner(template, target, passes, constants)
}

/// Produce the base library while retaining entries whose required constants
/// are not supplied as template-only functions.
pub fn translate_msl_template(
    template: &MslTemplate,
    target: &TargetProfile,
    passes: &PassPipeline,
) -> Result<TranslatedLibrary, TranslateError> {
    translate_msl_template_inner(template, target, passes, &FunctionConstantValues::new())
}

fn translate_msl_template_inner(
    template: &MslTemplate,
    target: &TargetProfile,
    passes: &PassPipeline,
    constants: &FunctionConstantValues,
) -> Result<TranslatedLibrary, TranslateError> {
    let dependencies = msl::function_constant_dependencies(&template.module);
    let mut module = template.module.clone();
    specialize(&mut module, constants)?;
    passes.run(&mut module)?;
    let mut rejected = template.rejected.clone();
    rejected.extend(msl::reject_undefined_entries(&mut module, &dependencies)?);
    let mut library = emit_spirv(&module, target)?;
    library.rejected.extend(rejected);
    attach_function_reflection(&mut library, reflect_functions(&template.module), true);
    Ok(library)
}

/// Translate source with the macro definitions and function constants supplied by Metal.
pub fn translate_msl_specialized(
    source: &str,
    macros: &[(String, String)],
    target: &TargetProfile,
    passes: &PassPipeline,
    constants: &FunctionConstantValues,
) -> Result<TranslatedLibrary, TranslateError> {
    let template = compile_msl_template(source, macros)?;
    translate_msl_template_specialized(&template, target, passes, constants)
}

fn with_msl_source_context(source: &str, mut error: TranslateError) -> TranslateError {
    for diagnostic in &mut error.diagnostics {
        if let Some(line) = diagnostic.line {
            diagnostic
                .message
                .push_str(&format_msl_source_context(source, line));
        }
    }
    error
}

/// Render a small source window under an MSL diagnostic.
///
/// The parser reports one-based lines. Keep the window deliberately small so
/// an Objective-C error message remains useful even for generated shaders.
fn format_msl_source_context(source: &str, line: usize) -> String {
    const CONTEXT_LINES: usize = 2;

    let lines: Vec<_> = source.lines().collect();
    let start = line.saturating_sub(CONTEXT_LINES + 1);
    let end = line.saturating_add(CONTEXT_LINES).min(lines.len());
    let width = end.max(1).to_string().len();
    let mut context = String::from("\n\nMSL source context:");

    for (index, text) in lines[start..end].iter().enumerate() {
        let number = start + index + 1;
        let marker = if number == line { '>' } else { ' ' };
        context.push_str(&format!("\n{marker} {number:>width$} | {text}"));
    }
    context
}

/// Extract the LLVM bitcode payload from a compiled Metal library.
///
/// A metallib has four offset/size pairs in its fixed header. The final pair
/// names a bitcode wrapper whose own header gives the payload offset and size.
pub fn metallib_air(bytes: &[u8]) -> Result<&[u8], TranslateError> {
    metallib_airs(bytes)?.into_iter().next().ok_or_else(|| {
        TranslateError::one(Diagnostic::new(
            Phase::AirContainer,
            "metallib contains no AIR modules",
        ))
    })
}

/// Extract every LLVM bitcode payload from a compiled Metal library.
///
/// A metallib concatenates one sixteen-byte-aligned bitcode wrapper per
/// exported module in the final section of its fixed header.
pub fn metallib_airs(bytes: &[u8]) -> Result<Vec<&[u8]>, TranslateError> {
    fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
        Some(u64::from_le_bytes(
            bytes.get(offset..offset + 8)?.try_into().ok()?,
        ))
    }
    fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
    }

    let malformed = |message| TranslateError::one(Diagnostic::new(Phase::AirContainer, message));
    if bytes.get(..4) != Some(b"MTLB") {
        return Err(malformed("input is not a metallib container"));
    }
    let wrapper_offset = u64_at(bytes, 72)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| malformed("metallib has no bitcode-wrapper offset"))?;
    let wrapper_size = u64_at(bytes, 80)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| malformed("metallib has no bitcode-wrapper size"))?;
    let wrappers = bytes
        .get(wrapper_offset..wrapper_offset.saturating_add(wrapper_size))
        .ok_or_else(|| malformed("metallib bitcode wrapper is out of bounds"))?;
    let mut offset = 0usize;
    let mut modules = Vec::new();
    while offset < wrappers.len() {
        let wrapper = &wrappers[offset..];
        if wrapper.get(..4) != Some(&0x0b17_c0de_u32.to_le_bytes()) {
            return Err(malformed("metallib has an unknown bitcode wrapper"));
        }
        let payload_offset = u32_at(wrapper, 8)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| malformed("metallib has no AIR payload offset"))?;
        let payload_size = u32_at(wrapper, 12)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| malformed("metallib has no AIR payload size"))?;
        let end = payload_offset
            .checked_add(payload_size)
            .ok_or_else(|| malformed("metallib AIR payload size overflows"))?;
        let payload = wrapper
            .get(payload_offset..end)
            .ok_or_else(|| malformed("metallib AIR payload is out of bounds"))?;
        if payload.get(..4) != Some(&[b'B', b'C', 0xc0, 0xde]) {
            return Err(malformed("metallib AIR payload is not LLVM bitcode"));
        }
        modules.push(payload);
        let length = if end == wrapper.len() {
            end
        } else {
            end.checked_add(15)
                .map(|length| length & !15)
                .ok_or_else(|| malformed("metallib bitcode wrapper size overflows"))?
        };
        offset = offset
            .checked_add(length)
            .ok_or_else(|| malformed("metallib bitcode wrapper offset overflows"))?;
    }
    if offset != wrappers.len() {
        return Err(malformed("metallib bitcode wrappers exceed their section"));
    }
    Ok(modules)
}
