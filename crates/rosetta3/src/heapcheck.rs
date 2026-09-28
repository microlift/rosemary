use std::collections::BTreeSet;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use machish::memory::Regions;
use machish::report::LinkReport;

/// Set to check every free the guest makes against the guest's own allocator.
pub const ENV: &str = "ROSETTA3_CHECK_GUEST_FREE";

pub fn requested() -> bool {
    std::env::var_os(ENV).is_some_and(|v| v != "0")
}

/// Set to arm **this process's** `mi_realloc_aligned`, armed when mimalloc's own
/// page map does not own the pointer it is handed.
///
/// `selftest` additionally proves the arm discriminates, by running the same
/// predicate over a block this process allocated and over two addresses that
/// cannot possibly be one - a stack slot and an `mmap`ed page. **An arm that
/// installs, prints nothing, and is read as "no crossings" is the failure this
/// exists to prevent, so the control is not optional.**
pub const HOST_ENV: &str = "ROSETTA3_CHECK_HOST_REALLOC";

pub fn host_requested() -> bool {
    std::env::var_os(HOST_ENV).is_some_and(|v| v != "0")
}

fn host_selftest() -> bool {
    std::env::var(HOST_ENV).is_ok_and(|v| v == "selftest")
}

/// The guest's real `free`, and the predicate that says whether it owns a
/// pointer. Written once, before any guest code runs.
static REAL_FREE: AtomicUsize = AtomicUsize::new(0);
static OWNS: AtomicUsize = AtomicUsize::new(0);
static REAL_REALLOC: AtomicUsize = AtomicUsize::new(0);
static FOREIGN: AtomicU64 = AtomicU64::new(0);
/// Proof the trampoline is on the path at all: a check that is never called
/// reports the same "no crossings" a clean run does.
static ANNOUNCED: std::sync::Once = std::sync::Once::new();

/// Return addresses already reported, so a crossing on a hot path says its
/// piece once rather than once per call.
static SEEN: Mutex<BTreeSet<u64>> = Mutex::new(BTreeSet::new());

/// `free(p)`, with `ra` appended by the trampoline.
/// Whether the guest's allocator made the block at `p`, reporting `ra` when it
/// did not. `false` means the caller must not hand it over.
fn owned(p: *mut c_void, ra: u64, what: &str) -> bool {
    ANNOUNCED.call_once(|| {
        eprintln!("rosetta3: heapcheck: the check is live - first {what} from {ra:#x}");
    });
    let owns: unsafe extern "C" fn(*const c_void) -> bool =
        // SAFETY: `OWNS` holds the address of the guest's
        // `mi_is_in_heap_region`, whose signature this is.
        unsafe { std::mem::transmute(OWNS.load(Ordering::Relaxed)) };
    // SAFETY: asking an allocator whether an address is one of its own is
    // defined for any address; that is the whole point of the call.
    if unsafe { owns(p) } {
        return true;
    }
    FOREIGN.fetch_add(1, Ordering::Relaxed);
    if SEEN.lock().unwrap_or_else(|e| e.into_inner()).insert(ra) {
        eprintln!(
            "rosetta3: heapcheck: {what}({p:?}) from {ra:#x} - the guest's allocator does not own \
             this block, so it came from somewhere else in this process. Left alone rather than \
             handed over."
        );
        eprintln!("rosetta3: heapcheck:   {ra:#x} is {}", where_is(ra));
    }
    false
}

/// `free(p)`, with `ra` appended by the trampoline.
unsafe extern "C" fn checked_free(p: *mut c_void, ra: u64) {
    if p.is_null() || !owned(p, ra, "free") {
        return;
    }
    let real: unsafe extern "C" fn(*mut c_void) =
        // SAFETY: `REAL_FREE` holds the guest's own `free`.
        unsafe { std::mem::transmute(REAL_FREE.load(Ordering::Relaxed)) };
    // SAFETY: the allocator has just said the block is its own.
    unsafe { real(p) };
}

/// `realloc(p, size)`. It frees too, and a foreign pointer reaching it is the
/// same crossing one step removed - so the block is copied out rather than
/// handed over, which is the only answer that keeps the caller's data.
unsafe extern "C" fn checked_realloc(p: *mut c_void, size: usize, ra: u64) -> *mut c_void {
    let real: unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void =
        // SAFETY: `REAL_REALLOC` holds the guest's own `realloc`.
        unsafe { std::mem::transmute(REAL_REALLOC.load(Ordering::Relaxed)) };
    if p.is_null() || owned(p, ra, "realloc") {
        // SAFETY: either a plain allocation or a block the allocator owns.
        return unsafe { real(p, size) };
    }
    // SAFETY: a fresh block of `size`, filled from the foreign one. The old
    // block is left alone: this allocator cannot free it and the other one is
    // not reachable from here.
    unsafe {
        let fresh = real(std::ptr::null_mut(), size);
        if !fresh.is_null() {
            std::ptr::copy_nonoverlapping(p.cast::<u8>(), fresh.cast::<u8>(), size);
        }
        fresh
    }
}

/// The guest's own `mi_realloc_aligned`, when it has one.
static REAL_REALLOC_ALIGNED: AtomicUsize = AtomicUsize::new(0);

