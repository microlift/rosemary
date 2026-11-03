mod ast;
mod blit;
mod index;
mod lex;
mod lower;
mod parse;
mod preprocess;
mod types;

use std::collections::HashMap;

use crate::ir::{
    Access, AddressSpace, Block, BlockId, ConvertOp, EntryPoint, FloatBinaryOp, Function,
    Instruction, InstructionKind, IntegerBinaryOp, IntegerPredicate, Module, Operand, Parameter,
    Resource, ResourceKind, Stage, StructField, StructType, Terminator, TextureDimension,
    TextureType, Type, ValueId,
};
use crate::{Diagnostic, Phase, TranslateError};

use blit::parse_blit_kernel;
use index::{IndexBindings, lower_blit_index};
use types::{
    parse_parameter_type, parse_resource_attribute, parse_structs, split_parameters,
    writable_float_texture,
};

/// Parse a Metal source library into the Metal IR.
pub fn parse_msl(source: &str) -> Result<Module, TranslateError> {
    parse_msl_with_macros(source, &[])
}

/// [`parse_msl`] with preprocessor macros defined first, as
/// `MTLCompileOptions.preprocessorMacros` defines them.
pub fn parse_msl_with_macros(
    source: &str,
    macros: &[(String, String)],
) -> Result<Module, TranslateError> {
    let (module, rejected) = parse_msl_partial(source, macros)?;
    match rejected.into_iter().next() {
        Some(rejected) => Err(TranslateError::one(Diagnostic::new(
            Phase::AirSyntax,
            format!("{}: {}", rejected.name, rejected.reason),
        ))),
        None => Ok(module),
    }
}

/// [`parse_msl_with_macros`], keeping the entry points which parse and
/// lower and naming each other one with its reason.
pub fn parse_msl_partial(
    source: &str,
    macros: &[(String, String)],
) -> Result<(Module, Vec<crate::RejectedEntry>), TranslateError> {
    // A guest's string may carry its C terminator.
    let source = source.trim_end_matches('\0');
    let compiled = (|| {
        let text = preprocess::preprocess(source, macros)?;
        let tokens = lex::lex(&text, 1)?;
        let program = parse::parse(&tokens)?;
        let (module, rejected) = lower::lower(&program)?;
        module.verify()?;
        Ok((module, rejected))
    })();
    match compiled {
        Ok(compiled) => Ok(compiled),
        // The narrow kernels the first frontend parsed are still accepted
        // exactly as before where the general frontend refuses them.
        Err(error) => parse_legacy(source)
            .map(|module| (module, Vec::new()))
            .map_err(|_| error),
    }
}

fn syntax_error(line: usize, message: impl Into<String>) -> TranslateError {
    TranslateError::one(Diagnostic::new(Phase::AirSyntax, message).at_line(line))
}

/// The first source frontend: one kernel shape at a time.
fn parse_legacy(source: &str) -> Result<Module, TranslateError> {
    let source = strip_comments(source);
    let structs = parse_structs(&source)?;
    let kernels: Vec<usize> = source
        .match_indices("kernel void")
        .map(|(offset, _)| offset)
        .collect();
    if kernels.is_empty() {
        return Err(syntax_at(
            &source,
            0,
            "MSL source has no `kernel void` entry point",
        ));
    }
    // One library declares several kernels and a guest asks for each by name,
    // so every one is parsed into the same module. Parsing only the first left
    // `newFunctionWithName:` answering nil for the rest, which a guest that
    // does not check reads as a shader which failed to compile.
    let mut merged = Module {
        functions: Vec::new(),
        entries: Vec::new(),
        globals: Vec::new(),
        static_initializers: Vec::new(),
    };
    for kernel in kernels {
        let module = parse_kernel(&source, &structs, kernel)?;
        let base = merged.functions.len();
        merged.functions.extend(module.functions);
        merged
            .entries
            .extend(module.entries.into_iter().map(|mut entry| {
                entry.function += base;
                entry
            }));
    }
    merged.verify()?;
    Ok(merged)
}

