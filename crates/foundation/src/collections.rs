use corefoundation::api;
use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
struct Predicate {
    isa: *const objc::class::Class,
    block: usize,
}

unsafe fn block_invoke(block: Id) -> Option<usize> {
    if block.is_null() {
        return None;
    }
    // SAFETY: `invoke` is the third word of every Objective-C block literal.
    let invoke = unsafe { *((block as *const u8).add(16) as *const usize) };
    (invoke != 0).then_some(invoke)
}

/// Report a mutation of a collection that was not created mutable.
///
/// A named abort rather than a wrong answer: an `NSArray` that silently accepted
/// `-addObject:` would hand a different object back to the next reader with
/// nothing anywhere to say why. Apple raises here.
#[cold]
fn immutable(what: &str, sel: &str) -> ! {
    objc::fail!("-[{what} {sel}]: the collection was not created mutable");
}

unsafe extern "C" fn count(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: an array.
    unsafe { api::CFArrayGetCount(receiver as CFTypeRef) as usize }
}

unsafe extern "C" fn object_at_index(receiver: Id, _sel: Sel, index: usize) -> Id {
    // SAFETY: an array.
    unsafe { api::CFArrayGetValueAtIndex(receiver as CFTypeRef, index as isize) as Id }
}

/// `-firstObject` and `-lastObject`, which answer nil for an empty array where
/// `-objectAtIndex:` on one is out of range. That difference is the reason a
/// caller uses them.
unsafe extern "C" fn first_object(receiver: Id, sel: Sel) -> Id {
    // SAFETY: an array; `-objectAtIndex:` is this module's own.
    unsafe {
        match count(receiver, sel) {
            0 => std::ptr::null_mut(),
            _ => object_at_index(receiver, sel, 0),
        }
    }
}

unsafe extern "C" fn last_object(receiver: Id, sel: Sel) -> Id {
    unsafe {
        match count(receiver, sel) {
            0 => std::ptr::null_mut(),
            n => object_at_index(receiver, sel, n - 1),
        }
    }
}

unsafe extern "C" fn sorted_array_using_selector(receiver: Id, _sel: Sel, _compare: Sel) -> Id {
    // Apps use this with NSString keys and `caseInsensitiveCompare:`. Keep
    // the ordering deterministic while avoiding guest callbacks during sort.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let mut values = array.values.clone();
    values.sort_by_cached_key(|value| {
        // SAFETY: this Foundation entry point is used with NSString arrays.
        unsafe { crate::string::bytes(*value as Id) }.to_ascii_lowercase()
    });
    // SAFETY: a fresh array follows the convenience-constructor rule.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &values,
            Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

/// `NSEnumerationReverse`. `NSEnumerationConcurrent` is a permission rather than
/// a promise, so walking in order honours it.
const ENUMERATION_REVERSE: usize = 1 << 1;

/// `NSNotFound`.
const NOT_FOUND: usize = isize::MAX as usize;

/// `-indexOfObjectWithOptions:passingTest:`: the first index whose object the
/// block accepts, walking backwards under `NSEnumerationReverse`.
unsafe extern "C" fn index_of_object_with_options_passing_test(
    receiver: Id,
    _sel: Sel,
    options: usize,
    block: Id,
) -> usize {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return NOT_FOUND;
    };
    // SAFETY: null or a block.
    let Some(invoke) = (unsafe { block_invoke(block) }) else {
        return NOT_FOUND;
    };
    // SAFETY: the block is `BOOL (^)(id, NSUInteger, BOOL *)`.
    let call: unsafe extern "C" fn(Id, Id, usize, *mut bool) -> bool =
        unsafe { std::mem::transmute(invoke) };
    // A copy, because the block may mutate the array it is testing.
    let values = array.values.clone();
    let order: Box<dyn Iterator<Item = usize>> = match options & ENUMERATION_REVERSE {
        0 => Box::new(0..values.len()),
        _ => Box::new((0..values.len()).rev()),
    };
    let mut stop = false;
    for at in order {
        // SAFETY: as above; `stop` outlives the call.
        if unsafe { call(block, values[at] as Id, at, &mut stop) } {
            return at;
        }
        // A block that stops without accepting anything found nothing.
        if stop {
            break;
        }
    }
    NOT_FOUND
}

unsafe extern "C" fn filtered_array_using_predicate(receiver: Id, _sel: Sel, predicate: Id) -> Id {
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    if predicate.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `predicateWithBlock:` creates this layout.
    let block = unsafe { (*(predicate as *const Predicate)).block as Id };
    let mut values = Vec::new();
    if let Some(invoke) = unsafe { block_invoke(block) } {
        // `NSPredicate` block predicates receive the object and a bindings
        // dictionary. There are no substitution variables in this operation.
        let call: unsafe extern "C" fn(Id, Id, Id) -> bool = unsafe { std::mem::transmute(invoke) };
        for value in &array.values {
            if unsafe { call(block, *value as Id, std::ptr::null_mut()) } {
                values.push(*value);
            }
        }
    }
    // SAFETY: a fresh array follows the convenience-constructor rule.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &values,
            Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

unsafe extern "C" fn predicate_with_block(_class: Id, _sel: Sel, block: Id) -> Id {
    let class = objc::class::lookup(c"NSPredicate").expect("NSPredicate is installed");
    // SAFETY: `Predicate` is the registered layout for this class.
    let predicate = unsafe { corefoundation::base::allocate::<Predicate>(class) };
    if predicate.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a fresh instance.
    unsafe { (*predicate).block = objc::block::copy(block) as usize };
    // SAFETY: a fresh object follows the convenience-constructor rule.
    unsafe { api::CFAutorelease(predicate as CFTypeRef) as Id }
}

unsafe extern "C" fn predicate_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a Predicate instance at the end of its lifetime.
    let block = unsafe { (*(receiver as *mut Predicate)).block as Id };
    objc::block::release(block);
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn add_object(receiver: Id, _sel: Sel, object: Id) {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    if !a.mutable {
        immutable("NSArray", "addObject:");
    }
    a.values.push(a.policy.retain(object as CFTypeRef));
}

unsafe extern "C" fn add_objects_from_array(receiver: Id, _sel: Sel, other: Id) {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    if !a.mutable {
        immutable("NSArray", "addObjectsFromArray:");
    }
    if other.is_null() {
        return;
    }
    // Cloned first: an array may be asked to add its own contents.
    // SAFETY: the argument is an array if it is one of ours.
    let values = match unsafe { collections::array(other as CFTypeRef) } {
        Some(o) => o.values.clone(),
        None => objc::fail!("-[NSMutableArray addObjectsFromArray:]: the argument is not an array"),
    };
    for value in values {
        a.values.push(a.policy.retain(value));
    }
}

unsafe extern "C" fn remove_object_at_index(receiver: Id, _sel: Sel, index: usize) {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    if !a.mutable {
        immutable("NSArray", "removeObjectAtIndex:");
    }
    if index < a.values.len() {
        let gone = a.values.remove(index);
        a.policy.release(gone);
    }
}

/// `-componentsJoinedByString:`, the other half of
/// `-[NSString componentsSeparatedByString:]`.
///
/// Every element is *sent* `-UTF8String` rather than read as a string, because
/// an array is entitled to hold anything and a guest joining an array of its own
/// objects expects each one's own answer. An element that answers nothing joins
/// as nothing, which is what a nil description would give.
unsafe extern "C" fn components_joined_by_string(receiver: Id, _sel: Sel, separator: Id) -> Id {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: null or a string.
    let separator = unsafe { crate::string::bytes(separator) };
    let mut out: Vec<u8> = Vec::new();
    for (i, value) in array.values.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(&separator);
        }
        // SAFETY: an element of a live array.
        out.extend_from_slice(&unsafe { crate::string::bytes(*value as Id) });
    }
    let joined = corefoundation::string::from_bytes(&out, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(joined) as Id }
}

/// One element's `-description`, for the container descriptions below.
///
/// The element is asked, rather than read as a string, because a container
/// holds whatever was put in it - a number, another container - and the answer
/// has to be that object's own text. An object that does not answer is written
/// the way Apple writes one with no description: its class and its address.
///
/// # Safety
///
/// `object` must be null or live.
unsafe fn describe(object: Id) -> Vec<u8> {
    if object.is_null() {
        return b"(null)".to_vec();
    }
    let sel = objc::sel::get("description");
    let described = (unsafe { objc::object::class_of(object) })
        .and_then(|class| class.method(sel))
        .map(|_| {
            // SAFETY: `-description` is `id (id, SEL)`.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel) -> Id =
                    std::mem::transmute(objc::msg::entry());
                f(object, sel)
            }
        })
        .filter(|described| !described.is_null());
    match described {
        // SAFETY: a string this object just answered with.
        Some(described) => unsafe { crate::string::bytes(described) },
        None => {
            let name = (unsafe { objc::object::class_of(object) })
                // SAFETY: a realized class names itself with a live C string.
                .map(|class| unsafe { std::ffi::CStr::from_ptr(class.name()) })
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| String::from("?"));
            format!("<{name}: {object:p}>").into_bytes()
        }
    }
}

/// `-[NSArray description]`, in Apple's bracketed layout.
///
/// A program logs a container to see what is in it, so the elements are what
/// this has to spell out; the exact whitespace is Apple's and matters to
/// nothing, but a reader comparing a log against a Mac's should not have to
/// translate.
unsafe extern "C" fn array_description(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let mut out: Vec<u8> = b"(\n".to_vec();
    for (i, value) in array.values.iter().enumerate() {
        out.extend_from_slice(b"    ");
        // SAFETY: an element of a live array.
        out.extend_from_slice(&unsafe { describe(*value as Id) });
        if i + 1 < array.values.len() {
            out.push(b',');
        }
        out.push(b'\n');
    }
    out.push(b')');
    let text = corefoundation::string::from_bytes(&out, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(text) as Id }
}

/// `-[NSDictionary description]`, in Apple's braced layout.
unsafe extern "C" fn dictionary_description(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a dictionary.
    let Some(dictionary) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let mut out: Vec<u8> = b"{\n".to_vec();
    for (key, value) in &dictionary.entries {
        out.extend_from_slice(b"    ");
        // SAFETY: a key of a live dictionary.
        out.extend_from_slice(&unsafe { describe(*key as Id) });
        out.extend_from_slice(b" = ");
        // SAFETY: a value of a live dictionary.
        out.extend_from_slice(&unsafe { describe(*value as Id) });
        out.extend_from_slice(b";\n");
    }
    out.push(b'}');
    let text = corefoundation::string::from_bytes(&out, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(text) as Id }
}

/// `+[NSData dataWithContentsOfFile:]`.
///
/// Nil for a file that cannot be read, which is Apple's answer and the one a
/// caller checks for - a program asking for a file it may not have is asking a
/// question, not making a mistake.
///
/// **One send in the guest**, at `Framework+0x4fe2033`, with
/// `_OBJC_CLASS_$_NSData` as the receiver.
unsafe extern "C" fn data_with_contents_of_file(class: Id, _sel: Sel, path: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let path = unsafe { crate::string::bytes(path) };
    let Ok(path) = String::from_utf8(path) else {
        return std::ptr::null_mut();
    };
    // The guest named it, so it is read at this host's spelling.
    let Ok(bytes) = std::fs::read(prefix::ns::host_path(&path)) else {
        return std::ptr::null_mut();
    };
    autoreleased_data(class, bytes)
}

/// `+[NSDictionary dictionaryWithContentsOfFile:]`.
///
/// This convenience constructor accepts XML and binary property lists through
/// the same parser used for bundle metadata. It answers nil when the file is
/// unreadable, malformed, or its root is not a dictionary.
unsafe extern "C" fn dictionary_with_contents_of_file(_class: Id, _sel: Sel, path: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let path = unsafe { crate::string::bytes(path) };
    let Ok(path) = String::from_utf8(path) else {
        return std::ptr::null_mut();
    };
    let Ok(bytes) = std::fs::read(prefix::ns::host_path(&path)) else {
        return std::ptr::null_mut();
    };
    let Ok(value) = crate::plist::parse(&bytes) else {
        return std::ptr::null_mut();
    };
    if !matches!(value, crate::plist::Value::Dictionary(_)) {
        return std::ptr::null_mut();
    }
    crate::objectify::object(&value, &path)
}

/// `NSCocoaErrorDomain`'s codes for the ways reading a property list can fail,
/// from the enumeration Apple documents as `NSCocoaErrorDomain`.
///
/// Each is chosen from the condition that actually happened rather than from one
/// catch-all, because a code that names the wrong cause is worse than one that
/// names none - a caller switches on it. `NSFileNoSuchFileError` is `4`,
/// `NSFileReadUnknownError` is `256`, `NSFileReadNoPermissionError` is `257`,
/// `NSFileReadUnsupportedSchemeError` is `262`, and
/// `NSPropertyListReadCorruptError` is `3840` - the last being the same number,
/// with the same meaning, that `crate::json` already reports for a document that
/// is not a readable plist.
///
/// Three more are needed by the *data* reader below, and they are the same
/// enumeration at the read end rather than at the operation end:
/// `NSFileReadInvalidFileNameError` is `258` and `NSFileReadNoSuchFileError` is
/// `260`. `260` rather than `4` is what a **read** of a missing file reports -
/// GNUstep's `getContentsOfFile` uses it, and so does
/// `+[NSData dataWithContentsOfFile:options:error:]`. The two numbers name the
/// same condition one level apart, and a caller switching on the code is
/// switching on which call failed.
mod cocoa {
    pub const FILE_NO_SUCH_FILE: i64 = 4;
    pub const FILE_READ_UNKNOWN: i64 = 256;
    pub const FILE_READ_NO_PERMISSION: i64 = 257;
    pub const FILE_READ_INVALID_FILE_NAME: i64 = 258;
    pub const FILE_READ_NO_SUCH_FILE: i64 = 260;
    pub const FILE_READ_UNSUPPORTED_SCHEME: i64 = 262;
    pub const PROPERTY_LIST_READ_CORRUPT: i64 = 3840;
}

