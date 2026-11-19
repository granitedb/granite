//! Accepted in crates/types: checked arithmetic, conversions with From and
//! TryFrom, `use ... as` renames, and anything in a test module.

use std::fmt::Write as _;
use std::num::TryFromIntError as Narrowing;

pub fn add(a: u64, b: u64) -> Option<u64> {
    a.checked_add(b)
}

pub fn widen(a: u32) -> u64 {
    u64::from(a)
}

/// Says "x as u8" and "wrapping_add" only in text.
pub fn narrow(a: u64) -> Result<u8, Narrowing> {
    let mut alias = String::new();
    let _ = write!(alias, "a as u8, wrapping_add"); // a as u8
    u8::try_from(a)
}

#[cfg(test)]
mod tests {
    #[test]
    fn tests_may_wrap() {
        assert_eq!(u8::MAX.wrapping_add(1), 0);
        assert_eq!(300u32 as u8, 44);
    }
}
