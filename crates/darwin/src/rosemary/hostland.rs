use crate::syscall::ScopedHostSyscallContext;
use std::ffi::{c_char, c_void};

unsafe extern "C" fn host_dlopen(filename: *const c_char, flags: i32) -> *mut c_void {
    let _savior = ScopedHostSyscallContext::new();
    unsafe { libc::dlopen(filename, flags) }
}

unsafe extern "C" fn host_dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void {
    let _savior = ScopedHostSyscallContext::new();
    unsafe { libc::dlsym(handle, symbol) }
}

unsafe extern "C" fn host_dlclose(handle: *mut c_void) -> i32 {
    let _savior = ScopedHostSyscallContext::new();
    unsafe { libc::dlclose(handle) }
}

unsafe extern "C" fn get_host_rtld_default() -> *mut c_void {
    libc::RTLD_DEFAULT
}

exports::exports! {
    functions  {
        "rosemary_host_dlopen" => host_dlopen,
        "rosemary_host_dlsym" => host_dlsym,
        "rosemary_host_dlclose" => host_dlclose,
        "rosemary_get_host_rtld_default" => get_host_rtld_default,
    }
}
