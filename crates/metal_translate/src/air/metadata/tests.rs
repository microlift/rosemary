use super::*;

use crate::air::fixtures::{bitcode_block, block, record};

/// A module whose types are `void, i32, metadata, ptr, fn` and whose
/// metadata describes one fragment entry point.
fn module() -> BitcodeBlock {
    bitcode_block(block(
        8,
        vec![
            // MODULE_CODE_VERSION 0, so value references are absolute.
            record(1, &[0]),
            // The entry function occupies module value 0.
            record(8, &[3, 0, 0, 0]),
            block(
                11,
                vec![
                    // Constants: SETTYPE i32, then 0, 1 and 2.
                    record(1, &[1]),
                    record(4, &[0]),
                    record(4, &[2]),
                    record(4, &[4]),
                ],
            ),
            block(
                15,
                vec![
                    record(1, string("air.render_target")),                 // 0
                    record(8, &[2, 0, 1, 1]),                               // 1: output
                    record(1, string("air.fragment_input")),                // 2
                    record(1, string("user(locn3)")),                       // 3
                    record(1, string("air.center")),                        // 4
                    record(1, string("air.perspective")),                   // 5
                    record(1, string("air.arg_name")),                      // 6
                    record(1, string("IN_Color")),                          // 7
                    record(8, &[1, 1, 2, 2, 2, 3, 2, 4, 2, 5, 2, 6, 2, 7]), // 8: input 0
                    record(1, string("air.buffer")),                        // 9
                    record(1, string("air.location_index")),                // 10
                    record(1, string("air.read")),                          // 11
                    record(8, &[1, 2, 2, 9, 2, 10, 1, 2, 1, 2, 2, 11]),     // 12: input 1
                    record(8, &[2, 1]),                                     // 13: output list
                    record(8, &[2, 8, 2, 12]),                              // 14: input list
                    record(8, &[3, 0, 2, 13, 2, 14]),                       // 15: entry
                    record(4, string("air.fragment")),
                    record(10, &[15]),
                ],
            ),
        ],
    ))
}

fn string(text: &str) -> &[u64] {
    // Records store one byte per field; the fixture builder takes u64s.
    Box::leak(
        text.bytes()
            .map(u64::from)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    )
}

fn types() -> Vec<DecodedType> {
    vec![
        DecodedType::Void,
        DecodedType::Integer(32),
        DecodedType::Metadata,
        DecodedType::Pointer {
            pointee: 4,
            address_space: 0,
        },
        DecodedType::Function {
            result: 0,
            parameters: Vec::new(),
        },
    ]
}

#[test]
fn decodes_a_fragment_stage_interface_from_metadata() {
    let module = module();
    let types = types();
    let metadata = Metadata::decode(&module, &types).unwrap();
    let values = ValueTable::module_values(&module, &types).unwrap();
    let interface = metadata
        .stage_interface(Stage::Fragment, &values, &types)
        .unwrap();
    assert_eq!(interface.function, 0);
    assert_eq!(
        interface.outputs,
        vec![StageOutput {
            field: Some(0),
            binding: StageBinding::RenderTarget { index: 0 },
            name: None,
        }]
    );
    assert_eq!(
        interface.inputs,
        vec![StageInput {
            argument: 0,
            binding: StageBinding::Location {
                location: 3,
                interpolation: Some(Interpolation {
                    sampling: Sampling::Center,
                    perspective: true,
                    flat: false,
                }),
            },
            name: Some("IN_Color".to_string()),
            signed: false,
        }]
    );
    let [buffer] = interface.resources.as_slice() else {
        panic!("expected one buffer resource");
    };
    assert_eq!(buffer.argument, 1);
    assert_eq!(buffer.kind, ResourceKind::Buffer);
    assert_eq!(buffer.index, 1);
    assert_eq!(buffer.count, 1);
    assert_eq!(buffer.access, Access::Read);
}

