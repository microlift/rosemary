use std::ffi::c_void;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use super::kern::*;
use super::local;
use super::ool;
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

/// `MACH_MSG_*_DESCRIPTOR`, and where a 64-bit descriptor's fields sit.
///
/// The type byte is at offset 11 of *every* descriptor, whatever its kind and
/// whatever its length - the headers lay the bitfields out that way on purpose,
/// so a reader can tell what it is looking at before it knows how far to step.
mod descriptor {
    pub const PORT: u8 = 0;
    pub const OOL: u8 = 1;
    pub const OOL_PORTS: u8 = 2;
    /// Copied exactly as `OOL` is; only `mach_msg_destroy` tells them apart.
    pub const OOL_VOLATILE: u8 = 3;

    pub const DEALLOCATE_AT: usize = 8;
    pub const COPY_AT: usize = 9;
    /// A port's, or a port array's, disposition.
    pub const DISPOSITION_AT: usize = 10;
    pub const TYPE_AT: usize = 11;
    /// An out-of-line region's size, or a port array's count.
    pub const SIZE_AT: usize = 12;

    /// `mach_msg_port_descriptor_t`.
    pub const PORT_LEN: usize = 12;
    /// `mach_msg_ool_descriptor64_t` and `mach_msg_ool_ports_descriptor64_t`.
    pub const OOL_LEN: usize = 16;
}

/// `MACH_MSG_PHYSICAL_COPY` and `MACH_MSG_VIRTUAL_COPY`, the only copy options
/// a sender may give out-of-line memory.
const PHYSICAL_COPY: u8 = 0;
const VIRTUAL_COPY: u8 = 1;

/// `mach_msg_header_t` and `mach_msg_body_t`, in bytes. Both are the same size
/// on x86-64 and arm64.
const HEADER: usize = 24;
const BODY: usize = 4;

/// `mach_msg_trailer_t`: the eight bytes the kernel appends to a received
/// message. `msgh_size` does not include it, but `rcv_size` has to - which is
/// the arithmetic a receiver gets wrong and then blames on the sender.
const TRAILER: usize = 8;
/// `MACH_MSG_TRAILER_FORMAT_0`.
const TRAILER_FORMAT_0: u32 = 0;

/// `MACH_RCV_TRAILER_ELEMENTS(option)`: how much trailer a receiver asks for.
const TRAILER_ELEMENTS_SHIFT: u32 = 24;
const TRAILER_ELEMENTS_MASK: i32 = 0xf;

/// The trailers `<mach/message.h>` defines, by `MACH_RCV_TRAILER_*` element
/// count, and the size `REQUESTED_TRAILER_SIZE` gives each.
mod trailer {
    pub const SEQNO: u32 = 1;
    pub const SENDER: u32 = 2;
    pub const AUDIT: u32 = 3;
    pub const SIZES: [usize; 8] = [8, 12, 20, 52, 60, 64, 68, 68];
    /// Offsets of `msgh_seqno`, `msgh_sender` and `msgh_audit`.
    pub const SEQNO_AT: usize = 8;
    pub const SENDER_AT: usize = 12;
    pub const AUDIT_AT: usize = 20;
}

/// What a trailer says about a message's sender: its `audit_token_t`.
///
/// The host pid is the kernel's word, through the socket; who that process is
/// as a guest - its ids - is the process server's.
fn audit_token(sender: Option<i32>) -> [u32; 8] {
    let Some(host) = sender else {
        return [0; 8];
    };
    let pid = crate::process::guest_pid(host);
    let ids = match host == crate::process::host_self() {
        true => {
            let me = crate::credentials::current();
            Some((me.ruid, me.euid, me.rgid, me.egid))
        }
        false => {
            let _host = crate::syscall::ScopedHostSyscallContext::new();
            server::client::process(host).map(|p| (p.uid, p.euid, p.gid, p.egid))
        }
    };
    let Some((uid, euid, gid, egid)) = ids else {
        // FIXME (ACCURACY): a sender the process server does not know has no
        // ids to report, so only its pid is told.
        return [0, 0, 0, 0, 0, pid as u32, 0, 0];
    };
    // `audit_token_to_au32`'s order: auid, euid, egid, ruid, rgid, pid, asid,
    // pidversion. FIXME (ACCURACY): no audit sessions or pid versions are
    // kept, so auid, asid and pidversion read zero.
    [0, euid, egid, uid, gid, pid as u32, 0, 0]
}

/// The envelope that crosses the socket, in front of the message's own bytes:
/// the fixed part, a [`Record`] per descriptor, every right the descriptors
/// carry as a [`Reference`], then the payload.
mod envelope {
    pub const MAGIC: u32 = 0x3372_4d43;
    pub const VERSION: u32 = 2;
    /// Offsets in the fixed part.
    pub const BITS_AT: usize = 8;
    pub const ID_AT: usize = 12;
    pub const RECORDS_AT: usize = 16;
    pub const REFERENCES_AT: usize = 20;
    pub const PAYLOAD_AT: usize = 24;
    /// Which of the datagram's descriptors is the out-of-line memory object,
    /// and how many bytes of it the message uses.
    pub const MEMORY_FD_AT: usize = 28;
    pub const MEMORY_LEN_AT: usize = 32;
    pub const REPLY_AT: usize = 40;
    pub const FIXED: usize = REPLY_AT + super::REFERENCE;
}

/// A port reference as it crosses: 16 bytes, never a name.
const REFERENCE: usize = 16;

