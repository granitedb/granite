//! Catalog objects and their validation.
//!
//! A [`Catalog`] describes a cluster's table groups and tables (document 03
//! section 1). [`Catalog::validate`] applies the rules of document 03
//! section 1.1, each with its own [`Error`] code. [`Catalog::encode`] and
//! [`Catalog::decode`] are the versioned binary form of section 1.2, and
//! [`Table::row_size_max`] is the static row bound of section 2.4.

use granite_base::constants::{FORMAT, FormatConstants};
use granite_types::key::Order;
use granite_types::storage::encode_fixed;
use granite_types::{Type, Value};
use std::collections::BTreeSet;

mod encoding;

pub use encoding::ENCODING_VERSION;

/// The tables of a cluster, with the schema version they belong to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    /// Schema version; every schema change gives a new one (document 03 section 1).
    pub version: u64,
    pub groups: Vec<TableGroup>,
}

/// Tables that share a partition key, so rows with the same partition key
/// value live in the same tablet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableGroup {
    pub name: String,
    pub partitioning: Partitioning,
    /// The columns, by name and type, every primary key in the group starts with.
    pub partition_key: Vec<Column>,
    pub tables: Vec<Table>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Partitioning {
    Hash,
    Range,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub name: String,
    pub kind: Kind,
    pub columns: Vec<Column>,
    /// Indexes into `columns`, in key order.
    pub primary_key: Vec<(u16, Order)>,
    /// `None` is a table that declared no layout, which is rejected: there is no default.
    pub layout: Option<Layout>,
    pub retention: Retention,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub ty: Type,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Immutable,
    Versioned,
}

/// How a table's columns are grouped in parts (document 01 section 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Layout {
    /// One group holding every column.
    Row,
    /// One group per column.
    Columnar,
    /// Explicit groups of column indexes; each column in exactly one.
    Groups(Vec<Vec<u16>>),
}

/// How long rows (immutable tables) or old versions (versioned tables) are kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retention {
    Forever,
    ArchiveAfterMs(u64),
    DropAfterMs(u64),
}

/// Why a catalog was rejected: one code per rule of document 03 section
/// 1.1, plus the two decoding failures. The numbers are part of the protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Error {
    /// The bytes are not a catalog encoding.
    Malformed = 1,
    /// The encoding version is not one this binary reads.
    UnknownVersion = 2,
    TooManyGroups = 3,
    TooManyTables = 4,
    TooManyColumns = 5,
    InvalidName = 6,
    DuplicateName = 7,
    InvalidType = 8,
    EmptyKey = 9,
    UnknownColumn = 10,
    RepeatedKeyColumn = 11,
    KeyColumnNotAllowed = 12,
    PartitionKeyNotPrefix = 13,
    KeyTooLarge = 14,
    RowTooLarge = 15,
    NoLayout = 16,
    LayoutGroups = 17,
    RetentionUnsupported = 18,
}

/// Bytes of a String or Bytes slot in a row: offset and length, both u32.
const SLOT: u64 = 8;

impl Catalog {
    /// Checks every rule of document 03 section 1.1; the first broken rule is returned.
    pub fn validate(&self) -> Result<(), Error> {
        let over = |n: usize, max: u64| n as u64 > max;
        if over(self.groups.len(), FORMAT.tables_max) {
            return Err(Error::TooManyGroups);
        }
        let tables = self.groups.iter().map(|g| g.tables.len()).sum();
        if over(tables, FORMAT.tables_max) {
            return Err(Error::TooManyTables);
        }
        unique(self.groups.iter().map(|g| &g.name))?;
        unique(self.groups.iter().flat_map(|g| &g.tables).map(|t| &t.name))?;
        self.groups.iter().try_for_each(TableGroup::validate)
    }
}

impl TableGroup {
    fn validate(&self) -> Result<(), Error> {
        columns(&self.partition_key)?;
        if self.partition_key.is_empty() {
            return Err(Error::EmptyKey);
        }
        // Checked here too, so that a group without tables is one a table can join.
        let mut key_size = 0;
        for c in &self.partition_key {
            key_size +=
                c.ty.key_size_max()
                    .map_err(|_| Error::KeyColumnNotAllowed)?;
        }
        if key_size > FORMAT.primary_key_size_max {
            return Err(Error::KeyTooLarge);
        }
        for t in &self.tables {
            t.validate()?;
            let key = t
                .primary_key
                .iter()
                .map(|(i, _)| &t.columns[usize::from(*i)]);
            if t.primary_key.len() < self.partition_key.len()
                || !self.partition_key.iter().zip(key).all(|(p, c)| p == c)
            {
                return Err(Error::PartitionKeyNotPrefix);
            }
        }
        Ok(())
    }
}

