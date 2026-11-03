//! Method type encodings walked the way Apple's `objc-typeencoding.mm` walks
//! them, for the `method_get...Type` family.
//!
//! An encoding is the return type, the frame size, then each argument's type
//! followed by its frame offset: `v24@0:8@16`.

/// The length of the first type in `b`, qualifiers included.
///
/// Apple's `SkipFirstType`. A quoted class name after `@` is not skipped,
/// because the runtime's method encodings never carry one.
pub fn first_type_len(b: &[u8]) -> usize {
    let mut at = 0;
    while let Some(&c) = b.get(at) {
        at += 1;
        match c {
            // bycopy, in, out, inout, const, oneway, and a pointer's pointee.
            b'O' | b'n' | b'o' | b'N' | b'r' | b'V' | b'^' => {}
            b'@' => {
                // A block.
                if b.get(at) == Some(&b'?') {
                    at += 1;
                }
                return at;
            }
            b'[' => {
                while b.get(at).is_some_and(u8::is_ascii_digit) {
                    at += 1;
                }
                return group_end(b, at, b']');
            }
            b'{' => return group_end(b, at, b'}'),
            b'(' => return group_end(b, at, b')'),
            _ => return at,
        }
    }
    at
}

/// Just past the `end` that closes the group whose contents start at `at`, or
/// the end of a truncated encoding.
fn group_end(b: &[u8], mut at: usize, end: u8) -> usize {
    let mut level = 0usize;
    while let Some(&c) = b.get(at) {
        if level == 0 && c == end {
            return at + 1;
        }
        match c {
            b']' | b'}' | b')' => level = level.saturating_sub(1),
            b'[' | b'{' | b'(' => level += 1,
            _ => {}
        }
        at += 1;
    }
    at
}

fn skip_offset(b: &[u8], mut at: usize) -> usize {
    // A GNU register hint, then a frame offset that may be negative.
    if b.get(at) == Some(&b'+') {
        at += 1;
    }
    if b.get(at) == Some(&b'-') {
        at += 1;
    }
    while b.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
    }
    at
}

/// Each argument's type, `self` and `_cmd` included.
pub fn arguments(b: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut at = first_type_len(b);
    while b.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
    }
    std::iter::from_fn(move || {
        let rest = b.get(at..).filter(|rest| !rest.is_empty())?;
        let len = first_type_len(rest);
        at = skip_offset(b, at + len);
        Some(&rest[..len])
    })
}

/// The return type.
pub fn return_type(b: &[u8]) -> &[u8] {
    &b[..first_type_len(b)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_method_encoding_into_its_arguments() {
        let encoding = b"v24@0:8@16";
        assert_eq!(return_type(encoding), b"v");
        let args: Vec<&[u8]> = arguments(encoding).collect();
        assert_eq!(args, [&b"@"[..], b":", b"@"]);
    }

    #[test]
    fn keeps_aggregates_qualifiers_and_blocks_whole() {
        let encoding = b"r^{CGPoint=dd}40@0:8[4{x=i}]16(u=if)24@?32";
        assert_eq!(return_type(encoding), b"r^{CGPoint=dd}");
        let args: Vec<&[u8]> = arguments(encoding).collect();
        assert_eq!(args, [&b"@"[..], b":", b"[4{x=i}]", b"(u=if)", b"@?"]);
    }

    #[test]
    fn counts_arguments_without_offsets() {
        assert_eq!(arguments(b"v@:@").count(), 3);
        assert_eq!(arguments(b"v").count(), 0);
    }
}
