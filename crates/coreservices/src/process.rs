use std::ffi::{CStr, c_char, c_void};

type OSStatus = i32;
const NO_ERR: OSStatus = 0;
/// `paramErr`.
const PARAM_ERR: OSStatus = -50;
/// `procNotFound`: the serial number names no process this tree knows.
const PROC_NOT_FOUND: OSStatus = -600;

/// `ProcessSerialNumber { UInt32 highLongOfPSN; UInt32 lowLongOfPSN; }`, whose
/// layout is ABI: a guest declares one on its stack.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessSerialNumber {
    pub high: u32,
    pub low: u32,
}

/// `kCurrentProcess`, which is `{ 0, 2 }` - and the only process here.
const CURRENT: ProcessSerialNumber = ProcessSerialNumber { high: 0, low: 2 };
/// `kNoProcess`, `{ 0, 0 }`.
const NO_PROCESS: ProcessSerialNumber = ProcessSerialNumber { high: 0, low: 0 };

/// Whether a serial number names this process.
///
/// `kCurrentProcess` does, and so does the number this tree hands out - they
/// are the same number. Anything else names a process that is not here.
///
/// # Safety
///
/// `psn` is null or a readable `ProcessSerialNumber`.
unsafe fn is_this_process(psn: *const ProcessSerialNumber) -> bool {
    if psn.is_null() {
        return false;
    }
    let psn = unsafe { *psn };
    psn == CURRENT || psn == NO_PROCESS
}

/// `OSErr GetCurrentProcess(ProcessSerialNumber *psn)`
///
/// # Safety
///
/// `psn` is null or writable for a `ProcessSerialNumber`.
pub unsafe extern "C" fn GetCurrentProcess(psn: *mut ProcessSerialNumber) -> i16 {
    if psn.is_null() {
        return PARAM_ERR as i16;
    }
    // SAFETY: the caller states it is writable.
    unsafe { *psn = CURRENT };
    NO_ERR as i16
}

/// `OSErr GetFrontProcess(ProcessSerialNumber *psn)`
///
/// The one in front is this one: nothing else here has a window.
///
/// # Safety
///
/// As [`GetCurrentProcess`].
pub unsafe extern "C" fn GetFrontProcess(psn: *mut ProcessSerialNumber) -> i16 {
    unsafe { GetCurrentProcess(psn) }
}

/// `OSErr SameProcess(const ProcessSerialNumber *, const ProcessSerialNumber *,
/// Boolean *result)`
///
/// # Safety
///
/// Both serial numbers are readable and `result` is writable.
pub unsafe extern "C" fn SameProcess(
    a: *const ProcessSerialNumber,
    b: *const ProcessSerialNumber,
    result: *mut u8,
) -> i16 {
    if a.is_null() || b.is_null() || result.is_null() {
        return PARAM_ERR as i16;
    }
    let same = unsafe { *a == *b };
    // SAFETY: the caller states `result` is writable.
    unsafe { *result = u8::from(same) };
    NO_ERR as i16
}

/// `OSErr SetFrontProcess(const ProcessSerialNumber *psn)`
///
/// # Safety
///
/// `psn` is null or a readable `ProcessSerialNumber`.
pub unsafe extern "C" fn SetFrontProcess(psn: *const ProcessSerialNumber) -> i16 {
    match unsafe { is_this_process(psn) } {
        true => NO_ERR as i16,
        false => PROC_NOT_FOUND as i16,
    }
}

/// `OSStatus TransformProcessType(const ProcessSerialNumber *,
/// ProcessApplicationTransformState)`
///
/// A program launched from a terminal rather than from a bundle is a background
/// process on the Mac, and this is how it promotes itself so that it can put up
/// a window and own the menu bar. Here a window is SDL's and is made whatever
/// the program's type is, so the promotion has nothing left to do - which is
/// why it succeeds rather than being refused.
///
/// # Safety
///
/// As [`SetFrontProcess`].
pub unsafe extern "C" fn TransformProcessType(
    psn: *const ProcessSerialNumber,
    _kind: u32,
) -> OSStatus {
    match unsafe { is_this_process(psn) } {
        true => NO_ERR,
        false => PROC_NOT_FOUND,
    }
}

