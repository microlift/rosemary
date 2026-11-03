use machish::resolve::SyntheticDylib;

use microlift::arch::x86_64::context::RegisterContext;
use static_init::dynamic;

use crate::abi::aggregate::{self, Aggregate, GuestClass, Leaf, Shape};
use crate::abi::marshal::{self, Convert};
use crate::abi::{AbiError, ArgType as T, GuestMemory, Result};
use crate::vm::Vm;

use super::encoding::{self, Signature};
use super::host;
use crate::shims::bind;

/// A runtime entry point the guest may call, and what crosses at it.
pub(super) struct Runtime {
    /// The C name, as `dlsym` spells it: no leading underscore.
    pub(super) name: &'static str,
    /// `[0]` is the return type, `[1..]` the parameters, as everywhere else.
    pub(super) types: &'static [T],
    /// Zero-based parameter indices holding a `Class`.
    pub(super) classes: &'static [u8],
    /// Whether the return value is a `Class`.
    pub(super) class_return: bool,
    /// Zero-based parameter indices that are **guest function pointers**, in exactly
    /// the sense [`crate::abi::FunctionAbi::callbacks`] means.
    pub(super) callbacks: &'static [u8],
    /// Zero-based parameter indices holding a **block literal**.
    pub(super) blocks: &'static [u8],
    /// Zero-based parameter indices holding a `Protocol *`.
    pub(super) protocols: &'static [u8],
    /// The scalar members of a struct returned **by value**, in declaration order, or
    /// `None` for the ordinary scalar return in `types[0]`.
    pub(super) struct_return: Option<&'static [T]>,
}

/// `(ret, args...)`, with no class anywhere.
macro_rules! rt {
    ($name:literal, $ret:expr $(, $arg:expr)* $(,)?) => {
        Runtime {
            name: $name,
            types: &[$ret $(, $arg)*],
            classes: &[],
            class_return: false,
            callbacks: &[],
            blocks: &[],
            protocols: &[],
            struct_return: None,
        }
    };
}

/// As [`rt`], naming the parameters that are classes and whether the return is.
macro_rules! rtc {
    ([$($c:literal),* $(,)?] $(-> $ret_is_class:literal)?, $name:literal, $ret:expr $(, $arg:expr)* $(,)?) => {
        Runtime {
            name: $name,
            types: &[$ret $(, $arg)*],
            classes: &[$($c),*],
            class_return: false $(|| $ret_is_class)?,
            callbacks: &[],
            blocks: &[],
            protocols: &[],
            struct_return: None,
        }
    };
}

/// As [`rt`], naming the parameters that are guest function pointers.
macro_rules! rtb {
    ([$($b:literal),* $(,)?], $name:literal, $ret:expr $(, $arg:expr)* $(,)?) => {
        Runtime {
            name: $name,
            types: &[$ret $(, $arg)*],
            classes: &[],
            class_return: false,
            callbacks: &[$($b),*],
            blocks: &[],
            protocols: &[],
            struct_return: None,
        }
    };
}

/// As [`rtc`], also naming the parameters that are guest function pointers.
macro_rules! rtcb {
    ([$($c:literal),* $(,)?], [$($b:literal),* $(,)?], $name:literal, $ret:expr $(, $arg:expr)* $(,)?) => {
        Runtime {
            name: $name,
            types: &[$ret $(, $arg)*],
            classes: &[$($c),*],
            class_return: false,
            callbacks: &[$($b),*],
            blocks: &[],
            protocols: &[],
            struct_return: None,
        }
    };
}

const V: T = T::None;
const I32: T = T::SignedWord;
const I64: T = T::SignedDoubleword;
const U64: T = T::UnsignedDoubleword;
const U8: T = T::UnsignedByte;
const P: T = T::Pointer;

