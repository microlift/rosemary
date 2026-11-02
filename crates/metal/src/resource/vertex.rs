use super::*;

/// `MTLVertexDescriptor`: 31 buffer layouts and 31 attributes, each a small
/// bag of integers the render pipeline reads at creation time.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VertexLayout {
    pub stride: u64,
    pub step_function: u64,
    pub step_rate: u64,
}

impl Default for VertexLayout {
    fn default() -> Self {
        Self {
            stride: 0,
            // MTLVertexStepFunctionPerVertex.
            step_function: 1,
            step_rate: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct VertexAttribute {
    pub format: u64,
    pub offset: u64,
    pub buffer_index: u64,
}

pub(crate) const VERTEX_SLOTS: usize = 31;

pub(crate) struct VertexDescriptor {
    pub layouts: [VertexLayout; VERTEX_SLOTS],
    pub attributes: [VertexAttribute; VERTEX_SLOTS],
    /// The `-layouts` and `-attributes` collections, made on first use because
    /// a game holds the element objects and mutates them in place.
    pub layout_objects: Id,
    pub attribute_objects: Id,
}

/// One element of a `MTLVertexDescriptor` collection: it points back into the
/// descriptor it belongs to, because Metal's setters mutate the descriptor
/// through the element a game holds.
#[derive(Clone, Copy)]
pub(crate) struct VertexElement {
    pub owner: Id,
    pub index: usize,
    pub layout: bool,
    /// The values of an element with no owner: a standalone
    /// `MTLVertexBufferLayoutDescriptor` or `MTLVertexAttributeDescriptor`, or
    /// one whose descriptor has gone.
    pub local_layout: VertexLayout,
    pub local_attribute: VertexAttribute,
}

impl VertexElement {
    fn standalone(layout: bool) -> Self {
        Self {
            owner: std::ptr::null_mut(),
            index: 0,
            layout,
            local_layout: VertexLayout::default(),
            local_attribute: VertexAttribute::default(),
        }
    }
}

/// The layout one element names: its descriptor's, or its own.
fn layout_of<'a>(element: Id) -> &'a mut VertexLayout {
    // SAFETY: a vertex element of this crate's.
    let element = unsafe { object::body::<VertexElement>(element) };
    if element.owner.is_null() {
        return &mut element.local_layout;
    }
    // SAFETY: the element holds the descriptor which made it, which orphans
    // the element before it goes.
    &mut unsafe { object::body::<VertexDescriptor>(element.owner) }.layouts[element.index]
}

fn attribute_of<'a>(element: Id) -> &'a mut VertexAttribute {
    // SAFETY: a vertex element of this crate's.
    let element = unsafe { object::body::<VertexElement>(element) };
    if element.owner.is_null() {
        return &mut element.local_attribute;
    }
    // SAFETY: as for `layout_of`.
    &mut unsafe { object::body::<VertexDescriptor>(element.owner) }.attributes[element.index]
}

fn assign_vertex_element(destination: Id, source: Id) {
    // SAFETY: a vertex element of this crate's.
    let layout = unsafe { object::body::<VertexElement>(destination) }.layout;
    let source = [
        c"RosemaryMTLVertexElement",
        c"MTLVertexBufferLayoutDescriptor",
        c"MTLVertexAttributeDescriptor",
        c"MTLBufferLayoutDescriptor",
        c"MTLAttributeDescriptor",
    ]
    .iter()
    .any(|name| object::is_kind(source, name))
    .then_some(source);
    if layout {
        *layout_of(destination) = source.map_or_else(VertexLayout::default, |s| *layout_of(s));
    } else {
        *attribute_of(destination) =
            source.map_or_else(VertexAttribute::default, |s| *attribute_of(s));
    }
}

pub(super) unsafe extern "C" fn vertex_layout_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a freshly allocated element.
    unsafe { object::init(receiver, VertexElement::standalone(true)) };
    receiver
}

pub(super) unsafe extern "C" fn vertex_attribute_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a freshly allocated element.
    unsafe { object::init(receiver, VertexElement::standalone(false)) };
    receiver
}

pub(super) unsafe extern "C" fn vertex_element_value(receiver: Id, sel: Sel) -> u64 {
    // SAFETY: a selector the runtime owns.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"stride" => layout_of(receiver).stride,
        b"stepFunction" => layout_of(receiver).step_function,
        b"stepRate" => layout_of(receiver).step_rate,
        b"format" => attribute_of(receiver).format,
        b"offset" => attribute_of(receiver).offset,
        b"bufferIndex" => attribute_of(receiver).buffer_index,
        _ => 0,
    }
}

pub(super) unsafe extern "C" fn collection_assign(
    receiver: Id,
    _sel: Sel,
    object: Id,
    index: usize,
) {
    // SAFETY: an indexed collection of this crate's.
    let collection = unsafe { object::body::<IndexedCollection>(receiver) };
    if let Some(element) = collection.elements.get(index) {
        (collection.assign)(*element, object);
    }
}

