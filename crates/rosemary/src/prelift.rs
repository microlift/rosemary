//! Lifts done ahead of first use, on worker threads.
//!
//! Realizing a guest's classes hands out an `IMP` for every method, and lifting
//! each one there, serially and before `main`, is most of a large program's start-up.
//! The lift only has to be done before the method's first call, and one that is not
//! done by then is done by the caller as any other miss is.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

use crate::vm::Vm;

/// Workers never outnumber this, however many CPUs there are: they compete with
/// the guest's own start-up.
const MAX_WORKERS: usize = 4;

struct Queue {
    pending: VecDeque<u64>,
    workers: usize,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue {
    pending: VecDeque::new(),
    workers: 0,
});
static READY: Condvar = Condvar::new();

/// How many workers to run: one CPU is left to the thread realizing classes.
fn wanted() -> usize {
    let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    cpus.saturating_sub(1).clamp(1, MAX_WORKERS)
}

/// Lift the region at `va` of `vm`'s program on a worker.
pub fn queue(vm: &'static Vm, va: u64) {
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    queue.pending.push_back(va);
    if queue.workers < wanted() {
        let spawned = std::thread::Builder::new()
            .name("rosemary-prelift".into())
            .spawn(move || work(vm));
        match spawned {
            Ok(_) => queue.workers += 1,
            // Not having a worker only means the lift happens at first call.
            Err(_) if queue.workers > 0 => {}
            Err(_) => {
                queue.pending.pop_back();
                drop(queue);
                vm.lift_now(va);
                return;
            }
        }
    }
    drop(queue);
    READY.notify_one();
}

fn work(vm: &'static Vm) {
    loop {
        let va = {
            let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(va) = queue.pending.pop_front() {
                    break va;
                }
                queue = READY.wait(queue).unwrap_or_else(|e| e.into_inner());
            }
        };
        vm.lift_now(va);
    }
}
