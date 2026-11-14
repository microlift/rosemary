use super::*;

struct NativeLibrary {
    host: HostId,
    device: Id,
}

pub(super) struct NativeFunction {
    pub(super) host: HostId,
    library: Id,
}

struct NativeComputePipeline {
    host: HostId,
    device: Id,
}

struct NativeComputeEncoder {
    host: HostId,
    command: Id,
}

pub(super) fn install() {
    let library = object::register::<NativeLibrary>(c"RosemaryNativeMTLLibrary");
    methods!(library, {
        "device" => library_device, c"@16@0:8";
        "functionNames" => library_function_names, c"@16@0:8";
        "newFunctionWithName:" => new_function, c"@24@0:8@16";
        "dealloc" => library_dealloc, c"v16@0:8";
    });
    let function = object::register::<NativeFunction>(c"RosemaryNativeMTLFunction");
    methods!(function, {
        "device" => function_device, c"@16@0:8";
        "dealloc" => function_dealloc, c"v16@0:8";
    });
    let pipeline =
        object::register::<NativeComputePipeline>(c"RosemaryNativeMTLComputePipelineState");
    methods!(pipeline, {
        "device" => pipeline_device, c"@16@0:8";
        "maxTotalThreadsPerThreadgroup" => pipeline_max_threads, c"Q16@0:8";
        "threadExecutionWidth" => pipeline_execution_width, c"Q16@0:8";
        "dealloc" => pipeline_dealloc, c"v16@0:8";
    });
    let encoder =
        object::register::<NativeComputeEncoder>(c"RosemaryNativeMTLComputeCommandEncoder");
    methods!(encoder, {
        "device" => encoder_device, c"@16@0:8";
        "setComputePipelineState:" => set_pipeline, c"v24@0:8@16";
        "setBuffer:offset:atIndex:" => set_buffer, c"v40@0:8@16Q24Q32";
        "setBytes:length:atIndex:" => set_bytes, c"v40@0:8^v16Q24Q32";
        "setTexture:atIndex:" => set_texture, c"v32@0:8@16Q24";
        "setSamplerState:atIndex:" => set_sampler, c"v32@0:8@16Q24";
        "dispatchThreadgroups:threadsPerThreadgroup:" => dispatch_threadgroups,
            c"v64@0:8{MTLSize=QQQ}16{MTLSize=QQQ}40";
        "endEncoding" => end_encoding, c"v16@0:8";
        "dealloc" => encoder_dealloc, c"v16@0:8";
    });
}

fn host_string(value: Id) -> Option<HostId> {
    if value.is_null() || !object::is_kind(value, c"NSString") {
        return None;
    }
    // The returned native NSString is autoreleased into the surrounding HostPool.
    let bytes = unsafe { foundation::string::bytes(value) };
    let string = std::ffi::CString::new(bytes).ok()?;
    let class = unsafe { host_get_class(c"NSString".as_ptr()) };
    (!class.is_null())
        .then(|| unsafe {
            host_send_id_ptr(class, sel(c"stringWithUTF8String:"), string.as_ptr().cast())
        })
        .filter(|s| !s.is_null())
}

pub(super) fn native_error(error: HostId, fallback: &str) -> String {
    if error.is_null() {
        return fallback.to_owned();
    }
    let desc = unsafe { host_send_id(error, sel(c"localizedDescription")) };
    if desc.is_null() {
        return fallback.to_owned();
    }
    let text = unsafe { host_send_ptr(desc, sel(c"UTF8String")) };
    if text.is_null() {
        return fallback.to_owned();
    }
    unsafe { CStr::from_ptr(text.cast()) }
        .to_string_lossy()
        .into_owned()
}

pub(super) fn fail(error: *mut Id, domain: &str, message: &str) -> Id {
    gpu::trace(format!("native Metal: {message}"));
    if !error.is_null() {
        unsafe { *error = foundation::error::make(domain, 1, message) };
    }
    std::ptr::null_mut()
}

