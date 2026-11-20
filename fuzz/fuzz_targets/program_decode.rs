//! The program decoder on arbitrary bytes: it never panics, and whatever it
//! accepts encodes back to exactly the same bytes. Run with
//! `-malloc_limit_mb=2` so that any allocation of 2 MiB or more fails the
//! run (document 03 section 5.1).

#![no_main]

use granite_txn::Program;

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    if let Ok(program) = Program::decode(bytes) {
        assert_eq!(program.encode().as_deref(), Ok(bytes));
    }
});
