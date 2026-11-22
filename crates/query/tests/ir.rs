//! The fragment encoding of document 05 section 4: a golden fragment, every
//! opcode, the limits at and one past their value, the version, and
//! canonical literals.

use granite_base::constants::FORMAT;
use granite_query::{
    Aggregate, BinOp, Error, Expr, FRAGMENT_VERSION, Fragment, KeyRange, Operator, Scan,
};
use granite_types::key::Order;
use granite_types::{Type, Value};
use std::ops::Bound;

fn scan(columns: Vec<u16>) -> Scan {
    Scan {
        table: "t".into(),
        columns,
        ranges: vec![],
        time: (0, u64::MAX),
        predicate: None,
    }
}

fn col(i: u16) -> Box<Expr> {
    Box::new(Expr::Column(i))
}

fn u64_(v: u64) -> Box<Expr> {
    Box::new(Expr::Literal(Box::new((Type::U64, Value::U64(v)))))
}

/// Version 1, table "t", columns 0 and 1, no range, every timestamp: the
/// bytes before the predicate.
fn head() -> Vec<u8> {
    let mut b = FRAGMENT_VERSION.to_le_bytes().to_vec();
    b.extend([1, b't', 2, 0, 0, 0, 1, 0, 0, 0]);
    b.extend(0u64.to_le_bytes());
    b.extend(u64::MAX.to_le_bytes());
    b
}

/// A fragment with no predicate and the one operator `op`.
fn with_operator(op: &[u8]) -> Result<Fragment, Error> {
    let mut b = head();
    b.extend([0, 1]);
    b.extend(op);
    Fragment::decode(&b)
}

/// A fragment whose predicate is `expr`, and no operator.
fn with_predicate(expr: &[u8]) -> Result<Fragment, Error> {
    let mut b = head();
    b.push(1);
    b.extend(expr);
    b.push(0);
    Fragment::decode(&b)
}

/// The example of document 05 section 4, byte for byte.
#[test]
fn golden_fragment() {
    let f = Fragment {
        scan: Scan {
            table: "trade".into(),
            columns: vec![0, 3],
            ranges: vec![KeyRange {
                lower: Bound::Included(vec![(Type::U64, Value::U64(7))]),
                upper: Bound::Unbounded,
            }],
            time: (0, u64::MAX),
            predicate: Some(Expr::Binary(BinOp::Gt, col(1), u64_(0))),
        },
        operators: vec![Operator::PartialAggregate {
            keys: vec![Expr::Column(0)],
            aggregates: vec![Aggregate::Count, Aggregate::Sum(Expr::Column(1))],
            groups: 1000,
        }],
    };
    #[rustfmt::skip]
    let bytes = [
        0x01, 0x00, 0x05, 0x74, 0x72, 0x61, 0x64, 0x65,     // version 1, "trade"
        0x02, 0x00, 0x00, 0x00, 0x03, 0x00,                 // columns 0, 3
        0x01, 0x00, 0x01, 0x01, 0x00, 0x03, 7, 0, 0, 0, 0, 0, 0, 0, 0x00, // [u64 7, ..)
        0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // time
        0x01, 0x15, 0x19, 0x01, 0x00, 0x00, 0x03, 0, 0, 0, 0, 0, 0, 0, 0, // where col 1 > u64 0
        0x01, 0x01, 0x01, 0x00, 0x19, 0x00, 0x00,           // 1 operator: aggregate by col 0
        0x02, 0x00, 0x00, 0x01, 0x19, 0x01, 0x00,           // count, sum(col 1)
        0xe8, 0x03, 0x00, 0x00,                             // 1000 groups
    ];
    assert_eq!(f.encode().as_deref(), Ok(&bytes[..]));
    assert_eq!(Fragment::decode(&bytes), Ok(f));
}

/// Operator opcodes with the smallest operands, and the element each decodes to.
const OPERATORS: [(&[u8], &str); 4] = [
    (&[0, 0, 0], "Project("),
    (&[1, 0, 0, 0, 0, 0, 0, 0, 0], "PartialAggregate {"),
    (&[2, 0, 0, 0, 0, 0, 0], "TopN {"),
    (&[3, 0, 0, 0, 0, 0, 0, 0, 0], "Limit("),
];

