//! The reference model (document 06 section 1, "Checkers"): a small,
//! obviously correct in-memory implementation of tables and the operations
//! of E1.5 (insert, get and scan), which every model test and checker
//! compares the engine with.
//!
//! It is written for clarity, not speed: a table is a `BTreeMap` from the
//! encoded primary key to the row. It shares no code with the engine beyond
//! the type definitions: values, their checks and the key encoding of
//! `granite_types`, the catalog of `granite_schema` and the error codes of
//! `granite_protocol`. `tools/lint.py` rejects an import of the storage,
//! lsm, columnar or shard crate anywhere in this directory.
//!
//! [`workload`] generates random valid and invalid operations for a catalog,
//! and [`harness`] applies them to the model and to an [`Engine`] and
//! compares every result.

pub mod harness;
pub mod workload;

use granite_protocol::Error;
use granite_schema::{Catalog, Kind, Table};
use granite_types::key::{Order, encode_key};
use granite_types::{Type, Value, canonical_f32, canonical_f64};
use std::collections::BTreeMap;

/// A table: its index in the catalog, counting every group's tables in order.
pub type TableId = u32;

/// One operation and its events, as a client sends it. Values are typed, and
/// an engine adapter carries them to the engine in their wire form. Every
/// invalid value an [`Op`] may hold can be sent in a row batch: a String or
/// Bytes content longer than its max, an enum index above its max, or a
/// decimal mantissa of more than P digits.
#[derive(Clone, Debug)]
pub enum Op {
    /// Rows of one table, each with every column in declaration order.
    Insert {
        table: TableId,
        rows: Vec<Vec<Value>>,
    },
    /// Primary keys, each with every key column in key order, and the
    /// columns to return, in the order to return them.
    Get {
        table: TableId,
        keys: Vec<Vec<Value>>,
        columns: Vec<u16>,
    },
    /// The first `limit` rows from `start` to `end`, in key order. Each bound
    /// is a prefix of a primary key, the partition key at least, and is
    /// inclusive: a row is in range when its key is at or after `start` and
    /// at or before `end`, or begins with `end`.
    Scan {
        table: TableId,
        start: Vec<Value>,
        end: Vec<Value>,
        limit: u32,
        columns: Vec<u16>,
    },
}

/// The result of one [`Op`].
#[derive(Clone, Debug)]
pub enum Outcome {
    /// The events that did not succeed, in event order (the sparse result
    /// list of document 05 section 1); every other event inserted its row.
    Inserted(Vec<(u32, Duplicate)>),
    /// Per key, in request order, the requested columns of its row, or
    /// `None` when no row has the key.
    Got(Vec<Option<Vec<Value>>>),
    /// The requested columns of each row in range, in key order.
    Scanned(Vec<Vec<Value>>),
    /// The whole operation was rejected and changed nothing.
    Rejected(Reject),
}

/// Why an insert event did not insert its row (document 03 section 9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Duplicate {
    /// A row with the same key and the same content exists: the retry of an
    /// insert that succeeded.
    ExistsIdentical,
    /// A row with the same key and different content exists; it is unchanged.
    ExistsDifferent,
}

/// Why a whole operation was rejected. An operation is checked in this
/// order, and the first broken rule rejects it: the table, the kind of an
/// insert's table, the column list, then each row, key or bound in order (a
/// bound shorter than the partition key, then each of its values), and last
/// whether a scan's bounds share a partition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// No table has this id.
    UnknownTable,
    /// An insert into a versioned table, which phase 1 does not write.
    NotImmutable,
    /// The column list names a column the table does not have.
    UnknownColumn,
    /// The scan bounds lie in different partitions, or one is shorter than
    /// the partition key.
    CrossPartition,
    /// A value that is not of its column's type, with the row batch error
    /// that rejects it on the wire.
    Invalid(Error),
}

/// A stored row and the commit timestamp of the insert that wrote it (`_ts`).
#[derive(Clone, Debug)]
pub struct Row {
    pub values: Vec<Value>,
    pub ts: u64,
}

/// Something the harness applies operations to: the model, or an adapter
/// over the real engine.
pub trait Engine {
    /// Applies `op` as the operation committed at timestamp `ts`. The
    /// timestamps of successive calls strictly increase.
    fn apply(&mut self, ts: u64, op: &Op) -> Outcome;

