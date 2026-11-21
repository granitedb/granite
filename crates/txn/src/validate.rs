//! The validator, document 03 section 5.2: everything about a program that
//! can be known before it runs, checked once against a catalog. A valid
//! program is well typed, its reads and writes per invocation are bounded,
//! and each access's key provenance and partition-key expressions are known.

use crate::{BinOp, Error, Expr, Param, Program, Stmt};
use granite_base::constants::FORMAT;
use granite_schema::{Catalog, Kind, Table};
use granite_types::{Type, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

/// Why a program is not valid, and where: `node` numbers the statements and
/// expressions from 1 in the order of the encoding; 0 is the program as a
/// whole (its encoding, its parameters, a missing return).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invalid {
    pub error: Error,
    pub node: u32,
}

/// What the validator found out about a valid program. Its expressions
/// are the program's own, borrowed, so validation copies none.
#[derive(Clone, Debug, PartialEq)]
pub struct Report<'p> {
    /// The most rows one invocation reads: a `get` and an `update` read one,
    /// a scan up to its limit.
    pub reads: u64,
    /// The most rows one invocation writes.
    pub writes: u64,
    /// The type every `return` gives, or `None` for a program that does not return.
    pub returns: Option<Type>,
    /// Every read and write of the program, in the order of the encoding.
    pub accesses: Vec<Access<'p>>,
    /// How each local is bound, by its number.
    pub locals: Vec<Local<'p>>,
    /// The conditions under which accesses happen; see [`Access::guard`].
    pub guards: Vec<Guard<'p>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessKind {
    Get,
    Scan,
    Insert,
    Upsert,
    Update,
    Delete,
}

/// Whether an access's key, and whether it happens at all, follow from the
/// parameters alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    /// Computable from the parameters before the invocation runs.
    Static,
    /// Depends on an earlier read, or on `commit_ts()`.
    Dependent,
}

/// One `get`, scan, `insert`, `upsert`, `update` or `delete` of the program.
#[derive(Clone, Debug, PartialEq)]
pub struct Access<'p> {
    pub kind: AccessKind,
    /// The table, by its group's and its own position in the catalog.
    pub group: usize,
    pub table: usize,
    pub provenance: Provenance,
    /// One expression per partition key column: the partition the access
    /// touches. Locals in it are resolved through [`Report::locals`].
    pub partition_key: Vec<&'p Expr>,
    /// The innermost condition the access is under, an index into
    /// [`Report::guards`]; the access happens, in an invocation that
    /// commits, exactly when every guard of the chain holds. Guards are
    /// evaluated from the outermost: an inner one may use a `match` local
    /// that is bound only where the outer ones hold.
    pub guard: Option<usize>,
}

/// A condition on the path to an access: `cond` evaluates to `holds`, or,
/// with `is_null`, `is_null(cond)` does (the value of a `match`).
#[derive(Clone, Debug, PartialEq)]
pub struct Guard<'p> {
    pub cond: &'p Expr,
    pub is_null: bool,
    pub holds: bool,
    pub parent: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Local<'p> {
    /// Bound by `let` to the value of the expression.
    Let(&'p Expr),
    /// Bound by `match` to the value of the expression, which is not null there.
    Match(&'p Expr),
    /// Bound by `for` to a scanned row.
    Row,
}

/// The type of an expression: a value, or a row of a table.
#[derive(Clone, Debug, PartialEq)]
enum Ty {
    /// A type without LowCardinality hints, which do not change the logical type.
    Value(Type),
    Row {
        group: usize,
        table: usize,
        nullable: bool,
    },
}

/// Reads, writes and both together on the costliest path; `total` is not
/// always `reads + writes`, as the two maxima can lie on different paths.
#[derive(Clone, Copy, Debug, Default)]
struct Cost {
    reads: u64,
    writes: u64,
    total: u64,
}

impl Cost {
    const READ: Cost = Cost {
        reads: 1,
        writes: 0,
        total: 1,
    };
    const WRITE: Cost = Cost {
        reads: 0,
        writes: 1,
        total: 1,
    };

    /// Each part is at most the access limit (checked at every node), so
    /// none of the sums and products below can overflow a u64.
    fn then(self, o: Cost) -> Cost {
        Cost {
            reads: self.reads + o.reads,
            writes: self.writes + o.writes,
            total: self.total + o.total,
        }
    }

    fn or(self, o: Cost) -> Cost {
        Cost {
            reads: self.reads.max(o.reads),
            writes: self.writes.max(o.writes),
            total: self.total.max(o.total),
        }
    }

    fn times(self, n: u64) -> Cost {
        Cost {
            reads: self.reads * n,
            writes: self.writes * n,
            total: self.total * n,
        }
    }
}

/// Whether a statement or block returns: on no path, on some, or on every path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    None,
    Some,
    All,
}