/// One retained host library becomes one guest-owned wrapper. Host ids never
/// escape the built-in Objective-C runtime's registered classes.
fn wrap_library(host: HostId, device: Id) -> Id {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    objc::arc::retain(device);
    let guest = object::make(c"RosemaryNativeMTLLibrary", NativeLibrary { host, device });
    if guest.is_null() {
        unsafe { host_release(host) };
        objc::arc::release(device);
    }
    guest
}

/// The input is guest-owned `dispatch_data_t`, which is not ABI-compatible
/// with the host dispatch runtime's object layout. libdispatch's NULL
/// destructor copies the bytes, so the host Metal driver never sees a pointer
/// into the guest dispatch implementation. This never touches the filesystem.
fn library_from_bytes(receiver: Id, bytes: &[u8], error: *mut Id) -> Id {
    if bytes.is_empty() {
        return fail(error, "MTLLibraryErrorDomain", "empty precompiled metallib");
    }
    let _pool = HostPool::new();
    let host_data = unsafe {
        host_dispatch_data_create(
            bytes.as_ptr().cast(),
            bytes.len(),
            std::ptr::null_mut(),
            std::ptr::null_mut(), // DISPATCH_DATA_DESTRUCTOR_DEFAULT: copy the bytes
        )
    };
    if host_data.is_null() {
        return fail(
            error,
            "MTLLibraryErrorDomain",
            "host dispatch_data_create failed",
        );
    }
    let mut host_error = std::ptr::null_mut();
    let host = unsafe {
        host_send_id_id_ptr(
            object::body::<NativeDevice>(receiver).host,
            sel(c"newLibraryWithData:error:"),
            host_data,
            &mut host_error,
        )
    };
    unsafe { host_dispatch_release(host_data) };
    if host.is_null() {
        return fail(
            error,
            "MTLLibraryErrorDomain",
            &native_error(host_error, "native Metal rejected precompiled metallib"),
        );
    }
    wrap_library(host, receiver)
}

pub(super) unsafe extern "C" fn new_library_with_data(
    receiver: Id,
    _sel: Sel,
    guest_data: u64,
    error: *mut Id,
) -> Id {
    if !error.is_null() {
        unsafe { *error = std::ptr::null_mut() };
    }
    let Some((bytes, length)) = darwin::dispatch::data::bytes(guest_data) else {
        return fail(
            error,
            "MTLLibraryErrorDomain",
            "expected guest dispatch_data_t",
        );
    };
    if bytes.is_null() || length == 0 {
        return fail(
            error,
            "MTLLibraryErrorDomain",
            "empty guest dispatch_data_t",
        );
    }
    // SAFETY: the guest dispatch object retains these bytes for the call;
    // host_dispatch_data_create copies them synchronously before returning.
    let source = unsafe { std::slice::from_raw_parts(bytes, length) };
    library_from_bytes(receiver, source, error)
}

pub(super) unsafe extern "C" fn new_default_library(receiver: Id, _sel: Sel) -> Id {
    // Use the *guest* bundle, not Rosemary's host bundle, to find default.metallib.
    let Some(path) =
        foundation::bundle::main().and_then(|bundle| bundle.resource("default", "metallib"))
    else {
        gpu::trace("native Metal: guest bundle has no default.metallib".to_string());
        return std::ptr::null_mut();
    };
    match std::fs::read(&path) {
        Ok(bytes) => library_from_bytes(receiver, &bytes, std::ptr::null_mut()),
        Err(reason) => {
            gpu::trace(format!(
                "native Metal: cannot read {}: {reason}",
                path.display()
            ));
            std::ptr::null_mut()
        }
    }
}

