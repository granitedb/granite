//! The validator of document 03 section 5.2: each class of invalid program
//! is rejected with its own code at its node, valid programs give the
//! expected report, and the largest valid programs validate in under 10 ms.

mod support;

use granite_base::constants::FORMAT;
use granite_txn::{AccessKind, BinOp, Error, Expr, Invalid, Program, Provenance, Stmt};
use granite_types::{Type, Value};
use std::ops::Bound;
use support::*;

fn check(p: &Program) -> Result<granite_txn::Report<'_>, Invalid> {
    p.validate(&catalog())
}

#[track_caller]
fn rejects(p: Program, error: Error, node: u32) {
    assert_eq!(check(&p).map(|_| ()), Err(Invalid { error, node }), "{p:?}");
}

fn ret(e: Expr) -> Program {
    program(&[Type::U64, Type::I64], vec![Stmt::Return(e)])
}

fn null_i64() -> Expr {
    lit(null(Type::I64), Value::Null)
}

/// The `transfer` shape of document 03 section 5 over the test catalog:
/// parameters from, to, amount and the entry id.
fn transfer() -> Program {
    program(
        &[Type::U64, Type::U64, DEC, Type::U32],
        vec![
            Stmt::Let(get("account", vec![p(0)])),
            require(matching(
                l(0),
                bool_(false),
                bin(BinOp::Ge, field(l(1), 1), p(2)),
            )),
            require(exists(get("account", vec![p(1)]))),
            insert("entry", vec![p(0), p(3), p(2), lit(KIND, Value::Enum8(1))]),
            Stmt::Return(matching(
                l(0),
                dec(0),
                bin(BinOp::Sub, field(l(2), 1), p(2)),
            )),
        ],
    )
}

#[test]
fn a_valid_program_gives_its_report() {
    let t = transfer();
    let r = check(&t).unwrap();
    assert_eq!((r.reads, r.writes), (2, 1));
    assert_eq!(r.returns, Some(DEC));
    let kinds: Vec<_> = r
        .accesses
        .iter()
        .map(|a| (a.kind, a.group, a.table))
        .collect();
    assert_eq!(
        kinds,
        [
            (AccessKind::Get, 0, 0),
            (AccessKind::Get, 0, 0),
            (AccessKind::Insert, 0, 1)
        ]
    );
    assert!(
        r.accesses
            .iter()
            .all(|a| a.provenance == Provenance::Static)
    );
    let keys: Vec<_> = r.accesses.iter().map(|a| a.partition_key.clone()).collect();
    assert_eq!(keys, [vec![&p(0)], vec![&p(1)], vec![&p(0)]]);
}

#[test]
fn an_encoding_error_is_reported_for_the_whole_program() {
    let q = ret(lit(Type::U64, Value::Bool(true)));
    rejects(q, Error::InvalidLiteral, 0);
}

#[test]
fn unknown_table() {
    rejects(ret(exists(get("nope", vec![p(0)]))), Error::UnknownTable, 4);
    let q = program(&[], vec![delete("nope", vec![])]);
    rejects(q, Error::UnknownTable, 1);
}

#[test]
fn unknown_column() {
    let row = matching(get("account", vec![p(0)]), i64_(0), field(l(0), 4));
    rejects(ret(row), Error::UnknownColumn, 6);
    let q = program(
        &[Type::U64],
        vec![update("account", vec![p(0)], vec![(4, i64_(1))])],
    );
    rejects(q, Error::UnknownColumn, 1);
}

#[test]
fn unknown_param() {
    rejects(ret(p(2)), Error::UnknownParam, 2);
}

#[test]
fn unknown_local() {
    rejects(ret(l(0)), Error::UnknownLocal, 2);
    // A let is not in scope in its own expression.
    let q = program(&[], vec![Stmt::Let(l(0))]);
    rejects(q, Error::UnknownLocal, 2);
    // Nor after the block it is bound in.
    let q = program(
        &[],
        vec![
            if_(bool_(true), vec![Stmt::Let(u64_(1))], vec![]),
            Stmt::Return(l(0)),
        ],
    );
    rejects(q, Error::UnknownLocal, 6);
    // Nor is a match's local outside its present branch.
    let m = matching(null_i64(), l(0), i64_(1));
    rejects(ret(m), Error::UnknownLocal, 4);
}

