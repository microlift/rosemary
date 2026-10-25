//! The WindowServer connection calls CoreGraphics exposes.

#![allow(non_snake_case)]

/// `CGWindowServerCreateServerPort`, a private call callers use to ask whether
/// a WindowServer is reachable. There is none here, so the answer is
/// `MACH_PORT_NULL`, which is what Apple's returns when it cannot connect.
pub unsafe extern "C" fn CGWindowServerCreateServerPort() -> u32 {
    0
}

exports::exports! {
    functions {
        "CGWindowServerCreateServerPort" => CGWindowServerCreateServerPort,
    }
}
