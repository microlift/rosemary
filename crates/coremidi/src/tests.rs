use super::*;

/// A packet list in a buffer of `N` bytes, and its first packet.
struct List<const N: usize> {
    bytes: Box<[u8; N]>,
    current: *mut u8,
}

impl<const N: usize> List<N> {
    fn new() -> Self {
        let mut bytes = Box::new([0u8; N]);
        // SAFETY: a buffer large enough for the count.
        let current = unsafe { MIDIPacketListInit(bytes.as_mut_ptr()) };
        List { bytes, current }
    }

    /// Add `data` at `time`, answering the new packet's offset and length.
    fn add(&mut self, time: u64, data: &[u8]) -> Option<(usize, usize)> {
        let base = self.bytes.as_mut_ptr();
        // SAFETY: the list this owns and its current packet.
        let packet =
            unsafe { MIDIPacketListAdd(base, N, self.current, time, data.len(), data.as_ptr()) };
        if packet.is_null() {
            return None;
        }
        self.current = packet;
        // SAFETY: a packet in the list.
        Some((packet as usize - base as usize, unsafe {
            packet_length(packet)
        }))
    }

    fn packets(&self) -> u32 {
        u32::from_ne_bytes(self.bytes[..4].try_into().unwrap())
    }
}

const NOTE_ON: [u8; 3] = [0x90, 60, 100];
const NOTE_OFF: [u8; 3] = [0x80, 60, 0];
const SYSEX: [u8; 4] = [0xf0, 1, 2, 0xf7];

/// The sequence and offsets an arm64 Mac produces, rounding each packet to a
/// word.
#[test]
#[cfg(target_arch = "aarch64")]
fn merges_events_at_one_time_and_keeps_sysex_apart() {
    let mut list = List::<1024>::new();
    assert_eq!(list.add(5, &NOTE_ON), Some((4, 3)));
    assert_eq!(list.add(5, &NOTE_OFF), Some((4, 6)));
    assert_eq!(list.add(5, &SYSEX), Some((20, 4)));
    assert_eq!(list.add(5, &NOTE_ON), Some((36, 3)));
    assert_eq!(list.add(6, &NOTE_ON), Some((52, 3)));
    // A running-status data byte joins the packet at its time.
    assert_eq!(list.add(6, &[60]), Some((52, 4)));
    assert_eq!(list.packets(), 4);
}

/// Merging is bounded by the list, not by a packet's nominal 256 bytes.
#[test]
fn merges_past_the_nominal_packet_size() {
    let mut list = List::<1024>::new();
    let events = [0x90u8; 200];
    list.add(7, &events).expect("first");
    assert_eq!(list.add(7, &events[..100]), Some((4, 300)));
    assert_eq!(list.packets(), 1);
}

#[test]
fn refuses_an_event_the_list_has_no_room_for() {
    let mut fits = List::<30>::new();
    assert_eq!(fits.add(1, &[0x90; 16]), Some((4, 16)));
    let mut over = List::<30>::new();
    assert_eq!(over.add(1, &[0x90; 17]), None);
    assert_eq!(over.packets(), 0);
}

#[test]
fn a_port_belongs_to_its_client() {
    corefoundation::bootstrap();
    let name = corefoundation::string::from_bytes(b"client", false);
    let (mut client, mut port) = (0, 0);
    // SAFETY: a string this test made and writable out-parameters.
    unsafe {
        assert_eq!(
            MIDIClientCreate(name, std::ptr::null(), std::ptr::null_mut(), &mut client),
            NO_ERR
        );
        assert_eq!(MIDIOutputPortCreate(client, name, &mut port), NO_ERR);
        assert_eq!(
            MIDIOutputPortCreate(client + 1000, name, &mut port),
            PARAM_ERR
        );
    }
    let property = corefoundation::string::from_bytes(PROPERTY_NAME.as_bytes(), false);
    let mut text = std::ptr::null();
    // SAFETY: a port and property this test made, and a writable out-parameter.
    assert_eq!(
        unsafe { MIDIObjectGetStringProperty(port, property, &mut text) },
        NO_ERR
    );
    // SAFETY: the string just returned.
    assert_eq!(unsafe { name_of(text) }.as_deref(), Some("client"));
    let mut value = 7;
    // SAFETY: as above.
    assert_eq!(
        unsafe { MIDIObjectGetIntegerProperty(port, property, &mut value) },
        UNKNOWN_PROPERTY
    );
    assert_eq!(value, 0);
    assert_eq!(MIDIClientDispose(client), NO_ERR);
    assert_eq!(MIDIPortDispose(port), PARAM_ERR);
    assert_eq!(MIDIClientDispose(client), PARAM_ERR);
    // SAFETY: three strings this test owns.
    unsafe {
        corefoundation::api::CFRelease(text);
        corefoundation::api::CFRelease(property);
        corefoundation::api::CFRelease(name);
    }
}
