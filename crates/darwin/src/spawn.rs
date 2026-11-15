use std::ffi::{CStr, CString, VaList, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::signal::signal_to_host;

// All supported hosts export these; `libc` omits declarations on some of them.
unsafe extern "C" {
    fn posix_spawn_file_actions_addchdir_np(
        actions: *mut libc::posix_spawn_file_actions_t,
        path: *const c_char,
    ) -> c_int;

    fn posix_spawn_file_actions_addfchdir_np(
        actions: *mut libc::posix_spawn_file_actions_t,
        fd: c_int,
    ) -> c_int;
}

/// Darwin's `POSIX_SPAWN_*`. The first four agree with Linux; the last three do not,
/// and two of them *collide* with a different Linux flag of the same value - Darwin's
/// `SETEXEC` is Linux's `USEVFORK`, and `START_SUSPENDED` is `SETSID`. Passing the
/// word through unchanged would silently ask for the wrong thing.
/// The whole set is written out even though the last two are not acted on, because
/// the set is what makes the collisions visible: a name missing here is a bit that
/// would be forwarded by accident.
#[allow(dead_code)]
mod flag {
    pub const RESETIDS: u32 = 0x01;
    pub const SETPGROUP: u32 = 0x02;
    pub const SETSIGDEF: u32 = 0x04;
    pub const SETSIGMASK: u32 = 0x08;
    /// Replace the calling process rather than making a child.
    pub const SETEXEC: u32 = 0x40;
    /// Start the child stopped, for a debugger to attach to. Not honoured: nothing
    /// here is being debugged through a guest's spawn, and starting a crash reporter
    /// suspended would hang the program waiting for it.
    pub const START_SUSPENDED: u32 = 0x80;
    /// Close every descriptor across the exec except those the file actions name.
    /// Not honoured: the effect is to leak descriptors into the child that Darwin
    /// would have closed, which is visible only to a child that goes looking.
    pub const CLOEXEC_DEFAULT: u32 = 0x4000;
}

// Darwin can apply spawn actions atomically while replacing the caller.
#[cfg(target_os = "macos")]
const HOST_SETEXEC: libc::c_short = libc::POSIX_SPAWN_SETEXEC as libc::c_short;
#[cfg(not(target_os = "macos"))]
const HOST_SETEXEC: libc::c_short = 0;

/// One entry of a `posix_spawn_file_actions_t`, in the order it was added.
/// Paths are translated to the host namespace when the action is recorded.
enum Action {
    Open {
        fd: c_int,
        path: CString,
        oflag: c_int,
        mode: libc::mode_t,
    },
    Dup2 {
        from: c_int,
        to: c_int,
    },
    Close(c_int),
    Chdir(CString),
    Fchdir(c_int),
    /// `addinherit_np`: keep this descriptor open across the spawn. The host's
    /// equivalent is simply not marking it close-on-exec, which a `dup2` onto itself
    /// achieves - see [`FileActions::to_host`].
    Inherit(c_int),
}

#[derive(Default)]
struct FileActions {
    actions: Vec<Action>,
}

#[derive(Default)]
struct Attributes {
    flags: u32,
    pgroup: libc::pid_t,
    /// Darwin's `sigset_t` is one 32-bit word, so both of these are held as the
    /// guest's word and widened only when a host object is built.
    sigdefault: u32,
    sigmask: u32,
}

/// Read the pointer a guest's `posix_spawn*_t` holds.
///
/// Darwin's type is `void *`, which is what makes this a plain load: the guest's eight
/// bytes hold whichever object `..._init` allocated, and never anything else.
///
/// # Safety
///
/// `at` is null or a readable `void *`.
unsafe fn object<T>(at: *mut c_void) -> Option<&'static mut T> {
    if at.is_null() {
        return None;
    }
    let stored = unsafe { *(at as *const u64) };
    if stored == 0 {
        return None;
    }
    // SAFETY: the pointer this module's `..._init` stored, still owned by the guest's
    // variable and not yet destroyed.
    Some(unsafe { &mut *(stored as *mut T) })
}

/// Allocate `T` and store it in the guest's `void *`.
///
/// # Safety
///
/// `at` is a writable `void *`.
unsafe fn init<T: Default>(at: *mut c_void) -> c_int {
    if at.is_null() {
        return libc::EINVAL;
    }
    let boxed = Box::into_raw(Box::new(T::default())) as u64;
    unsafe { *(at as *mut u64) = boxed };
    0
}

/// Free what [`init`] allocated and leave the guest's word null.
///
/// # Safety
///
/// As [`init`], and the object is this module's own.
unsafe fn destroy<T>(at: *mut c_void) -> c_int {
    if at.is_null() {
        return libc::EINVAL;
    }
    let stored = unsafe { *(at as *const u64) };
    if stored == 0 {
        return libc::EINVAL;
    }
    // SAFETY: this module allocated it, and the store below stops a second free.
    drop(unsafe { Box::from_raw(stored as *mut T) });
    unsafe { *(at as *mut u64) = 0 };
    0
}

/// The environment for a rewritten exec.
///
/// The child is another copy of *this runtime*, and it resolves its prefix and
/// its own host paths before it builds a guest environment of its own. Under
/// the builtin libc the two never mix: the child gets this process's own
/// environment, and the guest's block beside it as a hand-off (see
/// [`crate::env::hand_off`]) that becomes the child guest's, verbatim.
///
/// The one crossing is what [`crate::env::FORWARD_VAR`] allows, so that a
/// guest shell's `ROSEMARY_STATS=1 cmd` can configure the runtime it launches
/// when the user asked for that.
///
/// # Safety
///
/// `envp` is null or a null-terminated array of NUL-terminated strings.
unsafe fn runtime_environ(envp: *const *const c_char) -> Vec<CString> {
    if crate::libc_backend::using_builtin() {
        let mut out = host_environ();
        for entry in unsafe { argv_of(envp) } {
            let bytes = entry.as_bytes();
            let Some(split) = bytes.iter().position(|&b| b == b'=') else {
                continue;
            };
            let name = &bytes[..split];
            if !crate::env::is_forwarded(name) {
                continue;
            }
            let existing = out.iter().position(|e| {
                let e = e.as_bytes();
                e.starts_with(name) && e.get(name.len()) == Some(&b'=')
            });
            match existing {
                Some(at) => out[at] = entry,
                None => out.push(entry),
            }
        }
        out.extend(unsafe { crate::env::hand_off(envp) });
        return out;
    }
    let mut out = unsafe { argv_of(envp) };
    keep_runtime_settings(&mut out);
    out
}

/// This process's environment, without the hand-off it may itself have been
/// given, which is its parent guest's and not the child's.
fn host_environ() -> Vec<CString> {
    use std::os::unix::ffi::OsStrExt;
    std::env::vars_os()
        .filter(|(name, _)| !crate::env::is_hand_off(name.as_bytes()))
        .filter_map(|(name, value)| {
            CString::new([name.as_bytes(), b"=", value.as_bytes()].concat()).ok()
        })
        .collect()
}

/// Put back what this runtime was started with if the guest dropped it.
///
/// A guest that scrubs its environment before an `exec` - `sudo`'s
/// `env_reset`, `env -i`, `login` - means to scrub *its* variables, and has no
/// idea the program it runs is this runtime again. Without its `ROSEMARY_*`
/// settings the child would pick its own prefix, and without
/// `XDG_RUNTIME_DIR` it would look for a different process server - one that
/// knows nothing of who its parent was, so a `sudo`'d command would run as the
/// user again. What the guest set itself, it keeps.
fn keep_runtime_settings(out: &mut Vec<CString>) {
    use std::os::unix::ffi::OsStrExt;
    for (name, value) in std::env::vars_os() {
        let name = name.as_bytes();
        // A hand-off this process was given is its parent's, not the child's.
        let handed_off = crate::env::is_hand_off(name);
        let ours = (name.starts_with(b"ROSEMARY_") || name == b"XDG_RUNTIME_DIR") && !handed_off;
        let kept = out.iter().any(|entry| {
            let entry = entry.as_bytes();
            entry.starts_with(name) && entry.get(name.len()) == Some(&b'=')
        });
        if ours
            && !kept
            && let Ok(entry) = CString::new([name, b"=", value.as_bytes()].concat())
        {
            out.push(entry);
        }
    }
}