#[test]
fn type_mismatch() {
    rejects(
        ret(bin(BinOp::Add, bool_(true), bool_(true))),
        Error::TypeMismatch,
        3,
    );
    rejects(
        ret(bin(BinOp::Add, i64_(1), bool_(true))),
        Error::TypeMismatch,
        4,
    );
    let q = program(&[Type::U64], vec![require(p(0))]);
    rejects(q, Error::TypeMismatch, 2);
    // A key value of another type: the account key is a u64.
    rejects(
        ret(exists(get("account", vec![bool_(true)]))),
        Error::TypeMismatch,
        5,
    );
    // Comparing floats, returning a row, is_null of a non-null value.
    let f = lit(Type::F64, Value::F64(1.0));
    rejects(ret(bin(BinOp::Eq, f.clone(), f)), Error::TypeMismatch, 3);
    rejects(ret(get("account", vec![p(0)])), Error::TypeMismatch, 2);
    rejects(ret(Expr::IsNull(Box::new(p(0)))), Error::TypeMismatch, 3);
    // A cast to a type that is not numeric.
    rejects(
        ret(Expr::Cast(Box::new(p(0)), Type::Bool)),
        Error::TypeMismatch,
        2,
    );
}

#[test]
fn implicit_cast() {
    rejects(ret(bin(BinOp::Add, p(0), u32_(1))), Error::ImplicitCast, 4);
    let d4 = Type::Decimal {
        precision: 18,
        scale: 4,
    };
    let e = bin(BinOp::Lt, dec(1), lit(d4.clone(), Value::Decimal64(1)));
    rejects(ret(e), Error::ImplicitCast, 4);
    // Inserting an i64 into a Decimal column.
    let q = program(
        &[Type::U64],
        vec![insert(
            "entry",
            vec![p(0), u32_(0), i64_(1), lit(KIND, Value::Enum8(0))],
        )],
    );
    rejects(q, Error::ImplicitCast, 4);
    // With the cast it is valid.
    let e = bin(
        BinOp::Lt,
        Expr::Cast(Box::new(dec(1)), d4.clone()),
        lit(d4, Value::Decimal64(1)),
    );
    assert!(check(&ret(e)).is_ok());
}

#[test]
fn null_in_arithmetic_or_a_condition() {
    rejects(
        ret(bin(BinOp::Add, null_i64(), p(1))),
        Error::NullableOperand,
        3,
    );
    rejects(
        ret(bin(BinOp::Add, p(1), null_i64())),
        Error::NullableOperand,
        4,
    );
    let q = program(&[], vec![require(lit(null(Type::Bool), Value::Null))]);
    rejects(q, Error::NullableOperand, 2);
    // A field of a row that may not exist.
    rejects(
        ret(field(get("account", vec![p(0)]), 1)),
        Error::NullableOperand,
        3,
    );
    // unwrap_or and match make it valid.
    assert!(
        check(&ret(bin(
            BinOp::Add,
            Expr::UnwrapOr(Box::new(null_i64()), Box::new(i64_(0))),
            p(1)
        )))
        .is_ok()
    );
    assert!(
        check(&ret(matching(
            null_i64(),
            i64_(0),
            bin(BinOp::Add, l(0), p(1))
        )))
        .is_ok()
    );
}

#[test]
fn wrong_arity() {
    let q = program(&[Type::U64], vec![insert("account", vec![p(0)])]);
    rejects(q, Error::WrongArity, 1);
    rejects(ret(exists(get("entry", vec![p(0)]))), Error::WrongArity, 4);
    // A scan with a bound after the whole key.
    let s = for_(
        "account",
        vec![p(0)],
        Bound::Included(u64_(0)),
        Bound::Unbounded,
        1,
        vec![],
    );
    rejects(program(&[Type::U64], vec![s]), Error::WrongArity, 1);
}

#[test]
fn writes_to_an_immutable_table() {
    let key = vec![p(0), u32_(0)];
    rejects(
        program(&[Type::U64], vec![delete("entry", key.clone())]),
        Error::ImmutableTable,
        1,
    );
    let set = vec![(2, dec(1))];
    rejects(
        program(&[Type::U64], vec![update("entry", key, set)]),
        Error::ImmutableTable,
        1,
    );
    let row = vec![p(0), u32_(0), dec(1), lit(KIND, Value::Enum8(0))];
    rejects(
        program(&[Type::U64], vec![upsert("entry", row.clone())]),
        Error::ImmutableTable,
        1,
    );
    assert!(check(&program(&[Type::U64], vec![insert("entry", row)])).is_ok());
}

#[test]
fn updates_of_key_columns_or_one_column_twice() {
    let q = program(
        &[Type::U64],
        vec![update("account", vec![p(0)], vec![(0, u64_(1))])],
    );
    rejects(q, Error::KeyColumnUpdated, 1);
    let q = program(
        &[Type::U64],
        vec![update(
            "account",
            vec![p(0)],
            vec![(3, bool_(true)), (3, bool_(false))],
        )],
    );
    rejects(q, Error::ColumnSetTwice, 1);
}

#[test]
fn scan_across_partitions() {
    let s = for_(
        "entry",
        vec![],
        Bound::Unbounded,
        Bound::Unbounded,
        1,
        vec![],
    );
    rejects(program(&[], vec![s]), Error::ScanAcrossPartitions, 1);
}

