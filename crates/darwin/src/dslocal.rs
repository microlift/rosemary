//! The prefix's local directory node: the users and groups `dscl` and
//! `dseditgroup` keep under `/private/var/db/dslocal/nodes/Default`, one
//! property list per record, as macOS keeps them.
//!
//! Each record is a dictionary of attribute names to arrays of strings. Only
//! XML property lists of that shape are read; a record in any other form
//! (macOS's own tools write binary ones) is reported and left out rather
//! than guessed at.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// The node's directory, as the guest spells it.
const NODE: &str = "/private/var/db/dslocal/nodes/Default";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Users,
    Groups,
}

impl Kind {
    fn directory(self) -> &'static str {
        match self {
            Kind::Users => "users",
            Kind::Groups => "groups",
        }
    }
}

/// One record: its attributes, by dslocal's own names (`name`, `uid`, ...).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    attributes: Vec<(String, Vec<String>)>,
}

impl Record {
    /// Every value of `attribute`, none when the record lacks it.
    pub fn values(&self, attribute: &str) -> &[String] {
        self.attributes
            .iter()
            .find(|(name, _)| name == attribute)
            .map_or(&[], |(_, values)| values.as_slice())
    }

    /// The first value of `attribute`.
    pub fn first(&self, attribute: &str) -> Option<&str> {
        self.values(attribute).first().map(String::as_str)
    }
}

/// The records of `kind`, or none outside a prefix: there, the host's own
/// account database is the answer.
pub fn records(kind: Kind) -> Arc<Vec<Record>> {
    if prefix::ns::current().is_none() {
        return Arc::default();
    }
    let directory = prefix::ns::host_path(&format!("{NODE}/{}", kind.directory()));
    cached(kind, &directory)
}

type Cache = Option<(Option<SystemTime>, Arc<Vec<Record>>)>;

/// The records under `directory`, read again only when it has changed. `dscl`
/// replaces a record by renaming a new file over it, which a directory's
/// modification time shows.
fn cached(kind: Kind, directory: &Path) -> Arc<Vec<Record>> {
    static USERS: Mutex<Cache> = Mutex::new(None);
    static GROUPS: Mutex<Cache> = Mutex::new(None);
    let cache = match kind {
        Kind::Users => &USERS,
        Kind::Groups => &GROUPS,
    };
    let modified = std::fs::metadata(directory)
        .and_then(|meta| meta.modified())
        .ok();
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((seen, records)) = cache.as_ref()
        && *seen == modified
    {
        return records.clone();
    }
    let records = Arc::new(match modified {
        Some(_) => read_directory(directory),
        None => Vec::new(),
    });
    *cache = Some((modified, records.clone()));
    records
}

fn read_directory(directory: &Path) -> Vec<Record> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "plist"))
        .collect();
    paths.sort();
    paths
        .iter()
        .filter_map(|path| {
            let bytes = std::fs::read(path).ok()?;
            match parse(&bytes) {
                Ok(record) => Some(record),
                Err(why) => {
                    eprintln!(
                        "rosemary: dslocal: {}: {why}; record ignored",
                        path.display()
                    );
                    None
                }
            }
        })
        .collect()
}

