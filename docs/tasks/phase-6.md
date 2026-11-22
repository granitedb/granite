# Phase 6. HTAP

Phase exit: analytical queries over a consistent snapshot across shards, with a measured bound on OLTP latency impact.

Design references: `docs/05-query-and-protocol.md` section 4 and `docs/01-storage.md` section 4. Throughout this phase, "OLTP bound" means the agreed limit on how much transaction latency may rise while queries run. Proposed threshold, to be confirmed before the phase starts: commit latency p99 rises by at most 10% and p99.9 loop iteration stays under 5 ms.

## T6.1.1 Heavy codecs: delta, double-delta, RLE, FSST, LZ4, ZSTD

**Goal.** The encodings for deep levels, where data is read by scans and compression matters more than random access.

**Work.**
- Delta and double-delta for sorted and time columns; run-length encoding; FSST for strings.
- LZ4 and ZSTD as general-purpose block compression, vendored and pinned.
- A scalar implementation of each of our own codecs.
- Chunk headers that record the codec chain and its version.

**Not in this card.** Choosing which codec to use where (T6.1.2), SIMD versions (T6.2.4).

**Depends on.** T1.3.2, T1.3.3.

**Gates.**
- [ ] Round-trip fuzz gate 8 per codec and per supported type, including empty, single-value, all-equal, strictly increasing and maximum-range inputs.
- [ ] Decoder fuzz gate 8 per codec on arbitrary bytes: no panic, no out-of-bounds access, and output size bounded by the chunk header's declared size before any allocation.
- [ ] Output is deterministic across machines: golden files for every codec are byte-identical on Linux x86-64 and macOS ARM in CI.
- [ ] LZ4 and ZSTD are vendored at a pinned version with compression level and parameters fixed; a test fails if their output for the golden inputs changes.
- [ ] A version bump of a vendored codec is treated as a format change: a note in the documentation and a check in CI.
- [ ] A report gives compression ratio and encode and decode throughput per codec on at least five reference data sets, including the exchange schema's tables.

## T6.1.2 Level-dependent encoding policy

**Goal.** Each chunk gets the right encoding for its level, chosen by a rule every replica applies identically.

**Work.**
- The policy of document 01 section 4: lightweight random-access codecs in levels 0 and 1; delta, double-delta, RLE, FSST and LZ4 deeper; ZSTD optionally at the bottom.
- A deterministic selection rule from the chunk's data and the level, with no timed trials.
- Per-table overrides in the schema.
- Compression ratio tracked as a metric and in benchmarks.

**Not in this card.** New codecs.

**Depends on.** T6.1.1, T1.2.4.

**Gates.**
- [ ] The selection is a pure function of (column type, chunk values, level, table options): property test that the same inputs always give the same codec and bytes.
- [ ] Replicas stay byte-identical with the policy active (convergence checker, seed gate 100,000 with all faults).
- [ ] Levels 0 and 1 never use a codec without random access (assertion).
- [ ] Point-read latency from levels 0 and 1 is unchanged, within 5%, from before this task; point-read latency from deep levels is reported with the new codecs (report on real hardware).
- [ ] A report gives bytes on disk per row for the exchange schema's tables before and after, and the scan throughput change.
- [ ] Compaction CPU cost per byte with the policy is measured, and the pacing quota is re-validated: no level exceeds its bound under the worst-case workload.

## T6.1.3 Skip indexes: min/max, bloom, set

**Goal.** A scan skips chunks that cannot contain matching rows.

**Work.**
- Per-chunk minimum and maximum for every column by default.
- Optional bloom and small-set indexes on declared columns.
- Storage in skip-index blocks of the part.
- Use in the scan path: a predicate is tested against the index before the chunk is read.

**Not in this card.** Secondary indexes for point lookups (T3.4.3).

**Depends on.** T1.3.1, T1.2.5.

