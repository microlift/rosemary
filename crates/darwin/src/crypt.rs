//! `crypt(3)` as Darwin's libc has it: 4.4BSD's DES crypt, the traditional
//! two-character salt and BSDI's `_` extended form, and nothing else. A `$1$`
//! or `$2b$` setting is not MD5 or bcrypt there - it is a DES salt like any
//! other - so the host's `crypt`, which reads those as other algorithms (or has
//! no DES at all), cannot stand in for it.

use std::cell::RefCell;
use std::ffi::{CStr, c_char};

/// `_PASSWORD_EFMT1` from `<pwd.h>`: a setting that starts with it carries a
/// four-character round count and a four-character salt.
const PASSWORD_EFMT1: u8 = b'_';

/// The rounds a traditional setting gets.
const TRADITIONAL_ROUNDS: u32 = 25;

/// The alphabet salts, counts and hashes are written in, six bits a character.
const ITOA64: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// `_` + count + salt + hash + NUL, the longest answer.
const RESULT_LEN: usize = 1 + 4 + 4 + 11 + 1;

/// The value of an alphabet character; anything outside it is zero, as BSD's
/// zero-initialised `a64toi` table makes it.
fn a64toi(c: u8) -> u32 {
    ITOA64.iter().position(|&x| x == c).unwrap_or(0) as u32
}

/// `(input >> (in_bits - table[i]))` for each entry, packed MSB first: the
/// DES tables count bit 1 as the most significant.
fn permute(input: u64, in_bits: u32, table: &[u8]) -> u64 {
    table.iter().fold(0, |out, &from| {
        (out << 1) | ((input >> (in_bits - u32::from(from))) & 1)
    })
}

const IP: [u8; 64] = [
    58, 50, 42, 34, 26, 18, 10, 2, 60, 52, 44, 36, 28, 20, 12, 4, 62, 54, 46, 38, 30, 22, 14, 6,
    64, 56, 48, 40, 32, 24, 16, 8, 57, 49, 41, 33, 25, 17, 9, 1, 59, 51, 43, 35, 27, 19, 11, 3, 61,
    53, 45, 37, 29, 21, 13, 5, 63, 55, 47, 39, 31, 23, 15, 7,
];

/// `IP`'s inverse.
const FP: [u8; 64] = {
    let mut fp = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        fp[IP[i] as usize - 1] = i as u8 + 1;
        i += 1;
    }
    fp
};

const PC1: [u8; 56] = [
    57, 49, 41, 33, 25, 17, 9, 1, 58, 50, 42, 34, 26, 18, 10, 2, 59, 51, 43, 35, 27, 19, 11, 3, 60,
    52, 44, 36, 63, 55, 47, 39, 31, 23, 15, 7, 62, 54, 46, 38, 30, 22, 14, 6, 61, 53, 45, 37, 29,
    21, 13, 5, 28, 20, 12, 4,
];

const PC2: [u8; 48] = [
    14, 17, 11, 24, 1, 5, 3, 28, 15, 6, 21, 10, 23, 19, 12, 4, 26, 8, 16, 7, 27, 20, 13, 2, 41, 52,
    31, 37, 47, 55, 30, 40, 51, 45, 33, 48, 44, 49, 39, 56, 34, 53, 46, 42, 50, 36, 29, 32,
];

const SHIFTS: [u32; 16] = [1, 1, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1];

const P: [u8; 32] = [
    16, 7, 20, 21, 29, 12, 28, 17, 1, 15, 23, 26, 5, 18, 31, 10, 2, 8, 24, 14, 32, 27, 3, 9, 19,
    13, 30, 6, 22, 11, 4, 25,
];

