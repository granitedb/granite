//! The schema text compiler on arbitrary bytes: it never panics or hangs,
//! and whatever it accepts prints as text that compiles to the same bytes.

#![no_main]

use granite_schema::Catalog;
use granite_schemac::{compile, print};

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    if let Ok(catalog) = compile("fuzz", bytes) {
        let text = print(&Catalog::decode(&catalog).expect("compiled catalogs decode"));
        assert_eq!(compile("printed", text.as_bytes()), Ok(catalog));
    }
});
