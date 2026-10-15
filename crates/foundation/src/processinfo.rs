use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// `NSOperatingSystemVersion`, three `NSInteger`s.
#[repr(C)]
pub struct OperatingSystemVersion {
    major: i64,
    minor: i64,
    patch: i64,
}

/// `NSProcessInfoThermalState`, of which only `Nominal` is reported.
///
/// Nothing here watches the machine's thermal state, and the three states above
/// nominal all mean "the program should do less work". Claiming one would make a
/// program throttle itself for a condition this runtime cannot observe.
const THERMAL_STATE_NOMINAL: i64 = 0;

/// The one process, which `+processInfo` returns every time.
static SHARED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

unsafe extern "C" fn process_info(class: Id, _sel: Sel) -> Id {
    *SHARED.get_or_init(|| {
        // SAFETY: `class` is `NSProcessInfo`, which this module registered.
        let object = unsafe {
            objc::object::create_instance(
                objc::class::from_ptr(class as *const objc::class::Class).expect("a class"),
                0,
            )
        };
        // Retained forever, as every singleton in this tree is: one a guest can
        // release is one it can make dangle by releasing what it never owned.
        objc::arc::make_immortal(object);
        object as usize
    }) as Id
}

/// The macOS version, as the rest of this runtime states it - the host's own
/// is not the one a guest is told on any other host.
fn version() -> (i64, i64, i64) {
    let (major, minor, patch) = darwin::sysctl::macos_version();
    (major as i64, minor as i64, patch as i64)
}

unsafe extern "C" fn operating_system_version(_receiver: Id, _sel: Sel) -> OperatingSystemVersion {
    let (major, minor, patch) = version();
    OperatingSystemVersion {
        major,
        minor,
        patch,
    }
}

/// `-operatingSystemVersionString`, which is prose and not a version.
///
/// Apple's reads `Version 15.0 (Build 24A335)`, and a program that parses it is
/// doing so because it wants the build - so the build is in it.
unsafe extern "C" fn operating_system_version_string(_receiver: Id, _sel: Sel) -> Id {
    let (major, minor, patch) = version();
    let text = match patch {
        0 => format!("Version {major}.{minor}"),
        _ => format!("Version {major}.{minor}.{patch}"),
    };
    autoreleased(&text)
}

/// `-operatingSystem`, `NSMACHOperatingSystem`, which is what every macOS is.
unsafe extern "C" fn operating_system(_receiver: Id, _sel: Sel) -> i64 {
    5
}

unsafe extern "C" fn operating_system_name(_receiver: Id, _sel: Sel) -> Id {
    autoreleased("NSMACHOperatingSystem")
}

/// `-isOperatingSystemAtLeastVersion:`, compared field by field.
unsafe extern "C" fn at_least_version(
    _receiver: Id,
    _sel: Sel,
    wanted: OperatingSystemVersion,
) -> u8 {
    let (major, minor, patch) = version();
    u8::from((major, minor, patch) >= (wanted.major, wanted.minor, wanted.patch))
}

unsafe extern "C" fn processor_count(_receiver: Id, _sel: Sel) -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

/// `-physicalMemory`, in bytes.
///
/// Zero is refused rather than reported: a caller sizes a cache as a fraction of
/// it, and a fraction of zero is a cache that holds nothing. Where the machine
/// cannot be asked, the answer is the smallest one a Mac has ever shipped with.
unsafe extern "C" fn physical_memory(_receiver: Id, _sel: Sel) -> u64 {
    const FALLBACK: u64 = 4 << 30;
    // SAFETY: two `sysconf` names, and the multiplication is checked below.
    let bytes = unsafe {
        (libc::sysconf(libc::_SC_PHYS_PAGES) as u64)
            .saturating_mul(libc::sysconf(libc::_SC_PAGESIZE) as u64)
    };
    match bytes == 0 {
        true => FALLBACK,
        false => bytes,
    }
}

unsafe extern "C" fn process_identifier(_receiver: Id, _sel: Sel) -> i32 {
    std::process::id() as i32
}

