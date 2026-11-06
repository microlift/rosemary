use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use owned_ttf_parser::{AsFaceRef, FaceMut, OwnedFace};
use rust_fontconfig::{FcFontCache, FcPattern, GenericFamily, OwnedFontSource, PatternMatch};

/// One face a family offers.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceInfo {
    pub family: String,
    pub style: String,
    pub postscript: String,
    pub path: String,
    pub index: u32,
    /// OpenType `usWeightClass`: 400 regular, 700 bold.
    pub weight: u16,
    pub italic: bool,
    pub monospace: bool,
}

static REGISTERED: Mutex<Vec<FaceInfo>> = Mutex::new(Vec::new());

/// The host's font cache, scanned once.
fn system_cache() -> &'static FcFontCache {
    static CACHE: OnceLock<FcFontCache> = OnceLock::new();
    CACHE.get_or_init(build_cache)
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_os = "android"
))]
fn build_cache() -> FcFontCache {
    FcFontCache::build()
}

/// rust-fontconfig only scans the hosts above, so parse the usual Unix font
/// directories directly elsewhere (the BSDs).
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_os = "android"
)))]
fn build_cache() -> FcFontCache {
    use rust_fontconfig::{FcFallbackConfig, FcFontPath, OperatingSystem};
    let os = OperatingSystem::current();
    let cache = FcFontCache::default().with_fallback_config(FcFallbackConfig::os_defaults(os));
    let mut pending = rust_fontconfig::config::font_directories(os);
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let path = path.to_string_lossy();
            for (pattern, font) in
                rust_fontconfig::FcParseFontBytes(&bytes, &path).unwrap_or_default()
            {
                cache.insert_builder_font(
                    pattern,
                    FcFontPath {
                        path: path.to_string(),
                        font_index: font.font_index,
                        bytes_hash: 0,
                    },
                );
            }
        }
    }
    cache
}

/// The host's faces, listed once.
fn system_faces() -> &'static [FaceInfo] {
    static FACES: OnceLock<Vec<FaceInfo>> = OnceLock::new();
    FACES.get_or_init(|| {
        let cache = system_cache();
        let mut faces = Vec::new();
        for (pattern, id) in cache.list() {
            let Some(OwnedFontSource::Disk(path)) = cache.get_font_by_id(&id) else {
                continue;
            };
            let Some(face) = face_from_pattern(&pattern, &path.path, path.font_index) else {
                continue;
            };
            // The cache maps the file, so this touches only the tables read.
            let instances = cache
                .get_font_bytes(&id)
                .map(|bytes| named_instances(bytes.as_slice(), &face))
                .unwrap_or_default();
            faces.push(face);
            faces.extend(instances);
        }
        faces.sort_by(|a, b| (&a.family, &a.style, &a.path).cmp(&(&b.family, &b.style, &b.path)));
        faces.dedup_by(|a, b| a.family == b.family && a.style == b.style);
        faces
    })
}

/// A face rust-fontconfig parsed, named the way [`describe`] names one.
fn face_from_pattern(pattern: &FcPattern, path: &str, index: usize) -> Option<FaceInfo> {
    // owned_ttf_parser cannot read WOFF, so keep to plain sfnt files.
    let lower = path.to_ascii_lowercase();
    if !(lower.ends_with(".ttf") || lower.ends_with(".otf") || lower.ends_with(".ttc")) {
        return None;
    }
    let metadata = &pattern.metadata;
    let family = metadata
        .preferred_family
        .clone()
        .or_else(|| pattern.family.clone())?;
    Some(FaceInfo {
        family,
        style: metadata
            .preferred_subfamily
            .clone()
            .or_else(|| metadata.font_subfamily.clone())
            .unwrap_or_else(|| "Regular".to_string()),
        postscript: metadata.postscript_name.clone().unwrap_or_default(),
        path: path.to_string(),
        index: u32::try_from(index).ok()?,
        weight: pattern.weight as u16,
        // fontconfig's slant counts oblique faces as italic too.
        italic: pattern.italic == PatternMatch::True || pattern.oblique == PatternMatch::True,
        monospace: pattern.monospace == PatternMatch::True,
    })
}

