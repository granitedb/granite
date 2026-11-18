//! Property tests of the key encoding and the storage representation, from a
//! logged seed. Rerun a failure with `GRANITE_SEED=<seed> cargo test -p granite-types`.

use granite_types::key::{Order, decode_key, encode_key};
use granite_types::storage::{decode_fixed, encode_fixed};
use granite_types::{CANONICAL_NAN_F32, CANONICAL_NAN_F64, I256, TimePrecision, Type, U256, Value};
use std::cmp::Ordering;

const PAIRS: u32 = 1_000_000;

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn new(test: &str) -> Self {
        let seed = std::env::var("GRANITE_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0x5EED);
        println!("{test}: GRANITE_SEED={seed}");
        Rng(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[self.below(from.len() as u64) as usize]
    }

    /// Small, near-maximum or uniform, so that ties and neighbours are common.
    /// Truncated to a narrower type, near-maximum becomes near -1.
    fn u128(&mut self) -> u128 {
        let full = (u128::from(self.next()) << 64) | u128::from(self.next());
        match self.below(4) {
            0 => full % 4,
            1 => u128::MAX - full % 4,
            _ => full,
        }
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len)
            .map(|_| {
                let any = self.next() as u8;
                self.pick(&[0, 1, b'a', 0x7F, 0x80, 0xFE, 0xFF, any])
            })
            .collect()
    }
}

/// Every type allowed in a key.
fn key_types() -> Vec<Type> {
    let dec = |precision, scale| Type::Decimal { precision, scale };
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
        dec(9, 2),
        dec(18, 4),
        dec(38, 10),
        dec(76, 20),
        Type::Bool,
        Type::Date,
        Type::Timestamp(TimePrecision::Nanos),
        Type::Duration(TimePrecision::Millis),
        Type::Uuid,
        Type::Id128,
        Type::String { max: 16 },
        Type::Bytes { max: 16 },
        Type::FixedBytes(3),
        Type::Enum8 { max: 200 },
        Type::Enum16 { max: 60_000 },
        Type::LowCardinality(Box::new(Type::String { max: 16 })),
    ]
}

/// Minimum, maximum and zero (or the nearest the type has) of every type.
fn edges(ty: &Type) -> Vec<Value> {
    use Value as V;
    let i256 = |hi, lo| I256 { hi, lo };
    match ty {
        Type::U8 => vec![V::U8(0), V::U8(u8::MAX)],
        Type::U16 => vec![V::U16(0), V::U16(u16::MAX)],
        Type::U32 => vec![V::U32(0), V::U32(u32::MAX)],
        Type::U64 => vec![V::U64(0), V::U64(u64::MAX)],
        Type::U128 => vec![V::U128(0), V::U128(u128::MAX)],
        Type::U256 => vec![V::U256(U256::MIN), V::U256(U256::MAX)],
        Type::I8 => vec![V::I8(i8::MIN), V::I8(0), V::I8(i8::MAX)],
        Type::I16 => vec![V::I16(i16::MIN), V::I16(0), V::I16(i16::MAX)],
        Type::I32 => vec![V::I32(i32::MIN), V::I32(0), V::I32(i32::MAX)],
        Type::I64 => vec![V::I64(i64::MIN), V::I64(0), V::I64(i64::MAX)],
        Type::I128 => vec![V::I128(i128::MIN), V::I128(0), V::I128(i128::MAX)],
        Type::I256 => vec![V::I256(I256::MIN), V::I256(i256(0, 0)), V::I256(I256::MAX)],
        Type::Decimal { precision, .. } => match granite_types::decimal_width(*precision) {
            4 => vec![
                V::Decimal32(i32::MIN),
                V::Decimal32(0),
                V::Decimal32(i32::MAX),
            ],
            8 => vec![
                V::Decimal64(i64::MIN),
                V::Decimal64(0),
                V::Decimal64(i64::MAX),
            ],
            16 => vec![
                V::Decimal128(i128::MIN),
                V::Decimal128(0),
                V::Decimal128(i128::MAX),
            ],
            _ => vec![
                V::Decimal256(I256::MIN),
                V::Decimal256(i256(0, 0)),
                V::Decimal256(I256::MAX),
            ],
        },
        Type::F32 => vec![V::F32(f32::MIN), V::F32(0.0), V::F32(f32::MAX)],
        Type::F64 => vec![V::F64(f64::MIN), V::F64(0.0), V::F64(f64::MAX)],
        Type::Bool => vec![V::Bool(false), V::Bool(true)],
        Type::Date => vec![V::Date(i32::MIN), V::Date(0), V::Date(i32::MAX)],
        Type::Timestamp(_) => vec![
            V::Timestamp(i64::MIN),
            V::Timestamp(0),
            V::Timestamp(i64::MAX),
        ],
        Type::Duration(_) => vec![V::Duration(i64::MIN), V::Duration(0), V::Duration(i64::MAX)],
        Type::Uuid => vec![V::Uuid([0; 16]), V::Uuid([u8::MAX; 16])],
        Type::Id128 => vec![V::Id128(0), V::Id128(u128::MAX)],
        Type::String { .. } => vec![V::String(String::new()), V::String("\u{10FFFF}".repeat(4))],
        Type::Bytes { .. } => vec![
            V::Bytes(vec![]),
            V::Bytes(vec![0]),
            V::Bytes(vec![u8::MAX; 16]),
        ],
        Type::FixedBytes(n) => vec![
            V::FixedBytes(vec![0; *n as usize]),
            V::FixedBytes(vec![u8::MAX; *n as usize]),
        ],
        Type::Enum8 { max } => vec![V::Enum8(0), V::Enum8(*max)],
        Type::Enum16 { max } => vec![V::Enum16(0), V::Enum16(*max)],
        Type::Nullable(inner) => [vec![V::Null], edges(inner)].concat(),
        Type::LowCardinality(inner) => edges(inner),
    }
}

