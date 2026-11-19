#![cfg(not(target_os = "macos"))]

use std::ffi::c_int;

/// Darwin's `termios`, whose speeds follow the control-character array.
#[repr(C)]
struct Termios {
    pub c_iflag: u64,
    pub c_oflag: u64,
    pub c_cflag: u64,
    pub c_lflag: u64,
    pub c_cc: [u8; 20],
    pub c_ispeed: u64,
    pub c_ospeed: u64,
}

#[cfg(target_os = "linux")]
fn map_to_darwin(host: libc::tcflag_t, pairs: &[(u64, libc::tcflag_t)]) -> u64 {
    pairs.iter().fold(0, |out, (darwin, linux)| {
        out | if host & linux != 0 { *darwin } else { 0 }
    })
}

#[cfg(target_os = "linux")]
fn map_to_host(darwin: u64, pairs: &[(u64, libc::tcflag_t)]) -> libc::tcflag_t {
    pairs.iter().fold(0, |out, (darwin_bit, linux)| {
        out | if darwin & darwin_bit != 0 { *linux } else { 0 }
    })
}

#[cfg(target_os = "linux")]
const IFLAGS: &[(u64, libc::tcflag_t)] = &[
    (0x1, libc::IGNBRK),
    (0x2, libc::BRKINT),
    (0x4, libc::IGNPAR),
    (0x8, libc::PARMRK),
    (0x10, libc::INPCK),
    (0x20, libc::ISTRIP),
    (0x40, libc::INLCR),
    (0x80, libc::IGNCR),
    (0x100, libc::ICRNL),
    (0x200, libc::IXON),
    (0x400, libc::IXOFF),
    (0x800, libc::IXANY),
    (0x2000, libc::IMAXBEL),
];
#[cfg(target_os = "linux")]
const OFLAGS: &[(u64, libc::tcflag_t)] = &[(0x1, libc::OPOST), (0x2, libc::ONLCR)];
#[cfg(target_os = "linux")]
const CFLAGS: &[(u64, libc::tcflag_t)] = &[
    (0x400, libc::CSTOPB),
    (0x800, libc::CREAD),
    (0x1000, libc::PARENB),
    (0x2000, libc::PARODD),
    (0x4000, libc::HUPCL),
    (0x8000, libc::CLOCAL),
];
#[cfg(target_os = "linux")]
const LFLAGS: &[(u64, libc::tcflag_t)] = &[
    (0x1, libc::ECHOKE),
    (0x2, libc::ECHOE),
    (0x4, libc::ECHOK),
    (0x8, libc::ECHO),
    (0x10, libc::ECHONL),
    (0x20, libc::ECHOPRT),
    (0x40, libc::ECHOCTL),
    (0x80, libc::ISIG),
    (0x100, libc::ICANON),
    (0x400, libc::IEXTEN),
    (0x00400000, libc::TOSTOP),
    (0x00800000, libc::FLUSHO),
    (0x20000000, libc::PENDIN),
    (0x80000000, libc::NOFLSH),
];

#[cfg(target_os = "linux")]
fn speed_to_darwin(speed: libc::speed_t) -> u64 {
    for (darwin, linux) in SPEEDS {
        if speed == *linux {
            return *darwin;
        }
    }
    speed as u64
}

#[cfg(target_os = "linux")]
fn speed_to_host(speed: u64) -> libc::speed_t {
    for (darwin, linux) in SPEEDS {
        if speed == *darwin {
            return *linux;
        }
    }
    speed as libc::speed_t
}

#[cfg(target_os = "linux")]
const SPEEDS: &[(u64, libc::speed_t)] = &[
    (0, libc::B0),
    (50, libc::B50),
    (75, libc::B75),
    (110, libc::B110),
    (134, libc::B134),
    (150, libc::B150),
    (200, libc::B200),
    (300, libc::B300),
    (600, libc::B600),
    (1200, libc::B1200),
    (1800, libc::B1800),
    (2400, libc::B2400),
    (4800, libc::B4800),
    (9600, libc::B9600),
    (19200, libc::B19200),
    (38400, libc::B38400),
    (57600, libc::B57600),
    (115200, libc::B115200),
    (230400, libc::B230400),
];

