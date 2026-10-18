use std::ffi::c_void;

pub mod darwin {
    pub const PROT_NONE: i32 = 0x00;
    pub const PROT_READ: i32 = 0x01;
    pub const PROT_WRITE: i32 = 0x02;
    pub const PROT_EXEC: i32 = 0x04;

    pub const MAP_SHARED: i32 = 0x0001;
    pub const MAP_PRIVATE: i32 = 0x0002;
    pub const MAP_FIXED: i32 = 0x0010;
    pub const MAP_RENAME: i32 = 0x0020;
    pub const MAP_NORESERVE: i32 = 0x0040;
    pub const MAP_NOEXTEND: i32 = 0x0100;
    pub const MAP_HASSEMAPHORE: i32 = 0x0200;
    pub const MAP_NOCACHE: i32 = 0x0400;
    pub const MAP_JIT: i32 = 0x0800;
    pub const MAP_ANON: i32 = 0x1000;
    pub const MAP_RESILIENT_CODESIGN: i32 = 0x2000;
    pub const MAP_RESILIENT_MEDIA: i32 = 0x4000;
    pub const MAP_32BIT: i32 = 0x8000;
    pub const MAP_TRANSLATED_ALLOW_EXECUTE: i32 = 0x0002_0000;
    pub const MAP_UNIX03: i32 = 0x0004_0000;

    pub const MADV_NORMAL: i32 = 0;
    pub const MADV_RANDOM: i32 = 1;
    pub const MADV_SEQUENTIAL: i32 = 2;
    pub const MADV_WILLNEED: i32 = 3;
    pub const MADV_DONTNEED: i32 = 4;
    pub const MADV_FREE: i32 = 5;
    pub const MADV_ZERO_WIRED_PAGES: i32 = 6;
    pub const MADV_FREE_REUSABLE: i32 = 7;
    pub const MADV_FREE_REUSE: i32 = 8;
    pub const MADV_CAN_REUSE: i32 = 9;
    pub const MADV_PAGEOUT: i32 = 10;

    pub const MS_ASYNC: i32 = 0x0001;
    pub const MS_INVALIDATE: i32 = 0x0002;
    pub const MS_KILLPAGES: i32 = 0x0004;
    pub const MS_DEACTIVATE: i32 = 0x0008;
    pub const MS_SYNC: i32 = 0x0010;
}

/// Whether `flags` asks for anonymous memory, in Darwin's spelling.
///
/// Asked separately from [`map_flags`] because the descriptor has to be dealt
/// with too: on Darwin an anonymous mapping's `fd` argument is not a descriptor
/// at all but a VM tag, and Linux's `mmap` validates it.
#[inline(always)]
pub fn is_anonymous(flags: i32) -> bool {
    flags & darwin::MAP_ANON != 0
}

/// The `fd` to give the host for a guest's `(flags, fd)`.
///
/// `-1` for an anonymous mapping, whatever the guest passed. Darwin encodes a VM
/// tag there - `VM_MAKE_TAG(tag)`, a small integer shifted up by 24 - and Linux
/// wants either a real descriptor or `-1`. The tag is purely advisory
/// bookkeeping for `vmmap(1)`, so there is nothing to carry across.
pub fn map_fd(flags: i32, fd: i32) -> i32 {
    match is_anonymous(flags) {
        true => -1,
        false => fd,
    }
}

/// The host `mmap` flags for a guest's Darwin ones.
///
/// Bits Darwin has and this host does not are dropped rather than refused: every
/// one of them is a hint - `MAP_NOCACHE` asks not to enter the page onto the
/// inactive list, `MAP_JIT` asks for a W^X exception the host does not enforce,
/// `MAP_RESILIENT_*` govern behaviour when the backing file goes away - and a
/// mapping made without the hint is still a correct mapping. Losing a hint costs
/// performance; refusing the call costs the program.
pub fn map_flags(flags: i32) -> i32 {
    #[cfg(target_os = "macos")]
    {
        flags
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut out = 0;
        // The three both sides agree on, by value as well as by meaning. Copied
        // through the host's own names so that agreement is asserted rather
        // than assumed.
        for (theirs, ours) in [
            (darwin::MAP_SHARED, libc::MAP_SHARED),
            (darwin::MAP_PRIVATE, libc::MAP_PRIVATE),
            (darwin::MAP_FIXED, libc::MAP_FIXED),
            // These two do *not* agree by value, and are the reason this
            // function exists.
            (darwin::MAP_ANON, libc::MAP_ANONYMOUS),
            // FreeBSD removed `MAP_NORESERVE` in 11 - it always overcommits -
            // so the hint has nothing to carry to and is dropped.
            #[cfg(not(target_os = "freebsd"))]
            (darwin::MAP_NORESERVE, libc::MAP_NORESERVE),
            #[cfg(target_arch = "x86_64")]
            (darwin::MAP_32BIT, libc::MAP_32BIT),
        ] {
            if flags & theirs != 0 {
                out |= ours;
            }
        }
        // Dropped, each with nothing on this host to carry it to:
        // `MAP_RENAME`, `MAP_NOEXTEND`, `MAP_HASSEMAPHORE`, `MAP_NOCACHE`,
        // `MAP_JIT`, `MAP_RESILIENT_CODESIGN`, `MAP_RESILIENT_MEDIA`,
        // `MAP_TRANSLATED_ALLOW_EXECUTE`, `MAP_UNIX03`.
        let _ = (
            darwin::MAP_RENAME,
            darwin::MAP_NOEXTEND,
            darwin::MAP_HASSEMAPHORE,
            darwin::MAP_NOCACHE,
            darwin::MAP_JIT,
            darwin::MAP_RESILIENT_CODESIGN,
            darwin::MAP_RESILIENT_MEDIA,
            darwin::MAP_TRANSLATED_ALLOW_EXECUTE,
            darwin::MAP_UNIX03,
        );
        out
    }
}