fn random(ty: &Type, rng: &mut Rng) -> Value {
    use Value as V;
    let i256 = |rng: &mut Rng| I256 {
        hi: rng.u128() as i128,
        lo: rng.u128(),
    };
    match ty {
        Type::U8 => V::U8(rng.u128() as u8),
        Type::U16 => V::U16(rng.u128() as u16),
        Type::U32 => V::U32(rng.u128() as u32),
        Type::U64 => V::U64(rng.u128() as u64),
        Type::U128 => V::U128(rng.u128()),
        Type::U256 => V::U256(U256 {
            hi: rng.u128(),
            lo: rng.u128(),
        }),
        Type::I8 => V::I8(rng.u128() as i8),
        Type::I16 => V::I16(rng.u128() as i16),
        Type::I32 => V::I32(rng.u128() as i32),
        Type::I64 => V::I64(rng.u128() as i64),
        Type::I128 => V::I128(rng.u128() as i128),
        Type::I256 => V::I256(i256(rng)),
        Type::Decimal { precision, .. } => match granite_types::decimal_width(*precision) {
            4 => V::Decimal32(rng.u128() as i32),
            8 => V::Decimal64(rng.u128() as i64),
            16 => V::Decimal128(rng.u128() as i128),
            _ => V::Decimal256(i256(rng)),
        },
        Type::F32 => V::F32(f32::from_bits(rng.next() as u32)),
        Type::F64 => V::F64(f64::from_bits(rng.next())),
        Type::Bool => V::Bool(rng.next() & 1 == 1),
        Type::Date => V::Date(rng.u128() as i32),
        Type::Timestamp(_) => V::Timestamp(rng.u128() as i64),
        Type::Duration(_) => V::Duration(rng.u128() as i64),
        Type::Uuid => V::Uuid(rng.bytes(16).try_into().unwrap()),
        Type::Id128 => V::Id128(rng.u128()),
        Type::String { .. } => {
            let chars = [
                '\0',
                '\u{1}',
                'a',
                'b',
                '\u{7F}',
                'é',
                '\u{FFFF}',
                '\u{10FFFF}',
            ];
            V::String((0..rng.below(5)).map(|_| rng.pick(&chars)).collect())
        }
        Type::Bytes { max } => {
            let len = rng.below(u64::from(*max).min(6) + 1) as usize;
            V::Bytes(rng.bytes(len))
        }
        Type::FixedBytes(n) => V::FixedBytes(rng.bytes(*n as usize)),
        Type::Enum8 { max } => V::Enum8(rng.below(u64::from(*max) + 1) as u8),
        Type::Enum16 { max } => V::Enum16(rng.below(u64::from(*max) + 1) as u16),
        Type::Nullable(inner) => match rng.below(4) {
            0 => V::Null,
            _ => random(inner, rng),
        },
        Type::LowCardinality(inner) => random(inner, rng),
    }
}