impl Table {
    fn validate(&self) -> Result<(), Error> {
        columns(&self.columns)?;
        let column = |i: u16| self.columns.get(usize::from(i)).ok_or(Error::UnknownColumn);
        match &self.layout {
            None => return Err(Error::NoLayout),
            Some(Layout::Groups(groups)) => {
                let mut seen = BTreeSet::new();
                for &i in groups.iter().flatten() {
                    column(i)?;
                    if !seen.insert(i) {
                        return Err(Error::LayoutGroups);
                    }
                }
                if seen.len() != self.columns.len() || groups.iter().any(Vec::is_empty) {
                    return Err(Error::LayoutGroups);
                }
            }
            Some(Layout::Row | Layout::Columnar) => {}
        }
        if self.primary_key.is_empty() {
            return Err(Error::EmptyKey);
        }
        let mut seen = BTreeSet::new();
        let mut key_size = 0;
        for &(i, _) in &self.primary_key {
            let c = column(i)?;
            if !seen.insert(i) {
                return Err(Error::RepeatedKeyColumn);
            }
            key_size +=
                c.ty.key_size_max()
                    .map_err(|_| Error::KeyColumnNotAllowed)?;
        }
        if key_size > FORMAT.primary_key_size_max {
            return Err(Error::KeyTooLarge);
        }
        if !row_fits(self.row_size_max(), &FORMAT) {
            return Err(Error::RowTooLarge);
        }
        match (self.kind, self.retention) {
            // Archiving and dropping rows need time segments (phase 7).
            (_, Retention::Forever) | (Kind::Versioned, Retention::DropAfterMs(_)) => Ok(()),
            _ => Err(Error::RetentionUnsupported),
        }
    }

    /// The static upper bound of [`Table::encode_row`]'s output, from the
    /// column types alone (document 03 section 2.4).
    pub fn row_size_max(&self) -> u64 {
        self.columns.iter().map(|c| column_size_max(&c.ty)).sum()
    }

    /// Encodes one row (document 03 section 2.4): each column in declaration
    /// order, then the heap of String and Bytes contents. The table must be
    /// part of a valid catalog.
    pub fn encode_row(&self, values: &[Value]) -> Result<Vec<u8>, granite_types::Error> {
        if values.len() != self.columns.len() {
            return Err(granite_types::Error::ColumnCount);
        }
        let (mut row, mut heap) = (Vec::new(), Vec::new());
        for (c, v) in self.columns.iter().zip(values) {
            c.ty.check(v)?;
            encode_column(&c.ty, v, &mut row, &mut heap)?;
        }
        row.extend(heap);
        assert!(row.len() as u64 <= self.row_size_max());
        Ok(row)
    }
}

/// A row fits when it is within the row limit and, with a message header,
/// within one message.
fn row_fits(size: u64, f: &FormatConstants) -> bool {
    size <= f.row_size_max && size + f.header_size <= f.message_size_max
}

/// Checks a column list: count, names, types.
fn columns(columns: &[Column]) -> Result<(), Error> {
    if columns.len() as u64 > FORMAT.columns_per_table_max {
        return Err(Error::TooManyColumns);
    }
    unique(columns.iter().map(|c| &c.name))?;
    for c in columns {
        c.ty.validate().map_err(|_| Error::InvalidType)?;
    }
    Ok(())
}

/// Names are 1 to 255 bytes and unique in their scope.
fn unique<'a>(names: impl Iterator<Item = &'a String>) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    for name in names {
        if name.is_empty() || name.len() > usize::from(u8::MAX) {
            return Err(Error::InvalidName);
        }
        if !seen.insert(name) {
            return Err(Error::DuplicateName);
        }
    }
    Ok(())
}

/// The most bytes a value of `ty` takes in a row: its storage width, or a
/// slot plus `max` heap bytes for String and Bytes.
fn column_size_max(ty: &Type) -> u64 {
    match ty {
        Type::Nullable(inner) => 1 + column_size_max(inner),
        Type::LowCardinality(inner) => column_size_max(inner),
        Type::String { max } | Type::Bytes { max } => SLOT + u64::from(*max),
        ty => u64::from(ty.fixed_width().expect("every other type is fixed-width")),
    }
}

fn encode_column(
    ty: &Type,
    value: &Value,
    row: &mut Vec<u8>,
    heap: &mut Vec<u8>,
) -> Result<(), granite_types::Error> {
    if ty.fixed_width().is_some() {
        return encode_fixed(ty, value, row);
    }
    let bytes = match (ty, value) {
        // A valid type has no Nullable under a Nullable, so the slot follows the flag.
        (Type::Nullable(_), Value::Null) => {
            row.extend([0; 1 + SLOT as usize]);
            return Ok(());
        }
        (Type::Nullable(inner), v) => {
            row.push(1);
            return encode_column(inner, v, row, heap);
        }
        (Type::LowCardinality(inner), v) => return encode_column(inner, v, row, heap),
        (_, Value::String(s)) => s.as_bytes(),
        (_, Value::Bytes(b)) => b,
        _ => return Err(granite_types::Error::Mismatch),
    };
    for n in [heap.len(), bytes.len()] {
        let n = u32::try_from(n).expect("a valid table's row is within the row limit");
        row.extend(n.to_le_bytes());
    }
    heap.extend(bytes);
    Ok(())
}

#[cfg(test)]
mod tests;
