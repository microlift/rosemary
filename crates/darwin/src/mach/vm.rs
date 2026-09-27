#![cfg(not(target_os = "macos"))]

use std::ffi::c_void;
use std::sync::atomic::AtomicBool as Flag;

use super::kern::*;
use super::space::{MACH_PORT_NULL, Name, TASK_SELF};

/// `VM_REGION_BASIC_INFO_64` and its word count, both from
/// `<mach/vm_region.h>`, which is `#pragma pack(push, 4)` from line 53 to line
/// 377 and says at line 82 `#define VM_REGION_BASIC_INFO_64 9`.
///
/// This is the only flavour with a layout this tree can promise. The three
/// others the guest asks for - `VM_REGION_TOP_INFO` (12),
/// `VM_REGION_EXTENDED_INFO` (13) and the legacy `VM_REGION_BASIC_INFO` (10) -
/// are different structures and are refused by name rather than filled with
/// this one; see [`region`] for which of them the image actually calls and what
/// each would have to invent.
const VM_REGION_BASIC_INFO_64: i32 = 9;
/// `VM_REGION_BASIC_INFO_COUNT_64`, which the same header defines as
/// `sizeof(vm_region_basic_info_data_64_t)/sizeof(int)` - **9**, because the
/// packing puts the structure at 36 bytes and not the 40 an unpacked LP64
/// layout would give. The image agrees: every `VM_REGION_BASIC_INFO_64` call
/// site in `Chromium Framework` loads `9` into `*infoCnt` and poisons exactly
/// 36 bytes of `info` before the call (16 + 16 + 4: `movaps`, `movaps`, `mov
/// dword` - see `+0x5debe35..+0x5debe4d` for `vm_region_64` and
/// `+0x5d7cd1b..+0x5d7d002` for `mach_vm_region`).
const VM_REGION_BASIC_INFO_COUNT_64: usize = 9;

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

/// `VM_PROT_NONE`, the zero protection, and the one the image's own protection
/// table has at index 0 and index 5. `<mach/vm_prot.h>` gives
/// `VM_PROT_NONE 0x00`, `VM_PROT_READ 0x01`, `VM_PROT_WRITE 0x02`,
/// `VM_PROT_EXECUTE 0x04` and `VM_PROT_DEFAULT = READ|WRITE`.
///
/// `pub` for the same reason [`VM_PROT_DEFAULT`] is: it is a value the family
/// below is defined in terms of, and `VM_PROT_NONE` is a real request rather
/// than a missing one - the image's table asks for it.
pub const VM_PROT_NONE: i32 = 0;

/// `VM_FLAGS_*` bits `<mach/vm_types.h>` gives `vm_allocate` and
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
///
/// **`packed(4)`, which is the whole point of this declaration.** The header
/// wraps the flavour structures in `#pragma pack(push, 4)` /
/// `#pragma pack(pop)`, and with it `offset` lands at byte **20**, not at the
/// 24 an unpacked LP64 struct would give it: the five `int` fields before it end
/// at 20, and `memory_object_offset_t` is then packed down to a 4-byte-aligned
/// 64-bit field rather than being pushed out to 8. `behavior` is at 28 and
/// `user_wired_count` at 32, which leaves the structure 34 bytes of fields
/// rounded up to **36** - and 36/4 is the 9 that
/// `VM_REGION_BASIC_INFO_COUNT_64` says, which is the check that settles it.
#[repr(C, packed(4))]
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
    // The size, not the count: `vm_region_basic_info_data_64_t` is 36 bytes
    // under `vm_region.h`'s `#pragma pack(4)`, which is what makes
    // `VM_REGION_BASIC_INFO_COUNT_64` the 9 the image passes. An unpacked LP64
    // layout gives 40, and with it a caller that offers 9 words has its
    // `offset` field written four bytes late - the guest then reads the file
    // offset out of `reserved` and the two padding bytes below it.
    assert!(size_of::<BasicInfo64>() == 36);
    assert!(size_of::<BasicInfo64>() / size_of::<u32>() == VM_REGION_BASIC_INFO_COUNT_64);
    // `offset` is the first field that packing moves, and it is the one a
    // caller reads back by name.
    assert!(std::mem::offset_of!(BasicInfo64, offset) == 20);
    assert!(std::mem::offset_of!(BasicInfo64, behavior) == 28);
    assert!(std::mem::offset_of!(BasicInfo64, user_wired_count) == 32);
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

/// Say once that a flavour went unanswered, naming the symbol that was asked
/// and what answering it would have taken.
///
/// `VM_REGION_BASIC_INFO_64` is the whole of what this tree publishes, and the
/// image asks for two more. Both refusals are checked against the guest's own
/// error paths rather than assumed to be survivable:
///
/// - `VM_REGION_TOP_INFO` (**12**) at `Chromium Framework+0x5d98f22`, in the
///   function that deallocates a `ScopedMachSendRight`. Its `obj_id` and
///   `ref_count` are ipc-kernel object identities; a real Mac answers with the
///   name of the memory object the send right refers to, and **this tree mints
///   memory objects now** - see [`super::memory_object`] - but it still cannot
///   answer this, and the reason is different from the one this note used to
///   give. A port's cross-process identity here is its `Address`, and
///   `mach::space` has no direction from an `Address` back to a name: a
///   receiving process *draws* its own name for an address it was sent, so a
///   region of a memory entry may be backed by an entry this process never
///   minted and holds a name it chose. Reporting a name would be reporting
///   whichever name happened to be in the table, which is not the same thing as
///   naming the object. `obj_id` and `ref_count` are kernel counters with no
///   host equivalent at all. The site stores the return code in `r12d` and only
///   turns it into a `bool` at the end (`cmp r12d, 1 / adc eax, 1` at
///   `+0x5d990ab`), so a refusal is `false` and nothing more.
/// - `VM_REGION_EXTENDED_INFO` (**13**) at `+0x81c40c5`, in the GPU process's
///   IOSurface memory accounting - the same function's strings are `iosurface`,
///   `resident_size`, `swapped_out_size` and `dirty_size`, and it reads
///   `user_tag` at +4, `pages_resident` at +8, `pages_swapped_out` at +16 and
///   `pages_dirtied` at +20 of the answer. Those are Mach VM-object residency
///   counters. Reporting zeroes would hand a memory-accounting component a
///   confident, wrong number, which is the outcome this project is built to
///   avoid. The site's `kr != 0` branch is `xor eax, eax` at `+0x81c44e8`: a
///   refusal is `false`, with no abort on either path.
fn unsupported(symbol: &str, flavor: i32) {
    use std::sync::OnceLock;
    static SAID: OnceLock<std::sync::Mutex<Vec<(String, i32)>>> = OnceLock::new();
    let said = SAID.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    let mut said = said.lock().unwrap_or_else(|e| e.into_inner());
    if said.iter().any(|(s, f)| s == symbol && *f == flavor) {
        return;
    }
    said.push((symbol.to_string(), flavor));
    let because = match flavor {
        12 => {
            "VM_REGION_TOP_INFO: obj_id and ref_count are ipc-kernel object identities with no \
                host equivalent, and a region's memory object has no name to report either - a \
                port's cross-process identity here is its Address, and mach::space has no \
                direction from an Address back to the name this process happens to hold it under"
        }
        13 => {
            "VM_REGION_EXTENDED_INFO: pages_resident, pages_swapped_out, pages_dirtied and \
                ref_count are Mach VM-object residency counters, and zeroes would be a confident \
                wrong number for a memory-accounting caller"
        }
        10 => {
            "VM_REGION_BASIC_INFO is the 32-bit structure with a 32-bit offset; use \
                VM_REGION_BASIC_INFO_64"
        }
        _ => "this tree publishes no layout for it",
    };
    eprintln!(
        "rosetta3: {symbol}: flavour {flavor} is not answered - {because}. Only \
         VM_REGION_BASIC_INFO_64 (9) is"
    );
}

/// Say once that a call was refused, and why.
///
/// Every other refusal in this crate names itself. `KERN_INVALID_ARGUMENT` on
/// its own is four numbers and no sentence, and a caller that only checks the
/// return code - which is what PartitionAlloc's allocator interception does -
/// turns it into a log line naming *its own* file and line and this tree's
/// silence. That is exactly the diagnostic that cost a debugger session to
/// read: the answer was in the refusal the whole time.
///
/// Said once per `(symbol, reason)` rather than once for the whole family, for
/// the same reason [`unsupported`] is: `vm_region_64` and `mach_vm_region` are
/// one body reached by two names, and a reader who hit one refusal has no way
/// to tell which of them they hit.
fn refused(symbol: &str, why: &str) -> i32 {
    use std::sync::OnceLock;
    static SAID: OnceLock<std::sync::Mutex<Vec<(String, String)>>> = OnceLock::new();
    let said = SAID.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    let mut said = said.lock().unwrap_or_else(|e| e.into_inner());
    let key = (symbol.to_string(), why.to_string());
    if said.contains(&key) {
        return KERN_INVALID_ARGUMENT;
    }
    said.push(key);
    eprintln!("rosetta3: {symbol}: refused - {why}");
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
    // SAFETY: forwarded unchanged, and `region`'s contract is this one's.
    unsafe {
        region(
            "vm_region_64",
            task,
            address,
            size,
            flavor,
            info,
            count,
            object_name,
        )
    }
}

/// `kern_return_t mach_vm_region(vm_map_t target_task, mach_vm_address_t
/// *address, mach_vm_size_t *size, vm_region_flavor_t flavor, vm_region_info_t
/// info, mach_msg_type_number_t *infoCnt, mach_port_t *object_name)`
///
/// **The same seven words as [`vm_region_64`] and the same contract, because
/// `mach_vm_address_t` and `mach_vm_size_t` are `uint64_t`.** That is not an
/// assumption: XNU declares one MIG routine and publishes it under both names -
/// `osfmk/mach/mach_vm.defs:443` reads
///
/// ```text
/// routine PREFIX(mach_vm_region)
/// #else
/// routine PREFIX(vm_region_64)
/// #endif
///         target_task     : vm_map_read_t;
/// inout  address         : mach_vm_address_t;
///     out size            : mach_vm_size_t;
///         flavor          : vm_region_flavor_t;
/// out info                : vm_region_info_t, CountInOut;
/// out object_name         : memory_object_name_t = ... ctype: mach_port_t);
/// ```
///
/// so the two spellings are one declaration, and `inout address` and
/// `CountInOut info` are the kernel's own words for the two in/out
/// parameters. This is also why `mach_vm_region` and `vm_region_64` are two
/// rows in the image's import table and not one: `Chromium Framework` stubs
/// `_mach_vm_region` at `+0xcca15de` and `_vm_region_64` at `+0xcca18b4`, and
/// calls the first eleven times and the second once.
///
/// The image's eleven `mach_vm_region` call sites are `+0x5d7cd68`,
/// `+0x5d7cdd3`, `+0x5d7cfc2`, `+0x5d7d024`, `+0x5d98f22`, `+0x5d99192`,
/// `+0x81c40c5`, `+0xa589653`, `+0xa5896b6`, `+0xa589954` and `+0xa5899be`; see
/// [`unsupported`] for which flavours they pass and what the two this tree does
/// not answer would have to invent.
///
/// # Safety
///
/// As [`vm_region_64`].
pub unsafe extern "C" fn mach_vm_region(
    task: Name,
    address: *mut u64,
    size: *mut u64,
    flavor: i32,
    info: *mut c_void,
    count: *mut u32,
    object_name: *mut Name,
) -> i32 {
    // SAFETY: forwarded unchanged, and `region`'s contract is this one's.
    unsafe {
        region(
            "mach_vm_region",
            task,
            address,
            size,
            flavor,
            info,
            count,
            object_name,
        )
    }
}