/// `-processName`: the guest executable's name. The host's own `argv[0]` is
/// this runtime, which a program would put in its log and lock file names.
unsafe extern "C" fn process_name(_receiver: Id, _sel: Sel) -> Id {
    let guest = guest_arguments().into_iter().next();
    let name = guest
        .or_else(|| std::env::args_os().next())
        .and_then(|arg| {
            std::path::Path::new(&arg)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "rosemary".to_string());
    autoreleased(&name)
}

static GUEST_ARGUMENTS: std::sync::Mutex<Option<Vec<std::ffi::OsString>>> =
    std::sync::Mutex::new(None);

/// Record guest argv before running guest initializers or the entry point.
pub fn set_arguments(arguments: &[std::ffi::OsString]) {
    *GUEST_ARGUMENTS.lock().unwrap_or_else(|e| e.into_inner()) = Some(arguments.to_vec());
}

/// Guest argv only; host driver options must not become guest defaults.
pub fn guest_arguments() -> Vec<std::ffi::OsString> {
    GUEST_ARGUMENTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// `-arguments`, including the guest executable as the first element.
unsafe extern "C" fn arguments(_receiver: Id, _sel: Sel) -> Id {
    let args = GUEST_ARGUMENTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| std::env::args_os().collect());
    let strings: Vec<corefoundation::base::CFTypeRef> = args
        .iter()
        .map(|arg| {
            crate::string::from_str(&arg.to_string_lossy()) as corefoundation::base::CFTypeRef
        })
        .collect();
    let array = corefoundation::collections::new_array(
        &strings,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: the array retained each; these are this call's own references.
    for string in strings {
        unsafe { corefoundation::api::CFRelease(string) };
    }
    // SAFETY: a fresh array this call owns.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn environment(_receiver: Id, _sel: Sel) -> Id {
    // The guest's block, which is not this process's under the builtin libc.
    let mut seen = std::collections::HashSet::new();
    let entries: Vec<(
        corefoundation::base::CFTypeRef,
        corefoundation::base::CFTypeRef,
    )> = darwin::guest_environment()
        .iter()
        .filter_map(|entry| {
            let entry = entry.to_bytes();
            let split = entry.iter().position(|&b| b == b'=')?;
            Some((&entry[..split], &entry[split + 1..]))
        })
        // The first of a duplicated name, as `getenv` answers.
        .filter(|(name, _)| seen.insert(*name))
        .map(|(name, value)| {
            (
                crate::string::from_str(&String::from_utf8_lossy(name))
                    as corefoundation::base::CFTypeRef,
                crate::string::from_str(&String::from_utf8_lossy(value))
                    as corefoundation::base::CFTypeRef,
            )
        })
        .collect();
    let dictionary = corefoundation::collections::new_dictionary(
        &entries,
        corefoundation::callbacks::Policy::from_key_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: the dictionary retained both halves of every entry.
    for (name, value) in entries {
        unsafe {
            corefoundation::api::CFRelease(name);
            corefoundation::api::CFRelease(value);
        }
    }
    // SAFETY: a fresh dictionary this call owns.
    unsafe { corefoundation::api::CFAutorelease(dictionary) as Id }
}

/// `-systemUptime`, seconds the machine has been awake since boot as a `double`.
///
/// The clock `mach_absolute_time` reads, and so the one `-[NSEvent timestamp]`
/// is on: a program stamps a moment with this and compares later events against
/// it. Time asleep is not counted by either, and a clock that did count it would
/// put the stamp ahead of every event from the first suspend on.
unsafe extern "C" fn system_uptime(_receiver: Id, _sel: Sel) -> f64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a writable `timespec` and a clock every platform here has.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } != 0 {
        return 0.0;
    }
    now.tv_sec as f64 + now.tv_nsec as f64 / 1e9
}

unsafe extern "C" fn thermal_state(_receiver: Id, _sel: Sel) -> i64 {
    THERMAL_STATE_NOMINAL
}

unsafe extern "C" fn low_power_mode(_receiver: Id, _sel: Sel) -> u8 {
    0
}

/// `-globallyUniqueString`, which is a UUID and a process identity.
unsafe extern "C" fn globally_unique_string(_receiver: Id, _sel: Sel) -> Id {
    let uuid = corefoundation::uuid::format(&corefoundation::uuid::generate());
    autoreleased(&format!("{uuid}-{}", std::process::id()))
}

/// `-hostName`, which is the machine's name and not the process's.
unsafe extern "C" fn host_name(_receiver: Id, _sel: Sel) -> Id {
    let mut buffer = [0u8; 256];
    // SAFETY: a buffer described by its own length; the result is NUL-terminated
    // by `gethostname` on success.
    let name = match unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len() - 1) } {
        0 => String::from_utf8_lossy(&buffer[..buffer.iter().position(|b| *b == 0).unwrap_or(0)])
            .into_owned(),
        _ => String::new(),
    };
    autoreleased(&name)
}

fn autoreleased(text: &str) -> Id {
    let string = crate::string::from_str(text);
    // SAFETY: a fresh object this call owns.
    unsafe { corefoundation::api::CFAutorelease(string as corefoundation::base::CFTypeRef) as Id }
}

/// `-beginActivityWithOptions:reason:`: a token for `-endActivity:`.
///
/// An activity only tells macOS not to App Nap or idle-sleep the process,
/// neither of which a host here does to it, so the token stands for nothing.
unsafe extern "C" fn begin_activity(_receiver: Id, _sel: Sel, _options: u64, _reason: Id) -> Id {
    let class = objc::class::lookup(c"NSObject").expect("the runtime's own NSObject");
    objc::arc::autorelease(objc::object::create_instance(class, 0))
}

