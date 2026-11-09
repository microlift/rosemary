mod buffer;
mod common;
mod depth_stencil;
mod heap;
pub(crate) mod sampler;
mod texture;
mod vertex;

use objc::object::Id;
use objc::sel::Sel;
use static_init::dynamic;

use crate::gpu::{self, Gpu, GpuBuffer, GpuImage, ImageDescription};
use crate::object::{self, methods};
use ash::vk;

use buffer::*;
use depth_stencil::*;
use sampler::*;
use texture::*;
use vertex::*;

pub(crate) use buffer::leaking;

pub(crate) use buffer::new_buffer;
pub(crate) use depth_stencil::{DepthStencilState, StencilState, new_depth_stencil_state};
pub(crate) use heap::new_heap;
#[cfg(test)]
pub(crate) use heap::placement_of;
pub(crate) use heap::{heap_buffer_size_and_align, heap_texture_size_and_align};
pub(crate) use sampler::{SamplerDescriptor, SamplerState, compare_op, new_sampler};
#[cfg(test)]
pub(crate) use texture::Region;
pub(crate) use texture::{Texture, TextureDescriptor, drawable_texture, new_texture};
pub(crate) use vertex::{
    Element, IndexedCollection, VERTEX_SLOTS, VertexAttribute, VertexDescriptor, VertexLayout,
    copy_vertex_descriptor, new_empty_vertex_descriptor, orphan, orphan_element,
};

pub(crate) struct Buffer {
    pub gpu: &'static Gpu,
    /// For a buffer with host pages, `buffer` is null: the device reaches
    /// the bytes through [`Buffer::device_buffer`] and [`Buffer::device_range`].
    pub inner: GpuBuffer,
    pub options: u64,
    host: Option<gpu::HostPages>,
}

impl Default for SamplerDescriptor {
    fn default() -> Self {
        Self {
            min_filter: 0,
            mag_filter: 0,
            mip_filter: 0,
            address: [0; 3],
            max_anisotropy: 1,
            // `MTLCompareFunctionNever` is the Metal default, and it means the
            // sampler does no comparison at all rather than always failing.
            compare_function: 0,
            lod_min_clamp: 0.0,
            lod_max_clamp: f32::MAX,
            normalized_coordinates: true,
            border_color: 0,
        }
    }
}

impl Default for StencilState {
    fn default() -> Self {
        Self {
            compare: 7, // Always
            fail: 0,
            depth_fail: 0,
            pass: 0,
            read_mask: u32::MAX,
            write_mask: u32::MAX,
        }
    }
}

impl Default for DepthStencilState {
    fn default() -> Self {
        Self {
            depth_compare: 7, // Always
            depth_write: false,
            front: StencilState::default(),
            back: StencilState::default(),
            stencil_enabled: false,
        }
    }
}

