use std::ffi::c_void;

use aes::{Aes128, Aes192, Aes256};
use blowfish::Blowfish;
use cast5::Cast5;
use cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit, StreamCipher};
use des::{Des, TdesEde3};
use rc2::Rc2;
use rc4::Rc4;

const CC_MD5_DIGEST_LENGTH: usize = 16;
const CC_SHA1_DIGEST_LENGTH: usize = 20;
const CC_SHA224_DIGEST_LENGTH: usize = 28;
const CC_SHA256_DIGEST_LENGTH: usize = 32;
const CC_SHA384_DIGEST_LENGTH: usize = 48;
const CC_SHA512_DIGEST_LENGTH: usize = 64;

const CC_MD5_BLOCK_BYTES: usize = 64;
const CC_SHA1_BLOCK_BYTES: usize = 64;
const CC_SHA256_BLOCK_BYTES: usize = 64;
const CC_SHA512_BLOCK_BYTES: usize = 128;

const MD5_INITIAL_STATE: [u32; 4] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476];
const SHA1_INITIAL_STATE: [u32; 5] = [
    0x6745_2301,
    0xEFCD_AB89,
    0x98BA_DCFE,
    0x1032_5476,
    0xC3D2_E1F0,
];
const SHA224_INITIAL_STATE: [u32; 8] = [
    0xc105_9ed8,
    0x367c_d507,
    0x3070_dd17,
    0xf70e_5939,
    0xffc0_0b31,
    0x6858_1511,
    0x64f9_8fa7,
    0xbefa_4fa4,
];
const SHA256_INITIAL_STATE: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];
const SHA384_INITIAL_STATE: [u64; 8] = [
    0xcbbb_9d5d_c105_9ed8,
    0x629a_292a_367c_d507,
    0x9159_015a_3070_dd17,
    0x152f_ecd8_f70e_5939,
    0x6733_2667_ffc0_0b31,
    0x8eb4_4a87_6858_1511,
    0xdb0c_2e0d_64f9_8fa7,
    0x47b5_481d_befa_4fa4,
];
const SHA512_INITIAL_STATE: [u64; 8] = [
    0x6a09_e667_f3bc_c908,
    0xbb67_ae85_84ca_a73b,
    0x3c6e_f372_fe94_f82b,
    0xa54f_f53a_5f1d_36f1,
    0x510e_527f_ade6_82d1,
    0x9b05_688c_2b3e_6c1f,
    0x1f83_d9ab_fb41_bd6b,
    0x5be0_cd19_137e_2179,
];

/// Merkle-Damgard padding: this byte, zeros, then the message length in bits.
const PADDING_MARKER: u8 = 0x80;
const BITS_PER_BYTE: u128 = u8::BITS as u128;

/// The guest's CC_*_CTX holds all digest state, so contexts can be copied and dropped freely.
trait DigestContext<const BLOCK: usize>: Copy {
    const LENGTH_FIELD_BYTES: usize;
    fn message_bits(&self) -> u128;
    fn set_message_bits(&mut self, bits: u128);
    fn buffer(&mut self) -> &mut [u8; BLOCK];
    fn compress(&mut self, blocks: &[[u8; BLOCK]]);
    fn write_length(field: &mut [u8], bits: u128);
    fn write_digest(&self, output: &mut [u8]);
}

/// `CC_MD5_CTX`: A-D, Nl, Nh, data[16], num.
#[repr(C)]
#[derive(Clone, Copy)]
struct Md5Context {
    state: [u32; 4],
    bit_count: [u32; 2],
    data: [u8; CC_MD5_BLOCK_BYTES],
    num: u32,
}

/// `CC_SHA1_CTX`: h0-h4, Nl, Nh, data[16], num.
#[repr(C)]
#[derive(Clone, Copy)]
struct Sha1Context {
    state: [u32; 5],
    bit_count: [u32; 2],
    data: [u8; CC_SHA1_BLOCK_BYTES],
    num: u32,
}

/// `CC_SHA256_CTX`, shared by SHA-224: count[2], hash[8], wbuf[16].
#[repr(C)]
#[derive(Clone, Copy)]
struct Sha256Context {
    bit_count: [u32; 2],
    state: [u32; 8],
    data: [u8; CC_SHA256_BLOCK_BYTES],
}

/// `CC_SHA512_CTX`, shared by SHA-384: count[2], hash[8], wbuf[16].
#[repr(C)]
#[derive(Clone, Copy)]
struct Sha512Context {
    bit_count: [u64; 2],
    state: [u64; 8],
    data: [u8; CC_SHA512_BLOCK_BYTES],
}

fn split_u64(value: u128) -> [u32; 2] {
    let value = value as u64;
    [value as u32, (value >> u32::BITS) as u32]
}

fn join_u32(words: [u32; 2]) -> u128 {
    u128::from(words[0]) | u128::from(words[1]) << u32::BITS
}

fn buffered_bytes<const BLOCK: usize>(bits: u128) -> usize {
    ((bits / BITS_PER_BYTE) % BLOCK as u128) as usize
}

impl Md5Context {
    fn new() -> Self {
        Self {
            state: MD5_INITIAL_STATE,
            bit_count: [0; 2],
            data: [0; CC_MD5_BLOCK_BYTES],
            num: 0,
        }
    }
}

impl DigestContext<CC_MD5_BLOCK_BYTES> for Md5Context {
    const LENGTH_FIELD_BYTES: usize = size_of::<u64>();
    fn message_bits(&self) -> u128 {
        join_u32(self.bit_count)
    }
    fn set_message_bits(&mut self, bits: u128) {
        self.bit_count = split_u64(bits);
        self.num = buffered_bytes::<CC_MD5_BLOCK_BYTES>(bits) as u32;
    }
    fn buffer(&mut self) -> &mut [u8; CC_MD5_BLOCK_BYTES] {
        &mut self.data
    }
    fn compress(&mut self, blocks: &[[u8; CC_MD5_BLOCK_BYTES]]) {
        md5::block_api::compress(&mut self.state, blocks);
    }
    fn write_length(field: &mut [u8], bits: u128) {
        field.copy_from_slice(&(bits as u64).to_le_bytes());
    }
    fn write_digest(&self, output: &mut [u8]) {
        for (bytes, word) in output.chunks_exact_mut(size_of::<u32>()).zip(self.state) {
            bytes.copy_from_slice(&word.to_le_bytes());
        }
    }
}

