use std::ffi::c_void;
use std::sync::{Arc, Mutex, Weak};

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::{api, collections};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};
use crate::geometry::{NSRange, NSRect, NSSize};

type Contents = Arc<Mutex<State>>;

fn class() -> &'static objc::class::Class {
    objc::class::lookup(c"NSAttributedString").expect("NSAttributedString is registered")
}

fn mutable_class() -> &'static objc::class::Class {
    objc::class::lookup(c"NSMutableAttributedString")
        .expect("NSMutableAttributedString is registered")
}

/// Immutable dictionaries can be shared by runs and copies; values are retained.
struct Attributes(usize);

impl Attributes {
    fn from_pairs(pairs: &[(CFTypeRef, CFTypeRef)]) -> Self {
        Self(collections::new_dictionary(
            pairs,
            Policy::from_key_callbacks(),
            Policy::from_type_callbacks(),
            false,
        ) as usize)
    }

    fn snapshot(value: Id) -> Self {
        // SAFETY: the caller supplies a dictionary or nil.
        let pairs = unsafe { collections::dictionary(value.cast()) }
            .map(|d| d.entries().to_vec())
            .unwrap_or_default();
        Self::from_pairs(&pairs)
    }

    fn pairs(&self) -> Vec<(CFTypeRef, CFTypeRef)> {
        // SAFETY: this wrapper owns a dictionary made above.
        unsafe { collections::dictionary(self.0 as CFTypeRef) }
            .expect("attributes dictionary")
            .entries()
            .to_vec()
    }

    fn get(&self, key: Id) -> Id {
        // SAFETY: a dictionary owned by this wrapper and the caller's key.
        let d = unsafe { collections::dictionary(self.0 as CFTypeRef) }.unwrap();
        d.get(key.cast()).map_or(std::ptr::null_mut(), |v| v as Id)
    }

    fn adding(&self, other: &Self) -> Self {
        let mut pairs = self.pairs();
        pairs.extend(other.pairs());
        Self::from_pairs(&pairs)
    }

    fn removing(&self, key: Id) -> Self {
        let pairs: Vec<_> = self
            .pairs()
            .into_iter()
            .filter(|(k, _)| !equal(*k as Id, key))
            .collect();
        Self::from_pairs(&pairs)
    }
}

impl Clone for Attributes {
    fn clone(&self) -> Self {
        Self(objc::arc::retain(self.0 as Id) as usize)
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        objc::arc::release(self.0 as Id);
    }
}

impl PartialEq for Attributes {
    fn eq(&self, other: &Self) -> bool {
        if self.0 == other.0 {
            return true;
        }
        let (a, b) = (self.pairs(), other.pairs());
        a.len() == b.len()
            && a.iter().all(|(key, value)| {
                b.iter()
                    .any(|(k, v)| equal(*key as Id, *k as Id) && equal(*value as Id, *v as Id))
            })
    }
}

fn equal(a: Id, b: Id) -> bool {
    // SAFETY: retained attribute keys and values, or nil.
    unsafe { api::CFEqual(a.cast(), b.cast()) != 0 }
}

#[derive(Clone)]
struct Run {
    length: usize,
    attributes: Attributes,
}

struct State {
    /// Retained CFMutableString, also used by the -mutableString proxy.
    string: usize,
    /// Lengths and ranges count UTF-16 code units, including lone surrogates.
    runs: Vec<Run>,
}

impl Drop for State {
    fn drop(&mut self) {
        objc::arc::release(self.string as Id);
    }
}

impl State {
    fn units(&self) -> Vec<u16> {
        // SAFETY: the backing string is retained by this state.
        unsafe { string_units(self.string as Id) }
    }

    fn length(&self) -> usize {
        // SAFETY: a retained string.
        unsafe { api::CFStringGetLength(self.string as CFTypeRef) as usize }
    }

    fn run_at(&self, index: usize) -> (usize, NSRange) {
        let mut location = 0;
        for (i, run) in self.runs.iter().enumerate() {
            if index < location + run.length {
                return (
                    i,
                    NSRange {
                        location,
                        length: run.length,
                    },
                );
            }
            location += run.length;
        }
        objc::fail!("NSRangeException: attributed string index {index} past length {location}");
    }