#[rustfmt::skip]
const S: [[u8; 64]; 8] = [
    [14, 4, 13, 1, 2, 15, 11, 8, 3, 10, 6, 12, 5, 9, 0, 7,
     0, 15, 7, 4, 14, 2, 13, 1, 10, 6, 12, 11, 9, 5, 3, 8,
     4, 1, 14, 8, 13, 6, 2, 11, 15, 12, 9, 7, 3, 10, 5, 0,
     15, 12, 8, 2, 4, 9, 1, 7, 5, 11, 3, 14, 10, 0, 6, 13],
    [15, 1, 8, 14, 6, 11, 3, 4, 9, 7, 2, 13, 12, 0, 5, 10,
     3, 13, 4, 7, 15, 2, 8, 14, 12, 0, 1, 10, 6, 9, 11, 5,
     0, 14, 7, 11, 10, 4, 13, 1, 5, 8, 12, 6, 9, 3, 2, 15,
     13, 8, 10, 1, 3, 15, 4, 2, 11, 6, 7, 12, 0, 5, 14, 9],
    [10, 0, 9, 14, 6, 3, 15, 5, 1, 13, 12, 7, 11, 4, 2, 8,
     13, 7, 0, 9, 3, 4, 6, 10, 2, 8, 5, 14, 12, 11, 15, 1,
     13, 6, 4, 9, 8, 15, 3, 0, 11, 1, 2, 12, 5, 10, 14, 7,
     1, 10, 13, 0, 6, 9, 8, 7, 4, 15, 14, 3, 11, 5, 2, 12],
    [7, 13, 14, 3, 0, 6, 9, 10, 1, 2, 8, 5, 11, 12, 4, 15,
     13, 8, 11, 5, 6, 15, 0, 3, 4, 7, 2, 12, 1, 10, 14, 9,
     10, 6, 9, 0, 12, 11, 7, 13, 15, 1, 3, 14, 5, 2, 8, 4,
     3, 15, 0, 6, 10, 1, 13, 8, 9, 4, 5, 11, 12, 7, 2, 14],
    [2, 12, 4, 1, 7, 10, 11, 6, 8, 5, 3, 15, 13, 0, 14, 9,
     14, 11, 2, 12, 4, 7, 13, 1, 5, 0, 15, 10, 3, 9, 8, 6,
     4, 2, 1, 11, 10, 13, 7, 8, 15, 9, 12, 5, 6, 3, 0, 14,
     11, 8, 12, 7, 1, 14, 2, 13, 6, 15, 0, 9, 10, 4, 5, 3],
    [12, 1, 10, 15, 9, 2, 6, 8, 0, 13, 3, 4, 14, 7, 5, 11,
     10, 15, 4, 2, 7, 12, 9, 5, 6, 1, 13, 14, 0, 11, 3, 8,
     9, 14, 15, 5, 2, 8, 12, 3, 7, 0, 4, 10, 1, 13, 11, 6,
     4, 3, 2, 12, 9, 5, 15, 10, 11, 14, 1, 7, 6, 0, 8, 13],
    [4, 11, 2, 14, 15, 0, 8, 13, 3, 12, 9, 7, 5, 10, 6, 1,
     13, 0, 11, 7, 4, 9, 1, 10, 14, 3, 5, 12, 2, 15, 8, 6,
     1, 4, 11, 13, 12, 3, 7, 14, 10, 15, 6, 8, 0, 5, 9, 2,
     6, 11, 13, 8, 1, 4, 10, 7, 9, 5, 0, 15, 14, 2, 3, 12],
    [13, 2, 8, 4, 6, 15, 11, 1, 10, 9, 3, 14, 5, 0, 12, 7,
     1, 15, 13, 8, 10, 3, 7, 4, 12, 5, 6, 11, 0, 14, 9, 2,
     7, 11, 4, 1, 9, 12, 14, 2, 0, 6, 10, 13, 15, 3, 5, 8,
     2, 1, 14, 7, 4, 10, 8, 13, 15, 12, 9, 0, 3, 5, 6, 11],
];

/// Each S-box's four-bit answer for a six-bit input, already through `P` and
/// in place in the 32-bit word, so a round is eight lookups.
static SP: [[u32; 64]; 8] = {
    let mut sp = [[0u32; 64]; 8];
    let mut boxi = 0;
    while boxi < 8 {
        let mut input = 0;
        while input < 64 {
            // The outer two bits pick the row, the inner four the column.
            let row = ((input >> 4) & 2) | (input & 1);
            let col = (input >> 1) & 0xf;
            let nibble = S[boxi][row * 16 + col] as u32;
            let word = nibble << (28 - 4 * boxi);
            let mut out = 0u32;
            let mut i = 0;
            while i < 32 {
                out = (out << 1) | ((word >> (32 - P[i] as u32)) & 1);
                i += 1;
            }
            sp[boxi][input] = out;
            input += 1;
        }
        boxi += 1;
    }
    sp
};

