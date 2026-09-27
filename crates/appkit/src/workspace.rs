use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

static SHARED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
static CURRENT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

pub const WILL_SLEEP: &str = "NSWorkspaceWillSleepNotification";
pub const DID_WAKE: &str = "NSWorkspaceDidWakeNotification";
pub const SCREENS_DID_SLEEP: &str = "NSWorkspaceScreensDidSleepNotification";
pub const SCREENS_DID_WAKE: &str = "NSWorkspaceScreensDidWakeNotification";

/// `+[NSWorkspace sharedWorkspace]`.
///
/// The workspace is a process-lifetime singleton. Its individual services are
/// installed as they acquire host equivalents; callers must still be able to
/// obtain the object before asking for one of them.
unsafe extern "C" fn shared_workspace(class: Id, _sel: Sel) -> Id {
    *SHARED.get_or_init(|| {
        // SAFETY: the receiver is the `NSWorkspace` class this crate declared.
        let workspace = unsafe {
            objc::object::create_instance(
                objc::class::from_ptr(class as *const objc::class::Class).expect("a class"),
                0,
            )
        };
        // A singleton is not transferred to the caller and must survive a
        // guest incorrectly balancing it with `release`.
        objc::arc::retain(workspace);
        workspace as usize
    }) as Id
}

/// `-URLForApplicationToOpenURL:`.
///
/// Host desktop applications are not Mach-O bundles loaded by this runtime, so
/// there is no truthful application URL to return. Cocoa callers use nil to
/// mean that Launch Services has no registered handler and are required to
/// handle that result.
unsafe extern "C" fn application_url_for_url(_receiver: Id, _sel: Sel, _url: Id) -> Id {
    std::ptr::null_mut()
}

/// `-[NSWorkspace notificationCenter]`.
///
/// Workspace notifications share Foundation's process-wide center in this
/// runtime. It provides the same observer semantics while host sleep and wake
/// events remain unavailable.
unsafe extern "C" fn notification_center(_receiver: Id, _sel: Sel) -> Id {
    foundation::notification::default_center_object()
}

/// `-[NSWorkspace frontmostApplication]`: this process.
///
/// The same answer as `-[NSApplication isActive]`, which never resigns because
/// no other application here can be in front. A program comparing this against
/// its own process to decide whether it has focus must not find a stranger.
unsafe extern "C" fn frontmost_application(_receiver: Id, _sel: Sel) -> Id {
    current()
}

/// `-[NSWorkspace iconForFileType:]` and `-iconForFile:`: a blank 32x32 image.
///
/// Apple never answers nil - a type it knows nothing about gets the generic
/// document icon - and a program puts the result straight into a cell or menu
/// item without checking. There is no icon theme to take artwork from here, so
/// the image is the right size with nothing in it, which draws as nothing.
unsafe extern "C" fn icon_for_file_type(_receiver: Id, _sel: Sel, _type: Id) -> Id {
    let image = crate::image::empty(foundation::geometry::NSSize {
        width: 32.0,
        height: 32.0,
    });
    objc::arc::autorelease(image)
}

