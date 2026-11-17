use super::*;

/// Recognize compiler support functions whose bodies have a direct Metal IR
/// operation. SPIRV-Cross emits this helper when translating GLSL's unsigned
/// `findMSB`; retaining all non-entry LLVM functions just for it would obscure
/// the operation the source shader requested.
pub(super) fn known_helper(
    name: &str,
    arguments: &[Operand],
) -> Option<Result<InstructionKind, TranslateError>> {
    match (name, arguments) {
        ("_Z11spvFindUMSBIjET_S0_", [value]) => Some(Ok(InstructionKind::IntegerUnary {
            op: IntegerUnaryOp::FindMostSignificantBitUnsigned,
            value: value.clone(),
        })),
        ("_Z11spvFindUMSBIjET_S0_", _) => Some(Err(semantic(format!(
            "LLVM helper {name} takes one operand"
        )))),
        _ => None,
    }
}

/// Decode an AIR intrinsic name into a Metal IR operation.
///
/// AIR names are `air.<operation>[.<type suffix>]*`, where the operation may
/// carry a `fast_` or `precise_` prefix selecting the math library's accuracy.
/// Matching on the operation rather than the full spelling keeps every vector
/// and scalar width on one lowering path.
///
/// FIXME (ACCURACY): `fast_` and `precise_` lower identically. SPIR-V has no
/// portable way to request Metal's relaxed accuracy, and the difference is
/// below the precision most shaders depend on.
pub(super) fn air_intrinsic(
    name: &str,
    arguments: &[Operand],
) -> Result<InstructionKind, TranslateError> {
    let unsupported = || semantic(format!("LLVM intrinsic {name} needs Metal IR lowering"));
    let mut parts = name.split('.');
    if parts.next() != Some("air") {
        return Err(unsupported());
    }
    let operation = parts.next().ok_or_else(unsupported)?;
    let operation = operation
        .strip_prefix("fast_")
        .or_else(|| operation.strip_prefix("precise_"))
        .unwrap_or(operation);
    // AIR spells an integer operation's signedness in its first type suffix,
    // for instance `air.min.u.i32` and `air.abs.s.v3i32`.
    let signedness = parts
        .clone()
        .next()
        .filter(|part| matches!(*part, "s" | "u"));
    if operation == "convert" {
        // `air.convert.<result kind>.<result type>.<source kind>.<source type>`
        let suffix: Vec<_> = parts.collect();
        let [result_kind, _, source_kind, _] = suffix.as_slice() else {
            return Err(unsupported());
        };
        let op = match (*result_kind, *source_kind) {
            ("f", "u") => ConvertOp::UnsignedToFloat,
            ("f", "s") => ConvertOp::SignedToFloat,
            ("u", "f") => ConvertOp::FloatToUnsigned,
            ("s", "f") => ConvertOp::FloatToSigned,
            // Metal spells `half(x)` and `float(h)` as a conversion between
            // two floating kinds; the type suffixes say which widths.
            ("f", "f") => ConvertOp::FloatResize,
            _ => return Err(unsupported()),
        };
        let [value] = arguments else {
            return Err(semantic(format!("LLVM intrinsic {name} takes one operand")));
        };
        return Ok(InstructionKind::Convert {
            op,
            value: value.clone(),
        });
    }
    if operation == "wg" && parts.clone().next() == Some("barrier") {
        let [memory, scope] = arguments else {
            return Err(semantic(format!(
                "LLVM intrinsic {name} takes two operands"
            )));
        };
        return Ok(InstructionKind::Barrier {
            scope: barrier_scope(scope, name)?,
            memory: barrier_memory(memory, name)?,
        });
    }
    if operation == "atomic" {
        return air_atomic(name, parts.collect::<Vec<_>>().as_slice(), arguments);
    }
    // `is_function_constant_defined(c)`, which AIR asks of the global which
    // holds `c`. Specialization answers it.
    if operation == "is_function_constant_defined" {
        let [constant] = arguments else {
            return Err(semantic(format!("LLVM intrinsic {name} takes one operand")));
        };
        return Ok(InstructionKind::FunctionConstantDefined {
            constant: constant.clone(),
        });
    }
    if let Some(kind) = air_texture_intrinsic(operation, arguments)? {
        return Ok(kind);
    }
    let unary = |op| match arguments {
        [value] => Ok(InstructionKind::FloatUnary {
            op,
            value: value.clone(),
        }),
        _ => Err(semantic(format!("LLVM intrinsic {name} takes one operand"))),
    };
    let binary = |op| match arguments {
        [lhs, rhs] => Ok(InstructionKind::FloatBinary {
            op,
            lhs: lhs.clone(),
            rhs: rhs.clone(),
        }),
        _ => Err(semantic(format!(
            "LLVM intrinsic {name} takes two operands"
        ))),
    };
    if let Some(signedness) = signedness {
        let signed = signedness == "s";
        let integer_binary = |op| match arguments {
            [lhs, rhs] => Ok(InstructionKind::IntegerBinary {
                op,
                lhs: lhs.clone(),
                rhs: rhs.clone(),
            }),
            _ => Err(semantic(format!(
                "LLVM intrinsic {name} takes two operands"
            ))),
        };
        let integer_unary = |op| match arguments {
            [value] => Ok(InstructionKind::IntegerUnary {
                op,
                value: value.clone(),
            }),
            _ => Err(semantic(format!("LLVM intrinsic {name} takes one operand"))),
        };
        match operation {
            "abs" => return integer_unary(IntegerUnaryOp::AbsoluteSigned),
            "sign" => return integer_unary(IntegerUnaryOp::SignSigned),
            "min" if signed => return integer_binary(IntegerBinaryOp::MinimumSigned),
            "min" => return integer_binary(IntegerBinaryOp::MinimumUnsigned),
            "max" if signed => return integer_binary(IntegerBinaryOp::MaximumSigned),
            "max" => return integer_binary(IntegerBinaryOp::MaximumUnsigned),
            "clamp" => {
                return match arguments {
                    [value, min, max] => Ok(InstructionKind::IntegerClamp {
                        signed,
                        value: value.clone(),
                        min: min.clone(),
                        max: max.clone(),
                    }),
                    _ => Err(semantic(format!(
                        "LLVM intrinsic {name} takes three operands"
                    ))),
                };
            }
            _ => {}
        }
    }
    match operation {
        // The second operand is LLVM's "zero is undefined" hint. This
        // lowering answers the width for zero whether or not the caller would
        // have accepted anything, so the hint changes nothing.
        "clz" => match arguments {
            [value] | [value, _] => Ok(InstructionKind::IntegerUnary {
                op: IntegerUnaryOp::CountLeadingZeros,
                value: value.clone(),
            }),
            _ => Err(semantic(format!("LLVM intrinsic {name} takes one operand"))),
        },
        "ctz" => match arguments {
            [value] | [value, _] => Ok(InstructionKind::IntegerUnary {
                op: IntegerUnaryOp::CountTrailingZeros,
                value: value.clone(),
            }),
            _ => Err(semantic(format!("LLVM intrinsic {name} takes one operand"))),
        },
        "popcount" => match arguments {
            [value] => Ok(InstructionKind::IntegerUnary {
                op: IntegerUnaryOp::PopulationCount,
                value: value.clone(),
            }),
            _ => Err(semantic(format!("LLVM intrinsic {name} takes one operand"))),
        },
        "fabs" => unary(FloatUnaryOp::Absolute),
        "sign" => unary(FloatUnaryOp::Sign),
        "sqrt" => unary(FloatUnaryOp::Sqrt),
        "rsqrt" => unary(FloatUnaryOp::InverseSqrt),
        "exp" => unary(FloatUnaryOp::Exp),
        "exp2" => unary(FloatUnaryOp::Exp2),
        "log" => unary(FloatUnaryOp::Log),
        "log2" => unary(FloatUnaryOp::Log2),
        "sin" => unary(FloatUnaryOp::Sin),
        "cos" => unary(FloatUnaryOp::Cos),
        "tan" => unary(FloatUnaryOp::Tan),
        "asin" => unary(FloatUnaryOp::Asin),
        "acos" => unary(FloatUnaryOp::Acos),
        "atan" => unary(FloatUnaryOp::Atan),
        "floor" => unary(FloatUnaryOp::Floor),
        "ceil" => unary(FloatUnaryOp::Ceil),
        "round" => unary(FloatUnaryOp::Round),
        "rint" => unary(FloatUnaryOp::RoundEven),
        "trunc" => unary(FloatUnaryOp::Truncate),
        "fract" => unary(FloatUnaryOp::Fract),
        "dfdx" => unary(FloatUnaryOp::DerivativeX),
        "dfdy" => unary(FloatUnaryOp::DerivativeY),
        "fwidth" => unary(FloatUnaryOp::DerivativeWidth),
        "fmin" => binary(FloatBinaryOp::Minimum),
        "fmax" => binary(FloatBinaryOp::Maximum),
        "pow" | "powr" => binary(FloatBinaryOp::Power),
        "atan2" => binary(FloatBinaryOp::Atan2),
        "step" => binary(FloatBinaryOp::Step),
        "fmod" => binary(FloatBinaryOp::Remainder),
        "dot" => match arguments {
            [lhs, rhs] => Ok(InstructionKind::FloatDot {
                lhs: lhs.clone(),
                rhs: rhs.clone(),
            }),
            _ => Err(semantic(format!(
                "LLVM intrinsic {name} takes two operands"
            ))),
        },
        "clamp" => match arguments {
            [value, min, max] => Ok(InstructionKind::FloatClamp {
                value: value.clone(),
                min: min.clone(),
                max: max.clone(),
            }),
            _ => Err(semantic(format!(
                "LLVM intrinsic {name} takes three operands"
            ))),
        },
        "mix" => match arguments {
            [first, second, weight] => Ok(InstructionKind::FloatMix {
                first: first.clone(),
                second: second.clone(),
                weight: weight.clone(),
            }),
            _ => Err(semantic(format!(
                "LLVM intrinsic {name} takes three operands"
            ))),
        },
        "any" => match arguments {
            [vector] => Ok(InstructionKind::Any {
                vector: vector.clone(),
            }),
            _ => Err(semantic(format!("LLVM intrinsic {name} takes one operand"))),
        },
        "all" => match arguments {
            [vector] => Ok(InstructionKind::All {
                vector: vector.clone(),
            }),
            _ => Err(semantic(format!("LLVM intrinsic {name} takes one operand"))),
        },
        "discard_fragment" => Ok(InstructionKind::DiscardFragment),
        "fma" => match arguments {
            [first, second, addend] => Ok(InstructionKind::FloatFma {
                first: first.clone(),
                second: second.clone(),
                addend: addend.clone(),
            }),
            _ => Err(semantic(format!(
                "LLVM intrinsic {name} takes three operands"
            ))),
        },
        _ => Err(unsupported()),
    }
}

