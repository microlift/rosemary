#![cfg(not(target_os = "macos"))]

use std::ffi::c_void;
use std::sync::atomic::AtomicBool as Flag;

use super::kern::*;
use super::space::{MACH_PORT_NULL, Name, TASK_SELF};

/// `VM_REGION_BASIC_INFO_64`, the only flavour with a layout this tree can
/// promise. The 32-bit `VM_REGION_BASIC_INFO` is a different structure and is
/// refused rather than filled with this one.
const VM_REGION_BASIC_INFO_64: i32 = 9;

/// `VM_PROT_*`, which happen to be the same bits as `PROT_*` - stated rather
/// than relied on, because nothing makes two operating systems agree.
const VM_PROT_READ: i32 = 1;
const VM_PROT_WRITE: i32 = 2;
const VM_PROT_EXECUTE: i32 = 4;

/// `VM_PROT_DEFAULT`, the protection `<mach/vm_prot.h>` gives memory "for
/// newly-created virtual memory". It matters because of [`vm_allocate`]: the
/// one declaration of that call the image was compiled against has **four**
/// arguments, so there is no `prot` to honour and the default is the whole
/// answer.
pub const VM_PROT_DEFAULT: i32 = VM_PROT_READ | VM_PROT_WRITE;

/// Every bit of a `vm_prot_t` this tree knows, so a caller asking for a bit
/// outside it is refused by name rather than having it dropped.
const VM_PROT_ALL: i32 = VM_PROT_READ | VM_PROT_WRITE | VM_PROT_EXECUTE;

/// `KERN_PROTECTION_FAILURE`, which `<mach/kern_return.h>` numbers **2** - the
/// one code `mach_error_string` in [`super::kern`] does not yet name, and the
/// one `vm_protect` needs to say "the range is not there, or not yours to
/// change".
pub const KERN_PROTECTION_FAILURE: i32 = 2;

/// `PROT_*` for a `vm_prot_t`, or `None` for a word with a bit outside
/// `VM_PROT_ALL` in it.
///
/// `VM_PROT_NONE` maps to `PROT_NONE` and `VM_PROT_ALL` to `PROT_ALL`; anything
/// else is built from the same three bits, which is the whole reason the two
/// namespaces agree and the reason it is worth asserting rather than assuming.
pub(crate) fn host_protection(vm_prot: i32) -> Option<i32> {
    if vm_prot & !VM_PROT_ALL != 0 {
        return None;
    }
    let mut prot = 0;
    if vm_prot & VM_PROT_READ != 0 {
        prot |= libc::PROT_READ;
    }
    if vm_prot & VM_PROT_WRITE != 0 {
        prot |= libc::PROT_WRITE;
    }
    if vm_prot & VM_PROT_EXECUTE != 0 {
        prot |= libc::PROT_EXEC;
    }
    Some(prot)
}

/// The `VM_FLAGS_*` bits `<mach/vm_types.h>` gives `vm_allocate` and
/// `mach_vm_allocate`, and which of them this tree can honour.
///
/// Bit 0 is `VM_FLAGS_ANYWHERE` and the rest are behaviour changes to the
/// mapping: purging it, leaving it uncached, randomising its address, or
/// overwriting what is there. None of those has an `mmap` equivalent this host
/// could offer without pretending, so each is refused by name.
const VM_FLAGS_ANYWHERE: u32 = 0x0001;
const VM_FLAGS_PURGABLE: u32 = 0x0002;
const VM_FLAGS_RANDOM_ADDR: u32 = 0x0008;
const VM_FLAGS_NO_CACHE: u32 = 0x0010;
const VM_FLAGS_RESILIENT_CODESIGN: u32 = 0x0020;
const VM_FLAGS_RESILIENT_MEDIA: u32 = 0x0040;
const VM_FLAGS_OVERWRITE: u32 = 0x4000;

