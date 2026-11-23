//! Row batches and result lists (T1.4.4): the layout, every rejection code,
//! the golden batch of the exchange schema's `entry` table, and reading
//! whatever the validator accepts.

use granite_base::constants::FORMAT;
use granite_protocol::Error;
use granite_protocol::batch::{self, Batch, Field, HEADER_SIZE, ROWS_MAX, RowLayout};
use granite_protocol::results::{self, OK, Results};
use granite_types::storage::decode_fixed;
use granite_types::{I256, TimePrecision, Type, U256, Value};

/// The longest message body.
const BODY: usize = (FORMAT.message_size_max - FORMAT.header_size) as usize;

fn boxed(ty: Type) -> Box<Type> {
    Box::new(ty)
}

/// A table with a column of every type, and both wrappers on each kind of column.
fn every_type() -> Vec<Type> {
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
        dec(18, 2),
        dec(38, 18),
        dec(76, 10),
        Type::F32,
        Type::F64,
        Type::Bool,
        Type::Date,
        Type::Timestamp(TimePrecision::Millis),
        Type::Duration(TimePrecision::Nanos),
        Type::Uuid,
        Type::Id128,
        Type::String { max: 16 },
        Type::Bytes { max: 16 },
        Type::FixedBytes(3),
        Type::Enum8 { max: 3 },
        Type::Enum16 { max: 300 },
        Type::Nullable(boxed(Type::U32)),
        Type::Nullable(boxed(Type::String { max: 8 })),
        Type::LowCardinality(boxed(Type::String { max: 8 })),
        Type::LowCardinality(boxed(Type::Nullable(boxed(Type::Bytes { max: 4 })))),
        Type::Nullable(boxed(Type::LowCardinality(boxed(Type::FixedBytes(2))))),
    ]
}

fn every_type_rows() -> Vec<Vec<Value>> {
    let big = I256 {
        hi: -5,
        lo: u128::MAX,
    };
    let full = vec![
        Value::U8(u8::MAX),
        Value::U16(0x1234),
        Value::U32(7),
        Value::U64(u64::MAX),
        Value::U128(1 << 100),
        Value::U256(U256 { hi: 3, lo: 9 }),
        Value::I8(-1),
        Value::I16(i16::MIN),
        Value::I32(-7),
        Value::I64(i64::MAX),
        Value::I128(-(1 << 100)),
        Value::I256(big),
        Value::Decimal32(-999_999_999),
        Value::Decimal64(12),
        Value::Decimal128(100_500_000_000_000_000_000),
        Value::Decimal256(I256 { hi: 0, lo: 1 }),
        Value::F32(f32::NAN),
        Value::F64(-0.0),
        Value::Bool(true),
        Value::Date(-1),
        Value::Timestamp(1_700_000_000_000),
        Value::Duration(-5),
        Value::Uuid([0xab; 16]),
        Value::Id128(u128::MAX),
        Value::String("héllo".into()),
        Value::Bytes(vec![0, 1, 2]),
        Value::FixedBytes(vec![9, 8, 7]),
        Value::Enum8(3),
        Value::Enum16(300),
        Value::U32(5),
        Value::String("12345678".into()),
        Value::String("lc".into()),
        Value::Bytes(vec![0xff]),
        Value::FixedBytes(vec![1, 2]),
    ];
    let mut sparse = full.clone();
    for i in [24, 25] {
        sparse[i] = match &sparse[i] {
            Value::String(_) => Value::String(String::new()),
            _ => Value::Bytes(Vec::new()),
        };
    }
    for i in [29, 30, 32, 33] {
        sparse[i] = Value::Null;
    }
    vec![full, sparse.clone(), full_again(&sparse)]
}

/// A third row, so that content follows empty and null slots.
fn full_again(sparse: &[Value]) -> Vec<Value> {
    let mut row = sparse.to_vec();
    row[24] = Value::String("x".into());
    row[30] = Value::String("y".into());
    row
}

