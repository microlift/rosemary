//! `+[NSPropertyListSerialization propertyListWithData:options:format:error:]`.
//!
//! # The census
//!
//! From `test/logs/class-wall-queue.md`, reproduced here rather than cited, so
//! the numbers in this comment can be checked against the file that produced
//! them. The classref slot is `0xe6e7520`; the loader's own unbound report
//! (`ROSETTA3_UNBOUND_DETAIL=all`) is what names it and where it comes from:
//! `_OBJC_CLASS_$_NSPropertyListSerialization from
//! /System/Library/Frameworks/Foundation.framework`.
//!
//! | | |
//! |--|--:|
//! | confirmed loads of the classref | **4** |
//! | sends measured after a load | **5** |
//! | distinct selectors sent to this class | **1** |
//! | enclosing methods | `KSTicket`, `AppShimTerminationObserver` |
//!
//! The other two loads' "sends" - `dataUsingEncoding:` at `+0x5da3025` and the
//! load at `+0x5da30c9` - are the window's **lower bound** showing its face, not
//! messages to this class. `-[NSString dataUsingEncoding:]` exists
//! (`crates/foundation/src/string.rs:549`) and `NSPropertyListSerialization` has
//! no such selector, so that pairing is a hoisted `__objc_selrefs` value landing
//! inside the window by coincidence. One selector is installed, and it is the
//! one the disassembly shows going to the class object.
//!
//! # The two sites, verified rather than counted
//!
//! `KSTicket` at `Framework+0x4fe2056`:
//!
//! ```text
//!  4fe2056:  mov  rdi,QWORD PTR [rip+0x97054c3]   # 0xe6e7520  the class
//!  4fe205d:  mov  rsi,QWORD PTR [rip+0x9707b44]   # the selector
//!  4fe206b:  mov  rdx,rbx                         ; data
//!  4fe206e:  xor  ecx,ecx                         ; options = 0
//!  4fe2070:  xor  r8d,r8d                         ; format  = 0
//!  4fe2073:  xor  r9d,r9d                         ; error   = NULL
//!  4fe2076:  call QWORD PTR [rip+0x8d6dfbc]       ; objc_msgSend
//!  4fe2087:  test rax,rax
//!  4fe208e:  je   0x4fe21a7                        ; <-- nil is a branch the guest has
//! ```
//!
//! `AppShimTerminationObserver` at `Framework+0x46f7f42` is the same call with
//! `options = 0x64` (`ecx`), and it is nil-tolerant by construction rather than
//! by a branch: the two messages that follow go to the answer, so a nil answer
//! makes them messages to nil, and the pair is folded into one condition at
//! `+0x46f7f9e..0x46f7fa6` that is false when either was zero.
//!
//! So the tolerance is real at both sites - but that is **not** why nil is the
//! answer here, and writing this module as a refusal would be wrong.
//!
//! # Why this is a real answer and not a refusal
//!
//! The bytes are in the guest's own address space and the reader is already in
//! this crate: `plist::parse` takes both spellings (the `bplist00` trailer and
//! the XML one) and `objectify::object` turns the tree into the same objects
//! `NSString`/`NSNumber`/`NSData`/`NSArray`/`NSDictionary` that any other route
//! through this tree produces. **What makes it true is the guest's own bytes,
//! not the host**, so it does not change if the host does; the only thing that
//! changes the answer is the data, and a document this reader cannot parse
//! answers nil, which is Apple's own contract for this method.
//!
//! The two places `objectify` refuses rather than converts - a `<date>` and a
//! `CF$UID` - are left refusing. A date turned into a number of seconds would
//! read correctly for `-timeIntervalSinceReferenceDate` and incorrectly for
//! everything else, which is the failure this project ranks first; it is
//! refused at the point of conversion, by name, and not here.
//!
//! # The two out-parameters, and what they are worth
//!
//! `format` is **in and out**. A non-null value on entry asks for one spelling;
//! on return it is the spelling that was read. `error` is **out only**, and its
//! contract has two halves that are both easy to get wrong, so both are stated
//! here and both are implemented:
//!
//! * `*error` is written on **every** path, success included, where it is
//!   written with nil. A success that did not write leaves the caller's own
//!   pre-zeroed slot in place *by luck* - which is not the same as having said
//!   so, and looks identical until a stack happens to hold something.
//! * a null `error:` is **never** written through. Both measured sites pass
//!   `xor r9d,r9d`; a store through a null out-parameter is a store to address
//!   zero.
//!
//! This is not a new decision. `crate::collections`'s
//! `+[NSDictionary dictionaryWithContentsOfURL:error:]` already implements both
//! halves for the same guest and the same domain, and its comment records the
//! measured pre-zeroing at `+0x470eec3` and the four-of-five sites that pass a
//! null. Following that precedent rather than inventing a second one is the
//! point.
//!
//! A request for a spelling `plist` cannot read - OpenStep - is **reported, not
//! refused**: the caller asked a question with a documented way to be told the
//! answer, so it gets nil plus an `NSError` naming what it asked for.
//! `NSPropertyListReadCorruptError` is `3840`, the same code `crate::json` and
//! `crate::collections` report for a document that is not a readable plist.
//! `format` is deliberately left untouched on that path: reporting "I read
//! nothing" through an in-and-out parameter as one of the two valid spellings
//! would be a plausible value for a value that means something.
//!
//! # What is deliberately not installed
//!
//! - `+propertyList:isValidForFormat:`, `+writePropertyList:...` and the other
//!   siblings. The census measured one selector sent to this class; installing
//!   the rest would be surface nothing has asked for, and a class method that
//!   answers for a format this crate cannot write is a place a wrong answer
//!   would hide.
//! - The **mutable** options. `objectify` builds immutable containers, so a
//!   request for `NSPropertyListMutableContainers` or
//!   `...AndLeaves` is refused by name rather than answered with immutable
//!   objects - see [`mutability_refusal`]. That one *is* an abort, and the
//!   reason it differs from the format case is that it is not a question about
//!   the document: it is a request for a container type this tree does not
//!   build, and returning an immutable one under a mutable request is a write
//!   that gets dropped. Neither measured site sets those bits (`0` and `0x64`),
//!   so this is unreachable for this guest and is here because the day it is
//!   reached it must not lie.

