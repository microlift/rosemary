//! Lifts done ahead of first use, on worker threads.
//!
//! Realizing a guest's classes hands out an `IMP` for every method, and lifting
//! each one there, serially and before `main`, is most of a large program's start-up.
//! The lift only has to be done before the method's first call, and one that is not
//! done by then is done by the caller as any other miss is.
//!
//! The same workers lift ahead along a program's static control flow: when a region
//! is translated, the constant targets it can branch or call to are queued, so that
//! a thread running cold code finds its next region compiled or already compiling
//! instead of compiling every one itself, one after another. Each hop away from code
//! that actually ran is a level further back in the queue, and past
//! [`ahead_depth`] hops nothing is queued at all.
//!
//! Last come upgrades: under [`crate::jit::tiered`] compilation every region is first
//! compiled cheaply, and replaced here by code compiled at the level the run asked
//! for - regions that ran before regions only lifted ahead. Oldest first, so code that
//! ran early and is still running is not starved by the stream of new code a busy
//! start-up compiles.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

use static_init::dynamic;

use crate::vm::Vm;

/// Workers never outnumber this, however many CPUs there are: they compete with
/// the guest's own start-up.
const MAX_WORKERS: usize = 4;

/// Workers upgrading at once never outnumber this. An upgrade compiles at the run's
/// full level and takes several times as long as a lift, so with every worker on one
/// the lift a thread is about to need waits behind them.
const MAX_UPGRADERS: usize = 2;

/// How many hops past code that ran a lift ahead may go, unless
/// `ROSEMARY_LIFT_AHEAD` says otherwise. `0` turns lifting ahead off.
const DEFAULT_AHEAD_DEPTH: usize = 4;

/// Queued addresses past this many at one depth push out the oldest: an address
/// queued long ago belongs to code the guest has most likely left behind.
const MAX_QUEUED_PER_DEPTH: usize = 4096;

pub const AHEAD_ENV: &str = "ROSEMARY_LIFT_AHEAD";

/// How many hops past code that ran a lift ahead may go.
pub fn ahead_depth() -> usize {
    #[dynamic]
    static DEPTH: usize = std::env::var(AHEAD_ENV)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_AHEAD_DEPTH);
    *DEPTH
}

struct Queue {
    /// Addresses by how far they are from code that ran: index 0 holds the ones a
    /// caller asked for outright, taken in the order asked, since that is the order
    /// they will run in. Every other level is taken newest first, because the most
    /// recently translated code is the code the guest is running now.
    pending: Vec<VecDeque<u64>>,
    /// Regions to upgrade: those that ran, then those only lifted ahead.
    upgrades: [VecDeque<u64>; 2],
    workers: usize,
    /// Workers running an upgrade now.
    upgrading: usize,
}

/// One unit of a worker's work.
#[derive(Debug, PartialEq, Eq)]
enum Job {
    /// Lift an address, this many hops from code that ran.
    Lift(u64, usize),
    Upgrade(u64),
}

impl Queue {
    fn pop(&mut self) -> Option<Job> {
        let lift = self
            .pending
            .iter_mut()
            .enumerate()
            .find_map(|(depth, level)| {
                let va = match depth {
                    0 => level.pop_front(),
                    _ => level.pop_back(),
                };
                va.map(|va| Job::Lift(va, depth))
            });
        if lift.is_some() || self.upgrading >= MAX_UPGRADERS {
            return lift;
        }
        let upgrade = self.upgrades.iter_mut().find_map(VecDeque::pop_front)?;
        self.upgrading += 1;
        Some(Job::Upgrade(upgrade))
    }
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue {
    pending: Vec::new(),
    upgrades: [VecDeque::new(), VecDeque::new()],
    workers: 0,
    upgrading: 0,
});
static READY: Condvar = Condvar::new();

/// How many workers to run: one CPU is left to the thread realizing classes.
fn wanted() -> usize {
    let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    cpus.saturating_sub(1).clamp(1, MAX_WORKERS)
}

/// Start workers up to [`wanted`]. Returns whether there is at least one.
fn spawn_workers(queue: &mut Queue, vm: &'static Vm) -> bool {
    while queue.workers < wanted() {
        let spawned = std::thread::Builder::new()
            .name("rosemary-prelift".into())
            .spawn(move || work(vm));
        match spawned {
            Ok(_) => queue.workers += 1,
            Err(_) => break,
        }
    }
    queue.workers > 0
}

/// Lift the regions at `entries` on workers, in order, ahead of everything lifted
/// only because control flow may reach it: the caller is about to call each.
pub fn expect(vm: &'static Vm, entries: impl IntoIterator<Item = u64>) {
    push(vm, entries, 0);
}

