use std::sync::atomic::{AtomicU64, Ordering};

use static_init::dynamic;

use crate::abi::table::FunctionAbi;

use super::dispatch::{RUNTIME, Runtime, host_symbol};

/// A runtime entry point as an inlined native call.
pub use crate::abi::Promotion;

/// `name` as an inlined native call, if it is one.
pub fn promotion(name: &str, resolved: u64) -> Option<Promotion> {
    if objc::debug::enabled() {
        return None;
    }
    let name = name.strip_prefix('_').unwrap_or(name);
    let i = RUNTIME.binary_search_by_key(&name, |r| r.name).ok()?;
    let entry = &RUNTIME[i];

    // The address the shim would have called. Compared rather than assumed:
    // promotion is only equivalence if the two are the same function.
    let host = host_symbol(name)?;
    if resolved != host {
        return None;
    }

    // The condition that decides scope, asked before anything else about the
    // row: an entry that can reach guest code may not be inlined at all.
    if NEVER_DISPATCHES.binary_search(&name).is_err() {
        return None;
    }

    let address = match through(name) {
        Some(t) if t.matches(entry) => t.call,
        // A substituting entry whose declared mask disagrees with its row is a
        // bug in this file, caught by a test.
        Some(_) => return None,
        None if is_identity(entry) => host,
        None => return None,
    };
    Some(Promotion {
        abi: abi_for(i),
        address,
    })
}

/// The signature of a promotable entry, for a caller that only wants to classify it.
///
/// [`crate::abi::classify`] asks this so the audit reports how an import will really be
/// reached; `promotion` is what actually decides, because only it knows the resolved
/// address.
pub fn abi_of(name: &str) -> Option<&'static FunctionAbi> {
    let name = name.strip_prefix('_').unwrap_or(name);
    let i = RUNTIME.binary_search_by_key(&name, |r| r.name).ok()?;
    let entry = &RUNTIME[i];
    if NEVER_DISPATCHES.binary_search(&name).is_err() {
        return None;
    }
    let promotable = match through(name) {
        Some(t) => t.matches(entry),
        None => is_identity(entry),
    };
    promotable.then(|| abi_for(i))
}

/// The entry points that **cannot cause the host runtime to call an `IMP`**.
static NEVER_DISPATCHES: &[&str] = &[
    "class_getInstanceSize",
    "class_getInstanceVariable",
    "class_getName",
    "class_getSuperclass",
    "class_isMetaClass",
    "ivar_getName",
    "ivar_getOffset",
    "objc_allocateClassPair",
    "objc_autoreleasePoolPush",
    "objc_copyStruct",
    "objc_copyWeak",
    "objc_destroyWeak",
    "objc_getClass",
    "objc_getMetaClass",
    "objc_getProtocol",
    "objc_getRequiredClass",
    "objc_lookUpClass",
    "objc_moveWeak",
    "objc_opt_self",
    "objc_registerClassPair",
    "objc_sync_enter",
    "objc_sync_exit",
    "object_getClass",
    "object_getClassName",
    "object_getIndexedIvars",
    "object_getIvar",
    "property_copyAttributeList",
    "sel_getName",
    "sel_getUid",
    "sel_isEqual",
    "sel_registerName",
];

/// Whether the shim for `entry` is the identity, so the host's own address can be
/// inlined directly.
fn is_identity(entry: &Runtime) -> bool {
    entry.classes.is_empty()
        && entry.protocols.is_empty()
        && entry.callbacks.is_empty()
        && entry.blocks.is_empty()
        && !entry.class_return
        && entry.struct_return.is_none()
}

/// The [`FunctionAbi`] for `RUNTIME[i]`, built once.
fn abi_for(i: usize) -> &'static FunctionAbi {
    #[dynamic]
    static ABIS: Vec<FunctionAbi> = {
        RUNTIME
            .iter()
            .map(|r| FunctionAbi {
                name: r.name,
                types: r.types,
                variadic: false,
                callbacks: &[],
                reenters_guest: false,
                reads_caller: false,
            })
            .collect()
    };
    &ABIS[i]
}

