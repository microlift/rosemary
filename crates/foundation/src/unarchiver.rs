use std::collections::BTreeMap;

use crate::plist::{self, Value};

/// An index into the archive's object table.
///
/// Zero is `$null`, which is how the format spells `nil`, so [`Uid::is_null`]
/// is the check a caller makes before following one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Uid(pub usize);

impl Uid {
    pub fn is_null(self) -> bool {
        self.0 == 0
    }
}

#[derive(Debug)]
pub struct Archive {
    objects: Vec<Value>,
    nib_objects: Vec<NibObject>,
    top: BTreeMap<String, Uid>,
}

#[derive(Debug)]
struct NibObject {
    class_name: String,
    classes: Vec<Value>,
    fields: Vec<(String, Value)>,
}

/// What went wrong, as a sentence. [`plist::Error`] widened, not wrapped: a
/// caller reporting one reports the other the same way.
pub type Error = plist::Error;

fn bad(what: impl std::fmt::Display) -> Error {
    plist::Error(what.to_string())
}

impl Archive {
    pub fn parse(bytes: &[u8]) -> Result<Archive, Error> {
        if bytes.starts_with(b"NIBArchive") {
            return nib::parse(bytes);
        }
        Archive::from_plist(plist::parse(bytes)?)
    }

    pub fn from_plist(root: Value) -> Result<Archive, Error> {
        let archiver = root.get("$archiver").and_then(Value::as_str);
        if archiver != Some("NSKeyedArchiver") {
            return Err(bad(format!(
                "not a keyed archive: $archiver is {}",
                archiver.unwrap_or("absent")
            )));
        }
        let objects = root
            .get("$objects")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("a keyed archive with no $objects table"))?
            .to_vec();
        let mut top = BTreeMap::new();
        for (key, value) in root
            .get("$top")
            .and_then(Value::as_dictionary)
            .ok_or_else(|| bad("a keyed archive with no $top"))?
        {
            if let Some(uid) = as_uid(value) {
                top.insert(key.clone(), uid);
            }
        }
        Ok(Archive {
            objects,
            nib_objects: Vec::new(),
            top,
        })
    }

    /// The entry `$top` files under `key`, which for a nib is `"root"`.
    pub fn top(&self, key: &str) -> Option<Uid> {
        self.top.get(key).copied()
    }

    pub fn root(&self) -> Option<Uid> {
        self.top("root")
    }

    /// The raw entry at `uid`, or `None` for one past the end of the table.
    pub fn value(&self, uid: Uid) -> Option<&Value> {
        self.objects.get(uid.0)
    }

    /// The class chain of the object at `uid`, most-derived first.
    ///
    /// Empty for an entry that is not an archived object - a string, a number,
    /// or `$null` - which is how a caller tells the two apart.
    pub fn classes(&self, uid: Uid) -> &[Value] {
        if !self.nib_objects.is_empty() {
            return self
                .nib_objects
                .get(uid.0.checked_sub(1).unwrap_or(usize::MAX))
                .map(|o| o.classes.as_slice())
                .unwrap_or(&[]);
        }
        let Some(class) = self.field_uid(uid, "$class") else {
            return &[];
        };
        self.value(class)
            .and_then(|c| c.get("$classes"))
            .and_then(Value::as_array)
            .unwrap_or(&[])
    }

    /// The name the archive recorded for the object's own class.
    pub fn class_name(&self, uid: Uid) -> Option<&str> {
        if !self.nib_objects.is_empty() {
            return self
                .nib_objects
                .get(uid.0.checked_sub(1)?)
                .map(|o| o.class_name.as_str());
        }
        let class = self.field_uid(uid, "$class")?;
        self.value(class)?.get("$classname")?.as_str()
    }

    /// Whether the object at `uid` is a `name`, or descends from one.
    ///
    /// The archive's own chain rather than the runtime's: an archived
    /// `NSMutableArray` records `NSArray` above it whether or not this process
    /// has ever registered either class.
    pub fn is_kind_of(&self, uid: Uid, name: &str) -> bool {
        self.classes(uid)
            .iter()
            .filter_map(Value::as_str)
            .any(|c| c == name)
    }

    /// A keyed field of the object at `uid`, undereferenced.
    pub fn field(&self, uid: Uid, key: &str) -> Option<&Value> {
        if !self.nib_objects.is_empty() {
            return self
                .nib_objects
                .get(uid.0.checked_sub(1)?)?
                .fields
                .iter()
                .find_map(|(name, value)| (name == key).then_some(value));
        }
        self.value(uid)?.get(key)
    }

    /// A keyed field that is a reference.
    pub fn field_uid(&self, uid: Uid, key: &str) -> Option<Uid> {
        as_uid(self.field(uid, key)?)
    }

    /// A keyed field that is a reference, with `$null` read as absent.
    ///
    /// The distinction matters exactly once per nib: an `NSCustomObject`'s
    /// `NSObject` field is the instance it was already given, and `$null` there
    /// means "instantiate one", not "the field is missing".
    pub fn field_object(&self, uid: Uid, key: &str) -> Option<Uid> {
        self.field_uid(uid, key).filter(|u| !u.is_null())
    }

    /// The text of a string, whether it was archived as an object or written
    /// into the table directly.
    ///
    /// Both occur in one nib: a short key is a plain entry, and an `NSString`
    /// the graph refers to more than once is an object with `NS.string`.
    pub fn string(&self, uid: Uid) -> Option<&str> {
        if !self.nib_objects.is_empty() {
            let value = self
                .field(uid, "NS.bytes")
                .or_else(|| self.field(uid, "NS.string"))?;
            return match value {
                Value::String(s) => Some(s),
                Value::Data(bytes) => std::str::from_utf8(bytes)
                    .ok()
                    .map(|s| s.trim_end_matches('\0')),
                _ => None,
            };
        }
        match self.value(uid)? {
            Value::String(s) => Some(s),
            _ => self
                .field(uid, "NS.string")
                .or_else(|| self.field(uid, "NS.bytes"))
                .and_then(Value::as_str),
        }
    }

    /// The elements of an archived `NSArray` or `NSSet`.
    pub fn array(&self, uid: Uid) -> Vec<Uid> {
        if !self.nib_objects.is_empty() {
            return self
                .nib_objects
                .get(uid.0.checked_sub(1).unwrap_or(usize::MAX))
                .into_iter()
                .flat_map(|o| &o.fields)
                .filter(|(key, _)| key == "UINibEncoderEmptyKey")
                .filter_map(|(_, value)| as_uid(value))
                .collect();
        }
        let entries = match self.value(uid) {
            Some(Value::Array(a)) => a.as_slice(),
            _ => self
                .field(uid, "NS.objects")
                .and_then(Value::as_array)
                .unwrap_or(&[]),
        };
        entries.iter().filter_map(as_uid).collect()
    }

    /// The `(key, value)` pairs of an archived `NSDictionary`, in stored order.
    pub fn dictionary(&self, uid: Uid) -> Vec<(Uid, Uid)> {
        if !self.nib_objects.is_empty() {
            let entries: Vec<Uid> = self.array(uid);
            return entries
                .chunks_exact(2)
                .map(|pair| (pair[0], pair[1]))
                .collect();
        }
        let keys = self
            .field(uid, "NS.keys")
            .and_then(Value::as_array)
            .unwrap_or(&[]);
        let values = self
            .field(uid, "NS.objects")
            .and_then(Value::as_array)
            .unwrap_or(&[]);
        keys.iter()
            .zip(values)
            .filter_map(|(k, v)| Some((as_uid(k)?, as_uid(v)?)))
            .collect()
    }

    pub fn integer(&self, uid: Uid) -> Option<i64> {
        if !self.nib_objects.is_empty() {
            return ["NS.intval", "NS.boolval"].into_iter().find_map(|key| {
                match self.field(uid, key)? {
                    Value::Integer(v) => Some(*v),
                    Value::Bool(v) => Some(*v as i64),
                    _ => None,
                }
            });
        }
        match self.value(uid)? {
            Value::Integer(v) => Some(*v),
            Value::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }

    /// How many entries the table has, for a caller reporting on a whole file.
    pub fn len(&self) -> usize {
        if self.nib_objects.is_empty() {
            self.objects.len()
        } else {
            self.nib_objects.len() + 1
        }
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty() && self.nib_objects.is_empty()
    }
}

