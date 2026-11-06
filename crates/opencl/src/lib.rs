#![allow(
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    non_snake_case
)]

//! OpenCL forwards portable C calls to the host ICD loader, when installed.
//! No synthetic platform or device is advertised without a host runtime.

use std::ffi::{c_char, c_void};
use std::sync::OnceLock;

type ClInt = i32;
type Handle = *mut c_void;
type ContextNotify = Option<unsafe extern "C" fn(*const c_char, *const c_void, usize, *mut c_void)>;
type ProgramNotify = Option<unsafe extern "C" fn(Handle, *mut c_void)>;

const CL_INVALID_VALUE: ClInt = -30;
const CL_INVALID_PLATFORM: ClInt = -32;
const CL_INVALID_DEVICE: ClInt = -33;
const CL_INVALID_CONTEXT: ClInt = -34;
const CL_INVALID_COMMAND_QUEUE: ClInt = -36;
const CL_INVALID_MEM_OBJECT: ClInt = -38;
const CL_INVALID_PROGRAM: ClInt = -44;
const CL_INVALID_KERNEL: ClInt = -48;
const CL_INVALID_EVENT: ClInt = -58;
const CL_INVALID_PROPERTY: ClInt = -64;
const CL_PLATFORM_NOT_FOUND_KHR: ClInt = -1001;

// The loader stays open: returned handles and asynchronous callbacks outlive calls.
fn loader() -> Option<usize> {
    static LOADER: OnceLock<Option<usize>> = OnceLock::new();
    *LOADER.get_or_init(|| {
        #[cfg(target_os = "macos")]
        let name = c"/System/Library/Frameworks/OpenCL.framework/OpenCL";
        #[cfg(not(target_os = "macos"))]
        let name = c"libOpenCL.so.1";
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        (!handle.is_null()).then_some(handle as usize)
    })
}

macro_rules! api {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty = $fallback:expr;)*) => {
        #[derive(Default)]
        struct Host {
            clCreateContext: Option<unsafe extern "C" fn(*const isize, u32, *const Handle, ContextNotify, *mut c_void, *mut ClInt) -> Handle>,
            $($name: Option<unsafe extern "C" fn($($ty),*) -> $ret>,)*
        }

        impl Host {
            unsafe fn load(handle: usize) -> Self {
                Self {
                    clCreateContext: {
                        let address = unsafe { libc::dlsym(handle as Handle, c"clCreateContext".as_ptr()) };
                        if address.is_null() { None } else { Some(unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*const isize, u32, *const Handle, ContextNotify, *mut c_void, *mut ClInt) -> Handle>(address) }) }
                    },
                    $($name: {
                        let address = unsafe { libc::dlsym(handle as Handle, concat!(stringify!($name), "\0").as_ptr().cast()) };
                        if address.is_null() { None } else {
                            Some(unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($ty),*) -> $ret>(address) })
                        }
                    },)*
                }
            }
        }

        $(pub unsafe extern "C" fn $name($($arg: $ty),*) -> $ret {
            if let Some(function) = host().$name {
                unsafe { function($($arg),*) }
            } else {
                $fallback
            }
        })*
    };
}

fn host() -> &'static Host {
    #[cfg(test)]
    if let Some(host) = TEST_HOST.with(|slot| slot.get()) {
        return host;
    }
    static HOST: OnceLock<Host> = OnceLock::new();
    HOST.get_or_init(|| {
        loader()
            .map(|handle| unsafe { Host::load(handle) })
            .unwrap_or_default()
    })
}

unsafe fn creation_error(error: *mut ClInt, code: ClInt) -> Handle {
    if !error.is_null() {
        unsafe { *error = code };
    }
    std::ptr::null_mut()
}

unsafe fn no_platforms(entries: u32, platforms: *mut Handle, count: *mut u32) -> ClInt {
    if (platforms.is_null() && count.is_null()) || (!platforms.is_null() && entries == 0) {
        return CL_INVALID_VALUE;
    }
    if !count.is_null() {
        unsafe { *count = 0 };
    }
    CL_PLATFORM_NOT_FOUND_KHR
}

unsafe fn no_devices(count: *mut u32) -> ClInt {
    if !count.is_null() {
        unsafe { *count = 0 };
    }
    CL_INVALID_PLATFORM
}

