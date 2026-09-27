//! A Mach **memory entry**: the thing `mach_make_memory_entry_64` mints and
//! `vm_map` maps, and the only reason the image ever reaches [`super::vm`]'s
//! memory-object path.
//!
//! # What XNU's is
//!
//! `mach_make_memory_entry_64` forwards to `mach_make_memory_entry_internal`,
//! which asks the VM for a **`vm_named_entry_t`** - a kernel object holding a
//! reference to a memory object and a maximum protection - and hands back its
//! `ipc_port_t`. XNU says what the port is *for* in the comment above
//! `mach_make_memory_entry_named_create`: *"Think of it as a two-stage
//! `vm_remap()` operation. First you get a handle. Second, you get map that
//! handle in somewhere else. Rather than doing it all at once (and without
//! needing access to the other whole map)."* The second stage is
//! `vm_map`/`mach_vm_map` with the port as `memory_object`/`object`.
//!
//! So the object has to satisfy two things at once, and an answer that only
//! satisfies the first is the dangerous one:
//!
//! 1. **`vm_map` of it produces a mapping of the pages it stands for.** The
//!    pages have to be *the same pages*, not a copy: Chromium's browser process
//!    writes a GPU command buffer into one mapping and the GPU process reads it
//!    out of another.
//! 2. **The port reaches a different process and still names the same pages.**
//!    `base/memory/writable_shared_memory_region.h` says the handle is "for
//!    sending the handle from the current process to another", and the only
//!    thing that travels is the `mach_port_t`.
//!
//! # How a port crosses a process boundary here
//!
//! Established from the tree, not assumed:
//!
//! - A port's identity in [`super::space`] is its `Address` - a 64-bit number -
//!   and a send right's whole contents. `super::msg` puts that `Address` in the
//!   message (`mach/msg.rs`'s `Reference { address }`) and the receiver calls
//!   `space::insert_send(address)`, which **draws a fresh name in the receiving
//!   process's space for the same `Address`**. So the name is per-process and
//!   the `Address` is not.
//! - A guest `execve` of a Mach-O becomes `execve(<this runtime>, ["run",
//!   <guest path>, ...])` - `darwin/src/spawn.rs`'s `rewrite`. A child process
//!   is therefore a *fresh* process: it inherits no mappings and no port table.
//!
//! Together those say the only thing a receiving process can do with a port is
//! resolve its `Address`. **So this module makes the `Address` the name of a
//! directory**: `<namespace>/<016x address>/`. A process that did not mint the
//! entry resolves it by opening that directory, and gets the same pages,
//! because it is the same directory. That is the second stage, and it is what
//! the earlier refusal said did not exist.
//!
//! # The pages
//!
//! A file under `<016x address>/data`, mapped `MAP_SHARED`. Two processes
//! mapping one file share the one page cache, so a byte the browser writes is
//! the byte the GPU process reads. That is genuinely shared memory and not two
//! private copies, which is the failure the refusal existed to prevent.
//!
//! Where it is *not* the same as XNU, stated rather than left to be found out
//! later. The one real difference is in **who can reach the object**, not in
//! what a holder of the port gets:
//!
//! - On XNU only a task holding the port can map the entry, and a port cannot
//!   be forged from a number. Here the `Address` is a name in a directory, so
//!   any process of the same user that can read that directory can open it.
//!   That is **the trust boundary `super::bootstrap`'s namespace already
//!   draws**: a 0700 directory under `$XDG_RUNTIME_DIR/rosetta3` holding every
//!   port address the run uses, and that file's own note says so - *"The
//!   namespace is this user's, and a service name is a rendezvous token: another
//!   user able to write one could answer a lookup with a port of their own."* A
//!   directory of its own, rather than a corner of the bootstrap one, keeps a
//!   service name from ever resolving to a memory object.
//!
//! Everything a *holder of the port* observes is the real thing: the same
//! pages, a maximum protection that is enforced, a mapping that outlives the
//! entry, `KERN_INVALID_RIGHT` for a caller asking for more protection than the
//! entry has, and a refusal - never a plausible stand-in - for the shapes listed
//! on [`Make`].
//!
//! `#![cfg(not(target_os = "macos"))]`, like every other module under `mach`:
//! on macOS the host's own kernel has a real `vm_named_entry_t` and shadowing it
//! with a directory of files would be the worst of both worlds.

#![cfg(not(target_os = "macos"))]

use std::collections::HashMap;
use std::ffi::{c_int, c_void};
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};

use super::kern::{
    KERN_INVALID_ADDRESS, KERN_INVALID_ARGUMENT, KERN_INVALID_NAME, KERN_INVALID_RIGHT,
    KERN_NO_SPACE, KERN_NOT_SUPPORTED, KERN_RESOURCE_SHORTAGE, KERN_SUCCESS,
};
// `KERN_PROTECTION_FAILURE` is declared in `super::vm` rather than in
// `super::kern`, because it is the one code `mach_error_string` did not name
// until `vm_protect` needed it. It is XNU's 2, and the image's own
// `CheckPlatformHandlePermissionsCorrespondToMode` compares a return code
// against `0x11` - `KERN_INVALID_RIGHT` - so the two are not to be confused.
use super::vm::KERN_PROTECTION_FAILURE;
use super::space::{self, MACH_PORT_NULL, Name, TASK_SELF};
use super::wire::{self, Address};

/// `MAP_MEM_NAMED_CREATE`, from the *word the image passes* rather than from a
/// header this tree cannot read.
///
/// `base/memory/platform_shared_memory_region_apple.cc` writes
/// `MAP_MEM_NAMED_CREATE | VM_PROT_READ | VM_PROT_WRITE` as `permission_u`, and
/// the call at `Chromium Framework+0x5d954c7` compiles that to
/// `mov ecx,0x20003` - so the flag is `0x20000` and the protection in the low
/// sixteen bits is `0x3`. The other call site, `+0x5d9503a`, passes
/// `mov ecx,0x1`: the flag is **absent**, and that is the other of the two
/// shapes, spelled out on [`Make::Over`].
const MAP_MEM_NAMED_CREATE: i32 = 0x2_0000;

/// The bits of `permission_u` that are a protection rather than a
/// `vm_named_entry_flags_t`.
const PERMISSION_MASK: i32 = 0xffff;

/// The name this module's refusals print.
const SYMBOL: &str = "mach_make_memory_entry_64";

/// Say once that a call was refused, and why.
///
/// The same contract as [`super::vm`]'s `note`, kept local so this module does
/// not have to widen that one's visibility, and for the same reason it writes
/// with `write(2)`: the message has to be out before the refusal is.
fn note(symbol: &str, said: &AtomicBool, why: &str) {
    if !said.swap(true, Ordering::Relaxed) {
        let line = format!("rosetta3: {symbol}: {why}\n");
        // SAFETY: a live buffer for the length written.
        unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
    }
}

/// `ROSETTA3_NO_MEMOBJ=1` puts this module back the way it was before memory
/// entries existed: every call refuses with `KERN_NOT_SUPPORTED` and says so.
///
/// **Off by default, and a measurement rather than a mode.** It exists for one
/// experiment: the first run that ever got past the guest's
/// `CHECK(kr == KERN_SUCCESS)` on this call is also the first run that reached
/// a fault 0x3CB8A bytes further on, and those two facts cannot both be read
/// off one run. With this set the old wall is back and the question "did the
/// memory object introduce the new fault, or merely carry the run past the old
/// one" is answerable by observation rather than by argument.
///
/// It refuses rather than answering `KERN_SUCCESS` with nothing behind it, so a
/// run with it set is the run that died at the `CHECK` - which is the point.
const NO_MEMOBJ: &str = "ROSETTA3_NO_MEMOBJ";

