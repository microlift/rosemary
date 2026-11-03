pub mod air;
pub mod cache;
pub mod ir;
pub mod msl;
pub mod passes;
mod sha256;
pub mod specialize;
pub mod spirv;

use std::fmt;

pub use air::{AirInput, parse_air};
pub use cache::{
    CACHE_VERSION, translate_cached, translate_cached_with, translate_msl_cached,
    translate_msl_cached_with, translate_msl_cached_with_macros, translate_specialized_cached,
};
pub use msl::parse_msl;
pub use passes::{Pass, PassPipeline};
pub use specialize::{
    FunctionConstantValues, metal_data_type_of_encoding, metal_data_type_size, specialize,
};
pub use spirv::{
    EntryArtifact, FunctionConstantInfo, RejectedEntry, RequiredFeatures, ResourceBinding,
    StageVariable, TargetProfile, TranslatedLibrary, WORKGROUP_SIZE_SPEC_IDS, emit_spirv,
};

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
    specialize(&mut module, constants)?;
    passes.run(&mut module)?;
    emit_spirv(&module, target)
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
    let with_context = |mut error: TranslateError| {
        for diagnostic in &mut error.diagnostics {
            if let Some(line) = diagnostic.line {
                diagnostic
                    .message
                    .push_str(&format_msl_source_context(source, line));
            }
        }
        error
    };
    let (mut module, rejected) = msl::parse_msl_partial(source, macros).map_err(with_context)?;
    specialize(&mut module, &FunctionConstantValues::new())?;
    passes.run(&mut module)?;
    let mut library = emit_spirv(&module, target)?;
    library.rejected.extend(rejected);
    Ok(library)
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
