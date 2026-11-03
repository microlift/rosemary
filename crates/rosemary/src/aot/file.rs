//! What one cache file holds: the slide its image was lifted at, and the
//! Microlift images compiled for it, highest priority first.
//!
//! Several images rather than one so the offline compiler can build them on
//! separate threads, and so a build stopped by its deadline still writes every
//! chunk that finished.

use microlift::image::Image;

/// Identifies a Rosemary cache file, ahead of anything Microlift reads.
const MAGIC: [u8; 8] = *b"RMAOTv1\0";

/// Bytes before the first chunk: the magic, the lift slide and the chunk count.
const HEADER_LEN: usize = MAGIC.len() + 8 + 8;

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
        let chunks: Vec<Vec<u8>> = self.chunks.iter().map(Image::encode).collect();
        let len = HEADER_LEN + chunks.iter().map(|c| 8 + c.len()).sum::<usize>();
        let mut out = Vec::with_capacity(len);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&self.lift_slide.to_le_bytes());
        out.extend_from_slice(&(chunks.len() as u64).to_le_bytes());
        for chunk in &chunks {
            out.extend_from_slice(&(chunk.len() as u64).to_le_bytes());
            out.extend_from_slice(chunk);
        }
        out
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
