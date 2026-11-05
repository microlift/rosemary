mod codec;
mod store;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, OnceLock, mpsc};

use crate::ir::{Access, ResourceKind, Stage};
use crate::sha256::{self, Sha256};
use crate::{
    AirInput, EntryArtifact, FunctionConstantInfo, FunctionConstantValues, PassPipeline,
    RequiredFeatures, ResourceBinding, StageVariable, TargetProfile, TranslateError,
    TranslatedLibrary,
};
use codec::{decode, encode};
pub use store::flush;
use store::{entry_path, pending_entry, queue_entry, read_entry, root};

/// The SHA-256 of every file in this crate, as lowercase hexadecimal.
///
/// `build.rs` computes it and re-runs whenever any of those files changes.
pub const CACHE_VERSION: &str = env!("METAL_TRANSLATE_CACHE_VERSION");

/// The container magic, which changes when the entry encoding does.
const MAGIC: &[u8; 8] = b"RMMTLC02";

/// Directory to hold the cache, overriding the user's cache directory.
///
/// Set to `0`, `off` or the empty string to translate every library afresh.
const DIRECTORY_VARIABLE: &str = "ROSEMARY_SHADER_CACHE";

/// Set to keep the entries of every other build of this crate on disk.
const KEEP_VARIABLE: &str = "ROSEMARY_SHADER_CACHE_KEEP";

/// Which frontend produced an entry, so that two frontends given identical
/// bytes cannot collide.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Frontend {
    AirText,
    AirBitcode,
    Msl,
}

impl Frontend {
    fn tag(self) -> u8 {
        match self {
            Self::AirText => 0,
            Self::AirBitcode => 1,
            Self::Msl => 2,
        }
    }
}

/// What the cache did over the life of the process.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Stats {
    pub hits: u64,
    pub misses: u64,
    pub writes: u64,
    /// Reads and writes which failed, or entries which did not decode. Every
    /// one of them is recoverable - the library is translated instead - and a
    /// count that climbs is the only sign a cache directory is unusable.
    pub errors: u64,
}

static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static WRITES: AtomicU64 = AtomicU64::new(0);
static ERRORS: AtomicU64 = AtomicU64::new(0);

/// The directory this build's entries live in, for a runtime which keeps
/// what it derives from them (a driver's compiled pipelines) beside them:
/// pruned with them, and disabled with them.
pub fn directory() -> Option<&'static Path> {
    root()
}

/// What the cache has done so far, for a runtime which reports counters.
pub fn stats() -> Stats {
    Stats {
        hits: HITS.load(Ordering::Relaxed),
        misses: MISSES.load(Ordering::Relaxed),
        writes: WRITES.load(Ordering::Relaxed),
        errors: ERRORS.load(Ordering::Relaxed),
    }
}

/// [`crate::translate`] through the on-disk cache.
///
/// A cache which cannot be read or written is not an error: the library is
/// translated exactly as it would have been without one.
pub fn translate_cached(
    input: AirInput<'_>,
    target: &TargetProfile,
) -> Result<TranslatedLibrary, TranslateError> {
    translate_cached_with(input, target, PassPipeline::from_env())
}

/// [`crate::translate_with`] through the on-disk cache.
pub fn translate_cached_with(
    input: AirInput<'_>,
    target: &TargetProfile,
    passes: &PassPipeline,
) -> Result<TranslatedLibrary, TranslateError> {
    translate_specialized_cached(input, target, passes, &FunctionConstantValues::new())
}

/// [`crate::translate_specialized`] through the on-disk cache.
///
/// The values are part of the key, so each set of them a guest makes a
/// function with is an entry of its own.
pub fn translate_specialized_cached(
    input: AirInput<'_>,
    target: &TargetProfile,
    passes: &PassPipeline,
    constants: &FunctionConstantValues,
) -> Result<TranslatedLibrary, TranslateError> {
    let (frontend, bytes) = match input {
        AirInput::Text(text) => (Frontend::AirText, text.as_bytes()),
        AirInput::Bitcode(bytes) => (Frontend::AirBitcode, bytes),
    };
    cached(frontend, bytes, target, passes, constants, || {
        crate::translate_specialized(input, target, passes, constants)
    })
}

/// [`crate::translate_msl`] through the on-disk cache.
pub fn translate_msl_cached(
    source: &str,
    target: &TargetProfile,
) -> Result<TranslatedLibrary, TranslateError> {
    translate_msl_cached_with(source, target, PassPipeline::from_env())
}

/// [`crate::translate_msl_with`] through the on-disk cache.
pub fn translate_msl_cached_with(
    source: &str,
    target: &TargetProfile,
    passes: &PassPipeline,
) -> Result<TranslatedLibrary, TranslateError> {
    translate_msl_cached_with_macros(source, &[], target, passes)
}

/// [`crate::translate_msl_with_macros`] through the on-disk cache. The
/// macros are part of the key: they change what the source says.
pub fn translate_msl_cached_with_macros(
    source: &str,
    macros: &[(String, String)],
    target: &TargetProfile,
    passes: &PassPipeline,
) -> Result<TranslatedLibrary, TranslateError> {
    let mut bytes = Vec::new();
    for (name, value) in macros {
        for text in [name, value] {
            bytes.extend_from_slice(&(text.len() as u64).to_le_bytes());
            bytes.extend_from_slice(text.as_bytes());
        }
    }
    // An empty list keys exactly as a source without macros always has.
    if !bytes.is_empty() {
        bytes.extend_from_slice(b"\0macros\0");
    }
    bytes.extend_from_slice(source.as_bytes());
    cached(
        Frontend::Msl,
        &bytes,
        target,
        passes,
        &FunctionConstantValues::new(),
        || crate::translate_msl_with_macros(source, macros, target, passes),
    )
}