/// The type without its wrappers: what a [`Field::Fixed`] holds.
fn bare(ty: &Type) -> &Type {
    match ty {
        Type::Nullable(inner) | Type::LowCardinality(inner) => bare(inner),
        ty => ty,
    }
}

/// A field as a value of its column type.
fn value(ty: &Type, field: Field) -> Value {
    match field {
        Field::Null => Value::Null,
        Field::String(s) => Value::String(s.into()),
        Field::Bytes(b) => Value::Bytes(b.into()),
        Field::Fixed(b) => decode_fixed(bare(ty), b).expect("a validated value decodes"),
    }
}

/// Every row of a batch, every column read as a value.
fn read_all(types: &[Type], batch: &Batch) -> Vec<Vec<Value>> {
    let rows: Vec<Vec<Value>> = batch
        .rows()
        .map(|row| {
            assert!(row.get(types.len()).is_none());
            (types.iter().enumerate())
                .map(|(c, ty)| value(ty, row.get(c).expect("a column")))
                .collect()
        })
        .collect();
    assert!(batch.row(rows.len()).is_none());
    rows
}

/// By debug form, which tells NaN and -0.0 apart where `==` does not.
fn same(a: &[Vec<Value>], b: &[Vec<Value>]) {
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
}

#[test]
fn rows_of_every_type_read_back() {
    let types = every_type();
    let layout = RowLayout::new(&types);
    let rows = every_type_rows();
    let bytes = batch::encode(&layout, &rows).unwrap();
    let batch = Batch::validate(&layout, &bytes).unwrap();
    assert_eq!(batch.len(), 3);
    same(&read_all(&types, &batch), &rows);
    // Each row struct is the same size; the heap holds the contents back to back.
    let heap = ("héllo".len() + 3 + 8 + 2 + 1) + 2 + (1 + 1 + 2);
    assert_eq!(bytes.len(), HEADER_SIZE + 3 * layout.width() + heap);
}

#[test]
fn an_empty_batch_is_valid() {
    let layout = RowLayout::new(&[Type::U64]);
    let bytes = batch::encode(&layout, &[]).unwrap();
    assert_eq!(bytes, [0; HEADER_SIZE]);
    let batch = Batch::validate(&layout, &bytes).unwrap();
    assert!(batch.is_empty() && batch.row(0).is_none());
}

#[test]
fn encode_checks_values() {
    let layout = RowLayout::new(&[Type::U64, Type::String { max: 2 }]);
    let row = |s: &str| vec![Value::U64(1), Value::String(s.into())];
    assert_eq!(
        batch::encode(&layout, &[row("abc")]),
        Err(granite_types::Error::Length { len: 3, max: 2 })
    );
    assert_eq!(
        batch::encode(&layout, &[vec![Value::U64(1)]]),
        Err(granite_types::Error::ColumnCount)
    );
    assert!(matches!(
        batch::encode(&layout, &vec![row("ab"); ROWS_MAX + 1]),
        Err(granite_types::Error::Length { .. })
    ));
}

// The rejections. The table is (u64, String(4), Enum8 with values 0 and 1,
// Nullable(bool)), and `valid()` is two rows whose contents "ab" and "c" fill
// the heap "abc".

fn table() -> RowLayout {
    RowLayout::new(&[
        Type::U64,
        Type::String { max: 4 },
        Type::Enum8 { max: 1 },
        Type::Nullable(boxed(Type::Bool)),
    ])
}

/// Byte offsets in `valid()`.
const ROW: usize = 19;
const SLOT_1: usize = HEADER_SIZE + ROW + 8;
const ENUM_1: usize = HEADER_SIZE + ROW + 16;
const FLAG_1: usize = HEADER_SIZE + ROW + 17;
const HEAP: usize = HEADER_SIZE + 2 * ROW;