/// `-[NSWorkspace accessibilityDisplayShouldReduceTransparency]`,
/// `-…InvertColors`, `-…IncreaseContrast` and `-…ReduceMotion`.
///
/// **All four are answered NO, deliberately, and the reason is that the host was
/// asked and found to have nothing to ask.** These are not appearance queries
/// and they are not a themed value: each one reports *whether the user has
/// asked the system for a display option*, and the answer is a property of a
/// user account on a machine running a macOS accessibility subsystem. This host
/// has no such subsystem, so no user here has asked for anything, and "the user
/// did not ask" is a true answer rather than a stand-in for a missing one.
///
/// # Why this cannot be left to fall through a nil
///
/// **The truthful answer is `false`, and `false` is also exactly what
/// message-to-nil returns for a `BOOL`.** That coincidence is the whole
/// hazard: an unanswered selector here would abort loudly (so the byte could
/// not be wrong silently), but a *selectively* unanswered one, or any future
/// change that made these reach a nil receiver, would produce the right bit by
/// the wrong route and nothing in the guest could tell. The right byte is
/// installed on purpose, and the fact that it was chosen is printed once.
///
/// # The census behind the answer, with its positive controls
///
/// * **No `axserver`.** `/var/db/axserver` does not exist, and neither does any
///   `/var/run/axserver*`. The controls: `/var/db` exists and lists
///   (`Makefile`, `sudo`), and `pgrep` finds live processes on this host, so
///   the two negatives are real absences and not a broken listing or a
///   pattern that matches nothing.
/// * **No `com.apple.universalaccess` preference store.** The path does not
///   exist: `/Library/Preferences` does not exist on this host at all, and
///   `~/Library/Preferences` exists but is **empty** - it is a directory this
///   runtime's own Foundation emulation created, not a macOS one. So
///   `com.apple.universalaccess.plist` is absent from both, and no crate here
///   writes that name (a whole-tree search for `universalaccess` finds zero
///   hits in `crates/`), so nothing in this runtime can be the thing that put
///   a setting there.
/// * **The strongest evidence is the guest's own.** Chromium embeds its
///   renderer sandbox profile in this image, and that profile contains
///   `(path (user-homedir-path "/Library/Preferences/com.apple.universalaccess.plist"))`
///   under `(allow file-read-data)`. So the file the answer would come from is
///   named *by the guest's own authors*, it is exactly the file the census
///   above finds empty, and the guest is permitted to read it and is reading
///   nothing. The same image also carries the strings
///   `com.apple.universalaccess` and
///   `x-apple.systempreferences:com.apple.preference.universalaccess?Captioning`;
///   neither is referenced from `__TEXT,__text` (zero rip-relative
///   displacements to either, against a positive control that finds all three
///   of the `accessibilityDisplayShould…` selector slots), so on this build the
///   settings pane and the notification are not read either.
/// * **Nothing else that would answer.** No `/usr/libexec/Accessibility`, no
///   `Accessibility.framework`, no `/etc/axserver.conf`, and the only
///   accessibility-shaped files on the host are XDG desktop files
///   (`/usr/share/desktop-directories/Utility-Accessibility.directory`,
///   `/usr/share/dbus-1/accessibility-services/`), which are the freedesktop
///   desktop's category names and hold no per-user display state.
///
/// # What each one asserts, and where the byte goes
///
/// Measured by re-reading the send sites rather than by eye:
///
/// | selector | send sites | what the guest stores |
/// |---|---|---|
/// | `…ShouldReduceTransparency` | `0x8102db3`, `0x8103107`, `0x81036ad` | `setne` into record `+0x78` (`0x8102dc8`, `0x810311d`, `0x81036c1`) |
/// | `…ShouldInvertColors` | `0x8102df1`, `0x8103164`, `0x81036e9` | `setne` into record `+0x79` (`0x8102e06`, `0x810316a`, `0x81036fa`) |
/// | `…ShouldIncreaseContrast` | `0x8102e47`, `0x81030a5`, `0x8103659` | compare-then-`mov DWORD` into record `+0x80` (`0x8102e68`, `0x81030c3`, `0x8103672`) |
/// | `…ShouldReduceMotion` | `0x699fd34` | `setne` into the global `0xe87ce38` (`0x699fd49`) |
///
/// The three theme records are the six-flag native-theme state that Chromium
/// keeps for the renderer: the record is the function-local static at
/// `__DATA,__common` `0xe9301a0` behind the once-flag at `0xe930258`, built by
/// the initialiser at `Framework+0x8102ba0` and by the `-initWithHandler:`
/// handler of Chromium's own `NativeThemeEffectiveAppearanceObserver` at
/// `Framework+0x8103040`. `Framework+0x39b9e37` copies six fields out of it into
/// a per-object cache and `Framework+0x39f6780` diffs them and calls
/// `+0x39c46c0` when any moved, so a wrong bit is a silently wrong theme for
/// every renderer in the process rather than a visible failure.
///
/// **No site tests the answer for nil.** Every one of the eleven is a `test`
/// or a compare whose flags the send itself set (`0x8102dc5`/`0x8102e03`
/// `test r12b,r12b`; `0x8102e50` `test al,al`; `0x810311a`/`0x8103167`; and for
/// reduce motion `0x699fd47 test al,al` *after* the store's own flag has been
/// set), so the guest would not crash on a nil here - it would simply record
/// whatever the nil produced. Survivable and silent is the shape of trap this
/// project exists to refuse.
///
/// # The sixth field of the same record is *not* one of these
///
/// `0x39f6780` diffs six fields, and the sixth - `+0x70` - has no
/// `NSWorkspace` selector behind it at all. Its only writer is the record's own
/// constructor at `Framework+0x81049a0`:
///
/// ```text
/// 8104a2f  movzx eax,BYTE PTR [rip+…]   # 0xe930318
/// 8104a36  mov   BYTE PTR [rbx+0x70],al
/// ```
///
/// and `0xe930318` is filled by the call at `0x8104a98` to `0x5cba3d0` with
/// the C string at `0xd2b28e0`. That callee is a command-line switch lookup -
/// `strlen`, a table probe over the vector at `+0x18`, `setne` - and the string
/// is **`force-high-contrast`**. So `+0x70` is
/// `base::CommandLine::HasSwitch("force-high-contrast")`, and its sibling
/// `+0x69`'s constructor source is the same lookup on `force-dark-mode`
/// (string at `0xd2b28d0`). Neither is a question about this host, so nothing
/// is installed for them here: the milestone runs
/// `--headless --dump-dom --user-data-dir=… --no-first-run
/// --no-default-browser-check about:blank` and neither switch is on it, which is
/// the same answer a Mac gives.
///
/// **The one thing this deliberately does not do is mirror `+0x70` into the
/// notify payload.** The payload globals the observer at `Framework+0x39b9e37`
/// reads are `0xe9302c9`, `0xe9302d0`, `0xe9302d8`, `0xe9302d9`, `0xe9302dc`
/// and `0xe9302e0` - the record's six fields at a fixed `+0xc0` - and five of
/// the six are written by the notifying member function at
/// `Framework+0x8103614`, `0x8103627`, `0x8103672`, `0x81036c1` and
/// `0x81036fa`. `0xe9302d0` has **no writer anywhere in the image's text**: a
/// whole-`__text` rip-relative sweep for the range `0xe9302c0..0xe930300`
/// returns it exactly once, the read at `0x39b9e44`. So on this build the
/// cached copy of that field is whatever `__DATA,__common` zero-initialises it
/// to, on a Mac exactly as here. That is a statement about the guest, not a
/// gap for this runtime to fill, and it is left alone.
///
/// # The reduce-motion answer is not cached here, and must not be
///
/// `Framework+0x699fd00` reads as if it were:
///
/// ```text
/// 699fd34  call r14                     ; -[NSWorkspace accessibilityDisplayShouldReduceMotion]
/// 699fd37  cmp BYTE PTR [0xe87ce39],0x0
/// 699fd3e  jne 0x699fd47
/// 699fd40  mov BYTE PTR [0xe87ce39],0x1
/// 699fd47  test al,al
/// 699fd49  setne BYTE PTR [0xe87ce38]   ; <- reached from *both* paths
/// ```
///
/// The write-once byte gates **only its own write**; the store of the answer is
/// after the join and runs on every call. So the selector is sent on every
/// call and the answer is re-stored on every call, and there is no "first
/// answer wins" to reason about. Its four readers (`0x39c3ffe`, `0x699957d`,
/// `0xc2b56b6`, `0xc56a646`) each read the flag, call `0x699fd00` only if it is
/// clear, and then read `0xe87ce38` - the guest's own caching, which is
/// preserved by answering freshly every time.
unsafe extern "C" fn accessibility_display_should_reduce_transparency(
    _receiver: Id,
    _sel: Sel,
) -> u8 {
    refuse_display_option(
        "accessibilityDisplayShouldReduceTransparency",
        "the user asked for windows and menus to stop drawing translucent backgrounds",
    );
    0
}

