//! What a format description needs out of H.264 parameter sets: the picture
//! size and the VUI fields CoreMedia publishes as extensions, and the `avcC`
//! record built from them.

/// NAL unit types (H.264 table 7-1).
pub const SPS: u8 = 7;
pub const PPS: u8 = 8;

/// The fields of a sequence parameter set this crate reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sps {
    pub profile: u8,
    pub compatibility: u8,
    pub level: u8,
    pub chroma_format: u8,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    /// Whether the SPS carries the three fields above, which decides whether
    /// the `avcC` record has its high-profile tail.
    pub high: bool,
    /// The cropped picture, which is what CoreMedia reports.
    pub width: u32,
    pub height: u32,
    pub sample_aspect: Option<(u16, u16)>,
    pub full_range: Option<bool>,
    /// `colour_primaries`, `transfer_characteristics`, `matrix_coefficients`.
    pub colour: Option<(u8, u8, u8)>,
}

/// H.264 table E-1, for `aspect_ratio_idc` 1 through 16.
const SAMPLE_ASPECTS: [(u16, u16); 16] = [
    (1, 1),
    (12, 11),
    (10, 11),
    (16, 11),
    (40, 33),
    (24, 11),
    (20, 11),
    (32, 11),
    (80, 33),
    (18, 11),
    (15, 11),
    (64, 33),
    (160, 99),
    (4, 3),
    (3, 2),
    (2, 1),
];
const EXTENDED_SAR: u32 = 255;

/// Exp-Golomb bit reader over an RBSP.
struct Bits {
    data: Vec<u8>,
    pos: usize,
}

impl Bits {
    /// The payload of `nal` after its header, with emulation-prevention bytes
    /// removed.
    fn new(nal: &[u8]) -> Self {
        let mut data = Vec::with_capacity(nal.len());
        let mut zeroes = 0;
        for &byte in nal.get(1..).unwrap_or_default() {
            if zeroes == 2 && byte == 3 {
                zeroes = 0;
                continue;
            }
            data.push(byte);
            zeroes = if byte == 0 { zeroes + 1 } else { 0 };
        }
        Bits { data, pos: 0 }
    }

    fn bits(&mut self, width: u32) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..width {
            let byte = *self.data.get(self.pos / 8)?;
            value = (value << 1) | u32::from(byte >> (7 - self.pos % 8) & 1);
            self.pos += 1;
        }
        Some(value)
    }

    fn flag(&mut self) -> Option<bool> {
        self.bits(1).map(|b| b != 0)
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeroes = 0;
        while !self.flag()? {
            zeroes += 1;
            if zeroes > 31 {
                return None;
            }
        }
        let rest = u64::from(self.bits(zeroes)?);
        u32::try_from((1u64 << zeroes) - 1 + rest).ok()
    }

    fn se(&mut self) -> Option<i32> {
        let k = i64::from(self.ue()?);
        let magnitude = (k + 1) / 2;
        i32::try_from(if k % 2 == 1 { magnitude } else { -magnitude }).ok()
    }
}

/// Profiles whose SPS carries chroma format and bit depths (H.264 7.3.2.1.1);
/// 144 is the withdrawn High 4:4:4, which encoders still emit.
fn is_high(profile: u8) -> bool {
    matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135 | 144
    )
}

/// Skip one `scaling_list` (H.264 7.3.2.1.1.1); only its length matters here.
fn skip_scaling_list(bits: &mut Bits, size: usize) -> Option<()> {
    let (mut last, mut next) = (8i32, 8i32);
    for _ in 0..size {
        if next != 0 {
            let delta = bits.se()?;
            if !(-128..=127).contains(&delta) {
                return None;
            }
            next = (last + delta + 256) % 256;
        }
        if next != 0 {
            last = next;
        }
    }
    Some(())
}