/// The constant an AIR flag or enumerator argument must be.
///
/// Metal spells all four of a barrier's and an atomic's qualifiers as
/// compile-time enumerators, so a run-time value here is a shader this
/// translator has misread rather than one it should approximate.
pub(super) fn air_enumerator(
    operand: &Operand,
    name: &str,
    what: &str,
) -> Result<u64, TranslateError> {
    match operand {
        Operand::Integer { value, .. } => Ok(*value),
        _ => Err(semantic(format!(
            "LLVM intrinsic {name} has a run-time {what}, which Metal spells as a constant"
        ))),
    }
}

/// Metal's `mem_flags`, which say which address spaces a barrier orders.
pub(super) fn barrier_memory(
    operand: &Operand,
    name: &str,
) -> Result<BarrierMemory, TranslateError> {
    let flags = air_enumerator(operand, name, "memory flag set")?;
    // `mem_none` is 0; the rest are one bit each. `mem_threadgroup_imageblock`
    // and `mem_object_data` are threadgroup memory as far as ordering goes.
    const DEVICE: u64 = 1;
    const THREADGROUP: u64 = 2;
    const TEXTURE: u64 = 4;
    const IMAGEBLOCK: u64 = 8;
    const OBJECT_DATA: u64 = 16;
    let known = DEVICE | THREADGROUP | TEXTURE | IMAGEBLOCK | OBJECT_DATA;
    if flags & !known != 0 {
        return Err(semantic(format!(
            "LLVM intrinsic {name} names unknown Metal memory flags {flags}"
        )));
    }
    Ok(BarrierMemory {
        device: flags & DEVICE != 0,
        threadgroup: flags & (THREADGROUP | IMAGEBLOCK | OBJECT_DATA) != 0,
        texture: flags & TEXTURE != 0,
    })
}

