use std::collections::BTreeMap;

/// One value in a property list.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    Real(f64),
    String(String),
    Data(Vec<u8>),
    /// Seconds since 2001-01-01, which is Apple's epoch.
    Date(f64),
    Array(Vec<Value>),
    Dictionary(BTreeMap<String, Value>),
    /// A reference into the object table, which only an archive uses.
    Uid(u64),
    /// A marker this reader does not decode, kept so it can be named.
    Unsupported(u8),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_dictionary(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Dictionary(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Integer(v) => Some(*v),
            Value::Uid(v) => Some(*v as i64),
            _ => None,
        }
    }

    /// The value at `key`, for a dictionary.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_dictionary()?.get(key)
    }
}

/// What went wrong, as a sentence naming the offset.
#[derive(Debug, PartialEq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn bad(what: impl std::fmt::Display) -> Error {
    Error(what.to_string())
}

/// Parse a property list, in whichever of the two spellings it is written.
pub fn parse(bytes: &[u8]) -> Result<Value, Error> {
    match bytes.starts_with(b"bplist00") {
        true => binary::parse(bytes),
        false => xml::parse(bytes),
    }
}

/// The binary spelling: a trailer that says how wide the offsets are, a table of
/// them, and one tagged object at each.
mod binary {
    use super::*;

