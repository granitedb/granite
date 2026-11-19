# Fuzz targets

Coverage-guided fuzz targets for every decoder and codec. See `docs/06-testing-and-engineering.md`.

This directory is its own Cargo package, outside the root workspace, because fuzzing needs a nightly toolchain and cargo-fuzz:

```bash
rustup toolchain install nightly
cargo install cargo-fuzz --locked
cd fuzz
cargo +nightly fuzz run <target> -- -max_total_time=<seconds>
```

Each target's corpus is in `corpus/<target>/`, committed after every gate run; cargo-fuzz reads and extends it. A fuzz gate of H CPU-hours (`docs/tasks/README.md`) is H hours of one process, or less with `-fork=<n>` across n cores.

| Target | What it checks |
|---|---|
| `catalog_decode` | `granite_schema::Catalog::decode` on arbitrary bytes never panics, and what it accepts encodes back to the same bytes. The corpus grew from the golden catalog of `crates/schema/src/tests.rs`. |
| `schema_compile` | `granite_schemac::compile` on arbitrary bytes never panics or hangs, and what it accepts prints as text that compiles to the same bytes. Run with `-max_len=1048576` (one message, the input bound of T1.4.5's gate). The corpus grew from `schemas/exchange.schema` and a schema using every form of the grammar. |
| `program_decode` | `granite_txn::Program::decode` on arbitrary bytes never panics, and what it accepts encodes back to the same bytes. Run with `-max_len=1048576` (one message) and `-malloc_limit_mb=2`, so any allocation of 2 MiB or more fails the run (the bound of document 03 section 5.1); recursion is bounded by the depth limit, and a deep input that overflowed the stack would crash. The corpus grew from the golden program of `crates/txn/tests/ir.rs`, one input per opcode and literals of several types. |

## Gate runs

| Target | Gate | Date | Commit of the target | Command | Result |
|---|---|---|---|---|---|
| `catalog_decode` | 4 CPU-hours (T1.4.3) | 2026-11-19 | 577084a | `cargo +nightly fuzz run catalog_decode -- -fork=4 -max_total_time=3600`, then `cargo +nightly fuzz cmin catalog_decode` | macOS arm64, 14 cores, rustc 1.102.0-nightly (65ce8611f 2026-11-18), cargo-fuzz 0.13.2, libfuzzer-sys 0.4.13. 3,622 s x 4 processes, 1,390,134,077 executions, oom/timeout/crash 0/0/0, 1,062 coverage edges; corpus minimised to 801 inputs. |
| `schema_compile` | 4 CPU-hours (T1.4.5) | 2026-11-19 | 633ca07 | `/usr/bin/time -p cargo +nightly fuzz run schema_compile -- -fork=4 -max_total_time=3600 -max_len=1048576 -timeout=10` | macOS arm64, 14 cores shared with other builds (load average 78 to 133), rustc 1.102.0-nightly (65ce8611f 2026-11-18), cargo-fuzz 0.13.2, libfuzzer-sys 0.4.13. Run 1 of 8: 3,637.61 s wall, 8,462.90 s user+sys, 175,465,985 executions, 2,458 coverage edges, oom/timeout/crash 0/0/0. The host was loaded, so the CPU time was measured and runs 2 to 8 continued from the same corpus until user+sys reached 14,400 s. |
| `schema_compile` | 4 CPU-hours (T1.4.5) | 2026-11-19 | 633ca07 | `/usr/bin/time -p cargo +nightly fuzz run schema_compile -- -fork=4 -max_total_time=1485 -max_len=1048576 -timeout=10` | Run 2 of 8: 1,494.48 s wall, 4,163.85 s user+sys, 66,878,390 executions, 2,461 coverage edges, oom/timeout/crash 0/0/0. |
| `schema_compile` | 4 CPU-hours (T1.4.5) | 2026-11-19 | 633ca07 | `/usr/bin/time -p cargo +nightly fuzz run schema_compile -- -fork=4 -max_total_time=444 -max_len=1048576 -timeout=10` | Run 3 of 8: 456.89 s wall, 923.66 s user+sys, 14,074,287 executions, 2,461 coverage edges, oom/timeout/crash 0/0/0. |
| `schema_compile` | 4 CPU-hours (T1.4.5) | 2026-11-19 | 633ca07 | `/usr/bin/time -p cargo +nightly fuzz run schema_compile -- -fork=4 -max_total_time=213 -max_len=1048576 -timeout=10` | Run 4 of 8: 224.94 s wall, 595.27 s user+sys, 9,378,399 executions, 2,462 coverage edges, oom/timeout/crash 0/0/0. |
| `schema_compile` | 4 CPU-hours (T1.4.5) | 2026-11-19 | 633ca07 | `/usr/bin/time -p cargo +nightly fuzz run schema_compile -- -fork=4 -max_total_time=64 -max_len=1048576 -timeout=10` | Run 5 of 8: 68.07 s wall, 178.22 s user+sys, 2,493,135 executions, 2,462 coverage edges, oom/timeout/crash 0/0/0. |
| `schema_compile` | 4 CPU-hours (T1.4.5) | 2026-11-19 | 633ca07 | `/usr/bin/time -p cargo +nightly fuzz run schema_compile -- -fork=4 -max_total_time=20 -max_len=1048576 -timeout=10` | Run 6 of 8: 22.38 s wall, 60.03 s user+sys, 672,091 executions, 2,462 coverage edges, oom/timeout/crash 0/0/0. |
| `schema_compile` | 4 CPU-hours (T1.4.5) | 2026-11-19 | 633ca07 | `/usr/bin/time -p cargo +nightly fuzz run schema_compile -- -fork=4 -max_total_time=5 -max_len=1048576 -timeout=10` | Run 7 of 8: 6.48 s wall, 13.89 s user+sys, 107,687 executions, 2,462 coverage edges, oom/timeout/crash 0/0/0. |
| `schema_compile` | 4 CPU-hours (T1.4.5) | 2026-11-19 | 633ca07 | `/usr/bin/time -p cargo +nightly fuzz run schema_compile -- -fork=4 -max_total_time=5 -max_len=1048576 -timeout=10` | Run 8 of 8: 6.23 s wall, 13.87 s user+sys, 80,168 executions, 2,462 coverage edges, oom/timeout/crash 0/0/0. Total: 5,917.08 s wall, 14,411.69 s user+sys (4.0 CPU-hours), 269,150,142 executions, no crash, oom or timeout. Then `cargo +nightly fuzz cmin schema_compile -- -max_len=1048576`: corpus minimised to 1,182 inputs, 2,462 edges. |
