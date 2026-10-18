use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

use crate::class::{Class, Imp};
use crate::object::Id;
use crate::sel::Sel;

/// `Block_layout`'s flags word.
mod flag {
    /// The block is being deallocated: bit 0, below the count.
    pub const DEALLOCATING: i32 = 1;
    /// The reference count, in units of two so bit 0 stays the latch.
    pub const REFCOUNT_MASK: i32 = 0xfffe;
    /// The descriptor is the 32-bit-offset form, which this does not decode.
    pub const SMALL_DESCRIPTOR: i32 = 1 << 22;
    /// The block is on the heap and `free` owns it.
    pub const NEEDS_FREE: i32 = 1 << 24;
    /// The descriptor carries `copy` and `dispose` helpers.
    pub const HAS_COPY_DISPOSE: i32 = 1 << 25;
    /// The block is in `__DATA` and copying it is a no-op.
    pub const IS_GLOBAL: i32 = 1 << 28;
    /// The block returns a struct through a hidden first argument.
    pub const USE_STRET: i32 = 1 << 29;
}

/// `Block_byref`'s flags word, which shares the low bits with the above.
mod byref_flag {
    pub const NEEDS_FREE: i32 = 1 << 24;
    pub const HAS_COPY_DISPOSE: i32 = 1 << 25;
    pub const LAYOUT_EXTENDED: i32 = 1 << 28;
}

/// What a `__block` variable or captured field is, as the helpers describe it.
mod field {
    pub const OBJECT: i32 = 3;
    pub const BLOCK: i32 = 7;
    pub const BYREF: i32 = 8;
    pub const WEAK: i32 = 16;
    /// The helper is the byref's own, not the block's: the field has already
    /// been dealt with by whoever owns the byref.
    pub const BYREF_CALLER: i32 = 128;
    /// A `__weak __block` variable, which is still the byref's to copy.
    pub const BYREF_WEAK: i32 = BYREF | WEAK;
    pub const MASK: i32 = OBJECT | BLOCK | BYREF | WEAK | BYREF_CALLER;
}

type CopyHelper = unsafe extern "C" fn(*mut c_void, *const c_void);
type DisposeHelper = unsafe extern "C" fn(*mut c_void);

#[repr(C)]
struct Block {
    isa: *const Class,
    flags: AtomicI32,
    reserved: i32,
    invoke: *const c_void,
    descriptor: *const Descriptor,
}

/// `Block_descriptor_1`, and the two helpers that follow it when the flags say
/// they are there.
#[repr(C)]
struct Descriptor {
    reserved: usize,
    size: usize,
}

#[repr(C)]
struct DescriptorHelpers {
    reserved: usize,
    size: usize,
    copy: Option<CopyHelper>,
    dispose: Option<DisposeHelper>,
}

#[repr(C)]
struct Byref {
    isa: *const c_void,
    /// Where the live copy is: the stack original points at the heap copy once
    /// one exists, which is how two closures over one `__block` variable stay
    /// one variable.
    forwarding: *mut Byref,
    flags: AtomicI32,
    size: u32,
}

#[repr(C)]
struct ByrefHelpers {
    keep: Option<CopyHelper>,
    destroy: Option<DisposeHelper>,
}

/// Add a reference, saturating rather than wrapping into the flag bits.
///
/// A saturated count is Apple's answer to an over-retained block: the count
/// stops moving and the block is never freed, which leaks rather than frees
/// something still in use.
fn latching_incr(flags: &AtomicI32) {
    let mut current = flags.load(Ordering::Relaxed);
    loop {
        if current & flag::REFCOUNT_MASK == flag::REFCOUNT_MASK {
            return;
        }
        match flags.compare_exchange_weak(current, current + 2, Ordering::AcqRel, Ordering::Relaxed)
        {
            Ok(_) => return,
            Err(seen) => current = seen,
        }
    }
}

