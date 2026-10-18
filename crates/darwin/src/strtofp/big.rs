//! Just enough arbitrary-precision arithmetic for the exact slow path.

use std::cmp::Ordering;

/// An unsigned integer as little-endian 32-bit limbs, with no zero limb at
/// the top. 32-bit limbs keep every intermediate product in a `u64`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Big(Vec<u32>);

impl Big {
    pub fn from_u64(value: u64) -> Big {
        Big::from_u128(value.into())
    }

    pub fn from_u128(mut value: u128) -> Big {
        let mut limbs = Vec::new();
        while value != 0 {
            limbs.push(value as u32);
            value >>= 32;
        }
        Big(limbs)
    }

    pub fn is_zero(&self) -> bool {
        self.0.is_empty()
    }

    pub fn bit_len(&self) -> u32 {
        match self.0.last() {
            None => 0,
            Some(top) => 32 * self.0.len() as u32 - top.leading_zeros(),
        }
    }

    /// `self = self * m + a`.
    pub fn mul_add(&mut self, m: u64, a: u64) {
        let mut carry = u128::from(a);
        for limb in &mut self.0 {
            let t = u128::from(*limb) * u128::from(m) + carry;
            *limb = t as u32;
            carry = t >> 32;
        }
        while carry != 0 {
            self.0.push(carry as u32);
            carry >>= 32;
        }
    }

    pub fn mul_pow5(&mut self, mut k: u32) {
        // 5^27 is the largest power of five in a u64.
        const POWERS: [u64; 28] = {
            let mut table = [1; 28];
            let mut i = 1;
            while i < table.len() {
                table[i] = table[i - 1] * 5;
                i += 1;
            }
            table
        };
        while k > 0 {
            let step = k.min(27);
            self.mul_add(POWERS[step as usize], 0);
            k -= step;
        }
    }

    pub fn shl(&mut self, bits: u32) {
        if self.is_zero() {
            return;
        }
        let (words, bits) = ((bits / 32) as usize, bits % 32);
        if bits != 0 {
            let mut carry = 0;
            for limb in &mut self.0 {
                let wide = (u64::from(*limb) << bits) | carry;
                *limb = wide as u32;
                carry = wide >> 32;
            }
            if carry != 0 {
                self.0.push(carry as u32);
            }
        }
        self.0.splice(0..0, std::iter::repeat_n(0, words));
    }

    /// The leading 128 bits, as `(top, shift, sticky)` with
    /// `self = (top + e) * 2^shift` and `e` nonzero exactly when `sticky`.
    pub fn top128(&self) -> (u128, i32, bool) {
        let len = self.bit_len();
        if len <= 128 {
            let mut top = 0u128;
            for &limb in self.0.iter().rev() {
                top = (top << 32) | u128::from(limb);
            }
            return (top, 0, false);
        }
        let shift = len - 128;
        let (words, bits) = ((shift / 32) as usize, shift % 32);
        // Up to five limbs cover 128 bits at any bit offset.
        let mut wide = [0u32; 5];
        for (i, slot) in wide.iter_mut().enumerate() {
            *slot = self.0.get(words + i).copied().unwrap_or(0);
        }
        let low = wide[..4]
            .iter()
            .rev()
            .fold(0u128, |acc, &limb| (acc << 32) | u128::from(limb));
        let top = match bits {
            0 => low,
            _ => (low >> bits) | (u128::from(wide[4]) << (128 - bits)),
        };
        let dropped = wide[0] & ((1u32 << bits).wrapping_sub(1)) != 0
            || self.0[..words].iter().any(|&limb| limb != 0);
        (top, shift as i32, dropped)
    }

