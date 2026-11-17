use super::*;

/// Apple's own output fixes AIR's `memory_scope` numbering: a plain
/// `threadgroup_barrier(mem_flags::mem_threadgroup)` is
/// `air.wg.barrier(2, 1)`. Reading the 1 as a SIMD-group leaves a
/// threadgroup wider than one SIMD-group with no barrier at all.
#[test]
fn a_threadgroup_barrier_names_the_threadgroup_scope() {
    let kind = air_intrinsic("air.wg.barrier", &[Operand::u32(2), Operand::u32(1)])
        .expect("the barrier is lowered");
    assert_eq!(
        kind,
        InstructionKind::Barrier {
            scope: BarrierScope::Threadgroup,
            memory: BarrierMemory {
                threadgroup: true,
                ..BarrierMemory::default()
            },
        }
    );
}

/// The same numbering read from the other end: an atomic on a `device`
/// pointer defaults to `memory_scope_device`, which Apple writes as 2.
#[test]
fn a_device_atomic_names_the_device_scope() {
    let kind = air_intrinsic(
        "air.atomic.global.add.u.i32",
        &[
            Operand::Value(ValueId(0)),
            Operand::u32(1),
            Operand::u32(0),
            Operand::u32(2),
        ],
    )
    .expect("the atomic is lowered");
    let InstructionKind::Atomic { scope, order, .. } = kind else {
        panic!("an atomic read-modify-write");
    };
    assert_eq!(scope, BarrierScope::Device);
    assert_eq!(order, MemoryOrder::Relaxed);
}

#[test]
fn a_work_item_memory_scope_stays_with_one_invocation() {
    let kind = air_intrinsic(
        "air.atomic.local.add.u.i32",
        &[
            Operand::Value(ValueId(0)),
            Operand::u32(1),
            Operand::u32(0),
            Operand::u32(0),
        ],
    )
    .expect("the atomic is lowered");
    let InstructionKind::Atomic { scope, .. } = kind else {
        panic!("an atomic read-modify-write");
    };
    assert_eq!(scope, BarrierScope::Invocation);
}

/// Metal's compiler defines `__METAL_MEMORY_ORDER_*__` with C11's
/// `__ATOMIC_*` values and leaves out `consume`.
#[test]
fn air_atomic_memory_orders_follow_metal_enumerators() {
    for (enumerator, expected) in [
        (0, MemoryOrder::Relaxed),
        (2, MemoryOrder::Acquire),
        (3, MemoryOrder::Release),
        (4, MemoryOrder::AcquireRelease),
        (5, MemoryOrder::SequentiallyConsistent),
    ] {
        let kind = air_intrinsic(
            "air.atomic.global.add.u.i32",
            &[
                Operand::Value(ValueId(0)),
                Operand::u32(1),
                Operand::u32(enumerator),
                Operand::u32(2),
            ],
        )
        .expect("the atomic is lowered");
        let InstructionKind::Atomic { order, .. } = kind else {
            panic!("an atomic read-modify-write");
        };
        assert_eq!(order, expected);
    }
    assert!(
        air_intrinsic(
            "air.atomic.global.add.u.i32",
            &[
                Operand::Value(ValueId(0)),
                Operand::u32(1),
                Operand::u32(1),
                Operand::u32(2),
            ],
        )
        .is_err()
    );
}

/// `__METAL_MEMORY_SCOPE_SIMDGROUP__` is 4, after the system scope.
#[test]
fn a_simdgroup_memory_scope_names_the_simdgroup() {
    let kind = air_intrinsic(
        "air.atomic.local.add.u.i32",
        &[
            Operand::Value(ValueId(0)),
            Operand::u32(1),
            Operand::u32(0),
            Operand::u32(4),
        ],
    )
    .expect("the atomic is lowered");
    let InstructionKind::Atomic { scope, .. } = kind else {
        panic!("an atomic read-modify-write");
    };
    assert_eq!(scope, BarrierScope::Simdgroup);
}

