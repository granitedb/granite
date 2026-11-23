//! Reading a validated batch copies nothing and allocates nothing (T1.4.4):
//! every field the accessors return points into the received bytes, and the
//! counting allocator sees no allocation while they are read.

// The io tests' counting allocator, shared rather than copied: it is the one
// module outside `io` proper that tools/lint.py lets use `unsafe` for this.
#[path = "../../io/tests/common/counting_alloc.rs"]
mod counting_alloc;

use granite_protocol::batch::{self, Batch, Field, RowLayout};
use granite_protocol::results::{self, Results};
use granite_types::{Type, Value};
use std::hint::black_box;

#[test]
fn reading_a_validated_batch_copies_and_allocates_nothing() {
    let layout = RowLayout::new(&[
        Type::U64,
        Type::String { max: 32 },
        Type::Nullable(Box::new(Type::Bytes { max: 8 })),
        Type::FixedBytes(4),
    ]);
    let rows: Vec<Vec<Value>> = (0..100u64)
        .map(|i| {
            let bytes = if i % 3 == 0 {
                Value::Null
            } else {
                Value::Bytes(vec![1; usize::try_from(i % 8).unwrap()])
            };
            vec![
                Value::U64(i),
                Value::String(format!("row {i}")),
                bytes,
                Value::FixedBytes(vec![2; 4]),
            ]
        })
        .collect();
    let bytes = batch::encode(&layout, &rows).unwrap();
    let list = results::encode(&[(3, 7), (50, 9)]);
    let inside = |b: &[u8]| {
        let range = bytes.as_ptr_range();
        b.is_empty() || (range.contains(&b.as_ptr()) && b.as_ptr_range().end <= range.end)
    };

    let before = counting_alloc::allocations();
    let batch = Batch::validate(&layout, &bytes).unwrap();
    let results = Results::validate(&list, 100).unwrap();
    let (mut fields, mut nulls, mut copies) = (0, 0, 0);
    for row in batch.rows() {
        for c in 0..4 {
            match black_box(row.get(c).unwrap()) {
                Field::Null => nulls += 1,
                Field::Fixed(b) | Field::Bytes(b) => copies += usize::from(!inside(b)),
                Field::String(s) => copies += usize::from(!inside(s.as_bytes())),
            }
            fields += 1;
        }
    }
    let codes: u32 =
        (0..100).map(|i| results.code(i)).sum::<u32>() + results.iter().map(|e| e.1).sum::<u32>();
    let allocations = counting_alloc::allocations() - before;

    assert_eq!((fields, nulls, copies), (400, 34, 0));
    assert_eq!(codes, 32);
    assert_eq!(allocations, 0);
}
