//! `NSAttributedString`'s HTML import: `-initWithHTML:options:documentAttributes:`
//! and its siblings.
//!
//! FIXME (ACCURACY): a subset of what WebKit's importer reads. Paragraphs,
//! breaks, lists, bold, italic, underline, links and character references are
//! kept; style sheets, inline `style` attributes, tables, images and fonts
//! named in the markup are not, and paragraph spacing is not carried as a
//! paragraph style.

use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;
use crate::tree_controller::{send_id, send_id_id};

/// How a run of imported text is drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Style {
    bold: bool,
    italic: bool,
    underline: bool,
    link: Option<String>,
}

/// Text that shares one [`Style`].
#[derive(Debug, PartialEq)]
pub(crate) struct Span {
    text: String,
    style: Style,
}

/// Elements whose content is never shown.
const HIDDEN: &[&str] = &["head", "script", "style", "title", "template"];

/// Elements that start and end a line of their own.
const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "dd",
    "div",
    "dl",
    "dt",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tr",
    "ul",
];

/// Elements that never have content or a closing tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

struct Importer {
    spans: Vec<Span>,
    /// Each open element and the style it began with.
    open: Vec<(String, Style)>,
    style: Style,
    hidden: usize,
    preformatted: usize,
    /// Each open list: `None` for bullets, or the number of the next item.
    lists: Vec<Option<usize>>,
    line_start: bool,
    /// The text ends in a collapsed space, dropped if the line ends there.
    space: bool,
}

impl Importer {
    fn new() -> Importer {
        Importer {
            spans: Vec::new(),
            open: Vec::new(),
            style: Style::default(),
            hidden: 0,
            preformatted: 0,
            lists: Vec::new(),
            line_start: true,
            space: false,
        }
    }

    fn emit(&mut self, text: &str) {
        match self.spans.last_mut() {
            Some(last) if last.style == self.style => last.text.push_str(text),
            _ => self.spans.push(Span {
                text: text.to_string(),
                style: self.style.clone(),
            }),
        }
    }

    /// Text between tags: runs of white space are one space, and none at the
    /// start or end of a line, unless inside `<pre>`.
    fn text(&mut self, text: &str) {
        if self.hidden > 0 {
            return;
        }
        if self.preformatted > 0 {
            self.emit(text);
            self.line_start = text.ends_with('\n');
            return;
        }
        for c in text.chars() {
            if c.is_ascii_whitespace() {
                if !self.line_start && !self.space {
                    self.emit(" ");
                    self.space = true;
                }
                continue;
            }
            self.emit(c.encode_utf8(&mut [0; 4]));
            self.line_start = false;
            self.space = false;
        }
    }

    fn line_break(&mut self) {
        if std::mem::take(&mut self.space)
            && let Some(last) = self.spans.last_mut()
        {
            last.text.pop();
            if last.text.is_empty() {
                self.spans.pop();
            }
        }
        self.emit("\n");
        self.line_start = true;
    }

    fn end_line(&mut self) {
        if !self.line_start {
            self.line_break();
        }
    }

