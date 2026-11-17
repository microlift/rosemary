//! Exact IEEE-754 binary16 round-to-nearest-even in 32-bit shader arithmetic.
//!
//! Vulkan's `OpQuantizeToF16` and `PackHalf2x16` leave the rounding mode to
//! the implementation, and the former may flush every half subnormal. Metal's
//! float-to-half conversion requires ties-to-even and preserves representable
//! half subnormals, so the fallback spells the conversion with integer bits.

use super::*;

// IEEE-754 binary32 fields.
const FLOAT_SIGN: u64 = 0x8000_0000;
const FLOAT_EXPONENT: u64 = 0x7f80_0000;
const FLOAT_MANTISSA: u64 = 0x007f_ffff;
const FLOAT_MANTISSA_BITS: u64 = 23;
const FLOAT_HIDDEN_BIT: u64 = 1 << FLOAT_MANTISSA_BITS;
const FLOAT_QUIET_NAN: u64 = 0x0040_0000;
const FLOAT_SPECIAL_EXPONENT: u64 = 0xff;

// IEEE-754 binary16 fields.
const HALF_SIGN: u64 = 0x8000;
const HALF_EXPONENT: u64 = 0x7c00;
const HALF_MANTISSA: u64 = 0x03ff;
const HALF_MAGNITUDE: u64 = 0x7fff;
const HALF_MANTISSA_BITS: u64 = 10;
const HALF_QUIET_NAN: u64 = 0x0200;

