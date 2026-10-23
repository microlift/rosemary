use std::ffi::{CString, c_char};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use machish::error::Result;
use machish::exports::ExportTable;
use machish::report::LinkReport;

/// `__cxxabiv1::__si_class_type_info`, which is `{ vtable, name, base }`.
///
/// A root class uses the same three words with `base` zero and the *no-bases*
/// vtable, which reads only the first two - so one layout serves both.
#[repr(C)]
struct TypeInfo {
    /// Set by [`bind`], because it is an address in the vendored `libc++abi`
    /// and that image has not been placed when the record is made.
    vptr: u64,
    name: *const c_char,
    base: u64,
}

// SAFETY: `name` points at a leaked `CString` and is never written after the
// record is made; `vptr` is written once, before any guest code runs.
unsafe impl Send for TypeInfo {}

/// Every record made so far, by class name, so that two images asking for
/// `_OBJC_EHTYPE_$_NSException` get the same address - which is what makes
/// libc++abi's pointer comparison of two `type_info`s correct.
static RECORDS: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

/// The two `type_info` vtables, from the vendored `libc++abi`, and the
/// personality routine that goes with them.
///
/// Statics rather than arguments because a record is also made on the *throw*
/// path, long after [`bind`] has run and with nothing there to pass them.
static NO_BASES: AtomicU64 = AtomicU64::new(0);
static ONE_BASE: AtomicU64 = AtomicU64::new(0);

/// The record `@catch (id)` names, `_OBJC_EHTYPE_id`.
const ID_NAME: &str = "id";

/// An interned record by name.
fn interned(name: &str) -> Option<u64> {
    RECORDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, addr)| *addr)
}

/// Make and intern a record, or `None` before the vtables are known.
fn make(name: &str, base: u64) -> Option<u64> {
    let vptr = match base {
        0 => NO_BASES.load(Ordering::Acquire),
        _ => ONE_BASE.load(Ordering::Acquire),
    };
    if vptr == 0 {
        return None;
    }
    let text = CString::new(name).ok()?;
    let record = Box::leak(Box::new(TypeInfo {
        vptr,
        name: Box::leak(text.into_boxed_c_str()).as_ptr(),
        base,
    }));
    let addr = record as *const TypeInfo as usize as u64;
    RECORDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((name.to_string(), addr));
    Some(addr)
}

/// The `type_info` for `id`.
///
/// libobjc's personality routine special-cases this record to match any
/// Objective-C object. Here it is instead the base of every root class's
/// record, so libc++abi's ordinary base-chain walk reaches it from any thrown
/// object and `@catch (id)` needs no personality of its own.
pub fn id_record() -> Option<u64> {
    interned(ID_NAME).or_else(|| make(ID_NAME, 0))
}

/// The `type_info` for `class`, making it and its superclasses' if needed.
///
/// `None` before [`bind`] has found the vtables, and for a class this runtime
/// does not have: a record whose base chain stops short would answer "not a
/// subclass" for a class that is one, which is a wrong catch rather than a
/// missing one.
pub fn record_for(class: &'static objc::class::Class) -> Option<u64> {
    let name = class.name_str().to_string();
    if let Some(addr) = interned(&name) {
        return Some(addr);
    }
    let base = match class.superclass() {
        Some(parent) => record_for(parent)?,
        None => id_record()?,
    };
    make(&name, base)
}