fn cached(
    frontend: Frontend,
    bytes: &[u8],
    target: &TargetProfile,
    passes: &PassPipeline,
    constants: &FunctionConstantValues,
    translate: impl FnOnce() -> Result<TranslatedLibrary, TranslateError>,
) -> Result<TranslatedLibrary, TranslateError> {
    match root() {
        Some(root) => cached_in(root, frontend, bytes, target, passes, constants, translate),
        None => translate(),
    }
}

/// As [`cached`], against a named directory rather than the process-wide one.
///
/// `translate` must run exactly the `passes` the key is made from.
fn cached_in(
    root: &Path,
    frontend: Frontend,
    bytes: &[u8],
    target: &TargetProfile,
    passes: &PassPipeline,
    constants: &FunctionConstantValues,
    translate: impl FnOnce() -> Result<TranslatedLibrary, TranslateError>,
) -> Result<TranslatedLibrary, TranslateError> {
    let key = key(frontend, bytes, target, passes, constants);
    let path = entry_path(root, &key);

    if let Some(library) = read_entry(&path, &key) {
        HITS.fetch_add(1, Ordering::Relaxed);
        return Ok(library);
    }
    if let Some(library) = pending_entry(&path, &key) {
        HITS.fetch_add(1, Ordering::Relaxed);
        return Ok(library);
    }
    MISSES.fetch_add(1, Ordering::Relaxed);

    let library = translate()?;
    queue_entry(path, key, &library);
    Ok(library)
}

/// The key naming this input, this target, the IR passes the translation
/// runs, and this build of the translator.
///
/// The build covers what each pass does; the pipeline covers which of them a
/// process chose to run, which `ROSEMARY_METAL_PASSES` changes without a
/// rebuild. An entry made without a pass is not what a run with it emits.
fn key(
    frontend: Frontend,
    bytes: &[u8],
    target: &TargetProfile,
    passes: &PassPipeline,
    constants: &FunctionConstantValues,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update_field(MAGIC);
    hasher.update_field(CACHE_VERSION.as_bytes());
    hasher.update(&[frontend.tag()]);
    hasher.update_field(&encode_target(target));
    hasher.update_field(&passes.fingerprint());
    hasher.update_field(&constants.fingerprint());
    hasher.update_field(bytes);
    hasher.finish()
}

/// The target profile as the key sees it.
///
/// Every field is included, because emission reads every field: a module
/// emitted for an adapter without `variablePointers` is not the module the
/// same source emits for one with it.
fn encode_target(target: &TargetProfile) -> Vec<u8> {
    let TargetProfile {
        vulkan_version,
        spirv_version,
        max_bound_descriptor_sets,
        shader,
        shader_int8,
        shader_int16,
        shader_int64,
        shader_float16,
        storage_buffer8_bit_access,
        uniform_and_storage_buffer8_bit_access,
        storage_push_constant8,
        storage_buffer16_bit_access,
        uniform_and_storage_buffer16_bit_access,
        storage_push_constant16,
        storage_input_output16,
        shader_image_gather_extended,
        shader_storage_image_multisample,
        image_cube_array,
        shader_storage_image_read_without_format,
        shader_storage_image_write_without_format,
        shader_clip_distance,
        shader_draw_parameters,
        sample_rate_shading,
        geometry_shader,
        multi_viewport,
        demote_to_helper_invocation,
        group_non_uniform,
        image_1d,
        image_query,
        variable_pointers_storage_buffer,
        variable_pointers,
    } = target;

    let mut bytes = Vec::with_capacity(16);
    bytes.extend_from_slice(&[vulkan_version.0, vulkan_version.1]);
    bytes.extend_from_slice(&[spirv_version.0, spirv_version.1]);
    bytes.extend_from_slice(&max_bound_descriptor_sets.to_le_bytes());
    for flag in [
        shader,
        shader_int8,
        shader_int16,
        shader_int64,
        shader_float16,
        storage_buffer8_bit_access,
        uniform_and_storage_buffer8_bit_access,
        storage_push_constant8,
        storage_buffer16_bit_access,
        uniform_and_storage_buffer16_bit_access,
        storage_push_constant16,
        storage_input_output16,
        shader_image_gather_extended,
        shader_storage_image_multisample,
        image_cube_array,
        shader_storage_image_read_without_format,
        shader_storage_image_write_without_format,
        shader_clip_distance,
        shader_draw_parameters,
        sample_rate_shading,
        geometry_shader,
        multi_viewport,
        demote_to_helper_invocation,
        group_non_uniform,
        image_1d,
        image_query,
        variable_pointers_storage_buffer,
        variable_pointers,
    ] {
        bytes.push(u8::from(*flag));
    }
    bytes
}

// The tags below are the file format rather than the IR, so each is written
// out: a variant added to the IR must be given a tag here, and reordering the
// IR must not silently reinterpret entries already on disk.
