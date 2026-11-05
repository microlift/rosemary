//! The on-disk cache of guest code compiled ahead of time. See `docs/aot.md`.
//!
//! One immutable file per guest image, named by the image's SHA-256 under
//! `<base>/<build-id>/<machine>/<config>/`. A file is only ever replaced whole, by a
//! rename, so a reader never needs a lock; the `.lock` sidecar only keeps two
//! processes from compiling the same image at once.

pub mod build;
pub mod entries;
pub mod file;
pub mod profile;
pub mod resolve;
pub mod spawn;
pub mod store;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use microlift::core::fingerprint::StableHasher;

/// Where the cache lives; `0`, `off` or empty disables it.
pub const CACHE_ENV: &str = "ROSEMARY_AOT_CACHE";
/// Keep other builds' caches rather than pruning them, for bisecting between builds.
pub const KEEP_ENV: &str = "ROSEMARY_AOT_CACHE_KEEP";

/// Bumped when what Rosemary stores beside Microlift's image changes meaning.
const FORMAT_VERSION: u32 = 3;

/// Extension of a cached image.
const IMAGE_EXTENSION: &str = "rmc";

/// The directory beside the build directories that holds profile logs, which name
/// guest offsets and so outlive any one build.
const PROFILE_DIRECTORY: &str = "profile";

/// This build and both container formats; see `build.rs`.
pub fn build_id() -> String {
    format!(
        "{}-{}-{FORMAT_VERSION}",
        env!("ROSEMARY_BUILD_ID"),
        microlift::image::FORMAT_VERSION
    )
}

/// The host's own ISA, before any setting this run chose.
pub fn machine() -> &'static str {
    static MACHINE: LazyLock<String> =
        LazyLock::new(|| match microlift::backend::CraneliftBackend::new() {
            Ok(backend) => format!(
                "{:016x}",
                microlift::backend::isa_fingerprint(backend.isa())
            ),
            Err(_) => "unknown".into(),
        });
    &MACHINE
}

/// Every setting the code depends on: the lifter's, the ISA this run compiles for,
/// and each JIT and region knob.
pub fn config() -> &'static str {
    static CONFIG: LazyLock<String> = LazyLock::new(|| {
        let mut hasher = StableHasher::new();
        let lifter = crate::program::lifter_config_base().codegen_fingerprint();
        hasher.str(&format!("{lifter:016x}"));
        if let Ok(backend) = crate::jit::backend() {
            let isa = microlift::backend::isa_fingerprint(backend.isa());
            hasher.str(&format!("{isa:016x}"));
        }
        hasher.str(&crate::jit::knobs());
        hasher.str(&crate::region::knobs());
        format!("{:016x}", std::hash::Hasher::finish(&hasher))
    });
    &CONFIG
}

/// A guest image's identity: the SHA-256 of its file.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ImageId(pub [u8; 32]);

impl ImageId {
    pub fn of(bytes: &[u8]) -> Self {
        use sha2::Digest;
        ImageId(sha2::Sha256::digest(bytes).into())
    }
}

impl std::fmt::Display for ImageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

/// Where this run's cache files go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    base: PathBuf,
}

impl Layout {
    pub fn new(base: PathBuf) -> Self {
        Layout { base }
    }

    /// The process-wide layout, or `None` when the cache is disabled or there is
    /// nowhere to put it.
    pub fn current() -> Option<&'static Layout> {
        static LAYOUT: LazyLock<Option<Layout>> = LazyLock::new(|| {
            base_directory_from(
                std::env::var_os(CACHE_ENV),
                std::env::var_os("XDG_CACHE_HOME"),
                std::env::var_os("HOME"),
            )
            .map(Layout::new)
        });
        LAYOUT.as_ref()
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Cached images for this build, machine and configuration.
    pub fn directory(&self) -> PathBuf {
        self.base.join(build_id()).join(machine()).join(config())
    }

    pub fn image(&self, id: &ImageId) -> PathBuf {
        self.directory().join(format!("{id}.{IMAGE_EXTENSION}"))
    }

    pub fn profile(&self, id: &ImageId) -> PathBuf {
        self.base.join(PROFILE_DIRECTORY).join(format!("{id}.log"))
    }

    /// The images runs of the executable `id` `dlopen`ed.
    pub fn dlopens(&self, id: &ImageId) -> PathBuf {
        self.base
            .join(PROFILE_DIRECTORY)
            .join(format!("{id}.dlopen"))
    }

    /// Removes every other build's cache, unless [`KEEP_ENV`] is set.
    pub fn prune(&self) {
        if std::env::var_os(KEEP_ENV).is_none() {
            store::prune(&self.base, &[&build_id(), PROFILE_DIRECTORY]);
        }
    }
}

/// Why a cache file was not used.
#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    File(file::DecodeError),
    /// Built for another ISA or configuration than this run's.
    Stale,
    Image(microlift::MicroliftError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "{e}"),
            LoadError::File(e) => write!(f, "{e:?}"),
            LoadError::Stale => write!(f, "built for another machine or configuration"),
            LoadError::Image(e) => write!(f, "{e}"),
        }
    }
}

