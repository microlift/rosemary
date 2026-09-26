#![cfg(not(target_os = "macos"))]

use std::ffi::c_void;

use super::kern::*;
use super::space::{self, MACH_PORT_DEAD, MACH_PORT_NULL, Name};
use super::wire::{self, Datagram, MAX_RIGHTS, Receiver};

/// `MACH_MSG_TYPE_*`, the dispositions a header field or descriptor can carry.
pub const MOVE_RECEIVE: u32 = 16;
pub const MOVE_SEND: u32 = 17;
pub const MOVE_SEND_ONCE: u32 = 18;
pub const COPY_SEND: u32 = 19;
pub const MAKE_SEND: u32 = 20;
pub const MAKE_SEND_ONCE: u32 = 21;

/// `MACH_MSGH_BITS_*`.
const BITS_COMPLEX: u32 = 0x8000_0000;
const BITS_PORTS_MASK: u32 = 0x1f;
const BITS_LOCAL_SHIFT: u32 = 8;

/// `MACH_SEND_MSG` and friends, from `<mach/message.h>`.
const SEND_MSG: i32 = 0x0000_0001;
const RCV_MSG: i32 = 0x0000_0002;
const RCV_LARGE: i32 = 0x0000_0004;
const SEND_TIMEOUT: i32 = 0x0000_0010;
const RCV_TIMEOUT: i32 = 0x0000_0100;

/// `MACH_MSG_PORT_DESCRIPTOR`; every other descriptor type is refused.
///
/// The type byte is at offset 11 of *every* 64-bit descriptor, whatever its
/// kind and whatever its length - the headers lay the bitfields out that way on
/// purpose, so a reader can tell what it is looking at before it knows how far
/// to step. That is what makes refusing an unknown descriptor possible without
/// first knowing its size.
const DESCRIPTOR_TYPE_AT: usize = 11;
/// The disposition byte, one before it.
const DESCRIPTOR_DISPOSITION_AT: usize = 10;
const DESCRIPTOR_PORT: u8 = 0;

/// `mach_msg_header_t`, `mach_msg_body_t` and `mach_msg_port_descriptor_t`, in
/// bytes. All three are the same size on x86-64 and arm64.
const HEADER: usize = 24;
const BODY: usize = 4;
const DESCRIPTOR: usize = 12;

/// `mach_msg_trailer_t`: the eight bytes the kernel appends to a received
/// message. `msgh_size` does not include it, but `rcv_size` has to - which is
/// the arithmetic a receiver gets wrong and then blames on the sender.
const TRAILER: usize = 8;
/// `MACH_MSG_TRAILER_FORMAT_0`.
const TRAILER_FORMAT_0: u32 = 0;

/// The envelope that crosses the socket, in front of the message's own bytes.
mod envelope {
    pub const MAGIC: u32 = 0x3372_4d43;
    pub const VERSION: u32 = 1;
    /// Fixed part: magic, version, bits, id, descriptor count, payload length,
    /// then the reply reference.
    pub const FIXED: usize = 24 + super::REFERENCE;
}

/// A port reference as it crosses: 16 bytes, never a name.
const REFERENCE: usize = 16;

/// What a [`Reference`] refers to.
mod refkind {
    pub const NONE: u8 = 0;
    /// A send capability: the receiver may send to `address`.
    pub const SEND: u8 = 1;
    /// A receive right, whose socket is the descriptor at `fd_index`.
    pub const RECEIVE: u8 = 2;
    /// A right whose port is gone.
    pub const DEAD: u8 = 3;
}

#[derive(Clone, Copy)]
struct Reference {
    kind: u8,
    /// The disposition the *receiver* sees, which is always a `MOVE_`: a right
    /// that has arrived has already been transferred.
    disposition: u8,
    fd_index: i16,
    address: u64,
}

impl Reference {
    const NONE: Reference = Reference {
        kind: refkind::NONE,
        disposition: 0,
        fd_index: -1,
        address: 0,
    };

    fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.kind);
        out.push(self.disposition);
        out.extend_from_slice(&self.fd_index.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&self.address.to_le_bytes());
    }

    fn decode(bytes: &[u8]) -> Option<Reference> {
        let bytes: &[u8; REFERENCE] = bytes.get(..REFERENCE)?.try_into().ok()?;
        Some(Reference {
            kind: bytes[0],
            disposition: bytes[1],
            fd_index: i16::from_le_bytes([bytes[2], bytes[3]]),
            address: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
        })
    }

    /// Put this reference into the receiving task's space and answer with the
    /// name it got.
    fn adopt(&self, message: &mut Datagram) -> Name {
        match self.kind {
            refkind::SEND => match self.disposition as u32 {
                MOVE_SEND_ONCE => space::insert_send_once(self.address),
                _ => space::insert_send(self.address),
            },
            refkind::RECEIVE => match message.take_fd(self.fd_index.max(0) as usize) {
                // SAFETY: a descriptor that arrived over `SCM_RIGHTS` for the
                // socket bound to `address`, now owned by this process.
                Some(fd) => space::insert_receive(unsafe { Receiver::adopt(fd, self.address) }),
                // The sender said a right was coming and none did. The name it
                // would have had is dead rather than null, so a caller that
                // sends to it is told the port is gone.
                None => MACH_PORT_DEAD,
            },
            refkind::DEAD => MACH_PORT_DEAD,
            _ => MACH_PORT_NULL,
        }
    }
}

/// A right taken out of this task's space for a send that has not happened yet.
///
/// Held rather than consumed immediately because a failed send has not used the
/// right: Mach leaves the caller holding exactly what it had, and so does this.
enum Pending {
    /// Nothing to undo: `MAKE_SEND` and `COPY_SEND` leave the original in place.
    Copied,
    /// A name to release once the message is away.
    Moved(Name),
    /// A receive right already out of the space, with the descriptor to send.
    Receive { fd: std::ffi::c_int, address: u64 },
}

impl Pending {
    /// The send failed: put back whatever was taken.
    fn restore(self) {
        match self {
            Pending::Copied | Pending::Moved(_) => {}
            Pending::Receive { fd, address } => {
                // SAFETY: the descriptor taken out of the space above, never
                // handed to anything else.
                space::insert_receive(unsafe { Receiver::adopt(fd, address) });
            }
        }
    }

    /// The send succeeded: consume what the disposition said it would.
    fn commit(self) {
        match self {
            Pending::Copied => {}
            Pending::Moved(name) => {
                super::space::mach_port_deallocate(space::TASK_SELF, name);
            }
            Pending::Receive { fd, .. } => {
                // The kernel holds the in-flight reference until the receiver
                // takes it, so this end is done with the descriptor.
                // SAFETY: this process's own copy, already sent.
                unsafe { libc::close(fd) };
            }
        }
    }
}

/// Turn `name`, held under `disposition`, into something that can cross.
fn export(name: Name, disposition: u32) -> Result<(Reference, Option<Pending>), i32> {
    if name == MACH_PORT_NULL {
        return Ok((Reference::NONE, None));
    }
    if name == MACH_PORT_DEAD {
        return Ok((
            Reference {
                kind: refkind::DEAD,
                ..Reference::NONE
            },
            None,
        ));
    }
    match disposition {
        MOVE_RECEIVE => {
            let port = space::move_receive(name)?;
            let address = port.address();
            let fd = port.into_fd();
            Ok((
                Reference {
                    kind: refkind::RECEIVE,
                    disposition: MOVE_RECEIVE as u8,
                    fd_index: -1,
                    address,
                },
                Some(Pending::Receive { fd, address }),
            ))
        }
        MAKE_SEND | COPY_SEND | MOVE_SEND => {
            let address = space::address_of(name).ok_or(MACH_SEND_INVALID_RIGHT)?;
            let held = match disposition {
                MOVE_SEND => Pending::Moved(name),
                _ => Pending::Copied,
            };
            Ok((
                Reference {
                    kind: refkind::SEND,
                    disposition: MOVE_SEND as u8,
                    fd_index: -1,
                    address,
                },
                Some(held),
            ))
        }
        MAKE_SEND_ONCE | MOVE_SEND_ONCE => {
            let address = space::address_of(name).ok_or(MACH_SEND_INVALID_RIGHT)?;
            let held = match disposition {
                MOVE_SEND_ONCE => Pending::Moved(name),
                _ => Pending::Copied,
            };
            Ok((
                Reference {
                    kind: refkind::SEND,
                    disposition: MOVE_SEND_ONCE as u8,
                    fd_index: -1,
                    address,
                },
                Some(held),
            ))
        }
        _ => Err(MACH_SEND_INVALID_TYPE),
    }
}

