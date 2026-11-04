//! What one cache file holds: the slide its image was lifted at, and the
//! Microlift images compiled for it, in the order they finished.
//!
//! Several images rather than one so the offline compiler can build them on
//! separate threads and write each as it finishes, and so a build stopped by its
//! deadline still writes every chunk that finished.

use std::io::{Seek, SeekFrom, Write};

use microlift::image::Image;

/// Identifies a Rosemary cache file, ahead of anything Microlift reads.
const MAGIC: [u8; 8] = *b"RMAOTv1\0";

/// Where the chunk count is, after the magic and the lift slide.
const COUNT_OFFSET: u64 = (MAGIC.len() + 8) as u64;

#[derive(Debug, PartialEq)]
pub struct CacheFile {
    /// Run-time minus file address of the image when it was lifted. A run that
    /// places it elsewhere loads the code at the difference.
    pub lift_slide: u64,
    pub chunks: Vec<Image>,
}

/// Why a file's bytes could not be read back.
#[derive(Debug)]
pub enum DecodeError {
    NotACacheFile,
    Truncated,
    Image(microlift::MicroliftError),
}

impl CacheFile {
    pub fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::new(std::io::Cursor::new(Vec::new()), self.lift_slide)
            .expect("writing to memory");
        for chunk in &self.chunks {
            writer.push(chunk).expect("writing to memory");
        }
        writer.finish().expect("writing to memory").into_inner()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.get(..MAGIC.len()) != Some(&MAGIC[..]) {
            return Err(DecodeError::NotACacheFile);
        }
        let mut at = MAGIC.len();
        let word = |at: &mut usize| -> Result<u64, DecodeError> {
            let field = bytes.get(*at..*at + 8).ok_or(DecodeError::Truncated)?;
            *at += 8;
            Ok(u64::from_le_bytes(field.try_into().expect("eight bytes")))
        };
        let lift_slide = word(&mut at)?;
        let count = word(&mut at)?;
        let mut chunks = Vec::new();
        for _ in 0..count {
            let len = usize::try_from(word(&mut at)?).map_err(|_| DecodeError::Truncated)?;
            let end = at.checked_add(len).ok_or(DecodeError::Truncated)?;
            let chunk = bytes.get(at..end).ok_or(DecodeError::Truncated)?;
            chunks.push(Image::decode(chunk).map_err(DecodeError::Image)?);
            at = end;
        }
        if at != bytes.len() {
            return Err(DecodeError::Truncated);
        }
        Ok(CacheFile { lift_slide, chunks })
    }
}

/// Writes a cache file one chunk at a time, so a build never holds more than the
/// chunk it is writing.
pub struct Writer<W: Write + Seek> {
    out: W,
    chunks: u64,
}

impl<W: Write + Seek> Writer<W> {
    pub fn new(mut out: W, lift_slide: u64) -> std::io::Result<Self> {
        out.write_all(&MAGIC)?;
        out.write_all(&lift_slide.to_le_bytes())?;
        // The chunk count, filled in by `finish`.
        out.write_all(&0u64.to_le_bytes())?;
        Ok(Writer { out, chunks: 0 })
    }

    pub fn push(&mut self, chunk: &Image) -> std::io::Result<()> {
        let bytes = chunk.encode();
        self.out.write_all(&(bytes.len() as u64).to_le_bytes())?;
        self.out.write_all(&bytes)?;
        self.chunks += 1;
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<W> {
        self.out.seek(SeekFrom::Start(COUNT_OFFSET))?;
        self.out.write_all(&self.chunks.to_le_bytes())?;
        self.out.seek(SeekFrom::End(0))?;
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(fingerprint: u64) -> Image {
        Image {
            isa_fingerprint: 1,
            config_fingerprint: fingerprint,
            text: vec![0xc3; 16],
            rodata: Vec::new(),
            rodata_align: 16,
            bss_len: 0,
            symbols: Vec::new(),
            relocs: Vec::new(),
            regions: Vec::new(),
        }
    }

    #[test]
    fn a_file_round_trips_with_its_chunks_in_order() {
        let file = CacheFile {
            lift_slide: 0x1000,
            chunks: vec![image(2), image(3)],
        };
        assert_eq!(CacheFile::decode(&file.encode()).unwrap(), file);
    }

    #[test]
    fn a_streamed_file_counts_the_chunks_written() {
        let mut writer = Writer::new(std::io::Cursor::new(Vec::new()), 0x2000).unwrap();
        writer.push(&image(4)).unwrap();
        writer.push(&image(5)).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let file = CacheFile::decode(&bytes).unwrap();
        assert_eq!(file.lift_slide, 0x2000);
        assert_eq!(file.chunks, [image(4), image(5)]);
    }

    #[test]
    fn a_truncated_file_is_refused() {
        let bytes = CacheFile {
            lift_slide: 0,
            chunks: vec![image(2)],
        }
        .encode();
        assert!(matches!(
            CacheFile::decode(&bytes[..bytes.len() - 1]),
            Err(DecodeError::Truncated | DecodeError::Image(_))
        ));
        assert!(matches!(
            CacheFile::decode(b"not a cache file"),
            Err(DecodeError::NotACacheFile)
        ));
    }
}