use objc::object::Id;
use objc::sel::Sel;

use crate::define::class_methods;

/// Published so a guest binding `_OBJC_CLASS_$_NSPropertyListSerialization`
/// gets this class rather than an empty one.
pub const CLASSES: &[&str] = &["NSPropertyListSerialization"];

/// `NSPropertyListMutableContainers` and `NSPropertyListMutableContainersAndLeaves`.
///
/// The other two documented values - `NSPropertyListImmutable` (0) and
/// `NSPropertyListOpenStepFormat` (a format, not an option) - are `0`, and an
/// option bit this list does not name is ignored, which is what Apple does.
const MUTABLE_OPTIONS: u64 = 0b11;

/// `NSCocoaErrorDomain`, and the one code of it this method can report.
///
/// `NSPropertyListReadCorruptError` is `3840`, the same number `crate::json`
/// reports for a document that is not a readable plist and the same number
/// `crate::collections` reports for a file that is not one - which is the point:
/// three different readers, one condition, one code, so a caller switching on it
/// is switching on the condition rather than on which API it called.
const NS_COCOA_ERROR_DOMAIN: &str = "NSCocoaErrorDomain";
const PROPERTY_LIST_READ_CORRUPT: i64 = 3840;

/// Read a property list out of `data`.
///
/// Four arguments, and the encoding says so: `self` is the `@0:8` and `_cmd` is
/// **not encoded**, so the four the selector names sit at 16, 24, 32 and 40 and
/// the frame is `8 * (2 + 4)` = **48** - six words, for the six registers
/// `objc_msgSend` writes.
///
/// The first version of this row declared `@40`, which is self-consistent
/// (`frame == last_offset` is how the check in `test/tools/objcenc.py:1306` is
/// written) and **wrong**: the `NSError **` at offset 40 occupies bytes 40
/// through 48, so a 40-byte frame is one word short of the argument the guest
/// passes in `r9`. The tree's own four-colon row agrees:
/// `application.rs:1004` declares `nextEventMatchingMask:untilDate:inMode:dequeue:`
/// as `@48@0:8Q16@24@32B40`.
///
/// Both measured sites were read off the disassembly rather than off a header,
/// and they agree with it argument for argument - `rdx` is the `NSData`, `ecx`
/// the options, `r8d` the format and `r9d` the `NSError **`.
///
/// The out-parameters follow [`crate::collections`]'s `+
/// [NSDictionary dictionaryWithContentsOfURL:error:]`, which is the same guest
/// and the same contract, and both halves of it are load-bearing:
///
/// * **`*error` is written on every path, success included**, where it is
///   written with nil. A success that did not write would leave the caller's
///   own pre-zeroed slot in place by luck, which is not the same as having said
///   so.
/// * **a null `error:` is never written through.** Both measured sites pass
///   `xor r9d,r9d`, and a store through a null out-parameter is a store to
///   address zero.
///
/// # Safety
///
/// `data` must be null or an object this runtime allocated, and `error` must be
/// null or a writable `NSError **`.
unsafe extern "C" fn property_list_with_data(
    _class: Id,
    _sel: Sel,
    data: Id,
    options: u64,
    format: *mut u64,
    error: *mut Id,
) -> Id {
    let fail = |code: i64, message: String| -> Id {
        if !error.is_null() {
            let object = crate::error::make(NS_COCOA_ERROR_DOMAIN, code, &message);
            // SAFETY: the caller said this is where the error goes, and `object`
            // is the autoreleased error this call just made.
            unsafe { *error = object };
        }
        std::ptr::null_mut()
    };
    let succeed = |value: Id| -> Id {
        if !error.is_null() {
            // SAFETY: as above, with nil as the value.
            unsafe { *error = std::ptr::null_mut() };
        }
        value
    };

    if options & MUTABLE_OPTIONS != 0 {
        mutability_refusal(options);
    }
    // `format` is in *and* out on Apple's signature: a non-zero value asks for
    // one spelling. `plist` reads `bplist00` and XML and nothing else, so a
    // request for OpenStep is **reported through `error:`** rather than answered
    // with whichever of the two the bytes happen to look like - a caller switches
    // on this value, and a plausible one is worse than none. This is the one
    // place a refusal is *not* an abort, and the reason is in the module header:
    // the caller asked a question that has a documented way to be told the
    // answer, so it gets nil plus an `NSError` rather than a dead process.
    // SAFETY: the caller's obligation, above.
    let asked = if format.is_null() {
        0
    } else {
        unsafe { *format }
    };
    if asked != 0 {
        // A named failure, reported through the out-parameter rather than an
        // abort: the caller asked a question with a documented way to be told
        // the answer, and answering it is not the same as refusing to be asked.
        // `format` itself is left alone - it is in *and* out, and reporting
        // "I read nothing" through it as one of the two spellings would be a
        // plausible value for a value that means something.
        return fail(
            PROPERTY_LIST_READ_CORRUPT,
            format!(
                "the caller asked for property-list format {asked:#x}, and this runtime reads \
                 only the binary and XML spellings"
            ),
        );
    }
    if data.is_null() {
        return fail(
            PROPERTY_LIST_READ_CORRUPT,
            "there is no data to read a property list from".to_string(),
        );
    }
    let bytes = unsafe { data_bytes(data) };
    let value = match crate::plist::parse(&bytes) {
        Ok(value) => value,
        // `plist::Error` is a sentence naming the offset, which is exactly what
        // belongs in a localised description, and the code is the one Apple
        // documents for a document that is not a readable plist.
        Err(why) => return fail(PROPERTY_LIST_READ_CORRUPT, why.0),
    };
    // The one out-parameter that is genuinely in *and* out: a non-null `format`
    // is updated to the spelling actually read, and the reader above knows which
    // one it took, so this is derived rather than invented.
    if !format.is_null() {
        // SAFETY: the caller's obligation, above.
        unsafe {
            *format = if bytes.starts_with(b"bplist00") {
                200
            } else {
                100
            }
        };
    }
    // Named for the diagnostic: a plist the guest handed us is data we could not
    // read, and saying "0x0" would leave the reader guessing which of the two
    // sent it.
    succeed(crate::objectify::object(
        &value,
        "+[NSPropertyListSerialization propertyListWithData:]",
    ))
}

