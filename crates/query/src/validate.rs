//! The fragment validator, document 05 section 4: types, tables and
//! columns, and the static memory bound of every operator.

use crate::{Aggregate, BinOp, Error, Expr, Fragment, Literal, Operator};
use granite_base::constants::ProcessConfig;
use granite_schema::{Catalog, column_size_max};
use granite_types::Type;
use granite_types::expr::{arithmetic, logical, numeric};
use std::ops::Bound;

/// Why a fragment is not valid: the first broken rule and where. Elements
/// (key ranges, key values, expressions, operators, aggregates) are
/// numbered from 1 in the order of the encoding; 0 is the fragment as a
/// whole and its scan's table, columns and time range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invalid {
    pub error: Error,
    pub node: u32,
}

/// What the validator found out about a valid fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The types of the fragment's output columns.
    pub columns: Vec<Type>,
    /// The memory bound in bytes of the scan, then of each operator.
    pub memory: Vec<u64>,
    /// Their sum: the memory the fragment may use, at most the limit.
    pub total: u64,
}

fn invalid(error: Error, node: u32) -> Invalid {
    Invalid { error, node }
}

/// A checked expression: its type, the bytes per row of every value it
/// computes (one per element), and its number.
struct E {
    ty: Type,
    bytes: u64,
    node: u32,
}

const BOOL: Type = Type::Bool;

fn nullable(t: &Type) -> Option<&Type> {
    match t {
        Type::Nullable(inner) => Some(inner),
        _ => None,
    }
}

/// A value of `got` is a value of `want`: the same type, or T where
/// Nullable(T) is wanted.
fn assignable(got: &Type, want: &Type) -> bool {
    got == want || nullable(want) == Some(got)
}

/// `got`, of element `node`, where a value of `want` is needed.
fn expect(got: &Type, want: &Type, node: u32) -> Result<(), Invalid> {
    if assignable(got, want) {
        return Ok(());
    }
    let inner = |t: &Type| numeric(nullable(t).unwrap_or(t));
    let error = if nullable(got).is_some() && nullable(want).is_none() {
        Error::NullableOperand
    } else if inner(got) && inner(want) {
        Error::ImplicitCast
    } else {
        Error::TypeMismatch
    };
    Err(invalid(error, node))
}

/// The type of `e`, which must not be nullable.
fn value(e: &E) -> Result<Type, Invalid> {
    match &e.ty {
        Type::Nullable(_) => Err(invalid(Error::NullableOperand, e.node)),
        t => Ok(t.clone()),
    }
}

/// The type of `e` where values are compared: neither nullable nor a float.
fn comparable(e: &E) -> Result<Type, Invalid> {
    let t = value(e)?;
    if matches!(t, Type::F32 | Type::F64) {
        return Err(invalid(Error::TypeMismatch, e.node));
    }
    Ok(t)
}

/// Bytes of one row of `columns`: the sum of their widths.
fn row(columns: &[Type]) -> u64 {
    columns
        .iter()
        .fold(0, |sum, t| sum.saturating_add(column_size_max(t)))
}

struct Validator {
    /// Elements numbered so far.
    node: u32,
}

