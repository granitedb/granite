//! The golden row batch of `crates/protocol/tests/entry.batch` is a batch of
//! the `entry` table of `schemas/exchange.schema`: the column types that
//! protocol's `golden_entry_batch` test writes out (it cannot depend on the
//! schema compiler) are the compiled schema's, and the batch validates
//! against them.

use granite_protocol::batch::{Batch, RowLayout};
use granite_schema::Catalog;
use granite_types::Type;

#[test]
fn the_golden_entry_batch_matches_the_exchange_schema() {
    let source = include_str!("../../../schemas/exchange.schema");
    let catalog = granite_schemac::compile("exchange.schema", source.as_bytes()).expect("compiles");
    let catalog = Catalog::decode(&catalog).expect("decodes");
    let entry = (catalog.groups.iter().flat_map(|g| &g.tables))
        .find(|t| t.name == "entry")
        .expect("the exchange schema has an entry table");
    let types: Vec<Type> = entry.columns.iter().map(|c| c.ty.clone()).collect();
    // The list of `entry()` in crates/protocol/tests/batch.rs.
    let written_out = [
        Type::Id128,
        Type::U64,
        Type::U32,
        Type::Enum8 { max: 3 },
        Type::Decimal {
            precision: 38,
            scale: 18,
        },
        Type::Id128,
    ];
    assert_eq!(types, written_out);

    let golden = include_str!("../../protocol/tests/entry.batch");
    let digits: String = (golden.lines())
        .flat_map(|l| l.split('#').next().unwrap().split_whitespace())
        .collect();
    let bytes: Vec<u8> = (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).unwrap())
        .collect();
    let layout = RowLayout::new(&types);
    assert_eq!(
        Batch::validate(&layout, &bytes)
            .expect("the golden batch validates")
            .len(),
        2
    );
}