/// `+[NSDictionary dictionaryWithContentsOfURL:error:]`: the two-argument
/// spelling of the same question `+dictionaryWithContentsOfFile:` answers, with
/// the out-parameter Apple's signature adds.
///
/// Both halves of the out-parameter's contract are load-bearing, and the second
/// is the one that is easy to get wrong:
///
/// * **`*error` is written on every path, including success** - where it is
///   written with nil. The guest pre-zeroes its slot at `0x470eec3`
///   (`mov QWORD PTR [rbp-0x80], 0x0`) and reads it back at `0x470eeed`, so a
///   success that did not write would leave the caller's own zero in place by
///   luck and anything else by a stack that happened to hold something.
/// * **a null `error:` is never written through.** Four of this guest's five
///   sites pass `xor ecx,ecx`. A null out-parameter means the caller does not
///   want to be told, and writing through it anyway is a store to whatever that
///   address happens to be.
///
/// Only a `file:` URL is read, which is what
/// `corefoundation::url::Contents::file_path` already refuses to answer for
/// anything else - so an `https:` URL is a *named* failure here rather than a
/// path component that would be opened.
unsafe extern "C" fn dictionary_with_contents_of_url_error(
    _class: Id,
    _sel: Sel,
    url: Id,
    error: *mut Id,
) -> Id {
    let fail = |code: i64, message: String| -> Id {
        // A null out-parameter means the caller does not want to be told.
        if !error.is_null() {
            let object = crate::error::make("NSCocoaErrorDomain", code, &message);
            // SAFETY: the caller said this is where the error goes, and `object`
            // is the autoreleased error this call just made.
            unsafe { *error = object };
        }
        std::ptr::null_mut()
    };

    // SAFETY: the caller's `NSURL`, or null.
    let contents = match unsafe { corefoundation::url::as_url(url.cast()) } {
        Some(contents) if contents.is_file_url() => contents,
        Some(contents) => {
            return fail(
                cocoa::FILE_READ_UNSUPPORTED_SCHEME,
                format!(
                    "a {} URL is not a file, and a property list is read from disk",
                    contents.scheme()
                ),
            );
        }
        None => {
            return fail(
                cocoa::FILE_READ_UNSUPPORTED_SCHEME,
                "not a URL this runtime can read a file from".to_string(),
            );
        }
    };
    let Some(path) = contents.file_path() else {
        return fail(
            cocoa::FILE_READ_UNSUPPORTED_SCHEME,
            "a file URL with no path".to_string(),
        );
    };
    // The guest named it, so it is read at this host's spelling.
    let bytes = match std::fs::read(prefix::ns::host_path(&path)) {
        Ok(bytes) => bytes,
        Err(problem) => {
            let code = match problem.kind() {
                std::io::ErrorKind::NotFound => cocoa::FILE_NO_SUCH_FILE,
                std::io::ErrorKind::PermissionDenied => cocoa::FILE_READ_NO_PERMISSION,
                _ => cocoa::FILE_READ_UNKNOWN,
            };
            return fail(code, format!("{path}: {problem}"));
        }
    };
    let value = match crate::plist::parse(&bytes) {
        Ok(value) => value,
        Err(problem) => {
            return fail(
                cocoa::PROPERTY_LIST_READ_CORRUPT,
                format!("{path}: {problem}"),
            );
        }
    };
    if !matches!(value, crate::plist::Value::Dictionary(_)) {
        return fail(
            cocoa::PROPERTY_LIST_READ_CORRUPT,
            format!("{path}: the property list's root is not a dictionary"),
        );
    }
    // Success writes nil through the out-parameter, which is the half of the
    // contract a caller that inspects its own slot depends on.
    if !error.is_null() {
        // SAFETY: the caller said this is where the error goes, and there is
        // none to report.
        unsafe { *error = std::ptr::null_mut() };
    }
    crate::objectify::object(&value, &path)
}

/// `-writeToFile:atomically:`.
///
/// The atomic flag is honoured by writing beside the target and renaming, which
/// is what makes it atomic - a program that asked for it and got a plain write
/// would have a half-written save file after a crash and nothing to say so.
unsafe extern "C" fn write_to_file(receiver: Id, _sel: Sel, path: Id, atomically: u8) -> u8 {
    // SAFETY: an `NSString`, or nil.
    let path = unsafe { crate::string::bytes(path) };
    let Ok(path) = String::from_utf8(path) else {
        return 0;
    };
    // SAFETY: a data object.
    let Some(bytes) = (unsafe { collections::data(receiver as CFTypeRef) }) else {
        return 0;
    };
    let path = prefix::ns::host_path(&path);
    if atomically == 0 {
        return std::fs::write(&path, &**bytes).is_ok() as u8;
    }
    // Appended, not `with_extension`, which would *replace* `.dat` and could
    // collide with a real neighbour.
    let mut temporary = path.clone().into_os_string();
    temporary.push(format!(".rosetta3-{}", std::process::id()));
    let temporary = std::path::PathBuf::from(temporary);
    if std::fs::write(&temporary, &**bytes).is_err() {
        return 0;
    }
    match std::fs::rename(&temporary, &path) {
        Ok(()) => 1,
        Err(_) => {
            let _ = std::fs::remove_file(&temporary);
            0
        }
    }
}

/// `-[NSDictionary valueForKey:]`, which is *not* the `NSObject` one.
///
/// A dictionary overrides key-value coding to mean its own contents: Apple's
/// answers `-objectForKey:` for any key not beginning with `@`. Without the
/// override the inherited implementation looks for a getter or an ivar of that
/// name, finds neither, and reports a class that in fact has the key.
unsafe extern "C" fn dictionary_value_for_key(receiver: Id, sel: Sel, key: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let name = unsafe { crate::string::bytes(key) };
    if name.first() == Some(&b'@') {
        // `@count` and its relatives are the operators, which address the
        // dictionary itself rather than its contents. Refused by name: an
        // operator answered as a missing key would be nil where a number was
        // expected.
        objc::fail!(
            "-[NSDictionary valueForKey:]: {} is a key-value coding operator, which this runtime \
             does not implement",
            String::from_utf8_lossy(&name)
        );
    }
    // SAFETY: `-objectForKey:` is `id (id, SEL, id)`.
    unsafe { object_for_key(receiver, sel, key) }
}

/// `-[NSMutableDictionary setValue:forKey:]`, whose nil means *remove*.
unsafe extern "C" fn dictionary_set_value_for_key(receiver: Id, sel: Sel, value: Id, key: Id) {
    match value.is_null() {
        // SAFETY: `-removeObjectForKey:` is `void (id, SEL, id)`.
        true => unsafe { remove_object_for_key(receiver, sel, key) },
        // SAFETY: `-setObject:forKey:` is `void (id, SEL, id, id)`.
        false => unsafe { set_object_for_key(receiver, sel, value, key) },
    }
}

/// `NSFastEnumerationState`, which the guest allocates on its own stack.
///
/// The layout is ABI: `for (id x in collection)` is compiled by clang into a
/// call to `-countByEnumeratingWithState:objects:count:` and then a walk of
/// `items_ptr`, so a field at the wrong offset is a loop over the wrong memory.
#[repr(C)]
struct FastEnumerationState {
    state: usize,
    items: *mut Id,
    mutations: *mut usize,
    extra: [usize; 5],
}

/// A word every collection points `mutations` at.
///
/// The compiler reads it before and after the loop body and raises if it
/// changed, which is how a program is told it mutated a collection while
/// enumerating it. One shared word means this runtime never reports that - it
/// has no per-collection version counter - so the check always passes. The
/// alternative is pointing at the collection's own storage, whose value changes
/// for reasons that are not mutations and would raise on a correct program.
static NEVER_MUTATED: usize = 0;

/// `-countByEnumeratingWithState:objects:count:`, for anything whose elements
/// are a contiguous vector.
///
/// One pass: everything is already in one buffer, so the first call hands the
/// guest a pointer straight into it and the second says there is no more. That
/// is what Apple's array does too, and it is why `buffer` is untouched - it is
/// there for a collection that has to *materialise* its elements, which none of
/// these do.
///
/// # Safety
///
/// `state` must be a writable `NSFastEnumerationState`, and `values` must
/// outlive the loop - which it does, because a mutation during enumeration is
/// the program's error and the one this cannot detect.
unsafe fn enumerate(state: *mut FastEnumerationState, values: &mut [CFTypeRef]) -> usize {
    if state.is_null() {
        return 0;
    }
    let state = unsafe { &mut *state };
    if state.state != 0 {
        return 0;
    }
    state.state = 1;
    state.items = values.as_mut_ptr() as *mut Id;
    state.mutations = &NEVER_MUTATED as *const usize as *mut usize;
    values.len()
}

unsafe extern "C" fn array_enumerate(
    receiver: Id,
    _sel: Sel,
    state: *mut FastEnumerationState,
    _buffer: *mut Id,
    _len: usize,
) -> usize {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return 0;
    };
    // SAFETY: the guest's own state structure, and storage that outlives the
    // loop.
    unsafe { enumerate(state, &mut array.values) }
}

/// The dictionary's, which enumerates **keys** - as `for (id key in dict)` does.
///
/// A dictionary's keys are not contiguous here: it is a vector of *pairs*. So
/// this fills the caller's buffer a batch at a time, which is what that argument
/// is for and the reason the protocol has one. Copying them into a buffer of
/// this object's own would either leak it - a `break` means the second call
/// never comes - or need a lifetime nothing here can give it.
///
/// `state` is the cursor: zero on entry, and the index of the next key after
/// that.
unsafe extern "C" fn dictionary_enumerate(
    receiver: Id,
    _sel: Sel,
    state: *mut FastEnumerationState,
    buffer: *mut Id,
    len: usize,
) -> usize {
    // SAFETY: a dictionary.
    let Some(dictionary) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return 0;
    };
    if state.is_null() || buffer.is_null() || len == 0 {
        return 0;
    }
    // SAFETY: the guest's own state structure.
    let state = unsafe { &mut *state };
    let from = state.state;
    let batch = dictionary.entries.len().saturating_sub(from).min(len);
    if batch == 0 {
        return 0;
    }
    for i in 0..batch {
        // SAFETY: the caller said `buffer` holds `len` items, and `batch` is at
        // most that.
        unsafe { *buffer.add(i) = dictionary.entries[from + i].0 as Id };
    }
    state.state = from + batch;
    state.items = buffer;
    state.mutations = &NEVER_MUTATED as *const usize as *mut usize;
    batch
}

unsafe extern "C" fn remove_all_objects(receiver: Id, _sel: Sel) {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    if !a.mutable {
        immutable("NSArray", "removeAllObjects");
    }
    for value in a.values.drain(..) {
        a.policy.release(value);
    }
}

unsafe extern "C" fn contains_object(receiver: Id, _sel: Sel, object: Id) -> bool {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return false;
    };
    a.values
        .iter()
        .any(|v| a.policy.equal(*v, object as CFTypeRef))
}

unsafe extern "C" fn new_array(_class: Id, _sel: Sel) -> Id {
    collections::new_array(&[], Policy::from_type_callbacks(), true) as Id
}

unsafe extern "C" fn array_fn(_class: Id, _sel: Sel) -> Id {
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &[],
            Policy::from_type_callbacks(),
            true,
        )) as Id
    }
}

unsafe extern "C" fn array_with_capacity(_class: Id, _sel: Sel, _capacity: usize) -> Id {
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &[],
            Policy::from_type_callbacks(),
            true,
        )) as Id
    }
}

/// Whether a cluster's convenience constructor was sent to its mutable class.
///
/// Apple declares `+arrayWithObject:` and `+dictionaryWithObject:forKey:` on the
/// immutable class, and the mutable class inherits them - so one method answers
/// for both, and only the receiver says which was asked for. A program that
/// sends one to `NSMutableDictionary` writes to what comes back, and a
/// collection made immutable there fails on the next line.
fn mutable_receiver(class: Id, mutable: &std::ffi::CStr) -> bool {
    // SAFETY: the receiver of a class method is a class this runtime knows.
    let Some(class) = (unsafe { objc::class::from_ptr(class as *const objc::class::Class) }) else {
        return false;
    };
    class.chain().any(|c| {
        // SAFETY: a realized class names itself with a live C string.
        unsafe { std::ffi::CStr::from_ptr(c.name()) == mutable }
    })
}

unsafe extern "C" fn array_with_object(class: Id, _sel: Sel, object: Id) -> Id {
    // SAFETY: a fresh object holding a live one.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &[object as CFTypeRef],
            Policy::from_type_callbacks(),
            mutable_receiver(class, c"NSMutableArray"),
        )) as Id
    }
}

unsafe extern "C" fn array_with_array(_class: Id, _sel: Sel, other: Id) -> Id {
    let values: Vec<CFTypeRef> = if other.is_null() {
        Vec::new()
    } else {
        // SAFETY: the argument is an array if it is one of ours.
        match unsafe { collections::array(other as CFTypeRef) } {
            Some(a) => a.values.clone(),
            None => objc::fail!("+[NSMutableArray arrayWithArray:]: the argument is not an array"),
        }
    };
    // SAFETY: a fresh object holding live ones.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &values,
            Policy::from_type_callbacks(),
            true,
        )) as Id
    }
}

/// `+arrayWithObjects:count:`, which is the counted spelling
/// `objc::dispatch` rewrites the nil-terminated variadic into.
unsafe extern "C" fn array_with_objects_count(
    class: Id,
    _sel: Sel,
    objects: *const Id,
    count: usize,
) -> Id {
    if objects.is_null() && count > 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller says `objects` covers `count` entries.
    let slice = match count {
        0 => &[][..],
        _ => unsafe { std::slice::from_raw_parts(objects as *const CFTypeRef, count) },
    };
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_array(
            slice,
            Policy::from_type_callbacks(),
            mutable_receiver(class, c"NSMutableArray"),
        )) as Id
    }
}

unsafe extern "C" fn dictionary_count(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: a dictionary.
    unsafe { api::CFDictionaryGetCount(receiver as CFTypeRef) as usize }
}

unsafe extern "C" fn object_for_key(receiver: Id, _sel: Sel, key: Id) -> Id {
    // SAFETY: a dictionary and a key.
    unsafe { api::CFDictionaryGetValue(receiver as CFTypeRef, key as CFTypeRef) as Id }
}

unsafe extern "C" fn set_object_for_key(receiver: Id, _sel: Sel, object: Id, key: Id) {
    // SAFETY: a dictionary.
    let Some(d) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return;
    };
    if !d.mutable {
        immutable("NSDictionary", "setObject:forKey:");
    }
    d.set(key as CFTypeRef, object as CFTypeRef);
}

unsafe extern "C" fn remove_object_for_key(receiver: Id, _sel: Sel, key: Id) {
    // SAFETY: a dictionary.
    let Some(d) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return;
    };
    if !d.mutable {
        immutable("NSDictionary", "removeObjectForKey:");
    }
    if let Some(at) = d.position(key as CFTypeRef) {
        let (key, value) = d.entries.remove(at);
        d.keys.release(key);
        d.values.release(value);
    }
}

