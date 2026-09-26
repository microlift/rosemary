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
unsafe extern "C" fn data_with_contents_of_file(_class: Id, _sel: Sel, path: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let path = unsafe { crate::string::bytes(path) };
    let Ok(path) = String::from_utf8(path) else {
        return std::ptr::null_mut();
    };
    // The guest named it, so it is read at this host's spelling.
    let Ok(bytes) = std::fs::read(prefix::ns::host_path(&path)) else {
        return std::ptr::null_mut();
    };
    let data = collections::new_data(bytes);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(data) as Id }
}

/// `+dataWithContentsOfFile:options:error:`. The reading options only tune
/// how the file is read (mapped, uncached), not what is answered.
unsafe extern "C" fn data_with_contents_of_file_options(
    class: Id,
    sel: Sel,
    path: Id,
    _options: u64,
    error: *mut Id,
) -> Id {
    // SAFETY: forwarded unchanged.
    let data = unsafe { data_with_contents_of_file(class, sel, path) };
    if !error.is_null() {
        // `NSFileReadNoSuchFileError` or nothing: the caller passed a writable
        // `NSError **`.
        unsafe {
            *error = match data.is_null() {
                true => crate::error::make("NSCocoaErrorDomain", 260, "The file couldn't be opened."),
                false => std::ptr::null_mut(),
            };
        }
    }
    data
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

unsafe extern "C" fn data_length(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: a data object.
    unsafe { api::CFDataGetLength(receiver as CFTypeRef) as usize }
}

unsafe extern "C" fn data_bytes(receiver: Id, _sel: Sel) -> *const u8 {
    // SAFETY: a data object.
    unsafe { api::CFDataGetBytePtr(receiver as CFTypeRef) }
}

unsafe extern "C" fn data_with_bytes(_class: Id, _sel: Sel, bytes: *const u8, length: usize) -> Id {
    // SAFETY: the caller says `bytes` covers `length` bytes.
    unsafe { api::CFAutorelease(api::CFDataCreate(std::ptr::null(), bytes, length as isize)) as Id }
}

/// `+[NSData dataWithBytesNoCopy:length:]` with an owned compatibility copy.
///
/// CoreFoundation's data storage owns a `Vec`, so use the same compatibility
/// behavior as `CFDataCreateWithBytesNoCopy` rather than borrowing guest memory.
unsafe extern "C" fn data_with_bytes_no_copy(
    _class: Id,
    _sel: Sel,
    bytes: *const u8,
    length: usize,
) -> Id {
    // SAFETY: the caller says `bytes` covers `length` bytes.
    unsafe {
        api::CFAutorelease(api::CFDataCreateWithBytesNoCopy(
            std::ptr::null(),
            bytes,
            length as isize,
            std::ptr::null(),
        )) as Id
    }
}

/// The bytes of a data object, or empty for nil or anything else.
///
/// # Safety
///
/// `object` is nil or a live object.
unsafe fn data_contents<'a>(object: Id) -> &'a [u8] {
    match unsafe { collections::data(object as CFTypeRef) } {
        Some(bytes) => bytes.as_slice(),
        None => &[],
    }
}

/// `bytes` for `length` bytes as a slice; empty for a null pointer.
///
/// # Safety
///
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

fn autoreleased_data(bytes: Vec<u8>) -> Id {
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(collections::new_data(bytes)) as Id }
}

unsafe extern "C" fn data_init(receiver: Id, _sel: Sel) -> Id {
    replace_receiver(receiver, Vec::new())
}

unsafe extern "C" fn data_init_with_data(receiver: Id, _sel: Sel, other: Id) -> Id {
    // SAFETY: a data object or nil.
    replace_receiver(receiver, unsafe { data_contents(other) }.to_vec())
}

unsafe extern "C" fn data_init_with_bytes(receiver: Id, _sel: Sel, bytes: *const u8, length: usize) -> Id {
    // SAFETY: the caller says `bytes` covers `length` bytes.
    replace_receiver(receiver, unsafe { raw(bytes, length) }.to_vec())
}

/// `-initWithBytesNoCopy:length:freeWhenDone:`. Copied, as
/// [`data_with_bytes_no_copy`] explains, and the caller's buffer released
/// now when it handed it over.
unsafe extern "C" fn data_init_no_copy_free(
    receiver: Id,
    _sel: Sel,
    bytes: *mut u8,
    length: usize,
    free: bool,
) -> Id {
    // SAFETY: the caller says `bytes` covers `length` bytes.
    let copy = unsafe { raw(bytes, length) }.to_vec();
    if free && !bytes.is_null() {
        // SAFETY: a `malloc` block the caller gave away.
        unsafe { darwin::heap::free(bytes.cast()) };
    }
    replace_receiver(receiver, copy)
}

