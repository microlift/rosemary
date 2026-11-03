//! Named memory entries: the handles `mach_make_memory_entry_64` makes on
//! memory, which `mach_vm_map` maps.
//!
//! An entry is a window on a host shared-memory object. Memory with no such
//! object behind it - a private anonymous mapping, above all - has nothing a
//! second mapping could share, so an entry sharing it is refused rather than
//! made from a copy.

use std::ffi::c_void;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::{Arc, Mutex, MutexGuard};

use super::kern::*;
use super::local::{self, VM_PROT_ALL, VM_PROT_IS_MASK, VM_PROT_WRITE};
use super::space::{self, MACH_PORT_NULL, Name, Right, Target};

/// The `MAP_MEM_*` half of a `mach_make_memory_entry_64` permission word. The
/// low byte is `VM_PROT_*` and the top byte a cache mode.
const MAP_MEM_FLAGS_MASK: i32 = 0x00ff_ff00;
const MAP_MEM_LEDGER_TAGGED: i32 = 0x00_2000;
const MAP_MEM_PURGABLE_KERNEL_ONLY: i32 = 0x00_4000;
const MAP_MEM_GRAB_SECLUDED: i32 = 0x00_8000;
const MAP_MEM_ONLY: i32 = 0x01_0000;
const MAP_MEM_NAMED_CREATE: i32 = 0x02_0000;
const MAP_MEM_PURGABLE: i32 = 0x04_0000;
const MAP_MEM_NAMED_REUSE: i32 = 0x08_0000;
const MAP_MEM_USE_DATA_ADDR: i32 = 0x10_0000;
const MAP_MEM_VM_COPY: i32 = 0x20_0000;
const MAP_MEM_VM_SHARE: i32 = 0x40_0000;
const MAP_MEM_4K_DATA_ADDR: i32 = 0x80_0000;
const MAP_MEM_FLAGS_USER: i32 = MAP_MEM_LEDGER_TAGGED
    | MAP_MEM_PURGABLE_KERNEL_ONLY
    | MAP_MEM_GRAB_SECLUDED
    | MAP_MEM_ONLY
    | MAP_MEM_NAMED_CREATE
    | MAP_MEM_PURGABLE
    | MAP_MEM_NAMED_REUSE
    | MAP_MEM_USE_DATA_ADDR
    | MAP_MEM_VM_COPY
    | MAP_MEM_VM_SHARE
    | MAP_MEM_4K_DATA_ADDR;
/// What this host cannot do: change a cache mode, keep a sub-page data
/// offset, or hand the kernel control of purging.
const MAP_MEM_UNSUPPORTED: i32 = MAP_MEM_ONLY
    | MAP_MEM_USE_DATA_ADDR
    | MAP_MEM_4K_DATA_ADDR
    | MAP_MEM_GRAB_SECLUDED
    | MAP_MEM_PURGABLE_KERNEL_ONLY;

/// `GET_MAP_MEM`: the cache mode, of which only `MAP_MEM_NOOP` (keep the
/// object's) is something a host mapping can be.
const MAP_MEM_SHIFT: u32 = 24;
const MAP_MEM_NOOP: i32 = 0;

/// The memory an entry is a window on.
pub struct Object {
    fd: OwnedFd,
}

impl Object {
    /// `len` zero bytes.
    fn new(len: u64) -> Result<Arc<Object>, i32> {
        let fd = starnix::memory::object(len).map_err(|_| KERN_RESOURCE_SHORTAGE)?;
        Ok(Arc::new(Object { fd }))
    }

    pub fn fd(&self) -> libc::c_int {
        self.fd.as_raw_fd()
    }
}

/// A mapping of `[offset, offset + len)` of `object`, unmapped on drop.
struct View {
    at: *mut u8,
    len: usize,
}

