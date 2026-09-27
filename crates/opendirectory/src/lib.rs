//! The parts of Apple's OpenDirectory framework that Chromium Framework imports,
//! and the one answer this host can give.
//!
//! # The whole surface is one function
//!
//! Chromium Framework imports nine symbols from
//! `/System/Library/Frameworks/OpenDirectory.framework/Versions/A/OpenDirectory`:
//! four classes and five `kOD*` string globals. That is the entire import set -
//! a `ROSETTA3_UNBOUND_DETAIL=all` census shows the five globals bound (as
//! `darwin`'s named refusals) and only the four class symbols unbound.
//!
//! Every message the guest sends to those four classes comes from **one
//! function**, and this is measured rather than sampled. A RIP-relative scan of
//! all of `__TEXT,__text` - 214 MB, the `48/4c 8b/8d/8f mod=00 rm=101` shape,
//! seven bytes, and a one-in-2^56 false match per candidate - finds:
//!
//! | slot | symbol | references |
//! |---|---|---|
//! | `0xe6e7710` | `_OBJC_CLASS_$_ODSession` | **1**, at `0x5d9bf9a` |
//! | `0xe6e7718` | `_OBJC_CLASS_$_ODNode` | **1**, at `0x5d9c199` |
//! | `0xe6e7720` | `_OBJC_CLASS_$_ODQuery` | **1**, at `0x5d9c1fe` |
//! | `0xe6e7728` | `_OBJC_CLASS_$_ODRecord` | **1**, at `0x5d9c4b8` |
//! | `0xe6ea718` | `defaultSession` | **1**, at `0x5d9bfa1` |
//! | `0xe6ea720` | `nodeNamesAndReturnError:` | **1**, at `0x5d9bfcd` |
//! | `0xe6ea728` | `nodeWithSession:type:error:` | **1**, at `0x5d9c1ae` |
//! | `0xe6ea730` | `queryWithNode:…error:` | **1**, at `0x5d9c241` |
//! | `0xe6ea738` | `resultsAllowingPartial:error:` | **1**, at `0x5d9c2b7` |
//! | `0xe6ea740` | `valuesForAttribute:error:` | **1**, at `0x5d9c424` |
//!
//! and all ten are inside the single function at **`0x5d9bf70`**, which
//! `test/tools/whichfile.py` brackets between the `__FILE__` markers
//! `../../base/enterprise_util_mac.mm` at `0x5d9bcc7` (+0x2a9) and
//! `../../base/files/file_path_watcher_fsevents.cc` at `0x5d9cecf` (-0xf5f) - a
//! tight bracket on both sides, so the attribution is not a guess from a distant
//! marker.
//!
//! That function is `base::AreDeviceAndUserJoinedToDomain()`'s
//! function-local static initialiser, and the two `__cxa_guard` byte flags at
//! `0xe873a38` and `0xe873a48` around it say so: `0x5d9b7b0` is the guarded
//! accessor, and this initialiser is called from **seven** inlined copies of it
//! (`0x1af747c`, `0x1b1f7db`, `0x1b213de`, `0x1b21445`, `0x57808b8`,
//! `0x5b57944` and `0x5d9b834`), each of which re-disassembles to the same
//! `__cxa_guard_acquire` / test / `call` / `__cxa_guard_release` shape.
//!
//! The other two guards' owner is `base::IsDeviceRegisteredWithManagement()`,
//! which shells out to `/usr/bin/profiles` and has nothing to do with a
//! directory service.
//!
//! # What the guest does with the result, in the guest's own words
//!
//! `base/enterprise_util_mac.mm` (fetched at tag `130.0.6723.91`) - the binary
//! agrees with it in every particular that matters:
//!
//! ```objc
//! ODSession* session = [ODSession defaultSession];
//! if (session == nil) { DLOG(WARNING) << …; return state; }
//! NSArray<NSString*>* node_names = [session nodeNamesAndReturnError:&error];
//! if (!node_names) { DLOG(WARNING) << …; return state; }
//! for (NSString* node_name in node_names)
//!   if ([node_name hasPrefix:@"/LDAPv3"] || [node_name hasPrefix:@"/Active Directory"])
//!     state.device_joined = true;
//! ODNode* node = [ODNode nodeWithSession:session type:kODNodeTypeAuthentication error:&error];
//! if (node == nil) { DLOG(WARNING) << …; return state; }
//! ODQuery* query = [ODQuery queryWithNode:node …];
//! if (query == nil) { DLOG(WARNING) << …; return state; }
//! NSArray* results = [query resultsAllowingPartial:NO error:&error];
//! if (!results) { DLOG(WARNING) << …; return state; }
//! for (id element in results) {
//!   ODRecord* record = base::apple::ObjCCastStrict<ODRecord>(element);
//!   NSArray* attributes = [record valuesForAttribute:kODAttributeTypeMetaRecordName error:nil];
//!   for (id attribute in attributes) { … rangeOfString:@"(^|,)\\s*dc=" … }
//!   attributes = [record valuesForAttribute:kODAttributeTypeAltSecurityIdentities error:nil];
//!   for (id attribute in attributes) { … rangeOfString:@"CN=com.apple.idms.appleid.prd" … }
//! }
//! ```
//!
//! The three string constants in that source are in the binary's
//! `__DATA_CONST,__cfstring` and read back exactly: `/LDAPv3` (7 bytes) at
//! `0xe6748f0`, `/Active Directory` (17) at `0xe674910`,
//! `(^|,)\\s*dc=` (11) at `0xe674930` and
//! `CN=com.apple.idms.appleid.prd` (29) at `0xe674950`. The two
//! `rangeOfString:options:` option words are `0x400`
//! (`NSRegularExpressionSearch`) and `0x1` (`NSCaseInsensitiveSearch`).
//!
//! Two places where the binary is **not** the tag, and where the binary wins:
//!
//! - The two attribute loops run in the **opposite order** from the tag's
//!   source: the binary queries `kODAttributeTypeAltSecurityIdentities` first
//!   (`0xdd50870`, at `0x5d9c524`, paired with the `dc=` regex) and
//!   `kODAttributeTypeMetaRecordName` second (`0xdd50878`, at `0x5d9c70f`,
//!   paired with the iCloud string). The set of checks is the same; the order
//!   is not. This is the same branch-vs-tag divergence PLAN §7 records for
//!   `waitable_event_apple.cc`, so it is expected rather than surprising.
//! - `if (results.count != 1)` is in the source and **not** in the binary: the
//!   `-count` message is still sent, at `0x5d9c315`, and its result is discarded
//!   unexamined. Consistent with the `DLOG` being compiled out, below.
//!
//! # Whether the guest tolerates a nil - measured, and this is the load-bearing fact
//!
//! Every one of the five answers is nil, and each is safe **because the guest
//! tests it**. The tests, in address order, all inside `0x5d9bf70`:
//!
//! | message | call | test | nil lands at | what happens there |
//! |---|---|---|---|---|
//! | `+[ODSession defaultSession]` | `0x5d9bfa8` | `0x5d9bfb9 test rax,rax` | `0x5d9c149` | `xor r14d,r14d; xor ebx,ebx; jmp 0x5d9c980` -> returns 0 |
//! | `-[ODSession nodeNamesAndReturnError:]` | `0x5d9bfde` | `0x5d9bfff test rbx,rbx` | `0x5d9c170` | `xor r14d,r14d; [rbp-0x254]=0; jmp 0x5d9c965` -> returns 0 |
//! | `+[ODNode nodeWithSession:type:error:]` | `0x5d9c1c4` | `0x5d9c1ee test r14,r14` | `0x5d9c8f9` | `xor r14d,r14d; jmp 0x5d9c955` -> returns `[rbp-0x254]` |
//! | `+[ODQuery queryWithNode:…]` | `0x5d9c26c` | `0x5d9c2a7 test r14,r14` | `0x5d9c8fe` | `xor r14d,r14d; jmp 0x5d9c93b` -> returns `[rbp-0x254]` |
//! | `-[ODQuery resultsAllowingPartial:error:]` | `0x5d9c2cd` | `0x5d9c2fb test r15,r15` | `0x5d9c92b` | `objc_release(nil)`, then the same epilogue |
//! | `-[ODRecord valuesForAttribute:error:]` | `0x5d9c534`, `0x5d9c71f` | - | - | the nil is messaged (`0x5d9c58b`), and the test is on that result |
//!
//! All six nil destinations converge on one epilogue at `0x5d9c980`, which
//! releases, pops the pool, checks the pool token and returns `eax = bx |
//! r14d`. `bx` is `[rbp-0x254]`, which is 1 only if a node *name* matched
//! `/LDAPv3` or `/Active Directory`, and 0 on every path reachable without a
//! session. So the guest's conclusion is
//! `DeviceUserDomainJoinState{false,false}` - **not domain-joined** - which is
//! true on this host.
//!
//! The sixth row is different in kind and is worth separating out. A nil from
//! `valuesForAttribute:error:` is not tested directly; it is messaged, because
//! the guest enumerates the result:
//!
//! ```text
//! 5d9c534  call *objc_msgSend            ; -[ODRecord valuesForAttribute:… error:nil]
//! 5d9c560  call _objc_retain             ; retain(nil): a no-op
//! 5d9c573  mov  rdi,rax                  ; nil
//! 5d9c58b  call *objc_msgSend            ; -[nil countByEnumeratingWithState:objects:count:]
//! 5d9c594  test rax,rax
//! 5d9c597  je   0x5d9c6f4                ; the "no values" path
//! ```
//!
//! so this one nil has to survive **message-to-nil**, not merely be
//! non-null. It does: `crates/objc/src/msg.rs`'s x86-64 `msg_send` opens with
//! `test rdi, rdi / je` and an arm that zeroes the return registers, so the
//! enumeration answers 0 and the guest falls into the same branch it takes for
//! an empty collection. `objc_retain`/`objc_release` of nil are likewise
//! no-ops. So the chain holds at this row too - and it holds because the
//! *runtime* answers message-to-nil, not because any null check was added to
//! guest code.
//!
//! Two traps in the same function, both `int3; ud2`, both reachable only if an
//! answer is wrong in a specific way, and both worth naming because they are the
//! shapes a wrong answer would take:
//!
//! - `0x5d9c507 je 0x5d9c9c4` - fires when an element of the results array is
//!   nil **and** is not an `ODRecord`. So an array containing a nil would trap.
//!   An empty array does not, and neither does no array.
//! - `0x5d9c66d je 0x5d9c9b9` and `0x5d9c86d je 0x5d9c9bc` - fire when an
//!   attribute value is nil **and** is not an `NSString`, which is the
//!   `ObjCCastStrict<NSString>` check failing.
//!
//! The guest has a **verified** nil path at every one of the five answers, and
//! every one of them is what this crate returns. A refusal is therefore not a
//! risk here; it is the answer.
//!
//! # Why the answer is nil and not an empty collection
//!
//! The empty answer would be an `NSArray` of node names with nothing in it, from
//! a real session, a real node, a real query and a real result set. **Every one
//! of those would be invented.** There is no session to enumerate, no node in it,
//! no query over the node, and no records in the query's answer - and each of
//! those is a separate claim about a world that has to be checked before it is
//! made.
//!
//! Nil is a *different and smaller* claim: "there is no OpenDirectory service
//! here, so there is no session to give you." One fact, checkable, and the
//! guest's own source has an explicit branch written for it.
//!
//! # What makes the nil true
//!
//! Not a header, and not an assumption about Linux. The **guest binary itself**
//! says OpenDirectory is a Mach service: its embedded sandbox profile, in
//! `__TEXT,__const`, allows `mach-lookup` for
//!
//! ```text
//! @0xced1765  com.apple.system.opendirectoryd.libinfo
//! @0xced3004  com.apple.system.opendirectoryd.membership
//! @0xced482e  com.apple.system.opendirectoryd.membership
//! @0xced4c3f  com.apple.system.opendirectoryd.membership
//! ```
//!
//! Four `global-name` allowances in a binary that has no OpenDirectory to read
//! them from - a positive, from the guest, for the subsystem this crate refuses.
//!
//! And on this host, each of the four things such a service would be:
//!
//! | what | measured |
//! |---|---|
//! | the service itself | no `bootstrap_look_up`; the runtime provides its own |
//! | `directoryd` | not on `PATH`; no matching `systemd` unit |
//! | `/var/db/dslocal` | does not exist |
//! | `/System/Library/Frameworks` | does not exist |
//! | an LDAP server to proxy to | nothing listening on 389 or 636 |
//!
//! So "no directory service" is a reading of this machine, not a default.
//!
//! # What is deliberately *not* here
//!
//! - **The five `kOD*` globals.** `_kODRecordTypeUsers`,
//!   `_kODAttributeTypeRecordName`, `_kODAttributeTypeAllAttributes`,
//!   `_kODAttributeTypeMetaRecordName` and
//!   `_kODAttributeTypeAltSecurityIdentities` are `CFStringRef` constants whose
//!   text belongs to a framework this host does not have, and `darwin` already
//!   publishes all five as named zeroed refusals. They are read here as null -
//!   at `0x5d9c20c`, `0x5d9c21d`, `0x5d9c237`, `0x5d9c39e` and `0x5d9c3af` -
//!   and passed to a method that is never called, because the node that would
//!   precede it does not exist. Implementing them would mean guessing five
//!   strings that nothing in the image can check.
//! - **The `NSError` domain and code of a real OpenDirectory failure.** Apple's
//!   `ODErrorDomain` string appears nowhere in the Framework's `__TEXT` - it was
//!   searched for and is absent - and no `DLOG` string from this function is in
//!   the image either (see below), so nothing in this guest reads an error's
//!   domain. See [`refusal`] for what is used instead and why.
//! - **`+[ODRecord class]` and `-[ODRecord isKindOfClass:]`.** Both are the
//!   runtime's own `NSObject` methods (`crates/objc/src/nsobject.rs`, `+class`
//!   at line 26 and `isKindOfClass:` at line 64) and both resolve through the
//!   `NSObject` superclass these classes are registered with. They are
//!   *measured* as sent - at `0x5d9c4cd` and `0x5d9c4dd` - and that
//!   `+[ODRecord class]` is the *class* receiver is why the class objects have
//!   to be real and not the empty ones `ROSETTA3_NAME_MISSING_CLASSES` builds.
//!
//! # A measured negative about the error out-parameters
//!
//! Six `DLOG(WARNING)` messages appear in the source of this function - "ODSession
//! default session is nil", "ODSession failed to give node names", "ODSession
//! cannot obtain the authentication node", "ODSession cannot create user
//! query", "ODSession cannot obtain current user node", "ODSession unexpected
//! number of user nodes". **None of the six strings is in the Framework's
//! `__TEXT`**, which is what `DLOG` compiled out of a release build looks like.
//!
//! So the `error.localizedDescription` reads the source shows do not exist in
//! this binary, and the `NSError **` values are retained and released by the
//! guest (`objc_retain` at `0x5d9bf6`, `0x5d9c1dc`, `0x5d9c288`, `0x5d9c2e5`)
//! but never read. The domain and code in [`refusal`] are therefore
//! unobservable to this guest - which is stated so that nobody later reads
//! "unobservable" as "therefore important", and reaches for Apple's spelling.