/// Everything a family lookup considers, application fonts first.
pub fn all_faces() -> Vec<FaceInfo> {
    let mut faces = REGISTERED.lock().unwrap_or_else(|e| e.into_inner()).clone();
    faces.extend(system_faces().iter().cloned());
    faces
}

/// Read the faces in a font file and make them available by family.
pub fn register_file(path: &str) -> Vec<FaceInfo> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let count = owned_ttf_parser::fonts_in_collection(&bytes).unwrap_or(1);
    let faces: Vec<FaceInfo> = (0..count)
        .filter_map(|index| describe(&bytes, index, path))
        .flat_map(|face| {
            let instances = named_instances(&bytes, &face);
            std::iter::once(face).chain(instances)
        })
        .collect();
    let mut registered = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
    for face in &faces {
        if !registered.contains(face) {
            registered.push(face.clone());
        }
    }
    faces
}

fn describe(bytes: &[u8], index: u32, path: &str) -> Option<FaceInfo> {
    let face = owned_ttf_parser::Face::parse(bytes, index).ok()?;
    let name = |ids: &[u16]| {
        ids.iter().find_map(|id| {
            face.names()
                .into_iter()
                .filter(|name| name.name_id == *id && name.is_unicode())
                .find_map(|name| name.to_string())
        })
    };
    use owned_ttf_parser::name_id;
    let family = name(&[name_id::TYPOGRAPHIC_FAMILY, name_id::FAMILY])?;
    Some(FaceInfo {
        family,
        style: name(&[name_id::TYPOGRAPHIC_SUBFAMILY, name_id::SUBFAMILY])
            .unwrap_or_else(|| "Regular".to_string()),
        postscript: name(&[name_id::POST_SCRIPT_NAME]).unwrap_or_default(),
        path: path.to_string(),
        index,
        weight: face.weight().to_number(),
        italic: face.is_italic(),
        monospace: face.is_monospaced(),
    })
}

/// The named instances in a variable face's `fvar` table. fontconfig lists
/// each as a face of its own at `instance << 16 | index`, and guests look
/// them up by PostScript name, so they keep that numbering.
fn named_instances(bytes: &[u8], base: &FaceInfo) -> Vec<FaceInfo> {
    let Ok(face) = owned_ttf_parser::Face::parse(bytes, base.index) else {
        return Vec::new();
    };
    let name = |id: u16| {
        face.names()
            .into_iter()
            .filter(|name| name.name_id == id && name.is_unicode())
            .find_map(|name| name.to_string())
    };
    use owned_ttf_parser::name_id;
    let prefix = name(name_id::VARIATIONS_POST_SCRIPT_NAME_PREFIX)
        .unwrap_or_else(|| base.family.replace(' ', ""));
    fvar_instances(&face)
        .into_iter()
        .enumerate()
        .filter_map(
            |(
                instance,
                Instance {
                    subfamily,
                    postscript,
                    coordinates,
                },
            )| {
                let style = name(subfamily)?;
                let coordinate = |tag: &[u8; 4]| {
                    let tag = owned_ttf_parser::Tag::from_bytes(tag);
                    coordinates
                        .iter()
                        .find(|(axis, _)| *axis == tag)
                        .map(|&(_, value)| value)
                };
                // The PostScript name ID is optional; fontconfig derives one when
                // it is missing.
                let postscript = postscript
                    .and_then(name)
                    .unwrap_or_else(|| format!("{prefix}-{}", style.replace(' ', "")));
                Some(FaceInfo {
                    family: base.family.clone(),
                    postscript,
                    path: base.path.clone(),
                    index: (u32::try_from(instance + 1).ok()? << 16) | base.index,
                    weight: coordinate(b"wght").map_or(base.weight, |weight| {
                        weight.round().clamp(1.0, 1000.0) as u16
                    }),
                    italic: base.italic
                        || style.contains("Italic")
                        || style.contains("Oblique")
                        || coordinate(b"ital").is_some_and(|value| value >= 0.5)
                        || coordinate(b"slnt").is_some_and(|value| value != 0.0),
                    monospace: base.monospace,
                    style,
                })
            },
        )
        .collect()
}