/// A checked expression.
struct E {
    ty: Ty,
    /// Depends on a read or on `commit_ts()`.
    dep: bool,
    cost: Cost,
    node: u32,
}

fn logical(t: &Type) -> Type {
    match t {
        Type::LowCardinality(inner) => logical(inner),
        Type::Nullable(inner) => Type::Nullable(Box::new(logical(inner))),
        t => t.clone(),
    }
}

fn nullable(t: &Ty) -> Option<Ty> {
    match t {
        Ty::Value(Type::Nullable(inner)) => Some(Ty::Value((**inner).clone())),
        &Ty::Row {
            group,
            table,
            nullable: true,
        } => Some(Ty::Row {
            group,
            table,
            nullable: false,
        }),
        _ => None,
    }
}

/// Integers and decimals, which have arithmetic.
fn arithmetic(t: &Type) -> bool {
    !matches!(
        t,
        Type::F32
            | Type::F64
            | Type::Bool
            | Type::Date
            | Type::Timestamp(_)
            | Type::Duration(_)
            | Type::Uuid
            | Type::Id128
            | Type::String { .. }
            | Type::Bytes { .. }
            | Type::FixedBytes(_)
            | Type::Enum8 { .. }
            | Type::Enum16 { .. }
            | Type::Nullable(_)
            | Type::LowCardinality(_)
    )
}

/// The types between which a cast exists.
fn numeric(t: &Type) -> bool {
    arithmetic(t) || matches!(t, Type::F32 | Type::F64)
}

/// A value of `got` is a value of `want`: the same type, or T where
/// Nullable(T) is wanted.
fn assignable(got: &Ty, want: &Ty) -> bool {
    got == want || nullable(want).as_ref() == Some(got)
}

fn invalid(error: Error, node: u32) -> Invalid {
    Invalid { error, node }
}

/// `e` where a value of `want` is needed.
fn expect(e: &E, want: &Ty) -> Result<(), Invalid> {
    if assignable(&e.ty, want) {
        return Ok(());
    }
    let inner = |t: &Ty| match nullable(t).unwrap_or_else(|| t.clone()) {
        Ty::Value(t) => numeric(&t),
        Ty::Row { .. } => false,
    };
    let error = if nullable(&e.ty).is_some() && nullable(want).is_none() {
        Error::NullableOperand
    } else if inner(&e.ty) && inner(want) {
        Error::ImplicitCast
    } else {
        Error::TypeMismatch
    };
    Err(invalid(error, e.node))
}

/// The value type of `e`, which must not be a row or nullable.
fn value(e: &E) -> Result<Type, Invalid> {
    match &e.ty {
        Ty::Row { .. } => Err(invalid(Error::TypeMismatch, e.node)),
        Ty::Value(Type::Nullable(_)) => Err(invalid(Error::NullableOperand, e.node)),
        Ty::Value(t) => Ok(t.clone()),
    }
}

/// The type both of two branches give: the same, or Nullable(T) for T and Nullable(T).
fn unify(a: Ty, b: &E) -> Result<Ty, Invalid> {
    if assignable(&b.ty, &a) {
        Ok(a)
    } else if assignable(&a, &b.ty) {
        Ok(b.ty.clone())
    } else {
        expect(b, &a).map(|()| a)
    }
}

const BOOL: Ty = Ty::Value(Type::Bool);

