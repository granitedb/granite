# Roadmap

Structure: **phase** (a milestone with exit criteria) > **epic** (a GitHub milestone or parent issue) > **task** (one GitHub issue, roughly one to two weeks for one engineer). Tracks A to D are defined in [06-testing-and-engineering.md](06-testing-and-engineering.md) section 7.

Estimates assume 4 engineers and are rough; they are the least reliable part of the plan. With 3 engineers, add about a third. Total to the end of phase 6: roughly 22 to 30 months.

```
month   0    3    6    9    12   15   18   21   24   27
P0      ██
P1       ████████            (A, C)
P2         ██████████        (B, D)      P1 and P2 overlap
P3                 ███████   (C, A)
P4a                     ████         (B, C)  static multi-shard cluster: E4.1 to E4.3
P5                          ████████ (B, C)  cross-shard transactions
P4b                                 █████    (B)  tablet split, merge, move: E4.4, E4.5
P6                         ██████████████ (A, C)  starts after P3
P7                                   ████████████ ongoing
```

**Order changed on 2026-11-17.** The first application is an exchange where almost every transaction is between two users on different shards, so cross-shard transactions are the product's main path. Phase 4 is split: the static cluster (4a) comes first, then phase 5, and only then tablet split, merge and move (4b). A cluster with a fixed tablet map and cross-shard transactions is useful; one that can rebalance but cannot settle a trade is not.

**Hardware dependency.** Development starts on local machines. The gates below need production-like servers: one by the end of phase 1, three by the end of phase 2, six for phase 5 (table in [06-testing-and-engineering.md](06-testing-and-engineering.md) section 0). Without them a gate can be prepared but not passed.

Decision gates:
- **G1, end of phase 1:** for 20 to 100 column tables with point reads of any age, which layout (`row`, `columnar`, grouped) meets point-read and write-amplification targets? Sets the recommended layouts and chunk sizes, and decides whether the columnar projection moves earlier than phase 6.
- **G2, end of phase 3:** ledger benchmark against TigerBeetle. Within 2x proceeds; otherwise a performance phase is inserted.
- **G3, mid phase 5:** on the exchange settlement workload with nearly 100% cross-shard independent transactions, does cluster throughput reach the agreed fraction of single-shard throughput (design estimate: somewhat under half)? If not, a performance phase is inserted before tablet lifecycle work. Conflict-aware bypass is required regardless, for failure isolation.

---

## Phase 0. Foundations (about 2 months, everyone)

Exit: an empty cluster of simulated shards exchanges checksummed messages under fault injection, reproducibly from a seed, on Linux and macOS.

### E0.1 Repository and conventions
| ID | Task | Track | Done when |
|---|---|---|---|
| T0.1.1 | Create workspace, crate skeletons, pinned toolchain, dependency policy | D | `cargo build` and `cargo test` pass in CI on Linux and macOS |
| T0.1.5 | Open-source setup: licence, CONTRIBUTING, SECURITY, code of conduct, sign-off rule, licence checks | D | Files present; `cargo-deny` runs in CI |
| T0.1.6 | Local Linux virtual machine recipe for io_uring development | D | One command gives a developer on macOS a Linux environment that passes the Io conformance suite |
| T0.1.2 | Write coding rules and determinism lints | D | Lint configuration rejects the banned APIs listed in doc 06 section 3 |
| T0.1.3 | CI pipeline: build, test, lint, format, fixed simulator seeds | D | Every pull request runs it in under 15 minutes |
| T0.1.4 | Constants and configuration module with compile-time validation | A | All limits from docs live in one place with static assertions on their relationships |

### E0.2 Base primitives
| ID | Task | Track | Done when |
|---|---|---|---|
| T0.2.1 | Benchmark and choose the 128-bit checksum | A | Decision recorded with numbers on target hardware |
| T0.2.2 | Message header struct, encode, decode, validation | C | Fuzzer finds no panic; round-trip property test passes |
| T0.2.3 | Message pool and buffer management with fixed capacity | A | Exhaustion returns backpressure, never allocates |
| T0.2.4 | Id128 time-ordered id generator | C | Monotonic within a process under clock regressions |