impl Sha1Context {
    fn new() -> Self {
        Self {
            state: SHA1_INITIAL_STATE,
            bit_count: [0; 2],
            data: [0; CC_SHA1_BLOCK_BYTES],
            num: 0,
        }
    }
}

impl DigestContext<CC_SHA1_BLOCK_BYTES> for Sha1Context {
    const LENGTH_FIELD_BYTES: usize = size_of::<u64>();
    fn message_bits(&self) -> u128 {
        join_u32(self.bit_count)
    }
    fn set_message_bits(&mut self, bits: u128) {
        self.bit_count = split_u64(bits);
        self.num = buffered_bytes::<CC_SHA1_BLOCK_BYTES>(bits) as u32;
    }
    fn buffer(&mut self) -> &mut [u8; CC_SHA1_BLOCK_BYTES] {
        &mut self.data
    }
    fn compress(&mut self, blocks: &[[u8; CC_SHA1_BLOCK_BYTES]]) {
        sha1::block_api::compress(&mut self.state, blocks);
    }
    fn write_length(field: &mut [u8], bits: u128) {
        field.copy_from_slice(&(bits as u64).to_be_bytes());
    }
    fn write_digest(&self, output: &mut [u8]) {
        for (bytes, word) in output.chunks_exact_mut(size_of::<u32>()).zip(self.state) {
            bytes.copy_from_slice(&word.to_be_bytes());
        }
    }
}

impl Sha256Context {
    fn new(state: [u32; 8]) -> Self {
        Self {
            bit_count: [0; 2],
            state,
            data: [0; CC_SHA256_BLOCK_BYTES],
        }
    }
}

impl DigestContext<CC_SHA256_BLOCK_BYTES> for Sha256Context {
    const LENGTH_FIELD_BYTES: usize = size_of::<u64>();
    fn message_bits(&self) -> u128 {
        join_u32(self.bit_count)
    }
    fn set_message_bits(&mut self, bits: u128) {
        self.bit_count = split_u64(bits);
    }
    fn buffer(&mut self) -> &mut [u8; CC_SHA256_BLOCK_BYTES] {
        &mut self.data
    }
    fn compress(&mut self, blocks: &[[u8; CC_SHA256_BLOCK_BYTES]]) {
        sha2::block_api::compress256(&mut self.state, blocks);
    }
    fn write_length(field: &mut [u8], bits: u128) {
        field.copy_from_slice(&(bits as u64).to_be_bytes());
    }
    fn write_digest(&self, output: &mut [u8]) {
        for (bytes, word) in output.chunks_exact_mut(size_of::<u32>()).zip(self.state) {
            bytes.copy_from_slice(&word.to_be_bytes());
        }
    }
}

impl Sha512Context {
    fn new(state: [u64; 8]) -> Self {
        Self {
            bit_count: [0; 2],
            state,
            data: [0; CC_SHA512_BLOCK_BYTES],
        }
    }
}

impl DigestContext<CC_SHA512_BLOCK_BYTES> for Sha512Context {
    const LENGTH_FIELD_BYTES: usize = size_of::<u128>();
    fn message_bits(&self) -> u128 {
        u128::from(self.bit_count[0]) | u128::from(self.bit_count[1]) << u64::BITS
    }
    fn set_message_bits(&mut self, bits: u128) {
        self.bit_count = [bits as u64, (bits >> u64::BITS) as u64];
    }
    fn buffer(&mut self) -> &mut [u8; CC_SHA512_BLOCK_BYTES] {
        &mut self.data
    }
    fn compress(&mut self, blocks: &[[u8; CC_SHA512_BLOCK_BYTES]]) {
        sha2::block_api::compress512(&mut self.state, blocks);
    }
    fn write_length(field: &mut [u8], bits: u128) {
        field.copy_from_slice(&bits.to_be_bytes());
    }
    fn write_digest(&self, output: &mut [u8]) {
        for (bytes, word) in output.chunks_exact_mut(size_of::<u64>()).zip(self.state) {
            bytes.copy_from_slice(&word.to_be_bytes());
        }
    }
}

fn absorb<const BLOCK: usize, C: DigestContext<BLOCK>>(context: &mut C, mut input: &[u8]) {
    let bits = context.message_bits();
    let mut buffered = buffered_bytes::<BLOCK>(bits);
    context.set_message_bits(bits.wrapping_add(input.len() as u128 * BITS_PER_BYTE));
    if buffered != 0 {
        let taken = input.len().min(BLOCK - buffered);
        context.buffer()[buffered..buffered + taken].copy_from_slice(&input[..taken]);
        input = &input[taken..];
        buffered += taken;
        if buffered < BLOCK {
            return;
        }
        let block = *context.buffer();
        context.compress(&[block]);
    }
    let (blocks, rest) = input.as_chunks::<BLOCK>();
    context.compress(blocks);
    context.buffer()[..rest.len()].copy_from_slice(rest);
}

fn finish<const BLOCK: usize, C: DigestContext<BLOCK>>(mut context: C, output: &mut [u8]) {
    let bits = context.message_bits();
    let buffered = buffered_bytes::<BLOCK>(bits);
    let buffer = context.buffer();
    buffer[buffered] = PADDING_MARKER;
    buffer[buffered + 1..].fill(0);
    if BLOCK - buffered - 1 < C::LENGTH_FIELD_BYTES {
        let block = *buffer;
        context.compress(&[block]);
        context.buffer().fill(0);
    }
    C::write_length(&mut context.buffer()[BLOCK - C::LENGTH_FIELD_BYTES..], bits);
    let block = *context.buffer();
    context.compress(&[block]);
    context.write_digest(output);
}

