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

- Licence: Apache-2.0 is the candidate (the usual choice for Rust infrastructure, with a patent grant). To be confirmed before the repository is created.
- `LICENSE`, `CONTRIBUTING`, `SECURITY` (private vulnerability reporting), code of conduct.
- Contribution sign-off: Developer Certificate of Origin rather than a contributor licence agreement, unless you want to keep the option of relicensing.
- `cargo-deny` rejects dependencies with incompatible licences.
- Design documents live in the repository and are public; decisions are recorded there, not in private chat.

### Assertion policy and its cost

Assertions stay enabled in release builds. One consequence must be accepted knowingly: a bug in the deterministic state machine fails the same assertion on every replica of a group at the same op, so the whole group stops rather than diverging or corrupting data. Recovery is a fixed binary that can execute that op. This trades availability for safety, as TigerBeetle does. The simulator and fuzzers exist to make it rare; the upgrade path (document 02 section 10) is what makes it recoverable.

## 1. Deterministic simulation

The simulator is the primary correctness tool and is built before the things it tests. It runs a whole cluster (many nodes, many shards, clients, the metadata group) in one thread from one seed, with every source of nondeterminism replaced.

### The `Io` boundary

All production code reaches the outside world only through these traits:

```rust
trait Storage  { read(zone, offset, buf), write(zone, offset, buf), fsync() }   // completion-based
trait Network  { connect, accept, send(msg), recv() }
trait Time     { monotonic(), realtime(), tick() }
trait Random   { seeded PRNG, used only outside the state machine }
```

Backends:
- **io_uring** (production, Linux): registered buffers and files, direct I/O, one ring per shard.
- **portable** (development, macOS): blocking or kqueue-based; correctness only, not performance.
- **simulated**: in-memory disk and network with a virtual clock.

The state machine itself takes no `Io`: it is a pure function of (state, op), which is what makes replicas converge.

### Fault injection

| Domain | Faults |
|---|---|
| Storage | torn writes on crash, bit flips, misdirected reads and writes, lost writes, latency, read errors; bounded so that the fault model's assumptions hold (for example, never corrupt the same block on a quorum) |
| Network | drop, delay, reorder, duplicate, partition (symmetric and asymmetric), connection reset |
| Process | crash and restart at any I/O boundary, pause, slow shard |
| Clock | drift, jumps, per-node offset |
| Cluster | add and remove nodes, tablet moves, splits, reconfiguration, upgrades, all during load |

### Checkers

- **Storage convergence:** at every checkpoint, all replicas of a group that reached it have byte-identical grids and superblock roots.
- **Linearizability per group:** client histories checked against a reference model of the state machine.
- **Serializability across groups:** all committed transactions replayed serially in timestamp order against the reference model; every result must match.
- **Snapshot stability:** a snapshot read repeated later returns the same result.
- **Liveness:** after faults stop and a quorum is healthy, the cluster makes progress within a bound.
- **Resource bounds:** no pool is exhausted in a way that is not turned into backpressure.

The reference model is a small, obviously correct in-memory implementation of the data model and transaction IR, maintained alongside the real one.

### Operation

- Every failing seed reproduces exactly; the seed and git commit are the bug report.
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

- A hand-written, single-threaded, completion-based event loop per shard on the `io-uring` crate. No general-purpose async runtime in the server. Whether to express protocol code as explicit state machines with callbacks (TigerBeetle's style, simplest to make deterministic) or as `async` on a minimal custom executor is a phase 0 decision to make with a spike; proposed: explicit state machines for `vsr` and `storage`, `async` allowed in the client library.
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
| `crates/base` | Checksums, fixed-layout helpers, ids, error codes, constants | |
| `crates/io` | Traits and the three backends | base |
| `crates/storage` | Superblock, WAL, grid, free set, block cache | base, io |
| `crates/lsm` | Memtable, manifest, levels, compaction, iterators | storage |
| `crates/columnar` | Part format, codecs, filters, skip indexes | lsm |
| `crates/types` | Type system, encodings, expressions | base |
| `crates/schema` | Catalog objects and validation | types |
| `crates/txn` | Program IR, validator, executor | schema, lsm |
| `crates/shard` | State machine: operations, sessions, locks, pending transactions | txn, columnar |
| `crates/vsr` | Replica, client-session protocol, repair, sync, clock | storage, io |
| `crates/meta` | Metadata state machine, balancer | vsr |
| `crates/xshard` | Cross-shard protocol | vsr, shard |
| `crates/query` | Scan fragment IR and vectorised operators | columnar, types |
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
  - `.github/workflows/nightly.yml` runs `just sim-range FIRST COUNT` on 10,000 seeds, starting at the run number times 10,000, so each night covers new seeds. The log prints the range and the commit.
  - The jobs of `ci.yml` are required checks for merging to main. Third-party actions are pinned to a commit hash.
- Design changes go through short written proposals in `docs/`, updated in the same change as the code.
- On-disk and wire formats are versioned from the first commit, even before any compatibility promise.
