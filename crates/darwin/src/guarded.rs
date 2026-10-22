//! Guarded file descriptors, after XNU's `bsd/kern/kern_guarded.c`.
//!
//! A guard is a nonzero 64-bit id and a set of operations it forbids. Doing a
//! forbidden one is a guard exception, which ends the process, so a program
//! guards a descriptor it owns to catch code that closes or copies it by
//! mistake. The host has no guards, so this keeps them per descriptor and the
//! calls that can break one (`close`, `dup`, `dup2`, `fcntl`, `sendmsg` and
//! `fileport_makeport`) ask here first.

use std::collections::HashMap;
use std::sync::Mutex;

pub const GUARD_CLOSE: u32 = 1 << 0;
pub const GUARD_DUP: u32 = 1 << 1;
pub const GUARD_SOCKET_IPC: u32 = 1 << 2;
pub const GUARD_FILEPORT: u32 = 1 << 3;
pub const GUARD_WRITE: u32 = 1 << 4;
/// Every guard must forbid copying, or the copy would be an unguarded alias.
const REQUIRED: u32 = GUARD_DUP;
const ALL: u32 = GUARD_CLOSE | GUARD_DUP | GUARD_SOCKET_IPC | GUARD_FILEPORT | GUARD_WRITE;

/// `kGUARD_EXC_*`, the flavor a guard exception names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Close = 1 << 0,
    Dup = 1 << 1,
    NoCloexec = 1 << 2,
    SocketIpc = 1 << 3,
    Fileport = 1 << 4,
}

const FD_CLOEXEC: i32 = 1;
/// Darwin's private `FD_CLOFORK`, which `change_fdguard_np` reports.
const FD_CLOFORK: i32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Guard {
    id: u64,
    attrs: u32,
    /// Close-on-fork, which `GUARD_CLOSE` forces and the host cannot keep.
    clofork: bool,
    /// The file guarded, so a descriptor closed where this module could not
    /// see it and then reused is not taken for the guarded one.
    file: (u64, u64),
}

static GUARDS: Mutex<Option<HashMap<i32, Guard>>> = Mutex::new(None);

fn identity(fd: i32) -> Option<(u64, u64)> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: a writable `stat` for the host call to fill.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: filled by the successful call.
    let stat = unsafe { stat.assume_init() };
    Some((stat.st_dev as u64, stat.st_ino as u64))
}

/// The guard on `fd`, dropping one left from a file the descriptor no
/// longer names.
fn guard_of(fd: i32) -> Option<Guard> {
    let mut table = GUARDS.lock().unwrap_or_else(|e| e.into_inner());
    let guards = table.as_mut()?;
    let guard = *guards.get(&fd)?;
    if identity(fd) != Some(guard.file) {
        guards.remove(&fd);
        return None;
    }
    Some(guard)
}

fn set_guard(fd: i32, guard: Option<Guard>) {
    let mut table = GUARDS.lock().unwrap_or_else(|e| e.into_inner());
    let guards = table.get_or_insert_with(HashMap::new);
    match guard {
        Some(guard) => guards.insert(fd, guard),
        None => guards.remove(&fd),
    };
}

/// The descriptor is closed: its guard, if any, went with it.
pub(crate) fn forget(fd: i32) {
    if let Some(guards) = GUARDS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        guards.remove(&fd);
    }
}

/// The guard `attrs` on `fd` would forbid, if any: its id.
fn forbidding(fd: i32, attrs: u32) -> Option<u64> {
    guard_of(fd)
        .filter(|guard| guard.attrs & attrs == attrs)
        .map(|guard| guard.id)
}

/// End the process for breaking `fd`'s guard if it has one with `attrs`, as
/// `fp_guard_exception` does: the exception is fatal and is not a signal the
/// program can catch.
pub(crate) fn enforce(fd: i32, attrs: u32, flavor: Flavor) {
    if let Some(id) = forbidding(fd, attrs) {
        violation(fd, id, flavor);
    }
}

/// `F_SETFD` without `FD_CLOEXEC` on a guarded descriptor: every guard keeps
/// close-on-exec on.
pub(crate) fn enforce_cloexec(fd: i32, flags: i32) {
    if flags & FD_CLOEXEC == 0
        && let Some(guard) = guard_of(fd)
    {
        violation(fd, guard.id, Flavor::NoCloexec);
    }
}