/// `OSStatus GetProcessBundleLocation(const ProcessSerialNumber *, FSRef *)`
///
/// # Safety
///
/// `psn` is readable, and `location` is null or storage for a whole `FSRef`.
pub unsafe extern "C" fn GetProcessBundleLocation(
    psn: *const ProcessSerialNumber,
    location: *mut c_void,
) -> OSStatus {
    if location.is_null() {
        return PARAM_ERR;
    }
    if !unsafe { is_this_process(psn) } {
        return PROC_NOT_FOUND;
    }
    let Some(root) = foundation::bundle::main().map(|layout| layout.root.clone()) else {
        // A program outside a bundle has no bundle location, which is what
        // Apple answers for one too.
        return PROC_NOT_FOUND;
    };
    // SAFETY: the caller states `location` holds a whole `FSRef`.
    unsafe { crate::files::store_path(location, root) };
    NO_ERR
}

/// `OSErr GetNextProcess(ProcessSerialNumber *psn)`
///
/// The list holds this process alone, so it follows `kNoProcess` and nothing
/// follows it.
///
/// FIXME (ACCURACY): another guest application of the same prefix is a process
/// a Mac would list here too.
///
/// # Safety
///
/// `psn` is null or a readable and writable `ProcessSerialNumber`.
pub unsafe extern "C" fn GetNextProcess(psn: *mut ProcessSerialNumber) -> i16 {
    if psn.is_null() {
        return PARAM_ERR as i16;
    }
    // SAFETY: the caller states it is readable and writable.
    unsafe {
        match *psn == NO_PROCESS {
            true => {
                *psn = CURRENT;
                NO_ERR as i16
            }
            false => {
                *psn = NO_PROCESS;
                PROC_NOT_FOUND as i16
            }
        }
    }
}

/// `ProcessInfoRec`, packed to two bytes as `<Processes.h>` declares it.
#[repr(C, packed(2))]
pub struct ProcessInfoRec {
    length: u32,
    name: *mut u8,
    number: ProcessSerialNumber,
    kind: u32,
    signature: u32,
    mode: u32,
    location: *mut c_void,
    size: u32,
    free_memory: u32,
    launcher: ProcessSerialNumber,
    launch_date: u32,
    active_time: u32,
    app_ref: *mut c_void,
}

/// `'????'`, what a process with no bundle code reports.
const UNKNOWN_CODE: u32 = u32::from_be_bytes(*b"????");
/// A process name is a `Str31`.
const PROCESS_NAME_ROOM: usize = 31;

/// An `Info.plist` four-character code as an `OSType`.
fn four_char_code(layout: Option<&foundation::bundle::Layout>, key: &str) -> u32 {
    layout
        .and_then(|layout| layout.info_string(key))
        .and_then(|code| <[u8; 4]>::try_from(code.as_bytes()).ok())
        .map_or(UNKNOWN_CODE, u32::from_be_bytes)
}