    fn start(&mut self, name: &str, attributes: &[(String, String)]) {
        if HIDDEN.contains(&name) {
            self.hidden += 1;
        }
        if BLOCKS.contains(&name) {
            self.end_line();
        }
        match name {
            "br" => self.line_break(),
            "pre" => self.preformatted += 1,
            "ul" => self.lists.push(None),
            "ol" => self.lists.push(Some(1)),
            "li" if self.hidden == 0 => {
                let marker = match self.lists.last_mut() {
                    Some(Some(next)) => {
                        *next += 1;
                        format!("\t{}.\t", *next - 1)
                    }
                    _ => "\t\u{2022}\t".to_string(),
                };
                self.emit(&marker);
                self.line_start = true;
            }
            _ => {}
        }
        if VOID.contains(&name) {
            return;
        }
        self.open.push((name.to_string(), self.style.clone()));
        match name {
            "b" | "strong" => self.style.bold = true,
            "i" | "em" | "cite" | "var" => self.style.italic = true,
            "u" | "ins" => self.style.underline = true,
            "a" => {
                if let Some((_, href)) = attributes.iter().find(|(key, _)| key == "href") {
                    self.style.link = Some(href.clone());
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, name: &str) {
        let Some(at) = self.open.iter().rposition(|(open, _)| open == name) else {
            return;
        };
        // Closing an element closes everything opened inside it.
        for (open, style) in self.open.drain(at..).rev().collect::<Vec<_>>() {
            self.style = style;
            if HIDDEN.contains(&open.as_str()) {
                self.hidden -= 1;
            }
            match open.as_str() {
                "pre" => self.preformatted -= 1,
                "ul" | "ol" => {
                    self.lists.pop();
                }
                _ => {}
            }
            if BLOCKS.contains(&open.as_str()) {
                self.end_line();
            }
        }
    }
}

/// The value of a character reference without its `&` and `;`.
fn reference(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let value = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(value);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '\u{a9}',
        "reg" => '\u{ae}',
        "trade" => '\u{2122}',
        "hellip" => '\u{2026}',
        "ndash" => '\u{2013}',
        "mdash" => '\u{2014}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "bull" => '\u{2022}',
        "middot" => '\u{b7}',
        "deg" => '\u{b0}',
        "times" => '\u{d7}',
        "euro" => '\u{20ac}',
        _ => return None,
    })
}

/// `text` with its character references replaced; one that names nothing
/// is kept as written, as browsers do.
fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest[1..]
            .find(';')
            .filter(|end| *end <= 32)
            .and_then(|end| Some((reference(&rest[1..1 + end])?, end + 2)));
        match decoded {
            Some((c, used)) => {
                out.push(c);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A tag's name, lowercased, and its attributes, from the text between `<`
/// and `>`.
fn tag(inner: &str) -> (String, Vec<(String, String)>) {
    let inner = inner.trim_end_matches('/');
    let name_end = inner
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(inner.len());
    let name = inner[..name_end].to_ascii_lowercase();
    let mut attributes = Vec::new();
    let mut rest = inner[name_end..].trim_start();
    while !rest.is_empty() {
        let key_end = rest
            .find(|c: char| c == '=' || c.is_ascii_whitespace())
            .unwrap_or(rest.len());
        let key = rest[..key_end].to_ascii_lowercase();
        rest = rest[key_end..].trim_start();
        let mut value = String::new();
        if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            let (raw, remaining) = match after.chars().next() {
                Some(quote @ ('"' | '\'')) => {
                    let body = &after[1..];
                    let end = body.find(quote).unwrap_or(body.len());
                    (&body[..end], body.get(end + 1..).unwrap_or(""))
                }
                _ => {
                    let end = after
                        .find(|c: char| c.is_ascii_whitespace())
                        .unwrap_or(after.len());
                    (&after[..end], &after[end..])
                }
            };
            value = decode(raw);
            rest = remaining.trim_start();
        }
        if !key.is_empty() {
            attributes.push((key, value));
        }
    }
    (name, attributes)
}

/// The runs of text `html` shows.
pub(crate) fn parse(html: &str) -> Vec<Span> {
    let mut importer = Importer::new();
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        importer.text(&decode(&rest[..at]));
        rest = &rest[at..];
        let skip_to = |rest: &str, end: &str| rest.find(end).map_or(rest.len(), |i| i + end.len());
        if rest.starts_with("<!--") {
            rest = &rest[skip_to(rest, "-->")..];
            continue;
        }
        let Some(close) = rest.find('>') else {
            importer.text(&decode(rest));
            rest = "";
            break;
        };
        let inner = &rest[1..close];
        rest = &rest[close + 1..];
        if inner.starts_with(['!', '?']) {
            continue;
        }
        match inner.strip_prefix('/') {
            Some(name) => importer.end(&name.trim().to_ascii_lowercase()),
            None => {
                let (name, attributes) = tag(inner);
                importer.start(&name, &attributes);
            }
        }
    }
    importer.text(&decode(rest));
    importer.spans
}

/// `NSStringEncoding`s this reads a document in.
const UTF8: usize = 4;
const WINDOWS_LATIN1: usize = 12;

/// The encoding the options name, or the document's own `charset`, or
/// Windows Latin 1, which is what a browser assumes of a page that says none.
unsafe fn encoding_of(bytes: &[u8], options: Id) -> usize {
    if !options.is_null() {
        let named = send_id_id(
            options,
            "objectForKey:",
            foundation::string::from_str("CharacterEncoding"),
        );
        if !named.is_null() {
            // SAFETY: `-unsignedIntegerValue` is `NSUInteger (id, SEL)`.
            return unsafe {
                let f: unsafe extern "C" fn(Id, Sel) -> usize =
                    std::mem::transmute(objc::msg::entry());
                f(named, objc::sel::get("unsignedIntegerValue"))
            };
        }
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    match head.contains("charset=utf-8") || head.contains("charset=\"utf-8\"") {
        true => UTF8,
        false => WINDOWS_LATIN1,
    }
}

/// The font a run is drawn in: Times, the importer's default face, at its
/// default 12 points.
fn font_for(style: &Style) -> Id {
    let name = match (style.bold, style.italic) {
        (false, false) => "Times-Roman",
        (true, false) => "Times-Bold",
        (false, true) => "Times-Italic",
        (true, true) => "Times-BoldItalic",
    };
    let class = objc::class::ptr(objc::class::lookup(c"NSFont").expect("NSFont is registered"));
    // SAFETY: `+fontWithName:size:` is `NSFont * (Class, SEL, NSString *, CGFloat)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, f64) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            class as Id,
            objc::sel::get("fontWithName:size:"),
            foundation::string::from_str(name),
            12.0,
        )
    }
}

