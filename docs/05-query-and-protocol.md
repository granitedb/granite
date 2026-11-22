# 05. Wire protocol, client library and query execution

## 1. Wire protocol

One protocol for client-to-shard and shard-to-shard traffic. TCP, one connection per (peer, shard). Messages are a fixed 256-byte header followed by a body, total at most 1 MiB, sector-aligned in memory so the same buffer goes to the WAL without a copy.

### Header (256 bytes, little-endian, fixed layout)

| Field | Offset | Type | Notes |
|---|---|---|---|
| checksum | 0 | u128 | Over the rest of the header |
| checksum_body | 16 | u128 | Over the body |
| parent | 32 | u128 | Checksum of the previous prepare (hash chain); zero elsewhere |
| cluster | 48 | u128 | Cluster id; mismatches are dropped |
| client | 64 | u128 | Client session id, or zero |
| txn | 80 | u128 | Cross-shard transaction id, or zero |
| group | 96 | u32 | Target group |
| tablet_epoch | 100 | u32 | Sender's view of the tablet map |
| schema_version | 104 | u32 | Sender's schema version |
| view | 108 | u32 | |
| op | 112 | u64 | |
| commit | 120 | u64 | |
| timestamp | 128 | u64 | Prepare timestamp, or sender's highest seen timestamp |
| request | 136 | u32 | Session request number |
| size | 140 | u32 | Header plus body |
| replica | 144 | u8 | Sender replica index |
| command | 145 | u8 | Protocol message kind (document 02 section 2, document 04); codes below |
| operation | 146 | u8 | State machine operation (document 03 section 4); codes below |
| version | 147 | u8 | Protocol version, currently 1 |
| reserved | 148 | [u8; 108] | Must be zero |

The header is `granite_base::header::Header`. It lives in `crates/base`, not `crates/protocol`, because WAL prepares in `crates/storage` start with it too. Compile-time assertions fix its size (`FORMAT.header_size`) and every offset above; a test compares them with this table.

Command codes. Code 0 is not a command, so an all-zero header is rejected. Codes 1 to 20 are the messages of document 02 section 2 in table order, 21 and 22 the cross-shard messages of document 04 section 4:

| Code | Command |
|---|---|
| 1 | request |
| 2 | prepare |
| 3 | prepare_ok |
| 4 | commit |
| 5 | reply |
| 6 | start_view_change |
| 7 | do_view_change |
| 8 | start_view |
| 9 | request_start_view |
| 10 | request_headers |
| 11 | headers |
| 12 | request_prepare |
| 13 | nack_prepare |
| 14 | request_block |
| 15 | block |
| 16 | ping |
| 17 | pong |
| 18 | request_sync_checkpoint |
| 19 | sync_checkpoint |
| 20 | eviction |
| 21 | propose |
| 22 | vote |

Operation codes. Codes 1 to 8 are the operations of document 03 section 4 in table order:

| Code | Operation |
|---|---|
| 0 | none (a message that carries no operation) |
| 1 | insert |
| 2 | upsert |
| 3 | update |
| 4 | delete |
| 5 | get |
| 6 | scan |
| 7 | execute |
| 8 | query |

A received header is rejected, each reason with its own error, when: it is shorter than 256 bytes, its checksum does not match, the version is not 1, the command or operation code is unknown, a reserved byte is not zero, or `size` is below 256 or above the message limit. The body is then rejected if its length is not `size - 256` or its checksum does not match. Later cards append codes to these tables; each addition, like any change to these rules, is a protocol change and raises the version.

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
- `PartialAggregate(group keys, aggregates)` with sum, count, min, max, and mergeable deterministic sketches for distinct counts and quantiles (the sketches come with T6.2.5)
- `TopN(order, n)`, `Limit(n)`

Every fragment has a static memory bound, which the validator computes before it runs; a fragment without one, or whose bound is over the limit, is rejected. A partial aggregate declares the size of its group table as part of that bound: when the table is full, it flushes partial groups downstream instead of growing. So "an aggregate without a group-table bound" means one that declares no size, and is rejected; an aggregate that meets more groups than it declared still runs within its bound and returns partial groups that the coordinator merges. The typed client can send fragments directly, so programmatic queries do not need the gateway.

#### Elements (fragment IR version 1)

A fragment (`granite_query::Fragment`) is a scan followed by up to 255 operators, each reading the rows of the one before it. The snapshot timestamp is not part of the fragment.

