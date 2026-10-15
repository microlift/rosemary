use std::sync::OnceLock;

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use corefoundation::base::CFTypeRef;

use crate::block;
use crate::boxed::{Body, contents, make, register};
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
    let (mut parts, ()) = built.into_parts();
    parts.extensions.insert(
        ureq::config::Config::builder()
            .timeout_global(Some(timeout))
            .build(),
    );
    // A body is sent for every method that has one; `SendBody::none()` is what
    // makes a GET a GET rather than a POST with nothing in it.
    let body = match request.body.is_empty() {
        true => ureq::SendBody::none(),
        false => ureq::SendBody::from_owned_reader(std::io::Cursor::new(request.body.clone())),
    };
    let mut response = agent()
        .run(ureq::http::Request::from_parts(parts, body))
        .map_err(|e| e.to_string())?;

    let status = response.status().as_u16();
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
            url: request.url.clone(),
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

/// `+[NSURLConnection sendSynchronousRequest:returningResponse:error:]`, which
/// is the whole of `NSURLConnection` a program still uses.
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

    let connection = corefoundation::base::abstract_class(c"NSURLConnection", None);
    class_methods!(connection, {
        "sendSynchronousRequest:returningResponse:error:" => send_synchronous_request, c"@40@0:8@16^@24^@32";
    });

    crate::error::install();
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