    fn slice_runs(&self, range: NSRange) -> Vec<Run> {
        let end = checked_range(range, self.length()).end;
        let mut location = 0;
        let mut runs = Vec::new();
        for run in &self.runs {
            let start = location.max(range.location);
            let stop = (location + run.length).min(end);
            if start < stop {
                runs.push(Run {
                    length: stop - start,
                    attributes: run.attributes.clone(),
                });
            }
            location += run.length;
        }
        runs
    }

    fn coalesce(&mut self) {
        let mut runs: Vec<Run> = Vec::with_capacity(self.runs.len());
        for run in self.runs.drain(..) {
            if run.length == 0 {
                continue;
            }
            if let Some(last) = runs.last_mut()
                && last.attributes == run.attributes
            {
                last.length += run.length;
            } else {
                runs.push(run);
            }
        }
        self.runs = runs;
    }

    fn edit_attributes(&mut self, range: NSRange, edit: impl Fn(&Attributes) -> Attributes) {
        let end = checked_range(range, self.length()).end;
        if range.length == 0 {
            return;
        }
        let mut runs = self.slice_runs(NSRange {
            location: 0,
            length: range.location,
        });
        runs.extend(self.slice_runs(range).into_iter().map(|run| Run {
            length: run.length,
            attributes: edit(&run.attributes),
        }));
        runs.extend(self.slice_runs(NSRange {
            location: end,
            length: self.length() - end,
        }));
        self.runs = runs;
        self.coalesce();
    }

    fn replace(&mut self, range: NSRange, insertion: &[u16], attributes: Option<Vec<Run>>) {
        let mut units = self.units();
        let end = checked_range(range, units.len()).end;
        let inserted = attributes.unwrap_or_else(|| {
            let inherited = if self.runs.is_empty() {
                Attributes::from_pairs(&[])
            } else {
                let index = if range.length == 0 {
                    range.location.saturating_sub(1)
                } else {
                    range.location
                };
                self.runs[self.run_at(index).0].attributes.clone()
            };
            vec![Run {
                length: insertion.len(),
                attributes: inherited,
            }]
        });
        let mut runs = self.slice_runs(NSRange {
            location: 0,
            length: range.location,
        });
        runs.extend(inserted);
        runs.extend(self.slice_runs(NSRange {
            location: end,
            length: units.len() - end,
        }));
        units.splice(range.location..end, insertion.iter().copied());
        self.runs = runs;
        self.coalesce();
        // SAFETY: the state owns a mutable CFString with unchanged identity.
        unsafe { corefoundation::string::set_utf16(self.string as CFTypeRef, &units) };
    }
}

fn checked_range(range: NSRange, length: usize) -> std::ops::Range<usize> {
    let Some(end) = range
        .location
        .checked_add(range.length)
        .filter(|end| *end <= length)
    else {
        objc::fail!("NSRangeException: attributed string range {range:?} past length {length}");
    };
    range.location..end
}

/// # Safety
/// `value` is a string object or nil.
unsafe fn string_units(value: Id) -> Vec<u16> {
    unsafe { corefoundation::string::as_string(value.cast()) }.map_or_else(Vec::new, |s| s.units())
}

/// # Safety
/// `object` is null or an object this runtime allocated.
unsafe fn contents(object: Id) -> Option<Contents> {
    unsafe { crate::boxed::contents::<Contents>(object, class()) }.cloned()
}

fn lock(contents: &Contents) -> std::sync::MutexGuard<'_, State> {
    contents.lock().unwrap_or_else(|e| e.into_inner())
}

fn new_contents(units: &[u16], runs: Vec<Run>) -> Contents {
    let proxy_class =
        objc::class::lookup(c"__NSAttributedStringMutableString").expect("string proxy");
    // SAFETY: the registered subclass preserves the CFString prefix.
    let string = unsafe { corefoundation::string::new_mutable_in(proxy_class) } as Id;
    unsafe { corefoundation::string::set_utf16(string.cast(), units) };
    let contents = Arc::new(Mutex::new(State {
        string: string as usize,
        runs,
    }));
    // SAFETY: the constructor allocated the full proxy layout, initially zeroed.
    unsafe {
        (*(string as *mut StringProxy)).owner = Box::into_raw(Box::new(Arc::downgrade(&contents)))
    };
    contents
}