/// The host path an exec of the guest's `given` opens.
///
/// Through the namespace like every other file call, retried with the
/// case-insensitive reading when nothing is there - and never the path as
/// given. A host program is reachable at `/Volumes/Host` like any other host
/// file; a guest naming `/usr/bin/foo` means the prefix's, and being handed
/// this machine's instead is a different program with the same name. See
/// `docs/prefixes.md`.
fn exec_path(given: &CStr) -> CString {
    let host = prefix::ns::to_host(given);
    // SAFETY: a NUL-terminated path, and `access` only reads it.
    if unsafe { libc::access(host.as_ptr(), libc::F_OK) } == 0 {
        return host.into_owned();
    }
    prefix::ns::fold(&host).unwrap_or_else(|| host.into_owned())
}

/// Recognise thin and fat Mach-O images by their magic, in either byte order.
pub(crate) fn is_mach_o(path: &CStr) -> bool {
    let Ok(text) = path.to_str() else {
        return false;
    };
    let Ok(mut file) = std::fs::File::open(text) else {
        return false;
    };
    use std::io::Read;
    let mut magic = [0u8; 4];
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    matches!(
        u32::from_le_bytes(magic),
        // 64- and 32-bit thin images, and the same two byte-swapped.
        0xfeed_facf | 0xfeed_face | 0xcffa_edfe | 0xcefa_edfe
        // A fat archive, whose header is big-endian by definition.
        | 0xcafe_babe | 0xbebafeca | 0xcafe_babf | 0xbfba_feca
    )
}

/// This runtime's own executable, which is what a Mach-O spawn is re-pointed at.
fn runtime_path() -> Option<CString> {
    let exe = crate::exe::current().ok()?;
    CString::new(exe.into_os_string().into_encoded_bytes()).ok()
}

/// A NUL-terminated argument vector as a `Vec`, keeping the guest's strings.
///
/// # Safety
///
/// `argv` is null or a null-terminated array of C strings.
unsafe fn argv_of(argv: *const *const c_char) -> Vec<CString> {
    let mut out = Vec::new();
    if argv.is_null() {
        return out;
    }
    let mut index = 0;
    loop {
        // SAFETY: the caller states the array is null-terminated.
        let entry = unsafe { *argv.add(index) };
        if entry.is_null() {
            return out;
        }
        // SAFETY: an element of a valid argv is a NUL-terminated string.
        out.push(unsafe { CStr::from_ptr(entry) }.to_owned());
        index += 1;
    }
}

/// Whether the driver explicitly requested emulation for this process.
static EMULATED: AtomicBool = AtomicBool::new(false);

/// Preserve an explicit `--emulated` choice in child Rosemary launches.
///
/// Set by the driver before loading guest code. Automatic backend selection
/// remains automatic for children, which may have different Mach-O slices.
pub fn set_emulated(emulated: bool) {
    EMULATED.store(emulated, Ordering::Relaxed);
}

/// The runtime's arguments for a child, without the guest's `argv[0]`.
fn runtime_argv(runtime: &CStr, path: &CStr, argv: &[CString], emulated: bool) -> Vec<CString> {
    let mut rewritten = vec![runtime.to_owned(), c"run".to_owned()];
    if emulated {
        rewritten.push(c"--emulated".to_owned());
    }
    rewritten.push(path.to_owned());
    rewritten.extend(argv.iter().skip(1).cloned());
    rewritten
}

/// Re-execute this runtime for a Mach-O, using the guest spelling of its path.
fn rewrite(path: &CStr, on_disk: &CStr, argv: &[CString]) -> Option<(CString, Vec<CString>)> {
    if !is_mach_o(on_disk) {
        return None;
    }
    let runtime = runtime_path()?;
    let rewritten = runtime_argv(&runtime, path, argv, EMULATED.load(Ordering::Relaxed));
    Some((runtime, rewritten))
}

/// Darwin's `_PATH_BSHELL`, which `execvp` hands a script with no `#!` line.
const BSHELL: &CStr = c"/bin/sh";

static SYSTEM_SIGNALS: Mutex<()> = Mutex::new(());

/// Save the caller's signal state while `system` waits for its shell.
struct SystemSignals {
    _lock: MutexGuard<'static, ()>,
    mask: libc::sigset_t,
    int: libc::sigaction,
    quit: libc::sigaction,
}

impl SystemSignals {
    fn new() -> Result<Self, c_int> {
        let lock = SYSTEM_SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: these are plain C signal structures, initialised by the calls below.
        let (mut blocked, mut mask, mut ignore, mut int, mut quit) = unsafe {
            (
                std::mem::zeroed::<libc::sigset_t>(),
                std::mem::zeroed::<libc::sigset_t>(),
                std::mem::zeroed::<libc::sigaction>(),
                std::mem::zeroed::<libc::sigaction>(),
                std::mem::zeroed::<libc::sigaction>(),
            )
        };
        // SAFETY: every pointer is to a live host signal structure.
        unsafe {
            libc::sigemptyset(&mut blocked);
            libc::sigaddset(&mut blocked, libc::SIGCHLD);
            let rc = libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut mask);
            if rc != 0 {
                return Err(rc);
            }
            ignore.sa_sigaction = libc::SIG_IGN;
            libc::sigemptyset(&mut ignore.sa_mask);
            if libc::sigaction(libc::SIGINT, &ignore, &mut int) != 0 {
                let error = crate::errno::get();
                libc::pthread_sigmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut());
                return Err(error);
            }
            if libc::sigaction(libc::SIGQUIT, &ignore, &mut quit) != 0 {
                let error = crate::errno::get();
                libc::sigaction(libc::SIGINT, &int, std::ptr::null_mut());
                libc::pthread_sigmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut());
                return Err(error);
            }
        }
        Ok(Self {
            _lock: lock,
            mask,
            int,
            quit,
        })
    }

    fn child_attr(&self) -> Result<HostAttr, c_int> {
        let mut attr = HostAttr::new()?;
        // The child gets the original mask and the original ignored dispositions.
        // Signals temporarily ignored only by this call return to their defaults.
        let mut defaults = host_sigset(0);
        // SAFETY: both signal sets and the spawn attribute are initialised locals.
        unsafe {
            if self.int.sa_sigaction != libc::SIG_IGN {
                libc::sigaddset(&mut defaults, libc::SIGINT);
            }
            if self.quit.sa_sigaction != libc::SIG_IGN {
                libc::sigaddset(&mut defaults, libc::SIGQUIT);
            }
            for rc in [
                libc::posix_spawnattr_setflags(
                    &mut attr.0,
                    (libc::POSIX_SPAWN_SETSIGMASK | libc::POSIX_SPAWN_SETSIGDEF) as libc::c_short,
                ),
                libc::posix_spawnattr_setsigmask(&mut attr.0, &self.mask),
                libc::posix_spawnattr_setsigdefault(&mut attr.0, &defaults),
            ] {
                if rc != 0 {
                    return Err(rc);
                }
            }
        }
        Ok(attr)
    }
}

impl Drop for SystemSignals {
    fn drop(&mut self) {
        // SAFETY: these are the dispositions and thread mask saved by `new`.
        unsafe {
            libc::sigaction(libc::SIGQUIT, &self.quit, std::ptr::null_mut());
            libc::sigaction(libc::SIGINT, &self.int, std::ptr::null_mut());
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.mask, std::ptr::null_mut());
        }
    }
}

