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
             goto end;\n\
         }",
    )
    .unwrap_err();
    assert_eq!(error.diagnostics[0].phase, Phase::AirSyntax);
    assert_eq!(error.diagnostics[0].line, Some(3));
}

#[test]
fn source_msl_translation_errors_include_nearby_source() {
    let source = "// Prelude\n\
                  // More prelude\n\
                  kernel void f(\n\
                      device uint* output [[raster_order_group(0)]]\n\
                  ) { return; }\n\
                  // Epilogue";
    let error = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap_err();

    assert_eq!(error.diagnostics[0].line, Some(4));
    assert_eq!(
        error.to_string(),
        concat!(
            "AirSyntax at line 4: unsupported parameter attribute [[raster_order_group]]\n\n",
            "MSL source context:\n",
            "  2 | // More prelude\n",
            "  3 | kernel void f(\n",
            "> 4 | device uint* output [[raster_order_group(0)]]\n",
            "  5 | ) { return; }\n",
            "  6 | // Epilogue",
        )
    );
}

/// A vertex and a fragment function as Unity's shader compiler writes them:
/// a uniform structure, a `[[stage_in]]` structure, swizzles, `fma`, and a
/// varying linked by its `user()` name.
const UNITY_STYLE: &str = "#include <metal_stdlib>\n\
    using namespace metal;\n\
    #if !(__HAVE_FMA__)\n\
    #define fma(a,b,c) ((a) * (b) + (c))\n\
    #endif\n\
    #ifndef XLT_REMAP_O\n\
    #define XLT_REMAP_O {0, 1, 2, 3, 4, 5, 6, 7}\n\
    #endif\n\
    constexpr constant uint xlt_remap_o[] = XLT_REMAP_O;\n\
    struct Globals_Type { float4 _Color; float3 _Offset; float4 hlslcc_mtx4x4M[4]; int _Count; };\n\
    struct VIn { float4 POSITION0 [[ attribute(0) ]]; float2 TEXCOORD0 [[ attribute(3) ]]; };\n\
    struct VOut { float4 mtl_Position [[ position ]]; float2 TEXCOORD0 [[ user(TEXCOORD0) ]]; float4 COLOR0 [[ user(COLOR0) ]]; };\n\
    vertex VOut vmain(constant Globals_Type& Globals [[ buffer(0) ]], VIn input [[ stage_in ]]) {\n\
        VOut output;\n\
        float4 u_xlat0;\n\
        int u_xlati1;\n\
        u_xlat0 = input.POSITION0.yyyy * Globals.hlslcc_mtx4x4M[1];\n\
        u_xlat0 = fma(Globals.hlslcc_mtx4x4M[0], input.POSITION0.xxxx, u_xlat0);\n\
        u_xlat0.xyz = u_xlat0.xyz + Globals._Offset.xyz;\n\
        u_xlati1 = 0x0;\n\
        while(true){\n\
            if(u_xlati1>=Globals._Count){break;}\n\
            u_xlat0.x = u_xlat0.x * 0.5;\n\
            u_xlati1 = u_xlati1 + 0x1;\n\
        }\n\
        output.mtl_Position = u_xlat0;\n\
        output.TEXCOORD0.xy = input.TEXCOORD0.xy;\n\
        output.COLOR0 = Globals._Color;\n\
        return output;\n\
    }\n\
    struct FIn { float4 COLOR0 [[ user(COLOR0) ]]; float2 TEXCOORD0 [[ user(TEXCOORD0) ]]; };\n\
    struct FOut { float4 SV_Target0 [[ color(xlt_remap_o[0]) ]]; };\n\
    fragment FOut fmain(texture2d<float, access::sample > _MainTex [[ texture(0) ]],\n\
        sampler sampler_MainTex [[ sampler (0) ]], FIn input [[ stage_in ]]) {\n\
        FOut output;\n\
        float4 u_xlat0;\n\
        bool u_xlatb1;\n\
        u_xlat0 = _MainTex.sample(sampler_MainTex, input.TEXCOORD0.xy);\n\
        u_xlatb1 = u_xlat0.w<0.5;\n\
        if(((int(u_xlatb1) * int(0xffffffffu)))!=0){discard_fragment();}\n\
        output.SV_Target0 = u_xlat0 * input.COLOR0;\n\
        return output;\n\
    }\n";

