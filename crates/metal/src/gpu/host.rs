//! Shared buffers whose bytes are ordinary, lazily committed host memory.
//!
//! A Metal shared buffer costs nothing until it is written: the kernel hands
//! out its pages on first touch. Vulkan host-visible memory is committed when
//! it is allocated, so a guest which makes buffers far larger than what it
//! writes into them pays for every byte. Roblox stages each texture upload
//! through a buffer sized for the whole texture - every mip of every slice -
//! and writes one slice into it; joining a place makes hundreds of them in one
//! frame, which held gigabytes for the length of that frame.
//!
//! So a large shared buffer's `-contents` is an anonymous mapping, and the
//! device sees it through `VK_EXT_external_memory_host` only as far as it has
//! to: a copy imports just the pages it reads or writes, and any other use -
//! a binding, whose extent no call states - imports the mapping whole, once.
//! An import pins its pages, so what stays uncommitted is exactly what no
//! work on the device has named.
//!
//! Every live import is also revalidated by the kernel on every submission
//! (amdgpu userptr objects), and making one costs about 100us. So a copy out
//! of pages the device has never been given does not import them at all: the
//! command buffer copies the bytes into its own upload memory when it is
//! committed (see `CommandBuffer::copy_source`).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use ash::vk;

use super::Gpu;
use super::guard::Guard;
use super::memory::BUFFER_USAGE;

/// The smallest shared buffer given host pages of its own. Below it a buffer
/// is cut from a heap block: an import is an allocation and a pinning call
/// apiece, and what a small buffer leaves unwritten is small.
pub const HOST_BACKED_MIN: u64 = 1 << 20;

/// Pieces imported for copies before a buffer is imported whole instead: a
/// guest which copies a buffer in many pieces would otherwise hold an import
/// for each.
const RANGE_IMPORTS: usize = 16;

const HANDLE: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT;

/// `VK_EXT_external_memory_host`, where the device has it.
pub(crate) struct HostImport {
    api: ash::ext::external_memory_host::Device,
    /// `minImportedHostPointerAlignment`: both ends of an import fall on it.
    alignment: u64,
}

impl HostImport {
    pub(super) fn new(
        instance: &ash::Instance,
        physical: vk::PhysicalDevice,
        device: &ash::Device,
    ) -> Option<Self> {
        let mut host = vk::PhysicalDeviceExternalMemoryHostPropertiesEXT::default();
        let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut host);
        // SAFETY: the extension is enabled on this device, and both
        // structures live across the query.
        unsafe { instance.get_physical_device_properties2(physical, &mut properties) };
        let alignment = host.min_imported_host_pointer_alignment;
        // A mapping is page aligned and no more; a device which wants more
        // keeps every buffer in heap blocks.
        (alignment.is_power_of_two() && alignment <= page_size()).then(|| Self {
            api: ash::ext::external_memory_host::Device::new(instance, device),
            alignment,
        })
    }
}

fn page_size() -> u64 {
    // SAFETY: sysconf has no preconditions.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as u64 }
}

/// One stretch of a mapping the device can see.
struct Import {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    start: u64,
    end: u64,
}

/// A shared buffer's host pages and the imports the device sees them by.
pub struct HostPages {
    base: *mut u8,
    /// The mapping's length: the buffer's, rounded up to the import alignment.
    size: u64,
    /// The whole-mapping import's `VkBuffer`, or null before there is one.
    /// Read on every binding, so it is not behind the lock.
    whole: AtomicU64,
    imports: Mutex<Vec<Import>>,
}

// SAFETY: `base` is a private mapping which lives as long as this value, and
// every thread holding the Metal object may write through it, as through
// Metal's own `-contents`. The imports are only changed under the lock.
unsafe impl Send for HostPages {}
unsafe impl Sync for HostPages {}

impl HostPages {
    pub fn contents(&self) -> *mut u8 {
        self.base
    }

    /// Whether the device has ever been given these pages. Imports last as
    /// long as the pages do, so once true this stays true.
    pub fn imported(&self) -> bool {
        self.whole.load(Ordering::Acquire) != 0
            || !self.imports.lock().expect("host imports").is_empty()
    }
}

impl Gpu {
    /// Host pages for a shared buffer of `length` bytes, or `None` where the
    /// buffer is better cut from a heap block: small, or on a device which
    /// cannot import host memory.
    pub fn create_host_pages(&self, length: u64) -> Option<HostPages> {
        let host = self.host_import.as_ref()?;
        if length < HOST_BACKED_MIN {
            return None;
        }
        let size = length.checked_next_multiple_of(host.alignment)?;
        let base =
            starnix::memory::anonymous(size as usize, libc::PROT_READ | libc::PROT_WRITE).ok()?;
        Some(HostPages {
            base: base.as_ptr(),
            size,
            whole: AtomicU64::new(0),
            imports: Mutex::new(Vec::new()),
        })
    }

    /// The buffer the device reads and writes `pages` through, for a use
    /// whose extent is unknown. Null, with the failure reported, where the
    /// driver refuses the import.
    pub fn host_buffer(&self, pages: &HostPages) -> vk::Buffer {
        let whole = pages.whole.load(Ordering::Acquire);
        if whole != 0 {
            return vk::Handle::from_raw(whole);
        }
        let mut imports = pages.imports.lock().expect("host imports");
        self.import_whole(pages, &mut imports)
    }