**Gates.**
- [ ] Soundness: a chunk is never skipped when it contains a matching row. Model test comparing scans with and without skip indexes over random data and random predicates (1,000,000 cases).
- [ ] Removing the soundness condition for one comparison operator, as a deliberate defect, is caught by the model test (injected bug).
- [ ] For each index kind and each comparison operator there is a test with a chunk that must be skipped and one that must not.
- [ ] A report gives the pruning rate (chunks skipped over chunks considered) for a set of reference queries on the exchange data, on time-range and equality predicates.
- [ ] Index build is deterministic and replicas stay byte-identical.
- [ ] Skip-index blocks add under 2% to part size for min/max alone (measured).

## T6.1.4 Columnar projection for `row` tables

**Goal.** A table that needs both whole-row point reads of any age and fast scans can have both, by keeping a columnar copy of selected columns in its deep levels.

**Work.**
- A schema option naming the projected columns.
- Compaction into the deepest levels writes the projection alongside the row-layout chunks.
- Scans use the projection when it covers the requested columns; point reads never use it.
- The projection is derived data and can be rebuilt.

**Not in this card.** Changing a table's base layout.

**Depends on.** T6.1.2, T1.3.7 (the G1 decision says whether this is needed and when).

**Gates.**
- [ ] Model test, replicated: a scan answered from the projection returns exactly what the same scan answers from the row chunks, at any snapshot.
- [ ] Point reads perform the same number of device reads with and without the projection (counter).
- [ ] The feature is opt-in: a table without the option has byte-identical parts to before this task.
- [ ] A projection chunk that fails verification is rebuilt from the row chunks, and the scan still returns correct results (test).
- [ ] A report gives scan throughput on a 50-column `row` table for a 3-column scan with and without the projection, and the extra space and compaction work it costs.
- [ ] Replicas stay byte-identical (convergence checker, seed gate 10,000).

## T6.1.5 Time segments: one tree per time bucket

**Goal.** Tables partitioned by time inside a shard, so time-range queries prune whole trees and old data can later be dropped or archived cheaply.

**Work.**
- A schema option: a time bucket on a timestamp column.
- One LSM tree per bucket per table.
- Routing of writes to the bucket; reads and scans across buckets.
- Bucket creation driven by commit timestamps.
- Limits on the number of open buckets.

**Not in this card.** Dropping and archiving segments (T7.2.5, T7.2.6).

**Depends on.** T1.2.2, T1.2.5.

**Gates.**
- [ ] Model test, replicated: point reads and scans over a segmented table equal the reference model, for rows in any bucket and scans spanning buckets.
- [ ] A scan with a time range opens only the trees of buckets that intersect it (counter), and this still holds after every tree has been fully compacted.
- [ ] Bucket boundaries depend only on the column value and the schema, so replicas agree (convergence checker, seed gate 10,000).
- [ ] A uniqueness check on a segmented table finds a duplicate key in another bucket, or the schema requires the time column in the primary key and the validator enforces it (one of the two, documented and tested).
- [ ] The manifest handles 10,000 buckets for one table within stated memory and lookup bounds (test).
- [ ] A late row for an old bucket is accepted into that bucket (test).

## T6.2.1 Scan fragment IR, validator and memory bounding

**Goal.** The only analytical request a shard accepts: a scan fragment whose memory use is known before it runs.

**Work.**
- The fragment IR of document 05 section 4: `Scan`, `Project`, `PartialAggregate`, `TopN`, `Limit`, with typed expressions.
- A versioned binary encoding.
- A validator: types, known tables and columns, and a static memory bound computed for every operator.
- Rejection of any fragment without a bound, or whose bound exceeds the limit.

**Not in this card.** Execution (T6.2.2, T6.2.3), joins and sorts, which run in the gateway. Measuring the peak memory of executed fragments against the bound, which needs execution (T6.2.3).

**Depends on.** T3.3.1 (shares the expression encoding).