- **Scan** reads the rows of one table, each as the values of its column list, in that order. The list names columns by index and must not be empty. It keeps the rows whose primary key is within any of the key ranges (all rows when there is none) and whose commit timestamp is within the time range, both ends included; a time range whose lower end is above its upper end selects nothing. For a versioned table, that is the version visible at the snapshot. A key range has a lower and an upper bound. Each bound is absent, inclusive or exclusive, and gives the first one or more key values, each of its key column's type. Keys are compared in key order (document 03 section 2.1) on as many values as the bound gives. The predicate, when present, is a bool over the scan's columns, and only the rows where it holds are kept.
- **Project** turns each row into the values of its expressions.
- **PartialAggregate** groups rows by the values of its keys and turns each group into its keys followed by its aggregates: `count` (the rows, a u64), `sum`, `min` and `max` of an expression. It holds at most `groups` groups (above zero), and when the table is full it flushes them downstream, so one group may appear in several output rows. That is why no operator may follow it (`AfterAggregate`).
- **TopN** keeps the first n rows (n above zero) in the order of its keys, each ascending or descending. How ties are broken is T6.2.3's.
- **Limit** keeps the first n rows.

Expressions are those of the program IR (document 03 section 5.1) that need nothing but the row: literal, `-`, `not`, `is_null`, `unwrap_or`, cast and the binary operators, with the same opcodes, operands and type rules (document 03 section 5.2). One element is added, `column`, the value of a column of the input row. The program IR's parameter, local, field, `get`, `commit_ts()` and `match` are not fragment elements. There is no element for a join, a subquery, a sort of unbounded size or a write: tables are named only by the scan, and the only sort is a top-N, whose n bounds it.

#### Encoding (part of the protocol)

Integers are little-endian. The decoder reads at most one message and returns the tree or a typed error. Whether the tree is a valid fragment is the validator's decision. The encoding is canonical: a tree has exactly one encoding, and bytes the decoder accepts are exactly the encoding of the tree it returns. A change to any element needs a new version.

```
fragment   u16 version (1), scan, u8 operator count, operators
scan       name (table), u16 column count, per column: u16 index,
           u16 range count, per range: bound (lower), bound (upper),
           u64 time lower, u64 time upper,
           u8: 0 no predicate, 1 then expr
bound      u8: 0 none, 1 inclusive, 2 exclusive; unless none, u16 value count, per value: type, value
exprs      u16 count, expressions
name       u8 length, UTF-8 bytes
type       as in document 03 section 1.2, a valid type
value      a literal's value, as in document 03 section 5.1
```

| Opcode | Operator | Operands |
|---|---|---|
| 0 | `Project` | exprs |
| 1 | `PartialAggregate` | exprs (keys), u16 aggregate count, aggregates, u32 groups (0: none) |
| 2 | `TopN` | u16 key count, per key: u8 order (0 ascending, 1 descending), expr; u32 n (0: none) |
| 3 | `Limit` | u64 n |

| Opcode | Aggregate | Operands |
|---|---|---|
| 0 | `count` | |
| 1 | `sum` | expr |
| 2 | `min` | expr |
| 3 | `max` | expr |

| Opcode | Expression | Operands |
|---|---|---|
| 0, 6 to 9, 11, 12 to 24 | literal, `-`, `not`, `is_null`, `unwrap_or`, cast, binary | as in document 03 section 5.1 |
| 25 | `column` | u16 index |

Every other opcode is rejected with `UnknownOpcode`; opcodes 4 and up of aggregates are reserved for the sketches. Zero for `groups` or `n` encodes a missing bound, which only the validator rejects, so that each kind of unbounded fragment gets its own code.

Limits, each tested at its value and one over. Key ranges, key values, expressions, operators and aggregates are the fragment's elements. An expression that is an operand of the scan, an operator or an aggregate is at depth 1, and each nested expression is one deeper. A fragment shares these limits with programs, from document 03 section 8, so both IRs have the same decoder bounds: the encoded fragment is at most one message, nesting at most `program_depth_max`, the element count at most `program_nodes_max`, and a name at most 255 bytes. As for programs, the decoder's memory is proportional to its input and its largest single allocation is below 2 MiB.

Example: a scan of `trade` (columns 0 and 3) from key `[7]` on, where column 1 > 0, then a partial aggregate by column 0 of `count` and `sum(column 1)` into at most 1,000 groups, encodes as follows (test `golden_fragment`):

```
01 00  05 74 72 61 64 65  02 00 00 00 03 00              version 1, "trade", columns 0 and 3
01 00  01 01 00 03 07 00 00 00 00 00 00 00  00           1 range: [u64 7, unbounded)
00 00 00 00 00 00 00 00  ff ff ff ff ff ff ff ff         every commit timestamp
01  15 19 01 00 00 03 00 00 00 00 00 00 00 00            predicate column 1 > u64 0
01  01 01 00 19 00 00  02 00 00 01 19 01 00  e8 03 00 00 1 operator: aggregate, 1,000 groups
```

