# Phase 3. Transactions

Phase exit: versioned tables, conditional writes, linked events, programs, derived tables and local indexes, all single-shard and replicated; gate G2 answered. This is the first externally useful release.

Design reference for the whole phase: `docs/03-data-model-and-transactions.md`. Every gate that says "replicated" means the behaviour is checked on a 3-replica group in the simulator with all faults, with the storage convergence checker active.

## T3.1.1 Versioned sort key, tombstones, snapshot resolution in reads

**Goal.** Tables whose rows can change, stored without overwriting anything: every change is a new version, and a read sees the newest version at its snapshot.

**Work.**
- Sort key `(token, primary key, _ts descending)` for versioned tables.
- The hidden `_tombstone` column for deletes.
- Read resolution: the first version with `_ts` at or below the snapshot; a tombstone means absent.
- Point reads and merge iterators both resolve versions.
- The row cache holds latest versions only.

**Not in this card.** The write operations (T3.1.3), dropping old versions (T3.1.2).

**Depends on.** T1.3.5, T1.2.5, T1.4.1.

**Gates.**
- [ ] Model test: for random histories of writes and deletes, a read at any snapshot timestamp (including before the first version and between any two versions) returns the reference model's answer.
- [ ] A scan at snapshot T returns exactly one row per live key, in key order, and never a tombstoned key (model test).
- [ ] A read at a snapshot older than a key's first version returns absent.
- [ ] The newest version of a key sorts first within the key (property test on the encoding).
- [ ] Version resolution holds across flushes and compactions that happen between the writes (model test).
- [ ] A read of the latest version of a key with 10,000 versions performs the same number of device reads as a key with one version (counter).
- [ ] Reads at snapshot do not consult the row cache for versions older than the cached one (test).

## T3.1.2 Retention horizon in compaction

**Goal.** Old versions are removed on a rule that every replica applies identically, so storage stays bounded and byte-identical.

**Work.**
- A per-table retention setting: a duration or forever.
- The horizon derived from commit timestamps carried in ops, never from a wall clock.
- During compaction, a version is dropped when it is older than the horizon and a newer version of the same key exists at or below the horizon. A tombstone is dropped once everything older is gone.
- The horizon recorded so a snapshot read below it is rejected with a typed error.

**Not in this card.** Dropping whole time segments (T7.2.5).

**Depends on.** T3.1.1, T1.2.4.

**Gates.**
- [ ] Model test: after any amount of compaction, every read at any snapshot at or above the horizon equals the reference model.
- [ ] A read at a snapshot below the horizon returns the typed error `snapshot_too_old`, never a wrong or partial answer.
- [ ] The newest version at or below the horizon is never dropped while it is the visible one (assertion and model test).
- [ ] Two instances fed the same ops have byte-identical data files at every checkpoint with retention enabled (100 workloads), and the same holds for replicas under all faults (convergence checker, seed gate 10,000).
- [ ] With `retention = forever`, no version is ever dropped (test).
- [ ] The horizon advances only as a function of op timestamps (lint; and a test with replica clocks set at random).
- [ ] Space used converges to a stated bound for a workload that overwrites a fixed key set forever (10,000,000 ops).

## T3.1.3 Operations: update, delete, upsert

**Goal.** The write operations for versioned tables.

**Work.**
- `update`: read the current version, write a complete new version; fails if the key is absent.
- `delete`: write a tombstone; fails if the key is absent.
- `upsert`: write a new version whether or not the key exists; skips the uniqueness read.
- These operations are rejected on immutable tables.
- Per-event result codes, sparse results.

**Not in this card.** Conditions (T3.2.1).

**Depends on.** T3.1.1, T1.5.1.

**Gates.**
- [ ] Model test, replicated: results and table content equal the reference model for random mixes of insert, update, delete and upsert.
- [ ] Update and delete of an absent key return `not_found` and write nothing.
- [ ] Any of the three operations on an immutable table returns `table_immutable` for every event and writes nothing.
- [ ] An update writes a full row: every column of the new version is present even when the request changes one (test reading the new version alone).
- [ ] An upsert performs no point read for the uniqueness check (counter).
- [ ] Two writes to the same key in one batch produce two versions with increasing timestamps, and the later one is visible.
- [ ] Insert after delete of the same key succeeds and creates a new version.
- [ ] All result codes are in the closed enum documented in document 03.