/// What an exec of the guest's `path` really runs.
struct Launch {
    /// The host path to execute.
    program: CString,
    /// The argument vector, or `None` to pass the guest's own.
    argv: Option<Vec<CString>>,
    /// Whether `program` is this runtime, which wants the host spelling of
    /// the environment - see [`runtime_environ`].
    runtime: bool,
}

/// The `#!` line of the file at the host path `on_disk`: the interpreter as
/// the script spells it, and the one optional argument.
///
/// Read here rather than left to the kernel, because the interpreter is a
/// guest path - `#!/bin/sh` inside a prefix means the prefix's - and it may
/// be a Mach-O the kernel cannot run.
pub fn interpreter(on_disk: &CStr) -> Option<(CString, Option<CString>)> {
    use std::io::Read;
    let mut head = [0u8; 512];
    let mut file = std::fs::File::open(on_disk.to_str().ok()?).ok()?;
    let n = file.read(&mut head).ok()?;
    let line = head[..n].strip_prefix(b"#!")?;
    let line = &line[..line.iter().position(|&b| b == b'\n')?];
    let is_space = |b: &u8| *b == b' ' || *b == b'\t';
    let start = line.iter().position(|b| !is_space(b))?;
    let line = &line[start..];
    let end = line.iter().position(is_space).unwrap_or(line.len());
    let program = CString::new(&line[..end]).ok()?;
    // Everything after the interpreter is one argument, as both kernels have it.
    let rest = line[end..].trim_ascii();
    let argument = match rest.is_empty() {
        true => None,
        false => Some(CString::new(rest).ok()?),
    };
    Some((program, argument))
}

/// Plan the exec of `path` with `argv`: this runtime for a Mach-O, the
/// translated interpreter for a script, the translated file otherwise.
fn launch(path: &CStr, argv: &[CString]) -> Launch {
    let on_disk = exec_path(path);
    if let Some((runtime, rewritten)) = rewrite(path, &on_disk, argv) {
        return Launch {
            program: runtime,
            argv: Some(rewritten),
            runtime: true,
        };
    }
    let Some((program, argument)) = interpreter(&on_disk) else {
        return Launch {
            program: on_disk,
            argv: None,
            runtime: false,
        };
    };
    // `interpreter [argument] script args...`, with the script named the way
    // the guest named it, so an interpreter that is a guest translates it itself.
    let mut script_argv = vec![program.clone()];
    script_argv.extend(argument);
    let script_at = script_argv.len();
    script_argv.push(path.to_owned());
    script_argv.extend(argv.iter().skip(1).cloned());
    let program_on_disk = exec_path(&program);
    if let Some((runtime, rewritten)) = rewrite(&program, &program_on_disk, &script_argv) {
        return Launch {
            program: runtime,
            argv: Some(rewritten),
            runtime: true,
        };
    }
    // A host interpreter reads the script from this host's filesystem.
    script_argv[script_at] = on_disk;
    Launch {
        program: program_on_disk,
        argv: Some(script_argv),
        runtime: false,
    }
}

/// What `execvp` retries a file the kernel will not execute with:
/// `sh path args...`, as Darwin's does.
fn shell_argv(path: &CStr, argv: &[CString]) -> Vec<CString> {
    let mut out = vec![c"sh".to_owned(), path.to_owned()];
    out.extend(argv.iter().skip(1).cloned());
    out
}

/// Borrow a `Vec<CString>` as the null-terminated `char *const *` an exec wants.
fn as_argv(strings: &[CString]) -> Vec<*const c_char> {
    let mut raw: Vec<*const c_char> = strings.iter().map(|s| s.as_ptr()).collect();
    raw.push(std::ptr::null());
    raw
}

/// `int execve(const char *path, char *const argv[], char *const envp[])`.
///
/// # Safety
///
/// The three arguments are as `execve` declares them.
pub unsafe extern "C" fn execve(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    if path.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let (given, arguments) = unsafe { (CStr::from_ptr(path), argv_of(argv)) };
    let plan = launch(given, &arguments);
    let raw = plan.argv.as_deref().map(as_argv);
    let argv = raw.as_ref().map_or(argv, |raw| raw.as_ptr());
    let environment = match plan.runtime {
        // SAFETY: the caller's obligation for `envp`.
        true => Some(unsafe { runtime_environ(envp) }),
        false => None,
    };
    let environment = environment.as_deref().map(as_argv);
    let envp = environment.as_ref().map_or(envp, |e| e.as_ptr());
    // SAFETY: every string and array outlives the call, and each array is
    // null-terminated. On success this does not return.
    unsafe { libc::execve(plan.program.as_ptr(), argv, envp) }
}

/// Gather C string pointers through the terminating null argument.
///
/// # Safety
///
/// `first` and every argument through the null are C string pointers.
unsafe fn variadic_argv(
    first: *const c_char,
    args: &mut std::ffi::VaList<'_>,
) -> Vec<*const c_char> {
    let mut argv = vec![first];
    let mut argument = first;
    while !argument.is_null() {
        // SAFETY: the caller supplies pointers through a terminating null.
        argument = unsafe { args.next_arg::<*const c_char>() };
        argv.push(argument);
    }
    argv
}

// The argument lists after `first`, which is what each `...` reads.
unsafe extern "C" fn execl_list(
    path: *const c_char,
    first: *const c_char,
    mut args: VaList,
) -> c_int {
    let argv = unsafe { variadic_argv(first, &mut args) };
    // SAFETY: the vector holds the caller's strings and terminating null.
    unsafe { execv(path, argv.as_ptr()) }
}

unsafe extern "C" fn execlp_list(
    file: *const c_char,
    first: *const c_char,
    mut args: VaList,
) -> c_int {
    let argv = unsafe { variadic_argv(first, &mut args) };
    unsafe { execvp(file, argv.as_ptr()) }
}

unsafe extern "C" fn execle_list(
    path: *const c_char,
    first: *const c_char,
    mut args: VaList,
) -> c_int {
    let argv = unsafe { variadic_argv(first, &mut args) };
    // SAFETY: `execle` places its environment after the terminating null.
    let envp = unsafe { args.next_arg::<*const *const c_char>() };
    unsafe { execve(path, argv.as_ptr(), envp) }
}

crate::variadic! {
    /// `int execl(const char *path, const char *arg, ..., NULL)`.
    ///
    /// # Safety
    ///
    /// `path` and arguments are C strings, followed by a null pointer.
    pub fn execl(path: *const c_char, first: *const c_char) -> c_int => execl_list;
    /// `int execlp(const char *file, const char *arg, ..., NULL)`.
    ///
    /// # Safety
    ///
    /// As [`execl`].
    pub fn execlp(file: *const c_char, first: *const c_char) -> c_int => execlp_list;
    /// `int execle(const char *path, const char *arg, ..., NULL, char *const envp[])`.
    ///
    /// # Safety
    ///
    /// As [`execl`], with a null-terminated environment vector after the null argument.
    pub fn execle(path: *const c_char, first: *const c_char) -> c_int => execle_list;
}

/// `pid_t vfork(void)`, as `fork`, which POSIX allows.
///
/// A `vfork` child borrows the parent's memory and stack until it execs. Here
/// the child runs this runtime's own code before the exec - the Mach-O rewrite
/// in [`execve`] allocates - and, emulated, the recompiler's too, all of it on
/// the parent's stack and heap. A copy costs a little and cannot corrupt the
/// parent.
pub extern "C" fn vfork() -> libc::pid_t {
    // SAFETY: `fork` takes nothing; the caller handles both returns, as it
    // would `vfork`'s.
    unsafe { libc::fork() }
}

/// `int execv(const char *path, char *const argv[])`.
///
/// # Safety
///
/// As [`execve`].
pub unsafe extern "C" fn execv(path: *const c_char, argv: *const *const c_char) -> c_int {
    // SAFETY: `environ` is this process's own, and the caller's guarantee for the rest.
    unsafe { execve(path, argv, environ()) }
}