/// `-[NSWorkspace accessibilityDisplayShouldInvertColors]`.
unsafe extern "C" fn accessibility_display_should_invert_colors(_receiver: Id, _sel: Sel) -> u8 {
    refuse_display_option(
        "accessibilityDisplayShouldInvertColors",
        "the user asked for the whole screen's colours to be inverted",
    );
    0
}

/// `-[NSWorkspace accessibilityDisplayShouldIncreaseContrast]`.
unsafe extern "C" fn accessibility_display_should_increase_contrast(_receiver: Id, _sel: Sel) -> u8 {
    refuse_display_option(
        "accessibilityDisplayShouldIncreaseContrast",
        "the user asked for more contrast between colours",
    );
    0
}

/// `-[NSWorkspace accessibilityDisplayShouldReduceMotion`].
unsafe extern "C" fn accessibility_display_should_reduce_motion(_receiver: Id, _sel: Sel) -> u8 {
    refuse_display_option(
        "accessibilityDisplayShouldReduceMotion",
        "the user asked for animation and parallax to be reduced or removed",
    );
    0
}

/// Say, once per selector, that this host has no accessibility subsystem to
/// hold the answer.
///
/// Once per *name* rather than once per call or once per run, for the reason
/// `foundation`'s `refuse_missing_class` and this crate's
/// [`refuse_no_catalog`](crate::color) give: the guest asks about all four, and
/// the point of the line is that it is a census of what was asked and what this
/// host could answer, not one event. The answer itself is NOT a refusal - it is
/// a true `NO` - so the wording says the answer and then the evidence, and does
/// not suggest the guest should have got something else.
fn refuse_display_option(selector: &'static str, what: &'static str) {
    // A `Vec` and not a set, as in both of those: the census is four names, so a
    // linear scan of owned strings is cheaper than hashing and it keeps the
    // one-shot list in a static with no lazy initialiser.
    static SAID: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut said = SAID.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if said.iter().any(|said| *said == selector) {
        return;
    }
    said.push(selector);
    eprintln!(
        "rosetta3: appkit: -[NSWorkspace {selector}]: NO - {what}, and nobody here has asked. \
         Asked and found empty, with the controls that make the emptiness a measurement: no \
         axserver (/var/db/axserver is absent while /var/db lists; no /var/run/axserver* while \
         /var/run/systemd is there; pgrep finds this process and 500-odd others), and no \
         accessibility preference store - /Library/Preferences does not exist on this host at all, \
         ~/Library/Preferences exists and is empty, and no crate in this tree writes a \
         universalaccess preference (whole-tree search: zero hits). Chromium's own embedded \
         renderer sandbox profile in this very image names the file the answer would come from, \
         (allow file-read-data (path (user-homedir-path \
         \"/Library/Preferences/com.apple.universalaccess.plist\"))), so the guest is permitted \
         to read exactly the store that is not there. Also absent: /usr/libexec/Accessibility, \
         Accessibility.framework, /etc/axserver.conf. These four are the user's display options, \
         not a themed value, so NO is the answer and not a stand-in - and it is the same byte \
         message-to-nil would return, which is why the answer is made here on purpose rather than \
         left to a nil. Chromium records it in the six-flag native-theme state behind the \
         once-flag at 0xe930258 and the renderer themes itself from it, so the bit is silent when \
         wrong. The sibling sixth flag of that state, +0x70, is not asked of the host at all: it \
         is base::CommandLine::HasSwitch(\"force-high-contrast\") (Framework+0x8104a36), and the \
         milestone's command line does not carry it."
    );
}

