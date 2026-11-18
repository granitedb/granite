# Granite: design overview

A columnar OLTP database in Rust. Status: draft 3, reviewed document by document, 2026-11-18. "The database" in these documents means Granite.

This file is the entry point. Details live in `docs/`:

| Document | Contents |
|---|---|
| [docs/01-storage.md](docs/01-storage.md) | Data file, WAL, grid, LSM, columnar part format, codecs, compaction |
| [docs/02-replication.md](docs/02-replication.md) | VSR protocol, repair, checkpoints, state sync, clocks, client sessions |
| [docs/03-data-model-and-transactions.md](docs/03-data-model-and-transactions.md) | Types, schema, table kinds, transaction IR, derived tables, single-shard execution |
| [docs/04-cluster-and-cross-shard.md](docs/04-cluster-and-cross-shard.md) | Nodes, shards, groups, tablets, metadata group, rebalancing, cross-shard protocol |
| [docs/05-query-and-protocol.md](docs/05-query-and-protocol.md) | Wire protocol, client library, read paths, analytical execution |
| [docs/06-testing-and-engineering.md](docs/06-testing-and-engineering.md) | Simulator, checkers, model checking, coding rules, crates, benchmarks |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Phases, epics, issue-sized tasks with acceptance criteria |

## 1. Goals and non-goals

Goals, in priority order:
1. **Safety.** No acknowledged write is lost or reordered under process, network, clock and disk faults that the fault model covers.
2. **Performance.** Single-shard transactions cost one consensus round and one fsync per batch of thousands. Target to validate at the end of phase 3: within 2x of TigerBeetle on the same hardware for a two-row transfer workload.
3. **Strictness.** Typed schema, no implicit casts, no silent overflow, every size bounded, every limit static.
4. **HTAP.** Analytical scans over fresh data at a consistent snapshot, without a second system.
5. **Scale-out.** Linear scaling of single-shard work with cores and nodes.

Non-goals:
- SQL for writes or transactions, and MySQL or Postgres wire compatibility. (A read-only SQL gateway for analytics is in scope; see document 05.)
- Interactive transactions.
- Eventual consistency or tunable consistency levels.
- Byzantine fault tolerance.
- Windows or non-Linux production targets.
- Multi-region active-active in the first design (the protocol does not preclude it, but nothing is tuned for it).

## 2. Decisions

| Area | Decision | Status |
|---|---|---|
| Workload | Append-heavy transactional inserts, read-modify-write without in-place overwrite, HTAP scans | confirmed |
| Interface | Typed binary protocol, schema-defined, batched | confirmed |
| Transaction model | One-shot only | confirmed |
| Mutability | Per table: `immutable` or `versioned` | confirmed |
| Cross-shard | Granola-derived timestamp-ordered protocol | confirmed |
| Consensus | VSR, one group per core-shard holding many tablets | confirmed |
| Execution | One single-threaded shard per core | confirmed |
| Platform | Linux production (io_uring, direct I/O), macOS for development | confirmed |
| Language, team | Rust, 3 to 4 engineers | confirmed |
| Determinism | Byte-identical data files across replicas of a group; codec versions pinned | confirmed |
| Programs | Pre-registered, and also accepted inline in a request | confirmed |
| Replication factor | 3 by default, 5 supported, standbys in the first replicated release | confirmed |
| Security | Mutual TLS plus authorisation per table group, in the first replicated release | confirmed |
| Tenancy | One cluster per tenant; no namespaces or quotas | confirmed |
| Row shape | Defaults tuned for 20 to 100 columns, 256 B to 2 KiB per row | confirmed |
| Retention | Per table: keep forever, archive after, or drop after. Keep-forever only until time segments exist | confirmed |
| Time segments | Within the analytics phase (phase 6); drop and archive retention follow in phase 7 | confirmed |
| Analytics | Ad hoc exploration through a separate, stateless, read-only SQL gateway; shards execute only bounded scan fragments | confirmed |
| Change feed | Built in phase 3; per-group ordered streams served from backups, merged across groups by timestamp from phase 5 | confirmed |
| Clients | Rust, then Go, then Python | confirmed |
| First benchmark | Ledger workload against TigerBeetle at the end of phase 3 | confirmed |
| Table layout | Declared per table, no default: `row`, `columnar` or explicit column groups | confirmed |
| Data file | Grows on demand in fixed steps to a per-group limit; encryption at rest left to the volume | confirmed |
| Deployment | Bare metal enterprise NVMe, one data center, hundreds to thousands of client processes | confirmed |
| Failover | Under 200 ms; quorum-gated suspicion and a 1 ms task-slice rule on shard threads | confirmed |
| Reads | Linearizable reads bypass the log; sessions cover writes only | confirmed |
| First application | Exchange or trading; transactions between two arbitrary users | confirmed |
| Main write path | Independent cross-shard transactions (reserve single-shard, then settle unconditionally) | confirmed |
| Build order | Static cluster, then cross-shard transactions, then tablet split, merge and move | confirmed |
| Uniqueness | Local unique indexes from phase 3; global unique indexes from phase 5 via coordinated transactions | confirmed |
| Definition language | Text schema and program files, compiled; generates Rust, Go and Python client code | confirmed |
| Storage engine | One LSM engine: row-ordered memtable, immutable PAX parts; chunk sizes set by gate G1 | proposed |
| Licence | Open source from the first commit; Apache-2.0 proposed, to confirm | confirmed |
| CPU | x86-64 only in production; development also on ARM laptops via scalar reference paths | confirmed |
| Test hardware | Local machines for now; production-like servers required for gates G1 to G3 | confirmed |
| Sizing | 6 nodes, 10 to 100 TB before replication, fairly uniform user activity | confirmed |
| Site loss | Asynchronous standby site, manual failover, seconds of data loss at most | confirmed |
| Failure isolation | Conflict-aware bypass and fail-fast are required, so a dead group delays only transactions that involve it | confirmed |
| Consistent reads | Each shard waits for its own safe timestamp; no cluster-wide value, no metadata dependency | confirmed |
| Sharding | 64-bit token ranges (tablets, target 16 GiB) with an explicit epoch-versioned map | proposed |
| Tablet movement | Immutable parts shipped as blocks by checksum; only the recent tail goes through the target's log | proposed |