// Metal's `__METAL_MEMORY_SCOPE_*__` values, as its compiler defines them.
const MEMORY_SCOPE_THREAD: u64 = 0;
const MEMORY_SCOPE_THREADGROUP: u64 = 1;
const MEMORY_SCOPE_DEVICE: u64 = 2;
const MEMORY_SCOPE_SYSTEM: u64 = 3;
const MEMORY_SCOPE_SIMDGROUP: u64 = 4;

/// Metal's `thread_scope`, which says what a barrier makes wait and how far an
/// atomic's ordering reaches.
///
/// A plain `threadgroup_barrier(mem_threadgroup)` is `air.wg.barrier(2, 1)`,
/// and an `atomic_fetch_add_explicit` on a `device` pointer - whose scope
/// defaults to the device - is `air.atomic.global.add(..., 2)`.
pub(super) fn barrier_scope(operand: &Operand, name: &str) -> Result<BarrierScope, TranslateError> {
    match air_enumerator(operand, name, "scope")? {
        MEMORY_SCOPE_THREAD => Ok(BarrierScope::Invocation),
        MEMORY_SCOPE_SIMDGROUP => Ok(BarrierScope::Simdgroup),
        MEMORY_SCOPE_THREADGROUP => Ok(BarrierScope::Threadgroup),
        // The system scope reaches further than one device, and this runtime
        // drives one. Widening it costs ordering, not correctness.
        MEMORY_SCOPE_DEVICE | MEMORY_SCOPE_SYSTEM => Ok(BarrierScope::Device),
        scope => Err(semantic(format!(
            "LLVM intrinsic {name} names unknown Metal memory scope {scope}"
        ))),
    }
}