## T3.1.4 Snapshot reads on backups

**Goal.** Reads that tolerate a slightly old view can be served by any replica, taking load off the primary.

**Work.**
- A read carries a snapshot timestamp T.
- A replica serves it once its executed commit timestamp is at or beyond T, and otherwise waits up to a timeout or redirects.
- The snapshot stability checker: a snapshot read repeated later returns the same result.

**Not in this card.** Multi-shard snapshots (T5.3.2).

**Depends on.** T3.1.1, T2.2.11.

**Gates.**
- [ ] Snapshot stability checker passes over seed gate 100,000 with all faults: every snapshot read, repeated at the end of the run on every replica, returns an identical result.
- [ ] A backup never serves a snapshot read at T before it has executed every op with timestamp at or below T (assertion; injected bug that removes the wait is caught within 10,000 seeds).
- [ ] The same read at the same T returns identical bytes from the primary, each backup and a standby (test).
- [ ] A read at a T the replica cannot reach within the timeout returns a typed retryable error.
- [ ] Snapshot reads perform no WAL write and do not change storage state (data-file hash unchanged).

## T3.2.1 Per-event preconditions (not_exists, exists, version, column compare)

**Goal.** Conditional writes: the lightweight-transaction feature, evaluated inside the shard with no extra round trip.

**Work.**
- A precondition on any write event: `not_exists`, `exists`, `version = ts`, or `column op constant` with the six comparison operators.
- Evaluation against the current state including earlier events of the same op.
- Result code `condition_failed` with which condition failed.
- Type checking of the constant against the column, with no implicit cast.
- User documentation presenting this as the lightweight-transaction feature.

**Not in this card.** Conditions spanning several rows (programs, T3.3.3).

**Depends on.** T3.1.3.

**Gates.**
- [ ] Model test, replicated: for random conditional writes, results and state equal the reference model.
- [ ] Each of the four condition kinds has tests for the true case, the false case and the absent-row case.
- [ ] A condition on a nullable column that is null fails the event with a typed code; it is never treated as true or false silently.
- [ ] A constant of a different type than the column is rejected at validation, before execution (test for each type pair that looks compatible).
- [ ] A failed condition writes nothing and does not affect other events in the batch.
- [ ] `version = ts` succeeds only when the latest version's timestamp equals `ts` exactly, which gives compare-and-set (test with two competing writers: exactly one wins).
- [ ] The feature has a documentation page with a worked compare-and-set example that runs in CI.

## T3.2.2 Linked event chains with rollback

**Goal.** Several events that succeed or fail together, without writing a program.

**Work.**
- A `linked` flag on an event; a chain is a run of linked events ending with an unlinked one.
- A per-chain write buffer; any failure discards the whole chain.
- Result codes: the failing event's own code, and `linked_event_failed` for the others in the chain.
- Later events in a chain see earlier writes of the same chain.
- An open chain at the end of a batch is an error.

**Not in this card.** Chains across batches or across shards.

**Depends on.** T3.2.1.

**Gates.**
- [ ] Model test, replicated: results and state equal the reference model for random batches of chains and single events.
- [ ] A failure at any position of a chain (first, middle, last) leaves no trace: the data file hash, ignoring the WAL, equals the hash without the chain (test).
- [ ] After a failed chain, derived-table state, indexes and the row cache are all unchanged (test once those exist; the gate is re-run in T3.4.1 and T3.4.3).
- [ ] Events outside the chain in the same batch are unaffected.
- [ ] A batch ending with an open chain returns `linked_event_chain_open` for the chain and applies none of it.
- [ ] An event in a chain reads the earlier writes of the chain (test).
- [ ] A chain of the maximum length that fits a message executes within the 1 ms slice rule.

## T3.3.1 IR definition and binary encoding

**Goal.** The fixed, versioned form in which a transaction program travels and is stored.

