//! Fifty programs over the test catalog, each with its accesses classified
//! by hand (document 03 section 5.2): the validator's key provenance must
//! match exactly. For every program whose accesses are all static, the
//! shard set computed from the report and the parameters alone equals the
//! partitions an invocation that commits touches, on random parameters and
//! random state; an invocation that fails touches a subset.

mod support;

use granite_txn::{AccessKind, BinOp, Expr, Program, Provenance, Stmt};
use granite_types::{Type, Value};
use std::ops::Bound::{self, Included, Unbounded};
use support::*;

/// Parameters of most programs: users a and b, an amount, an entry id or asset.
const A: u8 = 0;
const B: u8 = 1;
const AMOUNT: u8 = 2;
const ID: u8 = 3;

fn std(body: Vec<Stmt>) -> Program {
    program(&[Type::U64, Type::U64, DEC, Type::U32], body)
}

fn kind0() -> Expr {
    lit(KIND, Value::Enum8(0))
}

fn entry(user: Expr) -> Stmt {
    insert("entry", vec![user, p(ID), p(AMOUNT), kind0()])
}

fn account(user: Expr) -> Expr {
    get("account", vec![user])
}

/// The user of an account row that may be missing, else `default`.
fn user_of(row: Expr, local: u16, default: Expr) -> Expr {
    matching(row, default, field(l(local), 0))
}

fn ret(e: Expr) -> Stmt {
    Stmt::Return(e)
}

fn gt0(e: Expr) -> Expr {
    bin(BinOp::Gt, e, dec(0))
}

fn none() -> Bound<Expr> {
    Unbounded
}

