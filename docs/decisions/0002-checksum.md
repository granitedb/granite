# 0002. The 128-bit checksum

Status: proposed on 2026-11-18. It becomes accepted when the report on x86-64 with AES-NI and AVX2 (section "Open") confirms the comparison below on production-like hardware (task T0.2.1).

## Question

Which function computes the 128-bit checksums of headers, prepares, blocks and chunks (document 01 section 2, document 05 section 1)? The value is stored on disk and sent on the wire, so the choice is part of the format and cannot change cheaply. The candidates (document 01) are AEGIS-128L, used by TigerBeetle, and XXH3-128.

## Decision

XXH3-128 as specified by xxHash 0.8 (`XXH3_128bits`): the default secret, seed 0, and the value `high64 << 64 | low64` as a `u128`, which is xxHash's canonical 128-bit value. How a `u128` is laid out in a header is decided with the header struct (T0.2.2).

It is written in `crates/base/src/checksum.rs` (no dependency) and reached only through `granite_base::checksum::checksum`. A scalar path and SIMD kernels compute the long-input loop: AVX2 on x86-64, chosen at run time, and NEON on aarch64. An x86-64 machine without AVX2 runs the scalar loop, which is correct but slower; production requires x86-64-v3 (document 06 section 0).

## Numbers

These are from a laptop, so they are relative comparisons on one machine (document 06 section 0). They do **not** meet the gate that asks for a report on real hardware; that report is still open.

| Item | Value |
|---|---|
| Machine | Apple M4 Pro, 14 cores, 24 GiB, macOS 27.0.1, aarch64 with AES instructions |
| Toolchain | rustc 1.96.1 (Homebrew), the workspace release profile (debug assertions and overflow checks on) |
| Commit | 4fb5acd (the code of this change before the fault-model test was added; the benchmarked code is unchanged since) |
| Command | `cargo test --release --locked -p granite-base --lib checksum::bench -- --ignored --nocapture` |
| Method | `crates/base/src/checksum/bench.rs`: for each function and size, the best of 5 runs of at least 50 ms each, one thread. The machine was shared with other builds (load average 12 to 17), so the best of several runs is the fairer figure. |

Throughput in GiB/s, median of three runs of the command:

| Function | 256 B (header) | 4 KiB (sector) | 128 KiB (block) | 1 MiB (largest message) |
|---|---:|---:|---:|---:|
| XXH3-128, NEON kernel | 23.89 | 35.74 | 34.28 | 33.46 |
| XXH3-128, scalar | 13.70 | 23.29 | 23.07 | 23.17 |
| AEGIS-128L, AES instructions | 7.60 | 19.13 | 20.94 | 20.69 |
| AEGIS-128L, software AES | 0.18 | 0.51 | 0.58 | 0.58 |

Raw output of the three runs:

```text
| XXH3-128, SIMD kernel | 24.07 | 36.07 | 33.97 | 33.46 |
| XXH3-128, scalar | 13.70 | 23.29 | 23.07 | 23.17 |
| AEGIS-128L, AES instructions | 7.60 | 19.13 | 21.31 | 20.81 |
| AEGIS-128L, software AES | 0.18 | 0.52 | 0.58 | 0.58 |

| XXH3-128, SIMD kernel | 23.89 | 35.74 | 34.42 | 33.44 |
| XXH3-128, scalar | 12.28 | 20.90 | 21.83 | 22.82 |
| AEGIS-128L, AES instructions | 7.56 | 18.84 | 20.39 | 20.69 |
| AEGIS-128L, software AES | 0.18 | 0.51 | 0.57 | 0.57 |

| XXH3-128, SIMD kernel | 23.81 | 35.63 | 34.28 | 34.02 |
| XXH3-128, scalar | 14.16 | 23.68 | 23.24 | 23.32 |
| AEGIS-128L, AES instructions | 7.71 | 19.29 | 20.94 | 20.50 |
| AEGIS-128L, software AES | 0.18 | 0.51 | 0.58 | 0.58 |
```

So that neither candidate loses through a weak implementation, the reference implementations were timed on the same machine, outside the repository, with the same input sizes: xxHash 0.8.3's `XXH3_128bits` (C, clang -O3, NEON) reached 21.3, 44.5, 42.9 and 43.7 GiB/s, and Zig 0.17's `Aegis128LMac_128` (ReleaseFast) 6.8, 18.1, 20.8 and 20.7 GiB/s. Our AEGIS-128L matches its reference; our XXH3-128 reaches 77 to 112 percent of its reference. xxHash's NEON code gets its extra speed by running two of the eight lanes as scalar code on Apple cores; that tuning does not apply to x86-64.

## Fault model

Document 01 section 1 lists the faults a checksum must catch.