/// `int execvp(const char *file, char *const argv[])`, which searches `PATH`.
///
/// The guest's `PATH`, searched in the guest's namespace - the host's own
/// `execvp` would search this process's `PATH` on this host's disk. A file
/// the kernel will not execute is handed to `/bin/sh`, as Darwin's does.
///
/// # Safety
///
/// As [`execve`].
pub unsafe extern "C" fn execvp(file: *const c_char, argv: *const *const c_char) -> c_int {
    if file.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let name = unsafe { CStr::from_ptr(file) };
    let found = match search_path(name) {
        Ok(found) => found,
        Err(errno) => {
            crate::errno::set(errno);
            return -1;
        }
    };
    // SAFETY: `found` outlives the call, and the caller's guarantee for argv.
    let rc = unsafe { execve(found.as_ptr(), argv, environ()) };
    if crate::errno::get() != libc::ENOEXEC {
        return rc;
    }
    let shell = shell_argv(&found, &unsafe { argv_of(argv) });
    let shell = as_argv(&shell);
    unsafe { execve(BSHELL.as_ptr(), shell.as_ptr(), environ()) }
}

/// The guest's `PATH`, or Darwin's `_PATH_DEFPATH` when it has none.
fn guest_path_variable() -> String {
    #[cfg(target_os = "macos")]
    if !crate::libc_backend::using_builtin() {
        return std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
    }
    // SAFETY: a literal name; the answer is null or the guest's own string.
    let value = unsafe {
        let p = crate::env::getenv(c"PATH".as_ptr());
        (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
    };
    value.unwrap_or_else(|| "/usr/bin:/bin".to_string())
}

/// `PATH`'s answer for `name`, as the guest spells it, or the error an exec
/// of it reports: `ENOENT` for nothing there, `EACCES` for something there
/// that may not be executed. A name with a slash in it is its own answer.
fn search_path(name: &CStr) -> Result<CString, c_int> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = name.to_bytes();
    if bytes.is_empty() {
        return Err(libc::ENOENT);
    }
    if bytes.contains(&b'/') {
        return Ok(name.to_owned());
    }
    let mut error = libc::ENOENT;
    for directory in guest_path_variable().split(':') {
        // An empty entry is the current directory.
        let directory = if directory.is_empty() { "." } else { directory };
        let mut candidate = directory.trim_end_matches('/').as_bytes().to_vec();
        candidate.push(b'/');
        candidate.extend_from_slice(bytes);
        let Ok(candidate) = CString::new(candidate) else {
            continue;
        };
        let on_disk = exec_path(&candidate);
        let Ok(meta) = std::fs::metadata(std::ffi::OsStr::from_bytes(on_disk.as_bytes())) else {
            continue;
        };
        // SAFETY: a NUL-terminated path, and `access` only reads it.
        if meta.is_file() && unsafe { libc::access(on_disk.as_ptr(), libc::X_OK) } == 0 {
            return Ok(candidate);
        }
        error = libc::EACCES;
    }
    Err(error)
}

/// The selected guest libc's environment for implicit `exec` arguments.
fn environ() -> *const *const c_char {
    // SAFETY: the selected libc publishes a live, null-terminated vector.
    unsafe { *crate::env::guest_environ_slot() as *const *const c_char }
}

impl FileActions {
    /// Build the host object these actions describe.
    ///
    /// Returned by value with its own destructor, because a host
    /// `posix_spawn_file_actions_t` owns heap the caller has to give back.
    fn to_host(&self) -> Result<HostActions, c_int> {
        let mut host = HostActions::new()?;
        for action in &self.actions {
            // SAFETY: `host.0` is initialised, and every path outlives the call.
            let rc = unsafe {
                match action {
                    Action::Open {
                        fd,
                        path,
                        oflag,
                        mode,
                    } => {
                        // The guest's `O_*` are Darwin's, which are not the host's.
                        let oflag =
                            crate::fcntl::flags_to_host(*oflag).map_err(|_| libc::EINVAL)?;
                        libc::posix_spawn_file_actions_addopen(
                            &mut host.0,
                            *fd,
                            path.as_ptr(),
                            oflag,
                            *mode,
                        )
                    }
                    Action::Dup2 { from, to } => {
                        libc::posix_spawn_file_actions_adddup2(&mut host.0, *from, *to)
                    }
                    Action::Close(fd) => libc::posix_spawn_file_actions_addclose(&mut host.0, *fd),
                    Action::Chdir(path) => {
                        posix_spawn_file_actions_addchdir_np(&mut host.0, path.as_ptr())
                    }
                    Action::Fchdir(fd) => posix_spawn_file_actions_addfchdir_np(&mut host.0, *fd),
                    // A descriptor duplicated onto itself survives the exec with its
                    // close-on-exec bit cleared, which is what "inherit" asks for.
                    Action::Inherit(fd) => {
                        libc::posix_spawn_file_actions_adddup2(&mut host.0, *fd, *fd)
                    }
                }
            };
            if rc != 0 {
                return Err(rc);
            }
        }
        Ok(host)
    }
}

/// A host `posix_spawn_file_actions_t` that destroys itself.
struct HostActions(libc::posix_spawn_file_actions_t);

impl HostActions {
    fn new() -> Result<HostActions, c_int> {
        // SAFETY: `posix_spawn_file_actions_init` fills the object it is given.
        let mut raw = unsafe { std::mem::zeroed::<libc::posix_spawn_file_actions_t>() };
        // SAFETY: a live local of the right type.
        match unsafe { libc::posix_spawn_file_actions_init(&mut raw) } {
            0 => Ok(HostActions(raw)),
            error => Err(error),
        }
    }
}

impl Drop for HostActions {
    fn drop(&mut self) {
        // SAFETY: initialised by `new` and destroyed exactly once.
        unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
    }
}

/// A host `posix_spawnattr_t` that destroys itself.
struct HostAttr(libc::posix_spawnattr_t);

impl HostAttr {
    fn new() -> Result<HostAttr, c_int> {
        // SAFETY: `posix_spawnattr_init` fills the object it is given.
        let mut raw = unsafe { std::mem::zeroed::<libc::posix_spawnattr_t>() };
        // SAFETY: a live local of the right type.
        match unsafe { libc::posix_spawnattr_init(&mut raw) } {
            0 => Ok(HostAttr(raw)),
            error => Err(error),
        }
    }
}

impl Drop for HostAttr {
    fn drop(&mut self) {
        // SAFETY: initialised by `new` and destroyed exactly once.
        unsafe { libc::posix_spawnattr_destroy(&mut self.0) };
    }
}

/// A Darwin `sigset_t` - one 32-bit word - as the host's.
fn host_sigset(darwin: u32) -> libc::sigset_t {
    // SAFETY: a `sigset_t` is a plain bit set; `sigemptyset` initialises it below.
    let mut set = unsafe { std::mem::zeroed::<libc::sigset_t>() };
    // SAFETY: a live local.
    unsafe { libc::sigemptyset(&mut set) };
    for bit in 0..32 {
        if darwin & (1 << bit) == 0 {
            continue;
        }
        // Darwin numbers signals from one, and bit zero is signal one.
        if let Some(host) = signal_to_host(bit + 1) {
            // SAFETY: a live local and a signal number the translation vouched for.
            unsafe { libc::sigaddset(&mut set, host) };
        }
    }
    set
}