/// Aggregate opcodes with the smallest operands.
const AGGREGATES: [(&[u8], &str); 4] = [
    (&[0], "Count"),
    (&[1, 25, 0, 0], "Sum("),
    (&[2, 25, 0, 0], "Min("),
    (&[3, 25, 0, 0], "Max("),
];

/// Expression opcodes with the smallest operands. The program IR's
/// parameter, local, field, `get`, `commit_ts` and `match` (1 to 5, 10) are
/// not fragment elements.
const EXPRESSIONS: [(&[u8], &str); 20] = [
    (&[0, 0, 7], "Literal("),
    (&[6, 25, 0, 0], "Neg("),
    (&[7, 25, 0, 0], "Not("),
    (&[8, 25, 0, 0], "IsNull("),
    (&[9, 25, 0, 0, 25, 0, 0], "UnwrapOr("),
    (&[11, 25, 0, 0, 3], "Cast("),
    (&[12, 25, 0, 0, 25, 0, 0], "Binary(Add,"),
    (&[13, 25, 0, 0, 25, 0, 0], "Binary(Sub,"),
    (&[14, 25, 0, 0, 25, 0, 0], "Binary(Mul,"),
    (&[15, 25, 0, 0, 25, 0, 0], "Binary(Div,"),
    (&[16, 25, 0, 0, 25, 0, 0], "Binary(Rem,"),
    (&[17, 25, 0, 0, 25, 0, 0], "Binary(Eq,"),
    (&[18, 25, 0, 0, 25, 0, 0], "Binary(Ne,"),
    (&[19, 25, 0, 0, 25, 0, 0], "Binary(Lt,"),
    (&[20, 25, 0, 0, 25, 0, 0], "Binary(Le,"),
    (&[21, 25, 0, 0, 25, 0, 0], "Binary(Gt,"),
    (&[22, 25, 0, 0, 25, 0, 0], "Binary(Ge,"),
    (&[23, 25, 0, 0, 25, 0, 0], "Binary(And,"),
    (&[24, 25, 0, 0, 25, 0, 0], "Binary(Or,"),
    (&[25, 0, 0], "Column("),
];

/// Every opcode value at an operator, an aggregate and an expression either
/// is one of the elements of document 05 section 4 or is rejected as
/// unknown, and no element is a join, a subquery, a sort, a write or a read
/// of another table: they have no encoding.
#[test]
fn every_opcode_is_a_listed_element_or_unknown() {
    let forbidden = [
        "join", "query", "sort", "order", "insert", "upsert", "update", "delete", "write", "get",
        "table", "param", "local", "match", "commit",
    ];
    for (_, element) in OPERATORS.iter().chain(&AGGREGATES).chain(&EXPRESSIONS) {
        let lower = element.to_lowercase();
        assert!(!forbidden.iter().any(|f| lower.contains(f)), "{element}");
    }
    for op in 0..=u8::MAX {
        match OPERATORS.iter().find(|(b, _)| b[0] == op) {
            Some((bytes, element)) => {
                let f = with_operator(bytes).unwrap_or_else(|e| panic!("operator {op}: {e:?}"));
                assert!(format!("{:?}", f.operators[0]).starts_with(element), "{op}");
                assert_eq!(
                    f.encode().as_deref(),
                    Ok(&[&head()[..], &[0, 1], bytes].concat()[..])
                );
            }
            None => assert_eq!(
                with_operator(&[op]),
                Err(Error::UnknownOpcode),
                "operator {op}"
            ),
        }
        // An aggregate by nothing with one aggregate, then 0 groups.
        let aggregate = |a: &[u8]| with_operator(&[&[1, 0, 0, 1, 0], a, &[0, 0, 0, 0]].concat());
        match AGGREGATES.iter().find(|(b, _)| b[0] == op) {
            Some((bytes, element)) => {
                let f = aggregate(bytes).unwrap_or_else(|e| panic!("aggregate {op}: {e:?}"));
                let Operator::PartialAggregate { aggregates, .. } = &f.operators[0] else {
                    unreachable!()
                };
                assert!(format!("{:?}", aggregates[0]).starts_with(element), "{op}");
            }
            None => assert_eq!(
                aggregate(&[op]),
                Err(Error::UnknownOpcode),
                "aggregate {op}"
            ),
        }
        match EXPRESSIONS.iter().find(|(b, _)| b[0] == op) {
            Some((bytes, element)) => {
                let f = with_predicate(bytes).unwrap_or_else(|e| panic!("expression {op}: {e:?}"));
                let e = f.scan.predicate.as_ref().expect("a predicate");
                assert!(format!("{e:?}").starts_with(element), "{op}");
            }
            None => assert_eq!(
                with_predicate(&[op]),
                Err(Error::UnknownOpcode),
                "expression {op}"
            ),
        }
    }
}

