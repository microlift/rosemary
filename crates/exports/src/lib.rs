#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub mod decl;
mod sink;

/// The bare C names a module answers.
///
/// Named so that [`exports!`] can spell it without the calling crate needing an
/// `alloc` of its own in scope.
pub type Names = Vec<&'static str>;

pub use sink::{Denying, NonNull, Recording};

/// Where a module's symbols go.
///
/// Every published symbol passes through one of these two calls, so a policy
/// that has to see all of them - refusing a null address, `ROSEMARY_WITHHOLD` -
/// is a wrapper here rather than a copy of `define_fn` in each crate that has
/// one.
///
/// Object-safe on purpose: a `fn(&mut dyn Exports)` is what lets a module be a
/// row in a table instead of a hand-written call.
pub trait Exports {
    /// `symbol` is the Mach-O spelling, leading underscore included.
    fn function(&mut self, symbol: &str, address: u64);
    fn data(&mut self, symbol: &str, address: u64);
    /// `symbol` exists and is null on purpose, as an absolute-zero export is.
    ///
    /// Its own call so that a sink can tell it from a `data` whose address came
    /// back zero by mistake, which [`NonNull`] refuses.
    fn null(&mut self, symbol: &str);
}

/// One module's answers, as a value.
///
/// The three used to be separate declarations that had to agree by hand. A row
/// is what makes them agree by construction, and what lets the crates above
/// hold their modules in a list rather than in a hand-ordered chain of calls
/// that a second hand-ordered chain of calls has to mirror.
#[derive(Clone, Copy)]
pub struct Module {
    /// The declaring module's path, for a test's failure message.
    pub path: &'static str,
    /// Every bare C name [`Module::symbol`] answers - no leading underscore,
    /// the spelling a `dlsym` uses.
    ///
    /// A function rather than a slice because a group's names are its members'
    /// and cannot be one `const`, and because a `cfg` on an entry is a
    /// statement here rather than an element of an array literal. Nothing on a
    /// hot path asks.
    pub names: fn() -> Names,
    pub symbol: fn(&str) -> Option<u64>,
    /// Every data symbol published as null on purpose, Mach-O spelling.
    pub null_data: fn() -> Names,
    pub provide: fn(&mut dyn Exports),
}

impl Module {
    /// Answer with the first module that knows `name`.
    ///
    /// The lookup half of a table. Its order is the order of the list, which is
    /// the same order [`Module::provide_all`] publishes in - one list, so the
    /// two cannot diverge.
    pub fn resolve(modules: &[Module], name: &str) -> Option<u64> {
        modules.iter().find_map(|m| (m.symbol)(name))
    }

    /// Publish every module in `modules`.
    pub fn provide_all(modules: &[Module], out: &mut dyn Exports) {
        for m in modules {
            (m.provide)(out);
        }
    }

    /// Every name any of `modules` answers.
    pub fn all_names(modules: &[Module]) -> Names {
        modules.iter().flat_map(|m| (m.names)()).collect()
    }

    /// Every intentionally null data symbol any of `modules` publishes.
    pub fn all_null_data(modules: &[Module]) -> Names {
        modules.iter().flat_map(|m| (m.null_data)()).collect()
    }
}

/// The address of a function, as the loader wants it.
///
/// One cast, rather than the copy of `macro_rules! at` that eight modules had
/// and a ninth spelled out by hand.
#[doc(hidden)]
pub fn address(f: *const ()) -> u64 {
    f as usize as u64
}