/// A `VM_FLAGS_*` bit this tree will not honour, named.
fn unhonourable_flag(bit: u32) -> Option<&'static str> {
    Some(match bit {
        VM_FLAGS_PURGABLE => "VM_FLAGS_PURGABLE",
        VM_FLAGS_RANDOM_ADDR => "VM_FLAGS_RANDOM_ADDR",
        VM_FLAGS_NO_CACHE => "VM_FLAGS_NO_CACHE",
        VM_FLAGS_RESILIENT_CODESIGN => "VM_FLAGS_RESILIENT_CODESIGN",
        VM_FLAGS_RESILIENT_MEDIA => "VM_FLAGS_RESILIENT_MEDIA",
        VM_FLAGS_OVERWRITE => "VM_FLAGS_OVERWRITE",
        _ => return None,
    })
}

/// `VM_INHERIT_COPY`, which is `VM_INHERIT_DEFAULT` and what a private mapping
/// on this host does across a fork.
const VM_INHERIT_COPY: u32 = 1;
/// `VM_INHERIT_SHARE`.
const VM_INHERIT_SHARE: u32 = 0;

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
    // The size, not the count. `vm_region_basic_info_data_64_t` is 40 bytes
    // with 4 bytes of padding before `offset` and 2 after `user_wired_count`,
    // and a caller does *not* have to offer all of it - see the capacity
    // handling in `vm_region_64`, which is where a caller that offers less than
    // this is answered rather than refused.
    assert!(size_of::<BasicInfo64>() == 40);
    // `offset` is a 64-bit field, so it is the first that needs the padding;
    // getting this wrong would put the file offset where the guest reads the
    // inheritance flag.
    assert!(std::mem::offset_of!(BasicInfo64, offset) == 24);
    assert!(std::mem::offset_of!(BasicInfo64, user_wired_count) == 36);
};

/// One line of `/proc/self/maps`, as much of it as a region answer needs.
struct Region {
    start: u64,
    end: u64,
    protection: i32,
    shared: bool,
    offset: u64,
}

/// The first mapping ending above `address`, which is Mach's own search: the
/// one containing it, or the next one up if there is a hole there.
fn region_at_or_after(address: u64) -> Option<Region> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    maps.lines().find_map(|line| {
        let region = parse(line)?;
        (region.end > address).then_some(region)
    })
}

/// `lo-hi perms offset dev inode path`.
fn parse(line: &str) -> Option<Region> {
    let mut fields = line.split_whitespace();
    let (low, high) = fields.next()?.split_once('-')?;
    let mut perms = fields.next()?.bytes();
    let offset = u64::from_str_radix(fields.next()?, 16).ok()?;
    let mut protection = 0;
    if perms.next() == Some(b'r') {
        protection |= VM_PROT_READ;
    }
    if perms.next() == Some(b'w') {
        protection |= VM_PROT_WRITE;
    }
    if perms.next() == Some(b'x') {
        protection |= VM_PROT_EXECUTE;
    }
    Some(Region {
        start: u64::from_str_radix(low, 16).ok()?,
        end: u64::from_str_radix(high, 16).ok()?,
        protection,
        shared: perms.next() == Some(b's'),
        offset,
    })
}

/// Say once that a flavour went unanswered, because the alternative is a
/// caller quietly told its own task has no regions.
fn unsupported(flavor: i32) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: vm_region_64: flavour {flavor} has a layout this tree does not \
             publish; only VM_REGION_BASIC_INFO_64 is answered"
        );
    });
}

/// Say once that a call was refused, and why.
///
/// Every other refusal in this crate names itself. `KERN_INVALID_ARGUMENT` on
/// its own is four numbers and no sentence, and a caller that only checks the
/// return code - which is what PartitionAlloc's allocator interception does -
/// turns it into a log line naming *its own* file and line and this tree's
/// silence. That is exactly the diagnostic that cost a debugger session to
/// read: the answer was in the refusal the whole time.
fn refused(why: &str) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!("rosetta3: vm_region_64: refused - {why}");
    });
    KERN_INVALID_ARGUMENT
}

