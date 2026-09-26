use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections::Number;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::class_methods;

/// `NSJSONWritingOptions`.
mod writing {
    pub const PRETTY_PRINTED: usize = 1 << 0;
    pub const SORTED_KEYS: usize = 1 << 1;
    pub const FRAGMENTS_ALLOWED: usize = 1 << 2;
    pub const WITHOUT_ESCAPING_SLASHES: usize = 1 << 3;
}

/// `NSJSONReadingOptions`.
mod reading {
    pub const MUTABLE_CONTAINERS: usize = 1 << 0;
    pub const FRAGMENTS_ALLOWED: usize = 1 << 2;
}

/// `NSCocoaErrorDomain`'s `NSPropertyListReadCorruptError`, which is the code
/// Apple's own reader reports for every malformed document.
const CORRUPT: i64 = 3840;

/// How deep a document may nest.
///
/// Both walks are recursive, and the input to the reader is whatever arrived on
/// a socket. A limit is what keeps a hostile document from ending the process in
/// a stack overflow, where a refusal is an ordinary parse failure.
const MAX_DEPTH: usize = 512;

/// Append `object` as JSON, or name what could not be represented.
fn write(object: CFTypeRef, out: &mut Vec<u8>, options: usize, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("the graph nests deeper than {MAX_DEPTH} levels"));
    }
    if object.is_null() {
        return Err("the graph holds nil, which is not an object".into());
    }
    if object == crate::value::null_object() as CFTypeRef {
        out.extend_from_slice(b"null");
        return Ok(());
    }
    // SAFETY: every accessor below is type-checked and answers `None` for an
    // object of another class.
    unsafe {
        if corefoundation::base::is_kind_of(object, corefoundation::collections::boolean_class()) {
            let value = corefoundation::collections::number(object);
            let truth = !matches!(value, Some(Number::Integer(0)));
            out.extend_from_slice(if truth { b"true" } else { b"false" });
            return Ok(());
        }
        if let Some(string) = corefoundation::string::as_string(object) {
            write_string(&string.utf8(), out, options);
            return Ok(());
        }
        if let Some(number) = corefoundation::collections::number(object) {
            write_number(&number, out)?;
            return Ok(());
        }
        if let Some(array) = corefoundation::collections::array(object) {
            return write_array(&array.values, out, options, depth);
        }
        if let Some(dictionary) = corefoundation::collections::dictionary(object) {
            return write_dictionary(&dictionary.entries, out, options, depth);
        }
    }
    // SAFETY: a live object.
    let class = unsafe { corefoundation::base::class_of(object) }
        .map(|c| c.name_str().to_string())
        .unwrap_or_else(|| "an object with no class".into());
    Err(format!(
        "the graph holds a {class}, which JSON cannot carry"
    ))
}

fn write_array(
    values: &[CFTypeRef],
    out: &mut Vec<u8>,
    options: usize,
    depth: usize,
) -> Result<(), String> {
    out.push(b'[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        newline(out, options, depth + 1);
        write(*value, out, options, depth + 1)?;
    }
    if !values.is_empty() {
        newline(out, options, depth);
    }
    out.push(b']');
    Ok(())
}

fn write_dictionary(
    entries: &[(CFTypeRef, CFTypeRef)],
    out: &mut Vec<u8>,
    options: usize,
    depth: usize,
) -> Result<(), String> {
    // The key has to be read as a string *by type*, not assumed to be one: a
    // number key is legal in an `NSDictionary` and has no JSON spelling, and
    // reading its bytes as a string would emit whatever followed it in memory.
    let mut pairs = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        // SAFETY: a live object; the accessor is type-checked.
        let Some(text) = (unsafe { corefoundation::string::as_string(*key) }) else {
            return Err("the graph holds a dictionary whose key is not a string".into());
        };
        pairs.push((text.utf8(), *value));
    }
    if options & writing::SORTED_KEYS != 0 {
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
    }

    out.push(b'{');
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        newline(out, options, depth + 1);
        write_string(key, out, options);
        out.push(b':');
        if options & writing::PRETTY_PRINTED != 0 {
            out.push(b' ');
        }
        write(*value, out, options, depth + 1)?;
    }
    if !pairs.is_empty() {
        newline(out, options, depth);
    }
    out.push(b'}');
    Ok(())
}

