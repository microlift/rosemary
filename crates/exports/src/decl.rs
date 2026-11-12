//! C declarations of what a module exports.
//!
//! An export table says which Rust function answers a C name. These say what
//! that name *is* in C - its type, its header - so that the headers and link
//! stubs a Darwin program is built against can be generated rather than
//! written, and so that the declaration and the implementation are checked
//! against each other by the compiler instead of by review.
//!
//! Every C type is a marker implementing [`CType`], which ties its C spelling
//! to the Rust type an implementation uses for it. [`c_functions!`] then
//! requires each implementing function to coerce to the signature its
//! declaration spells, so a header cannot claim a type the code does not take.
//!
//! [`c_functions!`]: crate::c_functions

use core::ffi::c_char;
use core::marker::PhantomData;

/// A C type as a header spells it.
#[derive(Debug)]
pub enum Type {
    /// A type C itself provides, spelled as is.
    Builtin(&'static str),
    Named(&'static Named),
    Pointer {
        to: &'static Type,
        constant: bool,
    },
}

impl Type {
    /// The typedef this is, if it is one.
    pub fn named(&self) -> Option<&'static Named> {
        match self {
            Type::Named(named) => Some(named),
            _ => None,
        }
    }
}

/// A typedef, and the header that defines it.
#[derive(Debug)]
pub struct Named {
    pub name: &'static str,
    pub header: &'static str,
    pub definition: Definition,
}

#[derive(Debug)]
pub enum Definition {
    Alias(&'static Type),
    /// A pointer to a struct no header completes: `typedef const struct
    /// __CFString *CFStringRef`.
    Opaque {
        tag: &'static str,
        constant: bool,
    },
    Struct(&'static [Field]),
    /// A function pointer: CoreFoundation's callbacks are all typedefs, and a
    /// header that spelled them inline would not be the API programs name.
    Callback {
        returns: Option<&'static Type>,
        params: &'static [&'static Type],
    },
}

#[derive(Debug)]
pub struct Field {
    pub name: &'static str,
    pub ty: &'static Type,
}

#[derive(Debug)]
pub struct Param {
    pub name: &'static str,
    pub ty: &'static Type,
}

#[derive(Debug)]
pub struct Function {
    pub name: &'static str,
    /// The Mach-O spelling of `name`.
    pub symbol: &'static str,
    pub header: &'static str,
    pub returns: Option<&'static Type>,
    pub params: &'static [Param],
    /// The implementation, once coerced to the declared signature.
    pub address: *const (),
}

/// An exported variable. Every one a framework here exports is `const`.
#[derive(Debug)]
pub struct Variable {
    pub name: &'static str,
    pub header: &'static str,
    pub ty: &'static Type,
}

/// Named values of a typedef'd integer type, as an anonymous C `enum`.
#[derive(Debug)]
pub struct Enum {
    pub header: &'static str,
    pub ty: &'static Type,
    pub cases: &'static [(&'static str, i64)],
}

/// Header text no declaration can express: a macro, an inline function.
#[derive(Debug)]
pub struct Fragment {
    pub header: &'static str,
    pub text: &'static str,
}

/// Everything one framework declares.
#[derive(Debug)]
pub struct Api {
    /// The framework's name: `CoreFoundation`.
    pub framework: &'static str,
    pub types: &'static [&'static Type],
    pub functions: &'static [Function],
    pub variables: &'static [Variable],
    pub enums: &'static [Enum],
    pub fragments: &'static [Fragment],
}

/// A C type, and the Rust type an implementation receives it as.
pub trait CType {
    type Rust;
    const TYPE: &'static Type;
}

macro_rules! builtins {
    ($($marker:ident = $spelling:literal => $rust:ty;)*) => {
        $(
            #[doc = concat!("C `", $spelling, "`.")]
            pub enum $marker {}

            impl CType for $marker {
                type Rust = $rust;
                const TYPE: &'static Type = &Type::Builtin($spelling);
            }
        )*
    };
}

// `long` is `isize` rather than `i64` because that is what the framework crates
// already spell `CFIndex` as; every host Rosemary runs on is LP64, as Darwin
// is. `char` is the host's `c_char` for the same reason - it is one byte
// either way, which is all an argument of it needs to agree on.
builtins! {
    Void = "void" => core::ffi::c_void;
    Char = "char" => c_char;
    UChar = "unsigned char" => u8;
    Short = "short" => i16;
    UShort = "unsigned short" => u16;
    Int = "int" => i32;
    UInt = "unsigned int" => u32;
    Long = "long" => isize;
    ULong = "unsigned long" => usize;
    LongLong = "long long" => i64;
    ULongLong = "unsigned long long" => u64;
    Float = "float" => f32;
    Double = "double" => f64;
}

