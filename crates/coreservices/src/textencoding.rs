//! `TextEncoding` arithmetic, the Script Manager's upgrade path to it, and the
//! Text Encoding Converter (`TEC*`) that moves bytes between two of them.
//!
//! `TextEncoding` and `CFStringEncoding` share Apple's numbering for every
//! encoding this tree answers - see `corefoundation::string::encoding` - so the
//! conversions here work directly in that space rather than keeping a second
//! table.

use std::ffi::c_void;

use corefoundation::string::encoding;

type OSStatus = i32;

const NO_ERR: OSStatus = 0;
const UNSUPPORTED_ENCODING: OSStatus = -8738;
const MALFORMED_INPUT: OSStatus = -8739;
const NO_CONVERSION_PATH: OSStatus = -8749;
const PARTIAL_CHAR: OSStatus = -8753;
const UNMAPPABLE: OSStatus = -8754;
const PARAM_ERR: OSStatus = -50;
/// `kTECOutputBufferFullStatus`: there was more to convert than `outputBuffer`
/// could hold, and what fits has been written.
const OUTPUT_BUFFER_FULL: OSStatus = -8785;

/// `TextEncoding CreateTextEncoding(TextEncodingBase, TextEncodingVariant,
/// TextEncodingFormat)`.
///
/// The format starts at bit 26, not bit 24. For example, Unicode format 2
/// (UTF-8) must produce 0x08000100, the same identifier CoreFoundation uses.
pub unsafe extern "C" fn CreateTextEncoding(base: u32, variant: u32, format: u32) -> u32 {
    create_text_encoding(base, variant, format)
}

fn create_text_encoding(base: u32, variant: u32, format: u32) -> u32 {
    (base & 0xffff) | ((variant & 0x3ff) << 16) | ((format & 0x3f) << 26)
}

/// `Fixed Long2Fix(long)`.
///
/// A `Fixed` is 16.16 fixed point, so this is the integer in the top half and
/// zero below it - the same wraparound on overflow Apple's own macro has,
/// since neither clamps.
pub unsafe extern "C" fn Long2Fix(the_long: i32) -> i32 {
    the_long.wrapping_mul(0x1_0000)
}

/// `OSStatus UpgradeScriptInfoToTextEncoding(ScriptCode scriptCode, LangCode
/// langCode, RegionCode regionCode, ConstStr255Param scriptName, TextEncoding
/// *textEncoding)`
///
/// The Script Manager's thirty-three base scripts (`smRoman` through
/// `smUninterp`) are numbered exactly as the `kTextEncodingMacXXX` bases they
/// upgrade to, so a script code in that range is the base encoding itself.
///
/// FIXME (ACCURACY): `langCode` and `regionCode` can steer Apple toward a more
/// specific *variant* of the same base (a different 8-bit table for the one
/// script) when the base alone is ambiguous; that steering is not modeled, so
/// every script answers its default variant and format. `scriptName` is a
/// fallback used only when `scriptCode` is `smScriptUnspecified` or similarly
/// unusable, which this tree never receives from its own callers and does not
/// parse.
///
/// # Safety
///
/// `text_encoding` is null or writable for a `TextEncoding`.
pub unsafe extern "C" fn UpgradeScriptInfoToTextEncoding(
    script_code: i16,
    lang_code: i16,
    _region_code: i16,
    _script_name: *const u8,
    text_encoding: *mut u32,
) -> OSStatus {
    if text_encoding.is_null() {
        return PARAM_ERR;
    }
    let base: u16 = match script_code {
        -1 | -2 => 0, // system and current script on this English US system
        -128 => match lang_code {
            11 => 1, // langJapanese
            -128 | 0 => 0,
            _ => return UNSUPPORTED_ENCODING,
        },
        0..=32 => script_code as u16,
        _ => return PARAM_ERR,
    };
    // SAFETY: checked non-null above.
    unsafe { *text_encoding = create_text_encoding(u32::from(base), 0, 0) };
    NO_ERR
}

/// A Pascal string's text: the first byte is the length, the rest are its
/// characters in Mac OS Roman, which is how the Script Manager spelled a
/// charset or script name.
///
/// # Safety
///
/// `pascal` is null or a byte, then that many more.
unsafe fn pascal_string(pascal: *const u8) -> Option<String> {
    if pascal.is_null() {
        return None;
    }
    // SAFETY: the caller states at least one byte is readable.
    let length = usize::from(unsafe { *pascal });
    // SAFETY: the caller states `length` more bytes follow.
    let bytes = unsafe { std::slice::from_raw_parts(pascal.add(1), length) };
    let units: Vec<u16> = bytes
        .iter()
        .filter_map(|b| corefoundation::eightbit::decode(*b, encoding::MAC_ROMAN))
        .collect();
    if units.len() != bytes.len() {
        return None;
    }
    String::from_utf16(&units).ok()
}

/// `OSStatus TECGetTextEncodingFromInternetName(TextEncoding
/// *encoding, ConstStr255Param internetName)`
///
/// # Safety
///
/// `text_encoding` is null or writable for a `TextEncoding`; `internet_name`
/// is a Pascal string.
pub unsafe extern "C" fn TECGetTextEncodingFromInternetName(
    text_encoding: *mut u32,
    internet_name: *const u8,
) -> OSStatus {
    if text_encoding.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: a Pascal string, by the caller's contract.
    let Some(name) = (unsafe { pascal_string(internet_name) }) else {
        return PARAM_ERR;
    };
    let Some(found) = corefoundation::api::encoding_for_charset_name(&name) else {
        return UNSUPPORTED_ENCODING;
    };
    // SAFETY: checked non-null above.
    unsafe { *text_encoding = found };
    NO_ERR
}