fn valid() -> Vec<u8> {
    let rows = [
        vec![
            Value::U64(1),
            Value::String("ab".into()),
            Value::Enum8(0),
            Value::Null,
        ],
        vec![
            Value::U64(2),
            Value::String("c".into()),
            Value::Enum8(1),
            Value::Bool(true),
        ],
    ];
    let bytes = batch::encode(&table(), &rows).unwrap();
    assert_eq!(table().width(), ROW);
    assert_eq!(&bytes[SLOT_1..SLOT_1 + 8], [2, 0, 0, 0, 1, 0, 0, 0]);
    assert_eq!(&bytes[HEAP..], b"abc");
    bytes
}

fn reject(bytes: &[u8]) -> Error {
    Batch::validate(&table(), bytes).expect_err("rejected")
}

fn with(at: usize, patch: &[u8]) -> Vec<u8> {
    let mut bytes = valid();
    bytes[at..at + patch.len()].copy_from_slice(patch);
    bytes
}

#[test]
fn the_valid_batch_is_accepted() {
    assert_eq!(Batch::validate(&table(), &valid()).unwrap().len(), 2);
}

#[test]
fn overlapping_slots_are_rejected() {
    // The second content starts inside the first: offset 1, length 2.
    assert_eq!(
        reject(&with(SLOT_1, &[1, 0, 0, 0, 2, 0, 0, 0])),
        Error::OverlappingSlots
    );
}

#[test]
fn an_offset_beyond_the_heap_is_rejected() {
    assert_eq!(
        reject(&with(SLOT_1, &[3, 0, 0, 0, 1, 0, 0, 0])),
        Error::OffsetBeyondHeap
    );
    assert_eq!(
        reject(&with(SLOT_1, &[2, 0, 0, 0, 2, 0, 0, 0])),
        Error::OffsetBeyondHeap
    );
    assert_eq!(reject(&with(SLOT_1, &[0xff; 4])), Error::OffsetBeyondHeap);
}

#[test]
fn invalid_utf8_is_rejected() {
    assert_eq!(reject(&with(HEAP + 1, &[0xff])), Error::InvalidUtf8);
}

#[test]
fn an_unknown_enum_value_is_rejected() {
    assert_eq!(reject(&with(ENUM_1, &[2])), Error::UnknownEnumValue);
}

#[test]
fn a_non_zero_reserved_bit_is_rejected() {
    for bit in 0..32 {
        let field = (1u32 << bit).to_le_bytes();
        assert_eq!(reject(&with(4, &field)), Error::ReservedBits);
    }
}

#[test]
fn heap_bytes_no_slot_covers_are_rejected() {
    let mut bytes = valid();
    bytes.push(b'd');
    assert_eq!(reject(&bytes), Error::HeapGap);
    // The second content starts one byte after the first ends.
    let mut bytes = with(SLOT_1, &[3, 0, 0, 0, 1, 0, 0, 0]);
    bytes.push(b'd');
    assert_eq!(reject(&bytes), Error::HeapGap);
}

#[test]
fn content_longer_than_its_max_is_rejected() {
    let mut bytes = with(SLOT_1, &[2, 0, 0, 0, 5, 0, 0, 0]);
    bytes.extend(b"defg");
    assert_eq!(reject(&bytes), Error::TooLong);
}

#[test]
fn fixed_bytes_no_value_encodes_to_are_rejected() {
    assert_eq!(reject(&with(FLAG_1, &[2, 1])), Error::InvalidValue);
    assert_eq!(reject(&with(FLAG_1, &[1, 2])), Error::InvalidValue);
    assert_eq!(reject(&with(FLAG_1, &[0, 1])), Error::InvalidValue);
    // A null String has its slot zero.
    let layout = RowLayout::new(&[Type::Nullable(boxed(Type::String { max: 4 }))]);
    let bytes = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    assert_eq!(
        Batch::validate(&layout, &bytes).unwrap_err(),
        Error::InvalidValue
    );
}

