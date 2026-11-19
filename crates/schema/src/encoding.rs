//! The binary form of a catalog, document 03 section 1.2. Integers are
//! little-endian; only valid catalogs are encoded or decoded, and the
//! encoding is canonical: decoding then encoding gives the same bytes.

use crate::{Catalog, Column, Error, Kind, Layout, Partitioning, Retention, Table, TableGroup};
use granite_base::constants::FORMAT;
use granite_types::key::Order;
use granite_types::{TimePrecision, Type};

/// The version of this encoding, its first two bytes.
pub const ENCODING_VERSION: u16 = 1;

// Every count of a valid catalog fits the u16 it is encoded in.
const _: () = assert!(FORMAT.tables_max <= u16::MAX as u64);
const _: () = assert!(FORMAT.columns_per_table_max <= u16::MAX as u64);

/// Wrappers a valid type can nest, as in LowCardinality(Nullable(LowCardinality(T))).
const WRAPPERS_MAX: u8 = 3;

impl Catalog {
    /// Validates the catalog and encodes it. The same catalog always gives the same bytes.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        let mut w = Vec::new();
        w.extend(ENCODING_VERSION.to_le_bytes());
        w.extend(self.version.to_le_bytes());
        count(&mut w, self.groups.len());
        for g in &self.groups {
            name(&mut w, &g.name);
            w.push(match g.partitioning {
                Partitioning::Hash => 0,
                Partitioning::Range => 1,
            });
            columns(&mut w, &g.partition_key);
            count(&mut w, g.tables.len());
            for t in &g.tables {
                table(&mut w, t);
            }
        }
        Ok(w)
    }

    /// Decodes and validates a catalog.
    pub fn decode(bytes: &[u8]) -> Result<Catalog, Error> {
        let mut r = Reader { bytes, pos: 0 };
        if r.u16()? != ENCODING_VERSION {
            return Err(Error::UnknownVersion);
        }
        let version = r.u64()?;
        let groups = r.list(|r| {
            Ok(TableGroup {
                name: r.name()?,
                partitioning: match r.u8()? {
                    0 => Partitioning::Hash,
                    1 => Partitioning::Range,
                    _ => return Err(Error::Malformed),
                },
                partition_key: r.list(Reader::column)?,
                tables: r.list(Reader::table)?,
            })
        })?;
        if r.pos != bytes.len() {
            return Err(Error::Malformed);
        }
        let catalog = Catalog { version, groups };
        catalog.validate()?;
        Ok(catalog)
    }
}

fn count(w: &mut Vec<u8>, n: usize) {
    let n = u16::try_from(n).expect("a valid catalog's counts fit u16");
    w.extend(n.to_le_bytes());
}

fn name(w: &mut Vec<u8>, name: &str) {
    w.push(u8::try_from(name.len()).expect("a valid name is at most 255 bytes"));
    w.extend(name.as_bytes());
}

fn columns(w: &mut Vec<u8>, columns: &[Column]) {
    count(w, columns.len());
    for c in columns {
        name(w, &c.name);
        ty(w, &c.ty);
    }
}

fn table(w: &mut Vec<u8>, t: &Table) {
    name(w, &t.name);
    w.push(match t.kind {
        Kind::Immutable => 0,
        Kind::Versioned => 1,
    });
    columns(w, &t.columns);
    count(w, t.primary_key.len());
    for (i, order) in &t.primary_key {
        w.extend(i.to_le_bytes());
        w.push(match order {
            Order::Asc => 0,
            Order::Desc => 1,
        });
    }
    match &t.layout {
        None => w.push(0),
        Some(Layout::Row) => w.push(1),
        Some(Layout::Columnar) => w.push(2),
        Some(Layout::Groups(groups)) => {
            w.push(3);
            count(w, groups.len());
            for g in groups {
                count(w, g.len());
                g.iter().for_each(|i| w.extend(i.to_le_bytes()));
            }
        }
    }
    let (tag, ms) = match t.retention {
        Retention::Forever => (0, None),
        Retention::ArchiveAfterMs(ms) => (1, Some(ms)),
        Retention::DropAfterMs(ms) => (2, Some(ms)),
    };
    w.push(tag);
    w.extend(ms.map(u64::to_le_bytes).into_iter().flatten());
}

