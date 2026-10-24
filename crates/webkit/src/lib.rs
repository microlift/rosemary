use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

mod clipboard;
mod compositor;
mod configuration;
mod data_store;
mod engine;
mod gtk;
mod js;
mod keys;
mod legacy;
mod web_view;

/// Send a message: `send!(receiver, "selector:", argument => Type, ...; Return)`.
///
/// # Safety
///
/// The receiver must be null or a live object, and the types the prototype of
/// the method that will run.
#[macro_export]
#[doc(hidden)]
macro_rules! send {
    ($receiver:expr, $sel:literal $(, $arg:expr => $ty:ty)* ; $ret:ty) => {{
        let f: unsafe extern "C" fn(objc::object::Id, objc::sel::Sel $(, $ty)*) -> $ret =
            std::mem::transmute(objc::msg::entry());
        f($receiver, objc::sel::get($sel) $(, $arg)*)
    }};
}

/// Install instance methods on a class.
#[macro_export]
#[doc(hidden)]
macro_rules! methods {
    ($class:expr, { $( $sel:literal => $imp:expr, $types:literal );* $(;)? }) => {{
        let class: &'static objc::class::Class = $class;
        $( class.replace_method(objc::sel::get($sel), $imp as *const std::ffi::c_void, $types); )*
    }};
}

/// Install class methods, which live on the metaclass.
#[macro_export]
#[doc(hidden)]
macro_rules! class_methods {
    ($class:expr, { $( $sel:literal => $imp:expr, $types:literal );* $(;)? }) => {{
        let meta = $class.metaclass();
        $( meta.replace_method(objc::sel::get($sel), $imp as *const std::ffi::c_void, $types); )*
    }};
}

/// An `NSNumber`'s truth, and false for nil.
pub(crate) fn bool_value(value: Id) -> bool {
    // SAFETY: null is checked; `-boolValue` is `BOOL (id, SEL)`.
    !value.is_null() && unsafe { send!(value, "boolValue"; u8) } != 0
}

/// The classes a guest can link against.
const CLASSES: &[&std::ffi::CStr] = &[
    c"WebPreferences",
    c"WKWebViewConfiguration",
    c"WKProcessPool",
    c"WKPreferences",
    c"WKUserContentController",
    c"WKWebsiteDataStore",
    c"WKHTTPCookieStore",
    c"WKUserScript",
    c"WKWebView",
    c"WKNavigation",
    c"WKNavigationAction",
    c"WKNavigationResponse",
    c"WKFrameInfo",
    c"WKSecurityOrigin",
    c"WKWindowFeatures",
    c"WebView",
    c"WebFrame",
    c"WebDataSource",
    c"WebScriptObject",
    c"WebUndefined",
];

pub fn bootstrap() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        foundation::bootstrap();
        appkit::bootstrap();
        configuration::install();
        data_store::install();
        web_view::install();
        legacy::install();

        let preferences = corefoundation::base::abstract_class(c"WebPreferences", None);
        class_methods!(preferences, {
            "standardPreferences" => standard_preferences, c"@16@0:8";
        });
        methods!(preferences, {
            "setPlugInsEnabled:" => set_plugins_enabled, c"v20@0:8c16";
        });
    });
}

/// `+[WebPreferences standardPreferences]`.
unsafe extern "C" fn standard_preferences(class: Id, _sel: Sel) -> Id {
    static STANDARD: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

    *STANDARD.get_or_init(|| {
        // SAFETY: a class-method receiver is a class registered with this runtime.
        let class = unsafe { objc::class::from_ptr(class as *const Class) }
            .expect("standardPreferences receiver is a class");
        let preferences = objc::object::create_instance(class, 0);
        // The standard preferences are process-global. Keep a reference so a
        // caller releasing its non-owned result cannot make the singleton dangle.
        objc::arc::retain(preferences);
        preferences as usize
    }) as Id
}

/// `-[WebPreferences setPlugInsEnabled:]`.
///
/// The legacy `WebView` these preferences configure is not implemented, so
/// there is no plug-in subsystem to configure. Accepting the setting lets
/// applications finish configuring the shared preferences object.
unsafe extern "C" fn set_plugins_enabled(_receiver: Id, _sel: Sel, _enabled: u8) {}

/// The Objective-C class objects exported by WebKit.framework.
pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    let mut symbols = Vec::new();
    for name in CLASSES {
        let class = objc::class::lookup(name).expect("registered during bootstrap");
        let name = name.to_string_lossy();
        symbols.push((
            format!("_OBJC_CLASS_$_{name}"),
            objc::class::ptr(class) as usize as u64,
        ));
        symbols.push((
            format!("_OBJC_METACLASS_$_{name}"),
            objc::class::ptr(class.metaclass()) as usize as u64,
        ));
    }
    // Legacy `WebView`'s progress notifications, which nothing here posts.
    symbols.extend(corefoundation::base::named_string_globals(&[
        "WebViewProgressStartedNotification",
        "WebViewProgressEstimateChangedNotification",
        "WebViewProgressFinishedNotification",
    ]));
    let cookies = foundation::notification::name_object("WKWebsiteDataTypeCookies");
    symbols.push((
        "_WKWebsiteDataTypeCookies".to_string(),
        corefoundation::base::global_cell("_WKWebsiteDataTypeCookies", cookies),
    ));
    symbols
}

