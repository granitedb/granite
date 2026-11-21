//! Shared by the validator tests: a catalog, tree builders, random values, a
//! reference evaluator over an in-memory state, and a generator of random
//! programs.
//!
//! The evaluator shares no code with the validator. It carries the type of
//! every value at run time and checks it at every operation, so a program
//! the validator wrongly accepted shows up as a type error here, which
//! panics. Failures a valid program may have (a failed `require`, overflow,
//! a duplicate key, a missing row) are its `Err` results.

#![allow(dead_code, reason = "each test binary uses a different part")]

#[path = "../../../schema/tests/common/mod.rs"]
mod common;

pub use common::Rng;
use granite_schema::{Catalog, Column, Kind, Layout, Partitioning, Retention, Table, TableGroup};
use granite_txn::{BinOp, Expr, Local, Param, Program, Report, Scan, Stmt};
use granite_types::arith::{self, Op};
use granite_types::key::{Order, encode_key, encode_primary_key};
use granite_types::{Error, Type, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

pub fn seeded(test: &str) -> Rng {
    let seed = std::env::var("GRANITE_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5EED);
    println!("{test}: GRANITE_SEED={seed}");
    Rng(seed)
}

pub fn full() -> bool {
    std::env::var_os("GRANITE_FULL").is_some()
}

// Catalog -------------------------------------------------------------------

pub const DEC: Type = Type::Decimal {
    precision: 18,
    scale: 2,
};
pub const S8: Type = Type::String { max: 8 };
pub const KIND: Type = Type::Enum8 { max: 3 };

pub fn null(t: Type) -> Type {
    Type::Nullable(Box::new(t))
}

fn col(name: &str, ty: Type) -> Column {
    Column {
        name: name.into(),
        ty,
    }
}

fn table(name: &str, kind: Kind, columns: Vec<Column>, key: &[(u16, Order)]) -> Table {
    Table {
        name: name.into(),
        kind,
        columns,
        primary_key: key.to_vec(),
        layout: Some(Layout::Row),
        retention: Retention::Forever,
    }
}

/// Two groups: `users`, hash-partitioned by `user u64`, with `account`
/// (versioned), `entry` (immutable) and `holding` (versioned, a descending
/// key column); and `assets`, range-partitioned by `asset u32`, with `asset`
/// (versioned) and `quote` (immutable).
pub fn catalog() -> Catalog {
    use Order::{Asc, Desc};
    let c = Catalog {
        version: 1,
        groups: vec![
            TableGroup {
                name: "users".into(),
                partitioning: Partitioning::Hash,
                partition_key: vec![col("user", Type::U64)],
                tables: vec![
                    table(
                        "account",
                        Kind::Versioned,
                        vec![
                            col("user", Type::U64),
                            col("balance", DEC),
                            col("nick", null(S8)),
                            col("active", Type::Bool),
                        ],
                        &[(0, Asc)],
                    ),
                    table(
                        "entry",
                        Kind::Immutable,
                        vec![
                            col("user", Type::U64),
                            col("id", Type::U32),
                            col("amount", DEC),
                            col("kind", KIND),
                        ],
                        &[(0, Asc), (1, Asc)],
                    ),
                    table(
                        "holding",
                        Kind::Versioned,
                        vec![
                            col("user", Type::U64),
                            col("asset", Type::U32),
                            col("qty", Type::I64),
                        ],
                        &[(0, Asc), (1, Desc)],
                    ),
                ],
            },
            TableGroup {
                name: "assets".into(),
                partitioning: Partitioning::Range,
                partition_key: vec![col("asset", Type::U32)],
                tables: vec![
                    table(
                        "asset",
                        Kind::Versioned,
                        vec![
                            col("asset", Type::U32),
                            col("price", Type::I64),
                            col("name", Type::LowCardinality(Box::new(S8))),
                            col("ticker", null(Type::I64)),
                        ],
                        &[(0, Asc)],
                    ),
                    table(
                        "quote",
                        Kind::Immutable,
                        vec![
                            col("asset", Type::U32),
                            col("at", Type::U16),
                            col("px", Type::I64),
                        ],
                        &[(0, Asc), (1, Asc)],
                    ),
                ],
            },
        ],
    };
    c.validate().expect("the test catalog is valid");
    c
}

// Tree builders ---------------------------------------------------------------

pub fn p(i: u8) -> Expr {
    Expr::Param(i)
}
pub fn l(i: u16) -> Expr {
    Expr::Local(i)
}
pub fn lit(ty: Type, v: Value) -> Expr {
    Expr::Literal(Box::new((ty, v)))
}
pub fn u64_(v: u64) -> Expr {
    lit(Type::U64, Value::U64(v))
}
pub fn u32_(v: u32) -> Expr {
    lit(Type::U32, Value::U32(v))
}
pub fn i64_(v: i64) -> Expr {
    lit(Type::I64, Value::I64(v))
}
pub fn dec(m: i64) -> Expr {
    lit(DEC, Value::Decimal64(m))
}
pub fn bool_(v: bool) -> Expr {
    lit(Type::Bool, Value::Bool(v))
}
pub fn bin(op: BinOp, a: Expr, b: Expr) -> Expr {
    Expr::Binary(op, Box::new(a), Box::new(b))
}
pub fn get(table: &str, key: Vec<Expr>) -> Expr {
    Expr::Get {
        table: table.into(),
        key,
    }
}
pub fn field(row: Expr, column: u16) -> Expr {
    Expr::Field(Box::new(row), column)
}
pub fn exists(e: Expr) -> Expr {
    Expr::Not(Box::new(Expr::IsNull(Box::new(e))))
}
pub fn matching(value: Expr, if_null: Expr, present: Expr) -> Expr {
    Expr::Match {
        value: Box::new(value),
        if_null: Box::new(if_null),
        present: Box::new(present),
    }
}
pub fn insert(table: &str, row: Vec<Expr>) -> Stmt {
    Stmt::Insert {
        table: table.into(),
        row,
    }
}
pub fn upsert(table: &str, row: Vec<Expr>) -> Stmt {
    Stmt::Upsert {
        table: table.into(),
        row,
    }
}
pub fn update(table: &str, key: Vec<Expr>, set: Vec<(u16, Expr)>) -> Stmt {
    Stmt::Update {
        table: table.into(),
        key,
        set,
    }
}
pub fn delete(table: &str, key: Vec<Expr>) -> Stmt {
    Stmt::Delete {
        table: table.into(),
        key,
    }
}
pub fn require(cond: Expr) -> Stmt {
    Stmt::Require {
        cond,
        code: "E".into(),
    }
}
pub fn if_(cond: Expr, then: Vec<Stmt>, otherwise: Vec<Stmt>) -> Stmt {
    Stmt::If {
        cond,
        then,
        otherwise,
    }
}
pub fn for_(
    table: &str,
    prefix: Vec<Expr>,
    lower: Bound<Expr>,
    upper: Bound<Expr>,
    limit: u16,
    body: Vec<Stmt>,
) -> Stmt {
    Stmt::For {
        scan: Box::new(Scan {
            table: table.into(),
            prefix,
            lower,
            upper,
            limit,
        }),
        body,
    }
}
pub fn program(params: &[Type], body: Vec<Stmt>) -> Program {
    Program {
        name: "p".into(),
        params: (params.iter().enumerate())
            .map(|(i, ty)| Param {
                name: format!("a{i}"),
                ty: ty.clone(),
            })
            .collect(),
        body,
    }
}

// Types and values --------------------------------------------------------------

/// Without LowCardinality, which does not change the logical type.
pub fn logical(t: &Type) -> Type {
    match t {
        Type::LowCardinality(inner) => logical(inner),
        Type::Nullable(inner) => null(logical(inner)),
        t => t.clone(),
    }
}

/// Without Nullable either: the type a non-null value of `t` has.
fn base(t: &Type) -> Type {
    match logical(t) {
        Type::Nullable(inner) => *inner,
        t => t,
    }
}

fn is_nullable(t: &Type) -> bool {
    matches!(logical(t), Type::Nullable(_))
}

fn int(rng: &mut Rng) -> i128 {
    match rng.below(10) {
        0 => i128::MAX,
        1 => i128::MIN,
        2 => -(rng.below(4) as i128),
        _ => rng.below(4) as i128,
    }
}

/// A random value of one of the types the tests use: integers are mostly
/// 0 to 3, so that keys meet, and sometimes at the ends of their range.
pub fn value(rng: &mut Rng, ty: &Type) -> Value {
    let i = int(rng);
    match ty {
        Type::Nullable(_) if rng.below(3) == 0 => Value::Null,
        Type::Nullable(inner) | Type::LowCardinality(inner) => value(rng, inner),
        Type::U16 => Value::U16(i.clamp(0, u16::MAX.into()) as u16),
        Type::U32 => Value::U32(i.clamp(0, u32::MAX.into()) as u32),
        Type::U64 => Value::U64(i.clamp(0, u64::MAX.into()) as u64),
        Type::I64 => Value::I64(i.clamp(i64::MIN.into(), i64::MAX.into()) as i64),
        Type::Decimal { precision, .. } => {
            let top = 10i128.pow(u32::from(*precision)) - 1;
            Value::Decimal64(i.clamp(-top, top) as i64)
        }
        Type::Bool => Value::Bool(rng.below(2) == 0),
        Type::Enum8 { max } => Value::Enum8(rng.below(u64::from(*max) + 1) as u8),
        Type::String { max } => {
            let n = rng.below(u64::from(*max) + 1);
            Value::String(
                (0..n)
                    .map(|_| ['a', 'b', '\0'][rng.below(3) as usize])
                    .collect(),
            )
        }
        t => panic!("no values of {t:?} in these tests"),
    }
}

// The reference evaluator ---------------------------------------------------------

/// Rows by table, then by encoded primary key, so in key order.
pub type State = BTreeMap<(usize, usize), BTreeMap<Vec<u8>, Vec<Value>>>;

/// A random state: up to 16 rows per table, keys from a small domain.
pub fn state(rng: &mut Rng, catalog: &Catalog) -> State {
    let mut s = State::new();
    for (g, group) in catalog.groups.iter().enumerate() {
        for (t, tb) in group.tables.iter().enumerate() {
            let rows = s.entry((g, t)).or_default();
            for _ in 0..rng.below(17) {
                let row: Vec<Value> = tb.columns.iter().map(|c| value(rng, &c.ty)).collect();
                if let Ok(k) = primary_key(tb, &key_values(tb, &row)) {
                    rows.insert(k, row);
                }
            }
        }
    }
    s
}

fn key_values(tb: &Table, row: &[Value]) -> Vec<Value> {
    (tb.primary_key.iter())
        .map(|&(c, _)| row[usize::from(c)].clone())
        .collect()
}

fn key_columns(tb: &Table) -> Vec<(Type, Order)> {
    (tb.primary_key.iter())
        .map(|&(c, o)| (tb.columns[usize::from(c)].ty.clone(), o))
        .collect()
}

fn primary_key(tb: &Table, values: &[Value]) -> Result<Vec<u8>, Error> {
    encode_primary_key(&key_columns(tb), values)
}

/// A value at run time, with its type: `Val(T, v)` with T not Nullable, and
/// `v` possibly null; or a row of a table, possibly null.
#[derive(Clone, Debug, PartialEq)]
pub enum V {
    Val(Type, Value),
    Row(usize, usize, Option<Vec<Value>>),
}

/// Why an invocation of a valid program did not commit.
#[derive(Clone, Debug, PartialEq)]
pub enum Fail {
    Require(String),
    Arith(Error),
    Exists,
    NotFound,
}

/// The result of one invocation and what it touched.
#[derive(Debug)]
pub struct Run {
    pub result: Result<Option<V>, Fail>,
    pub reads: u64,
    pub writes: u64,
    /// Every partition an access touched: group, encoded partition key.
    pub partitions: BTreeSet<(usize, Vec<u8>)>,
}

#[track_caller]
fn type_error(what: &str) -> ! {
    panic!("type error at run time: {what}")
}

fn arith_fail(e: Error) -> Fail {
    if e == Error::Mismatch {
        type_error("an operand of another type");
    }
    Fail::Arith(e)
}

/// Runs `program`, which the validator accepted with `report`, on `params`
/// and `state`. A failed invocation leaves `state` unchanged.
pub fn run(
    catalog: &Catalog,
    program: &Program,
    report: &Report,
    params: &[Value],
    commit_ts: u64,
    state: &mut State,
) -> Run {
    let mut x = Exec::new(catalog, program, report, params, commit_ts);
    let before = state.clone();
    x.state = std::mem::take(state);
    let result = x.block(&program.body).inspect(|r| {
        if let Some(v) = r {
            x.returned(v);
        } else {
            assert_eq!(
                report.returns, None,
                "fell off the end of a returning program"
            );
        }
    });
    *state = if result.is_ok() {
        std::mem::take(&mut x.state)
    } else {
        before
    };
    Run {
        result,
        reads: x.reads,
        writes: x.writes,
        partitions: x.partitions,
    }
}

/// The partitions an invocation that commits touches, computed from the
/// report and the parameters alone, for a program whose accesses are all
/// static: each access whose guards hold, at its partition-key expressions.
pub fn shard_set(
    catalog: &Catalog,
    program: &Program,
    report: &Report,
    params: &[Value],
) -> Result<BTreeSet<(usize, Vec<u8>)>, Fail> {
    let mut x = Exec::new(catalog, program, report, params, 0);
    x.from_params = true;
    let mut set = BTreeSet::new();
    'access: for a in &report.accesses {
        // Outermost first: an inner condition may only be evaluable where
        // the outer ones hold.
        let mut chain = Vec::new();
        let mut guard = a.guard;
        while let Some(i) = guard {
            chain.push(&report.guards[i]);
            guard = report.guards[i].parent;
        }
        for g in chain.into_iter().rev() {
            let holds = if g.is_null {
                matches!(x.expr(g.cond)?, V::Val(_, Value::Null) | V::Row(_, _, None))
            } else {
                x.boolean(g.cond)?
            };
            if holds != g.holds {
                continue 'access;
            }
        }
        let values = (a.partition_key.iter())
            .map(|e| x.key_value(e))
            .collect::<Result<Vec<_>, _>>()?;
        set.insert((a.group, x.partition(a.group, &values)));
    }
    Ok(set)
}

struct Exec<'a> {
    catalog: &'a Catalog,
    report: &'a Report<'a>,
    params: Vec<V>,
    commit_ts: u64,
    /// The local number of each binding element, by its address.
    numbers: BTreeMap<usize, u16>,
    locals: Vec<Option<V>>,
    state: State,
    reads: u64,
    writes: u64,
    partitions: BTreeSet<(usize, Vec<u8>)>,
    /// Evaluating from the parameters alone: locals come from the report, and
    /// any read fails the test.
    from_params: bool,
}