#[test]
fn a_modern_texture_write_accepts_its_trailing_qualifier() {
    let texture = Operand::Value(ValueId(0));
    let coordinate = Operand::Value(ValueId(1));
    let value = Operand::Value(ValueId(2));
    let kind = air_intrinsic(
        "air.write_texture_3d.v4f32",
        &[
            texture.clone(),
            coordinate.clone(),
            value.clone(),
            Operand::u32(0),
            Operand::u32(2),
        ],
    )
    .expect("the texture write is lowered");
    assert_eq!(
        kind,
        InstructionKind::TextureWrite {
            texture,
            coordinate,
            array_index: None,
            value,
        }
    );
}

/// `air.sample_texture_2d` as Roblox's pack spells it: the trailing flag picks
/// `level` (true) or `bias` (false), and a plain `sample` is a bias of zero.
fn sample_2d(flag: bool, value: f32) -> InstructionKind {
    let zero_offset = Operand::Composite {
        ty: Type::Vector {
            element: Box::new(Type::Int {
                bits: 32,
                signed: true,
            }),
            lanes: 2,
        },
        elements: vec![
            Operand::Integer {
                ty: Type::Int {
                    bits: 32,
                    signed: true,
                },
                value: 0,
            };
            2
        ],
    };
    air_intrinsic(
        "air.sample_texture_2d.v4f32",
        &[
            Operand::Value(ValueId(0)),
            Operand::Value(ValueId(1)),
            Operand::Value(ValueId(2)),
            Operand::Bool(true),
            zero_offset,
            Operand::Bool(flag),
            Operand::f32(value),
        ],
    )
    .expect("the sample is lowered")
}

/// SDK 14's AIR appends an integer to every sample; a plain one spells it 0.
#[test]
fn a_modern_sample_accepts_its_trailing_zero_and_refuses_others() {
    let sample = |trailing: u32| {
        air_intrinsic(
            "air.sample_texture_cube.v4f32",
            &[
                Operand::Value(ValueId(0)),
                Operand::Value(ValueId(1)),
                Operand::Value(ValueId(2)),
                Operand::Bool(true),
                Operand::f32(3.0),
                Operand::u32(trailing),
            ],
        )
    };
    let Ok(InstructionKind::TextureSample { lod, bias, .. }) = sample(0) else {
        panic!("a texture sample");
    };
    assert_eq!((lod, bias), (Some(Operand::f32(3.0)), None));
    assert!(sample(1).is_err());
}

#[test]
fn official_air_gathers_keep_each_literal_component() {
    let zero_offset = Operand::Composite {
        ty: Type::Vector {
            element: Box::new(Type::Int {
                bits: 32,
                signed: true,
            }),
            lanes: 2,
        },
        elements: vec![
            Operand::Integer {
                ty: Type::Int {
                    bits: 32,
                    signed: true,
                },
                value: 0,
            };
            2
        ],
    };
    let gather = |component: Operand, trailing: Option<u32>| {
        let mut arguments = vec![
            Operand::Value(ValueId(0)),
            Operand::Value(ValueId(1)),
            Operand::Value(ValueId(2)),
            Operand::Bool(true),
            zero_offset.clone(),
            component,
        ];
        arguments.extend(trailing.map(Operand::u32));
        air_intrinsic("air.gather_texture_2d.v4f32", &arguments)
    };
    for component in 0..4 {
        let InstructionKind::TextureSample {
            gather: selected, ..
        } = gather(Operand::u32(component), Some(0)).expect("an official modern gather")
        else {
            panic!("a texture gather");
        };
        assert_eq!(selected, Some(component));
    }
    let InstructionKind::TextureSample {
        gather: selected, ..
    } = gather(Operand::u32(2), None).expect("an older gather")
    else {
        panic!("a texture gather");
    };
    assert_eq!(selected, Some(2));
    assert!(gather(Operand::u32(4), Some(0)).is_err());
    assert!(gather(Operand::Value(ValueId(3)), Some(0)).is_err());
    assert!(gather(Operand::u32(0), Some(1)).is_err());
}

