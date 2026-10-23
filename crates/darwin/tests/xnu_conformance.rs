//! xnu conformance: the cases of Apple's xnu test suite
//! (apple-oss-distributions/xnu at xnu-12377.1.9, `tests/`) that exercise
//! `setattrlist`, `fsetattrlist`, `setattrlistat` and `fsctl`, ported one for
//! one and run against what a guest binds.
//!
//! What upstream does with root or a private entitlement is run here as the
//! unentitled program rosemary runs is: `disk_mount_conditioner.m` built with
//! `TEST_UNENTITLED`, plus the entitled-build cases that need neither, and the
//! rest of the entitled build skipped.
//!
//! ```text
//! cargo test -p darwin --test xnu_conformance -- --nocapture
//! ```
#![cfg(not(target_os = "macos"))]

mod conformance;

use std::ffi::{CString, c_char, c_int, c_uint, c_ulong, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use conformance::darwin_errno::{ELOOP, ENOTCAPABLE};
use conformance::{Case, Kind, Stop, T, errno, imports};

const UPSTREAM: &str = "xnu-12377.1.9 tests/";

/// `struct attrlist`
#[repr(C)]
#[derive(Default)]
struct AttrList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
const FSOPT_RESOLVE_BENEATH: c_uint = 0x1000;

type SetAttrListFn =
    unsafe extern "C" fn(*const c_char, *mut AttrList, *mut c_void, usize, c_uint) -> c_int;
type FSetAttrListFn =
    unsafe extern "C" fn(c_int, *mut AttrList, *mut c_void, usize, c_uint) -> c_int;
type SetAttrListAtFn =
    unsafe extern "C" fn(c_int, *const c_char, *mut AttrList, *mut c_void, usize, u32) -> c_int;
type FsctlFn = unsafe extern "C" fn(*const c_char, c_ulong, *mut c_void, c_uint) -> c_int;

fn cstr(path: &Path) -> CString {
    CString::new(path.as_os_str().as_bytes()).unwrap()
}

/// A directory of the test's own under the host's temporary directory, as
/// darwintest's `dt_tmpdir` gives one, and removed after.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let template = std::env::temp_dir().join(format!("{name}-XXXXXX"));
        let mut bytes = template.as_os_str().as_bytes().to_vec();
        bytes.push(0);
        // SAFETY: a NUL-terminated template `mkdtemp` rewrites in place.
        let made = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) };
        assert!(!made.is_null(), "mkdtemp");
        bytes.pop();
        TempDir(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `T_EXPECT_POSIX_SUCCESS`
fn expect_success(t: &mut T, rc: c_int, what: &str) {
    let err = errno();
    t.check(rc != -1, || format!("{what}: -1, errno {err}"));
}

/// `T_EXPECT_POSIX_FAILURE`
fn expect_failure(t: &mut T, rc: c_int, want: c_int, what: &str) {
    let err = errno();
    t.check(rc == -1 && err == want, || match rc {
        -1 => format!("{what}: errno {err}, wanted {want}"),
        rc => format!("{what}: {rc}, wanted -1 with errno {want}"),
    });
}

/// `tests/file_leases.c`: the `(f)setattrlist` steps of the helper the
/// directory-lease tests run.
fn file_leases_setattrlist(t: &mut T) -> Result<(), Stop> {
    imports! {
        setattrlist: SetAttrListFn = "_setattrlist";
        fsetattrlist: FSetAttrListFn = "_fsetattrlist";
    }
    let dir = TempDir::new("file_leases");
    let testfile = dir.0.join("test_file");
    std::fs::write(&testfile, b"").map_err(|e| Stop::Fail(format!("create test file: {e}")))?;
    let path = cstr(&testfile);
    // SAFETY: a NUL-terminated path.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR) };
    t.require(fd >= 0, || "open test file".into())?;

    let mut flags: u32 = 0;
    let mut attrlist = AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        commonattr: ATTR_CMN_FLAGS,
        ..Default::default()
    };
    // SAFETY: a path, an attribute list and its four-byte buffer.
    let err = unsafe {
        (setattrlist.get()?)(
            path.as_ptr(),
            &mut attrlist,
            (&raw mut flags).cast(),
            size_of::<u32>(),
            0,
        )
    };
    let e = errno();
    t.require(err != -1, || format!("setattrlist: errno {e}"))?;
    // SAFETY: an open descriptor, and as above.
    let err = unsafe {
        (fsetattrlist.get()?)(
            fd,
            &mut attrlist,
            (&raw mut flags).cast(),
            size_of::<u32>(),
            0,
        )
    };
    let e = errno();
    t.require(err != -1, || format!("fsetattrlist: errno {e}"))?;
    // SAFETY: the descriptor opened above.
    unsafe { libc::close(fd) };
    Ok(())
}

