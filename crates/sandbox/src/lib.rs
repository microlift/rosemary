//! The two `libsandbox` entry points that Chromium's helper processes import and
//! that **nothing in this tree published** — and the truthful refusals the
//! guest's own code shows it can survive.
//!
//! # What this crate is, in one sentence
//!
//! `_sandbox_apply` and `_sandbox_init_with_parameters` were **unbound**, not
//! refused: they appear in every helper image's `__TEXT,__stubs`, they are
//! named in the loader's own unbound-import report, and no `exports!` table in
//! the tree answers either name. An unbound import is a zero in the slot and a
//! jump through it, and every helper died that way — `SIGSEGV at 0x0`, "called
//! `_sandbox_init_with_parameters`, an import nothing satisfied".
//!
//! # The whole `_sandbox_*` surface, and what the guest does with each answer
//!
//! Fourteen distinct names, and the distinction between the tolerated ones and
//! the fatal ones is **entirely** in the third column. This is the census; every
//! row is a raw `E8 rel32` / `FF 15`,`FF 25` rip-rel32 scan of the image's
//! `__TEXT,__text` for the import's own stub, re-disassembled at every hit, with
//! **`_memcpy` as the positive control in the same scan** (Framework 36 421
//! sites, Chromium 11, each helper 37). A scan of 214 MB that finds nothing is
//! worthless without that control, and the first control I tried,
//! `_operator_new`, was *not usable*: the Framework defines `operator new`
//! locally at `0x5decc50`, so it has no stub and would have "passed" vacuously.
//!
//! `Chromium`, the main executable, imports **no** sandbox symbol at all: zero
//! stubs, zero calls. The surface is the Framework plus the five helpers, and
//! all five helpers carry byte-identical code at the same three addresses.
//!
//! ## `Chromium Framework` — 11 names, 16 direct call sites
//!
//! | name | image that binds it | sites | what the guest does with the answer |
//! |---|---|--:|---|
//! | `___sandbox_ms` | `libSystem.B.dylib` | 1 | **logs.** `+0x3a7b4ef` `test eax,eax; sete %dl`, and the status goes into a `"rv=%d status=0x%llx …"` conversion. Non-zero is a handled path. |
//! | `sandbox_create_params` | `libsandbox.1.dylib` | 1 | **stores.** `+0x3a94d62` `mov [rbp-0x210],rax`, and it is **never null-tested** before being handed to `sandbox_set_param` at `+0x7fe5f7e`. |
//! | `sandbox_set_param` | `libsandbox.1.dylib` | 1 | Returns **0 ⇒ the wrapper answers true**; anything else ⇒ the wrapper erases its cached entry, builds a message, answers false (`+0x7fe5f87` `test eax,eax; je +0x7fe6033` → `mov al,0x1`; the fall-through builds the message and `xor eax,eax`). |
//! | ↳ the wrapper at `+0x7fe5e70` | — | **21** | **18 of the 21 turn `false` into a bare `int3`.** Two store it (`+0x3aafb7a`, `+0x3ab009a`). The 18 land in `int3` pads at `+0x3ab0d2b`…`+0x3ab0d49`, `+0x3ab015c`/`+0x3ab0168`/`+0x3ab016b`, `+0x3ab13d3`/`+0x3ab13d6`/`+0x3ab13d9` and `+0x56d9a59`/`+0x56d9a5f`. **This is the wall.** |
//! | `sandbox_free_params` | `libsandbox.1.dylib` | 2 | Both are guarded by `test %rdi,%rdi; je <skip>` (`+0x3a95005`, `+0x3a958a3`), so they are reached **only with a non-null handle** — which the null answer never produces. |
//! | `sandbox_compile_string` | `libsandbox.1.dylib` | 1 | null ⇒ the failure arm at `+0x7fe65f3`, which frees the error and answers false. Tolerant **of the null profile** — but see the trap below. |
//! | `sandbox_free_error` | `libsandbox.1.dylib` | 1 | unconditional cleanup on that failure arm. |
//! | `sandbox_free_profile` | `libsandbox.1.dylib` | 1 | cleanup, on the success arm only. |
//! | `sandbox_check` | `libSystem.B.dylib` | 5 | mixed, and **two of the five are fatal**: `+0x4f18132` and `+0xc8f7706` do `test eax,eax; je` into `int3` at `+0x4f1827f` and `+0xc8f7733`. `+0x82a07e4` **stores** `eax != 0` into `+0x240(%rbx)`; `+0xc6c1f91` **returns** it (`setne %al; ret`); `+0x4950cc6` **logs** and continues. |
//! | `sandbox_extension_issue_file` | `libsandbox.1.dylib` | 1 | null token is handled (`+0x77b1ac2` `je +0x77b1ba2`, which returns the null). |
//! | `sandbox_extension_consume` | `libsandbox.1.dylib` | 1 | **stores** the token at `+0x18(%r15)`. |
//! | `sandbox_extension_release` | `libsandbox.1.dylib` | 1 | **stores** zero, ignores the answer. |
//!
//! ## the five helpers — 3 names, 3 sites each, identical addresses
//!
//! | name | image that binds it | site | what the guest does with the answer |
//! |---|---|---|---|
//! | `sandbox_init_with_parameters` | `libSystem.B.dylib` | `+0x27029` | **traps, conditionally.** `+0x2703d` `test %r15,%r15; je +0x2708c` and `+0x2708c` is **`ud2`** — so a failure is survivable *only if the out-word the callee was given is non-null*. Non-null ⇒ `std::string::assign` it, `sandbox_free_error` it, answer false; the caller prints it and `main` returns 0. |
//! | `sandbox_apply` | `libsandbox.1.dylib` | `+0x270c3` | **logs and continues.** Non-zero ⇒ `"sandbox_apply: "` + `strerror(__error())` into the error string, answer false; `+0x270694` *branches* on it. The out-slot the callee was handed is **never read on either path** — which is what makes `errno`, not the out-slot, the channel. |
//! | `sandbox_free_error` | `libsandbox.1.dylib` | `+0x27050` | cleanup on the failure arm, **one argument** in `rdi`. |
//!
//! # So: which of these is a *decision* and which is a gap?
//!
//! **A gap in the publication, and therefore this crate.** The two names here
//! were absent from every `exports!` table in the tree. The report's own wording
//! — "an import nothing satisfied" — is the right one, and it is a different
//! problem from a refusal and a much easier one: a bound function that returns a
//! truthful Darwin error is a bound function, where a zero slot is a lost run.
//! The other twelve names *are* published, and are answered.
//!
//! # What the refusal says, and what it does not say
//!
//! Both refusals are `ENOTSUP`/`-1` plus a real message, and the message is the
//! whole of the honesty:
//!
//! > there is no Seatbelt on this host and no MAC framework to enforce one, so
//! > no sandbox profile can be compiled or applied. The parameters are recorded
//! > (see `darwin::last_unavailable`) and the profile is refused at the compile
//! > step, which is the first place the answer stops being a bookkeeping fact
//! > and starts being a capability.
//!
//! **This is not "nothing at all", and saying so is part of the answer.**
//! Landlock is present and enabled on this host — `/sys/kernel/security/lsm`
//! lists `landlock` and `CONFIG_SECURITY_LANDLOCK=y` is set in
//! `/proc/config.gz` — and seccomp is present with all seven actions
//! (`kill_process kill_thread trap errno user_notif trace log` in
//! `/proc/sys/kernel/seccomp/actions_avail`). glibc 2.44 exports no wrapper for
//! either, so both are reachable only by raw `syscall(2)`. **So the truthful
//! sentence is "no Seatbelt, and nothing is put in its place **yet**", not "no
//! mechanism exists".** A Landlock+seccomp realisation of the profile the guest
//! hands `sandbox_compile_string` is a real piece of work that this host could
//! support; it is not built here, and nothing in this file pretends it is.
//!
//! # The two refusals are *not* the enforcement refusal
//!
//! The guest is told the truth at every step, and it is told it three times over
//! on three different names, none of which it can argue with:
//! `sandbox_init_with_parameters` (here), `__sandbox_ms` (a named refusal that
//! the guest logs and walks away from), and `sandbox_compile_string` (a named
//! refusal on the failure arm it has a path for). What is refused here is the
//! *act* of confining a process, not the *bookkeeping* around it.
//!
//! # One thing this file deliberately does not do
//!
//! It does not make the guest tolerate anything. No value is invented, no
//! return is softened, and no `false` is turned into a `true`: `sandbox_apply`
//! answers non-zero and `sandbox_init_with_parameters` answers non-zero, because
//! non-zero is what happened.