/// `mi_realloc_aligned(p, newsize, alignment)`. Its own trampoline because its
/// third argument is an alignment, which the two-argument one above would read
/// as a newsize.
unsafe extern "C" fn checked_realloc_aligned(
    p: *mut c_void,
    newsize: usize,
    alignment: usize,
    ra: u64,
) -> *mut c_void {
    let real: unsafe extern "C" fn(*mut c_void, usize, usize) -> *mut c_void =
        // SAFETY: `REAL_REALLOC_ALIGNED` holds the guest's own
        // `mi_realloc_aligned`, recovered from the binding it was rebound from.
        unsafe { std::mem::transmute(REAL_REALLOC_ALIGNED.load(Ordering::Relaxed)) };
    if p.is_null() || owned(p, ra, "mi_realloc_aligned") {
        // SAFETY: either a plain allocation or a block the allocator owns.
        return unsafe { real(p, newsize, alignment) };
    }
    // SAFETY: a fresh block of `newsize`, filled from the foreign one, and the
    // old block left alone. The copy is bounded by the new size, which is the
    // same bound the two-argument path above uses; nothing here knows the old
    // block's size, and this path does not run unless a guest actually imports
    // the aligned realloc.
    unsafe {
        let fresh = real(std::ptr::null_mut(), newsize, alignment);
        if !fresh.is_null() {
            std::ptr::copy_nonoverlapping(p.cast::<u8>(), fresh.cast::<u8>(), newsize);
        }
        fresh
    }
}

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".globl rosetta3_checked_free",
    "rosetta3_checked_free:",
    // The return address is still at the top of the stack, so it becomes the
    // second argument and the tail jump leaves the frame exactly as `free`
    // would have left it.
    "mov rsi, [rsp]",
    "jmp {inner}",
    inner = sym checked_free,
);

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".globl rosetta3_checked_realloc",
    "rosetta3_checked_realloc:",
    "mov rdx, [rsp]",
    "jmp {inner}",
    inner = sym checked_realloc,
);

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".globl rosetta3_checked_realloc_aligned",
    "rosetta3_checked_realloc_aligned:",
    "mov rcx, [rsp]",
    "jmp {inner}",
    inner = sym checked_realloc_aligned,
);

#[cfg(target_arch = "x86_64")]
unsafe extern "C" {
    fn rosetta3_checked_free();
    fn rosetta3_checked_realloc();
    fn rosetta3_checked_realloc_aligned();
}

/// Where each guest image landed, so a return address can be named without
/// taking the loader's lock from inside a free.
static IMAGES: Mutex<Vec<(String, u64, u64)>> = Mutex::new(Vec::new());

/// This executable's own `.symtab`, kept so a return address inside *this*
/// binary can be named. The host arm's crossings are host code by
/// construction - `where_is` said only "this runtime or a host library", which
/// is not a caller.
static SELF: Mutex<Option<selfimage::Image>> = Mutex::new(None);

fn where_is(addr: u64) -> String {
    let images = IMAGES.lock().unwrap_or_else(|e| e.into_inner());
    for (name, lo, hi) in images.iter() {
        if addr >= *lo && addr < *hi {
            let short = name.rsplit('/').next().unwrap_or(name);
            return format!("{short}+{:#x}", addr - lo);
        }
    }
    drop(images);
    let mut self_image = SELF.lock().unwrap_or_else(|e| e.into_inner());
    if self_image.is_none() {
        *self_image = selfimage::program();
    }
    if let Some(image) = self_image.as_ref() {
        let link = addr.wrapping_sub(image.bias);
        if let Some(symbols) = image.symbols.as_ref() {
            if let Some((name, of)) = symbols.function_at(link) {
                return format!("{name}+{of:#x} in this executable");
            }
        }
        if image.loads.iter().any(|p| {
            let lo = p.p_vaddr as u64;
            addr >= lo && addr < lo + p.p_memsz
        }) {
            return format!("this executable + {link:#x}, in no function with a size");
        }
    }
    "in no guest image and in no function this executable knows - so the caller is a host \
     library"
        .to_string()
}

/// Every name that hands a block back to the allocator.
const FREES: &[&str] = &["free", "mi_free"];
const REALLOCS: &[&str] = &["realloc", "mi_realloc"];

/// The *aligned* realloc, which is a **different function at a different
/// address** whose third argument is an alignment, not a size.
///
/// These names are deliberately **not** in [`REALLOCS`]. Folding them in would
/// point a three-argument function at the two-argument trampoline above, and
/// `rdx` at that trampoline's entry would be an alignment being read as the
/// newsize. They get their own arity-matched trampoline, which is what "cover
/// the aligned names" has to mean to be worth anything.
const REALLOCS_ALIGNED: &[&str] = &["mi_realloc_aligned", "mi_aligned_realloc"];

/// This executable, described from the outside: its load bias, its `PT_LOAD`s,
/// its `PT_GNU_RELRO`, and its `.symtab`.
///
/// The host's mimalloc is statically linked in, so its symbols are in this
/// file's `.symtab` and in **no** dynamic table. `dlsym` - and so
/// `crate::host::symbol` - finds neither `mi_realloc_aligned` nor `_mi_page_map`
/// nor `mi_page_map_max_address`, and `mi_is_in_heap_region`, mimalloc's public
/// "is this block yours", is not even in the static table. Reading the file is
/// the only way to name any of them, and it buys the second thing the arm
/// needs: a map from an address to a Rust function, so a return address can be
/// attributed by name instead of to "this runtime".
mod selfimage {
    use std::ffi::c_void;
    use std::os::unix::fs::FileExt;

    const SHT_SYMTAB: u32 = 2;
    const STT_FUNC: u8 = 2;

    pub struct Image {
        pub bias: u64,
        pub loads: Vec<libc::Elf64_Phdr>,
        /// The `PT_GNU_RELRO` range at runtime, which is where `.got` is and
        /// which the dynamic linker has made read-only before any of this runs.
        pub relro: Option<(u64, u64)>,
        pub symbols: Option<Symbols>,
    }

    impl Image {
        pub fn runtime(&self, link_addr: u64) -> u64 {
            link_addr + self.bias
        }

        /// The GOT slot holding `target`, by scanning the one writable region
        /// the dynamic linker has already resolved into.
        ///
        /// Returns every match. Exactly one is required and the caller says so
        /// by name if it is not: this binary has **one** GOT slot for
        /// `mi_realloc_aligned` and **two** for `mi_free`, so uniqueness is a
        /// thing to check rather than to assume.
        pub fn got_slots_for(&self, target: u64) -> Vec<u64> {
            let Some((lo, hi)) = self.relro else {
                return Vec::new();
            };
            let mut out = Vec::new();
            let mut addr = lo;
            while addr + 8 <= hi {
                // SAFETY: `relro` is this program's own resolved GOT range, so
                // every 8-aligned word in it is mapped and readable.
                let word = unsafe { std::ptr::read_volatile(addr as *const u64) };
                if word == target {
                    out.push(addr);
                }
                addr += 8;
            }
            out
        }

