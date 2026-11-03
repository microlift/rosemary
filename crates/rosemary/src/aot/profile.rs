//! Which image code a run had to JIT: FEX's code map.
//!
//! One log per image, `profile/<image-sha256>.log`, of little-endian `u64` unslid
//! addresses. Every record is one `O_APPEND` write of 8 bytes, so concurrent
//! processes interleave whole records and never need a lock.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use machish::memory::Regions;

use super::{ImageId, Layout};
use crate::program::Program;

const RECORD: usize = std::mem::size_of::<u64>();

/// Logs `pc`, which `get_or_lift` found no translation for, if it is image code.
///
/// Generated code is never logged: no later run has the same bytes at the same place.
pub fn note_miss<M: Regions>(program: &Program<M>, pc: u64) {
    let Some(layout) = Layout::current() else {
        return;
    };
    let Some((path, slide)) = program.code_image(pc) else {
        return;
    };
    let Some(id) = image_id(&path) else {
        return;
    };
    let mut logs = LOGS.lock().unwrap_or_else(|e| e.into_inner());
    let log = logs.entry(id).or_insert_with(|| open(&layout.profile(&id)));
    if let Some(file) = log {
        // Best effort: a lost record costs a later run one JIT compile.
        let _ = append(file, pc.wrapping_sub(slide));
    }
}

/// Open logs by image, `None` for one that could not be opened (asked once).
static LOGS: LazyLock<Mutex<HashMap<ImageId, Option<File>>>> = LazyLock::new(Mutex::default);

/// Image ids by file, hashed once per process.
fn image_id(path: &str) -> Option<ImageId> {
    static IDS: LazyLock<Mutex<HashMap<String, Option<ImageId>>>> = LazyLock::new(Mutex::default);
    let mut ids = IDS.lock().unwrap_or_else(|e| e.into_inner());
    *ids.entry(path.to_string())
        .or_insert_with(|| std::fs::read(path).ok().map(|bytes| ImageId::of(&bytes)))
}

fn open(path: &Path) -> Option<File> {
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// One record, in one `write`: an append of fewer bytes would let another process's
/// record land in the middle of it.
pub fn append(file: &mut File, address: u64) -> std::io::Result<()> {
    let written = file.write(&address.to_le_bytes())?;
    if written != RECORD {
        return Err(std::io::Error::other("short append to a profile log"));
    }
    Ok(())
}

/// Every address a log holds, in the order logged, ignoring a torn final record.
pub fn read(path: &Path) -> std::io::Result<Vec<u64>> {
    let bytes = std::fs::read(path)?;
    Ok(bytes
        .chunks_exact(RECORD)
        .map(|record| u64::from_le_bytes(record.try_into().expect("exact chunk")))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appended_addresses_read_back_in_order() {
        let dir = std::env::temp_dir().join(format!("rosemary-aot-profile-{}", std::process::id()));
        let path = dir.join("p.log");
        let mut first = open(&path).unwrap();
        let mut second = open(&path).unwrap();
        append(&mut first, 0x1000).unwrap();
        append(&mut second, 0x2000).unwrap();
        append(&mut first, 0x3000).unwrap();
        assert_eq!(read(&path).unwrap(), [0x1000, 0x2000, 0x3000]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_torn_record_is_ignored() {
        let dir = std::env::temp_dir().join(format!("rosemary-aot-torn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.log");
        let mut bytes = 0x1234u64.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[1, 2, 3]);
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(read(&path).unwrap(), [0x1234]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
