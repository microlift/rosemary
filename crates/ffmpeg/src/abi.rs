//! The FFmpeg structure fields this tree touches, by offset, and the
//! constants it needs. They are libavcodec 62 / libavutil 60's (FFmpeg 8);
//! [`super::api`] refuses any other major version rather than guess, and the
//! test below checks the numbers against the installed headers.
//!
//! Shared by `mediabackend` (H.264 through a hardware device context) and
//! `coreaudio` (AAC through the software decoder): one set of offsets into
//! `AVCodecContext`, `AVPacket` and `AVFrame`, derived once.

use std::ffi::c_void;

/// The library majors these offsets are for.
pub const AVCODEC_MAJOR: u32 = 62;
pub const AVUTIL_MAJOR: u32 = 60;

/// `AVCodecContext`.
pub mod context {
    pub const OPAQUE: usize = 48;
    pub const EXTRADATA: usize = 72;
    pub const EXTRADATA_SIZE: usize = 80;
    pub const GET_FORMAT: usize = 192;
    /// `AVCodecContext.sample_rate`, audio only.
    pub const SAMPLE_RATE: usize = 344;
    /// `AVCodecContext.ch_layout`, an `AVChannelLayout` (see [`super::ch_layout`]).
    pub const CH_LAYOUT: usize = 352;
    pub const HW_DEVICE_CTX: usize = 560;
}

/// `AVChannelLayout`, offsets from the start of the structure (so from
/// `context::CH_LAYOUT` for the one embedded in `AVCodecContext`).
///
/// It is `{ enum AVChannelOrder order; int nb_channels; union { uint64_t
/// mask; AVChannelCustom *map; } u; void *opaque; }` - 24 bytes, not 16: the
/// trailing `opaque` is easy to miss reading the header once.
pub mod ch_layout {
    pub const NB_CHANNELS: usize = 4;
}

/// `AVPacket`.
pub mod packet {
    pub const PTS: usize = 8;
    pub const DATA: usize = 24;
    pub const SIZE: usize = 32;
}

/// `AVFrame`.
pub mod frame {
    pub const DATA: usize = 0;
    pub const LINESIZE: usize = 64;
    pub const EXTENDED_DATA: usize = 96;
    pub const WIDTH: usize = 104;
    pub const HEIGHT: usize = 108;
    /// Both `enum AVPixelFormat` (video) and `enum AVSampleFormat` (audio):
    /// the same field, read as whichever the stream is.
    pub const FORMAT: usize = 116;
    /// Audio only: samples per channel in this frame.
    pub const NB_SAMPLES: usize = 112;
    pub const PTS: usize = 136;
    /// Audio only.
    pub const SAMPLE_RATE: usize = 180;
    pub const COLOR_RANGE: usize = 280;
    pub const COLOR_PRIMARIES: usize = 284;
    pub const COLOR_TRC: usize = 288;
    pub const COLORSPACE: usize = 292;
    pub const CROP_TOP: usize = 344;
    pub const CROP_LEFT: usize = 360;
    /// Audio only, an `AVChannelLayout` (see [`super::ch_layout`]).
    pub const CH_LAYOUT: usize = 384;
}

/// `AVCodecHWConfig`.
pub mod hw_config {
    pub const PIX_FMT: usize = 0;
    pub const METHODS: usize = 4;
    pub const DEVICE_TYPE: usize = 8;
}

/// `AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX`.
pub const METHOD_HW_DEVICE_CTX: i32 = 1;
/// `AV_PIX_FMT_NONE`.
pub const PIX_FMT_NONE: i32 = -1;
/// `AV_HWDEVICE_TYPE_NONE`.
pub const HWDEVICE_TYPE_NONE: i32 = 0;
/// `AV_SAMPLE_FMT_NONE`.
pub const SAMPLE_FMT_NONE: i32 = -1;
/// `AVERROR(EAGAIN)`, `AVERROR_EOF` and `AVERROR_INVALIDDATA`.
pub const EAGAIN: i32 = -11;
pub const EOF: i32 = -541478725;
pub const INVALIDDATA: i32 = -1094995529;
/// `AVCOL_RANGE_MPEG` and `AVCOL_RANGE_JPEG`.
pub const RANGE_MPEG: i32 = 1;
pub const RANGE_JPEG: i32 = 2;
/// `AV_LOG_ERROR`.
pub const LOG_ERROR: i32 = 16;
/// `AV_INPUT_BUFFER_PADDING_SIZE`: a compressed buffer FFmpeg reads (such as
/// `AVCodecContext.extradata`) must be allocated this many bytes larger than
/// its real size, zeroed, so the decoder's bit reader can overrun into the
/// padding instead of past the allocation.
pub const INPUT_BUFFER_PADDING_SIZE: usize = 64;

