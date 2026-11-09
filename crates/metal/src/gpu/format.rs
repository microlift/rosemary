use super::*;
use crate::constants::{MTL_PIXEL_FORMAT_BGRA8_UNORM, MTL_PIXEL_FORMAT_RGBA8_UNORM};

pub(crate) fn sample_count(count: u64) -> Option<vk::SampleCountFlags> {
    match count {
        1 => Some(vk::SampleCountFlags::TYPE_1),
        2 => Some(vk::SampleCountFlags::TYPE_2),
        4 => Some(vk::SampleCountFlags::TYPE_4),
        8 => Some(vk::SampleCountFlags::TYPE_8),
        _ => None,
    }
}

pub fn aspect_of(format: vk::Format) -> vk::ImageAspectFlags {
    match format {
        vk::Format::D16_UNORM | vk::Format::D32_SFLOAT | vk::Format::X8_D24_UNORM_PACK32 => {
            vk::ImageAspectFlags::DEPTH
        }
        vk::Format::D24_UNORM_S8_UINT
        | vk::Format::D32_SFLOAT_S8_UINT
        | vk::Format::D16_UNORM_S8_UINT => {
            vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL
        }
        vk::Format::S8_UINT => vk::ImageAspectFlags::STENCIL,
        _ => vk::ImageAspectFlags::COLOR,
    }
}

/// The one aspect a shader reads through a view of `MTLPixelFormat` `metal`.
///
/// Vulkan forbids a sampled or storage view of both halves of a depth/stencil
/// image, and Metal reads the depth of a combined texture; the stencil half is
/// what an `X32_Stencil8` or `X24_Stencil8` view reads.
pub fn shader_aspect(metal: u64) -> vk::ImageAspectFlags {
    match metal {
        261 | 262 => vk::ImageAspectFlags::STENCIL,
        _ => {
            let aspect = aspect_of(pixel_format(metal));
            if aspect.contains(vk::ImageAspectFlags::DEPTH) {
                vk::ImageAspectFlags::DEPTH
            } else {
                aspect
            }
        }
    }
}

/// The format a texture of `format` may also be viewed as without asking
/// for `MTLTextureUsagePixelFormatView`: its sRGB or linear twin.
pub fn srgb_twin(format: vk::Format) -> Option<vk::Format> {
    use vk::Format as F;
    let pairs = [
        (F::R8G8B8A8_UNORM, F::R8G8B8A8_SRGB),
        (F::B8G8R8A8_UNORM, F::B8G8R8A8_SRGB),
        (F::BC1_RGBA_UNORM_BLOCK, F::BC1_RGBA_SRGB_BLOCK),
        (F::BC2_UNORM_BLOCK, F::BC2_SRGB_BLOCK),
        (F::BC3_UNORM_BLOCK, F::BC3_SRGB_BLOCK),
        (F::BC7_UNORM_BLOCK, F::BC7_SRGB_BLOCK),
        (F::R8_UNORM, F::R8_SRGB),
        (F::R8G8_UNORM, F::R8G8_SRGB),
    ];
    pairs.iter().find_map(|&(linear, srgb)| match format {
        f if f == linear => Some(srgb),
        f if f == srgb => Some(linear),
        _ => None,
    })
}

pub fn is_srgb(format: vk::Format) -> bool {
    use vk::Format as F;
    matches!(
        format,
        F::R8G8B8A8_SRGB
            | F::B8G8R8A8_SRGB
            | F::BC1_RGBA_SRGB_BLOCK
            | F::BC2_SRGB_BLOCK
            | F::BC3_SRGB_BLOCK
            | F::BC7_SRGB_BLOCK
            | F::R8_SRGB
            | F::R8G8_SRGB
    )
}

/// Whether a colour format holds integers, which a resolve cannot average.
pub fn is_integer(format: vk::Format) -> bool {
    use vk::Format as F;
    matches!(
        format,
        F::R8_UINT
            | F::R8_SINT
            | F::R16_UINT
            | F::R16_SINT
            | F::R8G8_UINT
            | F::R8G8_SINT
            | F::R32_UINT
            | F::R32_SINT
            | F::R16G16_UINT
            | F::R16G16_SINT
            | F::R8G8B8A8_UINT
            | F::R8G8B8A8_SINT
            | F::A2B10G10R10_UINT_PACK32
            | F::R32G32_UINT
            | F::R32G32_SINT
            | F::R16G16B16A16_UINT
            | F::R16G16B16A16_SINT
            | F::R32G32B32A32_UINT
            | F::R32G32B32A32_SINT
    )
}