### E0.3 Io layer
| ID | Task | Track | Done when |
|---|---|---|---|
| T0.3.1 | Define `Storage`, `Network`, `Time` traits and completion model | D | Reviewed by all tracks; spike compares explicit state machines with async |
| T0.3.2 | io_uring backend: direct I/O file, registered buffers | D | Passes a read/write/fsync conformance suite on Linux |
| T0.3.3 | io_uring backend: TCP accept, connect, send, recv | D | Echo benchmark across two shards |
| T0.3.4 | Portable backend for macOS | D | Same conformance suite passes |
| T0.3.5 | Shard event loop with scheduling classes and core pinning | D | Budgets enforced in a synthetic test |
| T0.3.6 | Event-loop stall metric and 1 ms task-slice rule | D | p99.9 loop iteration tracked; benchmark fails above 5 ms |

### E0.4 Simulator skeleton
| ID | Task | Track | Done when |
|---|---|---|---|
| T0.4.1 | Virtual clock and seeded scheduler | D | Same seed gives identical event trace twice |
| T0.4.2 | Simulated network with drop, delay, reorder, partition | D | Faults configurable per seed |
| T0.4.3 | Simulated storage with torn writes, corruption, misdirection, latency | D | Fault bounds enforce the fault model |
| T0.4.4 | Process crash and restart at I/O boundaries | D | A toy replicated counter survives 10,000 seeds |
| T0.4.5 | Seed runner, failure reporting, reproduction command | D | A failing seed prints one command that reproduces it |

---

## Phase 1. Single-shard storage engine (about 5 months, tracks A and C)

Exit: one shard, no replication, serves typed inserts, gets and scans for immutable tables; survives simulated crashes and disk faults that are recoverable locally; gate G1 answered.

### E1.1 Data file
| ID | Task | Track | Done when |
|---|---|---|---|
| T1.1.1 | Data file format, `format` command, deterministic grid growth | A | Zones laid out per doc 01; two instances fed the same ops grow at the same op; `no_space` at the limit |
| T1.1.2 | Superblock: 4 copies, quorum open, atomic update | A | Simulator: any crash during update opens the old or new state, never neither |
| T1.1.3 | WAL: header ring and prepare ring, write path | A | Hash chain verified on append |
| T1.1.4 | WAL recovery with slot classification | A | Torn versus corrupt slots distinguished in simulator tests |
| T1.1.5 | Grid: block read, write, checksum, address verification | A | Misdirected and corrupt blocks detected |
| T1.1.6 | Free set with checkpoint-deferred release | A | Crash at any point never leaks or double-allocates a block |
| T1.1.7 | Block cache | A | Fixed memory; hit-rate metric exposed |

### E1.2 LSM
| ID | Task | Track | Done when |
|---|---|---|---|
| T1.2.1 | Memtable with arena and fixed capacity | A | Model test against `BTreeMap` |
| T1.2.2 | Manifest and levels | A | Manifest persisted in grid, reloaded at open |
| T1.2.3 | Flush memtable to a level-0 part | A | Content equals model |
| T1.2.4 | Compaction with op-number pacing | A | Two instances fed the same ops produce byte-identical files |
| T1.2.5 | Merge iterators and range scans | A | Model test including across flush and compaction |
| T1.2.6 | Checkpoint and replay | A | Crash anywhere, reopen, state equals model |

### E1.3 Columnar parts
| ID | Task | Track | Done when |
|---|---|---|---|
| T1.3.10 | Relocatable parts: part-relative block references and extent map | A | A part copied to different grid addresses reads identically |
| T1.3.1 | Part format: header, key index, column groups, per-group ordinal index, chunks | A | Documented format with version; round-trip test for `row`, `columnar` and `groups` layouts |
| T1.3.8 | Partial block reads verified by chunk checksum | A | Point read fetches only the chunk's sectors; corrupt chunk triggers block repair |
| T1.3.9 | Row cache for latest versions | A | Fixed memory; model test shows no stale read |
| T1.3.2 | Lightweight codecs: plain, bit-packing with frame of reference, dictionary | A | Round-trip fuzz; random access by row index |
| T1.3.3 | Variable-length and nullable column chunks | A | Round-trip fuzz |
| T1.3.4 | Primary-key filter (bloom or ribbon) | A | False-positive rate measured against target |
| T1.3.5 | Point-read path through parts | A | Latency benchmark by level depth recorded |
| T1.3.6 | Filter cache with its own memory budget | A | Budget enforced; uncached-filter read counted in metrics |
| T1.3.7 | G1 experiment and written decision | A | Report for 20, 50, 100 columns, uniform-age point reads, three layouts: latency, write amplification, space, scan speed |