/// A refusal from one of the calls added below, said once per symbol and per
/// reason.
///
/// Separate from [`refused`], which is `vm_region_64`'s and shares one `Once`
/// across every reason on purpose: that call has a single caller path, whereas
/// `vm_allocate`, `vm_map` and `vm_protect` each have several distinct reasons
/// and a reader needs to see which of them a given guest hit.
///
/// `write(2)` rather than `eprintln!` for the reason `super::thread`'s `note`
/// gives: a message about a refusal has to be out before the refusal is.
fn note(symbol: &str, said: &std::sync::atomic::AtomicBool, why: &str) {
    if !said.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let line = format!("rosetta3: {symbol}: {why}\n");
        // SAFETY: a live buffer for the length written.
        unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
    }
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
        return refused("a null out-parameter");
    }
    if task != TASK_SELF {
        // Worth naming, because `mach_task_self()` the function and
        // `mach_task_self_` the data symbol are two separate answers in this
        // tree, and a caller that reached for the wrong one is refused by a
        // comparison that looks arbitrary from the outside.
        return refused(&format!(
            "task {task:#x} is not this task ({TASK_SELF:#x}); only this process's \
             own address space is described, and it is described by mach_task_self()"
        ));
    }
    if flavor != VM_REGION_BASIC_INFO_64 {
        unsupported(flavor);
        return KERN_INVALID_ARGUMENT;
    }
    // `*infoCnt` is Mach's in/out count: on the way in it is the caller's
    // capacity in 32-bit words, on the way out it is how many words were
    // written. Mach accepts a capacity *below* the structure's size and fills
    // what fits - PartitionAlloc's allocator interception passes 9 words, 36
    // bytes, against a 40-byte `vm_region_basic_info_data_64_t`, and a real
    // kernel answers `KERN_SUCCESS`. Requiring the whole structure turned that
    // into an error the caller could only report by name and line number.
    //
    // SAFETY: the caller states `count` is readable.
    let capacity = unsafe { *count } as usize * size_of::<u32>();
    if capacity == 0 {
        return refused("the caller offered no room for any part of the answer");
    }
    // SAFETY: the caller states `address` is readable.
    let Some(region) = region_at_or_after(unsafe { *address }) else {
        // Past the last mapping. Mach says this with `KERN_INVALID_ADDRESS`,
        // and a walk stops on it.
        return KERN_INVALID_ADDRESS;
    };
    let basic = BasicInfo64 {
        protection: region.protection,
        // Nothing here records what a mapping *could* be made, and reporting
        // less than the current protection would be a contradiction.
        max_protection: region.protection,
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
    // SAFETY: the caller's obligations. Unaligned because Mach's structures are
    // packed to four bytes, and the copy is bounded by the capacity the caller
    // declared above, so a short buffer is filled as far as it goes and the
    // tail it did not offer is left as it found it - which is what the kernel
    // does, and what a caller that zero-initialised its structure expects.
    let written = capacity.min(size_of::<BasicInfo64>());
    unsafe {
        *address = region.start;
        *size = region.end - region.start;
        std::ptr::copy_nonoverlapping(
            std::ptr::from_ref(&basic).cast::<u8>(),
            info.cast::<u8>(),
            written,
        );
        *count = (written / size_of::<u32>()) as u32;
        if !object_name.is_null() {
            // A name for the memory object behind the region. There is no port
            // namespace entry for one, and null is what Mach returns for a
            // region with no pager.
            *object_name = MACH_PORT_NULL;
        }
    }
    KERN_SUCCESS
}

