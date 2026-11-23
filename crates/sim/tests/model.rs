//! The reference model (T1.5.3): unit tests of each operation, the coverage
//! of the workload generator, and the harness catching deliberately wrong
//! engines. Seeds start at `GRANITE_SEED` when it is set.

use granite_protocol::Error;
use granite_schema::Catalog;
use granite_sim::model::harness::Harness;
use granite_sim::model::workload::Workload;
use granite_sim::model::{Duplicate, Engine, Model, Op, Outcome, Reject, Row, TableId};
use granite_types::Value;
use std::collections::BTreeSet;

/// Every column type the generator can break, a descending key column,
/// floats, a nullable column, a versioned table and a two-column partition key.
const SCHEMA: &str = "
schema version 1

table group by_region partition by (region Enum8(eu, us, asia)) range

table event immutable layout row in by_region
  key (region, name desc, seq)
  region Enum8(eu, us, asia),
  name String(4),
  seq Decimal(9, 2),
  note Nullable(Bytes(3)),
  score f64,
  flag bool,
  at Timestamp(6)

table total versioned layout columnar in by_region
  key (region)
  region Enum8(eu, us, asia),
  sum Decimal(38, 18)

table group by_user partition by (user_id u64, shard u8) hash

table entry immutable layout row in by_user
  key (user_id, shard, id)
  user_id u64,
  shard u8,
  id Id128,
  amount Decimal(40, 2),
  tag LowCardinality(String(8))
";

fn compile(name: &str, text: &str) -> Catalog {
    let bytes = granite_schemac::compile(name, text.as_bytes()).expect("compiles");
    Catalog::decode(&bytes).expect("decodes")
}

fn catalog() -> Catalog {
    compile("model.schema", SCHEMA)
}