/// One named instance in an `fvar` table.
struct Instance {
    subfamily: u16,
    postscript: Option<u16>,
    coordinates: Vec<(owned_ttf_parser::Tag, f32)>,
}

/// The named instances a variable face declares, in table order.
fn fvar_instances(face: &owned_ttf_parser::Face<'_>) -> Vec<Instance> {
    let Some(fvar) = face
        .raw_face()
        .table(owned_ttf_parser::Tag::from_bytes(b"fvar"))
    else {
        return Vec::new();
    };
    let u16_at = |at: usize| {
        fvar.get(at..at + 2)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
    };
    let fixed_at = |at: usize| {
        fvar.get(at..at + 4)
            .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f32 / 65536.0)
    };
    let (Some(axes), Some(axis_count), Some(axis_size), Some(count), Some(size)) =
        (u16_at(4), u16_at(8), u16_at(10), u16_at(12), u16_at(14))
    else {
        return Vec::new();
    };
    let tags: Vec<owned_ttf_parser::Tag> = (0..axis_count)
        .map_while(|axis| {
            let tag = fvar.get(axes + axis * axis_size..)?.get(..4)?;
            Some(owned_ttf_parser::Tag::from_bytes(&[
                tag[0], tag[1], tag[2], tag[3],
            ]))
        })
        .collect();
    if tags.len() != axis_count {
        return Vec::new();
    }
    let first = axes + axis_count * axis_size;
    (0..count)
        .map_while(|instance| {
            let at = first + instance * size;
            let coordinates = tags
                .iter()
                .enumerate()
                .map(|(axis, &tag)| Some((tag, fixed_at(at + 4 + axis * 4)?)))
                .collect::<Option<Vec<_>>>()?;
            Some(Instance {
                subfamily: u16::try_from(u16_at(at)?).ok()?,
                postscript: (size >= axis_count * 4 + 6)
                    .then(|| u16_at(at + 4 + axis_count * 4))
                    .flatten()
                    .and_then(|id| u16::try_from(id).ok()),
                coordinates,
            })
        })
        .collect()
}

/// A family name that is a Mac's monospace font rather than its UI font.
fn wants_monospace(family: &str) -> bool {
    let family = family.to_ascii_lowercase();
    ["menlo", "monaco", "courier", "mono", "sf mono", "consol"]
        .iter()
        .any(|name| family.contains(name))
}

/// The face for `family` closest to `weight` and `italic`, falling back to the
/// host's default sans-serif or monospace family.
pub fn resolve(family: &str, weight: u16, italic: bool) -> Option<FaceInfo> {
    let faces = all_faces();
    let best = |family: &str| {
        faces
            .iter()
            .filter(|face| {
                face.family.eq_ignore_ascii_case(family)
                    || face.postscript.eq_ignore_ascii_case(family)
            })
            .min_by_key(|face| {
                (u32::from(face.italic != italic) * 1000) + u32::from(face.weight.abs_diff(weight))
            })
            .cloned()
    };
    if let Some(face) = best(family) {
        return Some(face);
    }
    let fallback = match wants_monospace(family) {
        true => default_family(true),
        false => default_family(false),
    }?;
    best(&fallback)
}

/// The first installed family the host's configuration lists for `sans-serif`
/// or `monospace`.
pub fn default_family(monospace: bool) -> Option<String> {
    static DEFAULTS: OnceLock<[Option<String>; 2]> = OnceLock::new();
    let defaults = DEFAULTS.get_or_init(|| {
        let config = system_cache().fallback_config();
        let faces = system_faces();
        let ask = |generic: GenericFamily| {
            config
                .generic_candidates(generic)
                .iter()
                .find_map(|wanted| {
                    faces
                        .iter()
                        .find(|face| face.family.eq_ignore_ascii_case(wanted))
                        .map(|face| face.family.clone())
                })
        };
        [ask(GenericFamily::SansSerif), ask(GenericFamily::Monospace)]
    });
    defaults[usize::from(monospace)].clone().or_else(|| {
        system_faces()
            .iter()
            .find(|face| face.monospace == monospace)
            .map(|face| face.family.clone())
    })
}

pub struct LoadedFace {
    face: OwnedFace,
}

