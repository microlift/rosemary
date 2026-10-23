//! Libnotify's client calls: `notify_post` and the registrations a post
//! reaches.
//!
//! Apple's `notify_client.c` keeps a table of registrations and has `notifyd`
//! fan a post out to every process registered for the name. There is no
//! `notifyd` here, so the table is this process's: a post reaches every
//! registration this process made, which is the whole of Apple's behaviour for
//! a `self.` name and what a process sees of its own posts for any other. What
//! it does not see is a post from another process, or one the system makes -
//! `com.apple.system.timezone` and the like - which nothing on this host posts.
//! A registration for one of those is accepted and stays quiet, which is what a
//! Mac does between the events.
//!
//! The delivery methods are Apple's: a block on a dispatch queue, the token in
//! network byte order written to a pipe, an empty Mach message whose `msgh_id`
//! is the token, a signal, or nothing but `notify_check`.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int};
use std::sync::{Mutex, MutexGuard};

use crate::mach::space::{self, Name};

/// `NOTIFY_STATUS_*`, from `<notify.h>`.
const OK: u32 = 0;
const INVALID_NAME: u32 = 1;
const INVALID_TOKEN: u32 = 2;
const INVALID_PORT: u32 = 3;
const INVALID_FILE: u32 = 4;
const INVALID_SIGNAL: u32 = 5;
const NULL_INPUT: u32 = 10;
const FAILED: u32 = 1_000_000;

/// `NOTIFY_REUSE`: the port or descriptor passed in is one an earlier
/// registration returned, to be used again.
const REUSE: c_int = 0x1;

/// How a registration is told.
enum Delivery {
    /// `notify_register_check`: only `notify_check` asks.
    Check,
    /// `notify_register_dispatch`: a copied `void (^)(int token)` and the queue
    /// it is submitted to, retained.
    Dispatch { queue: u64, block: u64 },
    /// `notify_register_file_descriptor`: the write end of a pipe this module
    /// made, shared by every registration that reused its read end.
    File { read: c_int },
    /// `notify_register_mach_port`: a port in this task's space.
    Port { port: Name },
    /// `notify_register_signal`: a Darwin signal number.
    Signal { signal: c_int },
}

struct Registration {
    name: String,
    delivery: Delivery,
    /// What `notify_check` answers next: true at first, and after a post.
    posted: bool,
    /// `notify_suspend`'s nesting, and whether a post came in meanwhile.
    suspended: u32,
    deferred: bool,
}

/// A pipe this module made for `notify_register_file_descriptor`, and how many
/// registrations use it. Both ends close with the last of them.
struct Pipe {
    write: c_int,
    users: usize,
}

/// A port this module allocated for `notify_register_mach_port`, and how many
/// registrations use it. Apple deallocates it with the last of them.
struct Port {
    users: usize,
}

#[derive(Default)]
struct Center {
    next: i32,
    registrations: HashMap<i32, Registration>,
    /// Each name's state, for `notify_set_state` and `notify_get_state`.
    state: HashMap<String, u64>,
    pipes: HashMap<c_int, Pipe>,
    ports: HashMap<Name, Port>,
}

fn center() -> MutexGuard<'static, Center> {
    static CENTER: std::sync::LazyLock<Mutex<Center>> =
        std::sync::LazyLock::new(|| Mutex::new(Center::default()));
    CENTER.lock().unwrap_or_else(|e| e.into_inner())
}