impl Program {
    /// Validates the program against `catalog`, which must be valid
    /// (document 03 section 5.2). The first broken rule is returned.
    pub fn validate<'p>(&'p self, catalog: &Catalog) -> Result<Report<'p>, Invalid> {
        // A tree that encodes is within the depth limit, so the walk below
        // recurses at most that deep, and its literals and types are valid.
        self.encode().map_err(|error| invalid(error, 0))?;
        let mut tables = BTreeMap::new();
        for (g, group) in catalog.groups.iter().enumerate() {
            for (t, table) in group.tables.iter().enumerate() {
                tables.insert(table.name.as_str(), (g, t));
            }
        }
        let mut v = Validator {
            catalog,
            tables,
            params: self.params.iter().map(|p| logical(&p.ty)).collect(),
            node: 0,
            locals: Vec::new(),
            scope: Vec::new(),
            control: false,
            guard: None,
            returns: None,
            report: Report {
                reads: 0,
                writes: 0,
                returns: None,
                accesses: Vec::new(),
                locals: Vec::new(),
                guards: Vec::new(),
            },
        };
        let (cost, flow) = v.block(&self.body)?;
        if flow == Flow::Some {
            return Err(invalid(Error::MissingReturn, 0));
        }
        let mut report = v.report;
        report.reads = cost.reads;
        report.writes = cost.writes;
        report.returns = v.returns.map(|t| match t {
            Ty::Value(t) => t,
            Ty::Row { .. } => unreachable!("a return of a row is rejected"),
        });
        assert!(cost.total <= FORMAT.program_accesses_max);
        assert!(report.reads + report.writes >= cost.total);
        Ok(report)
    }
}

impl Program {
    /// Checks one invocation's parameter values against the declared
    /// types: one value per parameter, each a value of its type
    /// (`Type::check`: null only for Nullable, decimals within their
    /// precision). The executor runs it before the program.
    pub fn check_params(&self, values: &[Value]) -> Result<(), Error> {
        let fits = |(p, v): (&Param, &Value)| p.ty.check(v).is_ok();
        if values.len() == self.params.len() && self.params.iter().zip(values).all(fits) {
            Ok(())
        } else {
            Err(Error::ParamType)
        }
    }
}

struct Validator<'a, 'p> {
    catalog: &'a Catalog,
    tables: BTreeMap<&'a str, (usize, usize)>,
    params: Vec<Type>,
    /// The number of the last statement or expression entered.
    node: u32,
    /// Each local's type, whether it depends on a read, and whether it is in scope.
    locals: Vec<(Ty, bool, bool)>,
    /// The locals in scope, innermost last.
    scope: Vec<usize>,
    /// Whether the current position is reached depending on a read.
    control: bool,
    guard: Option<usize>,
    returns: Option<Ty>,
    report: Report<'p>,
}

impl<'a, 'p> Validator<'a, 'p> {
    fn enter(&mut self) -> u32 {
        self.node += 1;
        self.node
    }

    /// Fails past the access limit; called on every node, which keeps every
    /// cost small enough that combining them cannot overflow.
    fn bounded(&self, cost: Cost, node: u32) -> Result<Cost, Invalid> {
        if cost.total > FORMAT.program_accesses_max {
            return Err(invalid(Error::TooManyAccesses, node));
        }
        Ok(cost)
    }

    fn table(&self, name: &str, node: u32) -> Result<(usize, usize, &'a Table), Invalid> {
        let &(g, t) = self
            .tables
            .get(name)
            .ok_or(invalid(Error::UnknownTable, node))?;
        Ok((g, t, &self.catalog.groups[g].tables[t]))
    }

    fn writable(&self, table: &Table, node: u32) -> Result<(), Invalid> {
        if table.kind == Kind::Immutable {
            return Err(invalid(Error::ImmutableTable, node));
        }
        Ok(())
    }

    fn column(table: &Table, i: u16) -> Ty {
        Ty::Value(logical(&table.columns[usize::from(i)].ty))
    }

    /// Allocates the next local; it comes into scope with [`Validator::bind`].
    fn local(&mut self, binding: Local<'p>) -> usize {
        self.report.locals.push(binding);
        self.locals.push((BOOL, false, false));
        self.locals.len() - 1
    }

    fn bind(&mut self, local: usize, ty: Ty, dep: bool) {
        self.locals[local] = (ty, dep, true);
        self.scope.push(local);
    }

    /// Takes the locals bound since `mark` out of scope.
    fn unbind(&mut self, mark: usize) {
        for l in self.scope.drain(mark..) {
            self.locals[l].2 = false;
        }
    }

    /// Runs `f` under the condition `cond == holds` (`is_null(cond)` with
    /// `is_null`), which depends on a read when `dep`.
    fn guarded<T>(
        &mut self,
        cond: &'p Expr,
        is_null: bool,
        holds: bool,
        dep: bool,
        f: impl FnOnce(&mut Self) -> Result<T, Invalid>,
    ) -> Result<T, Invalid> {
        let (control, guard) = (self.control, self.guard);
        self.report.guards.push(Guard {
            cond,
            is_null,
            holds,
            parent: guard,
        });
        self.guard = Some(self.report.guards.len() - 1);
        self.control |= dep;
        let r = f(self);
        (self.control, self.guard) = (control, guard);
        r
    }