#[test]
fn source_msl_compiles_unity_style_vertex_and_fragment_functions() {
    let target = TargetProfile {
        demote_to_helper_invocation: true,
        ..TargetProfile::vulkan_1_3()
    };
    let library = translate_msl(UNITY_STYLE, &target).unwrap();
    let names = library
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["vmain", "fmain"]);
    let vertex = &library.entries[0];
    let fragment = &library.entries[1];
    // The attributes keep the indices the source gave them.
    let attributes = vertex
        .vertex_inputs
        .iter()
        .map(|input| (input.name.as_str(), input.location, input.data_type))
        .collect::<Vec<_>>();
    assert_eq!(attributes, [("POSITION0", 0, 6), ("TEXCOORD0", 3, 4)]);
    // Each stage numbers its varyings in its own order; the names are what
    // links them.
    let varyings = |entry: &metal_translate::EntryArtifact| {
        entry
            .varyings
            .iter()
            .map(|varying| (varying.name.clone(), varying.location))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        varyings(vertex),
        [
            ("user(TEXCOORD0)".to_owned(), 0),
            ("user(COLOR0)".to_owned(), 1)
        ]
    );
    assert_eq!(
        varyings(fragment),
        [
            ("user(COLOR0)".to_owned(), 0),
            ("user(TEXCOORD0)".to_owned(), 1)
        ]
    );
}

#[test]
fn source_msl_preprocessor_macros_replace_defaults() {
    let module = metal_translate::msl::parse_msl_with_macros(
        UNITY_STYLE,
        &[(
            "XLT_REMAP_O".to_owned(),
            "{3, 0, 0, 0, 0, 0, 0, 0}".to_owned(),
        )],
    )
    .unwrap();
    let fragment = &module.entries[1];
    assert_eq!(
        fragment.outputs[0].binding,
        metal_translate::ir::StageBinding::RenderTarget { index: 3 }
    );
}

#[test]
fn source_msl_compiles_a_compound_literal_and_half_values() {
    let source = "#include <metal_stdlib>\n\
        using namespace metal;\n\
        struct ApplicationData { float4 pos [[attribute(0)]]; half4 color [[attribute(3)]]; };\n\
        struct RasterizerData { float4 pos [[position]]; half4 color; };\n\
        vertex RasterizerData clear_vprog(ApplicationData input [[stage_in]])\n\
        {\n\
            return (RasterizerData) { .pos = input.pos, .color = input.color };\n\
        }\n\
        fragment half4 clear_fshader(RasterizerData input [[stage_in]])\n\
        {\n\
            return input.color;\n\
        }\0";
    let target = TargetProfile {
        shader_float16: true,
        ..TargetProfile::vulkan_1_3()
    };
    let library = translate_msl(source, &target).unwrap();
    assert_eq!(library.entries.len(), 2);
    // An unattributed member is a varying named by the member.
    assert_eq!(library.entries[0].varyings[0].name, "user(color)");
    assert_eq!(library.entries[1].varyings[0].name, "user(color)");
}

#[test]
fn source_msl_threadgroup_atomics_reach_workgroup_and_buffer_memory() {
    let source = "#include <metal_stdlib>\n\
        using namespace metal;\n\
        struct Bin { uint value[1]; };\n\
        kernel void histogram(device Bin *out [[ buffer(0) ]],\n\
            uint3 local [[ thread_position_in_threadgroup ]])\n\
        {\n\
            threadgroup Bin bins[64];\n\
            bins[local.x].value[0] = 0u;\n\
            threadgroup_barrier(mem_flags::mem_threadgroup);\n\
            atomic_fetch_add_explicit(reinterpret_cast<threadgroup atomic_uint *>(&bins[local.x & 63u].value[0]), 1, memory_order::memory_order_relaxed);\n\
            threadgroup_barrier(mem_flags::mem_threadgroup | mem_flags::mem_device);\n\
            int count = int(bins[local.x].value[0]);\n\
            atomic_fetch_max_explicit(reinterpret_cast<device atomic_int *>(&out[local.x].value[0]), count, memory_order::memory_order_relaxed);\n\
        }\0";
    let library = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap();
    let module = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    let workgroup = module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::Variable)
        .filter(|instruction| {
            instruction.operands.first()
                == Some(&rspirv::dr::Operand::StorageClass(
                    rspirv::spirv::StorageClass::Workgroup,
                ))
        })
        .count();
    assert_eq!(workgroup, 1);
    assert_eq!(body_instructions(&module, Op::ControlBarrier).len(), 2);
    assert_eq!(body_instructions(&module, Op::AtomicIAdd).len(), 1);
    // `atomic_int` makes the maximum signed, whatever the place declared.
    assert_eq!(body_instructions(&module, Op::AtomicSMax).len(), 1);
}

