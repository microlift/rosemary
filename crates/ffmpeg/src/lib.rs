//! FFmpeg 8 (libavcodec 62 / libavutil 60), `dlopen`ed at run time and shared
//! by every backend in this tree that decodes through it: `mediabackend`'s
//! hardware H.264 and `coreaudio`'s software AAC. One loader, one set of
//! structure offsets, checked once against the installed headers rather than
//! copied and drifting apart.
//!
//! Nothing here is FFmpeg-specific knowledge about *video* or *audio* -
//! that belongs to the crate that calls it, which knows what it is decoding.

pub mod abi;
pub mod api;