fn add_attribute(target: Id, name: &str, value: Id, location: usize, length: usize) {
    if value.is_null() {
        return;
    }
    // SAFETY: `-addAttribute:value:range:` is `void (id, SEL, NSString *, id, NSRange)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, usize, usize) =
            std::mem::transmute(objc::msg::entry());
        f(
            target,
            objc::sel::get("addAttribute:value:range:"),
            foundation::string::from_str(name),
            value,
            location,
            length,
        );
    }
}

/// `NSUnderlineStyleSingle` as the number an attribute holds.
fn single_underline() -> Id {
    const NS_UNDERLINE_STYLE_SINGLE: isize = 1;
    let class = objc::class::ptr(objc::class::lookup(c"NSNumber").expect("NSNumber")) as Id;
    // SAFETY: `+numberWithInteger:` is `NSNumber * (Class, SEL, NSInteger)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, isize) -> Id = std::mem::transmute(objc::msg::entry());
        f(
            class,
            objc::sel::get("numberWithInteger:"),
            NS_UNDERLINE_STYLE_SINGLE,
        )
    }
}

/// The attributed string `spans` make, autoreleased and mutable.
fn attributed(spans: &[Span]) -> Id {
    let text: String = spans.iter().map(|span| span.text.as_str()).collect();
    let class = objc::class::ptr(
        objc::class::lookup(c"NSMutableAttributedString").expect("Foundation registered it"),
    ) as Id;
    let made = send_id_id(
        send_id(class, "alloc"),
        "initWithString:",
        foundation::string::from_str(&text),
    );
    let url_class = objc::class::ptr(objc::class::lookup(c"NSURL").expect("NSURL")) as Id;
    let mut location = 0;
    for span in spans {
        let length = span.text.encode_utf16().count();
        add_attribute(made, "NSFont", font_for(&span.style), location, length);
        if span.style.underline {
            add_attribute(made, "NSUnderline", single_underline(), location, length);
        }
        if let Some(href) = &span.style.link {
            let href = foundation::string::from_str(href);
            let url = send_id_id(url_class, "URLWithString:", href);
            let link = if url.is_null() { href } else { url };
            add_attribute(made, "NSLink", link, location, length);
        }
        location += length;
    }
    objc::arc::autorelease(made)
}

