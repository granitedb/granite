# Phase 0. Foundations

Phase exit: an empty cluster of simulated shards exchanges checksummed messages under fault injection, reproducibly from a seed, on Linux and macOS.

## T0.1.1 Create workspace, crate skeletons, pinned toolchain, dependency policy

**Goal.** One repository that builds and tests with a single command, with the crate boundaries of the design already enforced by the build, so every later task has a place to put its code.

**Work.**
- A Cargo workspace with the crates and dependency directions listed in `docs/06-testing-and-engineering.md` section 5.
- One `granite` binary with subcommands; the gateway as a package outside the workspace.
- A `Justfile` whose `check` recipe is what CI runs.
- A pinned compiler in `rust-toolchain.toml` and a minimum version in the workspace manifest.
- A written dependency policy: what may be added, who approves it, how it is audited.

**Not in this card.** Licence checking (T0.1.5), lint rules beyond the defaults (T0.1.2), the full CI pipeline (T0.1.3).

**Depends on.** Nothing.

**Gates.**
- [ ] `just check` exits 0 on a clean clone on Linux x86-64 and on macOS.
- [ ] A CI run on both systems passes on the main branch, and its link is recorded in this issue.
- [ ] A crate that imports another against the allowed direction fails to build. Shown by one deliberate wrong import that is rejected (injected bug).
- [ ] The `granite` binary does not depend on `granite-sim`. `cargo tree -p granite` shows no path to it.
- [ ] `cargo tree -p granite` lists zero third-party crates.
- [ ] The dependency policy is a section of `CONTRIBUTING.md` and names the approval rule and the audit step.
- [ ] Building with a compiler older than the declared minimum fails with a version error, not a compile error.

## T0.1.5 Open-source setup: licence, CONTRIBUTING, SECURITY, code of conduct, sign-off rule, licence checks

**Goal.** The project can accept outside contributions and security reports from the first public commit, and cannot accidentally take a dependency with an incompatible licence.

**Work.**
- `LICENSE` (Apache-2.0), `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`.
- A `deny.toml` for `cargo-deny`: allowed licences, banned crates, advisory database check, no duplicate major versions without a listed exception.
- A `just deny` recipe, run by `just check`.
- Sign-off enforced on pull requests by an automated check.
- Private vulnerability reporting enabled on the repository, and a contact on the organisation profile.

**Not in this card.** Release signing and supply-chain attestation (phase 7).

**Depends on.** T0.1.1.

**Gates.**
- [ ] `just deny` exits 0 on the main branch and is part of `just check`.
- [ ] Adding a dependency with a GPL licence makes `just deny` fail (injected bug).
- [ ] Adding a crate with a known advisory makes `just deny` fail (injected bug).
- [ ] A pull request containing a commit without a `Signed-off-by` line is blocked by a required check. Shown on a test pull request.
- [ ] The repository's "Report a vulnerability" button exists and delivers a test report to the maintainers.
- [ ] Every private-contact route named in `SECURITY.md` and `CODE_OF_CONDUCT.md` resolves to a working address.

## T0.1.6 Local Linux virtual machine recipe for io_uring development

**Goal.** A developer on macOS can run and debug the Linux-only code (io_uring, direct I/O) locally, because those paths cannot run on the host.

**Work.**
- A recipe (`just vm-up`, `just vm-check`, `just vm-shell`, `just vm-down`) that creates a Linux virtual machine with a pinned kernel and the pinned toolchain, mounts the repository, and runs commands inside it.
- A second virtual disk formatted for direct I/O tests, since shared host folders usually do not support `O_DIRECT`.
- A short document: requirements, first-time setup, known limits.

**Not in this card.** Performance testing in the virtual machine. Numbers from it are never evidence. Running the Io conformance suite in `just vm-check` is a gate of T0.3.2, which builds that suite.

**Depends on.** T0.1.1.

**Gates.**
- [ ] On a macOS machine with the documented prerequisites and nothing else, `just vm-up` followed by `just vm-check` exits 0 without manual steps.
- [ ] The kernel version inside the virtual machine is pinned, printed by `just vm-check`, and at least the minimum version the io_uring backend requires.
- [ ] A test that opens a file with `O_DIRECT | O_DSYNC` on the virtual disk and writes one sector passes inside the virtual machine.
- [ ] Destroying and recreating the virtual machine gives the same kernel and toolchain versions.

