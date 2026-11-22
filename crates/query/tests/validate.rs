//! The fragment validator of document 05 section 4: each rule with its
//! code, and the memory bound formula of every operator.

use granite_base::constants::{PROCESS_CONFIG_DEFAULT as P, ProcessConfig};
use granite_query::{
    Aggregate, BinOp, Error, Expr, Fragment, Invalid, KeyRange, Literal, Operator, Report, Scan,
};
use granite_schema::{Catalog, Column, Kind, Layout, Partitioning, Retention, Table, TableGroup};
use granite_types::key::Order;
use granite_types::{Type, Value};
use std::ops::Bound;

const B: u64 = P.batch_rows_max;

/// Widths of a value in a batch, as in a row (document 03 section 2.4).
const U64: u64 = 8;
const VENUE: u64 = 1 + 8 + 16;
const NOTE: u64 = 8 + 100;
const BOOL: u64 = 1;

/// One table, `trade`, keyed by (account, id desc), with a column of each
/// kind the rules tell apart.
fn catalog() -> Catalog {
    let col = |name: &str, ty: Type| Column {
        name: name.into(),
        ty,
    };
    Catalog {
        version: 1,
        groups: vec![TableGroup {
            name: "accounts".into(),
            partitioning: Partitioning::Hash,
            partition_key: vec![col("account", Type::U64)],
            tables: vec![Table {
                name: "trade".into(),
                kind: Kind::Immutable,
                columns: vec![
                    col("account", Type::U64),
                    col("id", Type::U64),
                    col(
                        "price",
                        Type::Decimal {
                            precision: 18,
                            scale: 2,
                        },
                    ),
                    col("qty", Type::I64),
                    col("venue", Type::Nullable(Box::new(Type::String { max: 16 }))),
                    col(
                        "note",
                        Type::LowCardinality(Box::new(Type::String { max: 100 })),
                    ),
                    col("px", Type::F64),
                    col("side", Type::Enum8 { max: 1 }),
                    col("small", Type::U32),
                ],
                primary_key: vec![(0, Order::Asc), (1, Order::Desc)],
                layout: Some(Layout::Columnar),
                retention: Retention::Forever,
            }],
        }],
    }
}

fn scan(columns: Vec<u16>) -> Scan {
    Scan {
        table: "trade".into(),
        columns,
        ranges: vec![],
        time: (0, u64::MAX),
        predicate: None,
    }
}

fn fragment(scan: Scan, operators: Vec<Operator>) -> Fragment {
    Fragment { scan, operators }
}

fn col(i: u16) -> Box<Expr> {
    Box::new(Expr::Column(i))
}

fn lit(ty: Type, v: Value) -> Box<Expr> {
    Box::new(Expr::Literal(Box::new((ty, v))))
}

fn bin(op: BinOp, a: Box<Expr>, b: Box<Expr>) -> Expr {
    Expr::Binary(op, a, b)
}

fn validate(f: &Fragment) -> Result<Report, Invalid> {
    f.validate(&catalog(), &P)
}

fn rejects(f: &Fragment, error: Error, node: u32) {
    assert_eq!(validate(f), Err(Invalid { error, node }));
}

/// A scan of `columns` with `predicate`.
fn filtered(columns: Vec<u16>, predicate: Expr) -> Fragment {
    fragment(
        Scan {
            predicate: Some(predicate),
            ..scan(columns)
        },
        vec![],
    )
}

#[test]
fn unknown_table() {
    let f = fragment(
        Scan {
            table: "nope".into(),
            ..scan(vec![0])
        },
        vec![],
    );
    rejects(&f, Error::UnknownTable, 0);
}

#[test]
fn unknown_column() {
    rejects(&fragment(scan(vec![0, 9]), vec![]), Error::UnknownColumn, 0);
    // Column 1 of a scan of one column.
    rejects(&filtered(vec![0], *col(1)), Error::UnknownColumn, 1);
    // After a projection to one column, column 1 is gone.
    let f = fragment(
        scan(vec![0, 1]),
        vec![
            Operator::Project(vec![Expr::Column(1)]),
            Operator::Project(vec![Expr::Column(1)]),
        ],
    );
    rejects(&f, Error::UnknownColumn, 4);
}

/// A scan without a column list (gate: unbounded fragments, each with its code).
#[test]
fn a_scan_without_a_column_list() {
    rejects(&fragment(scan(vec![]), vec![]), Error::NoColumns, 0);
}