/// `kern_return_t vm_deallocate(vm_map_t target_task, vm_address_t address,
/// vm_size_t size)`
///
/// # Safety
///
/// `address` and `size` describe a mapping this process made and is done with.
pub unsafe extern "C" fn vm_deallocate(task: Name, address: u64, size: u64) -> i32 {
    if task != TASK_SELF {
        return KERN_INVALID_ARGUMENT;
    }
    // Mach accepts a zero-length deallocation and does nothing; `munmap` calls
    // it an error.
    if size == 0 {
        return KERN_SUCCESS;
    }
    // SAFETY: the caller states this is its own mapping.
    let out = unsafe { libc::munmap(address as *mut c_void, size as usize) };
    match out {
        0 => KERN_SUCCESS,
        _ => KERN_INVALID_ADDRESS,
    }
}

/// `kern_return_t vm_read_overwrite(vm_map_t target_task, vm_address_t address,
/// vm_size_t size, vm_address_t data, vm_size_t *outsize)`
///
/// Within one address space this is a copy, which is what it is on Darwin too
/// when the task is the caller's own.
///
/// # Safety
///
/// `address` is readable for `size` bytes and `data` writable for `size`; they
/// do not overlap. `out_size` is null or writable.
pub unsafe extern "C" fn vm_read_overwrite(
    task: Name,
    address: u64,
    size: u64,
    data: u64,
    out_size: *mut u64,
) -> i32 {
    if task != TASK_SELF {
        return KERN_INVALID_ARGUMENT;
    }
    if address == 0 || data == 0 {
        return KERN_INVALID_ADDRESS;
    }
    // SAFETY: the caller's obligation, including that the two do not overlap.
    unsafe {
        std::ptr::copy_nonoverlapping(
            address as *const u8,
            data as *mut u8,
            usize::try_from(size).unwrap_or(0),
        );
        if !out_size.is_null() {
            *out_size = size;
        }
    }
    KERN_SUCCESS
}

