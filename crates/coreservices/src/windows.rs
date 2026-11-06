//! The classic Window Manager's `WindowRef` calls.
//!
//! This tree has no `WindowRef` concept at all - `appkit`'s windows are
//! `NSWindow` objects, and nothing anywhere bridges one back to the
//! QuickDraw-era `WindowRecord` these calls expect (a search for
//! `GetNativeWindowFromWindowRef` or a `WindowRef`/`HIWindow` type in this
//! tree finds neither). Every `WindowRef` a guest could pass here is
//! therefore invalid by construction, and every answer below is the one
//! Apple documents for that case: `errInvalidWindowRef` where the call
//! returns a status, `false` for the validity test, and `NULL` for the one
//! call that hands back a `WindowRef` itself.

use std::ffi::c_void;

type OSStatus = i32;

/// `errInvalidWindowRef` (`errInvalidWindowPtr`): "tried to pass a bad
/// WindowRef argument".
const ERR_INVALID_WINDOW_REF: OSStatus = -5600;

/// `Rect { short top; short left; short bottom; short right; }`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub top: i16,
    pub left: i16,
    pub bottom: i16,
    pub right: i16,
}

/// `Boolean IsValidWindowPtr(WindowRef possibleWindow)`
///
/// `false` for every `WindowRef`, since none is ever valid here - see the
/// module note.
pub unsafe extern "C" fn IsValidWindowPtr(_possible_window: *mut c_void) -> u8 {
    0
}

/// `Rect *GetWindowPortBounds(WindowRef window, Rect *bounds)`
///
/// There being no valid window to read bounds from, this writes the empty
/// rectangle a window with nothing drawn in it would have, and returns
/// `bounds` back - the same pointer Apple's own hands back for chaining.
///
/// # Safety
///
/// `bounds` is null or writable for a `Rect`.
pub unsafe extern "C" fn GetWindowPortBounds(_window: *mut c_void, bounds: *mut Rect) -> *mut Rect {
    if !bounds.is_null() {
        // SAFETY: the caller's contract.
        unsafe { bounds.write_unaligned(Rect::default()) };
    }
    bounds
}

/// `OSStatus GetWindowProperty(WindowRef window, PropertyCreator
/// propertyCreator, PropertyTag propertyTag, ByteCount bufferSize, ByteCount
/// *actualSize, void *propertyBuffer)`
pub unsafe extern "C" fn GetWindowProperty(
    _window: *mut c_void,
    _creator: u32,
    _tag: u32,
    _buffer_size: u64,
    _actual_size: *mut u64,
    _buffer: *mut c_void,
) -> OSStatus {
    ERR_INVALID_WINDOW_REF
}

/// `OSStatus SetWindowProperty(WindowRef window, PropertyCreator
/// propertyCreator, PropertyTag propertyTag, ByteCount propertySize, const
/// void *propertyBuffer)`
pub unsafe extern "C" fn SetWindowProperty(
    _window: *mut c_void,
    _creator: u32,
    _tag: u32,
    _size: u64,
    _buffer: *const c_void,
) -> OSStatus {
    ERR_INVALID_WINDOW_REF
}

/// `WindowRef GetDialogWindow(DialogRef dialog)`
///
/// `NULL`: there is no Dialog Manager here either, so `dialog` is never valid
/// any more than a `WindowRef` is, and `NULL` is the honest "no such window"
/// a function returning the reference itself (rather than an `OSStatus`) can
/// give.
pub unsafe extern "C" fn GetDialogWindow(_dialog: *mut c_void) -> *mut c_void {
    std::ptr::null_mut()
}

exports::exports! {
    functions {
        "IsValidWindowPtr" => IsValidWindowPtr,
        "GetWindowPortBounds" => GetWindowPortBounds,
        "GetWindowProperty" => GetWindowProperty,
        "SetWindowProperty" => SetWindowProperty,
        "GetDialogWindow" => GetDialogWindow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_window_reference_is_ever_valid() {
        let fake = std::ptr::without_provenance_mut::<c_void>(0x1234);
        // SAFETY: never dereferenced.
        unsafe {
            assert_eq!(IsValidWindowPtr(fake), 0);
            assert_eq!(IsValidWindowPtr(std::ptr::null_mut()), 0);
        }
    }

    #[test]
    fn window_port_bounds_answers_the_empty_rect() {
        let fake = std::ptr::without_provenance_mut::<c_void>(0x1234);
        let mut bounds = Rect {
            top: 1,
            left: 2,
            bottom: 3,
            right: 4,
        };
        // SAFETY: `bounds` is writable; `fake` is never dereferenced.
        let out = unsafe { GetWindowPortBounds(fake, &mut bounds) };
        assert_eq!(out, &raw mut bounds);
        assert_eq!(
            (bounds.top, bounds.left, bounds.bottom, bounds.right),
            (0, 0, 0, 0)
        );
    }

    #[test]
    fn window_properties_and_the_dialog_window_are_refused() {
        let fake = std::ptr::without_provenance_mut::<c_void>(0x1234);
        // SAFETY: never dereferenced.
        unsafe {
            assert_eq!(
                GetWindowProperty(fake, 0, 0, 0, std::ptr::null_mut(), std::ptr::null_mut()),
                ERR_INVALID_WINDOW_REF
            );
            assert_eq!(
                SetWindowProperty(fake, 0, 0, 0, std::ptr::null()),
                ERR_INVALID_WINDOW_REF
            );
            assert!(GetDialogWindow(fake).is_null());
        }
    }
}