/// A descriptor as it crosses: its type, the disposition and copy option the
/// receiver sees, a count, and where its contents are - the index of its first
/// [`Reference`] for a port or a port array, its offset in the memory object for
/// out-of-line memory.
#[derive(Clone, Copy)]
struct Record {
    kind: u8,
    disposition: u8,
    copy: u8,
    /// One for a port, the names in a port array, the bytes of a region.
    count: u32,
    at: u64,
}

const RECORD: usize = 16;

impl Record {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[self.kind, self.disposition, self.copy, 0]);
        out.extend_from_slice(&self.count.to_le_bytes());
        out.extend_from_slice(&self.at.to_le_bytes());
    }

    fn decode(bytes: &[u8]) -> Option<Record> {
        let bytes: &[u8; RECORD] = bytes.get(..RECORD)?.try_into().ok()?;
        Some(Record {
            kind: bytes[0],
            disposition: bytes[1],
            copy: bytes[2],
            count: word(bytes, 4),
            at: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
        })
    }

    /// The length of the descriptor the receiver is given for this.
    fn received_len(&self) -> usize {
        match self.kind {
            descriptor::PORT => descriptor::PORT_LEN,
            _ => descriptor::OOL_LEN,
        }
    }

    /// Whether this stays within the `references` and `memory` bytes that
    /// came with it.
    fn fits(&self, references: usize, memory: u64) -> bool {
        let end = self.at.checked_add(u64::from(self.count));
        match self.kind {
            descriptor::PORT => self.count == 1 && end.is_some_and(|end| end <= references as u64),
            descriptor::OOL_PORTS => end.is_some_and(|end| end <= references as u64),
            descriptor::OOL | descriptor::OOL_VOLATILE => end.is_some_and(|end| end <= memory),
            _ => false,
        }
    }
}

/// A descriptor as it sits in a message, sent or received.
#[derive(Clone, Copy)]
enum Descriptor {
    Port {
        name: Name,
        disposition: u32,
    },
    Memory {
        address: u64,
        size: u32,
        copy: u8,
        deallocate: bool,
        volatile: bool,
    },
    Ports {
        address: u64,
        count: u32,
        disposition: u32,
        deallocate: bool,
    },
}

impl Descriptor {
    /// The descriptor at `at` in `bytes`, and its length.
    ///
    /// `MACH_SEND_INVALID_TYPE` for a kind this transport does not carry, and
    /// `MACH_SEND_INVALID_DATA` for one that runs past the message.
    fn parse(bytes: &[u8], at: usize) -> Result<(Descriptor, usize), i32> {
        let kind = *bytes
            .get(at + descriptor::TYPE_AT)
            .ok_or(MACH_SEND_INVALID_DATA)?;
        let len = match kind {
            descriptor::PORT => descriptor::PORT_LEN,
            descriptor::OOL | descriptor::OOL_PORTS | descriptor::OOL_VOLATILE => {
                descriptor::OOL_LEN
            }
            _ => {
                announce_unsupported(kind);
                return Err(MACH_SEND_INVALID_TYPE);
            }
        };
        let field = bytes.get(at..at + len).ok_or(MACH_SEND_INVALID_DATA)?;
        let disposition = u32::from(field[descriptor::DISPOSITION_AT]);
        if kind == descriptor::PORT {
            let name = word(field, 0);
            return Ok((Descriptor::Port { name, disposition }, len));
        }
        let address = u64::from_le_bytes(field[..8].try_into().unwrap());
        let size = word(field, descriptor::SIZE_AT);
        let deallocate = field[descriptor::DEALLOCATE_AT] != 0;
        let parsed = match kind {
            descriptor::OOL_PORTS => Descriptor::Ports {
                address,
                count: size,
                disposition,
                deallocate,
            },
            _ => Descriptor::Memory {
                address,
                size,
                copy: field[descriptor::COPY_AT],
                deallocate,
                volatile: kind == descriptor::OOL_VOLATILE,
            },
        };
        Ok((parsed, len))
    }

    /// Append this descriptor as a receiver is given it.
    ///
    /// A port array always arrives in pages of its own that the receiver is
    /// told to deallocate; memory is to be deallocated when it was a virtual
    /// copy, which is what `ipc_kmsg_deflate_memory_descriptor` says too.
    fn write(&self, out: &mut Vec<u8>) {
        match *self {
            Descriptor::Port { name, disposition } => {
                out.extend_from_slice(&name.to_le_bytes());
                out.extend_from_slice(&[0; 6]);
                out.extend_from_slice(&[disposition as u8, descriptor::PORT]);
            }
            Descriptor::Memory {
                address,
                size,
                copy,
                volatile,
                ..
            } => {
                let kind = match volatile {
                    true => descriptor::OOL_VOLATILE,
                    false => descriptor::OOL,
                };
                out.extend_from_slice(&address.to_le_bytes());
                out.extend_from_slice(&[u8::from(copy == VIRTUAL_COPY), copy, 0, kind]);
                out.extend_from_slice(&size.to_le_bytes());
            }
            Descriptor::Ports {
                address,
                count,
                disposition,
                ..
            } => {
                out.extend_from_slice(&address.to_le_bytes());
                out.extend_from_slice(&[1, VIRTUAL_COPY, disposition as u8, descriptor::OOL_PORTS]);
                out.extend_from_slice(&count.to_le_bytes());
            }
        }
    }
}