/// The logical order, independent of the encoding.
fn logical_cmp(a: &Value, b: &Value) -> Ordering {
    macro_rules! same {
        ($($v:ident),*) => {
            match (a, b) {
                $((Value::$v(x), Value::$v(y)) => x.cmp(y),)*
                _ => panic!("not comparable: {a:?} {b:?}"),
            }
        };
    }
    same!(
        U8, U16, U32, U64, U128, U256, I8, I16, I32, I64, I128, I256, Decimal32, Decimal64,
        Decimal128, Decimal256, Bool, Date, Timestamp, Duration, Uuid, Id128, String, Bytes,
        FixedBytes, Enum8, Enum16
    )
}

fn tuple_cmp(columns: &[(Type, Order)], a: &[Value], b: &[Value]) -> Ordering {
    for ((_, order), (x, y)) in columns.iter().zip(a.iter().zip(b)) {
        let o = logical_cmp(x, y);
        if o != Ordering::Equal {
            return if *order == Order::Desc {
                o.reverse()
            } else {
                o
            };
        }
    }
    Ordering::Equal
}

/// Checks order, round trip and prefix freedom for one pair of keys.
fn check_pair(columns: &[(Type, Order)], a: &[Value], b: &[Value]) {
    let ka = encode_key(columns, a).unwrap();
    let kb = encode_key(columns, b).unwrap();
    let ctx = || format!("{columns:?}\n a={a:?}\n b={b:?}\n ka={ka:02x?}\n kb={kb:02x?}");
    assert_eq!(ka.cmp(&kb), tuple_cmp(columns, a, b), "order: {}", ctx());
    assert_eq!(
        decode_key(columns, &ka).unwrap(),
        a,
        "round trip: {}",
        ctx()
    );
    if ka != kb {
        assert!(
            !kb.starts_with(&ka) && !ka.starts_with(&kb),
            "prefix: {}",
            ctx()
        );
    }
}

fn storage_round_trip(ty: &Type, v: &Value) {
    if ty.fixed_width().is_none() {
        return;
    }
    let mut out = Vec::new();
    encode_fixed(ty, v, &mut out).unwrap();
    assert_eq!(out.len() as u32, ty.fixed_width().unwrap());
    let back = decode_fixed(ty, &out).unwrap();
    // NaN != NaN: compare floats by bits; random floats are canonicalised.
    match (&back, v) {
        (Value::F32(x), Value::F32(y)) if y.is_nan() => assert_eq!(x.to_bits(), CANONICAL_NAN_F32),
        (Value::F64(x), Value::F64(y)) if y.is_nan() => assert_eq!(x.to_bits(), CANONICAL_NAN_F64),
        (Value::F32(x), Value::F32(y)) => assert_eq!(x.to_bits(), y.to_bits()),
        (Value::F64(x), Value::F64(y)) => assert_eq!(x.to_bits(), y.to_bits()),
        _ => assert_eq!(&back, v, "{ty:?}"),
    }
}

/// One key type: edge values against each other, then random pairs, in both
/// orders, plus the storage round trip.
fn key_type(ty: Type) {
    let mut rng = Rng::new(&format!("{ty:?}"));
    let edges = edges(&ty);
    for order in [Order::Asc, Order::Desc] {
        let columns = [(ty.clone(), order)];
        for a in &edges {
            storage_round_trip(&ty, a);
            for b in &edges {
                check_pair(&columns, std::slice::from_ref(a), std::slice::from_ref(b));
            }
        }
    }
    for _ in 0..PAIRS {
        let a = random(&ty, &mut rng);
        let b = match rng.below(8) {
            0 => a.clone(),
            1 => edges[rng.pick(&[0, edges.len() - 1])].clone(),
            _ => random(&ty, &mut rng),
        };
        let order = if rng.next() & 1 == 0 {
            Order::Asc
        } else {
            Order::Desc
        };
        let columns = [(ty.clone(), order)];
        check_pair(&columns, std::slice::from_ref(&a), std::slice::from_ref(&b));
        storage_round_trip(&ty, &a);
    }
}