/// A predicate `-(-(...(column 0)))` at depth `depth`.
fn nested(depth: u32) -> Fragment {
    let mut e = Expr::Column(0);
    for _ in 1..depth {
        e = Expr::Neg(Box::new(e));
    }
    Fragment {
        scan: Scan {
            predicate: Some(e),
            ..scan(vec![0])
        },
        operators: vec![],
    }
}

#[test]
fn depth_is_limited() {
    let max = u32::try_from(FORMAT.program_depth_max).unwrap();
    let ok = nested(max);
    let bytes = ok.encode().unwrap();
    assert_eq!(Fragment::decode(&bytes), Ok(ok));
    assert_eq!(nested(max + 1).encode(), Err(Error::TooDeep));
    // The same bytes with one more negation.
    let mut deeper = bytes[..bytes.len() - 4].to_vec();
    deeper.extend([6, 25, 0, 0, 0]);
    assert_eq!(Fragment::decode(&deeper), Err(Error::TooDeep));
}

/// A projection of `n - 1` columns: `n` elements with the operator.
fn elements(n: u64) -> Fragment {
    Fragment {
        scan: scan(vec![0]),
        operators: vec![Operator::Project((1..n).map(|_| Expr::Column(0)).collect())],
    }
}

#[test]
fn element_count_is_limited() {
    let ok = elements(FORMAT.program_nodes_max);
    let bytes = ok.encode().unwrap();
    assert_eq!(Fragment::decode(&bytes), Ok(ok));
    assert_eq!(
        elements(FORMAT.program_nodes_max + 1).encode(),
        Err(Error::TooManyNodes)
    );
    // One more column in the same projection.
    let mut more = bytes.clone();
    // The projection's count, before its columns of 3 bytes each.
    let n = u16::try_from(FORMAT.program_nodes_max - 1).unwrap();
    let at = bytes.len() - 3 * usize::from(n) - 2;
    assert_eq!(u16::from_le_bytes([more[at], more[at + 1]]), n);
    more[at..at + 2].copy_from_slice(&(n + 1).to_le_bytes());
    more.extend([25, 0, 0]);
    assert_eq!(Fragment::decode(&more), Err(Error::TooManyNodes));
}

/// A fragment whose predicate is a string literal of `len` bytes.
fn with_string(len: usize) -> Fragment {
    let s = "a".repeat(len);
    let max = u32::try_from(len).unwrap();
    Fragment {
        scan: Scan {
            predicate: Some(Expr::Literal(Box::new((
                Type::String { max },
                Value::String(s),
            )))),
            ..scan(vec![0])
        },
        operators: vec![],
    }
}

#[test]
fn input_is_at_most_one_message() {
    let len = usize::try_from(FORMAT.message_size_max).unwrap();
    let fixed = with_string(0).encode().unwrap().len();
    let ok = with_string(len - fixed);
    let bytes = ok.encode().unwrap();
    assert_eq!(bytes.len(), len);
    assert_eq!(Fragment::decode(&bytes), Ok(ok));
    assert_eq!(with_string(len - fixed + 1).encode(), Err(Error::TooLarge));
    let mut over = bytes;
    over.push(0);
    assert_eq!(Fragment::decode(&over), Err(Error::TooLarge));
}

#[test]
fn names_and_operators_are_limited() {
    let named = |len| Fragment {
        scan: Scan {
            table: "x".repeat(len),
            ..scan(vec![0])
        },
        operators: vec![],
    };
    let ok = named(usize::from(u8::MAX));
    assert_eq!(Fragment::decode(&ok.encode().unwrap()), Ok(ok));
    assert_eq!(
        named(usize::from(u8::MAX) + 1).encode(),
        Err(Error::Malformed)
    );
    let limits = |n| Fragment {
        scan: scan(vec![0]),
        operators: vec![Operator::Limit(1); n],
    };
    let ok = limits(usize::from(u8::MAX));
    assert_eq!(Fragment::decode(&ok.encode().unwrap()), Ok(ok));
    assert_eq!(
        limits(usize::from(u8::MAX) + 1).encode(),
        Err(Error::Malformed)
    );
}

