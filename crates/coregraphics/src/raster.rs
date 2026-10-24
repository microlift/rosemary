/// An image in memory: RGBA, top row first, `width * height * 4` bytes.
#[derive(Clone, PartialEq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Bitmap {}x{} ({} bytes)",
            self.width,
            self.height,
            self.pixels.len()
        )
    }
}

impl Bitmap {
    pub fn new(width: u32, height: u32) -> Bitmap {
        Bitmap {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
        }
    }

    pub fn bytes_per_row(&self) -> usize {
        self.width as usize * 4
    }
}

/// Decode whatever `bytes` is, or `None` for a container this cannot read.
pub fn decode(bytes: &[u8]) -> Option<Bitmap> {
    if bytes.starts_with(b"BM") {
        return bmp(bytes);
    }
    if bytes.starts_with(b"II") || bytes.starts_with(b"MM") {
        return tiff(bytes);
    }
    if bytes.starts_with(PNG_SIGNATURE) {
        return png(bytes);
    }
    if bytes.starts_with(b"icns") {
        return icns(bytes);
    }
    None
}

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// An uncompressed Windows BMP of 24 or 32 bits per pixel.
///
/// The two things that are easy to get wrong: rows are padded to a multiple of
/// four bytes, and a positive height means the rows are stored bottom-up. A
/// decoder that ignores either produces an image that is skewed or upside down,
/// both of which look like a drawing bug much later.
fn bmp(bytes: &[u8]) -> Option<Bitmap> {
    let pixel_offset = le32(bytes, 10)? as usize;
    let header_size = le32(bytes, 14)?;
    if header_size < 40 {
        // A `BITMAPCOREHEADER` has twelve-bit fields in different places. Not
        // read rather than misread.
        return None;
    }
    let width = le32(bytes, 18)? as i32;
    let signed_height = le32(bytes, 22)? as i32;
    let bits = le16(bytes, 28)?;
    let compression = le32(bytes, 30)?;
    // 0 is `BI_RGB`, the only uncompressed one. `BI_BITFIELDS` (3) names its
    // channel masks in three words after the header and is not read here.
    if compression != 0 || !matches!(bits, 24 | 32) || width <= 0 || signed_height == 0 {
        return None;
    }

    let width = width as u32;
    let height = signed_height.unsigned_abs();
    let bottom_up = signed_height > 0;
    let source_bytes_per_pixel = bits as usize / 8;
    // Rows are padded to four bytes, which for 32-bit is already the case.
    let stride = (width as usize * source_bytes_per_pixel).next_multiple_of(4);

    let mut out = Bitmap::new(width, height);
    for row in 0..height as usize {
        let source_row = match bottom_up {
            true => height as usize - 1 - row,
            false => row,
        };
        let start = pixel_offset.checked_add(source_row.checked_mul(stride)?)?;
        let source = bytes.get(start..start + width as usize * source_bytes_per_pixel)?;
        let destination = row * out.bytes_per_row();
        for x in 0..width as usize {
            let p = &source[x * source_bytes_per_pixel..];
            // BMP stores blue, green, red - and alpha where it has one.
            out.pixels[destination + x * 4] = p[2];
            out.pixels[destination + x * 4 + 1] = p[1];
            out.pixels[destination + x * 4 + 2] = p[0];
            out.pixels[destination + x * 4 + 3] = match source_bytes_per_pixel {
                4 => p[3],
                _ => 0xff,
            };
        }
    }
    Some(out)
}

/// The TIFF tags this module writes and reads.
mod tag {
    pub const WIDTH: u16 = 256;
    pub const HEIGHT: u16 = 257;
    pub const BITS_PER_SAMPLE: u16 = 258;
    pub const COMPRESSION: u16 = 259;
    pub const PHOTOMETRIC: u16 = 262;
    pub const STRIP_OFFSETS: u16 = 273;
    pub const SAMPLES_PER_PIXEL: u16 = 277;
    pub const ROWS_PER_STRIP: u16 = 278;
    pub const STRIP_BYTE_COUNTS: u16 = 279;
    pub const PLANAR_CONFIGURATION: u16 = 284;
    pub const EXTRA_SAMPLES: u16 = 338;