/// The runtime surface a guest reaches, **sorted by name**.
pub(super) static RUNTIME: &[Runtime] = &[
    //
    // The other direction of [`crate::objc::register`]. That pass replays the
    // classes a guest *declared* in its `__objc_data`; these three are a guest
    // building one while it runs, and they need no replay at all - the class is
    // the host runtime's from the moment it is allocated, so what the guest
    // holds is already a real `Class` and the translation table has nothing to
    // say about it.
    //
    // What does need saying is the `IMP`. It is guest code, and once the class
    // is registered the host runtime will call it as arm64 from anywhere -
    // Foundation, a delegate callback, a thread this runtime never made - so it
    // leaves as a thunk exactly as `method_setImplementation`'s does.
    //
    // Terraria reaches all three: Mono's Objective-C bridge builds an
    // `NSApplication` delegate class at start-up rather than declaring one.
    rtcb!([0], [2], "class_addMethod", U8, P, P, P, P),
    //
    // Not methods and not ivars: a name and an attribute string beside them,
    // read only by code introspecting a class it did not write - which is
    // exactly what `libYoYoGamepad` does. The list a `class_copyPropertyList`
    // returns is `malloc`ed and the caller frees it, so nothing is translated on
    // the way back: a property handle is opaque, and the guest only ever hands
    // it to `property_getName` and its neighbours.
    rtc!([0], "class_addProperty", U8, P, P, P, I32),
    // Spelled out rather than built by a macro, as every entry taking a
    // protocol is.
    Runtime {
        name: "class_conformsToProtocol",
        types: &[U8, P, P],
        classes: &[0],
        class_return: false,
        callbacks: &[],
        blocks: &[],
        protocols: &[1],
        struct_return: None,
    },
    rtc!([0], "class_copyPropertyList", P, P, P),
    rtc!([0] -> true, "class_createInstance", P, P, U64),
    rtc!([0], "class_getClassMethod", P, P, P),
    rtc!([0], "class_getInstanceMethod", P, P, P),
    rtc!([0], "class_getInstanceSize", U64, P),
    rtc!([0], "class_getInstanceVariable", P, P, P),
    // The compiler-emitted ARC layout bitmap for a class's strong ivars. This
    // runtime does not model one; see `BUILTIN_GAPS`.
    rtc!([0], "class_getIvarLayout", P, P),
    rtc!([0], "class_getName", P, P),
    rtc!([0], "class_getProperty", P, P, P),
    rtc!([0] -> true, "class_getSuperclass", P, P),
    rtc!([0], "class_isMetaClass", U8, P),
    // `class_addMethod` that also overwrites; the IMP leaves as a thunk the same
    // way. Unity swizzles NSApplication with it at start-up.
    rtcb!([0], [2], "class_replaceMethod", P, P, P, P, P),
    rtc!([0], "class_replaceProperty", V, P, P, P, I32),
    rtc!([0], "class_respondsToSelector", U8, P, P),
    //
    // A guest that reads an ivar through `object_getIvar` rather than through a
    // compiled displacement is asking the *host* runtime where the ivar is, and
    // gets the right answer for free - the class is registered, so the layout
    // the host reports is the layout the object has. `ivar_getOffset` is what
    // makes that answer usable from guest code, and it returns a `ptrdiff_t`.
    rt!("ivar_getName", P, P),
    rt!("ivar_getOffset", I64, P),
    // Both implementations are already the runtime's, so nothing crosses.
    rt!("method_exchangeImplementations", V, P, P),
    rt!("method_getImplementation", P, P),
    rt!("method_getTypeEncoding", P, P),
    rtb!([1], "method_setImplementation", P, P, P),
    // ARC and the `objc_msgSend` optimisations clang emits at `-O` whether or
    // not the file uses ARC.
    rtc!([0] -> true, "objc_alloc", P, P),
    rtc!([0] -> true, "objc_allocWithZone", P, P),
    rtc!([0], "objc_alloc_init", P, P),
    rtc!([0] -> true, "objc_allocateClassPair", P, P, P, U64),
    rtc!([0], "objc_autorelease", P, P),
    rt!("objc_autoreleasePoolPop", V, P),
    rt!("objc_autoreleasePoolPush", P),
    rtc!([0], "objc_autoreleaseReturnValue", P, P),
    //
    // These four are the whole of ARC's weak-reference support that SDL3's Cocoa
    // backend imports, and passthrough is right for a reason worth stating: a
    // weak reference is an entry in the runtime's *own* side table, keyed by the
    // address of the storage. That storage is in guest memory, which is a real
    // address in this process because the guest is identity-mapped, and the
    // object is a host allocation on both sides. So the host's table is already
    // keyed and valued by exactly the things the guest holds - there is nothing
    // to translate and nothing that could be kept coherent by a second table.
    //
    // `objc_storeWeak`'s *value* is the exception, and it is the ordinary one:
    // a guest may legally weakly reference a class, and a class is the one
    // pointer that means something different on each side.
    rt!("objc_copyClassNamesForImage", P, P, P),
    // `void objc_copyStruct(void *dest, const void *src, ptrdiff_t size,
    // BOOL atomic, BOOL hasStrong)` - a synthesized accessor of a struct-typed
    // `atomic` property.
    rt!("objc_copyStruct", V, P, P, I64, U8, U8),
    rt!("objc_copyWeak", V, P, P),
    rt!("objc_destroyWeak", V, P),
    // The hook the compiler emits inside every `for (x in collection)` loop, to
    // be called when the collection changed underneath it.
    rt!("objc_enumerationMutation", V, P),
    // `object_getClass` of a class is its metaclass, so the argument is
    // translated as well as the result.
    rtc!([0] -> true, "objc_getClass", P, P),
    rtc!([] -> true, "objc_getMetaClass", P, P),
    //
    // A `@synthesize`d property does not compile to a load and a store. For
    // anything `atomic` or `copy`, clang emits a call to one of these, which the
    // runtime implements over a spinlock table keyed by the *address of the
    // ivar* - guest memory, so the same address the host would compute - and, in
    // the `copy` cases, a `-copyWithZone:` message to the new value.
    //
    // That message is why these belong in this table rather than in the C one:
    // the value may be a guest object whose class is a guest class, and sending
    // to it has to go through the host runtime with the class the runtime knows.
    // It already will - the object is a host allocation - so what these
    // need is the ordinary class substitution on any argument that could be a
    // class, and nothing more.
    //
    // `offset` is a `ptrdiff_t` and `atomic`/`shouldCopy` are single bytes.
    // **Argument order differs between `objc_setProperty` and its specialized
    // spellings** - the general one takes `offset` third and the value fourth,
    // and the four specializations take the value third and `offset` fourth.
    // Getting that backwards stores an object pointer as an ivar offset.
    rtc!([0], "objc_getProperty", P, P, P, I64, U8),
    rt!("objc_getProtocol", P, P),
    rtc!([] -> true, "objc_getRequiredClass", P, P),
    // Whatever `objc_storeWeak` put in the table comes back out, so the return
    // is translated for the same reason that call's argument is.
    rtc!([1], "objc_initWeak", P, P, P),
    // `Class objc_loadClassref(Class *)` resolves a class reference slot, which
    // on Apple's runtime may hold a *stub* class marked by its low bit.
    rtc!([] -> true, "objc_loadClassref", P, P),
    rtc!([] -> true, "objc_loadWeak", P, P),
    rtc!([] -> true, "objc_loadWeakRetained", P, P),
    rtc!([] -> true, "objc_lookUpClass", P, P),
    // `void objc_moveWeak(id *dst, id *src)` - the third of the four weak
    // primitives, alongside `objc_copyWeak` above.
    rt!("objc_moveWeak", V, P, P),
    rtc!([0] -> true, "objc_opt_class", P, P),
    rtc!([0, 1], "objc_opt_isKindOfClass", U8, P, P),
    rtc!([0], "objc_opt_new", P, P),
    rtc!([0], "objc_opt_respondsToSelector", U8, P, P),
    rtc!([0], "objc_opt_self", P, P),
    // `Class objc_readClassPair(Class, const struct objc_image_info *)` and
    // `const char **objc_copyClassNamesForImage(const char *, unsigned *)` are
    // both about *images*, which this runtime's classes do not belong to. See
    // `BUILTIN_GAPS`.
    rtc!([0] -> true, "objc_readClassPair", P, P, P),
    rtc!([0], "objc_registerClassPair", V, P),
    rtc!([0], "objc_release", V, P),
    rtc!([0], "objc_retain", P, P),
    rtc!([0], "objc_retainAutorelease", P, P),
    rtc!([0], "objc_retainAutoreleaseReturnValue", P, P),
    rtc!([0], "objc_retainAutoreleasedReturnValue", P, P),
    // `id objc_retainBlock(id)` is `_Block_copy` under another name. Most of it
    // needs nothing done, for the reason the roadmap records.
    Runtime {
        name: "objc_retainBlock",
        types: &[P, P],
        classes: &[],
        class_return: false,
        callbacks: &[],
        blocks: &[0],
        protocols: &[],
        struct_return: None,
    },
    // See the note on `objc_getProperty` for why the argument order is not the
    // same in the general entry point as in the four specialized ones.
    rtc!([0, 3], "objc_setProperty", V, P, P, I64, P, U8, U8),
    rtc!([0, 2], "objc_setProperty_atomic", V, P, P, P, I64),
    rtc!([0, 2], "objc_setProperty_atomic_copy", V, P, P, P, I64),
    rtc!([0, 2], "objc_setProperty_nonatomic", V, P, P, P, I64),
    rtc!([0, 2], "objc_setProperty_nonatomic_copy", V, P, P, P, I64),
    // `objc_storeStrong(id *location, id value)` writes through a guest
    // pointer.
    rtc!([1], "objc_storeStrong", V, P, P),
    // `id objc_storeWeak(id *location, id obj)` - see the weak note above.
    rtc!([1], "objc_storeWeak", P, P, P),
    // `@synchronized`. Passthrough is not a shortcut here.
    rtc!([0], "objc_sync_enter", I32, P),
    rtc!([0], "objc_sync_exit", I32, P),
    rt!("objc_terminate", V),
    // The `__unsafe_unretained` end of the autoreleased-return-value handshake.
    rtc!([0], "objc_unsafeClaimAutoreleasedReturnValue", P, P),
    rtc!([0] -> true, "object_getClass", P, P),
    rtc!([0], "object_getClassName", P, P),
    rt!("object_getIndexedIvars", P, P),
    rtc!([0], "object_getInstanceVariable", P, P, P, P),
    rtc!([0] -> true, "object_getIvar", P, P, P),
    rtc!([0, 1] -> true, "object_setClass", P, P, P),
    rtc!([0, 2], "object_setIvar", V, P, P, P),
    rt!("property_copyAttributeList", P, P, P),
    rt!("property_getAttributes", P, P),
    rt!("property_getName", P, P),
    //
    // `struct objc_method_description protocol_getMethodDescription(Protocol *,
    // SEL, BOOL isRequiredMethod, BOOL isInstanceMethod)` - two pointers, `{SEL
    // name; char *types;}`, sixteen bytes of integer class. Both conventions
    // return that in their first two integer registers, so nothing is copied
    // anywhere: the host's `x0`/`x1` become the guest's `rax`/`rdx`, which is a
    // path [`marshal::call_shapes`] already walks for a method whose encoding
    // names a struct.
    //
    // Unity's Metal backend asks this of `MTLDevice` while it builds the device
    // caps, which is the first thing after `Initializing Metal device caps` in
    // the player log and where Subnautica Below Zero stopped.
    //
    // The `types` pointer that comes back belongs to the host's `libobjc`, and
    // the guest reads it as a C string - which works for the reason every other
    // host pointer does: the guest is identity-mapped into this process, so a
    // host address is an address it can load from.
    Runtime {
        name: "protocol_getMethodDescription",
        types: &[V, P, P, U8, U8],
        classes: &[],
        class_return: false,
        callbacks: &[],
        blocks: &[],
        protocols: &[0],
        struct_return: Some(&[P, P]),
    },
    Runtime {
        name: "protocol_getName",
        types: &[P, P],
        classes: &[],
        class_return: false,
        callbacks: &[],
        blocks: &[],
        protocols: &[0],
        struct_return: None,
    },
    rt!("sel_getName", P, P),
    rt!("sel_getUid", P, P),
    rt!("sel_isEqual", U8, P, P),
    rt!("sel_registerName", P, P),
];

/// The `objc_msgSend` family, and what each spelling means here.
static MESSAGES: &[(&str, Send)] = &[
    (
        "objc_msgSend",
        Send {
            message: Message::Direct,
            stret: false,
            fixup: false,
        },
    ),
    (
        "objc_msgSendSuper",
        Send {
            message: Message::Super { skip_one: false },
            stret: false,
            fixup: false,
        },
    ),
    (
        "objc_msgSendSuper2",
        Send {
            message: Message::Super { skip_one: true },
            stret: false,
            fixup: false,
        },
    ),
    // The struct-returning spellings. Nothing about the *search* differs.
    (
        "objc_msgSend_stret",
        Send {
            message: Message::Direct,
            stret: true,
            fixup: false,
        },
    ),
    (
        "objc_msgSendSuper_stret",
        Send {
            message: Message::Super { skip_one: false },
            stret: true,
            fixup: false,
        },
    ),
    (
        "objc_msgSendSuper2_stret",
        Send {
            message: Message::Super { skip_one: true },
            stret: true,
            fixup: false,
        },
    ),
    // The pre-10.9 `message_ref_t` spellings: the selector register holds a
    // `{IMP, SEL}` ref whose `SEL` was uniqued at load.
    (
        "objc_msgSend_fixup",
        Send {
            message: Message::Direct,
            stret: false,
            fixup: true,
        },
    ),
    (
        "objc_msgSend_stret_fixup",
        Send {
            message: Message::Direct,
            stret: true,
            fixup: true,
        },
    ),
    (
        "objc_msgSendSuper2_fixup",
        Send {
            message: Message::Super { skip_one: true },
            stret: false,
            fixup: true,
        },
    ),
    (
        "objc_msgSendSuper2_stret_fixup",
        Send {
            message: Message::Super { skip_one: true },
            stret: true,
            fixup: true,
        },
    ),
];

