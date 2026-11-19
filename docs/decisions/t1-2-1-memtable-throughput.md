# T1.2.1: memtable insert and lookup throughput, 1 KiB rows

Status: development-machine numbers only. The gate asks for real hardware, which
[06-testing-and-engineering.md](../06-testing-and-engineering.md) section 0 defines
as a production-like server; a laptop does not close it. Rerun the command below
on that server and add its output here.

## Workload

[crates/lsm/tests/throughput.rs](../../crates/lsm/tests/throughput.rs):

- one `Memtable` with an arena of 1,100 MiB, allocated before timing starts;
- 1,000,000 rows of 16-byte key and 1,008-byte value (1 KiB per row), keys in
  pseudo-random order, no duplicates;
- inserts, timed; then a lookup of every key in a different pseudo-random order,
  timed, all found.

Single thread. The insert time includes the first touch of each arena page, as
the arena is zeroed lazily by the operating system.

## Development machine

- Commit: the one that adds this report, on top of e76c2697e85d39b1b4a210618453b0fbe12bf0dd
- Apple M4 Pro, 14 cores, 24 GiB, macOS 27.0.1
- rustc 1.96.1 (31fca3adb 2026-06-26), release profile of the workspace
  (assertions and overflow checks on)

Command, run three times:

```bash
cargo test --release --locked -p granite-lsm --test throughput -- --ignored --nocapture
```

Output:

```
rows: 1000000, arena used: 1042333065 bytes
insert: 1.504332125s, 664747 rows/s, 1504 ns/row
lookup: 1.715463583s, 582933 rows/s, 1715 ns/row

rows: 1000000, arena used: 1042333065 bytes
insert: 1.568714834s, 637464 rows/s, 1569 ns/row
lookup: 1.899058916s, 526577 rows/s, 1899 ns/row

rows: 1000000, arena used: 1042333065 bytes
insert: 1.43775325s, 695530 rows/s, 1438 ns/row
lookup: 1.7243975s, 579913 rows/s, 1724 ns/row
```

About 0.65 million inserts and 0.55 million lookups per second. Both are bound
by cache misses: with 1 GiB of rows, each step of the skip-list search touches
a node in a different place in the arena. Node overhead is 42 bytes per row on
average (1,042,333,065 bytes for 1,024,000,000 bytes of rows).

## Real hardware

Open.
