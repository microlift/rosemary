use std::collections::BTreeMap;
use std::ffi::CString;
use std::ops::Range;
use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::cxxlib::Flavor;
use machish::exports::ExportTable;
use machish::image::MachObject;
use machish::install_name::PathContext;
use machish::loader::Loader;
use machish::memory::{MmapSpace, Regions};
use machish::report::LinkReport;
use machish::resolve::SyntheticDylib;
use machish::spec::MachSpec;
use machish::spec::arch::X86_64;

use crate::program::{AddrMap, HostProviders, ImportSite};

/// An image this runtime loaded on the guest's behalf.
pub struct Image {
    /// The path actually opened, after `@`-prefix expansion.
    pub path: String,
    /// What `dlopen` returned. The image's base address, which is unique in this
    /// process by construction and is the one value a crash dump can be read against.
    pub handle: u64,
    pub report: LinkReport,
    /// The mapping, kept alive for the process. Leaked separately from the `Image` so
    /// that [`regions`](Image::regions) can borrow from it.
    _memory: &'static SharedSpace,
    /// The mapped extents, for [`contains`] and [`lift_region`].
    regions: Vec<(u64, &'static [u8])>,
    /// Every address a call site in this image might name - both `slot_va` and
    /// `stub_va` - exactly as [`crate::Program`] keys its own.
    imports: BTreeMap<u64, ImportSite>,
    /// The same sites by the address they resolved to; see
    /// [`Program::import_by_target`](crate::Program::import_by_target), which also
    /// explains why this one is hashed and [`imports`](Image::imports) is not.
    imports_by_target: AddrMap<ImportSite>,
    /// Slid extents from `LC_FUNCTION_STARTS`, the hint [`lift_region`] uses.
    functions: Vec<Range<u64>>,
    exports: ExportTable,
    slide: u64,
}

/// An [`MmapSpace`] shared across the threads running the guest.
struct SharedSpace(MmapSpace);

// SAFETY: see the type note.
unsafe impl Sync for SharedSpace {}
unsafe impl Send for SharedSpace {}

/// Every image `dlopen` has produced, in load order.
static IMAGES: Mutex<Vec<&'static Image>> = Mutex::new(Vec::new());

/// The published, immutable view of [`IMAGES`]: a leaked slice, swapped whole.
static VIEW: AtomicPtr<&'static [&'static Image]> = AtomicPtr::new(core::ptr::null_mut());

/// The guest executable's path and slide.
static EXECUTABLE: Mutex<Option<(String, u64)>> = Mutex::new(None);

/// One image this runtime mapped, as much of it as [`guest_image_at`] needs.
struct MappedImage {
    /// The mapped `mach_header`, which is what dyld calls an image's base.
    header: u64,
    slide: u64,
    /// Slid `[lo, hi)` of each segment.
    segments: Vec<(u64, u64)>,
}

/// The images the native backend mapped.
///
/// The emulated backend's are reachable through [`crate::thunk::current`], which
/// is a `Vm` and so is absent under `native`. Nothing else answers for them:
/// dyld never saw these mappings, so its `_dyld_find_unwind_sections` reports no
/// unwind info for a guest frame and a throw terminates instead of unwinding.
/// Written once, before the program runs, and read once per frame of every
/// unwind - so a lock rather than a `OnceLock` would put the unwinder's frame
/// walk behind a mutex.
static MAPPED: OnceLock<Vec<MappedImage>> = OnceLock::new();

/// One image the *program's own* link loaded, as something a later `dlopen` can
/// bind against.
struct ProgramImage {
    /// The install name or path the loader knew it by - `@rpath/QtCore.framework/
    /// Versions/5/QtCore` for a framework inside a bundle.
    name: String,
    slide: u64,
    /// Parsed from the file on first use, and `None` if it cannot be: the image
    /// is already mapped, so re-reading it is pure cost and only a `dlopen` ever
    /// pays it.
    exports: OnceLock<Option<ExportTable>>,
}

/// The images the program was linked against, for a later `dlopen` to resolve
/// against.
///
/// Set once, before the program runs. See [`ProgramImages`].
static PROGRAM_IMAGES: OnceLock<Vec<ProgramImage>> = OnceLock::new();

/// Everything the *executable* was linked against, as a symbol provider.
///
/// The gap this closes: [`MainExports`] answers for the executable and
/// [`LoadedImages`] for images the guest `dlopen`ed, and **neither** answers for
/// the dylibs the executable itself pulled in. So a plugin loaded at runtime
/// could not bind to the framework it is a plugin *for* - Qt's Cocoa plugin had
/// fourteen hundred unresolved imports, every one of them a symbol QtCore and
/// QtGui were already exporting from memory a few pages away.
///
/// Matched by leaf name like [`LoadedImages`], and for the same reason: one side
/// spells it `@rpath/QtCore.framework/Versions/5/QtCore` and the other holds
/// whatever path the file was opened from.
struct ProgramImages;

impl ProgramImages {
    /// The recorded images whose leaf name matches a two-level reference, or all
    /// of them for a flat one.
    fn matching<'a>(named: Option<&'a str>) -> impl Iterator<Item = &'static ProgramImage> + 'a {
        PROGRAM_IMAGES
            .get()
            .map(|v| v.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(move |image| named.is_none_or(|n| n == leaf(&image.name)))
    }

    /// `image`'s export table, parsed from disk on first use.
    fn exports(image: &'static ProgramImage) -> Option<&'static ExportTable> {
        image
            .exports
            .get_or_init(|| {
                let path = resolve_path(&image.name)?;
                let bytes = std::fs::read(&path).ok()?;
                match crate::exec::active()?.engine() {
                    crate::exec::Engine::Native => ExportTable::parse(
                        &MachObject::parse_any(&bytes, crate::exec::native::HOST_SPEC).ok()?,
                    )
                    .ok(),
                    crate::exec::Engine::Emulated => {
                        ExportTable::parse(&MachObject::parse_any(&bytes, X86_64).ok()?).ok()
                    }
                }
            })
            .as_ref()
    }
}

impl machish::resolve::SymbolProvider for ProgramImages {
    fn provides(&self, install_name: &str) -> bool {
        Self::matching(Some(leaf(install_name))).next().is_some()
    }

    fn resolve(&self, sym: &machish::fixups::SymbolRef<'_>) -> Option<machish::resolve::Resolved> {
        let named = sym.library.install_name().map(leaf);
        for image in Self::matching(named) {
            let Some(export) = Self::exports(image).and_then(|t| t.get(sym.name)) else {
                continue;
            };
            // A re-export has no address of its own; see [`MainExports`].
            let Some(addr) = export.address(image.slide) else {
                continue;
            };
            return Some(machish::resolve::Resolved {
                addr,
                kind: match export.kind {
                    machish::exports::ExportKind::ThreadLocal => {
                        machish::resolve::SymbolKind::ThreadLocal
                    }
                    _ => machish::resolve::SymbolKind::Function,
                },
                tag: 0,
            });
        }
        None
    }
}

/// Publish `report`'s images for the questions dyld cannot answer about them.
pub fn register_mapped_images(report: &LinkReport) {
    let images: Vec<MappedImage> = report
        .images
        .iter()
        .map(|i| MappedImage {
            header: i.base,
            slide: i.slide,
            segments: i
                .segments
                .iter()
                .map(|s| (s.vmaddr, s.vmaddr.wrapping_add(s.vmsize)))
                .collect(),
        })
        .collect();
    let _ = MAPPED.set(images);
    // The same images as something a later `dlopen` can bind against. The root
    // is left out: it is [`MainExports`]' job, which knows the path it was
    // launched from and answers `@executable_path` as well.
    let root = report.root().map(|r| r.name.as_str());
    let _ = PROGRAM_IMAGES.set(
        report
            .images
            .iter()
            .filter(|i| Some(i.name.as_str()) != root)
            .map(|i| ProgramImage {
                name: i.name.clone(),
                slide: i.slide,
                exports: OnceLock::new(),
            })
            .collect(),
    );
}

/// The main executable's export table, parsed once, on first use.
static MAIN_EXPORTS: Mutex<Option<&'static MainExports>> = Mutex::new(None);

/// The main executable, as something a `dlopen`ed image can bind against.
struct MainExports {
    exports: ExportTable,
    slide: u64,
    /// The path the executable was loaded from, and its last component, which
    /// are the two spellings a two-level reference could use.
    path: String,
}

impl machish::resolve::SymbolProvider for MainExports {
    fn provides(&self, install_name: &str) -> bool {
        install_name == self.path
            || Some(install_name) == self.path.rsplit('/').next()
            || install_name == "@executable_path"
    }

    fn resolve(&self, sym: &machish::fixups::SymbolRef<'_>) -> Option<machish::resolve::Resolved> {
        let export = self.exports.get(sym.name)?;
        // A re-export forwards into another library and has no address of its
        // own; answering zero would bind the caller to nothing and report
        // success.
        let addr = export.address(self.slide)?;
        Some(machish::resolve::Resolved {
            addr,
            kind: match export.kind {
                machish::exports::ExportKind::ThreadLocal => {
                    machish::resolve::SymbolKind::ThreadLocal
                }
                _ => machish::resolve::SymbolKind::Function,
            },
            tag: 0,
        })
    }
}

/// Every image the guest has already loaded, as something a newly loaded one can bind
/// against.
///
/// **Loading it again would be the wrong repair**, and is what a `DylibSource` that
/// guessed harder would do:
///
/// ## Matching
///
/// By **leaf name**, in both directions.
struct LoadedImages;

/// The file name of an install name or path, which is what both sides agree on.
fn leaf(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

impl machish::resolve::SymbolProvider for LoadedImages {
    fn provides(&self, install_name: &str) -> bool {
        let wanted = leaf(install_name);
        find(|i| (leaf(&i.path) == wanted).then_some(())).is_some()
    }

    fn resolve(&self, sym: &machish::fixups::SymbolRef<'_>) -> Option<machish::resolve::Resolved> {
        // A two-level reference is a claim about *which* image defines the
        // symbol, and honouring it is the whole reason `machish` carries the
        // library through to here.
        let named = sym.library.install_name().map(leaf);
        find(|image| {
            if named.is_some_and(|n| n != leaf(&image.path)) {
                return None;
            }
            let export = image.exports.get(sym.name)?;
            // A re-export has no address of its own; see [`MainExports`].
            let addr = export.address(image.slide)?;
            Some(machish::resolve::Resolved {
                addr,
                kind: match export.kind {
                    machish::exports::ExportKind::ThreadLocal => {
                        machish::resolve::SymbolKind::ThreadLocal
                    }
                    _ => machish::resolve::SymbolKind::Function,
                },
                tag: 0,
            })
        })
    }
}

/// The main executable's exports, parsed on first `dlopen` and kept.
///
/// Lazy rather than built at startup because a program that never calls
/// `dlopen` should not pay to re-read and parse its own executable.
fn main_exports() -> Option<&'static MainExports> {
    let mut slot = MAIN_EXPORTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = *slot {
        return Some(existing);
    }
    let (path, slide) = EXECUTABLE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()?;
    let bytes = std::fs::read(&path).ok()?;
    let exports = match crate::exec::active()?.engine() {
        crate::exec::Engine::Native => {
            ExportTable::parse(&MachObject::parse_any(&bytes, crate::exec::native::HOST_SPEC).ok()?)
                .ok()?
        }
        crate::exec::Engine::Emulated => {
            ExportTable::parse(&MachObject::parse_any(&bytes, X86_64).ok()?).ok()?
        }
    };
    let leaked: &'static MainExports = Box::leak(Box::new(MainExports {
        exports,
        slide,
        path,
    }));
    *slot = Some(leaked);
    Some(leaked)
}

