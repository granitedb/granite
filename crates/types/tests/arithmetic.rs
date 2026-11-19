//! Checked arithmetic and casts (`granite_types::arith`) against references:
//! exhaustively for the 8-bit and 16-bit types, by seeded property tests for
//! the wider ones, explicitly at every type's minimum and maximum, and over a
//! table of every pair of numeric types for casts.
//!
//! `just test` runs the quick form. `just test-arithmetic` runs the full one in
//! the release profile: every 16-bit pair, and 10,000,000 cases per operation
//! and type. Rerun a failure with `GRANITE_SEED=<seed>`.

mod reference;

use granite_types::arith::{Op, binary, cast, compare, negate};
use granite_types::{CANONICAL_NAN_F32, CANONICAL_NAN_F64, Error, Type, Value};
use reference::{Big, Rng, big, cases, full, range, scale, value};
use std::cmp::Ordering;

const OPS: [Op; 5] = [Op::Add, Op::Sub, Op::Mul, Op::Div, Op::Rem];

fn dec(precision: u8, scale: u8) -> Type {
    Type::Decimal { precision, scale }
}

fn one(x: i128) -> Big {
    Big::from_i128(x)
}

/// What `a op b` must be: the rules of `granite_types::arith`, computed on
/// arbitrary-precision integers.
fn expected(op: Op, ty: &Type, a: &Big, b: &Big) -> Result<Big, Error> {
    let (lo, hi) = range(ty);
    let decimal = matches!(ty, Type::Decimal { .. });
    let unit = Big::pow(10, scale(ty));
    let whole = |n: Big, d: &Big| {
        let (q, r) = n.div_rem(d);
        if r.is_zero() {
            Ok(q)
        } else {
            Err(Error::LossOfPrecision)
        }
    };
    let r = match op {
        Op::Add => a.add(b),
        Op::Sub => a.sub(b),
        Op::Mul if decimal => whole(a.mul(b), &unit)?,
        Op::Mul => a.mul(b),
        _ if b.is_zero() => return Err(Error::DivisionByZero),
        Op::Div if decimal => whole(a.mul(&unit), b)?,
        Op::Div => a.div_rem(b).0,
        Op::Rem => a.div_rem(b).1,
    };
    if r < lo || r > hi {
        Err(Error::Overflow)
    } else {
        Ok(r)
    }
}

fn expected_neg(ty: &Type, a: &Big) -> Result<Big, Error> {
    let (lo, hi) = range(ty);
    let r = a.neg();
    if r < lo || r > hi {
        Err(Error::Overflow)
    } else {
        Ok(r)
    }
}

/// Compares every operation on `a` and `b` with the reference.
fn check(ty: &Type, a: &Value, b: &Value) {
    let (x, y) = (big(a), big(b));
    for op in OPS {
        let got = binary(op, ty, a, b).map(|v| big(&v));
        assert_eq!(got, expected(op, ty, &x, &y), "{ty:?}: {a:?} {op:?} {b:?}");
    }
    let got = negate(ty, a).map(|v| big(&v));
    assert_eq!(got, expected_neg(ty, &x), "{ty:?}: -{a:?}");
    assert_eq!(compare(ty, a, b), Ok(x.cmp(&y)), "{ty:?}: {a:?} cmp {b:?}");
}