fn newline(out: &mut Vec<u8>, options: usize, depth: usize) {
    if options & writing::PRETTY_PRINTED == 0 {
        return;
    }
    out.push(b'\n');
    out.extend(std::iter::repeat_n(b' ', depth * 2));
}

fn write_string(bytes: &[u8], out: &mut Vec<u8>, options: usize) {
    out.push(b'"');
    for &byte in bytes {
        match byte {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            // Apple escapes the solidus unless asked not to. It is optional in
            // the grammar either way, so both spellings parse everywhere.
            b'/' if options & writing::WITHOUT_ESCAPING_SLASHES == 0 => {
                out.extend_from_slice(b"\\/")
            }
            0x08 => out.extend_from_slice(b"\\b"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x00..=0x1f => out.extend_from_slice(format!("\\u{byte:04x}").as_bytes()),
            _ => out.push(byte),
        }
    }
    out.push(b'"');
}

fn write_number(number: &Number, out: &mut Vec<u8>) -> Result<(), String> {
    match number {
        Number::Integer(v) => out.extend_from_slice(v.to_string().as_bytes()),
        // JSON has no spelling for either, and Apple raises rather than writing
        // one. Refused here for the same reason: `inf` is not a document any
        // reader accepts, so writing it moves the failure to the far side.
        Number::Float(v) if !v.is_finite() => {
            return Err(format!("the graph holds {v}, which JSON cannot carry"));
        }
        Number::Float(v) => out.extend_from_slice(format!("{v:?}").as_bytes()),
    }
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    mutable: bool,
}

impl Reader<'_> {
    fn skip_space(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        match self.peek() {
            Some(b) if b == byte => {
                self.at += 1;
                Ok(())
            }
            _ => Err(format!("expected '{}' at byte {}", byte as char, self.at)),
        }
    }

    fn literal(&mut self, word: &[u8]) -> Result<(), String> {
        if self.bytes[self.at..].starts_with(word) {
            self.at += word.len();
            return Ok(());
        }
        Err(format!("unrecognised value at byte {}", self.at))
    }

    /// A value, as a *retained* object the caller must release or hand to a
    /// collection.
    fn value(&mut self, depth: usize) -> Result<CFTypeRef, String> {
        if depth > MAX_DEPTH {
            return Err(format!("nests deeper than {MAX_DEPTH} levels"));
        }
        self.skip_space();
        match self.peek() {
            Some(b'{') => self.dictionary(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => {
                let text = self.string()?;
                Ok(corefoundation::string::from_bytes(&text, false))
            }
            Some(b't') => {
                self.literal(b"true")?;
                Ok(
                    objc::arc::retain(corefoundation::collections::boolean(true) as Id)
                        as CFTypeRef,
                )
            }
            Some(b'f') => {
                self.literal(b"false")?;
                Ok(
                    objc::arc::retain(corefoundation::collections::boolean(false) as Id)
                        as CFTypeRef,
                )
            }
            Some(b'n') => {
                self.literal(b"null")?;
                Ok(objc::arc::retain(crate::value::null_object()) as CFTypeRef)
            }
            Some(_) => self.number(),
            None => Err("the document ends where a value was expected".into()),
        }
    }

    fn array(&mut self, depth: usize) -> Result<CFTypeRef, String> {
        self.expect(b'[')?;
        let mut values: Vec<CFTypeRef> = Vec::new();
        let result = self.elements(&mut values, depth);
        let array = result.map(|()| {
            corefoundation::collections::new_array(
                &values,
                Policy::from_type_callbacks(),
                self.mutable,
            )
        });
        release_each(&values);
        array
    }

    fn elements(&mut self, values: &mut Vec<CFTypeRef>, depth: usize) -> Result<(), String> {
        self.skip_space();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(());
        }
        loop {
            values.push(self.value(depth + 1)?);
            self.skip_space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(());
                }
                _ => return Err(format!("expected ',' or ']' at byte {}", self.at)),
            }
        }
    }

    fn dictionary(&mut self, depth: usize) -> Result<CFTypeRef, String> {
        self.expect(b'{')?;
        let mut pairs: Vec<(CFTypeRef, CFTypeRef)> = Vec::new();
        let result = self.members(&mut pairs, depth);
        let dictionary = result.map(|()| {
            corefoundation::collections::new_dictionary(
                &pairs,
                Policy::from_key_callbacks(),
                Policy::from_type_callbacks(),
                self.mutable,
            )
        });
        for (key, value) in &pairs {
            release_each(&[*key, *value]);
        }
        dictionary
    }

    fn members(
        &mut self,
        pairs: &mut Vec<(CFTypeRef, CFTypeRef)>,
        depth: usize,
    ) -> Result<(), String> {
        self.skip_space();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(());
        }
        loop {
            self.skip_space();
            let key = self.string()?;
            self.skip_space();
            self.expect(b':')?;
            let value = self.value(depth + 1)?;
            pairs.push((corefoundation::string::from_bytes(&key, false), value));
            self.skip_space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(());
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.at)),
            }
        }
    }

    fn string(&mut self) -> Result<Vec<u8>, String> {
        self.expect(b'"')?;
        let mut out = Vec::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err("the document ends inside a string".into());
            };
            self.at += 1;
            match byte {
                b'"' => return Ok(out),
                b'\\' => self.escape(&mut out)?,
                _ => out.push(byte),
            }
        }
    }

    fn escape(&mut self, out: &mut Vec<u8>) -> Result<(), String> {
        let Some(byte) = self.peek() else {
            return Err("the document ends inside an escape".into());
        };
        self.at += 1;
        match byte {
            b'"' | b'\\' | b'/' => out.push(byte),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'u' => {
                let first = self.hex4()?;
                // A character above the BMP arrives as a surrogate pair, and
                // each half on its own is not a character at all - decoding
                // them separately gives two replacement characters where the
                // sender wrote one emoji.
                let scalar = match first {
                    0xd800..=0xdbff => {
                        if !self.bytes[self.at..].starts_with(b"\\u") {
                            return Err(format!(
                                "a leading surrogate with no trailing half at byte {}",
                                self.at
                            ));
                        }
                        self.at += 2;
                        let second = self.hex4()?;
                        if !(0xdc00..=0xdfff).contains(&second) {
                            return Err(format!(
                                "a leading surrogate followed by {second:#06x} at byte {}",
                                self.at
                            ));
                        }
                        0x10000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00)
                    }
                    0xdc00..=0xdfff => {
                        return Err(format!("a trailing surrogate alone at byte {}", self.at));
                    }
                    _ => u32::from(first),
                };
                let character = char::from_u32(scalar)
                    .ok_or_else(|| format!("{scalar:#x} is not a character"))?;
                let mut buffer = [0u8; 4];
                out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
            }
            _ => return Err(format!("unrecognised escape '\\{}'", byte as char)),
        }
        Ok(())
    }

    fn hex4(&mut self) -> Result<u16, String> {
        let end = self.at + 4;
        if end > self.bytes.len() {
            return Err("the document ends inside a \\u escape".into());
        }
        let digits = &self.bytes[self.at..end];
        self.at = end;
        let text =
            std::str::from_utf8(digits).map_err(|_| "a \\u escape is not hex".to_string())?;
        u16::from_str_radix(text, 16).map_err(|_| format!("'{text}' is not four hex digits"))
    }

    fn number(&mut self) -> Result<CFTypeRef, String> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        let mut real = false;
        while let Some(byte) = self.peek() {
            match byte {
                b'0'..=b'9' => self.at += 1,
                b'.' | b'e' | b'E' | b'+' | b'-' => {
                    real = true;
                    self.at += 1;
                }
                _ => break,
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.at])
            .map_err(|_| format!("a number that is not text at byte {start}"))?;
        // An integer is kept as one: a JSON id round-tripped through an f64
        // comes back in exponent form past 2^53, and the far side reads a
        // different id.
        if !real && let Ok(value) = text.parse::<i64>() {
            return Ok(corefoundation::collections::new_number(Number::Integer(
                value,
            )));
        }
        let value = text
            .parse::<f64>()
            .map_err(|_| format!("'{text}' is not a number, at byte {start}"))?;
        Ok(corefoundation::collections::new_number(Number::Float(
            value,
        )))
    }
}