macro_rules! key_type_tests {
    ($($name:ident: $index:expr,)*) => {
        $(#[test] fn $name() { key_type(key_types().swap_remove($index)); })*
        #[test]
        fn every_key_type_has_a_test() {
            assert_eq!(key_types().len(), [$($index),*].len());
        }
    };
}

key_type_tests! {
    key_u8: 0, key_u16: 1, key_u32: 2, key_u64: 3, key_u128: 4, key_u256: 5,
    key_i8: 6, key_i16: 7, key_i32: 8, key_i64: 9, key_i128: 10, key_i256: 11,
    key_decimal32: 12, key_decimal64: 13, key_decimal128: 14, key_decimal256: 15,
    key_bool: 16, key_date: 17, key_timestamp: 18, key_duration: 19, key_uuid: 20,
    key_id128: 21, key_string: 22, key_bytes: 23, key_fixed_bytes: 24, key_enum8: 25,
    key_enum16: 26, key_low_cardinality: 27,
}

#[test]
fn composite_keys() {
    let mut rng = Rng::new("composite_keys");
    let types = key_types();
    for _ in 0..PAIRS {
        let n = 1 + rng.below(8) as usize;
        let columns: Vec<(Type, Order)> = (0..n)
            .map(|_| {
                let order = if rng.next() & 1 == 0 {
                    Order::Asc
                } else {
                    Order::Desc
                };
                (types[rng.below(types.len() as u64) as usize].clone(), order)
            })
            .collect();
        let a: Vec<Value> = columns.iter().map(|(t, _)| random(t, &mut rng)).collect();
        // Copy most columns so that long common prefixes are frequent.
        let b: Vec<Value> = columns
            .iter()
            .zip(&a)
            .map(|((t, _), v)| {
                if rng.below(4) == 0 {
                    random(t, &mut rng)
                } else {
                    v.clone()
                }
            })
            .collect();
        check_pair(&columns, &a, &b);
    }
}

#[test]
fn non_key_types_round_trip_in_storage() {
    let mut rng = Rng::new("non_key_types_round_trip_in_storage");
    let types = [
        Type::F32,
        Type::F64,
        Type::Nullable(Box::new(Type::I64)),
        Type::Nullable(Box::new(Type::Decimal {
            precision: 76,
            scale: 0,
        })),
        Type::Nullable(Box::new(Type::F64)),
        Type::LowCardinality(Box::new(Type::Nullable(Box::new(Type::FixedBytes(3))))),
    ];
    for ty in &types {
        for v in edges(ty) {
            storage_round_trip(ty, &v);
        }
        for _ in 0..PAIRS {
            storage_round_trip(ty, &random(ty, &mut rng));
        }
    }
}

#[test]
fn every_nan_is_canonical() {
    let mut rng = Rng::new("every_nan_is_canonical");
    let f64_bits = |x: f64| {
        let mut out = Vec::new();
        encode_fixed(&Type::F64, &Value::F64(x), &mut out).unwrap();
        u64::from_le_bytes(out.try_into().unwrap())
    };
    let f32_bits = |x: f32| {
        let mut out = Vec::new();
        encode_fixed(&Type::F32, &Value::F32(x), &mut out).unwrap();
        u32::from_le_bytes(out.try_into().unwrap())
    };
    // Special NaNs: quiet, signalling, negative, all payload bits set.
    for bits in [
        0x7FF8_0000_0000_0000,
        0x7FF0_0000_0000_0001,
        0xFFF8_0000_0000_0000,
        u64::MAX,
    ] {
        assert_eq!(f64_bits(f64::from_bits(bits)), CANONICAL_NAN_F64);
    }
    for bits in [0x7FC0_0000, 0x7F80_0001, 0xFFC0_0000, u32::MAX] {
        assert_eq!(f32_bits(f32::from_bits(bits)), CANONICAL_NAN_F32);
    }
    assert_eq!(f64_bits(f64::NAN), CANONICAL_NAN_F64);
    assert_eq!(f32_bits(f32::NAN), CANONICAL_NAN_F32);
    // Other special values keep their bits.
    for x in [
        0.0,
        -0.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MIN_POSITIVE,
        5e-324,
        f64::MAX,
    ] {
        assert_eq!(f64_bits(x), x.to_bits());
    }
    for x in [
        0.0,
        -0.0,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MIN_POSITIVE,
        1e-45,
        f32::MAX,
    ] {
        assert_eq!(f32_bits(x), x.to_bits());
    }
    let (exp64, exp32) = (f64::INFINITY.to_bits(), f32::INFINITY.to_bits());
    for _ in 0..PAIRS {
        let r = rng.next();
        // Any sign, all exponent bits set, a non-zero mantissa: a NaN.
        let nan64 = r | exp64 | 1 << (r % 52);
        assert!(f64::from_bits(nan64).is_nan());
        assert_eq!(f64_bits(f64::from_bits(nan64)), CANONICAL_NAN_F64);
        let nan32 = (r as u32 | exp32) | 1 << (r % 23);
        assert!(f32::from_bits(nan32).is_nan());
        assert_eq!(f32_bits(f32::from_bits(nan32)), CANONICAL_NAN_F32);
    }
}
