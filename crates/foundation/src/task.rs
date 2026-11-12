use std::ffi::{CString, c_int};
use std::sync::Mutex;

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use corefoundation::base::CFTypeRef;

use crate::boxed::{contents, fill, register};
use crate::define::{class_methods, methods};

/// `NSTaskTerminationReason`.
const REASON_EXIT: i64 = 1;
const REASON_UNCAUGHT_SIGNAL: i64 = 2;

/// What `system` answers for a shell it could not run, and what a task that
/// could not be launched reports here - see [`launch`].
const COULD_NOT_RUN: c_int = 127 << 8;

#[derive(Default)]
struct Task {
    launch_path: Option<String>,
    arguments: Vec<String>,
    directory: Option<String>,
    environment: Option<Vec<(String, String)>>,
    /// Standard input, output and error: a retained pipe or file handle, or nil.
    stdio: [usize; 3],
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    launched: bool,
    pid: libc::pid_t,
    /// Darwin's wait status, once the child has been reaped.
    status: Option<c_int>,
}

impl Drop for Task {
    fn drop(&mut self) {
        for object in self.stdio {
            objc::arc::release(object as Id);
        }
    }
}

fn task_class() -> &'static Class {
    register::<Task>(c"NSTask", None)
}

/// # Safety
///
/// `object` must be null or an object this runtime allocated.
unsafe fn task_of<'a>(object: Id) -> Option<&'a mut Task> {
    unsafe { contents(object, task_class()) }
}

/// # Safety
///
/// `string` must be null or a string object.
unsafe fn text(string: Id) -> Option<String> {
    (!string.is_null())
        .then(|| String::from_utf8_lossy(&unsafe { crate::string::bytes(string) }).into_owned())
}

fn autoreleased_string(text: &str) -> Id {
    let string = crate::string::from_str(text);
    // SAFETY: a fresh string.
    unsafe { corefoundation::api::CFAutorelease(string as CFTypeRef) as Id }
}

/// The path of a file URL, as `-setExecutableURL:` and friends take one.
///
/// # Safety
///
/// `url` must be null or an `NSURL`.
unsafe fn path_of_url(url: Id) -> Option<String> {
    unsafe { corefoundation::url::as_url(url as CFTypeRef) }.map(|url| url.path())
}

fn autoreleased_file_url(path: &str) -> Id {
    let url = corefoundation::url::new_file_url(path, false);
    // SAFETY: a fresh URL.
    unsafe { corefoundation::api::CFAutorelease(url) as Id }
}

unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an instance of a class registered for `Task`.
    unsafe { fill(receiver, Task::default()) };
    receiver
}

unsafe extern "C" fn set_launch_path(receiver: Id, _sel: Sel, path: Id) {
    // SAFETY: a task, and an `NSString` or nil.
    if let Some(task) = unsafe { task_of(receiver) } {
        task.launch_path = unsafe { text(path) };
    }
}