**Work.**
- The IR elements of document 03 section 5: get, scan with constant limit, let, expressions, if/else, require, insert, update, delete, upsert, for-loop with constant bound, return.
- `commit_ts()`, `is_null`, `unwrap_or`, `match` on nullable values.
- A compact binary encoding with a version.
- A decoder that produces a typed tree or a typed error.

**Not in this card.** Validation rules (T3.3.2), execution (T3.3.3), `audit` (T5.2.11).

**Depends on.** T1.4.2, T1.4.3.

**Gates.**
- [ ] The IR and its encoding are specified in document 03, element by element, with a version number.
- [ ] Fuzz gate 8 on the decoder: no panic, no unbounded recursion, no allocation larger than a stated bound for any input up to the message size.
- [ ] Round trip: encode then decode returns an identical tree for 1,000,000 random valid programs.
- [ ] Encoding is canonical: one program has exactly one encoding, and a non-canonical encoding is rejected (property test).
- [ ] Nesting depth and node count have limits, tested at the limit and one over.
- [ ] The IR has no element for time, randomness, external calls, dynamic table names, unbounded loops or recursion; a test asserts that no opcode value decodes to such a thing.
- [ ] A program with a newer IR version than the binary supports is rejected with a typed error.

## T3.3.2 Validator: types, bounds, key provenance, shard set

**Goal.** Everything that can be known about a program before it runs is checked once, at registration, so execution cannot be surprised.

**Work.**
- Type checking with no implicit casts and no three-valued null logic.
- The maximum number of reads and writes per invocation, computed from the constant bounds, against the limit of 256.
- Key provenance: each read and write classified as static (key from parameters only) or dependent (key from an earlier read).
- Shard set: the partition-key expression of every access.
- A specific error code for each class of invalid program.

**Not in this card.** Classification as independent or coordinated (T5.2.6). Rejecting a write to a derived table, as derived tables do not exist yet (T3.4.1).

**Depends on.** T3.3.1.

**Gates.**
- [ ] Each class of invalid program has at least one test rejected with its own code: type mismatch, implicit cast, null used in arithmetic, bound over 256, unknown table or column, wrong parameter type, missing return, and one test per remaining code. The list of codes is a table in document 03. An unbounded loop cannot be expressed in the IR: a test shows that the only loop, `for` over a scan, is bounded by a constant of the encoding, and that every element and opcode is accounted for.
- [ ] Property: every program the validator accepts executes without a type error, without exceeding the computed read and write counts, on random parameters and random state (1,000,000 executions).
- [ ] The computed bound is exact or an upper bound, never lower: property test comparing with counted accesses.
- [ ] Key provenance for a corpus of 50 hand-classified programs matches the hand classification exactly.
- [ ] The shard set computed from parameters equals the set of partition keys actually accessed, for random parameters, for every static-key program in the corpus.
- [ ] Validation time is bounded: the largest valid program validates in under 10 ms.
- [ ] Fuzz gate 8 on the validator with arbitrary decoded trees: no panic.

## T3.3.3 Executor with per-invocation write buffer

**Goal.** Programs run to completion on the shard thread, with free aborts and no locks.

**Work.**
- An interpreter over the validated IR.
- A per-invocation write buffer: `require` failure discards it; success applies it.
- Reads see earlier writes of the same invocation and of earlier events in the same op.
- A batch of parameter sets per request, one result per invocation.
- Typed errors for runtime failures: overflow, division by zero, a failed `require` with its code.

**Not in this card.** Cross-shard execution (phase 5).

**Depends on.** T3.3.2, T3.2.2.

**Gates.**
- [ ] Model test with random valid programs, random parameters and random state, replicated: every result and the final state equal the reference model's interpreter.
- [ ] A failed `require` leaves no trace: the data-file hash, ignoring the WAL and the session reply, equals the hash without the invocation.
- [ ] An arithmetic overflow inside a program fails that invocation with `overflow` and leaves no trace.
- [ ] Execution is deterministic: the same op on every replica gives identical results and byte-identical storage (convergence checker, seed gate 100,000).
- [ ] An invocation never exceeds the validator's read and write bounds (assertion).
- [ ] No invocation runs longer than 1 ms; the worst-case program at the limits is measured on real hardware.
- [ ] The executor allocates nothing per invocation after startup (counting allocator).
- [ ] The reference interpreter in `sim` shares no code with the executor.