// CGL sharegroups and Apple's other properties have no host-compatible handles.
unsafe fn portable_properties(mut properties: *const isize) -> bool {
    if properties.is_null() || cfg!(target_os = "macos") {
        return true;
    }
    while unsafe { *properties } != 0 {
        match unsafe { *properties } {
            0x1084 | 0x1085 => {} // CL_CONTEXT_PLATFORM, CL_CONTEXT_INTEROP_USER_SYNC
            _ => return false,
        }
        properties = unsafe { properties.add(2) };
    }
    true
}

pub unsafe extern "C" fn clCreateContext(
    properties: *const isize,
    count: u32,
    devices: *const Handle,
    notify: ContextNotify,
    user: *mut c_void,
    error: *mut ClInt,
) -> Handle {
    if count == 0 || devices.is_null() || (notify.is_none() && !user.is_null()) {
        return unsafe { creation_error(error, CL_INVALID_VALUE) };
    }
    if !unsafe { portable_properties(properties) } {
        return unsafe { creation_error(error, CL_INVALID_PROPERTY) };
    }
    if let Some(function) = host().clCreateContext {
        unsafe { function(properties, count, devices, notify, user, error) }
    } else {
        unsafe { creation_error(error, CL_INVALID_DEVICE) }
    }
}

api! {
    clGetPlatformIDs(entries: u32, platforms: *mut Handle, count: *mut u32) -> ClInt = unsafe { no_platforms(entries, platforms, count) };
    clGetPlatformInfo(platform: Handle, param: u32, size: usize, value: *mut c_void, returned: *mut usize) -> ClInt = CL_INVALID_PLATFORM;
    clGetDeviceIDs(platform: Handle, kind: u64, entries: u32, devices: *mut Handle, count: *mut u32) -> ClInt = unsafe { no_devices(count) };
    clGetDeviceInfo(device: Handle, param: u32, size: usize, value: *mut c_void, returned: *mut usize) -> ClInt = CL_INVALID_DEVICE;
    clCreateCommandQueue(context: Handle, device: Handle, properties: u64, error: *mut ClInt) -> Handle = unsafe { creation_error(error, CL_INVALID_CONTEXT) };
    clCreateBuffer(context: Handle, flags: u64, size: usize, host: *mut c_void, error: *mut ClInt) -> Handle = unsafe { creation_error(error, CL_INVALID_CONTEXT) };
    clCreateProgramWithSource(context: Handle, count: u32, strings: *const *const c_char, lengths: *const usize, error: *mut ClInt) -> Handle = unsafe { creation_error(error, CL_INVALID_CONTEXT) };
    clBuildProgram(program: Handle, count: u32, devices: *const Handle, options: *const c_char, notify: ProgramNotify, user: *mut c_void) -> ClInt = CL_INVALID_PROGRAM;
    clCreateKernel(program: Handle, name: *const c_char, error: *mut ClInt) -> Handle = unsafe { creation_error(error, CL_INVALID_PROGRAM) };
    clSetKernelArg(kernel: Handle, index: u32, size: usize, value: *const c_void) -> ClInt = CL_INVALID_KERNEL;
    clEnqueueMapBuffer(queue: Handle, buffer: Handle, blocking: u32, flags: u64, offset: usize, size: usize, wait_count: u32, wait_list: *const Handle, event: *mut Handle, error: *mut ClInt) -> Handle = unsafe { creation_error(error, CL_INVALID_COMMAND_QUEUE) };
    clEnqueueReadBuffer(queue: Handle, buffer: Handle, blocking: u32, offset: usize, size: usize, data: *mut c_void, wait_count: u32, wait_list: *const Handle, event: *mut Handle) -> ClInt = CL_INVALID_COMMAND_QUEUE;
    clEnqueueWriteBuffer(queue: Handle, buffer: Handle, blocking: u32, offset: usize, size: usize, data: *const c_void, wait_count: u32, wait_list: *const Handle, event: *mut Handle) -> ClInt = CL_INVALID_COMMAND_QUEUE;
    clEnqueueNDRangeKernel(queue: Handle, kernel: Handle, dimensions: u32, offset: *const usize, global: *const usize, local: *const usize, wait_count: u32, wait_list: *const Handle, event: *mut Handle) -> ClInt = CL_INVALID_COMMAND_QUEUE;
    clEnqueueUnmapMemObject(queue: Handle, memory: Handle, mapped: *mut c_void, wait_count: u32, wait_list: *const Handle, event: *mut Handle) -> ClInt = CL_INVALID_COMMAND_QUEUE;
    clFinish(queue: Handle) -> ClInt = CL_INVALID_COMMAND_QUEUE;
    clGetCommandQueueInfo(queue: Handle, param: u32, size: usize, value: *mut c_void, returned: *mut usize) -> ClInt = CL_INVALID_COMMAND_QUEUE;
    clGetContextInfo(context: Handle, param: u32, size: usize, value: *mut c_void, returned: *mut usize) -> ClInt = CL_INVALID_CONTEXT;
    clGetKernelWorkGroupInfo(kernel: Handle, device: Handle, param: u32, size: usize, value: *mut c_void, returned: *mut usize) -> ClInt = CL_INVALID_KERNEL;
    clRetainCommandQueue(object: Handle) -> ClInt = CL_INVALID_COMMAND_QUEUE;
    clReleaseCommandQueue(object: Handle) -> ClInt = CL_INVALID_COMMAND_QUEUE;
    clRetainContext(object: Handle) -> ClInt = CL_INVALID_CONTEXT;
    clReleaseContext(object: Handle) -> ClInt = CL_INVALID_CONTEXT;
    clRetainDevice(object: Handle) -> ClInt = CL_INVALID_DEVICE;
    clReleaseDevice(object: Handle) -> ClInt = CL_INVALID_DEVICE;
    clReleaseEvent(object: Handle) -> ClInt = CL_INVALID_EVENT;
    clRetainKernel(object: Handle) -> ClInt = CL_INVALID_KERNEL;
    clReleaseKernel(object: Handle) -> ClInt = CL_INVALID_KERNEL;
    clRetainMemObject(object: Handle) -> ClInt = CL_INVALID_MEM_OBJECT;
    clReleaseMemObject(object: Handle) -> ClInt = CL_INVALID_MEM_OBJECT;
    clReleaseProgram(object: Handle) -> ClInt = CL_INVALID_PROGRAM;
}