fn plain_contents(units: &[u16], attributes: Id) -> Contents {
    let runs = if units.is_empty() {
        Vec::new()
    } else {
        vec![Run {
            length: units.len(),
            attributes: Attributes::snapshot(attributes),
        }]
    };
    new_contents(units, runs)
}

unsafe fn snapshot(value: Id) -> (Vec<u16>, Vec<Run>) {
    let Some(contents) = (unsafe { contents(value) }) else {
        return (Vec::new(), Vec::new());
    };
    let state = lock(&contents);
    (state.units(), state.runs.clone())
}

/// The text and its first run's attributes, for AppKit's drawing path.
///
/// # Safety
/// `object` is null or an object this runtime allocated. The returned attributes
/// are borrowed from it and must be retained to outlive a subsequent edit.
pub unsafe fn parts(object: Id) -> Option<(String, Id)> {
    let contents = unsafe { contents(object) }?;
    let state = lock(&contents);
    Some((
        String::from_utf16_lossy(&state.units()),
        state
            .runs
            .first()
            .map_or(std::ptr::null_mut(), |r| r.attributes.0 as Id),
    ))
}

unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    unsafe { crate::boxed::fill(receiver, plain_contents(&[], std::ptr::null_mut())) };
    receiver
}

unsafe extern "C" fn init_with_string(receiver: Id, sel: Sel, value: Id) -> Id {
    unsafe { init_with_string_attributes(receiver, sel, value, std::ptr::null_mut()) }
}

unsafe extern "C" fn init_with_string_attributes(
    receiver: Id,
    _sel: Sel,
    value: Id,
    attributes: Id,
) -> Id {
    let units = unsafe { string_units(value) };
    unsafe { crate::boxed::fill(receiver, plain_contents(&units, attributes)) };
    receiver
}

unsafe extern "C" fn init_with_attributed_string(receiver: Id, _sel: Sel, value: Id) -> Id {
    let (units, runs) = unsafe { snapshot(value) };
    unsafe { crate::boxed::fill(receiver, new_contents(&units, runs)) };
    receiver
}

unsafe extern "C" fn attributed_string_with_string(receiver: Id, _sel: Sel, value: Id) -> Id {
    let class = unsafe { objc::class::from_ptr(receiver.cast()) }.unwrap_or(class());
    let units = unsafe { string_units(value) };
    objc::arc::autorelease(crate::boxed::make(
        class,
        plain_contents(&units, std::ptr::null_mut()),
    ))
}

unsafe extern "C" fn string(receiver: Id, _sel: Sel) -> Id {
    let (units, _) = unsafe { snapshot(receiver) };
    objc::arc::autorelease(corefoundation::string::from_utf16(&units) as Id)
}

unsafe extern "C" fn length(receiver: Id, _sel: Sel) -> usize {
    unsafe { contents(receiver) }.map_or(0, |c| lock(&c).length())
}

unsafe extern "C" fn copy(receiver: Id, _sel: Sel) -> Id {
    let (units, runs) = unsafe { snapshot(receiver) };
    crate::boxed::make(class(), new_contents(&units, runs))
}

unsafe extern "C" fn mutable_copy(receiver: Id, _sel: Sel) -> Id {
    let (units, runs) = unsafe { snapshot(receiver) };
    crate::boxed::make(mutable_class(), new_contents(&units, runs))
}

unsafe extern "C" fn copy_with_zone(receiver: Id, sel: Sel, _zone: *mut c_void) -> Id {
    unsafe { copy(receiver, sel) }
}

unsafe extern "C" fn mutable_copy_with_zone(receiver: Id, sel: Sel, _zone: *mut c_void) -> Id {
    unsafe { mutable_copy(receiver, sel) }
}

unsafe extern "C" fn attributed_substring(receiver: Id, _sel: Sel, range: NSRange) -> Id {
    let Some(contents) = (unsafe { contents(receiver) }) else {
        return std::ptr::null_mut();
    };
    let state = lock(&contents);
    let units = state.units();
    let slice = checked_range(range, units.len());
    objc::arc::autorelease(crate::boxed::make(
        class(),
        new_contents(&units[slice], state.slice_runs(range)),
    ))
}

