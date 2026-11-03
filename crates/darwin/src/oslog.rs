#![cfg(not(target_os = "macos"))]

use std::ffi::{CStr, c_char, c_void};
use std::io::Write;

/// `os_log_type_t`. Apple's values, because the caller passes one in.
mod log_type {
    pub const DEFAULT: u8 = 0x00;
    pub const INFO: u8 = 0x01;
    pub const DEBUG: u8 = 0x02;
    pub const ERROR: u8 = 0x10;
    pub const FAULT: u8 = 0x11;
}

/// What a log handle names, kept for the process.
struct Log {
    subsystem: String,
    category: String,
}

/// The handles [`os_log_create`] has handed out, by address.
///
/// A registry rather than a field read back out of the handle, because a caller
/// may also pass `OS_LOG_DEFAULT` - which is `crate::data`'s storage and not one
/// of these at all. An address that is not in here is the default log, which is
/// the right answer for it.
fn logs() -> &'static std::sync::Mutex<Vec<(usize, &'static Log)>> {
    static LOGS: std::sync::Mutex<Vec<(usize, &'static Log)>> = std::sync::Mutex::new(Vec::new());
    &LOGS
}

fn log_of(handle: *const c_void) -> Option<&'static Log> {
    let logs = logs().lock().unwrap_or_else(|e| e.into_inner());
    logs.iter()
        .find(|(address, _)| *address == handle as usize)
        .map(|(_, log)| *log)
}

/// Whether debug-level messages are written.
///
/// Off by default, which is Apple's default too: `qDebug` is `OS_LOG_TYPE_DEBUG`
/// and a Qt application emits a great many of them. `ROSEMARY_OS_LOG_DEBUG=1`
/// turns them on, which is what makes a guest's own tracing readable while
/// something is being diagnosed.
fn debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("ROSEMARY_OS_LOG_DEBUG").is_some_and(|v| !v.is_empty() && v != "0")
    })
}

/// A C string, with a cap so that a pointer into something that is not a string
/// cannot be walked to the end of the address space.
///
/// # Safety
///
/// `p` is null or points to a NUL-terminated string.
unsafe fn text(p: *const c_char) -> String {
    if p.is_null() {
        // What Apple's logger writes for a null `%s`, and what a caller reading
        // the line needs to see rather than an empty gap.
        return "(null)".to_string();
    }
    let bytes = unsafe { CStr::from_ptr(p) }.to_bytes();
    String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).into_owned()
}

/// One argument out of an `_os_log_impl` buffer.
struct Item<'a> {
    /// The descriptor byte, kept for the string case: a string item whose
    /// payload is not a pointer carries its characters inline.
    descriptor: u8,
    payload: &'a [u8],
}

impl Item<'_> {
    /// The payload as an unsigned integer of its own width.
    fn unsigned(&self) -> u64 {
        let mut bytes = [0u8; 8];
        let n = self.payload.len().min(8);
        bytes[..n].copy_from_slice(&self.payload[..n]);
        u64::from_le_bytes(bytes)
    }

    /// The payload as a signed integer, sign-extended from its own width.
    fn signed(&self) -> i64 {
        let width = self.payload.len().min(8);
        if width == 0 || width == 8 {
            return self.unsigned() as i64;
        }
        let shift = 64 - width * 8;
        ((self.unsigned() as i64) << shift) >> shift
    }

    fn double(&self) -> f64 {
        match self.payload.len() {
            4 => f32::from_le_bytes(self.payload[..4].try_into().unwrap_or([0; 4])) as f64,
            _ => f64::from_le_bytes(
                self.payload[..8.min(self.payload.len())]
                    .try_into()
                    .unwrap_or([0; 8]),
            ),
        }
    }

    /// The payload as a string: the text a pointer points at, or the bytes
    /// themselves when the compiler inlined them.
    ///
    /// # Safety
    ///
    /// A pointer payload points to a NUL-terminated string.
    unsafe fn string(&self) -> String {
        // A pointer is eight bytes and an item type of 2 ("string"); anything
        // else of another width is characters the call site copied in.
        if self.payload.len() == 8 {
            return unsafe { text(self.unsigned() as *const c_char) };
        }
        let end = self
            .payload
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(self.payload.len());
        String::from_utf8_lossy(&self.payload[..end]).into_owned()
    }
}