## T0.1.2 Write coding rules and determinism lints

**Goal.** The determinism rules are enforced by tools, so a violation fails CI instead of depending on a reviewer noticing it.

**Work.**
- Lint configuration (clippy `disallowed-types`, `disallowed-methods`, and a custom check where clippy cannot express the rule) for every rule in `docs/06-testing-and-engineering.md` section 3.
- Per-crate scoping: the bans apply to state-machine and storage crates; `io` is the only crate allowed to use `std::time`, `std::thread`, `std::fs`, `std::net`.
- A rule for `unsafe`: allowed only in listed modules, each block preceded by a `SAFETY:` comment.
- `docs/coding-rules.md`: the rules, the reason for each, and how to request an exception.

**Not in this card.** Rules that need whole-program analysis (floating-point evaluation order); those stay review items and are listed as such.

**Depends on.** T0.1.1.

**Gates.**
- [ ] For each banned API in section 3 (hash maps with random state, `std::time`, `std::thread`, `std::fs`, `std::net`, `rand`, `usize`/`isize` in persisted structs, `mul_add`) there is a fixture file that uses it and a test asserting that `just lint` rejects that fixture.
- [ ] The same APIs used inside `crates/io` are accepted.
- [ ] An `unsafe` block without a `SAFETY:` comment, or outside the allowed modules, fails `just lint` (fixture and test).
- [ ] `just lint` passes on the main branch with zero `allow` attributes that lack a comment giving the reason.
- [ ] Every rule in section 3 is listed in `docs/coding-rules.md` as either "enforced by lint" with the lint name, or "review only" with the reason it cannot be automated.

## T0.1.3 CI pipeline: build, test, lint, format, fixed simulator seeds

**Goal.** Every change is checked the same way before it merges, fast enough that nobody is tempted to skip it.

**Work.**
- A pull-request workflow on Linux x86-64 and macOS that runs `just check`.
- A fixed list of simulator seeds run on every pull request, and a nightly workflow for long runs.
- Build caching.
- A decision, with evidence, on whether hosted runners allow io_uring; if not, a documented alternative for the Linux-only jobs.
- Branch protection: the checks are required for merging to main.

**Not in this card.** The continuous simulator fleet (T2.3.4) and benchmark tracking (T1.5.5). The seed steps, the nightly seed run and the run-time gate are gates of T0.4.5, which adds the seed runner and with it most of the run time.

**Depends on.** T0.1.1.

**Gates.**
- [ ] Merging to main is impossible while a required check is failing or missing. Shown on a test pull request.
- [ ] A pull request that breaks formatting, a lint or a test fails the matching step (three injected bugs, three failing runs).
- [ ] A job that creates an io_uring instance and submits one read passes in CI on Linux, or the alternative is documented and that job passes there.
- [ ] The fixed seed list is a file in the repository.
- [ ] Third-party actions are pinned to a commit hash.

## T0.1.4 Constants and configuration module with compile-time validation

**Goal.** Every limit in the design lives in one place, and a combination of limits that cannot work fails to compile.

**Work.**
- A module in `crates/base` holding every constant from the design documents: sector size, message size, WAL slot count, grid block size, pipeline depth, session count, checkpoint interval, limits table of document 03 section 8, timing budget of document 02 section 0, cross-shard bounds of document 04 section 4.
- Compile-time assertions on the relationships between them.
- A split between format constants (fixed at cluster format, stored in the superblock) and process configuration (set at start).
- A runtime check that compares the binary's format constants with a data file's.

**Not in this card.** Tuning the values; they are initial values to benchmark in phase 1. Checking the header struct's size against the constant is a gate of T0.2.2; refusing to open a data file is a gate of T1.1.2, which stores the format constants in the superblock.

**Depends on.** T0.1.1.