pub fn symbol(name: &str) -> Option<u64> {
    let wanted = name.trim_start_matches('_');
    data_symbols()
        .into_iter()
        .find(|(symbol, _)| symbol.trim_start_matches('_') == wanted)
        .map(|(_, address)| address)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn standard() -> Id {
        bootstrap();
        let class = objc::class::lookup(c"WebPreferences").expect("WebPreferences");
        unsafe { send!(objc::class::ptr(class) as Id, "standardPreferences"; Id) }
    }

    #[test]
    fn standard_preferences_is_a_stable_instance() {
        let first = standard();
        let second = standard();
        assert!(!first.is_null());
        assert_eq!(first, second);
        let class = unsafe { objc::object::class_of(first) }.expect("an Objective-C object");
        assert_eq!(class.name_str(), "WebPreferences");
    }

    #[test]
    fn accepts_enabling_and_disabling_plugins() {
        let preferences = standard();
        // SAFETY: the method's encoding and implementation have this signature.
        unsafe {
            send!(preferences, "setPlugInsEnabled:", 1u8 => u8; ());
            send!(preferences, "setPlugInsEnabled:", 0u8 => u8; ());
        }
    }

    #[test]
    fn exports_class_and_metaclass() {
        let symbols = data_symbols();
        assert!(
            symbols
                .iter()
                .any(|(name, _)| name == "_OBJC_CLASS_$_WKWebView")
        );
        assert!(
            symbols
                .iter()
                .any(|(name, _)| name == "_OBJC_METACLASS_$_WebPreferences")
        );
        assert!(symbols.iter().all(|(_, address)| *address != 0));
        let (_, cookie_cell) = symbols
            .iter()
            .find(|(name, _)| name == "_WKWebsiteDataTypeCookies")
            .expect("cookie data type");
        // SAFETY: data symbols publish the address of a process-lifetime cell.
        assert_ne!(unsafe { *(*cookie_cell as *const u64) }, 0);
    }

    #[test]
    fn web_view_is_a_view_that_keeps_its_configuration() {
        // SAFETY: no other test reads the environment; a unit test must not
        // start GTK and a web process.
        unsafe { std::env::set_var("ROSEMARY_WEBKIT", "0") };
        bootstrap();
        let class = objc::class::lookup(c"WKWebView").expect("registered");
        let view = objc::class::lookup(c"NSView").expect("NSView");
        assert!(class.chain().any(|ancestor| std::ptr::eq(ancestor, view)));
        let frame = foundation::geometry::NSRect {
            origin: foundation::geometry::NSPoint { x: 1.0, y: 2.0 },
            size: foundation::geometry::NSSize {
                width: 30.0,
                height: 40.0,
            },
        };
        // SAFETY: each message is sent with its method's prototype.
        unsafe {
            let configuration = send!(
                objc::class::ptr(objc::class::lookup(c"WKWebViewConfiguration").unwrap()) as Id,
                "new"; Id
            );
            let controller = send!(configuration, "userContentController"; Id);
            assert!(!controller.is_null());
            assert_eq!(
                send!(configuration, "userContentController"; Id),
                controller
            );
            let object = objc::object::create_instance(class, 0);
            let object = send!(object, "initWithFrame:configuration:",
                frame => foundation::geometry::NSRect, configuration => Id; Id);
            assert_eq!(send!(object, "configuration"; Id), configuration);
            // A structure this large is returned through memory, so the
            // method is called directly rather than through `objc_msgSend`.
            let get = class.method(objc::sel::get("frame")).expect("-frame");
            let get: unsafe extern "C" fn(Id, Sel) -> foundation::geometry::NSRect =
                std::mem::transmute(get.implementation());
            assert_eq!(get(object, objc::sel::get("frame")), frame);
            let store = send!(configuration, "websiteDataStore"; Id);
            let class = objc::class::lookup(c"WKWebsiteDataStore").unwrap();
            assert_eq!(
                store,
                send!(objc::class::ptr(class) as Id, "defaultDataStore"; Id)
            );
        }
    }

    #[test]
    fn user_scripts_keep_their_source_and_injection_point() {
        bootstrap();
        let class = objc::class::lookup(c"WKUserScript").expect("registered");
        // SAFETY: each message is sent with its method's prototype.
        unsafe {
            let script = objc::object::create_instance(class, 0);
            let script = send!(script, "initWithSource:injectionTime:forMainFrameOnly:",
                engine::string("window.rbx = {};") => Id, 1isize => isize, 1u8 => u8; Id);
            assert_eq!(
                engine::text_of(send!(script, "source"; Id)),
                "window.rbx = {};"
            );
            assert_eq!(send!(script, "injectionTime"; isize), 1);
            assert_eq!(send!(script, "isForMainFrameOnly"; u8), 1);
            let controller = send!(
                objc::class::ptr(objc::class::lookup(c"WKUserContentController").unwrap()) as Id,
                "new"; Id
            );
            send!(controller, "addUserScript:", script => Id; ());
            let scripts = send!(controller, "userScripts"; Id);
            assert_eq!(send!(scripts, "count"; usize), 1);
            send!(controller, "removeAllUserScripts"; ());
            assert_eq!(
                send!(send!(controller, "userScripts"; Id), "count"; usize),
                0
            );
        }
    }
}
