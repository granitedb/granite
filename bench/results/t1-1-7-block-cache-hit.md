# T1.1.7: cost of a block cache hit

Status: development-machine numbers only. The gate asks for under 100 ns on
real hardware, which [06-testing-and-engineering.md](../../docs/06-testing-and-engineering.md)
section 0 defines as a production-like server; a laptop does not close it.
Rerun the command below on that server and add its output here.

## Workload

[crates/sim/tests/block_cache.rs](../../crates/sim/tests/block_cache.rs),
`hit_cost`: a cache of 1,024 blocks (128 MiB of block data) filled from
4,096 addresses, then `Cache::get` called 100,000,000 times on the cached
references, 1,000,000 of them drawn at random before timing starts and
cycled 100 times. Each hit reads one byte of the returned block, so the
time includes bringing a line of a random block into the CPU cache. Single
thread.

## Development machine

- Commit: the one that adds this report, on top of 310c771fb4498f4b920f002fc4167e327d43abb8
- Apple M4 Pro, 14 cores, 24 GiB, macOS 27.0.1
- rustc 1.96.1 (31fca3adb 2026-06-26), release profile of the workspace
  (assertions and overflow checks on)

Command, run three times:

```bash
cargo test --release --locked -p granite-sim --test block_cache -- --ignored --nocapture
```

Output:

```
100000000 hits over 1024 cached blocks in 1.114011833s: 11.14 ns per hit
100000000 hits over 1024 cached blocks in 1.140604833s: 11.41 ns per hit
100000000 hits over 1024 cached blocks in 1.123155625s: 11.23 ns per hit
```