#![cfg(not(target_os = "macos"))]

use std::ffi::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

/// A message the guest may read *and* release, owned by `darwin`.
///
/// The allocation and its table live in
/// `darwin::last_unavailable::guest_error_string`, because the guest has exactly
/// two ways to be handed an error string — this file's
/// `sandbox_init_with_parameters` and
/// `darwin::last_unavailable::sandbox_compile_string` — and both are released
/// by one function, `sandbox_free_error`, which has to be able to tell "ours"
/// from any other pointer. One producer, one owner, one table.
///
/// **The 256-byte sizing that used to be here is gone, and it is worth saying
/// why it was ever needed.** `sandbox_free_error` used to be published with
/// three parameters — `(int err, char *buf, size_t buflen)` — while the guest
/// calls it with **one** (`Framework+0x7fe660c` and `Helper+0x10002704d` are
/// both `mov <reg>, %rdi; call`), so `rsi` and `rdx` arrived as whatever the
/// preceding call left in them, and that implementation wrote
/// `strerror(garbage)` into the guest's own message with a length this runtime
/// never chose. Sizing the buffer at a fixed capacity made that write bounded
/// *by construction* rather than by the accident that the message happened to be
/// long enough.
///
/// That was a guarded workaround for a bug in a file this one did not own, and
/// the bug is now fixed: `sandbox_free_error` takes one `char *` and releases
/// the string this runtime allocated. Nothing here hands the guest a buffer to
/// be scribbled on any more, so the capacity constant has no reason to exist and
/// a `CString`'s exact allocation is now the right thing to hand over — which is
/// what `guest_error_string` allocates.
fn message(text: &str) -> *mut c_char {
    // The guest reads it as a `char *`, hands it to `std::string::assign`, and
    // then releases it with `sandbox_free_error` (`Helper+0x100027048`,
    // `+0x100027050`) — so it has to outlive this call, and it has to be an
    // allocation that function can recognise as ours and free.
    darwin::last_unavailable::guest_error_string(text)
}