pub(crate) struct IndexedCollection {
    pub elements: Vec<Id>,
    /// How to tell one of `elements` that its owner is gone. See [`orphan`].
    pub orphan: fn(Id),
    /// `collection[i] = object`: copy the values of a standalone descriptor,
    /// or the defaults where it is nil, into element `i`. Metal copies rather
    /// than keeping the object, so a later change to it changes nothing here.
    pub assign: fn(Id, Id),
}

/// A child object which mutates the descriptor that vended it.
///
/// The descriptor owns the collection and the collection owns its elements,
/// but a guest may hold an element - or the collection - longer than the
/// descriptor: `-colorAttachments` is an object with its own retain count.
/// The element cannot retain the owner back without making a cycle neither
/// side can break, so the owner instead *orphans* every element it vended on
/// the way out, and an orphaned element's setters and getters do nothing
/// rather than writing through a dangling pointer.
pub(crate) trait Element {
    fn owner(&mut self) -> &mut Id;
}

/// Null the owner of one element of a `T`-shaped collection.
///
/// # Safety
///
/// `element` must be an instance of the class declared for this `T`.
pub(crate) unsafe fn orphan_element<T: Element>(element: Id) {
    *unsafe { object::body::<T>(element) }.owner() = std::ptr::null_mut();
}

/// Tell every element of a collection - which may be null, because a
/// descriptor only makes one when a guest asks - that its owner is gone.
pub(crate) fn orphan(collection: Id) {
    if collection.is_null() {
        return;
    }
    // SAFETY: a collection of this crate's.
    let collection = unsafe { object::body::<IndexedCollection>(collection) };
    for element in &collection.elements {
        (collection.orphan)(*element);
    }
}

impl Element for VertexElement {
    fn owner(&mut self) -> &mut Id {
        &mut self.owner
    }
}

/// `+[MTLStageInputOutputDescriptor stageInputOutputDescriptor]`.
pub(super) unsafe extern "C" fn new_stage_input_descriptor(_class: Id, _sel: Sel) -> Id {
    autoreleased_vertex_descriptor(c"MTLStageInputOutputDescriptor")
}

/// `+[MTLVertexDescriptor vertexDescriptor]`.
pub(super) unsafe extern "C" fn new_vertex_descriptor(_class: Id, _sel: Sel) -> Id {
    autoreleased_vertex_descriptor(c"MTLVertexDescriptor")
}

fn autoreleased_vertex_descriptor(class: &std::ffi::CStr) -> Id {
    objc::arc::autorelease(vertex_descriptor_with(
        class,
        [VertexLayout::default(); VERTEX_SLOTS],
        [VertexAttribute::default(); VERTEX_SLOTS],
    ))
}

fn vertex_descriptor_with(
    class: &std::ffi::CStr,
    layouts: [VertexLayout; VERTEX_SLOTS],
    attributes: [VertexAttribute; VERTEX_SLOTS],
) -> Id {
    object::make(
        class,
        VertexDescriptor {
            layouts,
            attributes,
            layout_objects: std::ptr::null_mut(),
            attribute_objects: std::ptr::null_mut(),
        },
    )
}

/// A new, empty `MTLVertexDescriptor`, owned by the caller.
pub(crate) fn new_empty_vertex_descriptor() -> Id {
    vertex_descriptor_with(
        c"MTLVertexDescriptor",
        [VertexLayout::default(); VERTEX_SLOTS],
        [VertexAttribute::default(); VERTEX_SLOTS],
    )
}

/// A new `MTLVertexDescriptor` with the values of `source`, owned by the
/// caller: what a `copy` property stores.
///
/// # Safety
///
/// `source` must be a vertex descriptor of this crate's.
pub(crate) unsafe fn copy_vertex_descriptor(source: Id) -> Id {
    // SAFETY: the caller's contract.
    let source = unsafe { object::body::<VertexDescriptor>(source) };
    vertex_descriptor_with(c"MTLVertexDescriptor", source.layouts, source.attributes)
}

pub(super) unsafe extern "C" fn vertex_descriptor_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a vertex descriptor of this crate's. This is `-reset` as well as
    // `-init`; collections vended before a reset stay this descriptor's, and
    // their elements read the reset values through it.
    let descriptor = unsafe { object::body::<VertexDescriptor>(receiver) };
    descriptor.layouts = [VertexLayout::default(); VERTEX_SLOTS];
    descriptor.attributes = [VertexAttribute::default(); VERTEX_SLOTS];
    receiver
}

