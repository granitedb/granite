//! Row batches (document 05 section 1, "Row batches"): `[count][rows][heap]`.
//!
//! A batch holds rows of one table. Each row is the table's row struct: its
//! columns in declaration order, a fixed-width column as its storage
//! representation and a String or Bytes column as an (offset, length) slot
//! into the heap shared by the whole batch (document 03 section 2.4).
//! [`Batch::validate`] checks every byte once, and the accessors then read
//! the batch in place, without copying or allocating.

use crate::{BODY_SIZE_MAX, Error, results};
use granite_base::constants::FORMAT;
use granite_types::storage::{decode_fixed, encode_fixed};
use granite_types::{Type, Value};

/// Bytes of the batch header: the u32 row count and a u32 reserved field.
pub const HEADER_SIZE: usize = 8;

/// Bytes of a String or Bytes slot: offset and length, both u32.
const SLOT: usize = 8;

/// The most rows of a batch: as many as a result list of one message body
/// has entries, so that a write's results always fit in its reply.
pub const ROWS_MAX: usize = BODY_SIZE_MAX / results::ENTRY;

/// Where each column of a table's row struct lies. Built once per table, from
/// a valid catalog, and shared by every batch of that table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowLayout {
    columns: Vec<Column>,
    width: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Column {
    ty: Type,
    /// Offset in the row struct.
    at: usize,
    width: usize,
}

impl RowLayout {
    /// The layout of a table with these column types, in declaration order.
    /// The table must be part of a valid catalog (document 03 section 1.1).
    pub fn new<'t>(types: impl IntoIterator<Item = &'t Type>) -> Self {
        let mut columns = Vec::new();
        let mut at = 0;
        for ty in types {
            assert_eq!(ty.validate(), Ok(()), "a valid catalog has valid types");
            let width = width(ty);
            columns.push(Column {
                ty: ty.clone(),
                at,
                width,
            });
            at += width;
        }
        assert!((1..=FORMAT.columns_per_table_max as usize).contains(&columns.len()));
        // A valid table's row bound, which includes the row struct, is within the row limit.
        assert!(at <= FORMAT.row_size_max as usize);
        Self { columns, width: at }
    }

    /// Bytes of one row struct.
    pub fn width(&self) -> usize {
        self.width
    }
}

/// Bytes of a column in the row struct: its storage width, or a slot for
/// String and Bytes, after a flag byte for Nullable.
fn width(ty: &Type) -> usize {
    match ty {
        Type::Nullable(inner) => 1 + width(inner),
        Type::LowCardinality(inner) => width(inner),
        Type::String { .. } | Type::Bytes { .. } => SLOT,
        ty => ty.fixed_width().expect("every other type is fixed-width") as usize,
    }
}

/// A batch that passed [`Batch::validate`]. There is no other way to make one.
#[derive(Clone, Copy, Debug)]
pub struct Batch<'a> {
    layout: &'a RowLayout,
    rows: &'a [u8],
    heap: &'a [u8],
}

/// One row of a [`Batch`].
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    layout: &'a RowLayout,
    bytes: &'a [u8],
    heap: &'a [u8],
}

/// One column of a [`Row`], borrowed from the batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field<'a> {
    Null,
    /// The storage representation of a fixed-width value (document 03
    /// section 2.2), without the Nullable flag.
    Fixed(&'a [u8]),
    String(&'a str),
    Bytes(&'a [u8]),
}

impl<'a> Batch<'a> {
    /// Checks a whole batch of rows of `layout`'s table. The first broken
    /// rule rejects it, in the order: size, header, every row and column in
    /// order, the end of the heap.
    pub fn validate(layout: &'a RowLayout, bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() > BODY_SIZE_MAX {
            return Err(Error::TooLarge);
        }
        let header = bytes.get(..HEADER_SIZE).ok_or(Error::Truncated)?;
        if u32_at(header, 4) != 0 {
            return Err(Error::ReservedBits);
        }
        let count = u32_at(header, 0) as usize;
        let body = &bytes[HEADER_SIZE..];
        // Both factors are below 2^32, so the product cannot overflow a 64-bit usize.
        let rows_size = count * layout.width;
        if count > ROWS_MAX || rows_size > body.len() {
            return Err(Error::CountTooLarge);
        }
        let (rows, heap) = body.split_at(rows_size);
        let mut cursor = 0;
        for row in rows.chunks_exact(layout.width) {
            for c in &layout.columns {
                check(&c.ty, &row[c.at..c.at + c.width], heap, &mut cursor)?;
            }
        }
        if cursor != heap.len() {
            return Err(Error::HeapGap);
        }
        Ok(Self { layout, rows, heap })
    }

    pub fn len(&self) -> usize {
        self.rows.len() / self.layout.width
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Row `i`, or `None` past the end.
    pub fn row(&self, i: usize) -> Option<Row<'a>> {
        let at = i.checked_mul(self.layout.width)?;
        Some(Row {
            layout: self.layout,
            bytes: self.rows.get(at..at + self.layout.width)?,
            heap: self.heap,
        })
    }

    pub fn rows(&self) -> impl Iterator<Item = Row<'a>> + use<'a> {
        let (layout, heap) = (self.layout, self.heap);
        (self.rows.chunks_exact(layout.width)).map(move |bytes| Row {
            layout,
            bytes,
            heap,
        })
    }
}

impl<'a> Row<'a> {
    /// Column `c`, or `None` past the last column.
    pub fn get(&self, c: usize) -> Option<Field<'a>> {
        let c = self.layout.columns.get(c)?;
        Some(read(&c.ty, &self.bytes[c.at..c.at + c.width], self.heap))
    }
}