/// Split an `_os_log_impl` buffer into its arguments.
///
/// The layout is a summary byte, a count, then that many `(descriptor, size,
/// bytes)` items. A buffer that runs out mid-item stops there rather than
/// refusing the whole message: a message with one argument missing is still
/// worth reading, and this is a diagnostic path.
fn items(buffer: &[u8]) -> Vec<Item<'_>> {
    let mut out = Vec::new();
    if buffer.len() < 2 {
        return out;
    }
    let count = buffer[1] as usize;
    let mut at = 2;
    while out.len() < count && at + 2 <= buffer.len() {
        let descriptor = buffer[at];
        let size = buffer[at + 1] as usize;
        at += 2;
        if at + size > buffer.len() {
            break;
        }
        out.push(Item {
            descriptor,
            payload: &buffer[at..at + size],
        });
        at += size;
    }
    out
}

/// `format` with the `%` conversions filled in from `buffer`.
///
/// `os_log`'s own annotations are dropped: `%{public}s` and `%{private,uuid_t}.16P`
/// are the same conversions as `%s` and `%.16P` with a note for the log collector
/// about what may be shown, and there is no collector here.
///
/// # Safety
///
/// `format` is a NUL-terminated string, `buffer` holds `size` readable bytes, and
/// any pointer argument in it points to a NUL-terminated string.
unsafe fn format_message(format: *const c_char, buffer: *const u8, size: u32) -> String {
    if format.is_null() {
        return String::new();
    }
    let template = unsafe { CStr::from_ptr(format) }.to_bytes().to_vec();
    let bytes: &[u8] = match buffer.is_null() || size == 0 {
        true => &[],
        // SAFETY: the caller states the buffer holds `size` readable bytes.
        false => unsafe { std::slice::from_raw_parts(buffer, size as usize) },
    };
    let args = items(bytes);
    let mut next = 0usize;
    let mut out = String::with_capacity(template.len() + 32);
    let mut chars = template.iter().copied().peekable();
    while let Some(byte) = chars.next() {
        if byte != b'%' {
            out.push(byte as char);
            continue;
        }
        if chars.peek() == Some(&b'%') {
            chars.next();
            out.push('%');
            continue;
        }
        // The annotation, then the flags, width and precision, then the
        // conversion. Everything before the conversion letter only decides how
        // wide the result is, which this does not reproduce - a log line is read,
        // not parsed.
        if chars.peek() == Some(&b'{') {
            for byte in chars.by_ref() {
                if byte == b'}' {
                    break;
                }
            }
        }
        let mut conversion = None;
        for byte in chars.by_ref() {
            if byte.is_ascii_alphabetic() || byte == b'@' {
                conversion = Some(byte);
                break;
            }
            // A `*` width is an argument of its own, which is one item consumed
            // before the value.
            if byte == b'*' {
                next += 1;
            }
        }
        let Some(conversion) = conversion else { break };
        // `l`, `ll`, `h`, `z` and friends are length modifiers, not conversions:
        // keep reading until the letter that is one.
        let conversion = match conversion {
            b'l' | b'h' | b'z' | b'j' | b't' | b'q' | b'L' => {
                let mut found = conversion;
                for byte in chars.by_ref() {
                    if !matches!(byte, b'l' | b'h' | b'z' | b'j' | b't' | b'q' | b'L') {
                        found = byte;
                        break;
                    }
                }
                found
            }
            other => other,
        };
        let Some(item) = args.get(next) else {
            // More conversions than arguments: say so in place rather than
            // dropping the rest of the line.
            out.push_str("<missing>");
            continue;
        };
        next += 1;
        match conversion {
            // SAFETY: the caller states a pointer argument is a string.
            b's' | b'S' => out.push_str(&unsafe { item.string() }),
            b'd' | b'i' => out.push_str(&item.signed().to_string()),
            b'u' => out.push_str(&item.unsigned().to_string()),
            b'x' => out.push_str(&format!("{:x}", item.unsigned())),
            b'X' => out.push_str(&format!("{:X}", item.unsigned())),
            b'o' => out.push_str(&format!("{:o}", item.unsigned())),
            b'c' => out.push(item.unsigned() as u8 as char),
            b'f' | b'F' => out.push_str(&format!("{:.6}", item.double())),
            b'e' | b'E' | b'g' | b'G' => out.push_str(&format!("{}", item.double())),
            b'p' | b'P' => out.push_str(&format!("{:#x}", item.unsigned())),
            // An object's `-description` is Foundation's to answer and this crate
            // is underneath it, so the pointer is what there is. The descriptor
            // is what says it was an object rather than a number.
            b'@' => out.push_str(&format!("<object {:#x}>", item.unsigned())),
            other => out.push_str(&format!("<%{}>", other as char)),
        }
        let _ = item.descriptor;
    }
    out
}

