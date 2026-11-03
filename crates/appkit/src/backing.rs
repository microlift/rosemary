use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use foundation::geometry::{NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

static NEXT_STORE_ID: AtomicU64 = AtomicU64::new(1);

pub struct Store {
    /// Monotonic identity for full replacements. An allocated view address
    /// and a reused generation number never authenticate prior CPU pixels.
    instance: u64,
    pub width: u32,
    pub height: u32,
    /// Where the store's top-left pixel is in the view it holds: a view's
    /// store covers only the part of it on screen. Zero for every other store.
    pub origin: (i64, i64),
    /// Premultiplied B, G, R, A, top row first.
    pub pixels: Vec<u8>,
    dirty: bool,
    /// Bumped by every change to what the store shows, so that a presenter
    /// can tell whether a copy it took is still current without comparing it.
    generation: u64,
    // Bounded producer-authored changes. Missing generations mean full damage.
    changes: VecDeque<(u64, Rect)>,
    /// One of the ALPHA_* classes. Invalidated by every Store::touch and
    /// Store::resize. Atomic so immutable window snapshots remain shareable
    /// with the Vulkan worker.
    alpha_class: AtomicU8,
}

const ALPHA_UNCLASSIFIED: u8 = 0;
const ALPHA_TRANSLUCENT: u8 = 1;
const ALPHA_OPAQUE: u8 = 2;

impl Store {
    #[cfg(test)]
    pub(crate) fn for_tests(width: u32, height: u32) -> Self {
        Store::new(width, height)
    }

    /// Allocate an initially clean premultiplied-BGRA backing store.
    pub fn new(width: u32, height: u32) -> Self {
        Store {
            instance: NEXT_STORE_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            origin: (0, 0),
            pixels: vec![0; width as usize * height as usize * 4],
            dirty: false,
            generation: 0,
            changes: VecDeque::new(),
            alpha_class: AtomicU8::new(ALPHA_UNCLASSIFIED),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// What the store shows changed, and has to be presented again.
    fn touch(&mut self) {
        self.touch_region(Rect {
            x: 0,
            y: 0,
            width: i64::from(self.width),
            height: i64::from(self.height),
        });
    }

    fn touch_region(&mut self, area: Rect) {
        const MAX_GENERATIONS: usize = 32;
        self.alpha_class
            .store(ALPHA_UNCLASSIFIED, Ordering::Relaxed);
        self.dirty = true;
        let next = self.generation.wrapping_add(1);
        if next <= self.generation {
            self.changes.clear();
        }
        self.generation = next;
        let whole = Rect {
            x: 0,
            y: 0,
            width: i64::from(self.width),
            height: i64::from(self.height),
        };
        self.changes.push_back((next, area.intersection(&whole)));
        if self.changes.len() > MAX_GENERATIONS {
            self.changes.pop_front();
        }
    }

    /// Only consecutive producer-verified versions authorize localized
    /// work. Answered in the coordinates of what the store holds, which for a
    /// view's store is the view. Preserve disjoint rectangles: unioning six 1px changes at
    /// distant 6K positions would needlessly dirty megabytes of pixels.
    fn damage_regions_since(&self, generation: u64) -> Option<Vec<Rect>> {
        if generation >= self.generation {
            return None;
        }
        let mut expected = generation.checked_add(1)?;
        let mut regions = Vec::new();
        for &(at, rect) in &self.changes {
            if at < expected {
                continue;
            }
            if at != expected {
                return None;
            }
            if !rect.is_empty() {
                regions.push(Rect {
                    x: rect.x + self.origin.0,
                    y: rect.y + self.origin.1,
                    ..rect
                });
            }
            expected = expected.checked_add(1)?;
        }
        (expected == self.generation.checked_add(1)? && !regions.is_empty()).then_some(regions)
    }
    /// Conservative union for consumers that still accept one damage rect.
    fn damage_since(&self, generation: u64) -> Option<Rect> {
        self.damage_regions_since(generation)?
            .into_iter()
            .reduce(|a, b| a.union(&b))
    }

    fn fully_opaque(&self) -> bool {
        match self.alpha_class.load(Ordering::Relaxed) {
            ALPHA_TRANSLUCENT => false,
            ALPHA_OPAQUE => true,
            _ => {
                #[cfg(test)]
                OPAQUE_ALPHA_SCANS.with(|count| count.set(count.get() + 1));
                let opaque = self
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| pixel[3] == 255);
                self.alpha_class.store(
                    if opaque {
                        ALPHA_OPAQUE
                    } else {
                        ALPHA_TRANSLUCENT
                    },
                    Ordering::Relaxed,
                );
                opaque
            }
        }
    }

    /// Whether the store is already known to hold a translucent pixel,
    /// without scanning it.
    fn known_translucent(&self) -> bool {
        self.alpha_class.load(Ordering::Relaxed) == ALPHA_TRANSLUCENT
    }

    /// Record a translucent pixel found while checking only part of the
    /// store, so no later caller scans all of it to learn the same thing.
    fn found_translucent(&self) {
        self.alpha_class.store(ALPHA_TRANSLUCENT, Ordering::Relaxed);
    }

    /// Change the size, keeping what the top-left corner held.
    fn resize(&mut self, width: u32, height: u32) {
        if (self.width, self.height) == (width, height) {
            return;
        }
        let mut pixels = vec![0; width as usize * height as usize * 4];
        let columns = self.width.min(width) as usize;
        for row in 0..self.height.min(height) as usize {
            let from = row * self.width as usize * 4;
            let to = row * width as usize * 4;
            pixels[to..to + columns * 4].copy_from_slice(&self.pixels[from..from + columns * 4]);
        }
        *self = Store {
            instance: NEXT_STORE_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            origin: self.origin,
            pixels,
            dirty: true,
            generation: self.generation.wrapping_add(1),
            changes: VecDeque::new(),
            alpha_class: AtomicU8::new(ALPHA_UNCLASSIFIED),
        };
    }
}

/// A rectangle of pixels, top-left origin; empty when either extent is not
/// positive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

impl Rect {
    pub(crate) fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    pub(crate) fn intersection(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        Rect {
            x,
            y,
            width: (right - x).max(0),
            height: (bottom - y).max(0),
        }
    }

    /// `self` less every rectangle in `cuts`, as rectangles that do not
    /// overlap.
    fn subtract(&self, cuts: &[Rect]) -> Vec<Rect> {
        let mut pieces = vec![*self];
        for cut in cuts {
            let mut next = Vec::with_capacity(pieces.len() + 4);
            for piece in pieces {
                let inside = piece.intersection(cut);
                if inside.is_empty() {
                    next.push(piece);
                    continue;
                }
                let (right, bottom) = (inside.x + inside.width, inside.y + inside.height);
                let bands = [
                    (piece.x, piece.y, piece.width, inside.y - piece.y),
                    (
                        piece.x,
                        bottom,
                        piece.width,
                        piece.y + piece.height - bottom,
                    ),
                    (piece.x, inside.y, inside.x - piece.x, inside.height),
                    (
                        right,
                        inside.y,
                        piece.x + piece.width - right,
                        inside.height,
                    ),
                ];
                next.extend(
                    bands
                        .into_iter()
                        .map(|(x, y, width, height)| Rect {
                            x,
                            y,
                            width,
                            height,
                        })
                        .filter(|band| !band.is_empty()),
                );
            }
            pieces = next;
        }
        pieces
    }

    pub(crate) fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect {
            x,
            y,
            width: (self.x + self.width).max(other.x + other.width) - x,
            height: (self.y + self.height).max(other.y + other.height) - y,
        }
    }
}

/// A view's layer as the window's store leaves room for it: the presenter
/// keeps what the layer last presented there instead of the store's pixels.
#[derive(Clone, Copy, Debug)]
pub struct Hole {
    pub layer: usize,
    pub rect: Rect,
}

#[cfg(test)]
thread_local! {
    // Test probes must count only work performed by their own test thread.
    static OPAQUE_ALPHA_SCANS: Cell<usize> = const { Cell::new(0) };
}
#[cfg(test)]
pub(crate) fn opaque_alpha_scan_count_for_tests() -> usize {
    OPAQUE_ALPHA_SCANS.with(Cell::get)
}

/// Each window's composite, keyed by the SDL window's address.
static STORES: Mutex<Option<HashMap<usize, Store>>> = Mutex::new(None);

/// Each drawn view's own pixels, keyed by the view's address.
static VIEW_STORES: Mutex<Option<HashMap<usize, Store>>> = Mutex::new(None);

/// Immutable per-view pixel captures keyed by native content versions, not by
/// the view's window placement, opacity, corner mask or ancestor geometry.
/// Only the AppKit owner thread accesses these entries. The GPU worker gets
/// an Arc snapshot and never follows an Objective-C view pointer.
/// Exact producer damage from one captured CPU image to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeCpuDamage {
    pub from_revision: u64,
    pub rect: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeCpuCapture {
    pub revision: u64,
    pub damage: Option<NativeCpuDamage>,
}

struct NativeCapture {
    layer: usize,
    /// Identifies the exact backing Store allocation, not merely its generation.
    store_instance: Option<u64>,
    width: u32,
    height: u32,
    crop_x: i64,
    crop_y: i64,
    view_generation: Option<u64>,
    overlay_generation: u64,
    ca_content_revision: u64,
    ca_descendant_revision: u64,
    excluded_view_layers: u64,
    plain_store: bool,
    pixels: std::sync::Arc<Vec<u8>>,
    /// Fixed-size chunk hashes for exact plain opaque Store snapshots. A
    /// producer-attested rectangle can update only touched chunks; readers
    /// of an earlier Arc still own its complete, immutable pixel generation.
    opaque_hashes: Option<Vec<u64>>,
    revision: u64,
    damage: Option<NativeCpuDamage>,
    last_used: u64,
}
type CaptureKey = (usize, i64, i64, u32, u32, u32, u32, bool, u64);
/// Immutable CPU-owned row bands; cloning a new generation clones only
/// Arc descriptors and the bands intersecting producer-attested damage.
/// GPU readers can retain any previous generation without a full-frame memcpy.
#[derive(Clone)]
pub struct NativePixelTiles {
    pub width: u32,
    pub height: u32,
    pub revision: u64,
    bands: Vec<std::sync::Arc<Vec<u8>>>,
    hashes: Vec<u64>,
}

const NATIVE_TILE_ROWS: usize = 32;
const MIN_TILED_CAPTURE_BYTES: u64 = 1024 * 1024;

impl NativePixelTiles {
    pub fn copy_rect_to(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        dst: &mut [u8],
    ) -> Option<()> {
        let right = x.checked_add(width)?;
        let bottom = y.checked_add(height)?;
        if right > self.width || bottom > self.height {
            return None;
        }
        let row_bytes = usize::try_from(width).ok()?.checked_mul(4)?;
        if dst.len() != row_bytes.checked_mul(usize::try_from(height).ok()?)? {
            return None;
        }
        let stride = usize::try_from(self.width).ok()?.checked_mul(4)?;
        for row in 0..height as usize {
            let global_row = y as usize + row;
            let band_index = global_row / NATIVE_TILE_ROWS;
            let band_row = global_row % NATIVE_TILE_ROWS;
            let start = band_row.checked_mul(stride)?.checked_add(x as usize * 4)?;
            let source = self.bands.get(band_index)?.get(start..start + row_bytes)?;
            dst.get_mut(row * row_bytes..(row + 1) * row_bytes)?
                .copy_from_slice(source);
        }
        Some(())
    }
    pub fn len(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
    pub fn is_empty(&self) -> bool {
        self.bands.is_empty()
    }
    pub fn retained_bytes(&self) -> usize {
        self.bands.iter().map(|b| b.len()).sum()
    }
    /// Build an independently owned opaque snapshot from tightly packed BGRA.
    pub fn from_opaque_bgra(width: u32, height: u32, pixels: &[u8]) -> Option<Self> {
        let stride = (width as usize).checked_mul(4)?;
        let expected = stride.checked_mul(height as usize)?;
        if width == 0
            || height == 0
            || pixels.len() != expected
            || pixels.as_chunks::<4>().0.iter().any(|p| p[3] != 255)
        {
            return None;
        }
        let mut bands = Vec::new();
        let mut hashes = Vec::new();
        for band in pixels.chunks(stride * NATIVE_TILE_ROWS) {
            hashes.push(hash_bytes(band));
            bands.push(std::sync::Arc::new(band.to_vec()));
        }
        let revision = opaque_capture_revision(&hashes, expected);
        Some(Self {
            width,
            height,
            revision,
            bands,
            hashes,
        })
    }
    fn new_from_opaque_store(source: &Store, crop: Rect) -> Option<Self> {
        let width = usize::try_from(crop.width).ok()?;
        let height = usize::try_from(crop.height).ok()?;
        let stride = width.checked_mul(4)?;
        let mut bands = Vec::with_capacity(height.div_ceil(NATIVE_TILE_ROWS));
        let mut hashes = Vec::with_capacity(bands.capacity());
        for row0 in (0..height).step_by(NATIVE_TILE_ROWS) {
            let rows = (height - row0).min(NATIVE_TILE_ROWS);
            let mut data = Vec::with_capacity(stride * rows);
            for y in row0..row0 + rows {
                let offset = ((crop.y as usize + y) * source.width as usize + crop.x as usize) * 4;
                data.extend_from_slice(source.pixels.get(offset..offset + stride)?);
            }
            hashes.push(hash_bytes(&data));
            bands.push(std::sync::Arc::new(data));
        }
        let revision = opaque_capture_revision(&hashes, stride * height);
        Some(Self {
            width: crop.width as u32,
            height: crop.height as u32,
            bands,
            hashes,
            revision,
        })
    }
    fn patched_regions(&self, source: &Store, crop: Rect, regions: &[Rect]) -> Option<Self> {
        if regions.is_empty() {
            return None;
        }
        let mut next = self.clone();
        let width = usize::try_from(crop.width).ok()?;
        let stride = width.checked_mul(4)?;
        let mut touched = vec![false; next.bands.len()];
        for &dirty in regions {
            let start_y = usize::try_from(dirty.y - crop.y).ok()?;
            let end_y = start_y.checked_add(usize::try_from(dirty.height).ok()?)?;
            let start_x = usize::try_from(dirty.x - crop.x).ok()?;
            let columns = usize::try_from(dirty.width).ok()?;
            if end_y > crop.height as usize || start_x.checked_add(columns)? > width {
                return None;
            }
            // `band_index` addresses several parallel band arrays.
            #[allow(clippy::needless_range_loop)]
            for band_index in start_y / NATIVE_TILE_ROWS..=(end_y - 1) / NATIVE_TILE_ROWS {
                let band_start = band_index * NATIVE_TILE_ROWS;
                let from = start_y.max(band_start);
                let to = end_y.min(band_start + NATIVE_TILE_ROWS);
                for row in from..to {
                    let src_at = ((crop.y as usize + row) * source.width as usize
                        + crop.x as usize
                        + start_x)
                        .checked_mul(4)?;
                    let dst_at = (row - band_start)
                        .checked_mul(stride)?
                        .checked_add(start_x.checked_mul(4)?)?;
                    let count = columns.checked_mul(4)?;
                    let authored = source.pixels.get(src_at..src_at.checked_add(count)?)?;
                    let prior = next
                        .bands
                        .get(band_index)?
                        .get(dst_at..dst_at.checked_add(count)?)?;
                    if authored != prior {
                        std::sync::Arc::make_mut(next.bands.get_mut(band_index)?)
                            .get_mut(dst_at..dst_at + count)?
                            .copy_from_slice(authored);
                        touched[band_index] = true;
                    }
                }
            }
        }
        for (band, changed) in touched.into_iter().enumerate() {
            if changed {
                next.hashes[band] = hash_bytes(&next.bands[band]);
            }
        }
        next.revision = opaque_capture_revision(&next.hashes, next.len());
        Some(next)
    }
}

struct NativeTiledCache {
    layer: usize,
    store_instance: u64,
    view_generation: u64,
    ca_revision: u64,
    descendant_revision: u64,
    overlay_revision: u64,
    tiles: std::sync::Arc<NativePixelTiles>,
    // Kept across republishes of the same tiles: the GPU may not have staged
    // the publication that first carried it.
    damage: Option<NativeCpuDamage>,
    last_used: u64,
}
static NATIVE_TILED_CAPTURES: std::sync::LazyLock<Mutex<HashMap<CaptureKey, NativeTiledCache>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

// A single sequence across both representations makes a flat hit newer
// than a tiled miss (and vice versa), even when their cache locks differ.
static NATIVE_CAPTURE_CLOCK: AtomicU64 = AtomicU64::new(1);
fn next_native_capture_use() -> u64 {
    NATIVE_CAPTURE_CLOCK.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Copy)]
enum NativeCacheKind {
    Flat,
    Tiled,
}

// Enforce one 256 MiB AppKit cache budget across both representations. A
// published Arc outlives eviction, but only entries retained by these maps
// count toward the cache limit. Both publishers hold VIEW_STORES through
// insertion and pruning, preventing concurrent admissions between those steps.
fn trim_native_capture_caches(recent: (NativeCacheKind, CaptureKey)) {
    let mut flat = NATIVE_CAPTURES.lock().unwrap_or_else(|e| e.into_inner());
    let mut tiled = NATIVE_TILED_CAPTURES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    trim_native_capture_maps(&mut flat, &mut tiled, recent, NATIVE_CAPTURE_CACHE_LIMIT);
}

fn trim_native_capture_maps(
    flat: &mut HashMap<CaptureKey, NativeCapture>,
    tiled: &mut HashMap<CaptureKey, NativeTiledCache>,
    recent: (NativeCacheKind, CaptureKey),
    limit: usize,
) {
    let flat_bytes: usize = flat.values().map(|capture| capture.pixels.len()).sum();
    let tiled_bytes: usize = tiled
        .values()
        .map(|capture| capture.tiles.retained_bytes())
        .sum();
    let mut used = flat_bytes.saturating_add(tiled_bytes);
    if used <= limit {
        return;
    }
    // Keep genuinely hot entries, regardless of representation. A publication
    // can evict a colder cache record without releasing an Arc held by an
    // asynchronous GPU reader. Sort only when a limit is exceeded.
    let mut candidates = Vec::with_capacity(flat.len() + tiled.len());
    candidates.extend(flat.iter().filter_map(|(key, capture)| {
        (!matches!(recent, (NativeCacheKind::Flat, keep) if keep == *key)).then_some((
            capture.last_used,
            NativeCacheKind::Flat,
            *key,
        ))
    }));
    candidates.extend(tiled.iter().filter_map(|(key, capture)| {
        (!matches!(recent, (NativeCacheKind::Tiled, keep) if keep == *key)).then_some((
            capture.last_used,
            NativeCacheKind::Tiled,
            *key,
        ))
    }));
    candidates.sort_unstable_by_key(|(last_used, _, _)| *last_used);
    for (_, kind, key) in candidates {
        if used <= limit {
            break;
        }
        let freed = match kind {
            NativeCacheKind::Flat => flat.remove(&key).map(|capture| capture.pixels.len()),
            NativeCacheKind::Tiled => tiled
                .remove(&key)
                .map(|capture| capture.tiles.retained_bytes()),
        };
        used = used.saturating_sub(freed.unwrap_or(0));
    }
}

/// Whether a view can take the row-banded path at all. None of this depends
/// on the crop, so a publisher tests it once per view, not once per tile.
pub(crate) fn native_scene_tiled_candidate(view: Id, width: u32, height: u32) -> bool {
    let layer = crate::view::layer_of(view);
    let stores = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
    stores
        .as_ref()
        .and_then(|all| all.get(&(view as usize)))
        .is_some_and(|source| tiled_view_eligible(view, layer, source, width, height))
}

fn tiled_view_eligible(view: Id, layer: Id, source: &Store, width: u32, height: u32) -> bool {
    source.width == width
        && source.height == height
        && !source.known_translucent()
        && !crate::coreanimation::has_tree(layer)
        && !crate::coreanimation::native_scene_content_animating(layer, darwin::mach::media_time())
        // Both CPU and DMA-BUF overlays require the established blended
        // contiguous path; banded sources have no mixed-DMA stage/fallback.
        && !OVERLAYS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|(id, _)| *id == view as usize)
}

/// Publication path for large plain opaque NSView backing stores. The scene
/// owns a row-banded value snapshot, never a borrowed mutable Store pointer.
/// Any uncertain damage or alpha proof falls back to the existing full capture.
pub(crate) fn native_scene_tiled_pixels_cropped_with_damage(
    view: Id,
    width: u32,
    height: u32,
    crop: Rect,
) -> Option<(
    std::sync::Arc<NativePixelTiles>,
    u64,
    Option<NativeCpuDamage>,
)> {
    if crop.x < 0
        || crop.y < 0
        || crop.is_empty()
        || crop.x.checked_add(crop.width)? > width as i64
        || crop.y.checked_add(crop.height)? > height as i64
        || !native_scene_extent_supported(crop.width as u32, crop.height as u32)
        || (crop.width as u64)
            .checked_mul(crop.height as u64)?
            .checked_mul(4)?
            < MIN_TILED_CAPTURE_BYTES
    {
        return None;
    }
    let layer = crate::view::layer_of(view);
    let stores = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
    let source = stores.as_ref()?.get(&(view as usize))?;
    if !tiled_view_eligible(view, layer, source, width, height) {
        return None;
    }
    let overlay_revision = OVERLAY_REVISIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(view as usize))
        .copied()
        .unwrap_or(0);
    let ca_revision = crate::coreanimation::native_scene_content_revision(layer);
    let descendant_revision = crate::coreanimation::native_scene_descendant_revision(layer);
    let key: CaptureKey = (
        view as usize,
        crop.x,
        crop.y,
        crop.width as u32,
        crop.height as u32,
        width,
        height,
        false,
        0,
    );
    let mut cache = NATIVE_TILED_CAPTURES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(old) = cache.get_mut(&key)
        && old.layer == layer as usize
        && old.store_instance == source.instance
        && old.ca_revision == ca_revision
        && old.descendant_revision == descendant_revision
        && old.overlay_revision == overlay_revision
    {
        old.last_used = next_native_capture_use();
        if old.view_generation == source.generation() {
            return Some((
                std::sync::Arc::clone(&old.tiles),
                old.tiles.revision,
                old.damage,
            ));
        }
        if let Some(all) = source.damage_regions_since(old.view_generation) {
            let regions = all
                .into_iter()
                .map(|r| r.intersection(&crop))
                .filter(|r| !r.is_empty())
                .collect::<Vec<_>>();
            if regions.is_empty() {
                old.view_generation = source.generation();
                return Some((
                    std::sync::Arc::clone(&old.tiles),
                    old.tiles.revision,
                    old.damage,
                ));
            }
            // All previously accepted unchanged pixels were verified
            // opaque; newly authored regions alone need an alpha check.
            let opaque = regions.iter().all(|r| {
                (r.y..r.y + r.height).all(|row| {
                    let offset = ((row as usize) * width as usize + r.x as usize) * 4;
                    source.pixels[offset..offset + r.width as usize * 4]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .all(|pixel| pixel[3] == 255)
                })
            });
            if !opaque {
                source.found_translucent();
                cache.remove(&key);
                return None;
            }
            let next = old.tiles.patched_regions(source, crop, &regions)?;
            let prior = old.tiles.revision;
            let revision = next.revision;
            // Preserve exact dirty rectangles for CPU patch work,
            // keep the old single-box GPU ABI conservative for now.
            let union = regions.iter().copied().reduce(|a, b| a.union(&b))?;
            if prior != revision {
                old.damage = Some(NativeCpuDamage {
                    from_revision: prior,
                    rect: Rect {
                        x: union.x - crop.x,
                        y: union.y - crop.y,
                        width: union.width,
                        height: union.height,
                    },
                });
            }
            old.view_generation = source.generation();
            old.tiles = std::sync::Arc::new(next);
            return Some((std::sync::Arc::clone(&old.tiles), revision, old.damage));
        }
    }
    // Cold capture and unknown mutation history require full alpha proof.
    if !source.fully_opaque() {
        cache.remove(&key);
        return None;
    }
    let tiles = std::sync::Arc::new(NativePixelTiles::new_from_opaque_store(source, crop)?);
    let revision = tiles.revision;
    cache.insert(
        key,
        NativeTiledCache {
            layer: layer as usize,
            store_instance: source.instance,
            view_generation: source.generation(),
            ca_revision,
            descendant_revision,
            overlay_revision,
            tiles: std::sync::Arc::clone(&tiles),
            damage: None,
            last_used: next_native_capture_use(),
        },
    );
    drop(cache);
    trim_native_capture_caches((NativeCacheKind::Tiled, key));
    Some((tiles, revision, None))
}

// Flat captures hash the same row bands as NativePixelTiles, so identical
// pixels keep one revision when a view moves between the two paths.
fn opaque_capture_hashes(pixels: &[u8], stride: usize) -> Vec<u64> {
    pixels
        .chunks(stride * NATIVE_TILE_ROWS)
        .map(hash_bytes)
        .collect()
}

// Every flat and tiled chunk hash must agree, or a revision computed one way
// never matches a predecessor computed the other.
fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    hash.finish()
}

fn opaque_capture_revision(hashes: &[u64], bytes: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    hashes.hash(&mut hash);
    hash.finish()
}

/// An incremental capture is admitted only from a validated plain opaque
/// image and a contiguous producer-authored damage journal. The patch edits
/// only current changed rows; Arc::make_mut preserves every GPU-held reader.
fn update_plain_opaque_capture(
    key: CaptureKey,
    source: &Store,
    layer: usize,
    crop: Rect,
    overlay_generation: u64,
    ca_content_revision: u64,
    ca_descendant_revision: u64,
    excluded_view_layers: u64,
) -> Option<(std::sync::Arc<Vec<u8>>, u64)> {
    let mut captures = NATIVE_CAPTURES.lock().unwrap_or_else(|e| e.into_inner());
    let prior = captures.get_mut(&key)?;
    if !prior.plain_store
        || prior.store_instance != Some(source.instance)
        || prior.layer != layer
        || prior.overlay_generation != overlay_generation
        || prior.ca_content_revision != ca_content_revision
        || prior.ca_descendant_revision != ca_descendant_revision
        || prior.excluded_view_layers != excluded_view_layers
        || prior.pixels.is_empty()
    {
        return None;
    }
    let from = prior.view_generation?;
    let region = source.damage_since(from)?.intersection(&crop);
    if region.is_empty() {
        return None;
    }
    let x = usize::try_from(region.x).ok()?;
    let y = usize::try_from(region.y).ok()?;
    let dx = usize::try_from(region.x - crop.x).ok()?;
    let dy = usize::try_from(region.y - crop.y).ok()?;
    let w = usize::try_from(region.width).ok()?;
    let h = usize::try_from(region.height).ok()?;
    let width = source.width as usize;
    let crop_width = usize::try_from(crop.width).ok()?;
    let row_bytes = w.checked_mul(4)?;
    let length = crop_width
        .checked_mul(usize::try_from(crop.height).ok()?)?
        .checked_mul(4)?;
    // Validate *all* bounds before mutating a cached Arc. An invalid
    // producer must not leave the cache holding half-applied pixel patches.
    if source.pixels.len() != width.checked_mul(source.height as usize)?.checked_mul(4)?
        || x.checked_add(w)? > width
        || y.checked_add(h)? > source.height as usize
        || dx.checked_add(w)? > crop_width
        || dy.checked_add(h)? > crop.height as usize
    {
        return None;
    }
    let band_bytes = crop_width.checked_mul(4)?.checked_mul(NATIVE_TILE_ROWS)?;
    if prior.pixels.len() != length
        || prior.opaque_hashes.as_ref()?.len() != length.div_ceil(band_bytes)
    {
        return None;
    }
    // Producer mutation was explicitly recorded; old texels outside the
    // rectangle were opaque, so only the new texels inside it need checking.
    for row in y..y + h {
        let start = row.checked_mul(width)?.checked_add(x)?.checked_mul(4)?;
        let bytes = source.pixels.get(start..start + row_bytes)?;
        if bytes.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 255) {
            source.found_translucent();
            return None; // Re-render through the general alpha-aware path.
        }
    }
    // Marking identical data dirty must not clone the old allocation or
    // change the content identity consumed by the Metal scene publisher.
    if (0..h).all(|row| {
        let src_at = ((y + row) * width + x) * 4;
        let dst_at = ((dy + row) * crop_width + dx) * 4;
        source.pixels[src_at..src_at + row_bytes] == prior.pixels[dst_at..dst_at + row_bytes]
    }) {
        prior.view_generation = Some(source.generation());
        prior.last_used = next_native_capture_use();
        return Some((std::sync::Arc::clone(&prior.pixels), prior.revision));
    }
    // Arc::make_mut copies the whole frame while a GPU reader holds it. Take
    // the entry out so that copy never runs under the shared cache lock; the
    // caller's VIEW_STORES guard keeps any other publisher from racing it.
    let mut entry = captures.remove(&key)?;
    drop(captures);
    let old_revision = entry.revision;
    let pixels = std::sync::Arc::make_mut(&mut entry.pixels);
    let mut touched = vec![false; length.div_ceil(band_bytes)];
    for row in 0..h {
        let src_at = ((y + row) * width + x) * 4;
        let dst_at = ((dy + row) * crop_width + dx) * 4;
        pixels[dst_at..dst_at + row_bytes]
            .copy_from_slice(&source.pixels[src_at..src_at + row_bytes]);
        touched[(dy + row) / NATIVE_TILE_ROWS] = true;
    }
    let hashes = entry.opaque_hashes.as_mut()?;
    for (index, changed) in touched.into_iter().enumerate() {
        if changed {
            let offset = index * band_bytes;
            let end = pixels.len().min(offset + band_bytes);
            hashes[index] = hash_bytes(&pixels[offset..end]);
        }
    }
    entry.revision = opaque_capture_revision(hashes, length);
    entry.view_generation = Some(source.generation());
    entry.last_used = next_native_capture_use();
    if old_revision != entry.revision {
        entry.damage = Some(NativeCpuDamage {
            from_revision: old_revision,
            rect: Rect {
                x: region.x - crop.x,
                y: region.y - crop.y,
                width: region.width,
                height: region.height,
            },
        });
    }
    let snapshot = (std::sync::Arc::clone(&entry.pixels), entry.revision);
    NATIVE_CAPTURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, entry);
    #[cfg(test)]
    {
        NATIVE_RENDER_COUNT.with(|count| count.set(count.get() + 1));
        NATIVE_INCREMENTAL_PATCHES.with(|count| count.set(count.get() + 1));
    }
    Some(snapshot)
}

