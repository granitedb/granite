//! The catalog decoder on arbitrary bytes: it never panics, and whatever it
//! accepts is a valid catalog that encodes back to exactly the same bytes.

#![no_main]

use granite_schema::Catalog;

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    if let Ok(catalog) = Catalog::decode(bytes) {
        assert_eq!(catalog.encode().as_deref(), Ok(bytes));
    }
});
