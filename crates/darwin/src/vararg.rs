//! Darwin's arm64 variadic calling convention, at the edge of the exports a
//! native guest calls.
//!
//! Apple's arm64 ABI passes every variadic argument on the stack, one 8-byte
//! slot each, and its `va_list` is a `char *` to the next slot. AAPCS64 hosts
//! pass them in registers and their `va_list` is a five-field structure. The
//! exports below are implemented in this host's ABI, so where the two differ
//! each is published behind an entry that rebuilds the host's form: a
//! structure whose register areas are exhausted, so every `va_arg` reads the
//! next slot of the guest's stack. Where the two ABIs agree - x86-64, and an
//! Apple host - the entries are the implementations themselves.
//!
//! The entries only hold for exports whose named arguments all arrive in
//! integer registers, which is every one this crate publishes.

/// Whether a guest's variadic call reaches this host's functions as they expect.
pub const HOST_MATCHES_DARWIN: bool =
    !cfg!(all(target_arch = "aarch64", not(target_vendor = "apple")));

/// A C-variadic export defined by its `va_list` form.
///
/// ```ignore
/// darwin::variadic! {
///     /// `int snprintf(char *, size_t, const char *, ...)`
///     pub fn snprintf(buffer: *mut c_char, size: usize, format: *const c_char) -> c_int
///         => vsnprintf;
/// }
/// ```
///
/// declares `snprintf` taking those arguments and an ellipsis, answered by
/// `vsnprintf(buffer, size, format, args)`. Where the guest's ABI is not this
/// host's, `snprintf` is instead an entry taking Darwin's stack tail.
#[cfg(not(all(target_arch = "aarch64", not(target_vendor = "apple"))))]
#[macro_export]
macro_rules! variadic {
    ($(
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)? => $target:path;
    )*) => {$(
        $(#[$meta])*
        $vis unsafe extern "C" fn $name($($arg: $ty,)* args: ...) $(-> $ret)? {
            // SAFETY: the caller's obligations, which are the `va_list` form's.
            unsafe { $target($($arg,)* args) }
        }
    )*};
}

/// See the other configuration's documentation.
#[cfg(all(target_arch = "aarch64", not(target_vendor = "apple")))]
#[macro_export]
macro_rules! variadic {
    ($(
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)? => $target:path;
    )*) => {$(
        $(#[$meta])*
        #[unsafe(naked)]
        $vis unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                $crate::__host_va_list!(
                    "add x16, sp, #48",
                    $crate::__count!($($arg)*)
                ),
                target = sym $target,
            )
        }
    )*};
}

/// An export taking a `va_list` as argument `index`, implemented by `target`,
/// which takes this host's.
///
/// ```ignore
/// darwin::va_list! {
///     darwin_vsnprintf = vsnprintf, 3;
/// }
/// ```
#[cfg(not(all(target_arch = "aarch64", not(target_vendor = "apple"))))]
#[macro_export]
macro_rules! va_list {
    ($($vis:vis $name:ident = $target:ident, $index:tt;)*) => {$(
        #[allow(unused_imports)]
        $vis use $target as $name;
    )*};
}

/// See the other configuration's documentation.
#[cfg(all(target_arch = "aarch64", not(target_vendor = "apple")))]
#[macro_export]
macro_rules! va_list {
    ($($vis:vis $name:ident = $target:ident, $index:tt;)*) => {$(
        #[unsafe(naked)]
        $vis unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                $crate::__host_va_list!(concat!("mov x16, x", $index), $index),
                target = sym $target,
            )
        }
    )*};
}

/// An export whose optional arguments `target` takes as named ones, starting
/// at argument `index`: `open`'s mode, `fcntl`'s and `ioctl`'s argument.
///
/// The slots are read whether or not the caller passed them, as reading a
/// register the caller did not set is under an ABI that passes them there;
/// `target` decides from the named arguments whether a value means anything.
///
/// ```ignore
/// darwin::stack_args! {
///     darwin_open = open_with_mode, 2, 1;
/// }
/// ```
#[cfg(not(all(target_arch = "aarch64", not(target_vendor = "apple"))))]
#[macro_export]
macro_rules! stack_args {
    ($($vis:vis $name:ident = $target:ident, $index:tt, $count:tt;)*) => {$(
        #[allow(unused_imports)]
        $vis use $target as $name;
    )*};
}

