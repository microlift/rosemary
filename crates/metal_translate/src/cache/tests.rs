use super::store::{base_directory_from, prune};
use super::*;

use crate::ir::Stage;

/// A directory of this test's own, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "metal-translate-cache-{}-{name}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        flush();
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn library() -> TranslatedLibrary {
    TranslatedLibrary {
        rejected: vec![crate::RejectedEntry {
            name: "kernel_untranslatable".to_owned(),
            reason: "AirSyntax: unsupported statement".to_owned(),
        }],
        function_constants: vec![FunctionConstantInfo {
            index: 3,
            name: "useFog".to_owned(),
            encoding: "b".to_owned(),
        }],
        entries: vec![
            EntryArtifact {
                name: "kernel_store_const".to_owned(),
                stage: Stage::Kernel,
                words: vec![0x0723_0203, 1, 2, 3, u32::MAX],
                resources: vec![
                    ResourceBinding {
                        stage: Stage::Kernel,
                        kind: ResourceKind::Buffer,
                        metal_index: 0,
                        descriptor_set: 0,
                        binding: 0,
                        count: 1,
                        access: Access::ReadWrite,
                        sampled: false,
                        texel_buffer: false,
                        inline_sampler: None,
                    },
                    ResourceBinding {
                        stage: Stage::Fragment,
                        kind: ResourceKind::Texture,
                        metal_index: 3,
                        descriptor_set: 1,
                        binding: 515,
                        count: 1,
                        access: Access::Sample,
                        sampled: true,
                        texel_buffer: false,
                        inline_sampler: None,
                    },
                ],
                required: RequiredFeatures {
                    capabilities: vec!["Shader".to_owned(), "VariablePointers".to_owned()],
                    extensions: vec![],
                },
                vertex_inputs: vec![StageVariable {
                    name: "position".to_owned(),
                    location: 2,
                    data_type: 6,
                }],
                varyings: Vec::new(),
            },
            EntryArtifact {
                name: String::new(),
                stage: Stage::Vertex,
                words: vec![],
                resources: vec![],
                required: RequiredFeatures {
                    capabilities: vec![],
                    extensions: vec!["SPV_KHR_variable_pointers".to_owned()],
                },
                vertex_inputs: Vec::new(),
                varyings: Vec::new(),
            },
        ],
    }
}

/// The narrowest MSL the frontend accepts, so a test can translate.
const SOURCE: &str = "kernel void clear(device uint* output [[buffer(3)]]) { output[0] = 42; }";

#[test]
fn an_entry_round_trips_through_its_file_encoding() {
    let key = [0x5a; 32];
    let encoded = encode(&key, &library());
    assert_eq!(decode(&encoded, &key), Some(library()));
}

#[test]
fn a_corrupt_or_foreign_entry_decodes_to_nothing() {
    let key = [0x5a; 32];
    let encoded = encode(&key, &library());

    assert_eq!(decode(&encoded, &[0x5b; 32]), None, "another key's entry");
    assert_eq!(decode(&encoded[..encoded.len() - 1], &key), None, "cut off");
    assert_eq!(decode(&[], &key), None, "empty");

    let mut trailing = encoded.clone();
    trailing.push(0);
    assert_eq!(decode(&trailing, &key), None, "trailing bytes");

    let mut flipped = encoded.clone();
    *flipped.last_mut().expect("a payload") ^= 0xff;
    assert_eq!(decode(&flipped, &key), None, "a flipped payload bit");

    let mut magic = encoded;
    magic[0] = b'X';
    assert_eq!(decode(&magic, &key), None, "another container");
}

