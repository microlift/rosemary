use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

pub type Ptr = *mut c_void;

/// `GAsyncReadyCallback`.
pub type AsyncReady = unsafe extern "C" fn(source: Ptr, result: Ptr, data: Ptr);

#[repr(C)]
pub struct GError {
    pub domain: u32,
    pub code: c_int,
    pub message: *const c_char,
}

#[repr(C)]
pub struct GList {
    pub data: Ptr,
    pub next: *mut GList,
    pub prev: *mut GList,
}

/// `GtkAllocation`.
#[repr(C)]
pub struct Allocation {
    pub x: c_int,
    pub y: c_int,
    pub width: c_int,
    pub height: c_int,
}

/// `GdkRGBA`.
#[repr(C)]
pub struct Rgba {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    pub alpha: f32,
}

pub const ORIENTATION_HORIZONTAL: c_int = 0;
pub const ORIENTATION_VERTICAL: c_int = 1;

pub const POLICY_DECISION_NAVIGATION_ACTION: c_int = 0;
pub const POLICY_DECISION_NEW_WINDOW_ACTION: c_int = 1;
pub const POLICY_DECISION_RESPONSE: c_int = 2;

pub const LOAD_STARTED: c_int = 0;
pub const LOAD_COMMITTED: c_int = 2;
pub const LOAD_FINISHED: c_int = 3;

pub const HARDWARE_ACCELERATION_POLICY_NEVER: c_int = 1;

pub const SNAPSHOT_REGION_VISIBLE: c_int = 0;
pub const SNAPSHOT_OPTIONS_TRANSPARENT_BACKGROUND: c_int = 1 << 1;

pub const USER_CONTENT_INJECT_ALL_FRAMES: c_int = 0;
pub const USER_CONTENT_INJECT_TOP_FRAME: c_int = 1;
pub const USER_SCRIPT_INJECT_AT_DOCUMENT_START: c_int = 0;
pub const USER_SCRIPT_INJECT_AT_DOCUMENT_END: c_int = 1;

/// `WEBKIT_NETWORK_ERROR_CANCELLED`.
pub const NETWORK_ERROR_CANCELLED: c_int = 302;

pub const SCRIPT_DIALOG_ALERT: c_int = 0;
pub const SCRIPT_DIALOG_CONFIRM: c_int = 1;
pub const SCRIPT_DIALOG_PROMPT: c_int = 2;

macro_rules! api {
    ($( $name:ident: fn($($arg:ty),* $(,)?) $(-> $ret:ty)?; )*) => {
        #[allow(non_snake_case)]
        pub struct Api {
            $( pub $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?, )*
            /// Variadic, so spelled out rather than listed above.
            pub g_object_new: unsafe extern "C" fn(usize, *const c_char, ...) -> Ptr,
            pub g_signal_emit_by_name: unsafe extern "C" fn(Ptr, *const c_char, ...),
        }

        fn resolve(handle: Ptr) -> Result<Api, &'static str> {
            Ok(Api {
                // SAFETY: each field's type is the prototype its library
                // declares for the symbol of the same name.
                $( $name: unsafe { entry(handle, concat!(stringify!($name), "\0")) }
                    .ok_or(stringify!($name))?, )*
                g_object_new: unsafe { entry(handle, "g_object_new\0") }.ok_or("g_object_new")?,
                g_signal_emit_by_name: unsafe { entry(handle, "g_signal_emit_by_name\0") }
                    .ok_or("g_signal_emit_by_name")?,
            })
        }
    };
}

