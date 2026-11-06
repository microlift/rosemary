use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use corefoundation::base::CFTypeRef;

use crate::block;
use crate::boxed::{Body, contents, fill, make, register};
use crate::define::{class_methods, methods};

/// How long a request waits when nothing said otherwise. Apple's default.
const DEFAULT_TIMEOUT_SECONDS: f64 = 60.0;

//

/// What a request carries, independent of any object holding it.
#[derive(Clone, Default)]
struct Request {
    url: String,
    method: String,
    /// In the order they were set, because a header may legitimately repeat.
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    timeout: f64,
    cache_policy: usize,
    /// `NSURLRequestNetworkServiceType`, only read back: the host decides how
    /// traffic is classified.
    service_type: usize,
}

impl Request {
    fn new(url: String, timeout: f64) -> Request {
        Request {
            url,
            method: "GET".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
            timeout,
            cache_policy: 0,
            service_type: 0,
        }
    }
}

/// What came back.
struct Response {
    url: String,
    status: u16,
    headers: Vec<(String, String)>,
}

//

fn request_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        // The mutable class descends from the immutable one, which is what a
        // guest's `isKindOfClass:` asks about either way.
        let base = register::<Request>(c"NSURLRequest", None);
        let mutable = corefoundation::base::register(
            c"NSMutableURLRequest",
            Some(base),
            size_of::<Body<Request>>(),
            false,
        );
        objc::class::ptr(mutable) as usize
    });
    // SAFETY: the cell holds a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

fn immutable_request_class() -> &'static Class {
    request_class();
    objc::class::lookup(c"NSURLRequest").expect("registered above")
}

/// # Safety
///
/// `receiver` must be null or an object this runtime allocated.
unsafe fn request_of<'a>(receiver: Id) -> Option<&'a mut Request> {
    unsafe { contents(receiver, immutable_request_class()) }
}

/// `+requestWithURL:`, and the two-and-three-argument forms.
///
/// Always a *mutable* request object, whichever class was asked: a guest that
/// asked `NSURLRequest` for one gets something it may not mutate by the type
/// system's account, and one that asked `NSMutableURLRequest` gets what it
/// needs. The distinction Apple enforces at runtime is not one this runtime has
/// a reason to reproduce, and getting it wrong the other way would refuse a
/// `-setHTTPMethod:` the guest is entitled to send.
unsafe extern "C" fn request_with_url(_class: Id, _sel: Sel, url: Id) -> Id {
    autoreleased_request(url, DEFAULT_TIMEOUT_SECONDS)
}

unsafe extern "C" fn request_with_url_policy_timeout(
    _class: Id,
    _sel: Sel,
    url: Id,
    _policy: usize,
    timeout: f64,
) -> Id {
    // The cache policy is ignored because nothing here caches: every request
    // goes to the network, which is what `NSURLRequestReloadIgnoringCacheData`
    // asks for and what the others get anyway.
    autoreleased_request(url, timeout)
}

fn autoreleased_request(url: Id, timeout: f64) -> Id {
    // SAFETY: an `NSURL`, or nil.
    let Some(text) = (unsafe { crate::url::text_of(url) }) else {
        return std::ptr::null_mut();
    };
    let timeout = match timeout > 0.0 {
        true => timeout,
        false => DEFAULT_TIMEOUT_SECONDS,
    };
    objc::arc::autorelease(make(request_class(), Request::new(text, timeout)))
}

/// A new autoreleased request for `url`, for a framework handing a guest the
/// request something else is making.
pub fn request_for(url: &str) -> Id {
    objc::arc::autorelease(make(
        request_class(),
        Request::new(url.to_string(), DEFAULT_TIMEOUT_SECONDS),
    ))
}

/// What a framework that performs a guest's request elsewhere needs of it.
pub struct RequestParts {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
}

/// A request's URL, method and headers.
///
/// # Safety
///
/// `request` must be null or an object this runtime allocated.
pub unsafe fn request_parts(request: Id) -> Option<RequestParts> {
    let request = unsafe { request_of(request) }?;
    Some(RequestParts {
        url: request.url.clone(),
        method: request.method.clone(),
        headers: request.headers.clone(),
    })
}

/// A new autoreleased `NSHTTPURLResponse`, for a framework whose own loader
/// received it.
pub fn http_response(url: &str, status: u16, headers: Vec<(String, String)>) -> Id {
    objc::arc::autorelease(make(
        response_class(),
        Response {
            url: url.to_string(),
            status,
            headers,
        },
    ))
}

/// `-initWithURL:`. `NSURLRequest` itself had never answered it here, which
/// left the mutable subclass with nothing to inherit either - unlike `-URL`
/// and the rest of the accessors below, every one of which is on the
/// immutable class for exactly that reason.
unsafe extern "C" fn init_with_url(receiver: Id, _sel: Sel, url: Id) -> Id {
    // SAFETY: an `NSURL`, or nil.
    let Some(text) = (unsafe { crate::url::text_of(url) }) else {
        return std::ptr::null_mut();
    };
    // `+alloc` leaves `receiver` with no contents yet - `make` is what
    // `+requestWithURL:` uses instead - so this `-init` is where they are
    // made, which is `fill`'s case rather than `request_of`'s.
    // SAFETY: `receiver` is an instance of the class this module registered
    // for `Request`.
    unsafe { crate::boxed::fill(receiver, Request::new(text, DEFAULT_TIMEOUT_SECONDS)) };
    receiver
}

unsafe extern "C" fn set_url(receiver: Id, _sel: Sel, url: Id) {
    // SAFETY: an `NSURL`, or nil.
    let Some(text) = (unsafe { crate::url::text_of(url) }) else {
        return;
    };
    // SAFETY: the receiver is a request this module made.
    if let Some(request) = unsafe { request_of(receiver) } {
        request.url = text;
    }
}

