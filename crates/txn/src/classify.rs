//! Classification of a valid program, document 03 section 5.4: whether it
//! runs on one shard, or across shards as an independent or a coordinated
//! transaction (document 04 section 4). It is decided from the IR alone, so
//! the server derives it again at registration and never takes a client's.

use crate::{BinOp, Error, Expr, Invalid, Program, Provenance, Report, Stmt};
use granite_schema::{Catalog, Kind};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Every access names one partition, computed from the parameters.
    SingleShard,
    /// Several partitions, and nothing that can fail.
    Independent,
    /// Several partitions, and `node` is the first statement or expression
    /// that can fail, for `cause`.
    Coordinated { node: u32, cause: Fallible },
}

/// What can make an invocation fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallible {
    Require,
    /// Arithmetic, a negation or a cast, which can overflow or not fit.
    Expression,
    /// An insert into a versioned table, which fails when the key exists.
    Insert,
    /// An update, which fails when the row does not exist.
    Update,
}

/// A table group and partition-key expressions: one partition per invocation.
pub type Partition<'p> = (usize, Vec<&'p Expr>);

#[derive(Clone, Debug, PartialEq)]
pub struct Classification<'p> {
    pub class: Class,
    /// The partitions the accesses name, compared as written, in the order
    /// of their first access: the program's shard set.
    pub partitions: Vec<Partition<'p>>,
}

/// Where a value comes from: reads of partitions (indexes into
/// `Classification::partitions`) and `commit_ts()`.
#[derive(Clone, Debug, Default)]
struct Src {
    reads: BTreeSet<usize>,
    ts: bool,
}

impl Src {
    fn with(mut self, o: &Src) -> Src {
        self.reads.extend(&o.reads);
        self.ts |= o.ts;
        self
    }

    fn depends(&self) -> bool {
        self.ts || !self.reads.is_empty()
    }
}

impl Program {
    /// Classifies the program, which `catalog` validated with `report`.
    /// Rejects, with `MustBeSingleShard`, a program with a dependent access
    /// whose accesses do not all name one partition computed from the
    /// parameters; and, with `ValueAcrossParticipants`, a program across
    /// partitions in which one participant would need a value only another
    /// has (document 03 section 5.4).
    pub fn classify<'p>(
        &'p self,
        catalog: &Catalog,
        report: &Report<'p>,
    ) -> Result<Classification<'p>, Invalid> {
        let mut partitions: Vec<Partition<'p>> = Vec::new();
        let mut at = BTreeMap::new();
        for a in &report.accesses {
            let p = (a.group, a.partition_key.clone());
            // Quadratic in the partitions, which `program_nodes_max` bounds.
            let n = (partitions.iter().position(|q| *q == p)).unwrap_or_else(|| {
                partitions.push(p);
                partitions.len() - 1
            });
            let versioned = catalog.groups[a.group].tables[a.table].kind == Kind::Versioned;
            at.insert(a.node, (n, versioned));
        }
        let keys = (report.accesses.iter())
            .flat_map(|a| a.partition_key.iter().map(|e| std::ptr::from_ref(*e)))
            .collect();
        let mut w = Walk {
            at,
            keys,
            dynamic: BTreeSet::new(),
            node: 0,
            locals: Vec::new(),
            control: Src::default(),
            flow: None,
            fallible: None,
        };
        w.block(&self.body);
        assert_eq!(w.locals.len(), report.locals.len());

        let dependent = (report.accesses.iter()).any(|a| a.provenance == Provenance::Dependent);
        let class = if dependent || partitions.len() <= 1 {
            for a in &report.accesses {
                let elsewhere = w.at[&a.node].0 != 0;
                let from_read =
                    (a.partition_key.iter()).any(|e| w.dynamic.contains(&std::ptr::from_ref(*e)));
                if elsewhere || from_read {
                    return Err(Invalid {
                        error: Error::MustBeSingleShard,
                        node: a.node,
                    });
                }
            }
            Class::SingleShard
        } else if let Some(node) = w.flow {
            return Err(Invalid {
                error: Error::ValueAcrossParticipants,
                node,
            });
        } else if let Some((node, cause)) = w.fallible {
            Class::Coordinated { node, cause }
        } else {
            Class::Independent
        };
        Ok(Classification { class, partitions })
    }
}

/// Walks the program in the order of the encoding, numbering nodes as the
/// validator does, and finds where each value comes from.
struct Walk {
    /// The partition of the access each node makes, and whether its table
    /// is versioned, by node.
    at: BTreeMap<u32, (usize, bool)>,
    /// The partition-key expressions, by address.
    keys: BTreeSet<*const Expr>,
    /// The partition-key expressions that depend on a read or `commit_ts()`.
    dynamic: BTreeSet<*const Expr>,
    node: u32,
    /// Where each local's value comes from, by number.
    locals: Vec<Src>,
    /// Where the conditions the current position is reached under come from.
    control: Src,
    /// The first node where a participant would need another's value.
    flow: Option<u32>,
    /// The first node that can fail.
    fallible: Option<(u32, Fallible)>,
}