## T3.3.4 Program text syntax and compiler in tooling

**Goal.** Programs are written as text, as in the design's examples, and compiled to the IR by the same tool as schemas.

**Work.**
- A grammar for programs as used in document 03 sections 5 and 10.
- Compilation to the binary IR, running the validator and reporting its errors with file, line and column.
- A report per program: read and write bounds, key provenance, shard set.
- A formatter that prints IR back as canonical text.

**Not in this card.** The `on <participant>:` blocks and classification output (T5.2.6).

**Depends on.** T3.3.2, T1.4.5.

**Gates.**
- [ ] The `transfer` example of document 03 section 5 compiles, registers and runs against a 3-replica group in an integration test, with both the success and the `InsufficientFunds` outcome checked.
- [ ] Round trip: compile, print, compile gives byte-identical IR (property test over random valid programs).
- [ ] Every validator error code surfaces as a compiler error naming file, line and column (one test per code).
- [ ] Fuzz gate 4 on the parser: no panic and no hang on arbitrary text.
- [ ] The compiler's report for the example programs is committed as a golden file and checked in CI.
- [ ] The grammar is documented, and every example in the design documents that is a program compiles or is corrected.

## T3.3.5 Register, retire and inline programs

**Goal.** Programs become part of replicated state, so every replica runs exactly the same code at the same op.

**Work.**
- `register` as a logged op: validate, store the IR under an id with a version.
- `retire` as a logged op.
- `execute` by program id, and inline programs sent with the request and validated on arrival.
- A validated-program cache per shard.
- The limit of 4096 programs.

**Not in this card.** Distribution through the metadata group (T4.2.2); here registration is per group.

**Depends on.** T3.3.3.

**Gates.**
- [ ] Register and retire take effect at the same op on every replica (convergence checker, seed gate 10,000 with all faults).
- [ ] An `execute` of an unknown or retired program returns a typed error and writes nothing.
- [ ] A program registered before a view change, a full-group restart or a state sync is still executable afterwards (three tests).
- [ ] An inline program is validated with the same validator; an invalid one is rejected whole (test).
- [ ] Registering the same id with different content is rejected; with identical content it succeeds idempotently.
- [ ] The 4097th registration is rejected with a typed error.
- [ ] The validated-program cache never changes results: two replicas with different cache sizes stay byte-identical.

## T3.4.1 Derived table definition and maintenance in the same op, with overflow-free accumulators

**Goal.** Tables such as balances that are always exactly the fold of an immutable source, updated in the same op as the source insert, and unable to fail.

**Work.**
- Derived table definition: source, key starting with the partition key, folds from sum, count, min, max, first, last, with filters.
- The rule that a derived table is in the same table group as its source and that the source is immutable.
- Accumulator sizing by the validator: value width plus 64 bits for the count, so a sum provably cannot overflow.
- Maintenance in the same op as the source insert, inside the same write buffer.
- Storage as a versioned table.

**Not in this card.** Reading derived tables in conditions (T3.4.2).

**Depends on.** T3.1.1, T1.5.1, T1.4.5, T3.3.2.

**Gates.**
- [ ] Model test, replicated: at every op, each derived row equals the fold computed from scratch over the source table by the reference model.
- [ ] A snapshot read of a derived table at T equals the fold over source rows with timestamp at or below T (model test).
- [ ] The validator rejects a derived table whose source is versioned, whose source is in another table group, or whose key does not start with the partition key (three tests).
- [ ] For every sum the validator states the accumulator width, and a property test shows that 2^64 additions of the maximum value fit (by arithmetic on the widths, plus a boundary test).
- [ ] No insert into the source can fail because of the derived table: fuzzing inserts of extreme values for 1,000,000 cases never produces an error from maintenance.
- [ ] An `exists_identical` duplicate insert does not change the derived table (test).
- [ ] A failed linked chain or failed program invocation leaves the derived table unchanged (the gate deferred from T3.2.2).
- [ ] Storage stays byte-identical across replicas (convergence checker, seed gate 100,000).
- [ ] The validator rejects a program that writes to a derived table with its own code, listed in the table of validator codes in document 03 (test) (from T3.3.2).