#[test]
fn sizes_are_checked() {
    let bytes = valid();
    assert_eq!(reject(&bytes[..HEADER_SIZE - 1]), Error::Truncated);
    assert_eq!(reject(&bytes[..HEAP - 1]), Error::CountTooLarge);
    assert_eq!(reject(&with(0, &[3])), Error::CountTooLarge);
    assert_eq!(reject(&with(0, &[0xff; 4])), Error::CountTooLarge);
    let body = vec![0; BODY];
    let layout = RowLayout::new(&[Type::U8]);
    // No rows, so every byte after the header is heap that no slot covers.
    assert_eq!(Batch::validate(&layout, &body).unwrap_err(), Error::HeapGap);
    let mut over = body.clone();
    over.push(0);
    assert_eq!(
        Batch::validate(&layout, &over).unwrap_err(),
        Error::TooLarge
    );
}

#[test]
fn the_row_count_is_bounded_by_the_result_list() {
    let layout = RowLayout::new(&[Type::U8]);
    let rows = |n: usize| {
        let mut bytes = u32::try_from(n).unwrap().to_le_bytes().to_vec();
        bytes.resize(HEADER_SIZE + n, 0);
        bytes
    };
    assert_eq!(
        Batch::validate(&layout, &rows(ROWS_MAX)).unwrap().len(),
        ROWS_MAX
    );
    assert_eq!(
        Batch::validate(&layout, &rows(ROWS_MAX + 1)).unwrap_err(),
        Error::CountTooLarge
    );
    // A result for every event of the largest batch fits in one reply.
    let all: Vec<(u32, u32)> = (0..u32::try_from(ROWS_MAX).unwrap())
        .map(|i| (i, 1))
        .collect();
    assert_eq!(results::encode(&all).len(), BODY);
}

#[test]
fn a_malformed_batch_is_rejected_whole() {
    // Only a validated batch has rows to apply, so a bad last row stops the first too.
    let layout = table();
    let apply = |bytes: &[u8]| {
        let mut applied = Vec::new();
        if let Ok(batch) = Batch::validate(&layout, bytes) {
            applied.extend(batch.rows().map(|r| r.get(0)));
        }
        applied.len()
    };
    assert_eq!(apply(&valid()), 2);
    assert_eq!(apply(&with(ENUM_1, &[2])), 0);
    assert_eq!(apply(&with(HEAP + 2, &[0xff])), 0);
}

#[test]
fn every_corruption_is_rejected_or_readable() {
    let types = every_type();
    let layout = RowLayout::new(&types);
    let bytes = batch::encode(&layout, &every_type_rows()).unwrap();
    let (mut accepted, mut rejected) = (0, 0);
    for at in 0..bytes.len() {
        for mask in [0x01, 0x80, 0xff] {
            let mut b = bytes.clone();
            b[at] ^= mask;
            match Batch::validate(&layout, &b) {
                Ok(batch) => {
                    read_all(&types, &batch);
                    accepted += 1;
                }
                Err(_) => rejected += 1,
            }
        }
        if let Ok(batch) = Batch::validate(&layout, &bytes[..at]) {
            read_all(&types, &batch);
        }
    }
    // Most bytes are values any bits of which are valid; the rest are checked.
    assert!(accepted > 0 && rejected > 0);
}

// The golden batch: two rows of the exchange schema's `entry` table.

/// The columns of `entry` in `schemas/exchange.schema`, in declaration order.
/// Written out because this crate may not depend on the schema compiler;
/// `crates/sim/tests/exchange_entry_batch.rs` checks them against the schema.
fn entry() -> Vec<Type> {
    vec![
        Type::Id128,            // id
        Type::U64,              // user_id
        Type::U32,              // asset
        Type::Enum8 { max: 3 }, // kind: credit, debit, hold, release
        Type::Decimal {
            precision: 38,
            scale: 18,
        }, // amount
        Type::Id128,            // ref
    ]
}

