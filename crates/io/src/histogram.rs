//! A histogram of nanosecond durations with a fixed set of buckets, for the
//! stall metric of the event loop (document 02 section 1).
//!
//! Values below 64 have a bucket each. Above that, each power of two is cut
//! into 32 buckets of equal width, so a bucket is at most 1/32 of its lower
//! bound wide, and a quantile, reported as the middle of its bucket, is
//! within 1/64 of the true value. Recording is a few instructions and never
//! allocates; the buckets are allocated once, in [`Histogram::new`].

/// Bits of a value kept below its leading one.
const SUB_BITS: u32 = 5;
/// Buckets per power of two.
const SUB: u64 = 1 << SUB_BITS;
/// Buckets in all: values below `2 * SUB` exactly, then `SUB` per power of
/// two up to `2^63`.
const BUCKETS: usize = (64 - SUB_BITS as usize + 1) * SUB as usize;

/// Counts of durations in nanoseconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Histogram {
    buckets: Box<[u64; BUCKETS]>,
    count: u64,
    max: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

impl Histogram {
    /// An empty histogram.
    pub fn new() -> Self {
        Histogram {
            buckets: Box::new([0; BUCKETS]),
            count: 0,
            max: 0,
        }
    }

    /// Counts one value.
    pub fn record(&mut self, value: u64) {
        self.buckets[index(value)] += 1;
        self.count += 1;
        self.max = self.max.max(value);
    }

    /// How many values were recorded.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// The largest value recorded, exactly; zero if none was.
    pub fn max(&self) -> u64 {
        self.max
    }

    /// The value below or at which `per_million` millionths of the recorded
    /// values lie (999,000 for p99.9), within 1/64 of it; zero if none was
    /// recorded.
    pub fn quantile(&self, per_million: u64) -> u64 {
        assert!(per_million <= 1_000_000);
        if self.count == 0 {
            return 0;
        }
        // The rank of the value, from 1: the smallest that covers the share.
        let rank = (u128::from(self.count) * u128::from(per_million)).div_ceil(1_000_000);
        let rank = u64::try_from(rank).expect("at most count").max(1);
        if rank == self.count {
            return self.max;
        }
        let mut seen = 0;
        for (i, &n) in self.buckets.iter().enumerate() {
            seen += n;
            if seen >= rank {
                let (low, width) = bounds(i);
                return (low + (width - 1) / 2).min(self.max);
            }
        }
        unreachable!("the buckets hold every recorded value")
    }
}

/// The bucket of `value`.
fn index(value: u64) -> usize {
    if value < SUB {
        return value as usize;
    }
    let top = 63 - value.leading_zeros();
    // The `SUB_BITS` bits after the leading one.
    let sub = (value >> (top - SUB_BITS)) - SUB;
    ((u64::from(top - SUB_BITS) + 1) * SUB + sub) as usize
}

/// The lowest value of bucket `i`, and how many values it covers.
fn bounds(i: usize) -> (u64, u64) {
    let i = i as u64;
    if i < 2 * SUB {
        return (i, 1);
    }
    let shift = i / SUB - 1;
    ((SUB + i % SUB) << shift, 1 << shift)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::SplitMix64;

    #[test]
    fn buckets_cover_every_value_in_order() {
        assert_eq!(index(u64::MAX), BUCKETS - 1);
        let mut previous = 0;
        for i in 0..BUCKETS {
            let (low, width) = bounds(i);
            assert_eq!(index(low), i);
            assert_eq!(index(low + (width - 1)), i);
            if i > 0 {
                assert_eq!(low, previous, "bucket {i} starts where {} ended", i - 1);
            }
            if low >= 2 * SUB {
                assert!(
                    width * SUB <= low,
                    "bucket {i} is at most 1/32 of its value"
                );
            }
            previous = low.wrapping_add(width);
        }
        assert_eq!(previous, 0, "the last bucket ends at 2^64");
    }

    #[test]
    fn small_values_and_the_max_are_exact() {
        let mut h = Histogram::new();
        assert_eq!((h.count(), h.max(), h.quantile(999_000)), (0, 0, 0));
        for v in 0..64 {
            h.record(v);
        }
        assert_eq!((h.count(), h.max()), (64, 63));
        assert_eq!(h.quantile(0), 0);
        assert_eq!(h.quantile(500_000), 31);
        assert_eq!(h.quantile(1_000_000), 63);
        h.record(u64::MAX);
        assert_eq!(h.quantile(1_000_000), u64::MAX);
    }

    /// The gate: p99.9 within 5% of the true value, for a synthetic
    /// distribution spread over six orders of magnitude, whose true p99.9 is
    /// found by sorting.
    #[test]
    fn quantiles_are_within_a_sixty_fourth() {
        let mut random = SplitMix64(42);
        let mut values: Vec<u64> = (0..1_000_000)
            .map(|_| {
                // A heavy tail: 1 to 2 µs times a power of two up to 2^15,
                // so 1 µs to 65 ms.
                let r = random.next_u64();
                (1_000 + r % 1_000) << (r >> 60)
            })
            .collect();
        let mut h = Histogram::new();
        for &v in &values {
            h.record(v);
        }
        values.sort_unstable();
        for per_million in [500_000, 990_000, 999_000, 999_900] {
            let rank = (values.len() as u64 * per_million).div_ceil(1_000_000);
            let truth = values[rank as usize - 1];
            let got = h.quantile(per_million);
            let error = got.abs_diff(truth) as f64 / truth as f64;
            println!("p{per_million}: true {truth}, reported {got}, error {error:.4}");
            assert!(error <= 1.0 / 64.0, "p{per_million}: {got} for {truth}");
        }
    }
}
