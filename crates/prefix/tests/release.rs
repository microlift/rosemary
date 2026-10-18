use std::path::PathBuf;

use prefix::cache::Cache;
use prefix::provision::{self, Answer, Chosen};
use prefix::verify::{Scope, compare};

fn scratch(name: &str) -> PathBuf {
    let at = std::env::temp_dir().join(format!(
        "rosemary-prefix-release-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&at);
    std::fs::create_dir_all(&at).unwrap();
    at
}

#[test]
fn the_pinned_release_verifies_offline() {
    for arch in prefix::pinned::architectures() {
        let mut chosen = Chosen::pinned(arch).expect("pinned spec");
        let bundle = std::env::var_os("ROSEMARY_TEST_BUNDLE")
            .map(PathBuf::from)
            .filter(|path| path.to_string_lossy().contains(arch));
        let user_cache = Cache::from_env();
        let cached = user_cache.as_ref().is_some_and(|cache| {
            let spec = &chosen.spec;
            cache
                .artifact(spec.artifact.sha256.as_deref().unwrap())
                .is_file()
                && cache
                    .manifest(spec.manifest.sha256.as_deref().unwrap())
                    .is_file()
        });
        let at = scratch(arch);
        let cache = match (&bundle, cached) {
            (Some(bundle), _) => {
                chosen.bundle = Some(bundle.clone());
                Cache::at(at.join("cache"))
            }
            (None, true) => user_cache.unwrap(),
            (None, false) => {
                eprintln!(
                    "skipped {arch}: set ROSEMARY_TEST_BUNDLE to its bundle.zip, or cache it \
                     with `rosemary prefix create`"
                );
                continue;
            }
        };
        let mut lines = Vec::new();
        let obtained = provision::obtain(&cache, &chosen, &mut |_| Answer::Decline, &mut |line| {
            lines.push(line.to_string())
        })
        .unwrap_or_else(|e| panic!("{arch}: {e}\n{}", lines.join("\n")));
        let manifest = &obtained.manifest;
        assert_eq!(Some(&manifest.tree), chosen.spec.tree.as_ref());
        assert_eq!(manifest.arch(), arch);
        assert_eq!(
            compare(&obtained.tree, manifest, false, &Scope::All).unwrap(),
            []
        );
        if arch == "x86_64" {
            // The numbers the manifest recorded for v14.0.0.
            let s = manifest.summary;
            assert_eq!(manifest.entries.len(), 12898);
            assert_eq!((s.files, s.symlinks, s.directories), (8669, 3733, 496));
            assert_eq!(s.bytes, 161_809_838);
        }
        let _ = std::fs::remove_dir_all(&at);
    }
}
