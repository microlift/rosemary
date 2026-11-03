//! The GL library SDL makes its contexts against.

use std::ffi::{CString, c_void};
use std::marker::PhantomData;
use std::sync::OnceLock;

/// Load SDL's GL library, once. Every entry point a guest binds has to come out
/// of the library its contexts were made with.
pub fn open() -> bool {
    static LOADED: OnceLock<bool> = OnceLock::new();
    *LOADED.get_or_init(|| {
        let loaded = sdl3::video::gl_load_library();
        if !loaded {
            // Said here because the symptom is otherwise every `gl` import
            // refused by name, which reads as a missing export rather than as a
            // driver that could not be opened.
            eprintln!(
                "rosemary: SDL could not load a GL library ({}); the guest's OpenGL imports \
                 will be refused",
                sdl3::error()
            );
        }
        loaded
    })
}

/// The driver's own function `name`.
pub fn address(name: &str) -> Option<u64> {
    let name = CString::new(name).ok()?;
    open()
        .then(|| sdl3::video::gl_proc_address(&name))
        .flatten()
        .map(|f| f as usize as u64)
}

/// A driver function this crate calls itself, looked up on first use.
pub struct Entry<F> {
    name: &'static std::ffi::CStr,
    address: OnceLock<Option<usize>>,
    prototype: PhantomData<F>,
}

impl<F: Copy> Entry<F> {
    /// # Safety
    ///
    /// `F` is the `extern "C"` prototype of the GL function `name`.
    pub const unsafe fn new(name: &'static std::ffi::CStr) -> Self {
        Entry {
            name,
            address: OnceLock::new(),
            prototype: PhantomData,
        }
    }

    pub fn get(&self) -> Option<F> {
        const { assert!(size_of::<F>() == size_of::<*mut c_void>()) };
        let address = self.address.get_or_init(|| {
            open()
                .then(|| sdl3::video::gl_proc_address(self.name))
                .flatten()
                .map(|f| f as usize)
        });
        // SAFETY: a function pointer of the prototype `new` was promised.
        address.map(|a| unsafe { std::mem::transmute_copy::<usize, F>(&a) })
    }
}