const NATIVE_CAPTURE_CACHE_LIMIT: usize = 256 * 1024 * 1024;
static NATIVE_CAPTURES: std::sync::LazyLock<Mutex<HashMap<CaptureKey, NativeCapture>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
#[cfg(test)]
thread_local! {
    // Process-global test counters make unrelated parallel view captures
    // indistinguishable from the exact operation under test.
    static NATIVE_RENDER_COUNT: Cell<usize> = const { Cell::new(0) };
    static NATIVE_INCREMENTAL_PATCHES: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn native_render_count_for_tests() -> usize {
    NATIVE_RENDER_COUNT.with(Cell::get)
}

/// What a window's composite is waiting on: the part of it views have drawn
/// over since it was last composed, and where each view was then, so that a
/// view which moved, hid or went away is recomposed without drawing.
#[derive(Default)]
struct WindowState {
    damage: Rect,
    /// Each view's `(view, clip, rect)`: the rectangle too, since a scrolled
    /// document moves under a clip that stays where it is.
    placed: Vec<(usize, Rect, Rect)>,
    holes: Vec<Hole>,
    /// For each layer with an overlay stacked above it, the overlays'
    /// rectangles, which its own presents must leave to the store.
    covered: Vec<(usize, Vec<Rect>)>,
    /// The generation of `crate::decor`'s chrome last copied in, zero for
    /// none.
    chrome: u64,
}

static WINDOWS: Mutex<Option<HashMap<usize, WindowState>>> = Mutex::new(None);

/// Views sent `-setNeedsDisplay:`, by address, drawn at the next pump.
static NEEDS_DISPLAY: Mutex<Vec<usize>> = Mutex::new(Vec::new());
// A partial request is authoritative only while no full-display request is
// pending. All explicit geometry is in guest NSView point coordinates.
static NEEDS_DISPLAY_RECTS: std::sync::LazyLock<Mutex<HashMap<usize, NSRect>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// The batches [`pump`] is drawing, innermost last, so a view freed by another
/// view's `-drawRect:` is taken out of them too. A stack because a `-drawRect:`
/// can run a nested event loop that pumps again. Zero marks a forgotten entry.
static DRAWING: Mutex<Vec<Vec<usize>>> = Mutex::new(Vec::new());

/// Distinguish an unavailable GPU presenter from a busy window. The latter
/// must not force a CPU fallback over content still being presented through
/// Vulkan: retain the backing store's dirty state for the next event pump.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentOutcome {
    Displayed,
    /// An earlier copy of the store reached the screen through the GPU, and
    /// this one is still on its way there: keep the store dirty.
    DisplayedEarlier,
    Fallback,
    Deferred,
}

pub type Presenter = fn(sdl3::Window, &Store, &[Hole], &[GpuOverlay]) -> PresentOutcome;

static PRESENTER: Mutex<Option<Presenter>> = Mutex::new(None);

/// A picture on the GPU, shared as a Linux dma-buf: premultiplied, top row
/// first.
pub struct Dmabuf {
    pub width: u32,
    pub height: u32,
    /// The DRM fourcc and format modifier.
    pub fourcc: u32,
    pub modifier: u64,
    pub planes: Vec<DmabufPlane>,
}

pub struct DmabufPlane {
    pub fd: std::os::fd::OwnedFd,
    pub offset: u32,
    pub stride: u32,
}

/// An overlay the presenter draws from a dma-buf, over `rect` of the store
/// in window pixels, source-over.
pub struct GpuOverlay {
    pub picture: std::sync::Arc<Dmabuf>,
    pub rect: Rect,
    /// Presenting NSView identity, not the allocation shared by its content.
    pub view: usize,
}

/// The GPU a presenter shows windows with, and what it can show from a
/// dma-buf without a copy.
#[derive(Clone, Debug)]
pub struct Importer {
    /// The GPU's DRM render node, as `st_rdev`.
    pub device: u64,
    /// DRM fourcc and modifier pairs it can sample.
    pub formats: Vec<(u32, u64)>,
}

/// Asks the presenter for its [`Importer`].
pub type ImporterQuery = fn() -> Option<Importer>;

static IMPORTER: Mutex<Option<ImporterQuery>> = Mutex::new(None);

/// Let the presenter say it can draw [`GpuOverlay`]s. `query` is asked
/// only when someone wants to know, so a GPU is not opened for nothing.
pub fn set_importer(query: ImporterQuery) {
    *IMPORTER.lock().unwrap_or_else(|e| e.into_inner()) = Some(query);
}

/// The presenter's GPU, if it draws dma-bufs at all.
pub fn importer() -> Option<Importer> {
    let query = (*IMPORTER.lock().unwrap_or_else(|e| e.into_inner()))?;
    query()
}

/// Windows whose last present went through the presenter, and so would draw
/// a dma-buf overlay; and those whose presenter could not import one.
static GPU_WINDOWS: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());

/// Whether an overlay on `sdl`'s window can be a dma-buf: the presenter
/// showed its last frame and has not failed to import one there. Otherwise
/// it has to be pixels, which every present can draw.
pub fn shows_dmabufs(sdl: sdl3::Window) -> bool {
    GPU_WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|&(window, refused)| window == sdl as usize && !refused)
}

/// The presenter could not draw a dma-buf overlay on `sdl`'s window; from
/// now on it is given pixels there.
/// Whether an actual import failed. Unlike shows_dmabufs(), false before
/// the first GPU present is not itself a permanent failure.
pub fn dmabuf_import_refused(sdl: sdl3::Window) -> bool {
    GPU_WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|&(window, refused)| window == sdl as usize && refused)
}

/// A rejected DMA image does not disable unrelated producers in the window.
/// Weak identity also prevents a later replacement at the same view address
/// from inheriting a previous producer's incompatibility.
struct RefusedDmabufProducer {
    window: usize,
    view: usize,
    picture: std::sync::Weak<Dmabuf>,
}

static REFUSED_DMABUF_PRODUCERS: Mutex<Vec<RefusedDmabufProducer>> = Mutex::new(Vec::new());

pub fn dmabuf_producer_refused(
    sdl: sdl3::Window,
    view: usize,
    picture: &std::sync::Arc<Dmabuf>,
) -> bool {
    let mut refused = REFUSED_DMABUF_PRODUCERS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    refused.retain(|entry| entry.picture.strong_count() > 0);
    refused.iter().any(|entry| {
        entry.window == sdl as usize
            && entry.view == view
            && entry.picture.ptr_eq(&std::sync::Arc::downgrade(picture))
    })
}

pub fn refuse_dmabuf_producer(sdl: sdl3::Window, view: usize, picture: &std::sync::Arc<Dmabuf>) {
    const MAX_REFUSED_PRODUCERS: usize = 256;
    let mut refused = REFUSED_DMABUF_PRODUCERS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    refused.retain(|entry| entry.picture.strong_count() > 0);
    if refused.iter().any(|entry| {
        entry.window == sdl as usize
            && entry.view == view
            && entry.picture.ptr_eq(&std::sync::Arc::downgrade(picture))
    }) {
        return;
    }
    if refused.len() == MAX_REFUSED_PRODUCERS {
        refused.remove(0);
    }
    refused.push(RefusedDmabufProducer {
        window: sdl as usize,
        view,
        picture: std::sync::Arc::downgrade(picture),
    });
}

/// WebKit's next CPU-frame request is restricted to its failed current image.
/// Other view producers remain eligible for DRM/DMA negotiation.
pub fn shows_dmabufs_for_view(sdl: sdl3::Window, view: usize) -> bool {
    if !shows_dmabufs(sdl) {
        return false;
    }
    let picture = OVERLAYS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(owner, _)| *owner == view)
        .and_then(|(_, overlay)| match &overlay.picture {
            Picture::Dmabuf(picture) => Some(std::sync::Arc::clone(picture)),
            Picture::Pixels(_) => None,
        });
    picture.is_none_or(|picture| !dmabuf_producer_refused(sdl, view, &picture))
}

pub fn refuse_dmabufs(sdl: sdl3::Window) {
    let mut windows = GPU_WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    match windows
        .iter_mut()
        .find(|(window, _)| *window == sdl as usize)
    {
        Some(entry) => entry.1 = true,
        None => windows.push((sdl as usize, true)),
    }
}

fn presented_through_gpu(sdl: sdl3::Window, gpu: bool) {
    let mut windows = GPU_WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    let at = windows
        .iter()
        .position(|(window, _)| *window == sdl as usize);
    match (at, gpu) {
        (None, true) => windows.push((sdl as usize, false)),
        (Some(at), false) if !windows[at].1 => {
            windows.remove(at);
        }
        _ => {}
    }
}

/// Let a GPU backend present stores, so that a window it also presents layers
/// to has one owner of its surface.
pub fn set_presenter(presenter: Presenter) {
    *PRESENTER.lock().unwrap_or_else(|e| e.into_inner()) = Some(presenter);
}

#[derive(Clone, Copy)]
struct Focus {
    view: Id,
    window: sdl3::Window,
    /// The part of the view its store covers - the part on screen - in
    /// window pixels: its top-left corner and its size.
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    /// Where that part starts in the view, in pixels from its top left, and
    /// the whole view's height in pixels, which an unflipped view counts
    /// up from.
    left: f64,
    top: f64,
    view_height: f64,
    /// Window pixels per point.
    density: f64,
    flipped: bool,
    /// Drawing into [`OFFSCREEN`]'s store keyed by `view`, an image being
    /// rendered, rather than into a view in a window.
    offscreen: bool,
    // `drawRect:`'s requested local top-left pixel region, if old pixels
    // outside it remain valid and the producer has supplied exact damage.
    clip: Option<Rect>,
}

thread_local! {
    static FOCUS: RefCell<Vec<Focus>> = const { RefCell::new(Vec::new()) };
}

/// Images being rendered by [`begin_offscreen`], keyed by the caller's key.
static OFFSCREEN: Mutex<Option<HashMap<usize, Store>>> = Mutex::new(None);

/// Send drawing to a transparent `width` by `height` pixel image, at
/// `density` pixels per point, until [`end_offscreen`] with the same key.
pub(crate) fn begin_offscreen(key: usize, width: u32, height: u32, density: f64, flipped: bool) {
    OFFSCREEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(key, Store::new(width.max(1), height.max(1)));
    FOCUS.with(|stack| {
        stack.borrow_mut().push(Focus {
            view: key as Id,
            window: std::ptr::null_mut(),
            x: 0.0,
            y: 0.0,
            width: f64::from(width.max(1)),
            height: f64::from(height.max(1)),
            left: 0.0,
            top: 0.0,
            view_height: f64::from(height.max(1)),
            density: if density > 0.0 { density } else { 1.0 },
            flipped,
            offscreen: true,
            clip: None,
        })
    });
}

/// The premultiplied B, G, R, A pixels, top row first, drawn since
/// [`begin_offscreen`], and that focus undone.
pub(crate) fn end_offscreen(key: usize) -> Option<Store> {
    FOCUS.with(|stack| {
        let mut stack = stack.borrow_mut();
        if let Some(at) = stack
            .iter()
            .rposition(|focus| focus.offscreen && focus.view as usize == key)
        {
            stack.truncate(at);
        }
    });
    release_context_override_above(focus_depth());
    OFFSCREEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()?
        .remove(&key)
}

/// A copy of the premultiplied B, G, R, A pixels drawn so far into the image
/// [`begin_offscreen`] started under `key`, as `(width, height, pixels)`.
pub(crate) fn offscreen_snapshot(key: usize) -> Option<(u32, u32, Vec<u8>)> {
    let stores = OFFSCREEN.lock().unwrap_or_else(|e| e.into_inner());
    let store = stores.as_ref()?.get(&key)?;
    Some((store.width, store.height, store.pixels.clone()))
}

fn focus_on(view: Id) -> Option<Focus> {
    // SAFETY: null or an object this runtime allocated.
    let frame = unsafe { crate::view::presentation_frame(view) }?;
    let (points, _) = sdl3::video::window_size(frame.window)?;
    let density = match points > 0 {
        true => f64::from(frame.window_width) / f64::from(points),
        false => 1.0,
    };
    // SAFETY: as above, and `frame` is the view's.
    let visible = unsafe { crate::view::presentation_visible(view, &frame) };
    Some(Focus {
        view,
        window: frame.window,
        x: f64::from(visible.x),
        y: f64::from(visible.y),
        width: f64::from(visible.w),
        height: f64::from(visible.h),
        left: f64::from(visible.x - frame.x),
        top: f64::from(visible.y - frame.y),
        view_height: f64::from(frame.height),
        density: if density > 0.0 { density } else { 1.0 },
        flipped: crate::view::is_flipped(view),
        offscreen: false,
        clip: None,
    })
}

fn push_focus(view: Id) -> bool {
    let Some(focus) = focus_on(view) else {
        return false;
    };
    FOCUS.with(|stack| stack.borrow_mut().push(focus));
    true
}

fn pop_focus() {
    let depth = FOCUS.with(|stack| {
        let mut stack = stack.borrow_mut();
        stack.pop();
        stack.len()
    });
    release_context_override_above(depth);
}

fn current_focus() -> Option<Focus> {
    FOCUS.with(|stack| stack.borrow().last().copied())
}

impl Focus {
    /// The store's top-left pixel in the view.
    fn origin(&self) -> (i64, i64) {
        (self.left as i64, self.top as i64)
    }

    /// The rectangle of the view, in its own points, that `area` - a
    /// rectangle of the store's pixels - shows.
    fn points_of(&self, area: Rect) -> NSRect {
        let top = area.y as f64 + self.top;
        let y = match self.flipped {
            true => top,
            false => self.view_height - (top + area.height as f64),
        };
        NSRect {
            origin: NSPoint {
                x: (area.x as f64 + self.left) / self.density,
                y: y / self.density,
            },
            size: NSSize {
                width: area.width as f64 / self.density,
                height: area.height as f64 / self.density,
            },
        }
    }

    /// What `-drawRect:` is asked to draw: the requested region, or all of
    /// the part on screen.
    fn drawn_rect(&self) -> NSRect {
        self.points_of(self.clip.unwrap_or(Rect {
            x: 0,
            y: 0,
            width: self.width as i64,
            height: self.height as i64,
        }))
    }
}

/// The focused view's store in `stores`, made or resized to the part of the
/// view on screen. A store whose part moved holds pixels of somewhere else,
/// so it starts over.
fn focused_store<'a>(stores: &'a mut HashMap<usize, Store>, focus: &Focus) -> &'a mut Store {
    let (width, height) = (focus.width.max(1.0) as u32, focus.height.max(1.0) as u32);
    let origin = focus.origin();
    let store = stores
        .entry(focus.view as usize)
        .or_insert_with(|| Store::new(width, height));
    if store.origin != origin {
        *store = Store::new(width, height);
        store.origin = origin;
        store.touch();
    }
    store.resize(width, height);
    store
}

/// What vector drawing needs to know about the focused view: where its pixels
/// are, and which focus it is.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Device {
    /// The view (or offscreen key) and how deep in the focus stack it is,
    /// which together tell one `-drawRect:` from the next.
    pub key: usize,
    pub depth: usize,
    /// The store's size: the part of the view on screen.
    pub width: u32,
    pub height: u32,
    /// Where that part starts in the view, and the view's whole height, in
    /// pixels.
    pub left: f64,
    pub top: f64,
    pub view_height: f64,
    pub density: f64,
    pub flipped: bool,
}

impl Device {
    /// A point in the view's coordinates, in pixels from the store's top left.
    pub fn to_pixels(self, x: f64, y: f64) -> [f64; 2] {
        let y = match self.flipped {
            true => y * self.density,
            false => self.view_height - y * self.density,
        };
        [x * self.density - self.left, y - self.top]
    }
}

pub(crate) fn focused_device() -> Option<Device> {
    FOCUS.with(|stack| {
        let stack = stack.borrow();
        let focus = stack.last()?;
        Some(Device {
            key: focus.view as usize,
            depth: stack.len(),
            width: focus.width.max(1.0) as u32,
            height: focus.height.max(1.0) as u32,
            left: focus.left,
            top: focus.top,
            view_height: focus.view_height,
            density: focus.density,
            flipped: focus.flipped,
        })
    })
}

/// Blend straight-alpha `color` over the focused view through `coverage`, a
/// `width`-wide mask of the view's pixels whose top-left is `(left, top)`.
pub(crate) fn paint_coverage(left: i64, top: i64, width: usize, coverage: &[f32], color: [f64; 4]) {
    let Some(focus) = current_focus() else {
        return;
    };
    if width == 0 || coverage.is_empty() {
        return;
    }
    let [red, green, blue, alpha] = color.map(|c| c.clamp(0.0, 1.0));
    let drawn = {
        let stores = match focus.offscreen {
            true => &OFFSCREEN,
            false => &VIEW_STORES,
        };
        let mut stores = stores.lock().unwrap_or_else(|e| e.into_inner());
        let store = focused_store(stores.get_or_insert_with(HashMap::new), &focus);
        let rows = coverage.len() / width;
        let clip = focus.clip.unwrap_or(Rect {
            x: 0,
            y: 0,
            width: i64::from(store.width),
            height: i64::from(store.height),
        });
        let x0 = left.max(0).max(clip.x);
        let y0 = top.max(0).max(clip.y);
        let x1 = (left + width as i64)
            .min(i64::from(store.width))
            .min(clip.x + clip.width);
        let y1 = (top + rows as i64)
            .min(i64::from(store.height))
            .min(clip.y + clip.height);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for y in y0..y1 {
            let mask = &coverage[(y - top) as usize * width..][..width];
            let base = y as usize * store.width as usize * 4;
            for x in x0..x1 {
                let a = f64::from(mask[(x - left) as usize].clamp(0.0, 1.0)) * alpha;
                if a <= 0.0 {
                    continue;
                }
                let pixel = &mut store.pixels[base + x as usize * 4..][..4];
                for (channel, source) in [blue, green, red, 1.0].into_iter().enumerate() {
                    let over = source * a + f64::from(pixel[channel]) / 255.0 * (1.0 - a);
                    pixel[channel] = (over * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        let changed = Rect {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        };
        store.touch_region(changed);
        changed
    };
    if focus.offscreen {
        return;
    }
    damage(
        focus.window,
        Rect {
            x: drawn.x + focus.x as i64,
            y: drawn.y + focus.y as i64,
            ..drawn
        },
    );
}

/// Pixels per point of what is being drawn into, 1 with nothing focused.
pub(crate) fn focus_density() -> f64 {
    current_focus().map_or(1.0, |focus| focus.density)
}

fn damage(window: sdl3::Window, rect: Rect) {
    if rect.is_empty() {
        return;
    }
    let mut windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    let state = windows
        .get_or_insert_with(HashMap::new)
        .entry(window as usize)
        .or_default();
    state.damage = state.damage.union(&rect);
}

/// `NSCompositingOperation` values this draws with; any other is drawn as
/// source-over.
const COMPOSITE_COPY: usize = 1;
/// Keeps the destination where the source is opaque: how a template image is
/// tinted, by filling with the colour and then drawing the image this way.
const COMPOSITE_DESTINATION_IN: usize = 7;

/// Draw premultiplied BGRA `source` pixels into the focused view.
///
/// `destination` is in the view's coordinates and `from` in the image's,
/// whose origin is its bottom-left corner. `points` is the image's size in
/// points, which scales `from` into the source's pixels.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_image(
    source: &[u8],
    source_width: usize,
    source_height: usize,
    points: NSSize,
    destination: NSRect,
    from: NSRect,
    operation: usize,
    fraction: f64,
    respect_flipped: bool,
) {
    let Some(focus) = current_focus() else {
        return;
    };
    let destination = crate::bezier::transform_rect(destination);
    let drawn = {
        let stores = match focus.offscreen {
            true => &OFFSCREEN,
            false => &VIEW_STORES,
        };
        let mut stores = stores.lock().unwrap_or_else(|e| e.into_inner());
        let store = focused_store(stores.get_or_insert_with(HashMap::new), &focus);
        draw_into(
            store,
            source,
            source_width,
            source_height,
            points,
            destination,
            from,
            operation,
            fraction,
            respect_flipped,
        )
    };
    if focus.offscreen {
        return;
    }
    damage(
        focus.window,
        Rect {
            x: drawn.x + focus.x as i64,
            y: drawn.y + focus.y as i64,
            ..drawn
        },
    );
}

/// The focused view's store as a canvas: `draw` gets its premultiplied B, G,
/// R, A pixels, top row first, with their width and height, and answers the
/// rectangle of them it changed. False with nothing focused.
pub(crate) fn draw_focused(draw: impl FnOnce(&mut [u8], u32, u32) -> Option<Rect>) -> bool {
    let Some(focus) = current_focus() else {
        return false;
    };
    let (width, height) = (focus.width.max(1.0) as u32, focus.height.max(1.0) as u32);
    let drawn = {
        let stores = match focus.offscreen {
            true => &OFFSCREEN,
            false => &VIEW_STORES,
        };
        let mut stores = stores.lock().unwrap_or_else(|e| e.into_inner());
        let store = focused_store(stores.get_or_insert_with(HashMap::new), &focus);
        let drawn = draw(&mut store.pixels, store.width, store.height);
        if let Some(area) = drawn.filter(|r| !r.is_empty()) {
            if focus.clip.is_some() {
                // Arbitrary canvas callbacks can alter pixels outside the
                // advertised dirty bounds; never certify narrow damage.
                store.touch();
            } else {
                store.touch_region(area);
            }
        }
        drawn
    };
    if let Some(drawn) = drawn
        && !focus.offscreen
    {
        let actual = if focus.clip.is_some() {
            Rect {
                x: 0,
                y: 0,
                width: width as i64,
                height: height as i64,
            }
        } else {
            drawn
        };
        damage(
            focus.window,
            Rect {
                x: actual.x + focus.x as i64,
                y: actual.y + focus.y as i64,
                ..actual
            },
        );
    }
    true
}

/// [`draw_image`], into the focused view's store; answers the rectangle of
/// it that changed.
#[allow(clippy::too_many_arguments)]
fn draw_into(
    store: &mut Store,
    source: &[u8],
    source_width: usize,
    source_height: usize,
    points: NSSize,
    destination: NSRect,
    from: NSRect,
    operation: usize,
    fraction: f64,
    respect_flipped: bool,
) -> Rect {
    let Some(focus) = current_focus() else {
        return Rect::default();
    };
    if source_width == 0
        || source_height == 0
        || fraction <= 0.0
        || source.len() < source_width * source_height * 4
    {
        return Rect::default();
    }
    let from = match from.size.width <= 0.0 || from.size.height <= 0.0 {
        true => NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: points,
        },
        false => from,
    };
    let scale_x = source_width as f64 / points.width.max(f64::MIN_POSITIVE);
    let scale_y = source_height as f64 / points.height.max(f64::MIN_POSITIVE);
    let source_left = from.origin.x * scale_x;
    let source_top = source_height as f64 - (from.origin.y + from.size.height) * scale_y;
    let source_w = from.size.width * scale_x;
    let source_h = from.size.height * scale_y;

    let view_height = focus.view_height / focus.density;
    let top = match focus.flipped {
        true => destination.origin.y,
        false => view_height - (destination.origin.y + destination.size.height),
    };
    let left = destination.origin.x * focus.density - focus.left;
    let top = top * focus.density - focus.top;
    let width = destination.size.width * focus.density;
    let height = destination.size.height * focus.density;
    if width <= 0.0 || height <= 0.0 {
        return Rect::default();
    }
    // A flipped context draws an image upside down unless asked not to.
    let mirrored = focus.flipped && !respect_flipped;
    let opacity = (fraction.min(1.0) * 256.0) as u32;

    let clip = focus.clip.unwrap_or(Rect {
        x: 0,
        y: 0,
        width: i64::from(store.width),
        height: i64::from(store.height),
    });
    let x0 = (left.max(0.0).round() as i64).max(clip.x);
    let x1 = ((left + width).min(f64::from(store.width)).round() as i64).min(clip.x + clip.width);
    let y0 = (top.max(0.0).round() as i64).max(clip.y);
    let y1 = ((top + height).min(f64::from(store.height)).round() as i64).min(clip.y + clip.height);
    if x0 >= x1 || y0 >= y1 {
        return Rect::default();
    }
    let columns: Vec<usize> = (x0..x1)
        .map(|x| {
            let u = (x as f64 + 0.5 - left) / width;
            ((source_left + u * source_w).floor().max(0.0) as usize).min(source_width - 1)
        })
        .collect();
    for y in y0..y1 {
        let mut v = (y as f64 + 0.5 - top) / height;
        if mirrored {
            v = 1.0 - v;
        }
        let row = ((source_top + v * source_h).floor().max(0.0) as usize).min(source_height - 1);
        let from_row = &source[row * source_width * 4..(row + 1) * source_width * 4];
        let base = y as usize * store.width as usize * 4;
        let to_row = &mut store.pixels[base + x0 as usize * 4..base + x1 as usize * 4];
        for (target, &column) in to_row.as_chunks_mut::<4>().0.iter_mut().zip(&columns) {
            let texel = &from_row[column * 4..column * 4 + 4];
            if operation == COMPOSITE_COPY && opacity >= 256 {
                target.copy_from_slice(texel);
                continue;
            }
            let alpha = texel[3] as u32 * opacity / 256;
            if operation == COMPOSITE_DESTINATION_IN {
                for channel in target.iter_mut() {
                    *channel = (u32::from(*channel) * alpha / 255) as u8;
                }
                continue;
            }
            for channel in 0..4 {
                let s = texel[channel] as u32 * opacity / 256;
                let d = target[channel] as u32;
                target[channel] = match operation == COMPOSITE_COPY {
                    true => s as u8,
                    false => (s + d * (255 - alpha) / 255).min(255) as u8,
                };
            }
        }
    }
    let changed = Rect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    };
    store.touch_region(changed);
    changed
}

/// Mark a view for `-drawRect:` at the next pump.
pub(crate) fn needs_display(view: Id) {
    let mut pending = NEEDS_DISPLAY.lock().unwrap_or_else(|e| e.into_inner());
    NEEDS_DISPLAY_RECTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(view as usize));
    if !pending.contains(&(view as usize)) {
        pending.push(view as usize);
    }
}