api! {
    g_main_context_iteration: fn(Ptr, c_int) -> c_int;
    g_object_ref_sink: fn(Ptr) -> Ptr;
    g_object_unref: fn(Ptr);
    g_object_ref: fn(Ptr) -> Ptr;
    g_signal_connect_data: fn(Ptr, *const c_char, Ptr, Ptr, Ptr, c_int) -> u64;
    g_signal_handlers_disconnect_matched: fn(Ptr, c_int, u32, u32, Ptr, Ptr, Ptr) -> u32;
    g_type_check_instance_is_a: fn(Ptr, usize) -> c_int;
    g_list_model_get_n_items: fn(Ptr) -> u32;
    g_list_model_get_item: fn(Ptr, u32) -> Ptr;
    g_error_free: fn(*mut GError);
    g_quark_to_string: fn(u32) -> *const c_char;
    g_free: fn(Ptr);
    g_list_free: fn(*mut GList);
    g_date_time_new_from_unix_utc: fn(i64) -> Ptr;
    g_date_time_to_unix: fn(Ptr) -> i64;
    g_date_time_unref: fn(Ptr);

    gtk_disable_setlocale: fn();
    gtk_init_check: fn() -> c_int;
    gtk_widget_measure: fn(Ptr, c_int, c_int, *mut c_int, *mut c_int, *mut c_int, *mut c_int);
    gtk_widget_size_allocate: fn(Ptr, *const Allocation, c_int);
    gtk_widget_map: fn(Ptr);
    gtk_widget_get_mapped: fn(Ptr) -> c_int;
    gtk_widget_observe_controllers: fn(Ptr) -> Ptr;
    gtk_event_controller_focus_get_type: fn() -> usize;
    gdk_texture_get_width: fn(Ptr) -> c_int;
    gdk_texture_get_height: fn(Ptr) -> c_int;
    gdk_texture_download: fn(Ptr, *mut u8, usize);

    webkit_web_view_get_type: fn() -> usize;
    webkit_network_session_new: fn(*const c_char, *const c_char) -> Ptr;
    webkit_network_session_get_cookie_manager: fn(Ptr) -> Ptr;
    webkit_user_content_manager_new: fn() -> Ptr;
    webkit_user_content_manager_add_script: fn(Ptr, Ptr);
    webkit_user_content_manager_remove_all_scripts: fn(Ptr);
    webkit_user_script_new: fn(*const c_char, c_int, c_int, *const *const c_char, *const *const c_char) -> Ptr;
    webkit_user_script_new_for_world: fn(*const c_char, c_int, c_int, *const c_char, *const *const c_char, *const *const c_char) -> Ptr;
    webkit_user_script_unref: fn(Ptr);
    webkit_web_view_get_settings: fn(Ptr) -> Ptr;
    webkit_settings_set_user_agent: fn(Ptr, *const c_char);
    webkit_settings_set_enable_developer_extras: fn(Ptr, c_int);
    webkit_settings_set_hardware_acceleration_policy: fn(Ptr, c_int);
    webkit_settings_set_allow_file_access_from_file_urls: fn(Ptr, c_int);
    webkit_settings_set_allow_universal_access_from_file_urls: fn(Ptr, c_int);
    webkit_settings_set_enable_write_console_messages_to_stdout: fn(Ptr, c_int);
    webkit_web_view_load_uri: fn(Ptr, *const c_char);
    webkit_web_view_load_html: fn(Ptr, *const c_char, *const c_char);
    webkit_web_view_reload: fn(Ptr);
    webkit_web_view_get_title: fn(Ptr) -> *const c_char;
    webkit_script_dialog_get_dialog_type: fn(Ptr) -> c_int;
    webkit_script_dialog_get_message: fn(Ptr) -> *const c_char;
    webkit_script_dialog_prompt_get_default_text: fn(Ptr) -> *const c_char;
    webkit_script_dialog_prompt_set_text: fn(Ptr, *const c_char);
    webkit_script_dialog_confirm_set_confirmed: fn(Ptr, c_int);
    webkit_web_view_stop_loading: fn(Ptr);
    webkit_web_view_get_uri: fn(Ptr) -> *const c_char;
    webkit_web_view_set_background_color: fn(Ptr, *const Rgba);
    webkit_web_view_execute_editing_command: fn(Ptr, *const c_char);
    webkit_web_view_evaluate_javascript: fn(Ptr, *const c_char, isize, *const c_char, *const c_char, Ptr, Option<AsyncReady>, Ptr);
    webkit_web_view_evaluate_javascript_finish: fn(Ptr, Ptr, *mut *mut GError) -> Ptr;
    webkit_web_view_get_snapshot: fn(Ptr, c_int, c_int, Ptr, Option<AsyncReady>, Ptr);
    webkit_web_view_get_snapshot_finish: fn(Ptr, Ptr, *mut *mut GError) -> Ptr;
    webkit_policy_decision_use: fn(Ptr);
    webkit_policy_decision_ignore: fn(Ptr);
    webkit_navigation_policy_decision_get_navigation_action: fn(Ptr) -> Ptr;
    webkit_navigation_action_get_request: fn(Ptr) -> Ptr;
    webkit_navigation_action_get_navigation_type: fn(Ptr) -> c_int;
    webkit_uri_request_get_uri: fn(Ptr) -> *const c_char;
    webkit_response_policy_decision_get_response: fn(Ptr) -> Ptr;
    webkit_response_policy_decision_is_main_frame_main_resource: fn(Ptr) -> c_int;
    webkit_uri_response_get_status_code: fn(Ptr) -> u32;
    webkit_uri_response_get_uri: fn(Ptr) -> *const c_char;
    webkit_cookie_manager_get_all_cookies: fn(Ptr, Ptr, Option<AsyncReady>, Ptr);
    webkit_cookie_manager_get_all_cookies_finish: fn(Ptr, Ptr, *mut *mut GError) -> *mut GList;
    webkit_cookie_manager_add_cookie: fn(Ptr, Ptr, Ptr, Option<AsyncReady>, Ptr);
    webkit_cookie_manager_add_cookie_finish: fn(Ptr, Ptr, *mut *mut GError) -> c_int;
    webkit_cookie_manager_delete_cookie: fn(Ptr, Ptr, Ptr, Option<AsyncReady>, Ptr);
    webkit_cookie_manager_delete_cookie_finish: fn(Ptr, Ptr, *mut *mut GError) -> c_int;

    soup_cookie_new: fn(*const c_char, *const c_char, *const c_char, *const c_char, c_int) -> Ptr;
    soup_cookie_free: fn(Ptr);
    soup_cookie_get_name: fn(Ptr) -> *const c_char;
    soup_cookie_get_value: fn(Ptr) -> *const c_char;
    soup_cookie_get_domain: fn(Ptr) -> *const c_char;
    soup_cookie_get_path: fn(Ptr) -> *const c_char;
    soup_cookie_get_secure: fn(Ptr) -> c_int;
    soup_cookie_get_expires: fn(Ptr) -> Ptr;
    soup_cookie_set_secure: fn(Ptr, c_int);
    soup_cookie_set_expires: fn(Ptr, Ptr);

    jsc_value_is_undefined: fn(Ptr) -> c_int;
    jsc_value_is_null: fn(Ptr) -> c_int;
    jsc_value_is_string: fn(Ptr) -> c_int;
    jsc_value_is_number: fn(Ptr) -> c_int;
    jsc_value_is_boolean: fn(Ptr) -> c_int;
    jsc_value_to_string: fn(Ptr) -> *mut c_char;
    jsc_value_to_double: fn(Ptr) -> f64;
    jsc_value_to_boolean: fn(Ptr) -> c_int;
    jsc_value_to_json: fn(Ptr, u32) -> *mut c_char;
}

