//! The program text syntax of document 03 section 5.3: a parser to the IR
//! of `granite-txn`, a printer back to canonical text, and a report of what
//! the validator found out about a program.
//!
//! Text and IR line up node by node: every statement and expression is
//! created where its first token is, and its place is kept in the order of
//! the encoding, which is the order in which the validator numbers nodes.
//! So a validator error is reported at the text of its node.

use crate::{CompileError, Fail, Parser, Pos, Problem, Token, name, ty, utf8};
use granite_base::constants::FORMAT;
use granite_schema::{Catalog, Table};
use granite_txn::{
    AccessKind, BinOp, Class, Error, Expr, Fallible, Guard, Invalid, Local, Param, Program,
    Provenance, Report, Scan, Stmt,
};
use granite_types::arith::{self, Op};
use granite_types::{Type, Value, decimal_width};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::ops::{Bound, RangeInclusive};

/// Compiles the program text `source`, read from `file`, against `catalog`,
/// which must be valid, validates and classifies the program (document 03
/// sections 5.2 and 5.4), and checks its `on` sections and its
/// `independent` annotation. `file` is used only in errors.
pub fn compile_program(
    file: &str,
    source: &[u8],
    catalog: &Catalog,
) -> Result<Program, CompileError> {
    let fail = |pos, problem| CompileError {
        file: file.into(),
        pos,
        problem,
    };
    let text = utf8(source).map_err(|pos| fail(pos, Problem::Program(Error::Malformed)))?;
    let mut c = Compiler {
        p: Parser::new(text, true).map_err(|(pos, problem)| fail(pos, problem))?,
        tables: tables(catalog),
        scope: Vec::new(),
        locals: Vec::new(),
        bound: BTreeSet::new(),
        spans: Vec::new(),
        depth: 0,
        independent: None,
        sections: Vec::new(),
    };
    let (program, at) = c.program().map_err(|(pos, problem)| fail(pos, problem))?;
    let span = |node: u32| match node {
        0 => at,
        n => c.spans[usize::try_from(n).expect("u32 fits usize") - 1],
    };
    let invalid = |i: Invalid| fail(span(i.node), Problem::Program(i.error));
    let report = program.validate(catalog).map_err(invalid)?;
    let class = program.classify(catalog, &report).map_err(invalid)?.class;
    for (key, nodes) in &c.sections {
        let mut group = None;
        for a in (report.accesses.iter()).filter(|a| nodes.contains(&a.node)) {
            let same = a.partition_key.len() == key.len()
                && a.partition_key.iter().zip(key).all(|(x, y)| *x == y)
                && *group.get_or_insert(a.group) == a.group;
            if !same {
                return Err(fail(
                    span(a.node),
                    Problem::Text(
                        "an access in an `on` section is on its participant: its partition key is the section's names, in one table group",
                    ),
                ));
            }
        }
    }
    if let Some(at) = c.independent {
        let (pos, message) = match class {
            Class::Independent => return Ok(program),
            Class::SingleShard => (
                at,
                "the program is annotated `independent`, but all its accesses name one partition, so it is single-shard",
            ),
            Class::Coordinated { node, cause } => (
                span(node),
                match cause {
                    Fallible::Require => {
                        "`require` can fail, so a program annotated `independent` has none: this one makes it coordinated"
                    }
                    Fallible::Expression => {
                        "arithmetic, a negation or a cast can fail, so a program annotated `independent` has none: this one makes it coordinated"
                    }
                    Fallible::Insert => {
                        "an insert into a versioned table fails when its key exists, so a program annotated `independent` has none: this one makes it coordinated"
                    }
                    Fallible::Update => {
                        "`update` fails when its row does not exist, so a program annotated `independent` has none: this one makes it coordinated"
                    }
                },
            ),
        };
        return Err(fail(pos, Problem::Text(message)));
    }
    Ok(program)
}

fn tables(catalog: &Catalog) -> BTreeMap<&str, &Table> {
    let all = catalog.groups.iter().flat_map(|g| &g.tables);
    all.map(|t| (t.name.as_str(), t)).collect()
}

/// The table of the row an expression gives, when it gives one: `locals`
/// holds the table of each local bound to a row.
fn table_of<'c>(
    tables: &BTreeMap<&str, &'c Table>,
    locals: &[Option<&'c Table>],
    e: &Expr,
) -> Option<&'c Table> {
    match e {
        Expr::Get { table, .. } => tables.get(table.as_str()).copied(),
        Expr::Local(n) => locals.get(usize::from(*n)).copied().flatten(),
        Expr::Match {
            if_null, present, ..
        } => table_of(tables, locals, present).or_else(|| table_of(tables, locals, if_null)),
        Expr::UnwrapOr(a, _) => table_of(tables, locals, a),
        _ => None,
    }
}

/// Words that start an expression or join two, so a name spelled like one
/// is quoted where it is used as a value.
const RESERVED: [&str; 13] = [
    "and",
    "or",
    "not",
    "get",
    "exists",
    "is_null",
    "unwrap_or",
    "match",
    "commit_ts",
    "cast",
    "true",
    "false",
    "null",
];

/// Words that start a type, and so a literal.
const TYPES: [&str; 28] = [
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "u256",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "i256",
    "Decimal",
    "f32",
    "f64",
    "bool",
    "Date",
    "Timestamp",
    "Duration",
    "Uuid",
    "Id128",
    "String",
    "Bytes",
    "FixedBytes",
    "Enum8",
    "Enum16",
    "Nullable",
    "LowCardinality",
];

fn is_type_word(w: &str) -> bool {
    TYPES.contains(&w)
}

/// A name as written in a program: quoted where it is not a word or is a
/// word an expression gives a meaning.
fn ident(n: &str) -> String {
    if RESERVED.contains(&n) || is_type_word(n) {
        return format!("\"{n}\"");
    }
    name(n)
}

/// The operators by precedence, loosest first: `or`, `and`, `not` (2),
/// comparisons, `+ -`, `* / %`, unary `-` (6), and everything else (7).
fn precedence(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 0,
        BinOp::And => 1,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 3,
        BinOp::Add | BinOp::Sub => 4,
        BinOp::Mul | BinOp::Div | BinOp::Rem => 5,
    }
}