/// Apple's compact nib spelling. It contains the same keyed object graph as an
/// `NSKeyedArchiver`, but keeps objects, keys, values, and class names in four
/// separate tables. Object references are zero based; `Archive` reserves UID
/// zero for nil, so references gain one while they are read.
mod nib {
    use super::*;

    #[derive(Debug)]
    struct Object {
        class: usize,
        first_value: usize,
        value_count: usize,
    }

    #[derive(Debug)]
    struct Class {
        name: String,
        fallbacks: Vec<usize>,
    }

    pub(super) fn parse(bytes: &[u8]) -> Result<Archive, Error> {
        if bytes.len() < 50 {
            return Err(bad("a NIBArchive is shorter than its header"));
        }
        let format = le32(bytes, 10)?;
        if format != 1 {
            return Err(bad(format!("unsupported NIBArchive format {format}")));
        }
        let object_count = le32(bytes, 18)? as usize;
        let object_offset = le32(bytes, 22)? as usize;
        let key_count = le32(bytes, 26)? as usize;
        let key_offset = le32(bytes, 30)? as usize;
        let value_count = le32(bytes, 34)? as usize;
        let value_offset = le32(bytes, 38)? as usize;
        let class_count = le32(bytes, 42)? as usize;
        let class_offset = le32(bytes, 46)? as usize;
        for (name, offset) in [
            ("object", object_offset),
            ("key", key_offset),
            ("value", value_offset),
            ("class", class_offset),
        ] {
            if offset > bytes.len() {
                return Err(bad(format!("the NIBArchive {name} table is past EOF")));
            }
        }

        let mut at = object_offset;
        let mut raw_objects = Vec::with_capacity(reserve(object_count, bytes));
        for _ in 0..object_count {
            raw_objects.push(Object {
                class: varint(bytes, &mut at)?,
                first_value: varint(bytes, &mut at)?,
                value_count: varint(bytes, &mut at)?,
            });
        }
        if at != key_offset {
            return Err(bad(format!(
                "the NIBArchive object table ends at {at}, not {key_offset}"
            )));
        }

        let mut keys = Vec::with_capacity(reserve(key_count, bytes));
        at = key_offset;
        for _ in 0..key_count {
            let len = varint(bytes, &mut at)?;
            keys.push(text(slice(bytes, at, len)?)?);
            at += len;
        }
        if at != value_offset {
            return Err(bad(format!(
                "the NIBArchive key table ends at {at}, not {value_offset}"
            )));
        }

        let mut values = Vec::with_capacity(reserve(value_count, bytes));
        at = value_offset;
        for _ in 0..value_count {
            let key = varint(bytes, &mut at)?;
            let kind = *bytes
                .get(at)
                .ok_or_else(|| bad("a NIBArchive value has no type"))?;
            at += 1;
            let value = match kind {
                0 => Value::Integer(read::<1>(bytes, &mut at)?[0] as i8 as i64),
                1 => Value::Integer(i16::from_le_bytes(read(bytes, &mut at)?) as i64),
                2 => Value::Integer(i32::from_le_bytes(read(bytes, &mut at)?) as i64),
                3 => Value::Integer(i64::from_le_bytes(read(bytes, &mut at)?)),
                4 => Value::Bool(false),
                5 => Value::Bool(true),
                6 => Value::Real(f32::from_le_bytes(read(bytes, &mut at)?) as f64),
                7 => Value::Real(f64::from_le_bytes(read(bytes, &mut at)?)),
                8 => {
                    let len = varint(bytes, &mut at)?;
                    let data = slice(bytes, at, len)?.to_vec();
                    at += len;
                    Value::Data(data)
                }
                9 => Value::Uid(0),
                10 => {
                    let object = u32::from_le_bytes(read(bytes, &mut at)?) as usize;
                    if object >= object_count {
                        return Err(bad(format!(
                            "NIBArchive value references object {object} of {object_count}"
                        )));
                    }
                    Value::Uid((object + 1) as u64)
                }
                other => return Err(bad(format!("unknown NIBArchive value type {other}"))),
            };
            let key = keys
                .get(key)
                .ok_or_else(|| bad(format!("NIBArchive value names absent key {key}")))?
                .clone();
            values.push((key, value));
        }
        if at != class_offset {
            return Err(bad(format!(
                "the NIBArchive value table ends at {at}, not {class_offset}"
            )));
        }

        let mut classes = Vec::with_capacity(reserve(class_count, bytes));
        at = class_offset;
        for _ in 0..class_count {
            let len = varint(bytes, &mut at)?;
            let fallback_count = varint(bytes, &mut at)?;
            let mut fallbacks = Vec::with_capacity(reserve(fallback_count, bytes));
            for _ in 0..fallback_count {
                fallbacks.push(u32::from_le_bytes(read(bytes, &mut at)?) as usize);
            }
            let raw = slice(bytes, at, len)?;
            at += len;
            let name = text(raw.strip_suffix(&[0]).unwrap_or(raw))?;
            classes.push(Class { name, fallbacks });
        }

        let mut nib_objects = Vec::with_capacity(raw_objects.len());
        for object in raw_objects {
            let class = classes.get(object.class).ok_or_else(|| {
                bad(format!(
                    "NIBArchive object names absent class {}",
                    object.class
                ))
            })?;
            let end = object
                .first_value
                .checked_add(object.value_count)
                .ok_or_else(|| bad("a NIBArchive object's value range overflows"))?;
            let fields = values
                .get(object.first_value..end)
                .ok_or_else(|| bad("a NIBArchive object's values are out of bounds"))?
                .to_vec();
            let mut chain = vec![Value::String(class.name.clone())];
            for fallback in &class.fallbacks {
                let name = classes.get(*fallback).ok_or_else(|| {
                    bad(format!("NIBArchive class names absent fallback {fallback}"))
                })?;
                chain.push(Value::String(name.name.clone()));
            }
            nib_objects.push(NibObject {
                class_name: class.name.clone(),
                classes: chain,
                fields,
            });
        }

        // The first object is the archive envelope. Its reference-valued keys
        // are the compact format's equivalent of NSKeyedArchiver's `$top`.
        let mut top = BTreeMap::new();
        if let Some(envelope) = nib_objects.first() {
            for (key, value) in &envelope.fields {
                if let Some(uid) = as_uid(value) {
                    top.insert(key.clone(), uid);
                }
            }
        }
        Ok(Archive {
            objects: Vec::new(),
            nib_objects,
            top,
        })
    }

