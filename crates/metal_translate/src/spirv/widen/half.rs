//! Exact IEEE-754 binary16 round-to-nearest-even in 32-bit shader arithmetic.
//!
//! Vulkan's `OpQuantizeToF16` and `PackHalf2x16` leave the rounding mode to
//! the implementation, and the former may flush every half subnormal. Metal's
//! float-to-half conversion requires ties-to-even and preserves representable
//! half subnormals, so the fallback spells the conversion with integer bits.

use super::*;

pub(super) fn quantize(
    source: Operand,
    half: &Type,
    result: ValueId,
    values: &mut Vec<Type>,
    instructions: &mut Vec<Instruction>,
) -> Option<()> {
    let uint = shape(
        half,
        Type::Int {
            bits: 32,
            signed: false,
        },
    )?;
    let boolean = shape(half, Type::Bool)?;
    let constant_type = uint.clone();
    let constant = |value| integer_constant(&constant_type, value);
    let mut sequence = Sequence {
        uint,
        boolean,
        values,
        instructions,
    };

    // float -> binary16 bits, following the same RTNE cases as IEEE-754:
    // finite normal, half subnormal, underflow, overflow, infinity and NaN.
    let bits = sequence.convert(sequence.uint.clone(), ConvertOp::Bitcast, source);
    let sign = sequence.and(bits.clone(), constant(0x8000_0000)?);
    let exponent_bits = sequence.and(bits.clone(), constant(0x7f80_0000)?);
    let mantissa = sequence.and(bits, constant(0x007f_ffff)?);
    let exponent = sequence.shr(exponent_bits, constant(23)?);
    let half_sign = sequence.shr(sign, constant(16)?);

    let mantissa_nonzero =
        sequence.compare(IntegerPredicate::NotEqual, mantissa.clone(), constant(0)?);
    let nan_bit = sequence.select(mantissa_nonzero, constant(0x0200)?, constant(0)?);
    let nan_payload = sequence.shr(mantissa.clone(), constant(13)?);
    let special = sequence.or(half_sign.clone(), constant(0x7c00)?);
    let special = sequence.or(special, nan_bit);
    let special = sequence.or(special, nan_payload);
    let overflow = sequence.or(half_sign.clone(), constant(0x7c00)?);

    let normal_exponent = sequence.sub(exponent.clone(), constant(112)?);
    let normal_exponent = sequence.shl(normal_exponent, constant(10)?);
    let normal_mantissa = sequence.shr(mantissa.clone(), constant(13)?);
    let normal_round_bit = sequence.and(mantissa.clone(), constant(0x1000)?);
    let normal_round_tail = sequence.and(mantissa.clone(), constant(0x2fff)?);
    let normal_round_bit =
        sequence.compare(IntegerPredicate::NotEqual, normal_round_bit, constant(0)?);
    let normal_round_tail =
        sequence.compare(IntegerPredicate::NotEqual, normal_round_tail, constant(0)?);
    let normal_round = sequence.bool_and(normal_round_bit, normal_round_tail);
    let normal_round = sequence.select(normal_round, constant(1)?, constant(0)?);
    let normal = sequence.or(half_sign.clone(), normal_exponent);
    let normal = sequence.or(normal, normal_mantissa);
    let normal = sequence.add(normal, normal_round);

    // Clamp the exponent before forming variable shifts: every branch is
    // evaluated by SPIR-V selects, and an unselected shift must still be < 32.
    let sub_exponent = sequence.clamp(exponent.clone(), constant(102)?, constant(112)?);
    let hidden_mantissa = sequence.or(mantissa, constant(0x0080_0000)?);
    let sub_shift = sequence.sub(constant(126)?, sub_exponent.clone());
    let sub_mantissa = sequence.shr(hidden_mantissa.clone(), sub_shift);
    let sub_round_shift = sequence.sub(constant(125)?, sub_exponent);
    let sub_round_bit = sequence.shl(constant(1)?, sub_round_shift);
    let sub_round_mask = sequence.mul(sub_round_bit.clone(), constant(3)?);
    let sub_round_mask = sequence.sub(sub_round_mask, constant(1)?);
    let sub_round_bit = sequence.and(hidden_mantissa.clone(), sub_round_bit);
    let sub_round_bit = sequence.compare(IntegerPredicate::NotEqual, sub_round_bit, constant(0)?);
    let sub_round_tail = sequence.and(hidden_mantissa, sub_round_mask);
    let sub_round_tail = sequence.compare(IntegerPredicate::NotEqual, sub_round_tail, constant(0)?);
    let sub_round = sequence.bool_and(sub_round_bit, sub_round_tail);
    let sub_round = sequence.select(sub_round, constant(1)?, constant(0)?);
    let subnormal = sequence.add(sub_mantissa, sub_round);
    let subnormal = sequence.or(half_sign.clone(), subnormal);
    let underflows = sequence.compare(
        IntegerPredicate::UnsignedLessThan,
        exponent.clone(),
        constant(102)?,
    );
    let small = sequence.select(underflows, half_sign.clone(), subnormal);
    let is_small = sequence.compare(
        IntegerPredicate::UnsignedLessEqual,
        exponent.clone(),
        constant(112)?,
    );
    let finite = sequence.select(is_small, small, normal);
    let overflows = sequence.compare(
        IntegerPredicate::UnsignedGreaterEqual,
        exponent.clone(),
        constant(143)?,
    );
    let finite = sequence.select(overflows, overflow, finite);
    let is_special = sequence.compare(IntegerPredicate::Equal, exponent, constant(255)?);
    let half_bits = sequence.select(is_special, special, finite);

    // binary16 bits -> float. This is exact and does not rely on Vulkan's
    // optional half-denormal preservation mode.
    let half_sign = sequence.and(half_bits.clone(), constant(0x8000)?);
    let half_sign = sequence.shl(half_sign, constant(16)?);
    let half_exponent = sequence.and(half_bits.clone(), constant(0x7c00)?);
    let half_mantissa = sequence.and(half_bits.clone(), constant(0x03ff)?);
    let magnitude = sequence.and(half_bits, constant(0x7fff)?);
    let is_zero = sequence.compare(IntegerPredicate::Equal, magnitude, constant(0)?);
    let is_special = sequence.compare(
        IntegerPredicate::Equal,
        half_exponent.clone(),
        constant(0x7c00)?,
    );
    let is_subnormal =
        sequence.compare(IntegerPredicate::Equal, half_exponent.clone(), constant(0)?);

    let normal_exponent = sequence.shr(half_exponent, constant(10)?);
    let normal_exponent = sequence.add(normal_exponent, constant(112)?);
    let normal_exponent = sequence.shl(normal_exponent, constant(23)?);
    let normal_mantissa = sequence.shl(half_mantissa.clone(), constant(13)?);
    let normal = sequence.or(half_sign.clone(), normal_exponent);
    let normal = sequence.or(normal, normal_mantissa);

    let highest = sequence.unary(
        sequence.uint.clone(),
        IntegerUnaryOp::FindMostSignificantBitUnsigned,
        half_mantissa.clone(),
    );
    let leading = sequence.sub(constant(31)?, highest);
    let adjustment = sequence.sub(leading, constant(22)?);
    let sub_exponent = sequence.sub(constant(112)?, adjustment.clone());
    let sub_exponent = sequence.shl(sub_exponent, constant(23)?);
    let sub_shift = sequence.add(constant(14)?, adjustment);
    let sub_mantissa = sequence.shl(half_mantissa.clone(), sub_shift);
    let sub_mantissa = sequence.and(sub_mantissa, constant(0x007f_ffff)?);
    let subnormal = sequence.or(half_sign.clone(), sub_exponent);
    let subnormal = sequence.or(subnormal, sub_mantissa);

    let mantissa_zero =
        sequence.compare(IntegerPredicate::Equal, half_mantissa.clone(), constant(0)?);
    let special_payload = sequence.shl(half_mantissa, constant(13)?);
    let special_payload = sequence.or(constant(0x0040_0000)?, special_payload);
    let special_mantissa = sequence.select(mantissa_zero, constant(0)?, special_payload);
    let special = sequence.or(half_sign.clone(), constant(0x7f80_0000)?);
    let special = sequence.or(special, special_mantissa);
    let finite = sequence.select(is_subnormal, subnormal, normal);
    let nonzero = sequence.select(is_special, special, finite);
    let float_bits = sequence.select(is_zero, half_sign, nonzero);
    sequence.finish(result, float_bits);
    Some(())
}