pub(crate) fn needs_display_in_rect(view: Id, rect: NSRect) {
    let values = [
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
    ];
    if !values.iter().all(|v| v.is_finite()) || rect.size.width < 0.0 || rect.size.height < 0.0 {
        needs_display(view);
        return;
    }
    if rect.size.width == 0.0 || rect.size.height == 0.0 {
        return;
    }
    let mut pending = NEEDS_DISPLAY.lock().unwrap_or_else(|e| e.into_inner());
    let mut areas = NEEDS_DISPLAY_RECTS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !pending.contains(&(view as usize)) {
        pending.push(view as usize);
        areas.insert(view as usize, rect);
    } else if let Some(previous) = areas.get_mut(&(view as usize)) {
        let x0 = previous.origin.x.min(rect.origin.x);
        let y0 = previous.origin.y.min(rect.origin.y);
        let x1 = (previous.origin.x + previous.size.width).max(rect.origin.x + rect.size.width);
        let y1 = (previous.origin.y + previous.size.height).max(rect.origin.y + rect.size.height);
        if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
            areas.remove(&(view as usize));
        } else {
            *previous = NSRect {
                origin: NSPoint { x: x0, y: y0 },
                size: NSSize {
                    width: x1 - x0,
                    height: y1 - y0,
                },
            };
        }
    }
}

fn focus_dirty_pixels(focus: Focus, requested: NSRect) -> Option<Rect> {
    let [x, y, w, h] = [
        requested.origin.x,
        requested.origin.y,
        requested.size.width,
        requested.size.height,
    ];
    let left = (x * focus.density - focus.left).floor();
    let right = ((x + w) * focus.density - focus.left).ceil();
    let top = if focus.flipped {
        (y * focus.density - focus.top).floor()
    } else {
        (focus.view_height - (y + h) * focus.density - focus.top).floor()
    };
    let bottom = if focus.flipped {
        ((y + h) * focus.density - focus.top).ceil()
    } else {
        (focus.view_height - y * focus.density - focus.top).ceil()
    };
    if ![left, right, top, bottom].iter().all(|v| v.is_finite()) {
        return None;
    }
    let width = focus.width.max(1.0) as i64;
    let height = focus.height.max(1.0) as i64;
    let x0 = (left as i64).clamp(0, width);
    let x1 = (right as i64).clamp(0, width);
    let y0 = (top as i64).clamp(0, height);
    let y1 = (bottom as i64).clamp(0, height);
    (x0 < x1 && y0 < y1).then_some(Rect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

/// Whether a view of `window` is waiting to be displayed.
pub(crate) fn window_needs_display(window: Id) -> bool {
    NEEDS_DISPLAY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        // SAFETY: a view recorded while live and forgotten on deallocation.
        .any(|view| unsafe { crate::view::window_of(*view as Id) } == window)
}

/// Mark a view and everything under it, as a window that has nothing valid
/// to show needs, and as a subtree moved into a window does: none of it has
/// been drawn there.
pub(crate) fn needs_display_in_tree(view: Id, depth: usize) {
    if view.is_null() || depth > 64 {
        return;
    }
    needs_display(view);
    // SAFETY: a view this crate made.
    for child in unsafe { crate::view::children(view) } {
        needs_display_in_tree(child, depth + 1);
    }
}

/// Forget a window whose SDL window is about to be destroyed.
///
/// The store is keyed by the SDL window's address and SDL reuses addresses, so
/// a store left behind would be handed to the next window made - at the size the
/// old one was.
pub fn forget_window(sdl: sdl3::Window) {
    GPU_ADMISSION_DAMAGE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(sdl as usize));
    GPU_ADMITTED_NATIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(sdl as usize));
    UNDERLAY_ANIMATING_WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(sdl as usize));
    GPU_UNDERLAYS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(sdl as usize));
    if let Some(stores) = STORES.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        stores.remove(&(sdl as usize));
    }
    GPU_WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(window, _)| *window != sdl as usize);
    REFUSED_DMABUF_PRODUCERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|entry| entry.window != sdl as usize);
    SURFACE_LAYERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(window, _)| *window != sdl as usize);
}

/// Layers that have written their frames into a window's SDL surface
/// themselves, as `(window, layer)`.
static SURFACE_LAYERS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

/// Record that `layer` has drawn its frame into `sdl`'s window surface, which
/// the store's own presents through that surface then leave alone.
///
/// A layer's first frame there can land before the window's layout settles,
/// over rows that are now the chrome's, so the store is presented again then.
pub fn layer_drew_on_surface(sdl: sdl3::Window, layer: usize) {
    let mut layers = SURFACE_LAYERS.lock().unwrap_or_else(|e| e.into_inner());
    if !layers.contains(&(sdl as usize, layer)) {
        layers.push((sdl as usize, layer));
        drop(layers);
        present_again(sdl);
    }
}

/// Forget a view that is being deallocated, so no pump draws freed memory.
pub(crate) fn forget(view: Id) {
    // A new view may reuse this address with an identical initial generation.
    // The old underlay is never safe across that lifetime transition.
    let key = view as usize;
    REFUSED_DMABUF_PRODUCERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|entry| entry.view != key);
    // An address may be recycled with generation zero. Evict only underlays
    // which actually read this NSView while it was alive; unrelated windows
    // retain their immutable backing images and cached upload identities.
    GPU_UNDERLAYS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|_, entry| !entry.views.contains(&key));
    NATIVE_CAPTURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(owner, _, _, _, _, _, _, _, _), _| *owner != key);
    NATIVE_TILED_CAPTURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(owner, _, _, _, _, _, _, _, _), _| *owner != key);
    crate::view::forget_native_tile_ids(view);
    NEEDS_DISPLAY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|v| *v != key);
    NEEDS_DISPLAY_RECTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
    ANIMATING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(_, v)| *v != key);
    for batch in DRAWING.lock().unwrap_or_else(|e| e.into_inner()).iter_mut() {
        for entry in batch.iter_mut().filter(|entry| **entry == key) {
            *entry = 0;
        }
    }
    if let Some(stores) = VIEW_STORES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        stores.remove(&key);
    }
}

/// Draw `view` now, as `-display` does.
///
/// A layer-backed view is displayed through its layer, as AppKit does it: a
/// layer whose delegate implements `-displayLayer:` is sent `-display`, which
/// hands the whole job to the delegate (Qt answers with an expose event and
/// then sets the layer's `contents`); a view that answers `-wantsUpdateLayer`
/// with YES is sent `-updateLayer`. Every other view has `-drawRect:` sent with
/// its focus locked, and what it draws is its layer's content.
pub(crate) fn display_view(view: Id) {
    // Explicit `-display` can run before a queued pump. The pixels it draws
    // satisfy that prior invalidation; a later region-only request must not
    // inherit an obsolete full-view pending entry.
    NEEDS_DISPLAY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|pending| *pending != view as usize);
    let dirty = NEEDS_DISPLAY_RECTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(view as usize));
    if crate::view::is_layer_backed(view) && focus_on(view).is_some() {
        let layer = crate::view::layer_of(view);
        let delegate = crate::layer::delegate_of(layer);
        // SAFETY: null or a live object, which the weak slot guarantees.
        let displays = !delegate.is_null()
            && unsafe { objc::object::class_of(delegate) }
                .is_some_and(|c| c.method(objc::sel::get("displayLayer:")).is_some());
        if displays {
            send(layer, "display");
            return;
        }
        // SAFETY: `-wantsUpdateLayer` is `BOOL (id, SEL)`.
        let updates = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
            f(view, objc::sel::get("wantsUpdateLayer"))
        } != 0;
        if updates {
            send(view, "updateLayer");
            return;
        }
    }
    let sel_draw = objc::sel::get("drawRect:");
    // SAFETY: a live object this runtime allocated.
    let responds =
        unsafe { objc::object::class_of(view) }.is_some_and(|c| c.method(sel_draw).is_some());
    if !responds || !push_focus(view) {
        return;
    }
    let on_screen = current_focus().is_some_and(|focus| focus.width >= 1.0 && focus.height >= 1.0);
    if !on_screen {
        // Nothing of it shows, so AppKit asks it to draw nothing. The empty
        // store records that, for `redisplay_moved` to notice it showing.
        if let Some(focus) = current_focus() {
            let mut store = Store::new(0, 0);
            store.origin = focus.origin();
            VIEW_STORES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert_with(HashMap::new)
                .insert(view as usize, store);
        }
        pop_focus();
        return;
    }
    FOCUS.with(|stack| {
        let mut stack = stack.borrow_mut();
        let focus = stack.last_mut().unwrap();
        if let Some(requested) = dirty {
            let has_base = VIEW_STORES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .and_then(|stores| stores.get(&(view as usize)))
                .is_some_and(|store| {
                    store.width == focus.width as u32
                        && store.height == focus.height as u32
                        && store.origin == focus.origin()
                });
            if has_base {
                focus.clip = focus_dirty_pixels(*focus, requested);
            }
        }
    });
    clear_focused_store();
    let bounds = current_focus().map_or(NSRect::default(), |focus| focus.drawn_rect());
    // SAFETY: `-drawRect:` is `void (id, SEL, NSRect)`.
    unsafe {
        let send: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
        send(view, sel_draw, bounds);
    }
    pop_focus();
}

/// Start a `-drawRect:` from a transparent store. What the view drew last time
/// is gone on a Mac too: the window behind a view that is not opaque is drawn
/// again first, so only this draw's pixels end up over it.
fn clear_focused_store() {
    let Some(focus) = current_focus() else {
        return;
    };
    let cleared = VIEW_STORES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .filter(|stores| stores.contains_key(&(focus.view as usize)))
        .map(|stores| focused_store(stores, &focus))
        .map(|store| {
            let area = focus.clip.unwrap_or(Rect {
                x: 0,
                y: 0,
                width: i64::from(store.width),
                height: i64::from(store.height),
            });
            if focus.clip.is_none() {
                store.pixels.fill(0);
                store.touch();
            } else {
                let row_bytes = area.width as usize * 4;
                for y in area.y..area.y + area.height {
                    let start = (y as usize * store.width as usize + area.x as usize) * 4;
                    store.pixels[start..start + row_bytes].fill(0);
                }
                store.touch_region(area);
            }
            Rect {
                x: area.x + focus.x as i64,
                y: area.y + focus.y as i64,
                ..area
            }
        });
    if let Some(area) = cleared {
        damage(focus.window, area);
    }
}

fn send(receiver: Id, name: &str) {
    // SAFETY: a `void (id, SEL)` method on a live object.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(name));
    }
}

/// Draw everything marked for display and present every window that changed.
pub fn pump() {
    for window in std::mem::take(
        &mut *GPU_ADMISSION_DAMAGE
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    ) {
        let sdl = window as sdl3::Window;
        if crate::window::window_for(sdl).is_null() {
            continue;
        }
        if let Some((width, height)) = sdl3::video::window_size_in_pixels(sdl) {
            damage(
                sdl,
                Rect {
                    x: 0,
                    y: 0,
                    width: i64::from(width.max(0)),
                    height: i64::from(height.max(0)),
                },
            );
        }
    }
    crate::decor::pump();
    crate::layout::prepare();
    for window in crate::window::all() {
        if !crate::window::is_shown(window) {
            continue;
        }
        // SAFETY: a window this crate made.
        let sdl = unsafe { crate::window::sdl_window(window) };
        let Some((width, height)) = sdl3::video::window_size_in_pixels(sdl) else {
            continue;
        };
        let current = STORES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|stores| stores.get(&(sdl as usize)))
            .map(|store| (store.width as i32, store.height as i32));
        // A window newly shown, or resized, has nothing valid in it: AppKit
        // redraws the whole window then.
        if current != Some((width.max(1), height.max(1))) {
            needs_display_in_tree(crate::window::content_view_of(window), 0);
        }
    }
    redisplay_moved();
    let pending = std::mem::take(&mut *NEEDS_DISPLAY.lock().unwrap_or_else(|e| e.into_inner()));
    let count = pending.len();
    let depth = {
        let mut drawing = DRAWING.lock().unwrap_or_else(|e| e.into_inner());
        drawing.push(pending);
        drawing.len() - 1
    };
    for at in 0..count {
        let view = DRAWING.lock().unwrap_or_else(|e| e.into_inner())[depth][at] as Id;
        if view.is_null() {
            continue;
        }
        // SAFETY: a view this crate recorded while it was live.
        let window = unsafe { crate::view::window_of(view) };
        if crate::window::is_shown(window) && !crate::view::is_hidden_or_has_hidden_ancestor(view) {
            display_view(view);
        }
    }
    DRAWING.lock().unwrap_or_else(|e| e.into_inner()).pop();
    damage_animating();
    for window in crate::window::all() {
        if crate::window::is_shown(window) {
            compose_window(window);
        }
    }
    present_dirty();
}

/// Mark for display each view whose store no longer holds the part of it on
/// screen: scrolling, or a move, showed another part, which it has not drawn.
fn redisplay_moved() {
    let held: Vec<(usize, (i64, i64), (u32, u32))> = VIEW_STORES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|stores| {
            stores
                .iter()
                .map(|(view, store)| (*view, store.origin, (store.width, store.height)))
                .collect()
        })
        .unwrap_or_default();
    for (view, origin, size) in held {
        let view = view as Id;
        // SAFETY: a view whose store this crate holds, so a live one.
        let window = unsafe { crate::view::window_of(view) };
        if !crate::window::is_shown(window) || crate::view::is_hidden_or_has_hidden_ancestor(view) {
            continue;
        }
        // SAFETY: as above.
        let Some(frame) = (unsafe { crate::view::presentation_frame(view) }) else {
            continue;
        };
        // SAFETY: as above, and `frame` is the view's.
        let visible = unsafe { crate::view::presentation_visible(view, &frame) };
        let now = (
            i64::from(visible.x - frame.x),
            i64::from(visible.y - frame.y),
        );
        if (now, (visible.w as u32, visible.h as u32)) != (origin, size) {
            needs_display(view);
        }
    }
}

/// Views whose layer trees were still animating when last composed, by window.
static ANIMATING: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

/// Recompose each animating view, at most once a display frame: the pump runs
/// far more often than that, and every recompose is a software fill.
fn damage_animating() {
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(16)) {
        return;
    }
    let views: Vec<usize> = ANIMATING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(_, view)| *view)
        .collect();
    if views.is_empty() {
        return;
    }
    *last = Some(std::time::Instant::now());
    drop(last);
    for view in views {
        damage_view(view as Id);
    }
}

/// Have the part of the window `view` covers recomposed at the next pump,
/// without asking the view to draw again: its layer tree changed, not it.
pub(crate) fn damage_view(view: Id) {
    // SAFETY: a view this crate made.
    let Some(frame) = (unsafe { crate::view::presentation_frame(view) }) else {
        return;
    };
    damage(
        frame.window,
        Rect {
            x: i64::from(frame.x),
            y: i64::from(frame.y),
            width: i64::from(frame.width),
            height: i64::from(frame.height),
        },
    );
}

/// One view's place in the composite.
struct Placed {
    view: Id,
    /// Where it is, in window pixels, and the part of that its ancestors let
    /// show.
    rect: Rect,
    clip: Rect,
    /// `-alphaValue` with its ancestors' folded in.
    alpha: f64,
}

/// The single rounded rectangle accepted by backing, software overlay,
/// OpenGL and Metal presentation. Avoid using point-density to reconstruct
/// widths: physical X/Y scales and half-point edge rounding can differ.
fn logical_pixel_bounds(sdl: sdl3::Window, whole: Rect) -> Rect {
    crate::window::presentation_clip(sdl)
        .map(|clip| Rect {
            x: i64::from(clip.x),
            y: i64::from(clip.y),
            width: i64::from(clip.w),
            height: i64::from(clip.h),
        })
        .unwrap_or(whole)
        .intersection(&whole)
}

/// The shown views of a window, back to front, placed.
fn place_tree(window: Id, sdl: sdl3::Window) -> Vec<Placed> {
    let mut out = Vec::new();
    let Some((points_high, pixels)) = sdl3::video::window_size(sdl)
        .zip(sdl3::video::window_size_in_pixels(sdl))
        .map(|((w, h), (pw, ph))| (h, (w, pw, ph)))
    else {
        return out;
    };
    let (points_wide, pixel_width, pixel_height) = pixels;
    let whole = Rect {
        x: 0,
        y: 0,
        width: i64::from(pixel_width),
        height: i64::from(pixel_height),
    };
    let content = crate::window::content_view_of(window);
    let Some(surface) = crate::window::surface_frame(window) else {
        return out;
    };
    // Use the exact edge rounding of every presentation backend. Rounding
    // an origin and a size independently can admit one pixel past the logical
    // right/bottom edge when the origin is fractional.
    let logical_clip = logical_pixel_bounds(sdl, whole);
    // The window's base coordinates: not flipped, origin at the bottom, and
    // taller than the content by the chrome above it.
    let base = NSRect {
        origin: NSPoint {
            x: surface.origin.x,
            y: f64::from(points_high) - surface.origin.y - surface.size.height,
        },
        size: NSSize {
            width: surface.size.width,
            height: (surface.size.height - crate::window::content_inset(window)).max(0.0),
        },
    };
    let geometry = Geometry {
        window_height: f64::from(points_high),
        host_points: (points_wide, points_high),
        host_pixels: (pixel_width, pixel_height),
    };
    place_view(content, base, false, logical_clip, geometry, 0, &mut out);
    out
}

/// Window pixels per point where `view` would draw.
#[cfg(test)]
pub(crate) fn focus_density_of_for_tests(view: Id) -> f64 {
    focus_on(view).map_or(1.0, |focus| focus.density)
}

/// Where a view's store starts in the view, and its size, in pixels.
#[cfg(test)]
pub(crate) fn view_store_geometry_for_tests(view: Id) -> Option<((i64, i64), (u32, u32))> {
    let stores = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
    let store = stores.as_ref()?.get(&(view as usize))?;
    Some((store.origin, (store.width, store.height)))
}

#[cfg(test)]
pub(crate) fn placed_rect_for_tests(window: Id, view: Id) -> Option<Rect> {
    let sdl = unsafe { crate::window::sdl_window(window) };
    place_tree(window, sdl)
        .into_iter()
        .find(|placed| placed.view == view)
        .map(|placed| placed.rect)
}

#[derive(Clone, Copy)]
struct Geometry {
    /// The window's height in points, which base coordinates count up from.
    window_height: f64,
    host_points: (i32, i32),
    host_pixels: (i32, i32),
}

/// Place `view`, whose superview's rectangle in window base coordinates is
/// `outer` and whose flippedness is `outer_flipped`, then its subviews.
fn place_view(
    view: Id,
    outer: NSRect,
    outer_flipped: bool,
    clip: Rect,
    geometry: Geometry,
    depth: usize,
    out: &mut Vec<Placed>,
) {
    if view.is_null() || depth > 64 || crate::view::is_hidden_itself(view) {
        return;
    }
    let frame = crate::view::frame_of(view);
    let y = match outer_flipped {
        true => outer.origin.y + outer.size.height - frame.origin.y - frame.size.height,
        false => outer.origin.y + frame.origin.y,
    };
    let in_window = NSRect {
        origin: NSPoint {
            x: outer.origin.x + frame.origin.x,
            y,
        },
        size: frame.size,
    };
    let pixels = crate::window::pixel_rect_from_surface(
        NSRect {
            origin: NSPoint {
                x: in_window.origin.x,
                y: geometry.window_height - (y + frame.size.height),
            },
            size: frame.size,
        },
        geometry.host_points,
        geometry.host_pixels,
    );
    let rect = Rect {
        x: i64::from(pixels.x),
        y: i64::from(pixels.y),
        width: i64::from(pixels.w),
        height: i64::from(pixels.h),
    };
    let clip = clip.intersection(&rect);
    let alpha = crate::view::effective_alpha(view);
    out.push(Placed {
        view,
        rect,
        clip,
        alpha,
    });
    let flipped = crate::view::is_flipped(view);
    // Subviews' frames are measured from the bounds origin, so a scrolled
    // clip view moves its document the other way.
    let shift = crate::view::bounds_origin_of(view);
    let inner = NSRect {
        origin: NSPoint {
            x: in_window.origin.x - shift.x,
            y: match flipped {
                true => in_window.origin.y + shift.y,
                false => in_window.origin.y - shift.y,
            },
        },
        size: in_window.size,
    };
    // SAFETY: a view this crate made.
    for child in unsafe { crate::view::children(view) } {
        place_view(child, inner, flipped, clip, geometry, depth + 1, out);
    }
}

/// The views placed above a Metal layer whose own drawing covers all of their
/// rectangle.
fn opaque_views(placed: &[Placed]) -> Vec<usize> {
    let Some(first) = placed
        .iter()
        .position(|p| crate::layer::is_metal(crate::view::layer_of(p.view)))
    else {
        return Vec::new();
    };
    let stores = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(stores) = stores.as_ref() else {
        return Vec::new();
    };
    placed[first + 1..]
        .iter()
        .filter(|p| {
            !p.clip.is_empty()
                && p.alpha >= 1.0
                && !crate::view::has_rounded_mask_ancestor(p.view)
                && crate::view::native_scene_ca_opacity(p.view, darwin::mach::media_time()) >= 1.0
        })
        .filter(|p| {
            stores.get(&(p.view as usize)).is_some_and(|store| {
                let held = Rect {
                    x: p.rect.x + store.origin.0,
                    y: p.rect.y + store.origin.1,
                    width: i64::from(store.width),
                    height: i64::from(store.height),
                };
                held.intersection(&p.clip) == p.clip && store.fully_opaque()
            })
        })
        .map(|p| p.view as usize)
        .collect()
}

/// Authoritative opaque native-view coverage, recomputed from live AppKit
/// stacking order on the video thread. Independent of whether a CPU window
/// backing image has *ever* been submitted to the Metal scene.
pub(crate) fn live_metal_occlusion(
    sdl: sdl3::Window,
    scene: &[crate::view::MetalSceneLayer],
) -> Vec<(usize, Vec<Rect>)> {
    let window = crate::window::window_for(sdl);
    if window.is_null() {
        return Vec::new();
    }
    let opaque = opaque_views(&place_tree(window, sdl));
    let native_id_bit = 1usize << (usize::BITS - 1);
    scene
        .iter()
        .enumerate()
        .filter(|(_, layer)| layer.layer & native_id_bit == 0)
        .map(|(index, layer)| {
            let metal = Rect {
                x: i64::from(layer.frame.x),
                y: i64::from(layer.frame.y),
                width: i64::from(layer.frame.width),
                height: i64::from(layer.frame.height),
            };
            let covered = scene[index + 1..]
                .iter()
                .filter(|native| {
                    native.layer & native_id_bit != 0
                        && opaque.contains(&native.native_owner)
                        && native.opacity >= 1.0
                        // Rounded clips are not fully covered by any rectangle.
                        && native.rounded.is_empty()
                })
                .map(|native| {
                    let image = Rect {
                        x: i64::from(native.frame.x),
                        y: i64::from(native.frame.y),
                        width: i64::from(native.frame.width),
                        height: i64::from(native.frame.height),
                    };
                    // The *same* clip projected by visit_metal_scene for the
                    // native GPU source, including intermediate rectangular CA
                    // masks. Never subtract outside its drawn geometry.
                    let clip = native.clip.map_or(image, |clip| {
                        image.intersection(&Rect {
                            x: i64::from(clip.x),
                            y: i64::from(clip.y),
                            width: i64::from(clip.width),
                            height: i64::from(clip.height),
                        })
                    });
                    clip.intersection(&metal)
                })
                .filter(|rect| !rect.is_empty())
                .collect();
            (layer.layer, covered)
        })
        .collect()
}

/// Render the native content of one NSView into a transparent, view-local
/// source. The Metal presenter retains its own copy; no Objective-C pointer
/// crosses onto the GPU worker. A CPU-owned view above Metal must be sampled
/// *after* that Metal source, not flattened into the window's bottom image.
pub fn native_scene_extent_supported(width: u32, height: u32) -> bool {
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|area| area.checked_mul(4))
        .is_some_and(|bytes| bytes > 0 && bytes <= 64 * 1024 * 1024)
}

/// DMA-BUF-only images do not allocate an RGBA copy on the AppKit thread.
/// Bound their GPU staging footprint separately from CPU-backed captures.
pub fn native_scene_dma_extent_supported(width: u32, height: u32) -> bool {
    width <= 16384
        && height <= 16384
        && u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|area| area.checked_mul(4))
            .is_some_and(|bytes| bytes > 0 && bytes <= 256 * 1024 * 1024)
}

#[cfg(test)]
pub(crate) fn native_scene_pixels(
    view: Id,
    width: u32,
    height: u32,
) -> Option<(std::sync::Arc<Vec<u8>>, u64)> {
    native_scene_pixels_cropped(
        view,
        width,
        height,
        Rect {
            x: 0,
            y: 0,
            width: i64::from(width),
            height: i64::from(height),
        },
    )
}

/// Rasterize only the visible window-pixel portion of a larger NSView. Keep
/// the full logical source extent for CA and overlay coordinate projection;
/// otherwise a clipped view's backing content gets scaled to its crop.
pub(crate) fn native_scene_pixels_cropped_with_damage(
    view: Id,
    width: u32,
    height: u32,
    crop: Rect,
) -> Option<(std::sync::Arc<Vec<u8>>, u64, Option<NativeCpuDamage>)> {
    let (pixels, revision) = native_scene_pixels_cropped(view, width, height, crop)?;
    let key = (
        view as usize,
        crop.x,
        crop.y,
        crop.width as u32,
        crop.height as u32,
        width,
        height,
        false,
        0,
    );
    let damage = NATIVE_CAPTURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .filter(|capture| {
            capture.revision == revision && std::sync::Arc::ptr_eq(&capture.pixels, &pixels)
        })
        .and_then(|capture| capture.damage);
    Some((pixels, revision, damage))
}

pub fn native_scene_pixels_cropped(
    view: Id,
    width: u32,
    height: u32,
    crop: Rect,
) -> Option<(std::sync::Arc<Vec<u8>>, u64)> {
    native_scene_pixels_impl(view, width, height, crop, false, &[], &[])
}

#[cfg(test)]
pub(crate) fn metal_sublayers_pixels_cropped(
    view: Id,
    width: u32,
    height: u32,
    crop: Rect,
) -> Option<(std::sync::Arc<Vec<u8>>, u64)> {
    native_scene_pixels_impl(view, width, height, crop, true, &[], &[])
}

pub(crate) fn metal_sublayers_segment_pixels_cropped(
    view: Id,
    width: u32,
    height: u32,
    crop: Rect,
    selected: &[usize],
    gpu_groups: &[usize],
) -> Option<(std::sync::Arc<Vec<u8>>, u64)> {
    native_scene_pixels_impl(view, width, height, crop, true, selected, gpu_groups)
}