/// # Safety
///
/// `T` must be the prototype the library declares for `name`, which must end
/// in a NUL.
unsafe fn entry<T: Copy>(handle: Ptr, name: &str) -> Option<T> {
    assert_eq!(
        size_of::<T>(),
        size_of::<Ptr>(),
        "an entry point is a bare function pointer"
    );
    let name = CStr::from_bytes_with_nul(name.as_bytes()).ok()?;
    // SAFETY: a handle from `dlopen` and a NUL-terminated name.
    let found = unsafe { libc::dlsym(handle, name.as_ptr()) };
    if found.is_null() {
        return None;
    }
    Some(unsafe { *(&found as *const Ptr as *const T) })
}

/// The library's entry points, or why there are none.
pub fn api() -> Result<&'static Api, &'static str> {
    static API: OnceLock<Result<Api, &'static str>> = OnceLock::new();
    API.get_or_init(|| {
        // SAFETY: a NUL-terminated soname. `RTLD_GLOBAL` and never closed, as
        // every other `dlopen` in this tree: the library's own threads and
        // callbacks outlive any point at which closing it would be safe.
        let handle = unsafe {
            libc::dlopen(
                c"libwebkitgtk-6.0.so.4".as_ptr(),
                libc::RTLD_LAZY | libc::RTLD_GLOBAL,
            )
        };
        if handle.is_null() {
            return Err("libwebkitgtk-6.0.so.4 is not installed");
        }
        resolve(handle)
    })
    .as_ref()
    .map_err(|missing| *missing)
}

/// A C string the library owns, copied.
///
/// # Safety
///
/// `text` must be null or a NUL-terminated string.
pub unsafe fn text(text: *const c_char) -> String {
    if text.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

/// A C string for passing in, with interior NULs dropped rather than
/// truncating the text at the first one.
pub fn c_string(text: &str) -> std::ffi::CString {
    std::ffi::CString::new(text.replace('\0', "")).expect("NULs were removed")
}

/// Take a `GError` out-parameter's message and free it.
///
/// # Safety
///
/// `error` must be null or a `GError` the caller owns.
pub unsafe fn take_error(api: &Api, error: *mut GError) -> Option<(c_int, String)> {
    if error.is_null() {
        return None;
    }
    let taken = unsafe { ((*error).code, text((*error).message)) };
    // SAFETY: as above; the error is not read again.
    unsafe { (api.g_error_free)(error) };
    Some(taken)
}