unsafe extern "C" fn request_url(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a request this module made.
    let Some(request) = (unsafe { request_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: a URL this call just made.
    unsafe { corefoundation::api::CFAutorelease(corefoundation::url::new(&request.url)) as Id }
}

unsafe extern "C" fn set_http_method(receiver: Id, _sel: Sel, method: Id) {
    // SAFETY: an `NSString`, or nil.
    let method = unsafe { text(method) };
    // SAFETY: the receiver is a request this module made.
    if let Some(request) = unsafe { request_of(receiver) } {
        request.method = method.to_ascii_uppercase();
    }
}

unsafe extern "C" fn http_method(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a request this module made.
    match unsafe { request_of(receiver) } {
        Some(request) => autoreleased_string(&request.method),
        None => std::ptr::null_mut(),
    }
}

/// `-setValue:forHTTPHeaderField:`, which *replaces* every value already set for
/// the field - unlike `-addValue:forHTTPHeaderField:`, which appends.
unsafe extern "C" fn set_value_for_header(receiver: Id, _sel: Sel, value: Id, field: Id) {
    // SAFETY: both are `NSString`s, or nil.
    let (value, field) = unsafe { (text(value), text(field)) };
    // SAFETY: the receiver is a request this module made.
    let Some(request) = (unsafe { request_of(receiver) }) else {
        return;
    };
    request
        .headers
        .retain(|(name, _)| !name.eq_ignore_ascii_case(&field));
    if !field.is_empty() {
        request.headers.push((field, value));
    }
}

unsafe extern "C" fn add_value_for_header(receiver: Id, _sel: Sel, value: Id, field: Id) {
    // SAFETY: both are `NSString`s, or nil.
    let (value, field) = unsafe { (text(value), text(field)) };
    // SAFETY: the receiver is a request this module made.
    if let Some(request) = unsafe { request_of(receiver) }
        && !field.is_empty()
    {
        request.headers.push((field, value));
    }
}

unsafe extern "C" fn value_for_header(receiver: Id, _sel: Sel, field: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let field = unsafe { text(field) };
    // SAFETY: the receiver is a request this module made.
    let Some(request) = (unsafe { request_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    match request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(&field))
    {
        Some((_, value)) => autoreleased_string(value),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn request_headers(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a request this module made.
    let Some(request) = (unsafe { request_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    headers_dictionary(&request.headers)
}

unsafe extern "C" fn set_http_body(receiver: Id, _sel: Sel, data: Id) {
    // SAFETY: an `NSData`, or nil.
    let bytes = unsafe { corefoundation::collections::data(data as CFTypeRef) }
        .map(|d| d.clone())
        .unwrap_or_default();
    // SAFETY: the receiver is a request this module made.
    if let Some(request) = unsafe { request_of(receiver) } {
        request.body = bytes;
    }
}

unsafe extern "C" fn http_body(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a request this module made.
    let Some(request) = (unsafe { request_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    if request.body.is_empty() {
        return std::ptr::null_mut();
    }
    let data = corefoundation::collections::new_data(request.body.clone());
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(data) as Id }
}

unsafe extern "C" fn set_timeout_interval(receiver: Id, _sel: Sel, timeout: f64) {
    // SAFETY: the receiver is a request this module made.
    if let Some(request) = unsafe { request_of(receiver) }
        && timeout > 0.0
    {
        request.timeout = timeout;
    }
}

unsafe extern "C" fn set_cache_policy(receiver: Id, _sel: Sel, policy: usize) {
    // SAFETY: the receiver is a request this module made.
    if let Some(request) = unsafe { request_of(receiver) } {
        request.cache_policy = policy;
    }
}

unsafe extern "C" fn cache_policy(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: the receiver is a request this module made.
    unsafe { request_of(receiver) }.map_or(0, |request| request.cache_policy)
}

unsafe extern "C" fn set_network_service_type(receiver: Id, _sel: Sel, kind: usize) {
    // SAFETY: the receiver is a request this module made.
    if let Some(request) = unsafe { request_of(receiver) } {
        request.service_type = kind;
    }
}

unsafe extern "C" fn network_service_type(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: the receiver is a request this module made.
    unsafe { request_of(receiver) }.map_or(0, |request| request.service_type)
}

unsafe extern "C" fn timeout_interval(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: the receiver is a request this module made.
    unsafe { request_of(receiver) }.map_or(DEFAULT_TIMEOUT_SECONDS, |r| r.timeout)
}

//

fn response_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        let base = register::<Response>(c"NSURLResponse", None);
        let http = corefoundation::base::register(
            c"NSHTTPURLResponse",
            Some(base),
            size_of::<Body<Response>>(),
            false,
        );
        objc::class::ptr(http) as usize
    });
    // SAFETY: the cell holds a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

/// # Safety
///
/// `receiver` must be null or an object this runtime allocated.
unsafe fn response_of<'a>(receiver: Id) -> Option<&'a mut Response> {
    let base = objc::class::lookup(c"NSURLResponse").expect("registered by `response_class`");
    unsafe { contents(receiver, base) }
}

unsafe extern "C" fn status_code(receiver: Id, _sel: Sel) -> i64 {
    // SAFETY: the receiver is a response this module made.
    unsafe { response_of(receiver) }.map_or(0, |r| r.status as i64)
}

/// `-allHeaderFields`, keyed by the header name as it arrived.
unsafe extern "C" fn all_header_fields(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a response this module made.
    let Some(response) = (unsafe { response_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    headers_dictionary(&response.headers)
}

fn headers_dictionary(headers: &[(String, String)]) -> Id {
    let pairs: Vec<(CFTypeRef, CFTypeRef)> = headers
        .iter()
        .map(|(name, value)| {
            (
                crate::string::from_str(name) as CFTypeRef,
                crate::string::from_str(value) as CFTypeRef,
            )
        })
        .collect();
    let dictionary = corefoundation::collections::new_dictionary(
        &pairs,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: the dictionary retained each; this call owns the ones it made.
    unsafe {
        for (name, value) in pairs {
            corefoundation::api::CFRelease(name);
            corefoundation::api::CFRelease(value);
        }
        corefoundation::api::CFAutorelease(dictionary) as Id
    }
}

unsafe extern "C" fn response_url(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a response this module made.
    let Some(response) = (unsafe { response_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: a URL this call just made.
    unsafe { corefoundation::api::CFAutorelease(corefoundation::url::new(&response.url)) as Id }
}

unsafe extern "C" fn mime_type(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a response this module made.
    let Some(response) = (unsafe { response_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    let content_type = response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"));
    // The MIME type is the media type alone; the charset and any other
    // parameter belong to `-textEncodingName`, which this does not answer.
    match content_type {
        Some((_, value)) => autoreleased_string(value.split(';').next().unwrap_or("").trim()),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn text_encoding_name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a response this module made.
    let Some(response) = (unsafe { response_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .and_then(|(_, value)| {
            value.split(';').skip(1).find_map(|part| {
                let (name, value) = part.trim().split_once('=')?;
                name.trim()
                    .eq_ignore_ascii_case("charset")
                    .then(|| autoreleased_string(value.trim().trim_matches('"')))
            })
        })
        .unwrap_or(std::ptr::null_mut())
}

/// `-expectedContentLength`, or `NSURLResponseUnknownLength` when the response
/// did not say - which is every chunked one.
unsafe extern "C" fn expected_content_length(receiver: Id, _sel: Sel) -> i64 {
    // SAFETY: the receiver is a response this module made.
    let Some(response) = (unsafe { response_of(receiver) }) else {
        return -1;
    };
    response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<i64>().ok())
        .unwrap_or(-1)
}

//

/// The one shared session, and the connection pool behind it.
///
/// An `Agent` rather than a call per request, because it is what keeps
/// connections alive between them and holds the cookie jar - which is the
/// behaviour `+sharedSession` is documented to have.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .user_agent(concat!("rosemary/", env!("CARGO_PKG_VERSION")))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

/// Perform `request`, returning the response and its body or a message.
fn perform(request: &Request) -> Result<(Response, Vec<u8>), String> {
    let mut builder = ureq::http::Request::builder()
        .method(request.method.as_str())
        .uri(request.url.as_str());
    for (name, value) in &request.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    let timeout = std::time::Duration::from_secs_f64(request.timeout);
    let built = builder
        .body(())
        .map_err(|e| format!("the request could not be built: {e}"))?;
    let (parts, ()) = built.into_parts();
    // A body is sent for every method that has one; `SendBody::none()` is what
    // makes a GET a GET rather than a POST with nothing in it.
    let body = match request.body.is_empty() {
        true => ureq::SendBody::none(),
        false => ureq::SendBody::from_owned_reader(std::io::Cursor::new(request.body.clone())),
    };
    let configured = agent()
        .configure_request(ureq::http::Request::from_parts(parts, body))
        .timeout_global(Some(timeout))
        .build();
    let tracing = std::env::var_os("ROSEMARY_HTTP_STATUS").is_some();
    if tracing {
        eprintln!("rosemary: http-status: started");
    }
    let mut response = agent().run(configured).map_err(|e| {
        if tracing {
            let category = match &e {
                ureq::Error::Timeout(_) => "timeout",
                ureq::Error::HostNotFound => "dns",
                ureq::Error::Io(_) => "io",
                ureq::Error::Tls(_) | ureq::Error::Rustls(_) => "tls",
                ureq::Error::StatusCode(_) => "http",
                ureq::Error::RedirectFailed | ureq::Error::TooManyRedirects => "redirect",
                _ => "transport",
            };
            eprintln!("rosemary: http-status: error={category}");
        }
        e.to_string()
    })?;

    let status = response.status().as_u16();
    if tracing {
        eprintln!("rosemary: http-status: response={status}");
    }
    // Where the content actually came from, which differs from `request.url`
    // once a redirect has been followed - `-URL` on the response object this
    // becomes is documented to say where it ended up, not where it was asked
    // for, and `NSURLConnection`'s redirect delegate method compares the two.
    let url = ureq::ResponseExt::get_uri(&response).to_string();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    // No cap on the body: a caller asked for the whole resource, and a limit
    // here would hand it a truncated one it had no way to notice.
    let bytes = response
        .body_mut()
        .with_config()
        .limit(u64::MAX)
        .read_to_vec()
        .map_err(|e| format!("the response body could not be read: {e}"))?;
    Ok((
        Response {
            url,
            status,
            headers,
        },
        bytes,
    ))
}

/// The three objects a completion handler is passed.
fn objects(result: Result<(Response, Vec<u8>), String>) -> (Id, Id, Id) {
    match result {
        Ok((response, bytes)) => {
            let data = corefoundation::collections::new_data(bytes);
            // SAFETY: two fresh objects, handed on autoreleased as Apple's are.
            let data = unsafe { corefoundation::api::CFAutorelease(data) as Id };
            let response = objc::arc::autorelease(make(response_class(), response));
            (data, response, std::ptr::null_mut())
        }
        Err(message) => (
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            crate::error::make(
                crate::error::NSURL_ERROR_DOMAIN,
                crate::error::NSURL_ERROR_UNKNOWN,
                &message,
            ),
        ),
    }
}

/// Call a `void (^)(NSData *, NSURLResponse *, NSError *)` block.
///
/// # Safety
///
/// `block` must be null or a block literal, whose `invoke` has that prototype.
unsafe fn call_completion(block: Id, data: Id, response: Id, error: Id) {
    if block.is_null() {
        return;
    }
    // SAFETY: a block literal, whose `invoke` is the
    // third word. Under the emulated backend the pointer found there is already
    // a thunk, because the method's `@?` encoding had `rosemary::shims::block`
    // rewrite it on the way in.
    unsafe {
        let invoke = *((block as *const u8).add(block::INVOKE_OFFSET) as *const usize);
        if invoke == 0 {
            return;
        }
        let f: unsafe extern "C" fn(Id, Id, Id, Id) = std::mem::transmute(invoke);
        f(block, data, response, error);
    }
}

//

/// A data task: the request it will make, and the handler it will call.
struct Task {
    request: Request,
    /// A heap copy, because `-resume` may be sent long after the block's own
    /// scope ended. Released once the handler has run.
    completion: usize,
    delegate: usize,
    session: usize,
    download: bool,
    description: usize,
    response: std::sync::atomic::AtomicUsize,
    /// Whether `-resume` has already been sent, so a second one does nothing -
    /// which is what Apple's does for a task that is no longer suspended.
    started: bool,
    canceled: bool,
}

impl Drop for Task {
    fn drop(&mut self) {
        objc::arc::release(self.description as Id);
        objc::arc::release(self.response.load(std::sync::atomic::Ordering::Relaxed) as Id);
        if !self.started {
            objc::block::release(self.completion as Id);
            objc::arc::release(self.delegate as Id);
            objc::arc::release(self.session as Id);
        }
    }
}

// SAFETY: the block is a heap copy the task owns until its handler has run, and
// the runtime it belongs to is shared across threads already.
unsafe impl Send for Task {}

fn task_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        let base = corefoundation::base::abstract_class(c"NSURLSessionTask", None);
        let task = register::<Task>(c"NSURLSessionDataTask", Some(base));
        objc::class::ptr(task) as usize
    });
    // SAFETY: the cell holds a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

fn download_task_class() -> &'static Class {
    corefoundation::base::register(
        c"NSURLSessionDownloadTask",
        Some(task_class()),
        size_of::<Body<Task>>(),
        false,
    )
}

/// `-resume`: run the request on a thread of its own and call the handler there.
///
/// A thread rather than the caller's, because that is what a caller of an
/// asynchronous API is entitled to.
unsafe extern "C" fn resume(receiver: Id, _sel: Sel) {
    // SAFETY: the receiver is a task this module made.
    let Some(task) = (unsafe { contents::<Task>(receiver, task_class()) }) else {
        return;
    };
    if task.started || task.canceled {
        return;
    }
    task.started = true;
    let request = task.request.clone();
    let completion = task.completion;
    let delegate = task.delegate;
    let session = task.session;
    let download = task.download;
    let task_id = objc::arc::retain(receiver) as usize;
    std::thread::spawn(move || {
        // A thread of this runtime's making needs a pool of its own: everything
        // handed to the completion handler is autoreleased into it.
        let pool = objc::arc::pool_push();
        if download {
            let result = perform(&request).and_then(|(response, bytes)| {
                // SAFETY: the task is retained for the duration of this worker.
                if let Some(task) = unsafe { contents::<Task>(task_id as Id, task_class()) } {
                    let object = make(response_class(), response);
                    task.response
                        .store(object as usize, std::sync::atomic::Ordering::Release);
                }
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let path = format!(
                    "/tmp/rosemary-download-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                );
                let host = prefix::ns::host_path(&path);
                std::fs::write(host, bytes).map_err(|e| e.to_string())?;
                Ok(path)
            });
            if completion != 0 {
                // The handler form: no delegate callbacks. Apple removes the
                // file once the handler returns, so the handler must move it.
                let response = unsafe { contents::<Task>(task_id as Id, task_class()) }
                    .map_or(0, |t| t.response.load(std::sync::atomic::Ordering::Acquire));
                let (url, error) = match &result {
                    Ok(path) => (
                        objc::arc::autorelease(corefoundation::url::new_file_url(path, false) as Id),
                        std::ptr::null_mut(),
                    ),
                    Err(message) => (
                        std::ptr::null_mut(),
                        crate::error::make(
                            crate::error::NSURL_ERROR_DOMAIN,
                            crate::error::NSURL_ERROR_UNKNOWN,
                            message,
                        ),
                    ),
                };
                // SAFETY: the block was retained when the task was made, and
                // takes (NSURL *, NSURLResponse *, NSError *) like the data one.
                unsafe { call_completion(completion as Id, url, response as Id, error) };
                if let Ok(path) = &result {
                    let _ = std::fs::remove_file(prefix::ns::host_path(path));
                }
            } else {
                let error = match result {
                    Ok(path) => {
                        let url = corefoundation::url::new_file_url(&path, false) as Id;
                        // SAFETY: the delegate implements this optional method, and
                        // the task retains it until the callback returns.
                        unsafe {
                            let f: unsafe extern "C" fn(Id, Sel, Id, Id, Id) =
                                std::mem::transmute(objc::msg::entry());
                            f(
                                delegate as Id,
                                objc::sel::get(
                                    "URLSession:downloadTask:didFinishDownloadingToURL:",
                                ),
                                session as Id,
                                task_id as Id,
                                url,
                            );
                            corefoundation::api::CFRelease(url as CFTypeRef);
                        }
                        std::ptr::null_mut()
                    }
                    Err(message) => crate::error::make(
                        crate::error::NSURL_ERROR_DOMAIN,
                        crate::error::NSURL_ERROR_UNKNOWN,
                        &message,
                    ),
                };
                // SAFETY: the delegate and session are retained by this task.
                unsafe {
                    let f: unsafe extern "C" fn(Id, Sel, Id, Id, Id) =
                        std::mem::transmute(objc::msg::entry());
                    f(
                        delegate as Id,
                        objc::sel::get("URLSession:task:didCompleteWithError:"),
                        session as Id,
                        task_id as Id,
                        error,
                    );
                }
            }
        } else {
            let (data, response, error) = objects(perform(&request));
            // SAFETY: the block was retained when the task was made.
            unsafe { call_completion(completion as Id, data, response, error) };
        }
        objc::arc::pool_pop(pool);
        objc::block::release(completion as Id);
        objc::arc::release(delegate as Id);
        objc::arc::release(session as Id);
        objc::arc::release(task_id as Id);
    });
}

/// `-cancel`, which cannot stop a request already in flight.
///
/// The task is marked canceled so a later `-resume` does nothing, and a request
/// already running is left to finish - its handler still runs, which is what a
/// caller waiting on one needs. Apple's calls the handler with
/// `NSURLErrorCancelled` instead; matching that would need the transport to be
/// interruptible, and claiming it without being able to is worse than saying so
/// here.
unsafe extern "C" fn cancel(receiver: Id, _sel: Sel) {
    // SAFETY: the receiver is a task this module made.
    if let Some(task) = unsafe { contents::<Task>(receiver, task_class()) } {
        task.canceled = true;
    }
}

unsafe extern "C" fn set_task_description(receiver: Id, _sel: Sel, description: Id) {
    // SAFETY: the receiver is a task this module made.
    let Some(task) = (unsafe { contents::<Task>(receiver, task_class()) }) else {
        return;
    };
    let old = std::mem::replace(
        &mut task.description,
        objc::arc::retain(description) as usize,
    );
    objc::arc::release(old as Id);
}

unsafe extern "C" fn task_description(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a task this module made.
    unsafe { contents::<Task>(receiver, task_class()) }
        .map_or(std::ptr::null_mut(), |task| task.description as Id)
}

unsafe extern "C" fn task_current_request(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a task this module made.
    let Some(task) = (unsafe { contents::<Task>(receiver, task_class()) }) else {
        return std::ptr::null_mut();
    };
    objc::arc::autorelease(make(immutable_request_class(), task.request.clone()))
}

unsafe extern "C" fn task_response(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a task this module made.
    unsafe { contents::<Task>(receiver, task_class()) }.map_or(std::ptr::null_mut(), |task| {
        task.response.load(std::sync::atomic::Ordering::Acquire) as Id
    })
}

//

fn session_class() -> &'static Class {
    register::<Session>(c"NSURLSession", None)
}

struct Session {
    delegate: usize,
}

impl Drop for Session {
    fn drop(&mut self) {
        objc::arc::release(self.delegate as Id);
    }
}

/// `+sharedSession`, and `+sessionWithConfiguration:` - which is the same
/// session, because there is one connection pool and no configuration this
/// runtime reads.
unsafe extern "C" fn shared_session(_class: Id, _sel: Sel) -> Id {
    static SESSION: OnceLock<usize> = OnceLock::new();
    *SESSION.get_or_init(|| {
        let session = make(session_class(), Session { delegate: 0 });
        // Retained forever: a singleton a guest can release is one it can make
        // dangle by releasing what it never owned.
        objc::arc::make_immortal(session);
        session as usize
    }) as Id
}

unsafe extern "C" fn session_with_configuration(class: Id, sel: Sel, _configuration: Id) -> Id {
    // SAFETY: `+sharedSession` reads neither argument.
    unsafe { shared_session(class, sel) }
}

unsafe extern "C" fn session_with_configuration_delegate_queue(
    class: Id,
    sel: Sel,
    configuration: Id,
    delegate: Id,
    _queue: Id,
) -> Id {
    let _ = (class, sel, configuration);
    objc::arc::autorelease(make(
        session_class(),
        Session {
            delegate: objc::arc::retain(delegate) as usize,
        },
    ))
}

unsafe extern "C" fn invalidate_session(_receiver: Id, _sel: Sel) {}

unsafe extern "C" fn data_task_with_request(
    _receiver: Id,
    _sel: Sel,
    request: Id,
    completion: Id,
) -> Id {
    // SAFETY: a request this module made, or nil.
    let Some(request) = (unsafe { request_of(request) }) else {
        return std::ptr::null_mut();
    };
    autoreleased_task(request.clone(), completion)
}

unsafe extern "C" fn data_task_with_url(_receiver: Id, _sel: Sel, url: Id, completion: Id) -> Id {
    // SAFETY: an `NSURL`, or nil.
    let Some(text) = (unsafe { crate::url::text_of(url) }) else {
        return std::ptr::null_mut();
    };
    autoreleased_task(Request::new(text, DEFAULT_TIMEOUT_SECONDS), completion)
}

fn autoreleased_task(request: Request, completion: Id) -> Id {
    let task = Task {
        request,
        // Copied here and released once the handler has run, because the block
        // a guest passes inline is stack-allocated and its scope ends the moment
        // this method returns.
        completion: objc::block::copy(completion) as usize,
        delegate: 0,
        session: 0,
        download: false,
        description: 0,
        response: std::sync::atomic::AtomicUsize::new(0),
        started: false,
        canceled: false,
    };
    objc::arc::autorelease(make(task_class(), task))
}

unsafe extern "C" fn download_task_with_request(receiver: Id, _sel: Sel, request: Id) -> Id {
    download_task(receiver, request, std::ptr::null_mut())
}

unsafe extern "C" fn download_task_with_request_handler(
    receiver: Id,
    _sel: Sel,
    request: Id,
    completion: Id,
) -> Id {
    download_task(receiver, request, completion)
}

fn download_task(receiver: Id, request: Id, completion: Id) -> Id {
    // SAFETY: a request and a session this module made.
    let Some(request) = (unsafe { request_of(request) }) else {
        return std::ptr::null_mut();
    };
    let Some(session) = (unsafe { contents::<Session>(receiver, session_class()) }) else {
        return std::ptr::null_mut();
    };
    objc::arc::autorelease(make(
        download_task_class(),
        Task {
            request: request.clone(),
            completion: objc::block::copy(completion) as usize,
            delegate: objc::arc::retain(session.delegate as Id) as usize,
            session: objc::arc::retain(receiver) as usize,
            download: true,
            description: 0,
            response: std::sync::atomic::AtomicUsize::new(0),
            started: false,
            canceled: false,
        },
    ))
}

//

/// `+[NSURLConnection sendSynchronousRequest:returningResponse:error:]`.
unsafe extern "C" fn send_synchronous_request(
    _class: Id,
    _sel: Sel,
    request: Id,
    response_out: *mut Id,
    error_out: *mut Id,
) -> Id {
    // SAFETY: a request this module made, or nil.
    let Some(request) = (unsafe { request_of(request) }) else {
        return std::ptr::null_mut();
    };
    let (data, response, error) = objects(perform(&request.clone()));
    // Both out-parameters are optional, which is why each is checked: a caller
    // that wants only the data passes NULL for the others.
    // SAFETY: each is null or a writable `NSURLResponse *` / `NSError *` the
    // caller owns.
    unsafe {
        if !response_out.is_null() {
            *response_out = response;
        }
        if !error_out.is_null() {
            *error_out = error;
        }
    }
    data
}

//

/// A `NSURLConnection`: the request it runs, and who hears about it.
struct Connection {
    request: Request,
    /// Retained from `-init...` until `-start` hands ownership to the worker
    /// thread (zeroed there) - Apple documents the connection retaining its
    /// delegate until it finishes, fails, or is canceled, and this is that
    /// retain's whole lifetime split across the two places that end it.
    delegate: usize,
    /// A retained `NSOperationQueue`, or 0 for the default described at
    /// [`deliver`]. Set by `-setDelegateQueue:`.
    delegate_queue: usize,
    started: bool,
    cancelled: AtomicBool,
    delivery: Arc<ConnectionDelivery>,
}

#[derive(Default)]
struct ConnectionDelivery {
    source: OnceLock<Arc<corefoundation::runloop::Source>>,
    loops: Mutex<Vec<(Arc<corefoundation::runloop::RunLoop>, String)>>,
    pending: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    explicitly_scheduled: AtomicBool,
}

impl ConnectionDelivery {
    fn schedule(
        &self,
        receiver: Id,
        run_loop: Arc<corefoundation::runloop::RunLoop>,
        mode: String,
    ) {
        let source = self.source.get_or_init(|| {
            corefoundation::runloop::new_source(Some(run_connection_delivery), receiver as usize)
        });
        let mut loops = self.loops.lock().unwrap();
        if !loops
            .iter()
            .any(|(l, m)| Arc::ptr_eq(l, &run_loop) && *m == mode)
        {
            run_loop.add_source(source, &mode);
            loops.push((run_loop, mode));
        }
        if self.pending.lock().unwrap().is_some() {
            source.signal();
            for (run_loop, _) in loops.iter() {
                run_loop.wake();
            }
        }
    }
}

unsafe extern "C" fn run_connection_delivery(context: *mut std::ffi::c_void) {
    // The pending callback retains the connection until it has run.
    let Some(connection) = (unsafe { connection_of(context) }) else {
        return;
    };
    let delivery = connection.delivery.clone();
    let work = delivery.pending.lock().unwrap().take();
    if let Some(work) = work {
        if let Some(source) = delivery.source.get() {
            source.invalidate();
            for (run_loop, mode) in delivery.loops.lock().unwrap().drain(..) {
                run_loop.remove_source(source, &mode);
            }
        }
        let pool = objc::arc::pool_push();
        work();
        objc::arc::pool_pop(pool);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(source) = self.delivery.source.get() {
            source.invalidate();
            for (run_loop, mode) in self.delivery.loops.lock().unwrap().iter() {
                run_loop.remove_source(source, mode);
            }
        }
        // Null once `-start` has taken them for the worker thread to release,
        // so this is only live for a connection that never ran.
        objc::arc::release(self.delegate as Id);
        objc::arc::release(self.delegate_queue as Id);
    }
}

// SAFETY: every field is a plain value or an object referenced by its address
// alone; the worker thread this module spawns reaches them only through a
// retained id, the way `Task` above already does.
unsafe impl Send for Connection {}

fn connection_class() -> &'static Class {
    register::<Connection>(c"NSURLConnection", None)
}

/// # Safety
///
/// `receiver` must be null or an object this runtime allocated.
unsafe fn connection_of<'a>(receiver: Id) -> Option<&'a mut Connection> {
    unsafe { contents(receiver, connection_class()) }
}

fn connection_contents(request: &Request, delegate: Id) -> Connection {
    Connection {
        request: request.clone(),
        delegate: objc::arc::retain(delegate) as usize,
        delegate_queue: 0,
        started: false,
        cancelled: AtomicBool::new(false),
        delivery: Arc::new(ConnectionDelivery::default()),
    }
}

unsafe extern "C" fn connection_with_request_delegate(
    _class: Id,
    _sel: Sel,
    request: Id,
    delegate: Id,
) -> Id {
    // SAFETY: a request this module made, or nil.
    let Some(request) = (unsafe { request_of(request) }) else {
        return std::ptr::null_mut();
    };
    let connection = make(connection_class(), connection_contents(request, delegate));
    if !connection.is_null() {
        // `+connectionWithRequest:delegate:` schedules and starts at once,
        // unlike the `startImmediately:NO` form of `-init`.
        unsafe { start(connection, objc::sel::get("start")) };
    }
    objc::arc::autorelease(connection)
}

unsafe extern "C" fn init_with_request_delegate(
    receiver: Id,
    _sel: Sel,
    request: Id,
    delegate: Id,
) -> Id {
    unsafe {
        init_with_request_delegate_start_immediately(
            receiver,
            objc::sel::get("initWithRequest:delegate:startImmediately:"),
            request,
            delegate,
            1,
        )
    }
}

unsafe extern "C" fn init_with_request_delegate_start_immediately(
    receiver: Id,
    _sel: Sel,
    request: Id,
    delegate: Id,
    start_immediately: i8,
) -> Id {
    // SAFETY: a request this module made, or nil.
    let Some(request) = (unsafe { request_of(request) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `receiver` was just `+alloc`'d, and this is this class's `-init`.
    unsafe { fill(receiver, connection_contents(request, delegate)) };
    if start_immediately != 0 {
        unsafe { start(receiver, objc::sel::get("start")) };
    }
    receiver
}

/// `-start`: hand the request to a worker thread, which reports back through
/// [`deliver`] once it has an answer.
///
/// A thread of its own, like `NSURLSessionTask`'s `-resume` above: the caller
/// of an asynchronous API is entitled to its thread back immediately.
unsafe extern "C" fn start(receiver: Id, _sel: Sel) {
    // SAFETY: the receiver is a connection this module made.
    let Some(connection) = (unsafe { connection_of(receiver) }) else {
        return;
    };
    if connection.started {
        return;
    }
    connection.started = true;
    if connection.delegate_queue == 0
        && !connection
            .delivery
            .explicitly_scheduled
            .load(Ordering::Acquire)
    {
        connection.delivery.schedule(
            receiver,
            corefoundation::runloop::current(),
            corefoundation::runloop::DEFAULT_MODE.to_owned(),
        );
    }
    let delegate = std::mem::replace(&mut connection.delegate, 0);
    let delegate_queue = std::mem::replace(&mut connection.delegate_queue, 0);
    if connection.cancelled.load(Ordering::Acquire) {
        // Canceled before it ever ran: no delegate message is owed, so the
        // reference `-init` took is this call's to let go of.
        objc::arc::release(delegate as Id);
        objc::arc::release(delegate_queue as Id);
        return;
    }
    let request = connection.request.clone();
    let connection_id = objc::arc::retain(receiver) as usize;
    std::thread::spawn(move || {
        let pool = objc::arc::pool_push();
        run_connection(connection_id, delegate, delegate_queue, request);
        objc::arc::pool_pop(pool);
        objc::arc::release(connection_id as Id);
    });
}

/// `-cancel`, which - like `NSURLSessionTask`'s above - cannot interrupt a
/// transfer already under way: the worker thread notices this flag once the
/// transfer ends, and reports nothing rather than finishing it off with a
/// delegate message of its own invention.
unsafe extern "C" fn cancel_connection(receiver: Id, _sel: Sel) {
    // SAFETY: the receiver is a connection this module made.
    if let Some(connection) = unsafe { connection_of(receiver) } {
        connection.cancelled.store(true, Ordering::Release);
    }
}

unsafe extern "C" fn schedule_in_run_loop(receiver: Id, _sel: Sel, run_loop: Id, mode: Id) {
    let Some(connection) = (unsafe { connection_of(receiver) }) else {
        return;
    };
    let Some(run_loop) = crate::timer::run_loop_of(run_loop) else {
        return;
    };
    connection
        .delivery
        .explicitly_scheduled
        .store(true, Ordering::Release);
    connection
        .delivery
        .schedule(receiver, run_loop, crate::timer::mode_name(mode));
}

unsafe extern "C" fn unschedule_from_run_loop(receiver: Id, _sel: Sel, run_loop: Id, mode: Id) {
    let Some(connection) = (unsafe { connection_of(receiver) }) else {
        return;
    };
    let Some(run_loop) = crate::timer::run_loop_of(run_loop) else {
        return;
    };
    let mode = crate::timer::mode_name(mode);
    let delivery = &connection.delivery;
    delivery.explicitly_scheduled.store(true, Ordering::Release);
    let mut loops = delivery.loops.lock().unwrap();
    if let Some(source) = delivery.source.get() {
        run_loop.remove_source(source, &mode);
    }
    loops.retain(|(l, m)| !(Arc::ptr_eq(l, &run_loop) && *m == mode));
}

unsafe extern "C" fn set_delegate_queue(receiver: Id, _sel: Sel, queue: Id) {
    // SAFETY: the receiver is a connection this module made.
    let Some(connection) = (unsafe { connection_of(receiver) }) else {
        return;
    };
    let old = std::mem::replace(
        &mut connection.delegate_queue,
        objc::arc::retain(queue) as usize,
    );
    objc::arc::release(old as Id);
}

/// `-originalRequest` and `-currentRequest`, answered the same way.
///
/// FIXME (ACCURACY): a real connection's `-currentRequest` reflects whatever
/// `-connection:willSendRequest:redirectResponse:` returned for the latest
/// hop; this always answers the request the connection started with, which is
/// what `-originalRequest` is supposed to mean.
unsafe extern "C" fn connection_current_request(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a connection this module made.
    let Some(connection) = (unsafe { connection_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    objc::arc::autorelease(make(immutable_request_class(), connection.request.clone()))
}

/// `+[NSURLConnection sendAsynchronousRequest:queue:completionHandler:]`.
unsafe extern "C" fn send_asynchronous_request(
    _class: Id,
    _sel: Sel,
    request: Id,
    queue: Id,
    completion: Id,
) {
    // SAFETY: a request this module made, or nil.
    let Some(request) = (unsafe { request_of(request) }) else {
        return;
    };
    let request = request.clone();
    let completion = objc::block::copy(completion) as usize;
    let queue_id = objc::arc::retain(queue) as usize;
    std::thread::spawn(move || {
        let pool = objc::arc::pool_push();
        // Carried as `usize`, like every other object a worker thread here
        // hands to `deliver`'s closure: a raw `Id` is a pointer, and a pointer
        // is not `Send`, even though everything it points to already is.
        let (data, response, error) = retained_objects(perform(&request));
        let (data, response, error) = (data as usize, response as usize, error as usize);
        objc::arc::pool_pop(pool);
        deliver(queue_id as Id, move || {
            let (data, response, error) = (data as Id, response as Id, error as Id);
            // SAFETY: the block was retained above; its shape is
            // `void (^)(NSURLResponse *, NSData *, NSError *)` - the response
            // and the data swap places from `NSURLSession`'s handler.
            unsafe { call_completion(completion as Id, response, data, error) };
            objc::arc::release(response);
            objc::arc::release(data);
            objc::arc::release(error);
            objc::block::release(completion as Id);
            objc::arc::release(queue_id as Id);
        });
    });
}

/// Like [`objects`], but at +1 rather than autoreleased: for a result that is
/// about to cross to another thread before anything runs there, where this
/// thread's own pool would otherwise free it first.
fn retained_objects(result: Result<(Response, Vec<u8>), String>) -> (Id, Id, Id) {
    match result {
        Ok((response, bytes)) => {
            let data = corefoundation::collections::new_data(bytes) as Id;
            let response = make(response_class(), response);
            (data, response, std::ptr::null_mut())
        }
        Err(message) => {
            // `crate::error::make` autoreleases; retained here so the error
            // outlives this thread's pool instead of the pool outliving it.
            let error = crate::error::make(
                crate::error::NSURL_ERROR_DOMAIN,
                crate::error::NSURL_ERROR_UNKNOWN,
                &message,
            );
            (
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                objc::arc::retain(error),
            )
        }
    }
}

/// Run `request` and report the outcome to `delegate`, unless `-cancel` beat
/// it there. The worker thread's whole job after `-start`.
fn run_connection(connection_id: usize, delegate: usize, delegate_queue: usize, request: Request) {
    let result = perform(&request);
    // SAFETY: `connection_id` is retained for the life of this call by `-start`.
    let canceled = unsafe { connection_of(connection_id as Id) }
        .is_some_and(|c| c.cancelled.load(Ordering::Acquire));
    if canceled {
        // Apple sends no further delegate messages once a connection is
        // canceled. The transfer already ran to completion - it cannot be
        // interrupted once started - but nothing is said about how.
        objc::arc::release(delegate as Id);
        objc::arc::release(delegate_queue as Id);
        return;
    }
    match result {
        Ok((response, bytes)) => deliver_success(
            connection_id,
            delegate,
            delegate_queue,
            &request,
            response,
            bytes,
        ),
        Err(message) => deliver_failure(connection_id, delegate, delegate_queue, message),
    }
}

/// Report a completed transfer to the delegate: the redirect it followed, if
/// any, then the response, the body, and that loading finished.
fn deliver_success(
    connection_id: usize,
    delegate: usize,
    delegate_queue: usize,
    request: &Request,
    response: Response,
    bytes: Vec<u8>,
) {
    // FIXME (ACCURACY): Apple calls `-connection:willSendRequest:redirectResponse:`
    // once per hop, before following it, with that hop's own 3xx response, and
    // either request may rewrite the next one or cancel the connection outright.
    // This transport follows redirects itself, so by the time this runs the
    // transfer is already over: only the final URL survived, there is no
    // intermediate response to show, and nothing the delegate returns can
    // change what already happened. What follows reports one redirect, from
    // the original URL straight to the final one, with the final response
    // standing in for the discarded 3xx, and ignores the delegate's answer.
    let redirected = response.url != request.url;
    // Carried as `usize` across the closure below, like everywhere else a
    // worker thread here hands an object to `deliver`: a raw `Id` is a
    // pointer, and a pointer is not `Send`, even though what it points to
    // already is.
    let redirect_request = redirected.then(|| {
        let mut redirected_request = request.clone();
        redirected_request.url = response.url.clone();
        make(immutable_request_class(), redirected_request) as usize
    });
    let has_body = !bytes.is_empty();
    let response_object = make(response_class(), response) as usize;
    let data_object = corefoundation::collections::new_data(bytes) as usize;
    let connection = objc::arc::retain(connection_id as Id) as usize;

    deliver_connection(connection_id, delegate_queue as Id, move || {
        let connection = connection as Id;
        let delegate = delegate as Id;
        let response_object = response_object as Id;
        let data_object = data_object as Id;
        // SAFETY: every object handed to a delegate method below was built
        // for exactly this call, and each helper releases what it is given.
        unsafe {
            if let Some(redirect_request) = redirect_request {
                send_will_send_request(
                    delegate,
                    connection,
                    redirect_request as Id,
                    objc::arc::retain(response_object),
                );
            }
            send_connection_arg(
                delegate,
                objc::sel::get("connection:didReceiveResponse:"),
                connection,
                response_object,
            );
            if has_body {
                // FIXME (ACCURACY): the whole body is read before any of it
                // is delivered (see `perform`), so this one call stands in
                // for the several a real streaming transfer would make.
                send_connection_arg(
                    delegate,
                    objc::sel::get("connection:didReceiveData:"),
                    connection,
                    data_object,
                );
            } else {
                objc::arc::release(data_object);
            }
            send_connection_only(
                delegate,
                objc::sel::get("connectionDidFinishLoading:"),
                connection,
            );
        }
        objc::arc::release(delegate);
        objc::arc::release(delegate_queue as Id);
        objc::arc::release(connection);
    });
}

fn deliver_failure(connection_id: usize, delegate: usize, delegate_queue: usize, message: String) {
    // `crate::error::make` autoreleases; retained here for the same reason as
    // in `retained_objects`, and carried as `usize` for the same reason as in
    // `deliver_success`.
    let error = objc::arc::retain(crate::error::make(
        crate::error::NSURL_ERROR_DOMAIN,
        crate::error::NSURL_ERROR_UNKNOWN,
        &message,
    )) as usize;
    let connection = objc::arc::retain(connection_id as Id) as usize;

    deliver_connection(connection_id, delegate_queue as Id, move || {
        let connection = connection as Id;
        let delegate = delegate as Id;
        let error = error as Id;
        // SAFETY: `error` was built for exactly this call.
        unsafe {
            send_connection_arg(
                delegate,
                objc::sel::get("connection:didFailWithError:"),
                connection,
                error,
            );
        }
        objc::arc::release(delegate);
        objc::arc::release(delegate_queue as Id);
        objc::arc::release(connection);
    });
}

/// Run `work` on the connection's run loop, or on `delegate_queue` if one was
/// set with `-setDelegateQueue:`.
///
/// A source wakes exactly the scheduled loops and is serviced only in their
/// requested modes, including private modes used by synchronous callers.
fn deliver_connection(
    connection_id: usize,
    delegate_queue: Id,
    work: impl FnOnce() + Send + 'static,
) {
    if !delegate_queue.is_null() {
        deliver(delegate_queue, work);
        return;
    }
    // The worker and the callback each hold a connection reference.
    let delivery = unsafe { connection_of(connection_id as Id) }
        .unwrap()
        .delivery
        .clone();
    *delivery.pending.lock().unwrap() = Some(Box::new(work));
    if let Some(source) = delivery.source.get() {
        source.signal();
    }
    for (run_loop, _) in delivery.loops.lock().unwrap().iter() {
        run_loop.wake();
    }
}

fn deliver(delegate_queue: Id, work: impl FnOnce() + Send + 'static) {
    if !delegate_queue.is_null() {
        let operation = crate::operation::with_work(work);
        crate::operation::add_to_queue(delegate_queue, operation);
        return;
    }
    let boxed: Box<dyn FnOnce() + Send> = Box::new(work);
    let context = Box::into_raw(Box::new(boxed)) as u64;
    let function = run_boxed as *const () as u64;
    // SAFETY: `context` is a box this call made, consumed exactly once by
    // `run_boxed` when the main queue runs it.
    unsafe {
        darwin::dispatch::queue::async_f(darwin::dispatch::queue::main_queue(), context, function)
    };
}

/// `dispatch_function_t` for whatever [`deliver`] boxed.
unsafe extern "C" fn run_boxed(context: *mut std::ffi::c_void) {
    // SAFETY: a box `deliver` made, consumed exactly once.
    let work = unsafe { Box::from_raw(context.cast::<Box<dyn FnOnce() + Send>>()) };
    work();
}

/// The delegate's implementation of `sel`, if it has one - this module's
/// `-respondsToSelector:` check for every optional delegate method below.
///
/// # Safety
///
/// `delegate` must be null or a live object.
unsafe fn delegate_method(delegate: Id, sel: Sel) -> Option<&'static objc::class::Method> {
    if delegate.is_null() {
        return None;
    }
    unsafe { objc::object::class_of(delegate) }.and_then(|c| c.method(sel))
}

/// Send `connection:<verb>:` with one more argument, if the delegate
/// implements `sel`. `argument` is consumed either way: released once sent,
/// or released here if there was nowhere to send it.
///
/// # Safety
///
/// `delegate` and `connection` must be null or live objects, and `sel`'s
/// method, if any, must be `void (id, SEL, id, id)`.
unsafe fn send_connection_arg(delegate: Id, sel: Sel, connection: Id, argument: Id) {
    if let Some(method) = unsafe { delegate_method(delegate, sel) } {
        // SAFETY: the caller's guarantee.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id) =
                std::mem::transmute(method.implementation());
            f(delegate, sel, connection, argument);
        }
    }
    objc::arc::release(argument);
}

/// `connectionDidFinishLoading:`, whose one argument is the connection itself
/// rather than something built for this call - so there is nothing to release
/// afterward, unlike [`send_connection_arg`].
///
/// # Safety
///
/// `delegate` and `connection` must be null or live objects, and `sel`'s
/// method, if any, must be `void (id, SEL, id)`.
unsafe fn send_connection_only(delegate: Id, sel: Sel, connection: Id) {
    if let Some(method) = unsafe { delegate_method(delegate, sel) } {
        // SAFETY: the caller's guarantee.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(method.implementation());
            f(delegate, sel, connection);
        }
    }
}

/// `connection:willSendRequest:redirectResponse:`. Both arguments are
/// consumed, and the delegate's answer is ignored - see the FIXME in
/// [`deliver_success`], the only caller.
///
/// # Safety
///
/// As [`send_connection_arg`], but the method returns an `id` this call never
/// reads and takes one argument more.
unsafe fn send_will_send_request(delegate: Id, connection: Id, request: Id, redirect_response: Id) {
    let sel = objc::sel::get("connection:willSendRequest:redirectResponse:");
    if let Some(method) = unsafe { delegate_method(delegate, sel) } {
        // SAFETY: the caller's guarantee.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id, Id) -> Id =
                std::mem::transmute(method.implementation());
            let _ = f(delegate, sel, connection, request, redirect_response);
        }
    }
    objc::arc::release(request);
    objc::arc::release(redirect_response);
}

//

/// # Safety
///
/// `string` must be null or a string object.
unsafe fn text(string: Id) -> String {
    if string.is_null() {
        return String::new();
    }
    String::from_utf8_lossy(&unsafe { crate::string::bytes(string) }).into_owned()
}

fn autoreleased_string(text: &str) -> Id {
    let string = crate::string::from_str(text);
    // SAFETY: a string this call just created.
    unsafe { corefoundation::api::CFAutorelease(string as CFTypeRef) as Id }
}

/// Every class this module answers for.
pub const CLASSES: &[&str] = &[
    "NSURLRequest",
    "NSMutableURLRequest",
    "NSURLResponse",
    "NSHTTPURLResponse",
    "NSURLSession",
    "NSURLSessionTask",
    "NSURLSessionDataTask",
    "NSURLSessionDownloadTask",
    "NSURLConnection",
    "NSError",
];

pub(crate) fn install() {
    let mutable = request_class();
    let immutable = immutable_request_class();
    // The accessors go on the immutable class, which the mutable one inherits;
    // the setters would too, and are on it only because that is where a reader
    // looks for them.
    class_methods!(immutable, {
        "requestWithURL:" => request_with_url, c"@24@0:8@16";
        "requestWithURL:cachePolicy:timeoutInterval:" => request_with_url_policy_timeout, c"@40@0:8@16Q24d32";
    });
    class_methods!(mutable, {
        "requestWithURL:" => request_with_url, c"@24@0:8@16";
        "requestWithURL:cachePolicy:timeoutInterval:" => request_with_url_policy_timeout, c"@40@0:8@16Q24d32";
    });
    methods!(immutable, {
        "initWithURL:" => init_with_url, c"@24@0:8@16";
        "URL" => request_url, c"@16@0:8";
        "HTTPMethod" => http_method, c"@16@0:8";
        "HTTPBody" => http_body, c"@16@0:8";
        "valueForHTTPHeaderField:" => value_for_header, c"@24@0:8@16";
        "allHTTPHeaderFields" => request_headers, c"@16@0:8";
        "timeoutInterval" => timeout_interval, c"d16@0:8";
        "cachePolicy" => cache_policy, c"Q16@0:8";
        "setURL:" => set_url, c"v24@0:8@16";
        "setHTTPMethod:" => set_http_method, c"v24@0:8@16";
        "setHTTPBody:" => set_http_body, c"v24@0:8@16";
        "setValue:forHTTPHeaderField:" => set_value_for_header, c"v32@0:8@16@24";
        "addValue:forHTTPHeaderField:" => add_value_for_header, c"v32@0:8@16@24";
        "setTimeoutInterval:" => set_timeout_interval, c"v24@0:8d16";
        "setCachePolicy:" => set_cache_policy, c"v24@0:8Q16";
        "networkServiceType" => network_service_type, c"Q16@0:8";
        "setNetworkServiceType:" => set_network_service_type, c"v24@0:8Q16";
    });

    let response = response_class();
    let base = objc::class::lookup(c"NSURLResponse").expect("registered by `response_class`");
    methods!(base, {
        "URL" => response_url, c"@16@0:8";
        "MIMEType" => mime_type, c"@16@0:8";
        "textEncodingName" => text_encoding_name, c"@16@0:8";
        "expectedContentLength" => expected_content_length, c"q16@0:8";
    });
    methods!(response, {
        "statusCode" => status_code, c"q16@0:8";
        "allHeaderFields" => all_header_fields, c"@16@0:8";
    });

    let task = task_class();
    download_task_class();
    methods!(task, {
        "resume" => resume, c"v16@0:8";
        "cancel" => cancel, c"v16@0:8";
        "setTaskDescription:" => set_task_description, c"v24@0:8@16";
        "taskDescription" => task_description, c"@16@0:8";
        "currentRequest" => task_current_request, c"@16@0:8";
        "originalRequest" => task_current_request, c"@16@0:8";
        "response" => task_response, c"@16@0:8";
    });

    let session = session_class();
    class_methods!(session, {
        "sharedSession" => shared_session, c"@16@0:8";
        "sessionWithConfiguration:" => session_with_configuration, c"@24@0:8@16";
        "sessionWithConfiguration:delegate:delegateQueue:"
            => session_with_configuration_delegate_queue, c"@40@0:8@16@24@32";
    });
    // `@?` for the handler, which is what tells the emulated backend's argument
    // conversion that the parameter is a block whose `invoke` needs a thunk.
    methods!(session, {
        "dataTaskWithRequest:completionHandler:" => data_task_with_request, c"@32@0:8@16@?24";
        "dataTaskWithURL:completionHandler:" => data_task_with_url, c"@32@0:8@16@?24";
        "downloadTaskWithRequest:" => download_task_with_request, c"@24@0:8@16";
        "downloadTaskWithRequest:completionHandler:"
            => download_task_with_request_handler, c"@32@0:8@16@?24";
        "invalidateAndCancel" => invalidate_session, c"v16@0:8";
        "finishTasksAndInvalidate" => invalidate_session, c"v16@0:8";
    });

    let connection = connection_class();
    class_methods!(connection, {
        "sendSynchronousRequest:returningResponse:error:" => send_synchronous_request, c"@40@0:8@16^@24^@32";
        "connectionWithRequest:delegate:" => connection_with_request_delegate, c"@32@0:8@16@24";
        // `@?` for the handler, like the session methods above.
        "sendAsynchronousRequest:queue:completionHandler:" => send_asynchronous_request, c"v40@0:8@16@24@?32";
    });
    methods!(connection, {
        "initWithRequest:delegate:" => init_with_request_delegate, c"@32@0:8@16@24";
        "initWithRequest:delegate:startImmediately:"
            => init_with_request_delegate_start_immediately, c"@40@0:8@16@24c32";
        "start" => start, c"v16@0:8";
        "cancel" => cancel_connection, c"v16@0:8";
        "scheduleInRunLoop:forMode:" => schedule_in_run_loop, c"v32@0:8@16@24";
        "unscheduleFromRunLoop:forMode:" => unschedule_from_run_loop, c"v32@0:8@16@24";
        "setDelegateQueue:" => set_delegate_queue, c"v24@0:8@16";
        "originalRequest" => connection_current_request, c"@16@0:8";
        "currentRequest" => connection_current_request, c"@16@0:8";
    });
    // `-connection:canAuthenticateAgainstProtectionSpace:` and
    // `-connection:willSendRequestForAuthenticationChallenge:` are
    // intentionally unimplemented: nothing in `perform` intercepts an auth
    // challenge, so there is no cheap way to hand one to a delegate - this
    // tree does whatever the transport does on its own, same as a plain
    // `NSURLSession` request.
    crate::error::install();
}

#[cfg(test)]
mod tests {
    use super::*;

    static CUSTOM_FINISHED: AtomicBool = AtomicBool::new(false);
    static CUSTOM_THREAD: Mutex<Option<std::thread::ThreadId>> = Mutex::new(None);

    unsafe extern "C" fn custom_finished(_delegate: Id, _sel: Sel, _connection: Id) {
        *CUSTOM_THREAD.lock().unwrap() = Some(std::thread::current().id());
        CUSTOM_FINISHED.store(true, Ordering::SeqCst);
    }

    #[test]
    fn a_connection_waits_for_its_scheduled_run_loop_mode() {
        crate::bootstrap();
        CUSTOM_FINISHED.store(false, Ordering::SeqCst);
        let class = corefoundation::base::register(
            c"RosemaryTestCustomModeDelegate",
            None,
            std::mem::size_of::<usize>(),
            false,
        );
        methods!(class, {
            "connectionDidFinishLoading:" => custom_finished, c"v24@0:8@16";
        });
        let delegate = objc::object::create_instance(class, 0);
        let request = request_object(local_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ));
        let connection = objc::object::create_instance(connection_class(), 0);
        let mode = crate::string::from_str("ConnectionPrivateMode");
        let run_loop_class = objc::class::lookup(c"NSRunLoop").unwrap();
        // SAFETY: registered class method and live objects for each selector.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            let run_loop = send(
                objc::class::ptr(run_loop_class).cast(),
                objc::sel::get("currentRunLoop"),
            );
            init_with_request_delegate_start_immediately(
                connection,
                std::ptr::null(),
                request,
                delegate,
                0,
            );
            schedule_in_run_loop(connection, std::ptr::null(), run_loop, mode);
            start(connection, std::ptr::null());
            let delivery = connection_of(connection).unwrap().delivery.clone();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while delivery.pending.lock().unwrap().is_none() && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(
                delivery.pending.lock().unwrap().is_some(),
                "local response never arrived"
            );
            darwin::dispatch::queue::drain_main();
            let loop_ = corefoundation::runloop::current();
            loop_.run_in_mode(corefoundation::runloop::DEFAULT_MODE, 0.0, true);
            assert!(!CUSTOM_FINISHED.load(Ordering::SeqCst));
            unschedule_from_run_loop(connection, std::ptr::null(), run_loop, mode);
            loop_.run_in_mode("ConnectionPrivateMode", 0.0, true);
            assert!(!CUSTOM_FINISHED.load(Ordering::SeqCst));
            schedule_in_run_loop(connection, std::ptr::null(), run_loop, mode);
            loop_.run_in_mode("ConnectionPrivateMode", 0.1, true);
            assert!(CUSTOM_FINISHED.load(Ordering::SeqCst));
            assert_eq!(
                *CUSTOM_THREAD.lock().unwrap(),
                Some(std::thread::current().id())
            );
        }
        for object in [mode, connection, request, delegate] {
            objc::arc::release(object);
        }
    }

    /// `-initWithURL:` configures the receiver it is sent to, on either the
    /// immutable class or the mutable one that inherits the method from it.
    #[test]
    fn init_with_url_configures_the_request_on_either_class() {
        crate::bootstrap();
        for class_name in [c"NSURLRequest", c"NSMutableURLRequest"] {
            let class = objc::class::lookup(class_name).expect("declared");
            let object = objc::object::create_instance(class, 0);
            let url = corefoundation::url::new("https://example.com/path");
            // SAFETY: `-initWithURL:` is `id (id, SEL, id)`.
            let request =
                unsafe { init_with_url(object, objc::sel::get("initWithURL:"), url as Id) };
            assert!(!request.is_null(), "{class_name:?}");
            // SAFETY: `init_with_url` just handed back a request this module made.
            let parts = unsafe { request_parts(request) }.expect("a request this module made");
            assert_eq!(parts.url, "https://example.com/path");
            assert_eq!(parts.method, "GET");
            objc::arc::release(request);
            // SAFETY: a URL this test just made.
            unsafe { corefoundation::api::CFRelease(url) };
        }
    }

    /// A URL `-initWithURL:` cannot make sense of answers nil, as every other
    /// request constructor in this module does for the same input.
    #[test]
    fn init_with_url_of_a_bad_url_answers_nil() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSURLRequest").expect("declared");
        let object = objc::object::create_instance(class, 0);
        // SAFETY: `-initWithURL:` is `id (id, SEL, id)`.
        let request =
            unsafe { init_with_url(object, objc::sel::get("initWithURL:"), std::ptr::null_mut()) };
        assert!(request.is_null());
    }

    // The peer accepts but deliberately sends no response before the deadline.
    #[test]
    fn a_request_timeout_is_applied_to_the_transport() {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut buffer = [0u8; 1024];
            let _ = stream.read(&mut buffer);
            std::thread::sleep(std::time::Duration::from_secs(1));
        });
        let request = Request::new(format!("http://{address}/"), 0.1);
        let started = std::time::Instant::now();
        let result = perform(&request);
        let elapsed = started.elapsed();
        server.join().unwrap();
        assert!(result.is_err());
        assert!(
            elapsed < std::time::Duration::from_millis(750),
            "request ignored its timeout: {elapsed:?}"
        );
    }

    /// A request built the way a guest builds one, run the way
    /// `+sendSynchronousRequest:` runs it. Against a real server, because what
    /// is being asserted is that this tree's TLS stack completes a handshake -
    /// which is the whole reason the transport is here.
    #[test]
    fn a_https_request_comes_back_with_a_status_and_a_body() {
        crate::bootstrap();
        let request = Request::new("https://example.com/".to_string(), 30.0);
        let (response, body) = match perform(&request) {
            Ok(pair) => pair,
            // A machine with no network is not a failing build. The refusal is
            // still reported, because a silent skip here would let a broken
            // transport look like an offline laptop.
            Err(e) => {
                eprintln!("skipping: no network ({e})");
                return;
            }
        };
        assert_eq!(response.status, 200);
        assert!(!body.is_empty(), "a body came back");
        assert!(
            response
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("content-type")),
            "the headers survived"
        );
    }

    /// The method and the headers a caller set are the ones that go out, which
    /// is what a server echoing them back proves.
    #[test]
    fn a_post_sends_its_method_body_and_headers() {
        crate::bootstrap();
        let mut request = Request::new("https://postman-echo.com/post".to_string(), 30.0);
        request.method = "POST".to_string();
        request
            .headers
            .push(("x-rosemary".to_string(), "1".to_string()));
        request.body = b"a body".to_vec();
        let (response, body) = match perform(&request) {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("skipping: no network ({e})");
                return;
            }
        };
        assert_eq!(response.status, 200);
        let echoed = String::from_utf8_lossy(&body);
        assert!(echoed.contains("a body"), "the body was sent: {echoed}");
        assert!(echoed.contains("x-rosemary"), "the header was sent");
    }

    /// A URL nothing serves is an error rather than a response, and the message
    /// reaches the `NSError` a completion handler is given.
    #[test]
    fn a_failure_becomes_an_error_with_a_message() {
        crate::bootstrap();
        let request = Request::new("https://invalid.invalid/".to_string(), 10.0);
        let Err(message) = perform(&request) else {
            panic!("a name that cannot resolve is not a response");
        };
        assert!(!message.is_empty(), "the failure says something");

        let (data, response, error) = objects(Err(message));
        assert!(data.is_null() && response.is_null());
        assert!(!error.is_null(), "the handler gets an NSError");
        // SAFETY: `-code` is `NSInteger (id, SEL)` on the error just made.
        let sel = objc::sel::get("code");
        let method = unsafe { objc::object::class_of(error) }
            .and_then(|c| c.method(sel))
            .expect("NSError answers -code");
        let code: i64 = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> i64 =
                std::mem::transmute(method.implementation());
            f(error, sel)
        };
        assert_eq!(code, crate::error::NSURL_ERROR_UNKNOWN);
    }

    //
    // `NSURLConnection`, against a local, canned server rather than the
    // network: these exercise the delegate machinery itself, which a real
    // host's status code or body does nothing to test.
    //

    /// A one-shot HTTP server on localhost: it answers the first connection it
    /// gets with `response`, verbatim, and does nothing else.
    fn local_server(response: &str) -> String {
        local_server_sequence(vec![response.to_string()])
    }

    /// Like `local_server`, but for a test that needs more than one request
    /// answered - a redirect, say. Each connection in turn gets the next
    /// response; a request past the end of `responses` gets none.
    fn local_server_sequence(responses: Vec<String>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a local port");
        let addr = listener.local_addr().expect("a bound address");
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{addr}/")
    }

    /// A server whose first connection is told to redirect to its second -
    /// the one case `local_server_sequence` cannot build ahead of time, since
    /// the redirect target names a port nothing picks until the listener binds.
    fn redirect_server(final_body: &str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a local port");
        let addr = listener.local_addr().expect("a bound address");
        let responses = vec![
            format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{addr}/final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ),
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{final_body}",
                final_body.len()
            ),
        ];
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{addr}/start")
    }

    /// Turn the main dispatch queue and loop until `done` is true or the
    /// budget runs out - what a test waits on instead of the real run loop's
    /// own idle sleep, since nothing here drives that loop on its own.
    fn pump_until(done: &std::sync::atomic::AtomicBool) -> bool {
        for _ in 0..500 {
            if done.load(Ordering::SeqCst) {
                return true;
            }
            darwin::dispatch::queue::drain_main();
            corefoundation::runloop::current().run_in_mode(
                corefoundation::runloop::DEFAULT_MODE,
                0.0,
                true,
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    /// A request built the way `perform`'s own tests build one, for a
    /// connection test that needs an `NSURLRequest` object rather than the
    /// plain `Request` those use.
    fn request_object(url: String) -> Id {
        make(request_class(), Request::new(url, 5.0))
    }

    /// An `NSOperationQueue`, for the tests that check delegate-queue
    /// delivery. Built by sending `-init` rather than calling `queue_init`
    /// directly, since that function belongs to `operation` and is not this
    /// module's to call.
    fn new_operation_queue() -> Id {
        let class = objc::class::lookup(c"NSOperationQueue").expect("registered by `operation`");
        let object = objc::object::create_instance(class, 0);
        let sel = objc::sel::get("init");
        // SAFETY: `-init` is `id (id, SEL)`, sent to a fresh instance.
        unsafe {
            let method = objc::object::class_of(object)
                .and_then(|c| c.method(sel))
                .expect("NSOperationQueue answers -init");
            let f: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(method.implementation());
            f(object, sel)
        }
    }

    static HAPPY_STATUS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);
    static HAPPY_BODY: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());
    static HAPPY_FINISHED: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn happy_did_receive_response(
        _delegate: Id,
        _sel: Sel,
        _connection: Id,
        response: Id,
    ) {
        // SAFETY: an `NSHTTPURLResponse` this module made.
        if let Some(response) = unsafe { response_of(response) } {
            HAPPY_STATUS.store(response.status as i64, Ordering::SeqCst);
        }
    }

    unsafe extern "C" fn happy_did_receive_data(
        _delegate: Id,
        _sel: Sel,
        _connection: Id,
        data: Id,
    ) {
        // SAFETY: an `NSData` this module made.
        if let Some(bytes) = unsafe { corefoundation::collections::data(data as CFTypeRef) } {
            HAPPY_BODY
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(bytes.clone());
        }
    }

    unsafe extern "C" fn happy_finished(_delegate: Id, _sel: Sel, _connection: Id) {
        HAPPY_FINISHED.store(true, Ordering::SeqCst);
    }

    /// `-initWithRequest:delegate:` starts at once, and the delegate hears the
    /// response, the body, and the finish, in that order - against a server
    /// this test controls rather than the network.
    #[test]
    fn a_delegate_based_connection_gets_the_response_body_and_finish() {
        crate::bootstrap();
        HAPPY_STATUS.store(-1, Ordering::SeqCst);
        HAPPY_BODY.lock().unwrap_or_else(|e| e.into_inner()).clear();
        HAPPY_FINISHED.store(false, Ordering::SeqCst);

        let class = corefoundation::base::register(
            c"RosemaryTestConnectionHappyDelegate",
            None,
            std::mem::size_of::<usize>(),
            false,
        );
        methods!(class, {
            "connection:didReceiveResponse:" => happy_did_receive_response, c"v32@0:8@16@24";
            "connection:didReceiveData:" => happy_did_receive_data, c"v32@0:8@16@24";
            "connectionDidFinishLoading:" => happy_finished, c"v24@0:8@16";
        });
        let delegate = objc::object::create_instance(class, 0);
        let url =
            local_server("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
        let request = request_object(url);
        let connection = objc::object::create_instance(connection_class(), 0);
        // SAFETY: a fresh connection instance, and a request this module made.
        unsafe {
            init_with_request_delegate(
                connection,
                objc::sel::get("initWithRequest:delegate:"),
                request,
                delegate,
            );
        }

        assert!(
            pump_until(&HAPPY_FINISHED),
            "the delegate never heard the connection finish"
        );
        assert_eq!(HAPPY_STATUS.load(Ordering::SeqCst), 200);
        assert_eq!(
            &*HAPPY_BODY.lock().unwrap_or_else(|e| e.into_inner()),
            b"hello"
        );

        objc::arc::release(delegate);
        objc::arc::release(request);
        objc::arc::release(connection);
    }

    static REDIRECT_SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    static REDIRECT_FINISHED: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn redirect_will_send_request(
        _delegate: Id,
        _sel: Sel,
        _connection: Id,
        request: Id,
        _redirect_response: Id,
    ) -> Id {
        // SAFETY: a request this module made.
        if let Some(r) = unsafe { request_of(request) } {
            REDIRECT_SEEN
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(r.url.clone());
        }
        request
    }

    unsafe extern "C" fn redirect_finished(_delegate: Id, _sel: Sel, _connection: Id) {
        REDIRECT_FINISHED.store(true, Ordering::SeqCst);
    }

    /// A connection that gets redirected reports it once, with the final URL -
    /// see the FIXME on `deliver_success` for what this does not claim: the
    /// real, per-hop version of this callback.
    #[test]
    fn a_redirected_connection_reports_the_final_url_once() {
        crate::bootstrap();
        REDIRECT_SEEN
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        REDIRECT_FINISHED.store(false, Ordering::SeqCst);

        let class = corefoundation::base::register(
            c"RosemaryTestConnectionRedirectDelegate",
            None,
            std::mem::size_of::<usize>(),
            false,
        );
        methods!(class, {
            "connection:willSendRequest:redirectResponse:" => redirect_will_send_request, c"@40@0:8@16@24@32";
            "connectionDidFinishLoading:" => redirect_finished, c"v24@0:8@16";
        });
        let delegate = objc::object::create_instance(class, 0);
        let url = redirect_server("done");
        let request = request_object(url);
        let connection = objc::object::create_instance(connection_class(), 0);
        // SAFETY: a fresh connection instance, and a request this module made.
        unsafe {
            init_with_request_delegate(
                connection,
                objc::sel::get("initWithRequest:delegate:"),
                request,
                delegate,
            );
        }

        assert!(
            pump_until(&REDIRECT_FINISHED),
            "the delegate never heard the connection finish"
        );
        let seen = REDIRECT_SEEN.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            seen.len(),
            1,
            "exactly one redirect should be reported: {seen:?}"
        );
        assert!(
            seen[0].ends_with("/final"),
            "the delegate saw the final URL: {seen:?}"
        );
        drop(seen);

        objc::arc::release(delegate);
        objc::arc::release(request);
        objc::arc::release(connection);
    }

    static FAILURE_CODE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static FAILURE_SEEN: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn failure_did_fail(_delegate: Id, _sel: Sel, _connection: Id, error: Id) {
        let sel = objc::sel::get("code");
        // SAFETY: an `NSError` this module made.
        if let Some(method) = unsafe { objc::object::class_of(error) }.and_then(|c| c.method(sel)) {
            let code: i64 = unsafe {
                let f: unsafe extern "C" fn(Id, Sel) -> i64 =
                    std::mem::transmute(method.implementation());
                f(error, sel)
            };
            FAILURE_CODE.store(code, Ordering::SeqCst);
        }
        FAILURE_SEEN.store(true, Ordering::SeqCst);
    }

    /// Nothing listening on a loopback port fails the way a bad host does -
    /// immediately, and without needing a real network to prove it - and the
    /// delegate hears about it as an `NSError`.
    #[test]
    fn a_connection_failure_reaches_the_delegate_as_an_error() {
        crate::bootstrap();
        FAILURE_CODE.store(0, Ordering::SeqCst);
        FAILURE_SEEN.store(false, Ordering::SeqCst);

        let class = corefoundation::base::register(
            c"RosemaryTestConnectionFailureDelegate",
            None,
            std::mem::size_of::<usize>(),
            false,
        );
        methods!(class, {
            "connection:didFailWithError:" => failure_did_fail, c"v32@0:8@16@24";
        });
        let delegate = objc::object::create_instance(class, 0);
        let request = request_object("http://127.0.0.1:1/".to_string());
        let connection = objc::object::create_instance(connection_class(), 0);
        // SAFETY: a fresh connection instance, and a request this module made.
        unsafe {
            init_with_request_delegate(
                connection,
                objc::sel::get("initWithRequest:delegate:"),
                request,
                delegate,
            );
        }

        assert!(
            pump_until(&FAILURE_SEEN),
            "the delegate never heard about the failure"
        );
        assert_eq!(
            FAILURE_CODE.load(Ordering::SeqCst),
            crate::error::NSURL_ERROR_UNKNOWN
        );

        objc::arc::release(delegate);
        objc::arc::release(request);
        objc::arc::release(connection);
    }

    static CANCELED_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    unsafe extern "C" fn canceled_any_call(_delegate: Id, _sel: Sel, _connection: Id, _arg: Id) {
        CANCELED_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    unsafe extern "C" fn canceled_finished(_delegate: Id, _sel: Sel, _connection: Id) {
        CANCELED_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    /// `-cancel`, sent right after `-start`, beats a response that is still
    /// on its way - the transfer finishes (this transport cannot interrupt
    /// it), but the delegate is told nothing at all about how.
    #[test]
    fn a_canceled_connection_sends_no_delegate_messages() {
        crate::bootstrap();
        CANCELED_CALLS.store(0, Ordering::SeqCst);

        let class = corefoundation::base::register(
            c"RosemaryTestConnectionCanceledDelegate",
            None,
            std::mem::size_of::<usize>(),
            false,
        );
        methods!(class, {
            "connection:didReceiveResponse:" => canceled_any_call, c"v32@0:8@16@24";
            "connection:didReceiveData:" => canceled_any_call, c"v32@0:8@16@24";
            "connection:didFailWithError:" => canceled_any_call, c"v32@0:8@16@24";
            "connectionDidFinishLoading:" => canceled_finished, c"v24@0:8@16";
        });
        let delegate = objc::object::create_instance(class, 0);

        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a local port");
        let addr = listener.local_addr().expect("a bound address");
        std::thread::spawn(move || {
            // Answered, but only after `-cancel` has had time to run - proving
            // this is about a message withheld, not a request too slow to
            // finish at all.
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(50)));
            let _ = stream.read(&mut buf);
            std::thread::sleep(std::time::Duration::from_millis(300));
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        });

        let request = request_object(format!("http://{addr}/"));
        let connection = objc::object::create_instance(connection_class(), 0);
        // SAFETY: a fresh connection instance, and a request this module made.
        unsafe {
            init_with_request_delegate(
                connection,
                objc::sel::get("initWithRequest:delegate:"),
                request,
                delegate,
            );
            cancel_connection(connection, objc::sel::get("cancel"));
        }

        // Long enough for the server's reply and this runtime's own delivery
        // to have arrived, if either were going to.
        for _ in 0..60 {
            darwin::dispatch::queue::drain_main();
            corefoundation::runloop::current().run_in_mode(
                corefoundation::runloop::DEFAULT_MODE,
                0.0,
                true,
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            CANCELED_CALLS.load(Ordering::SeqCst),
            0,
            "a canceled connection told its delegate something"
        );

        objc::arc::release(delegate);
        objc::arc::release(request);
        objc::arc::release(connection);
    }

    static ASYNC_SEEN: std::sync::Mutex<Option<(i64, Vec<u8>, bool)>> = std::sync::Mutex::new(None);
    static ASYNC_CALLED: AtomicBool = AtomicBool::new(false);

    /// `void (^)(NSURLResponse *, NSData *, NSError *)` - response first, the
    /// opposite order from `NSURLSession`'s handler.
    unsafe extern "C" fn async_handler(_block: Id, response: Id, data: Id, error: Id) {
        // SAFETY: a response and data this module made, or nil.
        let status = unsafe { response_of(response) }.map_or(-1, |r| r.status as i64);
        let bytes = unsafe { corefoundation::collections::data(data as CFTypeRef) }
            .cloned()
            .unwrap_or_default();
        *ASYNC_SEEN.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((status, bytes, error.is_null()));
        ASYNC_CALLED.store(true, Ordering::SeqCst);
    }

    /// `+sendAsynchronousRequest:queue:completionHandler:` runs the handler on
    /// the queue it was given, with the response, the body, and a nil error.
    #[test]
    fn send_asynchronous_request_calls_the_handler_on_its_queue() {
        crate::bootstrap();
        *ASYNC_SEEN.lock().unwrap_or_else(|e| e.into_inner()) = None;
        ASYNC_CALLED.store(false, Ordering::SeqCst);

        let url =
            local_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        let request = request_object(url);
        let block = crate::block::fake::block(async_handler as *const std::ffi::c_void);
        let queue = new_operation_queue();
        // SAFETY: the prototype is this class method's, and `block` outlives
        // the call - it is a global block, so nothing here ever copies it.
        unsafe {
            send_asynchronous_request(
                std::ptr::null_mut(),
                objc::sel::get("sendAsynchronousRequest:queue:completionHandler:"),
                request,
                queue,
                block.id(),
            );
        }

        assert!(pump_until(&ASYNC_CALLED), "the handler never ran");
        let seen = ASYNC_SEEN
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .expect("the handler ran");
        assert_eq!(seen.0, 200);
        assert_eq!(seen.1, b"ok");
        assert!(seen.2, "no error on success");

        objc::arc::release(request);
        objc::arc::release(queue);
    }
}
