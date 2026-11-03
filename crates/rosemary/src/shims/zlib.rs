use core::ffi::{c_char, c_int, c_void};

use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{Callbacks, GuestMemory, Result};

use super::{bad_pointer, set_int_return};
use crate::shims::bind;

/// Byte offset of `zalloc` in a `z_stream`.
///
/// `next_in`, `avail_in` (+4 padding), `total_in`, `next_out`, `avail_out`
/// (+4), `total_out`, `msg`, `state` - eight fields, 64 bytes.
const ZALLOC_OFFSET: u64 = 0x40;
/// Byte offset of `zfree`, immediately after `zalloc`.
const ZFREE_OFFSET: u64 = 0x48;
/// `sizeof(z_stream)` on LP64, which every initialiser is handed as its last
/// argument and which this shim refuses to proceed past a disagreement with.
const Z_STREAM_SIZE: c_int = 112;

/// `Z_VERSION_ERROR`, which is what zlib itself answers a caller whose
/// `stream_size` does not match - so a guest built against a different
/// `z_stream` gets the error its own library would have given it rather than a
/// silent write past the end of its structure.
const Z_VERSION_ERROR: i32 = -6;
/// `Z_STREAM_ERROR`, for a stream pointer that cannot be read or written.
const Z_STREAM_ERROR: i32 = -2;