/// `-allKeys`, which is an `NSArray` of the keys in insertion order.
unsafe extern "C" fn all_keys(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a dictionary.
    let Some(d) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let keys: Vec<CFTypeRef> = d.entries.iter().map(|(k, _)| *k).collect();
    // SAFETY: a fresh object holding live ones.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &keys,
            Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

unsafe extern "C" fn all_values(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a dictionary.
    let Some(d) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let values: Vec<CFTypeRef> = d.entries.iter().map(|(_, v)| *v).collect();
    // SAFETY: a fresh object holding live ones.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &values,
            Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

/// `-init` on `NSMutableArray` and `NSMutableDictionary`, which are class
/// clusters.
///
/// `+alloc` on one of those answers an instance of the abstract class itself -
/// an isa and nothing else - because that is all an abstract class has room
/// for. Every read and every write goes through the contents a concrete
/// `__NSCF*` object carries, so a placeholder handed back from `-init` answers
/// empty to every question and swallows every write, silently: nothing here is
/// wrong enough to fail on, and the program carries on with a collection that
/// never fills. Apple's clusters substitute here too, which is why `-init` is
/// documented there as returning "an object other than the receiver".
///
/// # Safety
///
/// `receiver` is null or an instance of the class this was installed on, and
/// `concrete` answers a fresh object with one reference.
unsafe fn cluster_init(receiver: Id, known: bool, concrete: impl FnOnce() -> CFTypeRef) -> Id {
    if receiver.is_null() || known {
        // Already concrete: `-init` on one of those is the identity, and
        // replacing it would throw away contents the caller just made.
        return receiver;
    }
    let object = concrete() as Id;
    // The placeholder's one reference was the caller's, and it belongs to what
    // is returned in its place.
    objc::arc::release(receiver);
    object
}

unsafe extern "C" fn init_mutable_array(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an object of this class.
    let known = unsafe { collections::array(receiver as CFTypeRef) }.is_some();
    // SAFETY: as above, with a fresh empty array.
    unsafe {
        cluster_init(receiver, known, || {
            collections::new_array(&[], Policy::from_type_callbacks(), true)
        })
    }
}

/// `-initWithCapacity:`, where the capacity is a hint this store has no use for.
unsafe extern "C" fn init_mutable_array_with_capacity(
    receiver: Id,
    sel: Sel,
    _capacity: usize,
) -> Id {
    unsafe { init_mutable_array(receiver, sel) }
}

unsafe extern "C" fn init_mutable_dictionary(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an object of this class.
    let known = unsafe { collections::dictionary(receiver as CFTypeRef) }.is_some();
    // SAFETY: as above, with a fresh empty dictionary.
    unsafe {
        cluster_init(receiver, known, || {
            collections::new_dictionary(
                &[],
                Policy::from_key_callbacks(),
                Policy::from_type_callbacks(),
                true,
            )
        })
    }
}

unsafe extern "C" fn init_mutable_dictionary_with_capacity(
    receiver: Id,
    sel: Sel,
    _capacity: usize,
) -> Id {
    unsafe { init_mutable_dictionary(receiver, sel) }
}

unsafe extern "C" fn new_dictionary(_class: Id, _sel: Sel) -> Id {
    collections::new_dictionary(
        &[],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        true,
    ) as Id
}

unsafe extern "C" fn dictionary(_class: Id, _sel: Sel) -> Id {
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(new_dictionary(_class, _sel) as CFTypeRef) as Id }
}

unsafe extern "C" fn dictionary_with_capacity(_class: Id, _sel: Sel, _capacity: usize) -> Id {
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(new_dictionary(_class, _sel) as CFTypeRef) as Id }
}

/// `+dictionaryWithObject:forKey:`, the one-pair spelling.
unsafe extern "C" fn dictionary_with_object_for_key(
    class: Id,
    _sel: Sel,
    object: Id,
    key: Id,
) -> Id {
    if object.is_null() || key.is_null() {
        return std::ptr::null_mut();
    }
    let dictionary = collections::new_dictionary(
        &[(key as CFTypeRef, object as CFTypeRef)],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        mutable_receiver(class, c"NSMutableDictionary"),
    );
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(dictionary) as Id }
}

/// `+dictionaryWithObjects:forKeys:count:`, the counted spelling.
unsafe extern "C" fn dictionary_with_objects_for_keys_count(
    class: Id,
    _sel: Sel,
    objects: *const Id,
    keys: *const Id,
    count: usize,
) -> Id {
    if (objects.is_null() || keys.is_null()) && count > 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller says both arrays cover `count` entries.
    let pairs: Vec<(CFTypeRef, CFTypeRef)> = match count {
        0 => Vec::new(),
        _ => unsafe {
            std::slice::from_raw_parts(keys as *const CFTypeRef, count)
                .iter()
                .copied()
                .zip(
                    std::slice::from_raw_parts(objects as *const CFTypeRef, count)
                        .iter()
                        .copied(),
                )
                .collect()
        },
    };
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_dictionary(
            &pairs,
            Policy::from_key_callbacks(),
            Policy::from_type_callbacks(),
            mutable_receiver(class, c"NSMutableDictionary"),
        )) as Id
    }
}

/// An autoreleased data object holding `bytes`, which every `+` constructor
/// below answers.
///
/// **Whether it may be changed is the class it was sent to**, because all five
/// of these constructors are declared on `NSData` and inherited by
/// `NSMutableData`, so one method answers for both and only the receiver says
/// which was asked for.  That is [`mutable_receiver`]'s argument, verbatim from
/// the array: `+[NSMutableArray arrayWithObject:]` is the same case, and
/// `+[NSMutableData data]` is a mutable data on a Mac for the same reason
/// `+[NSMutableArray array]` is a mutable array - the constructor is
/// `[[self alloc] init]` and `self` is what changed.
fn autoreleased_data(class: Id, bytes: Vec<u8>) -> Id {
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(new_data_for(class, bytes)) as Id }
}

/// A data object for a `+` constructor that `NSMutableData` inherits.
///
/// The two CoreFoundation constructors are `new_data` and `new_mutable_data`,
/// the same pair `new_set` and `new_mutable_set` are; which one this call wants
/// is decided here, beside [`mutable_receiver`], because the answer is a
/// property of the *receiver class* and not of the bytes.
fn new_data_for(class: Id, bytes: Vec<u8>) -> CFTypeRef {
    match mutable_receiver(class, c"NSMutableData") {
        true => collections::new_mutable_data(bytes),
        false => collections::new_data(bytes),
    }
}

/// `+[NSData data]`: an empty, autoreleased data object.
///
/// **Four sends in the guest** (`Framework+[0x3a92716, 0x3a92984, 0x68fde68,
/// 0x68fe318)`), and all four load `_OBJC_CLASS_$_NSData` into `%rdi` first, so
/// this is a class method; three of them call
/// `_objc_retainAutoreleasedReturnValue` on the answer and the fourth tail-calls
/// into a block that does the same, so an autoreleased answer is what the caller
/// is built for.
///
/// (Three of the four have the selector load within a byte of the send; the
/// fourth, `0x3a92984`, loads it into a stack slot at `0x3a92915` and sends it
/// later, which is why a "selector load followed by a call inside 96 bytes"
/// census reports three here and not four.)
///
/// Apple hands back a *shared* immutable empty instance, so two `+data` results
/// are `==` there and are not here. Nothing in the guest can observe that - the
/// only comparison available on an `NSData` in this runtime is `NSObject`'s
/// pointer identity - and a process-lifetime object would be a claim about
/// sharing that nothing here needs, so this answers a fresh empty one.
unsafe extern "C" fn data_empty(class: Id, _sel: Sel) -> Id {
    autoreleased_data(class, Vec::new())
}

// `data_contents` went with the fifteen above: its only callers were
// `-isEqual:`, `+dataWithData:`, `-initWithData:` and `-copy`, and the merge's own
// `data_get_bytes`/`data_hash` never came across. Nothing here answers a data's
// bytes any more, so the `+alloc` placeholder note in `data_length` is the last
// thing that touches storage directly.
/// Non-null `bytes` covers `length` bytes.
unsafe fn raw<'a>(bytes: *const u8, length: usize) -> &'a [u8] {
    match bytes.is_null() || length == 0 {
        true => &[],
        false => unsafe { std::slice::from_raw_parts(bytes, length) },
    }
}

/// Every `-init...` below answers a new object in place of the one `+alloc`
/// made, as the string initialisers do: data here is a CoreFoundation object
/// whatever class was asked for, and one class serves both mutabilities.
fn replace_receiver(receiver: Id, bytes: Vec<u8>) -> Id {
    objc::arc::release(receiver);
    collections::new_data(bytes) as Id
}

unsafe extern "C" fn data_init_with_bytes(
    receiver: Id,
    _sel: Sel,
    bytes: *const u8,
    length: usize,
) -> Id {
    // SAFETY: the caller says `bytes` covers `length` bytes.
    replace_receiver(receiver, unsafe { raw(bytes, length) }.to_vec())
}

// The three `+` constructors below take `class` rather than `_class` and thread
// it into [`autoreleased_data`], so a `+` constructor that `NSMutableData`
// inherits builds a **mutable** data. Upstream's versions of these take the class
// and discard it, which is why the merge produced a second one-argument
// `autoreleased_data` beside ours; that one is gone, and this is the note for
// whoever merges next. `+[NSData data]` is the fourth and lives above with its
// own census note. The answer is a property of the *receiver class* and not of
// the bytes, which is the same reason `new_data_for` exists beside
// `mutable_receiver`.
/// `-copyWithZone:`, which is also what `-[NSObject copy]` sends.
///
/// **The zone is ignored.** The one direct send in the guest passes nil
/// (`Framework+0x79364e0: xor edx,edx`), and this runtime has no `NSZone` to
/// honour - a zone chooses an allocator, and the storage behind an `NSData` here
/// is a `Vec` that owns itself.
///
/// **The answer is the receiver for an immutable data and a new immutable one for
/// a mutable data**, which is what Apple does, and it can be answered because
/// the body carries the bit: `collections::DataContents::mutable`, the same field
/// an array's `ArrayContents` has and for Apple's own stated reason - `CFData.h`
/// declares *one* `__CFData` type, `const` in `CFDataRef` and not in
/// `CFMutableDataRef`, so the distinction belongs to the reference and a runtime
/// with one class has nowhere else to keep it. Two independent implementations
/// agree on the split: GNUstep's `-copyWithZone:` returns `RETAIN(self)` and
/// swift-corelibs' `copy(with:)` returns `self`, and both return a *new* object
/// from `-mutableCopy`.
///
/// **Which branch the guest takes is measured, not assumed.** Its single
/// `copyWithZone:` send is at `Framework+0x79364e3` - `call r13`, with `rsi`
/// loaded from the selref at `0x79364d6` and `edx` cleared at `0x79364e0` - and
/// its receiver `%rdi` is `%r12`, which `0x79364b3` set from
/// `+[NSData dataWithBytes:length:]` sent at `0x79364a8` (`mov rdi,[rip+0xe6e72c0]`,
/// which is `_OBJC_CLASS_$_NSData`). So the guest takes the **receiver** branch,
/// and while there was no bit to read it was answered a fresh copy: the same
/// bytes under a different object, and an O(n) copy where Apple is O(1).
///
/// The mutable branch is not reachable from this guest - the census note on
/// `-mutableCopy` below says why - so what exercises it is the test, and what
/// makes it *right* is that `CFDataCreateMutable` and the `+` constructors a
/// guest can send to `NSMutableData` both produce a data with the bit set.
unsafe extern "C" fn data_copy_with_zone(receiver: Id, _sel: Sel, _zone: *mut std::ffi::c_void) -> Id {
    // SAFETY: a data object.
    let Some(d) = (unsafe { collections::data_contents(receiver as CFTypeRef) }) else {
        // A `+alloc` placeholder: an isa and no storage, so there is no
        // mutability to read and no bytes to copy, and an empty data is what a
        // copy of no bytes is. Not reachable from the guest, whose one site has
        // a concrete data as its receiver.
        return collections::new_data(Vec::new()) as Id;
    };
    // SAFETY: as above; the body is this runtime's.
    if !d.mutable {
        // **Not** autoreleased: `-copyWithZone:` is in the `copy`/`init`/`new`
        // family, so the answer is +1 and belongs to the caller. The guest's one
        // site uses the return value straight away with no
        // `_objc_retainAutoreleasedReturnValue`, which is what a +1 answer looks
        // like.
        return objc::arc::retain(receiver);
    }
    // A mutable data's copy is a **new immutable** one, and the bytes are cloned
    // rather than shared, so a later `-appendBytes:length:` on either object
    // cannot reach the other.
    collections::new_data(d.bytes.clone()) as Id
}

/// `-[NSData getBytes:length:]` - **not** upstream's `-[NSData getBytes:range:]`.
///
/// The merge renamed this function and kept its body, which is the reverse of
/// what happened: the body below has always been `getBytes:length:`'s, and
/// `getBytes:length:` is the one the guest sends (one site,
/// `Framework+0x7a1f094`, with the two argument pairs its `raw-range.py`
/// disassembly turned up). `getBytes:range:` is the *other* selector, it has
/// **0** send sites, and the function that used to carry its name was not
/// registered anywhere - so it was dead code sitting on top of a live method,
/// and the name is restored here rather than the body being deleted. See the
/// census table in `install()` for the rest of what is deliberately absent.
unsafe extern "C" fn data_get_bytes_length(
    receiver: Id,
    _sel: Sel,
    buffer: *mut u8,
    length: usize,
) {
    // SAFETY: a data object; a `+alloc` placeholder has no storage, and its
    // `-length` is 0, so it clamps to 0 here too rather than inventing bytes.
    let count = match unsafe { collections::data(receiver as CFTypeRef) } {
        Some(bytes) => length.min(bytes.len()),
        None => 0,
    };
    if count == 0 {
        return;
    }
    if buffer.is_null() {
        objc::fail!("-[NSData getBytes:length:]: {count} byte(s) asked for and no buffer to put them in");
    }
    // SAFETY: `count` is at most the receiver's own length, and the caller said
    // the buffer holds `length` bytes, which is at least `count`.
    unsafe { api::CFDataGetBytes(receiver as CFTypeRef, 0, count as isize, buffer) };
}