/// Read `len` bytes of the caller's message.
///
/// # Safety
///
/// `msg` addresses at least `len` readable bytes.
unsafe fn read_message(msg: *const u8, len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    // SAFETY: the caller's message, whose length it stated in `msgh_size`.
    unsafe { core::ptr::copy_nonoverlapping(msg, bytes.as_mut_ptr(), len) };
    bytes
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// # Safety
///
/// `msg` addresses a `mach_msg_header_t` and `send_size` readable bytes.
unsafe fn send(msg: *const u8, send_size: u32) -> i32 {
    let size = send_size as usize;
    if !(HEADER..=wire::MAX_MESSAGE).contains(&size) {
        return MACH_SEND_TOO_LARGE;
    }
    // SAFETY: the caller's message, of the size it stated.
    let bytes = unsafe { read_message(msg, size) };

    let bits = word(&bytes, 0);
    let remote_name = word(&bytes, 8);
    let local_name = word(&bytes, 12);
    let id = word(&bytes, 20) as i32;

    let destination = match space::destination(remote_name) {
        Ok((address, _)) => address,
        Err(code) => return code,
    };

    let mut pending = Vec::new();
    let failure = |pending: Vec<Pending>, code: i32| -> i32 {
        for held in pending {
            held.restore();
        }
        code
    };

    // The reply port. Its disposition is the *local* field of `msgh_bits`, which
    // is the one a MIG routine sets to `MAKE_SEND_ONCE` so the server can answer.
    let local_disposition = (bits >> BITS_LOCAL_SHIFT) & BITS_PORTS_MASK;
    let reply = match export(local_name, local_disposition) {
        Ok((reference, held)) => {
            pending.extend(held);
            reference
        }
        Err(code) => return failure(pending, code),
    };

    let complex = bits & BITS_COMPLEX != 0;
    let mut descriptors = Vec::new();
    let mut fds = Vec::new();
    let mut payload_at = HEADER;

    if complex {
        if size < HEADER + BODY {
            return failure(pending, MACH_SEND_INVALID_DATA);
        }
        let count = word(&bytes, HEADER) as usize;
        if count > MAX_RIGHTS {
            return failure(pending, MACH_SEND_TOO_LARGE);
        }
        payload_at = HEADER + BODY + count * DESCRIPTOR;
        if size < payload_at {
            return failure(pending, MACH_SEND_INVALID_DATA);
        }
        for i in 0..count {
            let at = HEADER + BODY + i * DESCRIPTOR;
            if bytes[at + DESCRIPTOR_TYPE_AT] != DESCRIPTOR_PORT {
                announce_out_of_line();
                return failure(pending, MACH_SEND_INVALID_TYPE);
            }
            let name = word(&bytes, at);
            let disposition = bytes[at + DESCRIPTOR_DISPOSITION_AT] as u32;
            match export(name, disposition) {
                Ok((mut reference, held)) => {
                    if reference.kind == refkind::RECEIVE {
                        reference.fd_index = fds.len() as i16;
                        if let Some(Pending::Receive { fd, .. }) = held.as_ref() {
                            fds.push(*fd);
                        }
                    }
                    pending.extend(held);
                    descriptors.push(reference);
                }
                Err(code) => return failure(pending, code),
            }
        }
    }

    let payload = &bytes[payload_at..];
    let mut wire_bytes =
        Vec::with_capacity(envelope::FIXED + descriptors.len() * REFERENCE + payload.len());
    wire_bytes.extend_from_slice(&envelope::MAGIC.to_le_bytes());
    wire_bytes.extend_from_slice(&envelope::VERSION.to_le_bytes());
    wire_bytes.extend_from_slice(&bits.to_le_bytes());
    wire_bytes.extend_from_slice(&id.to_le_bytes());
    wire_bytes.extend_from_slice(&(descriptors.len() as u32).to_le_bytes());
    wire_bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    reply.encode(&mut wire_bytes);
    for reference in &descriptors {
        reference.encode(&mut wire_bytes);
    }
    wire_bytes.extend_from_slice(payload);

    match wire::send(destination, &wire_bytes, &fds) {
        Ok(()) => {
            for held in pending {
                held.commit();
            }
            // A send-once right is spent by the message that used it, and the
            // destination's own right is released when it was moved.
            if let Ok((_, once)) = space::destination(remote_name)
                && once
            {
                super::space::mach_port_deallocate(space::TASK_SELF, remote_name);
            }
            MACH_MSG_SUCCESS
        }
        Err(error) => failure(
            pending,
            send_failure(error.raw_os_error().unwrap_or(libc::ECONNREFUSED)),
        ),
    }
}

fn announce_out_of_line() {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "rosetta3: mach_msg refused a message carrying out-of-line memory - a page mapped into \
             the receiver is not something one unrelated process can do to another here, and \
             copying the pointer across would hand the receiver an address that means something \
             else in its own space"
        );
    }
}

/// Take the next message for `rcv_name`, and the port it actually arrived on -
/// which is a member when the caller is receiving on a port set.
fn next_message(rcv_name: Name, timeout_ms: i32) -> Result<(Datagram, Name), i32> {
    if let Some(port) = space::receiver(rcv_name) {
        return match port.receive(timeout_ms) {
            Ok(Some(message)) => Ok((message, rcv_name)),
            Ok(None) => Err(MACH_RCV_TIMED_OUT),
            Err(_) => Err(MACH_RCV_INVALID_NAME),
        };
    }

    let Some(members) = space::members(rcv_name) else {
        return Err(MACH_RCV_INVALID_NAME);
    };
    let ports: Vec<_> = members
        .iter()
        .filter_map(|name| space::receiver(*name).map(|port| (*name, port)))
        .collect();
    if ports.is_empty() {
        return Err(MACH_RCV_INVALID_NAME);
    }
    // A message held back from a too-small buffer is not on the socket any more,
    // so the set has to be asked about it before anything is polled.
    for (name, port) in &ports {
        if let Some(held) = port.take_held() {
            return Ok((held, *name));
        }
    }
    let fds: Vec<_> = ports.iter().map(|(_, port)| port.fd()).collect();
    let Some(index) = wire::poll_any(&fds, timeout_ms) else {
        return Err(MACH_RCV_TIMED_OUT);
    };
    let (name, port) = &ports[index];
    match port.receive(0) {
        Ok(Some(message)) => Ok((message, *name)),
        // Another thread receiving on the same set took it first.
        Ok(None) => Err(MACH_RCV_TIMED_OUT),
        Err(_) => Err(MACH_RCV_INVALID_NAME),
    }
}

/// # Safety
///
/// `msg` addresses `rcv_size` writable bytes.
unsafe fn receive(
    msg: *mut u8,
    rcv_size: u32,
    rcv_name: Name,
    timeout_ms: i32,
    large: bool,
) -> i32 {
    let (mut message, arrived_on) = match next_message(rcv_name, timeout_ms) {
        Ok(taken) => taken,
        Err(code) => return code,
    };
    // The message is off the port's queue now, which is the only event this
    // transport can be certain of, and it is what `mach_port_peek`'s sequence
    // numbers are counted from.
    seqno::took(arrived_on);

    let bytes = &message.bytes;
    if bytes.len() < envelope::FIXED
        || word(bytes, 0) != envelope::MAGIC
        || word(bytes, 4) != envelope::VERSION
    {
        return MACH_RCV_BODY_ERROR;
    }
    let sender_bits = word(bytes, 8);
    let id = word(bytes, 12) as i32;
    let count = word(bytes, 16) as usize;
    let payload_len = word(bytes, 20) as usize;
    if count > MAX_RIGHTS || bytes.len() < envelope::FIXED + count * REFERENCE + payload_len {
        return MACH_RCV_BODY_ERROR;
    }

    let complex = count > 0 || sender_bits & BITS_COMPLEX != 0;
    let body = match complex {
        true => BODY + count * DESCRIPTOR,
        false => 0,
    };
    let size = HEADER + body + payload_len;
    if size + TRAILER > rcv_size as usize {
        // Without `MACH_RCV_LARGE` the message is destroyed, which is what
        // dropping `message` does - including any rights it carried. With it,
        // the caller means to come back with a bigger buffer, so the message is
        // kept for the next receive on the same port.
        if large {
            if rcv_size as usize >= HEADER {
                // SAFETY: at least a header's worth of the caller's buffer,
                // just checked.
                unsafe { write_header(msg, 0, size as u32, MACH_PORT_NULL, arrived_on, id) };
            }
            if let Some(port) = space::receiver(arrived_on) {
                port.hold(message);
            }
        }
        return MACH_RCV_TOO_LARGE;
    }

    // Every right in the message enters this task's space before any of it is
    // written out, so a failure part-way cannot leave the caller holding names
    // for rights it was never told about.
    let reply = Reference::decode(&bytes[24..]).unwrap_or(Reference::NONE);
    let reply_name = reply.adopt(&mut message);
    let adopted: Vec<(Name, u8)> = (0..count)
        .map(|i| {
            let at = envelope::FIXED + i * REFERENCE;
            let reference = Reference::decode(&message.bytes[at..]).unwrap_or(Reference::NONE);
            (reference.adopt(&mut message), reference.disposition)
        })
        .collect();

    let mut bits =
        (reply.disposition as u32 & BITS_PORTS_MASK) | (MOVE_RECEIVE << BITS_LOCAL_SHIFT);
    if complex {
        bits |= BITS_COMPLEX;
    }

    // SAFETY: `size + TRAILER` bytes of the caller's buffer, checked above.
    unsafe {
        write_header(msg, bits, size as u32, reply_name, arrived_on, id);
        if complex {
            write_word(msg, HEADER, count as u32);
            for (i, (name, disposition)) in adopted.iter().enumerate() {
                let at = HEADER + BODY + i * DESCRIPTOR;
                write_word(msg, at, *name);
                write_word(msg, at + 4, 0);
                msg.add(at + 8).write(0);
                msg.add(at + 9).write(0);
                msg.add(at + DESCRIPTOR_DISPOSITION_AT).write(*disposition);
                msg.add(at + DESCRIPTOR_TYPE_AT).write(DESCRIPTOR_PORT);
            }
        }
        let payload_at = envelope::FIXED + count * REFERENCE;
        core::ptr::copy_nonoverlapping(
            message.bytes.as_ptr().add(payload_at),
            msg.add(HEADER + body),
            payload_len,
        );
        // The trailer, which `msgh_size` does not count and `rcv_size` must.
        write_word(msg, size, TRAILER_FORMAT_0);
        write_word(msg, size + 4, TRAILER as u32);
    }
    MACH_MSG_SUCCESS
}

/// # Safety
///
/// `msg` addresses at least `at + 4` writable bytes.
unsafe fn write_word(msg: *mut u8, at: usize, value: u32) {
    // SAFETY: the caller's buffer, whose length was checked before the call.
    // Unaligned because a message is only guaranteed 4-byte aligned and the
    // trailer may land anywhere.
    unsafe { msg.add(at).cast::<u32>().write_unaligned(value) };
}