/// The host `PROT_*` bits for a guest's Darwin ones.
///
/// Bits other than read/write/execute are dropped rather than refused, since
/// xnu's `mprotect` masks them off too (`VM_PROT_COPY`, `VM_PROT_TRUSTED`, ...),
/// while Linux answers `EINVAL` for any bit it does not know.
pub fn prot_flags(prot: i32) -> i32 {
    #[cfg(target_os = "macos")]
    {
        prot
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut out = libc::PROT_NONE;
        for (theirs, ours) in [
            (darwin::PROT_READ, libc::PROT_READ),
            (darwin::PROT_WRITE, libc::PROT_WRITE),
            (darwin::PROT_EXEC, libc::PROT_EXEC),
        ] {
            if prot & theirs != 0 {
                out |= ours;
            }
        }
        out
    }
}

/// What a guest's `MADV_*` becomes on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advice {
    /// Ask the host, with this advice.
    Host(i32),
    /// Say nothing and succeed. Advice is advice, and Darwin's page-reuse family
    /// (`MADV_FREE_REUSE`, `MADV_CAN_REUSE`) exists to *undo* a
    /// `MADV_FREE_REUSABLE` before the pages are touched again - which under
    /// Linux's `MADV_FREE` is not something the caller has to say.
    Nothing,
    /// An advice neither side names: `EINVAL`, which is Darwin's own answer.
    /// Passing the number through would risk landing on an unrelated Linux
    /// advice that happens to share it.
    Invalid,
}

/// The host `madvise` advice for a guest's Darwin one.
pub fn advice(advice: i32) -> Advice {
    #[cfg(target_os = "macos")]
    {
        Advice::Host(advice)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Advice::Host(match advice {
            darwin::MADV_NORMAL => libc::MADV_NORMAL,
            darwin::MADV_RANDOM => libc::MADV_RANDOM,
            darwin::MADV_SEQUENTIAL => libc::MADV_SEQUENTIAL,
            darwin::MADV_WILLNEED => libc::MADV_WILLNEED,
            darwin::MADV_DONTNEED => libc::MADV_DONTNEED,
            // Same meaning, different number: keep the mapping, let the kernel
            // reclaim the pages, and do not promise their contents afterwards.
            darwin::MADV_FREE | darwin::MADV_FREE_REUSABLE => libc::MADV_FREE,
            // FreeBSD has no `MADV_PAGEOUT`. Its `MADV_DONTNEED` is the BSD
            // one - a hint that the range is not wanted soon, which keeps the
            // contents - so it asks for the same thing Darwin's does.
            #[cfg(target_os = "freebsd")]
            darwin::MADV_PAGEOUT => libc::MADV_DONTNEED,
            #[cfg(not(target_os = "freebsd"))]
            darwin::MADV_PAGEOUT => libc::MADV_PAGEOUT,
            darwin::MADV_ZERO_WIRED_PAGES | darwin::MADV_FREE_REUSE | darwin::MADV_CAN_REUSE => {
                return Advice::Nothing;
            }
            _ => return Advice::Invalid,
        })
    }
}

/// The host `msync` flags for a guest's Darwin ones.
pub fn sync_flags(flags: i32) -> i32 {
    #[cfg(target_os = "macos")]
    {
        flags
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut out = 0;
        for (theirs, ours) in [
            (darwin::MS_ASYNC, libc::MS_ASYNC),
            (darwin::MS_INVALIDATE, libc::MS_INVALIDATE),
            (darwin::MS_SYNC, libc::MS_SYNC),
        ] {
            if flags & theirs != 0 {
                out |= ours;
            }
        }
        // `MS_KILLPAGES` and `MS_DEACTIVATE` are Darwin's own; dropped.
        let _ = (darwin::MS_KILLPAGES, darwin::MS_DEACTIVATE);
        out
    }
}