/// The name a caller passed, or `None` for null or something that is not
/// UTF-8 - neither of which Apple's table could hold either.
///
/// # Safety
///
/// `name` is null or NUL-terminated.
unsafe fn name_of(name: *const c_char) -> Option<String> {
    if name.is_null() {
        return None;
    }
    // SAFETY: the caller's guarantee.
    unsafe { CStr::from_ptr(name) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

impl Center {
    fn register(&mut self, name: String, delivery: Delivery) -> i32 {
        let token = self.next;
        self.next += 1;
        self.registrations.insert(
            token,
            Registration {
                name,
                delivery,
                posted: true,
                suspended: 0,
                deferred: false,
            },
        );
        token
    }
}

/// Tell one registration, outside the table's lock: a block may call back in.
enum Send {
    Dispatch { queue: u64, block: u64 },
    File { write: c_int },
    Port { port: Name },
    Signal { signal: c_int },
}

fn send(token: i32, how: Send) {
    match how {
        Send::Dispatch { queue, block } => dispatch(queue, block, token),
        Send::File { write } => {
            let value = (token as u32).to_be_bytes();
            // SAFETY: a pipe this module opened. A full pipe drops the post,
            // as Apple's does: the reader has not caught up with the last one.
            unsafe {
                libc::write(write, value.as_ptr().cast(), value.len());
            }
        }
        Send::Port { port } => message(port, token),
        Send::Signal { signal } => {
            if let Some(host) = crate::signal::signal_to_host(signal) {
                // SAFETY: a signal to this process, which registered for it.
                unsafe { libc::kill(libc::getpid(), host) };
            }
        }
    }
}

/// What delivering to `registration` takes, or `None` if it only waits for
/// `notify_check`. A copied block, so the registration can be cancelled while
/// the delivery is still queued.
fn sending(registration: &Registration) -> Option<Send> {
    match &registration.delivery {
        Delivery::Check => None,
        Delivery::Dispatch { queue, block } => {
            let block = crate::dispatch::block::copy(*block)?;
            crate::dispatch::object::retain(*queue);
            Some(Send::Dispatch {
                queue: *queue,
                block,
            })
        }
        Delivery::File { read } => Some(Send::File {
            write: center_pipe_write(*read)?,
        }),
        Delivery::Port { port } => Some(Send::Port { port: *port }),
        Delivery::Signal { signal } => Some(Send::Signal { signal: *signal }),
    }
}

/// Filled in by [`notify_post`] before it lets go of the table, so the pipe
/// lookup does not need the lock a second time.
fn center_pipe_write(read: c_int) -> Option<c_int> {
    PIPE_WRITES.with(|w| w.borrow().get(&read).copied())
}

thread_local! {
    static PIPE_WRITES: std::cell::RefCell<HashMap<c_int, c_int>> =
        std::cell::RefCell::new(HashMap::new());
}

/// One block call waiting on a queue.
struct Call {
    block: u64,
    queue: u64,
    token: i32,
}

extern "C" fn run(context: u64) {
    // SAFETY: a `Call` boxed by `dispatch`, taken back once.
    let call = unsafe { Box::from_raw(context as *mut Call) };
    // SAFETY: a copied `notify_handler_t`, `void (^)(int token)`, whose
    // `invoke` takes the block and the token.
    unsafe {
        let invoke = ((call.block + 16) as *const u64).read();
        if invoke != 0 {
            let f: extern "C" fn(u64, c_int) = std::mem::transmute(invoke);
            f(call.block, call.token);
        }
    }
    crate::dispatch::block::release(call.block);
    // SAFETY: the reference `sending` took.
    unsafe { crate::dispatch::object::release(call.queue) };
}

fn dispatch(queue: u64, block: u64, token: i32) {
    let call = Box::into_raw(Box::new(Call {
        block,
        queue,
        token,
    }));
    // SAFETY: a live queue, and `run` takes back the box it is given.
    unsafe { crate::dispatch::queue::async_f(queue, call as u64, run as *const () as u64) };
}

/// `MACH_MSG_TYPE_COPY_SEND`, `MACH_SEND_MSG` and `MACH_SEND_TIMEOUT`.
const COPY_SEND: u32 = 19;
const SEND_MSG: i32 = 0x1;
const SEND_TIMEOUT: i32 = 0x10;

/// The empty message Apple sends: a bare header whose `msgh_id` is the token.
fn message(port: Name, token: i32) {
    #[repr(C)]
    struct Header {
        bits: u32,
        size: u32,
        remote: u32,
        local: u32,
        voucher: u32,
        id: i32,
    }
    let mut header = Header {
        bits: COPY_SEND,
        size: size_of::<Header>() as u32,
        remote: port,
        local: 0,
        voucher: 0,
        id: token,
    };
    // SAFETY: a whole message of the size in its header. Not waiting for room:
    // a full queue already holds a notification the receiver has not read.
    unsafe {
        crate::mach::msg::mach_msg(
            (&raw mut header).cast(),
            SEND_MSG | SEND_TIMEOUT,
            header.size,
            0,
            0,
            0,
            0,
        );
    }
}

/// `uint32_t notify_post(const char *name)`
///
/// # Safety
///
/// `name` is null or NUL-terminated.
pub unsafe extern "C" fn notify_post(name: *const c_char) -> u32 {
    // SAFETY: the caller's string.
    let Some(name) = (unsafe { name_of(name) }) else {
        return INVALID_NAME;
    };
    let sends: Vec<(i32, Send)> = {
        let mut center = center();
        let writes: HashMap<c_int, c_int> = center
            .pipes
            .iter()
            .map(|(&read, pipe)| (read, pipe.write))
            .collect();
        PIPE_WRITES.with(|w| *w.borrow_mut() = writes);
        center
            .registrations
            .iter_mut()
            .filter(|(_, r)| r.name == name)
            .filter_map(|(&token, r)| {
                r.posted = true;
                if r.suspended > 0 {
                    r.deferred = true;
                    return None;
                }
                sending(r).map(|how| (token, how))
            })
            .collect()
    };
    for (token, how) in sends {
        send(token, how);
    }
    OK
}

/// Write the token through `out`, which Apple requires be there.
///
/// # Safety
///
/// `out` is null or a writable `int`.
unsafe fn give(out: *mut c_int, token: i32) {
    // SAFETY: the caller's guarantee, checked by every caller before it
    // registered anything.
    unsafe { out.write(token) };
}

/// `uint32_t notify_register_check(const char *name, int *out_token)`
///
/// # Safety
///
/// `name` is null or NUL-terminated, `out_token` null or writable.
pub unsafe extern "C" fn notify_register_check(name: *const c_char, out_token: *mut c_int) -> u32 {
    // SAFETY: the caller's string.
    let Some(name) = (unsafe { name_of(name) }) else {
        return INVALID_NAME;
    };
    if out_token.is_null() {
        return NULL_INPUT;
    }
    let token = center().register(name, Delivery::Check);
    // SAFETY: checked above.
    unsafe { give(out_token, token) };
    OK
}

/// `uint32_t notify_register_dispatch(const char *name, int *out_token,
/// dispatch_queue_t queue, notify_handler_t handler)`
///
/// The queue is retained until the registration is cancelled, as `<notify.h>`
/// says, and the block copied.
///
/// # Safety
///
/// `name` is null or NUL-terminated, `out_token` null or writable, `queue` a
/// queue and `handler` a block, or null.
pub unsafe extern "C" fn notify_register_dispatch(
    name: *const c_char,
    out_token: *mut c_int,
    queue: u64,
    handler: u64,
) -> u32 {
    // SAFETY: the caller's string.
    let Some(name) = (unsafe { name_of(name) }) else {
        return INVALID_NAME;
    };
    if out_token.is_null() || queue == 0 || handler == 0 {
        return NULL_INPUT;
    }
    let Some(block) = crate::dispatch::block::copy(handler) else {
        return FAILED;
    };
    crate::dispatch::object::retain(queue);
    let token = center().register(name, Delivery::Dispatch { queue, block });
    // SAFETY: checked above.
    unsafe { give(out_token, token) };
    OK
}

/// `uint32_t notify_register_file_descriptor(const char *name, int *notify_fd,
/// int flags, int *out_token)`
///
/// Without `NOTIFY_REUSE`, a new pipe whose read end is handed back; with it,
/// the read end an earlier call handed back, which must be one of this
/// module's.
///
/// # Safety
///
/// `name` is null or NUL-terminated, `notify_fd` and `out_token` null or
/// readable and writable.
pub unsafe extern "C" fn notify_register_file_descriptor(
    name: *const c_char,
    notify_fd: *mut c_int,
    flags: c_int,
    out_token: *mut c_int,
) -> u32 {
    // SAFETY: the caller's string.
    let Some(name) = (unsafe { name_of(name) }) else {
        return INVALID_NAME;
    };
    if notify_fd.is_null() {
        return INVALID_FILE;
    }
    if out_token.is_null() {
        return NULL_INPUT;
    }
    let mut center = center();
    let read = match flags & REUSE {
        0 => {
            let mut ends = [0; 2];
            // SAFETY: a two-element array for the descriptors.
            if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
                return FAILED;
            }
            center.pipes.insert(
                ends[0],
                Pipe {
                    write: ends[1],
                    users: 0,
                },
            );
            ends[0]
        }
        _ => {
            // SAFETY: the caller's descriptor.
            let read = unsafe { notify_fd.read() };
            if !center.pipes.contains_key(&read) {
                return INVALID_FILE;
            }
            read
        }
    };
    if let Some(pipe) = center.pipes.get_mut(&read) {
        pipe.users += 1;
    }
    let token = center.register(name, Delivery::File { read });
    drop(center);
    // SAFETY: checked above.
    unsafe {
        notify_fd.write(read);
        give(out_token, token);
    }
    OK
}

/// `uint32_t notify_register_mach_port(const char *name,
/// mach_port_t *notify_port, int flags, int *out_token)`
///
/// Without `NOTIFY_REUSE`, a new receive right, handed back; with it, the port
/// passed in, to which a send right must be had.
///
/// # Safety
///
/// `name` is null or NUL-terminated, `notify_port` and `out_token` null or
/// readable and writable.
pub unsafe extern "C" fn notify_register_mach_port(
    name: *const c_char,
    notify_port: *mut Name,
    flags: c_int,
    out_token: *mut c_int,
) -> u32 {
    // SAFETY: the caller's string.
    let Some(name) = (unsafe { name_of(name) }) else {
        return INVALID_NAME;
    };
    if notify_port.is_null() {
        return INVALID_PORT;
    }
    if out_token.is_null() {
        return NULL_INPUT;
    }
    let port = match flags & REUSE {
        0 => {
            let mut port = space::MACH_PORT_NULL;
            // SAFETY: a live local for the out-parameter.
            let rc = unsafe {
                space::mach_port_allocate(space::TASK_SELF, space::RIGHT_RECEIVE, &mut port)
            };
            if rc != 0 {
                return FAILED;
            }
            center().ports.insert(port, Port { users: 0 });
            port
        }
        _ => {
            // SAFETY: the caller's port name.
            let port = unsafe { notify_port.read() };
            if space::address_of(port).is_none() {
                return INVALID_PORT;
            }
            port
        }
    };
    let mut center = center();
    if let Some(mine) = center.ports.get_mut(&port) {
        mine.users += 1;
    }
    let token = center.register(name, Delivery::Port { port });
    drop(center);
    // SAFETY: checked above.
    unsafe {
        notify_port.write(port);
        give(out_token, token);
    }
    OK
}

/// `uint32_t notify_register_signal(const char *name, int sig, int *out_token)`
///
/// # Safety
///
/// `name` is null or NUL-terminated, `out_token` null or writable.
pub unsafe extern "C" fn notify_register_signal(
    name: *const c_char,
    signal: c_int,
    out_token: *mut c_int,
) -> u32 {
    // SAFETY: the caller's string.
    let Some(name) = (unsafe { name_of(name) }) else {
        return INVALID_NAME;
    };
    if crate::signal::signal_to_host(signal).is_none() {
        return INVALID_SIGNAL;
    }
    if out_token.is_null() {
        return NULL_INPUT;
    }
    let token = center().register(name, Delivery::Signal { signal });
    // SAFETY: checked above.
    unsafe { give(out_token, token) };
    OK
}

/// `uint32_t notify_check(int token, int *check)`
///
/// # Safety
///
/// `check` is null or a writable `int`.
pub unsafe extern "C" fn notify_check(token: c_int, check: *mut c_int) -> u32 {
    if check.is_null() {
        return NULL_INPUT;
    }
    let mut center = center();
    let Some(registration) = center.registrations.get_mut(&token) else {
        return INVALID_TOKEN;
    };
    let posted = std::mem::take(&mut registration.posted);
    // SAFETY: checked above.
    unsafe { check.write(c_int::from(posted)) };
    OK
}

/// `uint32_t notify_cancel(int token)`
///
/// The queue is released and the block with it; a pipe or a port this module
/// made goes with the last registration using it.
pub extern "C" fn notify_cancel(token: c_int) -> u32 {
    let mut center = center();
    let Some(registration) = center.registrations.remove(&token) else {
        return INVALID_TOKEN;
    };
    match registration.delivery {
        Delivery::Dispatch { queue, block } => {
            drop(center);
            crate::dispatch::block::release(block);
            // SAFETY: the reference taken at registration.
            unsafe { crate::dispatch::object::release(queue) };
        }
        Delivery::File { read } => {
            if let Some(pipe) = center.pipes.get_mut(&read) {
                pipe.users -= 1;
                if pipe.users == 0 {
                    let write = pipe.write;
                    center.pipes.remove(&read);
                    // SAFETY: the two ends of a pipe this module opened.
                    unsafe {
                        libc::close(read);
                        libc::close(write);
                    }
                }
            }
        }
        Delivery::Port { port } => {
            if let Some(mine) = center.ports.get_mut(&port) {
                mine.users -= 1;
                if mine.users == 0 {
                    center.ports.remove(&port);
                    drop(center);
                    space::mach_port_mod_refs(space::TASK_SELF, port, space::RIGHT_RECEIVE, -1);
                }
            }
        }
        Delivery::Check | Delivery::Signal { .. } => {}
    }
    OK
}

/// `uint32_t notify_suspend(int token)`
pub extern "C" fn notify_suspend(token: c_int) -> u32 {
    match center().registrations.get_mut(&token) {
        Some(registration) => {
            registration.suspended += 1;
            OK
        }
        None => INVALID_TOKEN,
    }
}

/// `uint32_t notify_resume(int token)`
///
/// The posts that came in while suspended are one notification, delivered
/// when the last suspension is lifted.
pub extern "C" fn notify_resume(token: c_int) -> u32 {
    let how = {
        let mut center = center();
        let writes: HashMap<c_int, c_int> = center
            .pipes
            .iter()
            .map(|(&read, pipe)| (read, pipe.write))
            .collect();
        PIPE_WRITES.with(|w| *w.borrow_mut() = writes);
        let Some(registration) = center.registrations.get_mut(&token) else {
            return INVALID_TOKEN;
        };
        if registration.suspended > 0 {
            registration.suspended -= 1;
        }
        match registration.suspended == 0 && std::mem::take(&mut registration.deferred) {
            true => sending(registration),
            false => None,
        }
    };
    if let Some(how) = how {
        send(token, how);
    }
    OK
}

/// `uint32_t notify_set_state(int token, uint64_t state64)`
///
/// The state belongs to the name rather than the registration.
pub extern "C" fn notify_set_state(token: c_int, state: u64) -> u32 {
    let mut center = center();
    let Some(name) = center.registrations.get(&token).map(|r| r.name.clone()) else {
        return INVALID_TOKEN;
    };
    center.state.insert(name, state);
    OK
}

/// `uint32_t notify_get_state(int token, uint64_t *state64)`
///
/// # Safety
///
/// `state` is null or a writable `uint64_t`.
pub unsafe extern "C" fn notify_get_state(token: c_int, state: *mut u64) -> u32 {
    if state.is_null() {
        return NULL_INPUT;
    }
    let center = center();
    let Some(registration) = center.registrations.get(&token) else {
        return INVALID_TOKEN;
    };
    let value = center.state.get(&registration.name).copied().unwrap_or(0);
    // SAFETY: checked above.
    unsafe { state.write(value) };
    OK
}

/// `bool notify_is_valid_token(int val)`
pub extern "C" fn notify_is_valid_token(token: c_int) -> bool {
    center().registrations.contains_key(&token)
}

exports::exports! {
    functions {
        "notify_post" => notify_post,
        "notify_register_check" => notify_register_check,
        "notify_register_dispatch" => notify_register_dispatch,
        "notify_register_file_descriptor" => notify_register_file_descriptor,
        "notify_register_mach_port" => notify_register_mach_port,
        "notify_register_signal" => notify_register_signal,
        "notify_check" => notify_check,
        "notify_cancel" => notify_cancel,
        "notify_suspend" => notify_suspend,
        "notify_resume" => notify_resume,
        "notify_set_state" => notify_set_state,
        "notify_get_state" => notify_get_state,
        "notify_is_valid_token" => notify_is_valid_token,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cname(name: &str) -> std::ffi::CString {
        std::ffi::CString::new(format!("self.rosemary.test.{name}")).unwrap()
    }

    /// `notify_check` is true the first time, then only after a post.
    #[test]
    fn a_check_registration_sees_a_post_once() {
        let name = cname("check");
        let mut token = -1;
        // SAFETY: a NUL-terminated name and a live local.
        assert_eq!(
            unsafe { notify_register_check(name.as_ptr(), &mut token) },
            OK
        );
        assert!(notify_is_valid_token(token));
        let mut check = 0;
        unsafe { notify_check(token, &mut check) };
        assert_eq!(check, 1, "true the first time");
        unsafe { notify_check(token, &mut check) };
        assert_eq!(check, 0);
        unsafe { notify_post(name.as_ptr()) };
        unsafe { notify_check(token, &mut check) };
        assert_eq!(check, 1, "and after a post");
        assert_eq!(notify_cancel(token), OK);
        assert_eq!(notify_cancel(token), INVALID_TOKEN);
        assert!(!notify_is_valid_token(token));
    }

    /// The token, big-endian, down the pipe - and nothing while suspended
    /// until the suspension is lifted, when the posts are one.
    #[test]
    fn a_file_descriptor_registration_is_written_the_token() {
        let name = cname("fd");
        let (mut fd, mut token) = (-1, -1);
        // SAFETY: a NUL-terminated name and live locals.
        let rc = unsafe { notify_register_file_descriptor(name.as_ptr(), &mut fd, 0, &mut token) };
        assert_eq!(rc, OK);
        unsafe { notify_post(name.as_ptr()) };
        let mut word = [0u8; 4];
        // SAFETY: the pipe this module handed back.
        assert_eq!(unsafe { libc::read(fd, word.as_mut_ptr().cast(), 4) }, 4);
        assert_eq!(i32::from_be_bytes(word), token);

        notify_suspend(token);
        unsafe {
            notify_post(name.as_ptr());
            notify_post(name.as_ptr());
            libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
            assert_eq!(libc::read(fd, word.as_mut_ptr().cast(), 4), -1, "suspended");
        }
        notify_resume(token);
        unsafe {
            assert_eq!(libc::read(fd, word.as_mut_ptr().cast(), 4), 4);
            assert_eq!(libc::read(fd, word.as_mut_ptr().cast(), 4), -1, "coalesced");
        }

        // A second registration on the same descriptor keeps it open until
        // both are gone.
        let mut again = -1;
        let mut same = fd;
        let rc =
            unsafe { notify_register_file_descriptor(name.as_ptr(), &mut same, REUSE, &mut again) };
        assert_eq!((rc, same), (OK, fd));
        notify_cancel(token);
        assert!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0,
            "still in use"
        );
        notify_cancel(again);
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) },
            -1,
            "closed with the last"
        );
    }

    /// State belongs to the name: any registration for it reads what another set.
    #[test]
    fn state_is_shared_by_the_name() {
        let name = cname("state");
        let (mut a, mut b) = (-1, -1);
        unsafe {
            notify_register_check(name.as_ptr(), &mut a);
            notify_register_check(name.as_ptr(), &mut b);
        }
        assert_eq!(notify_set_state(a, 42), OK);
        let mut state = 0;
        assert_eq!(unsafe { notify_get_state(b, &mut state) }, OK);
        assert_eq!(state, 42);
        notify_cancel(a);
        notify_cancel(b);
        assert_eq!(unsafe { notify_get_state(a, &mut state) }, INVALID_TOKEN);
    }

    #[test]
    fn bad_input_is_refused() {
        let mut token = -1;
        unsafe {
            assert_eq!(
                notify_register_check(std::ptr::null(), &mut token),
                INVALID_NAME
            );
            assert_eq!(notify_post(std::ptr::null()), INVALID_NAME);
            let name = cname("bad");
            assert_eq!(
                notify_register_file_descriptor(name.as_ptr(), std::ptr::null_mut(), 0, &mut token),
                INVALID_FILE
            );
            let mut not_ours = 0;
            assert_eq!(
                notify_register_file_descriptor(name.as_ptr(), &mut not_ours, REUSE, &mut token),
                INVALID_FILE
            );
        }
    }
}
