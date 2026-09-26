#![cfg(not(target_os = "macos"))]
//! Task, thread and host *policies*: the calls that ask how the kernel is
//! scheduling a task, a thread or the machine, and the one that asks it to
//! change the answer.
//!
//! Nothing in this namespace is a Mach *port* call, so none of it needs
//! [`super::space`]'s table - and that is the reason these six could be left
//! unbound for so long without anything naming them. Every one of them is
//! decided here by reading the guest's own call sites out of the Chromium
//! Framework, because the difference between the right answer and a plausible
//! wrong one is entirely in the arguments and the return value.
//!
//! ## What the guest actually asks for
//!
//! Read with `test/tools/raw-range.py` over `__TEXT,__text`; each entry below
//! gives the stub address, the number of call sites, and what the sites pass.
//!
//! | function | sites | what the sites ask for |
//! |---|---:|---|
//! | `thread_policy_set` | 3 | flavours `1`, `2`, `3` |
//! | `thread_switch` | 1235 | always `port = MACH_PORT_NULL`, `options = 1`, `timeout = 1` |
//! | `host_processor_info` | 2 | always `flavor = 2` |
//! | `task_policy_get` | 2 | flavours `1` and `3` |
//! | `task_policy_set` | 6 | flavours `1`, `3` and `8` |
//! | `task_self_trap` | 1 | no arguments, and does not use the answer |
//!
//! The flavour numbers are not guesses. `<mach/task_policy.h>` says
//! `TASK_CATEGORY_POLICY 1`, `TASK_SUPPRESSION_POLICY 3`, `TASK_BASE_QOS_POLICY 8`;
//! `<mach/thread_policy.h>` says `THREAD_STANDARD_POLICY 1`,
//! `THREAD_TIME_CONSTRAINT_POLICY 2`, `THREAD_PRECEDENCE_POLICY 3`. The 1235 and
//! the 2 are not estimates either: they come from counting the `call rel32`
//! sites whose target is the stub, and for `thread_switch` every one of the
//! 1235 sites was checked to carry the same thirteen bytes of argument setup
//! (`31 ff be 01 00 00 00 ba 01 00 00 00`), so "always the current thread, wait,
//! one tick" is a fact about the image rather than about the first site looked
//! at.
//!
//! ## The three answers
//!
//! [`thread_switch`] and [`host_processor_info`] are implemented for the
//! arguments the guest passes and refused for the rest, because in both cases
//! the answer is a fact about this host that can be had: a yield, and per-CPU
//! tick counters out of `/proc/stat`.
//!
//! [`thread_policy_set`] answers **one** flavour, and the reason it can is in
//! the header: `THREAD_STANDARD_POLICY` has "Parameters: [none]" and
//! `THREAD_STANDARD_POLICY_COUNT` is **0**. Honouring it is not a no-op
//! pretence, it is reporting that a thread is in the default timesharing mode -
//! which is what every thread this runtime has is in. Every other flavour is
//! refused, and the two the guest also passes are refused *for the guest's
//! benefit*: the one caller of `thread_policy_set` in the image returns as soon
//! as either of its first two calls fails, so a refusal here is a path the
//! guest has and a wrong success would not be.
//!
//! [`task_policy_get`], [`task_policy_set`] and [`task_self_trap`] are refused
//! outright, and the refusals are the *point* rather than a consolation:
//!
//! * `task_policy_set(TASK_CATEGORY_POLICY)` is how Chromium backgrounded and
//!   foregrounded a process, and `IsProcessBackgrounded()` reads it back. A
//!   success that set nothing would make the process believe it had been
//!   categorised, which is the exact failure this tree exists to prevent.
//! * `task_policy_get` cannot be answered truthfully *and* usefully. Chromium
//!   `CHECK`s that `get_default` came back false (`int3` at
//!   `Chromium Framework+0x5dad3cc` and `+0x5dad48e` when it did not), and on a
//!   real Mac it is false only because `launchd` explicitly categorised the
//!   task. Nothing here does, so the two available answers are both untrue: say
//!   "default" and crash the guest on its `CHECK`, or invent an explicit
//!   category and lie about who set it. A refusal is the third option, and
//!   Chromium handles it - `GetTaskCategoryPolicyRole` returns `std::nullopt`
//!   and `Process::GetProcessPriority` then answers `kNormal`, which is what is
//!   true here.
//! * `task_self_trap` does not return on Darwin. It is a debugger breakpoint,
//!   and the one call site in the image poisons its locals, calls it, and then
//!   builds a fallback object graph *if it comes back* - a path a real Mac never
//!   takes. Returning would run it; trapping with `SIGTRAP` would kill the
//!   process at a point the guest only reaches on purpose.
//!
//! ## The one flavour a refusal here costs the run
//!
//! `task_policy_set(TASK_CATEGORY_POLICY)` is also called by
//! `content/utility/services.cc` (`MacSetDefaultTaskRole`), where a failure
//! only logs - so that path is free. But `vm_allocate`'s refusal over Mojo's
//! tag encoding is a `PCHECK` in `mojo/core/channel_mac.cc`, and that one does
//! abort. It is written down in [`super::vm::vm_allocate`] rather than here.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use super::kern::*;
use super::space::{HOST_SELF, MACH_PORT_NULL, Name, TASK_SELF};

/// `TASK_CATEGORY_POLICY`, and `struct task_category_policy { task_role_t role; }` -
/// one word, so `TASK_CATEGORY_POLICY_COUNT` is 1.
pub const TASK_CATEGORY_POLICY: i32 = 1;
/// `TASK_SUPPRESSION_POLICY`. The structure is not in the SDK header; the count
/// is `TASK_SUPPRESSION_POLICY_COUNT`, which Chromium uses as 16 - a 64-byte
/// structure. The image agrees: it offers `0x10` words and zeroes all 16.
pub const TASK_SUPPRESSION_POLICY: i32 = 3;
/// `TASK_BASE_QOS_POLICY`, and `struct task_qos_policy { task_latency_qos_t
/// task_latency_qos_tier; task_throughput_qos_t task_throughput_qos_tier; }` -
/// two words, which is the count the image passes.
pub const TASK_BASE_QOS_POLICY: i32 = 8;
/// `TASK_FOREGROUND_APPLICATION`, from `enum task_role`.
pub const TASK_FOREGROUND_APPLICATION: i32 = 1;
/// `TASK_BACKGROUND_APPLICATION`. The number Chromium compares against to
/// decide a process is backgrounded, read at `Chromium Framework+0x5dad49a`.
pub const TASK_BACKGROUND_APPLICATION: i32 = 2;

/// `THREAD_STANDARD_POLICY`, "the standard (fair) scheduling mode, assigned to
/// new threads ... Parameters: [none]".
pub const THREAD_STANDARD_POLICY: i32 = 1;
/// `THREAD_TIME_CONSTRAINT_POLICY`, and `struct thread_time_constraint_policy
/// { uint32_t period, computation, constraint; boolean_t preemptible; }` - four
/// words, which is the count the image passes.
pub const THREAD_TIME_CONSTRAINT_POLICY: i32 = 2;
/// `THREAD_PRECEDENCE_POLICY`, and `struct thread_precedence_policy
/// { integer_t importance; }` - one word.
pub const THREAD_PRECEDENCE_POLICY: i32 = 3;

