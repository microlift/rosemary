use std::cell::Cell;

/// Linux's `SYSCALL_DISPATCH_FILTER_ALLOW`: syscalls go to the host kernel.
pub(super) const FILTER_ALLOW: u8 = 0;
/// Linux's `SYSCALL_DISPATCH_FILTER_BLOCK`: syscalls from outside the host's libc
/// trap into [`super::generic`].
pub(super) const FILTER_BLOCK: u8 = 1;

thread_local! {
    /// The byte the kernel reads before every syscall made from outside the
    /// excluded region. Const-initialized and without a destructor, so its
    /// address is fixed for the thread's life and readable in a signal handler.
    pub(super) static SELECTOR: Cell<u8> = const { Cell::new(FILTER_ALLOW) };
}

fn switch(to: u8) -> u8 {
    SELECTOR.with(|selector| selector.replace(to))
}

/// Guest code runs within: its raw syscalls are Darwin's and must not reach the
/// host kernel. Nests with [`ScopedHostSyscallContext`], restoring on drop.
pub struct ScopedGuestSyscallContext {
    previous: u8,
}

impl ScopedGuestSyscallContext {
    pub fn new() -> Self {
        super::interpose::arm_current_thread();
        Self {
            previous: switch(FILTER_BLOCK),
        }
    }
}

impl Default for ScopedGuestSyscallContext {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ScopedGuestSyscallContext {
    fn drop(&mut self) {
        switch(self.previous);
    }
}

/// Host code that makes raw syscalls without going through libc - rustix, and
/// anything built on it such as `starnix` - runs within.
pub struct ScopedHostSyscallContext {
    previous: u8,
}

impl ScopedHostSyscallContext {
    pub fn new() -> Self {
        Self {
            previous: switch(FILTER_ALLOW),
        }
    }
}

impl Default for ScopedHostSyscallContext {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ScopedHostSyscallContext {
    fn drop(&mut self) {
        switch(self.previous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selector() -> u8 {
        SELECTOR.with(|selector| selector.get())
    }

    #[test]
    fn nested_contexts_restore_the_outer_one() {
        assert_eq!(selector(), FILTER_ALLOW);
        {
            let _guest = ScopedGuestSyscallContext::new();
            assert_eq!(selector(), FILTER_BLOCK);
            {
                let _host = ScopedHostSyscallContext::new();
                assert_eq!(selector(), FILTER_ALLOW);
                {
                    let _callback = ScopedGuestSyscallContext::new();
                    assert_eq!(selector(), FILTER_BLOCK);
                }
                assert_eq!(selector(), FILTER_ALLOW);
            }
            assert_eq!(selector(), FILTER_BLOCK);
        }
        assert_eq!(selector(), FILTER_ALLOW);
    }
}