/// The programs and their accesses in the order of the report: a letter for
/// the kind (g get, s scan, i insert, u upsert, p update, d delete) and S
/// for a static key, D for a dependent one.
fn corpus() -> Vec<(&'static str, Program, &'static str)> {
    let nullable_user = || program(&[null(Type::U64), Type::U64], vec![]);
    let with = |mut p: Program, body: Vec<Stmt>| {
        p.body = body;
        p
    };
    vec![
        ("exists", std(vec![ret(exists(account(p(A))))]), "gS"),
        (
            "transfer",
            std(vec![
                Stmt::Let(account(p(A))),
                require(matching(
                    l(0),
                    bool_(false),
                    bin(BinOp::Ge, field(l(1), 1), p(AMOUNT)),
                )),
                require(exists(account(p(B)))),
                entry(p(A)),
            ]),
            "gS gS iS",
        ),
        ("insert", std(vec![entry(p(A))]), "iS"),
        (
            "upsert",
            std(vec![upsert(
                "account",
                vec![p(A), p(AMOUNT), lit(null(S8), Value::Null), bool_(true)],
            )]),
            "uS",
        ),
        (
            "update",
            std(vec![update("account", vec![p(A)], vec![(1, p(AMOUNT))])]),
            "pS",
        ),
        (
            "delete",
            std(vec![delete("holding", vec![p(A), p(ID)])]),
            "dS",
        ),
        ("settle", std(vec![entry(p(A)), entry(p(B))]), "iS iS"),
        (
            "update the account a read names",
            std(vec![
                Stmt::Let(account(p(A))),
                update(
                    "account",
                    vec![user_of(l(0), 1, p(B))],
                    vec![(3, bool_(true))],
                ),
            ]),
            "gS pD",
        ),
        (
            "insert per scanned row",
            std(vec![for_(
                "entry",
                vec![p(A)],
                none(),
                none(),
                3,
                vec![insert(
                    "holding",
                    vec![field(l(0), 0), field(l(0), 1), i64_(0)],
                )],
            )]),
            "sS iD",
        ),
        (
            "static key inside a scan",
            std(vec![for_(
                "entry",
                vec![p(A)],
                none(),
                none(),
                3,
                vec![delete("holding", vec![p(B), p(ID)])],
            )]),
            "sS dD",
        ),
        (
            "branch on a parameter",
            std(vec![if_(
                gt0(p(AMOUNT)),
                vec![entry(p(A))],
                vec![entry(p(B))],
            )]),
            "iS iS",
        ),
        (
            "branch on a read",
            std(vec![if_(exists(account(p(A))), vec![entry(p(A))], vec![])]),
            "gS iD",
        ),
        (
            "require before a write",
            std(vec![require(exists(account(p(A)))), entry(p(A))]),
            "gS iS",
        ),
        (
            "key from a let of parameters",
            std(vec![
                Stmt::Let(bin(BinOp::Add, p(A), u64_(1))),
                ret(exists(account(l(0)))),
            ]),
            "gS",
        ),
        (
            "key from commit_ts",
            std(vec![ret(exists(account(Expr::CommitTs)))]),
            "gD",
        ),
        (
            "commit_ts outside the key",
            std(vec![update(
                "account",
                vec![p(A)],
                vec![(3, bin(BinOp::Gt, Expr::CommitTs, u64_(0)))],
            )]),
            "pS",
        ),
        (
            "match on a parameter",
            with(
                nullable_user(),
                vec![ret(matching(p(0), bool_(false), exists(account(l(0)))))],
            ),
            "gS",
        ),
        (
            "key from a matched row",
            std(vec![ret(matching(
                account(p(A)),
                bool_(false),
                exists(account(field(l(0), 0))),
            ))]),
            "gS gD",
        ),
        (
            "scan of a whole key",
            std(vec![for_("asset", vec![p(ID)], none(), none(), 1, vec![])]),
            "sS",
        ),
        (
            "scan with a lower bound",
            std(vec![for_(
                "quote",
                vec![p(ID)],
                Included(lit(Type::U16, Value::U16(0))),
                none(),
                2,
                vec![],
            )]),
            "sS",
        ),
        (
            "scan prefix from a read",
            std(vec![
                Stmt::Let(get("asset", vec![p(ID)])),
                for_(
                    "quote",
                    vec![matching(l(0), p(ID), field(l(2), 0))],
                    none(),
                    none(),
                    2,
                    vec![],
                ),
            ]),
            "gS sD",
        ),
        (
            "update of a composite key",
            std(vec![update(
                "holding",
                vec![p(A), p(ID)],
                vec![(2, i64_(7))],
            )]),
            "pS",
        ),
        (
            "return in both branches",
            std(vec![if_(
                bin(BinOp::Eq, p(A), p(B)),
                vec![ret(u64_(0))],
                vec![entry(p(A)), ret(u64_(1))],
            )]),
            "iS",
        ),
        (
            "read under a parameter, write under the read",
            std(vec![if_(
                gt0(p(AMOUNT)),
                vec![if_(
                    exists(account(p(B))),
                    vec![delete("holding", vec![p(A), p(ID)])],
                    vec![],
                )],
                vec![],
            )]),
            "gS dD",
        ),
        (
            "read in a non-key column",
            std(vec![insert(
                "entry",
                vec![
                    p(A),
                    p(ID),
                    matching(account(p(A)), dec(0), field(l(0), 1)),
                    kind0(),
                ],
            )]),
            "gS iS",
        ),
        (
            "read in a key column",
            std(vec![insert(
                "entry",
                vec![user_of(account(p(A)), 0, p(A)), p(ID), p(AMOUNT), kind0()],
            )]),
            "gS iD",
        ),
        (
            "update per scanned row",
            std(vec![for_(
                "holding",
                vec![p(A)],
                none(),
                none(),
                2,
                vec![update(
                    "account",
                    vec![field(l(0), 0)],
                    vec![(3, bool_(true))],
                )],
            )]),
            "sS pD",
        ),
        (
            "scan with a parameter bound",
            std(vec![for_(
                "holding",
                vec![p(A)],
                Included(p(ID)),
                none(),
                2,
                vec![],
            )]),
            "sS",
        ),
        (
            "scan bound from a read",
            std(vec![for_(
                "holding",
                vec![p(A)],
                Included(matching(get("asset", vec![p(ID)]), u32_(0), field(l(1), 0))),
                none(),
                2,
                vec![],
            )]),
            "gS sD",
        ),
        (
            "two reads in an and",
            std(vec![ret(bin(
                BinOp::And,
                exists(account(p(A))),
                exists(account(p(B))),
            ))]),
            "gS gS",
        ),
        (
            "key from unwrap_or of a parameter",
            with(
                nullable_user(),
                vec![ret(exists(account(Expr::UnwrapOr(
                    Box::new(p(0)),
                    Box::new(p(1)),
                ))))],
            ),
            "gS",
        ),
        (
            "two lets of reads",
            std(vec![
                Stmt::Let(account(p(A))),
                Stmt::Let(account(p(B))),
                ret(bin(
                    BinOp::And,
                    Expr::IsNull(Box::new(l(0))),
                    Expr::IsNull(Box::new(l(1))),
                )),
            ]),
            "gS gS",
        ),
        (
            "upsert when a parameter is present",
            with(
                nullable_user(),
                vec![if_(
                    Expr::IsNull(Box::new(p(0))),
                    vec![],
                    vec![upsert(
                        "account",
                        vec![
                            Expr::UnwrapOr(Box::new(p(0)), Box::new(u64_(0))),
                            dec(0),
                            lit(null(S8), Value::Null),
                            bool_(false),
                        ],
                    )],
                )],
            ),
            "uS",
        ),
        (
            "delete per scanned asset",
            std(vec![for_(
                "asset",
                vec![p(ID)],
                none(),
                none(),
                1,
                vec![delete("account", vec![p(A)])],
            )]),
            "sS dD",
        ),
        (
            "write in the else of a read",
            std(vec![if_(
                exists(account(p(A))),
                vec![],
                vec![insert(
                    "account",
                    vec![p(A), dec(0), lit(null(S8), Value::Null), bool_(true)],
                )],
            )]),
            "gS iD",
        ),
        (
            "key from arithmetic on parameters",
            std(vec![ret(exists(get(
                "holding",
                vec![
                    bin(BinOp::Mul, p(A), u64_(2)),
                    bin(BinOp::Add, p(ID), u32_(1)),
                ],
            )))]),
            "gS",
        ),
        (
            "key from a cast",
            std(vec![ret(exists(get(
                "holding",
                vec![p(A), Expr::Cast(Box::new(p(AMOUNT)), Type::U32)],
            )))]),
            "gS",
        ),
        ("no accesses, commit_ts", std(vec![ret(Expr::CommitTs)]), ""),
        (
            "no accesses",
            std(vec![ret(bin(BinOp::Add, p(AMOUNT), dec(1)))]),
            "",
        ),
        (
            "insert a quote",
            std(vec![insert(
                "quote",
                vec![
                    p(ID),
                    lit(Type::U16, Value::U16(0)),
                    Expr::Cast(Box::new(Expr::CommitTs), Type::I64),
                ],
            )]),
            "iS",
        ),
        (
            "quote key from a read",
            std(vec![insert(
                "quote",
                vec![
                    matching(get("asset", vec![p(ID)]), p(ID), field(l(0), 0)),
                    lit(Type::U16, Value::U16(0)),
                    i64_(0),
                ],
            )]),
            "gS iD",
        ),
        (
            "scan under a parameter",
            std(vec![if_(
                bin(BinOp::Gt, p(ID), u32_(1)),
                vec![for_(
                    "quote",
                    vec![p(ID)],
                    none(),
                    none(),
                    2,
                    vec![insert(
                        "holding",
                        vec![p(A), field(l(0), 0), field(l(0), 2)],
                    )],
                )],
                vec![],
            )]),
            "sS iD",
        ),
        (
            "require on a read, write elsewhere",
            std(vec![
                require(exists(account(p(A)))),
                update("account", vec![p(B)], vec![(1, p(AMOUNT))]),
            ]),
            "gS pS",
        ),
        (
            "write under a field of a read",
            std(vec![
                Stmt::Let(account(p(A))),
                if_(
                    matching(l(0), bool_(false), field(l(1), 3)),
                    vec![update("account", vec![p(A)], vec![(3, bool_(false))])],
                    vec![],
                ),
            ]),
            "gS pD",
        ),
        (
            "read in the null branch of a read",
            std(vec![ret(matching(
                account(p(A)),
                exists(account(p(B))),
                bool_(false),
            ))]),
            "gS gD",
        ),
        (
            "reads in both branches of a parameter match",
            with(
                nullable_user(),
                vec![ret(matching(
                    p(0),
                    exists(account(p(1))),
                    exists(account(l(0))),
                ))],
            ),
            "gS gS",
        ),
        (
            "update value from a read",
            std(vec![update(
                "account",
                vec![p(A)],
                vec![(1, matching(account(p(B)), dec(0), field(l(0), 1)))],
            )]),
            "gS pS",
        ),
        (
            "delete key from a read",
            std(vec![delete(
                "holding",
                vec![user_of(account(p(A)), 0, p(A)), p(ID)],
            )]),
            "gS dD",
        ),
        (
            "scan of no rows",
            std(vec![for_("entry", vec![p(A)], none(), none(), 0, vec![])]),
            "sS",
        ),
        (
            "nested scans",
            std(vec![for_(
                "entry",
                vec![p(A)],
                none(),
                none(),
                2,
                vec![for_(
                    "holding",
                    vec![field(l(0), 0)],
                    none(),
                    none(),
                    2,
                    vec![],
                )],
            )]),
            "sS sD",
        ),
    ]
}