/// `os_log_t os_log_create(const char *subsystem, const char *category)`
///
/// The handle is leaked, which is what its lifetime is: a log is created once per
/// subsystem during startup and used for the life of the process, and Apple's own
/// is never deallocated either.
///
/// # Safety
///
/// Both arguments are null or NUL-terminated strings.
unsafe extern "C" fn os_log_create(
    subsystem: *const c_char,
    category: *const c_char,
) -> *mut c_void {
    let (subsystem, category) = unsafe { (text(subsystem), text(category)) };
    let log: &'static Log = Box::leak(Box::new(Log {
        subsystem,
        category,
    }));
    let handle = log as *const Log as *mut c_void;
    logs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((handle as usize, log));
    handle
}

/// `bool os_log_type_enabled(os_log_t, os_log_type_t)`
unsafe extern "C" fn os_log_type_enabled(_log: *const c_void, kind: u8) -> bool {
    kind != log_type::DEBUG || debug_enabled()
}

/// `void _os_log_impl(void *dso, os_log_t log, os_log_type_t type, const char
/// *format, uint8_t *buffer, uint32_t size)`
///
/// # Safety
///
/// As [`format_message`]; `log` is null, a handle [`os_log_create`] made, or
/// `OS_LOG_DEFAULT`.
unsafe extern "C" fn _os_log_impl(
    _dso: *const c_void,
    log: *const c_void,
    kind: u8,
    format: *const c_char,
    buffer: *const u8,
    size: u32,
) {
    // Checked here as well as at the call site: a caller is allowed to log
    // without asking first, and `os_log` itself drops a disabled message.
    if kind == log_type::DEBUG && !debug_enabled() {
        return;
    }
    let message = unsafe { format_message(format, buffer, size) };
    let level = match kind {
        log_type::FAULT => "fault",
        log_type::ERROR => "error",
        log_type::DEBUG => "debug",
        log_type::INFO => "info",
        log_type::DEFAULT => "",
        // A level this module does not name reads as the default one: an
        // unadorned line, rather than one labelled with a number.
        _ => "",
    };
    // The subsystem and category when there are any, because that is how a reader
    // tells one of a program's logging categories from another - and nothing
    // else on stderr carries them.
    let source = match log_of(log) {
        Some(log) if !log.subsystem.is_empty() || !log.category.is_empty() => {
            format!("[{}:{}] ", log.subsystem, log.category)
        }
        _ => String::new(),
    };
    let prefix = match level.is_empty() {
        true => source,
        false => format!("{source}{level}: "),
    };
    // One write, so that a line from another thread cannot land inside this one.
    let line = format!("{prefix}{message}\n");
    let _ = std::io::stderr().write_all(line.as_bytes());
}

/// `void *os_retain(void *)` and `void os_release(void *)`
///
/// A log handle lives for the process - see [`os_log_create`] - so there is
/// nothing to count. Published rather than left out because a caller balances its
/// own pairs and an unbound one is a jump to zero.
unsafe extern "C" fn os_retain(object: *mut c_void) -> *mut c_void {
    object
}

unsafe extern "C" fn os_release(_object: *mut c_void) {}

/// `os_activity_t _os_activity_create(void *dso, const char *description,
/// os_activity_t parent, os_activity_flag_t flags)`
///
/// An activity groups the messages logged underneath it, which matters to a log
/// *reader* and there is none here - so this hands back a distinct, non-null
/// handle and the scope calls do nothing with it. Non-null because a caller tests
/// the result before entering the scope.
unsafe extern "C" fn _os_activity_create(
    _dso: *const c_void,
    _description: *const c_char,
    _parent: *mut c_void,
    _flags: u32,
) -> *mut c_void {
    // A leaked word, so that two activities are never the same handle.
    Box::leak(Box::new(0u64)) as *mut u64 as *mut c_void
}

/// `void os_activity_scope_enter(os_activity_t, os_activity_scope_state_t)`
///
/// The state is `struct os_activity_scope_state_s { uint64_t opaque[2]; }`, which
/// the caller owns and `os_activity_scope_leave` reads back. Zeroed rather than
/// left alone: the caller's is a stack variable, and leaving it uninitialised
/// hands `leave` whatever was underneath.
///
/// # Safety
///
/// `state` is null or writable for sixteen bytes.
unsafe extern "C" fn os_activity_scope_enter(_activity: *mut c_void, state: *mut u64) {
    if state.is_null() {
        return;
    }
    // SAFETY: the caller states it is writable for two words.
    unsafe { std::ptr::write_bytes(state, 0, 2) };
}