fn seed(test: &str) -> u64 {
    let seed = std::env::var("GRANITE_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    println!("{test}: GRANITE_SEED={seed}");
    seed
}

const EVENT: TableId = 0;
const TOTAL: TableId = 1;

fn event(region: u8, name: &str, seq: i32, score: f64) -> Vec<Value> {
    vec![
        Value::Enum8(region),
        Value::String(name.into()),
        Value::Decimal32(seq),
        Value::Null,
        Value::F64(score),
        Value::Bool(false),
        Value::Timestamp(0),
    ]
}

fn key(region: u8, name: &str, seq: i32) -> Vec<Value> {
    vec![
        Value::Enum8(region),
        Value::String(name.into()),
        Value::Decimal32(seq),
    ]
}

/// The model, its next timestamp, and a shorthand to apply one op.
struct Run(Model, u64);

impl Run {
    fn new() -> Self {
        Run(Model::new(&catalog()), 0)
    }

    fn op(&mut self, op: Op) -> Outcome {
        self.1 += 1;
        self.0.apply(self.1, &op)
    }

    fn insert(&mut self, table: TableId, rows: Vec<Vec<Value>>) -> Outcome {
        self.op(Op::Insert { table, rows })
    }

    fn scan(&mut self, start: Vec<Value>, end: Vec<Value>, limit: u32) -> Outcome {
        let columns = vec![1, 2];
        self.op(Op::Scan {
            table: EVENT,
            start,
            end,
            limit,
            columns,
        })
    }

    fn content(&mut self) -> Vec<(Vec<Value>, u64)> {
        (self.0.content(EVENT).into_iter())
            .map(|r| (r.values, r.ts))
            .collect()
    }
}

fn inserted(o: Outcome) -> Vec<(u32, Duplicate)> {
    match o {
        Outcome::Inserted(failed) => failed,
        o => panic!("not an insert result: {o:?}"),
    }
}

fn rejected(o: Outcome) -> Reject {
    match o {
        Outcome::Rejected(r) => r,
        o => panic!("not rejected: {o:?}"),
    }
}

fn scanned(o: Outcome) -> Vec<Vec<Value>> {
    match o {
        Outcome::Scanned(rows) => rows,
        o => panic!("not a scan result: {o:?}"),
    }
}

// Insert --------------------------------------------------------------------

#[test]
fn insert_stores_new_rows_with_their_timestamp() {
    let mut run = Run::new();
    let (a, b) = (event(0, "a", 1, 1.0), event(0, "b", 1, 1.0));
    assert_eq!(inserted(run.insert(EVENT, vec![a.clone(), b.clone()])), []);
    // "b" sorts first: the name is descending.
    assert_eq!(run.content(), [(b, 1), (a, 1)]);
}

#[test]
fn insert_reports_duplicates_across_batches_and_keeps_the_stored_row() {
    let mut run = Run::new();
    let a = event(0, "a", 1, 1.0);
    run.insert(EVENT, vec![a.clone()]);
    let changed = event(0, "a", 1, 2.0);
    assert_eq!(
        inserted(run.insert(EVENT, vec![a.clone(), changed])),
        [
            (0, Duplicate::ExistsIdentical),
            (1, Duplicate::ExistsDifferent)
        ]
    );
    assert_eq!(run.content(), [(a, 1)]);
}

#[test]
fn insert_reports_duplicates_within_a_batch() {
    let mut run = Run::new();
    let (a, b) = (event(1, "a", 1, 1.0), event(1, "b", 1, 1.0));
    let changed = event(1, "a", 1, 2.0);
    let rows = vec![a.clone(), b.clone(), a.clone(), changed];
    assert_eq!(
        inserted(run.insert(EVENT, rows)),
        [
            (2, Duplicate::ExistsIdentical),
            (3, Duplicate::ExistsDifferent)
        ]
    );
    assert_eq!(run.content(), [(b, 1), (a, 1)]);
}

#[test]
fn insert_compares_floats_by_their_stored_bits() {
    let mut run = Run::new();
    run.insert(
        EVENT,
        vec![event(0, "n", 1, f64::NAN), event(0, "z", 1, 0.0)],
    );
    let other_nan = f64::from_bits(0xfff8_0000_0000_0001);
    assert_eq!(
        inserted(run.insert(
            EVENT,
            vec![event(0, "n", 1, other_nan), event(0, "z", 1, -0.0)]
        )),
        [
            (0, Duplicate::ExistsIdentical),
            (1, Duplicate::ExistsDifferent)
        ]
    );
}

#[test]
fn insert_rejects_the_whole_batch_for_one_invalid_value() {
    let mut run = Run::new();
    let ok = event(0, "a", 1, 1.0);
    let mut long = event(0, "b", 1, 1.0);
    long[1] = Value::String("abcde".into());
    let mut region = event(0, "c", 1, 1.0);
    region[0] = Value::Enum8(3);
    let mut seq = event(0, "d", 1, 1.0);
    seq[2] = Value::Decimal32(1_000_000_000);
    for (bad, error) in [
        (long, Error::TooLong),
        (region, Error::UnknownEnumValue),
        (seq, Error::InvalidValue),
    ] {
        let o = run.insert(EVENT, vec![ok.clone(), bad]);
        assert_eq!(rejected(o), Reject::Invalid(error));
    }
    assert_eq!(run.content(), []);
}

#[test]
fn insert_rejects_unknown_and_versioned_tables() {
    let mut run = Run::new();
    assert_eq!(rejected(run.insert(9, vec![])), Reject::UnknownTable);
    let total = vec![Value::Enum8(0), Value::Decimal128(0)];
    assert_eq!(
        rejected(run.insert(TOTAL, vec![total])),
        Reject::NotImmutable
    );
}

// Get -----------------------------------------------------------------------

#[test]
fn get_returns_the_requested_columns_of_each_key_found() {
    let mut run = Run::new();
    run.insert(EVENT, vec![event(0, "a", 1, 1.5)]);
    let o = run.op(Op::Get {
        table: EVENT,
        keys: vec![key(0, "b", 1), key(0, "a", 1), key(0, "a", 1)],
        columns: vec![4, 0, 4],
    });
    let Outcome::Got(rows) = o else {
        panic!("{o:?}")
    };
    let row = vec![Value::F64(1.5), Value::Enum8(0), Value::F64(1.5)];
    assert_eq!(rows, [None, Some(row.clone()), Some(row)]);
    let o = run.op(Op::Get {
        table: EVENT,
        keys: vec![key(0, "a", 1)],
        columns: vec![],
    });
    assert!(matches!(o, Outcome::Got(rows) if rows == [Some(vec![])]));
}

#[test]
fn get_rejects_unknown_columns_and_invalid_keys() {
    let mut run = Run::new();
    let get = |keys, columns| Op::Get {
        table: EVENT,
        keys,
        columns,
    };
    let o = run.op(get(vec![key(0, "a", 1)], vec![0, 7]));
    assert_eq!(rejected(o), Reject::UnknownColumn);
    let o = run.op(get(vec![key(0, "a", 1), key(0, "toolong", 1)], vec![0]));
    assert_eq!(rejected(o), Reject::Invalid(Error::TooLong));
    let o = run.op(Op::Get {
        table: 5,
        keys: vec![],
        columns: vec![],
    });
    assert_eq!(rejected(o), Reject::UnknownTable);
}

// Scan ----------------------------------------------------------------------

fn names(rows: Vec<Vec<Value>>) -> Vec<(String, i32)> {
    (rows.into_iter())
        .map(|r| match &r[..] {
            [Value::String(n), Value::Decimal32(s)] => (n.clone(), *s),
            r => panic!("{r:?}"),
        })
        .collect()
}

fn scan_fixture() -> Run {
    let mut run = Run::new();
    let rows = [
        (0, "a", 2),
        (0, "a", 1),
        (0, "b", 3),
        (0, "b", 1),
        (1, "a", 1),
    ]
    .map(|(r, n, s)| event(r, n, s, 0.0))
    .to_vec();
    run.insert(EVENT, rows);
    run
}

fn s(n: &str, seq: i32) -> (String, i32) {
    (n.into(), seq)
}

#[test]
fn scan_returns_a_partition_in_key_order() {
    let mut run = scan_fixture();
    let eu = vec![Value::Enum8(0)];
    let rows = scanned(run.scan(eu.clone(), eu, u32::MAX));
    assert_eq!(names(rows), [s("b", 1), s("b", 3), s("a", 1), s("a", 2)]);
}

#[test]
fn scan_bounds_are_inclusive_prefixes() {
    let mut run = scan_fixture();
    let k = |n: &str| vec![Value::Enum8(0), Value::String(n.into())];
    let a = scanned(run.scan(k("a"), k("a"), u32::MAX));
    assert_eq!(names(a), [s("a", 1), s("a", 2)]);
    let mut from = k("b");
    from.push(Value::Decimal32(2));
    let b = scanned(run.scan(from, k("b"), u32::MAX));
    assert_eq!(names(b), [s("b", 3)]);
    let both = scanned(run.scan(k("b"), k("a"), u32::MAX));
    assert_eq!(both.len(), 4);
}

#[test]
fn scan_of_an_empty_range_or_with_limit_zero_is_empty() {
    let mut run = scan_fixture();
    let k = |n: &str| vec![Value::Enum8(0), Value::String(n.into())];
    // "a" sorts after "b": the name is descending.
    assert_eq!(
        scanned(run.scan(k("a"), k("b"), u32::MAX)),
        Vec::<Vec<Value>>::new()
    );
    let asia = vec![Value::Enum8(2)];
    assert_eq!(scanned(run.scan(asia.clone(), asia, u32::MAX)).len(), 0);
    assert_eq!(scanned(run.scan(k("b"), k("a"), 0)).len(), 0);
}

#[test]
fn scan_returns_exactly_the_first_limit_rows() {
    let mut run = scan_fixture();
    let eu = vec![Value::Enum8(0)];
    for limit in 1..=5 {
        let rows = names(scanned(run.scan(eu.clone(), eu.clone(), limit)));
        let all = [s("b", 1), s("b", 3), s("a", 1), s("a", 2)];
        assert_eq!(rows, all[..all.len().min(limit as usize)]);
    }
}

#[test]
fn scan_rejects_ranges_across_partitions_and_unknown_columns() {
    let mut run = scan_fixture();
    let (eu, us) = (vec![Value::Enum8(0)], vec![Value::Enum8(1)]);
    assert_eq!(
        rejected(run.scan(eu.clone(), us, 9)),
        Reject::CrossPartition
    );
    assert_eq!(
        rejected(run.scan(vec![], eu.clone(), 9)),
        Reject::CrossPartition
    );
    let bad = vec![Value::Enum8(4)];
    assert_eq!(
        rejected(run.scan(eu.clone(), bad, 9)),
        Reject::Invalid(Error::UnknownEnumValue)
    );
    let o = run.op(Op::Scan {
        table: EVENT,
        start: eu.clone(),
        end: eu,
        limit: 9,
        columns: vec![1, 70],
    });
    assert_eq!(rejected(o), Reject::UnknownColumn);
}

// Workload generator ----------------------------------------------------------

/// The result codes of one outcome, by name.
fn codes(op: &Op, outcome: &Outcome) -> Vec<String> {
    match outcome {
        Outcome::Inserted(failed) => {
            let Op::Insert { rows, .. } = op else {
                unreachable!()
            };
            let mut codes: Vec<String> = failed.iter().map(|(_, d)| format!("{d:?}")).collect();
            if failed.len() < rows.len() {
                codes.push("Ok".into());
            }
            codes
        }
        Outcome::Got(rows) => (rows.iter())
            .map(|r| if r.is_some() { "Found" } else { "Absent" }.into())
            .collect(),
        Outcome::Scanned(rows) if rows.is_empty() => vec!["ScannedNone".into()],
        Outcome::Scanned(_) => vec!["ScannedRows".into()],
        Outcome::Rejected(r) => vec![format!("{r:?}")],
    }
}

/// How many seeds of 10,000 operations each the coverage test runs.
const COVERAGE_SEEDS: u64 = 20;

fn coverage(catalog: &Catalog, test: &str) -> BTreeSet<String> {
    let first = seed(test);
    let mut always: Option<BTreeSet<String>> = None;
    for seed in first..first + COVERAGE_SEEDS {
        let (mut model, mut workload) = (Model::new(catalog), Workload::new(catalog, seed));
        let mut seen = BTreeSet::new();
        for ts in 1..=10_000 {
            let op = workload.next_op();
            seen.extend(codes(&op, &model.apply(ts, &op)));
        }
        always = Some(match always {
            None => seen,
            Some(a) => a.intersection(&seen).cloned().collect(),
        });
    }
    always.expect("at least one seed")
}

const EVERY_CODE: [&str; 14] = [
    "Ok",
    "ExistsIdentical",
    "ExistsDifferent",
    "Found",
    "Absent",
    "ScannedRows",
    "ScannedNone",
    "UnknownTable",
    "NotImmutable",
    "UnknownColumn",
    "CrossPartition",
    "Invalid(TooLong)",
    "Invalid(UnknownEnumValue)",
    "Invalid(InvalidValue)",
];

#[test]
fn the_workload_produces_every_result_code_in_10000_operations() {
    let seen = coverage(&catalog(), "the_workload_produces_every_result_code");
    let every: BTreeSet<String> = EVERY_CODE.iter().map(|c| c.to_string()).collect();
    assert_eq!(seen, every, "codes produced by every seed");
}

#[test]
fn the_workload_on_the_exchange_schema_produces_every_code_its_types_allow() {
    let exchange = compile(
        "exchange.schema",
        include_str!("../../../schemas/exchange.schema"),
    );
    let seen = coverage(&exchange, "the_workload_on_the_exchange_schema");
    // The exchange schema has no String or Bytes column to make too long.
    let every: BTreeSet<String> = (EVERY_CODE.iter())
        .filter(|c| **c != "Invalid(TooLong)")
        .map(|c| c.to_string())
        .collect();
    assert_eq!(seen, every, "codes produced by every seed");
}

#[test]
fn the_workload_is_a_function_of_its_seed() {
    let catalog = catalog();
    let ops = |seed| {
        let mut w = Workload::new(&catalog, seed);
        format!("{:?}", (0..1_000).map(|_| w.next_op()).collect::<Vec<_>>())
    };
    assert_eq!(ops(7), ops(7));
    assert_ne!(ops(7), ops(8));
}

// Harness ---------------------------------------------------------------------

/// The model with one deliberate defect: the toy engine that shows the
/// harness catches each injected bug of T1.5.3 before the engine exists.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Defect {
    None,
    /// The content loses a row, as a compaction that drops one would.
    LostRow,
    /// A retried insert is reported as `exists_different`.
    WrongDuplicate,
    /// A scan leaves out its last row.
    ScanSkipsLastRow,
}