impl Attributes {
    fn to_host(&self) -> Result<HostAttr, c_int> {
        let mut host = HostAttr::new()?;
        // Other Darwin flags collide with different host flags; see [`flag`].
        let mut host_flags: libc::c_short = 0;
        for (darwin, host_flag) in [
            (flag::RESETIDS, libc::POSIX_SPAWN_RESETIDS),
            (flag::SETPGROUP, libc::POSIX_SPAWN_SETPGROUP),
            (flag::SETSIGDEF, libc::POSIX_SPAWN_SETSIGDEF),
            (flag::SETSIGMASK, libc::POSIX_SPAWN_SETSIGMASK),
        ] {
            if self.flags & darwin != 0 {
                host_flags |= host_flag as libc::c_short;
            }
        }
        if self.flags & flag::SETEXEC != 0 {
            host_flags |= HOST_SETEXEC;
        }
        // SAFETY: `host.0` is initialised and every argument is a live local.
        unsafe {
            let rc = libc::posix_spawnattr_setflags(&mut host.0, host_flags);
            if rc != 0 {
                return Err(rc);
            }
            if self.flags & flag::SETPGROUP != 0 {
                let rc = libc::posix_spawnattr_setpgroup(&mut host.0, self.pgroup);
                if rc != 0 {
                    return Err(rc);
                }
            }
            if self.flags & flag::SETSIGMASK != 0 {
                let set = host_sigset(self.sigmask);
                let rc = libc::posix_spawnattr_setsigmask(&mut host.0, &set);
                if rc != 0 {
                    return Err(rc);
                }
            }
            if self.flags & flag::SETSIGDEF != 0 {
                let set = host_sigset(self.sigdefault);
                let rc = libc::posix_spawnattr_setsigdefault(&mut host.0, &set);
                if rc != 0 {
                    return Err(rc);
                }
            }
        }
        Ok(host)
    }
}

/// `int posix_spawn(pid_t *, const char *, const posix_spawn_file_actions_t *,
/// const posix_spawnattr_t *, char *const argv[], char *const envp[])`.
///
/// # Safety
///
/// Every argument is as `posix_spawn` declares it.
pub unsafe extern "C" fn posix_spawn(
    pid: *mut libc::pid_t,
    path: *const c_char,
    file_actions: *mut c_void,
    attrp: *mut c_void,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    if path.is_null() {
        return libc::EFAULT;
    }
    let (given, arguments) = unsafe { (CStr::from_ptr(path), argv_of(argv)) };
    // SAFETY: both are null or this module's own objects.
    let (actions, attributes) = unsafe {
        (
            object::<FileActions>(file_actions),
            object::<Attributes>(attrp),
        )
    };

    // Use the host's SETEXEC when available so it applies the actions too.
    // Other hosts replace the caller through the same exec rewrite.
    if HOST_SETEXEC == 0
        && attributes
            .as_ref()
            .is_some_and(|a| a.flags & flag::SETEXEC != 0)
    {
        unsafe { execve(path, argv, envp) };
        return crate::errno::from_host(crate::errno::get());
    }

    let host_actions = match actions {
        Some(actions) => match actions.to_host() {
            Ok(host) => Some(host),
            Err(error) => return crate::errno::from_host(error),
        },
        None => None,
    };
    let host_attr = match attributes {
        Some(attributes) => match attributes.to_host() {
            Ok(host) => Some(host),
            Err(error) => return crate::errno::from_host(error),
        },
        None => None,
    };
    // SAFETY: the caller's strings and the translated objects outlive the call.
    crate::errno::from_host(unsafe {
        spawn_host(
            pid,
            given,
            &arguments,
            envp,
            host_actions.as_ref(),
            host_attr.as_ref(),
        )
    })
}

/// Launch through the same rewrite for guest spawn, framework tasks and `system`.
///
/// # Safety
///
/// `pid` is writable and `envp` is a null-terminated environment vector or null.
unsafe fn spawn_host(
    pid: *mut libc::pid_t,
    path: &CStr,
    arguments: &[CString],
    envp: *const *const c_char,
    actions: Option<&HostActions>,
    attributes: Option<&HostAttr>,
) -> c_int {
    let plan = launch(path, arguments);
    let argv = as_argv(plan.argv.as_deref().unwrap_or(arguments));
    let environment = match plan.runtime {
        // SAFETY: the caller's obligation for `envp`.
        true => Some(unsafe { runtime_environ(envp) }),
        false => None,
    };
    let environment = environment.as_deref().map(as_argv);
    let envp = environment.as_ref().map_or(envp, |e| e.as_ptr());
    // SAFETY: all objects, strings and arrays outlive the call.
    unsafe {
        libc::posix_spawn(
            pid,
            plan.program.as_ptr(),
            actions.map_or(std::ptr::null(), |a| &a.0),
            attributes.map_or(std::ptr::null(), |a| &a.0),
            argv.as_ptr() as *const *mut c_char,
            envp as *const *mut c_char,
        )
    }
}

/// `int posix_spawnp(...)`, which searches `PATH` for the executable.
///
/// # Safety
///
/// As [`posix_spawn`].
pub unsafe extern "C" fn posix_spawnp(
    pid: *mut libc::pid_t,
    file: *const c_char,
    file_actions: *mut c_void,
    attrp: *mut c_void,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    if file.is_null() {
        return libc::EFAULT;
    }
    let name = unsafe { CStr::from_ptr(file) };
    // The guest's `PATH` in the guest's namespace, as for [`execvp`].
    let found = match search_path(name) {
        Ok(found) => found,
        Err(errno) => return errno,
    };
    // SAFETY: `found` outlives the call, and the caller's guarantee for the rest.
    let rc = unsafe { posix_spawn(pid, found.as_ptr(), file_actions, attrp, argv, envp) };
    if rc != crate::errno::from_host(libc::ENOEXEC) {
        return rc;
    }
    // A file the kernel will not execute is a script for `/bin/sh`, as
    // Darwin's `posix_spawnp` has it.
    let shell = shell_argv(&found, &unsafe { argv_of(argv) });
    let shell = as_argv(&shell);
    unsafe {
        posix_spawn(
            pid,
            BSHELL.as_ptr(),
            file_actions,
            attrp,
            shell.as_ptr(),
            envp,
        )
    }
}

/// `int system(const char *command)`, using the guest's `/bin/sh`.
///
/// A null command tests whether that shell is available. Otherwise the shell
/// receives the command unchanged with `-c`, and the caller receives its Darwin
/// wait status. A shell that cannot be executed exits with status 127.
///
/// # Safety
///
/// `command` is null or a NUL-terminated string.
pub unsafe extern "C" fn system(command: *const c_char) -> c_int {
    use std::os::unix::ffi::OsStrExt;

    let shell = exec_path(BSHELL);
    // SAFETY: the translated path is a NUL-terminated string.
    let available = std::fs::metadata(std::ffi::OsStr::from_bytes(shell.to_bytes()))
        .is_ok_and(|meta| meta.is_file())
        && unsafe { libc::access(shell.as_ptr(), libc::X_OK) } == 0;
    if command.is_null() {
        return c_int::from(available);
    }

    let signals = match SystemSignals::new() {
        Ok(signals) => signals,
        Err(error) => {
            crate::errno::set(error);
            return -1;
        }
    };
    let attr = match signals.child_attr() {
        Ok(attr) => attr,
        Err(error) => {
            drop(signals);
            crate::errno::set(error);
            return -1;
        }
    };
    let args = [
        c"sh".to_owned(),
        c"-c".to_owned(),
        unsafe { CStr::from_ptr(command) }.to_owned(),
    ];
    let mut pid = 0;
    // SAFETY: all strings, arrays, and the host attribute outlive this call.
    let rc = unsafe { spawn_host(&mut pid, BSHELL, &args, environ(), None, Some(&attr)) };
    if rc != 0 {
        drop(signals);
        if rc == libc::EAGAIN || rc == libc::ENOMEM {
            crate::errno::set(rc);
            return -1;
        }
        return 127 << 8;
    }
    let result = match wait_child(pid, true) {
        Ok(Some(status)) => status,
        _ => -1,
    };
    let error = crate::errno::get();
    drop(signals);
    crate::errno::set(error);
    result
}