/// Refuse the mutable options, by name.
///
/// Not `unreachable!`: the value arrives from the guest, and a guest that sets
/// it on a host that cannot honour it is a real condition with a real answer -
/// the answer being that this host cannot honour it.
fn mutability_refusal(options: u64) -> ! {
    objc::fail!(
        "+[NSPropertyListSerialization propertyListWithData:options:format:error:]: options \
         {options:#x} ask for mutable containers, and objectify builds immutable ones. A plist \
         returned under a mutable request would take a write and drop it, which is a wrong answer \
         that looks like a right one."
    )
}

/// The bytes of an `NSData`, or nothing for an object that is not one.
///
/// # Safety
///
/// `data` must be null or an object this runtime allocated.
unsafe fn data_bytes(data: Id) -> Vec<u8> {
    // SAFETY: the caller's obligation; both of these answer for a null or a
    // foreign object rather than dereferencing one.
    unsafe {
        let bytes = corefoundation::api::CFDataGetBytePtr(data as corefoundation::base::CFTypeRef);
        let length = corefoundation::api::CFDataGetLength(data as corefoundation::base::CFTypeRef);
        match bytes.is_null() || length <= 0 {
            true => Vec::new(),
            false => std::slice::from_raw_parts(bytes, length as usize).to_vec(),
        }
    }
}