    /// Room for `count` table entries, which each take at least a byte: a
    /// count the file cannot hold is refused when it runs out, not by an
    /// allocation that aborts first.
    fn reserve(count: usize, bytes: &[u8]) -> usize {
        count.min(bytes.len())
    }

    fn le32(bytes: &[u8], at: usize) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(
            slice(bytes, at, 4)?.try_into().unwrap_or_default(),
        ))
    }

    fn varint(bytes: &[u8], at: &mut usize) -> Result<usize, Error> {
        let mut value = 0usize;
        for shift in (0..usize::BITS as usize).step_by(7) {
            let byte = *bytes
                .get(*at)
                .ok_or_else(|| bad("a NIBArchive varint runs past EOF"))?;
            *at += 1;
            value |= ((byte & 0x7f) as usize) << shift;
            if byte & 0x80 != 0 {
                return Ok(value);
            }
        }
        Err(bad("a NIBArchive varint is too wide"))
    }

    fn read<const N: usize>(bytes: &[u8], at: &mut usize) -> Result<[u8; N], Error> {
        let value = slice(bytes, *at, N)?.try_into().unwrap_or([0; N]);
        *at += N;
        Ok(value)
    }

    fn slice(bytes: &[u8], at: usize, len: usize) -> Result<&[u8], Error> {
        bytes
            .get(at..at.saturating_add(len))
            .ok_or_else(|| bad(format!("NIBArchive data at {at} runs past EOF")))
    }

    fn text(bytes: &[u8]) -> Result<String, Error> {
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| bad("a NIBArchive name is not UTF-8"))
    }
}