const OPERATORS: [(BinOp, &str); 13] = [
    (BinOp::Add, "+"),
    (BinOp::Sub, "-"),
    (BinOp::Mul, "*"),
    (BinOp::Div, "/"),
    (BinOp::Rem, "%"),
    (BinOp::Eq, "="),
    (BinOp::Ne, "!="),
    (BinOp::Lt, "<"),
    (BinOp::Le, "<="),
    (BinOp::Gt, ">"),
    (BinOp::Ge, ">="),
    (BinOp::And, "and"),
    (BinOp::Or, "or"),
];

fn symbol(op: BinOp) -> &'static str {
    OPERATORS
        .iter()
        .find(|(o, _)| *o == op)
        .expect("every operator")
        .1
}

struct Compiler<'a, 'c> {
    p: Parser<'a>,
    tables: BTreeMap<&'c str, &'c Table>,
    /// The names in scope and what they stand for, innermost last.
    scope: Vec<(String, Expr)>,
    /// The table of each local bound to a row, by local number.
    locals: Vec<Option<&'c Table>>,
    /// Every name bound to a local so far, to tell a local out of its scope
    /// from a name that is no local at all.
    bound: BTreeSet<String>,
    /// Where each statement and expression starts, in the order of the encoding.
    spans: Vec<Pos>,
    /// How deeply the parser has recursed.
    depth: u32,
    /// Where `independent` is, if the program is annotated so.
    independent: Option<Pos>,
    /// Each `on` section's participant, as partition-key expressions, and
    /// the nodes of its statements.
    sections: Vec<(Vec<Expr>, RangeInclusive<u32>)>,
}