/// `tests/vfs/resolve_beneath.c`'s `setup`: the tree every case resolves in.
struct Beneath {
    _root: TempDir,
    test_fd: c_int,
    fd: c_int,
    cwd: PathBuf,
}

impl Beneath {
    fn setup(name: &str) -> Result<Beneath, Stop> {
        let root = TempDir::new(name);
        let base = root.0.clone();
        let fail = |what: &str| {
            Stop::Fail(format!(
                "setup: {what}: {}",
                std::io::Error::last_os_error()
            ))
        };
        std::fs::create_dir(base.join("test_dir")).map_err(|_| fail("test_dir"))?;
        std::fs::create_dir(base.join("test_dir/nested")).map_err(|_| fail("nested"))?;
        for file in [
            "outside_file.txt",
            "test_dir/inside_file.txt",
            "test_dir/nested/nested_file.txt",
        ] {
            std::fs::write(base.join(file), b"").map_err(|_| fail(file))?;
        }
        let links: [(&Path, &str); 5] = [
            (Path::new("../outside_file.txt"), "test_dir/symlink"),
            (
                Path::new("nested/nested_file.txt"),
                "test_dir/symlink_to_nested",
            ),
            (Path::new(".."), "test_dir/parent_symlink"),
            (Path::new("circular_symlink"), "test_dir/circular_symlink"),
            (&base, "test_dir/symlink_absolute"),
        ];
        for (from, at) in links {
            std::os::unix::fs::symlink(from, base.join(at)).map_err(|_| fail(at))?;
        }
        let test_dir = cstr(&base.join("test_dir"));
        // SAFETY: a NUL-terminated path.
        let test_fd = unsafe { libc::open(test_dir.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
        if test_fd < 0 {
            return Err(fail("open test_dir"));
        }
        let cwd = std::env::current_dir().map_err(|_| fail("getcwd"))?;
        // `fchdir(test_fd)`
        // SAFETY: the directory opened above.
        if unsafe { libc::fchdir(test_fd) } != 0 {
            return Err(fail("fchdir"));
        }
        // SAFETY: a NUL-terminated relative path.
        let fd = unsafe { libc::open(c"inside_file.txt".as_ptr(), libc::O_RDWR) };
        if fd < 0 {
            return Err(fail("open inside_file.txt"));
        }
        Ok(Beneath {
            _root: root,
            test_fd,
            fd,
            cwd,
        })
    }
}

impl Drop for Beneath {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.cwd);
        // SAFETY: the descriptors `setup` opened.
        unsafe {
            libc::close(self.fd);
            libc::close(self.test_fd);
        }
    }
}