#[test]
fn a_false_level_flag_is_a_bias() {
    let InstructionKind::TextureSample { lod, bias, .. } = sample_2d(false, 7.0) else {
        panic!("a texture sample");
    };
    assert_eq!(lod, None);
    assert_eq!(bias, Some(Operand::f32(7.0)));
}

#[test]
fn a_plain_sample_carries_no_bias() {
    let InstructionKind::TextureSample { lod, bias, .. } = sample_2d(false, 0.0) else {
        panic!("a texture sample");
    };
    assert_eq!((lod, bias), (None, None));
}

#[test]
fn a_true_level_flag_is_an_explicit_level() {
    let InstructionKind::TextureSample { lod, bias, .. } = sample_2d(true, 0.0) else {
        panic!("a texture sample");
    };
    assert_eq!(lod, Some(Operand::f32(0.0)));
    assert_eq!(bias, None);
}

/// MSL's `round` goes away from zero at a half and `rint` to even; they are
/// different operations, not two spellings of one.
#[test]
fn round_and_rint_differ_at_halfway() {
    let value = Operand::f32(2.5);
    let unary = |name| match air_intrinsic(name, std::slice::from_ref(&value)) {
        Ok(InstructionKind::FloatUnary { op, .. }) => op,
        other => panic!("{name} lowered to {other:?}"),
    };
    assert_eq!(unary("air.fast_round.f32"), FloatUnaryOp::Round);
    assert_eq!(unary("air.fast_rint.f32"), FloatUnaryOp::RoundEven);
}

#[test]
fn recognizes_spirv_cross_unsigned_find_msb_helper() {
    let value = Operand::u32(7);
    let kind = known_helper("_Z11spvFindUMSBIjET_S0_", std::slice::from_ref(&value))
        .expect("the helper is recognized")
        .unwrap();
    assert_eq!(
        kind,
        InstructionKind::IntegerUnary {
            op: IntegerUnaryOp::FindMostSignificantBitUnsigned,
            value,
        }
    );
}

// Encode owned, unabbreviated records to exercise the real bitstream
// reader without an LLVM toolchain or proprietary shader fixtures.
fn records_block(id: u32, records: &[(u64, &[u64])]) -> BitcodeBlock {
    use crate::air::fixtures::{bitcode_block, block, record};
    bitcode_block(block(
        id,
        records
            .iter()
            .map(|(code, fields)| record(*code, fields))
            .collect(),
    ))
}

#[test]
fn decodes_both_constant_scopes_and_forward_aggregate_members() {
    let types = vec![
        DecodedType::Integer(32),
        DecodedType::Vector {
            element: 0,
            lanes: 2,
        },
    ];
    let mut module = records_block(8, &[(1, &[1])]);
    module.elements.push(BitcodeElement::Block(records_block(
        11,
        &[(1, &[0]), (2, &[])],
    )));
    let mut body = records_block(12, &[(1, &[1])]);
    body.elements.push(BitcodeElement::Block(records_block(
        11,
        &[(1, &[1]), (7, &[0, 3]), (1, &[0]), (4, &[2])],
    )));
    let table = ValueTable::decode(&module, &body, &types, &[1]).unwrap();
    assert_eq!(table.argument_start, 1);
    assert_eq!(table.values.len(), 4);
    assert_eq!(
        table.values[2].constant,
        Some(Constant::Aggregate(vec![0, 3]))
    );
    assert_eq!(table.shuffle_mask(2, &types).unwrap(), vec![0, 1]);
    body.elements.pop();
    body.elements.push(BitcodeElement::Block(records_block(
        11,
        &[(1, &[1]), (7, &[0, 99])],
    )));
    assert!(ValueTable::decode(&module, &body, &types, &[1]).is_err());
}

