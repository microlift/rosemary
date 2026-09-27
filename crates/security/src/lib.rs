//! The parts of Apple's Security framework that Chromium Framework imports.
//!
//! The organising rule, and the reason the modules are split the way they are:
//!
//! - **A fact about this host is implemented.** There is no keychain, no
//!   code-signing database, no Secure Enclave, no anchor store, no
//!   Authorization Services and no signed Mach-O. Every one of those is an
//!   *absence*, and an absence has an honest answer - a null, an empty, an
//!   `errSecItemNotFound`, an `errSecCSUnsigned` - which is a real result and not
//!   a failure to be papered over. `SecTaskCopyValueForEntitlement` answering
//!   null for an entitlement this process does not have is the model, and it is
//!   the same answer for every one of them.
//! - **A decision is refused, by name.** Trust evaluation, code-signing
//!   validity, key generation, signing, privilege escalation, writing an ACL: each
//!   of these would be a claim about the world, made without checking the world.
//!   They are bound anyway - a bound function returning a refusal beats an
//!   unbound import, which is a jump through a zero slot - and each refusal names
//!   itself on stderr, once, through [`status::refuse`].
//!
//! Nothing in this crate ever returns a plausible stand-in. Where a function has
//! no error return and therefore cannot refuse in its result, that is said in its
//! own documentation, and the answer is the object's real contents rather than
//! something derived from them.

pub mod access;
pub mod authorization;
pub mod certificate;
pub mod codesign;
pub mod identity;
pub mod key;
pub mod keychain;
pub mod keychain_legacy;
pub mod status;
pub mod task;
pub mod trust;

exports::group! {
    access,
    authorization,
    certificate,
    codesign,
    identity,
    key,
    keychain,
    keychain_legacy,
    task,
    trust,
}

/// The `kSec*` key globals, each a cell holding an `NSString *`.
pub fn data_symbols() -> Vec<(String, u64)> {
    foundation::bootstrap();
    let mut symbols: Vec<_> = keychain::KEYS
        .iter()
        .map(|(symbol, text)| {
            (
                (*symbol).to_string(),
                corefoundation::base::global_cell(
                    symbol,
                    foundation::notification::name_object(text),
                ),
            )
        })
        .collect();
    // `const SecRandomRef kSecRandomDefault = NULL`: a word the guest reads.
    let random = "_kSecRandomDefault";
    symbols.push((
        random.to_string(),
        corefoundation::base::global_cell(random, 0),
    ));
    symbols
}
