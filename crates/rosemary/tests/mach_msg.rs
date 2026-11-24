mod common;

use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};

/// `server <name>` checks in a service and answers one request: every port in
/// its port array is sent the request's out-of-line bytes back. `client <name>`
/// makes that request and checks both answers.
const MACH_MSG_C: &str = r#"
#include <mach/mach.h>
#include <servers/bootstrap.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Larger than one datagram holds, so it cannot have crossed inline. */
#define LEN (256 * 1024 + 123)

typedef struct {
    mach_msg_header_t header;
    mach_msg_body_t body;
    mach_msg_ool_descriptor_t data;
    mach_msg_ool_ports_descriptor_t ports;
} request_t;

typedef struct {
    mach_msg_header_t header;
    mach_msg_body_t body;
    mach_msg_ool_descriptor_t data;
} reply_t;

static unsigned char pattern(size_t i) { return (unsigned char)(i * 31 + 7); }

static int serve(const char *name) {
    mach_port_t port;
    if (bootstrap_check_in(bootstrap_port, name, &port) != KERN_SUCCESS) return 3;
    printf("ready\n");
    fflush(stdout);

    struct { request_t request; mach_msg_trailer_t trailer; } in;
    kern_return_t kr = mach_msg(&in.request.header, MACH_RCV_MSG, 0, sizeof in, port, 0,
                                MACH_PORT_NULL);
    if (kr != KERN_SUCCESS) {
        fprintf(stderr, "server receive %#x\n", kr);
        return 4;
    }
    request_t *request = &in.request;
    if (request->data.size != LEN || request->ports.count != 2 ||
        request->ports.disposition != MACH_MSG_TYPE_PORT_SEND) {
        fprintf(stderr, "server got size %u count %u disposition %u\n", request->data.size,
                request->ports.count, request->ports.disposition);
        return 5;
    }
    mach_port_t *peers = request->ports.address;
    for (int i = 0; i < 2; i++) {
        reply_t out = {0};
        out.header.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_MOVE_SEND, 0) | MACH_MSGH_BITS_COMPLEX;
        out.header.msgh_size = sizeof out;
        out.header.msgh_remote_port = peers[i];
        out.header.msgh_id = 200 + i;
        out.body.msgh_descriptor_count = 1;
        out.data.address = request->data.address;
        out.data.size = request->data.size;
        out.data.copy = MACH_MSG_VIRTUAL_COPY;
        /* The last answer gives the region up instead of copying it. */
        out.data.deallocate = i == 1;
        out.data.type = MACH_MSG_OOL_DESCRIPTOR;
        kr = mach_msg(&out.header, MACH_SEND_MSG, sizeof out, 0, MACH_PORT_NULL, 0,
                      MACH_PORT_NULL);
        if (kr != KERN_SUCCESS) {
            fprintf(stderr, "server send %#x\n", kr);
            return 6;
        }
    }
    vm_deallocate(mach_task_self(), (vm_address_t)peers, 2 * sizeof *peers);
    return 0;
}

static int ask(const char *name) {
    mach_port_t server;
    if (bootstrap_look_up(bootstrap_port, name, &server) != KERN_SUCCESS) return 3;
    mach_port_t replies[2];
    for (int i = 0; i < 2; i++) {
        if (mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &replies[i])) return 4;
    }
    unsigned char *data = malloc(LEN);
    for (size_t i = 0; i < LEN; i++) data[i] = pattern(i);

    request_t out = {0};
    out.header.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0) | MACH_MSGH_BITS_COMPLEX;
    out.header.msgh_size = sizeof out;
    out.header.msgh_remote_port = server;
    out.header.msgh_id = 100;
    out.body.msgh_descriptor_count = 2;
    out.data.address = data;
    out.data.size = LEN;
    out.data.copy = MACH_MSG_PHYSICAL_COPY;
    out.data.type = MACH_MSG_OOL_DESCRIPTOR;
    out.ports.address = replies;
    out.ports.count = 2;
    out.ports.disposition = MACH_MSG_TYPE_MAKE_SEND;
    out.ports.type = MACH_MSG_OOL_PORTS_DESCRIPTOR;
    kern_return_t kr = mach_msg(&out.header, MACH_SEND_MSG, sizeof out, 0, MACH_PORT_NULL, 0,
                                MACH_PORT_NULL);
    if (kr != KERN_SUCCESS) {
        printf("send %#x\n", kr);
        return 5;
    }
    /* The server's copy is its own from the moment the send returns. */
    memset(data, 0, LEN);

    for (int i = 0; i < 2; i++) {
        struct { reply_t reply; mach_msg_trailer_t trailer; } in;
        kr = mach_msg(&in.reply.header, MACH_RCV_MSG, 0, sizeof in, replies[i], 0,
                      MACH_PORT_NULL);
        if (kr != KERN_SUCCESS) {
            printf("receive %#x\n", kr);
            return 6;
        }
        const unsigned char *back = in.reply.data.address;
        int same = in.reply.data.size == LEN && back != data;
        for (size_t j = 0; same && j < LEN; j++) same = back[j] == pattern(j);
        printf("reply %d id %d %s\n", i, in.reply.header.msgh_id, same ? "matches" : "differs");
        vm_deallocate(mach_task_self(), (vm_address_t)back, in.reply.data.size);
    }
    printf("done\n");
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    if (strcmp(argv[1], "server") == 0) return serve(argv[2]);
    return ask(argv[2]);
}
"#;

fn fixture() -> Option<String> {
    static PATH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    PATH.get_or_init(|| common::build_x86_64("mach_msg", MACH_MSG_C, &[]))
        .clone()
}

/// A Mac's own kernel would carry the messages instead under the native libc.
const BUILTIN: [(&str, &str); 2] = [("ROSEMARY_LIBC", "builtin"), ("ROSEMARY_OBJC", "builtin")];

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn out_of_line_memory_and_ports_cross_between_guests() {
    let Some(path) = fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let service = format!("org.rosemary.test.mach-msg.{}", std::process::id());
    let mut command = common::driver_command();
    command
        .args(["run", "--emulated", &path, "server", &service])
        .envs(BUILTIN)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut server = Server(command.spawn().expect("starting the server"));
    let mut line = String::new();
    BufReader::new(server.0.stdout.take().expect("piped"))
        .read_line(&mut line)
        .expect("the server's first line");
    assert!(line.starts_with("ready"), "the server said {line:?}");

    let (status, out, err) = common::run_guest_env(&BUILTIN, &path, &["client", &service]);
    assert_eq!(status, Some(0), "stdout: {out}\nstderr: {err}");
    assert_eq!(
        out, "reply 0 id 200 matches\nreply 1 id 201 matches\ndone\n",
        "stderr: {err}"
    );
    assert_eq!(
        server.0.wait().ok().and_then(|s| s.code()),
        Some(0),
        "the server answered both ports"
    );
}