        /// Make one page writable, because the GOT is in RELRO by the time
        /// anything here can run. Returns the address to write through.
        pub fn make_writable(&self, addr: u64) -> Option<u64> {
            // `sysconf`, not a constant: the page size is a property of this
            // host and guessing it wrong would mprotect the wrong span.
            // SAFETY: `_SC_PAGESIZE` takes no argument and always answers.
            let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            if size <= 0 {
                return None;
            }
            let size = size as u64;
            let page = addr & !(size - 1);
            // SAFETY: a `PROT_READ|PROT_WRITE` mapping of one page that is
            // already mapped in this process. Widening a mapping this process
            // owns is defined; nothing is created and nothing is unmapped.
            let ok = unsafe {
                libc::mprotect(page as *mut c_void, size as usize, libc::PROT_READ | libc::PROT_WRITE)
            };
            (ok == 0).then_some(page)
        }
    }

    /// `dlpi_name` is empty for exactly one entry in the walk: the program.
    extern "C" fn visit(
        info: *mut libc::dl_phdr_info,
        _size: libc::size_t,
        data: *mut c_void,
    ) -> libc::c_int {
        // SAFETY: `data` is the `&mut Option<Image>` `program` passes, and the
        // callback contract is that it outlives the walk.
        let out = unsafe { &mut *data.cast::<Option<Image>>() };
        if out.is_some() {
            return 1;
        }
        // SAFETY: `info` is a live `dl_phdr_info` for the duration of the call.
        let (phdr, num, addr) = unsafe {
            (
                (*info).dlpi_phdr,
                (*info).dlpi_phnum as usize,
                (*info).dlpi_addr,
            )
        };
        if phdr.is_null() || num == 0 {
            return 1;
        }
        // SAFETY: the loader guarantees `dlpi_phnum` entries at `dlpi_phdr`.
        let phdrs = unsafe { std::slice::from_raw_parts(phdr, num) };
        let loads = phdrs
            .iter()
            .filter(|p| p.p_type == libc::PT_LOAD)
            .copied()
            .collect::<Vec<_>>();
        let relro = phdrs
            .iter()
            .find(|p| p.p_type == libc::PT_GNU_RELRO)
            .map(|p| (p.p_vaddr as u64 + addr, p.p_vaddr as u64 + addr + p.p_memsz));
        *out = Some(Image {
            bias: addr,
            loads,
            relro,
            symbols: Symbols::load().ok().flatten(),
        });
        1
    }

    pub fn program() -> Option<Image> {
        let mut out = None;
        // SAFETY: `visit` writes only through the pointer it is handed, and
        // `out` outlives the call.
        unsafe { libc::dl_iterate_phdr(Some(visit), (&mut out as *mut Option<Image>).cast()) };
        out
    }

    /// One ELF symbol. Only the fields this uses.
    pub struct Sym {
        pub name_off: u32,
        pub info: u8,
        pub shndx: u16,
        pub value: u64,
        pub size: u64,
    }

    pub struct Symbols {
        symtab: Vec<u8>,
        strtab: Vec<u8>,
        /// Indices into `symtab`, ordered by `st_value`, so one address is one
        /// binary search. Built once, at install, and read-only afterwards.
        by_value: Vec<usize>,
    }

    impl Symbols {
        pub fn load() -> std::io::Result<Option<Symbols>> {
            let file = std::fs::File::open("/proc/self/exe")?;
            let mut ehdr = [0u8; 64];
            file.read_exact_at(&mut ehdr, 0)?;
            // A 64-bit little-endian ELF, or nothing here can read it.
            if &ehdr[0..4] != b"\x7fELF" || ehdr[4] != 2 || ehdr[5] != 1 {
                return Ok(None);
            }
            let le = |b: &[u8]| -> u64 {
                let mut a = [0u8; 8];
                a.copy_from_slice(&b[..8]);
                u64::from_le_bytes(a)
            };
            let shoff = le(&ehdr[0x28..]);
            let entsize = u16::from_le_bytes([ehdr[0x3a], ehdr[0x3b]]) as usize;
            let count = u16::from_le_bytes([ehdr[0x3c], ehdr[0x3d]]) as usize;
            if shoff == 0 || count == 0 || entsize < 64 {
                return Ok(None);
            }
            // SAFETY: `FileExt::read_at` fills exactly `shdrs.len()` bytes or
            // reports short; `count * entsize` is this file's section table.
            let shdrs = read_file_at(&file, shoff, count * entsize)?;
            let u32at = |i: usize, at: usize| -> u32 {
                let mut a = [0u8; 4];
                a.copy_from_slice(&shdrs[i * entsize + at..i * entsize + at + 4]);
                u32::from_le_bytes(a)
            };
            let u64at = |i: usize, at: usize| -> u64 {
                le(&shdrs[i * entsize + at..i * entsize + at + 8])
            };
            let Some(sym_i) = (0..count).find(|&i| u32at(i, 4) == SHT_SYMTAB) else {
                return Ok(None);
            };
            let str_i = u32at(sym_i, 40) as usize;
            if str_i >= count {
                return Ok(None);
            }
            let symtab = read_file_at(&file, u64at(sym_i, 24), u64at(sym_i, 32) as usize)?;
            let strtab = read_file_at(&file, u64at(str_i, 24), u64at(str_i, 32) as usize)?;
            let symbols = Symbols {
                symtab,
                strtab,
                by_value: Vec::new(),
            };
            let mut by_value = (0..symbols.len()).collect::<Vec<usize>>();
            by_value.sort_unstable_by_key(|&i| symbols.sym(i).value);
            Ok(Some(Symbols { by_value, ..symbols }))
        }

        fn len(&self) -> usize {
            self.symtab.len() / 24
        }

        fn sym(&self, i: usize) -> Sym {
            let b = &self.symtab[i * 24..i * 24 + 24];
            let g32 = |at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
            let mut a = [0u8; 8];
            a.copy_from_slice(&b[8..16]);
            let value = u64::from_le_bytes(a);
            a.copy_from_slice(&b[16..24]);
            Sym {
                name_off: g32(0),
                info: b[4],
                shndx: u16::from_le_bytes([b[6], b[7]]),
                value,
                size: u64::from_le_bytes(a),
            }
        }