/// A partial aggregate without a group-table bound.
#[test]
fn an_aggregate_without_a_group_bound() {
    let aggregate = |groups| Operator::PartialAggregate {
        keys: vec![Expr::Column(0)],
        aggregates: vec![Aggregate::Count],
        groups,
    };
    rejects(
        &fragment(scan(vec![0]), vec![aggregate(0)]),
        Error::NoGroupBound,
        1,
    );
    assert!(validate(&fragment(scan(vec![0]), vec![aggregate(1)])).is_ok());
}

/// A top-N without n.
#[test]
fn a_top_n_without_n() {
    let top = |n| Operator::TopN {
        order: vec![(Expr::Column(0), Order::Asc)],
        n,
    };
    rejects(&fragment(scan(vec![0]), vec![top(0)]), Error::NoTopN, 1);
    assert!(validate(&fragment(scan(vec![0]), vec![top(1)])).is_ok());
}

#[test]
fn nothing_follows_a_partial_aggregate() {
    let f = fragment(
        scan(vec![0]),
        vec![
            Operator::PartialAggregate {
                keys: vec![Expr::Column(0)],
                aggregates: vec![],
                groups: 10,
            },
            Operator::Limit(1),
        ],
    );
    rejects(&f, Error::AfterAggregate, 3);
}

#[test]
fn type_mismatch() {
    // A predicate that is not a bool.
    rejects(&filtered(vec![0], *col(0)), Error::TypeMismatch, 1);
    // Arithmetic on a string, comparison of floats, a cast of a string.
    rejects(
        &filtered(vec![5], Expr::Neg(col(0))),
        Error::TypeMismatch,
        2,
    );
    rejects(
        &filtered(vec![6], bin(BinOp::Lt, col(0), col(0))),
        Error::TypeMismatch,
        2,
    );
    let cast = Expr::Cast(col(0), Type::U64);
    rejects(
        &filtered(vec![5], bin(BinOp::Eq, Box::new(cast), col(0))),
        Error::TypeMismatch,
        2,
    );
    // A sum of an enum, a minimum of a float, a top-N by a float.
    let aggregate = |a| Operator::PartialAggregate {
        keys: vec![],
        aggregates: vec![a],
        groups: 1,
    };
    let f = fragment(
        scan(vec![7]),
        vec![aggregate(Aggregate::Sum(Expr::Column(0)))],
    );
    rejects(&f, Error::TypeMismatch, 3);
    let f = fragment(
        scan(vec![6]),
        vec![aggregate(Aggregate::Min(Expr::Column(0)))],
    );
    rejects(&f, Error::TypeMismatch, 3);
    let top = Operator::TopN {
        order: vec![(Expr::Column(0), Order::Asc)],
        n: 1,
    };
    rejects(&fragment(scan(vec![6]), vec![top]), Error::TypeMismatch, 2);
    // A key bound of another type.
    let f = fragment(
        Scan {
            ranges: vec![KeyRange {
                lower: Bound::Included(vec![(Type::Bool, Value::Bool(true))]),
                upper: Bound::Unbounded,
            }],
            ..scan(vec![0])
        },
        vec![],
    );
    rejects(&f, Error::TypeMismatch, 2);
}

#[test]
fn implicit_cast() {
    let u32_ = lit(Type::U32, Value::U32(1));
    rejects(
        &filtered(vec![0], bin(BinOp::Eq, col(0), u32_.clone())),
        Error::ImplicitCast,
        3,
    );
    let cast = Box::new(Expr::Cast(u32_, Type::U64));
    assert!(validate(&filtered(vec![0], bin(BinOp::Eq, col(0), cast))).is_ok());
}

#[test]
fn nullable_operand() {
    // venue = venue, where venue may be null.
    rejects(
        &filtered(vec![4], bin(BinOp::Eq, col(0), col(0))),
        Error::NullableOperand,
        2,
    );
    // A group key that may be null.
    let f = fragment(
        scan(vec![4]),
        vec![Operator::PartialAggregate {
            keys: vec![Expr::Column(0)],
            aggregates: vec![],
            groups: 1,
        }],
    );
    rejects(&f, Error::NullableOperand, 2);
    // Tested first, it is a value.
    let s = lit(Type::String { max: 16 }, Value::String(String::new()));
    let unwrapped = Box::new(Expr::UnwrapOr(col(0), s));
    assert!(
        validate(&filtered(
            vec![4],
            bin(BinOp::Eq, unwrapped.clone(), unwrapped)
        ))
        .is_ok()
    );
}