fn violation(fd: i32, id: u64, flavor: Flavor) -> ! {
    eprintln!(
        "rosemary: EXC_GUARD (GUARD_TYPE_FD, {flavor:?}) on descriptor {fd}, guard {id:#x}: \
         a guarded descriptor was misused, which ends the process"
    );
    // SAFETY: signals this process; SIGKILL cannot be caught, as the
    // exception cannot.
    unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
    std::process::abort()
}

/// `fcntl(fd, F_GETFD)` on the host: whether close-on-exec is set, or `None`
/// for a descriptor that is not open.
fn cloexec(fd: i32) -> Option<bool> {
    // SAFETY: an integer descriptor and command.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    (flags >= 0).then_some(flags & libc::FD_CLOEXEC != 0)
}

fn set_cloexec(fd: i32, on: bool) {
    // SAFETY: an integer descriptor and command.
    unsafe { libc::fcntl(fd, libc::F_SETFD, if on { libc::FD_CLOEXEC } else { 0 }) };
}

fn fail(errno: i32) -> i32 {
    crate::errno::set(errno);
    -1
}

/// `int change_fdguard_np(int fd, const guardid_t *guard, u_int guardflags,
/// const guardid_t *nguard, u_int nguardflags, int *fdflagsp)`: guard an
/// unguarded descriptor, change a guard, or take one away - each only for a
/// caller that names the current guard and its flags exactly.
///
/// The old flags go out through `fdflagsp` before anything is checked, and
/// what it held on the way in is the close-on-exec and close-on-fork state a
/// removed guard leaves behind.
///
/// # Safety
///
/// Each pointer is null or valid for its type.
pub unsafe extern "C" fn change_fdguard_np(
    fd: i32,
    guard: *const u64,
    guardflags: u32,
    nguard: *const u64,
    nguardflags: u32,
    fdflagsp: *mut i32,
) -> i32 {
    // SAFETY: the caller's pointers, each read only when present.
    let (old, new, new_flags) = unsafe {
        (
            (!guard.is_null()).then(|| *guard).unwrap_or(0),
            (!nguard.is_null()).then(|| *nguard).unwrap_or(0),
            (!fdflagsp.is_null()).then(|| *fdflagsp).unwrap_or(0),
        )
    };
    let Some(cloexec_now) = cloexec(fd) else {
        return fail(libc::EBADF);
    };
    let current = guard_of(fd);
    if !fdflagsp.is_null() {
        let clofork = current.is_some_and(|g| g.clofork);
        let flags = if cloexec_now { FD_CLOEXEC } else { 0 } | if clofork { FD_CLOFORK } else { 0 };
        // SAFETY: the caller's writable `int`.
        unsafe { *fdflagsp = flags };
    }
    match current {
        Some(_) if guard.is_null() || guardflags == 0 => return fail(libc::EINVAL),
        Some(_) if old == 0 => return fail(libc::EPERM),
        None if !guard.is_null() || guardflags != 0 => return fail(libc::EINVAL),
        _ => {}
    }
    if !nguard.is_null() {
        if new == 0 || nguardflags & REQUIRED != REQUIRED || nguardflags & !ALL != 0 {
            return fail(libc::EINVAL);
        }
        let replaced = match current {
            Some(current) if current.id != old || current.attrs != guardflags => {
                return fail(libc::EPERM);
            }
            Some(current) => Guard {
                id: new,
                attrs: nguardflags,
                clofork: nguardflags & GUARD_CLOSE != 0 || new_flags & FD_CLOFORK != 0,
                file: current.file,
            },
            None => {
                let Some(file) = identity(fd) else {
                    return fail(libc::EBADF);
                };
                set_cloexec(fd, true);
                Guard {
                    id: new,
                    attrs: nguardflags,
                    clofork: nguardflags & GUARD_CLOSE != 0,
                    file,
                }
            }
        };
        set_guard(fd, Some(replaced));
        return 0;
    }
    match current {
        Some(_) if nguardflags != 0 => fail(libc::EINVAL),
        Some(current) if current.id != old || current.attrs != guardflags => fail(libc::EPERM),
        Some(_) => {
            set_guard(fd, None);
            set_cloexec(fd, new_flags & FD_CLOEXEC != 0);
            0
        }
        // Neither guarded nor being guarded.
        None => fail(libc::EINVAL),
    }
}