/// Unity's blit to the drawable, which describes its sampler in the source.
const UNITY_BLIT: &str = "#include <metal_stdlib>\n\
    using namespace metal;\n\
    struct AppData { float4 in_pos [[attribute(0)]]; };\n\
    struct VProgOutput { float4 out_pos [[position]]; float2 texcoord; };\n\
    struct FShaderOutput { half4 frag_data [[color(0)]]; };\n\
    vertex VProgOutput vprog(AppData input [[stage_in]])\n\
    {\n\
        VProgOutput out = { float4(input.in_pos.xy, 0, 1), input.in_pos.zw };\n\
        return out;\n\
    }\n\
    constexpr sampler blit_tex_sampler(address::clamp_to_edge, filter::linear);\n\
    fragment FShaderOutput fshader(VProgOutput input [[stage_in]], texture2d<half> tex [[texture(0)]])\n\
    {\n\
        FShaderOutput out = { tex.sample(blit_tex_sampler, input.texcoord) };\n\
        return out;\n\
    }\0";

#[test]
fn source_msl_a_file_scope_constexpr_sampler_becomes_an_inline_sampler() {
    let target = TargetProfile {
        shader_float16: true,
        ..TargetProfile::vulkan_1_3()
    };
    let library = translate_msl(UNITY_BLIT, &target).unwrap();
    let fragment = library
        .entries
        .iter()
        .find(|entry| entry.name == "fshader")
        .expect("the fragment function");
    let sampler = fragment
        .resources
        .iter()
        .find(|resource| resource.kind == ResourceKind::Sampler)
        .expect("the inline sampler is a sampler binding");
    assert_eq!(
        sampler.metal_index,
        metal_translate::ir::INLINE_SAMPLER_BASE
    );
    let inline = sampler.inline_sampler.expect("described by the shader");
    assert_eq!(
        (inline.min_filter, inline.mag_filter, inline.mip_filter),
        (1, 1, 0)
    );
    assert_eq!(inline.address, [0; 3]);
    assert!(inline.normalized_coordinates);
    // The vertex function never sees it, and takes no binding for it.
    let vertex = library.entries.iter().find(|e| e.name == "vprog").unwrap();
    assert!(vertex.resources.iter().all(|r| r.inline_sampler.is_none()));
}

#[test]
fn source_msl_a_local_constexpr_sampler_reads_every_argument() {
    let source = "#include <metal_stdlib>\n\
        using namespace metal;\n\
        struct Varyings { float4 pos [[position]]; float2 uv; };\n\
        fragment float4 shade(Varyings input [[stage_in]],\n\
            texture2d<float> tex [[ texture(0) ]])\n\
        {\n\
            constexpr sampler s(s_address::repeat, t_address::mirrored_repeat,\n\
                min_filter::linear, mip_filter::linear, max_anisotropy(8),\n\
                lod_clamp(0.5, 4.0), border_color::opaque_white);\n\
            return tex.sample(s, input.uv);\n\
        }\0";
    let library = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap();
    let inline = library.entries[0]
        .resources
        .iter()
        .find_map(|resource| resource.inline_sampler)
        .expect("an inline sampler");
    assert_eq!(inline.address, [2, 3, 0]);
    assert_eq!(
        (inline.min_filter, inline.mag_filter, inline.mip_filter),
        (1, 0, 2)
    );
    assert_eq!(inline.max_anisotropy, 8);
    assert_eq!(f32::from_bits(inline.lod_min_clamp), 0.5);
    assert_eq!(f32::from_bits(inline.lod_max_clamp), 4.0);
    assert_eq!(inline.border_color, 2);
}

