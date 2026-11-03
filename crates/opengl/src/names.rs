//! What each of the framework's three images exports on a Mac.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The image a name is exported from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Library {
    /// `OpenGL` itself: CGL, and private entry points.
    OpenGL,
    /// `Libraries/libGL.dylib`.
    Gl,
    /// `Libraries/libGLU.dylib`.
    Glu,
}

const LISTS: [(Library, &str); 3] = [
    (Library::OpenGL, include_str!("exports/OpenGL.txt")),
    (Library::Gl, include_str!("exports/libGL.txt")),
    (Library::Glu, include_str!("exports/libGLU.txt")),
];

/// One bare C name per line; `#` starts a comment line.
fn parse(list: &'static str) -> impl Iterator<Item = &'static str> {
    list.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

/// The image that exports the C name `name`, or `None` where a Mac exports it
/// from none of them.
pub fn library(name: &str) -> Option<Library> {
    static ALL: OnceLock<HashMap<&'static str, Library>> = OnceLock::new();
    ALL.get_or_init(|| {
        LISTS
            .iter()
            .flat_map(|&(library, list)| parse(list).map(move |name| (name, library)))
            .collect()
    })
    .get(name)
    .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_name_is_exported_twice() {
        let mut seen = std::collections::HashSet::new();
        for (_, list) in LISTS {
            for name in parse(list) {
                assert!(seen.insert(name), "{name}");
            }
        }
    }

    #[test]
    fn names_resolve_to_their_image() {
        assert_eq!(library("CGLChoosePixelFormat"), Some(Library::OpenGL));
        assert_eq!(library("CGLGetCurrentContext"), Some(Library::Gl));
        assert_eq!(library("glBindVertexArrayAPPLE"), Some(Library::Gl));
        assert_eq!(library("gluOrtho2D"), Some(Library::Glu));
        assert_eq!(library("glDispatchCompute"), None);
    }
}
