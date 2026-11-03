use crate::syscall::ScopedHostSyscallContext;
use libffi::middle::{Arg, Cif, CodePtr, Type, arg};

/// Call the host function at `addr` with word-sized integer arguments, without
/// system call interposition getting in the way.
///
/// # Safety
///
/// `addr` must be a host function taking `args.len()` word-sized integer
/// arguments and returning a word-sized integer (or nothing).
pub unsafe fn call_host(addr: usize, args: &[usize]) -> usize {
    let _savior = ScopedHostSyscallContext::new();
    let cif = Cif::new(args.iter().map(|_| Type::usize()), Type::usize());
    let ffi_args: Vec<Arg> = args.iter().map(arg).collect();
    unsafe { cif.call(CodePtr(addr as *mut _), &ffi_args) }
}

/// The native backend's entry point; the emulated backend reads the guest's
/// variadic tail itself and calls [`call_host`].
///
/// FIXME: Assumes the VaList ABI matches the host, NOT safe for aarch64!
///
/// # Safety
///
/// Highly dangerous. Heavily review all code using this function.
unsafe extern "C" fn rosemary_host_call_trampoline(
    addr: usize,
    argcount: usize,
    mut args: ...
) -> usize {
    if !cfg!(target_arch = "x86_64") && !cfg!(target_os = "macos") {
        panic!("This function is not safe to call on your platform due to VaList ABI differences.")
    }

    let copied_args: Vec<usize> = (0..argcount)
        .map(|_| unsafe { args.next_arg::<usize>() })
        .collect();
    unsafe { call_host(addr, &copied_args) }
}

exports::exports! {
    functions {
        "rosemary_host_call_trampoline" => rosemary_host_call_trampoline,
    }
}