/// `air.atomic.<space>.<operation>[.<signedness>].<type>` and its operands.
///
/// The pointer's own address space says which memory the operation is in, so
/// the `global`/`local` suffix is checked rather than believed.
pub(super) fn air_atomic(
    name: &str,
    suffix: &[&str],
    arguments: &[Operand],
) -> Result<InstructionKind, TranslateError> {
    let unsupported = || semantic(format!("LLVM intrinsic {name} needs Metal IR lowering"));
    let [space, operation, rest @ ..] = suffix else {
        return Err(unsupported());
    };
    if !matches!(*space, "global" | "local") {
        return Err(unsupported());
    }
    let signed = matches!(rest.first(), Some(&"s"));
    let op = match *operation {
        "add" => AtomicOp::Add,
        "sub" => AtomicOp::Subtract,
        "min" if signed => AtomicOp::MinimumSigned,
        "min" => AtomicOp::MinimumUnsigned,
        "max" if signed => AtomicOp::MaximumSigned,
        "max" => AtomicOp::MaximumUnsigned,
        "and" => AtomicOp::And,
        "or" => AtomicOp::Or,
        "xor" => AtomicOp::Xor,
        "xchg" => AtomicOp::Exchange,
        _ => return Err(unsupported()),
    };
    let [pointer, value, order, scope] = arguments else {
        return Err(semantic(format!(
            "LLVM intrinsic {name} takes four operands"
        )));
    };
    Ok(InstructionKind::Atomic {
        op,
        pointer: pointer.clone(),
        value: value.clone(),
        order: memory_order(order, name)?,
        scope: barrier_scope(scope, name)?,
    })
}

// Metal's `__METAL_MEMORY_ORDER_*__` values, which follow C11's
// `__ATOMIC_*` numbering; Metal has no `memory_order_consume` (1).
const MEMORY_ORDER_RELAXED: u64 = 0;
const MEMORY_ORDER_ACQUIRE: u64 = 2;
const MEMORY_ORDER_RELEASE: u64 = 3;
const MEMORY_ORDER_ACQ_REL: u64 = 4;
const MEMORY_ORDER_SEQ_CST: u64 = 5;

