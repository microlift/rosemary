//! H.264 samples as FFmpeg's parser takes them.
//!
//! A sample holds an access unit the MP4 way (`avcC`): each NAL unit behind a
//! big-endian length of 1, 2 or 4 bytes, and the parameter sets kept apart.
//! The hardware decoders are fed Annex B: each NAL unit behind a start code,
//! the parameter sets in the stream. A NAL unit's bytes never hold a start
//! code (emulation prevention sees to it), so swapping one framing for the
//! other is exact.

use crate::Error;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// `nal_unit_type` of a sequence parameter set.
pub(super) const NAL_SPS: u8 = 7;
const NAL_TYPE_MASK: u8 = 0x1f;

/// The profiles whose SPS carries `chroma_format_idc` and the bit depths
/// (H.264 7.3.2.1.1).
const PROFILES_WITH_FORMAT: [u8; 13] =
    [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];
/// `chroma_format_idc` 3, which is followed by `separate_colour_plane_flag`.
const CHROMA_444: u32 = 3;

/// Append `sample`'s NAL units to `out` in Annex B form.
fn avcc_to_annex_b(sample: &[u8], length_size: usize, out: &mut Vec<u8>) -> Result<(), Error> {
    let bad = |why: &str| Error::InvalidInput(format!("the sample {why}"));
    if sample.is_empty() {
        return Err(bad("is empty"));
    }
    let mut rest = sample;
    while !rest.is_empty() {
        let (length, after) = rest
            .split_at_checked(length_size)
            .ok_or_else(|| bad("ends inside a NAL unit length"))?;
        let length = length
            .iter()
            .fold(0usize, |n, &byte| n << 8 | usize::from(byte));
        let (unit, after) = after
            .split_at_checked(length)
            .ok_or_else(|| bad("ends inside a NAL unit"))?;
        rest = after;
        // Some muxers pad with empty units; they carry nothing.
        if unit.is_empty() {
            continue;
        }
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(unit);
    }
    Ok(())
}

/// The NAL units of an Annex B stream, without their start codes.
pub fn split_annex_b(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut at = 0;
    while at + 3 <= stream.len() {
        if stream[at..at + 3] == [0, 0, 1] {
            starts.push(at + 3);
            at += 3;
        } else {
            at += 1;
        }
    }
    let mut units = Vec::with_capacity(starts.len());
    for (i, &start) in starts.iter().enumerate() {
        let mut end = starts.get(i + 1).map_or(stream.len(), |&next| next - 3);
        // The zero before a four-byte start code belongs to it, not to the
        // unit before.
        while end > start && stream[end - 1] == 0 && starts.get(i + 1).is_some() {
            end -= 1;
        }
        units.push(&stream[start..end]);
    }
    units
}

/// A session's samples turned into access units, with the parameter sets put
/// in front of the first one the decoder takes.
pub(super) struct Converter {
    length_size: usize,
    parameter_sets: Vec<Vec<u8>>,
    needs_parameter_sets: bool,
}

impl Converter {
    pub fn new(length_size: u8, parameter_sets: &[&[u8]]) -> Converter {
        Converter {
            length_size: usize::from(length_size),
            parameter_sets: parameter_sets.iter().map(|s| s.to_vec()).collect(),
            needs_parameter_sets: true,
        }
    }

    pub fn convert(&self, sample: &[u8]) -> Result<Vec<u8>, Error> {
        let mut annex_b = Vec::with_capacity(sample.len() + 64);
        if self.needs_parameter_sets {
            for set in &self.parameter_sets {
                annex_b.extend_from_slice(&START_CODE);
                annex_b.extend_from_slice(set);
            }
        }
        avcc_to_annex_b(sample, self.length_size, &mut annex_b)?;
        Ok(annex_b)
    }

    /// The decoder took a converted access unit, and with the first one the
    /// parameter sets.
    pub fn sent(&mut self) {
        self.needs_parameter_sets = false;
    }
}

/// What a decoder needs to know of an SPS before it is asked to decode it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SpsFormat {
    pub profile: u8,
    pub chroma_format: u32,
    /// The larger of the luma and chroma bit depths.
    pub bit_depth: u32,
}

/// Exp-Golomb reader over an RBSP.
struct Bits {
    data: Vec<u8>,
    pos: usize,
}

impl Bits {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(u32::from(bit))
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let mut value = 0u32;
        for _ in 0..zeros {
            value = value << 1 | self.bit()?;
        }
        Some((1u32 << zeros) - 1 + value)
    }
}