**Gates.**
- [ ] Fuzz gate 8 on the decoder and on the validator.
- [ ] Every operator has a documented memory-bound formula and a test that the validator computes it.
- [ ] Unbounded fragments are rejected, each with its own code: a `TopN` without n, an aggregate without a group-table bound, a scan without a column list (tests).
- [ ] A fragment with a join, a subquery, an unbounded sort or a write has no encoding; a test asserts no opcode decodes to one.
- [ ] The IR is specified in document 05 with a version.

## T6.2.2 Vectorised scan with version resolution and pushdown

**Goal.** The scan operator: fast over columns, correct for versioned tables, reading only what it needs.

**Work.**
- A pull-based scan producing column batches of up to 4096 rows.
- Pruning: parts by key and timestamp range, chunks by skip indexes, columns by the request.
- Predicate evaluation on encoded data where the codec allows (dictionary, frame of reference, RLE).
- Version resolution at the snapshot timestamp during the merge.
- Yielding after a fixed number of batches.

**Not in this card.** Aggregation (T6.2.3), SIMD kernels (T6.2.4).

**Depends on.** T6.2.1, T6.1.3, T3.1.1.

**Gates.**
- [ ] Results equal the reference model for 1,000,000 random scans over random data, across immutable and versioned tables, all three layouts, and any snapshot (model test, replicated).
- [ ] A predicate evaluated on encoded data returns the same rows as the same predicate on decoded data, for every codec that supports it (property test).
- [ ] A scan of 3 columns of a 50-column `columnar` table reads only those columns' chunks plus key and index blocks (counter).
- [ ] The operator yields within 1 ms; a scan over 1 billion rows never raises p99.9 loop iteration above 5 ms when run on the shard thread (real hardware).
- [ ] Zero heap allocations per batch after the scan is set up.
- [ ] A report gives scan throughput in rows per second and bytes per second for the reference queries, per layout.

## T6.2.3 Filter, project, partial aggregate with bounded flush, top-N

**Goal.** The remaining fragment operators, each within its memory bound.

**Work.**
- Filter and project over column batches with typed, checked expressions.
- Partial aggregation with sum, count, min and max by group keys, using a hash table of fixed size; when it is full, partial groups are flushed downstream.
- Top-N with a bounded heap; limit.
- A fixed evaluation order for floating-point sums so results are identical across machines.

**Not in this card.** Sketches (T6.2.5), final aggregation in the gateway (T6.4.3).

**Depends on.** T6.2.2.

**Gates.**
- [ ] Results equal the reference model for 1,000,000 random fragments (model test). For partial aggregates, the comparison is made after merging the partial groups.
- [ ] With a group table bound smaller than the number of groups, the operator flushes and the merged result is still exact (test with 100 times more groups than the bound).
- [ ] Property: for 1,000,000 random valid fragments executed on random data, measured peak memory never exceeds the validator's bound (from T6.2.1).
- [ ] Integer and decimal sums use accumulators that cannot overflow, or return a typed overflow error; they never wrap (test at the boundary).
- [ ] Floating-point aggregates are bit-identical on Linux x86-64 and macOS ARM for the golden queries.
- [ ] Top-N with ties is deterministic: the tie-break rule is documented and tested.
- [ ] Each operator yields within 1 ms.

## T6.2.4 SIMD kernels (AVX2 baseline) with scalar reference implementations

**Goal.** Fast decode and filter loops on production hardware, with a proof on every commit that they compute exactly what the portable code computes.

**Work.**
- AVX2 kernels for the hot loops: bit-unpacking, frame-of-reference decode, dictionary lookup, comparison filters, selection, sum.
- A scalar reference implementation of each, which is what runs on ARM development machines.
- Runtime selection by CPU feature, with x86-64-v3 as the production baseline.
- A differential test harness.

**Not in this card.** AVX-512 kernels; allowed later only with the same identity guarantee.

**Depends on.** T6.2.2, T6.2.3.

