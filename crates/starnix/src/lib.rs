//! What the host kernel says, in host-neutral terms.
//!
//! Linux keeps this in `/proc` and `/sys`, FreeBSD behind `sysctl(KERN_PROC)`,
//! macOS behind `libproc` and Mach. Everything above this crate asks the same
//! questions and gets the same types back; the per-host spelling lives in
//! [`sys`] and nowhere else.
//!
//! The rule every backend keeps: never invent a measurement. A host that cannot
//! say something leaves it zero or `None`, and one that cannot see a process at
//! all answers `None` for it.
//!
//! * [`process`] - another process's record, usage and threads, and this
//!   process's own descriptors, mappings and audit ids.
//! * [`system`] - the machine: memory, CPU ticks, cores, the CPU's name.
//! * [`socket`] - who is on the other end of a Unix socket.
//! * [`net`] - network interfaces, their link types, and the route out.
//! * [`errno`] - this thread's `errno` slot.
//! * [`xattr`] - extended attributes by descriptor or path.
//! * [`fd`] - a path that reaches what a descriptor has open.
//! * [`signal`] - what the kernel hands a signal handler.

pub mod errno;
pub mod fd;
pub mod net;
pub mod process;
pub mod signal;
pub mod socket;
pub mod system;
pub mod xattr;

mod sys;