    struct Reader<'a> {
        bytes: &'a [u8],
        offsets: Vec<usize>,
        ref_size: usize,
    }

    pub fn parse(bytes: &[u8]) -> Result<Value, Error> {
        // The trailer is the last 32 bytes: six unused, then the offset width,
        // the reference width, the object count, the root, and where the offset
        // table starts.
        if bytes.len() < 40 {
            return Err(bad("a binary plist is at least forty bytes"));
        }
        let trailer = &bytes[bytes.len() - 32..];
        let offset_size = trailer[6] as usize;
        let ref_size = trailer[7] as usize;
        let count = be(&trailer[8..16]) as usize;
        let root = be(&trailer[16..24]) as usize;
        let table = be(&trailer[24..32]) as usize;
        if offset_size == 0 || offset_size > 8 || ref_size == 0 || ref_size > 8 {
            return Err(bad("a binary plist trailer with an impossible width"));
        }
        let end = table
            .checked_add(count * offset_size)
            .ok_or_else(|| bad("an offset table past the end of the file"))?;
        if end > bytes.len() {
            return Err(bad("an offset table past the end of the file"));
        }

        let offsets: Vec<usize> = (0..count)
            .map(|i| be(&bytes[table + i * offset_size..][..offset_size]) as usize)
            .collect();
        let reader = Reader {
            bytes,
            offsets,
            ref_size,
        };
        reader.object(root, 0)
    }

    /// A big-endian integer of one to eight bytes.
    fn be(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0u64, |v, b| (v << 8) | *b as u64)
    }

    /// How deep a plist may nest.
    ///
    /// A file with a cycle in its offset table would otherwise recurse until the
    /// stack ran out, and a bundle is something this runtime reads from disk on
    /// a guest's say-so.
    const MAX_DEPTH: usize = 64;

    impl Reader<'_> {
        fn object(&self, index: usize, depth: usize) -> Result<Value, Error> {
            if depth > MAX_DEPTH {
                return Err(bad("a plist nested past sixty-four levels"));
            }
            let at = *self
                .offsets
                .get(index)
                .ok_or_else(|| bad(format!("object {index} is not in the offset table")))?;
            let marker = *self
                .bytes
                .get(at)
                .ok_or_else(|| bad(format!("object {index} is past the end of the file")))?;
            let (kind, low) = (marker >> 4, (marker & 0x0f) as usize);
            match kind {
                0x0 => Ok(match low {
                    0 => Value::Null,
                    8 => Value::Bool(false),
                    9 => Value::Bool(true),
                    _ => Value::Unsupported(marker),
                }),
                // An integer's length is 2^low bytes, and only an eight-byte one
                // is signed - which is why a `1 << 3` is read as `i64` and the
                // rest are not.
                0x1 => {
                    let width = 1usize << low;
                    let raw = be(self.slice(at + 1, width)?);
                    Ok(Value::Integer(match width {
                        8 => raw as i64,
                        _ => raw as i64,
                    }))
                }
                0x2 => {
                    let width = 1usize << low;
                    let raw = self.slice(at + 1, width)?;
                    Ok(Value::Real(match width {
                        4 => f32::from_be_bytes(raw.try_into().unwrap_or_default()) as f64,
                        8 => f64::from_be_bytes(raw.try_into().unwrap_or_default()),
                        _ => return Err(bad("a real that is neither four nor eight bytes")),
                    }))
                }
                0x3 => Ok(Value::Date(f64::from_be_bytes(
                    self.slice(at + 1, 8)?.try_into().unwrap_or_default(),
                ))),
                0x4 => {
                    let (len, body) = self.counted(at, low)?;
                    Ok(Value::Data(self.slice(body, len)?.to_vec()))
                }
                // ASCII, one byte a character.
                0x5 => {
                    let (len, body) = self.counted(at, low)?;
                    Ok(Value::String(
                        String::from_utf8_lossy(self.slice(body, len)?).into_owned(),
                    ))
                }
                // UTF-16, big-endian, `len` *characters* rather than bytes.
                0x6 => {
                    let (len, body) = self.counted(at, low)?;
                    let raw = self.slice(body, len * 2)?;
                    let units: Vec<u16> = raw
                        .chunks_exact(2)
                        .map(|c| u16::from_be_bytes([c[0], c[1]]))
                        .collect();
                    Ok(Value::String(String::from_utf16_lossy(&units)))
                }
                0x8 => Ok(Value::Uid(be(self.slice(at + 1, low + 1)?))),
                0xa | 0xc => {
                    let (len, body) = self.counted(at, low)?;
                    let mut out = Vec::with_capacity(len);
                    for i in 0..len {
                        let r = be(self.slice(body + i * self.ref_size, self.ref_size)?) as usize;
                        out.push(self.object(r, depth + 1)?);
                    }
                    Ok(Value::Array(out))
                }
                0xd => {
                    let (len, body) = self.counted(at, low)?;
                    let mut out = BTreeMap::new();
                    for i in 0..len {
                        let k = be(self.slice(body + i * self.ref_size, self.ref_size)?) as usize;
                        let v = be(self.slice(body + (len + i) * self.ref_size, self.ref_size)?)
                            as usize;
                        let key = match self.object(k, depth + 1)? {
                            Value::String(s) => s,
                            // A non-string key is legal in the format and means
                            // nothing to a reader of a bundle; named rather than
                            // dropped.
                            other => format!("<non-string key {other:?}>"),
                        };
                        out.insert(key, self.object(v, depth + 1)?);
                    }
                    Ok(Value::Dictionary(out))
                }
                _ => Ok(Value::Unsupported(marker)),
            }
        }

        /// The length and the body offset of a counted object.
        ///
        /// A `low` of fifteen means the count did not fit in the nibble and is
        /// an integer object that follows.
        fn counted(&self, at: usize, low: usize) -> Result<(usize, usize), Error> {
            if low != 0x0f {
                return Ok((low, at + 1));
            }
            let marker = *self
                .bytes
                .get(at + 1)
                .ok_or_else(|| bad("a counted object with no count"))?;
            if marker >> 4 != 0x1 {
                return Err(bad("a counted object whose count is not an integer"));
            }
            let width = 1usize << (marker & 0x0f);
            Ok((be(self.slice(at + 2, width)?) as usize, at + 2 + width))
        }

        fn slice(&self, at: usize, len: usize) -> Result<&[u8], Error> {
            self.bytes
                .get(at..at + len)
                .ok_or_else(|| bad(format!("a value at {at} runs past the end of the file")))
        }
    }
}

/// The XML spelling, read by shape rather than by a general parser.
///
/// Enough for an `Info.plist` a person edited: the tags are a fixed set and they
/// nest, so a stack of containers and a tag scanner is the whole of it.
mod xml {
    use super::*;