/// Drop a reference, answering whether this one was the last.
///
/// The last decrement clears one *less* than the others, leaving
/// [`flag::DEALLOCATING`] set: a second release racing this one then sees a
/// count of zero and does nothing, so the block is freed once.
fn latching_decr_should_deallocate(flags: &AtomicI32) -> bool {
    let mut current = flags.load(Ordering::Relaxed);
    loop {
        let count = current & flag::REFCOUNT_MASK;
        if count == flag::REFCOUNT_MASK || count == 0 {
            // Saturated, or already at zero - under-releasing is the caller's
            // error and freeing twice would be this runtime's.
            return false;
        }
        let last = count == 2 && current & flag::DEALLOCATING == 0;
        let next = match last {
            true => current - 1,
            false => current - 2,
        };
        match flags.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return last,
            Err(seen) => current = seen,
        }
    }
}

/// The two helpers a descriptor carries, if it carries any.
///
/// # Safety
///
/// `block` must be a block literal.
unsafe fn helpers(block: *const Block) -> Option<&'static DescriptorHelpers> {
    let (flags, descriptor) =
        unsafe { ((*block).flags.load(Ordering::Relaxed), (*block).descriptor) };
    if flags & flag::HAS_COPY_DISPOSE == 0 || descriptor.is_null() {
        return None;
    }
    if flags & flag::SMALL_DESCRIPTOR != 0 {
        // The small form stores its helpers as 32-bit offsets rather than
        // pointers. Refused by name rather than read as pointers, which would
        // be a call to whatever two adjacent words happened to hold.
        crate::fail!("_Block_copy: this block has a small descriptor, which is not decoded here");
    }
    // SAFETY: the flag says the descriptor is the four-field form.
    Some(unsafe { &*(descriptor as *const DescriptorHelpers) })
}

/// A block's invoke function, or `None` for nil or a block without one.
///
/// The caller transmutes it to the block's prototype, with the block itself
/// as the hidden first argument.
///
/// # Safety
///
/// `block` must be null or a live block literal.
pub unsafe fn invoke(block: Id) -> Option<usize> {
    if block.is_null() {
        return None;
    }
    // SAFETY: the caller's obligation; `invoke` is in the fixed header.
    let invoke = unsafe { (*(block as *const Block)).invoke } as usize;
    (invoke != 0).then_some(invoke)
}

/// `void *_Block_copy(const void *block)`.
///
/// A heap block gains a reference, a global block is itself, and a stack block
/// is moved to the heap along with everything it captured.
pub fn copy(block: Id) -> Id {
    if block.is_null() {
        return block;
    }
    crate::debug::validate(block, "_Block_copy");
    let source = block as *const Block;
    // SAFETY: a block literal, whose first three words are the header.
    let flags = unsafe { (*source).flags.load(Ordering::Relaxed) };
    if flags & flag::NEEDS_FREE != 0 {
        latching_incr(unsafe { &(*source).flags });
        return block;
    }
    if flags & flag::IS_GLOBAL != 0 {
        return block;
    }

    // SAFETY: a stack block, whose descriptor is the size of the whole literal.
    let size = unsafe {
        match (*source).descriptor.is_null() {
            true => crate::fail!("_Block_copy: the block has no descriptor, so it has no size"),
            false => (*(*source).descriptor).size,
        }
    };
    if size < size_of::<Block>() {
        crate::fail!("_Block_copy: the descriptor's size is smaller than a block header");
    }
    // SAFETY: `size` bytes, which is what the descriptor says the literal is.
    // From the guest's allocator, not this process's: libclosure calls the one
    // `malloc` its process has, and when the guest ships a replacement that is
    // the guest's. See [`crate::allocator`].
    let Some(allocation_size) = crate::debug::allocation_size(size) else {
        crate::fail!("_Block_copy: the block size overflows its debug guard");
    };
    let copied = unsafe { crate::allocator::guest_malloc(allocation_size) } as *mut Block;
    if copied.is_null() {
        crate::fail!("_Block_copy: out of memory copying a block to the heap");
    }
    // SAFETY: both are `size` bytes and they do not overlap.
    unsafe { std::ptr::copy_nonoverlapping(source as *const u8, copied as *mut u8, size) };
    // SAFETY: freshly allocated, and the header is written before it is read.
    unsafe {
        (*copied).flags.store(
            (flags & !(flag::REFCOUNT_MASK | flag::DEALLOCATING)) | flag::NEEDS_FREE | 2,
            Ordering::Relaxed,
        );
        let class = crate::class::from_ptr(class_ptr(&MALLOC_CLASS))
            .unwrap_or_else(|| crate::fail!("_Block_copy: malloc block class is not installed"));
        crate::debug::allocated(copied as Id, class, size);
        if let Some(helpers) = helpers(source)
            && let Some(helper) = helpers.copy
        {
            helper(copied as *mut c_void, block as *const c_void);
        }
        // The `isa` last, so nothing reading this block sees it as a heap one
        // before its captures have been copied into it.
        (*copied).isa = class_ptr(&MALLOC_CLASS);
    }
    crate::debug::validate(copied as Id, "_Block_copy helper");
    copied as Id
}