impl View {
    fn new(object: &Object, offset: u64, len: u64, prot: i32) -> Result<View, i32> {
        let offset = libc::off_t::try_from(offset).map_err(|_| KERN_INVALID_ARGUMENT)?;
        // SAFETY: a new shared mapping of a descriptor this runtime made.
        let at = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len as usize,
                prot,
                libc::MAP_SHARED,
                object.fd(),
                offset,
            )
        };
        match at {
            libc::MAP_FAILED => Err(KERN_RESOURCE_SHORTAGE),
            at => Ok(View {
                at: at.cast(),
                len: len as usize,
            }),
        }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        // SAFETY: the mapping `new` made, which nothing outlives.
        unsafe { libc::munmap(self.at.cast::<c_void>(), self.len) };
    }
}

/// What `mach_make_memory_entry_64` hands out a send right to.
pub struct Entry {
    pub object: Arc<Object>,
    /// Where the window starts in the object, and its length: whole pages.
    pub offset: u64,
    pub size: u64,
    /// The most a mapping of it may be, as `VM_PROT_*`.
    pub protection: i32,
    /// Made from a range with `MAP_MEM_VM_COPY` or `MAP_MEM_VM_SHARE`, which
    /// Mach will not take as a parent.
    is_copy: bool,
}

/// The entry `name` is a send right to. `Err` is what `mach_vm_map` answers
/// for a name that is not one.
pub fn lookup(name: Name) -> Result<Arc<Entry>, i32> {
    match space::with_right(name, |right| match right {
        Right::Memory { entry, .. } => Some(entry.clone()),
        _ => None,
    }) {
        Some(Some(entry)) => Ok(entry),
        Some(None) => Err(KERN_INVALID_OBJECT),
        None => Err(MACH_SEND_INVALID_RIGHT),
    }
}

/// A range of this task mapped from an object, which an entry can share.
struct Mapping {
    start: u64,
    end: u64,
    object: Arc<Object>,
    /// The object offset at `start`.
    offset: u64,
}

fn mappings() -> MutexGuard<'static, Vec<Mapping>> {
    static MAPPINGS: Mutex<Vec<Mapping>> = Mutex::new(Vec::new());
    MAPPINGS.lock().unwrap_or_else(|e| e.into_inner())
}

/// `[start, start + len)` now maps `object` from `offset`.
pub fn record(start: u64, len: u64, object: Arc<Object>, offset: u64) {
    let mut mappings = mappings();
    trim(&mut mappings, start, start + len);
    mappings.push(Mapping {
        start,
        end: start + len,
        object,
        offset,
    });
}

/// `[start, start + len)` no longer maps what [`record`] said it did.
pub fn forget(start: u64, len: u64) {
    trim(&mut mappings(), start, start.saturating_add(len));
}

/// Cut `[start, end)` out of every mapping, keeping what is either side.
fn trim(mappings: &mut Vec<Mapping>, start: u64, end: u64) {
    let mut kept = Vec::new();
    mappings.retain(|m| {
        if m.end <= start || m.start >= end {
            return true;
        }
        if m.start < start {
            kept.push(Mapping {
                start: m.start,
                end: start,
                object: m.object.clone(),
                offset: m.offset,
            });
        }
        if m.end > end {
            kept.push(Mapping {
                start: end,
                end: m.end,
                object: m.object.clone(),
                offset: m.offset + (end - m.start),
            });
        }
        false
    });
    mappings.extend(kept);
}

/// The object mapped at `start`, its offset there, where that mapping ends,
/// and the protection every page of `[start, start + len)` has now.
///
/// Checked against the host's map, because a raw `munmap` or a `MAP_FIXED`
/// over the range does not pass through [`forget`]: the host has to still
/// say it is a shared mapping at the same object offset.
fn mapped_at(start: u64, len: u64) -> Option<(Arc<Object>, u64, u64, i32)> {
    let (object, offset, mapped_end) = {
        let mappings = mappings();
        let m = mappings
            .iter()
            .find(|m| (m.start..m.end).contains(&start))?;
        (m.object.clone(), m.offset + (start - m.start), m.end)
    };
    let end = start + len.min(mapped_end - start);
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    let regions = starnix::process::regions().ok()?;
    let mut protection = VM_PROT_ALL;
    let mut covered = start;
    for region in regions.iter().filter(|r| r.end > start && r.start < end) {
        let expected = offset + (region.start.max(start) - start);
        let actual = region.offset + (region.start.max(start) - region.start);
        if region.start > covered || !region.shared || actual != expected {
            return None;
        }
        protection &= super::vm::protection(region);
        covered = region.end;
    }
    (covered >= end).then_some((object, offset, mapped_end, protection))
}