**Gates.**
- [ ] For every kernel, SIMD and scalar outputs are bit-identical over 10,000,000 random inputs including every length from 0 to 257 and all alignments, checked in CI on x86-64.
- [ ] Differential fuzz gate 8 per kernel with an assertion on identity.
- [ ] Every `unsafe` block has a safety argument, and the kernels pass Miri where Miri supports the intrinsics and address sanitizer everywhere else.
- [ ] A build for a CPU without AVX2 runs the scalar path and passes the whole test suite.
- [ ] A report gives the speed-up of each kernel over scalar on real hardware; a kernel under 1.5 times faster is removed.
- [ ] Replicas with SIMD enabled and disabled produce byte-identical data files and identical query results (test with a mixed group).

## T6.2.5 Deterministic mergeable sketches

**Goal.** Approximate distinct counts and quantiles that shards can compute in parts and the coordinator can merge, with identical results everywhere.

**Work.**
- A distinct-count sketch (HyperLogLog) and a quantile sketch (t-digest or DDSketch); the choice recorded.
- A fixed hash and a canonical serialised form.
- Merge that is associative and commutative on the serialised form.
- Use as aggregates in the fragment IR.

**Not in this card.** Exact distinct counts, which the gateway computes when asked.

**Depends on.** T6.2.3.

**Gates.**
- [ ] The decision between the quantile sketches is recorded in `docs/decisions/` with accuracy and size numbers; the open point in document 05 section 6 is closed.
- [ ] Error bounds are tested: for the distinct-count sketch, relative error under the documented bound in at least 99% of 10,000 trials at cardinalities from 10 to 10^9; for quantiles, error under the documented bound at p50, p99 and p99.9.
- [ ] Merge is associative and commutative: merging the same parts in any order and grouping gives byte-identical sketches (property test).
- [ ] A sketch built in one piece equals the merge of sketches built from any partition of the input, within the documented tolerance, and byte-identically where the algorithm allows.
- [ ] Sketches are byte-identical on Linux x86-64 and macOS ARM for golden inputs.
- [ ] Fuzz gate 4 on the sketch decoders.

## T6.4.1 Evaluate the query engine and its pushdown interfaces; written decision

**Goal.** A checked decision on which existing query engine the gateway is built on, instead of an assumption.

**Work.**
- Evaluate Apache DataFusion as the candidate: current state, release cadence, licence, the table-provider interface, and what it can push down (projection, filter, limit, partial aggregation).
- A spike: a SQL query over a stub table provider that returns Arrow batches.
- An assessment of Arrow Flight SQL for the user-facing protocol.
- Alternatives considered and why they were rejected.

**Not in this card.** The real table provider (T6.4.2).

**Depends on.** T6.2.1.

**Gates.**
- [ ] The spike is in the repository: a SQL query with a filter, a projection and a `GROUP BY` runs over a stub table provider, and a test checks its result.
- [ ] The decision document states, with references to the engine's documentation and source at a named version: which pushdowns the interface supports, which it does not, and how partial-aggregate pushdown will be achieved.
- [ ] The engine's licence and those of its dependencies pass the project's licence check.
- [ ] The gateway's dependency tree is shown to be absent from the server binary (`cargo tree -p granite`).
- [ ] The document names the fallback if the engine cannot push down partial aggregates.
- [ ] `DESIGN.md` and document 05 change the gateway's engine from "candidate" to "confirmed", or to the alternative.

## T6.4.2 Table provider: schema mapping, projection, filter and limit pushdown

**Goal.** The database appears to the query engine as tables, and the engine's simple pushdowns become scan fragments.

**Work.**
- Mapping of Granite schemas and types to the engine's Arrow schema.
- A table provider that turns projection, filter and limit into fragments and returns Arrow batches.
- Translation of the engine's filter expressions into fragment predicates; filters that cannot be pushed are left to the engine.
- A query corpus and a comparison harness.

**Not in this card.** Aggregate pushdown (T6.4.3), snapshot selection (T6.4.4).

**Depends on.** T6.4.1, T6.3.1, T6.3.4.