/// Whether [`NO_MEMOBJ`] is set in the environment.
fn disabled() -> bool {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    *DISABLED.get_or_init(|| std::env::var_os(NO_MEMOBJ).is_some())
}

/// The directory every memory entry of this user lives in.
///
/// `None` when the directory cannot be made - a read-only or absent
/// `XDG_RUNTIME_DIR` with no writable `/tmp` - and every call below treats that
/// as a refusal rather than inventing a location.
fn namespace() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let base = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(runtime) => Path::new(&runtime).join("rosetta3"),
            None => PathBuf::from(format!("/tmp/rosetta3-{}", unsafe { libc::getuid() })),
        };
        if fs::create_dir_all(&base).is_err() {
            return None;
        }
        let _ = fs::set_permissions(&base, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        let dir = base.join("memobj");
        if fs::create_dir_all(&dir).is_err() {
            return None;
        }
        let _ = fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        Some(dir)
    })
    .clone()
}

/// One memory entry's directory, from the `Address` its port names.
fn directory(address: Address) -> Option<PathBuf> {
    Some(namespace()?.join(format!("{address:016x}")))
}

/// Nothing is there.
fn absent() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "no memory entry at this address")
}

/// What a memory entry is, as recorded next to its pages.
///
/// Written when the entry is minted and read by **every** process that resolves
/// the entry, so the in-process copy in [`minted`] is a cache and never the
/// authority. That is the whole of the second stage: a process that did not
/// mint the entry can answer every question about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    /// Bytes in the entry's **view**, rounded up to a whole number of pages -
    /// which is what `*size_u` is written back as, and what the image's
    /// `DCHECK_GE(vm_size, size)` asks for.
    pub size: u64,
    /// The most protection any mapping of this entry may carry. Enforced by
    /// [`map`], and by [`super::vm::vm_protect`] for a range already mapped.
    pub max_protection: i32,
    /// The entry whose `data` file this one is a **view of**, and the byte
    /// offset into it. Zero and zero for an entry minted with
    /// `MAP_MEM_NAMED_CREATE`; set for the `ConvertToReadOnly` shape, which
    /// makes an entry over a mapping that already exists - and it is a view of
    /// the *same file*, not a copy of it, which is what keeps the write the
    /// browser made through the writable mapping visible through the read-only
    /// one.
    pub source: Address,
    pub source_offset: u64,
    /// The process that minted it, for [`sweep`].
    pub pid: i32,
}

/// The entries minted in *this* process, as a cache of [`Meta`].
///
/// Kept because a mapping of a memory entry has to be remembered so that
/// `mach_make_memory_entry_64` over an existing *address* can find the pages
/// behind it - see [`backing`]. It is a cache and not a registry: dropping it
/// costs a directory read and nothing else.
fn minted() -> &'static RwLock<HashMap<Address, Meta>> {
    static MINTED: OnceLock<RwLock<HashMap<Address, Meta>>> = OnceLock::new();
    MINTED.get_or_init(|| RwLock::new(HashMap::new()))
}

/// A mapping this process made of a memory entry.
///
/// Needed because `mach_make_memory_entry_64`'s second shape takes an **address
/// in this task**, not a file offset: `ConvertToReadOnly` maps the entry
/// read-only at some address and then asks for a *second* entry over that
/// address, and the only way to know which file is behind an address is to have
/// recorded making the mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    /// Where the mapping is in this address space.
    pub address: u64,
    pub length: u64,
    /// The entry it was made from, and the byte offset into that entry's view.
    pub entry: Address,
    pub view_offset: u64,
}

fn mappings() -> &'static RwLock<Vec<Mapping>> {
    static MAPPED: OnceLock<RwLock<Vec<Mapping>>> = OnceLock::new();
    MAPPED.get_or_init(|| RwLock::new(Vec::new()))
}

/// Record a mapping [`map`] just made, so a later entry over one of its
/// addresses can find the pages.
pub fn remember(mapping: Mapping) {
    let mut all = mappings().write().unwrap_or_else(|e| e.into_inner());
    if !all.contains(&mapping) {
        all.push(mapping);
    }
}

/// Forget the mappings inside a range that has just been unmapped.
///
/// A record that outlives its mapping is not a correctness problem: it can only
/// make a later protection request *more* restrictive, which is the direction
/// that cannot hand a guest writable pages it was refused.
pub fn forget(address: u64, length: u64) {
    let end = address.saturating_add(length);
    let mut all = mappings().write().unwrap_or_else(|e| e.into_inner());
    all.retain(|m| m.address < end && address < m.address.saturating_add(m.length));
}

/// The entry a range of this address space was mapped from, if it was.
pub fn backing(address: u64) -> Option<Mapping> {
    let all = mappings().read().unwrap_or_else(|e| e.into_inner());
    all.iter()
        .copied()
        .find(|m| address >= m.address && address < m.address.saturating_add(m.length))
}

/// The most protection a mapping of the entry behind this address may carry, or
/// `None` for a range this module did not map.
///
/// Read by [`super::vm::vm_protect`], which otherwise cannot tell a range whose
/// ceiling came from a memory entry from one mapped anonymously. `None` is the
/// answer for an ordinary mapping, and the caller keeps the behaviour
/// [`super::vm::vm_protect`] already documents.
pub fn ceiling_of(address: u64) -> Option<i32> {
    let mapping = backing(address)?;
    let meta = resolve(mapping.entry).ok()?;
    Some(meta.max_protection)
}

/// Read an entry's [`Meta`], from this process's cache if it minted one and from
/// the directory otherwise.
fn resolve(address: Address) -> io::Result<Meta> {
    if let Some(meta) = minted()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&address)
    {
        return Ok(*meta);
    }
    let dir = directory(address).ok_or_else(absent)?;
    let text = fs::read_to_string(dir.join("meta")).map_err(|_| absent())?;
    let mut size = None;
    let mut max_protection = None;
    let mut source = None;
    let mut source_offset = None;
    let mut pid = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let number = || value.parse::<u64>().ok();
        match key {
            "size" => size = number(),
            "max_prot" => max_protection = number().map(|v| v as i32),
            "source" => source = number(),
            "source_offset" => source_offset = number(),
            "pid" => pid = number().map(|v| v as i32),
            _ => {}
        }
    }
    Ok(Meta {
        size: size.ok_or_else(absent)?,
        max_protection: max_protection.ok_or_else(absent)?,
        source: source.ok_or_else(absent)?,
        source_offset: source_offset.ok_or_else(absent)?,
        pid: pid.ok_or_else(absent)?,
    })
}

/// The entry that actually owns the `data` file, and the byte offset into it.
///
/// An entry is either the owner - `source == 0` - or a view of one. Following
/// the view is what makes `ConvertToReadOnly` correct: the read-only entry and
/// the writable one are names for **one file**, not two.
fn root(address: Address, meta: &Meta) -> (Address, u64) {
    if meta.source == 0 {
        (address, 0)
    } else {
        (meta.source, meta.source_offset)
    }
}

fn write_meta(dir: &Path, meta: &Meta) -> io::Result<()> {
    fs::write(
        dir.join("meta"),
        format!(
            "size={}\nmax_prot={}\nsource={:016x}\nsource_offset={}\npid={}\n",
            meta.size, meta.max_protection, meta.source, meta.source_offset, meta.pid
        ),
    )
}

