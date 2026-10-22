use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::kern::{KERN_FAILURE, KERN_INVALID_ARGUMENT, KERN_SUCCESS};
use super::space::{self, HOST_SELF, Name};
use server::protocol::Usage;

const TASK_ABSOLUTETIME_INFO: i32 = 1;
const TASK_EVENTS_INFO: i32 = 2;
const TASK_THREAD_TIMES_INFO: i32 = 3;
const TASK_BASIC_INFO_64: i32 = 5;
const TASK_AUDIT_TOKEN: i32 = 15;
const MACH_TASK_BASIC_INFO: i32 = 20;
const TASK_POWER_INFO: i32 = 21;
const TASK_VM_INFO: i32 = 22;
const TASK_VM_INFO_PURGEABLE: i32 = 23;

/// `time_value_t`, which is a `timeval` of two `integer_t`.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
pub(super) struct TimeValue {
    seconds: i32,
    microseconds: i32,
}

/// `task_basic_info_64`, the flavour a 64-bit process gets from
/// `TASK_BASIC_INFO`.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct TaskBasicInfo64 {
    suspend_count: i32,
    virtual_size: u64,
    resident_size: u64,
    user_time: TimeValue,
    system_time: TimeValue,
    policy: i32,
}

/// `mach_task_basic_info`, which leads with the sizes and adds a peak.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct MachTaskBasicInfo {
    virtual_size: u64,
    resident_size: u64,
    resident_size_max: u64,
    user_time: TimeValue,
    system_time: TimeValue,
    policy: i32,
    suspend_count: i32,
}

impl TimeValue {
    pub(super) fn from_ns(ns: u64) -> TimeValue {
        TimeValue {
            seconds: (ns / 1_000_000_000) as i32,
            microseconds: ((ns % 1_000_000_000) / 1_000) as i32,
        }
    }
}

/// `task_absolutetime_info`, in `mach_absolute_time` units - nanoseconds
/// here, since the timebase is one to one.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct TaskAbsolutetimeInfo {
    total_user: u64,
    total_system: u64,
    threads_user: u64,
    threads_system: u64,
}

/// `task_events_info`. The Mach counters - messages, Mach traps - are not
/// kept by anything and read zero; the rest are the host's.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct TaskEventsInfo {
    faults: i32,
    pageins: i32,
    cow_faults: i32,
    messages_sent: i32,
    messages_received: i32,
    syscalls_mach: i32,
    syscalls_unix: i32,
    csw: i32,
}

/// `task_power_info`. Wakeups are not counted here and read zero.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct TaskPowerInfo {
    total_user: u64,
    total_system: u64,
    task_interrupt_wakeups: u64,
    task_platform_idle_wakeups: u64,
    task_timer_wakeups_bin_1: u64,
    task_timer_wakeups_bin_2: u64,
}

/// `task_thread_times_info`: the CPU time of the task's *live* threads.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct TaskThreadTimesInfo {
    user_time: TimeValue,
    system_time: TimeValue,
}

/// `task_vm_info`, as far as `phys_footprint`.
///
/// The footprint is the field callers actually want and it is at offset 144,
/// so every member before it has to be present and in the right place even
/// though most of them stay zero. The structure continues past this point in
/// the real header; a caller with room for more gets zeroes, which is the same
/// answer as "this host does not count that".
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct TaskVmInfo {
    virtual_size: u64,
    region_count: i32,
    page_size: i32,
    resident_size: u64,
    resident_size_peak: u64,
    device: u64,
    device_peak: u64,
    internal: u64,
    internal_peak: u64,
    external: u64,
    external_peak: u64,
    reusable: u64,
    reusable_peak: u64,
    purgeable_volatile_pmap: u64,
    purgeable_volatile_resident: u64,
    purgeable_volatile_virtual: u64,
    compressed: u64,
    compressed_peak: u64,
    compressed_lifetime: u64,
    phys_footprint: u64,
}

const _: () = {
    assert!(size_of::<TaskBasicInfo64>() == 40);
    assert!(size_of::<MachTaskBasicInfo>() == 48);
    assert!(size_of::<TaskThreadTimesInfo>() == 16);
    assert!(size_of::<TaskAbsolutetimeInfo>() == 32);
    assert!(size_of::<TaskEventsInfo>() == 32);
    assert!(size_of::<TaskPowerInfo>() == 48);
    assert!(std::mem::offset_of!(TaskVmInfo, phys_footprint) == 144);
};

pub(super) fn page_size() -> u64 {
    (unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).max(4096) as u64
}

