# 06. Testing and engineering

## 0. Constraints (confirmed 2026-11-18)

| Constraint | Value | Consequence |
|---|---|---|
| Licence | Open source from the first commit | Public repository, licence file, contribution rules, dependency licence checks, public design documents |
| Production CPU | x86-64 only | Byte-identical storage must hold across x86-64 machines only. Baseline is x86-64-v3 (AVX2); wider SIMD paths must produce identical bytes |
| Development hardware | Local machines for now | Correctness work runs in the simulator anywhere. Linux-only code and all absolute performance numbers need other hardware; see below |
| Team | Deep experience in consensus and storage | No ramp-up time added; the external audit before release stays |

### What local development can and cannot establish

Can, on any laptop including macOS on ARM:
- everything the simulator checks: safety, convergence, serializability, liveness in virtual time,
- model checking, fuzzing, property tests,
- relative comparisons (layout A against layout B on the same machine).

Cannot:
- anything about the io_uring backend or direct I/O behaviour, which are Linux-only. These run in CI on Linux and in a local Linux virtual machine. They run directly on the hosted runner, never in a container; the `io-uring` CI job checks that the runner allows io_uring, and [decisions/0001](decisions/0001-io-uring-on-hosted-runners.md) records the result and the alternative if it does not.
- absolute throughput and latency, write amplification on real drives, the 200 ms failover bound, the 1 ms slice rule under real load, power-fail behaviour.

Since development machines may be ARM while production is x86-64, every SIMD kernel has a scalar reference implementation that runs everywhere, and CI on x86-64 checks that the two produce identical bytes.

Hardware needed, and by when:

| By | Needed | For |
|---|---|---|
| Phase 1 exit (gate G1) | 1 production-like server with enterprise NVMe | Layout experiment, first real storage numbers |
| Phase 2 exit | 3 such servers | Replication on real disks and network, crash and power-fail tests, failover timing |
| Phase 3 exit (gate G2) | The same 3 | Ledger benchmark against TigerBeetle |
| Phase 5 (gate G3) | 6 servers | Cross-shard settlement benchmark at production shape |
| Continuous | CI minutes for nightly simulator seeds, later a small always-on machine | The simulator fleet; hosted CI for a public repository covers the early phases |

Until the first server exists, gates G1 to G3 cannot be passed, only prepared. This is the main schedule risk outside the code.

### Open-source hygiene

- Licence: Apache-2.0 (the usual choice for Rust infrastructure, with a patent grant), in `LICENSE`.
- `LICENSE`, `CONTRIBUTING`, `SECURITY` (private vulnerability reporting), code of conduct.
- Contribution sign-off: Developer Certificate of Origin rather than a contributor licence agreement. The `Sign-off` workflow fails a pull request with a commit that is not signed off by its author.
- `cargo-deny` rejects dependencies with incompatible licences, known advisories, banned crates and duplicate major versions (`deny.toml`, run by `just deny` as part of `just check`).
- Design documents live in the repository and are public; decisions are recorded there, not in private chat.

### Assertion policy and its cost

Assertions stay enabled in release builds. One consequence must be accepted knowingly: a bug in the deterministic state machine fails the same assertion on every replica of a group at the same op, so the whole group stops rather than diverging or corrupting data. Recovery is a fixed binary that can execute that op. This trades availability for safety, as TigerBeetle does. The simulator and fuzzers exist to make it rare; the upgrade path (document 02 section 10) is what makes it recoverable.

## 1. Deterministic simulation

The simulator is the primary correctness tool and is built before the things it tests. It runs a whole cluster (many nodes, many shards, clients, the metadata group) in one thread from one seed, with every source of nondeterminism replaced.

### The `Io` boundary

All production code reaches the outside world only through these traits (`crates/io/src/traits.rs`):