/// Spellings that exist and are refused, with the reason attached.
static REFUSED: &[(&str, &str)] = &[
    (
        "objc_msgSend_fpret",
        "a long double return lives in st0, which has no arm64 counterpart",
    ),
    // The Objective-C exception entry points, refused *here* and served
    // elsewhere. Under the builtin runtime `crate::objcexc` rebinds
    // `objc_end_catch` and `objc_exception_rethrow` to the vendored `libc++abi`
    // after linking, and [`exception_shim`] answers the other two - so reaching
    // one of these refusals means either no C++ runtime to bind to, or the host's
    // libobjc, whose `objc_exception_throw` raises a *host* C++ exception with no
    // guest frame to unwind to.
    (
        "objc_end_catch",
        "an Objective-C exception is a C++ one, and there is no vendored \
         libc++abi in this program to bind it to",
    ),
    ("objc_exception_rethrow", "as objc_end_catch"),
    ("objc_begin_catch", "as objc_end_catch"),
    (
        "objc_exception_throw",
        "an Objective-C exception is a C++ one, and this program has no \
         vendored libc++abi to throw it through - or is running on the host's \
         libobjc, whose throw raises a host exception with no guest frame to \
         unwind to",
    ),
    // Associated objects key host storage by the object's address, which would
    // work - but the *policy* argument decides whether the value is copied, and
    // copying sends `copyWithZone:` from inside the runtime with none of this
    // runtime's state on the stack. Named rather than half-supported.
    (
        "objc_setAssociatedObject",
        "associated objects may copy their value, which messages from \
                                  inside the host runtime (M6)",
    ),
    ("objc_getAssociatedObject", "as objc_setAssociatedObject"),
    (
        "objc_removeAssociatedObjects",
        "as objc_setAssociatedObject",
    ),
    // A native guest binds these straight to the builtin runtime's stubs, which
    // jump into the block's `invoke`. Translated, that `invoke` is guest code
    // the host stub cannot jump to.
    (
        "imp_implementationWithBlock",
        "a block IMP jumps straight into the block's invoke, which is guest code",
    ),
    ("imp_getBlock", "as imp_implementationWithBlock"),
    ("imp_removeBlock", "as imp_implementationWithBlock"),
];

/// A nil-terminated object-list variadic, and the counted method that says the same
/// thing without an ellipsis.
///
/// These are the other half of what [`names_a_format`] covers, and the half that cannot
/// be rendered.
struct ObjectList {
    /// The selector the guest sends.
    variadic: &'static str,
    /// The counted selector to send instead.
    counted: &'static str,
    /// Whether the list reads `object, key, object, key, ...` rather than plain
    /// objects - and so becomes *two* arrays rather than one.
    pairs: bool,
}

/// The counted twin of each nil-terminated list a program reaches.
///
/// **Incomplete, and cannot be otherwise**, for the reason [`is_variadic`] gives.
static OBJECT_LISTS: &[ObjectList] = &[
    ObjectList {
        variadic: "arrayWithObjects:",
        counted: "arrayWithObjects:count:",
        pairs: false,
    },
    ObjectList {
        variadic: "dictionaryWithObjectsAndKeys:",
        counted: "dictionaryWithObjects:forKeys:count:",
        pairs: true,
    },
    ObjectList {
        variadic: "initWithObjects:",
        counted: "initWithObjects:count:",
        pairs: false,
    },
    ObjectList {
        variadic: "initWithObjectsAndKeys:",
        counted: "initWithObjects:forKeys:count:",
        pairs: true,
    },
    ObjectList {
        variadic: "orderedSetWithObjects:",
        counted: "orderedSetWithObjects:count:",
        pairs: false,
    },
    ObjectList {
        variadic: "setWithObjects:",
        counted: "setWithObjects:count:",
        pairs: false,
    },
];

/// Which of the three sends a spelling asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Message {
    /// `objc_msgSend(receiver, sel, ...)`.
    Direct,
    /// `objc_msgSendSuper[2](struct objc_super *, sel, ...)`.
    Super { skip_one: bool },
}

/// One message send's shape: which trampoline, and whether the caller left a
/// hidden return buffer in the first integer register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Send {
    message: Message,
    /// `objc_msgSend_stret`.
    stret: bool,
    /// `objc_msgSend_fixup`: the selector register holds a `message_ref_t`.
    fixup: bool,
}

impl Send {
    /// Index of the integer register holding the receiver: `rdi` ordinarily,
    /// `rsi` when a hidden buffer came first.
    fn receiver_reg(self) -> usize {
        usize::from(self.stret)
    }
}

/// The SysV AMD64 integer argument registers, in order.
fn arg_reg(ctx: &RegisterContext, i: usize) -> u64 {
    [ctx.rdi, ctx.rsi, ctx.rdx, ctx.rcx, ctx.r8, ctx.r9][i]
}

fn set_arg_reg(ctx: &mut RegisterContext, i: usize, v: u64) {
    *[
        &mut ctx.rdi,
        &mut ctx.rsi,
        &mut ctx.rdx,
        &mut ctx.rcx,
        &mut ctx.r8,
        &mut ctx.r9,
    ][i] = v;
}

/// Symbols this runtime answers for, resolved to the host's own implementations.
pub fn provider() -> SyntheticDylib {
    let mut lib = SyntheticDylib::new("<rosemary objc runtime>");
    // Flat, for the same reason the rest of the runtime's shims are: which
    // library an image thinks `objc_msgSend` comes from is not the question.
    lib.flat(true);
    // But it is the runtime, so a prefix's own libobjc is never loaded beside it.
    lib.alias("/usr/lib/libobjc.A.dylib")
        .alias("/usr/lib/libobjc.dylib");
    let names = RUNTIME
        .iter()
        .map(|r| r.name)
        .chain(MESSAGES.iter().map(|(n, _)| *n))
        .chain(REFUSED.iter().map(|(n, _)| *n))
        // libclosure. Published here rather than beside the frameworks because
        // it is the runtime that answers for it: `objc_retainBlock` and
        // `_Block_copy` are one function, and whichever runtime has one has the
        // other.
        .chain(objc::block::names());
    for name in names {
        // Never dropped. A name this provider does not define falls through to
        // the native provider - or, on a host that denies the family, goes
        // unresolved - and either way the guest gets an address or a silence
        // instead of the refusal `serve` would have named it with.
        let addr = host_symbol(name)
            .or_else(|| absent(name))
            .unwrap_or_else(unintercepted);
        lib.define_fn(&format!("_{name}"), addr as usize as *const ());
    }
    for name in DATA {
        if let Some(addr) = host_symbol(name) {
            lib.define_data(&format!("_{name}"), addr);
        }
    }
    // The class objects, which are data and which only the builtin runtime has
    // to publish. See [`super::host::class_data`].
    for (name, addr) in super::host::class_data() {
        lib.define_data(&name, addr);
    }
    for (name, addr) in super::host::block_data() {
        lib.define_data(&name, addr);
    }
    // Null on purpose under either runtime: Apple's libobjc exports these as
    // absolute zeros too.
    for name in ::objc::null_data() {
        lib.define_absent(name);
    }
    lib
}

/// The address of a runtime entry point the selected runtime does not define.
///
/// Never called: every name in the three tables is intercepted by name before
/// its address is read. It exists so that the *slot* resolves here rather than
/// somewhere else, which is what keeps the refusal named.
fn unintercepted() -> u64 {
    unsafe extern "C" fn stub() -> u64 {
        unreachable!("an Objective-C runtime entry point was called rather than intercepted")
    }
    stub as *const () as usize as u64
}

const DATA: &[&str] = &["objc_ehtype_vtable", "_objc_empty_cache"];

/// An address for a trampoline the *host* does not have.
///
/// The three `_stret` spellings exist only on x86-64.
fn absent(name: &str) -> Option<u64> {
    unsafe extern "C" fn stret() -> u64 {
        unreachable!("objc_msgSend_stret was called rather than intercepted")
    }
    unsafe extern "C" fn super_stret() -> u64 {
        unreachable!("objc_msgSendSuper_stret was called rather than intercepted")
    }
    unsafe extern "C" fn super2_stret() -> u64 {
        unreachable!("objc_msgSendSuper2_stret was called rather than intercepted")
    }
    let f: unsafe extern "C" fn() -> u64 = match name {
        "objc_msgSend_stret" => stret,
        "objc_msgSendSuper_stret" => super_stret,
        "objc_msgSendSuper2_stret" => super2_stret,
        _ => return None,
    };
    Some(f as *const () as usize as u64)
}

/// The debug overlay's seam into dispatch, and nothing where there is no overlay.
#[cfg(target_os = "macos")]
#[inline]
fn overlay_before_send(sel: u64, receiver: u64, argument: u64) {
    crate::gui::hook::before_send(sel, receiver, argument);
}

#[cfg(not(target_os = "macos"))]
#[inline]
fn overlay_before_send(_sel: u64, _receiver: u64, _argument: u64) {}

/// The selected runtime's own implementation of `name`, if it has one.
///
/// The one place this module asks where the runtime is. Both the addresses
/// handed to the guest by [`provider`] and the ones [`crate::objc::fast`] proves
/// equal before inlining come from here, so the backend is chosen once.
pub(super) fn host_symbol(name: &str) -> Option<u64> {
    super::host::symbol(name)
}

/// Which of this module's tables `name` is in, found once by [`resolve`] so that a
/// caller servicing the same import again does not search them by name again.
#[derive(Clone, Copy)]
pub struct Entry(Kind);