impl<'c> Compiler<'_, 'c> {
    fn at(&self) -> Pos {
        self.p.at
    }

    fn text<T>(&self, message: &'static str) -> Result<T, Fail> {
        Err((self.at(), Problem::Text(message)))
    }

    fn punct(&mut self, punct: &'static str, expected: &'static str) -> Result<Pos, Fail> {
        self.p.eat(Token::Punct(punct), expected)
    }

    fn is_punct(&self, punct: &str) -> bool {
        matches!(self.p.token, Token::Punct(p) if p == punct)
    }

    /// Records a node at `at`, `before` the nodes from that index on: a
    /// binary operator or a field comes before its operands in the encoding.
    fn node_before(&mut self, before: usize, at: Pos) -> Result<(), Fail> {
        if u64::try_from(self.spans.len()).expect("usize fits u64") >= FORMAT.program_nodes_max {
            return Err((at, Problem::Program(Error::TooManyNodes)));
        }
        self.spans.insert(before, at);
        Ok(())
    }

    fn node(&mut self, at: Pos) -> Result<(), Fail> {
        self.node_before(self.spans.len(), at)
    }

    /// Runs `f` one level deeper. Text nested twice as deep as the IR may
    /// be, parentheses included, is rejected before the stack grows with it:
    /// the canonical text of a program within the depth limit nests at most
    /// two levels per IR level.
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, Fail>) -> Result<T, Fail> {
        if u64::from(self.depth) >= 2 * FORMAT.program_depth_max {
            return Err((self.at(), Problem::Program(Error::TooDeep)));
        }
        self.depth += 1;
        let r = f(self);
        self.depth -= 1;
        r
    }

    /// Allocates the next local; locals are numbered in the order of their
    /// binding elements, which the parser meets in the order of the encoding.
    fn local(&mut self) -> u16 {
        self.locals.push(None);
        u16::try_from(self.locals.len() - 1).expect("fewer locals than nodes")
    }

    fn bind(&mut self, name: String, local: u16) {
        self.bound.insert(name.clone());
        self.scope.push((name, Expr::Local(local)));
    }

    /// `[independent] program NAME ( [NAME : TYPE {, NAME : TYPE}] ) { statement }
    /// { on NAME {, NAME} : { statement } }`, to the end of the file.
    fn program(&mut self) -> Result<(Program, Pos), Fail> {
        if self.p.is("independent") {
            self.independent = Some(self.at());
            self.p.advance()?;
        }
        let at = self
            .p
            .keyword("program", "expected `program` at the start")?;
        let (name, _) = self.p.name()?;
        let declared = self.p.list(|p| {
            let (name, at) = p.name()?;
            p.eat(Token::Punct(":"), "expected `:` and the parameter's type")?;
            Ok((name, at, value_type(p)?))
        })?;
        let mut params = Vec::new();
        for (name, at, ty) in declared {
            let Ok(i) = u8::try_from(params.len()) else {
                return Err((at, Problem::Program(Error::Malformed)));
            };
            if self.scope.iter().any(|(n, _)| *n == name) {
                return Err((at, Problem::Text("parameter declared twice")));
            }
            self.scope.push((name.clone(), Expr::Param(i)));
            params.push(Param { name, ty });
        }
        // The statements before any `on` section: their locals stay in scope.
        let mut body = self.nested(|c| {
            let mut stmts = Vec::new();
            while !c.p.is("on") && c.p.token != Token::End {
                stmts.push(c.stmt()?);
            }
            Ok(stmts)
        })?;
        while self.p.is("on") {
            self.p.advance()?;
            let mut key = Vec::new();
            loop {
                let (name, at) = self.p.name()?;
                let e = self.scope.iter().rev().find(|(n, _)| *n == name);
                let e = e.ok_or((
                    at,
                    Problem::Text("expected a parameter or local naming the participant"),
                ))?;
                key.push(e.1.clone());
                if self.p.token != Token::Comma {
                    break;
                }
                self.p.advance()?;
            }
            self.punct(":", "expected `,` or `:` after the participant")?;
            let first = u32::try_from(self.spans.len()).expect("nodes are bounded") + 1;
            body.extend(self.block(false)?);
            let last = u32::try_from(self.spans.len()).expect("nodes are bounded");
            self.sections.push((key, first..=last));
        }
        Ok((Program { name, params, body }, at))
    }

    /// Statements up to `}` when `braced`, else to `on` or the end of the
    /// file. Locals bound here go out of scope at its end.
    fn block(&mut self, braced: bool) -> Result<Vec<Stmt>, Fail> {
        self.nested(|c| {
            let mark = c.scope.len();
            let mut stmts = Vec::new();
            let end = |c: &Self| {
                if braced {
                    c.is_punct("}")
                } else {
                    c.p.is("on")
                }
            };
            while !end(c) && c.p.token != Token::End {
                stmts.push(c.stmt()?);
            }
            c.scope.truncate(mark);
            Ok(stmts)
        })
    }

    /// `{ statements }`
    fn braced(&mut self) -> Result<Vec<Stmt>, Fail> {
        self.punct("{", "expected `{`")?;
        let stmts = self.block(true)?;
        self.punct("}", "expected a statement or `}`")?;
        Ok(stmts)
    }

    /// A table the catalog has.
    fn table(&mut self) -> Result<(&'c Table, String), Fail> {
        let (name, at) = self.p.name()?;
        let table = self.tables.get(name.as_str()).copied();
        let table = table.ok_or((at, Problem::Program(Error::UnknownTable)))?;
        Ok((table, name))
    }

    /// `[ [expr {, expr}] ]`
    fn key(&mut self) -> Result<Vec<Expr>, Fail> {
        self.punct("[", "expected `[`")?;
        let mut key = Vec::new();
        if !self.is_punct("]") {
            key.push(self.expr()?);
            while self.p.token == Token::Comma {
                self.p.advance()?;
                key.push(self.expr()?);
            }
        }
        self.punct("]", "expected `,` or `]`")?;
        Ok(key)
    }

    /// The column of `table` named at the current token.
    fn column(&mut self, table: &Table) -> Result<(u16, String, Pos), Fail> {
        let (name, at) = self.p.name()?;
        let i = table.columns.iter().position(|c| c.name == name);
        let i = i.ok_or((at, Problem::Program(Error::UnknownColumn)))?;
        Ok((
            u16::try_from(i).expect("a valid catalog's column index fits"),
            name,
            at,
        ))
    }

    fn stmt(&mut self) -> Result<Stmt, Fail> {
        let at = self.at();
        let keyword = match &self.p.token {
            Token::Word(w) => w.clone(),
            _ => return self.text("expected a statement"),
        };
        let stmt = match keyword.as_str() {
            "let" => {
                self.p.advance()?;
                self.node(at)?;
                let local = self.local();
                let (name, _) = self.p.name()?;
                self.punct("=", "expected `=`")?;
                let e = self.expr()?;
                self.locals[usize::from(local)] = table_of(&self.tables, &self.locals, &e);
                self.bind(name, local);
                Stmt::Let(e)
            }
            "if" => {
                self.p.advance()?;
                self.node(at)?;
                let cond = self.expr()?;
                let then = self.braced()?;
                let mut otherwise = Vec::new();
                if self.p.is("else") {
                    self.p.advance()?;
                    otherwise = if self.p.is("if") {
                        // `else if` is an `else` block holding one `if`.
                        self.nested(|c| Ok(vec![c.stmt()?]))?
                    } else {
                        self.braced()?
                    };
                }
                Stmt::If {
                    cond,
                    then,
                    otherwise,
                }
            }
            "require" => {
                self.p.advance()?;
                self.node(at)?;
                let cond = self.expr()?;
                self.p
                    .keyword("else", "expected `else` and an error code")?;
                let (code, _) = self.p.name()?;
                Stmt::Require { cond, code }
            }
            "insert" | "upsert" => {
                self.p.advance()?;
                self.node(at)?;
                let (table, name) = self.table()?;
                let row = self.row(table, at)?;
                if keyword == "insert" {
                    Stmt::Insert { table: name, row }
                } else {
                    Stmt::Upsert { table: name, row }
                }
            }
            "update" => {
                self.p.advance()?;
                self.node(at)?;
                let (table, name) = self.table()?;
                let key = self.key()?;
                self.punct("{", "expected `{` and the columns to set")?;
                let mut set = Vec::new();
                while !self.is_punct("}") {
                    let (column, _, _) = self.column(table)?;
                    self.punct(":", "expected `:` and the column's value")?;
                    set.push((column, self.expr()?));
                    if self.p.token != Token::Comma {
                        break;
                    }
                    self.p.advance()?;
                }
                self.punct("}", "expected `,` or `}`")?;
                Stmt::Update {
                    table: name,
                    key,
                    set,
                }
            }
            "delete" => {
                self.p.advance()?;
                self.node(at)?;
                let (_, table) = self.table()?;
                Stmt::Delete {
                    table,
                    key: self.key()?,
                }
            }
            "for" => {
                self.p.advance()?;
                self.node(at)?;
                let local = self.local();
                let (name, _) = self.p.name()?;
                self.p.keyword("in", "expected `in`")?;
                self.p.keyword("scan", "expected `scan`")?;
                let (table, table_name) = self.table()?;
                self.locals[usize::from(local)] = Some(table);
                let mut scan = self.range(table_name)?;
                self.p.keyword("limit", "expected `limit`")?;
                let (limit, limit_at) = self.p.int()?;
                let limit = limit.and_then(|n| u16::try_from(n).ok());
                scan.limit = limit.ok_or((limit_at, Problem::Text("a limit is at most 65535")))?;
                let mark = self.scope.len();
                self.bind(name, local);
                let body = self.braced()?;
                self.scope.truncate(mark);
                Stmt::For {
                    scan: Box::new(scan),
                    body,
                }
            }
            "return" => {
                self.p.advance()?;
                self.node(at)?;
                Stmt::Return(self.expr()?)
            }
            _ => return self.text("expected a statement"),
        };
        Ok(stmt)
    }

    /// `{ column [: expr] {, column [: expr]} }`, every column of `table`
    /// in declaration order; a column alone stands for the name it has.
    fn row(&mut self, table: &Table, at: Pos) -> Result<Vec<Expr>, Fail> {
        self.punct("{", "expected `{` and the row")?;
        let mut row = Vec::new();
        while !self.is_punct("}") {
            let (column, name, column_at) = self.column(table)?;
            if usize::from(column) != row.len() {
                return Err((
                    column_at,
                    Problem::Text("expected the columns of the row in declaration order"),
                ));
            }
            row.push(if self.is_punct(":") {
                self.p.advance()?;
                self.expr()?
            } else {
                self.reference(&name, column_at)?
            });
            if self.p.token != Token::Comma {
                break;
            }
            self.p.advance()?;
        }
        self.punct("}", "expected `,` or `}`")?;
        if row.len() != table.columns.len() {
            return Err((at, Problem::Program(Error::WrongArity)));
        }
        Ok(row)
    }

    /// `[ prefix {, prefix} [, > | >= lower] [, < | <= upper] ]`, after the
    /// table of a scan; the limit follows.
    fn range(&mut self, table: String) -> Result<Scan, Fail> {
        self.punct("[", "expected `[`")?;
        let (mut prefix, mut lower, mut upper) = (Vec::new(), Bound::Unbounded, Bound::Unbounded);
        if !self.is_punct("]") {
            loop {
                let bound = ["<", "<=", ">", ">="]
                    .into_iter()
                    .find(|p| self.is_punct(p));
                match bound {
                    Some(op) if matches!(upper, Bound::Unbounded) => {
                        if op.starts_with('>') && !matches!(lower, Bound::Unbounded) {
                            return self.text("expected `<` or `<=` and the upper bound");
                        }
                        self.p.advance()?;
                        let e = self.expr()?;
                        let b = if op.ends_with('=') {
                            Bound::Included(e)
                        } else {
                            Bound::Excluded(e)
                        };
                        if op.starts_with('>') {
                            lower = b;
                        } else {
                            upper = b;
                        }
                    }
                    None if matches!((&lower, &upper), (Bound::Unbounded, Bound::Unbounded)) => {
                        prefix.push(self.expr()?)
                    }
                    _ => return self.text("expected `]`: the bounds come last, lower first"),
                }
                if self.p.token != Token::Comma {
                    break;
                }
                self.p.advance()?;
            }
        }
        self.punct("]", "expected `,` or `]`")?;
        Ok(Scan {
            table,
            prefix,
            lower,
            upper,
            limit: 0,
        })
    }

    fn expr(&mut self) -> Result<Expr, Fail> {
        self.nested(|c| c.binary(0))
    }

    /// Operators of at least precedence `min`, left-associative.
    fn binary(&mut self, min: u8) -> Result<Expr, Fail> {
        let start = self.spans.len();
        let mut left = self.unary()?;
        loop {
            let op = OPERATORS.iter().find(|(_, s)| match &self.p.token {
                Token::Punct(p) => p == s,
                Token::Word(w) => w == s,
                _ => false,
            });
            let Some(&(op, _)) = op.filter(|(op, _)| precedence(*op) >= min) else {
                return Ok(left);
            };
            let at = self.at();
            self.p.advance()?;
            self.node_before(start, at)?;
            // Recursion bounded by the number of precedence levels.
            let right = self.binary(precedence(op) + 1)?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
    }

    /// `not` and unary `-`, then an operand and its fields.
    fn unary(&mut self) -> Result<Expr, Fail> {
        let at = self.at();
        if self.p.is("not") {
            self.p.advance()?;
            self.node(at)?;
            let a = self.nested(|c| c.binary(3))?;
            return Ok(Expr::Not(Box::new(a)));
        }
        if self.is_punct("-") {
            self.p.advance()?;
            self.node(at)?;
            let a = self.nested(Self::unary)?;
            return Ok(Expr::Neg(Box::new(a)));
        }
        let start = self.spans.len();
        let mut e = self.primary()?;
        while self.is_punct(".") {
            self.p.advance()?;
            let at = self.at();
            let Some(table) = table_of(&self.tables, &self.locals, &e) else {
                return Err((at, Problem::Program(Error::TypeMismatch)));
            };
            let (column, _, _) = self.column(table)?;
            self.node_before(start, at)?;
            e = Expr::Field(Box::new(e), column);
        }
        Ok(e)
    }

    /// `( expr )`, with the parentheses.
    fn args<const N: usize>(&mut self) -> Result<[Expr; N], Fail> {
        self.p.eat(Token::Open, "expected `(`")?;
        let mut args = Vec::new();
        for i in 0..N {
            if i > 0 {
                self.p.eat(Token::Comma, "expected `,`")?;
            }
            args.push(self.expr()?);
        }
        self.p.eat(Token::Close, "expected `)`")?;
        Ok(args
            .try_into()
            .unwrap_or_else(|_| unreachable!("N arguments")))
    }

    fn primary(&mut self) -> Result<Expr, Fail> {
        let at = self.at();
        let word = match &self.p.token {
            Token::Open => {
                self.p.advance()?;
                let e = self.expr()?;
                self.p.eat(Token::Close, "expected `)`")?;
                return Ok(e);
            }
            Token::Quoted(name) => {
                let name = name.clone();
                self.p.advance()?;
                return self.reference(&name, at);
            }
            Token::Word(w) => w.clone(),
            _ => return self.text("expected an expression"),
        };
        if is_type_word(&word) {
            self.node(at)?;
            let ty = value_type(&mut self.p)?;
            let value = literal(&mut self.p, &ty)?;
            return Ok(Expr::Literal(Box::new((ty, value))));
        }
        let e =
            match word.as_str() {
                "true" | "false" => {
                    self.node(at)?;
                    Expr::Literal(Box::new((Type::Bool, Value::Bool(word == "true"))))
                }
                "get" | "exists" => {
                    if word == "exists" {
                        self.node(at)?;
                        self.node(at)?;
                    }
                    self.node(at)?;
                    self.p.advance()?;
                    let (_, table) = self.table()?;
                    let get = Expr::Get {
                        table,
                        key: self.key()?,
                    };
                    return Ok(if word == "exists" {
                        Expr::Not(Box::new(Expr::IsNull(Box::new(get))))
                    } else {
                        get
                    });
                }
                "is_null" => {
                    self.node(at)?;
                    self.p.advance()?;
                    let [a] = self.args()?;
                    return Ok(Expr::IsNull(Box::new(a)));
                }
                "unwrap_or" => {
                    self.node(at)?;
                    self.p.advance()?;
                    let [a, b] = self.args()?;
                    return Ok(Expr::UnwrapOr(Box::new(a), Box::new(b)));
                }
                "commit_ts" => {
                    self.node(at)?;
                    self.p.advance()?;
                    self.args::<0>()?;
                    return Ok(Expr::CommitTs);
                }
                "cast" => {
                    self.node(at)?;
                    self.p.advance()?;
                    self.p.eat(Token::Open, "expected `(`")?;
                    let a = self.expr()?;
                    self.p.eat(Token::Comma, "expected `,` and the type")?;
                    let to = value_type(&mut self.p)?;
                    self.p.eat(Token::Close, "expected `)`")?;
                    return Ok(Expr::Cast(Box::new(a), to));
                }
                "match" => return self.matching(at),
                "null" => return self.text(
                    "expected an expression: a null literal has a type, as in `Nullable(u64) null`",
                ),
                w if RESERVED.contains(&w) => return self.text("expected an expression"),
                _ => {
                    self.p.advance()?;
                    return self.reference(&word, at);
                }
            };
        self.p.advance()?;
        Ok(e)
    }

    /// `match expr { null => expr, NAME => expr }`, at `match`.
    fn matching(&mut self, at: Pos) -> Result<Expr, Fail> {
        self.node(at)?;
        self.p.advance()?;
        let local = self.local();
        let value = self.expr()?;
        self.locals[usize::from(local)] = table_of(&self.tables, &self.locals, &value);
        self.punct("{", "expected `{`")?;
        self.p
            .keyword("null", "expected `null =>` and the value when null")?;
        self.punct("=>", "expected `=>`")?;
        let if_null = self.expr()?;
        self.p.eat(Token::Comma, "expected `,`")?;
        let (name, _) = self.p.name()?;
        self.punct("=>", "expected `=>`")?;
        let mark = self.scope.len();
        self.bind(name, local);
        let present = self.expr()?;
        self.scope.truncate(mark);
        self.punct("}", "expected `}`")?;
        Ok(Expr::Match {
            value: Box::new(value),
            if_null: Box::new(if_null),
            present: Box::new(present),
        })
    }

    /// The parameter or local a name stands for, at `at`.
    fn reference(&mut self, name: &str, at: Pos) -> Result<Expr, Fail> {
        self.node(at)?;
        if let Some((_, e)) = self.scope.iter().rev().find(|(n, _)| n == name) {
            return Ok(e.clone());
        }
        let error = if self.bound.contains(name) {
            Error::UnknownLocal
        } else {
            Error::UnknownParam
        };
        Err((at, Problem::Program(error)))
    }
}