// The host's own zlib - the same copy the native provider would have resolved
// the guest's import to, reached here by name instead of through a slot.
#[link(name = "z")]
unsafe extern "C" {
    fn inflateInit_(strm: *mut c_void, version: *const c_char, stream_size: c_int) -> c_int;
    fn inflateInit2_(
        strm: *mut c_void,
        window_bits: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;
    fn deflateInit_(
        strm: *mut c_void,
        level: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;
    #[allow(clippy::too_many_arguments)]
    fn deflateInit2_(
        strm: *mut c_void,
        level: c_int,
        method: c_int,
        window_bits: c_int,
        mem_level: c_int,
        strategy: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;
    /// zlib's own version string, used in place of the guest's.
    fn zlibVersion() -> *const c_char;
    #[cfg(test)]
    fn inflateEnd(strm: *mut c_void) -> c_int;
}

/// Replace the guest allocator pointers in the `z_stream` at `strm` with thunks, in
/// place.
fn substitute_allocators(
    symbol: &str,
    strm: u64,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    for offset in [ZALLOC_OFFSET, ZFREE_OFFSET] {
        let at = strm.wrapping_add(offset);
        let mut buf = [0u8; 8];
        mem.read(at, &mut buf)
            .map_err(|_| bad_pointer(symbol, at))?;
        let guest = u64::from_le_bytes(buf);
        let host = callbacks.substitute(symbol, guest)?;
        if host != guest {
            mem.write(at, &host.to_le_bytes())
                .map_err(|_| bad_pointer(symbol, at))?;
        }
    }
    Ok(())
}

/// Everything the four entry points share:
///
/// The version string is deliberately **not** the guest's.
fn init_with(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
    stream_size: c_int,
    call: impl FnOnce(*mut c_void, *const c_char) -> c_int,
) -> Result<()> {
    let strm = ctx.rdi;
    if strm == 0 {
        set_int_return(ctx, Z_STREAM_ERROR);
        return Ok(());
    }
    if stream_size != Z_STREAM_SIZE {
        set_int_return(ctx, Z_VERSION_ERROR);
        return Ok(());
    }
    substitute_allocators(symbol, strm, mem, callbacks)?;

    // SAFETY: `strm` is a guest address, and the guest is identity-mapped, so
    // it is a valid host pointer to at least `Z_STREAM_SIZE` bytes - which the
    // read and write above have just demonstrated by touching the middle of it.
    // The version string is the host library's own.
    let rc = unsafe { call(strm as *mut c_void, zlibVersion()) };
    set_int_return(ctx, rc);
    Ok(())
}

/// `int inflateInit_(z_streamp strm, const char *version, int stream_size)`
pub fn inflate_init(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let size = ctx.rdx as c_int;
    init_with(symbol, ctx, mem, callbacks, size, |strm, version| unsafe {
        inflateInit_(strm, version, Z_STREAM_SIZE)
    })
}

/// `int inflateInit2_(z_streamp strm, int windowBits, const char *version, int stream_size)`
pub fn inflate_init2(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let bits = ctx.rsi as c_int;
    let size = ctx.rcx as c_int;
    init_with(symbol, ctx, mem, callbacks, size, |strm, version| unsafe {
        inflateInit2_(strm, bits, version, Z_STREAM_SIZE)
    })
}

/// `int deflateInit_(z_streamp strm, int level, const char *version, int stream_size)`
pub fn deflate_init(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let level = ctx.rsi as c_int;
    let size = ctx.rcx as c_int;
    init_with(symbol, ctx, mem, callbacks, size, |strm, version| unsafe {
        deflateInit_(strm, level, version, Z_STREAM_SIZE)
    })
}

/// `int deflateInit2_(z_streamp strm, int level, int method, int windowBits, int
/// memLevel, int strategy, const char *version, int stream_size)`
///
/// Eight arguments: six in the guest's registers, and the last two on its stack.
pub fn deflate_init2(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let (level, method) = (ctx.rsi as c_int, ctx.rdx as c_int);
    let (bits, mem_level, strategy) = (ctx.rcx as c_int, ctx.r8 as c_int, ctx.r9 as c_int);
    let size = stack_argument(symbol, ctx, mem, 1)? as c_int;
    init_with(symbol, ctx, mem, callbacks, size, |strm, version| unsafe {
        deflateInit2_(
            strm,
            level,
            method,
            bits,
            mem_level,
            strategy,
            version,
            Z_STREAM_SIZE,
        )
    })
}

/// The `n`th argument the guest spilled to its stack, zero-based.
fn stack_argument(
    symbol: &str,
    ctx: &RegisterContext,
    mem: &mut dyn GuestMemory,
    n: u64,
) -> Result<u64> {
    let at = ctx.rsp.wrapping_add(8 * n);
    let mut buf = [0u8; 8];
    mem.read(at, &mut buf)
        .map_err(|_| bad_pointer(symbol, at))?;
    Ok(u64::from_le_bytes(buf))
}

/// One of this module's implementations, in the shape [`crate::shims`] wants -
/// the wider one, carrying [`Callbacks`], because a guest function pointer
/// crosses the seam here and the marshaller is not what places it.
pub type Shim = crate::signal::Shim;

/// Whether `name` (underscore already stripped) is one of this module's.
///
/// `inflate`, `inflateEnd`, `deflate` and the rest are deliberately **not** here.
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "inflateInit_" => Some(bind!(callback "inflateInit_" => inflate_init)),
        "inflateInit2_" => Some(bind!(callback "inflateInit2_" => inflate_init2)),
        "deflateInit_" => Some(bind!(callback "deflateInit_" => deflate_init)),
        "deflateInit2_" => Some(bind!(callback "deflateInit2_" => deflate_init2)),
        _ => None,
    }
}

/// A `z_stream` the tests can fill in, laid out by hand so a disagreement with
/// the constants above is what fails rather than something downstream.
#[cfg(test)]
#[repr(C)]
#[derive(Default)]
struct ZStream {
    next_in: u64,
    avail_in: u32,
    _pad0: u32,
    total_in: u64,
    next_out: u64,
    avail_out: u32,
    _pad1: u32,
    total_out: u64,
    msg: u64,
    state: u64,
    zalloc: u64,
    zfree: u64,
    opaque: u64,
    data_type: c_int,
    _pad2: u32,
    adler: u64,
    reserved: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offsets this module writes at must be the ones the host's zlib reads at.
    #[test]
    fn the_stream_offsets_are_the_hosts() {
        assert_eq!(core::mem::size_of::<ZStream>(), Z_STREAM_SIZE as usize);
        assert_eq!(core::mem::offset_of!(ZStream, zalloc) as u64, ZALLOC_OFFSET);
        assert_eq!(core::mem::offset_of!(ZStream, zfree) as u64, ZFREE_OFFSET);
    }

    /// A guest allocator that is really callable from the host, standing in for what a
    /// thunk would be. Returning real memory matters.
    extern "C" fn host_alloc(_opaque: *mut c_void, items: u32, size: u32) -> *mut c_void {
        // SAFETY: a plain allocation of a non-zero size, freed by `host_free`.
        unsafe { libc::calloc(items as usize, size as usize) }
    }

    extern "C" fn host_free(_opaque: *mut c_void, address: *mut c_void) {
        // SAFETY: `address` came from `host_alloc`'s `calloc`.
        unsafe { libc::free(address) }
    }

    /// Substitutes any address with the corresponding real host function, and
    /// records what it was asked about - the two claims this shim makes.
    struct Substituting(std::cell::RefCell<Vec<u64>>);

    impl Callbacks for Substituting {
        fn substitute(&self, _symbol: &str, va: u64) -> Result<u64> {
            self.0.borrow_mut().push(va);
            Ok(match va {
                0 => 0,
                0xdead_a110 => host_alloc as *const () as u64,
                0xdead_f4ee => host_free as *const () as u64,
                other => other,
            })
        }
    }

    /// Guest memory that is just this process's memory, which is what identity
    /// mapping makes true for a real guest too.
    struct Identity;

    impl GuestMemory for Identity {
        fn read(&self, va: u64, out: &mut [u8]) -> core::result::Result<(), machish::Error> {
            if va == 0 {
                return Err(machish::Error::OutOfBounds {
                    addr: va,
                    len: out.len(),
                });
            }
            // SAFETY: the tests only ever pass the address of a live `ZStream`.
            unsafe { core::ptr::copy_nonoverlapping(va as *const u8, out.as_mut_ptr(), out.len()) };
            Ok(())
        }

        fn write(&mut self, va: u64, data: &[u8]) -> core::result::Result<(), machish::Error> {
            if va == 0 {
                return Err(machish::Error::OutOfBounds {
                    addr: va,
                    len: data.len(),
                });
            }
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), va as *mut u8, data.len()) };
            Ok(())
        }
    }

    fn ctx_for(strm: &mut ZStream) -> RegisterContext {
        let mut ctx = RegisterContext::new();
        ctx.rdi = strm as *mut ZStream as u64;
        ctx.rdx = Z_STREAM_SIZE as u64;
        ctx
    }

    #[test]
    fn the_streams_allocators_are_substituted_in_place_and_zlib_uses_them() {
        let mut strm = ZStream {
            zalloc: 0xdead_a110,
            zfree: 0xdead_f4ee,
            ..Default::default()
        };
        let callbacks = Substituting(std::cell::RefCell::new(Vec::new()));
        let mut ctx = ctx_for(&mut strm);

        inflate_init("_inflateInit_", &mut ctx, &mut Identity, &callbacks).expect("init");

        assert_eq!(ctx.rax as i32, 0, "inflateInit_ should have succeeded");
        assert_eq!(
            *callbacks.0.borrow(),
            vec![0xdead_a110, 0xdead_f4ee],
            "both allocator fields must cross the seam"
        );
        assert_eq!(strm.zalloc, host_alloc as *const () as u64);
        assert_eq!(strm.zfree, host_free as *const () as u64);
        // Allocated through the substituted allocator, so it is non-null and
        // freeing it goes back through `zfree`.
        assert_ne!(strm.state, 0, "zlib allocated no state");

        // SAFETY: a stream `inflateInit_` initialised, ended once.
        let rc = unsafe { inflateEnd(&raw mut strm as *mut c_void) };
        assert_eq!(rc, 0);
    }

    /// A guest that supplies no allocator must reach zlib's own - and zlib **writes its
    /// defaults back into the stream**, which is the fact that makes re-initialising
    /// the same stream safe.
    #[test]
    fn a_default_allocator_reaches_zlibs_own_and_is_not_wrapped() {
        let mut strm = ZStream::default();
        let callbacks = Substituting(std::cell::RefCell::new(Vec::new()));
        let mut ctx = ctx_for(&mut strm);

        inflate_init("_inflateInit_", &mut ctx, &mut Identity, &callbacks).expect("init");

        assert_eq!(ctx.rax as i32, 0);
        assert_eq!(
            *callbacks.0.borrow(),
            vec![0, 0],
            "a null field must cross as null, not be invented"
        );
        let (installed_alloc, installed_free) = (strm.zalloc, strm.zfree);
        assert_ne!(installed_alloc, 0, "zlib installed no default allocator");

        // Round two, on the stream zlib has now filled in. Nothing may change.
        // SAFETY: a stream `inflateInit_` initialised, ended once.
        unsafe { inflateEnd(&raw mut strm as *mut c_void) };
        let mut ctx = ctx_for(&mut strm);
        inflate_init("_inflateInit_", &mut ctx, &mut Identity, &callbacks).expect("re-init");
        assert_eq!(strm.zalloc, installed_alloc);
        assert_eq!(strm.zfree, installed_free);
        unsafe { inflateEnd(&raw mut strm as *mut c_void) };
    }

    #[test]
    fn a_stream_the_guest_sizes_differently_is_refused_by_zlibs_own_rule() {
        let mut strm = ZStream::default();
        let callbacks = Substituting(std::cell::RefCell::new(Vec::new()));
        let mut ctx = ctx_for(&mut strm);
        ctx.rdx = 96; // a `z_stream` from a different zlib

        inflate_init("_inflateInit_", &mut ctx, &mut Identity, &callbacks).expect("init");

        assert_eq!(ctx.rax as i32, Z_VERSION_ERROR);
        assert!(
            callbacks.0.borrow().is_empty(),
            "nothing should have been substituted into a structure we will not touch"
        );
    }

    #[test]
    fn a_null_stream_is_an_error_rather_than_a_dereference() {
        let callbacks = Substituting(std::cell::RefCell::new(Vec::new()));
        let mut ctx = RegisterContext::new();
        ctx.rdx = Z_STREAM_SIZE as u64;

        inflate_init("_inflateInit_", &mut ctx, &mut Identity, &callbacks).expect("init");
        assert_eq!(ctx.rax as i32, Z_STREAM_ERROR);
    }
}
