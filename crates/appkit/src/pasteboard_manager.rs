// The C spellings are the interface.
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::Mutex;

use corefoundation::base::{CFIndex, CFTypeRef};

/// `OSStatus`. Apple's values, because a caller compares against them.
mod status {
    pub const NO_ERR: i32 = 0;
    pub const PARAM_ERR: i32 = -50;
    /// `badPasteboardItemErr`, for an item identifier no board here has.
    pub const BAD_ITEM: i32 = -25132;
    /// `badPasteboardFlavorErr`, for a flavour the board does not hold. This is
    /// the one a caller sees most: it is how "nothing of that type on the
    /// clipboard" is spelled, and it is not a failure.
    pub const BAD_FLAVOR: i32 = -25133;
    /// `badPasteboardIndexErr`, for an item index outside the board.
    pub const BAD_INDEX: i32 = -25131;
}

/// `PasteboardSyncFlags`.
mod sync_flags {
    /// Something outside this process changed the board since the last sync.
    pub const MODIFIED: u32 = 1;
    /// This process put the current contents there.
    pub const CLIENT_IS_OWNER: u32 = 2;
}

/// The one item identifier a board here has. See the module note.
const ITEM: usize = 1;

/// `kPasteboardClipboard`, the name a caller passes for the system clipboard.
const CLIPBOARD_NAME: &str = "com.apple.pasteboard.clipboard";

/// One `PasteboardRef` the guest holds.
struct Pasteboard {
    /// Which board in [`crate::pasteboard`] this handle names.
    board: Vec<u8>,
    /// The name the handle was created with, for `PasteboardCopyName`.
    name: String,
    /// The change count at the last `PasteboardSynchronize`, which is what makes
    /// `kPasteboardModified` mean "since you last asked" rather than "ever".
    seen: Mutex<isize>,
    /// Whether this handle wrote the current contents, for
    /// `kPasteboardClientIsOwner`.
    owner: Mutex<bool>,
    /// What `PasteboardSetPromiseKeeper` was given, recorded and never called -
    /// see [`PasteboardResolvePromises`].
    keeper: Mutex<(usize, usize)>,
}

/// The handles handed out, by address. Leaked: a `PasteboardRef` is released with
/// `CFRelease`, and a clipboard lives as long as the program that opened it.
fn boards() -> &'static Mutex<Vec<(usize, &'static Pasteboard)>> {
    static BOARDS: Mutex<Vec<(usize, &'static Pasteboard)>> = Mutex::new(Vec::new());
    &BOARDS
}

fn board_of(handle: *const c_void) -> Option<&'static Pasteboard> {
    let boards = boards().lock().unwrap_or_else(|e| e.into_inner());
    boards
        .iter()
        .find(|(address, _)| *address == handle as usize)
        .map(|(_, board)| *board)
}

/// The text of a `CFStringRef`.
///
/// # Safety
///
/// `value` is null or an object this runtime allocated.
unsafe fn text(value: CFTypeRef) -> Option<Vec<u8>> {
    unsafe { corefoundation::string::as_string(value) }.map(|s| s.utf8())
}

/// `OSStatus PasteboardCreate(CFStringRef inName, PasteboardRef *outPasteboard)`
///
/// A null name is the system clipboard, which is what `kPasteboardClipboard`
/// names too - so both land on the general board. Any other name is a board of
/// its own, process-local, exactly as a uniquely named `NSPasteboard` is.
///
/// # Safety
///
/// `name` is null or a `CFStringRef`, and `out` is null or writable for one
/// pointer.
pub unsafe extern "C" fn PasteboardCreate(name: CFTypeRef, out: *mut *const c_void) -> i32 {
    if out.is_null() {
        return status::PARAM_ERR;
    }
    let spelled = unsafe { text(name) }.unwrap_or_default();
    let spelled = String::from_utf8_lossy(&spelled).into_owned();
    let board = match spelled.is_empty() || spelled == CLIPBOARD_NAME {
        true => crate::pasteboard::CLIPBOARD.to_vec(),
        false => spelled.as_bytes().to_vec(),
    };
    let name = match spelled.is_empty() {
        true => CLIPBOARD_NAME.to_string(),
        false => spelled,
    };
    let leaked: &'static Pasteboard = Box::leak(Box::new(Pasteboard {
        board,
        name,
        seen: Mutex::new(0),
        owner: Mutex::new(false),
        keeper: Mutex::new((0, 0)),
    }));
    let handle = leaked as *const Pasteboard as *const c_void;
    // A `PasteboardRef` is a CoreFoundation type a caller may `CFRelease`, and it
    // is not an object - so the runtime has to be told, or retaining one reads
    // this struct's first word as an `isa`.
    corefoundation::base::note_opaque(handle);
    boards()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((handle as usize, leaked));
    // SAFETY: the caller states `out` is writable.
    unsafe { *out = handle };
    status::NO_ERR
}