    fn access(
        &mut self,
        kind: AccessKind,
        group: usize,
        table: usize,
        key: &[&'p Expr],
        dep: bool,
    ) {
        let n = self.catalog.groups[group].partition_key.len();
        self.report.accesses.push(Access {
            kind,
            group,
            table,
            provenance: if dep || self.control {
                Provenance::Dependent
            } else {
                Provenance::Static
            },
            partition_key: key[..n].to_vec(),
            guard: self.guard,
        });
    }

    /// Expressions for the first `exprs.len()` primary key columns of `table`.
    fn key(&mut self, table: &Table, exprs: &'p [Expr]) -> Result<(Cost, bool), Invalid> {
        let (mut cost, mut dep) = (Cost::default(), false);
        for (e, &(column, _)) in exprs.iter().zip(&table.primary_key) {
            let e = self.expr(e)?;
            expect(&e, &Self::column(table, column))?;
            cost = cost.then(e.cost);
            dep |= e.dep;
        }
        Ok((cost, dep))
    }

    fn block(&mut self, stmts: &'p [Stmt]) -> Result<(Cost, Flow), Invalid> {
        let mark = self.scope.len();
        let (mut cost, mut flow) = (Cost::default(), Flow::None);
        for s in stmts {
            let node = self.node + 1;
            if flow != Flow::None {
                return Err(invalid(Error::CodeAfterReturn, node));
            }
            let (c, f) = self.stmt(s)?;
            cost = self.bounded(cost.then(c), node)?;
            flow = f;
        }
        self.unbind(mark);
        Ok((cost, flow))
    }

    fn stmt(&mut self, s: &'p Stmt) -> Result<(Cost, Flow), Invalid> {
        let node = self.enter();
        let (cost, flow) = match s {
            Stmt::Let(e) => {
                let local = self.local(Local::Let(e));
                let e = self.expr(e)?;
                self.bind(local, e.ty, e.dep);
                (e.cost, Flow::None)
            }
            Stmt::If {
                cond,
                then,
                otherwise,
            } => {
                let c = self.expr(cond)?;
                expect(&c, &BOOL)?;
                let (a, fa) = self.guarded(cond, false, true, c.dep, |v| v.block(then))?;
                let (b, fb) = self.guarded(cond, false, false, c.dep, |v| v.block(otherwise))?;
                let flow = match (fa, fb) {
                    (Flow::None, Flow::None) => Flow::None,
                    (Flow::All, Flow::All) => Flow::All,
                    _ => Flow::Some,
                };
                (c.cost.then(a.or(b)), flow)
            }
            Stmt::Require { cond, .. } => {
                let c = self.expr(cond)?;
                expect(&c, &BOOL)?;
                (c.cost, Flow::None)
            }
            Stmt::Insert { table, row } | Stmt::Upsert { table, row } => {
                let (g, t, tb) = self.table(table, node)?;
                let kind = if matches!(s, Stmt::Insert { .. }) {
                    AccessKind::Insert
                } else {
                    self.writable(tb, node)?;
                    AccessKind::Upsert
                };
                if row.len() != tb.columns.len() {
                    return Err(invalid(Error::WrongArity, node));
                }
                let (mut cost, mut dep) = (Cost::WRITE, false);
                for (i, e) in row.iter().enumerate() {
                    let e = self.expr(e)?;
                    expect(&e, &Ty::Value(logical(&tb.columns[i].ty)))?;
                    cost = cost.then(e.cost);
                    // Only the key decides where the row goes.
                    dep |= e.dep && tb.primary_key.iter().any(|&(c, _)| usize::from(c) == i);
                }
                let key: Vec<&'p Expr> = (tb.primary_key.iter())
                    .map(|&(c, _)| &row[usize::from(c)])
                    .collect();
                self.access(kind, g, t, &key, dep);
                (cost, Flow::None)
            }
            Stmt::Update { table, key, set } => {
                let (g, t, tb) = self.table(table, node)?;
                self.writable(tb, node)?;
                if key.len() != tb.primary_key.len() {
                    return Err(invalid(Error::WrongArity, node));
                }
                let (mut cost, dep) = self.key(tb, key)?;
                let mut seen = BTreeSet::new();
                for (column, e) in set {
                    if usize::from(*column) >= tb.columns.len() {
                        return Err(invalid(Error::UnknownColumn, node));
                    }
                    if tb.primary_key.iter().any(|(c, _)| c == column) {
                        return Err(invalid(Error::KeyColumnUpdated, node));
                    }
                    if !seen.insert(*column) {
                        return Err(invalid(Error::ColumnSetTwice, node));
                    }
                    let e = self.expr(e)?;
                    expect(&e, &Self::column(tb, *column))?;
                    cost = cost.then(e.cost);
                }
                let key: Vec<&'p Expr> = key.iter().collect();
                self.access(AccessKind::Update, g, t, &key, dep);
                (cost.then(Cost::READ).then(Cost::WRITE), Flow::None)
            }
            Stmt::Delete { table, key } => {
                let (g, t, tb) = self.table(table, node)?;
                self.writable(tb, node)?;
                if key.len() != tb.primary_key.len() {
                    return Err(invalid(Error::WrongArity, node));
                }
                let (cost, dep) = self.key(tb, key)?;
                let key: Vec<&'p Expr> = key.iter().collect();
                self.access(AccessKind::Delete, g, t, &key, dep);
                (cost.then(Cost::WRITE), Flow::None)
            }
            Stmt::For { scan, body } => {
                let local = self.local(Local::Row);
                let (g, t, tb) = self.table(&scan.table, node)?;
                let partition = self.catalog.groups[g].partition_key.len();
                let n = scan.prefix.len();
                if n < partition {
                    return Err(invalid(Error::ScanAcrossPartitions, node));
                }
                let bounded = |b: &Bound<Expr>| !matches!(b, Bound::Unbounded);
                if n > tb.primary_key.len()
                    || (n == tb.primary_key.len() && (bounded(&scan.lower) || bounded(&scan.upper)))
                {
                    return Err(invalid(Error::WrongArity, node));
                }
                let (mut cost, mut dep) = self.key(tb, &scan.prefix)?;
                for b in [&scan.lower, &scan.upper] {
                    if let Bound::Included(e) | Bound::Excluded(e) = b {
                        let e = self.expr(e)?;
                        expect(&e, &Self::column(tb, tb.primary_key[n].0))?;
                        cost = cost.then(e.cost);
                        dep |= e.dep;
                    }
                }
                let prefix: Vec<&'p Expr> = scan.prefix.iter().collect();
                self.access(AccessKind::Scan, g, t, &prefix, dep);
                let limit = u64::from(scan.limit);
                let cost = self.bounded(cost.then(Cost::READ.times(limit)), node)?;
                // How often the body runs depends on what the scan finds.
                let control = std::mem::replace(&mut self.control, true);
                let mark = self.scope.len();
                self.bind(
                    local,
                    Ty::Row {
                        group: g,
                        table: t,
                        nullable: false,
                    },
                    true,
                );
                let body = self.block(body);
                self.unbind(mark);
                self.control = control;
                let (b, f) = body?;
                let flow = if f == Flow::None {
                    Flow::None
                } else {
                    Flow::Some
                };
                (cost.then(self.bounded(b, node)?.times(limit)), flow)
            }
            Stmt::Return(e) => {
                let e = self.expr(e)?;
                if let Ty::Row { .. } = e.ty {
                    return Err(invalid(Error::TypeMismatch, e.node));
                }
                self.returns = Some(match self.returns.take() {
                    None => e.ty.clone(),
                    Some(t) => unify(t, &e)?,
                });
                (e.cost, Flow::All)
            }
        };
        Ok((self.bounded(cost, node)?, flow))
    }

    fn expr(&mut self, e: &'p Expr) -> Result<E, Invalid> {
        let node = self.enter();
        let leaf = |ty, dep| E {
            ty,
            dep,
            cost: Cost::default(),
            node,
        };
        let r = match e {
            Expr::Literal(literal) => leaf(Ty::Value(logical(&literal.0)), false),
            Expr::Param(i) => {
                let ty =
                    (self.params.get(usize::from(*i))).ok_or(invalid(Error::UnknownParam, node))?;
                leaf(Ty::Value(ty.clone()), false)
            }
            Expr::Local(i) => {
                let (ty, dep, visible) =
                    (self.locals.get(usize::from(*i))).ok_or(invalid(Error::UnknownLocal, node))?;
                if !visible {
                    return Err(invalid(Error::UnknownLocal, node));
                }
                leaf(ty.clone(), *dep)
            }
            Expr::CommitTs => leaf(Ty::Value(Type::U64), true),
            Expr::Field(row, column) => {
                let r = self.expr(row)?;
                let ty = match r.ty {
                    Ty::Row {
                        group,
                        table,
                        nullable: false,
                    } => {
                        let tb = &self.catalog.groups[group].tables[table];
                        if usize::from(*column) >= tb.columns.len() {
                            return Err(invalid(Error::UnknownColumn, node));
                        }
                        Self::column(tb, *column)
                    }
                    Ty::Row { nullable: true, .. } => {
                        return Err(invalid(Error::NullableOperand, r.node));
                    }
                    Ty::Value(_) => return Err(invalid(Error::TypeMismatch, r.node)),
                };
                E { ty, node, ..r }
            }
            Expr::Get { table, key } => {
                let (g, t, tb) = self.table(table, node)?;
                if key.len() != tb.primary_key.len() {
                    return Err(invalid(Error::WrongArity, node));
                }
                let (cost, dep) = self.key(tb, key)?;
                let key: Vec<&'p Expr> = key.iter().collect();
                self.access(AccessKind::Get, g, t, &key, dep);
                E {
                    ty: Ty::Row {
                        group: g,
                        table: t,
                        nullable: true,
                    },
                    dep: true,
                    cost: cost.then(Cost::READ),
                    node,
                }
            }
            Expr::Neg(a) => {
                let a = self.expr(a)?;
                if !arithmetic(&value(&a)?) {
                    return Err(invalid(Error::TypeMismatch, a.node));
                }
                E { node, ..a }
            }
            Expr::Not(a) => {
                let a = self.expr(a)?;
                expect(&a, &BOOL)?;
                E { node, ..a }
            }
            Expr::IsNull(a) => {
                let a = self.expr(a)?;
                if nullable(&a.ty).is_none() {
                    return Err(invalid(Error::TypeMismatch, a.node));
                }
                E {
                    ty: BOOL,
                    node,
                    ..a
                }
            }
            Expr::UnwrapOr(a, b) => {
                let a = self.expr(a)?;
                let ty = nullable(&a.ty).ok_or(invalid(Error::TypeMismatch, a.node))?;
                let b = self.expr(b)?;
                expect(&b, &ty)?;
                E {
                    ty,
                    dep: a.dep || b.dep,
                    cost: a.cost.then(b.cost),
                    node,
                }
            }
            Expr::Match {
                value,
                if_null,
                present,
            } => {
                let local = self.local(Local::Match(value));
                let v = self.expr(value)?;
                let ty = nullable(&v.ty).ok_or(invalid(Error::TypeMismatch, v.node))?;
                let a = self.guarded(value, true, true, v.dep, |s| s.expr(if_null))?;
                let b = self.guarded(value, true, false, v.dep, |s| {
                    let mark = s.scope.len();
                    s.bind(local, ty, v.dep);
                    let b = s.expr(present);
                    s.unbind(mark);
                    b
                })?;
                E {
                    ty: unify(a.ty.clone(), &b)?,
                    dep: v.dep || a.dep || b.dep,
                    cost: v.cost.then(a.cost.or(b.cost)),
                    node,
                }
            }
            Expr::Cast(a, to) => {
                let a = self.expr(a)?;
                let to = logical(to);
                if !numeric(&value(&a)?) || !numeric(&to) {
                    return Err(invalid(Error::TypeMismatch, node));
                }
                E {
                    ty: Ty::Value(to),
                    node,
                    ..a
                }
            }
            Expr::Binary(op, a, b) => {
                let (a, b) = (self.expr(a)?, self.expr(b)?);
                let ty = match op {
                    BinOp::And | BinOp::Or => {
                        expect(&a, &BOOL)?;
                        BOOL
                    }
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                        let t = value(&a)?;
                        if !arithmetic(&t) {
                            return Err(invalid(Error::TypeMismatch, a.node));
                        }
                        Ty::Value(t)
                    }
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        let t = value(&a)?;
                        if matches!(t, Type::F32 | Type::F64) {
                            return Err(invalid(Error::TypeMismatch, a.node));
                        }
                        BOOL
                    }
                };
                // The right operand has the left one's type.
                expect(&b, &a.ty)?;
                E {
                    ty,
                    dep: a.dep || b.dep,
                    cost: a.cost.then(b.cost),
                    node,
                }
            }
        };
        let cost = self.bounded(r.cost, node)?;
        Ok(E { cost, ..r })
    }
}
