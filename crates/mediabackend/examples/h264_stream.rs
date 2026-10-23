//! Performance/continuity exerciser for software GStreamer H.264 decoding.
//! Usage: cargo run -p mediabackend --features gstreamer --release --example h264_stream -- FILE [THREADS]
use mediabackend::{AccessUnit, Error, VideoDecoder, VideoFrame, gstreamer::Decoder};
use std::{
    env, fs,
    time::{Duration, Instant},
};

fn access_units(bytes: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        let width = if bytes[index..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[index..].starts_with(&[0, 0, 1]) {
            3
        } else {
            index += 1;
            continue;
        };
        starts.push((index, width));
        index += width;
    }
    let mut units = Vec::new();
    for (i, &(pos, width)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(bytes.len(), |(p, _)| *p);
        let nal = &bytes[pos + width..end];
        if nal.is_empty() {
            continue;
        }
        if nal[0] & 31 == 9 {
            units.push(Vec::new());
        }
        if units.is_empty() {
            units.push(Vec::new());
        }
        units
            .last_mut()
            .unwrap()
            .extend_from_slice(&bytes[pos..end]);
    }
    if units.iter().any(Vec::is_empty) {
        return Err(Error::InvalidInput("AUD with no access unit".into()));
    }
    Ok(units)
}

fn receive(
    decoder: &mut Decoder,
    timeout: Duration,
    ids: &mut Vec<u64>,
    bytes: &mut u64,
) -> Result<bool, Error> {
    let Some(frame) = decoder.pull(timeout)? else {
        return Ok(false);
    };
    *bytes += frame.with_bytes(|buffer, _| buffer.len() as u64)?;
    ids.push(frame.id());
    Ok(true)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file = env::args().nth(1).ok_or("missing H.264 path")?;
    let threads: u32 = env::args().nth(2).unwrap_or_else(|| "1".into()).parse()?;
    let data = fs::read(&file)?;
    let units = access_units(&data)?;
    let started = Instant::now();
    let mut decoder = Decoder::h264_nv12(threads)?;
    let mut decoded = Vec::new();
    let mut bytes = 0;
    for (index, unit) in units.iter().enumerate() {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match decoder.submit(AccessUnit {
                id: index as u64,
                bytes: unit,
                pts_ns: Some((index as u64 + 1) * 1_000_000),
                dts_ns: None,
                duration_ns: Some(1_000_000),
            }) {
                Ok(()) => break,
                Err(Error::Backpressure) if Instant::now() < deadline => {
                    receive(
                        &mut decoder,
                        Duration::from_millis(100),
                        &mut decoded,
                        &mut bytes,
                    )?;
                }
                Err(e) => return Err(Box::new(e)),
            }
        }
        while receive(&mut decoder, Duration::ZERO, &mut decoded, &mut bytes)? {}
    }
    decoder.end_stream()?;
    while decoded.len() < units.len() {
        if !receive(
            &mut decoder,
            Duration::from_secs(3),
            &mut decoded,
            &mut bytes,
        )? {
            return Err(
                format!("decoder stalled: {}/{} frames", decoded.len(), units.len()).into(),
            );
        }
    }
    let elapsed = started.elapsed();
    decoded.sort_unstable();
    if decoded
        .iter()
        .copied()
        .ne((0..units.len()).map(|i| i as u64))
    {
        return Err("lost or duplicate decoded identities".into());
    }
    println!(
        "frames={} input_bytes={} mapped_bytes={} threads={} elapsed_ms={:.1} fps={:.1}",
        decoded.len(),
        data.len(),
        bytes,
        threads,
        elapsed.as_secs_f64() * 1000.0,
        decoded.len() as f64 / elapsed.as_secs_f64()
    );
    Ok(())
}
