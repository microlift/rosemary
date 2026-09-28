/// Arity, offset and frame checks for the type encodings this crate installs.
///
/// **Test-only.** Nothing in the library calls it, so it is compiled out of a
/// release build entirely. It exists because the project-level gate
/// (`NAME-ARITY 0` and `FRAME-SHORT 0` from `test/tools/objcenc.py diff`) cannot
/// see a row that arrives through a `const` slice, and it certifies a
/// *declaration* rather than what reached the runtime - which
/// [`assert_installed`] then closes.
///
/// # What went wrong twice, and what this checks instead
///
/// A self-consistent encoding can still be wrong. `crates/appkit/src/
/// appearance.rs` declared
///
/// ```text
/// "performAsCurrentDrawingAppearance:" => …,  c"v32@0:8@16@?24"
/// ```
///
/// for a selector with **one** colon. The frame is `last_offset + 8`, so the
/// string passes every internal check; the argument count is `3 - 2 = 1`, which
/// matches the one colon; and it was still wrong, because `@0:8` is *one*
/// argument - `self` at offset 0 with a size of 8 - and the next `@16` puts
/// `_cmd` at **16** instead of 8, leaving a hole where `_cmd` belongs. `rdx`
/// then held the block, not `rcx`, and the process died executing a stale
/// pointer. Its unit test stayed green throughout because the test sent the same
/// four registers.
///
/// So this does not count arguments and stop. It **collects every offset and
/// requires the `i`th to be exactly `8 * i`** - which is the property the
/// `appearance.rs` string violates and the property a colon count cannot see.
///
/// # The grammar
///
/// ```text
/// <return> <frame> <arg>*
/// <arg>                 ::= <type> <offset> [ ':' <size> ]
/// <return>              ::= v | @ | # | : | B | c | i | s | l | q | C | I | S | L | Q
///                         | f | d | D
/// <frame>               ::= decimal          (always, for every return type)
/// <type>                ::= v @ * ^ # : ? B c i s l q C I S L Q f d D
/// <offset>, <size>      ::= decimal
/// ```
///
/// **`_cmd` is not encoded.** `@16@0:8` is one argument - `self` at offset 0 -
/// and a 16-byte frame, so the word at offset 8 is the second register
/// `objc_msgSend` passes and the string does not mention it. An encoding
/// therefore carries `1 + colons` arguments and its frame is `8 * (2 + colons)`:
/// `self`, the unencoded `_cmd`, and one word per argument of the selector. It
/// is also why the offsets step by eight but do not start there - `self` is at
/// 0, the first real argument is at 16, and 8 belongs to `_cmd`.
///
/// A `'@'` type may be followed by a `'?'` for a pointer to an unknown pointee,
/// as in `@?24` for a block or an `NSError **`. Anything else - a struct
/// (`{...}`), an unusual pointee - **panics with the encoding in the message**
/// rather than guessing, because a checker that silently mis-parses what it does
/// not understand is worse than no checker: it reports green.
use std::ffi::CStr;

const TYPES: &[u8] = b"v@*^#:?BcsilqCISLQfdD";

/// One argument: where it sits, and how wide it is declared to be.
#[derive(Debug, PartialEq)]
pub struct Argument {
    pub offset: usize,
    pub size: Option<usize>,
}

/// Every argument the encoding declares, `self` and `_cmd` included.
pub fn arguments(types: &CStr) -> Vec<Argument> {
    let text = types.to_bytes();
    let mut at = 0usize;
    // The return type and then its frame. Every return type declares a frame -
    // `v32` and `i16` as much as `@16` - and it is the first number in the
    // string, which is what tells it apart from the arguments' offsets below.
    skip_type(text, &mut at, types);
    skip_digits(text, &mut at, types);
    let mut out = Vec::new();
    while at < text.len() {
        skip_type(text, &mut at, types);
        let offset = skip_digits(text, &mut at, types);
        let size = if at < text.len() && text[at] == b':' {
            at += 1;
            Some(skip_digits(text, &mut at, types))
        } else {
            None
        };
        out.push(Argument { offset, size });
    }
    out
}