```rust
trait Storage  { size(zone), read(tag, zone, offset, len, buf), write(tag, zone, offset, len, buf), fsync(tag) }
trait Network  { connect(tag, peer), accept(tag), send(tag, conn, len, buf), recv(tag, conn, buf), close(conn) }
trait Time     { monotonic(), realtime() }                 // nanoseconds
trait Random   { next_u64() }                              // seeded, used only outside the state machine
trait Io: Storage + Network + Time + Random { poll() }     // one completion, or none
```

The model is completion-based. A storage or network call only submits; its result comes back from a later `poll` as a completion carrying the caller's tag, never inside the submitting call. A full backend refuses a submission at once and returns its buffer, which is the backpressure. Buffers are `Message` handles from the shard's pool, moved into the backend for the operation and back with its completion; a read or receive needs the only handle, a write or send may share it. Errors, including a reset or timed-out connection, arrive in completions. Periodic work and timeouts count ticks that the event loop delivers, so `Time` has no timer. The rules, and why the traits differ from a first sketch, are in [decisions/0003](decisions/0003-io-completion-model.md); `granite_io::memory` is a trivial in-memory implementation and `granite_io::conformance` the suite every backend passes.

Backends:
- **io_uring** (production, Linux): registered buffers and files, direct I/O, one ring per shard.
- **portable** (development, macOS only, `granite_io::portable`): `pread`, `pwrite` and non-blocking sockets driven by `poll`, with connects on a helper thread; correctness only, never benchmarked or deployed. Linux builds do not contain it, so no production build can select it, and `granite start` warns that it is unsupported in production. Tests that run on one system only are listed, with reasons, in `crates/io/tests/skipped.md`.
- **simulated**: in-memory disk and network with a virtual clock.

How durable a completed operation is differs between the two real backends:

| | io_uring (Linux) | portable (macOS) |
|---|---|---|
| Data file opened with | `O_DIRECT \| O_DSYNC` | `F_NOCACHE` |
| A completed write | is on stable storage: `O_DSYNC` makes each write durable, flushing the drive's cache or writing through it | is not durable: it may be in the drive's volatile cache, or in the page cache, as `F_NOCACHE` is only a hint the kernel may ignore |
| A completed fsync | makes every write completed before it durable (`fsync`) | the same, by `fcntl(F_FULLFSYNC)`, which asks the drive to flush its cache; plain `fsync` on macOS stops short of the drive's cache. Some external and network drives ignore the request |
| A completed write after `SIGKILL` of the process | survives, and a test shows it (`completed_writes_survive_sigkill`) | survives, as the kernel already holds it; not tested |
| Power loss | survives once its write or fsync completed; not tested yet (section 0) | survives only once a later fsync completed, and only if the drive honours `F_FULLFSYNC` |

Code that is correct against the trait contract (a write is durable after a later fsync) is correct on both; the portable backend only removes the margin the io_uring backend gives a write between its completion and the next fsync.

The state machine itself takes no `Io`: it is a pure function of (state, op), which is what makes replicas converge.

### Fault injection

| Domain | Faults |
|---|---|
| Storage | torn writes on crash, bit flips, misdirected reads and writes, lost writes, latency, read errors; bounded so that the fault model's assumptions hold (for example, never corrupt the same block on a quorum) |
| Network | drop, delay, reorder, duplicate, partition (symmetric and asymmetric), connection reset |
| Process | crash and restart at any I/O boundary, pause, slow shard |
| Clock | drift, jumps, per-node offset |
| Cluster | add and remove nodes, tablet moves, splits, reconfiguration, upgrades, all during load |

The storage bound (`granite_sim::storage`) counts a unit of redundancy as corrupt on a replica when a bit flip, a lost write or a misdirected write left content there that its process did not write. A unit is a grid block, a WAL slot (its header and its prepare together) or the superblock (all its copies, since a replica needs a quorum of its own copies to open), at the same offsets on every replica. No fault may make a unit corrupt on a replication quorum of its group. Torn writes are not counted, since a write in flight was never acknowledged and recovery truncates it, and neither are read errors and misdirected reads, which change no stored content.

