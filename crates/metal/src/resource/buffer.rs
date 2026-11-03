use super::*;

/// Every `MTLBuffer` made and destroyed, with its
/// length, its `MTLResourceOptions` and how many are live.
#[inline(always)]
pub(super) fn tracing_buffers() -> bool {
    #[dynamic]
    static ON: bool = std::env::var_os("ROSEMARY_TRACE_BUFFERS").is_some();

    *ON
}

/// `MTLStorageModePrivate`, as `MTLResourceOptions` carries it in bits 4-7.
const STORAGE_MODE_PRIVATE: u64 = 2;

pub(crate) fn new_buffer(gpu: &'static Gpu, length: usize, options: u64) -> Id {
    if length == 0 {
        return std::ptr::null_mut();
    }
    crate::stats::peak(crate::stats::Counter::BufferPeakBytes, length as u64);
    if tracing_buffers() {
        eprintln!(
            "rosemary: Metal: newBuffer {length} options {options:#x} live {}",
            crate::live_buffers(1)
        );
    }
    let private = (options >> 4) & 0xf == STORAGE_MODE_PRIVATE;
    let host = match private {
        true => None,
        false => gpu.create_host_pages(length as u64),
    };
    let inner = match &host {
        // The device reaches these bytes by importing them when it uses them.
        Some(pages) => GpuBuffer {
            contents: pages.contents(),
            length: length as u64,
            ..GpuBuffer::none()
        },
        None => match crate::stats::timed(crate::stats::Counter::BufferMicros, || match private {
            true => gpu.create_private_buffer(length as u64),
            false => gpu.create_buffer(length as u64),
        }) {
            Ok(inner) => inner,
            Err(reason) => {
                gpu::trace(format!("newBufferWithLength: failed: {reason}"));
                return std::ptr::null_mut();
            }
        },
    };
    wrap_buffer(gpu, inner, options, host)
}

/// The `MTLBuffer` over a Vulkan buffer this crate made.
pub(crate) fn wrap_buffer(
    gpu: &'static Gpu,
    inner: GpuBuffer,
    options: u64,
    host: Option<gpu::HostPages>,
) -> Id {
    let length = inner.length as usize;
    crate::account(length as i64);
    if (options >> 4) & 0xf != STORAGE_MODE_PRIVATE {
        crate::add_shared_buffer_bytes(length as u64);
    }
    crate::stats::count(crate::stats::Counter::BuffersCreated);
    crate::stats::add(crate::stats::Counter::BufferBytes, length as u64);
    object::make(
        c"RosemaryMTLBuffer",
        Buffer {
            gpu,
            inner,
            options,
            host,
        },
    )
}

impl Buffer {
    /// The buffer the device sees all of this one through, for a use whose
    /// extent no call states.
    pub(crate) fn device_buffer(&self) -> vk::Buffer {
        match &self.host {
            Some(pages) => self.gpu.host_buffer(pages),
            None => self.inner.buffer,
        }
    }

    /// The buffer and offset the device sees `offset..offset + size` of this
    /// one through, for a copy. The caller has checked the range is inside.
    pub(crate) fn device_range(&self, offset: u64, size: u64) -> (vk::Buffer, u64) {
        match &self.host {
            Some(pages) => self.gpu.host_range(pages, offset, size),
            None => (self.inner.buffer, offset),
        }
    }
}

pub(super) unsafe extern "C" fn buffer_contents(receiver: Id, _sel: Sel) -> *mut u8 {
    // SAFETY: a buffer of this crate's.
    unsafe { object::body::<Buffer>(receiver) }.inner.contents
}

pub(super) unsafe extern "C" fn buffer_length(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: a buffer of this crate's.
    unsafe { object::body::<Buffer>(receiver) }.inner.length as usize
}

pub(super) unsafe extern "C" fn buffer_storage_mode(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a buffer of this crate's.
    (unsafe { object::body::<Buffer>(receiver) }.options >> 4) & 0xf
}

/// `-[MTLBuffer dealloc]`.
///
/// Without this every buffer a guest makes is a `VkBuffer` and a range of a
/// heap block held for the life of the process. A game which streams its world
/// in makes gigabytes of them in a second, so the leak is not a slow one: it
/// exhausts the adapter's memory during the first minute of play.
///
/// The last reference is gone, so nothing can bind it again, and a command
/// buffer which did bind it holds a reference of its own until the work it was
/// recorded into has completed.
/// TEMPORARY: `ROSEMARY_METAL_LEAK=1` keeps every device resource alive, to tell
/// a fault caused by freeing one too early from every other kind.
#[inline(always)]
pub(crate) fn leaking() -> bool {
    #[dynamic]
    static LEAK: bool = std::env::var_os("ROSEMARY_METAL_LEAK").is_some();

    *LEAK
}

pub(super) unsafe extern "C" fn buffer_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a buffer of this crate's.
    let buffer = unsafe { object::body::<Buffer>(receiver) };
    if tracing_buffers() {
        eprintln!(
            "rosemary: Metal: freeBuffer {} live {}",
            buffer.inner.length,
            crate::live_buffers(-1)
        );
    }
    crate::account(-(buffer.inner.length as i64));
    if !buffer.inner.private {
        crate::sub_shared_buffer_bytes(buffer.inner.length);
    }
    heap::leave(receiver);
    let inner = std::mem::replace(&mut buffer.inner, GpuBuffer::none());
    if !leaking() {
        match buffer.host.take() {
            Some(pages) => buffer.gpu.destroy_host_pages(pages),
            None => buffer.gpu.destroy_buffer(inner),
        }
    }
    objc::arc::finalize(receiver);
}
