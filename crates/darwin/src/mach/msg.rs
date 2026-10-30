use std::ffi::c_void;
use std::os::fd::FromRawFd;

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
    /// The descriptor that travelled with this right: a receive right's port,
    /// or a send right's claim on the port. `-1` for none.
    fd_index: i16,
    /// For a receive right, what follows its port in the descriptors: whether its
    /// listener does, how many claims on it do, and whether those are all of
    /// them.
    listener: bool,
    claims: u16,
    complete: bool,
    address: u64,
}

impl Reference {
    const NONE: Reference = Reference {
        kind: refkind::NONE,
        disposition: 0,
        fd_index: -1,
        listener: false,
        claims: 0,
        complete: false,
        address: 0,
    };

    fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.kind);
        out.push(self.disposition);
        out.extend_from_slice(&self.fd_index.to_le_bytes());
        let extra = u32::from(self.claims)
            | u32::from(self.listener) << 16
            | u32::from(self.complete) << 17;
        out.extend_from_slice(&extra.to_le_bytes());
        out.extend_from_slice(&self.address.to_le_bytes());
    }

    fn decode(bytes: &[u8]) -> Option<Reference> {
        let bytes: &[u8; REFERENCE] = bytes.get(..REFERENCE)?.try_into().ok()?;
        let extra = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
        Some(Reference {
            kind: bytes[0],
            disposition: bytes[1],
            fd_index: i16::from_le_bytes([bytes[2], bytes[3]]),
            listener: extra & 1 << 16 != 0,
            claims: (extra & 0xffff) as u16,
            complete: extra & 1 << 17 != 0,
            address: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
        })
    }

    /// Put this reference into the receiving task's space and answer with the
    /// name it got.
    fn adopt(&self, message: &mut Datagram) -> Name {
        match self.kind {
            refkind::SEND => match self.disposition as u32 {
                MOVE_SEND_ONCE => space::insert_send_once(self.address),
                _ => {
                    let claim = usize::try_from(self.fd_index)
                        .ok()
                        .and_then(|index| message.take_fd(index))
                        // SAFETY: a descriptor that arrived over `SCM_RIGHTS`,
                        // now owned by this process.
                        .map(|fd| unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
                    space::insert_send_with(self.address, claim)
                }
            },
            refkind::RECEIVE => match message.take_fd(self.fd_index.max(0) as usize) {
                Some(fd) => {
                    let base = self.fd_index.max(0) as usize;
                    let senders = match self.listener {
                        true => message.take_fd(base + 1).unwrap_or(-1),
                        false => -1,
                    };
                    let claims: Vec<_> = (0..self.claims as usize)
                        .filter_map(|i| message.take_fd(base + 2 + i))
                        .collect();
                    let complete = self.complete && claims.len() == self.claims as usize;
                    // SAFETY: descriptors that arrived over `SCM_RIGHTS` for
                    // the socket bound to `address`, its listener and the
                    // claims on it, now owned by this process.
                    space::insert_receive(unsafe {
                        Receiver::adopt(fd, senders, claims, complete, self.address)
                    })
                }
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
    /// A receive right already out of the space, with the descriptors to send:
    /// the port, and its listener or `-1`.
    Receive {
        name: Name,
        moved: wire::Moved,
        address: u64,
    },
}

impl Pending {
    /// The send failed: put back whatever was taken.
    fn restore(self) {
        match self {
            Pending::Copied | Pending::Moved(_) => {}
            Pending::Receive {
                name,
                moved,
                address,
            } => {
                // SAFETY: the descriptors taken out of the space above, never
                // handed to anything else.
                space::restore_receive(name, unsafe {
                    Receiver::adopt(
                        moved.fd,
                        moved.senders,
                        moved.claims,
                        moved.complete,
                        address,
                    )
                });
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
            Pending::Receive { moved, .. } => {
                // The kernel holds the in-flight reference until the receiver
                // takes it, so this end is done with the descriptors.
                // SAFETY: this process's own copies, already sent.
                unsafe {
                    libc::close(moved.fd);
                    if moved.senders >= 0 {
                        libc::close(moved.senders);
                    }
                    for claim in moved.claims {
                        libc::close(claim);
                    }
                }
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
            let moved = port.into_parts();
            Ok((
                Reference {
                    kind: refkind::RECEIVE,
                    disposition: MOVE_RECEIVE as u8,
                    fd_index: -1,
                    listener: moved.senders >= 0,
                    claims: 0,
                    complete: moved.complete,
                    address,
                },
                Some(Pending::Receive {
                    name,
                    moved,
                    address,
                }),
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
                    listener: false,
                    claims: 0,
                    complete: false,
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
                    listener: false,
                    claims: 0,
                    complete: false,
                    address,
                },
                Some(held),
            ))
        }
        _ => Err(MACH_SEND_INVALID_TYPE),
    }
}

/// The datagram for a message: the envelope, its references, then the payload.
fn encode(
    bits: u32,
    id: i32,
    reply: &Reference,
    descriptors: &[Reference],
    payload: &[u8],
) -> Vec<u8> {
    let mut wire_bytes =
        Vec::with_capacity(envelope::FIXED + descriptors.len() * REFERENCE + payload.len());
    wire_bytes.extend_from_slice(&envelope::MAGIC.to_le_bytes());
    wire_bytes.extend_from_slice(&envelope::VERSION.to_le_bytes());
    wire_bytes.extend_from_slice(&bits.to_le_bytes());
    wire_bytes.extend_from_slice(&id.to_le_bytes());
    wire_bytes.extend_from_slice(&(descriptors.len() as u32).to_le_bytes());
    wire_bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    reply.encode(&mut wire_bytes);
    for reference in descriptors {
        reference.encode(&mut wire_bytes);
    }
    wire_bytes.extend_from_slice(payload);
    wire_bytes
}

/// Queue a `MACH_NOTIFY_NO_SENDERS` message on the port at `destination`.
///
/// What a kernel would write on a port's behalf: a `mach_no_senders_notification_t`
/// - header, `NDR_record_t`, and the make-send count - arriving as a message with
/// no reply port, which is how a MIG notify server tells it from a request.
pub fn deliver_no_senders(destination: wire::Address, count: u32) {
    /// `NDR_record`: little-endian integers, ASCII characters, IEEE floats.
    const NDR: [u8; 8] = [0, 0, 0, 0, 1, 0, 0, 0];
    let mut payload = Vec::with_capacity(NDR.len() + 4);
    payload.extend_from_slice(&NDR);
    payload.extend_from_slice(&count.to_le_bytes());
    let bytes = encode(
        MOVE_SEND_ONCE,
        space::NOTIFY_NO_SENDERS,
        &Reference::NONE,
        &[],
        &payload,
    );
    // A destination that is gone has nothing left to tell.
    let _ = wire::send(destination, &bytes, &[]);
}

/// Put the descriptors a right travels with beside `fds`, and say where they are.
///
/// A send right carries a fresh claim on its port rather than the sender's own,
/// so the right counts as a sender while the message is in flight and after the
/// sender has let go of its copy. `claims` keeps this process's end open until
/// the send has finished.
fn attach(
    reference: &mut Reference,
    held: Option<&Pending>,
    fds: &mut Vec<std::ffi::c_int>,
    claims: &mut Vec<std::os::fd::OwnedFd>,
) {
    use std::os::fd::AsRawFd;
    match reference.kind {
        refkind::RECEIVE => {
            if let Some(Pending::Receive { moved, .. }) = held {
                reference.fd_index = fds.len() as i16;
                fds.push(moved.fd);
                if moved.senders >= 0 {
                    fds.push(moved.senders);
                }
                // As many of the port's claims as fit; the rest are closed once
                // the send is done, and the port is then no longer complete.
                let room = wire::MAX_FDS.saturating_sub(fds.len());
                let carried = moved.claims.len().min(room);
                fds.extend(&moved.claims[..carried]);
                reference.claims = carried as u16;
                reference.complete = moved.complete && carried == moved.claims.len();
            }
        }
        refkind::SEND if reference.disposition as u32 != MOVE_SEND_ONCE => {
            if let Some(claim) = wire::claim(reference.address) {
                reference.fd_index = fds.len() as i16;
                fds.push(claim.as_raw_fd());
                claims.push(claim);
            }
        }
        _ => {}
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
    let mut fds = Vec::new();
    let mut claims = Vec::new();
    let reply = match export(local_name, local_disposition) {
        Ok((mut reference, held)) => {
            attach(&mut reference, held.as_ref(), &mut fds, &mut claims);
            pending.extend(held);
            reference
        }
        Err(code) => return failure(pending, code),
    };

    let complex = bits & BITS_COMPLEX != 0;
    let mut descriptors = Vec::new();
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
                    attach(&mut reference, held.as_ref(), &mut fds, &mut claims);
                    pending.extend(held);
                    descriptors.push(reference);
                }
                Err(code) => return failure(pending, code),
            }
        }
    }

    let wire_bytes = encode(bits, id, &reply, &descriptors, &bytes[payload_at..]);

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
            "rosemary: mach_msg refused a message carrying out-of-line memory - a page mapped into \
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
    #[cfg(target_os = "linux")]
    fn descriptor_pressure_preserves_moved_receive_right_names() {
        let reservation = super::super::credits::reserve(256).unwrap();
        for option in [SEND_MSG, SEND_MSG | SEND_TIMEOUT] {
            let service = allocate_receive();
            let carried = allocate_receive();
            let address = space::address_of(carried).unwrap();
            let mut descriptor = Vec::new();
            descriptor.extend_from_slice(&1u32.to_le_bytes());
            descriptor.extend_from_slice(&carried.to_le_bytes());
            descriptor.extend_from_slice(&0u32.to_le_bytes());
            descriptor.extend_from_slice(&[0, 0, MOVE_RECEIVE as u8, DESCRIPTOR_PORT]);
            let mut sent = message(BITS_COMPLEX, service, MACH_PORT_NULL, 7, &descriptor);
            let result = unsafe {
                mach_msg(
                    sent.as_mut_ptr().cast(),
                    option,
                    sent.len() as u32,
                    0,
                    MACH_PORT_NULL,
                    0,
                    MACH_PORT_NULL,
                )
            };
            assert_eq!(
                result,
                if option & SEND_TIMEOUT != 0 {
                    MACH_SEND_TIMED_OUT
                } else {
                    MACH_SEND_NO_BUFFER
                }
            );
            assert_eq!(space::address_of(carried), Some(address));
            assert!(space::receiver(carried).is_some());
            space::mach_port_destroy(space::TASK_SELF, service);
            space::mach_port_destroy(space::TASK_SELF, carried);
        }
        drop(reservation);
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

    /// Receive one message from `port` into a fresh buffer, or `None` on timeout.
    fn take(port: Name, timeout_ms: u32) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 256];
        // SAFETY: 256 writable bytes.
        let rc = unsafe {
            mach_msg(
                buf.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT,
                0,
                buf.len() as u32,
                port,
                timeout_ms,
                MACH_PORT_NULL,
            )
        };
        (rc == MACH_MSG_SUCCESS).then_some(buf)
    }

    fn request_no_senders(port: Name, notify: Name) {
        // SAFETY: a null `previous` is allowed.
        let rc = unsafe {
            space::mach_port_request_notification(
                space::TASK_SELF,
                port,
                space::NOTIFY_NO_SENDERS,
                0,
                notify,
                MAKE_SEND_ONCE,
                core::ptr::null_mut(),
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
    }

    #[test]
    fn the_last_send_right_going_away_is_reported_to_the_notify_port() {
        let port = allocate_receive();
        let notify = allocate_receive();
        let first = space::insert_send(space::address_of(port).unwrap());
        let second = space::insert_send(space::address_of(port).unwrap());
        assert_eq!(first, second, "one name for one port");
        request_no_senders(port, notify);

        assert!(take(notify, 200).is_none(), "a sender is still held");
        space::mach_port_deallocate(space::TASK_SELF, first);
        assert!(take(notify, 200).is_none(), "one of two references remains");
        space::mach_port_deallocate(space::TASK_SELF, first);

        let told = take(notify, 2000).expect("a no-senders notification");
        assert_eq!(word(&told, 20) as i32, 0o106, "MACH_NOTIFY_NO_SENDERS");
        assert_eq!(
            word(&told, 4) as usize,
            HEADER + 8 + 4,
            "a notification's size"
        );
        assert_eq!(word(&told, 12), notify, "it arrived on the notify port");
        assert_eq!(word(&told, 8), MACH_PORT_NULL, "and has nobody to reply to");
        assert_eq!(word(&told, HEADER + 8), 1, "one right was ever made");

        space::destroy(port);
        space::destroy(notify);
    }

    /// A right inside a queued message is a sender, which is what keeps a port
    /// that was just handed to a client from being declared abandoned before the
    /// client has looked at the message.
    #[test]
    fn a_send_right_in_flight_still_counts_as_a_sender() {
        let port = allocate_receive();
        let notify = allocate_receive();
        let mailbox = allocate_receive();
        let held = space::insert_send(space::address_of(port).unwrap());
        request_no_senders(port, notify);

        let mut descriptor = Vec::new();
        descriptor.extend_from_slice(&1u32.to_le_bytes());
        descriptor.extend_from_slice(&held.to_le_bytes());
        descriptor.extend_from_slice(&0u32.to_le_bytes());
        descriptor.extend_from_slice(&[0, 0, MOVE_SEND as u8, DESCRIPTOR_PORT]);
        let mut sent = message(BITS_COMPLEX, mailbox, MACH_PORT_NULL, 9, &descriptor);
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
            space::address_of(held).is_none(),
            "the sender gave its right away"
        );
        assert!(
            take(notify, 300).is_none(),
            "the right is in the mailbox, not gone"
        );

        let arrived = take(mailbox, 1000).expect("the message");
        let received = word(&arrived, HEADER + BODY);
        assert_eq!(
            space::address_of(received),
            space::address_of(port),
            "it names the same port"
        );
        assert!(take(notify, 300).is_none(), "now the receiver holds it");

        space::mach_port_deallocate(space::TASK_SELF, received);
        assert!(take(notify, 2000).is_some(), "and now nobody does");

        space::destroy(port);
        space::destroy(notify);
        space::destroy(mailbox);
    }

    /// The case this exists for: the holder of the right is another process, and
    /// it exiting is the only thing that releases it.
    #[test]
    fn a_holder_process_that_exits_is_reported_as_no_senders() {
        let port = allocate_receive();
        let notify = allocate_receive();
        let address = space::address_of(port).unwrap();
        request_no_senders(port, notify);

        let claim = wire::claim(address).expect("a claim");
        // SAFETY: the child calls only async-signal-safe functions and never
        // returns into the test harness.
        let child = unsafe { libc::fork() };
        if child == 0 {
            unsafe {
                libc::usleep(300_000);
                libc::_exit(0);
            }
        }
        assert!(child > 0, "fork");
        // The child's copy is the only one left.
        drop(claim);
        assert!(take(notify, 100).is_none(), "the child still holds it");
        let mut status = 0;
        // SAFETY: the child just forked, and a live local for the status.
        unsafe { libc::waitpid(child, &mut status, 0) };
        assert!(take(notify, 2000).is_some(), "the holder is gone");

        space::destroy(port);
        space::destroy(notify);
    }

    /// How a crash reporter hands over its exception port: the client creates it,
    /// keeps a send right, and moves the receive right to its handler - which
    /// then asks to be told when the client's right is gone.
    #[test]
    fn a_moved_receive_right_keeps_the_senders_it_already_had() {
        let port = allocate_receive();
        let notify = allocate_receive();
        let mailbox = allocate_receive();
        let address = space::address_of(port).unwrap();
        let held = space::insert_send(address);

        let mut descriptor = Vec::new();
        descriptor.extend_from_slice(&1u32.to_le_bytes());
        descriptor.extend_from_slice(&port.to_le_bytes());
        descriptor.extend_from_slice(&0u32.to_le_bytes());
        descriptor.extend_from_slice(&[0, 0, MOVE_RECEIVE as u8, DESCRIPTOR_PORT]);
        let mut sent = message(BITS_COMPLEX, mailbox, MACH_PORT_NULL, 11, &descriptor);
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
        let arrived = take(mailbox, 1000).expect("the message");
        let moved = word(&arrived, HEADER + BODY);
        assert!(space::receiver(moved).is_some(), "a receive right again");

        request_no_senders(moved, notify);
        assert!(
            take(notify, 300).is_none(),
            "the sender's right is still held"
        );
        space::mach_port_deallocate(space::TASK_SELF, held);
        assert!(take(notify, 2000).is_some(), "and then it is not");

        space::destroy(moved);
        space::destroy(notify);
        space::destroy(mailbox);
    }

    /// A crash reporter registers its handler's port and lets go of its own
    /// right at once, because the kernel keeps one.
    #[test]
    fn a_registered_exception_port_is_a_sender_until_it_is_replaced() {
        const MASK: u32 = 1 << 9;
        let port = allocate_receive();
        let notify = allocate_receive();
        let held = space::insert_send(space::address_of(port).unwrap());
        super::super::task::task_set_exception_ports(space::TASK_SELF, MASK, held, 1, 0);
        request_no_senders(port, notify);

        space::mach_port_deallocate(space::TASK_SELF, held);
        assert!(
            take(notify, 300).is_none(),
            "the registration holds the port"
        );
        super::super::task::task_set_exception_ports(space::TASK_SELF, MASK, MACH_PORT_NULL, 0, 0);
        assert!(take(notify, 2000).is_some(), "until it is cleared");

        space::destroy(port);
        space::destroy(notify);
    }
}