/// `HOST_PROCESSOR_BASIC_INFO`, whose answer is `struct processor_basic_info
/// { cpu_type_t cpu_type; cpu_subtype_t cpu_subtype; integer_t running;
/// integer_t avail; }`. Refused, and the reason is in [`host_processor_info`].
pub const HOST_PROCESSOR_BASIC_INFO: i32 = 1;
/// `HOST_PROCESSOR_CPU_LOAD_INFO`, whose answer is `struct
/// processor_cpu_load_info { natural_t cpu_ticks[CPU_STATE_MAX]; }` with
/// `CPU_STATE_MAX == 4` and `enum cpu_state { CPU_STATE_USER = 0,
/// CPU_STATE_SYSTEM, CPU_STATE_IDLE, CPU_STATE_NICE }`. **This is the only
/// flavour the image ever asks for** (both sites pass 2), and the sixteen-byte
/// stride its two loops walk confirms the four words.
pub const HOST_PROCESSOR_CPU_LOAD_INFO: i32 = 2;

/// `SWITCH_OPTION_WAIT`, from `<mach/thread_switch.h>`: "wait until the thread
/// is running", which is the whole of what the image asks for.
pub const SWITCH_OPTION_WAIT: i32 = 1;
/// `SWITCH_OPTION_DISPATCH`: run the current thread's pending mach message
/// first. Refused by name - it belongs to the IPC subsystem.
pub const SWITCH_OPTION_DISPATCH: i32 = 2;

/// A refusal, said once per reason.
///
/// The return value is the *caller's*, not this function's: which code Darwin
/// uses for a given reason differs (a port that names nothing is
/// `KERN_INVALID_TASK`, a policy that cannot be read is a plain failure), and
/// each site below returns its own number.
///
/// `write(2)` rather than `eprintln!`, for the reason `crate::malloc`'s
/// `abort_naming` gives: a message about a refusal has to be out before the
/// refusal is, and a buffered one can be lost. Allocating to build the line is
/// safe here for a stated reason: this runtime suspends nothing, so no thread is
/// ever stopped holding this tree's allocator lock.
fn note(symbol: &str, said: &AtomicBool, why: &str) {
    if !said.swap(true, Ordering::Relaxed) {
        let line = format!("rosetta3: {symbol}: {why}\n");
        // SAFETY: a live buffer for the length written.
        unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
    }
}

/// `bool`s are four bytes wide in the structures the guest hands over, and the
/// count arguments are in words - so `0x10` words is 64 bytes, which is what
/// `TASK_SUPPRESSION_POLICY_COUNT` must be for the image's `[rbp-0x68]..` frame
/// slot to be the structure rather than something larger.
const _: () = assert!(size_of::<u32>() == 4 && size_of::<bool>() == 1);

/// What a `task_policy_get`/`task_policy_set` refusal is for, named.
fn task_flavour(flavor: i32) -> Option<&'static str> {
    Some(match flavor {
        TASK_CATEGORY_POLICY => "TASK_CATEGORY_POLICY",
        TASK_SUPPRESSION_POLICY => "TASK_SUPPRESSION_POLICY",
        TASK_BASE_QOS_POLICY => "TASK_BASE_QOS_POLICY",
        4 => "TASK_POLICY_STATE",
        9 => "TASK_OVERRIDE_QOS_POLICY",
        10 => "TASK_BASE_LATENCY_QOS_POLICY",
        11 => "TASK_BASE_THROUGHPUT_QOS_POLICY",
        _ => return None,
    })
}

/// What a `thread_policy_set` refusal is for, named.
fn thread_flavour(flavor: i32) -> Option<&'static str> {
    Some(match flavor {
        THREAD_STANDARD_POLICY => "THREAD_STANDARD_POLICY",
        THREAD_TIME_CONSTRAINT_POLICY => "THREAD_TIME_CONSTRAINT_POLICY",
        THREAD_PRECEDENCE_POLICY => "THREAD_PRECEDENCE_POLICY",
        4 => "THREAD_AFFINITY_POLICY",
        5 => "THREAD_BACKGROUND_POLICY",
        7 => "THREAD_LATENCY_QOS_POLICY",
        8 => "THREAD_THROUGHPUT_QOS_POLICY",
        _ => return None,
    })
}

/// What a `host_processor_info` refusal is for, named.
fn processor_flavour(flavor: i32) -> Option<&'static str> {
    Some(match flavor {
        HOST_PROCESSOR_BASIC_INFO => "HOST_PROCESSOR_BASIC_INFO",
        HOST_PROCESSOR_CPU_LOAD_INFO => "HOST_PROCESSOR_CPU_LOAD_INFO",
        _ => return None,
    })
}