#[derive(Clone, Copy)]
enum Kind {
    Exception(crate::jmpbuf::Shim),
    /// The spelling from [`MESSAGES`], and what it means.
    Send(&'static str, Send),
    /// An index into [`RUNTIME`], which also holds the name.
    Runtime(usize),
    /// The spelling from [`REFUSED`], and why.
    Refused(&'static str, &'static str),
}

/// The entry for `name` (underscore already stripped), if it is one of this
/// module's. A function of the name and of the runtime backend, which is fixed
/// before the first guest call.
pub fn resolve(name: &str) -> Option<Entry> {
    // Before [`REFUSED`], which still lists these: the refusal is what a program
    // the shim cannot serve gets, and the shim decides which that is.
    let kind = if let Some(f) = exception_shim(name) {
        Kind::Exception(f)
    } else if let Some((name, message)) = MESSAGES.iter().find(|(n, _)| *n == name) {
        Kind::Send(name, *message)
    } else if let Ok(i) = RUNTIME.binary_search_by_key(&name, |r| r.name) {
        Kind::Runtime(i)
    } else if let Some((name, reason)) = REFUSED.iter().find(|(n, _)| *n == name) {
        Kind::Refused(name, reason)
    } else {
        return None;
    };
    Some(Entry(kind))
}

impl Entry {
    /// Service one call to the name this entry was resolved from.
    pub fn call(self, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
        match self.0 {
            Kind::Exception(f) => f(ctx, mem),
            Kind::Send(symbol, message) => send(symbol, message, current_vm(symbol)?, ctx, mem),
            Kind::Runtime(i) => {
                let symbol = RUNTIME[i].name;
                call_runtime(symbol, i, current_vm(symbol)?, ctx, mem)
            }
            Kind::Refused(symbol, reason) => Err(AbiError::Refused {
                symbol: symbol.to_string(),
                reason,
            }),
        }
    }
}

/// The two libobjc exception entry points that are more than an alias for their
/// C++ counterpart, and so are served here rather than bound by
/// [`crate::objcexc`].
///
/// Only under the builtin runtime. Under the host's libobjc the classes, the
/// `_OBJC_EHTYPE_$_` records and `objc_exception_throw` are all Apple's, and
/// half-replacing them would hand this runtime's `type_info` builder a class
/// structure it did not make.
fn exception_shim(symbol: &str) -> Option<crate::jmpbuf::Shim> {
    if !crate::frameworks::using_builtin() {
        return None;
    }
    Some(match symbol {
        "objc_begin_catch" => bind!("objc_begin_catch" => begin_catch),
        "objc_exception_throw" => bind!("objc_exception_throw" => exception_throw),
        _ => return None,
    })
}

/// `void objc_exception_throw(id object)`.
///
/// libobjc's allocates an exception holding the object, gives it the class's
/// `type_info`, and tail-calls `__cxa_throw`. So does this: the tail call is
/// what makes the frame that threw the frame the unwinder starts from, and a
/// shim that *called* `__cxa_throw` would put a host frame between the two.
fn exception_throw(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let vm = current_vm(symbol)?;
    let refused = |reason: &'static str| AbiError::Refused {
        symbol: symbol.to_string(),
        reason,
    };
    let object = ctx.rdi;
    let isa = (mem as &dyn GuestMemory).read_u64(object, symbol)?;
    // SAFETY: `objc_host_class` maps a guest class object to the one this
    // runtime registered for it, and leaves anything else alone - so the pointer
    // is either a class of this runtime's or not a class at all, which
    // `from_ptr` is the check for.
    let class =
        unsafe { objc::class::from_ptr(vm.objc_host_class(isa) as *const objc::class::Class) }
            .ok_or_else(|| refused("the thrown object's isa is not a class this runtime knows"))?;
    let ehtype = crate::objcexc::record_for(class).ok_or_else(|| {
        refused("no type_info for the thrown class; the program has no libc++abi")
    })?;
    let allocate = vm
        .program
        .resolved_symbol("___cxa_allocate_exception")
        .ok_or_else(|| refused("the program has no __cxa_allocate_exception"))?;
    let throw = vm
        .program
        .resolved_symbol("___cxa_throw")
        .ok_or_else(|| refused("the program has no __cxa_throw"))?;

    let mut inner = RegisterContext::new();
    inner.rdi = size_of::<u64>() as u64;
    let buffer = vm
        .call_with(allocate, inner)
        .map_err(|_| refused("__cxa_allocate_exception did not return"))?;
    mem.write_u64(buffer, object, symbol)?;

    // `__cxa_throw(exception, tinfo, destructor)`. No destructor: the object is
    // an `id` the guest still owns, and releasing it here would free something
    // the handler is about to be given.
    ctx.rdi = buffer;
    ctx.rsi = ehtype;
    ctx.rdx = 0;
    vm.tail_call(throw, ctx)
        .map_err(|_| refused("the guest could not be redirected into __cxa_throw"))
}

/// `id objc_begin_catch(void *exception)`.
///
/// libobjc's is `__cxa_begin_catch` and one dereference: the exception an
/// `@throw` allocates holds the `id` itself, and libc++abi hands back the
/// buffer, so the object is the word inside it. That dereference is the whole
/// reason this is a shim rather than an alias.
fn begin_catch(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let vm = current_vm(symbol)?;
    let cxa = vm
        .program
        .resolved_symbol("___cxa_begin_catch")
        .ok_or(AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "the program has no __cxa_begin_catch to forward to",
        })?;
    let mut inner = RegisterContext::new();
    inner.rdi = ctx.rdi;
    let buffer = vm.call_with(cxa, inner).map_err(|_| AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "__cxa_begin_catch did not return",
    })?;
    ctx.rax = (mem as &dyn GuestMemory).read_u64(buffer, symbol)?;
    Ok(())
}

/// The host address of every entry in [`RUNTIME`], resolved once.
///
/// `dlsym` takes the loader's lock and hashes the name: 208ns, paid inside every
/// `objc_retain` and `objc_release` the guest issued.
fn host_address(i: usize) -> Option<u64> {
    #[dynamic]
    static ADDRS: Vec<u64> = {
        RUNTIME
            .iter()
            .map(|r| host_symbol(r.name).unwrap_or(0))
            .collect()
    };
    (ADDRS[i] != 0).then_some(ADDRS[i])
}

/// Forward one runtime entry point, translating the classes that cross it.
fn call_runtime(
    symbol: &str,
    i: usize,
    vm: &Vm,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let _debug_context = objc::debug::guest_context(ctx.rip, symbol);
    let entry = &RUNTIME[i];
    let Some(host) = host_address(i) else {
        return Err(AbiError::Unsatisfied {
            symbol: symbol.to_string(),
        });
    };
    let convert = Classes {
        vm,
        classes: entry.classes,
        class_return: entry.class_return,
        callbacks: entry.callbacks,
        blocks: entry.blocks,
        objects: &[],
        object_return: false,
        protocols: entry.protocols,
    };
    let Some((shapes, layout)) = struct_return_shapes(i) else {
        return marshal::call_types(symbol, entry.types, host, ctx, mem, &convert);
    };
    marshal::call_laid_out(symbol, shapes, layout, host, ctx, mem, &convert)
}

/// The general marshaller's description of every [`RUNTIME`] entry with a
/// struct return - the one return a `&[ArgType]` cannot spell - built once.
fn struct_return_shapes(i: usize) -> Option<&'static (Vec<Shape>, marshal::Layout)> {
    #[dynamic]
    static SHAPES: Vec<Option<(Vec<Shape>, marshal::Layout)>> = {
        RUNTIME
            .iter()
            .map(|entry| {
                let leaves = entry.struct_return?;
                let mut shapes = vec![Shape::Struct(packed(leaves))];
                shapes.extend(entry.types.iter().skip(1).copied().map(Shape::Scalar));
                let layout = marshal::Layout::of(&shapes);
                Some((shapes, layout))
            })
            .collect()
    };
    SHAPES[i].as_ref()
}

/// A C struct laid out from its members, each at its own alignment.
///
/// The full layout rule, not a shortcut for the one caller.
fn packed(members: &[T]) -> Aggregate {
    let mut offset = 0usize;
    let mut leaves = Vec::with_capacity(members.len());
    for &ty in members {
        let width = aggregate::width_of(ty);
        offset = offset.next_multiple_of(width);
        leaves.push(Leaf { ty, offset });
        offset += width;
    }
    let align = members
        .iter()
        .map(|&ty| aggregate::width_of(ty))
        .max()
        .unwrap_or(1);
    Aggregate {
        size: offset.next_multiple_of(align),
        align,
        leaves,
    }
}

/// Class pointers, translated by parameter index.
struct Classes<'a> {
    vm: &'a Vm,
    classes: &'a [u8],
    class_return: bool,
    /// See [`Runtime::callbacks`]. Empty for a message send, where the `IMP` on
    /// the far side is the host's own and nothing of the guest's is installed.
    callbacks: &'a [u8],
    /// Parameter indices holding a block, from the method's own encoding.
    blocks: &'a [u8],
    /// Parameter indices holding a plain `id`, from the method's own encoding.
    objects: &'a [u8],
    /// Whether an `id` **return** gets the reverse substitution, for the reason
    /// `objects` gets the forward one.
    object_return: bool,
    /// See [`Runtime::protocols`]. Empty for a message send.
    protocols: &'a [u8],
}

