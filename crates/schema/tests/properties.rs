//! Property test of the static row bound, from a logged seed: for random
//! valid schemas and random valid rows, the encoded row never exceeds
//! `Table::row_size_max`, and a row with every String and Bytes at its
//! maximum length reaches it. Each schema also round-trips through the
//! catalog encoding. Rerun a failure with
//! `GRANITE_SEED=<seed> cargo test -p granite-schema --test properties`.

use granite_base::constants::FORMAT;
use granite_schema::{Catalog, Column, Kind, Layout, Partitioning, Retention, Table, TableGroup};
use granite_types::key::Order;
use granite_types::{I256, TimePrecision, Type, U256, Value, decimal_width};

const SCHEMAS: u32 = 2_000;
const ROWS: u32 = 50;

/// SplitMix64.
struct Rng(u64);

impl Rng {
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

    fn u128(&mut self) -> u128 {
        (u128::from(self.next()) << 64) | u128::from(self.next())
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }

    /// Mostly small, sometimes large, so that the row and key limits are hit.
    fn max(&mut self) -> u32 {
        let top = [1, 8, 300, 20_000][self.below(4) as usize];
        self.below(top) as u32
    }
}

fn random_type(rng: &mut Rng, key: bool) -> Type {
    let precision = |rng: &mut Rng| match rng.below(4) {
        0 => TimePrecision::Seconds,
        1 => TimePrecision::Millis,
        2 => TimePrecision::Micros,
        _ => TimePrecision::Nanos,
    };
    let base = match rng.below(26) {
        0 => Type::U8,
        1 => Type::U16,
        2 => Type::U32,
        3 => Type::U64,
        4 => Type::U128,
        5 => Type::U256,
        6 => Type::I8,
        7 => Type::I16,
        8 => Type::I32,
        9 => Type::I64,
        10 => Type::I128,
        11 => Type::I256,
        12 => {
            let precision = 1 + rng.below(FORMAT.decimal_precision_max) as u8;
            let scale = rng.below(u64::from(precision) + 1) as u8;
            Type::Decimal { precision, scale }
        }
        13 if !key => Type::F32,
        14 if !key => Type::F64,
        15 => Type::Bool,
        16 => Type::Date,
        17 => Type::Timestamp(precision(rng)),
        18 => Type::Duration(precision(rng)),
        19 => Type::Uuid,
        20 => Type::Id128,
        21 => Type::String { max: rng.max() },
        22 => Type::Bytes { max: rng.max() },
        23 => Type::FixedBytes(1 + rng.max()),
        24 => Type::Enum8 {
            max: rng.next() as u8,
        },
        25 => Type::Enum16 {
            max: rng.next() as u16,
        },
        _ => Type::Id128,
    };
    let lc = |t| Type::LowCardinality(Box::new(t));
    let null = |t| Type::Nullable(Box::new(t));
    match rng.below(10) {
        0 | 1 => lc(base),
        2 if !key => null(base),
        3 if !key => lc(null(base)),
        4 if !key => null(lc(base)),
        5 if !key => lc(null(lc(base))),
        _ => base,
    }
}

/// A valid catalog of one group and one random table, or `None` when the
/// random table breaks the key or row limit.
fn random_catalog(rng: &mut Rng) -> Option<Catalog> {
    let key_len = 1 + rng.below(3) as usize;
    let top = [4, 40, 300][rng.below(3) as usize];
    let other = rng.below(top) as usize;
    let columns: Vec<Column> = (0..key_len + other)
        .map(|i| Column {
            name: format!("c{i}"),
            ty: random_type(rng, i < key_len),
        })
        .collect();
    let order = |rng: &mut Rng| [Order::Asc, Order::Desc][rng.below(2) as usize];
    let primary_key = (0..key_len as u16).map(|i| (i, order(rng))).collect();
    let n = columns.len() as u16;
    let layout = match rng.below(3) {
        0 => Layout::Row,
        1 => Layout::Columnar,
        _ => {
            let split = 1 + rng.below(u64::from(n)) as u16;
            let groups = [(0..split).collect(), (split..n).rev().collect()];
            Layout::Groups(
                groups
                    .into_iter()
                    .filter(|g: &Vec<u16>| !g.is_empty())
                    .collect(),
            )
        }
    };
    let (kind, retention) = match rng.below(3) {
        0 => (Kind::Immutable, Retention::Forever),
        1 => (Kind::Versioned, Retention::Forever),
        _ => (Kind::Versioned, Retention::DropAfterMs(rng.next())),
    };
    let catalog = Catalog {
        version: rng.next(),
        groups: vec![TableGroup {
            name: "g".into(),
            partitioning: [Partitioning::Hash, Partitioning::Range][rng.below(2) as usize],
            partition_key: columns[..1].to_vec(),
            tables: vec![Table {
                name: "t".into(),
                kind,
                columns,
                primary_key,
                layout: Some(layout),
                retention,
            }],
        }],
    };
    catalog.validate().ok().map(|()| catalog)
}