unsafe extern "C" fn end_activity(_receiver: Id, _sel: Sel, _activity: Id) {}

pub(crate) fn install() {
    let class =
        corefoundation::base::register(c"NSProcessInfo", None, std::mem::size_of::<usize>(), false);
    class_methods!(class, {
        "processInfo" => process_info, c"@16@0:8";
    });
    methods!(class, {
        "operatingSystemVersion" => operating_system_version, c"{?=qqq}16@0:8";
        "operatingSystemVersionString" => operating_system_version_string, c"@16@0:8";
        "operatingSystem" => operating_system, c"q16@0:8";
        "operatingSystemName" => operating_system_name, c"@16@0:8";
        "isOperatingSystemAtLeastVersion:" => at_least_version, c"c40@0:8{?=qqq}16";
        "processorCount" => processor_count, c"Q16@0:8";
        "activeProcessorCount" => processor_count, c"Q16@0:8";
        "physicalMemory" => physical_memory, c"Q16@0:8";
        "processIdentifier" => process_identifier, c"i16@0:8";
        "processName" => process_name, c"@16@0:8";
        "arguments" => arguments, c"@16@0:8";
        "environment" => environment, c"@16@0:8";
        "systemUptime" => system_uptime, c"d16@0:8";
        "thermalState" => thermal_state, c"q16@0:8";
        "isLowPowerModeEnabled" => low_power_mode, c"c16@0:8";
        "globallyUniqueString" => globally_unique_string, c"@16@0:8";
        "hostName" => host_name, c"@16@0:8";
        "beginActivityWithOptions:reason:" => begin_activity, c"@32@0:8Q16@24";
        "endActivity:" => end_activity, c"v24@0:8@16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_contain_only_guest_argv() {
        crate::bootstrap();
        let guest: Vec<std::ffi::OsString> = [
            "/Applications/Guest App",
            "--emulated",
            "run",
            "",
            "hello world",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        set_arguments(&guest);
        // SAFETY: arguments ignores the receiver and returns an NSArray of strings.
        unsafe {
            let result = arguments(std::ptr::null_mut(), objc::sel::get("arguments"));
            let array =
                corefoundation::collections::array(result as corefoundation::base::CFTypeRef)
                    .expect("an array");
            let actual: Vec<Vec<u8>> = array
                .values
                .iter()
                .map(|value| crate::string::bytes(*value as Id))
                .collect();
            let expected: Vec<Vec<u8>> = guest
                .iter()
                .map(|value| value.as_encoded_bytes().to_vec())
                .collect();
            assert_eq!(actual, expected);
            let name = process_name(std::ptr::null_mut(), objc::sel::get("processName"));
            assert_eq!(crate::string::bytes(name), b"Guest App");
        }
        *GUEST_ARGUMENTS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The whole point of the class: a version that is not zero, because
    /// `[[nil processInfo] operatingSystemVersion]` is macOS 0.0.0 and every
    /// minimum-version check passes or fails on it.
    #[test]
    fn the_operating_system_is_not_version_zero() {
        crate::bootstrap();
        // SAFETY: neither argument is read.
        let version =
            unsafe { operating_system_version(std::ptr::null_mut(), objc::sel::get("x")) };
        assert!(version.major >= 10, "macOS {}", version.major);
    }

    /// A version comparison has to answer both ways round, or a caller that
    /// gates on it is gated by a constant.
    #[test]
    fn a_version_is_compared_against_and_not_ignored() {
        crate::bootstrap();
        let ask = |major| {
            let wanted = OperatingSystemVersion {
                major,
                minor: 0,
                patch: 0,
            };
            // SAFETY: neither of the first two arguments is read.
            unsafe { at_least_version(std::ptr::null_mut(), objc::sel::get("x"), wanted) }
        };
        assert_eq!(ask(10), 1);
        assert_eq!(ask(99), 0);
    }

    /// `+processInfo` is a singleton, which is what makes a guest's cached
    /// pointer to it stay valid.
    #[test]
    fn the_process_is_one_object() {
        crate::bootstrap();
        let class = corefoundation::base::register(
            c"NSProcessInfo",
            None,
            std::mem::size_of::<usize>(),
            false,
        );
        let class = objc::class::ptr(class) as Id;
        // SAFETY: the class this module registered.
        let (a, b) = unsafe {
            (
                process_info(class, objc::sel::get("processInfo")),
                process_info(class, objc::sel::get("processInfo")),
            )
        };
        assert!(!a.is_null());
        assert_eq!(a, b);
    }
}
