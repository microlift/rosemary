// glibc only: finding the dynamic loader's text needs `dl_iterate_phdr`, and
// musl has no way to name its loader.
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
mod linux;
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
pub use linux::*;
#[cfg(not(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")))]
mod other;
#[cfg(not(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")))]
pub use other::*;
