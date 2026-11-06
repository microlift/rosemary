//! Small Unicode queries this crate needs that are not string transforms -
//! those go through `unicode-normalization` in `api::CFStringNormalize`
//! instead.

use unicode_general_category::{GeneralCategory, get_general_category};

/// Whether `c` is a combining mark - general category Mn, Mc or Me - which is
/// the Unicode property a grapheme cluster extends over.
///
/// Used by `api::CFStringGetRangeOfComposedCharactersAtIndex`; see its own
/// `FIXME (ACCURACY)` for what this does not cover.
pub fn is_combining_mark(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::NonspacingMark
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combining_marks_are_recognized_and_plain_letters_are_not() {
        // U+0301 COMBINING ACUTE ACCENT, as in "e" + "´" = "é".
        assert!(is_combining_mark('\u{0301}'));
        assert!(!is_combining_mark('e'));
    }
}
