mod common;

use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};

/// A listener and a client of it, as one program: `server <name>` answers each
/// request's `value` plus one until a request carries `quit`; `client <name>`
/// asks synchronously, then with a reply handler, then tells it to quit.
const XPC_C: &str = r#"
#include <dispatch/dispatch.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <xpc/xpc.h>

static void serve(const char *name) {
    xpc_connection_t listener = xpc_connection_create_mach_service(
        name, NULL, XPC_CONNECTION_MACH_SERVICE_LISTENER);
    xpc_connection_set_event_handler(listener, ^(xpc_object_t peer) {
        if (xpc_get_type(peer) != XPC_TYPE_CONNECTION) return;
        xpc_connection_set_event_handler(peer, ^(xpc_object_t message) {
            if (xpc_get_type(message) != XPC_TYPE_DICTIONARY) return;
            if (xpc_dictionary_get_bool(message, "quit")) exit(0);
            xpc_object_t reply = xpc_dictionary_create_reply(message);
            int64_t value = xpc_dictionary_get_int64(message, "value");
            xpc_dictionary_set_int64(reply, "answer", value + 1);
            xpc_dictionary_set_int64(reply, "client", xpc_connection_get_pid(peer));
            xpc_dictionary_set_string(reply, "from", "server");
            xpc_connection_send_message(peer, reply);
            xpc_release(reply);
        });
        xpc_connection_resume(peer);
    });
    xpc_connection_resume(listener);
    printf("ready %d\n", getpid());
    fflush(stdout);
    dispatch_semaphore_wait(dispatch_semaphore_create(0), DISPATCH_TIME_FOREVER);
}

static int ask(const char *name) {
    xpc_connection_t client = xpc_connection_create_mach_service(name, NULL, 0);
    dispatch_semaphore_t invalidated = dispatch_semaphore_create(0);
    xpc_connection_set_event_handler(client, ^(xpc_object_t event) {
        if (event == XPC_ERROR_CONNECTION_INVALID) {
            printf("invalid\n");
            dispatch_semaphore_signal(invalidated);
        }
    });
    xpc_connection_resume(client);

    for (int64_t value = 1; value <= 3; value++) {
        xpc_object_t message = xpc_dictionary_create(NULL, NULL, 0);
        xpc_dictionary_set_int64(message, "value", value * 10);
        xpc_object_t reply = xpc_connection_send_message_with_reply_sync(client, message);
        xpc_release(message);
        if (xpc_get_type(reply) != XPC_TYPE_DICTIONARY) {
            printf("error %s\n", xpc_dictionary_get_string(reply, XPC_ERROR_KEY_DESCRIPTION));
            return 1;
        }
        printf("sync %lld %s client-is-me %d\n",
               (long long)xpc_dictionary_get_int64(reply, "answer"),
               xpc_dictionary_get_string(reply, "from"),
               xpc_dictionary_get_int64(reply, "client") == getpid());
        xpc_release(reply);
    }

    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    xpc_object_t message = xpc_dictionary_create(NULL, NULL, 0);
    xpc_dictionary_set_int64(message, "value", 99);
    xpc_connection_send_message_with_reply(client, message, NULL, ^(xpc_object_t reply) {
        printf("async %lld\n", (long long)xpc_dictionary_get_int64(reply, "answer"));
        dispatch_semaphore_signal(done);
    });
    xpc_release(message);
    dispatch_semaphore_wait(done, DISPATCH_TIME_FOREVER);

    xpc_object_t quit = xpc_dictionary_create(NULL, NULL, 0);
    xpc_dictionary_set_bool(quit, "quit", true);
    xpc_connection_send_message(client, quit);
    xpc_release(quit);
    // A cancelled connection's handler hears XPC_ERROR_CONNECTION_INVALID last.
    xpc_connection_cancel(client);
    dispatch_semaphore_wait(invalidated, DISPATCH_TIME_FOREVER);
    printf("done\n");
    fflush(stdout);
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    if (strcmp(argv[1], "server") == 0) serve(argv[2]);
    return ask(argv[2]);
}
"#;

fn fixture() -> Option<String> {
    static PATH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    PATH.get_or_init(|| common::build_x86_64("xpc", XPC_C, &[]))
        .clone()
}

/// Darwin's own libxpc on a Mac would answer instead under the native libc.
const BUILTIN: [(&str, &str); 2] = [("ROSEMARY_LIBC", "builtin"), ("ROSEMARY_OBJC", "builtin")];

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The server, once it has said it listens.
fn serve(path: &str, service: &str) -> Server {
    let mut command = common::driver_command();
    command
        .args(["run", "--emulated", path, "server", service])
        .envs(BUILTIN)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command.spawn().expect("starting the server");
    let mut line = String::new();
    BufReader::new(child.stdout.take().expect("piped"))
        .read_line(&mut line)
        .expect("the server's first line");
    assert!(line.starts_with("ready"), "the server said {line:?}");
    Server(child)
}

#[test]
fn two_guests_talk_over_a_mach_service() {
    let Some(path) = fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let service = format!("org.rosemary.test.xpc.{}", std::process::id());
    let mut server = serve(&path, &service);

    let (status, out, err) = common::run_guest_env(&BUILTIN, &path, &["client", &service]);
    assert_eq!(status, Some(0), "stdout: {out}\nstderr: {err}");
    for value in [11, 21, 31] {
        assert!(
            out.contains(&format!("sync {value} server")),
            "the synchronous replies: {out}\nstderr: {err}"
        );
    }
    // Only Linux's kernel says which process sent a datagram.
    if cfg!(target_os = "linux") {
        assert!(
            !out.contains("client-is-me 0"),
            "the server saw another pid: {out}"
        );
    }
    assert!(
        out.contains("async 100\n"),
        "the reply handler: {out}\nstderr: {err}"
    );
    assert!(
        out.ends_with("async 100\ninvalid\ndone\n"),
        "cancelling ends with invalid: {out}"
    );

    let exited = (0..100).find_map(|_| {
        std::thread::sleep(std::time::Duration::from_millis(50));
        server.0.try_wait().ok().flatten()
    });
    assert_eq!(
        exited.and_then(|s| s.code()),
        Some(0),
        "the server quit when told"
    );
}

#[test]
fn a_service_nobody_offers_is_invalid_to_its_client() {
    let Some(path) = fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let service = format!("org.rosemary.test.xpc.absent.{}", std::process::id());
    let (status, out, err) = common::run_guest_env(&BUILTIN, &path, &["client", &service]);
    assert_eq!(status, Some(1), "stdout: {out}\nstderr: {err}");
    assert!(
        out.contains("error Connection invalid"),
        "{out}\nstderr: {err}"
    );
}