unsafe fn guest_input<'a>(input: *const c_void, len: u32) -> Option<&'a [u8]> {
    match (input.is_null(), len) {
        (_, 0) => Some(&[]),
        (true, _) => None,
        (false, len) => Some(unsafe { std::slice::from_raw_parts(input.cast(), len as usize) }),
    }
}

// Guest contexts may be under-aligned for Rust, so they are only accessed unaligned.
unsafe fn init<C>(context: *mut c_void, initial: C) -> i32 {
    if context.is_null() {
        return 0;
    }
    unsafe { context.cast::<C>().write_unaligned(initial) };
    1
}

unsafe fn update<const BLOCK: usize, C: DigestContext<BLOCK>>(
    context: *mut c_void,
    input: *const c_void,
    len: u32,
) -> i32 {
    let Some(input) = (unsafe { guest_input(input, len) }) else {
        return 0;
    };
    if context.is_null() {
        return 0;
    }
    let context = context.cast::<C>();
    let mut state = unsafe { context.read_unaligned() };
    absorb(&mut state, input);
    unsafe { context.write_unaligned(state) };
    1
}

unsafe fn final_<const BLOCK: usize, C: DigestContext<BLOCK>>(
    output: *mut u8,
    context: *mut c_void,
    digest_length: usize,
) -> i32 {
    if output.is_null() || context.is_null() {
        return 0;
    }
    let state = unsafe { context.cast::<C>().read_unaligned() };
    finish(state, unsafe {
        std::slice::from_raw_parts_mut(output, digest_length)
    });
    1
}

macro_rules! digest_api {
    ($i:ident, $u:ident, $f:ident, $one:ident, $context:ty, $initial:expr, $length:expr) => {
        unsafe extern "C" fn $i(context: *mut c_void) -> i32 {
            unsafe { init::<$context>(context, $initial) }
        }
        unsafe extern "C" fn $u(context: *mut c_void, input: *const c_void, len: u32) -> i32 {
            unsafe { update::<_, $context>(context, input, len) }
        }
        unsafe extern "C" fn $f(output: *mut u8, context: *mut c_void) -> i32 {
            unsafe { final_::<_, $context>(output, context, $length) }
        }
        unsafe extern "C" fn $one(input: *const c_void, len: u32, output: *mut u8) -> *mut u8 {
            let Some(input) = (unsafe { guest_input(input, len) }) else {
                return std::ptr::null_mut();
            };
            if output.is_null() {
                return std::ptr::null_mut();
            }
            let mut context: $context = $initial;
            absorb(&mut context, input);
            finish(context, unsafe {
                std::slice::from_raw_parts_mut(output, $length)
            });
            output
        }
    };
}
digest_api!(
    md5_init,
    md5_update,
    md5_final,
    md5,
    Md5Context,
    Md5Context::new(),
    CC_MD5_DIGEST_LENGTH
);
digest_api!(
    sha1_init,
    sha1_update,
    sha1_final,
    sha1,
    Sha1Context,
    Sha1Context::new(),
    CC_SHA1_DIGEST_LENGTH
);
digest_api!(
    sha224_init,
    sha224_update,
    sha224_final,
    sha224,
    Sha256Context,
    Sha256Context::new(SHA224_INITIAL_STATE),
    CC_SHA224_DIGEST_LENGTH
);
digest_api!(
    sha256_init,
    sha256_update,
    sha256_final,
    sha256,
    Sha256Context,
    Sha256Context::new(SHA256_INITIAL_STATE),
    CC_SHA256_DIGEST_LENGTH
);
digest_api!(
    sha384_init,
    sha384_update,
    sha384_final,
    sha384,
    Sha512Context,
    Sha512Context::new(SHA384_INITIAL_STATE),
    CC_SHA384_DIGEST_LENGTH
);
digest_api!(
    sha512_init,
    sha512_update,
    sha512_final,
    sha512,
    Sha512Context,
    Sha512Context::new(SHA512_INITIAL_STATE),
    CC_SHA512_DIGEST_LENGTH
);

/// `CCRandomGenerateBytes`: fill the supplied buffer from the OS CSPRNG.
unsafe extern "C" fn cc_random_generate_bytes(bytes: *mut u8, count: usize) -> i32 {
    if count == 0 {
        return KCC_SUCCESS;
    }
    if bytes.is_null() {
        return KCC_PARAM_ERROR;
    }
    let dest = unsafe { std::slice::from_raw_parts_mut(bytes, count) };
    match getrandom::fill(dest) {
        Ok(()) => KCC_SUCCESS,
        Err(_) => KCC_RNG_FAILURE,
    }
}

// CommonCrypto/CommonCryptor.h
const KCC_SUCCESS: i32 = 0;
const KCC_PARAM_ERROR: i32 = -4300;
const KCC_RNG_FAILURE: i32 = -4307;
const KCC_BUFFER_TOO_SMALL: i32 = -4301;
const KCC_ALIGNMENT_ERROR: i32 = -4303;
const KCC_DECODE_ERROR: i32 = -4304;
const KCC_UNIMPLEMENTED: i32 = -4305;
const KCC_ENCRYPT: u32 = 0;
const KCC_DECRYPT: u32 = 1;
const KCC_OPTION_PKCS7_PADDING: u32 = 1;
const KCC_OPTION_ECB_MODE: u32 = 2;

enum CryptorCipher {
    Aes128(Aes128),
    Aes192(Aes192),
    Aes256(Aes256),
    Des(Des),
    Tdes(TdesEde3),
    Cast5(Cast5),
    Rc2(Rc2),
    Blowfish(Box<Blowfish>),
    Rc4(Rc4),
}