/// A type, which must be valid (document 03 section 2).
fn value_type(p: &mut Parser<'_>) -> Result<Type, Fail> {
    let at = p.at;
    let malformed = (at, Problem::Program(Error::Malformed));
    let t = p.ty(0).map_err(|e| match e.1 {
        Problem::Rule(_) => malformed,
        _ => e,
    })?;
    t.validate().map_err(|_| malformed)?;
    Ok(t)
}

/// The type `t` without LowCardinality, and without Nullable when `strip_null`.
fn logical(t: &Type, strip_null: bool) -> &Type {
    match t {
        Type::LowCardinality(inner) => logical(inner, strip_null),
        Type::Nullable(inner) if strip_null => logical(inner, strip_null),
        t => t,
    }
}

/// The integer type whose values stand for the values of `t`, an integer,
/// decimal (its mantissa), date, time, Id128 or enum type.
fn integer_type(t: &Type) -> Option<Type> {
    Some(match t {
        Type::Decimal { precision, .. } => match decimal_width(*precision) {
            4 => Type::I32,
            8 => Type::I64,
            16 => Type::I128,
            _ => Type::I256,
        },
        Type::Date => Type::I32,
        Type::Timestamp(_) | Type::Duration(_) => Type::I64,
        Type::Id128 => Type::U128,
        Type::Enum8 { .. } => Type::U8,
        Type::Enum16 { .. } => Type::U16,
        Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::U128
        | Type::U256
        | Type::I8
        | Type::I16
        | Type::I32
        | Type::I64
        | Type::I128
        | Type::I256 => t.clone(),
        _ => return None,
    })
}