impl Walk {
    fn enter(&mut self) -> u32 {
        self.node += 1;
        self.node
    }

    /// A value from `src` used at `node`, on `partition` if given: across
    /// partitions, only one participant can have it.
    fn flow(&mut self, node: u32, src: &Src, partition: Option<usize>) {
        let src = src.clone().with(&self.control);
        let other = partition.is_some_and(|p| src.reads.iter().any(|&r| r != p));
        if (src.reads.len() > 1 || other) && self.flow.is_none() {
            self.flow = Some(node);
        }
    }

    /// `node` can fail. Across partitions a coordinated participant decides
    /// whether it fails when it votes, before `commit_ts()` is known.
    fn fails(&mut self, node: u32, cause: Fallible, src: &Src) {
        self.fallible.get_or_insert((node, cause));
        if (src.ts || self.control.ts) && self.flow.is_none() {
            self.flow = Some(node);
        }
    }

    fn access(&mut self, node: u32, src: &Src) -> usize {
        let (partition, _) = self.at[&node];
        self.flow(node, src, Some(partition));
        partition
    }

    fn under<T>(&mut self, cond: &Src, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = self.control.clone();
        self.control = saved.clone().with(cond);
        let r = f(self);
        self.control = saved;
        r
    }

    fn block(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn exprs(&mut self, exprs: &[Expr]) -> Src {
        (exprs.iter()).fold(Src::default(), |s, e| {
            let e = self.expr(e);
            s.with(&e)
        })
    }

    fn stmt(&mut self, s: &Stmt) {
        let node = self.enter();
        match s {
            Stmt::Let(e) => {
                self.locals.push(Src::default());
                let local = self.locals.len() - 1;
                self.locals[local] = self.expr(e);
            }
            Stmt::If {
                cond,
                then,
                otherwise,
            } => {
                let c = self.expr(cond);
                self.under(&c, |w| {
                    w.block(then);
                    w.block(otherwise);
                });
            }
            Stmt::Require { cond, .. } => {
                let c = self.expr(cond);
                self.fails(node, Fallible::Require, &c);
            }
            Stmt::Insert { row, .. } | Stmt::Upsert { row, .. } => {
                let src = self.exprs(row);
                self.access(node, &src);
                if matches!(s, Stmt::Insert { .. }) && self.at[&node].1 {
                    self.fails(node, Fallible::Insert, &Src::default());
                }
            }
            Stmt::Update { key, set, .. } => {
                let mut src = self.exprs(key);
                for (_, e) in set {
                    src = src.with(&self.expr(e));
                }
                self.access(node, &src);
                self.fails(node, Fallible::Update, &Src::default());
            }
            Stmt::Delete { key, .. } => {
                let src = self.exprs(key);
                self.access(node, &src);
            }
            Stmt::For { scan, body } => {
                self.locals.push(Src::default());
                let local = self.locals.len() - 1;
                let mut src = self.exprs(&scan.prefix);
                for b in [&scan.lower, &scan.upper] {
                    if let Bound::Included(e) | Bound::Excluded(e) = b {
                        src = src.with(&self.expr(e));
                    }
                }
                let partition = self.access(node, &src);
                src.reads.insert(partition);
                self.locals[local] = src.clone();
                // How often the body runs depends on what the scan finds.
                self.under(&src, |w| w.block(body));
            }
            Stmt::Return(e) => {
                self.expr(e);
            }
        }
    }

    fn expr(&mut self, e: &Expr) -> Src {
        let node = self.enter();
        let src = match e {
            Expr::Literal(_) | Expr::Param(_) => Src::default(),
            Expr::Local(i) => self.locals[usize::from(*i)].clone(),
            Expr::CommitTs => Src {
                reads: BTreeSet::new(),
                ts: true,
            },
            Expr::Field(a, _) | Expr::Not(a) | Expr::IsNull(a) => self.expr(a),
            Expr::Get { key, .. } => {
                let mut src = self.exprs(key);
                let partition = self.access(node, &src);
                src.reads.insert(partition);
                src
            }
            Expr::Neg(a) | Expr::Cast(a, _) => {
                let src = self.expr(a);
                self.fails(node, Fallible::Expression, &src);
                src
            }
            Expr::UnwrapOr(a, b) => {
                let a = self.expr(a);
                a.with(&self.expr(b))
            }
            Expr::Match {
                value,
                if_null,
                present,
            } => {
                self.locals.push(Src::default());
                let local = self.locals.len() - 1;
                let v = self.expr(value);
                self.locals[local] = v.clone();
                let (a, b) = self.under(&v, |w| (w.expr(if_null), w.expr(present)));
                v.with(&a).with(&b)
            }
            Expr::Binary(op, a, b) => {
                let a = self.expr(a);
                let src = a.with(&self.expr(b));
                if matches!(
                    op,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem
                ) {
                    self.fails(node, Fallible::Expression, &src);
                }
                src
            }
        };
        if src.depends() && self.keys.contains(&std::ptr::from_ref(e)) {
            self.dynamic.insert(std::ptr::from_ref(e));
        }
        self.flow(node, &src, None);
        src
    }
}