impl CryptorCipher {
    fn block_size(&self) -> Option<usize> {
        match self {
            Self::Aes128(_) | Self::Aes192(_) | Self::Aes256(_) => Some(16),
            Self::Des(_) | Self::Tdes(_) | Self::Cast5(_) | Self::Rc2(_) | Self::Blowfish(_) => {
                Some(8)
            }
            Self::Rc4(_) => None,
        }
    }

    fn crypt_block(&self, encrypt: bool, bytes: &mut [u8]) {
        macro_rules! crypt {
            ($cipher:expr, $ty:ty) => {{
                let mut block = cipher::Block::<$ty>::default();
                block.copy_from_slice(bytes);
                if encrypt {
                    $cipher.encrypt_block(&mut block);
                } else {
                    $cipher.decrypt_block(&mut block);
                }
                bytes.copy_from_slice(&block);
            }};
        }
        match self {
            Self::Aes128(c) => crypt!(c, Aes128),
            Self::Aes192(c) => crypt!(c, Aes192),
            Self::Aes256(c) => crypt!(c, Aes256),
            Self::Des(c) => crypt!(c, Des),
            Self::Tdes(c) => crypt!(c, TdesEde3),
            Self::Cast5(c) => crypt!(c, Cast5),
            Self::Rc2(c) => crypt!(c, Rc2),
            Self::Blowfish(c) => crypt!(c, Blowfish),
            Self::Rc4(_) => unreachable!("RC4 is a stream cipher"),
        }
    }

    fn apply_stream(&mut self, bytes: &mut [u8]) {
        match self {
            Self::Rc4(c) => c.apply_keystream(bytes),
            _ => unreachable!("block cipher used as a stream cipher"),
        }
    }
}

struct Cryptor {
    cipher: CryptorCipher,
    encrypt: bool,
    padding: bool,
    ecb: bool,
    initial_iv: Vec<u8>,
    chain: Vec<u8>,
    pending: Vec<u8>,
    finalized: bool,
    heap_allocated: bool,
}

impl Cryptor {
    fn output_length(&self, input: usize, final_: bool) -> usize {
        let Some(block) = self.cipher.block_size() else {
            return input;
        };
        let total = self.pending.len().saturating_add(input);
        if final_ {
            if self.encrypt && self.padding {
                return total / block * block + block;
            }
            return total;
        }
        let complete = total / block * block;
        if !self.encrypt && self.padding {
            complete.saturating_sub(block)
        } else {
            complete
        }
    }

    fn transform_block(&mut self, block: &mut [u8]) {
        if self.encrypt {
            if !self.ecb {
                for (byte, chain) in block.iter_mut().zip(&self.chain) {
                    *byte ^= chain;
                }
            }
            self.cipher.crypt_block(true, block);
            if !self.ecb {
                self.chain.copy_from_slice(block);
            }
        } else {
            let encrypted = block.to_vec();
            self.cipher.crypt_block(false, block);
            if !self.ecb {
                for (byte, chain) in block.iter_mut().zip(&self.chain) {
                    *byte ^= chain;
                }
                self.chain.copy_from_slice(&encrypted);
            }
        }
    }

    fn update(&mut self, input: &[u8], output: &mut Vec<u8>) {
        if self.cipher.block_size().is_none() {
            let mut bytes = input.to_vec();
            self.cipher.apply_stream(&mut bytes);
            output.extend_from_slice(&bytes);
            return;
        }
        let block = self.cipher.block_size().unwrap();
        self.pending.extend_from_slice(input);
        let keep = if !self.encrypt && self.padding {
            block
        } else {
            0
        };
        while self.pending.len() >= block + keep {
            let mut bytes: Vec<_> = self.pending.drain(..block).collect();
            self.transform_block(&mut bytes);
            output.extend_from_slice(&bytes);
        }
    }

    fn final_(&mut self, output: &mut Vec<u8>) -> Result<(), i32> {
        let Some(block) = self.cipher.block_size() else {
            self.finalized = true;
            return Ok(());
        };
        if self.encrypt && self.padding {
            let padding = block - self.pending.len() % block;
            self.pending
                .extend(std::iter::repeat_n(padding as u8, padding));
        }
        if !self.pending.len().is_multiple_of(block) {
            return Err(KCC_ALIGNMENT_ERROR);
        }
        while !self.pending.is_empty() {
            let mut bytes: Vec<_> = self.pending.drain(..block).collect();
            self.transform_block(&mut bytes);
            output.extend_from_slice(&bytes);
        }
        if !self.encrypt && self.padding {
            let Some(&padding) = output.last() else {
                return Err(KCC_DECODE_ERROR);
            };
            let padding = usize::from(padding);
            if padding == 0
                || padding > block
                || padding > output.len()
                || !output[output.len() - padding..]
                    .iter()
                    .all(|&byte| byte as usize == padding)
            {
                return Err(KCC_DECODE_ERROR);
            }
            output.truncate(output.len() - padding);
        }
        self.finalized = true;
        Ok(())
    }

    fn reset(&mut self, iv: Option<&[u8]>) -> Result<(), i32> {
        if self.cipher.block_size().is_none() {
            return Err(KCC_UNIMPLEMENTED);
        }
        if let Some(iv) = iv {
            if iv.len() != self.chain.len() {
                return Err(KCC_PARAM_ERROR);
            }
            self.initial_iv.copy_from_slice(iv);
        }
        self.chain.copy_from_slice(&self.initial_iv);
        self.pending.clear();
        self.finalized = false;
        Ok(())
    }
}

unsafe fn input<'a>(ptr: *const c_void, len: usize) -> Result<&'a [u8], i32> {
    if ptr.is_null() && len != 0 {
        return Err(KCC_PARAM_ERROR);
    }
    Ok(if len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ptr.cast(), len) }
    })
}

