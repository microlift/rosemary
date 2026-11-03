use crate::exec::Execution;
use crate::vm::VmError;

/// Run every initializer, in order.
pub fn run_initializers(exec: &dyn Execution) -> Result<usize, VmError> {
    let (argc, argv, envp, apple) = exec.main_args();
    let mut ran = 0;
    // Reverse load order: see the module note.
    let initializers: Vec<u64> = exec
        .report()
        .images
        .iter()
        .rev()
        .flat_map(|image| image.initializers.iter().copied())
        .collect();
    for init in initializers {
        exec.call(init, &[argc, argv, envp, apple])?;
        ran += 1;
    }
    Ok(ran)
}

/// Store dyld's two entry points into each image's `__DATA,__dyld` section, as
/// the loader did before the program's first instruction; `crt1` of that era
/// calls through them.
fn fill_dyld_sections(exec: &dyn Execution) {
    use darwin::bsd::{dyld_func_lookup, dyld_lazy_bind_helper};
    for addr in exec.report().images.iter().filter_map(|i| i.dyld_data) {
        let slot = addr as usize as *mut u64;
        // SAFETY: the section is part of a mapped, writable `__DATA` segment of
        // this process, and nothing of the guest has run yet.
        unsafe {
            slot.write_unaligned(dyld_lazy_bind_helper as *const () as usize as u64);
            slot.add(1)
                .write_unaligned(dyld_func_lookup as *const () as usize as u64);
        }
    }
}

/// Run initializers and then the entry point, returning `main`'s status.
pub fn run(exec: &dyn Execution) -> Result<i32, VmError> {
    // Before anything the program can fault in, which is everything below. The
    // image map goes with it.
    crate::fault::install();
    crate::fault::note_images(exec.report());
    // Before any framework can ask a question about the main executable and get
    // an answer about this binary rather than the program's. See [`crate::compat`].
    #[cfg(target_os = "macos")]
    crate::compat::install();
    // Before the program's own code, because `SteamAPI_Init` asks the name
    // server for Steam and a service published after the question is not there.
    crate::steam::install();
    // Before the first initializer, which may read it: QtCore's do.
    #[cfg(not(target_os = "macos"))]
    if let Err(e) = darwin::commpage::install() {
        eprintln!("rosemary: no comm page: {e}");
    }
    say_image_map(exec);
    // Whatever this backend must do before any of the program's own code runs.
    exec.prepare()?;
    // Everything above is this runtime's own start-up, and a host library that
    // returns successfully is still entitled to leave `errno` wherever it likes:
    // the loader leaves `EPERM` here and `NSUserDefaults` leaves `EINVAL`. A real
    // process reaches its first initializer with a clean one, so the guest gets
    // a clean one - after this line every write to it is the guest's own.
    crate::thread::clear_errno();
    fill_dyld_sections(exec);
    run_initializers(exec)?;
    let status = exec.enter_main()?;
    // `exit(main(...))`, as dyld's start does: the main thread's thread-local
    // terminators run before the process's `atexit` handlers.
    crate::tls::exit();
    Ok((status & 0xff) as i32)
}

/// Where each image ended up, under `ROSEMARY_TRACE_VM`.
///
/// Every guest address this runtime reports is slid by whatever `mmap` chose that run,
/// and a fault inside guest code is reported by a debugger - or by the guest's own
/// crash handler - as a bare number.
fn say_image_map(exec: &dyn Execution) {
    if !crate::shims::vmmap::tracing() {
        return;
    }
    for image in &exec.report().images {
        for seg in &image.segments {
            if seg.vmsize == 0 {
                continue;
            }
            eprintln!(
                "rosemary: image {:#x}..{:#x} {} {} (slide {:#x})",
                seg.vmaddr,
                seg.vmaddr.wrapping_add(seg.vmsize),
                seg.name,
                image.name.rsplit('/').next().unwrap_or(&image.name),
                image.slide,
            );
        }
    }
}