    pub const TYPE_SHORT: u16 = 3;
    pub const TYPE_LONG: u16 = 4;
}

/// `bitmap` as an uncompressed little-endian RGBA TIFF.
pub fn encode_tiff(bitmap: &Bitmap) -> Vec<u8> {
    // Eleven entries, each twelve bytes, after a two-byte count and before a
    // four-byte "no next directory" - so the data area starts at a fixed place
    // and the two out-of-line values go there in a known order.
    const ENTRIES: usize = 11;
    let ifd_at = 8usize;
    let data_at = ifd_at + 2 + ENTRIES * 12 + 4;
    let bits_at = data_at;
    let pixels_at = bits_at + 8;

    let mut out = Vec::with_capacity(pixels_at + bitmap.pixels.len());
    out.extend_from_slice(b"II");
    out.extend_from_slice(&42u16.to_le_bytes());
    out.extend_from_slice(&(ifd_at as u32).to_le_bytes());

    out.extend_from_slice(&(ENTRIES as u16).to_le_bytes());
    let mut entry = |tag: u16, kind: u16, count: u32, value: u32| {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        // A value narrower than four bytes sits in the low end of the field
        // rather than being right-justified, which is the rule most often got
        // backwards.
        out.extend_from_slice(&value.to_le_bytes());
    };
    entry(tag::WIDTH, tag::TYPE_LONG, 1, bitmap.width);
    entry(tag::HEIGHT, tag::TYPE_LONG, 1, bitmap.height);
    entry(tag::BITS_PER_SAMPLE, tag::TYPE_SHORT, 4, bits_at as u32);
    entry(tag::COMPRESSION, tag::TYPE_SHORT, 1, 1);
    // 2 is RGB.
    entry(tag::PHOTOMETRIC, tag::TYPE_SHORT, 1, 2);
    entry(tag::STRIP_OFFSETS, tag::TYPE_LONG, 1, pixels_at as u32);
    entry(tag::SAMPLES_PER_PIXEL, tag::TYPE_SHORT, 1, 4);
    entry(tag::ROWS_PER_STRIP, tag::TYPE_LONG, 1, bitmap.height);
    entry(
        tag::STRIP_BYTE_COUNTS,
        tag::TYPE_LONG,
        1,
        bitmap.pixels.len() as u32,
    );
    entry(tag::PLANAR_CONFIGURATION, tag::TYPE_SHORT, 1, 1);
    // 2 is "unassociated alpha", which is what an un-premultiplied RGBA is.
    entry(tag::EXTRA_SAMPLES, tag::TYPE_SHORT, 1, 2);
    out.extend_from_slice(&0u32.to_le_bytes());

    debug_assert_eq!(out.len(), bits_at);
    for _ in 0..4 {
        out.extend_from_slice(&8u16.to_le_bytes());
    }
    debug_assert_eq!(out.len(), pixels_at);
    out.extend_from_slice(&bitmap.pixels);
    out
}