/// A reference, in either of the two spellings a `CF$UID` has.
///
/// The binary format has a marker of its own; the XML one - what `plutil
/// -convert xml1` writes, and what a hand-edited archive is - has no such
/// marker and renders it as a one-key dictionary instead.
fn as_uid(value: &Value) -> Option<Uid> {
    match value {
        Value::Uid(v) => Some(Uid(*v as usize)),
        Value::Dictionary(d) => match d.get("CF$UID")? {
            Value::Integer(v) if *v >= 0 => Some(Uid(*v as usize)),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `plutil -convert xml1` writes for
    /// `@{@"name": @"outlet", @"objects": @[@1, @"three"]}` archived by Apple's
    /// `NSKeyedArchiver`, trimmed to the entries the assertions read.
    const ARCHIVE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>$archiver</key>
	<string>NSKeyedArchiver</string>
	<key>$objects</key>
	<array>
		<string>$null</string>
		<dict>
			<key>$class</key>
			<dict><key>CF$UID</key><integer>8</integer></dict>
			<key>NS.keys</key>
			<array>
				<dict><key>CF$UID</key><integer>2</integer></dict>
				<dict><key>CF$UID</key><integer>3</integer></dict>
			</array>
			<key>NS.objects</key>
			<array>
				<dict><key>CF$UID</key><integer>4</integer></dict>
				<dict><key>CF$UID</key><integer>5</integer></dict>
			</array>
		</dict>
		<string>name</string>
		<string>objects</string>
		<string>outlet</string>
		<dict>
			<key>$class</key>
			<dict><key>CF$UID</key><integer>7</integer></dict>
			<key>NS.objects</key>
			<array>
				<dict><key>CF$UID</key><integer>6</integer></dict>
			</array>
		</dict>
		<string>three</string>
		<dict>
			<key>$classes</key>
			<array><string>NSArray</string><string>NSObject</string></array>
			<key>$classname</key>
			<string>NSArray</string>
		</dict>
		<dict>
			<key>$classes</key>
			<array><string>NSDictionary</string><string>NSObject</string></array>
			<key>$classname</key>
			<string>NSDictionary</string>
		</dict>
	</array>
	<key>$top</key>
	<dict>
		<key>root</key>
		<dict><key>CF$UID</key><integer>1</integer></dict>
	</dict>
	<key>$version</key>
	<integer>100000</integer>
</dict>
</plist>"#;

    fn archive() -> Archive {
        Archive::parse(ARCHIVE.as_bytes()).expect("a keyed archive")
    }

    #[test]
    fn a_reference_is_followed_to_its_entry() {
        let a = archive();
        let root = a.root().expect("$top has a root");
        assert_eq!(a.class_name(root), Some("NSDictionary"));
        let pairs = a.dictionary(root);
        let keys: Vec<&str> = pairs.iter().filter_map(|(k, _)| a.string(*k)).collect();
        assert_eq!(keys, ["name", "objects"]);
    }

    #[test]
    fn the_class_chain_is_the_archives_own() {
        let a = archive();
        let root = a.root().expect("a root");
        assert!(a.is_kind_of(root, "NSObject"));
        assert!(!a.is_kind_of(root, "NSArray"));
    }

    #[test]
    fn a_nested_array_reads_as_references() {
        let a = archive();
        let root = a.root().expect("a root");
        let (_, objects) = a
            .dictionary(root)
            .into_iter()
            .find(|(k, _)| a.string(*k) == Some("objects"))
            .expect("the objects key");
        let elements = a.array(objects);
        assert_eq!(elements.len(), 1);
        assert_eq!(a.string(elements[0]), Some("three"));
    }

    /// `$null` is a reference like any other, and the one a caller must not
    /// follow: entry zero is the string `$null`, not the object it stands for.
    #[test]
    fn null_is_a_reference_that_is_not_an_object() {
        let a = archive();
        assert!(Uid(0).is_null());
        assert_eq!(a.class_name(Uid(0)), None);
        assert!(a.classes(Uid(0)).is_empty());
    }

    #[test]
    fn a_plist_that_is_not_an_archive_says_so() {
        let err = Archive::parse(b"<plist version=\"1.0\"><dict></dict></plist>")
            .expect_err("not an archive");
        assert!(err.0.contains("$archiver"), "{}", err.0);
    }

    #[test]
    fn a_nib_archive_with_counts_larger_than_the_file_is_an_error() {
        let mut bytes = b"NIBArchive".to_vec();
        bytes.resize(50, 0);
        bytes[10..14].copy_from_slice(&1u32.to_le_bytes());
        // Every table claims `u32::MAX` entries and starts at the end.
        for field in [18, 26, 34, 42] {
            bytes[field..field + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            bytes[field + 4..field + 8].copy_from_slice(&50u32.to_le_bytes());
        }
        assert!(Archive::parse(&bytes).is_err());
    }
}