        pub fn name(&self, off: u32) -> Option<&str> {
            let rest = self.strtab.get(off as usize..)?;
            let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
            std::str::from_utf8(&rest[..end]).ok()
        }

        /// The link-time address of a symbol that is actually defined here.
        pub fn value_of(&self, want: &str) -> Option<u64> {
            (0..self.len()).find(|&i| {
                let s = self.sym(i);
                s.shndx != 0 && self.name(s.name_off) == Some(want)
            })
            .map(|i| self.sym(i as usize).value)
        }

        /// The function containing `addr`, and `addr`'s offset into it.
        pub fn function_at(&self, addr: u64) -> Option<(&str, u64)> {
            let i = self.by_value.partition_point(|&i| self.sym(i).value <= addr);
            if i == 0 {
                return None;
            }
            let s = self.sym(self.by_value[i - 1]);
            if s.shndx == 0 || s.size == 0 || s.info & 0xf != STT_FUNC {
                return None;
            }
            (addr < s.value + s.size).then(|| {
                (
                    self.name(s.name_off).unwrap_or("?"),
                    addr - s.value,
                )
            })
        }
    }

    fn read_file_at(file: &std::fs::File, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        let mut read = 0;
        while read < len {
            match file.read_at(&mut buf[read..], offset + read as u64)? {
                0 => break,
                n => read += n,
            }
        }
        Ok(buf)
    }
}

// ---------------------------------------------------------------------------
// The host's own `mi_realloc_aligned`
// ---------------------------------------------------------------------------
//
// The wall's fault is mimalloc's `mi_usable_size` (exported also as
// `malloc_size`), and in this binary that function has exactly **one** call
// site, inside `mi_realloc_aligned` - the *aligned* realloc, which `mi_realloc`
// and `mi_free` never call. So the arm belongs on that one function.
//
// **The reference census that matters is a trap, and it is why this note
// exists.** `mi_realloc_aligned` is reached through a *GOT slot*, not a direct
// call: eight `call *0x160acd0(%rip)` sites and one `jmp *0x160acd0(%rip)`
// (`__rust_realloc`). A scan for `E8`/`E9 rel32` naming `mi_realloc_aligned`
// finds **zero**, and an `objdump | grep` for its address finds only the
// function's own label - so the host binary appears to contain an allocator
// function nothing can reach. It is not unreachable; it is reached indirectly.
// `objdump -d | grep -c 160acd0` is 9, and `mi_free`'s slot is 1335, which is
// the positive control that makes the count mean something.
//
// Two more measurements, both of which narrow the wall rather than widen it:
//
// - **Seven of the eight `call` sites pass `alignment == 8`** (`mov edx,0x8`
//   immediately before the call), and `mi_realloc_aligned` opens with
//   `cmp rdx,0x8; jbe` - so those seven **cannot reach `mi_usable_size` at
//   all**. The only site that can is `0x80d4a2`, in
//   `alloc::raw_vec::RawVecInner::finish_grow`, which forwards the old
//   layout's alignment in `rdx`.
// - The GOT slot is in `.got`, which is inside `PT_GNU_RELRO` and so is
//   **read-only** by the time any of this runs. Writing it needs an
//   `mprotect`, which [`Image::make_writable`] does and reports.

/// The `_mi_page_map` pointer, the number of first-level entries it has, and
/// the largest address the map covers. All four are read out of mimalloc's own
/// variables at install - **their values, not their addresses**, which is the
/// mistake this arm made the first time and which faulted in its own
/// self-test: with the address of `_mi_page_map` in hand, `p >> 29` indexes
/// `.bss` at an arbitrary offset and reads unmapped memory. Every one of these
/// is printed at install so a reader can check it against mimalloc's source
/// rather than take it on trust.
static PAGE_MAP: AtomicU64 = AtomicU64::new(0);
static PAGE_MAP_COUNT: AtomicU64 = AtomicU64::new(0);
static MAX_ADDRESS: AtomicU64 = AtomicU64::new(0);
/// The real `mi_realloc_aligned`, recovered from the GOT slot being replaced.
static REAL_HOST_REALLOC: AtomicU64 = AtomicU64::new(0);

/// Host reallocs this arm has seen, and how many of them crossed.
static HOST_CALLS: AtomicU64 = AtomicU64::new(0);
static HOST_CROSSINGS: AtomicU64 = AtomicU64::new(0);
static HOST_ANNOUNCED: std::sync::Once = std::sync::Once::new();
/// Reporting allocates, and a `realloc` that arrives *because* something
/// allocated would re-enter this arm. One flag, and the inner report is
/// dropped rather than recursed.
static IN_REPORT: AtomicBool = AtomicBool::new(false);

/// What mimalloc's own page map says about an address.
///
/// This reproduces `_mi_ptr_page_ex` and `_mi_safe_ptr_page` from mimalloc v3's
/// `internal.h` rather than calling `mi_is_in_heap_region`, which is the public
/// spelling of the safe one but is in no symbol table in this binary. The
/// arithmetic is not a guess; it is the disassembly of `mi_usable_size`:
///
/// ```text
/// 8187e0  mov  rax,QWORD PTR [rip+..]   # 16216c0 <_mi_page_map>
/// 8187ed  shr  rdx,0x1d                 # p >> 29
/// 8187f1  mov  rdx,QWORD PTR [rax+rdx*8]  # the first level: the load that faults
/// 8187f8  shr  rax,0x10
/// 8187fc  and  eax,0x1fff               # (p >> 16) & 0x1fff
/// 818801  mov  rdi,QWORD PTR [rdx+rax*8]  # SIGSEGV here when the first level is null
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    /// The map has this slice and it is a page of the allocator's.
    Owned,
    /// The first level is there and the slice is not. `mi_usable_size`
    /// survives this - it tests the result and returns 0 - so a crossing of
    /// this shape does **not** fault, and is reported separately from the one
    /// that does.
    Slice,
    /// The first level is null. This is the wall's shape: it faults at
    /// `fault_at`, which is the address a `SIGSEGV` report would carry.
    Submap { fault_at: u64 },
    /// Beyond the address range the map covers at all, so the question cannot
    /// be asked of it. mimalloc's own safe lookup answers null here too, and it
    /// does not fault.
    Unmapped,
    /// The map is not initialised, so nothing can be said. **Not a crossing**,
    /// and counted apart from them so it cannot be read as one.
    Uninitialised,
}