unsafe extern "C" fn os_activity_scope_leave(_state: *mut u64) {}

exports::exports! {
    functions {
        "os_log_create" => os_log_create,
        "os_log_type_enabled" => os_log_type_enabled,
        // `_os_log_impl` and `_os_activity_create` begin with an underscore of
        // their own, in addition to Mach-O's.
        "_os_log_impl" => _os_log_impl,
        // The level-specific entry points take the same arguments, the level
        // included, and differ only in what Apple records alongside.
        "_os_log_debug_impl" => _os_log_impl,
        "_os_log_error_impl" => _os_log_impl,
        "_os_log_fault_impl" => _os_log_impl,
        "os_retain" => os_retain,
        "os_release" => os_release,
        "_os_activity_create" => _os_activity_create,
        "os_activity_scope_enter" => os_activity_scope_enter,
        "os_activity_scope_leave" => os_activity_scope_leave,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a buffer the way the compiler does at an `os_log` call site.
    fn buffer(items: &[&[u8]]) -> Vec<u8> {
        let mut out = vec![0u8, items.len() as u8];
        for payload in items {
            // Descriptor 0 (a scalar with no privacy flags), then the width.
            out.push(0);
            out.push(payload.len() as u8);
            out.extend_from_slice(payload);
        }
        out
    }

    fn render(format: &CStr, bytes: &[u8]) -> String {
        // SAFETY: a literal format and a buffer this test built.
        unsafe { format_message(format.as_ptr(), bytes.as_ptr(), bytes.len() as u32) }
    }

    /// The call Qt makes for every warning it writes: one `%{public}s` holding a
    /// pointer to the message.
    #[test]
    fn a_public_string_is_the_message() {
        let message = c"QFont::setPointSize: Point size <= 0 (0)";
        let pointer = (message.as_ptr() as u64).to_le_bytes();
        let bytes = buffer(&[&pointer]);
        assert_eq!(
            render(c"%{public}s", &bytes),
            "QFont::setPointSize: Point size <= 0 (0)"
        );
    }

    #[test]
    fn the_integer_conversions_keep_their_sign_and_width() {
        let bytes = buffer(&[&(-5i32).to_le_bytes(), &7u32.to_le_bytes()]);
        assert_eq!(render(c"%d and %u", &bytes), "-5 and 7");
        // A length modifier is not a conversion.
        let wide = buffer(&[&(-5i64).to_le_bytes()]);
        assert_eq!(render(c"%lld", &wide), "-5");
        let hex = buffer(&[&255u32.to_le_bytes()]);
        assert_eq!(
            render(
                c"%x/%X",
                &buffer(&[&255u32.to_le_bytes(), &255u32.to_le_bytes()])
            ),
            "ff/FF"
        );
        assert_eq!(render(c"%#x", &hex), "ff");
    }

    #[test]
    fn a_literal_percent_consumes_no_argument() {
        let bytes = buffer(&[&42u32.to_le_bytes()]);
        assert_eq!(render(c"100%% of %d", &bytes), "100% of 42");
    }

    /// A format asking for more than the buffer holds must still print what it
    /// has: this runs on the path that reports other failures.
    #[test]
    fn a_missing_argument_is_named_rather_than_dropped() {
        let bytes = buffer(&[]);
        assert_eq!(render(c"a=%d b=%d", &bytes), "a=<missing> b=<missing>");
    }

    #[test]
    fn a_null_string_reads_as_null() {
        let bytes = buffer(&[&0u64.to_le_bytes()]);
        assert_eq!(render(c"%s", &bytes), "(null)");
    }

    /// Debug is off unless asked for, which is Apple's default and what keeps a
    /// Qt application's own tracing out of the way.
    #[test]
    fn debug_is_the_one_level_off_by_default() {
        // SAFETY: no guest state; these take a handle they do not read.
        unsafe {
            assert!(os_log_type_enabled(std::ptr::null(), log_type::DEFAULT));
            assert!(os_log_type_enabled(std::ptr::null(), log_type::ERROR));
            assert!(os_log_type_enabled(std::ptr::null(), log_type::INFO));
            assert_eq!(
                os_log_type_enabled(std::ptr::null(), log_type::DEBUG),
                debug_enabled()
            );
        }
    }
}