fn release_each(objects: &[CFTypeRef]) {
    for object in objects {
        // SAFETY: each is an object this module made and owns a reference to.
        unsafe { corefoundation::api::CFRelease(*object) };
    }
}

/// Whether `object` is a container, which is what a non-fragment document is.
fn is_container(object: CFTypeRef) -> bool {
    // SAFETY: a live object; both accessors are type-checked.
    unsafe {
        corefoundation::collections::array(object).is_some()
            || corefoundation::collections::dictionary(object).is_some()
    }
}

/// Report `message` through the caller's `NSError **`, if it passed one.
///
/// Apple's contract, and the reason the out-parameter is written rather than
/// only returned: a caller that passed null asked not to be told, and writing
/// through it anyway is a store to whatever that address happens to be.
fn fail(error: *mut Id, message: &str) {
    if error.is_null() {
        return;
    }
    let object = crate::error::make("NSCocoaErrorDomain", CORRUPT, message);
    // SAFETY: the guest handed us the address of its own `NSError *`.
    unsafe { *error = object };
}

unsafe extern "C" fn data_with_json_object(
    _class: Id,
    _sel: Sel,
    object: Id,
    options: usize,
    error: *mut Id,
) -> Id {
    let root = object as CFTypeRef;
    if options & writing::FRAGMENTS_ALLOWED == 0 && !is_container(root) {
        fail(
            error,
            "the top level of a JSON document is an array or a dictionary unless \
             NSJSONWritingFragmentsAllowed is set",
        );
        return std::ptr::null_mut();
    }
    let mut out = Vec::new();
    if let Err(why) = write(root, &mut out, options, 0) {
        fail(error, &format!("this object cannot be serialised: {why}"));
        return std::ptr::null_mut();
    }
    let data = corefoundation::collections::new_data(out);
    // SAFETY: a fresh object this function owns a reference to.
    unsafe { corefoundation::api::CFAutorelease(data) as Id }
}