#[test]
fn a_key_covers_the_input_the_target_the_frontend_and_the_passes() {
    let target = TargetProfile::vulkan_1_3();
    let base = key(
        Frontend::AirBitcode,
        b"air",
        &target,
        &PassPipeline::default(),
        &FunctionConstantValues::new(),
    );

    assert_ne!(
        base,
        key(
            Frontend::AirBitcode,
            b"air2",
            &target,
            &PassPipeline::default(),
            &FunctionConstantValues::new(),
        )
    );
    assert_ne!(
        base,
        key(
            Frontend::AirText,
            b"air",
            &target,
            &PassPipeline::default(),
            &FunctionConstantValues::new()
        )
    );
    assert_ne!(
        base,
        key(
            Frontend::Msl,
            b"air",
            &target,
            &PassPipeline::default(),
            &FunctionConstantValues::new()
        )
    );

    let mut wider = TargetProfile::vulkan_1_3();
    wider.variable_pointers = true;
    assert_ne!(
        base,
        key(
            Frontend::AirBitcode,
            b"air",
            &wider,
            &PassPipeline::default(),
            &FunctionConstantValues::new(),
        )
    );

    let mut int64 = TargetProfile::vulkan_1_3();
    int64.shader_int64 = true;
    assert_ne!(
        base,
        key(
            Frontend::AirBitcode,
            b"air",
            &int64,
            &PassPipeline::default(),
            &FunctionConstantValues::new(),
        )
    );

    let mut demote = TargetProfile::vulkan_1_3();
    demote.demote_to_helper_invocation = true;
    assert_ne!(
        base,
        key(
            Frontend::AirBitcode,
            b"air",
            &demote,
            &PassPipeline::default(),
            &FunctionConstantValues::new(),
        )
    );

    let unoptimized = key(
        Frontend::AirBitcode,
        b"air",
        &target,
        &PassPipeline::none(),
        &FunctionConstantValues::new(),
    );
    assert_ne!(base, unoptimized, "the passes a translation runs");
    let mut fog = FunctionConstantValues::new();
    fog.set_index(0, 53, &[1]).expect("a bool");
    assert_ne!(
        base,
        key(
            Frontend::AirBitcode,
            b"air",
            &target,
            &PassPipeline::default(),
            &fog
        ),
        "the function constant values",
    );
    let folded = PassPipeline::new([crate::Pass::FoldConstants]);
    assert_ne!(
        base,
        key(
            Frontend::AirBitcode,
            b"air",
            &target,
            &folded,
            &FunctionConstantValues::new()
        )
    );
    assert_ne!(
        unoptimized,
        key(
            Frontend::AirBitcode,
            b"air",
            &target,
            &folded,
            &FunctionConstantValues::new()
        )
    );

    assert_eq!(
        base,
        key(
            Frontend::AirBitcode,
            b"air",
            &target,
            &PassPipeline::default(),
            &FunctionConstantValues::new(),
        ),
        "stable"
    );
}

#[test]
fn a_second_translation_of_the_same_source_comes_from_the_cache() {
    let scratch = Scratch::new("hit");
    let target = TargetProfile::vulkan_1_3();
    let translate = || crate::translate_msl_with(SOURCE, &target, &PassPipeline::default());

    let first = cached_in(
        &scratch.0,
        Frontend::Msl,
        SOURCE.as_bytes(),
        &target,
        &PassPipeline::default(),
        &FunctionConstantValues::new(),
        translate,
    )
    .expect("the MSL frontend accepts this kernel");

    let mut translated_again = false;
    let second = cached_in(
        &scratch.0,
        Frontend::Msl,
        SOURCE.as_bytes(),
        &target,
        &PassPipeline::default(),
        &FunctionConstantValues::new(),
        || {
            translated_again = true;
            translate()
        },
    )
    .expect("a cached library");

    assert_eq!(first, second);
    assert!(!translated_again, "the second call retranslated");
}