/// The sixteen 48-bit round keys for an eight-byte key whose low bit in each
/// byte is parity, and so ignored.
fn schedule(key: [u8; 8]) -> [u64; 16] {
    const HALF: u32 = 28;
    const HALF_MASK: u64 = (1 << HALF) - 1;
    let pc1 = permute(u64::from_be_bytes(key), 64, &PC1);
    let (mut c, mut d) = (pc1 >> HALF, pc1 & HALF_MASK);
    let rotate = |half: u64, by: u32| ((half << by) | (half >> (HALF - by))) & HALF_MASK;
    SHIFTS.map(|by| {
        c = rotate(c, by);
        d = rotate(d, by);
        permute((c << HALF) | d, 56, &PC2)
    })
}

/// A salt as the E-box swaps it: bit `j` of `salt` exchanges bits `j` and
/// `j + 24` of the expansion, counting from its first bit.
fn salt_mask(salt: u32) -> u64 {
    (0..24)
        .filter(|j| salt & (1 << j) != 0)
        .fold(0, |mask, j| mask | (1 << (23 - j)))
}

/// The round function on the right half.
fn feistel(r: u32, key: u64, salt: u64) -> u32 {
    // E: eight overlapping six-bit groups, each four bits on from the last
    // and wrapping round the word's ends.
    let e = (0..8).fold(0u64, |e, i| {
        (e << 6) | u64::from(r.rotate_right(1).rotate_left(4 * i) >> 26)
    });
    let swap = ((e >> 24) ^ e) & salt;
    let e = e ^ swap ^ (swap << 24) ^ key;
    (0..8).fold(0, |out, i| {
        out | SP[i][((e >> (42 - 6 * i)) & 0x3f) as usize]
    })
}

/// `block` through `rounds` salted DES encryptions with `keys`.
fn encrypt(block: u64, keys: &[u64; 16], salt: u32, rounds: u32) -> u64 {
    let salt = salt_mask(salt);
    let ip = permute(block, 64, &IP);
    let (mut l, mut r) = ((ip >> 32) as u32, ip as u32);
    // The output permutation and the next round's input one cancel, so the
    // halves carry straight over, swapped as DES leaves them.
    for _ in 0..rounds {
        for key in keys {
            (l, r) = (r, l ^ feistel(r, *key, salt));
        }
        (l, r) = (r, l);
    }
    permute((u64::from(l) << 32) | u64::from(r), 64, &FP)
}

/// Up to eight key characters, each shifted left one so its seven bits land
/// above the parity bit; stops at the NUL and returns what follows.
fn key_block(key: &[u8]) -> ([u8; 8], &[u8]) {
    let mut block = [0u8; 8];
    let mut rest = key;
    for byte in &mut block {
        if let Some((&c, tail)) = rest.split_first() {
            *byte = c << 1;
            rest = tail;
        }
    }
    (block, rest)
}

/// A field of `width` alphabet characters from `setting`, least significant
/// first, padded with `.` past its end, and the characters themselves.
fn field(setting: &[u8], width: usize) -> (u32, Vec<u8>) {
    let chars: Vec<u8> = (0..width)
        .map(|i| setting.get(i).copied().unwrap_or(b'.'))
        .collect();
    let value = chars.iter().rev().fold(0, |v, &c| (v << 6) | a64toi(c));
    (value, chars)
}