fn attribute_range(state: &State, name: Option<Id>, index: usize) -> (Id, NSRange) {
    let (run_index, mut range) = state.run_at(index);
    let attributes = &state.runs[run_index].attributes;
    let value = name.map_or(attributes.0 as Id, |name| attributes.get(name));
    if let Some(name) = name {
        for run in state.runs[..run_index].iter().rev() {
            if !equal(run.attributes.get(name), value) {
                break;
            }
            range.location -= run.length;
            range.length += run.length;
        }
        for run in &state.runs[run_index + 1..] {
            if !equal(run.attributes.get(name), value) {
                break;
            }
            range.length += run.length;
        }
    }
    (value, range)
}

unsafe fn query(
    receiver: Id,
    name: Option<Id>,
    index: usize,
    out: *mut NSRange,
    limit: Option<NSRange>,
) -> Id {
    let Some(contents) = (unsafe { contents(receiver) }) else {
        return std::ptr::null_mut();
    };
    let state = lock(&contents);
    let (value, mut range) = attribute_range(&state, name, index);
    if let Some(limit) = limit {
        let end = checked_range(limit, state.length())
            .end
            .min(range.location + range.length);
        range.location = range.location.max(limit.location);
        range.length = end.saturating_sub(range.location);
    }
    if !out.is_null() {
        // SAFETY: a caller-provided optional NSRange output.
        unsafe { *out = range };
    }
    objc::arc::autorelease(objc::arc::retain(value))
}

unsafe extern "C" fn attributes_at_index(
    receiver: Id,
    _sel: Sel,
    index: usize,
    out: *mut NSRange,
) -> Id {
    unsafe { query(receiver, None, index, out, None) }
}

unsafe extern "C" fn attribute_at_index(
    receiver: Id,
    _sel: Sel,
    name: Id,
    index: usize,
    out: *mut NSRange,
) -> Id {
    unsafe { query(receiver, Some(name), index, out, None) }
}

unsafe extern "C" fn attributes_in_range(
    receiver: Id,
    _sel: Sel,
    index: usize,
    out: *mut NSRange,
    limit: NSRange,
) -> Id {
    unsafe { query(receiver, None, index, out, Some(limit)) }
}

unsafe extern "C" fn attribute_in_range(
    receiver: Id,
    _sel: Sel,
    name: Id,
    index: usize,
    out: *mut NSRange,
    limit: NSRange,
) -> Id {
    unsafe { query(receiver, Some(name), index, out, Some(limit)) }
}

unsafe extern "C" fn set_attributes(receiver: Id, _sel: Sel, value: Id, range: NSRange) {
    let attributes = Attributes::snapshot(value);
    if let Some(contents) = unsafe { contents(receiver) } {
        lock(&contents).edit_attributes(range, |_| attributes.clone());
    }
}

unsafe extern "C" fn add_attributes(receiver: Id, _sel: Sel, value: Id, range: NSRange) {
    let attributes = Attributes::snapshot(value);
    if let Some(contents) = unsafe { contents(receiver) } {
        lock(&contents).edit_attributes(range, |a| a.adding(&attributes));
    }
}

unsafe extern "C" fn add_attribute(receiver: Id, _sel: Sel, name: Id, value: Id, range: NSRange) {
    if name.is_null() || value.is_null() {
        objc::fail!(
            "NSInvalidArgumentException: -addAttribute:value:range: requires a name and value"
        );
    }
    let attributes = Attributes::from_pairs(&[(name.cast(), value.cast())]);
    if let Some(contents) = unsafe { contents(receiver) } {
        lock(&contents).edit_attributes(range, |a| a.adding(&attributes));
    }
}

unsafe extern "C" fn remove_attribute(receiver: Id, _sel: Sel, name: Id, range: NSRange) {
    if let Some(contents) = unsafe { contents(receiver) } {
        lock(&contents).edit_attributes(range, |a| a.removing(name));
    }
}

unsafe extern "C" fn replace_characters(receiver: Id, _sel: Sel, range: NSRange, value: Id) {
    let units = unsafe { string_units(value) };
    if let Some(contents) = unsafe { contents(receiver) } {
        lock(&contents).replace(range, &units, None);
    }
}