**Gates.**
- [ ] SQL results equal the reference model on a committed corpus of at least 500 queries covering every supported type, every comparison operator, nulls and empty results.
- [ ] Every Granite type maps to a documented Arrow type, and values at the minimum and maximum of each type survive the round trip (test).
- [ ] A filter that is pushed down and the same filter evaluated in the engine give identical results for the whole corpus (differential test).
- [ ] A query selecting 3 columns of a 50-column table requests only those 3 from the shards (counter).
- [ ] A `LIMIT 10` query stops the shard scans early (counter on rows scanned).
- [ ] A filter the provider cannot translate is never silently dropped: it is reported back to the engine as not pushed (test).

## T6.4.3 Partial-aggregate pushdown and merge

**Goal.** Aggregations run on the shards and only small partial results travel to the gateway.

**Work.**
- A planner rule that splits an aggregation into a partial stage pushed to the shards and a final stage in the gateway.
- Support for sum, count, min, max, average (as sum and count) and the sketches.
- Merge of partial results from many shards and from bounded flushes.
- A fallback to raw-row aggregation in the gateway for unsupported aggregates.

**Not in this card.** Join pushdown.

**Depends on.** T6.4.2, T6.2.3, T6.2.5.

**Gates.**
- [ ] For the aggregation queries in the corpus, pushed-down results equal the reference model and equal the non-pushed-down results.
- [ ] A `GROUP BY` over 1 billion rows with 1,000 groups transfers fewer than 100,000 rows to the gateway (counter).
- [ ] Bytes shipped to the gateway for the reference aggregation queries are reported with and without pushdown.
- [ ] Decimal and integer sums merged across shards cannot overflow silently: they widen or return a typed error (boundary test).
- [ ] An unsupported aggregate falls back to the gateway and still returns the correct result (test).
- [ ] The plan chosen for each corpus query is committed as a golden file, so a change in pushdown is visible in review.

## T6.4.4 Snapshot selection, fan-out, replica preference, retries

**Goal.** One SQL query sees one moment of the whole cluster, and survives the ordinary failures of the cluster under it.

**Work.**
- One snapshot timestamp chosen per query.
- Fan-out of fragments to the relevant groups, pruned by partition key where the query allows.
- Preference for backups, standbys and scan workers over primaries.
- Retry of a fragment on another replica at the same timestamp.
- A typed failure when a shard cannot serve the snapshot in time.

**Not in this card.** Admission control (T6.4.5).

**Depends on.** T6.4.2, T5.3.2.

**Gates.**
- [ ] Every fragment of a query carries the same timestamp (assertion), and results equal the reference model's state at that timestamp while writes continue (model test in the cluster simulator, seed gate 10,000 with all faults).
- [ ] A view change in any group mid-query does not change the result; the query completes or returns a typed error, never a partial result (scenario tests for both).
- [ ] A retried fragment never produces duplicate rows in the result (test with a fragment that fails after sending half its batches).
- [ ] With backups available, primaries receive zero fragments (counter).
- [ ] A query with a partition-key equality predicate contacts exactly one group (counter).
- [ ] A query that needs a shard blocked behind a dead group fails with `unavailable` after the timeout and names the group.

## T6.4.5 Gateway service: read-only credential, admission, memory limit, spill, timeouts

**Goal.** A runaway query can exhaust the gateway and nothing else.

**Work.**
- A stateless service; any number may run.
- `SELECT` only; a read-only credential towards the cluster.
- Admission control: a bound on concurrent queries.
- A per-query memory limit with spill to disk for sorts and joins; a per-query time limit.
- Cancellation that stops the shard fragments.
- Deployment as its own binary outside the server's build.

**Not in this card.** Multi-tenant quotas; one cluster serves one tenant.

**Depends on.** T6.4.4, T3.5.4.