/// `vm_size_t vm_page_size`, `vm_size_t vm_page_mask` and `int vm_page_shift` -
/// the three data symbols `<mach/vm_param.h>` publishes beside
/// [`host_page_size`].
///
/// Written once, before the loader binds anything, by [`publish_page_size`].
pub static VM_PAGE_SIZE: AtomicU64 = AtomicU64::new(0);
pub static VM_PAGE_MASK: AtomicU64 = AtomicU64::new(0);
pub static VM_PAGE_SHIFT: AtomicU32 = AtomicU32::new(0);

/// Fill the three globals above. Idempotent.
pub fn publish_page_size() {
    let size = page_size();
    VM_PAGE_SIZE.store(size, Ordering::Relaxed);
    VM_PAGE_MASK.store(size - 1, Ordering::Relaxed);
    VM_PAGE_SHIFT.store(size.trailing_zeros(), Ordering::Relaxed);
}

/// What `task` has used, or `None` for a name that is not a task this
/// process can see.
///
/// This process's CPU time comes from `getrusage`, which is exact where the
/// host's per-process accounting rounds to a clock tick.
fn measure(task: Name, flavor: i32) -> Option<Usage> {
    let pid = space::task_pid(task)?;
    let me = pid == starnix::process::me();
    let mut usage = match me && sizes_only(flavor) {
        true => own_sizes(matches!(flavor, TASK_VM_INFO | TASK_VM_INFO_PURGEABLE))?,
        false => crate::process::usage(pid)?,
    };
    if me {
        let mut rusage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: `getrusage` fills the `rusage` it is given and nothing else.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, rusage.as_mut_ptr()) } == 0 {
            // SAFETY: the call above succeeded, so it is initialised.
            let rusage = unsafe { rusage.assume_init() };
            let ns = |t: libc::timeval| t.tv_sec as u64 * 1_000_000_000 + t.tv_usec as u64 * 1_000;
            usage.user_ns = ns(rusage.ru_utime);
            usage.system_ns = ns(rusage.ru_stime);
        }
    }
    Some(usage)
}

/// Whether a flavour reports nothing but sizes and CPU times.
///
/// Engines poll these every few frames for their memory budget, so for this
/// process they skip the rest of [`Usage`]: on Linux its thread counts read
/// every thread's `stat`, about 3 ms in a process with a hundred threads.
fn sizes_only(flavor: i32) -> bool {
    matches!(
        flavor,
        TASK_BASIC_INFO_64 | MACH_TASK_BASIC_INFO | TASK_VM_INFO | TASK_VM_INFO_PURGEABLE
    )
}

/// This process's sizes, and its region count if asked for. Only the fields
/// the flavours of [`sizes_only`] read are filled; the times come after.
fn own_sizes(regions: bool) -> Option<Usage> {
    let footprint = starnix::process::footprint(starnix::process::me())?;
    Some(Usage {
        virtual_size: footprint.virtual_size,
        resident_size: footprint.resident_size,
        resident_peak: footprint.resident_peak,
        shared_size: footprint.shared_size,
        regions: if regions { region_count() } else { 0 },
        ..Usage::default()
    })
}

/// How many mappings this process has
fn region_count() -> u32 {
    let count = starnix::process::regions().map_or(0, |regions| regions.len() as u32);
    count
}

/// Resident memory the task does not share with another, which is the
/// closest thing here to Mach's `phys_footprint`.
///
/// macOS charges a process for its private pages and not for shared or
/// file-backed ones, and an engine budgets against that figure: reporting the
/// whole resident size instead tells it that it is out of memory.
fn private_memory(usage: &Usage) -> u64 {
    usage.resident_size.saturating_sub(usage.shared_size)
}

/// Copy `value` into the caller's buffer and say how many words it holds.
///
/// The count is in 32-bit words both ways, and a buffer too small for the
/// flavour is `KERN_INVALID_ARGUMENT` rather than a partial structure: the
/// caller would read the missing fields as measurements.
///
/// # Safety
///
/// `out` is writable for `*count` words, and `count` is writable.
pub(super) unsafe fn publish<T: Copy>(value: T, out: *mut c_void, count: *mut u32) -> i32 {
    // SAFETY: the caller states `count` is readable.
    let available = unsafe { *count } as usize * size_of::<u32>();
    if available < size_of::<T>() {
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller's buffer has room, and is not assumed to be aligned
    // for `T` - Mach's own structures are packed to four bytes.
    unsafe {
        out.cast::<T>().write_unaligned(value);
        *count = (size_of::<T>() / size_of::<u32>()) as u32;
    }
    KERN_SUCCESS
}

/// `audit_token_t`, eight words a task's messages and `csops_audittoken`
/// carry to say whose they are.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct AuditToken {
    pub val: [u32; 8],
}