/// `OSErr GetProcessInformation(const ProcessSerialNumber *psn,
/// ProcessInfoRec *info)`
///
/// Only the fields `info->processInfoLength` covers are written. The name goes
/// into the caller's buffer when there is one, and the application's location
/// into the caller's `FSRef`.
///
/// FIXME (ACCURACY): the mode, launch date and active time are reported as zero.
///
/// # Safety
///
/// `psn` is null or readable; `info` is null or writable for as many bytes as
/// its first field says, and the buffers it points at are a `Str31` and an
/// `FSRef`, or null.
pub unsafe extern "C" fn GetProcessInformation(
    psn: *const ProcessSerialNumber,
    info: *mut ProcessInfoRec,
) -> i16 {
    if info.is_null() {
        return PARAM_ERR as i16;
    }
    // SAFETY: as the caller states.
    if !unsafe { is_this_process(psn) } {
        return PROC_NOT_FOUND as i16;
    }
    let layout = foundation::bundle::main();
    // SAFETY: the caller states `info` is writable for its stated length.
    let length = unsafe { (&raw const (*info).length).read_unaligned() } as usize;
    macro_rules! set {
        ($field:ident, $value:expr) => {
            let offset = std::mem::offset_of!(ProcessInfoRec, $field);
            let value = $value;
            if offset + size_of_val(&value) <= length {
                // SAFETY: within the length the caller says is writable.
                unsafe { (&raw mut (*info).$field).write_unaligned(value) };
            }
        };
    }
    set!(number, CURRENT);
    set!(kind, four_char_code(layout, "CFBundlePackageType"));
    set!(signature, four_char_code(layout, "CFBundleSignature"));
    set!(mode, 0u32);
    set!(location, std::ptr::null_mut());
    set!(size, 0u32);
    set!(free_memory, 0u32);
    set!(launcher, NO_PROCESS);
    set!(launch_date, 0u32);
    set!(active_time, 0u32);
    let field = |offset: usize| offset + size_of::<*mut c_void>() <= length;
    if field(std::mem::offset_of!(ProcessInfoRec, name)) {
        // SAFETY: within the stated length.
        let name = unsafe { (&raw const (*info).name).read_unaligned() };
        let text = layout
            .and_then(|layout| layout.info_string("CFBundleName"))
            .map(str::to_owned)
            .or_else(|| {
                darwin::process::me().and_then(|me| me.path.rsplit('/').next().map(str::to_owned))
            })
            .unwrap_or_default();
        if !name.is_null() {
            let bytes = &text.as_bytes()[..text.len().min(PROCESS_NAME_ROOM)];
            // SAFETY: the caller's `Str31`: a length byte, then the bytes.
            unsafe {
                *name = bytes.len() as u8;
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), name.add(1), bytes.len());
            }
        }
    }
    if field(std::mem::offset_of!(ProcessInfoRec, app_ref)) {
        // SAFETY: within the stated length.
        let app_ref = unsafe { (&raw const (*info).app_ref).read_unaligned() };
        if let (false, Some(layout)) = (app_ref.is_null(), layout) {
            // SAFETY: the caller's whole `FSRef`.
            unsafe { crate::files::store_path(app_ref, layout.root.clone()) };
        }
    }
    NO_ERR as i16
}

/// `const char *GetMacOSStatusCommentString(OSStatus)`
///
/// Apple's hands back a pointer into a static table. There is no table here, so
/// the number is written into a buffer of this module's own and that is handed
/// back - which is the same contract: a pointer the caller reads and does not
/// free.
///
/// The *comment* is the long form; [`GetMacOSStatusErrorString`] is the short
/// one, and neither has a name for a code this tree does not know. Saying the
/// number is better than naming the wrong error - the trap `darwin::errno`
/// describes, one table over.
pub unsafe extern "C" fn GetMacOSStatusCommentString(status: OSStatus) -> *const c_char {
    described(status)
}

/// `const char *GetMacOSStatusErrorString(OSStatus)`
pub unsafe extern "C" fn GetMacOSStatusErrorString(status: OSStatus) -> *const c_char {
    described(status)
}

/// `OSErr UpdateSystemActivity(UInt8 activity)`
///
/// Tells macOS the user is still there, so it does not dim or sleep. A game
/// calls it every frame. The host's own idle policy is not this process's to
/// reset, so it is answered and dropped.
pub extern "C" fn UpdateSystemActivity(_activity: u8) -> i16 {
    NO_ERR as i16
}

/// The text for `status`, in storage that outlives the call.
///
/// One slot per thread, because the string is read after the call returns and
/// two threads asking at once must not be handed the same buffer.
fn described(status: OSStatus) -> *const c_char {
    const ROOM: usize = 32;
    thread_local! {
        static TEXT: std::cell::RefCell<[u8; ROOM]> = const {
            std::cell::RefCell::new([0; ROOM])
        };
    }
    TEXT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let text = format!("OSStatus {status}\0");
        let bytes = text.as_bytes();
        let used = bytes.len().min(ROOM - 1);
        slot[..used].copy_from_slice(&bytes[..used]);
        slot[used] = 0;
        // SAFETY: the bytes above are NUL-terminated and the storage is this
        // thread's for as long as the thread lives - which is the same promise
        // Apple's static table makes.
        unsafe { CStr::from_ptr(slot.as_ptr().cast()) }.as_ptr()
    })
}