/// A record from the XML property list `bytes`.
pub fn parse(bytes: &[u8]) -> Result<Record, String> {
    if bytes.starts_with(b"bplist") {
        return Err("binary property lists are not read".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
    let mut xml = Xml { rest: text };
    xml.skip_prolog()?;
    xml.open("plist")?;
    xml.open("dict")?;
    let mut attributes: Vec<(String, Vec<String>)> = Vec::new();
    loop {
        match xml.tag()? {
            Tag::Close("dict") => break,
            Tag::Open("key") => {}
            other => return Err(format!("expected a key, found {other:?}")),
        }
        let key = xml.text_until("key")?;
        if attributes.iter().any(|(name, _)| *name == key) {
            return Err(format!("duplicate key {key}"));
        }
        let values = match xml.tag()? {
            Tag::Empty("array") => Vec::new(),
            Tag::Open("array") => {
                let mut values = Vec::new();
                loop {
                    match xml.tag()? {
                        Tag::Close("array") => break,
                        Tag::Empty("string") => values.push(String::new()),
                        Tag::Open("string") => values.push(xml.text_until("string")?),
                        other => return Err(format!("{key}: expected a string, found {other:?}")),
                    }
                }
                values
            }
            other => return Err(format!("{key}: expected an array, found {other:?}")),
        };
        attributes.push((key, values));
    }
    xml.close("plist")?;
    if !xml.rest.trim().is_empty() {
        return Err("content after the property list".into());
    }
    Ok(Record { attributes })
}

#[derive(Debug, PartialEq, Eq)]
enum Tag<'a> {
    Open(&'a str),
    Close(&'a str),
    Empty(&'a str),
}

struct Xml<'a> {
    rest: &'a str,
}

impl<'a> Xml<'a> {
    /// Past the XML declaration, the DOCTYPE and any comments, which say
    /// nothing about the record.
    fn skip_prolog(&mut self) -> Result<(), String> {
        loop {
            self.rest = self.rest.trim_start();
            let end = if self.rest.starts_with("<?") {
                "?>"
            } else if self.rest.starts_with("<!--") {
                "-->"
            } else if self.rest.starts_with("<!DOCTYPE") {
                if self.rest[..self.rest.find('>').unwrap_or(0)].contains('[') {
                    return Err("DOCTYPE declarations are not read".into());
                }
                ">"
            } else {
                return Ok(());
            };
            let at = self.rest.find(end).ok_or("unterminated prolog")?;
            self.rest = &self.rest[at + end.len()..];
        }
    }

    fn tag(&mut self) -> Result<Tag<'a>, String> {
        loop {
            self.rest = self.rest.trim_start();
            if !self.rest.starts_with("<!--") {
                break;
            }
            let at = self.rest.find("-->").ok_or("unterminated comment")?;
            self.rest = &self.rest[at + 3..];
        }
        let inner = self.rest.strip_prefix('<').ok_or("expected a tag")?;
        let end = inner.find('>').ok_or("unterminated tag")?;
        let body = &inner[..end];
        self.rest = &inner[end + 1..];
        let name = |text: &'a str| text.split_whitespace().next().unwrap_or("");
        Ok(match (body.strip_prefix('/'), body.strip_suffix('/')) {
            (Some(closed), _) => Tag::Close(name(closed)),
            (None, Some(empty)) => Tag::Empty(name(empty)),
            (None, None) => Tag::Open(name(body)),
        })
    }

    fn open(&mut self, want: &str) -> Result<(), String> {
        match self.tag()? {
            Tag::Open(name) if name == want => Ok(()),
            other => Err(format!("expected <{want}>, found {other:?}")),
        }
    }

    fn close(&mut self, want: &str) -> Result<(), String> {
        match self.tag()? {
            Tag::Close(name) if name == want => Ok(()),
            other => Err(format!("expected </{want}>, found {other:?}")),
        }
    }

    /// Character data up to `</name>`, with its references resolved.
    fn text_until(&mut self, name: &str) -> Result<String, String> {
        let at = self.rest.find('<').ok_or("unterminated text")?;
        let raw = &self.rest[..at];
        self.rest = &self.rest[at..];
        self.close(name)?;
        unescape(raw)
    }
}

fn unescape(raw: &str) -> Result<String, String> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let end = rest[at..].find(';').ok_or("unterminated reference")? + at;
        let name = &rest[at + 1..end];
        let ch = match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = match name.strip_prefix("#x") {
                    Some(hex) => u32::from_str_radix(hex, 16),
                    None => name.strip_prefix('#').unwrap_or("x").parse(),
                };
                code.ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| format!("unknown reference &{name};"))?
            }
        };
        out.push(ch);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACPORTS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>name</key>
	<array>
		<string>macports</string>
	</array>
	<key>realname</key>
	<array>
		<string>Mac &amp; Ports</string>
	</array>
	<key>uid</key>
	<array>
		<string>502</string>
	</array>
	<key>users</key>
	<array/>
	<key>passwd</key>
	<array>
		<string/>
	</array>
</dict>
</plist>
"#;

    #[test]
    fn a_record_is_its_attributes_as_string_arrays() {
        let record = parse(MACPORTS.as_bytes()).unwrap();
        assert_eq!(record.first("name"), Some("macports"));
        assert_eq!(record.first("realname"), Some("Mac & Ports"));
        assert_eq!(record.first("uid"), Some("502"));
        assert!(record.values("users").is_empty());
        assert_eq!(record.values("passwd"), [String::new()]);
        assert!(record.values("shell").is_empty());
    }

    #[test]
    fn other_shapes_are_refused_rather_than_guessed_at() {
        assert!(parse(b"bplist00").is_err());
        let integer = MACPORTS.replace("<string>502</string>", "<integer>502</integer>");
        assert!(parse(integer.as_bytes()).is_err());
        let doubled = MACPORTS.replace("<key>users</key>", "<key>uid</key>");
        assert!(parse(doubled.as_bytes()).is_err());
        let doctype = MACPORTS.lines().nth(1).unwrap();
        let entity = MACPORTS.replace(doctype, "<!DOCTYPE plist [<!ENTITY x \"y\">]>");
        assert!(parse(entity.as_bytes()).is_err());
    }

    #[test]
    fn a_changed_directory_is_read_again() {
        let directory = std::env::temp_dir().join(format!("dslocal-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        assert!(cached(Kind::Groups, &directory).is_empty());
        // Coarse directory timestamps would hide a change made within the
        // same tick, so the time is moved on explicitly.
        std::fs::write(directory.join("macports.plist"), MACPORTS).unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::open(&directory)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let records = cached(Kind::Groups, &directory);
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].first("name"), Some("macports"));
    }
}