/// The field of type `T` at `offset` in the structure at `base`.
///
/// # Safety
///
/// `base` must point to a live structure of the kind the offset is for.
pub unsafe fn read<T: Copy>(base: *const c_void, offset: usize) -> T {
    // SAFETY: as the caller promises.
    unsafe { base.byte_add(offset).cast::<T>().read_unaligned() }
}

/// Write the field of type `T` at `offset` in the structure at `base`.
///
/// # Safety
///
/// As [`read`], and `T` must be the field's type.
pub unsafe fn write<T>(base: *mut c_void, offset: usize, value: T) {
    // SAFETY: as the caller promises.
    unsafe { base.byte_add(offset).cast::<T>().write_unaligned(value) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every number above, as the headers state it, in the order the C
    /// program prints them.
    fn expected() -> Vec<(&'static str, i64)> {
        vec![
            ("AVCodecContext.opaque", context::OPAQUE as i64),
            ("AVCodecContext.extradata", context::EXTRADATA as i64),
            (
                "AVCodecContext.extradata_size",
                context::EXTRADATA_SIZE as i64,
            ),
            ("AVCodecContext.get_format", context::GET_FORMAT as i64),
            ("AVCodecContext.sample_rate", context::SAMPLE_RATE as i64),
            ("AVCodecContext.ch_layout", context::CH_LAYOUT as i64),
            ("AVChannelLayout.nb_channels", ch_layout::NB_CHANNELS as i64),
            (
                "AVCodecContext.hw_device_ctx",
                context::HW_DEVICE_CTX as i64,
            ),
            ("AVPacket.pts", packet::PTS as i64),
            ("AVPacket.data", packet::DATA as i64),
            ("AVPacket.size", packet::SIZE as i64),
            ("AVFrame.data", frame::DATA as i64),
            ("AVFrame.linesize", frame::LINESIZE as i64),
            ("AVFrame.extended_data", frame::EXTENDED_DATA as i64),
            ("AVFrame.width", frame::WIDTH as i64),
            ("AVFrame.height", frame::HEIGHT as i64),
            ("AVFrame.nb_samples", frame::NB_SAMPLES as i64),
            ("AVFrame.format", frame::FORMAT as i64),
            ("AVFrame.pts", frame::PTS as i64),
            ("AVFrame.sample_rate", frame::SAMPLE_RATE as i64),
            ("AVFrame.ch_layout", frame::CH_LAYOUT as i64),
            ("AVFrame.color_range", frame::COLOR_RANGE as i64),
            ("AVFrame.color_primaries", frame::COLOR_PRIMARIES as i64),
            ("AVFrame.color_trc", frame::COLOR_TRC as i64),
            ("AVFrame.colorspace", frame::COLORSPACE as i64),
            ("AVFrame.crop_top", frame::CROP_TOP as i64),
            ("AVFrame.crop_left", frame::CROP_LEFT as i64),
            ("AVCodecHWConfig.pix_fmt", hw_config::PIX_FMT as i64),
            ("AVCodecHWConfig.methods", hw_config::METHODS as i64),
            ("AVCodecHWConfig.device_type", hw_config::DEVICE_TYPE as i64),
            ("METHOD_HW_DEVICE_CTX", METHOD_HW_DEVICE_CTX as i64),
            ("PIX_FMT_NONE", PIX_FMT_NONE as i64),
            ("HWDEVICE_TYPE_NONE", HWDEVICE_TYPE_NONE as i64),
            ("SAMPLE_FMT_NONE", SAMPLE_FMT_NONE as i64),
            ("EAGAIN", EAGAIN as i64),
            ("EOF", EOF as i64),
            ("INVALIDDATA", INVALIDDATA as i64),
            ("RANGE_MPEG", RANGE_MPEG as i64),
            ("RANGE_JPEG", RANGE_JPEG as i64),
            ("LOG_ERROR", LOG_ERROR as i64),
            (
                "INPUT_BUFFER_PADDING_SIZE",
                INPUT_BUFFER_PADDING_SIZE as i64,
            ),
            ("AVCODEC_MAJOR", AVCODEC_MAJOR as i64),
            ("AVUTIL_MAJOR", AVUTIL_MAJOR as i64),
        ]
    }

    const PROGRAM: &str = r#"
#include <stddef.h>
#include <stdio.h>
#include <libavcodec/avcodec.h>
#include <libavutil/frame.h>
#include <libavutil/channel_layout.h>
#include <libavutil/hwcontext.h>
#include <libavutil/samplefmt.h>
#include <libavutil/log.h>
#define O(t, f) printf(#t "." #f " %lld\n", (long long)offsetof(t, f))
#define V(n, v) printf(n " %lld\n", (long long)(v))
int main(void) {
    O(AVCodecContext, opaque); O(AVCodecContext, extradata); O(AVCodecContext, extradata_size);
    O(AVCodecContext, get_format); O(AVCodecContext, sample_rate); O(AVCodecContext, ch_layout);
    O(AVChannelLayout, nb_channels);
    O(AVCodecContext, hw_device_ctx);
    O(AVPacket, pts); O(AVPacket, data); O(AVPacket, size);
    O(AVFrame, data); O(AVFrame, linesize); O(AVFrame, extended_data);
    O(AVFrame, width); O(AVFrame, height); O(AVFrame, nb_samples);
    O(AVFrame, format); O(AVFrame, pts); O(AVFrame, sample_rate); O(AVFrame, ch_layout);
    O(AVFrame, color_range); O(AVFrame, color_primaries);
    O(AVFrame, color_trc); O(AVFrame, colorspace); O(AVFrame, crop_top); O(AVFrame, crop_left);
    O(AVCodecHWConfig, pix_fmt); O(AVCodecHWConfig, methods); O(AVCodecHWConfig, device_type);
    V("METHOD_HW_DEVICE_CTX", AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX);
    V("PIX_FMT_NONE", AV_PIX_FMT_NONE);
    V("HWDEVICE_TYPE_NONE", AV_HWDEVICE_TYPE_NONE);
    V("SAMPLE_FMT_NONE", AV_SAMPLE_FMT_NONE);
    V("EAGAIN", AVERROR(EAGAIN)); V("EOF", AVERROR_EOF); V("INVALIDDATA", AVERROR_INVALIDDATA);
    V("RANGE_MPEG", AVCOL_RANGE_MPEG); V("RANGE_JPEG", AVCOL_RANGE_JPEG);
    V("LOG_ERROR", AV_LOG_ERROR);
    V("INPUT_BUFFER_PADDING_SIZE", AV_INPUT_BUFFER_PADDING_SIZE);
    V("AVCODEC_MAJOR", LIBAVCODEC_VERSION_MAJOR); V("AVUTIL_MAJOR", LIBAVUTIL_VERSION_MAJOR);
    return 0;
}
"#;

    /// The compiler flags that find the headers: `pkg-config`'s, or Fedora's
    /// and RPM Fusion's directory.
    fn include_flags() -> Vec<String> {
        let from_pkg_config = std::process::Command::new("pkg-config")
            .args(["--cflags", "libavcodec", "libavutil"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            });
        from_pkg_config.unwrap_or_else(|| vec!["-I/usr/include/ffmpeg".to_string()])
    }

    #[test]
    fn offsets_match_installed_headers() {
        const SKIP: &str = "skipped: no FFmpeg headers; install ffmpeg-devel to check the offsets";
        let dir = std::env::temp_dir().join(format!("rosemary-ffmpeg-abi-{}", std::process::id()));
        if std::fs::create_dir_all(&dir).is_err() {
            eprintln!("{SKIP}");
            return;
        }
        let source = dir.join("offsets.c");
        let binary = dir.join("offsets");
        std::fs::write(&source, PROGRAM).unwrap();
        let built = std::process::Command::new("cc")
            .args(include_flags())
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output();
        let built = match built {
            Ok(out) if out.status.success() => true,
            Ok(out) => {
                let errors = String::from_utf8_lossy(&out.stderr);
                if errors.contains("No such file") || errors.contains("not found") {
                    false
                } else {
                    panic!("the offset program did not build:\n{errors}");
                }
            }
            Err(_) => false,
        };
        if !built {
            let _ = std::fs::remove_dir_all(&dir);
            eprintln!("{SKIP}");
            return;
        }
        let output = std::process::Command::new(&binary).output().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let text = String::from_utf8(output.stdout).unwrap();
        let actual: Vec<(String, i64)> = text
            .lines()
            .map(|line| {
                let (name, value) = line.rsplit_once(' ').unwrap();
                (name.to_string(), value.parse().unwrap())
            })
            .collect();
        let expected = expected();
        assert_eq!(actual.len(), expected.len(), "{text}");
        for ((name, value), (want_name, want)) in actual.iter().zip(&expected) {
            assert_eq!(name, want_name);
            assert_eq!(value, want, "{name}");
        }
    }
}