- **Bit flips (bit rot).** Both functions give 128 bits that behave as random for inputs nobody chose to collide, so a random corruption goes undetected with probability about 2^-128. Neither guarantees detection of short bursts the way a CRC does for bursts up to its width; at 128 bits the difference has no practical weight. The test `detects_bit_flips_and_torn_writes` checks that all 2,048 single-bit flips of a header and all 32,768 of a sector give distinct checksums.
- **Torn writes.** A torn block mixes new and old sectors; to the checksum this is one more corrupted input, caught with the same probability. The same test tears a 128 KiB block at every sector boundary.
- **Misdirected reads and writes.** No checksum detects correct data at the wrong place by itself. The format puts the block's own address and type among the checksummed bytes (document 01 section 2), which works the same with either function.
- **Deliberate collisions.** Not in the fault model. AEGIS-128L with a public all-zero key gives no protection against an attacker either; integrity against attackers on the network comes from mutual TLS.

The two candidates are equal on detection.

## Machines without AES instructions

AEGIS-128L is built from AES rounds. With AES in software it ran at 0.18 to 0.58 GiB/s here, 35 to 42 times slower than with the AES instructions. Every production CPU (x86-64-v3) and current ARM laptops have the instructions, but a scalar reference path and any machine without them would pay that price. A software AES that does not leak through cache timing would be slower still, though timing does not matter for a checksum.

XXH3-128 needs only 64-bit multiplications. Its scalar path ran at 13.7 to 23.3 GiB/s, and SSE2 and NEON, which every x86-64 and aarch64 CPU has, are enough for a SIMD kernel.

## Why XXH3-128

1. It was faster at every size, by the widest margin at 256 bytes (3.1 times), the size of every message header. AEGIS-128L pays a fixed cost on every call: 10 state updates to start and 7 to finish, whatever the length.
2. Its scalar path stays fast, so the scalar reference path that development and tests rely on (DESIGN.md, "CPU") is practical. AEGIS-128L without AES instructions is about 40 times slower.
3. It is easier to own: about 250 lines of scalar code and two small kernels, all checked against one reference. AEGIS-128L needs an AES round on each platform, plus a software AES.

## Rejected

- **AEGIS-128L** (MAC, 128-bit tag, all-zero key and nonce, as TigerBeetle uses it): slower here, especially for headers, and much slower without AES instructions, for no gain in detection. It is kept as test-only code in `crates/base/src/checksum/aegis.rs`, checked against Zig's standard library, so the benchmark can be rerun on x86-64. This decision is revisited if the x86-64 report shows AEGIS-128L clearly faster at the block and message sizes while staying close at 256 bytes.
- **A dependency** (`xxhash-rust` or `twox-hash`, `aegis`) instead of our own code: the function is part of the format and the third-party code would have to be vendored and pinned anyway (document 06 section 3). Writing it ourselves adds no dependency.

## Evidence for the other gates of T0.2.1

- Scalar and SIMD give identical output: `checksum::tests::simd_matches_scalar` compares them on 1,000,000 inputs of random length (log-uniform up to 4 KiB, so most take the long path) from the logged seed `0x0067_7261_6e69_7465`. On x86-64 the test fails if the machine has no AVX2, so a CI run on x86-64 always exercises the AVX2 kernel. CI runs it on ubuntu-24.04 x86-64 (AVX2) and on macos-15 arm64 (NEON).
- Fixed test vectors: `checksum::tests::vectors` (42 vectors, every length class from empty and one byte to 131,073 bytes) and `checksum::tests::every_length_to_5000` (one fold over every length from 0 to 5,000), all computed by xxHash 0.8.3 and identical from its scalar, NEON, SSE2 and AVX2 builds. Both check the SIMD and the scalar path.
- One API: everything but `checksum` is private to `granite_base::checksum`, so the compiler rejects other calls, and `crates/base/tests/checksum_api.rs` fails if any manifest or Rust file outside `crates/base` names XXH3, xxHash or AEGIS.
- Injected bug: changing the lane swap in the NEON kernel (`vextq_u64::<1>` to `vextq_u64::<0>`) made `vectors`, `every_length_to_5000` and `simd_matches_scalar` fail (`seed 0x6772616e697465, case 2, length 1254`).

## Note on the kernels

`_mm256_loadu_si256` and `vld1q_u8` copy through `copy_nonoverlapping`, whose precondition checks stay enabled in our release profile because debug assertions are on. With them the NEON kernel ran at 8 GiB/s; reading byte arrays and transmuting them to vectors brought it to 34 GiB/s. Other SIMD code in the project should load the same way.

## Open: report on x86-64

Run on an x86-64 server with AES-NI and AVX2 (document 06 section 0), from a clean checkout of the commit that adds this file:

```bash
lscpu | grep -E 'Model name|Flags' | grep -oE 'Model name.*|avx2|aes' | sort -u
rustc --version
git rev-parse HEAD
cargo test --release --locked -p granite-base --lib checksum::bench -- --ignored --nocapture
```

Add the output as a section "Report on x86-64" here, with the machine, and set the status to accepted, or reopen the decision.
