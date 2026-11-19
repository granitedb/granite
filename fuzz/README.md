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

## Gate runs

| Target | Gate | Date | Commit of the target | Command | Result |
|---|---|---|---|---|---|
| `catalog_decode` | 4 CPU-hours (T1.4.3) | 2026-11-19 | 577084a | `cargo +nightly fuzz run catalog_decode -- -fork=4 -max_total_time=3600`, then `cargo +nightly fuzz cmin catalog_decode` | macOS arm64, 14 cores, rustc 1.102.0-nightly (65ce8611f 2026-11-18), cargo-fuzz 0.13.2, libfuzzer-sys 0.4.13. 3,622 s x 4 processes, 1,390,134,077 executions, oom/timeout/crash 0/0/0, 1,062 coverage edges; corpus minimised to 801 inputs. |
