use objc::object::Id;

/// The directory a search-path constant names, relative to a domain's root.
///
/// `None` for a constant this tree cannot place. Named rather than guessed at:
/// a wrong directory here is a save file written somewhere the guest will not
/// look for it again.
fn relative(directory: u64) -> Option<&'static str> {
    Some(match directory {
        1 => "Applications",
        3 => "Developer/Applications",
        4 => "Applications/Utilities",
        5 => "Library",
        6 => "Developer",
        8 => "Library/Documentation",
        9 => "Documents",
        11 => "Library/Autosave Information",
        12 => "Desktop",
        13 => "Library/Caches",
        14 => "Library/Application Support",
        15 => "Downloads",
        16 => "Library/Input Methods",
        17 => "Movies",
        18 => "Music",
        19 => "Pictures",
        20 => "Library/PrinterDescriptions",
        22 => "Library/PreferencePanes",
        _ => return None,
    })
}

/// The root of each domain in `mask`, in the order Apple returns them.
///
/// Guest paths throughout - `/System`, `/Network` and the guest's own home -
/// because these are answers a guest is given, not files this crate opens.
///
/// `expand` false keeps the user domain's tilde, which is the whole point of
/// that argument: a caller passing false means to store the path, not open it.
fn roots(mask: u64, expand: bool) -> Vec<String> {
    let mut out = Vec::new();
    if mask & 1 != 0 {
        out.push(match expand {
            true => crate::home_directory(),
            false => "~".to_string(),
        });
    }
    if mask & 2 != 0 {
        out.push(String::new());
    }
    if mask & 4 != 0 {
        out.push("/Network".to_string());
    }
    if mask & 8 != 0 {
        out.push("/System".to_string());
    }
    out
}

/// The absolute paths a search-path constant names in each domain of `mask`.
///
/// The same answer [`search_path`] gives with `expand` true, as strings rather
/// than as objects - which is what the `NSURL` forms on `NSFileManager` need,
/// and the reason this is one function rather than two tables that could drift.
pub(crate) fn directory_paths(directory: u64, mask: u64) -> Vec<String> {
    let Some(relative) = relative(directory) else {
        return Vec::new();
    };
    roots(mask, true)
        .into_iter()
        .map(|root| format!("{root}/{relative}"))
        .collect()
}

/// `NSArray *NSSearchPathForDirectoriesInDomains(NSSearchPathDirectory,
/// NSSearchPathDomainMask, BOOL)`.
///
/// A directory this tree cannot place gives an empty array, which is what Apple
/// gives for one that does not exist in the requested domain - a caller already
/// has to handle it, and it leaves the program to say what it wanted rather than
/// sending it to the wrong place.
unsafe extern "C" fn search_path(directory: u64, mask: u64, expand: u8) -> Id {
    let paths: Vec<String> = match relative(directory) {
        Some(relative) => roots(mask, expand != 0)
            .into_iter()
            .map(|root| format!("{root}/{relative}"))
            .collect(),
        None => Vec::new(),
    };
    let objects: Vec<corefoundation::base::CFTypeRef> = paths
        .iter()
        .map(|p| corefoundation::string::from_bytes(p.as_bytes(), false))
        .collect();
    let array = corefoundation::collections::new_array(
        &objects,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for object in objects {
        // SAFETY: a string this function made and the array has retained.
        unsafe { corefoundation::api::CFRelease(object) };
    }
    // SAFETY: a fresh array.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn home_directory() -> Id {
    crate::string::from_str(&crate::home_directory())
}

/// The `~/Library` subdirectories every Mac ships, created if this host has not
/// got them.
///
/// The list is what a guest may assume rather than everything Apple ships: an
/// unused directory created here costs an empty directory, and a missing one
/// costs a failure the guest reports as something else.
///
/// Created at their *host* spelling: under a prefix these belong to the
/// prefix's own home, not to the host's.
#[cfg(not(target_os = "macos"))]
pub fn ensure_standard_directories() {
    let home = crate::host_home_directory();
    for relative in [
        "Library/Application Support",
        "Library/Caches",
        "Library/Logs",
        "Library/Preferences",
        "Library/Saved Application State",
    ] {
        let _ = std::fs::create_dir_all(home.join(relative));
    }
}

unsafe extern "C" fn temporary_directory() -> Id {
    // Apple's ends in a separator, and a caller that concatenates rather than
    // using `-stringByAppendingPathComponent:` depends on it.
    // The host's TMPDIR may sit below a private shell directory that the
    // guest prefix does not contain.
    // TODO: make this read the guest's environ block incase they set TMPDIR
    // themselves
    crate::string::from_str("/tmp/")
}

exports::exports! {
    functions {
        "NSSearchPathForDirectoriesInDomains" => search_path,
        "NSHomeDirectory" => home_directory,
        "NSTemporaryDirectory" => temporary_directory,
    }
}