/// A value of `t` from a value of its [`integer_type`], or the other way round.
fn convert(t: &Type, v: Value, to_integer: bool) -> Value {
    use Value as V;
    match (t, v, to_integer) {
        (Type::Decimal { .. }, V::I32(m), false) => V::Decimal32(m),
        (Type::Decimal { .. }, V::I64(m), false) => V::Decimal64(m),
        (Type::Decimal { .. }, V::I128(m), false) => V::Decimal128(m),
        (Type::Decimal { .. }, V::I256(m), false) => V::Decimal256(m),
        (Type::Date, V::I32(d), false) => V::Date(d),
        (Type::Timestamp(_), V::I64(x), false) => V::Timestamp(x),
        (Type::Duration(_), V::I64(x), false) => V::Duration(x),
        (Type::Id128, V::U128(x), false) => V::Id128(x),
        (Type::Enum8 { .. }, V::U8(x), false) => V::Enum8(x),
        (Type::Enum16 { .. }, V::U16(x), false) => V::Enum16(x),
        (_, V::Decimal32(m), true) | (_, V::Date(m), true) => V::I32(m),
        (_, V::Decimal64(m), true) | (_, V::Timestamp(m), true) | (_, V::Duration(m), true) => {
            V::I64(m)
        }
        (_, V::Decimal128(m), true) => V::I128(m),
        (_, V::Decimal256(m), true) => V::I256(m),
        (_, V::Id128(x), true) => V::U128(x),
        (_, V::Enum8(x), true) => V::U8(x),
        (_, V::Enum16(x), true) => V::U16(x),
        (_, v, _) => v,
    }
}

fn small(n: u8, t: &Type) -> Value {
    arith::cast(&Type::U8, &Value::U8(n), t).expect("0 to 10 fit every integer type")
}

/// `digits`, negated when `negative`, as a value of the integer type `t`;
/// `None` when it is not decimal digits or is out of range.
fn parse_integer(t: &Type, negative: bool, digits: &str) -> Option<Value> {
    let digits = digits.trim_start_matches('0');
    // More digits than any 256-bit value has: out of range, and not worth the work.
    if digits.len() > 78 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (mut n, ten) = (small(0, t), small(10, t));
    let op = if negative { Op::Sub } else { Op::Add };
    for d in digits.bytes() {
        n = arith::binary(Op::Mul, t, &n, &ten).ok()?;
        n = arith::binary(op, t, &n, &small(d - b'0', t)).ok()?;
    }
    Some(n)
}

