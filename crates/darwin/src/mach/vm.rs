use std::ffi::c_void;
use std::sync::Arc;

use starnix::process::Region;

use server::task::{TaskOp, TaskOutcome};

use super::entry::{self, Entry, Object};
use super::kern::*;
use super::local::{
    self, Backing, VM_FLAGS_ANYWHERE, VM_FLAGS_OVERWRITE, VM_FLAGS_RANDOM_ADDR, VM_PROT_ALL,
    VM_PROT_EXECUTE, VM_PROT_IS_MASK, VM_PROT_READ, VM_PROT_WRITE,
};
use super::remote;
use super::space::{self, MACH_PORT_NULL, Name, Target};

/// `VM_REGION_BASIC_INFO_64`, the only flavour with a layout this tree can
/// promise. The 32-bit `VM_REGION_BASIC_INFO` is a different structure and is
/// refused rather than filled with this one.
const VM_REGION_BASIC_INFO_64: i32 = 9;
/// `VM_REGION_EXTENDED_INFO`, what `ps` and `vmmap` read sharing and residency
/// from.
const VM_REGION_EXTENDED_INFO: i32 = 10;
/// `VM_REGION_TOP_INFO`, what `top` sorts a task's memory into private and
/// shared with.
const VM_REGION_TOP_INFO: i32 = 12;

/// `SM_PRIVATE` and `SM_SHARED`, the two sharing modes a mapping here can be.
const SM_PRIVATE: u8 = 2;
const SM_SHARED: u8 = 3;

/// `VM_INHERIT_COPY`, which is `VM_INHERIT_DEFAULT` and what a private mapping
/// on this host does across a fork.
const VM_INHERIT_COPY: u32 = 1;
/// `VM_INHERIT_SHARE`.
const VM_INHERIT_SHARE: u32 = 0;
/// `VM_INHERIT_NONE`, which leaves the range out of a child, and is
/// `VM_INHERIT_LAST_VALID`.
const VM_INHERIT_NONE: u32 = 2;

/// `VM_BEHAVIOR_DEFAULT`.
const VM_BEHAVIOR_DEFAULT: i32 = 0;

/// `struct vm_region_basic_info_64`, whose layout is ABI: the guest declared
/// one on its stack and reads the fields back by name.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BasicInfo64 {
    protection: i32,
    max_protection: i32,
    inheritance: u32,
    shared: i32,
    reserved: i32,
    offset: u64,
    behavior: i32,
    user_wired_count: u16,
}

const _: () = {
    // `VM_REGION_BASIC_INFO_COUNT_64` is this size in 32-bit words, and a
    // caller passes it in as the capacity of its buffer.
    assert!(size_of::<BasicInfo64>() == 40);
};

/// `struct vm_region_top_info`. Resident page counts are not tracked and read
/// zero, as in the submap flavour below.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct TopInfo {
    obj_id: u32,
    ref_count: u32,
    private_pages_resident: u32,
    shared_pages_resident: u32,
    share_mode: u8,
}

const _: () = {
    // `VM_REGION_TOP_INFO_COUNT` is five words, and rounds the last byte up.
    assert!(size_of::<TopInfo>() == 20);
};

/// `struct vm_region_extended_info`, without the trailing `pages_reusable`
/// that newer SDKs append. Resident page counts are not tracked and read zero.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct ExtendedInfo {
    protection: i32,
    user_tag: u32,
    pages_resident: u32,
    pages_shared_now_private: u32,
    pages_swapped_out: u32,
    pages_dirtied: u32,
    ref_count: u32,
    shadow_depth: u16,
    external_pager: u8,
    share_mode: u8,
}

const _: () = {
    // 8 words, and `VM_REGION_EXTENDED_INFO_COUNT` is 9 with `pages_reusable`.
    assert!(size_of::<ExtendedInfo>() == 32);
};

/// The first mapping of `task` ending above `address`, which is Mach's own
/// search: the one containing it, or the next one up if there is a hole there.
/// `Ok(None)` past the last one.
fn region_at_or_after(task: Name, address: u64) -> Result<Option<Region>, i32> {
    Ok(regions(task)?
        .into_iter()
        .find(|region| region.end > address))
}

/// Every mapping of `task`, in address order.
///
/// Another task's map is the host's to say, and the server asks it: reading a
/// map needs no access to the memory behind it.
fn regions(task: Name) -> Result<Vec<Region>, i32> {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    match space::target(task)? {
        Target::Local => starnix::process::regions().map_err(|_| KERN_FAILURE),
        Target::Remote { pid, image } => match server::client::regions(pid, image) {
            Some(Some(regions)) => Ok(regions),
            Some(None) => {
                space::task_died(task);
                Err(MACH_SEND_INVALID_DEST)
            }
            None => Err(KERN_FAILURE),
        },
    }
}

/// [`region_at_or_after`] as the `vm_region` calls answer it: past the last
/// mapping is `KERN_INVALID_ADDRESS`, which is how a walk is told to stop.
fn region(task: Name, address: u64) -> Result<Region, i32> {
    region_at_or_after(task, address)?.ok_or(KERN_INVALID_ADDRESS)
}

/// A region's `VM_PROT_*` bits.
pub(super) fn protection(region: &Region) -> i32 {
    let mut protection = 0;
    if region.read {
        protection |= VM_PROT_READ;
    }
    if region.write {
        protection |= VM_PROT_WRITE;
    }
    if region.execute {
        protection |= VM_PROT_EXECUTE;
    }
    protection
}

/// Say once that a flavour went unanswered, because the alternative is a
/// caller quietly told its own task has no regions.
fn unsupported(flavor: i32) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosemary: vm_region_64: flavour {flavor} has a layout this tree does not \
             publish; only VM_REGION_BASIC_INFO_64 is answered"
        );
    });
}