**Gates.**
- [ ] No numeric limit of 256 or more from the design documents appears as a literal anywhere outside this module, and a grep-based test enforces it. Limits below 256 cannot be told apart from ordinary integers by a grep and are a review item.
- [ ] Each of these fails to compile when violated (one test per rule, using a compile-fail test): message size is a multiple of the sector size; grid block size is a multiple of the sector size; the checkpoint interval is smaller than the WAL slot count minus the pipeline depth minus one compaction interval; the row size limit plus header fits in a message; the header size constant is exactly 256 bytes.
- [ ] Every constant has a doc comment naming the document section it comes from.
- [ ] A table generated from the module is compared in CI with the tables in the design documents, and a mismatch fails the check.
- [ ] Comparing a data file's stored format constants with the binary's returns a typed error naming the first constant that differs (test for each constant).

## T0.2.1 Benchmark and choose the 128-bit checksum

**Goal.** A fixed, recorded choice of the checksum function, because it becomes part of the on-disk and wire format and cannot change cheaply later.

**Work.**
- Implement or wrap the two candidates: AEGIS-128L and XXH3-128.
- Benchmark both for inputs of 256 bytes, 4 KiB, 128 KiB and 1 MiB.
- Assess each for detection of the faults in the fault model (bit flips, torn writes, misdirection) and for behaviour on machines without the AES instructions.
- Write the decision.

**Not in this card.** Checksum use in headers and blocks (T0.2.2, T1.1.5).

**Depends on.** T0.1.1.

**Gates.**
- [ ] A report gives throughput in GiB/s for both candidates at all four sizes, on real hardware (x86-64 with AES-NI and AVX2), with the commands to reproduce it.
- [ ] The chosen function has a scalar implementation and an accelerated one that return identical output for 1,000,000 random inputs of random length, checked in CI on x86-64.
- [ ] Fixed test vectors (at least 16, including empty input and one-byte input) are committed and pass on Linux x86-64 and macOS.
- [ ] `docs/decisions/` records the choice, the numbers, the alternatives rejected and why. The decisions table in `DESIGN.md` is updated to "confirmed".
- [ ] The function is exposed from `crates/base` through one API; no other crate calls the underlying implementation directly (lint or test).

## T0.2.2 Message header struct, encode, decode, validation

**Goal.** The 256-byte header that every message, WAL prepare and reply starts with, fixed in layout and safe against any input.

**Work.**
- The header struct with the exact field layout of `docs/05-query-and-protocol.md` section 1, little-endian, no implicit padding.
- Zero-copy view over a byte buffer; no copy on receive.
- Validation: header checksum, body checksum, size bounds, reserved bytes zero, known command and operation codes, version.
- Typed errors for each rejection reason.

**Not in this card.** Message bodies (T1.4.4), transport (T0.3.3).

**Depends on.** T0.2.1, T0.1.4.

**Gates.**
- [ ] A compile-time assertion fixes the size at 256 bytes and the offset of every field; the offsets are listed in the design document and match.
- [ ] Round-trip property test: encode then decode returns the same header for 1,000,000 random valid headers.
- [ ] Fuzz gate 4 on the decoder with arbitrary bytes: no panic, and every rejected input maps to a typed error.
- [ ] Flipping any single bit of a valid header, or of its body, is detected. Tested for every bit position of the header and 10,000 random body positions.
- [ ] A header with a non-zero reserved byte, a size below 256, a size above the message limit, or an unknown command is rejected, each with its own error code (one test each).
- [ ] The struct contains no `usize`, no pointers and no uninitialised padding. Checked by a derive that forbids padding.
- [ ] The size assertion compares with `FORMAT.header_size` from `crates/base/src/constants.rs`, so a header struct of another size fails to compile (compile-fail test) (from T0.1.4).

## T0.2.3 Message pool and buffer management with fixed capacity

**Goal.** All message memory comes from a pool sized at startup, so the commit path never allocates and overload becomes backpressure.

**Work.**
- A pool of sector-aligned buffers of the message size, with capacity fixed at construction.
- Reference-counted handles, so one buffer can be in the WAL write and in a network send at once.
- An acquire call that returns "none available" instead of allocating or blocking.
- Counters: in use, high-water mark, failed acquisitions.

**Not in this card.** Deciding the pool size for a real deployment (T1.5.5 measures it).

**Depends on.** T0.1.4.