/// The decimal digits of a value of the integer type `t`, with a `-` when negative.
fn print_integer(t: &Type, v: &Value) -> String {
    let (zero, ten) = (small(0, t), small(10, t));
    let negative = arith::compare(t, v, &zero) == Ok(Ordering::Less);
    let (mut v, mut digits) = (v.clone(), Vec::new());
    loop {
        let r = arith::binary(Op::Rem, t, &v, &ten).expect("10 divides");
        v = arith::binary(Op::Div, t, &v, &ten).expect("10 divides");
        let Ok(Value::I8(d)) = arith::cast(t, &r, &Type::I8) else {
            unreachable!("a remainder of 10 fits an i8")
        };
        digits.push(b'0' + d.unsigned_abs());
        if arith::compare(t, &v, &zero) == Ok(Ordering::Equal) {
            break;
        }
    }
    if negative {
        digits.push(b'-');
    }
    digits.reverse();
    String::from_utf8(digits).expect("ASCII")
}

/// The value of a literal of type `t`, after its type (document 03 section 5.3).
fn literal(p: &mut Parser<'_>, t: &Type) -> Result<Value, Fail> {
    let at = p.at;
    let invalid = (at, Problem::Program(Error::InvalidLiteral));
    let v = if p.is("null") {
        p.advance()?;
        Value::Null
    } else {
        let base = logical(t, true);
        let negative = p.token == Token::Punct("-");
        if negative {
            p.advance()?;
        }
        let token = p.token.clone();
        p.advance()?;
        match (base, token) {
            (Type::F32 | Type::F64, Token::Word(w) | Token::Number(w)) => {
                if !(w == "inf" || w == "nan" || w.starts_with(|c: char| c.is_ascii_digit())) {
                    return Err(invalid);
                }
                if *base == Type::F32 {
                    let x: f32 = w.parse().map_err(|_| invalid)?;
                    Value::F32(granite_types::canonical_f32(if negative { -x } else { x }))
                } else {
                    let x: f64 = w.parse().map_err(|_| invalid)?;
                    Value::F64(granite_types::canonical_f64(if negative { -x } else { x }))
                }
            }
            (Type::Bool, Token::Word(w)) if !negative && (w == "true" || w == "false") => {
                Value::Bool(w == "true")
            }
            (Type::String { .. }, Token::Quoted(s)) if !negative => Value::String(s),
            (Type::Uuid | Type::Bytes { .. } | Type::FixedBytes(_), Token::Quoted(s))
                if !negative =>
            {
                let bytes = hex(&s).ok_or(invalid)?;
                match base {
                    Type::Uuid => Value::Uuid(bytes.try_into().map_err(|_| invalid)?),
                    Type::Bytes { .. } => Value::Bytes(bytes),
                    _ => Value::FixedBytes(bytes),
                }
            }
            (Type::Decimal { scale, .. }, Token::Number(n)) => {
                let (whole, fraction) = n.split_once('.').unwrap_or((&n, ""));
                let scale = usize::from(*scale);
                if fraction.len() > scale || (n.contains('.') && fraction.is_empty()) {
                    return Err(invalid);
                }
                let mantissa = format!("{whole}{fraction:0<scale$}");
                let it = integer_type(base).expect("a decimal has one");
                let m = parse_integer(&it, negative, &mantissa).ok_or(invalid)?;
                convert(base, m, false)
            }
            (base, Token::Number(n)) => {
                let it = integer_type(base).ok_or(invalid)?;
                convert(
                    base,
                    parse_integer(&it, negative, &n).ok_or(invalid)?,
                    false,
                )
            }
            _ => return Err(invalid),
        }
    };
    t.check(&v).map_err(|_| invalid)?;
    Ok(v)
}

fn hex(s: &str) -> Option<Vec<u8>> {
    let digits: Vec<u8> = (s.chars())
        .map(|c| c.to_digit(16).and_then(|d| u8::try_from(d).ok()))
        .collect::<Option<_>>()?;
    let pairs = digits.chunks_exact(2);
    pairs
        .remainder()
        .is_empty()
        .then(|| pairs.map(|p| p[0] << 4 | p[1]).collect())
}

/// A value of type `t` as a literal writes it after its type.
fn print_value(t: &Type, v: &Value) -> String {
    let base = logical(t, true);
    let float = |x: f64, text: String| {
        if x.is_nan() {
            "nan".into()
        } else if x.is_infinite() {
            if x > 0.0 { "inf" } else { "-inf" }.into()
        } else {
            text
        }
    };
    match v {
        Value::Null => "null".into(),
        Value::F32(x) => float(f64::from(*x), format!("{x:?}")),
        Value::F64(x) => float(*x, format!("{x:?}")),
        Value::Bool(b) => b.to_string(),
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Uuid(b) => format!("\"{}\"", to_hex(b)),
        Value::Bytes(b) | Value::FixedBytes(b) => format!("\"{}\"", to_hex(b)),
        v => {
            let it = integer_type(base).expect("an integer-like value has an integer type");
            let digits = print_integer(&it, &convert(base, v.clone(), true));
            let Type::Decimal { scale, .. } = base else {
                return digits;
            };
            let scale = usize::from(*scale);
            if scale == 0 {
                return digits;
            }
            let (sign, digits) = match digits.strip_prefix('-') {
                Some(d) => ("-", d),
                None => ("", digits.as_str()),
            };
            let digits = format!("{digits:0>width$}", width = scale + 1);
            let (whole, fraction) = digits.split_at(digits.len() - scale);
            format!("{sign}{whole}.{fraction}")
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Prints a program that validates against `catalog` and whose parameter
/// names differ as canonical text: compiling the text gives the same IR.
/// Locals are named by their numbers, `l0`, `l1` and so on, with a `_`
/// added for every parameter that starts with the same prefix.
pub fn print_program(catalog: &Catalog, program: &Program) -> String {
    let report = validated(catalog, program);
    Printer::new(catalog, program, &report).out
}

fn validated<'p>(catalog: &Catalog, program: &'p Program) -> Report<'p> {
    let report = program.validate(catalog);
    assert!(
        report.is_ok(),
        "only a valid program is printed: {report:?}"
    );
    let names: BTreeSet<&str> = program.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names.len(), program.params.len(), "parameter names differ");
    report.expect("checked")
}

