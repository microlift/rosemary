use metal_translate::{
    FunctionConstantValues, PassPipeline, TargetProfile,
    air::{AirInput, parse_air},
    emit_spirv, metallib_air, specialize,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os().nth(1).ok_or("expected an input file")?;
    let bytes = std::fs::read(path)?;
    let limit = std::env::args()
        .nth(2)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(10usize);
    // Report what a device with 64-bit integer support would accept; that is
    // the common desktop Vulkan profile.
    let target = TargetProfile {
        shader_int8: true,
        shader_int64: true,
        storage_buffer8_bit_access: true,
        shader_storage_image_write_without_format: true,
        variable_pointers_storage_buffer: true,
        variable_pointers: true,
        demote_to_helper_invocation: true,
        ..TargetProfile::vulkan_1_3()
    };
    for (ordinal, offset) in bytes
        .windows(4)
        .enumerate()
        .filter_map(|(i, w)| (w == b"MTLB").then_some(i))
        .take(limit)
        .enumerate()
    {
        let result = metallib_air(&bytes[offset..])
            .inspect(|air| {
                // The AIR itself, for `llvm-dis`, when a rejection needs to be
                // read against the bitcode which produced it. Off unless asked
                // for: this is the shader's own source in another form.
                if let Some(directory) = std::env::var_os("ROSEMARY_AIR_OUT") {
                    let path = std::path::Path::new(&directory).join(format!("{ordinal}.bc"));
                    let _ = std::fs::write(path, air);
                }
            })
            .and_then(|air| parse_air(AirInput::Bitcode(air)));
        // The same IR passes the runtime runs, as `ROSEMARY_METAL_PASSES`
        // selects them, so that a rejection here is one a game would see.
        let result = result.and_then(|mut module| {
            specialize(&mut module, &FunctionConstantValues::new())?;
            PassPipeline::from_env().run(&mut module)?;
            Ok(module)
        });
        match result {
            Ok(module) => {
                // Keep these names with an emission failure: they are enough
                // to correlate a rejected library with an engine log, but do
                // not expose the shader implementation.
                let entries = module
                    .entries
                    .iter()
                    .map(|entry| module.functions[entry.function].name.as_str())
                    .collect::<Vec<_>>();
                let result = emit_spirv(&module, &target);
                match result {
                    Ok(library) => {
                        // Writing the modules out lets an external validator check
                        // them; this example never claims they run on a GPU.
                        if let Some(directory) = std::env::var_os("ROSEMARY_SPIRV_OUT") {
                            for (index, entry) in library.entries.iter().enumerate() {
                                let path = std::path::Path::new(&directory)
                                    .join(format!("{ordinal}-{index}.spv"));
                                let bytes: Vec<u8> = entry
                                    .words
                                    .iter()
                                    .flat_map(|word| word.to_le_bytes())
                                    .collect();
                                std::fs::write(path, bytes)?;
                            }
                        }
                        println!(
                            "{ordinal} at {offset}: emitted {:?}",
                            library
                                .entries
                                .iter()
                                .map(|entry| (entry.stage, entry.words.len()))
                                .collect::<Vec<_>>()
                        )
                    }
                    Err(error) => println!("{ordinal} at {offset} ({entries:?}): {error}"),
                }
            }
            Err(error) => println!("{ordinal} at {offset}: {error}"),
        }
    }
    Ok(())
}
