//! A generator of random operations for a catalog, valid and invalid, from
//! one seed.
//!
//! Values come from a small domain, four per type, so that keys repeat and
//! partitions hold several rows. Rows are drawn fresh, as a retry of an
//! earlier row (`exists_identical` when that row was stored), or as an
//! earlier row with one column drawn again (`exists_different`), so
//! duplicates occur within a batch and across batches. About one operation
//! in ten breaks one rule of [`Reject`](super::Reject).

use super::{Op, TableId};
use granite_io::memory::SplitMix64;
use granite_schema::{Catalog, Kind};
use granite_types::{I256, Type, U256, Value, decimal_width};

/// Rows remembered per table, to draw retries, keys and scan bounds from.
const HISTORY: usize = 64;
/// The most events of one operation. With at most one 64 KiB row each, a
/// batch always fits in a message.
const EVENTS_MAX: u64 = 8;

pub struct Workload {
    rng: SplitMix64,
    tables: Vec<Shape>,
}

struct Shape {
    types: Vec<Type>,
    /// Column indexes of the primary key, in key order.
    key: Vec<usize>,
    /// How many of the leading key columns are the partition key.
    partition: usize,
    immutable: bool,
    history: Vec<Vec<Value>>,
}

impl Workload {
    pub fn new(catalog: &Catalog, seed: u64) -> Self {
        let tables = (catalog.groups.iter())
            .flat_map(|g| g.tables.iter().map(move |t| (g, t)))
            .map(|(g, t)| Shape {
                types: t.columns.iter().map(|c| c.ty.clone()).collect(),
                key: t.primary_key.iter().map(|&(c, _)| usize::from(c)).collect(),
                partition: g.partition_key.len(),
                immutable: t.kind == Kind::Immutable,
                history: Vec::new(),
            })
            .collect::<Vec<_>>();
        assert!(!tables.is_empty(), "a catalog with a table");
        Self {
            rng: SplitMix64(seed),
            tables,
        }
    }

    /// The next operation.
    pub fn next_op(&mut self) -> Op {
        match self.below(10) {
            0 => self.invalid(),
            1..=5 => self.insert(),
            6 | 7 => self.get(),
            _ => self.scan(),
        }
    }

    fn insert(&mut self) -> Op {
        let immutable: Vec<usize> = (0..self.tables.len())
            .filter(|&t| self.tables[t].immutable)
            .collect();
        if immutable.is_empty() {
            return self.invalid();
        }
        let t = immutable[self.index(immutable.len())];
        let n = 1 + self.below(EVENTS_MAX);
        let rows = (0..n).map(|_| self.row(t)).collect();
        Op::Insert { table: id(t), rows }
    }

    fn get(&mut self) -> Op {
        let t = self.index(self.tables.len());
        let n = 1 + self.below(EVENTS_MAX);
        Op::Get {
            table: id(t),
            keys: (0..n).map(|_| self.key(t)).collect(),
            columns: self.columns(t),
        }
    }

    fn scan(&mut self) -> Op {
        let t = self.index(self.tables.len());
        let (partition, len) = (self.tables[t].partition, self.tables[t].key.len());
        let mut start = self.key(t);
        let mut end = self.key(t);
        end[..partition].clone_from_slice(&start[..partition]);
        start.truncate(partition + self.index(len - partition + 1));
        end.truncate(partition + self.index(len - partition + 1));
        let limit = match self.below(4) {
            0 => u32::MAX,
            _ => self.below(8) as u32,
        };
        Op::Scan {
            table: id(t),
            start,
            end,
            limit,
            columns: self.columns(t),
        }
    }