/// Prints a program; afterwards it prints any of its expressions with the
/// same names.
struct Printer<'p, 'c> {
    program: &'p Program,
    tables: BTreeMap<&'c str, &'c Table>,
    /// The number of the local each `let` and `match` binds, by the address
    /// of its expression (for `match`, of the value).
    numbers: BTreeMap<*const Expr, usize>,
    /// The numbers of the locals `for` binds, in order, and how many are printed.
    rows: Vec<usize>,
    next_row: usize,
    /// The table of each local bound to a row, once printed.
    locals: Vec<Option<&'c Table>>,
    prefix: String,
    out: String,
}

impl<'p, 'c> Printer<'p, 'c> {
    fn new(catalog: &'c Catalog, program: &'p Program, report: &Report<'p>) -> Self {
        let mut prefix = String::from("l");
        while program.params.iter().any(|p| p.name.starts_with(&prefix)) {
            prefix.push('_');
        }
        let mut numbers = BTreeMap::new();
        let mut rows = Vec::new();
        for (n, local) in report.locals.iter().enumerate() {
            match local {
                Local::Let(e) | Local::Match(e) => {
                    numbers.insert(std::ptr::from_ref(*e), n);
                }
                Local::Row => rows.push(n),
            }
        }
        let mut p = Printer {
            program,
            tables: tables(catalog),
            numbers,
            rows,
            next_row: 0,
            locals: vec![None; report.locals.len()],
            prefix,
            out: String::new(),
        };
        let params: Vec<String> = (program.params.iter())
            .map(|param| format!("{}: {}", ident(&param.name), ty(&param.ty)))
            .collect();
        let header = format!("program {}({})", ident(&program.name), params.join(", "));
        p.line(0, &header);
        for s in &program.body {
            p.stmt(s, 1);
        }
        p
    }

    fn number(&self, e: &Expr) -> usize {
        self.numbers[&std::ptr::from_ref(e)]
    }

    fn local(&self, n: usize) -> String {
        format!("{}{n}", self.prefix)
    }

    fn table(&self, name: &str) -> &'c Table {
        self.tables[name]
    }

    fn column(table: &Table, i: u16) -> String {
        name(&table.columns[usize::from(i)].name)
    }

    fn list(&mut self, exprs: &[Expr]) -> String {
        let items: Vec<String> = exprs.iter().map(|e| self.expr(e, 0)).collect();
        items.join(", ")
    }

    fn line(&mut self, indent: usize, text: &str) {
        let _ = writeln!(self.out, "{:indent$}{text}", "", indent = 2 * indent);
    }

    fn block(&mut self, stmts: &[Stmt], indent: usize) {
        for s in stmts {
            self.stmt(s, indent + 1);
        }
    }

    fn stmt(&mut self, s: &Stmt, indent: usize) {
        match s {
            Stmt::Let(e) => {
                let n = self.number(e);
                let text = self.expr(e, 0);
                self.locals[n] = table_of(&self.tables, &self.locals, e);
                self.line(indent, &format!("let {} = {text}", self.local(n)));
            }
            Stmt::If { .. } => self.if_(s, indent, "if"),
            Stmt::Require { cond, code } => {
                let cond = self.expr(cond, 0);
                self.line(indent, &format!("require {cond} else {}", ident(code)));
            }
            Stmt::Insert { table, row } | Stmt::Upsert { table, row } => {
                let tb = self.table(table);
                let columns: Vec<String> = (row.iter().enumerate())
                    .map(|(i, e)| format!("{}: {}", name(&tb.columns[i].name), self.expr(e, 0)))
                    .collect();
                let keyword = if matches!(s, Stmt::Insert { .. }) {
                    "insert"
                } else {
                    "upsert"
                };
                let text = format!("{keyword} {} {{ {} }}", ident(table), columns.join(", "));
                self.line(indent, &text);
            }
            Stmt::Update { table, key, set } => {
                let tb = self.table(table);
                let key = self.list(key);
                let set: Vec<String> = (set.iter())
                    .map(|(c, e)| format!("{}: {}", Self::column(tb, *c), self.expr(e, 0)))
                    .collect();
                let text = format!("update {}[{key}] {{ {} }}", ident(table), set.join(", "));
                self.line(indent, &text);
            }
            Stmt::Delete { table, key } => {
                let key = self.list(key);
                self.line(indent, &format!("delete {}[{key}]", ident(table)));
            }
            Stmt::For { scan, body } => {
                let n = self.rows[self.next_row];
                self.next_row += 1;
                self.locals[n] = Some(self.table(&scan.table));
                let mut items: Vec<String> = scan.prefix.iter().map(|e| self.expr(e, 0)).collect();
                for (b, open, closed) in [(&scan.lower, ">", ">="), (&scan.upper, "<", "<=")] {
                    match b {
                        Bound::Included(e) => items.push(format!("{closed} {}", self.expr(e, 0))),
                        Bound::Excluded(e) => items.push(format!("{open} {}", self.expr(e, 0))),
                        Bound::Unbounded => {}
                    }
                }
                let text = format!(
                    "for {} in scan {}[{}] limit {} {{",
                    self.local(n),
                    ident(&scan.table),
                    items.join(", "),
                    scan.limit
                );
                self.line(indent, &text);
                self.block(body, indent);
                self.line(indent, "}");
            }
            Stmt::Return(e) => {
                let text = format!("return {}", self.expr(e, 0));
                self.line(indent, &text);
            }
        }
    }

    /// An `if`, after `lead` (`if` or `} else if`).
    fn if_(&mut self, s: &Stmt, indent: usize, lead: &str) {
        let Stmt::If {
            cond,
            then,
            otherwise,
        } = s
        else {
            unreachable!("an if")
        };
        let cond = self.expr(cond, 0);
        self.line(indent, &format!("{lead} {cond} {{"));
        self.block(then, indent);
        match &otherwise[..] {
            [] => self.line(indent, "}"),
            [inner @ Stmt::If { .. }] => self.if_(inner, indent, "} else if"),
            _ => {
                self.line(indent, "} else {");
                self.block(otherwise, indent);
                self.line(indent, "}");
            }
        }
    }

    /// An expression, in parentheses when its precedence is below `min`.
    fn expr(&mut self, e: &Expr, min: u8) -> String {
        let (text, precedence) = match e {
            Expr::Binary(op, a, b) => {
                let p = precedence(*op);
                let (a, b) = (self.expr(a, p), self.expr(b, p + 1));
                (format!("{a} {} {b}", symbol(*op)), p)
            }
            Expr::Not(a) => match &**a {
                Expr::IsNull(get) if matches!(**get, Expr::Get { .. }) => {
                    let Expr::Get { table, key } = &**get else {
                        unreachable!("a get")
                    };
                    (format!("exists {}[{}]", ident(table), self.list(key)), 7)
                }
                a => (format!("not {}", self.expr(a, 3)), 2),
            },
            Expr::Neg(a) => (format!("-{}", self.expr(a, 7)), 6),
            Expr::Literal(literal) => {
                let (t, v) = &**literal;
                let text = match v {
                    Value::Bool(b) if *t == Type::Bool => b.to_string(),
                    v => format!("{} {}", ty(t), print_value(t, v)),
                };
                (text, 7)
            }
            Expr::Param(i) => (ident(&self.program.params[usize::from(*i)].name), 7),
            Expr::Local(n) => (self.local(usize::from(*n)), 7),
            Expr::Field(row, c) => {
                let tb =
                    table_of(&self.tables, &self.locals, row).expect("a valid field is of a row");
                (format!("{}.{}", self.expr(row, 7), Self::column(tb, *c)), 7)
            }
            Expr::Get { table, key } => (format!("get {}[{}]", ident(table), self.list(key)), 7),
            Expr::CommitTs => ("commit_ts()".into(), 7),
            Expr::IsNull(a) => (format!("is_null({})", self.expr(a, 0)), 7),
            Expr::UnwrapOr(a, b) => {
                let (a, b) = (self.expr(a, 0), self.expr(b, 0));
                (format!("unwrap_or({a}, {b})"), 7)
            }
            Expr::Match {
                value,
                if_null,
                present,
            } => {
                let n = self.number(value);
                let v = self.expr(value, 0);
                self.locals[n] = table_of(&self.tables, &self.locals, value);
                let (a, b) = (self.expr(if_null, 0), self.expr(present, 0));
                let text = format!("match {v} {{ null => {a}, {} => {b} }}", self.local(n));
                (text, 7)
            }
            Expr::Cast(a, t) => (format!("cast({}, {})", self.expr(a, 0), ty(t)), 7),
        };
        if precedence < min {
            format!("({text})")
        } else {
            text
        }
    }
}