/// A scan of `entry` in partition `p0` of at most `limit` rows.
fn scan(limit: u16, body: Vec<Stmt>) -> Program {
    let s = for_(
        "entry",
        vec![p(0)],
        Bound::Unbounded,
        Bound::Unbounded,
        limit,
        body,
    );
    program(&[Type::U64], vec![s])
}

#[test]
fn bound_over_the_access_limit() {
    let limit = FORMAT.program_accesses_max;
    let max = u16::try_from(limit).unwrap();
    let n = usize::from(max);
    let node = |stmts: usize| u32::try_from(stmts * 5).unwrap();
    assert_eq!(check(&scan(max, vec![])).unwrap().reads, limit);
    rejects(scan(max + 1, vec![]), Error::TooManyAccesses, 1);
    // Half the limit in rows, each deleting one row.
    let w = || delete("holding", vec![field(l(0), 0), field(l(0), 1)]);
    let s = scan(max / 2, vec![w()]);
    let r = check(&s).unwrap();
    assert_eq!((r.reads, r.writes), (limit / 2, limit / 2));
    rejects(scan(max / 2 + 1, vec![w()]), Error::TooManyAccesses, 1);
    // An update reads and writes: a third of the limit in rows fits.
    let u = || update("account", vec![field(l(0), 0)], vec![(3, bool_(true))]);
    assert_eq!(
        check(&scan(max / 3, vec![u()])).unwrap().reads,
        limit / 3 * 2
    );
    rejects(scan(max / 3 + 1, vec![u()]), Error::TooManyAccesses, 1);
    // The most expensive branch counts, not the sum of both. A read is 5 nodes.
    let get = || require(exists(get("account", vec![p(0)])));
    let branches = |a: usize, b: usize| {
        program(
            &[Type::U64],
            vec![if_(bool_(true), vec![get(); a], vec![get(); b])],
        )
    };
    assert_eq!(check(&branches(n, 200)).unwrap().reads, limit);
    rejects(
        branches(200, n + 1),
        Error::TooManyAccesses,
        2 + node(200) + node(n) + 1,
    );
    // Statements in sequence add up, also at the top of the program: one
    // read over the limit fails at its statement.
    let reads = |k| program(&[Type::U64], vec![get(); k]);
    assert_eq!(check(&reads(n)).unwrap().reads, limit);
    rejects(reads(n + 1), Error::TooManyAccesses, node(n) + 1);
    // Nested scans multiply, and overflow nothing however large.
    let deep = (0..10).fold(vec![], |body, _| {
        vec![for_(
            "entry",
            vec![p(0)],
            Bound::Unbounded,
            Bound::Unbounded,
            u16::MAX,
            body,
        )]
    });
    rejects(program(&[Type::U64], deep), Error::TooManyAccesses, 1);
}

#[test]
fn missing_return() {
    let q = program(
        &[],
        vec![if_(bool_(true), vec![Stmt::Return(u64_(1))], vec![])],
    );
    rejects(q, Error::MissingReturn, 0);
    // A return inside a scan does not return on every path.
    let s = for_(
        "entry",
        vec![u64_(0)],
        Bound::Unbounded,
        Bound::Unbounded,
        1,
        vec![Stmt::Return(u64_(1))],
    );
    rejects(program(&[], vec![s]), Error::MissingReturn, 0);
    let both = if_(
        bool_(true),
        vec![Stmt::Return(u64_(1))],
        vec![Stmt::Return(u64_(2))],
    );
    assert_eq!(
        check(&program(&[], vec![both])).unwrap().returns,
        Some(Type::U64)
    );
    // Returns of T and Nullable(T) give Nullable(T); of two types, an error.
    let mixed = if_(
        bool_(true),
        vec![Stmt::Return(i64_(1))],
        vec![Stmt::Return(null_i64())],
    );
    assert_eq!(
        check(&program(&[], vec![mixed])).unwrap().returns,
        Some(null(Type::I64))
    );
    let two = if_(
        bool_(true),
        vec![Stmt::Return(i64_(1))],
        vec![Stmt::Return(bool_(true))],
    );
    rejects(program(&[], vec![two]), Error::TypeMismatch, 6);
}

#[test]
fn code_after_return() {
    let q = program(&[], vec![Stmt::Return(u64_(1)), Stmt::Return(u64_(2))]);
    rejects(q, Error::CodeAfterReturn, 3);
    let q = program(
        &[],
        vec![
            if_(bool_(true), vec![Stmt::Return(u64_(1))], vec![]),
            Stmt::Return(u64_(2)),
        ],
    );
    rejects(q, Error::CodeAfterReturn, 5);
}

