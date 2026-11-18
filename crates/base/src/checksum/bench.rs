//! Throughput of the two candidates of decision 0002. Ignored by default:
//!
//! ```text
//! cargo test --release --locked -p granite-base --lib checksum::bench -- --ignored --nocapture
//! ```
//!
//! Timing a benchmark needs a clock, so this test-only module is the one
//! place in `base` that uses `std::time`.

use super::{aegis, checksum, checksum_scalar};
use crate::constants::{FORMAT, GIB};
use std::hint::black_box;
use std::time::Instant;

type Checksum = fn(&[u8]) -> u128;

/// Seconds for `iterations` calls of `f` on `input`.
fn time(f: Checksum, input: &[u8], iterations: u64) -> f64 {
    let start = Instant::now();
    for _ in 0..iterations {
        black_box(f(black_box(input)));
    }
    start.elapsed().as_secs_f64()
}

/// GiB/s: the best of 5 runs of at least 50 ms each.
fn throughput(f: Checksum, input: &[u8]) -> f64 {
    let mut iterations = 1;
    while time(f, input, iterations) < 0.05 {
        iterations *= 2;
    }
    let best = (0..5)
        .map(|_| time(f, input, iterations))
        .fold(f64::MAX, f64::min);
    (input.len() as f64 * iterations as f64) / best / GIB as f64
}

#[test]
#[ignore = "benchmark; run it in release mode, see the module comment"]
fn checksum_throughput() {
    let sizes = [
        ("256 B", FORMAT.header_size),
        ("4 KiB", FORMAT.sector_size),
        ("128 KiB", FORMAT.grid_block_size),
        ("1 MiB", FORMAT.message_size_max),
    ];
    let candidates: [(&str, Checksum); 4] = [
        ("XXH3-128, SIMD kernel", checksum),
        ("XXH3-128, scalar", checksum_scalar),
        ("AEGIS-128L, AES instructions", aegis::hardware),
        ("AEGIS-128L, software AES", aegis::software),
    ];
    #[cfg(target_arch = "x86_64")]
    println!(
        "x86_64, avx2: {}, aes: {}",
        std::arch::is_x86_feature_detected!("avx2"),
        std::arch::is_x86_feature_detected!("aes")
    );
    #[cfg(target_arch = "aarch64")]
    println!(
        "aarch64, aes: {}",
        std::arch::is_aarch64_feature_detected!("aes")
    );
    let input: Vec<u8> = (0..FORMAT.message_size_max)
        .map(|i| i.wrapping_mul(31) as u8)
        .collect();
    print!("| GiB/s |");
    for (name, _) in sizes {
        print!(" {name} |");
    }
    println!("\n|---|---:|---:|---:|---:|");
    for (name, f) in candidates {
        print!("| {name} |");
        for (_, size) in sizes {
            print!(" {:.2} |", throughput(f, &input[..size as usize]));
        }
        println!();
    }
}
