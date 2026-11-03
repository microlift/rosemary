//! Data-page stubs that enter a shared thunk template with their own guest address.
//!
//! A thunk compiled per guest function costs a Cranelift compile for every method
//! a class realizes. Instead one template is compiled per signature, taking the
//! guest address as one extra integer argument after the real ones, and each guest
//! function gets a stub that loads its address into that argument's register and
//! jumps to the template.
//!
//! The stubs are laid out as `objc/imp_block.rs` lays out its own: a writable page
//! of `[guest address, template]` pairs followed by an executable page of identical
//! stubs, each reading the pair exactly one page behind itself. The code page is
//! written once, so handing out a stub is only a data store. Every stub on a page
//! loads the same register, so each register has pages of its own.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Bytes per stub, and per `[guest address, template]` pair behind it.
const SLOT: usize = 16;

/// Integer argument registers of the host's C convention, in argument order.
#[cfg(target_arch = "aarch64")]
pub const ARGUMENT_REGISTERS: usize = 8;
#[cfg(target_arch = "x86_64")]
pub const ARGUMENT_REGISTERS: usize = 6;
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub const ARGUMENT_REGISTERS: usize = 0;

/// Free stubs, by the argument register they load.
static FREE: Mutex<[Vec<usize>; ARGUMENT_REGISTERS]> =
    Mutex::new([const { Vec::new() }; ARGUMENT_REGISTERS]);

fn page_size() -> usize {
    // SAFETY: no preconditions.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}

/// A stub handed out; returned to its page's free list when dropped.
pub struct Stub {
    code: usize,
    register: usize,
}

impl Stub {
    /// What the host calls.
    pub fn address(&self) -> u64 {
        self.code as u64
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        let words = data_words(self.code);
        words[0].store(0, Ordering::Release);
        words[1].store(0, Ordering::Release);
        FREE.lock().unwrap_or_else(|e| e.into_inner())[self.register].push(self.code);
    }
}

/// A stub that calls `template` with `guest` in integer argument `register`, or
/// `None` if no stub page could be made.
pub fn allocate(register: usize, guest: u64, template: u64) -> Option<Stub> {
    if register >= ARGUMENT_REGISTERS {
        return None;
    }
    let mut free = FREE.lock().unwrap_or_else(|e| e.into_inner());
    if free[register].is_empty() {
        grow(register, &mut free[register])?;
    }
    let code = free[register].pop().expect("grow added stubs");
    drop(free);
    let words = data_words(code);
    words[0].store(guest, Ordering::Release);
    words[1].store(template, Ordering::Release);
    Some(Stub { code, register })
}

/// The pair one page behind the stub at `code`.
fn data_words(code: usize) -> &'static [AtomicU64; 2] {
    // SAFETY: stub pages are never unmapped, and the data page precedes the code page.
    unsafe { &*((code - page_size()) as *const [AtomicU64; 2]) }
}

/// Map one more pair of pages of stubs loading `register` onto `free`.
fn grow(register: usize, free: &mut Vec<usize>) -> Option<()> {
    let page = page_size();
    // SAFETY: a fresh private anonymous mapping.
    let base = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            page * 2,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if base == libc::MAP_FAILED {
        return None;
    }
    let code = base as usize + page;
    let template = stub(register, page);
    for slot in (0..page).step_by(SLOT) {
        // SAFETY: inside the code page, which is still writable.
        unsafe { std::ptr::copy_nonoverlapping(template.as_ptr(), (code + slot) as *mut u8, SLOT) };
    }
    // SAFETY: the code page of the mapping above.
    if unsafe { libc::mprotect(code as *mut _, page, libc::PROT_READ | libc::PROT_EXEC) } != 0 {
        // SAFETY: unmapping the mapping above, which nothing has seen.
        unsafe { libc::munmap(base, page * 2) };
        return None;
    }
    #[cfg(target_arch = "aarch64")]
    {
        unsafe extern "C" {
            fn __clear_cache(start: *mut libc::c_char, end: *mut libc::c_char);
        }
        // SAFETY: the range just written.
        unsafe { __clear_cache(code as *mut _, (code + page) as *mut _) };
    }
    // Popped from the end, so the first stub is handed out first.
    free.extend((0..page).step_by(SLOT).rev().map(|slot| code + slot));
    Some(())
}