/// `CFStringRef CSCopyMachineName(void)` - the computer's name, owned by the
/// caller. The host name is the nearest thing a non-Mac host has.
pub unsafe extern "C" fn CSCopyMachineName() -> *mut c_void {
    let mut buffer = [0u8; 256];
    // SAFETY: a buffer described by its own length, less one so the result stays
    // NUL-terminated.
    let name = match unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len() - 1) } {
        0 => CStr::from_bytes_until_nul(&buffer)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        _ => String::new(),
    };
    foundation::string::from_str(&name).cast()
}

exports::exports! {
    functions {
        "CSCopyMachineName" => CSCopyMachineName,
        "GetCurrentProcess" => GetCurrentProcess,
        "GetFrontProcess" => GetFrontProcess,
        "SameProcess" => SameProcess,
        "SetFrontProcess" => SetFrontProcess,
        "TransformProcessType" => TransformProcessType,
        "GetProcessBundleLocation" => GetProcessBundleLocation,
        "GetNextProcess" => GetNextProcess,
        "GetProcessInformation" => GetProcessInformation,
        "GetMacOSStatusCommentString" => GetMacOSStatusCommentString,
        "GetMacOSStatusErrorString" => GetMacOSStatusErrorString,
        "UpdateSystemActivity" => UpdateSystemActivity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The number a program gets back is `kCurrentProcess`, and it compares
    /// equal to itself - which is the whole of what a caller does with it.
    #[test]
    fn this_process_names_itself() {
        let mut psn = NO_PROCESS;
        // SAFETY: writable storage for a serial number.
        assert_eq!(unsafe { GetCurrentProcess(&mut psn) }, NO_ERR as i16);
        assert_eq!(psn, CURRENT);
        let mut same = 0_u8;
        // SAFETY: two readable serial numbers and a writable flag.
        assert_eq!(
            unsafe { SameProcess(&psn, &CURRENT, &mut same) },
            NO_ERR as i16
        );
        assert_eq!(same, 1);
        // SAFETY: a readable serial number.
        assert_eq!(unsafe { SetFrontProcess(&psn) }, NO_ERR as i16);
        assert_eq!(unsafe { TransformProcessType(&psn, 1) }, NO_ERR);
    }

    /// Another process is not found rather than silently treated as this one.
    #[test]
    fn another_process_is_not_found() {
        let other = ProcessSerialNumber { high: 0, low: 99 };
        // SAFETY: a readable serial number.
        assert_eq!(unsafe { SetFrontProcess(&other) }, PROC_NOT_FOUND as i16);
        assert_eq!(unsafe { TransformProcessType(&other, 1) }, PROC_NOT_FOUND);
    }

    /// The list is this process alone, which describes itself by its bundle.
    #[test]
    fn the_process_list_holds_this_process() {
        let mut psn = NO_PROCESS;
        // SAFETY: a readable and writable serial number.
        assert_eq!(unsafe { GetNextProcess(&mut psn) }, NO_ERR as i16);
        assert_eq!(psn, CURRENT);
        let mut name = [0u8; 32];
        // SAFETY: an all-zero record is valid; a name buffer of a `Str31`.
        let mut info: ProcessInfoRec = unsafe { std::mem::zeroed() };
        info.length = size_of::<ProcessInfoRec>() as u32;
        info.name = name.as_mut_ptr();
        // SAFETY: a readable serial number and a record of its stated length.
        assert_eq!(
            unsafe { GetProcessInformation(&psn, &mut info) },
            NO_ERR as i16
        );
        assert_eq!({ info.number }, CURRENT);
        assert_eq!(size_of::<ProcessInfoRec>(), 72);
        // SAFETY: as above.
        assert_eq!(unsafe { GetNextProcess(&mut psn) }, PROC_NOT_FOUND as i16);
        assert_eq!(psn, NO_PROCESS);
    }

    /// A status with no name is reported as its number rather than as a
    /// neighbouring error's message.
    #[test]
    fn an_unknown_status_is_reported_as_its_number() {
        // SAFETY: the pointer is this thread's own storage, read before the
        // next call on this thread replaces it.
        let text = unsafe { CStr::from_ptr(GetMacOSStatusCommentString(-43)) };
        assert_eq!(text.to_bytes(), b"OSStatus -43");
    }
}
