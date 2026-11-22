use super::*;

impl BodyLowering<'_> {
    pub(super) fn call(
        &mut self,
        explicit_signature: Option<&u64>,
        callee: &u64,
        args: &[u64],
        record: &llvm_bitcode::bitcode::Record,
        fields: &[u64],
    ) -> Result<Lowered, TranslateError> {
        let (ty, kind) = {
            let callee = self.table.operand_id(*callee)?;
            let signature = match self.types[self.table.values[callee].ty] {
                DecodedType::Pointer { pointee, .. } => pointee,
                _ => self.table.values[callee].ty,
            };
            if let Some(explicit) = explicit_signature
                && type_reference(Some(explicit), self.types.len())? != signature
            {
                return Err(semantic(
                    "LLVM call's explicit type does not match its target",
                ));
            }
            let DecodedType::Function { result, parameters } = &self.types[signature] else {
                return Err(semantic("LLVM call target is not a function"));
            };
            let name = self
                .names
                .get(&callee)
                .ok_or_else(|| semantic("LLVM call target has no name"))?
                .clone();
            if let Some(callees) = self.callees
                && let Some(function) = callees.lower(callee)?
            {
                if args.len() != parameters.len() {
                    return Err(semantic("LLVM call arity mismatch"));
                }
                let mut actual = Vec::new();
                for (&arg, &expected) in args.iter().zip(parameters) {
                    let (ty, value) = self.operand(arg, None, record, fields)?;
                    if ty != expected {
                        return Err(semantic("LLVM call argument type mismatch"));
                    }
                    actual.push(value);
                }
                let returns = self.inline(&function, actual)?;
                if matches!(self.types[*result], DecodedType::Void) {
                    return Ok(Lowered::Done);
                }
                if returns.is_empty() {
                    return Err(semantic(format!("AIR function {name} never returns")));
                }
                return Ok(Lowered::Value(
                    *result,
                    InstructionKind::Phi { incomings: returns },
                ));
            }
            // `llvm.memcpy` declares byte pointers, so its
            // arguments never match the declaration by type. Metal
            // reaches it by assigning one whole structure, which
            // is a load and a store of that structure.
            if name.starts_with("llvm.memcpy.") {
                // Older LLVM spells an alignment between the
                // length and the volatile flag; both forms end
                // with markers this lowering does not need.
                let [destination, source, length, ..] = args else {
                    return Err(semantic("llvm.memcpy has unexpected operands"));
                };
                let destination_id = self.table.operand_id(*destination)?;
                let source_id = self.table.operand_id(*source)?;
                let (into, destination) =
                    self.copied_pointer(*destination, destination_id, record, fields)?;
                let (from, source) = self.copied_pointer(*source, source_id, record, fields)?;
                if into != from {
                    return Err(semantic("llvm.memcpy copies between two different types"));
                }
                let copied = ir_type(into, self.types)?;
                let (size, _) = layout(&copied)?;
                let bytes = self
                    .table
                    .integer(self.table.operand_id(*length)?, self.types)?;
                if bytes != u64::from(size) {
                    return Err(semantic(
                        "llvm.memcpy length is not one whole value of the copied type",
                    ));
                }
                let loaded = emit(
                    &mut self.function,
                    &mut self.instructions,
                    copied,
                    InstructionKind::Load { pointer: source },
                );
                self.instructions.push(Instruction {
                    result: None,
                    kind: InstructionKind::Store {
                        pointer: destination,
                        value: loaded,
                    },
                });
                return Ok(Lowered::Done);
            }
            // Metal reaches `llvm.memset` by initialising a local
            // array to zero, which is a store of a zero value.
            // Any other fill pattern would have to be spelled
            // byte by byte, which logical addressing cannot do.
            if name.starts_with("llvm.memset.") {
                let [destination, fill, length, ..] = args else {
                    return Err(semantic("llvm.memset has unexpected operands"));
                };
                let destination_id = self.table.operand_id(*destination)?;
                let (into, destination) = match self.aliases.get(&destination_id) {
                    Some(alias) => (alias.pointee, alias.operand.clone()),
                    None => {
                        let (ty, value) = self.operand(*destination, None, record, fields)?;
                        match self.types[ty] {
                            DecodedType::Pointer { pointee, .. } => (pointee, value),
                            _ => {
                                return Err(semantic("llvm.memset destination is not a pointer"));
                            }
                        }
                    }
                };
                if self
                    .table
                    .integer(self.table.operand_id(*fill)?, self.types)?
                    != 0
                {
                    return Err(semantic("llvm.memset fills with a nonzero byte"));
                }
                let cleared = ir_type(into, self.types)?;
                let (size, _) = layout(&cleared)?;
                if self
                    .table
                    .integer(self.table.operand_id(*length)?, self.types)?
                    != u64::from(size)
                {
                    return Err(semantic(
                        "llvm.memset length is not one whole value of the cleared type",
                    ));
                }
                self.instructions.push(Instruction {
                    result: None,
                    kind: InstructionKind::Store {
                        pointer: destination,
                        value: zero_composite(&cleared)?,
                    },
                });
                return Ok(Lowered::Done);
            }
            if args.len() != parameters.len() {
                return Err(semantic("LLVM call arity mismatch"));
            }
            let mut actual = Vec::new();
            for (&arg, &expected) in args.iter().zip(parameters) {
                let (ty, value) = self.operand(arg, None, record, fields)?;
                if ty != expected {
                    return Err(semantic("LLVM call argument type mismatch"));
                }
                actual.push(value);
            }
            // A lifetime marker bounds a local's live range for
            // LLVM's own optimizers. SPIR-V has no such thing, and
            // the variable is live for the whole function there.
            if name.starts_with("llvm.lifetime.") {
                return Ok(Lowered::Done);
            }
            let kind = if name.starts_with("air.normalize_function_constant_predicate.") {
                // A static initializer turns an integer expression derived
                // from function constants into the zero or one a gated
                // binding is tested against. Apple's CoreDisplay AIR passes
                // an unmasked i32 feature bitset here, while its i8 calls
                // include both Boolean loads and computed predicates: the
                // operation is `value != 0`, widened back to its input type.
                let [value] = actual.as_slice() else {
                    return Err(semantic(format!("LLVM intrinsic {name} takes one operand")));
                };
                let ty = ir_type(*result, self.types)?;
                normalize_function_constant_predicate(
                    value.clone(),
                    ty,
                    &mut self.function,
                    &mut self.instructions,
                )?
            } else {
                let result_type = ir_type(*result, self.types)?;
                known_helper(&name, &actual)
                    .or_else(|| typed_air_intrinsic(&name, &actual, &result_type))
                    .unwrap_or_else(|| air_intrinsic(&name, &actual))?
            };
            if matches!(self.types[*result], DecodedType::Void) {
                self.instructions.push(Instruction { result: None, kind });
                return Ok(Lowered::Done);
            }
            // Newer AIR returns a sample or read with a status byte beside the
            // texel. Only the texel can be taken out of it: what the status
            // says is not known, so the value is never given an operand.
            if let InstructionKind::TextureSample { .. } | InstructionKind::TextureRead { .. } =
                kind
                && let DecodedType::Struct { fields, .. } = &self.types[*result]
                && let [texel, status] = fields[..]
                && matches!(self.types[status], DecodedType::Integer(8))
            {
                let texel = emit(
                    &mut self.function,
                    &mut self.instructions,
                    ir_type(texel, self.types)?,
                    kind,
                );
                self.texels_with_status
                    .insert(self.table.values.len(), texel);
                self.table.values.push(Value {
                    ty: *result,
                    constant: None,
                });
                return Ok(Lowered::Done);
            }
            (*result, kind)
        };
        Ok(Lowered::Value(ty, kind))
    }

    /// The pointee type and address of one `llvm.memcpy` operand.
    ///
    /// A widening bitcast records the type its pointer really addresses; the
    /// copy is of that, not of what the cast claimed.
    fn copied_pointer(
        &mut self,
        encoded: u64,
        id: usize,
        record: &llvm_bitcode::bitcode::Record,
        fields: &[u64],
    ) -> Result<(usize, Operand), TranslateError> {
        if let Some(alias) = self.aliases.get(&id) {
            return Ok((alias.pointee, alias.operand.clone()));
        }
        let (ty, value) = self.operand(encoded, None, record, fields)?;
        match self.types[ty] {
            DecodedType::Pointer { pointee, .. } => Ok((pointee, value)),
            _ => Err(semantic("llvm.memcpy operand is not a pointer")),
        }
    }
}

