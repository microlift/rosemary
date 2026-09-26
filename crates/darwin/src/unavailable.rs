//! Darwin subsystems this host does not have, each answered with the truth.
//!
//! # Why this file exists
//!
//! Every name here is a **feature probe** in the guest: Chromium asks a
//! subsystem whether it exists, and branches on the answer. That makes them the
//! cheapest kind of import to retire, and retiring them matters mechanically -
//! an unbound import is a jump through a zero slot, which is how whole bring-up
//! runs have been lost. A bound function that answers "no" is a bound function.
//!
//! The rule this file follows, and the reason it is one file rather than five:
//!
//! * **A value that is true is implemented, not refused.** `MIDIGetNumberOfSources`
//!   returns `0` because this host has no MIDI sources. That is not a stub; it
//!   is the answer, and the guest's loop over the count then does the right
//!   thing without ever reaching the code that would need a real one.
//! * **Where there is no answer, the honest code is returned in the family's own
//!   vocabulary** - `kMIDIServerStartErr` for CoreMIDI, `kDAReturnNotFound` for
//!   DiskArbitration, `IPP_STATUS_ERROR_OPERATION_NOT_SUPPORTED` for CUPS - never
//!   a plausible zero, and never an uninitialised pointer the guest will read.
//! * **Where the caller is documented to check, the check is what the answer is
//!   chosen for.** Every one of those checks was read out of the guest binary
//!   first; the notes below say which instruction.
//!
//! # The families, and what each one is
//!
//! | family | names | the honest answer |
//! |---|--:|---|
//! | `launch_data_*` and `launch_msg` | 9 | the data model is implemented; the *message* is refused, because there is no launchd |
//! | DiskArbitration | 15 | no IOKit disk objects exist here, so the lookup answers null and the callback chain is never entered |
//! | CoreMIDI | 14 | `kMIDIServerStartErr`, and zero endpoints |
//! | VideoToolbox | 2 | no hardware video decode, and nothing to register |
//! | CUPS | 65 | no print system: no server, no destinations, no PPDs |
//!
//! # What is *not* here, and why
//!
//! The rest of the VideoToolbox surface - the compression and decompression
//! *sessions* - is real video work rather than a probe, and is listed in
//! [`crate::divergent`] so that it at least refuses by name instead of
//! unbound. AppKit, Security, CoreFoundation, Foundation, IOKit and the Mach
//! and libSystem families belong to other owners.

#[cfg(not(target_os = "macos"))]
mod imp {
    //! The answers, and why each one is the true answer.
    //!
    //! Every signature below was taken from a real header and then checked
    //! against the call site in `Chromium Framework`; where the two disagree
    //! the binary wins, and the note says so.

    use std::ffi::{c_char, c_int, c_longlong, c_void, CStr, CString};

    // ---------------------------------------------------------------- launchd

    /// `launch_data_type_t`, from `<launch.h>`. The values are the enum's, and
    /// the guest confirms three of them: `Chromium Framework+0x5da1696` compares
    /// `launch_data_get_type`'s answer against 1, `+0x5da16a7` against 9, and
    /// `+0x5da18f8` against 4.
    ///
    /// The whole enum rather than the three the guest happens to use, because a
    /// partial one would be a list of constants that quietly disagrees with the
    /// header it claims to be from.
    #[allow(dead_code)]
    mod launch_data_type {
        use std::ffi::c_int;

        pub const DICTIONARY: c_int = 1;
        pub const ARRAY: c_int = 2;
        pub const FD: c_int = 3;
        pub const INTEGER: c_int = 4;
        pub const REAL: c_int = 5;
        pub const BOOL: c_int = 6;
        pub const STRING: c_int = 7;
        pub const OPAQUE: c_int = 8;
        pub const ERRNO: c_int = 9;
        pub const MACHPORT: c_int = 10;
    }

    /// Darwin's `ESRCH` (3), which is also what a Mach bootstrap lookup against a
    /// dead port returns - and a bootstrap lookup is what `launch_msg` is.
    ///
    /// [`launch_msg`] cannot report an error of its own: its only failure answer
    /// is a null. So the number is carried by the one kind of `launch_data_t` that
    /// exists to carry a number, and [`launch_data_alloc`] seeds an
    /// errno-valued node with it - there is exactly one reason a message can fail
    /// to be delivered here, and naming it is more use than leaving the node
    /// zeroed.
    const LAUNCH_ERRSCH: c_int = 3;

    /// One `launch_data_t`.
    ///
    /// The layout is this file's own, and that is not a guess about anything:
    /// a `launch_data_t` is opaque to the guest, which only ever hands one back
    /// to another `launch_*` call - and every one of those is answered here.
    /// The alternative, inventing launchd's private `struct _launch_data`, would
    /// be a guess that nothing could check.
    ///
    /// `dict` owns its values rather than pointing at them, which is what makes
    /// [`launch_data_free`] correct on a whole tree and makes a second free of a
    /// value the dictionary already owns impossible to get right by accident.
    struct Data {
        kind: c_int,
        integer: c_longlong,
        number: c_int,
        text: CString,
        dict: Vec<(CString, Data)>,
    }

    impl Data {
        fn new(kind: c_int) -> Self {
            Data {
                kind,
                integer: 0,
                number: 0,
                text: CString::default(),
                dict: Vec::new(),
            }
        }
    }

    /// `launch_data_t launch_data_alloc(launch_data_type_t type)`.
    ///
    /// A real allocation: the guest's very first check is
    /// (`Framework+0x5da1634`) `test rax,rax; je`, so a null here would abandon
    /// the probe before it started, and a *live* dictionary is what lets
    /// [`launch_msg`] refuse for the right reason instead.
    unsafe extern "C" fn launch_data_alloc(kind: c_int) -> *mut c_void {
        let mut data = Data::new(kind);
        if kind == launch_data_type::ERRNO {
            data.number = LAUNCH_ERRSCH;
        }
        Box::into_raw(Box::new(data)) as *mut c_void
    }

    /// `launch_data_t launch_data_new_string(const char *val)`.
    ///
    /// Also checked (`Framework+0x5da1652` `test rax,rax; je`), and also real:
    /// the string is copied, because a dictionary outlives the caller's frame
    /// here exactly as it does in launchd.
    unsafe extern "C" fn launch_data_new_string(value: *const c_char) -> *mut c_void {
        let text = if value.is_null() {
            CString::default()
        } else {
            // SAFETY: the caller's own NUL-terminated string, per `OS_NONNULL1`.
            CString::new(unsafe { CStr::from_ptr(value) }.to_bytes()).unwrap_or_default()
        };
        let mut data = Data::new(launch_data_type::STRING);
        data.text = text;
        Box::into_raw(Box::new(data)) as *mut c_void
    }