/// Remove the entries of processes that are gone.
///
/// Without it every run leaves its memory objects behind, and a full `/tmp` has
/// already cost this project a session - so the sweep is part of the design and
/// not a nicety. It is safe against a *concurrent* run because it only removes
/// a directory whose recorded `pid` no longer exists, checked with `kill(pid, 0)`
/// and not guessed from the name. A directory whose `meta` cannot be read is
/// left alone: removing an entry whose holder is alive is the one failure this
/// module cannot recover from.
///
/// Once per process, at the first mint.
fn sweep() {
    static SWEPT: AtomicBool = AtomicBool::new(false);
    if SWEPT.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(dir) = namespace() else { return };
    let Ok(entries) = fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        let Some(address) = entry
            .file_name()
            .to_str()
            .and_then(|name| u64::from_str_radix(name, 16).ok())
        else {
            continue;
        };
        if let Ok(meta) = resolve(address)
            && !alive(meta.pid)
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Whether a process is still running.
fn alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 sends nothing and only reports existence.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// The page size mappings and entries are rounded to.
fn page_size() -> u64 {
    let published = super::info::VM_PAGE_SIZE.load(Ordering::Relaxed);
    if published != 0 {
        return published;
    }
    super::info::publish_page_size();
    super::info::VM_PAGE_SIZE.load(Ordering::Relaxed)
}

/// Round `size` up to a whole number of pages, or `None` on an overflow.
fn round_up(size: u64, page: u64) -> Option<u64> {
    if page == 0 {
        return None;
    }
    size.checked_add(page - 1).map(|value| value & !(page - 1))
}

/// A fresh `Address` with a directory of its own, or `None` if this host has
/// nowhere to put one.
fn fresh() -> Option<Address> {
    let dir = namespace()?;
    for _ in 0..8 {
        let address = wire::fresh_address();
        // `create_dir` is the collision test: an address already in use has a
        // directory, and `EEXIST` is how that says so, so two processes minting
        // at the same instant cannot end up sharing one.
        if fs::create_dir(dir.join(format!("{address:016x}"))).is_ok() {
            return Some(address);
        }
    }
    None
}

/// What `mach_make_memory_entry_64` is being asked for.
pub enum Make {
    /// `MAP_MEM_NAMED_CREATE` was set and `offset_u` is zero: a **new** memory
    /// object of `size` bytes, to be mapped later by this or another task.
    /// `PlatformSharedMemoryRegion::Create`'s shape.
    Fresh { size: u64, protection: i32 },
    /// `MAP_MEM_NAMED_CREATE` was clear and `offset_u` is an **address in the
    /// target task**: an entry over the object already mapped there, with
    /// `protection` as its ceiling. `ConvertToReadOnly`'s shape - it maps the
    /// entry read-only at an address, then mints a second entry over that
    /// address, so that the read-only view is the one which can be sent on.
    ///
    /// The address has to be one *this process* mapped from a memory entry; a
    /// mapping of anything else is refused, because there is nothing behind it
    /// this tree can name.
    Over {
        address: u64,
        size: u64,
        protection: i32,
    },
}

/// Mint a memory entry and hand back the port that names it.
///
/// `Err` is never an answer: it means this host could not put the pages
/// anywhere, and the caller refuses by name.
pub fn make(request: Make) -> io::Result<Name> {
    let page = page_size().max(1);
    let (source, source_offset, size, max_protection) = match request {
        Make::Fresh { size, protection } => {
            let size = round_up(size, page).ok_or_else(too_big)?;
            (0, 0, size, protection)
        }
        Make::Over {
            address,
            size,
            protection,
        } => {
            let mapping = backing(address).ok_or_else(absent)?;
            let entry = resolve(mapping.entry)?;
            let offset = mapping
                .view_offset
                .checked_add(address - mapping.address)
                .ok_or_else(too_big)?;
            let size = round_up(size, page).ok_or_else(too_big)?;
            // A read-only view of a writable object is the shape the image asks
            // for; a *writable* view of a read-only one would be a promise this
            // host cannot keep, and `mmap(2)` would be asked to break.
            if protection & !entry.max_protection != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "more protection than the object at that address has",
                ));
            }
            (mapping.entry, offset, size, protection)
        }
    };
    if size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a memory object of no bytes",
        ));
    }
    let address = fresh().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no free memory-entry address",
        )
    })?;
    let dir = directory(address).expect("just created");
    let meta = Meta {
        size,
        max_protection,
        source,
        source_offset,
        pid: unsafe { libc::getpid() },
    };
    if source == 0 {
        // The owner: a new file of exactly the entry's size, which a mapping
        // reads. Created and sized **before** the `meta`, so a process that
        // resolves this address in between sees no `meta` and refuses rather
        // than mapping a half-made entry.
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(dir.join("data"))?;
        file.set_len(size)?;
    }
    write_meta(&dir, &meta)?;
    minted()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(address, meta);
    // A send right, which is what XNU hands back: the port goes into messages
    // and never receives one, so `space`'s generic send-right handling -
    // `mach_port_type`, `mach_port_mod_refs`, `mach_port_deallocate`,
    // `mach_port_destroy` - is already the right answer for it. No socket is
    // bound for it, so a message sent to one is refused the way Darwin refuses a
    // message sent to a memory object.
    Ok(space::insert_send(address))
}

fn too_big() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "the size is not a representable mapping",
    )
}

/// The pages of the entry `address` names, open for reading and writing.
fn open_pages(address: Address, meta: &Meta) -> io::Result<fs::File> {
    let (owner, _) = root(address, meta);
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory(owner).ok_or_else(absent)?.join("data"))
}

/// What [`map`] needs to be told, gathered by the caller so this module does not
/// have to know about `*address`.
pub struct Request<'a> {
    /// The port, as the guest passed it.
    pub object: Name,
    /// `vm_map`'s fourth argument, an address in the *target task*. Nothing
    /// here implements mapping over an object that already exists at an
    /// address, so a non-zero one is refused rather than ignored.
    pub offset: u64,
    /// `vm_map`'s seventh argument, in bytes into the entry's view.
    pub file_offset: u64,
    pub size: u64,
    /// The protection the caller asked for, and the ceiling it named.
    pub protection: i32,
    pub max_protection: i32,
    /// Whether the host may choose the address, and if not where.
    pub anywhere: bool,
    pub hint: u64,
    /// The page size mappings are rounded to.
    pub page: u64,
    /// The name refusals print, so `mach_vm_map`'s say `mach_vm_map`.
    pub symbol: &'a str,
}

