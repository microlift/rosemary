//! The byte encoding of everything that crosses the socket.
//!
//! Hand-written rather than derived: both ends are this crate at the same
//! version (the handshake refuses anything else), so there is no schema to
//! evolve, and a dependency for it would be the only one this crate has.

use std::io::{self, Read, Write};

/// A frame larger than this is a corrupt length, not a message. The largest
/// real one is a process list, a few hundred bytes per process.
pub const MAX_FRAME: usize = 16 << 20;

/// A message that ended early or held a value its type cannot have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed;

impl From<Malformed> for io::Error {
    fn from(_: Malformed) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, "malformed server message")
    }
}

pub struct Encoder {
    pub bytes: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    pub fn put<T: Wire>(&mut self, value: &T) -> &mut Self {
        value.put(self);
        self
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Decoder<'a> {
    bytes: &'a [u8],
}

impl<'a> Decoder<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub fn take<T: Wire>(&mut self) -> Result<T, Malformed> {
        T::take(self)
    }

    fn raw(&mut self, len: usize) -> Result<&'a [u8], Malformed> {
        if self.bytes.len() < len {
            return Err(Malformed);
        }
        let (head, rest) = self.bytes.split_at(len);
        self.bytes = rest;
        Ok(head)
    }

    /// A length-prefixed byte string, as `Vec<u8>` encodes one, borrowed from
    /// the message rather than copied out of it.
    pub fn bytes(&mut self) -> Result<&'a [u8], Malformed> {
        let len = take_len(self)?;
        self.raw(len)
    }

    /// Whether every byte was consumed. A message with bytes left over was
    /// encoded from a different shape than it is being decoded into.
    pub fn finished(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// A value with a byte encoding.
pub trait Wire: Sized {
    fn put(&self, out: &mut Encoder);
    fn take(input: &mut Decoder<'_>) -> Result<Self, Malformed>;
}

macro_rules! number {
    ($($ty:ty),*) => {$(
        impl Wire for $ty {
            fn put(&self, out: &mut Encoder) {
                out.bytes.extend_from_slice(&self.to_le_bytes());
            }
            fn take(input: &mut Decoder<'_>) -> Result<Self, Malformed> {
                let raw = input.raw(size_of::<$ty>())?;
                Ok(<$ty>::from_le_bytes(raw.try_into().map_err(|_| Malformed)?))
            }
        }
    )*};
}

number!(u8, u16, u32, u64, i8, i16, i32, i64, f32);

impl Wire for bool {
    fn put(&self, out: &mut Encoder) {
        out.bytes.push(u8::from(*self));
    }
    fn take(input: &mut Decoder<'_>) -> Result<Self, Malformed> {
        match input.raw(1)?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Malformed),
        }
    }
}

fn put_len(out: &mut Encoder, len: usize) {
    (len as u32).put(out);
}

fn take_len(input: &mut Decoder<'_>) -> Result<usize, Malformed> {
    let len = u32::take(input)? as usize;
    // Every element is at least a byte, so a count beyond what is left is a
    // lie, and believing it would be a huge allocation.
    if len > input.bytes.len() {
        return Err(Malformed);
    }
    Ok(len)
}

impl Wire for Vec<u8> {
    fn put(&self, out: &mut Encoder) {
        put_len(out, self.len());
        out.bytes.extend_from_slice(self);
    }
    fn take(input: &mut Decoder<'_>) -> Result<Self, Malformed> {
        let len = take_len(input)?;
        Ok(input.raw(len)?.to_vec())
    }
}

impl Wire for String {
    fn put(&self, out: &mut Encoder) {
        put_len(out, self.len());
        out.bytes.extend_from_slice(self.as_bytes());
    }
    fn take(input: &mut Decoder<'_>) -> Result<Self, Malformed> {
        let len = take_len(input)?;
        String::from_utf8(input.raw(len)?.to_vec()).map_err(|_| Malformed)
    }
}