fn make_cryptor(
    op: u32,
    algorithm: u32,
    options: u32,
    key: &[u8],
    iv: Option<&[u8]>,
    heap_allocated: bool,
) -> Result<Cryptor, i32> {
    if op > KCC_DECRYPT || options & !(KCC_OPTION_PKCS7_PADDING | KCC_OPTION_ECB_MODE) != 0 {
        return Err(KCC_PARAM_ERROR);
    }
    let cipher = match algorithm {
        0 => match key.len() {
            16 => CryptorCipher::Aes128(Aes128::new_from_slice(key).unwrap()),
            24 => CryptorCipher::Aes192(Aes192::new_from_slice(key).unwrap()),
            32 => CryptorCipher::Aes256(Aes256::new_from_slice(key).unwrap()),
            _ => return Err(KCC_PARAM_ERROR),
        },
        1 if key.len() == 8 => CryptorCipher::Des(Des::new_from_slice(key).unwrap()),
        2 if key.len() == 24 => CryptorCipher::Tdes(TdesEde3::new_from_slice(key).unwrap()),
        3 => CryptorCipher::Cast5(Cast5::new_from_slice(key).map_err(|_| KCC_PARAM_ERROR)?),
        4 => CryptorCipher::Rc4(Rc4::new_from_slice(key).map_err(|_| KCC_PARAM_ERROR)?),
        5 => CryptorCipher::Rc2(Rc2::new_from_slice(key).map_err(|_| KCC_PARAM_ERROR)?),
        6 => CryptorCipher::Blowfish(Box::new(
            Blowfish::new_from_slice(key).map_err(|_| KCC_PARAM_ERROR)?,
        )),
        _ => return Err(KCC_UNIMPLEMENTED),
    };
    let block = cipher.block_size();
    if block.is_none() && options != 0 {
        return Err(KCC_PARAM_ERROR);
    }
    let iv = match block {
        Some(size) if options & KCC_OPTION_ECB_MODE == 0 => match iv {
            Some(iv) if iv.len() == size => iv.to_vec(),
            Some(_) => return Err(KCC_PARAM_ERROR),
            None => vec![0; size],
        },
        _ => Vec::new(),
    };
    Ok(Cryptor {
        cipher,
        encrypt: op == KCC_ENCRYPT,
        padding: options & KCC_OPTION_PKCS7_PADDING != 0,
        ecb: options & KCC_OPTION_ECB_MODE != 0,
        chain: iv.clone(),
        initial_iv: iv,
        pending: Vec::new(),
        finalized: false,
        heap_allocated,
    })
}

unsafe fn cryptor<'a>(handle: *mut c_void) -> Result<&'a mut Cryptor, i32> {
    unsafe { handle.cast::<Cryptor>().as_mut().ok_or(KCC_PARAM_ERROR) }
}

unsafe fn cryptor_ref<'a>(handle: *mut c_void) -> Result<&'a Cryptor, i32> {
    unsafe { handle.cast::<Cryptor>().as_ref().ok_or(KCC_PARAM_ERROR) }
}

unsafe extern "C" fn cryptor_create(
    op: u32,
    algorithm: u32,
    options: u32,
    key: *const c_void,
    key_len: usize,
    iv: *const c_void,
    out: *mut *mut c_void,
) -> i32 {
    if out.is_null() {
        return KCC_PARAM_ERROR;
    }
    unsafe {
        out.write(std::ptr::null_mut());
    }
    let key = match unsafe { input(key, key_len) } {
        Ok(key) => key,
        Err(status) => return status,
    };
    let iv = unsafe { read_iv(algorithm, iv) };
    let cryptor = match make_cryptor(op, algorithm, options, key, iv, true) {
        Ok(c) => c,
        Err(s) => return s,
    };
    let handle = Box::into_raw(Box::new(cryptor)).cast::<c_void>();
    unsafe {
        out.write(handle);
    }
    KCC_SUCCESS
}

unsafe fn read_iv<'a>(algorithm: u32, iv: *const c_void) -> Option<&'a [u8]> {
    if iv.is_null() {
        return None;
    }
    let length = match algorithm {
        0 => 16,
        1 | 2 | 3 | 5 | 6 => 8,
        _ => 0,
    };
    Some(if length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(iv.cast(), length) }
    })
}

unsafe extern "C" fn cryptor_create_from_data(
    op: u32,
    algorithm: u32,
    options: u32,
    key: *const c_void,
    key_len: usize,
    iv: *const c_void,
    data: *mut c_void,
    data_len: usize,
    out: *mut *mut c_void,
    data_used: *mut usize,
) -> i32 {
    if out.is_null() || data.is_null() {
        return KCC_PARAM_ERROR;
    }
    unsafe {
        out.write(std::ptr::null_mut());
    }
    let required = std::mem::size_of::<Cryptor>();
    if !data_used.is_null() {
        unsafe { data_used.write(required) };
    }
    if data_len < required {
        return KCC_BUFFER_TOO_SMALL;
    }
    if data.align_offset(std::mem::align_of::<Cryptor>()) != 0 {
        return KCC_PARAM_ERROR;
    }
    let key = match unsafe { input(key, key_len) } {
        Ok(key) => key,
        Err(status) => return status,
    };
    let cryptor = match make_cryptor(
        op,
        algorithm,
        options,
        key,
        unsafe { read_iv(algorithm, iv) },
        false,
    ) {
        Ok(c) => c,
        Err(s) => return s,
    };
    unsafe { data.cast::<Cryptor>().write(cryptor) };
    unsafe {
        out.write(data);
    }
    KCC_SUCCESS
}

unsafe extern "C" fn cryptor_release(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        return KCC_PARAM_ERROR;
    }
    let cryptor = match unsafe { cryptor(handle) } {
        Ok(cryptor) => cryptor,
        Err(status) => return status,
    };
    if cryptor.heap_allocated {
        unsafe {
            drop(Box::from_raw(handle.cast::<Cryptor>()));
        }
    } else {
        unsafe { std::ptr::drop_in_place(handle.cast::<Cryptor>()) };
    }
    KCC_SUCCESS
}

