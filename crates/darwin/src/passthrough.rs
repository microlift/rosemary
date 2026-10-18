pub const CLAIMED: &[&str] = &[
    "memchr",
    "memcmp",
    "memcpy",
    "memmove",
    "memset",
    "strcasecmp",
    "strcat",
    "strchr",
    "strcmp",
    "strcoll",
    "strcpy",
    "strcspn",
    "strdup",
    "strlen",
    "strncasecmp",
    "strncat",
    "strncmp",
    "strncpy",
    "strndup",
    "strnlen",
    "strpbrk",
    "strrchr",
    "strspn",
    "strstr",
    "strtok",
    "strtok_r",
    "bcmp",
    "bcopy",
    "index",
    "rindex",
    "abort",
    // Off Darwin these go through `crate::malloc`'s zones; on it, the host's
    // own libsystem_malloc is the right answer.
    #[cfg(target_os = "macos")]
    "calloc",
    "div",
    #[cfg(target_os = "macos")]
    "free",
    #[cfg(target_os = "macos")]
    "malloc",
    "rand",
    #[cfg(target_os = "macos")]
    "realloc",
    "srand",
    "strtol",
    "strtoll",
    "strtoul",
    "strtoull",
    "isalnum",
    "isalpha",
    "isascii",
    "isblank",
    "iscntrl",
    "isdigit",
    "isgraph",
    "islower",
    "isprint",
    "ispunct",
    "isspace",
    "isupper",
    "isxdigit",
    "tolower",
    "toupper",
    "_exit",
    // Ends the process without running a handler, the same on every host.
    "_Exit",
    // Reports and aborts; it reads no guard value, so this runtime's own
    // `___stack_chk_guard` is not at odds with it.
    "__stack_chk_fail",
    "alarm",
    "fsync",
    "ftruncate",
    "isatty",
    "lseek",
    "pipe",
    "pread",
    "pwrite",
    "read",
    "sync",
    "acos",
    "asin",
    "atan",
    "atan2",
    "cbrt",
    "ceil",
    "cos",
    "cosh",
    "exp",
    "exp2",
    "expm1",
    "fabs",
    "floor",
    "fmax",
    "fmin",
    "fmod",
    "hypot",
    "log",
    "log10",
    "log1p",
    "log2",
    "pow",
    "round",
    "sin",
    "sinh",
    "sqrt",
    "tan",
    "tanh",
    "trunc",
    "difftime",
    "mktime",
    "time",
    "mlock",
    "munlock",
    "mincore",
];

pub fn claimed(name: &str) -> bool {
    let name = crate::c_name(name);
    CLAIMED.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_has_no_duplicates() {
        let mut sorted = CLAIMED.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "a name is claimed twice");
    }
}