/// The message `dlerror` will report, and the buffer it last handed out.
static ERROR: Mutex<Option<CString>> = Mutex::new(None);
static REPORTED: Mutex<Option<CString>> = Mutex::new(None);

/// A token for "the main executable and everything already loaded".
static GLOBAL_TOKEN: u8 = 0;

fn global_handle() -> u64 {
    &GLOBAL_TOKEN as *const u8 as usize as u64
}

/// Libraries this runtime *is*, which a guest may `dlopen` by name.
///
/// A framework reimplemented in this tree has no file on disk, so
/// [`resolve_path`] - which ends in `fs::canonicalize` - can never find it, and
/// `dlopen("/System/Library/Frameworks/AppKit.framework/AppKit")` would fall
/// through to the host's loader and come back null. The *link* half of this
/// already works: a flat `SyntheticDylib` ahead of the native provider in
/// `HostProviders::chain` shadows every two-level binding. This is the other
/// half.
///
/// Registered by install name and by the framework paths that name it, because a
/// guest asks with whichever spelling it was given.
mod synthetic {
    use std::sync::Mutex;

    /// One virtual library: what it answers to, and what it answers with.
    pub struct Library {
        pub names: Vec<String>,
        pub symbol: fn(&str) -> Option<u64>,
        /// The address a guest is handed, which is this entry's own.
        pub handle: u64,
    }

    static LIBRARIES: Mutex<Vec<&'static Library>> = Mutex::new(Vec::new());

    /// Publish a library under `names`, answering through `symbol`.
    ///
    /// The handle is the leaked entry's own address, which is distinct from
    /// every image base and from [`super::global_handle`] by construction: it is
    /// this process's heap, and a guest image is mapped elsewhere.
    pub fn register(names: &[&str], symbol: fn(&str) -> Option<u64>) {
        let mut libraries = LIBRARIES.lock().unwrap_or_else(|e| e.into_inner());
        if libraries
            .iter()
            .any(|l| l.names.iter().any(|n| n == names[0]))
        {
            return;
        }
        let leaked: &'static Library = Box::leak(Box::new(Library {
            names: names.iter().map(|n| n.to_string()).collect(),
            symbol,
            handle: 0,
        }));
        // The handle is the entry's address, filled in now that it has one.
        let handle = leaked as *const Library as usize as u64;
        // SAFETY: the entry was just leaked and nothing else has seen it, so
        // this is the only write and it happens before publication.
        unsafe { (*(leaked as *const Library as *mut Library)).handle = handle };
        libraries.push(leaked);
    }

    /// The library `path` names, by install name or by any spelling of it.
    ///
    /// Matched on the *leaf* as well as the whole path, because a guest asks for
    /// `AppKit.framework/Versions/C/AppKit` and for `AppKit` and means the same
    /// library.
    pub fn find(path: &str) -> Option<&'static Library> {
        let leaf = path.rsplit('/').next().unwrap_or(path);
        LIBRARIES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|l| l.names.iter().any(|n| n == path || n == leaf))
            .copied()
    }

    /// The library a handle names, or `None` for a handle that is not one.
    pub fn by_handle(handle: u64) -> Option<&'static Library> {
        LIBRARIES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|l| l.handle == handle)
            .copied()
    }
}

pub use synthetic::register as register_synthetic;

/// Return the guest image at a dyld image-list index.
fn indexed_image(index: u32) -> Option<(u64, u64, String)> {
    if index == 0 {
        let (path, slide) = EXECUTABLE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        let header = MAIN_HEADER.load(Ordering::Acquire);
        return (header != 0).then_some((header, slide, path));
    }
    snapshot()
        .get(index as usize - 1)
        .map(|image| (image.handle, image.slide, image.path.clone()))
}

fn image_count() -> u32 {
    u32::from(MAIN_HEADER.load(Ordering::Acquire) != 0)
        .saturating_add(IMAGE_COUNT.load(Ordering::Acquire) as u32)
}

/// The main executable's `mach_header` as the guest can address it.
static MAIN_HEADER: AtomicU64 = AtomicU64::new(0);

/// Record the main executable's mach header address.
pub fn set_executable_header(va: u64) {
    MAIN_HEADER.store(va, Ordering::Release);
}

/// Record where the guest executable came from, and where it landed.
pub fn set_executable_path(path: &str, slide: u64) {
    let canonical = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string());
    *EXECUTABLE.lock().unwrap_or_else(|e| e.into_inner()) = Some((canonical, slide));
}

/// The path presented to guest process-introspection calls.
pub fn executable_path() -> Option<String> {
    EXECUTABLE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|(path, _)| path.clone())
}

/// How many images are loaded, and the address range they span.
///
/// A program that never calls `dlopen` now pays one relaxed atomic load, and a program
/// that has pays that plus a range check before any lock is taken.
static IMAGE_COUNT: AtomicUsize = AtomicUsize::new(0);
static REGION_LO: AtomicU64 = AtomicU64::new(u64::MAX);
static REGION_HI: AtomicU64 = AtomicU64::new(0);

static IMPORT_FILTER: AtomicPtr<&'static [u64]> = AtomicPtr::new(core::ptr::null_mut());

/// The same, over [`Image::imports_by_target`]'s keys.
static TARGET_FILTER: AtomicPtr<&'static [u64]> = AtomicPtr::new(core::ptr::null_mut());

