//! The validator on arbitrary decoded trees: it never panics, gives the same
//! answer twice, and what it accepts stays within the access limit. The
//! catalog has the tables of the validator tests (`crates/txn/tests/support`),
//! whose corpus programs seeded the fuzz corpus.

#![no_main]

use granite_base::constants::FORMAT;
use granite_schema::Catalog;
use granite_txn::Program;
use std::sync::LazyLock;

const SCHEMA: &str = "
schema version 1
table group users partition by (user u64) hash
table account versioned layout row in users
  key (user)
  user u64, balance Decimal(18, 2), nick Nullable(String(8)), active bool
table entry immutable layout row in users
  key (user, id)
  user u64, id u32, amount Decimal(18, 2), kind Enum8(3)
table holding versioned layout row in users
  key (user, asset desc)
  user u64, asset u32, qty i64
table group assets partition by (asset u32) range
table asset versioned layout row in assets
  key (asset)
  asset u32, price i64, name LowCardinality(String(8)), ticker Nullable(i64)
table quote immutable layout row in assets
  key (asset, at)
  asset u32, at u16, px i64
";

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    let bytes = granite_schemac::compile("fuzz", SCHEMA.as_bytes()).expect("the schema compiles");
    Catalog::decode(&bytes).expect("a compiled catalog decodes")
});

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    let Ok(program) = Program::decode(bytes) else {
        return;
    };
    let result = program.validate(&CATALOG);
    // By debug form, which tells NaN literals apart where `==` does not.
    assert_eq!(format!("{result:?}"), format!("{:?}", program.validate(&CATALOG)));
    if let Ok(report) = result {
        assert!(report.reads <= FORMAT.program_accesses_max);
        assert!(report.writes <= FORMAT.program_accesses_max);
        for a in &report.accesses {
            let group = &CATALOG.groups[a.group];
            assert!(a.table < group.tables.len());
            assert_eq!(a.partition_key.len(), group.partition_key.len());
        }
    }
});