impl LoadedFace {
    pub fn face(&self) -> &owned_ttf_parser::Face<'_> {
        self.face.as_face_ref()
    }
}

// Enumeration asks for every installed face, including every named instance
// of large collections. Keep only a working set, not a file copy per face for
// the lifetime of each guest process. Callers' Arcs survive cache eviction.
const FACE_CACHE_BYTES: usize = 64 * 1024 * 1024;
const FACE_CACHE_ENTRIES: usize = 128;

type FaceKey = (String, u32);
struct FaceCache<T> {
    entries: VecDeque<(FaceKey, Arc<T>, usize)>,
    bytes: usize,
    budget: usize,
}

impl<T> FaceCache<T> {
    fn new(budget: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            budget,
        }
    }

    fn get(&mut self, path: &str, index: u32) -> Option<Arc<T>> {
        let position = self
            .entries
            .iter()
            .position(|(key, _, _)| key.0 == path && key.1 == index)?;
        let entry = self.entries.remove(position)?;
        let face = Arc::clone(&entry.1);
        self.entries.push_back(entry);
        Some(face)
    }

    fn insert(&mut self, key: FaceKey, face: Arc<T>, bytes: usize) {
        if bytes > self.budget {
            return;
        }
        while self.bytes > self.budget - bytes || self.entries.len() >= FACE_CACHE_ENTRIES {
            if let Some((_, _, size)) = self.entries.pop_front() {
                self.bytes -= size;
            }
        }
        self.bytes += bytes;
        self.entries.push_back((key, face, bytes));
    }
}

/// The parsed face at `path` and `index`, from a bounded working-set cache.
pub fn load(path: &str, index: u32) -> Option<Arc<LoadedFace>> {
    static CACHE: Mutex<Option<FaceCache<LoadedFace>>> = Mutex::new(None);
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(|| FaceCache::new(FACE_CACHE_BYTES));
    if let Some(face) = cache.get(path, index) {
        return Some(face);
    }
    let bytes = std::fs::read(path).ok()?;
    let size = bytes.len();
    // The high half picks a variable font's named instance, which
    // ttf-parser only knows as a set of axis coordinates.
    let mut face = OwnedFace::from_vec(bytes, index & 0xffff).ok()?;
    if let Some(instance) = (index >> 16).checked_sub(1) {
        let instance = fvar_instances(face.as_face_ref())
            .into_iter()
            .nth(usize::try_from(instance).ok()?)?;
        for (axis, value) in instance.coordinates {
            face.set_variation(axis, value);
        }
    }
    let face = Arc::new(LoadedFace { face });
    cache.insert((path.to_string(), index), Arc::clone(&face), size);
    Some(face)
}

/// The value each of the face's variation axes currently holds: the named
/// instance's coordinate where `index` names one, and the axis default
/// otherwise - what `CTFontCopyVariation` reports.
pub fn current_variation(path: &str, index: u32) -> Vec<(u32, f32)> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let Ok(face) = owned_ttf_parser::Face::parse(&bytes, index & 0xffff) else {
        return Vec::new();
    };
    let instance = (index >> 16).checked_sub(1).and_then(|at| {
        fvar_instances(&face)
            .into_iter()
            .nth(usize::try_from(at).ok()?)
    });
    face.variation_axes()
        .into_iter()
        .map(|axis| {
            let value = instance
                .as_ref()
                .and_then(|instance| {
                    instance
                        .coordinates
                        .iter()
                        .find(|(tag, _)| *tag == axis.tag)
                        .map(|(_, value)| *value)
                })
                .unwrap_or(axis.def_value);
            (axis.tag.0, value)
        })
        .collect()
}

/// A glyph's outline as line and curve segments, in font units, y up.
pub fn outline(face: &owned_ttf_parser::Face<'_>, glyph: u16) -> Vec<Segment> {
    let mut builder = OutlineCollector::default();
    face.outline_glyph(owned_ttf_parser::GlyphId(glyph), &mut builder);
    builder.finish()
}

/// Collects a glyph's outline, closing every contour: CFF2 charstrings have
/// no closepath and ttf-parser leaves their contours open, and an open
/// contour's unbalanced winding fills the rest of the glyph's box.
#[derive(Default)]
struct OutlineCollector {
    segments: Vec<Segment>,
    start: (f32, f32),
    at: (f32, f32),
}