/// The `NSRunningApplication` for this process, a process-lifetime singleton.
fn current() -> Id {
    *CURRENT.get_or_init(|| {
        let class =
            objc::class::lookup(c"NSRunningApplication").expect("declared before its methods");
        let object = objc::object::create_instance(class, 0);
        objc::arc::retain(object);
        object as usize
    }) as Id
}

/// `+[NSRunningApplication currentApplication]`.
unsafe extern "C" fn current_application(_class: Id, _sel: Sel) -> Id {
    current()
}

fn autoreleased(text: &str) -> Id {
    let string = foundation::string::from_str(text);
    // SAFETY: a fresh CFString, toll-free bridged to NSString.
    unsafe { corefoundation::api::CFAutorelease(string as _) as Id }
}

/// `-processIdentifier`. Only this process is ever represented.
unsafe extern "C" fn process_identifier(_receiver: Id, _sel: Sel) -> i32 {
    std::process::id() as i32
}

/// `-bundleIdentifier`: nil for an executable outside a bundle, as on macOS.
unsafe extern "C" fn bundle_identifier(_receiver: Id, _sel: Sel) -> Id {
    match foundation::bundle::main().and_then(|b| b.identifier()) {
        Some(identifier) => autoreleased(identifier),
        None => std::ptr::null_mut(),
    }
}

/// `-localizedName`: the bundle's display name, else the executable's name.
unsafe extern "C" fn localized_name(_receiver: Id, _sel: Sel) -> Id {
    if let Some(name) = foundation::bundle::main().and_then(|b| b.display_name()) {
        return autoreleased(name);
    }
    let name = std::env::args_os().next().and_then(|arg| {
        std::path::Path::new(&arg)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    });
    match name {
        Some(name) => autoreleased(&name),
        None => std::ptr::null_mut(),
    }
}

