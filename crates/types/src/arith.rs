//! Checked arithmetic, comparison and casts of the numeric types: nothing
//! wraps, truncates or rounds silently, and every failure is a typed
//! [`Error`] (document 03 section 2).
//!
//! # Rules
//!
//! Every operation computes the exact result first, then fits it to the
//! result type. A result outside the type's range is [`Error::Overflow`]; a
//! result with more fractional digits (or, for a float, significant bits)
//! than the type holds is [`Error::LossOfPrecision`]. When both apply,
//! `LossOfPrecision` is returned. A zero divisor is [`Error::DivisionByZero`].
//!
//! [`binary`], [`negate`] and [`compare`] take operands of one integer or
//! Decimal type and return that same type: there is no implicit conversion,
//! so `Decimal(38, 2) + Decimal(38, 4)` needs an explicit [`cast`] first.
//! Floats have casts only. A Decimal(P, S) operand whose mantissa has more
//! than P digits is outside the type and gives `Overflow`.
//!
//! | Operation | Integers | Decimal(P, S): result scale S, mantissas `a`, `b` |
//! |---|---|---|
//! | `Add`, `Sub` | exact | `a ± b`, exact |
//! | `Mul` | exact | `a * b / 10^S`; `LossOfPrecision` if that is not whole |
//! | `Div` | truncated toward zero, as `Rem` gives what it drops | `a * 10^S / b`; `LossOfPrecision` if that is not whole |
//! | `Rem` | sign of the dividend: `a - b * trunc(a / b)`, so `MIN % -1 = 0` | `a % b`, the same rule, exact |
//! | [`negate`] | exact; `Overflow` for `MIN` and for a nonzero unsigned value | exact, as the range is symmetric |
//!
//! A result fits Decimal(P, S) when its mantissa has at most P digits.
//!
//! [`cast`] converts between any two of the integer, Decimal, f32 and f64
//! types and succeeds only when the value is unchanged: a fraction dropped by
//! a cast to an integer or to a smaller scale, a decimal fraction that is not
//! a binary fraction (0.1 to a float), a float bit or exponent beyond the
//! target float, and NaN to a non-float are `LossOfPrecision`; a value beyond
//! the target's range, infinity to a non-float included, is `Overflow`.
//! Between floats NaN stays NaN (canonical) and infinity stays infinity.

use crate::{CANONICAL_NAN_F32, Error, I256, Type, U256, Value, canonical_f32, canonical_f64};
use std::cmp::Ordering;

/// A binary arithmetic operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// `a op b` for two values of the integer or Decimal type `ty`, as a value of `ty`.
pub fn binary(op: Op, ty: &Type, a: &Value, b: &Value) -> Result<Value, Error> {
    let (decimal, scale) = match kind(ty)? {
        Kind::Int => (false, 0),
        Kind::Decimal(scale) => (true, scale),
        Kind::F32 | Kind::F64 => return Err(Error::Mismatch),
    };
    let (x, y) = (exact(ty, a)?, exact(ty, b)?);
    if matches!(op, Op::Div | Op::Rem) && y.mag == Mag::ZERO {
        return Err(Error::DivisionByZero);
    }
    let r = match op {
        Op::Add => x.add(y),
        Op::Sub => x.add(y.negate()),
        Op::Mul if decimal => whole(x.mul(y), Num::pos(pow(10, scale)))?,
        Op::Mul => x.mul(y),
        Op::Div if decimal => whole(x.mul(Num::pos(pow(10, scale))), y)?,
        Op::Div => x.div_rem(y).0,
        Op::Rem => x.div_rem(y).1,
    };
    fit(ty, r)
}

/// `-a` for a value of the integer or Decimal type `ty`.
pub fn negate(ty: &Type, a: &Value) -> Result<Value, Error> {
    if matches!(kind(ty)?, Kind::F32 | Kind::F64) {
        return Err(Error::Mismatch);
    }
    fit(ty, exact(ty, a)?.negate())
}