/// Whether `filter` has heard of `va`. A yes may be a collision; a no is final.
/// Null - nothing published yet - answers yes, which costs a probe, not an answer.
fn heard_of(filter: &AtomicPtr<&'static [u64]>, va: u64) -> bool {
    let published = filter.load(Ordering::Acquire);
    if published.is_null() {
        return true;
    }
    // SAFETY: as `find` - leaked by `publish_filters`, never freed, never mutated.
    crate::program::filter_has(unsafe { *published }, va)
}

/// Rebuild both filters from `images`. Called under the [`IMAGES`] lock.
fn publish_filters(images: &[&'static Image]) {
    let publish = |filter: &AtomicPtr<&'static [u64]>, built: Box<[u64]>| {
        let leaked: &'static [u64] = Box::leak(built);
        filter.store(Box::leak(Box::new(leaked)) as *mut _, Ordering::Release);
    };
    let imports = images.iter().map(|i| i.imports.len()).sum();
    publish(
        &IMPORT_FILTER,
        crate::program::build_import_filter(
            images.iter().flat_map(|i| i.imports.keys().copied()),
            imports,
        ),
    );
    let targets = images.iter().map(|i| i.imports_by_target.len()).sum();
    publish(
        &TARGET_FILTER,
        crate::program::build_import_filter(
            images
                .iter()
                .flat_map(|i| i.imports_by_target.keys().copied()),
            targets,
        ),
    );
}

/// Search the loaded images without copying the list.
///
/// Every `Image` is `&'static` - leaked at load, never freed - so a reference found
/// inside the lock outlives it, and the guard is dropped before the caller does
/// anything with the answer.
fn snapshot() -> Vec<&'static Image> {
    let view = VIEW.load(Ordering::Acquire);
    if view.is_null() {
        return Vec::new();
    }
    // SAFETY: as in `find`.
    unsafe { *view }.to_vec()
}

fn find<R>(mut f: impl FnMut(&'static Image) -> Option<R>) -> Option<R> {
    let view = VIEW.load(Ordering::Acquire);
    if view.is_null() {
        return None;
    }
    // SAFETY: non-null only after a `dlopen` stored a leaked slice, which is
    // never freed and never mutated. See the note on `VIEW`.
    let images: &'static [&'static Image] = unsafe { *view };
    images.iter().copied().find_map(&mut f)
}

/// The import registered at `va` in any dynamically loaded image.
pub fn import_at(va: u64) -> Option<&'static ImportSite> {
    // A slot or stub of a loaded image is *inside* that image, so an address
    // outside every dlopened mapping cannot be one.
    if va < REGION_LO.load(Ordering::Relaxed) || va >= REGION_HI.load(Ordering::Relaxed) {
        return None;
    }
    if !heard_of(&IMPORT_FILTER, va) {
        return None;
    }
    find(|i| i.imports.get(&va))
}

/// The import of any dynamically loaded image that *resolved to* `host`.
pub fn import_by_target(host: u64) -> Option<&'static ImportSite> {
    // The same two-compare rejection [`import_at`] makes, for a colder version
    // of the same problem.
    if IMAGE_COUNT.load(Ordering::Relaxed) == 0 || !heard_of(&TARGET_FILTER, host) {
        return native_symbol_at(host);
    }
    find(|i| i.imports_by_target.get(&host)).or_else(|| native_symbol_at(host))
}

/// What `symbol` resolved to in any dynamically loaded image.
pub fn resolved_symbol(symbol: &str) -> Option<u64> {
    find(|i| {
        i.imports
            .values()
            .find(|s| !s.is_stub && s.symbol == symbol && s.resolved != 0)
            .map(|s| s.resolved)
    })
}

/// Whether `va` lies inside a dynamically loaded image.
pub fn contains(va: u64) -> bool {
    // A host address is nowhere near a guest mapping, and this is the question
    // asked most often and answered "no" almost every time.
    if va < REGION_LO.load(Ordering::Relaxed) || va >= REGION_HI.load(Ordering::Relaxed) {
        return false;
    }
    find(|i| region_of(i, va).map(|_| ())).is_some()
}

/// The mapped region holding `va`, for an image the *guest* loaded.
///
/// The loader of the *executable* owns only the executable's regions, so a pass
/// that walks a `dlopen`ed image's metadata - selector uniquing and reference
/// rewriting both - cannot find its addresses there. This is where they are.
pub fn region_containing(va: u64) -> Option<(u64, &'static [u8])> {
    if va < REGION_LO.load(Ordering::Relaxed) || va >= REGION_HI.load(Ordering::Relaxed) {
        return None;
    }
    find(|image| region_of(image, va))
}

/// The recorded function starts inside `range`, for an image the *guest* loaded.
pub fn function_starts_in(range: std::ops::Range<u64>) -> Vec<u64> {
    // An address is in one image, so the first image with anything in `range` is
    // the answer - and `starts_within` finds it by binary search rather than by
    // scanning that image's function table. See
    // [`crate::program::Program::function_starts_in`].
    find(|image| {
        let starts: Vec<u64> = crate::program::starts_within(&image.functions, &range).collect();
        (!starts.is_empty()).then_some(starts)
    })
    .unwrap_or_default()
}

/// The region to lift in order to execute `va`, if it is in a loaded image.
pub fn lift_region(va: u64) -> Option<(crate::region::Region, Vec<u8>)> {
    find(|image| {
        let (base, bytes) = region_of(image, va)?;
        // A loaded image, so not memory the guest writes; see [`crate::region`].
        let region = crate::region::region_for(bytes, base, va, false)?;
        let start = usize::try_from(region.range.start - base).ok()?;
        let end = usize::try_from(region.range.end - base)
            .ok()?
            .min(bytes.len());
        Some((region, bytes.get(start..end)?.to_vec()))
    })
}

fn region_of(image: &'static Image, va: u64) -> Option<(u64, &'static [u8])> {
    image
        .regions
        .iter()
        .find(|(base, bytes)| va >= *base && va < base + bytes.len() as u64)
        .copied()
}

/// `va` as `image+offset`, for an image the guest loaded. See
/// [`Program::locate`](crate::Program::locate).
pub fn locate(va: u64) -> Option<String> {
    find(|image| {
        for report in &image.report.images {
            for seg in &report.segments {
                if va >= seg.vmaddr && va < seg.vmaddr.wrapping_add(seg.vmsize) {
                    let name = report.name.rsplit('/').next().unwrap_or(&report.name);
                    return Some(format!("{name}+{:#x}", va.wrapping_sub(report.slide)));
                }
            }
        }
        None
    })
}

/// How many images the guest has loaded.
pub fn loaded() -> usize {
    IMAGE_COUNT.load(Ordering::Acquire)
}

/// Load `path` as a guest image, or return the handle it already has.
pub fn open(path: &str) -> std::result::Result<u64, String> {
    // A library this runtime *is* has no file, so this has to come before
    // anything that looks for one. See [`synthetic`].
    if let Some(library) = synthetic::find(path) {
        return Ok(library.handle);
    }
    let resolved = resolve_path(path).ok_or_else(|| format!("no such file: {path}"))?;

    if let Some(handle) = find(|i| (i.path == resolved).then_some(i.handle)) {
        return Ok(handle);
    }

    let bytes = std::fs::read(&resolved).map_err(|e| format!("{resolved}: {e}"))?;
    match crate::exec::active().map(|exec| exec.engine()) {
        Some(crate::exec::Engine::Native) => open_with(
            &resolved,
            &bytes,
            crate::exec::native::HOST_SPEC,
            crate::cxxlib::HOST,
        ),
        Some(crate::exec::Engine::Emulated) => open_with(&resolved, &bytes, X86_64, Flavor::Intel),
        None => Err("no execution backend is active".to_string()),
    }
}