#[cfg(test)]
pub(crate) const USE_STRET_FLAG: i32 = flag::USE_STRET;

/// Whether `block` returns a struct through a hidden first argument, which on
/// x86-64 moves every other argument along by one register.
///
/// # Safety
///
/// `block` must be a block literal.
pub(crate) unsafe fn uses_stret(block: Id) -> bool {
    // SAFETY: the header is the first three words of every block.
    unsafe { (*(block as *const Block)).flags.load(Ordering::Relaxed) & flag::USE_STRET != 0 }
}

/// `void _Block_release(const void *block)`.
pub fn release(block: Id) {
    if block.is_null() {
        return;
    }
    crate::debug::validate(block, "_Block_release");
    let block = block as *mut Block;
    // SAFETY: a block literal.
    let flags = unsafe { (*block).flags.load(Ordering::Relaxed) };
    if flags & flag::IS_GLOBAL != 0 || flags & flag::NEEDS_FREE == 0 {
        // A global block is immortal and a stack block is the frame's, so
        // neither is this function's to free.
        return;
    }
    if !latching_decr_should_deallocate(unsafe { &(*block).flags }) {
        return;
    }
    crate::debug::begin_deallocation(block as Id);
    // SAFETY: nothing holds a reference; the dispose helper is the compiler's
    // and undoes exactly what the copy helper did.
    unsafe {
        if let Some(helpers) = helpers(block)
            && let Some(helper) = helpers.dispose
        {
            helper(block as *mut c_void);
        }
        if !crate::debug::finalized(block as Id) {
            crate::allocator::guest_free(block as *mut c_void);
        }
    }
}

/// `void _Block_object_assign(void *destination, const void *object, int flags)`
///
/// The compiler's copy helper calls this once per captured field, and `flags`
/// says what the field is.
///
/// # Safety
///
/// `destination` must be a writable pointer-sized slot, and `object` must be
/// whatever `flags` says it is.
pub unsafe fn object_assign(destination: *mut c_void, object: *const c_void, flags: i32) {
    let destination = destination as *mut *const c_void;
    match flags & field::MASK {
        field::OBJECT => {
            crate::arc::retain(object as Id);
            unsafe { *destination = object };
        }
        field::BLOCK => {
            unsafe { *destination = copy(object as Id) as *const c_void };
        }
        field::BYREF | field::BYREF_WEAK => {
            // SAFETY: as above; a byref is copied to the heap on first capture.
            unsafe { *destination = byref_copy(object as *mut Byref) as *const c_void };
        }
        // The field belongs to a byref the caller already owns, weak or not:
        // the pointer is assigned and nothing is retained.
        _ => unsafe { *destination = object },
    }
}

/// `void _Block_object_dispose(const void *object, int flags)`
///
/// # Safety
///
/// As [`object_assign`].
pub unsafe fn object_dispose(object: *const c_void, flags: i32) {
    match flags & field::MASK {
        field::OBJECT => crate::arc::release(object as Id),
        field::BLOCK => release(object as Id),
        field::BYREF | field::BYREF_WEAK => unsafe { byref_release(object as *mut Byref) },
        _ => {}
    }
}