/// A substituting function, and the substitution it performs.
struct Through {
    name: &'static str,
    /// The function to inline a call to. Its signature is the entry's own.
    call: u64,
    /// One entry per parameter, in order: whether this function translates it as a
    /// class.
    classes: &'static [bool],
    /// Whether it translates the return value back.
    class_return: bool,
}

impl Through {
    /// Whether this function really implements `entry`'s substitution.
    fn matches(&self, entry: &Runtime) -> bool {
        entry.protocols.is_empty()
            && entry.callbacks.is_empty()
            && entry.blocks.is_empty()
            && entry.struct_return.is_none()
            && entry.class_return == self.class_return
            && self.classes.len() + 1 == entry.types.len()
            && self
                .classes
                .iter()
                .enumerate()
                .all(|(i, &is)| is == entry.classes.contains(&(i as u8)))
    }
}

/// A class pointer on its way out, for a caller with no [`crate::vm::Vm`].
///
/// [`crate::vm::Vm::objc_host_class`] with the VM looked up from the process rather
/// than from a register context - see [`super::published`].
fn host_class(va: u64) -> u64 {
    match super::published() {
        Some(rt) => rt.host_or(va),
        None => va,
    }
}

/// A class pointer on its way back in. [`host_class`]'s reverse.
fn guest_class(host: u64) -> u64 {
    match super::published() {
        Some(rt) => rt.guest_or(host),
        None => host,
    }
}

/// The host function `name` names, cached.
fn host_of(cache: &AtomicU64, name: &str) -> u64 {
    let cached = cache.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let Some(addr) = host_symbol(name) else {
        eprintln!("rosemary: fatal: the host has no {name}, which was promoted anyway");
        std::process::abort();
    };
    cache.store(addr, Ordering::Relaxed);
    addr
}

/// `class` translates the argument; `plain` passes it through.
macro_rules! substitute {
    (class, $v:expr) => {
        host_class($v)
    };
    (plain, $v:expr) => {
        $v
    };
}

/// The same, for a return value, which travels the other way.
macro_rules! back {
    (class, $v:expr) => {
        guest_class($v)
    };
    (plain, $v:expr) => {
        $v
    };
}

/// Whether a keyword names a translated position, as a `bool` for the mask.
macro_rules! is_class {
    (class) => {
        true
    };
    (plain) => {
        false
    };
}

/// Define a substituting function and its table entry.
///
/// Each parameter is written `class` or `plain`, which is both the code that runs and
/// the mask [`Through::matches`] holds against the `RUNTIME` row - so the declaration
/// cannot drift from the behaviour, and neither can drift from the table.
macro_rules! through {
    ($( $f:ident, $host:literal, ( $($k:ident $a:ident : $t:ty),* ) -> $rk:ident $r:ty ; )*) => {
        $(
            unsafe extern "C" fn $f($($a: $t),*) -> $r {
                static HOST: AtomicU64 = AtomicU64::new(0);
                // SAFETY: `host_of` answers with the address `dlsym` gave for
                // this exact C name, and the prototype below is the row's own
                // signature - the same one `marshal::call_types` would have
                // placed arguments for. Aborting rather than returning zero is
                // what makes the transmute's precondition hold.
                let f: unsafe extern "C" fn($($t),*) -> $r =
                    unsafe { core::mem::transmute(host_of(&HOST, $host) as *const ()) };
                back!($rk, unsafe { f($(substitute!($k, $a)),*) })
            }
        )*

        /// Every entry whose shim is a substitution, and the function that performs it.
        /// **Sorted by name**, as `RUNTIME` is.
        fn through_table() -> &'static [Through] {
            static THROUGH: std::sync::OnceLock<Vec<Through>> = std::sync::OnceLock::new();
            THROUGH.get_or_init(|| vec![$(
                Through {
                    name: $host,
                    call: $f as *const () as u64,
                    classes: &[$(is_class!($k)),*],
                    class_return: is_class!($rk),
                },
            )*])
        }

        /// The entry for `name`, by binary search over [`through_table`].
        fn through(name: &str) -> Option<&'static Through> {
            let table = through_table();
            table.binary_search_by_key(&name, |t| t.name).ok().map(|i| &table[i])
        }
    };
}