#[cfg(target_os = "linux")]
const CC: &[(usize, usize)] = &[
    (0, libc::VEOF),
    (1, libc::VEOL),
    (2, libc::VEOL2),
    (3, libc::VERASE),
    (4, libc::VWERASE),
    (5, libc::VKILL),
    (6, libc::VREPRINT),
    (8, libc::VINTR),
    (9, libc::VQUIT),
    (10, libc::VSUSP),
    (12, libc::VSTART),
    (13, libc::VSTOP),
    (14, libc::VLNEXT),
    (15, libc::VDISCARD),
    (16, libc::VMIN),
    (17, libc::VTIME),
];

#[cfg(target_os = "linux")]
unsafe extern "C" fn tcgetattr(fd: c_int, out: *mut Termios) -> c_int {
    let mut host = std::mem::MaybeUninit::<libc::termios>::uninit();
    let status = unsafe { libc::tcgetattr(fd, host.as_mut_ptr()) };
    if status != 0 {
        return status;
    }
    let host = unsafe { host.assume_init() };
    let mut cc = [0; 20];
    for (darwin, linux) in CC {
        cc[*darwin] = host.c_cc[*linux];
    }
    let char_size = host.c_cflag & libc::CSIZE;
    let darwin_char_size = match char_size {
        libc::CS6 => 0x100,
        libc::CS7 => 0x200,
        libc::CS8 => 0x300,
        _ => 0,
    };
    unsafe {
        out.write(Termios {
            c_iflag: map_to_darwin(host.c_iflag, IFLAGS),
            c_oflag: map_to_darwin(host.c_oflag, OFLAGS),
            c_cflag: map_to_darwin(host.c_cflag, CFLAGS) | darwin_char_size,
            c_lflag: map_to_darwin(host.c_lflag, LFLAGS),
            c_cc: cc,
            c_ispeed: speed_to_darwin(libc::cfgetispeed(&host)),
            c_ospeed: speed_to_darwin(libc::cfgetospeed(&host)),
        })
    };
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn tcsetattr(fd: c_int, action: c_int, input: *const Termios) -> c_int {
    let input = unsafe { &*input };
    let mut host = unsafe { std::mem::MaybeUninit::<libc::termios>::zeroed().assume_init() };
    host.c_iflag = map_to_host(input.c_iflag, IFLAGS);
    host.c_oflag = map_to_host(input.c_oflag, OFLAGS);
    host.c_cflag = map_to_host(input.c_cflag, CFLAGS)
        | match input.c_cflag & 0x300 {
            0x100 => libc::CS6,
            0x200 => libc::CS7,
            0x300 => libc::CS8,
            _ => libc::CS5,
        };
    host.c_lflag = map_to_host(input.c_lflag, LFLAGS);
    for (darwin, linux) in CC {
        host.c_cc[*linux] = input.c_cc[*darwin];
    }
    unsafe {
        libc::cfsetispeed(&mut host, speed_to_host(input.c_ispeed));
        libc::cfsetospeed(&mut host, speed_to_host(input.c_ospeed));
    }
    // BSD's `TCSASOFT`: leave the hardware alone - the control flags and the
    // speeds stay what the terminal has. `sudo` and `stty` ask for it, and
    // this host has no such bit, so it is done here and not passed on.
    if action & TCSASOFT != 0 {
        let mut current = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: the caller's descriptor, and a structure to fill.
        if unsafe { libc::tcgetattr(fd, current.as_mut_ptr()) } != 0 {
            return -1;
        }
        // SAFETY: filled by the host just now.
        let current = unsafe { current.assume_init() };
        host.c_cflag = current.c_cflag;
        // SAFETY: two structures this frame owns.
        unsafe {
            libc::cfsetispeed(&mut host, libc::cfgetispeed(&current));
            libc::cfsetospeed(&mut host, libc::cfgetospeed(&current));
        }
    }
    let host_action = match action & !TCSASOFT {
        0 => libc::TCSANOW,
        1 => libc::TCSADRAIN,
        2 => libc::TCSAFLUSH,
        _ => {
            crate::errno::set(libc::EINVAL);
            return -1;
        }
    };
    unsafe { libc::tcsetattr(fd, host_action, &host) }
}

/// Darwin's `TCSASOFT`, a flag on `tcsetattr`'s action.
#[cfg(target_os = "linux")]
const TCSASOFT: c_int = 0x10;

// The remaining non-Darwin hosts are the BSDs, where none of the tables above
// is needed: every input, output, control and local flag, every `c_cc` index
// and every speed is 4.4BSD's, and so is Darwin's. `NCCS` is twenty on both.
//
// What does differ is width - `tcflag_t` and `speed_t` are 32 bits here and 64
// on Darwin - so the structure is not the same size and cannot be cast. Each
// word is widened, and narrowed again on the way back.
const _: () = assert!(size_of::<Termios>() != size_of::<libc::termios>());

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn tcgetattr(fd: c_int, out: *mut Termios) -> c_int {
    let mut host = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: a live local, which the call fills in when it answers zero.
    let status = unsafe { libc::tcgetattr(fd, host.as_mut_ptr()) };
    if status != 0 {
        return status;
    }
    // SAFETY: filled in by the call above.
    let host = unsafe { host.assume_init() };
    // SAFETY: the caller's Darwin `struct termios`, and `cfget*speed` read a
    // structure this frame owns.
    unsafe {
        out.write(Termios {
            c_iflag: host.c_iflag.into(),
            c_oflag: host.c_oflag.into(),
            c_cflag: host.c_cflag.into(),
            c_lflag: host.c_lflag.into(),
            c_cc: host.c_cc,
            c_ispeed: libc::cfgetispeed(&host).into(),
            c_ospeed: libc::cfgetospeed(&host).into(),
        })
    };
    0
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn tcsetattr(fd: c_int, action: c_int, input: *const Termios) -> c_int {
    // SAFETY: the caller's Darwin `struct termios`.
    let input = unsafe { &*input };
    // SAFETY: an all-zero `struct termios` is a valid one to fill in.
    let mut host = unsafe { std::mem::MaybeUninit::<libc::termios>::zeroed().assume_init() };
    // A guest's flag word cannot have bits above 32 set: there is no Darwin
    // name for one, and the host would refuse the whole call over it.
    host.c_iflag = input.c_iflag as libc::tcflag_t;
    host.c_oflag = input.c_oflag as libc::tcflag_t;
    host.c_cflag = input.c_cflag as libc::tcflag_t;
    host.c_lflag = input.c_lflag as libc::tcflag_t;
    host.c_cc = input.c_cc;
    // SAFETY: a structure this frame owns.
    unsafe {
        libc::cfsetispeed(&mut host, input.c_ispeed as libc::speed_t);
        libc::cfsetospeed(&mut host, input.c_ospeed as libc::speed_t);
    }
    // `TCSANOW`, `TCSADRAIN` and `TCSAFLUSH` are 0, 1 and 2 here as well.
    // SAFETY: a structure this frame owns.
    unsafe { libc::tcsetattr(fd, action, &host) }
}

// The speed accessors, which on Darwin are plain field accesses on its own
// structure. The host's would read and write its own layout's offsets inside
// the guest's - glibc's `cfsetispeed` rewrites `c_cflag`, which is the word
// where Darwin keeps `c_oflag` - so they are answered here from the fields
// `tcgetattr` filled in and `tcsetattr` translates.

/// `speed_t cfgetispeed(const struct termios *)`
unsafe extern "C" fn cfgetispeed(t: *const Termios) -> u64 {
    // SAFETY: the caller's Darwin `struct termios`.
    unsafe { (*t).c_ispeed }
}

/// `speed_t cfgetospeed(const struct termios *)`
unsafe extern "C" fn cfgetospeed(t: *const Termios) -> u64 {
    unsafe { (*t).c_ospeed }
}

/// `int cfsetispeed(struct termios *, speed_t)`: Darwin accepts any value.
unsafe extern "C" fn cfsetispeed(t: *mut Termios, speed: u64) -> c_int {
    unsafe { (*t).c_ispeed = speed };
    0
}

/// `int cfsetospeed(struct termios *, speed_t)`
unsafe extern "C" fn cfsetospeed(t: *mut Termios, speed: u64) -> c_int {
    unsafe { (*t).c_ospeed = speed };
    0
}

/// `int cfsetspeed(struct termios *, speed_t)`
unsafe extern "C" fn cfsetspeed(t: *mut Termios, speed: u64) -> c_int {
    unsafe {
        (*t).c_ispeed = speed;
        (*t).c_ospeed = speed;
    }
    0
}

/// `void cfmakeraw(struct termios *)`, as Darwin's Libc has it.
///
/// Arithmetic on the guest's own structure, in Darwin's bit positions: the
/// host's would clear its own bits in a structure laid out differently -
/// Linux's `ICRNL` happens to be Darwin's too, and nothing else is - and a
/// terminal "made raw" that way still echoes and still takes Control-D as
/// end of file.
///
/// # Safety
///
/// `term` is a writable Darwin `struct termios`.
unsafe extern "C" fn cfmakeraw(term: *mut Termios) {
    const IGNBRK: u64 = 0x1;
    const BRKINT: u64 = 0x2;
    const IGNPAR: u64 = 0x4;
    const PARMRK: u64 = 0x8;
    const INPCK: u64 = 0x10;
    const ISTRIP: u64 = 0x20;
    const INLCR: u64 = 0x40;
    const IGNCR: u64 = 0x80;
    const ICRNL: u64 = 0x100;
    const IXON: u64 = 0x200;
    const IXOFF: u64 = 0x400;
    const IMAXBEL: u64 = 0x2000;
    const OPOST: u64 = 0x1;
    const ECHOE: u64 = 0x2;
    const ECHOK: u64 = 0x4;
    const ECHO: u64 = 0x8;
    const ECHONL: u64 = 0x10;
    const ISIG: u64 = 0x80;
    const ICANON: u64 = 0x100;
    const IEXTEN: u64 = 0x400;
    const TOSTOP: u64 = 0x0040_0000;
    const PENDIN: u64 = 0x2000_0000;
    const NOFLSH: u64 = 0x8000_0000;
    const CSIZE: u64 = 0x300;
    const CS8: u64 = 0x300;
    const CREAD: u64 = 0x800;
    const PARENB: u64 = 0x1000;
    const VMIN: usize = 16;
    const VTIME: usize = 17;
    // SAFETY: the caller's structure.
    let term = unsafe { &mut *term };
    term.c_iflag &= !(IMAXBEL
        | IXOFF
        | INPCK
        | BRKINT
        | PARMRK
        | ISTRIP
        | INLCR
        | IGNCR
        | ICRNL
        | IXON
        | IGNPAR);
    term.c_iflag |= IGNBRK;
    term.c_oflag &= !OPOST;
    term.c_lflag &=
        !(ECHO | ECHOE | ECHOK | ECHONL | ICANON | ISIG | IEXTEN | NOFLSH | TOSTOP | PENDIN);
    term.c_cflag &= !(CSIZE | PARENB);
    term.c_cflag |= CS8 | CREAD;
    term.c_cc[VMIN] = 1;
    term.c_cc[VTIME] = 0;
}

/// Opens a pty pair on the host, copying the follower's path into `name`
/// when it is not null, as `openpty` does.
///
/// # Safety
///
/// `name` is null or has room for the path.
#[cfg(not(target_os = "freebsd"))]
unsafe fn host_openpty(name: *mut libc::c_char) -> Option<(c_int, c_int)> {
    let (mut leader, mut follower) = (0, 0);
    // SAFETY: two slots this frame owns; the caller's name buffer.
    let made = unsafe {
        libc::openpty(
            &mut leader,
            &mut follower,
            name,
            // glibc's binding takes `*const` for both; others take `*mut`.
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (made == 0).then_some((leader, follower))
}

/// FreeBSD keeps `openpty` in libutil, whose soname changed from 14 to 15, so
/// a build calling it runs only on the release it was built on. The pair is
/// opened with libc alone instead, the way libutil's `openpty` does it.
///
/// # Safety
///
/// `name` is null or has room for the path.
#[cfg(target_os = "freebsd")]
unsafe fn host_openpty(name: *mut libc::c_char) -> Option<(c_int, c_int)> {
    let flags = libc::O_RDWR | libc::O_NOCTTY;
    // SAFETY: plain calls on a descriptor this function opened.
    let leader = unsafe { libc::posix_openpt(flags) };
    if leader < 0 {
        return None;
    }
    let mut path = [0 as libc::c_char; 64];
    // SAFETY: the leader just opened, and a buffer of `path.len()` bytes.
    let follower = unsafe {
        if libc::grantpt(leader) == 0
            && libc::unlockpt(leader) == 0
            && libc::ptsname_r(leader, path.as_mut_ptr(), path.len()) == 0
        {
            libc::open(path.as_ptr(), flags)
        } else {
            -1
        }
    };
    if follower < 0 {
        let error = crate::errno::get();
        // SAFETY: a descriptor this function opened and has not handed out.
        unsafe { libc::close(leader) };
        crate::errno::set(error);
        return None;
    }
    if !name.is_null() {
        // SAFETY: `path` is terminated and the caller has room for it.
        unsafe { libc::strcpy(name, path.as_ptr()) };
    }
    Some((leader, follower))
}

/// `int openpty(int *, int *, char *, const struct termios *, const struct winsize *)`
///
/// The host opens the pair; the terminal settings are Darwin's layout and go
/// through `tcsetattr` above, so they arrive in the host's bits. `winsize` is
/// four `unsigned short`s on both and passes straight through.
///
/// # Safety
///
/// `leader` and `follower` are writable; `name`, `term` and `size` are null or
/// a buffer for the path, a Darwin `struct termios` and a `struct winsize`.
unsafe extern "C" fn openpty(
    leader: *mut c_int,
    follower: *mut c_int,
    name: *mut libc::c_char,
    term: *const Termios,
    size: *const libc::winsize,
) -> c_int {
    // The host would read `term` in its own layout, so none is passed on.
    // SAFETY: the caller's name buffer.
    let Some((master, slave)) = (unsafe { host_openpty(name) }) else {
        return -1;
    };
    // SAFETY: the caller's structures, and descriptors just opened.
    let configured = unsafe {
        (term.is_null() || tcsetattr(slave, 0, term) == 0)
            && (size.is_null() || libc::ioctl(slave, libc::TIOCSWINSZ, size) == 0)
    };
    if !configured {
        let error = crate::errno::get();
        // SAFETY: descriptors this call opened and has not handed out.
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
        crate::errno::set(error);
        return -1;
    }
    // SAFETY: the caller's slots.
    unsafe {
        leader.write(master);
        follower.write(slave);
    }
    0
}

exports::exports! {
    functions {
        "cfmakeraw" => cfmakeraw,
        "openpty" => openpty,
        "tcgetattr" => tcgetattr,
        "tcsetattr" => tcsetattr,
        "cfgetispeed" => cfgetispeed,
        "cfgetospeed" => cfgetospeed,
        "cfsetispeed" => cfsetispeed,
        "cfsetospeed" => cfsetospeed,
        "cfsetspeed" => cfsetspeed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_terminal_entry_is_published() {
        for name in [
            "cfmakeraw",
            "openpty",
            "tcgetattr",
            "tcsetattr",
            "cfgetispeed",
            "cfgetospeed",
            "cfsetispeed",
            "cfsetospeed",
            "cfsetspeed",
        ] {
            assert!(symbol(name).is_some(), "{name}");
        }
    }

    /// What `sudo` does to the user's terminal: `cfmakeraw` on what
    /// `tcgetattr` said, set back with `TCSADRAIN | TCSASOFT`. The terminal it
    /// reads back must be raw by Darwin's bits.
    #[test]
    fn a_terminal_made_raw_reads_back_raw() {
        const ECHO: u64 = 0x8;
        const ICANON: u64 = 0x100;
        const OPOST: u64 = 0x1;
        const TCSADRAIN_SOFT: c_int = 0x11;
        // SAFETY: no name asked for.
        let (leader, follower) =
            unsafe { host_openpty(std::ptr::null_mut()) }.expect("no pty to test with");
        // SAFETY: an all-zero Darwin `struct termios`, then the follower's.
        let mut term: Termios = unsafe { std::mem::zeroed() };
        unsafe {
            assert_eq!(tcgetattr(follower, &mut term), 0);
            assert_ne!(term.c_lflag & ICANON, 0, "a fresh pty is canonical");
            cfmakeraw(&mut term);
            assert_eq!(tcsetattr(follower, TCSADRAIN_SOFT, &term), 0);
            let mut back: Termios = std::mem::zeroed();
            assert_eq!(tcgetattr(follower, &mut back), 0);
            assert_eq!(back.c_lflag & (ICANON | ECHO), 0);
            assert_eq!(back.c_oflag & OPOST, 0);
            assert_eq!((back.c_cc[16], back.c_cc[17]), (1, 0), "VMIN and VTIME");
            libc::close(leader);
            libc::close(follower);
        }
    }

    /// `openpty` takes Darwin's `termios`: a raw one asked for at open time
    /// must read back raw, and the window size must land.
    #[test]
    fn openpty_applies_darwin_settings() {
        const ECHO: u64 = 0x8;
        const ICANON: u64 = 0x100;
        let (mut leader, mut follower) = (0, 0);
        let mut raw: Termios = unsafe { std::mem::zeroed() };
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let mut name = [0 as libc::c_char; 128];
        // SAFETY: slots, a name buffer and structures this test owns.
        unsafe {
            assert_eq!(
                openpty(
                    &mut leader,
                    &mut follower,
                    name.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null()
                ),
                0
            );
            let path = std::ffi::CStr::from_ptr(name.as_ptr());
            assert!(path.to_bytes().starts_with(b"/dev/"), "{path:?}");
            assert_eq!(tcgetattr(follower, &mut raw), 0);
            libc::close(leader);
            libc::close(follower);
            cfmakeraw(&mut raw);
            assert_eq!(
                openpty(
                    &mut leader,
                    &mut follower,
                    std::ptr::null_mut(),
                    &raw,
                    &size
                ),
                0
            );
            let mut back: Termios = std::mem::zeroed();
            assert_eq!(tcgetattr(follower, &mut back), 0);
            assert_eq!(back.c_lflag & (ICANON | ECHO), 0);
            let mut got: libc::winsize = std::mem::zeroed();
            assert_eq!(libc::ioctl(follower, libc::TIOCGWINSZ, &mut got), 0);
            assert_eq!((got.ws_row, got.ws_col), (24, 80));
            libc::close(leader);
            libc::close(follower);
        }
    }

    /// The speeds are the Darwin structure's own fields, and setting one
    /// leaves every other word of it alone.
    #[test]
    fn speeds_live_in_darwins_fields() {
        let mut t = Termios {
            c_iflag: 1,
            c_oflag: 2,
            c_cflag: 3,
            c_lflag: 4,
            c_cc: [0; 20],
            c_ispeed: 0,
            c_ospeed: 0,
        };
        // SAFETY: a structure this test owns.
        unsafe {
            assert_eq!(cfsetispeed(&mut t, 38400), 0);
            assert_eq!(cfsetospeed(&mut t, 9600), 0);
            assert_eq!(cfgetispeed(&t), 38400);
            assert_eq!(cfgetospeed(&t), 9600);
        }
        assert_eq!((t.c_iflag, t.c_oflag, t.c_cflag, t.c_lflag), (1, 2, 3, 4));
    }
}