## 3. Architecture at a glance

```
client library (shard-aware, caches tablet map)
        |  typed binary protocol, one TCP connection per shard it talks to
        v
+------------------------- node (one process) --------------------------+
|  shard 0 (core 0)      shard 1 (core 1)     ...     shard N-1         |
|  +----------------+    +----------------+                             |
|  | event loop     |    | event loop     |   each shard owns:          |
|  | VSR replica    |    | VSR replica    |   - one port                |
|  | state machine  |    | state machine  |   - one data file           |
|  | LSM forest     |    | LSM forest     |   - one WAL                 |
|  | data file      |    | data file      |   - its memory pools        |
|  +----------------+    +----------------+                             |
|        scan workers (optional, read-only, immutable parts)            |
+-----------------------------------------------------------------------+

replication group = the same shard index role on RF different nodes
metadata group    = small VSR group: schema, topology, tablet map
```

Life of a single-shard write:
1. Client hashes the partition key to a token, finds the tablet and its group in the cached map, sends a batch to the group primary's port.
2. Primary assigns an op number and timestamp, appends the prepare to its WAL, replicates it.
3. On a replication quorum the primary commits, executes the batch in the state machine, replies. Backups execute when they learn the commit.
4. Memtable flushes and compaction run at fixed op-number intervals, identically on every replica.

Life of a cross-shard write: section 4 of [docs/04-cluster-and-cross-shard.md](docs/04-cluster-and-cross-shard.md).

## 4. Guarantees

| Operation | Guarantee |
|---|---|
| Single-shard transaction | Strict serializable (linearizable log, serial execution) |
| Cross-shard transaction | Serializable in global timestamp order; real-time order holds for any two transactions that share a shard or are causally linked through a client |
| Linearizable read | Served by the group primary through the log position |
| Snapshot read at T | Consistent cut across all shards once T is below the cluster safe timestamp |
| Durability | An acknowledged op survives the loss of any minority of a group, and any single-replica disk corruption |

**Correction to draft 1.** I previously described cross-shard transactions as strict serializable. The Granola approach on its own gives serializability: two transactions on unrelated shards can receive timestamps that disagree with their real-time order by up to the clock offset between groups, and a third concurrent transaction can observe that. Two mechanisms narrow this, both in the plan:
- Clients carry the highest timestamp they have seen on every request, and shards advance their clocks past it (hybrid logical clock). This restores real-time order for anything a client could have observed through its own session.
- An optional commit-wait of the measured maximum clock offset before replying gives full strict serializability at a latency cost of a few milliseconds. It can be a per-transaction flag.