/// A process's audit token, filled as XNU's `set_security_token_task_internal`
/// fills it: audit user, effective user and group, real user and group, pid,
/// audit session and pid version. The audit user is the login's, which is the
/// real user; the session is the process's; a pid here is never reused within
/// a run, so its version is zero.
pub fn audit_token(pid: i32) -> AuditToken {
    let credentials = crate::credentials::current();
    // SAFETY: asks about the calling process.
    let session = unsafe { libc::getsid(0) }.max(0) as u32;
    AuditToken {
        val: [
            credentials.ruid,
            credentials.euid,
            credentials.egid,
            credentials.ruid,
            credentials.rgid,
            pid as u32,
            session,
            0,
        ],
    }
}

/// `kern_return_t task_info(task_name_t, task_flavor_t, task_info_t,
/// mach_msg_type_number_t *)`
///
/// This task, or another guest's through a port from `task_for_pid`; the
/// measurements are the host's, by way of the process server.
///
/// # Safety
///
/// `out` is writable for `*count` 32-bit words, and `count` is writable.
pub unsafe extern "C" fn task_info(
    task: Name,
    flavor: i32,
    out: *mut c_void,
    count: *mut u32,
) -> i32 {
    if out.is_null() || count.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    // A name that is not a task, or a task that has gone: answering about the
    // wrong task with a straight face is the failure this crate refuses
    // everywhere else.
    let Some(usage) = measure(task, flavor) else {
        return KERN_INVALID_ARGUMENT;
    };
    if flavor == TASK_AUDIT_TOKEN {
        let Some(pid) = space::task_pid(task) else {
            return KERN_INVALID_ARGUMENT;
        };
        // SAFETY: the caller's obligation, checked against the flavour's size.
        return unsafe { publish(audit_token(pid), out, count) };
    }
    let user_time = TimeValue::from_ns(usage.user_ns);
    let system_time = TimeValue::from_ns(usage.system_ns);
    let (virtual_size, resident_size) = (usage.virtual_size, usage.resident_size);
    match flavor {
        // SAFETY: the caller's obligation, checked against the flavour's size.
        TASK_BASIC_INFO_64 => unsafe {
            publish(
                TaskBasicInfo64 {
                    virtual_size,
                    resident_size,
                    user_time,
                    system_time,
                    ..TaskBasicInfo64::default()
                },
                out,
                count,
            )
        },
        MACH_TASK_BASIC_INFO => unsafe {
            publish(
                MachTaskBasicInfo {
                    virtual_size,
                    resident_size,
                    resident_size_max: usage.resident_peak.max(resident_size),
                    user_time,
                    system_time,
                    ..MachTaskBasicInfo::default()
                },
                out,
                count,
            )
        },
        TASK_THREAD_TIMES_INFO => unsafe {
            publish(
                TaskThreadTimesInfo {
                    user_time,
                    system_time,
                },
                out,
                count,
            )
        },
        TASK_ABSOLUTETIME_INFO => unsafe {
            publish(
                TaskAbsolutetimeInfo {
                    total_user: usage.user_ns,
                    total_system: usage.system_ns,
                    threads_user: usage.user_ns,
                    threads_system: usage.system_ns,
                },
                out,
                count,
            )
        },
        TASK_EVENTS_INFO => unsafe {
            let clamp = |n: u64| n.min(i32::MAX as u64) as i32;
            publish(
                TaskEventsInfo {
                    faults: clamp(usage.minor_faults + usage.major_faults),
                    pageins: clamp(usage.major_faults),
                    csw: clamp(usage.voluntary_switches + usage.involuntary_switches),
                    ..TaskEventsInfo::default()
                },
                out,
                count,
            )
        },
        TASK_POWER_INFO => unsafe {
            publish(
                TaskPowerInfo {
                    total_user: usage.user_ns,
                    total_system: usage.system_ns,
                    ..TaskPowerInfo::default()
                },
                out,
                count,
            )
        },
        TASK_VM_INFO | TASK_VM_INFO_PURGEABLE => {
            let available = unsafe { *count } as usize * size_of::<u32>();
            if available < size_of::<TaskVmInfo>() {
                return KERN_INVALID_ARGUMENT;
            }
            // Anything past this structure is zeroed rather than left as the
            // caller found it. A buffer the caller never cleared is the
            // `machdep.cpu.brand_string` trap: the guest reads its own stack
            // back as a measurement and the failure surfaces elsewhere.
            // SAFETY: the caller states the buffer holds `available` bytes.
            unsafe { std::ptr::write_bytes(out.cast::<u8>(), 0, available) };
            let info = TaskVmInfo {
                virtual_size,
                region_count: usage.regions as i32,
                page_size: page_size() as i32,
                resident_size,
                resident_size_peak: usage.resident_peak.max(resident_size),
                phys_footprint: private_memory(&usage),
                ..TaskVmInfo::default()
            };
            // SAFETY: as above, and the buffer is at least this long.
            unsafe { out.cast::<TaskVmInfo>().write_unaligned(info) };
            KERN_SUCCESS
        }
        _ => KERN_INVALID_ARGUMENT,
    }
}