/// What the validator found out about a program that validates and
/// classifies against `catalog` and whose parameter names differ: its
/// canonical text, then its bounds, how each local is bound, every access
/// with its table, key provenance, partition-key expressions and the
/// conditions it runs under, outermost first (document 03 section 5.2), and
/// its class and shard set (section 5.4). Expressions use the names of the
/// canonical text.
pub fn report(catalog: &Catalog, program: &Program) -> String {
    let report = validated(catalog, program);
    let mut p = Printer::new(catalog, program, &report);
    let mut out = std::mem::take(&mut p.out);
    let returns = report.returns.as_ref().map_or("nothing".into(), ty);
    let _ = writeln!(
        out,
        "-- reads at most {}, writes at most {}, returns {returns}",
        report.reads, report.writes
    );
    for (n, local) in report.locals.iter().enumerate() {
        let what = match local {
            Local::Let(e) => p.expr(e, 0),
            Local::Match(e) => format!("the value of {} where it is not null", p.expr(e, 0)),
            Local::Row => "a scanned row".into(),
        };
        let _ = writeln!(out, "-- {} is {what}", p.local(n));
    }
    for a in &report.accesses {
        let kind = match a.kind {
            AccessKind::Get => "get",
            AccessKind::Scan => "scan",
            AccessKind::Insert => "insert",
            AccessKind::Upsert => "upsert",
            AccessKind::Update => "update",
            AccessKind::Delete => "delete",
        };
        let provenance = match a.provenance {
            Provenance::Static => "static",
            Provenance::Dependent => "dependent",
        };
        let partition: Vec<String> = a.partition_key.iter().map(|e| p.expr(e, 0)).collect();
        let mut line = format!(
            "-- {kind} {}: {provenance} key, partition ({})",
            ident(&catalog.groups[a.group].tables[a.table].name),
            partition.join(", ")
        );
        let mut guards = Vec::new();
        let mut g = a.guard;
        while let Some(i) = g {
            let Guard {
                cond,
                is_null,
                holds,
                parent,
            } = &report.guards[i];
            let cond = if *is_null {
                format!("is_null({})", p.expr(cond, 0))
            } else {
                p.expr(cond, 0)
            };
            guards.push(if *holds {
                cond
            } else {
                format!("not ({cond})")
            });
            g = *parent;
        }
        guards.reverse();
        if !guards.is_empty() {
            let _ = write!(line, ", when {}", guards.join(" and "));
        }
        let _ = writeln!(out, "{line}");
    }
    let classification = program.classify(catalog, &report);
    let classification = classification.expect("a compiled program classifies");
    let class = match classification.class {
        Class::SingleShard => "single-shard",
        Class::Independent => "independent",
        Class::Coordinated { cause, .. } => match cause {
            Fallible::Require => "coordinated: a `require` can fail",
            Fallible::Expression => "coordinated: an expression can fail",
            Fallible::Insert => "coordinated: an insert into a versioned table can fail",
            Fallible::Update => "coordinated: an `update` can fail",
        },
    };
    let partitions: Vec<String> = (classification.partitions.iter())
        .map(|(g, key)| {
            let key: Vec<String> = key.iter().map(|e| p.expr(e, 0)).collect();
            format!("{} ({})", ident(&catalog.groups[*g].name), key.join(", "))
        })
        .collect();
    let _ = writeln!(out, "-- {class}; shard set: {}", partitions.join(", "));
    out
}
