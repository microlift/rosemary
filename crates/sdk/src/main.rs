//! `sdk <directory>`: write the generated frameworks into an SDK tree.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(root) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: sdk <directory>");
        return ExitCode::FAILURE;
    };
    match write(&root) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sdk: {error}");
            ExitCode::FAILURE
        }
    }
}

fn write(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    for (path, text) in sdk::corefoundation().files()? {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)?;
    }
    Ok(())
}