The simulated network (`granite_sim::network`) delays each message by a latency model (one of them matching the single data center of document 02: 25 to 250 µs one way) and draws at most one message fault per message: reset, drop, delay, duplicate or reorder. Partitions, symmetric or asymmetric, start and heal on the driver's ticks. With faults enabled a connection no longer carries each message once and in order, so protocols do not rely on it. A simulated node (`granite_sim::Node`) joins a simulated disk and network into an `Io`, and passes the conformance suite with faults disabled.

The process faults live in the simulated node (`granite_sim::node`). A crash is drawn from the seed at every I/O boundary: after a submission is taken, or after a completion is taken from the device and before the process sees it. It tears the disk's writes in flight, resets the node's connections, drops the messages it sent that have not arrived and returns those sent to it to their senders, failed. The node then swallows every call until the driver drops the process, with its memory, and starts a new one from what the disk holds. A pause holds the next completion and resumes the process later with its memory intact; a slow shard takes one completion per configured interval. Each boundary is counted by call site, so a run shows where crashes landed. A toy replicated counter (`granite_sim::counter`) proves this machinery under every storage, network and process fault.

### Checkers

- **Storage convergence:** at every checkpoint, all replicas of a group that reached it have byte-identical grids and superblock roots.
- **Linearizability per group:** client histories checked against a reference model of the state machine.
- **Serializability across groups:** all committed transactions replayed serially in timestamp order against the reference model; every result must match.
- **Snapshot stability:** a snapshot read repeated later returns the same result.
- **Liveness:** after faults stop and a quorum is healthy, the cluster makes progress within a bound.
- **Resource bounds:** no pool is exhausted in a way that is not turned into backpressure.

The reference model is a small, obviously correct in-memory implementation of the data model and transaction IR, maintained alongside the real one. It lives in `granite_sim::model`, with a seeded generator of valid and invalid operations for any catalog and a harness that applies the same operations to the model and to an engine and compares every result and, on demand, the full table content.

### Operation

- Every failing seed reproduces exactly; the seed and git commit are the bug report.
- The seed runner (`granite_sim::runner` and the `sim` binary of `crates/sim`) runs one seed, a range, a seed list or random seeds for a duration of one scenario, in parallel across cores, each seed on one thread. A thread reuses one message pool for all its seeds, so a seed returns every buffer and a scenario reads only bytes it wrote in that seed; the reproduction command runs the seed alone, from a fresh pool. For a failed seed it prints the commit, the failing checker and its message, the seed's swarm configuration, the last events of the run and one command that reruns that seed; adding `--trace` to the command prints every event of it. A panic or failed assertion is a failure (the runner builds in the `sim` profile, which unwinds), and so is a hang: a run that logs a million events at one virtual time. It exits non-zero if any seed failed. `just sim` runs the fixed list `crates/sim/ci-seeds.txt` on every pull request, `just sim-range FIRST COUNT` a range (the nightly workflow runs a new one each night), `just sim-random SECONDS` random seeds and `just sim-trace SEED` one seed with its trace.
- A scenario is a name and two functions of the seed: its swarm configuration and its run, which reports events to the runner's log and returns the checker that failed. The toy counter is the first; `counter-lost-write` is the same with an injected bug, and the `fixture-*` scenarios fail on purpose to test the runner.
- A fleet runs random seeds continuously against the main branch and reports failures. Budget for dedicated machines.
- Swarm testing: each seed randomises which faults and which workload features are enabled.
- Time compression: the virtual clock skips idle time, so hours of cluster time run in seconds.

## 2. Other testing