/// Load one image using the architecture and executable-memory policy of the
/// backend that will call it.
fn open_with<S: MachSpec + 'static>(
    resolved: &str,
    bytes: &[u8],
    spec: S,
    flavor: Flavor,
) -> std::result::Result<u64, String> {
    let object = MachObject::parse_any(bytes, spec)
        .map_err(|e| format!("{resolved}: not a loadable Mach-O for this backend: {e}"))?;

    let mut memory = MmapSpace::new();
    // Vendored libc++ ahead of the filesystem; see [`crate::cxxlib`].
    let mut source = crate::cxxlib::source(flavor);
    let hosts = HostProviders::new();
    // Everything already loaded, which is what dyld resolves a new image
    // against.
    let main = main_exports();
    let loaded = LoadedImages;
    let program = ProgramImages;
    let mut extra: Vec<&dyn machish::resolve::SymbolProvider> = main
        .map(|m| m as &dyn machish::resolve::SymbolProvider)
        .into_iter()
        .collect();
    // The program's own dependencies before the images the guest loaded: a
    // plugin binds to the framework it extends, and that framework came in with
    // the executable.
    extra.push(&program as &dyn machish::resolve::SymbolProvider);
    extra.push(&loaded as &dyn machish::resolve::SymbolProvider);
    let mut report = Loader::new(spec)
        .link_with(
            bytes,
            resolved,
            &mut memory,
            &mut source,
            &hosts.chain(&extra),
        )
        .map_err(|e| format!("{resolved}: {e}"))?;

    // The same repair the program's own link needs, for the same reason.
    crate::cxxlib::bind_unwinder_to_the_vendored_one(&mut report, &mut memory, flavor)
        .map_err(|e| format!("{resolved}: unwinder: {e}"))?;
    crate::cxxlib::bind_cxxabi_to_the_vendored_one(&mut report, &mut memory, flavor)
        .map_err(|e| format!("{resolved}: cxxabi: {e}"))?;
    crate::cxxlib::bind_objc_exceptions_to_the_vendored_cxxabi(&mut report, &mut memory, flavor)
        .map_err(|e| format!("{resolved}: objc exceptions: {e}"))?;
    // The program's allocator replacement reaches a plugin as it reaches every
    // image the program was linked with.
    crate::interpose::apply_to_loaded(&mut report, &mut memory)
        .map_err(|e| format!("{resolved}: interpose: {e}"))?;

    // Say what did not resolve, here, rather than letting the guest find out by
    // calling it. The program's own link says it the same way, and this is the
    // same announcement - see [`crate::program::announce_unbound_imports`].
    crate::program::announce_unbound_imports(&report, &format!("dlopen({resolved})"));
    // And where it landed, so a fault inside it - above all a call through one
    // of the stubs just announced - is named rather than "in no guest image".
    crate::fault::note_images(&report);

    let root = report
        .root()
        .ok_or_else(|| format!("{resolved}: linked to no images"))?;
    let (base, slide) = (root.base, root.slide);

    let functions = match object.section("__TEXT", "__text") {
        Some(text) => machish::image::function_ranges(&object, text.addr, text.addr + text.size)
            .map_err(|e| format!("{resolved}: {e}"))?
            .into_iter()
            .map(|r| r.start.wrapping_add(slide)..r.end.wrapping_add(slide))
            .collect(),
        // No `__text` is not a refusal here the way it is for a program.
        None => Vec::new(),
    };

    let exports = ExportTable::parse(&object).map_err(|e| format!("{resolved}: {e}"))?;
    // Everything mapped: this image, the ones loaded before it, and the program
    // that started it all.
    let vm = crate::thunk::current();
    let is_guest = |va: u64| {
        memory.region_at(va).is_some()
            || contains(va)
            || vm.is_some_and(|vm| vm.program.is_guest_address(va))
    };
    let (imports, imports_by_target) = collect_imports(&report, is_guest);

    // The same substitution the program's own slots get - see
    // [`crate::trampoline`] - and a `dlopen`ed plugin is where it pays most.
    let eligible: Vec<(u64, ImportSite)> = imports
        .iter()
        .filter(|(_, site)| !site.is_stub && !is_guest(site.resolved))
        .map(|(&va, site)| (va, site.clone()))
        .collect();
    crate::trampoline::install(
        eligible.iter().map(|(va, site)| (*va, site)),
        &|_| false,
        &mut memory,
    );

    // The space is leaked first so its regions really are `'static`; the image
    // then holds both the borrow and the thing borrowed from, which is legal
    // only because the space can no longer move.
    let memory: &'static SharedSpace = Box::leak(Box::new(SharedSpace(memory)));
    let leaked: &'static Image = Box::leak(Box::new(Image {
        path: resolved.to_string(),
        handle: base,
        report,
        regions: memory.0.regions(),
        imports,
        imports_by_target,
        functions,
        exports,
        slide,
        _memory: memory,
    }));

    // Published *before* the initializers run: an initializer is guest code in
    // this image, and lifting it needs the image to be findable.
    {
        let mut images = IMAGES.lock().unwrap_or_else(|e| e.into_inner());
        images.push(leaked);
        for &(base, bytes) in &leaked.regions {
            REGION_LO.fetch_min(base, Ordering::Relaxed);
            REGION_HI.fetch_max(base + bytes.len() as u64, Ordering::Relaxed);
        }
        // The extents and the filters go up before the view does, so a reader
        // that sees the image also sees a range and a filter that contain it.
        publish_filters(&images);
        let published: &'static [&'static Image] = images.clone().leak();
        let cell: &'static mut &'static [&'static Image] = Box::leak(Box::new(published));
        VIEW.store(cell as *mut _, Ordering::Release);
        IMAGE_COUNT.store(images.len(), Ordering::Release);
        // A whole image's worth of addresses that meant nothing a moment ago,
        // imports among them. See [`crate::dispatch::invalidate`].
        crate::dispatch::invalidate();
    }

    // A loaded image's thread-locals are its own, and nothing else registers
    // them.
    crate::tls::register(&leaked.report);

    // Before any of this image's code runs, and after it is findable. Asked of
    // the *backend* rather than of a `Vm`, because a native guest has no `Vm` and
    // would otherwise get an image whose selector references were never uniqued.
    let exec = crate::exec::active();
    if let Some(exec) = exec {
        exec.realize_loaded_objc(&leaked.report)
            .map_err(|e| format!("{resolved}: objc: {e}"))?;
    }

    // Every `_dyld_register_func_for_add_image` callback, before this image's
    // own code runs. See [`announce`].
    announce(leaked.handle, leaked.slide);

    // Now the image's own code, in dyld's order.
    if let Some(exec) = exec {
        exec.run_loaded_load_methods(&leaked.report)
            .map_err(|e| format!("{resolved}: objc +load: {e}"))?;
    }

    run_initializers(leaked)?;
    Ok(leaked.handle)
}

/// Every address a call site in `report` might name, both directions.
fn collect_imports(
    report: &LinkReport,
    is_guest: impl Fn(u64) -> bool,
) -> (BTreeMap<u64, ImportSite>, AddrMap<ImportSite>) {
    let mut imports: BTreeMap<u64, ImportSite> = BTreeMap::new();
    for b in report.bindings() {
        let site = ImportSite {
            symbol: b.symbol.clone(),
            library: b.library.clone(),
            resolved: b.resolved,
            tag: b.tag,
            is_stub: false,
        };
        imports.insert(b.slot_va, site.clone());
        if let Some(stub) = b.stub_va {
            imports.insert(
                stub,
                ImportSite {
                    is_stub: true,
                    ..site
                },
            );
        }
    }
    for image in &report.images {
        for m in &image.missing {
            let site = ImportSite {
                symbol: m.symbol.clone(),
                library: m.library.clone(),
                resolved: 0,
                tag: 0,
                is_stub: false,
            };
            imports.insert(m.slot_va, site.clone());
            if let Some(stub) = m.stub_va {
                imports.insert(
                    stub,
                    ImportSite {
                        is_stub: true,
                        ..site
                    },
                );
            }
        }
    }

    let mut by_symbol: Vec<&ImportSite> = imports.values().filter(|s| !s.is_stub).collect();
    by_symbol.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    let mut by_target: AddrMap<ImportSite> = AddrMap::default();
    for site in by_symbol {
        // A site that resolved into the **guest** must never appear here, and
        // the reason is a two-billion-iteration livelock rather than a nicety.
        if site.resolved != 0 && !is_guest(site.resolved) {
            by_target
                .entry(site.resolved)
                .or_insert_with(|| site.clone());
        }
    }
    (imports, by_target)
}

/// Run the image's constructors, as dyld does before `dlopen` returns.
fn run_initializers(image: &'static Image) -> std::result::Result<(), String> {
    let Some(exec) = crate::exec::active() else {
        return Err("no execution backend is active".to_string());
    };
    let (argc, argv, envp, apple) = exec.main_args();
    let initializers: Vec<u64> = image
        .report
        .images
        .iter()
        .rev()
        .flat_map(|i| i.initializers.iter().copied())
        .collect();
    for init in initializers {
        exec.call(init, &[argc, argv, envp, apple])
            .map_err(|e| format!("initializer at {init:#x}: {e}"))?;
    }
    Ok(())
}

/// Where a `dlopen` argument actually points.
fn search_path(var: &str, name: &str) -> Vec<String> {
    let Some(value) = std::env::var_os(var) else {
        return Vec::new();
    };
    value
        .to_string_lossy()
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| format!("{}/{name}", dir.trim_end_matches('/')))
        .collect()
}

