//! What Rosemary keeps in a cache file beside Microlift's image: the slide the image
//! was lifted at, the tier its code was compiled at, and the path it was read from.

use crate::cache::Tier;

/// Identifies Rosemary's part of a cache file.
const MAGIC: [u8; 8] = *b"RMAOTv3\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Embedded {
    /// Run-time minus file address of the image when it was lifted. A run that
    /// places it elsewhere loads the code at the difference.
    pub lift_slide: u64,
    /// How many addresses the image's profile log held when this was built, so a run
    /// can tell when it has grown enough to build again.
    pub profiled: u64,
    /// How hard Cranelift worked on the code: a provisional region is upgraded when
    /// it first runs, as a JIT one is.
    pub tier: Tier,
    /// Where the image was read from when the file was built. Files are keyed by
    /// the image's contents, so a copy elsewhere can find this file too; this is
    /// for tools reading the file, never for loading it.
    pub source: String,
}

/// Why a file's embedded data could not be read back.
#[derive(Debug)]
pub enum DecodeError {
    NotACacheFile,
    Truncated,
    SourceNotUtf8,
}

impl Embedded {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MAGIC.len() + 9 + self.source.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&self.lift_slide.to_le_bytes());
        out.extend_from_slice(&self.profiled.to_le_bytes());
        out.push(match self.tier {
            Tier::Final => 0,
            Tier::Provisional => 1,
        });
        out.extend_from_slice(self.source.as_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let rest = bytes
            .strip_prefix(&MAGIC)
            .ok_or(DecodeError::NotACacheFile)?;
        let (slide, rest) = rest.split_at_checked(8).ok_or(DecodeError::Truncated)?;
        let (profiled, rest) = rest.split_at_checked(8).ok_or(DecodeError::Truncated)?;
        let (&tier, source) = rest.split_first().ok_or(DecodeError::Truncated)?;
        Ok(Embedded {
            lift_slide: u64::from_le_bytes(slide.try_into().expect("eight bytes")),
            profiled: u64::from_le_bytes(profiled.try_into().expect("eight bytes")),
            tier: match tier {
                0 => Tier::Final,
                1 => Tier::Provisional,
                _ => return Err(DecodeError::NotACacheFile),
            },
            source: String::from_utf8(source.to_vec()).map_err(|_| DecodeError::SourceNotUtf8)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_data_round_trips() {
        let embedded = Embedded {
            lift_slide: 0x1000,
            profiled: 1234,
            tier: Tier::Provisional,
            source: "/Applications/Example.app/Contents/MacOS/Example".into(),
        };
        assert_eq!(Embedded::decode(&embedded.encode()).unwrap(), embedded);
    }

    #[test]
    fn foreign_or_truncated_data_is_refused() {
        let bytes = Embedded {
            lift_slide: 0,
            profiled: 0,
            tier: Tier::Final,
            source: String::new(),
        }
        .encode();
        assert!(matches!(
            Embedded::decode(&bytes[..12]),
            Err(DecodeError::Truncated)
        ));
        assert!(matches!(
            Embedded::decode(b"not a cache file"),
            Err(DecodeError::NotACacheFile)
        ));
    }
}