/// # Safety
///
/// `msg` addresses at least [`HEADER`] writable bytes.
unsafe fn write_header(msg: *mut u8, bits: u32, size: u32, remote: Name, local: Name, id: i32) {
    // SAFETY: the caller's buffer, at least a header long.
    unsafe {
        write_word(msg, 0, bits);
        write_word(msg, 4, size);
        write_word(msg, 8, remote);
        write_word(msg, 12, local);
        write_word(msg, 16, MACH_PORT_NULL);
        write_word(msg, 20, id as u32);
    }
}

/// `mach_msg_return_t mach_msg(mach_msg_header_t *msg, mach_msg_option_t option,
/// mach_msg_size_t send_size, mach_msg_size_t rcv_size, mach_port_name_t
/// rcv_name, mach_msg_timeout_t timeout, mach_port_name_t notify)`
///
/// A single call can do both halves, in that order, which is how every MIG
/// routine with a reply is implemented.
///
/// # Safety
///
/// `msg` addresses `send_size` readable bytes when `MACH_SEND_MSG` is set and
/// `rcv_size` writable ones when `MACH_RCV_MSG` is.
pub unsafe extern "C" fn mach_msg(
    msg: *mut c_void,
    option: i32,
    send_size: u32,
    rcv_size: u32,
    rcv_name: Name,
    timeout: u32,
    _notify: Name,
) -> i32 {
    if msg.is_null() {
        return MACH_SEND_INVALID_DATA;
    }
    let msg = msg.cast::<u8>();
    if option & SEND_MSG != 0 {
        // SAFETY: the caller's message, of the size it passed.
        let sent = unsafe { send(msg, send_size) };
        if sent != MACH_MSG_SUCCESS {
            return match option & SEND_TIMEOUT != 0 && sent == MACH_SEND_NO_BUFFER {
                true => MACH_SEND_TIMED_OUT,
                false => sent,
            };
        }
    }
    if option & RCV_MSG != 0 {
        let timeout_ms = match option & RCV_TIMEOUT != 0 {
            true => timeout.min(i32::MAX as u32) as i32,
            false => -1,
        };
        // SAFETY: the caller's buffer, of the size it passed.
        return unsafe { receive(msg, rcv_size, rcv_name, timeout_ms, option & RCV_LARGE != 0) };
    }
    MACH_MSG_SUCCESS
}

/// `mach_msg_return_t mach_msg_overwrite(...)` - `mach_msg` with a separate
/// buffer for the reply.
///
/// The overwrite buffer is honoured only when the caller supplies one; a null
/// `rcv_msg` means "reply into `msg`", which is plain [`mach_msg`].
///
/// # Safety
///
/// As [`mach_msg`], with `rcv_msg` addressing `rcv_limit` writable bytes when
/// it is not null.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn mach_msg_overwrite(
    msg: *mut c_void,
    option: i32,
    send_size: u32,
    rcv_size: u32,
    rcv_name: Name,
    timeout: u32,
    notify: Name,
    rcv_msg: *mut c_void,
    rcv_limit: u32,
) -> i32 {
    if rcv_msg.is_null() {
        // SAFETY: the caller's, as documented.
        return unsafe { mach_msg(msg, option, send_size, rcv_size, rcv_name, timeout, notify) };
    }
    if option & SEND_MSG != 0 {
        // SAFETY: the caller's message, of the size it passed.
        let sent = unsafe { send(msg.cast(), send_size) };
        if sent != MACH_MSG_SUCCESS {
            return sent;
        }
    }
    if option & RCV_MSG != 0 {
        let timeout_ms = match option & RCV_TIMEOUT != 0 {
            true => timeout.min(i32::MAX as u32) as i32,
            false => -1,
        };
        // SAFETY: the caller's reply buffer, of the size it passed.
        return unsafe {
            receive(
                rcv_msg.cast(),
                rcv_limit.min(rcv_size),
                rcv_name,
                timeout_ms,
                option & RCV_LARGE != 0,
            )
        };
    }
    MACH_MSG_SUCCESS
}