#[test]
fn key_bounds_give_one_to_all_key_values() {
    let u64_ = |v| -> Literal { (Type::U64, Value::U64(v)) };
    let range = |lower: Vec<Literal>| {
        fragment(
            Scan {
                ranges: vec![KeyRange {
                    lower: Bound::Included(lower),
                    upper: Bound::Excluded(vec![u64_(9)]),
                }],
                ..scan(vec![0])
            },
            vec![],
        )
    };
    assert!(validate(&range(vec![u64_(1)])).is_ok());
    assert!(validate(&range(vec![u64_(1), u64_(2)])).is_ok());
    rejects(&range(vec![]), Error::WrongArity, 1);
    rejects(
        &range(vec![u64_(1), u64_(2), u64_(3)]),
        Error::WrongArity,
        1,
    );
}

#[test]
fn an_encoding_error_is_reported_for_the_whole_fragment() {
    let mut e = Expr::Column(0);
    for _ in 0..100 {
        e = Expr::Not(Box::new(e));
    }
    rejects(&filtered(vec![7], e), Error::TooDeep, 0);
}

/// Scan: one batch of its columns, and one batch of every value its
/// predicate computes.
#[test]
fn the_scan_bound() {
    let f = fragment(scan(vec![0, 4, 5]), vec![]);
    let r = validate(&f).unwrap();
    assert_eq!(r.memory, [B * (U64 + VENUE + NOTE)]);
    assert_eq!(r.total, B * (U64 + VENUE + NOTE));
    // not(is_null(venue)): venue, then two bools.
    let p = Expr::Not(Box::new(Expr::IsNull(col(1))));
    let r = validate(&filtered(vec![0, 4], p)).unwrap();
    assert_eq!(r.memory, [B * (U64 + VENUE + VENUE + BOOL + BOOL)]);
}

/// Project: one batch of its output, and one batch of every value its
/// expressions compute.
#[test]
fn the_project_bound() {
    let qty_plus_one = bin(BinOp::Add, col(1), lit(Type::I64, Value::I64(1)));
    let f = fragment(
        scan(vec![0, 3]),
        vec![Operator::Project(vec![qty_plus_one, Expr::Column(0)])],
    );
    let r = validate(&f).unwrap();
    assert_eq!(r.columns, [Type::I64, Type::U64]);
    // Output i64 and u64; computed: qty, 1, the sum, and account.
    let project = B * (U64 + U64) + B * (U64 + U64 + U64 + U64);
    assert_eq!(r.memory, [B * (U64 + U64), project]);
    assert_eq!(r.total, B * (U64 + U64) + project);
}

/// Partial aggregate: a group table of `groups` entries, each its output
/// row and 8 bytes of index; one batch of its output (a flush), and one
/// batch of every value its keys and arguments compute.
#[test]
fn the_partial_aggregate_bound() {
    let f = fragment(
        scan(vec![0, 5, 3]),
        vec![Operator::PartialAggregate {
            keys: vec![Expr::Column(0), Expr::Column(1)],
            aggregates: vec![
                Aggregate::Count,
                Aggregate::Sum(Expr::Column(2)),
                Aggregate::Max(Expr::Column(1)),
            ],
            groups: 1000,
        }],
    );
    let r = validate(&f).unwrap();
    let string = Type::String { max: 100 };
    assert_eq!(
        r.columns,
        [Type::U64, string.clone(), Type::U64, Type::I64, string]
    );
    let out = U64 + NOTE + U64 + U64 + NOTE;
    let computed = U64 + NOTE + U64 + NOTE;
    let aggregate = 1000 * (out + 8) + B * out + B * computed;
    assert_eq!(r.memory, [B * (U64 + NOTE + U64), aggregate]);
}

/// Top-N: n rows of its input, each with its order key; one batch of its
/// output, and one batch of every value its order keys compute.
#[test]
fn the_top_n_bound() {
    let f = fragment(
        scan(vec![0, 4, 3]),
        vec![Operator::TopN {
            order: vec![
                (Expr::Neg(col(2)), Order::Desc),
                (Expr::Column(0), Order::Asc),
            ],
            n: 50,
        }],
    );
    let r = validate(&f).unwrap();
    let input = U64 + VENUE + U64;
    let keys = U64 + U64;
    let computed = U64 + U64 + U64;
    assert_eq!(
        r.memory,
        [B * input, 50 * (input + keys) + B * input + B * computed]
    );
}