/// Declare what a module publishes.
///
/// Five optional sections, each written once:
///
/// ```ignore
/// exports::exports! {
///     functions {
///         "uuid_clear" => clear,
///         /// All three generate a version-4 UUID; see the module note.
///         "uuid_generate" | "uuid_generate_random" | "uuid_generate_time" => generate_random,
///         #[cfg(not(target_os = "macos"))]
///         "uuid_parse" => parse,
///     }
///     data {
///         "_NSApp" => ns_app_cell(),
///     }
///     null_data {
///         "__objc_empty_vtable",
///     }
///     groups { pasteboard_manager, coretext, cgcontext }
/// }
/// ```
///
/// A `functions` name is the bare C spelling and the Mach-O `_` is added here,
/// so the two come from one token and no module allocates to publish. A `data`
/// name is the Mach-O spelling verbatim: those are not all C identifiers -
/// `_OBJC_CLASS_$_NSView` is not - and there is no bare form to derive one from.
///
/// A `null_data` entry is a data symbol that exists and is zero on purpose,
/// like Apple's absolute-zero exports, in the Mach-O spelling. It is published
/// through [`Exports::null`] rather than as zero `data`, so it binds without
/// being mistaken for an address that failed to resolve, and `null_data()`
/// lists them.
///
/// A `constants` entry is data at a fixed address - a `static`'s, not a cell
/// built on demand - written in the bare spelling. Being fixed is what lets
/// `symbol()` answer it as well, so a `dlsym` of `in6addr_any` gets the address
/// the loader bound. Ordinary `data` is left out of `symbol()`: not every such
/// expression can be evaluated twice and give the same answer.
///
/// Generated: `MEMBERS`, `names()`, `symbol()`, `null_data()`, `provide()` and
/// `MODULE`. Data
/// entries are deliberately absent from `names()` and `symbol()`, which are the
/// function lookup a `dlsym` uses; publishing them is `provide()`'s job.
#[macro_export]
macro_rules! exports {
    (
        $(functions {
            $($(#[$fmeta:meta])* $fname:literal $(| $falias:literal)* => $ftarget:expr),* $(,)?
        })?
        $(data {
            $($(#[$dmeta:meta])* $dname:literal => $dvalue:expr),* $(,)?
        })?
        $(null_data {
            $($(#[$nmeta:meta])* $nname:literal),* $(,)?
        })?
        $(constants {
            $($(#[$cmeta:meta])* $cname:literal => $cvalue:expr),* $(,)?
        })?
        $(groups {
            $($(#[$gmeta:meta])* $gmodule:ident $(:: $gtail:ident)*),* $(,)?
        })?
    ) => {
        /// The modules this one answers for, beyond its own entries.
        pub const MEMBERS: &[$crate::Module] = &[
            $($($(#[$gmeta])* $gmodule $(:: $gtail)* ::MODULE,)*)?
        ];

        /// Every bare C name [`symbol`] answers, this module's and its members'.
        #[allow(unused_mut)]
        pub fn names() -> $crate::Names {
            let mut out = $crate::Names::new();
            $($(
                $(#[$fmeta])*
                #[allow(unused_doc_comments)]
                {
                    out.push($fname);
                    $(out.push($falias);)*
                }
            )*)?
            $($(
                $(#[$cmeta])*
                #[allow(unused_doc_comments)]
                out.push($cname);
            )*)?
            out.extend($crate::Module::all_names(MEMBERS));
            out
        }

        /// The address a guest asking for the bare C name `name` should get.
        #[allow(unused_variables, unused_doc_comments)]
        pub fn symbol(name: &str) -> Option<u64> {
            match name {
                $($(
                    $(#[$fmeta])*
                    $fname $(| $falias)* => {
                        return Some($crate::address($ftarget as *const ()));
                    }
                )*)?
                $($(
                    $(#[$cmeta])*
                    $cname => return Some($cvalue),
                )*)?
                _ => {}
            }
            $crate::Module::resolve(MEMBERS, name)
        }

        /// Every data symbol this module and its members publish as null.
        #[allow(unused_mut)]
        pub fn null_data() -> $crate::Names {
            let mut out = $crate::Names::new();
            $($(
                $(#[$nmeta])*
                #[allow(unused_doc_comments)]
                out.push($nname);
            )*)?
            out.extend($crate::Module::all_null_data(MEMBERS));
            out
        }

        /// Publish everything this module defines into `out`.
        #[allow(unused_variables)]
        pub fn provide(out: &mut dyn $crate::Exports) {
            $($(
                $(#[$fmeta])*
                #[allow(unused_doc_comments)]
                {
                    let address = $crate::address($ftarget as *const ());
                    out.function(concat!("_", $fname), address);
                    $(out.function(concat!("_", $falias), address);)*
                }
            )*)?
            $($(
                $(#[$dmeta])*
                #[allow(unused_doc_comments)]
                out.data($dname, $dvalue);
            )*)?
            $($(
                $(#[$nmeta])*
                #[allow(unused_doc_comments)]
                out.null($nname);
            )*)?
            $($(
                $(#[$cmeta])*
                #[allow(unused_doc_comments)]
                out.data(concat!("_", $cname), $cvalue);
            )*)?
            $crate::Module::provide_all(MEMBERS, out);
        }

        /// This module as a row, for the table above it.
        pub const MODULE: $crate::Module = $crate::Module {
            path: module_path!(),
            names,
            symbol,
            null_data,
            provide,
        };
    };
}

/// Declare a module that only gathers others.
///
/// The short spelling of an [`exports!`] with nothing but a `groups` section,
/// which is what every aggregator in the tree is.
#[macro_export]
macro_rules! group {
    ($($(#[$meta:meta])* $module:ident $(:: $tail:ident)*),* $(,)?) => {
        $crate::exports! {
            groups { $($(#[$meta])* $module $(:: $tail)*),* }
        }
    };
}

#[cfg(test)]
mod tests;