impl<T: Wire> Wire for Option<T> {
    fn put(&self, out: &mut Encoder) {
        match self {
            None => false.put(out),
            Some(value) => {
                true.put(out);
                value.put(out);
            }
        }
    }
    fn take(input: &mut Decoder<'_>) -> Result<Self, Malformed> {
        match bool::take(input)? {
            false => Ok(None),
            true => Ok(Some(T::take(input)?)),
        }
    }
}

/// A list of anything but bytes, which have their own, flat, encoding above.
pub struct List<T>(pub Vec<T>);

impl<T: Wire> Wire for List<T> {
    fn put(&self, out: &mut Encoder) {
        put_len(out, self.0.len());
        for item in &self.0 {
            item.put(out);
        }
    }
    fn take(input: &mut Decoder<'_>) -> Result<Self, Malformed> {
        let len = take_len(input)?;
        let mut items = Vec::with_capacity(len);
        for _ in 0..len {
            items.push(T::take(input)?);
        }
        Ok(List(items))
    }
}

/// `Wire` for a struct, field by field in declaration order.
#[macro_export]
macro_rules! wire_struct {
    ($name:ident { $($field:ident),* $(,)? }) => {
        impl $crate::codec::Wire for $name {
            fn put(&self, out: &mut $crate::codec::Encoder) {
                $( $crate::codec::Wire::put(&self.$field, out); )*
            }
            fn take(
                input: &mut $crate::codec::Decoder<'_>,
            ) -> Result<Self, $crate::codec::Malformed> {
                Ok(Self { $( $field: input.take()?, )* })
            }
        }
    };
}

/// Encode `value` as a whole message.
pub fn encode<T: Wire>(value: &T) -> Vec<u8> {
    let mut out = Encoder::new();
    value.put(&mut out);
    out.bytes
}

/// Decode a whole message, refusing one with bytes left over.
pub fn decode<T: Wire>(bytes: &[u8]) -> Result<T, Malformed> {
    let mut input = Decoder::new(bytes);
    let value = T::take(&mut input)?;
    match input.finished() {
        true => Ok(value),
        false => Err(Malformed),
    }
}

/// Write one length-prefixed frame.
pub fn write_frame(stream: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "server message too large",
        ));
    }
    // One write, so a frame is never interleaved with another thread's even
    // if a caller forgets the lock.
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    stream.write_all(&frame)
}

/// Read one frame, or `None` at a clean end of stream.
pub fn read_frame(stream: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match stream.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(Malformed.into());
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    Ok(Some(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sample {
        a: u32,
        b: String,
        c: Option<i64>,
        d: List<Vec<u8>>,
    }
    wire_struct!(Sample { a, b, c, d });

    #[test]
    fn a_struct_round_trips() {
        let bytes = encode(&Sample {
            a: 7,
            b: "seven".into(),
            c: Some(-7),
            d: List(vec![b"x".to_vec(), Vec::new()]),
        });
        let back: Sample = decode(&bytes).unwrap();
        assert_eq!(back.a, 7);
        assert_eq!(back.b, "seven");
        assert_eq!(back.c, Some(-7));
        assert_eq!(back.d.0, vec![b"x".to_vec(), Vec::new()]);
    }

    #[test]
    fn a_truncated_or_padded_message_is_refused() {
        let bytes = encode(&(5u32));
        assert_eq!(decode::<u64>(&bytes).err(), Some(Malformed));
        let mut long = bytes.clone();
        long.push(0);
        assert_eq!(decode::<u32>(&long).err(), Some(Malformed));
    }

    #[test]
    fn a_length_beyond_the_message_is_refused_before_allocating() {
        let bytes = encode(&u32::MAX);
        assert_eq!(decode::<Vec<u8>>(&bytes).err(), Some(Malformed));
    }

    #[test]
    fn frames_round_trip_and_end_cleanly() {
        let mut wire = Vec::new();
        write_frame(&mut wire, b"one").unwrap();
        write_frame(&mut wire, b"").unwrap();
        let mut reader = wire.as_slice();
        assert_eq!(read_frame(&mut reader).unwrap().unwrap(), b"one");
        assert_eq!(read_frame(&mut reader).unwrap().unwrap(), b"");
        assert!(read_frame(&mut reader).unwrap().is_none());
    }
}
