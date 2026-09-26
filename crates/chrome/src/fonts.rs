use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use owned_ttf_parser::{AsFaceRef, OwnedFace};

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

/// The host's faces, listed once.
fn system_faces() -> &'static [FaceInfo] {
    static FACES: OnceLock<Vec<FaceInfo>> = OnceLock::new();
    FACES.get_or_init(|| {
        let output = std::process::Command::new("fc-list")
            .arg("--format")
            .arg("%{family[0]}\t%{style[0]}\t%{file}\t%{index}\t%{weight}\t%{slant}\t%{spacing}\t%{postscriptname}\n")
            .output();
        let Ok(output) = output else {
            return Vec::new();
        };
        let mut faces: Vec<FaceInfo> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_fc_line)
            .collect();
        faces.sort_by(|a, b| (&a.family, &a.style, &a.path).cmp(&(&b.family, &b.style, &b.path)));
        faces.dedup_by(|a, b| a.family == b.family && a.style == b.style);
        faces
    })
}

/// A line of the `fc-list` format [`system_faces`] asks for.
fn parse_fc_line(line: &str) -> Option<FaceInfo> {
    let fields: Vec<&str> = line.split('\t').collect();
    let [
        family,
        style,
        path,
        index,
        weight,
        slant,
        spacing,
        postscript,
    ] = fields[..]
    else {
        return None;
    };
    if !(path.ends_with(".ttf") || path.ends_with(".otf") || path.ends_with(".ttc")) {
        return None;
    }
    // fontconfig's weight scale: 80 regular, 100 medium, 180 demibold, 200
    // bold, 210 black.
    let weight = match weight.parse::<u32>().unwrap_or(80) {
        0..=45 => 200,
        46..=55 => 300,
        56..=90 => 400,
        91..=140 => 500,
        141..=190 => 600,
        191..=205 => 700,
        206..=212 => 800,
        _ => 900,
    };
    Some(FaceInfo {
        family: family.to_string(),
        style: style.to_string(),
        postscript: postscript.to_string(),
        path: path.to_string(),
        index: index.parse().unwrap_or(0),
        weight,
        italic: slant.parse::<u32>().unwrap_or(0) >= 100,
        monospace: spacing.parse::<u32>().unwrap_or(0) >= 90,
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

/// The family fontconfig chooses for `sans-serif` or `monospace`.
pub fn default_family(monospace: bool) -> Option<String> {
    static DEFAULTS: OnceLock<[Option<String>; 2]> = OnceLock::new();
    let defaults = DEFAULTS.get_or_init(|| {
        let ask = |pattern: &str| {
            let output = std::process::Command::new("fc-match")
                .args(["--format", "%{family[0]}", pattern])
                .output()
                .ok()?;
            let family = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (!family.is_empty()).then_some(family)
        };
        [ask("sans-serif"), ask("monospace")]
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

/// The parsed face at `path` and `index`, from the cache.
pub fn load(path: &str, index: u32) -> Option<Arc<LoadedFace>> {
    type Cache = HashMap<(String, u32), Option<Arc<LoadedFace>>>;
    static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache
        .get_or_insert_with(HashMap::new)
        .entry((path.to_string(), index))
        .or_insert_with(|| {
            let bytes = std::fs::read(path).ok()?;
            let face = OwnedFace::from_vec(bytes, index).ok()?;
            Some(Arc::new(LoadedFace { face }))
        })
        .clone()
}

/// A glyph's outline as line and curve segments, in font units, y up.
pub fn outline(face: &owned_ttf_parser::Face<'_>, glyph: u16) -> Vec<Segment> {
    struct Builder {
        segments: Vec<Segment>,
        start: (f32, f32),
        at: (f32, f32),
    }
    impl owned_ttf_parser::OutlineBuilder for Builder {
        fn move_to(&mut self, x: f32, y: f32) {
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
    let mut builder = Builder {
        segments: Vec::new(),
        start: (0.0, 0.0),
        at: (0.0, 0.0),
    };
    face.outline_glyph(owned_ttf_parser::GlyphId(glyph), &mut builder);
    builder.segments
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
    fn reads_fontconfig_lines() {
        let face = parse_fc_line(
            "Noto Sans\tBold Italic\t/fonts/NotoSans-BoldItalic.ttf\t0\t200\t100\t0\tNotoSans-BoldItalic",
        )
        .expect("a face");
        assert_eq!(face.weight, 700);
        assert!(face.italic);
        assert!(!face.monospace);
        assert!(parse_fc_line("Bitmap\tRegular\t/fonts/fixed.pcf.gz\t0\t80\t0\t100\t").is_none());
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