#[test]
fn a_damaged_entry_is_replaced_rather_than_trusted() {
    let scratch = Scratch::new("damaged");
    let target = TargetProfile::vulkan_1_3();
    let translate = || crate::translate_msl_with(SOURCE, &target, &PassPipeline::default());
    let key = key(
        Frontend::Msl,
        SOURCE.as_bytes(),
        &target,
        &PassPipeline::default(),
        &FunctionConstantValues::new(),
    );
    let path = entry_path(&scratch.0, &key);

    let expected = cached_in(
        &scratch.0,
        Frontend::Msl,
        SOURCE.as_bytes(),
        &target,
        &PassPipeline::default(),
        &FunctionConstantValues::new(),
        translate,
    )
    .expect("the MSL frontend accepts this kernel");
    flush();
    std::fs::write(&path, b"not a shader cache entry").expect("damage the entry");

    let recovered = cached_in(
        &scratch.0,
        Frontend::Msl,
        SOURCE.as_bytes(),
        &target,
        &PassPipeline::default(),
        &FunctionConstantValues::new(),
        translate,
    )
    .expect("a retranslated library");
    flush();
    assert_eq!(expected, recovered);
    assert!(path.exists(), "the entry was not written again");
    assert_eq!(
        decode(&std::fs::read(&path).expect("the entry"), &key),
        Some(expected)
    );
}

#[test]
fn a_translation_failure_is_not_cached() {
    let scratch = Scratch::new("failure");
    let target = TargetProfile::vulkan_1_3();
    let source = "this is not Metal shading language";
    let key = key(
        Frontend::Msl,
        source.as_bytes(),
        &target,
        &PassPipeline::default(),
        &FunctionConstantValues::new(),
    );

    assert!(
        cached_in(
            &scratch.0,
            Frontend::Msl,
            source.as_bytes(),
            &target,
            &PassPipeline::default(),
            &FunctionConstantValues::new(),
            || { crate::translate_msl_with(source, &target, &PassPipeline::default()) }
        )
        .is_err()
    );
    assert!(!entry_path(&scratch.0, &key).exists());
}

#[test]
fn flush_finishes_an_asynchronous_write() {
    let scratch = Scratch::new("flush");
    let key = [0x7b; 32];
    let path = entry_path(&scratch.0, &key);
    let expected = library();

    queue_entry(path.clone(), key, &expected);
    flush();

    assert_eq!(read_entry(&path, &key), Some(expected));
    let temporary_files = std::fs::read_dir(path.parent().expect("entry directory"))
        .expect("entry directory")
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(temporary_files, 0);
}

#[test]
fn the_cache_directory_follows_the_environment() {
    let named = base_directory_from(Some("/named".into()), None, None);
    assert_eq!(named, Some(PathBuf::from("/named")));

    for disabled in ["", "0", "off"] {
        assert_eq!(base_directory_from(Some(disabled.into()), None, None), None);
    }

    assert_eq!(
        base_directory_from(None, Some("/xdg".into()), Some("/home/user".into())),
        Some(PathBuf::from("/xdg/rosemary/metal-shaders")),
    );
    assert_eq!(
        base_directory_from(None, Some("".into()), Some("/home/user".into())),
        Some(PathBuf::from("/home/user/.cache/rosemary/metal-shaders")),
    );
    assert_eq!(base_directory_from(None, None, None), None);
}

#[test]
fn pruning_removes_other_builds_and_nothing_else() {
    let scratch = Scratch::new("prune");
    let version = &CACHE_VERSION[..16];
    let other = "0123456789abcdef";
    for name in [version, other, "notes", "0123456789abcdefff"] {
        std::fs::create_dir_all(scratch.0.join(name)).expect("a version directory");
    }
    std::fs::write(scratch.0.join("README"), b"kept").expect("a file");

    prune(&scratch.0, version);

    assert!(scratch.0.join(version).exists(), "this build's entries");
    assert!(!scratch.0.join(other).exists(), "another build's entries");
    assert!(scratch.0.join("notes").exists(), "not a version directory");
    assert!(scratch.0.join("0123456789abcdefff").exists(), "too long");
    assert!(scratch.0.join("README").exists(), "not a directory");
}

/// The version is a digest of this crate's own files, so it must be a
/// full SHA-256 in hexadecimal or the directory name below is wrong.
#[test]
fn the_cache_version_is_a_digest() {
    assert_eq!(CACHE_VERSION.len(), 64);
    assert!(CACHE_VERSION.bytes().all(|byte| byte.is_ascii_hexdigit()));
}
