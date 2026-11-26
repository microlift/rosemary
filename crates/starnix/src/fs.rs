//! Questions about the filesystem a path is on.

use std::ffi::CStr;

use crate::sys;

/// Whether the filesystem holding `directory` looks names up without regard
/// to case, so that a lookup that failed by its exact name cannot succeed by
/// another spelling. `false` where the host cannot say.
pub fn folds_case(directory: &CStr) -> bool {
    sys::folds_case(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_directory_does_not_fold_case() {
        assert!(!folds_case(c"/nonexistent/starnix-fs-test"));
    }
}