## T3.4.5 Local unique indexes

**Goal.** Uniqueness of a column set within a partition, enforced inside the shard.

**Work.**
- `unique local (columns)` in the schema.
- An index tree keyed by (token, columns), checked and written in the same op as the base write.
- Result code `unique_violation` naming the index.
- Maintenance on update and delete of versioned tables.

**Not in this card.** Cluster-wide uniqueness (T5.2.12).

**Depends on.** T3.4.3.

**Gates.**
- [ ] Model test, replicated: no two live rows in the same partition ever share the indexed values; results equal the reference model.
- [ ] Two inserts with the same indexed values in one batch: the first succeeds and the second gets `unique_violation`.
- [ ] After a delete, the value can be reused (test).
- [ ] An update that changes the indexed columns to a value in use fails and leaves both rows unchanged.
- [ ] Rows in different partitions may share the value, and the documentation says so explicitly.
- [ ] A failed write leaves the index unchanged (test with linked chains and programs).
- [ ] Storage stays byte-identical across replicas (convergence checker, seed gate 10,000).

## T3.4.6 Exchange reference schema: entries, balances, orders, place_order

**Goal.** The first application's schema runs end to end on one group, which tests the data model against the workload it was designed for.

**Work.**
- The schema of document 03 section 10 under `schemas/exchange/`: `entry`, `balance`, `order`, `trade_leg`.
- The `place_order` program: check available balance, insert a hold entry, insert the order.
- A cancel program that releases a hold.
- The per-partition fee row pattern.
- A benchmark workload for it.

**Not in this card.** `settle_trade`, which is cross-shard (T5.2.7).

**Depends on.** T3.4.2, T3.3.4.

**Gates.**
- [ ] The schema and programs compile in CI and the compiler's report is committed as a golden file.
- [ ] Model test, replicated: for random sequences of deposits, orders and cancels, `credits - debits - held` never goes negative for any (user, asset).
- [ ] An order larger than the available balance fails with `InsufficientFunds` and writes nothing.
- [ ] Two orders that together exceed the balance, in one batch: exactly the first succeeds.
- [ ] A report on real hardware gives `place_order` throughput and latency (median, p99, p99.9) on one 3-replica group, next to the ledger benchmark's numbers.
- [ ] The declared layouts match the G1 recommendation, or the report explains the difference.
- [ ] A documentation page walks through the schema and its commands run in CI.

## T3.4.2 Reads of derived tables in preconditions and programs

**Goal.** A transaction can decide on the basis of a balance, which is the reason derived tables exist.

**Work.**
- `get` on a derived table inside programs.
- Preconditions that compare a derived column with a constant.
- Reads see the effect of earlier events of the same op.
- Derived tables remain read-only to programs and clients.

**Not in this card.** Analytical scans of derived tables (phase 6).

**Depends on.** T3.4.1, T3.3.3, T3.2.1.

**Gates.**
- [ ] The balance-check `transfer` program of document 03 runs replicated: model test over random transfers shows no account's balance ever goes below zero.
- [ ] Two transfers in one batch that together exceed the balance: the first succeeds, the second fails with `InsufficientFunds`; the second saw the first's effect (test).
- [ ] A read of a derived row that does not exist yet returns the fold's identity values (zero sums, zero count), not an error (test).
- [ ] Any write to a derived table from a client or a program is rejected at validation.
- [ ] A read of a derived table inside a program costs one point read and can be served by the row cache (counter).
- [ ] Results are identical on every replica (convergence checker, seed gate 100,000).

## T3.4.3 Local secondary indexes

**Goal.** Lookups by columns other than the primary key, within a partition, always consistent with the base table.

**Work.**
- Index definition in the schema; optional covering columns.
- An index tree with sort key `(token, indexed columns, primary key)`.
- Maintenance in the same op as the base write, for inserts, updates and deletes.
- Index scans: by indexed-column range within a partition.
- Versioned semantics for indexes on versioned tables.

**Not in this card.** Cross-partition index lookups (fan-out queries, phase 6).