/// `kern_return_t host_page_size(host_t, vm_size_t *)`
///
/// # Safety
///
/// `out` is a writable `vm_size_t`.
pub unsafe extern "C" fn host_page_size(host: Name, out: *mut u64) -> i32 {
    if out.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    if host != HOST_SELF {
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller's `vm_size_t`.
    unsafe { out.write(page_size()) };
    KERN_SUCCESS
}

const HOST_VM_INFO: i32 = 2;
const HOST_CPU_LOAD_INFO: i32 = 3;
const HOST_VM_INFO64: i32 = 4;

/// `vm_statistics_data_t`, the 32-bit counters `HOST_VM_INFO` answers with.
///
/// Every count is in *pages*, which is why the caller asked for the page size
/// first.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct VmStatistics {
    free_count: u32,
    active_count: u32,
    inactive_count: u32,
    wire_count: u32,
    zero_fill_count: u32,
    reactivations: u32,
    pageins: u32,
    pageouts: u32,
    faults: u32,
    cow_faults: u32,
    lookups: u32,
    hits: u32,
    purgeable_count: u32,
    purges: u32,
    speculative_count: u32,
}

/// `vm_statistics64_data_t`, the same idea with 64-bit event counters and four
/// more page counts on the end.
///
/// The offsets matter and are asserted below: a caller works out how much
/// memory is available as `external + purgeable + (free - speculative)`, which
/// reads four separate fields, three of them past the point where this
/// structure stops looking like its 32-bit ancestor.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct VmStatistics64 {
    free_count: u32,
    active_count: u32,
    inactive_count: u32,
    wire_count: u32,
    zero_fill_count: u64,
    reactivations: u64,
    pageins: u64,
    pageouts: u64,
    faults: u64,
    cow_faults: u64,
    lookups: u64,
    hits: u64,
    purges: u64,
    purgeable_count: u32,
    speculative_count: u32,
    decompressions: u64,
    compressions: u64,
    swapins: u64,
    swapouts: u64,
    compressor_page_count: u32,
    throttled_count: u32,
    external_page_count: u32,
    internal_page_count: u32,
    total_uncompressed_pages_in_compressor: u64,
}

/// `host_cpu_load_info_data_t`: ticks in each of the four CPU states.
///
/// Darwin orders them `USER`, `SYSTEM`, `IDLE`, `NICE`; Linux's `/proc/stat`
/// reports user, **nice**, system, idle. The fields are named so the order is
/// never copied across by position.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct HostCpuLoadInfo {
    user: u32,
    system: u32,
    idle: u32,
    nice: u32,
}

const _: () = {
    assert!(size_of::<VmStatistics>() == 60);
    assert!(std::mem::offset_of!(VmStatistics64, purgeable_count) == 88);
    assert!(std::mem::offset_of!(VmStatistics64, speculative_count) == 92);
    assert!(std::mem::offset_of!(VmStatistics64, external_page_count) == 136);
    assert!(size_of::<VmStatistics64>() == 152);
};

/// The machine's memory, as the page counts Mach reports.
///
/// What this host does not keep a counter for stays zero rather than being
/// approximated from a neighbour: a caller reading `speculative_count` wants
/// pages the kernel read ahead and has not charged to anyone, and no number
/// here means that. The four that *are* answered are the four a caller needs
/// to work out how much memory is available.
fn vm_pages() -> VmStatistics64 {
    let memory = starnix::system::memory();
    // The host counts bytes and Mach counts pages, so a machine whose page
    // size is not 4 KiB converts rather than assuming.
    let pages = |bytes: u64| (bytes / page_size().max(1)) as u32;
    VmStatistics64 {
        free_count: pages(memory.free),
        active_count: pages(memory.active),
        inactive_count: pages(memory.inactive),
        wire_count: pages(memory.wired),
        // File-backed pages, which are the ones macOS calls external and a
        // caller counts as available because they can be evicted.
        external_page_count: pages(memory.file_backed),
        internal_page_count: pages(memory.anonymous),
        ..VmStatistics64::default()
    }
}

