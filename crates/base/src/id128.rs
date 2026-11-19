//! Time-ordered 128-bit ids, created by clients (document 03 section 2.1).
//!
//! The layout is part of the format: the Unix time in milliseconds in the
//! high [`TIMESTAMP_BITS`] bits, random bits in the low [`RANDOM_BITS`]. The
//! key encoding stores the u128 big-endian, so ids sort by creation time.
//!
//! The generator reads no clock and no randomness itself: the caller passes
//! both, which keeps it deterministic under the simulator.

/// Bits of the millisecond timestamp, the high bits of an id.
pub const TIMESTAMP_BITS: u32 = 48;
/// Bits of the random field, the low bits of an id.
pub const RANDOM_BITS: u32 = 80;
/// The mask of the random field.
pub const RANDOM_MASK: u128 = (1 << RANDOM_BITS) - 1;

/// Creates ids that strictly increase within one generator.
///
/// When the clock has moved past the last id's millisecond, the id is that
/// millisecond and fresh random bits. Otherwise (the same millisecond, or a
/// clock that went backwards) it is the last id plus one; when the random
/// field is full the carry moves into the timestamp, so the generator borrows
/// from future milliseconds instead of repeating or going down.
#[derive(Debug, Default)]
pub struct Id128Generator {
    last: u128,
}

impl Id128Generator {
    /// A generator that has created no id.
    pub const fn new() -> Self {
        Id128Generator { last: 0 }
    }

    /// Returns the next id for the wall-clock time `now_ms` (milliseconds
    /// since the Unix epoch) and `random`, of which only the low
    /// [`RANDOM_BITS`] bits are used.
    pub fn next(&mut self, now_ms: u64, random: u128) -> u128 {
        assert!(
            now_ms >> TIMESTAMP_BITS == 0,
            "Id128 timestamp {now_ms} ms does not fit in {TIMESTAMP_BITS} bits"
        );
        let id = if u128::from(now_ms) > self.last >> RANDOM_BITS {
            (u128::from(now_ms) << RANDOM_BITS) | (random & RANDOM_MASK)
        } else {
            self.last.checked_add(1).expect("Id128 space exhausted")
        };
        assert!(id > self.last, "Id128 did not increase");
        self.last = id;
        id
    }
}
