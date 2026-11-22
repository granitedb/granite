//! The validator on arbitrary decoded trees: it never panics, gives the same
//! answer twice, and what it accepts has one bound per operator, whose sum
//! is within the limit. The catalog has the table of the validator tests
//! (`crates/query/tests/validate.rs`), whose fragments seeded the corpus.

#![no_main]

use granite_base::constants::PROCESS_CONFIG_DEFAULT;
use granite_query::Fragment;
use granite_schema::Catalog;
use std::sync::LazyLock;

const SCHEMA: &str = "
schema version 1
table group accounts partition by (account u64) hash
table trade immutable layout columnar in accounts
  key (account, id desc)
  account u64, id u64, price Decimal(18, 2), qty i64, venue Nullable(String(16)), note LowCardinality(String(100)), px f64, side Enum8(buy, sell), small u32
";

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    let bytes = granite_schemac::compile("fuzz", SCHEMA.as_bytes()).expect("the schema compiles");
    Catalog::decode(&bytes).expect("a compiled catalog decodes")
});

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    let Ok(fragment) = Fragment::decode(bytes) else {
        return;
    };
    let config = &PROCESS_CONFIG_DEFAULT;
    let result = fragment.validate(&CATALOG, config);
    // By debug form, which tells NaN literals apart where `==` does not.
    assert_eq!(format!("{result:?}"), format!("{:?}", fragment.validate(&CATALOG, config)));
    if let Ok(report) = result {
        assert_eq!(report.memory.len(), fragment.operators.len() + 1);
        assert_eq!(report.total, report.memory.iter().sum::<u64>());
        assert!(report.total <= config.fragment_memory_max);
    }
});