impl Fragment {
    /// Validates the fragment against `catalog`, which must be valid, and
    /// computes its memory bound with the batch size and limit of `config`
    /// (document 05 section 4). The first broken rule is returned.
    pub fn validate(&self, catalog: &Catalog, config: &ProcessConfig) -> Result<Report, Invalid> {
        // A tree that encodes is within the depth limit, so the walk below
        // recurses at most that deep.
        self.encode().map_err(|e| invalid(e, 0))?;
        let batch = config.batch_rows_max;
        let mut v = Validator { node: 0 };
        let scan = &self.scan;
        let table = catalog
            .groups
            .iter()
            .flat_map(|g| &g.tables)
            .find(|t| t.name == scan.table)
            .ok_or(invalid(Error::UnknownTable, 0))?;
        if scan.columns.is_empty() {
            return Err(invalid(Error::NoColumns, 0));
        }
        let mut columns = scan
            .columns
            .iter()
            .map(|&c| {
                let c = table.columns.get(usize::from(c));
                c.map(|c| logical(&c.ty))
                    .ok_or(invalid(Error::UnknownColumn, 0))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let key: Vec<Type> = table
            .primary_key
            .iter()
            .map(|&(c, _)| logical(&table.columns[usize::from(c)].ty))
            .collect();
        for range in &scan.ranges {
            let node = v.enter();
            for bound in [&range.lower, &range.upper] {
                v.bound(bound, &key, node)?;
            }
        }
        let mut exprs = 0;
        if let Some(p) = &scan.predicate {
            let p = v.expr(p, &columns)?;
            expect(&p.ty, &BOOL, p.node)?;
            exprs = p.bytes;
        }
        let mut memory = vec![bound(batch, &columns, exprs, 0)];
        let mut aggregated = false;
        for op in &self.operators {
            let node = v.enter();
            if aggregated {
                return Err(invalid(Error::AfterAggregate, node));
            }
            let (out, exprs, state) = match op {
                Operator::Project(list) => {
                    let mut out = Vec::new();
                    let mut exprs = 0u64;
                    for e in list {
                        let e = v.expr(e, &columns)?;
                        exprs = exprs.saturating_add(e.bytes);
                        out.push(e.ty);
                    }
                    (out, exprs, 0)
                }
                Operator::PartialAggregate {
                    keys,
                    aggregates,
                    groups,
                } => {
                    if *groups == 0 {
                        return Err(invalid(Error::NoGroupBound, node));
                    }
                    aggregated = true;
                    let mut out = Vec::new();
                    let mut exprs = 0u64;
                    for k in keys {
                        let k = v.expr(k, &columns)?;
                        out.push(comparable(&k)?);
                        exprs = exprs.saturating_add(k.bytes);
                    }
                    for a in aggregates {
                        v.enter();
                        let ty = match a {
                            Aggregate::Count => Type::U64,
                            Aggregate::Sum(e) | Aggregate::Min(e) | Aggregate::Max(e) => {
                                let e = v.expr(e, &columns)?;
                                exprs = exprs.saturating_add(e.bytes);
                                let t = match a {
                                    Aggregate::Sum(_) => value(&e)?,
                                    _ => comparable(&e)?,
                                };
                                if matches!(a, Aggregate::Sum(_)) && !numeric(&t) {
                                    return Err(invalid(Error::TypeMismatch, e.node));
                                }
                                t
                            }
                        };
                        out.push(ty);
                    }
                    // A group's keys and aggregates, and 8 bytes of hash index.
                    let state = u64::from(*groups).saturating_mul(row(&out).saturating_add(8));
                    (out, exprs, state)
                }
                Operator::TopN { order, n } => {
                    if *n == 0 {
                        return Err(invalid(Error::NoTopN, node));
                    }
                    let mut keys = Vec::new();
                    let mut exprs = 0u64;
                    for (e, _) in order {
                        let e = v.expr(e, &columns)?;
                        keys.push(comparable(&e)?);
                        exprs = exprs.saturating_add(e.bytes);
                    }
                    // n rows of the input, each with its order key.
                    let state =
                        u64::from(*n).saturating_mul(row(&columns).saturating_add(row(&keys)));
                    (columns.clone(), exprs, state)
                }
                Operator::Limit(_) => (columns.clone(), 0, 0),
            };
            columns = out;
            memory.push(bound(batch, &columns, exprs, state));
        }
        assert_eq!(
            memory.len(),
            self.operators.len() + 1,
            "one bound per operator"
        );
        let total = memory.iter().fold(0u64, |sum, m| sum.saturating_add(*m));
        if total > config.fragment_memory_max {
            return Err(invalid(Error::OverMemoryLimit, 0));
        }
        Ok(Report {
            columns,
            memory,
            total,
        })
    }
}

/// An operator's memory bound: one batch of its output, one batch of every
/// value its expressions compute (`exprs` bytes per row), and its state.
fn bound(batch: u64, out: &[Type], exprs: u64, state: u64) -> u64 {
    batch
        .saturating_mul(row(out).saturating_add(exprs))
        .saturating_add(state)
}

impl Validator {
    fn enter(&mut self) -> u32 {
        self.node += 1;
        self.node
    }

    /// A key bound: one value per key column for its first ones, each of
    /// the column's type.
    fn bound(&mut self, b: &Bound<Vec<Literal>>, key: &[Type], range: u32) -> Result<(), Invalid> {
        let values = match b {
            Bound::Unbounded => return Ok(()),
            Bound::Included(v) | Bound::Excluded(v) => v,
        };
        if values.is_empty() || values.len() > key.len() {
            return Err(invalid(Error::WrongArity, range));
        }
        for ((ty, _), want) in values.iter().zip(key) {
            let node = self.enter();
            expect(&logical(ty), want, node)?;
        }
        Ok(())
    }

    fn expr(&mut self, e: &Expr, input: &[Type]) -> Result<E, Invalid> {
        let node = self.enter();
        let r = |ty: Type, bytes: u64| E {
            bytes: bytes.saturating_add(column_size_max(&ty)),
            ty,
            node,
        };
        Ok(match e {
            Expr::Literal(literal) => r(logical(&literal.0), 0),
            Expr::Column(i) => r(
                (input.get(usize::from(*i)).cloned()).ok_or(invalid(Error::UnknownColumn, node))?,
                0,
            ),
            Expr::Neg(a) => {
                let a = self.expr(a, input)?;
                let t = value(&a)?;
                if !arithmetic(&t) {
                    return Err(invalid(Error::TypeMismatch, a.node));
                }
                r(t, a.bytes)
            }
            Expr::Not(a) => {
                let a = self.expr(a, input)?;
                expect(&a.ty, &BOOL, a.node)?;
                r(BOOL, a.bytes)
            }
            Expr::IsNull(a) => {
                let a = self.expr(a, input)?;
                if nullable(&a.ty).is_none() {
                    return Err(invalid(Error::TypeMismatch, a.node));
                }
                r(BOOL, a.bytes)
            }
            Expr::UnwrapOr(a, b) => {
                let a = self.expr(a, input)?;
                let ty = (nullable(&a.ty).cloned()).ok_or(invalid(Error::TypeMismatch, a.node))?;
                let b = self.expr(b, input)?;
                expect(&b.ty, &ty, b.node)?;
                r(ty, a.bytes.saturating_add(b.bytes))
            }
            Expr::Cast(a, to) => {
                let a = self.expr(a, input)?;
                let to = logical(to);
                if !numeric(&value(&a)?) || !numeric(&to) {
                    return Err(invalid(Error::TypeMismatch, node));
                }
                r(to, a.bytes)
            }
            Expr::Binary(op, a, b) => {
                let (a, b) = (self.expr(a, input)?, self.expr(b, input)?);
                let ty = match op {
                    BinOp::And | BinOp::Or => {
                        expect(&a.ty, &BOOL, a.node)?;
                        BOOL
                    }
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                        let t = value(&a)?;
                        if !arithmetic(&t) {
                            return Err(invalid(Error::TypeMismatch, a.node));
                        }
                        t
                    }
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        comparable(&a)?;
                        BOOL
                    }
                };
                // The right operand has the left one's type.
                expect(&b.ty, &a.ty, b.node)?;
                r(ty, a.bytes.saturating_add(b.bytes))
            }
        })
    }
}
