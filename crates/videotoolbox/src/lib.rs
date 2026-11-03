pub mod compression;
pub mod constants;
pub mod property;
pub mod types;

pub fn data_symbols() -> Vec<(String, u64)> {
    corefoundation::bootstrap();
    corefoundation::base::string_globals(compression::KEYS)
}

exports::exports! {
    groups {
        compression,
    }
}
