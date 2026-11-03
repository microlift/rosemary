# Media backend foundation

Host-neutral compressed-video decoding interface for Rosemary. This crate is
independent of VideoToolbox, CoreMedia, CoreVideo, Metal, and the old FFmpeg
backend. It introduces an **optional** GStreamer H.264 proof with CPU-mappable
NV12, I420 and BGRA outputs.

## Boundary

- `VideoDecoder::submit` accepts a compressed access unit with a unique `id`.
  The identifier is not a presentation timestamp; repeated PTS is valid.
- `VideoDecoder::pull` returns a host-owned decoded frame preserving input
  identity across H.264 B-frame reordering. A sample that will never produce a
  frame of its own (discarded as corrupt or pre-recovery, or folded into
  another picture) is reported once by `VideoDecoder::take_dropped`, so every
  accepted id is eventually accounted for. A decoded frame whose metadata is
  unusable is reported the same way with its error, without ending the
  session; only losing a frame's identity is fatal. `VideoFrame::with_bytes` maps the
  original GStreamer sample without an intermediate per-plane owned `Vec`.
- Frame layout carries validated strides/offsets, per-plane mapped spans,
  pixel format, optional crop, sample aspect ratio, negotiated chroma siting
  (JPEG/MPEG-2/DV/other/unknown), per-buffer field order, repeat-first/
  one-field flags, and color information. The public colorimetry enums preserve
  the decoded BT.2020 matrix/primaries, SMPTE ST 2084 (PQ) and ARIB STD-B67
  (HLG) characteristics; unrecognized numeric values remain distinguishable
  from genuinely unspecified metadata. Progressive, interleaved and mixed
  streams are supported, with mixed streams reported per frame; Fields and
  alternate-field formats fail explicitly until a plane contract exists.
  Unknown color range is distinct from limited range. Signed PTS/DTS are
  expressed as nanoseconds, independently of sample IDs; durations remain
  unsigned. The GStreamer backend applies a reversible +2^62 ns epoch bias
  because GstClockTime is unsigned; values earlier than -2^62 ns are
  explicitly rejected rather than clamped. Decoded PTS/DTS describe actual
  output on the caller's signed media timeline, which may be reordered or
  omitted by libav. If a compressed sample has neither source PTS nor DTS,
  GStreamer's inferred unsigned output timestamps are not misinterpreted as
  epoch-biased media times: the frame exposes absent PTS/DTS instead. This
  decision is made per frame, including streams mixing timestamped and
  untimestamped access units, and preserves valid input timestamps at -2^62 ns.
  Original compressed source PTS/DTS remain available separately through
  VideoFrame source timing methods and are preserved using per-buffer
  timestamp metadata, without retaining an unbounded per-ID lookup table.
- AVCC configuration records use independent 16-bit SPS/PPS lengths and
  validated chroma/bit-depth fields for every profile whose SPS carries them
  (High, High 10, High 4:2:2, High 4:4:4 and the scalable/multiview profiles).
  Unsupported AVC profiles and SPS extension NALs fail explicitly. The
  sample NAL-prefix width is validated separately (1, 2, or 4 bytes). The
  ordinary AVCC path validates every length-prefixed NAL without conversion
  or an extra allocation; truncated prefixes/payloads, forbidden header bits,
  and undefined/reserved NAL types fail recoverably as `InvalidInput` before
  reaching the asynchronous decoder. This is structural validation, not a
  full H.264 bitstream syntax or decoder-capability check. Unusually large
  initial parameter sets use a bounds-checked Annex-B fallback to avoid a
  GStreamer codec-data parsing limitation while preserving sample identities
  and backpressure.
- AVCC input accepts SPS/PPS configuration and compatible live PPS changes.
  A changed PPS is carried in-band with the next accepted access unit rather
  than resetting the H.264 reference state through a caps update. If a changed
  PPS will not fit the sample NAL-prefix width in an ordinary AVCC session,
  the update is rejected without altering the existing session. The Annex-B
  fallback accepts longer in-band PPS updates. Neither path restricts the
  initial avcC record's 16-bit parameter-set lengths. Annex-B access-unit
  input is supported. PPS injection preserves a leading AUD in either input
  format. Aggregate SPS/PPS initialization and reconfiguration injection
  sizes are checked before decoder configuration state is committed. The
  Annex-B fallback reserves at least five bytes for a minimal first NAL
  after its initial parameter-set injection. Access units larger than 8 MiB
  are rejected; within that bound, queue backpressure is recoverable: drain output
  and retry the **same** unconsumed input, rather than dropping frames. AVCC
  fallback submissions validate the whole NAL chain and calculate the expanded
  byte count before testing queue saturation; no parameter-set injection or
  Annex-B conversion allocation occurs on a backpressured submission.
- End-of-stream is terminal and distinct from a temporarily empty output
  queue. `is_drained()` returns `Result<bool, Error>` and observes pending
  fatal bus errors before declaring successful completion. No Apple framework
  API is implemented in this crate.

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
PTS, pinned exact presentation-order frame IDs alongside a 60-picture
three-GOP pixel reference, colorimetry, range,
NV12/I420/BGRA lifetime, same-SPS PPS reconfiguration, 256-frame bounded queue
backpressure, terminal EOS, missing-IDR recovery, immediate single-IDR output
without EOS, retained fatal bus errors including after EOS, decoded timing,
BT.2020/PQ and BT.2020/HLG stream metadata, malformed or undefined
AVCC NAL headers and direct NAL-chain recovery, below/above 8 MiB aggregate SPS/PPS boundaries with atomic rejected
reconfiguration, anamorphic/interlaced metadata, per-buffer field flags, mixed interlace, explicit
field-separated interlace-mode rejection, MPEG-2 versus JPEG chroma siting, negative source DTS
and decoded signed PTS through B-frame reordering, one-byte AVCC prefixes with
a 425-byte legal High10 PPS and profile-specific avcC extensions, an AUD-less
10-picture stream, and truncated input. A 20-picture changed-PPS mid-GOP regression matches independent FFmpeg
luma hashes, including reference-dependent P frames after reconfiguration,
with the original AUD NAL units retained.
The example uses GStreamer's H.264 parser to find access-unit boundaries,
rather than assuming Access Unit Delimiters are mandatory. The libav element is
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
Never expose that drain as VideoToolbox-compatible. Frame field order comes from
negotiated caps when they fix it for the stream, otherwise from each decoded
buffer's flags. For the pinned interlaced fixture both GStreamer's caps and
FFprobe's per-frame top_field_first=0 say bottom field first, even though the
file name and FFprobe's stream summary (field_order=tt) suggest otherwise.
Multi-meta field buffers remain unsupported.
Signed media times are represented to nanosecond precision; fractional
CoreMedia time scales requiring subnanosecond precision must be converted by
an integrating adapter rather than silently rounded by this decoder.
Auto/frame threading can improve throughput but increases delayed output and RSS. See later scoped
integration work for CPU CoreVideo handoff and bundle packaging.
