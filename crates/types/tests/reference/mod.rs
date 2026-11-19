//! The reference the arithmetic tests compare with: an arbitrary-precision
//! signed integer over a vector of 32-bit limbs, written independently of the
//! fixed 512-bit arithmetic in `granite_types::arith`, plus the conversions
//! between it and the numeric values, and a seeded generator.

#![allow(dead_code, reason = "each test file uses part of this module")]

use granite_types::{I256, Type, U256, Value, decimal_width};
use std::cmp::Ordering;

/// Sign and magnitude, least significant limb first, without high zero limbs;
/// zero is never negative.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Big {
    neg: bool,
    mag: Vec<u32>,
}

impl Ord for Big {
    fn cmp(&self, o: &Self) -> Ordering {
        match (self.neg, o.neg) {
            (false, false) => mag_cmp(&self.mag, &o.mag),
            (true, true) => mag_cmp(&o.mag, &self.mag),
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
        }
    }
}

impl PartialOrd for Big {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Big {
    fn new(neg: bool, mut mag: Vec<u32>) -> Big {
        while mag.last() == Some(&0) {
            mag.pop();
        }
        Big {
            neg: neg && !mag.is_empty(),
            mag,
        }
    }

    pub fn zero() -> Big {
        Big::new(false, vec![])
    }

    pub fn from_u128(x: u128) -> Big {
        Big::new(false, (0..4).map(|i| (x >> (32 * i)) as u32).collect())
    }

    pub fn from_i128(x: i128) -> Big {
        Big::new(x < 0, Big::from_u128(x.unsigned_abs()).mag)
    }

    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }

    pub fn is_neg(&self) -> bool {
        self.neg
    }

    pub fn neg(&self) -> Big {
        Big::new(!self.neg, self.mag.clone())
    }

    pub fn abs(&self) -> Big {
        Big::new(false, self.mag.clone())
    }

    pub fn add(&self, o: &Big) -> Big {
        if self.neg == o.neg {
            Big::new(self.neg, mag_add(&self.mag, &o.mag))
        } else if mag_cmp(&self.mag, &o.mag) != Ordering::Less {
            Big::new(self.neg, mag_sub(&self.mag, &o.mag))
        } else {
            Big::new(o.neg, mag_sub(&o.mag, &self.mag))
        }
    }

    pub fn sub(&self, o: &Big) -> Big {
        self.add(&o.neg())
    }

    pub fn mul(&self, o: &Big) -> Big {
        let mut out = vec![0u32; self.mag.len() + o.mag.len()];
        for (i, &a) in self.mag.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &b) in o.mag.iter().enumerate() {
                let t = u64::from(a) * u64::from(b) + u64::from(out[i + j]) + carry;
                out[i + j] = t as u32;
                carry = t >> 32;
            }
            out[i + o.mag.len()] = carry as u32;
        }
        Big::new(self.neg != o.neg, out)
    }

    /// Quotient truncated toward zero, remainder with the dividend's sign.
    pub fn div_rem(&self, d: &Big) -> (Big, Big) {
        assert!(!d.is_zero());
        let (q, r) = mag_div_rem(&self.mag, &d.mag);
        (Big::new(self.neg != d.neg, q), Big::new(self.neg, r))
    }

    /// `base^k`, by squaring.
    pub fn pow(base: u32, k: u32) -> Big {
        let (mut acc, mut sq, mut k) = (Big::from_u128(1), Big::from_u128(u128::from(base)), k);
        while k > 0 {
            if k & 1 == 1 {
                acc = acc.mul(&sq);
            }
            sq = sq.mul(&sq);
            k >>= 1;
        }
        acc
    }

    /// The 256-bit two's complement `hi * 2^128 + lo`, or unsigned.
    pub fn from_bits256(hi: u128, lo: u128, signed: bool) -> Big {
        let limbs = (0..8).map(|i| ((if i < 4 { lo } else { hi }) >> (32 * (i % 4))) as u32);
        let x = Big::new(false, limbs.collect());
        if signed && hi >> 127 == 1 {
            x.sub(&Big::pow(2, 2 * u128::BITS))
        } else {
            x
        }
    }

    /// The number of significant bits of the magnitude.
    pub fn bits(&self) -> u32 {
        self.mag
            .last()
            .map_or(0, |l| 32 * self.mag.len() as u32 - l.leading_zeros())
    }

    pub fn is_even(&self) -> bool {
        self.mag.first().is_none_or(|l| l % 2 == 0)
    }

    pub fn to_u128(&self) -> Option<u128> {
        (!self.neg && self.mag.len() <= 4).then(|| {
            self.mag
                .iter()
                .rev()
                .fold(0u128, |acc, l| (acc << 32) | u128::from(*l))
        })
    }

    pub fn to_i128(&self) -> Option<i128> {
        let m = self.abs().to_u128()?;
        if self.neg {
            0i128.checked_sub_unsigned(m)
        } else {
            i128::try_from(m).ok()
        }
    }

    /// The low 256 bits of the two's complement of `self`, as (hi, lo).
    fn bits256(&self) -> (u128, u128) {
        let t = if self.neg {
            self.add(&Big::pow(2, 2 * u128::BITS))
        } else {
            self.clone()
        };
        let limb = |i: usize| u128::from(t.mag.get(i).copied().unwrap_or(0));
        let half = |at: usize| (0..4).fold(0u128, |acc, i| acc | (limb(at + i) << (32 * i)));
        (half(4), half(0))
    }
}

