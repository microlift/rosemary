#[repr(i32)]
enum CGWindowLevelKey {
    _BaseWindow = 0,
}

type CGWindowLevel = i32;

// FIXME: Window levels are not implemented; every key returns zero.
#[allow(non_snake_case)]
extern "C" fn CGWindowLevelForKey(_key: CGWindowLevelKey) -> CGWindowLevel {
    0
}

/// `Boolean UAZoomEnabled(void)`: whether the screen zoom is on, which it
/// never is for a guest.
#[allow(non_snake_case)]
extern "C" fn UAZoomEnabled() -> u8 {
    0
}

/// `OSStatus UAZoomChangeFocus(const CGRect *, const CGRect *, UAZoomChangeFocusType)`:
/// where the zoom should look. With no zoom there is nothing to move.
#[allow(non_snake_case)]
extern "C" fn UAZoomChangeFocus(_rect: *const f64, _highlight: *const f64, _kind: u32) -> i32 {
    0
}

exports::exports! {
    functions {
        "UAZoomEnabled" => UAZoomEnabled,
        "UAZoomChangeFocus" => UAZoomChangeFocus,
        "CGWindowLevelForKey" => CGWindowLevelForKey,
    }
}