### E1.4 Types and schema
| ID | Task | Track | Done when |
|---|---|---|---|
| T1.4.1 | Type system and order-preserving key encoding | C | Property test: encoded order equals logical order for every key type |
| T1.4.2 | Checked arithmetic and casts for integers and decimals | C | Exhaustive edge tests; no wrapping anywhere |
| T1.4.3 | Schema catalog: table groups, tables, validation, static row bound | C | Invalid schemas rejected with specific codes |
| T1.4.4 | Row batch wire format and validator | C | Fuzzed; zero-copy access |
| T1.4.5 | Schema text syntax and compiler in client tooling | C | Example schemas compile to the binary form |

### E1.5 State machine, first version
| ID | Task | Track | Done when |
|---|---|---|---|
| T1.5.1 | Operations: insert into immutable tables with uniqueness check | C | Model test |
| T1.5.2 | Operations: get and scan | C | Model test |
| T1.5.3 | Reference model in `sim` | D | Covers everything in E1.5 |
| T1.5.4 | Single-shard server binary and minimal Rust client | C | End-to-end insert and read over TCP |
| T1.5.5 | Benchmark harness and first tracked numbers | A | Nightly results stored per commit; 1 MiB against 4 MiB message size compared for 1 KiB rows |

---

## Phase 2. Replication (about 6 months, tracks B and D, overlapping phase 1)

Exit: a single group of 3 or 5 replicas passes the invariants in doc 02 section 11 over a large number of simulator seeds with all fault types enabled, and runs on real machines.

### E2.1 Specification
| ID | Task | Track | Done when |
|---|---|---|---|
| T2.1.1 | Read and annotate VR Revisited, protocol-aware recovery, TigerBeetle protocol docs | B | Written list of rules adopted and deviations |
| T2.1.2 | Model-check normal operation and view change with storage faults | B | Specification in repository; safety properties hold in the checker |

### E2.2 Protocol
| ID | Task | Track | Done when |
|---|---|---|---|
| T2.2.1 | Replica skeleton: status, message dispatch, timeouts | B | Runs in simulator with a no-op state machine |
| T2.2.2 | Normal operation: prepare, prepare_ok, commit, pipeline | B | No faults: committed ops identical on all replicas |
| T2.2.3 | View change including nack-based truncation | B | Invariants 1 and 2 hold under network and crash faults |
| T2.2.4 | WAL repair: headers and prepares | B | Holds under storage faults |
| T2.2.5 | Recovery and `recovering_head` | B | Holds with crash plus torn head |
| T2.2.6 | Grid repair and background scrubber | B | Corrupt blocks healed from peers |
| T2.2.7 | Checkpoint coordination and state sync | B | A replica left behind by more than the WAL rejoins |
| T2.2.8 | Clock synchronisation and timestamp assignment | B | Invariant 5 holds under clock faults; primary with bad clock steps down |
| T2.2.9 | Client sessions for writes in an LSM tree, inline and block-backed replies, eviction | B | Invariant 4 holds under retries and view changes with 16,384 sessions |
| T2.2.12 | Request coalescing: multi-frame prepares across clients and operations | B | Throughput with 1,000 single-row clients within 2x of one batching client |
| T2.2.13 | Linearizable reads without the log (quorum-confirmed leadership) | B | Invariant 7 holds under partitions and view changes |
| T2.2.14 | Quorum-gated suspicion and failover timing | B | Invariants 8 and 9 hold in simulation; under 200 ms measured on real hardware |
| T2.2.10 | Ring replication with fallback | B | Bandwidth on primary measured |
| T2.2.11 | Standbys | B | Standby follows and can be promoted by configuration |