/// The full answer for `key` and `setting`, without the NUL.
fn hash(key: &[u8], setting: &[u8]) -> Vec<u8> {
    let (mut block, mut rest) = key_block(key);
    let mut keys = schedule(block);
    let mut out = Vec::with_capacity(RESULT_LEN);
    let (rounds, salt) = if setting.first() == Some(&PASSWORD_EFMT1) {
        // The rest of the key, eight characters at a time, folded into the
        // key by encrypting it with itself.
        while !rest.is_empty() {
            block = encrypt(u64::from_be_bytes(block), &keys, 0, 1).to_be_bytes();
            let (next, tail) = key_block(rest);
            for (byte, add) in block.iter_mut().zip(next) {
                *byte ^= add;
            }
            rest = tail;
            keys = schedule(block);
        }
        out.push(PASSWORD_EFMT1);
        let (rounds, text) = field(&setting[1..], 4);
        out.extend(text);
        let (salt, text) = field(setting.get(5..).unwrap_or_default(), 4);
        out.extend(text);
        (rounds, salt)
    } else {
        let (salt, text) = field(setting, 2);
        out.extend(text);
        (TRADITIONAL_ROUNDS, salt)
    };
    let bits = encrypt(0, &keys, salt, rounds);
    // 64 bits as eleven characters, the last two bits padded with zeroes.
    let padded = u128::from(bits) << 2;
    out.extend((0..11).map(|i| ITOA64[(padded >> (60 - 6 * i)) as usize & 0x3f]));
    out
}

thread_local! {
    static RESULT: RefCell<[u8; RESULT_LEN]> = const { RefCell::new([0; RESULT_LEN]) };
}

/// `char *crypt(const char *key, const char *setting)`
///
/// The answer lives in storage the next call on this thread overwrites.
///
/// # Safety
///
/// `key` and `setting` are NUL-terminated strings.
pub unsafe extern "C" fn crypt(key: *const c_char, setting: *const c_char) -> *mut c_char {
    // SAFETY: the caller's strings.
    let (key, setting) = unsafe { (CStr::from_ptr(key), CStr::from_ptr(setting)) };
    let answer = hash(key.to_bytes(), setting.to_bytes());
    RESULT.with_borrow_mut(|result| {
        result.fill(0);
        result[..answer.len()].copy_from_slice(&answer);
        result.as_mut_ptr().cast()
    })
}

exports::exports! {
    functions {
        "crypt" => crypt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crypt_of(key: &str, setting: &str) -> String {
        String::from_utf8(hash(key.as_bytes(), setting.as_bytes())).unwrap()
    }

    #[test]
    fn unsalted_one_round_is_plain_des() {
        use des::cipher::{BlockCipherEncrypt, KeyInit};
        let key = 0x1334_5779_9BBC_DFF1u64.to_be_bytes();
        let plain = 0x0123_4567_89AB_CDEFu64;
        let mut block = plain.to_be_bytes().into();
        des::Des::new(&key.into()).encrypt_block(&mut block);
        let expected = u64::from_be_bytes(block.into());
        assert_eq!(expected, 0x85E8_1354_0F0A_B405);
        assert_eq!(encrypt(plain, &schedule(key), 0, 1), expected);
    }

    #[test]
    fn hashes_traditional_settings() {
        assert_eq!(crypt_of("password", "ab"), "abJnggxhB/yWI");
        assert_eq!(crypt_of("", ".."), "..X8NBuQ4l6uQ");
    }

    #[test]
    fn hashes_a_dollar_setting_as_a_des_salt() {
        let hashed = crypt_of("password", "$1$saltsalt$");
        assert_eq!(hashed.len(), 13);
        assert_eq!(&hashed[..2], "$1");
        assert_eq!(hashed, crypt_of("password", "$1"));
    }

    #[test]
    fn ignores_key_characters_past_eight() {
        assert_eq!(crypt_of("password", "ab"), crypt_of("password123", "ab"));
    }

    #[test]
    fn hashes_extended_settings() {
        assert_eq!(crypt_of("U*U*U*U*", "_J9..CCCC"), "_J9..CCCCXBrJUJV154M");
        assert_eq!(crypt_of("U*U***U", "_J9..CCCC"), "_J9..CCCCXUhOBTXzaiE");
        assert_eq!(
            crypt_of("*U*U*U*U*U*U*U*U", "_J9..XXXX"),
            "_J9..XXXXVL7qJCnku0I"
        );
    }

    #[test]
    fn pads_a_short_setting_with_dots() {
        assert_eq!(crypt_of("password", ""), crypt_of("password", ".."));
        assert!(crypt_of("password", "").starts_with(".."));
    }
}
