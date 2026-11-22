//! The fragment decoder on arbitrary bytes: it never panics, and whatever
//! it accepts encodes back to exactly the same bytes. Run with
//! `-malloc_limit_mb=2` so that any allocation of 2 MiB or more fails the
//! run (document 05 section 4).

#![no_main]

use granite_query::Fragment;

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    if let Ok(fragment) = Fragment::decode(bytes) {
        assert_eq!(fragment.encode().as_deref(), Ok(bytes));
    }
});