**Gates.**
- [ ] After pool construction, a test with a counting global allocator performs 1,000,000 acquire and release cycles with zero heap allocations.
- [ ] Acquiring from an exhausted pool returns the "none available" result; it does not allocate, block or panic (test).
- [ ] Every buffer is aligned to the sector size and its length is a multiple of it (assertion and test).
- [ ] Dropping the last handle returns the buffer to the pool; a model test of random acquire, clone and drop sequences never leaks and never frees twice.
- [ ] The crate passes Miri for its test suite.
- [ ] The three counters are exposed and a test checks their values after a known sequence.

## T0.2.4 Id128 time-ordered id generator

**Goal.** Clients can create unique ids that sort by creation time, so new keys land above older data and uniqueness checks stay cheap.

**Work.**
- A 128-bit id: milliseconds timestamp in the high bits, random bits below, monotonic within a process.
- A generator that never goes backwards when the wall clock does, and that handles more ids per millisecond than the random field can count by borrowing from the timestamp.
- Order-preserving byte encoding consistent with the key encoding (T1.4.1).

**Not in this card.** Server-side id assignment; ids are always client-generated.

**Depends on.** T0.1.1.

**Gates.**
- [ ] 10,000,000 ids generated in one process are strictly increasing.
- [ ] With a test clock that jumps backwards by 1 ms, 1 s and 1 hour, the ids still strictly increase (three tests).
- [ ] Generating more ids in one millisecond than the random field holds does not produce a duplicate or a smaller id (test with a frozen clock and 2^20 ids).
- [ ] Two generators with different random sources produce no collision in 100,000,000 ids in a test, and the document states the collision probability with its derivation.
- [ ] The byte encoding sorts in the same order as the numeric value for 1,000,000 random pairs.
- [ ] The bit layout is written in the data model document and marked as part of the format.

## T0.3.1 Define `Storage`, `Network`, `Time` traits and completion model

**Goal.** One boundary through which all code reaches the disk, network and clock, shaped so the same code runs on io_uring, on macOS and in the simulator.

**Work.**
- The `Storage`, `Network`, `Time` and `Random` traits, completion-based.
- The ownership rule for buffers across an in-flight operation.
- A spike: the same small replicated component written twice, once as an explicit state machine with callbacks and once as `async` on a minimal custom executor, both run in a toy simulator.
- A written decision between the two styles, per crate.

**Not in this card.** The real backends (T0.3.2 to T0.3.4) and the simulator (E0.4).

**Depends on.** T0.2.3.

**Gates.**
- [ ] The trait definitions are merged and documented, with a statement of which calls may complete synchronously and which never do.
- [ ] Both spike versions exist in the repository history and both produce an identical event trace for 1,000 seeds.
- [ ] The decision document compares the two on lines of code, allocations per operation, and whether a mid-operation crash can be injected at every await or callback point; it names the choice for `vsr`, `storage` and `client`.
- [ ] The decision is approved in review by the owner of each of tracks A, B, C and D; the approvals are visible on the pull request.
- [ ] No trait method takes or returns a type from `std::fs`, `std::net` or `std::time`.
- [ ] A trivial in-memory implementation of all four traits passes a first version of the conformance suite, proving the traits can be implemented without the real system.

## T0.3.2 io_uring backend: direct I/O file, registered buffers

**Goal.** The production storage backend: durable, direct, completion-based file I/O on Linux.

**Work.**
- A `Storage` implementation on the `io-uring` crate: one ring per shard, the data file opened with `O_DIRECT | O_DSYNC`, registered file and registered buffers.
- Read, write and fsync with completion delivery; handling of short reads, short writes, `EINTR` and `EAGAIN`.
- Errors mapped to typed results; an I/O error is never ignored.
- The storage conformance suite: a set of tests any `Storage` implementation must pass.

**Not in this card.** Network I/O (T0.3.3), fault tolerance above the device (phase 1).

**Depends on.** T0.3.1.