/// The body of [`vm_region_64`] and [`mach_vm_region`], which differ only in the
/// name their refusals print.
///
/// # Safety
///
/// As [`vm_region_64`].
unsafe fn region(
    symbol: &str,
    task: Name,
    address: *mut u64,
    size: *mut u64,
    flavor: i32,
    info: *mut c_void,
    count: *mut u32,
    object_name: *mut Name,
) -> i32 {
    if address.is_null() || size.is_null() || info.is_null() || count.is_null() {
        return refused(symbol, "a null out-parameter");
    }
    if task != TASK_SELF {
        // Worth naming, because `mach_task_self()` the function and
        // `mach_task_self_` the data symbol are two separate answers in this
        // tree, and a caller that reached for the wrong one is refused by a
        // comparison that looks arbitrary from the outside.
        return refused(
            symbol,
            &format!(
                "task {task:#x} is not this task ({TASK_SELF:#x}); only this process's \
                 own address space is described, and it is described by mach_task_self()"
            ),
        );
    }
    if flavor != VM_REGION_BASIC_INFO_64 {
        unsupported(symbol, flavor);
        return KERN_INVALID_ARGUMENT;
    }
    // `*infoCnt` is Mach's in/out count - `CountInOut` in the MIG definition
    // above: on the way in it is the caller's capacity in 32-bit words, on the
    // way out it is how many words were written. Mach accepts a capacity
    // *below* the structure's size and fills what fits, so the copy below is
    // bounded by the capacity rather than checked against it. Every call site in
    // the image offers the full 9, so this is belt and braces rather than a
    // case that arises here.
    //
    // SAFETY: the caller states `count` is readable.
    let capacity = unsafe { *count } as usize * size_of::<u32>();
    if capacity == 0 {
        return refused(
            symbol,
            "the caller offered no room for any part of the answer",
        );
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
    if task != TASK_SELF {
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller states `count` is readable.
    let capacity = unsafe { *count } as usize;
    if capacity < V0_WORDS {
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller states `address` is readable.
    let Some(region) = region_at_or_after(unsafe { *address }) else {
        return KERN_INVALID_ADDRESS;
    };
    let words = capacity.min(V1_WORDS);
    let mut out = vec![0u32; words];
    out[0] = region.protection as u32;
    out[1] = region.protection as u32;
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
    if out == 0 {
        // A mapping of a memory entry is remembered so that a later
        // `mach_make_memory_entry_64` over one of its addresses can find the
        // pages behind it. Once the range is gone the record is worse than
        // useless: it would name a file for an address that is now something
        // else.
        super::memory_object::forget(address, size);
        KERN_SUCCESS
    } else {
        KERN_INVALID_ADDRESS
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
/// vm_size_t size, vm_offset_t offset, boolean_t anywhere, memory_object_t
/// memory_object, vm_offset_t file_offset, boolean_t docow, vm_prot_t
/// current_protection, vm_prot_t max_protection, vm_inherit_t inheritance)`
///
/// # The parameter order, which this function used to get wrong
///
/// **Eleven parameters, and the fourth is `offset` and the sixth is
/// `memory_object`.** That is `<mach/vm_prot.h>`'s declaration and XNU's MIG
/// definition, `osfmk/mach/vm_map.defs`:
///
/// ```text
/// routine vm_map(
///         target_task     : vm_map_read_t;
/// inout  address         : vm_address_t;
///         size            : vm_size_t;
///         offset          : vm_offset_t;
///         anywhere        : boolean_t;
///         object          : memory_object_t;
///         file_offset     : vm_offset_t;
///         docow           : boolean_t;
///         cur_protection  : vm_prot_t;
///         max_protection  : vm_prot_t;
///         inheritance     : vm_inherit_t);
/// ```
///
/// The signature this function carried instead was
/// `(task, address, size, memory_object, offset, current_protection,
/// max_protection, inheritance, wired, user_wired, tag)`, which is wrong from
/// the **fourth** parameter on and wrong in a way that a reader could not see
/// from the count: it has eleven parameters too, and the same eleven *kinds*
/// in the same *kinds* of order, because `wired`/`user_wired`/`tag` and
/// `file_offset`/`docow` are all integers. The trailing five were a
/// four-byte-wide tail read as
/// `max_protection, inheritance, wired, user_wired, tag`, while the real
/// trailing five start with an **eight**-byte `file_offset` - so the old list
/// was not merely misnamed, it was *shifted by four bytes* against the stack
/// the caller had actually built, and `inheritance` was never read at all.
///
/// Three call sites in the image reach this, all in
/// `base/memory/platform_shared_memory_region_apple.cc`: a byte scan of every
/// `e8 rel32` in `__TEXT,__text` (214 MB) for **both** `_vm_map` stubs -
/// `0xcca16a4` and `0xcca3ba6`, which `macho.py`'s `stub_at` answers with the
/// same name - finds `+0x5d94adc`, `+0x5d94bd9` and `+0x5d952db` against the
/// first and **zero** against the second. The three that is a real count only
/// because the same scan reports three for the first stub; a scan of one stub
/// alone would have been a coin toss.
///
/// | | `+0x5d94adc` | `+0x5d94bd9` | `+0x5d952db` |
/// |---|---|---|---|
/// | function | `MapAt` | `CheckPlatformHandlePermissionsCorrespondToMode` | `ConvertToReadOnly` |
/// | `size` (`rdx`) | `r14` = its 6th argument | `r15` = its 5th | `[r12+0x18]` |
/// | `offset` (`rcx`) | `xor ecx,ecx` - 0 | `xor ecx,ecx` - 0 | `xor ecx,ecx` - 0 |
/// | `anywhere` (`r8d`) | `mov r8d,0x1` | `mov r8d,0x1` | `mov r8d,0x1` |
/// | `memory_object` (`r9d`) | its 3rd argument | `[rsi+8]` - `handle_` | `ebx` - the handle |
/// | `file_offset` | `rax` = its 5th argument | `push 0x0` | `push 0x0` |
/// | `docow` | `push 0x0` | `push 0x0` | `push 0x0` |
/// | `current_protection` | `r10` = `permissions*2+1` | `push 0x3` | `push 0x1` |
/// | `max_protection` | `r10` | `push 0x3` | `push 0x1` |
/// | `inheritance` | `push 0x2` | `push 0x2` | `push 0x2` |
///
/// The register half is `lea r10d,[rcx*2+0x1]` for the protection pair at
/// `+0x5d94ab8`, and the stack half is five pushes whose order in memory is
/// fixed by the fact that the *last* push is the lowest address - read at
/// `+0x5d952d1..+0x5d952db` as `0, 0, 1, 1, 2` for `file_offset, docow,
/// VM_PROT_READ, VM_PROT_READ, VM_INHERIT_NONE`, which is
/// `vm_map(mach_task_self(), &temp_addr, size_, 0, VM_FLAGS_ANYWHERE,
/// handle_copy.get(), 0, FALSE, VM_PROT_READ, VM_PROT_READ, VM_INHERIT_NONE)`
/// as `base/memory/platform_shared_memory_region_apple.cc` writes it. The
/// `permissions*2+1` is the same file's protection table: `0 -> VM_PROT_READ`,
/// `1 -> VM_PROT_READ|VM_PROT_WRITE`, `2 -> VM_PROT_READ|VM_PROT_EXECUTE`.
///
/// ## What the old order did to those three sites
///
/// It made the memory-object check **dead code**: it read `rcx`, which is zero
/// at all three, so the branch never ran. The port then landed in
/// `current_protection` (`r9`) and was turned away there as a protection mask
/// with bits outside `VM_PROT_ALL` - a refusal that fires and names the wrong
/// argument, which is the failure mode this file is written to avoid. The
/// correct `memory_object != MACH_PORT_NULL` refusal fires at the same three
/// sites, returns the **same** `KERN_INVALID_ARGUMENT` (so nothing the guest
/// branches on changes), and names the argument that is actually wrong.
///
/// The fifth argument is a **`boolean_t anywhere`**, not a `VM_FLAGS_*` word.
/// `VM_FLAGS_ANYWHERE` happens to be 1 and the image passes 1 at all three
/// sites, so the two cannot be told apart from the values alone - but
/// [`mach_vm_map`]'s fifth argument *is* a flag word, and reading this one as
/// flags would be a different mistake. It is honoured here as the boolean it
/// is: a non-zero `anywhere` lets the host choose the address, and a zero one
/// means "at `*address`", which is refused if anything is already mapped there
/// (Mach has no `VM_FLAGS_OVERWRITE` to say "replace it", and that is the
/// refusal `vm_map_locate_space_fixed` produces).
///
/// # What is answered, and what is not
///
/// - **A `memory_object` is mapped, and it is the same pages.** The entry is a
///   file in a per-user directory named by the port's `Address`, mapped
///   `MAP_SHARED`, so this process and a child that was sent the port see one
///   page cache. See [`super::memory_object`] for how the port reaches the
///   child, which is the half that made an earlier refusal of this branch
///   unavoidable.
/// - **`memory_object` with a protection above the entry's own maximum is
///   `KERN_INVALID_RIGHT`**, and that is the code
///   `CheckPlatformHandlePermissionsCorrespondToMode` reads to decide a region
///   is read-only.
/// - **`offset != 0` with a memory object is refused by name.** `offset` is an
///   address in the *target task* - the object already mapped there - and
///   ignoring it would answer a different request as asked.
/// - **`docow != 0` is refused by name**: a copy-on-write clone of a memory
///   object, which an anonymous mapping has no meaning for.
/// - **`inheritance` must be `VM_INHERIT_SHARE`, `VM_INHERIT_COPY` or
///   `VM_INHERIT_NONE`**, for [`mach_vm_map`]'s reason: `VM_INHERIT_DONATE_COPY`
///   deletes the parent's mapping and `mmap(2)` cannot say that.
/// - **`offset` and `file_offset` are ignored when there is no memory object**,
///   because there is nothing for either to index into and Mach ignores them
///   too. Stated rather than left to be inferred from their absence.
/// - **`max_protection` is not recorded as a ceiling**, for [`vm_protect`]'s
///   reason: Linux has no per-mapping limit on later protection changes. For a
///   range backed by a memory entry the *entry's* ceiling is enforced instead,
///   by [`vm_protect`], because that one is a promise about the pages rather
///   than about the mapping.
///
/// # Safety
///
/// `address` is a writable `vm_address_t`; `size` is the length of the
/// mapping the caller wants to exist afterwards.
pub unsafe extern "C" fn vm_map(
    task: Name,
    address: *mut u64,
    size: u64,
    offset: u64,
    anywhere: u32,
    memory_object: Name,
    file_offset: u64,
    docow: u32,
    current_protection: i32,
    max_protection: i32,
    inheritance: u32,
) -> i32 {
    static SAID_TASK: Flag = Flag::new(false);
    static SAID_NULL: Flag = Flag::new(false);
    static SAID_DCOW: Flag = Flag::new(false);
    static SAID_INHERIT: Flag = Flag::new(false);
    static SAID_PROT: Flag = Flag::new(false);
    static SAID_CLASH: Flag = Flag::new(false);

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
        if docow != 0 {
            note(
                "vm_map",
                &SAID_DCOW,
                &format!(
                    "docow {docow} asks for a copy-on-write clone of memory object \
                     {memory_object:#x}; the pages would be a second mapping of the same object \
                     that a later write does not reach, and this host does not build one"
                ),
            );
            return KERN_INVALID_ARGUMENT;
        }
        if inheritance > VM_INHERIT_NONE {
            note(
                "vm_map",
                &SAID_INHERIT,
                &format!(
                    "inheritance {inheritance} is not one of VM_INHERIT_SHARE (0), \
                     VM_INHERIT_COPY (1) or VM_INHERIT_NONE ({VM_INHERIT_NONE})"
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
        let _ = prot;
        let page = page_size();
        if page == 0 {
            return KERN_INVALID_ARGUMENT;
        }
        // The caller's hint, read before anything is mapped: a fixed request
        // hands it on, and an anywhere request hands back whatever the host
        // chose.
        //
        // SAFETY: the caller's `address`, checked non-null above.
        let hint = unsafe { std::ptr::read(address) };
        let status = super::memory_object::map(super::memory_object::Request {
            object: memory_object,
            offset,
            file_offset,
            size,
            protection: current_protection,
            max_protection,
            anywhere: anywhere != 0,
            hint,
            page,
            symbol: "vm_map",
        });
        let Some(got) = status.ok() else {
            return status.err().unwrap_or(KERN_FAILURE);
        };
        // SAFETY: the caller's `address`, checked non-null above.
        unsafe { *address = got };
        if max_protection != current_protection {
            note(
                "vm_map",
                &SAID_PROT,
                &format!(
                    "max_protection {max_protection:#x} is not recorded as a ceiling on this \
                     range: Linux has no per-mapping limit on later protection changes, so the \
                     ceiling that does exist is the memory entry's, and vm_protect enforces that \
                     one instead - see super::memory_object"
                ),
            );
        }
        return KERN_SUCCESS;
    }
    if docow != 0 {
        note(
            "vm_map",
            &SAID_DCOW,
            &format!(
                "docow {docow} asks for a copy-on-write clone of a memory object; with memory \
                 object MACH_PORT_NULL there is nothing to clone"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if inheritance > VM_INHERIT_NONE {
        note(
            "vm_map",
            &SAID_INHERIT,
            &format!(
                "inheritance {inheritance} is not one of VM_INHERIT_SHARE (0), VM_INHERIT_COPY \
                 (1) or VM_INHERIT_NONE ({VM_INHERIT_NONE}); VM_INHERIT_DONATE_COPY (3) deletes \
                 the parent's mapping, which mmap(2) has no way to express"
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
    // `offset` and `file_offset` index into an object, and there is no object:
    // Mach ignores both here and so does this, which is why a non-zero one is
    // not a refusal. Stated rather than left to be inferred.
    let _ = (offset, file_offset);
    let page = page_size();
    if page == 0 {
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller's `address`, checked non-null above.
    let hint = unsafe { std::ptr::read(address) };
    let anywhere_requested = anywhere != 0;
    if !anywhere_requested {
        // `vm_map` has no `VM_FLAGS_OVERWRITE` - its fifth argument is the
        // boolean, not a flag word - so there is no way to ask for a fixed
        // mapping to replace what is there, and Mach refuses instead. That is
        // `vm_map_locate_space_fixed` returning `KERN_MEMORY_PRESENT`, turned
        // into `KERN_NO_SPACE` for the caller, which is the reading
        // [`mach_vm_map`] uses for the same thing.
        if hint & (page - 1) != 0 {
            note(
                "vm_map",
                &SAID_CLASH,
                &format!(
                    "anywhere is FALSE, so the mapping has to be exactly at {hint:#x}, which is \
                     not a multiple of the {page:#x} page size; a Mach mapping is page-granular \
                     and MAP_FIXED cannot round one without moving the range the caller asked to \
                     be exactly here"
                ),
            );
            return KERN_INVALID_ARGUMENT;
        }
        match range_is_mapped(hint, hint + size) {
            Some(true) => {
                note(
                    "vm_map",
                    &SAID_CLASH,
                    &format!(
                        "{hint:#x}+{size:#x} is already mapped and vm_map has no \
                         VM_FLAGS_OVERWRITE to ask for it to be replaced, so Mach answers \
                         KERN_NO_SPACE; MAP_FIXED would have replaced it"
                    ),
                );
                return KERN_NO_SPACE;
            }
            // Nothing was read, so nothing is claimed: the call is refused and
            // the refusal says why rather than asserting a clash it did not see.
            None => {
                note(
                    "vm_map",
                    &SAID_CLASH,
                    &format!(
                        "{hint:#x}+{size:#x} was asked for as a fixed mapping and \
                         /proc/self/maps could not be read, so whether anything is there is \
                         unknown. MAP_FIXED would have replaced it without asking, so the call \
                         is refused instead"
                    ),
                );
                return KERN_NO_SPACE;
            }
            Some(false) => {}
        }
    }
    // `MAP_NORESERVE` because Mach does not commit a range at map time either,
    // and `MAP_PRIVATE|MAP_ANONYMOUS` because `VM_INHERIT_COPY` - the default,
    // and the only inheritance a `fork(2)` child can be given over a private
    // range - is exactly what those two mean.
    let host_flags = libc::MAP_PRIVATE
        | libc::MAP_ANONYMOUS
        | libc::MAP_NORESERVE
        | if anywhere_requested { 0 } else { libc::MAP_FIXED };
    // SAFETY: an anonymous mapping of `size` bytes, or `MAP_FAILED`. A fixed
    // request names a page-aligned address the caller checked is free above; an
    // anywhere request passes 0 and lets the host choose.
    let got = unsafe {
        libc::mmap(
            if anywhere_requested {
                std::ptr::null_mut()
            } else {
                hint as *mut c_void
            },
            size as usize,
            prot,
            host_flags,
            -1,
            0,
        )
    };
    if got == libc::MAP_FAILED {
        let err = std::io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(libc::EINVAL) => KERN_INVALID_ARGUMENT,
            Some(libc::ENOMEM) => KERN_NO_SPACE,
            _ => KERN_RESOURCE_SHORTAGE,
        };
    }
    // SAFETY: `got` is a whole mapping of `size` bytes from the call above.
    unsafe { *address = got as u64 };
    if max_protection != current_protection {
        note(
            "vm_map",
            &SAID_PROT,
            &format!(
                "max_protection {max_protection:#x} is not recorded as a ceiling: Linux has no \
                 per-mapping limit on later protection changes and mmap(2) takes a protection \
                 with no maximum beside it, so this range can be re-opened beyond \
                 {max_protection:#x}. current_protection {current_protection:#x} was applied"
            ),
        );
    }
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
    static SAID_CEILING_ENTRY: Flag = Flag::new(false);

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
        return KERN_INVALID_ARGUMENT;
    };
    // Mach accepts a zero-length `vm_protect` and does nothing; `mprotect` with
    // a zero length succeeds too, and saying so here keeps the two agreeing.
    if size == 0 {
        return KERN_SUCCESS;
    }
    // **The one ceiling this does enforce is a memory entry's**, and it is a
    // different kind of promise from the caller's own `max_protection`. A caller
    // names its own ceiling and can be trusted to stay under it; a memory
    // entry's is a property of the *pages*, and a guest that could re-open a
    // read-only region with `vm_protect` would be writing into memory
    // `ConvertToReadOnly` promised nobody could write to - a silent security
    // hole rather than a slow answer. XNU refuses it in `vm_protect` itself
    // (`if ((max_protection & protbits) != max_protection) return
    // KERN_PROTECTION_FAILURE;`), so this is the kernel's own code and its own
    // return value. `None` is the answer for a range that is not a memory
    // entry's, and the ordinary "not honoured" note below still applies there.
    if let Some(ceiling) = super::memory_object::ceiling_of(address) {
        let asked = current_protection | max_protection;
        if asked & !ceiling != 0 {
            note(
                "vm_protect",
                &SAID_CEILING_ENTRY,
                &format!(
                    "{address:#x}+{size:#x} is a mapping of a memory entry whose maximum \
                     protection is {ceiling:#x}, and {asked:#x} was asked of it. Mach answers \
                     KERN_PROTECTION_FAILURE here, and the promise being enforced is the one \
                     base::subtle::PlatformSharedMemoryRegion::ConvertToReadOnly makes when it \
                     replaces a writable handle with a read-only one"
                ),
            );
            return KERN_PROTECTION_FAILURE;
        }
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

// ---------------------------------------------------------------------------
// The `mach_vm_*` family
//
// `mach_vm_map`, `mach_vm_remap`, `mach_vm_region`, `mach_vm_allocate`,
// `mach_vm_deallocate`, `mach_vm_read` and `mach_vm_write`. All seven share one
// MIG definition file, `osfmk/mach/mach_vm.defs`, and all seven are the
// 64-bit-wide spellings of a `vm_*` call. The prototypes below are the ones
// `usr/include/mach/mach_vm.h` declares, read against the argument registers of
// every call site in `Chromium Framework` - see each function's own note.
// ---------------------------------------------------------------------------

/// `VM_INHERIT_NONE`, which is `<mach/vm_inherit.h>`'s third value and the
/// value every `mach_vm_map` and `mach_vm_remap` in the image passes
/// (`push 0x2` at `+0x7e5f24c`, `+0x7e6065f`, `+0x7e60692` and `+0x7e6071e`).
/// `<mach/vm_inherit.h>` numbers `VM_INHERIT_SHARE` 0, `VM_INHERIT_COPY` 1,
/// `VM_INHERIT_NONE` 2 and `VM_INHERIT_DONATE_COPY` 3, and sets
/// `VM_INHERIT_LAST_VALID` to `VM_INHERIT_NONE` - so 3 is the one that is a
/// defined constant *and* not accepted here, because donating the source
/// mapping away is a thing `mmap(2)` cannot do.
const VM_INHERIT_NONE: u32 = 2;

/// `VM_FLAGS_FIXED`, which is **zero** - `osfmk/mach/vm_statistics.h` says so in
/// as many words, "really the absence of `VM_FLAGS_ANYWHERE`", and
/// `#define VM_FLAGS_FIXED 0x00000000`. A fixed request is therefore
/// `flags & VM_FLAGS_ANYWHERE == 0` and nothing else, which is exactly how the
/// image tells its two calls apart at `+0x7e60659` and `+0x7e60689`.
///
/// `pub` because it is the one flag whose value a reader has to be told twice:
/// every other `VM_FLAGS_*` is a bit, and this one is the absence of one.
pub const VM_FLAGS_FIXED: u32 = 0x0000;

/// Every `flags` bit `mach_vm_map` and `mach_vm_remap` are answered for.
///
/// The values are XNU's, from `osfmk/mach/vm_statistics.h:277-290`:
///
/// ```text
/// #define VM_FLAGS_FIXED                  0x00000000
/// #define VM_FLAGS_ANYWHERE               0x00000001
/// #define VM_FLAGS_PURGABLE               0x00000002
/// #define VM_FLAGS_4GB_CHUNK              0x00000004
/// #define VM_FLAGS_RANDOM_ADDR            0x00000008
/// #define VM_FLAGS_NO_CACHE               0x00000010
/// #define VM_FLAGS_RESILIENT_CODESIGN     0x00000020
/// #define VM_FLAGS_RESILIENT_MEDIA        0x00000040
/// #define VM_FLAGS_PERMANENT              0x00000080
/// #define VM_FLAGS_TPRO                   0x00001000
/// #define VM_FLAGS_OVERWRITE              0x00004000
/// #define VM_FLAGS_SUPERPAGE_MASK         0x00070000
/// #define VM_FLAGS_RETURN_DATA_ADDR       0x00100000
/// #define VM_FLAGS_RETURN_4K_DATA_ADDR    0x00800000
/// #define VM_FLAGS_ALIAS_MASK             0xFF000000
/// ```
///
/// `VM_FLAGS_FIXED` is **zero** - the header says so in as many words, "really
/// the absence of `VM_FLAGS_ANYWHERE`" - which is why the distinction between a
/// fixed and an anywhere request is `flags & VM_FLAGS_ANYWHERE` and nothing
/// else. The image relies on that: the two calls at `+0x7e60668` and
/// `+0x7e6069b` are the *same* eleven arguments with `r8d` set to `0` and then
/// to `1`, i.e. "map at the address I asked for, and if that will not go, map
/// anywhere at all".
///
/// `VM_FLAGS_SUPERPAGE_MASK` and `VM_FLAGS_ALIAS_MASK` are not in
/// `VM_FLAGS_USER_MAP`'s low half but `VM_FLAGS_SUPERPAGE_MASK` is, so a
/// superpage request reaches the kernel and is answered here by refusing it
/// rather than by silently handing back base pages.
const MACH_VM_FLAGS_HONOURED: u32 = VM_FLAGS_ANYWHERE | VM_FLAGS_OVERWRITE;

/// `(mask, name)` for every `VM_FLAGS_*` mask `mach_vm_map` and `mach_vm_remap`
/// are **not** answered for, with the values XNU gives them at
/// `osfmk/mach/vm_statistics.h:277-290`.
///
/// The entries are the header's *masks*, not single bits, which is why
/// `VM_FLAGS_SUPERPAGE_MASK` and `VM_FLAGS_ALIAS_MASK` appear as the several-bit
/// masks they are - and why the value handed back with a name is the mask rather
/// than a bit out of it, since that is what a reader needs to see in a refusal.
const MACH_VM_FLAGS_UNHONOURABLE: &[(u32, &str)] = &[
    (VM_FLAGS_PURGABLE, "VM_FLAGS_PURGABLE"),
    (0x0000_0004, "VM_FLAGS_4GB_CHUNK"),
    (VM_FLAGS_RANDOM_ADDR, "VM_FLAGS_RANDOM_ADDR"),
    (VM_FLAGS_NO_CACHE, "VM_FLAGS_NO_CACHE"),
    (VM_FLAGS_RESILIENT_CODESIGN, "VM_FLAGS_RESILIENT_CODESIGN"),
    (VM_FLAGS_RESILIENT_MEDIA, "VM_FLAGS_RESILIENT_MEDIA"),
    (0x0000_0080, "VM_FLAGS_PERMANENT"),
    (0x0000_1000, "VM_FLAGS_TPRO"),
    (0x0007_0000, "VM_FLAGS_SUPERPAGE_MASK"),
    (0x0010_0000, "VM_FLAGS_RETURN_DATA_ADDR"),
    (0x0080_0000, "VM_FLAGS_RETURN_4K_DATA_ADDR"),
    (0xff00_0000, "VM_FLAGS_ALIAS_MASK"),
];

/// The first `flags` mask that is set and not honoured, as `(mask, name)`.
fn unhonourable_mach_vm_flag_of(flags: u32) -> Option<(u32, &'static str)> {
    MACH_VM_FLAGS_UNHONOURABLE
        .iter()
        .find(|(mask, _)| flags & mask != 0 && flags & mask & MACH_VM_FLAGS_HONOURED == 0)
        .map(|(mask, name)| (*mask, *name))
}

/// The page size this host maps with, or 0 before [`super::info`] has published
/// it. Everything that rounds or aligns asks here, and 0 is checked rather than
/// rounded by: `page - 1` on a 0 would underflow, and the only honest answer
/// from a call that cannot round is a refusal.
fn page_size() -> u64 {
    let published = super::info::VM_PAGE_SIZE.load(std::sync::atomic::Ordering::Relaxed);
    if published != 0 {
        return published;
    }
    super::info::publish_page_size();
    super::info::VM_PAGE_SIZE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether `[start, end)` overlaps a mapping this process already has, or
/// `None` when the host would not say.
///
/// Only consulted for a **fixed** request that did *not* ask to overwrite,
/// which is the one case where `mmap(2)` and Mach disagree if `MAP_FIXED` is
/// used: Linux's `MAP_FIXED` replaces whatever is there, and Mach without
/// `VM_FLAGS_OVERWRITE` returns `KERN_NO_SPACE` instead - `vm_map_enter` calls
/// `vm_map_locate_space_fixed`, which returns `KERN_MEMORY_PRESENT` for a
/// conflicting entry, and turns that into `KERN_NO_SPACE` for the caller
/// (`osfmk/vm/vm_map.c`, the `if (result == KERN_MEMORY_PRESENT)` block).
///
/// The file is the same one [`region_at_or_after`] reads, so this costs no new
/// dependency on the host, and it is read rather than inferred: `/proc` cannot
/// lie about what is mapped. `None` is a real third answer - a fixed map that
/// silently replaced an existing mapping would be the worst possible outcome,
/// and it must not be reported as "it was already mapped" when nothing was
/// actually read.
pub(crate) fn range_is_mapped(start: u64, end: u64) -> Option<bool> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    Some(
        maps.lines()
            .any(|line| parse(line).is_some_and(|region| region.start < end && start < region.end)),
    )
}

/// `kern_return_t mach_vm_map(vm_map_t target_task, mach_vm_address_t *address,
/// mach_vm_size_t size, mach_vm_offset_t mask, int flags, mem_entry_name_port_t
/// object, memory_object_offset_t offset, boolean_t copy, vm_prot_t
/// cur_protection, vm_prot_t max_protection, vm_inherit_t inheritance)`
///
/// **Eleven arguments, and the fourth one is a mask, not a flag.** The
/// prototype is `usr/include/mach/mach_vm.h`'s and XNU's MIG definition agrees
/// (`osfmk/mach/mach_vm.defs:354`, `routine PREFIX(KERNEL_SERVER_SUFFIX(
/// mach_vm_map))`), where the seventh parameter is the `mem_entry_name_port_t`
/// and **the second is `inout`**:
///
/// ```text
///         target_task     : vm_task_entry_t;
/// inout  address         : mach_vm_address_t;
///         size            : mach_vm_size_t;
///         mask            : mach_vm_offset_t;
///         flags           : int;
///         object          : mem_entry_name_port_t;
///         offset          : memory_object_offset_t;
///         copy            : boolean_t;
///         cur_protection  : vm_prot_t;
///         max_protection  : vm_prot_t;
///         inheritance     : vm_inherit_t);
/// ```
///
/// Three call sites in the image, all of which this tree can be checked
/// against. `+0x7e60668` and `+0x7e6069b` are the two halves of one wrapper
/// (`+0x7e60610`, behind a vtable, in the neighbourhood of the V8 sandbox):
///
/// ```text
///   7e6065c  mov r9d, r14d              ; object = the memory entry
///   7e60657  xor ecx, ecx               ; mask = 0
///   7e60659  xor r8d, r8d               ; flags = VM_FLAGS_FIXED
///   ...       push rbx                  ; offset
///   ...       push 0                    ; copy = false
///   ...       push r13 / push r13        ; cur_protection / max_protection
///   ...       push 2                    ; inheritance = VM_INHERIT_NONE
///   7e60668  call _mach_vm_map          ; <- fixed
///   ...       test eax, eax / je ok
///   7e60689  mov r8d, 0x1               ; flags = VM_FLAGS_ANYWHERE
///   7e6066d  call _mach_vm_map          ; <- anywhere, everything else identical
/// ```
///
/// and `+0x7e5f255`, in the function that carries V8's
/// `VirtualAddressSubspace::FreePages` string, is the same eleven arguments
/// with `mov r8d, 0x4000` - `VM_FLAGS_OVERWRITE` - which is how it takes a
/// reserved range back.
///
/// ### `*address` really is in/out
///
/// The image proves it rather than the header claiming it. Both register
/// arguments `rsi` and `rdx` are **pointers** (`lea rsi, [rbp-0x30]` and
/// `lea rdx, [r15]`), and the function that makes the `ANYWHERE` call reads the
/// result back out of the very same word it passed in and returns it
/// (`+0x7e606a8`: `mov r12, QWORD PTR [rbp-0x30]`, then `mov rax, r12`). A
/// fixed request must be honoured at the address given and an anywhere request
/// must ignore it and report where the mapping landed, and the code is written
/// to need both from one word.
///
/// The protections come out of a six-entry table the image carries twice, at
/// `+0xd3caf60` and `+0xd3cae8c`, indexed by a value the wrapper range-checks
/// against 6: `0, 1, 3, 7, 5, 0` - `VM_PROT_NONE`, `VM_PROT_READ`,
/// `VM_PROT_READ|VM_PROT_WRITE`, `VM_PROT_ALL`, `VM_PROT_READ|VM_PROT_EXECUTE`,
/// `VM_PROT_NONE`. So `cur_protection` can legitimately be `VM_PROT_NONE`, and
/// the mapping has to come back with no access at all.
///
/// ### What is answered, and what is not
///
/// - **`object != MACH_PORT_NULL` is refused by name**, and the message says
///   which shape it is: the three sites that reach a non-null object here are
///   `v8::Sandbox`'s own reservation, not a shared region. A memory entry **is**
///   mapped - by [`vm_map`], whose sixth argument is the same word, through
///   [`super::memory_object`].
/// - **`copy != 0` is refused by name**: a copy-on-write clone of a memory
///   object, which an anonymous mapping has no meaning for.
/// - **`VM_FLAGS_PURGABLE` and every other unlisted flag is refused by name**,
///   and so is a protection word with a bit outside `VM_PROT_ALL` - the same
///   rule [`vm_map`] uses.
/// - **`max_protection` is not recorded as a ceiling**, for the reason
///   [`vm_protect`] gives: Linux has no per-mapping limit on later protection
///   changes. Named once, not silently dropped.
/// - **`inheritance` is only `VM_INHERIT_SHARE`, `VM_INHERIT_COPY` or
///   `VM_INHERIT_NONE`.** All three describe what a `fork(2)` child inherits
///   over a range this host either shares or copies, which is what
///   `MAP_SHARED` and `MAP_PRIVATE` already do. `VM_INHERIT_DONATE_COPY` is a
///   defined constant and is refused: it deletes the parent's mapping, and
///   `mmap(2)` has no way to say that.
///
/// # Safety
///
/// `address` is a writable `mach_vm_address_t`.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn mach_vm_map(
    task: Name,
    address: *mut u64,
    size: u64,
    mask: u64,
    flags: u32,
    object: Name,
    offset: u64,
    copy: u32,
    cur_protection: i32,
    max_protection: i32,
    inheritance: u32,
) -> i32 {
    static SAID_TASK: Flag = Flag::new(false);
    static SAID_NULL: Flag = Flag::new(false);
    static SAID_OBJECT: Flag = Flag::new(false);
    static SAID_FLAG: Flag = Flag::new(false);
    static SAID_COPY: Flag = Flag::new(false);
    static SAID_INHERIT: Flag = Flag::new(false);
    static SAID_PROT: Flag = Flag::new(false);
    static SAID_ALIGN: Flag = Flag::new(false);
    static SAID_EMPTY: Flag = Flag::new(false);
    static SAID_MASK: Flag = Flag::new(false);
    static SAID_CLASH: Flag = Flag::new(false);
    static SAID_CEILING: Flag = Flag::new(false);

    if address.is_null() {
        note("mach_vm_map", &SAID_NULL, "a null `mach_vm_address_t *`");
        return KERN_INVALID_ADDRESS;
    }
    if task != TASK_SELF {
        note(
            "mach_vm_map",
            &SAID_TASK,
            &format!("task {task:#x} is not this task ({TASK_SELF:#x})"),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if let Some((bit, name)) = unhonourable_mach_vm_flag_of(flags) {
        note(
            "mach_vm_map",
            &SAID_FLAG,
            &format!(
                "flags {flags:#x} include {name} ({bit:#x}); only VM_FLAGS_FIXED (0), \
                 VM_FLAGS_ANYWHERE ({VM_FLAGS_ANYWHERE:#x}) and VM_FLAGS_OVERWRITE \
                 ({VM_FLAGS_OVERWRITE:#x}) are honoured here. VM_FLAGS_PURGABLE would make the \
                 range reclaimable on a purge this host has no queue for, and the rest change \
                 what the mapping is"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if object != MACH_PORT_NULL {
        note(
            "mach_vm_map",
            &SAID_OBJECT,
            &format!(
                "object {object:#x} is a memory entry, and the pages have to be shared with \
                 whatever holds it; an anonymous mapping would answer with private copies, which \
                 is a plausible wrong answer rather than a slow one. **This call is refused even \
                 though a memory entry is now something this tree can map** - see \
                 super::memory_object - because the three sites that reach a non-null object here \
                 (+0x7e60668, +0x7e6069b and +0x7e5f255) are v8::Sandbox's own reservation, \
                 not a shared region: the function at +0x7e60610 range-checks its second argument \
                 against 6 and indexes the six-entry protection table at +0xd3caf60 (0, 1, 3, 7, \
                 5, 0 - v8::Sandbox::Protection, kNone .. kRemoved), and +0x7e5f255 is in the \
                 function carrying VirtualAddressSubspace::FreePages. A memory entry is mapped by \
                 vm_map, whose sixth argument is the same word and which answers it"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if copy != 0 {
        note(
            "mach_vm_map",
            &SAID_COPY,
            &format!(
                "copy {copy} asks for a copy-on-write clone of a memory object; with object \
                 MACH_PORT_NULL there is nothing to clone"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if inheritance > VM_INHERIT_NONE {
        note(
            "mach_vm_map",
            &SAID_INHERIT,
            &format!(
                "inheritance {inheritance} is not one of VM_INHERIT_SHARE (0), VM_INHERIT_COPY \
                 (1) or VM_INHERIT_NONE ({VM_INHERIT_NONE}); VM_INHERIT_DONATE_COPY (3) deletes \
                 the parent's mapping, which mmap(2) has no way to express"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    let Some(prot) = host_protection(cur_protection) else {
        note(
            "mach_vm_map",
            &SAID_PROT,
            &format!(
                "cur_protection {cur_protection:#x} has a bit outside VM_PROT_ALL \
                 ({VM_PROT_ALL:#x}) in it"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    };
    if max_protection & !VM_PROT_ALL != 0 {
        note(
            "mach_vm_map",
            &SAID_PROT,
            &format!(
                "max_protection {max_protection:#x} has bits outside VM_PROT_ALL \
                 ({VM_PROT_ALL:#x}) in it"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    // `size == 0` and a `offset` that is not page-aligned are the two argument
    // mistakes the kernel names first, and they are the **only** two failures
    // that clear `*address`: `osfmk/vm/vm_map.c` reads
    // `if (size == 0 || (offset & MIN(VM_MAP_PAGE_MASK(map), PAGE_MASK_64)) != 0)
    // { *address = 0; return KERN_INVALID_ARGUMENT; }`, and the `BailOut` path
    // every other failure takes removes the new mapping *using* `*address`
    // without ever writing to it. So the request is left standing on every other
    // error, and that is load-bearing rather than pedantic: the image's own
    // wrapper calls this once with `VM_FLAGS_FIXED` and, on failure, again with
    // `VM_FLAGS_ANYWHERE` and the **same** `*address` word
    // (`Chromium Framework+0x7e60668` then `+0x7e6069b`), and the second call's
    // `*address` is an anywhere *hint*. Zeroing it would throw the hint away.
    //
    // SAFETY: the caller's `address`, checked non-null above.
    let hint = unsafe { std::ptr::read(address) };
    let page = page_size();
    if page == 0 {
        // The only host this can happen on is one that would not answer
        // `sysconf(_SC_PAGESIZE)`, and there is no page size to round or align
        // with. Refused rather than assumed: `page - 1` on a zero would
        // underflow and the next line would claim the caller's offset was
        // misaligned.
        note(
            "mach_vm_map",
            &SAID_ALIGN,
            "this host published no page size, so there is nothing to round or align a mapping to",
        );
        return KERN_INVALID_ARGUMENT;
    }
    if size == 0 {
        // SAFETY: as above.
        unsafe { *address = 0 };
        return KERN_INVALID_ARGUMENT;
    }
    if offset & (page - 1) != 0 {
        note(
            "mach_vm_map",
            &SAID_EMPTY,
            &format!(
                "offset {offset:#x} is not a multiple of the {page:#x} page size; Mach \
                 refuses this and writes *address = 0, which is what happens here"
            ),
        );
        // SAFETY: as above.
        unsafe { *address = 0 };
        return KERN_INVALID_ARGUMENT;
    }
    let anywhere = flags & VM_FLAGS_ANYWHERE != 0;
    // `mask` is an **alignment mask**, not a size: `vm_map_locate_space_fixed`
    // rejects a fixed request whose address does not satisfy `(start & mask) == 0`
    // with `KERN_NO_SPACE`, and `vm_map_locate_space_anywhere` picks a hole that
    // does. The image passes `0` at all three sites, so this is the shape that
    // has to be right for the one that matters and the rest is stated rather
    // than guessed.
    if mask != 0 && hint & mask != 0 && !anywhere {
        note(
            "mach_vm_map",
            &SAID_MASK,
            &format!(
                "a fixed request for {hint:#x} violates the alignment mask {mask:#x}; Mach \
                 answers KERN_NO_SPACE for this, because the address is where the mapping has \
                 to be and it does not satisfy (address & mask) == 0"
            ),
        );
        return KERN_NO_SPACE;
    }
    if !anywhere {
        if hint & (page - 1) != 0 {
            // `mmap(2)` with `MAP_FIXED` needs a page-aligned address, and Mach's
            // mappings are page-granular by definition
            // (`VM_MAP_PAGE_ALIGNED(*address, ...)` is asserted on the way in).
            // Rounding the caller's request silently would move a fixed mapping
            // that the caller asked to be exactly here, so this is refused
            // instead - the same answer [`vm_allocate`] gives for the same reason.
            note(
                "mach_vm_map",
                &SAID_ALIGN,
                &format!(
                    "a fixed request for {hint:#x}, which is not a multiple of the {page:#x} \
                     page size; a Mach mapping is page-granular and MAP_FIXED cannot round one \
                     without moving the range the caller asked to be exactly here"
                ),
            );
            return KERN_INVALID_ARGUMENT;
        }
        if flags & VM_FLAGS_OVERWRITE == 0 {
            match range_is_mapped(hint, hint + size) {
                // Darwin's own answer, from the code path above: a conflicting
                // entry is `KERN_MEMORY_PRESENT` inside the kernel and
                // `KERN_NO_SPACE` to the caller. `MAP_FIXED` would have replaced
                // whatever was here.
                Some(true) => {
                    note(
                        "mach_vm_map",
                        &SAID_CLASH,
                        &format!(
                            "{hint:#x}+{size:#x} is already mapped and VM_FLAGS_OVERWRITE \
                             ({VM_FLAGS_OVERWRITE:#x}) was not set, so Mach answers \
                             KERN_NO_SPACE; MAP_FIXED would have replaced it"
                        ),
                    );
                    return KERN_NO_SPACE;
                }
                // Nothing was read, so nothing is claimed: the call is refused,
                // and the refusal says why rather than asserting a clash it did
                // not see.
                None => {
                    note(
                        "mach_vm_map",
                        &SAID_CLASH,
                        &format!(
                            "{hint:#x}+{size:#x} was asked for as a fixed mapping without \
                             VM_FLAGS_OVERWRITE and /proc/self/maps could not be read, so whether \
                             anything is there is unknown. MAP_FIXED would have replaced it \
                             without asking, so the call is refused instead"
                        ),
                    );
                    return KERN_NO_SPACE;
                }
                Some(false) => {}
            }
        }
    }
    let want = if anywhere {
        align_up(hint, mask, page)
    } else {
        hint
    };
    // `MAP_NORESERVE` because Mach does not commit a range at map time either,
    // and `MAP_PRIVATE|MAP_ANONYMOUS` because `VM_INHERIT_COPY` - the default,
    // and the only inheritance a `fork(2)` child can be given over a private
    // range - is exactly what those two mean.
    let host_flags = libc::MAP_PRIVATE
        | libc::MAP_ANONYMOUS
        | libc::MAP_NORESERVE
        | if anywhere { 0 } else { libc::MAP_FIXED };
    // SAFETY: an anonymous mapping of `size` bytes, or `MAP_FAILED`. `want` is
    // either 0 (the kernel chooses) or a page-aligned address the caller named.
    let got = unsafe { libc::mmap(want as *mut c_void, size as usize, prot, host_flags, -1, 0) };
    if got == libc::MAP_FAILED {
        let err = std::io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(libc::EINVAL) => {
                note(
                    "mach_vm_map",
                    &SAID_ALIGN,
                    &format!(
                        "mmap refused {want:#x}+{size:#x} with protection {cur_protection:#x}: {err}"
                    ),
                );
                KERN_INVALID_ARGUMENT
            }
            Some(libc::ENOMEM) => {
                note(
                    "mach_vm_map",
                    &SAID_CLASH,
                    &format!("no room for {want:#x}+{size:#x}: {err}"),
                );
                KERN_NO_SPACE
            }
            _ => KERN_RESOURCE_SHORTAGE,
        };
    }
    let got = got as u64;
    if mask != 0 && got & mask != 0 {
        // Linux treats a non-fixed `mmap` address as a hint and is free to
        // ignore it, so a mask this host's allocator will not satisfy is a
        // refusal rather than an answer at the wrong alignment. The mapping
        // goes back first, so a refused call leaves nothing behind.
        // SAFETY: `got` is a whole mapping of `size` bytes from the call above.
        unsafe { libc::munmap(got as *mut c_void, size as usize) };
        note(
            "mach_vm_map",
            &SAID_MASK,
            &format!(
                "an anywhere request asked for alignment {mask:#x} and the host returned \
                 {got:#x}, which does not satisfy it; Linux treats the address as a hint, so \
                 the mapping was removed again rather than answered at the wrong alignment"
            ),
        );
        return KERN_NO_SPACE;
    }
    // The in/out contract: the address the mapping actually landed at, which for
    // a fixed request is the one asked for and for an anywhere request is not.
    //
    // SAFETY: the caller's `address`, checked non-null above.
    unsafe { *address = got };
    if max_protection != cur_protection {
        note(
            "mach_vm_map",
            &SAID_CEILING,
            &format!(
                "max_protection {max_protection:#x} is not recorded as a ceiling: Linux has no \
                 per-mapping limit on later protection changes and mmap(2) takes a protection \
                 with no maximum beside it, so this range can be re-opened beyond \
                 {max_protection:#x}. cur_protection {cur_protection:#x} was applied"
            ),
        );
    }
    KERN_SUCCESS
}

/// `hint` rounded up so that `(hint & mask) == 0` and `hint` is page-aligned.
/// `mask` of zero, or a mask with no page-alignable answer inside the address
/// space, gives 0 - the kernel's choice.
fn align_up(hint: u64, mask: u64, page: u64) -> u64 {
    if hint == 0 {
        return 0;
    }
    let align = (mask + 1).max(page);
    if !align.is_power_of_two() {
        return 0;
    }
    hint.checked_add(align - 1).map_or(0, |v| v & !(align - 1))
}

/// `kern_return_t mach_vm_remap(vm_map_t target_task, mach_vm_address_t
/// *target_address, mach_vm_size_t size, mach_vm_offset_t mask, int flags,
/// vm_map_t src_task, mach_vm_address_t src_address, boolean_t copy, vm_prot_t
/// *cur_protection, vm_prot_t *max_protection, vm_inherit_t inheritance)`
///
/// **A move within this address space is `mremap(2)`, and the image needs the
/// address honoured exactly.** Its one call site, `+0x7e60727`, in the thunk at
/// `+0x7e606d0`, is:
///
/// ```text
///   7e60713  xor ecx, ecx               ; mask = 0
///   7e60715  mov r8d, 0x4000            ; flags = VM_FLAGS_OVERWRITE
///   7e6071b  mov r9d, edi               ; src_task = mach_task_self()
///   ...       push 2                    ; inheritance = VM_INHERIT_NONE
///   ...       push r10 / push r11        ; &max_protection / &cur_protection
///   ...       push 0                    ; copy = false
///   ...       push rax                  ; src_address
///   7e60727  call _mach_vm_remap
///   7e60734  cmp QWORD PTR [rbp-0x18], rbx ; the *target_address it asked for
///   7e60738  jne 0x7e60749                 ; -> int3 / ud2
/// ```
///
/// So a `remap` that succeeds somewhere other than where it was asked to put the
/// range does not return an error: it traps. `flags` is
/// `VM_FLAGS_OVERWRITE` with no `VM_FLAGS_ANYWHERE`, which is a **fixed**
/// remap, and `*target_address` is in/out - it is seeded with the requested
/// address and compared against itself afterwards.
///
/// `mremap(old, oldlen, newlen, MREMAP_FIXED, new_addr)` is the same operation
/// with the same refusal to move, so it is what this calls. `cur_protection`
/// and `max_protection` are **out** parameters here (the image passes two stack
/// slots and reads neither), and the protections come from the caller's own
/// table, so they are written back as the source's protection.
///
/// A remap whose `src_task` is not this process, or that asks to `copy`, is
/// refused by name: both need a second address space or a second copy of a
/// memory object, and this host has one address space and no VM objects.
///
/// # Safety
///
/// `target_address`, and `cur_protection`/`max_protection` when non-null, are
/// writable.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn mach_vm_remap(
    task: Name,
    target_address: *mut u64,
    size: u64,
    mask: u64,
    flags: u32,
    src_task: Name,
    src_address: u64,
    copy: u32,
    cur_protection: *mut i32,
    max_protection: *mut i32,
    inheritance: u32,
) -> i32 {
    static SAID_TASK: Flag = Flag::new(false);
    static SAID_NULL: Flag = Flag::new(false);
    static SAID_FLAG: Flag = Flag::new(false);
    static SAID_COPY: Flag = Flag::new(false);
    static SAID_INHERIT: Flag = Flag::new(false);
    static SAID_ALIGN: Flag = Flag::new(false);
    static SAID_SIZE: Flag = Flag::new(false);

    if target_address.is_null() {
        note("mach_vm_remap", &SAID_NULL, "a null `mach_vm_address_t *`");
        return KERN_INVALID_ADDRESS;
    }
    if task != TASK_SELF || src_task != TASK_SELF {
        note(
            "mach_vm_remap",
            &SAID_TASK,
            &format!(
                "target_task {task:#x} / src_task {src_task:#x}: this host has one address \
                 space, described by mach_task_self() ({TASK_SELF:#x}), and a remap between two \
                 different tasks cannot be expressed"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if copy != 0 {
        note(
            "mach_vm_remap",
            &SAID_COPY,
            &format!(
                "copy {copy} asks for a second copy of the source's memory object rather than a \
                 move; mremap(2) moves"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if let Some((bit, name)) = unhonourable_mach_vm_flag_of(flags) {
        note(
            "mach_vm_remap",
            &SAID_FLAG,
            &format!(
                "flags {flags:#x} include {name} ({bit:#x}), which changes what the remapping is"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if inheritance > VM_INHERIT_NONE {
        note(
            "mach_vm_remap",
            &SAID_INHERIT,
            &format!("inheritance {inheritance} is not a vm_inherit_t this host can give a range"),
        );
        return KERN_INVALID_ARGUMENT;
    }
    let page = page_size();
    if page == 0 {
        note(
            "mach_vm_remap",
            &SAID_ALIGN,
            "this host published no page size, so a Mach mapping's page granularity is unknown and \
             the range cannot be checked against it",
        );
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller's `target_address`, checked non-null above.
    let want = unsafe { std::ptr::read(target_address) };
    // A zero-length range is refused. `*target_address` is **not** cleared for
    // it: the one place the kernel zeroes `*address` is `vm_map_enter`'s
    // `size == 0 || misaligned offset` guard, and `vm_remap` is a different
    // routine with no such line, so a failed remap leaves the caller's request
    // standing the way every other failure does.
    if size == 0 {
        return KERN_INVALID_ARGUMENT;
    }
    if src_address & (page - 1) != 0 || want & (page - 1) != 0 || size & (page - 1) != 0 {
        note(
            "mach_vm_remap",
            &SAID_ALIGN,
            &format!(
                "source {src_address:#x} + {size:#x} -> target {want:#x}: a Mach mapping is \
                 page-granular ({page:#x}) and mremap(2) will not round a range it was not asked \
                 to round"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if mask != 0 && want & mask != 0 {
        note(
            "mach_vm_remap",
            &SAID_ALIGN,
            &format!("the fixed target {want:#x} violates the alignment mask {mask:#x}"),
        );
        return KERN_NO_SPACE;
    }
    // The protections come back from the source, because that is the only place
    // this tree can read a protection from. `vm_region_64` is the reader, and
    // the range it is asked about is the source.
    let mut protection = 0_i32;
    if !cur_protection.is_null() || !max_protection.is_null() {
        let mut address = src_address;
        let mut length = size;
        let mut info = [0_u32; size_of::<BasicInfo64>() / size_of::<u32>()];
        let mut count = info.len() as u32;
        // SAFETY: every out-parameter is a live local, and `info` is a
        // `capacity`-word buffer as `count` says.
        let status = unsafe {
            region(
                "mach_vm_remap",
                TASK_SELF,
                &mut address,
                &mut length,
                VM_REGION_BASIC_INFO_64,
                info.as_mut_ptr().cast(),
                &mut count,
                std::ptr::null_mut(),
            )
        };
        if status != KERN_SUCCESS {
            // A source range this process cannot describe has no protection to
            // report, and reporting `VM_PROT_ALL` would be a guess in the
            // permissive direction.
            note(
                "mach_vm_remap",
                &SAID_SIZE,
                &format!(
                    "the source range {src_address:#x}+{size:#x} could not be described \
                     (kr {status}), so cur_protection/max_protection have nothing truthful to \
                     report and the remap is refused"
                ),
            );
            return status;
        }
        // SAFETY: `count` words of the `info` above were written.
        let basic = unsafe { info.as_ptr().cast::<BasicInfo64>().read_unaligned() };
        protection = basic.protection;
    }
    // SAFETY: the caller's ranges, and `mremap` writes nothing outside them.
    //
    // `MREMAP_MAYMOVE` has to be there as well as `MREMAP_FIXED`, and that is not
    // a detail: `mremap(2)` documents that `MREMAP_FIXED` "implies
    // MREMAP_MAYMOVE", and the kernel's `mremap_to` returns `EINVAL` for
    // `MREMAP_FIXED` on its own - measured on this host, not recalled. With an
    // explicit `new_addr` the permission to move is not exercised: the result
    // is `new_addr` or the call fails, which is the refusal to move that the
    // image's `cmp`/`int3` after the call depends on.
    let got = unsafe {
        libc::mremap(
            src_address as *mut c_void,
            size as usize,
            size as usize,
            libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED,
            want as *mut c_void,
        )
    };
    if got == libc::MAP_FAILED {
        let err = std::io::Error::last_os_error();
        note(
            "mach_vm_remap",
            &SAID_SIZE,
            &format!("mremap {src_address:#x}+{size:#x} -> {want:#x} failed: {err}"),
        );
        return match err.raw_os_error() {
            Some(libc::ENOMEM) => KERN_NO_SPACE,
            Some(libc::EINVAL) => KERN_INVALID_ARGUMENT,
            _ => KERN_FAILURE,
        };
    }
    let got = got as u64;
    // SAFETY: the caller's out-parameters, `target_address` checked non-null
    // above.
    unsafe {
        *target_address = got;
        if !cur_protection.is_null() {
            *cur_protection = protection;
        }
        if !max_protection.is_null() {
            *max_protection = protection;
        }
    }
    KERN_SUCCESS
}

/// `kern_return_t mach_vm_allocate(vm_map_t target, mach_vm_address_t *address,
/// mach_vm_size_t size, int flags)`
///
/// Four arguments, and the fourth is `flags` - the same word `vm_allocate`
/// receives as its fourth argument and called `anywhere`. `mach_vm_allocate` is
/// the 64-bit-address spelling of the same call: `usr/include/mach/mach_vm.h`
/// declares it with `mach_vm_address_t *` where `vm_prot.h` declares
/// `vm_address_t *`, and both are 64-bit here. So this is [`vm_allocate`] with
/// its spelling changed, and the note on that function - which is about the
/// `vm_tag_t` Chromium packs into the high sixteen bits of the flag word - is
/// this one's note too.
///
/// **Nothing in the shipped image imports it.** `Chromium Framework` stubs
/// `_mach_vm_allocate` nowhere: the two anonymous-allocation sites at
/// `+0x6311fe` and `+0x6312ab` are `_vm_allocate`, and the three map sites use
/// `_mach_vm_map`. So this is here because the name is otherwise an unbound
/// import, not because a call site is waiting on it.
///
/// # Safety
///
/// `address` is a writable `mach_vm_address_t`.
pub unsafe extern "C" fn mach_vm_allocate(
    task: Name,
    address: *mut u64,
    size: u64,
    flags: u32,
) -> i32 {
    // SAFETY: forwarded unchanged, and `vm_allocate`'s own contract is the same
    // four words.
    unsafe { vm_allocate(task, address, size, flags) }
}

/// `kern_return_t mach_vm_deallocate(vm_map_t target, mach_vm_address_t address,
/// mach_vm_size_t size)`
///
/// The 64-bit spelling of [`vm_deallocate`]: three words, the same three words.
/// Nothing in the shipped image imports it either.
///
/// # Safety
///
/// `address` and `size` describe a mapping this process made and is done with.
pub unsafe extern "C" fn mach_vm_deallocate(task: Name, address: u64, size: u64) -> i32 {
    // SAFETY: forwarded unchanged.
    unsafe { vm_deallocate(task, address, size) }
}

/// `kern_return_t mach_vm_read(vm_map_t target_task, mach_vm_address_t address,
/// mach_vm_size_t size, vm_offset_t *data, mach_msg_type_number_t *dataCnt)`
///
/// **Within this address space this is a copy, and `*dataCnt` is counted in
/// bytes.** The unit is XNU's, not this crate's: `osfmk/vm/vm_user.c:395` is
/// `mach_vm_read(map, addr, size, data, data_size)`, its first act is
/// `VM_SANITIZE_UNSAFE_FITS(size, mach_msg_type_number_t)` - "make sure we do
/// not overflow it" - and its last is `*data_size = size`. So `*dataCnt` is a
/// byte count on the way in and a byte count on the way out, and it is the
/// *only* difference from the `vm_region_64` in/out discipline besides the unit.
///
/// `data` is a `vm_offset_t *` out-parameter holding the address of a buffer,
/// which is what the MIG out-of-line reply carries on Darwin. Here the buffer
/// has to be the caller's own, so the value it arrives with is the destination
/// and it is written back unchanged.
///
/// **Only `chrome_crashpad_handler` imports this**, at `+0x1000bed42`, and it
/// is the only importer of anything in this family that is not the Framework.
/// The main executable and `Chromium Framework` do not import it at all, so this
/// exists so that the name is a bound function with a truthful answer rather
/// than a jump through a zero slot.
///
/// # Safety
///
/// `data` is a writable `vm_offset_t` holding the address of a buffer writable
/// for `*dataCnt` bytes, `address` is readable for `size` bytes, and the two do
/// not overlap.
pub unsafe extern "C" fn mach_vm_read(
    task: Name,
    address: u64,
    size: u64,
    data: *mut u64,
    data_count: *mut u32,
) -> i32 {
    static SAID_TASK: Flag = Flag::new(false);
    static SAID_NULL: Flag = Flag::new(false);
    static SAID_SHORT: Flag = Flag::new(false);

    if data.is_null() || data_count.is_null() {
        note("mach_vm_read", &SAID_NULL, "a null out-parameter");
        return KERN_INVALID_ADDRESS;
    }
    if task != TASK_SELF {
        note(
            "mach_vm_read",
            &SAID_TASK,
            &format!("task {task:#x} is not this task ({TASK_SELF:#x})"),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if size == 0 || address == 0 {
        return KERN_INVALID_ADDRESS;
    }
    // SAFETY: the caller's obligations, checked non-null above.
    let (into, capacity) = unsafe { (*data, *data_count as u64) };
    if into == 0 {
        note(
            "mach_vm_read",
            &SAID_NULL,
            "*data is MACH_PORT_NULL; Mach allocates the reply buffer out of line, and a \
             caller here has to name one",
        );
        return KERN_INVALID_ADDRESS;
    }
    if capacity < size {
        // The kernel's own guard is `VM_SANITIZE_UNSAFE_FITS(size,
        // mach_msg_type_number_t)`, i.e. that `size` fits the count at all; a
        // short buffer is the caller's own mistake and the count is what says so.
        note(
            "mach_vm_read",
            &SAID_SHORT,
            &format!("*dataCnt is {capacity} bytes and {size:#x} bytes were asked for"),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if size > u32::MAX as u64 {
        // `VM_SANITIZE_UNSAFE_FITS(size, mach_msg_type_number_t)`, again.
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller's obligations: `address` readable for `size`, `into`
    // writable for `size`, and the two do not overlap.
    unsafe {
        std::ptr::copy_nonoverlapping(address as *const u8, into as *mut u8, size as usize);
        *data_count = size as u32;
    }
    KERN_SUCCESS
}

/// `kern_return_t mach_vm_write(vm_map_t target_task, mach_vm_address_t address,
/// vm_offset_t data, mach_msg_type_number_t dataCnt)`
///
/// The other half of [`mach_vm_read`], and equally a copy within this address
/// space: `data` is a buffer address and `dataCnt` its length in **bytes**,
/// which is the count `mach_vm_read` reports and the same
/// `mach_msg_type_number_t`. Nothing in the shipped image imports it - the
/// crashpad handler imports `mach_vm_read` and not this - so it is here for the
/// same reason: a bound name beats a zero slot.
///
/// # Safety
///
/// `data` is readable for `data_count` bytes, `address` is writable for
/// `data_count` bytes, and the two do not overlap.
pub unsafe extern "C" fn mach_vm_write(
    task: Name,
    address: u64,
    data: u64,
    data_count: u32,
) -> i32 {
    static SAID_TASK: Flag = Flag::new(false);

    if task != TASK_SELF {
        note(
            "mach_vm_write",
            &SAID_TASK,
            &format!("task {task:#x} is not this task ({TASK_SELF:#x})"),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if address == 0 || data == 0 {
        return KERN_INVALID_ADDRESS;
    }
    if data_count == 0 {
        return KERN_SUCCESS;
    }
    // SAFETY: the caller's obligations: `data` readable and `address` writable
    // for `data_count` bytes, and the two do not overlap.
    unsafe {
        std::ptr::copy_nonoverlapping(data as *const u8, address as *mut u8, data_count as usize);
    }
    KERN_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::space::MACH_PORT_DEAD;

    /// The structure is 36 bytes with `offset` at 20, which is the whole reason
    /// this file declares it packed. A caller that offers the 9 words the image
    /// offers must have the file offset land where a `vm_region_basic_info_64`
    /// reader expects it, and the guest reads `offset` for a file-backed region
    /// by name.
    #[test]
    fn the_basic_info_structure_is_thirty_six_bytes_with_offset_at_twenty() {
        assert_eq!(size_of::<BasicInfo64>(), 36);
        assert_eq!(size_of::<BasicInfo64>() / size_of::<u32>(), 9);
        assert_eq!(VM_REGION_BASIC_INFO_COUNT_64, 9);
        assert_eq!(std::mem::offset_of!(BasicInfo64, protection), 0);
        assert_eq!(std::mem::offset_of!(BasicInfo64, max_protection), 4);
        assert_eq!(std::mem::offset_of!(BasicInfo64, inheritance), 8);
        assert_eq!(std::mem::offset_of!(BasicInfo64, shared), 12);
        assert_eq!(std::mem::offset_of!(BasicInfo64, reserved), 16);
        assert_eq!(std::mem::offset_of!(BasicInfo64, offset), 20);
        assert_eq!(std::mem::offset_of!(BasicInfo64, behavior), 28);
        assert_eq!(std::mem::offset_of!(BasicInfo64, user_wired_count), 32);
    }

    /// `mach_vm_region` and `vm_region_64` are one MIG routine under two names,
    /// so they answer identically for the flavour the image mostly asks for -
    /// which is the only way to know the second name was not wired to something
    /// else by accident.
    #[test]
    fn mach_vm_region_answers_what_vm_region_64_answers() {
        // A **static**, not a heap buffer: the probe has to be an address whose
        // surrounding mapping is the same on both calls, and a `Vec` on another
        // test's thread can grow the heap between them. A static's region is
        // part of this binary's data segment and does not move.
        static PROBE: [u8; 64] = [0; 64];
        let probe = (&raw const PROBE) as u64;
        let ask = |which: unsafe extern "C" fn(
            Name,
            *mut u64,
            *mut u64,
            i32,
            *mut c_void,
            *mut u32,
            *mut Name,
        ) -> i32| {
            let mut address = probe;
            let mut size = 0_u64;
            let mut info = [0xaaaa_aaaa_u32; 16];
            let mut count = VM_REGION_BASIC_INFO_COUNT_64 as u32;
            let mut object = 7_u32;
            // SAFETY: writable out-parameters, and a buffer whose capacity is
            // what `count` says.
            let status = unsafe {
                which(
                    TASK_SELF,
                    &mut address,
                    &mut size,
                    VM_REGION_BASIC_INFO_64,
                    info.as_mut_ptr().cast(),
                    &mut count,
                    &mut object,
                )
            };
            (status, address, size, info, count, object)
        };
        let a = ask(vm_region_64);
        let b = ask(mach_vm_region);
        assert_eq!(a.0, KERN_SUCCESS);
        assert_eq!(b.0, KERN_SUCCESS);
        assert_eq!(a.1, b.1);
        assert_eq!(a.2, b.2);
        assert_eq!(a.3, b.3);
        assert_eq!(a.4, VM_REGION_BASIC_INFO_COUNT_64 as u32);
        assert_eq!(b.4, VM_REGION_BASIC_INFO_COUNT_64 as u32);
        assert_eq!(a.5, b.5);
    }

    /// A flavour this tree publishes no layout for is refused by name rather
    /// than filled with the basic-info structure, which is what the two flavours
    /// the image also asks for - `VM_REGION_TOP_INFO` and
    /// `VM_REGION_EXTENDED_INFO` - would otherwise get.
    #[test]
    fn a_flavour_with_no_layout_here_is_refused_and_the_buffer_is_untouched() {
        for flavor in [10, 12, 13] {
            let mut address = 0_u64;
            let mut size = 0_u64;
            let mut info = [0xaaaa_aaaa_u32; 16];
            let mut count = VM_REGION_BASIC_INFO_COUNT_64 as u32;
            // SAFETY: writable out-parameters, and a buffer whose capacity is
            // what `count` says.
            let status = unsafe {
                mach_vm_region(
                    TASK_SELF,
                    &mut address,
                    &mut size,
                    flavor,
                    info.as_mut_ptr().cast(),
                    &mut count,
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(status, KERN_INVALID_ARGUMENT, "flavour {flavor}");
            assert!(info.iter().all(|w| *w == 0xaaaa_aaaa), "flavour {flavor}");
            assert_eq!(count, VM_REGION_BASIC_INFO_COUNT_64 as u32);
        }
    }

    /// The image's own shape: map a fixed address, and on failure the same
    /// request again with `VM_FLAGS_ANYWHERE`. The second call must not be able
    /// to lose the hint, which is why a refusal leaves `*address` alone.
    #[test]
    fn a_fixed_map_is_honoured_at_the_address_asked_for() {
        let page = page_size();
        // A page-aligned range far from the heap, the stack and the other fixed
        // tests below: `MAP_FIXED` is authoritative, so a fixed address that
        // another test in this binary also uses would be a race between two
        // tests rather than a failure of either.
        let want = 0x7000_0000_0000_u64 & !(page - 1);
        let mut address = want;
        // SAFETY: `address` is a live word and the mapping is this process's.
        let status = unsafe {
            mach_vm_map(
                TASK_SELF,
                &mut address,
                page,
                0,
                VM_FLAGS_FIXED | VM_FLAGS_OVERWRITE,
                MACH_PORT_NULL,
                0,
                0,
                VM_PROT_READ | VM_PROT_WRITE,
                VM_PROT_READ | VM_PROT_WRITE,
                VM_INHERIT_NONE,
            )
        };
        assert_eq!(status, KERN_SUCCESS);
        assert_eq!(address, want);
        // SAFETY: the mapping above.
        unsafe {
            assert_eq!(libc::munmap(address as *mut c_void, page as usize), 0);
        }
    }

    /// `VM_PROT_NONE` is in the protection table the image carries - `0` is the
    /// first of its six entries - so a mapping made with it has to come back
    /// with no access at all, not with a default.
    #[test]
    fn a_map_with_no_protection_has_no_protection() {
        let page = page_size();
        let mut address = 0_u64;
        // SAFETY: `address` is a live word and the mapping is this process's.
        let status = unsafe {
            mach_vm_map(
                TASK_SELF,
                &mut address,
                page,
                0,
                VM_FLAGS_ANYWHERE,
                MACH_PORT_NULL,
                0,
                0,
                VM_PROT_NONE,
                VM_PROT_NONE,
                VM_INHERIT_NONE,
            )
        };
        assert_eq!(status, KERN_SUCCESS);
        assert_ne!(address, 0);
        // `mprotect` is not the check - the kernel is the check. `/proc` says
        // what protection the range really has, and a mapping asked for with no
        // access has to show `---`.
        let maps = std::fs::read_to_string("/proc/self/maps").expect("this host has /proc");
        let line = maps
            .lines()
            .find(|line| {
                line.split_whitespace()
                    .next()
                    .and_then(|r| r.split_once('-'))
                    .is_some_and(|(lo, hi)| {
                        u64::from_str_radix(lo, 16).is_ok_and(|lo| lo == address)
                            && u64::from_str_radix(hi, 16).is_ok_and(|hi| hi == address + page)
                    })
            })
            .unwrap_or_else(|| panic!("the mapping at {address:#x} is not in /proc/self/maps"));
        assert!(
            line.starts_with(&format!("{address:x}-")),
            "the mapping at {address:#x} was not found where it was asked for: {line}"
        );
        let perms = line.split_whitespace().nth(1).unwrap_or("");
        assert_eq!(perms, "---p", "VM_PROT_NONE must map no access: {line}");
        // SAFETY: the mapping above, which is page-aligned by construction.
        unsafe { assert_eq!(libc::munmap(address as *mut c_void, page as usize), 0) };
    }

    /// The flag bits are XNU's, and the image passes exactly these three words:
    /// `0` at `+0x7e60659`, `1` at `+0x7e60689` and `0x4000` at `+0x7e5f242`.
    #[test]
    fn the_flag_bits_are_the_kernels() {
        assert_eq!(VM_FLAGS_ANYWHERE, 0x0000_0001);
        assert_eq!(VM_FLAGS_OVERWRITE, 0x0000_4000);
        assert_eq!(MACH_VM_FLAGS_HONOURED, 0x0000_4001);
        assert_eq!(VM_FLAGS_PURGABLE, 0x0000_0002);
        assert_eq!(VM_INHERIT_NONE, 2);
        // `VM_FLAGS_FIXED` is the *absence* of `VM_FLAGS_ANYWHERE`, so it is
        // zero and cannot be tested for as a bit that is present.
        assert_eq!(VM_FLAGS_FIXED, 0);
        // Every unhonourable mask must name itself, with the mask it is, and
        // the two honoured words must not be mistaken for one.
        for (mask, name) in MACH_VM_FLAGS_UNHONOURABLE {
            assert_eq!(
                unhonourable_mach_vm_flag_of(*mask).map(|(m, n)| (m, n)),
                Some((*mask, *name)),
                "{mask:#x}"
            );
            // Any single bit of a several-bit mask names it too, since a caller
            // may set one bit of `VM_FLAGS_ALIAS_MASK` on its own.
            for bit in 0..32u32 {
                if mask & (1 << bit) != 0 {
                    assert_eq!(
                        unhonourable_mach_vm_flag_of(1 << bit).map(|(_, n)| n),
                        Some(*name),
                        "bit {bit} of {mask:#x}"
                    );
                }
            }
        }
        assert!(unhonourable_mach_vm_flag_of(0).is_none());
        assert!(unhonourable_mach_vm_flag_of(VM_FLAGS_FIXED).is_none());
        assert!(unhonourable_mach_vm_flag_of(VM_FLAGS_OVERWRITE).is_none());
        assert!(unhonourable_mach_vm_flag_of(VM_FLAGS_ANYWHERE).is_none());
        assert!(unhonourable_mach_vm_flag_of(VM_FLAGS_OVERWRITE | 0x8).is_some());
    }

    /// A memory entry is refused rather than answered with an anonymous
    /// mapping, which is the one place where the answer would be a plausible
    /// lie: two processes asked to share pages would get private copies.
    #[test]
    fn a_memory_object_is_refused_rather_than_mapped_anonymously() {
        let mut address = 0_u64;
        // SAFETY: `address` is a live word; nothing is mapped.
        let status = unsafe {
            mach_vm_map(
                TASK_SELF,
                &mut address,
                page_size(),
                0,
                VM_FLAGS_ANYWHERE,
                0x1234,
                0,
                0,
                VM_PROT_READ,
                VM_PROT_READ,
                VM_INHERIT_NONE,
            )
        };
        assert_eq!(status, KERN_INVALID_ARGUMENT);
        assert_eq!(address, 0, "a refusal leaves *address alone");
    }

    /// **The signature's fifth argument, which is the witness that the order is
    /// now the real one.** Darwin's `vm_map` takes a `boolean_t anywhere` there
    /// and nothing else; under the order this function used to carry, `r8` was
    /// `offset` and `r9` was `current_protection`, so *both* of these calls
    /// would have had their hint ignored and been answered with an anonymous
    /// mapping wherever the host liked. One now succeeds at a page-aligned
    /// address the caller named and the other is refused for asking for an
    /// unaligned one, which is `vm_map_locate_space_fixed`'s
    /// `VM_MAP_PAGE_ALIGNED` check and cannot be true of a word nobody read.
    #[test]
    fn the_fifth_argument_is_the_anywhere_boolean_and_not_a_flags_word() {
        let page = page_size();
        let want = 0x7004_0000_0000_u64 & !(page - 1);
        let mut fixed = want;
        // SAFETY: `fixed` is a live word and the range is free - the address is
        // outside everything this binary's other fixed-map tests use.
        let status = unsafe {
            vm_map(
                TASK_SELF,
                &mut fixed,
                page,
                0, // offset
                0, // anywhere = FALSE: map exactly here
                MACH_PORT_NULL,
                0, // file_offset
                0, // docow
                VM_PROT_READ | VM_PROT_WRITE,
                VM_PROT_READ | VM_PROT_WRITE,
                VM_INHERIT_COPY,
            )
        };
        assert_eq!(status, KERN_SUCCESS, "a fixed map at {want:#x}");
        assert_eq!(fixed, want, "anywhere = FALSE must be honoured exactly");
        // SAFETY: the mapping the call above made.
        unsafe { assert_eq!(libc::munmap(fixed as *mut c_void, page as usize), 0) };

        // The same request with `anywhere` set: the address is a hint now, so
        // whatever comes back is the host's answer and only has to be a real
        // page-granular mapping. Nothing here depends on which address the
        // allocator picks, because the *fixed* call above is the witness: this
        // function used to pass 0 to `mmap` whatever the fifth argument said,
        // so it could not have returned `want` exactly.
        let mut anywhere_else = want;
        // SAFETY: as above.
        let status = unsafe {
            vm_map(
                TASK_SELF,
                &mut anywhere_else,
                page,
                0,
                1, // anywhere = TRUE
                MACH_PORT_NULL,
                0,
                0,
                VM_PROT_READ | VM_PROT_WRITE,
                VM_PROT_READ | VM_PROT_WRITE,
                VM_INHERIT_COPY,
            )
        };
        assert_eq!(status, KERN_SUCCESS);
        assert_ne!(anywhere_else, 0);
        assert_eq!(anywhere_else & (page - 1), 0, "a mapping is page-granular");
        // SAFETY: the mapping the call above made.
        unsafe { assert_eq!(libc::munmap(anywhere_else as *mut c_void, page as usize), 0) };

        // And a *fixed* request for an unaligned address is refused rather than
        // rounded, which is the case that would have been silently answered
        // anywhere had the fifth argument gone unread.
        let mut unaligned = want + 1;
        // SAFETY: the call refuses before it maps anything.
        assert_eq!(
            unsafe {
                vm_map(
                    TASK_SELF,
                    &mut unaligned,
                    page,
                    0,
                    0,
                    MACH_PORT_NULL,
                    0,
                    0,
                    VM_PROT_READ,
                    VM_PROT_READ,
                    VM_INHERIT_COPY,
                )
            },
            KERN_INVALID_ARGUMENT
        );
    }

    /// **The wiring this file's corrected order exists for.** `vm_map` with a
    /// non-null sixth argument maps the pages the memory entry stands for -
    /// and the test goes through `vm_map` rather than through
    /// [`super::memory_object`] directly, because the thing that was wrong was
    /// this function reading its arguments, not the object behind them.
    ///
    /// Under the old parameter order the handle arrived in `current_protection`
    /// and was refused as an out-of-range protection mask, so no mapping was ever
    /// made and `*address` stayed zero. Here `*address` comes back with a
    /// mapping in it, and two of them are **one** page cache.
    #[test]
    fn vm_map_maps_the_pages_of_a_memory_entry_rather_than_refusing_it() {
        let page = page_size();
        let size = 2 * page;
        let entry = super::super::memory_object::make(super::super::memory_object::Make::Fresh {
            size,
            protection: VM_PROT_READ | VM_PROT_WRITE,
        })
        .expect("a host with a writable namespace mints one");

        let mut first = 0_u64;
        // SAFETY: `first` is a live word and the mapping this makes is this
        // process's.
        let status = unsafe {
            vm_map(
                TASK_SELF,
                &mut first,
                size,
                0, // offset
                1, // anywhere
                entry,
                0, // file_offset
                0, // docow
                VM_PROT_READ | VM_PROT_WRITE,
                VM_PROT_READ | VM_PROT_WRITE,
                VM_INHERIT_NONE,
            )
        };
        assert_eq!(status, KERN_SUCCESS, "a memory entry maps");
        assert_ne!(first, 0, "and *address comes back with a mapping in it");

        let mut second = 0_u64;
        // SAFETY: as above.
        assert_eq!(
            unsafe {
                vm_map(
                    TASK_SELF,
                    &mut second,
                    size,
                    0,
                    1,
                    entry,
                    0,
                    0,
                    VM_PROT_READ | VM_PROT_WRITE,
                    VM_PROT_READ | VM_PROT_WRITE,
                    VM_INHERIT_NONE,
                )
            },
            KERN_SUCCESS
        );
        assert_ne!(second, first);
        // SAFETY: two whole mappings of `size` bytes this test made, and both
        // buffers are inside them.
        unsafe {
            let marker = b"vm_map memory entry";
            std::ptr::copy_nonoverlapping(marker.as_ptr(), first as *mut u8, marker.len());
            let mut seen = [0_u8; 6];
            std::ptr::copy_nonoverlapping((second as *const u8).add(7), seen.as_mut_ptr(), 6);
            assert_eq!(&seen, b"memory", "two mappings of one object");
            assert_eq!(libc::munmap(first as *mut c_void, size as usize), 0);
            assert_eq!(libc::munmap(second as *mut c_void, size as usize), 0);
        }
        super::super::memory_object::forget(first, size);
        super::super::memory_object::forget(second, size);
    }

    /// A name this process holds that is **not** a memory entry is refused by
    /// name, and is *not* answered with an anonymous mapping. That is the one
    /// thing the memory-object branch must never do: a plausible wrong answer
    /// here would be a mapping of fresh private memory handed to a caller that
    /// asked to share pages, and nothing downstream could tell.
    ///
    /// ## The port is drawn here, not written down
    ///
    /// This used to name `0x1003` and say that whether the run had drawn it was
    /// immaterial. **It is not immaterial, and the test said so falsely.** `0x1003`
    /// is `mach::space`'s `FIRST_DYNAMIC` - the *first* name any allocation in
    /// the process gets - so the first memory entry minted anywhere in this test
    /// binary takes it. Run alone with the entry test above it, `vm_map` was
    /// handed a real memory entry and answered `KERN_SUCCESS`, and the whole
    /// suite passed only because some earlier test happened to draw `0x1003`
    /// first. A test whose result depends on the order of unrelated tests is not
    /// a witness, and this one was quietly one.
    ///
    /// So the two cases are built rather than assumed. `MACH_PORT_DEAD` is a name
    /// no allocation in this space can ever return, so "this process holds no
    /// right to it" is a property of the space and not of the run. The second is
    /// a **live send right to an address with no memory-object directory**,
    /// which is the harder case - a name this process legitimately holds, of the
    /// shape a caller really passes, standing for something that is not a memory
    /// entry. Both must be refused; only `KERN_SUCCESS` would be wrong.
    #[test]
    fn vm_map_never_answers_a_memory_object_with_anonymous_memory() {
        let page = page_size();
        let probe = |object: Name, what: &str| {
            let mut address = 0_u64;
            // SAFETY: `address` is a live word; the call refuses or maps, and
            // nothing else is dereferenced either way.
            let status = unsafe {
                vm_map(
                    TASK_SELF,
                    &mut address,
                    page,
                    0,
                    1,
                    object,
                    0,
                    0,
                    VM_PROT_READ,
                    VM_PROT_READ,
                    VM_INHERIT_NONE,
                )
            };
            assert_ne!(status, KERN_SUCCESS, "{what}");
            if status == KERN_SUCCESS {
                // SAFETY: only reachable if the assertion above is wrong, and
                // then it is this process's own mapping.
                unsafe { libc::munmap(address as *mut c_void, page as usize) };
            }
        };
        probe(MACH_PORT_DEAD, "a name no allocation in this space can return");
        // A send right this process holds, for an address no entry was minted
        // at. `fresh_address` is 64 random bits (`getrandom`), so it cannot be an
        // entry another test minted - and if it somehow were, this fails loudly
        // rather than passing by luck.
        let stranger = super::super::space::insert_send(super::super::wire::fresh_address());
        probe(stranger, "a live right that is not a memory entry");
        super::super::space::destroy(stranger);
    }

    /// A fixed `vm_map` over something that is already mapped is
    /// `KERN_NO_SPACE`: `vm_map`'s fifth argument is a boolean, so there is no
    /// `VM_FLAGS_OVERWRITE` to ask for a replacement, and `MAP_FIXED` would
    /// have replaced a live range without being asked to.
    #[test]
    fn a_fixed_vm_map_over_something_mapped_is_no_space() {
        let page = page_size();
        let page_vec = vec![0_u8; page as usize];
        let start = (page_vec.as_ptr() as u64) & !(page - 1);
        let mut address = start;
        // SAFETY: `address` is a live word; the range belongs to `page_vec`.
        let status = unsafe {
            vm_map(
                TASK_SELF,
                &mut address,
                page,
                0,
                0,
                MACH_PORT_NULL,
                0,
                0,
                VM_PROT_READ,
                VM_PROT_READ,
                VM_INHERIT_NONE,
            )
        };
        assert_eq!(status, KERN_NO_SPACE);
        assert_eq!(address, start, "a refusal leaves *address alone");
    }

    /// The two arguments the real signature has and the invented one did not:
    /// `docow`, a copy-on-write clone of a memory object, and
    /// `VM_INHERIT_DONATE_COPY`, which deletes the parent's mapping. Neither is
    /// an `mmap(2)` operation, so both are refused by name rather than
    /// approximated.
    #[test]
    fn docow_and_a_donating_inheritance_are_refused_by_name() {
        let page = page_size();
        let mut address = 0_u64;
        // SAFETY: both calls refuse before they map anything.
        let copying = unsafe {
            vm_map(
                TASK_SELF,
                &mut address,
                page,
                0,
                1,
                MACH_PORT_NULL,
                0,
                1, // docow
                VM_PROT_READ,
                VM_PROT_READ,
                VM_INHERIT_NONE,
            )
        };
        assert_eq!(copying, KERN_INVALID_ARGUMENT);
        let donating = unsafe {
            vm_map(
                TASK_SELF,
                &mut address,
                page,
                0,
                1,
                MACH_PORT_NULL,
                0,
                0,
                VM_PROT_READ,
                VM_PROT_READ,
                3, // VM_INHERIT_DONATE_COPY
            )
        };
        assert_eq!(donating, KERN_INVALID_ARGUMENT);
    }

    /// A fixed request that would land on an existing mapping is refused, not
    /// silently satisfied by replacing it, because `VM_FLAGS_OVERWRITE` was not
    /// among the flags.
    #[test]
    fn a_fixed_map_over_something_mapped_is_no_space() {
        let page = page_size();
        let page_vec = vec![0_u8; page as usize];
        let start = (page_vec.as_ptr() as u64) & !(page - 1);
        let mut address = start;
        // SAFETY: `address` is a live word and the range is this process's.
        let status = unsafe {
            mach_vm_map(
                TASK_SELF,
                &mut address,
                page,
                0,
                VM_FLAGS_FIXED,
                MACH_PORT_NULL,
                0,
                0,
                VM_PROT_READ,
                VM_PROT_READ,
                VM_INHERIT_NONE,
            )
        };
        assert_eq!(status, KERN_NO_SPACE);
        assert_eq!(address, start, "a refusal leaves *address alone");
    }

    /// ... and the same range *with* `VM_FLAGS_OVERWRITE`, which is the other
    /// word the image passes (`mov r8d, 0x4000` at `Chromium Framework
    /// +0x7e5f242`). Mach's answer is a mapping, and the address is still the
    /// one asked for.
    ///
    /// This is the one place where a `mmap(2)` mapping would have been a
    /// **plausible wrong answer**: `MAP_FIXED` replaces whatever was there, so
    /// asking for no `OVERWRITE` and using `MAP_FIXED` anyway would silently
    /// unmap a live range and hand back `KERN_SUCCESS`. The range overwritten
    /// here is a scratch page this test made for the purpose, because a test
    /// that let `MAP_FIXED` eat a `Vec` would fault in `Vec`'s own `Drop`.
    #[test]
    fn the_same_range_with_overwrite_is_mapped_at_the_same_address() {
        let page = page_size();
        let start = 0x7003_0000_0000_u64 & !(page - 1);
        // SAFETY: a scratch anonymous mapping this test owns, so that
        // overwriting it destroys nothing.
        let scratch = unsafe {
            libc::mmap(
                start as *mut c_void,
                page as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
                -1,
                0,
            )
        };
        assert_ne!(scratch, libc::MAP_FAILED);
        let mut address = start;
        // SAFETY: `address` is a live word; the range is this process's, and
        // `VM_FLAGS_OVERWRITE` is what asks for it to be replaced.
        let status = unsafe {
            mach_vm_map(
                TASK_SELF,
                &mut address,
                page,
                0,
                VM_FLAGS_FIXED | VM_FLAGS_OVERWRITE,
                MACH_PORT_NULL,
                0,
                0,
                VM_PROT_READ | VM_PROT_WRITE,
                VM_PROT_READ | VM_PROT_WRITE,
                VM_INHERIT_NONE,
            )
        };
        assert_eq!(status, KERN_SUCCESS);
        assert_eq!(address, start);
        // SAFETY: the mapping above, which is the only thing at `start`.
        unsafe { assert_eq!(libc::munmap(address as *mut c_void, page as usize), 0) };
    }

    /// The remap half of the image's V8-sandbox wrapper: `*target_address` is
    /// seeded with the requested address and compared against itself after the
    /// call, so the address has to come back exactly or the guest traps.
    #[test]
    fn a_remap_is_honoured_at_the_address_asked_for() {
        let page = page_size();
        // Distinct fixed addresses from the map tests above, for the reason
        // given there: two tests mapping the same page would be a race.
        let source = 0x7001_0000_0000_u64 & !(page - 1);
        let target = 0x7002_0000_0000_u64 & !(page - 1);
        let mut address = source;
        // SAFETY: `address` is a live word and the mapping is this process's.
        assert_eq!(
            unsafe {
                mach_vm_map(
                    TASK_SELF,
                    &mut address,
                    page,
                    0,
                    VM_FLAGS_FIXED,
                    MACH_PORT_NULL,
                    0,
                    0,
                    VM_PROT_READ | VM_PROT_WRITE,
                    VM_PROT_READ | VM_PROT_WRITE,
                    VM_INHERIT_NONE,
                )
            },
            KERN_SUCCESS
        );
        assert_eq!(address, source);
        let mut to = target;
        let mut cur = 0_i32;
        let mut max = 0_i32;
        // SAFETY: `to`, `cur` and `max` are live words and the mapping is this
        // process's.
        let status = unsafe {
            mach_vm_remap(
                TASK_SELF,
                &mut to,
                page,
                0,
                VM_FLAGS_OVERWRITE,
                TASK_SELF,
                source,
                0,
                &mut cur,
                &mut max,
                VM_INHERIT_NONE,
            )
        };
        assert_eq!(status, KERN_SUCCESS);
        assert_eq!(
            to, target,
            "a fixed remap that moved is a trap in the image"
        );
        assert_eq!(cur & (VM_PROT_READ | VM_PROT_WRITE), 3);
        // SAFETY: the range the remap left at `target`.
        unsafe { assert_eq!(libc::munmap(target as *mut c_void, page as usize), 0) };
    }

    /// `mach_vm_read` counts `*dataCnt` in bytes, which is `vm_user.c`'s
    /// `*data_size = size` rather than a word count, and it is in/out.
    #[test]
    fn mach_vm_read_copies_and_reports_bytes() {
        let from = [1_u8, 2, 3, 4, 5, 6, 7, 8];
        let mut into = [0_u8; 8];
        let mut data = into.as_mut_ptr() as u64;
        let mut count = 8_u32;
        // SAFETY: two live, non-overlapping buffers and out-parameters the
        // caller owns.
        let status =
            unsafe { mach_vm_read(TASK_SELF, from.as_ptr() as u64, 8, &mut data, &mut count) };
        assert_eq!(status, KERN_SUCCESS);
        assert_eq!(count, 8);
        assert_eq!(into, from);
        // A short buffer is the caller's own mistake and the count says so.
        let mut short = 4_u32;
        let mut data = into.as_mut_ptr() as u64;
        // SAFETY: as above; the call refuses before it copies.
        assert_eq!(
            unsafe { mach_vm_read(TASK_SELF, from.as_ptr() as u64, 8, &mut data, &mut short) },
            KERN_INVALID_ARGUMENT
        );
        assert_eq!(short, 4, "a refusal leaves *dataCnt alone");
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