/// `+dataWithContentsOfFile:options:error:`, the spelling with the
/// out-parameter.
///
/// **One send in the guest**, at `Framework+0x4fe0237`, and every half of the
/// contract is readable off it:
///
/// * `mov QWORD PTR [rbp-0x80], 0x0` then `lea r8, [rbp-0x80]` - the caller
///   pre-zeroes its own `NSError *` and then passes its address, so `*error` is
///   read back after the call. It is written on **every** path here, with nil on
///   success, rather than left to the caller's zero;
/// * `xor ecx,ecx` - the options are **zero** at the only site, so no reading
///   option is in use;
/// * `_objc_retainAutoreleasedReturnValue` on the answer, so the answer is
///   autoreleased;
/// * `test r14, r14; je 0x4fe04c1` at `0x4fe0265` - **the result is nil-checked**
///   before `-length` is sent to it and before it is handed to
///   `+[NSKeyedUnarchiver initForReadingFromData:error:]`. So a truthful nil for a
///   file that cannot be read is a path the guest has, not a crash.
///
/// The options are **not** honoured, and that is a statement about `what` rather
/// than `how`: all four `NSDataReadingOptions` bits tune how the file is reached
/// (cached, uncached, mapped), and the bytes answered are the file's either way.
/// The one difference a caller could observe is `NSDataReadingMappedAlways`, whose
/// answer on a Mac is backed by the mapping and so reflects a later change to the
/// file; nothing in this runtime maps a file, so the answer is a snapshot.
unsafe extern "C" fn data_with_contents_of_file_options(
    class: Id,
    _sel: Sel,
    path: Id,
    _options: u64,
    error: *mut Id,
) -> Id {
    // A null out-parameter means the caller does not want to be told, and
    // writing through it is a store to whatever that address happens to be.
    let fail = |code: i64, message: String| -> Id {
        if !error.is_null() {
            // SAFETY: the caller said this is where the error goes, and
            // `error::make` answers an autoreleased error this call owns.
            unsafe { *error = crate::error::make("NSCocoaErrorDomain", code, &message) };
        }
        std::ptr::null_mut()
    };
    // Success writes nil through the out-parameter, which is the half of the
    // contract a caller that inspects its own slot depends on.
    let succeeded = |data: Id| -> Id {
        if !error.is_null() {
            // SAFETY: the caller said this is where the error goes, and there is
            // none to report.
            unsafe { *error = std::ptr::null_mut() };
        }
        data
    };

    // A path that is not a string, and a nil path, are the same failure and it is
    // not "no such file": it is a name this call cannot even read. Read through
    // `as_string` rather than `crate::string::bytes`, which answers an empty
    // string for both and would report a nil path as a missing one.
    // SAFETY: an `NSString` or null, both of which `as_string` answers for.
    let Some(name) = (unsafe { corefoundation::string::as_string(path as CFTypeRef) }) else {
        return fail(
            cocoa::FILE_READ_INVALID_FILE_NAME,
            "the path is null, or not a string".to_string(),
        );
    };
    let name = name.utf8();
    let Ok(name) = String::from_utf8(name) else {
        return fail(
            cocoa::FILE_READ_INVALID_FILE_NAME,
            "the path is not text this runtime can spell".to_string(),
        );
    };
    // The guest named it, so it is read at this host's spelling.
    let bytes = match std::fs::read(prefix::ns::host_path(&name)) {
        Ok(bytes) => bytes,
        Err(problem) => {
            // Each code names the condition that actually happened. A directory
            // and a path that is not there are different failures and a caller
            // switches on the code.
            let code = match problem.kind() {
                std::io::ErrorKind::NotFound => cocoa::FILE_READ_NO_SUCH_FILE,
                std::io::ErrorKind::PermissionDenied => cocoa::FILE_READ_NO_PERMISSION,
                _ => cocoa::FILE_READ_UNKNOWN,
            };
            return fail(code, format!("{name}: {problem}"));
        }
    };
    succeeded(autoreleased_data(class, bytes))
}

unsafe extern "C" fn data_length(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: a data object.
    unsafe { api::CFDataGetLength(receiver as CFTypeRef) as usize }
}

unsafe extern "C" fn data_bytes(receiver: Id, _sel: Sel) -> *const u8 {
    // SAFETY: a data object.
    unsafe { api::CFDataGetBytePtr(receiver as CFTypeRef) }
}

/// `+[NSData dataWithBytes:length:]`.
///
/// **Twenty-four sends in the guest** (23 in the Framework, 1 in `libGLESv2`),
/// every one of them to `_OBJC_CLASS_$_NSData` as the receiver of a `+` method,
/// and the lengths are real: `mov ecx,0x20` (32) at `0x79364a0`, `mov ecx,2` at
/// `0x7a1ef62`, and a `lea rdx,[r12]` that names a caller-owned buffer. So the
/// copy has to be exact in both directions.
unsafe extern "C" fn data_with_bytes(class: Id, _sel: Sel, bytes: *const u8, length: usize) -> Id {
    // `CFDataCreate`'s own answer for a null pointer and a non-zero length,
    // which is null rather than a data of invented bytes.
    if bytes.is_null() && length > 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller says `bytes` covers `length` bytes, checked just above.
    let copy = match length {
        0 => Vec::new(),
        _ => unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec(),
    };
    autoreleased_data(class, copy)
}

/// `+[NSData dataWithBytesNoCopy:length:]` with an owned compatibility copy.
///
/// CoreFoundation's data storage owns a `Vec`, so use the same compatibility
/// behavior as `CFDataCreateWithBytesNoCopy` rather than borrowing guest memory.
///
/// **Not sent by this guest** - the selector has no `__objc_selrefs` slot in any
/// of the 13 images and its name appears nowhere outside `__TEXT,__objc_methname`
/// - so it is here because it is a declared `NSData` constructor and costs one
/// row, not because anything needs it.
unsafe extern "C" fn data_with_bytes_no_copy(
    class: Id,
    _sel: Sel,
    bytes: *const u8,
    length: usize,
) -> Id {
    if bytes.is_null() && length > 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller says `bytes` covers `length` bytes, checked just above.
    let copy = match length {
        0 => Vec::new(),
        _ => unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec(),
    };
    autoreleased_data(class, copy)
}

/// `-[NSMutableData appendBytes:length:]`, and the one place the mutability bit
/// is *enforced*.
///
/// Apple declares it on `NSMutableData` alone, and a data that is not mutable is
/// not entitled to answer it at all.  Here it is reachable from an immutable one
/// - `__NSCFData` descends from `NSMutableData`, or a guest's `isKindOfClass:`
/// would be wrong - so the bit is what decides, and a data without it is refused
/// **by name** rather than quietly grown.  That is the array's convention
/// (`immutable("NSArray", "addObject:")`) and Apple's own behaviour: on a Mac the
/// message raises `NSInvalidArgumentException`.
///
/// **Not sent by this guest**: `appendBytes:length:` has no `__objc_selrefs` slot
/// in any of the 13 images, no RIP-relative reference to one from any section, and
/// its name appears nowhere outside `__TEXT,__objc_methname` - so it cannot even
/// be built at run time through `NSSelectorFromString`, which this Framework does
/// import.  So this refusal cannot break a live path, and leaving the write
/// unguarded could not have helped one either.
unsafe extern "C" fn append_bytes(receiver: Id, _sel: Sel, bytes: *const u8, length: usize) {
    // SAFETY: a data object.
    let Some(d) = (unsafe { collections::data_contents(receiver as CFTypeRef) }) else {
        return;
    };
    if !d.mutable {
        immutable("NSMutableData", "appendBytes:length:");
    }
    if bytes.is_null() {
        return;
    }
    // SAFETY: the caller says `bytes` covers `length` bytes.
    d.bytes
        .extend_from_slice(unsafe { std::slice::from_raw_parts(bytes, length) });
}

/// `-[NSMutableData mutableBytes]`, which is a `void *` **into** the receiver.
///
/// The writeable counterpart of `-bytes`, so it is the second thing the bit has
/// to guard: handing out a pointer into an immutable data's storage would let a
/// caller change a data this runtime has already said cannot change, silently.
///
/// Apple declares it on `NSMutableData` alone, so an immutable data refuses it
/// **by name** for the same reason `-appendBytes:length:` does.  Not sent by this
/// guest either - `mutableBytes` has a `__objc_methname` string and **no**
/// `__objc_selrefs` slot in any of the 13 images.
unsafe extern "C" fn mutable_bytes(receiver: Id, _sel: Sel) -> *mut u8 {
    // SAFETY: a data object.
    let Some(d) = (unsafe { collections::data_contents(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    if !d.mutable {
        immutable("NSMutableData", "mutableBytes");
    }
    // An empty data has no byte to point at, and `Vec::as_mut_ptr` on a `Vec`
    // that never allocated is address 1 - a pointer a caller may write to and
    // corrupt the heap with. Null is what `CFDataGetMutableBytePtr` answers for
    // the same case, and a null is a thing a caller checks.
    d.bytes.as_mut_ptr()
}

unsafe extern "C" fn int_value(receiver: Id, _sel: Sel) -> i32 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0, |n| n.as_i64() as i32)
}

/// The narrower integer accessors.
///
/// Each truncates the stored value the way a C cast does, which is what Apple's
/// do: an `NSNumber` holds one value and every accessor is a *conversion* of
/// it, not a claim about how it was made.
unsafe extern "C" fn unsigned_int_value(receiver: Id, _sel: Sel) -> u32 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0, |n| n.as_i64() as u32)
}

unsafe extern "C" fn short_value(receiver: Id, _sel: Sel) -> i16 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0, |n| n.as_i64() as i16)
}

unsafe extern "C" fn unsigned_short_value(receiver: Id, _sel: Sel) -> u16 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0, |n| n.as_i64() as u16)
}

unsafe extern "C" fn char_value(receiver: Id, _sel: Sel) -> i8 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0, |n| n.as_i64() as i8)
}

unsafe extern "C" fn unsigned_char_value(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0, |n| n.as_i64() as u8)
}

unsafe extern "C" fn integer_value(receiver: Id, _sel: Sel) -> i64 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0, |n| n.as_i64())
}

unsafe extern "C" fn double_value(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0.0, |n| n.as_f64())
}

unsafe extern "C" fn float_value(receiver: Id, _sel: Sel) -> f32 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0.0, |n| n.as_f64() as f32)
}

unsafe extern "C" fn bool_value(receiver: Id, _sel: Sel) -> bool {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.is_some_and(|n| n.as_i64() != 0)
}

/// `-stringValue`, which is also `-description` for a number.
///
/// An integer prints as itself; a float prints the way Rust's `{}` does, which
/// is the shortest text that reads back as the same double - the same rule
/// Apple's uses, so `@(9.25)` is `9.25` and `@(9.0)` is `9` on both.
unsafe extern "C" fn string_value(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a number.
    let text = match unsafe { collections::number(receiver as CFTypeRef) } {
        Some(collections::Number::Integer(value)) => value.to_string(),
        Some(collections::Number::Float(value)) => value.to_string(),
        // Not a number this runtime made. Apple would raise; an empty string is
        // the answer that lets the caller print something and carry on.
        None => String::new(),
    };
    let string = crate::string::from_str(&text);
    // SAFETY: a string this call just created.
    unsafe { api::CFAutorelease(string as CFTypeRef) as Id }
}

unsafe extern "C" fn number_with_int(_class: Id, _sel: Sel, value: i32) -> Id {
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_number_of_type(
            collections::Number::Integer(value as i64),
            collections::number_type::SINT32,
        )) as Id
    }
}

unsafe extern "C" fn number_with_integer(_class: Id, _sel: Sel, value: i64) -> Id {
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_number(collections::Number::Integer(value))) as Id
    }
}

unsafe extern "C" fn number_with_double(_class: Id, _sel: Sel, value: f64) -> Id {
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(collections::new_number(collections::Number::Float(value))) as Id }
}

/// `+numberWithBool:` is `kCFBooleanTrue` or `kCFBooleanFalse` itself, as
/// Apple's is: a guest compares the answer against `@YES` by identity, and
/// `CFGetTypeID` of it is the boolean type.
unsafe extern "C" fn number_with_bool(_class: Id, _sel: Sel, value: bool) -> Id {
    // Not autoreleased: the two objects live for the process.
    collections::boolean(value) as Id
}

// ---------------------------------------------------------------- the unsigned family
//
// Every `unsigned` spelling is the same two lines of arithmetic, so they are one
// function each and the interesting part is the *width*, which is read off the
// call sites rather than assumed:
//
//   `mov edx, 0x10a00`                     -> 32-bit in `edx`  -> `I`, `numberWithUnsignedInt:`
//   `mov rdx, rax` (a 64-bit value)        -> 64-bit in `rdx`  -> `Q`, `numberWithUnsignedLong:`
//   `mov rdx, rax` (a 64-bit value)        -> 64-bit in `rdx`  -> `Q`, `numberWithUnsignedInteger:`
//
// `unsigned long` and `NSUInteger` are both eight bytes on LP64, which is why
// the two `Q` encodings above are the same and not a guess about which is which:
// `test/tools/raw-range.py 0x80533a1:0x80533c1` for the first and
// `0x3a87113:0x3a87120` for the second, and both load a full `rax`.
//
// There is no unsigned `CFNumberType` - Apple has `SInt8`..`SInt64` and
// `Float32`/`Float64` and nothing else - so an unsigned value is stored in the
// signed type of the same width, which is what `canonical_number_type` already
// returns. The value widens by sign extension, because that is the conversion
// from the 32- or 64-bit unsigned the caller passed to the 64-bit signed cell
// this runtime keeps it in, and it is the conversion the already-installed
// truncating accessors undo: `-unsignedIntValue` of `numberWithUnsignedInt:` is
// `0xffff_ffff` for a caller who passed `0xffff_ffff`, and `-longLongValue` of
// it is `-1`.

unsafe extern "C" fn number_with_unsigned_char(_class: Id, _sel: Sel, value: u8) -> Id {
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_number_of_type(
            collections::Number::Integer(value as i64),
            collections::number_type::SINT8,
        )) as Id
    }
}

unsafe extern "C" fn number_with_unsigned_short(_class: Id, _sel: Sel, value: u16) -> Id {
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_number_of_type(
            // Sign extension, as above: the cell is `SINT16`.
            collections::Number::Integer(value as i16 as i64),
            collections::number_type::SINT16,
        )) as Id
    }
}

unsafe extern "C" fn number_with_unsigned_int(_class: Id, _sel: Sel, value: u32) -> Id {
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_number_of_type(
            // **Sign** extension, per the rule at the top of this group: the
            // cell is `SINT32`, so the caller's `u32` is reinterpreted as the
            // `i32` of the same width and *that* widens. `value as i64` would
            // zero-extend, which puts `0x8000_0000` in a signed cell as
            // `+2147483648` and makes `-longLongValue` disagree with
            // `-intValue` for the same object.
            collections::Number::Integer(value as i32 as i64),
            collections::number_type::SINT32,
        )) as Id
    }
}

unsafe extern "C" fn number_with_unsigned_long(_class: Id, _sel: Sel, value: u64) -> Id {
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_number_of_type(
            // Sign extension, as above: the cell is `SINT64`, so the caller's
            // `u64` is reinterpreted as the `i64` of the same width.
            collections::Number::Integer(value as i64),
            collections::number_type::SINT64,
        )) as Id
    }
}

/// `+[NSNumber numberWithFloat:]`: a `float`, so a 32-bit value in `xmm0`.
///
/// Read off the call site rather than assumed, because the difference is
/// observable and silent: `0x804d413` is `movss xmm0, DWORD PTR [rcx+0x4]` - a
/// 32-bit load - where a `double` would be `movsd` over eight. The stored value
/// is widened to the `f64` this runtime keeps, which is exact for every `f32`.
unsafe extern "C" fn number_with_float(_class: Id, _sel: Sel, value: f32) -> Id {
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_number_of_type(
            collections::Number::Float(value as f64),
            collections::number_type::FLOAT32,
        )) as Id
    }
}

unsafe extern "C" fn set_with_object(_class: Id, _sel: Sel, object: Id) -> Id {
    let values: &[CFTypeRef] = match object.is_null() {
        true => &[],
        false => &[object as CFTypeRef],
    };
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(collections::new_set(values, Policy::from_type_callbacks())) as Id }
}

unsafe extern "C" fn set_with_capacity(_class: Id, _sel: Sel, _capacity: usize) -> Id {
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(collections::new_set(&[], Policy::from_type_callbacks())) as Id }
}