/// `kern_return_t vm_region_64(vm_map_t target_task, vm_address_t *address,
/// vm_size_t *size, vm_region_flavor_t flavor, vm_region_info_t info,
/// mach_msg_type_number_t *infoCnt, mach_port_t *object_name)`
///
/// # Safety
///
/// `address` and `size` are writable words, `info` is writable for `*count`
/// 32-bit words, `count` is writable, and `object_name` is null or writable.
pub unsafe extern "C" fn vm_region_64(
    task: Name,
    address: *mut u64,
    size: *mut u64,
    flavor: i32,
    info: *mut c_void,
    count: *mut u32,
    object_name: *mut Name,
) -> i32 {
    if address.is_null() || size.is_null() || info.is_null() || count.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    if let Err(code) = space::target(task) {
        return code;
    }
    if flavor == VM_REGION_TOP_INFO {
        // SAFETY: the caller's obligations, as below.
        return unsafe { top_info(task, address, size, info, count, object_name) };
    }
    if flavor == VM_REGION_EXTENDED_INFO {
        // SAFETY: the caller's obligations, as below.
        return unsafe { extended_info(task, address, size, info, count, object_name) };
    }
    if flavor != VM_REGION_BASIC_INFO_64 {
        unsupported(flavor);
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller states `count` is readable.
    if (unsafe { *count } as usize) * size_of::<u32>() < size_of::<BasicInfo64>() {
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller states `address` is readable.
    let region = match region(task, unsafe { *address }) {
        Ok(region) => region,
        Err(code) => return code,
    };
    let basic = BasicInfo64 {
        protection: protection(&region),
        // Nothing here records what a mapping *could* be made, and reporting
        // less than the current protection would be a contradiction.
        max_protection: protection(&region),
        inheritance: match region.shared {
            true => VM_INHERIT_SHARE,
            false => VM_INHERIT_COPY,
        },
        shared: i32::from(region.shared),
        reserved: 0,
        offset: region.offset,
        behavior: VM_BEHAVIOR_DEFAULT,
        user_wired_count: 0,
    };
    // SAFETY: the caller's obligations, with the buffer's capacity checked
    // above. Unaligned because Mach's structures are packed to four bytes.
    unsafe {
        *address = region.start;
        *size = region.end - region.start;
        info.cast::<BasicInfo64>().write_unaligned(basic);
        *count = (size_of::<BasicInfo64>() / size_of::<u32>()) as u32;
        if !object_name.is_null() {
            // A name for the memory object behind the region. There is no port
            // namespace entry for one, and null is what Mach returns for a
            // region with no pager.
            *object_name = MACH_PORT_NULL;
        }
    }
    KERN_SUCCESS
}

/// [`vm_region_64`]'s `VM_REGION_TOP_INFO`.
///
/// # Safety
///
/// As [`vm_region_64`].
unsafe fn top_info(
    task: Name,
    address: *mut u64,
    size: *mut u64,
    info: *mut c_void,
    count: *mut u32,
    object_name: *mut Name,
) -> i32 {
    // SAFETY: the caller states `count` is readable.
    if (unsafe { *count } as usize) * size_of::<u32>() < size_of::<TopInfo>() {
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller states `address` is readable.
    let region = match region(task, unsafe { *address }) {
        Ok(region) => region,
        Err(code) => return code,
    };
    let top = TopInfo {
        ref_count: 1,
        share_mode: match region.shared {
            true => SM_SHARED,
            false => SM_PRIVATE,
        },
        ..TopInfo::default()
    };
    // SAFETY: the caller's obligations, with the capacity checked above.
    unsafe {
        *address = region.start;
        *size = region.end - region.start;
        info.cast::<TopInfo>().write_unaligned(top);
        *count = (size_of::<TopInfo>() / size_of::<u32>()) as u32;
        if !object_name.is_null() {
            *object_name = MACH_PORT_NULL;
        }
    }
    KERN_SUCCESS
}

/// [`vm_region_64`]'s `VM_REGION_EXTENDED_INFO`. A caller built against a newer
/// SDK offers one more word for `pages_reusable`, which is zeroed.
///
/// # Safety
///
/// As [`vm_region_64`].
unsafe fn extended_info(
    task: Name,
    address: *mut u64,
    size: *mut u64,
    info: *mut c_void,
    count: *mut u32,
    object_name: *mut Name,
) -> i32 {
    const WORDS: usize = size_of::<ExtendedInfo>() / size_of::<u32>();
    // SAFETY: the caller states `count` is readable.
    let capacity = unsafe { *count } as usize;
    if capacity < WORDS {
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller states `address` is readable.
    let region = match region(task, unsafe { *address }) {
        Ok(region) => region,
        Err(code) => return code,
    };
    let extended = ExtendedInfo {
        protection: protection(&region),
        ref_count: 1,
        share_mode: match region.shared {
            true => SM_SHARED,
            false => SM_PRIVATE,
        },
        ..ExtendedInfo::default()
    };
    let words = capacity.min(WORDS + 1);
    // SAFETY: the caller's obligations, with the capacity checked above.
    unsafe {
        *address = region.start;
        *size = region.end - region.start;
        if words > WORDS {
            info.cast::<u32>().add(WORDS).write_unaligned(0);
        }
        info.cast::<ExtendedInfo>().write_unaligned(extended);
        *count = words as u32;
        if !object_name.is_null() {
            *object_name = MACH_PORT_NULL;
        }
    }
    KERN_SUCCESS
}

/// `kern_return_t vm_region_recurse_64(vm_map_t target_task, vm_address_t
/// *address, vm_size_t *size, natural_t *nesting_depth,
/// vm_region_recurse_info_t info, mach_msg_type_number_t *infoCnt)`
///
/// Answers `struct vm_region_submap_info_64`, packed to four bytes, as far as
/// the caller's count reaches: the first sixteen words are every version's,
/// the seventeenth (`pages_reusable`) the newer ones'. There are no submaps
/// here, so every region is at depth zero and none is one. Page counts are
/// not tracked and read zero.
///
/// # Safety
///
/// `address`, `size`, `depth` and `count` are writable, and `info` is writable
/// for `*count` 32-bit words.
pub unsafe extern "C" fn vm_region_recurse_64(
    task: Name,
    address: *mut u64,
    size: *mut u64,
    depth: *mut u32,
    info: *mut u32,
    count: *mut u32,
) -> i32 {
    /// `VM_REGION_SUBMAP_INFO_V0_COUNT_64` and `_V1_`.
    const V0_WORDS: usize = 16;
    const V1_WORDS: usize = 17;
    if address.is_null() || size.is_null() || info.is_null() || count.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    if let Err(code) = space::target(task) {
        return code;
    }
    // SAFETY: the caller states `count` is readable.
    let capacity = unsafe { *count } as usize;
    if capacity < V0_WORDS {
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller states `address` is readable.
    let region = match region(task, unsafe { *address }) {
        Ok(region) => region,
        Err(code) => return code,
    };
    let words = capacity.min(V1_WORDS);
    let mut out = vec![0u32; words];
    out[0] = protection(&region) as u32;
    out[1] = protection(&region) as u32;
    out[2] = match region.shared {
        true => VM_INHERIT_SHARE,
        false => VM_INHERIT_COPY,
    };
    // `offset`, a 64-bit field at byte 12.
    out[3] = region.offset as u32;
    out[4] = (region.offset >> 32) as u32;
    // user_tag, pages_resident, _shared_now_private, _swapped_out, _dirtied.
    // ref_count, at word 10.
    out[10] = 1;
    // shadow_depth (u16), external_pager (u8), share_mode (u8) at word 11:
    // `SM_PRIVATE` is 1 and `SM_SHARED` 3.
    let share_mode: u32 = if region.shared { 3 } else { 1 };
    out[11] = share_mode << 24;
    // is_submap (12), behavior (13), object_id (14), user_wired_count (15).
    out[13] = VM_BEHAVIOR_DEFAULT as u32;
    // SAFETY: the caller's obligations, with `words` within its capacity.
    unsafe {
        *address = region.start;
        *size = region.end - region.start;
        if !depth.is_null() {
            *depth = 0;
        }
        std::ptr::copy_nonoverlapping(out.as_ptr(), info, words);
        *count = words as u32;
    }
    KERN_SUCCESS
}

/// Say once that `call` was asked for something it does not do, because the
/// alternative is a caller left guessing at a bare `KERN_INVALID_ARGUMENT`.
fn unsupported_flags(call: &str, flags: i32) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| eprintln!("rosemary: {call}: unsupported flags {flags:#x}"));
}

/// `kern_return_t vm_allocate(vm_map_t target_task, vm_address_t *address,
/// vm_size_t size, int flags)`, and `mach_vm_allocate`, which is the same call
/// on a 64-bit task.
///
/// Zero-filled anonymous memory, as Mach's is, in this task or another.
///
/// # Safety
///
/// `address` is readable and writable.
pub unsafe extern "C" fn vm_allocate(task: Name, address: *mut u64, size: u64, flags: i32) -> i32 {
    if address.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller states `address` is readable.
    let hint = unsafe { *address };
    let placed = match space::target(task) {
        Err(code) => Err(code),
        Ok(Target::Local) => local::allocate(hint, size, flags),
        Ok(Target::Remote { pid, image }) => {
            let op = TaskOp::Allocate {
                address: hint,
                size,
                flags,
            };
            match remote::perform(task, pid, image, op) {
                Ok(TaskOutcome::Address(placed)) => Ok(placed),
                Ok(_) => Err(KERN_FAILURE),
                Err(code) => Err(code),
            }
        }
    };
    match placed {
        Ok(placed) => {
            // SAFETY: the caller states `address` is writable.
            unsafe { *address = placed };
            KERN_SUCCESS
        }
        Err(KERN_INVALID_ARGUMENT) => {
            unsupported_flags("vm_allocate", flags);
            KERN_INVALID_ARGUMENT
        }
        Err(code) => code,
    }
}

/// `kern_return_t mach_vm_map(vm_map_t target_task, mach_vm_address_t
/// *address, mach_vm_size_t size, mach_vm_offset_t mask, int flags,
/// mem_entry_name_port_t object, memory_object_offset_t offset, boolean_t
/// copy, vm_prot_t cur_protection, vm_prot_t max_protection, vm_inherit_t
/// inheritance)`, and `vm_map` and `vm_map_64`, the same call on a 64-bit
/// task.
///
/// Zero-filled memory for `MACH_PORT_NULL`, or a named entry's object from
/// `offset`: shared with every other mapping of it, or with `copy` a private
/// copy of it as it is now. Placed as `vm_allocate` places memory, aligned to
/// `mask` when anywhere.
///
/// FIXME (ACCURACY): `max_protection` is checked against the entry and then
/// not kept, as in [`local::protect`]. And an entry's object mapped with
/// `VM_INHERIT_COPY` is shared with a child, not copied into it: a host has no
/// copy-on-fork of a shared mapping.
///
/// # Safety
///
/// `address` is readable and writable.
pub unsafe extern "C" fn mach_vm_map(
    task: Name,
    address: *mut u64,
    size: u64,
    mask: u64,
    flags: i32,
    object: Name,
    offset: u64,
    copy: u32,
    cur_protection: i32,
    max_protection: i32,
    inheritance: u32,
) -> i32 {
    if address.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    match space::target(task) {
        Err(code) => return code,
        Ok(Target::Local) => {}
        Ok(Target::Remote { .. }) => {
            static SAID: std::sync::Once = std::sync::Once::new();
            SAID.call_once(|| eprintln!("rosemary: mach_vm_map: another task is not supported"));
            return KERN_INVALID_ARGUMENT;
        }
    }
    let valid = VM_PROT_ALL | VM_PROT_IS_MASK;
    if cur_protection & !valid != 0 || max_protection & !valid != 0 {
        return KERN_INVALID_ARGUMENT;
    }
    if cur_protection & max_protection & VM_PROT_ALL != cur_protection & VM_PROT_ALL {
        return KERN_INVALID_ARGUMENT;
    }
    if inheritance > VM_INHERIT_NONE || size == 0 {
        return KERN_INVALID_ARGUMENT;
    }
    let page = super::info::page_size();
    let anywhere = flags & VM_FLAGS_ANYWHERE != 0;
    // SAFETY: the caller states `address` is readable.
    let hint = unsafe { *address };
    let placement = match anywhere {
        true => size.checked_next_multiple_of(page).map(|len| (0, len)),
        false => local::pages(hint, size),
    };
    let Some((start, length)) = placement else {
        return KERN_INVALID_ARGUMENT;
    };
    let mut protection = cur_protection & VM_PROT_ALL;
    let source = match object {
        MACH_PORT_NULL => None,
        name => {
            let entry = match entry::lookup(name) {
                Ok(entry) => entry,
                Err(code) => return code,
            };
            let mut max = max_protection & VM_PROT_ALL;
            if max_protection & VM_PROT_IS_MASK != 0 {
                max &= entry.protection;
            }
            if cur_protection & VM_PROT_IS_MASK != 0 {
                protection &= entry.protection;
            }
            if entry.protection & max != max || entry.protection & protection != protection {
                return KERN_INVALID_RIGHT;
            }
            // A mapping starts on a page of the object, which `mmap` insists
            // on and Mach's own callers always give.
            if !offset.is_multiple_of(page)
                || offset.checked_add(size).is_none_or(|end| end > entry.size)
            {
                return KERN_INVALID_ARGUMENT;
            }
            Some((entry, offset))
        }
    };
    let placed = match source {
        Some((entry, offset)) if copy == 0 => map_shared(
            start,
            length,
            mask,
            flags,
            &entry.object,
            entry.offset + offset,
        ),
        Some((entry, offset)) => map_copy(start, length, mask, flags, inheritance, &entry, offset),
        None => {
            let backing = match inheritance {
                VM_INHERIT_SHARE => Backing::Shared,
                _ => Backing::Private,
            };
            local::place(start, length, mask, flags, backing, libc::PROT_NONE)
        }
    };
    let placed = match placed {
        Ok(placed) => placed,
        Err(code) => {
            if code == KERN_INVALID_ARGUMENT {
                unsupported_flags("mach_vm_map", flags);
            }
            return code;
        }
    };
    let rc = finish(placed, length, protection, inheritance);
    if rc != KERN_SUCCESS {
        entry::forget(placed, length);
        // SAFETY: the mapping just made, which nothing else has been told of.
        unsafe { libc::munmap(placed as *mut std::ffi::c_void, length as usize) };
        return rc;
    }
    // SAFETY: the caller states `address` is writable.
    unsafe { *address = placed };
    KERN_SUCCESS
}

/// `_kernelrpc_mach_vm_map_trap`: [`mach_vm_map`] of zero-filled memory, with
/// every maximum protection and inherited by copy.
///
/// # Safety
///
/// As [`mach_vm_map`].
pub unsafe fn mach_vm_map_trap(
    task: Name,
    address: *mut u64,
    size: u64,
    mask: u64,
    flags: i32,
    cur_protection: i32,
) -> i32 {
    // SAFETY: the caller's obligation.
    unsafe {
        mach_vm_map(
            task,
            address,
            size,
            mask,
            flags,
            MACH_PORT_NULL,
            0,
            0,
            cur_protection,
            VM_PROT_ALL,
            VM_INHERIT_COPY,
        )
    }
}

/// `object` from `offset`, shared, and recorded so an entry can be made from
/// the mapping in turn.
fn map_shared(
    start: u64,
    length: u64,
    mask: u64,
    flags: i32,
    object: &Arc<Object>,
    offset: u64,
) -> Result<u64, i32> {
    let backing = Backing::Object {
        fd: object.fd(),
        offset,
    };
    let placed = local::place(start, length, mask, flags, backing, libc::PROT_NONE)?;
    entry::record(placed, length, object.clone(), offset);
    Ok(placed)
}

/// Fresh memory holding `entry`'s bytes from `offset` as they are now.
fn map_copy(
    start: u64,
    length: u64,
    mask: u64,
    flags: i32,
    inheritance: u32,
    entry: &Entry,
    offset: u64,
) -> Result<u64, i32> {
    let backing = match inheritance {
        VM_INHERIT_SHARE => Backing::Shared,
        _ => Backing::Private,
    };
    let writable = libc::PROT_READ | libc::PROT_WRITE;
    let placed = local::place(start, length, mask, flags, backing, writable)?;
    // Past the entry's end the copy is zero, as the rest of a fresh page is.
    let len = length.min(entry.size - offset);
    // SAFETY: the mapping just made writable.
    if let Err(code) = unsafe { entry::copy_out(entry, offset, placed, len) } {
        // SAFETY: as above, and nothing else has seen it.
        unsafe { libc::munmap(placed as *mut std::ffi::c_void, length as usize) };
        return Err(code);
    }
    Ok(placed)
}

/// Give a new mapping the protection and inheritance asked for. The
/// protection goes through [`local::protect`], so the runtime's hook sees an
/// executable mapping as it sees one from `mprotect`.
fn finish(placed: u64, length: u64, protection: i32, inheritance: u32) -> i32 {
    if inheritance == VM_INHERIT_NONE
        && starnix::memory::exclude_from_fork(placed as usize, length as usize).is_err()
    {
        return KERN_FAILURE;
    }
    local::protect(placed, length, false, protection)
}

/// `VM_FLAGS_*` only `mach_vm_remap` takes. The two `RESILIENT` flags are
/// about faults on code-signed and file-backed pages, which a host has none
/// of beyond what they also require of the mapping.
const VM_FLAGS_RESILIENT_CODESIGN: i32 = 0x0020;
const VM_FLAGS_RESILIENT_MEDIA: i32 = 0x0040;
const VM_FLAGS_RETURN_DATA_ADDR: i32 = 0x0010_0000;
/// `VM_FLAGS_USER_REMAP`, which unlike `mach_vm_map` takes no `VM_MAKE_TAG`.
const VM_FLAGS_USER_REMAP: i32 = VM_FLAGS_ANYWHERE
    | VM_FLAGS_RANDOM_ADDR
    | VM_FLAGS_OVERWRITE
    | VM_FLAGS_RETURN_DATA_ADDR
    | VM_FLAGS_RESILIENT_CODESIGN
    | VM_FLAGS_RESILIENT_MEDIA;

/// `kern_return_t mach_vm_remap(vm_map_t target_task, mach_vm_address_t
/// *target_address, mach_vm_size_t size, mach_vm_offset_t mask, int flags,
/// vm_map_read_t src_task, mach_vm_address_t src_address, boolean_t copy,
/// vm_prot_t *cur_protection, vm_prot_t *max_protection, vm_inherit_t
/// inheritance)`, and `vm_remap`, the same call on a 64-bit task.
///
/// The source's pages mapped again into this task: with `copy` a private copy
/// of them as they are now, and without it the same memory - which a host can
/// only give for memory mapped from a memory entry, see [`entry`]. The new
/// mapping has the strictest protection across the source, which is handed
/// back.
///
/// FIXME (ACCURACY): the maximum handed back is the current protection, as in
/// [`vm_region_64`]. A copy is read through the source's protection, so a page
/// that cannot be read fails it where Darwin copies the page regardless.
///
/// # Safety
///
/// `address` is readable and writable, and both protections writable.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn mach_vm_remap(
    task: Name,
    address: *mut u64,
    size: u64,
    mask: u64,
    flags: i32,
    source_task: Name,
    source: u64,
    copy: u32,
    cur_protection: *mut i32,
    max_protection: *mut i32,
    inheritance: u32,
) -> i32 {
    if address.is_null() || cur_protection.is_null() || max_protection.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    if flags & !VM_FLAGS_USER_REMAP != 0 {
        return KERN_INVALID_ARGUMENT;
    }
    match space::target(task) {
        Err(code) => return code,
        Ok(Target::Local) => {}
        Ok(Target::Remote { .. }) => {
            static SAID: std::sync::Once = std::sync::Once::new();
            SAID.call_once(|| {
                eprintln!("rosemary: mach_vm_remap: another target task is not supported")
            });
            return KERN_INVALID_ARGUMENT;
        }
    }
    let source_target = match space::target(source_task) {
        Ok(target) => target,
        Err(code) => return code,
    };
    let copy = copy != 0;
    if inheritance > VM_INHERIT_NONE || (flags & VM_FLAGS_RESILIENT_MEDIA != 0 && !copy) {
        return KERN_INVALID_ARGUMENT;
    }
    let page = super::info::page_size();
    let data_address = flags & VM_FLAGS_RETURN_DATA_ADDR != 0;
    // Without `RETURN_DATA_ADDR` Mach keeps its original rounding: `size` from
    // the start of the source's page, which can leave out the page its last
    // byte is on. Callers depend on that.
    let pages = match data_address {
        true => local::pages(source, size),
        false => local::pages(source & !(page - 1), size),
    };
    let Some((start, length)) = pages.filter(|&(_, length)| length != 0) else {
        return KERN_INVALID_ARGUMENT;
    };
    let mut protection = match strictest_protection(source_task, start, length) {
        Ok(protection) => protection,
        Err(code) => return code,
    };
    if flags & VM_FLAGS_RESILIENT_CODESIGN != 0 {
        protection = VM_PROT_READ;
    }
    // SAFETY: the caller states `address` is readable.
    let hint = unsafe { *address };
    let at = match flags & VM_FLAGS_ANYWHERE {
        0 => hint & !(page - 1),
        _ => 0,
    };
    let placement = flags & (VM_FLAGS_ANYWHERE | VM_FLAGS_RANDOM_ADDR | VM_FLAGS_OVERWRITE);
    let placed = match (copy, source_target) {
        (true, _) => remap_copy(source_task, start, length, at, mask, placement, inheritance),
        (false, Target::Local) => remap_shared(start, length, at, mask, placement),
        (false, Target::Remote { .. }) => {
            static SAID: std::sync::Once = std::sync::Once::new();
            SAID.call_once(|| {
                eprintln!("rosemary: mach_vm_remap: sharing another task's memory is not supported")
            });
            Err(KERN_INVALID_ARGUMENT)
        }
    };
    let placed = match placed {
        Ok(placed) => placed,
        Err(code) => return code,
    };
    let rc = finish(placed, length, protection, inheritance);
    if rc != KERN_SUCCESS {
        entry::forget(placed, length);
        // SAFETY: the mapping just made, which nothing else has been told of.
        unsafe { libc::munmap(placed as *mut c_void, length as usize) };
        return rc;
    }
    // SAFETY: the caller's out-parameters.
    unsafe {
        *address = match data_address {
            true => placed + (source & (page - 1)),
            false => placed,
        };
        *cur_protection = protection;
        *max_protection = protection;
    }
    KERN_SUCCESS
}

/// The protection every page of `[start, start + length)` of `task` has, or
/// `KERN_INVALID_ADDRESS` where one is not mapped.
fn strictest_protection(task: Name, start: u64, length: u64) -> Result<i32, i32> {
    let end = start + length;
    let mut strictest = VM_PROT_ALL;
    let mut covered = start;
    for region in regions(task)?
        .iter()
        .filter(|r| r.end > start && r.start < end)
    {
        if region.start > covered {
            return Err(KERN_INVALID_ADDRESS);
        }
        strictest &= protection(region);
        covered = region.end;
    }
    match covered >= end {
        true => Ok(strictest),
        false => Err(KERN_INVALID_ADDRESS),
    }
}

/// Fresh memory holding `[start, start + length)` of `task` as it is now.
///
/// Read before anything is placed, so an `OVERWRITE` over the source copies
/// what was there, and a source that cannot be read leaves the target alone.
fn remap_copy(
    task: Name,
    start: u64,
    length: u64,
    at: u64,
    mask: u64,
    flags: i32,
    inheritance: u32,
) -> Result<u64, i32> {
    let mut bytes = vec![0u8; length as usize];
    let rc = read_task(task, start, length, |offset, chunk| {
        bytes[offset as usize..][..chunk.len()].copy_from_slice(chunk);
        KERN_SUCCESS
    });
    if rc != KERN_SUCCESS {
        return Err(rc);
    }
    let backing = match inheritance {
        VM_INHERIT_SHARE => Backing::Shared,
        _ => Backing::Private,
    };
    let writable = libc::PROT_READ | libc::PROT_WRITE;
    let placed = local::place(at, length, mask, flags, backing, writable)?;
    // SAFETY: the mapping just made writable, `length` bytes long.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), placed as *mut u8, bytes.len()) };
    Ok(placed)
}

/// The memory object behind `[start, start + length)` of this task, mapped
/// again.
fn remap_shared(start: u64, length: u64, at: u64, mask: u64, flags: i32) -> Result<u64, i32> {
    let Some((object, offset)) = entry::shared_range(start, length) else {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| {
            eprintln!(
                "rosemary: mach_vm_remap: {start:#x}+{length:#x} is not mapped from one memory \
                 entry, and this host cannot share any other memory"
            )
        });
        return Err(KERN_INVALID_ARGUMENT);
    };
    map_shared(at, length, mask, flags, &object, offset)
}

/// `kern_return_t vm_deallocate(vm_map_t target_task, vm_address_t address,
/// vm_size_t size)`, over whole pages as Mach's is.
///
/// # Safety
///
/// `address` and `size` describe memory the task is done with.
pub unsafe extern "C" fn vm_deallocate(task: Name, address: u64, size: u64) -> i32 {
    match space::target(task) {
        Err(code) => code,
        Ok(Target::Local) => {
            if let Some((start, len)) = local::pages(address, size) {
                entry::forget(start, len);
            }
            // SAFETY: the caller's obligation.
            unsafe { local::deallocate(address, size) }
        }
        Ok(Target::Remote { pid, image }) => {
            remote::done(task, pid, image, TaskOp::Deallocate { address, size })
        }
    }
}

/// `kern_return_t vm_protect(vm_map_t target_task, vm_address_t address,
/// vm_size_t size, boolean_t set_maximum, vm_prot_t new_protection)`, and
/// `mach_vm_protect`, the same call on a 64-bit task.
pub unsafe extern "C" fn vm_protect(
    task: Name,
    address: u64,
    size: u64,
    set_maximum: u32,
    protection: i32,
) -> i32 {
    let set_maximum = set_maximum != 0;
    match space::target(task) {
        Err(code) => code,
        Ok(Target::Local) => local::protect(address, size, set_maximum, protection),
        Ok(Target::Remote { pid, image }) => remote::done(
            task,
            pid,
            image,
            TaskOp::Protect {
                address,
                size,
                set_maximum,
                protection,
            },
        ),
    }
}

/// Write `data` to this task's `address`, checked: the destination is the
/// caller's own buffer here, and a bad one is an error rather than a fault.
fn deliver(address: u64, data: &[u8]) -> i32 {
    // SAFETY: the caller named this buffer to be written.
    match unsafe { local::write(address, data) } {
        Ok(()) => KERN_SUCCESS,
        Err(code) => code,
    }
}

/// Read `size` bytes of `task` at `address`, a chunk at a time, handing each
/// to `each` with its offset. Stops at the first failure, all or nothing to
/// the caller.
fn read_task(task: Name, address: u64, size: u64, mut each: impl FnMut(u64, &[u8]) -> i32) -> i32 {
    if address.checked_add(size).is_none() {
        return KERN_INVALID_ADDRESS;
    }
    match space::target(task) {
        Err(code) => code,
        Ok(Target::Local) => {
            let mut buffer = [0u8; 4096];
            let mut done = 0;
            while done < size {
                let n = (size - done).min(buffer.len() as u64) as usize;
                if let Err(code) = local::read(address + done, &mut buffer[..n]) {
                    return code;
                }
                let rc = each(done, &buffer[..n]);
                if rc != KERN_SUCCESS {
                    return rc;
                }
                done += n as u64;
            }
            KERN_SUCCESS
        }
        Ok(Target::Remote { pid, image }) => {
            match remote::perform(task, pid, image, TaskOp::Read { address, size }) {
                Ok(TaskOutcome::Data(bytes)) if bytes.len() as u64 == size => each(0, &bytes),
                Ok(_) => KERN_FAILURE,
                Err(code) => code,
            }
        }
    }
}

/// `kern_return_t vm_read_overwrite(vm_map_t target_task, vm_address_t address,
/// vm_size_t size, vm_address_t data, vm_size_t *outsize)`, and
/// `mach_vm_read_overwrite`.
///
/// The task's memory into the caller's own buffer at `data`.
///
/// # Safety
///
/// `out_size` is null or writable.
pub unsafe extern "C" fn vm_read_overwrite(
    task: Name,
    address: u64,
    size: u64,
    data: u64,
    out_size: *mut u64,
) -> i32 {
    let rc = read_task(task, address, size, |offset, bytes| {
        deliver(data.wrapping_add(offset), bytes)
    });
    if rc == KERN_SUCCESS && !out_size.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *out_size = size };
    }
    rc
}