**Gates.**
- [ ] The conformance suite passes on Linux x86-64 against a real file on a file system that supports direct I/O, in CI or in the virtual machine of T0.1.6.
- [ ] `strace` (or an equivalent recorded trace) shows the file opened with both `O_DIRECT` and `O_DSYNC`, and no `read`, `write`, `pread` or `pwrite` system calls on the data file after startup.
- [ ] A test using fault injection at the system-call boundary shows that a short write is completed by resubmission and that an `EIO` reaches the caller as a typed error.
- [ ] 1,000,000 mixed operations run with zero heap allocations after startup (counting allocator).
- [ ] A write that has completed is readable after the process is killed with `SIGKILL` and restarted (test with 1,000 iterations).
- [ ] The minimum supported kernel version is stated in the documentation and checked at startup with a clear error.
- [ ] `just vm-check` runs the conformance suite and it passes, and `min_kernel` in `tools/vm/check.sh` equals the minimum stated here (from T0.1.6).

## T0.3.3 io_uring backend: TCP accept, connect, send, recv

**Goal.** The production network backend, so shards can exchange messages without leaving their thread.

**Work.**
- A `Network` implementation on io_uring: listen, accept, connect, send, receive, close.
- Message framing using the header's size field; partial sends and receives reassembled.
- Per-connection bounded send queue with backpressure.
- Connection failure, reset and timeout reported as events.
- An echo benchmark between two shards.

**Not in this card.** TLS (T3.5.4), connection management across a cluster (T4.1.3).

**Depends on.** T0.3.1, T0.2.2.

**Gates.**
- [ ] The network conformance suite passes on Linux: messages of 256 bytes, 4 KiB and 1 MiB arrive complete, in order and with a valid checksum, for 1,000,000 messages.
- [ ] A peer that stops reading causes the sender's queue to fill and the send call to report backpressure; memory use stays constant (test).
- [ ] A connection reset by the peer is reported as an event and the connection's buffers are returned to the pool (test, pool counter back to zero).
- [ ] A message whose header says a size above the limit closes the connection without allocating a buffer for it.
- [ ] A report gives round-trip latency (median, p99, p99.9) and messages per second for the echo benchmark between two shard threads on one machine, on real hardware.
- [ ] Zero heap allocations per message after startup (counting allocator).

## T0.3.4 Portable backend for macOS

**Goal.** Developers on macOS can run the real server for correctness work, without io_uring.

**Work.**
- `Storage` and `Network` implementations for macOS using standard system calls (blocking in a helper or kqueue-based).
- The closest available durability calls (`F_FULLFSYNC`, `F_NOCACHE`), with their limits documented.
- A startup warning that this backend is for development only.

**Not in this card.** Performance. This backend is never benchmarked and never deployed.

**Depends on.** T0.3.2, T0.3.3 (for the conformance suites).

**Gates.**
- [ ] The storage and network conformance suites pass unchanged on macOS in CI.
- [ ] The same suites still pass on Linux, with no test skipped on either system except those listed by name in a single file, each with a reason.
- [ ] Starting a node on this backend prints a warning that it is unsupported for production, and a test asserts the warning.
- [ ] A release build on Linux cannot select this backend: the option is absent, shown by a test that the configuration is rejected.
- [ ] The durability difference from the Linux backend is written in the documentation.

## T0.3.5 Shard event loop with scheduling classes and core pinning

**Goal.** The single-threaded loop that runs one shard, giving the commit path strict priority over everything else on that core.

**Work.**
- One loop per shard: collect completions, run ready tasks by class, submit new I/O.
- Scheduling classes in strict priority order with a time budget per iteration: commit path, foreground reads, repair, compaction, queries, connection handshakes.
- Thread pinned to a configured core; memory allocated from that core's NUMA node.
- Starvation protection: a lower class is guaranteed a minimum share over a window, so repair and compaction cannot stop forever.

**Not in this card.** The stall metric and the task-slice rule (T0.3.6).

**Depends on.** T0.3.1.

**Gates.**
- [ ] A synthetic test with all classes permanently ready shows each class's share of time within 5 percentage points of its configured budget over 10,000 iterations.
- [ ] With the commit class permanently ready, the lowest class still receives its guaranteed minimum share (test).
- [ ] With only one class ready, it receives the whole iteration; budgets do not waste time.
- [ ] On Linux, the shard thread's affinity mask contains exactly the configured core, checked by reading it back in a test.
- [ ] The loop runs under the simulator's virtual time with no call to the real clock (lint from T0.1.2 passes).
- [ ] The loop performs zero heap allocations per iteration after startup (counting allocator).