/// The view swizzle that makes `pixel_format(metal)` read as Metal's format.
///
/// `A8Unorm` is stored as `R8_UNORM`, and Metal samples it as `(0, 0, 0, a)`:
/// a text shader reading `.a` from a glyph atlas otherwise sees 1 everywhere.
pub fn pixel_format_components(metal: u64) -> vk::ComponentMapping {
    use vk::ComponentSwizzle as S;
    match metal {
        1 => vk::ComponentMapping {
            r: S::ZERO,
            g: S::ZERO,
            b: S::ZERO,
            a: S::R,
        },
        _ => vk::ComponentMapping::default(),
    }
}

/// `MTLPixelFormat` to `VkFormat`.
///
/// Metal names a texel's components in memory order and Vulkan names them in
/// the same order, so most formats map one to one. A format this table does
/// not know returns `UNDEFINED` and the caller reports it rather than
/// substituting a different layout.
pub fn pixel_format(metal: u64) -> vk::Format {
    use vk::Format as F;
    match metal {
        1 => F::R8_UNORM, // A8Unorm: one 8-bit channel; swizzle is the view's
        10 => F::R8_UNORM,
        11 => F::R8_SRGB,
        12 => F::R8_SNORM,
        13 => F::R8_UINT,
        14 => F::R8_SINT,
        20 => F::R16_UNORM,
        22 => F::R16_SNORM,
        23 => F::R16_UINT,
        24 => F::R16_SINT,
        25 => F::R16_SFLOAT,
        30 => F::R8G8_UNORM,
        31 => F::R8G8_SRGB,
        32 => F::R8G8_SNORM,
        33 => F::R8G8_UINT,
        34 => F::R8G8_SINT,
        // Metal names a packed format's components from the low bit and
        // Vulkan from the high bit, so these read reversed.
        40 => F::R5G6B5_UNORM_PACK16,
        41 => F::R5G5B5A1_UNORM_PACK16,
        42 => F::R4G4B4A4_UNORM_PACK16,
        43 => F::A1R5G5B5_UNORM_PACK16,
        53 => F::R32_UINT,
        54 => F::R32_SINT,
        55 => F::R32_SFLOAT,
        60 => F::R16G16_UNORM,
        62 => F::R16G16_SNORM,
        63 => F::R16G16_UINT,
        64 => F::R16G16_SINT,
        65 => F::R16G16_SFLOAT,
        MTL_PIXEL_FORMAT_RGBA8_UNORM => F::R8G8B8A8_UNORM,
        71 => F::R8G8B8A8_SRGB,
        72 => F::R8G8B8A8_SNORM,
        73 => F::R8G8B8A8_UINT,
        74 => F::R8G8B8A8_SINT,
        MTL_PIXEL_FORMAT_BGRA8_UNORM => F::B8G8R8A8_UNORM,
        81 => F::B8G8R8A8_SRGB,
        90 => F::A2B10G10R10_UNORM_PACK32,
        91 => F::A2B10G10R10_UINT_PACK32,
        92 => F::B10G11R11_UFLOAT_PACK32,
        93 => F::E5B9G9R9_UFLOAT_PACK32,
        94 => F::A2R10G10B10_UNORM_PACK32,
        103 => F::R32G32_UINT,
        104 => F::R32G32_SINT,
        105 => F::R32G32_SFLOAT,
        110 => F::R16G16B16A16_UNORM,
        112 => F::R16G16B16A16_SNORM,
        113 => F::R16G16B16A16_UINT,
        114 => F::R16G16B16A16_SINT,
        115 => F::R16G16B16A16_SFLOAT,
        123 => F::R32G32B32A32_UINT,
        124 => F::R32G32B32A32_SINT,
        125 => F::R32G32B32A32_SFLOAT,
        130 => F::BC1_RGBA_UNORM_BLOCK,
        131 => F::BC1_RGBA_SRGB_BLOCK,
        132 => F::BC2_UNORM_BLOCK,
        133 => F::BC2_SRGB_BLOCK,
        134 => F::BC3_UNORM_BLOCK,
        135 => F::BC3_SRGB_BLOCK,
        140 => F::BC4_UNORM_BLOCK,
        141 => F::BC4_SNORM_BLOCK,
        142 => F::BC5_UNORM_BLOCK,
        143 => F::BC5_SNORM_BLOCK,
        150 => F::BC6H_SFLOAT_BLOCK,
        151 => F::BC6H_UFLOAT_BLOCK,
        152 => F::BC7_UNORM_BLOCK,
        153 => F::BC7_SRGB_BLOCK,
        250 => F::D16_UNORM,
        252 => F::D32_SFLOAT,
        253 => F::S8_UINT,
        // Metal's Depth24Unorm_Stencil8 has no exact portable Vulkan format on
        // every driver; D24_UNORM_S8_UINT is the same layout where present.
        255 => F::D24_UNORM_S8_UINT,
        260 => F::D32_SFLOAT_S8_UINT,
        // `X32_Stencil8` and `X24_Stencil8` are the stencil half of a combined
        // texture seen on its own. The image carries both halves either way;
        // which one a view reads is the aspect, not the format.
        261 => F::D32_SFLOAT_S8_UINT,
        262 => F::D24_UNORM_S8_UINT,
        _ => F::UNDEFINED,
    }
}

