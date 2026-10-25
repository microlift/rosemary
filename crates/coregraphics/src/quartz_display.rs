//! The display and cursor calls that act on the process-wide pointer.

#![allow(non_snake_case)]

use std::sync::OnceLock;

use corefoundation::base::CFTypeRef;

use crate::CGPoint;

/// What AppKit provides to the display services: the process-wide cursor's
/// visibility and whether the pointer is associated with it.
pub trait Provider: Send + Sync {
    fn hide_cursor(&self);
    fn show_cursor(&self);
    fn cursor_visible(&self) -> bool;
    fn associate(&self, connected: bool);
}

static PROVIDER: OnceLock<Box<dyn Provider>> = OnceLock::new();

/// Install the provider AppKit answers cursor and association queries with.
pub fn set_provider(provider: Box<dyn Provider>) {
    let _ = PROVIDER.set(provider);
}

/// `CGSessionCopyCurrentDictionary`.
///
/// There is no WindowServer login session on this host. An empty dictionary is
/// still preferable to null: it represents a local session with none of the
/// optional flags (notably `kCGSSessionRemote`) set, which is the environment
/// the SDL-backed application actually runs in.
pub unsafe extern "C" fn CGSessionCopyCurrentDictionary() -> CFTypeRef {
    corefoundation::collections::new_dictionary(
        &[],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    )
}

/// `CGError CGDisplayHideCursor(CGDirectDisplayID)`.
///
/// SDL has one process-wide pointer rather than one per display, matching the
/// effective behavior of CoreGraphics' counted hide level. The display id is
/// consequently immaterial; games commonly pass `kCGNullDirectDisplay` here.
pub unsafe extern "C" fn CGDisplayHideCursor(_display: u32) -> i32 {
    if let Some(provider) = PROVIDER.get() {
        provider.hide_cursor();
    }
    0
}

/// `CGError CGDisplayShowCursor(CGDirectDisplayID)`.
pub unsafe extern "C" fn CGDisplayShowCursor(_display: u32) -> i32 {
    if let Some(provider) = PROVIDER.get() {
        provider.show_cursor();
    }
    0
}

pub unsafe extern "C" fn CGCursorIsVisible() -> u32 {
    // `boolean_t` is an `int`.
    PROVIDER
        .get()
        .is_some_and(|provider| provider.cursor_visible()) as u32
}

/// `CGError CGWarpMouseCursorPosition(CGPoint)` - move the pointer, in global
/// screen coordinates.
pub unsafe extern "C" fn CGWarpMouseCursorPosition(point: CGPoint) -> i32 {
    crate::quartz_event::warp_pointer(point);
    0
}

/// `CGError CGAssociateMouseAndMouseCursorPosition(boolean_t)`.
///
/// True links the pointer to the cursor; false leaves the cursor where it is
/// while deltas keep arriving, which is what a game's mouse look wants.
pub unsafe extern "C" fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> i32 {
    if let Some(provider) = PROVIDER.get() {
        provider.associate(connected != 0);
    }
    0
}

exports::exports! {
    functions {
        "CGSessionCopyCurrentDictionary" => CGSessionCopyCurrentDictionary,
        "CGDisplayHideCursor" => CGDisplayHideCursor,
        "CGDisplayShowCursor" => CGDisplayShowCursor,
        "CGCursorIsVisible" => CGCursorIsVisible,
        "CGWarpMouseCursorPosition" => CGWarpMouseCursorPosition,
        "CGAssociateMouseAndMouseCursorPosition" => CGAssociateMouseAndMouseCursorPosition,
    }
}