impl Page {
    fn owned(self) -> bool {
        self == Page::Owned
    }
}

/// Ask mimalloc's page map about `p`.
///
/// Only ever reads memory this process has mapped: the first level is a load
/// from the map, and the second is reached only when the first is non-null, so
/// this cannot fault on a foreign pointer - which is the whole reason to read
/// the map instead of letting the allocator read it.
fn page_of(p: u64) -> Page {
    let map = PAGE_MAP.load(Ordering::Relaxed);
    if map == 0 {
        return Page::Uninitialised;
    }
    // mimalloc's own bounds, and both are its own values rather than constants
    // invented here. `count` is the length of the first level, so this test is
    // what makes the load below in-bounds no matter what `p` is - which is the
    // whole reason this function is safe to call on any address at all.
    // `max_address` is what its own `_mi_safe_ptr_page` compares against.
    let count = PAGE_MAP_COUNT.load(Ordering::Relaxed);
    let idx = p >> 29;
    if count != 0 && idx >= count {
        return Page::Unmapped;
    }
    let max = MAX_ADDRESS.load(Ordering::Relaxed);
    if max != 0 && p >= max {
        return Page::Unmapped;
    }
    let sub_idx = ((p >> 16) & 0x1fff) as usize;
    // SAFETY: `map` is `_mi_page_map`, an array this allocator owns and keeps
    // initialised for the life of the process, and `idx` is inside it by the
    // count above. `Relaxed` is what mimalloc itself uses for this load.
    let submap = unsafe { std::ptr::read_volatile((map as *const usize).add(idx as usize)) };
    if submap == 0 {
        return Page::Submap {
            fault_at: (sub_idx * 8) as u64,
        };
    }
    // SAFETY: a non-null first level points at `MI_PAGE_MAP_SUB_COUNT` entries.
    let page = unsafe {
        let sub = submap as *const usize;
        std::ptr::read_volatile(sub.add(sub_idx))
    };
    if page == 0 {
        Page::Slice
    } else {
        Page::Owned
    }
}

/// `mi_realloc_aligned(p, newsize, alignment)`, entered from the GOT slot the
/// host's `GlobalAlloc::realloc` calls through, with the return address
/// appended.
///
/// The return address is taken from the stack rather than from `rbp` because
/// `mi_realloc_aligned` has no frame pointer - it does `sub rsp,0x28` and then
/// reuses `rbp` as a general register - so there is no frame to walk. The
/// return address is still at the top of the stack because the arm replaces a
/// *call target*, not a call: the eight sites and `__rust_realloc`'s tail jump
/// all leave the caller's return address exactly where it was.
unsafe extern "C" fn host_realloc_aligned(
    p: *mut c_void,
    newsize: usize,
    alignment: usize,
    ra: u64,
) -> *mut c_void {
    HOST_CALLS.fetch_add(1, Ordering::Relaxed);
    // Reporting allocates, and a `realloc` that arrives *because* something
    // allocated re-enters this function. The guard is taken **around** the
    // liveness line rather than only around the crossing report, because
    // `HOST_ANNOUNCED.call_once` on a thread already inside that `Once` blocks
    // forever - which is a hang, not a diagnostic.
    if !IN_REPORT.swap(true, Ordering::Relaxed) {
        HOST_ANNOUNCED.call_once(|| {
            eprintln!(
                "rosetta3: heapcheck: the host arm is live - first mi_realloc_aligned from \
                 {ra:#x}, which is {}",
                where_is(ra)
            );
        });
        if !p.is_null() {
            let page = page_of(p as u64);
            if !page.owned() {
                report_crossing(p, newsize, alignment, ra, page, "host");
            }
        }
        IN_REPORT.store(false, Ordering::Relaxed);
    }
    let real: unsafe extern "C" fn(*mut c_void, usize, usize) -> *mut c_void =
        // SAFETY: `REAL_HOST_REALLOC` is the address this slot held a moment
        // ago, read back out of it at install. The arm does not touch the
        // function it replaced, so it is still the function's entry.
        unsafe { std::mem::transmute(REAL_HOST_REALLOC.load(Ordering::Relaxed)) };
    // SAFETY: the caller's own three arguments, forwarded unchanged, into the
    // allocator function they were meant for.
    unsafe { real(p, newsize, alignment) }
}

