//! The packed word AIR gives a `constexpr sampler` (`__air_sampler_state`).
//!
//! Apple does not document the layout. These fields agree with the words
//! Apple's compiler emits for known sampler descriptions.

use super::*;
use crate::ir::InlineSampler;

/// Set in every one-word sampler state.
const STATE_MARKER: u64 = 1 << 63;

/// The `InlineSampler` a one-word AIR sampler state describes.
pub(super) fn decode(word: u64) -> Result<InlineSampler, TranslateError> {
    let field = |shift: u32, bits: u32| (word >> shift) & ((1 << bits) - 1);
    let unsupported = |what: &str, value: u64| {
        semantic(format!(
            "AIR sampler state {word:#x} has an unsupported {what} {value}"
        ))
    };
    if word & STATE_MARKER == 0 {
        return Err(unsupported("marker", 0));
    }
    // `MTLSamplerBorderColor` numbers its colors as AIR does.
    let border_color = match field(56, 2) {
        color @ 0..=2 => color as u8,
        color => return Err(unsupported("border color", color)),
    };
    let address = |shift| match field(shift, 3) {
        // AIR has one code for both; transparent black makes them the same.
        0 if border_color == 0 => Ok(MTL_SAMPLER_ADDRESS_MODE_CLAMP_TO_ZERO),
        0 => Ok(MTL_SAMPLER_ADDRESS_MODE_CLAMP_TO_BORDER_COLOR),
        1 => Ok(MTL_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE),
        2 => Ok(MTL_SAMPLER_ADDRESS_MODE_REPEAT),
        3 => Ok(MTL_SAMPLER_ADDRESS_MODE_MIRROR_REPEAT),
        mode => Err(unsupported("address mode", mode)),
    };
    // Nearest and linear; AIR's bicubic has no `MTLSamplerMinMagFilter`.
    let filter = |shift| match field(shift, 2) {
        filter @ 0..=1 => Ok(filter as u8),
        filter => Err(unsupported("filter", filter)),
    };
    // `MTLSamplerMipFilter` numbers its filters as AIR does.
    let mip_filter = match field(13, 2) {
        filter @ 0..=2 => filter as u8,
        filter => return Err(unsupported("mip filter", filter)),
    };
    let compare_function = match field(16, 4) {
        // No comparison, and `never`, which is what MSL leaves unset.
        AIR_COMPARE_NONE | AIR_COMPARE_NEVER => MTL_COMPARE_FUNCTION_NEVER,
        1 => MTL_COMPARE_FUNCTION_LESS,
        2 => MTL_COMPARE_FUNCTION_LESS_EQUAL,
        3 => MTL_COMPARE_FUNCTION_GREATER,
        4 => MTL_COMPARE_FUNCTION_GREATER_EQUAL,
        5 => MTL_COMPARE_FUNCTION_EQUAL,
        6 => MTL_COMPARE_FUNCTION_NOT_EQUAL,
        7 => MTL_COMPARE_FUNCTION_ALWAYS,
        function => return Err(unsupported("compare function", function)),
    };
    // Weighted average; minimum and maximum reductions have no field here.
    if field(58, 2) != 0 {
        return Err(unsupported("reduction", field(58, 2)));
    }
    let lod = |shift| {
        half::f16::from_bits(field(shift, 16) as u16)
            .to_f32()
            .to_bits()
    };
    Ok(InlineSampler {
        min_filter: filter(11)?,
        mag_filter: filter(9)?,
        mip_filter,
        address: [address(0)?, address(3)?, address(6)?],
        max_anisotropy: field(20, 4) as u8 + 1,
        compare_function,
        border_color,
        normalized_coordinates: field(15, 1) == 0,
        lod_min_clamp: lod(24),
        lod_max_clamp: lod(40),
    })
}

const AIR_COMPARE_NONE: u64 = 0;
const AIR_COMPARE_NEVER: u64 = 8;

const MTL_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE: u8 = 0;
const MTL_SAMPLER_ADDRESS_MODE_REPEAT: u8 = 2;
const MTL_SAMPLER_ADDRESS_MODE_MIRROR_REPEAT: u8 = 3;
const MTL_SAMPLER_ADDRESS_MODE_CLAMP_TO_ZERO: u8 = 4;
const MTL_SAMPLER_ADDRESS_MODE_CLAMP_TO_BORDER_COLOR: u8 = 5;

const MTL_COMPARE_FUNCTION_NEVER: u8 = 0;
const MTL_COMPARE_FUNCTION_LESS: u8 = 1;
const MTL_COMPARE_FUNCTION_EQUAL: u8 = 2;
const MTL_COMPARE_FUNCTION_LESS_EQUAL: u8 = 3;
const MTL_COMPARE_FUNCTION_GREATER: u8 = 4;
const MTL_COMPARE_FUNCTION_NOT_EQUAL: u8 = 5;
const MTL_COMPARE_FUNCTION_GREATER_EQUAL: u8 = 6;
const MTL_COMPARE_FUNCTION_ALWAYS: u8 = 7;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_linear_clamp_to_edge_sampler() {
        // `constexpr sampler(filter::linear, address::clamp_to_edge)`.
        let sampler = decode(0x807b_ff00_0008_0a49).unwrap();
        assert_eq!(
            sampler,
            InlineSampler {
                min_filter: 1,
                mag_filter: 1,
                address: [MTL_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE; 3],
                lod_max_clamp: 65504f32.to_bits(),
                ..InlineSampler::default()
            }
        );
    }

    #[test]
    fn rejects_a_word_without_the_state_marker() {
        assert!(decode(0x007b_ff00_0008_0a49).is_err());
    }
}
