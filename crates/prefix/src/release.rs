//! Which macOS release a guest is told it runs on.
//!
//! One answer for the whole runtime: `kern.osrelease`, `Gestalt`,
//! `NSProcessInfo` and `SystemVersion.plist` are the same question, and a
//! program that compares them must never catch them disagreeing. It lives with
//! the prefix because the plist is a file in it.
//!
//! The defaults describe macOS 15.0. A program that branches on the OS version
//! needs a *specific* answer far more than it needs a true one, and the
//! environment is where that belongs rather than a rebuild.

use std::sync::OnceLock;

/// The guest path of the file that states the release.
pub const SYSTEM_VERSION_PLIST: &str = "/System/Library/CoreServices/SystemVersion.plist";

/// The Darwin kernel release, `kern.osrelease`.
///
/// Read once: everything below is derived from it, and must keep agreeing if
/// the variable changes later.
pub fn darwin() -> &'static str {
    static RELEASE: OnceLock<String> = OnceLock::new();
    RELEASE.get_or_init(|| env_or("ROSEMARY_DARWIN_RELEASE", "24.0.0"))
}

/// The build, `kern.osversion` and the plist's `ProductBuildVersion`.
pub fn build() -> &'static str {
    static BUILD: OnceLock<String> = OnceLock::new();
    BUILD.get_or_init(|| env_or("ROSEMARY_DARWIN_BUILD", "24A335"))
}

/// The macOS version [`darwin`] implies, as `(major, minor, patch)`.
///
/// Darwin 20 is macOS 11 and each is one apart after that; before it, every
/// Darwin was some 10.x.
pub fn macos_version() -> (i32, i32, i32) {
    static VERSION: OnceLock<(i32, i32, i32)> = OnceLock::new();
    *VERSION.get_or_init(|| {
        let mut parts = darwin().split('.').map(|p| p.parse::<i32>().unwrap_or(0));
        let darwin = parts.next().unwrap_or(24);
        let minor = parts.next().unwrap_or(0);
        let patch = parts.next().unwrap_or(0);
        match darwin >= 20 {
            true => (darwin - 9, minor, patch),
            false => (10, darwin - 4, minor),
        }
    })
}

/// `kern.osproductversion`, spelled as macOS does: no patch component when it
/// is 0.
pub fn product_version() -> String {
    let (major, minor, patch) = macos_version();
    match patch {
        0 => format!("{major}.{minor}"),
        _ => format!("{major}.{minor}.{patch}"),
    }
}

/// `ProductName`, which Big Sur changed along with the major version.
pub fn product_name() -> &'static str {
    match macos_version() {
        (major, _, _) if major >= 11 => "macOS",
        _ => "Mac OS X",
    }
}

/// The contents of [`SYSTEM_VERSION_PLIST`].
///
/// `BuildID`, `ProductCopyright` and `iOSSupportVersion` are left out rather
/// than invented: none of them follows from the release.
pub fn system_version_plist() -> String {
    let version = escape(&product_version());
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
            "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<plist version=\"1.0\">\n",
            "<dict>\n",
            "\t<key>ProductBuildVersion</key>\n",
            "\t<string>{build}</string>\n",
            "\t<key>ProductName</key>\n",
            "\t<string>{name}</string>\n",
            "\t<key>ProductUserVisibleVersion</key>\n",
            "\t<string>{version}</string>\n",
            "\t<key>ProductVersion</key>\n",
            "\t<string>{version}</string>\n",
            "</dict>\n",
            "</plist>\n",
        ),
        build = escape(build()),
        name = product_name(),
        version = version,
    )
}

/// `text` as plist character data.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_and_the_macos_version_keep_agreeing() {
        let version = macos_version();
        let first = darwin();
        // SAFETY: no other test reads or writes this variable.
        unsafe { std::env::set_var("ROSEMARY_DARWIN_RELEASE", "99.0.0") };
        assert_eq!(darwin(), first);
        assert_eq!(macos_version(), version);
        unsafe { std::env::remove_var("ROSEMARY_DARWIN_RELEASE") };
    }

    #[test]
    fn the_plist_states_the_advertised_version() {
        let plist = system_version_plist();
        let version = format!("<string>{}</string>", product_version());
        assert_eq!(plist.matches(&version).count(), 2);
        assert!(plist.contains(&format!("<string>{}</string>", build())));
    }
}
