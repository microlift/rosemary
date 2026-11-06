//! `NSBeep`, the one alert sound every Mac can make without a preference pane
//! behind it.
//!
//! FIXME (ACCURACY): a real `NSBeep` plays the system alert sound chosen in
//! Sound settings, through whatever output device is current. This instead
//! rings the terminal bell on the process's standard error - a real beep on
//! most terminal emulators, and the host-neutral answer when there is no
//! Sound preference pane and no guarantee of a usable audio device at all.

/// `void NSBeep(void)`.
#[allow(non_snake_case)]
pub extern "C" fn NSBeep() {
    use std::io::Write;
    let _ = std::io::stderr().write_all(b"\x07");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_does_not_panic_with_no_terminal_attached() {
        // Calling it is the whole contract worth testing here: the write
        // itself is best-effort and its failure is swallowed, same as Apple's
        // own silently does nothing without a sound device.
        NSBeep();
    }
}