/// The object all of `[start, start + len)` maps, through one mapping of it,
/// and its offset at `start`. `None` for memory a second mapping cannot share.
pub fn shared_range(start: u64, len: u64) -> Option<(Arc<Object>, u64)> {
    let (object, offset, mapped_end, _) = mapped_at(start, len)?;
    (start + len <= mapped_end).then_some((object, offset))
}

/// Say once what was refused, because the alternative is a caller left with a
/// bare `KERN_INVALID_ARGUMENT` for a call that works on Darwin.
fn unsupported(why: std::fmt::Arguments) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| eprintln!("rosemary: mach_make_memory_entry_64: {why}"));
}

/// `kern_return_t mach_make_memory_entry_64(vm_map_t target_task,
/// memory_object_size_t *size, memory_object_offset_t offset, vm_prot_t
/// permission, mach_port_t *object_handle, mem_entry_name_port_t
/// parent_entry)`, and `mach_make_memory_entry`, the same call on a 64-bit
/// task.
///
/// # Safety
///
/// `size` is readable and writable, and `handle` writable.
pub unsafe extern "C" fn mach_make_memory_entry_64(
    task: Name,
    size: *mut u64,
    offset: u64,
    permission: i32,
    handle: *mut Name,
    parent: Name,
) -> i32 {
    if size.is_null() || handle.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    let flags = permission & MAP_MEM_FLAGS_MASK;
    if flags & !MAP_MEM_FLAGS_USER != 0 {
        return KERN_INVALID_VALUE;
    }
    let access = (permission as u32 >> MAP_MEM_SHIFT) as i32;
    if flags & MAP_MEM_UNSUPPORTED != 0 || access != MAP_MEM_NOOP {
        unsupported(format_args!(
            "permission {permission:#x} asks for a cache mode or flag this host has no equivalent of"
        ));
        return KERN_INVALID_ARGUMENT;
    }
    let target = match space::target(task) {
        Ok(target) => target,
        Err(code) => return code,
    };
    let parent = match parent {
        MACH_PORT_NULL => None,
        // Mach takes a name that is no entry as no parent at all.
        name => lookup(name).ok(),
    };
    if parent.as_ref().is_some_and(|parent| parent.is_copy) {
        return KERN_INVALID_ARGUMENT;
    }
    let request = Request {
        // SAFETY: the caller states `size` is readable.
        size: unsafe { *size },
        offset,
        protection: permission & VM_PROT_ALL,
        mask: permission & VM_PROT_IS_MASK != 0,
        flags,
    };
    let made = if flags & MAP_MEM_NAMED_CREATE != 0 {
        if request.size == 0 {
            // SAFETY: the caller states `handle` is writable.
            unsafe { *handle = MACH_PORT_NULL };
            return KERN_SUCCESS;
        }
        named_create(&request)
    } else if target != Target::Local {
        unsupported(format_args!(
            "an entry on another task's memory is not supported"
        ));
        Err(KERN_INVALID_ARGUMENT)
    } else if flags & MAP_MEM_VM_COPY != 0 {
        copy_of(&request)
    } else if let Some(parent) =
        parent.filter(|_| flags & (MAP_MEM_VM_SHARE | MAP_MEM_NAMED_REUSE) == 0)
    {
        within(&request, &parent)
    } else {
        // `MAP_MEM_NAMED_REUSE` is "reuse the parent if it is identical", and
        // a fresh entry on the same object is what a non-identical one gets.
        share(&request)
    };
    match made {
        Ok(entry) => {
            // SAFETY: the caller's out-parameters.
            unsafe {
                *size = entry.size;
                *handle = space::insert(Right::Memory {
                    entry: Arc::new(entry),
                    refs: 1,
                });
            }
            KERN_SUCCESS
        }
        Err(code) => code,
    }
}