/// `void *mmap(void *addr, size_t len, int prot, int flags, int fd, off_t off)`
///
/// # Safety
///
/// The guest's own request. Every argument is passed to the host `mmap` as
/// given, apart from the three this module translates.
pub unsafe extern "C" fn mmap(
    addr: *mut c_void,
    len: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    off: i64,
) -> *mut c_void {
    // SAFETY: the caller's own arguments, with the protection and flags
    // rewritten into this host's spelling of the same request.
    unsafe {
        libc::mmap(
            addr,
            len,
            prot_flags(prot),
            map_flags(flags),
            map_fd(flags, fd),
            off as libc::off_t,
        )
    }
}

/// `int mprotect(void *addr, size_t len, int prot)`
///
/// # Safety
///
/// The guest's own request.
pub unsafe extern "C" fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32 {
    // SAFETY: the caller's own range, with the protection rewritten.
    unsafe { libc::mprotect(addr, len, prot_flags(prot)) }
}

/// `int madvise(void *addr, size_t len, int advice)`
///
/// # Safety
///
/// The guest's own request; advice never dereferences the range.
pub unsafe extern "C" fn madvise(addr: *mut c_void, len: usize, adv: i32) -> i32 {
    let adv = match advice(adv) {
        Advice::Host(adv) => adv,
        Advice::Nothing => return 0,
        Advice::Invalid => {
            crate::errno::set(libc::EINVAL);
            return -1;
        }
    };
    // SAFETY: the caller's own range, with the advice rewritten.
    unsafe { libc::madvise(addr, len, adv) }
}

/// `int posix_madvise(void *addr, size_t len, int advice)`
///
/// Darwin's `POSIX_MADV_*` are its `MADV_*` under other names, and the same is
/// true here, so this is `madvise` with the POSIX return convention: the errno
/// value itself, rather than `-1` and `errno`.
///
/// # Safety
///
/// As [`madvise`].
pub unsafe extern "C" fn posix_madvise(addr: *mut c_void, len: usize, adv: i32) -> i32 {
    let host = match adv {
        darwin::MADV_NORMAL => libc::POSIX_MADV_NORMAL,
        darwin::MADV_RANDOM => libc::POSIX_MADV_RANDOM,
        darwin::MADV_SEQUENTIAL => libc::POSIX_MADV_SEQUENTIAL,
        darwin::MADV_WILLNEED => libc::POSIX_MADV_WILLNEED,
        darwin::MADV_DONTNEED => libc::POSIX_MADV_DONTNEED,
        _ => return libc::EINVAL,
    };
    // `posix_madvise` is not `madvise` with a different return convention:
    // Linux intentionally treats POSIX_MADV_DONTNEED as non-destructive,
    // whereas MADV_DONTNEED may discard private anonymous contents.
    // SAFETY: the caller's range and a translated POSIX advice.
    unsafe { libc::posix_madvise(addr, len, host) }
}

/// `int msync(void *addr, size_t len, int flags)`
///
/// # Safety
///
/// The guest's own request.
pub unsafe extern "C" fn msync(addr: *mut c_void, len: usize, flags: i32) -> i32 {
    // SAFETY: the caller's own range, with the flags rewritten.
    unsafe { libc::msync(addr, len, sync_flags(flags)) }
}