/// Move a `__block` variable to the heap, or add a reference to the copy that is
/// already there.
///
/// The stack original's `forwarding` is repointed at the heap copy, which is
/// what makes a write through either one a write to the same variable.
///
/// # Safety
///
/// `source` must be a `Block_byref`.
unsafe fn byref_copy(source: *mut Byref) -> *mut Byref {
    crate::debug::validate(source as Id, "_Block_object_assign byref");
    let forwarding = unsafe { (*source).forwarding };
    if forwarding.is_null() {
        return source;
    }
    // SAFETY: a byref, whose forwarding points at itself or at its heap copy.
    let live = unsafe { (*forwarding).flags.load(Ordering::Relaxed) };
    if live & flag::REFCOUNT_MASK != 0 {
        if live & byref_flag::NEEDS_FREE != 0 {
            latching_incr(unsafe { &(*forwarding).flags });
        }
        return forwarding;
    }

    // SAFETY: still on the stack, so this is the first capture.
    let (flags, size) = unsafe {
        (
            (*source).flags.load(Ordering::Relaxed),
            (*source).size as usize,
        )
    };
    if size < size_of::<Byref>() {
        crate::fail!("_Block_object_assign: a __block variable smaller than its own header");
    }
    // SAFETY: `size` is what the compiler recorded for the whole variable, and
    // the allocator is the guest's for [`copy`]'s reason.
    let Some(allocation_size) = crate::debug::allocation_size(size) else {
        crate::fail!("_Block_object_assign: the __block size overflows its debug guard");
    };
    let copied = unsafe { crate::allocator::guest_malloc(allocation_size) } as *mut Byref;
    if copied.is_null() {
        crate::fail!("_Block_object_assign: out of memory copying a __block variable");
    }
    // SAFETY: freshly allocated, and every field is written before it is read.
    unsafe {
        (*copied).isa = std::ptr::null();
        // Four, not two: the heap copy is referenced by the block that copied it
        // *and* by the stack original, which forwards to it until the frame
        // ends.
        (*copied).flags.store(
            (flags & !(flag::REFCOUNT_MASK | flag::DEALLOCATING)) | byref_flag::NEEDS_FREE | 4,
            Ordering::Relaxed,
        );
        (*copied).forwarding = copied;
        (*copied).size = size as u32;
        (*source).forwarding = copied;
        crate::debug::allocated_raw(copied as *mut c_void, size, "__block storage");

        match flags & byref_flag::HAS_COPY_DISPOSE != 0 {
            // The helpers are the compiler's, and copying the captured value is
            // theirs to do; only the two helper pointers and the layout string
            // are moved by hand.
            true => {
                let source_helpers = source.add(1) as *const ByrefHelpers;
                let copied_helpers = copied.add(1) as *mut ByrefHelpers;
                (*copied_helpers).keep = (*source_helpers).keep;
                (*copied_helpers).destroy = (*source_helpers).destroy;
                if flags & byref_flag::LAYOUT_EXTENDED != 0 {
                    let layout = size_of::<Byref>() + size_of::<ByrefHelpers>();
                    std::ptr::copy_nonoverlapping(
                        (source as *const u8).add(layout),
                        (copied as *mut u8).add(layout),
                        size_of::<usize>(),
                    );
                }
                if let Some(keep) = (*copied_helpers).keep {
                    keep(copied as *mut c_void, source as *const c_void);
                }
            }
            // Nothing in it needs a helper, so the bytes are the value.
            false => std::ptr::copy_nonoverlapping(
                (source as *const u8).add(size_of::<Byref>()),
                (copied as *mut u8).add(size_of::<Byref>()),
                size - size_of::<Byref>(),
            ),
        }
    }
    crate::debug::validate(copied as Id, "__block copy helper");
    copied
}