/// `kern_return_t vm_allocate(vm_map_t target_task, vm_address_t *address,
/// vm_size_t size, vm_any_addr_t anywhere)`
///
/// **Four arguments, and that is not a typo.** `<mach/vm_prot.h>` declares five
/// - `..., vm_any_addr_t anywhere, vm_prot_t prot)` - and the image's two call
/// sites (`Chromium Framework+0x6311fe` and `+0x6312ab`, in
/// `mojo/core/channel_mac.cc`) set only four: `edi` from `mach_task_self_`,
/// `rsi` to an out-parameter, `rdx` to `getpagesize()`, and `ecx` to
/// `0x14000001`. The fifth register is **not** set between the `getpagesize()`
/// call at `+0x6311df` and the call, so whatever Chromium compiled against, it
/// did not pass a `prot` - and the `sig!` row in `abi/table.rs` already agrees,
/// listing four. So the protection is Mach's own default for newly created
/// memory, `VM_PROT_DEFAULT` = `VM_PROT_READ|VM_PROT_WRITE`, which is what
/// `<mach/vm_prot.h>` says that means.
///
/// ### The fourth argument is not a boolean
///
/// `0x14000001` is `VM_FLAGS_ANYWHERE` (bit 0) with a **`vm_tag_t` in the high
/// sixteen bits**, and the only authority for that encoding in reach is
/// Chromium's own, in the same file as the call
/// (`mojo/core/channel_mac.cc`):
///
/// ```c
/// // The kernel uses the following tags in vm_allocate and mach_vm_allocate
/// constexpr vm_size_t VM_TAG_SHIFT_AMOUNT = 16;
/// constexpr vm_any_addr_t VM_FLAGS_ANYWHERE = 0x1;
/// #define VM_MAKE_TAG(tag, msg_type) ((((unsigned)tag) << VM_TAG_SHIFT_AMOUNT) | ((unsigned)msg_type))
/// #define VM_MARK_TAGS(tag, base) ((base) | VM_MAKE_TAG(tag, MSG_TYPE_SEND))
/// ...
/// vm_allocate(mach_task_self(), &address, vm_size,
///             VM_MARK_TAGS(VM_MEMORY_TAG_MOJO_PORT, VM_FLAGS_ANYWHERE),
///             VM_MAKE_TAG(VM_MEMORY_TAG_MOJO_PORT, MSG_TYPE_SEND));
/// ```
///
/// with the comment "Offset the tags so they're unique". So for the image's word
/// the tag is `0x14000001 >> 16` = `0x1400` and the flag is `VM_FLAGS_ANYWHERE`,
/// and a **tag is a kernel accounting label** - which zone the pages are
/// charged to - not anything about what the mapping *is*. This host has no
/// zones, so there is nothing to charge, and the tag is dropped with one line
/// saying so rather than invented.
///
/// ### What Mojo wants the range for, and why mmap is the right answer
///
/// The two ranges this call produces become the base of two sets of *port
/// names* - the call is immediately followed by a `mach_msg` carrying
/// `0x6d6a6873`, which is `'shjm'` reversed, Mojo's "make port" id. What the
/// kernel guarantees, and what Mojo relies on, is that the names are **fresh**:
/// address space it has just been given has never been a name before, and
/// nothing can collide with it. A private anonymous `mmap` of the same size has
/// exactly that property, which is why this is a real answer and not a
/// substitute.
///
/// ### Where a refusal here costs the run
///
/// The image does not tolerate a failure: `mojo/core/channel_mac.cc` does
/// `MACH_CHECK(kr == KERN_SUCCESS, kr) << "vm_allocate"`, and the string
/// `vm_allocate` is in the image at `+0x631671` for exactly that. So the
/// implementation below covers the word the image passes, and anything else -
/// a flag bit that changes the mapping, or a protection word with a bit
/// outside `VM_PROT_ALL` - is refused by name, which turns a null-slot jump
/// into a `PCHECK` that names the call.
///
/// # Safety
///
/// `address` is a writable `vm_address_t`.
pub unsafe extern "C" fn vm_allocate(
    task: Name,
    address: *mut u64,
    size: u64,
    anywhere: u32,
) -> i32 {
    static SAID_TASK: Flag = Flag::new(false);
    static SAID_NULL: Flag = Flag::new(false);
    static SAID_FLAG: Flag = Flag::new(false);
    static SAID_TAG: Flag = Flag::new(false);
    static SAID_SIZE: Flag = Flag::new(false);

    if address.is_null() {
        note("vm_allocate", &SAID_NULL, "a null `vm_address_t *`");
        return KERN_INVALID_ADDRESS;
    }
    if task != TASK_SELF {
        // Worth naming, for `vm_deallocate`'s reason: a caller that reached for
        // the wrong spelling of "this task" gets a refusal rather than an
        // answer about it.
        note(
            "vm_allocate",
            &SAID_TASK,
            &format!("task {task:#x} is not this task ({TASK_SELF:#x})"),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if let Some(bit) = (0..32u32)
        .map(|b| 1u32 << b)
        .find(|&bit| anywhere & bit != 0 && unhonourable_flag(bit).is_some())
    {
        note(
            "vm_allocate",
            &SAID_FLAG,
            &format!(
                "flags {anywhere:#x} include {} ({bit:#x}), which changes what the mapping is and \
                 has no mmap(2) equivalent this host could offer without pretending",
                unhonourable_flag(bit).unwrap_or("a flag")
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    let tag = anywhere >> 16;
    if tag != 0 {
        // Once, and only because it is a deviation worth naming rather than a
        // refusal: a `vm_tag_t` chooses the kernel zone the pages are
        // accounted to and this host has no zones.
        note(
            "vm_allocate",
            &SAID_TAG,
            &format!(
                "flags {anywhere:#x} carry vm_tag_t {tag:#x} (bits 16..31, per Mojo's VM_MAKE_TAG) \
                 and the tag is not recorded: a Mach tag selects the kernel zone a mapping is \
                 charged to, and this host has no zones to charge it to. The range itself is \
                 allocated"
            ),
        );
    }
    // The hint, in and out. Read before the length is considered, because Mach
    // writes it back even for a zero-length allocation - a caller that asked for
    // nothing gets its own address handed to it unchanged rather than a word
    // this runtime invented.
    //
    // SAFETY: the caller states `address` is a writable `vm_address_t`, checked
    // above.
    let hint = unsafe { std::ptr::read(address) };
    if size == 0 {
        // SAFETY: as above.
        unsafe { *address = hint };
        return KERN_SUCCESS;
    }
    let anywhere_flag = anywhere & VM_FLAGS_ANYWHERE != 0;
    let want = if anywhere_flag { 0 } else { hint };
    let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;
    // SAFETY: an anonymous mapping of `size` bytes, or `MAP_FAILED`. A
    // non-zero `want` that is not page-aligned makes `mmap` fail rather than
    // round it, which is the answer Darwin gives for a misaligned fixed hint
    // too, and is reported below rather than silently rounded.
    let got = unsafe {
        libc::mmap(
            want as *mut c_void,
            size as usize,
            host_protection(VM_PROT_DEFAULT).unwrap_or(libc::PROT_READ | libc::PROT_WRITE),
            flags,
            -1,
            0,
        )
    };
    if got == libc::MAP_FAILED {
        // `ENOMEM` and `EINVAL` are the two this can produce, and only the
        // second is an argument mistake.
        let err = std::io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(libc::EINVAL) => {
                note(
                    "vm_allocate",
                    &SAID_SIZE,
                    &format!(
                        "address {want:#x} with VM_FLAGS_FIXED and size {size:#x} was refused: {}",
                        err
                    ),
                );
                KERN_INVALID_ARGUMENT
            }
            _ => KERN_RESOURCE_SHORTAGE,
        };
    }
    // SAFETY: `got` is a whole mapping of `size` bytes from the call above.
    unsafe { *address = got as u64 };
    KERN_SUCCESS
}

/// `kern_return_t vm_map(vm_map_t target_task, vm_address_t *address,
/// vm_size_t size, memory_object_t memory_object, vm_offset_t offset,
/// vm_prot_t current_protection, vm_prot_t max_protection,
/// vm_inherit_t inheritance, boolean_t wired, boolean_t user_wired,
/// vm_tag_t tag)`
///
/// **Implemented for the anonymous map, refused for the file-backed one.** All
/// three call sites in the image are in
/// `base/memory/platform_shared_memory_region_apple.cc` - the file's own
/// strings for `mach_make_memory_entry_64`, `vm_deallocate`, `vm_map` and
/// `"VM region has a wrong protection mask: it is"` are in the same
/// translation unit as the call sites, at `+0x5d94adc`, `+0x5d94bd9` and
/// `+0x5d952db` - and **all three pass `MACH_PORT_NULL` in `rcx`**, the fourth
/// argument, which is the `memory_object`. So all three are anonymous, and an
/// `mmap` is the whole of what they need.
///
/// A `memory_object` that is not null is refused rather than answered with an
/// anonymous mapping, because that is the one case where the answer would be a
/// plausible lie: the guest asked for two processes to share a page and would
/// be handed two private copies. The third site is exactly that shape - it
/// sits in the function that calls `mach_make_memory_entry_64` to make a memory
/// entry from a file descriptor, and that call is *itself* still unbound, so
/// the site is unreachable in this runtime today.
///
/// `wired` memory is refused for the same reason: pinning a page is a real,
/// different thing that `mmap` cannot do.
///
/// # Safety
///
/// `address` is a writable `vm_address_t`; `size` is the length of the
/// mapping the caller wants to exist afterwards.
pub unsafe extern "C" fn vm_map(
    task: Name,
    address: *mut u64,
    size: u64,
    memory_object: Name,
    offset: u64,
    current_protection: i32,
    max_protection: i32,
    inheritance: i32,
    wired: u32,
    user_wired: u32,
    tag: u32,
) -> i32 {
    static SAID_TASK: Flag = Flag::new(false);
    static SAID_NULL: Flag = Flag::new(false);
    static SAID_OBJECT: Flag = Flag::new(false);
    static SAID_WIRED: Flag = Flag::new(false);
    static SAID_PROT: Flag = Flag::new(false);

    if address.is_null() {
        note("vm_map", &SAID_NULL, "a null `vm_address_t *`");
        return KERN_INVALID_ADDRESS;
    }
    if task != TASK_SELF {
        note(
            "vm_map",
            &SAID_TASK,
            &format!("task {task:#x} is not this task ({TASK_SELF:#x})"),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if memory_object != MACH_PORT_NULL {
        note(
            "vm_map",
            &SAID_OBJECT,
            &format!(
                "memory_object {memory_object:#x} asks for a file- or pager-backed mapping; the \
                 pages would have to be shared with whatever holds that object, and an anonymous \
                 mapping would answer with private copies - which is a plausible wrong answer, \
                 not a slow one. This is the shape base/memory/\
                 platform_shared_memory_region_apple.cc's MapAt() takes"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if wired != 0 || user_wired != 0 {
        note(
            "vm_map",
            &SAID_WIRED,
            &format!(
                "wired {wired}, user_wired {user_wired}: a wired mapping is locked in physical \
                 memory, which mmap(2) cannot do and which a caller that asked for it will rely \
                 on"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    let Some(prot) = host_protection(current_protection) else {
        note(
            "vm_map",
            &SAID_PROT,
            &format!(
                "current_protection {current_protection:#x} has a bit outside VM_PROT_ALL \
                 ({VM_PROT_ALL:#x}) in it"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    };
    if max_protection & !VM_PROT_ALL != 0 {
        note(
            "vm_map",
            &SAID_PROT,
            &format!(
                "max_protection {max_protection:#x} has bits outside VM_PROT_ALL ({VM_PROT_ALL:#x}) \
                 in it"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    // The address comes back as the caller wrote it when there is nothing to
    // map, which is what Mach does and what keeps a zero-length map from being
    // the one place the two operating systems disagree about whether zero is an
    // error.
    if size == 0 {
        // SAFETY: the caller's `address`, checked above.
        unsafe { *address = std::ptr::read(address) };
        return KERN_SUCCESS;
    }
    // `offset` is only meaningful with a memory object; with `MACH_PORT_NULL`
    // Mach ignores it and so does this, which is why a non-zero offset is not
    // a refusal. `inheritance` and `tag` are the other two: `VM_INHERIT_COPY`
    // is what a private mapping does across a `fork(2)`, and a `vm_tag_t` is an
    // accounting label with no zones to record it in.
    let _ = (offset, inheritance, tag);
    // SAFETY: an anonymous mapping of `size` bytes, or `MAP_FAILED`.
    let got = unsafe {
        libc::mmap(
            0 as *mut c_void,
            size as usize,
            prot,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if got == libc::MAP_FAILED {
        return KERN_RESOURCE_SHORTAGE;
    }
    // SAFETY: `got` is a whole mapping of `size` bytes from the call above.
    unsafe { *address = got as u64 };
    KERN_SUCCESS
}

/// `kern_return_t vm_protect(vm_map_t target_task, vm_address_t address,
/// vm_size_t size, vm_prot_t current_protection, vm_prot_t max_protection)`
///
/// **Implemented, and what is *not* implemented is named.** The two call sites
/// in the image are both in PartitionAlloc's purgeable-zone set-up -
/// `malloc_default_purgeable_zone()` and `os_unfair_lock` are the first three
/// calls of the function at `Chromium Framework+0x5debd80`, which is the
/// `StoreZoneFunctions` neighbourhood runs 012 and 013 died in:
///
/// ```text
/// vm_protect(mach_task_self(), address, size, 0, protection|VM_PROT_WRITE, 0)
/// vm_protect(mach_task_self(), address, size, 0, protection,            0)
/// ```
///
/// at `+0x5dec070` and `+0x5dec1cb`. The first makes purgeable memory
/// read-only while leaving it *capable* of being made writable again, and a
/// failure there is only logged - the guest saves `errno`, zeroes it, prints,
/// and builds the zone anyway. The second's return value is not read at all.
///
/// So the `current_protection` is a real answer and it is `mprotect`: after this
/// call the range has exactly that protection, and [`vm_region_64`] will report
/// it. The `max_protection` is **not** answered, because Linux has no notion of
/// a per-mapping ceiling on future protection changes: `mprotect` sets both.
/// This host is therefore *more* permissive than what was asked for, and the
/// alternative - keeping a shadow ceiling and refusing a later `vm_protect` that
/// crossed it - would be a lie a guest could only discover as a spurious
/// failure on memory it owns. It is called out here rather than left for a
/// reader to find out by having a write fault.
///
/// # Safety
///
/// `address` and `size` describe a mapping this process made.
pub unsafe extern "C" fn vm_protect(
    task: Name,
    address: u64,
    size: u64,
    current_protection: i32,
    max_protection: i32,
) -> i32 {
    static SAID_TASK: Flag = Flag::new(false);
    static SAID_PROT: Flag = Flag::new(false);
    static SAID_RANGE: Flag = Flag::new(false);

    if task != TASK_SELF {
        note(
            "vm_protect",
            &SAID_TASK,
            &format!("task {task:#x} is not this task ({TASK_SELF:#x})"),
        );
        return KERN_INVALID_ARGUMENT;
    }
    let Some(prot) = host_protection(current_protection) else {
        note(
            "vm_protect",
            &SAID_PROT,
            &format!(
                "current_protection {current_protection:#x} has a bit outside VM_PROT_ALL \
                 ({VM_PROT_ALL:#x}) in it"
            ),
        );
        return KERN_INVALID_ARGUMENT
    };
    // Mach accepts a zero-length `vm_protect` and does nothing; `mprotect` with
    // a zero length succeeds too, and saying so here keeps the two agreeing.
    if size == 0 {
        return KERN_SUCCESS;
    }
    // SAFETY: the caller's obligation - `address` and `size` describe a mapping
    // this process made. `mprotect` writes nothing.
    if unsafe { libc::mprotect(address as *mut c_void, size as usize, prot) } != 0 {
        // `ENOMEM` is the unaligned-range and not-mapped case, which is what
        // `KERN_PROTECTION_FAILURE` means to a caller.
        note(
            "vm_protect",
            &SAID_RANGE,
            &format!(
                "{address:#x}+{size:#x} could not be given protection {current_protection:#x}: {}",
                std::io::Error::last_os_error()
            ),
        );
        return KERN_PROTECTION_FAILURE;
    }
    // The ceiling is not honoured, and cannot be: `mprotect` sets the current
    // protection and the maximum together. Named once, because a reader who
    // wants to know whether a later `vm_protect` can re-open the range should
    // not have to infer it from the absence of a refusal.
    static SAID_CEILING: Flag = Flag::new(false);
    if max_protection != current_protection {
        note(
            "vm_protect",
            &SAID_CEILING,
            &format!(
                "max_protection {max_protection:#x} is not recorded as a ceiling: Linux has no \
                 per-mapping limit on later protection changes and mprotect(2) sets the current \
                 and the maximum protection together, so this range can be re-opened beyond \
                 {max_protection:#x}. current_protection {current_protection:#x} was applied"
            ),
        );
    }
    KERN_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Another task is refused rather than answered about with this one's map.
    #[test]
    fn another_task_is_refused() {
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
        assert_eq!(status, KERN_INVALID_ARGUMENT);
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
}