    pub fn parse(bytes: &[u8]) -> Result<Value, Error> {
        let text: String = String::from_utf8_lossy(bytes).into_owned();
        let mut stack: Vec<Value> = Vec::new();
        let mut keys: Vec<Option<String>> = Vec::new();
        let mut root = None;
        let mut rest: &str = &text;

        while let Some(open) = rest.find('<') {
            let close = match rest[open..].find('>') {
                Some(at) => open + at,
                None => break,
            };
            let tag = &rest[open + 1..close];
            let body_start = close + 1;
            let body_end = rest[body_start..].find('<').map(|at| body_start + at);
            let body = body_end.map(|end| &rest[body_start..end]).unwrap_or("");

            let mut push =
                |value: Value, stack: &mut Vec<Value>, keys: &mut Vec<Option<String>>| match stack
                    .last_mut()
                {
                    Some(Value::Array(a)) => a.push(value),
                    Some(Value::Dictionary(d)) => {
                        if let Some(Some(key)) = keys.last_mut().map(std::mem::take) {
                            d.insert(key, value);
                        }
                    }
                    _ => root = Some(value),
                };

            match tag {
                "dict" => {
                    stack.push(Value::Dictionary(BTreeMap::new()));
                    keys.push(None);
                }
                "array" => {
                    stack.push(Value::Array(Vec::new()));
                    keys.push(None);
                }
                "/dict" | "/array" => {
                    let done = stack
                        .pop()
                        .ok_or_else(|| bad("a closing tag with nothing open"))?;
                    keys.pop();
                    push(done, &mut stack, &mut keys);
                }
                "key" => {
                    if let Some(slot) = keys.last_mut() {
                        *slot = Some(unescape(body));
                    }
                }
                "string" => push(Value::String(unescape(body)), &mut stack, &mut keys),
                "string/" => push(Value::String(String::new()), &mut stack, &mut keys),
                "dict/" => push(Value::Dictionary(BTreeMap::new()), &mut stack, &mut keys),
                "array/" => push(Value::Array(Vec::new()), &mut stack, &mut keys),
                "data" => push(Value::Data(base64_decode(body)), &mut stack, &mut keys),
                "data/" => push(Value::Data(Vec::new()), &mut stack, &mut keys),
                "date" => push(
                    Value::Date(parse_date(body.trim()).unwrap_or(0.0)),
                    &mut stack,
                    &mut keys,
                ),
                "integer" => push(
                    Value::Integer(body.trim().parse().unwrap_or(0)),
                    &mut stack,
                    &mut keys,
                ),
                "real" => push(
                    Value::Real(body.trim().parse().unwrap_or(0.0)),
                    &mut stack,
                    &mut keys,
                ),
                "true/" => push(Value::Bool(true), &mut stack, &mut keys),
                "false/" => push(Value::Bool(false), &mut stack, &mut keys),
                _ => {}
            }
            rest = &rest[close + 1..];
        }
        root.ok_or_else(|| bad("an XML plist with no root value"))
    }

    fn unescape(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find('&') {
            out.push_str(&rest[..at]);
            rest = &rest[at..];
            let Some(end) = rest.find(';') else { break };
            let entity = &rest[1..end];
            let decoded = match entity {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "amp" => Some('&'),
                _ => entity.strip_prefix('#').and_then(|n| {
                    match n.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => n.parse().ok(),
                    }
                    .and_then(char::from_u32)
                }),
            };
            match decoded {
                Some(c) => {
                    out.push(c);
                    rest = &rest[end + 1..];
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

    /// `2001-01-01T00:00:00Z`, as seconds since that instant.
    pub(super) fn parse_date(text: &str) -> Option<f64> {
        let (date, time) = text.strip_suffix('Z')?.split_once('T')?;
        let mut d = date.split('-').map(|n| n.parse::<i64>().ok());
        let (y, m, day) = (d.next()??, d.next()??, d.next()??);
        let mut t = time.split(':').map(|n| n.parse::<f64>().ok());
        let (h, min, sec) = (t.next()??, t.next()??, t.next()??);
        // Days from civil, the standard proleptic Gregorian algorithm.
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146097 + doe - 719468;
        // 2001-01-01 is 11323 days after the Unix epoch.
        Some((days - 11323) as f64 * 86400.0 + h * 3600.0 + min * 60.0 + sec)
    }

    fn base64_decode(text: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len() / 4 * 3);
        let (mut buffer, mut bits) = (0u32, 0);
        for byte in text.bytes() {
            let sextet = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => continue,
            };
            buffer = (buffer << 6) | sextet as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buffer >> bits) as u8);
                buffer &= (1 << bits) - 1;
            }
        }
        out
    }
}