exports::exports! {
    functions {
        "clGetPlatformIDs" => clGetPlatformIDs,
        "clGetPlatformInfo" => clGetPlatformInfo,
        "clGetDeviceIDs" => clGetDeviceIDs,
        "clGetDeviceInfo" => clGetDeviceInfo,
        "clCreateContext" => clCreateContext,
        "clCreateCommandQueue" => clCreateCommandQueue,
        "clCreateBuffer" => clCreateBuffer,
        "clCreateProgramWithSource" => clCreateProgramWithSource,
        "clBuildProgram" => clBuildProgram,
        "clCreateKernel" => clCreateKernel,
        "clSetKernelArg" => clSetKernelArg,
        "clEnqueueMapBuffer" => clEnqueueMapBuffer,
        "clEnqueueReadBuffer" => clEnqueueReadBuffer,
        "clEnqueueWriteBuffer" => clEnqueueWriteBuffer,
        "clEnqueueNDRangeKernel" => clEnqueueNDRangeKernel,
        "clEnqueueUnmapMemObject" => clEnqueueUnmapMemObject,
        "clFinish" => clFinish,
        "clGetCommandQueueInfo" => clGetCommandQueueInfo,
        "clGetContextInfo" => clGetContextInfo,
        "clGetKernelWorkGroupInfo" => clGetKernelWorkGroupInfo,
        "clRetainCommandQueue" => clRetainCommandQueue,
        "clReleaseCommandQueue" => clReleaseCommandQueue,
        "clRetainContext" => clRetainContext,
        "clReleaseContext" => clReleaseContext,
        "clRetainDevice" => clRetainDevice,
        "clReleaseDevice" => clReleaseDevice,
        "clReleaseEvent" => clReleaseEvent,
        "clRetainKernel" => clRetainKernel,
        "clReleaseKernel" => clReleaseKernel,
        "clRetainMemObject" => clRetainMemObject,
        "clReleaseMemObject" => clReleaseMemObject,
        "clReleaseProgram" => clReleaseProgram,
    }
}