**Depends on.** T3.1.3, T1.2.5.

**Gates.**
- [ ] Model test, replicated: an index scan returns exactly the rows a full scan with the same predicate returns, at every op and at any snapshot.
- [ ] After an update that changes an indexed column, the old value no longer finds the row at the latest snapshot, and still finds it at an older snapshot (test).
- [ ] A failed linked chain or failed program invocation leaves every index unchanged (the gate deferred from T3.2.2).
- [ ] A covering index answers a query on covered columns without reading the base table (counter).
- [ ] Index and base table are written in the same op: no checkpoint or crash point exists where one has a row the other lacks (seed gate 100,000 with crashes, checked by a consistency scan after every recovery).
- [ ] Storage stays byte-identical across replicas.

## T3.4.4 Pending/post/void schema pattern library

**Goal.** A standard, tested pattern for two-phase business workflows that span external systems, so users do not reinvent it wrongly.

**Work.**
- A schema and programs under `schemas/patterns/`: create a pending row with a timeout, post it, void it.
- A derived table that counts pending amounts apart from posted ones.
- Expiry evaluated lazily against commit timestamps, deterministically.
- Documentation of the pattern and its guarantees.

**Not in this card.** A protocol-level two-phase feature; this is a schema pattern only.

**Depends on.** T3.4.2, T3.3.4.

**Gates.**
- [ ] Model test, replicated: for random sequences of pend, post, void and time advance, a pending row ends in exactly one of posted, voided or expired, and the derived totals equal the model.
- [ ] Post after expiry fails; void after post fails; post twice fails the second time; each with its own code (tests).
- [ ] Expiry depends only on commit timestamps: replicas with different wall clocks stay byte-identical (convergence checker).
- [ ] A pending amount is never counted as available balance (model test on the invariant).
- [ ] The example runs in CI and its documentation page states what is and is not guaranteed when the external system fails.

## T3.6.1 Subscription protocol, cursor, server-side table and column filters

**Goal.** Other services can follow committed changes of a group in commit order and resume exactly where they stopped.

**Work.**
- A subscribe request with a cursor `(group, op, index)`, a table filter and a column list.
- The change record: `(group, op, index, timestamp, table, kind, row)`.
- At-least-once delivery; flow control so a slow consumer does not buffer on the server.
- Changes from derived tables emitted like any versioned table.
- Filters applied on the server.

**Not in this card.** Serving from parts (T3.6.2), merging across groups (T5.3.4).

**Depends on.** T3.1.3, T3.4.1.

**Gates.**
- [ ] In the simulator with all faults (seed gate 100,000), a consumer that reconnects with its last cursor after every disconnection receives every committed change at least once, in commit order, with no gap.
- [ ] Applying the feed idempotently by cursor to an empty model reproduces the group's state exactly at the end of the run (checker).
- [ ] No uncommitted or later-truncated op is ever delivered (checker; injected bug that streams from uncommitted prepares is caught).
- [ ] A filtered subscription delivers only the named tables and columns, and the bytes sent are fewer (test).
- [ ] A consumer that stops reading causes no growth of server memory (measured over 1,000,000 changes).
- [ ] The cursor and record formats are specified in document 05 with a version.
- [ ] A change to a derived row is delivered in the same op position as the source insert that caused it.

## T3.6.2 Serve from WAL on backups; fall back to parts by timestamp

**Goal.** Subscribers never slow commits, and a subscriber that falls far behind still catches up.

**Work.**
- The feed served by backups and standbys only.
- Recent changes read from the WAL.
- Older history read from parts by commit timestamp, using `_ts` and tombstones.
- A switch from WAL to parts when a subscriber's cursor falls out of the WAL, and back when it catches up.
- A typed error when the cursor is older than the table's retention.

**Not in this card.** Server-stored cursors (T7.2.3).

**Depends on.** T3.6.1, T3.1.2, T2.2.11.

