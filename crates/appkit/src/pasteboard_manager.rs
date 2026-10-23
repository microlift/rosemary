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
const ITEM: u32 = 1;
const PROMISED: u32 = 512;

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
    /// The callback and context used to produce promised flavors.
    keeper: Mutex<(usize, usize)>,
}

type PromiseKeeperProc = unsafe extern "C" fn(*const c_void, u32, CFTypeRef, *mut c_void) -> i32;

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

fn is_text_flavor(flavor: &[u8]) -> bool {
    matches!(
        flavor,
        b"public.utf8-plain-text"
            | b"public.utf16-plain-text"
            | b"public.text"
            | b"com.apple.traditional-mac-plain-text"
            | b"com.trolltech.anymime.text--plain"
    )
}

fn invoke_promise_keeper(
    handle: *const c_void,
    board: &'static Pasteboard,
    item: u32,
    flavor: &[u8],
) -> Result<(), i32> {
    if !crate::pasteboard::flavor_is_promised(&board.board, flavor) {
        return Ok(());
    }
    let (keeper, context) = *board.keeper.lock().unwrap_or_else(|e| e.into_inner());
    if keeper == 0 {
        return Err(status::BAD_FLAVOR);
    }
    let flavor_ref = corefoundation::string::from_bytes(flavor, false);
    // SAFETY: PasteboardSetPromiseKeeper receives this callback with the
    // platform-defined PasteboardPromiseKeeperProcPtr signature.
    let callback: PromiseKeeperProc = unsafe { std::mem::transmute(keeper) };
    let result = unsafe { callback(handle, item, flavor_ref, context as *mut c_void) };
    unsafe { corefoundation::api::CFRelease(flavor_ref) };
    let materialized = crate::pasteboard::flavor_data(&board.board, flavor).is_some();
    if result != status::NO_ERR {
        return Err(result);
    }
    if materialized {
        Ok(())
    } else {
        Err(status::BAD_FLAVOR)
    }
}

fn resolve_exportable_text_promises(handle: *const c_void, board: &'static Pasteboard) {
    for flavor in crate::pasteboard::promised_flavors(&board.board) {
        if is_text_flavor(&flavor) {
            let _ = invoke_promise_keeper(handle, board, ITEM, &flavor);
        }
    }
}