/// How far a binary32 sign or mantissa moves to reach its binary16 place.
const SIGN_SHIFT: u64 = 16;
const MANTISSA_SHIFT: u64 = FLOAT_MANTISSA_BITS - HALF_MANTISSA_BITS;
const FLOAT_BIAS: u64 = 127;
const HALF_BIAS: u64 = 15;
/// The biased binary32 exponent of binary16's zero exponent, which holds
/// its subnormals.
const BIAS_DIFFERENCE: u64 = FLOAT_BIAS - HALF_BIAS;
/// The biased binary32 exponent of binary16's all-ones exponent: 2^16, the
/// first magnitude binary16 cannot hold.
const OVERFLOW_EXPONENT: u64 = BIAS_DIFFERENCE + (HALF_EXPONENT >> HALF_MANTISSA_BITS);
/// The biased binary32 exponent of 2^-25, half the smallest binary16
/// subnormal: anything below rounds to zero.
const UNDERFLOW_EXPONENT: u64 = BIAS_DIFFERENCE - HALF_MANTISSA_BITS;
/// A subnormal's mantissa, hidden bit included, shifts right by this less
/// its binary32 exponent.
const SUBNORMAL_SHIFT_BASE: u64 = BIAS_DIFFERENCE + MANTISSA_SHIFT + 1;

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
    let sign = sequence.and(bits.clone(), constant(FLOAT_SIGN)?);
    let exponent_bits = sequence.and(bits.clone(), constant(FLOAT_EXPONENT)?);
    let mantissa = sequence.and(bits, constant(FLOAT_MANTISSA)?);
    let exponent = sequence.shr(exponent_bits, constant(FLOAT_MANTISSA_BITS)?);
    let half_sign = sequence.shr(sign, constant(SIGN_SHIFT)?);

    let mantissa_nonzero =
        sequence.compare(IntegerPredicate::NotEqual, mantissa.clone(), constant(0)?);
    let nan_bit = sequence.select(mantissa_nonzero, constant(HALF_QUIET_NAN)?, constant(0)?);
    let nan_payload = sequence.shr(mantissa.clone(), constant(MANTISSA_SHIFT)?);
    let special = sequence.or(half_sign.clone(), constant(HALF_EXPONENT)?);
    let special = sequence.or(special, nan_bit);
    let special = sequence.or(special, nan_payload);
    let overflow = sequence.or(half_sign.clone(), constant(HALF_EXPONENT)?);

    // Round to nearest even: up when the first dropped bit is set and either
    // a later dropped bit or the kept lowest bit is.
    let round_bit = 1 << (MANTISSA_SHIFT - 1);
    let round_tail = (round_bit - 1) | (1 << MANTISSA_SHIFT);
    let normal_exponent = sequence.sub(exponent.clone(), constant(BIAS_DIFFERENCE)?);
    let normal_exponent = sequence.shl(normal_exponent, constant(HALF_MANTISSA_BITS)?);
    let normal_mantissa = sequence.shr(mantissa.clone(), constant(MANTISSA_SHIFT)?);
    let normal_round_bit = sequence.and(mantissa.clone(), constant(round_bit)?);
    let normal_round_tail = sequence.and(mantissa.clone(), constant(round_tail)?);
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
    let sub_exponent = sequence.clamp(
        exponent.clone(),
        constant(UNDERFLOW_EXPONENT)?,
        constant(BIAS_DIFFERENCE)?,
    );
    let hidden_mantissa = sequence.or(mantissa, constant(FLOAT_HIDDEN_BIT)?);
    let sub_shift = sequence.sub(constant(SUBNORMAL_SHIFT_BASE)?, sub_exponent.clone());
    let sub_mantissa = sequence.shr(hidden_mantissa.clone(), sub_shift);
    let sub_round_shift = sequence.sub(constant(SUBNORMAL_SHIFT_BASE - 1)?, sub_exponent);
    let sub_round_bit = sequence.shl(constant(1)?, sub_round_shift);
    // The bits below the round bit and the kept bit above it: 3r - 1.
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
        constant(UNDERFLOW_EXPONENT)?,
    );
    let small = sequence.select(underflows, half_sign.clone(), subnormal);
    let is_small = sequence.compare(
        IntegerPredicate::UnsignedLessEqual,
        exponent.clone(),
        constant(BIAS_DIFFERENCE)?,
    );
    let finite = sequence.select(is_small, small, normal);
    let overflows = sequence.compare(
        IntegerPredicate::UnsignedGreaterEqual,
        exponent.clone(),
        constant(OVERFLOW_EXPONENT)?,
    );
    let finite = sequence.select(overflows, overflow, finite);
    let is_special = sequence.compare(
        IntegerPredicate::Equal,
        exponent,
        constant(FLOAT_SPECIAL_EXPONENT)?,
    );
    let half_bits = sequence.select(is_special, special, finite);

    // binary16 bits -> float. This is exact and does not rely on Vulkan's
    // optional half-denormal preservation mode.
    let half_sign = sequence.and(half_bits.clone(), constant(HALF_SIGN)?);
    let half_sign = sequence.shl(half_sign, constant(SIGN_SHIFT)?);
    let half_exponent = sequence.and(half_bits.clone(), constant(HALF_EXPONENT)?);
    let half_mantissa = sequence.and(half_bits.clone(), constant(HALF_MANTISSA)?);
    let magnitude = sequence.and(half_bits, constant(HALF_MAGNITUDE)?);
    let is_zero = sequence.compare(IntegerPredicate::Equal, magnitude, constant(0)?);
    let is_special = sequence.compare(
        IntegerPredicate::Equal,
        half_exponent.clone(),
        constant(HALF_EXPONENT)?,
    );
    let is_subnormal =
        sequence.compare(IntegerPredicate::Equal, half_exponent.clone(), constant(0)?);

    let normal_exponent = sequence.shr(half_exponent, constant(HALF_MANTISSA_BITS)?);
    let normal_exponent = sequence.add(normal_exponent, constant(BIAS_DIFFERENCE)?);
    let normal_exponent = sequence.shl(normal_exponent, constant(FLOAT_MANTISSA_BITS)?);
    let normal_mantissa = sequence.shl(half_mantissa.clone(), constant(MANTISSA_SHIFT)?);
    let normal = sequence.or(half_sign.clone(), normal_exponent);
    let normal = sequence.or(normal, normal_mantissa);

    // A subnormal's highest set bit becomes the hidden bit. A zero mantissa
    // finds no bit, but that case is selected away and the shift stays < 32.
    let highest = sequence.unary(
        sequence.uint.clone(),
        IntegerUnaryOp::FindMostSignificantBitUnsigned,
        half_mantissa.clone(),
    );
    let adjustment = sequence.sub(constant(HALF_MANTISSA_BITS - 1)?, highest);
    let sub_exponent = sequence.sub(constant(BIAS_DIFFERENCE)?, adjustment.clone());
    let sub_exponent = sequence.shl(sub_exponent, constant(FLOAT_MANTISSA_BITS)?);
    let sub_shift = sequence.add(constant(MANTISSA_SHIFT + 1)?, adjustment);
    let sub_mantissa = sequence.shl(half_mantissa.clone(), sub_shift);
    let sub_mantissa = sequence.and(sub_mantissa, constant(FLOAT_MANTISSA)?);
    let subnormal = sequence.or(half_sign.clone(), sub_exponent);
    let subnormal = sequence.or(subnormal, sub_mantissa);

    let mantissa_zero =
        sequence.compare(IntegerPredicate::Equal, half_mantissa.clone(), constant(0)?);
    let special_payload = sequence.shl(half_mantissa, constant(MANTISSA_SHIFT)?);
    let special_payload = sequence.or(constant(FLOAT_QUIET_NAN)?, special_payload);
    let special_mantissa = sequence.select(mantissa_zero, constant(0)?, special_payload);
    let special = sequence.or(half_sign.clone(), constant(FLOAT_EXPONENT)?);
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