    /// The buffer and offset the device reaches `offset..offset + size` of
    /// `pages` by, for a copy. The caller has checked the range lies inside
    /// the buffer.
    pub fn host_range(&self, pages: &HostPages, offset: u64, size: u64) -> (vk::Buffer, u64) {
        let whole = pages.whole.load(Ordering::Acquire);
        if whole != 0 {
            return (vk::Handle::from_raw(whole), offset);
        }
        let Some(host) = self.host_import.as_ref() else {
            return (vk::Buffer::null(), offset);
        };
        let start = offset / host.alignment * host.alignment;
        let end = offset
            .saturating_add(size.max(1))
            .next_multiple_of(host.alignment)
            .min(pages.size);
        let mut imports = pages.imports.lock().expect("host imports");
        if let Some(import) = imports.iter().find(|i| i.start <= start && end <= i.end) {
            return (import.buffer, offset - import.start);
        }
        if imports.len() >= RANGE_IMPORTS {
            return (self.import_whole(pages, &mut imports), offset);
        }
        match self.import(pages, start, end) {
            Ok(import) => {
                let buffer = import.buffer;
                imports.push(import);
                (buffer, offset - start)
            }
            Err(reason) => {
                super::trace(format!("host buffer import failed: {reason}"));
                (vk::Buffer::null(), offset)
            }
        }
    }

    fn import_whole(&self, pages: &HostPages, imports: &mut Vec<Import>) -> vk::Buffer {
        let whole = pages.whole.load(Ordering::Acquire);
        if whole != 0 {
            return vk::Handle::from_raw(whole);
        }
        match self.import(pages, 0, pages.size) {
            Ok(import) => {
                let buffer = import.buffer;
                imports.push(import);
                pages
                    .whole
                    .store(vk::Handle::as_raw(buffer), Ordering::Release);
                buffer
            }
            Err(reason) => {
                super::trace(format!("host buffer import failed: {reason}"));
                vk::Buffer::null()
            }
        }
    }

    fn import(&self, pages: &HostPages, start: u64, end: u64) -> Result<Import, String> {
        let host = self.host_import.as_ref().ok_or("no host memory import")?;
        let size = end - start;
        // SAFETY: inside the mapping, which lives as long as `pages`.
        let pointer = unsafe { pages.base.add(start as usize) };
        let mut pointer_properties = vk::MemoryHostPointerPropertiesEXT::default();
        // SAFETY: an aligned pointer into a live mapping. ash 0.38 has no
        // wrapper for this entry point.
        unsafe {
            (host.api.fp().get_memory_host_pointer_properties_ext)(
                host.api.device(),
                HANDLE,
                pointer.cast(),
                &mut pointer_properties,
            )
        }
        .result()
        .map_err(|error| format!("vkGetMemoryHostPointerPropertiesEXT failed with {error:?}"))?;
        let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(HANDLE);
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(BUFFER_USAGE)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external);
        // SAFETY: a well-formed create info on this live device.
        let buffer = unsafe { self.device.create_buffer(&info, None) }
            .map_err(|error| format!("vkCreateBuffer failed with {error:?}"))?;
        let buffer = Guard::new(buffer, |buffer| {
            // SAFETY: a buffer of this device's which was never bound.
            unsafe { self.device.destroy_buffer(buffer, None) };
        });
        // SAFETY: the buffer was just created by this device.
        let requirements = unsafe { self.device.get_buffer_memory_requirements(*buffer) };
        if requirements.size > size {
            return Err(format!(
                "a {size}-byte buffer needs {} bytes",
                requirements.size
            ));
        }
        let bits = requirements.memory_type_bits & pointer_properties.memory_type_bits;
        let coherent =
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        // The guest writes through the mapping and never flushes, so the
        // memory must be coherent; cached is what anonymous memory already is.
        let index = self
            .memory_type(bits, coherent | vk::MemoryPropertyFlags::HOST_CACHED)
            .or_else(|| self.memory_type(bits, coherent))
            .ok_or("no coherent memory type admits host memory")?;
        let mut import = vk::ImportMemoryHostPointerInfoEXT::default()
            .handle_type(HANDLE)
            .host_pointer(pointer.cast());
        let allocate = vk::MemoryAllocateInfo::default()
            .allocation_size(size)
            .memory_type_index(index)
            .push_next(&mut import);
        // SAFETY: an aligned range of a live mapping, imported as the
        // pointer's own properties allow.
        let memory = unsafe { self.device.allocate_memory(&allocate, None) }
            .map_err(|error| format!("vkAllocateMemory (host pointer) failed with {error:?}"))?;
        let memory = Guard::new(memory, |memory| {
            // SAFETY: memory of this device's that nothing is bound to.
            unsafe { self.device.free_memory(memory, None) };
        });
        // SAFETY: a fresh, unbound buffer and memory exactly its size.
        unsafe { self.device.bind_buffer_memory(*buffer, *memory, 0) }
            .map_err(|error| format!("vkBindBufferMemory failed with {error:?}"))?;
        self.host_imported.fetch_add(size, Ordering::Relaxed);
        Ok(Import {
            buffer: buffer.take(),
            memory: memory.take(),
            start,
            end,
        })
    }

    /// Release every import of `pages` and unmap them. The caller must have
    /// waited for every submission which referred to them.
    pub fn destroy_host_pages(&self, pages: HostPages) {
        let imports = pages.imports.into_inner().expect("host imports");
        for import in imports {
            // SAFETY: the caller guarantees the device is done with it.
            unsafe {
                self.device.destroy_buffer(import.buffer, None);
                self.device.free_memory(import.memory, None);
            }
            self.host_imported
                .fetch_sub(import.end - import.start, Ordering::Relaxed);
        }
        // SAFETY: the mapping this value made, which nothing imports now.
        unsafe { libc::munmap(pages.base.cast(), pages.size as usize) };
    }
}
