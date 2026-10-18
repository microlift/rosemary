#![allow(
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    non_snake_case
)]

pub mod collator;
pub mod events;
pub mod files;
pub mod icons;
pub mod process;
pub mod resources;
pub mod uti;

exports::group! {
    collator,
    events,
    files,
    icons,
    process,
    resources,
    uti,
}