fn letter(kind: AccessKind) -> char {
    match kind {
        AccessKind::Get => 'g',
        AccessKind::Scan => 's',
        AccessKind::Insert => 'i',
        AccessKind::Upsert => 'u',
        AccessKind::Update => 'p',
        AccessKind::Delete => 'd',
    }
}

#[test]
fn key_provenance_matches_the_hand_classification() {
    let catalog = catalog();
    let corpus = corpus();
    assert_eq!(corpus.len(), 50);
    for (name, p, expected) in &corpus {
        let report = p
            .validate(&catalog)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let got: Vec<String> = (report.accesses.iter())
            .map(|a| {
                let s = if a.provenance == Provenance::Static {
                    'S'
                } else {
                    'D'
                };
                format!("{}{s}", letter(a.kind))
            })
            .collect();
        assert_eq!(got.join(" "), *expected, "{name}");
    }
}

#[test]
fn the_shard_set_from_parameters_is_the_set_of_partitions_accessed() {
    let mut rng = seeded("the_shard_set_from_parameters_is_the_set_of_partitions_accessed");
    let catalog = catalog();
    let runs = if full() { 20_000 } else { 1_000 };
    let mut statics = 0;
    for (name, p, expected) in corpus() {
        if expected.contains('D') {
            continue;
        }
        statics += 1;
        let report = p.validate(&catalog).unwrap();
        let (mut committed, mut failed) = (0, 0);
        for _ in 0..runs {
            let params: Vec<Value> = p.params.iter().map(|q| value(&mut rng, &q.ty)).collect();
            let mut state = state(&mut rng, &catalog);
            let run = run(&catalog, &p, &report, &params, rng.next(), &mut state);
            let set = shard_set(&catalog, &p, &report, &params);
            match (run.result, set) {
                (Ok(_), Ok(set)) => {
                    committed += 1;
                    assert_eq!(set, run.partitions, "{name} {params:?}");
                }
                (Err(_), Ok(set)) => {
                    failed += 1;
                    assert!(run.partitions.is_subset(&set), "{name} {params:?}");
                }
                // Evaluating the keys failed (overflow): so did the invocation.
                (r, Err(_)) => assert!(r.is_err(), "{name} {params:?}"),
            }
        }
        println!("{name}: {committed} committed, {failed} failed");
        assert!(committed > 0, "{name} never committed");
    }
    println!("{statics} static-key programs");
    assert!(statics >= 25);
}

/// Writes the encoding of each corpus program into the directory named by
/// `GRANITE_SEEDS`, the seeds of the `program_validate` fuzz corpus:
/// `GRANITE_SEEDS=$PWD/fuzz/corpus/program_validate cargo test -p granite-txn --test corpus -- --ignored`.
#[test]
#[ignore = "writes files; run by hand to seed the fuzz corpus"]
fn write_fuzz_seeds() {
    let dir = std::path::PathBuf::from(std::env::var_os("GRANITE_SEEDS").expect("GRANITE_SEEDS"));
    std::fs::create_dir_all(&dir).unwrap();
    for (i, (_, p, _)) in corpus().iter().enumerate() {
        std::fs::write(dir.join(format!("seed-{i:02}")), p.encode().unwrap()).unwrap();
    }
}