### E2.3 Integration and hardening
| ID | Task | Track | Done when |
|---|---|---|---|
| T2.3.1 | Connect the phase 1 state machine to VSR | B, C | Storage convergence checker passes at every checkpoint |
| T2.3.2 | Linearizability checker against the reference model | D | Detects a deliberately injected bug |
| T2.3.3 | Liveness checker | D | Detects a deliberately injected stall |
| T2.3.4 | Continuous simulator fleet | D | Random seeds run around the clock, failures filed automatically |
| T2.3.5 | Real-cluster deployment, crash and power-fail tests | D | Three machines, scripted faults, no invariant violation |
| T2.3.6 | Replicated benchmark numbers | A | Insert throughput and latency for RF 3 tracked |

---

## Phase 3. Transactions (about 4 months, tracks C and A)

Exit: versioned tables, conditional writes, linked events, programs, derived tables and local indexes, all single-shard and replicated; gate G2 answered. This is the first externally useful release.

### E3.1 Versioned tables
| ID | Task | Track | Done when |
|---|---|---|---|
| T3.1.1 | Versioned sort key, tombstones, snapshot resolution in reads | A | Model test with reads at arbitrary snapshots |
| T3.1.2 | Retention horizon in compaction | A | Old versions dropped exactly per rule, deterministically |
| T3.1.3 | Operations: update, delete, upsert | C | Model test |
| T3.1.4 | Snapshot reads on backups | B | Snapshot stability checker passes |

### E3.2 Conditions and linked events
| ID | Task | Track | Done when |
|---|---|---|---|
| T3.2.1 | Per-event preconditions (not_exists, exists, version, column compare) | C | Model test; documented as the LWT feature |
| T3.2.2 | Linked event chains with rollback | C | Any failure in a chain leaves no trace |

### E3.3 Programs
| ID | Task | Track | Done when |
|---|---|---|---|
| T3.3.1 | IR definition and binary encoding | C | Versioned; fuzzed decoder |
| T3.3.2 | Validator: types, bounds, key provenance, shard set | C | Rejects each class of invalid program with a specific code |
| T3.3.3 | Executor with per-invocation write buffer | C | Model test with random programs |
| T3.3.4 | Program text syntax and compiler in tooling | C | The transfer example compiles and runs |
| T3.3.5 | Register, retire and inline programs | C | Replicated through the log |

### E3.4 Derived tables and indexes
| ID | Task | Track | Done when |
|---|---|---|---|
| T3.4.1 | Derived table definition and maintenance in the same op, with overflow-free accumulators | C | Model test: derived state always equals fold over source; validator proves no overflow |
| T3.4.5 | Local unique indexes | A | Enforced within a partition in model test |
| T3.4.6 | Exchange reference schema: entries, balances, orders, place_order | C | Compiles; single-group benchmark recorded alongside the ledger benchmark |
| T3.4.2 | Reads of derived tables in preconditions and programs | C | Balance-check transfer works |
| T3.4.3 | Local secondary indexes | A | Always consistent with base table in model test |
| T3.4.4 | Pending/post/void schema pattern library | C | Example with timeouts passes model test |

### E3.6 Change feed
| ID | Task | Track | Done when |
|---|---|---|---|
| T3.6.1 | Subscription protocol, cursor, server-side table and column filters | C | At-least-once delivery with resumable cursor in simulator under faults |
| T3.6.2 | Serve from WAL on backups; fall back to parts by timestamp | A, B | A consumer that lags past the WAL catches up without gaps or duplicates beyond the cursor |
| T3.6.3 | Client subscription API and exactly-once consumer example | C | Balance-following example passes a model test |

### E3.5 Schema changes and release
| ID | Task | Track | Done when |
|---|---|---|---|
| T3.5.1 | Online add/drop column and index | C | Parts with mixed schema versions read correctly |
| T3.5.2 | Client code generation from schema | C | Typed structs and stubs for the example schemas |
| T3.5.3 | Ledger benchmark and G2 report | A | Comparison with TigerBeetle on the same hardware, written up |
| T3.5.4 | Mutual TLS and authorisation per table group | D | Unauthorised access rejected; overhead measured |
| T3.5.5 | Operator documentation and first tagged release | all | A new user can run a 3-replica cluster from the docs |

