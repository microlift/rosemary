use super::*;
use crate::ir::{
    AddressSpace, Block, BlockId, EntryPoint, Instruction, InstructionKind, IntegerBinaryOp,
    IntegerPredicate, Parameter, Stage, Type,
};

fn u32_ptr(space: AddressSpace) -> Type {
    Type::Pointer {
        space,
        pointee: Box::new(Type::u32()),
    }
}

fn instruction(result: Option<u32>, kind: InstructionKind) -> Instruction {
    Instruction {
        result: result.map(ValueId),
        kind,
    }
}

fn block(id: u32, instructions: Vec<Instruction>, terminator: Terminator) -> Block {
    Block {
        id: BlockId(id),
        name: format!("b{id}"),
        instructions,
        terminator,
    }
}

fn module(function: Function) -> Module {
    let module = Module {
        static_initializers: Vec::new(),
        functions: vec![function],
        entries: vec![EntryPoint {
            early_fragment_tests: false,
            function: 0,
            stage: Stage::Kernel,
            resources: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
        globals: Vec::new(),
    };
    module.verify().expect("the test module is valid");
    module
}

fn run(passes: &[Pass], mut module: Module) -> Function {
    PassPipeline::new(passes.iter().copied())
        .run(&mut module)
        .expect("passes keep the module valid");
    module.verify().expect("the result is valid");
    module.functions.remove(0)
}

/// `out = (2 + 3) * x`, or with `fold` false, `out = x * x`.
fn arithmetic() -> Module {
    module(Function {
        name: "arithmetic".to_owned(),
        result: Type::Void,
        parameters: vec![
            Parameter {
                value: ValueId(0),
                name: "out".to_owned(),
                ty: u32_ptr(AddressSpace::Device),
            },
            Parameter {
                value: ValueId(1),
                name: "x".to_owned(),
                ty: Type::u32(),
            },
        ],
        values: vec![
            u32_ptr(AddressSpace::Device),
            Type::u32(),
            Type::u32(),
            Type::u32(),
        ],
        blocks: vec![block(
            0,
            vec![
                instruction(
                    Some(2),
                    InstructionKind::IntegerBinary {
                        op: IntegerBinaryOp::Add,
                        lhs: Operand::u32(2),
                        rhs: Operand::u32(3),
                    },
                ),
                instruction(
                    Some(3),
                    InstructionKind::IntegerBinary {
                        op: IntegerBinaryOp::Multiply,
                        lhs: Operand::Value(ValueId(2)),
                        rhs: Operand::Value(ValueId(1)),
                    },
                ),
                instruction(
                    None,
                    InstructionKind::Store {
                        pointer: Operand::Value(ValueId(0)),
                        value: Operand::Value(ValueId(3)),
                    },
                ),
            ],
            Terminator::Return(None),
        )],
    })
}

#[test]
fn constants_fold_into_their_uses() {
    let function = run(&[Pass::FoldConstants], arithmetic());
    let instructions = &function.blocks[0].instructions;
    assert_eq!(instructions.len(), 2, "the addition is gone");
    assert_eq!(
        instructions[0].kind,
        InstructionKind::IntegerBinary {
            op: IntegerBinaryOp::Multiply,
            lhs: Operand::u32(5),
            rhs: Operand::Value(ValueId(1)),
        }
    );
}

#[test]
fn folding_keeps_the_width_and_leaves_undefined_results_alone() {
    use IntegerBinaryOp as Op;
    let ty = Type::Int {
        bits: 8,
        signed: true,
    };
    let byte = |value: u64| Operand::Integer {
        ty: ty.clone(),
        value,
    };
    let fold = |op, a, b| fold::integer_binary(op, &byte(a), &byte(b), &ty);

    assert_eq!(fold(Op::Add, 0xff, 2), Some(byte(1)), "wraps at eight bits");
    assert_eq!(fold(Op::ShiftRightArithmetic, 0x80, 7), Some(byte(0xff)));
    assert_eq!(fold(Op::DivideSigned, 0xfa, 2), Some(byte(0xfd)), "-6 / 2");
    assert_eq!(fold(Op::MinimumSigned, 0xff, 1), Some(byte(0xff)), "-1 < 1");
    assert_eq!(fold(Op::MinimumUnsigned, 0xff, 1), Some(byte(1)));
    assert_eq!(fold(Op::DivideUnsigned, 1, 0), None, "division by zero");
    assert_eq!(fold(Op::DivideSigned, 0x80, 0xff), None, "-128 / -1");
    assert_eq!(fold(Op::ShiftLeft, 1, 8), None, "a shift by the width");
}

/// `if (1 < 2) out = 1; else out = 2;` with the value chosen by a phi.
fn constant_branch() -> Module {
    let compare = instruction(
        Some(1),
        InstructionKind::IntegerCompare {
            predicate: IntegerPredicate::UnsignedLessThan,
            lhs: Operand::u32(1),
            rhs: Operand::u32(2),
        },
    );
    module(Function {
        name: "branch".to_owned(),
        result: Type::Void,
        parameters: vec![Parameter {
            value: ValueId(0),
            name: "out".to_owned(),
            ty: u32_ptr(AddressSpace::Device),
        }],
        values: vec![u32_ptr(AddressSpace::Device), Type::Bool, Type::u32()],
        blocks: vec![
            block(
                0,
                vec![compare],
                Terminator::CondBranch {
                    condition: Operand::Value(ValueId(1)),
                    true_block: BlockId(1),
                    false_block: BlockId(2),
                },
            ),
            block(1, Vec::new(), Terminator::Branch(BlockId(3))),
            block(2, Vec::new(), Terminator::Branch(BlockId(3))),
            block(
                3,
                vec![
                    instruction(
                        Some(2),
                        InstructionKind::Phi {
                            incomings: vec![
                                (BlockId(1), Operand::u32(1)),
                                (BlockId(2), Operand::u32(2)),
                            ],
                        },
                    ),
                    instruction(
                        None,
                        InstructionKind::Store {
                            pointer: Operand::Value(ValueId(0)),
                            value: Operand::Value(ValueId(2)),
                        },
                    ),
                ],
                Terminator::Return(None),
            ),
        ],
    })
}

#[test]
fn a_branch_on_a_constant_leaves_one_path() {
    let function = run(&Pass::ALL, constant_branch());
    let ids: Vec<_> = function.blocks.iter().map(|block| block.id.0).collect();
    assert_eq!(ids, [0, 1, 3], "the untaken arm is removed");
    assert_eq!(
        function.blocks[0].terminator,
        Terminator::Branch(BlockId(1))
    );
    assert_eq!(
        function.blocks[2].instructions,
        [instruction(
            None,
            InstructionKind::Store {
                pointer: Operand::Value(ValueId(0)),
                value: Operand::u32(1),
            },
        )],
        "the phi became its one remaining input"
    );
}

#[test]
fn a_local_nothing_reads_is_removed_with_its_stores() {
    let local = Type::Pointer {
        space: AddressSpace::Thread,
        pointee: Box::new(Type::u32()),
    };
    let function = run(
        &[Pass::EliminateDeadCode],
        module(Function {
            name: "local".to_owned(),
            result: Type::Void,
            parameters: vec![Parameter {
                value: ValueId(0),
                name: "out".to_owned(),
                ty: u32_ptr(AddressSpace::Device),
            }],
            values: vec![u32_ptr(AddressSpace::Device), local, Type::u32()],
            blocks: vec![block(
                0,
                vec![
                    instruction(Some(1), InstructionKind::LocalVariable { ty: Type::u32() }),
                    instruction(
                        None,
                        InstructionKind::Store {
                            pointer: Operand::Value(ValueId(1)),
                            value: Operand::u32(7),
                        },
                    ),
                    instruction(
                        Some(2),
                        InstructionKind::Load {
                            pointer: Operand::Value(ValueId(0)),
                        },
                    ),
                    instruction(
                        None,
                        InstructionKind::Store {
                            pointer: Operand::Value(ValueId(0)),
                            value: Operand::u32(9),
                        },
                    ),
                ],
                Terminator::Return(None),
            )],
        }),
    );
    assert_eq!(
        function.blocks[0].instructions,
        [instruction(
            None,
            InstructionKind::Store {
                pointer: Operand::Value(ValueId(0)),
                value: Operand::u32(9),
            },
        )],
        "the local, its store and the unused load are gone; the device store stays"
    );
}

#[test]
fn no_passes_leave_the_module_untouched() {
    let original = arithmetic();
    let mut module = original.clone();
    PassPipeline::none().run(&mut module).unwrap();
    assert_eq!(module, original);
}

#[test]
fn the_pipeline_syntax_selects_and_removes_passes() {
    assert_eq!(PassPipeline::parse("").unwrap(), PassPipeline::none());
    assert_eq!(PassPipeline::parse("off").unwrap(), PassPipeline::none());
    assert_eq!(
        PassPipeline::parse("default").unwrap(),
        PassPipeline::default()
    );
    assert_eq!(
        PassPipeline::parse("eliminate-dead-code, fold-constants")
            .unwrap()
            .passes(),
        [Pass::EliminateDeadCode, Pass::FoldConstants]
    );
    assert_eq!(
        PassPipeline::parse("-simplify-phis,-remove-unreachable-blocks")
            .unwrap()
            .passes(),
        [Pass::FoldConstants, Pass::EliminateDeadCode]
    );
    assert!(PassPipeline::parse("fold-constants,inline-everything").is_err());
}

#[test]
fn the_fingerprint_names_the_passes_and_their_order() {
    let default = PassPipeline::default().fingerprint();
    assert_eq!(default, PassPipeline::default().fingerprint(), "stable");
    assert_ne!(default, PassPipeline::none().fingerprint());
    assert_ne!(
        PassPipeline::new([Pass::FoldConstants, Pass::EliminateDeadCode]).fingerprint(),
        PassPipeline::new([Pass::EliminateDeadCode, Pass::FoldConstants]).fingerprint(),
    );
}
