//! `ODSession` and `ODNode`: the two handles into a directory service.
//!
//! Both are unreachable in a run of this guest, and that is a *result*, not an
//! accident of wiring. `-[ODSession nodeNamesAndReturnError:]` and
//! `+[ODNode nodeWithSession:type:error:]` are only ever sent to an object that
//! `+[ODSession defaultSession]` produced, and that answers nil - so the caller
//! takes its nil branch at the first message and never gets here. They exist
//! because a *class* that has been published must answer every selector the
//! guest sends to it: a missing one is `objc`'s own refusal-by-name, an abort,
//! and an abort is a worse answer than a refusal.
//!
//! Each one therefore refuses, and each says so. The disassembly and the nil
//! tolerance that makes refusing safe are in the crate note.

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::refusal;

/// `+[ODSession defaultSession]` - the class method, on the metaclass.
pub fn install(class: &'static Class) {
    use crate::{class_methods, methods};
    class_methods!(class, {
        "defaultSession" => default_session, c"@16@0:8";
    });
    methods!(class, {
        "nodeNamesAndReturnError:" => node_names_and_return_error, c"@24@0:8*@16";
    });
}

/// `+[ODNode nodeWithSession:type:error:]`.
pub fn install_node(class: &'static Class) {
    use crate::class_methods;
    class_methods!(class, {
        "nodeWithSession:type:error:" => node_with_session, c"@40@0:8@16I24*@32";
    });
}

/// `+[ODSession defaultSession]`
///
/// **nil, and that is the true answer.** An `ODSessionRef` is a client handle on
/// the OpenDirectory Mach service; there is no such service on this host, so
/// there is no handle. This is not a stand-in for a missing implementation - the
/// implementation is complete and its answer is "there is nothing here".
///
/// The caller is written for exactly this. From `base/enterprise_util_mac.mm`:
///
/// ```objc
/// ODSession* session = [ODSession defaultSession];
/// if (session == nil) {
///   DLOG(WARNING) << "ODSession default session is nil.";
///   return state;                       // state stays {false, false}
/// }
/// ```
///
/// and the binary has that branch, with no gap in it:
///
/// ```text
/// 5d9bfa8  call  *objc_msgSend           ; +[ODSession defaultSession]
/// 5d9bfae  mov  rdi,rax
/// 5d9bfb1  call  _objc_retainAutoreleasedReturnValue
/// 5d9bfb6  mov  r12,rax
/// 5d9bfb9  test rax,rax                  ; <-- the test
/// 5d9bfbc  je   0x5d9c149                ; <-- straight to the epilogue
/// ...
/// 5d9c149  xor  r14d,r14d
/// 5d9c14c  xor  ebx,ebx
/// 5d9c14e  jmp  0x5d9c980
/// ```
///
/// so `eax` is `bx | r14d` = 0 - `DeviceUserDomainJoinState{false,false}`. The
/// guest concludes "not joined to a domain", which is **true** on this host.
unsafe extern "C" fn default_session(_class: Id, _sel: Sel) -> Id {
    refusal::say("+[ODSession defaultSession]");
    std::ptr::null_mut()
}

/// `-[ODSession nodeNamesAndReturnError:]`
///
/// **nil, with the `NSError **` filled.** The alternative - an empty `NSArray` -
/// would be a different and *wrong* claim: it says "I looked, there are zero
/// nodes", where the truth is "there is no directory service to look at". Darwin
/// spells that difference as `nil` plus an error, and the caller's own branch
/// distinguishes them (`if (!node_names) return state;` before it iterates).
///
/// Unreachable, because no session is ever created. The caller's tolerance is
/// measured, not assumed:
///
/// ```text
/// 5d9bfff  test rbx,rbx
/// 5d9c009  je   0x5d9c170
/// 5d9c170  xor  r14d,r14d
/// 5d9c173  mov  DWORD PTR [rbp-0x254],0x0
/// 5d9c17d  jmp  0x5d9c965                ; -> the same epilogue, returning 0
/// ```
unsafe extern "C" fn node_names_and_return_error(
    _self: Id,
    _sel: Sel,
    error: *mut Id,
) -> Id {
    // SAFETY: `error` is the caller's `NSError **`, null or writable.
    unsafe { refusal::refuse(error, "-[ODSession nodeNamesAndReturnError:]") }
}

/// `+[ODNode nodeWithSession:type:error:]`
///
/// **nil, with the `NSError **` filled.** `type` is the node type the caller
/// asked for, and the binary shows it asking for one specific kind rather than
/// "any":
///
/// ```text
/// 5d9c1bf  mov  ecx,0x2201                ; the node type
/// ```
///
/// which `base/enterprise_util_mac.mm` names `kODNodeTypeAuthentication`. The
/// answer does not depend on it: **every** node in **every** directory service
/// lives inside a service, and there is none. Passing `type` straight through to
/// a refusal that ignores it is deliberate - inventing a node for the type the
/// caller asked about is exactly the wrong answer that would look like success.
///
/// Unreachable without a session. The caller's tolerance:
///
/// ```text
/// 5d9c1ee  test r14,r14
/// 5d9c1f8  je   0x5d9c8f9
/// 5d9c8f9  xor  r14d,r14d
/// 5d9c8fc  jmp  0x5d9c955
/// ```
unsafe extern "C" fn node_with_session(
    _class: Id,
    _sel: Sel,
    session: Id,
    node_type: u32,
    error: *mut Id,
) -> Id {
    refusal::say("+[ODNode nodeWithSession:type:error:]");
    // The two arguments are named in the log so a run's record says which node
    // was asked for and whether a session was offered. Both are this runtime
    // reading the caller's own arguments, not claims about the world.
    eprintln!(
        "rosetta3: opendirectory: +[ODNode nodeWithSession:type:error:]: asked for node type \
         {node_type:#x} with a {} session; there is no directory service to ask",
        if session.is_null() { "nil" } else { "given" },
    );
    // SAFETY: the caller's `NSError **`, null or writable.
    unsafe { refusal::set(error, "no OpenDirectory service on this host") };
    std::ptr::null_mut()
}