impl Convert for Classes<'_> {
    fn argument(&self, symbol: &str, index: usize, raw: u64) -> Result<u64> {
        if self.callbacks.contains(&(index as u8)) {
            return crate::abi::Callbacks::substitute(self.vm, symbol, raw);
        }
        if self.blocks.contains(&(index as u8)) {
            // A block is edited where it lies and travels unchanged, which is
            // why this returns `raw` rather than a substitute - see
            // [`crate::shims::block`] for why the thunk has to be written into
            // the literal instead of kept beside it.
            if raw != 0 {
                let mut view = self.vm.view();
                crate::shims::block::substitute_block(symbol, raw, &mut view, self.vm)?;
            }
            return Ok(raw);
        }
        if self.protocols.contains(&(index as u8)) {
            return Ok(self.vm.objc_host_protocol(raw));
        }
        match self.classes.contains(&(index as u8)) || self.objects.contains(&(index as u8)) {
            // A block passed as a plain `id` (`objc_retain`, `objc_storeStrong`,
            // an `@` argument) gets copied by the runtime just as one passed to
            // `objc_retainBlock` does, and the copy runs its helpers.
            true if crate::shims::block::is_block(raw, self.vm.view()) => {
                let mut view = self.vm.view();
                crate::shims::block::substitute_block(symbol, raw, &mut view, self.vm)?;
                Ok(raw)
            }
            // Not `objc_host_class`: an `@` in a method encoding is `id`,
            // `Class` and `Protocol *` alike, so both maps are consulted. See
            // [`Vm::objc_host_id`].
            true => Ok(self.vm.objc_host_id(raw)),
            false => Ok(raw),
        }
    }

    fn result(&self, raw: u64) -> u64 {
        match self.class_return || self.object_return {
            true => self.vm.objc_guest_class(raw),
            false => raw,
        }
    }
}

/// Send one message.
///
/// On success either `rip` is the guest `IMP` the message resolved to - the
/// transfer performed, not a call made - or the host method has run and its
/// result is in the guest's return register.
fn send(
    symbol: &str,
    send: Send,
    vm: &Vm,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let _debug_context = objc::debug::guest_context(ctx.rip, symbol);
    let base = send.receiver_reg();
    if send.fixup {
        let msg_ref = arg_reg(ctx, base + 1);
        let sel = (mem as &dyn GuestMemory).read_u64(msg_ref.wrapping_add(8), symbol)?;
        set_arg_reg(ctx, base + 1, sel);
    }
    let sel = arg_reg(ctx, base + 1);

    // Where the receiver is, and where the method search starts, which for a
    // `super` send are two different classes.
    let (receiver, search) = match send.message {
        Message::Direct => (arg_reg(ctx, base), None),
        Message::Super { skip_one } => {
            let sup = arg_reg(ctx, base);
            let receiver = (mem as &dyn GuestMemory).read_u64(sup, symbol)?;
            let named = (mem as &dyn GuestMemory).read_u64(sup.wrapping_add(8), symbol)?;
            let named = vm.objc_host_class(named) as host::Class;
            // SAFETY: `named` is a `Class` - either the host's own, arrived
            // through an ordinary bind, or one this runtime registered. A
            // superclass query on one is what the runtime is for.
            let start = match skip_one {
                true => unsafe { host::class_getSuperclass(named) },
                false => named,
            };
            (receiver, Some(start))
        }
    };

    // Messaging nil is not an error, it is the language: the result is zero, in
    // whichever register the caller reads.
    if receiver == 0 {
        ctx.rax = 0;
        ctx.xmm[0] = [0, 0];
        return Ok(());
    }

    // The receiver as the host runtime knows it.
    let host_receiver = vm.objc_host_class(receiver);
    // A receiver in guest memory is not automatically broken.
    if vm.program.is_guest_address(host_receiver) {
        let isa = vm.objc_host_class((mem as &dyn GuestMemory).read_u64(host_receiver, symbol)?);
        if isa == 0 || vm.program.is_guest_address(isa) {
            return Err(AbiError::Refused {
                symbol: symbol.to_string(),
                reason: "the receiver is a guest address whose class the runtime never realized",
            });
        }
    }

    // The overlay's seam.
    overlay_before_send(sel, host_receiver, arg_reg(ctx, base + 2));

    // SAFETY: `host_receiver` is a pointer the host runtime owns - an object it
    // allocated, or a class it realized - which is what `object_getClass` takes.
    let class = match search {
        Some(start) => start,
        None => unsafe { host::object_getClass(host_receiver as host::Class) },
    };
    if class.is_null() {
        return Err(AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "the receiver's class could not be determined",
        });
    }

    let plan = plan_for(symbol, vm, class, sel)?;

    // `ROSEMARY_TRACE_MSG` reports this send, if it names its selector.
    let watching = traced(sel);
    if let Some(name) = &watching {
        eprintln!(
            "rosemary: msg -[{} {name}] {} args={:#x},{:#x},{:#x}",
            class_of(host_receiver),
            match plan.to {
                Target::Guest(_) => "guest",
                Target::Host { .. } => "host",
            },
            arg_reg(ctx, base + 2),
            arg_reg(ctx, base + 3),
            arg_reg(ctx, base + 4),
        );
    }

    let (imp, signature, layout, variadic, stret) = match &plan.to {
        // Guest -> guest: the trampoline's whole job is the jump, and the
        // guest's registers are already the callee's. A `super` send arrived
        // with the `objc_super` structure in `rdi`, and the method is called
        // with the receiver.
        Target::Guest(guest_imp) => {
            set_arg_reg(ctx, base, receiver);
            return transfer(symbol, ctx, mem, *guest_imp);
        }
        Target::Host {
            imp,
            signature,
            layout,
            variadic,
            stret,
        } => (*imp, signature, layout, *variadic, *stret),
    };

    // Guest -> host, with the arguments the method's own encoding does not
    // describe dealt with first.
    match variadic {
        // A nil-terminated object list has no format to fold its arguments
        // into, so it is answered by sending the counted twin instead - which
        // returns rather than falling through to the send below, because the
        // selector that reaches the host is not the one the guest sent.
        Variadic::List(list) => {
            return send_object_list(
                vm,
                symbol,
                list,
                signature,
                send,
                class,
                host_receiver,
                base,
                ctx,
                mem,
            );
        }
        Variadic::Format => render_variadic(symbol, sel, signature, base, ctx, mem)?,
        Variadic::No => {}
    }
    // Two independent statements about the same fact, held to each other.
    if stret != send.stret {
        return Err(AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "the trampoline the guest called disagrees with the method's own \
                     return class",
        });
    }
    // Everything fallible happens before `rdi` moves, so a refusal leaves the
    // guest context exactly as it was found - the guarantee every other refusal
    // in this project makes.
    set_arg_reg(ctx, base, host_receiver);
    let convert = Classes {
        vm,
        classes: &signature.classes,
        class_return: signature.class_return,
        callbacks: &[],
        blocks: &signature.blocks,
        objects: &signature.objects,
        object_return: signature.object_return,
        protocols: &[],
    };
    // Named by the *message* when it fails.
    let result = marshal::call_laid_out(symbol, &signature.shapes, layout, imp, ctx, mem, &convert)
        .map_err(|e| {
            // `object_getClass` and `class_getName` are runtime calls rather
            // than messages, which matters here.
            let class = class_of(host_receiver);
            let sel = selector_name(sel).unwrap_or_else(|| format!("<sel {sel:#x}>"));
            e.renaming(format!("{symbol} -[{class} {sel}]"))
        });
    if let Some(name) = &watching
        && result.is_ok()
    {
        eprintln!("rosemary: msg -[? {name}] -> {:#x}", ctx.rax);
    }
    result
}

/// What a message to one class and selector resolves to.
///
/// Everything here is derived from the class, the selector and the method's own type
/// encoding, none of which changes between two sends - so it is derived once. Release,
/// per host-bound send to `-[NSString stringByAppendingString:]`: 105ns in
/// `class_getInstanceMethod`, 61ns parsing `@24@0:8@16`, 89ns rebuilding the selector's
/// name for the variadic check, against 12ns for a validated hit.
struct Plan {
    /// The answer [`host::class_getMethodImplementation`] gave when this was built.
    ///
    /// Checked against a fresh one on every hit, which is what makes the entry safe to
    /// keep: swizzling a method - `method_setImplementation`, a category loaded later,
    /// an override added to a subclass - installs a different `IMP`, and that
    /// difference is the invalidation.
    probe: u64,
    to: Target,
}

/// Where a resolved message goes, and what the crossing needs to know.
enum Target {
    /// The `IMP` is one of this runtime's thunks, and the message is a jump to the
    /// guest address behind it.
    ///
    /// **The method's type encoding is not decoded for one of these, and must not be.**
    /// [`add_method`] deliberately installs a guest method whose encoding this decoder
    /// refuses - recording `MethodNotHostCallable` and moving on - precisely so that a
    /// *guest* sender can still reach it. Decoding here made a union in a guest
    /// method's signature refuse a send that never needed it.
    ///
    /// Safe to keep alongside `probe` because a thunk's address never comes to mean a
    /// different guest function, and a new thunk is a fresh address in the arena rather
    /// than one already installed as an `IMP`.
    Guest(u64),
    /// A host `IMP`, and everything the marshaller needs in front of it.
    Host {
        imp: u64,
        signature: Signature,
        /// Where the guest leaves each of `signature`'s arguments.
        layout: marshal::Layout,
        variadic: Variadic,
        /// Whether the method's own encoding returns MEMORY-class, i.e. `_stret`.
        stret: bool,
    },
}