// `mlock`, `munlock` and `mincore` are deliberately absent: none of them takes
// a flag word that diverges, so each is a genuine passthrough and is claimed as
// one in `crate::passthrough`.
exports::exports! {
    functions {
        "mmap" => mmap,
        "munmap" => libc::munmap,
        "mprotect" => mprotect,
        "madvise" => madvise,
        "posix_madvise" => posix_madvise,
        "msync" => msync,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this module was written for: an anonymous private mapping, which
    /// is what every allocator asks for and what Darwin spells with a bit Linux
    /// reads as something else entirely.
    #[test]
    fn an_anonymous_mapping_is_anonymous_on_this_host() {
        let flags = darwin::MAP_PRIVATE | darwin::MAP_ANON;
        assert!(is_anonymous(flags));
        // mimalloc passes `VM_MAKE_TAG(100)` where Linux wants a descriptor.
        assert_eq!(map_fd(flags, 100 << 24), -1);

        #[cfg(not(target_os = "macos"))]
        {
            let host = map_flags(flags);
            assert_eq!(host & libc::MAP_ANONYMOUS, libc::MAP_ANONYMOUS);
            assert_eq!(host & libc::MAP_PRIVATE, libc::MAP_PRIVATE);
            #[cfg(target_os = "linux")]
            assert_eq!(host & libc::MAP_EXECUTABLE, 0, "0x1000 is not a hint here");
        }
    }

    /// End to end, because the translation only matters if the mapping happens.
    #[test]
    fn a_translated_request_returns_usable_memory() {
        let len = 64 * 1024;
        // SAFETY: an anonymous read-write mapping of our own, unmapped below.
        let p = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                darwin::MAP_PRIVATE | darwin::MAP_ANON,
                100 << 24,
                0,
            )
        };
        assert_ne!(p, libc::MAP_FAILED, "errno {}", crate::errno::get());
        // SAFETY: `len` bytes of readable, writable memory just obtained.
        unsafe {
            std::ptr::write_bytes(p.cast::<u8>(), 0xA5, len);
            assert_eq!(*p.cast::<u8>().add(len - 1), 0xA5);
            // And the advice family over the same range, which must not fail.
            assert_eq!(madvise(p, len, darwin::MADV_FREE), 0);
            assert_eq!(madvise(p, len, darwin::MADV_FREE_REUSE), 0);
            libc::munmap(p, len);
        }
    }

    /// Darwin's `MAP_NORESERVE` is Linux's `MAP_32BIT`, and a 32-bit mapping is
    /// not what a guest hinting at overcommit asked for.
    #[test]
    #[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
    fn noreserve_does_not_become_a_32_bit_mapping() {
        let host = map_flags(darwin::MAP_PRIVATE | darwin::MAP_ANON | darwin::MAP_NORESERVE);
        assert_eq!(host & libc::MAP_NORESERVE, libc::MAP_NORESERVE);
        #[cfg(target_arch = "x86_64")]
        assert_eq!(host & libc::MAP_32BIT, 0);
    }

    /// `MS_SYNC` is `0x10` on Darwin and `4` here; unrewritten it is no bit at
    /// all, and `msync` with no bits set is `EINVAL`.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn the_msync_mode_survives() {
        assert_eq!(sync_flags(darwin::MS_SYNC), libc::MS_SYNC);
        assert_eq!(sync_flags(darwin::MS_ASYNC), libc::MS_ASYNC);
        assert_eq!(
            sync_flags(darwin::MS_INVALIDATE | darwin::MS_SYNC),
            libc::MS_INVALIDATE | libc::MS_SYNC
        );
    }

    /// The two halves of Darwin's page-reuse protocol: the one that means
    /// something here, and the one that does not and must still succeed.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn the_reuse_family_maps_onto_madv_free_or_onto_nothing() {
        assert_eq!(advice(darwin::MADV_FREE), Advice::Host(libc::MADV_FREE));
        assert_eq!(
            advice(darwin::MADV_FREE_REUSABLE),
            Advice::Host(libc::MADV_FREE)
        );
        assert_eq!(advice(darwin::MADV_FREE_REUSE), Advice::Nothing);
        assert_eq!(advice(darwin::MADV_CAN_REUSE), Advice::Nothing);
        assert_eq!(
            advice(darwin::MADV_DONTNEED),
            Advice::Host(libc::MADV_DONTNEED)
        );
        assert_eq!(
            advice(9999),
            Advice::Invalid,
            "an advice neither side names"
        );
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn mprotect_drops_protection_bits_the_host_would_refuse() {
        // `VM_PROT_COPY`, which xnu masks off and Linux rejects.
        let prot = darwin::PROT_READ | darwin::PROT_WRITE | 0x10;
        assert_eq!(prot_flags(prot), libc::PROT_READ | libc::PROT_WRITE);
        assert_eq!(prot_flags(darwin::PROT_NONE), libc::PROT_NONE);
    }

    #[test]
    fn posix_advice_rejects_the_non_posix_madvise_tail() {
        // SAFETY: invalid advice is rejected before the range is inspected.
        assert_eq!(
            unsafe { posix_madvise(core::ptr::null_mut(), 0, darwin::MADV_FREE) },
            libc::EINVAL
        );
    }

    #[test]
    fn posix_dontneed_does_not_discard_private_anonymous_contents() {
        let len = 4096;
        // SAFETY: an anonymous mapping owned and released by this test.
        unsafe {
            let p = mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                darwin::MAP_PRIVATE | darwin::MAP_ANON,
                -1,
                0,
            );
            assert_ne!(p, libc::MAP_FAILED);
            p.cast::<u8>().write(0xa5);
            assert_eq!(posix_madvise(p, len, darwin::MADV_DONTNEED), 0);
            assert_eq!(p.cast::<u8>().read(), 0xa5);
            libc::munmap(p, len);
        }
    }
}