fn ty(w: &mut Vec<u8>, t: &Type) {
    let precision = |p: &TimePrecision| match p {
        TimePrecision::Seconds => 0,
        TimePrecision::Millis => 1,
        TimePrecision::Micros => 2,
        TimePrecision::Nanos => 3,
    };
    match t {
        Type::U8 => w.push(0),
        Type::U16 => w.push(1),
        Type::U32 => w.push(2),
        Type::U64 => w.push(3),
        Type::U128 => w.push(4),
        Type::U256 => w.push(5),
        Type::I8 => w.push(6),
        Type::I16 => w.push(7),
        Type::I32 => w.push(8),
        Type::I64 => w.push(9),
        Type::I128 => w.push(10),
        Type::I256 => w.push(11),
        Type::Decimal { precision, scale } => w.extend([12, *precision, *scale]),
        Type::F32 => w.push(13),
        Type::F64 => w.push(14),
        Type::Bool => w.push(15),
        Type::Date => w.push(16),
        Type::Timestamp(p) => w.extend([17, precision(p)]),
        Type::Duration(p) => w.extend([18, precision(p)]),
        Type::Uuid => w.push(19),
        Type::Id128 => w.push(20),
        Type::String { max } => {
            w.push(21);
            w.extend(max.to_le_bytes());
        }
        Type::Bytes { max } => {
            w.push(22);
            w.extend(max.to_le_bytes());
        }
        Type::FixedBytes(n) => {
            w.push(23);
            w.extend(n.to_le_bytes());
        }
        Type::Enum8 { max } => w.extend([24, *max]),
        Type::Enum16 { max } => {
            w.push(25);
            w.extend(max.to_le_bytes());
        }
        Type::Nullable(inner) => {
            w.push(26);
            ty(w, inner);
        }
        Type::LowCardinality(inner) => {
            w.push(27);
            ty(w, inner);
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let rest = &self.bytes[self.pos..];
        let taken = *rest.first_chunk::<N>().ok_or(Error::Malformed)?;
        self.pos += N;
        Ok(taken)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take()?))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take()?))
    }

    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take()?))
    }

    /// A u16 count, then that many items. Each item takes at least one byte,
    /// so the list grows only as far as the input lasts.
    fn list<T>(&mut self, item: fn(&mut Self) -> Result<T, Error>) -> Result<Vec<T>, Error> {
        let n = self.u16()?;
        let mut items = Vec::new();
        for _ in 0..n {
            items.push(item(self)?);
        }
        Ok(items)
    }

    fn name(&mut self) -> Result<String, Error> {
        let len = usize::from(self.u8()?);
        let bytes = self.bytes[self.pos..].get(..len).ok_or(Error::Malformed)?;
        self.pos += len;
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::Malformed)
    }

    fn column(&mut self) -> Result<Column, Error> {
        Ok(Column {
            name: self.name()?,
            ty: self.ty(0)?,
        })
    }

    fn table(&mut self) -> Result<Table, Error> {
        Ok(Table {
            name: self.name()?,
            kind: match self.u8()? {
                0 => Kind::Immutable,
                1 => Kind::Versioned,
                _ => return Err(Error::Malformed),
            },
            columns: self.list(Reader::column)?,
            primary_key: self.list(|r| {
                let i = r.u16()?;
                match r.u8()? {
                    0 => Ok((i, Order::Asc)),
                    1 => Ok((i, Order::Desc)),
                    _ => Err(Error::Malformed),
                }
            })?,
            layout: match self.u8()? {
                0 => None,
                1 => Some(Layout::Row),
                2 => Some(Layout::Columnar),
                3 => Some(Layout::Groups(self.list(|r| r.list(Reader::u16))?)),
                _ => return Err(Error::Malformed),
            },
            retention: match self.u8()? {
                0 => Retention::Forever,
                1 => Retention::ArchiveAfterMs(self.u64()?),
                2 => Retention::DropAfterMs(self.u64()?),
                _ => return Err(Error::Malformed),
            },
        })
    }

    fn ty(&mut self, depth: u8) -> Result<Type, Error> {
        let wrapped = |r: &mut Self| -> Result<Box<Type>, Error> {
            if depth == WRAPPERS_MAX {
                return Err(Error::Malformed);
            }
            Ok(Box::new(r.ty(depth + 1)?))
        };
        let precision = |r: &mut Self| match r.u8()? {
            0 => Ok(TimePrecision::Seconds),
            1 => Ok(TimePrecision::Millis),
            2 => Ok(TimePrecision::Micros),
            3 => Ok(TimePrecision::Nanos),
            _ => Err(Error::Malformed),
        };
        Ok(match self.u8()? {
            0 => Type::U8,
            1 => Type::U16,
            2 => Type::U32,
            3 => Type::U64,
            4 => Type::U128,
            5 => Type::U256,
            6 => Type::I8,
            7 => Type::I16,
            8 => Type::I32,
            9 => Type::I64,
            10 => Type::I128,
            11 => Type::I256,
            12 => Type::Decimal {
                precision: self.u8()?,
                scale: self.u8()?,
            },
            13 => Type::F32,
            14 => Type::F64,
            15 => Type::Bool,
            16 => Type::Date,
            17 => Type::Timestamp(precision(self)?),
            18 => Type::Duration(precision(self)?),
            19 => Type::Uuid,
            20 => Type::Id128,
            21 => Type::String { max: self.u32()? },
            22 => Type::Bytes { max: self.u32()? },
            23 => Type::FixedBytes(self.u32()?),
            24 => Type::Enum8 { max: self.u8()? },
            25 => Type::Enum16 { max: self.u16()? },
            26 => Type::Nullable(wrapped(self)?),
            27 => Type::LowCardinality(wrapped(self)?),
            _ => return Err(Error::Malformed),
        })
    }
}
