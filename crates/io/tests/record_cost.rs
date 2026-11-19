//! The cost of recording one sample in a `Histogram` (T0.3.6), for the
//! report in `bench/results/`. Ignored by default, as only a release build on
//! the target hardware gives a meaningful number:
//!
//! cargo test --release --locked -p granite-io --test record_cost -- --ignored --nocapture

use std::hint::black_box;
use std::time::Instant;

use granite_io::histogram::Histogram;
use granite_io::memory::SplitMix64;

#[test]
#[ignore = "a measurement, run by hand in a release build"]
fn record_cost() {
    // A million durations from 1 µs to 65 ms, drawn before timing starts, so the loop
    // times recording alone.
    let mut random = SplitMix64(1);
    let values: Vec<u64> = (0..1_000_000)
        .map(|_| {
            let r = random.next_u64();
            (1_000 + r % 1_000) << (r >> 60)
        })
        .collect();
    let mut h = Histogram::new();
    const ROUNDS: u64 = 100;
    let start = Instant::now();
    for _ in 0..ROUNDS {
        for &v in &values {
            h.record(black_box(v));
        }
    }
    let elapsed = start.elapsed();
    let samples = ROUNDS * values.len() as u64;
    assert_eq!(black_box(&h).count(), samples);
    println!(
        "{samples} samples in {elapsed:?}: {:.2} ns per sample",
        elapsed.as_nanos() as f64 / samples as f64
    );
}
