use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use objc::object::Id;
use objc::sel::Sel;

/// What the standard About panel shows, after the options have been merged with
/// the bundle's `Info.plist`.
#[derive(Debug, Default, PartialEq)]
struct About {
    name: String,
    /// `NSAboutPanelOptionApplicationVersion`: the marketing version.
    application_version: Option<String>,
    /// `NSAboutPanelOptionVersion`: the build, shown in parentheses after it.
    version: Option<String>,
    credits: Option<String>,
    copyright: Option<String>,
}

impl About {
    fn title(&self) -> String {
        format!("About {}", self.name)
    }

    /// The lines under the name, in AppKit's order. The build is left out
    /// when it repeats the marketing version, as AppKit does.
    fn details(&self) -> String {
        let mut lines = Vec::new();
        let version = match (&self.application_version, &self.version) {
            (Some(app), Some(build)) if app != build => Some(format!("Version {app} ({build})")),
            (Some(app), _) => Some(format!("Version {app}")),
            (None, Some(build)) => Some(format!("Version {build}")),
            (None, None) => None,
        };
        lines.extend(version);
        lines.extend(self.credits.clone());
        lines.extend(self.copyright.clone());
        lines.join("\n\n")
    }
}

/// `-[options objectForKey:]`, or nil for nil options or a receiver that does
/// not answer it.
///
/// # Safety
///
/// `options` must be null or a live object.
unsafe fn option(options: Id, key: &str) -> Id {
    unsafe { send_object(options, "objectForKey:", foundation::string::from_str(key)) }
}

/// # Safety
///
/// `target` must be null or live, and `selector` must take zero or one object
/// and return one.
unsafe fn send_object(target: Id, selector: &str, argument: Id) -> Id {
    if target.is_null() {
        return std::ptr::null_mut();
    }
    let sel = objc::sel::get(selector);
    let Some(method) = (unsafe { objc::object::class_of(target) }).and_then(|c| c.method(sel))
    else {
        return std::ptr::null_mut();
    };
    let f = unsafe {
        std::mem::transmute::<*const c_void, unsafe extern "C" fn(Id, Sel, Id) -> Id>(
            method.implementation(),
        )
    };
    unsafe { f(target, sel, argument) }
}

/// An option's text. Credits are usually an `NSAttributedString`, so a value
/// that is not itself a string is asked for its `-string`.
///
/// # Safety
///
/// `value` must be null or a live object.
unsafe fn text(value: Id) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let mut bytes = unsafe { foundation::string::bytes(value) };
    if bytes.is_empty() {
        let plain = unsafe { send_object(value, "string", std::ptr::null_mut()) };
        bytes = unsafe { foundation::string::bytes(plain) };
    }
    let text = String::from_utf8_lossy(&bytes).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// # Safety
///
/// `options` must be null or a dictionary.
unsafe fn about(options: Id) -> About {
    let bundle = foundation::bundle::main();
    let info = |key: &str| bundle.and_then(|b| b.info_string(key)).map(str::to_string);
    let chosen = |key: &str, fallback: Option<String>| {
        // SAFETY: as this function's.
        unsafe { text(option(options, key)) }.or(fallback)
    };
    About {
        name: chosen("ApplicationName", info("CFBundleName"))
            .unwrap_or_else(|| crate::metadata::name().to_string_lossy().into_owned()),
        application_version: chosen("ApplicationVersion", info("CFBundleShortVersionString")),
        version: chosen("Version", info("CFBundleVersion")),
        // `Credits.rtf` and `Credits.html` are not read: there is nothing here
        // to render either format into plain text faithfully.
        credits: chosen("Credits", None),
        copyright: chosen("Copyright", info("NSHumanReadableCopyright")),
    }
}

/// The open About panel's window, so ordering it front again raises it
/// rather than opening a second, as AppKit's single panel does.
static OPEN: AtomicU32 = AtomicU32::new(0);

/// `-orderFrontStandardAboutPanelWithOptions:`.
///
/// Shown as a modeless alert with the application's icon: the method returns
/// at once and the program goes on running, as with AppKit's panel. The icon
/// option is not read; the application's own icon is shown.
pub(crate) unsafe extern "C" fn order_front_with_options(_receiver: Id, _sel: Sel, options: Id) {
    let open = OPEN.load(Ordering::Acquire);
    if open != 0 && crate::dialog::raise(open) {
        return;
    }
    // SAFETY: the guest passes a dictionary or nil.
    let about = unsafe { about(options) };
    let request = crate::dialog::Request {
        title: about.title(),
        message: about.name.clone(),
        informative: about.details(),
        buttons: vec!["OK".to_string()],
        cancel: None,
        application_icon: true,
    };
    match crate::dialog::show(&request, |_| OPEN.store(0, Ordering::Release)) {
        Some(id) => OPEN.store(id, Ordering::Release),
        None => eprintln!("rosetta3: appkit: could not show the About panel"),
    }
}

/// `-orderFrontStandardAboutPanel:`, the menu item's action, which is the
/// options form with none.
pub(crate) unsafe extern "C" fn order_front(receiver: Id, sel: Sel, _sender: Id) {
    unsafe { order_front_with_options(receiver, sel, std::ptr::null_mut()) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn about(application_version: Option<&str>, version: Option<&str>) -> About {
        About {
            name: "Game".into(),
            application_version: application_version.map(Into::into),
            version: version.map(Into::into),
            credits: Some("Made by people".into()),
            copyright: Some("(c) 2026".into()),
        }
    }

    #[test]
    fn the_build_follows_the_version_in_parentheses() {
        let about = about(Some("1.2"), Some("345"));
        assert_eq!(about.title(), "About Game");
        assert_eq!(
            about.details(),
            "Version 1.2 (345)\n\nMade by people\n\n(c) 2026"
        );
    }

    #[test]
    fn a_build_equal_to_the_version_is_shown_once() {
        assert!(
            about(Some("1.2"), Some("1.2"))
                .details()
                .starts_with("Version 1.2\n")
        );
    }

    #[test]
    fn missing_lines_are_left_out() {
        let about = About {
            name: "Game".into(),
            ..Default::default()
        };
        assert_eq!(about.details(), "");
    }

    #[test]
    fn an_option_overrides_the_bundle_and_nil_options_fall_back() {
        crate::bootstrap();
        let key = foundation::string::from_str("ApplicationName");
        let value = foundation::string::from_str("Custom");
        let options = corefoundation::collections::new_dictionary(
            &[(key as _, value as _)],
            corefoundation::callbacks::Policy::from_key_callbacks(),
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        ) as Id;
        // SAFETY: a dictionary, and nil.
        assert_eq!(unsafe { super::about(options) }.name, "Custom");
        assert_eq!(unsafe { super::about(std::ptr::null_mut()) }.credits, None);
    }
}