/// `OSStatus PasteboardClear(PasteboardRef)`
///
/// Clearing is also how a caller takes ownership: everything after it is this
/// process's contents, which is what `kPasteboardClientIsOwner` reports.
pub unsafe extern "C" fn PasteboardClear(handle: *const c_void) -> i32 {
    let Some(board) = board_of(handle) else {
        return status::PARAM_ERR;
    };
    let count = crate::pasteboard::clear_named(&board.board);
    *board.seen.lock().unwrap_or_else(|e| e.into_inner()) = count;
    *board.owner.lock().unwrap_or_else(|e| e.into_inner()) = true;
    status::NO_ERR
}

/// `PasteboardSyncFlags PasteboardSynchronize(PasteboardRef)`
///
/// `kPasteboardModified` means "changed since you last asked", which is what a
/// caller polling the clipboard acts on: Qt re-reads the board only when this
/// says so, so an answer of zero every time is a clipboard that never appears to
/// change and an answer of `MODIFIED` every time is a re-read on every poll.
pub unsafe extern "C" fn PasteboardSynchronize(handle: *const c_void) -> u32 {
    let Some(board) = board_of(handle) else {
        return 0;
    };
    let count = crate::pasteboard::change_count_of(&board.board);
    let mut seen = board.seen.lock().unwrap_or_else(|e| e.into_inner());
    let modified = count != *seen;
    *seen = count;
    if modified {
        // A change this handle did not make means somebody else owns what is
        // there now.
        *board.owner.lock().unwrap_or_else(|e| e.into_inner()) = false;
    }
    let mut flags = 0;
    if modified {
        flags |= sync_flags::MODIFIED;
    }
    if *board.owner.lock().unwrap_or_else(|e| e.into_inner()) {
        flags |= sync_flags::CLIENT_IS_OWNER;
    }
    flags
}

/// `OSStatus PasteboardGetItemCount(PasteboardRef, ItemCount *outItemCount)`
///
/// One item when the board holds anything and none when it is empty - see the
/// module note on why it is never more.
///
/// # Safety
///
/// `out` is null or writable for one `ItemCount`.
pub unsafe extern "C" fn PasteboardGetItemCount(handle: *const c_void, out: *mut usize) -> i32 {
    let Some(board) = board_of(handle) else {
        return status::PARAM_ERR;
    };
    if out.is_null() {
        return status::PARAM_ERR;
    }
    let count = !crate::pasteboard::flavors(&board.board).is_empty() as usize;
    // SAFETY: the caller states `out` is writable.
    unsafe { *out = count };
    status::NO_ERR
}

/// `OSStatus PasteboardGetItemIdentifier(PasteboardRef, CFIndex inIndex,
/// PasteboardItemID *outItem)`
///
/// The index is **one-based**, which is the Pasteboard Manager's convention and
/// the one a caller's loop is written against.
///
/// # Safety
///
/// `out` is null or writable for one pointer.
pub unsafe extern "C" fn PasteboardGetItemIdentifier(
    handle: *const c_void,
    index: CFIndex,
    out: *mut usize,
) -> i32 {
    let Some(board) = board_of(handle) else {
        return status::PARAM_ERR;
    };
    if out.is_null() {
        return status::PARAM_ERR;
    }
    if index != 1 || crate::pasteboard::flavors(&board.board).is_empty() {
        return status::BAD_INDEX;
    }
    // SAFETY: the caller states `out` is writable.
    unsafe { *out = ITEM };
    status::NO_ERR
}