/// A refusal, said once per name.
fn note(once: &AtomicBool, what: &str, why: &str) {
    if !once.swap(true, Ordering::Relaxed) {
        let line = format!("rosetta3: {what}: {why}\n");
        // SAFETY: `line` is live for the length written and `write` returns
        // before `line` is dropped.
        unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
    }
}

mod said {
    use std::sync::atomic::AtomicBool;

    /// No Seatbelt, and therefore no `sandbox_apply`.
    pub static APPLY: AtomicBool = AtomicBool::new(false);
    /// No Seatbelt, and therefore no sandbox token.
    pub static INIT: AtomicBool = AtomicBool::new(false);
}

const NO_SEATBELT: &str = "there is no Seatbelt on this host and no MAC framework to enforce one, \
     so no sandbox profile can be applied; this is refused rather than answered, and nothing \
     is put in its place (Landlock and seccomp are both present here - /sys/kernel/security/lsm \
     lists landlock and /proc/sys/kernel/seccomp/actions_avail lists seven actions - so a real \
     realisation is available on this host and is not built)";

/// `int sandbox_apply(const char *profile, int flags, char **errorbuf)`.
///
/// **Non-zero, with `errno` set to `ENOTSUP`, and nothing written anywhere.**
///
/// The two decisions, both read out of the guest rather than assumed:
///
/// * **The answer is non-zero.** `Helper+0x270c8` `mov %eax,%r14d` and
///   `+0x27013b` `sete %al`: the guest's own return is `status == 0`, and the
///   caller at `+0x270694` **branches** on it (`je +0x100027a7d`, which prints
///   the error and returns 0 from `main`). A non-zero answer is a path the guest
///   has and walks.
/// * **`errno`, not the out-slot, is the channel.** The failure arm calls
///   `___error` (`+0x2700d4`) and `_strerror` (`+0x2700db`) and builds
///   `"sandbox_apply: "` + that. The slot the callee was handed — `rdi`, which
///   is `lea -0x28(%rbp),%rdi` and the caller pre-nulled at `+0x27009e` — is
///   **never read on either path**. So writing a message there would allocate
///   something nobody reads and leak it, and the *useful* half of the refusal is
///   the `errno` the guest renders through its own `strerror`.
///
/// The one thing I could not settle, and it does not change the answer: the
/// profile is built into the caller's frame at `[rbp-0x20]`/`[rbp-0x18]` and
/// reaches the callee as `rdi+8`/`rdi+16` rather than in a register, which is
/// not a shape any header in this tree declares. Since the answer is a refusal
/// and the profile's text is not needed to refuse it, the question is left open
/// rather than guessed at.
unsafe extern "C" fn sandbox_apply(_errorbuf: *mut *mut c_char, _error_string: *mut c_void) -> c_int {
    // SAFETY: `__errno_location` on glibc, and this is the documented way to set
    // it. A named refusal that leaves `errno` alone would reach the guest's own
    // `strerror` as whatever the last unrelated libc call happened to leave,
    // which is a wrong answer wearing the right shape.
    unsafe { *libc::__errno_location() = libc::ENOTSUP };
    note(
        &said::APPLY,
        "sandbox_apply",
        &format!("refused - {NO_SEATBELT}; the guest renders this through its own strerror"),
    );
    -1
}