unsafe extern "C" fn data_init_no_copy(receiver: Id, sel: Sel, bytes: *mut u8, length: usize) -> Id {
    // SAFETY: forwarded; `-initWithBytesNoCopy:length:` frees when done.
    unsafe { data_init_no_copy_free(receiver, sel, bytes, length, true) }
}

unsafe extern "C" fn data_init_with_contents_of_file(receiver: Id, sel: Sel, path: Id) -> Id {
    objc::arc::release(receiver);
    // SAFETY: forwarded; the answer is autoreleased, and `-init` answers +1.
    objc::arc::retain(unsafe { data_with_contents_of_file(receiver, sel, path) })
}

unsafe extern "C" fn data_init_with_capacity(receiver: Id, _sel: Sel, capacity: usize) -> Id {
    replace_receiver(receiver, Vec::with_capacity(capacity.min(1 << 24)))
}

unsafe extern "C" fn data_init_with_length(receiver: Id, _sel: Sel, length: usize) -> Id {
    replace_receiver(receiver, vec![0; length])
}

unsafe extern "C" fn data_empty(_class: Id, _sel: Sel) -> Id {
    autoreleased_data(Vec::new())
}

unsafe extern "C" fn data_with_data(_class: Id, _sel: Sel, other: Id) -> Id {
    // SAFETY: a data object or nil.
    autoreleased_data(unsafe { data_contents(other) }.to_vec())
}

unsafe extern "C" fn data_with_capacity(_class: Id, _sel: Sel, capacity: usize) -> Id {
    autoreleased_data(Vec::with_capacity(capacity.min(1 << 24)))
}

unsafe extern "C" fn data_with_length(_class: Id, _sel: Sel, length: usize) -> Id {
    autoreleased_data(vec![0; length])
}

unsafe extern "C" fn data_copy(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a data object.
    collections::new_data(unsafe { data_contents(receiver) }.to_vec()) as Id
}

unsafe extern "C" fn data_copy_with_zone(receiver: Id, sel: Sel, _zone: *mut std::ffi::c_void) -> Id {
    // SAFETY: forwarded.
    unsafe { data_copy(receiver, sel) }
}

unsafe extern "C" fn data_is_equal(receiver: Id, _sel: Sel, other: Id) -> bool {
    // SAFETY: a data object, and nil or a live object; anything that is not
    // data is not equal.
    let Some(theirs) = (unsafe { collections::data(other as CFTypeRef) }) else {
        return false;
    };
    (unsafe { data_contents(receiver) }) == theirs.as_slice()
}

unsafe extern "C" fn data_hash(receiver: Id, _sel: Sel) -> usize {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // SAFETY: a data object.
    unsafe { data_contents(receiver) }.hash(&mut hasher);
    hasher.finish() as usize
}

unsafe extern "C" fn data_get_bytes_length(receiver: Id, _sel: Sel, out: *mut u8, length: usize) {
    // SAFETY: a data object.
    let bytes = unsafe { data_contents(receiver) };
    let count = length.min(bytes.len());
    if !out.is_null() {
        // SAFETY: the caller says `out` holds `length` bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, count) };
    }
}

unsafe extern "C" fn data_get_bytes(receiver: Id, sel: Sel, out: *mut u8) {
    // SAFETY: the caller's buffer holds the whole of the data.
    let length = unsafe { data_contents(receiver) }.len();
    unsafe { data_get_bytes_length(receiver, sel, out, length) };
}

/// The bytes `location..location+length`, or `NSRangeException` by name.
///
/// # Safety
///
/// `receiver` is a data object.
unsafe fn data_range<'a>(receiver: Id, location: usize, length: usize, what: &str) -> &'a [u8] {
    let bytes = unsafe { data_contents(receiver) };
    match location.checked_add(length).filter(|end| *end <= bytes.len()) {
        Some(end) => &bytes[location..end],
        None => objc::fail!(
            "{what}: range {{{location}, {length}}} exceeds data length {}",
            bytes.len()
        ),
    }
}