- **Model checking:** specifications of VSR view change with storage faults, reconfiguration, tablet move, and the cross-shard protocol, in TLA+ or with `stateright`. Written before the corresponding code, kept in the repository.
- **Fuzzing:** every decoder (message header, row batch, program IR, plan IR, part blocks, superblock) under coverage-guided fuzzing; codec round-trip fuzzing; LSM against a model (`BTreeMap`).
- **Property tests:** type encodings are order-preserving; codecs round-trip; compaction preserves content.
- **Memory safety:** Miri on the crates that contain `unsafe` (pools, zero-copy views, SIMD reference paths); address and thread sanitizers on the scan-worker boundary.
- **Client conformance suite:** one set of scenarios that the Rust, Go and Python clients must all pass, including retries, failover and stale maps.
- **Gateway:** SQL results compared against the reference model over a query corpus; fault tests for mid-query view changes.
- **Real-hardware tests:** crash consistency with power-fail injection (dm-flakey or similar), long soak tests, Jepsen-style tests on real machines before any release.
- **Assertions:** preconditions, postconditions and invariants are asserted in production code and left enabled in release builds. A failed assertion aborts the process, so it stops every shard on that node; replication absorbs it, as it absorbs the loss of a node.

## 3. Determinism rules for Rust code

Enforced by clippy configuration, custom lints or review checklists:
- No `std::collections::HashMap` or `HashSet` in the state machine or storage; use ordered maps or hash maps with a fixed hasher and no iteration.
- No `std::time`, `std::thread`, `std::fs`, `std::net`, `rand` outside the `io` crate.
- No `usize` or `isize` in any persisted or transmitted format.
- No floating-point reductions in unspecified order; no `mul_add` or fast-math; canonical NaN.
- No dependence on allocation addresses, pointer ordering or uninitialised padding. All padding bytes in persisted structs are zeroed and checked.
- Third-party codecs are vendored and pinned; a codec version bump is a format change.
- `unsafe` only in `io`, pools and SIMD kernels, each block with a written safety argument.

## 4. Runtime