fn mag_cmp(a: &[u32], b: &[u32]) -> Ordering {
    a.len()
        .cmp(&b.len())
        .then_with(|| a.iter().rev().cmp(b.iter().rev()))
}

fn mag_add(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
    let mut carry = 0u64;
    for i in 0..a.len().max(b.len()) {
        let t = u64::from(*a.get(i).unwrap_or(&0)) + u64::from(*b.get(i).unwrap_or(&0)) + carry;
        out.push(t as u32);
        carry = t >> 32;
    }
    out.push(carry as u32);
    out
}

/// `a - b` for `a >= b`.
fn mag_sub(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0i64;
    for (i, &x) in a.iter().enumerate() {
        let mut t = i64::from(x) - i64::from(*b.get(i).unwrap_or(&0)) - borrow;
        borrow = i64::from(t < 0);
        if t < 0 {
            t += 1 << 32;
        }
        out.push(t as u32);
    }
    assert_eq!(borrow, 0);
    out
}

fn mag_div_rem(a: &[u32], d: &[u32]) -> (Vec<u32>, Vec<u32>) {
    if let [d] = d {
        // Short division by one limb.
        let mut q = vec![0u32; a.len()];
        let mut r = 0u64;
        for i in (0..a.len()).rev() {
            let t = (r << 32) | u64::from(a[i]);
            q[i] = (t / u64::from(*d)) as u32;
            r = t % u64::from(*d);
        }
        return (q, vec![r as u32]);
    }
    // Long division, one bit at a time, on a remainder of fixed length.
    let n = d.len() + 1;
    let mut q = vec![0u32; a.len()];
    let mut r = vec![0u32; n];
    let mut dd = d.to_vec();
    dd.push(0);
    for i in (0..a.len() * 32).rev() {
        let mut carry = (a[i / 32] >> (i % 32)) & 1;
        for limb in r.iter_mut() {
            let top = *limb >> 31;
            *limb = (*limb << 1) | carry;
            carry = top;
        }
        if r.iter().rev().cmp(dd.iter().rev()) != Ordering::Less {
            let mut borrow = 0i64;
            for (x, y) in r.iter_mut().zip(&dd) {
                let t = i64::from(*x) - i64::from(*y) - borrow;
                borrow = i64::from(t < 0);
                *x = t.rem_euclid(1 << 32) as u32;
            }
            q[i / 32] |= 1 << (i % 32);
        }
    }
    (q, r)
}

/// The exact value of an integer, or the mantissa of a Decimal.
pub fn big(v: &Value) -> Big {
    use Value as V;
    let i256 = |x: &I256| Big::from_bits256(x.hi as u128, x.lo, true);
    match v {
        V::U8(x) => Big::from_u128(u128::from(*x)),
        V::U16(x) => Big::from_u128(u128::from(*x)),
        V::U32(x) => Big::from_u128(u128::from(*x)),
        V::U64(x) => Big::from_u128(u128::from(*x)),
        V::U128(x) => Big::from_u128(*x),
        V::U256(x) => Big::from_bits256(x.hi, x.lo, false),
        V::I8(x) => Big::from_i128(i128::from(*x)),
        V::I16(x) => Big::from_i128(i128::from(*x)),
        V::I32(x) | V::Decimal32(x) => Big::from_i128(i128::from(*x)),
        V::I64(x) | V::Decimal64(x) => Big::from_i128(i128::from(*x)),
        V::I128(x) | V::Decimal128(x) => Big::from_i128(*x),
        V::I256(x) | V::Decimal256(x) => i256(x),
        other => panic!("not an integer or decimal: {other:?}"),
    }
}

/// The smallest and largest value (mantissa, for a Decimal) of an integer or Decimal type.
pub fn range(ty: &Type) -> (Big, Big) {
    let u = |bits| (Big::zero(), Big::pow(2, bits).sub(&Big::from_u128(1)));
    let i = |bits: u32| {
        let half = Big::pow(2, bits - 1);
        (half.neg(), half.sub(&Big::from_u128(1)))
    };
    match ty {
        Type::U8 => u(8),
        Type::U16 => u(16),
        Type::U32 => u(32),
        Type::U64 => u(64),
        Type::U128 => u(128),
        Type::U256 => u(2 * u128::BITS),
        Type::I8 => i(8),
        Type::I16 => i(16),
        Type::I32 => i(32),
        Type::I64 => i(64),
        Type::I128 => i(128),
        Type::I256 => i(2 * u128::BITS),
        Type::Decimal { precision, .. } => {
            let max = Big::pow(10, u32::from(*precision)).sub(&Big::from_u128(1));
            (max.neg(), max)
        }
        other => panic!("not an integer or decimal: {other:?}"),
    }
}