fn resolve_all_promises(handle: *const c_void, board: &'static Pasteboard) -> i32 {
    for flavor in crate::pasteboard::promised_flavors(&board.board) {
        if let Err(result) = invoke_promise_keeper(handle, board, ITEM, &flavor) {
            return result;
        }
    }
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
    let previous = *seen;
    let modified = count != previous;
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
    let owner = *board.owner.lock().unwrap_or_else(|e| e.into_inner());
    if owner {
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
pub unsafe extern "C" fn PasteboardGetItemCount(handle: *const c_void, out: *mut u32) -> i32 {
    let Some(board) = board_of(handle) else {
        return status::PARAM_ERR;
    };
    if out.is_null() {
        return status::PARAM_ERR;
    }
    let count = !crate::pasteboard::flavors(&board.board).is_empty() as u32;
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
/// `out` is null or writable for one `PasteboardItemID`.
pub unsafe extern "C" fn PasteboardGetItemIdentifier(
    handle: *const c_void,
    index: CFIndex,
    out: *mut u32,
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
    item: u32,
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
    item: u32,
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
    let mut bytes = crate::pasteboard::flavor_data(&board.board, &flavor);
    if bytes.is_none() && crate::pasteboard::flavor_is_promised(&board.board, &flavor) {
        let _ = invoke_promise_keeper(handle, board, item, &flavor);
        bytes = crate::pasteboard::flavor_data(&board.board, &flavor);
    }
    let Some(bytes) = bytes else {
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
    item: u32,
    flavor: CFTypeRef,
    data: CFTypeRef,
    _flags: u32,
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
    let promised = data.is_null();
    if promised {
        crate::pasteboard::put_promised_flavor(&board.board, &flavor);
    } else {
        let Some(bytes) = (unsafe { corefoundation::collections::data(data) }) else {
            return status::PARAM_ERR;
        };
        let bytes = bytes.clone();
        crate::pasteboard::put_flavor(&board.board, &flavor, bytes);
    }
    *board.owner.lock().unwrap_or_else(|e| e.into_inner()) = true;
    *board.seen.lock().unwrap_or_else(|e| e.into_inner()) =
        crate::pasteboard::local_change_count_of(&board.board);
    if promised && is_text_flavor(&flavor) {
        let handle = handle;
        resolve_exportable_text_promises(handle, board);
    }
    status::NO_ERR
}

/// `OSStatus PasteboardGetItemFlavorFlags(PasteboardRef, PasteboardItemID,
/// CFStringRef, PasteboardFlavorFlags *outFlags)`
///
/// Returns `kPasteboardFlavorPromised` until the promise keeper materializes the
/// flavor, and zero afterward.
///
/// # Safety
///
/// `out` is null or writable for one `PasteboardFlavorFlags`.
pub unsafe extern "C" fn PasteboardGetItemFlavorFlags(
    handle: *const c_void,
    item: u32,
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
    if out.is_null() {
        return status::PARAM_ERR;
    }
    let flags = if crate::pasteboard::flavor_is_promised(&board.board, &flavor) {
        PROMISED
    } else {
        0
    };
    unsafe { *out = flags };
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
/// Registers the callback used to materialize flavors passed with null data.
pub unsafe extern "C" fn PasteboardSetPromiseKeeper(
    handle: *const c_void,
    keeper: *const c_void,
    context: *mut c_void,
) -> i32 {
    let Some(board) = board_of(handle) else {
        return status::PARAM_ERR;
    };
    *board.keeper.lock().unwrap_or_else(|e| e.into_inner()) = (keeper as usize, context as usize);
    resolve_exportable_text_promises(handle, board);
    status::NO_ERR
}

/// `OSStatus PasteboardResolvePromises(PasteboardRef)`
///
/// Materializes every outstanding flavor by calling its promise keeper.
pub unsafe extern "C" fn PasteboardResolvePromises(handle: *const c_void) -> i32 {
    let Some(board) = board_of(handle) else {
        return status::PARAM_ERR;
    };
    resolve_all_promises(handle, board)
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
        let rc =
            unsafe { PasteboardCreate(string(&format!("rosemary.test.{which}")), &raw mut handle) };
        assert_eq!(rc, status::NO_ERR);
        assert!(!handle.is_null());
        handle
    }

    unsafe extern "C" fn materialize_text(
        handle: *const c_void,
        item: u32,
        flavor: CFTypeRef,
        context: *mut c_void,
    ) -> i32 {
        let calls = unsafe { &mut *(context as *mut usize) };
        *calls += 1;
        let data = corefoundation::collections::new_data(b"promised text".to_vec());
        let result = unsafe { PasteboardPutItemFlavor(handle, item, flavor, data, 0) };
        unsafe { corefoundation::api::CFRelease(data) };
        result
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
            let mut count = 99u32;
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
            let mut item = 0u32;
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

    #[test]
    fn promised_text_is_materialized_by_its_keeper() {
        let pb = private_board("promised-text");
        let flavor = string("public.utf8-plain-text");
        let mut calls = 0usize;
        unsafe {
            assert_eq!(PasteboardClear(pb), status::NO_ERR);
            assert_eq!(
                PasteboardPutItemFlavor(pb, ITEM, flavor, std::ptr::null(), 0),
                status::NO_ERR
            );

            let mut flags = 0;
            assert_eq!(
                PasteboardGetItemFlavorFlags(pb, ITEM, flavor, &raw mut flags),
                status::NO_ERR
            );
            assert_eq!(flags, PROMISED);

            assert_eq!(
                PasteboardSetPromiseKeeper(
                    pb,
                    materialize_text as *const () as usize as *const c_void,
                    &raw mut calls as *mut c_void,
                ),
                status::NO_ERR
            );
            assert_eq!(calls, 1);

            let mut data: CFTypeRef = std::ptr::null();
            assert_eq!(
                PasteboardCopyItemFlavorData(pb, ITEM, flavor, &raw mut data),
                status::NO_ERR
            );
            assert_eq!(
                corefoundation::collections::data(data).unwrap(),
                b"promised text"
            );
            corefoundation::api::CFRelease(data);

            assert_eq!(
                PasteboardGetItemFlavorFlags(pb, ITEM, flavor, &raw mut flags),
                status::NO_ERR
            );
            assert_eq!(flags, 0);
            assert_eq!(PasteboardResolvePromises(pb), status::NO_ERR);
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn resolve_promises_materializes_non_text_flavors_on_demand() {
        let pb = private_board("resolve-promise");
        let flavor = string("public.tiff");
        let mut calls = 0usize;
        unsafe {
            assert_eq!(PasteboardClear(pb), status::NO_ERR);
            assert_eq!(
                PasteboardPutItemFlavor(pb, ITEM, flavor, std::ptr::null(), 0),
                status::NO_ERR
            );
            assert_eq!(
                PasteboardSetPromiseKeeper(
                    pb,
                    materialize_text as *const () as usize as *const c_void,
                    &raw mut calls as *mut c_void,
                ),
                status::NO_ERR
            );
            assert_eq!(calls, 0);
            assert_eq!(PasteboardResolvePromises(pb), status::NO_ERR);
            assert_eq!(calls, 1);

            let mut data: CFTypeRef = std::ptr::null();
            assert_eq!(
                PasteboardCopyItemFlavorData(pb, ITEM, flavor, &raw mut data),
                status::NO_ERR
            );
            assert_eq!(
                corefoundation::collections::data(data).unwrap(),
                b"promised text"
            );
            corefoundation::api::CFRelease(data);
        }
    }

    #[test]
    fn pasteboard_item_count_uses_the_32_bit_carbon_abi() {
        let pb = private_board("item-count-abi");
        let mut storage = 0xAABB_CCDD_1122_3344u64;
        unsafe {
            assert_eq!(PasteboardClear(pb), status::NO_ERR);
            let data = corefoundation::collections::new_data(b"x".to_vec());
            assert_eq!(
                PasteboardPutItemFlavor(pb, ITEM, string("public.utf8-plain-text"), data, 0,),
                status::NO_ERR
            );
            corefoundation::api::CFRelease(data);
            assert_eq!(
                PasteboardGetItemCount(pb, (&raw mut storage).cast::<u32>()),
                status::NO_ERR
            );
        }
        assert_eq!(storage, 0xAABB_CCDD_0000_0001);
    }
}