fn native_scene_pixels_impl(
    view: Id,
    width: u32,
    height: u32,
    crop: Rect,
    metal_children: bool,
    selected: &[usize],
    gpu_groups: &[usize],
) -> Option<(std::sync::Arc<Vec<u8>>, u64)> {
    if (!metal_children
        && (crop.x < 0
            || crop.y < 0
            || crop.x.checked_add(crop.width)? > i64::from(width)
            || crop.y.checked_add(crop.height)? > i64::from(height)))
        || crop.is_empty()
        || !native_scene_extent_supported(crop.width as u32, crop.height as u32)
    {
        return None;
    }
    use std::hash::{Hash, Hasher};
    let selection_hash = if metal_children {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        selected.hash(&mut hash);
        gpu_groups.hash(&mut hash);
        hash.finish()
    } else {
        0
    };
    let key = (
        view as usize,
        crop.x,
        crop.y,
        crop.width as u32,
        crop.height as u32,
        width,
        height,
        metal_children,
        selection_hash,
    );
    let layer = crate::view::layer_of(view);
    let views = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
    let source = views.as_ref().and_then(|all| all.get(&(view as usize)));
    let overlays = OVERLAYS.lock().unwrap_or_else(|e| e.into_inner());
    let source = if metal_children { None } else { source };
    let overlay = if metal_children {
        None
    } else {
        overlays
            .iter()
            .find(|(id, entry)| *id == view as usize && matches!(entry.picture, Picture::Pixels(_)))
            .map(|(_, entry)| entry)
    };
    if metal_children
        && (!crate::layer::is_metal(layer)
            || !crate::coreanimation::has_renderable_sublayers(layer))
        || (!metal_children
            && source.is_none()
            && overlay.is_none()
            && !crate::coreanimation::has_tree(layer))
    {
        NATIVE_CAPTURES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
        return None;
    }
    let view_generation = source.map(Store::generation);
    let store_instance = source.map(|src| src.instance);
    let overlay_generation = OVERLAY_REVISIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(view as usize))
        .copied()
        .unwrap_or(0);
    let ca_content_revision = crate::coreanimation::native_scene_content_revision(layer);
    let ca_descendant_revision = crate::coreanimation::native_scene_descendant_revision(layer);
    let excluded_view_layers = if metal_children {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        crate::view::descendant_view_layers(view).hash(&mut hash);
        hash.finish()
    } else {
        0
    };
    let content_animating =
        crate::coreanimation::native_scene_content_animating(layer, darwin::mach::media_time());
    if !content_animating
        && let Some(hit) = NATIVE_CAPTURES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&key)
            .filter(|hit| {
                hit.layer == layer as usize
                    && hit.store_instance == store_instance
                    && hit.width == crop.width as u32
                    && hit.height == crop.height as u32
                    && hit.crop_x == crop.x
                    && hit.crop_y == crop.y
                    && hit.view_generation == view_generation
                    && hit.overlay_generation == overlay_generation
                    && hit.ca_content_revision == ca_content_revision
                    && hit.ca_descendant_revision == ca_descendant_revision
                    && hit.excluded_view_layers == excluded_view_layers
            })
    {
        hit.last_used = next_native_capture_use();
        return (!hit.pixels.is_empty())
            .then(|| (std::sync::Arc::clone(&hit.pixels), hit.revision));
    }
    // CA animations, overlay content and sublayer rendering have their own
    // invalidation paths: only a plain source store can assert narrow damage.
    let plain_store = !content_animating
        && overlay.is_none()
        && !crate::coreanimation::has_tree(layer)
        && source.is_some();
    // For a producer-verified local edit, other cached tiles remain
    // bit-identical. Advance their generation without re-rasterizing or
    // hashing a full tile; missing damage history fails closed below.
    if plain_store && let Some(src) = source {
        let mut captures = NATIVE_CAPTURES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prior) = captures.get_mut(&key)
            && prior.plain_store
            && prior.store_instance == Some(src.instance)
            && prior.layer == layer as usize
            && prior.overlay_generation == overlay_generation
            && prior.ca_content_revision == ca_content_revision
            && prior.ca_descendant_revision == ca_descendant_revision
            && prior.view_generation.is_some()
            && src.width == width
            && src.height == height
            && src
                .damage_since(prior.view_generation.unwrap())
                .is_some_and(|region| region.intersection(&crop).is_empty())
        {
            prior.view_generation = view_generation;
            prior.last_used = next_native_capture_use();
            return (!prior.pixels.is_empty())
                .then(|| (std::sync::Arc::clone(&prior.pixels), prior.revision));
        }
    }

    // Authenticate the producer's changed rectangle against the cached
    // immutable generation before any full alpha scan, raster or hash.
    #[cfg(test)]
    let disable_patch = std::env::var_os("ROSEMARY_BENCH_NATIVE_PLAIN_PATCH_DISABLE").is_some();
    #[cfg(not(test))]
    let disable_patch = false;
    #[cfg(test)]
    let compare_slow = std::env::var_os("ROSEMARY_BENCH_NATIVE_CAPTURE_FORCE_SLOW").is_some();
    #[cfg(not(test))]
    let compare_slow = false;
    if plain_store
        && !compare_slow
        && !disable_patch
        && let Some(src) = source
        && src.width == width
        && src.height == height
        && let Some(snapshot) = update_plain_opaque_capture(
            key,
            src,
            layer as usize,
            crop,
            overlay_generation,
            ca_content_revision,
            ca_descendant_revision,
            excluded_view_layers,
        )
    {
        return Some(snapshot);
    }
    let damage = if plain_store && let Some(source) = source {
        NATIVE_CAPTURES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .and_then(|prior| {
                (prior.plain_store
                    && prior.store_instance == Some(source.instance)
                    && prior.layer == layer as usize
                    && prior.ca_content_revision == ca_content_revision
                    && prior.ca_descendant_revision == ca_descendant_revision
                    && prior.overlay_generation == overlay_generation)
                    .then_some(prior)?;
                let region = source
                    .damage_since(prior.view_generation?)?
                    .intersection(&crop);
                (!region.is_empty()).then_some(NativeCpuDamage {
                    from_revision: prior.revision,
                    rect: Rect {
                        x: region.x - crop.x,
                        y: region.y - crop.y,
                        ..region
                    },
                })
            })
    } else {
        None
    };
    #[cfg(test)]
    NATIVE_RENDER_COUNT.with(|count| count.set(count.get() + 1));
    // A plain fully opaque backing store has exactly the same BGRA texels
    // after blending over a transparent native background. Allocate/copy its
    // crop directly; the existing compositor path handles alpha and CA.
    let exact_opaque = if plain_store && !compare_slow {
        source.filter(|src| src.width == width && src.height == height && src.fully_opaque())
    } else {
        None
    };
    let output = if let Some(src) = exact_opaque {
        let mut pixels = Vec::with_capacity(crop.width as usize * crop.height as usize * 4);
        for row in crop.y as usize..(crop.y + crop.height) as usize {
            let offset = (row * src.width as usize + crop.x as usize) * 4;
            pixels.extend_from_slice(&src.pixels[offset..offset + crop.width as usize * 4]);
        }
        Store {
            instance: NEXT_STORE_ID.fetch_add(1, Ordering::Relaxed),
            width: crop.width as u32,
            height: crop.height as u32,
            origin: (0, 0),
            pixels,
            dirty: false,
            generation: 0,
            changes: VecDeque::new(),
            alpha_class: AtomicU8::new(ALPHA_OPAQUE),
        }
    } else {
        let mut output = Store::new(crop.width as u32, crop.height as u32);
        let whole = Rect {
            x: 0,
            y: 0,
            width: crop.width,
            height: crop.height,
        };
        let logical = Rect {
            x: -crop.x,
            y: -crop.y,
            width: i64::from(width),
            height: i64::from(height),
        };
        use crate::coreanimation::{Pass, render_with_root_opacity};
        if !metal_children {
            render_with_root_opacity(
                view,
                &mut output,
                logical,
                whole,
                Pass::Background,
                Some(1.0),
            );
        }
        if let Some(source) = source {
            // CA/view opacity is applied by the retained scene, not a second time
            // during this isolated pixel capture.
            blend_at(&mut output, source, -crop.x, -crop.y, whole, 1.0);
        }
        if let Some(overlay) = overlay {
            // Software overlays (including CPU-backed web content) are also
            // separate native layers; previously they were irreversibly composed
            // under the later Metal drawable in the full window store.
            blend_overlay_clipped(
                &mut output,
                overlay,
                (-crop.x).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
                (-crop.y).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
                width,
                height,
                whole,
            );
        }
        if metal_children {
            crate::coreanimation::render_metal_sublayers_selected(
                view,
                &mut output,
                logical,
                whole,
                selected,
                gpu_groups,
            );
        } else {
            render_with_root_opacity(
                view,
                &mut output,
                logical,
                whole,
                Pass::Sublayers,
                Some(1.0),
            );
        }
        output
    };
    // A verified empty capture must have the same generation/dependency
    // validation as a nonempty image. Keep only a zero-byte sentinel, not
    // the full transparent RGBA allocation, and never emit it to Metal.
    let empty = output
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| pixel[3] == 0);
    let opaque_hashes = if !empty && exact_opaque.is_some() {
        Some(opaque_capture_hashes(
            &output.pixels,
            crop.width as usize * 4,
        ))
    } else {
        None
    };
    let (pixels, revision) = if empty {
        (std::sync::Arc::new(Vec::new()), 0)
    } else {
        let revision = if let Some(hashes) = &opaque_hashes {
            opaque_capture_revision(hashes, output.pixels.len())
        } else {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            output.pixels.hash(&mut hasher);
            hasher.finish()
        };
        (std::sync::Arc::new(output.pixels), revision)
    };
    let mut captures = NATIVE_CAPTURES.lock().unwrap_or_else(|e| e.into_inner());
    if content_animating {
        captures.remove(&key);
    } else {
        captures.insert(
            key,
            NativeCapture {
                layer: layer as usize,
                store_instance,
                width: crop.width as u32,
                height: crop.height as u32,
                crop_x: crop.x,
                crop_y: crop.y,
                view_generation,
                overlay_generation,
                ca_content_revision,
                ca_descendant_revision,
                excluded_view_layers,
                plain_store,
                pixels: std::sync::Arc::clone(&pixels),
                opaque_hashes,
                revision,
                damage,
                last_used: next_native_capture_use(),
            },
        );
        drop(captures);
        trim_native_capture_caches((NativeCacheKind::Flat, key));
    }
    (!empty).then_some((pixels, revision))
}

/// The AppKit event thread only publishes proposed native sources. Metal
/// acknowledges the exact original source revision after it has committed the
/// corresponding GPU scene. A proposed image is never proof of admission.
static GPU_ADMITTED_NATIVE: std::sync::LazyLock<Mutex<HashMap<usize, HashMap<usize, u64>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
/// Worker-to-AppKit notification. SDL video queries and new CPU backing
/// damage are performed by the owner pump, never under a Vulkan state lock.
static GPU_ADMISSION_DAMAGE: std::sync::LazyLock<Mutex<std::collections::HashSet<usize>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

pub fn set_native_gpu_admission(window: sdl3::Window, admitted: &[(usize, u64)]) {
    if window.is_null() {
        return;
    }
    let current: HashMap<usize, u64> = admitted.iter().copied().collect();
    let mut registry = GPU_ADMITTED_NATIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // Exact GPU admission changes require an owner-thread repaint, but the
    // lower-content underlay has no dependency on upper source revisions.
    let unchanged = registry.get(&(window as usize)) == Some(&current);
    registry.insert(window as usize, current);
    drop(registry);
    if unchanged {
        return;
    }
    // Do not evict GPU_UNDERLAYS here. Its independent lower-content
    // fingerprint is checked only when native separation is actually allowed.
    // The owner thread will re-snapshot the now-admitted independent scene
    // and recompute the backing source without changing the guest's store.
    GPU_ADMISSION_DAMAGE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(window as usize);
    crate::view::invalidate_metal_scene_for_window(window);
}

fn admitted_native_in_published_scene(
    sdl: sdl3::Window,
    dma_only: bool,
) -> std::collections::HashSet<usize> {
    let admitted = GPU_ADMITTED_NATIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let accepted = admitted.get(&(sdl as usize));
    let mut grouped = HashMap::<usize, (usize, usize)>::new();
    for layer in crate::view::metal_scene(sdl).unwrap_or_default() {
        let relevant = if dma_only {
            layer.native_dmabuf.is_some()
        } else {
            layer.has_native_cpu_source() || layer.native_dmabuf.is_some()
        };
        if !relevant {
            continue;
        }
        let member = grouped.entry(layer.native_owner).or_default();
        member.0 += 1;
        if accepted.is_some_and(|a| a.get(&layer.layer) == Some(&layer.native_revision)) {
            member.1 += 1;
        }
    }
    // A multi-tile native view may be excluded from CPU backing only once
    // every visible, nontransparent tile has its exact GPU revision admitted.
    grouped
        .into_iter()
        .filter_map(|(owner, (total, ready))| {
            (owner != 0 && total == ready).then_some(owner | (1usize << (usize::BITS - 1)))
        })
        .collect()
}

/// One immutable underlay per window, keyed only by content below the
/// first Metal view. Changes to independently composited native foreground
/// must not force a 4K CPU reconstruction or an unchanged GPU upload.
struct GpuUnderlayCache {
    fingerprint: u64,
    pixels: std::sync::Arc<Store>,
    // Only NSViews contributing below the first Metal layer affect this Arc.
    // Keep identities through detach until the cache is rebuilt, so a later
    // pointer reuse cannot resurrect an old view's pixels.
    views: std::collections::HashSet<usize>,
}
static GPU_UNDERLAYS: std::sync::LazyLock<Mutex<HashMap<usize, GpuUnderlayCache>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static UNDERLAY_ANIMATING_WINDOWS: std::sync::LazyLock<Mutex<std::collections::HashSet<usize>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));
#[cfg(test)]
static GPU_UNDERLAY_RENDERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
pub(crate) fn gpu_underlay_render_count_for_tests() -> usize {
    GPU_UNDERLAY_RENDERS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The flattened backing changes for independent foregrounds as well, but
/// only underlying view generations, placement and CA *contents* matter here.
pub fn gpu_under_metal_cached(sdl: sdl3::Window, source: &Store) -> Option<std::sync::Arc<Store>> {
    use std::hash::{Hash, Hasher};
    let window = crate::window::window_for(sdl);
    if window.is_null() {
        return None;
    }
    let placed = place_tree(window, sdl);
    let first = placed
        .iter()
        .position(|p| crate::layer::is_metal(crate::view::layer_of(p.view)))?;
    if !can_separate_native(sdl, &placed, first) {
        // A temporarily unadmitted foreground must use flattened pixels,
        // but it does not invalidate the independently cached lower content.
        return None;
    }
    let mut key = std::collections::hash_map::DefaultHasher::new();
    (source.width, source.height, first).hash(&mut key);
    crate::window::background_pixel(window).hash(&mut key);
    crate::window::presentation_clip(sdl)
        .map(|rect| (rect.x, rect.y, rect.w, rect.h))
        .hash(&mut key);
    crate::decor::pixels(sdl)
        .as_ref()
        .map(|pixels| pixels.generation)
        .hash(&mut key);
    let views = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
    let overlays = OVERLAY_REVISIONS.lock().unwrap_or_else(|e| e.into_inner());
    let mut animating = false;
    let deps: std::collections::HashSet<usize> = placed[..first]
        .iter()
        .map(|placed| placed.view as usize)
        .collect();
    for p in &placed[..first] {
        (p.view as usize, p.rect, p.clip).hash(&mut key);
        p.alpha.to_bits().hash(&mut key);
        views
            .as_ref()
            .and_then(|all| all.get(&(p.view as usize)))
            .map(Store::generation)
            .hash(&mut key);
        overlays.get(&(p.view as usize)).hash(&mut key);
        let layer = crate::view::layer_of(p.view);
        crate::coreanimation::native_scene_tree_revision(layer).hash(&mut key);
        animating |= crate::coreanimation::flattened_underlay_content_animating(
            layer,
            darwin::mach::media_time(),
        );
    }
    drop(overlays);
    drop(views);
    let fingerprint = key.finish();
    if animating {
        // The same static model revision can describe arbitrarily many
        // presentation-opacity frames. Do not resurrect pre-animation pixels
        // once the animation finishes with its original model value.
        GPU_UNDERLAYS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(sdl as usize));
    } else {
        if let Some(hit) = GPU_UNDERLAYS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(sdl as usize))
            .filter(|entry| entry.fingerprint == fingerprint)
        {
            return Some(std::sync::Arc::clone(&hit.pixels));
        }
    }
    let result = std::sync::Arc::new(gpu_under_metal(sdl, source)?);
    if !animating {
        GPU_UNDERLAYS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                sdl as usize,
                GpuUnderlayCache {
                    fingerprint,
                    pixels: std::sync::Arc::clone(&result),
                    views: deps,
                },
            );
    }
    Some(result)
}

/// A flattened background animation needs fresh CPU backing pixels even if
/// the Metal scene's own metadata is unchanged. The last tick after animation
/// expiration must restore the model value once rather than freezing the
/// final interpolated frame.
pub(crate) fn underlay_animation_tick(sdl: sdl3::Window, now: f64) -> bool {
    let window = crate::window::window_for(sdl);
    let active = if window.is_null() {
        false
    } else {
        let placed = place_tree(window, sdl);
        let first = placed
            .iter()
            .position(|p| crate::layer::is_metal(crate::view::layer_of(p.view)));
        first.is_some_and(|first| {
            placed[..first].iter().any(|p| {
                crate::coreanimation::flattened_underlay_content_animating(
                    crate::view::layer_of(p.view),
                    now,
                )
            })
        })
    };
    let mut running = UNDERLAY_ANIMATING_WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let was = if active {
        running.insert(sdl as usize)
    } else {
        running.remove(&(sdl as usize))
    };
    active || was
}

/// A missing native capture is not evidence of transparency. Recapture the
/// visible pixels and accept only the validated, zero-byte negative entry.
/// Use the same crop/tile geometry as native AppKit scene publication.
fn native_scene_proven_empty(placed: &Placed) -> bool {
    let visible = placed.rect.intersection(&placed.clip);
    if visible.is_empty() {
        return true;
    }
    let (Ok(width), Ok(height)) = (
        u32::try_from(placed.rect.width),
        u32::try_from(placed.rect.height),
    ) else {
        return false;
    };
    // Scene publication uses one capture when it fits the 64 MiB limit,
    // otherwise independent 2048-pixel tiles.
    let tile_edge = if native_scene_extent_supported(visible.width as u32, visible.height as u32) {
        i64::MAX
    } else {
        2048
    };
    let mut y = visible.y;
    while y < visible.y + visible.height {
        let height_tile = (visible.y + visible.height - y).min(tile_edge);
        let mut x = visible.x;
        while x < visible.x + visible.width {
            let width_tile = (visible.x + visible.width - x).min(tile_edge);
            let crop = Rect {
                x: x - placed.rect.x,
                y: y - placed.rect.y,
                width: width_tile,
                height: height_tile,
            };
            // A row-banded capture is opaque, so never empty. Trying it first
            // also keeps a view published tiled from gaining a flat copy.
            if native_scene_tiled_pixels_cropped_with_damage(placed.view, width, height, crop)
                .is_some()
                || native_scene_pixels_cropped(placed.view, width, height, crop).is_some()
            {
                return false;
            }
            let key = (
                placed.view as usize,
                crop.x,
                crop.y,
                width_tile as u32,
                height_tile as u32,
                width,
                height,
                false,
                0,
            );
            if !NATIVE_CAPTURES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
                .is_some_and(|capture| capture.pixels.is_empty())
            {
                return false;
            }
            x += width_tile;
        }
        y += height_tile;
    }
    true
}

/// A native source belongs outside the bottom store only once its complete
/// independent snapshot has been accepted. If any top view lacks a source,
/// preserve the original full window path instead of silently losing it.
fn can_separate_native(sdl: sdl3::Window, placed: &[Placed], first: usize) -> bool {
    // Never remove native pixels from the GPU base unless the owner thread
    // has already published a corresponding independent source for *every*
    // visible native view above the first Metal drawable. Oversized or not-
    // yet-ready native sources must remain visible, even if ordering cannot
    // be fully resolved during that transitional frame.
    let captured = admitted_native_in_published_scene(sdl, false);
    let views = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
    let overlays = OVERLAYS.lock().unwrap_or_else(|e| e.into_inner());
    let sources = placed[first + 1..]
        .iter()
        .filter(|p| !crate::layer::is_metal(crate::view::layer_of(p.view)))
        .map(|p| {
            let has_store = views
                .as_ref()
                .is_some_and(|stores| stores.contains_key(&(p.view as usize)));
            let has_overlay = overlays.iter().any(|(id, _)| *id == p.view as usize);
            let has_dma = overlays.iter().any(|(id, entry)| {
                *id == p.view as usize && matches!(entry.picture, Picture::Dmabuf(_))
            });
            (
                p,
                has_store
                    || has_overlay
                    || crate::coreanimation::has_tree(crate::view::layer_of(p.view)),
                has_dma,
            )
        })
        .collect::<Vec<_>>();
    drop(overlays);
    drop(views);
    let mut any = false;
    for (p, drawn, has_dma) in sources {
        if !drawn || p.clip.is_empty() {
            continue;
        }
        if captured.contains(&((p.view as usize) | (1usize << (usize::BITS - 1)))) {
            any = true;
            continue;
        }
        // DMA producers may be visible even when their CPU/CA capture is
        // empty. An unadmitted producer must never be silently subtracted.
        if !has_dma && native_scene_proven_empty(p) {
            continue;
        }
        return false;
    }
    any
}

/// Window backing source excluding native views painted *above* the first
/// Metal layer. Those views are separately retained and placed above Metal.
/// The original full CPU store remains unmodified for non-Vulkan fallbacks.
pub fn gpu_under_metal(sdl: sdl3::Window, source: &Store) -> Option<Store> {
    let _owed = crate::coreanimation::defer_releases();
    let window = crate::window::window_for(sdl);
    if window.is_null() {
        return None;
    }
    let placed = place_tree(window, sdl);
    let first = placed
        .iter()
        .position(|p| crate::layer::is_metal(crate::view::layer_of(p.view)))?;
    if !can_separate_native(sdl, &placed, first) {
        return None;
    }
    let views = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
    let overlays = OVERLAYS.lock().unwrap_or_else(|e| e.into_inner());
    #[cfg(test)]
    GPU_UNDERLAY_RENDERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut bottom = Store::new(source.width, source.height);
    let whole = Rect {
        x: 0,
        y: 0,
        width: i64::from(source.width),
        height: i64::from(source.height),
    };
    let background = crate::window::background_pixel(window);
    for pixel in bottom.pixels.as_chunks_mut::<4>().0 {
        *pixel = background;
    }
    for p in &placed[..first] {
        let clip = p.clip.intersection(&whole);
        use crate::coreanimation::{Pass, render};
        render(p.view, &mut bottom, p.rect, clip, Pass::Background);
        if let Some(source) = views
            .as_ref()
            .and_then(|stores| stores.get(&(p.view as usize)))
        {
            blend_at(&mut bottom, source, p.rect.x, p.rect.y, clip, p.alpha);
        }
        // CPU overlays painted below the first Metal drawable belong in its
        // underlay; those above it are separate scene sources instead.
        if let Some((_, overlay)) = overlays
            .iter()
            .find(|(id, o)| *id == p.view as usize && matches!(o.picture, Picture::Pixels(_)))
        {
            blend_overlay_clipped(
                &mut bottom,
                overlay,
                p.rect.x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
                p.rect.y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
                p.rect.width.clamp(0, i64::from(u32::MAX)) as u32,
                p.rect.height.clamp(0, i64::from(u32::MAX)) as u32,
                clip,
            );
        }
        render(p.view, &mut bottom, p.rect, clip, Pass::Sublayers);
    }
    if let Some(chrome) = crate::decor::pixels(sdl) {
        copy_chrome(&mut bottom, &chrome);
    }
    Some(bottom)
}

/// Bring a window's composite up to date with its views: recompose what was
/// drawn over, and everywhere a view appeared, moved or went away.
#[cfg(test)]
pub(crate) fn compose_window_for_tests(window: Id) {
    compose_window(window);
}

fn compose_window(window: Id) {
    // First, so it drops last: after every lock below is released.
    let _owed = crate::coreanimation::defer_releases();
    // SAFETY: a window this crate made.
    let sdl = unsafe { crate::window::sdl_window(window) };
    let Some((width, height)) = sdl3::video::window_size_in_pixels(sdl) else {
        return;
    };
    let (width, height) = (width.max(1) as u32, height.max(1) as u32);
    let placed = place_tree(window, sdl);
    let surface_damage: Vec<Rect> = placed
        .iter()
        .flat_map(|p| crate::coreanimation::surface_contents_damage(p.view, p.rect, p.clip))
        .collect();
    let now: Vec<(usize, Rect, Rect)> = placed
        .iter()
        .map(|p| (p.view as usize, p.clip, p.rect))
        .collect();
    let overlaid: Vec<usize> = OVERLAYS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(view, _)| *view)
        .collect();
    let opaque = opaque_views(&placed);
    let mut holes = Vec::new();
    let mut covered = Vec::new();
    for (index, p) in placed.iter().enumerate() {
        let layer = crate::view::layer_of(p.view);
        if !crate::layer::is_metal(layer) || p.clip.is_empty() {
            continue;
        }
        // An overlay is stacked above the layers of the views placed before
        // it - a web view inside a Metal-drawn panel - so there the store,
        // which carries the overlay, shows instead of the layer. So is a view
        // that draws: a web view's title bar over a game. Only where it drew
        // opaquely, since the store holds nothing of the layer to show
        // through a label's transparent pixels.
        let above: Vec<Rect> = placed[index + 1..]
            .iter()
            .filter(|q| {
                overlaid.contains(&(q.view as usize)) || opaque.contains(&(q.view as usize))
            })
            .map(|q| q.clip.intersection(&p.clip))
            .filter(|rect| !rect.is_empty())
            .collect();
        holes.extend(p.clip.subtract(&above).into_iter().map(|rect| Hole {
            layer: layer as usize,
            rect,
        }));
        if !above.is_empty() {
            covered.push((layer as usize, above));
        }
    }

    let mut stores = STORES.lock().unwrap_or_else(|e| e.into_inner());
    // A window no view has drawn into has nothing to compose. Composing it
    // anyway makes a store of transparent black out of the content view's own
    // rectangle - a view that appeared is a region to recompose, and there is
    // nothing to recompose it from - and presenting that paints over whatever
    // else owns the window's pixels. For a guest drawing through an
    // `NSOpenGLContext` that is the whole of its frame, so the window goes
    // black. Once a view has drawn, the composite exists and is kept up to
    // date from then on, including as content goes away again.
    let composed_before = stores
        .as_ref()
        .is_some_and(|stores| stores.contains_key(&(sdl as usize)));
    // A window with chrome always has something of its own to show.
    let chrome = crate::decor::pixels(sdl);
    if !composed_before && holes.is_empty() && chrome.is_none() {
        let views = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
        let drawn = views.as_ref().is_some_and(|views| {
            placed
                .iter()
                .any(|p| views.contains_key(&(p.view as usize)))
        }) || placed
            .iter()
            .any(|p| crate::coreanimation::has_tree(crate::view::layer_of(p.view)));
        if !drawn {
            return;
        }
    }
    let store = stores
        .get_or_insert_with(HashMap::new)
        .entry(sdl as usize)
        .or_insert_with(|| Store::new(width, height));
    let whole = Rect {
        x: 0,
        y: 0,
        width: i64::from(width),
        height: i64::from(height),
    };
    let resized = (store.width, store.height) != (width, height);
    if resized {
        *store = Store::new(width, height);
    }

    let mut windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    let state = windows
        .get_or_insert_with(HashMap::new)
        .entry(sdl as usize)
        .or_default();
    let mut region = std::mem::take(&mut state.damage);
    for rect in &surface_damage {
        region = region.union(rect);
    }
    if resized {
        region = whole;
    }
    if state.placed != now {
        // Where each view was and is, for every one that changed.
        for (_, rect, _) in state.placed.iter().filter(|entry| !now.contains(entry)) {
            region = region.union(rect);
        }
        for (_, rect, _) in now.iter().filter(|entry| !state.placed.contains(entry)) {
            region = region.union(rect);
        }
        state.placed = now;
    }
    let holes_changed = state.holes.len() != holes.len()
        || state
            .holes
            .iter()
            .zip(&holes)
            .any(|(a, b)| a.layer != b.layer || a.rect != b.rect);
    state.holes = holes;
    state.covered = covered;
    let chrome_generation = chrome.as_ref().map_or(0, |chrome| chrome.generation);
    let chrome_changed =
        std::mem::replace(&mut state.chrome, chrome_generation) != chrome_generation;
    drop(windows);
    let region = region.intersection(&whole);
    if region.is_empty() {
        if let Some(chrome) = chrome.as_ref().filter(|_| chrome_changed || resized) {
            copy_chrome(store, chrome);
            store.touch();
        }
        if holes_changed {
            store.touch();
        }
        return;
    }

    // What no view covers shows the window's `backgroundColor`, which a
    // program that draws only a logo and a bar relies on for everything else.
    // With none set it is `windowBackgroundColor`, AppKit's default; a layer
    // view's hole is the layer's either way.
    let background = crate::window::background_pixel(window);
    let logical = logical_pixel_bounds(sdl, whole);
    for row in region.y..region.y + region.height {
        let base = (row as usize * width as usize + region.x as usize) * 4;
        let pixels = store.pixels[base..base + region.width as usize * 4]
            .as_chunks_mut::<4>()
            .0;
        // Outside AppKit's logical window is the fullscreen container,
        // not more of the window's default background.
        // FIXME (ACCURACY): the native probe did not measure this area's color.
        if row < logical.y || row >= logical.y + logical.height {
            pixels.fill([0, 0, 0, 255]);
        } else {
            let left = (logical.x - region.x).clamp(0, region.width) as usize;
            let right = (logical.x + logical.width - region.x).clamp(0, region.width) as usize;
            pixels[..left].fill([0, 0, 0, 255]);
            pixels[left..right].fill(background);
            pixels[right..].fill([0, 0, 0, 255]);
        }
    }
    let views = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
    let mut animating = Vec::new();
    for p in &placed {
        let clip = p.clip.intersection(&region);
        use crate::coreanimation::{Pass, render};
        render(p.view, store, p.rect, clip, Pass::Background);
        if let Some(source) = views
            .as_ref()
            .and_then(|views| views.get(&(p.view as usize)))
        {
            blend_at(store, source, p.rect.x, p.rect.y, clip, p.alpha);
        }
        if render(p.view, store, p.rect, clip, Pass::Sublayers) {
            animating.push(p.view as usize);
        }
    }
    drop(views);
    ANIMATING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(window, _)| *window != sdl as usize);
    ANIMATING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .extend(animating.into_iter().map(|view| (sdl as usize, view)));
    if let Some(chrome) = &chrome {
        copy_chrome(store, chrome);
    }
    store.touch();
    // Only windows with independent native-over-Metal sources require this
    // additional snapshot when CPU content changes; ordinary AppKit-only
    // windows must not pay the hierarchy walk on every backing repaint.
    if crate::view::native_metal_scene_active(sdl) {
        crate::view::invalidate_metal_scene_for_window(sdl);
    }
}