/// Limit: one batch of its output.
#[test]
fn the_limit_bound() {
    let f = fragment(scan(vec![0, 5]), vec![Operator::Limit(10)]);
    let r = validate(&f).unwrap();
    assert_eq!(r.memory, [B * (U64 + NOTE), B * (U64 + NOTE)]);
    assert_eq!(r.total, 2 * B * (U64 + NOTE));
}

/// A scan of account and a top-N of `n` by it: 3 batches of u64s and n
/// rows of two u64s.
fn top(n: u32) -> Fragment {
    fragment(
        scan(vec![0]),
        vec![Operator::TopN {
            order: vec![(Expr::Column(0), Order::Asc)],
            n,
        }],
    )
}

#[test]
fn the_memory_limit_at_its_value_and_one_over() {
    let fixed = 3 * B * U64;
    let per_row = 2 * U64;
    assert_eq!((P.fragment_memory_max - fixed) % per_row, 0);
    let n = u32::try_from((P.fragment_memory_max - fixed) / per_row).unwrap();
    assert_eq!(validate(&top(n)).unwrap().total, P.fragment_memory_max);
    rejects(&top(n + 1), Error::OverMemoryLimit, 0);
    // A smaller limit applies as given.
    let config = ProcessConfig {
        fragment_memory_max: fixed + per_row,
        ..P
    };
    assert!(top(1).validate(&catalog(), &config).is_ok());
    let over = top(2).validate(&catalog(), &config);
    assert_eq!(
        over.map(|r| r.total),
        Err(Invalid {
            error: Error::OverMemoryLimit,
            node: 0
        })
    );
}

/// Bounds near u64::MAX saturate rather than wrap, and are rejected.
#[test]
fn the_largest_bounds_saturate() {
    rejects(&top(u32::MAX), Error::OverMemoryLimit, 0);
    let config = ProcessConfig {
        batch_rows_max: u64::MAX,
        fragment_memory_max: u64::MAX - 1,
        ..P
    };
    let f = fragment(scan(vec![5]), vec![]);
    assert_eq!(
        f.validate(&catalog(), &config),
        Err(Invalid {
            error: Error::OverMemoryLimit,
            node: 0
        })
    );
}

/// Writes the fragments of these tests, valid and not, as the seed corpus
/// of the `fragment_decode` and `fragment_validate` fuzz targets, whose
/// catalog has the same table.
#[test]
#[ignore = "run by hand to reseed the fuzz corpora"]
fn write_fuzz_seeds() {
    let aggregate = Operator::PartialAggregate {
        keys: vec![Expr::Column(0), Expr::Column(1)],
        aggregates: vec![
            Aggregate::Count,
            Aggregate::Sum(Expr::Column(2)),
            Aggregate::Min(Expr::Column(1)),
            Aggregate::Max(Expr::Column(1)),
        ],
        groups: 1000,
    };
    let ranged = Scan {
        ranges: vec![KeyRange {
            lower: Bound::Included(vec![(Type::U64, Value::U64(1)), (Type::U64, Value::U64(2))]),
            upper: Bound::Excluded(vec![(Type::U64, Value::U64(9))]),
        }],
        time: (1, 2),
        predicate: Some(Expr::Not(Box::new(Expr::IsNull(col(1))))),
        ..scan(vec![0, 4, 3])
    };
    let s = lit(Type::String { max: 16 }, Value::String("x".into()));
    let seeds = [
        fragment(scan(vec![0, 5, 3]), vec![aggregate]),
        fragment(ranged, vec![Operator::Limit(10)]),
        fragment(
            scan(vec![0, 4, 3]),
            vec![
                Operator::Project(vec![
                    Expr::UnwrapOr(col(1), s),
                    bin(BinOp::Mul, col(2), lit(Type::I64, Value::I64(2))),
                    Expr::Cast(col(0), Type::F64),
                ]),
                Operator::TopN {
                    order: vec![
                        (Expr::Neg(col(1)), Order::Desc),
                        (Expr::Column(0), Order::Asc),
                    ],
                    n: 50,
                },
            ],
        ),
        filtered(
            vec![0, 6],
            bin(BinOp::Or, Box::new(bin(BinOp::Lt, col(0), col(0))), col(1)),
        ),
        top(u32::MAX),
        fragment(scan(vec![]), vec![]),
    ];
    for target in ["fragment_decode", "fragment_validate"] {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fuzz/corpus")
            .join(target);
        std::fs::create_dir_all(&dir).unwrap();
        for (i, f) in seeds.iter().enumerate() {
            std::fs::write(dir.join(format!("seed-{i:02}")), f.encode().unwrap()).unwrap();
        }
    }
}