/// Which of the two things an ellipsis means here, decided from the selector.
#[derive(Clone, Copy)]
enum Variadic {
    No,
    /// Arguments to fold into a format string.
    Format,
    /// A nil-terminated object list, and the counted selector to send instead.
    List(&'static ObjectList),
}

/// The resolutions made so far, keyed by `(class, selector)`.
///
/// Address-hashed: this is read on every send, and SipHash over the two words was
/// as much as the rest of the read.
type Plans = std::collections::HashMap<(u64, u64), &'static Plan, crate::program::AddrHash>;

fn plans() -> &'static std::sync::RwLock<Plans> {
    static PLANS: std::sync::OnceLock<std::sync::RwLock<Plans>> = std::sync::OnceLock::new();
    PLANS.get_or_init(Default::default)
}

/// Resolve one message, from the cache when the class still answers it the same way.
fn plan_for(symbol: &str, vm: &Vm, class: host::Class, sel: u64) -> Result<&'static Plan> {
    // The lookup `objc_msgSend` itself performs, so it is answered out of the
    // class's own method cache: 6ns, against 105ns for the
    // `class_getInstanceMethod` below, which walks the superclass chain under
    // the runtime lock.
    //
    // SAFETY: `class` is a `Class` and `sel` a uniqued `SEL` - class realization
    // rewrote the guest's `__objc_selrefs` slots from string pointers into the
    // host runtime's own selectors, so the value the guest loaded *is* one.
    let probe = unsafe { host::class_getMethodImplementation(class, sel as host::Sel) } as u64;
    let key = (class as u64, sel);
    if let Some(&plan) = plans().read().unwrap_or_else(|e| e.into_inner()).get(&key)
        && plan.probe == probe
    {
        return Ok(plan);
    }

    // One lookup for both halves of what a send needs. A class method is an
    // instance method of the metaclass, and `class` is already the metaclass
    // when the receiver is a class, so this answers for both.
    //
    let method = unsafe { host::class_getInstanceMethod(class, sel as host::Sel) };
    if method.is_null() {
        return Err(unrecognised(symbol, class, sel));
    }
    // SAFETY: `method` is a `Method` the runtime just returned for `class`.
    let imp = unsafe { host::method_getImplementation(method) } as u64;
    let to = match vm.guest_behind_thunk(imp) {
        Some(guest_imp) => Target::Guest(guest_imp),
        None => {
            let signature = signature_of(symbol, method)?;
            Target::Host {
                imp,
                stret: matches!(
                    signature.shapes.first(),
                    Some(Shape::Struct(agg)) if agg.guest_class() == GuestClass::Memory
                ),
                variadic: match (is_variadic(sel), object_list(sel)) {
                    (true, Some(list)) => Variadic::List(list),
                    (true, None) => Variadic::Format,
                    (false, _) => Variadic::No,
                },
                layout: marshal::Layout::of(&signature.shapes),
                signature,
            }
        }
    };
    let plan = Plan { probe, to };
    // Leaked rather than reference-counted: a plan is read on every send and
    // there is one per method a program actually messages, plus one more each
    // time a method is swizzled underneath it.
    let plan: &'static Plan = Box::leak(Box::new(plan));
    plans()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, plan);
    Ok(plan)
}

/// Whether `ROSEMARY_TRACE_MSG` asked for this selector, and its name.
fn traced(sel: u64) -> Option<String> {
    #[dynamic]
    static WANTED: Vec<String> = {
        std::env::var("ROSEMARY_TRACE_MSG")
            .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default()
    };
    let wanted = &WANTED;
    if wanted.is_empty() {
        return None;
    }
    let name = selector_name(sel)?;
    // `*` traces everything. A program that has stopped somewhere unexpected
    // does not tell you which selector to ask for, and the whole sequence is
    // what says where it went - so the list a reader can name is not always the
    // list a reader needs.
    let all = wanted.iter().any(|w| w == "*");
    (all || wanted.contains(&name)).then_some(name)
}

/// The receiver's class name, for a diagnostic - never by messaging it.
fn class_of(receiver: u64) -> String {
    if receiver == 0 {
        return String::from("nil");
    }
    // SAFETY: `receiver` is an object pointer the guest passed and this shim
    // has already substituted for the host's, so the host runtime owns its
    // `isa`. Both calls read runtime metadata and send nothing.
    unsafe {
        let cls = host::object_getClass(receiver as host::Class);
        if cls.is_null() {
            return String::new();
        }
        let name = host::class_getName(cls);
        if name.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr(name)
            .to_string_lossy()
            .into_owned()
    }
}

/// Continue the guest at `target`, as the trampoline would have.
fn transfer(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    target: u64,
) -> Result<()> {
    let Some(state) = crate::thread::current() else {
        return Err(AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "a message sent outside a running guest thread",
        });
    };
    if !state.entered_by_jump() {
        // An intercepted `call`: microlift exits before the push, so the return
        // address is this runtime's to write.
        let rsp = ctx.rsp.wrapping_sub(8);
        mem.write(rsp, &ctx.rip.to_le_bytes())
            .map_err(|_| AbiError::Memory {
                symbol: symbol.to_string(),
                addr: rsp,
            })?;
        ctx.rsp = rsp;
    }
    ctx.rip = target;
    // The live stack pointer moved without an exit reporting it, and `rip` is
    // now a destination rather than a resume point.
    state.rsp.set(ctx.rsp);
    state.redirect();
    Ok(())
}

/// The method's type encoding, decoded, or a refusal naming what stopped it.
fn signature_of(symbol: &str, method: host::Method) -> Result<Signature> {
    // SAFETY: `method` is a `Method` the runtime returned; its encoding is a
    // NUL-terminated string the runtime owns, read and copied before anything
    // else happens.
    let types = unsafe { host::method_getTypeEncoding(method) };
    if types.is_null() {
        return Err(AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "the method carries no type encoding, so its signature is unknown",
        });
    }
    let types = unsafe { std::ffi::CStr::from_ptr(types) };
    let Ok(types) = types.to_str() else {
        return Err(AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "the method's type encoding is not valid UTF-8",
        });
    };

    encoding::signature(types).map_err(|reason| AbiError::Refused {
        symbol: symbol.to_string(),
        reason,
    })
}

/// Whether the selector names a method that takes an ellipsis.
fn is_variadic(sel: u64) -> bool {
    let Some(name) = selector_bytes(sel) else {
        return false;
    };
    names_a_format(name) || OBJECT_LISTS.iter().any(|l| l.variadic.as_bytes() == name)
}

/// The counted twin of a nil-terminated list selector, if this is one.
fn object_list(sel: u64) -> Option<&'static ObjectList> {
    let name = selector_bytes(sel)?;
    OBJECT_LISTS.iter().find(|l| l.variadic.as_bytes() == name)
}

/// Whether the selector's last keyword names a *format string* parameter.
fn names_a_format(selector: &[u8]) -> bool {
    let Some(stripped) = selector.strip_suffix(b":") else {
        return false;
    };
    let last = match stripped.iter().rposition(|&c| c == b':') {
        Some(i) => &stripped[i + 1..],
        None => stripped,
    };
    let (before, word) = last_word(last);
    if !word.eq_ignore_ascii_case(b"format") {
        return false;
    }
    if before.is_empty() {
        return true;
    }
    let (_, previous) = last_word(before);
    [b"with".as_slice(), b"append", b"appending"]
        .iter()
        .any(|w| previous.eq_ignore_ascii_case(w))
}

/// Split one camelCase word off the end: `stringByAppending` -> `(stringBy, Appending)`.
fn last_word(s: &[u8]) -> (&[u8], &[u8]) {
    let start = (1..s.len())
        .rev()
        .find(|&i| s[i].is_ascii_uppercase())
        .unwrap_or(0);
    s.split_at(start)
}

/// Turn a variadic format-style send into the non-variadic send of the rendered string,
/// in place.
///
/// The shapes this cannot do are still refused by name.
fn render_variadic(
    symbol: &str,
    sel: u64,
    signature: &Signature,
    base: usize,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    // Named, because "a variadic method this cannot render" is a work item and
    // the selector is the whole of it - the reader otherwise learns only that
    // *some* message was refused, on a list that cannot be enumerated from the
    // image (see [`VARIADIC_SELECTORS`]).
    let refuse = |reason: &str| {
        let selector = selector_name(sel).unwrap_or_else(|| format!("<sel {sel:#x}>"));
        Err(AbiError::Nested {
            symbol: symbol.to_string(),
            reason: format!("{selector}: {reason}"),
        })
    };
    // Every named parameter must be integer-class, or the format is not in the
    // register this computes and the variadic tail does not start after it.
    let params = &signature.shapes[1..];
    if !params.iter().all(|s| s.scalar() == Some(T::Pointer)) {
        return refuse(
            "a variadic method whose named parameters are not all objects; the format's \
             register cannot be computed",
        );
    }
    // `self`, `_cmd`, then the arguments - the format is the last of them, and
    // `base` accounts for a hidden struct-return buffer ahead of all of it.
    let Some(fmt_reg) = params.len().checked_sub(1).map(|i| i + base) else {
        return refuse("a variadic method with no named format argument");
    };
    if fmt_reg >= SYSV_INT_REGS {
        return refuse("a variadic method whose format is not in an argument register");
    }
    let Some(text) =
        crate::shims::nslog::render_format(symbol, ctx, &*mem, arg_reg(ctx, fmt_reg), fmt_reg + 1)?
    else {
        // Not a readable string. Left alone rather than refused: the method
        // will do to it whatever it does to any other argument.
        return Ok(());
    };
    let Some(replacement) = crate::shims::nslog::escaped_string(&text) else {
        return refuse("the rendered format could not be made into a string");
    };
    set_arg_reg(ctx, fmt_reg, replacement);
    Ok(())
}