#[test]
fn lowers_extractelement_36_98_using_values_not_record_pattern() {
    let types = vec![
        DecodedType::Integer(32),
        DecodedType::Vector {
            element: 0,
            lanes: 4,
        },
    ];
    let mut values = vec![
        Value {
            ty: 0,
            constant: Some(Constant::Null)
        };
        105
    ];
    values[69] = Value {
        ty: 1,
        constant: None,
    };
    let table = ValueTable {
        globals: Vec::new(),
        relative: true,
        values,
        argument_start: 69,
    };
    let body = records_block(12, &[(1, &[1]), (6, &[36, 98]), (10, &[1])]);
    let function = table
        .lower_body(
            &body,
            &types,
            &Signature {
                names: &HashMap::new(),
                name: "extract".into(),
                parameter_types: &[1],
                result_type: 0,
                opaque_types: &HashMap::new(),
            },
        )
        .unwrap();
    assert_eq!(
        function.blocks[0].instructions[0].kind,
        InstructionKind::CompositeExtract {
            composite: Operand::Value(ValueId(0)),
            index: 0
        }
    );
    Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![function],
        entries: Vec::new(),
    }
    .verify()
    .unwrap();
}

/// LLVM's `switch` has no Metal IR form: it becomes one equality test
/// per case, each in the block the test before it falls through to, with
/// the phis of every target following the edge which now reaches it.
#[test]
fn a_switch_expands_into_a_chain_of_comparisons() {
    let types = vec![
        DecodedType::Integer(32),
        DecodedType::Integer(1),
        DecodedType::Void,
    ];
    let table = ValueTable {
        globals: Vec::new(),
        relative: true,
        values: vec![
            Value {
                ty: 0,
                constant: Some(Constant::Integer(7)),
            },
            Value {
                ty: 0,
                constant: Some(Constant::Integer(9)),
            },
        ],
        argument_start: 0,
    };
    // entry:  switch i32 7, label %block3 [ 7, %block1  9, %block2 ]
    // block1: br block3
    // block2: br block3
    // block3: %r = phi i32 [7, entry], [9, block1], [7, block2]; ret void
    let body = records_block(
        12,
        &[
            (1, &[4]),
            // SWITCH: [condition type, condition, default, (value, block)...]
            (12, &[0, 2, 3, 0, 1, 1, 2]),
            (11, &[3]),
            (11, &[3]),
            (16, &[0, 4, 0, 2, 1, 4, 2]),
            (10, &[]),
        ],
    );
    let function = table
        .lower_body(
            &body,
            &types,
            &Signature {
                names: &HashMap::new(),
                name: "choose".into(),
                parameter_types: &[],
                result_type: 2,
                opaque_types: &HashMap::new(),
            },
        )
        .unwrap();
    // Two cases: the first test stays in the switch's block and the
    // second gets a block of its own.
    assert_eq!(function.blocks.len(), 5);
    assert!(matches!(
        function.blocks[0].instructions.last().map(|i| &i.kind),
        Some(InstructionKind::IntegerCompare {
            predicate: IntegerPredicate::Equal,
            ..
        })
    ));
    let InstructionKind::Phi { incomings } = &function.blocks[3].instructions[0].kind else {
        panic!("the default block keeps its phi");
    };
    // The edge from the switch block now arrives from the last test.
    assert!(
        incomings
            .iter()
            .any(|(from, _)| *from == function.blocks[4].id),
        "the default's phi follows the chain's last block"
    );
    Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![function],
        entries: Vec::new(),
    }
    .verify()
    .unwrap();
}