#[test]
fn source_msl_rejects_an_unknown_constexpr_sampler_argument() {
    let source = "#include <metal_stdlib>\n\
        using namespace metal;\n\
        constexpr sampler s(address::clamp_to_edge, reduction::minimum);\n\
        fragment float4 f(float4 pos [[position]], texture2d<float> t [[texture(0)]])\n\
        { return t.sample(s, pos.xy); }\0";
    let error = parse_msl(source).unwrap_err();
    assert!(
        error.diagnostics[0].message.contains("reduction::minimum"),
        "{}",
        error.diagnostics[0].message
    );
}

/// The width of the float or integer type `id` names.
fn scalar_width(module: &rspirv::dr::Module, id: u32) -> Option<u32> {
    let ty = module
        .types_global_values
        .iter()
        .find(|instruction| instruction.result_id == Some(id))?;
    match ty.class.opcode {
        Op::TypeFloat | Op::TypeInt => Some(ty.operands[0].unwrap_literal_bit32()),
        Op::TypeVector => scalar_width(module, ty.operands[0].unwrap_id_ref()),
        _ => None,
    }
}

fn image_sampled_width(module: &rspirv::dr::Module) -> Option<u32> {
    let image = module
        .types_global_values
        .iter()
        .find(|instruction| instruction.class.opcode == Op::TypeImage)?;
    scalar_width(module, image.operands[0].unwrap_id_ref())
}

/// Vulkan's sampled type is 32-bit, so a `half` texture samples through a
/// 32-bit image and narrows the result before computing with it.
#[test]
fn source_msl_a_half_texture_computes_at_half_precision() {
    let source = "#include <metal_stdlib>\n\
        using namespace metal;\n\
        struct Varyings { float4 pos [[position]]; float2 uv; };\n\
        fragment half4 shade(Varyings input [[stage_in]],\n\
            texture2d<half, access::sample> tex [[ texture(0) ]],\n\
            sampler s [[ sampler(0) ]])\n\
        {\n\
            return tex.sample(s, input.uv) * 3.0h;\n\
        }\0";
    let target = TargetProfile {
        shader_float16: true,
        ..TargetProfile::vulkan_1_3()
    };
    let library = translate_msl(source, &target).unwrap();
    let module = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    assert_eq!(image_sampled_width(&module), Some(32));
    let [sample] = &body_instructions(&module, Op::ImageSampleImplicitLod)[..] else {
        panic!("expected one sample");
    };
    let convert = body_instructions(&module, Op::FConvert)
        .into_iter()
        .find(|convert| convert.operands[0].unwrap_id_ref() == sample.result_id.unwrap())
        .expect("the sample is narrowed");
    assert_eq!(
        scalar_width(&module, convert.result_type.unwrap()),
        Some(16)
    );
    let [multiply] = &body_instructions(&module, Op::FMul)[..] else {
        panic!("expected one multiply");
    };
    assert_eq!(
        scalar_width(&module, multiply.result_type.unwrap()),
        Some(16)
    );
}

/// A `ushort` texture reads and writes 32-bit texels and computes with
/// 16-bit ones, so an addition wraps as Metal's does.
#[test]
fn source_msl_a_short_texture_computes_at_short_width() {
    let target = TargetProfile {
        shader_int16: true,
        shader_storage_image_read_without_format: true,
        shader_storage_image_write_without_format: true,
        ..TargetProfile::vulkan_1_3()
    };
    let library = translate_msl(SHORT_TEXTURE_KERNEL, &target).unwrap();
    let module = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    assert_eq!(image_sampled_width(&module), Some(32));
    let [read] = &body_instructions(&module, Op::ImageRead)[..] else {
        panic!("expected one read");
    };
    assert_eq!(scalar_width(&module, read.result_type.unwrap()), Some(32));
    let [add] = &body_instructions(&module, Op::IAdd)[..] else {
        panic!("expected one addition");
    };
    assert_eq!(scalar_width(&module, add.result_type.unwrap()), Some(16));
    let [write] = &body_instructions(&module, Op::ImageWrite)[..] else {
        panic!("expected one write");
    };
    let written = write.operands[2].unwrap_id_ref();
    let widen = body_instructions(&module, Op::UConvert)
        .into_iter()
        .find(|convert| convert.result_id == Some(written))
        .expect("the written texel is widened");
    assert_eq!(scalar_width(&module, widen.result_type.unwrap()), Some(32));
}