/// Numbers the binding elements in the order of the encoding.
fn number(p: &Program) -> BTreeMap<usize, u16> {
    fn stmts(s: &[Stmt], m: &mut BTreeMap<usize, u16>) {
        s.iter().for_each(|s| stmt(s, m))
    }
    fn next(m: &mut BTreeMap<usize, u16>, at: usize) {
        let n = m.len() as u16;
        m.insert(at, n);
    }
    fn stmt(s: &Stmt, m: &mut BTreeMap<usize, u16>) {
        match s {
            Stmt::Let(e) => {
                next(m, s as *const Stmt as usize);
                expr(e, m)
            }
            Stmt::If {
                cond,
                then,
                otherwise,
            } => {
                expr(cond, m);
                stmts(then, m);
                stmts(otherwise, m)
            }
            Stmt::Require { cond, .. } | Stmt::Return(cond) => expr(cond, m),
            Stmt::Insert { row, .. } | Stmt::Upsert { row, .. } => {
                row.iter().for_each(|e| expr(e, m))
            }
            Stmt::Update { key, set, .. } => {
                key.iter().for_each(|e| expr(e, m));
                set.iter().for_each(|(_, e)| expr(e, m))
            }
            Stmt::Delete { key, .. } => key.iter().for_each(|e| expr(e, m)),
            Stmt::For { scan, body } => {
                next(m, s as *const Stmt as usize);
                scan.prefix.iter().for_each(|e| expr(e, m));
                for b in [&scan.lower, &scan.upper] {
                    if let Bound::Included(e) | Bound::Excluded(e) = b {
                        expr(e, m)
                    }
                }
                stmts(body, m)
            }
        }
    }
    fn expr(e: &Expr, m: &mut BTreeMap<usize, u16>) {
        match e {
            Expr::Literal(_) | Expr::Param(_) | Expr::Local(_) | Expr::CommitTs => {}
            Expr::Field(a, _)
            | Expr::Neg(a)
            | Expr::Not(a)
            | Expr::IsNull(a)
            | Expr::Cast(a, _) => expr(a, m),
            Expr::Get { key, .. } => key.iter().for_each(|e| expr(e, m)),
            Expr::UnwrapOr(a, b) | Expr::Binary(_, a, b) => {
                expr(a, m);
                expr(b, m)
            }
            Expr::Match {
                value,
                if_null,
                present,
            } => {
                next(m, e as *const Expr as usize);
                expr(value, m);
                expr(if_null, m);
                expr(present, m)
            }
        }
    }
    let mut m = BTreeMap::new();
    stmts(&p.body, &mut m);
    m
}