/// A body whose blocks are laid out so that a comparison names the phi
/// which dominates it but appears later in the record stream.
#[test]
fn lowers_multiple_blocks_with_a_phi_and_a_forward_reference() {
    let types = vec![
        DecodedType::Integer(32),
        DecodedType::Integer(1),
        DecodedType::Void,
    ];
    let table = ValueTable {
        globals: Vec::new(),
        relative: true,
        values: vec![
            Value {
                ty: 0,
                constant: Some(Constant::Integer(0)),
            },
            Value {
                ty: 0,
                constant: Some(Constant::Integer(1)),
            },
        ],
        argument_start: 0,
    };
    // entry:  br block2
    // block1: %2 = icmp ult %3, 1; br %2, block2, block3
    // block2: %3 = phi i32 [0, entry], [1, block1]; br block1
    // block3: ret void
    let body = records_block(
        12,
        &[
            (1, &[4]),
            (11, &[2]),
            (28, &[4294967295, 0, 1, 36]),
            (11, &[2, 3, 1]),
            (16, &[0, 6, 0, 4, 1]),
            (11, &[1]),
            (10, &[]),
        ],
    );
    let function = table
        .lower_body(
            &body,
            &types,
            &Signature {
                names: &HashMap::new(),
                name: "loop".into(),
                parameter_types: &[],
                result_type: 2,
                opaque_types: &HashMap::new(),
            },
        )
        .unwrap();
    assert_eq!(function.blocks.len(), 4);
    let InstructionKind::Phi { .. } = function.blocks[2].instructions[0].kind else {
        panic!("the third block starts with a phi");
    };
    // The comparison and the phi must name one SSA value, not two.
    let InstructionKind::IntegerCompare { lhs, .. } = &function.blocks[1].instructions[0].kind
    else {
        panic!("the second block starts with a comparison");
    };
    assert_eq!(
        Some(lhs.clone()),
        function.blocks[2].instructions[0]
            .result
            .map(Operand::Value)
    );
    Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![function],
        entries: Vec::new(),
    }
    .verify()
    .unwrap();
}

/// A value named before its definition carries its type inline; both uses
/// must resolve to the one SSA definition.
#[test]
fn rejects_a_forward_reference_whose_declared_type_is_wrong() {
    let types = vec![
        DecodedType::Integer(32),
        DecodedType::Integer(1),
        DecodedType::Void,
    ];
    let table = ValueTable {
        globals: Vec::new(),
        relative: true,
        values: vec![Value {
            ty: 0,
            constant: Some(Constant::Integer(1)),
        }],
        argument_start: 0,
    };
    // The comparison declares its forward operand as `i1`, but the
    // instruction which defines it produces an `i32`.
    let body = records_block(
        12,
        &[
            (1, &[2]),
            (28, &[2, 1, 1, 36]),
            (11, &[1]),
            (2, &[2, 2, 0]),
            (10, &[]),
        ],
    );
    assert!(
        table
            .lower_body(
                &body,
                &types,
                &Signature {
                    names: &HashMap::new(),
                    name: "bad".into(),
                    parameter_types: &[],
                    result_type: 2,
                    opaque_types: &HashMap::new(),
                },
            )
            .is_err()
    );
}

#[test]
fn lowers_integer_resize_and_checks_its_direction() {
    let types = vec![DecodedType::Integer(32), DecodedType::Integer(64)];
    let make = || ValueTable {
        globals: Vec::new(),
        relative: true,
        values: vec![Value {
            ty: 0,
            constant: None,
        }],
        argument_start: 0,
    };
    let body = records_block(12, &[(1, &[1]), (3, &[1, 1, 1]), (10, &[1])]);
    let function = make()
        .lower_body(
            &body,
            &types,
            &Signature {
                names: &HashMap::new(),
                name: "extend".into(),
                parameter_types: &[0],
                result_type: 1,
                opaque_types: &HashMap::new(),
            },
        )
        .unwrap();
    Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![function],
        entries: Vec::new(),
    }
    .verify()
    .unwrap();
    let body = records_block(12, &[(1, &[1]), (3, &[1, 1, 0]), (10, &[1])]);
    assert!(
        make()
            .lower_body(
                &body,
                &types,
                &Signature {
                    names: &HashMap::new(),
                    name: "bad".into(),
                    parameter_types: &[0],
                    result_type: 1,
                    opaque_types: &HashMap::new(),
                }
            )
            .is_err()
    );
}