#[cfg(test)]
thread_local! {
    static TEST_HOST: std::cell::Cell<Option<&'static Host>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::{null, null_mut};

    fn with_host(host: Host, run: impl FnOnce()) {
        struct Restore(Option<&'static Host>);
        impl Drop for Restore {
            fn drop(&mut self) {
                TEST_HOST.with(|slot| slot.set(self.0));
            }
        }
        let previous = TEST_HOST.with(|slot| slot.replace(Some(Box::leak(Box::new(host)))));
        let _restore = Restore(previous);
        run();
    }

    #[test]
    fn no_host_reports_no_platforms_and_rejects_invalid_enumeration() {
        with_host(Host::default(), || unsafe {
            let mut count = 123;
            assert_eq!(
                clGetPlatformIDs(0, null_mut(), &mut count),
                CL_PLATFORM_NOT_FOUND_KHR
            );
            assert_eq!(count, 0);
            assert_eq!(
                clGetPlatformIDs(0, null_mut(), null_mut()),
                CL_INVALID_VALUE
            );
            let mut platform = null_mut();
            assert_eq!(
                clGetPlatformIDs(0, &mut platform, &mut count),
                CL_INVALID_VALUE
            );
            assert_eq!(
                clGetDeviceIDs(null_mut(), !0, 0, null_mut(), &mut count),
                CL_INVALID_PLATFORM
            );
            assert_eq!(count, 0);
        });
    }

    #[test]
    fn no_host_returns_typed_handle_errors() {
        with_host(Host::default(), || unsafe {
            let mut error = 0;
            let device = std::ptr::dangling_mut::<c_void>();
            assert!(clCreateContext(null(), 1, &device, None, null_mut(), &mut error).is_null());
            assert_eq!(error, CL_INVALID_DEVICE);
            assert!(clCreateContext(null(), 0, null(), None, null_mut(), &mut error).is_null());
            assert_eq!(error, CL_INVALID_VALUE);
            assert_eq!(
                clGetPlatformInfo(null_mut(), 0, 0, null_mut(), null_mut()),
                CL_INVALID_PLATFORM
            );
            assert_eq!(
                clGetDeviceInfo(null_mut(), 0, 0, null_mut(), null_mut()),
                CL_INVALID_DEVICE
            );
            assert!(clCreateCommandQueue(null_mut(), null_mut(), 0, &mut error).is_null());
            assert_eq!(error, CL_INVALID_CONTEXT);
            assert!(clCreateBuffer(null_mut(), 0, 0, null_mut(), &mut error).is_null());
            assert_eq!(error, CL_INVALID_CONTEXT);
            assert!(clCreateProgramWithSource(null_mut(), 0, null(), null(), &mut error).is_null());
            assert_eq!(error, CL_INVALID_CONTEXT);
            assert_eq!(
                clBuildProgram(null_mut(), 0, null(), null(), None, null_mut()),
                CL_INVALID_PROGRAM
            );
            assert!(clCreateKernel(null_mut(), null(), &mut error).is_null());
            assert_eq!(error, CL_INVALID_PROGRAM);
            assert_eq!(clSetKernelArg(null_mut(), 0, 0, null()), CL_INVALID_KERNEL);
            assert!(
                clEnqueueMapBuffer(
                    null_mut(),
                    null_mut(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    null(),
                    null_mut(),
                    &mut error
                )
                .is_null()
            );
            assert_eq!(error, CL_INVALID_COMMAND_QUEUE);
            assert_eq!(
                clEnqueueReadBuffer(
                    null_mut(),
                    null_mut(),
                    0,
                    0,
                    0,
                    null_mut(),
                    0,
                    null(),
                    null_mut()
                ),
                CL_INVALID_COMMAND_QUEUE
            );
            assert_eq!(
                clEnqueueWriteBuffer(
                    null_mut(),
                    null_mut(),
                    0,
                    0,
                    0,
                    null(),
                    0,
                    null(),
                    null_mut()
                ),
                CL_INVALID_COMMAND_QUEUE
            );
            assert_eq!(
                clEnqueueNDRangeKernel(
                    null_mut(),
                    null_mut(),
                    0,
                    null(),
                    null(),
                    null(),
                    0,
                    null(),
                    null_mut()
                ),
                CL_INVALID_COMMAND_QUEUE
            );
            assert_eq!(
                clEnqueueUnmapMemObject(null_mut(), null_mut(), null_mut(), 0, null(), null_mut()),
                CL_INVALID_COMMAND_QUEUE
            );
            assert_eq!(clFinish(null_mut()), CL_INVALID_COMMAND_QUEUE);
            assert_eq!(
                clGetCommandQueueInfo(null_mut(), 0, 0, null_mut(), null_mut()),
                CL_INVALID_COMMAND_QUEUE
            );
            assert_eq!(
                clGetContextInfo(null_mut(), 0, 0, null_mut(), null_mut()),
                CL_INVALID_CONTEXT
            );
            assert_eq!(
                clGetKernelWorkGroupInfo(null_mut(), null_mut(), 0, 0, null_mut(), null_mut()),
                CL_INVALID_KERNEL
            );
            assert_eq!(clRetainCommandQueue(null_mut()), CL_INVALID_COMMAND_QUEUE);
            assert_eq!(clReleaseCommandQueue(null_mut()), CL_INVALID_COMMAND_QUEUE);
            assert_eq!(clRetainContext(null_mut()), CL_INVALID_CONTEXT);
            assert_eq!(clReleaseContext(null_mut()), CL_INVALID_CONTEXT);
            assert_eq!(clRetainDevice(null_mut()), CL_INVALID_DEVICE);
            assert_eq!(clReleaseDevice(null_mut()), CL_INVALID_DEVICE);
            assert_eq!(clReleaseEvent(null_mut()), CL_INVALID_EVENT);
            assert_eq!(clRetainKernel(null_mut()), CL_INVALID_KERNEL);
            assert_eq!(clReleaseKernel(null_mut()), CL_INVALID_KERNEL);
            assert_eq!(clRetainMemObject(null_mut()), CL_INVALID_MEM_OBJECT);
            assert_eq!(clReleaseMemObject(null_mut()), CL_INVALID_MEM_OBJECT);
            assert_eq!(clReleaseProgram(null_mut()), CL_INVALID_PROGRAM);
        });
    }

    unsafe extern "C" fn build(
        program: Handle,
        count: u32,
        devices: *const Handle,
        options: *const c_char,
        notify: ProgramNotify,
        user: *mut c_void,
    ) -> ClInt {
        assert_eq!(program, std::ptr::dangling_mut::<c_void>());
        assert_eq!(count, 0);
        assert!(devices.is_null());
        assert_eq!(
            unsafe { std::ffi::CStr::from_ptr(options) },
            c"-cl-opt-disable"
        );
        unsafe { notify.unwrap()(program, user) };
        0
    }

    unsafe extern "C" fn built(_program: Handle, user: *mut c_void) {
        unsafe { *user.cast::<bool>() = true };
    }

    unsafe extern "C" fn context(
        properties: *const isize,
        count: u32,
        devices: *const Handle,
        notify: ContextNotify,
        user: *mut c_void,
        error: *mut ClInt,
    ) -> Handle {
        assert!(properties.is_null());
        assert_eq!(count, 1);
        unsafe {
            *error = 0;
            notify.unwrap()(c"diagnostic".as_ptr(), null(), usize::MAX, user);
            *devices
        }
    }

    unsafe extern "C" fn notified(
        message: *const c_char,
        private: *const c_void,
        length: usize,
        user: *mut c_void,
    ) {
        assert_eq!(unsafe { std::ffi::CStr::from_ptr(message) }, c"diagnostic");
        assert!(private.is_null());
        unsafe { *user.cast::<usize>() = length };
    }

    #[test]
    fn forwards_program_and_context_callbacks_without_narrowing() {
        with_host(
            Host {
                clBuildProgram: Some(build),
                clCreateContext: Some(context),
                ..Host::default()
            },
            || unsafe {
                let handle = std::ptr::dangling_mut::<c_void>();
                let mut called = false;
                assert_eq!(
                    clBuildProgram(
                        handle,
                        0,
                        null(),
                        c"-cl-opt-disable".as_ptr(),
                        Some(built),
                        (&mut called as *mut bool).cast()
                    ),
                    0
                );
                assert!(called);
                let mut length = 0usize;
                let mut error = -1;
                assert_eq!(
                    clCreateContext(
                        null(),
                        1,
                        &handle,
                        Some(notified),
                        (&mut length as *mut usize).cast(),
                        &mut error
                    ),
                    handle
                );
                assert_eq!(error, 0);
                assert_eq!(length, usize::MAX);
            },
        );
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn rejects_apple_context_properties_without_forwarding() {
        with_host(
            Host {
                clCreateContext: Some(context),
                ..Host::default()
            },
            || unsafe {
                let device = std::ptr::dangling_mut::<c_void>();
                let mut error = 0;
                let cgl = [0x10000000isize, 123, 0];
                assert!(
                    clCreateContext(cgl.as_ptr(), 1, &device, None, null_mut(), &mut error)
                        .is_null()
                );
                assert_eq!(error, CL_INVALID_PROPERTY);
                let diagnostics = [0x10000006isize, 1, 0];
                assert!(!portable_properties(diagnostics.as_ptr()));
                let standard = [0x1084isize, 123, 0x1085, 1, 0];
                assert!(portable_properties(standard.as_ptr()));
            },
        );
    }

    #[test]
    fn host_loader_resolves_enumeration_when_installed() {
        let Some(handle) = loader() else {
            return;
        };
        let host = unsafe { Host::load(handle) };
        let enumerate = host
            .clGetPlatformIDs
            .expect("OpenCL ICD enumeration symbol");
        let mut count = 0;
        let status = unsafe { enumerate(0, null_mut(), &mut count) };
        assert!(status == 0 || status == CL_PLATFORM_NOT_FOUND_KHR);
        if status == CL_PLATFORM_NOT_FOUND_KHR {
            assert_eq!(count, 0);
        }
    }

    #[test]
    fn exports_every_implemented_entry_point() {
        assert!(symbol("clGetPlatformIDs").is_some(), "clGetPlatformIDs");
        assert!(symbol("clGetPlatformInfo").is_some(), "clGetPlatformInfo");
        assert!(symbol("clGetDeviceIDs").is_some(), "clGetDeviceIDs");
        assert!(symbol("clGetDeviceInfo").is_some(), "clGetDeviceInfo");
        assert!(symbol("clCreateContext").is_some(), "clCreateContext");
        assert!(
            symbol("clCreateCommandQueue").is_some(),
            "clCreateCommandQueue"
        );
        assert!(symbol("clCreateBuffer").is_some(), "clCreateBuffer");
        assert!(
            symbol("clCreateProgramWithSource").is_some(),
            "clCreateProgramWithSource"
        );
        assert!(symbol("clBuildProgram").is_some(), "clBuildProgram");
        assert!(symbol("clCreateKernel").is_some(), "clCreateKernel");
        assert!(symbol("clSetKernelArg").is_some(), "clSetKernelArg");
        assert!(symbol("clEnqueueMapBuffer").is_some(), "clEnqueueMapBuffer");
        assert!(
            symbol("clEnqueueReadBuffer").is_some(),
            "clEnqueueReadBuffer"
        );
        assert!(
            symbol("clEnqueueWriteBuffer").is_some(),
            "clEnqueueWriteBuffer"
        );
        assert!(
            symbol("clEnqueueNDRangeKernel").is_some(),
            "clEnqueueNDRangeKernel"
        );
        assert!(
            symbol("clEnqueueUnmapMemObject").is_some(),
            "clEnqueueUnmapMemObject"
        );
        assert!(symbol("clFinish").is_some(), "clFinish");
        assert!(
            symbol("clGetCommandQueueInfo").is_some(),
            "clGetCommandQueueInfo"
        );
        assert!(symbol("clGetContextInfo").is_some(), "clGetContextInfo");
        assert!(
            symbol("clGetKernelWorkGroupInfo").is_some(),
            "clGetKernelWorkGroupInfo"
        );
        assert!(
            symbol("clRetainCommandQueue").is_some(),
            "clRetainCommandQueue"
        );
        assert!(
            symbol("clReleaseCommandQueue").is_some(),
            "clReleaseCommandQueue"
        );
        assert!(symbol("clRetainContext").is_some(), "clRetainContext");
        assert!(symbol("clReleaseContext").is_some(), "clReleaseContext");
        assert!(symbol("clRetainDevice").is_some(), "clRetainDevice");
        assert!(symbol("clReleaseDevice").is_some(), "clReleaseDevice");
        assert!(symbol("clReleaseEvent").is_some(), "clReleaseEvent");
        assert!(symbol("clRetainKernel").is_some(), "clRetainKernel");
        assert!(symbol("clReleaseKernel").is_some(), "clReleaseKernel");
        assert!(symbol("clRetainMemObject").is_some(), "clRetainMemObject");
        assert!(symbol("clReleaseMemObject").is_some(), "clReleaseMemObject");
        assert!(symbol("clReleaseProgram").is_some(), "clReleaseProgram");
    }
}
