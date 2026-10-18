use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

/// `char **environ` as the guest sees it: the *variable*, whose address is what
/// is published and what `_NSGetEnviron` answers with.
static ENVIRON: AtomicPtr<*mut c_char> = AtomicPtr::new(std::ptr::null_mut());

// Serialize our API calls, but keep the published vector authoritative:
// guests can replace it, edit its entries, or lend strings through `putenv`.
// Direct guest edits must not race environment API calls.
static INITIALIZED: Mutex<bool> = Mutex::new(false);

/// Build the block, once.
///
/// Called by every entry point below rather than from a start-up hook: a guest
/// can read `environ` before anything else runs, and a block that is empty
/// until something initialises it is a guest that sees no environment at all.
fn block() -> std::sync::MutexGuard<'static, bool> {
    let mut initialized = INITIALIZED.lock().unwrap_or_else(|e| e.into_inner());
    if *initialized {
        return initialized;
    }
    let entries = handed_off().unwrap_or_else(|| {
        let host: Vec<_> = std::env::vars_os().collect();
        let app = APP_LAUNCH.load(std::sync::atomic::Ordering::Relaxed);
        initial(&host, &guest_value, app)
    });
    let entries: Vec<_> = entries
        .into_iter()
        // A variable whose name or value holds a NUL cannot be spelled in a
        // block at all, and is dropped rather than truncated.
        .filter_map(|entry| CString::new(entry).ok())
        .map(CString::into_raw)
        .collect();
    publish(&entries);
    *initialized = true;
    initialized
}

/// The variables a guest is asked to start with, one `NAME=VALUE` per line.
///
/// The guest's block is not this process's: host variables name host libraries
/// and host servers (`QT_PLUGIN_PATH`, `DISPLAY`, `LD_LIBRARY_PATH`), and a
/// Darwin program that follows them loads or talks to things that are not
/// Darwin's. So nothing is copied unless asked for here. A line that is a bare
/// `NAME` copies this process's value of it, if it has one.
pub const ENVIRON_VAR: &str = "ROSEMARY_ENVIRON";

/// What every guest starts with: the variables a Mac's `login` and terminal
/// set, each with the guest's own value. See [`guest_value`].
const BASELINE: &[&str] = &["HOME", "USER", "LOGNAME", "SHELL", "PATH", "TMPDIR", "PWD"];

/// Host variables copied without being asked for: the terminal and the locale,
/// which describe the user rather than the host's software, and which a
/// command-line guest is lost without. An app gets no locale variables, as one
/// launched from the Finder has none: it reads its language from preferences.
fn passes_through(name: &[u8], app: bool) -> bool {
    matches!(name, b"TERM" | b"COLORTERM") || (!app && is_locale_var(name))
}

fn is_locale_var(name: &[u8]) -> bool {
    name == b"LANG" || name.starts_with(b"LC_")
}

static APP_LAUNCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn launch_as_app() {
    APP_LAUNCH.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The starting block of a guest no other guest launched, as `NAME=VALUE`.
///
/// `host` is this process's environment and `baseline` the guest's value for a
/// [`BASELINE`] name; both are parameters so tests need not touch either.
/// `app` is whether the guest is an application bundle (see [`passes_through`]);
/// `ROSEMARY_ENVIRON` can still copy anything by name.
fn initial(
    host: &[(std::ffi::OsString, std::ffi::OsString)],
    baseline: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    app: bool,
) -> Vec<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    let host_value = |name: &[u8]| {
        host.iter()
            .find(|(n, _)| n.as_bytes() == name)
            .map(|(_, value)| value.as_bytes())
    };
    let mut entries: Vec<Vec<u8>> = Vec::new();
    let mut set = |name: &[u8], value: &[u8]| {
        let entry = [name, b"=", value].concat();
        match entries
            .iter()
            .position(|e| e.len() > name.len() && e[name.len()] == b'=' && e.starts_with(name))
        {
            Some(at) => entries[at] = entry,
            None => entries.push(entry),
        }
    };
    for name in BASELINE {
        if let Some(value) = baseline(name) {
            set(name.as_bytes(), value.as_bytes());
        }
    }
    for (name, value) in host {
        if passes_through(name.as_bytes(), app) {
            set(name.as_bytes(), value.as_bytes());
        }
    }
    let requested = host_value(ENVIRON_VAR.as_bytes()).unwrap_or_default();
    for line in requested.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        match line.iter().position(|&b| b == b'=') {
            // An empty name is not a variable `getenv` could ever answer for.
            Some(0) => {}
            Some(split) => set(&line[..split], &line[split + 1..]),
            None if line.is_empty() => {}
            None => {
                if let Some(value) = host_value(line) {
                    set(line, value);
                }
            }
        }
    }
    entries
}

