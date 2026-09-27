//! `ODQuery` and `ODRecord`: a search over a directory, and one record of it.
//!
//! A record exists only as an element of a query's result, and a query only
//! against a node, and a node only inside a session. `+[ODSession defaultSession]`
//! is nil, so the whole chain below it is unreachable in a run of this guest -
//! and every one of these still refuses rather than aborts by name, because a
//! published class that cannot answer a selector the guest sends to it turns a
//! refusal into a crash.
//!
//! The caller's tolerance for each nil is measured; see the crate note.

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::refusal;

/// The `ODQuery` methods, one class method and one instance method.
///
/// **One `+` selector, and it is the only one.** An exhaustive scan of the
/// Framework's `__TEXT,__objc_methname` finds exactly one string beginning
/// `queryWithNode` - the one at `0xdbfa1db` - and `allowedAttributes` appears in
/// no section of the image at all. OpenDirectory's headers do declare a longer
/// `…maximumResults:allowedAttributes:error:` form, and it was tempting to
/// install it "just in case". That would be inventing a selector name and a
/// type encoding that nothing in the guest can check, which is the one thing a
/// refusal must not do. The only selector a guest can be shown sending is the
/// one that exists.
pub fn install_query(class: &'static Class) {
    use crate::{class_methods, methods};
    class_methods!(class, {
        "queryWithNode:forRecordTypes:attribute:matchType:queryValues:returnAttributes:maximumResults:error:"
            => query_with_node, c"@80@0:8@16I24@32I40@48@56I64*@72";
    });
    // An instance method, so it goes on the class and not the metaclass - and
    // `-resultsAllowingPartial:error:` is the *only* instance method OpenDirectory
    // declares on `ODQuery` in a way this guest can reach. Getting that owner
    // wrong would make the guest's send at 0x5d9c2cd land on a class method,
    // which answers with the metaclass instead of running this.
    methods!(class, {
        "resultsAllowingPartial:error:" => results_allowing_partial, c"@32@0:8B16*@24";
    });
}

/// `-[ODRecord valuesForAttribute:error:]`.
pub fn install_record(class: &'static Class) {
    use crate::methods;
    methods!(class, {
        "valuesForAttribute:error:" => values_for_attribute, c"@32@0:8@16*@24";
    });
}

/// `+[ODQuery queryWithNode:forRecordTypes:attribute:matchType:queryValues:returnAttributes:maximumResults:error:]`
///
/// **nil, with the `NSError **` filled.** The arguments do not change the
/// answer and are named in the log rather than acted on, because a search needs
/// a directory and there is none:
///
/// ```text
/// 5d9c1fe  mov  r12,[rip+…]               ; ODQuery
/// 5d9c205  mov  rax,[rip+…]               ; &_kODRecordTypeUsers
/// 5d9c20c  mov  rax,[rax]                 ; <- NULL: darwin publishes this as a
///                                           ;    zeroed refusal, so the record
///                                           ;    type is null in this build
/// 5d9c216  mov  rax,[rip+…]               ; &_kODAttributeTypeRecordName
/// 5d9c21d  mov  r13,[rax]                 ; <- NULL, for the same reason
/// 5d9c220  call _NSUserName
/// 5d9c230  mov  rax,[rip+…]               ; &_kODAttributeTypeAllAttributes
/// 5d9c237  mov  rax,[rax]                 ; <- NULL
/// 5d9c25f  mov  r9d,0x2001                ; the match type
/// 5d9c267  push 0x0                       ; maximumResults
/// 5d9c26c  call *objc_msgSend
/// 5d9c2a7  test r14,r14
/// 5d9c2aa  je   0x5d9c8fe                 ; <-- nil is the guest's own path
/// ```
///
/// That last pair is the reason this is safe, and it is worth stating plainly
/// because the alternative would have been to implement the three `kOD*`
/// globals as well. **They are not implemented here, and doing so would be
/// wrong twice over:** they are `CFStringRef` constants whose text is a property
/// of a framework that is not on this host, and the query that would consume
/// them cannot be created because there is no node to create it against. If a
/// future change made `+[ODNode nodeWithSession:type:error:]` answer with a
/// node, this method would have to become an implementation and the three
/// globals would have to be answered with their real strings at the same time -
/// neither is reachable today.
///
/// Two of the declared widths are read off the binary and one is not, and the
/// difference is worth stating rather than smoothing over. `recordTypes` and
/// `matchType` are narrowed by the *caller*: `mov ecx, …` and `mov r9d, 0x2001`
/// write 32 bits, which is what clang does for a 32-bit argument and not for a
/// 64-bit one. `maximumResults` arrives as `push 0x0`, and a `push imm8` writes
/// all eight bytes of the slot, so **nothing in this guest can tell a 32-bit
/// `maximumResults` from a 64-bit one.** It is declared 32-bit here because that
/// reading is safe either way: the low four bytes of the slot are what a 32-bit
/// argument defines, whereas a 64-bit reader of a slot a 32-bit writer left
/// partly undefined would be reading the wrong thing.
unsafe extern "C" fn query_with_node(
    _class: Id,
    _sel: Sel,
    node: Id,
    record_types: u32,
    attribute: Id,
    match_type: u32,
    _query_values: Id,
    return_attributes: Id,
    maximum_results: u32,
    error: *mut Id,
) -> Id {
    refusal::say("+[ODQuery queryWithNode:…]");
    // Every argument is echoed once, so the record says exactly what was asked
    // for. They are the caller's own words, read back - not claims about the
    // world.
    eprintln!(
        "rosetta3: opendirectory: +[ODQuery queryWithNode:…]: node {}, recordTypes {record_types:#x}, \
         attribute {}, matchType {match_type:#x}, returnAttributes {}, maximumResults {maximum_results}; \
         there is no directory service to search",
        if node.is_null() { "nil" } else { "given" },
        if attribute.is_null() { "nil" } else { "given" },
        if return_attributes.is_null() { "nil" } else { "given" },
    );
    // SAFETY: the caller's `NSError **`, null or writable.
    unsafe { refusal::set(error, "no OpenDirectory service on this host") };
    std::ptr::null_mut()
}

