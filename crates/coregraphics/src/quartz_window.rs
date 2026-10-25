//! The window-level calls CoreGraphics exposes.

#![allow(non_snake_case)]

#[repr(i32)]
enum CGWindowLevelKey {
    _BaseWindow = 0,
}

type CGWindowLevel = i32;

// FIXME: Window levels are not implemented; every key returns zero.
extern "C" fn CGWindowLevelForKey(_key: CGWindowLevelKey) -> CGWindowLevel {
    0
}

/// `CGWindowLevel CGShieldingWindowLevel(void)`: `kCGMaximumWindowLevel - 3`.
pub unsafe extern "C" fn CGShieldingWindowLevel() -> i32 {
    2147483628
}

exports::exports! {
    functions {
        "CGWindowLevelForKey" => CGWindowLevelForKey,
        "CGShieldingWindowLevel" => CGShieldingWindowLevel,
    }
}