/// # Safety
///
/// As [`byref_copy`].
unsafe fn byref_release(byref: *mut Byref) {
    crate::debug::validate(byref as Id, "_Block_object_dispose byref");
    let byref = unsafe { (*byref).forwarding };
    if byref.is_null() {
        return;
    }
    crate::debug::validate(byref as Id, "_Block_object_dispose byref");
    // SAFETY: a byref.
    let flags = unsafe { (*byref).flags.load(Ordering::Relaxed) };
    if flags & byref_flag::NEEDS_FREE == 0 {
        return;
    }
    if !latching_decr_should_deallocate(unsafe { &(*byref).flags }) {
        return;
    }
    crate::debug::begin_deallocation(byref as Id);
    // SAFETY: nothing holds a reference.
    unsafe {
        if flags & byref_flag::HAS_COPY_DISPOSE != 0
            && let Some(destroy) = (*(byref.add(1) as *const ByrefHelpers)).destroy
        {
            destroy(byref as *mut c_void);
        }
        if !crate::debug::finalized(byref as Id) {
            crate::allocator::guest_free(byref as *mut c_void);
        }
    }
}

/// The three classes a block's `isa` names, and the `NSBlock` they descend from.
///
/// They are real classes rather than the opaque `void *[32]` libclosure declares
/// so that a block answers a message: `[block copy]` and an ARC `objc_retain`
/// both have to reach the reference count in the block's own header, and both
/// arrive through the `isa`.
static STACK_CLASS: AtomicUsize = AtomicUsize::new(0);
static GLOBAL_CLASS: AtomicUsize = AtomicUsize::new(0);
static MALLOC_CLASS: AtomicUsize = AtomicUsize::new(0);

fn class_ptr(cell: &AtomicUsize) -> *const Class {
    cell.load(Ordering::Relaxed) as *const Class
}

/// Whether `object`'s `isa` is one a block carries.
///
/// Three loads and three comparisons on the retain path, which is why the class
/// pointers are cached here rather than looked up by name.
///
/// # Safety
///
/// `object` must be null or an object this runtime can read the first word of.
pub unsafe fn is_block(object: Id) -> bool {
    if object.is_null() {
        return false;
    }
    // SAFETY: the caller's obligation; the first word is the `isa`.
    let isa = unsafe { *(object as *const usize) };
    if isa == 0 {
        return false;
    }
    [&STACK_CLASS, &GLOBAL_CLASS, &MALLOC_CLASS]
        .iter()
        .any(|cell| cell.load(Ordering::Relaxed) == isa)
}

unsafe extern "C" fn block_copy_message(receiver: Id, _sel: Sel) -> Id {
    copy(receiver)
}

unsafe extern "C" fn block_copy_with_zone(receiver: Id, _sel: Sel, _zone: *const c_void) -> Id {
    copy(receiver)
}

unsafe extern "C" fn block_release_message(receiver: Id, _sel: Sel) {
    release(receiver);
}

unsafe extern "C" fn block_autorelease(receiver: Id, _sel: Sel) -> Id {
    crate::arc::autorelease(receiver)
}

/// Install `NSBlock` and the three concrete classes. Called from
/// [`crate::bootstrap`].
pub(crate) fn install() {
    let Some(block) = crate::class::allocate_pair(
        crate::class::lookup(c"NSObject"),
        c"NSBlock",
        size_of::<Block>() - size_of::<usize>(),
    ) else {
        return;
    };
    crate::class::register_pair(block);
    for (sel, imp, types) in [
        ("copy", block_copy_message as Imp, c"@16@0:8"),
        ("copyWithZone:", block_copy_with_zone as Imp, c"@24@0:8^v16"),
        ("retain", block_copy_message as Imp, c"@16@0:8"),
        ("release", block_release_message as Imp, c"v16@0:8"),
        ("autorelease", block_autorelease as Imp, c"@16@0:8"),
    ] {
        block.replace_method(crate::sel::get(sel), imp, types);
    }
    for (name, cell) in [
        (c"__NSStackBlock__", &STACK_CLASS),
        (c"__NSGlobalBlock__", &GLOBAL_CLASS),
        (c"__NSMallocBlock__", &MALLOC_CLASS),
    ] {
        let Some(class) = crate::class::allocate_pair(Some(block), name, 0) else {
            continue;
        };
        crate::class::register_pair(class);
        cell.store(crate::class::ptr(class) as usize, Ordering::Relaxed);
    }
}