    /// Every row of `table`, in key order.
    fn content(&mut self, table: TableId) -> Vec<Row>;
}

/// The reference model of a catalog's tables.
pub struct Model {
    tables: Vec<ModelTable>,
    /// The timestamp of the last operation applied.
    ts: u64,
}

struct ModelTable {
    schema: Table,
    /// Key columns, in key order, with their types.
    key: Vec<(Type, Order)>,
    /// How many of the leading key columns are the partition key.
    partition: usize,
    rows: BTreeMap<Vec<u8>, Row>,
}

impl Model {
    /// Empty tables of a valid catalog.
    pub fn new(catalog: &Catalog) -> Self {
        assert_eq!(
            catalog.validate(),
            Ok(()),
            "the model needs a valid catalog"
        );
        let tables = (catalog.groups.iter())
            .flat_map(|g| g.tables.iter().map(|t| (g.partition_key.len(), t)))
            .map(|(partition, t)| ModelTable {
                key: (t.primary_key.iter())
                    .map(|&(c, order)| (t.columns[usize::from(c)].ty.clone(), order))
                    .collect(),
                schema: t.clone(),
                partition,
                rows: BTreeMap::new(),
            })
            .collect();
        Self { tables, ts: 0 }
    }

    fn insert(&mut self, ts: u64, table: TableId, rows: &[Vec<Value>]) -> Result<Outcome, Reject> {
        let t = self.table(table)?;
        if t.schema.kind != Kind::Immutable {
            return Err(Reject::NotImmutable);
        }
        // shortcut: the size of the batch is not checked (`ROWS_MAX`, one message); an adapter
        // cannot encode a batch past it, so add the check when a test sends one on purpose.
        // The whole batch is checked before any row is written.
        for row in rows {
            assert_eq!(
                row.len(),
                t.schema.columns.len(),
                "a row batch row has every column"
            );
            for (c, v) in t.schema.columns.iter().zip(row) {
                check(&c.ty, v)?;
            }
        }
        let mut failed = Vec::new();
        for (i, row) in (0u32..).zip(rows) {
            let key = t.key_of(row);
            // A duplicate within the batch meets the row its earlier event wrote.
            match t.rows.get(&key) {
                None => {
                    let row = Row {
                        values: row.clone(),
                        ts,
                    };
                    t.rows.insert(key, row);
                }
                Some(old) if same(&old.values, row) => failed.push((i, Duplicate::ExistsIdentical)),
                Some(_) => failed.push((i, Duplicate::ExistsDifferent)),
            }
        }
        Ok(Outcome::Inserted(failed))
    }

    fn get(
        &mut self,
        table: TableId,
        keys: &[Vec<Value>],
        columns: &[u16],
    ) -> Result<Outcome, Reject> {
        let t = self.table(table)?;
        t.check_columns(columns)?;
        let mut encoded = Vec::new();
        for key in keys {
            assert_eq!(key.len(), t.key.len(), "a get key has every key column");
            encoded.push(t.encode(key)?);
        }
        let found = (encoded.iter())
            .map(|k| t.rows.get(k).map(|r| project(&r.values, columns)))
            .collect();
        Ok(Outcome::Got(found))
    }

    fn scan(
        &mut self,
        table: TableId,
        bounds: [&[Value]; 2],
        limit: u32,
        columns: &[u16],
    ) -> Result<Outcome, Reject> {
        let t = self.table(table)?;
        t.check_columns(columns)?;
        let mut encoded = Vec::new();
        for bound in bounds {
            if bound.len() < t.partition {
                return Err(Reject::CrossPartition);
            }
            encoded.push(t.encode(bound)?);
        }
        let partition = |b: &[Value]| t.encode(&b[..t.partition]).expect("checked above");
        if partition(bounds[0]) != partition(bounds[1]) {
            return Err(Reject::CrossPartition);
        }
        let (start, end) = (&encoded[0], &encoded[1]);
        let rows = (t.rows.range(start.clone()..))
            .take_while(|(k, _)| *k <= end || k.starts_with(end))
            .take(usize::try_from(limit).expect("u32 fits usize"))
            .map(|(_, r)| project(&r.values, columns))
            .collect();
        Ok(Outcome::Scanned(rows))
    }