/// Parse the one kernel which begins at `kernel` in `source`.
fn parse_kernel(
    source: &str,
    structs: &HashMap<String, Type>,
    kernel: usize,
) -> Result<Module, TranslateError> {
    let after_kernel = source[kernel + "kernel void".len()..].trim_start();
    let name_end = after_kernel
        .find(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .ok_or_else(|| syntax_at(source, kernel, "MSL kernel has no parameter list"))?;
    let name = after_kernel[..name_end].trim();
    if name.is_empty() {
        return Err(syntax_at(source, kernel, "MSL kernel has no name"));
    }
    let after_name = &after_kernel[name_end..];
    let open = after_name
        .find('(')
        .ok_or_else(|| syntax_at(source, kernel, "MSL kernel has no parameter list"))?;
    let close = matching(after_name, open, '(', ')').ok_or_else(|| {
        syntax_at(
            source,
            kernel + "kernel void".len() + name_end + open,
            "MSL kernel has an unterminated parameter list",
        )
    })?;
    let body_open = after_name[close + 1..]
        .find('{')
        .map(|offset| close + 1 + offset)
        .ok_or_else(|| {
            syntax_at(
                source,
                kernel + "kernel void".len() + name_end + close,
                "MSL kernel has no body",
            )
        })?;
    let body_close = matching(after_name, body_open, '{', '}').ok_or_else(|| {
        syntax_at(
            source,
            kernel + "kernel void".len() + name_end + body_open,
            "MSL kernel has an unterminated body",
        )
    })?;
    let parameters = &after_name[open + 1..close];
    let body = &after_name[body_open + 1..body_close];

    let mut resources = Vec::new();
    let mut function_parameters = Vec::new();
    let mut values = Vec::new();
    let mut builtins = HashMap::new();
    for raw in split_parameters(parameters) {
        // Search from this kernel, not from the start of the library: two
        // kernels may declare a parameter with identical text, and a
        // diagnostic which named the first would point at the wrong line.
        let offset = source[kernel..]
            .find(raw)
            .map_or(kernel, |offset| kernel + offset);
        let (declaration, attribute) = raw
            .split_once("[[")
            .ok_or_else(|| syntax_at(source, offset, "MSL parameter has no attribute"))?;
        let attribute = attribute
            .strip_suffix("]]")
            .ok_or_else(|| syntax_at(source, offset, "MSL parameter attribute is malformed"))?
            .trim();
        let declaration = declaration.trim();
        let name = declaration
            .split_whitespace()
            .last()
            .ok_or_else(|| syntax_at(source, offset, "MSL parameter has no name"))?;
        let ty = parse_parameter_type(declaration, name, structs)
            .map_err(|message| syntax_at(source, offset, message))?;
        let value = ValueId(values.len() as u32);
        values.push(ty.clone());
        if attribute.starts_with("thread_position_in_grid") {
            builtins.insert(name.to_string(), value);
            continue;
        }
        let argument = function_parameters.len() as u32;
        function_parameters.push(Parameter {
            value,
            name: name.to_string(),
            ty: ty.clone(),
        });
        if let Some((kind, index, access)) = parse_resource_attribute(attribute, declaration, &ty) {
            resources.push(Resource {
                argument,
                kind,
                index,
                count: 1,
                access,
                ty,
                name: Some(name.to_string()),
                inline_sampler: None,
            });
        } else {
            return Err(syntax_at(
                source,
                offset,
                "unsupported MSL parameter attribute",
            ));
        }
    }
    if body.trim_start().starts_with("if") {
        return parse_blit_kernel(
            source,
            name,
            body,
            function_parameters,
            values,
            resources,
            builtins,
        );
    }
    let Some(writable_buffer) = resources.iter().find(|resource| {
        resource.kind == ResourceKind::Buffer
            && resource.access != Access::Read
            && resource.ty
                == Type::Pointer {
                    space: AddressSpace::Device,
                    pointee: Box::new(Type::u32()),
                }
    }) else {
        return Err(syntax_at(
            source,
            source.find(body).unwrap_or(0),
            "MSL function body requires operations not implemented by the source frontend",
        ));
    };
    let parameter_value = function_parameters[writable_buffer.argument as usize].value;
    let prefix = format!(
        "{}[0] =",
        function_parameters[writable_buffer.argument as usize].name
    );
    let statement = body.trim().trim_end_matches(';').trim();
    let value = statement
        .strip_prefix(&prefix)
        .ok_or_else(|| {
            syntax_at(
                source,
                source.find(body).unwrap_or(0),
                "only `buffer[0] = integer_literal;` is supported",
            )
        })?
        .trim()
        .parse::<u32>()
        .map_err(|_| {
            syntax_at(
                source,
                source.find(body).unwrap_or(0),
                "MSL store value is not a `uint` literal",
            )
        })?;

    let module = Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![Function {
            name: name.to_string(),
            result: Type::Void,
            parameters: function_parameters,
            values,
            blocks: vec![Block {
                id: BlockId(0),
                name: "entry".to_string(),
                instructions: vec![Instruction {
                    result: None,
                    kind: InstructionKind::Store {
                        pointer: Operand::Value(parameter_value),
                        value: Operand::u32(value),
                    },
                }],
                terminator: Terminator::Return(None),
            }],
        }],
        entries: vec![EntryPoint {
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

fn matching(text: &str, open: usize, left: char, right: char) -> Option<usize> {
    let mut depth = 0_u32;
    for (offset, character) in text[open..].char_indices() {
        if character == left {
            depth += 1;
        } else if character == right {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(open + offset);
            }
        }
    }
    None
}

fn strip_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

fn syntax_at(source: &str, offset: usize, message: impl Into<String>) -> TranslateError {
    let line = source[..offset.min(source.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    TranslateError::one(Diagnostic::new(Phase::AirSyntax, message).at_line(line))
}