use std::ffi::CStr;

mod query;
mod refusal;
mod session;

pub use refusal::{CODE, DOMAIN};

/// Install instance methods on a class.
macro_rules! methods {
    ($class:expr, { $( $sel:literal => $imp:expr, $types:literal );* $(;)? }) => {{
        let class: &'static objc::class::Class = $class;
        $(
            class.replace_method(
                objc::sel::get($sel),
                $imp as *const std::ffi::c_void,
                $types,
            );
        )*
    }};
}

/// Install class methods, which live on the metaclass.
macro_rules! class_methods {
    ($class:expr, { $( $sel:literal => $imp:expr, $types:literal );* $(;)? }) => {{
        let class: &'static objc::class::Class = $class;
        let meta = class.metaclass();
        $(
            meta.replace_method(
                objc::sel::get($sel),
                $imp as *const std::ffi::c_void,
                $types,
            );
        )*
    }};
}

pub(crate) use {class_methods, methods};

/// The four classes this crate answers for, in the order the guest names them.
pub const CLASSES: &[&CStr] = &[c"ODSession", c"ODNode", c"ODQuery", c"ODRecord"];

/// Every selector the guest sends to those four classes, and the class each one
/// goes to.
///
/// The list is the census in [`crate`]: one rip-reference each, all inside
/// `0x5d9bf70`. `+[ODRecord class]` and `-[ODRecord isKindOfClass:]` are the two
/// the guest sends and this crate does **not** implement, because `NSObject`
/// already answers them; they are listed because a message the guest sends is a
/// message the report has to account for.
pub const SELECTORS: &[(&CStr, &str, &str)] = &[
    (c"ODSession", "+", "defaultSession"),
    (c"ODSession", "-", "nodeNamesAndReturnError:"),
    (c"ODNode", "+", "nodeWithSession:type:error:"),
    (c"ODQuery", "+", "queryWithNode:forRecordTypes:attribute:matchType:queryValues:\
                       returnAttributes:maximumResults:error:"),
    (c"ODQuery", "-", "resultsAllowingPartial:error:"),
    (c"ODRecord", "-", "valuesForAttribute:error:"),
    // Answered by `NSObject`, through the superclass these classes are
    // registered with - `crates/objc/src/nsobject.rs`.
    (c"ODRecord", "+", "class"),
    (c"ODRecord", "-", "isKindOfClass:"),
];

