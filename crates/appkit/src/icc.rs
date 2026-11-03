//! The ICC profile of the one colour space this runtime draws in, sRGB (see
//! `crate::color`), so a program that reads a screen's or a space's profile is
//! told what the pixels already are. Built here rather than shipped as a blob:
//! every value is a published sRGB constant, written out as an ICC v4 display
//! profile (ICC.1:2022).

/// sRGB's primaries, adapted to the D50 profile connection space with the
/// Bradford transform, as every sRGB profile carries them.
const RED: [f64; 3] = [0.436066, 0.222488, 0.013916];
const GREEN: [f64; 3] = [0.385147, 0.716873, 0.097076];
const BLUE: [f64; 3] = [0.143066, 0.060608, 0.714096];

/// The PCS illuminant, which a v4 display profile also gives as its white.
const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

/// The Bradford adaptation from sRGB's D65 white to D50, row by row.
const D65_TO_D50: [f64; 9] = [
    1.047882, 0.022918, -0.050217, //
    0.029586, 0.990478, -0.017075, //
    -0.009247, 0.015075, 0.751678,
];

/// IEC 61966-2-1's transfer function as ICC parametric curve type 3:
/// `Y = (aX + b)^g` for `X >= d`, else `Y = cX`, listed as g, a, b, c, d.
const SRGB_CURVE_FUNCTION: u16 = 3;
const SRGB_CURVE: [f64; 5] = [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045];

const DESCRIPTION: &str = "sRGB IEC61966-2.1";
const COPYRIGHT: &str = "No copyright, use freely";

const HEADER_LEN: usize = 128;
const TAG_ENTRY_LEN: usize = 12;
const VERSION_4_3: u32 = 0x0430_0000;

fn s15_fixed16(value: f64) -> [u8; 4] {
    ((value * 65536.0).round() as i32).to_be_bytes()
}

fn xyz_numbers(values: &[f64]) -> Vec<u8> {
    values.iter().flat_map(|&v| s15_fixed16(v)).collect()
}

/// A tag's type signature and the four reserved bytes after it.
fn typed(signature: &[u8; 4], body: &[u8]) -> Vec<u8> {
    [signature.as_slice(), &[0; 4], body].concat()
}

fn xyz(values: [f64; 3]) -> Vec<u8> {
    typed(b"XYZ ", &xyz_numbers(&values))
}

fn parametric_curve() -> Vec<u8> {
    let mut body = SRGB_CURVE_FUNCTION.to_be_bytes().to_vec();
    body.extend([0; 2]);
    body.extend(xyz_numbers(&SRGB_CURVE));
    typed(b"para", &body)
}

/// A one-record `multiLocalizedUnicodeType` in US English.
fn localized(text: &str) -> Vec<u8> {
    const RECORD_LEN: u32 = 12;
    const FIRST_RECORD: u32 = 16;
    let utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let mut body = Vec::new();
    body.extend(1u32.to_be_bytes());
    body.extend(RECORD_LEN.to_be_bytes());
    body.extend(b"enUS");
    body.extend((utf16.len() as u32).to_be_bytes());
    body.extend((FIRST_RECORD + RECORD_LEN).to_be_bytes());
    body.extend(utf16);
    typed(b"mluc", &body)
}

/// The profile's bytes.
pub fn srgb_profile() -> Vec<u8> {
    let curve = parametric_curve();
    let tags: [(&[u8; 4], Vec<u8>); 10] = [
        (b"desc", localized(DESCRIPTION)),
        (b"cprt", localized(COPYRIGHT)),
        (b"wtpt", xyz(D50)),
        (b"chad", typed(b"sf32", &xyz_numbers(&D65_TO_D50))),
        (b"rXYZ", xyz(RED)),
        (b"gXYZ", xyz(GREEN)),
        (b"bXYZ", xyz(BLUE)),
        (b"rTRC", curve.clone()),
        (b"gTRC", curve.clone()),
        (b"bTRC", curve),
    ];

    let mut table = (tags.len() as u32).to_be_bytes().to_vec();
    let mut data = Vec::new();
    let data_start = HEADER_LEN + 4 + tags.len() * TAG_ENTRY_LEN;
    for (signature, body) in &tags {
        // Every tag starts on a four-byte boundary.
        data.resize(data.len().next_multiple_of(4), 0);
        table.extend(*signature);
        table.extend(((data_start + data.len()) as u32).to_be_bytes());
        table.extend((body.len() as u32).to_be_bytes());
        data.extend(body);
    }

    let mut header = [0u8; HEADER_LEN];
    let total = HEADER_LEN + table.len() + data.len();
    header[0..4].copy_from_slice(&(total as u32).to_be_bytes());
    header[8..12].copy_from_slice(&VERSION_4_3.to_be_bytes());
    header[12..16].copy_from_slice(b"mntr");
    header[16..20].copy_from_slice(b"RGB ");
    header[20..24].copy_from_slice(b"XYZ ");
    header[36..40].copy_from_slice(b"acsp");
    header[40..44].copy_from_slice(b"APPL");
    header[68..80].copy_from_slice(&xyz_numbers(&D50));
    [header.as_slice(), &table, &data].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(bytes: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    fn tag<'a>(profile: &'a [u8], signature: &[u8; 4]) -> &'a [u8] {
        let count = word(profile, HEADER_LEN) as usize;
        (0..count)
            .map(|i| HEADER_LEN + 4 + i * TAG_ENTRY_LEN)
            .find(|&entry| &profile[entry..entry + 4] == signature)
            .map(|entry| {
                let offset = word(profile, entry + 4) as usize;
                &profile[offset..offset + word(profile, entry + 8) as usize]
            })
            .expect("tag present")
    }

    #[test]
    fn the_srgb_profile_is_a_well_formed_v4_display_profile() {
        let profile = srgb_profile();
        assert_eq!(word(&profile, 0) as usize, profile.len());
        assert_eq!(&profile[36..40], b"acsp");
        assert_eq!(&profile[12..24], b"mntrRGB XYZ ");
        // The illuminant is the spec's exact D50 encoding.
        assert_eq!(
            &profile[68..80],
            &[0, 0, 0xf6, 0xd6, 0, 1, 0, 0, 0, 0, 0xd3, 0x2d]
        );

        let count = word(&profile, HEADER_LEN) as usize;
        for i in 0..count {
            let entry = HEADER_LEN + 4 + i * TAG_ENTRY_LEN;
            let (offset, len) = (word(&profile, entry + 4), word(&profile, entry + 8));
            assert_eq!(offset % 4, 0);
            assert!((offset + len) as usize <= profile.len());
        }
    }

    #[test]
    fn the_srgb_profile_carries_srgbs_primaries_and_curve() {
        let profile = srgb_profile();
        let red = tag(&profile, b"rXYZ");
        assert_eq!(&red[..4], b"XYZ ");
        assert_eq!(word(red, 8) as i32, 0x6fa2);

        let curve = tag(&profile, b"gTRC");
        assert_eq!(&curve[..4], b"para");
        assert_eq!(
            u16::from_be_bytes([curve[8], curve[9]]),
            SRGB_CURVE_FUNCTION
        );
        assert_eq!(word(curve, 12) as i32, 0x26666);

        let description = tag(&profile, b"desc");
        let text: Vec<u16> = description[28..]
            .chunks(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(String::from_utf16(&text).unwrap(), DESCRIPTION);
    }
}