/// `void mach_msg_destroy(mach_msg_header_t *)`
///
/// Releases every right in a message the caller has decided not to act on. The
/// rights are already in this task's space by the time a receiver sees them, so
/// skipping this leaks a name per message - which is what turns a server loop
/// that discards unknown messages into one that runs out of names.
///
/// # Safety
///
/// `msg` addresses a received `mach_msg_header_t` and the body it describes.
pub unsafe extern "C" fn mach_msg_destroy(msg: *mut c_void) {
    if msg.is_null() {
        return;
    }
    let msg = msg.cast::<u8>();
    // SAFETY: a received message, whose header states its own size.
    let (bits, size) = unsafe {
        (
            msg.cast::<u32>().read_unaligned(),
            msg.add(4).cast::<u32>().read_unaligned() as usize,
        )
    };
    // SAFETY: as above, and the size is the message's own.
    let bytes = unsafe { read_message(msg, size.clamp(HEADER, wire::MAX_MESSAGE)) };

    let remote = word(&bytes, 8);
    if remote != MACH_PORT_NULL && remote != MACH_PORT_DEAD {
        super::space::mach_port_deallocate(space::TASK_SELF, remote);
    }
    if bits & BITS_COMPLEX == 0 || bytes.len() < HEADER + BODY {
        return;
    }
    let count = word(&bytes, HEADER) as usize;
    for i in 0..count {
        let at = HEADER + BODY + i * DESCRIPTOR;
        if at + DESCRIPTOR > bytes.len() || bytes[at + DESCRIPTOR_TYPE_AT] != DESCRIPTOR_PORT {
            return;
        }
        let name = word(&bytes, at);
        if name == MACH_PORT_NULL || name == MACH_PORT_DEAD {
            continue;
        }
        match bytes[at + DESCRIPTOR_DISPOSITION_AT] as u32 {
            // A receive right that arrived is destroyed, not deallocated: it is
            // the port itself and nobody else in this task holds it.
            MOVE_RECEIVE => {
                space::destroy(name);
            }
            _ => {
                super::space::mach_port_deallocate(space::TASK_SELF, name);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `mach_port_peek`: the head of a receive right's queue, read without taking it.
//
// ## The return value is the interface, and it has two answers
//
// `KERN_SUCCESS` when a message is waiting and `KERN_FAILURE` when none is - not
// `MACH_RCV_TIMED_OUT`, and not a success reporting a queue of length zero.
// `ipc_mqueue_peek_locked` answers `1` for a message at the requested sequence
// number and `0` for nothing there, and `osfmk/ipc/mach_port.c` turns exactly
// those into `KERN_SUCCESS` and `KERN_FAILURE`.
//
// Chromium's `base::WaitableEvent::PeekPort`
// (`base/synchronization/waitable_event_apple.cc:244`, read at
// `refs/branch-heads/6723`) is
//
// ```c
//   kern_return_t kr = mach_port_peek(mach_task_self(), port,
//       MACH_RCV_TRAILER_TYPE(MACH_RCV_TRAILER_NULL), &seqno, &size, &id,
//       reinterpret_cast<mach_msg_trailer_info_t>(&trailer), &trailer_size);
//   if (kr == KERN_SUCCESS) return true;
//   else { MACH_CHECK(kr == KERN_FAILURE, kr) << "mach_port_peek"; return false; }
// ```
//
// and the machine code the shipped framework actually runs, at
// `Chromium Framework+0x5d9a3a4` with the call at `+0x5d9a3ec`, branches on
// nothing else: `test eax,eax` for the success, then `cmp eax,0x5` for
// `KERN_FAILURE`, then a `CHECK` naming `waitable_event_apple.cc` line 251. So a
// port with an empty queue must answer `KERN_FAILURE`: a success, a timeout, or
// an invalid-name code all abort the browser instead of letting it poll. This
// is the first thing a MANUAL-reset `WaitableEvent` meets, because
// `IsSignaled()` is `PeekPort(name, policy_ == ResetPolicy::AUTOMATIC)` and
// MANUAL is the default.
//
// ## Eight arguments, and the trailer is not what the kernel calls it
//
// The header declares nine parameter *types* for the kernel routine and the
// user call takes eight, because `mach_msg_trailer_info_t` is `char *` and
// carries the buffer rather than a pointer to a buffer: the argument is the
// caller's trailer storage itself. The eight, in register order, and where the
// guest puts each one at `+0x5d9a3c0`:
//
// | | argument | guest |
// |---|---|---|
// | `rdi` | `ipc_space_t space` | `*mach_task_self_` read through the GOT |
// | `rsi` | `mach_port_name_t name` | `mov esi,eax` - the caller's own argument |
// | `rdx` | `mach_msg_trailer_type_t trailer_type` | `xor edx,edx` |
// | `rcx` | `mach_port_seqno_t *seqno` | `lea rcx,[rbp-0x18]` |
// | `r8` | `mach_msg_size_t *size` | `lea r8,[rbp-0x14]` |
// | `r9` | `mach_msg_id_t *id` | `lea r9,[rbp-0x10]` |
// | `rsp` | `mach_msg_trailer_info_t trailer` | `lea r11,[rbp-0x40]`, pushed |
// | `rsp+8` | `mach_msg_type_number_t *trailer_size` | `lea r10,[rbp-0xc]`, pushed |
//
// The last two are an in/out pair, and the `mov DWORD PTR [rbp-0xc],0x8` just
// above the call is the guest storing `sizeof(trailer)` into `*trailer_size` -
// the buffer is eight bytes, and it is the kernel that fills it in.

/// `MACH_RCV_TRAILER_NULL`: the element type that asks for nothing at all.
///
/// `MACH_RCV_TRAILER_TYPE(x)` is `((x & 0xf) << 28)`, so the whole word is zero
/// for the null element type, and `GET_RCV_ELEMENTS(y)` - the field the kernel
/// reads the element type out of - is `(y >> 24) & 0xf`.
const TRAILER_NULL: u32 = 0;
const TRAILER_SEQNO: u32 = 1;
const TRAILER_SENDER: u32 = 2;
/// `MACH_RCV_TRAILER_AUDIT`, the largest element type `mach_port_peek` accepts;
/// anything above it is `KERN_INVALID_VALUE` before the name is even looked up.
/// The two above it are only here so that a refusal can name what it refused.
const TRAILER_AUDIT: u32 = 3;
const TRAILER_CTX: u32 = 4;
const TRAILER_AV: u32 = 7;
const TRAILER_LABELS: u32 = 8;

fn trailer_elements(trailer_type: u32) -> u32 {
    (trailer_type >> 24) & 0xf
}

/// The trailers `<mach/message.h>` publishes, in the order
/// `REQUESTED_TRAILER_SIZE_NATIVE` names them.
///
/// **The public `mach_msg_trailer_t` is `{msgh_trailer_type, msgh_trailer_size}`
/// and not `{msgh_seqno, msgh_trailer_type}`**, which is the shape a reader
/// remembers from the kernel's own `osfmk/mach/message.h` and which the
/// `_KERNEL` copy of that header does use. The two agree on their first two
/// words, so the bytes [`receive`] appends to a message - `MACH_MSG_TRAILER_FORMAT_0`
/// and `sizeof(mach_msg_trailer_t)` - are right either way, and this file's
/// existing receive path was already writing the SDK's pair. What the difference
/// moves is `msgh_seqno`, from offset 0 to offset 8, and with it the *sizes*: a
/// size read off the wrong structure is `KERN_INVALID_VALUE` for a caller whose
/// buffer is comfortably big enough.
#[repr(C)]
struct TrailerNull {
    msgh_trailer_type: u32,
    msgh_trailer_size: u32,
}
#[repr(C)]
struct TrailerSeqno {
    msgh_trailer_type: u32,
    msgh_trailer_size: u32,
    msgh_seqno: u32,
}
#[repr(C)]
struct TrailerSender {
    msgh_trailer_type: u32,
    msgh_trailer_size: u32,
    msgh_seqno: u32,
    /// `security_token_t` is `{unsigned int val[2]}`.
    msgh_sender: [u32; 2],
}
#[repr(C)]
struct TrailerAudit {
    msgh_trailer_type: u32,
    msgh_trailer_size: u32,
    msgh_seqno: u32,
    msgh_sender: [u32; 2],
    /// `audit_token_t` is `{unsigned int val[8]}`.
    msgh_audit: [u32; 8],
}

const _: () = {
    assert!(size_of::<TrailerNull>() == 8);
    assert!(size_of::<TrailerSeqno>() == 12);
    assert!(size_of::<TrailerSender>() == 20);
    assert!(size_of::<TrailerAudit>() == 52);
    assert!(std::mem::offset_of!(TrailerSeqno, msgh_seqno) == 8);
};

/// `sizeof` of the trailer `REQUESTED_TRAILER_SIZE(TRUE, trailer_type)` asks
/// for, or `None` for an element type the kernel rejects outright.
fn requested_trailer_size(elements: u32) -> Option<u32> {
    Some(match elements {
        TRAILER_NULL => size_of::<TrailerNull>() as u32,
        TRAILER_SEQNO => size_of::<TrailerSeqno>() as u32,
        TRAILER_SENDER => size_of::<TrailerSender>() as u32,
        TRAILER_AUDIT => size_of::<TrailerAudit>() as u32,
        _ => return None,
    })
}

/// The trailer a message of this transport carries, written where a caller
/// asked for one.
///
/// The same eight bytes [`receive`] appends to every message it hands back, and
/// the same eight bytes a caller of `mach_port_peek` gets back: the trailer is a
/// property of the message, not of the call that looked at it.
///
/// # Safety
///
/// `trailer` addresses at least [`TRAILER`] writable bytes.
unsafe fn write_trailer(trailer: *mut u8) {
    // SAFETY: the caller's trailer buffer, sized by its `trailer_size` out-value
    // and no smaller than one `mach_msg_trailer_t`.
    unsafe {
        write_word(trailer, 0, TRAILER_FORMAT_0);
        write_word(trailer, 4, TRAILER as u32);
    }
}

/// How many messages this namespace has taken off each port.
///
/// Mach's `mach_port_seqno_t` is assigned by the kernel as a message is
/// enqueued, and it is the one part of a port's state this transport cannot ask
/// the host for: a unix datagram socket has no sequence numbers, and
/// `FIONREAD` counts bytes rather than datagrams. What it *can* count is how
/// many messages this namespace has received, and that is the same number for
/// as long as a message is waiting - the transport is strictly first in first
/// out and nothing is ever put back ahead of a message already in the socket -
/// so the head of a non-empty queue is numbered `taken + 1` exactly as long as
/// Mach would number it `taken + 1`. A caller that orders messages by it gets
/// the right answer; a caller that uses it to name a message for a later peek
/// gets a number that counts this namespace's history of the port rather than
/// the host's.
///
/// Keyed by name, so a right that is moved to another name, or into another
/// process, starts again at one. That is a divergence from Mach, and the
/// alternative is a table that outlives the ports in it.
mod seqno {
    use super::*;

    /// Above this many entries the table is swept against the space.
    ///
    /// A destroyed port leaves its counter behind and nothing announces it:
    /// `mach_port_destroy` is not this module's to hook. Sweeping is O(entries)
    /// and only happens once the table has more names than any program has live
    /// ports, so a program that creates and destroys ports normally never pays
    /// for it and a program that does pays for it rarely.
    const BOUND: usize = 4096;

    fn taken() -> &'static Mutex<std::collections::HashMap<Name, u32>> {
        static TAKEN: OnceLock<Mutex<HashMap<Name, u32>>> = OnceLock::new();
        TAKEN.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// The number Mach would give the message at the head of `name`'s queue.
    pub fn head(name: Name) -> u32 {
        let guard = taken().lock().unwrap_or_else(|e| e.into_inner());
        guard.get(&name).copied().unwrap_or(0).wrapping_add(1)
    }

    /// `name` just gave a message to a receiver.
    pub fn took(name: Name) {
        let mut guard = taken().lock().unwrap_or_else(|e| e.into_inner());
        let count = guard.entry(name).or_default();
        *count = count.wrapping_add(1);
        if guard.len() <= BOUND {
            return;
        }
        guard.retain(|held, _| space::with_right(*held, |_| ()).is_some());
    }

    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
}

/// The `msgh_size` and `msgh_id` of the message at the head of `port`'s queue,
/// read without taking it.
///
/// `MSG_PEEK` rather than a receive, because that message is not this call's to
/// take - the caller's own next `mach_msg` has to get it, and a peek that
/// consumed it would turn a poll into a dequeue.
///
/// `None` when the head is not sitting on the socket. That happens in two cases
/// worth telling apart, because Chromium produces both:
///
/// * a message held back from a receive whose buffer was too small. Chromium's
///   `Wait()` on a MANUAL-reset event is exactly that - `MACH_RCV_LARGE` with
///   `rcv_size = 0` - and the message then waits in this namespace's own
///   one-message holding place rather than on the socket. `Receiver::ready()`
///   sees it, which is why the queue is reported as non-empty, and there is no
///   way to read its words without taking it out of there.
/// * a datagram from a process that is not this runtime, or one another thread
///   took between the readiness test and this read.
///
/// The caller learns the head's *existence* and nothing else in either case,
/// which is the difference between an answer and a number invented here.
fn head_message(port: &Receiver) -> Option<(u32, u32)> {
    let mut bytes = vec![0u8; envelope::FIXED];
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    // SAFETY: a plain C structure, zeroed as its header's own examples do.
    let mut message: libc::msghdr = unsafe { core::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    // SAFETY: `message` describes `bytes` and the descriptor `port` holds, and
    // neither is touched by anything else. A short read is a truncated peek,
    // which is why only the fixed part of the envelope is asked for.
    let got =
        unsafe { libc::recvmsg(port.fd(), &mut message, libc::MSG_PEEK | libc::MSG_DONTWAIT) };
    if got < envelope::FIXED as isize
        || word(&bytes, 0) != envelope::MAGIC
        || word(&bytes, 4) != envelope::VERSION
    {
        return None;
    }
    let count = word(&bytes, 16) as usize;
    if count > MAX_RIGHTS {
        return None;
    }
    // The `msgh_size` [`receive`] would have written, from the same three words.
    let body = match count > 0 || word(&bytes, 8) & BITS_COMPLEX != 0 {
        true => BODY + count * DESCRIPTOR,
        false => 0,
    };
    let size = HEADER
        .checked_add(body)?
        .checked_add(word(&bytes, 20) as usize)?;
    Some((size as u32, word(&bytes, 12)))
}

/// `kern_return_t mach_port_peek(ipc_space_t space, mach_port_name_t name,
/// mach_msg_trailer_type_t trailer_type, mach_port_seqno_t *seqno,
/// mach_msg_size_t *size, mach_msg_id_t *id, mach_msg_trailer_info_t trailer,
/// mach_msg_type_number_t *trailer_size)`
///
/// The order of the work is the kernel's, because two of its steps are visible
/// to a caller: `*trailer_size` is written **before** the name is looked up, so
/// a caller gets the requested size back even when the answer is
/// `KERN_INVALID_NAME`, and nothing at all is written through `seqno`, `size` or
/// `id` when the answer is `KERN_FAILURE`.
///
/// **What is served, and what is not.** `MACH_RCV_TRAILER_NULL` and
/// `MACH_RCV_TRAILER_SEQNO` are answered in full. `MACH_RCV_TRAILER_SENDER` and
/// `MACH_RCV_TRAILER_AUDIT` are refused, and the reason is not convenience: a
/// security token and an audit token identify a sender by Mach's own
/// accounting of tasks and threads, and this namespace has none of it. Filling
/// those eight and thirty-two bytes with zeros would hand a caller a
/// *confident* identity for "the sender is the null audit token", which is a
/// claim a sandbox decides on.
///
/// # Safety
///
/// `seqno`, `size`, `id` and `trailer_size` are writable `natural_t`s, and
/// `trailer` is at least `*trailer_size` writable bytes.
pub unsafe extern "C" fn mach_port_peek(
    task: Name,
    name: Name,
    trailer_type: u32,
    seqno: *mut u32,
    size: *mut u32,
    id: *mut u32,
    trailer: *mut u8,
    trailer_size: *mut u32,
) -> i32 {
    // MIG's generated server checks its out-parameters before it reaches the
    // kernel, and answers `KERN_INVALID_ARGUMENT` for a null one; the kernel
    // itself would fault. A null here is a caller bug either way, and faulting
    // is not an answer this runtime gives.
    if seqno.is_null()
        || size.is_null()
        || id.is_null()
        || trailer.is_null()
        || trailer_size.is_null()
    {
        return KERN_INVALID_ARGUMENT;
    }
    if task != space::TASK_SELF {
        return KERN_INVALID_TASK;
    }
    // `MACH_PORT_VALID(name)`: neither null nor dead. Checked before the trailer,
    // as the kernel does.
    if name == MACH_PORT_NULL || name == MACH_PORT_DEAD {
        return KERN_INVALID_RIGHT;
    }
    let elements = trailer_elements(trailer_type);
    // Two trailer tests before the name is looked up, both of them the kernel's
    // and in the kernel's order. The first is about what the *caller* asked for
    // and has to fit: an element type above `MACH_RCV_TRAILER_AUDIT` is not
    // defined at all, and a buffer smaller than the trailer requested is
    // `KERN_INVALID_VALUE` however willing the port is.
    // SAFETY: the caller's writable `mach_msg_type_number_t`, checked above.
    let offered = unsafe { trailer_size.read() };
    let Some(requested) = requested_trailer_size(elements) else {
        return refused_peek_trailer(trailer_type, name, offered, TRAILER_REFUSED_UNDEFINED);
    };
    if requested > offered {
        return refused_peek_trailer(trailer_type, name, offered, TRAILER_REFUSED_TOO_SMALL);
    }
    // The second is about what *this* namespace can report rather than about the
    // request, and it is the reason the two element types above `SEQNO` are
    // refused instead of filled with zeros.
    if elements > TRAILER_SEQNO {
        return refused_peek_trailer(trailer_type, name, offered, TRAILER_REFUSED_IDENTITY);
    }
    // SAFETY: the caller's writable `mach_msg_type_number_t`, checked above, and
    // this happens before the name is looked up, as it does in the kernel.
    unsafe { trailer_size.write(requested) };

    let Some(port) = space::receiver(name) else {
        return match space::with_right(name, |_| ()) {
            // The name is one of ours and is not a receive right - a port set, a
            // send right, one of the special names. Mach says
            // "Name doesn't denote receive rights".
            Some(()) => KERN_INVALID_RIGHT,
            None => KERN_INVALID_NAME,
        };
    };

    if !port.ready() {
        // Nothing at the head. Mach leaves `seqno`, `size` and `id` alone here
        // and answers `KERN_FAILURE`, which is the code Chromium's `MACH_CHECK`
        // is waiting for.
        return KERN_FAILURE;
    }

    // SAFETY: the caller's writable `mach_port_seqno_t`.
    unsafe { seqno.write(seqno::head(name)) };
    match head_message(&port) {
        Some((size_of_message, id_of_message)) => {
            // SAFETY: the caller's writable `mach_msg_size_t` and
            // `mach_msg_id_t`.
            unsafe {
                size.write(size_of_message);
                id.write(id_of_message);
            }
        }
        None => announce_unreadable_head(),
    }
    // SAFETY: the caller's trailer buffer, at least `*trailer_size` bytes as
    // just written, and the only element types reachable here are the two whose
    // trailer is `mach_msg_trailer_t` plus, for `SEQNO`, one more word.
    unsafe {
        write_trailer(trailer);
        if elements == TRAILER_SEQNO {
            write_word(trailer, 8, seqno::head(name));
        }
    }
    KERN_SUCCESS
}

/// The element-type name a refusal speaks, in `<mach/message.h>`'s order.
fn elements_name(trailer_type: u32) -> &'static str {
    match trailer_elements(trailer_type) {
        TRAILER_NULL => "MACH_RCV_TRAILER_NULL",
        TRAILER_SEQNO => "MACH_RCV_TRAILER_SEQNO",
        TRAILER_SENDER => "MACH_RCV_TRAILER_SENDER",
        TRAILER_AUDIT => "MACH_RCV_TRAILER_AUDIT",
        TRAILER_CTX => "MACH_RCV_TRAILER_CTX",
        TRAILER_AV => "MACH_RCV_TRAILER_AV",
        TRAILER_LABELS => "MACH_RCV_TRAILER_LABELS",
        _ => "an element type mach_port_peek does not define",
    }
}

/// Why a trailer was refused, when the answer is not the size of anything.
///
/// Named as constants rather than inlined so that the one message the refusal
/// prints has one wording per cause, and so that a test can assert which of the
/// two refusals a given word gets.
const TRAILER_REFUSED_UNDEFINED: &str =
    "that element type is above MACH_RCV_TRAILER_AUDIT, which is as far as mach_port_peek goes";
const TRAILER_REFUSED_TOO_SMALL: &str =
    "the caller\'s buffer is smaller than the trailer it asked for";
const TRAILER_REFUSED_IDENTITY: &str = "a security token and an audit token name a sender by Mach\'s own accounting of tasks and \
     threads, and a datagram socket has none of it - the words would be zeros, which is a \
     confident answer to \"who sent this\" that is not true";

/// Say once that a peek asked for a trailer this does not report, and say which.
fn refused_peek_trailer(trailer_type: u32, name: Name, offered: u32, because: &str) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: mach_port_peek: port {name:#x} asked for trailer type {trailer_type:#x} \
             ({}), and the caller offered {offered} bytes, but {because}",
            elements_name(trailer_type)
        );
    });
    KERN_INVALID_VALUE
}

/// Say once that a port reported a head whose words could not be read.
fn announce_unreadable_head() {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "rosetta3: mach_port_peek found a message waiting on a port and did not report its \
             msgh_size or msgh_id: the head of the queue is not on the socket, so there is nothing \
             to read them out of - it is most often a message held back from a receive whose \
             buffer was too small, which is what Chromium's Wait() on a manual-reset event leaves \
             behind. The queue is reported as non-empty, which is true, and those two words are \
             left as the caller wrote them"
        );
    }
}