/// Lift the regions at `targets`, `depth` hops from code that ran, on workers.
pub fn ahead(vm: &'static Vm, targets: impl IntoIterator<Item = u64>, depth: usize) {
    if depth == 0 || depth > ahead_depth() {
        return;
    }
    push(vm, targets, depth);
}

/// Recompile the provisional translation at `va` at the run's own level, on a worker.
/// `ran` says whether a thread has entered it, which puts it ahead of the regions that
/// were only lifted ahead.
pub fn upgrade(vm: &'static Vm, va: u64, ran: bool) {
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    queue.upgrades[usize::from(!ran)].push_back(va);
    // Without a worker the provisional code simply stays.
    if spawn_workers(&mut queue, vm) {
        drop(queue);
        READY.notify_one();
    }
}

fn push(vm: &'static Vm, targets: impl IntoIterator<Item = u64>, depth: usize) {
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    if queue.pending.len() <= depth {
        queue.pending.resize_with(depth + 1, VecDeque::new);
    }
    let level = &mut queue.pending[depth];
    let before = level.len();
    level.extend(targets);
    let added = level.len() - before;
    if depth > 0 && level.len() > MAX_QUEUED_PER_DEPTH {
        let excess = level.len() - MAX_QUEUED_PER_DEPTH;
        level.drain(..excess);
    }
    if added == 0 {
        return;
    }
    // Not having a worker only means the lift happens at first call.
    if !spawn_workers(&mut queue, vm) {
        let level = &mut queue.pending[depth];
        let start = level.len().saturating_sub(added);
        let lost: Vec<u64> = level.drain(start..).collect();
        drop(queue);
        if depth == 0 {
            lost.into_iter().for_each(|va| vm.lift_now(va));
        }
        return;
    }
    drop(queue);
    match added {
        1 => READY.notify_one(),
        _ => READY.notify_all(),
    }
}

fn work(vm: &'static Vm) {
    loop {
        let job = {
            let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(next) = queue.pop() {
                    break next;
                }
                queue = READY.wait(queue).unwrap_or_else(|e| e.into_inner());
            }
        };
        match job {
            Job::Lift(va, depth) => vm.lift_ahead(va, depth),
            Job::Upgrade(va) => {
                vm.upgrade(va);
                QUEUE.lock().unwrap_or_else(|e| e.into_inner()).upgrading -= 1;
                // The slot this frees may be the only thing another worker waits on.
                READY.notify_one();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue() -> Queue {
        Queue {
            pending: vec![VecDeque::new(), VecDeque::new(), VecDeque::new()],
            upgrades: [VecDeque::new(), VecDeque::new()],
            workers: 0,
            upgrading: 0,
        }
    }

    #[test]
    fn expected_calls_come_first_and_in_the_order_they_will_run() {
        let mut q = queue();
        q.pending[1].extend([0x10, 0x20]);
        q.pending[0].extend([0x1, 0x2]);
        assert_eq!(q.pop(), Some(Job::Lift(0x1, 0)));
        assert_eq!(q.pop(), Some(Job::Lift(0x2, 0)));
    }

    #[test]
    fn a_lift_ahead_nearer_code_that_ran_comes_first_and_newest_first() {
        let mut q = queue();
        q.pending[2].push_back(0x300);
        q.pending[1].extend([0x10, 0x20]);
        assert_eq!(q.pop(), Some(Job::Lift(0x20, 1)));
        assert_eq!(q.pop(), Some(Job::Lift(0x10, 1)));
        assert_eq!(q.pop(), Some(Job::Lift(0x300, 2)));
    }

    #[test]
    fn upgrades_wait_for_every_lift_and_code_that_ran_goes_first_oldest_first() {
        let mut q = queue();
        q.upgrades[1].push_back(0xc0);
        q.upgrades[0].extend([0xa0, 0xb0]);
        q.pending[2].push_back(0x300);
        assert_eq!(q.pop(), Some(Job::Lift(0x300, 2)));
        assert_eq!(q.pop(), Some(Job::Upgrade(0xa0)));
        q.upgrading = 0;
        assert_eq!(q.pop(), Some(Job::Upgrade(0xb0)));
        q.upgrading = 0;
        assert_eq!(q.pop(), Some(Job::Upgrade(0xc0)));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn a_lift_never_waits_for_a_free_upgrader() {
        let mut q = queue();
        q.upgrades[0].extend([0xa0, 0xb0, 0xc0]);
        for _ in 0..MAX_UPGRADERS {
            assert!(matches!(q.pop(), Some(Job::Upgrade(_))));
        }
        assert_eq!(q.pop(), None, "every upgrader is busy");
        q.pending[1].push_back(0x10);
        assert_eq!(q.pop(), Some(Job::Lift(0x10, 1)));
        q.upgrading -= 1;
        assert!(matches!(q.pop(), Some(Job::Upgrade(_))));
    }
}