/// `-isActive`: always, for the reason given at [`frontmost_application`].
unsafe extern "C" fn running_is_active(_receiver: Id, _sel: Sel) -> u8 {
    1
}

/// `-activateWithOptions:`: already active, which Apple reports as success.
unsafe extern "C" fn activate_with_options(_receiver: Id, _sel: Sel, _options: usize) -> u8 {
    1
}

/// `+[NSRunningApplication runningApplicationsWithBundleIdentifier:]`.
///
/// Host processes are not registered applications in this Darwin workspace.
unsafe extern "C" fn running_applications(_class: Id, _sel: Sel, _identifier: Id) -> Id {
    let array = corefoundation::collections::new_array(
        &[],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh CFArray, toll-free bridged to NSArray.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSWorkspace").expect("declared before its methods");
    class_methods!(class, {
        "sharedWorkspace" => shared_workspace, c"@16@0:8";
    });
    methods!(class, {
        "URLForApplicationToOpenURL:" => application_url_for_url, c"@24@0:8@16";
        "notificationCenter" => notification_center, c"@16@0:8";
        "frontmostApplication" => frontmost_application, c"@16@0:8";
        "iconForFileType:" => icon_for_file_type, c"@24@0:8@16";
        "iconForFile:" => icon_for_file_type, c"@24@0:8@16";
        // `c16@0:8`: a `BOOL` return and **no arguments**, so the frame is
        // `_cmd` at 8 plus its own 8 = 16 and the colon count is 0. The guest
        // reads each answer as a single byte (`test al,al` / `test r12b,r12b`)
        // and only ever sets `rdi` and `rsi` before the send.
        "accessibilityDisplayShouldReduceTransparency" =>
            accessibility_display_should_reduce_transparency, c"c16@0:8";
        "accessibilityDisplayShouldInvertColors" =>
            accessibility_display_should_invert_colors, c"c16@0:8";
        "accessibilityDisplayShouldIncreaseContrast" =>
            accessibility_display_should_increase_contrast, c"c16@0:8";
        "accessibilityDisplayShouldReduceMotion" =>
            accessibility_display_should_reduce_motion, c"c16@0:8";
    });

    let running =
        objc::class::lookup(c"NSRunningApplication").expect("declared before its methods");
    class_methods!(running, {
        "runningApplicationsWithBundleIdentifier:" => running_applications, c"@24@0:8@16";
        "currentApplication" => current_application, c"@16@0:8";
    });
    methods!(running, {
        "processIdentifier" => process_identifier, c"i16@0:8";
        "bundleIdentifier" => bundle_identifier, c"@16@0:8";
        "localizedName" => localized_name, c"@16@0:8";
        "isActive" => running_is_active, c"c16@0:8";
        "activateWithOptions:" => activate_with_options, c"c24@0:8Q16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_workspace_is_one_process_lifetime_object() {
        crate::bootstrap();
        let class =
            objc::class::ptr(objc::class::lookup(c"NSWorkspace").expect("NSWorkspace")) as Id;
        // SAFETY: `+sharedWorkspace` is `id (Class, SEL)`.
        let first = unsafe { shared_workspace(class, objc::sel::get("sharedWorkspace")) };
        let second = unsafe { shared_workspace(class, objc::sel::get("sharedWorkspace")) };
        assert!(!first.is_null());
        assert_eq!(first, second);
        // SAFETY: `-notificationCenter` takes no explicit arguments.
        let center = unsafe { notification_center(first, objc::sel::get("notificationCenter")) };
        assert!(!center.is_null());
        assert_eq!(center, foundation::notification::default_center_object());
    }

    #[test]
    fn a_file_type_icon_is_an_image_even_for_an_unknown_type() {
        crate::bootstrap();
        let kind = foundation::string::from_str("r3-no-such-type");
        // SAFETY: `-iconForFileType:` is `id (id, SEL, id)`.
        let icon = unsafe {
            icon_for_file_type(
                std::ptr::null_mut(),
                objc::sel::get("iconForFileType:"),
                kind,
            )
        };
        assert!(!icon.is_null());
        assert_eq!(crate::image::declared_size(icon).width, 32.0);
    }

    #[test]
    fn frontmost_application_is_this_process() {
        crate::bootstrap();
        // SAFETY: both take no explicit arguments.
        let frontmost = unsafe {
            frontmost_application(std::ptr::null_mut(), objc::sel::get("frontmostApplication"))
        };
        let current = unsafe {
            current_application(std::ptr::null_mut(), objc::sel::get("currentApplication"))
        };
        assert!(!frontmost.is_null());
        assert_eq!(frontmost, current);
        // SAFETY: `-processIdentifier` takes no explicit arguments.
        let pid = unsafe { process_identifier(frontmost, objc::sel::get("processIdentifier")) };
        assert_eq!(pid, std::process::id() as i32);
    }

    /// The four display options are NO, and each one says so once.
    ///
    /// Sent through `objc_msgSend` against a real `NSWorkspace` rather than
    /// called directly, so the check is that the **selector is answered on the
    /// class the guest reaches** and with the arity the guest uses: the guest
    /// writes only `rdi` and `rsi` before all eleven of its sends, and reads
    /// the answer as one byte. A `c16@0:8` row gives it `BOOL` in `al` with
    /// nothing read from `rdx`, which is what the disassembly at
    /// `Framework+0x8102db3`, `0x8102df1`, `0x8102e47`, `0x8103107`,
    /// `0x8103164`, `0x81036ad`, `0x81036e9` and `0x699fd34` shows.
    ///
    /// The assertion is `== 0` and not `!= 0` deliberately: the point is that
    /// the byte is *false*, and the hazard these four carry is that false is
    /// also what a nil would hand back. A test that only checked "no crash"
    /// would pass for the wrong reason.
    #[test]
    fn every_display_option_is_no_and_is_answered_by_name() {
        crate::bootstrap();
        let class = objc::class::ptr(objc::class::lookup(c"NSWorkspace").expect("NSWorkspace"))
            as Id;
        // SAFETY: `+sharedWorkspace` is `id (Class, SEL)`.
        let workspace = unsafe { shared_workspace(class, objc::sel::get("sharedWorkspace")) };
        assert!(!workspace.is_null());
        // SAFETY: each of the four is `BOOL (id, SEL)` - no arguments, so the
        // caller writes `rdi` and `rsi` and nothing else.
        let answer: unsafe extern "C" fn(Id, Sel) -> u8 =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        for selector in [
            "accessibilityDisplayShouldReduceTransparency",
            "accessibilityDisplayShouldInvertColors",
            "accessibilityDisplayShouldIncreaseContrast",
            "accessibilityDisplayShouldReduceMotion",
        ] {
            // SAFETY: the receiver is the singleton and the selector declares
            // no arguments.
            let value = unsafe { answer(workspace, objc::sel::get(selector)) };
            assert_eq!(value, 0, "{selector} must be NO on a host with no axserver");
        }
    }

    /// The four are instance methods on `NSWorkspace`, and each returns `BOOL`
    /// with a frame that reaches `_cmd` and nothing past it.
    ///
    /// A witness assertion, so it is checked against the installed method
    /// rather than against the source: the encoding string in the
    /// `methods!` row and the `u8` the function returns have to agree with the
    /// selector's own colon count, which is 0 for all four. Getting that wrong
    /// is a live crash in this guest - `performAsCurrentDrawingAppearance:`
    /// declared four parameters for a one-colon selector and ran a stale
    /// pointer out of `rcx` (see `crate::appearance`).
    #[test]
    fn a_display_option_is_answered_on_the_workspace_instance() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSWorkspace").expect("NSWorkspace");
        for (selector, arity) in [
            ("accessibilityDisplayShouldReduceTransparency", 0usize),
            ("accessibilityDisplayShouldInvertColors", 0),
            ("accessibilityDisplayShouldIncreaseContrast", 0),
            ("accessibilityDisplayShouldReduceMotion", 0),
        ] {
            assert_eq!(
                // SAFETY: a registered selector's name is a live NUL-terminated
                // string, and it is compared rather than read past its NUL.
                unsafe { std::ffi::CStr::from_ptr(objc::sel::name(objc::sel::get(selector))) }
                    .to_bytes()
                    .iter()
                    .filter(|byte| **byte == b':')
                    .count(),
                arity,
                "{selector}: the colon count is a property of the name"
            );
            assert!(
                class.method(objc::sel::get(selector)).is_some(),
                "{selector} must be installed on NSWorkspace, not only on a metaclass"
            );
        }
    }
}