impl<'a> Exec<'a> {
    fn new(
        catalog: &'a Catalog,
        program: &Program,
        report: &'a Report,
        params: &[Value],
        commit_ts: u64,
    ) -> Self {
        program
            .check_params(params)
            .expect("parameters of the declared types");
        let params = (program.params.iter().zip(params))
            .map(|(p, v)| V::Val(base(&p.ty), v.clone()))
            .collect();
        let numbers = number(program);
        Exec {
            catalog,
            report,
            params,
            commit_ts,
            locals: vec![None; numbers.len()],
            numbers,
            state: State::new(),
            reads: 0,
            writes: 0,
            partitions: BTreeSet::new(),
            from_params: false,
        }
    }

    fn table(&self, name: &str) -> (usize, usize, &'a Table) {
        for (g, group) in self.catalog.groups.iter().enumerate() {
            for (t, tb) in group.tables.iter().enumerate() {
                if tb.name == name {
                    return (g, t, tb);
                }
            }
        }
        type_error("an unknown table")
    }

    fn partition(&self, g: usize, values: &[Value]) -> Vec<u8> {
        let columns: Vec<(Type, Order)> = (self.catalog.groups[g].partition_key.iter())
            .map(|c| (c.ty.clone(), Order::Asc))
            .collect();
        encode_key(&columns, &values[..columns.len()])
            .unwrap_or_else(|_| type_error("a partition key that does not encode"))
    }