/// The binary spelling of `value` (`bplist00`), which is what
/// `kCFPropertyListBinaryFormat_v1_0` asks for. Objects are not shared: each
/// value is written where it occurs.
pub fn write_binary(value: &Value) -> Vec<u8> {
    /// One object to write: a value, or a dictionary key, which is a string
    /// the dictionary holds as text rather than as a `Value`.
    enum Item<'a> {
        Value(&'a Value),
        Key(&'a str),
    }
    // Every object, flattened, with the key and value references each
    // container holds.
    fn flatten<'a>(
        value: &'a Value,
        objects: &mut Vec<(Item<'a>, Vec<usize>, Vec<usize>)>,
    ) -> usize {
        let at = objects.len();
        objects.push((Item::Value(value), Vec::new(), Vec::new()));
        match value {
            Value::Array(items) => {
                let refs: Vec<usize> = items.iter().map(|v| flatten(v, objects)).collect();
                objects[at].2 = refs;
            }
            Value::Dictionary(entries) => {
                let mut keys = Vec::new();
                let mut values = Vec::new();
                for (key, item) in entries {
                    keys.push(objects.len());
                    objects.push((Item::Key(key), Vec::new(), Vec::new()));
                    values.push(flatten(item, objects));
                }
                objects[at].1 = keys;
                objects[at].2 = values;
            }
            _ => {}
        }
        at
    }
    let mut objects = Vec::new();
    flatten(value, &mut objects);
    let width = |max: u64| match max {
        0..=0xFF => 1u8,
        0x100..=0xFFFF => 2,
        0x1_0000..=0xFFFF_FFFF => 4,
        _ => 8,
    };
    let ref_size = width(objects.len() as u64);
    let push_sized = |out: &mut Vec<u8>, value: u64, size: u8| {
        out.extend_from_slice(&value.to_be_bytes()[8 - size as usize..]);
    };
    let push_header = |out: &mut Vec<u8>, marker: u8, count: usize| {
        if count < 15 {
            out.push(marker | count as u8);
        } else {
            out.push(marker | 0x0F);
            let size = width(count as u64);
            out.push(0x10 | size.trailing_zeros() as u8);
            push_sized(out, count as u64, size);
        }
    };
    let mut out = b"bplist00".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (item, keys, refs) in &objects {
        offsets.push(out.len() as u64);
        let key;
        let value = match item {
            Item::Value(value) => *value,
            Item::Key(text) => {
                key = Value::String(text.to_string());
                &key
            }
        };
        match value {
            Value::Null => out.push(0x00),
            Value::Bool(false) => out.push(0x08),
            Value::Bool(true) => out.push(0x09),
            Value::Integer(v) => {
                let size = match *v < 0 {
                    true => 8,
                    false => width(*v as u64),
                };
                out.push(0x10 | size.trailing_zeros() as u8);
                push_sized(&mut out, *v as u64, size);
            }
            Value::Real(v) => {
                out.push(0x23);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Value::Date(v) => {
                out.push(0x33);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Value::Data(bytes) => {
                push_header(&mut out, 0x40, bytes.len());
                out.extend_from_slice(bytes);
            }
            Value::String(text) if text.is_ascii() => {
                push_header(&mut out, 0x50, text.len());
                out.extend_from_slice(text.as_bytes());
            }
            Value::String(text) => {
                let units: Vec<u16> = text.encode_utf16().collect();
                push_header(&mut out, 0x60, units.len());
                for unit in units {
                    out.extend_from_slice(&unit.to_be_bytes());
                }
            }
            Value::Uid(v) => {
                let size = width(*v);
                out.push(0x80 | (size - 1));
                push_sized(&mut out, *v, size);
            }
            Value::Array(_) => {
                push_header(&mut out, 0xA0, refs.len());
                for r in refs {
                    push_sized(&mut out, *r as u64, ref_size);
                }
            }
            Value::Dictionary(_) => {
                push_header(&mut out, 0xD0, refs.len());
                for r in keys.iter().chain(refs) {
                    push_sized(&mut out, *r as u64, ref_size);
                }
            }
            // Nothing a writer is handed holds one: it is only ever read.
            Value::Unsupported(_) => out.push(0x00),
        }
    }
    let table_at = out.len() as u64;
    let offset_size = width(table_at);
    for offset in &offsets {
        push_sized(&mut out, *offset, offset_size);
    }
    out.extend_from_slice(&[0; 6]);
    out.push(offset_size);
    out.push(ref_size);
    out.extend_from_slice(&(objects.len() as u64).to_be_bytes());
    out.extend_from_slice(&0u64.to_be_bytes());
    out.extend_from_slice(&table_at.to_be_bytes());
    out
}

/// The XML spelling of `value`, which is what Apple's own tools read and what
/// `defaults read` shows. Dictionary keys come out sorted.
pub fn write_xml(value: &Value) -> Vec<u8> {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n",
    );
    write_value(&mut out, value, 0);
    out.push_str("</plist>\n");
    out.into_bytes()
}

fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            // XML 1.0 cannot carry most control characters at all.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
}

fn write_value(out: &mut String, value: &Value, depth: usize) {
    let pad = "\t".repeat(depth);
    out.push_str(&pad);
    match value {
        Value::Bool(true) => out.push_str("<true/>\n"),
        Value::Bool(false) => out.push_str("<false/>\n"),
        Value::Integer(v) => out.push_str(&format!("<integer>{v}</integer>\n")),
        Value::Uid(v) => out.push_str(&format!("<integer>{v}</integer>\n")),
        Value::Real(v) if v.is_nan() => out.push_str("<real>nan</real>\n"),
        Value::Real(v) if v.is_infinite() => out.push_str(match *v > 0.0 {
            true => "<real>+infinity</real>\n",
            false => "<real>-infinity</real>\n",
        }),
        Value::Real(v) => out.push_str(&format!("<real>{v:?}</real>\n")),
        Value::String(text) => {
            out.push_str("<string>");
            escape(text, out);
            out.push_str("</string>\n");
        }
        Value::Data(bytes) => {
            out.push_str("<data>");
            out.push_str(&base64_encode(bytes));
            out.push_str("</data>\n");
        }
        Value::Date(interval) => {
            out.push_str(&format!("<date>{}</date>\n", format_date(*interval)))
        }
        Value::Array(items) if items.is_empty() => out.push_str("<array/>\n"),
        Value::Array(items) => {
            out.push_str("<array>\n");
            for item in items {
                write_value(out, item, depth + 1);
            }
            out.push_str(&pad);
            out.push_str("</array>\n");
        }
        Value::Dictionary(entries) if entries.is_empty() => out.push_str("<dict/>\n"),
        Value::Dictionary(entries) => {
            out.push_str("<dict>\n");
            for (key, item) in entries {
                out.push_str(&pad);
                out.push_str("\t<key>");
                escape(key, out);
                out.push_str("</key>\n");
                write_value(out, item, depth + 1);
            }
            out.push_str(&pad);
            out.push_str("</dict>\n");
        }
        // Not property list values; written as an empty string rather than
        // producing a document nothing can read.
        Value::Null | Value::Unsupported(_) => out.push_str("<string></string>\n"),
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            match i <= chunk.len() {
                true => out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char),
                false => out.push('='),
            }
        }
    }
    out
}