/// The frame size an encoding declares: the first number in the string, which
/// follows the return type.
pub fn declared_frame(types: &CStr) -> usize {
    let text = types.to_bytes();
    let mut at = 0usize;
    skip_type(text, &mut at, types);
    skip_digits(text, &mut at, types)
}

/// Every declared encoding has `selector`'s colon count plus the two implicit
/// words (`self`, `_cmd`), every argument sits at `8 * i`, and the frame reaches
/// the last of them.
pub fn assert_row(selector: &str, types: &CStr) {
    let colons = selector.bytes().filter(|b| *b == b':').count();
    let arguments = arguments(types);
    // `_cmd` is not encoded, so a string carries one argument for `self` and one
    // per colon - and *not* two, which is the mistake an `@` count makes.
    assert_eq!(
        colons + 1,
        arguments.len(),
        "{selector} has {colons} colon(s), so {} argument(s) counting self and not the \
         unencoded _cmd, but {} declares {}",
        colons + 1,
        types.to_string_lossy(),
        arguments.len()
    );
    // `self` is at 0 and every argument after it is one word further, with the
    // word at 8 belonging to `_cmd`, which the string does not mention.
    for (i, argument) in arguments.iter().enumerate() {
        let expected = if i == 0 { 0 } else { 8 * (i + 1) };
        assert_eq!(
            argument.offset, expected,
            "{selector}: argument {i} is declared at {} where {expected} was expected, so a \
             word objc_msgSend writes is being read for something else",
            argument.offset
        );
        if let Some(size) = argument.size {
            assert_eq!(
                size, 8,
                "{selector}: argument {i} declares a size of {size}; only self is sized in a \
                 string this tree writes, and it is eight bytes"
            );
        }
    }
    let frame = declared_frame(types);
    assert_eq!(
        frame,
        8 * (2 + colons),
        "{selector}: the frame is {frame} and the selector's shape wants {}, which is self, \
         _cmd and one word per argument",
        8 * (2 + colons)
    );
    let last = arguments.last().expect("self is always there").offset;
    assert!(
        frame >= last + 8,
        "{selector}: the frame is {frame} but the last argument ends at {}, so one of its \
         registers is outside the frame objc_msgSend writes",
        last + 8
    );
}

/// The encoding that is **actually installed** is the one the table declares.
///
/// This is what makes [`assert_row`] mean anything, and it exists because of a
/// measured near-miss. `test/tools/objcenc.py diff` reads a `#[cfg(test)]`
/// install table as readily as it reads a `class_methods!` body - the two AppKit
/// rows in this round were reported from the table, not from the macro - so
/// arity, offset and frame can all be certified against a **copy** of the
/// encoding that never reached the runtime. A copy that is certified but never
/// compared is the "a test that shares the author's misreading" failure with an
/// extra step.
///
/// So this asks the runtime. `Method::types` is the string `replace_method` was
/// handed, read back out of the class's own method list.
///
/// # Panics
///
/// If the selector is not installed on the class's metaclass, or is installed
/// with a different encoding.
pub fn assert_installed(class: &std::ffi::CStr, selector: &str, declared: &CStr) {
    // SAFETY: `Method::types` hands back the `CString` the method was installed
    // with; every `Method` in this runtime is leaked and never freed, which is
    // what the type's own comment says.
    let installed = objc::class::lookup(class)
        .unwrap_or_else(|| panic!("{class:?} is not registered"))
        .metaclass()
        .method(objc::sel::get(selector))
        .unwrap_or_else(|| panic!("+[{class:?} {selector}] is not installed"));
    // SAFETY: as above.
    let installed = unsafe { std::ffi::CStr::from_ptr(installed.types()) };
    assert_eq!(
        installed.to_bytes(),
        declared.to_bytes(),
        "+[{class:?} {selector}] is installed with {} but the table declares {}",
        installed.to_string_lossy(),
        declared.to_string_lossy()
    );
}

