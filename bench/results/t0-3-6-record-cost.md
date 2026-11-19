# T0.3.6: cost of recording one stall-metric sample

Status: development-machine numbers only. The gate asks for under 50 ns on
real hardware, which [06-testing-and-engineering.md](../../docs/06-testing-and-engineering.md)
section 0 defines as a production-like server; a laptop does not close it.
Rerun the command below on that server and add its output here.

## Workload

[crates/io/tests/record_cost.rs](../../crates/io/tests/record_cost.rs):
`Histogram::record` called 100,000,000 times on one histogram, with 1,000,000
durations from 1 µs to 65 ms drawn before timing starts and cycled 100 times.
Single thread.

A sample in the event loop is this call. The clock read before it is not
counted: the loop already reads the clock after every task to charge it to
its class. The iteration-time sample adds one clock read per iteration, at
its start.

## Development machine

- Commit: the one that adds this report, on top of 3e58288eb545e6a32d4a55873fcd0e79bb24b6db
- Apple M4 Pro, 14 cores, 24 GiB, macOS 27.0.1
- rustc 1.96.1 (31fca3adb 2026-06-26), release profile of the workspace
  (assertions and overflow checks on)

Command, run three times:

```bash
cargo test --release --locked -p granite-io --test record_cost -- --ignored --nocapture
```

Output:

```
100000000 samples in 246.030041ms: 2.46 ns per sample
100000000 samples in 229.648ms: 2.30 ns per sample
100000000 samples in 231.35125ms: 2.31 ns per sample
```

About 2.3 ns per sample, a twentieth of the 50 ns limit.

## Real hardware

Open.
