//! The first page of a PDF, rasterised: what `NSImage` draws for the vector
//! artwork applications ship as `.pdf` resources (template icons, mostly).
//!
//! Only what such artwork uses is read: paths, fills, strokes, clips, colours,
//! alpha, and form XObjects. A page that needs anything else - text, images,
//! shadings, patterns, blend modes, soft masks, object streams - is refused
//! with a diagnostic naming it, so the image is nil rather than wrong.

use std::collections::HashMap;

use tiny_skia::{
    FillRule, LineCap, LineJoin, Mask, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke,
    StrokeDash, Transform,
};

use crate::raster::Bitmap;

#[derive(Clone, Debug, PartialEq)]
enum Object {
    Null,
    Bool(bool),
    Number(f64),
    Name(Vec<u8>),
    String(Vec<u8>),
    Array(Vec<Object>),
    Dict(Dict),
    Ref(u32),
    /// A dictionary and its still-encoded data.
    Stream(Dict, Vec<u8>),
    /// An operator in a content stream.
    Operator(Vec<u8>),
}

type Dict = HashMap<Vec<u8>, Object>;

impl Object {
    fn number(&self) -> Option<f64> {
        match self {
            Object::Number(n) => Some(*n),
            _ => None,
        }
    }

    fn name(&self) -> Option<&[u8]> {
        match self {
            Object::Name(n) => Some(n),
            _ => None,
        }
    }
}

/// Why a page was not drawn, for the diagnostic.
type Refusal = String;

