use std::collections::{BTreeMap, HashMap};

use llvm_bitcode::Bitcode;
use llvm_bitcode::bitcode::{BitcodeElement, Block as BitcodeBlock, Payload};

use crate::ir::{
    Access, AddressSpace, AtomicOp, BarrierMemory, BarrierScope, Block, BlockId, Builtin,
    ConvertOp, EntryPoint, FloatBinaryOp, FloatPredicate, FloatUnaryOp, Function, Instruction,
    InstructionKind, IntegerBinaryOp, IntegerPredicate, IntegerUnaryOp, Interpolation, MemoryOrder,
    Module, Operand, Parameter, Resource, ResourceKind, Sampling, Stage, StageBinding, StageInput,
    StageOutput, Terminator, TextureDimension, TextureType, Type, ValueId,
};
use crate::{Diagnostic, Phase, TranslateError};

mod bitcode;
#[cfg(test)]
pub(crate) mod fixtures;
mod metadata;
mod sampler_state;
mod text;
mod values;

use bitcode::parse_minimal_store_bitcode;
use text::parse_text;

// Re-imported so that `values` and `metadata`, which glob this module, reach
// the bitcode decoding vocabulary by the same unqualified names they always
// have.
use bitcode::{
    DecodedType, decode_sign_rotated, payload_string, record_values, section_name, type_reference,
};

/// An AIR module in one of the representations accepted by the translator.
#[derive(Clone, Copy, Debug)]
pub enum AirInput<'a> {
    Text(&'a str),
    Bitcode(&'a [u8]),
}

/// Parse AIR and verify the resulting Metal IR.
pub fn parse_air(input: AirInput<'_>) -> Result<Module, TranslateError> {
    let module = match input {
        AirInput::Text(text) => parse_text(text)?,
        AirInput::Bitcode(bytes) => parse_bitcode(bytes)?,
    };
    module.verify()?;
    Ok(module)
}

fn parse_bitcode(bytes: &[u8]) -> Result<Module, TranslateError> {
    let bitcode = Bitcode::new(bytes).map_err(|error| {
        TranslateError::one(Diagnostic::new(
            Phase::AirContainer,
            format!("invalid LLVM bitcode: {error}"),
        ))
    })?;
    parse_minimal_store_bitcode(&bitcode)
}

fn syntax(message: impl Into<String>, line: Option<usize>) -> TranslateError {
    let diagnostic = Diagnostic::new(Phase::AirSyntax, message);
    TranslateError::one(match line {
        Some(line) => diagnostic.at_line(line),
        None => diagnostic,
    })
}

fn semantic(message: impl Into<String>) -> TranslateError {
    TranslateError::one(Diagnostic::new(Phase::AirSemantics, message))
}
