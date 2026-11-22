//! Code requirements in their compiled form, written back out as the text
//! `codesign -d -r-` and `SecRequirementCopyString` print.
//!
//! The grammar and spelling follow Apple's `reqdumper.cpp`. An operation this
//! does not know is refused rather than guessed at, so a requirement either
//! comes back exactly as macOS spells it or not at all.

/// `kSecCodeMagicRequirement`: one compiled requirement.
const MAGIC_REQUIREMENT: u32 = 0xfade_0c00;
/// `kSecCodeMagicRequirementSet`: the requirements a signature carries.
const MAGIC_REQUIREMENT_SET: u32 = 0xfade_0c01;
/// `Requirement::exprForm`, the only body kind there is.
const EXPR_FORM: u32 = 1;
/// `kSecDesignatedRequirementType`.
pub(crate) const DESIGNATED: u32 = 3;
/// The flag bits of an `ExprOp`, which do not change how it prints.
const OP_FLAG_MASK: u32 = 0xff00_0000;

/// `ExprOp`.
mod op {
    pub const FALSE: u32 = 0;
    pub const TRUE: u32 = 1;
    pub const IDENT: u32 = 2;
    pub const APPLE_ANCHOR: u32 = 3;
    pub const ANCHOR_HASH: u32 = 4;
    pub const INFO_KEY_VALUE: u32 = 5;
    pub const AND: u32 = 6;
    pub const OR: u32 = 7;
    pub const CD_HASH: u32 = 8;
    pub const NOT: u32 = 9;
    pub const INFO_KEY_FIELD: u32 = 10;
    pub const CERT_FIELD: u32 = 11;
    pub const TRUSTED_CERT: u32 = 12;
    pub const TRUSTED_CERTS: u32 = 13;
    pub const CERT_GENERIC: u32 = 14;
    pub const APPLE_GENERIC_ANCHOR: u32 = 15;
    pub const ENTITLEMENT_FIELD: u32 = 16;
    pub const CERT_POLICY: u32 = 17;
    pub const NAMED_ANCHOR: u32 = 18;
    pub const NAMED_CODE: u32 = 19;
    pub const PLATFORM: u32 = 20;
    pub const NOTARIZED: u32 = 21;
    pub const LEGACY_DEV_ID: u32 = 23;
}

/// `MatchOperation`. The timestamp comparisons are left out, and refused.
mod matching {
    pub const EXISTS: u32 = 0;
    pub const EQUAL: u32 = 1;
    pub const CONTAINS: u32 = 2;
    pub const BEGINS_WITH: u32 = 3;
    pub const ENDS_WITH: u32 = 4;
    pub const LESS_THAN: u32 = 5;
    pub const GREATER_THAN: u32 = 6;
    pub const LESS_EQUAL: u32 = 7;
    pub const GREATER_EQUAL: u32 = 8;
    pub const ABSENT: u32 = 14;
}

/// `certSlot` values with names of their own.
const LEAF_CERT: i32 = 0;
const ANCHOR_CERT: i32 = -1;

/// Words the requirement language reserves, which an unquoted string would be
/// read back as.
const KEYWORDS: &[&str] = &[
    "always",
    "and",
    "anchor",
    "apple",
    "carbon",
    "cdhash",
    "cert",
    "certificate",
    "designated",
    "entitlement",
    "exists",
    "false",
    "generic",
    "guest",
    "host",
    "identifier",
    "info",
    "leaf",
    "legacy",
    "library",
    "never",
    "notarized",
    "or",
    "platform",
    "plugin",
    "root",
    "timestamp",
    "trusted",
    "true",
];

/// How deep inside `and`/`or` an expression sits, which decides its parentheses.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Level {
    Primary,
    And,
    Or,
    Top,
}

/// The requirement of `kind` in a requirement set blob.
pub(crate) fn in_set(set: &[u8], kind: u32) -> Option<&[u8]> {
    if be32(set, 0)? != MAGIC_REQUIREMENT_SET {
        return None;
    }
    let count = be32(set, 8)? as usize;
    (0..count).find_map(|index| {
        let entry = 12 + index * 8;
        if be32(set, entry)? != kind {
            return None;
        }
        let offset = be32(set, entry + 4)? as usize;
        let length = be32(set, offset + 4)? as usize;
        set.get(offset..offset.checked_add(length)?)
    })
}

/// The text of one compiled requirement, or `None` for a malformed one or one
/// using an operation this does not decode.
pub(crate) fn decompile(blob: &[u8]) -> Option<String> {
    if be32(blob, 0)? != MAGIC_REQUIREMENT || be32(blob, 8)? != EXPR_FORM {
        return None;
    }
    let length = be32(blob, 4)? as usize;
    let mut reader = Reader {
        bytes: blob.get(..length)?,
        at: 12,
        out: String::new(),
    };
    reader.expr(Level::Top)?;
    Some(reader.out)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    out: String,
}

impl<'a> Reader<'a> {
    fn word(&mut self) -> Option<u32> {
        let value = be32(self.bytes, self.at)?;
        self.at += 4;
        Some(value)
    }