/// The machine's CPU ticks, in Darwin's order.
fn cpu_ticks() -> HostCpuLoadInfo {
    let ticks = starnix::system::cpu_ticks();
    // `natural_t` counters, which wrap on a Mac too.
    HostCpuLoadInfo {
        user: ticks.user as u32,
        system: ticks.system as u32,
        idle: ticks.idle as u32,
        nice: ticks.nice as u32,
    }
}

/// `kern_return_t host_statistics(host_t, host_flavor_t, host_info_t,
/// mach_msg_type_number_t *)`
///
/// # Safety
///
/// As [`task_info`]: `out` is writable for `*count` words.
pub unsafe extern "C" fn host_statistics(
    host: Name,
    flavor: i32,
    out: *mut c_void,
    count: *mut u32,
) -> i32 {
    if out.is_null() || count.is_null() || host != HOST_SELF {
        return KERN_INVALID_ARGUMENT;
    }
    match flavor {
        HOST_VM_INFO => {
            let wide = vm_pages();
            // The 32-bit flavour is the same counts in narrower fields, so it
            // is the wide answer with the event counters this host does not
            // keep left where they are: zero.
            let value = VmStatistics {
                free_count: wide.free_count,
                active_count: wide.active_count,
                inactive_count: wide.inactive_count,
                wire_count: wide.wire_count,
                ..VmStatistics::default()
            };
            // SAFETY: the caller's obligation, checked against the size.
            unsafe { publish(value, out, count) }
        }
        HOST_CPU_LOAD_INFO => unsafe { publish(cpu_ticks(), out, count) },
        _ => KERN_INVALID_ARGUMENT,
    }
}

/// `kern_return_t host_statistics64(host_t, host_flavor_t, host_info64_t,
/// mach_msg_type_number_t *)`
///
/// # Safety
///
/// As [`host_statistics`].
pub unsafe extern "C" fn host_statistics64(
    host: Name,
    flavor: i32,
    out: *mut c_void,
    count: *mut u32,
) -> i32 {
    if out.is_null() || count.is_null() || host != HOST_SELF {
        return KERN_INVALID_ARGUMENT;
    }
    match flavor {
        // SAFETY: the caller's obligation, checked against the size.
        HOST_VM_INFO64 => unsafe { publish(vm_pages(), out, count) },
        // The narrow flavours are not answered through the wide call: the
        // caller sized its buffer for the wide structure and would read the
        // fields at the other set of offsets.
        _ => KERN_INVALID_ARGUMENT,
    }
}

const HOST_BASIC_INFO: i32 = 1;
const HOST_SCHED_INFO: i32 = 3;
const HOST_PRIORITY_INFO: i32 = 5;
const HOST_PREFERRED_USER_ARCH: i32 = 12;

/// How many words of [`HostBasicInfo`] predate the CPU counts and `max_mem`.
///
/// Callers built against old headers pass this count, and the kernel still
/// answers them with the prefix they know about.
const HOST_BASIC_INFO_OLD_COUNT: u32 = 5;

/// `host_basic_info_data_t`.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct HostBasicInfo {
    max_cpus: i32,
    avail_cpus: i32,
    memory_size: u32,
    cpu_type: i32,
    cpu_subtype: i32,
    cpu_threadtype: i32,
    physical_cpu: i32,
    physical_cpu_max: i32,
    logical_cpu: i32,
    logical_cpu_max: i32,
    max_mem: u64,
}

/// `host_sched_info_data_t`, both in milliseconds.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct HostSchedInfo {
    min_timeout: i32,
    min_quantum: i32,
}

/// `host_priority_info_data_t`.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct HostPriorityInfo {
    kernel_priority: i32,
    system_priority: i32,
    server_priority: i32,
    user_priority: i32,
    depress_priority: i32,
    idle_priority: i32,
    minimum_priority: i32,
    maximum_priority: i32,
}

/// `host_preferred_user_arch_data_t`.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct HostPreferredUserArch {
    cpu_type: i32,
    cpu_subtype: i32,
}