/// The numeric order of two values of the integer or Decimal type `ty`.
pub fn compare(ty: &Type, a: &Value, b: &Value) -> Result<Ordering, Error> {
    if matches!(kind(ty)?, Kind::F32 | Kind::F64) {
        return Err(Error::Mismatch);
    }
    Ok(exact(ty, a)?.cmp(&exact(ty, b)?))
}

/// `value`, of the numeric type `from`, as a value of the numeric type `to`;
/// an error unless the value is unchanged.
pub fn cast(from: &Type, value: &Value, to: &Type) -> Result<Value, Error> {
    let (source, target) = (kind(from)?, kind(to)?);
    from.check(value)?;
    let x = match (source, value) {
        (Kind::F32, Value::F32(x)) => f64::from(*x),
        (Kind::F64, Value::F64(x)) => *x,
        (Kind::Int | Kind::Decimal(_), _) => {
            let m = exact(from, value)?;
            return match target {
                Kind::Int | Kind::Decimal(_) => {
                    fit(to, rescale(m, source.scale(), target.scale())?)
                }
                Kind::F32 | Kind::F64 => {
                    // m / 10^s is a binary fraction only if 5^s divides m.
                    let s = source.scale();
                    let (n, rest) = m.mag.div_rem(pow(5, s));
                    if rest != Mag::ZERO {
                        return Err(Error::LossOfPrecision);
                    }
                    float(target, m.neg, n, -i32::from(s))
                }
            };
        }
        _ => unreachable!("check() matched the value to its float type"),
    };
    match target {
        Kind::F64 => Ok(Value::F64(canonical_f64(x))),
        Kind::F32 if x.is_nan() => Ok(Value::F32(f32::from_bits(CANONICAL_NAN_F32))),
        Kind::F32 if x.is_infinite() => Ok(Value::F32(if x > 0.0 {
            f32::INFINITY
        } else {
            f32::NEG_INFINITY
        })),
        _ if x.is_nan() => Err(Error::LossOfPrecision),
        _ if x.is_infinite() => Err(Error::Overflow),
        Kind::F32 => {
            let (neg, n, e) = split_f64(x);
            float(target, neg, Mag::from_u128(u128::from(n)), e)
        }
        Kind::Int | Kind::Decimal(_) => {
            // x * 10^t = n * 10^t * 2^e must be whole.
            let (neg, n, e) = split_f64(x);
            let t = Mag::from_u128(u128::from(n)).mul(pow(10, target.scale()));
            let shift = e.unsigned_abs();
            let mag = if t == Mag::ZERO {
                t
            } else if e >= 0 {
                if t.bits() + shift > Mag::BITS {
                    return Err(Error::Overflow);
                }
                t.shl(shift)
            } else if t.trailing_zeros() >= shift {
                t.shr(shift)
            } else {
                return Err(Error::LossOfPrecision);
            };
            fit(to, Num::new(neg, mag))
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Int,
    Decimal(u8),
    F32,
    F64,
}

impl Kind {
    /// Fractional decimal digits: 0 for an integer.
    fn scale(self) -> u8 {
        match self {
            Kind::Decimal(s) => s,
            _ => 0,
        }
    }
}

/// The numeric kind of a valid type; Nullable and the other types are `Mismatch`.
fn kind(ty: &Type) -> Result<Kind, Error> {
    ty.validate()?;
    Ok(match ty.logical() {
        Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::U128
        | Type::U256
        | Type::I8
        | Type::I16
        | Type::I32
        | Type::I64
        | Type::I128
        | Type::I256 => Kind::Int,
        Type::Decimal { scale, .. } => Kind::Decimal(*scale),
        Type::F32 => Kind::F32,
        Type::F64 => Kind::F64,
        _ => return Err(Error::Mismatch),
    })
}

/// The exact value of an integer, or the mantissa of a Decimal.
fn exact(ty: &Type, v: &Value) -> Result<Num, Error> {
    ty.check(v)?;
    let n = match v {
        Value::U8(x) => Num::pos(Mag::from_u128(u128::from(*x))),
        Value::U16(x) => Num::pos(Mag::from_u128(u128::from(*x))),
        Value::U32(x) => Num::pos(Mag::from_u128(u128::from(*x))),
        Value::U64(x) => Num::pos(Mag::from_u128(u128::from(*x))),
        Value::U128(x) => Num::pos(Mag::from_u128(*x)),
        Value::U256(x) => Num::pos(Mag::from_halves(x.hi, x.lo)),
        Value::I8(x) => Num::int(i128::from(*x)),
        Value::I16(x) => Num::int(i128::from(*x)),
        Value::I32(x) | Value::Decimal32(x) => Num::int(i128::from(*x)),
        Value::I64(x) | Value::Decimal64(x) => Num::int(i128::from(*x)),
        Value::I128(x) | Value::Decimal128(x) => Num::int(*x),
        Value::I256(x) | Value::Decimal256(x) => {
            if x.hi >= 0 {
                Num::pos(Mag::from_halves(x.hi.unsigned_abs(), x.lo))
            } else {
                // hi * 2^128 + lo = -(|hi| * 2^128 - lo), and |hi| * 2^128 > lo.
                let big = Mag::from_halves(x.hi.unsigned_abs(), 0);
                Num::new(true, big.sub(Mag::from_u128(x.lo)))
            }
        }
        _ => unreachable!("kind() admits integers and decimals only"),
    };
    if let Type::Decimal { precision, .. } = ty.logical()
        && n.mag >= pow(10, *precision)
    {
        return Err(Error::Overflow);
    }
    Ok(n)
}

/// `n` as a value of the integer or Decimal type `ty`, or `Overflow`.
fn fit(ty: &Type, n: Num) -> Result<Value, Error> {
    use Value as V;
    let u = || n.unsigned();
    let i = || n.signed();
    let v = match ty.logical() {
        Type::U8 => u().and_then(|x| x.try_into().ok()).map(V::U8),
        Type::U16 => u().and_then(|x| x.try_into().ok()).map(V::U16),
        Type::U32 => u().and_then(|x| x.try_into().ok()).map(V::U32),
        Type::U64 => u().and_then(|x| x.try_into().ok()).map(V::U64),
        Type::U128 => u().map(V::U128),
        Type::U256 => n.u256().map(V::U256),
        Type::I8 => i().and_then(|x| x.try_into().ok()).map(V::I8),
        Type::I16 => i().and_then(|x| x.try_into().ok()).map(V::I16),
        Type::I32 => i().and_then(|x| x.try_into().ok()).map(V::I32),
        Type::I64 => i().and_then(|x| x.try_into().ok()).map(V::I64),
        Type::I128 => i().map(V::I128),
        Type::I256 => n.i256().map(V::I256),
        Type::Decimal { precision, .. } if n.mag >= pow(10, *precision) => None,
        Type::Decimal { precision, .. } => match crate::decimal_width(*precision) {
            4 => i().and_then(|x| x.try_into().ok()).map(V::Decimal32),
            8 => i().and_then(|x| x.try_into().ok()).map(V::Decimal64),
            16 => i().map(V::Decimal128),
            _ => n.i256().map(V::Decimal256),
        },
        _ => unreachable!("kind() admits integers and decimals only"),
    };
    v.ok_or(Error::Overflow)
}

/// `n / d` when that is whole, else `LossOfPrecision`.
fn whole(n: Num, d: Num) -> Result<Num, Error> {
    let (q, r) = n.div_rem(d);
    if r.mag == Mag::ZERO {
        Ok(q)
    } else {
        Err(Error::LossOfPrecision)
    }
}

/// A mantissa at scale `from` as a mantissa at scale `to`.
fn rescale(m: Num, from: u8, to: u8) -> Result<Num, Error> {
    if to >= from {
        Ok(m.mul(Num::pos(pow(10, to - from))))
    } else {
        whole(m, Num::pos(pow(10, from - to)))
    }
}

/// `base^k`, for the bases 5 and 10 and `k` up to the largest decimal precision.
fn pow(base: u128, k: u8) -> Mag {
    assert!(matches!(base, 5 | 10) && u64::from(k) <= crate::FORMAT.decimal_precision_max);
    // 10^38 and 5^38 fit a u128, and 76 is the largest k.
    let (h, l) = (u32::from(k / 2), u32::from(k - k / 2));
    Mag::from_u128(base.pow(h)).mul(Mag::from_u128(base.pow(l)))
}

/// A finite f64 as `(negative, n, e)` with `|x| = n * 2^e`.
fn split_f64(x: f64) -> (bool, u64, i32) {
    let bits = x.to_bits();
    let exp = i32::try_from((bits >> 52) & 0x7ff).expect("11 bits");
    let frac = bits & ((1 << 52) - 1);
    assert!(exp != 0x7ff, "the caller handles NaN and infinities");
    let neg = bits >> 63 == 1;
    if exp == 0 {
        (neg, frac, -1074)
    } else {
        (neg, frac | (1 << 52), exp - 1075)
    }
}

/// The float of kind `target` equal to `(-1)^neg * n * 2^e`.
fn float(target: Kind, neg: bool, n: Mag, e: i32) -> Result<Value, Error> {
    // Stored fraction bits, exponent bias and sign bit of the format.
    let (frac_bits, bias, sign_bit) = match target {
        Kind::F32 => (23, 127, 31),
        _ => (52, 1023, 63),
    };
    let sign = u64::from(neg) << sign_bit;
    let bits = if n == Mag::ZERO {
        sign
    } else {
        let tz = n.trailing_zeros();
        let (n, e) = (n.shr(tz), e + i32::try_from(tz).expect("at most 512"));
        let len = n.bits();
        // The weight of the lowest bit of the smallest subnormal.
        let e_min = 1 - bias - i32::try_from(frac_bits).expect("small");
        if len > frac_bits + 1 || e < e_min {
            return Err(Error::LossOfPrecision);
        }
        let top = e + i32::try_from(len).expect("at most 53") - 1;
        if top > bias {
            return Err(Error::Overflow);
        }
        let m = u64::try_from(n.unsigned_low()).expect("at most 53 bits");
        if top >= 1 - bias {
            let biased = u64::try_from(top + bias).expect("positive");
            let frac = (m << (frac_bits + 1 - len)) - (1 << frac_bits);
            sign | (biased << frac_bits) | frac
        } else {
            sign | (m << (e - e_min))
        }
    };
    Ok(match target {
        Kind::F32 => Value::F32(canonical_f32(f32::from_bits(
            u32::try_from(bits).expect("32 bits"),
        ))),
        _ => Value::F64(canonical_f64(f64::from_bits(bits))),
    })
}

/// An unsigned 512-bit integer, least significant limb first: wide enough
/// for the product of two 256-bit values and for any 76-digit mantissa
/// times 10^76.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mag([u64; 8]);

impl Ord for Mag {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.iter().rev().cmp(other.0.iter().rev())
    }
}

impl PartialOrd for Mag {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The low and high 64 bits of `x`.
fn halves(x: u128) -> (u64, u64) {
    let low = u64::try_from(x & u128::from(u64::MAX)).expect("masked to 64 bits");
    (low, u64::try_from(x >> 64).expect("shifted to 64 bits"))
}

impl Mag {
    const ZERO: Mag = Mag([0; 8]);
    const BITS: u32 = 8 * u64::BITS;

    fn from_u128(x: u128) -> Mag {
        Mag::from_halves(0, x)
    }

    /// `hi * 2^128 + lo`.
    fn from_halves(hi: u128, lo: u128) -> Mag {
        let mut m = Mag::ZERO;
        (m.0[0], m.0[1]) = halves(lo);
        (m.0[2], m.0[3]) = halves(hi);
        m
    }

    /// The low 128 bits.
    fn unsigned_low(self) -> u128 {
        (u128::from(self.0[1]) << 64) | u128::from(self.0[0])
    }

    /// Bits 128 to 255.
    fn second_u128(self) -> u128 {
        (u128::from(self.0[3]) << 64) | u128::from(self.0[2])
    }

    fn below_bits(self, bits: u32) -> bool {
        self.bits() <= bits
    }

    /// The number of significant bits.
    fn bits(self) -> u32 {
        let mut bits = Mag::BITS;
        for limb in self.0.iter().rev() {
            if *limb != 0 {
                return bits - limb.leading_zeros();
            }
            bits -= 64;
        }
        0
    }

    /// Trailing zero bits; 512 for zero.
    fn trailing_zeros(self) -> u32 {
        let mut zeros = 0;
        for limb in self.0 {
            if limb != 0 {
                return zeros + limb.trailing_zeros();
            }
            zeros += 64;
        }
        zeros
    }

    fn add(self, other: Mag) -> Mag {
        let mut out = Mag::ZERO;
        let mut carry = 0;
        for (o, (a, b)) in out.0.iter_mut().zip(self.0.iter().zip(other.0)) {
            let s = u128::from(*a) + u128::from(b) + carry;
            (*o, _) = halves(s);
            carry = s >> 64;
        }
        assert!(carry == 0, "sums of operands below 2^511 fit");
        out
    }

    /// `self - other`, for `self >= other`.
    fn sub(self, other: Mag) -> Mag {
        assert!(self >= other);
        let mut out = Mag::ZERO;
        let mut borrow = 0;
        for (o, (a, b)) in out.0.iter_mut().zip(self.0.iter().zip(other.0)) {
            // 2^64 + a - b - borrow is in [1, 2^65): bit 64 clear means a borrow.
            let d = (1 << 64) + u128::from(*a) - u128::from(b) - borrow;
            (*o, _) = halves(d);
            borrow = 1 - (d >> 64);
        }
        out
    }

    fn mul(self, other: Mag) -> Mag {
        let mut out = [0u64; 16];
        for (i, a) in self.0.iter().enumerate().filter(|(_, a)| **a != 0) {
            let mut carry = 0;
            for (j, b) in other.0.iter().enumerate() {
                let t = u128::from(*a) * u128::from(*b) + u128::from(out[i + j]) + carry;
                (out[i + j], _) = halves(t);
                carry = t >> 64;
            }
            (out[i + 8], _) = halves(carry);
        }
        assert!(
            out[8..].iter().all(|l| *l == 0),
            "products of operands below 2^256 fit"
        );
        Mag(out[..8].try_into().expect("8 limbs"))
    }

    /// Quotient and remainder.
    fn div_rem(self, d: Mag) -> (Mag, Mag) {
        assert!(
            d != Mag::ZERO && d.below_bits(Mag::BITS - 1),
            "doubling the remainder fits"
        );
        if self.below_bits(128) {
            let (a, b) = (self.unsigned_low(), d.unsigned_low());
            return if d.below_bits(128) {
                (Mag::from_u128(a / b), Mag::from_u128(a % b))
            } else {
                (Mag::ZERO, self)
            };
        }
        if d.below_bits(64) {
            // Short division, one limb at a time: each quotient limb is below 2^64.
            let (mut q, mut r, d) = (Mag::ZERO, 0, u128::from(d.0[0]));
            for (ql, limb) in q.0.iter_mut().zip(self.0).rev() {
                let t = (r << 64) | u128::from(limb);
                (*ql, _) = halves(t / d);
                r = t % d;
            }
            return (q, Mag::from_u128(r));
        }
        // shortcut: long division one bit per step, a few microseconds for 512 bits; use word division if it shows in profiles.
        let (mut q, mut r) = (Mag::ZERO, Mag::ZERO);
        for limb in self.0.into_iter().rev().skip_while(|l| *l == 0) {
            for bit in (0..64).rev() {
                r = r.shl(1);
                r.0[0] |= (limb >> bit) & 1;
                q = q.shl(1);
                if r >= d {
                    r = r.sub(d);
                    q.0[0] |= 1;
                }
            }
        }
        (q, r)
    }

    /// `self * 2^k`; the result must fit.
    fn shl(mut self, k: u32) -> Mag {
        assert!(self == Mag::ZERO || self.bits() + k <= Mag::BITS);
        for _ in 0..k {
            let mut carry = 0;
            for limb in self.0.iter_mut() {
                let top = *limb >> 63;
                *limb = (*limb << 1) | carry;
                carry = top;
            }
        }
        self
    }

    /// `self / 2^k`, rounded down.
    fn shr(mut self, k: u32) -> Mag {
        for _ in 0..k.min(Mag::BITS) {
            let mut carry = 0;
            for limb in self.0.iter_mut().rev() {
                let low = *limb & 1;
                *limb = (*limb >> 1) | (carry << 63);
                carry = low;
            }
        }
        self
    }
}

/// A signed integer below 2^512 in magnitude; zero is never negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Num {
    neg: bool,
    mag: Mag,
}

impl Ord for Num {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.neg, other.neg) {
            (false, false) => self.mag.cmp(&other.mag),
            (true, true) => other.mag.cmp(&self.mag),
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
        }
    }
}