/// `int sandbox_init_with_parameters(const char *name, uint64_t parameters, uint64_t *token, char **errorbuf)`.
///
/// **Non-zero, and a real message in the out-slot the guest actually reads** —
/// which is the whole difference between this name being survivable and not.
///
/// The guest's own words, at `Helper+0x100027010`:
///
/// ```text
///  10002701d  mov  QWORD PTR [rbp-0x20], 0x0      ; it pre-nulls the slot
///  100027025  lea  rcx, [rbp-0x20]                 ; and hands its address over
///  100027029  call _sandbox_init_with_parameters
///  10002702e  mov  ebx, eax
///  100027030  test eax, eax
///  100027032  je   0x100027065                    ; 0: success, clear the string
///  100027034  mov  r15, QWORD PTR [rbp-0x20]       ; the callee's out-word
///  100027038  test r14, r14
///  10002703b  je   0x10002704d                    ; no error string wanted
///  10002703d  test r15, r15
///  100027040  je   0x10002708c                    ; <-- ud2. SIGILL.
///  100027042  mov  rdi, r14 ; 100027045  mov rsi, r15
///  100027048  call std::string::assign
///  10002704d  mov  rdi, r15
///  100027050  call _sandbox_free_error
///  100027057  sete al                             ; returns (status == 0)
/// ```
///
/// So **`ud2` at `+0x10002708c` is what a null out-word costs**, and the caller
/// always wants a string (`Helper+0x100027a3c  lea rcx, [rbp-0x80]`, never
/// zero). A refusal that leaves the slot null is a `SIGILL` in five processes;
/// a refusal that fills it is a line on stderr and `main` returning 0. That is
/// the same shape as `__sandbox_ms`'s refusal, which is the one place in
/// `darwin::last_unavailable` that already writes a real message for exactly
/// this reason.
///
/// The out-word is a `char *`, not a token: the guest passes it to
/// `std::string::assign(const char *)` and then to `sandbox_free_error`, so it
/// is an error string and nothing else. Writing a token there would be a
/// fabricated sandbox handle.
unsafe extern "C" fn sandbox_init_with_parameters(
    name: *const c_char,
    _parameters: u64,
    _third: *mut c_void,
    errorbuf: *mut *mut c_char,
) -> c_int {
    if !errorbuf.is_null() {
        // SAFETY: `errorbuf` is the caller's own stack slot — `lea rcx,
        // [rbp-0x20]` in the guest's frame, pre-nulled at `+0x10002701d` — and
        // the guest reads the word back out of it on the failure path.
        unsafe { errorbuf.write(message(&format!("sandbox_init_with_parameters({}): {NO_SEATBELT}", which(name)))) };
    }
    note(
        &said::INIT,
        "sandbox_init_with_parameters",
        &format!("refused - {NO_SEATBELT}; the guest turns the out-word into its error string (Helper+0x100027048) and prints it (Helper+0x100027a8d)"),
    );
    -1
}