/// The SysV AMD64 integer argument registers there are.
const SYSV_INT_REGS: usize = 6;

/// How many objects a nil-terminated list may hold before this stops walking.
///
/// A list with no terminator is indistinguishable from a long one until the read
/// faults, and the read need not fault.
const OBJECT_LIST_CAP: usize = 4096;

/// Send the counted twin of a nil-terminated object-list message.
///
/// The three questions this has to answer are all about *where the list is*, and SysV
/// AMD64 answers each of them.
#[allow(clippy::too_many_arguments)]
fn send_object_list(
    vm: &Vm,
    symbol: &str,
    list: &ObjectList,
    signature: &Signature,
    send: Send,
    class: host::Class,
    host_receiver: u64,
    base: usize,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let refuse = |reason: String| {
        Err(AbiError::Nested {
            symbol: symbol.to_string(),
            reason: format!("{}: {reason}", list.variadic),
        })
    };
    // The guest called `objc_msgSend_stret` for a method returning an object.
    if send.stret {
        return refuse("sent through the struct-return trampoline, which returns an object".into());
    }
    // `self`, `_cmd`, `firstObject`, each integer-class - which is what makes
    // the register the list starts in computable at all.
    let params = &signature.shapes[1..];
    if params.len() != 3 || !params.iter().all(|s| s.scalar() == Some(T::Pointer)) {
        return refuse(format!(
            "expected a receiver, a selector and one named object, and the method's \
             encoding describes {} named parameter(s)",
            params.len().saturating_sub(2)
        ));
    }

    // Walk it. `firstObject` is at the last named register; the ellipsis
    // continues from the next one and spills to the stack after `r9`.
    let mut items = Vec::new();
    let mut reg = base + 2;
    let mut spill = 0u64;
    loop {
        let raw = match reg < SYSV_INT_REGS {
            true => arg_reg(ctx, reg),
            false => {
                let at = ctx.rsp.wrapping_add(spill);
                spill += 8;
                (mem as &dyn GuestMemory).read_u64(at, symbol)?
            }
        };
        reg += 1;
        if raw == 0 {
            break;
        }
        if items.len() == OBJECT_LIST_CAP {
            return refuse(format!(
                "a list of more than {OBJECT_LIST_CAP} objects with no nil terminator found"
            ));
        }
        // A guest class is legal in any of these - `@[SomeClass]` is an array
        // of one object - and is the one pointer that has to be translated.
        items.push(vm.objc_host_class(raw) as *mut std::ffi::c_void);
    }

    // The counted twin, looked up on the same class the variadic one was found
    // on, so a receiver that does not have it is named rather than assumed.
    let Some(name) = host::c_string(list.counted) else {
        return refuse("the counted selector is not a C string".into());
    };
    // SAFETY: `name` is a NUL-terminated string that outlives the call, and
    // `class` is a `Class` the runtime returned.
    let counted = unsafe { host::sel_registerName(name.as_ptr()) };
    // SAFETY: as above; `counted` is a uniqued `SEL`.
    let method = unsafe { host::class_getInstanceMethod(class, counted) };
    if method.is_null() {
        return refuse(format!(
            "the receiver's class does not implement {}",
            list.counted
        ));
    }
    // SAFETY: `method` is a `Method` the runtime just returned for `class`.
    let imp = unsafe { host::method_getImplementation(method) };
    // A guest class overriding the counted form would be re-entered as arm64.
    if vm.guest_behind_thunk(imp as u64).is_some() {
        return refuse(format!("{} is implemented by the guest", list.counted));
    }

    let receiver = host_receiver as *mut std::ffi::c_void;
    let result = match list.pairs {
        false => {
            // SAFETY: `imp` implements `...WithObjects:count:`, whose prototype
            // is exactly this - an object array and its length. `items` is
            // alive across the call and holds `items.len()` objects.
            let f: extern "C" fn(
                *mut std::ffi::c_void,
                host::Sel,
                *const *mut std::ffi::c_void,
                usize,
            ) -> u64 = unsafe { std::mem::transmute(imp) };
            f(receiver, counted, items.as_ptr(), items.len())
        }
        true => {
            // The list is `object, key, object, key, ...`; an odd one is a guest
            // bug, and Foundation's answer to it is a raise this cannot catch.
            if items.len() % 2 != 0 {
                return refuse(format!(
                    "{} objects and keys, which is not a whole number of pairs",
                    items.len()
                ));
            }
            let (objects, keys): (Vec<_>, Vec<_>) =
                items.chunks_exact(2).map(|p| (p[0], p[1])).unzip();
            // SAFETY: as above, for `...Objects:forKeys:count:`. Both vectors
            // hold `objects.len()` objects and are alive across the call.
            let f: extern "C" fn(
                *mut std::ffi::c_void,
                host::Sel,
                *const *mut std::ffi::c_void,
                *const *mut std::ffi::c_void,
                usize,
            ) -> u64 = unsafe { std::mem::transmute(imp) };
            f(
                receiver,
                counted,
                objects.as_ptr(),
                keys.as_ptr(),
                objects.len(),
            )
        }
    };
    ctx.rax = result;
    Ok(())
}

/// A uniqued selector's name, borrowed from the runtime rather than copied.
///
/// The `'static` is the runtime's own promise: a `SEL` and its name are interned for
/// the life of the process. Borrowing matters because the variadic check runs on every
/// send, and copying the name there was an allocation per message.
fn selector_bytes(sel: u64) -> Option<&'static [u8]> {
    if sel == 0 {
        return None;
    }
    // SAFETY: `sel` is a uniqued `SEL`; `sel_getName` returns a NUL-terminated
    // string the runtime owns and never frees.
    let name = unsafe { host::sel_getName(sel as host::Sel) };
    if name.is_null() {
        return None;
    }
    Some(unsafe { std::ffi::CStr::from_ptr(name) }.to_bytes())
}

/// A uniqued selector as its name, for a diagnostic.
fn selector_name(sel: u64) -> Option<String> {
    selector_bytes(sel).map(|b| String::from_utf8_lossy(b).into_owned())
}