/// An uncompressed TIFF, of the shape [`encode_tiff`] writes.
///
/// Little-endian only, which is what `II` says. A `MM` file is big-endian and is
/// declined rather than read with the bytes the wrong way round.
fn tiff(bytes: &[u8]) -> Option<Bitmap> {
    if !bytes.starts_with(b"II") || le16(bytes, 2)? != 42 {
        return None;
    }
    let ifd = le32(bytes, 4)? as usize;
    let count = le16(bytes, ifd)? as usize;

    let mut fields: Vec<(u16, u16, u32, u32)> = Vec::with_capacity(count);
    for i in 0..count {
        let at = ifd + 2 + i * 12;
        fields.push((
            le16(bytes, at)?,
            le16(bytes, at + 2)?,
            le32(bytes, at + 4)?,
            le32(bytes, at + 8)?,
        ));
    }
    let field = |wanted: u16| fields.iter().find(|(t, ..)| *t == wanted).copied();
    // A single-count SHORT sits in the low half of its value field.
    let scalar = |wanted: u16| {
        field(wanted).map(|(_, kind, _, value)| match kind {
            tag::TYPE_SHORT => value & 0xffff,
            _ => value,
        })
    };

    let width = scalar(tag::WIDTH)?;
    let height = scalar(tag::HEIGHT)?;
    let samples = scalar(tag::SAMPLES_PER_PIXEL).unwrap_or(1);
    let compression = scalar(tag::COMPRESSION).unwrap_or(1);
    let planar = scalar(tag::PLANAR_CONFIGURATION).unwrap_or(1);
    if compression != 1 || planar != 1 || !matches!(samples, 3 | 4) {
        return None;
    }
    // Every sample has to be eight bits; the array is out of line for more than
    // two samples, and inline for one or two.
    let (_, _, bits_count, bits_value) = field(tag::BITS_PER_SAMPLE)?;
    let bits_ok = match bits_count > 2 {
        true => {
            (0..bits_count as usize).all(|i| le16(bytes, bits_value as usize + i * 2) == Some(8))
        }
        false => bits_value & 0xffff == 8,
    };
    if !bits_ok {
        return None;
    }

    // One strip, which is what `rows_per_strip >= height` means and what this
    // module writes. A multi-strip file would need the offsets array walked.
    let rows_per_strip = scalar(tag::ROWS_PER_STRIP).unwrap_or(height);
    let (_, _, strips, first_strip) = field(tag::STRIP_OFFSETS)?;
    if rows_per_strip < height || strips != 1 {
        return None;
    }

    let source_bytes_per_pixel = samples as usize;
    let stride = width as usize * source_bytes_per_pixel;
    let mut out = Bitmap::new(width, height);
    for row in 0..height as usize {
        let start = (first_strip as usize).checked_add(row.checked_mul(stride)?)?;
        let source = bytes.get(start..start + stride)?;
        let destination = row * out.bytes_per_row();
        for x in 0..width as usize {
            let p = &source[x * source_bytes_per_pixel..];
            out.pixels[destination + x * 4] = p[0];
            out.pixels[destination + x * 4 + 1] = p[1];
            out.pixels[destination + x * 4 + 2] = p[2];
            out.pixels[destination + x * 4 + 3] = match source_bytes_per_pixel {
                4 => p[3],
                _ => 0xff,
            };
        }
    }
    Some(out)
}

// zlib's one-shot inflate: the same copy of the library the guest's own zlib
// imports are forwarded to, reached here by name rather than through a slot.
#[link(name = "z")]
unsafe extern "C" {
    fn uncompress(
        dest: *mut u8,
        dest_len: *mut std::ffi::c_ulong,
        source: *const u8,
        source_len: std::ffi::c_ulong,
    ) -> std::ffi::c_int;
}

/// A whole zlib stream inflated, for data whose inflated size is not known
/// up front: the one-shot call is retried with a larger buffer until it fits.
pub(crate) fn inflate(source: &[u8]) -> Option<Vec<u8>> {
    const Z_OK: std::ffi::c_int = 0;
    const Z_BUF_ERROR: std::ffi::c_int = -5;
    let mut capacity = source.len().saturating_mul(4).max(1024);
    loop {
        let mut out = vec![0u8; capacity];
        let mut length = capacity as std::ffi::c_ulong;
        // SAFETY: both buffers are live and their lengths are the ones passed.
        let status = unsafe {
            uncompress(
                out.as_mut_ptr(),
                &mut length,
                source.as_ptr(),
                source.len() as std::ffi::c_ulong,
            )
        };
        match status {
            Z_OK => {
                out.truncate(length as usize);
                return Some(out);
            }
            // Short of room only when the buffer was filled; otherwise the
            // input ran out, which more room does not fix.
            Z_BUF_ERROR if length as usize == capacity && capacity < 256 << 20 => capacity *= 4,
            _ => return None,
        }
    }
}

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// `IHDR`, which is everything the rest of the decoder branches on.
struct PngHeader {
    width: u32,
    height: u32,
    depth: u8,
    colour: u8,
    interlace: u8,
}

