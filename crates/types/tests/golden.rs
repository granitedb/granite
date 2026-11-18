//! Golden vectors: the key encoding (ascending and descending) and the
//! storage representation of fixed values of every type, compared with the
//! committed `golden_vectors.txt`. CI runs this on Linux x86-64 and macOS
//! arm64, so both produce the same bytes. A change to that file is a format
//! change (document 03 section 2).

use granite_types::key::{Order, encode_key};
use granite_types::storage::encode_fixed;
use granite_types::{I256, TimePrecision, Type, U256, Value};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn cases() -> Vec<(Type, Vec<Value>)> {
    use Value as V;
    let dec = |precision, scale| Type::Decimal { precision, scale };
    let i256 = |hi, lo| I256 { hi, lo };
    let boxed = Box::new;
    vec![
        (Type::U8, vec![V::U8(0), V::U8(1), V::U8(u8::MAX)]),
        (Type::U16, vec![V::U16(0), V::U16(0x1234), V::U16(u16::MAX)]),
        (
            Type::U32,
            vec![V::U32(0), V::U32(0x1234_5678), V::U32(u32::MAX)],
        ),
        (
            Type::U64,
            vec![V::U64(0), V::U64(0x0102_0304_0506_0708), V::U64(u64::MAX)],
        ),
        (Type::U128, vec![V::U128(0), V::U128(1), V::U128(u128::MAX)]),
        (
            Type::U256,
            vec![
                V::U256(U256::MIN),
                V::U256(U256 { hi: 1, lo: 2 }),
                V::U256(U256::MAX),
            ],
        ),
        (
            Type::I8,
            vec![
                V::I8(i8::MIN),
                V::I8(-1),
                V::I8(0),
                V::I8(1),
                V::I8(i8::MAX),
            ],
        ),
        (
            Type::I16,
            vec![V::I16(i16::MIN), V::I16(-2), V::I16(0), V::I16(i16::MAX)],
        ),
        (
            Type::I32,
            vec![V::I32(i32::MIN), V::I32(-3), V::I32(0), V::I32(i32::MAX)],
        ),
        (
            Type::I64,
            vec![V::I64(i64::MIN), V::I64(-4), V::I64(0), V::I64(i64::MAX)],
        ),
        (
            Type::I128,
            vec![
                V::I128(i128::MIN),
                V::I128(-5),
                V::I128(0),
                V::I128(i128::MAX),
            ],
        ),
        (
            Type::I256,
            vec![
                V::I256(I256::MIN),
                V::I256(i256(-1, u128::MAX)),
                V::I256(i256(0, 0)),
                V::I256(I256::MAX),
            ],
        ),
        (
            dec(9, 2),
            vec![
                V::Decimal32(-12_345),
                V::Decimal32(0),
                V::Decimal32(999_999_999),
            ],
        ),
        (
            dec(18, 4),
            vec![V::Decimal64(-1), V::Decimal64(123_456_789_012)],
        ),
        (
            dec(38, 18),
            vec![V::Decimal128(-1), V::Decimal128(10i128.pow(37))],
        ),
        (
            dec(76, 20),
            vec![V::Decimal256(i256(-2, 7)), V::Decimal256(i256(3, 0))],
        ),
        (
            Type::F32,
            vec![
                V::F32(-0.0),
                V::F32(1.5),
                V::F32(f32::NEG_INFINITY),
                V::F32(-f32::NAN),
            ],
        ),
        (
            Type::F64,
            vec![
                V::F64(-0.0),
                V::F64(1.5),
                V::F64(f64::INFINITY),
                V::F64(-f64::NAN),
            ],
        ),
        (Type::Bool, vec![V::Bool(false), V::Bool(true)]),
        (Type::Date, vec![V::Date(-1), V::Date(0), V::Date(20_000)]),
        (
            Type::Timestamp(TimePrecision::Nanos),
            vec![V::Timestamp(-1), V::Timestamp(1_763_424_000_000_000_000)],
        ),
        (
            Type::Timestamp(TimePrecision::Seconds),
            vec![V::Timestamp(1_763_424_000)],
        ),
        (
            Type::Duration(TimePrecision::Millis),
            vec![V::Duration(-1_500), V::Duration(0)],
        ),
        (Type::Duration(TimePrecision::Micros), vec![V::Duration(1)]),
        (
            Type::Uuid,
            vec![V::Uuid([
                0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44,
                0x00, 0x00,
            ])],
        ),
        (
            Type::Id128,
            vec![
                V::Id128(0),
                V::Id128(0x0193_4A5B_6C7D_7E8F_9AAB_BCCD_DEEF_F001),
            ],
        ),
        (
            Type::String { max: 16 },
            vec![
                V::String(String::new()),
                V::String("a".into()),
                V::String("a\0b".into()),
                V::String("héllo".into()),
            ],
        ),
        (
            Type::Bytes { max: 16 },
            vec![
                V::Bytes(vec![]),
                V::Bytes(vec![0]),
                V::Bytes(vec![0, 0xFF, 1]),
            ],
        ),
        (Type::FixedBytes(3), vec![V::FixedBytes(vec![0, 0xFF, 7])]),
        (Type::Enum8 { max: 3 }, vec![V::Enum8(0), V::Enum8(3)]),
        (
            Type::Enum16 { max: 999 },
            vec![V::Enum16(0), V::Enum16(999)],
        ),
        (Type::Nullable(boxed(Type::I32)), vec![V::Null, V::I32(-3)]),
        (
            Type::Nullable(boxed(Type::String { max: 4 })),
            vec![V::Null],
        ),
        (
            Type::LowCardinality(boxed(Type::String { max: 16 })),
            vec![V::String("eur".into())],
        ),
        (Type::LowCardinality(boxed(Type::U16)), vec![V::U16(7)]),
    ]
}

/// One line per value: type, value, then the encodings it has; a type not
/// allowed in keys or without a fixed width shows `-`.
fn render() -> String {
    let mut text = String::new();
    for (ty, values) in cases() {
        for v in values {
            let key = |order| {
                encode_key(&[(ty.clone(), order)], std::slice::from_ref(&v))
                    .map_or("-".into(), |k| hex(&k))
            };
            let mut fixed = Vec::new();
            let fixed = match encode_fixed(&ty, &v, &mut fixed) {
                Ok(()) => hex(&fixed),
                Err(_) => "-".into(),
            };
            text += &format!(
                "{ty:?} {v:?}\n  asc {}\n  desc {}\n  storage {fixed}\n",
                key(Order::Asc),
                key(Order::Desc)
            );
        }
    }
    text
}

#[test]
fn golden_vectors() {
    let actual = render();
    if actual != include_str!("golden_vectors.txt") {
        print!("{actual}");
        panic!("encodings differ from tests/golden_vectors.txt; the actual text is printed above");
    }
}

#[test]
fn composite_golden_vector() {
    let columns = [
        (Type::U64, Order::Asc),
        (Type::Id128, Order::Desc),
        (Type::String { max: 8 }, Order::Asc),
    ];
    let key = encode_key(
        &columns,
        &[Value::U64(7), Value::Id128(1), Value::String("x".into())],
    )
    .unwrap();
    assert_eq!(
        hex(&key),
        concat!(
            "0000000000000007",
            "fffffffffffffffffffffffffffffffe",
            "780000"
        )
    );
}