// The substituting half of the table, in `RUNTIME`'s order. Read each line
// against the row it stands for.
through! {
    class_get_instance_size, "class_getInstanceSize", (class cls: u64) -> plain u64;
    class_get_instance_variable, "class_getInstanceVariable", (class cls: u64, plain name: u64) -> plain u64;
    class_get_name, "class_getName", (class cls: u64) -> plain u64;
    class_get_superclass, "class_getSuperclass", (class cls: u64) -> class u64;
    class_is_meta_class, "class_isMetaClass", (class cls: u64) -> plain u8;
    objc_allocate_class_pair, "objc_allocateClassPair", (class sup: u64, plain name: u64, plain extra: u64) -> class u64;
    objc_get_class, "objc_getClass", (class name: u64) -> class u64;
    objc_get_meta_class, "objc_getMetaClass", (plain name: u64) -> class u64;
    objc_get_required_class, "objc_getRequiredClass", (plain name: u64) -> class u64;
    objc_look_up_class, "objc_lookUpClass", (plain name: u64) -> class u64;
    objc_opt_self, "objc_opt_self", (class obj: u64) -> plain u64;
    objc_register_class_pair, "objc_registerClassPair", (class cls: u64) -> plain ();
    objc_sync_enter, "objc_sync_enter", (class obj: u64) -> plain i32;
    objc_sync_exit, "objc_sync_exit", (class obj: u64) -> plain i32;
    object_get_class, "object_getClass", (class obj: u64) -> class u64;
    object_get_class_name, "object_getClassName", (class obj: u64) -> plain u64;
    object_get_ivar, "object_getIvar", (class obj: u64, plain ivar: u64) -> class u64;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two tables are one table.
    #[test]
    fn every_through_entry_matches_its_table_row() {
        for t in through_table() {
            let name = t.name;
            let i = RUNTIME
                .binary_search_by_key(&name, |r| r.name)
                .unwrap_or_else(|_| panic!("{name} names no RUNTIME entry"));
            assert!(t.matches(&RUNTIME[i]), "{name} disagrees with its row");
        }
    }

    /// And every substituting function is a distinct, non-null address.
    #[test]
    fn every_through_entry_has_its_own_function() {
        let mut seen: Vec<u64> = through_table().iter().map(|t| t.call).collect();
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), total, "two entries share a function");
        assert!(!seen.contains(&0), "an entry has a null function");
    }

    /// The through table is sorted, because the lookup is a binary search, and
    /// every name in it is findable through that search.
    #[test]
    fn the_through_table_is_sorted_and_searchable() {
        let mut previous = "";
        for t in through_table() {
            let name = t.name;
            assert!(previous <= name, "{previous} then {name}");
            previous = name;
            assert!(through(name).is_some(), "{name} is not findable");
        }
    }

    /// The two populations partition the promotable table.
    #[test]
    fn the_two_populations_do_not_overlap() {
        for entry in RUNTIME {
            let name = entry.name;
            if is_identity(entry) {
                assert!(
                    through(name).is_none(),
                    "{name} is both a passthrough and a substitution"
                );
                // Promotable exactly when it also cannot dispatch.
                assert_eq!(
                    abi_of(name).is_some(),
                    NEVER_DISPATCHES.binary_search(&name).is_ok(),
                    "{name}"
                );
            }
        }
    }

    /// An entry with a callback, a block, a protocol or a struct return is never
    /// promoted, whatever else is true of it.
    #[test]
    fn the_entries_that_must_stay_shims_are_not_promotable() {
        for name in [
            "class_addMethod",
            "method_setImplementation",
            "objc_retainBlock",
            "class_conformsToProtocol",
            "protocol_getName",
            "protocol_getMethodDescription",
        ] {
            assert!(abi_of(name).is_none(), "{name} was promotable");
        }
    }

    /// A symbol that resolved somewhere other than the host's own function is
    /// not promoted. Equivalence is the claim, and it rests on the address.
    #[test]
    fn an_entry_that_resolved_elsewhere_is_not_promoted() {
        let host = host_symbol("class_getName").expect("this process has libobjc");
        assert!(promotion("_class_getName", host).is_some());
        assert!(promotion("_class_getName", host + 4).is_none());
        assert!(promotion("_class_getName", 0).is_none());
    }

    /// The signature promoted is the row's own, not a second copy of it.
    #[test]
    fn the_signature_comes_from_the_table() {
        let abi = abi_of("class_getName").expect("promotable");
        let i = RUNTIME
            .binary_search_by_key(&"class_getName", |r| r.name)
            .unwrap();
        assert_eq!(abi.types, RUNTIME[i].types);
        assert!(!abi.variadic);
        assert!(abi.callbacks.is_empty());
    }

    /// The message-send family is not in this table under any spelling: a send has no
    /// signature to inline with.
    #[test]
    fn no_message_send_is_promotable() {
        for name in [
            "objc_msgSend",
            "objc_msgSendSuper",
            "objc_msgSendSuper2",
            "objc_msgSend_stret",
            "objc_msgSendSuper2_stret",
        ] {
            assert!(abi_of(name).is_none(), "{name} was promotable");
            assert!(
                through(name).is_none(),
                "{name} has a substituting function"
            );
        }
    }

    /// A pure passthrough entry promotes to the host's own address, so the
    /// inlined call reaches exactly what the marshaller would have called.
    #[test]
    fn an_identity_entry_promotes_to_the_host_itself() {
        let host = host_symbol("objc_autoreleasePoolPush").expect("this process has libobjc");
        let p = promotion("_objc_autoreleasePoolPush", host).expect("promotable");
        assert_eq!(p.address, host);
    }

    /// And a substituting one does not: its address is this runtime's function,
    /// which is the whole mechanism.
    #[test]
    fn a_substituting_entry_promotes_to_this_runtimes_function() {
        let host = host_symbol("class_getName").expect("this process has libobjc");
        let p = promotion("_class_getName", host).expect("promotable");
        assert_ne!(p.address, host);
        assert_eq!(p.address, class_get_name as *const () as u64);
    }

    /// An entry that can make the host runtime call an `IMP` is not promoted, whatever
    /// its signature says.
    #[test]
    fn nothing_that_can_reach_guest_code_is_promoted() {
        for name in [
            // A release that reaches zero runs `dealloc`; a class with custom
            // retain/release makes even the retain a message.
            "objc_retain",
            "objc_release",
            "objc_autorelease",
            "objc_retainAutoreleasedReturnValue",
            "objc_storeStrong",
            "object_setIvar",
            // `+initialize`, `init`, `new`, and the guest's C++ ivar
            // constructors.
            "objc_alloc",
            "objc_alloc_init",
            "objc_opt_new",
            "class_createInstance",
            // Fast paths that fall back to a real message.
            "objc_opt_class",
            "objc_opt_isKindOfClass",
            "objc_opt_respondsToSelector",
            // Both trigger `+resolveInstanceMethod:`.
            "class_getInstanceMethod",
            "class_respondsToSelector",
            // Releases everything in the pool.
            "objc_autoreleasePoolPop",
            // May send `allowsWeakReference`, or retain.
            "objc_initWeak",
            "objc_storeWeak",
            "objc_loadWeakRetained",
        ] {
            assert!(abi_of(name).is_none(), "{name} was promotable");
            let host = host_symbol(name).unwrap_or(1);
            assert!(promotion(name, host).is_none(), "{name} was promoted");
        }
    }

    /// The allowlist is sorted, because the lookup is a binary search, and every
    /// name in it really names a row.
    #[test]
    fn the_allowlist_is_sorted_and_names_real_entries() {
        let mut previous = "";
        for &name in NEVER_DISPATCHES {
            assert!(previous <= name, "{previous} then {name}");
            previous = name;
            assert!(
                RUNTIME.binary_search_by_key(&name, |r| r.name).is_ok(),
                "{name} names no RUNTIME entry"
            );
        }
    }

    /// With nothing published, a class pointer is itself on both sides.
    #[test]
    fn translation_without_a_published_runtime_is_the_identity() {
        if super::super::published().is_none() {
            assert_eq!(host_class(0x1234), 0x1234);
            assert_eq!(guest_class(0x1234), 0x1234);
        }
    }
}