impl PartialOrd for Num {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Num {
    fn new(neg: bool, mag: Mag) -> Num {
        Num {
            neg: neg && mag != Mag::ZERO,
            mag,
        }
    }

    fn pos(mag: Mag) -> Num {
        Num { neg: false, mag }
    }

    fn int(x: i128) -> Num {
        Num::new(x < 0, Mag::from_u128(x.unsigned_abs()))
    }

    fn negate(self) -> Num {
        Num::new(!self.neg, self.mag)
    }

    fn add(self, other: Num) -> Num {
        if self.neg == other.neg {
            Num::new(self.neg, self.mag.add(other.mag))
        } else if self.mag >= other.mag {
            Num::new(self.neg, self.mag.sub(other.mag))
        } else {
            Num::new(other.neg, other.mag.sub(self.mag))
        }
    }

    fn mul(self, other: Num) -> Num {
        Num::new(self.neg != other.neg, self.mag.mul(other.mag))
    }

    /// Quotient truncated toward zero, and the remainder, with the dividend's sign.
    fn div_rem(self, d: Num) -> (Num, Num) {
        let (q, r) = self.mag.div_rem(d.mag);
        (Num::new(self.neg != d.neg, q), Num::new(self.neg, r))
    }

    fn unsigned(self) -> Option<u128> {
        (!self.neg && self.mag.below_bits(128)).then(|| self.mag.unsigned_low())
    }

