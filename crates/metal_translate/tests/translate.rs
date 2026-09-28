use metal_translate::ir::{
    Access, AddressSpace, AtomicOp, BarrierMemory, BarrierScope, Block, BlockId, ConvertOp,
    EntryPoint, FloatPredicate, FloatUnaryOp, Function, Instruction, InstructionKind,
    IntegerBinaryOp, MemoryOrder, Module, Operand, Parameter, Resource, ResourceKind, Stage,
    StageBinding, StageInput, Terminator, TextureDimension, TextureType, Type, ValueId,
};
use metal_translate::{
    AirInput, Phase, TargetProfile, emit_spirv, metallib_air, parse_air, parse_msl, translate,
    translate_msl,
};
use rspirv::spirv::{Op, Scope};

const STORE: &str = include_str!("fixtures/kernel_store_const.ll");
const COPY: &str = include_str!("fixtures/kernel_copy_word.ll");
const INVERT: &str = include_str!("fixtures/kernel_invert_word.ll");
const LOOP: &str = include_str!("fixtures/kernel_bounded_loop_word.ll");

fn store_bitcode() -> Vec<u8> {
    let encoded = include_str!("fixtures/kernel_store_const.bc.hex")
        .trim()
        .as_bytes();
    let (pairs, remainder) = encoded.as_chunks::<2>();
    assert!(remainder.is_empty());
    pairs
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(text, 16).unwrap()
        })
        .collect()
}

#[test]
fn imports_the_four_core_compute_fixtures() {
    for (source, name) in [
        (STORE, "store_const"),
        (COPY, "copy_word"),
        (INVERT, "invert_word"),
        (LOOP, "bounded_loop_word"),
    ] {
        let module = parse_air(AirInput::Text(source)).unwrap();
        assert_eq!(module.functions[0].name, name);
        assert_eq!(module.entries[0].stage, Stage::Kernel);
    }
}

#[test]
fn preserves_metal_resource_access_and_indices() {
    let module = parse_air(AirInput::Text(COPY)).unwrap();
    let resources = &module.entries[0].resources;
    assert_eq!(resources.len(), 2);
    assert_eq!((resources[0].index, resources[0].access), (0, Access::Read));
    assert_eq!(
        (resources[1].index, resources[1].access),
        (1, Access::Write)
    );
}

#[test]
fn preserves_loop_ssa_before_spirv_lowering() {
    let module = parse_air(AirInput::Text(LOOP)).unwrap();
    assert_eq!(module.functions[0].blocks.len(), 4);
    assert!(matches!(
        module.functions[0].blocks[1].instructions[0].kind,
        InstructionKind::Phi { .. }
    ));
}

#[test]
fn emits_parseable_spirv_for_each_core_kernel() {
    for source in [STORE, COPY, INVERT, LOOP] {
        let library = translate(AirInput::Text(source), &TargetProfile::vulkan_1_3()).unwrap();
        assert_eq!(library.entries.len(), 1);
        let entry = &library.entries[0];
        assert_eq!(entry.words[0], rspirv::spirv::MAGIC_NUMBER);
        assert_eq!(entry.required.capabilities, ["Shader"]);
        let module = rspirv::dr::load_words(&entry.words).unwrap();
        assert_eq!(module.entry_points.len(), 1);
    }
}

#[test]
fn emits_decoded_buffer_addresses_math_and_integer_widths() {
    use metal_translate::ir::{Instruction, Operand};
    let mut module = parse_air(AirInput::Text(STORE)).unwrap();
    let function = &mut module.functions[0];
    let pointer = function.parameters[0].ty.clone();
    let f32_ty = Type::Float { bits: 32 };
    let u64_ty = Type::Int {
        bits: 64,
        signed: false,
    };
    let operations = [
        (
            pointer,
            InstructionKind::BufferAddress {
                pointer: Operand::Value(ValueId(0)),
                indices: vec![Operand::u32(0)],
            },
        ),
        (
            u64_ty,
            InstructionKind::Convert {
                op: ConvertOp::ZeroExtend,
                value: Operand::u32(17),
            },
        ),
        (
            f32_ty.clone(),
            InstructionKind::FloatUnary {
                op: FloatUnaryOp::Absolute,
                value: Operand::f32(-4.0),
            },
        ),
        (
            f32_ty.clone(),
            InstructionKind::FloatUnary {
                op: FloatUnaryOp::InverseSqrt,
                value: Operand::f32(4.0),
            },
        ),
        (
            Type::Bool,
            InstructionKind::FloatCompare {
                predicate: FloatPredicate::OrderedGreaterEqual,
                lhs: Operand::f32(1.0),
                rhs: Operand::f32(0.0),
            },
        ),
        (
            f32_ty,
            InstructionKind::Select {
                condition: Operand::Bool(true),
                on_true: Operand::f32(1.0),
                on_false: Operand::f32(0.0),
            },
        ),
    ];
    for (index, (ty, kind)) in operations.into_iter().enumerate() {
        let result = ValueId(function.values.len() as u32);
        function.values.push(ty);
        function.blocks[0].instructions.insert(
            index,
            Instruction {
                result: Some(result),
                kind,
            },
        );
    }
    module.verify().unwrap();
    let mut target = TargetProfile::vulkan_1_3();
    assert!(
        emit_spirv(&module, &target)
            .unwrap_err()
            .to_string()
            .contains("Int64")
    );
    target.shader_int64 = true;
    let library = emit_spirv(&module, &target).unwrap();
    assert_eq!(
        library.entries[0].required.capabilities,
        ["Shader", "Int64"]
    );
    let binary = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    let opcodes: Vec<_> = binary
        .functions
        .iter()
        .flat_map(|f| &f.blocks)
        .flat_map(|b| &b.instructions)
        .map(|i| i.class.opcode)
        .collect();
    for opcode in [
        Op::AccessChain,
        Op::UConvert,
        Op::ExtInst,
        Op::FOrdGreaterThanEqual,
        Op::Select,
    ] {
        assert!(opcodes.contains(&opcode));
    }
}

#[test]
fn rejects_buffer_address_with_noninteger_index() {
    use metal_translate::ir::{Instruction, Operand};
    let mut module = parse_air(AirInput::Text(STORE)).unwrap();
    let function = &mut module.functions[0];
    let result = ValueId(function.values.len() as u32);
    function.values.push(function.parameters[0].ty.clone());
    function.blocks[0].instructions.insert(
        0,
        Instruction {
            result: Some(result),
            kind: InstructionKind::BufferAddress {
                pointer: Operand::Value(ValueId(0)),
                indices: vec![Operand::f32(0.0)],
            },
        },
    );
    assert!(module.verify().is_err());
}