/// The disposition a right sent under `disposition` arrives with, as
/// `ipc_object_copyin_type` answers it: zero for one that names no right.
fn arrives_as(disposition: u32) -> u32 {
    match disposition {
        MOVE_RECEIVE => MOVE_RECEIVE,
        MOVE_SEND | COPY_SEND | MAKE_SEND => MOVE_SEND,
        MOVE_SEND_ONCE | MAKE_SEND_ONCE => MOVE_SEND_ONCE,
        _ => 0,
    }
}

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
    Receive { moved: wire::Moved, address: u64 },
    /// Out-of-line memory, or a port array, the sender asked to give up.
    Deallocate { address: u64, size: u64 },
}

impl Pending {
    /// The send failed: put back whatever was taken.
    fn restore(self) {
        match self {
            Pending::Copied | Pending::Moved(_) | Pending::Deallocate { .. } => {}
            Pending::Receive { moved, address } => {
                // SAFETY: the descriptors taken out of the space above, never
                // handed to anything else.
                space::insert_receive(unsafe {
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
            Pending::Deallocate { address, size } => {
                // SAFETY: memory the sender said it was done with once sent.
                unsafe { super::vm::vm_deallocate(space::TASK_SELF, address, size) };
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
                Some(Pending::Receive { moved, address }),
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

/// What a message carries besides its payload, ready to cross.
struct Carried {
    reply: Reference,
    records: Vec<Record>,
    references: Vec<Reference>,
    /// The memory object's index among the datagram's descriptors, and how many
    /// of its bytes the records use.
    memory: Option<(usize, u64)>,
}

impl Carried {
    const NOTHING: Carried = Carried {
        reply: Reference::NONE,
        records: Vec::new(),
        references: Vec::new(),
        memory: None,
    };
}

/// The datagram for a message: the envelope, then the payload.
fn encode(bits: u32, id: i32, carried: &Carried, payload: &[u8]) -> Vec<u8> {
    let (memory_fd, memory_len) = match carried.memory {
        Some((index, len)) => (index as i32, len),
        None => (-1, 0),
    };
    let mut wire_bytes = Vec::with_capacity(
        envelope::FIXED
            + carried.records.len() * RECORD
            + carried.references.len() * REFERENCE
            + payload.len(),
    );
    wire_bytes.extend_from_slice(&envelope::MAGIC.to_le_bytes());
    wire_bytes.extend_from_slice(&envelope::VERSION.to_le_bytes());
    wire_bytes.extend_from_slice(&bits.to_le_bytes());
    wire_bytes.extend_from_slice(&id.to_le_bytes());
    wire_bytes.extend_from_slice(&(carried.records.len() as u32).to_le_bytes());
    wire_bytes.extend_from_slice(&(carried.references.len() as u32).to_le_bytes());
    wire_bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    wire_bytes.extend_from_slice(&memory_fd.to_le_bytes());
    wire_bytes.extend_from_slice(&memory_len.to_le_bytes());
    carried.reply.encode(&mut wire_bytes);
    for record in &carried.records {
        record.encode(&mut wire_bytes);
    }
    for reference in &carried.references {
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
        &Carried::NOTHING,
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
    // Every right leaves this task's space through here, and `failure` puts it
    // back if the message never goes.
    let mut take = |name: Name, disposition: u32, pending: &mut Vec<Pending>| {
        let (mut reference, held) = export(name, disposition)?;
        attach(&mut reference, held.as_ref(), &mut fds, &mut claims);
        pending.extend(held);
        Ok::<_, i32>(reference)
    };
    let reply = match take(local_name, local_disposition, &mut pending) {
        Ok(reference) => reference,
        Err(code) => return failure(pending, code),
    };

    let mut records = Vec::new();
    let mut references = Vec::new();
    // Out-of-line memory as `(address, size)`, in the order the records use.
    let mut regions = Vec::new();
    let mut memory_len = 0u64;
    let mut payload_at = HEADER;

    if bits & BITS_COMPLEX != 0 {
        if size < HEADER + BODY {
            return failure(pending, MACH_SEND_INVALID_DATA);
        }
        let count = word(&bytes, HEADER);
        let mut at = HEADER + BODY;
        for _ in 0..count {
            let (descriptor, len) = match Descriptor::parse(&bytes, at) {
                Ok(parsed) => parsed,
                Err(code) => return failure(pending, code),
            };
            at += len;
            let first = references.len() as u64;
            let record = match descriptor {
                Descriptor::Port { name, disposition } => {
                    if references.len() >= MAX_RIGHTS {
                        return failure(pending, MACH_SEND_TOO_LARGE);
                    }
                    match take(name, disposition, &mut pending) {
                        Ok(reference) => references.push(reference),
                        Err(code) => return failure(pending, code),
                    }
                    Record {
                        kind: descriptor::PORT,
                        disposition: arrives_as(disposition) as u8,
                        copy: 0,
                        count: 1,
                        at: first,
                    }
                }
                Descriptor::Ports {
                    address,
                    count,
                    disposition,
                    deallocate,
                } => {
                    if references.len() + count as usize > MAX_RIGHTS {
                        return failure(pending, MACH_SEND_TOO_LARGE);
                    }
                    let mut names = vec![0u8; count as usize * size_of::<Name>()];
                    if local::read(address, &mut names).is_err() {
                        return failure(pending, MACH_SEND_INVALID_MEMORY);
                    }
                    for name in names.chunks_exact(size_of::<Name>()) {
                        match take(word(name, 0), disposition, &mut pending) {
                            Ok(reference) => references.push(reference),
                            Err(code) => return failure(pending, code),
                        }
                    }
                    if deallocate {
                        let size = names.len() as u64;
                        pending.push(Pending::Deallocate { address, size });
                    }
                    Record {
                        kind: descriptor::OOL_PORTS,
                        disposition: arrives_as(disposition) as u8,
                        copy: VIRTUAL_COPY,
                        count,
                        at: first,
                    }
                }
                Descriptor::Memory {
                    address,
                    size,
                    copy,
                    deallocate,
                    volatile,
                } => {
                    if copy != PHYSICAL_COPY && copy != VIRTUAL_COPY {
                        return failure(pending, MACH_SEND_INVALID_TYPE);
                    }
                    let size_bytes = u64::from(size);
                    regions.push((address, size_bytes));
                    if deallocate {
                        pending.push(Pending::Deallocate {
                            address,
                            size: size_bytes,
                        });
                    }
                    let record = Record {
                        kind: match volatile {
                            true => descriptor::OOL_VOLATILE,
                            false => descriptor::OOL,
                        },
                        disposition: 0,
                        copy,
                        count: size,
                        at: memory_len,
                    };
                    memory_len += size_bytes;
                    record
                }
            };
            records.push(record);
        }
        payload_at = at;
    }

    // Held until the send is done, so the receiver gets its own descriptor for
    // the object before this one is closed.
    let memory = match ool::gather(&regions) {
        Ok(memory) => memory,
        Err(code) => return failure(pending, code),
    };
    let memory_at = memory.as_ref().map(|object| {
        fds.push(object.as_raw_fd());
        (fds.len() - 1, memory_len)
    });
    let carried = Carried {
        reply,
        records,
        references,
        memory: memory_at,
    };
    let wire_bytes = encode(bits, id, &carried, &bytes[payload_at..]);
    if wire_bytes.len() > wire::MAX_MESSAGE {
        return failure(pending, MACH_SEND_TOO_LARGE);
    }

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

/// Say once that a descriptor of `kind` was refused, since a guest that sends
/// one is left with nothing but `MACH_SEND_INVALID_TYPE` to go on.
fn announce_unsupported(kind: u8) {
    /// `MACH_MSG_GUARDED_PORT_DESCRIPTOR`.
    const GUARDED_PORT: u8 = 4;
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        match kind {
            GUARDED_PORT => eprintln!(
                "rosemary: mach_msg refused a guarded port descriptor, which is not supported"
            ),
            _ => eprintln!("rosemary: mach_msg refused a descriptor of unknown type {kind}"),
        }
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
    let deadline = wire::Deadline::after(timeout_ms);
    loop {
        let Some(index) = wire::poll_any(&fds, deadline.remaining_ms()) else {
            return Err(MACH_RCV_TIMED_OUT);
        };
        let (name, port) = &ports[index];
        match port.receive(0) {
            Ok(Some(message)) => return Ok((message, *name)),
            // Another thread receiving on the same set took it first, or it
            // was refused; either way the wait goes on.
            Ok(None) => continue,
            Err(_) => return Err(MACH_RCV_INVALID_NAME),
        }
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
    option: i32,
) -> i32 {
    let large = option & RCV_LARGE != 0;
    let elements = ((option >> TRAILER_ELEMENTS_SHIFT) & TRAILER_ELEMENTS_MASK) as usize;
    let trailer_size = trailer::SIZES[elements.min(trailer::SIZES.len() - 1)];
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
    let sender_bits = word(bytes, envelope::BITS_AT);
    let id = word(bytes, envelope::ID_AT) as i32;
    let count = word(bytes, envelope::RECORDS_AT) as usize;
    let reference_count = word(bytes, envelope::REFERENCES_AT) as usize;
    let payload_len = word(bytes, envelope::PAYLOAD_AT) as usize;
    let memory_fd = word(bytes, envelope::MEMORY_FD_AT) as i32;
    let memory_len = u64::from_le_bytes(
        bytes[envelope::MEMORY_LEN_AT..envelope::MEMORY_LEN_AT + 8]
            .try_into()
            .unwrap(),
    );
    let references_at = envelope::FIXED + count * RECORD;
    let payload_at = references_at + reference_count * REFERENCE;
    if reference_count > MAX_RIGHTS || bytes.len() < payload_at + payload_len {
        return MACH_RCV_BODY_ERROR;
    }
    let records: Vec<Record> = (0..count)
        .filter_map(|i| Record::decode(&bytes[envelope::FIXED + i * RECORD..]))
        .collect();
    if !records
        .iter()
        .all(|record| record.fits(reference_count, memory_len))
    {
        return MACH_RCV_BODY_ERROR;
    }

    let complex = count > 0 || sender_bits & BITS_COMPLEX != 0;
    let body = match complex {
        true => BODY + records.iter().map(Record::received_len).sum::<usize>(),
        false => 0,
    };
    let size = HEADER + body + payload_len;
    if size + trailer_size > rcv_size as usize {
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

    // A memory object that does not hold what its sender said makes the whole
    // message unreadable, so it is refused before any right in it is adopted.
    let memory = match memory_len {
        0 => None,
        len => {
            let object = usize::try_from(memory_fd)
                .ok()
                .and_then(|index| message.take_fd(index))
                // SAFETY: a descriptor that arrived over `SCM_RIGHTS`, now
                // owned by this process.
                .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) });
            match object.and_then(|object| ool::Arrived::map(object, len)) {
                Some(arrived) => Some(arrived),
                None => return MACH_RCV_BODY_ERROR,
            }
        }
    };

    // Every right in the message enters this task's space before any of it is
    // written out, so a failure part-way cannot leave the caller holding names
    // for rights it was never told about.
    let reply = Reference::decode(&message.bytes[envelope::REPLY_AT..]).unwrap_or(Reference::NONE);
    let reply_name = reply.adopt(&mut message);
    let names: Vec<Name> = (0..reference_count)
        .map(|i| {
            let at = references_at + i * REFERENCE;
            let reference = Reference::decode(&message.bytes[at..]).unwrap_or(Reference::NONE);
            reference.adopt(&mut message)
        })
        .collect();

    // Out-of-line memory that cannot be placed arrives as nothing, and the
    // receive says why, as `ipc_kmsg_copyout_body` does.
    let mut unplaced = 0;
    let mut place = |bytes: &[u8]| {
        ool::place(bytes).unwrap_or_else(|code| {
            unplaced |= match code {
                KERN_RESOURCE_SHORTAGE => MACH_MSG_VM_KERNEL,
                _ => MACH_MSG_VM_SPACE,
            };
            0
        })
    };
    let mut descriptors = Vec::with_capacity(body);
    if complex {
        descriptors.extend_from_slice(&(records.len() as u32).to_le_bytes());
    }
    for record in &records {
        let at = record.at as usize;
        let disposition = u32::from(record.disposition);
        let received = match record.kind {
            descriptor::PORT => Descriptor::Port {
                name: names[at],
                disposition,
            },
            descriptor::OOL_PORTS => {
                let carried = &names[at..at + record.count as usize];
                let array: Vec<u8> = carried.iter().flat_map(|name| name.to_le_bytes()).collect();
                let address = place(&array);
                if address == 0 {
                    for name in carried {
                        destroy_port(*name, disposition);
                    }
                }
                Descriptor::Ports {
                    address,
                    count: record.count,
                    disposition,
                    deallocate: true,
                }
            }
            kind => {
                let size = u64::from(record.count);
                let bytes = match &memory {
                    Some(memory) => memory.bytes(record.at, size),
                    None => &[],
                };
                let address = place(bytes);
                Descriptor::Memory {
                    address,
                    size: match address {
                        0 => 0,
                        _ => record.count,
                    },
                    copy: record.copy,
                    deallocate: record.copy == VIRTUAL_COPY,
                    volatile: kind == descriptor::OOL_VOLATILE,
                }
            }
        };
        received.write(&mut descriptors);
    }
    drop(memory);

    let mut bits =
        (reply.disposition as u32 & BITS_PORTS_MASK) | (MOVE_RECEIVE << BITS_LOCAL_SHIFT);
    if complex {
        bits |= BITS_COMPLEX;
    }

    // SAFETY: `size + trailer_size` bytes of the caller's buffer, checked above.
    unsafe {
        write_header(msg, bits, size as u32, reply_name, arrived_on, id);
        core::ptr::copy_nonoverlapping(descriptors.as_ptr(), msg.add(HEADER), descriptors.len());
        core::ptr::copy_nonoverlapping(
            message.bytes.as_ptr().add(payload_at),
            msg.add(HEADER + body),
            payload_len,
        );
        // The trailer, which `msgh_size` does not count and `rcv_size` must.
        write_word(msg, size, TRAILER_FORMAT_0);
        write_word(msg, size + 4, trailer_size as u32);
        // Zeroed first: the elements past the audit token are labels and
        // contexts this transport never has, and Darwin leaves them zero too.
        core::ptr::write_bytes(msg.add(size + TRAILER), 0, trailer_size - TRAILER);
        if elements as u32 >= trailer::SEQNO {
            // FIXME (ACCURACY): ports keep no sequence numbers, so it reads zero.
            write_word(msg, size + trailer::SEQNO_AT, 0);
        }
        if elements as u32 >= trailer::SENDER {
            let audit = audit_token(message.sender);
            // `security_token_t` is the sender's euid and egid.
            write_word(msg, size + trailer::SENDER_AT, audit[1]);
            write_word(msg, size + trailer::SENDER_AT + 4, audit[2]);
            if elements as u32 >= trailer::AUDIT {
                for (i, word) in audit.iter().enumerate() {
                    write_word(msg, size + trailer::AUDIT_AT + i * 4, *word);
                }
            }
        }
    }
    match unplaced {
        0 => MACH_MSG_SUCCESS,
        why => MACH_RCV_BODY_ERROR | why,
    }
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

/// Where `msgh_size` and `msgh_local_port` sit in a `mach_msg_header_t`.
const SIZE_AT: usize = 4;
const LOCAL_PORT_AT: usize = 12;

/// `mach_msg_return_t mach_msg_send(mach_msg_header_t *)`: [`mach_msg`]'s send
/// half, of the size the header states, with no timeout.
///
/// # Safety
///
/// `msg` addresses a whole message of the size its header states.
pub unsafe extern "C" fn mach_msg_send(msg: *mut c_void) -> i32 {
    if msg.is_null() {
        return MACH_SEND_INVALID_DATA;
    }
    // SAFETY: the caller's header.
    let size = unsafe { msg.cast::<u8>().add(SIZE_AT).cast::<u32>().read_unaligned() };
    // SAFETY: the caller's obligation.
    unsafe { mach_msg(msg, SEND_MSG, size, 0, MACH_PORT_NULL, 0, MACH_PORT_NULL) }
}

/// `mach_msg_return_t mach_msg_receive(mach_msg_header_t *)`: [`mach_msg`]'s
/// receive half, into a buffer of the header's `msgh_size` on its
/// `msgh_local_port`, with no timeout.
///
/// # Safety
///
/// `msg` addresses a writable buffer of the size its header states.
pub unsafe extern "C" fn mach_msg_receive(msg: *mut c_void) -> i32 {
    if msg.is_null() {
        return MACH_RCV_INVALID_DATA;
    }
    let header = msg.cast::<u8>();
    // SAFETY: the caller's header.
    let (size, local) = unsafe {
        (
            header.add(SIZE_AT).cast::<u32>().read_unaligned(),
            header.add(LOCAL_PORT_AT).cast::<Name>().read_unaligned(),
        )
    };
    // SAFETY: the caller's obligation.
    unsafe { mach_msg(msg, RCV_MSG, 0, size, local, 0, MACH_PORT_NULL) }
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
        return unsafe { receive(msg, rcv_size, rcv_name, timeout_ms, option) };
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
                option,
            )
        };
    }
    MACH_MSG_SUCCESS
}

/// `void mach_msg_destroy(mach_msg_header_t *)`
///
/// Releases every right and every out-of-line region in a message the caller
/// has decided not to act on. The rights are already in this task's space by
/// the time a receiver sees them, so skipping this leaks a name per message -
/// which is what turns a server loop that discards unknown messages into one
/// that runs out of names.
///
/// # Safety
///
/// `msg` addresses a `mach_msg_header_t` and the body it describes.
pub unsafe extern "C" fn mach_msg_destroy(msg: *mut c_void) {
    if msg.is_null() {
        return;
    }
    let msg = msg.cast::<u8>();
    // SAFETY: a message whose header states its own size.
    let (bits, size) = unsafe {
        (
            msg.cast::<u32>().read_unaligned(),
            msg.add(4).cast::<u32>().read_unaligned() as usize,
        )
    };
    // SAFETY: as above, and the size is the message's own.
    let bytes = unsafe { read_message(msg, size.clamp(HEADER, wire::MAX_MESSAGE)) };

    destroy_port(word(&bytes, 8), bits & BITS_PORTS_MASK);
    if bits & BITS_COMPLEX == 0 || bytes.len() < HEADER + BODY {
        return;
    }
    let count = word(&bytes, HEADER);
    let mut at = HEADER + BODY;
    for _ in 0..count {
        let Ok((descriptor, len)) = Descriptor::parse(&bytes, at) else {
            return;
        };
        at += len;
        match descriptor {
            Descriptor::Port { name, disposition } => destroy_port(name, disposition),
            // Volatile memory is left alone, as libsyscall's own leaves it.
            Descriptor::Memory {
                address,
                size,
                deallocate: true,
                volatile: false,
                ..
            } => {
                // SAFETY: memory the message said was the receiver's to free.
                unsafe { super::vm::vm_deallocate(space::TASK_SELF, address, u64::from(size)) };
            }
            Descriptor::Memory { .. } => {}
            Descriptor::Ports {
                address,
                count,
                disposition,
                deallocate,
            } => {
                let step = size_of::<Name>() as u64;
                for i in 0..u64::from(count) {
                    let mut name = [0u8; size_of::<Name>()];
                    if local::read(address + i * step, &mut name).is_err() {
                        break;
                    }
                    destroy_port(Name::from_le_bytes(name), disposition);
                }
                if deallocate {
                    let size = u64::from(count) * step;
                    // SAFETY: the array the message said was the receiver's to free.
                    unsafe { super::vm::vm_deallocate(space::TASK_SELF, address, size) };
                }
            }
        }
    }
}

/// Release the right a message holds in `name` under `disposition`, as
/// libsyscall's `mach_msg_destroy_port` does.
///
/// FIXME (ACCURACY): Darwin makes a `MAKE_SEND` or `MAKE_SEND_ONCE` right only
/// to destroy it, which a make-send count or a send-once notification can
/// observe; here neither is made.
fn destroy_port(name: Name, disposition: u32) {
    if name == MACH_PORT_NULL || name == MACH_PORT_DEAD {
        return;
    }
    match disposition {
        MOVE_SEND | MOVE_SEND_ONCE => {
            super::space::mach_port_deallocate(space::TASK_SELF, name);
        }
        // A receive right that arrived is the port itself, so it is destroyed.
        MOVE_RECEIVE => {
            space::destroy(name);
        }
        _ => {}
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

    /// `MACH_RCV_TRAILER_ELEMENTS(MACH_RCV_TRAILER_AUDIT)` asks for the
    /// sender's audit token, which is how a MIG or XPC server learns who
    /// called it.
    #[test]
    fn an_audit_trailer_names_the_sender() {
        const AUDIT: i32 = (trailer::AUDIT as i32) << TRAILER_ELEMENTS_SHIFT;
        let port = allocate_receive();
        let mut sent = message(0, port, MACH_PORT_NULL, 0x1234, b"four");
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
        // SAFETY: 256 writable bytes for the message.
        let rc = unsafe {
            mach_msg(
                buf.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT | AUDIT,
                0,
                buf.len() as u32,
                port,
                1000,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_MSG_SUCCESS);
        let size = word(&buf, 4) as usize;
        assert_eq!(
            word(&buf, size + 4) as usize,
            trailer::SIZES[trailer::AUDIT as usize]
        );
        let pid = word(&buf, size + trailer::AUDIT_AT + 5 * 4) as i32;
        // Only Linux's kernel says who sent a datagram.
        if cfg!(target_os = "linux") {
            assert_eq!(pid, crate::process::getpid(), "the audit token's pid");
            let euid = word(&buf, size + trailer::AUDIT_AT + 4);
            assert_eq!(
                euid,
                crate::credentials::current().euid,
                "the audit token's euid"
            );
            assert_eq!(
                word(&buf, size + trailer::SENDER_AT),
                euid,
                "the security token"
            );
        }

        // A buffer with room for a plain trailer only is not enough for it.
        let mut sent = message(0, port, MACH_PORT_NULL, 0x1234, b"four");
        // SAFETY: as above.
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
        let mut small = vec![0u8; HEADER + 4 + TRAILER];
        // SAFETY: the buffer's own length.
        let rc = unsafe {
            mach_msg(
                small.as_mut_ptr().cast(),
                RCV_MSG | RCV_TIMEOUT | AUDIT,
                0,
                small.len() as u32,
                port,
                1000,
                MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, MACH_RCV_TOO_LARGE);

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
        descriptor.extend_from_slice(&[0, 0, MOVE_RECEIVE as u8, descriptor::PORT]);
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
            buf[HEADER + BODY + descriptor::DISPOSITION_AT] as u32,
            MOVE_RECEIVE
        );
        assert_eq!(space::address_of(arrived), Some(address));
        assert!(
            space::receiver(arrived).is_some(),
            "and it is a receive right"
        );
        assert_eq!(
            word(&buf, HEADER + BODY + descriptor::PORT_LEN),
            0xdead_beef,
            "the payload follows the descriptors"
        );

        space::destroy(service);
        space::destroy(arrived);
    }

    /// An out-of-line descriptor: 16 bytes, the type at offset 11 as in a port
    /// descriptor.
    fn out_of_line(kind: u8, address: u64, size: u32, flags: [u8; 3]) -> Vec<u8> {
        let mut bytes = address.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[flags[0], flags[1], flags[2], kind]);
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes
    }

    fn send_complex(port: Name, descriptors: &[Vec<u8>], payload: &[u8]) -> i32 {
        let mut body = (descriptors.len() as u32).to_le_bytes().to_vec();
        for descriptor in descriptors {
            body.extend_from_slice(descriptor);
        }
        body.extend_from_slice(payload);
        let mut sent = message(BITS_COMPLEX, port, MACH_PORT_NULL, 3, &body);
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
        }
    }

    fn quad(bytes: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
    }

    /// The receiver's copy is its own: fresh pages, not the sender's address,
    /// and still there after the sender has changed its region.
    #[test]
    fn out_of_line_memory_arrives_as_a_copy_in_the_receivers_own_pages() {
        let port = allocate_receive();
        let mut region: Vec<u8> = (0..20_000u32).map(|i| (i * 7) as u8).collect();
        let expected = region.clone();
        let descriptor = out_of_line(
            descriptor::OOL,
            region.as_ptr() as u64,
            region.len() as u32,
            [0, VIRTUAL_COPY, 0],
        );
        assert_eq!(send_complex(port, &[descriptor], b"tail"), MACH_MSG_SUCCESS);
        region.fill(0);

        let arrived = take(port, 1000).expect("the message");
        let at = HEADER + BODY;
        assert_eq!(word(&arrived, HEADER), 1);
        assert_eq!(arrived[at + descriptor::TYPE_AT], descriptor::OOL);
        assert_eq!(arrived[at + descriptor::COPY_AT], VIRTUAL_COPY);
        assert_eq!(
            arrived[at + descriptor::DEALLOCATE_AT],
            1,
            "a virtual copy is the receiver's to deallocate"
        );
        assert_eq!(word(&arrived, at + descriptor::SIZE_AT), 20_000);
        assert_eq!(
            &arrived[at + descriptor::OOL_LEN..at + descriptor::OOL_LEN + 4],
            b"tail"
        );
        let address = quad(&arrived, at);
        assert_ne!(address, region.as_ptr() as u64);
        // SAFETY: the 20000 bytes the receive placed at `address`.
        let copy = unsafe { std::slice::from_raw_parts(address as *const u8, 20_000) };
        assert_eq!(copy, &expected[..]);

        // SAFETY: the region the message handed over.
        let rc = unsafe { super::super::vm::vm_deallocate(space::TASK_SELF, address, 20_000) };
        assert_eq!(rc, KERN_SUCCESS);
        space::destroy(port);
    }

    #[test]
    fn an_empty_out_of_line_region_arrives_with_no_address() {
        let port = allocate_receive();
        let descriptor = out_of_line(descriptor::OOL, 0x1000, 0, [0, PHYSICAL_COPY, 0]);
        assert_eq!(send_complex(port, &[descriptor], &[]), MACH_MSG_SUCCESS);
        let arrived = take(port, 1000).expect("the message");
        assert_eq!(quad(&arrived, HEADER + BODY), 0);
        assert_eq!(word(&arrived, HEADER + BODY + descriptor::SIZE_AT), 0);
        space::destroy(port);
    }

    #[test]
    fn a_sender_that_deallocates_out_of_line_memory_gives_its_pages_up() {
        let port = allocate_receive();
        let mut address = 0u64;
        let page = super::super::info::page_size();
        // SAFETY: a live local for the out-parameter.
        let rc = unsafe { super::super::vm::vm_allocate(space::TASK_SELF, &mut address, page, 1) };
        assert_eq!(rc, KERN_SUCCESS);
        // SAFETY: the page just allocated.
        unsafe { (address as *mut u8).write(0x5a) };
        let descriptor = out_of_line(descriptor::OOL, address, 1, [1, VIRTUAL_COPY, 0]);
        assert_eq!(send_complex(port, &[descriptor], &[]), MACH_MSG_SUCCESS);
        assert!(
            !starnix::memory::mapped(address as usize),
            "the sender's page is gone"
        );
        let arrived = take(port, 1000).expect("the message");
        // SAFETY: the byte the receive placed there.
        assert_eq!(
            unsafe { *(quad(&arrived, HEADER + BODY) as *const u8) },
            0x5a
        );
        space::destroy(port);
    }

    #[test]
    fn unreadable_out_of_line_memory_is_refused_and_the_sender_keeps_its_rights() {
        let port = allocate_receive();
        let target = allocate_receive();
        let held = space::insert_send(space::address_of(target).unwrap());
        let mut moved = held.to_le_bytes().to_vec();
        moved.extend_from_slice(&[0; 6]);
        moved.extend_from_slice(&[MOVE_SEND as u8, descriptor::PORT]);
        let unreadable = out_of_line(descriptor::OOL, 8, 16, [0, PHYSICAL_COPY, 0]);
        assert_eq!(
            send_complex(port, &[moved, unreadable], &[]),
            MACH_SEND_INVALID_MEMORY
        );
        assert_eq!(
            space::address_of(held),
            space::address_of(target),
            "the moved right is still held"
        );
        assert!(take(port, 100).is_none(), "and nothing was sent");
        space::mach_port_deallocate(space::TASK_SELF, held);
        space::destroy(port);
        space::destroy(target);
    }

    /// `MACH_MSG_ALLOCATE` and the rest describe a receive, not a send.
    #[test]
    fn a_copy_option_other_than_physical_or_virtual_is_refused() {
        /// `MACH_MSG_ALLOCATE`.
        const ALLOCATE: u8 = 2;
        let port = allocate_receive();
        let byte = 0u8;
        let descriptor = out_of_line(descriptor::OOL, &raw const byte as u64, 1, [0, ALLOCATE, 0]);
        assert_eq!(
            send_complex(port, &[descriptor], &[]),
            MACH_SEND_INVALID_TYPE
        );
        space::destroy(port);
    }

    /// Each name in a port array crosses as its own right, and the array
    /// arrives in pages of the receiver's own.
    #[test]
    fn an_out_of_line_port_array_carries_each_right() {
        let port = allocate_receive();
        let first = allocate_receive();
        let second = allocate_receive();
        let names = [first, MACH_PORT_NULL, second];
        let descriptor = out_of_line(
            descriptor::OOL_PORTS,
            names.as_ptr() as u64,
            names.len() as u32,
            [0, PHYSICAL_COPY, MAKE_SEND as u8],
        );
        assert_eq!(send_complex(port, &[descriptor], &[]), MACH_MSG_SUCCESS);

        let arrived = take(port, 1000).expect("the message");
        let at = HEADER + BODY;
        assert_eq!(arrived[at + descriptor::TYPE_AT], descriptor::OOL_PORTS);
        assert_eq!(
            u32::from(arrived[at + descriptor::DISPOSITION_AT]),
            MOVE_SEND
        );
        assert_eq!(word(&arrived, at + descriptor::SIZE_AT), 3);
        let array = quad(&arrived, at);
        assert_ne!(array, names.as_ptr() as u64);
        // SAFETY: the three names the receive placed at `array`.
        let received = unsafe { std::slice::from_raw_parts(array as *const Name, 3) };
        assert_eq!(space::address_of(received[0]), space::address_of(first));
        assert_eq!(received[1], MACH_PORT_NULL);
        assert_eq!(space::address_of(received[2]), space::address_of(second));

        // SAFETY: the message just received, in its own buffer.
        unsafe { mach_msg_destroy(arrived.as_ptr().cast_mut().cast()) };
        assert!(
            !starnix::memory::mapped(array as usize),
            "destroying the message frees the array"
        );
        space::destroy(port);
        space::destroy(first);
        space::destroy(second);
    }

    /// The message's out-of-line memory and the descriptor it travels as are
    /// both released when the receiver throws the message away.
    #[test]
    fn destroying_a_received_message_frees_its_out_of_line_memory() {
        let port = allocate_receive();
        let region = [1u8; 64];
        let descriptor = out_of_line(
            descriptor::OOL,
            region.as_ptr() as u64,
            64,
            [0, VIRTUAL_COPY, 0],
        );
        assert_eq!(send_complex(port, &[descriptor], &[]), MACH_MSG_SUCCESS);
        let arrived = take(port, 1000).expect("the message");
        let address = quad(&arrived, HEADER + BODY);
        assert!(starnix::memory::mapped(address as usize));
        // SAFETY: the message just received, in its own buffer.
        unsafe { mach_msg_destroy(arrived.as_ptr().cast_mut().cast()) };
        assert!(!starnix::memory::mapped(address as usize));
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
        descriptor.extend_from_slice(&[0, 0, MOVE_SEND as u8, descriptor::PORT]);
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
        descriptor.extend_from_slice(&[0, 0, MOVE_RECEIVE as u8, descriptor::PORT]);
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
