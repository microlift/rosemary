//! The on-disk cache of guest code compiled ahead of time. See `docs/aot.md`.
//!
//! Immutable files per guest image, named by the image's SHA-256 under
//! `<base>/<build-id>/<machine>/<config>/`: part 0 holds what start-up needs, later
//! parts everything else. Each is a Microlift image, mapped and run in place, whose
//! regions are published one at a time as the run first reaches them. A file is only
//! ever replaced whole, by a rename, so a reader never needs a lock; the `.lock`
//! sidecar only keeps two processes from compiling the same image at once.

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

use microlift::core::LifterConfig;
use microlift::core::fingerprint::StableHasher;
use microlift::runtime::image::{LoadedRegion, MappedImage};

use crate::cache::Tier;

/// Where the cache lives; `0`, `off` or empty disables it.
pub const CACHE_ENV: &str = "ROSEMARY_AOT_CACHE";
/// Keep other builds' caches rather than pruning them, for bisecting between builds.
pub const KEEP_ENV: &str = "ROSEMARY_AOT_CACHE_KEEP";

/// Bumped when what Rosemary stores beside Microlift's image changes meaning.
const FORMAT_VERSION: u32 = 4;

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
        microlift::image::file::FORMAT_VERSION
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
        if let Ok(isa) = crate::jit::region_isa(Tier::Final) {
            let isa = microlift::backend::isa_fingerprint(isa);
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

    /// Part 0 of image `id`'s cache: what start-up needs, and the file whose
    /// presence says the image has a cache.
    pub fn image(&self, id: &ImageId) -> PathBuf {
        self.part(id, 0)
    }

    /// Part `n` of image `id`'s cache.
    pub fn part(&self, id: &ImageId, n: u32) -> PathBuf {
        self.directory().join(format!("{id}.{n}.{IMAGE_EXTENSION}"))
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

/// One cache file of an image, mapped for this run. Its regions are published one at
/// a time, as the run first reaches each.
pub struct Cached {
    image: MappedImage,
    /// The image mapping this was bound for: its file and run-time slide.
    path: String,
    slide: u64,
    pub tier: Tier,
    /// Part 0: what earlier runs executed and what start-up needs.
    pub hot: bool,
    /// Run-time minus lift-time address of the image's code.
    moved: u64,
    /// One bit per region, set once it has been looked at: a region is published at
    /// most once, and never again after a write to its code retired it.
    claimed: Vec<std::sync::atomic::AtomicU64>,
}

impl Cached {
    /// Whether this was bound for the mapping of `path` at `slide`.
    pub fn maps(&self, path: &str, slide: u64) -> bool {
        self.slide == slide && self.path == path
    }

    /// The region that can be entered at `pc`, unless it was looked at before or its
    /// code is not mapped yet.
    pub fn claim(&self, pc: u64) -> Option<u32> {
        use std::sync::atomic::Ordering;
        if !self.image.code_mapped() {
            return None;
        }
        let index = self.image.lookup(pc)?;
        let bit = 1u64 << (index % 64);
        let word = &self.claimed[index as usize / 64];
        (word.fetch_or(bit, Ordering::AcqRel) & bit == 0).then_some(index)
    }

    /// Region `index`, or `None` where `config` may not run it or its code is not
    /// mapped yet.
    pub fn region(&self, config: &LifterConfig, index: u32) -> Option<LoadedRegion> {
        let region = self.image.region(index).ok()??;
        usable(config, &region, self.moved).then_some(region)
    }

    /// Maps the code; see [`MappedImage::map_code`] for why that is apart.
    pub fn map_code(&self) -> Result<(), LoadError> {
        self.image.map_code().map(drop).map_err(LoadError::Image)
    }

    pub fn code_mapped(&self) -> bool {
        self.image.code_mapped()
    }

    /// Whether the code runs from the file's own pages.
    pub fn in_place(&self) -> bool {
        self.image.in_place()
    }

    pub fn region_count(&self) -> u32 {
        self.image.region_count()
    }
}

/// What part 0 of image `id`'s cache says about itself, or `None` without a readable one.
pub fn embedded(layout: &Layout, id: &ImageId) -> Option<file::Embedded> {
    let mapped = store::map(&layout.image(id)).ok()??;
    let view = microlift::image::file::View::parse(&mapped).ok()?;
    file::Embedded::decode(view.user()).ok()
}

/// Every cache file of image `id`, which this run mapped from `path` at `slide`, mapped for
/// reading and bound for `config`; empty when nothing is cached. No code is mapped
/// yet: see [`Cached::map_code`].
pub fn open(
    layout: &Layout,
    id: &ImageId,
    config: &LifterConfig,
    path: &str,
    slide: u64,
) -> Result<Vec<Cached>, LoadError> {
    let mut parts = Vec::new();
    for n in 0.. {
        let file = match std::fs::File::open(layout.part(id, n)) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(LoadError::Io(e)),
        };
        parts.push(open_part(&file, n == 0, config, path, slide)?);
    }
    Ok(parts)
}

fn open_part(
    file: &std::fs::File,
    hot: bool,
    config: &LifterConfig,
    path: &str,
    slide: u64,
) -> Result<Cached, LoadError> {
    let mut image = MappedImage::open(file).map_err(LoadError::Image)?;
    let embedded = file::Embedded::decode(image.user()).map_err(LoadError::File)?;
    let isa = crate::jit::region_isa(embedded.tier).map_err(LoadError::Image)?;
    let header = image.header();
    if header.isa_fingerprint != microlift::backend::isa_fingerprint(isa)
        || header.config_fingerprint != config.codegen_fingerprint()
    {
        return Err(LoadError::Stale);
    }
    let moved = slide.wrapping_sub(embedded.lift_slide);
    let mut resolver = resolve::Resolver::new(config, moved);
    resolver.learn(config, image.sites());
    image.bind(&resolver).map_err(LoadError::Image)?;
    let words = (image.region_count() as usize).div_ceil(64);
    Ok(Cached {
        image,
        path: path.to_owned(),
        slide,
        tier: embedded.tier,
        hot,
        moved,
        claimed: (0..words).map(|_| Default::default()).collect(),
    })
}

/// Whether a published region may run in this process.
///
/// Guest addresses in code are relocated, but a value the lift read out of a
/// read-only segment and folded in is not: a rebased pointer there differs with
/// the slide. So a region that read anything outside its own bytes is used only
/// at the slide it was lifted at.
fn usable(config: &LifterConfig, region: &LoadedRegion, moved: u64) -> bool {
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
        assert_eq!(parts[3].to_str().unwrap(), format!("{id}.0.rmc"));
    }

    #[test]
    fn an_image_id_is_its_sha256() {
        assert_eq!(
            ImageId::of(b"abc").to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