/// Put a window's chrome over the top rows of its store.
fn copy_chrome(store: &mut Store, chrome: &crate::decor::Pixels) {
    let columns = store.width.min(chrome.width) as usize;
    for row in 0..store.height.min(chrome.height) as usize {
        let from = row * chrome.width as usize * 4;
        let to = row * store.width as usize * 4;
        store.pixels[to..to + columns * 4]
            .copy_from_slice(&chrome.pixels[from..from + columns * 4]);
    }
}

/// Present a window's store again at the next pump, without recomposing it:
/// for a presenter that lost what it last showed.
pub fn present_again(sdl: sdl3::Window) {
    // WSI recovery can request a genuinely fresh backing publication even
    // when the underlying view pixels have not changed.
    GPU_UNDERLAYS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(sdl as usize));
    let mut stores = STORES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(store) = stores
        .as_mut()
        .and_then(|stores| stores.get_mut(&(sdl as usize)))
    {
        store.touch();
    }
}

/// Recompose the whole of a window, whose background changed.
pub(crate) fn window_background_changed(window: Id) {
    // SAFETY: a window this crate made.
    let sdl = unsafe { crate::window::sdl_window(window) };
    if sdl.is_null() {
        return;
    }
    if let Some((width, height)) = sdl3::video::window_size_in_pixels(sdl) {
        damage(
            sdl,
            Rect {
                x: 0,
                y: 0,
                width: i64::from(width.max(0)),
                height: i64::from(height.max(0)),
            },
        );
    }
}

/// Have a window's chrome composited again, from `crate::decor` when it
/// repaints it.
pub(crate) fn chrome_changed(sdl: sdl3::Window) {
    let mut windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = windows
        .as_mut()
        .and_then(|windows| windows.get_mut(&(sdl as usize)))
    {
        // Zero is "none", so any real generation differs from it.
        state.chrome = 0;
    }
}

/// Draw `source` with its top-left corner at `left`, `top` of `store`,
/// source-over, within `clip`, faded to `opacity`.
fn blend_at(store: &mut Store, source: &Store, left: i64, top: i64, clip: Rect, opacity: f64) {
    if opacity <= 0.0 {
        return;
    }
    // 256 is exact: a scaled channel is then `texel * scale >> 8` with no
    // rounding error at full opacity.
    let scale = (opacity.min(1.0) * 256.0).round() as u32;
    // A view's store holds only the part of the view on screen.
    let (left, top) = (left + source.origin.0, top + source.origin.1);
    let source_rect = Rect {
        x: left,
        y: top,
        width: i64::from(source.width),
        height: i64::from(source.height),
    };
    let area = clip.intersection(&source_rect);
    if area.is_empty() {
        return;
    }
    let columns = area.width as usize;
    for row in area.y..area.y + area.height {
        let from = ((row - top) as usize * source.width as usize + (area.x - left) as usize) * 4;
        let to = (row as usize * store.width as usize + area.x as usize) * 4;
        let from_row = &source.pixels[from..from + columns * 4];
        let to_row = &mut store.pixels[to..to + columns * 4];
        for (target, texel) in to_row
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(from_row.as_chunks::<4>().0)
        {
            let texel = match scale {
                256 => *texel,
                _ => texel.map(|channel| ((u32::from(channel) * scale) >> 8) as u8),
            };
            match texel[3] {
                255 => target.copy_from_slice(&texel),
                0 => {}
                alpha => {
                    let keep = 255 - u32::from(alpha);
                    for channel in 0..4 {
                        let blended =
                            u32::from(texel[channel]) + u32::from(target[channel]) * keep / 255;
                        target[channel] = blended.min(255) as u8;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
static TEST_CPU_SURFACE_FALLBACKS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn cpu_fallback_calls_for_tests(window: sdl3::Window) -> usize {
    TEST_CPU_SURFACE_FALLBACKS
        .lock()
        .unwrap()
        .iter()
        .filter(|&&key| key == window as usize)
        .count()
}

/// Opt-in allocation/CPU benchmark for metadata-only native captures.
/// The view is retained solely in the builtin AppKit test process, not on a
/// host-visible desktop. Run with ROSEMARY_BENCH_NATIVE_4K=1 in release mode.
#[test]
fn metadata_only_4k_native_capture_reuses_immutable_pixels() {
    if std::env::var_os("ROSEMARY_BENCH_NATIVE_4K").is_none() {
        return;
    }
    crate::bootstrap();
    let view = objc::object::create_instance(objc::class::lookup(c"NSView").expect("NSView"), 0);
    let (w, h) = (3840u32, 2160u32);
    set_native_view_pixels_for_tests(
        view,
        w,
        h,
        [128u8, 64, 32, 255].repeat(w as usize * h as usize),
    );
    let before = native_render_count_for_tests();
    let start = std::time::Instant::now();
    let (original, revision) = native_scene_pixels(view, w, h).unwrap();
    let first_ms = start.elapsed().as_secs_f64() * 1e3;
    let start = std::time::Instant::now();
    for _ in 0..240 {
        let (cached, again) = native_scene_pixels(view, w, h).unwrap();
        assert!(std::sync::Arc::ptr_eq(&original, &cached));
        assert_eq!(again, revision);
    }
    let cached_us = start.elapsed().as_secs_f64() * 1e6 / 240.0;
    assert_eq!(
        native_render_count_for_tests() - before,
        1,
        "240 metadata updates allocated, rendered or hashed 4K sources again"
    );
    eprintln!(
        "4K native pixel capture first_ms={first_ms:.3} metadata_cache_mean_us={cached_us:.3} bytes={} cache_renders=1",
        original.len()
    );
    forget(view);
    objc::arc::release(view);
}

/// Measures multiple 6K native sources when unrelated scene changes
/// repeatedly resnapshot them. Opt in to avoid high allocations in CI;
/// the emitted cache hit rate and video-thread capture time are diagnostics.
#[test]
fn two_large_native_views_capture_cache_pressure_benchmark() {
    if std::env::var_os("ROSEMARY_BENCH_NATIVE_MULTI_6K").is_none() {
        return;
    }
    crate::bootstrap();
    const W: u32 = 6016;
    const H: u32 = 3384;
    let views: Vec<Id> = (0..2)
        .map(|n| {
            let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
            set_native_view_pixels_for_tests(
                view,
                W,
                H,
                [32u8, 64, 128 + n as u8, 255].repeat(W as usize * H as usize),
            );
            view
        })
        .collect();
    let mut tiles = Vec::new();
    for y in (0..H).step_by(2048) {
        for x in (0..W).step_by(2048) {
            tiles.push(Rect {
                x: i64::from(x),
                y: i64::from(y),
                width: i64::from((W - x).min(2048)),
                height: i64::from((H - y).min(2048)),
            });
        }
    }
    assert_eq!(tiles.len(), 6);
    let before = native_render_count_for_tests();
    let first = std::time::Instant::now();
    for view in &views {
        for crop in &tiles {
            assert!(native_scene_pixels_cropped(*view, W, H, *crop).is_some());
        }
    }
    let warm_ms = first.elapsed().as_secs_f64() * 1000.0;
    let after_warm = native_render_count_for_tests();
    let loops = 4usize;
    let refresh = std::time::Instant::now();
    for _ in 0..loops {
        for view in &views {
            for crop in &tiles {
                assert!(native_scene_pixels_cropped(*view, W, H, *crop).is_some());
            }
        }
    }
    let repeat_ms = refresh.elapsed().as_secs_f64() * 1000.0 / loops as f64;
    let again = native_render_count_for_tests();
    let misses = again - after_warm;
    let total = loops * views.len() * tiles.len();
    let hits = total.saturating_sub(misses);
    let cache_bytes: usize = NATIVE_CAPTURES
        .lock()
        .unwrap()
        .values()
        .map(|v| v.pixels.len())
        .sum();
    eprintln!(
        "6K multi-view native capture warm_ms={warm_ms:.3} repeat_mean_ms={repeat_ms:.3} repeat_hits={hits}/{total} rerecaptures={misses} cache_bytes={cache_bytes} new_renders={}",
        again - before
    );
    assert!(cache_bytes <= NATIVE_CAPTURE_CACHE_LIMIT);
    for view in views {
        forget(view);
        objc::arc::release(view)
    }
}

#[cfg(test)]
pub(crate) fn set_native_view_pixels_for_tests(view: Id, width: u32, height: u32, pixels: Vec<u8>) {
    assert_eq!(pixels.len(), width as usize * height as usize * 4);
    let mut store = Store::new(width, height);
    store.pixels = pixels;
    // A replacement fixture is a new producer generation, not the unchanged
    // generation 0 of a newly allocated Store.
    store.generation = VIEW_STORES
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|all| all.get(&(view as usize)))
        .map_or(1, |old| old.generation().wrapping_add(1));
    VIEW_STORES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(view as usize, store);
    crate::view::invalidate_metal_scene_for_view(view);
}

#[cfg(test)]
pub(crate) fn queue_dirty_store_for_tests(window: sdl3::Window) {
    let (width, height) = sdl3::video::window_size_in_pixels(window).unwrap();
    let mut store = Store::new(width.max(1) as u32, height.max(1) as u32);
    store.touch();
    STORES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(window as usize, store);
}

#[cfg(test)]
pub(crate) fn has_store_for_tests(window: sdl3::Window) -> bool {
    STORES
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|stores| stores.contains_key(&(window as usize)))
}

/// The part of the surface to keep when a logical frame excludes the rest, or
/// `None` for a window without one: a windowed surface that outgrew its store
/// during a resize is left as it was, not painted black.
fn software_exterior(clip: Rect, whole: Rect, shown: Rect) -> Option<Rect> {
    (clip != whole).then(|| clip.intersection(&shown))
}

/// Black out obsolete host pixels outside the logical AppKit bounds without
/// changing content/Metal layers within it. `pitch` includes SDL row padding.
fn blackout_software_exterior(
    pixels: &mut [u8],
    pitch: usize,
    width: usize,
    height: usize,
    allowed: Rect,
) {
    let whole = Rect {
        x: 0,
        y: 0,
        width: width as i64,
        height: height as i64,
    };
    let allowed = whole.intersection(&allowed);
    for band in whole.subtract(&[allowed]) {
        for y in band.y as usize..(band.y + band.height) as usize {
            let start = y * pitch + band.x as usize * 4;
            for pixel in pixels[start..start + band.width as usize * 4]
                .as_chunks_mut::<4>()
                .0
            {
                pixel.copy_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
}

fn present_dirty() {
    let presenter = *PRESENTER.lock().unwrap_or_else(|e| e.into_inner());
    let mut stores = STORES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(stores) = stores.as_mut() else {
        return;
    };
    for (&window, store) in stores
        .iter_mut()
        .filter(|(window, store)| store.dirty || dump_overdue(**window as sdl3::Window))
    {
        let window = window as sdl3::Window;
        store.dirty = false;
        let backing = store;
        let (composed, gpu_overlays) = compose(window, backing);
        let store = composed.as_ref().unwrap_or(backing);
        dump(window, store);
        let holes = WINDOWS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|windows| windows.get(&(window as usize)))
            .map(|state| state.holes.clone())
            .unwrap_or_default();
        let outcome = presenter.map_or(PresentOutcome::Fallback, |present| {
            present(window, store, &holes, &gpu_overlays)
        });
        if outcome == PresentOutcome::Deferred {
            // Preserve the original store and its overlays for a later turn;
            // a slow FIFO acquisition must not hold up input for other windows.
            backing.dirty = true;
            continue;
        }
        if outcome == PresentOutcome::DisplayedEarlier {
            presented_through_gpu(window, true);
            backing.dirty = true;
            continue;
        }
        let presented = outcome == PresentOutcome::Displayed;
        presented_through_gpu(window, presented);
        if presented {
            continue;
        }
        // The dma-buf overlays are left out of this frame; `shows_dmabufs`
        // now says no, and their views send pixels from the next one.
        // The guest's own GL context owns this window's surface; see
        // [`crate::window::draws_with_opengl`]. There is nothing to fall back
        // to, so the store is left unshown rather than taking the context away.
        if crate::window::draws_with_opengl(window) {
            continue;
        }
        let transparent = sdl3::video::window_flags(window) & sdl3::video::flags::TRANSPARENT != 0;
        let chrome = crate::decor::pixels(window).filter(|_| transparent);
        // A layer that draws into the surface itself keeps its last frame
        // there; the store holds only background where it is.
        let kept: Vec<Rect> = {
            let layers = SURFACE_LAYERS.lock().unwrap_or_else(|e| e.into_inner());
            holes
                .iter()
                .filter(|hole| layers.contains(&(window as usize, hole.layer)))
                .map(|hole| hole.rect)
                .collect()
        };
        #[cfg(test)]
        TEST_CPU_SURFACE_FALLBACKS
            .lock()
            .unwrap()
            .push(window as usize);
        sdl3::video::present_window(window, |surface| {
            let rows = store.height.min(surface.height.max(0) as u32) as usize;
            let columns = store.width.min(surface.width.max(0) as u32) as usize;
            let shown = Rect {
                x: 0,
                y: 0,
                width: columns as i64,
                height: rows as i64,
            };
            let whole = Rect {
                x: 0,
                y: 0,
                width: i64::from(surface.width.max(0)),
                height: i64::from(surface.height.max(0)),
            };
            let clip = logical_pixel_bounds(window, whole);
            let logical = clip.intersection(&shown);
            if let Some(allowed) = software_exterior(clip, whole, shown) {
                // SAFETY: SDL owns pitch * height bytes during this callback.
                let pixels = unsafe {
                    std::slice::from_raw_parts_mut(
                        surface.pixels,
                        surface.pitch as usize * surface.height.max(0) as usize,
                    )
                };
                blackout_software_exterior(
                    pixels,
                    surface.pitch as usize,
                    surface.width.max(0) as usize,
                    surface.height.max(0) as usize,
                    allowed,
                );
            }
            for piece in logical.subtract(&kept) {
                let (left, width) = (piece.x as usize, piece.width as usize);
                for row in piece.y as usize..(piece.y + piece.height) as usize {
                    let from = (row * store.width as usize + left) * 4;
                    // SAFETY: SDL reported `pitch` bytes per row for `height`
                    // rows, and the piece lies inside `rows` by `columns`.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            store.pixels[from..from + width * 4].as_ptr(),
                            surface.pixels.add(row * surface.pitch as usize + left * 4),
                            width * 4,
                        )
                    };
                }
            }
            if transparent && rows > 0 && columns > 0 {
                // SAFETY: SDL reported `pitch` bytes per row for `height` rows.
                let pixels = unsafe {
                    std::slice::from_raw_parts_mut(surface.pixels, surface.pitch as usize * rows)
                };
                let pitch = surface.pitch as usize;
                let chrome_rows = chrome.as_ref().map_or(0, |chrome| chrome.height as usize);
                make_opaque(pixels, columns, rows, pitch, chrome_rows);
                if let Some(chrome) = chrome.as_ref().filter(|chrome| chrome.radius > 0.0) {
                    chrome::corners::clip(pixels, columns, rows, pitch, chrome.radius, false, true);
                }
            }
            rows > 0 && columns > 0
        });
    }
}

/// Give every pixel from row `from` down full alpha.
///
/// A window with room for rounded corners shows its alpha, which a Mac's
/// window content never does: what no view drew is transparent in the
/// store, and there it would be a hole through to the desktop. Premultiplied
/// pixels given full alpha are as if drawn over black, which is what an
/// opaque window shows for them too.
fn make_opaque(pixels: &mut [u8], width: usize, height: usize, pitch: usize, from: usize) {
    for row in from..height {
        let start = row * pitch;
        for pixel in pixels[start..start + width * 4].as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
    }
}

/// Pixels a view shows above its window's store.
///
/// A view whose content comes from outside the window's drawing - a web view,
/// whose page another toolkit renders - is a layer the window server stacks
/// above the views that draw into the store. Writing it into the store instead
/// would lose it at the next flush of the view underneath, which repaints its
/// whole rectangle without knowing a subview is there.
pub struct Overlay {
    pub width: u32,
    pub height: u32,
    pub picture: Picture,
}

pub enum Picture {
    /// Premultiplied B, G, R, A, top row first.
    Pixels(Vec<u8>),
    /// Drawn by the presenter; see [`shows_dmabufs`].
    Dmabuf(std::sync::Arc<Dmabuf>),
}

/// Keyed by the view's address, in the order the overlays were first set.
static OVERLAYS: Mutex<Vec<(usize, Overlay)>> = Mutex::new(Vec::new());
static OVERLAY_REVISIONS: std::sync::LazyLock<Mutex<HashMap<usize, u64>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_OVERLAY_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Capture the DMA-BUF identity and setter revision on the AppKit owner
/// thread; no guest view pointer or mutable overlay crosses to the GPU worker.
pub(crate) fn native_dma_overlay(view: Id) -> Option<(std::sync::Arc<Dmabuf>, u64)> {
    let overlays = OVERLAYS.lock().unwrap_or_else(|e| e.into_inner());
    let (_, overlay) = overlays.iter().find(|(id, overlay)| {
        *id == view as usize && matches!(overlay.picture, Picture::Dmabuf(_))
    })?;
    let Picture::Dmabuf(picture) = &overlay.picture else {
        unreachable!()
    };
    let revision = OVERLAY_REVISIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(view as usize))
        .copied()
        .unwrap_or(0);
    Some((std::sync::Arc::clone(picture), revision))
}

/// Only DMA-BUF sources that have an independently retained native scene
/// layer may be excluded from the bottom window-store overlay blending.
pub fn native_dma_above_metal(sdl: sdl3::Window) -> Vec<usize> {
    let window = crate::window::window_for(sdl);
    if window.is_null() {
        return Vec::new();
    }
    let placed = place_tree(window, sdl);
    let Some(first) = placed
        .iter()
        .position(|p| crate::layer::is_metal(crate::view::layer_of(p.view)))
    else {
        return Vec::new();
    };
    let native = admitted_native_in_published_scene(sdl, true);
    let overlays = OVERLAYS.lock().unwrap_or_else(|e| e.into_inner());
    placed[first + 1..]
        .iter()
        .filter_map(|p| {
            if !native.contains(&((p.view as usize) | (1usize << (usize::BITS - 1)))) {
                return None;
            }
            overlays
                .iter()
                .any(|(id, o)| *id == p.view as usize && matches!(o.picture, Picture::Dmabuf(_)))
                .then_some(p.view as usize)
        })
        .collect()
}

/// Show `overlay` over `view`'s rectangle from the next present on.
///
/// The view must call [`remove_overlay`] before it is deallocated.
pub fn set_overlay(view: Id, overlay: Overlay) {
    {
        let mut overlays = OVERLAYS.lock().unwrap_or_else(|e| e.into_inner());
        match overlays.iter_mut().find(|(key, _)| *key == view as usize) {
            Some((_, existing)) => *existing = overlay,
            None => overlays.push((view as usize, overlay)),
        }
    }
    OVERLAY_REVISIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            view as usize,
            NEXT_OVERLAY_REVISION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
    mark_window_of(view);
    crate::view::invalidate_metal_scene_for_view(view);
}

pub fn remove_overlay(view: Id) {
    let removed = {
        let mut overlays = OVERLAYS.lock().unwrap_or_else(|e| e.into_inner());
        let before = overlays.len();
        overlays.retain(|(key, _)| *key != view as usize);
        overlays.len() != before
    };
    if removed {
        OVERLAY_REVISIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(view as usize));
        mark_window_of(view);
        crate::view::invalidate_metal_scene_for_view(view);
    }
}

/// The rectangles, in window pixels, of the overlays stacked above `layer`
/// in `sdl`'s window, which that layer's presents must not draw over.
pub fn covered(sdl: sdl3::Window, layer: Id) -> Vec<Rect> {
    let windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    windows
        .as_ref()
        .and_then(|windows| windows.get(&(sdl as usize)))
        .and_then(|state| state.covered.iter().find(|(key, _)| *key == layer as usize))
        .map(|(_, rects)| rects.clone())
        .unwrap_or_default()
}

/// Have `view`'s window presented again, composing it if nothing has been:
/// an overlay on a window nothing else draws into still has to reach the
/// screen.
fn mark_window_of(view: Id) {
    // SAFETY: a live view, which both callers are passed.
    let Some(frame) = (unsafe { crate::view::presentation_frame(view) }) else {
        return;
    };
    damage(
        frame.window,
        Rect {
            x: i64::from(frame.x),
            y: i64::from(frame.y),
            width: i64::from(frame.width),
            height: i64::from(frame.height),
        },
    );
}

/// The store with every visible pixel overlay of `window` drawn over it, or
/// `None` when there is nothing to draw and the store can be presented as it
/// is; and the dma-buf overlays, which only the presenter can draw.
#[cfg(test)]
pub(crate) fn gpu_overlay_instances_for_tests(
    window: sdl3::Window,
    store: &Store,
) -> Vec<GpuOverlay> {
    compose(window, store).1
}

fn compose(window: sdl3::Window, store: &Store) -> (Option<Store>, Vec<GpuOverlay>) {
    let overlays = OVERLAYS.lock().unwrap_or_else(|e| e.into_inner());
    let mut composed: Option<Store> = None;
    let mut gpu = Vec::new();
    let allowed = logical_pixel_bounds(
        window,
        Rect {
            x: 0,
            y: 0,
            width: i64::from(store.width),
            height: i64::from(store.height),
        },
    );
    for (view, overlay) in overlays.iter() {
        let view = *view as Id;
        // SAFETY: an overlay is removed before its view is deallocated.
        let shown = unsafe { crate::view::window_of(view) };
        if !crate::window::is_shown(shown) || crate::view::is_hidden_or_has_hidden_ancestor(view) {
            continue;
        }
        let Some(frame) = (unsafe { crate::view::presentation_frame(view) }) else {
            continue;
        };
        if frame.window != window {
            continue;
        }
        let original = Rect {
            x: i64::from(frame.x),
            y: i64::from(frame.y),
            width: i64::from(frame.width),
            height: i64::from(frame.height),
        };
        if original.intersection(&allowed).is_empty() {
            continue;
        }
        if let Picture::Dmabuf(picture) = &overlay.picture {
            gpu.push(GpuOverlay {
                picture: picture.clone(),
                view: view as usize,
                // Keep original placement for texture sampling; the final
                // Vulkan presentation masks pixels beyond the logical window.
                rect: original,
            });
            continue;
        }
        let target = composed.get_or_insert_with(|| Store {
            instance: NEXT_STORE_ID.fetch_add(1, Ordering::Relaxed),
            width: store.width,
            height: store.height,
            origin: store.origin,
            pixels: store.pixels.clone(),
            dirty: false,
            // An overlay that changes damages the store under it.
            generation: store.generation,
            changes: VecDeque::new(),
            alpha_class: AtomicU8::new(ALPHA_UNCLASSIFIED),
        });
        blend_overlay_clipped(
            target,
            overlay,
            frame.x,
            frame.y,
            frame.width,
            frame.height,
            allowed,
        );
    }
    (composed, gpu)
}

/// Draw `overlay` scaled into the rectangle at `left`, `top` of `store`,
/// source-over, nearest-neighbour.
#[cfg(test)]
fn blend_overlay(
    store: &mut Store,
    overlay: &Overlay,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
) {
    blend_overlay_clipped(
        store,
        overlay,
        left,
        top,
        width,
        height,
        Rect {
            x: 0,
            y: 0,
            width: i64::from(store.width),
            height: i64::from(store.height),
        },
    );
}

fn blend_overlay_clipped(
    store: &mut Store,
    overlay: &Overlay,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    allowed: Rect,
) {
    let Picture::Pixels(pixels) = &overlay.picture else {
        return;
    };
    let (source_width, source_height) = (overlay.width as usize, overlay.height as usize);
    if source_width == 0
        || source_height == 0
        || width == 0
        || height == 0
        || pixels.len() < source_width * source_height * 4
    {
        return;
    }
    let clipped = Rect {
        x: i64::from(left),
        y: i64::from(top),
        width: i64::from(width),
        height: i64::from(height),
    }
    .intersection(&allowed)
    .intersection(&Rect {
        x: 0,
        y: 0,
        width: i64::from(store.width),
        height: i64::from(store.height),
    });
    let x0 = clipped.x as usize;
    let y0 = clipped.y as usize;
    let x1 = (clipped.x + clipped.width) as usize;
    let y1 = (clipped.y + clipped.height) as usize;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let columns: Vec<usize> = (x0..x1)
        .map(|x| {
            ((x as i64 - i64::from(left)) as usize * source_width / width as usize)
                .min(source_width - 1)
        })
        .collect();
    for y in y0..y1 {
        let row = ((y as i64 - i64::from(top)) as usize * source_height / height as usize)
            .min(source_height - 1);
        let from_row = &pixels[row * source_width * 4..(row + 1) * source_width * 4];
        let base = y * store.width as usize * 4;
        let to_row = &mut store.pixels[base + x0 * 4..base + x1 * 4];
        for (target, &column) in to_row.as_chunks_mut::<4>().0.iter_mut().zip(&columns) {
            let texel = &from_row[column * 4..column * 4 + 4];
            let alpha = u32::from(texel[3]);
            if alpha == 255 {
                target.copy_from_slice(texel);
                continue;
            }
            for channel in 0..4 {
                let blended =
                    u32::from(texel[channel]) + u32::from(target[channel]) * (255 - alpha) / 255;
                target[channel] = blended.min(255) as u8;
            }
        }
    }
}

/// `ROSEMARY_STORE_OUT=DIR`: write each window's store as a binary PPM when it
/// is presented, at most once a second per window, overwriting the last.
///
/// The Metal frame dump sees only layers; this is the half of a window that
/// views drew on the CPU, for a session with no screen to look at.
/// When each window's store was last written, and whether a present since
/// then went unwritten.
static DUMPED: Mutex<Vec<(usize, std::time::Instant, bool)>> = Mutex::new(Vec::new());

/// Whether a store should be presented again only so that its last frame is
/// written: without this, a window that stops changing within a second of
/// its last dump leaves a stale picture of itself behind.
fn dump_overdue(window: sdl3::Window) -> bool {
    std::env::var_os("ROSEMARY_STORE_OUT").is_some()
        && DUMPED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|(key, at, skipped)| {
                *key == window as usize && *skipped && at.elapsed().as_secs_f64() >= 1.0
            })
}

fn dump(window: sdl3::Window, store: &Store) {
    use std::time::Instant;
    let Some(directory) = std::env::var_os("ROSEMARY_STORE_OUT") else {
        return;
    };
    let mut last = DUMPED.lock().unwrap_or_else(|e| e.into_inner());
    match last.iter_mut().find(|(key, _, _)| *key == window as usize) {
        Some((_, at, skipped)) if at.elapsed().as_secs_f64() < 1.0 => {
            *skipped = true;
            return;
        }
        Some((_, at, skipped)) => {
            *at = Instant::now();
            *skipped = false;
        }
        None => last.push((window as usize, Instant::now(), false)),
    }
    let mut out = format!("P6\n{} {}\n255\n", store.width, store.height).into_bytes();
    for texel in store.pixels.as_chunks::<4>().0 {
        out.extend_from_slice(&[texel[2], texel[1], texel[0]]);
    }
    let name = format!("store-{:x}.ppm", window as usize);
    let _ = std::fs::write(std::path::Path::new(&directory).join(name), out);
}

unsafe extern "C" fn lock_focus_if_can_draw(receiver: Id, _sel: Sel) -> u8 {
    if crate::view::is_hidden_or_has_hidden_ancestor(receiver) {
        return 0;
    }
    push_focus(receiver) as u8
}

unsafe extern "C" fn lock_focus(receiver: Id, _sel: Sel) {
    push_focus(receiver);
}

unsafe extern "C" fn unlock_focus(_receiver: Id, _sel: Sel) {
    pop_focus();
}

unsafe extern "C" fn focus_view(_class: Id, _sel: Sel) -> Id {
    current_focus().map_or(std::ptr::null_mut(), |focus| focus.view)
}

/// `-getRectsBeingDrawn:count:`: the whole view, which is what [`display_view`]
/// draws.
unsafe extern "C" fn get_rects_being_drawn(
    receiver: Id,
    _sel: Sel,
    rects: *mut *const NSRect,
    count: *mut isize,
) {
    thread_local! {
        static DRAWN: RefCell<NSRect> = const { RefCell::new(NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSSize { width: 0.0, height: 0.0 },
        }) };
    }
    let rect = current_focus()
        .filter(|focus| focus.view == receiver)
        .map(|focus| focus.drawn_rect())
        .unwrap_or_default();
    DRAWN.with(|drawn| {
        *drawn.borrow_mut() = rect;
        if !rects.is_null() {
            // SAFETY: the caller's out-pointer; the rect lives as long as the
            // thread, past the `-drawRect:` it is read in.
            unsafe { *rects = drawn.as_ptr() };
        }
    });
    if !count.is_null() {
        // SAFETY: the caller's out-pointer.
        unsafe { *count = 1 };
    }
}