    /// `self / divisor`, and whether the remainder is nonzero. Knuth's
    /// Algorithm D (TAOCP 4.3.1).
    pub fn div_rem(&self, divisor: &Big) -> (Big, bool) {
        let v = &divisor.0;
        let n = v.len();
        assert!(n > 0, "division by zero");
        if self.0.len() < n {
            return (Big::default(), !self.is_zero());
        }
        if n == 1 {
            let d = u64::from(v[0]);
            let mut q = vec![0u32; self.0.len()];
            let mut rem = 0u64;
            for (i, &limb) in self.0.iter().enumerate().rev() {
                let t = (rem << 32) | u64::from(limb);
                q[i] = (t / d) as u32;
                rem = t % d;
            }
            return (Big(q).trimmed(), rem != 0);
        }

        // Normalize so the divisor's top limb has its high bit set, which
        // makes each trial quotient off by at most two.
        let s = v[n - 1].leading_zeros();
        let mut v = divisor.clone();
        v.shl(s);
        let v = v.0;
        let mut u = self.clone();
        u.shl(s);
        let mut u = u.0;
        u.resize(self.0.len() + 1, 0);
        let m = u.len() - 1 - n;
        let mut q = vec![0u32; m + 1];
        const B: u64 = 1 << 32;
        for j in (0..=m).rev() {
            let numerator = (u64::from(u[j + n]) << 32) | u64::from(u[j + n - 1]);
            let mut qhat = numerator / u64::from(v[n - 1]);
            let mut rhat = numerator % u64::from(v[n - 1]);
            while qhat >= B || qhat * u64::from(v[n - 2]) > ((rhat << 32) | u64::from(u[j + n - 2]))
            {
                qhat -= 1;
                rhat += u64::from(v[n - 1]);
                if rhat >= B {
                    break;
                }
            }
            // u[j..=j+n] -= qhat * v
            let mut borrow = 0i64;
            for i in 0..n {
                let p = qhat * u64::from(v[i]);
                let t = i64::from(u[i + j]) - borrow - (p & 0xffff_ffff) as i64;
                u[i + j] = t as u32;
                borrow = (p >> 32) as i64 - (t >> 32);
            }
            let t = i64::from(u[j + n]) - borrow;
            u[j + n] = t as u32;
            if t < 0 {
                // qhat was one too large: add the divisor back.
                qhat -= 1;
                let mut carry = 0u64;
                for i in 0..n {
                    let t = u64::from(u[i + j]) + u64::from(v[i]) + carry;
                    u[i + j] = t as u32;
                    carry = t >> 32;
                }
                u[j + n] = u[j + n].wrapping_add(carry as u32);
            }
            q[j] = qhat as u32;
        }
        let remainder = u[..n].iter().any(|&limb| limb != 0);
        (Big(q).trimmed(), remainder)
    }

    fn trimmed(mut self) -> Big {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
        self
    }
}

impl PartialOrd for Big {
    fn partial_cmp(&self, other: &Big) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Big {
    fn cmp(&self, other: &Big) -> Ordering {
        self.0
            .len()
            .cmp(&other.0.len())
            .then_with(|| self.0.iter().rev().cmp(other.0.iter().rev()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(big: &Big) -> u128 {
        let (top, shift, sticky) = big.top128();
        assert!(shift == 0 && !sticky, "fits in 128 bits");
        top
    }

    #[test]
    fn divides_like_the_native_integers() {
        let cases: [(u128, u128); 6] = [
            (u128::MAX, 3),
            (u128::MAX, u64::MAX as u128 + 7),
            (0x1234_5678_9abc_def0_1122_3344_5566_7788, 0x1_0000_0001),
            (0xffff_ffff_0000_0000_ffff_ffff, 0xffff_ffff_ffff),
            (5, 7),
            (1 << 100, (1 << 64) - 1),
        ];
        for (n, d) in cases {
            let (q, rem) = Big::from_u128(n).div_rem(&Big::from_u128(d));
            assert_eq!(value(&q), n / d, "{n:x} / {d:x}");
            assert_eq!(rem, n % d != 0, "{n:x} % {d:x}");
        }
    }

    #[test]
    fn divides_large_powers_exactly() {
        // 5^400 * 7 / 5^400 == 7, and one more gives a remainder.
        let mut five = Big::from_u64(1);
        five.mul_pow5(400);
        let mut n = five.clone();
        n.mul_add(7, 0);
        assert_eq!(n.div_rem(&five), (Big::from_u64(7), false));
        n.mul_add(1, 1);
        assert_eq!(n.div_rem(&five), (Big::from_u64(7), true));
    }

    #[test]
    fn top128_keeps_the_leading_bits() {
        let mut big = Big::from_u128(u128::MAX);
        big.shl(37);
        big.mul_add(1, 1);
        let (top, shift, sticky) = big.top128();
        assert_eq!((top, shift, sticky), (u128::MAX, 37, true));
        assert_eq!(big.bit_len(), 165);
    }
}