/// Without `shaderInt16` a `ushort` texture's texels are computed as `uint`
/// rather than rejected.
#[test]
fn source_msl_a_short_texture_widens_without_shader_int16() {
    let target = TargetProfile {
        shader_storage_image_read_without_format: true,
        shader_storage_image_write_without_format: true,
        ..TargetProfile::vulkan_1_3()
    };
    let library = translate_msl(SHORT_TEXTURE_KERNEL, &target).unwrap();
    let module = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    let [add] = &body_instructions(&module, Op::IAdd)[..] else {
        panic!("expected one addition");
    };
    assert_eq!(scalar_width(&module, add.result_type.unwrap()), Some(32));
    assert!(body_instructions(&module, Op::UConvert).is_empty());
}

const SHORT_TEXTURE_KERNEL: &str = "#include <metal_stdlib>\n\
    using namespace metal;\n\
    kernel void add(texture2d<ushort, access::read_write> counts [[ texture(0) ]],\n\
        uint2 gid [[ thread_position_in_grid ]])\n\
    {\n\
        counts.write(counts.read(gid) + ushort4(1), gid);\n\
    }\0";

#[test]
fn source_msl_loops_leave_through_one_block() {
    let source = "kernel void f(device uint* output [[buffer(0)]], uint gid [[thread_position_in_grid]]) {\n\
        uint total = 0;\n\
        for (uint i = 0; i < 16; i++) {\n\
            if (i == gid) { continue; }\n\
            if (total > 100u) { break; }\n\
            total += output[i];\n\
            if (output[i] == 7u) { break; }\n\
        }\n\
        output[gid] = total;\n\
    }";
    translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap();
}