/// Install this crate's classes and methods. Idempotent.
pub fn bootstrap() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        foundation::bootstrap();
        // `NSObject` is the superclass of all four on Darwin, and the reason
        // `+[ODRecord class]` and `-[ODRecord isKindOfClass:]` resolve without
        // this crate implementing anything: both are `NSObject` methods. A
        // `nil` superclass would make the guest's own `ObjCCastStrict<ODRecord>`
        // fail, and its failure path is `0x5d9c9c4: int3; ud2`.
        for name in CLASSES {
            corefoundation::base::abstract_class(name, None);
        }
        let class = |name: &CStr| {
            objc::class::lookup(name).unwrap_or_else(|| {
                unreachable!("{name:?} was registered by the loop above")
            })
        };
        session::install(class(c"ODSession"));
        session::install_node(class(c"ODNode"));
        query::install_query(class(c"ODQuery"));
        query::install_record(class(c"ODRecord"));
    });
}

/// The Objective-C class objects exported by OpenDirectory.framework.
///
/// The class objects and **not** the `kOD*` string globals: those are five
/// `CFStringRef` constants whose text is a property of a framework this host
/// does not have, and `darwin` publishes all five as named zeroed refusals
/// already. Publishing them here too would define one name twice, which the
/// `no_framework_defines_a_symbol_another_one_does` test rightly refuses.
///
/// A `_OBJC_CLASS_$_X` symbol's *value* is the class, not the address of a
/// pointer to it, so the published address is the class object itself - the
/// same shape `foundation::class_data` and `webkit::data_symbols` use.
pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    let mut out = Vec::new();
    for name in CLASSES {
        let class = objc::class::lookup(name)
            .unwrap_or_else(|| panic!("{name:?} is registered by bootstrap"));
        let name = name.to_string_lossy();
        out.push((
            format!("_OBJC_CLASS_$_{name}"),
            objc::class::ptr(class) as usize as u64,
        ));
        out.push((
            format!("_OBJC_METACLASS_$_{name}"),
            objc::class::ptr(class.metaclass()) as usize as u64,
        ));
    }
    out
}