/// Convert the built-in compile options into host runtime objects. An
/// invalid macro name/value is rejected rather than silently ignored.
fn host_compile_options(guest: Id) -> Result<HostId, String> {
    if guest.is_null() {
        return Ok(std::ptr::null_mut());
    }
    if !object::is_kind(guest, c"MTLCompileOptions") {
        return Err("expected a guest MTLCompileOptions object".to_owned());
    }
    let options = unsafe { object::body::<pipeline::CompileOptions>(guest) };
    let class = unsafe { host_get_class(c"MTLCompileOptions".as_ptr()) };
    if class.is_null() {
        return Err("host MTLCompileOptions class unavailable".to_owned());
    }
    let host = unsafe { host_send_id(class, sel(c"new")) };
    if host.is_null() {
        return Err("host MTLCompileOptions allocation failed".to_owned());
    }
    if let Some(value) = options.fast_math {
        unsafe { host_send_void_bool(host, sel(c"setFastMathEnabled:"), value) };
    }
    if let Some(value) = options.preserve_invariance {
        unsafe { host_send_void_bool(host, sel(c"setPreserveInvariance:"), value) };
    }
    if let Some(value) = options.language_version {
        unsafe { host_send_void_u64(host, sel(c"setLanguageVersion:"), value) };
    }
    if !options.macros.is_empty() {
        let dictionary_class = unsafe { host_get_class(c"NSMutableDictionary".as_ptr()) };
        let string_class = unsafe { host_get_class(c"NSString".as_ptr()) };
        if dictionary_class.is_null() || string_class.is_null() {
            unsafe { host_release(host) };
            return Err("host Foundation dictionary/string classes unavailable".to_owned());
        }
        let dict = unsafe { host_send_id(dictionary_class, sel(c"new")) };
        if dict.is_null() {
            unsafe { host_release(host) };
            return Err("host preprocessor macro dictionary allocation failed".to_owned());
        }
        for (key, value) in &options.macros {
            let (Ok(key), Ok(value)) = (
                std::ffi::CString::new(key.as_bytes()),
                std::ffi::CString::new(value.as_bytes()),
            ) else {
                unsafe {
                    host_release(dict);
                    host_release(host)
                };
                return Err("preprocessor macro contains an embedded NUL".to_owned());
            };
            let key = unsafe {
                host_send_id_ptr(
                    string_class,
                    sel(c"stringWithUTF8String:"),
                    key.as_ptr().cast(),
                )
            };
            let value = unsafe {
                host_send_id_ptr(
                    string_class,
                    sel(c"stringWithUTF8String:"),
                    value.as_ptr().cast(),
                )
            };
            if key.is_null() || value.is_null() {
                unsafe {
                    host_release(dict);
                    host_release(host)
                };
                return Err("invalid preprocessor macro key or value".to_owned());
            }
            unsafe { host_send_void_id_id(dict, sel(c"setObject:forKey:"), value, key) };
        }
        unsafe {
            host_send_void_id(host, sel(c"setPreprocessorMacros:"), dict);
            host_release(dict)
        };
    }
    Ok(host)
}

pub(super) unsafe extern "C" fn new_library_with_source(
    receiver: Id,
    _sel: Sel,
    source: Id,
    options: Id,
    error: *mut Id,
) -> Id {
    if !error.is_null() {
        unsafe { *error = std::ptr::null_mut() };
    }
    let _pool = HostPool::new();
    let host_options = match host_compile_options(options) {
        Ok(value) => value,
        Err(reason) => return fail(error, "MTLLibraryErrorDomain", &reason),
    };
    let Some(host_source) = host_string(source) else {
        if !host_options.is_null() {
            unsafe { host_release(host_options) };
        }
        return fail(
            error,
            "MTLLibraryErrorDomain",
            "expected a valid source NSString",
        );
    };
    let mut host_error = std::ptr::null_mut();
    let host = unsafe {
        host_send_id_id_id_ptr(
            object::body::<NativeDevice>(receiver).host,
            sel(c"newLibraryWithSource:options:error:"),
            host_source,
            host_options,
            &mut host_error,
        )
    };
    if !host_options.is_null() {
        unsafe { host_release(host_options) };
    }
    if host.is_null() {
        return fail(
            error,
            "MTLLibraryErrorDomain",
            &native_error(host_error, "Metal source compilation failed"),
        );
    }
    wrap_library(host, receiver)
}

