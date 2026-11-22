use super::*;

impl BodyLowering<'_> {
    pub(super) fn binary(
        &mut self,
        lhs: &u64,
        rhs: &u64,
        opcode: &u64,
        record: &llvm_bitcode::bitcode::Record,
        fields: &[u64],
    ) -> Result<Lowered, TranslateError> {
        let (ty, kind) = {
            // A four-field record is ambiguous: it is either an
            // operation with fast-math flags, or one whose first
            // operand is not defined yet and so repeats its type.
            // Only the value table can tell them apart. A
            // five-field record is both at once and needs no
            // guessing.
            let forward_form = matches!(
                fields,
                [first, ..] if self.table.reference(*first)? >= self.table.values.len()
            );
            let (lhs, second, opcode) = match (fields, forward_form) {
                ([first, declared, second, code], true)
                | ([first, declared, second, code, _], _) => (
                    self.operand(*first, Some(declared), record, fields)?,
                    second,
                    code,
                ),
                _ => (self.operand(*lhs, None, record, fields)?, rhs, opcode),
            };
            let (ty, lhs) = lhs;
            // LLVM writes no type for the second operand of a
            // binary operation, even when it names a value the
            // record stream has not reached: both operands of one
            // have the same type, so the first one's is it.
            let (rhs_ty, rhs) = self.operand(*second, Some(&(ty as u64)), record, fields)?;
            if ty != rhs_ty {
                return Err(semantic("LLVM binary operand type mismatch"));
            }
            let scalar = match self.types[ty] {
                DecodedType::Vector { element, .. } => &self.types[element],
                _ => &self.types[ty],
            };
            // LLVM shares one opcode space between integer and
            // floating arithmetic; the operand type selects which.
            let kind = match scalar {
                DecodedType::Float(_) => {
                    let op = match opcode {
                        0 => FloatBinaryOp::Add,
                        1 => FloatBinaryOp::Subtract,
                        2 => FloatBinaryOp::Multiply,
                        4 => FloatBinaryOp::Divide,
                        6 => FloatBinaryOp::Remainder,
                        _ => {
                            return Err(semantic(format!(
                                "unsupported LLVM floating binary opcode {opcode}"
                            )));
                        }
                    };
                    InstructionKind::FloatBinary { op, lhs, rhs }
                }
                DecodedType::Integer(_) => {
                    let op = match opcode {
                        0 => IntegerBinaryOp::Add,
                        1 => IntegerBinaryOp::Subtract,
                        2 => IntegerBinaryOp::Multiply,
                        3 => IntegerBinaryOp::DivideUnsigned,
                        4 => IntegerBinaryOp::DivideSigned,
                        5 => IntegerBinaryOp::RemainderUnsigned,
                        6 => IntegerBinaryOp::RemainderSigned,
                        7 => IntegerBinaryOp::ShiftLeft,
                        8 => IntegerBinaryOp::ShiftRightLogical,
                        9 => IntegerBinaryOp::ShiftRightArithmetic,
                        10 => IntegerBinaryOp::And,
                        11 => IntegerBinaryOp::Or,
                        12 => IntegerBinaryOp::Xor,
                        _ => {
                            return Err(semantic(format!(
                                "unsupported LLVM integer binary opcode {opcode}"
                            )));
                        }
                    };
                    InstructionKind::IntegerBinary { op, lhs, rhs }
                }
                _ => return Err(semantic("LLVM binary operand is not numeric")),
            };
            (ty, kind)
        };
        Ok(Lowered::Value(ty, kind))
    }

    /// `CMP` and `CMP2`: `[lhs(+type), rhs, predicate, fast-math flags?]`.
    /// The flags describe optimization latitude, not a different comparison.
    pub(super) fn compare(
        &mut self,
        record: &llvm_bitcode::bitcode::Record,
        fields: &[u64],
    ) -> Result<Lowered, TranslateError> {
        let (lhs, declared, rest) = self.typed_operand_fields(fields)?;
        let ([rhs, predicate] | [rhs, predicate, _]) = rest else {
            return Err(semantic(format!(
                "LLVM comparison record has operands {fields:?}"
            )));
        };
        let (ty, lhs) = self.operand(*lhs, declared, record, fields)?;
        // Both operands have one type, so a forward right operand has no type
        // of its own.
        let (rhs_ty, rhs) = self.operand(*rhs, Some(&(ty as u64)), record, fields)?;
        if ty != rhs_ty {
            return Err(semantic("LLVM comparison type mismatch"));
        }
        let scalar = match self.types[ty] {
            DecodedType::Vector { element, .. } => &self.types[element],
            _ => &self.types[ty],
        };
        let kind = match scalar {
            DecodedType::Float(_) => InstructionKind::FloatCompare {
                predicate: float_predicate(*predicate)?,
                lhs,
                rhs,
            },
            DecodedType::Integer(_) => InstructionKind::IntegerCompare {
                predicate: integer_predicate(*predicate)?,
                lhs,
                rhs,
            },
            _ => return Err(semantic("LLVM comparison operand is not numeric")),
        };
        Ok(Lowered::Value(boolean_type(ty, self.types)?, kind))
    }

    /// `VSELECT`: `[true(+type), false, condition(+type), fast-math flags?]`.
    pub(super) fn select(
        &mut self,
        record: &llvm_bitcode::bitcode::Record,
        fields: &[u64],
    ) -> Result<Lowered, TranslateError> {
        let (on_true, declared, rest) = self.typed_operand_fields(fields)?;
        let [on_false, rest @ ..] = rest else {
            return Err(semantic(format!(
                "LLVM select record has operands {fields:?}"
            )));
        };
        let (condition, condition_declared, rest) = self.typed_operand_fields(rest)?;
        if rest.len() > 1 {
            return Err(semantic(format!(
                "LLVM select record has operands {fields:?}"
            )));
        }
        let (ty, on_true) = self.operand(*on_true, declared, record, fields)?;
        let (false_ty, on_false) = self.operand(*on_false, Some(&(ty as u64)), record, fields)?;
        let (_, condition) = self.operand(*condition, condition_declared, record, fields)?;
        if ty != false_ty {
            return Err(semantic("LLVM select type mismatch"));
        }
        Ok(Lowered::Value(
            ty,
            InstructionKind::Select {
                condition,
                on_true,
                on_false,
            },
        ))
    }
}