/// Rewrite the `type_info` records the *guest* laid out for its own classes.
///
/// clang emits `_OBJC_EHTYPE_$_Foo` into the image that implements `Foo`, as
/// `{ objc_ehtype_vtable + 2, "Foo", OBJC_CLASS_$_Foo }` - a libobjc structure
/// whose vtable this runtime has nothing to bind, so its first word is zero and
/// the vendored `libc++abi` calls through it. Each is rewritten in place into
/// the `__si_class_type_info` libc++abi can read: the vtable, and the third word
/// from the class to the superclass's record. The name in the middle already
/// says which class it is, which is what makes the rewrite possible without a
/// symbol table.
///
/// Separate from [`bind`] because it needs the guest's classes to be
/// *registered*, and that happens long after linking - see
/// [`crate::objc::register`].
pub fn repair_image_records(mem: &mut dyn crate::abi::GuestMemory, report: &LinkReport) {
    let one_base = ONE_BASE.load(Ordering::Acquire);
    if one_base == 0 {
        return;
    }
    for image in &report.images {
        let slots = image
            .missing
            .iter()
            .filter(|m| m.symbol == EHTYPE_VTABLE)
            .map(|m| m.slot_va)
            .chain(
                image
                    .bindings
                    .iter()
                    .filter(|b| b.symbol == EHTYPE_VTABLE)
                    .map(|b| b.slot_va),
            );
        for record in slots {
            let Some(class) = class_named_in(mem, record) else {
                continue;
            };
            let base = match class.superclass() {
                Some(parent) => record_for(parent),
                None => id_record(),
            };
            let Some(base) = base else {
                continue;
            };
            let _ = mem.write(record, &one_base.to_le_bytes());
            let _ = mem.write(record.wrapping_add(16), &base.to_le_bytes());
        }
    }
}

/// The symbol whose slot *is* an in-image record's vtable word.
const EHTYPE_VTABLE: &str = "_objc_ehtype_vtable";

/// The class an in-image `type_info` names, read out of its `name` word.
fn class_named_in(
    mem: &dyn crate::abi::GuestMemory,
    record: u64,
) -> Option<&'static objc::class::Class> {
    /// A class name past this is not one, and the read has to stop somewhere.
    const MAX_NAME: usize = 256;
    let mut word = [0u8; 8];
    mem.read(record.wrapping_add(8), &mut word).ok()?;
    let at = u64::from_le_bytes(word);
    let mut name = Vec::new();
    let mut byte = [0u8; 1];
    for i in 0..MAX_NAME {
        mem.read(at.wrapping_add(i as u64), &mut byte).ok()?;
        if byte[0] == 0 {
            break;
        }
        name.push(byte[0]);
    }
    objc::class::lookup(&CString::new(name).ok()?)
}