fn shape(half: &Type, scalar: Type) -> Option<Type> {
    match half {
        Type::Float { bits: 16 } => Some(scalar),
        Type::Vector { element, lanes } if **element == Type::Float { bits: 16 } => {
            Some(Type::Vector {
                element: Box::new(scalar),
                lanes: *lanes,
            })
        }
        _ => None,
    }
}

struct Sequence<'a> {
    uint: Type,
    boolean: Type,
    values: &'a mut Vec<Type>,
    instructions: &'a mut Vec<Instruction>,
}

impl Sequence<'_> {
    fn push(&mut self, ty: Type, kind: InstructionKind) -> Operand {
        let result = ValueId(self.values.len() as u32);
        self.values.push(ty);
        self.instructions.push(Instruction {
            result: Some(result),
            kind,
        });
        Operand::Value(result)
    }

    fn binary(&mut self, op: IntegerBinaryOp, lhs: Operand, rhs: Operand) -> Operand {
        self.push(
            self.uint.clone(),
            InstructionKind::IntegerBinary { op, lhs, rhs },
        )
    }

    fn add(&mut self, lhs: Operand, rhs: Operand) -> Operand {
        self.binary(IntegerBinaryOp::Add, lhs, rhs)
    }

    fn sub(&mut self, lhs: Operand, rhs: Operand) -> Operand {
        self.binary(IntegerBinaryOp::Subtract, lhs, rhs)
    }

    fn mul(&mut self, lhs: Operand, rhs: Operand) -> Operand {
        self.binary(IntegerBinaryOp::Multiply, lhs, rhs)
    }

    fn and(&mut self, lhs: Operand, rhs: Operand) -> Operand {
        self.binary(IntegerBinaryOp::And, lhs, rhs)
    }

    fn or(&mut self, lhs: Operand, rhs: Operand) -> Operand {
        self.binary(IntegerBinaryOp::Or, lhs, rhs)
    }

    fn shl(&mut self, lhs: Operand, rhs: Operand) -> Operand {
        self.binary(IntegerBinaryOp::ShiftLeft, lhs, rhs)
    }

    fn shr(&mut self, lhs: Operand, rhs: Operand) -> Operand {
        self.binary(IntegerBinaryOp::ShiftRightLogical, lhs, rhs)
    }

    fn bool_and(&mut self, lhs: Operand, rhs: Operand) -> Operand {
        self.push(
            self.boolean.clone(),
            InstructionKind::IntegerBinary {
                op: IntegerBinaryOp::And,
                lhs,
                rhs,
            },
        )
    }

    fn compare(&mut self, predicate: IntegerPredicate, lhs: Operand, rhs: Operand) -> Operand {
        self.push(
            self.boolean.clone(),
            InstructionKind::IntegerCompare {
                predicate,
                lhs,
                rhs,
            },
        )
    }

    fn select(&mut self, condition: Operand, on_true: Operand, on_false: Operand) -> Operand {
        self.push(
            self.uint.clone(),
            InstructionKind::Select {
                condition,
                on_true,
                on_false,
            },
        )
    }

    fn clamp(&mut self, value: Operand, min: Operand, max: Operand) -> Operand {
        self.push(
            self.uint.clone(),
            InstructionKind::IntegerClamp {
                signed: false,
                value,
                min,
                max,
            },
        )
    }

    fn unary(&mut self, ty: Type, op: IntegerUnaryOp, value: Operand) -> Operand {
        self.push(ty, InstructionKind::IntegerUnary { op, value })
    }

    fn convert(&mut self, ty: Type, op: ConvertOp, value: Operand) -> Operand {
        self.push(ty, InstructionKind::Convert { op, value })
    }

    fn finish(&mut self, result: ValueId, bits: Operand) {
        self.instructions.push(Instruction {
            result: Some(result),
            kind: InstructionKind::Convert {
                op: ConvertOp::Bitcast,
                value: bits,
            },
        });
    }
}