/// `dlsym` for this framework's names.
///
/// `darwin::symbol` strips exactly one leading underscore, so the comparison
/// here strips one too - which is why it is written against `data_symbols()`
/// rather than against the `CLASSES` list, whose members carry no underscore.
pub fn symbol(name: &str) -> Option<u64> {
    let wanted = name.trim_start_matches('_');
    data_symbols()
        .into_iter()
        .find(|(symbol, _)| symbol.trim_start_matches('_') == wanted)
        .map(|(_, address)| address)
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc::class::Class;
    use objc::object::Id;
    use objc::sel::Sel;

    /// The installed `IMP` for a selector, transmuted to a prototype.
    ///
    /// Called directly rather than through `objc_msgSend` because that is what
    /// the tree's other class tests do (`contacts::tests`, `webkit::tests`), and
    /// because a `BOOL`-returning selector does not come back in a full
    /// register through a variadic prototype - a test that read one as a `usize`
    /// would be testing the harness.
    fn imp_of(
        class: &'static Class,
        selector: &str,
        class_method: bool,
    ) -> objc::class::Imp {
        let owner = if class_method {
            class.metaclass()
        } else {
            class
        };
        owner
            .method(objc::sel::get(selector))
            .unwrap_or_else(|| panic!("{selector} is not installed"))
            .implementation()
    }

    fn named(name: &CStr) -> &'static Class {
        bootstrap();
        objc::class::lookup(name).unwrap_or_else(|| panic!("{name:?} is registered"))
    }

    #[test]
    fn the_default_session_is_nil() {
        let class = named(c"ODSession");
        let f: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(imp_of(class, "defaultSession", true)) };
        let session = unsafe { f(objc::class::ptr(class) as Id, objc::sel::get("defaultSession")) };
        assert_eq!(
            session, std::ptr::null_mut(),
            "+defaultSession must answer nil. An ODSession standing in for a service that is not \
             here would be a claim about a directory this host does not have"
        );
    }

    #[test]
    fn the_other_four_refuse_and_answer_nil() {
        // Each is given the arguments the guest passes, read out of its own
        // disassembly, so this is the shape the real call site has rather than a
        // shape invented for the test.
        let mut error: Id = std::ptr::null_mut();

        let session = named(c"ODSession");
        let f: unsafe extern "C" fn(Id, Sel, *mut Id) -> Id =
            unsafe { std::mem::transmute(imp_of(session, "nodeNamesAndReturnError:", false)) };
        // An instance, made by hand: `+defaultSession` refuses to make one, and
        // that is the point - the instance method must still answer.
        let instance = objc::object::create_instance(session, 0);
        let names = unsafe {
            f(
                instance,
                objc::sel::get("nodeNamesAndReturnError:"),
                &raw mut error,
            )
        };
        assert!(names.is_null(), "an empty NSArray here would claim a search happened");
        assert!(!error.is_null(), "a nil result owes the caller an NSError");
        objc::arc::release(error);
        error = std::ptr::null_mut();

        let node = named(c"ODNode");
        let f: unsafe extern "C" fn(Id, Sel, Id, u32, *mut Id) -> Id =
            unsafe { std::mem::transmute(imp_of(node, "nodeWithSession:type:error:", true)) };
        // 0x2201 is the node type the guest asks for, at 0x5d9c1bf.
        let got = unsafe {
            f(
                objc::class::ptr(node) as Id,
                objc::sel::get("nodeWithSession:type:error:"),
                std::ptr::null_mut(),
                0x2201,
                &raw mut error,
            )
        };
        assert!(got.is_null());
        assert!(!error.is_null());
        objc::arc::release(error);
        error = std::ptr::null_mut();

        let query = named(c"ODQuery");
        let f: unsafe extern "C" fn(Id, Sel, Id, u32, Id, u32, Id, Id, u32, *mut Id) -> Id = unsafe {
            std::mem::transmute(imp_of(
                query,
                "queryWithNode:forRecordTypes:attribute:matchType:queryValues:\
                 returnAttributes:maximumResults:error:",
                true,
            ))
        };
        let got = unsafe {
            f(
                objc::class::ptr(query) as Id,
                objc::sel::get(
                    "queryWithNode:forRecordTypes:attribute:matchType:queryValues:\
                     returnAttributes:maximumResults:error:",
                ),
                std::ptr::null_mut(), // node
                0,                   // recordTypes: the guest's is null in this build
                std::ptr::null_mut(), // attribute
                0x2001,              // matchType, at 0x5d9c25f
                std::ptr::null_mut(), // queryValues
                std::ptr::null_mut(), // returnAttributes
                0,                   // maximumResults, at 0x5d9c267
                &raw mut error,
            )
        };
        assert!(got.is_null());
        assert!(!error.is_null());
        objc::arc::release(error);
        error = std::ptr::null_mut();

        let f: unsafe extern "C" fn(Id, Sel, u8, *mut Id) -> Id =
            unsafe { std::mem::transmute(imp_of(query, "resultsAllowingPartial:error:", false)) };
        let instance = objc::object::create_instance(query, 0);
        // The guest passes `allowingPartial:NO` (`xor edx,edx` at 0x5d9c2cb).
        let got = unsafe {
            f(
                instance,
                objc::sel::get("resultsAllowingPartial:error:"),
                0,
                &raw mut error,
            )
        };
        assert!(
            got.is_null(),
            "an empty NSArray of results here would be the wrong answer that looks like a search \
             that found nothing"
        );
        assert!(!error.is_null());
        objc::arc::release(error);

        let record = named(c"ODRecord");
        let f: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(imp_of(record, "valuesForAttribute:error:", false)) };
        let instance = objc::object::create_instance(record, 0);
        // `error:nil` at both of the guest's sites (`xor ecx,ecx` at 0x5d9c52b
        // and 0x5d9c716), so the null out-parameter is a shape it really uses.
        let got = unsafe {
            f(
                instance,
                objc::sel::get("valuesForAttribute:error:"),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert!(got.is_null());
    }

    #[test]
    fn a_nil_answer_from_values_for_attribute_survives_being_messaged() {
        // The one answer that has to be nil **and** be sent a message: the guest
        // enumerates the result of `valuesForAttribute:error:` straight away, at
        // 0x5d9c58b. So this is the row where nil is a segfault in polite
        // clothes if messaging nil does not answer zero.
        let record = named(c"ODRecord");
        let f: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(imp_of(record, "valuesForAttribute:error:", false)) };
        let instance = objc::object::create_instance(record, 0);
        let values = unsafe {
            f(
                instance,
                objc::sel::get("valuesForAttribute:error:"),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert!(values.is_null());
        // The runtime's own `objc_msgSend`, through the real lookup.
        let entry: unsafe extern "C" fn(Id, Sel, ...) -> usize =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let sel = objc::sel::get("countByEnumeratingWithState:objects:count:");
        let mut state = [0usize; 8];
        let mut buffer = [std::ptr::null_mut::<Id>(); 16];
        // SAFETY: four arguments, the last three of which are the pointers the
        // selector's name says they are, and `state` is 64 bytes, which is the
        // size of an `NSFastEnumerationState`.
        let produced = unsafe {
            entry(
                values,
                sel,
                &raw mut state as usize,
                &raw mut buffer as usize,
                buffer.len(),
            )
        };
        assert_eq!(
            produced, 0,
            "messaging nil must answer zero, or the guest's enumeration at 0x5d9c594 takes the \
             wrong branch"
        );
    }

    #[test]
    fn the_object_classes_inherit_the_two_generic_selectors_the_guest_sends() {
        // `+[ODRecord class]` at 0x5d9c4cd and `-[ODRecord isKindOfClass:]` at
        // 0x5d9c4dd are `NSObject` methods reached through the superclass, and
        // the class receiver in the first is exactly why these four classes have
        // to be real objects rather than the empty ones
        // `ROSETTA3_NAME_MISSING_CLASSES` builds.
        let record = named(c"ODRecord");
        let meta = record.metaclass();
        let class_of: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(imp_of(record, "class", true)) };
        let got = unsafe { class_of(objc::class::ptr(record) as Id, objc::sel::get("class")) };
        assert_eq!(
            got,
            objc::class::ptr(record) as Id,
            "+[ODRecord class] returns the class"
        );
        assert!(meta.method(objc::sel::get("class")).is_some());

        let is_kind: unsafe extern "C" fn(Id, Sel, Id) -> u8 =
            unsafe { std::mem::transmute(imp_of(record, "isKindOfClass:", false)) };
        let instance = objc::object::create_instance(record, 0);
        let got = unsafe {
            is_kind(
                instance,
                objc::sel::get("isKindOfClass:"),
                objc::class::ptr(record) as Id,
            )
        };
        assert_ne!(got, 0, "a record is a kind of ODRecord");
    }

    #[test]
    fn the_four_classes_are_ns_objects_and_descend_from_it() {
        let nsobject = objc::class::lookup(c"NSObject").expect("NSObject");
        for name in CLASSES {
            let class = named(name);
            assert_eq!(class.name_str(), name.to_str().unwrap());
            assert!(
                class.chain().any(|ancestor| std::ptr::eq(ancestor, nsobject)),
                "{name:?} must descend from NSObject, or +class and isKindOfClass: do not resolve \
                 and the guest's ObjCCastStrict<ODRecord> lands on int3 at 0x5d9c9c4"
            );
        }
    }

    #[test]
    fn exports_class_and_metaclass_for_each() {
        bootstrap();
        let symbols = data_symbols();
        for name in CLASSES {
            let spelling = name.to_string_lossy();
            for prefix in ["_OBJC_CLASS_$_", "_OBJC_METACLASS_$_"] {
                let wanted = format!("{prefix}{spelling}");
                assert!(
                    symbols.iter().any(|(published, _)| *published == wanted),
                    "{wanted} is not published"
                );
            }
        }
        assert!(
            symbols.iter().all(|(_, address)| *address != 0),
            "a class object at address zero is the unbound-import failure mode this crate exists \
             to remove"
        );
    }

    #[test]
    fn the_published_spelling_is_the_images_spelling() {
        // The census names these four exactly, with ONE leading underscore, and
        // the synthetic dylib resolves imports verbatim with no normalisation. A
        // clean build and a green suite both accept a name that is off by one
        // underscore; only this check does not. `darwin::symbol` strips exactly
        // one, which is the same trap.
        bootstrap();
        let symbols = data_symbols();
        for wanted in [
            "_OBJC_CLASS_$_ODSession",
            "_OBJC_CLASS_$_ODNode",
            "_OBJC_CLASS_$_ODQuery",
            "_OBJC_CLASS_$_ODRecord",
        ] {
            let found = symbols
                .iter()
                .find(|(published, _)| published == wanted)
                .map(|(_, address)| *address);
            assert!(found.is_some(), "{wanted} does not match the image's spelling");
            assert_eq!(
                symbol(wanted),
                found,
                "dlsym must answer the address the flat provider binds, for {wanted}"
            );
            assert_ne!(found, Some(0));
        }
        // And nothing extra: a name published that no import asks for is a name
        // that can only ever be wrong.
        assert_eq!(symbols.len(), CLASSES.len() * 2);
    }

    #[test]
    fn the_selector_census_and_the_installed_methods_agree() {
        for (name, kind, selector) in SELECTORS {
            let class = named(name);
            let owner = if *kind == "+" {
                class.metaclass()
            } else {
                class
            };
            assert!(
                owner.method(objc::sel::get(selector)).is_some(),
                "{kind}[{name:?} {selector}] is not installed, and an absent selector on a \
                 published class is an abort by name"
            );
        }
    }

    #[test]
    fn the_default_session_being_nil_is_what_keeps_the_rest_unreachable() {
        // The property the whole crate rests on, asserted rather than assumed:
        // the guest's own `test rax,rax` at 0x5d9bfb9 takes its branch, so the
        // four methods below it are not called by this guest at all. They exist
        // so a published class can answer, not because they are needed.
        let class = named(c"ODSession");
        let f: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(imp_of(class, "defaultSession", true)) };
        let session = unsafe { f(objc::class::ptr(class) as Id, objc::sel::get("defaultSession")) };
        assert!(session.is_null());
    }
}