/// A live `TECObjectRef`: the two encodings a conversion moves between.
struct Converter {
    input: u32,
    output: u32,
    // Once a BOM has selected the order, continuation buffers retain it.
    order: std::sync::Mutex<Option<u32>>,
}

fn supported(code: u32) -> Option<u32> {
    let code = if code & 0xff00 == 0x0100 {
        (code & 0xfc00_0000) | 0x0100
    } else {
        code
    };
    (code == encoding::UTF8
        || corefoundation::eightbit::is_table(code)
        || corefoundation::string::is_utf16(code)
        || corefoundation::string::is_utf32(code))
    .then_some(code)
}

/// `OSStatus TECCreateConverter(TECObjectRef *tec, TextEncoding
/// inputEncoding, TextEncoding outputEncoding)`
///
/// # Safety
///
/// `tec` is writable for a `TECObjectRef`.
pub unsafe extern "C" fn TECCreateConverter(
    tec: *mut *mut c_void,
    input_encoding: u32,
    output_encoding: u32,
) -> OSStatus {
    if tec.is_null() {
        return PARAM_ERR;
    }
    unsafe { *tec = std::ptr::null_mut() };
    let (Some(input_encoding), Some(output_encoding)) =
        (supported(input_encoding), supported(output_encoding))
    else {
        return NO_CONVERSION_PATH;
    };
    let converter = Box::into_raw(Box::new(Converter {
        input: input_encoding,
        output: output_encoding,
        order: Default::default(),
    }));
    // SAFETY: checked non-null above.
    unsafe { *tec = converter.cast() };
    NO_ERR
}

/// `OSStatus TECDisposeConverter(TECObjectRef tec)`
///
/// # Safety
///
/// `tec` is null or a `TECObjectRef` [`TECCreateConverter`] made and that has
/// not already been disposed.
pub unsafe extern "C" fn TECDisposeConverter(tec: *mut c_void) -> OSStatus {
    if tec.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller's contract.
    drop(unsafe { Box::from_raw(tec.cast::<Converter>()) });
    NO_ERR
}

/// `OSStatus TECClearConverterContextInfo(TECObjectRef tec)`
///
/// Start the next stream with its original encoding and BOM detection.
///
/// # Safety
///
/// `tec` is null or live.
pub unsafe extern "C" fn TECClearConverterContextInfo(tec: *mut c_void) -> OSStatus {
    if tec.is_null() {
        return PARAM_ERR;
    }
    *unsafe { &*tec.cast::<Converter>() }.order.lock().unwrap() = None;
    NO_ERR
}

fn decode_one(bytes: &[u8], code: u32) -> Result<(char, usize), OSStatus> {
    if code == encoding::UTF8 {
        let width = match bytes[0] {
            0..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return Err(MALFORMED_INPUT),
        };
        let text = std::str::from_utf8(&bytes[..bytes.len().min(width)]).map_err(|e| {
            if e.error_len().is_none() {
                PARTIAL_CHAR
            } else {
                MALFORMED_INPUT
            }
        })?;
        return Ok((text.chars().next().unwrap(), width));
    }
    if corefoundation::string::is_utf16(code) {
        let read = |at: usize| -> Result<u16, OSStatus> {
            let pair: [u8; 2] = bytes
                .get(at..at + 2)
                .ok_or(PARTIAL_CHAR)?
                .try_into()
                .unwrap();
            Ok(if code == encoding::UTF16_BE {
                u16::from_be_bytes(pair)
            } else {
                u16::from_le_bytes(pair)
            })
        };
        let high = read(0)?;
        if (0xd800..=0xdbff).contains(&high) {
            let low = read(2)?;
            let ch = char::decode_utf16([high, low])
                .next()
                .unwrap()
                .map_err(|_| MALFORMED_INPUT)?;
            return Ok((ch, 4));
        }
        return char::from_u32(high.into())
            .map(|ch| (ch, 2))
            .ok_or(MALFORMED_INPUT);
    }
    if corefoundation::string::is_utf32(code) {
        let quad = bytes.get(..4).ok_or(PARTIAL_CHAR)?.try_into().unwrap();
        let value = if code == encoding::UTF32_BE {
            u32::from_be_bytes(quad)
        } else {
            u32::from_le_bytes(quad)
        };
        return char::from_u32(value)
            .map(|ch| (ch, 4))
            .ok_or(MALFORMED_INPUT);
    }
    corefoundation::eightbit::decode(bytes[0], code)
        .and_then(|u| char::from_u32(u.into()))
        .map(|ch| (ch, 1))
        .ok_or(MALFORMED_INPUT)
}

/// `ch` as bytes under `code`.
fn encode_one(ch: char, code: u32) -> Result<Vec<u8>, OSStatus> {
    if code == encoding::UTF8 {
        return Ok(ch.to_string().into_bytes());
    }
    if corefoundation::eightbit::is_table(code) {
        let unit = u16::try_from(ch as u32).ok();
        let byte = unit.and_then(|u| corefoundation::eightbit::encode(u, code));
        return byte.map(|b| vec![b]).ok_or(UNMAPPABLE);
    }
    match code {
        encoding::UTF16 | encoding::UTF16_BE | encoding::UTF16_LE => {
            let mut units = [0_u16; 2];
            Ok(ch
                .encode_utf16(&mut units)
                .iter()
                .flat_map(|unit| match code == encoding::UTF16_BE {
                    true => unit.to_be_bytes(),
                    false => unit.to_le_bytes(),
                })
                .collect())
        }
        encoding::UTF32 | encoding::UTF32_BE | encoding::UTF32_LE => {
            let scalar = ch as u32;
            Ok(match code == encoding::UTF32_BE {
                true => scalar.to_be_bytes().to_vec(),
                false => scalar.to_le_bytes().to_vec(),
            })
        }
        _ => Err(NO_CONVERSION_PATH),
    }
}