/// Start `sh -c command` as `popen` does: `dup2` each `(from, to)` and close
/// each of `close` in the child, with the guest's environment.
pub(crate) fn spawn_shell(
    command: &CStr,
    dup2: &[(c_int, c_int)],
    close: &[c_int],
) -> Result<libc::pid_t, c_int> {
    let actions = FileActions {
        actions: dup2
            .iter()
            .map(|&(from, to)| Action::Dup2 { from, to })
            .chain(close.iter().map(|&fd| Action::Close(fd)))
            .collect(),
    };
    let host = actions.to_host()?;
    let args = [c"sh".to_owned(), c"-c".to_owned(), command.to_owned()];
    let mut pid = 0;
    // SAFETY: the strings and the host actions outlive the call.
    match unsafe { spawn_host(&mut pid, BSHELL, &args, environ(), Some(&host), None) } {
        0 => Ok(pid),
        rc => Err(rc),
    }
}

/// # Safety
///
/// Each takes the guest's `posix_spawn*_t`, which Darwin declares as a `void *`.
pub unsafe extern "C" fn file_actions_init(at: *mut c_void) -> c_int {
    unsafe { init::<FileActions>(at) }
}

/// # Safety
///
/// As above.
pub unsafe extern "C" fn file_actions_destroy(at: *mut c_void) -> c_int {
    unsafe { destroy::<FileActions>(at) }
}

/// Add `action` to the object at `at`.
///
/// # Safety
///
/// As above.
unsafe fn add(at: *mut c_void, action: Action) -> c_int {
    match unsafe { object::<FileActions>(at) } {
        Some(actions) => {
            actions.actions.push(action);
            0
        }
        None => libc::EINVAL,
    }
}

/// # Safety
///
/// As above, and `path` is a NUL-terminated string.
pub unsafe extern "C" fn file_actions_addopen(
    at: *mut c_void,
    fd: c_int,
    path: *const c_char,
    oflag: c_int,
    mode: libc::mode_t,
) -> c_int {
    if path.is_null() {
        return libc::EINVAL;
    }
    // The child opens this on the host's behalf, after this process has gone,
    // so the prefix's spelling is what has to be recorded.
    let path = prefix::ns::to_host(unsafe { CStr::from_ptr(path) }).into_owned();
    unsafe {
        add(
            at,
            Action::Open {
                fd,
                path,
                oflag,
                mode,
            },
        )
    }
}

/// # Safety
///
/// As above.
pub unsafe extern "C" fn file_actions_adddup2(at: *mut c_void, from: c_int, to: c_int) -> c_int {
    unsafe { add(at, Action::Dup2 { from, to }) }
}

/// # Safety
///
/// As above.
pub unsafe extern "C" fn file_actions_addclose(at: *mut c_void, fd: c_int) -> c_int {
    unsafe { add(at, Action::Close(fd)) }
}

/// # Safety
///
/// As above.
pub unsafe extern "C" fn file_actions_addinherit_np(at: *mut c_void, fd: c_int) -> c_int {
    unsafe { add(at, Action::Inherit(fd)) }
}

/// # Safety
///
/// As above, and `path` is a NUL-terminated string.
pub unsafe extern "C" fn file_actions_addchdir_np(at: *mut c_void, path: *const c_char) -> c_int {
    if path.is_null() {
        return libc::EINVAL;
    }
    // As `addopen`: recorded now, used by the child.
    let path = prefix::ns::to_host(unsafe { CStr::from_ptr(path) }).into_owned();
    unsafe { add(at, Action::Chdir(path)) }
}

/// # Safety
///
/// As above.
pub unsafe extern "C" fn file_actions_addfchdir_np(at: *mut c_void, fd: c_int) -> c_int {
    unsafe { add(at, Action::Fchdir(fd)) }
}

/// # Safety
///
/// As above.
pub unsafe extern "C" fn attr_init(at: *mut c_void) -> c_int {
    unsafe { init::<Attributes>(at) }
}

/// # Safety
///
/// As above.
pub unsafe extern "C" fn attr_destroy(at: *mut c_void) -> c_int {
    unsafe { destroy::<Attributes>(at) }
}

/// # Safety
///
/// As above.
pub unsafe extern "C" fn attr_setflags(at: *mut c_void, flags: u16) -> c_int {
    match unsafe { object::<Attributes>(at) } {
        Some(attributes) => {
            attributes.flags = flags as u32;
            0
        }
        None => libc::EINVAL,
    }
}

/// # Safety
///
/// As above, and `out` is a writable `short *`.
pub unsafe extern "C" fn attr_getflags(at: *mut c_void, out: *mut u16) -> c_int {
    match unsafe { object::<Attributes>(at) } {
        Some(attributes) if !out.is_null() => {
            // SAFETY: the caller states `out` is writable.
            unsafe { *out = attributes.flags as u16 };
            0
        }
        _ => libc::EINVAL,
    }
}

/// # Safety
///
/// As above.
pub unsafe extern "C" fn attr_setpgroup(at: *mut c_void, pgroup: libc::pid_t) -> c_int {
    match unsafe { object::<Attributes>(at) } {
        Some(attributes) => {
            attributes.pgroup = pgroup;
            0
        }
        None => libc::EINVAL,
    }
}

/// # Safety
///
/// As above, and `set` is a readable Darwin `sigset_t` - one 32-bit word.
pub unsafe extern "C" fn attr_setsigmask(at: *mut c_void, set: *const u32) -> c_int {
    match unsafe { object::<Attributes>(at) } {
        Some(attributes) if !set.is_null() => {
            // SAFETY: the caller states `set` is a readable Darwin `sigset_t`.
            attributes.sigmask = unsafe { *set };
            0
        }
        _ => libc::EINVAL,
    }
}

/// # Safety
///
/// As above.
pub unsafe extern "C" fn attr_setsigdefault(at: *mut c_void, set: *const u32) -> c_int {
    match unsafe { object::<Attributes>(at) } {
        Some(attributes) if !set.is_null() => {
            attributes.sigdefault = unsafe { *set };
            0
        }
        _ => libc::EINVAL,
    }
}

/// # Safety
///
/// `at` holds this module's attribute object and `out` is a writable pid_t.
pub unsafe extern "C" fn attr_getpgroup(at: *mut c_void, out: *mut libc::pid_t) -> c_int {
    match unsafe { object::<Attributes>(at) } {
        Some(attributes) if !out.is_null() => {
            unsafe { *out = attributes.pgroup };
            0
        }
        _ => libc::EINVAL,
    }
}

/// # Safety
///
/// `at` holds this module's attribute object and `out` is a writable Darwin sigset_t.
pub unsafe extern "C" fn attr_getsigmask(at: *mut c_void, out: *mut u32) -> c_int {
    match unsafe { object::<Attributes>(at) } {
        Some(attributes) if !out.is_null() => {
            unsafe { *out = attributes.sigmask };
            0
        }
        _ => libc::EINVAL,
    }
}

/// # Safety
///
/// `at` holds this module's attribute object and `out` is a writable Darwin sigset_t.
pub unsafe extern "C" fn attr_getsigdefault(at: *mut c_void, out: *mut u32) -> c_int {
    match unsafe { object::<Attributes>(at) } {
        Some(attributes) if !out.is_null() => {
            unsafe { *out = attributes.sigdefault };
            0
        }
        _ => libc::EINVAL,
    }
}

/// `int posix_spawnattr_setbinpref_np(posix_spawnattr_t *, size_t, cpu_type_t *,
/// size_t *)`.
///
/// Accepted and ignored: it chooses which slice of a fat Mach-O to run, and this
/// runtime makes that choice itself when it loads the image. The out-parameter is
/// still filled, because a caller checks how many preferences were taken.
///
/// # Safety
///
/// As above, and `taken` is null or a writable `size_t`.
pub unsafe extern "C" fn attr_setbinpref_np(
    _at: *mut c_void,
    count: usize,
    _prefs: *mut u32,
    taken: *mut usize,
) -> c_int {
    if !taken.is_null() {
        // SAFETY: the caller states `taken` is writable.
        unsafe { *taken = count };
    }
    0
}