/// One crossing, in full, and once per return address.
fn report_crossing(
    p: *mut c_void,
    newsize: usize,
    alignment: usize,
    ra: u64,
    page: Page,
    who: &str,
) {
    HOST_CROSSINGS.fetch_add(1, Ordering::Relaxed);
    let n = HOST_CALLS.load(Ordering::Relaxed);
    let (shape, detail) = match page {
        Page::Submap { fault_at } => (
            "the page map's FIRST level is null for that 512 MiB segment, which is the load \
             that SIGSEGVs inside mi_usable_size",
            format!(
                "p >> 29 = {:#x}, (p >> 16) & 0x1fff = {:#x}, so mi_usable_size would fault at \
                 {fault_at:#x} here",
                p as u64 >> 29,
                (p as u64 >> 16) & 0x1fff
            ),
        ),
        Page::Slice => (
            "the page map has that segment but not that slice, so the allocator does not own \
             the block. This shape does NOT fault: mi_usable_size tests the page it gets back \
             and answers 0",
            format!(
                "p >> 29 = {:#x}, (p >> 16) & 0x1fff = {:#x}",
                p as u64 >> 29,
                (p as u64 >> 16) & 0x1fff
            ),
        ),
        Page::Unmapped => (
            "the address is beyond the range mimalloc's page map covers at all, so the \
             question cannot be asked of it. This shape does NOT fault",
            format!("p = {:#x}, page map covers below {:#x}", p as u64, MAX_ADDRESS.load(Ordering::Relaxed)),
        ),
        Page::Uninitialised => (
            "mimalloc's page map is not initialised, so this arm cannot answer. NOT a crossing \
             and not counted as one",
            String::new(),
        ),
        Page::Owned => return,
    };
    if SEEN.lock().unwrap_or_else(|e| e.into_inner()).insert(ra) {
        eprintln!(
            "rosetta3: heapcheck: {who} mi_realloc_aligned(p={p:?}, newsize={newsize}, \
             alignment={alignment}) from {ra:#x}"
        );
        eprintln!("rosetta3: heapcheck:   {shape} - {detail}.");
        eprintln!("rosetta3: heapcheck:   {ra:#x} is {}", where_is(ra));
        eprintln!(
            "rosetta3: heapcheck: {n} host mi_realloc_aligned call(s) seen so far, \
             {} crossing(s), {} distinct caller(s)",
            HOST_CROSSINGS.load(Ordering::Relaxed),
            SEEN.lock().unwrap_or_else(|e| e.into_inner()).len()
        );
    }
}

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".globl rosetta3_host_realloc_aligned",
    "rosetta3_host_realloc_aligned:",
    // `rcx` is dead at every entry to this function: all eight `call` sites set
    // only `rdi`, `rsi` and `rdx` immediately before the call, and
    // `__rust_realloc` has already moved its own `rcx` argument into `rsi`.
    "mov rcx, [rsp]",
    "jmp {inner}",
    inner = sym host_realloc_aligned,
);

#[cfg(target_arch = "x86_64")]
unsafe extern "C" {
    fn rosetta3_host_realloc_aligned();
}

/// Arm this process's `mi_realloc_aligned`. Returns the number of GOT slots
/// rewritten, which is 1 or the arm did not go in.
#[cfg(target_arch = "x86_64")]
fn install_host_arm() -> usize {
    use crate::heapcheck::selfimage::program;

    let Some(image) = program() else {
        eprintln!(
            "rosetta3: heapcheck: {HOST_ENV} asked for the host arm, but `dl_iterate_phdr` \
             named no program image, so this executable's own symbols cannot be read. No arm. \
             Grep for `heapcheck`."
        );
        return 0;
    };
    let Some(symbols) = image.symbols.as_ref() else {
        eprintln!(
            "rosetta3: heapcheck: {HOST_ENV} asked for the host arm, but /proc/self/exe has no \
             readable 64-bit ELF .symtab, and mimalloc is statically linked here, so there is no \
             other way to name `_mi_page_map`. No arm. Grep for `heapcheck`."
        );
        return 0;
    };
    let need = |name: &str| -> Option<u64> {
        symbols
            .value_of(name)
            .map(|link| image.runtime(link))
            .or_else(|| {
                eprintln!(
                    "rosetta3: heapcheck: {HOST_ENV}: `{name}` is not in this executable's \
                     .symtab (nor in any dynamic table, because mimalloc is statically linked \
                     here), so the host arm cannot be aimed. No arm. Grep for `heapcheck`."
                );
                None
            })
    };
    let (Some(realloc), Some(page_map), Some(count), Some(max_address)) = (
        need("mi_realloc_aligned"),
        need("_mi_page_map"),
        need("mi_page_map_count"),
        need("mi_page_map_max_address"),
    ) else {
        return 0;
    };
    // **Values, not addresses.** All four are objects, not constants: reading
    // `_mi_page_map`'s own storage would hand back the address of the pointer
    // rather than the array, and every index computed from it would then be an
    // offset into `.bss`. This is not a hypothetical - it is what the first
    // version of this arm did, and it faulted inside its own self-test on
    // `mov rax,[rdx+rax*8]`, one line below the arm's install message.
    let read_u64 = |addr: u64| -> u64 {
        // SAFETY: each address is the runtime address of a `size_t`-sized
        // object inside this executable's own `.bss`, resolved from its
        // `.symtab` a moment ago and mapped for the life of the process.
        unsafe { std::ptr::read_volatile(addr as *const u64) }
    };
    let map_value = read_u64(page_map);
    let count_value = read_u64(count);
    let max_value = read_u64(max_address);
    PAGE_MAP.store(map_value, Ordering::Relaxed);
    PAGE_MAP_COUNT.store(count_value, Ordering::Relaxed);
    MAX_ADDRESS.store(max_value, Ordering::Relaxed);
    eprintln!(
        "rosetta3: heapcheck: {HOST_ENV}: mimalloc's page map, read out of its own variables: \
         _mi_page_map holds {map_value:#x}, it has {count_value} first-level entries (so it \
         covers addresses below {:#x}), and mi_page_map_max_address holds {max_value:#x}. For \
         reference MI_PAGE_MAP_COUNT is `1 << (MI_MAX_VABITS - 29)`, which is 524288 for the 48 \
         vbits this build has.",
        count_value << 29
    );

    // The GOT slot is found by the value the loader put in it, and there has to
    // be exactly one: this binary has two for `mi_free`.
    let slots = image.got_slots_for(realloc);
    if slots.len() != 1 {
        eprintln!(
            "rosetta3: heapcheck: {HOST_ENV}: {} GOT slots in this executable's RELRO hold \
             mi_realloc_aligned ({realloc:#x}), not 1, and the one to arm is a guess. No arm. \
             Grep for `heapcheck`.",
            slots.len()
        );
        return 0;
    }
    let slot = slots[0];
    if page_of(0x1000) == Page::Uninitialised {
        eprintln!(
            "rosetta3: heapcheck: {HOST_ENV}: `_mi_page_map` (variable at {page_map:#x}) holds \
             {map_value:#x}, so mimalloc's map is not initialised and the arm would answer \
             nothing. No arm. Grep for `heapcheck`."
        );
        return 0;
    }
    let Some(page) = image.make_writable(slot) else {
        eprintln!(
            "rosetta3: heapcheck: {HOST_ENV}: could not make the page holding the GOT slot \
             ({slot:#x}) writable, so the arm could not be written. No arm. Grep for \
             `heapcheck`."
        );
        return 0;
    };
    REAL_HOST_REALLOC.store(realloc, Ordering::Relaxed);
    // SAFETY: `slot` is an 8-aligned word of this program's own `.got`, just
    // made writable, and the old value is kept in `REAL_HOST_REALLOC` above. The
    // page stays writable afterwards so no concurrent read can fault on it; the
    // install line says so.
    let arm = rosetta3_host_realloc_aligned as *const () as u64;
    unsafe { std::ptr::write_volatile(slot as *mut u64, arm) };
    // SAFETY: `libc::atexit` takes a plain `extern "C" fn()`.
    unsafe { libc::atexit(host_arm_summary) };
    eprintln!(
        "rosetta3: heapcheck: {HOST_ENV}: this process's mi_realloc_aligned ({realloc:#x}) is \
         armed at GOT slot {slot:#x} (page {page:#x} made writable, left writable) behind \
         {arm:#x}. Ownership is mimalloc's own page map, and every crossing is reported as it \
         happens. Grep for `heapcheck`."
    );
    if host_selftest() {
        host_arm_selftest(slot, arm);
    }
    1
}