/// Parse `nal`, a whole SPS NAL unit including its header byte.
pub fn parse_sps(nal: &[u8]) -> Option<Sps> {
    if nal.len() < 4 || nal[0] & 0x80 != 0 || nal[0] & 0x1f != SPS {
        return None;
    }
    let mut bits = Bits::new(nal);
    let mut sps = Sps {
        profile: bits.bits(8)? as u8,
        compatibility: bits.bits(8)? as u8,
        level: bits.bits(8)? as u8,
        chroma_format: 1,
        ..Sps::default()
    };
    if bits.ue()? > 31 {
        return None;
    }
    let mut separate_planes = false;
    if is_high(sps.profile) {
        sps.high = true;
        let chroma_format = bits.ue()?;
        if chroma_format > 3 {
            return None;
        }
        sps.chroma_format = chroma_format as u8;
        if chroma_format == 3 {
            separate_planes = bits.flag()?;
        }
        let (luma, chroma) = (bits.ue()?, bits.ue()?);
        if luma > 6 || chroma > 6 {
            return None;
        }
        sps.bit_depth_luma_minus8 = luma as u8;
        sps.bit_depth_chroma_minus8 = chroma as u8;
        bits.flag()?; // qpprime_y_zero_transform_bypass_flag
        if bits.flag()? {
            let lists = if chroma_format == 3 { 12 } else { 8 };
            for i in 0..lists {
                if bits.flag()? {
                    skip_scaling_list(&mut bits, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    if bits.ue()? > 12 {
        return None; // log2_max_frame_num_minus4
    }
    match bits.ue()? {
        0 => {
            if bits.ue()? > 12 {
                return None;
            }
        }
        1 => {
            bits.flag()?;
            bits.se()?;
            bits.se()?;
            let cycle = bits.ue()?;
            if cycle > 255 {
                return None;
            }
            for _ in 0..cycle {
                bits.se()?;
            }
        }
        2 => {}
        _ => return None,
    }
    bits.ue()?; // max_num_ref_frames
    bits.flag()?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = u64::from(bits.ue()?) + 1;
    let height_units = u64::from(bits.ue()?) + 1;
    let frame_mbs_only = bits.flag()?;
    if !frame_mbs_only {
        bits.flag()?; // mb_adaptive_frame_field_flag
    }
    bits.flag()?; // direct_8x8_inference_flag
    let crop = match bits.flag()? {
        true => [bits.ue()?, bits.ue()?, bits.ue()?, bits.ue()?].map(u64::from),
        false => [0; 4],
    };

    // H.264 7.4.2.1.1: crop offsets count in chroma samples, and in frame
    // pairs of lines for field coding.
    let chroma_array = if separate_planes {
        0
    } else {
        sps.chroma_format
    };
    let (sub_width, sub_height) = match chroma_array {
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    };
    let field_factor = if frame_mbs_only { 1 } else { 2 };
    let coded_width = width_mbs * 16;
    let coded_height = height_units * 16 * field_factor;
    let crop_x = (crop[0] + crop[1]).checked_mul(sub_width)?;
    let crop_y = (crop[2] + crop[3]).checked_mul(sub_height * field_factor)?;
    sps.width = u32::try_from(coded_width.checked_sub(crop_x)?).ok()?;
    sps.height = u32::try_from(coded_height.checked_sub(crop_y)?).ok()?;
    if sps.width == 0 || sps.height == 0 || sps.width > i32::MAX as u32 {
        return None;
    }

    // A truncated VUI loses only the extensions it would have carried.
    if bits.flag() == Some(true) {
        parse_vui(&mut bits, &mut sps);
    }
    Some(sps)
}

fn parse_vui(bits: &mut Bits, sps: &mut Sps) -> Option<()> {
    if bits.flag()? {
        let sar = match bits.bits(8)? {
            EXTENDED_SAR => (bits.bits(16)? as u16, bits.bits(16)? as u16),
            idc @ 1..=16 => SAMPLE_ASPECTS[idc as usize - 1],
            _ => (0, 0),
        };
        if sar.0 != 0 && sar.1 != 0 {
            sps.sample_aspect = Some(sar);
        }
    }
    if bits.flag()? {
        bits.flag()?; // overscan_appropriate_flag
    }
    if bits.flag()? {
        bits.bits(3)?; // video_format
        sps.full_range = Some(bits.flag()?);
        if bits.flag()? {
            sps.colour = Some((
                bits.bits(8)? as u8,
                bits.bits(8)? as u8,
                bits.bits(8)? as u8,
            ));
        }
    }
    Some(())
}

/// The `AVCDecoderConfigurationRecord` (ISO/IEC 14496-15 5.3.3.1) for these
/// sets, the `avcC` atom CoreMedia stores among the extensions.
///
/// Every set must fit a 16-bit length and the counts their fields, which the
/// caller checks.
pub fn avcc(sps: &Sps, sequence: &[Vec<u8>], picture: &[Vec<u8>], nal_length: u8) -> Vec<u8> {
    let mut out = vec![
        1,
        sps.profile,
        sps.compatibility,
        sps.level,
        0xfc | (nal_length - 1),
        0xe0 | sequence.len() as u8,
    ];
    for set in sequence {
        out.extend_from_slice(&(set.len() as u16).to_be_bytes());
        out.extend_from_slice(set);
    }
    out.push(picture.len() as u8);
    for set in picture {
        out.extend_from_slice(&(set.len() as u16).to_be_bytes());
        out.extend_from_slice(set);
    }
    if sps.high {
        out.extend_from_slice(&[
            0xfc | sps.chroma_format,
            0xf8 | sps.bit_depth_luma_minus8,
            0xf8 | sps.bit_depth_chroma_minus8,
            0, // numOfSequenceParameterSetExt
        ]);
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// x264's 1920x1080 High-profile SPS: 120x68 macroblocks cropped by
    /// eight lines.
    pub const SPS_1080P_HIGH: &[u8] = &[
        0x67, 0x64, 0x00, 0x28, 0xac, 0xd9, 0x40, 0x78, 0x02, 0x27, 0xe5, 0xc0, 0x44, 0x00, 0x00,
        0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0x08, 0x3c, 0x60, 0xc6, 0x58,
    ];
    pub const PPS_1080P_HIGH: &[u8] = &[0x68, 0xeb, 0xe3, 0xcb, 0x22, 0xc0];
    /// x264's 320x240 Baseline SPS.
    pub const SPS_240P_BASELINE: &[u8] = &[
        0x67, 0x42, 0xc0, 0x0b, 0xd9, 0x01, 0x41, 0xfb, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10,
        0x00, 0x00, 0x03, 0x00, 0x20, 0xf1, 0x42, 0xa4, 0x80,
    ];
    /// x264's 1280x720 full-range SPS, with BT.709 matrix coefficients.
    pub const SPS_720P_FULL_RANGE: &[u8] = &[
        0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9, 0x40, 0x50, 0x05, 0xbb, 0x01, 0x6e, 0x04, 0x04, 0x02,
        0x80, 0x00, 0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x03, 0x01, 0x07, 0x8c, 0x18, 0xcb,
    ];
    /// mediabackend's anamorphic fixture: 64x64 with a 4:3 sample aspect.
    pub const SPS_ANAMORPHIC: &[u8] = &[
        0x67, 0x42, 0xc0, 0x0a, 0xda, 0x10, 0x9b, 0x0e, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00,
        0x00, 0x03, 0x01, 0x40, 0xf1, 0x22, 0x6a,
    ];
    /// mediabackend's interlaced fixture: field-coded 64x64 Main profile.
    pub const SPS_INTERLACED: &[u8] = &[
        0x67, 0x4d, 0x40, 0x15, 0xf4, 0x22, 0x6c, 0x04, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40, 0x00,
        0x00, 0x05, 0x07, 0xc5, 0x0a, 0xa8,
    ];

    #[test]
    fn reads_the_cropped_size_of_a_high_profile_sps() {
        let sps = parse_sps(SPS_1080P_HIGH).expect("parses");
        assert_eq!((sps.width, sps.height), (1920, 1080));
        assert_eq!((sps.profile, sps.level), (100, 40));
        assert!(sps.high);
        assert_eq!(sps.chroma_format, 1);
        assert_eq!(sps.sample_aspect, Some((1, 1)));
        assert_eq!(sps.full_range, None);
    }

    #[test]
    fn reads_a_baseline_sps() {
        let sps = parse_sps(SPS_240P_BASELINE).expect("parses");
        assert_eq!((sps.width, sps.height), (320, 240));
        assert!(!sps.high);
    }

    #[test]
    fn reads_range_and_colour_from_the_vui() {
        let sps = parse_sps(SPS_720P_FULL_RANGE).expect("parses");
        assert_eq!((sps.width, sps.height), (1280, 720));
        assert_eq!(sps.full_range, Some(true));
        assert_eq!(sps.colour.map(|c| c.2), Some(1));
    }

    #[test]
    fn reads_an_anamorphic_sample_aspect() {
        let sps = parse_sps(SPS_ANAMORPHIC).expect("parses");
        assert_eq!((sps.width, sps.height), (64, 64));
        assert_eq!(sps.sample_aspect, Some((4, 3)));
    }

    #[test]
    fn counts_field_coded_height_in_frame_lines() {
        let sps = parse_sps(SPS_INTERLACED).expect("parses");
        assert_eq!((sps.width, sps.height), (64, 64));
    }

    #[test]
    fn rejects_what_is_not_an_sps() {
        assert_eq!(parse_sps(PPS_1080P_HIGH), None);
        assert_eq!(parse_sps(&SPS_240P_BASELINE[..5]), None);
        assert_eq!(parse_sps(&[]), None);
    }

    #[test]
    fn strips_emulation_prevention_bytes() {
        let bits = Bits::new(&[0x67, 0x00, 0x00, 0x03, 0x01]);
        assert_eq!(bits.data, [0, 0, 1]);
    }

    #[test]
    fn builds_the_avcc_record_with_its_high_profile_tail() {
        let sps = parse_sps(SPS_1080P_HIGH).unwrap();
        let record = avcc(
            &sps,
            &[SPS_1080P_HIGH.to_vec()],
            &[PPS_1080P_HIGH.to_vec()],
            4,
        );
        assert_eq!(&record[..6], &[1, 0x64, 0x00, 0x28, 0xff, 0xe1]);
        let sps_len = SPS_1080P_HIGH.len();
        assert_eq!(&record[6..8], &(sps_len as u16).to_be_bytes());
        assert_eq!(record[8 + sps_len], 1);
        assert_eq!(&record[record.len() - 4..], &[0xfd, 0xf8, 0xf8, 0]);
    }
}
