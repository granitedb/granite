//! The schema text syntax of document 03 section 1.3: [`compile`] turns a
//! schema file into the binary catalog of `granite-schema`, and [`print()`]
//! turns a catalog back into canonical text. The program text syntax of
//! section 5.3: [`compile_program`] turns a program file into the IR of
//! `granite-txn`, [`print_program`] turns a program back into canonical
//! text, and [`report`] describes what the validator found out about it.
//!
//! This is client tooling. The server never links it: it reads only the
//! binary form (`tools/crate_graph.py` keeps this crate out of `granite`).
//! The output is a function of the input bytes alone; nothing here reads a
//! file, the clock or the network.

use granite_base::constants::FORMAT;
use granite_schema::{
    At, Catalog, Column, Error, Invalid, Kind, Layout, Partitioning, Retention, Table, TableGroup,
};
use granite_types::key::Order;
use granite_types::{TimePrecision, Type};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write};

mod program;

pub use program::{compile_program, print_program, report};

/// A place in a source file. Lines and columns start at 1; a column counts characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pos {
    pub line: u32,
    pub column: u32,
}

/// Why a schema file did not compile, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    pub file: String,
    pub pos: Pos,
    pub problem: Problem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// A catalog rule of document 03 section 1.1 is broken.
    Rule(Error),
    /// A program rule of document 03 section 5.1 (codes 1 to 7), 5.2 (8 to 23) or 5.4 (from 24) is broken.
    Program(granite_txn::Error),
    /// The text does not follow the grammar, or names a group not declared before it.
    Text(&'static str),
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Pos { line, column } = self.pos;
        write!(f, "{}:{line}:{column}: ", self.file)?;
        match self.problem {
            Problem::Rule(e) => write!(f, "rule {} {e:?} (document 03 section 1.1)", e as u8),
            Problem::Program(e) => {
                let (section, why) = match e {
                    granite_txn::Error::MustBeSingleShard => (
                        4,
                        ": a program with dependent keys must be single-shard, so all its accesses name one partition, computed from the parameters",
                    ),
                    granite_txn::Error::ValueAcrossParticipants => (
                        4,
                        ": a program across partitions moves no value between its participants",
                    ),
                    granite_txn::Error::TooManyParticipants => (
                        4,
                        ": a cross-shard transaction has at most `participants_max` participants (document 04 section 4)",
                    ),
                    e if (e as u8) < 8 => (1, ""),
                    _ => (2, ""),
                };
                write!(
                    f,
                    "program rule {} {e:?} (document 03 section 5.{section}){why}",
                    e as u8
                )
            }
            Problem::Text(message) => f.write_str(message),
        }
    }
}

/// Compiles the schema text `source`, read from `file`, into the binary
/// catalog (document 03 section 1.2). `file` is used only in errors.
pub fn compile(file: &str, source: &[u8]) -> Result<Vec<u8>, CompileError> {
    let fail = |pos, problem| CompileError {
        file: file.into(),
        pos,
        problem,
    };
    let text = utf8(source).map_err(|pos| fail(pos, Problem::Rule(Error::Malformed)))?;
    let (catalog, spans) = Parser::new(text, false)
        .and_then(Parser::schema)
        .map_err(|(pos, problem)| fail(pos, problem))?;
    catalog
        .check()
        .map_err(|i| fail(locate(&spans, &i), Problem::Rule(i.error)))?;
    Ok(catalog.encode().expect("a checked catalog encodes"))
}