/// Metal's `memory_order` enumeration.
pub(super) fn memory_order(operand: &Operand, name: &str) -> Result<MemoryOrder, TranslateError> {
    match air_enumerator(operand, name, "memory order")? {
        MEMORY_ORDER_RELAXED => Ok(MemoryOrder::Relaxed),
        MEMORY_ORDER_ACQUIRE => Ok(MemoryOrder::Acquire),
        MEMORY_ORDER_RELEASE => Ok(MemoryOrder::Release),
        MEMORY_ORDER_ACQ_REL => Ok(MemoryOrder::AcquireRelease),
        MEMORY_ORDER_SEQ_CST => Ok(MemoryOrder::SequentiallyConsistent),
        order => Err(semantic(format!(
            "LLVM intrinsic {name} names unknown Metal memory order {order}"
        ))),
    }
}

/// Lower the AIR texture intrinsic family.
///
/// The operation name carries the access and the texture shape, for instance
/// `sample_texture_2d_array` or `read_texture_2d_ms`. Metal's optional
/// sampling arguments are passed as a flag followed by the value; only a
/// constant flag is accepted, because a runtime choice between implicit and
/// explicit level of detail has no SPIR-V form.
///
/// A `_grad` suffix is Metal's `gradient2d(dPdx, dPdy)`: the derivatives take
/// the place of the level-of-detail flag and value, and follow the array
/// index rather than preceding it.
pub(super) fn air_texture_intrinsic(
    operation: &str,
    arguments: &[Operand],
) -> Result<Option<InstructionKind>, TranslateError> {
    let Some((access, shape)) = operation.split_once("_texture") else {
        return Ok(None);
    };
    let shape = shape.strip_prefix('_').unwrap_or(shape);
    let (shape, gradient) = match shape.strip_suffix("_grad") {
        Some(shape) => (shape, true),
        None => (shape, false),
    };
    let array = shape.ends_with("_array");
    let multisampled = shape.ends_with("_ms");
    let malformed = || {
        semantic(format!(
            "AIR intrinsic air.{operation} has unexpected operands"
        ))
    };
    match access {
        "sample" | "gather" => {
            // (texture, sampler, coordinate, [array index], offset flag,
            //  [offset], level flag, level). A cube texture takes no offset.
            let [texture, sampler, coordinate, rest @ ..] = arguments else {
                return Err(malformed());
            };
            let (array_index, rest) = if array {
                let [index, rest @ ..] = rest else {
                    return Err(malformed());
                };
                (Some(index.clone()), rest)
            } else {
                (None, rest)
            };
            let (gradient, rest) = match (gradient, rest) {
                (false, rest) => (None, rest),
                (true, [dx, dy, rest @ ..]) => (Some(Box::new((dx.clone(), dy.clone()))), rest),
                (true, _) => return Err(malformed()),
            };
            let (offset, rest) = match rest {
                [flag, offset, rest @ ..] if shape.starts_with("2d") || shape.starts_with("3d") => {
                    (optional(flag, offset)?, rest)
                }
                rest => (None, rest),
            };
            if access == "gather" {
                let (component, trailing) = match rest {
                    [component] => (component, None),
                    [component, trailing] => (component, Some(trailing)),
                    _ => return Err(malformed()),
                };
                if let Some(trailing) = trailing
                    && air_enumerator(trailing, operation, "gather qualifier")? != 0
                {
                    return Err(semantic(format!(
                        "AIR intrinsic air.{operation} has a nonzero gather qualifier"
                    )));
                }
                let component = air_enumerator(component, operation, "gather component")?;
                let component = u32::try_from(component)
                    .ok()
                    .filter(|component| *component < 4)
                    .ok_or_else(|| {
                        semantic(format!(
                            "AIR intrinsic air.{operation} names gather component {component}"
                        ))
                    })?;
                return Ok(Some(InstructionKind::TextureSample {
                    texture: texture.clone(),
                    sampler: sampler.clone(),
                    coordinate: coordinate.clone(),
                    array_index,
                    offset,
                    lod: None,
                    gradient: None,
                    bias: None,
                    gather: Some(component),
                    compare: None,
                }));
            }
            // AIR from Metal 3.1 tools appends one more integer to a sample.
            // Every plain `sample` spells it 0; any other value selects
            // something this lowering does not know, so it is refused.
            let rest = match rest {
                [head @ .., Operand::Integer { value, .. }] => match value {
                    0 => head,
                    value => {
                        return Err(semantic(format!(
                            "AIR intrinsic air.{operation} has trailing operand {value}, which needs Metal IR lowering"
                        )));
                    }
                },
                rest => rest,
            };
            // A gradient sample carries no level-of-detail flag: the
            // derivatives are the level.
            let (lod, bias) = match (gradient.is_some(), rest) {
                (true, []) => (None, None),
                (false, [flag, value]) => level_or_bias(flag, value)?,
                _ => return Err(malformed()),
            };
            Ok(Some(InstructionKind::TextureSample {
                texture: texture.clone(),
                sampler: sampler.clone(),
                coordinate: coordinate.clone(),
                array_index,
                offset,
                lod,
                gradient,
                bias,
                gather: None,
                compare: None,
            }))
        }
        "read" => {
            // (texture, coordinate, [array index], level or sample index).
            let [texture, coordinate, rest @ ..] = arguments else {
                return Err(malformed());
            };
            let (array_index, rest) = if array {
                let [index, rest @ ..] = rest else {
                    return Err(malformed());
                };
                (Some(index.clone()), rest)
            } else {
                (None, rest)
            };
            let [selector] = rest else {
                return Err(malformed());
            };
            Ok(Some(InstructionKind::TextureRead {
                texture: texture.clone(),
                coordinate: coordinate.clone(),
                array_index,
                lod: (!multisampled).then(|| selector.clone()),
                sample: multisampled.then(|| selector.clone()),
            }))
        }
        "write" => {
            let [texture, coordinate, rest @ ..] = arguments else {
                return Err(malformed());
            };
            let (array_index, rest) = if array {
                let [index, rest @ ..] = rest else {
                    return Err(malformed());
                };
                (Some(index.clone()), rest)
            } else {
                (None, rest)
            };
            // Recent AIR adds a trailing write qualifier after the mip
            // level. Neither changes the storage-image write SPIR-V emits.
            let value = match rest {
                [value, _] | [value, _, _] => value,
                _ => return Err(malformed()),
            };
            Ok(Some(InstructionKind::TextureWrite {
                texture: texture.clone(),
                coordinate: coordinate.clone(),
                array_index,
                value: value.clone(),
            }))
        }
        "get_width" | "get_height" | "get_depth" => {
            let [texture, lod @ ..] = arguments else {
                return Err(malformed());
            };
            Ok(Some(InstructionKind::TextureQuerySize {
                texture: texture.clone(),
                lod: lod.first().cloned(),
                component: match access {
                    "get_width" => 0,
                    "get_height" => 1,
                    _ => 2,
                },
            }))
        }
        _ => Ok(None),
    }
}

