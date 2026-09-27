//! `.strings` tables: the OpenStep `"key" = "value";` text a bundle localizes
//! with, in UTF-16 or UTF-8, or the property list `plutil` compiles it into.

use std::collections::HashMap;

/// The pairs in `bytes`, or `None` for a file that is neither form.
pub fn parse(bytes: &[u8]) -> Option<HashMap<String, String>> {
    if bytes.starts_with(b"bplist") || bytes.trim_ascii_start().starts_with(b"<?xml") {
        return from_plist(bytes);
    }
    let text = decode(bytes)?;
    Parser {
        chars: text.chars().collect(),
        at: 0,
    }
    .pairs()
}

fn from_plist(bytes: &[u8]) -> Option<HashMap<String, String>> {
    let crate::plist::Value::Dictionary(entries) = crate::plist::parse(bytes).ok()? else {
        return None;
    };
    Some(
        entries
            .into_iter()
            .filter_map(|(key, value)| Some((key, value.as_str()?.to_owned())))
            .collect(),
    )
}

/// UTF-16 by its byte-order mark, as Xcode writes these; UTF-8 otherwise.
fn decode(bytes: &[u8]) -> Option<String> {
    let units = |big: bool| {
        bytes[2..]
            .chunks_exact(2)
            .map(|pair| match big {
                true => u16::from_be_bytes([pair[0], pair[1]]),
                false => u16::from_le_bytes([pair[0], pair[1]]),
            })
            .collect::<Vec<u16>>()
    };
    match bytes {
        [0xff, 0xfe, ..] => String::from_utf16(&units(false)).ok(),
        [0xfe, 0xff, ..] => String::from_utf16(&units(true)).ok(),
        [0xef, 0xbb, 0xbf, rest @ ..] => String::from_utf8(rest.to_vec()).ok(),
        _ => String::from_utf8(bytes.to_vec()).ok(),
    }
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    /// Past whitespace and both comment forms.
    fn skip(&mut self) {
        loop {
            match (self.peek(), self.chars.get(self.at + 1)) {
                (Some(c), _) if c.is_whitespace() => self.at += 1,
                (Some('/'), Some('*')) => {
                    self.at += 2;
                    while self.at < self.chars.len()
                        && !(self.chars[self.at] == '*'
                            && self.chars.get(self.at + 1) == Some(&'/'))
                    {
                        self.at += 1;
                    }
                    self.at += 2;
                }
                (Some('/'), Some('/')) => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.at += 1;
                    }
                }
                _ => return,
            }
        }
    }

    fn pairs(mut self) -> Option<HashMap<String, String>> {
        let mut out = HashMap::new();
        loop {
            self.skip();
            if self.peek().is_none() {
                return Some(out);
            }
            let key = self.token()?;
            self.skip();
            // A bare `"key";` is a key that localizes to itself.
            let value = match self.peek()? {
                '=' => {
                    self.at += 1;
                    self.skip();
                    self.token()?
                }
                _ => key.clone(),
            };
            self.skip();
            if self.peek()? != ';' {
                return None;
            }
            self.at += 1;
            out.insert(key, value);
        }
    }

    /// A quoted string with its escapes, or a bare word.
    fn token(&mut self) -> Option<String> {
        if self.peek()? != '"' {
            let start = self.at;
            while self
                .peek()
                .is_some_and(|c| c.is_alphanumeric() || "_$+/:.-".contains(c))
            {
                self.at += 1;
            }
            return (self.at > start).then(|| self.chars[start..self.at].iter().collect());
        }
        self.at += 1;
        let mut out = String::new();
        loop {
            let c = self.peek()?;
            self.at += 1;
            match c {
                '"' => return Some(out),
                '\\' => {
                    let escaped = self.peek()?;
                    self.at += 1;
                    match escaped {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'a' => out.push('\u{7}'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'v' => out.push('\u{b}'),
                        'U' | 'u' => {
                            let digits: String = self.chars[self.at..].iter().take(4).collect();
                            self.at += digits.len();
                            let unit = u32::from_str_radix(&digits, 16).ok()?;
                            out.push(char::from_u32(unit).unwrap_or('\u{fffd}'));
                        }
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_utf16_pairs_with_comments_and_escapes() {
        let text =
            "/* note */\n\"Play\" = \"Jouer\";\n// line\n\"Quote\" = \"a \\\"b\\\"\\n\";\nSame;";
        let mut bytes = vec![0xff, 0xfe];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let table = parse(&bytes).unwrap();
        assert_eq!(table["Play"], "Jouer");
        assert_eq!(table["Quote"], "a \"b\"\n");
        assert_eq!(table["Same"], "Same");
    }
}