unsafe extern "C" fn set_count(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: a set.
    unsafe { collections::set(receiver as CFTypeRef) }.map_or(0, |set| set.values.len())
}

unsafe extern "C" fn set_contains_object(receiver: Id, _sel: Sel, object: Id) -> bool {
    // SAFETY: a set.
    unsafe { collections::set(receiver as CFTypeRef) }.is_some_and(|set| {
        set.values
            .iter()
            .any(|value| set.policy.equal(*value, object as CFTypeRef))
    })
}

/// `-addObject:`, which keeps the set's contents unique under its own equality.
unsafe extern "C" fn set_add_object(receiver: Id, _sel: Sel, object: Id) {
    if object.is_null() {
        objc::fail!("-[NSMutableSet addObject:]: object cannot be nil");
    }
    // SAFETY: a set.
    let Some(set) = (unsafe { collections::set(receiver as CFTypeRef) }) else {
        return;
    };
    let object = object as CFTypeRef;
    if !set
        .values
        .iter()
        .any(|value| set.policy.equal(*value, object))
    {
        set.values.push(set.policy.retain(object));
    }
}

unsafe extern "C" fn set_any_object(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a set.
    unsafe { collections::set(receiver as CFTypeRef) }
        .and_then(|set| set.values.first().copied())
        .map_or(std::ptr::null_mut(), |value| value as Id)
}

/// `+[NSMutableSet new]`, and what `[[NSMutableSet alloc] init]` substitutes to.
///
/// The guest's own source for the set it builds at start-up is
/// `chrome/browser/web_applications/os_integration/mac/app_shim_termination_observer.mm`:
///
/// ```objc
/// + (NSMutableSet<AppShimTerminationObserver*>*)allObservers {
///   static NSMutableSet<AppShimTerminationObserver*>* set = [NSMutableSet set];
///   return set;
/// }
/// ```
///
/// and the compiled form is `mov rdi, [rip+_OBJC_CLASS_$_NSMutableSet];
/// mov rsi, [rip@"set"]; call objc_msgSend` - a single message to the *class*,
/// with no `objc_alloc` in between, at `Framework+0x46f15f3` and
/// `Framework+0x8158783`. `Foundation has no +set`, so that message refuses by
/// name here rather than answering a value; the other five sites for this class
/// are the ordinary `[[NSMutableSet alloc] init]`, compiled as
/// `mov rdi, [rip+class]; call _objc_alloc_init`, and those are what this answers.
unsafe extern "C" fn new_set(_class: Id, _sel: Sel) -> Id {
    collections::new_mutable_set(Policy::from_type_callbacks()) as Id
}

/// `-[NSMutableSet init]`, the class-cluster substitution.
///
/// `+alloc` on `NSMutableSet` answers an instance of the abstract facade -
/// an `isa` and nothing else - because that is all an abstract class has room
/// for, and the contents live on the concrete `__NSCFSet` this module's
/// `mutable_pair` registers. A placeholder handed straight back answers empty to
/// every question and swallows every write, silently: nothing about it is wrong
/// enough to fail on, and the program carries on with a set that never fills.
/// Apple's clusters substitute here too, which is why `-init` is documented
/// there as returning "an object other than the receiver".
unsafe extern "C" fn init_mutable_set(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an object of this class.
    let known = unsafe { collections::set(receiver as CFTypeRef) }.is_some();
    // SAFETY: as `cluster_init`'s.
    unsafe { cluster_init(receiver, known, new_set_receiver) }
}

/// `cluster_init` wants a `CFTypeRef`; `+new` is written for a receiver.
fn new_set_receiver() -> corefoundation::base::CFTypeRef {
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(collections::new_mutable_set(Policy::from_type_callbacks())) }
}

/// `-[NSMutableSet initWithCapacity:]`, where the capacity is a hint this store
/// has no use for.
unsafe extern "C" fn init_mutable_set_with_capacity(receiver: Id, _sel: Sel, _capacity: usize) -> Id {
    // SAFETY: identical to `-init`; the argument is ignored.
    unsafe { init_mutable_set(receiver, std::ptr::null_mut()) }
}

/// `-[NSSet init]`, the same substitution on the immutable side.
unsafe extern "C" fn init_set(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an object of this class.
    let known = unsafe { collections::set(receiver as CFTypeRef) }.is_some();
    // SAFETY: as `cluster_init`'s.
    unsafe {
        cluster_init(receiver, known, || {
            api::CFAutorelease(collections::new_set(&[], Policy::from_type_callbacks()))
        })
    }
}

/// `+setWithArray:`, which is how the guest makes the immutable one it sends
/// `setWithObject:` to a peer: `+[NSSet setWithObject:]` at
/// `Framework+0x3a8a3d4`, inside `-[RenderWidgetHostViewCocoa makeTouchBar]`.
unsafe extern "C" fn set_with_array(_class: Id, _sel: Sel, array: Id) -> Id {
    let values: Vec<CFTypeRef> = match unsafe { collections::array(array as CFTypeRef) } {
        // SAFETY: a set made here copies what it is given.
        Some(a) => a.values.clone(),
        None => {
            if array.is_null() {
                Vec::new()
            } else {
                // A named refusal rather than an empty set: the argument was
                // not an array, and answering empty would look like a set that
                // legitimately holds nothing.
                objc::fail!("+[NSSet setWithArray:]: the argument is not an array");
            }
        }
    };
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(collections::new_set(&values, Policy::from_type_callbacks())) as Id }
}

/// `-[NSMutableSet removeObject:]`.
unsafe extern "C" fn set_remove_object(receiver: Id, _sel: Sel, object: Id) {
    // SAFETY: a set.
    let Some(set) = (unsafe { collections::set(receiver as CFTypeRef) }) else {
        return;
    };
    let object = object as CFTypeRef;
    let mut removed = Vec::new();
    set.values.retain(|value| {
        let equal = set.policy.equal(*value, object);
        if equal {
            removed.push(*value);
        }
        !equal
    });
    // Released outside the borrow: an equality callback can run arbitrary guest
    // code, and this one runs after the set's own storage has been shortened.
    for value in removed {
        set.policy.release(value);
    }
}

/// `-[NSMutableSet removeAllObjects]`.
unsafe extern "C" fn set_remove_all_objects(receiver: Id, _sel: Sel) {
    // SAFETY: a set.
    let Some(set) = (unsafe { collections::set(receiver as CFTypeRef) }) else {
        return;
    };
    let drained: Vec<CFTypeRef> = set.values.drain(..).collect();
    for value in drained {
        set.policy.release(value);
    }
}