unsafe extern "C" fn data_get_bytes_range(
    receiver: Id,
    _sel: Sel,
    out: *mut u8,
    location: usize,
    length: usize,
) {
    // SAFETY: a data object.
    let bytes = unsafe { data_range(receiver, location, length, "-getBytes:range:") };
    if !out.is_null() {
        // SAFETY: the caller says `out` holds `length` bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    }
}

unsafe extern "C" fn data_subdata(receiver: Id, _sel: Sel, location: usize, length: usize) -> Id {
    // SAFETY: a data object.
    autoreleased_data(unsafe { data_range(receiver, location, length, "-subdataWithRange:") }.to_vec())
}

unsafe extern "C" fn data_set_length(receiver: Id, _sel: Sel, length: usize) {
    // SAFETY: a data object.
    if let Some(bytes) = unsafe { collections::data(receiver as CFTypeRef) } {
        bytes.resize(length, 0);
    }
}

unsafe extern "C" fn data_increase_length(receiver: Id, _sel: Sel, extra: usize) {
    // SAFETY: a data object.
    if let Some(bytes) = unsafe { collections::data(receiver as CFTypeRef) } {
        let length = bytes.len().saturating_add(extra);
        bytes.resize(length, 0);
    }
}

unsafe extern "C" fn data_append_data(receiver: Id, _sel: Sel, other: Id) {
    // SAFETY: data objects; copied first, since `other` may be the receiver.
    let theirs = unsafe { data_contents(other) }.to_vec();
    if let Some(bytes) = unsafe { collections::data(receiver as CFTypeRef) } {
        bytes.extend_from_slice(&theirs);
    }
}

unsafe extern "C" fn data_replace_bytes(
    receiver: Id,
    _sel: Sel,
    location: usize,
    length: usize,
    with: *const u8,
) {
    // SAFETY: a data object; `with` covers `length` bytes by contract.
    unsafe { data_range(receiver, location, length, "-replaceBytesInRange:withBytes:") };
    let replacement = unsafe { raw(with, length) }.to_vec();
    if let Some(bytes) = unsafe { collections::data(receiver as CFTypeRef) } {
        bytes[location..location + length].copy_from_slice(&replacement);
    }
}

unsafe extern "C" fn data_reset_bytes(receiver: Id, _sel: Sel, location: usize, length: usize) {
    // SAFETY: a data object.
    unsafe { data_range(receiver, location, length, "-resetBytesInRange:") };
    if let Some(bytes) = unsafe { collections::data(receiver as CFTypeRef) } {
        bytes[location..location + length].fill(0);
    }
}

unsafe extern "C" fn append_bytes(receiver: Id, _sel: Sel, bytes: *const u8, length: usize) {
    // SAFETY: a data object.
    let Some(d) = (unsafe { collections::data(receiver as CFTypeRef) }) else {
        return;
    };
    if bytes.is_null() {
        return;
    }
    // SAFETY: the caller says `bytes` covers `length` bytes.
    d.extend_from_slice(unsafe { std::slice::from_raw_parts(bytes, length) });
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

/// The unsigned 64-bit constructors. A value past `i64::MAX` is kept as its bit
/// pattern, which `-unsignedLongLongValue` reads back unchanged.
unsafe extern "C" fn number_with_unsigned_long(class: Id, sel: Sel, value: u64) -> Id {
    // SAFETY: the same receiver and selector, forwarded.
    unsafe { number_with_integer(class, sel, value as i64) }
}

unsafe extern "C" fn number_with_unsigned_int(class: Id, sel: Sel, value: u32) -> Id {
    // SAFETY: as above.
    unsafe { number_with_integer(class, sel, i64::from(value)) }
}

unsafe extern "C" fn number_with_short(class: Id, sel: Sel, value: i16) -> Id {
    // SAFETY: as above.
    unsafe { number_with_int(class, sel, i32::from(value)) }
}

unsafe extern "C" fn number_with_unsigned_short(class: Id, sel: Sel, value: u16) -> Id {
    // SAFETY: as above.
    unsafe { number_with_int(class, sel, i32::from(value)) }
}

unsafe extern "C" fn number_with_char(class: Id, sel: Sel, value: i8) -> Id {
    // SAFETY: as above.
    unsafe { number_with_int(class, sel, i32::from(value)) }
}

unsafe extern "C" fn number_with_unsigned_char(class: Id, sel: Sel, value: u8) -> Id {
    // SAFETY: as above.
    unsafe { number_with_int(class, sel, i32::from(value)) }
}

unsafe extern "C" fn number_with_float(class: Id, sel: Sel, value: f32) -> Id {
    // SAFETY: as above.
    unsafe { number_with_double(class, sel, f64::from(value)) }
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

pub(crate) fn install() {
    let set = objc::class::lookup(c"NSSet").expect("CoreFoundation registered NSSet");
    class_methods!(set, {
        "setWithObject:" => set_with_object, c"@24@0:8@16";
        "setWithCapacity:" => set_with_capacity, c"@24@0:8Q16";
    });
    methods!(set, {
        "count" => set_count, c"Q16@0:8";
        "containsObject:" => set_contains_object, c"B24@0:8@16";
        "anyObject" => set_any_object, c"@16@0:8";
    });
    let mutable_set = objc::class::lookup(c"NSMutableSet").expect("NSMutableSet");
    methods!(mutable_set, {
        "addObject:" => set_add_object, c"v24@0:8@16";
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
    });
    class_methods!(data, {
        "dataWithBytes:length:" => data_with_bytes, c"@32@0:8^v16Q24";
        "dataWithBytesNoCopy:length:" => data_with_bytes_no_copy, c"@32@0:8^v16Q24";
        "dataWithContentsOfFile:" => data_with_contents_of_file, c"@24@0:8@16";
        "dataWithContentsOfFile:options:error:"
            => data_with_contents_of_file_options, c"@40@0:8@16Q24^@32";
    });
    methods!(data, {
        "writeToFile:atomically:" => write_to_file, c"B28@0:8@16c24";
        "init" => data_init, c"@16@0:8";
        "initWithData:" => data_init_with_data, c"@24@0:8@16";
        "initWithBytes:length:" => data_init_with_bytes, c"@32@0:8r^v16Q24";
        "initWithBytesNoCopy:length:" => data_init_no_copy, c"@32@0:8^v16Q24";
        "initWithBytesNoCopy:length:freeWhenDone:" => data_init_no_copy_free, c"@36@0:8^v16Q24B32";
        "initWithContentsOfFile:" => data_init_with_contents_of_file, c"@24@0:8@16";
        "copy" => data_copy, c"@16@0:8";
        "copyWithZone:" => data_copy_with_zone, c"@24@0:8^v16";
        "mutableCopy" => data_copy, c"@16@0:8";
        "mutableCopyWithZone:" => data_copy_with_zone, c"@24@0:8^v16";
        "isEqual:" => data_is_equal, c"B24@0:8@16";
        "isEqualToData:" => data_is_equal, c"B24@0:8@16";
        "hash" => data_hash, c"Q16@0:8";
        "getBytes:" => data_get_bytes, c"v24@0:8^v16";
        "getBytes:length:" => data_get_bytes_length, c"v32@0:8^v16Q24";
        "getBytes:range:" => data_get_bytes_range, c"v40@0:8^v16{_NSRange=QQ}24";
        "subdataWithRange:" => data_subdata, c"@32@0:8{_NSRange=QQ}16";
    });
    class_methods!(data, {
        "data" => data_empty, c"@16@0:8";
        "dataWithData:" => data_with_data, c"@24@0:8@16";
    });
    methods!(mutable_data, {
        "appendBytes:length:" => append_bytes, c"v32@0:8^v16Q24";
        "mutableBytes" => data_bytes, c"^v16@0:8";
        "initWithCapacity:" => data_init_with_capacity, c"@24@0:8Q16";
        "initWithLength:" => data_init_with_length, c"@24@0:8Q16";
        "setLength:" => data_set_length, c"v24@0:8Q16";
        "increaseLengthBy:" => data_increase_length, c"v24@0:8Q16";
        "appendData:" => data_append_data, c"v24@0:8@16";
        "replaceBytesInRange:withBytes:" => data_replace_bytes, c"v40@0:8{_NSRange=QQ}16r^v32";
        "resetBytesInRange:" => data_reset_bytes, c"v32@0:8{_NSRange=QQ}16";
    });
    class_methods!(mutable_data, {
        "dataWithCapacity:" => data_with_capacity, c"@24@0:8Q16";
        "dataWithLength:" => data_with_length, c"@24@0:8Q16";
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
        "numberWithUnsignedInteger:" => number_with_unsigned_long, c"@24@0:8Q16";
        "numberWithUnsignedLong:" => number_with_unsigned_long, c"@24@0:8Q16";
        "numberWithUnsignedLongLong:" => number_with_unsigned_long, c"@24@0:8Q16";
        "numberWithUnsignedInt:" => number_with_unsigned_int, c"@24@0:8I16";
        "numberWithShort:" => number_with_short, c"@24@0:8s16";
        "numberWithUnsignedShort:" => number_with_unsigned_short, c"@24@0:8S16";
        "numberWithChar:" => number_with_char, c"@24@0:8c16";
        "numberWithUnsignedChar:" => number_with_unsigned_char, c"@24@0:8C16";
        "numberWithFloat:" => number_with_float, c"@24@0:8f16";
    });
}
