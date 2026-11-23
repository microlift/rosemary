//! The memory model this thread's loads and stores run under.

use crate::sys;

/// Ask the host to run this thread under x86's total store order in hardware.
///
/// Per thread, and true only if the kernel confirms the mode is now in effect.
/// Apple silicon has the mode, but only Asahi Linux lets a process ask for it;
/// macOS reserves it for Rosetta.
pub fn enter_total_store_order() -> bool {
    sys::enter_total_store_order()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asking_twice_gives_the_same_answer() {
        assert_eq!(enter_total_store_order(), enter_total_store_order());
    }
}