/// `tests/vfs/resolve_beneath.c`: `resolve_beneath_setattrlist`.
fn resolve_beneath_setattrlist(t: &mut T) -> Result<(), Stop> {
    imports! {
        setattrlist: SetAttrListFn = "_setattrlist";
        fsetattrlist: FSetAttrListFn = "_fsetattrlist";
        setattrlistat: SetAttrListAtFn = "_setattrlistat";
    }
    let tree = Beneath::setup("resolve_beneath_setattrlist")?;
    let mut flags: c_int = 0;
    let mut attrlist = AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        commonattr: ATTR_CMN_FLAGS,
        ..Default::default()
    };
    let size = size_of::<c_int>();
    let buf = (&raw mut flags).cast::<c_void>();

    // `setattrlist`
    let mut set =
        |t: &mut T, path: &std::ffi::CStr, want: Option<c_int>, what: &str| -> Result<(), Stop> {
            // SAFETY: a path, an attribute list and its buffer.
            let rc = unsafe {
                (setattrlist.get()?)(
                    path.as_ptr(),
                    &mut attrlist,
                    buf,
                    size,
                    FSOPT_RESOLVE_BENEATH,
                )
            };
            match want {
                None => expect_success(t, rc, what),
                Some(errno) => expect_failure(t, rc, errno, what),
            }
            Ok(())
        };
    set(
        t,
        c"inside_file.txt",
        None,
        "Test Case 1: File within the directory",
    )?;
    set(
        t,
        c"symlink",
        Some(ENOTCAPABLE),
        "Test Case 2: File using a symlink pointing outside",
    )?;
    set(
        t,
        c"../outside_file.txt",
        Some(ENOTCAPABLE),
        "Test Case 3: File using \"..\" to navigate outside",
    )?;
    set(
        t,
        c"nested/nested_file.txt",
        None,
        "Test Case 4: File within a nested directory",
    )?;
    set(
        t,
        c"symlink_to_nested",
        None,
        "Test Case 5: Symlink to a file within the same directory",
    )?;
    set(
        t,
        c"/etc/passwd",
        Some(ENOTCAPABLE),
        "Test Case 6: File using an absolute path",
    )?;
    set(
        t,
        c"parent_symlink/outside_file.txt",
        Some(ENOTCAPABLE),
        "Test Case 7: Valid symlink to parent directory",
    )?;
    set(
        t,
        c"circular_symlink",
        Some(ELOOP),
        "Test Case 8: Circular symlink within directory",
    )?;
    set(
        t,
        c"../test_dir/inside_file.txt",
        Some(ENOTCAPABLE),
        "Test Case 9: Path can not escape outside at any point of the resolution",
    )?;
    set(
        t,
        c"symlink_absolute/test_dir/inside_file.txt",
        Some(ENOTCAPABLE),
        "Test Case 10: File using a symlink pointing to absolute path",
    )?;

    // `fsetattrlist`
    let mut attrlist = AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        commonattr: ATTR_CMN_FLAGS,
        ..Default::default()
    };
    // SAFETY: an open descriptor, and as above.
    let rc =
        unsafe { (fsetattrlist.get()?)(tree.fd, &mut attrlist, buf, size, FSOPT_RESOLVE_BENEATH) };
    expect_success(
        t,
        rc,
        "Test Case 11: fsetattrlist() syscall using FSOPT_RESOLVE_BENEATH",
    );

    // `setattrlistat`
    let mut at =
        |t: &mut T, path: &std::ffi::CStr, want: Option<c_int>, what: &str| -> Result<(), Stop> {
            // SAFETY: a directory descriptor, a path, an attribute list and its buffer.
            let rc = unsafe {
                (setattrlistat.get()?)(
                    tree.test_fd,
                    path.as_ptr(),
                    &mut attrlist,
                    buf,
                    size,
                    FSOPT_RESOLVE_BENEATH,
                )
            };
            match want {
                None => expect_success(t, rc, what),
                Some(errno) => expect_failure(t, rc, errno, what),
            }
            Ok(())
        };
    at(
        t,
        c"inside_file.txt",
        None,
        "Test Case 12: File within the directory",
    )?;
    at(
        t,
        c"symlink",
        Some(ENOTCAPABLE),
        "Test Case 13: File using a symlink pointing outside",
    )?;
    at(
        t,
        c"../outside_file.txt",
        Some(ENOTCAPABLE),
        "Test Case 14: File using \"..\" to navigate outside",
    )?;
    at(
        t,
        c"nested/nested_file.txt",
        None,
        "Test Case 15: File within a nested directory",
    )?;
    at(
        t,
        c"symlink_to_nested",
        None,
        "Test Case 16: Symlink to a file within the same directory",
    )?;
    at(
        t,
        c"/etc/passwd",
        Some(ENOTCAPABLE),
        "Test Case 17: File using an absolute path",
    )?;
    at(
        t,
        c"parent_symlink/outside_file.txt",
        Some(ENOTCAPABLE),
        "Test Case 18: Valid symlink to parent directory",
    )?;
    at(
        t,
        c"circular_symlink",
        Some(ELOOP),
        "Test Case 19: Circular symlink within directory",
    )?;
    at(
        t,
        c"../test_dir/inside_file.txt",
        Some(ENOTCAPABLE),
        "Test Case 20: Path can not escape outside at any point of the resolution",
    )?;
    at(
        t,
        c"symlink_absolute/test_dir/inside_file.txt",
        Some(ENOTCAPABLE),
        "Test Case 21: File using a symlink pointing to absolute path",
    )?;
    Ok(())
}