fn is_white(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

struct Lexer<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Lexer<'a> {
    fn new(bytes: &'a [u8], at: usize) -> Self {
        Lexer { bytes, at }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn skip_white(&mut self) {
        while let Some(byte) = self.peek() {
            if is_white(byte) {
                self.at += 1;
            } else if byte == b'%' {
                while self.peek().is_some_and(|b| b != b'\n' && b != b'\r') {
                    self.at += 1;
                }
            } else {
                break;
            }
        }
    }

    fn regular(&mut self) -> &'a [u8] {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|b| !is_white(b) && !is_delimiter(b))
        {
            self.at += 1;
        }
        &self.bytes[start..self.at]
    }

    /// One object. `Ok(None)` at the end of the input or at a closing
    /// delimiter, which the caller of a container consumes.
    fn object(&mut self) -> Result<Option<Object>, Refusal> {
        self.skip_white();
        let Some(byte) = self.peek() else {
            return Ok(None);
        };
        match byte {
            b'/' => {
                self.at += 1;
                let raw = self.regular();
                Ok(Some(Object::Name(unescape_name(raw))))
            }
            b'(' => self.literal_string().map(|s| Some(Object::String(s))),
            b'<' if self.bytes.get(self.at + 1) == Some(&b'<') => {
                self.at += 2;
                self.dict().map(|d| Some(Object::Dict(d)))
            }
            b'<' => self.hex_string().map(|s| Some(Object::String(s))),
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    match self.object()? {
                        Some(item) => items.push(item),
                        None if self.peek() == Some(b']') => {
                            self.at += 1;
                            return Ok(Some(Object::Array(items)));
                        }
                        None => return Err("unterminated array".into()),
                    }
                }
            }
            b']' | b'>' | b')' | b'}' => Ok(None),
            b'{' => Err("PostScript calculator functions".into()),
            _ => {
                let word = self.regular();
                if word.is_empty() {
                    return Err(format!("unexpected byte {byte:#x}"));
                }
                Ok(Some(self.word(word)))
            }
        }
    }

    fn word(&mut self, word: &[u8]) -> Object {
        match word {
            b"true" => return Object::Bool(true),
            b"false" => return Object::Bool(false),
            b"null" => return Object::Null,
            _ => {}
        }
        let Some(number) = std::str::from_utf8(word)
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|_| {
                word.iter()
                    .all(|b| b.is_ascii_digit() || b"+-.".contains(b))
            })
        else {
            return Object::Operator(word.to_vec());
        };
        // `N G R` is a reference: look ahead without committing.
        if word.iter().all(u8::is_ascii_digit) {
            let save = self.at;
            self.skip_white();
            let generation = self.regular();
            if !generation.is_empty() && generation.iter().all(u8::is_ascii_digit) {
                self.skip_white();
                if self.regular() == b"R" {
                    return Object::Ref(number as u32);
                }
            }
            self.at = save;
        }
        Object::Number(number)
    }

    fn dict(&mut self) -> Result<Dict, Refusal> {
        let mut dict = Dict::new();
        loop {
            match self.object()? {
                Some(Object::Name(key)) => {
                    let value = self.object()?.ok_or("dictionary value missing")?;
                    dict.insert(key, value);
                }
                Some(other) => return Err(format!("dictionary key {other:?}")),
                None if self.bytes.get(self.at..self.at + 2) == Some(b">>") => {
                    self.at += 2;
                    return Ok(dict);
                }
                None => return Err("unterminated dictionary".into()),
            }
        }
    }

    fn literal_string(&mut self) -> Result<Vec<u8>, Refusal> {
        self.at += 1;
        let mut out = Vec::new();
        let mut depth = 0;
        loop {
            let byte = self.peek().ok_or("unterminated string")?;
            self.at += 1;
            match byte {
                b'(' => {
                    depth += 1;
                    out.push(byte);
                }
                b')' if depth == 0 => return Ok(out),
                b')' => {
                    depth -= 1;
                    out.push(byte);
                }
                b'\\' => {
                    let escaped = self.peek().ok_or("unterminated string")?;
                    self.at += 1;
                    match escaped {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut value = u32::from(escaped - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        value = value * 8 + u32::from(d - b'0');
                                        self.at += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(value as u8);
                        }
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.at += 1;
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                _ => out.push(byte),
            }
        }
    }

    fn hex_string(&mut self) -> Result<Vec<u8>, Refusal> {
        self.at += 1;
        let mut digits = Vec::new();
        loop {
            let byte = self.peek().ok_or("unterminated hex string")?;
            self.at += 1;
            match byte {
                b'>' => break,
                b if is_white(b) => {}
                b if b.is_ascii_hexdigit() => digits.push((b as char).to_digit(16).unwrap() as u8),
                _ => return Err("bad hex string".into()),
            }
        }
        if digits.len() % 2 == 1 {
            digits.push(0);
        }
        Ok(digits.chunks(2).map(|p| p[0] << 4 | p[1]).collect())
    }
}

fn unescape_name(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'#'
            && let Some(value) = raw
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(value);
            i += 3;
            continue;
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

/// Every `N G obj ... endobj` in the file, later definitions winning, as an
/// incremental update's do. The cross-reference table is not read: scanning
/// finds the same objects in the files this is for, and survives a table
/// whose offsets are stale.
struct Document {
    objects: HashMap<u32, Object>,
}

impl Document {
    fn parse(bytes: &[u8]) -> Result<Document, Refusal> {
        let mut objects = HashMap::new();
        let mut at = 0;
        while let Some(found) = find(bytes, b"obj", at) {
            at = found + 3;
            if bytes
                .get(at)
                .is_some_and(|b| !is_white(*b) && !is_delimiter(*b))
            {
                continue;
            }
            let Some(number) = object_number_before(bytes, found) else {
                continue;
            };
            let mut lexer = Lexer::new(bytes, at);
            let Some(object) = lexer.object()? else {
                continue;
            };
            lexer.skip_white();
            let object = match object {
                Object::Dict(dict) if bytes[lexer.at..].starts_with(b"stream") => {
                    let (data, end) = stream_data(bytes, lexer.at + 6, &dict)?;
                    at = end;
                    Object::Stream(dict, data.to_vec())
                }
                other => {
                    at = lexer.at;
                    other
                }
            };
            if let Object::Stream(dict, _) | Object::Dict(dict) = &object
                && dict.get(b"Type".as_slice()).and_then(Object::name) == Some(b"ObjStm")
            {
                return Err("object streams".into());
            }
            objects.insert(number, object);
        }
        Ok(Document { objects })
    }

    fn resolve<'a>(&'a self, mut object: &'a Object) -> &'a Object {
        for _ in 0..32 {
            match object {
                Object::Ref(number) => {
                    object = self.objects.get(number).unwrap_or(&Object::Null);
                }
                _ => return object,
            }
        }
        &Object::Null
    }

    fn get<'a>(&'a self, dict: &'a Dict, key: &[u8]) -> &'a Object {
        dict.get(key).map_or(&Object::Null, |o| self.resolve(o))
    }

    fn dict<'a>(&'a self, object: &'a Object) -> Option<&'a Dict> {
        match self.resolve(object) {
            Object::Dict(dict) | Object::Stream(dict, _) => Some(dict),
            _ => None,
        }
    }

    fn numbers(&self, object: &Object) -> Option<Vec<f64>> {
        match self.resolve(object) {
            Object::Array(items) => items.iter().map(|i| self.resolve(i).number()).collect(),
            _ => None,
        }
    }

    /// The stream's data with its filters undone.
    fn decoded(&self, object: &Object) -> Result<Vec<u8>, Refusal> {
        let Object::Stream(dict, data) = self.resolve(object) else {
            return Err("content that is not a stream".into());
        };
        let filters = match self.get(dict, b"Filter") {
            Object::Null => Vec::new(),
            Object::Name(name) => vec![name.clone()],
            Object::Array(items) => items
                .iter()
                .filter_map(|i| self.resolve(i).name().map(<[u8]>::to_vec))
                .collect(),
            _ => return Err("a malformed /Filter".into()),
        };
        if !matches!(self.get(dict, b"DecodeParms"), Object::Null) && !filters.is_empty() {
            return Err("filter parameters (predictors)".into());
        }
        let mut data = data.clone();
        for filter in filters {
            data = match filter.as_slice() {
                b"FlateDecode" | b"Fl" => {
                    crate::raster::inflate(&data).ok_or("a stream that does not inflate")?
                }
                other => {
                    return Err(format!("the {} filter", String::from_utf8_lossy(other)));
                }
            };
        }
        Ok(data)
    }
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// The `N` of `N G obj` ending just before `at`.
fn object_number_before(bytes: &[u8], at: usize) -> Option<u32> {
    let mut end = at;
    let mut numbers = [0u32; 2];
    for slot in numbers.iter_mut().rev() {
        while end > 0 && is_white(bytes[end - 1]) {
            end -= 1;
        }
        let start = bytes[..end]
            .iter()
            .rposition(|b| !b.is_ascii_digit())
            .map_or(0, |p| p + 1);
        if start == end || end == at {
            return None;
        }
        *slot = std::str::from_utf8(&bytes[start..end]).ok()?.parse().ok()?;
        end = start;
    }
    Some(numbers[0])
}

/// A stream's raw data starting after the `stream` keyword, and where its
/// object ends. `/Length` is used when it is direct and lands on
/// `endstream`; an indirect one may be defined after the stream, so the
/// keyword is searched for instead.
fn stream_data<'a>(
    bytes: &'a [u8],
    mut at: usize,
    dict: &Dict,
) -> Result<(&'a [u8], usize), Refusal> {
    if bytes.get(at) == Some(&b'\r') {
        at += 1;
    }
    if bytes.get(at) == Some(&b'\n') {
        at += 1;
    }
    if let Some(length) = dict.get(b"Length".as_slice()).and_then(Object::number) {
        let end = at + length as usize;
        let mut after = end;
        while bytes.get(after).is_some_and(|b| is_white(*b)) {
            after += 1;
        }
        if bytes
            .get(after..)
            .is_some_and(|b| b.starts_with(b"endstream"))
        {
            return Ok((&bytes[at..end], after + 9));
        }
    }
    let keyword = find(bytes, b"endstream", at).ok_or("unterminated stream")?;
    let mut end = keyword;
    if end > at && bytes[end - 1] == b'\n' {
        end -= 1;
    }
    if end > at && bytes[end - 1] == b'\r' {
        end -= 1;
    }
    Ok((&bytes[at..end], keyword + 9))
}

/// The page to draw: the first leaf of the page tree, with the attributes it
/// inherits.
struct Page<'a> {
    media_box: [f64; 4],
    resources: Option<&'a Dict>,
    contents: Vec<&'a Object>,
}

fn first_page(document: &Document) -> Result<Page<'_>, Refusal> {
    let catalog = document
        .objects
        .values()
        .filter_map(|o| document.dict(o))
        .find(|d| document.get(d, b"Type").name() == Some(b"Catalog"))
        .ok_or("no catalog")?;
    let mut node = document
        .dict(catalog.get(b"Pages".as_slice()).ok_or("no page tree")?)
        .ok_or("no page tree")?;
    let mut media_box = None;
    let mut resources = None;
    for _ in 0..32 {
        if let Some(found) = document.numbers(document.get(node, b"MediaBox"))
            && let [x0, y0, x1, y1] = found[..]
        {
            media_box = Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]);
        }
        if let Some(found) = document.dict(document.get(node, b"Resources")) {
            resources = Some(found);
        }
        if let Some(rotate) = document.get(node, b"Rotate").number()
            && rotate.rem_euclid(360.0) != 0.0
        {
            return Err("a rotated page".into());
        }
        match document.get(node, b"Type").name() {
            Some(b"Page") => {
                let contents = match document.get(node, b"Contents") {
                    Object::Array(items) => items.iter().collect(),
                    Object::Null => Vec::new(),
                    _ => vec![node.get(b"Contents".as_slice()).unwrap()],
                };
                return Ok(Page {
                    media_box: media_box.ok_or("a page with no /MediaBox")?,
                    resources,
                    contents,
                });
            }
            _ => {
                let Object::Array(kids) = document.get(node, b"Kids") else {
                    return Err("a page tree node with no /Kids".into());
                };
                node = kids
                    .first()
                    .and_then(|k| document.dict(k))
                    .ok_or("an empty page tree")?;
            }
        }
    }
    Err("a page tree too deep".into())
}