/// Shapes of the largest valid program, `program_nodes_max` nodes each.
fn largest() -> Vec<Program> {
    let nodes = usize::try_from(FORMAT.program_nodes_max).unwrap();
    // A chain of lets over the parameter, then 250 static reads at its end.
    let mut lets = vec![Stmt::Let(p(0))];
    for n in 0..1500 {
        lets.push(Stmt::Let(bin(BinOp::Add, l(n), p(0))));
    }
    lets.extend((0..250).map(|_| require(exists(get("account", vec![l(1500)])))));
    // Conditions over the parameters, each a guard of a static access.
    let mut guarded = Vec::new();
    for _ in 0..200 {
        let c = (0..13).fold(bin(BinOp::Lt, p(0), u64_(1)), |c, _| {
            bin(BinOp::And, c, bool_(true))
        });
        let read = require(exists(get("account", vec![p(0)])));
        guarded.push(if_(c, vec![read], vec![]));
    }
    // Matches nested in the value of a match, around a large expression:
    // validation must not copy the value once per enclosing match.
    fn sum(depth: u32) -> Expr {
        match depth {
            0 => i64_(1),
            d => bin(BinOp::Add, sum(d - 1), sum(d - 1)),
        }
    }
    let none = || lit(null(Type::I64), Value::Null);
    let nested = (0..48).fold(matching(none(), sum(11), none()), |e, _| {
        matching(e, none(), none())
    });
    let mut out = vec![
        program(&[Type::U64], lets),
        program(&[Type::U64], guarded),
        program(&[], vec![Stmt::Let(nested)]),
    ];
    for p in &mut out {
        // Pad to the limit with requires of 2 nodes, and one of 3 if odd.
        let pad = nodes - count(&p.body);
        let mut stmts = vec![require(bool_(true)); pad / 2];
        if pad % 2 == 1 {
            stmts[0] = require(Expr::Not(Box::new(bool_(true))));
        }
        p.body.splice(0..0, stmts);
    }
    out
}

/// Statements and expressions, as the node limit counts them.
fn count(stmts: &[Stmt]) -> usize {
    stmts.iter().map(nodes_stmt).sum()
}

fn nodes_expr(e: &Expr) -> usize {
    1 + match e {
        Expr::Literal(_) | Expr::Param(_) | Expr::Local(_) | Expr::CommitTs => 0,
        Expr::Field(a, _) | Expr::Neg(a) | Expr::Not(a) | Expr::IsNull(a) | Expr::Cast(a, _) => {
            nodes_expr(a)
        }
        Expr::Get { key, .. } => key.iter().map(nodes_expr).sum(),
        Expr::UnwrapOr(a, b) | Expr::Binary(_, a, b) => nodes_expr(a) + nodes_expr(b),
        Expr::Match {
            value,
            if_null,
            present,
        } => nodes_expr(value) + nodes_expr(if_null) + nodes_expr(present),
    }
}

fn nodes_stmt(s: &Stmt) -> usize {
    1 + match s {
        Stmt::Let(e) | Stmt::Require { cond: e, .. } | Stmt::Return(e) => nodes_expr(e),
        Stmt::If {
            cond,
            then,
            otherwise,
        } => nodes_expr(cond) + then.iter().chain(otherwise).map(nodes_stmt).sum::<usize>(),
        Stmt::Insert { row: es, .. }
        | Stmt::Upsert { row: es, .. }
        | Stmt::Delete { key: es, .. } => es.iter().map(nodes_expr).sum(),
        Stmt::Update { key, set, .. } => {
            key.iter().map(nodes_expr).sum::<usize>()
                + set.iter().map(|(_, e)| nodes_expr(e)).sum::<usize>()
        }
        Stmt::For { scan, body } => {
            let b = |b: &Bound<Expr>| match b {
                Bound::Included(e) | Bound::Excluded(e) => nodes_expr(e),
                Bound::Unbounded => 0,
            };
            scan.prefix.iter().map(nodes_expr).sum::<usize>()
                + b(&scan.lower)
                + b(&scan.upper)
                + body.iter().map(nodes_stmt).sum::<usize>()
        }
    }
}

#[test]
fn the_largest_valid_programs_validate_in_under_10_ms() {
    let catalog = catalog();
    for p in largest() {
        assert_eq!(
            count(&p.body),
            usize::try_from(FORMAT.program_nodes_max).unwrap()
        );
        let mut one_more = p.clone();
        one_more.body.push(require(bool_(true)));
        assert_eq!(
            one_more.validate(&catalog).unwrap_err().error,
            Error::TooManyNodes
        );
        let best = (0..5)
            .map(|_| {
                let start = std::time::Instant::now();
                p.validate(&catalog).expect("the largest program is valid");
                start.elapsed()
            })
            .min()
            .unwrap();
        println!("{} nodes validated in {best:?}", FORMAT.program_nodes_max);
        assert!(best < std::time::Duration::from_millis(10), "{best:?}");
    }
}