## T0.3.6 Event-loop stall metric and 1 ms task-slice rule

**Goal.** A measured guarantee that no task holds a shard thread long enough to look like a dead primary, which is what the 200 ms failover budget rests on.

**Work.**
- A histogram of loop iteration time and of each task's run time, per shard, per class.
- A yield helper that long-running work (compaction, scans, checkpoint, ingest) must call, and a debug assertion that fires when a task runs longer than 1 ms without yielding.
- A benchmark harness check that fails a run when p99.9 iteration time exceeds 5 ms.
- The metric exported for production.

**Not in this card.** Making each subsystem obey the rule; each later task that adds long-running work carries that gate itself.

**Depends on.** T0.3.5.

**Gates.**
- [ ] A task that spins for 2 ms without yielding triggers the assertion in a debug build and is counted as a violation in a release build (two tests).
- [ ] A benchmark run with an injected 6 ms stall once per 500 iterations fails the harness check; the same run without it passes (injected bug).
- [ ] The histogram's reported p99.9 is within 5% of the true value for a synthetic distribution with a known p99.9 (test).
- [ ] Recording a sample costs under 50 ns on real hardware (report).
- [ ] Per-shard and per-class values are readable through the metrics interface, and a test reads them.
- [ ] In the simulator the same measurements use virtual time and are identical across two runs of one seed.

## T0.4.1 Virtual clock and seeded scheduler

**Goal.** The core of the simulator: one thread, one seed, one fully reproducible order of events for a whole cluster.

**Work.**
- A virtual clock that advances only when the scheduler says so, and skips idle time.
- An event queue ordered by virtual time with a deterministic tie-break.
- A seeded random generator, the only source of randomness in a run.
- The simulated `Time` implementation.
- An event trace that can be hashed and compared.

**Not in this card.** Simulated network and storage (T0.4.2, T0.4.3).

**Depends on.** T0.3.1.

**Gates.**
- [ ] Running one seed twice in the same process, and twice in separate processes, gives an identical trace hash, for 1,000 seeds.
- [ ] The trace hash for a fixed set of 10 seeds is identical on Linux x86-64 and macOS ARM, checked in CI.
- [ ] Two events at the same virtual time always run in the same order; a test with 10,000 simultaneous events confirms it.
- [ ] One hour of virtual time with an idle cluster completes in under one second of real time.
- [ ] No code in `crates/sim` reads the real clock, real randomness or thread ids (lint).
- [ ] Changing the seed changes the trace for a non-trivial scenario (test over 100 seed pairs).

## T0.4.2 Simulated network with drop, delay, reorder, partition

**Goal.** A network the simulator fully controls, so every protocol is tested against the failures a real network produces.

**Work.**
- The simulated `Network`: in-memory links between simulated shards and clients.
- Faults, each with a per-seed probability or schedule: drop, delay, reorder, duplicate, connection reset, symmetric partition, asymmetric partition.
- Latency models, including one that matches the data-center assumptions of the replication document.
- A log of every injected fault in the trace.

**Not in this card.** Process crashes (T0.4.4).

**Depends on.** T0.4.1, T0.2.2.

**Gates.**
- [ ] Each of the seven fault types has a test showing that it occurs when enabled and never occurs when disabled.
- [ ] With all faults disabled, 1,000,000 messages arrive exactly once and in order per link.
- [ ] An asymmetric partition delivers messages from A to B and none from B to A (test).
- [ ] The measured rate of each probabilistic fault over 1,000,000 messages is within 1% (absolute) of its configured probability.
- [ ] Fault settings are derived from the seed alone; the same seed gives the same faults at the same virtual times (trace hash equal over 1,000 seeds).
- [ ] The network passes the same conformance suite as the real backends when all faults are disabled.

## T0.4.3 Simulated storage with torn writes, corruption, misdirection, latency

**Goal.** A disk the simulator fully controls, producing exactly the storage faults of the fault model and no more than the model allows.

**Work.**
- The simulated `Storage`: an in-memory data file per simulated shard.
- Faults: torn write on crash, bit flip, misdirected write, misdirected read, lost write that was acknowledged, read error, latency.
- Bounds that keep the run inside the fault model, for example never corrupting the same block or the same WAL slot on a quorum of replicas.
- A record of every fault, so a checker can tell an injected fault from a real bug.