/// A context explicitly selected on this thread, and the focus depth it was
/// selected at. AppKit installs its own context for each `-drawRect:`, so an
/// override only applies at the depth it was set; `None` leaves the focused
/// view's context available.
#[derive(Clone, Copy)]
struct ContextOverride {
    context: Id,
    depth: usize,
}

struct CurrentGraphicsContext(Cell<Option<ContextOverride>>);

impl Drop for CurrentGraphicsContext {
    fn drop(&mut self) {
        if let Some(held) = self.0.get().filter(|held| !held.context.is_null()) {
            objc::arc::release(held.context);
        }
    }
}

thread_local! {
    static CURRENT_GRAPHICS_CONTEXT: CurrentGraphicsContext =
        const { CurrentGraphicsContext(Cell::new(None)) };
}

fn focus_depth() -> usize {
    FOCUS.with(|stack| stack.borrow().len())
}

fn set_thread_current_context(context: Id) {
    let depth = focus_depth();
    let _ = CURRENT_GRAPHICS_CONTEXT.try_with(|current| {
        let retained = if context.is_null() {
            context
        } else {
            objc::arc::retain(context)
        };
        let previous = current.0.replace(Some(ContextOverride {
            context: retained,
            depth,
        }));
        if let Some(previous) = previous.filter(|previous| !previous.context.is_null()) {
            objc::arc::release(previous.context);
        }
    });
}

/// Drop an override whose focus has ended, so a stale context neither shadows
/// later drawing nor outlives the window it came from.
fn release_context_override_above(depth: usize) {
    let _ = CURRENT_GRAPHICS_CONTEXT.try_with(|current| {
        if let Some(held) = current.0.get().filter(|held| held.depth > depth) {
            current.0.set(None);
            if !held.context.is_null() {
                objc::arc::release(held.context);
            }
        }
    });
}

unsafe extern "C" fn set_current_context(_class: Id, _sel: Sel, context: Id) {
    set_thread_current_context(context);
}

unsafe extern "C" fn current_context(_class: Id, _sel: Sel) -> Id {
    current_graphics_context()
}

/// `-displayRectIgnoringOpacity:inContext:`: the view and its subviews drawn
/// into `context` under that context's current transform, which stands for
/// the view's own coordinates, rather than into the window.
unsafe extern "C" fn display_rect_ignoring_opacity(view: Id, _sel: Sel, rect: NSRect, context: Id) {
    if context.is_null() {
        return;
    }
    // SAFETY: an `NSGraphicsContext`, whose port is the caller's.
    let port = unsafe { cg_context(context, objc::sel::get("CGContext")) };
    if port.is_null() {
        return;
    }
    let depth = focus_depth();
    let Ok(previous) = CURRENT_GRAPHICS_CONTEXT.try_with(|current| {
        current.0.replace(Some(ContextOverride {
            context: objc::arc::retain(context),
            depth,
        }))
    }) else {
        return;
    };
    draw_tree_into(view, rect, port);
    let _ = CURRENT_GRAPHICS_CONTEXT.try_with(|current| {
        if let Some(held) = current.0.replace(previous) {
            objc::arc::release(held.context);
        }
    });
}

/// Send `-drawRect:` to `view`, clipped to `rect`, then to each visible
/// subview with `port`'s transform moved into that subview's coordinates.
fn draw_tree_into(view: Id, rect: NSRect, port: *const c_void) {
    use crate::cgcontext::{
        CGContextClipToRect, CGContextRestoreGState, CGContextSaveGState, CGContextScaleCTM,
        CGContextTranslateCTM,
    };
    // SAFETY: the caller's live context; every save is restored below.
    unsafe {
        CGContextSaveGState(port);
        CGContextClipToRect(port, rect);
        let draw: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
        draw(view, objc::sel::get("drawRect:"), rect);
    }
    let flipped = crate::view::is_flipped(view);
    for child in crate::view::subviews_of(view) {
        if crate::view::is_hidden_itself(child) {
            continue;
        }
        let frame = crate::view::frame_of(child);
        let bounds = crate::view::bounds_of(child);
        // SAFETY: as above.
        unsafe {
            CGContextSaveGState(port);
            CGContextTranslateCTM(port, frame.origin.x, frame.origin.y);
            if crate::view::is_flipped(child) != flipped {
                CGContextTranslateCTM(port, 0.0, frame.size.height);
                CGContextScaleCTM(port, 1.0, -1.0);
            }
            CGContextTranslateCTM(port, -bounds.origin.x, -bounds.origin.y);
        }
        draw_tree_into(child, bounds, port);
        // SAFETY: the state saved above.
        unsafe { CGContextRestoreGState(port) };
    }
    // SAFETY: the state saved on entry.
    unsafe { CGContextRestoreGState(port) };
}

/// The `CGContext` the current `NSGraphicsContext` was made over with
/// `+graphicsContextWithGraphicsPort:flipped:` (or a bitmap rep), and whether
/// it is flipped; `None` while drawing goes to the focused view.
pub(crate) fn current_port() -> Option<(*const c_void, bool)> {
    let (port, flipped) = wrapped(current_graphics_context())?;
    Some((port as *const c_void, flipped))
}

fn current_graphics_context() -> Id {
    let depth = focus_depth();
    match CURRENT_GRAPHICS_CONTEXT.try_with(|current| current.0.get()) {
        Ok(Some(held)) if held.depth == depth => return held.context,
        Ok(_) => {}
        // Avoid panicking through Objective-C if Cocoa calls back as this
        // thread's local graphics context is being destroyed.
        Err(_) => return std::ptr::null_mut(),
    }
    if current_focus().is_none() {
        return std::ptr::null_mut();
    }
    shared_instance(c"NSGraphicsContext")
}

fn shared_instance(name: &'static std::ffi::CStr) -> Id {
    static INSTANCES: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
    let class = objc::class::lookup(name).expect("declared during bootstrap");
    let key = objc::class::ptr(class) as usize;
    let mut instances = INSTANCES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, object)) = instances.iter().find(|(k, _)| *k == key) {
        return *object as Id;
    }
    let object = objc::object::create_instance(class, 0);
    objc::arc::retain(object);
    instances.push((key, object as usize));
    object
}

/// Graphics contexts made over a caller's `CGContext`, by object: the port,
/// retained, and whether the context is flipped.
static WRAPPED: Mutex<Vec<(usize, usize, bool)>> = Mutex::new(Vec::new());

fn wrapped(context: Id) -> Option<(usize, bool)> {
    WRAPPED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(object, _, _)| *object == context as usize)
        .map(|(_, port, flipped)| (*port, *flipped))
}

/// `+graphicsContextWithGraphicsPort:flipped:` and its `CGContext` spelling:
/// a context that draws into the port given rather than the focused view.
unsafe extern "C" fn context_with_port(
    class: Id,
    _sel: Sel,
    port: *const c_void,
    flipped: u8,
) -> Id {
    if port.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the class this method is installed on.
    let class = unsafe { objc::class::from_ptr(class as *const objc::class::Class) }
        .or_else(|| objc::class::lookup(c"NSGraphicsContext"))
        .expect("NSGraphicsContext is declared");
    let context = objc::object::create_instance(class, 0);
    // SAFETY: the caller's live `CGContext`, held for this object's life.
    let port = unsafe { crate::cgcontext::CGContextRetain(port) };
    WRAPPED.lock().unwrap_or_else(|e| e.into_inner()).push((
        context as usize,
        port as usize,
        flipped != 0,
    ));
    objc::arc::autorelease(context)
}