/// `int guarded_close_np(int fd, const guardid_t *guard)`: close a guarded
/// descriptor, for the caller holding its guard. `EINVAL` for one that is not
/// guarded and `EPERM` for the wrong guard - not an exception.
///
/// # Safety
///
/// `guard` is null or a readable `guardid_t`.
pub unsafe extern "C" fn guarded_close_np(fd: i32, guard: *const u64) -> i32 {
    if guard.is_null() {
        return fail(libc::EFAULT);
    }
    // SAFETY: the caller's readable guard.
    let id = unsafe { *guard };
    if cloexec(fd).is_none() {
        return fail(libc::EBADF);
    }
    match guard_of(fd) {
        None => fail(libc::EINVAL),
        Some(current) if current.id != id => fail(libc::EPERM),
        Some(_) => {
            forget(fd);
            // SAFETY: an integer descriptor; the guard it had is gone, so the
            // close below is not a violation of it.
            unsafe { crate::net::descriptor::close(fd) }
        }
    }
}

exports::exports! {
    functions {
        "change_fdguard_np" => change_fdguard_np,
        "guarded_close_np" => guarded_close_np,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe() -> (i32, i32) {
        let mut ends = [0i32; 2];
        // SAFETY: a writable two-element array.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        (ends[0], ends[1])
    }

    fn errno() -> i32 {
        crate::errno::get()
    }

    /// Guarding forces close-on-exec and reports the flags it found; the
    /// guard then forbids what it names, and comes off only for its holder.
    #[test]
    fn a_guard_is_added_checked_and_removed_by_its_holder() {
        let (read, write) = pipe();
        let id = 0x5eed_u64;
        let mut flags = -1;
        // SAFETY: live descriptors and pointers to locals.
        unsafe {
            assert_eq!(
                change_fdguard_np(
                    read,
                    std::ptr::null(),
                    0,
                    &id,
                    GUARD_CLOSE | GUARD_DUP,
                    &mut flags
                ),
                0
            );
            assert_eq!(flags, 0, "neither close-on-exec nor close-on-fork before");
            assert_eq!(cloexec(read), Some(true));
            assert_eq!(forbidding(read, GUARD_CLOSE), Some(id));
            assert_eq!(forbidding(read, GUARD_FILEPORT), None);
            assert_eq!(forbidding(write, GUARD_CLOSE), None);

            // The wrong guard is refused, and so is guarding it again.
            let wrong = 7u64;
            assert_eq!(
                change_fdguard_np(
                    read,
                    &wrong,
                    GUARD_CLOSE | GUARD_DUP,
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut()
                ),
                -1
            );
            assert_eq!(errno(), libc::EPERM);
            assert_eq!(
                change_fdguard_np(
                    read,
                    std::ptr::null(),
                    0,
                    &id,
                    GUARD_DUP,
                    std::ptr::null_mut()
                ),
                -1
            );
            assert_eq!(errno(), libc::EINVAL);
            assert_eq!(guarded_close_np(read, &wrong), -1);
            assert_eq!(errno(), libc::EPERM);

            // Removing it with flags of zero leaves close-on-exec off.
            let mut restore = 0;
            assert_eq!(
                change_fdguard_np(
                    read,
                    &id,
                    GUARD_CLOSE | GUARD_DUP,
                    std::ptr::null(),
                    0,
                    &mut restore
                ),
                0
            );
            assert_eq!(restore & FD_CLOEXEC, FD_CLOEXEC, "the flags while guarded");
            assert_eq!(cloexec(read), Some(false));
            assert_eq!(forbidding(read, GUARD_CLOSE), None);
            libc::close(read);
            libc::close(write);
        }
    }

    /// A guard needs `GUARD_DUP` and only known flags, and a nonzero id.
    #[test]
    fn a_guard_without_its_required_flags_is_invalid() {
        let (read, write) = pipe();
        let (id, zero) = (1u64, 0u64);
        // SAFETY: live descriptors and pointers to locals.
        unsafe {
            for (guard, flags) in [
                (&id, GUARD_CLOSE),
                (&id, GUARD_DUP | 1 << 9),
                (&zero, GUARD_DUP),
            ] {
                assert_eq!(
                    change_fdguard_np(
                        read,
                        std::ptr::null(),
                        0,
                        guard,
                        flags,
                        std::ptr::null_mut()
                    ),
                    -1
                );
                assert_eq!(errno(), libc::EINVAL);
            }
            assert_eq!(
                change_fdguard_np(
                    read,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut()
                ),
                -1,
                "unguarded with no new guard"
            );
            assert_eq!(guarded_close_np(read, &id), -1);
            assert_eq!(errno(), libc::EINVAL);
            libc::close(read);
            libc::close(write);
        }
    }

    /// The holder closes a guarded descriptor with `guarded_close_np`, and a
    /// guard is swapped only for one naming the old one exactly.
    #[test]
    fn a_guarded_descriptor_is_closed_and_reguarded_by_its_holder() {
        let (read, write) = pipe();
        let (first, second) = (11u64, 22u64);
        // SAFETY: live descriptors and pointers to locals.
        unsafe {
            assert_eq!(
                change_fdguard_np(
                    write,
                    std::ptr::null(),
                    0,
                    &first,
                    GUARD_DUP,
                    std::ptr::null_mut()
                ),
                0
            );
            assert_eq!(
                change_fdguard_np(
                    write,
                    &first,
                    GUARD_DUP | GUARD_CLOSE,
                    &second,
                    GUARD_DUP,
                    std::ptr::null_mut()
                ),
                -1,
                "flags must match too"
            );
            assert_eq!(errno(), libc::EPERM);
            assert_eq!(
                change_fdguard_np(
                    write,
                    &first,
                    GUARD_DUP,
                    &second,
                    GUARD_DUP | GUARD_CLOSE,
                    std::ptr::null_mut()
                ),
                0
            );
            assert_eq!(forbidding(write, GUARD_CLOSE), Some(second));
            assert_eq!(guarded_close_np(write, &second), 0);
            assert_eq!(cloexec(write), None, "closed");
            libc::close(read);
        }
    }

    /// The half of the test below that breaks a guard, in a process of its own.
    #[test]
    fn closing_a_guarded_descriptor_child() {
        if std::env::var_os("ROSEMARY_GUARD_CHILD").is_none() {
            return;
        }
        let (read, _write) = pipe();
        let id = 9u64;
        // SAFETY: a live descriptor and a pointer to a local.
        unsafe {
            change_fdguard_np(
                read,
                std::ptr::null(),
                0,
                &id,
                GUARD_DUP | GUARD_CLOSE,
                std::ptr::null_mut(),
            );
            crate::net::descriptor::close(read);
        }
        unreachable!("closing a guarded descriptor returned");
    }

    /// Breaking a guard ends the process with a signal it cannot catch.
    #[test]
    fn closing_a_guarded_descriptor_ends_the_process() {
        use std::os::unix::process::ExitStatusExt;
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "guarded::tests::closing_a_guarded_descriptor_child",
                "--nocapture",
            ])
            .env("ROSEMARY_GUARD_CHILD", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(status.status.signal(), Some(libc::SIGKILL));
        let said = String::from_utf8_lossy(&status.stderr);
        assert!(said.contains("EXC_GUARD (GUARD_TYPE_FD, Close)"), "{said}");
    }

    /// A descriptor closed behind this module's back and reused for another
    /// file is not still guarded.
    #[test]
    fn a_reused_descriptor_does_not_inherit_a_stale_guard() {
        let (read, write) = pipe();
        let id = 3u64;
        // SAFETY: live descriptors and pointers to locals.
        unsafe {
            assert_eq!(
                change_fdguard_np(
                    read,
                    std::ptr::null(),
                    0,
                    &id,
                    GUARD_DUP | GUARD_CLOSE,
                    std::ptr::null_mut()
                ),
                0
            );
            libc::close(read);
            let (again, other) = pipe();
            if again == read {
                assert_eq!(forbidding(again, GUARD_CLOSE), None);
            }
            libc::close(again);
            libc::close(other);
            libc::close(write);
        }
    }
}
