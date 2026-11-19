//! Rejected: `unsafe` in a module that is not listed in tools/lint.py, even
//! with a safety argument and the lint allowed locally.

#![allow(unsafe_code, reason = "the fixture shows that a local allow is not enough")]

pub fn first(bytes: &[u8]) -> u8 {
    assert!(!bytes.is_empty());
    // SAFETY: the slice is not empty, checked above.
    unsafe { *bytes.get_unchecked(0) }
}