/// The guest variables that may reach the environment of a runtime a guest
/// launches, as comma-separated names; a name ending in `*` is a prefix.
///
/// Read from this process's environment, never the guest's, so a guest cannot
/// widen it - unless the list names itself, which lets the guest's value
/// replace it for the child.
pub const FORWARD_VAR: &str = "ROSEMARY_FORWARD_ENV_VARS";

/// Whether the guest's `name` crosses into a child runtime's environment.
pub fn is_forwarded(name: &[u8]) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let list = std::env::var_os(FORWARD_VAR).unwrap_or_default();
    allows(list.as_bytes(), name)
}

fn allows(list: &[u8], name: &[u8]) -> bool {
    if name.is_empty() || is_hand_off(name) {
        return false;
    }
    list.split(|&b| b == b',')
        .map(<[u8]>::trim_ascii)
        .any(|pattern| match pattern.strip_suffix(b"*") {
            Some(prefix) => name.starts_with(prefix),
            None => pattern == name,
        })
}

/// A Mac's default `PATH`: `/etc/paths`, in its order.
pub const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// How a parent guest's block reaches a child it execs: the count here, and
/// each entry as `ROSEMARY_GUEST_ENVIRON_<n>`.
///
/// The child is another copy of this runtime, which needs this machine's
/// environment for itself and the host libraries in it, so the guest's block
/// travels beside that one rather than in place of it. One variable per entry
/// keeps any value spellable without an encoding, and within the kernel's
/// limit on one string.
const HAND_OFF_COUNT: &str = "ROSEMARY_GUEST_ENVIRON";

fn hand_off_var(index: usize) -> String {
    format!("{HAND_OFF_COUNT}_{index}")
}

/// Whether `name` is part of a hand-off, which belongs to this runtime.
pub fn is_hand_off(name: &[u8]) -> bool {
    name.strip_prefix(HAND_OFF_COUNT.as_bytes())
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(b"_"))
}

/// The block a parent guest handed over, verbatim, if one did.
///
/// Verbatim because it is already the guest's: what that guest set itself -
/// `su -`'s `HOME=/var/root`, `env -i`'s nothing at all - is what its child
/// must start with.
fn handed_off() -> Option<Vec<Vec<u8>>> {
    use std::os::unix::ffi::OsStringExt;
    let count: usize = std::env::var(HAND_OFF_COUNT).ok()?.parse().ok()?;
    Some(
        (0..count)
            .filter_map(|index| std::env::var_os(hand_off_var(index)))
            .map(OsStringExt::into_vec)
            .collect(),
    )
}

/// `envp` as the variables that hand it to a child runtime.
///
/// # Safety
///
/// `envp` is null or a null-terminated array of NUL-terminated strings.
pub unsafe fn hand_off(envp: *const *const c_char) -> Vec<CString> {
    // SAFETY: the caller's guarantee.
    let entries = unsafe { vector_entries(envp.cast()) };
    let mut out = Vec::with_capacity(entries.len() + 1);
    out.push(CString::new(format!("{HAND_OFF_COUNT}={}", entries.len())).expect("no NUL"));
    for (index, entry) in entries.into_iter().enumerate() {
        // SAFETY: as above, each entry is NUL-terminated.
        let entry = unsafe { CStr::from_ptr(entry) }.to_bytes();
        let var = [hand_off_var(index).as_bytes(), b"=", entry].concat();
        out.push(CString::new(var).expect("from a C string"));
    }
    out
}

/// The guest's value for a [`BASELINE`] name.
///
/// In identity mode that is this host's, if it has one: with no prefix the
/// guest's paths are this host's, and correcting them would only make them
/// wrong.
fn guest_value(name: &str) -> Option<std::ffi::OsString> {
    if prefix::ns::current().is_none() {
        return std::env::var_os(name);
    }
    let value = match name {
        // Where the guest's files go.
        "HOME" => prefix::ns::home().to_string(),
        // What `/Users/<name>` is called, and what `getpwuid` answers.
        "USER" | "LOGNAME" => prefix::layout::user_name(),
        // A login shell the prefix has; see `guest_shell`.
        "SHELL" => {
            let host = std::env::var_os("SHELL").unwrap_or_default();
            let guest = guest_shell(host.as_encoded_bytes());
            String::from_utf8_lossy(&guest).into_owned()
        }
        // Darwin's is per-session and ends in a separator, which a program that
        // concatenates rather than joining depends on.
        "TMPDIR" => "/tmp/".to_string(),
        // Directories in the guest's namespace, not this machine's.
        "PATH" => DEFAULT_PATH.to_string(),
        // A shell sets this to the directory it started the program in. It is
        // the same directory `getcwd` reports, so it gets the same spelling.
        "PWD" => prefix::ns::to_guest(&std::env::current_dir().ok()?)?
            .to_string_lossy()
            .into_owned(),
        _ => return None,
    };
    Some(value.into())
}

