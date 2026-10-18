use std::collections::HashMap;
use std::sync::Mutex;

use super::*;

const MTL_STORAGE_MODE_PRIVATE: u64 = 2;
const MTL_HEAP_TYPE_PLACEMENT: u64 = 1;
const MTL_PURGEABLE_STATE_NON_VOLATILE: u64 = 2;

/// Alignments of the sizes the device quotes. Any will do as long as the
/// quote and the heap's charge agree; these are in the range Apple's are.
const BUFFER_ALIGNMENT: u64 = 256;
const TEXTURE_ALIGNMENT: u64 = 4096;

#[derive(Clone, Copy, Debug)]
pub(crate) struct HeapDescriptor {
    size: u64,
    storage_mode: u64,
    cpu_cache_mode: u64,
    hazard_tracking_mode: u64,
    heap_type: u64,
}

impl Default for HeapDescriptor {
    fn default() -> Self {
        Self {
            size: 0,
            storage_mode: MTL_STORAGE_MODE_PRIVATE,
            cpu_cache_mode: 0,
            hazard_tracking_mode: 0,
            heap_type: 0,
        }
    }
}

impl HeapDescriptor {
    /// `MTLResourceOptions`: cache mode in bits 0-3, storage mode in 4-7,
    /// hazard tracking in 8-9.
    fn resource_options(&self) -> u64 {
        self.cpu_cache_mode | (self.storage_mode << 4) | (self.hazard_tracking_mode << 8)
    }

    fn set_resource_options(&mut self, options: u64) {
        self.cpu_cache_mode = options & 0xf;
        self.storage_mode = (options >> 4) & 0xf;
        self.hazard_tracking_mode = (options >> 8) & 0x3;
    }
}

pub(crate) struct Heap {
    device: Id,
    gpu: &'static Gpu,
    descriptor: HeapDescriptor,
    used: Mutex<u64>,
}

/// What a resource made from a heap owes it.
struct Membership {
    heap: Id,
    charge: u64,
    aliasable: bool,
}

// SAFETY: the ids are only handed back to the Objective-C runtime, which is
// thread-safe, never dereferenced here.
unsafe impl Send for Membership {}

static MEMBERS: Mutex<Option<HashMap<usize, Membership>>> = Mutex::new(None);

/// `MTLSizeAndAlign`.
#[repr(C)]
pub(crate) struct SizeAndAlign {
    size: u64,
    align: u64,
}

fn round_up(value: u64, align: u64) -> u64 {
    value.div_ceil(align) * align
}

fn buffer_quote(length: u64) -> SizeAndAlign {
    SizeAndAlign {
        size: round_up(length.max(1), BUFFER_ALIGNMENT),
        align: BUFFER_ALIGNMENT,
    }
}

/// Every level of every slice, at the format's block size.
fn texture_quote(descriptor: &TextureDescriptor) -> Option<SizeAndAlign> {
    let format = gpu::pixel_format(descriptor.pixel_format);
    if format == vk::Format::UNDEFINED {
        return None;
    }
    let (_, _, layers, _) = descriptor.shape();
    let (block_width, block_height) = gpu::block_extent(format);
    let block = u64::from(gpu::texel_size(format)).max(1);
    let depth = match descriptor.texture_type {
        7 => descriptor.depth.max(1),
        _ => 1,
    };
    let mut size = 0;
    for level in 0..descriptor.mipmap_level_count.max(1) {
        let width = (descriptor.width.max(1) >> level).max(1);
        let height = (descriptor.height.max(1) >> level).max(1);
        let depth = (depth >> level).max(1);
        size += width.div_ceil(u64::from(block_width))
            * height.div_ceil(u64::from(block_height))
            * depth
            * block;
    }
    size *= u64::from(layers) * descriptor.sample_count.max(1);
    Some(SizeAndAlign {
        size: round_up(size, TEXTURE_ALIGNMENT),
        align: TEXTURE_ALIGNMENT,
    })
}

/// `-[MTLDevice heapBufferSizeAndAlignWithLength:options:]`.
pub(crate) unsafe extern "C" fn heap_buffer_size_and_align(
    _receiver: Id,
    _sel: Sel,
    length: u64,
    _options: u64,
) -> SizeAndAlign {
    buffer_quote(length)
}