/// The page's size in points, which is the image's size.
pub fn page_size(bytes: &[u8]) -> Option<(f64, f64)> {
    let document = Document::parse(bytes).ok()?;
    let page = first_page(&document).ok()?;
    let [x0, y0, x1, y1] = page.media_box;
    Some((x1 - x0, y1 - y0))
}

/// The first page drawn at `scale` pixels per point, or `None` (with a
/// diagnostic) for a page that uses something this does not draw.
pub fn render(bytes: &[u8], scale: f64) -> Option<Bitmap> {
    match try_render(bytes, scale) {
        Ok(bitmap) => Some(bitmap),
        Err(why) => {
            eprintln!("rosemary: appkit: not drawing a PDF that uses {why}");
            None
        }
    }
}

fn try_render(bytes: &[u8], scale: f64) -> Result<Bitmap, Refusal> {
    let document = Document::parse(bytes)?;
    let page = first_page(&document)?;
    let [x0, y0, x1, y1] = page.media_box;
    let width = ((x1 - x0) * scale).ceil().max(1.0) as u32;
    let height = ((y1 - y0) * scale).ceil().max(1.0) as u32;
    let mut pixmap = Pixmap::new(width, height).ok_or("an empty or enormous page")?;
    let base = Transform::from_row(
        scale as f32,
        0.0,
        0.0,
        -scale as f32,
        (-x0 * scale) as f32,
        (y1 * scale) as f32,
    );
    let mut content = Vec::new();
    for part in &page.contents {
        content.extend(document.decoded(part)?);
        content.push(b'\n');
    }
    let mut painter = Painter {
        document: &document,
        base,
        width,
        height,
        depth: 0,
    };
    painter.run(&mut pixmap, &content, page.resources, State::new())?;
    let mut pixels = Vec::with_capacity(pixmap.data().len());
    for pixel in pixmap.pixels() {
        let c = pixel.demultiply();
        pixels.extend([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Ok(Bitmap {
        width,
        height,
        pixels,
    })
}

#[derive(Clone)]
struct State {
    ctm: Transform,
    clip: Option<Mask>,
    fill: [f32; 3],
    stroke: [f32; 3],
    fill_components: usize,
    stroke_components: usize,
    fill_alpha: f32,
    stroke_alpha: f32,
    line_width: f32,
    cap: LineCap,
    join: LineJoin,
    miter: f32,
    dash: Option<(Vec<f32>, f32)>,
}

impl State {
    fn new() -> State {
        State {
            ctm: Transform::identity(),
            clip: None,
            fill: [0.0; 3],
            stroke: [0.0; 3],
            fill_components: 1,
            stroke_components: 1,
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            line_width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter: 10.0,
            dash: None,
        }
    }
}

struct Painter<'a> {
    document: &'a Document,
    base: Transform,
    width: u32,
    height: u32,
    depth: usize,
}

fn colour(components: &[f64]) -> Result<[f32; 3], Refusal> {
    let c = |v: f64| v.clamp(0.0, 1.0) as f32;
    Ok(match *components {
        [gray] => [c(gray); 3],
        [r, g, b] => [c(r), c(g), c(b)],
        [cyan, magenta, yellow, black] => [
            (1.0 - c(cyan)) * (1.0 - c(black)),
            (1.0 - c(magenta)) * (1.0 - c(black)),
            (1.0 - c(yellow)) * (1.0 - c(black)),
        ],
        _ => return Err(format!("a colour of {} components", components.len())),
    })
}

fn paint(rgb: [f32; 3], alpha: f32) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(
        (rgb[0] * 255.0).round() as u8,
        (rgb[1] * 255.0).round() as u8,
        (rgb[2] * 255.0).round() as u8,
        (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
    );
    paint.anti_alias = true;
    paint
}

impl Painter<'_> {
    fn transform(&self, state: &State) -> Transform {
        self.base.pre_concat(state.ctm)
    }

    /// The number of components a colour space named `name` in `resources`
    /// has, or a refusal for one whose colours are not plain numbers.
    fn components(&self, name: &[u8], resources: Option<&Dict>) -> Result<usize, Refusal> {
        match name {
            b"DeviceGray" | b"G" | b"CalGray" => return Ok(1),
            b"DeviceRGB" | b"RGB" | b"CalRGB" => return Ok(3),
            b"DeviceCMYK" | b"CMYK" => return Ok(4),
            _ => {}
        }
        let space = resources
            .and_then(|r| self.document.dict(self.document.get(r, b"ColorSpace")))
            .map(|spaces| self.document.get(spaces, name))
            .unwrap_or(&Object::Null);
        let Object::Array(items) = space else {
            return Err(format!(
                "the colour space {}",
                String::from_utf8_lossy(name)
            ));
        };
        let family = items.first().and_then(|f| self.document.resolve(f).name());
        match family {
            Some(b"ICCBased") => {
                let stream = items
                    .get(1)
                    .and_then(|s| self.document.dict(s))
                    .ok_or("an ICC colour space with no profile")?;
                self.document
                    .get(stream, b"N")
                    .number()
                    .map(|n| n as usize)
                    .filter(|n| matches!(n, 1 | 3 | 4))
                    .ok_or_else(|| "an ICC profile of unusual size".into())
            }
            Some(b"CalGray") => Ok(1),
            Some(b"CalRGB") => Ok(3),
            Some(other) => Err(format!(
                "the {} colour space",
                String::from_utf8_lossy(other)
            )),
            None => Err("a malformed colour space".into()),
        }
    }

    fn graphics_state(
        &self,
        state: &mut State,
        name: &[u8],
        resources: Option<&Dict>,
    ) -> Result<(), Refusal> {
        let dict = resources
            .and_then(|r| self.document.dict(self.document.get(r, b"ExtGState")))
            .and_then(|states| self.document.dict(self.document.get(states, name)))
            .ok_or("a missing graphics state")?;
        for (key, value) in dict {
            let value = self.document.resolve(value);
            match key.as_slice() {
                b"ca" => state.fill_alpha = value.number().unwrap_or(1.0) as f32,
                b"CA" => state.stroke_alpha = value.number().unwrap_or(1.0) as f32,
                b"LW" => state.line_width = value.number().unwrap_or(1.0) as f32,
                b"LC" => state.cap = cap(value.number().unwrap_or(0.0)),
                b"LJ" => state.join = join(value.number().unwrap_or(0.0)),
                b"ML" => state.miter = value.number().unwrap_or(10.0) as f32,
                b"BM" => {
                    let normal = match value {
                        Object::Name(n) => matches!(n.as_slice(), b"Normal" | b"Compatible"),
                        Object::Array(items) => items
                            .iter()
                            .all(|i| matches!(i.name(), Some(b"Normal") | Some(b"Compatible"))),
                        _ => false,
                    };
                    if !normal {
                        return Err("a blend mode".into());
                    }
                }
                b"SMask" => {
                    if value.name() != Some(b"None") {
                        return Err("a soft mask".into());
                    }
                }
                b"Font" => return Err("text".into()),
                b"TR" | b"TR2"
                    if value.name() != Some(b"Identity") && value.name() != Some(b"Default") =>
                {
                    return Err("a transfer function".into());
                }
                // Overprint, flatness, smoothness, stroke adjustment, rendering
                // intent and alpha-is-shape (false) do not change an RGB raster.
                b"Type" | b"OP" | b"op" | b"OPM" | b"FL" | b"SM" | b"SA" | b"RI" | b"TK"
                | b"TR" | b"TR2" => {}
                b"AIS" if *value == Object::Bool(false) => {}
                other => {
                    return Err(format!(
                        "the graphics state key /{}",
                        String::from_utf8_lossy(other)
                    ));
                }
            }
        }
        Ok(())
    }

    fn fill(&self, pixmap: &mut Pixmap, state: &State, path: &Path, rule: FillRule) {
        let paint = paint(state.fill, state.fill_alpha);
        pixmap.fill_path(
            path,
            &paint,
            rule,
            self.transform(state),
            state.clip.as_ref(),
        );
    }

    fn stroke(&self, pixmap: &mut Pixmap, state: &State, path: &Path) {
        let paint = paint(state.stroke, state.stroke_alpha);
        let stroke = Stroke {
            width: state.line_width,
            miter_limit: state.miter,
            line_cap: state.cap,
            line_join: state.join,
            dash: state
                .dash
                .as_ref()
                .and_then(|(array, phase)| StrokeDash::new(array.clone(), *phase)),
        };
        pixmap.stroke_path(
            path,
            &paint,
            &stroke,
            self.transform(state),
            state.clip.as_ref(),
        );
    }

    fn clip(&self, state: &mut State, path: &Path, rule: FillRule) {
        let transform = self.transform(state);
        match &mut state.clip {
            Some(mask) => mask.intersect_path(path, rule, true, transform),
            None => {
                let Some(mut mask) = Mask::new(self.width, self.height) else {
                    return;
                };
                mask.fill_path(path, rule, true, transform);
                state.clip = Some(mask);
            }
        }
    }

    fn run(
        &mut self,
        pixmap: &mut Pixmap,
        content: &[u8],
        resources: Option<&Dict>,
        state: State,
    ) -> Result<(), Refusal> {
        let mut stack = vec![state];
        let mut operands: Vec<Object> = Vec::new();
        let mut path = PathBuilder::new();
        let mut pending_clip: Option<FillRule> = None;
        let mut lexer = Lexer::new(content, 0);
        loop {
            let Some(object) = lexer.object()? else {
                if lexer.peek().is_none() {
                    break;
                }
                return Err("a stray delimiter in content".into());
            };
            let Object::Operator(op) = object else {
                operands.push(object);
                continue;
            };
            let n = |i: usize| -> Result<f32, Refusal> {
                operands
                    .get(i)
                    .and_then(Object::number)
                    .map(|v| v as f32)
                    .ok_or_else(|| format!("bad operands to {}", String::from_utf8_lossy(&op)))
            };
            let state = stack.last_mut().expect("never pops the last");
            let mut paint_op: Option<(bool, Option<FillRule>, bool)> = None;
            match op.as_slice() {
                b"q" => {
                    let copy = state.clone();
                    stack.push(copy);
                }
                b"Q" => {
                    if stack.len() > 1 {
                        stack.pop();
                    }
                }
                b"cm" => {
                    let m = Transform::from_row(n(0)?, n(1)?, n(2)?, n(3)?, n(4)?, n(5)?);
                    state.ctm = state.ctm.pre_concat(m);
                }
                b"w" => state.line_width = n(0)?,
                b"J" => state.cap = cap(f64::from(n(0)?)),
                b"j" => state.join = join(f64::from(n(0)?)),
                b"M" => state.miter = n(0)?,
                b"d" => {
                    let array = match operands.first() {
                        Some(Object::Array(items)) => items
                            .iter()
                            .map(|i| i.number().map(|v| v as f32))
                            .collect::<Option<Vec<_>>>()
                            .ok_or("a malformed dash")?,
                        _ => return Err("a malformed dash".into()),
                    };
                    state.dash = (!array.is_empty()).then(|| (array, n(1).unwrap_or(0.0)));
                }
                // Rendering intent and flatness tolerance.
                b"ri" | b"i" => {}
                b"gs" => {
                    let name = operands.first().and_then(Object::name).ok_or("bad gs")?;
                    self.graphics_state(state, name, resources)?;
                }
                b"m" => path.move_to(n(0)?, n(1)?),
                b"l" => path.line_to(n(0)?, n(1)?),
                b"c" => path.cubic_to(n(0)?, n(1)?, n(2)?, n(3)?, n(4)?, n(5)?),
                b"v" => {
                    let from = path.last_point().ok_or("a curve with no current point")?;
                    path.cubic_to(from.x, from.y, n(0)?, n(1)?, n(2)?, n(3)?);
                }
                b"y" => path.cubic_to(n(0)?, n(1)?, n(2)?, n(3)?, n(2)?, n(3)?),
                b"h" => path.close(),
                b"re" => {
                    let (x, y, w, h) = (n(0)?, n(1)?, n(2)?, n(3)?);
                    path.move_to(x, y);
                    path.line_to(x + w, y);
                    path.line_to(x + w, y + h);
                    path.line_to(x, y + h);
                    path.close();
                }
                b"W" => pending_clip = Some(FillRule::Winding),
                b"W*" => pending_clip = Some(FillRule::EvenOdd),
                b"f" | b"F" => paint_op = Some((false, Some(FillRule::Winding), false)),
                b"f*" => paint_op = Some((false, Some(FillRule::EvenOdd), false)),
                b"S" => paint_op = Some((false, None, true)),
                b"s" => paint_op = Some((true, None, true)),
                b"B" => paint_op = Some((false, Some(FillRule::Winding), true)),
                b"B*" => paint_op = Some((false, Some(FillRule::EvenOdd), true)),
                b"b" => paint_op = Some((true, Some(FillRule::Winding), true)),
                b"b*" => paint_op = Some((true, Some(FillRule::EvenOdd), true)),
                b"n" => paint_op = Some((false, None, false)),
                b"g" => {
                    state.fill = colour(&[f64::from(n(0)?)])?;
                    state.fill_components = 1;
                }
                b"G" => {
                    state.stroke = colour(&[f64::from(n(0)?)])?;
                    state.stroke_components = 1;
                }
                b"rg" => {
                    state.fill = colour(&numbers(&operands))?;
                    state.fill_components = 3;
                }
                b"RG" => {
                    state.stroke = colour(&numbers(&operands))?;
                    state.stroke_components = 3;
                }
                b"k" => {
                    state.fill = colour(&numbers(&operands))?;
                    state.fill_components = 4;
                }
                b"K" => {
                    state.stroke = colour(&numbers(&operands))?;
                    state.stroke_components = 4;
                }
                b"cs" | b"CS" => {
                    let name = operands.first().and_then(Object::name).ok_or("bad cs")?;
                    let components = self.components(name, resources)?;
                    // A new colour space starts at its initial colour, black.
                    let black = [0.0; 3];
                    if op == b"cs" {
                        state.fill_components = components;
                        state.fill = black;
                    } else {
                        state.stroke_components = components;
                        state.stroke = black;
                    }
                }
                b"sc" | b"scn" | b"SC" | b"SCN" => {
                    if operands.iter().any(|o| matches!(o, Object::Name(_))) {
                        return Err("a pattern".into());
                    }
                    let values = numbers(&operands);
                    let fill = op[0] == b's';
                    let expected = match fill {
                        true => state.fill_components,
                        false => state.stroke_components,
                    };
                    if values.len() != expected {
                        return Err("a colour that does not match its space".into());
                    }
                    let rgb = colour(&values)?;
                    match fill {
                        true => state.fill = rgb,
                        false => state.stroke = rgb,
                    }
                }
                b"Do" => {
                    let name = operands.first().and_then(Object::name).ok_or("bad Do")?;
                    let state = state.clone();
                    self.xobject(pixmap, name, resources, state)?;
                }
                // Marked content and compatibility sections draw nothing.
                b"BMC" | b"BDC" | b"EMC" | b"MP" | b"DP" | b"BX" | b"EX" => {}
                b"BT" => return Err("text".into()),
                b"BI" => return Err("an inline image".into()),
                b"sh" => return Err("a shading".into()),
                b"d0" | b"d1" => return Err("a Type 3 glyph".into()),
                other => {
                    return Err(format!("the operator {}", String::from_utf8_lossy(other)));
                }
            }
            if let Some((close, fill, stroke)) = paint_op {
                if close {
                    path.close();
                }
                let built = std::mem::take(&mut path).finish();
                let state = stack.last_mut().expect("never pops the last");
                if let Some(built) = built {
                    if let Some(rule) = fill {
                        self.fill(pixmap, state, &built, rule);
                    }
                    if stroke {
                        self.stroke(pixmap, state, &built);
                    }
                    if let Some(rule) = pending_clip {
                        self.clip(state, &built, rule);
                    }
                } else if pending_clip.is_some() {
                    // An empty clip path leaves nothing visible.
                    state.clip = Mask::new(self.width, self.height);
                }
                pending_clip = None;
            }
            operands.clear();
        }
        Ok(())
    }

    fn xobject(
        &mut self,
        pixmap: &mut Pixmap,
        name: &[u8],
        resources: Option<&Dict>,
        mut state: State,
    ) -> Result<(), Refusal> {
        let document = self.document;
        let object = resources
            .and_then(|r| document.dict(document.get(r, b"XObject")))
            .map(|x| document.get(x, name))
            .ok_or("a missing XObject")?;
        let Object::Stream(dict, _) = object else {
            return Err("a malformed XObject".into());
        };
        match document.get(dict, b"Subtype").name() {
            Some(b"Form") => {}
            Some(b"Image") => return Err("an image".into()),
            _ => return Err("an unknown XObject".into()),
        }
        if self.depth > 16 {
            return Err("forms nested too deep".into());
        }
        if !matches!(document.get(dict, b"Ref"), Object::Null) {
            return Err("a reference XObject".into());
        }
        if let Some(m) = document.numbers(document.get(dict, b"Matrix"))
            && let [a, b, c, d, e, f] = m[..]
        {
            let m = Transform::from_row(a as f32, b as f32, c as f32, d as f32, e as f32, f as f32);
            state.ctm = state.ctm.pre_concat(m);
        }
        if let Some(bbox) = document.numbers(document.get(dict, b"BBox"))
            && let [x0, y0, x1, y1] = bbox[..]
            && let Some(rect) = Rect::from_ltrb(
                x0.min(x1) as f32,
                y0.min(y1) as f32,
                x0.max(x1) as f32,
                y0.max(y1) as f32,
            )
        {
            self.clip(&mut state, &PathBuilder::from_rect(rect), FillRule::Winding);
        }
        let content = document.decoded(object)?;
        let own = document
            .dict(document.get(dict, b"Resources"))
            .or(resources);
        let group = document.dict(document.get(dict, b"Group"));
        if let Some(group) = group {
            if document.get(group, b"S").name() != Some(b"Transparency") {
                return Err("an unknown group".into());
            }
            if *document.get(group, b"K") == Object::Bool(true) {
                return Err("a knockout group".into());
            }
        }
        self.depth += 1;
        let result = match group {
            Some(_) => {
                // Drawn on its own and composited with the alpha in force,
                // which is what a group's opacity is; with only the normal
                // blend mode, isolated and not are the same.
                let alpha = state.fill_alpha;
                let mask = state.clip.clone();
                let mut layer = Pixmap::new(self.width, self.height).ok_or("an enormous group")?;
                let mut inner = state;
                inner.fill_alpha = 1.0;
                inner.stroke_alpha = 1.0;
                let result = self.run(&mut layer, &content, own, inner);
                let paint = PixmapPaint {
                    opacity: alpha.clamp(0.0, 1.0),
                    ..PixmapPaint::default()
                };
                pixmap.draw_pixmap(
                    0,
                    0,
                    layer.as_ref(),
                    &paint,
                    Transform::identity(),
                    mask.as_ref(),
                );
                result
            }
            None => self.run(pixmap, &content, own, state),
        };
        self.depth -= 1;
        result
    }
}

fn numbers(operands: &[Object]) -> Vec<f64> {
    operands.iter().filter_map(Object::number).collect()
}

fn cap(value: f64) -> LineCap {
    match value as i64 {
        1 => LineCap::Round,
        2 => LineCap::Square,
        _ => LineCap::Butt,
    }
}

fn join(value: f64) -> LineJoin {
    match value as i64 {
        1 => LineJoin::Round,
        2 => LineJoin::Bevel,
        _ => LineJoin::Miter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page PDF around `content`, uncompressed.
    fn pdf(content: &str, resources: &str) -> Vec<u8> {
        format!(
            "%PDF-1.3\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
             2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 10 10] >>\nendobj\n\
             3 0 obj\n<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources {resources} >>\nendobj\n\
             4 0 obj\n<< /Length 5 0 R >>\nstream\n{content}\nendstream\nendobj\n\
             5 0 obj\n{}\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n",
            content.len()
        )
        .into_bytes()
    }

    fn pixel(bitmap: &Bitmap, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * bitmap.width + x) * 4) as usize;
        bitmap.pixels[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn page_size_is_the_inherited_media_box() {
        assert_eq!(page_size(&pdf("", "<< >>")), Some((10.0, 10.0)));
    }

    #[test]
    fn fills_a_rectangle_with_y_up() {
        let bitmap = render(&pdf("1 0 0 rg 0 0 10 5 re f", "<< >>"), 2.0).unwrap();
        assert_eq!((bitmap.width, bitmap.height), (20, 20));
        // The bottom half of the page is the bottom half of the bitmap.
        assert_eq!(pixel(&bitmap, 10, 15), [255, 0, 0, 255]);
        assert_eq!(pixel(&bitmap, 10, 5)[3], 0);
    }

    #[test]
    fn even_odd_clip_cuts_a_hole() {
        let content = "0 0 10 10 re 3 3 4 4 re W* n 0 g 0 0 10 10 re f";
        let bitmap = render(&pdf(content, "<< >>"), 1.0).unwrap();
        assert_eq!(pixel(&bitmap, 1, 1), [0, 0, 0, 255]);
        assert_eq!(pixel(&bitmap, 5, 5)[3], 0);
    }

    #[test]
    fn graphics_state_alpha_applies_to_fills() {
        let resources = "<< /ExtGState << /A << /ca 0.5 >> >> >>";
        let bitmap = render(&pdf("/A gs 0 0 10 10 re f", resources), 1.0).unwrap();
        assert_eq!(pixel(&bitmap, 5, 5)[3], 128);
    }

    #[test]
    fn icc_colour_space_takes_its_component_count() {
        let resources = "<< /ColorSpace << /Cs1 [/ICCBased 6 0 R] >> >>";
        let mut bytes = pdf("/Cs1 cs 0 0 1 sc 0 0 10 10 re f", resources);
        bytes.extend_from_slice(b"6 0 obj\n<< /N 3 /Length 0 >>\nstream\n\nendstream\nendobj\n");
        let bitmap = render(&bytes, 1.0).unwrap();
        assert_eq!(pixel(&bitmap, 5, 5), [0, 0, 255, 255]);
    }

    #[test]
    fn refuses_text() {
        assert!(render(&pdf("BT /F1 12 Tf (hi) Tj ET", "<< >>"), 1.0).is_none());
    }
}