/// See the other configuration's documentation.
#[cfg(all(target_arch = "aarch64", not(target_vendor = "apple")))]
#[macro_export]
macro_rules! stack_args {
    ($($vis:vis $name:ident = $target:ident, $index:tt, $count:tt;)*) => {$(
        #[unsafe(naked)]
        $vis unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                $crate::__load_slots!($index, $count),
                "b {target}",
                target = sym $target,
            )
        }
    )*};
}

/// The body of an entry that calls `{target}` with an AAPCS64 `va_list` in
/// register `index`, whose stack area `load` puts in x16.
///
/// The structure is `{ __stack, __gr_top, __vr_top, __gr_offs, __vr_offs }`;
/// zero offsets say no register arguments remain, so `va_arg` reads
/// `__stack` only, one 8-byte slot at a time, which is Darwin's layout.
#[doc(hidden)]
#[macro_export]
macro_rules! __host_va_list {
    ($load:expr, $index:expr) => {
        concat!(
            "stp x29, x30, [sp, #-48]!\n",
            "mov x29, sp\n",
            $load,
            "\n",
            "stp x16, xzr, [sp, #16]\n",
            "stp xzr, xzr, [sp, #32]\n",
            "add x",
            $index,
            ", sp, #16\n",
            "bl {target}\n",
            "ldp x29, x30, [sp], #48\n",
            "ret",
        )
    };
}

/// Load `count` stack slots into the registers from `index` on.
#[doc(hidden)]
#[macro_export]
macro_rules! __load_slots {
    ($index:tt, 1) => {
        concat!("ldr x", $index, ", [sp]")
    };
    (2, 2) => {
        "ldp x2, x3, [sp]"
    };
    (3, 2) => {
        "ldp x3, x4, [sp]"
    };
}

/// The number of named arguments, which is the register the tail's `va_list`
/// goes in.
#[doc(hidden)]
#[macro_export]
macro_rules! __count {
    () => {
        0
    };
    ($a:ident) => {
        1
    };
    ($a:ident $b:ident) => {
        2
    };
    ($a:ident $b:ident $c:ident) => {
        3
    };
    ($a:ident $b:ident $c:ident $d:ident) => {
        4
    };
    ($a:ident $b:ident $c:ident $d:ident $e:ident) => {
        5
    };
    ($a:ident $b:ident $c:ident $d:ident $e:ident $f:ident) => {
        6
    };
    ($a:ident $b:ident $c:ident $d:ident $e:ident $f:ident $g:ident) => {
        7
    };
}

/// Call `f` as a Darwin arm64 caller would: `named` in x0 onwards and `tail`
/// on the stack, one slot each. Returns x0.
///
/// # Safety
///
/// `f` takes `named` as its named integer arguments and `tail` as what its
/// variadic arguments or optional ones should read.
#[cfg(all(test, target_arch = "aarch64"))]
pub unsafe fn call_darwin(f: u64, named: &[u64], tail: &[u64]) -> u64 {
    let mut regs = [0u64; 8];
    regs[..named.len()].copy_from_slice(named);
    // A 16-byte aligned block of slots, as the caller's outgoing area is.
    let mut slots = vec![0u64; tail.len().next_multiple_of(2).max(2)];
    slots[..tail.len()].copy_from_slice(tail);
    let bytes = slots.len() * 8;
    let out: u64;
    // SAFETY: the caller's obligations; the copy keeps sp 16-byte aligned and
    // the stack is restored before the block ends.
    unsafe {
        core::arch::asm!(
            "mov x20, sp",
            "sub sp, sp, x10",
            "mov x11, #0",
            "2:",
            "cmp x11, x10",
            "b.eq 3f",
            "ldr x12, [x9, x11]",
            "str x12, [sp, x11]",
            "add x11, x11, #8",
            "b 2b",
            "3:",
            "ldp x0, x1, [x8]",
            "ldp x2, x3, [x8, #16]",
            "ldp x4, x5, [x8, #32]",
            "ldp x6, x7, [x8, #48]",
            "blr x13",
            "mov sp, x20",
            in("x8") regs.as_ptr(),
            in("x9") slots.as_ptr(),
            in("x10") bytes,
            in("x13") f,
            out("x20") _,
            lateout("x0") out,
            clobber_abi("C"),
        );
    }
    out
}