unsafe extern "C" fn json_object_with_data(
    _class: Id,
    _sel: Sel,
    data: Id,
    options: usize,
    error: *mut Id,
) -> Id {
    // Read as an `NSData` by type rather than because it arrived in that
    // position: a length read out of another class's field is the first thing
    // this would allocate.
    // SAFETY: a live object; the accessor is type-checked.
    let Some(bytes) = (unsafe { corefoundation::collections::data(data as CFTypeRef) }) else {
        fail(error, "the argument is not an NSData");
        return std::ptr::null_mut();
    };
    let mut reader = Reader {
        bytes,
        at: 0,
        mutable: options & reading::MUTABLE_CONTAINERS != 0,
    };
    let object = match reader.value(0) {
        Ok(object) => object,
        Err(why) => {
            fail(error, &format!("this is not valid JSON: {why}"));
            return std::ptr::null_mut();
        }
    };
    reader.skip_space();
    if reader.at != reader.bytes.len() {
        release_each(&[object]);
        fail(
            error,
            &format!("garbage after the document, at byte {}", reader.at),
        );
        return std::ptr::null_mut();
    }
    if options & reading::FRAGMENTS_ALLOWED == 0 && !is_container(object) {
        release_each(&[object]);
        fail(
            error,
            "the top level of a JSON document is an array or a dictionary unless \
             NSJSONReadingFragmentsAllowed is set",
        );
        return std::ptr::null_mut();
    }
    // SAFETY: a fresh object this function owns a reference to.
    unsafe { corefoundation::api::CFAutorelease(object) as Id }
}

/// `object` as JSON text, fragments included, or why it cannot be.
pub fn to_text(object: Id) -> Result<String, String> {
    let mut out = Vec::new();
    write(object as CFTypeRef, &mut out, 0, 0)?;
    String::from_utf8(out).map_err(|e| e.to_string())
}

