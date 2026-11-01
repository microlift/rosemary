use crate::syscall::ScopedHostSyscallContext;
use libffi::middle::{Arg, Cif, Type, arg};

/// Call a host function pointer without system call interposition
/// getting in the way.
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
    let _savior = ScopedHostSyscallContext::new();
    if !cfg!(target_arch = "x86_64") && !cfg!(target_os = "macos") {
        panic!("This function is not safe to call on your platform due to VaList ABI differences.")
    }

    let mut copied_args: Vec<usize> = Vec::new();
    let mut cif_typed_args: Vec<(Type, Arg)> = Vec::new();
    cif_typed_args.reserve(argcount);
    for _ in 0..argcount {
        let arg_value: usize = unsafe { args.next_arg::<usize>() };
        copied_args.push(arg_value);
    }
    for copied in &copied_args {
        cif_typed_args.push((Type::usize(), arg(copied)));
    }
    let ffi_ret_type = Type::usize();
    let function_context = Cif::new(cif_typed_args.iter().cloned().map(|x| x.0), ffi_ret_type);
    unsafe {
        let cif_args: Vec<Arg> = cif_typed_args.iter().cloned().map(|x| x.1).collect();
        function_context.call(std::mem::transmute(addr), &cif_args)
    }
}

exports::exports! {
    functions {
        "rosemary_host_call_trampoline" => rosemary_host_call_trampoline,
    }
}