impl OutlineCollector {
    fn finish(mut self) -> Vec<Segment> {
        owned_ttf_parser::OutlineBuilder::close(&mut self);
        self.segments
    }
}

impl owned_ttf_parser::OutlineBuilder for OutlineCollector {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.start = (x, y);
        self.at = (x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.segments.push(Segment::Line(self.at, (x, y)));
        self.at = (x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.segments.push(Segment::Quad(self.at, (x1, y1), (x, y)));
        self.at = (x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.segments
            .push(Segment::Cubic(self.at, (x1, y1), (x2, y2), (x, y)));
        self.at = (x, y);
    }
    fn close(&mut self) {
        if self.at != self.start {
            self.segments.push(Segment::Line(self.at, self.start));
        }
        self.at = self.start;
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Segment {
    Line((f32, f32), (f32, f32)),
    Quad((f32, f32), (f32, f32), (f32, f32)),
    Cubic((f32, f32), (f32, f32), (f32, f32), (f32, f32)),
}

/// Coverage of `segments` after `transform` maps them to device pixels, y up.
///
/// Returns the mask's left column, top row (as a device y, the row of the
/// mask's first line) and width, and the coverage rows running down.
pub fn rasterize(
    segments: &[Segment],
    transform: impl Fn(f32, f32) -> (f32, f32),
) -> Option<(i64, i64, usize, Vec<f32>)> {
    let mut points = Vec::with_capacity(segments.len() * 4);
    let mut mapped = Vec::with_capacity(segments.len());
    for segment in segments {
        let segment = match *segment {
            Segment::Line(a, b) => Segment::Line(transform(a.0, a.1), transform(b.0, b.1)),
            Segment::Quad(a, b, c) => Segment::Quad(
                transform(a.0, a.1),
                transform(b.0, b.1),
                transform(c.0, c.1),
            ),
            Segment::Cubic(a, b, c, d) => Segment::Cubic(
                transform(a.0, a.1),
                transform(b.0, b.1),
                transform(c.0, c.1),
                transform(d.0, d.1),
            ),
        };
        match segment {
            Segment::Line(a, b) => points.extend([a, b]),
            Segment::Quad(a, b, c) => points.extend([a, b, c]),
            Segment::Cubic(a, b, c, d) => points.extend([a, b, c, d]),
        }
        mapped.push(segment);
    }
    let (min_x, max_x, min_y, max_y) = points.iter().fold(
        (f32::MAX, f32::MIN, f32::MAX, f32::MIN),
        |(a, b, c, d), &(x, y)| (a.min(x), b.max(x), c.min(y), d.max(y)),
    );
    if points.is_empty() || !min_x.is_finite() || !max_y.is_finite() {
        return None;
    }
    let left = min_x.floor();
    let top = max_y.ceil();
    let width = (max_x.ceil() - left).max(1.0) as usize;
    let height = (top - min_y.floor()).max(1.0) as usize;
    if width > 4096 || height > 4096 {
        return None;
    }
    let mut rasterizer = ab_glyph_rasterizer::Rasterizer::new(width, height);
    let local = |(x, y): (f32, f32)| ab_glyph_rasterizer::point(x - left, top - y);
    for segment in mapped {
        match segment {
            Segment::Line(a, b) => rasterizer.draw_line(local(a), local(b)),
            Segment::Quad(a, b, c) => rasterizer.draw_quad(local(a), local(b), local(c)),
            Segment::Cubic(a, b, c, d) => {
                rasterizer.draw_cubic(local(a), local(b), local(c), local(d))
            }
        }
    }
    let mut coverage = vec![0.0f32; width * height];
    rasterizer.for_each_pixel(|at, value| coverage[at] = value);
    // Row 0 of the mask spans device y from `top - 1` to `top`.
    Some((left as i64, top as i64 - 1, width, coverage))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumeration_releases_old_faces_but_keeps_borrowed_faces_alive() {
        let mut cache = FaceCache::new(8);
        let first = Arc::new(vec![1u8; 4]);
        let weak = Arc::downgrade(&first);
        cache.insert(("collection".into(), 0), first.clone(), 4);
        for index in 1..100 {
            let face = Arc::new(vec![index as u8; 4]);
            cache.insert(("collection".into(), index), face, 4);
            assert!(cache.bytes <= 8);
        }
        assert!(cache.get("collection", 0).is_none());
        assert_eq!(&*first, &[1; 4]);
        drop(first);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn recently_used_faces_survive_eviction_and_oversized_faces_are_not_retained() {
        let mut cache = FaceCache::new(8);
        for index in 0..2 {
            cache.insert(("font".into(), index), Arc::new(index), 4);
        }
        let first = cache.get("font", 0).unwrap();
        cache.insert(("font".into(), 2), Arc::new(2), 4);
        assert!(Arc::ptr_eq(&first, &cache.get("font", 0).unwrap()));
        assert!(cache.get("font", 1).is_none());
        let large = Arc::new(3);
        let weak = Arc::downgrade(&large);
        cache.insert(("large".into(), 0), large, 9);
        assert!(weak.upgrade().is_none());
        assert_eq!(cache.bytes, 8);
    }

    #[test]
    fn names_faces_by_typographic_family() {
        use rust_fontconfig::{FcFontMetadata, FcWeight};
        let pattern = FcPattern {
            family: Some("Noto Sans SemiBold".to_string()),
            weight: FcWeight::SemiBold,
            oblique: PatternMatch::True,
            monospace: PatternMatch::False,
            metadata: FcFontMetadata {
                preferred_family: Some("Noto Sans".to_string()),
                font_subfamily: Some("Italic".to_string()),
                preferred_subfamily: Some("SemiBold Italic".to_string()),
                postscript_name: Some("NotoSans-SemiBoldItalic".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let face =
            face_from_pattern(&pattern, "/fonts/NotoSans-SemiBoldItalic.ttf", 0).expect("a face");
        assert_eq!(face.family, "Noto Sans");
        assert_eq!(face.style, "SemiBold Italic");
        assert_eq!(face.postscript, "NotoSans-SemiBoldItalic");
        assert_eq!(face.weight, 600);
        assert!(face.italic);
        assert!(!face.monospace);
        assert!(face_from_pattern(&pattern, "/fonts/NotoSans.woff2", 0).is_none());
    }

    #[test]
    fn closes_contours_the_font_leaves_open() {
        use owned_ttf_parser::OutlineBuilder;
        let mut builder = OutlineCollector::default();
        builder.move_to(0.0, 0.0);
        builder.line_to(2.0, 0.0);
        builder.line_to(2.0, 2.0);
        builder.move_to(5.0, 0.0);
        builder.line_to(6.0, 0.0);
        builder.line_to(6.0, 1.0);
        builder.close();
        builder.move_to(8.0, 0.0);
        builder.line_to(9.0, 0.0);
        builder.line_to(9.0, 1.0);
        let segments = builder.finish();
        let closing: Vec<_> = segments
            .iter()
            .filter_map(|segment| match *segment {
                Segment::Line(a, b) if b.1 == 0.0 && a.1 != 0.0 => Some((a, b)),
                _ => None,
            })
            .collect();
        assert_eq!(
            closing,
            [
                ((2.0, 2.0), (0.0, 0.0)),
                ((6.0, 1.0), (5.0, 0.0)),
                ((9.0, 1.0), (8.0, 0.0)),
            ]
        );
        assert_eq!(segments.len(), 9);
    }

    #[test]
    fn rasterizes_a_square_covering_its_pixels() {
        let square = [
            Segment::Line((0.0, 0.0), (2.0, 0.0)),
            Segment::Line((2.0, 0.0), (2.0, 2.0)),
            Segment::Line((2.0, 2.0), (0.0, 2.0)),
            Segment::Line((0.0, 2.0), (0.0, 0.0)),
        ];
        let (left, top, width, coverage) =
            rasterize(&square, |x, y| (x + 3.0, y + 5.0)).expect("coverage");
        assert_eq!((left, top, width), (3, 6, 2));
        assert!(coverage.iter().all(|value| (value - 1.0).abs() < 1e-3));
    }
}