#### Validation and memory bounds

`Fragment::validate` checks a fragment against the catalog and computes its memory bound. The first broken rule rejects it with its code and its element, numbered from 1 in the order of the encoding. Element 0 is the fragment as a whole, with its scan's table, columns and time range. A tree that `Fragment::encode` rejects is rejected with that error at element 0.

Types are those of document 03 section 5.2: a column has its logical type, LowCardinality removed. A group key, a top-N key and the operands of a comparison are neither nullable nor floats. `sum` takes an integer, decimal or float and gives its type, and `min` and `max` give their operand's type. Overflow is a typed error at run time (T6.2.3).

The bound of a fragment is the sum of the bounds of its scan and its operators. B is the rows of a batch (`batch_rows_max`). w(T) is the most bytes a value of T takes in a row (document 03 section 2.4): its storage width, or 8 plus `max` for String and Bytes, plus 1 for Nullable. row(columns) is the sum of w over the columns, and E is the sum of w over every element of an operator's expressions, which is every value they compute for one row. Each operator holds one batch of its output and one batch of every value its expressions compute, B × (row(output) + E), plus its state:

| Operator | State |
|---|---|
| `Scan` | none |
| `Project` | none |
| `PartialAggregate` | groups × (row(output) + 8): the group table, each entry its keys and aggregates and 8 bytes of hash index |
| `TopN` | n × (row(input) + row(keys)): n rows, each with its order key |
| `Limit` | none |

The bound counts what the operators hold. Blocks read from parts go through the block cache, and the decoded tree is proportional to its encoding. The validator rejects a fragment whose bound is over the fragment memory limit of 512 MiB (`fragment_memory_max`). Admission control reserves the bound for each fragment when it starts, so the limit is a per-process memory reservation and belongs to the process configuration, beside `batch_rows_max`, the B it is computed with. It need not be equal on every replica. A fragment only reads: accepting or rejecting it changes no replicated state, and a coordinator whose fragment one replica rejects can retry on another. The value: one batch of a table whose rows are at the 64 KiB row limit is 4096 rows × 64 KiB = 256 MiB, so a scan of any table fits, with the same again for expression results and operator state. The configuration check asserts that one batch of the widest rows fits in half the limit. All sums saturate, so a bound past 2^64 - 1 is rejected and never wraps.

| Code | Name | Rule |
|---|---|---|
| 1 | `Malformed` | Truncated, trailing bytes, a tag out of range, invalid UTF-8 or an invalid type; or, when encoding, a name over 255 bytes or more than 255 operators. |
| 2 | `UnsupportedVersion` | A fragment IR version this binary does not read. |
| 3 | `UnknownOpcode` | An operator, aggregate or expression opcode not in the tables above. |
| 4 | `TooDeep` | Nesting deeper than `program_depth_max`. |
| 5 | `TooManyNodes` | More elements than `program_nodes_max`. |
| 6 | `TooLarge` | An encoding longer than one message. |
| 7 | `InvalidLiteral` | A literal that is not a valid value of its type, or not in its canonical form. |
| 8 | `UnknownTable` | The scanned table is in the catalog. |
| 9 | `UnknownColumn` | Every column index names a column of the table, or of the operator's input. |
| 10 | `TypeMismatch` | Every value has the type its place needs, and every operator and aggregate gets a type it takes. |
| 11 | `ImplicitCast` | Two different numeric types never meet without an explicit cast. |
| 12 | `NullableOperand` | No nullable value is used where a value is needed. |
| 13 | `WrongArity` | A key bound gives from one value to as many as the primary key has columns. |
| 14 | `NoColumns` | A scan has a column list. |
| 15 | `NoGroupBound` | A partial aggregate has a group-table bound. |
| 16 | `NoTopN` | A top-N has an n. |
| 17 | `AfterAggregate` | No operator follows a partial aggregate. |
| 18 | `OverMemoryLimit` | The fragment's memory bound is at most `fragment_memory_max`. |

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
1. **Scheduling classes** on the shard thread with strict priority and time budgets per loop iteration: commit path, foreground reads, repair, compaction, queries, connection handshakes. Each class runs in priority order while it has budget left; an overrun is repaid from its next budgets, and a class with nothing ready gives its unused budget to the ready classes in priority order. So a class that stays ready always gets at least its budget's share of the time, whatever runs above it (`granite_io::event_loop`). No task may run longer than 1 ms without yielding (document 02 section 1): long-running work asks a `Slice` when to return, and the loop records every iteration's and every task's time per class in its `Metrics`, counts each task over 1 ms; under the simulator, on virtual time, such a task also fails an assertion. A query operator yields after a fixed number of batches.
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
