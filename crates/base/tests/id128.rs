//! The Id128 generator's gates (T0.2.4), from a logged seed. Rerun a failure
//! with `GRANITE_SEED=<seed> cargo test -p granite-base --test id128`.
//!
//! The long runs never store their ids: each id is compared with the one
//! before it, and two generators are checked by merging their two increasing
//! streams.

use granite_base::id128::{Id128Generator, RANDOM_BITS, RANDOM_MASK};

/// 2025-11-18, in milliseconds since the Unix epoch.
const START_MS: u64 = 1_763_500_000_000;
const SECOND_MS: u64 = 1000;
const HOUR_MS: u64 = 3600 * SECOND_MS;

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn new(test: &str) -> Self {
        let seed = std::env::var("GRANITE_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0x5EED);
        println!("{test}: GRANITE_SEED={seed}");
        Rng(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn u128(&mut self) -> u128 {
        (u128::from(self.next()) << 64) | u128::from(self.next())
    }
}

#[test]
fn ten_million_ids_strictly_increase() {
    let mut rng = Rng::new("ten_million_ids_strictly_increase");
    let mut generator = Id128Generator::new();
    let (mut now, mut prev) = (START_MS, 0);
    for _ in 0..10_000_000 {
        // About 1,000 ids per millisecond, with runs that cross milliseconds.
        now += u64::from(rng.next().is_multiple_of(1000));
        let id = generator.next(now, rng.u128());
        assert!(id > prev, "{id:#x} after {prev:#x}");
        prev = id;
    }
    assert!(prev >> RANDOM_BITS > u128::from(START_MS));
}

/// Ids made before, during and after the clock jumps back by `back_ms` and
/// returns, all strictly increasing.
fn clock_jumps_back(test: &str, back_ms: u64) {
    let mut rng = Rng::new(test);
    let mut generator = Id128Generator::new();
    let mut prev = 0;
    let ahead = START_MS + back_ms;
    // Before the jump, during it, the clock catching up, and past it.
    let times = (0..1000)
        .map(|_| ahead)
        .chain((0..1000).map(|_| START_MS))
        .chain((0..1000).map(|i| START_MS + back_ms * i / 1000))
        .chain((1..1000).map(|i| ahead + i));
    for now in times {
        let id = generator.next(now, rng.u128());
        assert!(id > prev, "{test}: {id:#x} after {prev:#x} at {now} ms");
        prev = id;
    }
    // Once the clock has passed the jump, ids carry the clock's time again.
    assert_eq!(prev >> RANDOM_BITS, u128::from(ahead + 999));
}

#[test]
fn clock_back_one_millisecond() {
    clock_jumps_back("clock_back_one_millisecond", 1);
}

#[test]
fn clock_back_one_second() {
    clock_jumps_back("clock_back_one_second", SECOND_MS);
}

#[test]
fn clock_back_one_hour() {
    clock_jumps_back("clock_back_one_hour", HOUR_MS);
}

/// The random field holds 2^80 ids per millisecond, too many to create in a
/// test, so the first id starts 2^19 below the field's top: of the 2^20 ids
/// made at one frozen millisecond, the second half has to borrow from the
/// timestamp.
#[test]
fn frozen_clock_borrows_from_the_timestamp() {
    let mut rng = Rng::new("frozen_clock_borrows_from_the_timestamp");
    let mut generator = Id128Generator::new();
    let count = 2u128.pow(20);
    let first = generator.next(START_MS, RANDOM_MASK - count / 2 + 1);
    let mut prev = first;
    for _ in 1..count {
        let id = generator.next(START_MS, rng.u128());
        assert!(id > prev, "{id:#x} after {prev:#x}");
        prev = id;
    }
    // Strictly increasing by exactly one each time, so no duplicate and no gap.
    assert_eq!(prev - first, count - 1);
    assert_eq!(first >> RANDOM_BITS, u128::from(START_MS));
    assert_eq!(prev >> RANDOM_BITS, u128::from(START_MS) + 1);
    assert_eq!(prev & RANDOM_MASK, count / 2 - 1);
    // The clock reaching the borrowed millisecond still gives a larger id.
    assert!(generator.next(START_MS + 1, 0) > prev);
}

#[test]
fn two_generators_never_collide_in_100_million_ids() {
    let mut rng = Rng::new("two_generators_never_collide_in_100_million_ids");
    // Two generators with different random sources, seeded from the logged one.
    let mut sources = [Rng(rng.next()), Rng(rng.next())];
    let mut generators = [Id128Generator::new(), Id128Generator::new()];
    // One shared clock that moves every 2,000 ids, so both generators create
    // ids in the same milliseconds.
    let clock = |made: u64| START_MS + made / 2000;
    let mut made = 0;
    let mut heads = [0, 0];
    for g in 0..2 {
        heads[g] = generators[g].next(clock(made), sources[g].u128());
        made += 1;
    }
    while made < 100_000_000 {
        assert_ne!(heads[0], heads[1], "collision after {made} ids");
        let g = usize::from(heads[1] < heads[0]);
        let id = generators[g].next(clock(made), sources[g].u128());
        assert!(id > heads[g]);
        heads[g] = id;
        made += 1;
    }
    assert_ne!(heads[0], heads[1]);
}

/// The key encoding of an Id128 is the u128 big-endian (T1.4.1); the golden
/// vectors of `crates/types` pin those bytes.
#[test]
fn byte_encoding_matches_the_key_encoding() {
    let vectors = include_str!("../../types/tests/golden_vectors.txt");
    let mut lines = vectors.lines();
    let mut checked = 0;
    while let Some(line) = lines.next() {
        let Some(value) = line.strip_prefix("Id128 Id128(") else {
            continue;
        };
        let id: u128 = value.trim_end_matches(')').parse().unwrap();
        let asc = lines.next().unwrap().trim().strip_prefix("asc ").unwrap();
        let bytes = id.to_be_bytes();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, asc, "Id128({id})");
        checked += 1;
    }
    assert!(checked >= 2, "no Id128 golden vectors found");
}

#[test]
fn byte_encoding_sorts_like_the_number() {
    let mut rng = Rng::new("byte_encoding_sorts_like_the_number");
    let mut generator = Id128Generator::new();
    let mut now = START_MS;
    for _ in 0..1_000_000 {
        now += rng.next() % 3;
        let a = generator.next(now, rng.u128());
        // b shares a random number of leading bits with a, so near-ties are common.
        let keep = (rng.next() % 129) as u32;
        let low = u128::MAX.checked_shr(keep).unwrap_or(0);
        let b = (a & !low) | (rng.u128() & low);
        assert_eq!(
            a.to_be_bytes().cmp(&b.to_be_bytes()),
            a.cmp(&b),
            "{a:#x} {b:#x}"
        );
    }
}

#[test]
#[should_panic(expected = "does not fit")]
fn timestamp_beyond_48_bits_is_rejected() {
    Id128Generator::new().next(1 << 48, 0);
}