/// `-[MTLDevice heapTextureSizeAndAlignWithDescriptor:]`.
pub(crate) unsafe extern "C" fn heap_texture_size_and_align(
    _receiver: Id,
    _sel: Sel,
    descriptor: Id,
) -> SizeAndAlign {
    if descriptor.is_null() {
        return SizeAndAlign { size: 0, align: 1 };
    }
    // SAFETY: `MTLTextureDescriptor` is this crate's class.
    let descriptor = unsafe { object::body::<TextureDescriptor>(descriptor) };
    texture_quote(descriptor).unwrap_or(SizeAndAlign { size: 0, align: 1 })
}

/// `-[MTLDevice newHeapWithDescriptor:]`.
pub(crate) fn new_heap(gpu: &'static Gpu, device: Id, descriptor: Id) -> Id {
    if descriptor.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `MTLHeapDescriptor` is this crate's class.
    let descriptor = *unsafe { object::body::<HeapDescriptor>(descriptor) };
    if descriptor.size == 0 {
        return std::ptr::null_mut();
    }
    if descriptor.heap_type == MTL_HEAP_TYPE_PLACEMENT {
        gpu::trace("rejecting newHeapWithDescriptor: MTLHeapTypePlacement is not implemented");
        return std::ptr::null_mut();
    }
    object::make(
        c"RosemaryMTLHeap",
        Heap {
            device: objc::arc::retain(device),
            gpu,
            descriptor,
            used: Mutex::new(0),
        },
    )
}

/// The heap's use once `quote` is charged to it, or `None` when it does not fit.
fn fit(used: u64, capacity: u64, quote: &SizeAndAlign) -> Option<u64> {
    let end = round_up(used, quote.align) + quote.size;
    (end <= capacity).then_some(end)
}

/// Charge `quote` to `heap`, or refuse it as Metal does when it does not fit.
fn reserve(heap: &Heap, quote: &SizeAndAlign) -> bool {
    let mut used = heap.used.lock().expect("heap books");
    match fit(*used, heap.descriptor.size, quote) {
        Some(end) => {
            *used = end;
            true
        }
        None => false,
    }
}

fn give_back(heap: Id, charge: u64) {
    // SAFETY: only heaps are recorded as a member's heap.
    let heap = unsafe { object::body::<Heap>(heap) };
    let mut used = heap.used.lock().expect("heap books");
    *used = used.saturating_sub(charge);
}

/// Record `resource` as made from `heap`, which it keeps alive.
fn join(resource: Id, heap: Id, charge: u64) {
    let mut members = MEMBERS.lock().expect("heap members");
    members.get_or_insert_with(HashMap::new).insert(
        resource as usize,
        Membership {
            heap: objc::arc::retain(heap),
            charge,
            aliasable: false,
        },
    );
}

/// A heap resource's dealloc: its space goes back unless it already did.
pub(super) fn leave(resource: Id) {
    let member = MEMBERS
        .lock()
        .expect("heap members")
        .as_mut()
        .and_then(|members| members.remove(&(resource as usize)));
    let Some(member) = member else {
        return;
    };
    if !member.aliasable {
        give_back(member.heap, member.charge);
    }
    objc::arc::release(member.heap);
}

/// `-[MTLResource heap]`.
pub(super) unsafe extern "C" fn resource_heap(receiver: Id, _sel: Sel) -> Id {
    MEMBERS
        .lock()
        .expect("heap members")
        .as_ref()
        .and_then(|members| members.get(&(receiver as usize)))
        .map_or(std::ptr::null_mut(), |member| member.heap)
}

/// `-[MTLResource isAliasable]`.
pub(super) unsafe extern "C" fn resource_is_aliasable(receiver: Id, _sel: Sel) -> bool {
    MEMBERS
        .lock()
        .expect("heap members")
        .as_ref()
        .and_then(|members| members.get(&(receiver as usize)))
        .is_some_and(|member| member.aliasable)
}

/// `-[MTLResource makeAliasable]`: the space is free for the heap's next
/// resource. A resource not made from a heap ignores it, as Metal's does.
pub(super) unsafe extern "C" fn resource_make_aliasable(receiver: Id, _sel: Sel) {
    let returned = {
        let mut members = MEMBERS.lock().expect("heap members");
        members
            .as_mut()
            .and_then(|members| members.get_mut(&(receiver as usize)))
            .filter(|member| !member.aliasable)
            .map(|member| {
                member.aliasable = true;
                (member.heap, member.charge)
            })
    };
    if let Some((heap, charge)) = returned {
        give_back(heap, charge);
    }
}

