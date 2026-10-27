//! Translate every Metal source file in a directory - what
//! `ROSEMARY_MSL_OUT` writes - and report the distinct reasons any failed.
//!
//! `cargo run --release -p metal_translate --example inspect_sources -- DIR [SPIRV_DIR]`

use std::collections::BTreeMap;

use metal_translate::{PassPipeline, TargetProfile, translate_msl_with};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args_os().nth(1).ok_or("expected a directory")?;
    // A second directory receives every emitted module, for `spirv-val`.
    let output = std::env::args_os().nth(2).map(std::path::PathBuf::from);
    let target = TargetProfile {
        shader_int8: true,
        shader_int16: true,
        shader_float16: true,
        storage_buffer8_bit_access: true,
        storage_buffer16_bit_access: true,
        shader_storage_image_write_without_format: true,
        variable_pointers_storage_buffer: true,
        variable_pointers: true,
        demote_to_helper_invocation: true,
        shader_draw_parameters: true,
        ..TargetProfile::vulkan_1_3()
    };
    let passes = PassPipeline::from_env();
    let mut failures: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total = 0;
    let mut paths = std::fs::read_dir(directory)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "metal")
        })
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        total += 1;
        let source = std::fs::read_to_string(&path)?;
        let translated = translate_msl_with(&source, &target, passes);
        if let (Ok(library), Some(output)) = (&translated, &output) {
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            for entry in &library.entries {
                let bytes = entry
                    .words
                    .iter()
                    .flat_map(|word| word.to_le_bytes())
                    .collect::<Vec<_>>();
                std::fs::write(output.join(format!("{stem}-{}.spv", entry.name)), bytes)?;
            }
        }
        if let Err(error) = translated {
            let first = &error.diagnostics[0];
            let key = format!("{:?}: {}", first.phase, first.message);
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            failures
                .entry(key)
                .or_default()
                .push(format!("{name}:{}", first.line.unwrap_or(0)));
        }
    }
    let failed: usize = failures.values().map(Vec::len).sum();
    let mut sorted = failures.into_iter().collect::<Vec<_>>();
    sorted.sort_by_key(|(_, files)| std::cmp::Reverse(files.len()));
    for (reason, files) in &sorted {
        println!("{:5}  {reason}", files.len());
        println!("       e.g. {}", files[..files.len().min(3)].join(", "));
    }
    println!("{} of {total} translated", total - failed);
    Ok(())
}