/// `kern_return_t vm_read(vm_map_read_t target_task, vm_address_t address,
/// vm_size_t size, vm_offset_t *data, mach_msg_type_number_t *dataCnt)`, and
/// `mach_vm_read`.
///
/// The task's memory, copied into fresh pages of the caller's that it frees
/// with `vm_deallocate(mach_task_self(), data, count)`. The copy starts at the
/// same offset into its first page as `address` does, which is where Mach's
/// out-of-line memory puts it.
///
/// # Safety
///
/// `data` and `count` are writable.
pub unsafe extern "C" fn vm_read(
    task: Name,
    address: u64,
    size: u64,
    data: *mut u64,
    count: *mut u32,
) -> i32 {
    if data.is_null() || count.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    // The count is a `mach_msg_type_number_t`.
    let Ok(length) = u32::try_from(size) else {
        return KERN_INVALID_ARGUMENT;
    };
    if let Err(code) = space::target(task) {
        return code;
    }
    if size == 0 {
        // SAFETY: the caller's out-parameters.
        unsafe {
            *data = 0;
            *count = 0;
        }
        return KERN_SUCCESS;
    }
    let (into, base) = match local::out_of_line(address, size) {
        Ok(pages) => pages,
        Err(code) => return code,
    };
    let rc = read_task(task, address, size, |offset, bytes| {
        // SAFETY: the fresh pages hold `size` bytes from `into`.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), into.add(offset as usize), bytes.len())
        };
        KERN_SUCCESS
    });
    if rc != KERN_SUCCESS {
        // SAFETY: the pages made above, which nobody has seen.
        unsafe { local::deallocate(base, into as u64 - base + size) };
        return rc;
    }
    // SAFETY: the caller's out-parameters.
    unsafe {
        *data = into as u64;
        *count = length;
    }
    KERN_SUCCESS
}

