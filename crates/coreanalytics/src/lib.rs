//! CoreAnalytics, the private framework a Mac's own tools report usage events
//! through (`ld` sends `com.apple.ld.options` on every link).
//!
//! An event is only collected when a diagnostics client has asked for it, and
//! there is none here: as on a Mac where no client uses the event, nothing is
//! sent, and a lazy event's payload builder is never called. That is the whole
//! behaviour, not an approximation of it.
//!
//! The signatures are Apple's SPI as WebKit declares it (`CoreAnalyticsSPI.h`):
//! `void analytics_send_event(const char *, xpc_object_t)` and
//! `void analytics_send_event_lazy(const char *, xpc_object_t (^)(void))`.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_void};

/// `void analytics_send_event(const char *event_name, xpc_object_t payload)`
///
/// The payload stays the caller's: CoreAnalytics only reads it.
pub unsafe extern "C" fn analytics_send_event(_event: *const c_char, _payload: *mut c_void) {}

/// `void analytics_send_event_lazy(const char *event_name,
/// xpc_object_t (^builder)(void))`
///
/// The builder runs only for an event some client uses, which none does.
pub unsafe extern "C" fn analytics_send_event_lazy(_event: *const c_char, _builder: *mut c_void) {}

exports::exports! {
    functions {
        "analytics_send_event" => analytics_send_event,
        "analytics_send_event_lazy" => analytics_send_event_lazy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A builder that ran would be a payload built for nobody; the block here
    /// would crash if it were called, because it is not a block.
    #[test]
    fn a_lazy_event_never_builds_its_payload() {
        let not_a_block = std::ptr::dangling_mut::<c_void>();
        // SAFETY: the function reads neither argument.
        unsafe { analytics_send_event_lazy(c"com.example.event".as_ptr(), not_a_block) };
    }
}