struct Toy {
    model: Model,
    defect: Defect,
}

impl Engine for Toy {
    fn apply(&mut self, ts: u64, op: &Op) -> Outcome {
        let mut outcome = self.model.apply(ts, op);
        match (self.defect, &mut outcome) {
            (Defect::WrongDuplicate, Outcome::Inserted(failed)) => failed
                .iter_mut()
                .for_each(|(_, d)| *d = Duplicate::ExistsDifferent),
            (Defect::ScanSkipsLastRow, Outcome::Scanned(rows)) => {
                rows.pop();
            }
            _ => {}
        }
        outcome
    }

    fn content(&mut self, table: TableId) -> Vec<Row> {
        let mut rows = self.model.content(table);
        if self.defect == Defect::LostRow && rows.len() > 1 {
            rows.remove(rows.len() / 2);
        }
        rows
    }
}

/// Runs `ops` operations of one seed through the harness, checking the
/// content every 100; the first difference, if any.
fn run_harness(defect: Defect, seed: u64, ops: u64) -> Result<(), String> {
    let catalog = catalog();
    let toy = Toy {
        model: Model::new(&catalog),
        defect,
    };
    let (mut harness, mut workload) = (Harness::new(&catalog, toy), Workload::new(&catalog, seed));
    for i in 1..=ops {
        harness.apply(&workload.next_op())?;
        if i % 100 == 0 {
            harness.check_content()?;
        }
    }
    harness.check_content()
}

#[test]
fn the_harness_finds_no_difference_between_two_models() {
    let first = seed("the_harness_finds_no_difference");
    for seed in first..first + 20 {
        run_harness(Defect::None, seed, 10_000).unwrap();
    }
}

#[test]
fn the_harness_catches_each_injected_defect() {
    let first = seed("the_harness_catches_each_injected_defect");
    for defect in [
        Defect::LostRow,
        Defect::WrongDuplicate,
        Defect::ScanSkipsLastRow,
    ] {
        for seed in first..first + 20 {
            let found = run_harness(defect, seed, 10_000);
            assert!(found.is_err(), "{defect:?} not caught with seed {seed}");
        }
    }
}