/// Every pair of an 8-bit or 16-bit type against i64 arithmetic: values
/// `lo..=hi`, made by `make`. The quick form takes every value as `a` and a
/// sample of them as `b` for the 16-bit types.
fn exhaustive(ty: Type, lo: i64, hi: i64, make: fn(i64) -> Value) {
    let reference = |op: Op, a: i64, b: i64| -> Result<i64, Error> {
        let r = match op {
            Op::Add => a + b,
            Op::Sub => a - b,
            Op::Mul => a * b,
            _ if b == 0 => return Err(Error::DivisionByZero),
            Op::Div => a / b,
            Op::Rem => a % b,
        };
        if (lo..=hi).contains(&r) {
            Ok(r)
        } else {
            Err(Error::Overflow)
        }
    };
    let int = |v: Value| match v {
        Value::U8(x) => i64::from(x),
        Value::U16(x) => i64::from(x),
        Value::I8(x) => i64::from(x),
        Value::I16(x) => i64::from(x),
        other => panic!("{other:?}"),
    };
    let sampled = hi - lo > 255 && !full();
    let bs: Vec<i64> = (lo..=hi)
        .filter(|b| !sampled || b % 4093 == 0 || (b - lo) < 4 || (hi - b) < 4 || b.abs() < 4)
        .collect();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()) as i64;
    std::thread::scope(|s| {
        for t in 0..threads {
            let (bs, ty) = (&bs, &ty);
            s.spawn(move || {
                for a in (lo..=hi).filter(|a| a.rem_euclid(threads) == t) {
                    let va = make(a);
                    let neg = if (lo..=hi).contains(&-a) {
                        Ok(-a)
                    } else {
                        Err(Error::Overflow)
                    };
                    assert_eq!(negate(ty, &va).map(int), neg, "{ty:?}: -{a}");
                    for &b in bs {
                        let vb = make(b);
                        for op in OPS {
                            let got = binary(op, ty, &va, &vb).map(int);
                            assert_eq!(got, reference(op, a, b), "{ty:?}: {a} {op:?} {b}");
                        }
                        assert_eq!(compare(ty, &va, &vb), Ok(a.cmp(&b)), "{ty:?}: {a} cmp {b}");
                    }
                }
            });
        }
    });
}

#[test]
fn exhaustive_u8() {
    exhaustive(Type::U8, 0, 255, |x| Value::U8(x as u8));
}

#[test]
fn exhaustive_i8() {
    exhaustive(Type::I8, -128, 127, |x| Value::I8(x as i8));
}

#[test]
fn exhaustive_u16() {
    exhaustive(Type::U16, 0, 65535, |x| Value::U16(x as u16));
}

#[test]
fn exhaustive_i16() {
    exhaustive(Type::I16, i16::MIN.into(), i16::MAX.into(), |x| {
        Value::I16(x as i16)
    });
}

/// Seeded random pairs, weighted toward the boundaries, for one wider type.
fn property(ty: Type) {
    let mut rng = Rng::new(&format!("{ty:?}"));
    let n = cases(2_000);
    for _ in 0..n {
        let (a, b) = (rng.sample(&ty), rng.sample(&ty));
        check(&ty, &a, &b);
    }
    println!("{ty:?}: {n} cases per operation");
}