const _: () = {
    assert!(size_of::<HostBasicInfo>() == 48);
    assert!(std::mem::offset_of!(HostBasicInfo, max_mem) == 40);
    assert!(size_of::<HostPriorityInfo>() == 32);
};

fn host_basic_info() -> HostBasicInfo {
    let arch = crate::sysctl::guest_arch();
    let logical = crate::sysctl::ncpu() as i32;
    // SAFETY: `sysconf` reads a count and touches no memory of ours.
    let configured = (unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) } as i32).max(logical);
    // A host that cannot tell cores from SMT threads answers what a machine
    // without SMT would.
    let physical =
        starnix::system::physical_cores().map_or(logical, |cores| (cores as i32).min(logical));
    let max_mem = crate::sysctl::memsize();
    HostBasicInfo {
        max_cpus: configured,
        avail_cpus: logical,
        // `natural_t`, which XNU clamps at 2 GiB; `max_mem` is the real figure.
        memory_size: max_mem.min(0x8000_0000) as u32,
        cpu_type: arch.cpu_type(),
        cpu_subtype: arch.cpu_subtype(),
        // `CPU_THREADTYPE_INTEL_HTT` when an Intel part has more threads than
        // cores; XNU reports no thread type for anything else.
        cpu_threadtype: i32::from(arch == crate::sysctl::GuestArch::X86_64 && logical > physical),
        physical_cpu: physical,
        physical_cpu_max: physical,
        logical_cpu: logical,
        logical_cpu_max: configured,
        max_mem,
    }
}