/// `-initWithHTML:options:documentAttributes:`.
unsafe extern "C" fn init_with_html_options(
    receiver: Id,
    _sel: Sel,
    data: Id,
    options: Id,
    document_attributes: *mut Id,
) -> Id {
    if data.is_null() {
        objc::arc::release(receiver);
        return std::ptr::null_mut();
    }
    // SAFETY: `-bytes` and `-length` of an `NSData`.
    let bytes = unsafe {
        let pointer: unsafe extern "C" fn(Id, Sel) -> *const u8 =
            std::mem::transmute(objc::msg::entry());
        let length: unsafe extern "C" fn(Id, Sel) -> usize =
            std::mem::transmute(objc::msg::entry());
        let n = length(data, objc::sel::get("length"));
        match n {
            0 => &[][..],
            n => std::slice::from_raw_parts(pointer(data, objc::sel::get("bytes")), n),
        }
    };
    // SAFETY: the caller's options dictionary or nil.
    let encoding = unsafe { encoding_of(bytes, options) };
    let string_class = objc::class::ptr(corefoundation::string::class()) as Id;
    // SAFETY: `-initWithData:encoding:` is `id (id, SEL, NSData *, NSStringEncoding)`.
    let text = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, usize) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            send_id(string_class, "alloc"),
            objc::sel::get("initWithData:encoding:"),
            data,
            encoding,
        )
    };
    if text.is_null() {
        objc::arc::release(receiver);
        return std::ptr::null_mut();
    }
    // SAFETY: a string this just made.
    let html = String::from_utf8_lossy(&unsafe { foundation::string::bytes(text) }).into_owned();
    objc::arc::release(text);
    if !document_attributes.is_null() {
        let dictionary_class =
            objc::class::ptr(objc::class::lookup(c"NSDictionary").expect("NSDictionary")) as Id;
        // SAFETY: the caller's out-pointer; `+dictionaryWithObject:forKey:` is
        // `id (Class, SEL, id, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            *document_attributes = f(
                dictionary_class,
                objc::sel::get("dictionaryWithObject:forKey:"),
                foundation::string::from_str("NSHTML"),
                foundation::string::from_str("DocumentType"),
            );
        }
    }
    send_id_id(
        receiver,
        "initWithAttributedString:",
        attributed(&parse(&html)),
    )
}

/// `-initWithHTML:documentAttributes:`.
unsafe extern "C" fn init_with_html(
    receiver: Id,
    sel: Sel,
    data: Id,
    document_attributes: *mut Id,
) -> Id {
    // SAFETY: forwarded with no options.
    unsafe {
        init_with_html_options(
            receiver,
            sel,
            data,
            std::ptr::null_mut(),
            document_attributes,
        )
    }
}

/// `-initWithHTML:baseURL:documentAttributes:`. Links are kept as written,
/// so the base matters only to relative ones.
///
/// FIXME (ACCURACY): a relative link is not resolved against `base`.
unsafe extern "C" fn init_with_html_base_url(
    receiver: Id,
    sel: Sel,
    data: Id,
    _base: Id,
    document_attributes: *mut Id,
) -> Id {
    // SAFETY: forwarded with no options.
    unsafe {
        init_with_html_options(
            receiver,
            sel,
            data,
            std::ptr::null_mut(),
            document_attributes,
        )
    }
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSAttributedString").expect("Foundation registered it");
    methods!(class, {
        "initWithHTML:options:documentAttributes:" => init_with_html_options, c"@40@0:8@16@24^@32";
        "initWithHTML:documentAttributes:" => init_with_html, c"@32@0:8@16^@24";
        "initWithHTML:baseURL:documentAttributes:" => init_with_html_base_url, c"@40@0:8@16@24^@32";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(html: &str) -> String {
        parse(html).iter().map(|span| span.text.as_str()).collect()
    }

    #[test]
    fn paragraphs_and_breaks_end_lines_and_white_space_collapses() {
        assert_eq!(
            texts("<p>One\n  two</p><p>Three<br>four</p>"),
            "One two\nThree\nfour\n"
        );
    }

    #[test]
    fn character_references_are_decoded() {
        assert_eq!(
            texts("a &amp; b &lt;c&gt; &#233;&#x41; &bogus;"),
            "a & b <c> \u{e9}A &bogus;"
        );
    }

    #[test]
    fn list_items_carry_markers() {
        assert_eq!(
            texts("<ul><li>a</li><li>b</li></ul><ol><li>c</li><li>d</li></ol>"),
            "\t\u{2022}\ta\n\t\u{2022}\tb\n\t1.\tc\n\t2.\td\n"
        );
    }

    #[test]
    fn inline_elements_style_their_text() {
        let spans = parse("x <b>bold <i>both</i></b> <a href=\"https://example.com\">link</a>");
        let styled: Vec<_> = spans
            .iter()
            .map(|s| {
                (
                    s.text.as_str(),
                    s.style.bold,
                    s.style.italic,
                    s.style.link.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            styled,
            [
                ("x ", false, false, None),
                ("bold ", true, false, None),
                ("both", true, true, None),
                (" ", false, false, None),
                ("link", false, false, Some("https://example.com")),
            ]
        );
    }

    #[test]
    fn hidden_elements_and_comments_show_nothing() {
        assert_eq!(
            texts("<head><title>t</title><style>p{}</style></head><!-- c --><p>shown</p>"),
            "shown\n"
        );
    }
}