/// `disk_conditioner_info`, from `<sys/fsctl.h>`.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct DiskConditionerInfo {
    enabled: c_int,
    access_time_usec: u64,
    read_throughput_mbps: u64,
    write_throughput_mbps: u64,
    is_ssd: c_int,
    ioqueue_depth: u32,
    maxreadcnt: u32,
    maxwritecnt: u32,
    segreadcnt: u32,
    segwritecnt: u32,
}

const _: () = assert!(size_of::<DiskConditionerInfo>() == 56);

const fn ioc(inout: c_ulong, group: u8, num: u8, len: usize) -> c_ulong {
    inout | ((len as c_ulong & 0x1fff) << 16) | ((group as c_ulong) << 8) | num as c_ulong
}

/// `_IOR('A', 18, disk_conditioner_info)`
const DISK_CONDITIONER_IOC_GET: c_ulong =
    ioc(0x4000_0000, b'A', 18, size_of::<DiskConditionerInfo>());
/// `_IOW('A', 19, disk_conditioner_info)`
const DISK_CONDITIONER_IOC_SET: c_ulong =
    ioc(0x8000_0000, b'A', 19, size_of::<DiskConditionerInfo>());

/// `mktempmount`, which mounts a devfs on a new directory - as root. A new
/// directory stands in: what the conditioner keeps is per mount, and nothing
/// in this process has set any on the one the directory is on.
fn temp_mount() -> TempDir {
    TempDir::new("dmc")
}

fn fsctl_info(
    fsctl: FsctlFn,
    path: &Path,
    request: c_ulong,
    info: &mut DiskConditionerInfo,
) -> (c_int, c_int) {
    let path = cstr(path);
    // SAFETY: a path, and the structure the request names.
    let rc = unsafe {
        fsctl(
            path.as_ptr(),
            request,
            (info as *mut DiskConditionerInfo).cast(),
            0,
        )
    };
    (rc, errno())
}

/// `disk_mount_conditioner.m`: `fsctl_get_uninitialized`.
fn fsctl_get_uninitialized(t: &mut T) -> Result<(), Stop> {
    imports! { fsctl: FsctlFn = "_fsctl"; }
    let mount = temp_mount();
    let mut info = DiskConditionerInfo {
        enabled: 1,
        is_ssd: 1,
        ..Default::default()
    };
    let (err, e) = fsctl_info(fsctl.get()?, &mount.0, DISK_CONDITIONER_IOC_GET, &mut info);
    t.require(err == 0, || {
        format!("fsctl(DISK_CONDITIONER_IOC_GET): {err}, errno {e}")
    })?;
    t.require(info == DiskConditionerInfo::default(), || {
        format!("initial DMC info is zeroed: {info:?}")
    })?;
    Ok(())
}

/// `disk_mount_conditioner.m` (`TEST_UNENTITLED`): `fsctl_get_unentitled`,
/// and the entitled build's `fsctl_get_nonroot`, which is the same steps
/// without root.
fn fsctl_get_unentitled(t: &mut T) -> Result<(), Stop> {
    imports! { fsctl: FsctlFn = "_fsctl"; }
    let mount = temp_mount();
    let mut info = DiskConditionerInfo::default();
    let (err, e) = fsctl_info(fsctl.get()?, &mount.0, DISK_CONDITIONER_IOC_GET, &mut info);
    t.require(err == 0, || {
        format!("fsctl.get without entitlement: {err}, errno {e}")
    })?;
    Ok(())
}