/// C `T *`.
pub struct Ptr<T>(PhantomData<T>);

/// C `const T *`.
pub struct ConstPtr<T>(PhantomData<T>);

impl<T: CType> CType for Ptr<T> {
    type Rust = *mut T::Rust;
    const TYPE: &'static Type = &Type::Pointer {
        to: T::TYPE,
        constant: false,
    };
}

impl<T: CType> CType for ConstPtr<T> {
    type Rust = *const T::Rust;
    const TYPE: &'static Type = &Type::Pointer {
        to: T::TYPE,
        constant: true,
    };
}

/// Declare typedefs, grouped by the header that defines them, and list them
/// all as `TYPES`.
///
/// ```ignore
/// exports::c_types! {
///     "CFBase.h" {
///         typedef CFIndex = Long;
///         opaque CFStringRef = const __CFString;
///         callback CFArrayReleaseCallBack = fn(CFAllocatorRef, ConstPtr<Void>);
///         struct CFArrayCallBacks = callbacks::ValueCallBacks {
///             version: CFIndex,
///             release: CFArrayReleaseCallBack,
///         }
///     }
/// }
/// ```
///
/// A `struct` names the Rust type that implements it, and the two must have
/// the same size and alignment.
#[macro_export]
macro_rules! c_types {
    ($($header:literal { $($body:tt)* })*) => {
        $($crate::c_types!(@items $header; $($body)*);)*

        /// Every typedef declared here.
        pub const TYPES: &[&'static $crate::decl::Type] =
            $crate::c_types!(@list []; $($($body)*)*);
    };

    (@items $header:literal;) => {};
    (@items $header:literal; typedef $name:ident = $target:ty; $($rest:tt)*) => {
        $crate::c_types!(@marker $name, <$target as $crate::decl::CType>::Rust,
            $header, $crate::decl::Definition::Alias(<$target as $crate::decl::CType>::TYPE));
        $crate::c_types!(@items $header; $($rest)*);
    };
    (@items $header:literal; opaque $name:ident = const $tag:ident; $($rest:tt)*) => {
        $crate::c_types!(@marker $name, *const core::ffi::c_void, $header,
            $crate::decl::Definition::Opaque { tag: stringify!($tag), constant: true });
        $crate::c_types!(@items $header; $($rest)*);
    };
    (@items $header:literal; opaque $name:ident = $tag:ident; $($rest:tt)*) => {
        // A handle is a handle: the framework crates take every one as a
        // `*const c_void`, mutable or not.
        $crate::c_types!(@marker $name, *const core::ffi::c_void, $header,
            $crate::decl::Definition::Opaque { tag: stringify!($tag), constant: false });
        $crate::c_types!(@items $header; $($rest)*);
    };
    (@items $header:literal;
        callback $name:ident = fn($($param:ty),* $(,)?) $(-> $ret:ty)?; $($rest:tt)*) => {
        $crate::c_types!(@marker $name,
            Option<unsafe extern "C" fn($(<$param as $crate::decl::CType>::Rust),*)
                $(-> <$ret as $crate::decl::CType>::Rust)?>,
            $header,
            $crate::decl::Definition::Callback {
                returns: $crate::c_types!(@returns $($ret)?),
                params: &[$(<$param as $crate::decl::CType>::TYPE),*],
            });
        $crate::c_types!(@items $header; $($rest)*);
    };
    (@items $header:literal;
        struct $name:ident = $rust:path { $($field:ident: $fty:ty),* $(,)? } $($rest:tt)*) => {
        $crate::c_types!(@marker $name, $rust, $header,
            $crate::decl::Definition::Struct(&[
                $($crate::decl::Field {
                    name: stringify!($field),
                    ty: <$fty as $crate::decl::CType>::TYPE,
                }),*
            ]));
        const _: () = {
            #[allow(non_snake_case, dead_code)]
            #[repr(C)]
            struct Declared {
                $($field: <$fty as $crate::decl::CType>::Rust),*
            }
            assert!(
                core::mem::size_of::<Declared>() == core::mem::size_of::<$rust>()
                    && core::mem::align_of::<Declared>() == core::mem::align_of::<$rust>(),
                concat!("`", stringify!($name), "` does not have its Rust type's layout"),
            );
        };
        $crate::c_types!(@items $header; $($rest)*);
    };

    (@marker $name:ident, $rust:ty, $header:literal, $definition:expr) => {
        #[allow(non_camel_case_types)]
        pub enum $name {}

        impl $crate::decl::CType for $name {
            type Rust = $rust;
            const TYPE: &'static $crate::decl::Type =
                &$crate::decl::Type::Named(&$crate::decl::Named {
                    name: stringify!($name),
                    header: $header,
                    definition: $definition,
                });
        }
    };

    (@returns) => { None };
    (@returns $ret:ty) => { Some(<$ret as $crate::decl::CType>::TYPE) };

    (@list [$($acc:tt)*];) => { &[$($acc)*] };
    (@list [$($acc:tt)*]; typedef $name:ident = $target:ty; $($rest:tt)*) => {
        $crate::c_types!(@list [$($acc)* <$name as $crate::decl::CType>::TYPE,]; $($rest)*)
    };
    (@list [$($acc:tt)*]; opaque $name:ident = const $tag:ident; $($rest:tt)*) => {
        $crate::c_types!(@list [$($acc)* <$name as $crate::decl::CType>::TYPE,]; $($rest)*)
    };
    (@list [$($acc:tt)*]; opaque $name:ident = $tag:ident; $($rest:tt)*) => {
        $crate::c_types!(@list [$($acc)* <$name as $crate::decl::CType>::TYPE,]; $($rest)*)
    };
    (@list [$($acc:tt)*]; callback $name:ident = fn($($param:ty),* $(,)?) $(-> $ret:ty)?;
        $($rest:tt)*) => {
        $crate::c_types!(@list [$($acc)* <$name as $crate::decl::CType>::TYPE,]; $($rest)*)
    };
    (@list [$($acc:tt)*]; struct $name:ident = $rust:path { $($fields:tt)* } $($rest:tt)*) => {
        $crate::c_types!(@list [$($acc)* <$name as $crate::decl::CType>::TYPE,]; $($rest)*)
    };
}

/// Declare functions, grouped by header, as `FUNCTIONS`, and export them.
///
/// Each names the function that implements it, which must coerce to the
/// signature the declaration spells: a declaration taking a `CFIndex` for a
/// function taking an `i32` does not compile.
///
/// The declaration is also the export row: this generates the same `MODULE`
/// an [`exports!`](crate::exports) does, for the module above to list in its
/// `groups`, so a declared function is never also listed in a `functions`
/// section that could name a different implementation.
#[macro_export]
macro_rules! c_functions {
    ($($header:literal {
        $(fn $name:ident($($param:ident: $pty:ty),* $(,)?) $(-> $ret:ty)? = $imp:path;)*
    })*) => {
        /// Every function declared here.
        pub const FUNCTIONS: &[$crate::decl::Function] = &[
            $($($crate::decl::Function {
                name: stringify!($name),
                symbol: concat!("_", stringify!($name)),
                header: $header,
                returns: $crate::c_types!(@returns $($ret)?),
                params: &[$($crate::decl::Param {
                    name: stringify!($param),
                    ty: <$pty as $crate::decl::CType>::TYPE,
                }),*],
                address: {
                    let imp: unsafe extern "C" fn($(<$pty as $crate::decl::CType>::Rust),*)
                        $(-> <$ret as $crate::decl::CType>::Rust)? = $imp;
                    imp as *const ()
                },
            },)*)*
        ];

        /// Every bare C name [`symbol`] answers.
        pub fn names() -> $crate::Names {
            FUNCTIONS.iter().map(|f| f.name).collect()
        }

        /// The address a guest asking for the bare C name `name` should get.
        pub fn symbol(name: &str) -> Option<u64> {
            FUNCTIONS
                .iter()
                .find(|f| f.name == name)
                .map(|f| $crate::address(f.address))
        }

        /// Publish every function declared here into `out`.
        pub fn provide(out: &mut dyn $crate::Exports) {
            for f in FUNCTIONS {
                out.function(f.symbol, $crate::address(f.address));
            }
        }

        /// This module as a row, for the table above it.
        pub const MODULE: $crate::Module = $crate::Module {
            path: module_path!(),
            names,
            symbol,
            null_data: $crate::Names::new,
            provide,
        };
    };
}

/// Declare exported variables, grouped by header, as `VARIABLES`.
#[macro_export]
macro_rules! c_variables {
    ($($header:literal { $($name:ident: $ty:ty;)* })*) => {
        /// Every variable declared here.
        pub const VARIABLES: &[$crate::decl::Variable] = &[
            $($($crate::decl::Variable {
                name: stringify!($name),
                header: $header,
                ty: <$ty as $crate::decl::CType>::TYPE,
            },)*)*
        ];
    };
}

/// Declare named values of integer typedefs, grouped by header, as `ENUMS`.
#[macro_export]
macro_rules! c_enums {
    ($($header:literal { $($ty:ident { $($case:ident = $value:expr),* $(,)? })* })*) => {
        /// Every enumeration declared here.
        pub const ENUMS: &[$crate::decl::Enum] = &[
            $($($crate::decl::Enum {
                header: $header,
                ty: <$ty as $crate::decl::CType>::TYPE,
                cases: &[$((stringify!($case), $value as i64)),*],
            },)*)*
        ];
    };
}