#[test]
fn other_versions_are_rejected() {
    let mut b = head();
    b.extend([0, 0]);
    assert!(Fragment::decode(&b).is_ok());
    for v in [0, FRAGMENT_VERSION + 1, u16::MAX] {
        b[..2].copy_from_slice(&v.to_le_bytes());
        assert_eq!(Fragment::decode(&b), Err(Error::UnsupportedVersion));
    }
}

#[test]
fn tags_out_of_range_and_trailing_bytes_are_malformed() {
    let mut b = head();
    b.extend([2]);
    assert_eq!(
        Fragment::decode(&b),
        Err(Error::Malformed),
        "predicate flag"
    );
    assert_eq!(
        with_operator(&[2, 1, 0, 2, 25, 0, 0, 1, 0, 0, 0]),
        Err(Error::Malformed),
        "order"
    );
    let mut range = head()[..12].to_vec();
    range.extend([1, 0, 3]);
    assert_eq!(Fragment::decode(&range), Err(Error::Malformed), "bound");
    let mut trailing = head();
    trailing.extend([0, 0, 0]);
    assert_eq!(Fragment::decode(&trailing), Err(Error::Malformed));
    for n in 0..head().len() {
        assert_eq!(Fragment::decode(&head()[..n]), Err(Error::Malformed), "{n}");
    }
}

/// Literals are those of the program IR: a NaN must be canonical, a
/// Nullable flag 0 or 1, a value within its type.
#[test]
fn non_canonical_and_invalid_literals_are_rejected() {
    let literal = |b: &[u8]| with_predicate(&[&[0], b].concat());
    // f64 (tag 14) NaN with other bits.
    assert_eq!(
        literal(&[&[14][..], &0x7ff8_0000_0000_0001u64.to_le_bytes()].concat()),
        Err(Error::InvalidLiteral)
    );
    // Nullable(u8): flag 2.
    let nullable_u8 = [&[26][..], &[0]].concat();
    assert!(literal(&[&nullable_u8[..], &[1, 5]].concat()).is_ok());
    assert_eq!(
        literal(&[&nullable_u8[..], &[2, 5]].concat()),
        Err(Error::InvalidLiteral)
    );
    let string = |v: Value| {
        let f = Fragment {
            scan: Scan {
                predicate: Some(Expr::Literal(Box::new((Type::String { max: 2 }, v)))),
                ..scan(vec![0])
            },
            operators: vec![],
        };
        f.encode()
    };
    assert!(string(Value::String("ab".into())).is_ok());
    assert_eq!(
        string(Value::String("abc".into())),
        Err(Error::InvalidLiteral)
    );
}

/// Order tags and the zero meaning "absent" round-trip.
#[test]
fn every_element_round_trips() {
    let f = Fragment {
        scan: Scan {
            ranges: vec![KeyRange {
                lower: Bound::Excluded(vec![(Type::U64, Value::U64(1))]),
                upper: Bound::Included(vec![(Type::U64, Value::U64(9)), (Type::U8, Value::U8(3))]),
            }],
            time: (5, 6),
            ..scan(vec![0, 1])
        },
        operators: vec![
            Operator::Project(vec![
                Expr::UnwrapOr(col(0), u64_(1)),
                Expr::Cast(col(1), Type::F64),
                Expr::Not(Box::new(Expr::IsNull(col(0)))),
            ]),
            Operator::TopN {
                order: vec![
                    (Expr::Column(0), Order::Desc),
                    (Expr::Column(1), Order::Asc),
                ],
                n: 0,
            },
            Operator::Limit(u64::MAX),
            Operator::PartialAggregate {
                keys: vec![],
                aggregates: vec![
                    Aggregate::Min(Expr::Neg(col(0))),
                    Aggregate::Max(Expr::Column(1)),
                ],
                groups: 0,
            },
        ],
    };
    let bytes = f.encode().unwrap();
    assert_eq!(Fragment::decode(&bytes), Ok(f));
}