/// `kern_return_t host_info(host_t, host_flavor_t, host_info_t,
/// mach_msg_type_number_t *)`
///
/// # Safety
///
/// As [`task_info`]: `out` is writable for `*count` words.
pub unsafe extern "C" fn host_info(
    host: Name,
    flavor: i32,
    out: *mut c_void,
    count: *mut u32,
) -> i32 {
    if out.is_null() || count.is_null() || host != HOST_SELF {
        return KERN_INVALID_ARGUMENT;
    }
    match flavor {
        HOST_BASIC_INFO => {
            // SAFETY: the caller states `count` is readable.
            let available = unsafe { *count };
            if available < HOST_BASIC_INFO_OLD_COUNT {
                return KERN_FAILURE;
            }
            let info = host_basic_info();
            if available as usize * size_of::<u32>() >= size_of::<HostBasicInfo>() {
                // SAFETY: the caller's obligation, checked against the size.
                return unsafe { publish(info, out, count) };
            }
            // The old layout is a prefix of the new one.
            // SAFETY: the caller's buffer holds at least the old count of
            // words, which is all that is written.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (&raw const info).cast::<u8>(),
                    out.cast::<u8>(),
                    HOST_BASIC_INFO_OLD_COUNT as usize * size_of::<u32>(),
                );
                *count = HOST_BASIC_INFO_OLD_COUNT;
            }
            KERN_SUCCESS
        }
        // XNU answers both with its standard quantum, 10 ms.
        HOST_SCHED_INFO => unsafe {
            publish(
                HostSchedInfo {
                    min_timeout: 10,
                    min_quantum: 10,
                },
                out,
                count,
            )
        },
        // XNU's band boundaries from `<kern/sched.h>`. Darwin priorities are
        // not this host's nice values, so these are the constants a caller
        // compares its own `thread_policy` numbers against.
        HOST_PRIORITY_INFO => unsafe {
            publish(
                HostPriorityInfo {
                    kernel_priority: 80,
                    system_priority: 80,
                    server_priority: 64,
                    user_priority: 31,
                    depress_priority: 0,
                    idle_priority: 0,
                    minimum_priority: 0,
                    maximum_priority: 79,
                },
                out,
                count,
            )
        },
        HOST_PREFERRED_USER_ARCH => {
            let arch = crate::sysctl::guest_arch();
            // SAFETY: the caller's obligation, checked against the size.
            unsafe {
                publish(
                    HostPreferredUserArch {
                        cpu_type: arch.cpu_type(),
                        cpu_subtype: arch.cpu_subtype(),
                    },
                    out,
                    count,
                )
            }
        }
        _ => KERN_INVALID_ARGUMENT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mach::space::TASK_SELF;

    /// Read `n` bytes at `offset` out of a buffer `task_info` filled.
    fn word(buffer: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(buffer[offset..offset + 8].try_into().expect("eight bytes"))
    }

    #[test]
    fn the_basic_flavours_report_this_processs_own_memory() {
        let mut info = [0u8; size_of::<MachTaskBasicInfo>()];
        let mut count = (info.len() / 4) as u32;
        // SAFETY: a buffer this test sized, and a live count.
        let rc = unsafe {
            task_info(
                TASK_SELF,
                MACH_TASK_BASIC_INFO,
                info.as_mut_ptr().cast(),
                &raw mut count,
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
        assert_eq!(count, 12);
        let resident = word(&info, 8);
        assert!(resident > 0, "this process is resident in memory");
        assert!(word(&info, 0) >= resident, "virtual covers resident");
        assert!(word(&info, 16) >= resident, "the peak covers the present");
    }

    #[test]
    fn the_vm_flavour_answers_the_footprint_at_the_offset_callers_read() {
        let mut info = [0u8; 512];
        let mut count = (info.len() / 4) as u32;
        let rc = unsafe {
            task_info(
                TASK_SELF,
                TASK_VM_INFO,
                info.as_mut_ptr().cast(),
                &raw mut count,
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
        let footprint = word(&info, 144);
        assert!(footprint > 0, "some memory is this process's own");
        assert!(
            footprint <= word(&info, 16),
            "and no more than the resident"
        );
        assert_eq!(
            u32::from_le_bytes(info[12..16].try_into().expect("four bytes")) as u64,
            page_size()
        );
    }

    #[test]
    fn the_page_size_is_a_page_size() {
        let mut size = 0u64;
        // SAFETY: a live `vm_size_t`.
        assert_eq!(
            unsafe { host_page_size(HOST_SELF, &raw mut size) },
            KERN_SUCCESS
        );
        assert!(size >= 4096 && size.is_power_of_two());
        // SAFETY: a null out-parameter is what this checks.
        assert_eq!(
            unsafe { host_page_size(HOST_SELF, std::ptr::null_mut()) },
            KERN_INVALID_ARGUMENT
        );
    }

    /// The four fields a caller works "available memory" out of, read at the
    /// offsets it reads them at rather than through this file's own structure.
    #[test]
    fn the_wide_vm_statistics_answer_what_available_memory_is_computed_from() {
        let mut info = [0u8; size_of::<VmStatistics64>()];
        let mut count = (info.len() / 4) as u32;
        // SAFETY: a buffer this test sized, and a live count.
        let rc = unsafe {
            host_statistics64(
                HOST_SELF,
                HOST_VM_INFO64,
                info.as_mut_ptr().cast(),
                &raw mut count,
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
        assert_eq!(count, 38);
        let at = |offset: usize| {
            u32::from_le_bytes(info[offset..offset + 4].try_into().expect("four bytes")) as u64
        };
        let (free, purgeable, speculative, external) = (at(0), at(88), at(92), at(136));
        assert!(free > 0, "the machine has some free memory");
        assert!(external > 0, "and some file-backed memory");
        let available = (external + purgeable + (free - speculative)) * page_size();
        assert!(available > 0);
    }

    /// The narrow flavour is a different structure, and the CPU ticks are in
    /// Darwin's order and not this host's.
    #[test]
    fn the_narrow_flavours_are_their_own_structures() {
        let mut info = [0u8; 128];
        let mut count = (size_of::<VmStatistics>() / 4) as u32;
        // SAFETY: a buffer larger than either flavour, and a live count.
        unsafe {
            assert_eq!(
                host_statistics(
                    HOST_SELF,
                    HOST_VM_INFO,
                    info.as_mut_ptr().cast(),
                    &raw mut count
                ),
                KERN_SUCCESS
            );
            assert_eq!(count, 15);

            let mut count = 4;
            assert_eq!(
                host_statistics(
                    HOST_SELF,
                    HOST_CPU_LOAD_INFO,
                    info.as_mut_ptr().cast(),
                    &raw mut count
                ),
                KERN_SUCCESS
            );
        }
        let ticks: Vec<u32> = (0..4)
            .map(|i| u32::from_le_bytes(info[i * 4..i * 4 + 4].try_into().expect("four bytes")))
            .collect();
        assert!(ticks[0] > 0, "user time has passed");
        assert!(
            ticks[2] > ticks[3],
            "idle is the third field and nice the fourth, which is not this host's order"
        );
        // And the wide flavour is not reachable through the narrow call, or a
        // caller would read the wide fields at the narrow offsets.
        let mut count = (info.len() / 4) as u32;
        assert_eq!(
            unsafe {
                host_statistics(
                    HOST_SELF,
                    HOST_VM_INFO64,
                    info.as_mut_ptr().cast(),
                    &raw mut count,
                )
            },
            KERN_INVALID_ARGUMENT
        );
    }

    /// A flavour nothing here measures, a buffer too small for the one asked
    /// for, and another task are all refused rather than answered with zeroes.
    #[test]
    fn what_it_cannot_answer_is_refused() {
        let info = [0u8; 512];
        let mut count = (info.len() / 4) as u32;
        let ask = |flavor, task, count: &mut u32| {
            let mut info = info;
            // SAFETY: a buffer this test sized, and a live count.
            unsafe { task_info(task, flavor, info.as_mut_ptr().cast(), count) }
        };
        assert_eq!(ask(999, TASK_SELF, &mut count), KERN_INVALID_ARGUMENT);
        assert_eq!(
            ask(TASK_VM_INFO, TASK_SELF + 1, &mut count),
            KERN_INVALID_ARGUMENT,
            "another task is not this one"
        );
        let mut small = 4;
        assert_eq!(
            ask(TASK_VM_INFO, TASK_SELF, &mut small),
            KERN_INVALID_ARGUMENT
        );
        // SAFETY: a null buffer is what this checks.
        assert_eq!(
            unsafe {
                task_info(
                    TASK_SELF,
                    TASK_VM_INFO,
                    std::ptr::null_mut(),
                    &raw mut count,
                )
            },
            KERN_INVALID_ARGUMENT
        );
    }

    #[test]
    fn host_basic_info_describes_the_guests_machine() {
        let mut info = [0u8; size_of::<HostBasicInfo>()];
        let mut count = (info.len() / 4) as u32;
        // SAFETY: a buffer this test sized, and a live count.
        let rc = unsafe {
            host_info(
                HOST_SELF,
                HOST_BASIC_INFO,
                info.as_mut_ptr().cast(),
                &raw mut count,
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
        assert_eq!(count, 12);
        let at = |offset: usize| {
            i32::from_le_bytes(info[offset..offset + 4].try_into().expect("four bytes"))
        };
        assert!(at(4) >= 1, "at least one CPU is available");
        assert!(at(0) >= at(4), "max covers available");
        assert_eq!(at(12), crate::sysctl::guest_arch().cpu_type());
        assert!(
            at(24) >= 1 && at(24) <= at(32),
            "cores do not outnumber threads"
        );
        assert_eq!(word(&info, 40), crate::sysctl::memsize());
    }

    #[test]
    fn host_basic_info_answers_an_old_count_with_the_old_prefix() {
        let mut info = [0xAAu8; size_of::<HostBasicInfo>()];
        let mut count = HOST_BASIC_INFO_OLD_COUNT;
        // SAFETY: the buffer is larger than the count claims.
        let rc = unsafe {
            host_info(
                HOST_SELF,
                HOST_BASIC_INFO,
                info.as_mut_ptr().cast(),
                &raw mut count,
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
        assert_eq!(count, HOST_BASIC_INFO_OLD_COUNT);
        assert!(
            info[20..].iter().all(|b| *b == 0xAA),
            "nothing past the old layout"
        );

        let mut count = HOST_BASIC_INFO_OLD_COUNT - 1;
        // SAFETY: as above.
        let rc = unsafe {
            host_info(
                HOST_SELF,
                HOST_BASIC_INFO,
                info.as_mut_ptr().cast(),
                &raw mut count,
            )
        };
        assert_eq!(rc, KERN_FAILURE);
    }

    #[test]
    fn host_info_refuses_what_it_does_not_answer() {
        let mut info = [0u8; 64];
        let mut count = (info.len() / 4) as u32;
        // SAFETY: a buffer this test sized, and a live count.
        let ask = |host, flavor, count: &mut u32, info: &mut [u8; 64]| unsafe {
            host_info(host, flavor, info.as_mut_ptr().cast(), count)
        };
        assert_eq!(
            ask(HOST_SELF, 999, &mut count, &mut info),
            KERN_INVALID_ARGUMENT
        );
        assert_eq!(
            ask(TASK_SELF, HOST_BASIC_INFO, &mut count, &mut info),
            KERN_INVALID_ARGUMENT,
            "a task port is not the host"
        );
        let mut small = 1;
        assert_eq!(
            ask(HOST_SELF, HOST_PRIORITY_INFO, &mut small, &mut info),
            KERN_INVALID_ARGUMENT
        );
        assert_eq!(
            ask(HOST_SELF, HOST_PREFERRED_USER_ARCH, &mut count, &mut info),
            KERN_SUCCESS
        );
        assert_eq!(count, 2);
    }
}