/// `int posix_spawnattr_setarchpref_np(posix_spawnattr_t *, size_t,
/// cpu_type_t *, cpu_subtype_t *, size_t *)`
///
/// Ignored for the reason [`attr_setbinpref_np`] is: the slice is chosen when
/// the image is loaded. The out-parameter is filled all the same.
///
/// # Safety
///
/// As above, and `taken` is null or a writable `size_t`.
pub unsafe extern "C" fn attr_setarchpref_np(
    at: *mut c_void,
    count: usize,
    prefs: *mut u32,
    _subprefs: *mut u32,
    taken: *mut usize,
) -> c_int {
    unsafe { attr_setbinpref_np(at, count, prefs, taken) }
}

/// Spawn a child for a framework's process API, such as `NSTask`.
///
/// Launched as the guest's own `posix_spawn` would launch it, so `path` and
/// `cwd` are guest paths and a Mach-O runs through this runtime. `stdio` holds
/// the descriptors the child gets as 0, 1 and 2, with `None` inheriting the
/// parent's. With no `envp`, the child gets the guest's environment.
pub fn spawn_child(
    path: &CStr,
    argv: &[CString],
    envp: Option<&[CString]>,
    cwd: Option<&CStr>,
    stdio: [Option<c_int>; 3],
) -> Result<libc::pid_t, c_int> {
    let mut actions = FileActions::default();
    for (to, from) in stdio.iter().enumerate() {
        if let Some(from) = *from {
            actions.actions.push(Action::Dup2 {
                from,
                to: to as c_int,
            });
        }
    }
    if let Some(cwd) = cwd {
        actions
            .actions
            .push(Action::Chdir(prefix::ns::to_host(cwd).into_owned()));
    }
    let actions = actions.to_host()?;
    let envp = envp.map(as_argv);
    let envp = envp.as_ref().map_or(environ(), |e| e.as_ptr());
    let mut pid = 0;
    // SAFETY: every string, array and the actions outlive the call.
    let rc = unsafe { spawn_host(&mut pid, path, argv, envp, Some(&actions), None) };
    match rc {
        0 => Ok(pid),
        error => Err(error),
    }
}

/// Launch the application bundle at the guest path `bundle` as LaunchServices
/// does: this runtime runs the bundle itself, so the child claims the
/// application and hands the launch to an instance already running instead,
/// unless `new_instance`. `args` follow the bundle, as `open --args` passes them.
pub fn spawn_application(
    bundle: &CStr,
    args: &[CString],
    new_instance: bool,
) -> Result<libc::pid_t, c_int> {
    let runtime = runtime_path().ok_or(libc::ENOENT)?;
    let mut argv = vec![runtime.clone(), c"run".to_owned()];
    if EMULATED.load(Ordering::Relaxed) {
        argv.push(c"--emulated".to_owned());
    }
    if new_instance {
        argv.push(c"--new-instance".to_owned());
    }
    argv.push(bundle.to_owned());
    argv.extend(args.iter().cloned());
    let argv = as_argv(&argv);
    // SAFETY: the guest's environment is a live, null-terminated vector.
    let environment = unsafe { runtime_environ(environ()) };
    let environment = as_argv(&environment);
    let mut pid = 0;
    // SAFETY: every string and array outlives the call.
    let rc = unsafe {
        libc::posix_spawn(
            &mut pid,
            runtime.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            argv.as_ptr() as *const *mut c_char,
            environment.as_ptr() as *const *mut c_char,
        )
    };
    match rc {
        0 => Ok(pid),
        error => Err(error),
    }
}

/// Reap a child [`spawn_child`] made, answering its Darwin wait status.
///
/// `Ok(None)` when `block` is false and the child is still running.
pub fn wait_child(pid: libc::pid_t, block: bool) -> Result<Option<c_int>, c_int> {
    let mut status = 0;
    loop {
        #[cfg(not(target_os = "macos"))]
        // SAFETY: a writable local; Darwin's `WNOHANG` is 1.
        let reaped = unsafe { crate::wait::waitpid(pid, &mut status, if block { 0 } else { 1 }) };
        #[cfg(target_os = "macos")]
        // SAFETY: a writable local.
        let reaped =
            unsafe { libc::waitpid(pid, &mut status, if block { 0 } else { libc::WNOHANG }) };
        match reaped {
            0 => return Ok(None),
            -1 if crate::errno::get() == libc::EINTR => continue,
            -1 => return Err(crate::errno::get()),
            _ => return Ok(Some(status)),
        }
    }
}