/// Metal spells a texture's complete shape in `air.arg_type_name`; the
/// LLVM argument type is opaque and carries none of it.
#[test]
fn decodes_texture_shapes_from_their_metal_type_name() {
    let shape = |name: &str, access| {
        texture_type(&[Item::Text("air.arg_type_name"), Item::Text(name)], access)
    };
    assert_eq!(
        shape("texture2d<float>", Access::Sample).unwrap(),
        Type::Texture(TextureType {
            dimension: TextureDimension::D2,
            array: false,
            multisampled: false,
            depth: false,
            texel: Box::new(Type::Float { bits: 32 }),
            signed: false,
            access: Access::Sample,
        })
    );
    assert_eq!(
        shape("depthcube<float>", Access::Sample).unwrap(),
        Type::Texture(TextureType {
            dimension: TextureDimension::Cube,
            array: false,
            multisampled: false,
            depth: true,
            texel: Box::new(Type::Float { bits: 32 }),
            signed: false,
            access: Access::Sample,
        })
    );
    assert_eq!(
        shape("texture2d_ms<half>", Access::Read).unwrap(),
        Type::Texture(TextureType {
            dimension: TextureDimension::D2,
            array: false,
            multisampled: true,
            depth: false,
            texel: Box::new(Type::Float { bits: 16 }),
            signed: false,
            access: Access::Read,
        })
    );
    let Type::Texture(array) = shape("texture2d_array<uint>", Access::Write).unwrap() else {
        panic!("expected a texture type");
    };
    assert!(array.array);
    // The texel stays signless like AIR's reads; signedness rides beside it
    // for the Vulkan image type.
    assert_eq!(array.texel.as_ref(), &Type::u32());
    assert!(!array.signed);
    let Type::Texture(signed) = shape("texture2d<int>", Access::Read).unwrap() else {
        panic!("expected a texture type");
    };
    assert_eq!(signed.texel.as_ref(), &Type::u32());
    assert!(signed.signed);
    assert_eq!(signed.sampled_scalar(), Type::i32());
    assert!(shape("texture4d<float>", Access::Sample).is_err());
    assert!(shape("texture2d<double>", Access::Sample).is_err());
}

/// Metadata numbering counts only value-producing records. A NAME record
/// between nodes must not shift the indices its NAMED_NODE refers to.
#[test]
fn named_records_do_not_consume_metadata_indices() {
    let module = bitcode_block(block(
        8,
        vec![
            record(1, &[0]),
            block(
                15,
                vec![
                    record(1, string("first")),
                    record(4, string("unrelated")),
                    record(10, &[0]),
                    record(1, string("second")),
                ],
            ),
        ],
    ));
    let metadata = Metadata::decode(&module, &types()).unwrap();
    assert_eq!(metadata.string(0), Some("first"));
    assert_eq!(metadata.string(1), Some("second"));
    assert_eq!(metadata.named("unrelated"), Some([0].as_slice()));
}

#[test]
fn decodes_modern_node_operands_and_value_wrappers() {
    let module = bitcode_block(block(
        8,
        vec![
            record(1, &[0]),
            record(8, &[3, 0, 0, 0]),
            block(
                15,
                vec![
                    record(1, string("kind")), // metadata 0
                    record(2, &[3, 0]),        // metadata 1: function value 0
                    record(3, &[2, 1, 0]),     // metadata 2: value, string, null
                    record(4, string("entry")),
                    record(10, &[2]),
                ],
            ),
        ],
    ));
    let metadata = Metadata::decode(&module, &types()).unwrap();
    let [value, text, empty] = metadata.node(2).unwrap() else {
        panic!("expected three modern metadata operands");
    };
    assert!(matches!(value, Some(MetadataOperand::Metadata(1))));
    assert!(matches!(text, Some(MetadataOperand::Metadata(0))));
    assert!(empty.is_none());
    assert_eq!(metadata.named("entry"), Some([2].as_slice()));
}

#[test]
fn decodes_packed_metadata_string_table() {
    fn push_vbr6(bits: &mut Vec<bool>, mut value: u64) {
        loop {
            let mut group = (value & 0x1f) as u8;
            value >>= 5;
            if value != 0 {
                group |= 0x20;
            }
            bits.extend((0..6).map(|bit| group & (1 << bit) != 0));
            if value == 0 {
                break;
            }
        }
    }

    let mut bits = Vec::new();
    push_vbr6(&mut bits, 3);
    push_vbr6(&mut bits, 10);
    let mut blob: Vec<u8> = bits.chunks(8).map(|_| 0).collect();
    for (index, set) in bits.into_iter().enumerate() {
        blob[index / 8] |= u8::from(set) << (index % 8);
    }
    let offset = blob.len();
    blob.extend_from_slice(b"AIRair.kernel");
    assert_eq!(
        metadata_strings(&blob, 2, offset as u64).unwrap(),
        ["AIR", "air.kernel"]
    );
}

#[test]
fn a_vertex_attribute_spelled_as_a_signed_integer_is_signed() {
    let signed =
        |name: &str| signed_integer_argument(&[Item::Text("air.arg_type_name"), Item::Text(name)]);
    for name in ["int", "short2", "char4", "packed_int3"] {
        assert!(signed(name), "{name}");
    }
    for name in ["uint", "ushort4", "uchar2", "float4", "half2"] {
        assert!(!signed(name), "{name}");
    }
    assert!(!signed_integer_argument(&[
        Item::Text("air.arg_name"),
        Item::Text("int")
    ]));
}
