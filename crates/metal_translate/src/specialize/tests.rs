use super::*;
use crate::ir::{
    Block, BlockId, EntryPoint, FunctionConstantSlot, Instruction, IntegerBinaryOp, Parameter,
    Stage,
};
use crate::{PassPipeline, TargetProfile, emit_spirv};

const BOOL: u64 = 53;
const UINT2: u64 = 34;

fn byte() -> Type {
    Type::Int {
        bits: 8,
        signed: false,
    }
}

fn constant_pointer(ty: Type) -> Type {
    Type::Pointer {
        space: AddressSpace::Constant,
        pointee: Box::new(ty),
    }
}

fn device_pointer() -> Type {
    Type::Pointer {
        space: AddressSpace::Device,
        pointee: Box::new(Type::u32()),
    }
}

fn global(ty: &Type, index: u32) -> Operand {
    Operand::Global {
        index,
        ty: constant_pointer(ty.clone()),
    }
}

fn instruction(result: Option<u32>, kind: InstructionKind) -> Instruction {
    Instruction {
        result: result.map(ValueId),
        kind,
    }
}

/// A kernel which stores `use_fog ? 7 : 3` when its `use_fog` function
/// constant is defined and true, and 1 when it is not defined at all.
///
/// ```text
/// constant bool use_fog [[function_constant(0)]];
/// constant bool fog_off = !use_fog;           // a static initializer
/// kernel void k(device uint* out) {
///     *out = is_function_constant_defined(use_fog) ? (fog_off ? 3 : 7) : 1;
/// }
/// ```
fn fog_module() -> Module {
    let fog_slot = FunctionConstantSlot {
        index: 0,
        name: "use_fog".to_owned(),
        encoding: "b".to_owned(),
    };
    let globals = vec![
        GlobalVariable {
            name: "use_fog.MTL_FC_INIT_0_b".to_owned(),
            space: AddressSpace::Constant,
            ty: byte(),
            initializer: Some(Operand::Undef(byte())),
            function_constant: Some(fog_slot),
        },
        GlobalVariable {
            name: "fog_off".to_owned(),
            space: AddressSpace::Constant,
            ty: byte(),
            initializer: Some(Operand::Integer {
                ty: byte(),
                value: 0,
            }),
            function_constant: None,
        },
    ];
    let initializer = Function {
        name: "_GLOBAL__sub_I_fog".to_owned(),
        result: Type::Void,
        parameters: Vec::new(),
        values: vec![byte(), byte()],
        blocks: vec![Block {
            id: BlockId(0),
            name: "entry".to_owned(),
            instructions: vec![
                instruction(
                    Some(0),
                    InstructionKind::Load {
                        pointer: global(&byte(), 0),
                    },
                ),
                instruction(
                    Some(1),
                    InstructionKind::IntegerBinary {
                        op: IntegerBinaryOp::Xor,
                        lhs: Operand::Value(ValueId(0)),
                        rhs: Operand::Integer {
                            ty: byte(),
                            value: 1,
                        },
                    },
                ),
                instruction(
                    None,
                    InstructionKind::Store {
                        pointer: global(&byte(), 1),
                        value: Operand::Value(ValueId(1)),
                    },
                ),
            ],
            terminator: Terminator::Return(None),
        }],
    };
    // %1 = defined(use_fog); %2 = load fog_off; %3 = trunc %2;
    // %4 = select %3, 3, 7; %5 = select %1, %4, 1; store %5
    let kernel = Function {
        name: "k".to_owned(),
        result: Type::Void,
        parameters: vec![Parameter {
            value: ValueId(0),
            name: "out".to_owned(),
            ty: device_pointer(),
        }],
        values: vec![
            device_pointer(),
            Type::Bool,
            byte(),
            Type::Bool,
            Type::u32(),
            Type::u32(),
        ],
        blocks: vec![Block {
            id: BlockId(0),
            name: "entry".to_owned(),
            instructions: vec![
                instruction(
                    Some(1),
                    InstructionKind::FunctionConstantDefined {
                        constant: global(&byte(), 0),
                    },
                ),
                instruction(
                    Some(2),
                    InstructionKind::Load {
                        pointer: global(&byte(), 1),
                    },
                ),
                instruction(
                    Some(3),
                    InstructionKind::Convert {
                        op: crate::ir::ConvertOp::IntegerTruncate,
                        value: Operand::Value(ValueId(2)),
                    },
                ),
                instruction(
                    Some(4),
                    InstructionKind::Select {
                        condition: Operand::Value(ValueId(3)),
                        on_true: Operand::u32(3),
                        on_false: Operand::u32(7),
                    },
                ),
                instruction(
                    Some(5),
                    InstructionKind::Select {
                        condition: Operand::Value(ValueId(1)),
                        on_true: Operand::Value(ValueId(4)),
                        on_false: Operand::u32(1),
                    },
                ),
                instruction(
                    None,
                    InstructionKind::Store {
                        pointer: Operand::Value(ValueId(0)),
                        value: Operand::Value(ValueId(5)),
                    },
                ),
            ],
            terminator: Terminator::Return(None),
        }],
    };
    let module = Module {
        functions: vec![kernel],
        entries: vec![EntryPoint {
            early_fragment_tests: false,
            function: 0,
            stage: Stage::Kernel,
            resources: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
        globals,
        static_initializers: vec![initializer],
    };
    module.verify().expect("the test module is valid");
    module
}

/// Specialize, optimize, and answer what the kernel stores.
fn stored(values: &FunctionConstantValues) -> Operand {
    let mut module = fog_module();
    specialize(&mut module, values).expect("specialization succeeds");
    assert!(module.static_initializers.is_empty(), "initializers ran");
    PassPipeline::default().run(&mut module).expect("passes");
    module.verify().expect("valid after specialization");
    let instructions = &module.functions[0].blocks[0].instructions;
    let [store] = instructions.as_slice() else {
        panic!("the kernel folds to one store, not {instructions:#?}");
    };
    let InstructionKind::Store { value, .. } = &store.kind else {
        panic!("not a store: {store:?}");
    };
    value.clone()
}

#[test]
fn a_function_constant_and_what_derives_from_it_fold_away() {
    let mut on = FunctionConstantValues::new();
    on.set_index(0, BOOL, &[1]).unwrap();
    assert_eq!(stored(&on), Operand::u32(7));

    let mut off = FunctionConstantValues::new();
    off.set_name("use_fog", BOOL, &[0]).unwrap();
    assert_eq!(stored(&off), Operand::u32(3), "looked up by name");

    // Metal reads any nonzero byte as true.
    let mut two = FunctionConstantValues::new();
    two.set_index(0, BOOL, &[2]).unwrap();
    assert_eq!(stored(&two), Operand::u32(7));
}

#[test]
fn an_undefined_function_constant_answers_false_and_stays_undefined() {
    assert_eq!(stored(&FunctionConstantValues::new()), Operand::u32(1));

    let mut module = fog_module();
    specialize(&mut module, &FunctionConstantValues::new()).unwrap();
    assert_eq!(
        module.globals[1].initializer,
        Some(Operand::Undef(byte())),
        "a constant derived from an undefined one is undefined"
    );
}

#[test]
fn a_value_of_the_wrong_type_is_rejected() {
    let mut wrong = FunctionConstantValues::new();
    wrong.set_index(0, UINT2, &[0; 8]).unwrap();
    let error = specialize(&mut fog_module(), &wrong).unwrap_err();
    assert_eq!(error.diagnostics[0].phase, Phase::Specialization);
    assert!(error.to_string().contains("use_fog"), "{error}");
}

#[test]
fn values_are_read_at_their_width() {
    let uint2 = Type::Vector {
        element: Box::new(Type::u32()),
        lanes: 2,
    };
    let bytes = [7u32.to_le_bytes(), 11u32.to_le_bytes()].concat();
    assert_eq!(
        decode(&uint2, &bytes, false),
        Some(Operand::Composite {
            ty: uint2.clone(),
            elements: vec![Operand::u32(7), Operand::u32(11)],
        })
    );
    assert_eq!(decode(&uint2, &bytes[..7], false), None, "too few bytes");
    assert_eq!(metal_data_type_size(UINT2), Some(8));
    assert_eq!(metal_data_type_size(BOOL), Some(1));
    assert_eq!(metal_data_type_size(58), None, "a texture is no constant");
    assert!(
        FunctionConstantValues::new()
            .set_index(0, 29, &[0; 3])
            .is_err()
    );
}

#[test]
fn air_names_its_function_constants_by_index_and_type() {
    assert_eq!(
        parse_slot_name("lanes.MTL_FC_INIT_3_Dv2_j"),
        Some(FunctionConstantSlot {
            index: 3,
            name: "lanes".to_owned(),
            encoding: "Dv2_j".to_owned(),
        })
    );
    assert_eq!(parse_slot_name("aoCache1"), None);
    let uint2 = Type::Vector {
        element: Box::new(Type::u32()),
        lanes: 2,
    };
    assert!(encoding_matches(&uint2, "Dv2_j"));
    assert!(!encoding_matches(&uint2, "Dv3_j"));
    assert!(encoding_matches(&byte(), "b"));
    assert!(encoding_matches(&Type::Float { bits: 16 }, "Dh"));
    assert!(!encoding_matches(&Type::u32(), "f"));
}

#[test]
fn a_module_with_initializers_is_not_emitted_unspecialized() {
    let error = emit_spirv(&fog_module(), &TargetProfile::vulkan_1_3()).unwrap_err();
    assert!(error.to_string().contains("specialize"), "{error}");
}

#[test]
fn an_initializer_which_does_more_than_compute_constants_is_rejected() {
    let mut module = fog_module();
    module.static_initializers[0].blocks[0].instructions.insert(
        0,
        Instruction {
            result: None,
            kind: InstructionKind::DiscardFragment,
        },
    );
    let error = specialize(&mut module, &FunctionConstantValues::new()).unwrap_err();
    assert!(error.to_string().contains("fragment discard"), "{error}");
}