/// The number of bytes one texel of `format` occupies, for buffer/texture
/// copies. Block-compressed formats report the block's size.
pub fn texel_size(format: vk::Format) -> u32 {
    use vk::Format as F;
    match format {
        F::R8_UNORM | F::R8_SRGB | F::R8_SNORM | F::R8_UINT | F::R8_SINT | F::S8_UINT => 1,
        F::R8G8_UNORM
        | F::R8G8_SRGB
        | F::R8G8_SNORM
        | F::R8G8_UINT
        | F::R8G8_SINT
        | F::R16_UNORM
        | F::R16_SNORM
        | F::R16_UINT
        | F::R16_SINT
        | F::R16_SFLOAT
        | F::R5G6B5_UNORM_PACK16
        | F::A1R5G5B5_UNORM_PACK16
        | F::R4G4B4A4_UNORM_PACK16
        | F::R5G5B5A1_UNORM_PACK16
        | F::D16_UNORM => 2,
        F::R8G8B8A8_UNORM
        | F::R8G8B8A8_SRGB
        | F::R8G8B8A8_SNORM
        | F::R8G8B8A8_UINT
        | F::R8G8B8A8_SINT
        | F::B8G8R8A8_UNORM
        | F::B8G8R8A8_SRGB
        | F::A2B10G10R10_UNORM_PACK32
        | F::A2B10G10R10_UINT_PACK32
        | F::A2R10G10B10_UNORM_PACK32
        | F::B10G11R11_UFLOAT_PACK32
        | F::E5B9G9R9_UFLOAT_PACK32
        | F::R32_UINT
        | F::R32_SINT
        | F::R32_SFLOAT
        | F::R16G16_UNORM
        | F::R16G16_SNORM
        | F::R16G16_UINT
        | F::R16G16_SINT
        | F::R16G16_SFLOAT
        | F::D32_SFLOAT
        | F::D24_UNORM_S8_UINT => 4,
        F::D32_SFLOAT_S8_UINT => 5,
        F::R32G32_UINT
        | F::R32G32_SINT
        | F::R32G32_SFLOAT
        | F::R16G16B16A16_UNORM
        | F::R16G16B16A16_SNORM
        | F::R16G16B16A16_UINT
        | F::R16G16B16A16_SINT
        | F::R16G16B16A16_SFLOAT
        | F::BC1_RGBA_UNORM_BLOCK
        | F::BC1_RGBA_SRGB_BLOCK
        | F::BC4_UNORM_BLOCK
        | F::BC4_SNORM_BLOCK => 8,
        F::R32G32B32A32_UINT
        | F::R32G32B32A32_SINT
        | F::R32G32B32A32_SFLOAT
        | F::BC2_UNORM_BLOCK
        | F::BC2_SRGB_BLOCK
        | F::BC3_UNORM_BLOCK
        | F::BC3_SRGB_BLOCK
        | F::BC5_UNORM_BLOCK
        | F::BC5_SNORM_BLOCK
        | F::BC6H_SFLOAT_BLOCK
        | F::BC6H_UFLOAT_BLOCK
        | F::BC7_UNORM_BLOCK
        | F::BC7_SRGB_BLOCK => 16,
        _ => 4,
    }
}