/// The C spellings, which are the interface; renaming them would only hide it.
#[allow(non_snake_case)]
mod entry {
    use std::ffi::c_void;

    use crate::object::Id;

    pub(super) unsafe extern "C" fn _Block_copy(block: Id) -> Id {
        super::copy(block)
    }

    pub(super) unsafe extern "C" fn _Block_release(block: Id) {
        super::release(block);
    }

    pub(super) unsafe extern "C" fn _Block_object_assign(
        destination: *mut c_void,
        object: *const c_void,
        flags: i32,
    ) {
        // SAFETY: the compiler's copy helper passes a slot in the block it is
        // filling in, and a field of the kind `flags` names.
        unsafe { super::object_assign(destination, object, flags) };
    }

    pub(super) unsafe extern "C" fn _Block_object_dispose(object: *const c_void, flags: i32) {
        // SAFETY: as above, from the dispose helper.
        unsafe { super::object_dispose(object, flags) };
    }
}

mod entries {
    use super::entry;

    exports::exports! {
        functions {
            "_Block_copy" => entry::_Block_copy,
            "_Block_release" => entry::_Block_release,
            "_Block_object_assign" => entry::_Block_object_assign,
            "_Block_object_dispose" => entry::_Block_object_dispose,
        }
    }
}

pub use entries::{MODULE, names, provide};

/// The C entry points, by name, in either spelling.
///
/// These are the one family whose C identifiers *begin* with an underscore, so a
/// caller that has stripped the one a Mach-O symbol carries asks for
/// `_Block_copy` and a caller that has stripped it twice - which
/// `darwin::c_name` does on the way to a `dlsym` - asks for `Block_copy`.
pub fn symbol(name: &str) -> Option<u64> {
    crate::bootstrap();
    entries::symbol(name).or_else(|| entries::symbol(&alloc_prefixed(name)))
}

/// `name` with the underscore its C identifier carries put back.
fn alloc_prefixed(name: &str) -> String {
    format!("_{name}")
}