    fn table(&mut self, table: TableId) -> Result<&mut ModelTable, Reject> {
        let i = usize::try_from(table).expect("u32 fits usize");
        self.tables.get_mut(i).ok_or(Reject::UnknownTable)
    }
}

impl Engine for Model {
    fn apply(&mut self, ts: u64, op: &Op) -> Outcome {
        assert!(ts > self.ts, "timestamps strictly increase");
        self.ts = ts;
        let result = match op {
            Op::Insert { table, rows } => self.insert(ts, *table, rows),
            Op::Get {
                table,
                keys,
                columns,
            } => self.get(*table, keys, columns),
            Op::Scan {
                table,
                start,
                end,
                limit,
                columns,
            } => self.scan(*table, [start, end], *limit, columns),
        };
        result.unwrap_or_else(Outcome::Rejected)
    }

    fn content(&mut self, table: TableId) -> Vec<Row> {
        let t = self
            .table(table)
            .expect("content of a table of the catalog");
        t.rows.values().cloned().collect()
    }
}

impl ModelTable {
    /// The encoded primary key of a checked row.
    fn key_of(&self, row: &[Value]) -> Vec<u8> {
        let values: Vec<Value> = (self.schema.primary_key.iter())
            .map(|&(c, _)| row[usize::from(c)].clone())
            .collect();
        encode_key(&self.key, &values).expect("a checked row has a valid key")
    }

    /// Checks the values of a key or key prefix and encodes them.
    fn encode(&self, values: &[Value]) -> Result<Vec<u8>, Reject> {
        assert!(
            values.len() <= self.key.len(),
            "a key has at most every key column"
        );
        let columns = &self.key[..values.len()];
        for ((ty, _), v) in columns.iter().zip(values) {
            check(ty, v)?;
        }
        // A valid catalog bounds every key within the key limit.
        Ok(encode_key(columns, values).expect("checked values"))
    }

    fn check_columns(&self, columns: &[u16]) -> Result<(), Reject> {
        match columns
            .iter()
            .all(|&c| usize::from(c) < self.schema.columns.len())
        {
            true => Ok(()),
            false => Err(Reject::UnknownColumn),
        }
    }
}

/// The requested columns of a row, in request order.
fn project(values: &[Value], columns: &[u16]) -> Vec<Value> {
    columns
        .iter()
        .map(|&c| values[usize::from(c)].clone())
        .collect()
}

/// Checks a value against its column's type, with the row batch error that
/// rejects the same value on the wire (document 05 section 1).
fn check(ty: &Type, value: &Value) -> Result<(), Reject> {
    use granite_types::Error as E;
    ty.check(value).map_err(|e| {
        Reject::Invalid(match e {
            E::Length { .. } if matches!(base(ty), Type::String { .. } | Type::Bytes { .. }) => {
                Error::TooLong
            }
            E::UnknownEnumValue => Error::UnknownEnumValue,
            // A decimal mantissa of more than P digits.
            E::Overflow => Error::InvalidValue,
            e => panic!("{e:?}: a value a row batch cannot carry"),
        })
    })
}

/// The type under any Nullable and LowCardinality wrappers.
fn base(ty: &Type) -> &Type {
    match ty {
        Type::Nullable(t) | Type::LowCardinality(t) => base(t),
        t => t,
    }
}

/// Whether two rows, or two lists of values, have the same content: equal
/// values, where floats compare by their stored bits, so that every NaN is
/// the same and -0.0 differs from 0.0 (document 03 section 2.2).
pub fn same(a: &[Value], b: &[Value]) -> bool {
    a.len() == b.len()
        && (a.iter().zip(b)).all(|(x, y)| match (x, y) {
            (Value::F32(x), Value::F32(y)) => {
                canonical_f32(*x).to_bits() == canonical_f32(*y).to_bits()
            }
            (Value::F64(x), Value::F64(y)) => {
                canonical_f64(*x).to_bits() == canonical_f64(*y).to_bits()
            }
            (x, y) => x == y,
        })
}