## 5. What we take from each system

See draft 1 reasoning, condensed:

| System | Take | Leave |
|---|---|---|
| TigerBeetle | VSR, hash-chained WAL, storage determinism, simulation testing, storage fault model, batching, static limits, client sessions, linked events, pending/post/void | Fixed schema, single replication group |
| TiDB | Metadata group (placement driver), split/merge/move of ranges, type vocabulary | MySQL semantics, interactive transactions, central timestamp oracle, separate row and column stores |
| ClickHouse | Sorted immutable parts, sparse index, granules, codecs, skip indexes, vectorised execution, type vocabulary | Eventual deduplication, asynchronous mutations |
| ScyllaDB | Shard-per-core, shard-aware clients, tablets, scheduling classes, LWT semantics | Paxos LWT implementation, eventual consistency |

Scylla LWT semantics need no separate mechanism: a conditional write is a single-shard transaction with a precondition, one consensus round.

## 6. Glossary

- **Shard:** one core, one thread, one data file, one VSR replica.
- **Group:** RF shards on different nodes forming one VSR cluster.
- **Tablet:** contiguous token range of a table group, owned by one group.
- **Table group:** tables sharing a partition key definition, always placed together.
- **Op:** one entry in a group's log; carries a batch of events.
- **Part:** immutable sorted columnar run on disk.
- **Granule:** fixed number of consecutive rows in a part; the unit of indexing.
- **Safe timestamp:** a timestamp below which no shard will commit anything new.

## 7. Open questions for refinement

The ten questions from draft 2 were answered on 2026-11-17 and are recorded in section 2. Consequences and what remains:

1. **Message size.** With rows of about 1 KiB, a 1 MiB message carries about 1,000 rows, against roughly 8,000 for a narrow ledger row. Phase 1 benchmarks 1 MiB against 4 MiB messages (task T1.5.5) before the constant is frozen.
2. **Retention before segments.** Tables can only be keep-forever until time segments land in phase 7. Drop-after and archive-after are declared in the schema from the start but rejected until then.
3. **Sharding and tablet movement** remain proposed; confirm or challenge during the document review.
4. Each subsystem document ends with its own "to decide" list; those are the next refinement pass.

## Sources

- Granola: [USENIX ATC 2012](https://www.usenix.org/conference/atc12/technical-sessions/presentation/cowling), [paper PDF](https://news.cs.nyu.edu/~jinyang/ds-reading/granola.pdf), [thesis](https://dspace.mit.edu/handle/1721.1/75706), [summary](https://muratbuffalo.blogspot.com/2014/11/granola-low-overhead-distributed.html)
- TigerBeetle: [architecture](https://docs.tigerbeetle.com/about/architecture), [safety](https://docs.tigerbeetle.com/concepts/safety/), [data file](https://docs.tigerbeetle.com/about/internals/data_file), [protocol-aware DST](https://tigerbeetle.com/blog/2026-08-20-protocol-aware-dst)
- ScyllaDB: [Raft strong consistency](https://www.scylladb.com/2026/06/24/raft-strong-consistency/), [2026.2](https://www.scylladb.com/2026/06/29/scylladb-2026-2/), [tablets](https://www.scylladb.com/2023/07/10/why-scylladb-is-moving-to-a-new-replication-algorithm-tablets)
- Rust runtimes: [Apache Iggy thread-per-core migration](https://iggy.apache.org/blogs/2026/02/27/thread-per-core-io_uring/)
- Simulation testing: [S2](https://s2.dev/blog/dst), [Polar Signals](https://blog.polarsignals.com/blog/posts/2025/07/08/dst-rust), [RisingWave](https://risingwave.com/blog/deterministic-simulation-a-new-era-of-distributed-system-testing/)
- Hybrid storage: [ClickHouse on OLTP and OLAP](https://clickhouse.com/resources/engineering/unifying-oltp-and-olap), [Tinybird on SingleStore](https://www.tinybird.co/blog/clickhouse-vs-singlestore-olap-vs-htap)

The design documents are written from my knowledge of the VSR Revisited, Granola, protocol-aware recovery and MergeTree designs. Protocol details must be checked against the primary papers before implementation; each document lists what to verify.