**Gates.**
- [ ] A consumer paused for longer than the WAL covers resumes and receives every change with no gap and no duplicate beyond its cursor (seed gate 10,000 with all faults; checker rebuilds state from the feed).
- [ ] The switch from parts back to the WAL loses and repeats nothing at the boundary (targeted test at every boundary offset).
- [ ] The primary serves no subscription: a subscribe sent to it is redirected (test), and commit latency with 100 subscribers attached to backups is within 5% of baseline (report).
- [ ] A cursor older than retention returns `cursor_too_old`; no partial history is delivered silently.
- [ ] After a view change, a subscriber on the replica that became primary is moved to a backup without losing its place (test).
- [ ] Feed work on a backup obeys the 1 ms slice rule.

## T3.6.3 Client subscription API and exactly-once consumer example

**Goal.** A consumer library that makes the correct pattern the easy one, and a worked example other teams can copy.

**Work.**
- A subscription API in the Rust client: stream of changes, cursor accessor, automatic reconnect to another replica.
- An example consumer that follows balances and stores its cursor atomically with its own state, giving exactly-once effect.
- Documentation of at-least-once delivery and how to get exactly-once effect.

**Not in this card.** Go and Python bindings (T7.3.4, T7.3.5).

**Depends on.** T3.6.2.

**Gates.**
- [ ] The balance-following example passes a model test in the simulator with all faults and with the consumer itself crashing at random points: its final state equals the database's balances, for 10,000 seeds.
- [ ] The same example with the cursor stored separately from the state, as a deliberate defect, fails the test (injected bug), and the documentation uses it to explain why.
- [ ] The client reconnects to another replica without user code and continues from the cursor (test).
- [ ] The API cannot hand out a change without its cursor (by its types).
- [ ] The example and its documentation page run in CI.

## T3.5.1 Online add/drop column and index

**Goal.** A schema can change while the cluster serves traffic, and old data stays readable.

**Work.**
- Schema change as a logged op taking effect at a defined op on all replicas.
- Add a nullable or defaulted column; drop a column; add and drop an index.
- Parts record the schema version they were written with; readers map old parts to the current schema.
- Index build for existing data as paced background work, with the index usable only when complete.
- Rejection of unsupported changes: column type, keys, table kind.

**Not in this card.** Distribution of schema changes across groups (T4.2.2).

**Depends on.** T3.4.3, T1.3.1.

**Gates.**
- [ ] Model test, replicated, with schema changes interleaved with writes and reads: results equal the reference model, across parts written under at least three schema versions.
- [ ] A row written before a column was added reads back with the default, or null, for that column.
- [ ] A dropped column is no longer returned, and its space is reclaimed after compaction (measured).
- [ ] A new index is not used for reads until its build is complete, and once complete it equals a full scan (model test with writes during the build).
- [ ] The switch happens at the same op on every replica (convergence checker, seed gate 10,000 with all faults, including a crash during the index build).
- [ ] Each unsupported change is rejected with its own typed error.
- [ ] The index build obeys the 1 ms slice rule and commit latency during it stays within 10% of baseline (report).

## T3.5.2 Client code generation from schema

**Goal.** Application code uses typed structs and typed calls, so a schema mismatch is a compile error in the application.

**Work.**
- A generator for Rust: a row struct per table, operation builders, a stub per program with typed parameters and typed result codes.
- Output that is deterministic and formatted.
- A build-script integration and a command-line mode.
- Generated code for the example schemas checked in CI.

**Not in this card.** Go and Python generators (with T7.3.4 and T7.3.5).

**Depends on.** T3.3.4, T1.5.4.

**Gates.**
- [ ] Generated code for the exchange and pattern schemas compiles and is used by their integration tests.
- [ ] A row built with the generated struct encodes to bytes identical to the hand-written reference encoding (property test).
- [ ] Changing a column's type in the schema makes an application using the old type fail to compile (compile-fail test).
- [ ] Generation is deterministic: two runs give byte-identical output, and CI fails if committed generated code is stale.
- [ ] Every result code of every program is a variant of a generated enum; no stringly-typed errors.
- [ ] Generated code contains no `unsafe`.

## T3.5.3 Ledger benchmark and G2 report

**Goal.** Answer gate G2: on a ledger workload, are we within 2x of TigerBeetle on the same hardware?

