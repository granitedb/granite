//! Transaction program representation, validator and executor.
//!
//! A [`Program`] is the typed tree of a transaction program (document 03
//! section 5). [`Program::encode`] and [`Program::decode`] are its versioned
//! binary form, specified element by element in section 5.1. The tree holds
//! only what the IR can express: there is no element for time other than
//! [`Expr::CommitTs`], for randomness, external calls, dynamic table names,
//! unbounded loops or recursion. Whether a tree is a valid program (types,
//! bounds, names) is checked by the validator, not here.

use granite_types::{Type, Value};
use std::ops::Bound;

mod encoding;

/// The version of the IR and its encoding, its first two bytes.
pub const IR_VERSION: u16 = 1;

/// A transaction program: its name, its parameters and its body.
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub name: String,
    pub params: Vec<Param>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

/// A statement. `Let`, `For` and `Expr::Match` each bind one local; locals
/// are numbered from 0 in the order their elements appear in the encoding.
#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Let(Expr),
    If {
        cond: Expr,
        then: Vec<Stmt>,
        otherwise: Vec<Stmt>,
    },
    /// Aborts the invocation with the error `code` unless `cond` holds.
    Require {
        cond: Expr,
        code: String,
    },
    /// A full row, in column order.
    Insert {
        table: String,
        row: Vec<Expr>,
    },
    /// A full row, in column order.
    Upsert {
        table: String,
        row: Vec<Expr>,
    },
    /// The row with primary key `key`, with each `(column index, value)` of
    /// `set` replaced; the other columns keep their current values.
    Update {
        table: String,
        key: Vec<Expr>,
        set: Vec<(u16, Expr)>,
    },
    Delete {
        table: String,
        key: Vec<Expr>,
    },
    /// Runs `body` once per row of `scan`, with the row bound to the next local.
    For {
        scan: Box<Scan>,
        body: Vec<Stmt>,
    },
    Return(Expr),
}

/// At most `limit` rows of `table` whose primary key starts with `prefix`,
/// with the next key column within `lower` and `upper`, in key order.
#[derive(Clone, Debug, PartialEq)]
pub struct Scan {
    pub table: String,
    pub prefix: Vec<Expr>,
    pub lower: Bound<Expr>,
    pub upper: Bound<Expr>,
    pub limit: u16,
}

/// An expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// A constant `value` of type `ty`.
    Literal(Box<(Type, Value)>),
    /// A parameter, by its index.
    Param(u8),
    /// A local, by its number.
    Local(u16),
    /// A column of a row, by its index.
    Field(Box<Expr>, u16),
    /// The row of `table` with primary key `key`, or null.
    Get {
        table: String,
        key: Vec<Expr>,
    },
    /// The commit timestamp of the transaction.
    CommitTs,
    Neg(Box<Expr>),
    Not(Box<Expr>),
    IsNull(Box<Expr>),
    /// The value, or the default when it is null.
    UnwrapOr(Box<Expr>, Box<Expr>),
    /// `if_null` when `value` is null; otherwise `present`, with the value
    /// bound to the next local.
    Match {
        value: Box<Expr>,
        if_null: Box<Expr>,
        present: Box<Expr>,
    },
    /// The value cast to the given type, an error unless it is unchanged.
    Cast(Box<Expr>, Type),
    Binary(BinOp, Box<Expr>, Box<Expr>),
}

/// The binary operators, in the order of their opcodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// Why bytes are not an IR encoding, or a tree cannot be encoded. The
/// numbers are part of the protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Error {
    /// Truncated, trailing bytes, a tag out of range, invalid UTF-8 or an
    /// invalid type; or a tree with a name over 255 bytes or more than 255
    /// parameters.
    Malformed = 1,
    /// An IR version this binary does not read.
    UnsupportedVersion = 2,
    /// A statement or expression opcode the IR does not define.
    UnknownOpcode = 3,
    /// Nesting deeper than `FORMAT.program_depth_max`: a statement of the
    /// program's body is at depth 1, and each element is one deeper than the
    /// statement or expression that contains it.
    TooDeep = 4,
    /// More than `FORMAT.program_nodes_max` statements and expressions.
    TooManyNodes = 5,
    /// An encoding longer than one message.
    TooLarge = 6,
    /// A literal whose value is not a value of its type, or not in its
    /// canonical form.
    InvalidLiteral = 7,
}