/// Skip one type specifier; say whether it was an object.
///
/// # Panics
///
/// On anything outside [`TYPES`], and on a struct.
fn skip_type(text: &[u8], at: &mut usize, types: &CStr) -> bool {
    assert!(
        *at < text.len(),
        "ran off the end of {} while reading a type",
        types.to_string_lossy()
    );
    let c = text[*at];
    if c == b'{' {
        panic!(
            "this checker does not parse struct arguments, so it cannot certify {}; a struct is \\
             one Rust parameter and several words, and pretending otherwise is how a wrong \\
             offset gets called a pass",
            types.to_string_lossy()
        );
    }
    assert!(
        TYPES.contains(&c),
        "unexpected byte {:?} at offset {at} in {}",
        c as char,
        types.to_string_lossy()
    );
    *at += 1;
    // A generic pointer carries its pointee: `@?24` is a block, `@*16` a `char *`.
    if (c == b'@' || c == b'^' || c == b'*') && *at < text.len() && text[*at] == b'?' {
        *at += 1;
    }
    c == b'@'
}

fn skip_digits(text: &[u8], at: &mut usize, types: &CStr) -> usize {
    let start = *at;
    while *at < text.len() && text[*at].is_ascii_digit() {
        *at += 1;
    }
    assert_ne!(
        start,
        *at,
        "expected a number at offset {start} in {}",
        types.to_string_lossy()
    );
    text[start..*at]
        .iter()
        .fold(0usize, |n, b| n * 10 + (b - b'0') as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The control: the encoding this project shipped and that **was** wrong.
    ///
    /// `v32@0:8@16@?24` on a **one**-colon selector declares three arguments
    /// where there may be two (`self` and one), and its frame of 32 is exactly
    /// `8 * 4` - perfectly consistent with the *wrong* count. That is the whole
    /// trap: the string passes every check that can be made against the string,
    /// and only the selector's own name says it is wrong.
    #[test]
    fn the_arity_this_catches_is_caught() {
        assert_eq!(
            arguments(c"v32@0:8@16@?24"),
            vec![
                Argument {
                    offset: 0,
                    size: Some(8)
                },
                Argument {
                    offset: 16,
                    size: None
                },
                Argument {
                    offset: 24,
                    size: None
                },
            ],
            "self, then a word at 16 and a word at 24 - three, for a one-colon selector"
        );
        assert!(
            std::panic::catch_unwind(|| {
                assert_row("performAsCurrentDrawingAppearance:", c"v32@0:8@16@?24")
            })
            .is_err(),
            "a self-consistent encoding with the wrong arity must fail"
        );
    }

    /// The same selector spelled correctly - two arguments, a 24-byte frame - so
    /// the control above is not passing because the checker refuses everything.
    #[test]
    fn the_correct_spelling_passes() {
        assert_row("performAsCurrentDrawingAppearance:", c"v24@0:8@?16");
    }

    /// A non-object argument is still an argument. This is the case an `@`-count
    /// gets wrong, and it is why this walks the string.
    #[test]
    fn a_non_object_argument_is_counted() {
        // `self`, the `NSData`, two `NSUInteger`s and the `NSError **` - five,
        // of which two carry no `@` at all.
        assert_eq!(arguments(c"@48@0:8@16Q24Q32@?40").len(), 5);
        assert_row(
            "propertyListWithData:options:format:error:",
            c"@48@0:8@16Q24Q32@?40",
        );
    }

    /// A frame that stops short of its last argument is the other half of the
    /// gate, and it is silent otherwise: the guest reads the second argument out
    /// of a register nobody wrote.
    #[test]
    fn a_frame_that_stops_short_is_caught() {
        assert!(std::panic::catch_unwind(|| assert_row("two:", c"@16@0:8@16")).is_err());
    }

    /// A nullary method encodes one argument and a 16-byte frame, because
    /// `_cmd` is not written out. This is the shape both of the round's AppKit
    /// rows have, and getting it wrong in the *other* direction - declaring
    /// `_cmd` - is a frame eight bytes too large.
    #[test]
    fn a_nullary_row_is_one_argument_and_a_sixteen_byte_frame() {
        assert_eq!(arguments(c"@16@0:8").len(), 1);
        assert_row("sharedSpellChecker", c"@16@0:8");
    }

    /// An encoding this checker cannot parse must fail loudly, not pass quietly.
    #[test]
    fn an_unparseable_encoding_panics_rather_than_guessing() {
        assert!(
            std::panic::catch_unwind(|| {
                arguments(c"@64@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16")
            })
            .is_err()
        );
    }
}