/// The character-at-a-time core [`TECConvertText`], [`ConvertFromTextToUnicode`]
/// and [`ConvertFromUnicodeToText`] all share: convert as much of `input` as
/// fits in `output_buffer_length` bytes, stopping and reporting
/// [`OUTPUT_BUFFER_FULL`] the moment one more character would not fit - never
/// writing past that length, and never reporting a character consumed whose
/// output was not written. Returns `(status, bytes of input consumed, bytes of
/// output produced)`.
///
/// # Safety
///
/// `output_buffer` is null or writable for `output_buffer_length` bytes.
unsafe fn convert_core(
    input: &[u8],
    converter: &Converter,
    output_buffer: *mut u8,
    output_buffer_length: usize,
) -> (OSStatus, usize, usize) {
    let mut status = NO_ERR;
    let mut order = converter.order.lock().unwrap();
    let (input_code, mut consumed) = match *order {
        Some(code) => (code, 0),
        None => corefoundation::string::stream_encoding(input, converter.input),
    };
    let min_width = match converter.input {
        encoding::UTF16 => 2,
        encoding::UTF32 => 4,
        _ => 1,
    };
    if input.len() >= min_width {
        *order = Some(input_code);
    }
    let (output_code, _) = corefoundation::string::stream_encoding(&[], converter.output);
    let mut produced = 0_usize;
    while consumed < input.len() {
        let (ch, used) = match decode_one(&input[consumed..], input_code) {
            Ok(pair) => pair,
            Err(err) => return (err, consumed, produced),
        };
        let bytes = match encode_one(ch, output_code) {
            Ok(bytes) => bytes,
            Err(err) => return (err, consumed, produced),
        };
        if produced + bytes.len() > output_buffer_length {
            status = OUTPUT_BUFFER_FULL;
            break;
        }
        if !output_buffer.is_null() {
            // SAFETY: the capacity check above, and the caller's contract.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    output_buffer.add(produced),
                    bytes.len(),
                )
            };
        }
        produced += bytes.len();
        consumed += used;
    }
    (status, consumed, produced)
}

/// `OSStatus TECConvertText(TECObjectRef tec, const UInt8 *inputBuffer,
/// ByteCount inputBufferLength, ByteCount *actualInputLength, UInt8
/// *outputBuffer, ByteCount outputBufferLength, ByteCount *actualOutputLength)`
///
/// Converts whole characters at a time, stopping and reporting
/// [`OUTPUT_BUFFER_FULL`] the moment one more would not fit - never writing
/// past `outputBufferLength`, and never reporting a character consumed whose
/// output was not written.
///
/// # Safety
///
/// `tec` is a live `TECObjectRef`; `input_buffer` is null or readable for
/// `input_buffer_length` bytes; `output_buffer` is null or writable for
/// `output_buffer_length` bytes; the two actual-length outputs are null or
/// writable for a `ByteCount`.
pub unsafe extern "C" fn TECConvertText(
    tec: *mut c_void,
    input_buffer: *const u8,
    input_buffer_length: u64,
    actual_input_length: *mut u64,
    output_buffer: *mut u8,
    output_buffer_length: u64,
    actual_output_length: *mut u64,
) -> OSStatus {
    if tec.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: a converter `TECCreateConverter` made and not yet disposed.
    let converter = unsafe { &*tec.cast::<Converter>() };
    let input = match input_buffer.is_null() {
        true => &[][..],
        // SAFETY: the caller's contract.
        false => unsafe { std::slice::from_raw_parts(input_buffer, input_buffer_length as usize) },
    };
    // SAFETY: the caller's contract on `output_buffer`/`output_buffer_length`.
    let (status, consumed, produced) = unsafe {
        convert_core(
            input,
            converter,
            output_buffer,
            output_buffer_length as usize,
        )
    };
    if !actual_input_length.is_null() {
        // SAFETY: the caller's contract.
        unsafe { *actual_input_length = consumed as u64 };
    }
    if !actual_output_length.is_null() {
        unsafe { *actual_output_length = produced as u64 };
    }
    status
}

/// `TextEncodingBase GetTextEncodingBase(TextEncoding encoding)`.
///
/// The low sixteen bits [`create_text_encoding`] packs the base into - the
/// inverse half of that packing, not a lookup of anything.
pub unsafe extern "C" fn GetTextEncodingBase(encoding: u32) -> u32 {
    encoding & 0xffff
}

/// `smSingleByte`: every character [`CharacterByteType`] is ever asked about
/// is one, since nothing in this tree implements a double-byte script's
/// encoding (`smDoubleByte` answers "none installed" for the same reason - see
/// `GetScriptManagerVariable`).
const SM_SINGLE_BYTE: i16 = 0;

/// `short CharacterByteType(Ptr textBuf, short textOffset, ScriptCode script)`.
///
/// `textBuf`/`textOffset` name the character being asked about; neither is
/// read, because the answer does not depend on them here - see
/// [`SM_SINGLE_BYTE`].
pub unsafe extern "C" fn CharacterByteType(
    _text_buf: *const u8,
    _text_offset: i16,
    _script: i16,
) -> i16 {
    SM_SINGLE_BYTE
}