    /// A length-prefixed run of bytes, padded to a word.
    fn data(&mut self) -> Option<&'a [u8]> {
        let length = self.word()? as usize;
        let start = self.at;
        let bytes = self.bytes.get(start..start.checked_add(length)?)?;
        self.at = start + length.next_multiple_of(4);
        Some(bytes)
    }

    fn expr(&mut self, level: Level) -> Option<()> {
        match self.word()? & !OP_FLAG_MASK {
            op::FALSE => self.out.push_str("never"),
            op::TRUE => self.out.push_str("always"),
            op::IDENT => {
                self.out.push_str("identifier ");
                self.string(false)?;
            }
            op::APPLE_ANCHOR => self.out.push_str("anchor apple"),
            op::APPLE_GENERIC_ANCHOR => self.out.push_str("anchor apple generic"),
            op::ANCHOR_HASH => {
                self.out.push_str("certificate");
                self.cert_slot()?;
                self.out.push_str(" = ");
                self.hash()?;
            }
            op::INFO_KEY_VALUE => {
                self.out.push_str("info[");
                self.string(true)?;
                self.out.push_str("] = ");
                self.string(false)?;
            }
            op::AND => self.join(level, Level::And, " and ")?,
            op::OR => self.join(level, Level::Or, " or ")?,
            op::NOT => {
                self.out.push_str("! ");
                self.expr(Level::Primary)?;
            }
            op::CD_HASH => {
                self.out.push_str("cdhash ");
                self.hash()?;
            }
            op::INFO_KEY_FIELD => self.field("info[")?,
            op::ENTITLEMENT_FIELD => self.field("entitlement[")?,
            op::CERT_FIELD => {
                self.out.push_str("certificate");
                self.cert_slot()?;
                self.field("[")?;
            }
            op::CERT_GENERIC => self.oid_field("field.")?,
            op::CERT_POLICY => self.oid_field("policy.")?,
            op::TRUSTED_CERT => {
                self.out.push_str("certificate");
                self.cert_slot()?;
                self.out.push_str(" trusted");
            }
            op::TRUSTED_CERTS => self.out.push_str("anchor trusted"),
            op::NAMED_ANCHOR => {
                self.out.push_str("anchor ");
                self.string(false)?;
            }
            op::NAMED_CODE => {
                self.out.push('(');
                self.string(false)?;
                self.out.push(')');
            }
            op::PLATFORM => {
                let platform = self.word()?;
                self.out.push_str(&format!("platform = {platform}"));
            }
            op::NOTARIZED => self.out.push_str("notarized"),
            op::LEGACY_DEV_ID => self.out.push_str("legacy"),
            _ => return None,
        }
        Some(())
    }

    fn join(&mut self, outer: Level, inner: Level, word: &str) -> Option<()> {
        let parenthesize = outer < inner;
        if parenthesize {
            self.out.push('(');
        }
        self.expr(inner)?;
        self.out.push_str(word);
        self.expr(inner)?;
        if parenthesize {
            self.out.push(')');
        }
        Some(())
    }

    /// `name[key] <match>`, the key a dotted string.
    fn field(&mut self, name: &str) -> Option<()> {
        self.out.push_str(name);
        self.string(true)?;
        self.out.push(']');
        self.matching()
    }

    /// `certificate <slot>[<prefix><oid>] <match>`.
    fn oid_field(&mut self, prefix: &str) -> Option<()> {
        self.out.push_str("certificate");
        self.cert_slot()?;
        self.out.push('[');
        self.out.push_str(prefix);
        let oid = oid(self.data()?)?;
        self.out.push_str(&oid);
        self.out.push(']');
        self.matching()
    }

    fn cert_slot(&mut self) -> Option<()> {
        match self.word()? as i32 {
            LEAF_CERT => self.out.push_str(" leaf"),
            ANCHOR_CERT => self.out.push_str(" root"),
            slot => self.out.push_str(&format!(" {slot}")),
        }
        Some(())
    }

    fn matching(&mut self) -> Option<()> {
        let (before, after) = match self.word()? {
            matching::EXISTS => {
                self.out.push_str(" /* exists */");
                return Some(());
            }
            matching::ABSENT => {
                self.out.push_str(" absent ");
                return Some(());
            }
            matching::EQUAL => (" = ", ""),
            matching::CONTAINS => (" ~ ", ""),
            matching::BEGINS_WITH => (" = ", "*"),
            matching::ENDS_WITH => (" = *", ""),
            matching::LESS_THAN => (" < ", ""),
            matching::GREATER_THAN => (" > ", ""),
            matching::LESS_EQUAL => (" <= ", ""),
            matching::GREATER_EQUAL => (" >= ", ""),
            _ => return None,
        };
        self.out.push_str(before);
        self.string(false)?;
        self.out.push_str(after);
        Some(())
    }

    fn hash(&mut self) -> Option<()> {
        let bytes = self.data()?;
        self.out.push_str(&hex(bytes));
        Some(())
    }

    /// A string as bare word, quoted text or hex, whichever the bytes need.
    fn string(&mut self, dot_okay: bool) -> Option<()> {
        let bytes = self.data()?;
        let simple = |b: &u8| b.is_ascii_alphanumeric() || (dot_okay && *b == b'.');
        let printable = |b: &u8| b.is_ascii_graphic() || *b == b' ';
        if !bytes.iter().all(printable) {
            self.out.push_str(&hex(bytes));
            return Some(());
        }
        // Every byte is printable ASCII, so this is a lossless view.
        let text = std::str::from_utf8(bytes).ok()?;
        let bare = !text.is_empty()
            && bytes.iter().all(simple)
            && !bytes[0].is_ascii_digit()
            && !KEYWORDS.contains(&text);
        if bare {
            self.out.push_str(text);
            return Some(());
        }
        self.out.push('"');
        for ch in text.chars() {
            if ch == '"' || ch == '\\' {
                self.out.push('\\');
            }
            self.out.push(ch);
        }
        self.out.push('"');
        Some(())
    }
}