/// `-[NSMutableSet copy]`, which the guest's `NSSpeechSynthesizer` path reaches
/// through `-mutableCopy` on an immutable set.
unsafe extern "C" fn set_mutable_copy(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a set.
    let Some(set) = (unsafe { collections::set(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let values = set.values.clone();
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(collections::new_set(&values, Policy::from_type_callbacks())) as Id }
}

pub(crate) fn install() {
    let set = objc::class::lookup(c"NSSet").expect("CoreFoundation registered NSSet");
    class_methods!(set, {
        "setWithObject:" => set_with_object, c"@24@0:8@16";
        "setWithCapacity:" => set_with_capacity, c"@24@0:8Q16";
        "setWithArray:" => set_with_array, c"@24@0:8@16";
    });
    methods!(set, {
        "init" => init_set, c"@16@0:8";
        "count" => set_count, c"Q16@0:8";
        "containsObject:" => set_contains_object, c"B24@0:8@16";
        "anyObject" => set_any_object, c"@16@0:8";
    });
    let mutable_set = objc::class::lookup(c"NSMutableSet").expect("NSMutableSet");
    class_methods!(mutable_set, {
        "new" => new_set, c"@16@0:8";
    });
    methods!(mutable_set, {
        "init" => init_mutable_set, c"@16@0:8";
        "initWithCapacity:" => init_mutable_set_with_capacity, c"@24@0:8Q16";
        "addObject:" => set_add_object, c"v24@0:8@16";
        "removeObject:" => set_remove_object, c"v24@0:8@16";
        "removeAllObjects" => set_remove_all_objects, c"v16@0:8";
        "mutableCopy" => set_mutable_copy, c"@16@0:8";
    });

    let array = objc::class::lookup(c"NSArray").expect("CoreFoundation registered NSArray");
    let mutable_array = objc::class::lookup(c"NSMutableArray").expect("NSMutableArray");
    methods!(array, {
        "count" => count, c"Q16@0:8";
        "objectAtIndex:" => object_at_index, c"@24@0:8Q16";
        "objectAtIndexedSubscript:" => object_at_index, c"@24@0:8Q16";
        "containsObject:" => contains_object, c"B24@0:8@16";
        "firstObject" => first_object, c"@16@0:8";
        "lastObject" => last_object, c"@16@0:8";
        "componentsJoinedByString:" => components_joined_by_string, c"@24@0:8@16";
        "sortedArrayUsingSelector:" => sorted_array_using_selector, c"@24@0:8:16";
        "filteredArrayUsingPredicate:" => filtered_array_using_predicate, c"@24@0:8@16";
        "indexOfObjectWithOptions:passingTest:" => index_of_object_with_options_passing_test, c"Q32@0:8Q16@?24";
        "countByEnumeratingWithState:objects:count:" => array_enumerate, c"Q40@0:8^{?=Q^@^QQ5}16^@24Q32";
        "description" => array_description, c"@16@0:8";
    });
    class_methods!(array, {
        "array" => array_fn, c"@16@0:8";
        "arrayWithObject:" => array_with_object, c"@24@0:8@16";
        "arrayWithObjects:count:" => array_with_objects_count, c"@32@0:8^@16Q24";
    });
    methods!(mutable_array, {
        "init" => init_mutable_array, c"@16@0:8";
        "initWithCapacity:" => init_mutable_array_with_capacity, c"@24@0:8Q16";
        "addObject:" => add_object, c"v24@0:8@16";
        "addObjectsFromArray:" => add_objects_from_array, c"v24@0:8@16";
        "removeObjectAtIndex:" => remove_object_at_index, c"v24@0:8Q16";
        "removeAllObjects" => remove_all_objects, c"v16@0:8";
    });
    class_methods!(mutable_array, {
        "new" => new_array, c"@16@0:8";
        "array" => array_fn, c"@16@0:8";
        "arrayWithCapacity:" => array_with_capacity, c"@24@0:8Q16";
        "arrayWithArray:" => array_with_array, c"@24@0:8@16";
    });

    let dictionary_class =
        objc::class::lookup(c"NSDictionary").expect("CoreFoundation registered NSDictionary");
    let mutable_dictionary =
        objc::class::lookup(c"NSMutableDictionary").expect("NSMutableDictionary");
    methods!(dictionary_class, {
        "count" => dictionary_count, c"Q16@0:8";
        "objectForKey:" => object_for_key, c"@24@0:8@16";
        "objectForKeyedSubscript:" => object_for_key, c"@24@0:8@16";
        "allKeys" => all_keys, c"@16@0:8";
        "allValues" => all_values, c"@16@0:8";
        "description" => dictionary_description, c"@16@0:8";
        "valueForKey:" => dictionary_value_for_key, c"@24@0:8@16";
        "countByEnumeratingWithState:objects:count:" => dictionary_enumerate, c"Q40@0:8^{?=Q^@^QQ5}16^@24Q32";
    });
    class_methods!(dictionary_class, {
        "dictionaryWithObjects:forKeys:count:" => dictionary_with_objects_for_keys_count, c"@40@0:8^@16^@24Q32";
        "dictionaryWithObject:forKey:" => dictionary_with_object_for_key, c"@32@0:8@16@24";
        "dictionaryWithContentsOfFile:" => dictionary_with_contents_of_file, c"@24@0:8@16";
        "dictionaryWithContentsOfURL:error:" => dictionary_with_contents_of_url_error, c"@32@0:8@16^@24";
    });
    methods!(mutable_dictionary, {
        "init" => init_mutable_dictionary, c"@16@0:8";
        "initWithCapacity:" => init_mutable_dictionary_with_capacity, c"@24@0:8Q16";
        "setObject:forKey:" => set_object_for_key, c"v32@0:8@16@24";
        "setObject:forKeyedSubscript:" => set_object_for_key, c"v32@0:8@16@24";
        "removeObjectForKey:" => remove_object_for_key, c"v24@0:8@16";
        "setValue:forKey:" => dictionary_set_value_for_key, c"v32@0:8@16@24";
    });
    class_methods!(mutable_dictionary, {
        "new" => new_dictionary, c"@16@0:8";
        "dictionary" => dictionary, c"@16@0:8";
        "dictionaryWithCapacity:" => dictionary_with_capacity, c"@24@0:8Q16";
    });

    let predicate = corefoundation::base::register(
        c"NSPredicate",
        None,
        std::mem::size_of::<Predicate>(),
        false,
    );
    methods!(predicate, {
        "dealloc" => predicate_dealloc, c"v16@0:8";
    });
    class_methods!(predicate, {
        "predicateWithBlock:" => predicate_with_block, c"@24@0:8@?16";
    });

    let data = objc::class::lookup(c"NSData").expect("CoreFoundation registered NSData");
    let mutable_data = objc::class::lookup(c"NSMutableData").expect("NSMutableData");
    methods!(data, {
        "length" => data_length, c"Q16@0:8";
        "bytes" => data_bytes, c"^v16@0:8";
        // Three of the instance methods a merge dropped, restored because the
        // guest's own `__objc_selrefs` name them: each is reached either through
        // `+alloc` on `NSData` or through `NSObject`'s `-copy`. Each function's
        // doc comment carries the send sites it was measured at. The `data_census`
        // test at the bottom of this file prints the whole table, so the next
        // person can diff it rather than trust this comment.
        //
        // **What is deliberately *not* here, and why** - the census over all 13
        // guest images, counting a selector's `__objc_selrefs` loads and the
        // loads followed by a message send, then resolving the receiver's class
        // at every site by disassembly:
        //
        // | selector | loads | sends | whose |
        // |---|---|---|---|
        // | `initWithBytes:length:` | 8 | 8 | all `+[NSData alloc]`, 6 direct and 2 through a stack-hoisted selector |
        // | `getBytes:length:` | 1 | 1 | one site, two argument pairs |
        // | `copyWithZone:` | 1 | 1 | one site, an immutable receiver |
        // | `data` | 4 | 4 | all `+[NSData data]` |
        // | `dataWithBytes:length:` | 24 | 24 | all `+[NSData …]` |
        // | `dataWithContentsOfFile:` | 1 | 1 | `_OBJC_CLASS_$_NSData` |
        // | `dataWithContentsOfFile:options:error:` | 1 | 1 | `_OBJC_CLASS_$_NSData` |
        // | `initWithCapacity:` | 7 | 7 | **6 `NSMutableArray`, 1 `NSMutableDictionary`, 0 data** |
        // | `mutableCopy` | 11 | 9 | **arrays, a dictionary, a paragraph style, a window - 0 data** |
        // | `initWithData:` | 2 | 2 | **`NSImage` and `NSBitmapImageRep`, 0 data** |
        // | `appendBytes:length:`, `mutableBytes`, `setData:`, `increaseLengthBy:`, `dataWithCapacity:`, `dataWithLength:`, `isEqualToData:`, `subdataWithRange:`, `dataWithData:`, `writeToFile:options:error:`, `getBytes:range:`, `initWithBytesNoCopy:length:`, `setLength:` | 0 | 0 | nothing, and no name outside `__objc_methname` to build one from |
        //
        // So `-[NSMutableData initWithCapacity:]`, `+[NSMutableData dataWithCapacity:]`
        // and `-[NSMutableData mutableCopy]` are **not** installed: the first two
        // because the 7 and the 0 are other classes' selectors, the third because
        // all nine `mutableCopy` sends are on something else. Installing them
        // would be code nothing sends, outliving the reason it was written.
        //
        // **Eight methods upstream added are absent here, and that is a
        // decision, not a merge accident** - it is recorded because a merge that
        // drops code is otherwise indistinguishable from a merge that dropped it
        // by accident, and the next person to merge will otherwise have to
        // rediscover this. Upstream's eight, and the census over all 13 images
        // for each: `hash`, `getBytes:`, `setLength:`, `increaseLengthBy:`,
        // `appendData:`, `replaceBytesInRange:withBytes:`,
        // `numberWithShort:`, `numberWithChar:`. Seven measure **0** loads and
        // **0** sends, with no name outside `__objc_methname` to build one from at
        // run time. (`hash` is the eighth and was not separately adjudicated - see
        // the note on `description` below.)
        //
        // There is a second and stronger reason for the four mutators.
        // `setLength:`, `increaseLengthBy:`, `appendData:` and
        // `replaceBytesInRange:withBytes:` all write through
        // `collections::data(...)`, which hands out a `&mut Vec<u8>` and **does
        // not consult the mutability bit** - so each one as upstream writes it
        // mutates a data this runtime has said cannot change, silently. That is
        // precisely the hole `-[NSMutableData appendBytes:length:]` refuses by
        // name, and `-mutableBytes` beside it. Restoring them unmodified would
        // reopen it four times over, for four selectors no guest sends. If they
        // are ever wanted, they belong behind the same `immutable(...)` refusal
        // `append_bytes` uses, not behind `collections::data`.
        //
        // `numberWithShort:` is the one worth a second look before it is written
        // off: a review this session asked whether the guest ever sends it
        // refuted that it does, on a census over all 13 images, and the burden of
        // proof is on the selector rather than on the refutation.
        "initWithBytes:length:" => data_init_with_bytes, c"@32@0:8r^v16Q24";
        "getBytes:length:" => data_get_bytes_length, c"v32@0:8^v16Q24";
        "copyWithZone:" => data_copy_with_zone, c"@24@0:8^v16";
    });
    class_methods!(data, {
        // The two `+` methods restored here, measured at four and one guest send
        // respectively; see their doc comments. Every one of these five is
        // declared on `NSData` and inherited by `NSMutableData`, so each takes
        // its mutability from the receiver class - `new_data_for`.
        "data" => data_empty, c"@16@0:8";
        "dataWithBytes:length:" => data_with_bytes, c"@32@0:8^v16Q24";
        "dataWithBytesNoCopy:length:" => data_with_bytes_no_copy, c"@32@0:8^v16Q24";
        "dataWithContentsOfFile:" => data_with_contents_of_file, c"@24@0:8@16";
        "dataWithContentsOfFile:options:error:"
            => data_with_contents_of_file_options, c"@40@0:8@16Q24^@32";
    });
    methods!(data, {
        "writeToFile:atomically:" => write_to_file, c"B28@0:8@16c24";
    });
    methods!(mutable_data, {
        // Both of these are declared on `NSMutableData` alone and both refuse an
        // immutable data by name; see their doc comments for why the guest's
        // silence about them is a measurement rather than an omission.
        "appendBytes:length:" => append_bytes, c"v32@0:8^v16Q24";
        "mutableBytes" => mutable_bytes, c"^v16@0:8";
    });

    let number = objc::class::lookup(c"NSNumber").expect("CoreFoundation registered NSNumber");
    methods!(number, {
        "intValue" => int_value, c"i16@0:8";
        "integerValue" => integer_value, c"q16@0:8";
        "longValue" => integer_value, c"q16@0:8";
        "longLongValue" => integer_value, c"q16@0:8";
        "unsignedIntegerValue" => integer_value, c"Q16@0:8";
        "unsignedLongValue" => integer_value, c"Q16@0:8";
        "unsignedLongLongValue" => integer_value, c"Q16@0:8";
        "unsignedIntValue" => unsigned_int_value, c"I16@0:8";
        "shortValue" => short_value, c"s16@0:8";
        "unsignedShortValue" => unsigned_short_value, c"S16@0:8";
        "charValue" => char_value, c"c16@0:8";
        "unsignedCharValue" => unsigned_char_value, c"C16@0:8";
        "doubleValue" => double_value, c"d16@0:8";
        "floatValue" => float_value, c"f16@0:8";
        "boolValue" => bool_value, c"B16@0:8";
        "stringValue" => string_value, c"@16@0:8";
        "description" => string_value, c"@16@0:8";
    });
    class_methods!(number, {
        "numberWithInt:" => number_with_int, c"@24@0:8i16";
        "numberWithInteger:" => number_with_integer, c"@24@0:8q16";
        "numberWithLong:" => number_with_integer, c"@24@0:8q16";
        "numberWithLongLong:" => number_with_integer, c"@24@0:8q16";
        "numberWithDouble:" => number_with_double, c"@24@0:8d16";
        "numberWithBool:" => number_with_bool, c"@24@0:8B16";
        // The unsigned spellings, each with the width its call site loads. The
        // guest sends `numberWithUnsignedInt:`, `numberWithUnsignedLong:`,
        // `numberWithUnsignedInteger:`, `numberWithFloat:` and
        // `numberWithUnsignedChar:`; the short and long-long pair complete the
        // family and cost one line each.
        "numberWithUnsignedChar:" => number_with_unsigned_char, c"@24@0:8C16";
        "numberWithUnsignedShort:" => number_with_unsigned_short, c"@24@0:8S16";
        "numberWithUnsignedInt:" => number_with_unsigned_int, c"@24@0:8I16";
        "numberWithUnsignedLong:" => number_with_unsigned_long, c"@24@0:8Q16";
        "numberWithUnsignedLongLong:" => number_with_unsigned_long, c"@24@0:8Q16";
        "numberWithUnsignedInteger:" => number_with_unsigned_long, c"@24@0:8Q16";
        "numberWithFloat:" => number_with_float, c"@24@0:8f16";
    });
}

#[cfg(test)]
mod tier1_tests {
    use super::*;

    fn class(name: &std::ffi::CStr) -> Id {
        crate::bootstrap();
        objc::class::ptr(objc::class::lookup(name).expect("registered")) as Id
    }

    /// Send through the runtime's own dispatch, the way a guest does, so the
    /// test exercises the installed method rather than the function beside it.
    ///
    /// # Safety
    ///
    /// `A` and `R` must be the method's real argument and return types.
    unsafe fn send<A, R>(receiver: Id, name: &str, arg: A) -> R {
        // SAFETY: the caller's obligation.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, A) -> R =
                std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get(name), arg)
        }
    }

    /// A number made by a `+numberWith…` factory reads back the value that was
    /// written, through **both** the signed and the unsigned accessor of the
    /// same width.
    ///
    /// Both halves matter and the pair is the point: a factory that stored the
    /// value zero-extended would pass a `-longLongValue` check and fail
    /// `-unsignedIntValue`, and the reverse is equally silent. The values are
    /// chosen to have different top bits, so an accidental sign or zero extension
    /// cannot pass both.
    #[test]
    fn an_unsigned_number_reads_back_as_signed_and_unsigned() {
        crate::bootstrap();
        let number = class(c"NSNumber");
        // SAFETY: each prototype is the one its selector declares, and the
        // widths are those read off the guest's call sites.
        unsafe {
            let from_unsigned_int = send(number, "numberWithUnsignedInt:", 0x8000_0001u32);
            assert_eq!(crate::number_of(from_unsigned_int), Some(-2147483647));
            assert_eq!(
                send::<(), u32>(from_unsigned_int, "unsignedIntValue", ()),
                0x8000_0001
            );

            // The 64-bit spellings: `unsigned long` and `NSUInteger`, both of
            // which the guest loads as a full `rax`.
            let from_unsigned_long = send(number, "numberWithUnsignedLong:", 0x8000_0000_0000_0001u64);
            assert_eq!(
                crate::number_of(from_unsigned_long),
                Some(i64::MIN + 1),
                "an unsigned 64-bit value widens by sign extension"
            );
            assert_eq!(
                send::<(), u64>(from_unsigned_long, "unsignedLongLongValue", ()),
                0x8000_0000_0000_0001,
                "and the unsigned accessor recovers the bits that were written"
            );

            let from_unsigned_integer =
                send(number, "numberWithUnsignedInteger:", 0xffff_ffff_ffff_ffffu64);
            assert_eq!(
                send::<(), u64>(from_unsigned_integer, "unsignedLongLongValue", ()),
                u64::MAX
            );
        }
    }

    /// `+numberWithFloat:` is a `float`, so a value a `double` could not hold
    /// exactly must not come back as one that it can - that would mean the 32-bit
    /// argument was widened somewhere it should not have been.
    #[test]
    fn a_float_is_stored_at_float_precision() {
        crate::bootstrap();
        let number = class(c"NSNumber");
        // 0.1 is not representable in binary; its `f32` neighbour is what the
        // guest's `movss` load produces, and reading it back must give that
        // neighbour rather than the `f64` the caller wrote in the source.
        let made = unsafe { send(number, "numberWithFloat:", 0.1f32) };
        // SAFETY: the value is a number.
        let as_f32 = unsafe { send::<(), f32>(made, "floatValue", ()) };
        assert_eq!(as_f32, 0.1f32);
        // SAFETY: the value is a number.
        let as_f64 = unsafe { send::<(), f64>(made, "doubleValue", ()) };
        assert_eq!(
            as_f64,
            0.1f32 as f64,
            "the value widened from the float that was passed, not from a double"
        );
        assert_ne!(as_f64, 0.1f64, "so it is not the double the source spelled");
    }

    /// The whole `numberWith` family is installed, and each carries the encoding
    /// its argument width implies.
    ///
    /// Asserted on the encodings rather than only on behaviour, because a wrong
    /// width is what makes an `objc_msgSend` read the wrong register: the guest
    /// loads a 32-bit immediate for `numberWithUnsignedInt:` and a full `rax` for
    /// `numberWithUnsignedInteger:`, and an implementation declaring the other's
    /// width would still pass a value round-trip on a host where both arrive in
    /// the same register.
    #[test]
    fn the_number_factory_encodings_are_the_widths_the_guest_loads() {
        crate::bootstrap();
        let number = objc::class::lookup(c"NSNumber").expect("NSNumber");
        let expected = [
            ("numberWithInt:", c"@24@0:8i16"),
            ("numberWithInteger:", c"@24@0:8q16"),
            ("numberWithLong:", c"@24@0:8q16"),
            ("numberWithLongLong:", c"@24@0:8q16"),
            ("numberWithDouble:", c"@24@0:8d16"),
            ("numberWithBool:", c"@24@0:8B16"),
            ("numberWithUnsignedChar:", c"@24@0:8C16"),
            ("numberWithUnsignedShort:", c"@24@0:8S16"),
            ("numberWithUnsignedInt:", c"@24@0:8I16"),
            ("numberWithUnsignedLong:", c"@24@0:8Q16"),
            ("numberWithUnsignedLongLong:", c"@24@0:8Q16"),
            ("numberWithUnsignedInteger:", c"@24@0:8Q16"),
            ("numberWithFloat:", c"@24@0:8f16"),
        ];
        for (name, types) in expected {
            let method = number
                .metaclass()
                .method(objc::sel::get(name))
                .unwrap_or_else(|| panic!("+[{name}] is not installed"));
            // SAFETY: a registered method's types are a NUL-terminated string.
            let declared = unsafe { std::ffi::CStr::from_ptr(method.types()) };
            assert_eq!(declared, types, "+[{name}] declares the wrong width");
        }
    }

    /// `+[NSDictionary dictionaryWithContentsOfURL:error:]` reads a real plist
    /// and clears the out-parameter.
    ///
    /// The clearing is the half a caller can be hurt by silently: the guest
    /// zeroes its own slot and reads it back (`0x470eec3` / `0x470eeed`), so a
    /// success that did not write would leave whatever the stack held. The slot
    /// is therefore pre-loaded with a **non-null canary** - not a null, which
    /// would pass whether or not anything was written - and the assertion is that
    /// the call cleared it.
    #[test]
    fn a_plist_url_reads_the_dictionary_and_clears_the_error_out_parameter() {
        crate::bootstrap();
        let directory = std::env::temp_dir().join("rosetta3-tier1-plist");
        std::fs::create_dir_all(&directory).expect("a temporary directory");
        let path = directory.join("Brand.plist");
        std::fs::write(
            &path,
            br#"<?xml version="1.0"?>
               <plist version="1.0"><dict>
                 <key>KSBrandID</key><string>GGGL</string>
                 <key>CFBundleName</key><string>Chromium</string>
               </dict></plist>"#,
        )
        .expect("a plist to write");

        // SAFETY: `+fileURLWithPath:` is `id (Class, SEL, NSString *)`.
        let url: Id = unsafe {
            send(
                class(c"NSURL"),
                "fileURLWithPath:",
                crate::string::from_str(&path.to_string_lossy()),
            )
        };
        assert!(!url.is_null());

        let canary = crate::string::from_str("not an error, and not nil");
        let mut out = canary;
        // SAFETY: `+dictionaryWithContentsOfURL:error:` is
        // `id (Class, SEL, id, NSError **)`.
        let dictionary = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                class(c"NSDictionary"),
                objc::sel::get("dictionaryWithContentsOfURL:error:"),
                url,
                &raw mut out,
            )
        };
        assert!(!dictionary.is_null(), "the plist did not read");
        assert!(
            out.is_null(),
            "success must write nil through the out-parameter, not leave it"
        );

        // Read the way the guest reads it: `-objectForKey:`.
        // SAFETY: `-objectForKey:` is `id (id, SEL, id)`.
        let brand = unsafe {
            send(
                dictionary,
                "objectForKey:",
                crate::string::from_str("KSBrandID"),
            )
        };
        // SAFETY: the value is a string.
        assert_eq!(unsafe { crate::string::bytes(brand) }, b"GGGL");
        let _ = std::fs::remove_file(&path);
    }

    /// The out-parameter is written on **failure** too, with an error that names
    /// the cause, and a null out-parameter is never written through.
    #[test]
    fn a_failure_writes_an_error_and_a_null_out_parameter_is_left_alone() {
        crate::bootstrap();
        // SAFETY: `+fileURLWithPath:` is `id (Class, SEL, NSString *)`.
        let url = unsafe {
            send(
                class(c"NSURL"),
                "fileURLWithPath:",
                crate::string::from_str("/nonexistent/rosetta3/no-such.plist"),
            )
        };
        let mut out = crate::string::from_str("canary");
        // SAFETY: `+dictionaryWithContentsOfURL:error:` is
        // `id (Class, SEL, id, NSError **)`.
        let dictionary = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                class(c"NSDictionary"),
                objc::sel::get("dictionaryWithContentsOfURL:error:"),
                url,
                &raw mut out,
            )
        };
        assert!(dictionary.is_null(), "a missing file is not a dictionary");
        assert!(!out.is_null(), "the failure must be reported");

        // SAFETY: `out` is an NSError this runtime made.
        let code = unsafe { send::<(), i64>(out, "code", ()) };
        assert_eq!(
            code, 4,
            "NSFileNoSuchFileError, which is what a missing file is"
        );
        // SAFETY: `out` is an NSError this runtime made.
        let domain = unsafe { send::<(), Id>(out, "domain", ()) };
        // SAFETY: the domain is a string.
        assert_eq!(unsafe { crate::string::bytes(domain) }, b"NSCocoaErrorDomain");

        // Four of the guest's five sites pass a null out-parameter. Writing
        // through it would be a store to whatever that address happens to be,
        // so this must answer nil without faulting and without writing.
        // SAFETY: as above, with a null out-parameter.
        let none = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                class(c"NSDictionary"),
                objc::sel::get("dictionaryWithContentsOfURL:error:"),
                url,
                std::ptr::null_mut(),
            )
        };
        assert!(none.is_null());
    }

    /// A plist whose root is not a dictionary is a *different* failure from a
    /// missing file, and reports a different code - a caller switches on it.
    #[test]
    fn a_plist_that_is_not_a_dictionary_reports_a_corrupt_plist() {
        crate::bootstrap();
        let path = std::env::temp_dir().join("rosetta3-tier1-array.plist");
        std::fs::write(
            &path,
            br#"<?xml version="1.0"?>
               <plist version="1.0"><array><string>a</string></array></plist>"#,
        )
        .expect("a plist to write");
        // SAFETY: `+fileURLWithPath:` is `id (Class, SEL, NSString *)`.
        let url: Id = unsafe {
            send(
                class(c"NSURL"),
                "fileURLWithPath:",
                crate::string::from_str(&path.to_string_lossy()),
            )
        };
        let mut out = crate::string::from_str("canary");
        // SAFETY: `+dictionaryWithContentsOfURL:error:` is
        // `id (Class, SEL, id, NSError **)`.
        let dictionary = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                class(c"NSDictionary"),
                objc::sel::get("dictionaryWithContentsOfURL:error:"),
                url,
                &raw mut out,
            )
        };
        assert!(dictionary.is_null());
        // SAFETY: `out` is an NSError this runtime made.
        let code = unsafe { send::<(), i64>(out, "code", ()) };
        assert_eq!(
            code, 3840,
            "NSPropertyListReadCorruptError - the same code crate::json reports"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The two-argument selector is a class method with the four-word encoding,
    /// and `^@` for the out-parameter - a mistake there would have the guest's
    /// stack slot read as an object.
    #[test]
    fn the_url_error_spelling_is_a_class_method_with_an_out_parameter() {
        crate::bootstrap();
        let dictionary = objc::class::lookup(c"NSDictionary").expect("NSDictionary");
        let sel = objc::sel::get("dictionaryWithContentsOfURL:error:");
        assert!(
            dictionary.method(sel).is_none(),
            "not an instance method"
        );
        let method = dictionary
            .metaclass()
            .method(sel)
            .expect("a + method on NSDictionary");
        // SAFETY: a registered method's types are a NUL-terminated string.
        let types = unsafe { std::ffi::CStr::from_ptr(method.types()) };
        assert_eq!(types, c"@32@0:8@16^@24");
        assert!(!method.implementation().is_null());
    }

    /// `[[NSMutableSet alloc] init]` is what five of the guest's six sites for
    /// this class compile to, so it has to return something that holds what is
    /// put in it.
    ///
    /// The write-then-read is the whole test. `+alloc` on the abstract facade
    /// answers an object with an `isa` and nothing else; if `-init` handed that
    /// back, `-addObject:` would find no contents, return early, and the program
    /// would carry on with a set that silently never fills - which is why the
    /// substitution has to be here and not merely plausible.
    #[test]
    fn a_guest_built_mutable_set_holds_what_is_added_to_it() {
        crate::bootstrap();
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            let send0: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            let send1: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            let count: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());
            let contains: unsafe extern "C" fn(Id, Sel, Id) -> bool =
                std::mem::transmute(objc::msg::entry());

            let set = send0(
                send0(
                    objc::class::ptr(objc::class::lookup(c"NSMutableSet").unwrap()) as Id,
                    objc::sel::get("alloc"),
                ),
                objc::sel::get("init"),
            );
            assert!(!set.is_null());
            let element = crate::string::from_str("AppShimTerminationObserver");
            send1(set, objc::sel::get("addObject:"), element);
            assert_eq!(count(set, objc::sel::get("count")), 1);
            assert!(contains(set, objc::sel::get("containsObject:"), element));

            send1(set, objc::sel::get("removeObject:"), element);
            assert_eq!(
                count(set, objc::sel::get("count")),
                0,
                "-removeObject: left the element in place"
            );
            send1(set, objc::sel::get("addObject:"), element);
            send0(set, objc::sel::get("removeAllObjects"));
            assert_eq!(count(set, objc::sel::get("count")), 0);
        }
    }

    /// `+[NSMutableSet new]` and `-[NSMutableSet initWithCapacity:]` both hand
    /// back a set that will hold what is put in it, which is what the guest's
    /// `allObservers` static is built from.
    ///
    /// `initWithCapacity:` is sent to an *instance*: it is the initialiser, not
    /// a convenience constructor, and sending it to the class would look for it
    /// on the metaclass.
    #[test]
    fn a_mutable_set_from_new_and_from_capacity_are_the_same_kind_of_thing() {
        crate::bootstrap();
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            let send0: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            let with: unsafe extern "C" fn(Id, Sel, usize) -> Id =
                std::mem::transmute(objc::msg::entry());
            let count: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());
            let class = objc::class::ptr(objc::class::lookup(c"NSMutableSet").unwrap()) as Id;
            let from_new = send0(class, objc::sel::get("new"));
            assert_eq!(count(from_new, objc::sel::get("count")), 0);
            let from_capacity = with(
                send0(class, objc::sel::get("alloc")),
                objc::sel::get("initWithCapacity:"),
                16,
            );
            assert_eq!(count(from_capacity, objc::sel::get("count")), 0);
        }
    }

    /// `+setWithArray:` over the guest's own `-[RenderWidgetHostViewCocoa
    /// makeTouchBar]`, which is the site that reaches `+[NSSet setWithObject:]`
    /// next door.
    #[test]
    fn a_set_built_from_an_array_holds_the_arrays_elements() {
        crate::bootstrap();
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            let send0: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            let with: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
            let count: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());
            let array_class = objc::class::ptr(objc::class::lookup(c"NSArray").unwrap()) as Id;
            let set_class = objc::class::ptr(objc::class::lookup(c"NSSet").unwrap()) as Id;
            let one = with(array_class, objc::sel::get("arrayWithObject:"), crate::string::from_str("a"));
            let two = with(array_class, objc::sel::get("arrayWithObject:"), crate::string::from_str("b"));
            let both = send0(
                objc::class::ptr(objc::class::lookup(c"NSMutableArray").unwrap()) as Id,
                objc::sel::get("array"),
            );
            let add: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            add(both, objc::sel::get("addObject:"), one);
            add(both, objc::sel::get("addObject:"), two);
            let set = with(set_class, objc::sel::get("setWithArray:"), both);
            assert_eq!(count(set, objc::sel::get("count")), 2);
        }
    }

    /// `-init` on `NSSet` and `-mutableCopy` off it: both substitute, because
    /// both are clusters.
    #[test]
    fn an_immutable_set_substitutes_on_init_and_copies_into_a_mutable_one() {
        crate::bootstrap();
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            let send0: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            let send1: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
            let count: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());
            let set_class = objc::class::ptr(objc::class::lookup(c"NSSet").unwrap()) as Id;
            let set = send0(send0(set_class, objc::sel::get("alloc")), objc::sel::get("init"));
            assert!(!set.is_null());
            // An immutable set is filled through its class, not by writing to it.
            let one = send1(
                set_class,
                objc::sel::get("setWithObject:"),
                crate::string::from_str("x"),
            );
            assert_eq!(count(one, objc::sel::get("count")), 1);
            let copy = send0(one, objc::sel::get("mutableCopy"));
            assert_eq!(
                count(copy, objc::sel::get("count")),
                1,
                "-mutableCopy did not copy what the set held"
            );
            send1(copy, objc::sel::get("addObject:"), crate::string::from_str("y"));
            assert_eq!(count(copy, objc::sel::get("count")), 2);
            assert_eq!(
                count(one, objc::sel::get("count")),
                1,
                "writing to the copy changed the original"
            );
        }
    }

    /// Print every selector `NSData` and `NSMutableData` answer, with the
    /// encoding each declares, so a change to this module's `install()` can be
    /// **diffed** rather than taken on trust.
    ///
    /// This exists because a test pass is not evidence in this project: a wrong
    /// spelling, a method installed on the wrong class, or a method installed
    /// and never reached all look identical to `cargo test`. The guest's own
    /// `__objc_selrefs` are the other half - this says what the runtime offers,
    /// and the census over the images says what the guest asks for, and the
    /// intersection is the only thing worth having.
    ///
    ///     cargo test -p foundation --lib -- --test-threads=1 --nocapture \
    ///         data_census
    #[test]
    fn data_census() {
        crate::bootstrap();
        for name in [c"NSData", c"NSMutableData"] {
            let class = objc::class::lookup(name).expect("registered");
            println!("== {} (metaclass = {})", class.name_str(), {
                // SAFETY: a registered class's name is a NUL-terminated string.
                unsafe { std::ffi::CStr::from_ptr(class.metaclass().name()) }
                    .to_string_lossy()
            });
            for (label, on) in [("instance", class), ("class", class.metaclass())] {
                println!("-- {} {label}", class.name_str());
                let mut rows: Vec<String> = on
                    .chain()
                    .flat_map(|c| c.own_methods())
                    .map(|m| {
                        // SAFETY: a registered method's selector and types are
                        // NUL-terminated strings.
                        let (sel, types) = unsafe {
                            (
                                std::ffi::CStr::from_ptr(objc::sel::name(m.sel)),
                                std::ffi::CStr::from_ptr(m.types()),
                            )
                        };
                        format!(
                            "{} {} {}", sel.to_string_lossy(), types.to_string_lossy(),
                            if m.implementation().is_null() { "NULL-IMP" } else { "ok" }
                        )
                    })
                    .collect();
                rows.sort();
                rows.dedup();
                for row in rows {
                    println!("   {row}");
                }
            }
        }
    }

    /// The five restored selectors are installed, with the **exact** spelling the
    /// guest's `__objc_selrefs` name and the encoding each one's arguments
    /// imply.
    ///
    /// The spelling is the point. A selector off by one character is a
    /// *different* selector: it is registered, the suite is green, and the guest
    /// still aborts by name on the one it actually sends. So each name here is
    /// looked up through `sel::get` and the method has to be found under *that*
    /// selector, which fails loudly if a string and its registration ever drift
    /// apart.
    ///
    /// The encodings are checked because a wrong one fails the same way: a `^v16`
    /// where the guest loads a `char *` still round-trips on a host where both
    /// arrive in the same register, and only the declared encoding says which it
    /// was.
    #[test]
    fn the_restored_data_selectors_are_installed_with_the_guests_spellings() {
        crate::bootstrap();
        let data = objc::class::lookup(c"NSData").expect("NSData");
        let expected = [
            // (selector, encoding, is it a + method?)
            ("data", c"@16@0:8", true),
            ("dataWithContentsOfFile:options:error:", c"@40@0:8@16Q24^@32", true),
            ("initWithBytes:length:", c"@32@0:8r^v16Q24", false),
            ("getBytes:length:", c"v32@0:8^v16Q24", false),
            ("copyWithZone:", c"@24@0:8^v16", false),
        ];
        for (name, types, is_class) in expected {
            let (on, where_) = match is_class {
                true => (data.metaclass(), "a + method"),
                false => (data, "an instance method"),
            };
            let method = on.method(objc::sel::get(name)).unwrap_or_else(|| {
                panic!("[{name}] is not installed on NSData as {where_}")
            });
            // SAFETY: a registered method's types are a NUL-terminated string.
            let declared = unsafe { std::ffi::CStr::from_ptr(method.types()) };
            assert_eq!(declared, types, "[{name}] declares the wrong encoding");
            assert!(!method.implementation().is_null(), "[{name}] has no IMP");
        }
        // `NSMutableData` inherits all three instance methods from `NSData`, so
        // the guest's `[[NSMutableData alloc] initWithBytes:length:]` reaches them
        // without a second registration. Asserted rather than assumed, because
        // `mutable_pair` is what makes it true and that is a fact about
        // corefoundation's class table rather than about this file.
        let mutable = objc::class::lookup(c"NSMutableData").expect("NSMutableData");
        for name in ["initWithBytes:length:", "getBytes:length:", "copyWithZone:"] {
            assert!(
                mutable.method(objc::sel::get(name)).is_some(),
                "NSMutableData does not inherit -[{name}] from NSData"
            );
        }
    }

    /// `+[NSData data]` answers an empty data rather than nil, which is the
    /// answer all four of the guest's sites are built around.
    #[test]
    fn empty_data_is_empty_and_not_null() {
        crate::bootstrap();
        // SAFETY: `+data` is `id (Class, SEL)`.
        let data: Id = unsafe { send(class(c"NSData"), "data", ()) };
        assert!(!data.is_null(), "+[NSData data] answered nil");
        // SAFETY: the value is a data object.
        assert!(unsafe { data_of(data) }.is_empty());
    }

    /// `-initWithBytes:length:` **copies**, and it substitutes for the `+alloc`
    /// placeholder.
    ///
    /// The copy is what the assertion is for: the caller's buffer is overwritten
    /// afterwards and the data is read again. A borrowing implementation would
    /// answer the caller's new bytes, and every one of the guest's six sites
    /// passes a pointer into a vector the caller goes on to use, so "copied" is
    /// the property the whole thing rests on. `length == 0` is the other end, and
    /// neither implementation of the contract treats a null pointer there as a
    /// failure.
    #[test]
    fn init_with_bytes_copies_rather_than_borrows() {
        crate::bootstrap();
        let mut source = b"Chromium".to_vec();
        // SAFETY: `+alloc` is `id (Class, SEL)` and the initialiser is
        // `id (id, SEL, const void *, NSUInteger)`.
        let data: Id = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, *const u8, usize) -> Id =
                std::mem::transmute(objc::msg::entry());
            let allocated: Id = send(class(c"NSData"), "alloc", ());
            f(
                allocated,
                objc::sel::get("initWithBytes:length:"),
                source.as_mut_ptr(),
                source.len(),
            )
        };
        assert!(!data.is_null(), "-initWithBytes:length: answered nil");
        // SAFETY: both values are data objects.
        assert_eq!(unsafe { data_of(data) }, b"Chromium");
        source.fill(b'X');
        // SAFETY: as above.
        assert_eq!(
            unsafe { data_of(data) },
            b"Chromium",
            "-initWithBytes:length: borrowed the caller's buffer"
        );

        // A null pointer with a zero length: not a failure on either
        // implementation, and the case an empty vector reduces to.
        // SAFETY: as above, with a null pointer and no length.
        let empty: Id = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, *const u8, usize) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                send(class(c"NSData"), "alloc", ()),
                objc::sel::get("initWithBytes:length:"),
                std::ptr::null(),
                0,
            )
        };
        assert!(!empty.is_null(), "a null pointer with length 0 must not fail");
        // SAFETY: the value is a data object.
        assert!(unsafe { data_of(empty) }.is_empty());
    }

    /// `-getBytes:length:` copies **no more than the data holds**, and the
    /// caller's buffer is not written past that.
    ///
    /// The canary is the assertion that matters. The buffer is pre-filled with a
    /// pattern and the tail beyond the data's length has to come back untouched:
    /// an implementation that copied `length` bytes regardless - the mistake that
    /// corrupts a guest's stack silently - would write over the canary, and a
    /// caller that sized its buffer for what it asked for would never know.
    ///
    /// The three cases are the three the contract distinguishes: a length inside
    /// the data, a length past the end (clamped, not refused), and the guest's
    /// own `(null, 0)`.
    #[test]
    fn get_bytes_length_never_writes_past_what_the_data_holds() {
        crate::bootstrap();
        // SAFETY: `+dataWithBytes:length:` is
        // `id (Class, SEL, const void *, NSUInteger)`.
        let data: Id = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, *const u8, usize) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                class(c"NSData"),
                objc::sel::get("dataWithBytes:length:"),
                b"0123456789".as_ptr(),
                10,
            )
        };
        const CANARY: u8 = 0xa5;
        let mut buffer = [CANARY; 32];
        // SAFETY: `-getBytes:length:` is `void (id, SEL, void *, NSUInteger)` and
        // the buffer is 32 bytes belonging to this test.
        let get: unsafe extern "C" fn(Id, Sel, *mut u8, usize) =
            unsafe { std::mem::transmute(objc::msg::entry()) };

        // A length inside the data copies exactly that much and nothing else.
        unsafe { get(data, objc::sel::get("getBytes:length:"), buffer.as_mut_ptr(), 4) };
        assert_eq!(&buffer[..4], b"0123");
        assert!(
            buffer[4..].iter().all(|b| *b == CANARY),
            "wrote past the length that was asked for"
        );

        // A length past the end copies what exists - the contract, per GNUstep's
        // "only the available bytes are copied" and swift-corelibs'
        // `min(length, self.length)` - and stops at the end of the data.
        buffer.fill(CANARY);
        unsafe { get(data, objc::sel::get("getBytes:length:"), buffer.as_mut_ptr(), 32) };
        assert_eq!(&buffer[..10], b"0123456789");
        assert!(
            buffer[10..].iter().all(|b| *b == CANARY),
            "a length past the end overwrote {} canary byte(s)",
            buffer[10..].iter().filter(|b| **b != CANARY).count()
        );

        // Zero length with a null buffer: the guest's one site, instruction for
        // instruction (`Framework+0x7a1f086: xor edx,edx; xor ecx,ecx`). It has
        // to return rather than fault, and to write nothing.
        buffer.fill(CANARY);
        unsafe { get(data, objc::sel::get("getBytes:length:"), std::ptr::null_mut(), 0) };
        assert!(
            buffer.iter().all(|b| *b == CANARY),
            "a zero-length read wrote to the buffer"
        );
    }

    /// `-copyWithZone:` answers **the receiver** for an immutable data and a
    /// **new immutable** one for a mutable data, and both branches are asserted
    /// here because the answer is the whole contract.
    ///
    /// This is the test the missing mutability bit made impossible to write.
    /// While `__NSCFData`'s body was a bare `Vec<u8>`, `-copyWithZone:` had to
    /// answer a fresh copy in both cases, and the only property that could be
    /// checked was "the copy does not share storage" - which is true of the
    /// wrong answer too.  The identity of the immutable answer is what
    /// distinguishes Apple's answer from the workaround, and it is what the
    /// guest's one `-copyWithZone:` send actually gets.
    ///
    /// `-copy` is sent rather than `-copyWithZone:` because `NSObject`'s own
    /// implementation *sends* `-copyWithZone:`, so the path exercised is the one
    /// a guest takes.
    #[test]
    fn a_copy_of_a_data_is_the_receiver_unless_it_was_mutable() {
        crate::bootstrap();
        // SAFETY: `+dataWithBytes:length:` is `id (Class, SEL, const void *, NSUInteger)`.
        let with: unsafe extern "C" fn(Id, Sel, *const u8, usize) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let sel = objc::sel::get("dataWithBytes:length:");

        // The immutable case: sent to `NSData`, so the copy is the data itself.
        // SAFETY: as above, with three bytes of our own.
        let immutable = unsafe { with(class(c"NSData"), sel, b"abc".as_ptr(), 3) };
        // SAFETY: `-copy` is `id (id, SEL)`.
        let copied: Id = unsafe { send(immutable, "copy", ()) };
        assert!(!copied.is_null(), "-copy on a data answered nil");
        assert_eq!(
            copied, immutable,
            "an immutable data's copy must be the data itself, which is what \
             Apple returns and what the guest's one -copyWithZone: send gets"
        );
        // SAFETY: the value is a data object.
        assert_eq!(unsafe { data_of(copied) }, b"abc");

        // The mutable case: the same constructor sent to `NSMutableData` gives a
        // data that may be changed, and its copy is a different, immutable one.
        // SAFETY: as above.
        let mutable = unsafe { with(class(c"NSMutableData"), sel, b"abc".as_ptr(), 3) };
        assert_ne!(
            mutable, immutable,
            "two data objects, and not one object handed back twice"
        );
        // SAFETY: as above.
        let mutable_copy: Id = unsafe { send(mutable, "copy", ()) };
        assert!(!mutable_copy.is_null(), "-copy on a mutable data answered nil");
        assert_ne!(
            mutable_copy, mutable,
            "a mutable data's copy must be a new object, or -appendBytes:length: \
             on the copy would change the data that was copied"
        );
        // SAFETY: the value is a data object.
        assert_eq!(unsafe { data_of(mutable_copy) }, b"abc");

        // And the write that motivated the old workaround is now made on the
        // object it is legal on. `-appendBytes:length:` on the *immutable* copy
        // would abort by name, so this is the mutable original: a write to it
        // must not reach its own copy either.
        // SAFETY: `-appendBytes:length:` is
        // `void (id, SEL, const void *, NSUInteger)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, *const u8, usize) =
                std::mem::transmute(objc::msg::entry());
            f(mutable, objc::sel::get("appendBytes:length:"), b"def".as_ptr(), 3);
        }
        // SAFETY: both values are data objects.
        assert_eq!(unsafe { data_of(mutable) }, b"abcdef");
        assert_eq!(
            unsafe { data_of(mutable_copy) },
            b"abc",
            "writing to the data reached the copy taken from it"
        );
    }

    /// The bit is on the body, and it is set by **the class the constructor was
    /// sent to** - which is the only place the distinction exists, because
    /// `+dataWithBytes:length:` is declared on `NSData` and inherited.
    ///
    /// White-box on purpose, and it is white-box for a reason: the *behaviour*
    /// that follows from the bit is asserted in
    /// [`a_copy_of_a_data_is_the_receiver_unless_it_was_mutable`], and the other
    /// behaviour it drives - refusing `-appendBytes:length:` on an immutable data
    /// - ends in `objc::fail!`, which is `abort()` and cannot be caught by a
    /// test.  So this says where the bit is, and that test says what it decides.
    #[test]
    fn a_data_records_the_class_its_constructor_was_sent_to() {
        crate::bootstrap();
        // SAFETY: `+dataWithBytes:length:` is `id (Class, SEL, const void *, NSUInteger)`.
        let with: unsafe extern "C" fn(Id, Sel, *const u8, usize) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let sel = objc::sel::get("dataWithBytes:length:");
        for (name, mutable) in [(c"NSData", false), (c"NSMutableData", true)] {
            // SAFETY: as above, with three bytes of our own.
            let data = unsafe { with(class(name), sel, b"abc".as_ptr(), 3) };
            // SAFETY: the value is a data object this runtime made.
            let recorded = unsafe { collections::data_contents(data as CFTypeRef) }
                .expect("a data with a body")
                .mutable;
            assert_eq!(recorded, mutable, "{name:?}");
        }
    }

    /// `-appendBytes:length:` **works** on a data made mutable, which is the
    /// only way this bit is observable without aborting: the same message on an
    /// immutable data is refused by name, and a refusal is a process abort.
    ///
    /// So this is half a test, and it is labelled as one.  What it does prove is
    /// that the write path is live on a mutable data and that `+[NSMutableData
    /// dataWithBytes:length:]` produces one - i.e. that the mutability flag is
    /// carried from the CoreFoundation constructor all the way through the
    /// bridged class cluster to a method the guest could send.
    #[test]
    fn a_mutable_data_grows() {
        crate::bootstrap();
        // SAFETY: `+dataWithBytes:length:` is `id (Class, SEL, const void *, NSUInteger)`.
        let data: Id = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, *const u8, usize) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                class(c"NSMutableData"),
                objc::sel::get("dataWithBytes:length:"),
                b"ab".as_ptr(),
                2,
            )
        };
        assert!(!data.is_null(), "+[NSMutableData dataWithBytes:length:] answered nil");
        // SAFETY: `-appendBytes:length:` is
        // `void (id, SEL, const void *, NSUInteger)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, *const u8, usize) =
                std::mem::transmute(objc::msg::entry());
            f(data, objc::sel::get("appendBytes:length:"), b"cde".as_ptr(), 3);
        }
        // SAFETY: the value is a data object.
        assert_eq!(unsafe { data_of(data) }, b"abcde");
    }

    /// `+dataWithContentsOfFile:options:error:` writes the out-parameter on
    /// **both** paths, answers nil with a code that names the condition for a
    /// file that is not there, and leaves a null out-parameter alone.
    ///
    /// The canary is a live object rather than a null, because a null would pass
    /// whether or not anything was written - the reason the property-list test in
    /// this module uses one.
    #[test]
    fn data_with_contents_of_file_writes_the_error_on_both_paths() {
        crate::bootstrap();
        // SAFETY: the prototype is the one the selector declares.
        let read: unsafe extern "C" fn(Id, Sel, Id, u64, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let sel = objc::sel::get("dataWithContentsOfFile:options:error:");
        let ns_data = class(c"NSData");

        // A real file, and the out-parameter cleared.
        let path = std::env::temp_dir().join("rosetta3-data-census.bin");
        std::fs::write(&path, b"contents").expect("a file to read");
        let mut out = crate::string::from_str("not an error, and not nil");
        // SAFETY: the prototype is the one the selector declares.
        let data = unsafe {
            read(
                ns_data,
                sel,
                crate::string::from_str(&path.to_string_lossy()),
                0,
                &raw mut out,
            )
        };
        assert!(!data.is_null(), "the file did not read");
        assert!(
            out.is_null(),
            "success must write nil through the out-parameter, not leave it"
        );
        // SAFETY: the value is a data object.
        assert_eq!(unsafe { data_of(data) }, b"contents");

        // A file that is not there: nil, and a code that names the condition.
        let mut out = crate::string::from_str("canary");
        // SAFETY: as above.
        let missing = unsafe {
            read(
                ns_data,
                sel,
                crate::string::from_str("/nonexistent/rosetta3/no-such.bin"),
                0,
                &raw mut out,
            )
        };
        assert!(missing.is_null(), "a missing file is not a data object");
        assert!(!out.is_null(), "the failure must be reported");
        // SAFETY: the value is a number.
        assert_eq!(
            unsafe { send::<(), i64>(out, "code", ()) },
            260,
            "NSFileReadNoSuchFileError, which is what a *read* of a missing file is"
        );

        // A null out-parameter is never written through - the `xor ecx,ecx`
        // shape four of this guest's five property-list sites use.
        // SAFETY: as above, with a null out-parameter.
        let none = unsafe {
            read(
                ns_data,
                sel,
                crate::string::from_str("/nonexistent/rosetta3/no-such.bin"),
                0,
                std::ptr::null_mut(),
            )
        };
        assert!(none.is_null());
        let _ = std::fs::remove_file(&path);
    }

    /// The bytes of a data object, for the assertions above.
    ///
    /// # Safety
    ///
    /// `data` must be null or a data object this runtime made.
    unsafe fn data_of(data: Id) -> Vec<u8> {
        // SAFETY: the caller's obligation.
        match unsafe { collections::data(data as CFTypeRef) } {
            Some(bytes) => bytes.clone(),
            None => Vec::new(),
        }
    }
}


