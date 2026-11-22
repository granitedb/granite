//! The program text compiler on arbitrary bytes: it never panics or hangs,
//! and whatever it accepts prints as text that compiles to the same IR, and
//! reports. The catalog has the tables of the validator tests
//! (`crates/txn/tests/support`) and of the reference schemas' programs.

#![no_main]

use granite_schema::Catalog;
use granite_schemac::{compile, compile_program, print_program, report};
use std::collections::BTreeSet;
use std::sync::LazyLock;

const SCHEMA: &str = "
schema version 1
table group users partition by (user u64) hash
table account versioned layout row in users
  key (user)
  user u64, balance Decimal(18, 2), nick Nullable(String(8)), active bool
table entry immutable layout row in users
  key (user, id)
  user u64, id u32, amount Decimal(18, 2), kind Enum8(4)
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
    let bytes = compile("fuzz", SCHEMA.as_bytes()).expect("the schema compiles");
    Catalog::decode(&bytes).expect("a compiled catalog decodes")
});

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    let Ok(program) = compile_program("fuzz", bytes, &CATALOG) else {
        return;
    };
    let names: BTreeSet<&str> = program.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names.len(), program.params.len(), "the compiler rejects repeated names");
    let text = print_program(&CATALOG, &program);
    let printed = compile_program("printed", text.as_bytes(), &CATALOG).expect("printed text compiles");
    assert_eq!(printed.encode(), program.encode(), "{text}");
    report(&CATALOG, &program);
});