exports::exports! {
    functions {
        "execl" => execl,
        "execlp" => execlp,
        "execle" => execle,
        "execv" => execv,
        "vfork" => vfork,
        "execve" => execve,
        "execvp" => execvp,
        "posix_spawn" => posix_spawn,
        "posix_spawnp" => posix_spawnp,
        "system" => system,
        "posix_spawn_file_actions_init" => file_actions_init,
        "posix_spawn_file_actions_destroy" => file_actions_destroy,
        "posix_spawn_file_actions_addopen" => file_actions_addopen,
        "posix_spawn_file_actions_adddup2" => file_actions_adddup2,
        "posix_spawn_file_actions_addclose" => file_actions_addclose,
        "posix_spawn_file_actions_addinherit_np" => file_actions_addinherit_np,
        "posix_spawn_file_actions_addchdir_np" => file_actions_addchdir_np,
        "posix_spawn_file_actions_addfchdir_np" => file_actions_addfchdir_np,
        "posix_spawnattr_init" => attr_init,
        "posix_spawnattr_destroy" => attr_destroy,
        "posix_spawnattr_setflags" => attr_setflags,
        "posix_spawnattr_getflags" => attr_getflags,
        "posix_spawnattr_getsigdefault" => attr_getsigdefault,
        "posix_spawnattr_getsigmask" => attr_getsigmask,
        "posix_spawnattr_getpgroup" => attr_getpgroup,
        "posix_spawnattr_setpgroup" => attr_setpgroup,
        "posix_spawnattr_setsigmask" => attr_setsigmask,
        "posix_spawnattr_setsigdefault" => attr_setsigdefault,
        "posix_spawnattr_setbinpref_np" => attr_setbinpref_np,
        "posix_spawnattr_setarchpref_np" => attr_setarchpref_np,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test that decides everything else: a Mach-O is recognised by its magic,
    /// and the host's own binaries are not.
    #[test]
    fn a_mach_o_is_told_from_an_elf() {
        let dir = std::env::temp_dir().join(format!("rosemary-spawn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");

        let mach_o = dir.join("thin");
        std::fs::write(&mach_o, 0xfeed_facfu32.to_le_bytes()).expect("write");
        let fat = dir.join("fat");
        std::fs::write(&fat, 0xcafe_babeu32.to_le_bytes()).expect("write");
        let elf = dir.join("elf");
        std::fs::write(&elf, b"\x7fELF____").expect("write");

        let c = |p: &std::path::Path| CString::new(p.to_str().unwrap()).unwrap();
        assert!(is_mach_o(&c(&mach_o)), "a thin 64-bit image");
        assert!(is_mach_o(&c(&fat)), "a fat archive");
        assert!(!is_mach_o(&c(&elf)), "an ELF must be left alone");
        assert!(!is_mach_o(c"/nonexistent-path-for-this-test"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The rewrite puts `run <path>` in front of the guest's arguments, and drops the
    /// guest's `argv[0]` - which this runtime derives from the image instead.
    #[test]
    fn a_mach_o_spawn_is_repointed_at_this_runtime() {
        let dir = std::env::temp_dir().join(format!("rosemary-rewrite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let helper = dir.join("ExampleAppCrashHandler");
        std::fs::write(&helper, 0xfeed_facfu32.to_le_bytes()).expect("write");
        let path = CString::new(helper.to_str().unwrap()).unwrap();

        let argv = vec![c"ExampleAppCrashHandler".to_owned(), c"--pipe=3".to_owned()];
        let (runtime, rewritten) = rewrite(&path, &path, &argv).expect("a Mach-O is rewritten");

        assert_eq!(runtime, rewritten[0], "argv[0] is the runtime itself");
        assert_eq!(rewritten[1], c"run".to_owned());
        assert_eq!(rewritten[2], path);
        assert_eq!(rewritten[3], c"--pipe=3".to_owned());
        assert_eq!(rewritten.len(), 4, "the guest's argv[0] is not passed on");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn explicit_emulation_is_a_runtime_option_before_the_guest_path() {
        let argv = [
            c"guest-name".to_owned(),
            c"--native".to_owned(),
            c"argument".to_owned(),
        ];
        assert_eq!(
            runtime_argv(c"rosemary", c"/guest/helper", &argv, true),
            [
                c"rosemary",
                c"run",
                c"--emulated",
                c"/guest/helper",
                c"--native",
                c"argument"
            ]
            .map(CStr::to_owned)
        );
        assert_eq!(
            runtime_argv(c"rosemary", c"/guest/helper", &argv, false),
            [
                c"rosemary",
                c"run",
                c"/guest/helper",
                c"--native",
                c"argument"
            ]
            .map(CStr::to_owned)
        );
    }

    #[test]
    fn invalid_file_actions_report_the_hosts_error() {
        let actions = FileActions {
            actions: vec![Action::Dup2 { from: -1, to: 1 }],
        };
        assert!(matches!(actions.to_host(), Err(libc::EBADF)));
        assert_eq!(
            spawn_child(c"/missing", &[], None, None, [None, Some(-1), None]),
            Err(libc::EBADF)
        );
    }

    fn scratch(test: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rosemary-{test}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn c_path(p: &std::path::Path) -> CString {
        CString::new(p.to_str().unwrap()).unwrap()
    }

    #[test]
    fn a_shebang_line_names_an_interpreter_and_one_argument() {
        let dir = scratch("shebang");
        let script = dir.join("script");
        std::fs::write(&script, b"#!  /bin/sh -e -x\necho hi\n").unwrap();
        let bare = dir.join("bare");
        std::fs::write(&bare, b"#!/usr/local/bin/ash\n").unwrap();
        let plain = dir.join("plain");
        std::fs::write(&plain, b"echo hi\n").unwrap();

        assert_eq!(
            interpreter(&c_path(&script)),
            Some((c"/bin/sh".to_owned(), Some(c"-e -x".to_owned())))
        );
        assert_eq!(
            interpreter(&c_path(&bare)),
            Some((c"/usr/local/bin/ash".to_owned(), None))
        );
        assert_eq!(interpreter(&c_path(&plain)), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A script's interpreter is resolved the way the script's own path is:
    /// a host program runs it directly, a Mach-O runs it through this runtime.
    #[test]
    fn a_script_runs_under_its_translated_interpreter() {
        let dir = scratch("script");
        let host_shell = dir.join("host-shell");
        std::fs::write(&host_shell, b"\x7fELF____").unwrap();
        let script = dir.join("script");
        std::fs::write(&script, format!("#!{} -e\ntrue\n", host_shell.display())).unwrap();
        let argv = [c"script".to_owned(), c"one".to_owned()];

        let plan = launch(&c_path(&script), &argv);
        assert!(!plan.runtime);
        assert_eq!(plan.program, c_path(&host_shell));
        assert_eq!(
            plan.argv.unwrap(),
            [
                c_path(&host_shell),
                c"-e".to_owned(),
                c_path(&script),
                c"one".to_owned()
            ]
        );

        let shell = dir.join("macho-shell");
        std::fs::write(&shell, 0xfeed_facfu32.to_le_bytes()).unwrap();
        let guest_script = dir.join("guest-script");
        std::fs::write(&guest_script, format!("#!{}\n", shell.display())).unwrap();
        let plan = launch(&c_path(&guest_script), &argv);
        assert!(
            plan.runtime,
            "a Mach-O interpreter runs through this runtime"
        );
        let rewritten = plan.argv.unwrap();
        assert_eq!(rewritten[1], c"run".to_owned());
        assert_eq!(rewritten[2], c_path(&shell));
        assert_eq!(rewritten[3], c_path(&guest_script));
        assert_eq!(rewritten[4], c"one".to_owned());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn path_search_answers_with_the_guest_spelling_or_an_error() {
        let found = search_path(c"sh").expect("a shell on PATH");
        assert!(found.to_bytes().ends_with(b"/sh"));
        assert_eq!(search_path(c"./anything"), Ok(c"./anything".to_owned()));
        assert_eq!(
            search_path(c"no-such-program-anywhere-rosemary"),
            Err(libc::ENOENT)
        );
        assert_eq!(search_path(c""), Err(libc::ENOENT));
    }

    /// Anything that is not a Mach-O is left entirely alone.
    #[test]
    fn a_host_program_is_not_rewritten() {
        let dir = scratch("host-program");
        let host = dir.join("elf");
        std::fs::write(&host, b"\x7fELF____").unwrap();
        assert!(rewrite(&c_path(&host), &c_path(&host), &[c"elf".to_owned()]).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Darwin's attribute types are `void *`, so the guest's eight bytes must hold
    /// this module's object and be left null once destroyed.
    #[test]
    fn an_attribute_object_lives_in_the_guest_s_pointer() {
        let mut slot: *mut c_void = std::ptr::null_mut();
        let at = &mut slot as *mut _ as *mut c_void;
        // SAFETY: `at` is a writable `void *`.
        unsafe {
            assert_eq!(file_actions_init(at), 0);
            assert!(!slot.is_null(), "init stores an object");
            assert_eq!(file_actions_adddup2(at, 3, 1), 0);
            assert_eq!(file_actions_addclose(at, 4), 0);
            assert_eq!(object::<FileActions>(at).unwrap().actions.len(), 2);
            assert_eq!(file_actions_destroy(at), 0);
            assert!(slot.is_null(), "destroy clears the guest's word");
            // And a second destroy is refused rather than freeing twice.
            assert_eq!(file_actions_destroy(at), libc::EINVAL);
        }
    }

    /// The flags that collide are the reason this translation exists: Darwin's
    /// `SETEXEC` is Linux's `USEVFORK`, so the word must never be passed through.
    #[cfg(target_os = "linux")]
    #[test]
    fn only_the_agreeing_flags_are_forwarded() {
        let attributes = Attributes {
            flags: flag::SETSIGMASK | flag::START_SUSPENDED,
            sigmask: 1 << 1,
            ..Default::default()
        };
        let host = attributes.to_host().expect("a host attribute object");
        let mut flags: libc::c_short = 0;
        // SAFETY: an initialised host object and a live local.
        unsafe { libc::posix_spawnattr_getflags(&host.0, &mut flags) };
        assert_eq!(
            flags & libc::POSIX_SPAWN_SETSIGMASK as libc::c_short,
            libc::POSIX_SPAWN_SETSIGMASK as libc::c_short
        );
        assert_eq!(
            flags & libc::POSIX_SPAWN_SETSID as libc::c_short,
            0,
            "START_SUSPENDED must not be forwarded as SETSID"
        );
    }

    /// A Darwin signal number is translated on the way into a host mask.
    #[test]
    fn a_signal_mask_is_translated_rather_than_copied() {
        // Bit 1 is signal 2, `SIGINT`, which both platforms number the same.
        let set = host_sigset(1 << 1);
        // SAFETY: a live local and a valid signal.
        assert_eq!(unsafe { libc::sigismember(&set, libc::SIGINT) }, 1);
        // SAFETY: as above; nothing else was asked for.
        assert_eq!(unsafe { libc::sigismember(&set, libc::SIGQUIT) }, 0);
    }
}