/// The object a JSON text describes, fragments included, autoreleased.
pub fn from_text(text: &str) -> Result<Id, String> {
    let mut reader = Reader { bytes: text.as_bytes(), at: 0, mutable: false };
    let object = reader.value(0)?;
    reader.skip_space();
    if reader.at != reader.bytes.len() {
        release_each(&[object]);
        return Err(format!("garbage after the document, at byte {}", reader.at));
    }
    // SAFETY: a fresh object this function owns a reference to.
    Ok(unsafe { corefoundation::api::CFAutorelease(object) as Id })
}

unsafe extern "C" fn is_valid_json_object(_class: Id, _sel: Sel, object: Id) -> bool {
    let root = object as CFTypeRef;
    is_container(root) && write(root, &mut Vec::new(), 0, 0).is_ok()
}

/// `+dataWithJSONObject:options:error:` under its older spelling, which takes a
/// stream this runtime has no `NSOutputStream` for.
unsafe extern "C" fn write_to_stream(
    _class: Id,
    _sel: Sel,
    _object: Id,
    _stream: *mut c_void,
    _options: usize,
    error: *mut Id,
) -> i64 {
    fail(
        error,
        "+[NSJSONSerialization writeJSONObject:toStream:options:error:] needs an NSOutputStream, \
         which this runtime does not have - use +dataWithJSONObject:options:error:",
    );
    0
}