/// `void NumToString(long theNum, Str255 theString)`.
///
/// `Str255` is a Pascal string: the first byte is the length, the rest its
/// characters - plain ASCII digits and an optional leading `-` here, which is
/// always representable regardless of the caller's encoding.
///
/// # Safety
///
/// `the_string` is writable for 256 bytes.
pub unsafe extern "C" fn NumToString(the_num: i64, the_string: *mut u8) {
    if the_string.is_null() {
        return;
    }
    let digits = the_num.to_string();
    // A `Str255` cannot hold more than 255 characters, which no `i64` decimal
    // ever reaches, so the truncation this would otherwise need never fires.
    let len = digits.len().min(255);
    // SAFETY: the caller states 256 writable bytes.
    unsafe {
        the_string.write(len as u8);
        std::ptr::copy_nonoverlapping(digits.as_ptr(), the_string.add(1), len);
    }
}

/// `smSysScript`: the System script, which this tree answers like an English
/// US Mac - Roman, the same base [`UpgradeScriptInfoToTextEncoding`] gives
/// `smRoman`.
const SM_SYS_SCRIPT: i16 = 18;
/// `smRegionCode`: the current region code.
const SM_REGION_CODE: i16 = 40;
/// `smRoman`, the value both a script code and `smSysScript`'s answer share.
const SM_ROMAN: i64 = 0;
/// `verUS`, the region code an English US system answers.
const VER_US: i64 = 0;
/// `smBadVerb`: a selector this tree does not answer.
const SM_BAD_VERB: i64 = -1;

/// `long GetScriptManagerVariable(short selector)`.
///
/// Only the two selectors a caller can still get real information from on a
/// Unicode system - see `TextCommon.h`'s deprecation note - are answered;
/// every other selector is Script Manager bookkeeping this tree never had, so
/// [`SM_BAD_VERB`] says the verb is not one of these two rather than guessing
/// at a classic global that was never allocated.
pub unsafe extern "C" fn GetScriptManagerVariable(selector: i16) -> i64 {
    match selector {
        SM_SYS_SCRIPT => SM_ROMAN,
        SM_REGION_CODE => VER_US,
        _ => SM_BAD_VERB,
    }
}

/// `UnicodeMapping { TextEncoding unicodeEncoding; TextEncoding otherEncoding;
/// UnicodeMapVersion mappingVersion; }`, laid out exactly as the guest's.
#[repr(C)]
pub struct UnicodeMapping {
    unicode_encoding: u32,
    other_encoding: u32,
    mapping_version: i32,
}

/// The body of [`CreateTextToUnicodeInfo`] and [`CreateUnicodeToTextInfo`]:
/// The Unicode buffer consists of native-order UniChar values.
///
/// # Safety
///
/// `mapping` is null or readable for a `UnicodeMapping`; `out` is null or
/// writable for a pointer.
unsafe fn create_legacy_info(
    mapping: *const UnicodeMapping,
    out: *mut *mut c_void,
    to_unicode: bool,
) -> OSStatus {
    if mapping.is_null() || out.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller's contract.
    let mapping = unsafe { &*mapping };
    let other = mapping.other_encoding;
    let native = if cfg!(target_endian = "little") {
        encoding::UTF16_LE
    } else {
        encoding::UTF16_BE
    };
    match supported(mapping.unicode_encoding) {
        Some(code) if code == encoding::UTF16 || code == native => (),
        _ => {
            unsafe { *out = std::ptr::null_mut() };
            return NO_CONVERSION_PATH;
        }
    }
    let (input, output) = if to_unicode {
        (other, native)
    } else {
        (native, other)
    };
    unsafe { TECCreateConverter(out, input, output) }
}

/// `OSStatus CreateTextToUnicodeInfo(ConstUnicodeMappingPtr iUnicodeMapping,
/// TextToUnicodeInfo *oTextToUnicodeInfo)`
///
/// # Safety
///
/// As [`create_legacy_info`].
pub unsafe extern "C" fn CreateTextToUnicodeInfo(
    mapping: *const UnicodeMapping,
    out: *mut *mut c_void,
) -> OSStatus {
    unsafe { create_legacy_info(mapping, out, true) }
}

/// `OSStatus CreateUnicodeToTextInfo(ConstUnicodeMappingPtr iUnicodeMapping,
/// UnicodeToTextInfo *oUnicodeToTextInfo)`
///
/// # Safety
///
/// As [`create_legacy_info`].
pub unsafe extern "C" fn CreateUnicodeToTextInfo(
    mapping: *const UnicodeMapping,
    out: *mut *mut c_void,
) -> OSStatus {
    unsafe { create_legacy_info(mapping, out, false) }
}

/// The body of [`DisposeTextToUnicodeInfo`] and [`DisposeUnicodeToTextInfo`]:
/// both take a pointer to the opaque reference, free what it points to, and
/// null the caller's copy - unlike [`TECDisposeConverter`], which takes the
/// reference itself.
///
/// # Safety
///
/// `info` is null or a writable pointer to a live [`Converter`] this module
/// made, not yet disposed.
unsafe fn dispose_legacy_info(info: *mut *mut c_void) -> OSStatus {
    if info.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller's contract.
    let ptr = unsafe { *info };
    if !ptr.is_null() {
        // SAFETY: a `Converter` this module boxed, not yet freed.
        drop(unsafe { Box::from_raw(ptr.cast::<Converter>()) });
    }
    // SAFETY: checked non-null above.
    unsafe { *info = std::ptr::null_mut() };
    NO_ERR
}

