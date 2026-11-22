//! Scan fragment representation and vectorised operators.
//!
//! A [`Fragment`] is the only analytical request a shard accepts (document
//! 05 section 4): a [`Scan`] of one table followed by a pipeline of
//! [`Operator`]s. [`Fragment::encode`] and [`Fragment::decode`] are its
//! versioned binary form, whose expressions share their opcodes with the
//! program IR (`granite_types::expr`). [`Fragment::validate`] checks a tree
//! against a catalog and computes the static memory bound of every
//! operator, rejecting a fragment without one or over the limit. The tree
//! has no element for a join, a subquery, a sort of unbounded size or a
//! write.

use granite_types::key::Order;
use granite_types::{Type, Value};
use std::ops::Bound;

mod encoding;
mod validate;

pub use granite_types::expr::BinOp;
pub use validate::{Invalid, Report};

/// The version of the fragment IR and its encoding, its first two bytes.
pub const FRAGMENT_VERSION: u16 = 1;

/// A scan and the operators its rows pass through, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Fragment {
    pub scan: Scan,
    pub operators: Vec<Operator>,
}

/// The rows of `table` within `ranges` (all of them when there is none)
/// whose commit timestamp is within `time`, both ends included, that satisfy
/// `predicate`; each as the values of `columns`, in that order.
#[derive(Clone, Debug, PartialEq)]
pub struct Scan {
    pub table: String,
    /// Column indexes of the table. Empty is a scan without a column list,
    /// which the validator rejects.
    pub columns: Vec<u16>,
    pub ranges: Vec<KeyRange>,
    pub time: (u64, u64),
    /// A bool over the scan's columns.
    pub predicate: Option<Expr>,
}

/// Rows whose primary key, compared in key order on its first values, is
/// within the bounds. A bound gives the first one or more key values.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyRange {
    pub lower: Bound<Vec<Literal>>,
    pub upper: Bound<Vec<Literal>>,
}

/// A constant: a type and a value of it.
pub type Literal = (Type, Value);

/// An operator over the rows of the one before it; [`Expr::Column`] names
/// a column of those rows.
#[derive(Clone, Debug, PartialEq)]
pub enum Operator {
    /// Each row as the values of the expressions.
    Project(Vec<Expr>),
    /// Rows grouped by the values of `keys`, each group as its keys then
    /// its aggregates. At most `groups` groups are held; when the table is
    /// full, partial groups are flushed downstream, so a group may appear
    /// more than once. Zero is no bound, which the validator rejects.
    PartialAggregate {
        keys: Vec<Expr>,
        aggregates: Vec<Aggregate>,
        groups: u32,
    },
    /// The first `n` rows in the order of `order`. Zero is no n, which the
    /// validator rejects.
    TopN { order: Vec<(Expr, Order)>, n: u32 },
    /// The first `n` rows.
    Limit(u64),
}

/// An aggregate of a group's rows.
#[derive(Clone, Debug, PartialEq)]
pub enum Aggregate {
    /// The number of rows, a u64.
    Count,
    Sum(Expr),
    Min(Expr),
    Max(Expr),
}

/// An expression over one row of an operator's input.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// A constant.
    Literal(Box<Literal>),
    /// A column of the input row, by its index.
    Column(u16),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    IsNull(Box<Expr>),
    /// The value, or the default when it is null.
    UnwrapOr(Box<Expr>, Box<Expr>),
    /// The value cast to the given type, an error unless it is unchanged.
    Cast(Box<Expr>, Type),
    Binary(BinOp, Box<Expr>, Box<Expr>),
}

/// Why bytes are not a fragment encoding (1 to 7), or a tree is not a valid
/// fragment (from 8, the validator). The numbers are part of the protocol;
/// document 05 section 4 gives the rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Error {
    /// Truncated, trailing bytes, a tag out of range, invalid UTF-8 or an
    /// invalid type; or a tree with a name over 255 bytes or more than 255
    /// operators.
    Malformed = 1,
    /// A fragment IR version this binary does not read.
    UnsupportedVersion = 2,
    /// An operator, aggregate or expression opcode the IR does not define.
    UnknownOpcode = 3,
    /// Expressions nested deeper than `FORMAT.program_depth_max`.
    TooDeep = 4,
    /// More elements than `FORMAT.program_nodes_max`.
    TooManyNodes = 5,
    /// An encoding longer than one message.
    TooLarge = 6,
    /// A literal whose value is not a value of its type, or not in its
    /// canonical form.
    InvalidLiteral = 7,
    /// A table the catalog does not have.
    UnknownTable = 8,
    /// A column index past the table's columns or the input's.
    UnknownColumn = 9,
    /// A value of a type where another is needed, with no cast between
    /// them, or an operator applied to a type it does not take.
    TypeMismatch = 10,
    /// Two different numeric types meet without an explicit cast.
    ImplicitCast = 11,
    /// A nullable value where a value is needed.
    NullableOperand = 12,
    /// A key bound with no value or more values than the primary key.
    WrongArity = 13,
    /// A scan without a column list.
    NoColumns = 14,
    /// A partial aggregate without a group-table bound.
    NoGroupBound = 15,
    /// A top-N without n.
    NoTopN = 16,
    /// An operator after a partial aggregate, whose output may repeat a group.
    AfterAggregate = 17,
    /// A memory bound over the limit.
    OverMemoryLimit = 18,
}