#[cfg(not(target_arch = "x86_64"))]
fn install_host_arm() -> usize {
    0
}

/// The end-of-run tally, so that a run with **no** crossing still says how many
/// calls were watched. Without it "0 crossings" and "the arm was never
/// entered" print the same thing, and that is exactly the reading this arm
/// exists to make impossible.
///
/// An `atexit` handler does not run when the process dies on a signal - which
/// is what the wall does - so a killed run shows the crossing lines and
/// nothing else. The crossing lines are the part that matters there.
extern "C" fn host_arm_summary() {
    let calls = HOST_CALLS.load(Ordering::Relaxed);
    let crossings = HOST_CROSSINGS.load(Ordering::Relaxed);
    eprintln!(
        "rosetta3: heapcheck: {HOST_ENV}: {calls} host mi_realloc_aligned call(s) watched, \
         {crossings} crossing(s) - {}",
        if crossings == 0 {
            "the allocator owned every pointer it was handed"
        } else {
            "every one is above, once per caller"
        }
    );
}

/// Prove the arm discriminates, before any quiet run is believed.
///
/// Three controls, and each has to pass: the predicate says `Owned` for a block
/// this process allocated, it says *not* `Owned` for two addresses that cannot
/// possibly be one (a slot of this stack, and a page `mmap` handed us), and the
/// GOT slot really does read back as the arm. A fourth calls through the arm
/// with a block the allocator owns, so a broken call-through cannot pass as a
/// quiet run either.
#[cfg(target_arch = "x86_64")]
fn host_arm_selftest(slot: u64, arm: u64) {
    // SAFETY: the GOT slot is this program's own and the page is writable, as
    // `install_host_arm` just established.
    let written = unsafe { std::ptr::read_volatile(slot as *const u64) };
    let got_ok = written == arm;
    eprintln!(
        "rosetta3: heapcheck: self-test: PAGE_MAP={:#x} PAGE_MAP_COUNT={:#x} \
         MAX_ADDRESS={:#x}",
        PAGE_MAP.load(Ordering::Relaxed),
        PAGE_MAP_COUNT.load(Ordering::Relaxed),
        MAX_ADDRESS.load(Ordering::Relaxed)
    );
    eprintln!(
        "rosetta3: heapcheck: self-test: GOT slot {slot:#x} reads {written:#x}, arm is \
         {arm:#x} - {}",
        if got_ok { "the arm is in place" } else { "FAILED: the arm is NOT in place" }
    );

    // A stack slot. Nothing allocated a block on this process's stack.
    let on_stack = 0u8;
    let stack_addr = &on_stack as *const u8 as u64;
    let stack_page = page_of(stack_addr);
    // SAFETY: a page of this process's own address space, mapped anonymous, so
    // it overlaps nothing. Never written, only asked about.
    let mapped = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    let mapped_addr = if mapped as isize > 0 {
        Some(mapped as u64)
    } else {
        None
    };

    // A real block from this process's allocator.
    // SAFETY: a plain `malloc(64)`.
    let mine = unsafe { libc::malloc(64) } as u64;
    let mine_page = page_of(mine);

    let stack_ok = !stack_page.owned();
    let mapped_ok = mapped_addr.map(|a| !page_of(a).owned()).unwrap_or(false);
    let mine_ok = mine_page.owned();
    eprintln!(
        "rosetta3: heapcheck: self-test: a block this process allocated ({mine:#x}) reads \
         {mine_page:?} - {}",
        if mine_ok { "owned, as it must be" } else { "FAILED: the predicate does not know its own" }
    );
    eprintln!(
        "rosetta3: heapcheck: self-test: a slot of this process's own stack ({stack_addr:#x}) \
         reads {stack_page:?} - {}",
        if stack_ok { "not owned, as it must be" } else { "FAILED: it claims the stack" }
    );
    if let Some(a) = mapped_addr {
        eprintln!(
            "rosetta3: heapcheck: self-test: a page this process mmap'd ({a:#x}) reads \
             {:?} - {}",
            page_of(a),
            if !page_of(a).owned() { "not owned, as it must be" } else { "FAILED: it claims the page" }
        );
    } else {
        eprintln!("rosetta3: heapcheck: self-test: FAILED: mmap gave nothing to test against");
    }
    // SAFETY: through the arm, on a block the allocator owns, so this is a
    // normal realloc that also proves the call-through works.
    //
    // `mine` is deliberately **not** freed afterwards: the realloc already
    // freed it and returned a different block, so a second `free(mine)` is a
    // double free. That was in the first version of this self-test, and it is
    // why the arm looked like it corrupted the heap - `realloc`'s own contract
    // is that the old pointer is dead on return, and a self-test that ignores
    // it is not testing the arm, it is attacking the allocator.
    let grown = unsafe { host_realloc_aligned(mine as *mut c_void, 128, 8, 0xdead_beef) };
    // SAFETY: the arm returned the allocator's own answer.
    let call_ok = !grown.is_null();
    eprintln!(
        "rosetta3: heapcheck: self-test: calling through the arm on a block the allocator owns \
         returned {} - {}",
        if grown.is_null() { "null".to_string() } else { format!("{grown:?}") },
        if call_ok { "the call-through works" } else { "FAILED: the arm does not call through" }
    );
    // SAFETY: `grown` is this process's allocator's own answer to the realloc
    // above, and nothing else holds it.
    unsafe { libc::free(grown) };
    if mapped_addr.is_some() {
        // SAFETY: the mapping `mmap` returned, still mapped, never written.
        unsafe { libc::munmap(mapped, 4096) };
    }
    let all = got_ok && stack_ok && mapped_ok && mine_ok && call_ok;
    eprintln!(
        "rosetta3: heapcheck: self-test: summary - GOT readback {got_ok}, own block {mine_ok}, \
         stack slot {stack_ok}, mmap page {mapped_ok}, call-through {call_ok}"
    );
    eprintln!(
        "rosetta3: heapcheck: self-test: {} - the arm can report a crossing, so a run that \
         reports none is a measurement",
        if all { "all controls passed" } else { "ONE OR MORE CONTROLS FAILED" }
    );
}