    /// `bool launch_data_dict_insert(launch_data_t ldict, launch_data_t lval, const char *key)`.
    ///
    /// Takes ownership of `lval`, as launchd's does. The guest checks the
    /// answer (`Framework+0x5da166c` `test al,al; je`) and has a path for it.
    unsafe extern "C" fn launch_data_dict_insert(
        dictionary: *mut c_void,
        value: *mut c_void,
        key: *const c_char,
    ) -> bool {
        if dictionary.is_null() || value.is_null() || key.is_null() {
            return false;
        }
        // SAFETY: both are this file's own `Data` boxes, per `OS_NONNULL1..3`,
        // and the value is being handed over rather than aliased.
        let (dictionary, value) = (dictionary.cast::<Data>(), value.cast::<Data>());
        // SAFETY: as above - reading the value out of its box frees the box and
        // leaves the value owned by the dictionary, which is the transfer
        // launchd's own `launch_data_dict_insert` performs.
        let value = unsafe { *Box::from_raw(value) };
        let key = CString::new(unsafe { CStr::from_ptr(key) }.to_bytes()).unwrap_or_default();
        // SAFETY: a `Data` this file allocated and has not freed.
        let dictionary = unsafe { &mut *dictionary };
        match dictionary.dict.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => dictionary.dict.push((key, value)),
        }
        true
    }

    /// `launch_data_t launch_data_dict_lookup(const launch_data_t ldict, const char *key)`.
    ///
    /// The result is owned by `ldict`, which is launchd's own contract. The guest
    /// null-checks it (`Framework+0x5da18e4`).
    unsafe extern "C" fn launch_data_dict_lookup(
        dictionary: *mut c_void,
        key: *const c_char,
    ) -> *mut c_void {
        if dictionary.is_null() || key.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: a `Data` this file allocated, and the caller's NUL-terminated key.
        let found = unsafe { &*dictionary.cast::<Data>() }
            .dict
            .iter()
            .find_map(|(k, v)| (k.as_bytes() == unsafe { CStr::from_ptr(key) }.to_bytes())
                .then_some(v as *const Data as *mut c_void));
        found.unwrap_or(std::ptr::null_mut())
    }

    /// `launch_data_type_t launch_data_get_type(const launch_data_t ld)`.
    ///
    /// The value the node was allocated with. `LAUNCH_DATA_DICTIONARY` for a
    /// dictionary, which is what the guest requires before it will look a key
    /// up - so this is checked rather than asserted.
    unsafe extern "C" fn launch_data_get_type(data: *const c_void) -> c_int {
        if data.is_null() {
            return 0;
        }
        // SAFETY: a `Data` this file allocated, or null, which returned above.
        unsafe { (*data.cast::<Data>()).kind }
    }

    /// `long long launch_data_get_integer(const launch_data_t ld)`.
    ///
    /// The guest sign-extends the answer and traps if it does not fit in 32 bits
    /// (`Framework+0x5da190c`), so the width matters: this is an `i64`.
    unsafe extern "C" fn launch_data_get_integer(data: *const c_void) -> c_longlong {
        if data.is_null() {
            return 0;
        }
        // SAFETY: a `Data` this file allocated, or null, which returned above.
        unsafe { (*data.cast::<Data>()).integer }
    }

    /// `int launch_data_get_errno(const launch_data_t ld)`.
    ///
    /// Only meaningful for a `LAUNCH_DATA_ERRNO` value, and the guest asks for
    /// exactly that (`Framework+0x5da16a7` `cmp eax,0x9`). A node of any other
    /// type has no errno, and reporting one would be inventing it.
    unsafe extern "C" fn launch_data_get_errno(data: *const c_void) -> c_int {
        match null_safe_kind(data) {
            launch_data_type::ERRNO => {
                // SAFETY: the kind was just read off this node, so it is a `Data`.
                unsafe { (*data.cast::<Data>()).number }
            }
            _ => 0,
        }
    }

    /// The node's type, or zero for a null node. The one place the null check
    /// for the accessors is written, so that it cannot be forgotten in one of
    /// them.
    fn null_safe_kind(data: *const c_void) -> c_int {
        if data.is_null() {
            return 0;
        }
        // SAFETY: a `Data` this file allocated, or null, which returned above.
        unsafe { (*data.cast::<Data>()).kind }
    }

    /// `void launch_data_free(launch_data_t ld)`.
    ///
    /// Frees the whole tree, as launchd's does. Idempotent on null, because
    /// `OS_NONNULL1` is a promise the caller makes and this costs nothing.
    unsafe extern "C" fn launch_data_free(data: *mut c_void) {
        if data.is_null() {
            return;
        }
        // SAFETY: a `Data` this file allocated and has not freed. The `dict`
        // field owns its children, so this is the whole tree.
        drop(unsafe { Box::from_raw(data.cast::<Data>()) });
    }

    /// `launch_data_t launch_msg(const launch_data_t request)`.
    ///
    /// **The refusal, and the only one in this family that is one.** There is no
    /// launchd on this host: no `launchd` process, no bootstrap namespace, no
    /// `/var/run/com.apple.launchd.*` socket. A message cannot be delivered, so
    /// the honest answer is null - and the guest is built for it:
    /// `Framework+0x5da1685` tests the answer and, on null, returns `-1` from a
    /// function whose other exit is a parsed launchd reply. That is Chromium's
    /// own "could not reach launchd" path, reached for the right reason.
    ///
    /// Note what is deliberately *not* done here: the dictionary is built, filled
    /// and freed exactly as it would be on Darwin, so the only thing that fails
    /// is the one thing that genuinely does not exist.
    unsafe extern "C" fn launch_msg(_request: *const c_void) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// The errno a delivered message would have carried, for
    /// [`launch_data_get_errno`] to report; seeded by [`launch_data_alloc`].
    #[cfg(test)]
    const LAUNCH_ERRSCH_FOR_TESTS: c_int = LAUNCH_ERRSCH;

    // -------------------------------------------------------- DiskArbitration

    /// `DAReturn`, from `DiskArbitration.framework`. The values are the ones
    /// `DADissenter.h` prints beside them, hex digits and all.
    mod da {
        /// `kDAReturnNotFound` - "the object you asked about is not here", which
        /// `DADissenter.h` writes as `0xF8DA0006`.
        ///
        /// The right one for every call in this family, and the reason the
        /// dissenter exists at all: there is no IOKit disk object for a Darwin
        /// volume on this host, so a claim cannot be refused as *busy* or
        /// *exclusive access* - the disk was never found in the first place.
        pub const NOT_FOUND: i32 = 0xF8DA_0006u32 as i32;
    }

    /// One `DADissenterRef`.
    ///
    /// A `DADiskClaim` that could not be claimed has to *say so* through a
    /// dissenter, and the guest's caller then asks the dissenter for its status
    /// and its string. So the status is real data this file owns; the string is
    /// not, and [`dissenter_get_status_string`] is honest about why.
    struct Dissenter {
        status: i32,
    }

    /// `DADiskRef DADiskCreateFromBSDName(CFAllocatorRef, DASessionRef, const char *)`.
    ///
    /// **Null, and this is the whole feature detection.** `DADiskRef` is an IOKit
    /// `io_registry_entry_t` reached through the Darwin `IOKit` plug-in; there is
    /// no such object on this host and no way to invent one that would mean
    /// anything.
    ///
    /// The guest checks it and has a defined path for null - `base/mac`'s
    /// `MountWatcher` does `test r14,r14; je` at `Framework+0x5632a84` and logs
    /// "Unable to get disk" without ever reaching `DADiskClaim`. Returning a
    /// non-null disk would be the worst possible answer: it would put the guest
    /// on a path that dereferences a pointer this runtime would then have to
    /// service.
    unsafe extern "C" fn dadisk_create_from_bsd_name(
        _allocator: *const c_void,
        _session: *const c_void,
        _name: *const c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `io_service_t DADiskCopyIOMedia(DADiskRef)`.
    ///
    /// `MACH_PORT_NULL`. There is no media service to copy an io_service_t from,
    /// and a `mach_port_t` the guest would then send messages through is exactly
    /// the invented value to avoid.
    unsafe extern "C" fn dadisk_copy_io_media(_disk: *const c_void) -> i32 {
        0
    }

    /// `DADiskRef DADiskCopyWholeDisk(DADiskRef)`.
    ///
    /// Null, for the same reason [`dadisk_create_from_bsd_name`] is: the whole
    /// disk is the disk, and there is no disk.
    unsafe extern "C" fn dadisk_copy_whole_disk(_disk: *const c_void) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `DADissenterRef DADissenterCreate(CFAllocatorRef, DAReturn, CFStringRef)`.
    ///
    /// Real, because the guest's approval callbacks build their own and hand
    /// them to `DADiskClaim`; a null here would turn "I refuse" into "I did not
    /// answer". Only the status is kept - the caller's `CFStringRef` is not
    /// retained, because a `CFStringRef` this runtime did not create and does not
    /// own is not something to keep alive on its behalf.
    unsafe extern "C" fn dadissenter_create(
        _allocator: *const c_void,
        status: i32,
        _string: *const c_void,
    ) -> *mut c_void {
        Box::into_raw(Box::new(Dissenter { status })) as *mut c_void
    }

    /// `DAReturn DADissenterGetStatus(DADissenterRef)`.
    ///
    /// The status [`dadissenter_create`] was given, or `kDAReturnNotFound` for a
    /// dissenter this runtime refused to build - which is the status a null
    /// `DADiskRef` deserves.
    unsafe extern "C" fn dadissenter_get_status(dissenter: *const c_void) -> i32 {
        if dissenter.is_null() {
            return da::NOT_FOUND;
        }
        // SAFETY: a `Dissenter` this file allocated, or null, which returned above.
        unsafe { (*dissenter.cast::<Dissenter>()).status }
    }

    /// `CFStringRef DADissenterGetStatusString(DADissenterRef)`.
    ///
    /// **Null, and the header allows it** (`CFStringRef __nullable`). A dissenter
    /// here carries a status and no text: the failure is this runtime's, and the
    /// status code says which, so there is no string to hand over. Returning an
    /// invented one would be worse - the guest would message a `CFStringRef` it
    /// did not get from CoreFoundation.
    unsafe extern "C" fn dadissenter_get_status_string(_dissenter: *const c_void) -> *const c_void {
        std::ptr::null()
    }

    /// `void DADiskClaim(DADiskRef, DADiskClaimOptions, DADiskClaimReleaseCallback, DADiskClaimCallback, void *)`.
    ///
    /// **The completion callback is called**, with a dissenter, which is the one
    /// thing that must not be skipped: a claim that silently never completes is a
    /// wait that never ends. Darwin calls the callback with a null dissenter on
    /// success and a real one on failure; there is no success here.
    ///
    /// Unreachable in practice - the guest only claims a disk
    /// [`dadisk_create_from_bsd_name`] returned, and that is null - so this is
    /// the net under the feature detection rather than the detection itself.
    ///
    /// # Safety
    ///
    /// `callback`, if non-null, must be callable as
    /// `void (*)(DADiskRef, DADissenterRef, void *)`.
    unsafe extern "C" fn dadisk_claim(
        disk: *const c_void,
        _options: u32,
        _release: Option<unsafe extern "C" fn(*const c_void, *mut c_void) -> *mut c_void>,
        callback: Option<unsafe extern "C" fn(*const c_void, *mut c_void, *mut c_void)>,
        context: *mut c_void,
    ) {
        // SAFETY: the caller's own obligation, per the `DADiskClaimCallback` type.
        unsafe { callback.map(|f| f(disk, refused_dissenter(), context)) };
    }

    /// `void DADiskUnmount(DADiskRef, DADiskUnmountOptions, DADiskUnmountCallback, void *)`.
    ///
    /// As [`dadisk_claim`]: the completion callback is called with a dissenter,
    /// because the alternative is a caller waiting for a mount that will not
    /// happen and cannot be told so.
    ///
    /// # Safety
    ///
    /// `callback`, if non-null, must be callable as
    /// `void (*)(DADiskRef, DADissenterRef, void *)`.
    unsafe extern "C" fn dadisk_unmount(
        disk: *const c_void,
        _options: u32,
        callback: Option<unsafe extern "C" fn(*const c_void, *mut c_void, *mut c_void)>,
        context: *mut c_void,
    ) {
        // SAFETY: the caller's own obligation, per the `DADiskUnmountCallback` type.
        unsafe { callback.map(|f| f(disk, refused_dissenter(), context)) };
    }

    /// `void DADiskEject(DADiskRef, DADiskEjectOptions, DADiskEjectCallback, void *)`.
    ///
    /// As [`dadisk_unmount`].
    ///
    /// # Safety
    ///
    /// `callback`, if non-null, must be callable as
    /// `void (*)(DADiskRef, DADissenterRef, void *)`.
    unsafe extern "C" fn dadisk_eject(
        disk: *const c_void,
        _options: u32,
        callback: Option<unsafe extern "C" fn(*const c_void, *mut c_void, *mut c_void)>,
        context: *mut c_void,
    ) {
        // SAFETY: the caller's own obligation, per the `DADiskEjectCallback` type.
        unsafe { callback.map(|f| f(disk, refused_dissenter(), context)) };
    }

    /// A dissenter saying the disk was not found, for the three callbacks above.
    fn refused_dissenter() -> *mut c_void {
        Box::into_raw(Box::new(Dissenter { status: da::NOT_FOUND })) as *mut c_void
    }

    /// `void DADiskUnclaim(DADiskRef)`.
    ///
    /// Nothing to release. Chromium's `~MountWatcher` calls this on a disk it
    /// holds (`Framework+0x56327e9`), and on this host that disk is always null
    /// - but the function is `void` and the handle was never ours, so declining
    /// to free it is the only correct answer. Freeing it would be freeing a
    /// pointer from a foreign allocator.
    unsafe extern "C" fn dadisk_unclaim(_disk: *const c_void) {}

    /// `void DASessionScheduleWithRunLoop(DASessionRef, CFRunLoopRef, CFStringRef)`.
    ///
    /// A no-op, and deliberately so: a `DASessionRef` scheduled against a run
    /// loop is a subscription that would then have to fire, and there are no
    /// disk objects for it to report. Scheduling one would be a promise nothing
    /// here can keep.
    unsafe extern "C" fn dasession_schedule_with_run_loop(
        _session: *const c_void,
        _run_loop: *const c_void,
        _mode: *const c_void,
    ) {
    }

    /// `void DASessionUnscheduleFromRunLoop(DASessionRef, CFRunLoopRef, CFStringRef)`.
    ///
    /// The counterpart of a scheduling that did not happen, so also nothing.
    unsafe extern "C" fn dasession_unschedule_from_run_loop(
        _session: *const c_void,
        _run_loop: *const c_void,
        _mode: *const c_void,
    ) {
    }

    /// `void DARegisterDiskAppearedCallback(DASessionRef, DADiskAppearedCallback, void *)`.
    ///
    /// The registration is accepted and remembered as not made. Calling the
    /// callback is impossible - there is no disk, and there never will be one to
    /// appear - and dropping the registration on the floor without saying so is
    /// what this file exists to avoid, so [`registered_callbacks`] counts it.
    unsafe extern "C" fn daregister_disk_appeared_callback(
        _session: *const c_void,
        _callback: Option<unsafe extern "C" fn(*const c_void, *mut c_void)>,
        _context: *mut c_void,
    ) {
        count_registration();
    }

    /// `void DARegisterDiskDisappearedCallback(DASessionRef, DADiskDisappearedCallback, void *)`.
    ///
    /// As [`daregister_disk_appeared_callback`].
    unsafe extern "C" fn daregister_disk_disappeared_callback(
        _session: *const c_void,
        _callback: Option<unsafe extern "C" fn(*const c_void, *mut c_void)>,
        _context: *mut c_void,
    ) {
        count_registration();
    }

    /// `void DARegisterDiskDescriptionChangedCallback(DASessionRef, CFDictionaryRef, CFArrayRef, DADiskDescriptionChangedCallback, void *)`.
    ///
    /// As [`daregister_disk_appeared_callback`].
    unsafe extern "C" fn daregister_disk_description_changed_callback(
        _session: *const c_void,
        _match: *const c_void,
        _watch: *const c_void,
        _callback: Option<unsafe extern "C" fn(*const c_void, *const c_void, *mut c_void)>,
        _context: *mut c_void,
    ) {
        count_registration();
    }

    /// How many `DARegister*Callback` calls this process has accepted.
    ///
    /// Not a behaviour anything depends on: a count is here so that a test, and
    /// anyone reading a log, can see that the registration was *reached* rather
    /// than that it was quietly dropped.
    static REGISTRATIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn count_registration() {
        REGISTRATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// [`REGISTRATIONS`], for a test that wants to see the registration reached.
    #[cfg(test)]
    fn registrations() -> usize {
        REGISTRATIONS.load(std::sync::atomic::Ordering::Relaxed)
    }

    // ------------------------------------------------------------- CoreMIDI

    /// `kMIDIServerStartErr`, from `CoreMIDI.framework`'s `MIDIServices.h`.
    ///
    /// The one CoreMIDI status that names this situation exactly: the MIDI
    /// server could not be started. CoreMIDI is a client of a launchd-managed
    /// `MIDIServer`; there is no such service on this host and nothing to start.
    const MIDI_SERVER_START: i32 = -10839;

    /// `kMIDIInvalidClient`, from the same enum - the status for handing
    /// `MIDIClientDispose` a client that does not exist.
    const MIDI_INVALID_CLIENT: i32 = -10830;

    /// `OSStatus MIDIClientCreate(CFStringRef, MIDINotifyProc, void *, MIDIClientRef *)`.
    ///
    /// `kMIDIServerStartErr`, with the out-parameter set to null so that the
    /// caller cannot read a client that was never created.
    ///
    /// The guest checks *both* halves before using either: `Framework+0x80ef571`
    /// tests the status and `+0x80ef575` tests the out-client, and takes its own
    /// "no MIDI" path on either. That is the point of this entry: Chromium's
    /// `MidiManagerMac` treats a failed client as "MIDI is unavailable" and
    /// carries on with the rest of start-up.
    unsafe extern "C" fn midiclient_create(
        _name: *const c_void,
        _notify: Option<unsafe extern "C" fn(*const c_void, *mut c_void)>,
        _refcon: *mut c_void,
        out_client: *mut i32,
    ) -> i32 {
        if !out_client.is_null() {
            // SAFETY: the caller's own out-parameter, declared non-null.
            unsafe { out_client.write(0) };
        }
        MIDI_SERVER_START
    }

    /// `OSStatus MIDIClientDispose(MIDIClientRef)`.
    ///
    /// `kMIDIInvalidClient`: there is no client to dispose. The guest guards it
    /// with `test edi,edi; je` (`Framework+0x80ef38f`), so this is the defensive
    /// arm rather than a live path.
    unsafe extern "C" fn midiclient_dispose(_client: i32) -> i32 {
        MIDI_INVALID_CLIENT
    }

    /// `OSStatus MIDIInputPortCreate(MIDIClientRef, CFStringRef, MIDIReadProc, void *, MIDIPortRef *)`.
    ///
    /// As [`midiclient_create`]: the status names the reason and the out-parameter
    /// is nulled so no port handle exists to be used.
    unsafe extern "C" fn midiinput_port_create(
        _client: i32,
        _name: *const c_void,
        _read: Option<unsafe extern "C" fn(*const c_void, *const c_void, *mut c_void)>,
        _refcon: *mut c_void,
        out_port: *mut i32,
    ) -> i32 {
        if !out_port.is_null() {
            // SAFETY: the caller's own out-parameter, declared non-null.
            unsafe { out_port.write(0) };
        }
        MIDI_SERVER_START
    }

    /// `OSStatus MIDIOutputPortCreate(MIDIClientRef, CFStringRef, MIDIPortRef *)`.
    ///
    /// As [`midiclient_create`].
    unsafe extern "C" fn midioutput_port_create(
        _client: i32,
        _name: *const c_void,
        out_port: *mut i32,
    ) -> i32 {
        if !out_port.is_null() {
            // SAFETY: the caller's own out-parameter, declared non-null.
            unsafe { out_port.write(0) };
        }
        MIDI_SERVER_START
    }

    /// `OSStatus MIDIPortConnectSource(MIDIPortRef, MIDIEndpointRef, void *)`.
    ///
    /// There is no source to connect: [`midi_get_number_of_sources`] is zero, so
    /// this is unreachable from a guest that iterates endpoints, and the status
    /// says so if it is reached some other way.
    unsafe extern "C" fn midiport_connect_source(
        _port: i32,
        _source: i32,
        _refcon: *mut c_void,
    ) -> i32 {
        MIDI_SERVER_START
    }

    /// `OSStatus MIDISend(MIDIPortRef, MIDIEndpointRef, const MIDIPacketList *)`.
    ///
    /// No destination, so nothing to send to. `MIDIGetNumberOfDestinations` is
    /// zero, which is what makes a send unreachable rather than merely
    /// unsuccessful.
    unsafe extern "C" fn midisend(_port: i32, _destination: i32, _packets: *const c_void) -> i32 {
        MIDI_SERVER_START
    }

    /// `OSStatus MIDIObjectGetIntegerProperty(MIDIObjectRef, CFStringRef, SInt32 *)`.
    ///
    /// The status names the reason, and the out-parameter is zeroed rather than
    /// left holding whatever the caller had put there - a stale value read as a
    /// property is the silent-wrong-answer this project is avoiding.
    unsafe extern "C" fn midiobject_get_integer_property(
        _object: i32,
        _property: *const c_void,
        out_value: *mut i32,
    ) -> i32 {
        if !out_value.is_null() {
            // SAFETY: the caller's own out-parameter, declared non-null.
            unsafe { out_value.write(0) };
        }
        MIDI_SERVER_START
    }

    /// `OSStatus MIDIObjectGetStringProperty(MIDIObjectRef, CFStringRef, CFStringRef *)`.
    ///
    /// As [`midiobject_get_integer_property`], and null is the value the header
    /// allows: `CFStringRef __nullable * __nonnull str` is a non-null *pointer*
    /// to a nullable string, so writing null is the documented "no such
    /// property" answer.
    unsafe extern "C" fn midiobject_get_string_property(
        _object: i32,
        _property: *const c_void,
        out_value: *mut *const c_void,
    ) -> i32 {
        if !out_value.is_null() {
            // SAFETY: the caller's own out-parameter, declared non-null.
            unsafe { out_value.write(std::ptr::null()) };
        }
        MIDI_SERVER_START
    }

    /// `ItemCount MIDIGetNumberOfSources(void)`.
    ///
    /// **Zero, and it is the true answer rather than a stub.** This host has no
    /// CoreMIDI server, so it has no sources. The guest uses the count as a loop
    /// bound - `Framework+0x80ef7f8` subtracts it from the vector's length and
    /// resizes - so zero produces "no inputs", which is what
    /// `MidiManagerMac::RefreshInputs` should conclude.
    unsafe extern "C" fn midi_get_number_of_sources() -> u32 {
        0
    }

    /// `MIDIEndpointRef MIDIGetSource(ItemCount sourceIndex0)`.
    ///
    /// `0`, which is `kMIDIInvalidUniqueID` and a null object reference: there is
    /// no source at any index, because there are none. The guest null-checks
    /// before using it (`Framework+0x80ef7f8` onward).
    unsafe extern "C" fn midi_get_source(_index: u32) -> i32 {
        0
    }

    /// `ItemCount MIDIGetNumberOfDestinations(void)`.
    ///
    /// Zero, for [`midi_get_number_of_sources`]'s reason.
    unsafe extern "C" fn midi_get_number_of_destinations() -> u32 {
        0
    }

    /// `MIDIEndpointRef MIDIGetDestination(ItemCount destIndex0)`.
    ///
    /// [`midi_get_source`]'s answer, for the same reason.
    unsafe extern "C" fn midi_get_destination(_index: u32) -> i32 {
        0
    }

    /// `MIDIPacket * MIDIPacketListInit(MIDIPacketList *)`.
    ///
    /// **Null, and this one is a refusal rather than an answer.** It writes a
    /// `MIDITimeStamp` at the head of a `MIDIPacketList` and hands back the
    /// first `MIDIPacket`, at a fixed offset inside a structure this crate does
    /// not lay out. Returning a pointer into a caller's buffer at an invented
    /// offset would corrupt it silently, so the answer is null - and it is never
    /// reached, because a send needs a destination and there are none.
    unsafe extern "C" fn midipacketlist_init(_packets: *mut c_void) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `MIDIPacket * MIDIPacketListAdd(MIDIPacketList *, ByteCount, MIDIPacket *, MIDITimeStamp, ByteCount, const Byte *)`.
    ///
    /// [`midipacketlist_init`]'s refusal, for the same reason: it walks
    /// `MIDIPacket`s at a fixed stride inside a `MIDIPacketList` this crate does
    /// not describe, and it has no room to refuse *safely* without that layout.
    unsafe extern "C" fn midipacketlist_add(
        _packets: *mut c_void,
        _list_size: usize,
        _current: *mut c_void,
        _time: u64,
        _length: usize,
        _data: *const u8,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    // --------------------------------------------------------- VideoToolbox

    /// `Boolean VTIsHardwareDecodeSupported(CMVideoCodecType codecType)`.
    ///
    /// **False, and that is a true answer.** `VideoToolbox` decodes H.264, HEVC
    /// and VP9 by handing frames to the GPU through `IOSurface`; this host has
    /// no such path, and no codec is hardware-decoded here. The guest asks about
    /// `'avc1'` and `'vp09'` (`Framework+0x2fd2cac` and `+0x2fd4898` load those
    /// four-character codes) and **checks** the answer - `test al,al; je` at
    /// `+0x2fd2cb6` and `setne al` at `+0x2fd48aa` - so false puts it on its own
    /// software path, which is the correct one.
    ///
    /// The return is a `Boolean`, i.e. an `unsigned char`, which the guest reads
    /// out of `al`.
    unsafe extern "C" fn vt_is_hardware_decode_supported(_codec: u32) -> u8 {
        0
    }

    /// `void VTRegisterSupplementalVideoDecoderIfAvailable(CMVideoCodecType codecType)`.
    ///
    /// Does nothing, which is the whole of the function on this host: it
    /// registers a decoder that the operating system supplies, and there is no
    /// operating system here to ask.
    ///
    /// `void`, and the guest agrees - `Framework+0x2fd4899` calls it, discards
    /// `rax` entirely, and immediately calls something else. There is no return
    /// value to get right, and inventing one would be the only way to get this
    /// wrong.
    unsafe extern "C" fn vt_register_supplemental_video_decoder_if_available(_codec: u32) {}

    // ------------------------------------------------------------------ CUPS

    /// `IPP_STATUS_ERROR_OPERATION_NOT_SUPPORTED`, from `cups/ipp.h`.
    ///
    /// The IPP code for "this server does not support that operation", which is
    /// the truest thing an `ipp_status_t` can say here: there is no CUPS server
    /// on this host, so no IPP operation is available. Chosen over
    /// `IPP_STATUS_ERROR_SERVICE_UNAVAILABLE` because the *code* is about a
    /// capability, and there is no capability.
    const IPP_NO_SERVER: i32 = 0x0501;

    /// `HTTP_STATUS_SERVICE_UNAVAILABLE`, from `cups/http.h`: 500 is
    /// `Internal Server Error`, 501 `Not Implemented`, 502 `Bad Gateway`, 503
    /// `Service Unavailable`. The connection could not be made because nothing is
    /// listening, which is 503's exact meaning.
    const HTTP_NO_SERVER: i32 = 503;

    /// `PPD_FILE_OPEN_ERROR`, from `cups/ppd.h`: 0 is `PPD_OK`, 1 is
    /// "Unable to open PPD file". There is no PPD to open here, and 0 would be a
    /// lie about a file that was never opened.
    const PPD_NO_FILE: i32 = 1;

    /// What `cupsLastErrorString` says. This is the sentence the whole CUPS
    /// family exists to make reachable: on Darwin it would be a message from
    /// `cupsd`; here it is the reason there is no `cupsd`.
    const NO_PRINT_SYSTEM: &str = "rosetta3: no print system on this host - no CUPS server, no \
         destinations and no PPDs, so there is nothing here to answer a printing request";

    /// `ipp_status_t cupsLastError(void)`.
    ///
    /// [`IPP_NO_SERVER`], unconditionally. There is no per-thread state to keep
    /// here and inventing some would be worse than not having it: on this host
    /// *every* CUPS entry point refuses, so "the last error" has exactly one
    /// possible value and reporting it always is true rather than stale.
    unsafe extern "C" fn cups_last_error() -> i32 {
        IPP_NO_SERVER
    }

    /// `const char *cupsLastErrorString(void)`.
    ///
    /// [`NO_PRINT_SYSTEM`]. The one string in this file that exists to be read by
    /// a human, through the guest's own logging, and it says what is missing.
    unsafe extern "C" fn cups_last_error_string() -> *const c_char {
        NO_PRINT_SYSTEM.as_ptr().cast()
    }

    /// `const char *ppdErrorString(ppd_status_t status)`.
    ///
    /// The real table, from `cups/ppd.c`'s own `messages[]` - not a summary of
    /// it, and not an approximation, because this is a function whose entire
    /// content is that table and a wrong entry would be a wrong sentence about a
    /// real PPD error.
    unsafe extern "C" fn ppd_error_string(status: i32) -> *const c_char {
        /// `messages[]` from `cups/ppd.c`, in `ppd_status_t` order.
        const MESSAGES: [&str; 25] = [
            "OK",
            "Unable to open PPD file",
            "NULL PPD file pointer",
            "Memory allocation error",
            "Missing PPD-Adobe-4.x header",
            "Missing value string",
            "Internal error",
            "Bad OpenGroup",
            "OpenGroup without a CloseGroup first",
            "Bad OpenUI/JCLOpenUI",
            "OpenUI/JCLOpenUI without a CloseUI/JCLCloseUI first",
            "Bad OrderDependency",
            "Bad UIConstraints",
            "Missing asterisk in column 1",
            "Line longer than the maximum allowed (255 characters)",
            "Illegal control character",
            "Illegal main keyword string",
            "Illegal option keyword string",
            "Illegal translation string",
            "Illegal whitespace character",
            "Bad custom parameter",
            "Missing option keyword",
            "Bad value string",
            "Missing CloseGroup",
            "Missing CloseUI/JCLCloseUI",
        ];
        // Out of range in either direction is `Unknown`, which is what CUPS
        // returns for a status it does not have a sentence for.
        let text = match MESSAGES.get(status.max(0) as usize) {
            Some(text) if status >= 0 => *text,
            _ => "Unknown error",
        };
        text.as_ptr().cast()
    }

    /// `ppd_status_t ppdLastError(int *line)`.
    ///
    /// [`PPD_NO_FILE`], with the line written as zero: there is no file, so there
    /// is no line in it. A nonzero line would point at a line of a file that does
    /// not exist.
    unsafe extern "C" fn ppd_last_error(line: *mut c_int) -> i32 {
        if !line.is_null() {
            // SAFETY: the caller's own out-parameter, `int *`.
            unsafe { line.write(0) };
        }
        PPD_NO_FILE
    }

    /// `ppd_file_t *ppdOpenFd(int fd)`.
    ///
    /// Null. A PPD is a printer description, and there is no printer; there is
    /// also no `/Library/Printers/PPDs` on this host to find one in. The guest
    /// treats null as "no PPD, no options" - which is why the whole `ppdFind*`
    /// family below is unreachable rather than merely empty.
    unsafe extern "C" fn ppd_open_fd(_fd: c_int) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `void ppdClose(ppd_file_t *)`.
    ///
    /// Nothing to close: [`ppd_open_fd`] never produced a handle, and this crate
    /// did not allocate the one it is handed. Freeing a foreign pointer is the one
    /// thing a `void` close function must never do.
    unsafe extern "C" fn ppd_close(_ppd: *mut c_void) {}

    /// `void ppdMarkDefaults(ppd_file_t *)`.
    ///
    /// A no-op, for [`ppd_close`]'s reason.
    unsafe extern "C" fn ppd_mark_defaults(_ppd: *mut c_void) {}

    /// `ppd_attr_t *ppdFindAttr(ppd_file_t *, const char *name, const char *spec)`.
    ///
    /// Null: there is no PPD, so it has no attributes.
    unsafe extern "C" fn ppd_find_attr(
        _ppd: *const c_void,
        _name: *const c_char,
        _spec: *const c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ppd_option_t *ppdFindOption(ppd_file_t *, const char *keyword)`.
    ///
    /// Null, for [`ppd_find_attr`]'s reason.
    unsafe extern "C" fn ppd_find_option(_ppd: *const c_void, _keyword: *const c_char) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ppd_choice_t *ppdFindMarkedChoice(ppd_file_t *, const char *keyword)`.
    ///
    /// Null, for [`ppd_find_attr`]'s reason. Note that this is *not* "the default
    /// choice": a marked choice is a selection in a PPD, and with no PPD there is
    /// no selection to report.
    unsafe extern "C" fn ppd_find_marked_choice(
        _ppd: *const c_void,
        _keyword: *const c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ppd_choice_t *ppdFindChoice(ppd_option_t *, const char *option)`.
    ///
    /// Null, for [`ppd_find_attr`]'s reason.
    unsafe extern "C" fn ppd_find_choice(
        _option: *const c_void,
        _name: *const c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `int cupsMarkOptions(ppd_file_t *, int num_options, cups_option_t *)`.
    ///
    /// Zero options marked, which is the count, and the truth: there is no PPD to
    /// mark anything in.
    unsafe extern "C" fn cups_mark_options(
        _ppd: *const c_void,
        _num_options: c_int,
        _options: *const c_void,
    ) -> c_int {
        0
    }

    /// `ipp_t *ippNew(void)`.
    ///
    /// Null. An `ipp_t` is a request or a response, and both are messages to a
    /// CUPS server; with no server there is nothing to address.
    ///
    /// Worth recording that the guest does **not** check this: at
    /// `Framework+0x8b63014` it calls `ippNew` and passes the result straight
    /// into `ippAddInteger`. That is safe, and it is safe *because* an `ipp_t` is
    /// opaque - the guest never dereferences it, it only passes it on, and every
    /// function it can reach it with is in this file.
    unsafe extern "C" fn ipp_new() -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ipp_t *ippNewRequest(ipp_op_t op)`.
    ///
    /// [`ipp_new`]'s null, for the same reason. The guest does not check this one
    /// either (`Framework+0x8b63ddd`).
    unsafe extern "C" fn ipp_new_request(_op: c_int) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `int ippSetVersion(ipp_t *, int major, int minor)`.
    ///
    /// [`IPP_NO_SERVER`] rather than 0. The version was not set, because there is
    /// no request to set it on, and reporting success for an operation that did
    /// not happen is the failure mode this file exists to avoid. The guest
    /// discards the answer (`Framework+0x8b63df2`).
    unsafe extern "C" fn ipp_set_version(_ipp: *mut c_void, _major: c_int, _minor: c_int) -> c_int {
        IPP_NO_SERVER
    }

    /// `ipp_attribute_t *ippAddInteger(ipp_t *, ipp_tag_t, ipp_tag_t, const char *, int)`.
    unsafe extern "C" fn ipp_add_integer(
        _ipp: *mut c_void,
        _group: i32,
        _value_tag: i32,
        _name: *const c_char,
        _value: c_int,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ipp_attribute_t *ippAddString(ipp_t *, ipp_tag_t, ipp_tag_t, const char *, const char *, const char *)`.
    unsafe extern "C" fn ipp_add_string(
        _ipp: *mut c_void,
        _group: i32,
        _value_tag: i32,
        _name: *const c_char,
        _language: *const c_char,
        _value: *const c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ipp_attribute_t *ippAddStrings(ipp_t *, ipp_tag_t, ipp_tag_t, const char *, int, const char *, const char * const *)`.
    unsafe extern "C" fn ipp_add_strings(
        _ipp: *mut c_void,
        _group: i32,
        _value_tag: i32,
        _name: *const c_char,
        _num_values: c_int,
        _language: *const c_char,
        _values: *const *const c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ipp_attribute_t *ippAddBoolean(ipp_t *, ipp_tag_t, const char *, char)`.
    unsafe extern "C" fn ipp_add_boolean(
        _ipp: *mut c_void,
        _group: i32,
        _name: *const c_char,
        _value: c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ipp_attribute_t *ippAddRange(ipp_t *, ipp_tag_t, const char *, int, int)`.
    unsafe extern "C" fn ipp_add_range(
        _ipp: *mut c_void,
        _group: i32,
        _name: *const c_char,
        _lower: c_int,
        _upper: c_int,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ipp_attribute_t *ippAddCollection(ipp_t *, ipp_tag_t, const char *, ipp_t *)`.
    unsafe extern "C" fn ipp_add_collection(
        _ipp: *mut c_void,
        _group: i32,
        _name: *const c_char,
        _value: *mut c_void,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ipp_attribute_t *ippAddCollections(ipp_t *, ipp_tag_t, const char *, int, const ipp_t **)`.
    unsafe extern "C" fn ipp_add_collections(
        _ipp: *mut c_void,
        _group: i32,
        _name: *const c_char,
        _num_values: c_int,
        _values: *const *mut c_void,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ipp_attribute_t *ippCopyAttribute(ipp_t *, ipp_attribute_t *, int)`.
    unsafe extern "C" fn ipp_copy_attribute(
        _destination: *mut c_void,
        _attribute: *mut c_void,
        _quickcopy: c_int,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `void ippDelete(ipp_t *)`.
    ///
    /// A no-op: no `ipp_t` this file allocated is ever handed to a guest, so
    /// there is nothing to free and nothing that could be freed twice.
    unsafe extern "C" fn ipp_delete(_ipp: *mut c_void) {}

    /// `void ippDeleteAttribute(ipp_t *, ipp_attribute_t *)`.
    ///
    /// A no-op, for [`ipp_delete`]'s reason.
    unsafe extern "C" fn ipp_delete_attribute(_ipp: *mut c_void, _attribute: *mut c_void) {}

    /// `ipp_attribute_t *ippFirstAttribute(ipp_t *)`.
    ///
    /// Null: no request, so no attributes to walk. The guest loops on this
    /// (`Framework+0x8b65bb0` onward), so null ends the walk rather than faulting.
    unsafe extern "C" fn ipp_first_attribute(_ipp: *mut c_void) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ipp_attribute_t *ippNextAttribute(ipp_t *)`.
    ///
    /// Null, for [`ipp_first_attribute`]'s reason.
    unsafe extern "C" fn ipp_next_attribute(_ipp: *mut c_void) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ipp_attribute_t *ippFindAttribute(ipp_t *, const char *, ipp_tag_t)`.
    ///
    /// Null, for [`ipp_first_attribute`]'s reason.
    unsafe extern "C" fn ipp_find_attribute(
        _ipp: *mut c_void,
        _name: *const c_char,
        _value_tag: i32,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ipp_t *ippGetCollection(ipp_attribute_t *, int)`.
    ///
    /// Null, for [`ipp_first_attribute`]'s reason.
    unsafe extern "C" fn ipp_get_collection(
        _attribute: *const c_void,
        _element: c_int,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `const char *ippGetName(ipp_attribute_t *)`.
    ///
    /// Null, for [`ipp_first_attribute`]'s reason.
    unsafe extern "C" fn ipp_get_name(_attribute: *const c_void) -> *const c_char {
        std::ptr::null()
    }

    /// `ipp_tag_t ippGetGroupTag(ipp_attribute_t *)`.
    ///
    /// `IPP_TAG_ZERO`, the enum's own "no tag" - the value `ippGetGroupTag` gives
    /// for an attribute that is not there. (Checked: the guest only reads it
    /// after `ippFindAttribute` has returned something.)
    unsafe extern "C" fn ipp_get_group_tag(_attribute: *const c_void) -> i32 {
        0
    }

    /// `int ippGetCount(ipp_attribute_t *)`.
    ///
    /// Zero values, for [`ipp_first_attribute`]'s reason. This one is a count and
    /// the guest uses it as one, so zero is a real answer rather than a refusal.
    unsafe extern "C" fn ipp_get_count(_attribute: *const c_void) -> c_int {
        0
    }

    /// `int ippGetInteger(ipp_attribute_t *, int)`.
    ///
    /// Zero, for [`ipp_get_count`]'s reason. The guest null-checks the attribute
    /// it passes.
    unsafe extern "C" fn ipp_get_integer(_attribute: *const c_void, _element: c_int) -> c_int {
        0
    }

    /// `const char *ippGetString(ipp_attribute_t *, int, const char **)`.
    ///
    /// Null, for [`ipp_get_count`]'s reason. The `language` out-parameter is
    /// deliberately not written: there is no value, so there is no language to
    /// report, and leaving the caller's own initialisation alone is better than
    /// writing a null it may print.
    unsafe extern "C" fn ipp_get_string(
        _attribute: *const c_void,
        _element: c_int,
        _language: *mut *const c_char,
    ) -> *const c_char {
        std::ptr::null()
    }

    /// `int ippGetRange(ipp_attribute_t *, int, int *)`.
    ///
    /// Zero, for [`ipp_get_count`]'s reason.
    unsafe extern "C" fn ipp_get_range(
        _attribute: *const c_void,
        _element: c_int,
        _upper: *mut c_int,
    ) -> c_int {
        0
    }

    /// `int ippGetResolution(ipp_attribute_t *, int, int *, ipp_res_t *)`.
    ///
    /// Zero, for [`ipp_get_count`]'s reason.
    unsafe extern "C" fn ipp_get_resolution(
        _attribute: *const c_void,
        _element: c_int,
        _yres: *mut c_int,
        _units: *mut i32,
    ) -> c_int {
        0
    }

    /// `ipp_status_t ippGetStatusCode(ipp_t *)`.
    ///
    /// [`IPP_NO_SERVER`], and **not** `IPP_STATUS_OK`.
    ///
    /// The one place in this family where zero would be the dangerous answer, and
    /// it is worth spelling out. Chromium calls this *unconditionally* on the
    /// result of `cupsDoRequest` - `Framework+0x8b63f1f` - and then branches on
    /// it with `test ebx,ebx; je` at `+0x8b63f6c`. Zero there is
    /// `successful-ok`: the guest would carry on as if a print job had been
    /// accepted. A non-zero status is what puts it on its own error path, which
    /// is the whole point of answering honestly.
    unsafe extern "C" fn ipp_get_status_code(_ipp: *const c_void) -> i32 {
        IPP_NO_SERVER
    }

    /// `const char *cupsGetOption(const char *name, int num_options, cups_option_t *)`.
    ///
    /// Null: no option matched, because there are no options. A guest that walks
    /// an options array with this returns the first null, which is what
    /// `cupsGetOptions`'s own contract says to do.
    unsafe extern "C" fn cups_get_option(
        _name: *const c_char,
        _num_options: c_int,
        _options: *const c_void,
    ) -> *const c_char {
        std::ptr::null()
    }

    /// `pwg_media_t *pwgMediaForSize(int width, int length)`.
    ///
    /// Null. The function is documented to return null for a size it cannot name,
    /// and this crate does not hold CUPS's PWG media table - so the honest answer
    /// for every input is "no media of that size".
    ///
    /// **The trap, and the reason this is a refusal:** the guest does not
    /// null-check its way out of trouble, it null-checks its way to a *second*
    /// dereference. `Framework+0x8b62f81` tests the answer and, when it is
    /// non-null, reads `width` at `+0x18` and `length` at `+0x1c` of the returned
    /// `pwg_media_t`. A non-null answer therefore obliges this crate to publish
    /// a structure at a layout it has no source for. Null is both true and the
    /// only safe answer.
    unsafe extern "C" fn pwg_media_for_size(_width: c_int, _length: c_int) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `const char *cupsServer(void)`.
    ///
    /// Null. On Darwin this is the CUPS server's name or address, and the honest
    /// answer to "which server" on a host with no server is "none". The guest
    /// null-checks before using it, and building a plausible host name here would
    /// send a print job into a socket that does not exist.
    unsafe extern "C" fn cups_server() -> *const c_char {
        std::ptr::null()
    }

    /// `const char *cupsUser(void)`.
    ///
    /// The real login name, because that part *is* answerable: CUPS attributes a
    /// job to the invoking user, and this host has one. The only thing missing is
    /// the server to receive the job.
    ///
    /// The string is copied into storage that outlives the call, because CUPS
    /// documents the answer as valid until the next call and a guest may print it
    /// much later.
    unsafe extern "C" fn cups_user() -> *const c_char {
        // `crate::passwd`'s own `getpwuid` is the lookup a *guest* should be
        // given, because it hands back this host's `struct passwd` translated to
        // Darwin's offsets. Nothing here wants a `struct passwd` - only the one
        // `char *` out of it, and a `char *` is a `char *` on both platforms, so
        // the question of field order does not arise and the copy below is the
        // whole of the work.
        // SAFETY: a scalar uid, and a `getpwuid` this process's libc owns.
        let entry = unsafe { libc::getpwuid(libc::getuid()) };
        if entry.is_null() {
            return std::ptr::null();
        }
        // SAFETY: a non-null `struct passwd` libc just returned; `pw_name` is
        // either a NUL-terminated string or null, and neither is read past its NUL.
        let name = unsafe { (*entry).pw_name };
        if name.is_null() {
            return std::ptr::null();
        }
        // A `CString` allocation that is deliberately never freed: the guest owns
        // the answer for as long as it likes, exactly as it does on Darwin.
        match CString::new(unsafe { CStr::from_ptr(name) }.to_bytes()) {
            Ok(text) => text.into_raw(),
            Err(_) => std::ptr::null(),
        }
    }

    /// `const char *cupsUserAgent(void)`.
    ///
    /// Null. This is CUPS's own `CUPS_SERVER_VERSION`-derived user-agent string
    /// for the *library*, and there is no CUPS library here to derive one from.
    /// Writing a hand-made agent string would be claiming to be a version of
    /// something that is not installed.
    unsafe extern "C" fn cups_user_agent() -> *const c_char {
        std::ptr::null()
    }

    /// `int cupsGetDests2(http_t *http, cups_dest_t **dests)`.
    ///
    /// **Zero destinations, and this one is a true answer rather than a stub.**
    /// The two-argument spelling is the one the guest calls -
    /// `Framework+0x8b690ab` passes exactly an `http_t *` and a `cups_dest_t **`
    /// - and the count it returns is the number of printers CUPS knows about.
    /// This host has no printers, so the count is zero and `*dests` is null.
    ///
    /// The guest branches on the count at `Framework+0x8b690d8` (`test eax,eax;
    /// jle`) and has a path for "no destinations", which is the empty-printer-list
    /// case rather than an error.
    unsafe extern "C" fn cups_get_dests2(_http: *mut c_void, dests: *mut *mut c_void) -> c_int {
        if !dests.is_null() {
            // SAFETY: the caller's own out-parameter, declared non-null.
            unsafe { dests.write(std::ptr::null_mut()) };
        }
        0
    }

    /// `int cupsEnumDests(cups_dest_flags_t, int, int *, cups_ptype_t, cups_ptype_t, cups_dest_cb_t, void *)`.
    ///
    /// Zero, with the callback never invoked: there is nothing to enumerate.
    /// Invoking it would be inventing a destination.
    unsafe extern "C" fn cups_enum_dests(
        _flags: u32,
        _msec: c_int,
        _cancel: *mut c_int,
        _type: u32,
        _mask: u32,
        _callback: Option<unsafe extern "C" fn(*const c_void, *const c_void, *mut c_void)>,
        _user_data: *mut c_void,
    ) -> c_int {
        0
    }

    /// `cups_dest_t *cupsGetNamedDest(http_t *http, const char *name, const char *instance)`.
    ///
    /// Null: the named destination does not exist, because none do. The guest
    /// null-checks it at `Framework+0x8b66db0`.
    unsafe extern "C" fn cups_get_named_dest(
        _http: *mut c_void,
        _name: *const c_char,
        _instance: *const c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `cups_dest_t *cupsGetDest(const char *name, const char *instance, int num_dests, cups_dest_t *)`.
    ///
    /// Null, for [`cups_get_named_dest`]'s reason. Note the fourth argument is
    /// the array itself, not a count - a search over a list this crate never
    /// produced.
    unsafe extern "C" fn cups_get_dest(
        _name: *const c_char,
        _instance: *const c_char,
        _num_dests: c_int,
        _dests: *mut c_void,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `int cupsCopyDest(cups_dest_t *dest, int num_dests, cups_dest_t **dests)`.
    ///
    /// Zero destinations copied, for [`cups_get_dests2`]'s reason.
    unsafe extern "C" fn cups_copy_dest(
        _dest: *mut c_void,
        _num_dests: c_int,
        _dests: *mut *mut c_void,
    ) -> c_int {
        0
    }

    /// `int cupsRemoveDest(const char *name, const char *instance, int num_dests, cups_dest_t **dests)`.
    ///
    /// Zero destinations removed, for [`cups_get_dests2`]'s reason. The array is
    /// deliberately not written: there is no entry to shift down, and rewriting a
    /// list the guest did not get from this crate would be inventing one.
    unsafe extern "C" fn cups_remove_dest(
        _name: *const c_char,
        _instance: *const c_char,
        _num_dests: c_int,
        _dests: *mut *mut c_void,
    ) -> c_int {
        0
    }

    /// `cups_dinfo_t *cupsCopyDestInfo(http_t *http, cups_dest_t *dest)`.
    ///
    /// Null: there is no destination to describe.
    unsafe extern "C" fn cups_copy_dest_info(
        _http: *mut c_void,
        _dest: *const c_void,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `void cupsFreeDestInfo(cups_dinfo_t *)`.
    ///
    /// A no-op: [`cups_copy_dest_info`] never allocated one, and this crate did
    /// not receive the pointer from its own allocator.
    unsafe extern "C" fn cups_free_dest_info(_dinfo: *mut c_void) {}

    /// `void cupsFreeDests(int num_dests, cups_dest_t *)`.
    ///
    /// A no-op, for [`cups_free_dest_info`]'s reason. The guest calls it
    /// unconditionally after [`cups_get_dests2`], so it is reached on the very
    /// path this file makes reachable - and has nothing to do.
    unsafe extern "C" fn cups_free_dests(_num_dests: c_int, _dests: *mut c_void) {}

    /// `const char *cupsLocalizeDestValue(http_t *, cups_dest_t *, cups_dinfo_t *, const char *, const char *)`.
    ///
    /// Null: there is no destination, and no value to localise.
    unsafe extern "C" fn cups_localize_dest_value(
        _http: *mut c_void,
        _dest: *const c_void,
        _info: *const c_void,
        _option: *const c_char,
        _value: *const c_char,
    ) -> *const c_char {
        std::ptr::null()
    }

    /// `int cupsCheckDestSupported(http_t *, cups_dest_t *, cups_dinfo_t *, const char *, const char *)`.
    ///
    /// Zero - "not supported" - and true, not a refusal: no printer here supports
    /// any option, because there is no printer. The negative error answer would
    /// claim the check itself could not be made, which is not what is wrong.
    unsafe extern "C" fn cups_check_dest_supported(
        _http: *mut c_void,
        _dest: *mut c_void,
        _info: *const c_void,
        _option: *const c_char,
        _value: *const c_char,
    ) -> c_int {
        0
    }

    /// `const char *cupsGetPPD(const char *name)`.
    ///
    /// Null: there is no PPD database here to look a name up in.
    unsafe extern "C" fn cups_get_ppd(_name: *const c_char) -> *const c_char {
        std::ptr::null()
    }

    /// `const char *cupsGetPPD2(http_t *http, const char *name)`.
    ///
    /// [`cups_get_ppd`]'s null, for the same reason.
    unsafe extern "C" fn cups_get_ppd2(_http: *mut c_void, _name: *const c_char) -> *const c_char {
        std::ptr::null()
    }

    /// `ipp_t *cupsDoRequest(http_t *http, ipp_t *request, const char *resource)`.
    ///
    /// Null: no request was sent, because there is no server to send it to. See
    /// [`ipp_get_status_code`] for what the guest does with this answer and why
    /// the next function's value is the load-bearing one.
    unsafe extern "C" fn cups_do_request(
        _http: *mut c_void,
        _request: *mut c_void,
        _resource: *const c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `ipp_t *cupsGetResponse(http_t *http, const char *resource)`.
    ///
    /// [`cups_do_request`]'s null, for the same reason.
    unsafe extern "C" fn cups_get_response(
        _http: *mut c_void,
        _resource: *const c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `http_status_t cupsSendRequest(http_t *, ipp_t *, const char *, size_t)`.
    ///
    /// [`HTTP_NO_SERVER`]: the request was not sent, and 503 is the status that
    /// says a service was not available.
    unsafe extern "C" fn cups_send_request(
        _http: *mut c_void,
        _request: *const c_void,
        _resource: *const c_char,
        _length: usize,
    ) -> c_int {
        HTTP_NO_SERVER
    }

    /// `http_status_t cupsWriteRequestData(http_t *, const char *, size_t)`.
    ///
    /// [`cups_send_request`]'s status, for the same reason.
    unsafe extern "C" fn cups_write_request_data(
        _http: *mut c_void,
        _buffer: *const c_char,
        _length: usize,
    ) -> c_int {
        HTTP_NO_SERVER
    }

    /// `ipp_status_t cupsCancelJob2(http_t *, const char *name, int job_id, int purge)`.
    ///
    /// [`IPP_NO_SERVER`]: the job was not cancelled because there is no server
    /// holding jobs, and `client-error-not-found` would wrongly blame the job id.
    unsafe extern "C" fn cups_cancel_job2(
        _http: *mut c_void,
        _name: *const c_char,
        _job_id: c_int,
        _purge: c_int,
    ) -> i32 {
        IPP_NO_SERVER
    }

    /// `int ippPort(void)`.
    ///
    /// 631, IANA's registered IPP port and CUPS's own compiled-in default.
    ///
    /// The one constant in this file that is *not* a refusal, and it is worth
    /// saying why it is not a lie: this is a protocol constant, not a claim that
    /// anything is listening on it. A zero would be worse on both counts - it
    /// would deny the protocol's own port *and* hand a guest a URI with no port
    /// in it. See `cups/ipp.h`.
    unsafe extern "C" fn ipp_port() -> c_int {
        631
    }

    /// `http_t *httpConnectEncrypt(const char *host, int port, http_encryption_t)`.
    ///
    /// Null. There is no CUPS server to connect to on any host, and no TLS stack
    /// `HTTP_ENCRYPTION_REQUIRED` could be satisfied by. The guest null-checks
    /// every result of this shape - `Framework+0x8b690c5` is the one immediately
    /// downstream of it.
    unsafe extern "C" fn http_connect_encrypt(
        _host: *const c_char,
        _port: c_int,
        _encryption: c_int,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `void httpClose(http_t *)`.
    ///
    /// A no-op. Called unconditionally by the guest after every use of the
    /// connection - eleven call sites, `Framework+0x8b5bbd7` among them - and
    /// there is no connection to close, because [`http_connect_encrypt`] never
    /// produced one. Freeing the handle would be freeing a foreign pointer.
    unsafe extern "C" fn http_close(_http: *mut c_void) {}

    /// `void httpBlocking(http_t *http, int b)`.
    ///
    /// A no-op: there is no connection whose blocking mode could be set.
    unsafe extern "C" fn http_blocking(_http: *mut c_void, _blocking: c_int) {}

    /// `int httpError(http_t *http)`.
    ///
    /// [`HTTP_NO_SERVER`]. Every state an `http_t` can be in on this host is the
    /// one where the connection was never made, so this is the only answer that
    /// is ever true. `HTTP_STATUS_ERROR` (-1) would be the alternative and it says
    /// strictly less: "an error happened", with nothing about which.
    unsafe extern "C" fn http_error(_http: *mut c_void) -> c_int {
        HTTP_NO_SERVER
    }


    exports::exports! {
        functions {
            // `launchd`'s data model, and the one message that has nowhere to go.
            "launch_data_alloc" => launch_data_alloc,
            "launch_data_new_string" => launch_data_new_string,
            "launch_data_dict_insert" => launch_data_dict_insert,
            "launch_data_dict_lookup" => launch_data_dict_lookup,
            "launch_data_get_type" => launch_data_get_type,
            "launch_data_get_integer" => launch_data_get_integer,
            "launch_data_get_errno" => launch_data_get_errno,
            "launch_data_free" => launch_data_free,
            "launch_msg" => launch_msg,

            // DiskArbitration. Nothing to look up, and a claim that always says so.
            "DADiskCreateFromBSDName" => dadisk_create_from_bsd_name,
            "DADiskCopyIOMedia" => dadisk_copy_io_media,
            "DADiskCopyWholeDisk" => dadisk_copy_whole_disk,
            "DADissenterCreate" => dadissenter_create,
            "DADissenterGetStatus" => dadissenter_get_status,
            "DADissenterGetStatusString" => dadissenter_get_status_string,
            "DADiskClaim" => dadisk_claim,
            "DADiskUnclaim" => dadisk_unclaim,
            "DADiskUnmount" => dadisk_unmount,
            "DADiskEject" => dadisk_eject,
            "DASessionScheduleWithRunLoop" => dasession_schedule_with_run_loop,
            "DASessionUnscheduleFromRunLoop" => dasession_unschedule_from_run_loop,
            "DARegisterDiskAppearedCallback" => daregister_disk_appeared_callback,
            "DARegisterDiskDisappearedCallback" => daregister_disk_disappeared_callback,
            "DARegisterDiskDescriptionChangedCallback" => daregister_disk_description_changed_callback,

            // CoreMIDI: no server, and no endpoints.
            "MIDIClientCreate" => midiclient_create,
            "MIDIClientDispose" => midiclient_dispose,
            "MIDIInputPortCreate" => midiinput_port_create,
            "MIDIOutputPortCreate" => midioutput_port_create,
            "MIDIPortConnectSource" => midiport_connect_source,
            "MIDISend" => midisend,
            "MIDIObjectGetIntegerProperty" => midiobject_get_integer_property,
            "MIDIObjectGetStringProperty" => midiobject_get_string_property,
            "MIDIGetNumberOfSources" => midi_get_number_of_sources,
            "MIDIGetSource" => midi_get_source,
            "MIDIGetNumberOfDestinations" => midi_get_number_of_destinations,
            "MIDIGetDestination" => midi_get_destination,
            "MIDIPacketListInit" => midipacketlist_init,
            "MIDIPacketListAdd" => midipacketlist_add,

            // VideoToolbox: the two probes.
            "VTIsHardwareDecodeSupported" => vt_is_hardware_decode_supported,
            "VTRegisterSupplementalVideoDecoderIfAvailable" => vt_register_supplemental_video_decoder_if_available,

            // CUPS: the printer database.
            "cupsGetDests2" => cups_get_dests2,
            "cupsEnumDests" => cups_enum_dests,
            "cupsGetNamedDest" => cups_get_named_dest,
            "cupsGetDest" => cups_get_dest,
            "cupsCopyDest" => cups_copy_dest,
            "cupsRemoveDest" => cups_remove_dest,
            "cupsCopyDestInfo" => cups_copy_dest_info,
            "cupsFreeDestInfo" => cups_free_dest_info,
            "cupsFreeDests" => cups_free_dests,
            "cupsLocalizeDestValue" => cups_localize_dest_value,
            "cupsCheckDestSupported" => cups_check_dest_supported,
            "cupsGetOption" => cups_get_option,
            "cupsGetPPD" => cups_get_ppd,
            "cupsGetPPD2" => cups_get_ppd2,
            "cupsLastError" => cups_last_error,
            "cupsLastErrorString" => cups_last_error_string,
            "cupsMarkOptions" => cups_mark_options,
            "cupsServer" => cups_server,
            "cupsUser" => cups_user,
            "cupsUserAgent" => cups_user_agent,
            "pwgMediaForSize" => pwg_media_for_size,

            // CUPS: the IPP message model, which is a message to a server.
            "ippNew" => ipp_new,
            "ippNewRequest" => ipp_new_request,
            "ippSetVersion" => ipp_set_version,
            "ippAddInteger" => ipp_add_integer,
            "ippAddString" => ipp_add_string,
            "ippAddStrings" => ipp_add_strings,
            "ippAddBoolean" => ipp_add_boolean,
            "ippAddRange" => ipp_add_range,
            "ippAddCollection" => ipp_add_collection,
            "ippAddCollections" => ipp_add_collections,
            "ippCopyAttribute" => ipp_copy_attribute,
            "ippDelete" => ipp_delete,
            "ippDeleteAttribute" => ipp_delete_attribute,
            "ippFirstAttribute" => ipp_first_attribute,
            "ippNextAttribute" => ipp_next_attribute,
            "ippFindAttribute" => ipp_find_attribute,
            "ippGetCollection" => ipp_get_collection,
            "ippGetName" => ipp_get_name,
            "ippGetGroupTag" => ipp_get_group_tag,
            "ippGetCount" => ipp_get_count,
            "ippGetInteger" => ipp_get_integer,
            "ippGetString" => ipp_get_string,
            "ippGetRange" => ipp_get_range,
            "ippGetResolution" => ipp_get_resolution,
            "ippGetStatusCode" => ipp_get_status_code,
            "ippPort" => ipp_port,

            // CUPS: the HTTP transport, and the requests that need it.
            "httpConnectEncrypt" => http_connect_encrypt,
            "httpClose" => http_close,
            "httpBlocking" => http_blocking,
            "httpError" => http_error,
            "cupsDoRequest" => cups_do_request,
            "cupsGetResponse" => cups_get_response,
            "cupsSendRequest" => cups_send_request,
            "cupsWriteRequestData" => cups_write_request_data,
            "cupsCancelJob2" => cups_cancel_job2,

            // CUPS: PPDs, of which there are none.
            "ppdOpenFd" => ppd_open_fd,
            "ppdClose" => ppd_close,
            "ppdMarkDefaults" => ppd_mark_defaults,
            "ppdFindAttr" => ppd_find_attr,
            "ppdFindOption" => ppd_find_option,
            "ppdFindMarkedChoice" => ppd_find_marked_choice,
            "ppdFindChoice" => ppd_find_choice,
            "ppdLastError" => ppd_last_error,
            "ppdErrorString" => ppd_error_string,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Read a `const char *` a refusal function returned.
        ///
        /// # Safety
        ///
        /// `at` must be a pointer a function in this file returned, which for
        /// the `&'static str` constants it always is.
        unsafe fn text(at: *const c_char) -> &'static str {
            assert!(!at.is_null(), "expected a string, got null");
            // SAFETY: the caller's obligation, per the doc comment.
            unsafe { CStr::from_ptr(at) }.to_str().expect("a UTF-8 C string")
        }

        #[test]
        fn the_refusals_name_themselves() {
            // SAFETY: both are this module's own `&'static str` constants.
            let said = unsafe { text(cups_last_error_string()) };
            assert!(said.contains("no print system"), "{said}");
            // And the number that goes with it is the one the header defines.
            assert_eq!(IPP_NO_SERVER, 0x0501);
            assert_eq!(HTTP_NO_SERVER, 503);
        }

        #[test]
        fn a_zero_printer_list_is_zero_and_not_an_error() {
            let mut dests = 0xdead as *mut c_void;
            // SAFETY: `dests` is a live, writable out-parameter of the right type.
            let count = unsafe { cups_get_dests2(std::ptr::null_mut(), &mut dests) };
            assert_eq!(count, 0);
            assert!(dests.is_null(), "no destinations means a null list");
            // And the guest's own test - `test eax,eax; jle` - takes the
            // "none" path rather than an error path.
            assert!(count <= 0);
        }

        #[test]
        fn the_status_code_is_not_success() {
            // SAFETY: the function reads nothing.
            let status = unsafe { ipp_get_status_code(std::ptr::null()) };
            assert_ne!(status, 0, "zero here is `successful-ok` and would be a lie");
            assert_eq!(status, IPP_NO_SERVER);
        }

        #[test]
        fn the_ppd_error_table_is_cups_own() {
            // SAFETY: the function reads nothing but its argument.
            unsafe {
                assert_eq!(text(ppd_error_string(PPD_NO_FILE)), "Unable to open PPD file");
                assert_eq!(text(ppd_error_string(0)), "OK");
                assert_eq!(text(ppd_error_string(2)), "NULL PPD file pointer");
                assert_eq!(text(ppd_error_string(24)), "Missing CloseUI/JCLCloseUI");
                // Out of range in both directions is CUPS's own "Unknown".
                assert_eq!(text(ppd_error_string(-1)), "Unknown error");
                assert_eq!(text(ppd_error_string(999)), "Unknown error");
            }
        }

        #[test]
        fn the_last_ppd_error_points_at_no_line() {
            let mut line = 12345;
            // SAFETY: `line` is a live `int`.
            let status = unsafe { ppd_last_error(&mut line) };
            assert_eq!(status, PPD_NO_FILE);
            assert_eq!(line, 0, "there is no file, so there is no line in it");
            // A null out-parameter is declined rather than dereferenced.
            // SAFETY: the function checks for null before writing.
            assert_eq!(unsafe { ppd_last_error(std::ptr::null_mut()) }, PPD_NO_FILE);
        }

        #[test]
        fn midi_says_the_server_is_the_problem() {
            let mut client = 0x1234;
            // SAFETY: `client` is a live `MIDIClientRef` out-parameter.
            let status = unsafe {
                midiclient_create(
                    std::ptr::null(),
                    None,
                    std::ptr::null_mut(),
                    &mut client,
                )
            };
            assert_eq!(status, MIDI_SERVER_START);
            assert_eq!(client, 0, "no client, so no handle to be used by mistake");
            // And the count the guest loops on really is zero.
            // SAFETY: both take no arguments and read nothing.
            assert_eq!(unsafe { midi_get_number_of_sources() }, 0);
            assert_eq!(unsafe { midi_get_number_of_destinations() }, 0);
        }

        #[test]
        fn a_claim_that_cannot_happen_still_completes() {
            // The thing that must not be skipped: a callback that never fires is
            // a wait that never ends.
            static mut FIRED: Option<*mut c_void> = None;
            unsafe extern "C" fn callback(
                _disk: *const c_void,
                dissenter: *mut c_void,
                context: *mut c_void,
            ) {
                unsafe {
                    FIRED = Some(context);
                    // The dissenter must say *why*, not merely that it failed.
                    assert_eq!(dadissenter_get_status(dissenter), da::NOT_FOUND);
                    assert!(dadissenter_get_status_string(dissenter).is_null());
                }
            }
            let mut marker = 7usize;
            // SAFETY: `callback` has the `DADiskClaimCallback` type, and `marker`
            // is live for the call.
            unsafe {
                dadisk_claim(
                    std::ptr::null(),
                    0,
                    None,
                    Some(callback),
                    (&mut marker as *mut usize).cast(),
                );
            }
            // SAFETY: only this thread wrote it, and it has not been read yet.
            assert_eq!(unsafe { FIRED }, Some((&mut marker as *mut usize).cast()));
        }

        #[test]
        fn no_disk_object_is_invented() {
            // SAFETY: every one of these reads nothing and returns a constant.
            unsafe {
                assert!(dadisk_create_from_bsd_name(
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null()
                )
                .is_null());
                assert_eq!(dadisk_copy_io_media(std::ptr::null()), 0);
                assert!(dadisk_copy_whole_disk(std::ptr::null()).is_null());
                // And a status read out of a null dissenter is still an answer.
                assert_eq!(dadissenter_get_status(std::ptr::null()), da::NOT_FOUND);
            }
        }

        #[test]
        fn a_hardware_video_decoder_is_not_claimed() {
            // SAFETY: both take one word and return a value or nothing.
            unsafe {
                // 'avc1' and 'vp09', the two the guest asks about.
                assert_eq!(vt_is_hardware_decode_supported(0x6176_6331), 0);
                assert_eq!(vt_is_hardware_decode_supported(0x7670_3039), 0);
                vt_register_supplemental_video_decoder_if_available(0x7670_3039);
            }
        }

        /// The launchd data model is real, and a tree of it frees whole.
        #[test]
        fn a_launch_dictionary_is_built_inspected_and_freed() {
            // SAFETY: every pointer below comes from this module and is freed
            // here; `lookup` deliberately returns a value the dictionary owns.
            unsafe {
                let dict = launch_data_alloc(launch_data_type::DICTIONARY);
                assert!(!dict.is_null());
                assert_eq!(launch_data_get_type(dict), launch_data_type::DICTIONARY);
                // The guest requires a dictionary before it will look a key up.
                assert_eq!(launch_data_get_type(dict), 1);

                let value = launch_data_new_string(c"Label".as_ptr());
                assert!(!value.is_null());
                assert_eq!(launch_data_get_type(value), launch_data_type::STRING);
                assert!(launch_data_dict_insert(dict, value, c"JobLabelKey".as_ptr()));
                // Insert took ownership, so the caller must not free it.
                assert!(launch_data_dict_lookup(dict, c"JobLabelKey".as_ptr()).is_null() == false);
                assert!(launch_data_dict_lookup(dict, c"NoSuchKey".as_ptr()).is_null());

                // And the refusal: built in full, and undeliverable.
                assert!(launch_msg(dict).is_null());

                // A null out of every accessor is declined rather than read.
                assert_eq!(launch_data_get_type(std::ptr::null()), 0);
                assert_eq!(launch_data_get_integer(std::ptr::null()), 0);
                assert_eq!(launch_data_get_errno(std::ptr::null()), 0);
                launch_data_free(std::ptr::null_mut());

                launch_data_free(dict);
            }
        }

        /// The value the accessors would report for an undelivered message.
        #[test]
        fn an_errno_value_reports_the_refusal() {
            // SAFETY: a `Data` this module allocated, of the type the guest asks
            // for at `Framework+0x5da16a7` (`cmp eax,0x9`).
            unsafe {
                let mut node = Data::new(launch_data_type::ERRNO);
                node.number = LAUNCH_ERRSCH_FOR_TESTS;
                let at = Box::into_raw(Box::new(node)) as *mut c_void;
                assert_eq!(launch_data_get_type(at), 9);
                assert_eq!(launch_data_get_errno(at), 3, "Darwin's ESRCH");
                launch_data_free(at);
            }
        }

        #[test]
        fn a_registration_is_counted_rather_than_dropped() {
            let before = registrations();
            // SAFETY: the callbacks may be null; nothing is called.
            unsafe {
                daregister_disk_appeared_callback(
                    std::ptr::null(),
                    None,
                    std::ptr::null_mut(),
                );
                daregister_disk_disappeared_callback(
                    std::ptr::null(),
                    None,
                    std::ptr::null_mut(),
                );
                daregister_disk_description_changed_callback(
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    None,
                    std::ptr::null_mut(),
                );
            }
            assert_eq!(registrations(), before + 3);
        }

        #[test]
        fn the_ipp_port_is_the_protocols_own() {
            // SAFETY: the function reads nothing.
            assert_eq!(unsafe { ipp_port() }, 631);
        }

        #[test]
        fn the_current_user_is_answerable_even_without_a_server() {
            // SAFETY: the function allocates and leaks a copy of the login name,
            // exactly as CUPS's own answer would be owned.
            let at = unsafe { cups_user() };
            if !at.is_null() {
                // SAFETY: a non-null `const char *` this module allocated.
                let said = unsafe { CStr::from_ptr(at) }.to_bytes();
                assert!(!said.is_empty(), "a user name, not an empty string");
            }
        }
    }
}

/// The one row [`crate::lib`] holds for this file.
///
/// The table is a list of `exports::Module` values, and every one of them is
/// declared by the macro at the bottom of `imp` - so this is a re-export
/// rather than a second declaration, and the two cannot disagree about which
/// names the module answers for.
#[cfg(not(target_os = "macos"))]
pub use imp::MODULE;

/// Nothing of this file's on Darwin: the host has every one of these
/// subsystems, and shadowing its own with a refusal would be the worst of
/// both worlds. See the `exports!` at the foot of the file.
#[cfg(target_os = "macos")]
exports::exports! {}
