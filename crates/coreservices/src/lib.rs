#![allow(
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    non_snake_case
)]

pub mod collator;
pub mod datetime;
pub mod events;
pub mod files;
pub mod icons;
pub mod memory;
pub mod metadata;
pub mod osutils;
pub mod process;
pub mod resources;
pub mod textencoding;
pub mod uti;
pub mod uttype;
pub mod windows;

exports::group! {
    collator,
    datetime,
    events,
    files,
    icons,
    memory,
    metadata,
    osutils,
    process,
    resources,
    textencoding,
    uti,
    windows,
}

/// The `kMDItem*` globals; see `metadata::ATTRIBUTE_NAMES`.
pub fn data_symbols() -> Vec<(String, u64)> {
    metadata::data_symbols()
}