/// The reply port MIG keeps per thread.
///
/// A MIG routine sends its request with `msgh_local_port` set to this and then
/// receives the reply on it, so it has to be a real receive right and it has to
/// be the *same* one across the two halves of one call.
mod reply {
    use super::*;

    thread_local! {
        static PORT: std::cell::Cell<Name> = const { std::cell::Cell::new(MACH_PORT_NULL) };
    }

    pub extern "C" fn mig_get_reply_port() -> Name {
        PORT.with(|port| {
            if port.get() == MACH_PORT_NULL {
                let mut fresh = MACH_PORT_NULL;
                // SAFETY: a live local for the out-parameter.
                unsafe {
                    super::super::space::mach_port_allocate(
                        space::TASK_SELF,
                        space::RIGHT_RECEIVE,
                        &mut fresh,
                    )
                };
                port.set(fresh);
            }
            port.get()
        })
    }

    /// Hand a reply port back for reuse. A no-op: it was never taken away.
    pub extern "C" fn mig_put_reply_port(_name: Name) {}

    /// Throw the thread's reply port away, which a MIG routine does after a
    /// failed call so the next one is not answered by a stale reply.
    pub extern "C" fn mig_dealloc_reply_port(_name: Name) {
        PORT.with(|port| {
            let held = port.replace(MACH_PORT_NULL);
            if held != MACH_PORT_NULL {
                space::destroy(held);
            }
        });
    }
}

