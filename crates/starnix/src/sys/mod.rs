//! One backend per host, each answering the same set of functions.
//!
//! This is the only place in the crate that branches on the host.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "freebsd")]
mod freebsd;
#[cfg(target_os = "freebsd")]
pub use freebsd::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(any(target_os = "freebsd", target_os = "macos"))]
mod sysctl;

#[cfg(any(target_os = "freebsd", target_os = "macos"))]
mod ifmedia;
#[cfg(any(target_os = "freebsd", target_os = "macos"))]
pub use ifmedia::link_medium;

#[cfg(target_os = "linux")]
mod procmap;

#[cfg_attr(not(target_os = "freebsd"), allow(dead_code))]
mod kinfo;

#[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "macos")))]
mod other;
#[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "macos")))]
pub use other::*;
