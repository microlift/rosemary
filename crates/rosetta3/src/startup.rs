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

/// Run initializers and then the entry point, returning `main`'s status.
pub fn run(exec: &dyn Execution) -> Result<i32, VmError> {
    // Before anything the program can fault in, which is everything below. The
    // image map goes with it.
    crate::fault::install();
    crate::fault::note_images(exec.report());
    // So that a block entered on a thread *this runtime* started - a dispatch
    // pool worker, which `darwin` spawns and which `DarwinThread::enter` gives
    // a Darwin thread pointer but does not register - has guest thread state
    // before its guest code runs. Without it `thread::current()` is `None` there,
    // and guest code reached that way runs unadopted: no indirect-call cache,
    // and nothing keyed on the current thread to answer from. Set before any
    // framework can post a block. See `darwin::dispatch::block`.
    darwin::dispatch::block::set_thread_adopt_hook(Some(|| {
        // Read *before* adopting, because after it the answer is always "yes" and
        // the diagnostic would report the opposite of what it claims to measure.
        let was_untracked = crate::thread::current().is_none();
        // Idempotent, and it allocates a guest stack on first use, which is
        // what any thread entering guest code needs.
        let _ = crate::thread::ensure();
        // One line, so a run that shows the fault unmoved can be distinguished
        // from a run where the hook never fired. Validating the instrument
        // before reading its negative is the whole lesson of this investigation.
        crate::fault::note_block_adopted(was_untracked);
    }));
    // Before any framework can ask a question about the main executable and get
    // an answer about this binary rather than the program's. See [`crate::compat`].
    #[cfg(target_os = "macos")]
    crate::compat::install();
    // Before the program's own code, because `SteamAPI_Init` asks the name
    // server for Steam and a service published after the question is not there.
    crate::steam::install();
    say_image_map(exec);
    // Whatever this backend must do before any of the program's own code runs.
    exec.prepare()?;
    // Everything above is this runtime's own start-up, and a host library that
    // returns successfully is still entitled to leave `errno` wherever it likes:
    // the loader leaves `EPERM` here and `NSUserDefaults` leaves `EINVAL`. A real
    // process reaches its first initializer with a clean one, so the guest gets
    // a clean one - after this line every write to it is the guest's own.
    crate::thread::clear_errno();
    run_initializers(exec)?;
    let status = exec.enter_main()?;
    Ok((status & 0xff) as i32)
}

/// Where each image ended up, under `ROSETTA3_TRACE_VM`.
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
                "rosetta3: image {:#x}..{:#x} {} {} (slide {:#x})",
                seg.vmaddr,
                seg.vmaddr.wrapping_add(seg.vmsize),
                seg.name,
                image.name.rsplit('/').next().unwrap_or(&image.name),
                image.slide,
            );
        }
    }
}