/// Point every `_OBJC_EHTYPE_$_` and `_OBJC_EHTYPE_id` binding at a record, and
/// `___objc_personality_v0` at the vendored `__gxx_personality_v0`.
///
/// Runs after [`crate::cxxlib::bind_cxxabi_to_the_runtime`], because the
/// vtable addresses it needs are that image's.
pub fn bind<M: machish::memory::Regions>(
    report: &mut machish::report::LinkReport,
    memory: &mut M,
    exports: &ExportTable,
    is_libcxxabi: &dyn Fn(&str) -> bool,
) -> Result<usize> {
    // Under the host's libobjc every one of these symbols is Apple's, and its
    // `type_info` records describe Apple's classes.
    if !crate::frameworks::using_builtin() {
        return Ok(0);
    }
    let Some(slide) = report
        .images
        .iter()
        .find(|i| is_libcxxabi(&i.name))
        .map(|i| i.slide)
        .or_else(|| crate::dyld::program_image_slide(is_libcxxabi))
    else {
        // No C++ runtime is no exceptions, so there is nothing for a type record
        // to be compared against.
        return Ok(0);
    };
    let at = |symbol: &str| exports.get(symbol).and_then(|e| e.address(slide));

    // Two words in, which is where a C++ vtable *pointer* points: past the
    // offset-to-top and the type_info pointer every vtable begins with.
    let (Some(no_bases), Some(one_base), Some(personality)) = (
        at("__ZTVN10__cxxabiv117__class_type_infoE").map(|a| a.wrapping_add(16)),
        at("__ZTVN10__cxxabiv120__si_class_type_infoE").map(|a| a.wrapping_add(16)),
        at("___gxx_personality_v0"),
    ) else {
        return Ok(0);
    };

    // The libobjc entry points that are their C++ counterpart and nothing else.
    // They are bound here rather than served as shims because a shim would be a
    // host frame in the middle of an unwind. `objc_begin_catch` is not among
    // them: it returns the object rather than the exception buffer, which is one
    // dereference and therefore not an alias.
    let aliases = [
        ("_objc_end_catch", "___cxa_end_catch"),
        ("_objc_exception_rethrow", "___cxa_rethrow"),
    ];

    NO_BASES.store(no_bases, Ordering::Release);
    ONE_BASE.store(one_base, Ordering::Release);

    let mut bound = 0;
    // The type records this runtime publishes, and out of `missing` rather than `bindings`: a
    // symbol no provider answered leaves no binding record at all, only the slot
    // it did not write. That slot is precisely the zero the LSDA reads as
    // `catch (...)`.
    for image in &mut report.images {
        let mut satisfied = Vec::new();
        for (i, missing) in image.missing.iter().enumerate() {
            let record = if missing.symbol == "_OBJC_EHTYPE_id" {
                id_record()
            } else if missing.symbol == "___objc_personality_v0" {
                // Nothing else exports it, so it is only ever missing.
                Some(personality)
            } else if let Some(class) = missing.symbol.strip_prefix("_OBJC_EHTYPE_$_") {
                std::ffi::CString::new(class)
                    .ok()
                    .and_then(|n| objc::class::lookup(&n))
                    .and_then(record_for)
            } else {
                continue;
            };
            let Some(record) = record else {
                continue;
            };
            memory.write(missing.slot_va, &record.to_le_bytes())?;
            satisfied.push((i, record));
        }
        for (i, record) in satisfied.iter().rev() {
            let was = image.missing.remove(*i);
            image.bindings.push(machish::report::BindingRecord {
                slot_va: was.slot_va,
                stub_va: was.stub_va,
                library: was.library,
                symbol: was.symbol,
                resolved: *record,
                kind: machish::fixups::BindKind::NonLazy,
                weak_import: was.weak_import,
                tag: 0,
            });
            bound += 1;
        }
    }

    for image in &mut report.images {
        for binding in &mut image.bindings {
            let addr = if binding.symbol == "___objc_personality_v0" {
                personality
            } else if let Some((_, cxx)) = aliases.iter().find(|(objc, _)| *objc == binding.symbol)
            {
                // Unconditionally, over whatever provider answered: this
                // runtime publishes these names so that a call to one is
                // refused by name rather than silently unresolved, and the
                // vendored library is the real implementation.
                match at(cxx) {
                    Some(addr) => addr,
                    None => continue,
                }
            } else {
                continue;
            };
            memory.write(binding.slot_va, &addr.to_le_bytes())?;
            binding.resolved = addr;
            binding.tag = 0;
            bound += 1;
        }
    }
    if crate::shims::vmmap::tracing() {
        eprintln!("rosemary: objc exceptions: {bound} binding(s) repaired");
    }
    Ok(bound)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three properties libc++abi reads a record for, in one test because
    /// they share the statics [`bind`] fills in and separate tests would race
    /// over them.
    #[test]
    fn a_record_is_a_class_type_info_for_its_class() {
        foundation::bootstrap();
        // Nothing is published before the vendored library has been found: a
        // record with a null vtable is a crash inside libc++abi, which is worse
        // than the binding it replaces.
        let exception = objc::class::lookup(c"NSException").expect("NSException");
        assert_eq!(record_for(exception), None, "a record without the vtables");

        NO_BASES.store(0x1000, Ordering::Release);
        ONE_BASE.store(0x2000, Ordering::Release);

        let string = objc::class::lookup(c"NSString").expect("NSString");
        let first = record_for(string).expect("a record");
        assert_eq!(record_for(string), Some(first), "not interned");

        let object = objc::class::lookup(c"NSObject").expect("NSObject");
        // SAFETY: addresses this module leaked, whose `name` is a leaked CStr.
        unsafe {
            let record = &*(first as *const TypeInfo);
            // The base chain is the class chain, which is what makes
            // `@catch (NSObject *)` take an `NSString`.
            assert_eq!(Some(record.base), record_for(object));
            assert_eq!(
                std::ffi::CStr::from_ptr(record.name).to_str(),
                Ok("NSString")
            );
            // The root class hangs off `id`, which uses the vtable with no
            // base word; everything else uses the one with it.
            let root = &*(record.base as *const TypeInfo);
            assert_eq!(root.vptr, 0x2000);
            assert_eq!(Some(root.base), id_record());
            let id = &*(root.base as *const TypeInfo);
            assert_eq!(id.base, 0);
            assert_eq!(id.vptr, 0x1000);
            assert_eq!(record.vptr, 0x2000);
        }
    }
}
