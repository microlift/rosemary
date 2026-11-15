use std::ffi::VaList;

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

crate::variadic! {
    /// `usize rosemary_host_call_trampoline(usize addr, usize argcount, ...)`, the
    /// native backend's entry point; the emulated backend reads the guest's
    /// variadic tail itself and calls [`call_host`].
    ///
    /// # Safety
    ///
    /// Highly dangerous. Heavily review all code using this function.
    fn rosemary_host_call_trampoline(addr: usize, argcount: usize) -> usize => host_call_list;
}

/// [`rosemary_host_call_trampoline`]'s `argcount` words, as a `va_list`.
unsafe extern "C" fn host_call_list(addr: usize, argcount: usize, mut args: VaList) -> usize {
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
