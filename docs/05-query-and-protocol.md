# 05. Wire protocol, client library and query execution

## 1. Wire protocol

One protocol for client-to-shard and shard-to-shard traffic. TCP, one connection per (peer, shard). Messages are a fixed 256-byte header followed by a body, total at most 1 MiB, sector-aligned in memory so the same buffer goes to the WAL without a copy.

### Header (256 bytes, little-endian, fixed layout)

| Field | Type | Notes |
|---|---|---|
| checksum | u128 | Over the rest of the header |
| checksum_body | u128 | Over the body |
| parent | u128 | Checksum of the previous prepare (hash chain); zero elsewhere |
| cluster | u128 | Cluster id; mismatches are dropped |
| client | u128 | Client session id, or zero |
| txn | u128 | Cross-shard transaction id, or zero |
| group | u32 | Target group |
| tablet_epoch | u32 | Sender's view of the tablet map |
| schema_version | u32 | Sender's schema version |
| view | u32 | |
| op | u64 | |
| commit | u64 | |
| timestamp | u64 | Prepare timestamp, or sender's highest seen timestamp |
| request | u32 | Session request number |
| size | u32 | Header plus body |
| replica | u8 | Sender replica index |
| command | u8 | Protocol message kind (document 02 section 2, document 04) |
| operation | u8 | State machine operation |
| version | u8 | Protocol version |
| reserved | bytes | Must be zero |

### Bodies

- **Row batches (writes, gets):** each table has a generated fixed-size row struct for its fixed-width columns, followed by (offset, length) slots for variable-length columns pointing into a per-batch heap. A batch is `[count][rows][heap]`. Zero-copy readable; validated once on arrival (lengths, UTF-8, enum values, reserved bits).
- **Results for writes:** sparse list of `(event index, result code)`; an empty list means every event succeeded.
- **Column batches (scan and query results):** per column: validity bitmap, values or offsets plus bytes. Layout is Arrow-compatible so the client can expose results as Arrow arrays without copying. Large results stream as several messages with a continuation token.
- **Programs and plans:** a compact, versioned binary IR. Text syntax exists only in client tooling.

Everything is validated at the boundary: a malformed message is rejected whole with a protocol error and never reaches the state machine.

### Security

Confirmed for the first replicated release: mutual TLS for all connections using rustls, with certificates identifying nodes and clients; authorisation by client certificate to table-group level. TLS runs in userspace on the shard thread; kernel TLS is an optimisation to evaluate later.

## 2. Client library

Rust first, then Go, then Python. Go and Python bind to a C ABI over the Rust core, as TigerBeetle's clients do.

Responsibilities:
- Fetch and cache the cluster map (groups, primaries, tablet map, schema) from any node; refresh on `stale_map` or view change hints.
- Compute tokens and route each event to its tablet's group primary. Split a user batch by destination and reassemble results in order.
- Manage sessions: register, number requests, retry with the same request number, re-register on eviction.
- Batch: coalesce concurrent calls into one request per (group, operation) up to the message size, with a small linger.
- Generate time-ordered `Id128` values.
- Track the highest timestamp seen and send it on every request.
- For cross-shard programs: compute participants, send to all, collect replies, apply backoff on `conflict` and `busy`.
- Code generation: from the schema, emit typed row structs, operation builders and program invocation stubs. Type errors become compile errors in the application.

API shape (illustrative):

```rust
let client = Client::connect(cluster_id, &addresses).await?;
let results = client.insert(&[transfer_a, transfer_b]).await?;      // typed rows
let row     = client.get::<Account>(account_id).await?;
let out     = client.execute(Transfer { from, to, amount, id }).await?;
let batches = client.query(plan).snapshot(Snapshot::Safe).await?;
```

## 3. Read paths

| Read | Routed to | Consistency | Goes through log |
|---|---|---|---|
| get, scan (default) | group primary | linearizable | no; primary confirms leadership with a quorum first |
| get, scan at snapshot T | any replica of the group | snapshot | no |
| query, single partition | any replica of the group | snapshot | no |
| query, multi-shard | coordinator, then fan-out | snapshot at cluster safe timestamp | no |

Reads inside programs are part of the transaction and need nothing extra.

## 4. Analytical queries

### Division of labour (revised 2026-11-18)

The analytical need is ad hoc exploration, served through a read-only SQL gateway. That changes what the shards must do: they no longer need a general query engine, only a fast, bounded **scan fragment**. Everything else runs in the gateway, outside the database's failure and determinism boundary.

| Layer | Does | Does not |
|---|---|---|
| Shard | Scan with pruning, filter, project, partial aggregate, top-N, limit; all with static memory bounds | Joins, sorts of unbounded size, subqueries, SQL |
| Gateway | SQL parsing, planning, optimisation, joins, final aggregation, sorting, window functions, spilling to disk | Writes, transactions, anything that affects shard state |

### Scan fragment IR

A fragment is a typed request built by the client library or the gateway:

- `Scan(table, columns, key ranges, time range, predicate)`
- `Project(expressions)`
- `PartialAggregate(group keys, aggregates)` with sum, count, min, max, and mergeable deterministic sketches for distinct counts and quantiles
- `TopN(order, n)`, `Limit(n)`

