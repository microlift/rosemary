//! `tlv_get_addr`, the function every rewritten descriptor's thunk points at.
//!
//! dyld's, on both architectures: read the block for the descriptor's key off the
//! thread pointer and add the offset, or fall into the slow path when this thread has
//! no block yet. dyld preserves every register but the result across the whole call,
//! and the compiler relies on it, so the slow paths sit behind register-preserving
//! wrappers.

use std::sync::OnceLock;

use crate::exec::Engine;
use crate::program::ImportSite;

/// The thunk descriptors of an image run under `engine` point at.
pub(super) fn entry(engine: Engine) -> u64 {
    match engine {
        Engine::Emulated => guest::entry(),
        Engine::Native => native_entry(),
    }
}

/// The host's own fast path, where a native guest's thread pointer reaches the keys.
fn native_entry() -> u64 {
    #[cfg(any(
        target_arch = "x86_64",
        all(any(target_os = "macos", target_os = "linux"), target_arch = "aarch64")
    ))]
    if darwin::tls::key::addressable() {
        return tlv_get_addr as *const () as usize as u64;
    }
    tlv_allocate_thunk as *const () as usize as u64
}

/// A naked function that calls `$target(descriptor)` and preserves every register
/// but the return value, as dyld's `tlv_get_addr` slow path does.
macro_rules! preserving {
    ($(#[$meta:meta])* $name:ident => $target:path) => {
        $(#[$meta])*
        #[cfg(target_arch = "aarch64")]
        #[unsafe(naked)]
        pub unsafe extern "C" fn $name(_descriptor: *const u8) -> *mut u8 {
            core::arch::naked_asm!(
                "stp x29, x30, [sp, #-16]!",
                "mov x29, sp",
                "stp x1, x2, [sp, #-16]!",
                "stp x3, x4, [sp, #-16]!",
                "stp x5, x6, [sp, #-16]!",
                "stp x7, x8, [sp, #-16]!",
                "stp x9, x10, [sp, #-16]!",
                "stp x11, x12, [sp, #-16]!",
                "stp x13, x14, [sp, #-16]!",
                "stp x15, x16, [sp, #-16]!",
                "stp q0, q1, [sp, #-32]!",
                "stp q2, q3, [sp, #-32]!",
                "stp q4, q5, [sp, #-32]!",
                "stp q6, q7, [sp, #-32]!",
                "bl {target}",
                "ldp q6, q7, [sp], #32",
                "ldp q4, q5, [sp], #32",
                "ldp q2, q3, [sp], #32",
                "ldp q0, q1, [sp], #32",
                "ldp x15, x16, [sp], #16",
                "ldp x13, x14, [sp], #16",
                "ldp x11, x12, [sp], #16",
                "ldp x9, x10, [sp], #16",
                "ldp x7, x8, [sp], #16",
                "ldp x5, x6, [sp], #16",
                "ldp x3, x4, [sp], #16",
                "ldp x1, x2, [sp], #16",
                "ldp x29, x30, [sp], #16",
                "ret",
                target = sym $target,
            )
        }

        $(#[$meta])*
        #[cfg(target_arch = "x86_64")]
        #[unsafe(naked)]
        pub unsafe extern "C" fn $name(_descriptor: *const u8) -> *mut u8 {
            core::arch::naked_asm!(
                "push rbp",
                "mov rbp, rsp",
                // Eight pushes keep the sixteen-byte alignment the frame arrived
                // with, so the `sub` below lands the `call` on an aligned stack.
                "push rdi",
                "push rsi",
                "push rdx",
                "push rcx",
                "push r8",
                "push r9",
                "push r10",
                "push r11",
                "sub rsp, 128",
                "movaps [rsp + 0x00], xmm0",
                "movaps [rsp + 0x10], xmm1",
                "movaps [rsp + 0x20], xmm2",
                "movaps [rsp + 0x30], xmm3",
                "movaps [rsp + 0x40], xmm4",
                "movaps [rsp + 0x50], xmm5",
                "movaps [rsp + 0x60], xmm6",
                "movaps [rsp + 0x70], xmm7",
                "call {target}",
                "movaps xmm0, [rsp + 0x00]",
                "movaps xmm1, [rsp + 0x10]",
                "movaps xmm2, [rsp + 0x20]",
                "movaps xmm3, [rsp + 0x30]",
                "movaps xmm4, [rsp + 0x40]",
                "movaps xmm5, [rsp + 0x50]",
                "movaps xmm6, [rsp + 0x60]",
                "movaps xmm7, [rsp + 0x70]",
                "add rsp, 128",
                "pop r11",
                "pop r10",
                "pop r9",
                "pop r8",
                "pop rcx",
                "pop rdx",
                "pop rsi",
                "pop rdi",
                "pop rbp",
                "ret",
                target = sym $target,
            )
        }
    };
}

preserving! {
    /// [`super::block::tlv_allocate`] behind dyld's register discipline.
    tlv_allocate_thunk => super::block::tlv_allocate
}

preserving! {
    /// [`super::block::tlv_bootstrap`] behind dyld's register discipline: what
    /// `__tlv_bootstrap` is bound to, for a descriptor `register` declined.
    tlv_bootstrap_thunk => super::block::tlv_bootstrap
}

/// Apple's arm64 `tlv_get_addr`. `x16` and `x17` are the intra-procedure scratch
/// registers, which a callee may clobber without saving. Off macOS the native
/// backend runs only once `TPIDRRO_EL0` holds the Darwin array.
#[cfg(all(any(target_os = "macos", target_os = "linux"), target_arch = "aarch64"))]
#[unsafe(naked)]
unsafe extern "C" fn tlv_get_addr(_descriptor: *const u8) -> *mut u8 {
    core::arch::naked_asm!(
        "mrs x16, tpidrro_el0",
        "and x16, x16, #0xfffffffffffffff8",
        "ldr x17, [x0, #8]",
        "ldr x16, [x16, x17, lsl #3]",
        "cbz x16, 1f",
        "ldr x17, [x0, #16]",
        "add x0, x16, x17",
        "ret",
        "1:",
        "b {slow}",
        slow = sym tlv_allocate_thunk,
    )
}

/// x86-64 `tlv_get_addr`, for a native guest: `%gs` is the Darwin array, the host's
/// own on a Mac and [`darwin::tls::DarwinThread`]'s elsewhere.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn tlv_get_addr(_descriptor: *const u8) -> *mut u8 {
    core::arch::naked_asm!(
        "mov rax, [rdi + 8]",
        "mov rax, gs:[rax * 8]",
        "test rax, rax",
        "jz 1f",
        "add rax, [rdi + 16]",
        "ret",
        "1:",
        "jmp {slow}",
        slow = sym tlv_allocate_thunk,
    )
}

/// `tlv_get_addr` as x86-64 *guest* code, for an emulated guest.
///
/// The fast path is lifted like any other guest code, so a thread-local read that
/// hits never leaves the JIT: the descriptor's `call` links to it, `%gs` is the
/// context's `gs_base`, and the `ret` links back. Only the slow path leaves, by a
/// direct tail `jmp` to an import address - the shape of a call through `__stubs`,
/// which is the one microlift matches - so an inline fast import or the marshaller
/// makes the call and returns to the caller.
pub(super) mod guest {
    use super::*;

    /// `mov rax, [rdi+8]; mov rax, gs:[rax*8]; test rax, rax; jz slow;
    /// add rax, [rdi+16]; ret; slow: jmp SLOT`.
    const CODE: [u8; 28] = [
        0x48, 0x8B, 0x47, 0x08, // mov rax, [rdi+8]
        0x65, 0x48, 0x8B, 0x04, 0xC5, 0x00, 0x00, 0x00, 0x00, // mov rax, gs:[rax*8]
        0x48, 0x85, 0xC0, // test rax, rax
        0x74, 0x05, // jz slow
        0x48, 0x03, 0x47, 0x10, // add rax, [rdi+16]
        0xC3, // ret
        // slow:
        0xE9, 0x04, 0x00, 0x00, 0x00, // jmp SLOT
    ];

    /// Where `slow:` begins - the whole of `tlv_get_addr` where keys cannot be read
    /// off the thread pointer.
    const SLOW: u64 = 23;

    /// The import address the slow path jumps to. Never executed: an import's
    /// address stands for the host function, as a `__stubs` entry's does. It holds
    /// that function's address anyway, for a reader of the page.
    const SLOT: u64 = 32;

    /// The symbol the slot is imported as. See `abi::table`.
    const SYMBOL: &str = "_rosemary_tlv_allocate";

    static PAGE: OnceLock<Option<u64>> = OnceLock::new();

    /// The page, mapped on first use.
    fn page() -> Option<u64> {
        *PAGE.get_or_init(map)
    }

    /// The thunk an emulated descriptor points at.
    pub fn entry() -> u64 {
        let page = page().expect("rosemary: could not map tlv_get_addr");
        match darwin::tls::key::addressable() {
            true => page,
            false => page + SLOW,
        }
    }

    /// The page's code, if it exists and holds `va`.
    fn code(va: u64) -> Option<u64> {
        let page = (*PAGE.get()?)?;
        (va >= page && va < page + CODE.len() as u64).then_some(page)
    }

    /// Whether `va` is `tlv_get_addr`, and so guest code.
    pub fn contains(va: u64) -> bool {
        code(va).is_some()
    }

    /// The region to lift in order to execute `va`.
    pub fn lift_region(va: u64) -> Option<(crate::region::Region, Vec<u8>)> {
        let page = code(va)?;
        let region = crate::region::region_for(&CODE, page, va, false)?;
        Some((region, CODE.to_vec()))
    }

    /// `va` as a name, for a diagnostic.
    pub fn locate(va: u64) -> Option<String> {
        let page = code(va)?;
        Some(format!("<tlv_get_addr+{:#x}>", va - page))
    }

    /// The slow path's import site, at its slot's address.
    pub fn import_site() -> Option<(u64, &'static ImportSite)> {
        static SITE: OnceLock<ImportSite> = OnceLock::new();
        let page = page()?;
        let site = SITE.get_or_init(|| ImportSite {
            symbol: SYMBOL.to_string(),
            library: None,
            resolved: super::super::block::tlv_allocate as *const () as usize as u64,
            tag: 0,
            is_stub: false,
            plan: Default::default(),
        });
        Some((page + SLOT, site))
    }

    /// The site registered at `va`, if `va` is the slot.
    pub fn import_at(va: u64) -> Option<&'static ImportSite> {
        let (slot, site) = import_site()?;
        (va == slot).then_some(site)
    }

    /// Map the page and write the code and the slot.
    ///
    /// Readable and writable by this runtime and never executable - the guest is
    /// x86-64 and it is lifted, which is the decision every guest mapping is made
    /// under.
    fn map() -> Option<u64> {
        let size = machish::memory::host_page_size() as usize;
        // SAFETY: an anonymous private mapping of one page at an address of the
        // kernel's choosing. Nothing is assumed about where it lands.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return None;
        }
        let base = base as *mut u8;
        // SAFETY: the page just mapped, owned by this module, filled before its
        // address is published. `int3` everywhere else, so a stray jump into it
        // traps rather than running zeroes.
        unsafe {
            std::ptr::write_bytes(base, 0xCC, size);
            std::ptr::copy_nonoverlapping(CODE.as_ptr(), base, CODE.len());
            (base.add(SLOT as usize) as *mut u64)
                .write(super::super::block::tlv_allocate as *const () as usize as u64);
        }
        Some(base as usize as u64)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_slow_path_jumps_to_the_import() {
            // `jmp rel32` at SLOW is five bytes; its target is the next instruction
            // plus the displacement.
            assert_eq!(CODE[SLOW as usize], 0xE9);
            let disp = i32::from_le_bytes(CODE[SLOW as usize + 1..][..4].try_into().unwrap());
            assert_eq!(SLOW + 5 + disp as u64, SLOT);
            assert_eq!(CODE.len() as u64, SLOW + 5);
        }

        #[test]
        fn the_fast_path_branches_to_the_slow_one() {
            // `jz rel8` at 16 lands on SLOW.
            assert_eq!(CODE[16], 0x74);
            assert_eq!(18 + CODE[17] as u64, SLOW);
        }

        #[test]
        fn the_page_is_guest_code_and_its_slot_an_import() {
            let at = entry();
            assert!(contains(at));
            let (slot, site) = import_site().unwrap();
            assert!(!contains(slot), "the slot is data, not code");
            assert_eq!(import_at(slot).map(|s| s.symbol.as_str()), Some(SYMBOL));
            assert!(lift_region(at).is_some());
            assert_eq!(
                site.resolved,
                super::super::super::block::tlv_allocate as *const () as usize as u64
            );
        }
    }
}