#[test]
fn emits_dynamic_array_index_inside_a_buffer_struct() {
    use metal_translate::ir::{Instruction, Operand, StructField, StructType};
    let mut module = parse_air(AirInput::Text(STORE)).unwrap();
    let function = &mut module.functions[0];
    let scalar_pointer = function.parameters[0].ty.clone();
    let Type::Pointer { space, .. } = scalar_pointer.clone() else {
        unreachable!()
    };
    let scalar_pointer = Type::Pointer {
        space,
        pointee: Box::new(Type::u32()),
    };
    let pointer = Type::Pointer {
        space,
        pointee: Box::new(Type::Struct(StructType {
            fields: vec![StructField {
                name: None,
                offset: 0,
                ty: Type::Array {
                    element: Box::new(Type::u32()),
                    count: 4,
                },
            }],
            size: 16,
            alignment: 4,
            packed: false,
        })),
    };
    function.parameters[0].ty = pointer.clone();
    function.values = vec![pointer.clone(), Type::u32(), scalar_pointer];
    module.entries[0].resources[0].ty = pointer;
    function.blocks[0].instructions = vec![
        Instruction {
            result: Some(ValueId(1)),
            kind: InstructionKind::IntegerBinary {
                op: IntegerBinaryOp::Add,
                lhs: Operand::u32(1),
                rhs: Operand::u32(1),
            },
        },
        Instruction {
            result: Some(ValueId(2)),
            kind: InstructionKind::BufferAddress {
                pointer: Operand::Value(ValueId(0)),
                indices: vec![Operand::u32(0), Operand::u32(0), Operand::Value(ValueId(1))],
            },
        },
        Instruction {
            result: None,
            kind: InstructionKind::Store {
                pointer: Operand::Value(ValueId(2)),
                value: Operand::u32(42),
            },
        },
    ];
    let library = emit_spirv(&module, &TargetProfile::vulkan_1_3()).unwrap();
    let binary = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    assert!(
        binary
            .types_global_values
            .iter()
            .any(|i| i.class.opcode == Op::TypeArray)
    );
    assert!(
        binary
            .functions
            .iter()
            .flat_map(|f| &f.blocks)
            .flat_map(|b| &b.instructions)
            .any(|i| i.class.opcode == Op::AccessChain && i.operands.len() == 5)
    );
    // A struct field itself cannot be selected dynamically.
    if let InstructionKind::BufferAddress { indices, .. } =
        &mut module.functions[0].blocks[0].instructions[1].kind
    {
        indices[1] = Operand::Value(ValueId(1));
    }
    assert!(module.verify().is_err());
}

#[test]
fn maps_buffer_indices_into_the_buffer_descriptor_set() {
    let library = translate(AirInput::Text(COPY), &TargetProfile::vulkan_1_3()).unwrap();
    let resources = &library.entries[0].resources;
    assert_eq!((resources[0].descriptor_set, resources[0].binding), (0, 0));
    assert_eq!((resources[1].descriptor_set, resources[1].binding), (0, 1));
}

#[test]
fn emits_loop_merge_and_phi_for_the_bounded_loop() {
    let library = translate(AirInput::Text(LOOP), &TargetProfile::vulkan_1_3()).unwrap();
    let module = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    let opcodes: Vec<_> = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .map(|instruction| instruction.class.opcode)
        .collect();
    assert!(opcodes.contains(&Op::Phi));
    assert!(opcodes.contains(&Op::LoopMerge));
}

#[test]
fn rejects_non_kernel_air_without_a_spirv_fallback() {
    let error = parse_air(AirInput::Text(
        "define void @f() { ret void }\n!air.fragment = !{!0}\n",
    ))
    .unwrap_err();
    assert_eq!(error.diagnostics[0].phase, Phase::AirSemantics);
}

#[test]
fn rejects_truncated_bitcode_as_a_container_error() {
    let error = parse_air(AirInput::Bitcode(b"not bitcode")).unwrap_err();
    assert_eq!(error.diagnostics[0].phase, Phase::AirContainer);
}

#[test]
fn extracts_air_bitcode_from_a_metallib() {
    let payload = [b'B', b'C', 0xc0, 0xde, 1, 2, 3, 4];
    let mut library = vec![0; 88];
    library[..4].copy_from_slice(b"MTLB");
    library[72..80].copy_from_slice(&88_u64.to_le_bytes());
    library[80..88].copy_from_slice(&28_u64.to_le_bytes());
    library.extend_from_slice(&0x0b17_c0de_u32.to_le_bytes());
    library.extend_from_slice(&0_u32.to_le_bytes());
    library.extend_from_slice(&20_u32.to_le_bytes());
    library.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    library.extend_from_slice(&u32::MAX.to_le_bytes());
    library.extend_from_slice(&payload);

    assert_eq!(metallib_air(&library).unwrap(), payload);
}

#[test]
fn extracts_every_air_module_from_a_metallib() {
    fn wrapper(payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x0b17_c0de_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&20_u32.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes.resize((bytes.len() + 15) & !15, 0);
        bytes
    }

    let first = [b'B', b'C', 0xc0, 0xde, 1];
    let second = [b'B', b'C', 0xc0, 0xde, 2, 3];
    let wrappers = [wrapper(&first), wrapper(&second)].concat();
    let mut library = vec![0; 88];
    library[..4].copy_from_slice(b"MTLB");
    library[72..80].copy_from_slice(&88_u64.to_le_bytes());
    library[80..88].copy_from_slice(&(wrappers.len() as u64).to_le_bytes());
    library.extend_from_slice(&wrappers);

    assert_eq!(
        metal_translate::metallib_airs(&library).unwrap(),
        vec![first.as_slice(), second.as_slice()]
    );
}

#[test]
fn textual_and_bitcode_store_modules_are_equivalent() {
    let text = parse_air(AirInput::Text(STORE)).unwrap();
    let bitcode = parse_air(AirInput::Bitcode(&store_bitcode())).unwrap();
    assert_eq!(bitcode, text);
    translate(
        AirInput::Bitcode(&store_bitcode()),
        &TargetProfile::vulkan_1_3(),
    )
    .unwrap();
}

#[test]
fn emission_is_deterministic() {
    let first = translate(AirInput::Text(LOOP), &TargetProfile::vulkan_1_3()).unwrap();
    let second = translate(AirInput::Text(LOOP), &TargetProfile::vulkan_1_3()).unwrap();
    assert_eq!(first, second);
}