    /// A valid-looking operation with one rule broken.
    fn invalid(&mut self) -> Op {
        let unknown = id(self.tables.len()) + self.below(3) as u32;
        match self.below(5) {
            0 => {
                let mut op = self.any();
                *table(&mut op) = unknown;
                op
            }
            1 => match (0..self.tables.len()).find(|&t| !self.tables[t].immutable) {
                Some(t) => Op::Insert {
                    table: id(t),
                    rows: vec![self.fresh(t)],
                },
                None => Op::Insert {
                    table: unknown,
                    rows: Vec::new(),
                },
            },
            2 => {
                let mut op = self.any();
                self.spoil(&mut op);
                op
            }
            3 => {
                let mut op = if self.below(2) == 0 {
                    self.get()
                } else {
                    self.scan()
                };
                let t = usize::try_from(*table(&mut op)).expect("u32 fits usize");
                let unknown = self.tables[t].types.len() + self.index(3);
                if let Op::Get { columns, .. } | Op::Scan { columns, .. } = &mut op {
                    let at = self.index(columns.len() + 1);
                    columns.insert(at, u16::try_from(unknown).expect("at most 1027 columns"));
                }
                op
            }
            _ => {
                let mut op = self.scan();
                if let Op::Scan {
                    table, start, end, ..
                } = &mut op
                {
                    let t = usize::try_from(*table).expect("u32 fits usize");
                    // Shorter than the partition key, or in another partition
                    // (unless the drawn partition happens to be the same).
                    match self.below(2) {
                        0 => start.clear(),
                        _ => *end = self.key(t)[..self.tables[t].partition].to_vec(),
                    }
                }
                op
            }
        }
    }

    fn any(&mut self) -> Op {
        match self.below(3) {
            0 => self.insert(),
            1 => self.get(),
            _ => self.scan(),
        }
    }

    /// Replaces one value of `op` by one its column's type does not allow,
    /// if any of its columns has such values.
    fn spoil(&mut self, op: &mut Op) {
        let t = usize::try_from(*table(op)).expect("u32 fits usize");
        let shape = &self.tables[t];
        let key_types: Vec<&Type> = shape.key.iter().map(|&c| &shape.types[c]).collect();
        let (lists, types): (Vec<&mut Vec<Value>>, Vec<&Type>) = match op {
            Op::Insert { rows, .. } => (rows.iter_mut().collect(), shape.types.iter().collect()),
            Op::Get { keys, .. } => (keys.iter_mut().collect(), key_types),
            Op::Scan { start, end, .. } => (vec![start, end], key_types),
        };
        let mut places = Vec::new();
        for (l, list) in lists.into_iter().enumerate() {
            for (c, ty) in types.iter().enumerate().take(list.len()) {
                if let Some(v) = invalid(ty) {
                    places.push((l, c, v));
                }
            }
        }
        if places.is_empty() {
            return;
        }
        let (l, c, v) = places.swap_remove(self.index(places.len()));
        match op {
            Op::Insert { rows, .. } => rows[l][c] = v,
            Op::Get { keys, .. } => keys[l][c] = v,
            Op::Scan { start, end, .. } => [start, end][l][c] = v,
        }
    }

    /// A row of table `t`: fresh, an earlier row again, or an earlier row
    /// with one column drawn again. It is remembered.
    fn row(&mut self, t: usize) -> Vec<Value> {
        let earlier = self.earlier(t);
        let row = match (self.below(4), earlier) {
            (0, Some(row)) => row,
            (1, Some(mut row)) => {
                let shape = &self.tables[t];
                let others: Vec<usize> = (0..shape.types.len())
                    .filter(|c| !shape.key.contains(c))
                    .collect();
                if !others.is_empty() {
                    let c = others[self.index(others.len())];
                    let ty = self.tables[t].types[c].clone();
                    row[c] = self.value(&ty);
                }
                row
            }
            _ => self.fresh(t),
        };
        let at = self.index(HISTORY);
        let history = &mut self.tables[t].history;
        match history.len() < HISTORY {
            true => history.push(row.clone()),
            false => history[at] = row.clone(),
        }
        row
    }

    fn earlier(&mut self, t: usize) -> Option<Vec<Value>> {
        let n = self.tables[t].history.len();
        let i = self.index(n.max(1));
        self.tables[t].history.get(i).cloned()
    }

    fn fresh(&mut self, t: usize) -> Vec<Value> {
        let types = self.tables[t].types.clone();
        types.iter().map(|ty| self.value(ty)).collect()
    }

    /// A key of table `t`, mostly of a row drawn earlier.
    fn key(&mut self, t: usize) -> Vec<Value> {
        let row = match (self.below(4), self.earlier(t)) {
            (1.., Some(row)) => row,
            _ => self.fresh(t),
        };
        self.tables[t].key.iter().map(|&c| row[c].clone()).collect()
    }

    /// A column list: any columns, in any order, repeats allowed.
    fn columns(&mut self, t: usize) -> Vec<u16> {
        let n = self.tables[t].types.len();
        (0..self.index(n + 1))
            .map(|_| u16::try_from(self.index(n)).expect("at most 1024 columns"))
            .collect()
    }