/// The guest's spelling of a host account's login shell.
///
/// The host's is a host program - often somewhere a Mac has nothing, like
/// `/run/current-system/sw/bin/zsh` - and a guest that runs it (`su`, `sudo -s`,
/// `login`) would leave the prefix for a shell that is not Darwin's. A Mac's
/// account names `/bin/zsh` or `/bin/bash`, so the same shell in the prefix's
/// `/bin` is the answer, and `/bin/sh` when the prefix has none of that name.
/// Outside a prefix the host's path is the guest's, and is left alone.
pub fn guest_shell(host: &[u8]) -> Vec<u8> {
    if prefix::ns::current().is_none() || host.is_empty() {
        return host.to_vec();
    }
    let name = host.rsplit(|b| *b == b'/').next().unwrap_or(host);
    let candidate = [b"/bin/".as_slice(), name].concat();
    let present = std::str::from_utf8(&candidate)
        .ok()
        .is_some_and(|guest| prefix::ns::host_path(guest).is_file());
    match present {
        true => candidate,
        false => b"/bin/sh".to_vec(),
    }
}

/// Point `environ` at a fresh vector of the current entries.
///
/// The old vector is leaked: a guest that read `environ` into a local still
/// holds it, and C gives no way to tell it not to.
fn publish(entries: &[*mut c_char]) {
    let mut vector = entries.to_vec();
    vector.push(std::ptr::null_mut());
    let leaked = Box::leak(vector.into_boxed_slice());
    ENVIRON.store(leaked.as_mut_ptr(), Ordering::Release);
}

/// Read the live block while the environment lock is held.
fn pointers() -> Vec<*mut c_char> {
    let mut entries = Vec::new();
    let mut at = ENVIRON.load(Ordering::Acquire);
    if !at.is_null() {
        // SAFETY: the published block, including guest replacements, must be
        // null-terminated and live until the next environment mutation.
        unsafe {
            while !(*at).is_null() {
                entries.push(*at);
                at = at.add(1);
            }
        }
    }
    entries
}

/// The address of the guest's `environ` variable.
pub fn environ_slot() -> *mut *mut *mut c_char {
    // Built now if it has not been, so that the address is of a live block.
    drop(block());
    std::ptr::addr_of!(ENVIRON) as *mut *mut *mut c_char
}

/// The slot used by the selected guest libc, including native macOS libc.
pub fn guest_environ_slot() -> *mut *mut *mut c_char {
    #[cfg(target_os = "macos")]
    if !crate::libc_backend::using_builtin() {
        // SAFETY: Darwin's accessor returns a process-lifetime slot.
        return unsafe { libc::_NSGetEnviron() };
    }
    environ_slot()
}

/// The guest's environment as `NAME=VALUE` strings for its initial `envp`.
pub fn entries() -> Vec<CString> {
    let _guard = block();
    pointers()
        .iter()
        // SAFETY: each live entry is a NUL-terminated string.
        .map(|&entry| unsafe { CStr::from_ptr(entry) }.to_owned())
        .collect()
}

/// The index of `name` in `entries`, comparing up to the `=`.
fn position(entries: &[*mut c_char], name: &[u8]) -> Option<usize> {
    entries.iter().position(|&entry| {
        // SAFETY: each live entry is a NUL-terminated string.
        let bytes = unsafe { CStr::from_ptr(entry) }.to_bytes();
        bytes.len() > name.len() && bytes[name.len()] == b'=' && &bytes[..name.len()] == name
    })
}

/// `char *getenv(const char *)`
///
/// # Safety
///
/// `name` is null or a NUL-terminated string.
pub unsafe extern "C" fn getenv(name: *const c_char) -> *mut c_char {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    let name = unsafe { CStr::from_ptr(name) }.to_bytes();
    // Darwin compares only the part before the first '='.
    let name = name.split(|&byte| byte == b'=').next().unwrap();
    let _guard = block();
    let block = pointers();
    let Some(at) = position(&block, name) else {
        return std::ptr::null_mut();
    };
    // The value, which is the entry past `NAME=` - the pointer C callers expect
    // and are entitled to hold on to.
    // SAFETY: the entry is at least `name.len() + 1` bytes and is live, so
    // the interior pointer remains valid while the entry does.
    unsafe { block[at].add(name.len() + 1) }
}