- A hand-written, single-threaded, completion-based event loop per shard on the `io-uring` crate. No general-purpose async runtime in the server. Protocol code is written as explicit state machines with callbacks (TigerBeetle's style), in `vsr`, `storage` and the core of the client library; the Rust client may offer an `async` interface on top of its state machine. A spike compared this with `async` on a minimal custom executor: the same traces, no allocation per operation against at least two, and in-flight state in named fields rather than in generated futures ([decisions/0003](decisions/0003-io-completion-model.md)).
- Shard threads are pinned; memory is allocated from the local NUMA node; interrupts steered away where possible.
- Scheduling classes with budgets (document 05).

## 5. Repository layout and crates

Everything lives in one repository and is versioned and tested together: the database, the client libraries, the gateway, the simulator, the protocol models, the benchmarks and these documents.

```
Cargo.toml            workspace root: shared lints, versions, internal dependencies
Justfile              task recipes; CI runs `just check`
rust-toolchain.toml   pinned compiler
docs/                 design documents and roadmap
crates/               the database, one folder per crate
clients/              rust, c (C ABI), go, python
gateway/              read-only SQL gateway, built outside the workspace
schemas/              reference schemas
fuzz/                 fuzz targets
models/               protocol specifications
bench/                workload definitions and stored results
tools/                repository scripts
```

Package names carry a `granite-` prefix (`crates/vsr` is the package `granite-vsr`). Dependencies in the table are the allowed direction; the workspace manifest is the place where they are declared. A crate may also depend directly on anything below its listed dependencies; any other dependency between the packages in this table, the gateway included, is rejected by `just check` (`tools/crate_graph.py`, which mirrors this table).

| Path | Responsibility | Depends on |
|---|---|---|
| `crates/base` | Checksums, fixed-layout helpers, the message header, ids, error codes, constants | |
| `crates/io` | Traits and the three backends | base |
| `crates/storage` | Superblock, WAL, grid, free set, block cache | base, io |
| `crates/lsm` | Memtable, manifest, levels, compaction, iterators | storage |
| `crates/columnar` | Part format, codecs, filters, skip indexes | lsm |
| `crates/types` | Type system, encodings, expressions | base |
| `crates/schema` | Catalog objects and validation | types |
| `crates/schemac` | Schema and program text syntax: compilers to the binary catalog and the program IR, printers back to text, program reports. Client tooling, never linked into the server | schema, txn |
| `crates/txn` | Program IR, validator, executor | schema, lsm |
| `crates/shard` | State machine: operations, sessions, locks, pending transactions | txn, columnar |
| `crates/vsr` | Replica, client-session protocol, repair, sync, clock | storage, io |
| `crates/meta` | Metadata state machine, balancer | vsr |
| `crates/xshard` | Cross-shard protocol | vsr, shard |
| `crates/query` | Scan fragment IR and vectorised operators | columnar, schema |
| `crates/protocol` | Message definitions, validation | base, types |
| `crates/sim` | Simulator, fault injection, checkers, reference model, and its own runner binary. Never linked into the server | all of the above |
| `crates/granite` | The single binary: `format`, `start`, `repl`, `bench`, `version`. Process bootstrap, configuration, shard supervision, benchmark harness | everything except `sim` |
| `clients/rust` | Client library and codegen | protocol |
| `clients/c` | C ABI over the Rust client, the base of the Go and Python clients | client |
| `gateway` | Read-only SQL service on an external query engine; separate binary, not a workspace member, not linked into the server | client |

The server, its tools and the benchmark harness ship as one binary so that a tool always matches the server version. The gateway is kept out of the workspace so that its large dependency tree never enters the database build.

External dependencies are kept minimal and audited: `io-uring`, `zerocopy` or `bytemuck`, `rustls`, vendored `lz4`/`zstd`, a checksum crate. Adding a dependency to the workspace needs a stated reason in the change that adds it. The approval rule and the audit step are in `CONTRIBUTING.md`.

## 6. Benchmarks

Tracked from phase 1, run on fixed hardware, results stored with the commit:
- single-shard insert throughput and latency percentiles, by batch size,
- point-read latency by level depth and column count,
- write amplification and space amplification,
- the two-row transfer workload (ledger benchmark),
- recovery time, state sync time, view change time,
- cross-shard transaction throughput at 1%, 10%, 50% cross-shard mix,
- scan throughput and compression ratio per codec,
- OLTP latency while analytical queries run.

Performance regressions over a threshold fail the nightly run.

## 7. Team shape (3 to 4 engineers)

Proposed ownership, with everyone contributing to the simulator:

| Track | Owner focus |
|---|---|
| A. Storage | `storage`, `lsm`, `columnar`, benchmarks |
| B. Consensus and cluster | `vsr`, `meta`, `xshard`, model checking |
| C. Data model and client | `types`, `schema`, `txn`, `shard`, `protocol`, `client`, later `query` |
| D. Simulation and infrastructure (the fourth engineer, or shared) | `io`, `sim`, fuzzing, CI, soak fleet |

With three engineers, track D is split between A and B and the schedule stretches accordingly.

## 8. Project hygiene

- Monorepo, one Cargo workspace, pinned toolchain, `deny.toml` for dependencies.
- CI on every change: build, unit and property tests, a fixed set of simulator seeds, lints, format. Nightly: long simulator runs, fuzzing, benchmarks.
  - `.github/workflows/ci.yml` runs on every pull request and push to main, on Linux x86-64 and macOS: each recipe of `just check` as its own step, then `just sim`, which runs the fixed seed list `crates/sim/ci-seeds.txt`. A seed that once failed is appended to that list. The `io-uring` job checks that the Linux runner allows io_uring (see section 0).
  - `just sim` and `just sim-range` arrive with the seed runner (T0.4.5). Until then the seed steps of both workflows only print a warning and run no seeds.
  - `.github/workflows/nightly.yml` runs `just sim-range FIRST COUNT` on 10,000 seeds, starting at the run number times 10,000, so each night covers new seeds. The log prints the range and the commit.
  - `.github/workflows/sign-off.yml` checks on every pull request that each commit has a `Signed-off-by` line matching its author.
  - The jobs of `ci.yml` and `sign-off.yml` are required checks for merging to main. Third-party actions are pinned to a commit hash.
- Design changes go through short written proposals in `docs/`, updated in the same change as the code.
- On-disk and wire formats are versioned from the first commit, even before any compatibility promise.