/// A random valid value of `ty`; with `full`, String and Bytes at their
/// maximum length and no nulls.
fn random_value(ty: &Type, rng: &mut Rng, full: bool) -> Value {
    let i256 = |rng: &mut Rng| I256 {
        hi: rng.u128() as i128,
        lo: rng.u128(),
    };
    let len = |rng: &mut Rng, max: u32| {
        if full {
            max as usize
        } else {
            rng.below(u64::from(max) + 1) as usize
        }
    };
    match ty {
        Type::Nullable(_) if !full && rng.below(4) == 0 => Value::Null,
        Type::Nullable(inner) | Type::LowCardinality(inner) => random_value(inner, rng, full),
        Type::U8 => Value::U8(rng.next() as u8),
        Type::U16 => Value::U16(rng.next() as u16),
        Type::U32 => Value::U32(rng.next() as u32),
        Type::U64 => Value::U64(rng.next()),
        Type::U128 => Value::U128(rng.u128()),
        Type::U256 => Value::U256(U256 {
            hi: rng.u128(),
            lo: rng.u128(),
        }),
        Type::I8 => Value::I8(rng.next() as i8),
        Type::I16 => Value::I16(rng.next() as i16),
        Type::I32 => Value::I32(rng.next() as i32),
        Type::I64 => Value::I64(rng.next() as i64),
        Type::I128 => Value::I128(rng.u128() as i128),
        Type::I256 => Value::I256(i256(rng)),
        Type::Decimal { precision, .. } => match decimal_width(*precision) {
            4 => Value::Decimal32(rng.next() as i32),
            8 => Value::Decimal64(rng.next() as i64),
            16 => Value::Decimal128(rng.u128() as i128),
            _ => Value::Decimal256(i256(rng)),
        },
        Type::F32 => Value::F32(f32::from_bits(rng.next() as u32)),
        Type::F64 => Value::F64(f64::from_bits(rng.next())),
        Type::Bool => Value::Bool(rng.next() & 1 == 1),
        Type::Date => Value::Date(rng.next() as i32),
        Type::Timestamp(_) => Value::Timestamp(rng.next() as i64),
        Type::Duration(_) => Value::Duration(rng.next() as i64),
        Type::Uuid => Value::Uuid(rng.bytes(16).try_into().unwrap()),
        Type::Id128 => Value::Id128(rng.u128()),
        Type::String { max } => {
            let mut left = len(rng, *max);
            let mut s = String::new();
            while left > 0 {
                let c = ['\0', 'a', 'é', '€', '\u{10FFFF}'][rng.below(5) as usize];
                let c = if c.len_utf8() <= left { c } else { 'a' };
                s.push(c);
                left -= c.len_utf8();
            }
            Value::String(s)
        }
        Type::Bytes { max } => {
            let n = len(rng, *max);
            Value::Bytes(rng.bytes(n))
        }
        Type::FixedBytes(n) => Value::FixedBytes(rng.bytes(*n as usize)),
        Type::Enum8 { max } => Value::Enum8(rng.below(u64::from(*max) + 1) as u8),
        Type::Enum16 { max } => Value::Enum16(rng.below(u64::from(*max) + 1) as u16),
    }
}

#[test]
fn encoded_rows_stay_within_the_static_bound() {
    let seed = std::env::var("GRANITE_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5EED);
    println!("GRANITE_SEED={seed}");
    let mut rng = Rng(seed);
    let (mut schemas, mut rejected) = (0, 0);
    while schemas < SCHEMAS {
        let Some(catalog) = random_catalog(&mut rng) else {
            rejected += 1;
            continue;
        };
        schemas += 1;
        let bytes = catalog.encode().unwrap();
        assert_eq!(Catalog::decode(&bytes).as_ref(), Ok(&catalog));
        assert_eq!(catalog.encode(), Ok(bytes));
        let table = &catalog.groups[0].tables[0];
        let bound = table.row_size_max();
        assert!(bound <= FORMAT.row_size_max);
        for i in 0..=ROWS {
            let full = i == ROWS;
            let values: Vec<Value> = table
                .columns
                .iter()
                .map(|c| random_value(&c.ty, &mut rng, full))
                .collect();
            let size = table.encode_row(&values).unwrap().len() as u64;
            assert!(size <= bound, "{size} > {bound}: {table:?}");
            if full {
                assert_eq!(size, bound, "a full row reaches the bound: {table:?}");
            }
        }
    }
    // The generator must also produce schemas over the limits, or it tests too little.
    println!("{schemas} valid schemas, {rejected} over a limit");
    assert!(rejected > 0);
}