    fn signed(self) -> Option<i128> {
        let m = self.mag.below_bits(128).then(|| self.mag.unsigned_low())?;
        if self.neg {
            0i128.checked_sub_unsigned(m)
        } else {
            i128::try_from(m).ok()
        }
    }

    fn u256(self) -> Option<U256> {
        (!self.neg && self.mag.below_bits(2 * u128::BITS)).then(|| U256 {
            hi: self.mag.second_u128(),
            lo: self.mag.unsigned_low(),
        })
    }

    fn i256(self) -> Option<I256> {
        if !self.mag.below_bits(2 * u128::BITS) {
            return None;
        }
        let (h, l) = (self.mag.second_u128(), self.mag.unsigned_low());
        Some(match (self.neg, l) {
            (false, _) => I256 {
                hi: i128::try_from(h).ok()?,
                lo: l,
            },
            (true, 0) => I256 {
                hi: 0i128.checked_sub_unsigned(h)?,
                lo: 0,
            },
            // -(h * 2^128 + l) = -(h + 1) * 2^128 + (2^128 - l).
            (true, _) => I256 {
                hi: (-1i128).checked_sub_unsigned(h)?,
                lo: u128::MAX - l + 1,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mag_arithmetic() {
        let max = Mag::from_halves(u128::MAX, u128::MAX);
        let sq = max.mul(max);
        assert_eq!(sq.bits(), Mag::BITS);
        // (2M + 1)^2 = (4M + 4) * M + 1, with M = 2^255 - 1.
        let (q, r) = sq.div_rem(max.shr(1));
        assert_eq!(q, Mag::from_u128(1).shl(257));
        assert_eq!(r, Mag::from_u128(1));
        assert_eq!(sq.div_rem(max), (max, Mag::ZERO));
        assert_eq!(max.add(Mag::from_u128(1)).sub(max), Mag::from_u128(1));
        assert_eq!(Mag::ZERO.trailing_zeros(), Mag::BITS);
        assert_eq!(pow(10, 76).bits(), 253);
    }
}