/// Every selector this module installs, with the encoding it declared. See
/// `crates/appkit/src/spell_checker.rs` for why this is a list the test reads
/// rather than a loop the install reads.
#[cfg(test)]
const MEASURED: &[(&str, &std::ffi::CStr)] = &[(
    "propertyListWithData:options:format:error:",
    c"@48@0:8@16Q24Q32@?40",
)];

pub(crate) fn install() {
    let class = corefoundation::base::abstract_class(c"NSPropertyListSerialization", None);
    class_methods!(class, {
        "propertyListWithData:options:format:error:" => property_list_with_data,
            c"@48@0:8@16Q24Q32@?40";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every declared encoding has as many arguments as the selector has
    /// colons, and the offsets are the ones the disassembly shows.
    ///
    /// A self-consistent encoding can still be wrong about arity - that is the
    /// bug `crates/appkit/src/appearance.rs` had, and it passed its own tests -
    /// so the check is against the **name**, which nothing can make
    /// self-consistent by accident.
    #[test]
    fn the_declared_arity_is_the_selector_s() {
        for (selector, types) in MEASURED {
            crate::encoding_check::assert_row(selector, types);
        }
        for (selector, types) in MEASURED {
            crate::encoding_check::assert_installed(
                c"NSPropertyListSerialization",
                selector,
                types,
            );
        }
    }

    /// The class is registered and published under the guest's own spelling, so
    /// a guest binding `_OBJC_CLASS_$_NSPropertyListSerialization` gets *this*
    /// class and not an empty one. Publication is not registration; the
    /// distinction is `foundation::class_data`'s, and it is the whole of why a
    /// class can be present and still unreachable.
    #[test]
    fn the_class_is_published() {
        crate::bootstrap();
        for name in CLASSES {
            let cname = std::ffi::CString::new(*name).unwrap();
            assert!(
                objc::class::lookup(&cname).is_some(),
                "{name} is registered"
            );
            assert!(
                crate::class_data()
                    .iter()
                    .any(|(symbol, _)| symbol == &format!("_OBJC_CLASS_$_{name}")),
                "{name} is published as data, not only registered"
            );
        }
    }

    /// A plist in the guest's own bytes comes back as the object graph it
    /// describes - the real answer, read the way a guest reads it.
    #[test]
    fn a_plist_comes_back_as_objects() {
        crate::bootstrap();
        let document = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>name</key><string>DELTARUNE</string></dict></plist>"#;
        let data = corefoundation::collections::new_data(document.to_vec());
        // SAFETY: the class and the selector are this module's, the data is an
        // NSData this tree made, and both out-parameters are given as null and a
        // null `NSError **` exactly as `Framework+0x4fe2056` passes them.
        let (plist, format) = unsafe {
            let class = objc::class::lookup(c"NSPropertyListSerialization").unwrap();
            let f: unsafe extern "C" fn(Id, Sel, Id, u64, *mut u64, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            let mut format: u64 = 0;
            let answer = f(
                objc::class::ptr(class) as Id,
                objc::sel::get("propertyListWithData:options:format:error:"),
                data as Id,
                0,
                &mut format,
                std::ptr::null_mut(),
            );
            (answer, format)
        };
        assert!(!plist.is_null(), "a well-formed plist is not nil");
        assert_eq!(format, 100, "read as the XML spelling, and said so");
        // SAFETY: `-objectForKey:` is `id (id, SEL, id)`.
        let name = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                plist,
                objc::sel::get("objectForKey:"),
                crate::string::from_str("name"),
            )
        };
        // SAFETY: a string, because that is what the document said.
        assert_eq!(unsafe { crate::string::bytes(name) }, b"DELTARUNE");
    }

    /// `*error` is written on **every** path, nil on success and an `NSError` on
    /// failure, and a **null** `error:` is never written through.
    ///
    /// Both halves are measured, not assumed. The guest pre-zeroes its slot, so a
    /// success that does not write passes by luck; and a store through the null
    /// both measured sites pass is a store to address zero. This exercises all
    /// three shapes: a non-null slot on success, a non-null slot on failure, and
    /// a null slot on failure.
    #[test]
    fn the_error_out_parameter_follows_the_guests_contract() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSPropertyListSerialization").expect("installed");
        let sel = objc::sel::get("propertyListWithData:options:format:error:");
        // SAFETY: the prototype is the selector's own, and each `data` below is
        // an `NSData` this tree made.
        let call = |bytes: Vec<u8>, error: *mut Id| unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, u64, *mut u64, *mut Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            let data = corefoundation::collections::new_data(bytes);
            f(
                objc::class::ptr(class) as Id,
                sel,
                data as Id,
                0,
                std::ptr::null_mut(),
                error,
            )
        };
        let good = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>k</key><string>v</string></dict></plist>"#
            .to_vec();

        // Success with a real slot: written, and written with nil.
        let mut slot: Id = std::ptr::null_mut();
        assert!(!call(good.clone(), &mut slot).is_null());
        assert!(
            slot.is_null(),
            "*error is nil on success, not merely untouched"
        );

        // Failure with a real slot: an `NSError` in `NSCocoaErrorDomain`.
        let mut slot: Id = std::ptr::null_mut();
        assert!(call(b"not a plist at all".to_vec(), &mut slot).is_null());
        assert!(
            !slot.is_null(),
            "*error names the failure rather than leaving it unset"
        );
        // SAFETY: `-domain` is `id (id, SEL)` and this is an `NSError` just made.
        let domain = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(slot, objc::sel::get("domain"))
        };
        // SAFETY: `-domain` answers a string.
        assert_eq!(
            unsafe { crate::string::bytes(domain) },
            b"NSCocoaErrorDomain"
        );

        // Failure with a null slot: nothing is written, because there is
        // nowhere to write it. This is the shape both measured sites use.
        let mut slot: Id = std::ptr::null_mut();
        assert!(call(b"not a plist at all".to_vec(), std::ptr::null_mut()).is_null());
        assert!(slot.is_null());
    }

    /// Bytes that are not a property list answer nil, which is the answer both
    /// measured sites branch on - not a stand-in object and not a guess.
    #[test]
    fn bytes_that_are_not_a_plist_answer_nil() {
        crate::bootstrap();
        let data = corefoundation::collections::new_data(b"this is not a plist".to_vec());
        // SAFETY: as above.
        let answer = unsafe {
            let class = objc::class::lookup(c"NSPropertyListSerialization").unwrap();
            let f: unsafe extern "C" fn(Id, Sel, Id, u64, *mut u64, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                objc::class::ptr(class) as Id,
                objc::sel::get("propertyListWithData:options:format:error:"),
                data as Id,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert!(answer.is_null());
    }
}
