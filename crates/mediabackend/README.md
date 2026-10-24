# Media backend foundation

Host-neutral compressed-video decoding interface for Rosemary. This crate is
independent of VideoToolbox, CoreMedia, CoreVideo, Metal, and the old FFmpeg
backend. It introduces an **optional** GStreamer H.264 proof with CPU-mappable
NV12, I420 and BGRA outputs.

## Boundary

- `VideoDecoder::submit` accepts a compressed access unit with a unique `id`.
  The identifier is not a presentation timestamp; repeated PTS is valid.
- `VideoDecoder::pull` returns a host-owned decoded frame preserving input
  identity across H.264 B-frame reordering. `VideoFrame::with_bytes` maps the
  original GStreamer sample without an intermediate per-plane owned `Vec`.
- Frame layout carries validated strides/offsets, per-plane mapped spans,
  pixel format, optional crop, and color information. Unknown color range is
  distinct from limited range. Frame timing comes from decoded buffers; output
  DTS may be absent even when the compressed input provided it.
- AVCC input accepts SPS/PPS configuration and compatible live PPS changes;
  Annex-B access-unit input is supported. Access units larger than 8 MiB are
  rejected; within that bound, queue backpressure is recoverable: drain output
  and retry the **same** unconsumed input, rather than dropping frames.
- End-of-stream is terminal and distinct from a temporarily empty output
  queue. No Apple framework API is implemented in this crate.

## Build and test

Normal Rosemary builds do **not** link GStreamer. The opt-in `gstreamer`
feature requires GStreamer >= 1.28 development libraries, plus runtime plugins
`coreelements`, `app`, `videoparsersbad`, `libav`, `videoconvertscale`.

```bash
cargo test -p mediabackend
cargo test -p mediabackend --features gstreamer
cargo run --release -p mediabackend --features gstreamer --example h264_stream -- clip.h264 1
```

The regression fixtures are stored under this crate, so no other feature PR is
required to test decoding. Tests cover B-frame reference identity with repeated
PTS, a pinned 60-picture three-GOP pixel reference, colorimetry, range,
NV12/I420/BGRA lifetime, same-SPS PPS reconfiguration, 256-frame bounded queue
backpressure, terminal EOS, missing-IDR recovery, immediate single-IDR output
without EOS, retained fatal bus errors, decoded timing, and truncated input. The libav element is
configured with `output-corrupt=false` and `discard-corrupted-frames=true`:
its unmodified default emitted invalid pre-recovery pictures. The pinned
60-picture truncated fixture confirms it drops all 19 invalid pictures and
outputs the remaining 40 with the same pixel hashes as FFmpeg's normal output.
The multi-GOP luma checksum fixture is
`tests/fixtures/multi_gop_bframes_64x64.luma_fnv64`.

## Known limits

This crate alone is **not** VideoToolbox compatibility. Apple's synchronous
frame callback requirements, nonterminal delayed-frame waits, and GPU-backed
CoreVideo/Metal presentation require separate integration. A public GStreamer
still-frame/segment-done drain emits delayed pictures, but on the tested libav
backend damages subsequent H.264 predicted-picture content until next IDR.
Never expose that drain as VideoToolbox-compatible. Auto/frame threading can
improve throughput but increases delayed output and RSS. See later scoped
integration work for CPU CoreVideo handoff and bundle packaging.