**Gates.**
- [ ] A cross join of two billion-row tables with no limit fails inside the gateway with a typed error, and during it commit latency p99 on every shard stays within the OLTP bound (real hardware).
- [ ] Any statement other than `SELECT` is rejected by the gateway, and independently by the cluster because the credential is read-only (two tests: one through the gateway, one using the credential directly).
- [ ] A query over its memory limit spills or fails; the gateway process's memory never exceeds its configured total (measured under 100 concurrent heavy queries).
- [ ] A cancelled or timed-out query stops its shard fragments within 100 ms (test with a counter on the shard).
- [ ] Killing a gateway mid-query affects only its own queries; another gateway serves a retry (test).
- [ ] Beyond the concurrency limit, new queries queue or are refused with a typed error; they do not degrade running ones (test).
- [ ] The gateway holds no data: it restarts with an empty disk (test).

## T6.4.6 Arrow Flight SQL endpoint

**Goal.** Users reach the gateway with standard tools, without us defining a SQL wire protocol.

**Work.**
- An Arrow Flight SQL endpoint on the gateway.
- Authentication with TLS client certificates or tokens mapped to the read-only role.
- Result streaming as Arrow batches.
- Catalog metadata: tables and columns visible to clients.
- Documentation for connecting from Python and from a JDBC tool.

**Not in this card.** PostgreSQL or MySQL wire compatibility.

**Depends on.** T6.4.5.

**Gates.**
- [ ] A CI job runs a query from Python through an Arrow Flight SQL client and checks the result.
- [ ] A CI job runs a query through the Flight SQL JDBC driver and checks the result.
- [ ] A named desktop SQL tool connects, lists tables and columns, and runs a query; the steps are documented with the tool's version.
- [ ] An unauthenticated connection is refused, and one authenticated without the read role is refused (tests).
- [ ] A result of 100 million rows streams with gateway memory bounded by the per-query limit (measured).
- [ ] Every Granite type appears in a Flight SQL result with the documented Arrow type (test over all types).

## T6.3.1 Fragment fan-out and partial-aggregate merge in the typed client

**Goal.** Programs can run analytical fragments directly through the typed client, without SQL and without the gateway.

**Work.**
- A query call in the Rust client that takes a fragment and a snapshot choice.
- Fan-out to one replica of each relevant group at one timestamp.
- Merge of partial aggregates and of sorted streams.
- Streaming of large results with continuation tokens.

**Not in this card.** SQL (E6.4).

**Depends on.** T6.2.3, T5.3.2, T4.3.2.

**Gates.**
- [ ] A multi-shard query returns results equal to the reference model at its snapshot timestamp, while writes continue (cluster model test, seed gate 10,000 with all faults).
- [ ] All fragments of one query carry one timestamp (assertion).
- [ ] A top-N across shards returns the true global top N (model test with ties).
- [ ] A result larger than client memory can be consumed as a stream with bounded client memory (measured on a 100-million-row result).
- [ ] A fragment whose key range lies in one partition is sent to one group only (counter).
- [ ] A failure of one replica mid-stream is retried on another at the same timestamp without duplicate rows (test).

## T6.3.2 Scan workers on pinned manifest snapshots

**Goal.** Analytical scans run on cores reserved for them, reading immutable data, so they cannot touch a shard's mutable state.

**Work.**
- A node configuration splitting cores between shards and scan workers.
- The shard thread hands a worker a pinned manifest snapshot and the memtable delta for the snapshot timestamp.
- Workers read parts through their own I/O ring and their own cache.
- Blocks referenced by a pinned snapshot are not reused until it is released; a bound on pin duration.
- A statement of this as the one contained exception to one thread per shard.

**Not in this card.** Admission control (T6.3.3).

**Depends on.** T6.2.2, T1.2.5, T4.1.1.

**Gates.**
- [ ] The process passes thread sanitizer under combined OLTP and scan load with zero reports.
- [ ] No mutable shard state is reachable from a worker: the types handed over are immutable snapshots, and a compile-fail test shows a worker cannot obtain a mutable reference.
- [ ] Results from a worker equal results from the same fragment run on the shard thread (differential test over the fragment corpus).
- [ ] A pinned snapshot's blocks are never reused while pinned, and are reclaimed after release or after the pin limit, when the query is failed with a typed error (tests for both).
- [ ] With scans saturating the worker cores, commit latency on the shard cores stays within the OLTP bound (real hardware).
- [ ] Workers running or not running does not change any data file: replicas with and without scan load stay byte-identical.