**Not in this card.** The recovery logic that survives these faults (phase 1 and phase 2).

**Depends on.** T0.4.1.

**Gates.**
- [ ] Each of the seven fault types has a test showing that it occurs when enabled and never occurs when disabled.
- [ ] A write in flight at a crash leaves a state where each sector holds either the old or the new content, with at least one case of a mix across sectors (test over 1,000 crashes).
- [ ] The bound is enforced: over 10,000 seeds with maximum fault rates, no block and no WAL slot is ever corrupt on a replication quorum of a group at the same time. An assertion in the simulator checks it on every injection.
- [ ] With the bound deliberately disabled, the assertion fires within 1,000 seeds (injected bug).
- [ ] With all faults disabled, the storage passes the same conformance suite as the real backend.
- [ ] Fault injection is a function of the seed only (trace hash equal over 1,000 seeds).

## T0.4.4 Process crash and restart at I/O boundaries

**Goal.** The simulator can kill and restart any process at any point where it touches the outside world, which is where crash bugs live.

**Work.**
- Crash and restart of a simulated shard or node at any I/O submission or completion, chosen from the seed.
- On crash: volatile memory lost, in-flight writes torn per T0.4.3, in-flight messages dropped.
- Pause and slow-shard faults.
- A toy replicated counter with a deliberately simple protocol, used to prove the machinery.

**Not in this card.** The real replication protocol (phase 2).

**Depends on.** T0.4.2, T0.4.3.

**Gates.**
- [ ] Seed gate 10,000 on the toy replicated counter with network, storage and crash faults enabled: the counter never goes backwards and never loses an acknowledged increment.
- [ ] The same toy with its durable write removed before the acknowledgement fails within 1,000 seeds (injected bug).
- [ ] Over 10,000 seeds, a crash was injected at every distinct I/O call site in the toy at least once; a coverage report lists the sites and counts.
- [ ] A restarted process sees no memory from before the crash; a test that tries to read a stale in-memory value fails to find it.
- [ ] A paused shard resumes with its state intact and with virtual time advanced (test).

## T0.4.5 Seed runner, failure reporting, reproduction command

**Goal.** Running many seeds is one command, and any failure can be handed to another engineer as a single line that reproduces it exactly.

**Work.**
- A runner binary in `crates/sim`: run one seed, a range, or random seeds for a duration, in parallel across cores.
- On failure: the seed, the commit hash, the failing checker, the last events before the failure, and one command that reproduces it.
- `just sim` recipes for the common cases.
- Swarm testing: each seed chooses which faults and workload features are on.
- An option to dump the full trace of one seed.

**Not in this card.** The always-on fleet and automatic issue filing (T2.3.4).

**Depends on.** T0.4.4.

**Gates.**
- [ ] For a deliberately failing scenario, the printed command, pasted unchanged into a shell on another machine at the same commit, fails at the same event with the same message (shown on Linux and macOS).
- [ ] The runner exits non-zero if any seed fails and zero only if all pass.
- [ ] Running seeds on 8 cores is at least 6 times faster than on one core for a fixed range, and the set of failing seeds is identical.
- [ ] A panic, an assertion failure and a detected hang (no progress in virtual time) are each reported as a failure with a reproduction command (three tests).
- [ ] The swarm configuration of a seed is printed with the failure and is a function of the seed only.
- [ ] `just sim` with no arguments runs the fixed seed list used by CI.
- [ ] `just sim` runs the fixed seed list `crates/sim/ci-seeds.txt` and `just sim-range FIRST COUNT` runs a range; the seed step of `.github/workflows/ci.yml` and the nightly workflow call them, and their warning guards are removed (from T0.1.3).
- [ ] A pull request that breaks a fixed seed fails the seed step of CI (injected bug) (from T0.1.3).
- [ ] The nightly workflow runs at least 10,000 further seeds (from T0.1.3).
- [ ] The pull-request workflow finishes in under 15 minutes wall-clock at the 95th percentile over 20 consecutive runs with the seed step running. The run times are recorded (from T0.1.3).