pub use reply::{mig_dealloc_reply_port, mig_get_reply_port, mig_put_reply_port};

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `mach_msg_header_t` plus `extra` bytes of payload.
    fn message(bits: u32, remote: Name, local: Name, id: i32, extra: &[u8]) -> Vec<u8> {
        let size = HEADER + extra.len();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&bits.to_le_bytes());
        bytes.extend_from_slice(&(size as u32).to_le_bytes());
        bytes.extend_from_slice(&remote.to_le_bytes());
        bytes.extend_from_slice(&local.to_le_bytes());
        bytes.extend_from_slice(&MACH_PORT_NULL.to_le_bytes());
        bytes.extend_from_slice(&id.to_le_bytes());
        bytes.extend_from_slice(extra);
        bytes
    }

    fn allocate_receive() -> Name {
        let mut name = MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe {
            super::super::space::mach_port_allocate(
                space::TASK_SELF,
                space::RIGHT_RECEIVE,
                &mut name,
            )
        };
        name
    }

    #[test]
    fn a_message_sent_to_a_port_comes_back_with_its_id_and_payload() {
        let port = allocate_receive();
        let mut sent = message(0, port, MACH_PORT_NULL, 0x1234, b"eight!!\0");
        // SAFETY: `sent` is a whole message of the size in its header.
        let rc = unsafe {
            mach_msg(
                sent.as_mut_ptr().cast(),
                SEND_MSG,
                sent.len() as u32,
                0,
                MACH_PORT_NULL,
                0,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_MSG_SUCCESS);

        let mut buf = vec![0u8; 256];
        // SAFETY: 256 writable bytes for the reply.
        let rc = unsafe {
            mach_msg(
                buf.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                buf.len() as u32,
                port,
                1000,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_MSG_SUCCESS);
        assert_eq!(word(&buf, 20) as i32, 0x1234, "msgh_id");
        assert_eq!(
            word(&buf, 4) as usize,
            HEADER + 8,
            "msgh_size excludes the trailer"
        );
        assert_eq!(
            word(&buf, 12),
            port,
            "msgh_local_port is the port it arrived on"
        );
        assert_eq!(&buf[HEADER..HEADER + 8], b"eight!!\0");
        // The trailer the kernel appends, which `msgh_size` does not count.
        assert_eq!(word(&buf, HEADER + 8 + 4) as usize, TRAILER);

        space::destroy(port);
    }

    #[test]
    fn a_receive_with_no_message_times_out_rather_than_waiting() {
        let port = allocate_receive();
        let mut buf = vec![0u8; 64];
        // SAFETY: 64 writable bytes.
        let rc = unsafe {
            mach_msg(
                buf.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                buf.len() as u32,
                port,
                20,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_RCV_TIMED_OUT);
        space::destroy(port);
    }

    /// The reply port is how a MIG routine gets an answer, so the send right
    /// the sender put in `msgh_local_port` has to arrive as a usable name.
    #[test]
    fn a_reply_port_arrives_as_a_send_right_that_can_be_answered() {
        let service = allocate_receive();
        let reply = allocate_receive();
        let bits = MAKE_SEND_ONCE << BITS_LOCAL_SHIFT;
        let mut sent = message(bits, service, reply, 99, &[]);
        // SAFETY: a whole message of the size in its header.
        let rc = unsafe {
            mach_msg(
                sent.as_mut_ptr().cast(),
                SEND_MSG,
                sent.len() as u32,
                0,
                MACH_PORT_NULL,
                0,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_MSG_SUCCESS);

        let mut buf = vec![0u8; 256];
        // SAFETY: 256 writable bytes.
        unsafe {
            mach_msg(
                buf.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                buf.len() as u32,
                service,
                1000,
                MACH_PORT_NULL,
            )
        };
        let answer_to = word(&buf, 8);
        assert_ne!(answer_to, MACH_PORT_NULL);
        assert_eq!(word(&buf, 0) & BITS_PORTS_MASK, MOVE_SEND_ONCE);
        assert_eq!(space::address_of(answer_to), space::address_of(reply));

        // And the answer reaches the port that asked for it.
        let mut back = message(0, answer_to, MACH_PORT_NULL, 100, &[]);
        // SAFETY: a whole message of the size in its header.
        let rc = unsafe {
            mach_msg(
                back.as_mut_ptr().cast(),
                SEND_MSG,
                back.len() as u32,
                0,
                MACH_PORT_NULL,
                0,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_MSG_SUCCESS);
        // SAFETY: 256 writable bytes.
        let rc = unsafe {
            mach_msg(
                buf.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                buf.len() as u32,
                reply,
                1000,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_MSG_SUCCESS);
        assert_eq!(word(&buf, 20) as i32, 100);

        space::destroy(service);
        space::destroy(reply);
    }

    /// A right inside a message: the shape of Crashpad's handshake, minus the
    /// process boundary.
    #[test]
    fn a_port_descriptor_carries_a_right_that_the_receiver_can_use() {
        let service = allocate_receive();
        let carried = allocate_receive();
        let address = space::address_of(carried).unwrap();

        let mut descriptor = Vec::new();
        descriptor.extend_from_slice(&1u32.to_le_bytes()); // one descriptor
        descriptor.extend_from_slice(&carried.to_le_bytes());
        descriptor.extend_from_slice(&0u32.to_le_bytes());
        descriptor.extend_from_slice(&[0, 0, MOVE_RECEIVE as u8, DESCRIPTOR_PORT]);
        descriptor.extend_from_slice(&0xdead_beefu32.to_le_bytes()); // the payload

        let mut sent = message(BITS_COMPLEX, service, MACH_PORT_NULL, 7, &descriptor);
        // SAFETY: a whole message of the size in its header.
        let rc = unsafe {
            mach_msg(
                sent.as_mut_ptr().cast(),
                SEND_MSG,
                sent.len() as u32,
                0,
                MACH_PORT_NULL,
                0,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_MSG_SUCCESS);
        assert!(
            space::receiver(carried).is_none(),
            "a moved receive right leaves the sender's space"
        );

        let mut buf = vec![0u8; 256];
        // SAFETY: 256 writable bytes.
        let rc = unsafe {
            mach_msg(
                buf.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                buf.len() as u32,
                service,
                1000,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_MSG_SUCCESS);
        assert_ne!(word(&buf, 0) & BITS_COMPLEX, 0, "still complex");
        assert_eq!(word(&buf, HEADER), 1, "one descriptor");
        let arrived = word(&buf, HEADER + BODY);
        assert_eq!(
            buf[HEADER + BODY + DESCRIPTOR_DISPOSITION_AT] as u32,
            MOVE_RECEIVE
        );
        assert_eq!(space::address_of(arrived), Some(address));
        assert!(
            space::receiver(arrived).is_some(),
            "and it is a receive right"
        );
        assert_eq!(
            word(&buf, HEADER + BODY + DESCRIPTOR),
            0xdead_beef,
            "the payload follows the descriptors"
        );

        space::destroy(service);
        space::destroy(arrived);
    }

    /// Out-of-line memory cannot cross, and is refused by name rather than sent
    /// with the sender's pointer in it.
    #[test]
    fn a_message_carrying_out_of_line_memory_is_refused() {
        let port = allocate_receive();
        let mut descriptor = Vec::new();
        descriptor.extend_from_slice(&1u32.to_le_bytes());
        descriptor.extend_from_slice(&[0u8; 8]); // address
        // `deallocate`, `copy`, `pad1`, `type` - the type byte at offset 11, as
        // in a port descriptor.
        descriptor.extend_from_slice(&[0, 0, 0, 1]); // MACH_MSG_OOL_DESCRIPTOR
        descriptor.extend_from_slice(&8u32.to_le_bytes()); // size
        let mut sent = message(BITS_COMPLEX, port, MACH_PORT_NULL, 1, &descriptor);
        // SAFETY: a whole message of the size in its header.
        let rc = unsafe {
            mach_msg(
                sent.as_mut_ptr().cast(),
                SEND_MSG,
                sent.len() as u32,
                0,
                MACH_PORT_NULL,
                0,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_SEND_INVALID_TYPE);
        space::destroy(port);
    }

    #[test]
    fn a_send_to_a_name_that_is_not_a_right_fails_rather_than_going_nowhere() {
        let mut sent = message(0, 0x7fff_0000, MACH_PORT_NULL, 1, &[]);
        // SAFETY: a whole message of the size in its header.
        let rc = unsafe {
            mach_msg(
                sent.as_mut_ptr().cast(),
                SEND_MSG,
                sent.len() as u32,
                0,
                MACH_PORT_NULL,
                0,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_SEND_INVALID_DEST);
    }

    /// Too small a buffer destroys the message unless the caller said it would
    /// come back, in which case the message waits for the bigger buffer.
    #[test]
    fn a_message_too_large_for_the_buffer_is_kept_only_when_the_caller_asked() {
        let port = allocate_receive();
        let payload = [0u8; 64];
        let mut sent = message(0, port, MACH_PORT_NULL, 5, &payload);
        let send_once = |bytes: &mut Vec<u8>| {
            // SAFETY: a whole message of the size in its header.
            unsafe {
                mach_msg(
                    bytes.as_mut_ptr().cast(),
                    SEND_MSG,
                    bytes.len() as u32,
                    0,
                    MACH_PORT_NULL,
                    0,
                    MACH_PORT_NULL,
                )
            }
        };
        assert_eq!(send_once(&mut sent), MACH_MSG_SUCCESS);

        let mut small = vec![0u8; HEADER + 8];
        // SAFETY: `small.len()` writable bytes.
        let rc = unsafe {
            mach_msg(
                small.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT | RCV_LARGE,
                0,
                small.len() as u32,
                port,
                100,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_RCV_TOO_LARGE);
        assert_eq!(
            word(&small, 4) as usize,
            HEADER + payload.len(),
            "the size the caller needs"
        );

        let mut big = vec![0u8; 256];
        // SAFETY: 256 writable bytes.
        let rc = unsafe {
            mach_msg(
                big.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                big.len() as u32,
                port,
                100,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_MSG_SUCCESS, "the held message was still there");
        assert_eq!(word(&big, 20) as i32, 5);

        // Without `MACH_RCV_LARGE` the message goes away instead.
        assert_eq!(send_once(&mut sent), MACH_MSG_SUCCESS);
        // SAFETY: `small.len()` writable bytes.
        let rc = unsafe {
            mach_msg(
                small.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                small.len() as u32,
                port,
                100,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_RCV_TOO_LARGE);
        // SAFETY: 256 writable bytes.
        let rc = unsafe {
            mach_msg(
                big.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                big.len() as u32,
                port,
                50,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_RCV_TIMED_OUT, "it was destroyed, not queued");

        space::destroy(port);
    }

    /// A port set has no queue of its own: a receive on one answers with a
    /// message from a member, and says which member it came from.
    #[test]
    fn a_receive_on_a_port_set_names_the_member_it_arrived_on() {
        let mut set = MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe {
            super::super::space::mach_port_allocate(
                space::TASK_SELF,
                space::RIGHT_PORT_SET,
                &mut set,
            )
        };
        let first = allocate_receive();
        let second = allocate_receive();
        super::super::space::mach_port_insert_member(space::TASK_SELF, first, set);
        super::super::space::mach_port_insert_member(space::TASK_SELF, second, set);

        let mut sent = message(0, second, MACH_PORT_NULL, 42, &[]);
        // SAFETY: a whole message of the size in its header.
        unsafe {
            mach_msg(
                sent.as_mut_ptr().cast(),
                SEND_MSG,
                sent.len() as u32,
                0,
                MACH_PORT_NULL,
                0,
                MACH_PORT_NULL,
            )
        };

        let mut buf = vec![0u8; 256];
        // SAFETY: 256 writable bytes.
        let rc = unsafe {
            mach_msg(
                buf.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                buf.len() as u32,
                set,
                1000,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_MSG_SUCCESS);
        assert_eq!(word(&buf, 20) as i32, 42);
        assert_eq!(word(&buf, 12), second, "the member, not the set");

        space::destroy(set);
        space::destroy(first);
        space::destroy(second);
    }

    /// A `mach_port_peek`, and what a guest is told.
    ///
    /// The out-parameters come back as `(kr, seqno, size, id, *trailer_size)`, the
    /// trailer itself in `out.6` - which is how the *guest*'s stack frame is laid
    /// out, since Chromium's `PeekPort` has five locals and the call pushes two of
    /// them.
    #[allow(clippy::type_complexity)]
    fn peek(port: Name) -> (i32, u32, u32, u32, u32, [u8; 12]) {
        let (mut seqno, mut size, mut id) = (0xaaaa_aaaa, 0xaaaa_aaaa, 0xaaaa_aaaa);
        let mut trailer = [0xaa; 12];
        let mut offered = TRAILER as u32;
        // SAFETY: three live `natural_t`s, a live `mach_msg_type_number_t` and a
        // twelve-byte trailer buffer that is more than the eight `offered` says.
        let kr = unsafe {
            mach_port_peek(
                space::TASK_SELF,
                port,
                TRAILER_NULL,
                &mut seqno,
                &mut size,
                &mut id,
                trailer.as_mut_ptr(),
                &mut offered,
            )
        };
        (kr, seqno, size, id, offered, trailer)
    }

    /// The answer Chromium's `MACH_CHECK` is built on. An empty port is
    /// `KERN_FAILURE` and nothing else - not `MACH_RCV_TIMED_OUT`, and not a
    /// success with a queue of length zero, either of which aborts the browser
    /// at `waitable_event_apple.cc:251` instead of letting it poll. And a
    /// `KERN_FAILURE` writes nothing through the three out-parameters, so the
    /// caller's own uninitialised values are still there.
    #[test]
    fn an_empty_port_is_kern_failure_and_writes_nothing() {
        let port = allocate_receive();
        let (kr, seqno, size, id, offered, trailer) = peek(port);
        assert_eq!(kr, KERN_FAILURE);
        assert_eq!(
            (seqno, size, id),
            (0xaaaa_aaaa, 0xaaaa_aaaa, 0xaaaa_aaaa),
            "the kernel leaves all three alone when it finds no message"
        );
        assert_eq!(
            offered, TRAILER as u32,
            "the requested trailer size is written before the name is looked up"
        );
        assert_eq!(trailer, [0xaa; 12], "and no trailer is written");
        space::destroy(port);
    }

    /// The other half, and the one the default MANUAL-reset event reaches on its
    /// first `IsSignaled()`. The queue is polled, not drained: three rounds of
    /// signal, peek, receive, and the sequence number rises by one each time.
    #[test]
    fn a_port_with_a_message_reports_it_and_the_sequence_number_rises() {
        let port = allocate_receive();
        let mut previous = 0;
        for round in 0..3u32 {
            let mut sent = message(0, port, MACH_PORT_NULL, 0x1f + round as i32, &[]);
            // SAFETY: a whole message of the size in its header.
            assert_eq!(
                unsafe {
                    mach_msg(
                        sent.as_mut_ptr().cast(),
                        SEND_MSG,
                        sent.len() as u32,
                        0,
                        MACH_PORT_NULL,
                        0,
                        MACH_PORT_NULL,
                    )
                },
                MACH_MSG_SUCCESS
            );

            let (kr, seqno, size, id, _, trailer) = peek(port);
            assert_eq!(kr, KERN_SUCCESS, "round {round}");
            assert_eq!(
                size, HEADER as u32,
                "an empty message is a header and nothing else"
            );
            assert_eq!(
                id,
                0x1f + round as u32,
                "msgh_id of the message at the head"
            );
            assert!(
                seqno > previous,
                "round {round}: the sequence number rises, {previous} then {seqno}"
            );
            previous = seqno;
            let mut expected = Vec::new();
            expected.extend_from_slice(&TRAILER_FORMAT_0.to_ne_bytes());
            expected.extend_from_slice(&(TRAILER as u32).to_ne_bytes());
            assert_eq!(
                &trailer[..TRAILER],
                expected.as_slice(),
                "the trailer is the one a receive would have appended"
            );

            // Polling twice must not consume: this is a peek.
            assert_eq!(peek(port).0, KERN_SUCCESS, "round {round}: still there");

            let mut buf = vec![0u8; 64];
            // SAFETY: 64 writable bytes.
            let rc = unsafe {
                mach_msg(
                    buf.as_mut_ptr().cast(),
                    RCV_MSG | RCV_TIMEOUT,
                    0,
                    buf.len() as u32,
                    port,
                    1000,
                    MACH_PORT_NULL,
                )
            };
            assert_eq!(
                rc, MACH_MSG_SUCCESS,
                "round {round}: the message was received"
            );
            assert_eq!(
                peek(port).0,
                KERN_FAILURE,
                "round {round}: and the queue is empty again"
            );
        }
        assert_eq!(previous, 3, "the first message of a port is numbered one");
        space::destroy(port);
    }

    /// The two questions a caller can ask about a name, and neither of them is
    /// `KERN_FAILURE`: a name this namespace never issued is `KERN_INVALID_NAME`
    /// and a name that is not a receive right is `KERN_INVALID_RIGHT`. Chromium
    /// aborts on all three equally, so which one it is matters.
    #[test]
    fn a_name_that_is_not_a_receive_right_is_told_which_kind_of_wrong_it_is() {
        let mut set = MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe { space::mach_port_allocate(space::TASK_SELF, space::RIGHT_PORT_SET, &mut set) };
        for (name, expected) in [
            (set, KERN_INVALID_RIGHT),
            (0x7fff_0000, KERN_INVALID_NAME),
            (MACH_PORT_NULL, KERN_INVALID_RIGHT), // !MACH_PORT_VALID
            (MACH_PORT_DEAD, KERN_INVALID_RIGHT), // !MACH_PORT_VALID
        ] {
            assert_eq!(peek(name).0, expected, "name {name:#x}");
        }
        // A send right is a name and not a receive right.
        let mut receive = MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe { space::mach_port_allocate(space::TASK_SELF, space::RIGHT_RECEIVE, &mut receive) };
        let (mut send, mut kind) = (MACH_PORT_NULL, 0);
        // SAFETY: two live locals for the out-parameters.
        unsafe {
            space::mach_port_extract_right(
                space::TASK_SELF,
                receive,
                MAKE_SEND,
                &mut send,
                &mut kind,
            )
        };
        assert_eq!(peek(send).0, KERN_INVALID_RIGHT);
        assert_eq!(peek(space::TASK_SELF).0, KERN_INVALID_RIGHT);

        // Another task's space is refused before anything else is looked at,
        // which is what `mach_port_construct` and the rest of the file do.
        let (mut seqno, mut size, mut id, mut offered) = (0u32, 0u32, 0u32, 8u32);
        let mut trailer = [0u8; 8];
        // SAFETY: live out-parameters and an eight-byte trailer buffer.
        let kr = unsafe {
            mach_port_peek(
                space::HOST_SELF,
                receive,
                TRAILER_NULL,
                &mut seqno,
                &mut size,
                &mut id,
                trailer.as_mut_ptr(),
                &mut offered,
            )
        };
        assert_eq!(kr, KERN_INVALID_TASK);

        // A null out-parameter is `KERN_INVALID_ARGUMENT`, which is what MIG's
        // server answers rather than faulting.
        // SAFETY: a null out-parameter is the case being tested.
        assert_eq!(
            unsafe {
                mach_port_peek(
                    space::TASK_SELF,
                    receive,
                    TRAILER_NULL,
                    std::ptr::null_mut(),
                    &mut size,
                    &mut id,
                    trailer.as_mut_ptr(),
                    &mut offered,
                )
            },
            KERN_INVALID_ARGUMENT
        );

        space::destroy(set);
        space::mach_port_deallocate(space::TASK_SELF, send);
        space::destroy(receive);
    }

    /// `MACH_RCV_TRAILER_SEQNO` is answered, because the sequence number is
    /// the one trailer element this namespace does know. The two above it are
    /// refused rather than filled with zeros, and a buffer too small for what
    /// was asked for is `KERN_INVALID_VALUE` the way the kernel says.
    #[test]
    fn the_trailer_element_types_are_answered_or_refused_by_number() {
        let port = allocate_receive();
        let mut sent = message(0, port, MACH_PORT_NULL, 1, &[]);
        // SAFETY: a whole message of the size in its header.
        unsafe {
            mach_msg(
                sent.as_mut_ptr().cast(),
                SEND_MSG,
                sent.len() as u32,
                0,
                MACH_PORT_NULL,
                0,
                MACH_PORT_NULL,
            )
        };

        // `MACH_RCV_TRAILER_ELEMENTS(MACH_RCV_TRAILER_SEQNO)` is `1 << 24`.
        let ask = |elements: u32, offered: u32| {
            let (mut seqno, mut size, mut id) = (0u32, 0u32, 0u32);
            let mut trailer = [0u8; 64];
            let mut out = offered;
            // SAFETY: live out-parameters and a 64-byte trailer buffer.
            let kr = unsafe {
                mach_port_peek(
                    space::TASK_SELF,
                    port,
                    elements << 24,
                    &mut seqno,
                    &mut size,
                    &mut id,
                    trailer.as_mut_ptr(),
                    &mut out,
                )
            };
            (kr, out, trailer)
        };
        let (kr, out, trailer) = ask(TRAILER_SEQNO, 12);
        assert_eq!(kr, KERN_SUCCESS);
        assert_eq!(out, 12, "sizeof(mach_msg_seqno_trailer_t)");
        assert_eq!(
            u32::from_ne_bytes(trailer[8..12].try_into().unwrap()),
            1,
            "msgh_seqno"
        );

        // `MACH_RCV_TRAILER_SENDER` and `_AUDIT` are refused, and so is a
        // `MACH_RCV_TRAILER_CTX`, which the kernel rejects before the name.
        for elements in [TRAILER_SENDER, TRAILER_AUDIT, TRAILER_CTX] {
            assert_eq!(
                ask(elements, 64).0,
                KERN_INVALID_VALUE,
                "elements {elements}"
            );
        }
        // A buffer too small for the trailer that was asked for.
        assert_eq!(ask(TRAILER_SEQNO, 8).0, KERN_INVALID_VALUE);

        // Chromium's own call: `MACH_RCV_TRAILER_TYPE(MACH_RCV_TRAILER_NULL)`
        // is the whole word zero, with eight bytes offered for eight bytes.
        let (kr, out, trailer) = ask(TRAILER_NULL, TRAILER as u32);
        assert_eq!(kr, KERN_SUCCESS);
        assert_eq!(out, TRAILER as u32);
        assert_eq!(
            u32::from_ne_bytes(trailer[0..4].try_into().unwrap()),
            TRAILER_FORMAT_0
        );
        assert_eq!(
            u32::from_ne_bytes(trailer[4..8].try_into().unwrap()),
            TRAILER as u32
        );
        space::destroy(port);
    }

    /// The trailer sizes are read off the *published* structures, and the shape
    /// the kernel's own `<mach/message.h>` uses is a different one: the SDK's
    /// `mach_msg_trailer_t` is `{msgh_trailer_type, msgh_trailer_size}`, not
    /// `{msgh_seqno, msgh_trailer_type}`. Getting it wrong is a `KERN_INVALID_VALUE`
    /// for a caller whose buffer is big enough.
    #[test]
    fn the_trailer_sizes_are_the_ones_the_published_header_declares() {
        assert_eq!(size_of::<TrailerNull>(), 8);
        assert_eq!(size_of::<TrailerSeqno>(), 12);
        assert_eq!(size_of::<TrailerSender>(), 20);
        assert_eq!(size_of::<TrailerAudit>(), 52);
        assert_eq!(requested_trailer_size(TRAILER_NULL), Some(8));
        assert_eq!(requested_trailer_size(TRAILER_AUDIT), Some(52));
        assert_eq!(
            requested_trailer_size(TRAILER_AUDIT + 1),
            None,
            "the kernel stops here"
        );
        // `GET_RCV_ELEMENTS(y)` is `(y >> 24) & 0xf`, so the element type lives
        // in bits 24 to 27 and `MACH_RCV_TRAILER_TYPE`'s format in bits 28 to
        // 31. The kernel reads the first and ignores the second, so a word that
        // puts the element type where the format belongs is read as "no
        // trailer" - which is worth asserting, because the two nibbles being
        // adjacent is how a value that looks right is not.
        assert_eq!(
            trailer_elements(TRAILER_NULL),
            0,
            "the whole word, for no trailer at all"
        );
        assert_eq!(trailer_elements(TRAILER_SEQNO << 24), TRAILER_SEQNO);
        assert_eq!(
            trailer_elements(TRAILER_SEQNO << 28),
            0,
            "the format nibble is not one"
        );
    }

    /// A `mach_port_peek` and a `kevent64` filter ask the same question about
    /// the same port, so they must never disagree: a peek that reports a
    /// message `poll` cannot see is the failure that sends Chromium into a
    /// `mach_msg` that never returns. The two are checked against each other in
    /// `kevent64`'s own test, where both are reachable.
    #[test]
    fn a_peek_agrees_with_what_a_receive_can_take() {
        let port = allocate_receive();
        assert_eq!(peek(port).0, KERN_FAILURE);
        // A receive with a zero timeout is the same question asked the other
        // way round, and it is what `MACH_RCV_TIMED_OUT` answers.
        let mut buf = vec![0u8; 64];
        // SAFETY: 64 writable bytes.
        let rc = unsafe {
            mach_msg(
                buf.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                buf.len() as u32,
                port,
                0,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_RCV_TIMED_OUT);

        let mut sent = message(0, port, MACH_PORT_NULL, 1, &[]);
        // SAFETY: a whole message of the size in its header.
        unsafe {
            mach_msg(
                sent.as_mut_ptr().cast(),
                SEND_MSG,
                sent.len() as u32,
                0,
                MACH_PORT_NULL,
                0,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(peek(port).0, KERN_SUCCESS);
        // SAFETY: 64 writable bytes.
        let rc = unsafe {
            mach_msg(
                buf.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                buf.len() as u32,
                port,
                0,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_MSG_SUCCESS, "and it is there to be taken");
        space::destroy(port);
    }

    /// The whole of `base::WaitableEvent`'s MANUAL-reset path, in the order
    /// Chromium runs it and with the arguments it passes - because a MANUAL-reset
    /// event is the default, so this is the first thing a guest does with a
    /// `WaitableEvent` and the only way to know the two halves of the port queue
    /// agree.
    ///
    /// * `IsSignaled()` is `PeekPort(name, policy_ == AUTOMATIC)`, so a MANUAL
    ///   event is a **peek** - it must report without taking.
    /// * `Signal()` sends a 24-byte `mach_msg_empty_send_t` with
    ///   `MACH_SEND_MSG | MACH_SEND_TIMEOUT`.
    /// * `Wait()` receives with `MACH_RCV_LARGE` and `rcv_size = 0`, so the
    ///   message stays queued and the answer is `MACH_RCV_TOO_LARGE`.
    /// * `Reset()` is `PeekPort(name, true)`: a real receive with
    ///   `MACH_RCV_TIMEOUT` and a zero timeout.
    #[test]
    fn a_manual_reset_event_polls_signals_waits_and_resets() {
        let port = allocate_receive();

        // IsSignaled(), before anything has been sent.
        assert_eq!(peek(port).0, KERN_FAILURE, "not signalled");

        // Signal().
        let mut sent = message(0, port, MACH_PORT_NULL, 0, &[]);
        assert_eq!(sent.len(), HEADER);
        // SAFETY: a whole message of the size in its header.
        assert_eq!(
            unsafe {
                mach_msg(
                    sent.as_mut_ptr().cast(),
                    SEND_MSG | SEND_TIMEOUT,
                    sent.len() as u32,
                    0,
                    MACH_PORT_NULL,
                    0,
                    MACH_PORT_NULL,
                )
            },
            MACH_MSG_SUCCESS
        );

        // IsSignaled(), twice, and a MANUAL-reset event must not drain.
        assert_eq!(peek(port).0, KERN_SUCCESS);
        assert_eq!(
            peek(port).0,
            KERN_SUCCESS,
            "still signalled: nothing was dequeued"
        );

        // Wait(): a zero-sized receive with MACH_RCV_LARGE, which is how Chromium
        // waits on a MANUAL event without resetting it.
        let mut buf = vec![0u8; 0];
        // SAFETY: zero writable bytes, which is what the caller asked for.
        let rc = unsafe {
            mach_msg(
                buf.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT | RCV_LARGE,
                0,
                0,
                port,
                0,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(
            rc, MACH_RCV_TOO_LARGE,
            "Chromium's Wait() on a MANUAL event"
        );
        assert_eq!(peek(port).0, KERN_SUCCESS, "MACH_RCV_LARGE left it queued");

        // Reset(): PeekPort(port, true). `mach_msg_empty_rcv_t` is a header *and*
        // a `mach_msg_trailer_t` - 32 bytes, not 24 - and this is the one place
        // that shows the trailer is accounted for: `rcv_size` has to cover the
        // trailer or the receive is `MACH_RCV_TOO_LARGE` and takes nothing.
        let mut buf = vec![0u8; HEADER + TRAILER];
        // SAFETY: a header's worth of writable bytes.
        let rc = unsafe {
            mach_msg(
                buf.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                buf.len() as u32,
                port,
                0,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_MSG_SUCCESS, "the message was taken");
        assert_eq!(peek(port).0, KERN_FAILURE, "and the event is reset");
        // A second Reset() has nothing to take, which is what it says.
        // SAFETY: a header's worth of writable bytes.
        let rc = unsafe {
            mach_msg(
                buf.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                buf.len() as u32,
                port,
                0,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(
            rc, MACH_RCV_TIMED_OUT,
            "PeekPort's own MACH_CHECK accepts this"
        );
        space::destroy(port);
    }

    /// The same object as an AUTOMATIC-reset event, where `IsSignaled()` is
    /// `PeekPort(name, true)` and therefore *does* dequeue. This is the case
    /// `WaitManyImpl` takes when it reports a triggered index.
    #[test]
    fn an_automatic_reset_event_drains_itself_when_it_is_polled() {
        let port = allocate_receive();
        let mut sent = message(0, port, MACH_PORT_NULL, 0, &[]);
        // SAFETY: a whole message of the size in its header.
        unsafe {
            mach_msg(
                sent.as_mut_ptr().cast(),
                SEND_MSG | SEND_TIMEOUT,
                sent.len() as u32,
                0,
                MACH_PORT_NULL,
                0,
                MACH_PORT_NULL,
            )
        };
        // `IsSignaled()` on an AUTOMATIC event is PeekPort(port, true), whose
        // buffer is a `mach_msg_empty_rcv_t`: a header and a trailer.
        let mut buf = vec![0u8; HEADER + TRAILER];
        // SAFETY: a header's worth of writable bytes.
        let rc = unsafe {
            mach_msg(
                buf.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                buf.len() as u32,
                port,
                0,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_MSG_SUCCESS);
        assert_eq!(peek(port).0, KERN_FAILURE, "polled and reset in one call");
        space::destroy(port);
    }

    #[test]
    fn a_migs_reply_port_is_one_per_thread_and_survives_until_it_is_thrown_away() {
        let first = mig_get_reply_port();
        assert_ne!(first, MACH_PORT_NULL);
        assert_eq!(mig_get_reply_port(), first);
        mig_dealloc_reply_port(first);
        let second = mig_get_reply_port();
        assert_ne!(second, first);

        let other = std::thread::spawn(|| mig_get_reply_port()).join().unwrap();
        assert_ne!(other, second, "a reply port belongs to one thread");
        mig_dealloc_reply_port(second);
    }
}