    fn touch(&mut self, g: usize, key: &[Value]) {
        let part = self.partition(g, key);
        self.partitions.insert((g, part));
    }

    /// Binds a local by the address of its binding element.
    fn bind<T>(&mut self, at: &T, v: V) -> usize {
        let n = usize::from(self.numbers[&(at as *const T as usize)]);
        self.locals[n] = Some(v);
        n
    }

    fn returned(&self, v: &V) {
        let want = self
            .report
            .returns
            .as_ref()
            .expect("a return in a program that returns none");
        match v {
            V::Val(t, x) if *t == base(want) && (*x != Value::Null || is_nullable(want)) => {}
            _ => type_error("a return value of another type"),
        }
    }

    fn block(&mut self, stmts: &[Stmt]) -> Result<Option<V>, Fail> {
        for s in stmts {
            if let Some(v) = self.stmt(s)? {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    /// A value for a column of type `ty`.
    fn store(&self, v: V, ty: &Type) -> Value {
        match v {
            V::Val(t, x) if t == base(ty) && (x != Value::Null || is_nullable(ty)) => {
                ty.check(&x)
                    .unwrap_or_else(|_| type_error("a value outside its type"));
                x
            }
            _ => type_error("a value of another type for a column"),
        }
    }

    fn row_values(&mut self, tb: &Table, row: &[Expr]) -> Result<Vec<Value>, Fail> {
        if row.len() != tb.columns.len() {
            type_error("a row of another length");
        }
        let mut out = Vec::new();
        for (e, c) in row.iter().zip(&tb.columns) {
            let v = self.expr(e)?;
            out.push(self.store(v, &c.ty));
        }
        Ok(out)
    }

    fn key(&mut self, tb: &Table, key: &[Expr]) -> Result<Vec<Value>, Fail> {
        let mut out = Vec::new();
        for (e, &(c, _)) in key.iter().zip(&tb.primary_key) {
            let v = self.expr(e)?;
            if matches!(v, V::Val(_, Value::Null)) {
                type_error("a null key value");
            }
            out.push(self.store(v, &tb.columns[usize::from(c)].ty));
        }
        Ok(out)
    }

    fn encoded(tb: &Table, key: &[Value]) -> Result<Vec<u8>, Fail> {
        primary_key(tb, key).map_err(Fail::Arith)
    }

    fn stmt(&mut self, s: &Stmt) -> Result<Option<V>, Fail> {
        match s {
            Stmt::Let(e) => {
                let v = self.expr(e)?;
                self.bind(s, v);
            }
            Stmt::If {
                cond,
                then,
                otherwise,
            } => {
                return if self.boolean(cond)? {
                    self.block(then)
                } else {
                    self.block(otherwise)
                };
            }
            Stmt::Require { cond, code } => {
                if !self.boolean(cond)? {
                    return Err(Fail::Require(code.clone()));
                }
            }
            Stmt::Insert { table, row } | Stmt::Upsert { table, row } => {
                let (g, t, tb) = self.table(table);
                let row = self.row_values(tb, row)?;
                let key = key_values(tb, &row);
                self.writes += 1;
                self.touch(g, &key);
                let k = Self::encoded(tb, &key)?;
                let rows = self.state.entry((g, t)).or_default();
                let upsert = matches!(s, Stmt::Upsert { .. });
                match rows.get(&k) {
                    _ if upsert => {
                        assert_eq!(tb.kind, Kind::Versioned, "upsert of an immutable table")
                    }
                    None => {}
                    Some(old) if *old == row && tb.kind == Kind::Immutable => {}
                    Some(_) => return Err(Fail::Exists),
                }
                rows.insert(k, row);
            }
            Stmt::Update { table, key, set } => {
                let (g, t, tb) = self.table(table);
                assert_eq!(tb.kind, Kind::Versioned, "update of an immutable table");
                let key = self.key(tb, key)?;
                let mut new = Vec::new();
                for (c, e) in set {
                    let v = self.expr(e)?;
                    new.push((
                        usize::from(*c),
                        self.store(v, &tb.columns[usize::from(*c)].ty),
                    ));
                }
                self.reads += 1;
                self.writes += 1;
                self.touch(g, &key);
                let k = Self::encoded(tb, &key)?;
                let row =
                    (self.state.entry((g, t)).or_default().get_mut(&k)).ok_or(Fail::NotFound)?;
                for (c, v) in new {
                    assert!(tb.primary_key.iter().all(|&(k, _)| usize::from(k) != c));
                    row[c] = v;
                }
            }
            Stmt::Delete { table, key } => {
                let (g, t, tb) = self.table(table);
                assert_eq!(tb.kind, Kind::Versioned, "delete from an immutable table");
                let key = self.key(tb, key)?;
                self.writes += 1;
                self.touch(g, &key);
                let k = Self::encoded(tb, &key)?;
                self.state.entry((g, t)).or_default().remove(&k);
            }
            Stmt::For { scan, body } => {
                assert!(!self.from_params, "a scan evaluated from the parameters");
                let (g, t, tb) = self.table(&scan.table);
                let prefix = self.key(tb, &scan.prefix)?;
                let n = prefix.len();
                let mut bound = |b: &Bound<Expr>| -> Result<Bound<Value>, Fail> {
                    Ok(match b {
                        Bound::Unbounded => Bound::Unbounded,
                        Bound::Included(e) | Bound::Excluded(e) => {
                            let column = &tb.columns[usize::from(tb.primary_key[n].0)];
                            let v = self.expr(e)?;
                            let v = self.store(v, &column.ty);
                            if matches!(b, Bound::Included(_)) {
                                Bound::Included(v)
                            } else {
                                Bound::Excluded(v)
                            }
                        }
                    })
                };
                let (lower, upper) = (bound(&scan.lower)?, bound(&scan.upper)?);
                self.touch(g, &prefix);
                let next_ty = tb
                    .primary_key
                    .get(n)
                    .map(|&(c, _)| tb.columns[usize::from(c)].ty.clone());
                let order = |a: &Value, b: &Value| {
                    let ty = next_ty.clone().expect("a bound needs a next key column");
                    let enc = |v: &Value| {
                        encode_key(&[(ty.clone(), Order::Asc)], std::slice::from_ref(v)).unwrap()
                    };
                    enc(a).cmp(&enc(b))
                };
                let rows: Vec<Vec<Value>> = (self.state.entry((g, t)).or_default().values())
                    .filter(|row| {
                        let k = key_values(tb, row);
                        k[..n] == prefix[..]
                            && match &lower {
                                Bound::Unbounded => true,
                                Bound::Included(v) => order(&k[n], v).is_ge(),
                                Bound::Excluded(v) => order(&k[n], v).is_gt(),
                            }
                            && match &upper {
                                Bound::Unbounded => true,
                                Bound::Included(v) => order(&k[n], v).is_le(),
                                Bound::Excluded(v) => order(&k[n], v).is_lt(),
                            }
                    })
                    .take(usize::from(scan.limit))
                    .cloned()
                    .collect();
                for row in rows {
                    self.reads += 1;
                    self.bind(s, V::Row(g, t, Some(row)));
                    if let Some(v) = self.block(body)? {
                        return Ok(Some(v));
                    }
                }
            }
            Stmt::Return(e) => return Ok(Some(self.expr(e)?)),
        }
        Ok(None)
    }

    fn boolean(&mut self, e: &Expr) -> Result<bool, Fail> {
        match self.expr(e)? {
            V::Val(Type::Bool, Value::Bool(b)) => Ok(b),
            _ => type_error("a condition that is not a bool"),
        }
    }

    /// A non-null value and its type.
    fn operand(&mut self, e: &Expr) -> Result<(Type, Value), Fail> {
        match self.expr(e)? {
            V::Val(_, Value::Null) => type_error("null used as an operand"),
            V::Val(t, v) => Ok((t, v)),
            V::Row(..) => type_error("a row used as an operand"),
        }
    }

    fn key_value(&mut self, e: &Expr) -> Result<Value, Fail> {
        Ok(self.operand(e)?.1)
    }

    fn expr(&mut self, e: &Expr) -> Result<V, Fail> {
        Ok(match e {
            Expr::Literal(l) => V::Val(base(&l.0), l.1.clone()),
            Expr::Param(i) => self.params[usize::from(*i)].clone(),
            Expr::Local(i) if self.from_params => {
                let report = self.report;
                match &report.locals[usize::from(*i)] {
                    Local::Let(e) => self.expr(e)?,
                    Local::Match(e) => match self.expr(e)? {
                        V::Val(_, Value::Null) => type_error("a match local of a null value"),
                        v => v,
                    },
                    Local::Row => panic!("a row local evaluated from the parameters"),
                }
            }
            Expr::Local(i) => (self.locals[usize::from(*i)].clone())
                .unwrap_or_else(|| type_error("a local that is not bound")),
            Expr::CommitTs => {
                assert!(
                    !self.from_params,
                    "commit_ts() evaluated from the parameters"
                );
                V::Val(Type::U64, Value::U64(self.commit_ts))
            }
            Expr::Field(row, c) => match self.expr(row)? {
                V::Row(g, t, Some(values)) => {
                    let ty = &self.catalog.groups[g].tables[t].columns[usize::from(*c)].ty;
                    V::Val(base(ty), values[usize::from(*c)].clone())
                }
                _ => type_error("a field of a null row or of a value"),
            },
            Expr::Get { table, key } => {
                assert!(!self.from_params, "a get evaluated from the parameters");
                let (g, t, tb) = self.table(table);
                if key.len() != tb.primary_key.len() {
                    type_error("a key of another length");
                }
                let key = self.key(tb, key)?;
                self.reads += 1;
                self.touch(g, &key);
                let k = Self::encoded(tb, &key)?;
                V::Row(g, t, self.state.entry((g, t)).or_default().get(&k).cloned())
            }
            Expr::Neg(a) => {
                let (t, v) = self.operand(a)?;
                V::Val(t.clone(), arith::negate(&t, &v).map_err(arith_fail)?)
            }
            Expr::Not(a) => V::Val(Type::Bool, Value::Bool(!self.boolean(a)?)),
            Expr::IsNull(a) => {
                let null = match self.expr(a)? {
                    V::Val(_, v) => v == Value::Null,
                    V::Row(_, _, row) => row.is_none(),
                };
                V::Val(Type::Bool, Value::Bool(null))
            }
            Expr::UnwrapOr(a, b) => {
                let (a, b) = (self.expr(a)?, self.expr(b)?);
                match (a, b) {
                    (V::Val(s, Value::Null), V::Val(t, v)) if s == t && v != Value::Null => {
                        V::Val(t, v)
                    }
                    (V::Row(g, t, None), V::Row(h, u, Some(r))) if (g, t) == (h, u) => {
                        V::Row(h, u, Some(r))
                    }
                    (a @ (V::Val(..) | V::Row(_, _, Some(_))), b) if same_kind(&a, &b) => a,
                    _ => type_error("unwrap_or with a default of another type"),
                }
            }
            Expr::Match {
                value,
                if_null,
                present,
            } => match self.expr(value)? {
                V::Val(_, Value::Null) | V::Row(_, _, None) => self.expr(if_null)?,
                v => {
                    if !self.from_params {
                        self.bind(e, v);
                    }
                    self.expr(present)?
                }
            },
            Expr::Cast(a, to) => {
                let (t, v) = self.operand(a)?;
                let to = base(to);
                V::Val(to.clone(), arith::cast(&t, &v, &to).map_err(arith_fail)?)
            }
            Expr::Binary(op, a, b) => {
                if let BinOp::And | BinOp::Or = op {
                    let (a, b) = (self.boolean(a)?, self.boolean(b)?);
                    let r = if *op == BinOp::And { a && b } else { a || b };
                    return Ok(V::Val(Type::Bool, Value::Bool(r)));
                }
                let ((t, x), (u, y)) = (self.operand(a)?, self.operand(b)?);
                if t != u {
                    type_error("operands of two types");
                }
                let arith = |o| arith::binary(o, &t, &x, &y).map_err(arith_fail);
                let cmp = || {
                    if matches!(t, Type::F32 | Type::F64) {
                        type_error("a float comparison");
                    }
                    let enc = |v: &Value| {
                        encode_key(&[(t.clone(), Order::Asc)], std::slice::from_ref(v)).unwrap()
                    };
                    enc(&x).cmp(&enc(&y))
                };
                let b = |b| V::Val(Type::Bool, Value::Bool(b));
                match op {
                    BinOp::Add => V::Val(t.clone(), arith(Op::Add)?),
                    BinOp::Sub => V::Val(t.clone(), arith(Op::Sub)?),
                    BinOp::Mul => V::Val(t.clone(), arith(Op::Mul)?),
                    BinOp::Div => V::Val(t.clone(), arith(Op::Div)?),
                    BinOp::Rem => V::Val(t.clone(), arith(Op::Rem)?),
                    BinOp::Eq => b(cmp().is_eq()),
                    BinOp::Ne => b(cmp().is_ne()),
                    BinOp::Lt => b(cmp().is_lt()),
                    BinOp::Le => b(cmp().is_le()),
                    BinOp::Gt => b(cmp().is_gt()),
                    BinOp::Ge => b(cmp().is_ge()),
                    BinOp::And | BinOp::Or => unreachable!(),
                }
            }
        })
    }
}

/// The present value of `unwrap_or` and its default have one type.
fn same_kind(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Val(s, _), V::Val(t, v)) => s == t && *v != Value::Null,
        (V::Row(g, t, _), V::Row(h, u, Some(_))) => (g, t) == (h, u),
        _ => false,
    }
}

// The generator of random programs ------------------------------------------------

/// The types expressions are generated at, besides the catalog's.
fn types() -> Vec<Type> {
    vec![
        Type::U16,
        Type::U32,
        Type::U64,
        Type::I64,
        DEC,
        Type::Bool,
        S8,
        KIND,
        null(Type::I64),
        null(S8),
        null(Type::Bool),
    ]
}

#[derive(Clone, Debug, PartialEq)]
enum GTy {
    Val(Type),
    Row(usize, usize),
}

/// Builds random programs over a catalog, by type, in the order of the
/// encoding so that locals get the numbers the IR gives them. One
/// expression in `sloppy` is of a random type instead of the wanted one.
pub struct Gen<'a> {
    pub rng: &'a mut Rng,
    catalog: &'a Catalog,
    params: Vec<Type>,
    next: u16,
    scope: Vec<(u16, GTy)>,
    pub sloppy: u64,
}