/// Give up the collections a vertex descriptor vended, as it goes.
///
/// A guest which holds `-layouts` past the descriptor keeps the collection and
/// its elements alive, and orphaning leaves their setters harmless rather than
/// writing into freed memory.
fn release_collections(descriptor: &mut VertexDescriptor) {
    for slot in [
        &mut descriptor.layout_objects,
        &mut descriptor.attribute_objects,
    ] {
        let collection = std::mem::replace(slot, std::ptr::null_mut());
        orphan(collection);
        objc::arc::release(collection);
    }
}

pub(super) fn vertex_collection(receiver: Id, layout: bool) -> Id {
    // SAFETY: a vertex descriptor of this crate's.
    let descriptor = unsafe { object::body::<VertexDescriptor>(receiver) };
    let existing = if layout {
        descriptor.layout_objects
    } else {
        descriptor.attribute_objects
    };
    if !existing.is_null() {
        return existing;
    }
    let elements = (0..VERTEX_SLOTS)
        .map(|index| {
            object::make(
                c"RosemaryMTLVertexElement",
                VertexElement {
                    owner: receiver,
                    index,
                    ..VertexElement::standalone(layout)
                },
            )
        })
        .collect::<Vec<_>>();
    let collection = object::make(
        c"RosemaryMTLIndexedCollection",
        IndexedCollection {
            elements,
            // SAFETY: every element of this collection is an `RosemaryMTLVertexElement`.
            orphan: |element| unsafe { orphan_element::<VertexElement>(element) },
            assign: assign_vertex_element,
        },
    );
    let descriptor = unsafe { object::body::<VertexDescriptor>(receiver) };
    if layout {
        descriptor.layout_objects = collection;
    } else {
        descriptor.attribute_objects = collection;
    }
    collection
}

pub(super) unsafe extern "C" fn vertex_layouts(receiver: Id, _sel: Sel) -> Id {
    vertex_collection(receiver, true)
}

pub(super) unsafe extern "C" fn vertex_attributes(receiver: Id, _sel: Sel) -> Id {
    vertex_collection(receiver, false)
}

pub(super) unsafe extern "C" fn indexed_element(receiver: Id, _sel: Sel, index: usize) -> Id {
    // SAFETY: an indexed collection of this crate's.
    unsafe { object::body::<IndexedCollection>(receiver) }
        .elements
        .get(index)
        .copied()
        .unwrap_or(std::ptr::null_mut())
}

/// `-[RosemaryMTLIndexedCollection dealloc]`: the elements are the collection's own.
///
/// Metal vends `-colorAttachments`, `-layouts` and `-attributes` as objects a
/// game subscripts and mutates in place, so the descriptor which made one holds
/// it and each element it contains. Without this every render pass descriptor a
/// frame created left its eight attachment objects behind.
pub(super) unsafe extern "C" fn collection_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a collection of this crate's.
    let collection = unsafe { object::body::<IndexedCollection>(receiver) };
    for element in std::mem::take(&mut collection.elements) {
        objc::arc::release(element);
    }
    objc::arc::finalize(receiver);
}

/// `-[MTLVertexDescriptor dealloc]`.
pub(super) unsafe extern "C" fn vertex_descriptor_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a vertex descriptor of this crate's.
    release_collections(unsafe { object::body::<VertexDescriptor>(receiver) });
    objc::arc::finalize(receiver);
}

pub(super) unsafe extern "C" fn set_vertex_element_value(receiver: Id, sel: Sel, value: u64) {
    // SAFETY: a selector the runtime owns.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"setStride:" => layout_of(receiver).stride = value,
        b"setStepFunction:" => layout_of(receiver).step_function = value,
        b"setStepRate:" => layout_of(receiver).step_rate = value,
        b"setFormat:" => attribute_of(receiver).format = value,
        b"setOffset:" => attribute_of(receiver).offset = value,
        b"setBufferIndex:" => attribute_of(receiver).buffer_index = value,
        _ => (),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_collection_held_across_reset_is_orphaned_when_its_descriptor_goes() {
        crate::bootstrap();
        let descriptor = new_empty_vertex_descriptor();
        // SAFETY: the registered methods of a vertex descriptor this test made.
        unsafe {
            let layouts = objc::arc::retain(vertex_layouts(descriptor, objc::sel::get("layouts")));
            let element = indexed_element(layouts, objc::sel::get("objectAtIndexedSubscript:"), 1);
            set_vertex_element_value(element, objc::sel::get("setStride:"), 16);
            vertex_descriptor_init(descriptor, objc::sel::get("reset"));
            assert_eq!(
                vertex_layouts(descriptor, objc::sel::get("layouts")),
                layouts
            );
            assert_eq!(vertex_element_value(element, objc::sel::get("stride")), 0);

            objc::arc::release(descriptor);
            assert!(object::body::<VertexElement>(element).owner.is_null());
            objc::arc::release(layouts);
        }
    }
}