unsafe extern "C" fn replace_attributed(receiver: Id, _sel: Sel, range: NSRange, value: Id) {
    // Snapshot before taking the receiver lock: self-replacement is legal.
    let (units, runs) = unsafe { snapshot(value) };
    if let Some(contents) = unsafe { contents(receiver) } {
        lock(&contents).replace(range, &units, Some(runs));
    }
}

unsafe extern "C" fn delete_characters(receiver: Id, _sel: Sel, range: NSRange) {
    if let Some(contents) = unsafe { contents(receiver) } {
        lock(&contents).replace(range, &[], Some(Vec::new()));
    }
}

unsafe extern "C" fn insert_attributed(receiver: Id, sel: Sel, value: Id, index: usize) {
    unsafe {
        replace_attributed(
            receiver,
            sel,
            NSRange {
                location: index,
                length: 0,
            },
            value,
        )
    };
}

unsafe extern "C" fn append_attributed_string(receiver: Id, sel: Sel, value: Id) {
    let index = unsafe { length(receiver, sel) };
    unsafe { insert_attributed(receiver, sel, value, index) };
}

unsafe extern "C" fn set_attributed_string(receiver: Id, sel: Sel, value: Id) {
    let length = unsafe { length(receiver, sel) };
    unsafe {
        replace_attributed(
            receiver,
            sel,
            NSRange {
                location: 0,
                length,
            },
            value,
        )
    };
}

// Apple's base implementations are hooks for subclasses and do nothing.
unsafe extern "C" fn editing(_receiver: Id, _sel: Sel) {}

unsafe extern "C" fn script_range(receiver: Id, sel: Sel, range: NSRange) {
    let script = if sel == objc::sel::get("superscriptRange:") {
        1
    } else if sel == objc::sel::get("subscriptRange:") {
        -1
    } else {
        0
    };
    let name = crate::string::from_str("NSSuperscript");
    unsafe { add_attribute(receiver, sel, name, crate::number(script), range) };
    objc::arc::release(name);
}

/// Foundation estimates a canvas until AppKit supplies glyph metrics.
unsafe extern "C" fn size(receiver: Id, _sel: Sel) -> NSSize {
    let (units, _) = unsafe { snapshot(receiver) };
    let count = String::from_utf16_lossy(&units).chars().count();
    NSSize {
        width: count as f64 * 8.0,
        height: if count == 0 { 0.0 } else { 16.0 },
    }
}

unsafe extern "C" fn draw_in_rect(_receiver: Id, _sel: Sel, _rect: NSRect) {}

/// A CFString prefix lets every inherited NSString reader and CF reader work.
/// The weak owner avoids a cycle and permits a retained proxy to outlive it.
#[repr(C)]
struct StringProxy {
    string: corefoundation::string::CFString,
    owner: *mut Weak<Mutex<State>>,
}

unsafe fn proxy_owner(receiver: Id) -> Option<Contents> {
    let owner = unsafe { (*(receiver as *const StringProxy)).owner };
    if owner.is_null() {
        None
    } else {
        unsafe { &*owner }.upgrade()
    }
}

unsafe extern "C" fn mutable_string(receiver: Id, _sel: Sel) -> Id {
    unsafe { contents(receiver) }.map_or(std::ptr::null_mut(), |c| lock(&c).string as Id)
}

unsafe extern "C" fn proxy_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: this class owns the CFString prefix and boxed weak reference.
    unsafe {
        let proxy = receiver as *mut StringProxy;
        if !(*proxy).owner.is_null() {
            drop(Box::from_raw((*proxy).owner));
        }
        corefoundation::string::release_storage(receiver.cast());
    }
    objc::arc::finalize(receiver);
}

unsafe fn proxy_replace(receiver: Id, range: NSRange, insertion: &[u16]) {
    if let Some(owner) = unsafe { proxy_owner(receiver) } {
        lock(&owner).replace(range, insertion, None);
    } else {
        let mut units = unsafe { string_units(receiver) };
        let slice = checked_range(range, units.len());
        units.splice(slice, insertion.iter().copied());
        unsafe { corefoundation::string::set_utf16(receiver.cast(), &units) };
    }
}