// -----------------------------------------------------------------------------
// What the merge brought and this census removes
//
// Seventeen functions arrived from upstream with their `methods!` rows and are
// gone again here, because every one of them answers a selector this guest does
// **not** send to a data. The census is the table in `install()` above: it counts
// a selector's `__objc_selrefs` loads and the loads followed by a message send,
// over all 13 guest images, and resolves the receiver's class at every site by
// disassembly. Removed:
//
//   -init, -initWithData:, -initWithCapacity:, -initWithLength:,
//   -initWithBytesNoCopy:length:, -initWithBytesNoCopy:length:freeWhenDone:,
//   -initWithContentsOfFile:, -copy, -isEqual:, -subdataWithRange:,
//   -resetBytesInRange:, +dataWithData:, +dataWithCapacity:, +dataWithLength:
//
// The two that look closest to live are not:
//
// * `-initWithData:` measures **2 sends**, and both are to `NSImage` and
//   `NSBitmapImageRep` - other classes' selectors, zero of them a data.
// * `-initWithCapacity:` measures **7 sends**, and **6 are `NSMutableArray` and
//   1 is `NSMutableDictionary`** - again, none a data.
//
// And one row is not merely dead but **wrong**, which is the reason this is not
// simply "add the rows back". Upstream registers
// `"mutableBytes" => data_bytes, c"^v16@0:8"` - that is, the `*const u8` answer
// cast to a `*mut` - and this file has a separate `mutable_bytes` for exactly
// that reason. Handing out a pointer into an immutable data's storage lets a
// caller change a data the runtime has already said cannot change, silently, and
// it is the second thing the mutability bit has to guard. Installing upstream's
// row would reopen the hole `-appendBytes:length:` refuses by name.
//
// If any of these is wanted back, the row needs a send site first.