/// The arguments every flavour of entry is made from.
struct Request {
    size: u64,
    offset: u64,
    protection: i32,
    mask: bool,
    flags: i32,
}

impl Request {
    /// The whole pages `[offset, offset + size)` covers.
    fn pages(&self) -> Result<(u64, u64), i32> {
        local::pages(self.offset, self.size).ok_or(KERN_INVALID_ARGUMENT)
    }
}

/// `MAP_MEM_NAMED_CREATE`: a new zero-filled object, as long as the range.
fn named_create(request: &Request) -> Result<Entry, i32> {
    let (_, len) = request.pages()?;
    // Purgeable memory starts out nonvolatile, which is ordinary memory. A
    // purge cannot be asked for without write access to it.
    if request.flags & MAP_MEM_PURGABLE != 0 && request.protection & VM_PROT_WRITE == 0 {
        return Err(KERN_INVALID_ARGUMENT);
    }
    Ok(Entry {
        object: Object::new(len)?,
        offset: 0,
        size: len,
        protection: request.protection,
        is_copy: false,
    })
}

/// `MAP_MEM_VM_COPY`: the range's bytes as they are now, in a new object.
fn copy_of(request: &Request) -> Result<Entry, i32> {
    if request.size == 0 {
        return Err(KERN_INVALID_ARGUMENT);
    }
    let (start, len) = request.pages()?;
    let object = Object::new(len)?;
    let view = View::new(&object, 0, len, libc::PROT_READ | libc::PROT_WRITE)?;
    // SAFETY: the view is `len` bytes of fresh, private memory.
    let into = unsafe { std::slice::from_raw_parts_mut(view.at, view.len) };
    local::read(start, into)?;
    drop(view);
    Ok(Entry {
        object,
        offset: 0,
        size: len,
        protection: request.protection,
        is_copy: true,
    })
}

/// No parent and no copy: the object this task maps at `offset`, as far as
/// that one mapping of it goes, or with `MAP_MEM_VM_SHARE` exactly the range.
fn share(request: &Request) -> Result<Entry, i32> {
    if request.size == 0 {
        return Err(KERN_INVALID_ARGUMENT);
    }
    let (start, len) = request.pages()?;
    let Some((object, offset, mapped_end, current)) = mapped_at(start, len) else {
        unsupported(format_args!(
            "{start:#x}+{len:#x} is not memory mapped from a memory entry, and this host \
             cannot share any other"
        ));
        return Err(KERN_INVALID_ARGUMENT);
    };
    let vm_share = request.flags & MAP_MEM_VM_SHARE != 0;
    if vm_share && start + len > mapped_end {
        unsupported(format_args!(
            "MAP_MEM_VM_SHARE of {start:#x}+{len:#x} spans more than one memory object"
        ));
        return Err(KERN_INVALID_ARGUMENT);
    }
    let len = len.min(mapped_end - start);
    let protection = match request.mask {
        true => request.protection & current,
        false => request.protection,
    };
    if (request.mask && protection == 0) || current & protection != protection {
        return Err(KERN_PROTECTION_FAILURE);
    }
    Ok(Entry {
        object,
        offset,
        size: len,
        protection,
        is_copy: vm_share,
    })
}

/// A parent and no flags: a window on the parent's window, `offset` into it.
fn within(request: &Request, parent: &Entry) -> Result<Entry, i32> {
    let (start, len) = request.pages()?;
    if start + len > parent.size {
        return Err(KERN_INVALID_ARGUMENT);
    }
    let protection = match request.mask {
        true => request.protection & parent.protection,
        false => request.protection,
    };
    if parent.protection & protection != protection {
        return Err(KERN_PROTECTION_FAILURE);
    }
    Ok(Entry {
        object: parent.object.clone(),
        offset: parent.offset + start,
        size: len,
        protection,
        is_copy: false,
    })
}

