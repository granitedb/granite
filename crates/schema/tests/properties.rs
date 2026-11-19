//! Property test of the static row bound, from a logged seed: for random
//! valid schemas and random valid rows, the encoded row never exceeds
//! `Table::row_size_max`, and a row with every String and Bytes at its
//! maximum length reaches it. Each schema also round-trips through the
//! catalog encoding. Rerun a failure with
//! `GRANITE_SEED=<seed> cargo test -p granite-schema --test properties`.

use granite_base::constants::FORMAT;
use granite_schema::{Catalog, Column, Kind, Layout, Partitioning, Retention, Table, TableGroup};
use granite_types::key::Order;
use granite_types::{I256, Type, U256, Value, decimal_width};

mod common;
use common::{Rng, random_type};

const SCHEMAS: u32 = 2_000;
const ROWS: u32 = 50;

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
        // Mantissas within ±(10^P - 1); an i256 with |hi| <= 1 is under 2^129 < 10^39.
        Type::Decimal { precision, .. } => {
            let p = u32::from(*precision);
            match decimal_width(*precision) {
                4 => Value::Decimal32(rng.next() as i32 % 10i32.pow(p)),
                8 => Value::Decimal64(rng.next() as i64 % 10i64.pow(p)),
                16 => Value::Decimal128(rng.u128() as i128 % 10i128.pow(p)),
                _ => Value::Decimal256(I256 {
                    hi: rng.u128() as i128 % 2,
                    lo: rng.u128(),
                }),
            }
        }
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