/// AIR passes `level(l)` as `i1 true, float l` and `bias(b)` as `i1 false,
/// float b`; a plain `sample` is a bias of zero, which is left implicit so
/// that it emits exactly the unbiased sample. Like `optional`, the flag must
/// be a constant.
pub(super) fn level_or_bias(
    flag: &Operand,
    value: &Operand,
) -> Result<(Option<Operand>, Option<Operand>), TranslateError> {
    match flag {
        Operand::Bool(true) => Ok((Some(value.clone()), None)),
        Operand::Bool(false) => match value {
            Operand::Float { bits, .. } if f32::from_bits(*bits as u32) == 0.0 => Ok((None, None)),
            bias => Ok((None, Some(bias.clone()))),
        },
        other => Err(semantic(format!(
            "AIR texture level-of-detail selector is not a constant flag: {other:?}"
        ))),
    }
}

/// AIR passes an optional sampling argument as a boolean flag and a value.
/// The flag must be a constant: SPIR-V chooses the sampling opcode at
/// translation time, not at run time.
pub(super) fn optional(flag: &Operand, value: &Operand) -> Result<Option<Operand>, TranslateError> {
    match flag {
        Operand::Bool(false) => Ok(None),
        Operand::Bool(true) => Ok(Some(value.clone())),
        other => Err(semantic(format!(
            "AIR texture sampling option is not a constant flag: {other:?}"
        ))),
    }
}