/// MojoShader marks each constant it declares as used with a cast to void,
/// and guards `rsqrt` with `<metal_stdlib>`'s `FLT_MAX`.
#[test]
fn source_msl_compiles_mojoshader_idioms() {
    let source = "kernel void f(device float4* output [[buffer(0)]]) {\n\
        const float4 c0 = float4(0.0, 0.5, 1.0, 2.0); (void) c0;\n\
        (void) output[1];\n\
        float r = (c0.x == 0.0) ? FLT_MAX : rsqrt(c0.y);\n\
        output[0] = c0 * r * M_PI_F + float(UINT_MAX);\n\
    }";
    translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap();
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
                inline_sampler: None,
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
                inline_sampler: None,
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
                inline_sampler: None,
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
                inline_sampler: None,
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
                inline_sampler: None,
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
                inline_sampler: None,
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
                inline_sampler: None,
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

/// Metal's `mix` returns `y` exactly at a weight of one, however large `x`
/// is. GLSL's `FMix` leaves the form to the driver, and NVIDIA's rounds `y`
/// away against a large `x`: Roblox's fog trick mixes a fog colour in the
/// millions at weight one and lost every dark colour to it. The lowering must
/// be an endpoint-exact form the driver may not refactor.
#[test]
fn mix_is_exact_at_its_endpoints() {
    let module = buffer_kernel(
        vec![Type::Float { bits: 32 }],
        vec![Instruction {
            result: Some(ValueId(1)),
            kind: InstructionKind::FloatMix {
                first: Operand::f32(3_844_675.0),
                second: Operand::f32(0.02),
                weight: Operand::f32(1.0),
            },
        }],
    );
    module.verify().unwrap();
    let artifact = emit_spirv(&module, &TargetProfile::vulkan_1_3())
        .unwrap()
        .entries
        .remove(0);
    let parsed = rspirv::dr::load_words(&artifact.words).unwrap();
    assert!(
        body_instructions(&parsed, Op::ExtInst).is_empty(),
        "mix is not left to GLSL.std.450 FMix"
    );
    let arithmetic: Vec<u32> = [Op::FMul, Op::FSub, Op::FAdd]
        .into_iter()
        .flat_map(|op| body_instructions(&parsed, op))
        .filter_map(|instruction| instruction.result_id)
        .collect();
    assert_eq!(arithmetic.len(), 4, "x * a, x - x * a, y * a and their sum");
    for id in arithmetic {
        assert!(
            parsed.annotations.iter().any(|annotation| {
                annotation.class.opcode == Op::Decorate
                    && annotation.operands.first() == Some(&rspirv::dr::Operand::IdRef(id))
                    && annotation.operands.get(1)
                        == Some(&rspirv::dr::Operand::Decoration(
                            rspirv::spirv::Decoration::NoContraction,
                        ))
            }),
            "%{id} carries NoContraction"
        );
    }
}

/// Emit a kernel which samples a 2D texture at an explicit level with
/// `offset`, and return the operands of its `OpImageSampleExplicitLod`
/// alongside the parsed module.
fn sample_with_offset(offset: Operand) -> (rspirv::dr::Module, Vec<rspirv::dr::Operand>) {
    let texture = Type::Texture(TextureType {
        dimension: TextureDimension::D2,
        array: false,
        multisampled: false,
        depth: false,
        texel: Box::new(Type::Float { bits: 32 }),
        signed: false,
        access: Access::Sample,
    });
    let float = Type::Float { bits: 32 };
    let float2 = Type::Vector {
        element: Box::new(float.clone()),
        lanes: 2,
    };
    let module = Module {
        static_initializers: Vec::new(),
        globals: Vec::new(),
        functions: vec![Function {
            name: "sample_offset".to_string(),
            result: Type::Void,
            parameters: vec![
                Parameter {
                    value: ValueId(0),
                    name: "source".to_string(),
                    ty: texture.clone(),
                },
                Parameter {
                    value: ValueId(1),
                    name: "linear".to_string(),
                    ty: Type::Sampler,
                },
            ],
            values: vec![
                texture.clone(),
                Type::Sampler,
                Type::Vector {
                    element: Box::new(float.clone()),
                    lanes: 4,
                },
            ],
            blocks: vec![Block {
                id: BlockId(0),
                name: "entry".to_string(),
                instructions: vec![Instruction {
                    result: Some(ValueId(2)),
                    kind: InstructionKind::TextureSample {
                        texture: Operand::Value(ValueId(0)),
                        sampler: Operand::Value(ValueId(1)),
                        coordinate: Operand::Composite {
                            ty: float2,
                            elements: vec![Operand::f32(0.5); 2],
                        },
                        array_index: None,
                        offset: Some(offset),
                        lod: Some(Operand::f32(0.0)),
                        gradient: None,
                        bias: None,
                        gather: None,
                        compare: None,
                    },
                }],
                terminator: Terminator::Return(None),
            }],
        }],
        entries: vec![EntryPoint {
            function: 0,
            stage: Stage::Kernel,
            resources: vec![
                Resource {
                    argument: 0,
                    kind: ResourceKind::Texture,
                    index: 0,
                    count: 1,
                    access: Access::Sample,
                    ty: texture,
                    name: Some("source".to_string()),
                    inline_sampler: None,
                },
                Resource {
                    argument: 1,
                    kind: ResourceKind::Sampler,
                    index: 0,
                    count: 1,
                    access: Access::Read,
                    ty: Type::Sampler,
                    name: Some("linear".to_string()),
                    inline_sampler: None,
                },
            ],
            inputs: Vec::new(),
            outputs: Vec::new(),
        }],
    };
    let artifact = emit_spirv(&module, &TargetProfile::vulkan_1_3())
        .unwrap()
        .entries
        .remove(0);
    let parsed = rspirv::dr::load_words(&artifact.words).unwrap();
    let operands = parsed
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .find(|instruction| instruction.class.opcode == Op::ImageSampleExplicitLod)
        .expect("the sample is emitted")
        .operands
        .clone();
    (parsed, operands)
}

/// AIR's signless `<2 x i32>` offset, as the importer produces it.
fn unsigned_int2(x: i32, y: i32) -> Operand {
    let int = Type::Int {
        bits: 32,
        signed: false,
    };
    Operand::Composite {
        ty: Type::Vector {
            element: Box::new(int.clone()),
            lanes: 2,
        },
        elements: [x, y]
            .into_iter()
            .map(|value| Operand::Integer {
                ty: int.clone(),
                value: u64::from(value as u32),
            })
            .collect(),
    }
}

#[test]
fn emits_a_sampling_offset_as_a_signed_constant() {
    use rspirv::dr::Operand as Word;
    use rspirv::spirv::ImageOperands;

    let (module, operands) = sample_with_offset(unsigned_int2(-1, 2));
    // Sampled image, coordinate, mask, then the Lod and ConstOffset ids.
    let [
        _,
        _,
        Word::ImageOperands(mask),
        Word::IdRef(_),
        Word::IdRef(offset),
    ] = operands.as_slice()
    else {
        panic!("unexpected sample operands: {operands:?}");
    };
    assert_eq!(*mask, ImageOperands::LOD | ImageOperands::CONST_OFFSET);

    let defined = |id: u32| {
        module
            .types_global_values
            .iter()
            .find(|instruction| instruction.result_id == Some(id))
            .unwrap()
    };
    let composite = defined(*offset);
    assert_eq!(composite.class.opcode, Op::ConstantComposite);
    let vector = defined(composite.result_type.unwrap());
    let Word::IdRef(element) = vector.operands[0] else {
        panic!("vector has no element type");
    };
    // OpTypeInt's operands are its width and signedness.
    assert_eq!(
        defined(element).operands,
        [Word::LiteralBit32(32), Word::LiteralBit32(1)]
    );
    let lanes: Vec<_> = composite
        .operands
        .iter()
        .map(|lane| {
            let Word::IdRef(lane) = lane else {
                panic!("constant lane is not an id");
            };
            defined(*lane).operands[0].clone()
        })
        .collect();
    assert_eq!(lanes, [Word::LiteralBit32(u32::MAX), Word::LiteralBit32(2)]);
}

#[test]
fn omits_a_zero_sampling_offset() {
    use rspirv::dr::Operand as Word;
    use rspirv::spirv::ImageOperands;

    for offset in [
        unsigned_int2(0, 0),
        Operand::Undef(Type::Vector {
            element: Box::new(Type::u32()),
            lanes: 2,
        }),
    ] {
        let (_, operands) = sample_with_offset(offset);
        let [_, _, Word::ImageOperands(mask), Word::IdRef(_)] = operands.as_slice() else {
            panic!("unexpected sample operands: {operands:?}");
        };
        assert_eq!(*mask, ImageOperands::LOD);
    }
}

#[test]
fn source_msl_transforms_by_column_major_matrices() {
    let source = "#include <metal_stdlib>\n\
        #include <simd/simd.h>\n\
        using namespace metal;\n\
        struct Transforms { float4x4 projection; float3x3 normal; };\n\
        struct Out { float4 position [[position, invariant]]; float3 normal [[user(locn0)]]; };\n\
        vertex Out main0(constant Transforms& t [[buffer(0)]], uint id [[vertex_id]]) {\n\
            Out out = {};\n\
            float4x4 scale = float4x4(2.0);\n\
            out.position = (t.projection * scale) * float4(float(id), 0.0, 0.0, 1.0);\n\
            out.normal = float3(1.0, 0.0, 0.0) * transpose(t.normal);\n\
            out.position.y = -out.position.y;\n\
            return out;\n\
        }";
    let library = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap();
    assert_eq!(library.entries.len(), 1);
    assert!(library.rejected.is_empty());
}

#[test]
fn source_msl_inlines_helpers_overloads_and_templates() {
    let source = "#include <metal_stdlib>\n\
        using namespace metal;\n\
        template<typename Tx, typename Ty>\n\
        inline Tx spvSMod(Tx x, Ty y) { Tx remainder = x - y * (x / y); return remainder; }\n\
        static inline __attribute__((always_inline))\n\
        float scaled(float value, float by) { return value * by; }\n\
        static inline __attribute__((always_inline))\n\
        float scaled(float value) { return scaled(value, 2.0); }\n\
        static inline __attribute__((always_inline))\n\
        void bump(thread int& counter) { counter += 1; }\n\
        kernel void main0(device float* output [[buffer(0)]], uint id [[thread_position_in_grid]]) {\n\
            int counter = spvSMod(int(id), 3);\n\
            bump(counter);\n\
            output[id] = scaled(float(counter));\n\
        }";
    let library = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap();
    assert_eq!(library.entries.len(), 1);
}

#[test]
fn source_msl_lays_out_array_templates_and_packed_vectors() {
    let source = "#include <metal_stdlib>\n\
        using namespace metal;\n\
        template<typename T, size_t Num>\n\
        struct spvUnsafeArray\n\
        {\n\
            T elements[Num ? Num : 1];\n\
            thread T& operator [] (size_t pos) thread { return elements[pos]; }\n\
            constexpr const thread T& operator [] (size_t pos) const thread { return elements[pos]; }\n\
        };\n\
        struct Globals { packed_float3 offset; float time; };\n\
        kernel void main0(constant Globals& g [[buffer(0)]], device float4* output [[buffer(1)]]) {\n\
            spvUnsafeArray<float, 4> weights = {};\n\
            weights[1] = g.time;\n\
            spvUnsafeArray<float, 4> copy = weights;\n\
            output[0] = float4(g.offset, copy[1]);\n\
        }";
    let module = parse_msl(source).unwrap();
    assert_eq!(module.entries.len(), 1);
    let library = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap();
    assert!(library.rejected.is_empty());
}

#[test]
fn source_msl_keeps_entry_points_that_translate() {
    let source = "#include <metal_stdlib>\n\
        using namespace metal;\n\
        kernel void good(device uint* output [[buffer(0)]]) { output[0] = 1; }\n\
        kernel void bad(device uint* output [[buffer(0)]]) {\n\
            goto end;\n\
        }";
    let library = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap();
    assert_eq!(library.entries.len(), 1);
    assert_eq!(library.entries[0].name, "good");
    assert_eq!(library.rejected.len(), 1);
    assert_eq!(library.rejected[0].name, "bad");
    // The library as a whole is still refused where every entry is.
    assert!(parse_msl(source).is_err());
}

#[test]
fn source_msl_reads_texture_buffers_as_texel_buffers() {
    let source = "#include <metal_stdlib>\n\
        using namespace metal;\n\
        kernel void main0(texture_buffer<int> faces [[texture(0)]], device int* output [[buffer(0)]],\n\
                          uint id [[thread_position_in_grid]]) {\n\
            output[id] = faces.read(id).x + int(faces.get_width());\n\
        }";
    let library = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap();
    let texture = library.entries[0]
        .resources
        .iter()
        .find(|resource| resource.kind == metal_translate::ir::ResourceKind::Texture)
        .expect("the texture buffer is a resource");
    assert!(texture.texel_buffer);
    assert!(texture.sampled);
}

#[test]
fn source_msl_knows_the_standard_indirect_argument_structures() {
    let source = "#include <metal_stdlib>\n\
        using namespace metal;\n\
        kernel void main0(const device MTLDrawIndexedPrimitivesIndirectArguments* src [[buffer(0)]],\n\
                          device MTLDrawIndexedPrimitivesIndirectArguments* dst [[buffer(1)]]) {\n\
            dst[0] = src[0];\n\
            dst[0].instanceCount *= 2;\n\
            dst[0].baseVertex = -src[0].baseVertex;\n\
        }";
    let library = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap();
    assert!(library.rejected.is_empty());
    assert_eq!(library.entries.len(), 1);
}

#[test]
fn source_msl_rejects_a_pointer_cast_between_typed_buffers() {
    let source = "#include <metal_stdlib>\n\
        using namespace metal;\n\
        kernel void main0(device float* values [[buffer(0)]]) {\n\
            ((device uint*)values)[0] = 1;\n\
        }";
    let error = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap_err();
    assert!(
        error.to_string().contains("only a buffer of bytes allows"),
        "{error}"
    );
}

#[test]
fn source_msl_prefers_its_own_declaration_of_a_standard_structure() {
    let source = "#include <metal_stdlib>\n\
        using namespace metal;\n\
        struct MTLDrawPrimitivesIndirectArguments { uint only; };\n\
        kernel void main0(device MTLDrawPrimitivesIndirectArguments* args [[buffer(0)]]) {\n\
            args[0].only = 1;\n\
        }";
    let library = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap();
    assert!(library.rejected.is_empty());
}