/// Checks one column's bytes in the row struct, and its content in the heap.
/// Contents lie in the heap in row order, then column order, back to back,
/// so each slot must start at `cursor`, the end of the one before it.
fn check(ty: &Type, field: &[u8], heap: &[u8], cursor: &mut usize) -> Result<(), Error> {
    match ty {
        Type::LowCardinality(inner) => check(inner, field, heap, cursor),
        Type::Nullable(inner) => match field[0] {
            // A null is the flag and everything after it zero, slot included.
            0 if field[1..].iter().all(|b| *b == 0) => Ok(()),
            1 => check(inner, &field[1..], heap, cursor),
            _ => Err(Error::InvalidValue),
        },
        Type::String { max } | Type::Bytes { max } => {
            let (offset, len) = (u32_at(field, 0) as usize, u32_at(field, 4));
            if len > *max {
                return Err(Error::TooLong);
            }
            let end = offset + len as usize;
            if end > heap.len() {
                return Err(Error::OffsetBeyondHeap);
            }
            if offset < *cursor {
                return Err(Error::OverlappingSlots);
            }
            if offset > *cursor {
                return Err(Error::HeapGap);
            }
            if matches!(ty, Type::String { .. }) && std::str::from_utf8(&heap[offset..end]).is_err()
            {
                return Err(Error::InvalidUtf8);
            }
            *cursor = end;
            Ok(())
        }
        // Every bit pattern of these is a value, so there is nothing to check.
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
        | Type::I256
        | Type::Date
        | Type::Timestamp(_)
        | Type::Duration(_)
        | Type::Uuid
        | Type::Id128
        | Type::FixedBytes(_) => Ok(()),
        // Bool, floats, enums and decimals.
        ty => match decode_fixed(ty, field) {
            Ok(_) => Ok(()),
            Err(granite_types::Error::UnknownEnumValue) => Err(Error::UnknownEnumValue),
            Err(_) => Err(Error::InvalidValue),
        },
    }
}

/// Reads a column that [`check`] accepted.
fn read<'a>(ty: &Type, field: &'a [u8], heap: &'a [u8]) -> Field<'a> {
    match ty {
        Type::LowCardinality(inner) => read(inner, field, heap),
        Type::Nullable(_) if field[0] == 0 => Field::Null,
        Type::Nullable(inner) => read(inner, &field[1..], heap),
        Type::String { .. } | Type::Bytes { .. } => {
            let offset = u32_at(field, 0) as usize;
            let content = &heap[offset..offset + u32_at(field, 4) as usize];
            match ty {
                Type::String { .. } => {
                    Field::String(std::str::from_utf8(content).expect("validated as UTF-8"))
                }
                _ => Field::Bytes(content),
            }
        }
        _ => Field::Fixed(field),
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

/// Encodes rows of `layout`'s table as a batch. Fails on a value that is not
/// of its column's type, or with [`granite_types::Error::Length`] when the
/// batch would not fit in a message body.
pub fn encode(layout: &RowLayout, rows: &[Vec<Value>]) -> Result<Vec<u8>, granite_types::Error> {
    let too_large = |len: usize| granite_types::Error::Length {
        len: len as u64,
        max: BODY_SIZE_MAX as u64,
    };
    let size = HEADER_SIZE + rows.len().saturating_mul(layout.width);
    if rows.len() > ROWS_MAX || size > BODY_SIZE_MAX {
        return Err(too_large(size));
    }
    let count =
        u32::try_from(rows.len()).expect("a batch within a message has fewer than 2^32 rows");
    let mut out = Vec::with_capacity(size);
    out.extend(count.to_le_bytes());
    out.extend([0; 4]);
    let mut heap = Vec::new();
    for row in rows {
        if row.len() != layout.columns.len() {
            return Err(granite_types::Error::ColumnCount);
        }
        for (c, v) in layout.columns.iter().zip(row) {
            c.ty.check(v)?;
            encode_column(&c.ty, v, &mut out, &mut heap);
            if out.len() + heap.len() > BODY_SIZE_MAX {
                return Err(too_large(out.len() + heap.len()));
            }
        }
    }
    out.extend(heap);
    Ok(out)
}

/// Appends a checked value: fixed-width to the row struct, String and Bytes
/// as a slot there and their content to the heap.
fn encode_column(ty: &Type, value: &Value, row: &mut Vec<u8>, heap: &mut Vec<u8>) {
    let content = match (ty, value) {
        (Type::LowCardinality(inner), v) => return encode_column(inner, v, row, heap),
        (Type::Nullable(inner), Value::Null) => {
            row.resize(row.len() + 1 + width(inner), 0);
            return;
        }
        (Type::Nullable(inner), v) => {
            row.push(1);
            return encode_column(inner, v, row, heap);
        }
        (_, Value::String(s)) => s.as_bytes(),
        (_, Value::Bytes(b)) => b,
        (ty, v) => {
            encode_fixed(ty, v, row).expect("checked against its type");
            return;
        }
    };
    // The caller keeps the batch within a message, so both fit a u32.
    for n in [heap.len(), content.len()] {
        row.extend(u32::try_from(n).expect("within a message").to_le_bytes());
    }
    heap.extend(content);
}