impl PngHeader {
    /// Samples per pixel for this colour type, or `None` for one that is not a
    /// colour type at all.
    fn channels(&self) -> Option<usize> {
        match self.colour {
            0 => Some(1),
            2 => Some(3),
            3 => Some(1),
            4 => Some(2),
            6 => Some(4),
            _ => None,
        }
    }
}

/// An eight-bit non-interlaced PNG.
///
/// Sixteen-bit samples and Adam7 are declined rather than approximated: half of
/// a sixteen-bit sample is a visibly different image, and an interlaced file
/// read as if it were not is seven overlapping thumbnails.
fn png(bytes: &[u8]) -> Option<Bitmap> {
    let mut header = None;
    let mut palette: &[u8] = &[];
    let mut transparency: &[u8] = &[];
    let mut deflated: Vec<u8> = Vec::new();

    let mut at = PNG_SIGNATURE.len();
    while at + 8 <= bytes.len() {
        let length = be32(bytes, at)? as usize;
        let kind = bytes.get(at + 4..at + 8)?;
        let data = bytes.get(at + 8..at + 8 + length)?;
        match kind {
            b"IHDR" => {
                header = Some(PngHeader {
                    width: be32(data, 0)?,
                    height: be32(data, 4)?,
                    depth: *data.get(8)?,
                    colour: *data.get(9)?,
                    interlace: *data.get(12)?,
                })
            }
            b"PLTE" => palette = data,
            b"tRNS" => transparency = data,
            b"IDAT" => deflated.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        // Length, type, data, then a four-byte CRC this decoder does not check:
        // a corrupt icon is not a threat model, and a file that fails to
        // inflate is refused below anyway.
        at += 12 + length;
    }

    let header = header?;
    if header.depth != 8 || header.interlace != 0 || header.width == 0 || header.height == 0 {
        return None;
    }
    let channels = header.channels()?;
    let width = header.width as usize;
    let height = header.height as usize;
    let stride = width.checked_mul(channels)?;

    // Every row is one filter byte followed by its samples, so the inflated
    // size is known exactly - which is what lets this use the one-shot
    // `uncompress` instead of driving a stream.
    let mut raw = vec![0u8; (stride + 1).checked_mul(height)?];
    let mut produced = raw.len() as std::ffi::c_ulong;
    // SAFETY: `raw` has room for `produced` bytes and `deflated` is that many
    // valid bytes of input.
    let status = unsafe {
        uncompress(
            raw.as_mut_ptr(),
            &mut produced,
            deflated.as_ptr(),
            deflated.len() as std::ffi::c_ulong,
        )
    };
    if status != 0 || produced as usize != raw.len() {
        return None;
    }

    let mut out = Bitmap::new(header.width, header.height);
    let mut previous = vec![0u8; stride];
    let mut row = vec![0u8; stride];
    for y in 0..height {
        let start = y * (stride + 1);
        let filter = raw[start];
        row.copy_from_slice(&raw[start + 1..start + 1 + stride]);
        unfilter(filter, &mut row, &previous, channels)?;
        let destination = y * out.bytes_per_row();
        for x in 0..width {
            let sample = &row[x * channels..];
            let rgba = match header.colour {
                0 => [sample[0], sample[0], sample[0], 0xff],
                2 => [sample[0], sample[1], sample[2], 0xff],
                3 => {
                    let index = sample[0] as usize;
                    let entry = palette.get(index * 3..index * 3 + 3)?;
                    let alpha = transparency.get(index).copied().unwrap_or(0xff);
                    [entry[0], entry[1], entry[2], alpha]
                }
                4 => [sample[0], sample[0], sample[0], sample[1]],
                _ => [sample[0], sample[1], sample[2], sample[3]],
            };
            out.pixels[destination + x * 4..destination + x * 4 + 4].copy_from_slice(&rgba);
        }
        std::mem::swap(&mut previous, &mut row);
    }
    Some(out)
}

/// Undo one row's filter, given the row above it already undone.
///
/// The filters predict from the byte to the left, the byte above and the byte
/// diagonally above-left; outside the image each of those reads as zero, which
/// is what the first row and the first pixel of every row rely on.
fn unfilter(filter: u8, row: &mut [u8], previous: &[u8], channels: usize) -> Option<()> {
    for i in 0..row.len() {
        let left = match i >= channels {
            true => row[i - channels] as i32,
            false => 0,
        };
        let above = previous.get(i).copied().unwrap_or(0) as i32;
        let diagonal = match i >= channels {
            true => previous.get(i - channels).copied().unwrap_or(0) as i32,
            false => 0,
        };
        let predicted = match filter {
            0 => 0,
            1 => left,
            2 => above,
            3 => (left + above) / 2,
            4 => paeth(left, above, diagonal),
            _ => return None,
        };
        row[i] = (row[i] as i32).wrapping_add(predicted) as u8;
    }
    Some(())
}

fn paeth(left: i32, above: i32, diagonal: i32) -> i32 {
    let estimate = left + above - diagonal;
    let (dl, da, dd) = (
        (estimate - left).abs(),
        (estimate - above).abs(),
        (estimate - diagonal).abs(),
    );
    if dl <= da && dl <= dd {
        left
    } else if da <= dd {
        above
    } else {
        diagonal
    }
}

/// The classic entries: a 24-bit image, the 8-bit mask that goes with it, and
/// the side both are.
///
/// An `.icns` with none of the PNG entries is not old enough to be rare -
/// Subnautica's is exactly this and nothing else - so these are read rather
/// than skipped past.
const CLASSIC: &[(&[u8; 4], &[u8; 4], u32)] = &[
    (b"is32", b"s8mk", 16),
    (b"il32", b"l8mk", 32),
    (b"ih32", b"h8mk", 48),
    (b"it32", b"t8mk", 128),
];

/// The straight ARGB entries, which carry their own alpha.
const ARGB: &[(&[u8; 4], u32)] = &[(b"ic04", 16), (b"ic05", 32)];

/// The largest representation worth taking out of an `.icns`.
///
/// A window icon is scaled by the window server from whatever it is given, and
/// the 1024-pixel entry a modern bundle carries is four megabytes to hand over
/// for something drawn at 128.
const ICON_SIDE_LIMIT: u32 = 512;

/// One representation inside an `.icns`, before it is decoded.
///
/// Chosen by side and decoded afterwards rather than the other way round: a
/// bundle may carry ten of these, and decoding all of them to compare sizes
/// would inflate a megabyte of PNG to throw nine tenths of it away.
enum Rep<'a> {
    Png(&'a [u8]),
    Classic {
        data: &'a [u8],
        mask: Option<&'a [u8]>,
        side: u32,
        leading_header: bool,
    },
    Argb(&'a [u8], u32),
}

/// The `.icns` representation nearest [`ICON_SIDE_LIMIT`] from below, or the
/// smallest one when every entry is larger.
fn icns(bytes: &[u8]) -> Option<Bitmap> {
    let mut entries: Vec<([u8; 4], &[u8])> = Vec::new();
    let mut at = 8;
    while at + 8 <= bytes.len() {
        let kind: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
        let length = be32(bytes, at + 4)? as usize;
        if length < 8 {
            break;
        }
        if let Some(data) = bytes.get(at + 8..at + length) {
            entries.push((kind, data));
        }
        at += length;
    }
    let entry = |wanted: &[u8; 4]| {
        entries
            .iter()
            .find(|(kind, _)| kind == wanted)
            .map(|(_, data)| *data)
    };

    let mut reps: Vec<(u32, Rep)> = Vec::new();
    for (kind, data) in &entries {
        if data.starts_with(PNG_SIGNATURE)
            && let Some(side) = be32(data, 16)
        {
            reps.push((side, Rep::Png(data)));
            continue;
        }
        if let Some((_, mask, side)) = CLASSIC.iter().find(|(t, ..)| *t == kind) {
            reps.push((
                *side,
                Rep::Classic {
                    data,
                    mask: entry(mask),
                    side: *side,
                    leading_header: kind == b"it32",
                },
            ));
            continue;
        }
        if let Some((_, side)) = ARGB.iter().find(|(t, _)| *t == kind)
            && let Some(planes) = data.strip_prefix(b"ARGB")
        {
            reps.push((*side, Rep::Argb(planes, *side)));
        }
    }

    let best = reps
        .iter()
        .filter(|(side, _)| *side <= ICON_SIDE_LIMIT)
        .max_by_key(|(side, _)| *side)
        .or_else(|| reps.iter().min_by_key(|(side, _)| *side))?;
    match &best.1 {
        Rep::Png(data) => png(data),
        Rep::Classic {
            data,
            mask,
            side,
            leading_header,
        } => classic(data, *mask, *side, *leading_header),
        Rep::Argb(data, side) => argb(data, *side),
    }
}

/// A 24-bit `.icns` entry and its separate mask.
///
/// The three colour planes are stored one after another in a single run-length
/// stream, so one unpack of `3 * pixels` bytes covers all of them. A missing
/// mask is opaque rather than an error: the mask is a separate entry and an
/// encoder is free to leave it out.
fn classic(data: &[u8], mask: Option<&[u8]>, side: u32, leading_header: bool) -> Option<Bitmap> {
    let pixels = (side as usize).checked_mul(side as usize)?;
    // The 128-pixel entry begins with four zero bytes that are not part of the
    // stream.
    let data = match leading_header && data.starts_with(&[0, 0, 0, 0]) {
        true => &data[4..],
        false => data,
    };
    let planes = match data.len() == pixels * 3 {
        true => data.to_vec(),
        false => unpack(data, pixels * 3)?,
    };

    let mut out = Bitmap::new(side, side);
    for i in 0..pixels {
        out.pixels[i * 4] = planes[i];
        out.pixels[i * 4 + 1] = planes[pixels + i];
        out.pixels[i * 4 + 2] = planes[pixels * 2 + i];
        out.pixels[i * 4 + 3] = mask.and_then(|m| m.get(i).copied()).unwrap_or(0xff);
    }
    Some(out)
}

/// An `ARGB` entry: four run-length planes, alpha first.
fn argb(data: &[u8], side: u32) -> Option<Bitmap> {
    let pixels = (side as usize).checked_mul(side as usize)?;
    let planes = unpack(data, pixels * 4)?;
    let mut out = Bitmap::new(side, side);
    for i in 0..pixels {
        out.pixels[i * 4] = planes[pixels + i];
        out.pixels[i * 4 + 1] = planes[pixels * 2 + i];
        out.pixels[i * 4 + 2] = planes[pixels * 3 + i];
        out.pixels[i * 4 + 3] = planes[i];
    }
    Some(out)
}

/// The `.icns` run-length encoding: a control byte with the high bit set is a
/// run of `control - 125` copies of the byte after it, and one without is
/// `control + 1` literal bytes.
fn unpack(mut data: &[u8], length: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(length);
    while out.len() < length {
        let (&control, rest) = data.split_first()?;
        data = rest;
        if control & 0x80 != 0 {
            let (&value, rest) = data.split_first()?;
            data = rest;
            out.extend(std::iter::repeat_n(value, control as usize - 125));
        } else {
            let count = control as usize + 1;
            out.extend_from_slice(data.get(..count)?);
            data = &data[count..];
        }
    }
    out.truncate(length);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Bitmap {
        let mut bitmap = Bitmap::new(3, 2);
        for (i, byte) in bitmap.pixels.iter_mut().enumerate() {
            *byte = i as u8;
        }
        bitmap
    }

    /// The encoder and the decoder are each other's inverse, which is the whole
    /// claim: `-TIFFRepresentation` is a round trip through this pair, because
    /// the guest hands the bytes straight back to `NSBitmapImageRep`.
    #[test]
    fn a_tiff_round_trips() {
        let original = sample();
        let encoded = encode_tiff(&original);
        assert_eq!(decode(&encoded).as_ref(), Some(&original));
    }

    /// A BMP's rows are stored bottom-up when its height is positive, and the
    /// decoder has to put them back the other way.
    #[test]
    fn a_bottom_up_bmp_comes_back_the_right_way_up() {
        // Two rows of one pixel, 24-bit: each row is one pixel plus padding to
        // four bytes. The first row stored is the *bottom* one.
        let mut file = b"BM".to_vec();
        file.resize(10, 0);
        file.extend_from_slice(&54u32.to_le_bytes()); // pixel offset
        file.extend_from_slice(&40u32.to_le_bytes()); // header size
        file.extend_from_slice(&1i32.to_le_bytes()); // width
        file.extend_from_slice(&2i32.to_le_bytes()); // height, positive
        file.extend_from_slice(&1u16.to_le_bytes()); // planes
        file.extend_from_slice(&24u16.to_le_bytes()); // bits
        file.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        file.resize(54, 0);
        // Bottom row blue, top row red - written bottom first.
        file.extend_from_slice(&[0xff, 0x00, 0x00, 0]);
        file.extend_from_slice(&[0x00, 0x00, 0xff, 0]);

        let decoded = decode(&file).expect("a 24-bit BMP");
        assert_eq!(decoded.width, 1);
        assert_eq!(decoded.height, 2);
        // Row 0 is the top row, which is the one stored last: red.
        assert_eq!(&decoded.pixels[0..4], &[0xff, 0x00, 0x00, 0xff]);
        assert_eq!(&decoded.pixels[4..8], &[0x00, 0x00, 0xff, 0xff]);
    }

    /// A top-down BMP says so with a negative height, and is not flipped twice.
    #[test]
    fn a_top_down_bmp_is_not_flipped() {
        let mut file = b"BM".to_vec();
        file.resize(10, 0);
        file.extend_from_slice(&54u32.to_le_bytes());
        file.extend_from_slice(&40u32.to_le_bytes());
        file.extend_from_slice(&1i32.to_le_bytes());
        file.extend_from_slice(&(-2i32).to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&24u16.to_le_bytes());
        file.extend_from_slice(&0u32.to_le_bytes());
        file.resize(54, 0);
        file.extend_from_slice(&[0x00, 0x00, 0xff, 0]);
        file.extend_from_slice(&[0xff, 0x00, 0x00, 0]);

        let decoded = decode(&file).expect("a top-down BMP");
        assert_eq!(&decoded.pixels[0..4], &[0xff, 0x00, 0x00, 0xff]);
    }

    /// A zlib stream of one stored block, so a test can write a PNG without a
    /// compressor.
    fn stored(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01, 0x01];
        out.extend_from_slice(&(data.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(data.len() as u16)).to_le_bytes());
        out.extend_from_slice(data);
        let (mut a, mut b) = (1u32, 0u32);
        for byte in data {
            a = (a + *byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        out.extend_from_slice(&((b << 16) | a).to_be_bytes());
        out
    }

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        // The CRC, which the decoder does not check.
        out.extend_from_slice(&[0; 4]);
        out
    }

    /// A PNG of `width * height` RGBA rows, each with the filter byte in front.
    fn png_file(width: u32, height: u32, colour: u8, rows: &[u8]) -> Vec<u8> {
        let mut header = width.to_be_bytes().to_vec();
        header.extend_from_slice(&height.to_be_bytes());
        header.extend_from_slice(&[8, colour, 0, 0, 0]);

        let mut file = PNG_SIGNATURE.to_vec();
        file.extend_from_slice(&chunk(b"IHDR", &header));
        file.extend_from_slice(&chunk(b"IDAT", &stored(rows)));
        file.extend_from_slice(&chunk(b"IEND", &[]));
        file
    }

    /// The unfiltering is the whole of a PNG decoder that can be wrong without
    /// failing, so the two rows here are filtered differently: `Sub` predicts
    /// from the pixel to the left and `Up` from the row above.
    #[test]
    fn a_filtered_png_comes_back_unfiltered() {
        let rows = [
            // Sub: red, then a delta of one per channel.
            1, 10, 20, 30, 40, 1, 1, 1, 1, //
            // Up: a delta of two per channel against the row above.
            2, 2, 2, 2, 2, 2, 2, 2, 2,
        ];
        let decoded = decode(&png_file(2, 2, 6, &rows)).expect("an RGBA PNG");
        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 2);
        assert_eq!(&decoded.pixels[0..8], &[10, 20, 30, 40, 11, 21, 31, 41]);
        assert_eq!(&decoded.pixels[8..16], &[12, 22, 32, 42, 13, 23, 33, 43]);
    }

    /// A colour type with no alpha channel is opaque rather than transparent,
    /// which is the difference between an icon and an empty rectangle.
    #[test]
    fn a_png_without_alpha_is_opaque() {
        let decoded = decode(&png_file(1, 1, 2, &[0, 1, 2, 3])).expect("an RGB PNG");
        assert_eq!(&decoded.pixels[..], &[1, 2, 3, 0xff]);
    }

    /// Sixteen-bit samples would have to be halved and Adam7 reassembled, so
    /// both are declined - a decoder that ignored either would answer with a
    /// plausible wrong image.
    #[test]
    fn a_png_this_decoder_cannot_read_is_declined() {
        let mut deep = png_file(1, 1, 6, &[0; 9]);
        deep[24] = 16;
        assert_eq!(decode(&deep), None);

        let mut interlaced = png_file(1, 1, 6, &[0; 5]);
        interlaced[28] = 1;
        assert_eq!(decode(&interlaced), None);
    }

    fn icns_file(entries: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (kind, data) in entries {
            body.extend_from_slice(*kind);
            body.extend_from_slice(&(data.len() as u32 + 8).to_be_bytes());
            body.extend_from_slice(data);
        }
        let mut file = b"icns".to_vec();
        file.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
        file.extend_from_slice(&body);
        file
    }

    /// The classic entries, which are all a bundle predating retina icons has:
    /// three colour planes in one run-length stream, and the alpha in a
    /// separate entry.
    #[test]
    fn a_classic_icns_entry_is_read_with_its_mask() {
        // Two runs of a hundred and twenty-eight per plane - the encoding's
        // longest run is a hundred and thirty - red, then green, then blue.
        let mut runs = Vec::new();
        for value in [0x10, 0x20, 0x30] {
            runs.extend_from_slice(&[128 + 125, value, 128 + 125, value]);
        }
        let icns = icns_file(&[(b"is32", runs), (b"s8mk", vec![0x40; 16 * 16])]);
        let decoded = decode(&icns).expect("a classic icns");
        assert_eq!((decoded.width, decoded.height), (16, 16));
        assert!(
            decoded
                .pixels
                .chunks(4)
                .all(|p| p == [0x10, 0x20, 0x30, 0x40])
        );
    }

    /// The entry a window wants is the largest one that is not extravagant: a
    /// bundle carrying a 1024-pixel icon is asking for four megabytes to draw
    /// something the window server will scale down anyway.
    #[test]
    fn the_largest_icns_entry_under_the_limit_wins() {
        let small = png_file(32, 32, 6, &vec![0; (32 * 4 + 1) * 32]);
        // Its pixels are never inflated, because its header is enough to rule
        // it out - which is the point of choosing before decoding.
        let large = png_file(1024, 1024, 6, &[]);
        let decoded =
            decode(&icns_file(&[(b"ic11", small), (b"ic10", large)])).expect("an icns of two PNGs");
        assert_eq!(decoded.width, 32);
    }

    /// A compression this module cannot read is declined rather than read as if
    /// it were not compressed.
    #[test]
    fn a_compressed_bmp_is_declined() {
        let mut file = b"BM".to_vec();
        file.resize(10, 0);
        file.extend_from_slice(&54u32.to_le_bytes());
        file.extend_from_slice(&40u32.to_le_bytes());
        file.extend_from_slice(&1i32.to_le_bytes());
        file.extend_from_slice(&1i32.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&24u16.to_le_bytes());
        // `BI_BITFIELDS`.
        file.extend_from_slice(&3u32.to_le_bytes());
        file.resize(64, 0);
        assert_eq!(decode(&file), None);
    }
}