fn resolve_path(path: &str) -> Option<String> {
    // Ahead of everything: the Steam client a game asks for is at whatever
    // absolute path its install would have had, and what has to open is the
    // bridge's own. See [`crate::steam::redirect`].
    if let Some(bridged) = crate::steam::redirect(path) {
        return Some(bridged);
    }
    let executable = EXECUTABLE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .map(|(path, _)| path);
    let loader_dir = executable.as_deref().map(machish::loader::loader_directory);

    let mut candidates: Vec<String> = Vec::new();
    if path.starts_with('@') {
        let rpaths = executable.as_deref().map(rpaths_of).unwrap_or_default();
        let ctx = PathContext {
            executable: executable.as_deref(),
            loader: executable.as_deref(),
            rpaths: &rpaths,
        };
        candidates.extend(machish::install_name::expand(path, &ctx));
    } else {
        // `DYLD_LIBRARY_PATH` first, and `DYLD_FALLBACK_LIBRARY_PATH` after the
        // ordinary candidates - the order the host's own loader uses.
        let name = path.rsplit('/').next().unwrap_or(path);
        candidates.extend(search_path("DYLD_LIBRARY_PATH", name));
        candidates.push(path.to_string());
        if !path.starts_with('/')
            && let Some(dir) = loader_dir
        {
            candidates.push(format!("{dir}/{name}"));
            candidates.push(format!("{dir}/../Frameworks/{name}"));
            candidates.push(format!("{dir}/../Resources/{name}"));
        }
        // The executable's own `LC_RPATH` directories, searched by leaf name.
        if let Some(exe) = executable.as_deref() {
            let rpaths = rpaths_of(exe);
            let ctx = PathContext {
                executable: Some(exe),
                loader: Some(exe),
                rpaths: &rpaths,
            };
            candidates.extend(machish::install_name::expand(
                &format!("@rpath/{name}"),
                &ctx,
            ));
        }
        candidates.extend(search_path("DYLD_FALLBACK_LIBRARY_PATH", name));
    }

    // Each candidate is a path the *guest* named, so the prefix's spelling of it
    // is tried before the path as given. Both, and in that order: a prefix holds
    // the Darwin userland a guest asks for, and the runtime's own libraries -
    // the Steam bridge, a dylib beside the executable - are on the host beside
    // it. See `docs/prefixes.md`.
    candidates
        .into_iter()
        .flat_map(|c| [prefix::ns::host_path(&c), std::path::PathBuf::from(c)])
        .filter_map(|c| std::fs::canonicalize(&c).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .next()
}

/// The `LC_RPATH` entries of the image at `path`, for `@rpath` expansion.
fn rpaths_of(path: &str) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    match crate::exec::active().map(|exec| exec.engine()) {
        Some(crate::exec::Engine::Native) => {
            let Ok(object) = MachObject::parse_any(&bytes, crate::exec::native::HOST_SPEC) else {
                return Vec::new();
            };
            object.rpaths().map(str::to_string).collect()
        }
        _ => {
            let Ok(object) = MachObject::parse_any(&bytes, X86_64) else {
                return Vec::new();
            };
            object.rpaths().map(str::to_string).collect()
        }
    }
}

/// The address `name` has in `handle`, as `dlsym` reports it.
fn lookup(handle: u64, name: &str) -> Option<u64> {
    // A handle for a library this runtime is. Scoped: a guest that opened one
    // framework and asks it for another's symbol should be told no, which is
    // what the host's loader would say.
    //
    // Recorded like any other host answer: what a synthetic library returns is
    // a host address as much as `dlsym`'s is - the GL driver's entry point, or
    // one of this runtime's own functions - and a guest that calls it through a
    // register reaches it by no import site unless one is made here.
    if let Some(library) = synthetic::by_handle(handle) {
        let addr = (library.symbol)(name)?;
        return Some(remember_native_symbol(
            addr,
            name,
            library.names.first().cloned(),
        ));
    }

    // A handle from the host's loader.
    if let Some(path) = native_handle(handle) {
        let c = CString::new(name).ok()?;
        // SAFETY: a handle this module obtained from `dlopen` and never closed,
        // and a null-terminated name.
        let found = unsafe { libc::dlsym(handle as usize as *mut libc::c_void, c.as_ptr()) };
        if found.is_null() {
            return None;
        }
        return Some(remember_native_symbol(
            found as usize as u64,
            name,
            Some(path),
        ));
    }

    // `dlsym` takes the C identifier; a Mach-O export carries the underscore.
    let mangled = format!("_{name}");
    let scoped: Vec<&'static Image> = match snapshot().into_iter().find(|i| i.handle == handle) {
        Some(image) => vec![image],
        // Every pseudo-handle - `RTLD_DEFAULT`, `RTLD_SELF`, `RTLD_NEXT`, and
        // the token `dlopen(NULL)` returns - is treated as a global search.
        None => snapshot(),
    };
    for image in scoped {
        if let Some(export) = image.exports.get(&mangled)
            && let Some(addr) = export.address(image.slide)
        {
            return Some(addr);
        }
    }
    // The **main executable's** own exports, which a global search must include
    // and which nothing above covers.
    if let Some(main) = main_exports()
        && let Some(export) = main.exports.get(&mangled)
        && let Some(addr) = export.address(main.slide)
    {
        return Some(addr);
    }

    // Then the libraries this runtime is, for a global search.
    if let Some(addr) = crate::frameworks::symbol(name) {
        return Some(remember_native_symbol(addr, name, None));
    }

    // The global fallback, which is a *host* search - and one that has to go
    // through `darwin`, not straight to `dlsym`. A guest asking `RTLD_DEFAULT`
    // for `readdir` on Linux would otherwise get glibc's, whose `struct dirent`
    // is not the one the guest was compiled against; `darwin::symbol` refuses a
    // divergent name by the same list the loader binds against.
    let addr = system_symbol(name)?;
    Some(remember_native_symbol(addr, name, None))
}

/// What libSystem answers `name` with: the global search's host half, and the
/// handle [`crate::frameworks`] registers for `dlopen("/usr/lib/libSystem.B.dylib")`,
/// which must agree with it.
pub fn system_symbol(name: &str) -> Option<u64> {
    crate::shims::openat::host_substitute(name).or_else(|| darwin::symbol(name))
}

/// Handles the *host's* `dlopen` returned, and the paths they were asked for.
static NATIVE: Mutex<Vec<(u64, String)>> = Mutex::new(Vec::new());

/// The import sites a native `dlsym` result has been given.
static NATIVE_SYMBOLS: Mutex<BTreeMap<u64, &'static ImportSite>> = Mutex::new(BTreeMap::new());

/// How many entries [`NATIVE_SYMBOLS`] has, read without the lock.
static NATIVE_SYMBOL_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Open `path` with the host's loader, as a native arm64 library.
///
/// `RTLD_GLOBAL` and never closed, for the reason the native provider's own fallback
/// is: closing it would dangle every pointer already written into a guest slot.
fn open_native(path: &str) -> Option<u64> {
    let c = CString::new(path).ok()?;
    // SAFETY: a null-terminated path and the host's own loader. The handle is
    // kept for the process; see the note above on why it is never closed.
    let handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL) };
    if handle.is_null() {
        return None;
    }
    let handle = handle as usize as u64;
    let mut native = NATIVE.lock().unwrap_or_else(|e| e.into_inner());
    if !native.iter().any(|(h, _)| *h == handle) {
        native.push((handle, path.to_string()));
    }
    Some(handle)
}

/// Whether `handle` came from [`open_native`], and the path it was opened as.
fn native_handle(handle: u64) -> Option<String> {
    NATIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(h, _)| *h == handle)
        .map(|(_, path)| path.clone())
}

