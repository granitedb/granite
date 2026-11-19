//! Rejected: an `unsafe` block without a `SAFETY:` comment, in a module that
//! may use `unsafe`.

#![allow(unsafe_code, reason = "the fixture is linted as a listed module")]

pub fn first(bytes: &[u8]) -> u8 {
    assert!(!bytes.is_empty());
    unsafe { *bytes.get_unchecked(0) }
}