/// `H"..."`, the requirement language's spelling of raw bytes.
fn hex(bytes: &[u8]) -> String {
    let digits: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("H\"{digits}\"")
}

/// A DER object identifier's content bytes, in dotted form.
fn oid(bytes: &[u8]) -> Option<String> {
    let (&first, rest) = bytes.split_first()?;
    let mut arcs = vec![u64::from(first / 40), u64::from(first % 40)];
    let mut arc = 0u64;
    for &byte in rest {
        arc = arc.checked_mul(128)? | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            arcs.push(arc);
            arc = 0;
        }
    }
    // A last byte with its continuation bit set leaves an arc unfinished.
    if rest.last().is_some_and(|byte| byte & 0x80 != 0) {
        return None;
    }
    Some(
        arcs.iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("."),
    )
}

fn be32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    fn data(bytes: &[u8]) -> Vec<u8> {
        let mut out = words(&[bytes.len() as u32]);
        out.extend_from_slice(bytes);
        out.resize(4 + bytes.len().next_multiple_of(4), 0);
        out
    }

    fn requirement(body: &[u8]) -> Vec<u8> {
        let mut out = words(&[MAGIC_REQUIREMENT, (12 + body.len()) as u32, EXPR_FORM]);
        out.extend_from_slice(body);
        out
    }

    /// The designated requirement of a Developer ID app, as `codesign -d -r-`
    /// prints it.
    #[test]
    fn decompiles_a_developer_id_designated_requirement() {
        // 1.2.840.113635.100.6.2.6 and 1.2.840.113635.100.6.1.13, DER content.
        let intermediate = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x63, 0x64, 0x06, 0x02, 0x06];
        let leaf = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x63, 0x64, 0x06, 0x01, 0x0d];
        let mut body = words(&[op::AND, op::AND, op::AND, op::AND, op::IDENT]);
        body.extend(data(b"com.example.app"));
        body.extend(words(&[op::APPLE_GENERIC_ANCHOR, op::CERT_GENERIC, 1]));
        body.extend(data(&intermediate));
        body.extend(words(&[matching::EXISTS, op::CERT_GENERIC, 0]));
        body.extend(data(&leaf));
        body.extend(words(&[matching::EXISTS, op::CERT_FIELD, 0]));
        body.extend(data(b"subject.OU"));
        body.extend(words(&[matching::EQUAL]));
        body.extend(data(b"ABCDE12345"));
        assert_eq!(
            decompile(&requirement(&body)).as_deref(),
            Some(
                "identifier \"com.example.app\" and anchor apple generic and \
                 certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and \
                 certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and \
                 certificate leaf[subject.OU] = ABCDE12345"
            )
        );
    }

    #[test]
    fn parenthesizes_an_or_inside_an_and() {
        let body = words(&[op::AND, op::OR, op::TRUE, op::FALSE, op::NOT, op::TRUE]);
        assert_eq!(
            decompile(&requirement(&body)).as_deref(),
            Some("(always or never) and ! always")
        );
    }

    #[test]
    fn refuses_an_unknown_operation() {
        assert_eq!(decompile(&requirement(&words(&[0x7f]))), None);
    }

    #[test]
    fn quotes_keywords_and_escapes_quotes() {
        let mut body = words(&[op::AND, op::IDENT]);
        body.extend(data(b"anchor"));
        body.extend(words(&[op::IDENT]));
        body.extend(data(b"a\"b"));
        assert_eq!(
            decompile(&requirement(&body)).as_deref(),
            Some("identifier \"anchor\" and identifier \"a\\\"b\"")
        );
    }

    #[test]
    fn finds_a_requirement_in_a_set_by_type() {
        let designated = requirement(&words(&[op::APPLE_ANCHOR]));
        let mut set = words(&[MAGIC_REQUIREMENT_SET, 0, 1, DESIGNATED, 20]);
        set.extend_from_slice(&designated);
        let length = set.len() as u32;
        set[4..8].copy_from_slice(&length.to_be_bytes());
        let found = in_set(&set, DESIGNATED).expect("the designated requirement");
        assert_eq!(decompile(found).as_deref(), Some("anchor apple"));
        assert!(in_set(&set, DESIGNATED - 1).is_none());
    }
}