/// Record a host address handed to the guest by `dlsym`, so it can be called.
fn remember_native_symbol(addr: u64, name: &str, library: Option<String>) -> u64 {
    let mut map = NATIVE_SYMBOLS.lock().unwrap_or_else(|e| e.into_inner());
    let before = map.len();
    let site = *map.entry(addr).or_insert_with(|| {
        Box::leak(Box::new(ImportSite {
            // The Mach-O spelling, because that is what the signature table is
            // keyed by - `dlsym` takes the C identifier and drops the underscore.
            symbol: format!("_{name}"),
            library,
            resolved: addr,
            tag: 0,
            is_stub: false,
        }))
    });
    if map.len() != before {
        // Publish the snapshot first, then the count, and both while the lock
        // is still held.
        let view: Vec<(u64, &'static ImportSite)> = map.iter().map(|(a, s)| (*a, *s)).collect();
        NATIVE_SYMBOL_VIEW.store(Box::leak(Box::new(view)), Ordering::Release);
        NATIVE_SYMBOL_COUNT.store(map.len(), Ordering::Release);
    }
    drop(map);

    // And the whole reason this passthrough is worth optimising at all.
    crate::trampoline::for_site(site).unwrap_or(addr)
}

/// The published, immutable half of [`NATIVE_SYMBOLS`]: a leaked, sorted slice of
/// `(address, site)`, swapped whole.
static NATIVE_SYMBOL_VIEW: AtomicPtr<Vec<(u64, &'static ImportSite)>> =
    AtomicPtr::new(core::ptr::null_mut());

/// The import site for a host address the guest got from `dlsym`, if any.
pub fn native_symbol_at(host: u64) -> Option<&'static ImportSite> {
    if NATIVE_SYMBOL_COUNT.load(Ordering::Acquire) == 0 {
        return None;
    }
    let published = NATIVE_SYMBOL_VIEW.load(Ordering::Acquire);
    if published.is_null() {
        return None;
    }
    // SAFETY: leaked by `remember_native_symbol` and never freed, so the slice
    // stays valid for the life of the process - the same claim `VIEW` makes.
    //
    // A snapshot one `dlsym` behind can miss a site another thread has just
    // recorded, and unlike a thunk that is harmless: the caller falls through to
    // `UnknownIndirectCall`, which is what it would have got a microsecond
    // earlier anyway, and the guest cannot have the address yet - `dlsym` has
    // not returned it.
    let view = unsafe { &*published };
    // Sorted, so the two ends bound it: the run loop asks this of a *guest* pc on
    // every window miss, and a guest pc is nowhere near a host symbol.
    let (Some(first), Some(last)) = (view.first(), view.last()) else {
        return None;
    };
    if host < first.0 || host > last.0 {
        return None;
    }
    view.binary_search_by_key(&host, |(a, _)| *a)
        .ok()
        .map(|i| view[i].1)
}

/// The `add image` callbacks the guest has registered, as native-callable
/// addresses.
static ADD_IMAGE: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// Tell one callback about one image.
///
/// The two arguments are dyld's: the image's `mach_header` and its slide.
fn notify(thunk: u64, header: u64, slide: u64) {
    if thunk == 0 || header == 0 {
        return;
    }
    if crate::shims::vmmap::tracing() {
        eprintln!("rosetta3: add-image notice: header {header:#x}, slide {slide:#x}");
    }
    // SAFETY: `thunk` is native code of dyld's callback signature - the guest's
    // own function, or a [`crate::thunk`] standing for it.
    let f: extern "C" fn(u64, u64) = unsafe { std::mem::transmute::<u64, _>(thunk) };
    f(header, slide);
}

/// Announce `image` to every callback registered so far.
fn announce(header: u64, slide: u64) {
    let callbacks = ADD_IMAGE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for thunk in callbacks {
        notify(thunk, header, slide);
    }
}

fn set_error(message: String) {
    *ERROR.lock().unwrap_or_else(|e| e.into_inner()) = CString::new(message).ok();
}

fn clear_error() {
    *ERROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The `dyld_unwind_sections` this runtime fills in for one of its own images.
///
/// Returns false when `addr` is in no image this runtime mapped, which is the
/// caller's cue to ask the host.
fn unwind_sections_of(addr: u64) -> Option<[u8; 40]> {
    let (header, slide) = guest_image_at(addr)?;
    let mut words = [0u8; 40];
    words[0..8].copy_from_slice(&header.to_le_bytes());
    for (i, section) in ["__eh_frame", "__unwind_info"].into_iter().enumerate() {
        let (at, len) = section_in(header, section)
            .map_or((0, 0), |(addr, size)| (addr.wrapping_add(slide), size));
        let base = 8 + i * 16;
        words[base..base + 8].copy_from_slice(&at.to_le_bytes());
        words[base + 8..base + 16].copy_from_slice(&len.to_le_bytes());
    }
    Some(words)
}

/// `(mach_header VA, slide)` for the guest image containing `va`.
fn guest_image_at(va: u64) -> Option<(u64, u64)> {
    fn containing(report: &LinkReport, va: u64) -> Option<(u64, u64)> {
        report
            .images
            .iter()
            .find(|i| {
                i.segments
                    .iter()
                    .any(|s| va >= s.vmaddr && va < s.vmaddr.wrapping_add(s.vmsize))
            })
            .map(|i| (i.base, i.slide))
    }
    if let Some(found) = find(|i| containing(&i.report, va)) {
        return Some(found);
    }
    if let Some(found) = MAPPED.get().and_then(|images| {
        images
            .iter()
            .find(|i| i.segments.iter().any(|(lo, hi)| va >= *lo && va < *hi))
    }) {
        return Some((found.header, found.slide));
    }
    containing(&crate::thunk::current()?.program.report, va)
}

/// `(unslid addr, size)` of a section in the guest image at `header`.
///
/// The image's own load commands, walked here rather than through libmacho's
/// `getsectbynamefromheader_64`. That entry point is Darwin's alone, so off it
/// the lookup failed for every image and [`unwind_sections_of`] answered with
/// both section bounds zero - a `_dyld_find_unwind_sections` that *succeeds*
/// and reports the frame has no unwind info. A throw then reaches
/// `_Unwind_RaiseException`, cannot unwind even its own frame, and the process
/// dies with a stack overflow rather than with a missing symbol.
fn section_in(header: u64, section: &str) -> Option<(u64, u64)> {
    /// `MH_MAGIC_64`.
    const MAGIC: u32 = 0xfeed_facf;
    /// `LC_SEGMENT_64`.
    const SEGMENT_64: u32 = 0x19;
    /// `sizeof(struct mach_header_64)`.
    const HEADER: usize = 32;
    /// `sizeof(struct segment_command_64)`, which the sections follow.
    const SEGMENT: usize = 72;
    /// `sizeof(struct section_64)`.
    const SECTION: usize = 80;

    let base = header as *const u8;
    if base.is_null() {
        return None;
    }
    // SAFETY: `header` is an image this runtime mapped, so its `mach_header_64`
    // and the `sizeofcmds` bytes of load commands after it are mapped and stay
    // mapped for the life of the process. Every read below is inside that run,
    // bounded by the header's own `ncmds` and each command's `cmdsize`.
    unsafe {
        if base.cast::<u32>().read_unaligned() != MAGIC {
            return None;
        }
        let ncmds = base.add(16).cast::<u32>().read_unaligned();
        let mut cmd = base.add(HEADER);
        for _ in 0..ncmds {
            let kind = cmd.cast::<u32>().read_unaligned();
            let cmdsize = cmd.add(4).cast::<u32>().read_unaligned() as usize;
            if cmdsize < SEGMENT {
                // Shorter than a segment command, so it is not one; a zero size
                // would also loop forever.
                if cmdsize < 8 {
                    return None;
                }
                cmd = cmd.add(cmdsize);
                continue;
            }
            if kind == SEGMENT_64 {
                let nsects = cmd.add(64).cast::<u32>().read_unaligned() as usize;
                for i in 0..nsects {
                    let at = SEGMENT + i * SECTION;
                    if at + SECTION > cmdsize {
                        break;
                    }
                    let sect = cmd.add(at);
                    // `sectname` is sixteen bytes, NUL-padded rather than
                    // NUL-terminated when the name fills the field.
                    let name = core::slice::from_raw_parts(sect, 16);
                    let name = name.split(|b| *b == 0).next().unwrap_or_default();
                    if name == section.as_bytes() {
                        return Some((
                            sect.add(32).cast::<u64>().read_unaligned(),
                            sect.add(40).cast::<u64>().read_unaligned(),
                        ));
                    }
                }
            }
            cmd = cmd.add(cmdsize);
        }
    }
    None
}

/// A `segment_command_64`, or one of its `section_64` records, in the main
/// guest image. The pointer is returned exactly as libmacho's process-global
/// `getsegbyname` and `getsectbyname` return it.
fn load_command_in(header: u64, segment: &[u8], section: Option<&[u8]>) -> u64 {
    const MAGIC: u32 = 0xfeed_facf;
    const SEGMENT_64: u32 = 0x19;
    const HEADER: usize = 32;
    const SEGMENT: usize = 72;
    const SECTION: usize = 80;

    let base = header as *const u8;
    if base.is_null() {
        return 0;
    }
    // SAFETY: `header` names the mapped main image and every load-command read
    // is bounded by `ncmds`, `cmdsize`, and the segment's section count.
    unsafe {
        if base.cast::<u32>().read_unaligned() != MAGIC {
            return 0;
        }
        let ncmds = base.add(16).cast::<u32>().read_unaligned();
        let mut cmd = base.add(HEADER);
        for _ in 0..ncmds {
            let kind = cmd.cast::<u32>().read_unaligned();
            let cmdsize = cmd.add(4).cast::<u32>().read_unaligned() as usize;
            if cmdsize < 8 {
                return 0;
            }
            if kind == SEGMENT_64 && cmdsize >= SEGMENT {
                let name = core::slice::from_raw_parts(cmd.add(8), 16);
                let name = name.split(|byte| *byte == 0).next().unwrap_or_default();
                if name == segment {
                    let Some(section) = section else {
                        return cmd as usize as u64;
                    };
                    let nsects = cmd.add(64).cast::<u32>().read_unaligned() as usize;
                    for index in 0..nsects {
                        let offset = SEGMENT + index * SECTION;
                        if offset + SECTION > cmdsize {
                            break;
                        }
                        let record = cmd.add(offset);
                        let name = core::slice::from_raw_parts(record, 16);
                        let name = name.split(|byte| *byte == 0).next().unwrap_or_default();
                        if name == section {
                            return record as usize as u64;
                        }
                    }
                    return 0;
                }
            }
            cmd = cmd.add(cmdsize);
        }
    }
    0
}

/// What [`dladdr`] found: the four `Dl_info` fields, as host addresses.
struct Described {
    fname: u64,
    fbase: u64,
    sname: u64,
    saddr: u64,
}

/// `addr` as an image and a symbol, if this runtime is the one that mapped it.
///
/// Covers both kinds of guest image - the program itself and anything [`open`] loaded -
/// because a caller cannot tell them apart and neither should the answer.
fn describe(addr: u64) -> Option<Described> {
    if addr == 0 {
        return None;
    }
    if let Some(found) = find(|i| {
        region_of(i, addr).map(|_| {
            let (sname, saddr) = nearest_export(&i.exports, i.slide, addr);
            Described {
                fname: path_cstr(&i.path),
                fbase: i.handle,
                sname,
                saddr,
            }
        })
    }) {
        return Some(found);
    }
    // The main executable, which is a guest image with no `Image` record.
    let vm = crate::thunk::current()?;
    if !vm.program.is_guest_address(addr) {
        return None;
    }
    let (path, slide) = EXECUTABLE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()?;
    let (sname, saddr) = main_exports()
        .map(|m| nearest_export(&m.exports, slide, addr))
        .unwrap_or((0, 0));
    Some(Described {
        fname: path_cstr(&path),
        fbase: slide,
        sname,
        saddr,
    })
}

/// The export at or below `addr`, as `(name, address)`, or `(0, 0)`.
fn nearest_export(exports: &ExportTable, slide: u64, addr: u64) -> (u64, u64) {
    let mut best: Option<(&str, u64)> = None;
    for e in exports.entries() {
        let Some(at) = e.address(slide) else {
            continue;
        };
        if at <= addr && best.is_none_or(|(_, b)| at > b) {
            best = Some((e.name.as_str(), at));
        }
    }
    match best {
        Some((name, at)) => (symbol_cstr(name), at),
        None => (0, 0),
    }
}

/// A NUL-terminated copy of `text`, kept for the life of the process.
fn interned(text: &str) -> u64 {
    static STRINGS: Mutex<Option<BTreeMap<String, &'static CString>>> = Mutex::new(None);
    let mut map = STRINGS.lock().unwrap_or_else(|e| e.into_inner());
    let map = map.get_or_insert_with(BTreeMap::new);
    if let Some(existing) = map.get(text) {
        return existing.as_ptr() as usize as u64;
    }
    let Ok(owned) = CString::new(text) else {
        return 0;
    };
    let leaked: &'static CString = Box::leak(Box::new(owned));
    map.insert(text.to_string(), leaked);
    leaked.as_ptr() as usize as u64
}

fn path_cstr(path: &str) -> u64 {
    interned(path)
}

/// The symbol name as C sees it: `dladdr` reports `main`, not `_main`.
fn symbol_cstr(name: &str) -> u64 {
    interned(name.strip_prefix('_').unwrap_or(name))
}

/// The `dyld` family, one implementation for both backends.
///
/// A native guest calls these through its import slots. The emulated backend
/// reaches the same functions through the ABI table, so every signature here
/// has a row there - and `dlopen`/`dlclose` are marked as reentering the guest,
/// because initializers and terminators run inside them and an inlined call
/// cannot host that.
pub(crate) mod entry {
    use std::ffi::{CStr, c_char, c_void};

    use super::Ordering;

    // A guest pointer is a host pointer under both backends: the emulated
    // guest is identity-mapped.

    /// `uint32_t _dyld_image_count(void)`
    pub extern "C" fn dyld_image_count() -> u32 {
        super::image_count()
    }

    /// `const struct mach_header *_dyld_get_image_header(uint32_t)`
    pub extern "C" fn dyld_get_image_header(index: u32) -> u64 {
        super::indexed_image(index).map_or(0, |(header, _, _)| header)
    }

    /// `const struct mach_header_64 *_NSGetMachExecuteHeader(void)`
    ///
    /// The guest's main image, which is what `__mh_execute_header` names inside
    /// it - never this runtime's own header.
    pub extern "C" fn ns_get_mach_execute_header() -> u64 {
        super::MAIN_HEADER.load(Ordering::Acquire)
    }

    /// `intptr_t _dyld_get_image_vmaddr_slide(uint32_t)`
    pub extern "C" fn dyld_get_image_vmaddr_slide(index: u32) -> u64 {
        super::indexed_image(index).map_or(0, |(_, slide, _)| slide)
    }

    /// `const char *_dyld_get_image_name(uint32_t)`
    pub extern "C" fn dyld_get_image_name(index: u32) -> u64 {
        super::indexed_image(index).map_or(0, |(_, _, path)| super::path_cstr(&path))
    }

    /// `const struct mach_header *_dyld_image_containing_address(const void *)`
    pub extern "C" fn dyld_image_containing_address(va: u64) -> u64 {
        super::guest_image_at(va).map_or(0, |(header, _)| header)
    }

    /// `void _dyld_register_func_for_add_image(void (*)(const struct mach_header *,
    /// intptr_t))`
    ///
    /// `f` is native code: a thunk under emulation, since the table names it as a
    /// callback.
    pub extern "C" fn dyld_register_func_for_add_image(f: u64) {
        if f == 0 {
            return;
        }
        // Registered before the retrospective notices, so an image one of them
        // loads is announced rather than missed.
        super::ADD_IMAGE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(f);

        let mut already: Vec<(u64, u64)> = Vec::new();
        if let Some((_, slide)) = super::EXECUTABLE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            already.push((super::MAIN_HEADER.load(Ordering::Acquire), slide));
        }
        already.extend(super::snapshot().iter().map(|i| (i.handle, i.slide)));
        for (header, slide) in already {
            super::notify(f, header, slide);
        }
    }

    /// `void _dyld_register_func_for_remove_image(...)`. Accepted and never
    /// called, because nothing is ever unloaded.
    pub extern "C" fn dyld_register_func_for_remove_image(_f: u64) {}

    /// `bool _dyld_find_unwind_sections(void *addr, struct dyld_unwind_sections *)`
    ///
    /// dyld answers only for images it mapped, so without this every guest frame
    /// has no unwind info and a `throw` reaches `terminate`. The layout, read out
    /// of the vendored `libunwind`: five words - `mach_header *`, then address
    /// and length of `__eh_frame` and of `__unwind_info`.
    ///
    /// # Safety
    ///
    /// `out` is null or forty writable bytes.
    pub unsafe extern "C" fn dyld_find_unwind_sections(addr: *const c_void, out: *mut u8) -> bool {
        let addr = addr as usize as u64;
        let Some(words) = super::unwind_sections_of(addr) else {
            let Some(host) = crate::host::symbol("_dyld_find_unwind_sections") else {
                return false;
            };
            // SAFETY: `bool (*)(void *, struct dyld_unwind_sections *)`, with the
            // caller's own arguments.
            let f: unsafe extern "C" fn(u64, *mut u8) -> bool =
                unsafe { core::mem::transmute(host as usize as *const ()) };
            return unsafe { f(addr, out) };
        };
        if out.is_null() {
            return false;
        }
        // SAFETY: the caller reserved the forty bytes written here.
        unsafe { core::ptr::copy_nonoverlapping(words.as_ptr(), out, words.len()) };
        // True even when both sections are absent: that is an answer.
        true
    }

    /// `const struct segment_command_64 *getsegbyname(const char *)`, over the
    /// guest's main image rather than this process's.
    ///
    /// # Safety
    ///
    /// `segment` is NUL-terminated.
    pub unsafe extern "C" fn getsegbyname(segment: *const c_char) -> u64 {
        if segment.is_null() {
            return 0;
        }
        // SAFETY: the caller states `segment` is NUL-terminated.
        let segment = unsafe { CStr::from_ptr(segment) };
        super::load_command_in(
            super::MAIN_HEADER.load(Ordering::Acquire),
            segment.to_bytes(),
            None,
        )
    }

    /// `const struct section_64 *getsectbyname(const char *, const char *)`
    ///
    /// # Safety
    ///
    /// Both names are NUL-terminated.
    pub unsafe extern "C" fn getsectbyname(segment: *const c_char, section: *const c_char) -> u64 {
        if segment.is_null() || section.is_null() {
            return 0;
        }
        // SAFETY: the caller states both are NUL-terminated.
        let (segment, section) = unsafe { (CStr::from_ptr(segment), CStr::from_ptr(section)) };
        super::load_command_in(
            super::MAIN_HEADER.load(Ordering::Acquire),
            segment.to_bytes(),
            Some(section.to_bytes()),
        )
    }

    /// `void *dlopen(const char *path, int mode)`
    ///
    /// The mode is ignored: nothing is unloaded and every image is global. That
    /// also sidesteps the flag values, which differ between Darwin and glibc.
    ///
    /// # Safety
    ///
    /// `path` is null or NUL-terminated.
    pub unsafe extern "C" fn dlopen(path: *const c_char, mode: i32) -> *mut c_void {
        if path.is_null() {
            super::clear_error();
            return super::global_handle() as usize as *mut c_void;
        }
        // SAFETY: the caller states `path` is NUL-terminated.
        let path = unsafe { CStr::from_ptr(path) }
            .to_string_lossy()
            .into_owned();
        if crate::shims::vmmap::tracing() {
            eprintln!("rosetta3: dlopen({path:?}, flags {mode:#x})");
        }
        let handle = match super::open(&path) {
            Ok(handle) => Some(handle),
            // No Mach-O for this backend on disk under that name.
            Err(reason) => super::open_native(&path).or_else(|| {
                super::set_error(format!("dlopen({path}): {reason}"));
                None
            }),
        };
        match handle {
            Some(handle) => {
                super::clear_error();
                handle as usize as *mut c_void
            }
            None => std::ptr::null_mut(),
        }
    }

    /// `void *dlsym(void *handle, const char *name)`
    ///
    /// # Safety
    ///
    /// `name` is NUL-terminated. `handle` is never dereferenced.
    pub unsafe extern "C" fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void {
        if name.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: the caller states `name` is NUL-terminated.
        let name = unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned();
        match super::lookup(handle as usize as u64, &name) {
            Some(addr) => {
                super::clear_error();
                addr as usize as *mut c_void
            }
            None => {
                // Traced only: a null `dlsym` is how a program asks whether
                // something exists, sometimes once a frame.
                let err = format!("dlsym({name}): symbol not found");
                if crate::shims::vmmap::tracing() {
                    eprintln!("rosetta3: {err}");
                }
                super::set_error(err);
                std::ptr::null_mut()
            }
        }
    }

    /// `int dlclose(void *handle)`. Nothing is unloaded, so no terminator runs
    /// yet; the table still marks it as reentering for when one does.
    pub extern "C" fn dlclose(_handle: *mut c_void) -> i32 {
        super::clear_error();
        0
    }

    /// `char *dlerror(void)`, which clears the error it reports.
    pub extern "C" fn dlerror() -> *const c_char {
        let taken = super::ERROR
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let mut reported = super::REPORTED.lock().unwrap_or_else(|e| e.into_inner());
        *reported = taken;
        reported
            .as_ref()
            .map_or(std::ptr::null(), |s| s.as_ptr().cast())
    }

    /// `int dladdr(const void *addr, Dl_info *info)`
    ///
    /// # Safety
    ///
    /// `info` is null or four writable words.
    pub unsafe extern "C" fn dladdr(addr: *const c_void, info: *mut u64) -> i32 {
        let addr = addr as usize as u64;
        let Some(found) = super::describe(addr) else {
            // Not ours; the host knows its own images.
            let Some(host) = crate::host::symbol("dladdr") else {
                return 0;
            };
            // SAFETY: `int (const void *, Dl_info *)`; Darwin's and glibc's
            // `Dl_info` are the same four pointers.
            let f: unsafe extern "C" fn(u64, *mut u64) -> i32 =
                unsafe { core::mem::transmute(host as usize as *const ()) };
            return unsafe { f(addr, info) };
        };
        if info.is_null() {
            return 0;
        }
        // SAFETY: the caller states `info` is four writable words.
        unsafe {
            info.write(found.fname);
            info.add(1).write(found.fbase);
            info.add(2).write(found.sname);
            info.add(3).write(found.saddr);
        }
        // Non-zero on success, unlike the rest of `<dlfcn.h>`.
        1
    }
}

/// Every name [`provider`] defines, and the function it binds to.
fn entries() -> [(&'static str, *const ()); 15] {
    [
        ("_dlopen", entry::dlopen as *const ()),
        ("_dlsym", entry::dlsym as *const ()),
        ("_dlclose", entry::dlclose as *const ()),
        ("_dlerror", entry::dlerror as *const ()),
        ("_dladdr", entry::dladdr as *const ()),
        ("__dyld_image_count", entry::dyld_image_count as *const ()),
        (
            "__dyld_get_image_header",
            entry::dyld_get_image_header as *const (),
        ),
        (
            "__NSGetMachExecuteHeader",
            entry::ns_get_mach_execute_header as *const (),
        ),
        (
            "__dyld_get_image_vmaddr_slide",
            entry::dyld_get_image_vmaddr_slide as *const (),
        ),
        (
            "__dyld_get_image_name",
            entry::dyld_get_image_name as *const (),
        ),
        (
            "__dyld_image_containing_address",
            entry::dyld_image_containing_address as *const (),
        ),
        (
            "__dyld_register_func_for_add_image",
            entry::dyld_register_func_for_add_image as *const (),
        ),
        (
            "__dyld_register_func_for_remove_image",
            entry::dyld_register_func_for_remove_image as *const (),
        ),
        ("_getsegbyname", entry::getsegbyname as *const ()),
        ("_getsectbyname", entry::getsectbyname as *const ()),
    ]
}

/// The `dyld` family, claimed ahead of the host's: dyld never saw the images
/// this runtime mapped, and off a Mac there is no dyld at all.
///
/// `__dyld_find_unwind_sections` is [`crate::shims::runtime::provider`]'s,
/// which is in the native chain even when this one is not.
pub fn provider() -> SyntheticDylib {
    let mut lib = SyntheticDylib::new("<rosetta3 dyld>");
    lib.flat(true);
    for (name, f) in entries() {
        lib.define_fn(name, f);
    }
    lib
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::{Classification, Lookup};

    #[test]
    fn every_entry_has_a_signature_the_emulated_backend_can_call() {
        let mut claimed = Vec::new();
        machish::resolve::SymbolProvider::enumerate(&provider(), &mut |name, _| {
            claimed.push(name.to_string())
        });
        assert_eq!(claimed.len(), entries().len());
        for name in claimed
            .iter()
            .map(String::as_str)
            .chain(["__dyld_find_unwind_sections"])
        {
            assert!(
                matches!(crate::abi::table::lookup(0, name), Lookup::Known(_)),
                "{name} has no signature"
            );
            assert!(!crate::shims::is_shimmed(name), "{name} is still shimmed");
        }
    }

    #[test]
    fn calls_that_run_guest_code_are_never_inlined() {
        for name in [
            "_dlopen",
            "_dlclose",
            "__dyld_register_func_for_add_image",
            "__dyld_register_func_for_remove_image",
        ] {
            assert_eq!(
                crate::abi::classify(0, name),
                Classification::Slow,
                "{name}"
            );
        }
    }

    #[test]
    fn the_global_handle_is_not_a_plausible_image_base() {
        // Compared against `Image::handle`, a mapped base address.
        assert!(global_handle() > 0x1000);
    }

    #[test]
    fn a_missing_file_is_a_named_failure_rather_than_a_panic() {
        let err = open("/nonexistent/libnothing.dylib").unwrap_err();
        assert!(err.contains("libnothing.dylib"), "{err}");
    }

    #[test]
    fn a_native_arm64_image_is_refused_by_name() {
        let host_dylib = "/usr/lib/libSystem.B.dylib";
        if std::path::Path::new(host_dylib).exists() {
            let err = open(host_dylib).unwrap_err();
            assert!(
                err.contains("x86-64") || err.contains("no such file"),
                "{err}"
            );
        }
    }
}