---

## Phase 4. Multi-shard cluster (about 6 months, tracks B and C)

Exit: many groups across many nodes; shard-aware client; tablets split, merge and move under load with no invariant violation. Transactions are still single-shard.

### E4.1 Node process
| ID | Task | Track | Done when |
|---|---|---|---|
| T4.1.1 | Multi-shard process: one thread and data file per core, per-shard ports | D | N independent groups run in one process |
| T4.1.2 | Group id and epochs in every message; rejection of misrouted traffic | B | Simulator test with misrouting |
| T4.1.3 | Intra-process shard rings and cross-node connection management | D | Bounded connections; backpressure tested |

### E4.2 Metadata group
| ID | Task | Track | Done when |
|---|---|---|---|
| T4.2.1 | Metadata state machine: nodes, groups, schema, tablet map | B | Replicated; model test |
| T4.2.2 | Schema and program distribution to data groups at a defined op | B, C | All replicas of a group switch at the same op |
| T4.2.3 | Subscription and cache protocol for shards and clients | B | Stale caches corrected by `stale_map` |
| T4.2.4 | Cluster bootstrap and node join | B | Scripted creation of a multi-node cluster |

### E4.3 Routing
| ID | Task | Track | Done when |
|---|---|---|---|
| T4.3.1 | Token hashing and tablet lookup | C | Fixed test vectors; part of format |
| T4.3.2 | Shard-aware client: split batches by tablet, reassemble | C | Results in caller order under retries |
| T4.3.3 | Ownership checks on the shard | C | Invariant 6 of doc 04 holds |

### E4.4 Tablet lifecycle
| ID | Task | Track | Done when |
|---|---|---|---|
| T4.4.1 | Model-check tablet move | B | Specification in repository |
| T4.4.2 | Split and merge | B | No data movement; map consistent |
| T4.4.3 | Move: reserve_ingest, block fetch by checksum, install clipped parts | B, A | Target replicas stay byte-identical; crash between reserve and install recovers |
| T4.4.6 | Move: tail catch-up through the target log | B | Lag converges under write load |
| T4.4.4 | Move: freeze, adopt, release, range delete | B | Invariant 7 holds under faults during every step |
| T4.4.7 | Move rate benchmark | A | A 16 GiB tablet moves within the agreed time with OLTP latency impact measured |
| T4.4.5 | Balancer | B | Converges to even size and load in simulation |

### E4.5 Cluster testing
| ID | Task | Track | Done when |
|---|---|---|---|
| T4.5.1 | Simulator: multi-group clusters with topology faults | D | Moves, splits and node churn randomised per seed |
| T4.5.2 | Scale benchmark: throughput against cores and nodes | A | Near-linear single-shard scaling shown or gaps explained |

---

## Phase 5. Cross-shard transactions (about 5 months, track B with C)

Exit: independent and coordinated transactions pass the invariants in doc 04 section 6; consistent multi-shard snapshot reads; gate G3 answered.

### E5.1 Specification
| ID | Task | Track | Done when |
|---|---|---|---|
| T5.1.1 | Read and annotate the Granola paper and thesis; list deviations | B | Written and reviewed |
| T5.1.2 | Model-check our protocol including view changes and locks | B | Atomicity and serializability hold in the checker |

