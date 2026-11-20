//! Property tests of the program encoding, from a logged seed:
//! - round trip: encode then decode returns an identical tree;
//! - canonical: bytes the decoder accepts, here random corruptions of valid
//!   encodings, are the encoding of the tree it returns, so a program has
//!   exactly one encoding and every other one is rejected.
//!
//! `just test` runs 20,000 programs. The gate count of T3.3.1, 1,000,000,
//! runs in release with
//! `GRANITE_FULL=1 cargo test --release --locked -p granite-txn --test properties -- --nocapture`.
//! Rerun a failure with `GRANITE_SEED=<seed>`.

#[path = "../../schema/tests/common/mod.rs"]
mod common;

use common::{Rng, random_type};
use granite_txn::{BinOp, Expr, Param, Program, Scan, Stmt};
use granite_types::{I256, Type, U256, Value, canonical_f32, canonical_f64, decimal_width};
use std::ops::Bound;

fn seeded(test: &str) -> Rng {
    let seed = std::env::var("GRANITE_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5EED);
    println!("{test}: GRANITE_SEED={seed}");
    Rng(seed)
}

fn programs() -> u64 {
    if std::env::var_os("GRANITE_FULL").is_some() {
        1_000_000
    } else {
        20_000
    }
}

const OPS: [BinOp; 13] = [
    BinOp::Add,
    BinOp::Sub,
    BinOp::Mul,
    BinOp::Div,
    BinOp::Rem,
    BinOp::Eq,
    BinOp::Ne,
    BinOp::Lt,
    BinOp::Le,
    BinOp::Gt,
    BinOp::Ge,
    BinOp::And,
    BinOp::Or,
];

/// A name of 0 to 255 bytes, sometimes not ASCII.
fn name(rng: &mut Rng) -> String {
    match rng.below(8) {
        0 => String::new(),
        1 => "é".repeat(127),
        2 => "x".repeat(255),
        _ => format!("n{}", rng.below(1000)),
    }
}

fn small(rng: &mut Rng) -> u16 {
    [0, 1, rng.next() as u16, u16::MAX][rng.below(4) as usize]
}

/// A random valid value of `ty`. Decimal mantissas stay within P digits.
fn value(ty: &Type, rng: &mut Rng) -> Value {
    let len = |rng: &mut Rng, max: u32| rng.below(u64::from(max.min(300)) + 1) as usize;
    match ty {
        Type::Nullable(_) if rng.below(3) == 0 => Value::Null,
        Type::Nullable(inner) | Type::LowCardinality(inner) => value(inner, rng),
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
        Type::I256 => Value::I256(I256 {
            hi: rng.u128() as i128,
            lo: rng.u128(),
        }),
        Type::Decimal { precision, .. } => {
            let digits = u32::from(*precision).min(18);
            let m = rng.below(10u64.pow(digits)) as i64 * [1, -1][rng.below(2) as usize];
            match decimal_width(*precision) {
                4 => Value::Decimal32(m as i32),
                8 => Value::Decimal64(m),
                16 => Value::Decimal128(i128::from(m)),
                _ => Value::Decimal256(I256 {
                    hi: if m < 0 { -1 } else { 0 },
                    lo: i128::from(m) as u128,
                }),
            }
        }
        Type::F32 => Value::F32(canonical_f32(f32::from_bits(rng.next() as u32))),
        Type::F64 => Value::F64(canonical_f64(f64::from_bits(rng.next()))),
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

fn exprs(rng: &mut Rng, depth: u32) -> Vec<Expr> {
    (0..rng.below(4)).map(|_| expr(rng, depth)).collect()
}

fn boxed(rng: &mut Rng, depth: u32) -> Box<Expr> {
    Box::new(expr(rng, depth))
}

fn expr(rng: &mut Rng, depth: u32) -> Expr {
    let leaf = depth == 0;
    let d = depth.saturating_sub(1);
    match rng.below(if leaf { 4 } else { 14 }) {
        0 => {
            let ty = random_type(rng, false);
            let v = value(&ty, rng);
            Expr::Literal(Box::new((ty, v)))
        }
        1 => Expr::Param(rng.next() as u8),
        2 => Expr::Local(small(rng)),
        3 => Expr::CommitTs,
        4 => Expr::Field(boxed(rng, d), small(rng)),
        5 => Expr::Get {
            table: name(rng),
            key: exprs(rng, d),
        },
        6 => Expr::Neg(boxed(rng, d)),
        7 => Expr::Not(boxed(rng, d)),
        8 => Expr::IsNull(boxed(rng, d)),
        9 => Expr::UnwrapOr(boxed(rng, d), boxed(rng, d)),
        10 => Expr::Match {
            value: boxed(rng, d),
            if_null: boxed(rng, d),
            present: boxed(rng, d),
        },
        11 => Expr::Cast(boxed(rng, d), random_type(rng, false)),
        _ => Expr::Binary(OPS[rng.below(13) as usize], boxed(rng, d), boxed(rng, d)),
    }
}

fn bound(rng: &mut Rng, depth: u32) -> Bound<Expr> {
    match rng.below(3) {
        0 => Bound::Unbounded,
        1 => Bound::Included(expr(rng, depth)),
        _ => Bound::Excluded(expr(rng, depth)),
    }
}

fn block(rng: &mut Rng, depth: u32) -> Vec<Stmt> {
    (0..rng.below(4)).map(|_| stmt(rng, depth)).collect()
}

fn stmt(rng: &mut Rng, depth: u32) -> Stmt {
    let d = depth.saturating_sub(1);
    // Blocks only while depth is left, so trees stay small.
    match rng.below(if depth == 0 { 7 } else { 9 }) {
        0 => Stmt::Let(expr(rng, d)),
        1 => Stmt::Require {
            cond: expr(rng, d),
            code: name(rng),
        },
        2 => Stmt::Insert {
            table: name(rng),
            row: exprs(rng, d),
        },
        3 => Stmt::Upsert {
            table: name(rng),
            row: exprs(rng, d),
        },
        4 => Stmt::Update {
            table: name(rng),
            key: exprs(rng, d),
            set: (0..rng.below(4))
                .map(|_| (small(rng), expr(rng, d)))
                .collect(),
        },
        5 => Stmt::Delete {
            table: name(rng),
            key: exprs(rng, d),
        },
        6 => Stmt::Return(expr(rng, d)),
        7 => Stmt::If {
            cond: expr(rng, d),
            then: block(rng, d),
            otherwise: block(rng, d),
        },
        _ => Stmt::For {
            scan: Box::new(Scan {
                table: name(rng),
                prefix: exprs(rng, d),
                lower: bound(rng, d),
                upper: bound(rng, d),
                limit: small(rng),
            }),
            body: block(rng, d),
        },
    }
}

fn program(rng: &mut Rng) -> Program {
    let depth = rng.below(5) as u32;
    Program {
        name: name(rng),
        params: (0..rng.below(5))
            .map(|_| Param {
                name: name(rng),
                ty: random_type(rng, false),
            })
            .collect(),
        body: block(rng, depth),
    }
}

/// Trees are compared by their debug form, which tells every value apart:
/// `==` on a float literal is false for NaN.
fn same(a: &Program, b: &Program) -> bool {
    format!("{a:?}") == format!("{b:?}")
}

#[test]
fn encode_then_decode_is_identity() {
    let mut rng = seeded("encode_then_decode_is_identity");
    let n = programs();
    for i in 0..n {
        let p = program(&mut rng);
        let bytes = p
            .encode()
            .unwrap_or_else(|e| panic!("program {i}: {e:?}\n{p:?}"));
        let q = Program::decode(&bytes).unwrap_or_else(|e| panic!("program {i}: {e:?}\n{p:?}"));
        assert!(same(&p, &q), "program {i}:\n{p:?}\n{q:?}");
    }
    println!("{n} programs round-tripped");
}

#[test]
fn accepted_bytes_are_the_encoding_of_their_tree() {
    let mut rng = seeded("accepted_bytes_are_the_encoding_of_their_tree");
    let (mut accepted, mut rejected) = (0u64, 0u64);
    for _ in 0..programs() / 4 {
        let mut bytes = program(&mut rng).encode().unwrap();
        for _ in 0..1 + rng.below(3) {
            let i = rng.below(bytes.len() as u64) as usize;
            match rng.below(3) {
                0 => bytes[i] ^= 1 << rng.below(8),
                1 => bytes[i] = rng.next() as u8,
                _ => {
                    bytes.remove(i);
                }
            }
        }
        match Program::decode(&bytes) {
            Ok(p) => {
                accepted += 1;
                assert_eq!(p.encode().as_deref(), Ok(&bytes[..]), "{p:?}");
            }
            Err(_) => rejected += 1,
        }
    }
    println!("corrupted encodings: {accepted} accepted and canonical, {rejected} rejected");
    assert!(accepted > 0 && rejected > 0);
}
