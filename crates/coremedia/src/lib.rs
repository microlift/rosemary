#![allow(clippy::missing_safety_doc)]

pub mod block_buffer;
pub mod format_description;
mod h264;
pub mod sample_buffer;
mod time;

pub use time::{CMTime, CMTimeGetSeconds, CMTimeMake};

/// The extension keys and values, as the constant strings a guest imports.
pub fn data_symbols() -> Vec<(String, u64)> {
    corefoundation::bootstrap();
    let mut values = corefoundation::base::string_globals(format_description::KEYS);
    values.extend(corefoundation::base::string_globals(&[
        ("_kCMSampleAttachmentKey_NotSync", "NotSync"),
        ("_kCMSampleAttachmentKey_PartialSync", "PartialSync"),
        ("_kCMSampleAttachmentKey_DependsOnOthers", "DependsOnOthers"),
        (
            "_kCMSampleAttachmentKey_IsDependedOnByOthers",
            "IsDependedOnByOthers",
        ),
        (
            "_kCMSampleAttachmentKey_DisplayImmediately",
            "DisplayImmediately",
        ),
    ]));
    values
}

exports::exports! {
    groups {
        format_description,
        block_buffer,
        sample_buffer,
        time,
    }
}