macro_rules! properties {
    ($($name:ident: $ty:expr,)*) => {
        $(#[test] fn $name() { property($ty); })*
    };
}

properties! {
    property_u32: Type::U32,
    property_u64: Type::U64,
    property_u128: Type::U128,
    property_u256: Type::U256,
    property_i32: Type::I32,
    property_i64: Type::I64,
    property_i128: Type::I128,
    property_i256: Type::I256,
    property_decimal_9_2: dec(9, 2),
    property_decimal_18_4: dec(18, 4),
    property_decimal_38_10: dec(38, 10),
    property_decimal_38_0: dec(38, 0),
    property_decimal_76_20: dec(76, 20),
    property_decimal_76_76: dec(76, 76),
}

/// Every integer type, and decimals of every width, at small and full scale.
fn exact_types() -> Vec<Type> {
    vec![
        Type::U8,
        Type::U16,
        Type::U32,
        Type::U64,
        Type::U128,
        Type::U256,
        Type::I8,
        Type::I16,
        Type::I32,
        Type::I64,
        Type::I128,
        Type::I256,
        dec(1, 0),
        dec(9, 2),
        dec(9, 9),
        dec(18, 4),
        dec(38, 10),
        dec(38, 38),
        dec(76, 0),
        dec(76, 20),
        dec(76, 76),
    ]
}

/// Every operation at each type's minimum and maximum, with the result
/// written out: `u` is the smallest positive value (mantissa 1).
#[test]
fn every_operation_at_min_and_max() {
    use Error::{DivisionByZero as Zero, LossOfPrecision as Loss, Overflow as Over};
    for ty in exact_types() {
        let (lo, hi) = range(&ty);
        let v = |b: &Big| value(&ty, b).unwrap();
        let (min, max, zero, u) = (v(&lo), v(&hi), v(&Big::zero()), v(&one(1)));
        let signed = lo.is_neg();
        let decimal = matches!(ty, Type::Decimal { .. });
        let op = |op, a: &Value, b: &Value| binary(op, &ty, a, b);
        let ok = |b: Big| Ok(v(&b));
        let ctx = format!("{ty:?}");
        let minus_u = if signed { v(&one(-1)) } else { zero.clone() };

        assert_eq!(op(Op::Add, &max, &u), Err(Over), "{ctx}");
        assert_eq!(op(Op::Add, &max, &zero), ok(hi.clone()), "{ctx}");
        assert_eq!(op(Op::Add, &min, &zero), ok(lo.clone()), "{ctx}");
        assert_eq!(op(Op::Add, &min, &max), ok(lo.add(&hi)), "{ctx}");
        assert_eq!(op(Op::Sub, &min, &u), Err(Over), "{ctx}");
        assert_eq!(op(Op::Sub, &max, &max), ok(Big::zero()), "{ctx}");
        assert_eq!(op(Op::Sub, &min, &min), ok(Big::zero()), "{ctx}");
        let max_minus_min = if signed { Err(Over) } else { ok(hi.clone()) };
        assert_eq!(op(Op::Sub, &max, &min), max_minus_min, "{ctx}");
        if signed {
            assert_eq!(op(Op::Add, &min, &minus_u), Err(Over), "{ctx}");
            assert_eq!(op(Op::Sub, &max, &minus_u), Err(Over), "{ctx}");
        }

        assert_eq!(op(Op::Mul, &max, &zero), ok(Big::zero()), "{ctx}");
        assert_eq!(op(Op::Mul, &min, &zero), ok(Big::zero()), "{ctx}");
        assert_eq!(op(Op::Div, &max, &zero), Err(Zero), "{ctx}");
        assert_eq!(op(Op::Div, &min, &zero), Err(Zero), "{ctx}");
        assert_eq!(op(Op::Rem, &max, &zero), Err(Zero), "{ctx}");
        assert_eq!(op(Op::Rem, &min, &zero), Err(Zero), "{ctx}");
        assert_eq!(op(Op::Rem, &max, &max), ok(Big::zero()), "{ctx}");
        assert_eq!(op(Op::Rem, &max, &u), ok(Big::zero()), "{ctx}");

        if decimal {
            let s = scale(&ty);
            // max * u: the product's mantissa is max / 10^s, which drops digits
            // unless s = 0; max * max overflows or drops digits.
            let max_u = if s == 0 { ok(hi.clone()) } else { Err(Loss) };
            assert_eq!(op(Op::Mul, &max, &u), max_u, "{ctx}");
            let max_max = if s == 0 && hi > one(1) {
                Err(Over)
            } else {
                Err(Loss)
            };
            assert_eq!(op(Op::Mul, &max, &max), max_max, "{ctx}");
            // max / u = max * 10^s, which overflows unless s = 0.
            let div_u = if s == 0 { ok(hi.clone()) } else { Err(Over) };
            assert_eq!(op(Op::Div, &max, &u), div_u, "{ctx}");
            let one_value = value(&ty, &Big::pow(10, s));
            assert_eq!(op(Op::Div, &max, &max).ok(), one_value, "{ctx}");
            assert_eq!(
                op(Op::Div, &min, &max).ok(),
                one_value.map(|o| negate(&ty, &o).unwrap()),
                "{ctx}"
            );
            // The range is symmetric, so min = -max.
            assert_eq!(op(Op::Rem, &min, &max), ok(Big::zero()), "{ctx}");
            assert_eq!(negate(&ty, &max), Ok(min.clone()), "{ctx}");
            assert_eq!(negate(&ty, &min), Ok(max.clone()), "{ctx}");
        } else {
            assert_eq!(op(Op::Mul, &max, &u), ok(hi.clone()), "{ctx}");
            assert_eq!(op(Op::Mul, &min, &u), ok(lo.clone()), "{ctx}");
            assert_eq!(op(Op::Mul, &max, &v(&one(2))), Err(Over), "{ctx}");
            assert_eq!(op(Op::Mul, &max, &max), Err(Over), "{ctx}");
            assert_eq!(op(Op::Div, &max, &u), ok(hi.clone()), "{ctx}");
            assert_eq!(op(Op::Div, &max, &max), ok(one(1)), "{ctx}");
            assert_eq!(op(Op::Div, &u, &max), ok(Big::zero()), "{ctx}");
            assert_eq!(op(Op::Rem, &u, &max), ok(one(1)), "{ctx}");
            if signed {
                assert_eq!(op(Op::Mul, &min, &minus_u), Err(Over), "{ctx}");
                assert_eq!(op(Op::Mul, &max, &minus_u), ok(hi.neg()), "{ctx}");
                assert_eq!(op(Op::Mul, &min, &min), Err(Over), "{ctx}");
                assert_eq!(op(Op::Div, &min, &minus_u), Err(Over), "{ctx}");
                assert_eq!(op(Op::Div, &min, &max), ok(one(-1)), "{ctx}");
                assert_eq!(op(Op::Rem, &min, &minus_u), ok(Big::zero()), "{ctx}");
                assert_eq!(op(Op::Rem, &min, &max), ok(one(-1)), "{ctx}");
                assert_eq!(negate(&ty, &min), Err(Over), "{ctx}");
                assert_eq!(negate(&ty, &max), ok(hi.neg()), "{ctx}");
            } else {
                assert_eq!(op(Op::Rem, &min, &max), ok(Big::zero()), "{ctx}");
                assert_eq!(negate(&ty, &max), Err(Over), "{ctx}");
                assert_eq!(negate(&ty, &min), ok(Big::zero()), "{ctx}");
            }
        }
        assert_eq!(compare(&ty, &min, &max), Ok(Ordering::Less), "{ctx}");
        assert_eq!(compare(&ty, &max, &min), Ok(Ordering::Greater), "{ctx}");
        assert_eq!(compare(&ty, &max, &max), Ok(Ordering::Equal), "{ctx}");
        assert_eq!(compare(&ty, &min, &min), Ok(Ordering::Equal), "{ctx}");
    }
}

#[test]
fn operands_must_belong_to_the_type() {
    // A Decimal(9, 2) mantissa of ten digits is outside the type.
    let ty = dec(9, 2);
    let (big, small) = (Value::Decimal32(1_000_000_000), Value::Decimal32(1));
    assert_eq!(binary(Op::Add, &ty, &big, &small), Err(Error::Overflow));
    assert_eq!(compare(&ty, &small, &big), Err(Error::Overflow));
    assert_eq!(cast(&ty, &big, &Type::I64), Err(Error::Overflow));
    // No implicit conversion, no arithmetic on floats, nullables or non-numbers.
    let u = Value::U8(1);
    assert_eq!(
        binary(Op::Add, &Type::U8, &u, &Value::U16(1)),
        Err(Error::Mismatch)
    );
    assert_eq!(
        binary(Op::Add, &dec(10, 2), &small, &small),
        Err(Error::Mismatch)
    );
    let f = Value::F64(1.0);
    assert_eq!(binary(Op::Add, &Type::F64, &f, &f), Err(Error::Mismatch));
    assert_eq!(negate(&Type::F64, &f), Err(Error::Mismatch));
    let nullable = Type::Nullable(Box::new(Type::U8));
    assert_eq!(binary(Op::Add, &nullable, &u, &u), Err(Error::Mismatch));
    assert_eq!(cast(&nullable, &u, &Type::U16), Err(Error::Mismatch));
    assert_eq!(cast(&Type::U8, &u, &Type::Bool), Err(Error::Mismatch));
    assert_eq!(
        binary(Op::Add, &dec(0, 0), &small, &small),
        Err(Error::InvalidType)
    );
    // A storage hint is not a different type.
    let low = Type::LowCardinality(Box::new(Type::U8));
    assert_eq!(binary(Op::Add, &low, &u, &u), Ok(Value::U8(2)));
}

/// The exact value of a numeric value: `num * 2^two * 5^five`, or a float
/// special (NaN, or infinity with its sign).
#[derive(Clone, Debug, PartialEq)]
enum Exact {
    Finite {
        num: Big,
        two: i32,
        five: i32,
        neg_zero: bool,
    },
    Nan,
    Inf(bool),
}

/// Fraction bits, exponent bias, and the smallest exponent of a float format.
fn format(ty: &Type) -> (u32, i32) {
    match ty {
        Type::F32 => (23, 127),
        _ => (52, 1023),
    }
}

/// The exact value of float bits, decoded from the IEEE 754 fields.
fn float_exact(bits: u64, ty: &Type) -> Exact {
    let (frac_bits, bias) = format(ty);
    let neg = bits >> (frac_bits + if bias == 127 { 8 } else { 11 }) & 1 == 1;
    let exp = ((bits >> frac_bits) & (2 * bias as u64 + 1)) as i32;
    let frac = bits & ((1 << frac_bits) - 1);
    if exp == 2 * bias + 1 {
        return if frac == 0 {
            Exact::Inf(neg)
        } else {
            Exact::Nan
        };
    }
    let (n, e) = if exp == 0 {
        (frac, 1 - bias - frac_bits as i32)
    } else {
        (frac | 1 << frac_bits, exp - bias - frac_bits as i32)
    };
    let num = Big::from_u128(u128::from(n));
    Exact::Finite {
        num: if neg { num.neg() } else { num },
        two: e,
        five: 0,
        neg_zero: neg && n == 0,
    }
}

fn exact_of(ty: &Type, v: &Value) -> Exact {
    match v {
        Value::F32(x) => float_exact(u64::from(x.to_bits()), ty),
        Value::F64(x) => float_exact(x.to_bits(), ty),
        _ => {
            let s = scale(ty) as i32;
            Exact::Finite {
                num: big(v),
                two: -s,
                five: -s,
                neg_zero: false,
            }
        }
    }
}

/// `x` with no factor 2 (or 5) left in `num` that the exponent can absorb.
fn normal(x: Exact) -> Exact {
    match x {
        Exact::Finite {
            mut num,
            mut two,
            mut five,
            neg_zero,
        } => {
            if num.is_zero() {
                return Exact::Finite {
                    num,
                    two: 0,
                    five: 0,
                    neg_zero,
                };
            }
            for (p, e) in [(2, &mut two), (5, &mut five)] {
                loop {
                    let (q, r) = num.div_rem(&Big::from_u128(p));
                    if !r.is_zero() {
                        break;
                    }
                    (num, *e) = (q, *e + 1);
                }
            }
            Exact::Finite {
                num,
                two,
                five,
                neg_zero,
            }
        }
        other => other,
    }
}

/// What `cast(from, v, to)` must give, as an exact value.
fn expected_cast(from: &Type, v: &Value, to: &Type) -> Result<Exact, Error> {
    let is_float = |t: &Type| matches!(t, Type::F32 | Type::F64);
    let x = exact_of(from, v);
    let (num, two, five, neg_zero) = match x {
        Exact::Nan if is_float(to) => return Ok(Exact::Nan),
        Exact::Inf(s) if is_float(to) => return Ok(Exact::Inf(s)),
        Exact::Nan => return Err(Error::LossOfPrecision),
        Exact::Inf(_) => return Err(Error::Overflow),
        Exact::Finite {
            num,
            two,
            five,
            neg_zero,
        } => (num, two, five, neg_zero),
    };
    if is_float(to) {
        let Exact::Finite { num, two, five, .. } = normal(Exact::Finite {
            num,
            two,
            five,
            neg_zero,
        }) else {
            unreachable!()
        };
        if num.is_zero() {
            return Ok(Exact::Finite {
                num,
                two: 0,
                five: 0,
                neg_zero,
            });
        }
        if five < 0 {
            return Err(Error::LossOfPrecision);
        }
        // An odd significand times a power of two.
        let (mut num, mut two) = (num.mul(&Big::pow(5, five as u32)), two);
        while num.is_even() {
            (num, two) = (num.div_rem(&Big::from_u128(2)).0, two + 1);
        }
        let (frac_bits, bias) = format(to);
        let bits = num.bits() as i32;
        if bits > frac_bits as i32 + 1 || two < 1 - bias - frac_bits as i32 {
            return Err(Error::LossOfPrecision);
        }
        if two + bits - 1 > bias {
            return Err(Error::Overflow);
        }
        return Ok(Exact::Finite {
            num,
            two,
            five: 0,
            neg_zero: false,
        });
    }
    // num * 2^(two + t) * 5^(five + t) must be a whole number in range.
    let t = scale(to) as i32;
    let up = |p: u32, e: i32| Big::pow(p, e.max(0) as u32);
    let n = num.mul(&up(2, two + t)).mul(&up(5, five + t));
    let d = up(2, -(two + t)).mul(&up(5, -(five + t)));
    let (q, r) = n.div_rem(&d);
    if !r.is_zero() {
        return Err(Error::LossOfPrecision);
    }
    let (lo, hi) = range(to);
    if q < lo || q > hi {
        return Err(Error::Overflow);
    }
    let s = -t;
    Ok(Exact::Finite {
        num: q,
        two: s,
        five: s,
        neg_zero: false,
    })
}

fn numeric_types() -> Vec<Type> {
    let mut types = exact_types();
    types.extend([Type::F32, Type::F64]);
    types
}

/// Values of `ty` for the cast table: the boundaries, small values and
/// fractions, float specials, and seeded samples.
fn cast_inputs(ty: &Type, rng: &mut Rng, samples: u64) -> Vec<Value> {
    let mut out = vec![];
    match ty {
        Type::F32 => {
            for x in [
                0.0,
                -0.0,
                1.0,
                -1.0,
                0.5,
                0.1,
                255.0,
                256.0,
                -128.0,
                -129.0,
                1e9,
                16_777_217.0,
                f32::MAX,
                f32::MIN,
                f32::MIN_POSITIVE,
                f32::EPSILON,
                1e-45,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NAN,
            ] {
                out.push(Value::F32(x));
            }
            out.extend((0..samples).map(|_| Value::F32(f32::from_bits(rng.next() as u32))));
        }
        Type::F64 => {
            for x in [
                0.0,
                -0.0,
                1.0,
                -1.0,
                0.5,
                0.1,
                0.25,
                1e-2,
                65535.0,
                65536.0,
                -32768.5,
                1e18,
                9_007_199_254_740_993.0,
                9_007_199_254_740_992.0,
                2f64.powi(64),
                2f64.powi(127),
                -(2f64.powi(127)),
                2f64.powi(128),
                2f64.powi(255),
                2f64.powi(u128::BITS as i32 * 2),
                1e76,
                1e77,
                f64::from(f32::MAX),
                f64::MAX,
                f64::MIN,
                f64::MIN_POSITIVE,
                5e-324,
                1e-20,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NAN,
            ] {
                out.push(Value::F64(x));
            }
            out.extend((0..samples).map(|_| Value::F64(f64::from_bits(rng.next()))));
        }
        _ => {
            let (lo, hi) = range(ty);
            let unit = Big::pow(10, scale(ty));
            for b in [
                lo.clone(),
                hi.clone(),
                lo.add(&one(1)),
                hi.sub(&one(1)),
                Big::zero(),
                one(1),
                one(-1),
                unit.clone(),
                unit.neg(),
                unit.mul(&one(5)).div_rem(&one(10)).0,
                one(255).mul(&unit),
                one(255).add(&one(1)).mul(&unit),
                one(-129).mul(&unit),
                Big::pow(2, 53).add(&one(1)).mul(&unit),
            ] {
                out.extend(value(ty, &b));
            }
            out.extend((0..samples).map(|_| rng.sample(ty)));
        }
    }
    out
}

/// Every cast between every pair of numeric types, compared with the exact
/// value: a cast either keeps the value or returns the typed error.
#[test]
fn cast_table() {
    let mut rng = Rng::new("cast_table");
    let samples = if full() { 20_000 } else { 50 };
    let types = numeric_types();
    let mut pairs = 0;
    for from in &types {
        let inputs = cast_inputs(from, &mut rng, samples);
        for to in &types {
            pairs += 1;
            for v in &inputs {
                let got = cast(from, v, to).map(|w| normal(exact_of(to, &w)));
                let want = expected_cast(from, v, to).map(normal);
                assert_eq!(got, want, "{v:?} as {from:?} to {to:?}");
                if let Ok(Value::F32(x)) = cast(from, v, to)
                    && x.is_nan()
                {
                    assert_eq!(x.to_bits(), CANONICAL_NAN_F32);
                }
                if let Ok(Value::F64(x)) = cast(from, v, to)
                    && x.is_nan()
                {
                    assert_eq!(x.to_bits(), CANONICAL_NAN_F64);
                }
            }
        }
    }
    assert_eq!(pairs, types.len() * types.len());
}

/// A few casts written out, so the table's reference is itself checked.
#[test]
fn casts_by_example() {
    use Error::{LossOfPrecision as Loss, Overflow as Over};
    let d = |p, s| dec(p, s);
    assert_eq!(
        cast(&Type::U16, &Value::U16(255), &Type::U8),
        Ok(Value::U8(255))
    );
    assert_eq!(
        cast(&Type::U16, &Value::U16(u16::from(u8::MAX) + 1), &Type::U8),
        Err(Over)
    );
    assert_eq!(cast(&Type::I8, &Value::I8(-1), &Type::U64), Err(Over));
    assert_eq!(
        cast(&Type::I64, &Value::I64(-5), &d(3, 1)),
        Ok(Value::Decimal32(-50))
    );
    assert_eq!(cast(&Type::I64, &Value::I64(100), &d(3, 1)), Err(Over));
    assert_eq!(cast(&d(9, 2), &Value::Decimal32(150), &Type::I8), Err(Loss));
    assert_eq!(
        cast(&d(9, 2), &Value::Decimal32(-200), &Type::I8),
        Ok(Value::I8(-2))
    );
    assert_eq!(
        cast(&d(9, 2), &Value::Decimal32(150), &d(18, 1)),
        Ok(Value::Decimal64(15))
    );
    assert_eq!(cast(&d(9, 2), &Value::Decimal32(155), &d(18, 1)), Err(Loss));
    assert_eq!(
        cast(&d(9, 2), &Value::Decimal32(50), &Type::F32),
        Ok(Value::F32(0.5))
    );
    assert_eq!(cast(&d(9, 2), &Value::Decimal32(10), &Type::F64), Err(Loss));
    // 0.1 as an f64 has 55 fractional digits.
    assert_eq!(cast(&Type::F64, &Value::F64(0.1), &d(38, 38)), Err(Loss));
    assert!(cast(&Type::F64, &Value::F64(0.1), &d(76, 76)).is_ok());
    assert_eq!(
        cast(&Type::F64, &Value::F64(0.375), &d(5, 3)),
        Ok(Value::Decimal32(375))
    );
    assert_eq!(
        cast(&Type::F64, &Value::F64(-0.0), &Type::U8),
        Ok(Value::U8(0))
    );
    assert_eq!(
        cast(&Type::F64, &Value::F64(f64::NAN), &Type::I32),
        Err(Loss)
    );
    assert_eq!(
        cast(&Type::F64, &Value::F64(f64::INFINITY), &Type::I32),
        Err(Over)
    );
    assert_eq!(cast(&Type::F64, &Value::F64(1e300), &Type::U256), Err(Over));
    assert_eq!(
        cast(&Type::F64, &Value::F64(2f64.powi(200)), &Type::F32),
        Err(Over)
    );
    // Too many significant bits for an f32 wins over the range.
    assert_eq!(cast(&Type::F64, &Value::F64(1e300), &Type::F32), Err(Loss));
    assert_eq!(cast(&Type::F64, &Value::F64(0.1), &Type::F32), Err(Loss));
    assert_eq!(cast(&Type::F64, &Value::F64(5e-324), &Type::F32), Err(Loss));
    assert_eq!(cast(&Type::F64, &Value::F64(1e-45), &Type::F32), Err(Loss));
    let tiny = f32::from_bits(1);
    assert_eq!(
        cast(&Type::F32, &Value::F32(tiny), &Type::F64),
        Ok(Value::F64(f64::from(tiny)))
    );
    assert_eq!(
        cast(&Type::F64, &Value::F64(f64::from(tiny)), &Type::F32),
        Ok(Value::F32(tiny))
    );
    let neg_zero = cast(&Type::F64, &Value::F64(-0.0), &Type::F32);
    assert_eq!(
        neg_zero.map(|v| matches!(v, Value::F32(x) if x.to_bits() == 1 << 31)),
        Ok(true)
    );
    assert_eq!(
        cast(&Type::U64, &Value::U64((1 << 53) + 1), &Type::F64),
        Err(Loss)
    );
    assert_eq!(
        cast(&Type::U64, &Value::U64(1 << 63), &Type::F64),
        Ok(Value::F64(2f64.powi(63)))
    );
    assert_eq!(
        cast(
            &Type::U256,
            &Value::U256(granite_types::U256 {
                hi: 1 << 127,
                lo: 0
            }),
            &Type::F32
        ),
        Err(Over)
    );
}