/// `int setenv(const char *, const char *, int)`
///
/// # Safety
///
/// Both strings are null or NUL-terminated.
pub unsafe extern "C" fn setenv(
    name: *const c_char,
    value: *const c_char,
    overwrite: c_int,
) -> c_int {
    if name.is_null() || value.is_null() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let (name, value) = unsafe { (CStr::from_ptr(name), CStr::from_ptr(value)) };
    let name = name.to_bytes();
    if name.is_empty() || name.contains(&b'=') {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let _guard = block();
    let mut block = pointers();
    let at = position(&block, name);
    if at.is_some() && overwrite == 0 {
        return 0;
    }
    let mut entry = name.to_vec();
    entry.push(b'=');
    entry.extend_from_slice(value.to_bytes());
    let Ok(entry) = CString::new(entry) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    let entry = entry.into_raw();
    match at {
        Some(at) => block[at] = entry,
        None => block.push(entry),
    }
    publish(&block);
    0
}

/// `int unsetenv(const char *)`
///
/// # Safety
///
/// `name` is null or NUL-terminated.
pub unsafe extern "C" fn unsetenv(name: *const c_char) -> c_int {
    if name.is_null() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let name = unsafe { CStr::from_ptr(name) }.to_bytes();
    if name.is_empty() || name.contains(&b'=') {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let _guard = block();
    let mut block = pointers();
    let old_len = block.len();
    // Darwin removes every occurrence, including duplicates in guest blocks.
    while let Some(at) = position(&block, name) {
        block.remove(at);
    }
    if block.len() != old_len {
        publish(&block);
    }
    0
}

/// `int putenv(char *)`
///
/// Darwin retains the caller's buffer; later edits change the environment.
///
/// # Safety
///
/// `entry` is null or NUL-terminated. On success it must remain live while in
/// the environment, and edits must not race environment API calls.
pub unsafe extern "C" fn putenv(entry: *mut c_char) -> c_int {
    if entry.is_null() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let text = unsafe { CStr::from_ptr(entry) }.to_bytes();
    let Some(split) = text.iter().position(|b| *b == b'=') else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    let _guard = block();
    let mut block = pointers();
    match position(&block, &text[..split]) {
        Some(at) => block[at] = entry,
        None => block.push(entry),
    }
    publish(&block);
    0
}

/// The entries of a guest-supplied environment vector, up to its null.
///
/// # Safety
///
/// `vector` is null or a null-terminated array of NUL-terminated strings.
unsafe fn vector_entries(vector: *const *mut c_char) -> Vec<*mut c_char> {
    let mut entries = Vec::new();
    if vector.is_null() {
        return entries;
    }
    let mut at = vector;
    // SAFETY: walked only as far as the null that ends it.
    unsafe {
        while !(*at).is_null() {
            entries.push(*at);
            at = at.add(1);
        }
    }
    entries
}

/// A null-terminated vector of `entries`, from the allocator the guest frees
/// with, or null when it is out of memory.
fn allocate_vector(entries: &[*mut c_char]) -> *mut *mut c_char {
    // SAFETY: sized for the entries and the terminating null.
    let vector = unsafe { crate::heap::malloc((entries.len() + 1) * size_of::<*mut c_char>()) }
        .cast::<*mut c_char>();
    if vector.is_null() {
        return vector;
    }
    // SAFETY: `vector` holds `entries.len() + 1` pointers.
    unsafe {
        std::ptr::copy_nonoverlapping(entries.as_ptr(), vector, entries.len());
        *vector.add(entries.len()) = std::ptr::null_mut();
    }
    vector
}

/// Whether `envp` is the process's own `environ`, whose block this module keeps
/// authoritative and which therefore goes through the public entry points.
fn is_process_environ(envp: *mut *mut *mut c_char) -> bool {
    envp == environ_slot() || envp == guest_environ_slot()
}

/// `int _setenvp(const char *name, const char *value, int rewrite, char ***envp,
/// void *state)`
///
/// `setenv` against a vector of the caller's choosing, which libc's own `setenv`
/// calls with `&environ`. The vector is replaced rather than edited, as the
/// process's is, so a guest still holding the old one reads a coherent block.
/// `state` is libc's bookkeeping for which entries it allocated; nothing here
/// frees an entry, so it is never consulted.
///
/// # Safety
///
/// Both strings are null or NUL-terminated; `envp` is null or points at a
/// null-terminated vector, or at null for an empty one.
pub unsafe extern "C" fn _setenvp(
    name: *const c_char,
    value: *const c_char,
    rewrite: c_int,
    envp: *mut *mut *mut c_char,
    _state: *mut c_void,
) -> c_int {
    if envp.is_null() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    if is_process_environ(envp) {
        return unsafe { setenv(name, value, rewrite) };
    }
    if name.is_null() || value.is_null() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let (name, value) = unsafe { (CStr::from_ptr(name), CStr::from_ptr(value)) };
    let name = name.to_bytes();
    if name.is_empty() || name.contains(&b'=') {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let mut entries = unsafe { vector_entries(*envp) };
    let at = position(&entries, name);
    if at.is_some() && rewrite == 0 {
        return 0;
    }
    let mut entry = name.to_vec();
    entry.push(b'=');
    entry.extend_from_slice(value.to_bytes());
    let entry = CString::new(entry)
        .expect("neither part holds a NUL")
        .into_raw();
    match at {
        Some(at) => entries[at] = entry,
        None => entries.push(entry),
    }
    let vector = allocate_vector(&entries);
    if vector.is_null() {
        // SAFETY: the entry was made above and is not in any vector.
        drop(unsafe { CString::from_raw(entry) });
        crate::errno::set(libc::ENOMEM);
        return -1;
    }
    // The old vector is left alone: the guest may hold it, as for `publish`.
    unsafe { *envp = vector };
    0
}

/// `int _unsetenvp(const char *name, char ***envp, void *state)`
///
/// `unsetenv` against the caller's vector, which is edited in place: removal
/// only ever shortens it.
///
/// # Safety
///
/// As [`_setenvp`].
pub unsafe extern "C" fn _unsetenvp(
    name: *const c_char,
    envp: *mut *mut *mut c_char,
    _state: *mut c_void,
) -> c_int {
    if name.is_null() || envp.is_null() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    if is_process_environ(envp) {
        return unsafe { unsetenv(name) };
    }
    let name = unsafe { CStr::from_ptr(name) }.to_bytes();
    if name.is_empty() || name.contains(&b'=') {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let mut entries = unsafe { vector_entries(*envp) };
    let old_len = entries.len();
    while let Some(at) = position(&entries, name) {
        entries.remove(at);
    }
    if entries.len() != old_len {
        // SAFETY: the vector held `old_len` entries and a null, which is room
        // for the shorter one.
        unsafe {
            let vector = *envp;
            std::ptr::copy(entries.as_ptr(), vector, entries.len());
            *vector.add(entries.len()) = std::ptr::null_mut();
        }
    }
    0
}

/// `char **_copyenv(char **env)`
///
/// A new vector of the same entries, which the caller may edit without touching
/// the original's. The strings are shared, as a vector's entries are everywhere
/// else in this module, and the result is null with `ENOMEM` when out of memory.
///
/// # Safety
///
/// `env` is null or a null-terminated array of NUL-terminated strings.
pub unsafe extern "C" fn _copyenv(env: *mut *mut c_char) -> *mut *mut c_char {
    let entries = unsafe { vector_entries(env) };
    let vector = allocate_vector(&entries);
    if vector.is_null() {
        crate::errno::set(libc::ENOMEM);
    }
    vector
}

exports::exports! {
    functions {
        "getenv" => getenv,
        "setenv" => setenv,
        "unsetenv" => unsetenv,
        "putenv" => putenv,
        "_setenvp" => _setenvp,
        "_unsetenvp" => _unsetenvp,
        "_copyenv" => _copyenv,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The block is one process-wide object, so each test uses a name of its
    /// own rather than a shared one.
    fn unique(what: &str) -> CString {
        CString::new(format!("ROSEMARY_TEST_{what}_{}", std::process::id())).expect("no NUL")
    }

    fn host(pairs: &[(&str, &str)]) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        pairs
            .iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect()
    }

    fn texts(entries: Vec<Vec<u8>>) -> Vec<String> {
        entries
            .into_iter()
            .map(|e| String::from_utf8(e).unwrap())
            .collect()
    }

    /// Host variables name host software; a guest gets none of them unless
    /// they are asked for.
    #[test]
    fn a_guest_starts_without_the_hosts_variables() {
        let host = host(&[
            ("DISPLAY", ":0"),
            ("QT_PLUGIN_PATH", "/usr/lib/qt6/plugins"),
            ("LD_LIBRARY_PATH", "/opt/lib"),
            ("HOME", "/home/user"),
            ("TERM", "xterm-256color"),
            ("LC_CTYPE", "en_US.UTF-8"),
        ]);
        let baseline = |name: &str| (name == "HOME").then(|| "/Users/user".into());
        let got = texts(initial(&host, &baseline, false));
        assert_eq!(
            got,
            [
                "HOME=/Users/user",
                "TERM=xterm-256color",
                "LC_CTYPE=en_US.UTF-8"
            ]
        );
    }

    /// `ROSEMARY_ENVIRON` sets a variable, copies a bare name's host value,
    /// and wins over the baseline.
    #[test]
    fn requested_variables_are_set_or_copied() {
        let host = host(&[
            ("DISPLAY", ":0"),
            (
                ENVIRON_VAR,
                "FOO=bar=baz\nDISPLAY\nMISSING\n\n=nameless\nPATH=/opt/bin\r\n",
            ),
        ]);
        let baseline = |name: &str| (name == "PATH").then(|| DEFAULT_PATH.into());
        let got = texts(initial(&host, &baseline, false));
        assert_eq!(got, ["PATH=/opt/bin", "FOO=bar=baz", "DISPLAY=:0"]);
    }

    /// An app gets none of the host's locale variables by default, but one
    /// named in `ROSEMARY_ENVIRON` is still copied.
    #[test]
    fn an_app_gets_only_the_locale_it_asks_for() {
        let host = host(&[
            ("TERM", "xterm-256color"),
            ("LANG", "de_DE.UTF-8"),
            ("LC_CTYPE", "en_US.UTF-8"),
            (ENVIRON_VAR, "LANG"),
        ]);
        let got = texts(initial(&host, &|_| None, true));
        assert_eq!(got, ["TERM=xterm-256color", "LANG=de_DE.UTF-8"]);
    }

    /// Nothing crosses by default, a `*` pattern is a prefix, and a hand-off
    /// is never a guest's to forward.
    #[test]
    fn only_listed_variables_are_forwarded() {
        assert!(!allows(b"", b"ROSEMARY_STATS"));
        let list = b"ROSEMARY_STATS, FOO_* ,ROSEMARY_FORWARD_ENV_VARS";
        assert!(allows(list, b"ROSEMARY_STATS"));
        assert!(!allows(list, b"ROSEMARY_STATSX"));
        assert!(allows(list, b"FOO_BAR"));
        assert!(!allows(list, b"FOO"));
        assert!(allows(list, FORWARD_VAR.as_bytes()));
        assert!(!allows(b"ROSEMARY_*", b"ROSEMARY_GUEST_ENVIRON_0"));
        assert!(!allows(b"*", b""));
    }

    #[test]
    fn hand_off_names_belong_to_the_runtime() {
        assert!(is_hand_off(b"ROSEMARY_GUEST_ENVIRON"));
        assert!(is_hand_off(b"ROSEMARY_GUEST_ENVIRON_12"));
        assert!(!is_hand_off(b"ROSEMARY_GUEST_ENVIRONMENT"));
        assert!(!is_hand_off(b"ROSEMARY_ENVIRON"));
    }

    /// `PATH` is not `PATHX`. Comparing the prefix alone would have `getenv`
    /// answer about whichever entry happened to come first.
    #[test]
    fn a_name_is_matched_up_to_the_equals() {
        let entries: Vec<*mut c_char> = vec![
            CString::new("PATHX=wrong").unwrap().into_raw(),
            CString::new("PATH=right").unwrap().into_raw(),
        ];
        assert_eq!(position(&entries, b"PATH"), Some(1));
        assert_eq!(position(&entries, b"PATHX"), Some(0));
        assert_eq!(position(&entries, b"PAT"), None);
        // An entry that is exactly the name with no `=` is not that name.
        let bare: Vec<*mut c_char> = vec![CString::new("PATH").unwrap().into_raw()];
        assert_eq!(position(&bare, b"PATH"), None);
    }

    #[test]
    fn a_value_survives_being_set_and_read_back() {
        let name = unique("SET");
        // SAFETY: two NUL-terminated strings this test owns.
        unsafe {
            assert!(getenv(name.as_ptr()).is_null(), "not set yet");
            assert_eq!(setenv(name.as_ptr(), c"first".as_ptr(), 1), 0);
            assert_eq!(CStr::from_ptr(getenv(name.as_ptr())), c"first");
            // Without `overwrite` the first value stands.
            assert_eq!(setenv(name.as_ptr(), c"second".as_ptr(), 0), 0);
            assert_eq!(CStr::from_ptr(getenv(name.as_ptr())), c"first");
            assert_eq!(setenv(name.as_ptr(), c"second".as_ptr(), 1), 0);
            assert_eq!(CStr::from_ptr(getenv(name.as_ptr())), c"second");
            assert_eq!(unsetenv(name.as_ptr()), 0);
            assert!(getenv(name.as_ptr()).is_null(), "removed");
        }
    }

    /// The guest's block is this runtime's, so a variable it sets is *not* in
    /// this process's own environment - which is the whole point.
    #[test]
    fn what_the_guest_sets_does_not_reach_this_process() {
        let name = unique("ISOLATED");
        // SAFETY: NUL-terminated strings this test owns.
        unsafe { assert_eq!(setenv(name.as_ptr(), c"guest".as_ptr(), 1), 0) };
        assert!(
            std::env::var_os(name.to_str().expect("utf-8")).is_none(),
            "the host's environment was written too"
        );
        unsafe { unsetenv(name.as_ptr()) };
    }

    /// Darwin rejects `putenv` without an equals sign and keeps the old value.
    #[test]
    fn putenv_without_an_equals_sign_keeps_the_value() {
        let name = unique("PUT");
        let entry = CString::new(format!("{}=value", name.to_str().unwrap())).expect("no NUL");
        // SAFETY: the entry stays live until `unsetenv` removes it.
        unsafe {
            assert_eq!(putenv(entry.as_ptr().cast_mut()), 0);
            assert_eq!(CStr::from_ptr(getenv(name.as_ptr())), c"value");
            assert_eq!(putenv(name.as_ptr().cast_mut()), -1);
            assert_eq!(CStr::from_ptr(getenv(name.as_ptr())), c"value");
            assert_eq!(unsetenv(name.as_ptr()), 0);
        }
    }

    #[test]
    fn putenv_buffer_child() {
        if std::env::var("ROSEMARY_ENV_TEST_CHILD").as_deref() != Ok("borrowed") {
            return;
        }
        let name = unique("BORROWED");
        let mut entry = format!("{}=first\0", name.to_str().unwrap()).into_bytes();
        let offset = name.to_bytes().len() + 1;
        // SAFETY: entry remains NUL-terminated and live until removal.
        unsafe {
            assert_eq!(putenv(entry.as_mut_ptr().cast()), 0);
            assert_eq!(getenv(name.as_ptr()), entry.as_mut_ptr().add(offset).cast());
            entry[offset..offset + 5].copy_from_slice(b"later");
            assert_eq!(CStr::from_ptr(getenv(name.as_ptr())), c"later");
            let with_equals = CString::new(format!("{}=ignored", name.to_str().unwrap())).unwrap();
            assert_eq!(getenv(with_equals.as_ptr()), getenv(name.as_ptr()));
            assert_eq!(setenv(name.as_ptr(), c"owned".as_ptr(), 1), 0);
            entry[offset..offset + 5].copy_from_slice(b"again");
            assert_eq!(CStr::from_ptr(getenv(name.as_ptr())), c"owned");
            assert_eq!(unsetenv(name.as_ptr()), 0);
        }
    }

    #[test]
    fn putenv_keeps_the_callers_mutable_buffer() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "env::tests::putenv_buffer_child"])
            .env("ROSEMARY_ENV_TEST_CHILD", "borrowed")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    #[test]
    fn environment_replacement_child() {
        if std::env::var_os("ROSEMARY_ENV_TEST_CHILD").is_none() {
            return;
        }
        let slot = environ_slot();
        let first = CString::new("DUP=first").unwrap();
        let second = CString::new("DUP=second").unwrap();
        let mut replacement = [
            first.as_ptr().cast_mut(),
            second.as_ptr().cast_mut(),
            std::ptr::null_mut(),
        ];
        // SAFETY: this isolated process runs one test; the replacement and its
        // strings outlive all uses of the environment.
        unsafe {
            *slot = replacement.as_mut_ptr();
            assert_eq!(CStr::from_ptr(getenv(c"DUP".as_ptr())), c"first");
            assert_eq!(entries(), [first.clone(), second.clone()]);
            assert_eq!(unsetenv(c"DUP".as_ptr()), 0);
            assert!(getenv(c"DUP".as_ptr()).is_null());
            assert!(entries().is_empty());
            assert!((**slot).is_null());
            assert_eq!(setenv(c"ONLY".as_ptr(), c"one".as_ptr(), 1), 0);
            assert_eq!(entries(), [c"ONLY=one".to_owned()]);
            *slot = std::ptr::null_mut();
            assert!(entries().is_empty());
            assert_eq!(setenv(c"ONLY".as_ptr(), c"two".as_ptr(), 1), 0);
            assert_eq!(entries(), [c"ONLY=two".to_owned()]);
        }
    }

    #[test]
    fn guest_replacement_and_empty_blocks_stay_authoritative() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "env::tests::environment_replacement_child"])
            .env("ROSEMARY_ENV_TEST_CHILD", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    /// `_setenvp` and `_unsetenvp` edit the vector they are given, not the
    /// process's, and a copy is independent of its original.
    #[test]
    fn a_private_vector_is_edited_without_touching_the_process() {
        let name = unique("PRIVATE");
        let first = CString::new("A=1").unwrap();
        let mut original = [first.as_ptr().cast_mut(), std::ptr::null_mut()];
        // SAFETY: strings and vectors this test owns, null-terminated.
        unsafe {
            let mut copy = _copyenv(original.as_mut_ptr());
            assert_eq!(vector_entries(copy).len(), 1);
            assert_eq!(
                _setenvp(
                    c"B".as_ptr(),
                    c"2".as_ptr(),
                    1,
                    &mut copy,
                    std::ptr::null_mut()
                ),
                0
            );
            assert_eq!(
                _setenvp(
                    c"A".as_ptr(),
                    c"9".as_ptr(),
                    0,
                    &mut copy,
                    std::ptr::null_mut()
                ),
                0
            );
            let got: Vec<_> = vector_entries(copy)
                .iter()
                .map(|e| CStr::from_ptr(*e).to_owned())
                .collect();
            assert_eq!(got, [c"A=1".to_owned(), c"B=2".to_owned()]);
            assert_eq!(
                _unsetenvp(c"A".as_ptr(), &mut copy, std::ptr::null_mut()),
                0
            );
            assert_eq!(vector_entries(copy).len(), 1);
            assert_eq!(vector_entries(original.as_ptr()).len(), 1, "original kept");
            assert_eq!(
                _setenvp(
                    c"=".as_ptr(),
                    c"x".as_ptr(),
                    1,
                    &mut copy,
                    std::ptr::null_mut()
                ),
                -1
            );
            assert!(getenv(name.as_ptr()).is_null());
            assert!(getenv(c"B".as_ptr()).is_null(), "the process was written");
        }
    }

    /// `environ` is null-terminated and holds what the block holds, so a guest
    /// walking it sees the same variables `getenv` answers.
    #[test]
    fn environ_is_a_null_terminated_view_of_the_block() {
        let name = unique("ENVIRON");
        // SAFETY: NUL-terminated strings this test owns.
        unsafe { assert_eq!(setenv(name.as_ptr(), c"here".as_ptr(), 1), 0) };
        let wanted = CString::new(format!("{}=here", name.to_str().unwrap())).expect("no NUL");
        // SAFETY: the slot holds a null-terminated vector this runtime owns.
        let mut at = unsafe { *environ_slot() };
        let mut found = false;
        // SAFETY: walked only as far as the null that ends it.
        unsafe {
            while !(*at).is_null() {
                found |= CStr::from_ptr(*at) == wanted.as_c_str();
                at = at.add(1);
            }
        }
        assert!(found, "the block's entry is not in `environ`");
        unsafe { unsetenv(name.as_ptr()) };
    }

    /// The child of a rewritten `exec` gets the guest's block entry by entry,
    /// including a value with a newline and an empty block.
    #[test]
    fn a_guest_block_is_handed_off_entry_by_entry() {
        let entries = [
            CString::new("HOME=/var/root").unwrap(),
            CString::new("MULTI=a\nb").unwrap(),
        ];
        let mut envp: Vec<*const c_char> = entries.iter().map(|e| e.as_ptr()).collect();
        envp.push(std::ptr::null());
        // SAFETY: a null-terminated array of NUL-terminated strings.
        let out = unsafe { hand_off(envp.as_ptr()) };
        let out: Vec<&str> = out.iter().map(|e| e.to_str().unwrap()).collect();
        assert_eq!(
            out,
            [
                "ROSEMARY_GUEST_ENVIRON=2",
                "ROSEMARY_GUEST_ENVIRON_0=HOME=/var/root",
                "ROSEMARY_GUEST_ENVIRON_1=MULTI=a\nb",
            ]
        );
        // SAFETY: null is an empty block.
        let empty = unsafe { hand_off(std::ptr::null()) };
        assert_eq!(empty, [c"ROSEMARY_GUEST_ENVIRON=0".to_owned()]);
    }

    #[test]
    fn handed_off_block_child() {
        if std::env::var("ROSEMARY_ENV_TEST_CHILD").as_deref() != Ok("handed") {
            return;
        }
        assert_eq!(entries(), [c"ONLY=this".to_owned()]);
        // SAFETY: a literal name.
        assert!(unsafe { getenv(c"ROSEMARY_ENV_TEST_CHILD".as_ptr()) }.is_null());
    }

    /// A handed-off block is the whole of the child's, with nothing of this
    /// process's environment mixed in.
    #[test]
    fn a_handed_off_block_replaces_the_hosts() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "env::tests::handed_off_block_child"])
            .env("ROSEMARY_ENV_TEST_CHILD", "handed")
            .env("ROSEMARY_GUEST_ENVIRON", "1")
            .env("ROSEMARY_GUEST_ENVIRON_0", "ONLY=this")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
}