/// The cached regions of image `id`, which this run placed at `slide`, keeping only
/// those `config` can still use; `Ok(None)` when nothing is cached.
pub fn load(
    layout: &Layout,
    id: &ImageId,
    config: &microlift::core::LifterConfig,
    slide: u64,
) -> Result<Option<Vec<microlift::runtime::image::LoadedRegion>>, LoadError> {
    let Some(mapped) = store::map(&layout.image(id)).map_err(LoadError::Io)? else {
        return Ok(None);
    };
    let file = file::CacheFile::decode(&mapped).map_err(LoadError::File)?;
    let backend = crate::jit::backend().map_err(LoadError::Image)?;
    let isa = microlift::backend::isa_fingerprint(backend.isa());
    let fingerprint = config.codegen_fingerprint();
    if file
        .chunks
        .iter()
        .any(|chunk| chunk.isa_fingerprint != isa || chunk.config_fingerprint != fingerprint)
    {
        return Err(LoadError::Stale);
    }
    let moved = slide.wrapping_sub(file.lift_slide);
    let mut resolver = resolve::Resolver::new(config, moved);
    resolver.learn(
        config,
        file.chunks
            .iter()
            .flat_map(|chunk| &chunk.regions)
            .flat_map(|region| &region.import_decisions),
    );
    let mut regions = Vec::new();
    for chunk in &file.chunks {
        let loaded =
            microlift::runtime::image::load_image(chunk, &resolver).map_err(LoadError::Image)?;
        regions.extend(
            loaded
                .into_iter()
                .filter(|region| usable(config, region, moved)),
        );
    }
    Ok(Some(regions))
}

/// Whether a loaded region may run in this process.
///
/// Guest addresses in code are relocated, but a value the lift read out of a
/// read-only segment and folded in is not: a rebased pointer there differs with
/// the slide. So a region that read anything outside its own bytes is used only
/// at the slide it was lifted at.
fn usable(
    config: &microlift::core::LifterConfig,
    region: &microlift::runtime::image::LoadedRegion,
    moved: u64,
) -> bool {
    (moved == 0 || region.translation.data_dependencies.is_empty())
        && resolve::decisions_hold(config, &region.import_decisions)
}

/// Whether the code `memory` holds for `report` is the code in `bytes`, the file
/// it was read from: every executable segment byte for byte.
///
/// A file id says what is on disk, and a loader may substitute other bytes under
/// a path; code cached under the id must be code from the file.
pub fn matches_file<M: machish::memory::AddressSpace + ?Sized>(
    memory: &M,
    report: &machish::report::ImageReport,
    bytes: &[u8],
) -> bool {
    let Ok(thin) = machish::fat::select(bytes, &machish::spec::arch::X86_64) else {
        return false;
    };
    report
        .segments
        .iter()
        .filter(|segment| segment.executable)
        .all(|segment| {
            let Some(range) = segment.file_range.clone() else {
                return true;
            };
            let Some(expected) = thin.get(range) else {
                return false;
            };
            let mut mapped = vec![0u8; expected.len()];
            memory.read(segment.vmaddr, &mut mapped).is_ok() && mapped == expected
        })
}

/// Where the cache lives, given the three variables which can name it.
pub fn base_directory_from(
    override_directory: Option<OsString>,
    xdg_cache_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(directory) = override_directory {
        let disabled = matches!(directory.to_str(), Some("" | "0" | "off"));
        return (!disabled).then(|| PathBuf::from(directory));
    }
    if let Some(cache) = xdg_cache_home.filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(cache).join("rosemary/aot"));
    }
    let home = home.filter(|value| !value.is_empty())?;
    Some(PathBuf::from(home).join(".cache/rosemary/aot"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_follows_xdg_then_home() {
        let home = Some(OsString::from("/home/u"));
        assert_eq!(
            base_directory_from(None, Some("/c".into()), home.clone()),
            Some(PathBuf::from("/c/rosemary/aot"))
        );
        assert_eq!(
            base_directory_from(None, None, home.clone()),
            Some(PathBuf::from("/home/u/.cache/rosemary/aot"))
        );
        assert_eq!(
            base_directory_from(Some("/x".into()), Some("/c".into()), home.clone()),
            Some(PathBuf::from("/x"))
        );
        assert_eq!(base_directory_from(Some("off".into()), None, home), None);
    }

    #[test]
    fn an_image_file_is_keyed_by_build_machine_config_and_hash() {
        let layout = Layout::new(PathBuf::from("/c"));
        let id = ImageId::of(b"image");
        let path = layout.image(&id);
        let parts: Vec<_> = path.strip_prefix("/c").unwrap().iter().collect();
        assert_eq!(parts.len(), 4, "{path:?}");
        assert_eq!(parts[0], build_id().as_str());
        assert_eq!(parts[1], machine());
        assert_eq!(parts[2], config());
        assert_eq!(parts[3].to_str().unwrap(), format!("{id}.rmc"));
    }

    #[test]
    fn an_image_id_is_its_sha256() {
        assert_eq!(
            ImageId::of(b"abc").to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