/// Fractional digits: the scale of a Decimal, 0 for an integer.
pub fn scale(ty: &Type) -> u32 {
    match ty {
        Type::Decimal { scale, .. } => u32::from(*scale),
        _ => 0,
    }
}

/// `b` as a value of `ty`, if it is in range.
pub fn value(ty: &Type, b: &Big) -> Option<Value> {
    use Value as V;
    let (lo, hi) = range(ty);
    if *b < lo || *b > hi {
        return None;
    }
    let u = || b.to_u128().unwrap();
    let i = || b.to_i128().unwrap();
    let (h, l) = b.bits256();
    Some(match ty {
        Type::U8 => V::U8(u() as u8),
        Type::U16 => V::U16(u() as u16),
        Type::U32 => V::U32(u() as u32),
        Type::U64 => V::U64(u() as u64),
        Type::U128 => V::U128(u()),
        Type::U256 => V::U256(U256 { hi: h, lo: l }),
        Type::I8 => V::I8(i() as i8),
        Type::I16 => V::I16(i() as i16),
        Type::I32 => V::I32(i() as i32),
        Type::I64 => V::I64(i() as i64),
        Type::I128 => V::I128(i()),
        Type::I256 => V::I256(I256 {
            hi: h as i128,
            lo: l,
        }),
        Type::Decimal { precision, .. } => match decimal_width(*precision) {
            4 => V::Decimal32(i() as i32),
            8 => V::Decimal64(i() as i64),
            16 => V::Decimal128(i()),
            _ => V::Decimal256(I256 {
                hi: h as i128,
                lo: l,
            }),
        },
        other => panic!("not an integer or decimal: {other:?}"),
    })
}

/// SplitMix64, seeded from `GRANITE_SEED` (default 0x5EED); the seed is logged.
pub struct Rng(u64);

impl Rng {
    pub fn new(test: &str) -> Self {
        let seed = std::env::var("GRANITE_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0x5EED);
        println!("{test}: GRANITE_SEED={seed}");
        Rng(seed)
    }

    /// A generator with a fixed seed, for golden vectors.
    pub fn fixed(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// A uniform value below 2^bits.
    pub fn bits(&mut self, bits: u32) -> Big {
        let limbs = (0..bits.div_ceil(32)).map(|_| self.next() as u32).collect();
        let x = Big::new(false, limbs);
        x.div_rem(&Big::pow(2, bits)).1
    }

    /// A value of `ty`, weighted toward its boundaries: the extremes and their
    /// neighbours, powers of two and of ten and their neighbours, small
    /// values, values of a random bit length, and uniform values.
    pub fn sample(&mut self, ty: &Type) -> Value {
        let (lo, hi) = range(ty);
        let width = hi.bits() + 1;
        loop {
            let delta = Big::from_i128(self.below(9) as i128 - 4);
            let sign = |rng: &mut Rng, b: Big| if rng.below(2) == 0 { b } else { b.neg() };
            let b = match self.below(8) {
                0 => [&lo, &hi, &Big::zero()][self.below(3) as usize].clone(),
                1 => [&lo, &hi][self.below(2) as usize].add(&delta),
                2 => {
                    let p = Big::pow(2, self.below(u64::from(width)) as u32);
                    sign(self, p).add(&delta)
                }
                3 => {
                    let digits = width * 3 / 10 + 1;
                    let p = Big::pow(10, self.below(u64::from(digits)) as u32);
                    sign(self, p).add(&delta)
                }
                4 => Big::from_i128(self.below(201) as i128 - 100),
                5 => {
                    let bits = self.below(u64::from(width)) as u32 + 1;
                    let x = self.bits(bits);
                    sign(self, x)
                }
                _ => {
                    // Uniform over the type: its range is within [-2^width, 2^width).
                    let x = self.bits(width);
                    if lo.is_neg() {
                        x.sub(&Big::pow(2, width - 1))
                    } else {
                        x
                    }
                }
            };
            if let Some(v) = value(ty, &b) {
                return v;
            }
        }
    }
}

/// The number of cases per operation: 10,000,000 with `GRANITE_FULL=1`
/// (the release run of `just test-arithmetic`), else `quick`.
pub fn cases(quick: u64) -> u64 {
    if full() { 10_000_000 } else { quick }
}

pub fn full() -> bool {
    std::env::var_os("GRANITE_FULL").is_some()
}