pub(crate) fn install() {
    let buffer = object::register::<Buffer>(c"RosemaryMTLBuffer");
    methods!(buffer, {
        "newTextureWithDescriptor:offset:bytesPerRow:" => texture::buffer_new_texture,
            c"@40@0:8@16Q24Q32";
        "dealloc" => buffer_dealloc, c"v16@0:8";
        "contents" => buffer_contents, c"^v16@0:8";
        "length" => buffer_length, c"Q16@0:8";
        "storageMode" => buffer_storage_mode, c"Q16@0:8";
        "didModifyRange:" => crate::no_op, c"v32@0:8{_NSRange=QQ}16";
        "addDebugMarker:range:" => crate::no_op, c"v40@0:8@16{_NSRange=QQ}24";
    });

    let descriptor = object::register::<TextureDescriptor>(c"MTLTextureDescriptor");
    let texture = object::register::<Texture>(c"RosemaryMTLTexture");
    methods!(descriptor, {
        "init" => descriptor_init, c"@16@0:8";
        "copyWithZone:" => descriptor_copy, c"@24@0:8^{_NSZone=}16";
        "resourceOptions" => texture::descriptor_resource_options, c"Q16@0:8";
        "setResourceOptions:" => texture::set_descriptor_resource_options, c"v24@0:8Q16";
        "cpuCacheMode" => crate::zero, c"Q16@0:8";
        "setCpuCacheMode:" => crate::no_op, c"v24@0:8Q16";
        "hazardTrackingMode" => crate::zero, c"Q16@0:8";
        "setHazardTrackingMode:" => crate::no_op, c"v24@0:8Q16";
        // How a texture's memory is laid out is the Vulkan driver's choice,
        // as it is Metal's when this is left at its default.
        "allowGPUOptimizedContents" => crate::device::yes, c"B16@0:8";
        "setAllowGPUOptimizedContents:" => crate::no_op, c"v20@0:8c16";
    });
    methods!(descriptor.metaclass(), {
        "texture2DDescriptorWithPixelFormat:width:height:mipmapped:"
            => texture::texture_2d_descriptor, c"@44@0:8Q16Q24Q32B40";
        "textureCubeDescriptorWithPixelFormat:size:mipmapped:"
            => texture::texture_cube_descriptor, c"@36@0:8Q16Q24B32";
        "textureBufferDescriptorWithPixelFormat:width:resourceOptions:usage:"
            => texture::texture_buffer_descriptor, c"@48@0:8Q16Q24Q32Q40";
    });
    for selector in [
        "setTextureType:",
        "setPixelFormat:",
        "setWidth:",
        "setHeight:",
        "setDepth:",
        "setMipmapLevelCount:",
        "setSampleCount:",
        "setArrayLength:",
        "setStorageMode:",
        "setUsage:",
    ] {
        descriptor.replace_method(
            objc::sel::get(selector),
            set_descriptor_value as *const std::ffi::c_void,
            c"v24@0:8Q16",
        );
    }
    for selector in [
        "textureType",
        "pixelFormat",
        "width",
        "height",
        "depth",
        "mipmapLevelCount",
        "sampleCount",
        "arrayLength",
        "storageMode",
        "usage",
    ] {
        descriptor.replace_method(
            objc::sel::get(selector),
            descriptor_value as *const std::ffi::c_void,
            c"Q16@0:8",
        );
        texture.replace_method(
            objc::sel::get(selector),
            texture_value as *const std::ffi::c_void,
            c"Q16@0:8",
        );
    }
    heap::install();
    common::install(buffer);
    common::install_buffer(buffer);
    common::install(texture);
    common::install_texture(texture);
    methods!(texture, {
        "dealloc" => texture_dealloc, c"v16@0:8";
        "replaceRegion:mipmapLevel:slice:withBytes:bytesPerRow:bytesPerImage:"
            => replace_region,
                c"v104@0:8{MTLRegion={MTLOrigin=QQQ}{MTLSize=QQQ}}16Q64Q72^v80Q88Q96";
        "replaceRegion:mipmapLevel:withBytes:bytesPerRow:" => replace_region_level,
            c"v88@0:8{MTLRegion={MTLOrigin=QQQ}{MTLSize=QQQ}}16Q64^v72Q80";
        "getBytes:bytesPerRow:fromRegion:mipmapLevel:" => get_bytes,
            c"v88@0:8^v16Q24{MTLRegion={MTLOrigin=QQQ}{MTLSize=QQQ}}32Q80";
        "getBytes:bytesPerRow:bytesPerImage:fromRegion:mipmapLevel:slice:" => get_bytes_slice,
            c"v104@0:8^v16Q24Q32{MTLRegion={MTLOrigin=QQQ}{MTLSize=QQQ}}40Q88Q96";
        "setLabel:" => crate::no_op, c"v24@0:8@16";
        "newTextureViewWithPixelFormat:" => new_texture_view, c"@24@0:8Q16";
        "newTextureViewWithPixelFormat:textureType:levels:slices:" => new_texture_view_range,
            c"@64@0:8Q16Q24{_NSRange=QQ}32{_NSRange=QQ}48";
        "newTextureViewWithPixelFormat:textureType:levels:slices:swizzle:"
            => texture::new_texture_view_swizzled,
            c"@68@0:8Q16Q24{_NSRange=QQ}32{_NSRange=QQ}48{MTLTextureSwizzleChannels=CCCC}64";
    });
    methods!(texture, {
        "parentTexture" => texture_parent, c"@16@0:8";
        "rootResource" => texture_parent, c"@16@0:8";
    });

    let sampler_descriptor = object::register::<SamplerDescriptor>(c"MTLSamplerDescriptor");
    let sampler_state = object::register::<SamplerState>(c"RosemaryMTLSamplerState");
    methods!(sampler_state, {
        "dealloc" => sampler_state_dealloc, c"v16@0:8";
        "setLabel:" => crate::no_op, c"v24@0:8@16";
    });
    methods!(sampler_descriptor, {
        "init" => sampler_descriptor_init, c"@16@0:8";
        "setLodMinClamp:" => set_sampler_clamp, c"v20@0:8f16";
        "setLodMaxClamp:" => set_sampler_clamp, c"v20@0:8f16";
        "setLabel:" => crate::no_op, c"v24@0:8@16";
        // A Vulkan sampler can be bound anywhere a sampler can, so no
        // sampler needs to be made differently for an argument buffer.
        "setSupportArgumentBuffers:" => crate::no_op, c"v20@0:8c16";
    });
    for selector in [
        "setMinFilter:",
        "setMagFilter:",
        "setMipFilter:",
        "setSAddressMode:",
        "setTAddressMode:",
        "setRAddressMode:",
        "setMaxAnisotropy:",
        "setCompareFunction:",
        "setBorderColor:",
        "setNormalizedCoordinates:",
    ] {
        sampler_descriptor.replace_method(
            objc::sel::get(selector),
            set_sampler_value as *const std::ffi::c_void,
            c"v24@0:8Q16",
        );
    }

    let depth = object::register::<DepthStencilState>(c"MTLDepthStencilDescriptor");
    object::register::<DepthStencilState>(c"RosemaryMTLDepthStencilState");
    let stencil = object::register::<StencilState>(c"MTLStencilDescriptor");
    methods!(depth, {
        "init" => depth_descriptor_init, c"@16@0:8";
        "depthCompareFunction" => depth_compare, c"Q16@0:8";
        "setDepthCompareFunction:" => set_depth_compare, c"v24@0:8Q16";
        "isDepthWriteEnabled" => depth_write, c"c16@0:8";
        "setDepthWriteEnabled:" => set_depth_write, c"v20@0:8c16";
        "frontFaceStencil" => face_stencil, c"@16@0:8";
        "setFrontFaceStencil:" => set_face_stencil, c"v24@0:8@16";
        "backFaceStencil" => face_stencil, c"@16@0:8";
        "setBackFaceStencil:" => set_face_stencil, c"v24@0:8@16";
        "setLabel:" => crate::no_op, c"v24@0:8@16";
    });
    methods!(stencil, {
        "init" => stencil_descriptor_init, c"@16@0:8";
    });
    for selector in [
        "setStencilCompareFunction:",
        "setStencilFailureOperation:",
        "setDepthFailureOperation:",
        "setDepthStencilPassOperation:",
        "setReadMask:",
        "setWriteMask:",
    ] {
        stencil.replace_method(
            objc::sel::get(selector),
            set_stencil_value as *const std::ffi::c_void,
            c"v24@0:8Q16",
        );
    }

    let vertex = object::register::<VertexDescriptor>(c"MTLVertexDescriptor");
    let collection = object::register::<IndexedCollection>(c"RosemaryMTLIndexedCollection");
    let element = object::register::<VertexElement>(c"RosemaryMTLVertexElement");
    methods!(vertex, {
        "init" => vertex_descriptor_init, c"@16@0:8";
        "layouts" => vertex_layouts, c"@16@0:8";
        "attributes" => vertex_attributes, c"@16@0:8";
        "dealloc" => vertex_descriptor_dealloc, c"v16@0:8";
    });
    methods!(vertex.metaclass(), {
        "vertexDescriptor" => new_vertex_descriptor, c"@16@0:8";
    });
    // A kernel's `[[stage_in]]` is described with the same layouts and
    // attributes. No translated kernel has one, so the index buffer it names
    // is never read.
    let stage_input = object::register::<VertexDescriptor>(c"MTLStageInputOutputDescriptor");
    methods!(stage_input, {
        "init" => vertex_descriptor_init, c"@16@0:8";
        "reset" => vertex_descriptor_init, c"@16@0:8";
        "layouts" => vertex_layouts, c"@16@0:8";
        "attributes" => vertex_attributes, c"@16@0:8";
        "dealloc" => vertex_descriptor_dealloc, c"v16@0:8";
        "setIndexBufferIndex:" => crate::no_op, c"v24@0:8Q16";
        "setIndexType:" => crate::no_op, c"v24@0:8Q16";
    });
    methods!(stage_input.metaclass(), {
        "stageInputOutputDescriptor" => new_stage_input_descriptor, c"@16@0:8";
    });
    methods!(collection, {
        "objectAtIndexedSubscript:" => indexed_element, c"@24@0:8Q16";
        "setObject:atIndexedSubscript:" => collection_assign, c"v32@0:8@16Q24";
        "dealloc" => collection_dealloc, c"v16@0:8";
    });
    // A game makes standalone layouts and attributes with `+new` and assigns
    // them into a descriptor's collections.
    let layout = object::register::<VertexElement>(c"MTLVertexBufferLayoutDescriptor");
    let attribute = object::register::<VertexElement>(c"MTLVertexAttributeDescriptor");
    let stage_layout = object::register::<VertexElement>(c"MTLBufferLayoutDescriptor");
    let stage_attribute = object::register::<VertexElement>(c"MTLAttributeDescriptor");
    for class in [layout, stage_layout] {
        methods!(class, { "init" => vertex_layout_init, c"@16@0:8"; });
    }
    for class in [attribute, stage_attribute] {
        methods!(class, { "init" => vertex_attribute_init, c"@16@0:8"; });
    }
    for class in [element, layout, attribute, stage_layout, stage_attribute] {
        for selector in [
            "setStride:",
            "setStepFunction:",
            "setStepRate:",
            "setFormat:",
            "setOffset:",
            "setBufferIndex:",
        ] {
            class.replace_method(
                objc::sel::get(selector),
                set_vertex_element_value as *const std::ffi::c_void,
                c"v24@0:8Q16",
            );
        }
        for selector in [
            "stride",
            "stepFunction",
            "stepRate",
            "format",
            "offset",
            "bufferIndex",
        ] {
            class.replace_method(
                objc::sel::get(selector),
                vertex_element_value as *const std::ffi::c_void,
                c"Q16@0:8",
            );
        }
    }
}