/// `kern_return_t vm_write(vm_map_t target_task, vm_address_t address,
/// vm_offset_t data, mach_msg_type_number_t dataCnt)`, and `mach_vm_write`.
///
/// `count` bytes of the caller's own memory at `data` into the task at
/// `address`: `KERN_INVALID_ADDRESS` where nothing is mapped there, and
/// `KERN_PROTECTION_FAILURE` where something is but cannot be written.
pub extern "C" fn vm_write(task: Name, address: u64, data: u64, count: u32) -> i32 {
    let size = u64::from(count);
    match space::target(task) {
        Err(code) => code,
        // The source is this task's memory as much as the destination is.
        // SAFETY: the guest named both ranges.
        Ok(Target::Local) => unsafe { local::copy(data, size, address) },
        Ok(Target::Remote { pid, image }) => {
            let mut bytes = vec![0u8; count as usize];
            if let Err(code) = local::read(data, &mut bytes) {
                return code;
            }
            let op = TaskOp::Write {
                address,
                data: bytes,
            };
            remote::done(task, pid, image, op)
        }
    }
}

/// `kern_return_t vm_copy(vm_map_t target_task, vm_address_t source_address,
/// vm_size_t size, vm_address_t dest_address)`, and `mach_vm_copy`.
///
/// Darwin takes the source as it was before the call, so an overlapping copy
/// is a `memmove`. jemalloc uses it to move page runs in `realloc`.
pub extern "C" fn vm_copy(task: Name, source: u64, size: u64, dest: u64) -> i32 {
    match space::target(task) {
        Err(code) => code,
        // SAFETY: the guest named both ranges, in its own task.
        Ok(Target::Local) => unsafe { local::copy(source, size, dest) },
        Ok(Target::Remote { pid, image }) => {
            remote::done(task, pid, image, TaskOp::Copy { source, size, dest })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mach::info::page_size;
    use crate::mach::space::{HOST_SELF, TASK_SELF};

    fn map(prot: i32) -> u64 {
        let mut address = 0_u64;
        // SAFETY: a writable out-parameter.
        assert_eq!(
            unsafe { vm_allocate(TASK_SELF, &mut address, 2 * 4096, VM_FLAGS_ANYWHERE) },
            KERN_SUCCESS
        );
        assert_eq!(
            unsafe { vm_protect(TASK_SELF, address, 2 * 4096, 0, prot) },
            KERN_SUCCESS
        );
        address
    }

    /// The call names the region a known address is in, which is the whole of
    /// what a memory walk reads out of it.
    #[test]
    fn names_the_region_an_address_is_in() {
        let page = vec![0_u8; 4096];
        let mut address = page.as_ptr() as u64;
        let mut size = 0_u64;
        let mut info = [0_u32; 16];
        let mut count = (size_of::<BasicInfo64>() / size_of::<u32>()) as u32;
        let mut object = 1_u32;
        // SAFETY: writable out-parameters, and a buffer whose capacity is what
        // `count` says.
        let status = unsafe {
            vm_region_64(
                TASK_SELF,
                &mut address,
                &mut size,
                VM_REGION_BASIC_INFO_64,
                info.as_mut_ptr().cast(),
                &mut count,
                &mut object,
            )
        };
        assert_eq!(status, KERN_SUCCESS);
        assert!(address <= page.as_ptr() as u64);
        assert!(address + size > page.as_ptr() as u64);
        assert_eq!(object, MACH_PORT_NULL);
        // SAFETY: the call above filled it.
        let basic = unsafe { info.as_ptr().cast::<BasicInfo64>().read_unaligned() };
        assert_eq!(basic.protection & (VM_PROT_READ | VM_PROT_WRITE), 3);
    }

    /// Past the last mapping the walk is told to stop, rather than being handed
    /// a region that is not there.
    #[test]
    fn the_end_of_the_map_is_an_invalid_address() {
        let mut address = u64::MAX - 4096;
        let mut size = 0_u64;
        let mut info = [0_u32; 16];
        let mut count = (size_of::<BasicInfo64>() / size_of::<u32>()) as u32;
        // SAFETY: writable out-parameters; `object_name` is allowed to be null.
        let status = unsafe {
            vm_region_64(
                TASK_SELF,
                &mut address,
                &mut size,
                VM_REGION_BASIC_INFO_64,
                info.as_mut_ptr().cast(),
                &mut count,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, KERN_INVALID_ADDRESS);
    }

    /// A name that holds nothing is refused as Darwin's MIG refuses a send to
    /// it, rather than answered about with this task's map.
    #[test]
    fn a_name_that_is_no_task_is_refused() {
        let mut address = 0_u64;
        let mut size = 0_u64;
        let mut info = [0_u32; 16];
        let mut count = 16_u32;
        // SAFETY: writable out-parameters.
        let status = unsafe {
            vm_region_64(
                TASK_SELF + 4,
                &mut address,
                &mut size,
                VM_REGION_BASIC_INFO_64,
                info.as_mut_ptr().cast(),
                &mut count,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, MACH_SEND_INVALID_DEST);
        // A right to something else is the wrong argument.
        assert_eq!(vm_copy(HOST_SELF, 0, 0, 0), KERN_INVALID_ARGUMENT);
    }

    /// A read of this task's own memory is a copy.
    #[test]
    fn reads_this_tasks_own_memory() {
        let from = [1_u8, 2, 3, 4];
        let mut into = [0_u8; 4];
        let mut wrote = 0_u64;
        // SAFETY: two live, non-overlapping buffers of the stated length.
        let status = unsafe {
            vm_read_overwrite(
                TASK_SELF,
                from.as_ptr() as u64,
                4,
                into.as_mut_ptr() as u64,
                &mut wrote,
            )
        };
        assert_eq!(status, KERN_SUCCESS);
        assert_eq!(into, from);
        assert_eq!(wrote, 4);
    }

    /// An overlapping copy sees the source as it was before the call.
    #[test]
    fn copies_overlapping_memory_like_memmove() {
        let mut buf = [1_u8, 2, 3, 4, 5, 6];
        let base = buf.as_mut_ptr() as u64;
        // SAFETY: both ranges lie inside `buf`.
        let status = vm_copy(TASK_SELF, base, 4, base + 2);
        assert_eq!(status, KERN_SUCCESS);
        assert_eq!(buf, [1, 2, 1, 2, 3, 4]);
    }

    /// `mach_vm_read` hands the bytes back in fresh pages, at the same offset
    /// into the first one as the address read, which `vm_deallocate` frees.
    #[test]
    fn reads_into_fresh_pages_the_caller_frees() {
        let from = map(VM_PROT_READ | VM_PROT_WRITE);
        let marker = b"rosemary";
        // SAFETY: inside the two pages just mapped.
        unsafe { std::ptr::copy_nonoverlapping(marker.as_ptr(), (from + 4090) as *mut u8, 8) };
        let (mut data, mut count) = (0_u64, 0_u32);
        assert_eq!(
            unsafe { vm_read(TASK_SELF, from + 4090, 8, &mut data, &mut count) },
            KERN_SUCCESS
        );
        assert_eq!(count, 8);
        assert_eq!(data % 4096, 4090 % 4096);
        // SAFETY: the 8 bytes just handed back.
        assert_eq!(
            unsafe { std::slice::from_raw_parts(data as *const u8, 8) },
            marker
        );
        assert_eq!(unsafe { vm_deallocate(TASK_SELF, data, 8) }, KERN_SUCCESS);
        unsafe { vm_deallocate(TASK_SELF, from, 2 * 4096) };
    }

    /// A read of a hole is an invalid address, and of an unreadable page a
    /// protection failure - and neither faults.
    #[test]
    fn bad_reads_fail_with_darwins_codes() {
        let base = map(VM_PROT_READ | VM_PROT_WRITE);
        assert_eq!(
            unsafe { vm_protect(TASK_SELF, base + 4096, 4096, 0, 0) },
            KERN_SUCCESS
        );
        let (mut data, mut count) = (0_u64, 0_u32);
        assert_eq!(
            unsafe { vm_read(TASK_SELF, base + 4000, 200, &mut data, &mut count) },
            KERN_PROTECTION_FAILURE
        );
        unsafe { vm_deallocate(TASK_SELF, base, 2 * 4096) };
        // The first page is never mapped, so no other test can be there.
        let mut into = [0_u8; 4];
        assert_eq!(
            unsafe {
                vm_read_overwrite(
                    TASK_SELF,
                    16,
                    4,
                    into.as_mut_ptr() as u64,
                    std::ptr::null_mut(),
                )
            },
            KERN_INVALID_ADDRESS
        );
    }

    /// A write lands where it can and is refused where the page is read-only.
    #[test]
    fn writes_this_tasks_memory_unless_it_is_read_only() {
        let base = map(VM_PROT_READ | VM_PROT_WRITE);
        let data = *b"written";
        assert_eq!(
            vm_write(TASK_SELF, base + 10, data.as_ptr() as u64, 7),
            KERN_SUCCESS
        );
        // SAFETY: inside the mapping.
        assert_eq!(
            unsafe { std::slice::from_raw_parts((base + 10) as *const u8, 7) },
            &data
        );
        assert_eq!(
            unsafe { vm_protect(TASK_SELF, base, 4096, 0, VM_PROT_READ) },
            KERN_SUCCESS
        );
        assert_eq!(
            vm_write(TASK_SELF, base + 10, data.as_ptr() as u64, 7),
            KERN_PROTECTION_FAILURE
        );
        unsafe { vm_deallocate(TASK_SELF, base, 2 * 4096) };
        assert_eq!(
            vm_write(TASK_SELF, 16, data.as_ptr() as u64, 7),
            KERN_INVALID_ADDRESS
        );
    }

    /// The buffer Crashpad's message server receives into.
    #[test]
    fn allocates_zeroed_memory_anywhere_and_gives_it_back() {
        let mut address = 0_u64;
        // `VM_FLAGS_ANYWHERE | VM_MAKE_TAG(VM_MEMORY_MACH_MSG)`.
        // SAFETY: a writable out-parameter.
        let status = unsafe { vm_allocate(TASK_SELF, &mut address, 100, 0x1400_0001) };
        assert_eq!(status, KERN_SUCCESS);
        assert_ne!(address, 0);
        assert_eq!(address % super::super::info::page_size(), 0);
        // SAFETY: the 100 bytes just allocated.
        let bytes = unsafe { std::slice::from_raw_parts_mut(address as *mut u8, 100) };
        assert!(bytes.iter().all(|byte| *byte == 0));
        bytes[99] = 1;
        // SAFETY: the allocation above.
        assert_eq!(
            unsafe { vm_deallocate(TASK_SELF, address, 100) },
            KERN_SUCCESS
        );
    }

    /// A fixed request over memory already in use is refused, not moved.
    #[test]
    fn a_fixed_allocation_over_a_mapping_is_no_space() {
        let mut taken = 0_u64;
        // SAFETY: a writable out-parameter.
        assert_eq!(
            unsafe { vm_allocate(TASK_SELF, &mut taken, 4096, VM_FLAGS_ANYWHERE) },
            KERN_SUCCESS
        );
        let mut address = taken;
        // SAFETY: a writable out-parameter.
        let status = unsafe { vm_allocate(TASK_SELF, &mut address, 4096, 0) };
        assert_eq!(status, KERN_NO_SPACE);
        assert_eq!(address, taken);
        // SAFETY: the allocation above.
        unsafe { vm_deallocate(TASK_SELF, taken, 4096) };
    }

    fn vm_map(address: &mut u64, size: u64, mask: u64, flags: i32, cur: i32, max: i32) -> i32 {
        // SAFETY: a writable out-parameter.
        unsafe {
            mach_vm_map(
                TASK_SELF,
                address,
                size,
                mask,
                flags,
                MACH_PORT_NULL,
                0,
                0,
                cur,
                max,
                VM_INHERIT_COPY,
            )
        }
    }

    /// A mask is an alignment when anywhere: an allocator asking for a
    /// megabyte-aligned arena gets one, zero-filled.
    #[test]
    fn maps_zeroed_memory_aligned_to_the_mask() {
        const MASK: u64 = (1 << 20) - 1;
        let rw = VM_PROT_READ | VM_PROT_WRITE;
        let mut address = 0;
        let size = 3 * page_size();
        let rc = vm_map(&mut address, size, MASK, VM_FLAGS_ANYWHERE, rw, VM_PROT_ALL);
        assert_eq!(rc, KERN_SUCCESS);
        assert_eq!(address & MASK, 0);
        // SAFETY: the mapping just made, readable and writable.
        let bytes = unsafe { std::slice::from_raw_parts_mut(address as *mut u8, size as usize) };
        assert!(bytes.iter().all(|byte| *byte == 0));
        bytes[size as usize - 1] = 1;
        unsafe { vm_deallocate(TASK_SELF, address, size) };
    }

    /// The requested protection is the mapping's, not `vm_allocate`'s
    /// read-write default.
    #[test]
    fn a_mapping_has_the_protection_asked_for() {
        let mut address = 0;
        let rc = vm_map(
            &mut address,
            1,
            0,
            VM_FLAGS_ANYWHERE,
            VM_PROT_READ,
            VM_PROT_ALL,
        );
        assert_eq!(rc, KERN_SUCCESS);
        let data = [1_u8];
        assert_eq!(
            vm_write(TASK_SELF, address, data.as_ptr() as u64, 1),
            KERN_PROTECTION_FAILURE
        );
        unsafe { vm_deallocate(TASK_SELF, address, page_size()) };
    }

    #[test]
    fn a_fixed_address_off_the_mask_is_no_space() {
        let mut address = 0x7000_0000_0000 + page_size();
        let rc = vm_map(
            &mut address,
            page_size(),
            (1 << 20) - 1,
            0,
            VM_PROT_READ,
            VM_PROT_ALL,
        );
        assert_eq!(rc, KERN_NO_SPACE);
    }

    #[test]
    fn a_protection_above_the_maximum_or_an_empty_range_is_refused() {
        let mut address = 0;
        let rw = VM_PROT_READ | VM_PROT_WRITE;
        let anywhere = VM_FLAGS_ANYWHERE;
        assert_eq!(
            vm_map(&mut address, page_size(), 0, anywhere, rw, VM_PROT_READ),
            KERN_INVALID_ARGUMENT
        );
        assert_eq!(
            vm_map(&mut address, 0, 0, anywhere, rw, rw),
            KERN_INVALID_ARGUMENT
        );
    }
    /// `mach_vm_remap` of `size` bytes at `source` in this task, anywhere.
    fn remap(source: u64, size: u64, flags: i32, copy: u32) -> Result<(u64, i32), i32> {
        let (mut address, mut cur, mut max) = (0, 0, 0);
        // SAFETY: writable out-parameters.
        let rc = unsafe {
            mach_vm_remap(
                TASK_SELF,
                &mut address,
                size,
                0,
                VM_FLAGS_ANYWHERE | flags,
                TASK_SELF,
                source,
                copy,
                &mut cur,
                &mut max,
                VM_INHERIT_COPY,
            )
        };
        match rc {
            KERN_SUCCESS => Ok((address, cur)),
            code => Err(code),
        }
    }

    /// A copy holds the source's bytes as they were, with the source's
    /// protection, and is no longer tied to it.
    #[test]
    fn a_remapped_copy_keeps_the_bytes_it_was_made_with() {
        let source = map(VM_PROT_READ | VM_PROT_WRITE);
        // SAFETY: the readable, writable mapping above.
        unsafe { *((source + 5) as *mut u8) = 7 };
        let (copy, cur) = remap(source, 2 * 4096, 0, 1).unwrap();
        assert_eq!(cur, VM_PROT_READ | VM_PROT_WRITE);
        // SAFETY: both mappings are readable and writable.
        unsafe {
            *((source + 5) as *mut u8) = 9;
            assert_eq!(*((copy + 5) as *const u8), 7);
            vm_deallocate(TASK_SELF, source, 2 * 4096);
            vm_deallocate(TASK_SELF, copy, 2 * 4096);
        }
    }

    /// The legacy rounding covers `size` from the source's page, and
    /// `RETURN_DATA_ADDR` covers the bytes asked for and points at the first.
    #[test]
    fn a_remap_rounds_from_the_page_unless_asked_for_the_data_address() {
        let page = page_size();
        let source = map(VM_PROT_READ | VM_PROT_WRITE);
        let start = source + page - 1;
        // SAFETY: inside the mapping above.
        unsafe { *(start as *mut u8) = 3 };
        let (legacy, _) = remap(start, 2, 0, 1).unwrap();
        assert_eq!(legacy % page, 0);
        let (data, _) = remap(start, 2, VM_FLAGS_RETURN_DATA_ADDR, 1).unwrap();
        assert_eq!(data % page, page - 1);
        // SAFETY: the copies above, the second of which spans two pages.
        unsafe {
            assert_eq!(*(data as *const u8), 3);
            assert_eq!(*((data + 1) as *const u8), 0);
            vm_deallocate(TASK_SELF, legacy, page);
            vm_deallocate(TASK_SELF, data - (page - 1), 2 * page);
            vm_deallocate(TASK_SELF, source, 2 * 4096);
        }
    }

    /// Without `copy`, memory mapped from an entry is the same memory twice.
    #[test]
    fn a_shared_remap_of_an_entry_mapping_aliases_it() {
        let rw = VM_PROT_READ | VM_PROT_WRITE;
        let (mut size, mut entry) = (page_size(), MACH_PORT_NULL);
        // `MAP_MEM_NAMED_CREATE`.
        let named_create = 0x2_0000;
        // SAFETY: writable out-parameters.
        let rc = unsafe {
            entry::mach_make_memory_entry_64(
                TASK_SELF,
                &mut size,
                0,
                rw | named_create,
                &mut entry,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
        let mut first = 0;
        // SAFETY: a writable out-parameter.
        let rc = unsafe {
            mach_vm_map(
                TASK_SELF,
                &mut first,
                size,
                0,
                VM_FLAGS_ANYWHERE,
                entry,
                0,
                0,
                rw,
                rw,
                VM_INHERIT_COPY,
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
        let (second, cur) = remap(first, size, 0, 0).unwrap();
        assert_eq!(cur, rw);
        // SAFETY: both mappings are readable and writable.
        unsafe {
            *((first + 3) as *mut u8) = 0x5a;
            assert_eq!(*((second + 3) as *const u8), 0x5a);
            vm_deallocate(TASK_SELF, first, size);
            vm_deallocate(TASK_SELF, second, size);
        }
        assert_eq!(space::mach_port_deallocate(TASK_SELF, entry), KERN_SUCCESS);
    }

    /// Private memory has no object a second mapping could share, and the
    /// call says so rather than handing back a copy.
    #[test]
    fn a_shared_remap_of_private_memory_is_refused() {
        let source = map(VM_PROT_READ | VM_PROT_WRITE);
        assert_eq!(remap(source, 4096, 0, 0), Err(KERN_INVALID_ARGUMENT));
        unsafe { vm_deallocate(TASK_SELF, source, 2 * 4096) };
    }

    #[test]
    fn a_remap_of_unmapped_memory_or_with_a_tag_is_refused() {
        // The first page above zero, which no host lets anything map.
        let unmapped = page_size();
        assert_eq!(remap(unmapped, 1, 0, 1), Err(KERN_INVALID_ADDRESS));
        let buffer = vec![0u8; 4096];
        // `VM_MAKE_TAG(VM_MEMORY_MALLOC)`, which `mach_vm_map` takes and this does not.
        let tag = 1 << 24;
        let source = buffer.as_ptr() as u64;
        assert_eq!(remap(source, 1, tag, 1), Err(KERN_INVALID_ARGUMENT));
    }
}