impl<'a> Gen<'a> {
    pub fn new(rng: &'a mut Rng, catalog: &'a Catalog, sloppy: u64) -> Self {
        Gen {
            rng,
            catalog,
            params: Vec::new(),
            next: 0,
            scope: Vec::new(),
            sloppy,
        }
    }

    fn pick<T: Clone>(&mut self, items: &[T]) -> T {
        items[self.rng.below(items.len() as u64) as usize].clone()
    }

    fn tables(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for (g, group) in self.catalog.groups.iter().enumerate() {
            for t in 0..group.tables.len() {
                out.push((g, t));
            }
        }
        out
    }

    fn tb(&self, (g, t): (usize, usize)) -> &'a Table {
        &self.catalog.groups[g].tables[t]
    }

    pub fn program(&mut self) -> Program {
        self.next = 0;
        self.scope.clear();
        let pool = types();
        self.params = (0..self.rng.below(5)).map(|_| self.pick(&pool)).collect();
        let returns = (self.rng.below(2) == 0).then(|| self.pick(&pool));
        let depth = self.rng.below(4) as u32;
        let mut body = self.block(depth);
        if let Some(t) = returns {
            if self.rng.below(3) == 0 {
                let c = self.expr(&Type::Bool, 2);
                let mark = self.scope.len();
                let a = vec![Stmt::Return(self.expr(&t, 2))];
                self.scope.truncate(mark);
                let b = vec![Stmt::Return(self.expr(&t, 2))];
                body.push(if_(c, a, b));
            } else {
                body.push(Stmt::Return(self.expr(&t, 2)));
            }
        }
        program(&self.params.clone(), body)
    }

    fn block(&mut self, depth: u32) -> Vec<Stmt> {
        let mark = self.scope.len();
        let stmts = (0..self.rng.below(4)).map(|_| self.stmt(depth)).collect();
        self.scope.truncate(mark);
        stmts
    }

    fn local(&mut self) -> u16 {
        self.next += 1;
        self.next - 1
    }

    fn stmt(&mut self, depth: u32) -> Stmt {
        let d = depth.saturating_sub(1);
        let tables = self.tables();
        let versioned: Vec<_> = (tables.iter().copied())
            .filter(|&t| self.tb(t).kind == Kind::Versioned)
            .collect();
        match self.rng.below(if depth == 0 { 7 } else { 9 }) {
            0 => {
                let n = self.local();
                let ty = self.pick(&types());
                let e = self.expr(&ty, 2);
                self.scope.push((n, GTy::Val(logical(&ty))));
                Stmt::Let(e)
            }
            1 => require(self.expr(&Type::Bool, 2)),
            2 | 3 => {
                let t = self.pick(&tables);
                let row = self.row(t);
                if self.rng.below(2) == 0 && self.tb(t).kind == Kind::Versioned {
                    upsert(&self.tb(t).name, row)
                } else {
                    insert(&self.tb(t).name, row)
                }
            }
            4 => {
                let t = self.pick(&versioned);
                let tb = self.tb(t);
                let key = self.key(t, tb.primary_key.len());
                let free: Vec<u16> = (0..tb.columns.len() as u16)
                    .filter(|c| tb.primary_key.iter().all(|(k, _)| k != c))
                    .collect();
                let mut set = Vec::new();
                for c in free {
                    if self.rng.below(2) == 0 {
                        set.push((c, self.expr(&tb.columns[usize::from(c)].ty, 1)));
                    }
                }
                update(&tb.name, key, set)
            }
            5 => {
                let t = self.pick(&versioned);
                let tb = self.tb(t);
                delete(&tb.name, self.key(t, tb.primary_key.len()))
            }
            6 => {
                let t = self.pick(&tables);
                let r = self.get_row(t);
                require(exists(r))
            }
            7 => {
                let c = self.expr(&Type::Bool, 2);
                if_(c, self.block(d), self.block(d))
            }
            _ => {
                let n = self.local();
                let t = self.pick(&tables);
                let tb = self.tb(t);
                let parts = self.catalog.groups[t.0].partition_key.len();
                let len =
                    parts + self.rng.below((tb.primary_key.len() - parts + 1) as u64) as usize;
                let prefix = self.key(t, len);
                let (lower, upper) = if len < tb.primary_key.len() {
                    let ty = tb.columns[usize::from(tb.primary_key[len].0)].ty.clone();
                    (self.bound(&ty), self.bound(&ty))
                } else {
                    (Bound::Unbounded, Bound::Unbounded)
                };
                let limit = self.rng.below(4) as u16;
                let mark = self.scope.len();
                self.scope.push((n, GTy::Row(t.0, t.1)));
                let body = self.block(d);
                self.scope.truncate(mark);
                for_(&tb.name, prefix, lower, upper, limit, body)
            }
        }
    }

    fn bound(&mut self, ty: &Type) -> Bound<Expr> {
        match self.rng.below(3) {
            0 => Bound::Unbounded,
            1 => Bound::Included(self.expr(ty, 1)),
            _ => Bound::Excluded(self.expr(ty, 1)),
        }
    }

    fn row(&mut self, t: (usize, usize)) -> Vec<Expr> {
        let tb = self.tb(t);
        tb.columns.iter().map(|c| self.expr(&c.ty, 1)).collect()
    }

    fn key(&mut self, t: (usize, usize), n: usize) -> Vec<Expr> {
        let tb = self.tb(t);
        (tb.primary_key[..n].iter())
            .map(|&(c, _)| self.expr(&tb.columns[usize::from(c)].ty, 1))
            .collect()
    }

    fn get_row(&mut self, t: (usize, usize)) -> Expr {
        let tb = self.tb(t);
        get(&tb.name, self.key(t, tb.primary_key.len()))
    }

    /// An expression of type `ty`, nesting at most `depth` operators.
    fn expr(&mut self, ty: &Type, depth: u32) -> Expr {
        let ty = &logical(ty);
        if self.rng.below(self.sloppy) == 0 {
            let other = self.pick(&types());
            return self.expr_of(&other, depth);
        }
        self.expr_of(ty, depth)
    }

    fn expr_of(&mut self, ty: &Type, depth: u32) -> Expr {
        let d = depth.saturating_sub(1);
        let want = GTy::Val(ty.clone());
        let inner = match ty {
            Type::Nullable(i) => Some((**i).clone()),
            _ => None,
        };
        // Places a value of `ty` can come from.
        let mut from = Vec::new();
        for (i, p) in self.params.iter().enumerate() {
            let p = logical(p);
            if p == *ty || inner.as_ref() == Some(&p) {
                from.push(Expr::Param(i as u8));
            }
        }
        for (n, t) in &self.scope {
            if *t == want || inner.as_ref().is_some_and(|i| *t == GTy::Val(i.clone())) {
                from.push(Expr::Local(*n));
            }
            if let GTy::Row(g, tb) = t {
                for (c, column) in self.catalog.groups[*g].tables[*tb]
                    .columns
                    .iter()
                    .enumerate()
                {
                    if logical(&column.ty) == *ty {
                        from.push(field(Expr::Local(*n), c as u16));
                    }
                }
            }
        }
        let leaves = if from.is_empty() { 1 } else { 2 };
        let choice = self.rng.below(if depth == 0 { leaves } else { leaves + 5 });
        match choice {
            0 => lit(ty.clone(), value(self.rng, ty)),
            1 if leaves == 2 => self.pick(&from),
            c => match (c - leaves, ty) {
                (0, Type::Nullable(i)) => self.expr(i, d),
                (0, t)
                    if matches!(t, Type::U16 | Type::U32 | Type::U64 | Type::I64) || *t == DEC =>
                {
                    let op =
                        self.pick(&[BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div, BinOp::Rem]);
                    bin(op, self.expr(t, d), self.expr(t, d))
                }
                (1, t) if *t == DEC || matches!(t, Type::U64 | Type::I64) => {
                    let from = self.pick(&[Type::U16, Type::U32, Type::I64, DEC]);
                    Expr::Cast(Box::new(self.expr(&from, d)), t.clone())
                }
                (0 | 1, Type::Bool) => {
                    let t = self.pick(&[Type::U64, Type::I64, DEC, S8, KIND, Type::Bool]);
                    let op = self.pick(&[
                        BinOp::Eq,
                        BinOp::Ne,
                        BinOp::Lt,
                        BinOp::Le,
                        BinOp::Gt,
                        BinOp::Ge,
                    ]);
                    bin(op, self.expr(&t, d), self.expr(&t, d))
                }
                (2, Type::Bool) => match self.rng.below(3) {
                    0 => Expr::Not(Box::new(self.expr(ty, d))),
                    1 => {
                        let op = self.pick(&[BinOp::And, BinOp::Or]);
                        bin(op, self.expr(ty, d), self.expr(ty, d))
                    }
                    _ => {
                        let t = self.pick(&[null(Type::I64), null(S8)]);
                        Expr::IsNull(Box::new(self.expr(&t, d)))
                    }
                },
                (2, t) if inner.is_none() => Expr::UnwrapOr(
                    Box::new(self.expr(&null(t.clone()), d)),
                    Box::new(self.expr(t, d)),
                ),
                (3, _) => {
                    // match on a nullable value
                    let n = self.local();
                    let v = self.pick(&[null(Type::I64), null(S8), null(Type::Bool)]);
                    let value = self.expr(&v, d);
                    let if_null = self.expr(ty, d);
                    self.scope.push((n, GTy::Val(base(&v))));
                    let present = self.expr(ty, d);
                    self.scope.pop();
                    matching(value, if_null, present)
                }
                (4, _) => {
                    // a field of a row that may not exist
                    let tables = self.tables();
                    let with: Vec<(usize, usize, u16)> = (tables.iter())
                        .flat_map(|&t| {
                            let tb = self.tb(t);
                            (0..tb.columns.len()).filter_map(move |c| {
                                (logical(&tb.columns[c].ty) == *ty).then_some((t.0, t.1, c as u16))
                            })
                        })
                        .collect();
                    if with.is_empty() {
                        return lit(ty.clone(), value(self.rng, ty));
                    }
                    let (g, t, c) = self.pick(&with);
                    let n = self.local();
                    let value = self.get_row((g, t));
                    let if_null = self.expr(ty, d);
                    matching(value, if_null, field(Expr::Local(n), c))
                }
                _ => lit(ty.clone(), value(self.rng, ty)),
            },
        }
    }
}