unsafe extern "C" fn launch_path(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a task.
    match unsafe { task_of(receiver) }.and_then(|task| task.launch_path.as_deref()) {
        Some(path) => autoreleased_string(path),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn set_executable_url(receiver: Id, _sel: Sel, url: Id) {
    // SAFETY: a task, and an `NSURL` or nil.
    if let Some(task) = unsafe { task_of(receiver) } {
        task.launch_path = unsafe { path_of_url(url) };
    }
}

unsafe extern "C" fn executable_url(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a task.
    match unsafe { task_of(receiver) }.and_then(|task| task.launch_path.as_deref()) {
        Some(path) => autoreleased_file_url(path),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn set_arguments(receiver: Id, _sel: Sel, list: Id) {
    // SAFETY: a task.
    let Some(task) = (unsafe { task_of(receiver) }) else {
        return;
    };
    // SAFETY: an `NSArray` of strings, or nil.
    task.arguments = unsafe { corefoundation::collections::array(list as CFTypeRef) }
        .map(|array| {
            array
                .values
                .iter()
                .map(|value| unsafe { text(*value as Id) }.unwrap_or_default())
                .collect()
        })
        .unwrap_or_default();
}

unsafe extern "C" fn arguments(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a task.
    let Some(task) = (unsafe { task_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    let strings: Vec<Id> = task
        .arguments
        .iter()
        .map(|argument| crate::string::from_str(argument))
        .collect();
    let values: Vec<CFTypeRef> = strings.iter().map(|s| *s as CFTypeRef).collect();
    let array = corefoundation::collections::new_array(
        &values,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for string in strings {
        objc::arc::release(string);
    }
    // SAFETY: a fresh array.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn set_directory_path(receiver: Id, _sel: Sel, path: Id) {
    // SAFETY: a task, and an `NSString` or nil.
    if let Some(task) = unsafe { task_of(receiver) } {
        task.directory = unsafe { text(path) };
    }
}

/// `-currentDirectoryPath`, which is the parent's until one is set.
unsafe extern "C" fn directory_path(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a task.
    let Some(task) = (unsafe { task_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    match &task.directory {
        Some(path) => autoreleased_string(path),
        None => std::env::current_dir()
            .ok()
            .and_then(|dir| prefix::ns::to_guest(&dir))
            .map_or(std::ptr::null_mut(), |dir| {
                autoreleased_string(&dir.to_string_lossy())
            }),
    }
}

unsafe extern "C" fn set_directory_url(receiver: Id, _sel: Sel, url: Id) {
    // SAFETY: a task, and an `NSURL` or nil.
    if let Some(task) = unsafe { task_of(receiver) } {
        task.directory = unsafe { path_of_url(url) };
    }
}

unsafe extern "C" fn directory_url(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a task.
    match unsafe { task_of(receiver) }.and_then(|task| task.directory.as_deref()) {
        Some(path) => autoreleased_file_url(path),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn set_environment(receiver: Id, _sel: Sel, dictionary: Id) {
    // SAFETY: a task.
    let Some(task) = (unsafe { task_of(receiver) }) else {
        return;
    };
    // SAFETY: an `NSDictionary` of strings, or nil.
    task.environment = unsafe { corefoundation::collections::dictionary(dictionary as CFTypeRef) }
        .map(|dictionary| {
            dictionary
                .entries()
                .iter()
                .filter_map(|(key, value)| unsafe {
                    Some((text(*key as Id)?, text(*value as Id)?))
                })
                .collect()
        });
}

/// `-environment`: only what was set. Nil means the parent's, as on macOS.
unsafe extern "C" fn environment(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a task.
    let Some(pairs) = (unsafe { task_of(receiver) }).and_then(|task| task.environment.as_ref())
    else {
        return std::ptr::null_mut();
    };
    let strings: Vec<(Id, Id)> = pairs
        .iter()
        .map(|(key, value)| (crate::string::from_str(key), crate::string::from_str(value)))
        .collect();
    let entries: Vec<(CFTypeRef, CFTypeRef)> = strings
        .iter()
        .map(|(key, value)| (*key as CFTypeRef, *value as CFTypeRef))
        .collect();
    let dictionary = corefoundation::collections::new_dictionary(
        &entries,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for (key, value) in strings {
        objc::arc::release(key);
        objc::arc::release(value);
    }
    // SAFETY: a fresh dictionary.
    unsafe { corefoundation::api::CFAutorelease(dictionary) as Id }
}

fn set_stdio(receiver: Id, slot: usize, object: Id) {
    // SAFETY: a task.
    if let Some(task) = unsafe { task_of(receiver) } {
        let old = std::mem::replace(&mut task.stdio[slot], objc::arc::retain(object) as usize);
        objc::arc::release(old as Id);
    }
}

fn stdio(receiver: Id, slot: usize) -> Id {
    // SAFETY: a task.
    unsafe { task_of(receiver) }.map_or(std::ptr::null_mut(), |task| task.stdio[slot] as Id)
}

unsafe extern "C" fn set_standard_input(receiver: Id, _sel: Sel, object: Id) {
    set_stdio(receiver, 0, object);
}

unsafe extern "C" fn set_standard_output(receiver: Id, _sel: Sel, object: Id) {
    set_stdio(receiver, 1, object);
}

unsafe extern "C" fn set_standard_error(receiver: Id, _sel: Sel, object: Id) {
    set_stdio(receiver, 2, object);
}

unsafe extern "C" fn standard_input(receiver: Id, _sel: Sel) -> Id {
    stdio(receiver, 0)
}

unsafe extern "C" fn standard_output(receiver: Id, _sel: Sel) -> Id {
    stdio(receiver, 1)
}

unsafe extern "C" fn standard_error(receiver: Id, _sel: Sel) -> Id {
    stdio(receiver, 2)
}

/// Start the child, or answer the errno that stopped it.
///
/// A task that cannot start is recorded as having exited with 127, as a
/// shell reports a command it could not run. Apple raises from `-launch`
/// instead, and nothing here can raise, so a caller that checks
/// `-terminationStatus` still sees the failure rather than a success.
fn launch(task: &mut Task) -> Result<(), c_int> {
    let mut state = task.state.lock().unwrap_or_else(|e| e.into_inner());
    if state.launched {
        return Ok(());
    }
    state.launched = true;
    let result = spawn(task);
    match result {
        Ok(pid) => state.pid = pid,
        Err(error) => {
            eprintln!(
                "rosemary: foundation: NSTask could not launch {:?}: {}",
                task.launch_path.as_deref().unwrap_or("<no launch path>"),
                std::io::Error::from_raw_os_error(error)
            );
            state.status = Some(COULD_NOT_RUN);
        }
    }
    result.map(drop)
}

fn spawn(task: &Task) -> Result<libc::pid_t, c_int> {
    let c = |text: &str| CString::new(text).map_err(|_| libc::EINVAL);
    let path = c(task.launch_path.as_deref().ok_or(libc::ENOENT)?)?;
    let mut argv = vec![path.clone()];
    for argument in &task.arguments {
        argv.push(c(argument)?);
    }
    let envp = match &task.environment {
        Some(pairs) => Some(
            pairs
                .iter()
                .map(|(key, value)| c(&format!("{key}={value}")))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        None => None,
    };
    let directory = task.directory.as_deref().map(c).transpose()?;

    let mut fds = [None; 3];
    let mut parent_copies = Vec::new();
    for (slot, object) in task.stdio.iter().enumerate() {
        // SAFETY: a pipe, a file handle, or nil.
        if let Some((fd, end)) = unsafe { crate::filehandle::child_end(*object as Id, slot == 0) } {
            fds[slot] = Some(fd);
            parent_copies.extend(end);
        }
    }
    let spawned =
        darwin::spawn::spawn_child(&path, &argv, envp.as_deref(), directory.as_deref(), fds);
    // Closed whether or not the child started: a task that could not launch
    // reports itself exited, and a reader of its output pipe has to see end of
    // file rather than wait on a writer that never existed.
    for end in parent_copies {
        // SAFETY: a pipe end this task handed to the child.
        unsafe { crate::filehandle::close(end) };
    }
    spawned
}

unsafe extern "C" fn launch_task(receiver: Id, _sel: Sel) {
    // SAFETY: a task.
    if let Some(task) = unsafe { task_of(receiver) } {
        let _ = launch(task);
    }
}

unsafe extern "C" fn launch_and_return_error(receiver: Id, _sel: Sel, error_out: *mut Id) -> i8 {
    // SAFETY: a task.
    let Some(task) = (unsafe { task_of(receiver) }) else {
        return 0;
    };
    match launch(task) {
        Ok(()) => 1,
        Err(errno) => {
            if !error_out.is_null() {
                let message = std::io::Error::from_raw_os_error(errno).to_string();
                // SAFETY: the caller's writable `NSError **`.
                unsafe {
                    *error_out = crate::error::make("NSPOSIXErrorDomain", errno.into(), &message)
                };
            }
            0
        }
    }
}

/// The child's Darwin wait status, reaping it if it has exited. `block`
/// waits for it to exit first.
fn reap(task: &Task, block: bool) -> Option<c_int> {
    loop {
        let pid = {
            let state = task.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.launched || state.status.is_some() {
                return state.status;
            }
            state.pid
        };
        // Waited on outside the lock, so `-isRunning` on another thread is
        // not held up by `-waitUntilExit` on this one.
        match darwin::spawn::wait_child(pid, block) {
            Ok(Some(status)) => {
                let mut state = task.state.lock().unwrap_or_else(|e| e.into_inner());
                state.status = Some(status);
                return Some(status);
            }
            Ok(None) => return None,
            // Another thread reaped it and is about to record the status.
            Err(_) if block => std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(_) => {
                return task.state.lock().unwrap_or_else(|e| e.into_inner()).status;
            }
        }
    }
}

unsafe extern "C" fn wait_until_exit(receiver: Id, _sel: Sel) {
    // SAFETY: a task.
    if let Some(task) = unsafe { task_of(receiver) } {
        reap(task, true);
    }
}

unsafe extern "C" fn is_running(receiver: Id, _sel: Sel) -> i8 {
    // SAFETY: a task.
    let Some(task) = (unsafe { task_of(receiver) }) else {
        return 0;
    };
    let launched = task
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .launched;
    i8::from(launched && reap(task, false).is_none())
}

/// `-terminationStatus`: the exit code, or the signal that ended the child.
unsafe extern "C" fn termination_status(receiver: Id, _sel: Sel) -> c_int {
    // SAFETY: a task.
    let Some(status) = (unsafe { task_of(receiver) }).and_then(|task| reap(task, false)) else {
        return 0;
    };
    match status & 0x7f {
        0 => (status >> 8) & 0xff,
        signal => signal,
    }
}

unsafe extern "C" fn termination_reason(receiver: Id, _sel: Sel) -> i64 {
    // SAFETY: a task.
    match unsafe { task_of(receiver) }.and_then(|task| reap(task, false)) {
        Some(status) if status & 0x7f != 0 => REASON_UNCAUGHT_SIGNAL,
        _ => REASON_EXIT,
    }
}

unsafe extern "C" fn process_identifier(receiver: Id, _sel: Sel) -> c_int {
    // SAFETY: a task.
    unsafe { task_of(receiver) }.map_or(0, |task| {
        task.state.lock().unwrap_or_else(|e| e.into_inner()).pid
    })
}

fn signal(receiver: Id, host_signal: c_int) {
    // SAFETY: a task.
    let Some(task) = (unsafe { task_of(receiver) }) else {
        return;
    };
    let state = task.state.lock().unwrap_or_else(|e| e.into_inner());
    // Never a reaped child's pid, which may belong to someone else by now.
    if state.pid > 0 && state.status.is_none() {
        // SAFETY: a child of this process.
        unsafe { libc::kill(state.pid, host_signal) };
    }
}

unsafe extern "C" fn terminate(receiver: Id, _sel: Sel) {
    signal(receiver, libc::SIGTERM);
}

unsafe extern "C" fn interrupt(receiver: Id, _sel: Sel) {
    signal(receiver, libc::SIGINT);
}

/// `+launchedTaskWithLaunchPath:arguments:`.
unsafe extern "C" fn launched_task(_class: Id, _sel: Sel, path: Id, list: Id) -> Id {
    let task = crate::boxed::make(task_class(), Task::default());
    // SAFETY: a task just made, and the caller's string and array.
    unsafe {
        set_launch_path(task, objc::sel::get("setLaunchPath:"), path);
        set_arguments(task, objc::sel::get("setArguments:"), list);
        launch_task(task, objc::sel::get("launch"));
    }
    objc::arc::autorelease(task)
}

pub(crate) fn install() {
    let class = task_class();
    class_methods!(class, {
        "launchedTaskWithLaunchPath:arguments:" => launched_task, c"@32@0:8@16@24";
    });
    methods!(class, {
        "init" => init, c"@16@0:8";
        "setLaunchPath:" => set_launch_path, c"v24@0:8@16";
        "launchPath" => launch_path, c"@16@0:8";
        "setExecutableURL:" => set_executable_url, c"v24@0:8@16";
        "executableURL" => executable_url, c"@16@0:8";
        "setArguments:" => set_arguments, c"v24@0:8@16";
        "arguments" => arguments, c"@16@0:8";
        "setCurrentDirectoryPath:" => set_directory_path, c"v24@0:8@16";
        "currentDirectoryPath" => directory_path, c"@16@0:8";
        "setCurrentDirectoryURL:" => set_directory_url, c"v24@0:8@16";
        "currentDirectoryURL" => directory_url, c"@16@0:8";
        "setEnvironment:" => set_environment, c"v24@0:8@16";
        "environment" => environment, c"@16@0:8";
        "setStandardInput:" => set_standard_input, c"v24@0:8@16";
        "setStandardOutput:" => set_standard_output, c"v24@0:8@16";
        "setStandardError:" => set_standard_error, c"v24@0:8@16";
        "standardInput" => standard_input, c"@16@0:8";
        "standardOutput" => standard_output, c"@16@0:8";
        "standardError" => standard_error, c"@16@0:8";
        "launch" => launch_task, c"v16@0:8";
        "launchAndReturnError:" => launch_and_return_error, c"c24@0:8^@16";
        "waitUntilExit" => wait_until_exit, c"v16@0:8";
        "isRunning" => is_running, c"c16@0:8";
        "terminationStatus" => termination_status, c"i16@0:8";
        "terminationReason" => termination_reason, c"q16@0:8";
        "processIdentifier" => process_identifier, c"i16@0:8";
        "terminate" => terminate, c"v16@0:8";
        "interrupt" => interrupt, c"v16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send(receiver: Id, selector: &str, argument: Id) {
        // SAFETY: every selector sent here is `void (id, SEL, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get(selector), argument);
        }
    }

    fn new_task() -> Id {
        crate::bootstrap();
        let task = objc::object::create_instance(task_class(), 0);
        // SAFETY: a fresh instance of the class.
        unsafe { init(task, objc::sel::get("init")) }
    }

    #[cfg(not(target_os = "macos"))]
    fn strings(values: &[&str]) -> Id {
        let strings: Vec<Id> = values.iter().map(|v| crate::string::from_str(v)).collect();
        let refs: Vec<CFTypeRef> = strings.iter().map(|s| *s as CFTypeRef).collect();
        let array = corefoundation::collections::new_array(
            &refs,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        );
        strings.into_iter().for_each(objc::arc::release);
        array as Id
    }

    /// Not on macOS, where `/bin/sh` is a Mach-O and so runs under the
    /// runtime, which a unit-test binary is not; `tests/spawn.rs` covers that.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_task_reports_its_output_and_exit_code() {
        let pool = objc::arc::pool_push();
        let task = new_task();
        let path = crate::string::from_str("/bin/sh");
        let arguments = strings(&["-c", "printf out; exit 3"]);
        // SAFETY: the declared prototypes, on objects this crate made.
        unsafe {
            let pipe = crate::filehandle::tests_pipe();
            send(task, "setLaunchPath:", path);
            send(task, "setArguments:", arguments);
            send(task, "setStandardOutput:", pipe);
            launch_task(task, objc::sel::get("launch"));
            let (_, reader) = crate::filehandle::tests_ends(pipe);
            assert_eq!(crate::filehandle::tests_read_all(reader), b"out");
            wait_until_exit(task, objc::sel::get("waitUntilExit"));
            assert_eq!(is_running(task, objc::sel::get("isRunning")), 0);
            assert_eq!(
                termination_status(task, objc::sel::get("terminationStatus")),
                3
            );
            assert_eq!(
                termination_reason(task, objc::sel::get("terminationReason")),
                REASON_EXIT
            );
        }
        objc::arc::release(path);
        objc::arc::release(arguments);
        objc::arc::release(task);
        objc::arc::pool_pop(pool);
    }

    #[test]
    fn a_task_that_cannot_launch_says_so() {
        let pool = objc::arc::pool_push();
        let task = new_task();
        let path = crate::string::from_str("/rosemary-no-such-directory/tool");
        send(task, "setLaunchPath:", path);
        let pipe = crate::filehandle::tests_pipe();
        send(task, "setStandardOutput:", pipe);
        let mut error: Id = std::ptr::null_mut();
        // SAFETY: the declared prototypes, on a task this test made.
        unsafe {
            let launched =
                launch_and_return_error(task, objc::sel::get("launchAndReturnError:"), &mut error);
            assert_eq!(launched, 0);
            assert!(!error.is_null());
            assert_eq!(
                termination_status(task, objc::sel::get("terminationStatus")),
                127
            );
        }
        // The output pipe ends rather than waiting on a child that never ran.
        let (_, reader) = crate::filehandle::tests_ends(pipe);
        assert!(crate::filehandle::tests_read_all(reader).is_empty());
        objc::arc::release(path);
        objc::arc::release(task);
        objc::arc::pool_pop(pool);
    }
}
