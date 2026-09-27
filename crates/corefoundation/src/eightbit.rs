//! The one-byte encodings a Mac program names most: Mac Roman, ISO Latin 1 and
//! Windows Latin 1, each ASCII below 0x80 and a table above it.

use crate::string::encoding;

/// Mac OS Roman, 0x80 to 0xFF.
const MAC_ROMAN: [u16; 128] = [
    0x00C4, 0x00C5, 0x00C7, 0x00C9, 0x00D1, 0x00D6, 0x00DC, 0x00E1, 0x00E0, 0x00E2, 0x00E4, 0x00E3,
    0x00E5, 0x00E7, 0x00E9, 0x00E8, 0x00EA, 0x00EB, 0x00ED, 0x00EC, 0x00EE, 0x00EF, 0x00F1, 0x00F3,
    0x00F2, 0x00F4, 0x00F6, 0x00F5, 0x00FA, 0x00F9, 0x00FB, 0x00FC, 0x2020, 0x00B0, 0x00A2, 0x00A3,
    0x00A7, 0x2022, 0x00B6, 0x00DF, 0x00AE, 0x00A9, 0x2122, 0x00B4, 0x00A8, 0x2260, 0x00C6, 0x00D8,
    0x221E, 0x00B1, 0x2264, 0x2265, 0x00A5, 0x00B5, 0x2202, 0x2211, 0x220F, 0x03C0, 0x222B, 0x00AA,
    0x00BA, 0x03A9, 0x00E6, 0x00F8, 0x00BF, 0x00A1, 0x00AC, 0x221A, 0x0192, 0x2248, 0x2206, 0x00AB,
    0x00BB, 0x2026, 0x00A0, 0x00C0, 0x00C3, 0x00D5, 0x0152, 0x0153, 0x2013, 0x2014, 0x201C, 0x201D,
    0x2018, 0x2019, 0x00F7, 0x25CA, 0x00FF, 0x0178, 0x2044, 0x20AC, 0x2039, 0x203A, 0xFB01, 0xFB02,
    0x2021, 0x00B7, 0x201A, 0x201E, 0x2030, 0x00C2, 0x00CA, 0x00C1, 0x00CB, 0x00C8, 0x00CD, 0x00CE,
    0x00CF, 0x00CC, 0x00D3, 0x00D4, 0xF8FF, 0x00D2, 0x00DA, 0x00DB, 0x00D9, 0x0131, 0x02C6, 0x02DC,
    0x00AF, 0x02D8, 0x02D9, 0x02DA, 0x00B8, 0x02DD, 0x02DB, 0x02C7,
];

/// Windows-1252, 0x80 to 0x9F; above that it is ISO Latin 1. The five bytes
/// Windows leaves undefined map to the C1 controls of the same value, as
/// CoreFoundation's own converter does.
const WINDOWS_LATIN1: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
];

/// Whether `code` is one of the table encodings here, or ASCII.
pub fn is_table(code: u32) -> bool {
    matches!(
        code,
        encoding::ASCII | encoding::ISO_LATIN_1 | encoding::MAC_ROMAN | encoding::WINDOWS_LATIN1
    )
}

/// The character `byte` is in `code`, or `None` for ASCII above 0x7F.
pub fn decode(byte: u8, code: u32) -> Option<u16> {
    if byte < 0x80 {
        return Some(u16::from(byte));
    }
    match code {
        encoding::MAC_ROMAN => Some(MAC_ROMAN[usize::from(byte - 0x80)]),
        encoding::WINDOWS_LATIN1 if byte < 0xA0 => Some(WINDOWS_LATIN1[usize::from(byte - 0x80)]),
        encoding::WINDOWS_LATIN1 | encoding::ISO_LATIN_1 => Some(u16::from(byte)),
        _ => None,
    }
}

/// The byte `unit` is in `code`, or `None` for a character it has not got.
pub fn encode(unit: u16, code: u32) -> Option<u8> {
    if unit < 0x80 {
        return Some(unit as u8);
    }
    let found = match code {
        encoding::MAC_ROMAN => MAC_ROMAN.iter().position(|u| *u == unit),
        encoding::WINDOWS_LATIN1 => match WINDOWS_LATIN1.iter().position(|u| *u == unit) {
            Some(at) => Some(at),
            None if (0xA0..=0xFF).contains(&unit) => return Some(unit as u8),
            None => None,
        },
        encoding::ISO_LATIN_1 if unit <= 0xFF => return Some(unit as u8),
        _ => None,
    };
    found.map(|at| 0x80 + at as u8)
}

/// `bytes` in `code` as UTF-16, or `None` if one has no character.
pub fn decode_all(bytes: &[u8], code: u32) -> Option<Vec<u16>> {
    bytes.iter().map(|b| decode(*b, code)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_table_round_trips_its_upper_half() {
        for code in [
            encoding::MAC_ROMAN,
            encoding::WINDOWS_LATIN1,
            encoding::ISO_LATIN_1,
        ] {
            for byte in 0x80..=0xFFu8 {
                let unit = decode(byte, code).unwrap();
                assert_eq!(encode(unit, code), Some(byte), "{code:#x} {byte:#x}");
            }
        }
    }

    #[test]
    fn the_trademark_sign_has_a_byte_in_each_table_but_latin_1() {
        assert_eq!(encode(0x2122, encoding::MAC_ROMAN), Some(0xAA));
        assert_eq!(encode(0x2122, encoding::WINDOWS_LATIN1), Some(0x99));
        assert_eq!(encode(0x2122, encoding::ISO_LATIN_1), None);
        assert_eq!(decode(0xC9, encoding::MAC_ROMAN), Some(0x2026));
    }
}
