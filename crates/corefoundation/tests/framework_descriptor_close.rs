#![cfg(target_os = "linux")]

use std::ffi::c_void;

#[test]
fn cfsocket_invalidation_clears_darwin_socket_descriptor_bookkeeping() {
    type Create = unsafe extern "C" fn(
        *const c_void,
        i32,
        u64,
        *const c_void,
        *const c_void,
    ) -> *const c_void;
    type Invalidate = unsafe extern "C" fn(*const c_void);
    type SetSockOpt = unsafe extern "C" fn(i32, i32, i32, *const c_void, u32) -> i32;
    let create: Create = unsafe {
        std::mem::transmute(
            corefoundation::symbol("CFSocketCreateWithNative").expect("CFSocketCreateWithNative"),
        )
    };
    let invalidate: Invalidate = unsafe {
        std::mem::transmute(
            corefoundation::symbol("CFSocketInvalidate").expect("CFSocketInvalidate"),
        )
    };
    let shim: SetSockOpt =
        unsafe { std::mem::transmute(darwin::symbol("_setsockopt").expect("Darwin setsockopt")) };
    let mut fds = [-1; 2];
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) },
        0
    );
    let enabled = 1i32;
    assert_eq!(
        unsafe { shim(fds[0], 0xffff, 0x1022, (&raw const enabled).cast(), 4) },
        0
    );
    assert!(darwin::net::sockopt::suppresses_sigpipe(fds[0]));
    let socket = unsafe {
        create(
            std::ptr::null(),
            fds[0],
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert!(!socket.is_null());
    unsafe { invalidate(socket) };
    assert_eq!(unsafe { libc::fcntl(fds[0], libc::F_GETFD) }, -1);
    assert!(!darwin::net::sockopt::suppresses_sigpipe(fds[0]));
    assert_eq!(unsafe { libc::close(fds[1]) }, 0);
}