/// The text, or the place of its first byte that is not UTF-8.
fn utf8(source: &[u8]) -> Result<&str, Pos> {
    std::str::from_utf8(source).map_err(|e| {
        let valid = std::str::from_utf8(&source[..e.valid_up_to()]).expect("checked prefix");
        let line_start = valid.rfind('\n').map_or(0, |i| i + 1);
        Pos {
            line: count(valid.matches('\n').count() + 1),
            column: count(valid[line_start..].chars().count() + 1),
        }
    })
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

type Fail = (Pos, Problem);

/// Where each part of each group and table was written, in the shape of the catalog.
struct GroupSpans {
    name: Pos,
    key: Pos,
    columns: Vec<Pos>,
    tables: Vec<TableSpans>,
}

struct TableSpans {
    name: Pos,
    columns: Vec<Pos>,
    key: Pos,
    key_columns: Vec<Pos>,
    layout: Pos,
    retention: Pos,
}

fn locate(spans: &[GroupSpans], i: &Invalid) -> Pos {
    let g = &spans[i.group];
    let Some(t) = i.table else {
        return match i.at {
            At::Column(c) => g.columns[c],
            At::Key => g.key,
            _ => g.name,
        };
    };
    let t = &g.tables[t];
    match i.at {
        At::Name => t.name,
        At::Column(c) => t.columns[c],
        At::Key => t.key,
        At::KeyColumn(k) => t.key_columns[k],
        At::Layout => t.layout,
        At::Retention => t.retention,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    /// An unquoted name or keyword.
    Word(String),
    /// A quoted name, unescaped.
    Quoted(String),
    /// Digits as written; in program text also letters, `.` and an
    /// exponent's sign, which only a literal's type gives a meaning.
    Number(String),
    Open,
    Close,
    Comma,
    /// Program text only: `[ ] { } : . = != < <= > >= => + - * / %`.
    Punct(&'static str),
    End,
}

/// A recursive-descent parser over a one-token lookahead. In schema text
/// only types recurse, at most [`WRAPPERS_MAX`] deep, so the work is linear
/// in the input apart from the ordered maps of names.
struct Parser<'a> {
    /// Whether this is program text, which has more punctuation and numbers.
    program: bool,
    rest: std::iter::Peekable<std::str::Chars<'a>>,
    line: u32,
    column: u32,
    token: Token,
    /// Where `token` starts.
    at: Pos,
}

/// Wrappers a type can nest; any deeper type repeats one and is invalid
/// (document 03 section 1.2).
const WRAPPERS_MAX: u8 = 3;

impl<'a> Parser<'a> {
    fn new(text: &'a str, program: bool) -> Result<Self, Fail> {
        let mut p = Parser {
            program,
            rest: text.chars().peekable(),
            line: 1,
            column: 1,
            token: Token::End,
            at: Pos { line: 1, column: 1 },
        };
        p.advance()?;
        Ok(p)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.rest.next()?;
        if c == '\n' {
            self.line = self.line.saturating_add(1);
            self.column = 1;
        } else {
            self.column = self.column.saturating_add(1);
        }
        Some(c)
    }

    fn here(&self) -> Pos {
        Pos {
            line: self.line,
            column: self.column,
        }
    }

    /// Reads the next token into `self.token`.
    fn advance(&mut self) -> Result<(), Fail> {
        loop {
            match self.rest.peek() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('-') => {
                    let at = self.here();
                    self.bump();
                    if self.rest.peek() != Some(&'-') {
                        if self.program {
                            (self.at, self.token) = (at, Token::Punct("-"));
                            return Ok(());
                        }
                        return Err((at, Problem::Text("expected `--` to start a comment")));
                    }
                    while self.rest.peek().is_some_and(|&c| c != '\n') {
                        self.bump();
                    }
                }
                _ => break,
            }
        }
        self.at = self.here();
        let word = |c: &char| c.is_ascii_alphanumeric() || *c == '_';
        self.token = match self.rest.peek().copied() {
            None => Token::End,
            Some(c @ ('(' | ')' | ',')) => {
                self.bump();
                match c {
                    '(' => Token::Open,
                    ')' => Token::Close,
                    _ => Token::Comma,
                }
            }
            Some(c) if c.is_ascii_digit() => {
                let mut s = String::new();
                loop {
                    let c = self.rest.peek().copied();
                    let more = match c {
                        Some(c) if self.program => {
                            word(&c)
                                || c == '.'
                                || (matches!(c, '+' | '-') && s.ends_with(['e', 'E']))
                        }
                        Some(c) => c.is_ascii_digit(),
                        None => false,
                    };
                    if !more {
                        break;
                    }
                    self.bump();
                    s.extend(c);
                }
                if !self.program && self.rest.peek().is_some_and(word) {
                    return Err((self.at, Problem::Text("expected a number")));
                }
                Token::Number(s)
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let mut s = String::new();
                while let Some(c) = self.rest.peek().copied().filter(word) {
                    self.bump();
                    s.push(c);
                }
                Token::Word(s)
            }
            Some('"') => {
                self.bump();
                let mut s = String::new();
                loop {
                    match self.bump() {
                        None => return Err((self.at, Problem::Text("unterminated quoted name"))),
                        Some('"') => break,
                        Some('\\') => match self.bump() {
                            Some(c @ ('"' | '\\')) => s.push(c),
                            _ => {
                                let problem =
                                    Problem::Text("expected `\\\"` or `\\\\` in a quoted name");
                                return Err((self.at, problem));
                            }
                        },
                        Some(c) => s.push(c),
                    }
                }
                Token::Quoted(s)
            }
            Some(c) if self.program && "[]{}:.=!<>+*/%".contains(c) => {
                self.bump();
                let next = self.rest.peek().copied();
                let two = match (c, next) {
                    ('!', Some('=')) => "!=",
                    ('<', Some('=')) => "<=",
                    ('>', Some('=')) => ">=",
                    ('=', Some('>')) => "=>",
                    _ => "",
                };
                if two.is_empty() {
                    let one = [
                        "[", "]", "{", "}", ":", ".", "=", "<", ">", "+", "*", "/", "%",
                    ];
                    let one = one.into_iter().find(|p| p.starts_with(c));
                    Token::Punct(one.ok_or((self.at, Problem::Text("unexpected character")))?)
                } else {
                    self.bump();
                    Token::Punct(two)
                }
            }
            Some(_) => return Err((self.at, Problem::Text("unexpected character"))),
        };
        Ok(())
    }

    fn is(&self, keyword: &str) -> bool {
        matches!(&self.token, Token::Word(w) if w == keyword)
    }

    /// Consumes `token`, or fails with `expected`.
    fn eat(&mut self, token: Token, expected: &'static str) -> Result<Pos, Fail> {
        if self.token != token {
            return Err((self.at, Problem::Text(expected)));
        }
        let at = self.at;
        self.advance()?;
        Ok(at)
    }

    fn keyword(&mut self, keyword: &'static str, expected: &'static str) -> Result<Pos, Fail> {
        self.eat(Token::Word(keyword.into()), expected)
    }

    /// Consumes the first of `choices` that is the current word and returns its index.
    fn choice(&mut self, choices: &[&str], expected: &'static str) -> Result<usize, Fail> {
        let i = choices.iter().position(|c| self.is(c));
        let i = i.ok_or((self.at, Problem::Text(expected)))?;
        self.advance()?;
        Ok(i)
    }

    fn name(&mut self) -> Result<(String, Pos), Fail> {
        let at = self.at;
        let (Token::Word(name) | Token::Quoted(name)) = &self.token else {
            return Err((at, Problem::Text("expected a name")));
        };
        let name = name.clone();
        self.advance()?;
        Ok((name, at))
    }

    /// A number, `None` when it does not fit a u64.
    fn int(&mut self) -> Result<(Option<u64>, Pos), Fail> {
        let at = self.at;
        let n = match &self.token {
            Token::Number(s) if s.bytes().all(|b| b.is_ascii_digit()) => s.parse().ok(),
            _ => return Err((at, Problem::Text("expected a number"))),
        };
        self.advance()?;
        Ok((n, at))
    }

    fn u64(&mut self) -> Result<u64, Fail> {
        let (n, at) = self.int()?;
        n.ok_or((at, Problem::Text("number above 2^64 - 1")))
    }

    /// `( [item {, item}] )`.
    fn list<T>(
        &mut self,
        mut item: impl FnMut(&mut Self) -> Result<T, Fail>,
    ) -> Result<Vec<T>, Fail> {
        self.eat(Token::Open, "expected `(`")?;
        let mut items = Vec::new();
        if self.token != Token::Close {
            items.push(item(self)?);
            while self.token == Token::Comma {
                self.advance()?;
                items.push(item(self)?);
            }
        }
        self.eat(Token::Close, "expected `,` or `)`")?;
        Ok(items)
    }

    /// `schema version N { group | table }`
    fn schema(mut self) -> Result<(Catalog, Vec<GroupSpans>), Fail> {
        self.keyword("schema", "expected `schema version` at the start")?;
        self.keyword("version", "expected `version`")?;
        let version = self.u64()?;
        let mut catalog = Catalog {
            version,
            groups: Vec::new(),
        };
        let mut spans = Vec::new();
        let mut group_names = BTreeMap::new();
        while self.token != Token::End {
            self.keyword("table", "expected `table` or the end of the file")?;
            if self.is("group") {
                self.advance()?;
                let (group, span) = self.group()?;
                group_names
                    .entry(group.name.clone())
                    .or_insert(catalog.groups.len());
                catalog.groups.push(group);
                spans.push(span);
            } else {
                let (table, span, group) = self.table()?;
                let g = *group_names.get(&group.0).ok_or((
                    group.1,
                    Problem::Text("unknown group; a group is declared before its tables"),
                ))?;
                catalog.groups[g].tables.push(table);
                spans[g].tables.push(span);
            }
        }
        Ok((catalog, spans))
    }

    /// `NAME partition by ( [column {, column}] ) (hash | range)`, after `table group`.
    fn group(&mut self) -> Result<(TableGroup, GroupSpans), Fail> {
        let (name, name_at) = self.name()?;
        self.keyword("partition", "expected `partition by`")?;
        self.keyword("by", "expected `by`")?;
        let key = self.at;
        let columns = self.list(Self::column)?;
        let partitioning = match self.choice(&["hash", "range"], "expected `hash` or `range`")? {
            0 => Partitioning::Hash,
            _ => Partitioning::Range,
        };
        let (partition_key, columns) = columns.into_iter().unzip();
        let group = TableGroup {
            name,
            partitioning,
            partition_key,
            tables: Vec::new(),
        };
        let spans = GroupSpans {
            name: name_at,
            key,
            columns,
            tables: Vec::new(),
        };
        Ok((group, spans))
    }

    /// `NAME KIND [layout LAYOUT] in GROUP key ( [key column {, key column}] )
    /// column {, column} [retention RETENTION]`, after `table`. Returns the
    /// group's name and place too.
    fn table(&mut self) -> Result<(Table, TableSpans, (String, Pos)), Fail> {
        let (name, name_at) = self.name()?;
        let kind = match self.choice(
            &["immutable", "versioned"],
            "expected `immutable` or `versioned`",
        )? {
            0 => Kind::Immutable,
            _ => Kind::Versioned,
        };
        let mut layout_at = name_at;
        let (mut layout, mut groups) = (None, Vec::new());
        if self.is("layout") {
            layout_at = self.at;
            self.advance()?;
            let expected = "expected `row`, `columnar` or `groups`";
            layout = Some(
                match self.choice(&["row", "columnar", "groups"], expected)? {
                    0 => Layout::Row,
                    1 => Layout::Columnar,
                    _ => {
                        // Column names, resolved once the columns are known.
                        groups = self.list(|p| p.list(Self::name))?;
                        Layout::Groups(Vec::new())
                    }
                },
            );
        }
        self.keyword("in", "expected `in` and the table's group")?;
        let group = self.name()?;
        let key_at = self.keyword("key", "expected `key`")?;
        let key = self.list(|p| {
            let name = p.name()?;
            let desc = p.is("desc");
            if desc || p.is("asc") {
                p.advance()?;
            }
            Ok((name, if desc { Order::Desc } else { Order::Asc }))
        })?;
        let mut columns = vec![self.column()?];
        while self.token == Token::Comma {
            self.advance()?;
            columns.push(self.column()?);
        }
        let mut retention_at = name_at;
        let mut retention = Retention::Forever;
        if self.is("retention") {
            retention_at = self.at;
            self.advance()?;
            let what = self.choice(
                &["forever", "archive", "drop"],
                "expected `forever`, `archive after` or `drop after`",
            )?;
            if what > 0 {
                self.keyword("after", "expected `after`")?;
                let ms = self.u64()?;
                self.keyword("ms", "expected `ms`")?;
                retention = if what == 1 {
                    Retention::ArchiveAfterMs(ms)
                } else {
                    Retention::DropAfterMs(ms)
                };
            }
        }
        let (columns, column_spans): (Vec<Column>, Vec<Pos>) = columns.into_iter().unzip();

        // Names to column indexes. Too many columns for a u16 index is the column limit.
        let max = FORMAT.columns_per_table_max as usize;
        if let Some(&at) = column_spans.get(max) {
            return Err((at, Problem::Rule(Error::TooManyColumns)));
        }
        let mut index = BTreeMap::new();
        for (i, c) in columns.iter().enumerate() {
            index.entry(c.name.as_str()).or_insert(i as u16);
        }
        let resolve = |(name, at): &(String, Pos)| {
            let i = index.get(name.as_str()).copied();
            i.ok_or((*at, Problem::Rule(Error::UnknownColumn)))
        };
        if let Some(Layout::Groups(indexes)) = &mut layout {
            *indexes = (groups.iter())
                .map(|g| g.iter().map(resolve).collect())
                .collect::<Result<_, _>>()?;
        }
        let primary_key = (key.iter())
            .map(|(name, order)| Ok((resolve(name)?, *order)))
            .collect::<Result<_, Fail>>()?;
        let table = Table {
            name,
            kind,
            columns,
            primary_key,
            layout,
            retention,
        };
        let spans = TableSpans {
            name: name_at,
            columns: column_spans,
            key: key_at,
            key_columns: key.iter().map(|((_, at), _)| *at).collect(),
            layout: layout_at,
            retention: retention_at,
        };
        Ok((table, spans, group))
    }

    /// `NAME TYPE`
    fn column(&mut self) -> Result<(Column, Pos), Fail> {
        let (name, at) = self.name()?;
        let ty = self.ty(0)?;
        Ok((Column { name, ty }, at))
    }

    /// A type as in document 03 section 2; `depth` wrappers enclose it.
    fn ty(&mut self, depth: u8) -> Result<Type, Fail> {
        let at = self.at;
        let invalid = (at, Problem::Rule(Error::InvalidType));
        let Token::Word(word) = &self.token else {
            return Err((at, Problem::Text("expected a type")));
        };
        let word = word.clone();
        self.advance()?;
        // One number in parentheses, which must fit `T`.
        fn param<T: TryFrom<u64>>(p: &mut Parser<'_>, invalid: Fail) -> Result<T, Fail> {
            let (n, _) = p.int()?;
            n.and_then(|n| T::try_from(n).ok()).ok_or(invalid)
        }
        let one = |p: &mut Self| -> Result<u64, Fail> {
            p.eat(Token::Open, "expected `(`")?;
            let n = param(p, invalid)?;
            p.eat(Token::Close, "expected `)`")?;
            Ok(n)
        };
        let precision = |p: &mut Self| match one(p)? {
            0 => Ok(TimePrecision::Seconds),
            3 => Ok(TimePrecision::Millis),
            6 => Ok(TimePrecision::Micros),
            9 => Ok(TimePrecision::Nanos),
            _ => Err(invalid),
        };
        let u32 = |p: &mut Self| u32::try_from(one(p)?).map_err(|_| invalid);
        // The number of values: a count, or the declared names.
        let values = |p: &mut Self| -> Result<u64, Fail> {
            p.eat(Token::Open, "expected `(`")?;
            let n = if matches!(p.token, Token::Number(_)) {
                param(p, invalid)?
            } else {
                let mut seen = BTreeSet::new();
                loop {
                    let (name, at) = p.name()?;
                    if !seen.insert(name) {
                        return Err((at, Problem::Text("enum value declared twice")));
                    }
                    if p.token != Token::Comma {
                        break;
                    }
                    p.advance()?;
                }
                seen.len() as u64
            };
            p.eat(Token::Close, "expected `,` or `)`")?;
            Ok(n)
        };
        let max = |n: u64| n.checked_sub(1).ok_or(invalid);
        let wrapped = |p: &mut Self| -> Result<Box<Type>, Fail> {
            if depth == WRAPPERS_MAX {
                return Err(invalid);
            }
            p.eat(Token::Open, "expected `(`")?;
            let inner = p.ty(depth + 1)?;
            p.eat(Token::Close, "expected `)`")?;
            Ok(Box::new(inner))
        };
        Ok(match word.as_str() {
            "u8" => Type::U8,
            "u16" => Type::U16,
            "u32" => Type::U32,
            "u64" => Type::U64,
            "u128" => Type::U128,
            "u256" => Type::U256,
            "i8" => Type::I8,
            "i16" => Type::I16,
            "i32" => Type::I32,
            "i64" => Type::I64,
            "i128" => Type::I128,
            "i256" => Type::I256,
            "Decimal" => {
                self.eat(Token::Open, "expected `(`")?;
                let precision = param(self, invalid)?;
                self.eat(Token::Comma, "expected `,`")?;
                let scale = param(self, invalid)?;
                self.eat(Token::Close, "expected `)`")?;
                Type::Decimal { precision, scale }
            }
            "f32" => Type::F32,
            "f64" => Type::F64,
            "bool" => Type::Bool,
            "Date" => Type::Date,
            "Timestamp" => Type::Timestamp(precision(self)?),
            "Duration" => Type::Duration(precision(self)?),
            "Uuid" => Type::Uuid,
            "Id128" => Type::Id128,
            "String" => Type::String { max: u32(self)? },
            "Bytes" => Type::Bytes { max: u32(self)? },
            "FixedBytes" => Type::FixedBytes(u32(self)?),
            "Enum8" => Type::Enum8 {
                max: u8::try_from(max(values(self)?)?).map_err(|_| invalid)?,
            },
            "Enum16" => Type::Enum16 {
                max: u16::try_from(max(values(self)?)?).map_err(|_| invalid)?,
            },
            "Nullable" => Type::Nullable(wrapped(self)?),
            "LowCardinality" => Type::LowCardinality(wrapped(self)?),
            _ => return Err((at, Problem::Text("unknown type"))),
        })
    }
}

/// Prints a valid catalog as canonical text: compiling the text gives the
/// catalog's encoding again. Comments, layout of the text and enum value
/// names are not part of the catalog, so enums print as their value count.
pub fn print(catalog: &Catalog) -> String {
    assert_eq!(
        catalog.validate(),
        Ok(()),
        "only a valid catalog is printed"
    );
    let mut s = String::new();
    let columns = |columns: &[Column], sep: &str| {
        let c = columns
            .iter()
            .map(|c| format!("{} {}", name(&c.name), ty(&c.ty)));
        c.collect::<Vec<_>>().join(sep)
    };
    // Writing to a String does not fail.
    let _ = writeln!(s, "schema version {}", catalog.version);
    for g in &catalog.groups {
        let partitioning = match g.partitioning {
            Partitioning::Hash => "hash",
            Partitioning::Range => "range",
        };
        let key = columns(&g.partition_key, ", ");
        let _ = writeln!(
            s,
            "\ntable group {} partition by ({key}) {partitioning}",
            name(&g.name)
        );
        for t in &g.tables {
            let kind = match t.kind {
                Kind::Immutable => "immutable",
                Kind::Versioned => "versioned",
            };
            let column = |i: &u16| name(&t.columns[usize::from(*i)].name);
            let layout = match t.layout.as_ref().expect("a valid table has a layout") {
                Layout::Row => "row".into(),
                Layout::Columnar => "columnar".into(),
                Layout::Groups(groups) => {
                    let groups: Vec<String> = (groups.iter())
                        .map(|g| {
                            format!("({})", g.iter().map(column).collect::<Vec<_>>().join(", "))
                        })
                        .collect();
                    format!("groups({})", groups.join(", "))
                }
            };
            let key: Vec<String> = (t.primary_key.iter())
                .map(|(i, order)| match order {
                    Order::Asc => column(i),
                    Order::Desc => format!("{} desc", column(i)),
                })
                .collect();
            let _ = writeln!(
                s,
                "\ntable {} {kind} layout {layout} in {}",
                name(&t.name),
                name(&g.name)
            );
            let _ = writeln!(s, "  key ({})", key.join(", "));
            let _ = writeln!(s, "  {}", columns(&t.columns, ",\n  "));
            match t.retention {
                Retention::Forever => {}
                Retention::ArchiveAfterMs(ms) => {
                    let _ = writeln!(s, "  retention archive after {ms} ms");
                }
                Retention::DropAfterMs(ms) => {
                    let _ = writeln!(s, "  retention drop after {ms} ms");
                }
            }
        }
    }
    s
}

/// A name as written: bare when it lexes as one word and is not `group`
/// (which after `table` starts a group), quoted otherwise.
fn name(name: &str) -> String {
    let bare = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name != "group";
    if bare {
        return name.into();
    }
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

fn ty(t: &Type) -> String {
    let precision = |p: &TimePrecision| match p {
        TimePrecision::Seconds => 0,
        TimePrecision::Millis => 3,
        TimePrecision::Micros => 6,
        TimePrecision::Nanos => 9,
    };
    match t {
        Type::U8 => "u8".into(),
        Type::U16 => "u16".into(),
        Type::U32 => "u32".into(),
        Type::U64 => "u64".into(),
        Type::U128 => "u128".into(),
        Type::U256 => "u256".into(),
        Type::I8 => "i8".into(),
        Type::I16 => "i16".into(),
        Type::I32 => "i32".into(),
        Type::I64 => "i64".into(),
        Type::I128 => "i128".into(),
        Type::I256 => "i256".into(),
        Type::Decimal { precision, scale } => format!("Decimal({precision}, {scale})"),
        Type::F32 => "f32".into(),
        Type::F64 => "f64".into(),
        Type::Bool => "bool".into(),
        Type::Date => "Date".into(),
        Type::Timestamp(p) => format!("Timestamp({})", precision(p)),
        Type::Duration(p) => format!("Duration({})", precision(p)),
        Type::Uuid => "Uuid".into(),
        Type::Id128 => "Id128".into(),
        Type::String { max } => format!("String({max})"),
        Type::Bytes { max } => format!("Bytes({max})"),
        Type::FixedBytes(n) => format!("FixedBytes({n})"),
        Type::Enum8 { max } => format!("Enum8({})", u32::from(*max) + 1),
        Type::Enum16 { max } => format!("Enum16({})", u32::from(*max) + 1),
        Type::Nullable(inner) => format!("Nullable({})", ty(inner)),
        Type::LowCardinality(inner) => format!("LowCardinality({})", ty(inner)),
    }
}