unsafe extern "C" fn library_function_names(receiver: Id, _sel: Sel) -> Id {
    use corefoundation::base::CFTypeRef;
    let _pool = HostPool::new();
    let library = unsafe { object::body::<NativeLibrary>(receiver) };
    let names = unsafe { host_send_id(library.host, sel(c"functionNames")) };
    if names.is_null() {
        return std::ptr::null_mut();
    }
    let count = unsafe { host_send_u64(names, sel(c"count")) };
    let mut strings = Vec::<CFTypeRef>::new();
    for index in 0..count {
        let name = unsafe { host_send_id_u64(names, sel(c"objectAtIndex:"), index) };
        if name.is_null() {
            continue;
        }
        let bytes = unsafe { host_send_ptr(name, sel(c"UTF8String")) };
        if bytes.is_null() {
            continue;
        }
        let text = unsafe { CStr::from_ptr(bytes.cast()) }.to_string_lossy();
        let guest = foundation::string::from_str(&text);
        if !guest.is_null() {
            strings.push(guest as CFTypeRef);
        }
    }
    let result = corefoundation::collections::new_array(
        &strings,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for name in strings {
        objc::arc::release(name as Id);
    }
    objc::arc::autorelease(result as Id)
}

unsafe extern "C" fn library_device(receiver: Id, _sel: Sel) -> Id {
    unsafe { object::body::<NativeLibrary>(receiver) }.device
}

unsafe extern "C" fn library_dealloc(receiver: Id, _sel: Sel) {
    let library = unsafe { object::body::<NativeLibrary>(receiver) };
    unsafe { host_release(library.host) };
    objc::arc::release(library.device);
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn new_function(receiver: Id, _sel: Sel, name: Id) -> Id {
    let _pool = HostPool::new();
    let Some(host_name) = host_string(name) else {
        return std::ptr::null_mut();
    };
    let library = unsafe { object::body::<NativeLibrary>(receiver) };
    let host = unsafe { host_send_id_id(library.host, sel(c"newFunctionWithName:"), host_name) };
    if host.is_null() {
        return std::ptr::null_mut();
    }
    objc::arc::retain(receiver);
    let guest = object::make(
        c"RosemaryNativeMTLFunction",
        NativeFunction {
            host,
            library: receiver,
        },
    );
    if guest.is_null() {
        unsafe { host_release(host) };
        objc::arc::release(receiver);
    }
    guest
}

pub(super) unsafe extern "C" fn function_device(receiver: Id, _sel: Sel) -> Id {
    let library = unsafe { object::body::<NativeFunction>(receiver) }.library;
    unsafe { object::body::<NativeLibrary>(library) }.device
}

unsafe extern "C" fn function_dealloc(receiver: Id, _sel: Sel) {
    let function = unsafe { object::body::<NativeFunction>(receiver) };
    unsafe { host_release(function.host) };
    objc::arc::release(function.library);
    objc::arc::finalize(receiver);
}

pub(super) unsafe extern "C" fn new_compute_pipeline(
    receiver: Id,
    _sel: Sel,
    function: Id,
    error: *mut Id,
) -> Id {
    if !error.is_null() {
        unsafe { *error = std::ptr::null_mut() };
    }
    if !object::is_kind(function, c"RosemaryNativeMTLFunction")
        || unsafe { function_device(function, std::ptr::null()) } != receiver
    {
        return fail(
            error,
            "MTLComputePipelineErrorDomain",
            "expected a native function of this device",
        );
    }
    let _pool = HostPool::new();
    let native_function = unsafe { object::body::<NativeFunction>(function) }.host;
    let mut host_error = std::ptr::null_mut();
    let host = unsafe {
        host_send_id_id_ptr(
            object::body::<NativeDevice>(receiver).host,
            sel(c"newComputePipelineStateWithFunction:error:"),
            native_function,
            &mut host_error,
        )
    };
    if host.is_null() {
        return fail(
            error,
            "MTLComputePipelineErrorDomain",
            &native_error(host_error, "native compute pipeline creation failed"),
        );
    }
    objc::arc::retain(receiver);
    let guest = object::make(
        c"RosemaryNativeMTLComputePipelineState",
        NativeComputePipeline {
            host,
            device: receiver,
        },
    );
    if guest.is_null() {
        unsafe { host_release(host) };
        objc::arc::release(receiver);
    }
    guest
}

unsafe extern "C" fn pipeline_device(receiver: Id, _sel: Sel) -> Id {
    unsafe { object::body::<NativeComputePipeline>(receiver) }.device
}

unsafe extern "C" fn pipeline_max_threads(receiver: Id, _sel: Sel) -> u64 {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeComputePipeline>(receiver) }.host;
    unsafe { host_send_u64(host, sel(c"maxTotalThreadsPerThreadgroup")) }
}

unsafe extern "C" fn pipeline_execution_width(receiver: Id, _sel: Sel) -> u64 {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeComputePipeline>(receiver) }.host;
    unsafe { host_send_u64(host, sel(c"threadExecutionWidth")) }
}