/// Lower AIR's integer predicate normalization without treating it as a
/// numeric clamp. The comparison records the intrinsic's actual truth table;
/// the extension preserves Apple's i8 or i32 result representation.
fn normalize_function_constant_predicate(
    value: Operand,
    ty: Type,
    function: &mut Function,
    instructions: &mut Vec<Instruction>,
) -> Result<InstructionKind, TranslateError> {
    if !matches!(ty, Type::Int { .. }) {
        return Err(semantic(
            "AIR function-constant predicate normalization is not scalar integer",
        ));
    }
    let predicate = emit(
        function,
        instructions,
        Type::Bool,
        InstructionKind::IntegerCompare {
            predicate: IntegerPredicate::NotEqual,
            lhs: value,
            rhs: Operand::Integer {
                ty: ty.clone(),
                value: 0,
            },
        },
    );
    Ok(InstructionKind::Convert {
        op: ConvertOp::ZeroExtend,
        value: predicate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Pass, PassPipeline};

    /// Recovery CoreDisplay AIR sends an i32 feature bitset straight to the
    /// intrinsic. Every nonzero bit pattern is true, not just literal one.
    #[test]
    fn a_function_constant_predicate_normalizes_every_nonzero_integer() {
        for (input, expected) in [(0, 0), (1, 1), (2, 1), (u32::MAX as u64, 1)] {
            let ty = Type::u32();
            let mut function = Function {
                name: "normalize".to_owned(),
                result: ty.clone(),
                parameters: Vec::new(),
                values: Vec::new(),
                blocks: Vec::new(),
            };
            let mut instructions = Vec::new();
            let kind = normalize_function_constant_predicate(
                Operand::Integer {
                    ty: ty.clone(),
                    value: input,
                },
                ty.clone(),
                &mut function,
                &mut instructions,
            )
            .expect("an integer predicate");
            let result = emit(&mut function, &mut instructions, ty.clone(), kind);
            function.blocks.push(Block {
                id: BlockId(0),
                name: "entry".to_owned(),
                instructions,
                terminator: Terminator::Return(Some(result)),
            });
            let mut module = Module {
                static_initializers: Vec::new(),
                globals: Vec::new(),
                functions: vec![function],
                entries: Vec::new(),
            };
            module.verify().expect("the lowered intrinsic is valid");
            PassPipeline::new([Pass::FoldConstants])
                .run(&mut module)
                .expect("the predicate folds");
            assert_eq!(
                module.functions[0].blocks[0].terminator,
                Terminator::Return(Some(Operand::Integer {
                    ty: ty.clone(),
                    value: expected,
                }))
            );
        }
    }
}
