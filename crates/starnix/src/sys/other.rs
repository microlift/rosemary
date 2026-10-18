//! Any other Unix. A process is known to exist and nothing more, which is
//! enough for a process list to be right and every measurement to read zero.

pub mod signal;

use std::io;
use std::os::fd::RawFd;
use std::path::PathBuf;

use crate::process::{Audit, Descriptor, Identity, Process, Region, Thread, Usage};
use crate::socket::Peer;
use crate::system::{CpuTicks, Memory};

fn exists(pid: i32) -> bool {
    // SAFETY: signal zero only checks that the pid can be signalled.
    unsafe { libc::kill(pid, 0) == 0 }
    || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn unsupported() -> io::Error {
    io::Error::from(io::ErrorKind::Unsupported)
}

pub fn identity(pid: i32) -> Option<Identity> {
    exists(pid).then(Identity::default)
}

pub fn process(pid: i32) -> Option<Process> {
    exists(pid).then(Process::default)
}

pub fn usage(pid: i32) -> Option<Usage> {
    exists(pid).then(Usage::default)
}

pub fn threads(pid: i32) -> Option<Vec<Thread>> {
    exists(pid).then(Vec::new)
}

pub fn has_thread(_tid: i32) -> bool {
    false
}

pub fn command_line(_pid: i32) -> Option<Vec<u8>> {
    None
}

pub fn descriptors() -> io::Result<Vec<Descriptor>> {
    Err(unsupported())
}

pub fn regions() -> io::Result<Vec<Region>> {
    Err(unsupported())
}

pub fn audit() -> Audit {
    Audit::default()
}

pub fn memory() -> Memory {
    Memory::default()
}

pub fn cpu_ticks() -> CpuTicks {
    CpuTicks::default()
}

pub fn physical_cores() -> Option<u32> {
    None
}

pub fn cpu_brand() -> Option<String> {
    None
}

pub fn peer(fd: RawFd) -> Option<Peer> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: two writable ids.
    let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
    (rc == 0).then_some(Peer { pid: None, uid })
}

pub fn in_use(_pid: i32) -> Option<crate::process::InUse> {
    None
}

pub fn fd_path(_fd: RawFd) -> Option<PathBuf> {
    None
}

pub fn fd_target(fd: RawFd) -> Option<PathBuf> {
    fd_path(fd)
}

pub fn errno_location() -> *mut libc::c_int {
    // SAFETY: takes no argument and returns this thread's own slot.
    unsafe { libc::__error() }
}

pub fn xattr_get_fd(_: RawFd, _: &std::ffi::CStr, _: &mut [u8]) -> io::Result<usize> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn xattr_get_path(
    _: &std::ffi::CStr,
    _: &std::ffi::CStr,
    _: &mut [u8],
    _: bool,
) -> io::Result<usize> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn xattr_set_fd(_: RawFd, _: &std::ffi::CStr, _: &[u8]) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn xattr_set_path(_: &std::ffi::CStr, _: &std::ffi::CStr, _: &[u8], _: bool) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn xattr_remove_fd(_: RawFd, _: &std::ffi::CStr) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn xattr_remove_path(_: &std::ffi::CStr, _: &std::ffi::CStr, _: bool) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn xattr_is_missing(_: &io::Error) -> bool {
    false
}

pub fn link_medium(_name: &std::ffi::CStr) -> Option<crate::net::Link> {
    None
}