/// "This class does not implement that selector", with both names in it.
fn unrecognised(symbol: &str, class: host::Class, sel: u64) -> AbiError {
    // SAFETY: `class` is a `Class` the runtime owns; `class_getName` returns a
    // string it owns.
    let name = unsafe { host::class_getName(class) };
    let name = match name.is_null() {
        true => "<unnamed>".to_string(),
        false => unsafe { std::ffi::CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned(),
    };
    let selector = selector_name(sel).unwrap_or_else(|| format!("<sel {sel:#x}>"));
    AbiError::Nested {
        symbol: symbol.to_string(),
        reason: format!(
            "{name} does not respond to {selector}; message forwarding builds an NSInvocation \
             describing a frame this runtime does not have (M6)"
        ),
    }
}

/// The VM running this thread, which a message needs for the class maps and for
/// entering guest code.
fn current_vm(symbol: &str) -> Result<&'static Vm> {
    crate::thunk::current().ok_or_else(|| AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "messaging needs the JIT, and no VM is running on this thread",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_entries_are_sorted_and_unique() {
        for pair in RUNTIME.windows(2) {
            assert!(
                pair[0].name < pair[1].name,
                "{} and {} are out of order; binary_search would miss one",
                pair[0].name,
                pair[1].name
            );
        }
    }

    #[test]
    fn a_struct_return_lands_in_the_guests_two_registers() {
        // `objc_method_description` is the shape the whole struct-return path
        // exists for, and what makes it cheap is that *both* conventions put it
        // in two integer registers.
        let agg = packed(&[P, P]);
        assert_eq!(agg.size, 16);
        assert_eq!(agg.align, 8);
        assert_eq!(agg.leaves[1].offset, 8);
        assert_eq!(
            agg.guest_class(),
            GuestClass::Registers(vec![
                crate::abi::aggregate::Eightbyte::Integer,
                crate::abi::aggregate::Eightbyte::Integer,
            ])
        );
        // Two integer registers on either host, said in each one's own words.
        #[cfg(target_arch = "aarch64")]
        assert_eq!(
            agg.host_class(),
            Ok(crate::abi::aggregate::HostClass::Integer { words: 2 })
        );
        #[cfg(target_arch = "x86_64")]
        assert_eq!(
            agg.host_class(),
            Ok(crate::abi::aggregate::HostClass::Eightbytes {
                first: crate::abi::aggregate::Eightbyte::Integer,
                second: Some(crate::abi::aggregate::Eightbyte::Integer),
            })
        );
    }

    #[test]
    fn a_struct_returning_entry_leaves_its_scalar_return_empty() {
        // `types[0]` is the return everywhere else in this table, so an entry
        // that describes its return twice would have the marshaller write a
        // scalar over the struct it just placed.
        for entry in RUNTIME.iter().filter(|r| r.struct_return.is_some()) {
            assert_eq!(
                entry.types[0],
                T::None,
                "{}'s scalar return would overwrite its struct",
                entry.name
            );
        }
    }

    #[test]
    fn every_name_this_module_claims_is_one_it_can_serve() {
        // The provider and the shim table are two lists that must agree.
        for name in RUNTIME
            .iter()
            .map(|r| r.name)
            .chain(MESSAGES.iter().map(|(n, _)| *n))
            .chain(REFUSED.iter().map(|(n, _)| *n))
        {
            assert!(resolve(name).is_some(), "{name} is claimed and not served");
            assert!(
                crate::shims::is_shimmed(&format!("_{name}")),
                "{name} would be promoted to an inlined host call"
            );
        }
    }

    /// Runtime entry points the builtin runtime does not implement yet.
    ///
    /// A named list rather than a silence: each of these is a guest that stops
    /// with "unsatisfied import" under `ROSEMARY_OBJC=builtin`, and the name is
    /// the whole of the work queue.
    const BUILTIN_GAPS: &[&str] = &[
        // A class here belongs to the runtime rather than to the file it was
        // declared in, so there is nothing to read a compiler-laid-out
        // `objc_class` out of - that pass is `crate::objc::register`, and it runs
        // at load time from the link transcript rather than on a guest's say-so.
        "objc_readClassPair",
    ];

    #[test]
    fn the_selected_runtime_really_exports_what_this_module_forwards_to() {
        // Every entry in the table is a passthrough, so a name misspelled here
        // is a program that stops with "unsatisfied import" at run time.
        for entry in RUNTIME {
            if BUILTIN_GAPS.contains(&entry.name)
                && super::super::host::backend() != super::super::host::Backend::Native
            {
                continue;
            }
            assert!(
                host_symbol(entry.name).is_some(),
                "the {} runtime has no {}",
                super::super::host::backend(),
                entry.name
            );
        }
    }

    /// Nothing in the three tables goes unresolved, whichever runtime is chosen.
    ///
    /// A name this provider drops binds somewhere else - or nowhere - and either
    /// way the guest gets an address or a silence instead of the refusal
    /// [`serve`] would have named it with.
    #[test]
    fn the_provider_defines_every_name_the_shim_claims() {
        let lib = provider();
        let mut defined = std::collections::HashSet::new();
        machish::resolve::SymbolProvider::enumerate(&lib, &mut |name, _| {
            defined.insert(name.to_string());
        });
        let names = RUNTIME
            .iter()
            .map(|r| r.name)
            .chain(MESSAGES.iter().map(|(n, _)| *n))
            .chain(REFUSED.iter().map(|(n, _)| *n));
        let missing: Vec<String> = names
            .map(|n| format!("_{n}"))
            .filter(|n| !defined.contains(n))
            .collect();
        assert!(
            missing.is_empty(),
            "the objc provider defines none of {missing:?}"
        );
    }

    #[test]
    fn empty_vtable_binds_to_null() {
        let mut found = None;
        machish::resolve::SymbolProvider::enumerate(&provider(), &mut |name, resolved| {
            if name == "__objc_empty_vtable" {
                found = Some(resolved);
            }
        });
        let resolved = found.expect("the builtin runtime defines __objc_empty_vtable");
        assert_eq!(resolved.addr, 0);
        assert_eq!(resolved.kind, machish::resolve::SymbolKind::Absent);
    }

    #[test]
    fn empty_cache_binds_to_an_empty_cache() {
        let mut found = None;
        machish::resolve::SymbolProvider::enumerate(&provider(), &mut |name, resolved| {
            if name == "__objc_empty_cache" {
                found = Some(resolved);
            }
        });
        let resolved = found.expect("the selected runtime defines __objc_empty_cache");
        assert_eq!(resolved.kind, machish::resolve::SymbolKind::Data);
        assert_ne!(resolved.addr, 0);
        // `mask` and `occupied` both zero: no buckets to probe.
        let head = unsafe { *(resolved.addr as usize as *const u64) };
        assert_eq!(head, 0);
    }

    /// The gaps are gaps, and not names that quietly started working.
    ///
    /// A list that outlives what it describes is worse than no list.
    #[test]
    fn every_named_gap_is_still_a_gap() {
        if super::super::host::backend() == super::super::host::Backend::Native {
            return;
        }
        for name in BUILTIN_GAPS {
            assert!(
                host_symbol(name).is_none(),
                "{name} is implemented now; take it out of BUILTIN_GAPS"
            );
        }
    }

    #[test]
    fn a_refused_spelling_is_never_also_served_as_a_message() {
        for (name, _) in REFUSED {
            assert!(
                !MESSAGES.iter().any(|(m, _)| m == name)
                    && RUNTIME.binary_search_by_key(name, |r| r.name).is_err(),
                "{name} is both refused and implemented"
            );
        }
    }

    #[test]
    fn the_variadic_check_is_by_selector_name() {
        // `sel_registerName` is the host's own, so this is checking the rule
        // against real selectors rather than against a string.
        // SAFETY: both are NUL-terminated literals.
        let format = unsafe { host::sel_registerName(c"stringWithFormat:".as_ptr()) } as u64;
        let objects = unsafe { host::sel_registerName(c"arrayWithObjects:".as_ptr()) } as u64;
        let plain = unsafe { host::sel_registerName(c"count".as_ptr()) } as u64;
        assert!(is_variadic(format));
        assert!(is_variadic(objects));
        assert!(!is_variadic(plain));
        assert!(!is_variadic(0));
    }

    #[test]
    fn a_format_keyword_is_a_word_not_a_suffix() {
        for yes in [
            "stringWithFormat:",
            "initWithFormat:",
            "localizedStringWithFormat:",
            "appendFormat:",
            "stringByAppendingFormat:",
            "raise:format:",
            "predicateWithFormat:",
        ] {
            assert!(names_a_format(yes.as_bytes()), "{yes} should name a format");
        }
        // `initWithFrame:pixelFormat:` is the one that mattered.
        for no in [
            "initWithFrame:pixelFormat:",
            "setPixelFormat:",
            "audioFormat:",
            "count",
            "format",
            "",
        ] {
            assert!(
                !names_a_format(no.as_bytes()),
                "{no} should not name a format"
            );
        }
    }

    #[test]
    fn a_plan_can_be_validated_by_the_cheap_lookup() {
        // `class_getMethodImplementation` answers out of the class's own method
        // cache; `class_getInstanceMethod` walks the superclass chain under the
        // runtime lock. The plan cache keeps the second's answer and re-checks
        // it with the first, which only works while the two agree.
        // SAFETY: NUL-terminated literals; every pointer is checked before use.
        let mut checked = 0;
        unsafe {
            for (class, selector) in [
                (c"NSString".as_ptr(), c"stringByAppendingString:".as_ptr()),
                (c"NSString".as_ptr(), c"length".as_ptr()),
                (c"NSObject".as_ptr(), c"class".as_ptr()),
                (c"NSObject".as_ptr(), c"isEqual:".as_ptr()),
                (c"NSMutableArray".as_ptr(), c"addObject:".as_ptr()),
            ] {
                // A Foundation class the selected runtime does not have is not
                // a disagreement between the two lookups; it is a Foundation
                // this tree has not written yet.
                let cls = host::objc_getClass(class);
                if cls.is_null() {
                    continue;
                }
                let sel = host::sel_registerName(selector);
                let method = host::class_getInstanceMethod(cls, sel);
                assert!(!method.is_null());
                assert_eq!(
                    host::method_getImplementation(method),
                    host::class_getMethodImplementation(cls, sel),
                    "the two lookups disagree, so no plan would ever be reused"
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "no class in the list was found at all");
    }

    #[test]
    fn swizzling_changes_the_answer_a_plan_is_checked_against() {
        // The whole of the cache's invalidation: a method replaced underneath a
        // resolved plan must make the cheap lookup report something else.
        //
        // The two bodies differ, and must. Empty ones are identical machine code,
        // and the linker folds identical functions together at `--release` - so
        // both `as *const c_void` casts yield the *same* address, the swizzle
        // becomes unobservable, and this test fails without anything being wrong
        // with the code it is about. `black_box` of a distinct value is the
        // cheapest thing the folder cannot see through.
        unsafe extern "C" fn original() {
            std::hint::black_box(0u8);
        }
        unsafe extern "C" fn replacement() {
            std::hint::black_box(1u8);
        }

        // SAFETY: NUL-terminated literals, and a class this test allocates and
        // registers itself, so the method being replaced is its own.
        unsafe {
            let cls = host::objc_allocateClassPair(
                host::objc_getClass(c"NSObject".as_ptr()),
                c"RosemarySwizzleProbe".as_ptr(),
                0,
            );
            assert!(!cls.is_null());
            let sel = host::sel_registerName(c"probe".as_ptr());
            assert!(host::class_addMethod(
                cls,
                sel,
                original as *const std::ffi::c_void,
                c"v16@0:8".as_ptr(),
            ));
            host::objc_registerClassPair(cls);

            let before = host::class_getMethodImplementation(cls, sel);
            let method = host::class_getInstanceMethod(cls, sel);
            host::method_setImplementation(method, replacement as *const std::ffi::c_void);
            assert_ne!(before, host::class_getMethodImplementation(cls, sel));
        }
    }
}
