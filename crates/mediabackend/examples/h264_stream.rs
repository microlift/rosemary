//! Performance/continuity exerciser for software GStreamer H.264 decoding.
//! Raw Annex-B inputs are split into access units by h264parse, including streams without AUDs.
//! Usage: cargo run -p mediabackend --features gstreamer --release --example h264_stream -- FILE [THREADS]
use mediabackend::{AccessUnit, Error, VideoDecoder, VideoFrame, gstreamer::Decoder};
use std::{
    env, fs,
    time::{Duration, Instant},
};

fn access_units(bytes: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    use gst::prelude::*;
    use gst_app::{AppSink, AppSrc};

    if bytes.is_empty() {
        return Err(Error::InvalidInput("empty H.264 stream".into()));
    }
    gst::init().map_err(|e| Error::BackendUnavailable(e.to_string()))?;
    // An AUD is optional in H.264. Delegate picture-boundary recognition to
    // h264parse instead of treating the whole AUD-less file as one access unit.
    let pipeline = gst::parse::launch(
        "appsrc name=compressed format=time is-live=false block=false \
         caps=video/x-h264,stream-format=byte-stream \
         ! h264parse ! video/x-h264,alignment=au,stream-format=byte-stream \
         ! appsink name=parsed sync=false max-buffers=32 drop=false",
    )
    .map_err(|e| Error::BackendUnavailable(format!("H.264 parser: {e}")))?
    .downcast::<gst::Pipeline>()
    .map_err(|_| Error::BackendUnavailable("invalid parser pipeline".into()))?;
    let input = pipeline
        .by_name("compressed")
        .and_then(|e| e.downcast::<AppSrc>().ok())
        .ok_or_else(|| Error::BackendUnavailable("missing parser appsrc".into()))?;
    let output = pipeline
        .by_name("parsed")
        .and_then(|e| e.downcast::<AppSink>().ok())
        .ok_or_else(|| Error::BackendUnavailable("missing parser appsink".into()))?;
    let result = (|| {
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| Error::BackendUnavailable(format!("cannot start parser: {e:?}")))?;
        input
            .push_buffer(gst::Buffer::from_mut_slice(bytes.to_vec()))
            .map_err(|e| Error::Decode(format!("cannot submit H.264 stream: {e:?}")))?;
        input
            .end_of_stream()
            .map_err(|e| Error::Decode(format!("cannot end parsed stream: {e:?}")))?;
        let bus = pipeline
            .bus()
            .ok_or_else(|| Error::Decode("missing parser bus".into()))?;
        let mut units = Vec::new();
        let mut deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error])
                && let gst::MessageView::Error(e) = msg.view()
            {
                return Err(Error::Decode(format!(
                    "H.264 parser: {}: {:?}",
                    e.error(),
                    e.debug()
                )));
            }
            if let Some(sample) = output.try_pull_sample(gst::ClockTime::from_mseconds(100)) {
                let buffer = sample
                    .buffer()
                    .ok_or_else(|| Error::Decode("parser emitted sample without buffer".into()))?;
                let mapped = buffer
                    .map_readable()
                    .map_err(|_| Error::Decode("cannot map parsed access unit".into()))?;
                if mapped.as_slice().is_empty() {
                    return Err(Error::Decode("parser emitted empty access unit".into()));
                }
                units.push(mapped.as_slice().to_vec());
                deadline = Instant::now() + Duration::from_secs(15);
            } else if output.is_eos() {
                break;
            } else if Instant::now() > deadline {
                return Err(Error::Decode("H.264 parser timed out".into()));
            }
        }
        if let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error])
            && let gst::MessageView::Error(e) = msg.view()
        {
            return Err(Error::Decode(format!(
                "H.264 parser: {}: {:?}",
                e.error(),
                e.debug()
            )));
        }
        if units.is_empty() {
            return Err(Error::InvalidInput(
                "H.264 stream contains no decodable access units".into(),
            ));
        }
        Ok(units)
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}

fn receive(
    decoder: &mut Decoder,
    timeout: Duration,
    ids: &mut Vec<u64>,
    bytes: &mut u64,
) -> Result<bool, Error> {
    if let Some(dropped) = decoder.take_dropped() {
        return Err(Error::Decode(format!(
            "decoder dropped access unit {}: {:?}",
            dropped.id, dropped.error
        )));
    }
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
                pts_ns: Some((index as i64 + 1) * 1_000_000),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_audless_h264_into_individual_pictures() {
        let data = include_bytes!("../tests/fixtures/audless_10_64x64.h264");
        assert_eq!(access_units(data).unwrap().len(), 10);
    }

    #[test]
    fn parses_aud_delimited_h264_without_losing_pictures() {
        let data = include_bytes!("../tests/fixtures/anamorphic_4_64x64.h264");
        assert_eq!(access_units(data).unwrap().len(), 4);
    }
}