### E5.2 Protocol
| ID | Task | Track | Done when |
|---|---|---|---|
| T5.2.1 | Pending-transaction table and timestamp-order queue | B | Replicated state; bounded |
| T5.2.2 | Independent transactions: propose, vote, finalize, execute | B | Invariants 1 to 3 hold |
| T5.2.3 | Vote retransmission and proposal forwarding after failures | B | Holds under crash and partition |
| T5.2.4 | Lock table and coordinated transactions | B | No deadlock in simulation; conflicts retried |
| T5.2.5 | Hybrid clock floor and client timestamp propagation | B | Causal order checker passes |
| T5.2.6 | Automatic classification of programs | C | Validator output matches hand classification on a test corpus |
| T5.2.7 | Client: participant computation, fan-out, retry policy | C | End-to-end cross-shard transfer |
| T5.2.8 | Interaction with tablet moves (done in phase 4b) | B | Pending transactions drain before freeze |
| T5.2.9 | Vote and finalize batching between group pairs | B | Per-transaction vote traffic and log bytes measured |
| T5.2.10 | Same-group shortcut | C | Transactions whose keys share a group take the single-shard path |
| T5.2.11 | `audit` statement and system violations table | C | Non-aborting; model test |
| T5.2.12 | Global unique indexes over coordinated transactions | C | Uniqueness holds cluster-wide under concurrent inserts and faults |

### E5.3 Consistent reads
| ID | Task | Track | Done when |
|---|---|---|---|
| T5.3.1 | Shard safe timestamp and idle heartbeats | B | Advances on idle groups |
| T5.3.2 | Snapshot reads that wait for the shard safe timestamp, with timeout | B | Invariant 5 holds; `unavailable` when blocked by a dead group |
| T5.3.3 | Optional commit-wait | B | Strictness checker passes with it enabled |
| T5.3.4 | Multi-group change feed merged by timestamp in the client | C | Legs of a cross-shard transaction delivered together, in global timestamp order |

### E5.4 Evaluation
| ID | Task | Track | Done when |
|---|---|---|---|
| T5.4.1 | Global serializability checker | D | Detects an injected ordering bug |
| T5.4.2 | Exchange settlement benchmark (place order, settle trade) at 10%, 50%, 100% cross-shard; G3 report | A | Throughput and latency against single-shard baseline; decision on conflict-aware bypass recorded |
| T5.4.3 | Conflict-aware bypass (required) | B | Same checkers pass; invariant 9 of doc 04 holds with one group down |
| T5.4.4 | Group reachability tracking and fail-fast `unavailable` | B | No pile-up of pending transactions against a dead group |
| T5.4.5 | Finished-transaction table with retention window; `too_old` | B | Duplicate cross-shard requests answered, never re-executed |
| T5.4.6 | Remote timestamp guard | B | A node with a clock far ahead is rejected, cluster timestamps unaffected |

---

## Phase 6. HTAP (about 7 months, tracks A and C, can start after phase 3)

Exit: analytical queries over a consistent snapshot across shards, with a measured bound on OLTP latency impact.

### E6.1 Storage for scans
| ID | Task | Track | Done when |
|---|---|---|---|
| T6.1.1 | Heavy codecs: delta, double-delta, RLE, FSST, LZ4, ZSTD | A | Round-trip fuzz; deterministic output across machines |
| T6.1.2 | Level-dependent encoding policy | A | Deterministic choice; compression ratio tracked |
| T6.1.3 | Skip indexes: min/max, bloom, set | A | Pruning rate measured |
| T6.1.4 | Columnar projection for `row` tables | A | Opt-in; scans use it, point reads do not; consistent with base in model test |
| T6.1.5 | Time segments: one tree per time bucket | A | Time-range pruning by segment survives full compaction |

### E6.2 Execution
| ID | Task | Track | Done when |
|---|---|---|---|
| T6.2.1 | Scan fragment IR, validator and memory bounding | C | Unbounded fragments rejected |
| T6.2.2 | Vectorised scan with version resolution and pushdown | A | Results equal reference model |
| T6.2.3 | Filter, project, partial aggregate with bounded flush, top-N | C | Results equal reference model |
| T6.2.4 | SIMD kernels (AVX2 baseline) with scalar reference implementations | A | Bit-identical results between SIMD and scalar paths, checked in CI on x86-64 |
| T6.2.5 | Deterministic mergeable sketches | C | Error bounds tested |

