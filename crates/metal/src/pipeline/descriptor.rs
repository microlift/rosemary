use super::*;

use crate::object;
use objc::object::Id;
use objc::sel::Sel;

/// One element of `-[MTLRenderPipelineDescriptor colorAttachments]`, which
/// mutates the descriptor which vended it.
#[derive(Clone, Copy)]
pub(crate) struct ColorElement {
    pub owner: Id,
    pub index: usize,
    /// The values of an element with no owner: a standalone
    /// `MTLRenderPipelineColorAttachmentDescriptor`, or one whose descriptor
    /// has gone.
    pub local: ColorAttachment,
}

/// The colour attachment one element names: its descriptor's, or its own.
fn color_of<'a>(element: Id) -> &'a mut ColorAttachment {
    // SAFETY: a colour attachment element of this crate's.
    let element = unsafe { object::body::<ColorElement>(element) };
    if element.owner.is_null() {
        return &mut element.local;
    }
    // SAFETY: the element holds the descriptor which made it, which orphans
    // the element before it goes.
    &mut unsafe { object::body::<RenderPipelineDescriptor>(element.owner) }.colors[element.index]
}

pub(super) const COLOR_CLASSES: [&std::ffi::CStr; 2] = [
    c"RosemaryMTLColorAttachmentDescriptor",
    c"MTLRenderPipelineColorAttachmentDescriptor",
];

fn assign_color(destination: Id, source: Id) {
    let is_color = COLOR_CLASSES
        .iter()
        .any(|name| object::is_kind(source, name));
    *color_of(destination) = if is_color {
        *color_of(source)
    } else {
        ColorAttachment::fresh()
    };
}

/// `-init` of a standalone colour attachment descriptor.
pub(super) unsafe extern "C" fn color_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a freshly allocated element.
    unsafe {
        object::init(
            receiver,
            ColorElement {
                owner: std::ptr::null_mut(),
                index: 0,
                local: ColorAttachment::fresh(),
            },
        )
    };
    receiver
}

pub(super) unsafe extern "C" fn color_value(receiver: Id, sel: Sel) -> u64 {
    let color = color_of(receiver);
    // SAFETY: a selector the runtime owns.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"pixelFormat" => color.pixel_format,
        b"isBlendingEnabled" => u64::from(color.blending),
        b"sourceRGBBlendFactor" => color.source_rgb,
        b"destinationRGBBlendFactor" => color.destination_rgb,
        b"rgbBlendOperation" => color.rgb_operation,
        b"sourceAlphaBlendFactor" => color.source_alpha,
        b"destinationAlphaBlendFactor" => color.destination_alpha,
        b"alphaBlendOperation" => color.alpha_operation,
        b"writeMask" => color.write_mask,
        _ => 0,
    }
}

impl crate::resource::Element for ColorElement {
    fn owner(&mut self) -> &mut Id {
        &mut self.owner
    }
}

pub(super) unsafe extern "C" fn descriptor_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a render pipeline descriptor of this crate's. This is `-reset`
    // as well as `-init`, and a reset descriptor still owns what it holds.
    unsafe { release_descriptor_objects(receiver) };
    let descriptor = unsafe { object::body::<RenderPipelineDescriptor>(receiver) };
    // A `-colorAttachments` taken before the reset stays this descriptor's,
    // and its elements read the reset values through it.
    let color_objects = descriptor.color_objects;
    *descriptor = RenderPipelineDescriptor {
        vertex_function: std::ptr::null_mut(),
        fragment_function: std::ptr::null_mut(),
        vertex_descriptor: std::ptr::null_mut(),
        colors: [ColorAttachment::fresh(); COLOR_ATTACHMENTS],
        depth_format: 0,
        stencil_format: 0,
        sample_count: 1,
        alpha_to_coverage: false,
        alpha_to_one: false,
        rasterization: true,
        input_topology: 0,
        color_objects,
    };
    receiver
}

/// Give up the functions and vertex descriptor a render pipeline descriptor
/// holds.
///
/// # Safety
///
/// `receiver` must be a `MTLRenderPipelineDescriptor` of this crate's.
unsafe fn release_descriptor_objects(receiver: Id) {
    let descriptor = unsafe { object::body::<RenderPipelineDescriptor>(receiver) };
    for slot in [
        &mut descriptor.vertex_function,
        &mut descriptor.fragment_function,
        &mut descriptor.vertex_descriptor,
    ] {
        objc::arc::release(std::mem::replace(slot, std::ptr::null_mut()));
    }
}