unsafe extern "C" fn heap_new_buffer(receiver: Id, _sel: Sel, length: u64, options: u64) -> Id {
    // SAFETY: a heap of this crate's.
    let heap = unsafe { object::body::<Heap>(receiver) };
    let quote = buffer_quote(length);
    if length == 0 || !reserve(heap, &quote) {
        return std::ptr::null_mut();
    }
    // The heap decides where the memory lives; the call's options only add
    // to that, so its storage and cache bits are the heap's.
    let options = (options & !0x3ff) | heap.descriptor.resource_options();
    let buffer = new_buffer(heap.gpu, length as usize, options);
    if buffer.is_null() {
        give_back(receiver, quote.size);
    } else {
        join(buffer, receiver, quote.size);
    }
    buffer
}

unsafe extern "C" fn heap_new_texture(receiver: Id, _sel: Sel, descriptor: Id) -> Id {
    // SAFETY: a heap of this crate's.
    let heap = unsafe { object::body::<Heap>(receiver) };
    if descriptor.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `MTLTextureDescriptor` is this crate's class.
    let mut descriptor = *unsafe { object::body::<TextureDescriptor>(descriptor) };
    let Some(quote) = texture_quote(&descriptor) else {
        return std::ptr::null_mut();
    };
    if !reserve(heap, &quote) {
        return std::ptr::null_mut();
    }
    descriptor.storage_mode = heap.descriptor.storage_mode;
    let texture = new_texture(heap.gpu, descriptor);
    if texture.is_null() {
        give_back(receiver, quote.size);
    } else {
        join(texture, receiver, quote.size);
    }
    texture
}

unsafe extern "C" fn heap_used_size(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a heap of this crate's.
    *unsafe { object::body::<Heap>(receiver) }
        .used
        .lock()
        .expect("heap books")
}

unsafe extern "C" fn heap_max_available(receiver: Id, _sel: Sel, alignment: u64) -> u64 {
    // SAFETY: a heap of this crate's.
    let heap = unsafe { object::body::<Heap>(receiver) };
    let used = *heap.used.lock().expect("heap books");
    let start = round_up(used, alignment.max(1));
    heap.descriptor.size.saturating_sub(start)
}

unsafe extern "C" fn heap_size(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a heap of this crate's.
    unsafe { object::body::<Heap>(receiver) }.descriptor.size
}

unsafe extern "C" fn heap_device(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a heap of this crate's.
    unsafe { object::body::<Heap>(receiver) }.device
}

unsafe extern "C" fn heap_value(receiver: Id, sel: Sel) -> u64 {
    // SAFETY: a heap of this crate's.
    let descriptor = &unsafe { object::body::<Heap>(receiver) }.descriptor;
    descriptor_field(descriptor, sel)
}

unsafe extern "C" fn heap_purgeable_state(_receiver: Id, _sel: Sel, _state: u64) -> u64 {
    MTL_PURGEABLE_STATE_NON_VOLATILE
}

unsafe extern "C" fn heap_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a heap of this crate's; its device was retained when it was made.
    objc::arc::release(unsafe { object::body::<Heap>(receiver) }.device);
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn heap_descriptor_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a freshly allocated heap descriptor.
    unsafe { object::init(receiver, HeapDescriptor::default()) };
    receiver
}

fn descriptor_field(descriptor: &HeapDescriptor, sel: Sel) -> u64 {
    // SAFETY: a selector the runtime owns.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"size" => descriptor.size,
        b"storageMode" => descriptor.storage_mode,
        b"cpuCacheMode" => descriptor.cpu_cache_mode,
        b"hazardTrackingMode" => descriptor.hazard_tracking_mode,
        b"type" => descriptor.heap_type,
        b"resourceOptions" => descriptor.resource_options(),
        _ => unreachable!("only registered heap getters reach this method"),
    }
}

unsafe extern "C" fn heap_descriptor_value(receiver: Id, sel: Sel) -> u64 {
    // SAFETY: a heap descriptor of this crate's.
    descriptor_field(unsafe { object::body::<HeapDescriptor>(receiver) }, sel)
}