pub(crate) fn install() {
    let class = corefoundation::base::abstract_class(c"NSJSONSerialization", None);
    class_methods!(class, {
        "dataWithJSONObject:options:error:" => data_with_json_object, c"@40@0:8@16Q24^@32";
        "JSONObjectWithData:options:error:" => json_object_with_data, c"@40@0:8@16Q24^@32";
        "isValidJSONObject:" => is_valid_json_object, c"B24@0:8@16";
        "writeJSONObject:toStream:options:error:" => write_to_stream, c"q48@0:8@16^v24Q32^@40";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send3(class: &str, sel: &str, a: Id, b: usize) -> Id {
        let class = objc::class::lookup(&std::ffi::CString::new(class).unwrap()).expect("a class");
        let mut error: Id = std::ptr::null_mut();
        // SAFETY: both selectors are `id (Class, SEL, id, NSUInteger, id *)`.
        let value = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, usize, *mut Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                objc::class::ptr(class) as Id,
                objc::sel::get(sel),
                a,
                b,
                &mut error,
            )
        };
        assert_eq!(
            value.is_null(),
            !error.is_null(),
            "one of the two, never both"
        );
        value
    }

    fn encode(object: Id, options: usize) -> Vec<u8> {
        let data = send3(
            "NSJSONSerialization",
            "dataWithJSONObject:options:error:",
            object,
            options,
        );
        assert!(!data.is_null());
        // SAFETY: an `NSData` this call made.
        unsafe { corefoundation::collections::data(data as CFTypeRef) }
            .expect("an NSData")
            .clone()
    }

    fn decode(bytes: &[u8], options: usize) -> Id {
        let data = corefoundation::collections::new_data(bytes.to_vec());
        send3(
            "NSJSONSerialization",
            "JSONObjectWithData:options:error:",
            data as Id,
            options,
        )
    }

    /// The property the message bus depends on: whatever
    /// `-serializeMessageParams:` wrote, `-deserializeMessageParams:` reads back
    /// as the same keys and the same values.
    #[test]
    fn a_dictionary_of_parameters_round_trips() {
        crate::bootstrap();
        let source = br#"{"id":"Credentials.isAvailable","seq":42,"ok":true,"ratio":0.5,"tags":["a","b"],"absent":null}"#;
        let object = decode(source, 0);
        assert!(!object.is_null());
        let text = encode(object, writing::SORTED_KEYS);
        let again = decode(&text, 0);
        assert_eq!(encode(again, writing::SORTED_KEYS), text);

        // Read one value back the way the guest does, to prove the keys are
        // strings and not something that merely prints like one.
        // SAFETY: `-objectForKey:` is `id (id, SEL, id)`.
        let value = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                object,
                objc::sel::get("objectForKey:"),
                corefoundation::string::from_bytes(b"id", false) as Id,
            )
        };
        // SAFETY: a string.
        assert_eq!(
            unsafe { crate::string::bytes(value) },
            b"Credentials.isAvailable"
        );
    }

    /// A boolean must not come back as the number one: the far side of the bus
    /// is Lua, where `1` is truthy but is not `true`, and a field compared
    /// against `true` would fail.
    #[test]
    fn a_boolean_stays_a_boolean() {
        crate::bootstrap();
        let object = decode(br#"{"ok":true,"n":1}"#, 0);
        assert_eq!(
            encode(object, writing::SORTED_KEYS),
            br#"{"n":1,"ok":true}"#
        );
    }

    /// `false` is written as `false`: a boolean used to read as no number at
    /// all, which the writer took for true.
    #[test]
    fn false_stays_false() {
        crate::bootstrap();
        let object = decode(br#"{"no":false,"yes":true}"#, 0);
        assert_eq!(
            encode(object, writing::SORTED_KEYS),
            br#"{"no":false,"yes":true}"#
        );
    }

    /// `+numberWithBool:` is the boolean object itself, and it answers the
    /// `NSNumber` getters with its value.
    #[test]
    fn number_with_bool_is_the_boolean() {
        crate::bootstrap();
        let number = objc::class::ptr(objc::class::lookup(c"NSNumber").expect("NSNumber")) as Id;
        for value in [true, false] {
            // SAFETY: `+numberWithBool:` is `id (Class, SEL, BOOL)` and
            // `-boolValue`/`-intValue` are `(id, SEL)` getters.
            unsafe {
                let make: unsafe extern "C" fn(Id, Sel, bool) -> Id =
                    std::mem::transmute(objc::msg::entry());
                let object = make(number, objc::sel::get("numberWithBool:"), value);
                assert_eq!(
                    object as CFTypeRef,
                    corefoundation::collections::boolean(value)
                );
                let get_bool: unsafe extern "C" fn(Id, Sel) -> bool =
                    std::mem::transmute(objc::msg::entry());
                assert_eq!(get_bool(object, objc::sel::get("boolValue")), value);
                let get_int: unsafe extern "C" fn(Id, Sel) -> i32 =
                    std::mem::transmute(objc::msg::entry());
                assert_eq!(get_int(object, objc::sel::get("intValue")), value as i32);
            }
        }
    }

    /// An id past 2^53 is the case a JSON reader that keeps every number as a
    /// double gets wrong.
    #[test]
    fn a_large_integer_survives() {
        crate::bootstrap();
        let object = decode(br#"[9007199254740993]"#, 0);
        assert_eq!(encode(object, 0), b"[9007199254740993]");
    }

    /// Malformed input is an ordinary answer - nil and an error - rather than a
    /// refusal, because the bytes came off a socket.
    #[test]
    fn malformed_input_answers_an_error() {
        crate::bootstrap();
        for source in [&b"{"[..], b"{\"a\":}", b"[1,]", b"{} trailing", b"\"bare\""] {
            let data = corefoundation::collections::new_data(source.to_vec());
            let mut error: Id = std::ptr::null_mut();
            // SAFETY: `id (Class, SEL, id, NSUInteger, id *)`.
            let value = unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Id, usize, *mut Id) -> Id =
                    std::mem::transmute(objc::msg::entry());
                f(
                    objc::class::ptr(
                        objc::class::lookup(c"NSJSONSerialization").expect("the class"),
                    ) as Id,
                    objc::sel::get("JSONObjectWithData:options:error:"),
                    data as Id,
                    0,
                    &mut error,
                )
            };
            assert!(value.is_null(), "{:?} parsed", std::str::from_utf8(source));
            assert!(!error.is_null());
        }
    }

    /// A character above the BMP arrives as two escapes and is one character.
    #[test]
    fn a_surrogate_pair_is_one_character() {
        crate::bootstrap();
        let object = decode("[\"\u{1f600}\"]".as_bytes(), 0);
        assert_eq!(encode(object, 0), "[\"\u{1f600}\"]".as_bytes());
    }
}
