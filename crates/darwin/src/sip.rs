use std::ffi::c_int;

pub extern "C" fn csr_check(_mask: u32) -> c_int {
    0
}

/// `CSR_ALLOW_UNTRUSTED_KEXTS` through `CSR_ALLOW_UNAUTHENTICATED_ROOT`, Apple's
/// `CSR_VALID_FLAGS`.
const CSR_VALID_FLAGS: u32 = CSR_ALLOW_UNTRUSTED_KEXTS
    | CSR_ALLOW_UNRESTRICTED_FS
    | CSR_ALLOW_TASK_FOR_PID
    | CSR_ALLOW_KERNEL_DEBUGGER
    | CSR_ALLOW_APPLE_INTERNAL
    | CSR_ALLOW_UNRESTRICTED_DTRACE
    | CSR_ALLOW_UNRESTRICTED_NVRAM
    | CSR_ALLOW_DEVICE_CONFIGURATION
    | CSR_ALLOW_ANY_RECOVERY_OS
    | CSR_ALLOW_UNAPPROVED_KEXTS
    | CSR_ALLOW_EXECUTABLE_POLICY_OVERRIDE
    | CSR_ALLOW_UNAUTHENTICATED_ROOT;
const CSR_ALLOW_UNTRUSTED_KEXTS: u32 = 1 << 0;
const CSR_ALLOW_UNRESTRICTED_FS: u32 = 1 << 1;
const CSR_ALLOW_TASK_FOR_PID: u32 = 1 << 2;
const CSR_ALLOW_KERNEL_DEBUGGER: u32 = 1 << 3;
const CSR_ALLOW_APPLE_INTERNAL: u32 = 1 << 4;
const CSR_ALLOW_UNRESTRICTED_DTRACE: u32 = 1 << 5;
const CSR_ALLOW_UNRESTRICTED_NVRAM: u32 = 1 << 6;
const CSR_ALLOW_DEVICE_CONFIGURATION: u32 = 1 << 7;
const CSR_ALLOW_ANY_RECOVERY_OS: u32 = 1 << 8;
const CSR_ALLOW_UNAPPROVED_KEXTS: u32 = 1 << 9;
const CSR_ALLOW_EXECUTABLE_POLICY_OVERRIDE: u32 = 1 << 10;
const CSR_ALLOW_UNAUTHENTICATED_ROOT: u32 = 1 << 11;

/// `int csr_get_active_config(csr_config_t *config)`
///
/// Every exception allowed, so that it agrees with [`csr_check`]: nothing on
/// this host enforces System Integrity Protection.
///
/// # Safety
///
/// `config` is null or writable.
pub unsafe extern "C" fn csr_get_active_config(config: *mut u32) -> c_int {
    if config.is_null() {
        return libc::EINVAL;
    }
    // SAFETY: the caller states it is writable.
    unsafe { config.write_unaligned(CSR_VALID_FLAGS) };
    0
}

pub extern "C" fn rootless_restricted_environment() -> c_int {
    0
}

pub extern "C" fn rootless_check_trusted_fd(fd: c_int) -> c_int {
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return -1;
    }
    1
}

exports::exports! {
    functions {
        "csr_check" => csr_check,
        "csr_get_active_config" => csr_get_active_config,
        "rootless_restricted_environment" => rootless_restricted_environment,
        "rootless_check_trusted_fd" => rootless_check_trusted_fd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_protected() {
        assert_eq!(csr_check(1 << 1), 0);
        let mut config = 0;
        // SAFETY: a writable local.
        assert_eq!(unsafe { csr_get_active_config(&mut config) }, 0);
        assert_eq!(
            config & CSR_ALLOW_UNRESTRICTED_FS,
            CSR_ALLOW_UNRESTRICTED_FS
        );
        assert_eq!(rootless_restricted_environment(), 0);
        assert_eq!(rootless_check_trusted_fd(2), 1);
        assert_eq!(rootless_check_trusted_fd(-1), -1);
    }
}
