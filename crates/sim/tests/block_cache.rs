//! The gates of T1.1.7, the block cache: the model test against a map of
//! the blocks live at each address, memory and allocations over 10,000,000
//! random lookups, the hit rate on a Zipfian workload, and the cost of a hit
//! (ignored, run by hand in a release build for the report). The counter
//! and release-and-reuse gates are unit tests of `granite_storage::cache`.
//! Seeds start at `GRANITE_SEED` when it is set.

#[path = "../../io/tests/common/counting_alloc.rs"]
mod counting_alloc;

use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::Instant;

use granite_io::memory::SplitMix64;
use granite_storage::cache::{Cache, WAYS};
use granite_storage::grid::{BLOCK_SIZE, Reference};

fn seed(test: &str) -> u64 {
    let seed = std::env::var("GRANITE_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    println!("{test}: GRANITE_SEED={seed}");
    seed
}

/// Writes into `block` a header carrying `reference`'s checksum and, after
/// it, a stamp of the reference, so a returned block shows whose it is.
fn fill(block: &mut [u8], reference: Reference) {
    block[..16].copy_from_slice(&reference.checksum.to_le_bytes());
    block[16..24].copy_from_slice(&reference.address.to_le_bytes());
    block[24..40].copy_from_slice(&reference.checksum.to_le_bytes());
}

#[test]
fn model_a_lookup_returns_only_the_block_it_names() {
    const CASES: u64 = 1_000;
    const OPS: u64 = 1_000;
    let first = seed("model");
    let mut block = vec![0; BLOCK_SIZE];
    let (mut hits, mut stale) = (0u64, 0u64);
    for case in first..first + CASES {
        let mut random = SplitMix64(case);
        // Two sets and twice as many addresses, so sets fill and evict;
        // four checksums, so stale and wrong references are common.
        let mut cache = Cache::new(2 * WAYS);
        let mut live = BTreeMap::new();
        for _ in 0..OPS {
            let r = random.next_u64();
            let reference = Reference {
                address: (r >> 8) % (4 * WAYS as u64),
                checksum: (u128::from((r >> 4) % 4) << 100) | 1,
            };
            match r % 8 {
                // An insert, then the block it just cached must hit.
                0..=2 => {
                    fill(&mut block, reference);
                    cache.insert(reference, &block);
                    live.insert(reference.address, reference.checksum);
                    assert_eq!(
                        cache.get(reference).map(|b| b[..40] == block[..40]),
                        Some(true)
                    );
                }
                3 => {
                    cache.release(reference.address);
                    live.remove(&reference.address);
                }
                _ => match cache.get(reference) {
                    Some(got) => {
                        hits += 1;
                        assert_eq!(
                            live.get(&reference.address),
                            Some(&reference.checksum),
                            "seed {case}"
                        );
                        fill(&mut block, reference);
                        assert_eq!(got[..40], block[..40], "seed {case}");
                    }
                    None => {
                        stale += u64::from(
                            live.get(&reference.address)
                                .is_some_and(|&c| c != reference.checksum),
                        )
                    }
                },
            }
        }
    }
    // The run tests something: lookups hit, and others named a stale checksum.
    println!("model: {hits} checked hits, {stale} misses on a stale checksum");
    assert!(hits > CASES * OPS / 100 && stale > CASES * OPS / 100);
}

#[test]
fn memory_and_allocations_stay_fixed_over_ten_million_random_lookups() {
    const BLOCKS: usize = 8 * WAYS;
    const LOOKUPS: u64 = 10_000_000;
    let mut random = SplitMix64(seed("memory"));
    let mut cache = Cache::new(BLOCKS);
    let mut block = vec![0; BLOCK_SIZE];
    // A set header is 8 addresses, 8 checksums and two bytes, padded to 208.
    let memory = BLOCKS * BLOCK_SIZE + BLOCKS / WAYS * 208;
    assert_eq!(cache.memory(), memory);
    let before = counting_alloc::allocations();
    for _ in 0..LOOKUPS {
        let r = random.next_u64();
        // Four times as many addresses as blocks, any address in u64.
        let reference = Reference {
            address: r % (4 * BLOCKS as u64) * 0x1_0000_0001,
            checksum: u128::from(r % (4 * BLOCKS as u64)),
        };
        // Insert on one miss in 256, to keep the block copies few.
        if cache.get(reference).is_none() && r >> 56 == 0 {
            fill(&mut block, reference);
            cache.insert(reference, &block);
        } else if r >> 56 == 1 {
            cache.release(reference.address);
        }
    }
    let allocations = counting_alloc::allocations() - before;
    let stats = cache.stats();
    println!(
        "memory: {} bytes, {allocations} allocations, {stats:?}",
        cache.memory()
    );
    assert_eq!(stats.hits + stats.misses, LOOKUPS);
    assert!(stats.evictions > 0 && stats.hits > 0);
    assert_eq!(allocations, 0, "allocations during {LOOKUPS} lookups");
    assert_eq!(cache.memory(), memory);
}

#[test]
fn zipfian_hit_rate_with_a_tenth_of_the_keys_is_at_least_sixty_percent() {
    const KEYS: usize = 4_000;
    const LOOKUPS: u64 = 200_000;
    // The cumulative weights of ranks 1..=KEYS for exponent 0.99, scaled to u64.
    let weights: Vec<f64> = (1..=KEYS).map(|k| (k as f64).powf(-0.99)).collect();
    let total: f64 = weights.iter().sum();
    let mut sum = 0.0;
    let cdf: Vec<u64> = weights
        .iter()
        .map(|w| {
            sum += w;
            (sum / total * u64::MAX as f64) as u64
        })
        .collect();
    let mut random = SplitMix64(seed("zipf"));
    let mut cache = Cache::new(KEYS / 10);
    let mut block = vec![0; BLOCK_SIZE];
    for _ in 0..LOOKUPS {
        let rank = cdf
            .partition_point(|&c| c < random.next_u64())
            .min(KEYS - 1);
        let reference = Reference {
            address: rank as u64,
            checksum: rank as u128 + 1,
        };
        if cache.get(reference).is_none() {
            fill(&mut block, reference);
            cache.insert(reference, &block);
        }
    }
    let stats = cache.stats();
    println!(
        "zipf 0.99, {KEYS} keys, {} blocks, {LOOKUPS} lookups: hit rate {:.2}%, {stats:?}",
        KEYS / 10,
        stats.hit_rate_ppm() as f64 / 10_000.0
    );
    assert!(stats.hit_rate_ppm() >= 600_000);
}

/// The cost of a hit, for the report in `bench/results/`: only a release
/// build on the target hardware gives a meaningful number.
///
/// cargo test --release --locked -p granite-sim --test block_cache -- --ignored --nocapture
#[test]
#[ignore = "a measurement, run by hand in a release build"]
fn hit_cost() {
    const BLOCKS: usize = 128 * WAYS;
    let mut cache = Cache::new(BLOCKS);
    let mut block = vec![0; BLOCK_SIZE];
    // Fill every slot from four times as many addresses, then keep those cached.
    let mut cached = Vec::new();
    for address in 0..BLOCKS as u64 * 4 {
        let reference = Reference {
            address,
            checksum: u128::from(address) + 1,
        };
        fill(&mut block, reference);
        cache.insert(reference, &block);
    }
    for address in 0..BLOCKS as u64 * 4 {
        let reference = Reference {
            address,
            checksum: u128::from(address) + 1,
        };
        if cache.get(reference).is_some() {
            cached.push(reference);
        }
    }
    // A million hits in a random order, drawn before timing starts.
    let mut random = SplitMix64(1);
    let lookups: Vec<Reference> = (0..1_000_000)
        .map(|_| cached[(random.next_u64() % cached.len() as u64) as usize])
        .collect();
    const ROUNDS: u64 = 100;
    let start = Instant::now();
    let mut bytes = 0u64;
    for _ in 0..ROUNDS {
        for &reference in &lookups {
            bytes += u64::from(black_box(cache.get(black_box(reference))).expect("a hit")[40]);
        }
    }
    let elapsed = start.elapsed();
    let hits = ROUNDS * lookups.len() as u64;
    black_box(bytes);
    println!(
        "{hits} hits over {} cached blocks in {elapsed:?}: {:.2} ns per hit",
        cached.len(),
        elapsed.as_nanos() as f64 / hits as f64
    );
}