unsafe extern "C" fn cryptor_update(
    handle: *mut c_void,
    data_in: *const c_void,
    data_in_len: usize,
    data_out: *mut c_void,
    data_out_available: usize,
    moved: *mut usize,
) -> i32 {
    if !moved.is_null() {
        unsafe {
            moved.write(0);
        }
    }
    let input = match unsafe { input(data_in, data_in_len) } {
        Ok(input) => input,
        Err(status) => return status,
    };
    let cryptor = match unsafe { cryptor(handle) } {
        Ok(cryptor) => cryptor,
        Err(status) => return status,
    };
    if cryptor.finalized {
        return KCC_PARAM_ERROR;
    }
    let needed = cryptor.output_length(input.len(), false);
    if needed > data_out_available || (needed != 0 && data_out.is_null()) {
        return KCC_BUFFER_TOO_SMALL;
    }
    let mut output = Vec::with_capacity(needed);
    cryptor.update(input, &mut output);
    debug_assert_eq!(output.len(), needed);
    if needed != 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(output.as_ptr(), data_out.cast(), needed);
        }
    }
    if !moved.is_null() {
        unsafe {
            moved.write(needed);
        }
    }
    KCC_SUCCESS
}

unsafe extern "C" fn cryptor_final(
    handle: *mut c_void,
    data_out: *mut c_void,
    data_out_available: usize,
    moved: *mut usize,
) -> i32 {
    if !moved.is_null() {
        unsafe {
            moved.write(0);
        }
    }
    let cryptor = match unsafe { cryptor(handle) } {
        Ok(cryptor) => cryptor,
        Err(status) => return status,
    };
    if cryptor.finalized {
        return KCC_PARAM_ERROR;
    }
    let maximum = cryptor.output_length(0, true);
    if maximum > data_out_available || (maximum != 0 && data_out.is_null()) {
        return KCC_BUFFER_TOO_SMALL;
    }
    let mut output = Vec::with_capacity(maximum);
    if let Err(status) = cryptor.final_(&mut output) {
        return status;
    }
    if !output.is_empty() {
        unsafe {
            std::ptr::copy_nonoverlapping(output.as_ptr(), data_out.cast(), output.len());
        }
    }
    if !moved.is_null() {
        unsafe {
            moved.write(output.len());
        }
    }
    KCC_SUCCESS
}

unsafe extern "C" fn cryptor_get_output_length(
    handle: *mut c_void,
    input_len: usize,
    final_: bool,
) -> usize {
    unsafe { cryptor_ref(handle) }.map_or(0, |cryptor| cryptor.output_length(input_len, final_))
}

unsafe extern "C" fn cryptor_reset(handle: *mut c_void, iv: *const c_void) -> i32 {
    let cryptor = match unsafe { cryptor(handle) } {
        Ok(cryptor) => cryptor,
        Err(status) => return status,
    };
    let iv = if iv.is_null() {
        None
    } else {
        let Some(length) = cryptor.cipher.block_size() else {
            return KCC_UNIMPLEMENTED;
        };
        Some(unsafe { std::slice::from_raw_parts(iv.cast(), length) })
    };
    cryptor
        .reset(iv)
        .map_or_else(|status| status, |_| KCC_SUCCESS)
}