/// `kern_return_t thread_policy_set(thread_act_t thread,
/// thread_policy_flavor_t flavor, thread_policy_t policy_info,
/// mach_msg_type_number_t count)`
///
/// **One flavour is answered and the rest are refused, including two the guest
/// passes.** The shape of the call is not Mojo's or Chromium's to choose: the
/// three sites in the image are
///
/// ```text
/// thread_policy_set(t, 1, &standard,  1)   ; test eax,eax; jne <return>
/// thread_policy_set(t, 3, &precedence,1)   ; test eax,eax; jne <return>
/// ...compute a deadline...
/// thread_policy_set(t, 2, &time_constraint, 4)  ; answer ignored
/// ```
///
/// in `Chromium Framework+0x5d9b1eb`, `+0x5d9b20f` and `+0x5d9b36c`, so a
/// failure of either of the first two ends the function early and the third is
/// reached only if both succeeded. `t` is
/// `pthread_mach_thread_np(pthread_self())` - the *other* thread namespace from
/// [`super::task::mach_thread_self`], and the one [`super::thread`] accepts, so
/// it is checked against [`crate::pthread::id`] and nothing else is accepted.
///
/// # Safety
///
/// `policy_info` is readable for `count` 32-bit words.
pub unsafe extern "C" fn thread_policy_set(
    thread: Name,
    flavor: i32,
    policy_info: *mut i32,
    count: u32,
) -> i32 {
    static SAID_FLAVOUR: AtomicBool = AtomicBool::new(false);
    static SAID_NULL: AtomicBool = AtomicBool::new(false);
    static SAID_THREAD: AtomicBool = AtomicBool::new(false);
    static SAID_PRECEDENCE: AtomicBool = AtomicBool::new(false);
    static SAID_TIME: AtomicBool = AtomicBool::new(false);

    if policy_info.is_null() {
        note(
            "thread_policy_set",
            &SAID_NULL,
            "a null `thread_policy_t`; mach/thread_policy.h declares it as the \
             structure itself, not an optional pointer",
        );
        return KERN_INVALID_ADDRESS;
    }
    // The one thread this runtime can speak for. The same two namespaces as
    // `super::thread::admit`, and the same reason for the message.
    let mine = crate::pthread::id();
    if thread != mine {
        note(
            "thread_policy_set",
            &SAID_THREAD,
            &format!(
                "thread {thread:#x} is not this thread ({mine:#x}); this runtime reports the \
                 calling thread's own scheduling and no other thread's, and mach_thread_self() \
                 ({:#x}) is a different namespace again",
                super::task::mach_thread_self()
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    match flavor {
        // The flavour with no parameters. See the module note: this is a state
        // answer, not a stub, and it is the one the guest cannot proceed
        // without.
        THREAD_STANDARD_POLICY => KERN_SUCCESS,
        THREAD_PRECEDENCE_POLICY => {
            // SAFETY: the caller declared `count` readable words, and the
            // caller is a guest that has been loading this struct since before
            // it made the call.
            let importance = if policy_info.is_null() || count == 0 {
                None
            } else {
                // SAFETY: as above.
                Some(unsafe { std::ptr::read_unaligned(policy_info) })
            };
            note(
                "thread_policy_set",
                &SAID_PRECEDENCE,
                &format!(
                    "THREAD_PRECEDENCE_POLICY with importance {} - the importance scale is a \
                     Darwin scheduler table, not a priority number, and this host has no \
                     equivalent to place a value in; the one caller in the image returns as soon \
                     as this fails, so a refusal is a path it has",
                    match importance {
                        Some(value) => format!("{value}"),
                        None => "of an unreadable length".to_string(),
                    }
                ),
            );
            KERN_INVALID_ARGUMENT
        }
        THREAD_TIME_CONSTRAINT_POLICY => {
            note(
                "thread_policy_set",
                &SAID_TIME,
                "THREAD_TIME_CONSTRAINT_POLICY - period, computation, constraint and preemptible \
                 ask the scheduler to reserve processor time, which is SCHED_FIFO with a \
                 reserved bandwidth and a privilege this runtime does not have; there is no CFS \
                 equivalent, and a success would be claiming a reservation nothing holds",
            );
            KERN_INVALID_ARGUMENT
        }
        _ => {
            note(
                "thread_policy_set",
                &SAID_FLAVOUR,
                &format!(
                    "flavor {flavor} ({}) - not answered; this runtime schedules no thread and \
                     sets no policy, and the guest's own sites use only 1, 2 and 3",
                    thread_flavour(flavor).unwrap_or("not a thread-policy flavour")
                ),
            );
            KERN_INVALID_ARGUMENT
        }
    }
}

/// `kern_return_t task_policy_get(task_t task, task_policy_flavor_t flavor,
/// task_policy_t policy_info, mach_msg_type_number_t *count,
/// boolean_t *get_default)`
///
/// **Refused, always.** The module note is the argument; the two things a
/// caller here does with a failure are the reason refusing is safe:
///
/// * `base/process/process_mac.cc`'s `GetTaskCategoryPolicyRole` logs the
///   `kern_return_t` and returns `std::nullopt`, which
///   `Process::GetProcessPriority` turns into `Process::Priority::kNormal` -
///   and so `IsProcessBackgrounded()` answers false, which is **true** on this
///   host: nothing here ever sets a task category, so nothing can be
///   backgrounded.
/// * `content/utility/services.cc` and `macOS`'s own role plumbing only
///   `MACH_LOG` a failure.
///
/// A success would be worse than the crash it avoids. `get_default` is an
/// in/out parameter whose documented meaning is "there are no current settings
/// due to other parameters taking precedence, and the default ones are being
/// returned instead" (`<mach/task_policy.h>`), and Chromium `CHECK`s that it
/// came back *false*. On a Mac it is false only because `launchd` explicitly
/// categorised the task; here nobody has, so `true` is the honest word and it
/// is an `int3`. The only other answer is a category somebody did not set.
///
/// # Safety
///
/// `policy_info` is writable for `*count` 32-bit words, `count` and
/// `get_default` are writable.
pub unsafe extern "C" fn task_policy_get(
    task: Name,
    flavor: i32,
    policy_info: *mut c_void,
    count: *mut u32,
    get_default: *mut u32,
) -> i32 {
    static SAID_TASK: AtomicBool = AtomicBool::new(false);
    static SAID: AtomicBool = AtomicBool::new(false);

    if task != TASK_SELF {
        note(
            "task_policy_get",
            &SAID_TASK,
            &format!(
                "task {task:#x} is not this task ({TASK_SELF:#x}); no other task's policies exist \
                 in this runtime, and this tree's `mach_task_self_` and `mach_task_self()` are \
                 two spellings of {TASK_SELF:#x} while a name from `mach_port_allocate` is not"
            ),
        );
        return KERN_INVALID_TASK;
    }
    let _ = (policy_info, count, get_default);
    note(
        "task_policy_get",
        &SAID,
        &format!(
            "flavor {flavor} ({}) - refused, and no out-parameter is written: answering would mean \
             reporting a task category or a suppression state that this runtime never set, and \
             `get_default` is what the guest CHECKs, so a truthful `true` is an int3 and a \
             truthful `false` is a claim about a category nobody assigned",
            task_flavour(flavor).unwrap_or("not a task-policy flavour")
        ),
    );
    KERN_FAILURE
}

/// `kern_return_t task_policy_set(task_t task, task_policy_flavor_t flavor,
/// task_policy_t policy_info, mach_msg_type_number_t count)`
///
/// **Refused, always** - and this is the one where a `KERN_SUCCESS` would be
/// the failure this tree exists to prevent. `Process::IsProcessBackgrounded()`
/// is `GetProcessPriority() == kLow`, and `GetProcessPriority()` is a
/// `task_policy_get` of `TASK_CATEGORY_POLICY` that has to be *paired* with
/// the `task_policy_set` that set it: `Process::SetPriorityImpl` reads the
/// category, and the comment it carries says the difference is "unlinked to
/// match the state" - a process is backgrounded only when the category says so
/// **and** the suppression policy is inactive. Setting the category without the
/// suppression policy (or claiming to have) is how a process ends up
/// "backgrounded" with no thread suppressed, or told it is foreground when its
/// threads are not. This namespace sets neither, so it says neither.
///
/// The three flavours the image passes, and who passes them:
///
/// * `TASK_CATEGORY_POLICY` (1 word) - `base/process/process_mac.cc`'s
///   `SetTaskCategoryPolicy`, and `content/utility/services.cc`'s
///   `MacSetDefaultTaskRole`. Failure is a `MACH_LOG` and a `false` return in
///   both.
/// * `TASK_SUPPRESSION_POLICY` (16 words) - `SetTaskSuppressionPolicy`, same
///   shape.
/// * `TASK_BASE_QOS_POLICY` (2 words, and the image writes
///   `0x00ff0001`/`0x00fe0001`, which are `LATENCY_QOS_TIER_0` and
///   `THROUGHPUT_QOS_TIER_0`) - `content/utility/services.cc`, whose failure
///   only logs.
///
/// `TASK_SET_STATE` is not among them and is not reachable from this image; it
/// would be refused by name like every other, which is the only correct answer
/// for it, since claiming to have put a thread to sleep is exactly the state a
/// caller would then rely on not being set.
///
/// # Safety
///
/// `policy_info` is readable for `count` 32-bit words.
pub unsafe extern "C" fn task_policy_set(
    task: Name,
    flavor: i32,
    policy_info: *mut c_void,
    count: u32,
) -> i32 {
    static SAID_TASK: AtomicBool = AtomicBool::new(false);
    static SAID: AtomicBool = AtomicBool::new(false);

    if task != TASK_SELF {
        note(
            "task_policy_set",
            &SAID_TASK,
            &format!(
                "task {task:#x} is not this task ({TASK_SELF:#x}); this runtime sets no other \
                 task's policy"
            ),
        );
        return KERN_INVALID_TASK;
    }
    let _ = (policy_info, count);
    note(
        "task_policy_set",
        &SAID,
        &format!(
            "flavor {flavor} ({}), {count} word(s) - refused: this runtime sets no task policy, \
             and Process::IsProcessBackgrounded() is a task_policy_get of exactly this category \
             paired with this suppression policy, so a success that set nothing would make a \
             process believe it had been categorised",
            task_flavour(flavor).unwrap_or("not a task-policy flavour")
        ),
    );
    KERN_FAILURE
}

/// `kern_return_t task_self_trap(void)`
///
/// **Refused, and the refusal is the honest shape of the answer rather than a
/// dodge.** On Darwin this is a debugger breakpoint: `osfmk/kern/task.c` traps
/// and does not come back, and `task.defs` declares it as an ordinary MIG
/// routine, so its `kern_return_t` is unreachable.
///
/// The one call site in the image is
/// `Chromium Framework+0x936c39d`, and what follows it is the evidence for
/// both halves of that sentence. It poisons three locals with `0xaa…`, calls
/// `task_self_trap`, and then - **only if the call returned** - fills in a
/// thirty-byte structure whose word 8 is the return value, builds a second one
/// identically, hands both to a Mojo C thunk, and destroys them again. A real
/// Mac never runs that block. So the two available answers are both wrong: a
/// return value runs a fallback the guest has a path to only because this
/// runtime cannot trap, and `raise(SIGTRAP)` kills the process at a point it
/// reached deliberately.
///
/// The number is `KERN_FAILURE`, which is also what `xnu` answers for a
/// `task_self_trap` it will not perform, and it is written where the guest's own
/// fallback would read it, so the fallback's `eax` is this and not the poison
/// the guest put there.
pub extern "C" fn task_self_trap() -> i32 {
    static SAID: AtomicBool = AtomicBool::new(false);
    note(
        "task_self_trap",
        &SAID,
        "this does not return on Darwin - it is a debugger breakpoint (osfmk/kern/task.c \
         DebuggerBreakpoint), and this runtime has no Mach trap machinery and no debugger to \
         hand it to; the one call site in the image (Chromium Framework+0x936c39d) builds a \
         fallback only because the call is expected not to come back, so returning would run a \
         path a real Mac never reaches, and raising SIGTRAP would kill the process at a point \
         the guest reached on purpose",
    );
    KERN_FAILURE
}

/// `kern_return_t thread_switch(mach_port_t port, int options, int timeout)`
///
/// **Implemented for the one call the image makes, and refused for the rest.**
/// All 1235 sites pass `MACH_PORT_NULL`, `SWITCH_OPTION_WAIT` and one tick -
/// checked, not sampled, by comparing the thirteen bytes before each `call`
/// against that one sequence. The shape of the loop around them is the reason
/// the answer can be `sched_yield`:
///
/// ```text
/// for (;;) {
///   if (*state != 1) return;          // the guest's own condition
///   if (now() - start > 999) { usleep(1000); continue; }   // 0x3e8 microseconds
///   thread_switch(MACH_PORT_NULL, SWITCH_OPTION_WAIT, 1);
/// }
/// ```
///
/// at `Chromium Framework+0x487047`. It is a poll that sleeps a millisecond and,
/// in the last millisecond before its deadline, hands the CPU to whoever else
/// wants it. `thread_switch(MACH_PORT_NULL, SWITCH_OPTION_WAIT, n)` is the Mach
/// spelling of "take me off the run queue and put me back in `n` ticks"; the
/// part of that a host can do is the taking off, which is `sched_yield(2)`.
///
/// What is **not** emulated is the deadline. `sched_yield` has no timeout, so
/// this returns at once and the loop spins where Mach would have slept for up to
/// a tick. The cost is processor time, not correctness, because the guest
/// re-reads its own condition at the top of every iteration - and the code
/// above shows it takes the `usleep(1000)` branch for all but the final
/// millisecond of the wait.
///
/// The return value is discarded at every one of the 1235 sites, so the number
/// here is a formality; it is `KERN_SUCCESS` because the thread really was
/// descheduled.
pub extern "C" fn thread_switch(port: Name, options: i32, timeout: i32) -> i32 {
    static SAID_PORT: AtomicBool = AtomicBool::new(false);
    static SAID_OPTIONS: AtomicBool = AtomicBool::new(false);
    static SAID_TIMEOUT: AtomicBool = AtomicBool::new(false);

    if port != MACH_PORT_NULL {
        // `MACH_PORT_NULL` *is* the calling thread in this call's vocabulary,
        // so a non-null port is a request to switch to some *other* thread -
        // and naming another thread needs the same identity
        // `super::thread::thread_suspend` cannot establish on this host.
        note(
            "thread_switch",
            &SAID_PORT,
            &format!(
                "port {port:#x} names a thread to switch to; MACH_PORT_NULL (0) is how this call \
                 spells the calling thread, and all {sites} sites in the image pass it. Switching \
                 to a *named* thread needs a thread port, and this runtime has two disjoint \
                 thread namespaces and no way to map one onto the other",
                sites = 1235
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if options & SWITCH_OPTION_DISPATCH != 0 {
        note(
            "thread_switch",
            &SAID_OPTIONS,
            &format!(
                "options {options:#x} include SWITCH_OPTION_DISPATCH ({SWITCH_OPTION_DISPATCH:#x}), \
                 which runs the calling thread's pending mach message first - that is the IPC \
                 subsystem's business, and this thread is not in one of its message loops"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if options & !SWITCH_OPTION_WAIT != 0 {
        note(
            "thread_switch",
            &SAID_OPTIONS,
            &format!(
                "options {options:#x} - only SWITCH_OPTION_WAIT ({SWITCH_OPTION_WAIT:#x}) is \
                 answered, which is the only value the image passes"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if timeout != 1 {
        // Not a refusal of the call - a note, because a different timeout is
        // the same yield with a different deadline and the deadline is the part
        // that is not emulated (see the function note).
        note(
            "thread_switch",
            &SAID_TIMEOUT,
            &format!(
                "timeout {timeout} tick(s) - the deadline is not emulated: sched_yield(2) hands \
                 the CPU over and comes straight back, so the caller spins where Mach would have \
                 slept. The image's own sites all pass 1, and their loops re-read their condition \
                 every iteration, so the cost is processor time and not a wrong answer"
            ),
        );
    }
    // SAFETY: `sched_yield` takes no arguments, has no preconditions, and
    // cannot fail.
    unsafe { libc::sched_yield() };
    KERN_SUCCESS
}

/// `host_processor_info`'s array: the page-aligned, page-sized buffer it hands
/// out, kept so that a caller which asks twice does not leak a page each time.
///
/// Darwin's answer is an out-of-line MIG array - `mach_msg_ool_descriptor_t
/// out_processor_info` in the `__Reply__host_processor_info_t` the SDK's
/// `mach/mach_host.h` carries - and the image frees it with
/// `vm_deallocate(task, array)`, which on Darwin works because an IPC kalloc'd
/// block is found by its address. This runtime's `vm_deallocate` is a `munmap`,
/// which needs a size the caller does not have, so that free fails and is
/// ignored by the caller; holding one buffer keeps the cost at one page for the
/// life of the process instead of one per call.
///
/// The deviation is one line and it is stated here: two calls made before the
/// first has been read would see the same array. The image does not do that -
/// the loop that reads the array starts at `Chromium Framework+0x43395e0`,
/// thirteen instructions after the call it is reading.
fn load_array() -> &'static std::sync::Mutex<Load> {
    static ARRAY: std::sync::OnceLock<std::sync::Mutex<Load>> = std::sync::OnceLock::new();
    ARRAY.get_or_init(|| std::sync::Mutex::new(Load::default()))
}

/// The one buffer [`load_array`] hands out, and why it is behind a `Mutex`.
///
/// `Send` is spelled out rather than derived: a raw pointer is not `Send` in
/// this Rust, and the reason it is safe *here* is specific - the pointer is
/// only ever written by [`host_processor_info`] while this lock is held, and
/// only read by the guest afterwards. A reader that is not that guest does not
/// exist, because the array is a Mach IPC out-of-line buffer and there is one
/// of it per call.
#[derive(Default)]
struct Load {
    /// The mapping's base, or null before the first call.
    base: usize,
    /// Its length, so a growth can unmap exactly what it mapped.
    len: usize,
}

// SAFETY: see `Load`'s own note - every access is under `load_array`'s lock,
// and the only other holder of the address is the guest that was just handed
// it, which cannot run concurrently with the call that is holding the lock.
unsafe impl Send for Load {}

/// One CPU's four `cpu_ticks`, in `enum cpu_state` order.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CpuLoad {
    user: u32,
    system: u32,
    idle: u32,
    nice: u32,
}

const _: () = {
    assert!(size_of::<CpuLoad>() == 16);
    assert!(size_of::<CpuLoad>() / size_of::<u32>() == 4);
};

/// Per-CPU tick counters out of `/proc/stat`, in `cpu_ticks[]` order.
///
/// `/proc/stat`'s per-CPU line is `user nice system idle iowait …` and Darwin's
/// is `user system idle nice`, so the four this fills are fields 0, 2, 3 and 1
/// of the Linux line and nothing else - `iowait`, `irq`, `softirq` and `steal`
/// have no `cpu_ticks` slot to go in and are dropped rather than folded into
/// one that means something else.
fn cpu_load() -> Vec<CpuLoad> {
    let Ok(stat) = std::fs::read_to_string("/proc/stat") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in stat.lines() {
        // The aggregate line is `cpu ` and a per-CPU line is `cpu0 ` .. `cpuN `,
        // so the first token after `cpu` is the processor number on one and
        // nothing at all on the other. Reading the number as a tick count is
        // the way this would come out as "cpu0 is idle and the rest are busy",
        // so it is skipped explicitly rather than by position.
        let Some(rest) = line.strip_prefix("cpu") else {
            continue;
        };
        // The character right after `cpu` is what tells the two forms apart, and
        // it has to be tested *before* the whitespace is: the aggregate line is
        // `cpu  1907658 60 …`, so its first number is all digits too and a
        // `split_ascii_whitespace` test alone would take the aggregate for
        // `cpu0`. That is a wrong answer rather than a wrong count - every
        // index would be off by one, and index 0 would be the machine's
        // lifetime totals.
        if !rest.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let mut fields = rest.split_ascii_whitespace();
        // The processor number, which is not a tick count.
        let Some(index) = fields.next().filter(|f| f.bytes().all(|b| b.is_ascii_digit())) else {
            continue;
        };
        debug_assert!(index.parse::<u32>().is_ok());
        let mut next = || fields.next().and_then(|f| f.parse::<u64>().ok());
        let (Some(user), Some(nice), Some(system), Some(idle)) = (next(), next(), next(), next())
        else {
            continue;
        };
        out.push(CpuLoad {
            user: user.min(u32::MAX as u64) as u32,
            system: system.min(u32::MAX as u64) as u32,
            idle: idle.min(u32::MAX as u64) as u32,
            nice: nice.min(u32::MAX as u64) as u32,
        });
    }
    out
}

/// `kern_return_t host_processor_info(host_t host, processor_flavor_t flavor,
/// natural_t *out_processor_count, processor_info_array_t
/// *out_processor_info, mach_msg_type_number_t *out_processor_infoCnt)`
///
/// **One flavour is implemented, and it is the only one the image asks for**:
/// both call sites pass `HOST_PROCESSOR_CPU_LOAD_INFO` (2), and both read the
/// answer at a **sixteen-byte stride**, which is
/// `sizeof(struct processor_cpu_load_info)` for `CPU_STATE_MAX == 4` and
/// nothing else. The third out-parameter is the number of words *per element* -
/// the SDK's MIG reply carries `out_processor_infoCnt` beside an OOL
/// descriptor - and the image never reads it; it is written because it is what
/// the kernel writes.
///
/// `HOST_PROCESSOR_BASIC_INFO` (1) is **refused**, and this is the case the
/// warning about invented encodings is about. Its answer is
/// `struct processor_basic_info { cpu_type_t cpu_type; cpu_subtype_t
/// cpu_subtype; integer_t running; integer_t avail; }`, and the first two words
/// are a Darwin ABI: `CPU_TYPE_X86_64` is `CPU_ARCH_ABI64 | 7` = `0x0100_0007`,
/// the subtype's top byte is `CPU_SUBTYPE_MASK` and its low bits are *feature*
/// bits (`CPU_SUBTYPE_LIB64`, `CPU_SUBTYPE_HASWELL`, …) that a caller is
/// entitled to switch on. A number produced by looking at what this CPU
/// happens to be would be a plausible answer that means nothing, so the flavour
/// is refused and the message says which two fields and why.
///
/// # Safety
///
/// All three out-parameters are writable; `*out_processor_info` is a pointer
/// to `out_processor_infoCnt` words of space on return.
pub unsafe extern "C" fn host_processor_info(
    host: Name,
    flavor: i32,
    out_processor_count: *mut u32,
    out_processor_info: *mut *mut u32,
    out_processor_info_count: *mut u32,
) -> i32 {
    static SAID_HOST: AtomicBool = AtomicBool::new(false);
    static SAID_NULL: AtomicBool = AtomicBool::new(false);
    static SAID_BASIC: AtomicBool = AtomicBool::new(false);
    static SAID_FLAVOUR: AtomicBool = AtomicBool::new(false);
    static SAID_READ: AtomicBool = AtomicBool::new(false);

    if out_processor_count.is_null() || out_processor_info.is_null() || out_processor_info_count.is_null()
    {
        note(
            "host_processor_info",
            &SAID_NULL,
            "a null out-parameter; mach/mach_host.h declares all three as required outputs",
        );
        return KERN_INVALID_ADDRESS;
    }
    if host != HOST_SELF {
        note(
            "host_processor_info",
            &SAID_HOST,
            &format!(
                "host {host:#x} is not this host ({HOST_SELF:#x}), which is what \
                 mach_host_self() returns; only this machine's processors are described"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    if flavor != HOST_PROCESSOR_CPU_LOAD_INFO {
        if flavor == HOST_PROCESSOR_BASIC_INFO {
            note(
                "host_processor_info",
                &SAID_BASIC,
                "HOST_PROCESSOR_BASIC_INFO - its answer starts with a cpu_type/cpu_subtype pair, \
                 and that encoding is a Darwin ABI rather than a description of this CPU: \
                 CPU_TYPE_X86_64 is CPU_ARCH_ABI64|7 = 0x01000007 and the subtype's low bits are \
                 feature flags a caller may switch on (CPU_SUBTYPE_LIB64, CPU_SUBTYPE_HASWELL, \
                 ...). Any number derived from what this processor happens to be would be a \
                 plausible answer that means nothing, so the flavour is refused",
            );
        } else {
            note(
                "host_processor_info",
                &SAID_FLAVOUR,
                &format!(
                    "flavor {flavor} ({}) - only HOST_PROCESSOR_CPU_LOAD_INFO \
                     ({HOST_PROCESSOR_CPU_LOAD_INFO}) is answered, and it is the only one the \
                     image asks for",
                    processor_flavour(flavor).unwrap_or("not a processor-info flavour")
                ),
            );
        }
        return KERN_INVALID_ARGUMENT;
    }

    let load = cpu_load();
    if load.is_empty() {
        note(
            "host_processor_info",
            &SAID_READ,
            "no per-CPU lines in /proc/stat, so there are no tick counters to report; this host's \
             cpu_ticks are not invented",
        );
        return KERN_FAILURE;
    }
    let words = load.len() * 4;
    let bytes = words * size_of::<u32>();

    let mut held = load_array().lock().unwrap_or_else(|e| e.into_inner());
    if held.len < bytes {
        // SAFETY: a previous buffer, if any, is ours - nothing else holds a
        // pointer to it, and the guest's `vm_deallocate` of it has already
        // failed (see `load_array`).
        unsafe {
            if held.base != 0 {
                libc::munmap(held.base as *mut c_void, held.len);
            }
        }
        // One `mmap` per growth, page-aligned and page-rounded, so the address
        // a guest is handed looks the way a page-granular Mach allocation does.
        // The page size is this tree's own, the one `host_page_size` publishes
        // and the one `vm_region_64` reports - not a second opinion from
        // `sysconf`, which would be a page size that disagrees with the map the
        // guest is being shown.
        super::info::publish_page_size();
        let page = super::info::VM_PAGE_SIZE.load(Ordering::Relaxed).max(4096) as usize;
        let want = bytes.div_ceil(page) * page;
        // SAFETY: `mmap` with `MAP_ANONYMOUS` writes nothing and hands back
        // either a whole mapping of `want` bytes or `MAP_FAILED`.
        let fresh = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                want,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if fresh == libc::MAP_FAILED {
            return KERN_RESOURCE_SHORTAGE;
        }
        held.base = fresh as usize;
        held.len = want;
    }
    // SAFETY: `held.len >= bytes` above, so the mapping covers every word about
    // to be written, and it is `PROT_READ | PROT_WRITE`.
    unsafe {
        std::ptr::copy_nonoverlapping(
            load.as_ptr().cast::<u8>(),
            held.base as *mut u8,
            bytes,
        );
        out_processor_count.write(load.len() as u32);
        out_processor_info.write(held.base as *mut u32);
        out_processor_info_count.write(4);
    }
    KERN_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the `thread_policy_set` sites pass, in the order they pass it: the
    /// first two must succeed for the third to be reached, and the third's
    /// answer is discarded. This test is the state half of that statement.
    #[test]
    fn the_standard_policy_is_answered_and_the_two_the_guest_also_passes_are_not() {
        let mine = crate::pthread::id();
        let mut standard = 0i32;
        // SAFETY: a live `i32` and a count of 1, which is what the site passes.
        assert_eq!(
            unsafe { thread_policy_set(mine, THREAD_STANDARD_POLICY, &mut standard, 1) },
            KERN_SUCCESS
        );
        assert_eq!(standard, 0, "THREAD_STANDARD_POLICY has no parameters to write");

        let mut precedence = [63i32];
        // SAFETY: a live one-word array and a count of 1.
        assert_eq!(
            unsafe { thread_policy_set(mine, THREAD_PRECEDENCE_POLICY, precedence.as_mut_ptr(), 1) },
            KERN_INVALID_ARGUMENT
        );
        assert_eq!(precedence, [63], "a refusal wrote to the policy");

        let mut constraint = [1u32, 2, 3, 0];
        // SAFETY: a live four-word array and a count of 4.
        assert_eq!(
            unsafe {
                thread_policy_set(
                    mine,
                    THREAD_TIME_CONSTRAINT_POLICY,
                    constraint.as_mut_ptr().cast(),
                    4,
                )
            },
            KERN_INVALID_ARGUMENT
        );
        assert_eq!(constraint, [1, 2, 3, 0], "a refusal wrote to the policy");
    }

    /// `THREAD_STANDARD_POLICY_COUNT` is **0** in the header, next to a structure
    /// with one field. Both facts are what make honouring the flavour a state
    /// answer, so both are pinned here rather than only in a comment.
    #[test]
    fn the_answered_flavour_is_the_one_with_no_parameters() {
        // `struct thread_standard_policy { natural_t no_data; }` - one word in
        // the structure, and `THREAD_STANDARD_POLICY_COUNT 0`, which is why the
        // image's one-word argument is read by nobody.
        assert_eq!(size_of::<u32>(), 4);
        // `struct thread_time_constraint_policy` is four words, which is the
        // count the third site passes - so a structure of another length would
        // make that site's `count` wrong rather than merely generous.
        assert_eq!(size_of::<CpuLoad>(), 16);
    }

    /// A thread in the other namespace, and no thread at all, are refused -
    /// and neither reaches the flavour question, so a caller cannot get a
    /// success for a thread this runtime knows nothing about.
    #[test]
    fn a_thread_this_runtime_does_not_have_is_refused_before_the_flavour() {
        let foreign = super::super::task::mach_thread_self();
        assert_ne!(foreign, 0, "`mach_thread_self` always names something");
        let mut policy = 0i32;
        for thread in [0u32, foreign, crate::pthread::id() + 1] {
            // SAFETY: a live `i32` and a count of 1.
            assert_eq!(
                unsafe { thread_policy_set(thread, THREAD_STANDARD_POLICY, &mut policy, 1) },
                KERN_INVALID_ARGUMENT,
                "thread {thread:#x} was answered"
            );
        }
        // A null policy is an address, not an argument, whichever thread.
        // SAFETY: the null is what is being tested.
        assert_eq!(
            unsafe { thread_policy_set(crate::pthread::id(), THREAD_STANDARD_POLICY, std::ptr::null_mut(), 1) },
            KERN_INVALID_ADDRESS
        );
    }

    /// **`task_policy_get` writes nothing, ever.** The three out-parameters are
    /// poisoned the way Chromium poisons them (`-ftrivial-auto-var-init=pattern`
    /// is what put the `0xaa…` at `Chromium Framework+0x5dad321`), and a
    /// refusal that left one of them written would be a policy reported.
    #[test]
    fn task_policy_get_leaves_every_out_parameter_as_the_caller_wrote_it() {
        const POISON: u32 = 0xaaaa_aaaa;
        for flavor in [TASK_CATEGORY_POLICY, TASK_SUPPRESSION_POLICY, TASK_BASE_QOS_POLICY, 4, 77]
        {
            let mut policy = [POISON; 16];
            let mut count = 1u32;
            let mut get_default = POISON;
            // SAFETY: a live 16-word buffer, and `count` says one word is used.
            let rc = unsafe {
                task_policy_get(
                    TASK_SELF,
                    flavor,
                    policy.as_mut_ptr().cast(),
                    &mut count,
                    &mut get_default,
                )
            };
            assert_eq!(rc, KERN_FAILURE, "flavor {flavor} was answered");
            assert!(
                policy.iter().all(|w| *w == POISON),
                "flavor {flavor}: the policy was written"
            );
            assert_eq!(count, 1, "flavor {flavor}: the count was written");
            assert_eq!(get_default, POISON, "flavor {flavor}: get_default was written");
        }
        // Another task is a different code entirely, and still writes nothing.
        let mut policy = [POISON; 16];
        let mut count = 1u32;
        let mut get_default = POISON;
        // SAFETY: as above.
        let rc = unsafe {
            task_policy_get(
                TASK_SELF + 4,
                TASK_CATEGORY_POLICY,
                policy.as_mut_ptr().cast(),
                &mut count,
                &mut get_default,
            )
        };
        assert_eq!(rc, KERN_INVALID_TASK);
        assert!(policy.iter().all(|w| *w == POISON));
        assert_eq!(count, 1);
        assert_eq!(get_default, POISON);
    }

    /// The same for `task_policy_set`, and the reason is stronger here: a
    /// `TASK_CATEGORY_POLICY` success is what `Process::IsProcessBackgrounded()`
    /// later reads back, so the test is written the way the guest reads it -
    /// set, then get, and the two must not disagree.
    #[test]
    fn task_policy_set_is_refused_and_leaves_the_policy_as_the_caller_wrote_it() {
        for (flavor, count) in [
            (TASK_CATEGORY_POLICY, 1usize),
            (TASK_SUPPRESSION_POLICY, 16),
            (TASK_BASE_QOS_POLICY, 2),
            // `TASK_SET_STATE` is not reachable from this image - no site
            // passes it - and it is the one flavour where a success would be
            // the worst possible lie, so it is refused by name like the rest.
            (4, 1),
        ] {
            let mut policy = vec![0xffff_ffffu32; count];
            let before = policy.clone();
            // SAFETY: a live `count`-word buffer and a matching count.
            let rc = unsafe { task_policy_set(TASK_SELF, flavor, policy.as_mut_ptr().cast(), count as u32) };
            assert_eq!(rc, KERN_FAILURE, "flavor {flavor} was answered");
            assert_eq!(policy, before, "flavor {flavor}: the policy was written");
        }
        // Another task is a different code, and writes nothing either.
        let mut policy = vec![0xffff_ffffu32; 1];
        // SAFETY: as above.
        assert_eq!(
            unsafe { task_policy_set(TASK_SELF + 4, TASK_CATEGORY_POLICY, policy.as_mut_ptr().cast(), 1) },
            KERN_INVALID_TASK
        );
        assert_eq!(policy, vec![0xffff_ffffu32; 1]);
    }

    /// `task_self_trap` refuses with a number the guest's own fallback can read
    /// rather than the poison it put there, and the value is one it is told
    /// about by name.
    #[test]
    fn task_self_trap_refuses_with_a_readable_number() {
        assert_eq!(task_self_trap(), KERN_FAILURE);
        // The call site stores `eax` at `+0x30` of its structure, so the
        // number has to be a `kern_return_t` and not, say, a trap.
        let stored = task_self_trap() as i32;
        assert_eq!(stored, KERN_FAILURE);
        assert!(super::super::kern::describe(stored).is_some(), "and it has a name");
    }

    /// **The one the image makes: the current thread, wait, one tick.** It
    /// succeeds, and it succeeds *quickly* - the point of the call is to hand
    /// the processor away, so a `thread_switch` that blocked would be a
    /// different function.
    #[test]
    fn switching_the_calling_thread_off_yields_and_succeeds() {
        let before = std::time::Instant::now();
        assert_eq!(thread_switch(MACH_PORT_NULL, SWITCH_OPTION_WAIT, 1), KERN_SUCCESS);
        assert!(
            before.elapsed() < std::time::Duration::from_millis(500),
            "thread_switch blocked, which is not a yield"
        );
    }

    /// And the refusals: a *named* thread, and the dispatch option. Both are
    /// `KERN_INVALID_ARGUMENT`, and neither can be reached by mistake from the
    /// image's own argument vector.
    #[test]
    fn switching_to_a_named_thread_or_dispatching_is_refused() {
        let foreign = super::super::task::mach_thread_self();
        assert_ne!(foreign, 0);
        assert_eq!(thread_switch(foreign, SWITCH_OPTION_WAIT, 1), KERN_INVALID_ARGUMENT);
        assert_eq!(
            thread_switch(MACH_PORT_NULL, SWITCH_OPTION_WAIT | SWITCH_OPTION_DISPATCH, 1),
            KERN_INVALID_ARGUMENT
        );
        // An option bit that is not one of the two.
        assert_eq!(thread_switch(MACH_PORT_NULL, 0x40, 1), KERN_INVALID_ARGUMENT);
    }

    /// `host_processor_info`'s answer, read the way the guest reads it: the
    /// count, then sixteen bytes per CPU, four signed words in
    /// `cpu_ticks[]` order. What a guest can *check* is that the stride is
    /// right and that the numbers are tick counts rather than the poison - so
    /// this asserts the stride, the word count, and that two reads a moment
    /// apart do not go backwards.
    #[test]
    fn the_load_array_is_four_words_per_processor_and_moves_forward() {
        let mut count = 0u32;
        let mut array: *mut u32 = std::ptr::null_mut();
        let mut words = 0u32;
        // SAFETY: three live out-parameters.
        let rc = unsafe {
            host_processor_info(HOST_SELF, HOST_PROCESSOR_CPU_LOAD_INFO, &mut count, &mut array, &mut words)
        };
        assert_eq!(rc, KERN_SUCCESS);
        assert!(!array.is_null());
        // The SDK's MIG reply carries `out_processor_infoCnt` beside an OOL
        // descriptor, so it is the number of words *per element*.
        assert_eq!(words, 4, "CPU_STATE_MAX is 4");
        assert!(count >= 1, "a host with no processors is not a host");

        let first = || {
            // SAFETY: the call above published `count * words` words.
            let mut ticks = Vec::new();
            for cpu in 0..count as usize {
                // SAFETY: as above; 4 words per element is what `words` says.
                let base = unsafe { array.add(cpu * 4) };
                for i in 0..4 {
                    // SAFETY: as above.
                    ticks.push(unsafe { std::ptr::read(base.add(i)) });
                }
            }
            ticks
        };
        let before = first();
        assert_eq!(before.len(), count as usize * 4);
        // Every CPU accumulates time, so no element may be zero in all four
        // words: an all-zero element is what a refusal or a short fill looks
        // like, and the guest would divide by it.
        for cpu in 0..count as usize {
            let element = &before[cpu * 4..cpu * 4 + 4];
            assert!(
                element.iter().any(|w| *w != 0),
                "cpu{cpu} has four zero ticks, which is not a running processor"
            );
        }
        // And a moment later they are the same or larger: these are counters,
        // not instantaneous load.
        std::thread::sleep(std::time::Duration::from_millis(20));
        for (cpu, (was, now)) in before.iter().zip(first()).enumerate() {
            assert!(
                now >= *was,
                "cpu{} user tick went backwards, {was} to {now}",
                cpu / 4
            );
        }
    }

    /// The flavour the warning is about is refused, and the refusal does not
    /// write a `cpu_type` this tree made up. `HOST_PROCESSOR_BASIC_INFO`'s first
    /// two words are the Darwin ABI, and the message says so by name.
    #[test]
    fn basic_info_is_refused_and_writes_no_processor_type() {
        let mut count = 0xaaaa_aaaau32;
        let mut array: *mut u32 = 0xdead_beef as *mut u32;
        let mut words = 0xaaaa_aaaau32;
        // SAFETY: three live out-parameters.
        let rc = unsafe {
            host_processor_info(HOST_SELF, HOST_PROCESSOR_BASIC_INFO, &mut count, &mut array, &mut words)
        };
        assert_eq!(rc, KERN_INVALID_ARGUMENT);
        assert_eq!(count, 0xaaaa_aaaa, "the count was written");
        assert_eq!(array as usize, 0xdead_beef, "an array pointer was written");
        assert_eq!(words, 0xaaaa_aaaa, "the word count was written");

        // And an undefined flavour, and another host, are refused too - the
        // second with a different code, because a port that names nothing is
        // not the same mistake as a flavour nobody answers.
        // SAFETY: as above.
        assert_eq!(
            unsafe { host_processor_info(HOST_SELF, 99, &mut count, &mut array, &mut words) },
            KERN_INVALID_ARGUMENT
        );
        // SAFETY: as above.
        assert_eq!(
            unsafe { host_processor_info(HOST_SELF + 4, HOST_PROCESSOR_CPU_LOAD_INFO, &mut count, &mut array, &mut words) },
            KERN_INVALID_ARGUMENT
        );
        // SAFETY: as above; the null is what is being tested.
        assert_eq!(
            unsafe { host_processor_info(HOST_SELF, HOST_PROCESSOR_CPU_LOAD_INFO, std::ptr::null_mut(), &mut array, &mut words) },
            KERN_INVALID_ADDRESS
        );
    }

    /// `/proc/stat` is read in `cpu_ticks[]` order, which is **not** the order
    /// Linux writes. `user nice system idle` on this host has to come out as
    /// `user system idle nice`, and a mapping that quietly used the Linux order
    /// would report every CPU's system time as its idle time - plausible, and
    /// wrong in a way nothing downstream could detect.
    #[test]
    fn proc_stat_is_mapped_into_cpustate_order_not_its_own() {
        let stat = std::fs::read_to_string("/proc/stat").expect("a Linux host has /proc/stat");
        let line = stat
            .lines()
            .find_map(|l| l.strip_prefix("cpu0"))
            .expect("a host with one CPU has a cpu0 line");
        let fields: Vec<u64> = line
            .split_ascii_whitespace()
            .map(|f| f.parse().unwrap())
            .collect();
        assert!(fields.len() >= 4, "the line is shorter than the four states");
        let load = cpu_load();
        let first = load.first().expect("cpu_load found cpu0");
        assert_eq!(u64::from(first.user), fields[0], "user is field 0");
        assert_eq!(u64::from(first.nice), fields[1], "nice is field 1, not field 3");
        assert_eq!(u64::from(first.system), fields[2], "system is field 2, not field 1");
        assert_eq!(u64::from(first.idle), fields[3], "idle is field 3, not field 2");
        // And the aggregate `cpu ` line is not counted as a processor, which
        // would make the count one too many and every index off by one.
        assert_eq!(
            load.len(),
            stat.lines()
                .filter(|l| l.starts_with("cpu") && l[3..].starts_with(|c: char| c.is_ascii_digit()))
                .count()
        );
    }

    /// The names have to reach the guest as *addresses*, or none of the above is
    /// callable, and none of them may also be refused by family - `task_`,
    /// `host_` and `thread_policy_` are all denied prefixes, and the whole point
    /// of publishing here is to carve them back out.
    #[test]
    fn the_six_names_are_published_rather_than_left_to_the_host_or_the_family() {
        for name in [
            "thread_policy_set",
            "task_policy_get",
            "task_policy_set",
            "task_self_trap",
            "thread_switch",
            "host_processor_info",
        ] {
            assert!(crate::symbol(name).is_some(), "{name} is not published");
            assert!(
                !crate::divergent::contains(name),
                "{name} is still refused by its family, so a guest asking for it is turned away \
                 at the import"
            );
            assert_ne!(
                crate::symbol(name).expect("published, checked above"),
                crate::host_symbol(name).unwrap_or(0),
                "{name} would bind to the host's, which has no such function"
            );
        }
    }
}
