//! What the four-argument resource lookup answers for the **real** Chromium on
//! this box, asserted rather than described.
//!
//! Every other claim about `-[NSBundle pathForResource:ofType:inDirectory:
//! forLocalization:]` is about a scratch bundle built in a temporary directory.
//! This one is about the 337 MB browser the milestone actually runs, because
//! the answer that matters is whether a real `.pak` is there to be named - and
//! that is a fact about the filesystem, not about the shape of a bundle.
//!
//! Where `test/Chromium` is not extracted, every assertion here is skipped and
//! the test says so: a test that could only pass on the machine it was written
//! on would be a test that says nothing on any other.
//!
//! ```text
//! cargo test -p foundation --test pak_lookup -- --nocapture
//! ```

use std::path::{Path, PathBuf};

use foundation::bundle::Layout;

const APP: &str = "test/Chromium/Chromium.app";
const FRAMEWORK: &str = "Contents/Frameworks/Chromium Framework.framework/Versions/130.0.6723.91";

/// The app bundle, found by walking up from this crate, so the measurement runs
/// the same way whichever directory `cargo test` was started in.
fn app() -> Option<PathBuf> {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let candidate = dir.join(APP);
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn lookup(layout: &Layout, name: &str, kind: &str, localization: Option<&str>) -> Option<PathBuf> {
    layout.resource_in_localized(name, kind, "", localization)
}

fn show(label: &str, answer: Option<&Path>) {
    println!(
        "{label:<46} {:?}",
        answer.map(|p| p.display().to_string())
    );
}

#[test]
fn the_real_chromium_answers_a_real_pak_for_en() {
    let Some(app) = app() else {
        println!("no {APP} above this crate; nothing to measure");
        return;
    };
    let framework_path = app.join(FRAMEWORK);
    let Some(framework) = Layout::at(&framework_path) else {
        panic!(
            "{} is not a bundle, so there is nothing for the lookup to find",
            framework_path.display()
        );
    };

    // The guest's own query, argument for argument:
    // `GetResourcesPakFilePath(@"locale", mac_locale)` in
    // `ui/base/resource/resource_bundle_mac.mm`, which is
    // `pathForResource:@"locale" ofType:@"pak" inDirectory:@"" forLocalization:@"en"`.
    let en = lookup(&framework, "locale", "pak", Some("en"));
    show("Framework  locale/pak/\"\"/en", en.as_deref());
    assert_eq!(
        en.as_deref(),
        Some(framework.resources.join("en.lproj/locale.pak").as_path()),
        "and it is the en.lproj copy, named by the directory it was found in"
    );
    // A path that is not there is the failure this whole design is about, so
    // the file is opened rather than the string trusted.
    assert!(en.as_deref().is_some_and(|p| p.is_file()), "en.lproj/locale.pak is a file");

    // The localization argument has to *select*, or the answer is "the only
    // locale.pak that happened to be findable" wearing a localization's name.
    let de = lookup(&framework, "locale", "pak", Some("de"));
    show("Framework  locale/pak/\"\"/de", de.as_deref());
    assert!(de.as_deref().is_some_and(|p| p.is_file()));
    assert_ne!(de, en, "and it is not en's, so the argument did the work");
    // A language the bundle ships no pak for is nil, not some other language's
    // and not the unlocalized one.
    assert_eq!(lookup(&framework, "locale", "pak", Some("qqq")), None);
    show("Framework  locale/pak/\"\"/qqq", None);

    // The two-argument form, for `chrome_100_percent`, is the same lookup with
    // no localization named and is what `LoadCommonResources` sends.
    let common = framework.resource("chrome_100_percent", "pak");
    show("Framework  [pathForResource:ofType:]", common.as_deref());
    assert!(common.as_deref().is_some_and(|p| p.is_file()));

    // The main bundle is the other bundle this can be sent to, and it is the
    // one `base::apple::FrameworkBundle()` answers until
    // `SetOverrideFrameworkBundlePath` replaces it. It has no pak at all, so
    // nil here is the truth and not a gap: returning the Framework's file for
    // the `.app` would be a path into a bundle nobody asked about.
    let main = Layout::at(&app).expect("the .app is a bundle");
    let main_en = lookup(&main, "locale", "pak", Some("en"));
    show("mainBundle locale/pak/\"\"/en", main_en.as_deref());
    assert_eq!(main_en, None, "Contents/Resources/en.lproj ships InfoPlist.strings only");
    assert!(
        std::fs::read_dir(main.resources.join("en.lproj"))
            .is_ok_and(|d| d.flatten().all(|e| e.file_name() != "locale.pak")),
        "which is checkable: there is no locale.pak under the .app at all"
    );
    assert_eq!(lookup(&main, "chrome_100_percent", "pak", None), None);
}
