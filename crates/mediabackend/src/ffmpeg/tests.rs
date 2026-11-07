use super::*;

/// The probe clip on `hardware`, pulled through the decoder interface: every
/// picture once, in display order, with the pixels it was made with.
fn decodes_the_probe_clip(hardware: &'static Hardware) {
    if let Err(why) = hardware.probe() {
        eprintln!(
            "skipped: {} cannot decode H.264 here: {why}",
            hardware.name()
        );
        return;
    }
    eprintln!("{}: {}", hardware.name(), hardware.description());
    let (sets, samples) = probe_samples();
    let sets: Vec<&[u8]> = sets.iter().map(Vec::as_slice).collect();
    let mut decoder = hardware.h264_avcc(&sets, 4).expect("a decoder");
    let mut frames = Vec::new();
    for (id, sample) in samples.iter().enumerate() {
        let display = PROBE_DISPLAY_ORDER[id] as i64;
        decoder
            .submit(AccessUnit {
                id: id as u64,
                bytes: sample,
                pts_ns: Some(display * 1000),
                dts_ns: Some(id as i64 * 1000),
                duration_ns: Some(1000),
            })
            .expect("a sample the decoder takes");
        while let Some(frame) = decoder.pull(Duration::ZERO).unwrap() {
            frames.push(frame);
        }
    }
    assert!(!decoder.is_drained().unwrap());
    decoder.end_stream().unwrap();
    while let Some(frame) = decoder.pull(Duration::ZERO).unwrap() {
        frames.push(frame);
    }
    assert!(decoder.is_drained().unwrap());
    assert!(decoder.take_dropped().is_none());
    let shown: Vec<i64> = frames.iter().map(|f| f.pts_ns().unwrap() / 1000).collect();
    assert_eq!(shown, [0, 1, 2, 3, 4, 5]);
    for (n, frame) in frames.iter().enumerate() {
        let coded = PROBE_DISPLAY_ORDER
            .iter()
            .position(|&d| d == n as u64)
            .unwrap();
        assert_eq!(frame.id(), coded as u64);
        assert_eq!(frame.source_dts_ns(), Some(coded as i64 * 1000));
        let layout = frame.layout();
        assert_eq!(
            (layout.width, layout.height, layout.format),
            (64, 64, PixelFormat::Nv12)
        );
        assert_eq!(layout.plane_byte_span(1), Some(32 * 64));
        let luma = 40 + 30 * n as u8;
        frame
            .with_bytes(|bytes, layout| {
                let near = |v: u8, want: u8| v.abs_diff(want) <= 2;
                let y = &bytes[..64 * 64];
                assert!(y.iter().all(|&v| near(v, luma)), "frame {n}: {:?}", &y[..8]);
                let uv = &bytes[layout.offsets[1]..];
                for row in 0..32 {
                    for column in 0..32 {
                        let at = row * layout.strides[1] + 2 * column;
                        let want = (
                            if column < 16 { 90 } else { 170 },
                            if row < 16 { 100 } else { 160 },
                        );
                        assert!(
                            near(uv[at], want.0) && near(uv[at + 1], want.1),
                            "frame {n} chroma ({column}, {row})"
                        );
                    }
                }
            })
            .unwrap();
    }
    assert!(matches!(
        decoder.submit(AccessUnit {
            id: 99,
            bytes: &samples[0],
            pts_ns: None,
            dts_ns: None,
            duration_ns: None,
        }),
        Err(Error::EndOfStream)
    ));
}

#[test]
fn vulkan_decodes_the_probe_clip() {
    decodes_the_probe_clip(&VULKAN);
}

#[test]
fn vaapi_decodes_the_probe_clip() {
    decodes_the_probe_clip(&VAAPI);
}

#[test]
fn cuda_decodes_the_probe_clip() {
    decodes_the_probe_clip(&CUDA);
}

#[test]
fn a_missing_library_leaves_the_backend_unavailable() {
    let hardware = Hardware::bogus();
    let why = hardware.probe().unwrap_err();
    assert!(why.contains("not installed"), "{why}");
    let (sets, _) = probe_samples();
    let sets: Vec<&[u8]> = sets.iter().map(Vec::as_slice).collect();
    assert!(matches!(
        hardware.h264_avcc(&sets, 4),
        Err(Error::BackendUnavailable(_))
    ));
    assert_eq!(hardware.description(), "vulkan");
}

#[test]
fn streams_no_hardware_backend_decodes_are_refused_before_opening() {
    let (sets, _) = probe_samples();
    let pps = sets[1].as_slice();
    // High 10 4:2:2, as in the bitstream tests.
    let high_422: &[u8] = &[0x67, 122, 0, 40, 0b1011_0110, 0b1110_0000];
    let refused = Hardware::bogus().h264_avcc(&[high_422, pps], 4);
    assert!(
        matches!(&refused, Err(Error::BackendUnavailable(why)) if why.contains("profile_idc 122")),
        "{:?}",
        refused.err()
    );
    assert!(matches!(
        Hardware::bogus().h264_avcc(&[pps], 4),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        Hardware::bogus().h264_avcc(&[&sets[0], pps], 3),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn aspect_ratio_is_reduced_and_unknown_is_square() {
    assert_eq!(aspect_ratio(0, 1), (1, 1));
    assert_eq!(aspect_ratio(-4, 3), (1, 1));
    assert_eq!(aspect_ratio(64, 48), (4, 3));
}
