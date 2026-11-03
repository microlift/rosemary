//! The machine as a whole.

use crate::sys;

/// Physical memory by what it is doing, in bytes.
///
/// A host that does not keep a counter leaves it zero rather than
/// approximating it from a neighbour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Memory {
    pub free: u64,
    pub active: u64,
    pub inactive: u64,
    /// Memory the kernel may not page out.
    pub wired: u64,
    /// File-backed pages, which can be dropped and read back.
    pub file_backed: u64,
    /// Anonymous pages, which can only be swapped.
    pub anonymous: u64,
}

/// Ticks every CPU has spent in each state since boot, summed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CpuTicks {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
}

/// Swap space, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Swap {
    pub total: u64,
    pub free: u64,
}

pub fn page_size() -> u64 {
    // SAFETY: `sysconf` only reads its argument.
    (unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).max(4096) as u64
}

pub fn memory() -> Memory {
    sys::memory()
}

pub fn cpu_ticks() -> CpuTicks {
    sys::cpu_ticks()
}

/// Physical cores, where the host can tell them from SMT threads.
pub fn physical_cores() -> Option<u32> {
    sys::physical_cores()
}

/// The CPU's marketing name.
pub fn cpu_brand() -> Option<String> {
    sys::cpu_brand()
}

/// Processor packages - sockets - however many cores each carries.
pub fn packages() -> Option<u32> {
    sys::packages()
}

/// The CPU's nominal clock in hertz, or its highest where the host does not
/// say which is nominal.
pub fn cpu_frequency() -> Option<u64> {
    sys::cpu_frequency()
}

/// When the machine booted, on the wall clock.
pub fn boot_time() -> Option<std::time::SystemTime> {
    sys::booted()
}

pub fn swap() -> Option<Swap> {
    sys::swap()
}

/// The most processes the system runs at once.
pub fn max_processes() -> Option<u32> {
    sys::max_processes()
}

/// A UUID that names this machine and stays the same across boots.
pub fn host_uuid() -> Option<[u8; 16]> {
    sys::host_uuid()
}

/// Sixteen bytes from 32 hex digits, with or without a UUID's dashes.
#[cfg_attr(not(any(target_os = "linux", target_os = "freebsd")), allow(dead_code))]
pub(crate) fn parse_uuid(text: &str) -> Option<[u8; 16]> {
    let mut digits = text.trim().bytes().filter(|&b| b != b'-');
    let mut out = [0u8; 16];
    for byte in &mut out {
        let high = (digits.next()? as char).to_digit(16)?;
        let low = (digits.next()? as char).to_digit(16)?;
        *byte = (high << 4 | low) as u8;
    }
    digits.next().is_none().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_machine_has_free_memory_and_has_been_busy() {
        assert!(memory().free > 0);
        let ticks = cpu_ticks();
        assert!(ticks.user + ticks.system + ticks.idle > 0);
    }

    #[test]
    fn a_uuid_parses_with_or_without_dashes() {
        let plain = parse_uuid("0123456789abcdef0123456789ABCDEF\n");
        let dashed = parse_uuid("01234567-89ab-cdef-0123-456789abcdef");
        assert_eq!(plain, dashed);
        assert_eq!(plain.map(|bytes| bytes[1]), Some(0x23));
        assert_eq!(parse_uuid("0123"), None);
        assert_eq!(parse_uuid("0123456789abcdef0123456789abcdef00"), None);
        assert_eq!(parse_uuid("g123456789abcdef0123456789abcdef"), None);
    }

    #[test]
    fn there_is_at_least_one_core() {
        assert!(physical_cores().is_none_or(|cores| cores >= 1));
        assert!(packages().is_none_or(|packages| packages >= 1));
    }

    #[test]
    fn the_machine_booted_in_the_past() {
        let now = std::time::SystemTime::now();
        assert!(boot_time().is_none_or(|booted| booted < now));
    }

    #[test]
    fn free_swap_fits_in_the_total() {
        assert!(swap().is_none_or(|swap| swap.free <= swap.total));
    }
}
