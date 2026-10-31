#![allow(clippy::missing_safety_doc)]

pub mod compression;
pub mod constants;
pub mod decompression;
pub mod property;
pub mod types;

pub fn data_symbols() -> Vec<(String, u64)> {
    corefoundation::bootstrap();
    let mut symbols = corefoundation::base::string_globals(compression::KEYS);
    symbols.extend(corefoundation::base::string_globals(decompression::KEYS));
    symbols
}

exports::exports! {
    groups {
        compression,
        decompression,
        property,
    }
}