/// `OSStatus DisposeTextToUnicodeInfo(TextToUnicodeInfo *ioTextToUnicodeInfo)`
///
/// # Safety
///
/// As [`dispose_legacy_info`].
pub unsafe extern "C" fn DisposeTextToUnicodeInfo(info: *mut *mut c_void) -> OSStatus {
    unsafe { dispose_legacy_info(info) }
}

/// `OSStatus DisposeUnicodeToTextInfo(UnicodeToTextInfo *ioUnicodeToTextInfo)`
///
/// # Safety
///
/// As [`dispose_legacy_info`].
pub unsafe extern "C" fn DisposeUnicodeToTextInfo(info: *mut *mut c_void) -> OSStatus {
    unsafe { dispose_legacy_info(info) }
}

/// `OSStatus ConvertFromTextToUnicode(TextToUnicodeInfo iTextToUnicodeInfo,
/// ByteCount iSourceLen, ConstLogicalAddress iSourceStr, OptionBits
/// iControlFlags, ItemCount iOffsetCount, const ByteOffset iOffsetArray[],
/// ItemCount *oOffsetCount, ByteOffset oOffsetArray[], ByteCount
/// iOutputBufLen, ByteCount *oSourceRead, ByteCount *oUnicodeLen, UniChar
/// oUnicodeStr[])`
///
/// FIXME (ACCURACY): the offset-mapping arrays (`iOffsetArray`/`oOffsetArray`)
/// are not produced - `oOffsetCount` is set to 0 when the caller asked for
/// them, which is honest about not tracking them rather than a guess at where
/// they would have landed.
///
/// # Safety
///
/// `info` is a live `TextToUnicodeInfo` this module made; `source` is null or
/// readable for `source_len` bytes; `unicode_str` is null or writable for
/// `output_buf_len` bytes; the `ItemCount`/`ByteCount` out-parameters are null
/// or writable.
pub unsafe extern "C" fn ConvertFromTextToUnicode(
    info: *mut c_void,
    source_len: u64,
    source: *const u8,
    _control_flags: u32,
    _offset_count: u64,
    _offset_array: *const u32,
    out_offset_count: *mut u64,
    _out_offset_array: *mut u32,
    output_buf_len: u64,
    source_read: *mut u64,
    unicode_len: *mut u64,
    unicode_str: *mut u16,
) -> OSStatus {
    if info.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: a `Converter` this module made.
    let converter = unsafe { &*info.cast::<Converter>() };
    let input = match source.is_null() {
        true => &[][..],
        // SAFETY: the caller's contract.
        false => unsafe { std::slice::from_raw_parts(source, source_len as usize) },
    };
    // SAFETY: the caller's contract on `unicode_str`/`output_buf_len`.
    let (status, consumed, produced) = unsafe {
        convert_core(
            input,
            converter,
            unicode_str.cast(),
            output_buf_len as usize,
        )
    };
    if !out_offset_count.is_null() {
        // SAFETY: the caller's contract.
        unsafe { *out_offset_count = 0 };
    }
    if !source_read.is_null() {
        unsafe { *source_read = consumed as u64 };
    }
    if !unicode_len.is_null() {
        unsafe { *unicode_len = produced as u64 };
    }
    status
}

/// `OSStatus ConvertFromUnicodeToText(UnicodeToTextInfo iUnicodeToTextInfo,
/// ByteCount iUnicodeLen, const UniChar iUnicodeStr[], OptionBits
/// iControlFlags, ItemCount iOffsetCount, const ByteOffset iOffsetArray[],
/// ItemCount *oOffsetCount, ByteOffset oOffsetArray[], ByteCount
/// iOutputBufLen, ByteCount *oInputRead, ByteCount *oOutputLen, LogicalAddress
/// oOutputStr)`
///
/// As [`ConvertFromTextToUnicode`], the other direction - see its FIXME about
/// the offset-mapping arrays.
///
/// # Safety
///
/// `info` is a live `UnicodeToTextInfo` this module made; `unicode_str` is
/// null or readable for `unicode_len` bytes (not `UniChar` units - the type
/// Apple gives `iUnicodeLen` is `ByteCount`); `output_str` is null or writable
/// for `output_buf_len` bytes; the `ItemCount`/`ByteCount` out-parameters are
/// null or writable.
pub unsafe extern "C" fn ConvertFromUnicodeToText(
    info: *mut c_void,
    unicode_len: u64,
    unicode_str: *const u16,
    _control_flags: u32,
    _offset_count: u64,
    _offset_array: *const u32,
    out_offset_count: *mut u64,
    _out_offset_array: *mut u32,
    output_buf_len: u64,
    input_read: *mut u64,
    output_len: *mut u64,
    output_str: *mut u8,
) -> OSStatus {
    if info.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: a `Converter` this module made.
    let converter = unsafe { &*info.cast::<Converter>() };
    let input = match unicode_str.is_null() {
        true => &[][..],
        // SAFETY: the caller's contract; read as bytes, since `unicode_len` is
        // already a `ByteCount`.
        false => unsafe {
            std::slice::from_raw_parts(unicode_str.cast::<u8>(), unicode_len as usize)
        },
    };
    // SAFETY: the caller's contract on `output_str`/`output_buf_len`.
    let (status, consumed, produced) =
        unsafe { convert_core(input, converter, output_str, output_buf_len as usize) };
    if !out_offset_count.is_null() {
        // SAFETY: the caller's contract.
        unsafe { *out_offset_count = 0 };
    }
    if !input_read.is_null() {
        unsafe { *input_read = consumed as u64 };
    }
    if !output_len.is_null() {
        unsafe { *output_len = produced as u64 };
    }
    status
}