unsafe extern "C" fn proxy_replace_characters(receiver: Id, _sel: Sel, range: NSRange, value: Id) {
    let units = unsafe { string_units(value) };
    unsafe { proxy_replace(receiver, range, &units) };
}

unsafe extern "C" fn proxy_delete(receiver: Id, _sel: Sel, range: NSRange) {
    unsafe { proxy_replace(receiver, range, &[]) };
}

unsafe extern "C" fn proxy_insert(receiver: Id, _sel: Sel, value: Id, index: usize) {
    let units = unsafe { string_units(value) };
    unsafe {
        proxy_replace(
            receiver,
            NSRange {
                location: index,
                length: 0,
            },
            &units,
        )
    };
}

unsafe extern "C" fn proxy_append(receiver: Id, sel: Sel, value: Id) {
    let length = unsafe { api::CFStringGetLength(receiver.cast()) as usize };
    unsafe { proxy_insert(receiver, sel, value, length) };
}

unsafe extern "C" fn proxy_append_format(receiver: Id, sel: Sel, value: Id) {
    // As for NSMutableString, the runtime supplies an already-rendered format.
    let bytes = crate::string::unescape_rendered(&unsafe { crate::string::bytes(value) });
    let value = corefoundation::string::from_bytes(&bytes, false) as Id;
    unsafe { proxy_append(receiver, sel, value) };
    objc::arc::release(value);
}

unsafe extern "C" fn proxy_set(receiver: Id, _sel: Sel, value: Id) {
    let units = unsafe { string_units(value) };
    let length = unsafe { api::CFStringGetLength(receiver.cast()) as usize };
    unsafe {
        proxy_replace(
            receiver,
            NSRange {
                location: 0,
                length,
            },
            &units,
        )
    };
}

unsafe extern "C" fn proxy_replace_occurrences(
    receiver: Id,
    _sel: Sel,
    target: Id,
    replacement: Id,
    options: usize,
    range: NSRange,
) -> usize {
    let units = unsafe { string_units(receiver) };
    let needle = unsafe { string_units(target) };
    let insertion = unsafe { string_units(replacement) };
    let slice = checked_range(range, units.len());
    if needle.is_empty() {
        return 0;
    }
    // Match the ASCII case-folding supported by NSMutableString's implementation.
    let fold = |unit: u16| {
        if options & 1 != 0 && unit <= 0x7f {
            u16::from((unit as u8).to_ascii_lowercase())
        } else {
            unit
        }
    };
    let mut matches = Vec::new();
    let mut at = slice.start;
    while at < slice.end {
        if needle.len() <= slice.end - at
            && units[at..at + needle.len()]
                .iter()
                .zip(&needle)
                .all(|(a, b)| fold(*a) == fold(*b))
        {
            matches.push(at);
            at += needle.len();
        } else {
            at += 1;
        }
    }
    for at in matches.iter().rev() {
        unsafe {
            proxy_replace(
                receiver,
                NSRange {
                    location: *at,
                    length: needle.len(),
                },
                &insertion,
            )
        };
    }
    matches.len()
}

unsafe extern "C" fn proxy_copy(receiver: Id, _sel: Sel) -> Id {
    corefoundation::string::from_utf16(&unsafe { string_units(receiver) }) as Id
}

unsafe extern "C" fn proxy_copy_with_zone(receiver: Id, sel: Sel, _zone: *mut c_void) -> Id {
    unsafe { proxy_copy(receiver, sel) }
}

pub(crate) fn make(text: &str) -> Id {
    let units: Vec<_> = text.encode_utf16().collect();
    objc::arc::autorelease(crate::boxed::make(
        class(),
        plain_contents(&units, std::ptr::null_mut()),
    ))
}