/// `disk_mount_conditioner.m` (`TEST_UNENTITLED`): `fsctl_set_unentitled`,
/// and the entitled build's `fsctl_set_nonroot`, likewise.
fn fsctl_set_unentitled(t: &mut T) -> Result<(), Stop> {
    imports! { fsctl: FsctlFn = "_fsctl"; }
    let fsctl = fsctl.get()?;
    let mount = temp_mount();
    let mut expected = DiskConditionerInfo::default();
    let (err, e) = fsctl_info(fsctl, &mount.0, DISK_CONDITIONER_IOC_GET, &mut expected);
    t.require(err == 0, || {
        format!("Get original DMC info: {err}, errno {e}")
    })?;
    let mut info = DiskConditionerInfo {
        enabled: 1,
        access_time_usec: 10,
        ..Default::default()
    };
    let (err, _) = fsctl_info(fsctl, &mount.0, DISK_CONDITIONER_IOC_SET, &mut info);
    t.require(err != 0, || {
        "fsctl.set returns error without entitlement".into()
    })?;
    let (err, e) = fsctl_info(fsctl, &mount.0, DISK_CONDITIONER_IOC_GET, &mut info);
    t.require(err == 0, || {
        format!("fsctl.get after unentitled fsctl.set: {err}, errno {e}")
    })?;
    t.require(info == expected, || {
        format!("fsctl.set should not change info without entitlement: {info:?}")
    })?;
    Ok(())
}

fn needs_entitlement(_t: &mut T) -> Result<(), Stop> {
    Err(Stop::Skip(
        "the entitled build: needs root and com.apple.private.dmc.set, which no program rosemary runs holds".into(),
    ))
}

static CASES: &[Case] = &[
    Case {
        name: "file_leases (f)setattrlist",
        kind: Kind::Excerpt,
        source: "file_leases.c: the (f)setattrlist steps of the helper the read_lease_dir tests run",
        timeout_secs: 30,
        run: file_leases_setattrlist,
    },
    Case {
        name: "resolve_beneath_setattrlist",
        kind: Kind::Upstream,
        source: "vfs/resolve_beneath.c, 21 test cases",
        timeout_secs: 30,
        run: resolve_beneath_setattrlist,
    },
    Case {
        name: "fsctl_get_uninitialized",
        kind: Kind::Upstream,
        source: "disk_mount_conditioner.m; a new directory for the devfs mount",
        timeout_secs: 30,
        run: fsctl_get_uninitialized,
    },
    Case {
        name: "fsctl_get_unentitled",
        kind: Kind::Upstream,
        source: "disk_mount_conditioner.m, TEST_UNENTITLED; as above",
        timeout_secs: 30,
        run: fsctl_get_unentitled,
    },
    Case {
        name: "fsctl_set_unentitled",
        kind: Kind::Upstream,
        source: "disk_mount_conditioner.m, TEST_UNENTITLED; as above",
        timeout_secs: 30,
        run: fsctl_set_unentitled,
    },
    Case {
        name: "fsctl_set",
        kind: Kind::Upstream,
        source: "disk_mount_conditioner.m",
        timeout_secs: 5,
        run: needs_entitlement,
    },
    Case {
        name: "fsctl_set_zero",
        kind: Kind::Upstream,
        source: "disk_mount_conditioner.m",
        timeout_secs: 5,
        run: needs_entitlement,
    },
    Case {
        name: "fsctl_set_out_of_bounds",
        kind: Kind::Upstream,
        source: "disk_mount_conditioner.m",
        timeout_secs: 5,
        run: needs_entitlement,
    },
    Case {
        name: "fsctl_restore_mount_fields",
        kind: Kind::Upstream,
        source: "disk_mount_conditioner.m",
        timeout_secs: 5,
        run: needs_entitlement,
    },
    Case {
        name: "fsctl_get_nonroot",
        kind: Kind::Upstream,
        source: "disk_mount_conditioner.m; a new directory for the devfs mount",
        timeout_secs: 30,
        run: fsctl_get_unentitled,
    },
    Case {
        name: "fsctl_set_nonroot",
        kind: Kind::Upstream,
        source: "disk_mount_conditioner.m; as above",
        timeout_secs: 30,
        run: fsctl_set_unentitled,
    },
    Case {
        name: "fsctl_delays",
        kind: Kind::Upstream,
        source: "disk_mount_conditioner.m",
        timeout_secs: 5,
        run: needs_entitlement,
    },
];

#[test]
fn xnu_conformance() {
    conformance::run("xnu_conformance", "xnu", UPSTREAM, CASES);
}