## T6.3.3 Admission control and scheduling budgets

**Goal.** The measured promise of the phase: queries run, and transactions do not notice beyond the agreed bound.

**Work.**
- A bound on concurrent fragments per shard, each with its memory bound reserved up front.
- The query scheduling class with a time budget per loop iteration; fragments yield after a fixed number of batches.
- Rejection with `busy` beyond the limits.
- A combined benchmark: the exchange OLTP workload with analytical queries running.

**Not in this card.** Gateway-side limits (T6.4.5).

**Depends on.** T6.3.2, T0.3.5.

**Gates.**
- [ ] The OLTP bound is written into this card before the run, and then met on real hardware: with the reference query mix running continuously against backups and scan workers, commit latency p99 and p99.9 on the exchange workload stay within the bound for a 1-hour run.
- [ ] The same run with queries forced onto primaries' shard threads is reported, with its impact, to show what the isolation mechanisms buy.
- [ ] p99.9 loop iteration stays under 5 ms on every shard throughout.
- [ ] Beyond the concurrency limit, fragments get `busy`; memory reserved for queries never exceeds its configured total (measured).
- [ ] A fragment that declares one memory bound and tries to use more is stopped at the bound (test).
- [ ] The benchmark joins the nightly tracked set, and a regression past the bound fails it.

## T6.3.4 Arrow-compatible result batches in the client

**Goal.** Query results reach analytical code as Arrow arrays without being copied or converted.

**Work.**
- The column-batch body: per column a validity bitmap and values, or offsets plus bytes, laid out as Arrow expects.
- Client-side exposure of a received batch as Arrow arrays that borrow the message buffer.
- Mapping of every Granite type to an Arrow type.
- Streaming of batches with continuation tokens.

**Not in this card.** Python bindings (T7.3.5).

**Depends on.** T6.2.2, T1.4.4.

**Gates.**
- [ ] The Arrow arrays point into the received buffer: the data addresses lie inside it, and zero bytes are copied (address check and counting allocator).
- [ ] Every Granite type round-trips through a column batch and reads back equal through the Arrow API, including nulls, empty strings and extreme values (test over all types).
- [ ] The batches pass validation by the Arrow library's own full validation routine.
- [ ] Fuzz gate 8 on the column-batch validator: any batch it accepts can be read through the Arrow API without a bounds error.
- [ ] Buffers are aligned as Arrow requires (assertion).
- [ ] The layout is specified in document 05 with a version.

## T6.3.5 Composite types: Array, Tuple, Map, Json

**Goal.** The remaining data types, across storage, wire and expressions.

**Work.**
- `Array(T, max)`, `Tuple(...)`, `Map(K, V, max)` and `Json(max)`, each with a declared maximum size.
- Storage as nested offset and value chunks.
- Wire format in row batches and column batches, mapped to Arrow list, struct and map types.
- Expressions: element access, length, containment; JSON path extraction with typed results.
- Not allowed in keys.

**Not in this card.** Indexes on nested values.

**Depends on.** T1.3.3, T6.3.4, T3.3.1.

**Gates.**
- [ ] Round-trip fuzz gate 8 for each type through storage and through both wire formats.
- [ ] Decoder fuzz gate 8 on arbitrary bytes: nesting depth and total size are bounded before allocation.
- [ ] A value over its declared maximum is rejected with a typed error and never truncated (boundary tests for each type).
- [ ] Any composite type in a primary key or partition key is rejected by the schema validator.
- [ ] Expression results equal the reference model for 1,000,000 random expressions over random nested values.
- [ ] JSON is validated on write; invalid JSON is rejected. Number handling (integer range, no silent conversion to float) is documented and tested.
- [ ] The static row-size bound stays correct with composite columns (the property test of T1.4.3 extended).
- [ ] Map key order is canonical, so equal maps have equal bytes (test).