exports::exports! {
    functions {
        "CreateTextEncoding" => CreateTextEncoding,
        "Long2Fix" => Long2Fix,
        "UpgradeScriptInfoToTextEncoding" => UpgradeScriptInfoToTextEncoding,
        "TECGetTextEncodingFromInternetName" => TECGetTextEncodingFromInternetName,
        "TECCreateConverter" => TECCreateConverter,
        "TECDisposeConverter" => TECDisposeConverter,
        "TECClearConverterContextInfo" => TECClearConverterContextInfo,
        "TECConvertText" => TECConvertText,
        "GetTextEncodingBase" => GetTextEncodingBase,
        "CharacterByteType" => CharacterByteType,
        "NumToString" => NumToString,
        "GetScriptManagerVariable" => GetScriptManagerVariable,
        "CreateTextToUnicodeInfo" => CreateTextToUnicodeInfo,
        "CreateUnicodeToTextInfo" => CreateUnicodeToTextInfo,
        "DisposeTextToUnicodeInfo" => DisposeTextToUnicodeInfo,
        "DisposeUnicodeToTextInfo" => DisposeUnicodeToTextInfo,
        "ConvertFromTextToUnicode" => ConvertFromTextToUnicode,
        "ConvertFromUnicodeToText" => ConvertFromUnicodeToText,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three pieces land where the bits of every encoding this tree
    /// already answers put them - see `corefoundation::string::encoding`.
    #[test]
    fn unsupported_paths_are_rejected_in_both_directions() {
        for (input, output) in [(1, encoding::UTF8), (encoding::UTF16, 0x0a01)] {
            let mut handle = std::ptr::null_mut();
            assert_eq!(
                unsafe { TECCreateConverter(&mut handle, input, output) },
                NO_CONVERSION_PATH
            );
            assert!(handle.is_null());
        }
        let mapping = UnicodeMapping {
            unicode_encoding: encoding::UTF16,
            other_encoding: 1,
            mapping_version: -1,
        };
        let mut handle = std::ptr::null_mut();
        assert_eq!(
            unsafe { CreateUnicodeToTextInfo(&mapping, &mut handle) },
            NO_CONVERSION_PATH
        );
        assert_eq!(
            unsafe { CreateTextToUnicodeInfo(&mapping, &mut handle) },
            NO_CONVERSION_PATH
        );
    }

    #[test]
    fn malformed_and_partial_input_leave_exact_retry_offsets() {
        let mut tec = std::ptr::null_mut();
        let versioned = create_text_encoding(0x0106, 0x300, 2);
        assert_eq!(versioned, 0x0b00_0106);
        unsafe {
            assert_eq!(
                TECCreateConverter(&mut tec, versioned, encoding::UTF8),
                NO_ERR
            );
            for (input, status) in [
                (&b"A\xe2\x82"[..], PARTIAL_CHAR),
                (&b"A\xe2X"[..], MALFORMED_INPUT),
            ] {
                let mut output = [0; 8];
                let (mut read, mut written) = (0, 0);
                assert_eq!(
                    TECConvertText(
                        tec,
                        input.as_ptr(),
                        input.len() as u64,
                        &mut read,
                        output.as_mut_ptr(),
                        8,
                        &mut written
                    ),
                    status
                );
                assert_eq!((read, written, output[0]), (1, 1, b'A'));
            }
            TECDisposeConverter(tec);
        }
    }

    #[test]
    fn bom_selects_order_across_buffers_and_clear_starts_a_new_stream() {
        for (code, width) in [(encoding::UTF16, 2), (encoding::UTF32, 4)] {
            let mut tec = std::ptr::null_mut();
            unsafe {
                assert_eq!(TECCreateConverter(&mut tec, code, encoding::UTF8), NO_ERR);
                let mut output = [0; 8];
                let (mut read, mut written) = (0, 0);
                let mut feed = |bytes: &[u8]| {
                    let status = TECConvertText(
                        tec,
                        bytes.as_ptr(),
                        bytes.len() as u64,
                        &mut read,
                        output.as_mut_ptr(),
                        8,
                        &mut written,
                    );
                    (status, read, output[..written as usize].to_vec())
                };
                let mark = &0xfeffu32.to_be_bytes()[4 - width..];
                assert_eq!(feed(&mark[..1]), (PARTIAL_CHAR, 0, vec![]));
                assert_eq!(feed(mark), (NO_ERR, width as u64, vec![]));
                assert_eq!(
                    feed(&65u32.to_be_bytes()[4 - width..]),
                    (NO_ERR, width as u64, vec![65])
                );
                TECClearConverterContextInfo(tec);
                assert_eq!(
                    feed(&0xfeffu32.to_le_bytes()[..width]),
                    (NO_ERR, width as u64, vec![])
                );
                assert_eq!(
                    feed(&66u32.to_le_bytes()[..width]),
                    (NO_ERR, width as u64, vec![66])
                );
                TECDisposeConverter(tec);
            }
        }
    }

    #[test]
    fn script_sentinels_select_the_system_or_language_script() {
        for (script, language, expected) in [(-1, -128, 0), (-2, -128, 0), (-128, 11, 1)] {
            let mut code = u32::MAX;
            assert_eq!(
                unsafe {
                    UpgradeScriptInfoToTextEncoding(
                        script,
                        language,
                        -128,
                        std::ptr::null(),
                        &mut code,
                    )
                },
                NO_ERR
            );
            assert_eq!(code, expected);
        }
    }

    #[test]
    fn create_text_encoding_matches_the_known_constants() {
        assert_eq!(create_text_encoding(0, 0, 0), encoding::MAC_ROMAN);
        assert_eq!(create_text_encoding(0x0100, 0, 2), encoding::UTF8);
        assert_eq!(create_text_encoding(0x0100, 0, 3), encoding::UTF32);
    }

    #[test]
    fn created_utf8_encoding_converts_xml_names_to_unicode() {
        let utf8 = unsafe { CreateTextEncoding(0x0100, 0, 2) };
        let utf16 = unsafe { CreateTextEncoding(0x0100, 0, 0) };
        let mut converter = std::ptr::null_mut();
        assert_eq!(
            unsafe { TECCreateConverter(&mut converter, utf8, utf16) },
            NO_ERR
        );
        let input = b"base";
        let mut output = [0_u16; 5];
        let (mut consumed, mut produced) = (0, 0);
        assert_eq!(
            unsafe {
                TECConvertText(
                    converter,
                    input.as_ptr(),
                    input.len() as u64,
                    &mut consumed,
                    output.as_mut_ptr().cast(),
                    8,
                    &mut produced,
                )
            },
            NO_ERR
        );
        assert_eq!(consumed, 4);
        assert_eq!(produced, 8);
        assert_eq!(output, [98, 97, 115, 101, 0]);
        unsafe { TECDisposeConverter(converter) };
    }

    #[test]
    fn long_2_fix_shifts_into_the_integer_half() {
        assert_eq!(unsafe { Long2Fix(1) }, 0x1_0000);
        assert_eq!(unsafe { Long2Fix(-1) }, -0x1_0000);
    }

    /// `smRoman` upgrades to Mac Roman, the base it is numbered the same as.
    #[test]
    fn upgrading_roman_gives_mac_roman() {
        let mut out = 0_u32;
        // SAFETY: storage for a `TextEncoding`.
        let status =
            unsafe { UpgradeScriptInfoToTextEncoding(0, 0, 0, std::ptr::null(), &mut out) };
        assert_eq!(status, NO_ERR);
        assert_eq!(out, encoding::MAC_ROMAN);
    }

    /// A script code past the Script Manager's own range is refused rather
    /// than guessed at.
    #[test]
    fn upgrading_an_unknown_script_is_refused() {
        let mut out = 0_u32;
        // SAFETY: storage for a `TextEncoding`.
        let status =
            unsafe { UpgradeScriptInfoToTextEncoding(100, 0, 0, std::ptr::null(), &mut out) };
        assert_eq!(status, PARAM_ERR);
    }

    fn pascal(text: &str) -> Vec<u8> {
        let mut out = vec![text.len() as u8];
        out.extend_from_slice(text.as_bytes());
        out
    }

    #[test]
    fn an_internet_name_resolves_to_its_encoding() {
        let name = pascal("UTF-8");
        let mut out = 0_u32;
        // SAFETY: a Pascal string this test owns, and storage for a `TextEncoding`.
        let status = unsafe { TECGetTextEncodingFromInternetName(&mut out, name.as_ptr()) };
        assert_eq!(status, NO_ERR);
        assert_eq!(out, encoding::UTF8);
    }

    #[test]
    fn an_unknown_internet_name_is_refused() {
        let name = pascal("no-such-charset");
        let mut out = 0_u32;
        // SAFETY: as above.
        let status = unsafe { TECGetTextEncodingFromInternetName(&mut out, name.as_ptr()) };
        assert_eq!(status, UNSUPPORTED_ENCODING);
    }

    /// ASCII text round-trips unchanged between any two of this tree's
    /// encodings, UTF-8 included.
    #[test]
    fn ascii_round_trips_through_every_encoding() {
        for (input_encoding, output_encoding) in [
            (encoding::UTF8, encoding::MAC_ROMAN),
            (encoding::MAC_ROMAN, encoding::UTF8),
            (encoding::UTF8, encoding::UTF16_BE),
            (encoding::UTF16_LE, encoding::UTF8),
        ] {
            let mut tec = std::ptr::null_mut();
            // SAFETY: storage for a `TECObjectRef`.
            assert_eq!(
                unsafe { TECCreateConverter(&mut tec, input_encoding, output_encoding) },
                NO_ERR
            );
            let message = b"Hello";
            let input: Vec<u8> = match input_encoding {
                encoding::UTF16_LE => message.iter().flat_map(|b| [*b, 0]).collect(),
                _ => message.to_vec(),
            };
            let mut output = [0_u8; 64];
            let (mut used_in, mut used_out) = (0_u64, 0_u64);
            // SAFETY: a live converter, the input above, and storage sized for it.
            let status = unsafe {
                TECConvertText(
                    tec,
                    input.as_ptr(),
                    input.len() as u64,
                    &mut used_in,
                    output.as_mut_ptr(),
                    output.len() as u64,
                    &mut used_out,
                )
            };
            assert_eq!(status, NO_ERR);
            assert_eq!(used_in, input.len() as u64);
            let produced: Vec<u8> = match output_encoding {
                encoding::UTF16_BE => message.iter().flat_map(|b| [0, *b]).collect(),
                _ => message.to_vec(),
            };
            assert_eq!(&output[..used_out as usize], produced.as_slice());
            // SAFETY: the same converter, not yet disposed.
            assert_eq!(unsafe { TECDisposeConverter(tec) }, NO_ERR);
        }
    }

    /// A buffer too small to hold the whole conversion stops rather than
    /// overruns, and reports exactly how far it got.
    #[test]
    fn a_full_output_buffer_stops_cleanly() {
        let mut tec = std::ptr::null_mut();
        // SAFETY: storage for a `TECObjectRef`.
        assert_eq!(
            unsafe { TECCreateConverter(&mut tec, encoding::UTF8, encoding::UTF8) },
            NO_ERR
        );
        let input = b"Hello";
        let mut output = [0_u8; 3];
        let (mut used_in, mut used_out) = (0_u64, 0_u64);
        // SAFETY: a live converter and a buffer shorter than the input.
        let status = unsafe {
            TECConvertText(
                tec,
                input.as_ptr(),
                input.len() as u64,
                &mut used_in,
                output.as_mut_ptr(),
                output.len() as u64,
                &mut used_out,
            )
        };
        assert_eq!(status, OUTPUT_BUFFER_FULL);
        assert_eq!(used_in, 3);
        assert_eq!(used_out, 3);
        assert_eq!(&output, b"Hel");
        // SAFETY: the same converter, not yet disposed.
        assert_eq!(unsafe { TECDisposeConverter(tec) }, NO_ERR);
    }

    /// The base is the low word of the packing `create_text_encoding` does.
    #[test]
    fn text_encoding_base_unpacks_the_low_word() {
        assert_eq!(unsafe { GetTextEncodingBase(encoding::UTF8) }, 0x0100);
        assert_eq!(unsafe { GetTextEncodingBase(encoding::MAC_ROMAN) }, 0);
    }

    /// Nothing here has a double-byte script installed, so every character is
    /// single-byte regardless of which script is asked about.
    #[test]
    fn every_character_is_single_byte() {
        let buf = [0_u8; 4];
        assert_eq!(
            unsafe { CharacterByteType(buf.as_ptr(), 0, 0) },
            SM_SINGLE_BYTE
        );
    }

    #[test]
    fn num_to_string_writes_a_pascal_string() {
        let mut out = [0xaa_u8; 256];
        unsafe { NumToString(-123, out.as_mut_ptr()) };
        assert_eq!(out[0], 4);
        assert_eq!(&out[1..5], b"-123");
    }

    /// `smSysScript` and `smRegionCode` answer an English US Mac;
    /// anything else is a selector this tree never allocated.
    #[test]
    fn script_manager_variable_answers_an_english_us_mac() {
        assert_eq!(unsafe { GetScriptManagerVariable(SM_SYS_SCRIPT) }, SM_ROMAN);
        assert_eq!(unsafe { GetScriptManagerVariable(SM_REGION_CODE) }, VER_US);
        assert_eq!(unsafe { GetScriptManagerVariable(999) }, SM_BAD_VERB);
    }

    /// Round-trips through the two high-level Unicode converter objects,
    /// mirroring `ascii_round_trips_through_every_encoding` above.
    #[test]
    fn text_and_unicode_info_round_trip() {
        let mapping = UnicodeMapping {
            unicode_encoding: encoding::UTF16,
            other_encoding: encoding::MAC_ROMAN,
            mapping_version: -1,
        };
        let mut to_unicode = std::ptr::null_mut();
        // SAFETY: a `UnicodeMapping` this test owns, and storage for the
        // opaque reference.
        assert_eq!(
            unsafe { CreateTextToUnicodeInfo(&mapping, &mut to_unicode) },
            NO_ERR
        );
        let source = b"Hi";
        let mut unicode_buf = [0_u16; 8];
        let (mut read, mut produced) = (0_u64, 0_u64);
        // SAFETY: a live info, a source this test owns, and a sized buffer.
        let status = unsafe {
            ConvertFromTextToUnicode(
                to_unicode,
                source.len() as u64,
                source.as_ptr(),
                0,
                0,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                (unicode_buf.len() * 2) as u64,
                &mut read,
                &mut produced,
                unicode_buf.as_mut_ptr(),
            )
        };
        assert_eq!(status, NO_ERR);
        assert_eq!(read, source.len() as u64);
        assert_eq!(&unicode_buf[..2], &[u16::from(b'H'), u16::from(b'i')]);

        let mut to_text = std::ptr::null_mut();
        // SAFETY: as above.
        assert_eq!(
            unsafe { CreateUnicodeToTextInfo(&mapping, &mut to_text) },
            NO_ERR
        );
        let mut text_buf = [0_u8; 8];
        let (mut input_read, mut output_len) = (0_u64, 0_u64);
        // SAFETY: a live info, the Unicode this test produced above, and a
        // sized buffer.
        let status = unsafe {
            ConvertFromUnicodeToText(
                to_text,
                produced,
                unicode_buf.as_ptr(),
                0,
                0,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                text_buf.len() as u64,
                &mut input_read,
                &mut output_len,
                text_buf.as_mut_ptr(),
            )
        };
        assert_eq!(status, NO_ERR);
        assert_eq!(&text_buf[..output_len as usize], source);

        // SAFETY: both still live, and not yet disposed.
        unsafe {
            assert_eq!(DisposeTextToUnicodeInfo(&mut to_unicode), NO_ERR);
            assert_eq!(DisposeUnicodeToTextInfo(&mut to_text), NO_ERR);
        }
        assert!(to_unicode.is_null());
        assert!(to_text.is_null());
    }
}