#[test]
fn lowers_a_modern_load_with_an_explicit_result_type() {
    let types = vec![
        DecodedType::Integer(32),
        DecodedType::Pointer {
            pointee: 0,
            address_space: 1,
        },
        DecodedType::Integer(64),
    ];
    let make = || ValueTable {
        globals: Vec::new(),
        relative: true,
        values: vec![Value {
            ty: 1,
            constant: None,
        }],
        argument_start: 0,
    };
    let body = records_block(12, &[(1, &[1]), (20, &[1, 0, 3, 0]), (10, &[1])]);
    let function = make()
        .lower_body(
            &body,
            &types,
            &Signature {
                names: &HashMap::new(),
                name: "load".into(),
                parameter_types: &[1],
                result_type: 0,
                opaque_types: &HashMap::new(),
            },
        )
        .unwrap();
    assert!(matches!(
        function.blocks[0].instructions[0].kind,
        InstructionKind::Load { .. }
    ));

    let bad = records_block(12, &[(1, &[1]), (20, &[1, 2, 3, 0]), (10, &[1])]);
    assert!(
        make()
            .lower_body(
                &bad,
                &types,
                &Signature {
                    names: &HashMap::new(),
                    name: "bad".into(),
                    parameter_types: &[1],
                    result_type: 0,
                    opaque_types: &HashMap::new(),
                },
            )
            .is_err()
    );
}

#[test]
fn extraction_uses_constant_contents_and_rejects_bad_indices() {
    let types = vec![
        DecodedType::Integer(32),
        DecodedType::Vector {
            element: 0,
            lanes: 4,
        },
        DecodedType::Float(32),
    ];
    let make = |ty, constant| ValueTable {
        globals: Vec::new(),
        relative: true,
        argument_start: 0,
        values: vec![
            Value {
                ty: 1,
                constant: None,
            },
            Value {
                ty,
                constant: Some(constant),
            },
        ],
    };
    let body = records_block(12, &[(1, &[1]), (6, &[2, 1]), (10, &[1])]);
    let function = make(0, Constant::Integer(2))
        .lower_body(
            &body,
            &types,
            &Signature {
                names: &HashMap::new(),
                name: "lane2".into(),
                parameter_types: &[1],
                result_type: 0,
                opaque_types: &HashMap::new(),
            },
        )
        .unwrap();
    assert!(matches!(
        function.blocks[0].instructions[0].kind,
        InstructionKind::CompositeExtract { index: 2, .. }
    ));
    assert!(
        make(0, Constant::Integer(4))
            .lower_body(
                &body,
                &types,
                &Signature {
                    names: &HashMap::new(),
                    name: "bad".into(),
                    parameter_types: &[1],
                    result_type: 0,
                    opaque_types: &HashMap::new(),
                }
            )
            .is_err()
    );
    assert!(
        make(2, Constant::Null)
            .lower_body(
                &body,
                &types,
                &Signature {
                    names: &HashMap::new(),
                    name: "bad".into(),
                    parameter_types: &[1],
                    result_type: 0,
                    opaque_types: &HashMap::new(),
                }
            )
            .is_err()
    );
}

#[test]
fn relative_references_include_module_constants() {
    let table = ValueTable {
        globals: Vec::new(),
        relative: true,
        values: vec![
            Value {
                ty: 0,
                constant: None
            };
            100
        ],
        argument_start: 0,
    };
    assert_eq!(table.operand_id(36).unwrap(), 64);
    assert_eq!(table.operand_id(98).unwrap(), 2);
    assert!(table.operand_id(101).is_err());
    assert!(table.operand_id(0).is_err());
    assert!(table.operand_id(1u64 << 32).is_err());
}

#[test]
fn absolute_references_do_not_subtract() {
    let table = ValueTable {
        globals: Vec::new(),
        relative: false,
        values: vec![
            Value {
                ty: 0,
                constant: None
            };
            100
        ],
        argument_start: 0,
    };
    assert_eq!(table.operand_id(36).unwrap(), 36);
}

#[test]
fn integer_constants_preserve_signed_bits() {
    assert_eq!(
        decode_constant(4, &[3], &DecodedType::Integer(32)).unwrap(),
        Constant::Integer(u32::MAX as u64)
    );
    assert_eq!(
        decode_constant(4, &[1], &DecodedType::Integer(64)).unwrap(),
        Constant::Integer(1u64 << 63)
    );
    assert!(decode_constant(4, &[2, 4], &DecodedType::Integer(32)).is_err());
}