pub(crate) fn install() {
    let proxy = corefoundation::base::register(
        c"__NSAttributedStringMutableString",
        Some(corefoundation::string::class()),
        size_of::<StringProxy>(),
        false,
    );
    methods!(proxy, {
        "dealloc" => proxy_dealloc, c"v16@0:8";
        "copy" => proxy_copy, c"@16@0:8";
        "copyWithZone:" => proxy_copy_with_zone, c"@24@0:8^v16";
        "replaceCharactersInRange:withString:" => proxy_replace_characters, c"v40@0:8{_NSRange=QQ}16@32";
        "deleteCharactersInRange:" => proxy_delete, c"v32@0:8{_NSRange=QQ}16";
        "insertString:atIndex:" => proxy_insert, c"v32@0:8@16Q24";
        "appendString:" => proxy_append, c"v24@0:8@16";
        "appendFormat:" => proxy_append_format, c"v24@0:8@16";
        "setString:" => proxy_set, c"v24@0:8@16";
        "replaceOccurrencesOfString:withString:options:range:" => proxy_replace_occurrences, c"Q56@0:8@16@24Q32{_NSRange=QQ}40";
    });
    let class = crate::boxed::register::<Contents>(c"NSAttributedString", None);
    methods!(class, {
        "init" => init, c"@16@0:8";
        "initWithString:" => init_with_string, c"@24@0:8@16";
        "initWithString:attributes:" => init_with_string_attributes, c"@32@0:8@16@24";
        "initWithAttributedString:" => init_with_attributed_string, c"@24@0:8@16";
        "string" => string, c"@16@0:8";
        "length" => length, c"Q16@0:8";
        "copy" => copy, c"@16@0:8";
        "copyWithZone:" => copy_with_zone, c"@24@0:8^v16";
        "mutableCopy" => mutable_copy, c"@16@0:8";
        "mutableCopyWithZone:" => mutable_copy_with_zone, c"@24@0:8^v16";
        "attributedSubstringFromRange:" => attributed_substring, c"@32@0:8{_NSRange=QQ}16";
        "attributesAtIndex:effectiveRange:" => attributes_at_index, c"@32@0:8Q16^{_NSRange=QQ}24";
        "attribute:atIndex:effectiveRange:" => attribute_at_index, c"@40@0:8@16Q24^{_NSRange=QQ}32";
        "attributesAtIndex:longestEffectiveRange:inRange:" => attributes_in_range, c"@48@0:8Q16^{_NSRange=QQ}24{_NSRange=QQ}32";
        "attribute:atIndex:longestEffectiveRange:inRange:" => attribute_in_range, c"@56@0:8@16Q24^{_NSRange=QQ}32{_NSRange=QQ}40";
        "size" => size, c"{CGSize=dd}16@0:8";
        "drawInRect:" => draw_in_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
    });
    class_methods!(class, {
        "attributedStringWithString:" => attributed_string_with_string, c"@24@0:8@16";
    });
    let mutable = crate::boxed::register::<Contents>(c"NSMutableAttributedString", Some(class));
    methods!(mutable, {
        "mutableString" => mutable_string, c"@16@0:8";
        "setAttributes:range:" => set_attributes, c"v40@0:8@16{_NSRange=QQ}24";
        "addAttribute:value:range:" => add_attribute, c"v48@0:8@16@24{_NSRange=QQ}32";
        "addAttributes:range:" => add_attributes, c"v40@0:8@16{_NSRange=QQ}24";
        "removeAttribute:range:" => remove_attribute, c"v40@0:8@16{_NSRange=QQ}24";
        "replaceCharactersInRange:withString:" => replace_characters, c"v40@0:8{_NSRange=QQ}16@32";
        "replaceCharactersInRange:withAttributedString:" => replace_attributed, c"v40@0:8{_NSRange=QQ}16@32";
        "deleteCharactersInRange:" => delete_characters, c"v32@0:8{_NSRange=QQ}16";
        "insertAttributedString:atIndex:" => insert_attributed, c"v32@0:8@16Q24";
        "setAttributedString:" => set_attributed_string, c"v24@0:8@16";
        "appendAttributedString:" => append_attributed_string, c"v24@0:8@16";
        "beginEditing" => editing, c"v16@0:8";
        "endEditing" => editing, c"v16@0:8";
        "superscriptRange:" => script_range, c"v32@0:8{_NSRange=QQ}16";
        "subscriptRange:" => script_range, c"v32@0:8{_NSRange=QQ}16";
        "unscriptRange:" => script_range, c"v32@0:8{_NSRange=QQ}16";
    });
}