    /// A value from the small domain of `ty`.
    fn value(&mut self, ty: &Type) -> Value {
        let n = self.below(4);
        let i = n as i64 - 2;
        let i256 = I256 {
            hi: if i < 0 { -1 } else { 0 },
            lo: i128::from(i) as u128,
        };
        match ty {
            Type::LowCardinality(t) => self.value(t),
            Type::Nullable(_) if n == 0 => Value::Null,
            Type::Nullable(t) => self.value(t),
            Type::U8 => Value::U8(n as u8),
            Type::U16 => Value::U16(n as u16),
            Type::U32 => Value::U32(n as u32),
            Type::U64 => Value::U64(n),
            Type::U128 => Value::U128(u128::from(n)),
            Type::U256 => Value::U256(U256 {
                hi: 0,
                lo: u128::from(n),
            }),
            Type::I8 => Value::I8(i as i8),
            Type::I16 => Value::I16(i as i16),
            Type::I32 => Value::I32(i as i32),
            Type::I64 => Value::I64(i),
            Type::I128 => Value::I128(i128::from(i)),
            Type::I256 => Value::I256(i256),
            Type::Decimal { precision, .. } => match decimal_width(*precision) {
                4 => Value::Decimal32(i as i32),
                8 => Value::Decimal64(i),
                16 => Value::Decimal128(i128::from(i)),
                _ => Value::Decimal256(i256),
            },
            // Zero and minus zero differ; NaNs, whatever their bits, do not.
            Type::F32 => Value::F32([0.0, -0.0, f32::NAN, f32::from_bits(0xffc0_0001)][n as usize]),
            Type::F64 => {
                Value::F64([0.0, -0.0, f64::NAN, f64::from_bits(0xfff8_0000_0000_0001)][n as usize])
            }
            Type::Bool => Value::Bool(n % 2 == 1),
            Type::Date => Value::Date(i as i32),
            Type::Timestamp(_) => Value::Timestamp(i),
            Type::Duration(_) => Value::Duration(i),
            Type::Uuid => Value::Uuid([n as u8; 16]),
            Type::Id128 => Value::Id128(u128::from(n)),
            // Zero bytes, which the key encoding escapes, included.
            Type::String { max } => Value::String(
                (0..n.min(u64::from(*max)))
                    .map(|_| ['a', '\0', 'z'][self.index(3)])
                    .collect(),
            ),
            Type::Bytes { max } => Value::Bytes(
                (0..n.min(u64::from(*max)))
                    .map(|_| [0, 1, 0xff][self.index(3)])
                    .collect(),
            ),
            Type::FixedBytes(len) => Value::FixedBytes(vec![n as u8; *len as usize]),
            Type::Enum8 { max } => Value::Enum8((n % (u64::from(*max) + 1)) as u8),
            Type::Enum16 { max } => Value::Enum16((n % (u64::from(*max) + 1)) as u16),
        }
    }

    fn below(&mut self, n: u64) -> u64 {
        self.rng.next_u64() % n
    }

    fn index(&mut self, n: usize) -> usize {
        self.below(n as u64) as usize
    }
}

/// A value the type does not allow but a row batch can carry, if it has one.
fn invalid(ty: &Type) -> Option<Value> {
    Some(match ty {
        Type::LowCardinality(t) | Type::Nullable(t) => return invalid(t),
        // The max is within the row limit, so the batch still fits in a message (`EVENTS_MAX`).
        Type::String { max } => Value::String("x".repeat(*max as usize + 1)),
        Type::Bytes { max } => Value::Bytes(vec![0; *max as usize + 1]),
        Type::Enum8 { max } if *max < u8::MAX => Value::Enum8(max + 1),
        Type::Enum16 { max } if *max < u16::MAX => Value::Enum16(max + 1),
        // The largest mantissa of each width has more digits than its precisions allow.
        Type::Decimal { precision, .. } => match decimal_width(*precision) {
            4 => Value::Decimal32(i32::MAX),
            8 => Value::Decimal64(i64::MAX),
            16 => Value::Decimal128(i128::MAX),
            _ => Value::Decimal256(I256::MAX),
        },
        _ => return None,
    })
}

fn table(op: &mut Op) -> &mut TableId {
    match op {
        Op::Insert { table, .. } | Op::Get { table, .. } | Op::Scan { table, .. } => table,
    }
}

fn id(t: usize) -> TableId {
    TableId::try_from(t).expect("a catalog has at most 4096 tables")
}
