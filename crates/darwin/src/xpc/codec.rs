//! An XPC value as the bytes of a Mach message.
//!
//! Both ends are this module, so the format is its own: a tag byte per value,
//! little-endian scalars, lengths before strings, data and collections. An
//! endpoint is a send right, which travels as a port descriptor; the payload
//! carries its index among the message's descriptors.

use std::ffi::CString;

use super::collection::{Array, Dictionary};
use super::connection::Endpoint;
use super::object::{self, Handle, Value, bool_object, new, release, retain};
use crate::mach::space::Name;

mod tag {
    pub const NULL: u8 = 0;
    pub const BOOL: u8 = 1;
    pub const INT64: u8 = 2;
    pub const UINT64: u8 = 3;
    pub const DOUBLE: u8 = 4;
    pub const DATE: u8 = 5;
    pub const DATA: u8 = 6;
    pub const STRING: u8 = 7;
    pub const UUID: u8 = 8;
    pub const ARRAY: u8 = 9;
    pub const DICTIONARY: u8 = 10;
    pub const ENDPOINT: u8 = 11;
}

/// How deep a received value may nest. A peer is trusted no further than
/// to be this module: a hostile one must not overflow the stack.
const MAX_DEPTH: usize = 64;

/// What cannot be sent: a connection or an error, as on Darwin.
#[derive(Debug, PartialEq, Eq)]
pub struct Unencodable;