/// `OSStatus PasteboardCopyItemFlavors(PasteboardRef, PasteboardItemID,
/// CFArrayRef *outFlavorTypes)`
///
/// # Safety
///
/// `out` is null or writable for one pointer.
pub unsafe extern "C" fn PasteboardCopyItemFlavors(
    handle: *const c_void,
    item: usize,
    out: *mut CFTypeRef,
) -> i32 {
    let Some(board) = board_of(handle) else {
        return status::PARAM_ERR;
    };
    if out.is_null() {
        return status::PARAM_ERR;
    }
    if item != ITEM {
        return status::BAD_ITEM;
    }
    let flavors = crate::pasteboard::flavors(&board.board);
    let strings: Vec<CFTypeRef> = flavors
        .iter()
        .map(|ty| corefoundation::string::from_bytes(ty, false))
        .collect();
    let array = corefoundation::collections::new_array(
        &strings,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for string in strings {
        // The array took its own reference; this one is this call's.
        // SAFETY: a live object made immediately above.
        unsafe { corefoundation::api::CFRelease(string) };
    }
    // SAFETY: the caller states `out` is writable, and a `Copy` hands the
    // reference over.
    unsafe { *out = array };
    status::NO_ERR
}

/// `OSStatus PasteboardCopyItemFlavorData(PasteboardRef, PasteboardItemID,
/// CFStringRef inFlavorType, CFDataRef *outData)`
///
/// # Safety
///
/// `flavor` is null or a `CFStringRef`, and `out` is null or writable for one
/// pointer.
pub unsafe extern "C" fn PasteboardCopyItemFlavorData(
    handle: *const c_void,
    item: usize,
    flavor: CFTypeRef,
    out: *mut CFTypeRef,
) -> i32 {
    let Some(board) = board_of(handle) else {
        return status::PARAM_ERR;
    };
    if out.is_null() {
        return status::PARAM_ERR;
    }
    if item != ITEM {
        return status::BAD_ITEM;
    }
    let Some(flavor) = (unsafe { text(flavor) }) else {
        return status::PARAM_ERR;
    };
    let Some(bytes) = crate::pasteboard::flavor_data(&board.board, &flavor) else {
        return status::BAD_FLAVOR;
    };
    // SAFETY: the caller states `out` is writable.
    unsafe { *out = corefoundation::collections::new_data(bytes) };
    status::NO_ERR
}

/// `OSStatus PasteboardPutItemFlavor(PasteboardRef, PasteboardItemID,
/// CFStringRef inFlavorType, CFDataRef inData, PasteboardFlavorFlags)`
///
/// The item identifier is not honoured, for the reason the module note gives: a
/// board here has one item and every flavour belongs to it.
///
/// # Safety
///
/// `flavor` and `data` are null or objects this runtime allocated.
pub unsafe extern "C" fn PasteboardPutItemFlavor(
    handle: *const c_void,
    _item: usize,
    flavor: CFTypeRef,
    data: CFTypeRef,
    _flags: u32,
) -> i32 {
    let Some(board) = board_of(handle) else {
        return status::PARAM_ERR;
    };
    let Some(flavor) = (unsafe { text(flavor) }) else {
        return status::PARAM_ERR;
    };
    // SAFETY: as above - a `CFData` or null.
    let bytes = match unsafe { corefoundation::collections::data(data) } {
        Some(bytes) => bytes.clone(),
        // A null `CFData` is how a caller *promises* the flavour: the data is to
        // be asked for later, through the promise keeper. Recorded as empty, and
        // see [`PasteboardResolvePromises`].
        None => Vec::new(),
    };
    crate::pasteboard::put_flavor(&board.board, &flavor, bytes);
    *board.owner.lock().unwrap_or_else(|e| e.into_inner()) = true;
    *board.seen.lock().unwrap_or_else(|e| e.into_inner()) =
        crate::pasteboard::change_count_of(&board.board);
    status::NO_ERR
}

/// `OSStatus PasteboardGetItemFlavorFlags(PasteboardRef, PasteboardItemID,
/// CFStringRef, PasteboardFlavorFlags *outFlags)`
///
/// Zero flags - no promise, not sender-only, not translated - for a flavour the
/// board holds, and `badPasteboardFlavorErr` for one it does not. The flags are
/// what a caller checks to decide whether reading the data is free; here it
/// always is, because the data is already in memory.
///
/// # Safety
///
/// `out` is null or writable for one `PasteboardFlavorFlags`.
pub unsafe extern "C" fn PasteboardGetItemFlavorFlags(
    handle: *const c_void,
    item: usize,
    flavor: CFTypeRef,
    out: *mut u32,
) -> i32 {
    let Some(board) = board_of(handle) else {
        return status::PARAM_ERR;
    };
    if item != ITEM {
        return status::BAD_ITEM;
    }
    let Some(flavor) = (unsafe { text(flavor) }) else {
        return status::PARAM_ERR;
    };
    if !crate::pasteboard::flavors(&board.board).contains(&flavor) {
        return status::BAD_FLAVOR;
    }
    if !out.is_null() {
        // SAFETY: the caller states `out` is writable.
        unsafe { *out = 0 };
    }
    status::NO_ERR
}

/// `OSStatus PasteboardCopyName(PasteboardRef, CFStringRef *outName)`
///
/// # Safety
///
/// `out` is null or writable for one pointer.
pub unsafe extern "C" fn PasteboardCopyName(handle: *const c_void, out: *mut CFTypeRef) -> i32 {
    let Some(board) = board_of(handle) else {
        return status::PARAM_ERR;
    };
    if out.is_null() {
        return status::PARAM_ERR;
    }
    // SAFETY: the caller states `out` is writable, and a `Copy` hands the
    // reference over.
    unsafe { *out = corefoundation::string::from_bytes(board.name.as_bytes(), false) };
    status::NO_ERR
}

/// `OSStatus PasteboardSetPromiseKeeper(PasteboardRef,
/// PasteboardPromiseKeeperProcPtr, void *inContext)`
///
/// Recorded and not called. A promise keeper exists so a large flavour - a
/// rendered image, a file's contents - is produced only if somebody pastes it,
/// and [`PasteboardPutItemFlavor`] here always stores the bytes it was given, so
/// nothing is ever outstanding. Accepting the registration is still right: a
/// caller that could not set one would take the "promises unsupported" branch of
/// its own code, which is a drag that offers nothing at all.
pub unsafe extern "C" fn PasteboardSetPromiseKeeper(
    handle: *const c_void,
    keeper: *const c_void,
    context: *mut c_void,
) -> i32 {
    let Some(board) = board_of(handle) else {
        return status::PARAM_ERR;
    };
    *board.keeper.lock().unwrap_or_else(|e| e.into_inner()) = (keeper as usize, context as usize);
    status::NO_ERR
}

/// `OSStatus PasteboardResolvePromises(PasteboardRef)`
///
/// Nothing to resolve - see [`PasteboardSetPromiseKeeper`] - so this succeeds.
/// Failing would be the wrong answer: a caller reads it as "the drag could not be
/// completed" and abandons the contents it had just put there.
pub unsafe extern "C" fn PasteboardResolvePromises(handle: *const c_void) -> i32 {
    match board_of(handle).is_some() {
        true => status::NO_ERR,
        false => status::PARAM_ERR,
    }
}

/// `void NSPerformService(NSPasteboardName pboardName, NSArray *objects)`
///
/// Refused, and no drag is started.
///
/// The function begins a drag-and-drop session carrying `objects` on the named
/// pasteboard. A drag session needs a window server to track the mouse against, a
/// pasteboard server to promise the data on, and a run loop to pump the session's
/// events; none of those exists here, and a "drag" that started and immediately
/// ended would tell the guest a drag happened when none did.
///
/// The guest's one call site shows that this is the tail of a decision it has
/// already made and that it reads nothing back (`0x3aaab06`):
///
/// ```text
/// 3aaaacd  call objc_msgSend             ; -[delegate writeObjects:items]
/// 3aaaad3  mov  BYTE PTR [rbp-0x39], al
/// 3aaaae9  cmp  BYTE PTR [rbp-0x39], 0
/// 3aaaeed  je   0x3aaab14                ; not written: no drag at all
/// 3aaaafc  lea  rdi, [0xe672b10]         ; the NSPasteboardName
/// 3aaab03  mov  rsi, rax                 ; the items
/// 3aaab06  call _NSPerformService
/// 3aaab0b  call _objc_release            ; and the return is discarded
/// ```
///
/// So refusing here leaves the guest exactly where a board that could not start a
/// drag would leave it: the delegate already said it wrote the objects, and the
/// drag does not follow. The function is `void` with no out-parameter, so there is
/// nothing to write on a refusal - which is the rule this project keeps, and the
/// reason it is worth stating that none was needed.
#[allow(non_snake_case)]
pub extern "C" fn NSPerformService(_pboard_name: *const c_void, _objects: *const c_void) {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if ONCE.set(()).is_ok() {
        eprintln!(
            "rosetta3: NSPerformService: refused - a drag session needs a window server, a \
             pasteboard server and a run loop to pump it, and this runtime has none of the \
             three; no drag was started, and the guest reads no result (0x3aaab0b)"
        );
    }
}

exports::exports! {
    functions {
        "PasteboardCreate" => PasteboardCreate,
        "PasteboardClear" => PasteboardClear,
        "PasteboardSynchronize" => PasteboardSynchronize,
        "PasteboardGetItemCount" => PasteboardGetItemCount,
        "PasteboardGetItemIdentifier" => PasteboardGetItemIdentifier,
        "PasteboardCopyItemFlavors" => PasteboardCopyItemFlavors,
        "PasteboardCopyItemFlavorData" => PasteboardCopyItemFlavorData,
        "PasteboardPutItemFlavor" => PasteboardPutItemFlavor,
        "PasteboardGetItemFlavorFlags" => PasteboardGetItemFlavorFlags,
        "PasteboardCopyName" => PasteboardCopyName,
        "PasteboardSetPromiseKeeper" => PasteboardSetPromiseKeeper,
        "PasteboardResolvePromises" => PasteboardResolvePromises,
        "NSPerformService" => NSPerformService,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(text: &str) -> CFTypeRef {
        corefoundation::string::from_bytes(text.as_bytes(), false)
    }

    /// A board of this test's own name.
    ///
    /// Named per test rather than shared: a board is process-global - that is its
    /// documented identity - so two tests on one name race, and the desktop's
    /// clipboard is not a board a test may disturb at all.
    fn private_board(which: &str) -> *const c_void {
        crate::bootstrap();
        let mut handle: *const c_void = std::ptr::null();
        // SAFETY: a string this test made and a live out-parameter.
        let rc = unsafe { PasteboardCreate(string(&format!("r3.test.{which}")), &raw mut handle) };
        assert_eq!(rc, status::NO_ERR);
        assert!(!handle.is_null());
        handle
    }

    /// The round trip Qt's clipboard is made of: clear, put a flavour, then
    /// enumerate and read it back.
    #[test]
    fn a_flavour_reads_back_under_the_one_item() {
        let pb = private_board("round-trip");
        let flavor = string("public.utf8-plain-text");
        // SAFETY: a live handle, a string this test made, and live
        // out-parameters throughout.
        unsafe {
            assert_eq!(PasteboardClear(pb), status::NO_ERR);
            let mut count = 99usize;
            assert_eq!(PasteboardGetItemCount(pb, &raw mut count), status::NO_ERR);
            assert_eq!(count, 0, "a cleared board has no items");

            let data = corefoundation::collections::new_data(b"hello".to_vec());
            assert_eq!(
                PasteboardPutItemFlavor(pb, ITEM, flavor, data, 0),
                status::NO_ERR
            );
            corefoundation::api::CFRelease(data);

            assert_eq!(PasteboardGetItemCount(pb, &raw mut count), status::NO_ERR);
            assert_eq!(count, 1);
            let mut item = 0usize;
            assert_eq!(
                PasteboardGetItemIdentifier(pb, 1, &raw mut item),
                status::NO_ERR
            );
            assert_eq!(item, ITEM);

            let mut flavors: CFTypeRef = std::ptr::null();
            assert_eq!(
                PasteboardCopyItemFlavors(pb, item, &raw mut flavors),
                status::NO_ERR
            );
            let listed = corefoundation::collections::array(flavors).expect("an array of flavours");
            assert_eq!(listed.values.len(), 1);
            corefoundation::api::CFRelease(flavors);

            let mut out: CFTypeRef = std::ptr::null();
            assert_eq!(
                PasteboardCopyItemFlavorData(pb, item, flavor, &raw mut out),
                status::NO_ERR
            );
            let bytes = corefoundation::collections::data(out).expect("the flavour's data");
            assert_eq!(bytes.as_slice(), b"hello");
            corefoundation::api::CFRelease(out);
        }
    }

    /// A flavour nothing wrote is `badPasteboardFlavorErr`, which is how "not on
    /// the clipboard" is spelled - and is the answer a caller branches on.
    #[test]
    fn an_absent_flavour_is_refused_rather_than_empty() {
        let pb = private_board("absent");
        unsafe {
            assert_eq!(PasteboardClear(pb), status::NO_ERR);
            let mut out: CFTypeRef = std::ptr::null();
            assert_eq!(
                PasteboardCopyItemFlavorData(pb, ITEM, string("public.tiff"), &raw mut out),
                status::BAD_FLAVOR
            );
            assert!(out.is_null());
            let mut flags = 7u32;
            assert_eq!(
                PasteboardGetItemFlavorFlags(pb, ITEM, string("public.tiff"), &raw mut flags),
                status::BAD_FLAVOR
            );
        }
    }

    /// `kPasteboardModified` has to mean "since you last asked": a caller that
    /// saw it every time would re-read the clipboard on every poll.
    #[test]
    fn synchronize_reports_a_change_once() {
        let pb = private_board("sync");
        unsafe {
            PasteboardClear(pb);
            PasteboardSynchronize(pb);
            assert_eq!(
                PasteboardSynchronize(pb) & sync_flags::MODIFIED,
                0,
                "nothing changed between the two calls"
            );
            let data = corefoundation::collections::new_data(b"x".to_vec());
            PasteboardPutItemFlavor(pb, ITEM, string("public.utf8-plain-text"), data, 0);
            corefoundation::api::CFRelease(data);
            // This process wrote it, so it owns the contents.
            assert_ne!(
                PasteboardSynchronize(pb) & sync_flags::CLIENT_IS_OWNER,
                0,
                "this process put the contents there"
            );
        }
    }

    /// The clipboard handle and `+[NSPasteboard generalPasteboard]` must be one
    /// board, or a program that copies through one and pastes through the other
    /// loses what it copied.
    #[test]
    fn the_clipboard_handle_is_the_general_pasteboard() {
        crate::bootstrap();
        let mut handle: *const c_void = std::ptr::null();
        // SAFETY: a live out-parameter; a null name is the system clipboard.
        unsafe { PasteboardCreate(std::ptr::null(), &raw mut handle) };
        let board = board_of(handle).expect("the handle just created");
        assert_eq!(board.board, crate::pasteboard::CLIPBOARD);
    }

    /// `NSPerformService` is bound and starts no drag.
    ///
    /// The assertion is that the shape is callable and that the board it was
    /// handed is unchanged: a test that only checked the symbol was published
    /// would pass on an implementation that started a session, and there is no
    /// session to start here.
    ///
    /// A **private** board, named by its own name. The system clipboard is
    /// process-global and two other tests in this suite write to it, so an
    /// assertion about it is a race - which is exactly what the first version of
    /// this test did, and it failed for that reason and no other.
    #[test]
    fn a_drag_is_refused_rather_than_started() {
        crate::bootstrap();
        let pb = private_board("drag-refused");
        let board = board_of(pb).expect("the handle just created");
        let name = string(&board.name);
        let items = corefoundation::collections::new_array(
            &[string("public.utf8-plain-text")],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        );
        let mut before = 99usize;
        // SAFETY: a live handle and out-parameter.
        unsafe {
            assert_eq!(PasteboardGetItemCount(pb, &raw mut before), status::NO_ERR);
        }
        // SAFETY: both are objects this test made; the function is `void` and
        // reads neither.
        unsafe { NSPerformService(name, items) };
        // Nothing was taken off the board the guest named, which is what "no drag"
        // means for it.
        let mut after = 99usize;
        // SAFETY: a live handle and out-parameter.
        unsafe {
            assert_eq!(PasteboardGetItemCount(pb, &raw mut after), status::NO_ERR);
            assert_eq!(
                after, before,
                "a refused drag took nothing off {}",
                board.name
            );
        }
    }
}
