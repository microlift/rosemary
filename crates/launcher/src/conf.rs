//! The bundle's description of itself, `share/rosemary/bundle.conf`.
//!
//! `key=value` lines, `#` comments. Kept this plain because the launcher has
//! no dependencies to parse anything richer with.

use crate::glibc::Version;
use std::path::Path;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Conf {
    /// The newest glibc symbol version anything in the bundle needs. A host
    /// whose glibc defines at least this can run the bundle's libraries.
    pub required_glibc: Option<Version>,
    /// Variables to set unless the caller already did, as `(name, value)`
    /// with `@ROOT@` standing for the bundle root and `@CACHE@` for the
    /// user's cache directory.
    pub default_env: Vec<(String, String)>,
    /// Variables to set whatever the caller had, written like `default_env`:
    /// `env.NAME=value`. For settings where a host value would point bundled
    /// code at host files built against other versions of its libraries.
    pub env: Vec<(String, String)>,
    /// Variables to set, unless the caller already did, to the first of
    /// several host files that exists: `default_path.NAME=/a|/b`. For
    /// configuration each distribution keeps in its own place.
    pub default_path: Vec<(String, Vec<String>)>,
}

impl Conf {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    pub fn parse(text: &str) -> Self {
        let mut conf = Self::default();
        for line in text.lines() {
            let line = line.trim();
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            if key == "required_glibc" {
                conf.required_glibc = Version::parse(value);
            } else if let Some(name) = key.strip_prefix("default_env.") {
                conf.default_env.push((name.to_string(), value.to_string()));
            } else if let Some(name) = key.strip_prefix("env.") {
                conf.env.push((name.to_string(), value.to_string()));
            } else if let Some(name) = key.strip_prefix("default_path.") {
                let candidates = value.split('|').map(str::trim).filter(|c| !c.is_empty());
                conf.default_path
                    .push((name.to_string(), candidates.map(String::from).collect()));
            }
        }
        conf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_requirement_and_default_environment() {
        let conf =
            Conf::parse("# comment\nrequired_glibc = 2.42\ndefault_env.A=@ROOT@/x\n\nnonsense\n");
        assert_eq!(conf.required_glibc, Version::parse("2.42"));
        assert_eq!(
            conf.default_env,
            [("A".to_string(), "@ROOT@/x".to_string())]
        );
    }

    #[test]
    fn reads_forced_variables_including_empty_ones() {
        let conf = Conf::parse("env.A=@ROOT@/x\nenv.B=\n");
        assert_eq!(
            conf.env,
            [
                ("A".to_string(), "@ROOT@/x".to_string()),
                ("B".to_string(), String::new())
            ]
        );
        assert!(conf.default_env.is_empty());
    }

    #[test]
    fn reads_candidate_paths_in_order() {
        let conf = Conf::parse("default_path.F=/a | /b|\n");
        assert_eq!(
            conf.default_path,
            [("F".to_string(), vec!["/a".to_string(), "/b".to_string()])]
        );
    }

    #[test]
    fn a_missing_file_is_an_empty_conf() {
        assert_eq!(
            Conf::load(Path::new("/nonexistent/bundle.conf")),
            Conf::default()
        );
    }
}
