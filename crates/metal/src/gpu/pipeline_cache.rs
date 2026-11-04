//! A device's `VkPipelineCache`, kept on disk between runs beside the
//! translated shaders it compiles.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, Once};

use ash::{Device, vk};
use sha2::{Digest, Sha256};

/// The file magic, which changes when the container does.
const MAGIC: &[u8; 8] = b"RMVKPC01";
const LENGTH_SIZE: usize = size_of::<u64>();
const DIGEST_SIZE: usize = 32;
const HEADER_SIZE: usize = MAGIC.len() + LENGTH_SIZE + DIGEST_SIZE;

/// Pipelines made between saves, so a guest which is killed rather than
/// exiting keeps most of what it compiled.
const SAVE_INTERVAL: u64 = 64;

pub(crate) struct PipelineCache {
    pub handle: vk::PipelineCache,
    /// `None` where the shader cache is disabled.
    path: Option<PathBuf>,
    unsaved: AtomicU64,
    saving: AtomicBool,
    /// Held across a save: the background save and the one at exit share a
    /// temporary file.
    writing: Mutex<()>,
}

impl PipelineCache {
    pub(super) fn open(device: &Device, properties: &vk::PhysicalDeviceProperties) -> Self {
        let path = metal_translate::cache::directory().map(|directory| {
            directory.join(format!(
                "pipelines-{:04x}-{:04x}-{}.bin",
                properties.vendor_id,
                properties.device_id,
                properties
                    .pipeline_cache_uuid
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ))
        });
        let file = path.as_deref().and_then(|path| std::fs::read(path).ok());
        let data = file.as_deref().and_then(decode).unwrap_or_default();
        let create = |data: &[u8]| {
            let info = vk::PipelineCacheCreateInfo::default().initial_data(data);
            // SAFETY: a well-formed create info on this live device. The
            // driver checks the data's own header and ignores what does not
            // match it.
            unsafe { device.create_pipeline_cache(&info, None) }
        };
        let handle = create(data)
            .or_else(|_| create(&[]))
            .unwrap_or_else(|error| {
                super::trace(format!("vkCreatePipelineCache failed with {error:?}"));
                vk::PipelineCache::null()
            });
        if path.is_some() && handle != vk::PipelineCache::null() {
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                extern "C" fn save_at_exit() {
                    crate::vulkan::save_pipeline_caches();
                }
                // SAFETY: the callback has C's required signature and touches
                // only process-lifetime state.
                if unsafe { libc::atexit(save_at_exit) } != 0 {
                    super::trace("pipeline cache: atexit registration failed");
                }
            });
        }
        Self {
            handle,
            path,
            unsaved: AtomicU64::new(0),
            saving: AtomicBool::new(false),
            writing: Mutex::new(()),
        }
    }

    /// Note a pipeline made through this cache, saving in the background
    /// every [`SAVE_INTERVAL`].
    pub(super) fn created(&'static self, device: &'static Device) {
        if self.unsaved.fetch_add(1, Ordering::Relaxed) + 1 < SAVE_INTERVAL
            || self.saving.swap(true, Ordering::Acquire)
        {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("metal:pipeline-cache".to_owned())
            .spawn(move || {
                self.save(device);
                self.saving.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            self.saving.store(false, Ordering::Release);
        }
    }

    pub(super) fn save(&self, device: &Device) {
        let Some(path) = &self.path else {
            return;
        };
        let _writing = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        if self.unsaved.swap(0, Ordering::Relaxed) == 0 {
            return;
        }
        // SAFETY: a cache of this device's; pipeline caches are internally
        // synchronized, so a compile on another thread may run alongside.
        let data = match unsafe { device.get_pipeline_cache_data(self.handle) } {
            Ok(data) => data,
            Err(error) => {
                return super::trace(format!("vkGetPipelineCacheData failed with {error:?}"));
            }
        };
        if let Err(error) = write(path, &encode(&data)) {
            super::trace(format!("pipeline cache: writing {path:?} failed: {error}"));
        }
    }
}

#[cfg(test)]
impl PipelineCache {
    pub(super) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// How many bytes the driver reports for a new cache made from `initial`,
    /// or from the saved file where it is `None`.
    pub(super) fn reopened_bytes(&self, device: &Device, initial: Option<&[u8]>) -> Option<usize> {
        let file = match initial {
            Some(_) => Vec::new(),
            None => std::fs::read(self.path.as_ref()?).ok()?,
        };
        let data = match initial {
            Some(initial) => initial,
            None => decode(&file)?,
        };
        let info = vk::PipelineCacheCreateInfo::default().initial_data(data);
        // SAFETY: a well-formed create info on this live device, and a cache
        // nothing else has seen.
        unsafe {
            let cache = device.create_pipeline_cache(&info, None).ok()?;
            let held = device.get_pipeline_cache_data(cache);
            device.destroy_pipeline_cache(cache, None);
            held.ok().map(|held| held.len())
        }
    }
}

/// Replace the file whole, so a reader never sees a partial one.
fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

/// The driver's data behind a header which says whether it arrived intact:
/// drivers check that the data is theirs, not that it is whole.
fn encode(data: &[u8]) -> Vec<u8> {
    let mut file = Vec::with_capacity(HEADER_SIZE + data.len());
    file.extend_from_slice(MAGIC);
    file.extend_from_slice(&(data.len() as u64).to_le_bytes());
    file.extend_from_slice(&Sha256::digest(data));
    file.extend_from_slice(data);
    file
}

fn decode(file: &[u8]) -> Option<&[u8]> {
    let (magic, rest) = file.split_at_checked(MAGIC.len())?;
    let (length, rest) = rest.split_at_checked(LENGTH_SIZE)?;
    let (digest, data) = rest.split_at_checked(DIGEST_SIZE)?;
    let length = u64::from_le_bytes(length.try_into().ok()?);
    (magic == MAGIC && data.len() as u64 == length && Sha256::digest(data)[..] == *digest)
        .then_some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_what_it_encodes() {
        let data = b"driver pipeline data";
        assert_eq!(decode(&encode(data)), Some(&data[..]));
        assert_eq!(decode(&encode(&[])), Some(&[][..]));
    }

    #[test]
    fn rejects_a_truncated_or_altered_file() {
        let file = encode(b"driver pipeline data");
        assert_eq!(decode(&file[..file.len() - 1]), None);
        assert_eq!(decode(&file[..HEADER_SIZE - 1]), None);
        let mut altered = file.clone();
        *altered.last_mut().unwrap() ^= 1;
        assert_eq!(decode(&altered), None);
        let mut renamed = file;
        renamed[0] ^= 1;
        assert_eq!(decode(&renamed), None);
    }
}
