//! Rejected in crates/types, crates/txn and crates/shard: arithmetic that
//! wraps, saturates or skips its overflow check, and `as` casts.

use std::num::{Saturating, Wrapping};

pub fn wrap(a: u64, b: u64) -> u64 {
    a.wrapping_add(b)
}

pub fn saturate(a: i32, b: i32) -> i32 {
    a.saturating_sub(b)
}

pub fn wrapped_with_flag(a: u8, b: u8) -> (u8, bool) {
    a.overflowing_mul(b)
}

pub fn path_call(a: u16) -> u16 {
    u16::wrapping_neg(a)
}

pub fn reinterpret(a: i64) -> u64 {
    a.cast_unsigned()
}

pub fn unchecked() {
    let _add = u32::unchecked_add;
}

pub fn wrapper(a: Wrapping<u8>) -> Saturating<u8> {
    Saturating(a.0)
}

pub fn truncate(a: u64) -> u8 {
    a as u8
}

pub fn inferred(a: i64) -> u32 {
    a as _
}

pub fn through_a_path(a: u64) -> u8 {
    a as core::primitive::u8
}

pub fn through_an_alias(a: u64) -> Small {
    a as Small
}

pub type Small = u8;