fn entry_rows() -> Vec<Vec<Value>> {
    let row = |id, kind, amount, r| {
        vec![
            Value::Id128(id),
            Value::U64(7),
            Value::U32(1),
            Value::Enum8(kind),
            Value::Decimal128(amount),
            Value::Id128(r),
        ]
    };
    vec![
        // A credit of 100.5 and a debit of 0.25 of asset 1 for user 7.
        row(
            0x0193_5b2a_1c00_0000_0000_0000_0000_0001,
            0,
            100_500_000_000_000_000_000,
            0x0193_5b2a_1c00_0000_0000_0000_0000_00f1,
        ),
        row(
            0x0193_5b2a_1c00_0000_0000_0000_0000_0002,
            1,
            250_000_000_000_000_000,
            0x0193_5b2a_1c00_0000_0000_0000_0000_00f2,
        ),
    ]
}

/// The bytes of a file of hex pairs, where `#` starts a comment.
fn hex(text: &str) -> Vec<u8> {
    let digits: String = (text.lines())
        .flat_map(|l| l.split('#').next().unwrap().split_whitespace())
        .collect();
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn golden_entry_batch() {
    let golden = hex(include_str!("entry.batch"));
    let types = entry();
    let layout = RowLayout::new(&types);
    assert_eq!(layout.width(), 61);
    let batch = Batch::validate(&layout, &golden).unwrap();
    same(&read_all(&types, &batch), &entry_rows());
    assert_eq!(batch::encode(&layout, &entry_rows()).unwrap(), golden);
}

// Result lists.

#[test]
fn an_empty_result_list_means_every_event_succeeded() {
    let results = Results::validate(&[], 3).unwrap();
    assert_eq!(results.iter().count(), 0);
    assert!((0..3).all(|i| results.code(i) == OK));
}

#[test]
fn listed_events_have_their_codes() {
    let bytes = results::encode(&[(1, 5), (4, 2)]);
    assert_eq!(bytes, [1, 0, 0, 0, 5, 0, 0, 0, 4, 0, 0, 0, 2, 0, 0, 0]);
    let results = Results::validate(&bytes, 5).unwrap();
    assert_eq!(results.iter().collect::<Vec<_>>(), [(1, 5), (4, 2)]);
    let codes: Vec<u32> = (0..5).map(|i| results.code(i)).collect();
    assert_eq!(codes, [OK, 5, OK, OK, 2]);
}

#[test]
fn malformed_result_lists_are_rejected() {
    let reject = |entries: &[(u32, u32)], events| {
        let bytes: Vec<u8> = (entries.iter())
            .flat_map(|(i, c)| [i.to_le_bytes(), c.to_le_bytes()].concat())
            .collect();
        Results::validate(&bytes, events).unwrap_err()
    };
    assert_eq!(reject(&[(2, 1), (1, 1)], 5), Error::InvalidResult);
    assert_eq!(reject(&[(1, 1), (1, 2)], 5), Error::InvalidResult);
    assert_eq!(reject(&[(5, 1)], 5), Error::InvalidResult);
    assert_eq!(reject(&[(0, OK)], 5), Error::InvalidResult);
    assert_eq!(Results::validate(&[0; 7], 5).unwrap_err(), Error::Truncated);
    let over = vec![0; BODY + 8];
    assert_eq!(
        Results::validate(&over, u32::MAX).unwrap_err(),
        Error::TooLarge
    );
}

#[test]
fn document_05_lists_every_code_and_the_row_limit() {
    let doc = include_str!("../../../docs/05-query-and-protocol.md");
    use Error::*;
    let codes = [
        Truncated,
        TooLarge,
        ReservedBits,
        CountTooLarge,
        OffsetBeyondHeap,
        OverlappingSlots,
        HeapGap,
        TooLong,
        InvalidUtf8,
        UnknownEnumValue,
        InvalidValue,
        InvalidResult,
    ];
    for (i, e) in codes.into_iter().enumerate() {
        assert_eq!(usize::from(e as u8), i + 1);
        assert!(doc.contains(&format!("| {} | `{e:?}` |", e as u8)), "{e:?}");
    }
    assert_eq!(ROWS_MAX, 131_040);
    assert!(doc.contains("`ROWS_MAX` is 131,040"));
}

/// A type in the catalog encoding of document 03 section 1.2, which the
/// `row_batch` fuzz target reads its schema in.
fn catalog_type(ty: &Type) -> Vec<u8> {
    let p = |p: &TimePrecision| match p {
        TimePrecision::Seconds => 0,
        TimePrecision::Millis => 1,
        TimePrecision::Micros => 2,
        TimePrecision::Nanos => 3,
    };
    let tagged = |tag: u8, rest: &[u8]| [&[tag], rest].concat();
    match ty {
        Type::U8 => vec![0],
        Type::U16 => vec![1],
        Type::U32 => vec![2],
        Type::U64 => vec![3],
        Type::U128 => vec![4],
        Type::U256 => vec![5],
        Type::I8 => vec![6],
        Type::I16 => vec![7],
        Type::I32 => vec![8],
        Type::I64 => vec![9],
        Type::I128 => vec![10],
        Type::I256 => vec![11],
        Type::Decimal { precision, scale } => vec![12, *precision, *scale],
        Type::F32 => vec![13],
        Type::F64 => vec![14],
        Type::Bool => vec![15],
        Type::Date => vec![16],
        Type::Timestamp(x) => vec![17, p(x)],
        Type::Duration(x) => vec![18, p(x)],
        Type::Uuid => vec![19],
        Type::Id128 => vec![20],
        Type::String { max } => tagged(21, &max.to_le_bytes()),
        Type::Bytes { max } => tagged(22, &max.to_le_bytes()),
        Type::FixedBytes(n) => tagged(23, &n.to_le_bytes()),
        Type::Enum8 { max } => vec![24, *max],
        Type::Enum16 { max } => tagged(25, &max.to_le_bytes()),
        Type::Nullable(inner) => tagged(26, &catalog_type(inner)),
        Type::LowCardinality(inner) => tagged(27, &catalog_type(inner)),
    }
}

/// Writes the seeds of the `row_batch` fuzz target: a column count, the
/// column types, then a batch. Run with `cargo test -p granite-protocol
/// --test batch -- --ignored write_fuzz_seeds`.
#[test]
#[ignore = "writes the fuzz corpus"]
fn write_fuzz_seeds() {
    let seed = |types: &[Type], batch: &[u8]| {
        let mut out = vec![u8::try_from(types.len()).unwrap()];
        types.iter().for_each(|t| out.extend(catalog_type(t)));
        out.extend(batch);
        out
    };
    let every = every_type();
    let every_batch = batch::encode(&RowLayout::new(&every), &every_type_rows()).unwrap();
    let small = [
        Type::U64,
        Type::String { max: 4 },
        Type::Enum8 { max: 1 },
        Type::Nullable(boxed(Type::Bool)),
    ];
    let seeds = [
        seed(&entry(), &hex(include_str!("entry.batch"))),
        seed(&every, &every_batch),
        seed(&small, &valid()),
        seed(&small, &with(SLOT_1, &[1, 0, 0, 0, 2, 0, 0, 0])),
        seed(&[Type::U8], &[0; HEADER_SIZE]),
        // A result list read from the same bytes: entries for events 1 and 4.
        seed(&[Type::U8], &results::encode(&[(1, 5), (4, 2)])),
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/row_batch");
    std::fs::create_dir_all(&dir).unwrap();
    for (i, s) in seeds.iter().enumerate() {
        std::fs::write(dir.join(format!("seed-{i:02}")), s).unwrap();
    }
}