/// The profile, chroma format and bit depth of the SPS NAL unit `nal`.
pub(super) fn sps_format(nal: &[u8]) -> Option<SpsFormat> {
    let (&header, payload) = nal.split_first()?;
    if header & NAL_TYPE_MASK != NAL_SPS {
        return None;
    }
    // Drop emulation-prevention bytes: 00 00 03 is 00 00 in the RBSP.
    let mut data = Vec::with_capacity(payload.len());
    let mut zeros = 0;
    for &byte in payload {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        data.push(byte);
    }
    let profile = *data.first()?;
    // profile_idc, the constraint flags and level_idc.
    let mut bits = Bits { data, pos: 24 };
    bits.ue()?;
    if !PROFILES_WITH_FORMAT.contains(&profile) {
        return Some(SpsFormat {
            profile,
            chroma_format: 1,
            bit_depth: 8,
        });
    }
    let chroma_format = bits.ue()?;
    if chroma_format == CHROMA_444 {
        bits.bit()?;
    }
    let luma = bits.ue()?;
    let chroma = bits.ue()?;
    Some(SpsFormat {
        profile,
        chroma_format,
        bit_depth: 8 + luma.max(chroma),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLICE: &[u8] = &[0x41, 0x9a, 0x26];
    const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x01];

    fn avcc(units: &[&[u8]], length_size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for unit in units {
            let length = unit.len().to_be_bytes();
            out.extend_from_slice(&length[length.len() - length_size..]);
            out.extend_from_slice(unit);
        }
        out
    }

    fn annex_b(units: &[&[u8]]) -> Vec<u8> {
        units
            .iter()
            .flat_map(|u| [&START_CODE[..], u].concat())
            .collect()
    }

    #[test]
    fn avcc_with_length_sizes_1_2_4_becomes_annex_b() {
        for length_size in [1, 2, 4] {
            let converter = Converter::new(length_size as u8, &[]);
            let sample = avcc(&[SLICE, IDR], length_size);
            assert_eq!(
                converter.convert(&sample).unwrap(),
                annex_b(&[SLICE, IDR]),
                "length size {length_size}"
            );
        }
    }

    #[test]
    fn truncated_sample_is_invalid_input_and_keeps_the_parameter_sets() {
        let sample = avcc(&[SLICE, IDR], 4);
        let converter = Converter::new(4, &[&[0x67, 1], &[0x68, 2]]);
        for cut in [0, 1, 3, 5, sample.len() - 1] {
            assert!(
                matches!(
                    converter.convert(&sample[..cut]),
                    Err(Error::InvalidInput(_))
                ),
                "cut at {cut}"
            );
        }
        assert!(
            converter
                .convert(&sample)
                .unwrap()
                .starts_with(&[0, 0, 0, 1, 0x67])
        );
    }

    #[test]
    fn parameter_sets_lead_until_a_unit_is_sent() {
        let (sps, pps): (&[u8], &[u8]) = (&[0x67, 0x64, 0x00], &[0x68, 0xea]);
        let mut converter = Converter::new(4, &[sps, pps]);
        let sample = avcc(&[IDR], 4);
        assert_eq!(
            converter.convert(&sample).unwrap(),
            annex_b(&[sps, pps, IDR])
        );
        assert_eq!(
            converter.convert(&sample).unwrap(),
            annex_b(&[sps, pps, IDR])
        );
        converter.sent();
        assert_eq!(converter.convert(&sample).unwrap(), annex_b(&[IDR]));
    }

    #[test]
    fn the_probe_clip_splits_into_its_eight_units() {
        let units = split_annex_b(super::super::PROBE_CLIP);
        let sizes: Vec<usize> = units.iter().map(|u| u.len()).collect();
        assert_eq!(sizes, [24, 6, 51, 13, 7, 46, 21, 7]);
        let types: Vec<u8> = units.iter().map(|u| u[0] & NAL_TYPE_MASK).collect();
        assert_eq!(types, [7, 8, 5, 1, 1, 1, 1, 1]);
    }

    #[test]
    fn sps_format_reads_high_profile_fields() {
        let units = split_annex_b(super::super::PROBE_CLIP);
        assert_eq!(
            sps_format(units[0]),
            Some(SpsFormat {
                profile: 100,
                chroma_format: 1,
                bit_depth: 8,
            })
        );
        // Baseline, level 3.0, seq_parameter_set_id 0: no format fields.
        assert_eq!(
            sps_format(&[0x67, 66, 0xc0, 30, 0x80]),
            Some(SpsFormat {
                profile: 66,
                chroma_format: 1,
                bit_depth: 8,
            })
        );
        // High 10 4:2:2: chroma_format_idc 2, bit depths 2 and 2.
        // ue(0)=1, ue(2)=011, ue(2)=011, ue(2)=011 -> 1011 0110 11xx xxxx.
        assert_eq!(
            sps_format(&[0x67, 122, 0, 40, 0b1011_0110, 0b1110_0000]),
            Some(SpsFormat {
                profile: 122,
                chroma_format: 2,
                bit_depth: 10,
            })
        );
        assert_eq!(sps_format(IDR), None);
    }
}
