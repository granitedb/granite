//! The row batch validator on arbitrary bytes and arbitrary schemas: it never
//! panics, gives the same answer twice, and every batch it accepts reads
//! through the accessors, each field borrowed from the input, and encodes
//! back to the same bytes. The same bytes are also read as a result list.
//!
//! The input is a column count, that many column types in the catalog
//! encoding (document 03 section 1.2), then the batch. The table keys on its
//! first column and must pass the catalog's validation, as the server's do.

#![no_main]

use granite_protocol::batch::{self, Batch, Field, RowLayout};
use granite_protocol::results::Results;
use granite_schema::{
    Catalog, Column, Kind, Layout, Partitioning, Retention, Table, TableGroup, decode_type,
};
use granite_types::key::Order;
use granite_types::storage::decode_fixed;
use granite_types::{Type, Value};

/// The column types the input starts with, if they make a valid table, and the rest of the input.
fn schema(bytes: &[u8]) -> Option<(Vec<Type>, &[u8])> {
    let (&n, mut rest) = bytes.split_first()?;
    let mut columns = Vec::new();
    for i in 0..n {
        let (ty, used) = decode_type(rest).ok()?;
        rest = &rest[used..];
        columns.push(Column {
            name: format!("c{i}"),
            ty,
        });
    }
    let partition_key = vec![columns.first()?.clone()];
    let table = Table {
        name: "t".into(),
        kind: Kind::Immutable,
        columns,
        primary_key: vec![(0, Order::Asc)],
        layout: Some(Layout::Row),
        retention: Retention::Forever,
    };
    let group = TableGroup {
        name: "g".into(),
        partitioning: Partitioning::Hash,
        partition_key,
        tables: vec![table],
    };
    let catalog = Catalog {
        version: 1,
        groups: vec![group],
    };
    catalog.validate().ok()?;
    let types = catalog.groups[0].tables[0]
        .columns
        .iter()
        .map(|c| c.ty.clone())
        .collect();
    Some((types, rest))
}

fn bare(ty: &Type) -> &Type {
    match ty {
        Type::Nullable(inner) | Type::LowCardinality(inner) => bare(inner),
        ty => ty,
    }
}

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    let Some((types, input)) = schema(bytes) else {
        return;
    };
    let layout = RowLayout::new(&types);
    let result = Batch::validate(&layout, input);
    assert_eq!(
        result.map(|b| b.len()),
        Batch::validate(&layout, input).map(|b| b.len())
    );
    if let Ok(batch) = result {
        let range = input.as_ptr_range();
        let inside = |b: &[u8]| {
            b.is_empty() || (range.contains(&b.as_ptr()) && b.as_ptr_range().end <= range.end)
        };
        // shortcut: re-encoding builds every value, so only batches of up to 1,000 rows are
        // compared, which keeps the run under the 2 MiB allocation limit.
        let mut rows = Vec::new();
        for row in batch.rows() {
            let values: Vec<Value> = (types.iter().enumerate())
                .map(|(c, ty)| match row.get(c).expect("every column") {
                    Field::Null => Value::Null,
                    Field::String(s) => {
                        assert!(inside(s.as_bytes()));
                        Value::String(s.into())
                    }
                    Field::Bytes(b) => {
                        assert!(inside(b));
                        Value::Bytes(b.into())
                    }
                    Field::Fixed(b) => {
                        assert!(inside(b));
                        decode_fixed(bare(ty), b).expect("a validated value decodes")
                    }
                })
                .collect();
            assert!(row.get(types.len()).is_none());
            if batch.len() <= 1000 {
                rows.push(values);
            }
        }
        assert!(batch.row(batch.len()).is_none());
        if batch.len() <= 1000 {
            assert_eq!(
                batch::encode(&layout, &rows).expect("valid rows encode"),
                input
            );
        }
    }
    let events = input
        .get(..4)
        .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()));
    if let Ok(results) = Results::validate(input, events) {
        for (index, code) in results.iter() {
            assert!(index < events);
            assert_eq!(results.code(index), code);
        }
    }
});