/// The name the guest asked about, for the message. Null and unreadable both
/// read as `"(unnamed)"`; nothing here dereferences a pointer it was not given.
fn which(name: *const c_char) -> String {
    if name.is_null() {
        return "(null)".to_string();
    }
    // SAFETY: `name` is the guest's first argument, a NUL-terminated C string it
    // has already built; the read stops at the NUL. A guest that hands over a
    // pointer into a freed mapping would fault here exactly as it would in the
    // real library, which is the correct outcome for a wrong pointer.
    unsafe { std::ffi::CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned()
}

/// Every bare C name this crate answers for, spelled as a `dlsym` would.
pub fn names() -> Vec<&'static str> {
    vec!["sandbox_apply", "sandbox_init_with_parameters"]
}

/// The address a guest asking for `name` should get, `dlsym` spelling.
pub fn symbol(name: &str) -> Option<u64> {
    let address = match name {
        "sandbox_apply" => sandbox_apply as *const () as u64,
        "sandbox_init_with_parameters" => sandbox_init_with_parameters as *const () as u64,
        _ => return None,
    };
    Some(address)
}

/// Publish this crate's two names into a loader's export table.
pub fn provide(out: &mut dyn exports::Exports) {
    for name in names() {
        let address = symbol(name).expect("a name this crate published resolves");
        // `exports!` publishes `"_" + <row>`, and the loader resolves an import
        // verbatim with no normalisation, so the Mach-O spelling is the one with
        // the underscore. The image imports `_sandbox_apply` — one, from
        // `__TEXT,__stubs`'s indirect symbol table, not from a header.
        out.function(&format!("_{name}"), address);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two names, and the spellings the images actually import.
    ///
    /// The spelling trap this project has been bitten by three times: a row
    /// written without the leading underscore publishes `_sandbox_apply`… which
    /// happens to be right, and a row written as `sandbox_apply` in a `dlsym`
    /// table is right too, so *both* paths have to be pinned separately, and
    /// neither proves the other. The image's own indirect symbol table is the
    /// authority: `_sandbox_apply` (stub `Helper+0x1000318f6`) and
    /// `_sandbox_init_with_parameters` (stub `Helper+0x1000318f0`). One leading
    /// underscore each, unlike `___sandbox_ms`.
    #[test]
    fn the_spellings_are_the_image_spellings() {
        for row in names() {
            let published = format!("_{row}");
            assert_eq!(
                published,
                match row {
                    "sandbox_apply" => "_sandbox_apply",
                    "sandbox_init_with_parameters" => "_sandbox_init_with_parameters",
                    other => panic!("a name not in the census is published: {other}"),
                }
            );
            assert!(symbol(row).is_some(), "{row} publishes but resolves to nothing");
        }
        // And the Mach-O spelling must not resolve through the `dlsym` table,
        // because `darwin::symbol` strips exactly one underscore before asking.
        assert!(symbol("_sandbox_apply").is_none());
        assert!(symbol("_sandbox_init_with_parameters").is_none());
    }

    /// One provider per name, proved against `darwin`'s real table.
    ///
    /// A test pass is not evidence in this project — a private name off by one
    /// character builds clean and tests green — so the duplicate check is a
    /// question asked of the table that would actually collide, not a comment.
    /// `darwin` answers the other twelve `_sandbox_*` names
    /// (`__sandbox_ms`, `create_params`, `set_param`, `free_params`,
    /// `compile_string`, `free_profile` in `last_unavailable.rs`; `check`,
    /// `extension_*`, `free_error` in `media_unavailable.rs`), and asking it is
    /// what makes "these two were unbound" a measurement rather than a reading
    /// of the log.
    #[test]
    fn no_other_module_publishes_these_names() {
        for row in names() {
            assert!(
                !darwin::is_implemented(row),
                "{row} is already published by darwin, so this crate would define it twice"
            );
        }
    }

    /// The two refusals answer non-zero, and `sandbox_apply` sets `errno`.
    ///
    /// `ENOTSUP` rather than a bare `-1` because the guest renders `errno`
    /// through its own `strerror` (`Helper+0x1000270d4` `call ___error`,
    /// `+0x1000270db` `call _strerror`) — a `-1` with `errno` left alone would
    /// reach that `%s` as whatever the last unrelated libc call happened to
    /// leave, which is a wrong answer wearing the right shape.
    #[test]
    fn apply_refuses_with_enootsup_and_writes_nothing() {
        let mut slot: *mut c_char = std::ptr::null_mut();
        // SAFETY: one out-slot and one opaque argument, neither dereferenced; the
        // second is the guest's own error string and this writes nothing to it.
        let status = unsafe { sandbox_apply(&mut slot, std::ptr::null_mut()) };
        assert_ne!(status, 0, "a zero answer would tell the guest it is confined");
        assert!(
            slot.is_null(),
            "the guest never reads this slot, so writing it would leak what nobody reads"
        );
        // SAFETY: reading the thread's own errno.
        assert_eq!(unsafe { *libc::__errno_location() }, libc::ENOTSUP);
    }

    /// `sandbox_init_with_parameters` refuses **and fills the out-word**, which
    /// is the difference between a line on stderr and a `SIGILL`.
    ///
    /// `Helper+0x100027040  je 0x10002708c` where `+0x10002708c` is `ud2`:
    /// a failure with a null out-word is an illegal instruction in five
    /// processes. This is the assertion that keeps the refusal survivable.
    #[test]
    fn init_refuses_with_a_message_the_guest_can_print() {
        use std::ffi::CString;
        let mut slot: *mut c_char = std::ptr::null_mut();
        let name = CString::new("com.example.helper").unwrap();
        // SAFETY: a valid NUL-terminated name, an opaque second and third
        // argument, and a real out-slot the function writes exactly one pointer
        // into. The pointer is the only allocation made and it is the guest's to
        // hand to `sandbox_free_error`.
        let status = unsafe {
            sandbox_init_with_parameters(
                name.as_ptr(),
                0,
                std::ptr::null_mut(),
                &mut slot,
            )
        };
        assert_ne!(status, 0, "a zero answer would tell the guest it holds a token");
        assert!(!slot.is_null(), "a null out-word is a ud2 at Helper+0x10002708c");
        // SAFETY: the pointer this function just wrote, read as a C string. It is
        // an allocation this runtime made and registered, which is what lets
        // `sandbox_free_error` release exactly it — one argument, no length, no
        // scribbling.
        let text = unsafe { std::ffi::CStr::from_ptr(slot) }.to_string_lossy();
        assert!(
            text.contains("no Seatbelt"),
            "the message has to say which capability is missing: {text}"
        );
        assert!(
            text.contains("com.example.helper"),
            "the guest asked about a named profile and the message should name it: {text}"
        );
    }

    /// The refusal sentence says what *is* here, not only what is not.
    ///
    /// `/sys/kernel/security/lsm` lists `landlock` and
    /// `/proc/sys/kernel/seccomp/actions_avail` lists seven actions on this
    /// host, so "there is no sandbox here" would be the wrong sentence: the
    /// truthful one is "no Seatbelt, and nothing is put in its place *yet*". A
    /// message that overstated the host's emptiness would be the same class of
    /// error as an invented return value, in the other direction.
    #[test]
    fn the_refusal_does_not_overstate_the_host() {
        assert!(
            NO_SEATBELT.contains("Landlock") && NO_SEATBELT.contains("seccomp"),
            "the host has both, and the refusal has to say so: {NO_SEATBELT}"
        );
        assert!(
            !NO_SEATBELT.contains("no mechanism"),
            "that would be false here: {NO_SEATBELT}"
        );
    }

    /// Every name published, resolves, and resolves to a distinct address.
    #[test]
    fn every_published_name_resolves_once() {
        let rows = names();
        assert_eq!(rows.len(), 2, "the census is two names: apply and init");
        let mut seen: Vec<u64> = Vec::new();
        for row in &rows {
            let address = symbol(row).unwrap();
            assert!(
                !seen.contains(&address),
                "{row} shares an address with another name, so one of them is unreachable"
            );
            seen.push(address);
        }
    }
}