#[test]
fn rejects_malformed_resource_metadata() {
    let source = STORE.replace("air.location_index", "air.unknown_location");
    let error = parse_air(AirInput::Text(&source)).unwrap_err();
    assert_eq!(error.diagnostics[0].phase, Phase::AirSemantics);
}

#[test]
fn rejects_unsupported_air_instructions() {
    let source = INVERT.replace("xor i32", "mul i32");
    let error = parse_air(AirInput::Text(&source)).unwrap_err();
    assert_eq!(error.diagnostics[0].phase, Phase::AirSyntax);
}

#[test]
fn verifier_rejects_an_undefined_ssa_use() {
    let mut module = parse_air(AirInput::Text(STORE)).unwrap();
    let metal_translate::ir::InstructionKind::Store { value, .. } =
        &mut module.functions[0].blocks[0].instructions[0].kind
    else {
        unreachable!()
    };
    *value = metal_translate::ir::Operand::Value(metal_translate::ir::ValueId(99));
    let error = module.verify().unwrap_err();
    assert_eq!(error.diagnostics[0].phase, Phase::IrVerification);
}

#[test]
fn verifier_rejects_duplicate_metal_bindings() {
    let mut module = parse_air(AirInput::Text(COPY)).unwrap();
    module.entries[0].resources[1].index = 0;
    assert!(module.verify().is_err());
}

#[test]
fn rejects_a_target_without_required_capabilities() {
    let mut target = TargetProfile::vulkan_1_3();
    target.shader = false;
    let error = translate(AirInput::Text(STORE), &target).unwrap_err();
    assert_eq!(error.diagnostics[0].phase, Phase::SpirvEmission);
}

#[test]
fn compiles_a_source_msl_compute_kernel_to_spirv() {
    let source = "kernel void clear(device uint* output [[buffer(3)]]) { output[0] = 42; }";
    let module = parse_msl(source).unwrap();
    assert_eq!(module.functions[0].name, "clear");
    assert_eq!(module.entries[0].resources[0].index, 3);
    let library = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap();
    assert_eq!(library.entries[0].words[0], rspirv::spirv::MAGIC_NUMBER);
    assert_eq!(library.entries[0].resources[0].binding, 3);
}

#[test]
fn source_msl_rejects_unimplemented_operations() {
    let error = parse_msl(
        "\n\
         kernel void f(device uint* output [[buffer(0)]]) {\n\
             output[1] = 1;\n\
         }",
    )
    .unwrap_err();
    assert_eq!(error.diagnostics[0].phase, Phase::AirSyntax);
    assert_eq!(error.diagnostics[0].line, Some(2));
}

#[test]
fn source_msl_translation_errors_include_nearby_source() {
    let source = "// Prelude\n\
                  // More prelude\n\
                  kernel void f(\n\
                      texturecube<float, access::write> output [[texture(0)]]\n\
                  ) { return; }\n\
                  // Epilogue";
    let error = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap_err();

    assert_eq!(error.diagnostics[0].line, Some(4));
    assert_eq!(
        error.to_string(),
        concat!(
            "AirSyntax at line 4: unsupported MSL texture type\n\n",
            "MSL source context:\n",
            "  2 | // More prelude\n",
            "  3 | kernel void f(\n",
            "> 4 | texturecube<float, access::write> output [[texture(0)]]\n",
            "  5 | ) { return; }\n",
            "  6 | // Epilogue",
        )
    );
}

#[test]
fn source_msl_rejects_texture_shapes_without_an_ir_type() {
    let error = parse_msl(
        "kernel void f(texturecube<float, access::write> output [[texture(0)]]) { return; }",
    )
    .unwrap_err();
    assert_eq!(error.diagnostics[0].message, "unsupported MSL texture type");
}

#[test]
fn source_msl_compiles_a_bounded_typed_blit_kernel() {
    let source = "struct BlitParams { uint3 offset; uint3 size; uint2 stride; };\n\
         kernel void blit(\n\
           uint3 gid [[thread_position_in_grid]],\n\
           constant BlitParams& params [[buffer(0)]],\n\
           device uchar4* input [[buffer(1)]],\n\
         texture3d<float, access::write> output [[texture(0)]])\n\
         {\n\
           if (all(gid < params.size)) {\n\
             uchar4 element = input[(gid.z * params.stride.y + gid.y) * params.stride.x + gid.x + params.offset.x];\n\
             output.write(float4(element) / 255.f, gid + params.offset);\n\
           }\n\
         }";
    let target = TargetProfile {
        shader_int8: true,
        storage_buffer8_bit_access: true,
        shader_storage_image_write_without_format: true,
        ..TargetProfile::vulkan_1_3()
    };
    let library = translate_msl(source, &target).unwrap();
    let module = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    for operation in [
        Op::ImageWrite,
        Op::ULessThan,
        Op::All,
        Op::ConvertUToF,
        Op::FDiv,
        Op::IMul,
        Op::IAdd,
    ] {
        assert!(
            module
                .functions
                .iter()
                .flat_map(|function| &function.blocks)
                .flat_map(|block| &block.instructions)
                .any(|instruction| instruction.class.opcode == operation)
        );
    }
}