/// The three `isa` values, which a guest's block literal names as *data*.
///
/// A global block in a guest's `__DATA` has `_NSConcreteGlobalBlock` in its
/// first word already, bound by the loader, and a stack block gets
/// `_NSConcreteStackBlock` written into it by the prologue - so these have to be
/// the same objects this module compares against.
pub fn data_symbols() -> Vec<(String, u64)> {
    crate::bootstrap();
    [
        ("_NSConcreteStackBlock", &STACK_CLASS),
        ("_NSConcreteGlobalBlock", &GLOBAL_CLASS),
        ("_NSConcreteMallocBlock", &MALLOC_CLASS),
    ]
    .iter()
    .map(|(name, cell)| (format!("_{name}"), cell.load(Ordering::Relaxed) as u64))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A block literal with one captured object, laid out as the compiler lays
    /// one out.
    #[repr(C)]
    struct Literal {
        isa: *const Class,
        flags: i32,
        reserved: i32,
        invoke: *const c_void,
        descriptor: *const DescriptorHelpers,
        captured: Id,
    }

    unsafe extern "C" fn invoke(_block: Id) {}

    // SAFETY: a descriptor is immutable and its two helpers are code addresses
    // that outlive the process.
    unsafe impl Sync for DescriptorHelpers {}

    /// A descriptor and a pair of counters, per test.
    ///
    /// Per test rather than shared: the suite runs the tests on several threads
    /// at once, and one counter between them makes every assertion here depend
    /// on which other test happened to be running.
    macro_rules! descriptor {
        () => {{
            static COPIES: AtomicUsize = AtomicUsize::new(0);
            static DISPOSALS: AtomicUsize = AtomicUsize::new(0);

            unsafe extern "C" fn copy_helper(destination: *mut c_void, source: *const c_void) {
                COPIES.fetch_add(1, Ordering::SeqCst);
                // SAFETY: both are `Literal`s, which is what this descriptor
                // describes.
                unsafe {
                    let captured = (*(source as *const Literal)).captured;
                    object_assign(
                        &raw mut (*(destination as *mut Literal)).captured as *mut c_void,
                        captured as *const c_void,
                        field::OBJECT,
                    );
                }
            }

            unsafe extern "C" fn dispose_helper(block: *mut c_void) {
                DISPOSALS.fetch_add(1, Ordering::SeqCst);
                // SAFETY: a `Literal`.
                unsafe {
                    object_dispose(
                        (*(block as *const Literal)).captured as *const c_void,
                        field::OBJECT,
                    );
                }
            }

            static HELPERS: DescriptorHelpers = DescriptorHelpers {
                reserved: 0,
                size: size_of::<Literal>(),
                copy: Some(copy_helper),
                dispose: Some(dispose_helper),
            };

            (
                &HELPERS,
                || COPIES.load(Ordering::SeqCst),
                || DISPOSALS.load(Ordering::SeqCst),
            )
        }};
    }

    fn object() -> Id {
        crate::bootstrap();
        let class = crate::class::lookup(c"NSObject").expect("the root class");
        crate::object::create_instance(class, 0)
    }

    fn stack_block(captured: Id, descriptor: &'static DescriptorHelpers) -> Literal {
        crate::bootstrap();
        Literal {
            isa: class_ptr(&STACK_CLASS),
            flags: flag::HAS_COPY_DISPOSE,
            reserved: 0,
            invoke: invoke as *const c_void,
            descriptor,
            captured,
        }
    }

    /// The whole of the stack-to-heap copy: the literal moves, the copy helper
    /// runs over its captures, and the copy says it is a heap block.
    #[test]
    fn a_stack_block_is_copied_to_the_heap_with_its_captures() {
        let (descriptor, copies, disposals) = descriptor!();
        let captured = object();
        let block = stack_block(captured, descriptor);

        let copied = copy(&block as *const Literal as Id);
        assert_ne!(copied as usize, &block as *const Literal as usize);
        assert_eq!(copies(), 1, "the copy helper ran");
        assert_eq!(
            crate::arc::retain_count(captured),
            2,
            "the captured object was not retained by the copy"
        );
        // SAFETY: a block this module just made.
        let heap = unsafe { &*(copied as *const Literal) };
        assert_eq!(heap.captured, captured);
        assert_eq!(heap.flags & flag::NEEDS_FREE, flag::NEEDS_FREE);
        assert_eq!(heap.flags & flag::REFCOUNT_MASK, 2, "one reference");
        let class = unsafe { crate::object::class_of(copied) }.expect("a class");
        assert_eq!(class.name_str(), "__NSMallocBlock__");

        release(copied);
        assert_eq!(disposals(), 1, "the dispose helper ran");
        assert_eq!(crate::arc::retain_count(captured), 1);
        crate::arc::release(captured);
    }

    /// A copy of a *heap* block is the same block, and it takes as many releases
    /// as there were copies to dispose of it.
    #[test]
    fn a_heap_block_is_reference_counted_in_place() {
        let (descriptor, _, disposals) = descriptor!();
        let captured = object();
        let block = stack_block(captured, descriptor);

        let first = copy(&block as *const Literal as Id);
        let second = copy(first);
        assert_eq!(first, second, "copying a heap block moved it");

        release(second);
        assert_eq!(
            disposals(),
            0,
            "the block was disposed of while a reference was still held"
        );
        release(first);
        assert_eq!(disposals(), 1);
        crate::arc::release(captured);
    }

    /// A global block is itself, copied or not, and releasing one does nothing -
    /// it is in `__DATA` and there is nothing to free.
    #[test]
    fn a_global_block_is_immortal() {
        let (descriptor, _, _) = descriptor!();
        let mut block = stack_block(std::ptr::null_mut(), descriptor);
        block.isa = class_ptr(&GLOBAL_CLASS);
        block.flags = flag::IS_GLOBAL;
        let address = &block as *const Literal as Id;
        assert_eq!(copy(address), address);
        release(address);
        release(address);
        assert_eq!(
            block.flags,
            flag::IS_GLOBAL,
            "the flags word was written to"
        );
    }

    /// `objc_retain` on a block has to reach the count in the block, not the
    /// side table - which is what the `isa` comparison is for.
    #[test]
    fn arc_routes_a_block_to_this_module() {
        let (descriptor, _, disposals) = descriptor!();
        let block = stack_block(std::ptr::null_mut(), descriptor);
        let heap = crate::arc::retain(&block as *const Literal as Id);
        assert_ne!(heap as usize, &block as *const Literal as usize);
        assert_eq!(
            crate::arc::retain_count(heap),
            1,
            "the block was counted in the side table"
        );
        crate::arc::release(heap);
        assert_eq!(disposals(), 1);
    }

    /// A `__block` variable, with the two helpers the compiler gives one.
    #[repr(C)]
    struct TestByref {
        isa: *const c_void,
        forwarding: *mut TestByref,
        flags: i32,
        size: u32,
        keep: Option<CopyHelper>,
        destroy: Option<DisposeHelper>,
        value: i64,
    }

    static DESTROYED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn byref_keep(destination: *mut c_void, source: *const c_void) {
        // SAFETY: both are `TestByref`s.
        unsafe { (*(destination as *mut TestByref)).value = (*(source as *const TestByref)).value };
    }

    unsafe extern "C" fn byref_destroy(_byref: *mut c_void) {
        DESTROYED.fetch_add(1, Ordering::SeqCst);
    }

    /// A `__block` variable captured by a block moves to the heap once, and both
    /// the stack original and every capture then name the heap copy - which is
    /// what makes two closures over one variable see one variable.
    #[test]
    fn a_block_variable_moves_to_the_heap_once() {
        crate::bootstrap();
        let mut byref = TestByref {
            isa: std::ptr::null(),
            forwarding: std::ptr::null_mut(),
            flags: byref_flag::HAS_COPY_DISPOSE,
            size: size_of::<TestByref>() as u32,
            keep: Some(byref_keep),
            destroy: Some(byref_destroy),
            value: 42,
        };
        byref.forwarding = &raw mut byref;
        DESTROYED.store(0, Ordering::SeqCst);

        let mut first: *mut TestByref = std::ptr::null_mut();
        let mut second: *mut TestByref = std::ptr::null_mut();
        // SAFETY: two pointer-sized slots, and a byref of the shape the flags
        // describe.
        unsafe {
            let source = &raw mut byref as *const c_void;
            object_assign(&raw mut first as *mut c_void, source, field::BYREF);
            object_assign(&raw mut second as *mut c_void, source, field::BYREF);
        }
        assert!(!first.is_null());
        assert_eq!(first, second, "the variable was copied to the heap twice");
        assert_eq!(
            byref.forwarding, first,
            "the stack copy still forwards home"
        );
        // SAFETY: the heap copy, which this module made.
        unsafe {
            assert_eq!((*first).value, 42, "the keep helper did not run");
            assert_eq!((*first).forwarding, first, "the copy forwards to itself");
        }

        // Three references: the two captures and the stack original's.
        // SAFETY: a byref this module made.
        unsafe {
            object_dispose(first as *const c_void, field::BYREF);
            object_dispose(second as *const c_void, field::BYREF);
        }
        assert_eq!(
            DESTROYED.load(Ordering::SeqCst),
            0,
            "the variable was destroyed while the frame still held it"
        );
        // SAFETY: the last reference, the one the stack frame holds.
        unsafe { object_dispose(&raw mut byref as *const c_void, field::BYREF) };
        assert_eq!(DESTROYED.load(Ordering::SeqCst), 1);
    }
}