### E6.4 SQL gateway
| ID | Task | Track | Done when |
|---|---|---|---|
| T6.4.1 | Evaluate the query engine and its pushdown interfaces; written decision | C | Spike runs a SQL query over a stub table provider |
| T6.4.2 | Table provider: schema mapping, projection, filter and limit pushdown | C | SQL results equal reference model on a query corpus |
| T6.4.3 | Partial-aggregate pushdown and merge | C | Aggregations do not ship raw rows to the gateway |
| T6.4.4 | Snapshot selection, fan-out, replica preference, retries | C | One timestamp per query; survives a view change mid-query |
| T6.4.5 | Gateway service: read-only credential, admission, memory limit, spill, timeouts | D | A runaway query fails in the gateway with no shard latency impact |
| T6.4.6 | Arrow Flight SQL endpoint | C | Query from Python and a JDBC tool |

### E6.3 Distribution and isolation
| ID | Task | Track | Done when |
|---|---|---|---|
| T6.3.1 | Fragment fan-out and partial-aggregate merge in the typed client | C | Multi-shard query at one snapshot timestamp |
| T6.3.2 | Scan workers on pinned manifest snapshots | A | No mutable state shared; checked under thread sanitizer |
| T6.3.3 | Admission control and scheduling budgets | D | OLTP latency under query load within the agreed bound |
| T6.3.4 | Arrow-compatible result batches in the client | C | Zero-copy into Arrow arrays |
| T6.3.5 | Composite types: Array, Tuple, Map, Json | C | Storage, wire and expressions |

---

## Phase 7. Operations (ongoing, starts alongside phase 4)

### E7.1 Membership
| ID | Task | Track | Done when |
|---|---|---|---|
| T7.1.1 | Model-check group reconfiguration | B | Specification in repository |
| T7.1.2 | Reconfigure op, epoch change at checkpoint boundary | B | Replica replacement under faults in simulator |
| T7.1.3 | Node add, drain and remove workflows | B | Scripted and tested |

### E7.2 Lifecycle
| ID | Task | Track | Done when |
|---|---|---|---|
| T7.2.1 | Versioned upgrade op and multi-version binaries | B | Rolling upgrade in simulator with mixed versions |
| T7.2.2 | Backup and restore | A | Restore to a consistent cluster safe timestamp |
| T7.2.3 | Server-stored named cursors for the change feed | C | A consumer restarts without keeping its own cursor |
| T7.2.5 | Retention `drop after`: drop whole segments | A | Deterministic across replicas; space reclaimed |
| T7.2.6 | Retention `archive after`: move segments to object storage | A | Transparent reads of archived data |

### E7.4 Standby site
| ID | Task | Track | Done when |
|---|---|---|---|
| T7.4.1 | Model-check consistent apply and promotion | B | Invariant 8 of doc 04 holds in the checker |
| T7.4.2 | Remote standbys with asynchronous log shipping | B | Primary-site commit latency unaffected by standby lag |
| T7.4.3 | Standby coordinator and apply timestamp | B | Standby state always equals primary state at one timestamp |
| T7.4.4 | Promotion with epoch fencing, and failback by state sync | B | Scripted site failover drill; data loss bounded and reported |

### E7.3 Operability and security
| ID | Task | Track | Done when |
|---|---|---|---|
| T7.3.1 | Metrics, tracing and structured logs per shard | D | Dashboards for commit latency, compaction debt, repair |
| T7.3.2 | Admin tool: inspect data file, WAL, manifest | A | Used in at least one real debugging session |
| T7.3.3 | C ABI over the Rust client core | C | Stable header; used by both bindings below |
| T7.3.4 | Go client | C | Passes the client conformance suite |
| T7.3.5 | Python client | C | Passes the client conformance suite; results as Arrow |
| T7.3.6 | External correctness audit (Jepsen-style) | all | Report published or findings fixed |

---

## Turning this into GitHub cards

Mapping:
- One GitHub Project board with columns Backlog, Ready, In progress, Review, Done.
- One milestone per phase.
- One parent issue per epic with a task list; one issue per task. The "Done when" column here is a one-line summary. The full specification and the strict gates of every task are in [tasks/](tasks/README.md), and that text is what the issue shows.
- Labels: `track:A` to `track:D`, `phase:0` to `phase:7`, `kind:spec`, `kind:impl`, `kind:test`, `kind:bench`, `gate`.
- Every task has an issue. Later phases will change as earlier ones teach us things; a change is made in `tasks/` and published with `tools/roadmap_issues.py update`.