/// A new descriptor with the values `source` has now, owned by the caller,
/// for work which reads it after its maker may have changed it.
///
/// # Safety
///
/// `source` must be a `MTLRenderPipelineDescriptor` of this crate's.
pub(crate) unsafe fn copy_render_pipeline_descriptor(source: Id) -> Id {
    // SAFETY: the caller's contract.
    let source = unsafe { object::body::<RenderPipelineDescriptor>(source) };
    let vertex_descriptor = if source.vertex_descriptor.is_null() {
        std::ptr::null_mut()
    } else {
        // SAFETY: a descriptor only holds a vertex descriptor of this crate's.
        unsafe { crate::resource::copy_vertex_descriptor(source.vertex_descriptor) }
    };
    object::make(
        c"MTLRenderPipelineDescriptor",
        RenderPipelineDescriptor {
            vertex_function: objc::arc::retain(source.vertex_function),
            fragment_function: objc::arc::retain(source.fragment_function),
            vertex_descriptor,
            color_objects: std::ptr::null_mut(),
            ..*source
        },
    )
}

/// `-[MTLRenderPipelineDescriptor dealloc]`.
pub(super) unsafe extern "C" fn descriptor_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a render pipeline descriptor of this crate's.
    unsafe { release_descriptor_objects(receiver) };
    // The colour attachments point back at this descriptor, and a guest may
    // hold one after releasing it: orphan them before the body goes.
    let collection = std::mem::replace(
        &mut unsafe { object::body::<RenderPipelineDescriptor>(receiver) }.color_objects,
        std::ptr::null_mut(),
    );
    crate::resource::orphan(collection);
    objc::arc::release(collection);
    objc::arc::finalize(receiver);
}

pub(super) unsafe extern "C" fn set_descriptor_object(receiver: Id, sel: Sel, value: Id) {
    // SAFETY: a render pipeline descriptor of this crate's.
    let descriptor = unsafe { object::body::<RenderPipelineDescriptor>(receiver) };
    // SAFETY: a selector the runtime owns.
    let (slot, owned) = match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"setVertexFunction:" => (&mut descriptor.vertex_function, objc::arc::retain(value)),
        b"setFragmentFunction:" => (&mut descriptor.fragment_function, objc::arc::retain(value)),
        // A `copy` property: a guest which goes on editing its descriptor
        // must not change this one.
        b"setVertexDescriptor:" if value.is_null() => {
            (&mut descriptor.vertex_descriptor, std::ptr::null_mut())
        }
        b"setVertexDescriptor:" => (
            &mut descriptor.vertex_descriptor,
            // SAFETY: `MTLVertexDescriptor` is this crate's class.
            unsafe { crate::resource::copy_vertex_descriptor(value) },
        ),
        _ => return,
    };
    objc::arc::release(std::mem::replace(slot, owned));
}

pub(super) unsafe extern "C" fn descriptor_object(receiver: Id, sel: Sel) -> Id {
    // SAFETY: a render pipeline descriptor of this crate's.
    let descriptor = unsafe { object::body::<RenderPipelineDescriptor>(receiver) };
    // SAFETY: a selector the runtime owns.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"vertexFunction" => descriptor.vertex_function,
        b"fragmentFunction" => descriptor.fragment_function,
        // Never nil in Metal: MoltenVK fills in the one this vends rather
        // than setting its own.
        b"vertexDescriptor" => {
            if descriptor.vertex_descriptor.is_null() {
                descriptor.vertex_descriptor = crate::resource::new_empty_vertex_descriptor();
            }
            descriptor.vertex_descriptor
        }
        _ => std::ptr::null_mut(),
    }
}

pub(super) unsafe extern "C" fn descriptor_number(receiver: Id, sel: Sel) -> u64 {
    // SAFETY: a render pipeline descriptor of this crate's.
    let descriptor = unsafe { object::body::<RenderPipelineDescriptor>(receiver) };
    // SAFETY: a selector the runtime owns.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"depthAttachmentPixelFormat" => descriptor.depth_format,
        b"stencilAttachmentPixelFormat" => descriptor.stencil_format,
        b"sampleCount" | b"rasterSampleCount" => descriptor.sample_count,
        b"isAlphaToCoverageEnabled" => u64::from(descriptor.alpha_to_coverage),
        b"isAlphaToOneEnabled" => u64::from(descriptor.alpha_to_one),
        b"isRasterizationEnabled" => u64::from(descriptor.rasterization),
        b"inputPrimitiveTopology" => descriptor.input_topology,
        _ => 0,
    }
}