/// Map a memory entry, or say why not.
///
/// The protections are the load-bearing part. XNU's VM object carries a maximum
/// protection and `vm_map_enter` answers **`KERN_INVALID_RIGHT`** when the
/// caller asks for more than the object has, and
/// `PlatformSharedMemoryRegion::CheckPlatformHandlePermissionsCorrespondToMode`
/// is written around exactly that code:
/// `bool is_read_only = kr == KERN_INVALID_RIGHT;`. So this is the one answer
/// in the whole path that must be `KERN_INVALID_RIGHT`, and must not be
/// confused with any other failure - which is why an unresolvable port below
/// is `KERN_INVALID_NAME` and not this.
pub fn map(request: Request<'_>) -> Result<u64, i32> {
    static SAID_OFFSET: AtomicBool = AtomicBool::new(false);
    static SAID_UNKNOWN: AtomicBool = AtomicBool::new(false);
    static SAID_PROT: AtomicBool = AtomicBool::new(false);
    static SAID_ALIGN: AtomicBool = AtomicBool::new(false);
    static SAID_SHAPE: AtomicBool = AtomicBool::new(false);
    static SAID_CLASH: AtomicBool = AtomicBool::new(false);

    let page = request.page.max(1);
    if request.offset != 0 {
        note(
            request.symbol,
            &SAID_OFFSET,
            &format!(
                "offset {:#x} is an address in the target task, so this asks for the object \
                 already mapped there rather than for the memory entry; that is a second Mach \
                 shape this tree does not implement, and the entry is refused rather than \
                 ignored",
                request.offset
            ),
        );
        return Err(KERN_INVALID_ARGUMENT);
    }
    if request.file_offset & (page - 1) != 0 {
        note(
            request.symbol,
            &SAID_ALIGN,
            &format!(
                "file_offset {:#x} is not a multiple of the {page:#x} page size, and it is the \
                 offset into a memory object",
                request.file_offset
            ),
        );
        return Err(KERN_INVALID_ARGUMENT);
    }
    let address = space::address_of(request.object).ok_or_else(|| {
        note(
            request.symbol,
            &SAID_UNKNOWN,
            &format!(
                "memory object {:#x} is not a name this process holds a right to, so there is \
                 nothing to map. On Darwin that is a port that was destroyed or never received, \
                 and it is refused rather than answered with fresh memory",
                request.object
            ),
        );
        KERN_INVALID_NAME
    })?;
    let meta = match resolve(address) {
        Ok(meta) => meta,
        Err(err) => {
            note(
                request.symbol,
                &SAID_UNKNOWN,
                &format!(
                    "memory object {:#x} names address {address:#018x}, and this user's \
                     memory-object directory has no entry for it ({err}). A port here is a name \
                     for a directory, so this is the case where the port reached a process that \
                     cannot open what it stands for; the pages behind it are not invented",
                    request.object
                ),
            );
            return Err(KERN_INVALID_NAME);
        }
    };
    if request.protection & !meta.max_protection != 0 {
        // `KERN_INVALID_RIGHT`, and nothing else. See this function's own note:
        // the guest reads this one code to decide that a region is read-only.
        note(
            request.symbol,
            &SAID_PROT,
            &format!(
                "protection {:#x} was asked of memory object {:#x}, whose maximum protection is \
                 {:#x}. Mach answers KERN_INVALID_RIGHT here, and it is the code base::subtle::\
                 PlatformSharedMemoryRegion::CheckPlatformHandlePermissionsCorrespondToMode reads \
                 to decide that a region is read-only, so it is the code the guest is entitled \
                 to get",
                request.protection, request.object, meta.max_protection
            ),
        );
        return Err(KERN_INVALID_RIGHT);
    }
    if request.max_protection & !meta.max_protection != 0 {
        note(
            request.symbol,
            &SAID_PROT,
            &format!(
                "max_protection {:#x} is above the memory object's own {:#x}",
                request.max_protection, meta.max_protection
            ),
        );
        return Err(KERN_PROTECTION_FAILURE);
    }
    let Some(size) = round_up(request.size, page) else {
        return Err(KERN_INVALID_ARGUMENT);
    };
    if size == 0 {
        return Err(KERN_INVALID_ARGUMENT);
    }
    let (owner, owner_offset) = root(address, &meta);
    let Some(file_offset) = owner_offset.checked_add(request.file_offset) else {
        return Err(KERN_INVALID_ARGUMENT);
    };
    // The bound is the **owner's** size, not this entry's view of it: a view is
    // a window onto the same file, and the file is what `mmap` is offset into.
    let owner_size = match resolve(owner) {
        Ok(owner) => owner.size,
        Err(err) => {
            let object = request.object;
            note(
                request.symbol,
                &SAID_UNKNOWN,
                &format!("memory object {object:#x} names a view of {owner:#018x}, which has no entry: {err}"),
            );
            return Err(KERN_INVALID_NAME);
        }
    };
    if file_offset.saturating_add(size) > owner_size {
        note(
            request.symbol,
            &SAID_SHAPE,
            &format!(
                "{file_offset:#x}+{size:#x} is past the end of a memory object of \
                 {owner_size} bytes"
            ),
        );
        return Err(KERN_INVALID_ARGUMENT);
    }
    let Some(prot) = super::vm::host_protection(request.protection) else {
        return Err(KERN_INVALID_ARGUMENT);
    };
    if !request.anywhere {
        if request.hint & (page - 1) != 0 {
            note(
                request.symbol,
                &SAID_CLASH,
                &format!(
                    "the mapping has to be exactly at {:#x}, which is not a multiple of the \
                     {page:#x} page size",
                    request.hint
                ),
            );
            return Err(KERN_INVALID_ARGUMENT);
        }
        match super::vm::range_is_mapped(request.hint, request.hint + size) {
            Some(true) => {
                note(
                    request.symbol,
                    &SAID_CLASH,
                    &format!(
                        "{:#x}+{size:#x} is already mapped and there is no flag word here that \
                         asks for it to be replaced, so Mach answers KERN_NO_SPACE",
                        request.hint
                    ),
                );
                return Err(KERN_NO_SPACE);
            }
            None => {
                note(
                    request.symbol,
                    &SAID_CLASH,
                    &format!(
                        "{:#x}+{size:#x} was asked for as a fixed mapping and \
                         /proc/self/maps could not be read, so MAP_FIXED would have replaced \
                         whatever was there without asking",
                        request.hint
                    ),
                );
                return Err(KERN_NO_SPACE);
            }
            Some(false) => {}
        }
    }
    let pages = open_pages(address, &meta).map_err(|err| {
        let object = request.object;
        note(
            request.symbol,
            &SAID_UNKNOWN,
            &format!("the pages of memory object {object:#x} could not be opened: {err}"),
        );
        KERN_INVALID_NAME
    })?;
    // SAFETY: `pages` is a file this function opened, `mmap` writes nothing, and
    // `size` is a whole number of pages at a page-aligned offset.
    let got = unsafe {
        libc::mmap(
            if request.anywhere {
                std::ptr::null_mut()
            } else {
                request.hint as *mut c_void
            },
            size as usize,
            prot as libc::c_int,
            // `MAP_SHARED` is the answer and not a detail: it is what makes two
            // mappings one page cache rather than two, which is the whole
            // difference between shared memory and two private copies.
            libc::MAP_SHARED
                | libc::MAP_NORESERVE
                | if request.anywhere { 0 } else { libc::MAP_FIXED },
            pages.as_raw_fd(),
            file_offset as libc::off_t,
        )
    };
    if got == libc::MAP_FAILED {
        return Err(match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINVAL) => KERN_INVALID_ARGUMENT,
            Some(libc::ENOMEM) => KERN_NO_SPACE,
            _ => KERN_RESOURCE_SHORTAGE,
        });
    }
    remember(Mapping {
        address: got as u64,
        length: size,
        entry: address,
        view_offset: request.file_offset,
    });
    Ok(got as u64)
}