unsafe extern "C" fn pipeline_dealloc(receiver: Id, _sel: Sel) {
    let pipeline = unsafe { object::body::<NativeComputePipeline>(receiver) };
    unsafe { host_release(pipeline.host) };
    objc::arc::release(pipeline.device);
    objc::arc::finalize(receiver);
}

pub(super) unsafe extern "C" fn new_compute_encoder(receiver: Id, _sel: Sel) -> Id {
    let _pool = HostPool::new();
    let command = unsafe { object::body::<NativeCommandBuffer>(receiver) };
    let host = unsafe { host_send_id(command.host, sel(c"computeCommandEncoder")) };
    if host.is_null() {
        return std::ptr::null_mut();
    }
    let host = unsafe { host_retain(host) };
    objc::arc::retain(receiver);
    let guest = object::make(
        c"RosemaryNativeMTLComputeCommandEncoder",
        NativeComputeEncoder {
            host,
            command: receiver,
        },
    );
    if guest.is_null() {
        unsafe { host_release(host) };
        objc::arc::release(receiver);
    }
    objc::arc::autorelease(guest)
}

unsafe extern "C" fn encoder_device(receiver: Id, _sel: Sel) -> Id {
    let command = unsafe { object::body::<NativeComputeEncoder>(receiver) }.command;
    unsafe { command_device(command, std::ptr::null()) }
}

unsafe extern "C" fn set_pipeline(receiver: Id, _sel: Sel, pipeline: Id) {
    if !object::is_kind(pipeline, c"RosemaryNativeMTLComputePipelineState") {
        gpu::trace("native compute encoder requires a native pipeline".to_string());
        return;
    }
    if unsafe { pipeline_device(pipeline, std::ptr::null()) }
        != unsafe { encoder_device(receiver, std::ptr::null()) }
    {
        gpu::trace("native compute pipeline belongs to another device".to_string());
        return;
    }
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeComputeEncoder>(receiver) }.host;
    let pipeline = unsafe { object::body::<NativeComputePipeline>(pipeline) }.host;
    unsafe { host_send_void_id(host, sel(c"setComputePipelineState:"), pipeline) };
}