/// `value`'s bytes, appending each endpoint's send right to `ports`.
pub fn encode(value: &Value, out: &mut Vec<u8>, ports: &mut Vec<Name>) -> Result<(), Unencodable> {
    match value {
        Value::Null => out.push(tag::NULL),
        Value::Bool(v) => {
            out.push(tag::BOOL);
            out.push(*v as u8);
        }
        Value::Int64(v) => {
            out.push(tag::INT64);
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::Uint64(v) => {
            out.push(tag::UINT64);
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::Double(v) => {
            out.push(tag::DOUBLE);
            out.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        Value::Date(v) => {
            out.push(tag::DATE);
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::Data(bytes) => {
            out.push(tag::DATA);
            bytes_with_length(bytes, out);
        }
        Value::String(string) => {
            out.push(tag::STRING);
            bytes_with_length(string.as_bytes(), out);
        }
        Value::Uuid(bytes) => {
            out.push(tag::UUID);
            out.extend_from_slice(bytes);
        }
        Value::Array(array) => {
            out.push(tag::ARRAY);
            let items = array.snapshot();
            let result = encode_handles(&items, out, ports);
            release_all(items);
            result?;
        }
        Value::Dictionary(dictionary) => {
            out.push(tag::DICTIONARY);
            let entries = dictionary.snapshot();
            out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
            let mut result = Ok(());
            for (key, value) in &entries {
                bytes_with_length(key.as_bytes(), out);
                // SAFETY: a value the snapshot holds a reference to.
                result = match unsafe { object::value(*value) } {
                    Some(value) => encode(value, out, ports),
                    None => Err(Unencodable),
                };
                if result.is_err() {
                    break;
                }
            }
            release_all(entries.into_iter().map(|(_, v)| v).collect());
            result?;
        }
        Value::Endpoint(endpoint) => {
            out.push(tag::ENDPOINT);
            out.extend_from_slice(&(ports.len() as u32).to_le_bytes());
            ports.push(endpoint.port());
        }
        Value::Error(_) | Value::Connection(_) => return Err(Unencodable),
    }
    Ok(())
}

fn encode_handles(
    items: &[Handle],
    out: &mut Vec<u8>,
    ports: &mut Vec<Name>,
) -> Result<(), Unencodable> {
    out.extend_from_slice(&(items.len() as u32).to_le_bytes());
    for item in items {
        // SAFETY: a value the caller holds a reference to.
        match unsafe { object::value(*item) } {
            Some(value) => encode(value, out, ports)?,
            None => return Err(Unencodable),
        }
    }
    Ok(())
}

fn release_all(handles: Vec<Handle>) {
    for handle in handles {
        // SAFETY: references the caller took for the encoding.
        unsafe { release(handle) };
    }
}

fn bytes_with_length(bytes: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// A malformed or truncated payload.
#[derive(Debug, PartialEq, Eq)]
pub struct Malformed;

/// The reader over a received payload. `ports` are the send rights that came
/// with it, by descriptor index; each is taken at most once, and those left
/// are the caller's to release.
pub struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    ports: &'a mut [Option<Name>],
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8], ports: &'a mut [Option<Name>]) -> Self {
        Reader {
            bytes,
            at: 0,
            ports,
        }
    }

    pub fn take(&mut self, count: usize) -> Result<&'a [u8], Malformed> {
        let end = self.at.checked_add(count).ok_or(Malformed)?;
        let taken = self.bytes.get(self.at..end).ok_or(Malformed)?;
        self.at = end;
        Ok(taken)
    }

    pub fn u32(&mut self) -> Result<u32, Malformed> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| Malformed)?,
        ))
    }

    pub fn u64(&mut self) -> Result<u64, Malformed> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| Malformed)?,
        ))
    }

    fn byte(&mut self) -> Result<u8, Malformed> {
        Ok(self.take(1)?[0])
    }

    fn sized(&mut self) -> Result<&'a [u8], Malformed> {
        let length = self.u32()? as usize;
        self.take(length)
    }

    /// The next value, as an object the caller owns.
    pub fn value(&mut self) -> Result<Handle, Malformed> {
        self.value_at(0)
    }

    /// The next value, which must be a dictionary, built around `dictionary`.
    pub fn dictionary_into(&mut self, dictionary: Dictionary) -> Result<Handle, Malformed> {
        if self.byte()? != tag::DICTIONARY {
            return Err(Malformed);
        }
        self.fill_dictionary(&dictionary, 0)?;
        Ok(new(Value::Dictionary(dictionary)))
    }

    fn fill_dictionary(&mut self, dictionary: &Dictionary, depth: usize) -> Result<(), Malformed> {
        let count = self.u32()?;
        for _ in 0..count {
            let key = CString::new(self.sized()?).map_err(|_| Malformed)?;
            let value = self.value_at(depth + 1)?;
            dictionary.set_owned(&key, value);
        }
        Ok(())
    }

    fn value_at(&mut self, depth: usize) -> Result<Handle, Malformed> {
        if depth > MAX_DEPTH {
            return Err(Malformed);
        }
        let value = match self.byte()? {
            tag::NULL => Value::Null,
            tag::BOOL => return Ok(retain(bool_object(self.byte()? != 0))),
            tag::INT64 => Value::Int64(self.u64()? as i64),
            tag::UINT64 => Value::Uint64(self.u64()?),
            tag::DOUBLE => Value::Double(f64::from_bits(self.u64()?)),
            tag::DATE => Value::Date(self.u64()? as i64),
            tag::DATA => Value::Data(self.sized()?.into()),
            tag::STRING => Value::String(CString::new(self.sized()?).map_err(|_| Malformed)?),
            tag::UUID => Value::Uuid(self.take(16)?.try_into().map_err(|_| Malformed)?),
            tag::ARRAY => {
                let array = Array::new();
                for _ in 0..self.u32()? {
                    array.push_owned(self.value_at(depth + 1)?);
                }
                Value::Array(array)
            }
            tag::DICTIONARY => {
                let dictionary = Dictionary::new();
                self.fill_dictionary(&dictionary, depth)?;
                Value::Dictionary(dictionary)
            }
            tag::ENDPOINT => {
                let index = self.u32()? as usize;
                let port = self.ports.get_mut(index).and_then(Option::take);
                Value::Endpoint(Endpoint::adopt(port.ok_or(Malformed)?))
            }
            _ => return Err(Malformed),
        };
        Ok(new(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xpc::collection;
    use crate::xpc::object::equal;

    fn round_trip(handle: Handle) -> Handle {
        let mut bytes = Vec::new();
        let mut ports = Vec::new();
        // SAFETY: a live object of this module's.
        let value = unsafe { object::value(handle) }.expect("an XPC object");
        encode(value, &mut bytes, &mut ports).expect("encodable");
        assert!(ports.is_empty());
        Reader::new(&bytes, &mut []).value().expect("decodes")
    }

    #[test]
    fn a_nested_dictionary_survives_the_wire() {
        // SAFETY: objects created and released here.
        unsafe {
            let inner = collection::xpc_array_create_empty();
            collection::xpc_array_set_int64(inner, usize::MAX, -7);
            collection::xpc_array_set_string(inner, usize::MAX, c"two".as_ptr());
            let message = collection::xpc_dictionary_create_empty();
            collection::xpc_dictionary_set_uint64(message, c"generation".as_ptr(), 1 << 40);
            collection::xpc_dictionary_set_data(
                message,
                c"blob".as_ptr(),
                b"\0\x01\x02".as_ptr(),
                3,
            );
            collection::xpc_dictionary_set_bool(message, c"flag".as_ptr(), true);
            collection::xpc_dictionary_set_value(message, c"list".as_ptr(), inner);
            release(inner);

            let copy = round_trip(message);
            assert!(equal(
                object::value(message).unwrap(),
                object::value(copy).unwrap()
            ));
            release(message);
            release(copy);
        }
    }

    #[test]
    fn a_truncated_payload_is_refused() {
        // SAFETY: as above.
        unsafe {
            let message = collection::xpc_dictionary_create_empty();
            collection::xpc_dictionary_set_string(message, c"key".as_ptr(), c"value".as_ptr());
            let mut bytes = Vec::new();
            encode(object::value(message).unwrap(), &mut bytes, &mut Vec::new()).unwrap();
            for cut in 0..bytes.len() {
                assert_eq!(Reader::new(&bytes[..cut], &mut []).value(), Err(Malformed));
            }
            release(message);
        }
    }

    #[test]
    fn nesting_past_the_limit_is_refused() {
        let mut bytes = vec![tag::ARRAY, 1, 0, 0, 0].repeat(MAX_DEPTH + 2);
        bytes.push(tag::NULL);
        assert_eq!(Reader::new(&bytes, &mut []).value(), Err(Malformed));
    }

    #[test]
    fn an_error_cannot_be_sent() {
        let error = crate::xpc::connection::error_interrupted();
        // SAFETY: a global object of this module's.
        let value = unsafe { object::value(error) }.unwrap();
        assert_eq!(
            encode(value, &mut Vec::new(), &mut Vec::new()),
            Err(Unencodable)
        );
    }
}