/// One stub loading `x<register>`, its pair `page` bytes behind it.
#[cfg(target_arch = "aarch64")]
fn stub(register: usize, page: usize) -> [u8; SLOT] {
    const LDR_LITERAL_X: u32 = 0x5800_0000;
    const X16: u32 = 16;
    const BR_X16: u32 = 0xd61f_0200;
    // A literal load is pc-relative in words from the load itself.
    let ldr = |rt: u32, from: usize, word: usize| {
        let offset = word as i64 - (page + from) as i64;
        LDR_LITERAL_X | ((((offset / 4) as u32) & 0x7ffff) << 5) | rt
    };
    let words = [
        ldr(register as u32, 0, 0), // ldr x<register>, guest
        ldr(X16, 4, 8),             // ldr x16, template
        BR_X16,                     // br x16
        0,
    ];
    let mut code = [0; SLOT];
    for (i, word) in words.iter().enumerate() {
        code[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    code
}

/// One stub loading the `register`th SysV integer argument, its pair `page` bytes
/// behind it.
#[cfg(target_arch = "x86_64")]
fn stub(register: usize, page: usize) -> [u8; SLOT] {
    // rdi, rsi, rdx, rcx, r8, r9 as ModRM register numbers.
    const REGISTERS: [u8; ARGUMENT_REGISTERS] = [7, 6, 2, 1, 8, 9];
    const REX_W: u8 = 0x48;
    const REX_R: u8 = 0x04;
    const MOV_LOAD: u8 = 0x8b;
    const RIP_RELATIVE: u8 = 0b101;
    const JMP_INDIRECT: [u8; 2] = [0xff, 0x25];
    const MOV_LEN: usize = 7;
    const JMP_LEN: usize = 6;
    let reg = REGISTERS[register];
    // rip-relative displacements count from the end of each instruction.
    let disp = |end: usize, word: usize| ((word as i64 - (page + end) as i64) as i32).to_le_bytes();
    let mut code = [0xcc; SLOT];
    // mov r, [rip - page]
    code[0] = REX_W | if reg >= 8 { REX_R } else { 0 };
    code[1] = MOV_LOAD;
    code[2] = ((reg & 7) << 3) | RIP_RELATIVE;
    code[3..MOV_LEN].copy_from_slice(&disp(MOV_LEN, 0));
    // jmp [rip - page + 8]
    code[MOV_LEN..MOV_LEN + 2].copy_from_slice(&JMP_INDIRECT);
    code[MOV_LEN + 2..MOV_LEN + JMP_LEN].copy_from_slice(&disp(MOV_LEN + JMP_LEN, 8));
    code
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
fn stub(_register: usize, _page: usize) -> [u8; SLOT] {
    unreachable!("no argument registers to load")
}

#[cfg(all(test, any(target_arch = "aarch64", target_arch = "x86_64")))]
mod tests {
    use super::*;

    unsafe extern "C" fn first(guest: u64) -> u64 {
        guest
    }

    unsafe extern "C" fn third(a: u64, b: u64, guest: u64) -> u64 {
        a * 100 + b * 10 + guest
    }

    #[test]
    fn a_stub_passes_its_guest_in_the_register_it_loads() {
        let stub = allocate(0, 0x1234, first as *const () as u64).unwrap();
        let call: unsafe extern "C" fn() -> u64 = unsafe { std::mem::transmute(stub.address()) };
        assert_eq!(unsafe { call() }, 0x1234);

        let stub = allocate(2, 7, third as *const () as u64).unwrap();
        let call: unsafe extern "C" fn(u64, u64) -> u64 =
            unsafe { std::mem::transmute(stub.address()) };
        assert_eq!(unsafe { call(1, 2) }, 127);
    }

    #[test]
    fn a_dropped_stub_is_reused() {
        let register = ARGUMENT_REGISTERS - 1;
        let stub = allocate(register, 1, first as *const () as u64).unwrap();
        let address = stub.address();
        drop(stub);
        let again = allocate(register, 2, first as *const () as u64).unwrap();
        assert_eq!(again.address(), address);
    }
}