/// Whether `format` is block compressed, and its block extent.
pub fn block_extent(format: vk::Format) -> (u32, u32) {
    use vk::Format as F;
    match format {
        F::BC1_RGBA_UNORM_BLOCK
        | F::BC1_RGBA_SRGB_BLOCK
        | F::BC2_UNORM_BLOCK
        | F::BC2_SRGB_BLOCK
        | F::BC3_UNORM_BLOCK
        | F::BC3_SRGB_BLOCK
        | F::BC4_UNORM_BLOCK
        | F::BC4_SNORM_BLOCK
        | F::BC5_UNORM_BLOCK
        | F::BC5_SNORM_BLOCK
        | F::BC6H_SFLOAT_BLOCK
        | F::BC6H_UFLOAT_BLOCK
        | F::BC7_UNORM_BLOCK
        | F::BC7_SRGB_BLOCK => (4, 4),
        _ => (1, 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_alpha_only_texture_reads_its_channel_as_alpha() {
        let a8 = pixel_format_components(1);
        assert_eq!(pixel_format(1), vk::Format::R8_UNORM);
        assert_eq!(
            [a8.r, a8.g, a8.b, a8.a],
            [
                vk::ComponentSwizzle::ZERO,
                vk::ComponentSwizzle::ZERO,
                vk::ComponentSwizzle::ZERO,
                vk::ComponentSwizzle::R,
            ]
        );
        let r8 = pixel_format_components(10);
        assert_eq!(
            [r8.r, r8.g, r8.b, r8.a],
            [vk::ComponentSwizzle::IDENTITY; 4]
        );
    }

    #[test]
    fn a_shader_reads_one_aspect_of_a_depth_stencil_texture() {
        assert_eq!(shader_aspect(260), vk::ImageAspectFlags::DEPTH);
        assert_eq!(shader_aspect(255), vk::ImageAspectFlags::DEPTH);
        assert_eq!(shader_aspect(261), vk::ImageAspectFlags::STENCIL);
        assert_eq!(shader_aspect(253), vk::ImageAspectFlags::STENCIL);
        assert_eq!(shader_aspect(80), vk::ImageAspectFlags::COLOR);
    }

    #[test]
    fn metal_pixel_formats_map_to_their_vulkan_layout() {
        assert_eq!(pixel_format(80), vk::Format::B8G8R8A8_UNORM);
        assert_eq!(pixel_format(70), vk::Format::R8G8B8A8_UNORM);
        assert_eq!(pixel_format(252), vk::Format::D32_SFLOAT);
        // The single-channel 32-bit trio is numbered like the two- and
        // four-channel ones, three above a multiple of ten and not below it.
        assert_eq!(pixel_format(53), vk::Format::R32_UINT);
        assert_eq!(pixel_format(54), vk::Format::R32_SINT);
        assert_eq!(pixel_format(55), vk::Format::R32_SFLOAT);
        assert_eq!(pixel_format(50), vk::Format::UNDEFINED);
        assert_eq!(pixel_format(260), vk::Format::D32_SFLOAT_S8_UINT);
        assert_eq!(pixel_format(9999), vk::Format::UNDEFINED);
    }

    #[test]
    fn packed_16bit_formats_keep_their_bit_layout() {
        // Metal lists components from bit 0 and Vulkan from bit 15, so the
        // alpha bit of A1BGR5 is Vulkan's trailing A1 and BGR5A1's leading one.
        assert_eq!(pixel_format(40), vk::Format::R5G6B5_UNORM_PACK16);
        assert_eq!(pixel_format(41), vk::Format::R5G5B5A1_UNORM_PACK16);
        assert_eq!(pixel_format(42), vk::Format::R4G4B4A4_UNORM_PACK16);
        assert_eq!(pixel_format(43), vk::Format::A1R5G5B5_UNORM_PACK16);
    }

    #[test]
    fn depth_formats_carry_their_aspect() {
        assert_eq!(
            aspect_of(vk::Format::D32_SFLOAT),
            vk::ImageAspectFlags::DEPTH
        );
        assert_eq!(
            aspect_of(vk::Format::D24_UNORM_S8_UINT),
            vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL
        );
        assert_eq!(
            aspect_of(vk::Format::B8G8R8A8_UNORM),
            vk::ImageAspectFlags::COLOR
        );
    }

    #[test]
    fn compressed_formats_report_their_block() {
        assert_eq!(block_extent(vk::Format::BC3_UNORM_BLOCK), (4, 4));
        assert_eq!(block_extent(vk::Format::R8G8B8A8_UNORM), (1, 1));
        assert_eq!(texel_size(vk::Format::BC1_RGBA_UNORM_BLOCK), 8);
    }
}