fn format_date(interval: f64) -> String {
    let total = interval.floor();
    let seconds = total.rem_euclid(86400.0) as i64;
    // Days since 1970-01-01, then civil-from-days.
    let days = (total / 86400.0).floor() as i64 + 11323;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_xml_plist_reads_the_keys_a_bundle_needs() {
        let text = br#"<?xml version="1.0"?>
        <plist version="1.0">
        <dict>
            <key>CFBundleExecutable</key><string>Mac_Runner</string>
            <key>NSPrincipalClass</key><string>YYApplication</string>
            <key>NSMainNibFile</key><string>MainMenu</string>
            <key>LSMinimumSystemVersion</key><string>10.9.0</string>
            <key>CFBundleSupportedPlatforms</key>
            <array><string>MacOSX</string></array>
            <key>NSHighResolutionCapable</key><true/>
        </dict>
        </plist>"#;
        let value = parse(text).expect("an XML plist");
        assert_eq!(
            value.get("CFBundleExecutable").and_then(Value::as_str),
            Some("Mac_Runner")
        );
        assert_eq!(
            value.get("NSPrincipalClass").and_then(Value::as_str),
            Some("YYApplication")
        );
        assert_eq!(
            value.get("NSHighResolutionCapable"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            value
                .get("CFBundleSupportedPlatforms")
                .and_then(Value::as_array),
            Some(&[Value::String("MacOSX".into())][..])
        );
    }

    /// A binary plist built here rather than checked in: the point is the
    /// decoding, and a fixture on disk would only move the question.
    #[test]
    fn a_binary_plist_reads_the_same_shape() {
        // { "a": 1, "b": "two" } - one dictionary, two ASCII keys, an integer
        // and a string, laid out by hand against the format above.
        let mut out = Vec::new();
        out.extend_from_slice(b"bplist00");
        let mut offsets = Vec::new();
        offsets.push(out.len());
        out.push(0xd2); // dict, two entries
        out.extend_from_slice(&[1, 2, 3, 4]); // key refs then value refs
        offsets.push(out.len());
        out.extend_from_slice(&[0x51, b'a']); // "a"
        offsets.push(out.len());
        out.extend_from_slice(&[0x51, b'b']); // "b"
        offsets.push(out.len());
        out.extend_from_slice(&[0x10, 1]); // 1
        offsets.push(out.len());
        out.extend_from_slice(&[0x53, b't', b'w', b'o']); // "two"

        let table = out.len();
        for offset in &offsets {
            out.push(*offset as u8);
        }
        out.extend_from_slice(&[0; 6]);
        out.push(1); // offset width
        out.push(1); // reference width
        out.extend_from_slice(&(offsets.len() as u64).to_be_bytes());
        out.extend_from_slice(&0u64.to_be_bytes()); // root
        out.extend_from_slice(&(table as u64).to_be_bytes());

        let value = parse(&out).expect("a binary plist");
        assert_eq!(value.get("a"), Some(&Value::Integer(1)));
        assert_eq!(value.get("b").and_then(Value::as_str), Some("two"));
    }

    #[test]
    fn written_xml_reads_back_the_same_value() {
        let mut inner = BTreeMap::new();
        inner.insert("t".to_owned(), Value::Bool(true));
        let mut root = BTreeMap::new();
        root.insert("s".to_owned(), Value::String("a<b>&\"c\"".into()));
        root.insert("i".to_owned(), Value::Integer(-7));
        root.insert("r".to_owned(), Value::Real(1.5));
        root.insert("d".to_owned(), Value::Data(vec![0, 1, 2, 3, 254]));
        root.insert("when".to_owned(), Value::Date(86400.0 * 365.0 + 61.0));
        root.insert("a".to_owned(), Value::Array(vec![Value::Integer(1)]));
        root.insert("empty".to_owned(), Value::Dictionary(BTreeMap::new()));
        root.insert("none".to_owned(), Value::Array(Vec::new()));
        root.insert("nested".to_owned(), Value::Dictionary(inner));
        let value = Value::Dictionary(root);
        assert_eq!(parse(&write_xml(&value)).expect("readable"), value);
    }

    #[test]
    fn a_date_is_written_in_iso_8601() {
        assert_eq!(format_date(0.0), "2001-01-01T00:00:00Z");
        assert_eq!(xml::parse_date("2001-01-02T00:00:01Z"), Some(86401.0));
    }

    #[test]
    fn a_written_binary_plist_reads_back_the_same() {
        let mut inner = BTreeMap::new();
        inner.insert(
            "caf\u{e9}".to_string(),
            Value::String("\u{1F600} text".into()),
        );
        inner.insert("n".to_string(), Value::Integer(-7));
        let mut top = BTreeMap::new();
        top.insert(
            "array".to_string(),
            Value::Array(vec![
                Value::Bool(true),
                Value::Bool(false),
                Value::Integer(300),
                Value::Integer(1 << 40),
                Value::Real(2.5),
                Value::Date(1.0e8),
                Value::Data(vec![0; 20]),
                Value::String("x".repeat(40)),
            ]),
        );
        top.insert("inner".to_string(), Value::Dictionary(inner));
        let value = Value::Dictionary(top);
        assert_eq!(parse(&write_binary(&value)), Ok(value));
    }

    #[test]
    fn a_truncated_plist_is_an_error_rather_than_a_panic() {
        assert!(parse(b"bplist00").is_err());
        assert!(parse(b"bplist00\x00\x00\x00\x00\x00\x00\x00\x00").is_err());
        // A trailer promising an offset table past the end.
        let mut bytes = vec![0u8; 64];
        bytes[..8].copy_from_slice(b"bplist00");
        bytes[64 - 32 + 6] = 8;
        bytes[64 - 32 + 7] = 8;
        bytes[64 - 32 + 15] = 0xff;
        assert!(parse(&bytes).is_err());
    }
}