Every fragment has a static memory bound; a partial aggregate whose group table exceeds it flushes partial groups downstream instead of growing. The typed client can send fragments directly, so programmatic queries do not need the gateway.

### SQL gateway

- A separate, stateless service. Any number can run; they hold no data and can crash without consequence.
- Built on an existing Rust query engine (Apache DataFusion is the candidate; confirm its state and pushdown interfaces at the start of phase 6). The database appears to it as a table provider that supports projection, filter, limit and partial-aggregate pushdown, and returns Arrow batches.
- Read-only. It accepts `SELECT` only and connects with a read-only credential. Writes stay on the typed protocol.
- Every query runs at one snapshot timestamp across all shards (document 04 section 5).
- It routes fragments to backups and scan workers first, and applies its own admission control, per-query memory limit and timeouts. A runaway ad hoc query can exhaust the gateway, not a shard.
- Wire protocol towards users: Arrow Flight SQL is the candidate, which gives JDBC, ODBC and Python access without us defining a SQL wire protocol.

This is a narrower promise than SQL compatibility: one SQL dialect, reads only, in a separate process.

### Execution

- Vectorised, pull-based, over column batches of up to 4096 rows (aligned with granules).
- Scans prune parts by key and timestamp range, granules by skip indexes, and decode only needed columns. Predicates are evaluated on encoded data where the codec allows (dictionary, frame-of-reference, RLE).
- Versioned tables: the scan resolves the visible version per key at the snapshot timestamp during the merge.
- SIMD through portable kernels with a fixed evaluation order for floats, so results are identical across machines.

### Distribution

- The gateway (or the typed client) is the coordinator: it picks the snapshot timestamp, sends the fragment to one replica of each relevant group (preferring backups, to keep primaries free), and merges partial aggregates or sorted streams. Shards never coordinate queries for each other.
- Fragments whose key range lies in one partition go to that partition's group only.
- Joins run in the gateway over the streams it receives. Pushing a join's small side down to shards as a filter is a later optimisation.

### Isolation from OLTP

Analytical work must not delay commits. Mechanisms:
1. **Scheduling classes** on the shard thread with strict priority and time budgets per loop iteration: commit path, foreground reads, repair, compaction, queries, connection handshakes. Each class runs in priority order while it has budget left; an overrun is repaid from its next budgets, and a class with nothing ready gives its unused budget to the ready classes in priority order. So a class that stays ready always gets at least its budget's share of the time, whatever runs above it (`granite_io::event_loop`). A query operator yields after a fixed number of batches.
2. **Scan workers:** a node's cores are split by configuration between shards and scan workers (for example 28 and 4 of 32). Parts are immutable and the grid is read-only below the checkpoint, so read-only worker threads on cores reserved for analytics can execute fragments against a pinned manifest snapshot. The shard thread only hands out the snapshot and the memtable delta. This is a deliberate, contained exception to one-thread-per-shard; it never touches mutable state.
3. **Backup preference:** fragments go to backups and standbys first.
4. **Admission control:** a bounded number of concurrent queries per shard, each with its declared memory bound reserved up front.

## 5. Change feed (moved early, 2026-11-18)

Other exchange services (risk, market data, notifications) need committed changes as they happen, so the change feed is built right after transactions, in phase 3.

- **Unit:** a subscription follows one group and receives committed row changes in commit order: `(group, op, index, timestamp, table, kind, row)`, plus the cross-shard transaction id where there is one. Filters by table and by column list are applied on the server.
- **Cursor:** `(group, op, index)`, held by the consumer. Delivery is at-least-once; the cursor makes consumers idempotent. Server-stored named cursors are a later convenience.
- **Source:** recent ops come from the WAL. Older history is read from parts by timestamp, which works because every row carries its commit timestamp and deletes are tombstones. History is available as far back as the table's retention.
- **Served by backups and standbys**, never by the primary, so subscribers cannot slow commits. A subscriber that falls behind is switched from the WAL to parts rather than buffered in memory.
- **Across groups (phase 5):** the client library merges per-group feeds by timestamp, releasing an event only once every group's feed has passed that timestamp, so a consumer sees one globally ordered stream in which the legs of a cross-shard transaction appear together.
- **Derived tables** emit changes like any versioned table, so a risk service can follow balances directly.

## 5a. Connections

- Clients keep warm, authenticated connections to every replica of each group they use, not only the primary. After a failover they redirect traffic without a wave of TLS handshakes landing on the new primary's thread. TLS session resumption is enabled as a second line of defence.
- New-connection handshakes run in the lowest scheduling class with a per-iteration cap.
- With thousands of clients and about 64 groups, each shard holds a few thousand connections; idle ones cost only memory.

## 6. To decide in refinement

- Linger and batch defaults in the client.
- Whether linearizable reads bypass the log in the first release.
- The exact set of aggregate sketches (HyperLogLog, t-digest or DDSketch), which must be deterministic and mergeable.
- Client languages and their order.