/// `-[ODQuery resultsAllowingPartial:error:]`
///
/// **nil, with the `NSError **` filled.** This is the call that would have
/// returned the array of matching records, and the one place where a made-up
/// answer would be most tempting: an empty `NSArray` here would walk the
/// caller's enumeration loop zero times and produce
/// `DeviceUserDomainJoinState{false, false}` - the same result as a refusal, by
/// a route that looks like a successful search that found nothing.
///
/// It is refused, because "no records matched the query" and "there is no
/// directory service to run the query against" are different claims, and only
/// the second is true.
///
/// The caller's tolerance, measured:
///
/// ```text
/// 5d9c2cd  call *objc_msgSend            ; -[ODQuery resultsAllowingPartial:NO error:&error]
/// 5d9c2fb  test r15,r15
/// 5d9c2fe  je   0x5d9c92b
/// 5d9c92b  mov  rdi,r15
/// 5d9c92e  call _objc_release             ; releasing nil is a no-op, and the
///                                         ; epilogue at 0x5d9c9a1 returns
///                                         ; 0 either way
/// ```
unsafe extern "C" fn results_allowing_partial(
    _self: Id,
    _sel: Sel,
    allowing_partial: u8,
    error: *mut Id,
) -> Id {
    refusal::say("-[ODQuery resultsAllowingPartial:error:]");
    if allowing_partial != 0 {
        eprintln!(
            "rosetta3: opendirectory: -[ODQuery resultsAllowingPartial:error:]: asked to allow \
             partial results; there is no directory service to ask"
        );
    }
    // SAFETY: the caller's `NSError **`, null or writable.
    unsafe { refusal::set(error, "no OpenDirectory service on this host") };
    std::ptr::null_mut()
}

/// `-[ODRecord valuesForAttribute:error:]`
///
/// **nil, with the `NSError **` filled** where the caller passed one.** This is
/// the one method here the guest would actually reach *if* a record existed, and
/// its nil has to survive a second step: the guest sends
/// `countByEnumeratingWithState:objects:count:` straight to the result, so the
/// nil has to be answerable by message-to-nil rather than merely non-null.
///
/// That is exactly what messaging nil does in this runtime - the return
/// registers are zeroed and the send is over (`crates/objc/src/msg.rs`, the
/// `test rdi, rdi / je` arm of the x86-64 `msg_send`), so the enumeration
/// answers `0` and the guest takes the same branch it takes for an empty
/// collection:
///
/// ```text
/// 5d9c534  call *objc_msgSend            ; -[ODRecord valuesForAttribute:… error:nil]
/// 5d9c560  call _objc_retain             ; retain(nil): a no-op
/// 5d9c573  mov  rdi,rax                  ; nil
/// 5d9c58b  call *objc_msgSend            ; -[nil countByEnumeratingWithState:…]
/// 5d9c594  test rax,rax
/// 5d9c597  je   0x5d9c6f4                ; <-- the "no attribute values" path
/// ```
///
/// And `error` is genuinely null at both call sites, so the out-parameter
/// handling here is exercised by the shape the guest really uses rather than
/// invented for it:
///
/// ```text
/// 5d9c52b  xor  ecx,ecx                  ; error: nil
/// 5d9c716  xor  ecx,ecx                  ; error: nil
/// ```
unsafe extern "C" fn values_for_attribute(
    _self: Id,
    _sel: Sel,
    attribute: Id,
    error: *mut Id,
) -> Id {
    refusal::say("-[ODRecord valuesForAttribute:error:]");
    if !attribute.is_null() {
        eprintln!(
            "rosetta3: opendirectory: -[ODRecord valuesForAttribute:error:]: asked for an \
             attribute; there is no directory service holding attribute values"
        );
    }
    // SAFETY: the caller's `NSError **`, null or writable - and in this guest it
    // is null.
    unsafe { refusal::set(error, "no OpenDirectory service on this host") };
    std::ptr::null_mut()
}