#[cfg(not(target_arch = "x86_64"))]
fn host_arm_selftest(_slot: u64, _arm: u64) {}


/// How many foreign frees were caught, for the line printed at exit.
pub fn foreign_count() -> u64 {
    FOREIGN.load(Ordering::Relaxed)
}

/// Replace the guest's `free` with the checking one, and - independently of
/// [`ENV`] - arm this process's own `mi_realloc_aligned` when [`HOST_ENV`] is
/// set.
///
/// Call after [`crate::interpose::apply`], so the address recovered is the
/// allocator the guest's code actually ends up calling.
#[cfg(target_arch = "x86_64")]
pub fn install<M: Regions>(
    report: &mut LinkReport,
    memory: &mut M,
) -> Result<usize, machish::Error> {
    // Before either check, and whether or not either is asked for: `where_is`
    // reads both of these, and a decline below must not leave a crossing
    // reportable only as "somewhere in this runtime".
    *IMAGES.lock().unwrap_or_else(|e| e.into_inner()) = report
        .images
        .iter()
        .map(|i| {
            let end = i
                .segments
                .iter()
                .map(|s| s.vmaddr.wrapping_add(s.vmsize))
                .max()
                .unwrap_or(i.base);
            (i.name.clone(), i.base, end)
        })
        .collect();
    // The host arm first, and regardless of `ENV`: the crossing the wall is made
    // of happens in *this* process's allocator, so it is not something the
    // guest-side check below can decline to look at.
    //
    // Gated on its own variable and **off unless it is set**. The first version
    // of this line had no gate, and the default-env run then printed twelve
    // `heapcheck:` lines a run had no business printing - which is the whole
    // failure this file is about, committed by the very instrument built to
    // detect it.
    if host_requested() {
        install_host_arm();
    }
    if !requested() {
        return Ok(0);
    }
    let resolved = |name: &str| {
        report
            .images
            .iter()
            .flat_map(|image| &image.bindings)
            .find(|b| darwin::base_name(&b.symbol) == name && b.resolved != 0)
            .map(|b| b.resolved)
    };
    let Some(owns) = resolved("mi_is_in_heap_region") else {
        eprintln!(
            "rosetta3: heapcheck: {ENV} was set, but the guest imports no `mi_is_in_heap_region`, \
             so there is nothing to ask which blocks are its own - no check installed. Grep for \
             `heapcheck`, not for {ENV}: that is the string that says the check did not go in. \
             (The host's own allocator crossing is a separate question, and {HOST_ENV} is what \
             asks it.)"
        );
        return Ok(0);
    };
    let Some(free) = FREES.iter().find_map(|n| resolved(n)) else {
        return Ok(0);
    };
    let realloc = REALLOCS.iter().find_map(|n| resolved(n));
    // Resolved by name *and* address, so a binding is only moved onto a
    // trampoline that has the same arity as the function it replaced.
    let realloc_aligned = REALLOCS_ALIGNED.iter().find_map(|n| resolved(n));
    OWNS.store(owns as usize, Ordering::Relaxed);
    REAL_FREE.store(free as usize, Ordering::Relaxed);
    REAL_REALLOC.store(realloc.unwrap_or(0) as usize, Ordering::Relaxed);
    REAL_REALLOC_ALIGNED.store(realloc_aligned.unwrap_or(0) as usize, Ordering::Relaxed);

    let checked_free = rosetta3_checked_free as *const () as u64;
    let checked_realloc = rosetta3_checked_realloc as *const () as u64;
    let checked_realloc_aligned = rosetta3_checked_realloc_aligned as *const () as u64;
    let mut rebound = 0;
    for image in &mut report.images {
        for binding in &mut image.bindings {
            let name = darwin::base_name(&binding.symbol);
            let replacement = if FREES.contains(&name) && binding.resolved == free {
                checked_free
            } else if REALLOCS.contains(&name) && Some(binding.resolved) == realloc {
                checked_realloc
            } else if REALLOCS_ALIGNED.contains(&name) && Some(binding.resolved) == realloc_aligned {
                checked_realloc_aligned
            } else {
                continue;
            };
            memory.write(binding.slot_va, &replacement.to_le_bytes())?;
            binding.resolved = replacement;
            rebound += 1;
        }
    }
    eprintln!(
        "rosetta3: heapcheck: {ENV}: {rebound} free/realloc binding(s) now checked against the \
         guest's allocator, across {} free, {} realloc and {} aligned-realloc name(s)",
        FREES.len(),
        REALLOCS.len(),
        REALLOCS_ALIGNED.len()
    );
    Ok(rebound)
}

#[cfg(not(target_arch = "x86_64"))]
pub fn install<M: Regions>(
    _report: &mut LinkReport,
    _memory: &mut M,
) -> Result<usize, machish::Error> {
    Ok(0)
}