**Work.**
- A two-row transfer workload equivalent to TigerBeetle's benchmark: accounts and transfers with balance maintenance.
- Run both systems on the same three servers with replication factor 3, each configured per its documentation.
- Measure throughput and latency percentiles, and write the comparison.
- If outside 2x, a profile-based analysis of where the time goes.

**Not in this card.** The performance work itself, if the gate fails; that becomes an inserted phase.

**Depends on.** T3.4.2, T2.3.6. Three production-like servers.

**Gates.**
- [ ] Both systems run on the same real hardware, same drives, same network, same replication factor; versions and full configurations are in the report.
- [ ] The workloads are equivalent, and the report justifies it: same number of accounts, same transfer mix, same durability guarantees.
- [ ] The report gives transfers per second and latency (median, p99, p99.9) for both systems, each from 3 runs of at least 10 minutes after warm-up, with the spread.
- [ ] The report states the ratio and a plain pass or fail against "within 2x of TigerBeetle's throughput".
- [ ] If fail: a profile of the commit path with the top five costs and an estimate for each, and a proposed plan for the inserted performance phase.
- [ ] The comparison is reviewed by an engineer who did not run it, who reproduces at least one data point.
- [ ] The benchmark for our side reruns with one `just` recipe, and the result is recorded in `DESIGN.md`.

## T3.5.4 Mutual TLS and authorisation per table group

**Goal.** Only authenticated nodes and clients can connect, and a client can touch only the table groups it is allowed to.

**Work.**
- Mutual TLS on every connection with rustls: client to shard and shard to shard.
- Certificates identify nodes and clients.
- Authorisation rules mapping a client identity to table groups and to read or read-write access, stored as replicated configuration.
- Certificate reload without restart; TLS session resumption.
- Handshakes in the lowest scheduling class with a per-iteration cap.

**Not in this card.** Kernel TLS; per-row or per-column permissions.

**Depends on.** T0.3.3, T1.5.4.

**Gates.**
- [ ] A connection without a client certificate, with an expired one, with one from an unknown authority, or a plaintext connection is refused (four tests).
- [ ] A client authorised for table group A gets `unauthorized` for any operation on group B, for reads and writes, including through programs and subscriptions (tests for each path).
- [ ] A read-only client cannot write.
- [ ] Authorisation is checked on the shard for every request, not once per connection (test that revokes access mid-connection).
- [ ] A report on real hardware gives the throughput and latency cost of TLS against plaintext; the cost is under 15% of throughput at full-message batches (proposed threshold).
- [ ] 1,000 simultaneous new handshakes do not raise p99.9 loop iteration above 5 ms on the shard (real hardware).
- [ ] Certificates are replaced without restart and without dropping established connections (test).
- [ ] No key material is written to logs or traces (test that greps the output of a run).

## T3.5.5 Operator documentation and first tagged release

**Goal.** Someone outside the team can run a replicated cluster and use it, from the documentation alone. This is the first externally useful release.

**Work.**
- Documentation: installation, hardware requirements, formatting and starting a 3-replica cluster, schemas and programs, the client, the change feed, monitoring basics, failure recovery, known limits.
- A release process: version number, changelog, built binaries for Linux x86-64, checksums.
- A statement of what is and is not guaranteed in this release.
- A tag.

**Not in this card.** Multi-shard clusters (phase 4), upgrades between releases (T7.2.1).

**Depends on.** Every other task of phases 0 to 3.

**Gates.**
- [ ] An engineer who has not worked on the project follows the documentation on three fresh machines and reaches a running 3-replica cluster with the exchange example in under 60 minutes, without asking for help. Their notes are recorded and every problem they hit is fixed.
- [ ] Every command in the documentation is executed by a CI job.
- [ ] The release notes list the guarantees (single-shard strict serializability, durability under the stated fault model) and the limits (single group, no upgrades, x86-64 Linux only).
- [ ] The release binary is built by CI from the tag, reproducibly: two builds give identical checksums.
- [ ] Gate G2 has a recorded result.
- [ ] The fleet (T2.3.4) has run at least 10,000,000 seeds on the release commit with zero failures.
- [ ] Every issue of phases 0 to 3 is closed or explicitly moved, with the reason.