unsafe extern "C" fn set_heap_descriptor_value(receiver: Id, sel: Sel, value: u64) {
    // SAFETY: a heap descriptor of this crate's.
    let descriptor = unsafe { object::body::<HeapDescriptor>(receiver) };
    // SAFETY: a selector the runtime owns.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"setSize:" => descriptor.size = value,
        b"setStorageMode:" => descriptor.storage_mode = value,
        b"setCpuCacheMode:" => descriptor.cpu_cache_mode = value,
        b"setHazardTrackingMode:" => descriptor.hazard_tracking_mode = value,
        b"setType:" => descriptor.heap_type = value,
        b"setResourceOptions:" => descriptor.set_resource_options(value),
        _ => unreachable!("only registered heap setters reach this method"),
    }
}

const GETTERS: [&str; 6] = [
    "size",
    "storageMode",
    "cpuCacheMode",
    "hazardTrackingMode",
    "type",
    "resourceOptions",
];

pub(super) fn install() {
    let descriptor = object::register::<HeapDescriptor>(c"MTLHeapDescriptor");
    methods!(descriptor, {
        "init" => heap_descriptor_init, c"@16@0:8";
    });
    for selector in GETTERS {
        descriptor.replace_method(
            objc::sel::get(selector),
            heap_descriptor_value as *const std::ffi::c_void,
            c"Q16@0:8",
        );
    }
    for selector in [
        "setSize:",
        "setStorageMode:",
        "setCpuCacheMode:",
        "setHazardTrackingMode:",
        "setType:",
        "setResourceOptions:",
    ] {
        descriptor.replace_method(
            objc::sel::get(selector),
            set_heap_descriptor_value as *const std::ffi::c_void,
            c"v24@0:8Q16",
        );
    }

    let heap = object::register::<Heap>(c"RosemaryMTLHeap");
    for selector in GETTERS {
        heap.replace_method(
            objc::sel::get(selector),
            heap_value as *const std::ffi::c_void,
            c"Q16@0:8",
        );
    }
    methods!(heap, {
        "size" => heap_size, c"Q16@0:8";
        "device" => heap_device, c"@16@0:8";
        "usedSize" => heap_used_size, c"Q16@0:8";
        "currentAllocatedSize" => heap_size, c"Q16@0:8";
        "maxAvailableSizeWithAlignment:" => heap_max_available, c"Q24@0:8Q16";
        "newBufferWithLength:options:" => heap_new_buffer, c"@32@0:8Q16Q24";
        "newTextureWithDescriptor:" => heap_new_texture, c"@24@0:8@16";
        "setPurgeableState:" => heap_purgeable_state, c"Q24@0:8Q16";
        "dealloc" => heap_dealloc, c"v16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(width: u64, height: u64, levels: u64) -> TextureDescriptor {
        TextureDescriptor {
            texture_type: 2,
            // MTLPixelFormatRGBA8Unorm.
            pixel_format: 70,
            width,
            height,
            depth: 1,
            mipmap_level_count: levels,
            sample_count: 1,
            array_length: 1,
            storage_mode: MTL_STORAGE_MODE_PRIVATE,
            usage: 0,
        }
    }

    #[test]
    fn a_texture_quote_covers_every_level() {
        let quote = texture_quote(&descriptor(256, 256, 9)).expect("a known format");
        // 4 bytes a texel; levels 256..1 sum to just over 4/3 of the base.
        let exact = (0..9).map(|l| (256u64 >> l).pow(2) * 4).sum::<u64>();
        assert_eq!(quote.size, round_up(exact, TEXTURE_ALIGNMENT));
        assert_eq!(quote.size % quote.align, 0);
    }

    #[test]
    fn a_heap_refuses_what_does_not_fit() {
        let quote = buffer_quote(600);
        assert_eq!(quote.size, 768);
        assert_eq!(fit(0, 1024, &quote), Some(768));
        assert_eq!(fit(768, 1024, &quote), None);
        // Alignment pads the start, and the padding counts against the heap:
        // 10 + 768 would fit in 1000, but 256 + 768 does not.
        assert_eq!(fit(10, 1000, &quote), None);
        assert_eq!(fit(10, 1024, &quote), Some(1024));
        assert_eq!(fit(10, 1024, &buffer_quote(1)), Some(512));
    }
}