/// Copy `len` bytes of `entry` from `offset` into this task at `into`.
///
/// # Safety
///
/// `[into, into + len)` is memory the caller just mapped writable.
pub unsafe fn copy_out(entry: &Entry, offset: u64, into: u64, len: u64) -> Result<(), i32> {
    let view = View::new(&entry.object, entry.offset + offset, len, libc::PROT_READ)?;
    // SAFETY: the caller's obligation, and a view of `len` readable bytes.
    unsafe { std::ptr::copy_nonoverlapping(view.at, into as *mut u8, view.len) };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mach::local::{VM_FLAGS_ANYWHERE, VM_PROT_READ};
    use crate::mach::space::TASK_SELF;
    use crate::mach::vm::{mach_vm_map, vm_deallocate};

    const RW: i32 = VM_PROT_READ | VM_PROT_WRITE;
    /// `VM_INHERIT_COPY`, the default.
    const COPY: u32 = 1;

    fn page() -> u64 {
        crate::mach::info::page_size()
    }

    fn named(size: u64, permission: i32) -> Name {
        let (mut size, mut handle) = (size, MACH_PORT_NULL);
        // SAFETY: writable out-parameters.
        let rc = unsafe {
            mach_make_memory_entry_64(
                TASK_SELF,
                &mut size,
                0,
                permission | MAP_MEM_NAMED_CREATE,
                &mut handle,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
        handle
    }

    fn map(object: Name, offset: u64, size: u64, copy: u32, protection: i32) -> Result<u64, i32> {
        let mut address = 0;
        // SAFETY: a writable out-parameter.
        let rc = unsafe {
            mach_vm_map(
                TASK_SELF,
                &mut address,
                size,
                0,
                VM_FLAGS_ANYWHERE,
                object,
                offset,
                copy,
                protection,
                protection,
                COPY,
            )
        };
        match rc {
            KERN_SUCCESS => Ok(address),
            code => Err(code),
        }
    }

    fn entry_on(address: u64, size: u64, permission: i32, parent: Name) -> Result<Name, i32> {
        let (mut size, mut handle) = (size, MACH_PORT_NULL);
        // SAFETY: writable out-parameters.
        let rc = unsafe {
            mach_make_memory_entry_64(
                TASK_SELF,
                &mut size,
                address,
                permission,
                &mut handle,
                parent,
            )
        };
        match rc {
            KERN_SUCCESS => Ok(handle),
            code => Err(code),
        }
    }

    fn byte(address: u64) -> u8 {
        // SAFETY: inside a mapping the test made readable.
        unsafe { *(address as *const u8) }
    }

    fn set(address: u64, value: u8) {
        // SAFETY: inside a mapping the test made writable.
        unsafe { *(address as *mut u8) = value };
    }

    #[test]
    fn two_mappings_of_one_entry_share_their_pages() {
        let entry = named(2 * page(), RW);
        let first = map(entry, 0, 2 * page(), 0, RW).unwrap();
        let second = map(entry, page(), page(), 0, VM_PROT_READ).unwrap();
        assert_eq!(byte(second), 0);
        set(first + page() + 7, 0x5a);
        assert_eq!(byte(second + 7), 0x5a);
        unsafe {
            vm_deallocate(TASK_SELF, first, 2 * page());
            vm_deallocate(TASK_SELF, second, page());
        }
        assert_eq!(space::mach_port_deallocate(TASK_SELF, entry), KERN_SUCCESS);
    }

    #[test]
    fn a_copy_mapping_keeps_the_bytes_it_was_made_with() {
        let entry = named(page(), RW);
        let shared = map(entry, 0, page(), 0, RW).unwrap();
        set(shared, 1);
        let copy = map(entry, 0, page(), 1, RW).unwrap();
        set(shared, 2);
        set(copy + 1, 3);
        assert_eq!((byte(copy), byte(shared + 1)), (1, 0));
        unsafe {
            vm_deallocate(TASK_SELF, shared, page());
            vm_deallocate(TASK_SELF, copy, page());
        }
    }

    #[test]
    fn an_entry_made_from_a_mapping_shares_the_object_behind_it() {
        let entry = named(2 * page(), RW);
        let mapped = map(entry, 0, 2 * page(), 0, RW).unwrap();
        let again = entry_on(mapped + page(), page(), RW, MACH_PORT_NULL).unwrap();
        let alias = map(again, 0, page(), 0, RW).unwrap();
        set(alias, 9);
        assert_eq!(byte(mapped + page()), 9);
        unsafe {
            vm_deallocate(TASK_SELF, mapped, 2 * page());
            vm_deallocate(TASK_SELF, alias, page());
        }
    }

    #[test]
    fn a_child_entry_is_a_window_on_its_parent() {
        let parent = named(2 * page(), RW);
        let child = entry_on(page(), page(), VM_PROT_READ, parent).unwrap();
        let whole = map(parent, 0, 2 * page(), 0, RW).unwrap();
        let window = map(child, 0, page(), 0, VM_PROT_READ).unwrap();
        set(whole + page(), 4);
        assert_eq!(byte(window), 4);
        // Read-only, so neither a writable mapping of it nor a window past
        // the parent's end is to be had.
        assert_eq!(map(child, 0, page(), 0, RW), Err(KERN_INVALID_RIGHT));
        assert_eq!(
            entry_on(page(), 2 * page(), VM_PROT_READ, parent),
            Err(KERN_INVALID_ARGUMENT)
        );
        unsafe {
            vm_deallocate(TASK_SELF, whole, 2 * page());
            vm_deallocate(TASK_SELF, window, page());
        }
    }

    #[test]
    fn a_vm_copy_entry_is_a_snapshot_of_the_range() {
        let source = map(MACH_PORT_NULL, 0, page(), 0, RW).unwrap();
        set(source, 6);
        let snapshot = entry_on(source, page(), RW | MAP_MEM_VM_COPY, MACH_PORT_NULL).unwrap();
        set(source, 7);
        let mapped = map(snapshot, 0, page(), 0, VM_PROT_READ).unwrap();
        assert_eq!(byte(mapped), 6);
        unsafe {
            vm_deallocate(TASK_SELF, source, page());
            vm_deallocate(TASK_SELF, mapped, page());
        }
    }

    /// Private memory has no object a second mapping could share, so sharing
    /// it is refused rather than quietly copied.
    #[test]
    fn sharing_private_memory_is_refused() {
        let private = map(MACH_PORT_NULL, 0, page(), 0, RW).unwrap();
        assert_eq!(
            entry_on(private, page(), RW, MACH_PORT_NULL),
            Err(KERN_INVALID_ARGUMENT)
        );
        unsafe { vm_deallocate(TASK_SELF, private, page()) };
    }

    /// Once unmapped, a mapping is not an object an entry can be made from,
    /// even when the host puts something else at the same address.
    #[test]
    fn an_unmapped_mapping_is_no_longer_shareable() {
        let entry = named(page(), RW);
        let mapped = map(entry, 0, page(), 0, RW).unwrap();
        // SAFETY: the mapping just made.
        unsafe { libc::munmap(mapped as *mut c_void, page() as usize) };
        assert_eq!(
            entry_on(mapped, page(), RW, MACH_PORT_NULL),
            Err(KERN_INVALID_ARGUMENT)
        );
    }

    #[test]
    fn unknown_permission_flags_are_an_invalid_value() {
        // A bit inside `MAP_MEM_FLAGS_MASK` that no flag is.
        assert_eq!(
            entry_on(0, page(), RW | 0x100, MACH_PORT_NULL),
            Err(KERN_INVALID_VALUE)
        );
    }

    #[test]
    fn a_name_that_is_no_entry_is_an_invalid_object() {
        assert_eq!(
            map(space::HOST_SELF, 0, page(), 0, RW),
            Err(KERN_INVALID_OBJECT)
        );
    }
}