unsafe extern "C" fn set_buffer(receiver: Id, _sel: Sel, buffer: Id, offset: u64, index: u64) {
    let native_buffer = if buffer.is_null() {
        // Metal permits nil to unbind a buffer slot; do not leave the old
        // resource bound when the guest explicitly clears it.
        std::ptr::null_mut()
    } else if object::is_kind(buffer, c"RosemaryNativeMTLBuffer") {
        let guest = unsafe { object::body::<NativeBuffer>(buffer) };
        if guest.device != unsafe { encoder_device(receiver, std::ptr::null()) } {
            gpu::trace("native compute buffer belongs to another device".to_string());
            return;
        }
        guest.host
    } else {
        gpu::trace("native compute encoder requires a native buffer".to_string());
        return;
    };
    let _pool = HostPool::new();
    if !native_buffer.is_null() && offset > unsafe { host_send_u64(native_buffer, sel(c"length")) }
    {
        gpu::trace("native compute buffer offset is out of bounds".to_string());
        return;
    }
    let encoder = unsafe { object::body::<NativeComputeEncoder>(receiver) }.host;
    unsafe {
        host_send_void_id_u64_u64(
            encoder,
            sel(c"setBuffer:offset:atIndex:"),
            native_buffer,
            offset,
            index,
        )
    };
}

unsafe extern "C" fn set_bytes(
    receiver: Id,
    _sel: Sel,
    bytes: *const c_void,
    length: u64,
    index: u64,
) {
    if (length > 0 && bytes.is_null()) || length > 4096 {
        gpu::trace("native Metal compute inline bytes invalid or over 4 KiB".to_string());
        return;
    }
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeComputeEncoder>(receiver) }.host;
    unsafe {
        host_send_void_ptr_u64_u64(host, sel(c"setBytes:length:atIndex:"), bytes, length, index)
    };
}

unsafe extern "C" fn set_texture(receiver: Id, _sel: Sel, texture: Id, index: u64) {
    let native = if texture.is_null() {
        std::ptr::null_mut()
    } else if object::is_kind(texture, c"RosemaryNativeMTLTexture") {
        let body = unsafe { object::body::<texture::NativeTexture>(texture) };
        if body.device != unsafe { encoder_device(receiver, std::ptr::null()) } {
            gpu::trace("native compute texture belongs to a different device".to_string());
            return;
        }
        body.host
    } else {
        gpu::trace("native compute encoder requires a native Metal texture".to_string());
        return;
    };
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeComputeEncoder>(receiver) }.host;
    unsafe { host_send_void_id_u64(host, sel(c"setTexture:atIndex:"), native, index) };
}

unsafe extern "C" fn set_sampler(receiver: Id, _sel: Sel, sampler: Id, index: u64) {
    let native = if sampler.is_null() {
        std::ptr::null_mut()
    } else if object::is_kind(sampler, c"RosemaryNativeMTLSamplerState") {
        let body = unsafe { object::body::<sampler::NativeSampler>(sampler) };
        if body.device != unsafe { encoder_device(receiver, std::ptr::null()) } {
            gpu::trace("native compute sampler belongs to a different device".to_string());
            return;
        }
        body.host
    } else {
        gpu::trace("native compute encoder requires a native Metal sampler".to_string());
        return;
    };
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeComputeEncoder>(receiver) }.host;
    unsafe { host_send_void_id_u64(host, sel(c"setSamplerState:atIndex:"), native, index) };
}

unsafe extern "C" fn dispatch_threadgroups(
    receiver: Id,
    _sel: Sel,
    groups: encoder::MtlSize,
    size: encoder::MtlSize,
) {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeComputeEncoder>(receiver) }.host;
    unsafe {
        host_send_void_size_size(
            host,
            sel(c"dispatchThreadgroups:threadsPerThreadgroup:"),
            groups,
            size,
        )
    };
}

unsafe extern "C" fn end_encoding(receiver: Id, _sel: Sel) {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeComputeEncoder>(receiver) }.host;
    unsafe { host_send_void(host, sel(c"endEncoding")) };
}

unsafe extern "C" fn encoder_dealloc(receiver: Id, _sel: Sel) {
    let encoder = unsafe { object::body::<NativeComputeEncoder>(receiver) };
    unsafe { host_release(encoder.host) };
    objc::arc::release(encoder.command);
    objc::arc::finalize(receiver);
}