/// Whether Darwin's `long double` is a different type from this host's, which
/// a host formatter would read or store at the wrong width.
pub const LONG_DOUBLE_DIFFERS: bool =
    cfg!(all(target_arch = "aarch64", not(target_vendor = "apple")));

/// `format`, through its NUL, with each `L` length on a floating-point
/// conversion made `l`, or `None` when it has none.
///
/// Darwin's arm64 `long double` is `double`, so `%Lf` there is `%lf` - `double`
/// for printf, `double *` for scanf - to a host whose `long double` is wider.
/// The scan skips scanf's `[...]` sets, whose characters are not lengths.
///
/// # Safety
///
/// `format` is NUL-terminated.
pub unsafe fn narrow_long_double<T: Copy + Into<u32> + From<u8>>(
    format: *const T,
) -> Option<Vec<T>> {
    let mut out = Vec::new();
    let mut changed = false;
    let mut i = 0;
    // SAFETY: the caller's NUL-terminated string, read up to its NUL.
    let at = |i: usize| -> u32 { unsafe { (*format.add(i)).into() } };
    loop {
        let c = at(i);
        // SAFETY: as above; `i` has not passed the NUL.
        out.push(unsafe { *format.add(i) });
        i += 1;
        if c == 0 {
            break;
        }
        if c != u32::from(b'%') {
            continue;
        }
        while let Some(&b) = b"0123456789$*.-+ #'"
            .iter()
            .find(|&&b| u32::from(b) == at(i))
        {
            out.push(T::from(b));
            i += 1;
        }
        while let Some(&b) = b"hlLqjzt".iter().find(|&&b| u32::from(b) == at(i)) {
            let float = b"aAeEfFgG".iter().any(|&f| u32::from(f) == at(i + 1));
            match b == b'L' && float {
                true => {
                    out.push(T::from(b'l'));
                    changed = true;
                }
                false => out.push(T::from(b)),
            }
            i += 1;
        }
        if at(i) == u32::from(b'[') {
            // A `]` straight after `[` or `[^` is a member, not the end.
            let mut j = i + 1;
            if at(j) == u32::from(b'^') {
                j += 1;
            }
            if at(j) == u32::from(b']') {
                j += 1;
            }
            while at(j) != 0 && at(j) != u32::from(b']') {
                j += 1;
            }
            let end = if at(j) == 0 { j } else { j + 1 };
            while i < end {
                // SAFETY: before the NUL.
                out.push(unsafe { *format.add(i) });
                i += 1;
            }
        } else if at(i) != 0 {
            // The conversion character, so a `%%` does not start another.
            // SAFETY: before the NUL.
            out.push(unsafe { *format.add(i) });
            i += 1;
        }
    }
    changed.then_some(out)
}

#[cfg(all(test, target_arch = "aarch64", not(target_vendor = "apple")))]
mod tests {
    use super::call_darwin;

    /// `fcntl(fd, F_SETFD, FD_CLOEXEC)` from a Darwin caller, which leaves x2
    /// whatever it was and puts the flag on the stack.
    #[test]
    fn an_optional_argument_is_read_from_the_stack() {
        let mut fds = [0; 2];
        // SAFETY: a writable pair.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let fcntl = crate::fcntl::entry::symbol("fcntl").expect("published");
        let garbage = 0x80_0000;
        let named = [fds[0] as u64, crate::fcntl::cmd::F_SETFD as u64, garbage];
        // SAFETY: a descriptor this test owns and an `int` in the first slot.
        let rc = unsafe { call_darwin(fcntl, &named, &[libc::FD_CLOEXEC as u64]) };
        assert_eq!(rc as i32, 0);
        // SAFETY: as above.
        let flags = unsafe { libc::fcntl(fds[0], libc::F_GETFD) };
        assert_eq!(flags, libc::FD_CLOEXEC);
        // SAFETY: both ends are this test's.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
}
