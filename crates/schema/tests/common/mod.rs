//! Pieces of random catalogs for property tests, shared with the schema text
//! compiler's tests in `crates/schemac`.

use granite_base::constants::FORMAT;
use granite_types::{TimePrecision, Type};

/// SplitMix64.
pub struct Rng(pub u64);

impl Rng {
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

    pub fn u128(&mut self) -> u128 {
        (u128::from(self.next()) << 64) | u128::from(self.next())
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }

    /// Mostly small, sometimes large, so that the row and key limits are hit.
    pub fn max(&mut self) -> u32 {
        let top = [1, 8, 300, 20_000][self.below(4) as usize];
        self.below(top) as u32
    }
}

/// A random valid type; with `key`, one allowed in a key.
pub fn random_type(rng: &mut Rng, key: bool) -> Type {
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