#[test]
fn emits_a_writable_3d_texture_descriptor_in_its_metal_namespace() {
    let texture = Type::Texture(TextureType {
        dimension: TextureDimension::D3,
        array: false,
        multisampled: false,
        depth: false,
        texel: Box::new(Type::Float { bits: 32 }),
        signed: false,
        access: Access::Write,
    });
    let module = Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![Function {
            name: "clear_volume".to_string(),
            result: Type::Void,
            parameters: vec![Parameter {
                value: ValueId(0),
                name: "output".to_string(),
                ty: texture.clone(),
            }],
            values: vec![texture.clone()],
            blocks: vec![Block {
                id: BlockId(0),
                name: "entry".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Return(None),
            }],
        }],
        entries: vec![EntryPoint {
            function: 0,
            stage: Stage::Kernel,
            resources: vec![Resource {
                argument: 0,
                kind: ResourceKind::Texture,
                index: 7,
                count: 1,
                access: Access::Write,
                ty: texture,
                name: Some("output".to_string()),
            }],
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
    };

    let target = TargetProfile {
        shader_storage_image_write_without_format: true,
        ..TargetProfile::vulkan_1_3()
    };
    let artifact = emit_spirv(&module, &target).unwrap().entries.remove(0);
    assert_eq!(artifact.resources[0].descriptor_set, 1);
    assert_eq!(artifact.resources[0].binding, 7);
    let parsed = rspirv::dr::load_words(&artifact.words).unwrap();
    assert!(
        parsed
            .types_global_values
            .iter()
            .any(|instruction| instruction.class.opcode == Op::TypeImage)
    );
}

/// SPIR-V has no unstructured control flow: a conditional branch must declare
/// where its paths rejoin, and a loop must additionally name its back edge.
/// LLVM records neither, so both are recovered from the graph.
#[test]
fn emits_structured_merges_for_a_loop_containing_a_conditional() {
    let bool_ty = Type::Bool;
    let function = Function {
        name: "shade".to_string(),
        result: Type::Void,
        parameters: Vec::new(),
        values: vec![bool_ty.clone(), bool_ty.clone()],
        blocks: vec![
            // header: loops while %0 holds, and contains an `if` on %1.
            Block {
                id: BlockId(0),
                name: "header".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::CondBranch {
                    condition: Operand::Bool(true),
                    true_block: BlockId(1),
                    false_block: BlockId(3),
                },
            },
            Block {
                id: BlockId(1),
                name: "body".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::CondBranch {
                    condition: Operand::Bool(false),
                    true_block: BlockId(2),
                    false_block: BlockId(4),
                },
            },
            Block {
                id: BlockId(2),
                name: "taken".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Branch(BlockId(4)),
            },
            Block {
                id: BlockId(3),
                name: "exit".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Return(None),
            },
            // latch: the back edge to the header.
            Block {
                id: BlockId(4),
                name: "latch".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Branch(BlockId(0)),
            },
        ],
    };
    let module = Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![function],
        entries: vec![EntryPoint {
            function: 0,
            stage: Stage::Fragment,
            resources: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
    };
    module.verify().unwrap();
    let artifact = emit_spirv(&module, &TargetProfile::vulkan_1_3())
        .unwrap()
        .entries
        .remove(0);
    let parsed = rspirv::dr::load_words(&artifact.words).unwrap();
    let opcodes: Vec<_> = parsed.functions[0]
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .map(|instruction| instruction.class.opcode)
        .collect();
    assert!(opcodes.contains(&Op::LoopMerge));
    assert!(opcodes.contains(&Op::SelectionMerge));
}

/// The second test in `a || b` initially shares the first test's merge. The
/// structurizer gives it a merge block of its own, as SPIR-V requires of every
/// conditional branch.
#[test]
fn gives_each_short_circuit_selection_its_own_merge() {
    let function = Function {
        name: "short_circuit".to_string(),
        result: Type::Void,
        parameters: Vec::new(),
        values: Vec::new(),
        blocks: vec![
            Block {
                id: BlockId(0),
                name: "outer".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::CondBranch {
                    condition: Operand::Bool(true),
                    true_block: BlockId(1),
                    false_block: BlockId(2),
                },
            },
            Block {
                id: BlockId(1),
                name: "taken".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Branch(BlockId(4)),
            },
            Block {
                id: BlockId(2),
                name: "inner".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::CondBranch {
                    condition: Operand::Bool(false),
                    true_block: BlockId(1),
                    false_block: BlockId(3),
                },
            },
            Block {
                id: BlockId(3),
                name: "not_taken".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Branch(BlockId(4)),
            },
            Block {
                id: BlockId(4),
                name: "merge".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Return(None),
            },
        ],
    };
    let module = Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![function],
        entries: vec![EntryPoint {
            function: 0,
            stage: Stage::Fragment,
            resources: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
    };
    let artifact = emit_spirv(&module, &TargetProfile::vulkan_1_3())
        .unwrap()
        .entries
        .remove(0);
    let parsed = rspirv::dr::load_words(&artifact.words).unwrap();
    let merges = parsed.functions[0]
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| instruction.class.opcode == Op::SelectionMerge)
        .count();
    assert_eq!(merges, 2);
}

/// Metal returns from several places; SPIR-V needs one returning block so
/// every conditional above them has somewhere to rejoin.
#[test]
fn routes_several_returns_through_one_exit_block() {
    let module = Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![Function {
            name: "clip".to_string(),
            result: Type::Void,
            parameters: Vec::new(),
            values: Vec::new(),
            blocks: vec![
                Block {
                    id: BlockId(0),
                    name: "entry".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::CondBranch {
                        condition: Operand::Bool(true),
                        true_block: BlockId(1),
                        false_block: BlockId(2),
                    },
                },
                Block {
                    id: BlockId(1),
                    name: "inside".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(None),
                },
                Block {
                    id: BlockId(2),
                    name: "outside".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(None),
                },
            ],
        }],
        entries: vec![EntryPoint {
            function: 0,
            stage: Stage::Fragment,
            resources: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
    };
    module.verify().unwrap();
    let artifact = emit_spirv(&module, &TargetProfile::vulkan_1_3())
        .unwrap()
        .entries
        .remove(0);
    let parsed = rspirv::dr::load_words(&artifact.words).unwrap();
    let returns = parsed.functions[0]
        .blocks
        .iter()
        .filter(|block| {
            block
                .instructions
                .last()
                .is_some_and(|instruction| instruction.class.opcode == Op::Return)
        })
        .count();
    assert_eq!(returns, 1);
    assert_eq!(parsed.functions[0].blocks.len(), 4);
}

/// A Metal `constant` lookup table lives outside every function. It becomes a
/// SPIR-V `Private` variable with its initializer, and reading it is an access
/// chain into that variable rather than into a buffer block.
#[test]
fn a_module_constant_table_becomes_a_private_variable() {
    use metal_translate::ir::{AddressSpace, GlobalVariable, Instruction};

    let float = Type::Float { bits: 32 };
    let table = Type::Array {
        element: Box::new(float.clone()),
        count: 2,
    };
    let pointer = Type::Pointer {
        space: AddressSpace::Constant,
        pointee: Box::new(table.clone()),
    };
    let element_pointer = Type::Pointer {
        space: AddressSpace::Constant,
        pointee: Box::new(float.clone()),
    };
    let buffer = Type::Pointer {
        space: AddressSpace::Device,
        pointee: Box::new(float.clone()),
    };
    let module = Module {
        static_initializers: Vec::new(),
        globals: vec![GlobalVariable {
            function_constant: None,
            name: "weights".to_string(),
            space: AddressSpace::Constant,
            ty: table.clone(),
            initializer: Some(Operand::Composite {
                ty: table,
                elements: vec![Operand::f32(0.25), Operand::f32(0.75)],
            }),
        }],
        functions: vec![Function {
            name: "read_table".to_string(),
            result: Type::Void,
            parameters: vec![Parameter {
                value: ValueId(0),
                name: "output".to_string(),
                ty: buffer.clone(),
            }],
            values: vec![
                buffer.clone(),
                element_pointer.clone(),
                float.clone(),
                buffer.clone(),
            ],
            blocks: vec![Block {
                id: BlockId(0),
                name: "entry".to_string(),
                instructions: vec![
                    Instruction {
                        result: Some(ValueId(1)),
                        kind: InstructionKind::BufferAddress {
                            pointer: Operand::Global {
                                index: 0,
                                ty: pointer,
                            },
                            // LLVM's leading index steps the pointer itself.
                            indices: vec![Operand::u32(0), Operand::u32(1)],
                        },
                    },
                    Instruction {
                        result: Some(ValueId(2)),
                        kind: InstructionKind::Load {
                            pointer: Operand::Value(ValueId(1)),
                        },
                    },
                    Instruction {
                        result: Some(ValueId(3)),
                        kind: InstructionKind::ElementAddress {
                            pointer: Operand::Value(ValueId(0)),
                            index: Operand::u32(0),
                        },
                    },
                    Instruction {
                        result: None,
                        kind: InstructionKind::Store {
                            pointer: Operand::Value(ValueId(3)),
                            value: Operand::Value(ValueId(2)),
                        },
                    },
                ],
                terminator: Terminator::Return(None),
            }],
        }],
        entries: vec![EntryPoint {
            function: 0,
            stage: Stage::Kernel,
            resources: vec![Resource {
                argument: 0,
                kind: ResourceKind::Buffer,
                index: 0,
                count: 1,
                access: Access::Write,
                ty: buffer,
                name: Some("output".to_string()),
            }],
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
    };
    let library = emit_spirv(&module, &TargetProfile::vulkan_1_3()).expect("the module emits");
    let parsed = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    let private = parsed
        .types_global_values
        .iter()
        .find(|instruction| {
            instruction.class.opcode == Op::Variable
                && instruction.operands.first()
                    == Some(&rspirv::dr::Operand::StorageClass(
                        rspirv::spirv::StorageClass::Private,
                    ))
        })
        .expect("the constant table is a Private variable");
    assert_eq!(
        private.operands.len(),
        2,
        "a Private variable keeps its initializer"
    );
    let table_chain = parsed.functions[0].blocks[0]
        .instructions
        .iter()
        .find(|instruction| {
            instruction.class.opcode == Op::AccessChain
                && instruction.operands.first()
                    == Some(&rspirv::dr::Operand::IdRef(private.result_id.unwrap()))
        })
        .expect("the table is read through an access chain");
    assert_eq!(
        table_chain.operands.len(),
        2,
        "an access chain into a variable drops LLVM's pointer step"
    );
}

/// An `alloca` is a `Function`-storage variable, and SPIR-V requires every one
/// of them in the entry block whatever block the `alloca` was written in.
#[test]
fn a_local_variable_is_declared_in_the_entry_block() {
    use metal_translate::ir::{AddressSpace, Instruction};

    let word = Type::u32();
    let local = Type::Pointer {
        space: AddressSpace::Thread,
        pointee: Box::new(word.clone()),
    };
    let buffer = Type::Pointer {
        space: AddressSpace::Device,
        pointee: Box::new(word.clone()),
    };
    let module = Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![Function {
            name: "scratch".to_string(),
            result: Type::Void,
            parameters: vec![Parameter {
                value: ValueId(0),
                name: "output".to_string(),
                ty: buffer.clone(),
            }],
            values: vec![buffer.clone(), local.clone(), word.clone(), buffer.clone()],
            blocks: vec![
                Block {
                    id: BlockId(0),
                    name: "entry".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Branch(BlockId(1)),
                },
                Block {
                    id: BlockId(1),
                    name: "body".to_string(),
                    instructions: vec![
                        Instruction {
                            result: Some(ValueId(1)),
                            kind: InstructionKind::LocalVariable { ty: word.clone() },
                        },
                        Instruction {
                            result: None,
                            kind: InstructionKind::Store {
                                pointer: Operand::Value(ValueId(1)),
                                value: Operand::u32(7),
                            },
                        },
                        Instruction {
                            result: Some(ValueId(2)),
                            kind: InstructionKind::Load {
                                pointer: Operand::Value(ValueId(1)),
                            },
                        },
                        Instruction {
                            result: Some(ValueId(3)),
                            kind: InstructionKind::ElementAddress {
                                pointer: Operand::Value(ValueId(0)),
                                index: Operand::u32(0),
                            },
                        },
                        Instruction {
                            result: None,
                            kind: InstructionKind::Store {
                                pointer: Operand::Value(ValueId(3)),
                                value: Operand::Value(ValueId(2)),
                            },
                        },
                    ],
                    terminator: Terminator::Return(None),
                },
            ],
        }],
        entries: vec![EntryPoint {
            function: 0,
            stage: Stage::Kernel,
            resources: vec![Resource {
                argument: 0,
                kind: ResourceKind::Buffer,
                index: 0,
                count: 1,
                access: Access::Write,
                ty: buffer,
                name: Some("output".to_string()),
            }],
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
    };
    let library = emit_spirv(&module, &TargetProfile::vulkan_1_3()).expect("the module emits");
    let parsed = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    let entry = &parsed.functions[0].blocks[0];
    assert!(
        entry.instructions.iter().any(|instruction| {
            instruction.class.opcode == Op::Variable
                && instruction.operands.first()
                    == Some(&rspirv::dr::Operand::StorageClass(
                        rspirv::spirv::StorageClass::Function,
                    ))
        }),
        "the local is declared in the entry block"
    );
    assert!(
        parsed.functions[0].blocks[1]
            .instructions
            .iter()
            .all(|instruction| instruction.class.opcode != Op::Variable),
        "no variable is left in the block the alloca was written in"
    );
}

/// A kernel with one `device uint*` buffer, whose body is what the caller
/// supplies. Enough shape for the emitter; the buffer is the atomic's target.
fn buffer_kernel(values: Vec<Type>, instructions: Vec<Instruction>) -> Module {
    let buffer = Type::Pointer {
        space: AddressSpace::Device,
        pointee: Box::new(Type::Array {
            element: Box::new(Type::u32()),
            count: 4,
        }),
    };
    let mut all = vec![buffer.clone()];
    all.extend(values);
    Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![Function {
            name: "main0".to_string(),
            result: Type::Void,
            parameters: vec![Parameter {
                value: ValueId(0),
                name: "counters".to_string(),
                ty: buffer.clone(),
            }],
            values: all,
            blocks: vec![Block {
                id: BlockId(0),
                name: "entry".to_string(),
                instructions,
                terminator: Terminator::Return(None),
            }],
        }],
        entries: vec![EntryPoint {
            function: 0,
            stage: Stage::Kernel,
            resources: vec![Resource {
                argument: 0,
                kind: ResourceKind::Buffer,
                index: 0,
                count: 1,
                access: Access::ReadWrite,
                ty: buffer,
                name: Some("counters".to_string()),
            }],
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
    }
}

fn typed_buffer_kernel(element: Type, values: Vec<Type>, instructions: Vec<Instruction>) -> Module {
    let buffer = Type::Pointer {
        space: AddressSpace::Device,
        pointee: Box::new(element),
    };
    let mut all = vec![buffer.clone()];
    all.extend(values);
    Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![Function {
            name: "small_values".to_string(),
            result: Type::Void,
            parameters: vec![Parameter {
                value: ValueId(0),
                name: "values".to_string(),
                ty: buffer.clone(),
            }],
            values: all,
            blocks: vec![Block {
                id: BlockId(0),
                name: "entry".to_string(),
                instructions,
                terminator: Terminator::Return(None),
            }],
        }],
        entries: vec![EntryPoint {
            function: 0,
            stage: Stage::Kernel,
            resources: vec![Resource {
                argument: 0,
                kind: ResourceKind::Buffer,
                index: 0,
                count: 1,
                access: Access::ReadWrite,
                ty: buffer,
                name: Some("values".to_string()),
            }],
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
    }
}

#[test]
fn small_storage_buffer_types_use_storage_capabilities() {
    for (element, capability) in [
        (
            Type::Int {
                bits: 8,
                signed: false,
            },
            "StorageBuffer8BitAccess",
        ),
        (
            Type::Int {
                bits: 16,
                signed: true,
            },
            "StorageBuffer16BitAccess",
        ),
        (Type::Float { bits: 16 }, "StorageBuffer16BitAccess"),
    ] {
        let module = typed_buffer_kernel(element, Vec::new(), Vec::new());
        let mut target = TargetProfile::vulkan_1_3();
        let error = emit_spirv(&module, &target).unwrap_err();
        assert!(error.to_string().contains(capability));

        match capability {
            "StorageBuffer8BitAccess" => target.storage_buffer8_bit_access = true,
            "StorageBuffer16BitAccess" => target.storage_buffer16_bit_access = true,
            _ => unreachable!(),
        }
        let artifact = emit_spirv(&module, &target).unwrap().entries.remove(0);
        assert!(
            artifact
                .required
                .capabilities
                .iter()
                .any(|it| it == capability)
        );
        assert!(
            !artifact
                .required
                .capabilities
                .iter()
                .any(|it| { matches!(it.as_str(), "Int8" | "Int16" | "Float16") })
        );
    }
}

#[test]
fn spirv_1_3_declares_the_8bit_storage_extension() {
    let module = typed_buffer_kernel(
        Type::Int {
            bits: 8,
            signed: false,
        },
        Vec::new(),
        Vec::new(),
    );
    let target = TargetProfile {
        vulkan_version: (1, 1),
        spirv_version: (1, 3),
        storage_buffer8_bit_access: true,
        ..TargetProfile::vulkan_1_3()
    };
    let artifact = emit_spirv(&module, &target).unwrap().entries.remove(0);
    assert_eq!(artifact.required.extensions, ["SPV_KHR_8bit_storage"]);
}

#[test]
fn storage_only_8bit_values_can_be_loaded_and_widened() {
    let byte = Type::Int {
        bits: 8,
        signed: false,
    };
    let byte_pointer = Type::Pointer {
        space: AddressSpace::Device,
        pointee: Box::new(byte.clone()),
    };
    let module = typed_buffer_kernel(
        byte.clone(),
        vec![byte_pointer, byte.clone(), Type::u32()],
        vec![
            Instruction {
                result: Some(ValueId(1)),
                kind: InstructionKind::ElementAddress {
                    pointer: Operand::Value(ValueId(0)),
                    index: Operand::u32(0),
                },
            },
            Instruction {
                result: Some(ValueId(2)),
                kind: InstructionKind::Load {
                    pointer: Operand::Value(ValueId(1)),
                },
            },
            Instruction {
                result: Some(ValueId(3)),
                kind: InstructionKind::Convert {
                    op: ConvertOp::ZeroExtend,
                    value: Operand::Value(ValueId(2)),
                },
            },
        ],
    );
    let target = TargetProfile {
        storage_buffer8_bit_access: true,
        ..TargetProfile::vulkan_1_3()
    };
    let artifact = emit_spirv(&module, &target).unwrap().entries.remove(0);
    assert!(
        artifact
            .required
            .capabilities
            .contains(&"StorageBuffer8BitAccess".to_string())
    );
    assert!(!artifact.required.capabilities.contains(&"Int8".to_string()));
}

#[test]
fn arithmetic_on_8bit_values_requires_shader_int8() {
    let byte = Type::Int {
        bits: 8,
        signed: false,
    };
    let byte_pointer = Type::Pointer {
        space: AddressSpace::Device,
        pointee: Box::new(byte.clone()),
    };
    let module = typed_buffer_kernel(
        byte.clone(),
        vec![byte_pointer, byte.clone(), byte],
        vec![
            Instruction {
                result: Some(ValueId(1)),
                kind: InstructionKind::ElementAddress {
                    pointer: Operand::Value(ValueId(0)),
                    index: Operand::u32(0),
                },
            },
            Instruction {
                result: Some(ValueId(2)),
                kind: InstructionKind::Load {
                    pointer: Operand::Value(ValueId(1)),
                },
            },
            Instruction {
                result: Some(ValueId(3)),
                kind: InstructionKind::IntegerBinary {
                    op: IntegerBinaryOp::Add,
                    lhs: Operand::Value(ValueId(2)),
                    rhs: Operand::Value(ValueId(2)),
                },
            },
        ],
    );
    let mut target = TargetProfile {
        storage_buffer8_bit_access: true,
        ..TargetProfile::vulkan_1_3()
    };
    let error = emit_spirv(&module, &target).unwrap_err();
    assert!(error.to_string().contains("Int8"));

    target.shader_int8 = true;
    let artifact = emit_spirv(&module, &target).unwrap().entries.remove(0);
    assert!(artifact.required.capabilities.contains(&"Int8".to_string()));
}

#[test]
fn threadgroup_barrier_becomes_a_control_barrier() {
    let module = buffer_kernel(
        Vec::new(),
        vec![Instruction {
            result: None,
            kind: InstructionKind::Barrier {
                scope: BarrierScope::Threadgroup,
                memory: BarrierMemory {
                    threadgroup: true,
                    ..BarrierMemory::default()
                },
            },
        }],
    );
    module.verify().unwrap();
    let artifact = emit_spirv(&module, &TargetProfile::vulkan_1_3())
        .unwrap()
        .entries
        .remove(0);
    let parsed = rspirv::dr::load_words(&artifact.words).unwrap();
    let barrier = parsed.functions[0].blocks[0]
        .instructions
        .iter()
        .find(|instruction| instruction.class.opcode == Op::ControlBarrier)
        .expect("a control barrier");
    // The execution scope is the workgroup. A SIMD-group scope compiles and
    // validates, and synchronises a fraction of the threadgroup: on a
    // sixty-four thread group over thirty-two lane SIMD-groups, half of the
    // writes a barrier is there to publish are still in flight after it.
    let scope = |operand: &rspirv::dr::Operand| {
        let id = operand.id_ref_any().expect("a scope operand");
        parsed
            .types_global_values
            .iter()
            .find(|instruction| instruction.result_id == Some(id))
            .and_then(|instruction| instruction.operands.first())
            .and_then(|operand| match operand {
                rspirv::dr::Operand::LiteralBit32(value) => Some(*value),
                _ => None,
            })
            .expect("a constant scope")
    };
    assert_eq!(scope(&barrier.operands[0]), Scope::Workgroup as u32);
    assert_eq!(scope(&barrier.operands[1]), Scope::Workgroup as u32);
}

#[test]
fn a_device_atomic_add_becomes_an_atomic_i_add() {
    let element = Type::Pointer {
        space: AddressSpace::Device,
        pointee: Box::new(Type::u32()),
    };
    let module = buffer_kernel(
        vec![element, Type::u32()],
        vec![
            Instruction {
                result: Some(ValueId(1)),
                kind: InstructionKind::BufferAddress {
                    pointer: Operand::Value(ValueId(0)),
                    indices: vec![
                        Operand::Integer {
                            ty: Type::u32(),
                            value: 0,
                        },
                        Operand::Integer {
                            ty: Type::u32(),
                            value: 2,
                        },
                    ],
                },
            },
            Instruction {
                result: Some(ValueId(2)),
                kind: InstructionKind::Atomic {
                    op: AtomicOp::Add,
                    pointer: Operand::Value(ValueId(1)),
                    value: Operand::Integer {
                        ty: Type::u32(),
                        value: 1,
                    },
                    order: MemoryOrder::Relaxed,
                    scope: BarrierScope::Device,
                },
            },
        ],
    );
    module.verify().unwrap();
    let artifact = emit_spirv(&module, &TargetProfile::vulkan_1_3())
        .unwrap()
        .entries
        .remove(0);
    let parsed = rspirv::dr::load_words(&artifact.words).unwrap();
    assert!(
        parsed.functions[0].blocks[0]
            .instructions
            .iter()
            .any(|instruction| instruction.class.opcode == Op::AtomicIAdd)
    );
}

#[test]
fn a_pointer_phi_needs_the_variable_pointers_capability() {
    let pointer = Type::Pointer {
        space: AddressSpace::Device,
        pointee: Box::new(Type::u32()),
    };
    let module = buffer_kernel(
        vec![pointer],
        vec![Instruction {
            result: Some(ValueId(1)),
            kind: InstructionKind::Phi {
                incomings: Vec::new(),
            },
        }],
    );
    let target = TargetProfile::vulkan_1_3();
    let error = emit_spirv(&module, &target).unwrap_err();
    assert_eq!(error.diagnostics[0].phase, Phase::SpirvEmission);

    let target = TargetProfile {
        variable_pointers_storage_buffer: true,
        ..target
    };
    let artifact = emit_spirv(&module, &target).unwrap().entries.remove(0);
    assert!(
        artifact
            .required
            .capabilities
            .iter()
            .any(|capability| capability == "VariablePointersStorageBuffer")
    );
}

#[test]
fn a_constant_address_space_pointer_phi_keeps_its_buffer_storage_class() {
    let array = Type::Array {
        element: Box::new(Type::u32()),
        count: 4,
    };
    let buffer = Type::Pointer {
        space: AddressSpace::Constant,
        pointee: Box::new(array),
    };
    let element = Type::Pointer {
        space: AddressSpace::Constant,
        pointee: Box::new(Type::u32()),
    };
    let module = Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![Function {
            name: "main0".to_string(),
            result: Type::Void,
            parameters: vec![Parameter {
                value: ValueId(0),
                name: "input".to_string(),
                ty: buffer.clone(),
            }],
            values: vec![buffer.clone(), element.clone(), element],
            blocks: vec![
                Block {
                    id: BlockId(0),
                    name: "entry".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Branch(BlockId(1)),
                },
                Block {
                    id: BlockId(1),
                    name: "address".to_string(),
                    instructions: vec![Instruction {
                        result: Some(ValueId(1)),
                        kind: InstructionKind::ElementAddress {
                            pointer: Operand::Value(ValueId(0)),
                            index: Operand::u32(0),
                        },
                    }],
                    terminator: Terminator::Branch(BlockId(2)),
                },
                Block {
                    id: BlockId(2),
                    name: "merge".to_string(),
                    instructions: vec![Instruction {
                        result: Some(ValueId(2)),
                        kind: InstructionKind::Phi {
                            incomings: vec![(BlockId(1), Operand::Value(ValueId(1)))],
                        },
                    }],
                    terminator: Terminator::Return(None),
                },
            ],
        }],
        entries: vec![EntryPoint {
            function: 0,
            stage: Stage::Kernel,
            resources: vec![Resource {
                argument: 0,
                kind: ResourceKind::Buffer,
                index: 0,
                count: 1,
                access: Access::Read,
                ty: buffer,
                name: Some("input".to_string()),
            }],
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
    };
    let target = TargetProfile {
        variable_pointers_storage_buffer: true,
        ..TargetProfile::vulkan_1_3()
    };
    let artifact = emit_spirv(&module, &target).unwrap().entries.remove(0);
    assert!(
        artifact
            .required
            .capabilities
            .contains(&"VariablePointersStorageBuffer".to_string())
    );
}

#[test]
fn fragment_demote_is_negotiated_and_fully_reflected() {
    let module = Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![Function {
            name: "fragment_main".to_string(),
            result: Type::Void,
            parameters: Vec::new(),
            values: Vec::new(),
            blocks: vec![Block {
                id: BlockId(0),
                name: "entry".to_string(),
                instructions: vec![Instruction {
                    result: None,
                    kind: InstructionKind::DiscardFragment,
                }],
                terminator: Terminator::Return(None),
            }],
        }],
        entries: vec![EntryPoint {
            function: 0,
            stage: Stage::Fragment,
            resources: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
    };
    let mut target = TargetProfile {
        vulkan_version: (1, 2),
        spirv_version: (1, 5),
        ..TargetProfile::vulkan_1_3()
    };
    target.demote_to_helper_invocation = false;
    let error = emit_spirv(&module, &target).unwrap_err();
    assert!(error.to_string().contains("DemoteToHelperInvocation"));

    target.demote_to_helper_invocation = true;
    let entry = emit_spirv(&module, &target).unwrap().entries.remove(0);
    assert!(
        entry
            .required
            .capabilities
            .contains(&"DemoteToHelperInvocation".to_string())
    );
    assert_eq!(
        entry.required.extensions,
        ["SPV_EXT_demote_to_helper_invocation"]
    );
    assert_eq!(target.unsupported_requirement(&entry.required), None);
    target.demote_to_helper_invocation = false;
    assert_eq!(
        target.unsupported_requirement(&entry.required),
        Some("DemoteToHelperInvocation")
    );
}

fn uvec(lanes: u8) -> Type {
    Type::Vector {
        element: Box::new(Type::u32()),
        lanes,
    }
}

/// The instructions of `op` in the module's function bodies.
fn body_instructions(module: &rspirv::dr::Module, op: Op) -> Vec<rspirv::dr::Instruction> {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| instruction.class.opcode == op)
        .cloned()
        .collect()
}

/// Whether `id` names `OpTypeInt 32 1`, or a vector of it.
fn is_signed_int32(module: &rspirv::dr::Module, id: u32) -> bool {
    use rspirv::dr::Operand as O;
    let Some(ty) = module
        .types_global_values
        .iter()
        .find(|instruction| instruction.result_id == Some(id))
    else {
        return false;
    };
    match ty.class.opcode {
        Op::TypeInt => ty.operands == [O::LiteralBit32(32), O::LiteralBit32(1)],
        Op::TypeVector => {
            matches!(ty.operands[0], O::IdRef(element) if is_signed_int32(module, element))
        }
        _ => false,
    }
}

/// Vulkan requires an image's sampled type to carry its format's
/// signedness, while the IR reads texels signless: the fetch is signed and a
/// bitcast gives the body back its unsigned value.
#[test]
fn a_signed_texture_is_read_through_a_signed_image_type() {
    let texture = Type::Texture(TextureType {
        dimension: TextureDimension::D2,
        array: false,
        multisampled: false,
        depth: false,
        texel: Box::new(Type::u32()),
        signed: true,
        access: Access::Read,
    });
    let coordinate = Operand::Composite {
        ty: uvec(2),
        elements: vec![
            Operand::Integer {
                ty: Type::u32(),
                value: 0,
            };
            2
        ],
    };
    let module = Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![Function {
            name: "read_ids".to_string(),
            result: Type::Void,
            parameters: vec![Parameter {
                value: ValueId(0),
                name: "ids".to_string(),
                ty: texture.clone(),
            }],
            values: vec![texture.clone(), uvec(4)],
            blocks: vec![Block {
                id: BlockId(0),
                name: "entry".to_string(),
                instructions: vec![Instruction {
                    result: Some(ValueId(1)),
                    kind: InstructionKind::TextureRead {
                        texture: Operand::Value(ValueId(0)),
                        coordinate,
                        array_index: None,
                        lod: None,
                        sample: None,
                    },
                }],
                terminator: Terminator::Return(None),
            }],
        }],
        entries: vec![EntryPoint {
            function: 0,
            stage: Stage::Kernel,
            resources: vec![Resource {
                argument: 0,
                kind: ResourceKind::Texture,
                index: 0,
                count: 1,
                access: Access::Read,
                ty: texture,
                name: Some("ids".to_string()),
            }],
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
    };

    let artifact = emit_spirv(&module, &TargetProfile::vulkan_1_3())
        .unwrap()
        .entries
        .remove(0);
    let parsed = rspirv::dr::load_words(&artifact.words).unwrap();
    let image = parsed
        .types_global_values
        .iter()
        .find(|instruction| instruction.class.opcode == Op::TypeImage)
        .expect("an image type");
    assert!(is_signed_int32(&parsed, image.operands[0].unwrap_id_ref()));
    let [fetch] = &body_instructions(&parsed, Op::ImageFetch)[..] else {
        panic!("expected one fetch");
    };
    assert!(is_signed_int32(&parsed, fetch.result_type.unwrap()));
    let [bitcast] = &body_instructions(&parsed, Op::Bitcast)[..] else {
        panic!("expected one bitcast");
    };
    assert_eq!(
        bitcast.operands[0].unwrap_id_ref(),
        fetch.result_id.unwrap()
    );
    assert!(!is_signed_int32(&parsed, bitcast.result_type.unwrap()));
}

/// A `short4` attribute has a `SINT` Vulkan format, so the input it lands in
/// must be signed even though the body reads it signless.
#[test]
fn a_signed_vertex_attribute_is_declared_signed() {
    let module = Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![Function {
            name: "vertex_ids".to_string(),
            result: Type::Void,
            parameters: vec![Parameter {
                value: ValueId(0),
                name: "ids".to_string(),
                ty: uvec(4),
            }],
            values: vec![uvec(4)],
            blocks: vec![Block {
                id: BlockId(0),
                name: "entry".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Return(None),
            }],
        }],
        entries: vec![EntryPoint {
            function: 0,
            stage: Stage::Vertex,
            resources: Vec::new(),
            inputs: vec![StageInput {
                argument: 0,
                binding: StageBinding::Location {
                    location: 0,
                    interpolation: None,
                },
                name: Some("ids".to_string()),
                signed: true,
            }],
            outputs: Vec::new(),
        }],
    };

    let artifact = emit_spirv(&module, &TargetProfile::vulkan_1_3())
        .unwrap()
        .entries
        .remove(0);
    let parsed = rspirv::dr::load_words(&artifact.words).unwrap();
    let [load] = &body_instructions(&parsed, Op::Load)[..] else {
        panic!("expected one load");
    };
    assert!(is_signed_int32(&parsed, load.result_type.unwrap()));
    let [bitcast] = &body_instructions(&parsed, Op::Bitcast)[..] else {
        panic!("expected one bitcast");
    };
    assert_eq!(bitcast.operands[0].unwrap_id_ref(), load.result_id.unwrap());
    assert!(!is_signed_int32(&parsed, bitcast.result_type.unwrap()));
}