pub(super) unsafe extern "C" fn set_descriptor_number(receiver: Id, sel: Sel, value: u64) {
    // SAFETY: a render pipeline descriptor of this crate's.
    let descriptor = unsafe { object::body::<RenderPipelineDescriptor>(receiver) };
    // SAFETY: a selector the runtime owns.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"setDepthAttachmentPixelFormat:" => descriptor.depth_format = value,
        b"setStencilAttachmentPixelFormat:" => descriptor.stencil_format = value,
        b"setSampleCount:" | b"setRasterSampleCount:" => descriptor.sample_count = value,
        b"setAlphaToCoverageEnabled:" => descriptor.alpha_to_coverage = value & 0xff != 0,
        b"setAlphaToOneEnabled:" => descriptor.alpha_to_one = value & 0xff != 0,
        b"setRasterizationEnabled:" => descriptor.rasterization = value & 0xff != 0,
        b"setInputPrimitiveTopology:" => descriptor.input_topology = value,
        _ => (),
    }
}

pub(super) unsafe extern "C" fn color_attachments(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a render pipeline descriptor of this crate's.
    let descriptor = unsafe { object::body::<RenderPipelineDescriptor>(receiver) };
    if !descriptor.color_objects.is_null() {
        return descriptor.color_objects;
    }
    let elements = (0..COLOR_ATTACHMENTS)
        .map(|index| {
            object::make(
                c"RosemaryMTLColorAttachmentDescriptor",
                ColorElement {
                    owner: receiver,
                    index,
                    local: ColorAttachment::fresh(),
                },
            )
        })
        .collect::<Vec<_>>();
    let collection = object::make(
        c"RosemaryMTLIndexedCollection",
        crate::resource::IndexedCollection {
            elements,
            // SAFETY: every element here is an `RosemaryMTLColorAttachmentDescriptor`.
            orphan: |element| unsafe { crate::resource::orphan_element::<ColorElement>(element) },
            assign: assign_color,
        },
    );
    unsafe { object::body::<RenderPipelineDescriptor>(receiver) }.color_objects = collection;
    collection
}

pub(super) unsafe extern "C" fn set_color_value(receiver: Id, sel: Sel, value: u64) {
    let color = color_of(receiver);
    // SAFETY: a selector the runtime owns.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"setPixelFormat:" => color.pixel_format = value,
        b"setBlendingEnabled:" => color.blending = value & 0xff != 0,
        b"setSourceRGBBlendFactor:" => color.source_rgb = value,
        b"setDestinationRGBBlendFactor:" => color.destination_rgb = value,
        b"setRgbBlendOperation:" => color.rgb_operation = value,
        b"setSourceAlphaBlendFactor:" => color.source_alpha = value,
        b"setDestinationAlphaBlendFactor:" => color.destination_alpha = value,
        b"setAlphaBlendOperation:" => color.alpha_operation = value,
        b"setWriteMask:" => color.write_mask = value,
        _ => (),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_attachments_held_across_reset_are_orphaned_when_their_descriptor_goes() {
        crate::bootstrap();
        let descriptor = object::make(
            c"MTLRenderPipelineDescriptor",
            RenderPipelineDescriptor {
                vertex_function: std::ptr::null_mut(),
                fragment_function: std::ptr::null_mut(),
                vertex_descriptor: std::ptr::null_mut(),
                colors: [ColorAttachment::fresh(); COLOR_ATTACHMENTS],
                depth_format: 0,
                stencil_format: 0,
                sample_count: 1,
                alpha_to_coverage: false,
                alpha_to_one: false,
                rasterization: true,
                input_topology: 0,
                color_objects: std::ptr::null_mut(),
            },
        );
        let get = objc::sel::get("colorAttachments");
        // SAFETY: the registered methods of a descriptor this test made.
        unsafe {
            let colors = objc::arc::retain(color_attachments(descriptor, get));
            let element = object::body::<crate::resource::IndexedCollection>(colors).elements[0];
            set_color_value(element, objc::sel::get("setPixelFormat:"), 80);
            descriptor_init(descriptor, objc::sel::get("reset"));
            assert_eq!(color_attachments(descriptor, get), colors);
            assert_eq!(color_value(element, objc::sel::get("pixelFormat")), 0);

            objc::arc::release(descriptor);
            assert!(object::body::<ColorElement>(element).owner.is_null());
            objc::arc::release(colors);
        }
    }
}