unsafe extern "C" fn context_dealloc(receiver: Id, _sel: Sel) {
    let port = {
        let mut table = WRAPPED.lock().unwrap_or_else(|e| e.into_inner());
        let at = table
            .iter()
            .position(|(object, _, _)| *object == receiver as usize);
        at.map(|at| table.remove(at).1)
    };
    if let Some(port) = port {
        // SAFETY: the reference `context_with_port` took.
        unsafe { crate::cgcontext::CGContextRelease(port as *const c_void) };
    }
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn context_is_flipped(receiver: Id, _sel: Sel) -> u8 {
    if let Some((_, flipped)) = wrapped(receiver) {
        return u8::from(flipped);
    }
    current_focus().is_some_and(|focus| focus.flipped) as u8
}

unsafe extern "C" fn cg_context(receiver: Id, _sel: Sel) -> *const c_void {
    if let Some((port, _)) = wrapped(receiver) {
        return port as *const c_void;
    }
    crate::cgcontext::focus_context()
}

unsafe extern "C" fn ignored(_receiver: Id, _sel: Sel) {}

unsafe extern "C" fn ignored_integer(_receiver: Id, _sel: Sel, _value: isize) {}

unsafe extern "C" fn zero(_receiver: Id, _sel: Sel) -> isize {
    0
}

pub(crate) unsafe extern "C" fn shared_color_space(_receiver: Id, _sel: Sel) -> Id {
    shared_instance(c"NSColorSpace")
}

/// `-[NSColorSpace CGColorSpace]`, which the caller does not own.
unsafe extern "C" fn cg_color_space(_receiver: Id, _sel: Sel) -> *const c_void {
    static SPACE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    // SAFETY: creates an object this runtime keeps for the process.
    *SPACE.get_or_init(|| unsafe { crate::graphics::CGColorSpaceCreateDeviceRGB() } as usize)
        as *const c_void
}

/// `-[NSColorSpace ICCProfileData]`: the one space's sRGB profile, kept for
/// the process like the space itself.
unsafe extern "C" fn icc_profile_data(_receiver: Id, _sel: Sel) -> Id {
    static DATA: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *DATA.get_or_init(|| corefoundation::collections::new_data(crate::icc::srgb_profile()) as usize)
        as Id
}

pub(crate) fn install() {
    let view = objc::class::lookup(c"NSView").expect("declared before its methods");
    methods!(view, {
        "lockFocusIfCanDraw" => lock_focus_if_can_draw, c"c16@0:8";
        "lockFocus" => lock_focus, c"v16@0:8";
        "unlockFocus" => unlock_focus, c"v16@0:8";
        "getRectsBeingDrawn:count:" => get_rects_being_drawn, c"v32@0:8^^{CGRect}16^q24";
        "displayRectIgnoringOpacity:inContext:" => display_rect_ignoring_opacity,
            c"v56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48";
    });
    class_methods!(view, {
        "focusView" => focus_view, c"@16@0:8";
    });

    let context = objc::class::lookup(c"NSGraphicsContext").expect("declared");
    // Saving and restoring the graphics state is `crate::bezier`'s.
    class_methods!(context, {
        "currentContext" => current_context, c"@16@0:8";
        "setCurrentContext:" => set_current_context, c"v24@0:8@16";
        "graphicsContextWithGraphicsPort:flipped:" => context_with_port, c"@28@0:8^v16c24";
        "graphicsContextWithCGContext:flipped:" => context_with_port, c"@28@0:8^v16c24";
    });
    methods!(context, {
        "dealloc" => context_dealloc, c"v16@0:8";
        "isFlipped" => context_is_flipped, c"c16@0:8";
        "CGContext" => cg_context, c"^v16@0:8";
        "graphicsPort" => cg_context, c"^v16@0:8";
        "flushGraphics" => ignored, c"v16@0:8";
        "compositingOperation" => zero, c"q16@0:8";
        "setCompositingOperation:" => ignored_integer, c"v24@0:8q16";
    });

    let space = objc::class::lookup(c"NSColorSpace").expect("declared");
    class_methods!(space, {
        "sRGBColorSpace" => shared_color_space, c"@16@0:8";
        "deviceRGBColorSpace" => shared_color_space, c"@16@0:8";
        "genericRGBColorSpace" => shared_color_space, c"@16@0:8";
    });
    methods!(space, {
        "CGColorSpace" => cg_color_space, c"^v16@0:8";
        "ICCProfileData" => icc_profile_data, c"@16@0:8";
    });
    let window = objc::class::lookup(c"NSWindow").expect("declared");
    methods!(window, {
        "colorSpace" => shared_color_space, c"@16@0:8";
    });
    // Every colour is in the one space; see `crate::color`.
    let color = objc::class::lookup(c"NSColor").expect("declared");
    methods!(color, {
        "colorSpace" => shared_color_space, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_native_direct_capture_matches_translucent_fallback_and_crops() {
        crate::bootstrap();
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        const WIDTH: u32 = 6;
        const HEIGHT: u32 = 4;
        let mut pixels = [12u8, 34, 56, 255].repeat((WIDTH * HEIGHT) as usize);
        pixels[((WIDTH as usize) + 4) * 4..((WIDTH as usize) + 5) * 4]
            .copy_from_slice(&[90, 80, 70, 255]);
        set_native_view_pixels_for_tests(view, WIDTH, HEIGHT, pixels);
        let crop = Rect {
            x: 3,
            y: 1,
            width: 2,
            height: 2,
        };
        let opaque = native_scene_pixels_cropped(view, WIDTH, HEIGHT, crop)
            .unwrap()
            .0;
        assert_eq!(&opaque[..4], &[12, 34, 56, 255]);
        assert_eq!(&opaque[4..8], &[90, 80, 70, 255]);
        set_native_view_pixels_for_tests(view, WIDTH, HEIGHT, {
            let mut pixels = [12u8, 34, 56, 255].repeat((WIDTH * HEIGHT) as usize);
            pixels[((WIDTH as usize) + 4) * 4..((WIDTH as usize) + 5) * 4]
                .copy_from_slice(&[90, 80, 70, 0]);
            pixels
        });
        let mixed = native_scene_pixels_cropped(view, WIDTH, HEIGHT, crop)
            .unwrap()
            .0;
        assert_eq!(&mixed[..4], &[12, 34, 56, 255]);
        assert_eq!(
            &mixed[4..8],
            &[0, 0, 0, 0],
            "alpha-zero premultiplied source may never preserve nonzero RGB"
        );
        forget(view);
        objc::arc::release(view);
    }

    #[test]
    fn store_producer_damage_requires_contiguous_versions_and_full_unknown_fallback() {
        let mut store = Store::new(32, 16);
        assert_eq!(store.damage_since(0), None);
        store.touch_region(Rect {
            x: 2,
            y: 3,
            width: 2,
            height: 1,
        });
        store.touch_region(Rect {
            x: 8,
            y: 5,
            width: 3,
            height: 2,
        });
        assert_eq!(
            store.damage_since(0),
            Some(Rect {
                x: 2,
                y: 3,
                width: 9,
                height: 4
            })
        );
        assert_eq!(
            store.damage_since(1),
            Some(Rect {
                x: 8,
                y: 5,
                width: 3,
                height: 2
            })
        );
        store.touch();
        assert_eq!(
            store.damage_since(0),
            Some(Rect {
                x: 0,
                y: 0,
                width: 32,
                height: 16
            })
        );
        for _ in 0..34 {
            store.touch_region(Rect {
                x: 9,
                y: 7,
                width: 1,
                height: 1,
            });
        }
        assert_eq!(
            store.damage_since(0),
            None,
            "missing generations must disable partial uploads"
        );
        assert_eq!(
            store.damage_since(store.generation() - 1),
            Some(Rect {
                x: 9,
                y: 7,
                width: 1,
                height: 1
            })
        );
        let before = store.generation();
        store.resize(64, 8);
        assert_eq!(
            store.damage_since(before),
            None,
            "resize invalidates previous extent authority"
        );
    }

    #[test]
    fn unchanged_native_tiles_reuse_pixels_after_disjoint_producer_damage() {
        if !sdl3::isolated::run_here() {
            return;
        }
        crate::bootstrap();
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let mut store = Store::new(8, 4);
        store.pixels = [0, 0, 255, 255].repeat(8 * 4);
        store.touch();
        VIEW_STORES
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(view as usize, store);
        let left = Rect {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        };
        let right = Rect {
            x: 4,
            y: 0,
            width: 4,
            height: 4,
        };
        let old_left = native_scene_pixels_cropped(view, 8, 4, left).unwrap();
        let old_right = native_scene_pixels_cropped(view, 8, 4, right).unwrap();
        let before = native_render_count_for_tests();
        {
            let mut stores = VIEW_STORES.lock().unwrap();
            let src = stores.as_mut().unwrap().get_mut(&(view as usize)).unwrap();
            src.pixels[(8 + 6) * 4..(8 + 6) * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
            src.touch_region(Rect {
                x: 6,
                y: 1,
                width: 1,
                height: 1,
            });
        }
        let new_left = native_scene_pixels_cropped_with_damage(view, 8, 4, left).unwrap();
        assert!(std::sync::Arc::ptr_eq(&old_left.0, &new_left.0));
        assert_eq!(new_left.1, old_left.1);
        assert_eq!(new_left.2, None);
        assert_eq!(
            native_render_count_for_tests(),
            before,
            "unaffected tile was rasterized or rehashed"
        );
        let new_right = native_scene_pixels_cropped_with_damage(view, 8, 4, right).unwrap();
        assert_eq!(new_right.2.unwrap().from_revision, old_right.1);
        assert_ne!(new_right.1, old_right.1);
        assert_eq!(native_render_count_for_tests(), before + 1);
        forget(view);
        objc::arc::release(view);
    }

    #[test]
    fn flat_and_tiled_captures_of_the_same_pixels_share_a_revision() {
        // 600-pixel rows make a band differ from any power-of-two chunk.
        const W: u32 = 600;
        const H: u32 = 520;
        crate::bootstrap();
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let mut store = Store::new(W, H);
        store.pixels = [10, 20, 30, 255].repeat((W * H) as usize);
        store.touch();
        VIEW_STORES
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(view as usize, store);
        let crop = Rect {
            x: 0,
            y: 0,
            width: i64::from(W),
            height: i64::from(H),
        };
        let tiled = native_scene_tiled_pixels_cropped_with_damage(view, W, H, crop).unwrap();
        let flat = native_scene_pixels_cropped_with_damage(view, W, H, crop).unwrap();
        assert_eq!(flat.1, tiled.1);
        {
            let mut stores = VIEW_STORES.lock().unwrap();
            let src = stores.as_mut().unwrap().get_mut(&(view as usize)).unwrap();
            let at = (300 * W as usize + 7) * 4;
            src.pixels[at..at + 4].copy_from_slice(&[1, 2, 3, 255]);
            src.touch_region(Rect {
                x: 7,
                y: 300,
                width: 1,
                height: 1,
            });
        }
        // Both incremental patches must rehash the band the edit landed in.
        let patches = NATIVE_INCREMENTAL_PATCHES.with(Cell::get);
        let tiled = native_scene_tiled_pixels_cropped_with_damage(view, W, H, crop).unwrap();
        let flat = native_scene_pixels_cropped_with_damage(view, W, H, crop).unwrap();
        assert_eq!(NATIVE_INCREMENTAL_PATCHES.with(Cell::get), patches + 1);
        assert_eq!(
            flat.2.unwrap().from_revision,
            tiled.2.unwrap().from_revision
        );
        assert_eq!(flat.1, tiled.1);
        forget(view);
        objc::arc::release(view);
    }

    #[test]
    fn a_tiled_republish_keeps_damage_the_gpu_may_not_have_staged() {
        const W: u32 = 512;
        const H: u32 = 520;
        crate::bootstrap();
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let mut store = Store::new(W, H);
        store.pixels = [10, 20, 30, 255].repeat((W * H) as usize);
        store.touch();
        VIEW_STORES
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(view as usize, store);
        let crop = Rect {
            x: 0,
            y: 0,
            width: i64::from(W),
            height: i64::from(H),
        };
        let touch = |pixel: [u8; 4]| {
            let mut stores = VIEW_STORES.lock().unwrap();
            let src = stores.as_mut().unwrap().get_mut(&(view as usize)).unwrap();
            let at = (200 * W as usize + 9) * 4;
            src.pixels[at..at + 4].copy_from_slice(&pixel);
            src.touch_region(Rect {
                x: 9,
                y: 200,
                width: 1,
                height: 1,
            });
        };
        let cold = native_scene_tiled_pixels_cropped_with_damage(view, W, H, crop).unwrap();
        touch([1, 2, 3, 255]);
        let patched = native_scene_tiled_pixels_cropped_with_damage(view, W, H, crop).unwrap();
        let damage = patched.2.expect("a real edit carries damage");
        assert_eq!(damage.from_revision, cold.1);
        let again = native_scene_tiled_pixels_cropped_with_damage(view, W, H, crop).unwrap();
        assert_eq!((again.1, again.2), (patched.1, Some(damage)));
        touch([1, 2, 3, 255]);
        let identical = native_scene_tiled_pixels_cropped_with_damage(view, W, H, crop).unwrap();
        assert_eq!((identical.1, identical.2), (patched.1, Some(damage)));
        forget(view);
        objc::arc::release(view);
    }

    #[test]
    fn a_translucent_dirty_pixel_disproves_opacity_without_a_full_scan() {
        const W: u32 = 512;
        const H: u32 = 520;
        crate::bootstrap();
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let mut store = Store::new(W, H);
        store.pixels = [10, 20, 30, 255].repeat((W * H) as usize);
        store.touch();
        VIEW_STORES
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(view as usize, store);
        let crop = Rect {
            x: 0,
            y: 0,
            width: i64::from(W),
            height: i64::from(H),
        };
        native_scene_tiled_pixels_cropped_with_damage(view, W, H, crop).unwrap();
        native_scene_pixels_cropped(view, W, H, crop).unwrap();
        {
            let mut stores = VIEW_STORES.lock().unwrap();
            let src = stores.as_mut().unwrap().get_mut(&(view as usize)).unwrap();
            let at = (400 * W as usize + 3) * 4;
            src.pixels[at..at + 4].copy_from_slice(&[0, 0, 0, 0]);
            src.touch_region(Rect {
                x: 3,
                y: 400,
                width: 1,
                height: 1,
            });
        }
        let scans = opaque_alpha_scan_count_for_tests();
        assert!(native_scene_tiled_pixels_cropped_with_damage(view, W, H, crop).is_none());
        native_scene_pixels_cropped(view, W, H, crop).unwrap();
        assert_eq!(opaque_alpha_scan_count_for_tests(), scans);
        forget(view);
        objc::arc::release(view);
    }

    #[test]
    fn proving_a_tiled_view_nonempty_caches_no_flat_copy() {
        const W: u32 = 512;
        const H: u32 = 520;
        crate::bootstrap();
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let mut store = Store::new(W, H);
        store.pixels = [10, 20, 30, 255].repeat((W * H) as usize);
        store.touch();
        VIEW_STORES
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(view as usize, store);
        let rect = Rect {
            x: 0,
            y: 0,
            width: i64::from(W),
            height: i64::from(H),
        };
        let placed = Placed {
            view,
            rect,
            clip: rect,
            alpha: 1.0,
        };
        assert!(!native_scene_proven_empty(&placed));
        let owner = view as usize;
        assert!(
            NATIVE_TILED_CAPTURES
                .lock()
                .unwrap()
                .keys()
                .any(|key| key.0 == owner)
        );
        assert!(
            !NATIVE_CAPTURES
                .lock()
                .unwrap()
                .keys()
                .any(|key| key.0 == owner)
        );
        forget(view);
        objc::arc::release(view);
    }

    #[test]
    fn a_view_known_to_be_translucent_is_not_a_tiled_candidate() {
        const W: u32 = 512;
        const H: u32 = 520;
        crate::bootstrap();
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let mut store = Store::new(W, H);
        store.pixels = [10, 20, 30, 255].repeat((W * H) as usize);
        store.touch();
        VIEW_STORES
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(view as usize, store);
        assert!(native_scene_tiled_candidate(view, W, H));
        {
            let mut stores = VIEW_STORES.lock().unwrap();
            let src = stores.as_mut().unwrap().get_mut(&(view as usize)).unwrap();
            src.pixels[3] = 0;
            src.touch();
            assert!(!src.fully_opaque());
        }
        let scans = opaque_alpha_scan_count_for_tests();
        assert!(!native_scene_tiled_candidate(view, W, H));
        assert_eq!(opaque_alpha_scan_count_for_tests(), scans);
        forget(view);
        objc::arc::release(view);
    }

    #[test]
    fn flat_and_tiled_capture_caches_share_one_budget_without_releasing_readers() {
        const W: u32 = 512;
        const H: u32 = 1024;
        const LIMIT: usize = 3 * 1024 * 1024;
        let key = |view| (view, 0, 0, W, H, W, H, false, 0);
        let flat_capture = |color: [u8; 4]| NativeCapture {
            layer: 1,
            store_instance: Some(1),
            width: W,
            height: H,
            crop_x: 0,
            crop_y: 0,
            view_generation: Some(1),
            overlay_generation: 0,
            ca_content_revision: 0,
            ca_descendant_revision: 0,
            excluded_view_layers: 0,
            plain_store: true,
            pixels: std::sync::Arc::new(color.repeat((W * H) as usize)),
            opaque_hashes: None,
            revision: 1,
            damage: None,
            last_used: next_native_capture_use(),
        };
        let tiled_capture = |color: [u8; 4]| {
            let tiles =
                NativePixelTiles::from_opaque_bgra(W, H, &color.repeat((W * H) as usize)).unwrap();
            NativeTiledCache {
                layer: 1,
                store_instance: 1,
                view_generation: 1,
                ca_revision: 0,
                descendant_revision: 0,
                overlay_revision: 0,
                tiles: std::sync::Arc::new(tiles),
                damage: None,
                last_used: next_native_capture_use(),
            }
        };
        let mut flat = HashMap::new();
        let mut tiled = HashMap::new();
        let flat_key = key(1);
        let mixed_key = key(2);
        flat.insert(flat_key, flat_capture([11, 22, 33, 255]));
        let held_flat = std::sync::Arc::clone(&flat[&flat_key].pixels);
        tiled.insert(mixed_key, tiled_capture([44, 55, 66, 255]));
        let held_tiled = std::sync::Arc::clone(&tiled[&mixed_key].tiles);
        assert_eq!(flat[&flat_key].pixels.len(), 2 * 1024 * 1024);
        assert_eq!(tiled[&mixed_key].tiles.retained_bytes(), 2 * 1024 * 1024);
        trim_native_capture_maps(
            &mut flat,
            &mut tiled,
            (NativeCacheKind::Tiled, mixed_key),
            LIMIT + 2 * 1024 * 1024,
        );
        assert!(flat.contains_key(&flat_key) && tiled.contains_key(&mixed_key));
        // Publishing a tiled snapshot must evict the older flat cache entry,
        // not the caller's separately owned flat Arc.
        trim_native_capture_maps(
            &mut flat,
            &mut tiled,
            (NativeCacheKind::Tiled, mixed_key),
            LIMIT,
        );
        assert!(flat.is_empty());
        assert!(tiled.contains_key(&mixed_key));
        assert_eq!(&held_flat[..4], &[11, 22, 33, 255]);
        // The same view moves to the flat path (e.g. a DMA overlay). The
        // previous tiled source remains readable by a GPU worker.
        flat.insert(mixed_key, flat_capture([77, 88, 99, 255]));
        let held_second_flat = std::sync::Arc::clone(&flat[&mixed_key].pixels);
        trim_native_capture_maps(
            &mut flat,
            &mut tiled,
            (NativeCacheKind::Flat, mixed_key),
            LIMIT,
        );
        assert!(tiled.is_empty());
        assert!(flat.contains_key(&mixed_key));
        let mut prior = [0; 4];
        held_tiled.copy_rect_to(12, 31, 1, 1, &mut prior).unwrap();
        assert_eq!(prior, [44, 55, 66, 255]);
        // Re-enter the eligible tiled path. Eviction must never modify the
        // flat snapshot still retained by another reader.
        tiled.insert(mixed_key, tiled_capture([111, 122, 133, 255]));
        trim_native_capture_maps(
            &mut flat,
            &mut tiled,
            (NativeCacheKind::Tiled, mixed_key),
            LIMIT,
        );
        assert!(flat.is_empty());
        assert!(tiled.contains_key(&mixed_key));
        assert_eq!(&held_second_flat[..4], &[77, 88, 99, 255]);
        let total: usize = flat.values().map(|v| v.pixels.len()).sum::<usize>()
            + tiled
                .values()
                .map(|v| v.tiles.retained_bytes())
                .sum::<usize>();
        assert!(total <= LIMIT);
        assert_eq!(NATIVE_CAPTURE_CACHE_LIMIT, 256 * 1024 * 1024);
    }

    #[test]
    fn alternating_large_native_captures_keep_hot_flat_and_tiled_entries() {
        const W: u32 = 512;
        const H: u32 = 1024;
        const CAPACITY: usize = 3 * 2 * 1024 * 1024;
        let key = |view| (view, 0, 0, W, H, W, H, false, 0);
        let make_flat = |color: u8| NativeCapture {
            layer: 1,
            store_instance: Some(1),
            width: W,
            height: H,
            crop_x: 0,
            crop_y: 0,
            view_generation: Some(1),
            overlay_generation: 0,
            ca_content_revision: 0,
            ca_descendant_revision: 0,
            excluded_view_layers: 0,
            plain_store: true,
            pixels: std::sync::Arc::new([color, 10, 20, 255].repeat((W * H) as usize)),
            opaque_hashes: None,
            revision: 1,
            damage: None,
            last_used: next_native_capture_use(),
        };
        let make_tiled = |color: u8| NativeTiledCache {
            layer: 1,
            store_instance: 1,
            view_generation: 1,
            ca_revision: 0,
            descendant_revision: 0,
            overlay_revision: 0,
            tiles: std::sync::Arc::new(
                NativePixelTiles::from_opaque_bgra(
                    W,
                    H,
                    &[color, 10, 20, 255].repeat((W * H) as usize),
                )
                .unwrap(),
            ),
            damage: None,
            last_used: next_native_capture_use(),
        };
        let mut flat = HashMap::new();
        let mut tiled = HashMap::new();
        let hot_flat = key(0x111);
        let hot_tiled = key(0x222);
        let mut held = Vec::new();
        flat.insert(hot_flat, make_flat(30));
        tiled.insert(hot_tiled, make_tiled(40));
        let flat_reader = std::sync::Arc::clone(&flat[&hot_flat].pixels);
        let tiled_reader = std::sync::Arc::clone(&tiled[&hot_tiled].tiles);
        let mut old_cold = None;
        let start = std::time::Instant::now();
        for n in 0..48usize {
            // Revisit both hot captures before a new cold publication, even
            // if they belong to different cache representations.
            flat.get_mut(&hot_flat).unwrap().last_used = next_native_capture_use();
            tiled.get_mut(&hot_tiled).unwrap().last_used = next_native_capture_use();
            let cold = key(0x400 + n);
            if n % 2 == 0 {
                flat.insert(cold, make_flat(n as u8));
                held.push(std::sync::Arc::clone(&flat[&cold].pixels));
            } else {
                tiled.insert(cold, make_tiled(n as u8));
            }
            trim_native_capture_maps(
                &mut flat,
                &mut tiled,
                (
                    if n % 2 == 0 {
                        NativeCacheKind::Flat
                    } else {
                        NativeCacheKind::Tiled
                    },
                    cold,
                ),
                CAPACITY,
            );
            assert!(
                flat.contains_key(&hot_flat),
                "hot flat source evicted in round {n}"
            );
            assert!(
                tiled.contains_key(&hot_tiled),
                "hot tiled source evicted in round {n}"
            );
            assert!(
                old_cold.is_none_or(|key| !flat.contains_key(&key) && !tiled.contains_key(&key)),
                "old cold capture not evicted in round {n}"
            );
            assert!(
                flat.values().map(|v| v.pixels.len()).sum::<usize>()
                    + tiled
                        .values()
                        .map(|v| v.tiles.retained_bytes())
                        .sum::<usize>()
                    <= CAPACITY
            );
            old_cold = Some(cold);
        }
        eprintln!(
            "48 alternating 2MiB cache publications with hot flat+tiled reuse: {:?}",
            start.elapsed()
        );
        assert_eq!(&flat_reader[..4], &[30, 10, 20, 255]);
        let mut previous = [0; 4];
        tiled_reader
            .copy_rect_to(10, 10, 1, 1, &mut previous)
            .unwrap();
        assert_eq!(previous, [40, 10, 20, 255]);
        assert_eq!(held.len(), 24);
        assert_eq!(
            &held[0][..4],
            &[0, 10, 20, 255],
            "evicting a cold entry invalidated an Arc retained by a reader"
        );
    }

    #[test]
    fn flat_and_tiled_cache_hits_update_shared_recency_clock() {
        crate::bootstrap();
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        const W: u32 = 512;
        const H: u32 = 512;
        set_native_view_pixels_for_tests(view, W, H, [7, 17, 37, 255].repeat((W * H) as usize));
        let crop = Rect {
            x: 0,
            y: 0,
            width: W as i64,
            height: H as i64,
        };
        let key = (view as usize, 0, 0, W, H, W, H, false, 0);
        let first = native_scene_pixels_cropped(view, W, H, crop).unwrap();
        let tiled = native_scene_tiled_pixels_cropped_with_damage(view, W, H, crop).unwrap();
        assert_eq!(tiled.0.retained_bytes(), (W * H * 4) as usize);
        let before = NATIVE_TILED_CAPTURES.lock().unwrap()[&key].last_used;
        let flat_again = native_scene_pixels_cropped(view, W, H, crop).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first.0, &flat_again.0));
        assert!(NATIVE_CAPTURES.lock().unwrap()[&key].last_used > before);
        let after = NATIVE_CAPTURES.lock().unwrap()[&key].last_used;
        let tiled_again = native_scene_tiled_pixels_cropped_with_damage(view, W, H, crop).unwrap();
        assert!(std::sync::Arc::ptr_eq(&tiled.0, &tiled_again.0));
        assert!(NATIVE_TILED_CAPTURES.lock().unwrap()[&key].last_used > after);
        drop((first, tiled, flat_again, tiled_again));
        forget(view);
        objc::arc::release(view);
    }

    #[test]
    fn native_tiled_cache_distinguishes_replaced_store_with_reused_generation() {
        crate::bootstrap();
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        const W: u32 = 512;
        const H: u32 = 512;
        let mut red = Store::new(W, H);
        red.pixels = [0u8, 0, 255, 255].repeat((W * H) as usize);
        red.touch();
        let old_generation = red.generation;
        let old_instance = red.instance;
        VIEW_STORES
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(view as usize, red);
        let crop = Rect {
            x: 0,
            y: 0,
            width: W as i64,
            height: H as i64,
        };
        let first = native_scene_tiled_pixels_cropped_with_damage(view, W, H, crop).unwrap();
        let mut first_sample = [0u8; 4];
        first
            .0
            .copy_rect_to(40, 23, 1, 1, &mut first_sample)
            .unwrap();
        assert_eq!(first_sample, [0, 0, 255, 255]);
        let mut blue = Store::new(W, H);
        blue.pixels = [255u8, 0, 0, 255].repeat((W * H) as usize);
        blue.touch();
        assert_eq!(blue.generation, old_generation);
        assert_ne!(blue.instance, old_instance);
        VIEW_STORES
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .insert(view as usize, blue);
        let second = native_scene_tiled_pixels_cropped_with_damage(view, W, H, crop).unwrap();
        let mut sample = [0u8; 4];
        second.0.copy_rect_to(40, 23, 1, 1, &mut sample).unwrap();
        assert_eq!(sample, [255, 0, 0, 255]);
        assert_ne!(second.1, first.1);
        let mut old_sample = [0u8; 4];
        first.0.copy_rect_to(40, 23, 1, 1, &mut old_sample).unwrap();
        assert_eq!(old_sample, [0, 0, 255, 255]);
        assert!(second.2.is_none(), "replacement is a new complete source");
        drop(first);
        drop(second);
        forget(view);
        objc::arc::release(view);
    }

    #[test]
    fn identical_native_row_band_damage_reuses_all_reader_allocations() {
        const W: usize = 512;
        const H: usize = 128;
        let mut source = Store::new(W as u32, H as u32);
        source.pixels = [17u8, 31, 79, 255].repeat(W * H);
        source.touch();
        let crop = Rect {
            x: 0,
            y: 0,
            width: W as i64,
            height: H as i64,
        };
        let original = NativePixelTiles::new_from_opaque_store(&source, crop).unwrap();
        let retained = original.clone();
        let identical = Rect {
            x: 19,
            y: 40,
            width: 1,
            height: 1,
        };
        // Real producers can invalidate an unchanged pixel every frame.
        // The output must reuse every band, including the supposedly dirty one.
        let started = std::time::Instant::now();
        let mut current = original.clone();
        for _ in 0..200 {
            source.touch_region(identical);
            let next = current
                .patched_regions(&source, crop, &[identical])
                .unwrap();
            assert_eq!(next.revision, original.revision);
            assert!(
                next.bands
                    .iter()
                    .zip(&original.bands)
                    .all(|(a, b)| { std::sync::Arc::ptr_eq(a, b) })
            );
            current = next;
        }
        eprintln!(
            "200 unchanged 1px row-band updates: {:?}",
            started.elapsed()
        );
        // A batch with one genuine edit and one identical region clones only
        // the genuinely touched band; old GPU readers still see old pixels.
        let changed = Rect {
            x: 11,
            y: 72,
            width: 1,
            height: 1,
        };
        let at = (72 * W + 11) * 4;
        source.pixels[at] = 201;
        source.touch_region(changed);
        let patched = current
            .patched_regions(&source, crop, &[identical, changed])
            .unwrap();
        assert_ne!(patched.revision, original.revision);
        assert_eq!(patched.bands.len(), original.bands.len());
        for index in 0..patched.bands.len() {
            assert_eq!(
                std::sync::Arc::ptr_eq(&patched.bands[index], &original.bands[index]),
                index != 2,
                "only the truly changed band may be cloned"
            );
        }
        let mut old_pixel = [0u8; 4];
        let mut new_pixel = [0u8; 4];
        retained.copy_rect_to(11, 72, 1, 1, &mut old_pixel).unwrap();
        patched.copy_rect_to(11, 72, 1, 1, &mut new_pixel).unwrap();
        assert_eq!(old_pixel, [17, 31, 79, 255]);
        assert_eq!(new_pixel, [201, 31, 79, 255]);
        // Repeating the already applied edit must not copy a second band.
        let same_again = patched.patched_regions(&source, crop, &[changed]).unwrap();
        assert_eq!(same_again.revision, patched.revision);
        assert!(
            same_again
                .bands
                .iter()
                .zip(&patched.bands)
                .all(|(a, b)| { std::sync::Arc::ptr_eq(a, b) })
        );
    }

    #[test]
    fn native_row_bands_preserve_unmodified_chunks_and_full_content_identity() {
        let width = 128;
        let height = 100;
        let crop = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut producer = Store::new(width as u32, height as u32);
        producer.pixels = [11u8, 22, 33, 255].repeat(width as usize * height as usize);
        producer.touch();
        let old = NativePixelTiles::new_from_opaque_store(&producer, crop).unwrap();
        assert_eq!(old.bands.len(), 4);
        assert!(old.bands.iter().all(|b| b.len() <= 128 * 32 * 4));
        for (x, y, blue) in [(7usize, 2usize, 60u8), (75, 77, 90)] {
            let offset = (y * width as usize + x) * 4;
            producer.pixels[offset] = blue;
            producer.touch_region(Rect {
                x: x as i64,
                y: y as i64,
                width: 1,
                height: 1,
            });
        }
        let changes = producer.damage_regions_since(1).unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(
            changes[0],
            Rect {
                x: 7,
                y: 2,
                width: 1,
                height: 1
            }
        );
        assert_eq!(
            changes[1],
            Rect {
                x: 75,
                y: 77,
                width: 1,
                height: 1
            }
        );
        let updated = old.patched_regions(&producer, crop, &changes).unwrap();
        assert!(std::sync::Arc::ptr_eq(&old.bands[1], &updated.bands[1]));
        assert!(!std::sync::Arc::ptr_eq(&old.bands[0], &updated.bands[0]));
        assert!(!std::sync::Arc::ptr_eq(&old.bands[2], &updated.bands[2]));
        assert!(std::sync::Arc::ptr_eq(&old.bands[3], &updated.bands[3]));
        let full = NativePixelTiles::new_from_opaque_store(&producer, crop).unwrap();
        assert_eq!(full.revision, updated.revision);
        let mut copied = vec![0; updated.len()];
        updated
            .copy_rect_to(0, 0, width as u32, height as u32, &mut copied)
            .unwrap();
        assert_eq!(copied, producer.pixels);
        let mut unchanged = vec![0; old.len()];
        old.copy_rect_to(0, 0, width as u32, height as u32, &mut unchanged)
            .unwrap();
        assert_eq!(unchanged[(2 * 128 + 7) * 4], 11);
        assert_eq!(unchanged[(77 * 128 + 75) * 4], 11);
        assert_eq!(full.bands.len(), 4);
        let no_change = updated.patched_regions(&producer, crop, &changes).unwrap();
        assert_eq!(no_change.revision, updated.revision);
        assert_eq!(
            producer.damage_since(1).unwrap(),
            Rect {
                x: 7,
                y: 2,
                width: 69,
                height: 76
            }
        );
    }

    #[test]
    fn flat_native_capture_rejects_replaced_store_even_with_reused_generation() {
        crate::bootstrap();
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        const W: usize = 128;
        const H: usize = 128;
        let crop = Rect {
            x: 0,
            y: 0,
            width: 128,
            height: 128,
        };
        let mut red = Store::new(W as u32, H as u32);
        red.pixels = [0u8, 0, 255, 255].repeat(W * H);
        red.touch();
        VIEW_STORES
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(view as usize, red);
        let old = native_scene_pixels_cropped_with_damage(view, W as u32, H as u32, crop).unwrap();
        assert_eq!(&old.0[0..4], &[0, 0, 255, 255]);

        // Identical generation must not authenticate a different Store.
        let mut blue = Store::new(W as u32, H as u32);
        blue.pixels = [255u8, 0, 0, 255].repeat(W * H);
        blue.touch();
        VIEW_STORES
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .insert(view as usize, blue);
        let replaced =
            native_scene_pixels_cropped_with_damage(view, W as u32, H as u32, crop).unwrap();
        assert_eq!(&replaced.0[0..4], &[255, 0, 0, 255]);
        assert_ne!(old.1, replaced.1);
        assert!(
            replaced.2.is_none(),
            "replaced Store cannot assert incremental provenance"
        );

        // Also replace immediately *before* a generation-2 dirty patch;
        // otherwise the new Store's journal would authorize mixing blue
        // backing with unrelated old red snapshot bytes.
        let mut fresh = Store::new(W as u32, H as u32);
        fresh.pixels = [10u8, 20, 30, 255].repeat(W * H);
        fresh.touch(); // generation 1
        let offset = (99 * W + 100) * 4;
        fresh.pixels[offset..offset + 4].copy_from_slice(&[42, 55, 66, 255]);
        fresh.touch_region(Rect {
            x: 100,
            y: 99,
            width: 1,
            height: 1,
        }); // generation 2
        VIEW_STORES
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .insert(view as usize, fresh);
        let patched =
            native_scene_pixels_cropped_with_damage(view, W as u32, H as u32, crop).unwrap();
        assert_eq!(&patched.0[0..4], &[10, 20, 30, 255]);
        assert_eq!(&patched.0[offset..offset + 4], &[42, 55, 66, 255]);
        assert_eq!(&replaced.0[0..4], &[255, 0, 0, 255]);
        assert!(
            patched.2.is_none(),
            "cross-Store damage was presented as a valid patch"
        );

        // Off-crop advancement must also reject the new Store identity.
        let left = Rect {
            x: 0,
            y: 0,
            width: 64,
            height: 128,
        };
        let first_left = native_scene_pixels_cropped_with_damage(view, 128, 128, left).unwrap();
        let mut fourth = Store::new(W as u32, H as u32);
        fourth.pixels = [99u8, 11, 22, 255].repeat(W * H);
        fourth.touch();
        fourth.pixels[(100 * W + 110) * 4] = 77;
        fourth.touch_region(Rect {
            x: 110,
            y: 100,
            width: 1,
            height: 1,
        });
        VIEW_STORES
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .insert(view as usize, fourth);
        let next_left = native_scene_pixels_cropped_with_damage(view, 128, 128, left).unwrap();
        assert_eq!(&next_left.0[0..4], &[99, 11, 22, 255]);
        assert!(next_left.2.is_none());
        assert_eq!(&first_left.0[0..4], &[10, 20, 30, 255]);
        drop((old, replaced, patched, first_left, next_left));
        forget(view);
        objc::arc::release(view);
    }

    #[test]
    fn mixed_dma_overlay_is_ineligible_for_row_banded_cpu_capture() {
        crate::bootstrap();
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        const W: u32 = 512;
        const H: u32 = 512;
        let mut store = Store::new(W, H);
        store.pixels = [255u8, 0, 0, 255].repeat((W * H) as usize);
        store.touch();
        VIEW_STORES
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(view as usize, store);
        let rect = Rect {
            x: 0,
            y: 0,
            width: W as i64,
            height: H as i64,
        };
        assert!(native_scene_tiled_pixels_cropped_with_damage(view, W, H, rect).is_some());
        let dma = std::sync::Arc::new(Dmabuf {
            width: W,
            height: H,
            fourcc: 0,
            modifier: 0,
            planes: Vec::new(),
        });
        set_overlay(
            view,
            Overlay {
                width: W,
                height: H,
                picture: Picture::Dmabuf(dma),
            },
        );
        assert!(native_dma_overlay(view).is_some());
        assert!(
            native_scene_tiled_pixels_cropped_with_damage(view, W, H, rect).is_none(),
            "a mixed CPU/DMA source must use the original flat CPU background"
        );
        let cpu = native_scene_pixels_cropped_with_damage(view, W, H, rect).unwrap();
        assert_eq!(cpu.0.len(), (W * H * 4) as usize);
        assert_eq!(&cpu.0[..4], &[255, 0, 0, 255]);
        remove_overlay(view);
        assert!(native_scene_tiled_pixels_cropped_with_damage(view, W, H, rect).is_some());
        forget(view);
        objc::arc::release(view);
    }

    #[test]
    fn incremental_plain_capture_keeps_gpu_readers_immutable_and_hashes_only_damage() {
        crate::bootstrap();
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let mut store = Store::new(130, 140);
        store.pixels = [12, 34, 56, 255].repeat(130 * 140);
        store.touch();
        VIEW_STORES
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(view as usize, store);
        let crop = Rect {
            x: 5,
            y: 7,
            width: 117,
            height: 122,
        };
        let first = native_scene_pixels_cropped_with_damage(view, 130, 140, crop).unwrap();
        let original = std::sync::Arc::clone(&first.0); // GPU reader
        let baseline = original.as_ref().clone();
        let count = NATIVE_INCREMENTAL_PATCHES.with(Cell::get);
        {
            let mut sources = VIEW_STORES.lock().unwrap();
            let src = sources.as_mut().unwrap().get_mut(&(view as usize)).unwrap();
            for (x, y, pixel) in [
                (6, 8, [3, 8, 19, 255]),
                (60, 75, [99, 88, 77, 255]),
                (121, 128, [2, 22, 222, 255]),
            ] {
                let at = (y * 130 + x) * 4;
                src.pixels[at..at + 4].copy_from_slice(&pixel);
                src.touch_region(Rect {
                    x: x as i64,
                    y: y as i64,
                    width: 1,
                    height: 1,
                });
            }
        }
        let updated = native_scene_pixels_cropped_with_damage(view, 130, 140, crop).unwrap();
        assert_eq!(
            original.as_ref(),
            &baseline,
            "GPU reader's source was mutated"
        );
        assert!(!std::sync::Arc::ptr_eq(&original, &updated.0));
        assert_ne!(updated.1, first.1);
        assert_eq!(updated.2.unwrap().from_revision, first.1);
        assert_eq!(NATIVE_INCREMENTAL_PATCHES.with(Cell::get), count + 1);
        for (x, y, pixel) in [
            (6usize, 8usize, [3, 8, 19, 255]),
            (60, 75, [99, 88, 77, 255]),
            (121, 128, [2, 22, 222, 255]),
        ] {
            let at = (((y - 7) * 117) + (x - 5)) * 4;
            assert_eq!(&updated.0[at..at + 4], &pixel);
        }
        // Marking identical texels as dirty must retain content identity.
        {
            let mut sources = VIEW_STORES.lock().unwrap();
            sources
                .as_mut()
                .unwrap()
                .get_mut(&(view as usize))
                .unwrap()
                .touch_region(Rect {
                    x: 60,
                    y: 75,
                    width: 1,
                    height: 1,
                });
        }
        let same = native_scene_pixels_cropped_with_damage(view, 130, 140, crop).unwrap();
        assert_eq!(same.1, updated.1);
        assert_eq!(same.2, updated.2, "unchanged texels dropped staged damage");
        drop(first);
        drop(original);
        drop(updated);
        drop(same);
        forget(view);
        objc::arc::release(view);
    }

    #[test]
    fn incremental_plain_capture_rejects_alpha_change_and_incomplete_journal() {
        crate::bootstrap();
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let mut store = Store::new(24, 16);
        store.pixels = [44, 55, 66, 255].repeat(24 * 16);
        store.touch();
        VIEW_STORES
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(view as usize, store);
        let crop = Rect {
            x: 0,
            y: 0,
            width: 24,
            height: 16,
        };
        let old = native_scene_pixels_cropped_with_damage(view, 24, 16, crop).unwrap();
        let patches = NATIVE_INCREMENTAL_PATCHES.with(Cell::get);
        {
            let mut views = VIEW_STORES.lock().unwrap();
            let src = views.as_mut().unwrap().get_mut(&(view as usize)).unwrap();
            src.pixels[(3 * 24 + 7) * 4..(3 * 24 + 7) * 4 + 4].copy_from_slice(&[25, 50, 75, 128]);
            src.touch_region(Rect {
                x: 7,
                y: 3,
                width: 1,
                height: 1,
            });
        }
        let alpha = native_scene_pixels_cropped_with_damage(view, 24, 16, crop).unwrap();
        assert_eq!(NATIVE_INCREMENTAL_PATCHES.with(Cell::get), patches);
        assert_eq!(
            &alpha.0[(3 * 24 + 7) * 4..(3 * 24 + 7) * 4 + 4],
            &[25, 50, 75, 128]
        );
        assert_eq!(old.0[(3 * 24 + 7) * 4 + 3], 255);
        {
            let mut views = VIEW_STORES.lock().unwrap();
            let src = views.as_mut().unwrap().get_mut(&(view as usize)).unwrap();
            src.pixels[(3 * 24 + 7) * 4..(3 * 24 + 7) * 4 + 4].copy_from_slice(&[25, 50, 75, 255]);
            src.touch_region(Rect {
                x: 7,
                y: 3,
                width: 1,
                height: 1,
            });
        }
        let opaque = native_scene_pixels_cropped_with_damage(view, 24, 16, crop).unwrap();
        assert_eq!(opaque.0[(3 * 24 + 7) * 4 + 3], 255);
        assert_eq!(
            NATIVE_INCREMENTAL_PATCHES.with(Cell::get),
            patches,
            "no opaque chunk-hash cache existed after translucent capture"
        );
        {
            let mut views = VIEW_STORES.lock().unwrap();
            let src = views.as_mut().unwrap().get_mut(&(view as usize)).unwrap();
            for index in 0..36 {
                let at = (index % (24 * 16)) * 4;
                src.pixels[at] = index as u8;
                src.touch_region(Rect {
                    x: ((index / 4) % 24) as i64,
                    y: (index % 4) as i64,
                    width: 1,
                    height: 1,
                });
            }
        }
        let recovered = native_scene_pixels_cropped_with_damage(view, 24, 16, crop).unwrap();
        assert_eq!(
            NATIVE_INCREMENTAL_PATCHES.with(Cell::get),
            patches,
            "a lost producer generation must fall back to full capture"
        );
        assert!(recovered.0[0] != opaque.0[0]);
        drop(old);
        drop(alpha);
        drop(opaque);
        drop(recovered);
        forget(view);
        objc::arc::release(view);
    }

    #[test]
    fn plain_view_capture_carries_exact_dirty_pixel_provenance() {
        crate::bootstrap();
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let mut store = Store::new(8, 6);
        store.pixels = [255, 0, 0, 255].repeat(8 * 6);
        store.touch();
        VIEW_STORES
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(view as usize, store);
        let crop = Rect {
            x: 2,
            y: 1,
            width: 4,
            height: 3,
        };
        let first = native_scene_pixels_cropped_with_damage(view, 8, 6, crop).unwrap();
        assert_eq!(first.2, None);
        {
            let mut stores = VIEW_STORES.lock().unwrap();
            let store = stores.as_mut().unwrap().get_mut(&(view as usize)).unwrap();
            let at = (2 * 8 + 4) * 4;
            store.pixels[at..at + 4].copy_from_slice(&[0, 255, 0, 255]);
            store.touch_region(Rect {
                x: 4,
                y: 2,
                width: 1,
                height: 1,
            });
        }
        let second = native_scene_pixels_cropped_with_damage(view, 8, 6, crop).unwrap();
        assert_eq!(
            second.2,
            Some(NativeCpuDamage {
                from_revision: first.1,
                rect: Rect {
                    x: 2,
                    y: 1,
                    width: 1,
                    height: 1
                },
            })
        );
        assert_eq!(&second.0[(4 + 2) * 4..(4 + 2) * 4 + 4], &[0, 255, 0, 255]);
        {
            let mut stores = VIEW_STORES.lock().unwrap();
            let store = stores.as_mut().unwrap().get_mut(&(view as usize)).unwrap();
            store.pixels.fill(0);
            store.touch();
        }
        let third = native_scene_pixels_cropped_with_damage(view, 8, 6, crop);
        assert!(
            third.is_none(),
            "fully transparent source is not emitted as foreground"
        );
        forget(view);
        objc::arc::release(view);
    }

    #[test]
    fn failed_dma_producer_only_disables_that_live_view_image() {
        use std::os::fd::OwnedFd;
        let window = 0x7d48usize as sdl3::Window;
        let bad_view = 0x7d49usize;
        let good_view = 0x7d50usize;
        let picture = || {
            std::sync::Arc::new(Dmabuf {
                width: 2,
                height: 2,
                fourcc: u32::from_le_bytes(*b"AR24"),
                modifier: 0,
                planes: vec![DmabufPlane {
                    fd: OwnedFd::from(std::fs::File::open("/dev/null").unwrap()),
                    offset: 0,
                    stride: 8,
                }],
            })
        };
        let bad = picture();
        let good = picture();
        GPU_WINDOWS.lock().unwrap().push((window as usize, false));
        {
            let mut overlays = OVERLAYS.lock().unwrap();
            overlays.push((
                bad_view,
                Overlay {
                    width: 2,
                    height: 2,
                    picture: Picture::Dmabuf(bad.clone()),
                },
            ));
            overlays.push((
                good_view,
                Overlay {
                    width: 2,
                    height: 2,
                    picture: Picture::Dmabuf(good.clone()),
                },
            ));
        }
        assert!(shows_dmabufs_for_view(window, bad_view));
        refuse_dmabuf_producer(window, bad_view, &bad);
        refuse_dmabuf_producer(window, bad_view, &bad);
        assert!(dmabuf_producer_refused(window, bad_view, &bad));
        assert!(!shows_dmabufs_for_view(window, bad_view));
        assert!(shows_dmabufs_for_view(window, good_view));
        assert!(!dmabuf_producer_refused(window, good_view, &good));
        {
            let mut overlays = OVERLAYS.lock().unwrap();
            let source = overlays.iter_mut().find(|(id, _)| *id == bad_view).unwrap();
            source.1.picture = Picture::Dmabuf(good.clone());
        }
        assert!(
            shows_dmabufs_for_view(window, bad_view),
            "a new producer on the same NSView must be eligible"
        );
        OVERLAYS
            .lock()
            .unwrap()
            .retain(|(id, _)| *id != bad_view && *id != good_view);
        forget_window(window);
        assert!(!dmabuf_producer_refused(window, bad_view, &bad));
    }

    #[test]
    fn deferred_backing_store_survives_the_pump_while_another_window_progresses() {
        fn presenter(
            window: sdl3::Window,
            _: &Store,
            _: &[Hole],
            _: &[GpuOverlay],
        ) -> PresentOutcome {
            match window as usize {
                0x71a1 => PresentOutcome::Deferred,
                0x71c3 => PresentOutcome::DisplayedEarlier,
                _ => PresentOutcome::Displayed,
            }
        }
        let before_presenter = PRESENTER.lock().expect("presenter").replace(presenter);
        let mut deferred = Store::new(2, 2);
        deferred.dirty = true;
        let mut independent = Store::new(2, 2);
        independent.dirty = true;
        let mut uploading = Store::new(2, 2);
        uploading.dirty = true;
        let before_stores = {
            let mut stores = STORES.lock().expect("stores");
            (*stores).replace(HashMap::from([
                (0x71a1usize, deferred),
                (0x71b2usize, independent),
                (0x71c3usize, uploading),
            ]))
        };
        present_dirty();
        {
            let stores = STORES.lock().expect("stores");
            let stores = stores.as_ref().expect("stores");
            assert!(
                stores[&0x71a1].dirty,
                "busy A must stay dirty for another event turn"
            );
            assert!(
                !stores[&0x71b2].dirty,
                "ready B must be presented on this turn"
            );
            assert!(
                stores[&0x71c3].dirty,
                "C's current store has not reached the screen yet"
            );
        }
        assert!(
            shows_dmabufs(0x71c3 as sdl3::Window),
            "C's earlier store was shown through the GPU"
        );
        *STORES.lock().expect("stores") = before_stores;
        *PRESENTER.lock().expect("presenter") = before_presenter;
        GPU_WINDOWS
            .lock()
            .expect("gpu windows")
            .retain(|(key, _)| *key != 0x71b2 && *key != 0x71c3);
    }

    fn set_thread_context_from_class(context: Id) {
        let class = objc::class::lookup(c"NSGraphicsContext").expect("declared");
        // SAFETY: `msg::entry` has the registered class-method ABI below.
        let send: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        // SAFETY: the class and selector identify the registered setter.
        unsafe {
            send(
                objc::class::ptr(class).cast(),
                objc::sel::get("setCurrentContext:"),
                context,
            )
        }
    }

    fn thread_context_from_class() -> Id {
        let class = objc::class::lookup(c"NSGraphicsContext").expect("declared");
        // SAFETY: `msg::entry` has the registered class-method ABI below.
        let send: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        // SAFETY: the class and selector identify the registered getter.
        unsafe {
            send(
                objc::class::ptr(class).cast(),
                objc::sel::get("currentContext"),
            )
        }
    }

    #[test]
    fn setting_current_graphics_context_retains_it_until_cleared() {
        crate::bootstrap();
        let pool = unsafe { objc::api::objc_autoreleasePoolPush() };
        let class = objc::class::lookup(c"NSGraphicsContext").expect("declared");
        let context = objc::arc::autorelease(objc::object::create_instance(class, 0));

        set_thread_context_from_class(context);
        assert_eq!(thread_context_from_class(), context);

        // The current-context state owns a reference beyond the autorelease
        // pool that would otherwise release this object.
        unsafe { objc::api::objc_autoreleasePoolPop(pool) };
        assert_eq!(thread_context_from_class(), context);

        set_thread_context_from_class(std::ptr::null_mut());
        assert!(thread_context_from_class().is_null());
    }

    #[test]
    fn explicit_nil_does_not_disable_the_focused_context_later() {
        crate::bootstrap();
        set_thread_context_from_class(std::ptr::null_mut());
        assert!(thread_context_from_class().is_null());

        begin_offscreen(0x7e57, 4, 4, 1.0, false);
        assert!(!thread_context_from_class().is_null());
        let class = objc::class::lookup(c"NSGraphicsContext").expect("declared");
        let bitmap = objc::object::create_instance(class, 0);
        set_thread_context_from_class(bitmap);
        assert_eq!(thread_context_from_class(), bitmap);
        end_offscreen(0x7e57);

        // The override ended with its focus.
        assert!(thread_context_from_class().is_null());
        begin_offscreen(0x7e58, 4, 4, 1.0, false);
        assert!(!thread_context_from_class().is_null());
        assert_ne!(thread_context_from_class(), bitmap);
        end_offscreen(0x7e58);
    }

    #[test]
    fn current_graphics_context_is_per_thread() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSGraphicsContext").expect("declared");
        let context = objc::object::create_instance(class, 0);
        set_thread_context_from_class(context);
        std::thread::spawn(|| assert!(thread_context_from_class().is_null()))
            .join()
            .unwrap();
        assert_eq!(thread_context_from_class(), context);
        set_thread_context_from_class(std::ptr::null_mut());
    }

    /// A view of `NSView`'s, and one of a flipped subclass, so a tree can be
    /// placed without a window.
    fn view(frame: NSRect, flipped: bool) -> Id {
        crate::bootstrap();
        let name = match flipped {
            true => c"RosemaryBackingFlippedView",
            false => c"NSView",
        };
        let class = objc::class::lookup(name).unwrap_or_else(|| {
            let class = corefoundation::base::register(
                name,
                objc::class::lookup(c"NSView"),
                crate::view::INSTANCE_SIZE,
                false,
            );
            methods!(class, {
                "isFlipped" => flipped_yes, c"c16@0:8";
            });
            class
        });
        let object = objc::object::create_instance(class, 0);
        // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)` on a view.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, NSRect) -> Id =
                std::mem::transmute(objc::msg::entry());
            send(object, objc::sel::get("initWithFrame:"), frame)
        }
    }

    unsafe extern "C" fn flipped_yes(_receiver: Id, _sel: Sel) -> u8 {
        1
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect {
            origin: NSPoint { x, y },
            size: NSSize { width, height },
        }
    }

    fn add(parent: Id, child: Id) {
        // SAFETY: `-addSubview:` is `void (id, SEL, id)` on views.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            send(parent, objc::sel::get("addSubview:"), child);
        }
    }

    /// Where the compositor puts each view, from a flipped tree: a subview at
    /// the top of its superview is at the top of the window, and one that
    /// hangs over an edge shows only the part inside.
    #[test]
    fn places_a_flipped_tree_from_the_top_of_the_window() {
        let content = view(rect(0.0, 0.0, 200.0, 100.0), true);
        let ribbon = view(rect(0.0, 0.0, 200.0, 20.0), true);
        let overhanging = view(rect(150.0, 10.0, 100.0, 5.0), true);
        add(content, ribbon);
        add(ribbon, overhanging);

        let mut placed = Vec::new();
        let whole = Rect {
            x: 0,
            y: 0,
            width: 200,
            height: 100,
        };
        place_view(
            content,
            rect(0.0, 0.0, 200.0, 100.0),
            false,
            whole,
            Geometry {
                window_height: 100.0,
                host_points: (200, 100),
                host_pixels: (200, 100),
            },
            0,
            &mut placed,
        );
        let rects: Vec<Rect> = placed.iter().map(|p| p.rect).collect();
        assert_eq!(rects[0], whole);
        assert_eq!(
            rects[1],
            Rect {
                x: 0,
                y: 0,
                width: 200,
                height: 20
            }
        );
        assert_eq!(
            rects[2],
            Rect {
                x: 150,
                y: 10,
                width: 100,
                height: 5
            }
        );
        // Clipped to its superview, which is what keeps a scrolled view's
        // children inside it.
        assert_eq!(
            placed[2].clip,
            Rect {
                x: 150,
                y: 10,
                width: 50,
                height: 5
            }
        );
    }

    /// A hidden view is placed nowhere, and takes its subviews with it.
    #[test]
    fn a_hidden_view_is_not_placed() {
        let content = view(rect(0.0, 0.0, 10.0, 10.0), true);
        let hidden = view(rect(0.0, 0.0, 10.0, 5.0), true);
        let under = view(rect(0.0, 0.0, 10.0, 2.0), true);
        add(content, hidden);
        add(hidden, under);
        // SAFETY: `-setHidden:` is `void (id, SEL, BOOL)` on a view.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, u8) = std::mem::transmute(objc::msg::entry());
            send(hidden, objc::sel::get("setHidden:"), 1);
        }
        let mut placed = Vec::new();
        place_view(
            content,
            rect(0.0, 0.0, 10.0, 10.0),
            false,
            Rect {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
            Geometry {
                window_height: 10.0,
                host_points: (10, 10),
                host_pixels: (10, 10),
            },
            0,
            &mut placed,
        );
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].view, content);
    }

    /// A subview's pixels are over its superview's, whichever drew last: one
    /// store per window would instead show whichever flushed last.
    #[test]
    fn a_subviews_pixels_are_over_its_superviews() {
        let mut window = store_of(2, 1);
        let mut parent = store_of(2, 1);
        parent.pixels = vec![1, 1, 1, 255, 1, 1, 1, 255];
        let mut child = store_of(1, 1);
        child.pixels = vec![9, 9, 9, 255];
        let whole = Rect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        blend_at(&mut window, &parent, 0, 0, whole, 1.0);
        blend_at(&mut window, &child, 1, 0, whole, 1.0);
        assert_eq!(window.pixels, [1, 1, 1, 255, 9, 9, 9, 255]);
    }

    /// `-alphaValue` fades a view's pixels into what is under them.
    #[test]
    fn a_translucent_views_pixels_are_faded_over_the_store() {
        let mut window = store_of(2, 1);
        window.pixels = vec![0, 0, 200, 255, 0, 0, 200, 255];
        let mut view = store_of(2, 1);
        view.pixels = vec![200, 0, 0, 255, 200, 0, 0, 255];
        let whole = Rect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        blend_at(&mut window, &view, 0, 0, whole, 0.5);
        assert_eq!(window.pixels[..4], [100, 0, 100, 255]);
        let before = window.pixels.clone();
        blend_at(&mut window, &view, 0, 0, whole, 0.0);
        assert_eq!(window.pixels, before);
    }

    fn store_of(width: u32, height: u32) -> Store {
        Store {
            instance: NEXT_STORE_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            origin: (0, 0),
            pixels: vec![0; width as usize * height as usize * 4],
            dirty: false,
            generation: 0,
            changes: VecDeque::new(),
            alpha_class: AtomicU8::new(ALPHA_UNCLASSIFIED),
        }
    }

    #[test]
    fn native_capture_test_counters_ignore_parallel_thread_work() {
        let before_scan = opaque_alpha_scan_count_for_tests();
        let before_render = native_render_count_for_tests();
        let before_patches = NATIVE_INCREMENTAL_PATCHES.with(Cell::get);
        let worker = std::thread::spawn(|| {
            let mut store = Store::new(16, 8);
            store
                .pixels
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .for_each(|pixel| pixel[3] = 255);
            assert!(store.fully_opaque());
            assert_eq!(opaque_alpha_scan_count_for_tests(), 1);
            NATIVE_RENDER_COUNT.with(|count| count.set(count.get() + 1));
            NATIVE_INCREMENTAL_PATCHES.with(|count| count.set(count.get() + 1));
        });
        worker.join().unwrap();
        assert_eq!(opaque_alpha_scan_count_for_tests(), before_scan);
        assert_eq!(native_render_count_for_tests(), before_render);
        assert_eq!(NATIVE_INCREMENTAL_PATCHES.with(Cell::get), before_patches);
    }

    #[test]
    fn opaque_classification_is_cached_until_pixels_or_extent_change() {
        if !sdl3::isolated::run_here() {
            return;
        }
        let mut store = Store::new(32, 12);
        let scanned = opaque_alpha_scan_count_for_tests();
        for _ in 0..100 {
            assert!(!store.fully_opaque());
        }
        assert_eq!(opaque_alpha_scan_count_for_tests(), scanned + 1);
        store
            .pixels
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .for_each(|pixel| pixel[3] = 255);
        store.touch();
        for _ in 0..100 {
            assert!(store.fully_opaque());
        }
        assert_eq!(opaque_alpha_scan_count_for_tests(), scanned + 2);
        store.resize(24, 12);
        assert!(store.fully_opaque());
        assert_eq!(opaque_alpha_scan_count_for_tests(), scanned + 3);
        store.resize(33, 12);
        assert!(!store.fully_opaque());
        assert_eq!(opaque_alpha_scan_count_for_tests(), scanned + 4);
    }

    #[test]
    fn copies_a_flipped_view_upright() {
        // Two rows: red on top, blue below, as Qt's image holds them.
        let source = [0, 0, 255, 255, 255, 0, 0, 255];
        let focus = Focus {
            view: std::ptr::null_mut(),
            window: std::ptr::null_mut(),
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 2.0,
            left: 0.0,
            top: 0.0,
            view_height: 2.0,
            density: 1.0,
            flipped: true,
            offscreen: false,
            clip: None,
        };
        let size = NSSize {
            width: 1.0,
            height: 2.0,
        };
        let rect = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size,
        };
        FOCUS.with(|stack| stack.borrow_mut().push(focus));
        let mut store = store_of(1, 2);
        draw_into(&mut store, &source, 1, 2, size, rect, rect, 1, 1.0, true);
        assert_eq!(store.pixels, source);
        let mut mirrored = store_of(1, 2);
        draw_into(
            &mut mirrored,
            &source,
            1,
            2,
            size,
            rect,
            rect,
            1,
            1.0,
            false,
        );
        assert_eq!(mirrored.pixels, [255, 0, 0, 255, 0, 0, 255, 255]);
        FOCUS.with(|stack| stack.borrow_mut().clear());
    }

    #[test]
    fn fractional_backing_fallback_uses_the_same_right_and_bottom_pixel_edges() {
        let clip = crate::window::pixel_clip_from_surface(
            NSRect {
                origin: NSPoint { x: 0.5, y: 0.5 },
                size: NSSize {
                    width: 1438.5,
                    height: 868.5,
                },
            },
            (1440, 900),
            (1440, 900),
        );
        assert_eq!(
            clip,
            sdl3::Rect {
                x: 1,
                y: 1,
                w: 1438,
                h: 868
            }
        );
        let allowed = Rect {
            x: clip.x.into(),
            y: clip.y.into(),
            width: clip.w.into(),
            height: clip.h.into(),
        };
        let mut pixels = vec![0u8; 1440 * 900 * 4];
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[0, 0, 255, 255]);
        }
        blackout_software_exterior(&mut pixels, 1440 * 4, 1440, 900, allowed);
        let at = |x: usize, y: usize| &pixels[(y * 1440 + x) * 4..(y * 1440 + x + 1) * 4];
        assert_eq!(at(1, 1), &[0, 0, 255, 255]);
        assert_eq!(at(1438, 868), &[0, 0, 255, 255]);
        assert_eq!(at(1439, 868), &[0, 0, 0, 255], "rightmost pixel escaped");
        assert_eq!(at(1438, 869), &[0, 0, 0, 255], "bottom pixel escaped");
        assert_eq!(at(0, 1), &[0, 0, 0, 255]);
        assert_eq!(at(1, 0), &[0, 0, 0, 255]);
        let retina = crate::window::pixel_clip_from_surface(
            NSRect {
                origin: NSPoint { x: 0.25, y: 0.25 },
                size: NSSize {
                    width: 99.25,
                    height: 79.25,
                },
            },
            (100, 80),
            (200, 160),
        );
        assert_eq!(
            retina,
            sdl3::Rect {
                x: 1,
                y: 1,
                w: 198,
                h: 158
            }
        );
    }

    #[test]
    fn only_a_logical_frame_blacks_out_the_software_surface() {
        let whole = Rect {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        // A windowed store lagging a grow-resize.
        let shown = Rect {
            x: 0,
            y: 0,
            width: 700,
            height: 500,
        };
        assert_eq!(software_exterior(whole, whole, shown), None);
        let clip = Rect {
            x: 0,
            y: 31,
            width: 800,
            height: 569,
        };
        assert_eq!(
            software_exterior(clip, whole, shown),
            Some(Rect {
                x: 0,
                y: 31,
                width: 700,
                height: 469,
            })
        );
    }

    #[test]
    fn an_overlay_is_scaled_into_its_rectangle_over_the_store() {
        let mut store = store_of(4, 2);
        store.pixels = vec![9; 4 * 2 * 4];
        // One opaque texel and one fully transparent one, stretched over two
        // store pixels each.
        let overlay = Overlay {
            width: 2,
            height: 1,
            picture: Picture::Pixels(vec![1, 2, 3, 255, 0, 0, 0, 0]),
        };
        blend_overlay(&mut store, &overlay, 0, 1, 4, 1);
        assert_eq!(&store.pixels[..16], &[9; 16]);
        assert_eq!(
            &store.pixels[16..],
            &[1, 2, 3, 255, 1, 2, 3, 255, 9, 9, 9, 9, 9, 9, 9, 9]
        );
    }

    #[test]
    fn an_oversized_overlay_is_clipped_without_rescaling_its_source() {
        let mut store = store_of(6, 3);
        let mut source = Vec::new();
        for row in 0..3u8 {
            for column in 0..6u8 {
                source.extend_from_slice(&[column + 10, row + 20, 37, 255]);
            }
        }
        let overlay = Overlay {
            width: 6,
            height: 3,
            picture: Picture::Pixels(source),
        };
        let allowed = Rect {
            x: 2,
            y: 1,
            width: 3,
            height: 1,
        };
        blend_overlay_clipped(&mut store, &overlay, 0, 0, 6, 3, allowed);
        for y in 0..3 {
            for x in 0..6 {
                let pixel = &store.pixels[(y * 6 + x) * 4..(y * 6 + x + 1) * 4];
                if (2..5).contains(&x) && y == 1 {
                    assert_eq!(pixel, &[(x as u8) + 10, 21, 37, 255]);
                } else {
                    assert_eq!(pixel, &[0, 0, 0, 0]);
                }
            }
        }
    }

    #[test]
    fn a_layer_hole_leaves_out_the_overlay_above_it() {
        let rect = |x, y, width, height| Rect {
            x,
            y,
            width,
            height,
        };
        // A web view filling the lower part of a Metal-drawn panel.
        let hole = rect(1, 510, 278, 338);
        let pieces = hole.subtract(&[rect(1, 546, 278, 302)]);
        assert_eq!(pieces, [rect(1, 510, 278, 36)]);
        // One in the middle leaves a band on every side.
        let pieces = rect(0, 0, 10, 10).subtract(&[rect(2, 3, 4, 5)]);
        let area: i64 = pieces.iter().map(|p| p.width * p.height).sum();
        assert_eq!(area, 100 - 20);
        assert!(
            pieces
                .iter()
                .all(|p| p.intersection(&rect(2, 3, 4, 5)).is_empty())
        );
        // One elsewhere leaves the hole as it was.
        assert_eq!(hole.subtract(&[rect(400, 0, 5, 5)]), [hole]);
    }

    #[test]
    fn an_overlay_past_the_store_edge_is_clipped() {
        let mut store = store_of(2, 2);
        let overlay = Overlay {
            width: 1,
            height: 1,
            picture: Picture::Pixels(vec![7, 7, 7, 255]),
        };
        blend_overlay(&mut store, &overlay, 1, -1, 4, 2);
        assert_eq!(
            store.pixels,
            [0, 0, 0, 0, 7, 7, 7, 255, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn source_over_blends_premultiplied_pixels() {
        let focus = Focus {
            view: std::ptr::null_mut(),
            window: std::ptr::null_mut(),
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            left: 0.0,
            top: 0.0,
            view_height: 1.0,
            density: 1.0,
            flipped: true,
            offscreen: false,
            clip: None,
        };
        FOCUS.with(|stack| stack.borrow_mut().push(focus));
        let size = NSSize {
            width: 1.0,
            height: 1.0,
        };
        let rect = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size,
        };
        let mut store = store_of(1, 1);
        store.pixels = vec![200, 200, 200, 255];
        draw_into(
            &mut store,
            &[0, 0, 128, 128],
            1,
            1,
            size,
            rect,
            rect,
            2,
            1.0,
            true,
        );
        assert_eq!(store.pixels, [99, 99, 227, 255]);
        FOCUS.with(|stack| stack.borrow_mut().clear());
    }
}

#[cfg(test)]
#[test]
fn changing_six_k_native_row_bands_are_granular() {
    if std::env::var_os("ROSEMARY_BENCH_TILED_NATIVE_CHANGED_6K").is_none() {
        return;
    }
    crate::bootstrap();
    const W: u32 = 6016;
    const H: u32 = 3384;
    let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
    set_native_view_pixels_for_tests(
        view,
        W,
        H,
        [1u8, 120, 200, 255].repeat(W as usize * H as usize),
    );
    let mut regions = Vec::new();
    for y in (0..H).step_by(2048) {
        for x in (0..W).step_by(2048) {
            regions.push(Rect {
                x: x as i64,
                y: y as i64,
                width: (W - x).min(2048) as i64,
                height: (H - y).min(2048) as i64,
            });
        }
    }
    assert_eq!(regions.len(), 6);
    let mut old = regions
        .iter()
        .map(|r| native_scene_tiled_pixels_cropped_with_damage(view, W, H, *r).unwrap())
        .collect::<Vec<_>>();
    let mut held = Vec::new();
    let mut timings = Vec::new();
    for rev in 2u8..=25 {
        {
            let mut stores = VIEW_STORES.lock().unwrap();
            let src = stores.as_mut().unwrap().get_mut(&(view as usize)).unwrap();
            for rect in &regions {
                let x = (rect.x + rect.width / 2) as usize;
                let y = (rect.y + rect.height / 2) as usize;
                src.pixels[(y * W as usize + x) * 4] = rev;
                src.touch_region(Rect {
                    x: x as i64,
                    y: y as i64,
                    width: 1,
                    height: 1,
                });
            }
        }
        held.clear();
        held.extend(old.iter().map(|(tiles, _, _)| std::sync::Arc::clone(tiles)));
        let started = std::time::Instant::now();
        for (i, rect) in regions.iter().enumerate() {
            let (now, revision, damage) =
                native_scene_tiled_pixels_cropped_with_damage(view, W, H, *rect).unwrap();
            assert_eq!(damage.unwrap().from_revision, old[i].1);
            let (x, y) = ((rect.width / 2) as u32, (rect.height / 2) as u32);
            let (mut previous, mut current) = ([0u8; 4], [0u8; 4]);
            held[i].copy_rect_to(x, y, 1, 1, &mut previous).unwrap();
            now.copy_rect_to(x, y, 1, 1, &mut current).unwrap();
            assert_eq!(previous[0], rev - 1);
            assert_eq!(current, [rev, 120, 200, 255]);
            old[i] = (now, revision, damage);
        }
        timings.push(started.elapsed().as_secs_f64() * 1000.);
    }
    timings.sort_by(f64::total_cmp);
    let mean = timings.iter().sum::<f64>() / timings.len() as f64;
    let p95 = timings[timings.len() * 95 / 100];
    eprintln!(
        "6K row-banded native captures six tiles held GPU readers mean_ms={mean:.3} p95_ms={p95:.3}"
    );
    drop(old);
    drop(held);
    forget(view);
    objc::arc::release(view);
}

#[cfg(test)]
#[test]
fn changing_six_k_native_store_capture_costs() {
    if std::env::var_os("ROSEMARY_BENCH_NATIVE_CHANGED_6K").is_none() {
        return;
    }
    crate::bootstrap();
    const W: u32 = 6016;
    const H: u32 = 3384;
    let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
    set_native_view_pixels_for_tests(
        view,
        W,
        H,
        [48u8, 128, 196, 255].repeat(W as usize * H as usize),
    );
    let mut tiles = Vec::new();
    for y in (0..H).step_by(2048) {
        for x in (0..W).step_by(2048) {
            tiles.push(Rect {
                x: x as i64,
                y: y as i64,
                width: (W - x).min(2048) as i64,
                height: (H - y).min(2048) as i64,
            });
        }
    }
    assert_eq!(tiles.len(), 6);
    let retained = std::env::var_os("ROSEMARY_BENCH_NATIVE_RETAIN_OLD").is_some();
    let mut prior = tiles
        .iter()
        .map(|rect| native_scene_pixels_cropped_with_damage(view, W, H, *rect).unwrap())
        .collect::<Vec<_>>();
    let mut measured = Vec::new();
    for revision in 1..=24 {
        {
            let mut stores = VIEW_STORES.lock().unwrap();
            let store = stores.as_mut().unwrap().get_mut(&(view as usize)).unwrap();
            for rect in &tiles {
                let x = (rect.x + rect.width / 2) as usize;
                let y = (rect.y + rect.height / 2) as usize;
                let at = (y * W as usize + x) * 4;
                store.pixels[at] = revision as u8;
                store.touch_region(Rect {
                    x: x as i64,
                    y: y as i64,
                    width: 1,
                    height: 1,
                });
            }
        }
        let started = std::time::Instant::now();
        for (i, rect) in tiles.iter().enumerate() {
            let (pixels, version, damage) =
                native_scene_pixels_cropped_with_damage(view, W, H, *rect).unwrap();
            assert_eq!(damage.unwrap().from_revision, prior[i].1);
            let at = ((rect.height / 2 * rect.width + rect.width / 2) * 4) as usize;
            assert_eq!(pixels[at], revision as u8);
            if retained {
                prior[i] = (pixels, version, damage);
            } else {
                prior[i].1 = version;
            }
        }
        measured.push(started.elapsed().as_secs_f64() * 1000.);
    }
    measured.sort_by(f64::total_cmp);
    let mean = measured.iter().sum::<f64>() / measured.len() as f64;
    let p95 = measured[measured.len() * 95 / 100];
    eprintln!("6K changed plain native captures six tiles mean_ms={mean:.3} p95_ms={p95:.3}");
    forget(view);
    objc::arc::release(view);
}