unsafe extern "C" fn crypt(
    op: u32,
    algorithm: u32,
    options: u32,
    key: *const c_void,
    key_len: usize,
    iv: *const c_void,
    data_in: *const c_void,
    data_in_len: usize,
    data_out: *mut c_void,
    data_out_available: usize,
    moved: *mut usize,
) -> i32 {
    if !moved.is_null() {
        unsafe {
            moved.write(0);
        }
    }
    let mut handle = std::ptr::null_mut();
    let status = unsafe { cryptor_create(op, algorithm, options, key, key_len, iv, &mut handle) };
    if status != KCC_SUCCESS {
        return status;
    }
    let mut update_moved = 0;
    let status = unsafe {
        cryptor_update(
            handle,
            data_in,
            data_in_len,
            data_out,
            data_out_available,
            &mut update_moved,
        )
    };
    if status != KCC_SUCCESS {
        unsafe {
            cryptor_release(handle);
        }
        return status;
    }
    let final_out = if data_out.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { data_out.cast::<u8>().add(update_moved).cast() }
    };
    let final_available = data_out_available.saturating_sub(update_moved);
    let mut final_moved = 0;
    let status = unsafe { cryptor_final(handle, final_out, final_available, &mut final_moved) };
    unsafe {
        cryptor_release(handle);
    }
    if status == KCC_SUCCESS && !moved.is_null() {
        unsafe {
            moved.write(update_moved + final_moved);
        }
    }
    status
}
exports::exports! {
    functions {
        "CC_MD5_Init"=>md5_init,
        "CC_MD5_Update"=>md5_update,
        "CC_MD5_Final"=>md5_final,
        "CC_MD5"=>md5,
        "CC_SHA1_Init"=>sha1_init,
        "CC_SHA1_Update"=>sha1_update,
        "CC_SHA1_Final"=>sha1_final,
        "CC_SHA1"=>sha1,
        "CC_SHA224_Init"=>sha224_init,
        "CC_SHA224_Update"=>sha224_update,
        "CC_SHA224_Final"=>sha224_final,
        "CC_SHA224"=>sha224,
        "CC_SHA256_Init"=>sha256_init,
        "CC_SHA256_Update"=>sha256_update,
        "CC_SHA256_Final"=>sha256_final,
        "CC_SHA256"=>sha256,
        "CC_SHA384_Init"=>sha384_init,
        "CC_SHA384_Update"=>sha384_update,
        "CC_SHA384_Final"=>sha384_final,
        "CC_SHA384"=>sha384,
        "CC_SHA512_Init"=>sha512_init,
        "CC_SHA512_Update"=>sha512_update,
        "CC_SHA512_Final"=>sha512_final,
        "CC_SHA512"=>sha512,
        "CCCryptorCreate"=>cryptor_create,
        "CCCryptorCreateFromData"=>cryptor_create_from_data,
        "CCCryptorRelease"=>cryptor_release,
        "CCCryptorUpdate"=>cryptor_update,
        "CCCryptorFinal"=>cryptor_final,
        "CCCryptorGetOutputLength"=>cryptor_get_output_length,
        "CCCryptorReset"=>cryptor_reset,
        "CCCrypt"=>crypt,
        "CCRandomGenerateBytes"=>cc_random_generate_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Mirrors Apple's CommonRandom.c zero-size and invalid pointer cases.
    #[test]
    fn common_random_rejects_invalid_buffers_and_fills_valid_buffers() {
        let null = std::ptr::null_mut();
        assert_eq!(unsafe { cc_random_generate_bytes(null, 0) }, KCC_SUCCESS);
        assert_eq!(
            unsafe { cc_random_generate_bytes(null, 1) },
            KCC_PARAM_ERROR
        );
        let mut bytes = [0xA5u8; 32];
        assert_eq!(
            unsafe { cc_random_generate_bytes(bytes.as_mut_ptr(), bytes.len()) },
            KCC_SUCCESS
        );
        assert_ne!(bytes, [0xA5; 32]);
    }

    #[test]
    fn sha256_streaming_matches_known_digest() {
        let mut c = [0u8; 104];
        let mut out = [0u8; 32];
        unsafe {
            assert_eq!(sha256_init(c.as_mut_ptr().cast()), 1);
            assert_eq!(
                sha256_update(c.as_mut_ptr().cast(), c"abc".as_ptr().cast(), 3),
                1
            );
            assert_eq!(sha256_final(out.as_mut_ptr(), c.as_mut_ptr().cast()), 1);
        }
        assert_eq!(
            out,
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad
            ]
        );
    }

    type Init = unsafe extern "C" fn(*mut c_void) -> i32;
    type Update = unsafe extern "C" fn(*mut c_void, *const c_void, u32) -> i32;
    type Final = unsafe extern "C" fn(*mut u8, *mut c_void) -> i32;
    type OneShot = unsafe extern "C" fn(*const c_void, u32, *mut u8) -> *mut u8;

    struct Algorithm {
        init: Init,
        update: Update,
        final_: Final,
        one_shot: OneShot,
        reference: fn(&[u8]) -> Vec<u8>,
        context_size: usize,
        digest_length: usize,
    }

    fn reference<D: sha2::Digest>(input: &[u8]) -> Vec<u8> {
        D::digest(input).to_vec()
    }

    fn algorithms() -> [Algorithm; 6] {
        [
            Algorithm {
                init: md5_init,
                update: md5_update,
                final_: md5_final,
                one_shot: md5,
                reference: reference::<md5::Md5>,
                context_size: size_of::<Md5Context>(),
                digest_length: CC_MD5_DIGEST_LENGTH,
            },
            Algorithm {
                init: sha1_init,
                update: sha1_update,
                final_: sha1_final,
                one_shot: sha1,
                reference: reference::<sha1::Sha1>,
                context_size: size_of::<Sha1Context>(),
                digest_length: CC_SHA1_DIGEST_LENGTH,
            },
            Algorithm {
                init: sha224_init,
                update: sha224_update,
                final_: sha224_final,
                one_shot: sha224,
                reference: reference::<sha2::Sha224>,
                context_size: size_of::<Sha256Context>(),
                digest_length: CC_SHA224_DIGEST_LENGTH,
            },
            Algorithm {
                init: sha256_init,
                update: sha256_update,
                final_: sha256_final,
                one_shot: sha256,
                reference: reference::<sha2::Sha256>,
                context_size: size_of::<Sha256Context>(),
                digest_length: CC_SHA256_DIGEST_LENGTH,
            },
            Algorithm {
                init: sha384_init,
                update: sha384_update,
                final_: sha384_final,
                one_shot: sha384,
                reference: reference::<sha2::Sha384>,
                context_size: size_of::<Sha512Context>(),
                digest_length: CC_SHA384_DIGEST_LENGTH,
            },
            Algorithm {
                init: sha512_init,
                update: sha512_update,
                final_: sha512_final,
                one_shot: sha512,
                reference: reference::<sha2::Sha512>,
                context_size: size_of::<Sha512Context>(),
                digest_length: CC_SHA512_DIGEST_LENGTH,
            },
        ]
    }

    impl Algorithm {
        fn feed(&self, context: &mut [u8], input: &[u8]) {
            let len = input.len() as u32;
            assert_eq!(
                unsafe { (self.update)(context.as_mut_ptr().cast(), input.as_ptr().cast(), len) },
                1
            );
        }

        fn finish(&self, context: &mut [u8]) -> Vec<u8> {
            let mut output = vec![0; self.digest_length];
            assert_eq!(
                unsafe { (self.final_)(output.as_mut_ptr(), context.as_mut_ptr().cast()) },
                1
            );
            output
        }

        fn streamed(&self, chunks: &[&[u8]]) -> Vec<u8> {
            let mut context = vec![0; self.context_size];
            assert_eq!(unsafe { (self.init)(context.as_mut_ptr().cast()) }, 1);
            for chunk in chunks {
                self.feed(&mut context, chunk);
            }
            self.finish(&mut context)
        }
    }

    #[test]
    fn digest_contexts_have_common_digest_sizes() {
        assert_eq!(size_of::<Md5Context>(), 92);
        assert_eq!(size_of::<Sha1Context>(), 96);
        assert_eq!(size_of::<Sha256Context>(), 104);
        assert_eq!(size_of::<Sha512Context>(), 208);
    }

    #[test]
    fn digests_match_reference_across_block_boundaries() {
        let message: Vec<u8> = (0..=u8::MAX).cycle().take(300).collect();
        for algorithm in algorithms() {
            for len in [0, 1, 55, 56, 63, 64, 65, 111, 112, 127, 128, 129, 300] {
                let input = &message[..len];
                let expected = (algorithm.reference)(input);
                let mut one_shot = vec![0; algorithm.digest_length];
                unsafe {
                    (algorithm.one_shot)(input.as_ptr().cast(), len as u32, one_shot.as_mut_ptr());
                }
                assert_eq!(one_shot, expected, "one-shot, {len} bytes");
                assert_eq!(algorithm.streamed(&[input]), expected, "{len} bytes");
                for split in [1, 7, 63, 64, 65, 127, 128]
                    .into_iter()
                    .filter(|&s| s < len)
                {
                    let (head, tail) = input.split_at(split);
                    assert_eq!(
                        algorithm.streamed(&[head, &[], tail]),
                        expected,
                        "{len} bytes split at {split}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_copied_digest_context_finishes_independently() {
        let prefix: Vec<u8> = (0..100).collect();
        for algorithm in algorithms() {
            let mut original = vec![0; algorithm.context_size];
            assert_eq!(unsafe { (algorithm.init)(original.as_mut_ptr().cast()) }, 1);
            algorithm.feed(&mut original, &prefix);
            let mut copy = original.clone();
            algorithm.feed(&mut original, b"left");
            algorithm.feed(&mut copy, b"right side");
            assert_eq!(
                algorithm.finish(&mut copy),
                (algorithm.reference)(&[&prefix[..], b"right side"].concat())
            );
            assert_eq!(
                algorithm.finish(&mut original),
                (algorithm.reference)(&[&prefix[..], b"left"].concat())
            );
        }
    }

    #[test]
    fn one_shot_returns_output_argument() {
        let mut out = [0u8; 20];
        assert_eq!(
            unsafe { sha1(c"abc".as_ptr().cast(), 3, out.as_mut_ptr()) },
            out.as_mut_ptr()
        );
    }

    #[test]
    fn cccrypt_aes_cbc_matches_nist_vector() {
        let key = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let iv = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let plain = [
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93,
            0x17, 0x2a,
        ];
        let expected = [
            0x76, 0x49, 0xab, 0xac, 0x81, 0x19, 0xb2, 0x46, 0xce, 0xe9, 0x8e, 0x9b, 0x12, 0xe9,
            0x19, 0x7d,
        ];
        let mut block = cipher::Block::<Aes128>::default();
        for ((byte, plain), iv) in block.iter_mut().zip(plain).zip(iv) {
            *byte = plain ^ iv;
        }
        Aes128::new_from_slice(&key)
            .unwrap()
            .encrypt_block(&mut block);
        assert_eq!(&block[..], expected);
        let mut encrypted = [0u8; 16];
        let mut moved = 0;
        assert_eq!(
            unsafe {
                crypt(
                    KCC_ENCRYPT,
                    0,
                    0,
                    key.as_ptr().cast(),
                    key.len(),
                    iv.as_ptr().cast(),
                    plain.as_ptr().cast(),
                    plain.len(),
                    encrypted.as_mut_ptr().cast(),
                    encrypted.len(),
                    &mut moved,
                )
            },
            KCC_SUCCESS
        );
        assert_eq!(moved, expected.len());
        assert_eq!(encrypted, expected);
    }

    #[test]
    fn cryptor_streaming_padding_reset_and_caller_storage_work() {
        let key = *b"0123456789abcdef";
        let iv = *b"fedcba9876543210";
        let plain = b"streaming plaintext";
        let mut storage = std::mem::MaybeUninit::<Cryptor>::uninit();
        let mut handle = std::ptr::null_mut();
        let mut used = 0;
        assert_eq!(
            unsafe {
                cryptor_create_from_data(
                    KCC_ENCRYPT,
                    0,
                    KCC_OPTION_PKCS7_PADDING,
                    key.as_ptr().cast(),
                    key.len(),
                    iv.as_ptr().cast(),
                    storage.as_mut_ptr().cast(),
                    std::mem::size_of::<Cryptor>(),
                    &mut handle,
                    &mut used,
                )
            },
            KCC_SUCCESS
        );
        assert_eq!(handle, storage.as_mut_ptr().cast());
        assert_eq!(used, std::mem::size_of::<Cryptor>());
        assert_eq!(
            unsafe { cryptor_get_output_length(handle, plain.len(), true) },
            32
        );
        let mut encrypted = [0u8; 32];
        let mut update_moved = 0;
        assert_eq!(
            unsafe {
                cryptor_update(
                    handle,
                    plain[..5].as_ptr().cast(),
                    5,
                    encrypted.as_mut_ptr().cast(),
                    encrypted.len(),
                    &mut update_moved,
                )
            },
            KCC_SUCCESS
        );
        assert_eq!(update_moved, 0);
        assert_eq!(
            unsafe {
                cryptor_update(
                    handle,
                    plain[5..].as_ptr().cast(),
                    plain.len() - 5,
                    encrypted.as_mut_ptr().cast(),
                    encrypted.len(),
                    &mut update_moved,
                )
            },
            KCC_SUCCESS
        );
        let mut final_moved = 0;
        assert_eq!(
            unsafe {
                cryptor_final(
                    handle,
                    encrypted[update_moved..].as_mut_ptr().cast(),
                    encrypted.len() - update_moved,
                    &mut final_moved,
                )
            },
            KCC_SUCCESS
        );
        assert_eq!(update_moved + final_moved, 32);
        assert_eq!(
            unsafe { cryptor_reset(handle, iv.as_ptr().cast()) },
            KCC_SUCCESS
        );
        assert_eq!(unsafe { cryptor_release(handle) }, KCC_SUCCESS);

        let mut decrypted = [0u8; 32];
        let mut decrypted_len = 0;
        assert_eq!(
            unsafe {
                crypt(
                    KCC_DECRYPT,
                    0,
                    KCC_OPTION_PKCS7_PADDING,
                    key.as_ptr().cast(),
                    key.len(),
                    iv.as_ptr().cast(),
                    encrypted.as_ptr().cast(),
                    encrypted.len(),
                    decrypted.as_mut_ptr().cast(),
                    decrypted.len(),
                    &mut decrypted_len,
                )
            },
            KCC_SUCCESS
        );
        assert_eq!(&decrypted[..decrypted_len], plain);
    }
}