/// `kern_return_t mach_make_memory_entry_64(vm_map_t target_map,
/// memory_object_size_ut *size_u, vm_map_offset_ut offset_u, vm_prot_ut
/// permission_u, ipc_port_t *object_handle, ipc_port_t parent_handle)`
///
/// **Six parameters, and `permission_u` is two things at once.** The low
/// sixteen bits are the maximum protection the entry may ever be mapped with;
/// everything above them is a `vm_named_entry_flags_t`, and
/// `MAP_MEM_NAMED_CREATE` is the bit that decides *which of the two shapes* is
/// being asked for. The image settles both halves: `+0x5d954c7`
/// (`Create`) passes `mov ecx,0x20003` - create a new object, read-write - and
/// `+0x5d9503a` (`ConvertToReadOnly`) passes `mov ecx,0x1` with `rdx` set to
/// the address it has just mapped, which is the *view* shape.
///
/// # The two call sites
///
/// Two, and no more: a byte scan of every `e8 rel32` in `__TEXT,__text` (214
/// MB) for **both** `_mach_make_memory_entry_64` stubs - `0xcca16aa` and
/// `0xcca3bac`, which `macho.py`'s `stub_at` answers with the same name - finds
/// `+0x5d9503a` and `+0x5d954c7` against the first and **zero** against the
/// second. Three is the positive control in the same scan, so the zero is a
/// measurement rather than a scan that finds nothing.
///
/// | | `+0x5d9503a` | `+0x5d954c7` |
/// |---|---|---|
/// | function | `ConvertToReadOnly` | `Create` |
/// | `target_map` (`rdi`) | `_mach_task_self_` | `_mach_task_self_` |
/// | `*size_u` (`rsi`) | a word pre-loaded from `this+0x18` | a word pre-loaded from `size` |
/// | `offset_u` (`rdx`) | the `mapped_addr` argument | `xor edx,edx` |
/// | `permission_u` (`rcx`) | `mov ecx,0x1` = `VM_PROT_READ` | `mov ecx,0x20003` |
/// | `*object_handle` (`r8`) | a local | a local |
/// | `parent_handle` (`r9`) | `xor r9d,r9d` | `xor r9d,r9d` |
/// | what it does with `kr` | `MACH_DLOG` + `return false` | **`MACH_CHECK(kr == KERN_SUCCESS, kr)`** |
///
/// `target_map` is a **genuine task port**: both sites load it from the
/// `__DATA_CONST,__got` slot at `+0xdd50070`, the indirect symbol table names
/// that slot `_mach_task_self_`, and this runtime binds it to
/// `mach::space::TASK_SELF_PORT` (`0x103`) - a name its own port space holds.
///
/// # The `CHECK`, and what it means here
///
/// `+0x5d954c7` is a real `MACH_CHECK` and not a `DCHECK`:
/// `mov r8d,[rbp-0x40]; test r8d,r8d; jne +0x5d956c9`, and `+0x5d956c9` loads
/// `platform_shared_memory_region_apple.cc(150)` and
/// `"Check failed: kr == KERN_SUCCESS"`. The guest aborts, and the run log says
/// so verbatim. That is the guest reporting this host correctly, and it is not
/// weakened here: this function answers `KERN_SUCCESS` because the thing it
/// mints is a real memory object - one file, mapped `MAP_SHARED`, named by a
/// send right this process can put in a message - or it answers a code that
/// says what is missing.
///
/// # What is answered, and what is not
///
/// - **Both shapes the image asks for are answered.** `MAP_MEM_NAMED_CREATE`
///   makes a new object of `*size_u`; without it, `offset_u` is an address in
///   this task and the entry is a view of whatever memory entry that address was
///   mapped from, sharing its one file.
/// - **`parent_handle != MACH_PORT_NULL` is refused by name**: an entry carved
///   out of another entry is a third shape, and approximating it by minting a
///   whole new object would be a different answer rather than a slower one.
/// - **a `vm_named_entry_flags_t` bit other than `MAP_MEM_NAMED_CREATE` is
///   refused by name.**
/// - **`offset_u != 0` together with `MAP_MEM_NAMED_CREATE` is refused**: a new
///   object has no address in `target_map` to start at.
/// - **A `target_map` that is not this task is refused**: the object is minted
///   over a mapping in one address space and this host has one.
/// - **`*size_u` comes back rounded up to a whole number of pages**, which is
///   what a Mach VM object is and what the image's `DCHECK_GE(vm_size, size)`
///   asks for.
///
/// # Safety
///
/// `size_u` and `object_handle` are writable words; `target_map` and
/// `parent_handle` are the caller's own.
pub unsafe extern "C" fn mach_make_memory_entry_64(
    target_map: Name,
    size_u: *mut u64,
    offset_u: u64,
    permission_u: c_int,
    object_handle: *mut Name,
    parent_handle: Name,
) -> i32 {
    static SAID_SHAPE: AtomicBool = AtomicBool::new(false);
    static SAID_FLAGS: AtomicBool = AtomicBool::new(false);
    static SAID_PARENT: AtomicBool = AtomicBool::new(false);
    static SAID_TASK: AtomicBool = AtomicBool::new(false);
    static SAID_NULL: AtomicBool = AtomicBool::new(false);
    static SAID_MADE: AtomicBool = AtomicBool::new(false);
    static SAID_ABSENT: AtomicBool = AtomicBool::new(false);
    static SAID_OFF: AtomicBool = AtomicBool::new(false);

    if disabled() {
        note(
            SYMBOL,
            &SAID_OFF,
            &format!(
                "refused because {NO_MEMOBJ} is set: this measurement turns the whole of this \
                 module off, and a named memory entry is a kernel IPC object that this host has \
                 no other way to be. KERN_NOT_SUPPORTED, which is the answer this call gave \
                 before it was implemented"
            ),
        );
        return KERN_NOT_SUPPORTED;
    }

    if target_map != TASK_SELF {
        note(
            SYMBOL,
            &SAID_TASK,
            &format!(
                "target_map {target_map:#x} is not this task ({TASK_SELF:#x}); a memory object is \
                 minted over a mapping in one address space and this host has one"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if size_u.is_null() || object_handle.is_null() {
        note(SYMBOL, &SAID_NULL, "a null out-parameter");
        return KERN_INVALID_ADDRESS;
    }
    if parent_handle != MACH_PORT_NULL {
        note(
            SYMBOL,
            &SAID_PARENT,
            &format!(
                "parent_handle {parent_handle:#x} asks for an entry carved out of another entry, \
                 which is a third shape of this call. Neither call site in the image passes \
                 anything but MACH_PORT_NULL, and the shape is refused rather than approximated \
                 by minting a whole new object"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller's `size_u`, checked non-null above.
    let size = unsafe { *size_u };
    let protection = permission_u & PERMISSION_MASK;
    let flags = permission_u & !PERMISSION_MASK;
    if flags & !MAP_MEM_NAMED_CREATE != 0 {
        note(
            SYMBOL,
            &SAID_FLAGS,
            &format!(
                "permission_u {permission_u:#x} carries {:#x} above MAP_MEM_NAMED_CREATE \
                 ({MAP_MEM_NAMED_CREATE:#x}); those are vm_named_entry_flags_t this tree does \
                 not implement",
                flags & !MAP_MEM_NAMED_CREATE
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    let request = if flags & MAP_MEM_NAMED_CREATE != 0 {
        if offset_u != 0 {
            note(
                SYMBOL,
                &SAID_SHAPE,
                &format!(
                    "MAP_MEM_NAMED_CREATE asks for a new memory object and offset_u is \
                     {offset_u:#x}; a new object has no address in target_map to start at, so \
                     the request is refused rather than read as a file offset"
                ),
            );
            return KERN_INVALID_ARGUMENT;
        }
        Make::Fresh { size, protection }
    } else {
        Make::Over {
            address: offset_u,
            size,
            protection,
        }
    };
    sweep();
    let port = match make(request) {
        Ok(port) => port,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            note(
                SYMBOL,
                &SAID_ABSENT,
                &format!(
                    "offset_u {offset_u:#x} is an address in this task, and the only mapping \
                     this tree knows the pages behind is one it made from a memory entry itself. \
                     There is nothing at that address to name, and a fresh object would be a \
                     different answer: {err}"
                ),
            );
            return KERN_INVALID_ADDRESS;
        }
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
            note(
                SYMBOL,
                &SAID_SHAPE,
                &format!(
                    "offset_u {offset_u:#x} is an address of an object with less protection than \
                     permission_u {permission_u:#x} asks for: {err}"
                ),
            );
            return KERN_PROTECTION_FAILURE;
        }
        Err(err) => {
            note(
                SYMBOL,
                &SAID_MADE,
                &format!(
                    "this host could not put a memory object anywhere: {err}. The pages have to \
                     be one file that both processes map MAP_SHARED, and there is nowhere to \
                     create one"
                ),
            );
            return if err.kind() == io::ErrorKind::AlreadyExists {
                KERN_RESOURCE_SHORTAGE
            } else {
                KERN_NOT_SUPPORTED
            };
        }
    };
    // A Mach VM object is a whole number of pages, and the image checks
    // `DCHECK_GE(vm_size, size)` - so the rounded size is written back, which is
    // what the caller's own word is for.
    //
    // SAFETY: the caller's two out-parameters, both checked non-null above.
    unsafe {
        *size_u = round_up(size, page_size()).unwrap_or(size).max(size);
        *object_handle = port;
    }
    KERN_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::kern::MACH_MSG_SUCCESS;

    /// A name for a port nothing holds, which is what the refusal paths are
    /// about.
    const STRANGER: Name = 0xdead_beef;

    fn page() -> u64 {
        page_size()
    }

    fn ask(object: Name, size: u64, protection: i32) -> Result<u64, i32> {
        map(Request {
            object,
            offset: 0,
            file_offset: 0,
            size,
            protection,
            max_protection: protection,
            anywhere: true,
            hint: 0,
            page: page(),
            symbol: "test",
        })
    }

    /// The whole shape of `Create` -> map -> write -> map again, in one process,
    /// with the two mappings checked to be **one** page cache: the byte the
    /// first mapping writes is the byte the second reads. That is the property
    /// an anonymous mapping cannot have, and it is the reason the earlier
    /// refusal was right to refuse one.
    #[test]
    fn a_minted_entry_maps_the_same_pages_twice() {
        let size = round_up(2 * page(), page()).expect("a page size");
        let port = make(Make::Fresh {
            size,
            protection: 3,
        })
        .expect("a host with a writable namespace mints one");

        let first = ask(port, size, 3).expect("a fresh entry maps read-write");
        let second = ask(port, size, 3).expect("a second mapping of the same entry");
        assert_ne!(second, first, "two mappings, two addresses, one object");

        // SAFETY: both addresses are whole mappings of `size` bytes this test
        // made, and both buffers are inside them.
        unsafe {
            let marker = b"rosetta3 memory entry";
            std::ptr::copy_nonoverlapping(marker.as_ptr(), first as *mut u8, marker.len());
            let mut seen = [0_u8; 6];
            std::ptr::copy_nonoverlapping((second as *const u8).add(9), seen.as_mut_ptr(), 6);
            assert_eq!(&seen, b"memory", "the second mapping sees the first's write");
            assert_eq!(libc::munmap(first as *mut c_void, size as usize), 0);
            assert_eq!(libc::munmap(second as *mut c_void, size as usize), 0);
        }
        forget(first, size);
        forget(second, size);
    }

    /// The second half of the same thing, from the *other* direction: a process
    /// that did not mint the entry resolves it from the `Address` alone, which
    /// is all a receiving process is given - the port name it draws is its own.
    #[test]
    fn an_entry_resolves_from_its_address_and_not_from_the_name_it_was_given() {
        let size = round_up(page(), page()).expect("a page size");
        let port = make(Make::Fresh {
            size,
            protection: 3,
        })
        .expect("minted");
        let address = space::address_of(port).expect("a send right has an address");
        // Drop the cache, so the read below has to come off the disk - which is
        // what a process that never saw `make` would do.
        minted()
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&address);
        let meta = resolve(address).expect("the entry is on disk, not only in the cache");
        assert_eq!(meta.size, size);
        assert_eq!(meta.max_protection, 3);
        assert!(
            open_pages(address, &meta).is_ok(),
            "and its pages are openable by address alone"
        );
    }

    /// The code the guest reads. `CheckPlatformHandlePermissionsCorrespondToMode`
    /// is `bool is_read_only = kr == KERN_INVALID_RIGHT;` - so a read ceiling
    /// must answer `KERN_INVALID_RIGHT` and a writable entry must answer
    /// `KERN_SUCCESS`, and the two must not be confused with any other failure.
    ///
    /// The three steps are the three `ConvertToReadOnly` steps, in its order: it
    /// maps the writable entry **read-only** at a fresh address, and mints its
    /// replacement entry over *that* address. The browser keeps writing through
    /// its own read-write mapping, which is why the two have to be one file.
    #[test]
    fn a_read_only_view_answers_kern_invalid_right_to_a_writable_caller() {
        let size = round_up(page(), page()).expect("a page size");
        let writable = make(Make::Fresh {
            size,
            protection: 3,
        })
        .expect("minted");
        // The browser's own read-write mapping, which it keeps.
        let mine = ask(writable, size, 3).expect("a writable entry maps read-write");
        // `ConvertToReadOnly`: a read-only mapping of the same entry.
        let theirs = ask(writable, size, 1).expect("the same entry maps read-only");
        let read_only = make(Make::Over {
            address: theirs,
            size,
            protection: 1,
        })
        .expect("an entry over the read-only mapping just made");

        assert_eq!(
            ask(writable, size, 3).map(|_| KERN_SUCCESS),
            Ok(KERN_SUCCESS),
            "a writable entry still answers KERN_SUCCESS to a read-write probe"
        );
        assert_eq!(
            ask(read_only, size, 3),
            Err(KERN_INVALID_RIGHT),
            "and a read-only view answers KERN_INVALID_RIGHT, which is the code the guest reads"
        );
        // The read-only view is the *same* file, so it sees what the writable
        // mapping wrote - which is the whole point of a view rather than a copy,
        // and the thing a `memfd`-per-entry answer would get wrong.
        let through_view = ask(read_only, size, 1).expect("the read-only view maps");
        // SAFETY: three whole mappings of `size` bytes this test made, and every
        // buffer below is inside one of them.
        unsafe {
            let marker = b"one file, two protections";
            std::ptr::copy_nonoverlapping(marker.as_ptr(), mine as *mut u8, marker.len());
            let mut seen = [0_u8; 10];
            std::ptr::copy_nonoverlapping((through_view as *const u8).add(4), seen.as_mut_ptr(), 10);
            assert_eq!(&seen, b"file, two ");
            // And the ceiling is a promise about the pages, not about the
            // mapping: `vm_protect` must not re-open it. `mach::vm` checks it,
            // and this is that check from the outside.
            assert_eq!(
                crate::mach::vm::vm_protect(TASK_SELF, through_view, size, 3, 3),
                KERN_PROTECTION_FAILURE,
                "a read-only entry's pages cannot be re-opened for writing"
            );
            assert_eq!(libc::munmap(mine as *mut c_void, size as usize), 0);
            assert_eq!(libc::munmap(theirs as *mut c_void, size as usize), 0);
            assert_eq!(libc::munmap(through_view as *mut c_void, size as usize), 0);
        }
        forget(mine, size);
        forget(theirs, size);
        forget(through_view, size);
    }

    /// A port nothing holds is `KERN_INVALID_NAME` and **not**
    /// `KERN_INVALID_RIGHT`: the guest reads the latter as "this region is
    /// read-only", so an unresolvable name must never be reported as it.
    #[test]
    fn a_name_nothing_holds_is_not_reported_as_a_read_only_region() {
        let size = round_up(page(), page()).expect("a page size");
        assert_ne!(ask(STRANGER, size, 3), Err(KERN_INVALID_RIGHT));
    }

    /// **The second half, in a second process.** Everything else in this file
    /// can be satisfied by one process mapping its own file, and that is the
    /// half that was never in doubt; what was in doubt is whether a *different*
    /// process can resolve a port it was sent and map the pages behind it. So
    /// this is a real second process - this test binary, re-executed with a
    /// brief in its environment - and the brief is **only the port's
    /// `Address`**, which is the one and only thing `super::msg` puts in a
    /// message for a send right. No name, no mapping, no port table, no
    /// descriptor, no path.
    ///
    /// The child then does what `mach/msg.rs`'s `Reference::adopt` does on
    /// receipt - draws its own name in its own space for that `Address` - and
    /// maps the entry, and reads what this process wrote.
    #[test]
    fn another_process_resolves_a_port_it_was_sent_and_maps_the_same_pages() {
        /// The child role's brief. Deliberately the only thing passed, so that a
        /// future refactor which needs more than the address fails here rather
        /// than working by accident on the parent's state.
        const BRIEF: &str = "ROSETTA3_MEMOBJ_CHILD";
        /// This test's own name, so the child runs this test and no other.
        const SELF: &str =
            "mach::memory_object::tests::another_process_resolves_a_port_it_was_sent_and_maps_the_same_pages";
        const MARKER: &str = "written in the parent";

        if let Some(brief) = std::env::var_os(BRIEF) {
            // ---- the child role -----------------------------------------------
            let brief = brief.to_str().expect("a UTF-8 brief").to_owned();
            let (address, expected) = brief.split_once(':').expect("address:marker");
            let address = u64::from_str_radix(address, 16).expect("a hex address");
            // Exactly what `Reference::adopt` does for a `MOVE_SEND`: a **fresh
            // name in this process's own space** for the same `Address`. The name
            // the minting process drew is not here to be found, and nothing above
            // this line had one.
            let mine = space::insert_send(address);
            let size = round_up(page(), page()).expect("a page size");
            let got = ask(mine, size, 3).expect("an entry this process never minted still maps");
            // SAFETY: a whole mapping of `size` bytes the call above made.
            let seen = unsafe { std::slice::from_raw_parts(got as *const u8, expected.len()) };
            assert_eq!(seen, expected.as_bytes(), "the parent's write, in this process");
            // And the ceiling travelled with it, so a read-only entry is still
            // read-only on the far side.
            assert_eq!(
                ceiling_of(got),
                Some(3),
                "the entry's maximum protection is part of what the port stands for"
            );
            // SAFETY: the mapping the call above made.
            assert_eq!(unsafe { libc::munmap(got as *mut c_void, size as usize) }, 0);
            return;
        }

        // ---- the parent role -------------------------------------------------
        let size = round_up(page(), page()).expect("a page size");
        let port = make(Make::Fresh {
            size,
            protection: 3,
        })
        .expect("minted");
        let address = space::address_of(port).expect("a send right has an address");
        let mine = ask(port, size, 3).expect("mapped here");
        // SAFETY: a whole mapping of `size` bytes the call above made.
        unsafe {
            let marker = MARKER.as_bytes();
            std::ptr::copy_nonoverlapping(marker.as_ptr(), mine as *mut u8, marker.len());
        }

        let status = std::process::Command::new(std::env::current_exe().expect("this binary"))
            .args(["--exact", SELF, "--test-threads=1", "--nocapture"])
            .env(BRIEF, format!("{address:016x}:{MARKER}"))
            .status()
            .expect("this test binary, re-executed");
        // SAFETY: a whole mapping of `size` bytes the call above made.
        unsafe { assert_eq!(libc::munmap(mine as *mut c_void, size as usize), 0) };
        assert!(
            status.success(),
            "a process that was sent the port and given only its address could not map the \
             pages behind it; the child's own output above is the reason"
        );
    }

    /// ## The second half, through a real `mach_msg` into a real second process
    ///
    /// The test above proves the resolution and proves the process boundary, but
    /// it *simulates* the receipt: it calls `space::insert_send(address)`
    /// itself, which is what [`super::msg`]'s `Reference::adopt` does. So it
    /// leaves one link out of the chain, and the link is the one the guest
    /// actually walks: a port put in a message by one process, sent over a
    /// socket, received and adopted by another. `mach::msg`'s own tests cover a
    /// send right crossing a real socket and say so - *"the shape of
    /// Crashpad's handshake, minus the process boundary"* - so between them
    /// every link had a witness and the chain had none.
    ///
    /// This walks the chain. It is the guest's own shape: Chromium sends a
    /// `base::SharedMemoryHandle` as a `MACH_MSG_TYPE_MOVE_SEND` port, so the
    /// entry's port goes in the message's **local port** field with `MOVE_SEND`
    /// as its disposition, and the receiver reads it back out of
    /// `msgh_remote_port`. The only thing the child is given out of band is the
    /// **bootstrap port** - it binds a receive right and prints its `Address`,
    /// which is what `bootstrap_check_in`/`bootstrap_look_up` do in a directory
    /// rather than a pipe, and which is the one rendezvous XPC itself needs.
    /// The entry's address is never mentioned to the child: the only channel it
    /// arrives on is the message.
    ///
    /// The wire constants are XNU's, restated rather than imported, because
    /// `mach::msg` keeps its own private: `MACH_MSGH_BITS(x, y)` from
    /// `<mach/message.h>` (mask `0x1f`, remote shift 0, local shift 8),
    /// `MACH_SEND_MSG` and `MACH_RCV_MSG|MACH_RCV_TIMEOUT` from
    /// `mach_msg_option_t`, and `sizeof(mach_msg_header_t) = 24`, which
    /// `mach::msg` establishes from the guest's own compiled stores.
    #[test]
    fn a_port_carried_in_a_real_message_maps_the_same_pages_in_another_process() {
        /// The child role's brief, and the line it prints its bootstrap port on.
        const BRIEF: &str = "ROSETTA3_MEMOBJ_IPC";
        const SAYS: &str = "MEMOBJ_BOOTSTRAP";        const SELF: &str =
            "mach::memory_object::tests::a_port_carried_in_a_real_message_maps_the_same_pages_in_another_process";
        const MARKER: &[u8] = b"sent in a message, mapped in another process";

        const HEADER: usize = 24;
        const LOCAL_SHIFT: u32 = 8;
        const BITS_MASK: u32 = 0x1f;
        const MACH_SEND_MSG: i32 = 0x0000_0001;
        const MACH_RCV_MSG: i32 = 0x0000_0002;
        const MACH_RCV_TIMEOUT: i32 = 0x0000_0100;
        const TIMEOUT_MS: u32 = 20_000;

        if let Some(brief) = std::env::var_os(BRIEF) {
            // ---- the child role -----------------------------------------------
            // The brief is a rendezvous directory and nothing else: where to
            // publish the bootstrap port. The memory object's address is not in
            // it, and never is.
            let brief = PathBuf::from(brief);
            let socket = super::super::wire::Receiver::bind().expect("a Mach port to bind");
            let bootstrap = space::insert_receive(socket);
            let address = space::address_of(bootstrap).expect("a receive right has an address");
            fs::write(brief.join(SAYS), format!("{address:016x}"))
                .expect("the parent is polling for this file");

            let mut buf = vec![0u8; 256];
            // SAFETY: 256 writable bytes for the message and its trailer.
            let rc = unsafe {
                super::super::msg::mach_msg(
                    buf.as_mut_ptr().cast(),
                    MACH_RCV_MSG | MACH_RCV_TIMEOUT,
                    0,
                    buf.len() as u32,
                    bootstrap,
                    TIMEOUT_MS,
                    MACH_PORT_NULL,
                )
            };
            assert_eq!(rc, MACH_MSG_SUCCESS, "the parent's message arrived");
            // The port came back in the message, not from anywhere else.
            let carried = u32::from_le_bytes(buf[8..12].try_into().expect("msgh_remote_port"));
            assert_ne!(carried, MACH_PORT_NULL, "a MOVE_SEND arrives as a name");
            assert!(
                space::address_of(carried).is_some(),
                "and as a right this process now holds, drawn in its own space"
            );
            assert_ne!(
                carried, bootstrap,
                "a name this process drew for itself is not the one it was sent"
            );
            let size = round_up(page(), page()).expect("a page size");
            let got = ask(carried, size, 3)
                .expect("an entry that arrived in a message still maps in the receiving process");
            // SAFETY: a whole mapping of `size` bytes the call above made, and
            // the parent wrote a four-byte length there before the send.
            let view = unsafe { std::slice::from_raw_parts(got as *const u8, size as usize) };
            let len = u32::from_le_bytes(view[..4].try_into().expect("the length prefix")) as usize;
            assert_eq!(
                &view[4..4 + len],
                MARKER,
                "the parent's write, read through a port that crossed a socket in a real message"
            );
            // SAFETY: the mapping the call above made.
            assert_eq!(unsafe { libc::munmap(got as *mut c_void, size as usize) }, 0);
            return;
        }

        // ---- the parent role -------------------------------------------------
        let size = round_up(page(), page()).expect("a page size");
        let entry = make(Make::Fresh {
            size,
            protection: 3,
        })
        .expect("minted");
        let mine = ask(entry, size, 3).expect("mapped here");
        // SAFETY: a whole mapping of `size` bytes this test made, and the two
        // writes below are inside it.
        unsafe {
            let len = MARKER.len() as u32;
            std::ptr::copy_nonoverlapping(len.to_le_bytes().as_ptr(), mine as *mut u8, 4);
            std::ptr::copy_nonoverlapping(MARKER.as_ptr(), (mine as *mut u8).add(4), MARKER.len());
        }

        // A rendezvous for the bootstrap port, in this module's own namespace -
        // the same shape as `mach::bootstrap`, and the only thing the child is
        // told that is not a page.
        let rendezvous = namespace()
            .expect("a namespace to rendezvous in")
            .join(format!("ipc-{}-{}", unsafe { libc::getpid() }, SAYS));
        let _ = fs::remove_dir_all(&rendezvous);
        fs::create_dir_all(&rendezvous).expect("a rendezvous directory");
        let mut child = std::process::Command::new(std::env::current_exe().expect("this binary"))
            .args(["--exact", SELF, "--test-threads=1", "--nocapture"])
            .env(BRIEF, &rendezvous)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("this test binary, re-executed");
        // Wait for the child to publish it. Bounded, so a child that died before
        // publishing is a failure here rather than a hang.
        let mut bootstrap = None;
        for _ in 0..400 {
            if let Ok(hex) = fs::read_to_string(rendezvous.join(SAYS))
                && let Ok(address) = u64::from_str_radix(hex.trim(), 16)
            {
                bootstrap = Some(address);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let bootstrap = bootstrap.unwrap_or_else(|| {
            let _ = child.wait();
            let _ = fs::remove_dir_all(&rendezvous);
            panic!("the child never published its bootstrap port");
        });
        let _ = fs::remove_dir_all(&rendezvous);
        // The parent's name for the child's port, which is a send right for an
        // address it has never held a receive right for - the shape a
        // `bootstrap_look_up` answer is.
        let to_child = space::insert_send(bootstrap);

        // A `mach_msg_header_t` with the entry in `msgh_local_port` and
        // `MACH_MSG_TYPE_MOVE_SEND` as the local disposition: Chromium's
        // `MACH_MSGH_BITS(MACH_MSG_TYPE_MOVE_SEND, 0)`.
        let bits = (super::super::msg::MOVE_SEND & BITS_MASK) << LOCAL_SHIFT;
        let size_of = (HEADER + 4) as u32;
        let mut sent = Vec::with_capacity(size_of as usize);
        sent.extend_from_slice(&bits.to_le_bytes());
        sent.extend_from_slice(&size_of.to_le_bytes());
        sent.extend_from_slice(&to_child.to_le_bytes());
        sent.extend_from_slice(&entry.to_le_bytes());
        sent.extend_from_slice(&MACH_PORT_NULL.to_le_bytes());
        sent.extend_from_slice(&0x5a5au32.to_le_bytes());
        sent.extend_from_slice(&(MARKER.len() as u32).to_le_bytes());
        // SAFETY: a whole message of the size in its header.
        let rc = unsafe {
            super::super::msg::mach_msg(
                sent.as_mut_ptr().cast(),
                MACH_SEND_MSG,
                size_of,
                0,
                MACH_PORT_NULL,
                0,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_MSG_SUCCESS, "the entry's port went into a message");

        let status = child.wait().expect("the child to finish");
        // SAFETY: a whole mapping of `size` bytes this test made.
        unsafe { assert_eq!(libc::munmap(mine as *mut c_void, size as usize), 0) };
        assert!(
            status.success(),
            "a process that received the port in a real message could not map the pages behind \
             it; the child's own output above is the reason"
        );
    }

    /// The shapes this tree does not implement are refused rather than ignored: a
    /// request silently answered as a different request is the failure mode this
    /// whole module exists to avoid.
    #[test]
    fn the_shapes_that_are_not_implemented_are_refused_rather_than_ignored() {
        let size = round_up(page(), page()).expect("a page size");
        let port = make(Make::Fresh {
            size,
            protection: 3,
        })
        .expect("minted");
        let at = |offset, file_offset| {
            map(Request {
                object: port,
                offset,
                file_offset,
                size,
                protection: 3,
                max_protection: 3,
                anywhere: true,
                hint: 0,
                page: page(),
                symbol: "test",
            })
        };
        assert_eq!(at(0x1000, 0), Err(KERN_INVALID_ARGUMENT), "offset");
        assert_eq!(at(0, 1), Err(KERN_INVALID_ARGUMENT), "unaligned file_offset");
        assert_eq!(at(0, size), Err(KERN_INVALID_ARGUMENT), "past the end");
    }

    /// An address this process never mapped from a memory entry has no pages
    /// behind it that this tree can name, so the `Over` shape is refused there
    /// rather than answered with a fresh object.
    #[test]
    fn an_address_no_memory_entry_was_mapped_at_has_nothing_behind_it() {
        let size = round_up(page(), page()).expect("a page size");
        let mut out = MACH_PORT_NULL;
        let mut asked = size;
        // SAFETY: two live locals for the out-parameters, and a host pointer the
        // call cannot map over - the address is outside every mapping.
        let status = unsafe {
            mach_make_memory_entry_64(
                TASK_SELF,
                &mut asked,
                0x7009_0000_0000,
                1,
                &mut out,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(status, KERN_INVALID_ADDRESS);
        assert_eq!(out, MACH_PORT_NULL, "a refusal writes no port");
    }
}
