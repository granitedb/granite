# Phase 1. Single-shard storage engine

Phase exit: one shard, no replication, serves typed inserts, gets and scans for immutable tables; survives simulated crashes and disk faults that are recoverable locally; gate G1 answered.

Design reference for the whole phase: `docs/01-storage.md`.

## T1.1.1 Data file format, `format` command, deterministic grid growth

**Goal.** The single file that holds everything a shard persists, created by one command, growing in a way that is identical on every replica.

**Work.**
- The zone layout of document 01 section 2: superblock, WAL headers, WAL prepares, grid.
- `granite format`: creates the file, preallocates the fixed zones, writes the initial superblock with cluster id, group id, replica index and format constants.
- Grid growth in fixed 1 GiB steps, triggered by the free set falling below a threshold, which depends only on the ops applied.
- A per-group size limit; at the limit, writes are rejected with `no_space`.

**Not in this card.** Superblock update logic (T1.1.2), the free set itself (T1.1.6). The growth trigger and extending the file's grid zone come with the free set's low-space threshold in T1.1.6, which takes the growth gates; `no_space` at the size limit with reopen and verify moves to T1.2.6, the first card with blocks, allocation and a checkpoint that records the grid.

**Depends on.** T0.3.2, T0.1.4, T0.2.2.

**Gates.**
- [ ] The offset and size of every zone are computed by one function, asserted at compile time to be sector-aligned, and match the table in document 01.
- [ ] `granite format` on an existing data file refuses and changes nothing (test comparing the file hash before and after).
- [ ] Growth is never triggered by time or by file-system free space: a lint rejects their names in the storage crate.
- [ ] A file formatted for cluster A is refused when opened as cluster B, or with a different group id or replica index, each with its own typed error.
- [ ] The format version is stored and a file with a higher version is refused.

## T1.1.2 Superblock: 4 copies, quorum open, atomic update

**Goal.** The root of the data file can be updated atomically and read back correctly after a crash at any instant or the corruption of some copies.

**Work.**
- The superblock record: identity, format version, VSR state, checkpoint references, each reference as (address, checksum).
- Four copies written in sequence, each with its own checksum and a sequence number.
- Open: choose the highest sequence number that has a valid quorum of copies; repair the other copies.
- Update as the atomic commit point of a checkpoint.

**Not in this card.** What a checkpoint puts into the superblock (T1.2.6).

**Depends on.** T1.1.1, T0.4.3.

**Gates.**
- [ ] Seed gate 10,000 with a crash injected during superblock updates: every open returns exactly the old state or the new state, never a mix and never a failure.
- [ ] A crash is injected between every pair of consecutive sector writes of an update at least once; a coverage report lists the positions.
- [ ] With any one copy corrupted, open succeeds and rewrites that copy (test for each of the four).
- [ ] With copies corrupted so that no sequence number has a quorum, open fails with a typed error and the file is not modified.
- [ ] A copy written at the wrong offset (misdirected write) is not accepted as a copy for its position (test).
- [ ] Updating to a sequence number that is not exactly the previous plus one is rejected by an assertion.
- [ ] The update without the quorum rule, as a deliberate defect, fails within 1,000 seeds (injected bug).
- [ ] Opening a data file whose stored format constants differ from the binary's is refused with a typed error naming the constant (`FormatMismatch` from `crates/base/src/constants.rs`) (from T0.1.4).

## T1.1.3 WAL: header ring and prepare ring, write path

**Goal.** The write-ahead log that makes a prepare durable, with a hash chain that makes any gap or substitution detectable.

**Work.**
- Two rings of 1024 slots: 256-byte headers and 1 MiB prepares. Slot index is op number modulo slot count.
- Write order: the prepare first, then its header in the header ring.
- Each header holds the checksum of its body, of itself, and of the previous prepare's header.
- Append API with completion, and a read API by op number that verifies checksums.

**Not in this card.** Recovery (T1.1.4), repair from peers (T2.2.4).

**Depends on.** T1.1.1, T0.2.2, T0.2.3.

**Gates.**
- [ ] Appending a prepare whose parent checksum does not match the previous header is rejected by an assertion (test).
- [ ] A trace of the I/O for 10,000 appends shows the prepare write completing before the header write is submitted, every time.
- [ ] The append completion is delivered only after both writes are durable (test using the simulated disk's acknowledgement points).
- [ ] After the ring wraps, a read of an overwritten op returns "not present", never the newer prepare (test across 3 wraps).
- [ ] Reading a slot whose body or header fails its checksum returns a typed error, never the bytes.
- [ ] The buffer passed to append is written without a copy (address comparison in a test).
- [ ] Zero heap allocations per append after startup.

## T1.1.4 WAL recovery with slot classification

**Goal.** After a crash, the log is read back and every slot is classified correctly, because confusing a torn write with corruption either loses committed data or blocks the group for nothing.

**Work.**
- At open, read both rings and classify each slot as clean, torn, corrupt or missing, using the redundant header.
- The classification rules written out as a table of (header ring state, prepare ring state) to result, checked against Alagappan et al., FAST 2018.
- Decide the recovered head, and whether the head is uncertain.
- Hand corrupt committed slots to the caller as "needs repair".

**Not in this card.** Fetching repairs from peers (T2.2.4), the `recovering_head` protocol state (T2.2.5).

**Depends on.** T1.1.3, T0.4.3.

**Gates.**
- [ ] The classification table is in document 01 and each row has a unit test that constructs that exact on-disk state.
- [ ] A written note maps each rule to the paper's corresponding case and lists any difference; the review finding in document 01 section 9 is closed.
- [ ] Seed gate 10,000 with crashes during appends and no other fault: recovery never classifies a slot as corrupt, and never loses a prepare whose append had completed.
- [ ] Seed gate 10,000 with bit flips and no crashes: recovery never classifies a damaged slot as torn.
- [ ] A slot classified as torn is only ever the head or beyond it; an assertion enforces it.
- [ ] Swapping the two results in the table, as a deliberate defect, fails within 1,000 seeds (injected bug).
- [ ] Recovery reads each ring once; for 1024 slots it completes in under 2 seconds on real hardware (report).

## T1.1.5 Grid: block read, write, checksum, address verification

**Goal.** Fixed-size blocks that are never trusted without a check stored outside the block, so bit rot and misdirected I/O are caught before they reach a reader.

**Work.**
- Blocks of 128 KiB addressed by a 64-bit index. A reference is always (address, checksum).
- A block header with its own address and block type; the checksum covers the header and content.
- Read verifies checksum, address and type against the reference; write computes them.
- A failed read is reported as "needs repair" with the expected (address, checksum).

**Not in this card.** Allocation (T1.1.6), caching (T1.1.7), repair from peers (T2.2.6).

**Depends on.** T1.1.1, T0.2.1.

**Gates.**
- [ ] There is no API to read a block by address alone; every read takes the expected checksum (enforced by the type signature).
- [ ] A block written to address A and read through a reference to address B fails verification, even when the content is valid (test).
- [ ] Any single bit flip in a block is detected: tested for 100,000 random positions and for every bit of the block header.
- [ ] A read that returns a different valid block (misdirected read) is detected (simulated-disk test).
- [ ] A block of the wrong type for the reference is rejected.
- [ ] No verified read ever returns bytes that failed a check; a fuzz target over corrupted blocks runs fuzz gate 2 with an assertion on this.
- [ ] One block read performs exactly one device read and zero copies after it (counter and address check).

## T1.1.6 Free set with checkpoint-deferred release

**Goal.** Block allocation that is deterministic across replicas and that always lets a crashed replica fall back to its previous checkpoint.

**Work.**
- A bitset of allocated blocks, persisted to grid blocks at checkpoint.
- Deterministic allocation: the same sequence of acquire and release calls gives the same addresses.
- Blocks released during a checkpoint interval become reusable only after the next checkpoint is durable.
- Reservations, so compaction can claim its worst-case blocks up front in a fixed order.
- The low-space threshold that triggers grid growth, and the growth itself: one step of `granite_storage::growth::GridSize`, extending the data file's grid zone through a resize in `crates/io` (from T1.1.1, which built the step and the limit).

**Not in this card.** The checkpoint sequence (T1.2.6).

**Depends on.** T1.1.5.

**Gates.**
- [ ] Model test against a simple reference set: no block is allocated twice, and no released block is handed out before the checkpoint after its release is durable.
- [ ] Seed gate 10,000 with crashes at any point: after recovery, the set of allocated blocks equals the set reachable from the recovered checkpoint plus those allocated by replayed ops. No leak, no double allocation.
- [ ] Two instances given the same sequence of calls return identical addresses for 1,000,000 calls.
- [ ] Releasing a block immediately, as a deliberate defect, fails the crash test within 1,000 seeds (injected bug).
- [ ] Encode then decode of the persisted form returns the same set for 10,000 random sets, including empty and full.
- [ ] Acquire on a full set returns a typed "no space" result and never an invalid address.
- [ ] Find-free on a 1 TiB grid at 90% occupancy takes under 1 microsecond on average on real hardware (report).
- [ ] Two instances fed the same 100,000 ops grow their grids at the same op number, for 100 random workloads (from T1.1.1).
- [ ] Growth is never triggered by time, by file-system free space or by read load: a test varies all three (from T1.1.1, which has the lint).

## T1.1.7 Block cache

**Goal.** A fixed-size cache of verified blocks, so repeated reads of index and filter blocks do not touch the device.

**Work.**
- A set-associative cache keyed by block address, with capacity fixed at startup, one per shard.
- A cached block is associated with the checksum it was verified against; a lookup with a different checksum is a miss.
- An eviction policy (CLOCK or similar) with no allocation.
- Metrics: hits, misses, evictions, hit rate.
- Invalidation when a block address is released and reused.

**Not in this card.** The row cache (T1.3.9) and filter cache (T1.3.6).

**Depends on.** T1.1.5.

**Gates.**
- [ ] Memory use equals the configured capacity and does not change over 10,000,000 lookups with random keys (measured).
- [ ] Model test: a lookup never returns a block whose checksum differs from the one requested.
- [ ] After an address is released, reused for new content and read, the old content is never returned (test).
- [ ] Hit rate on a Zipfian workload (parameter 0.99) with a cache of 10% of the key space is at least 60% (test); the measured value is recorded.
- [ ] Hit-rate, hit, miss and eviction counters are exposed and checked in a test against a known sequence.
- [ ] A hit costs under 100 ns on real hardware (report).
- [ ] Zero heap allocations per operation after startup.

## T1.2.1 Memtable with arena and fixed capacity

**Goal.** The in-memory sorted buffer that receives writes before they are flushed to disk, with a hard memory bound.

**Work.**
- A sorted, row-oriented structure (skip list or B-tree) over an arena of fixed capacity.
- Insert, point lookup, ordered iteration from a key, and a "full" signal.
- Two memtables per tree: one mutable, one immutable while it is flushed.
- Iteration order that is identical on every replica for the same inserts.

**Not in this card.** Flush (T1.2.3), rebuild by WAL replay (T1.2.6).

**Depends on.** T0.2.3, T1.4.1 (key encoding; a byte-string key is enough to start).

**Gates.**
- [ ] Model test against `BTreeMap` for inserts, lookups and range iteration, including duplicate keys and empty ranges.
- [ ] The arena never exceeds its configured size; an insert that does not fit returns "full" and leaves the memtable unchanged (test).
- [ ] Zero heap allocations after construction over 1,000,000 inserts (counting allocator).
- [ ] The structure uses no randomness that differs between replicas: a skip list draws its levels from a generator seeded by a fixed constant, and two instances given the same inserts have identical memory contents (byte comparison).
- [ ] Iteration is stable while the memtable is frozen and a new mutable one receives writes (test).
- [ ] Insert and lookup throughput for 1 KiB rows is recorded in a report on real hardware.
- [ ] The crate's tests pass under Miri.

## T1.2.2 Manifest and levels

**Goal.** The record of which parts exist in which level of which tree, persisted so the shard can find its data again.

**Work.**
- The manifest: for every tree, the list of parts per level, each with key range, timestamp range, size and (address, checksum) of its header.
- Level 0 may overlap; levels 1 and deeper never do.
- Manifest changes logged as a chain of grid blocks, compacted when the chain grows.
- Load at open from the superblock's manifest root.
- Lookup: parts that may contain a key or overlap a range, newest first.

**Not in this card.** Deciding what to compact (T1.2.4).

**Depends on.** T1.1.5, T1.1.6.

**Gates.**
- [ ] Model test: after any sequence of add and remove operations, a reload from disk gives a manifest equal to the in-memory one.
- [ ] An assertion rejects any change that would make two parts overlap in a level deeper than 0 (test).
- [ ] Seed gate 10,000 with crashes: the manifest loaded after recovery equals the one at the last durable checkpoint.
- [ ] Two instances applying the same changes produce byte-identical manifest blocks.
- [ ] The manifest log is compacted; its size stays under a stated bound relative to the number of live parts over 1,000,000 changes (test).
- [ ] Lookup of the parts for a key in a tree with 10,000 parts across 7 levels inspects at most one part per level deeper than 0 (counter in a test).
- [ ] A manifest block that fails verification is reported for repair and is never partially applied.

## T1.2.3 Flush memtable to a level-0 part

**Goal.** Turning a full memtable into an immutable part on disk, without stalling the shard thread.

**Work.**
- Freeze the mutable memtable, write its rows in order as a part, add the part to level 0, drop the memtable.
- Flush triggered at fixed op intervals, never by time or memory pressure alone.
- Work sliced to the 1 ms rule.
- Block allocation in a deterministic order.

**Not in this card.** The part's internal format (T1.3.1); a minimal row-layout part is enough until then.

**Depends on.** T1.2.1, T1.2.2, T0.3.6.

**Gates.**
- [ ] Model test: after flush, reading through memtable plus parts returns exactly the model's content for every key and every range.
- [ ] Two instances fed the same ops flush at the same op number and write byte-identical blocks at identical addresses (100 random workloads).
- [ ] No task slice during a flush of a full memtable exceeds 1 ms; the stall metric shows p99.9 loop iteration under 5 ms in a benchmark on real hardware.
- [ ] Writes keep being accepted during a flush (test: inserts arriving mid-flush are visible afterwards).
- [ ] Seed gate 10,000 with a crash during flush: after recovery no row is lost, none is duplicated, and no block is leaked.
- [ ] An empty memtable produces no part.

## T1.2.4 Compaction with op-number pacing

**Goal.** Compaction that keeps reads fast and space bounded, and that performs exactly the same steps on every replica, which is what makes storage byte-identical.

**Work.**
- Merge a part from level N with the overlapping parts of level N+1; levels grow by a factor of 8.
- Pacing: every committed op grants a fixed quota of compaction work. No timers, no load feedback.
- Deterministic choice of what to compact next.
- Worst-case provisioning: the quota is always enough to keep every level within its size bound.
- Block reservations taken up front in a fixed order.

**Not in this card.** Dropping old versions (T3.1.2), heavy codecs (T6.1.1).

**Depends on.** T1.2.3, T1.1.6.

**Gates.**
- [ ] Two instances fed the same 1,000,000 ops have byte-identical data files at every checkpoint, for 100 random workloads. The comparison is a whole-file hash.
- [ ] The same holds when the two instances run with different simulated disk latencies and different read loads.
- [ ] Model test: content read back equals the model after any amount of compaction.
- [ ] No level exceeds its size bound at any checkpoint under a worst-case workload (uniform random keys, maximum row size) for 10,000,000 ops; a written argument for the quota accompanies the test.
- [ ] No compaction task slice exceeds 1 ms; p99.9 loop iteration stays under 5 ms on real hardware.
- [ ] Seed gate 10,000 with crashes during compaction: no row lost or duplicated, no block leaked.
- [ ] Write amplification for the uniform workload is measured and recorded in a report.
- [ ] Making the choice of the next part depend on wall-clock time, as a deliberate defect, fails the byte-identity test (injected bug).

## T1.2.5 Merge iterators and range scans

**Goal.** One sorted view over memtables and all parts of a tree, which every read and every compaction is built on.

**Work.**
- A k-way merge iterator over memtables and parts, newest source first on equal keys.
- Seek to a key; forward iteration; bounded ranges.
- Pruning of parts whose key range or timestamp range cannot match.
- A snapshot of the manifest pinned for the iterator's lifetime, so a concurrent flush or compaction does not disturb it.

**Not in this card.** Version resolution for versioned tables (T3.1.1), skip indexes (T6.1.3).

**Depends on.** T1.2.2, T1.2.3.

**Gates.**
- [ ] Model test against `BTreeMap`: every range scan returns the same rows in the same order, across flushes and compactions that happen during the scan.
- [ ] A scan pinned before a compaction still returns the pre-compaction view and the blocks it reads are not reused until it ends (test).
- [ ] Empty ranges, single-key ranges, ranges before the first key and after the last key each have a test.
- [ ] A part whose key range does not intersect the scan range is never opened (counter).
- [ ] The iterator performs zero heap allocations per row after it is created.
- [ ] A scan yields within 1 ms; a 10,000,000-row scan never produces a loop iteration over 5 ms on real hardware.

## T1.2.6 Checkpoint and replay

**Goal.** A shard can stop at any instant and come back to exactly the state it had acknowledged.

**Work.**
- Checkpoint every fixed number of ops: persist manifest changes and the free set, then write the superblock, then make released blocks reusable.
- Recovery: open the superblock, load manifest and free set, replay WAL prepares after the checkpoint op to rebuild the memtables.
- Checkpoint writes sliced to the 1 ms rule.
- The checkpoint interval bounded by WAL size minus pipeline minus one compaction interval.

**Not in this card.** Coordinating checkpoints across replicas (T2.2.7).

**Depends on.** T1.1.2, T1.1.4, T1.2.4.

**Gates.**
- [ ] Seed gate 100,000 with crashes at any I/O boundary and torn writes: after reopen, the state equals the reference model for every acknowledged op, checked by reading every key.
- [ ] A crash is injected at every distinct I/O site of the checkpoint sequence at least once (coverage report).
- [ ] Replay is deterministic: recovering the same file twice gives byte-identical memory state for the memtables.
- [ ] The WAL never wraps over a prepare that is not covered by a durable checkpoint; an assertion enforces it and a test with a slow checkpoint shows writes being held back instead.
- [ ] Writing the superblock before the manifest blocks are durable, as a deliberate defect, fails within 1,000 seeds (injected bug).
- [ ] Recovery time for a full WAL of 1 KiB-row prepares is measured on real hardware and recorded.
- [ ] No checkpoint task slice exceeds 1 ms.
- [ ] At the grid's size limit, the next write that needs a block returns `no_space`; reads keep working; nothing is corrupted (test, then reopen and verify) (from T1.1.1).

## T1.3.10 Relocatable parts: part-relative block references and extent map

**Goal.** A part can be copied into another group's grid at different addresses and still be read, which the tablet move in phase 4b depends on. It is built now because it changes the part format.

**Work.**
- Blocks inside a part refer to each other by index within the part.
- The part header holds the extent map from index to grid address.
- Each block identifies itself by (part id, index), covered by its checksum.
- A manifest entry may carry a visible key range that clips the part; rows outside it are invisible.

**Not in this card.** The move protocol (T4.4.3).

**Depends on.** T1.3.1 (designed together; this card owns the addressing rules).

**Gates.**
- [ ] A part copied block by block to different addresses, with only its header rewritten, returns identical results for every point read and every scan (model test over 1,000 random parts).
- [ ] A block from part A placed where part B expects a block fails verification, even with the same index (test).
- [ ] No block other than the part header contains a grid address (enforced by the block types; checked by a test that scans block contents for the part's own addresses).
- [ ] With a clip range set, point reads and scans return only rows inside it; rows outside are never returned (model test).
- [ ] Compacting a clipped part writes only the visible rows.
- [ ] The rules are written in document 01 with a statement of how they preserve the byte-identity invariant, and the open point in document 04 section 8 is closed.

## T1.3.1 Part format: header, key index, column groups, per-group ordinal index, chunks

**Goal.** The on-disk format of a part, supporting all three declared layouts in one engine.

**Work.**
- Part header block: schema version, row count, key range, timestamp range, level, per-column statistics, references to the other blocks.
- Key index: sparse, first key of each key chunk to row ordinal.
- Column groups: `row` (one group), `columnar` (one group per column), `groups(...)` (explicit).
- Per-group ordinal index: ordinal to (block reference, offset, length, chunk checksum).
- Chunks sized by a byte target (about 32 KiB for multi-column groups; up to 8192 rows for single-column groups), each independently decodable, each column encoded contiguously inside it.
- Hidden column `_ts` on every table.

**Not in this card.** Codecs beyond plain (T1.3.2), nullable and variable-length chunks (T1.3.3), filters (T1.3.4).

**Depends on.** T1.2.3, T1.4.1, T1.4.3.

**Gates.**
- [ ] The format is specified in document 01 field by field, with a version number, and a test parses a committed golden part file for each layout.
- [ ] Round-trip test for `row`, `columnar` and `groups` layouts: write random rows for random schemas of 1 to 100 columns, read back identical rows (1,000 cases each).
- [ ] Fuzz gate 4 on the part reader with corrupted blocks: no panic, no out-of-bounds read, every failure a typed error.
- [ ] No chunk of a multi-column group exceeds twice its byte target; no single-column chunk exceeds 8192 rows (assertion and test).
- [ ] Every chunk decodes without reading any other chunk (test that zeroes all other chunks first).
- [ ] The same rows written twice produce byte-identical parts.
- [ ] A part written with an older schema version is readable after a column is added (test with a hand-built older part).
- [ ] A reader given a part with an unknown format version refuses it with a typed error.

## T1.3.8 Partial block reads verified by chunk checksum

**Goal.** A point read fetches only the bytes of the chunk it needs instead of a whole 128 KiB block, without giving up verification.

**Work.**
- Use the chunk checksum stored in the ordinal index to verify a sector-aligned byte range read on its own.
- The read path for one chunk: compute the sector range, read it, verify, decode.
- A chunk that fails verification triggers a full-block read and, if that fails, block repair.
- Block-level checksums stay in use for scrubbing and repair.

**Not in this card.** Repair from peers (T2.2.6); here a failure is reported as "needs repair".

**Depends on.** T1.3.1, T1.1.5.

**Gates.**
- [ ] A point read of one row in a `row`-layout part issues device reads covering only the chunk's sector range; the byte count read is at most the chunk length rounded up to sectors plus one sector (counter in a test).
- [ ] A bit flip inside the chunk is detected by the chunk checksum (10,000 random positions).
- [ ] A bit flip in the same block but outside the chunk does not fail the partial read, and is found by a full-block verification (test).
- [ ] A chunk that fails verification causes the block to be reported for repair and the read to return a typed error, never data.
- [ ] No unverified byte is ever handed to a decoder (fuzz gate 2 with an assertion).
- [ ] Bytes read per point lookup are recorded for the three layouts in a report.

## T1.3.9 Row cache for latest versions

**Goal.** Hot rows, such as balances, are served from memory without touching parts.

**Work.**
- A fixed-size cache of decoded latest rows keyed by (table, primary key), one per shard.
- Updated or invalidated by writes on the same thread; no locking.
- Negative entries for keys known to be absent, if the experiment shows they help uniqueness checks.
- Metrics: hits, misses, evictions.

**Not in this card.** Caching of older versions; the cache holds only the latest.

**Depends on.** T1.3.5.

**Gates.**
- [ ] Model test with random interleaved writes, reads, flushes and compactions: a read through the cache never returns a value different from a read with the cache disabled.
- [ ] Memory use equals the configured capacity and stays constant over 10,000,000 operations.
- [ ] Removing the invalidation on write, as a deliberate defect, fails the model test (injected bug).
- [ ] A snapshot read at a timestamp older than the cached version does not use the cache entry (test).
- [ ] The cache is not consulted by, and does not change, the state machine's results: two instances with different cache sizes produce byte-identical data files.
- [ ] Hit rate and point-read latency with and without the cache, on a Zipfian workload, are recorded in a report.

## T1.3.2 Lightweight codecs: plain, bit-packing with frame of reference, dictionary

**Goal.** Column encodings for the shallow levels that compress well and still allow reading one value without decoding the chunk.

**Work.**
- Codecs: plain, fixed-width, frame of reference with bit-packing, dictionary.
- Random access to the value at a row index for every codec.
- A deterministic rule that picks the codec for a chunk from the data itself.
- A scalar implementation of each; accelerated versions are later work.

**Not in this card.** Delta, RLE, string and general-purpose compression (T6.1.1).

**Depends on.** T1.3.1, T1.4.1.

**Gates.**
- [ ] Round-trip property test for every codec and every fixed-width type, including minimum, maximum, all-equal and empty chunks.
- [ ] Fuzz gate 4 per decoder on arbitrary bytes: no panic, no out-of-bounds access.
- [ ] Reading the value at a random index returns the same value as full decode, for every codec (property test), and touches a number of bytes independent of the chunk length (counter).
- [ ] The codec choice is a pure function of the chunk's values: the same values always give the same codec and the same bytes, on Linux x86-64 and macOS ARM (golden files in CI).
- [ ] The codec id and its parameters are stored in the chunk header with a version.
- [ ] Encode and decode throughput and compression ratio on three reference data sets are recorded in a report.

## T1.3.3 Variable-length and nullable column chunks

**Goal.** Strings, byte arrays and nullable columns stored in the same chunked format.

**Work.**
- Variable-length columns as an offsets chunk plus a bytes chunk.
- Nullable columns as a validity bitmap chunk next to the value chunk.
- Random access to one value by row index.
- Length limits from the schema enforced at encode time.

**Not in this card.** String compression (T6.1.1).

**Depends on.** T1.3.2.

**Gates.**
- [ ] Round-trip fuzz gate 4 for variable-length chunks and for nullable chunks of every fixed-width type.
- [ ] Decoder fuzz gate 4 on arbitrary bytes: offsets that are not monotonic, that point outside the bytes chunk, or that overflow are rejected with a typed error.
- [ ] Edge cases each have a test: empty string, all nulls, no nulls, a value of exactly the maximum length, a value one byte over (rejected).
- [ ] A null value's slot in the value chunk is always zero bytes, so identical logical data gives identical bytes (test comparing two encodings built differently).
- [ ] UTF-8 is validated before encoding; invalid input is rejected and never stored.
- [ ] Reading one value by index does not scan preceding values (counter).

## T1.3.4 Primary-key filter (bloom or ribbon)

**Goal.** A compact filter per part that lets a point read skip parts that cannot contain the key.

**Work.**
- Implement a split-block bloom filter and evaluate a ribbon filter as the alternative.
- About 10 bits per key; the hash is fixed and part of the format.
- Build during flush and compaction, deterministically.
- Query without allocation.

**Not in this card.** Caching of filter blocks (T1.3.6).

**Depends on.** T1.3.1.

**Gates.**
- [ ] Zero false negatives over 100,000,000 inserted keys in a test.
- [ ] The measured false-positive rate at 10 bits per key is at or below 1.0%, for both random keys and sequential time-ordered ids; the measured value is recorded.
- [ ] A report compares the two filter kinds on false-positive rate, bits per key, build time and query time, and the choice is recorded in `docs/decisions/`.
- [ ] The same keys produce a byte-identical filter on Linux x86-64 and macOS ARM (golden file).
- [ ] A query costs at most one cache-line-sized read of the filter block (by construction, with a test on the access pattern).
- [ ] The filter's hash function, seed and layout are specified in document 01 with a version.

## T1.3.5 Point-read path through parts

**Goal.** The complete lookup of one row by primary key, which is the operation transactions perform most.

**Work.**
- The read path of document 01 section 4: memtables, row cache, then each level newest first: key-range check, filter, key index, key chunk, then the chunk of each requested group.
- All chunk reads of one lookup, and all lookups of one batch, submitted to the device together.
- Stop at the first hit.
- Uniqueness check for inserts as a special case that prunes on key range for time-ordered ids.

**Not in this card.** Snapshot resolution for versioned tables (T3.1.1).

**Depends on.** T1.3.1, T1.3.4, T1.3.8, T1.2.5.

**Gates.**
- [ ] Model test: every point read returns the model's value, for present and absent keys, across flushes and compactions.
- [ ] With filters and index blocks cached, a hit in a `row`-layout part at level L performs exactly one device read; in a `columnar` part, one per requested column, all submitted in one batch (counters).
- [ ] An insert of a time-ordered id newer than every part's key range performs zero device reads and zero filter probes (counter).
- [ ] A batch of 1,000 lookups submits its reads together: the number of submission calls is bounded by a constant, not by 1,000 (counter).
- [ ] A report gives point-read latency (median, p99, p99.9) by level depth 0 to 6 for each layout on real hardware, with a cold block cache and a warm one.
- [ ] No lookup task slice exceeds 1 ms.

## T1.3.6 Filter cache with its own memory budget

**Goal.** Filter blocks are kept in memory within a separate budget, so data blocks cannot push them out and make every lookup pay an extra read.

**Work.**
- A cache for filter blocks with its own fixed capacity, separate from the block cache.
- A metric counting reads that had to fetch an uncached filter.
- A sizing rule in the documentation: bits per key times keys per shard against the budget.

**Not in this card.** Changing the filter format.

**Depends on.** T1.3.4, T1.1.7.

**Gates.**
- [ ] The filter cache's memory equals its configured budget and is unaffected by block-cache pressure: a test fills the block cache with data blocks and shows no filter eviction.
- [ ] A lookup that needs an uncached filter increments the counter exactly once and performs exactly one extra device read (test).
- [ ] A report gives the filter memory needed for 1, 10 and 100 billion rows per shard at 10 bits per key, and the point-read latency when the budget covers 100%, 50% and 10% of filters.
- [ ] The open point "filter memory at scale" in document 01 section 9 is closed with a stated recommendation.
- [ ] Two instances with different filter-cache sizes produce byte-identical data files.

## T1.3.7 G1 experiment and written decision

**Goal.** Answer gate G1 with measurements: which layout meets point-read and write-amplification targets for wide tables, and whether the columnar projection must move earlier than phase 6.

**Work.**
- A benchmark with tables of 20, 50 and 100 columns at about 1 KiB per row, loaded to at least 5 levels deep.
- Point reads uniformly distributed over data of all ages, whole-row and 3-column.
- The three layouts: `row`, `columnar`, and a two-group layout (5 hot columns, the rest cold).
- Measurements: point-read latency, write amplification, space on disk, scan throughput for a 3-column scan.
- A written decision with recommended layouts per workload and chunk byte targets.

**Not in this card.** Implementing the columnar projection (T6.1.4).

**Depends on.** T1.3.5, T1.3.6, T1.3.9, T1.2.4, T1.5.5. One production-like server.

**Gates.**
- [ ] Every number is measured on real hardware; the server's CPU, memory, drive model and kernel are in the report.
- [ ] The report contains all 9 combinations (3 widths, 3 layouts), each with: point-read latency median, p99 and p99.9 for whole-row and for 3-column reads, write amplification, bytes on disk per row, and scan rows per second.
- [ ] Each data set is at least 10 times larger than the block cache, and the data file reaches at least level 5, both stated with evidence.
- [ ] Each measurement is repeated 3 times and the spread is reported.
- [ ] Proposed thresholds, to be confirmed before the experiment runs: whole-row point read p99 under 1 ms from the deepest level with caches cold for data blocks; write amplification under 30. The report states pass or fail per combination.
- [ ] The decision document states the recommended layout for the exchange schema's four tables, the chunk byte target, and a yes or no on moving the columnar projection earlier, each with the numbers behind it.
- [ ] `DESIGN.md` and document 01 are updated: "Storage engine" changes from "proposed" to "confirmed" or the changes are listed.
- [ ] The benchmark is committed and reruns with one `just` recipe.

## T1.4.1 Type system and order-preserving key encoding

**Goal.** The value types of the data model, and a byte encoding of keys whose byte order equals the logical order, so indexes compare bytes.

**Work.**
- The types of document 03 section 2 except the composite ones: integers to 256 bits, Decimal, floats, bool, Date, Timestamp, Duration, Uuid, Id128, String, Bytes, FixedBytes, Enum8/16, Nullable, LowCardinality.
- Order-preserving encoding for every type allowed in a key, and for composite keys, including descending order.
- Typed in-memory representation and the fixed-width storage representation.
- NaN canonicalisation for floats; floats and nullable types rejected in keys.

**Not in this card.** Arithmetic (T1.4.2), composite types (T6.3.5).

**Depends on.** T0.1.4.

**Gates.**
- [ ] Property test for every key type and for composite keys of up to 8 columns: for 1,000,000 random pairs, the byte comparison of the encodings equals the logical comparison of the values.
- [ ] Decode of encode returns the original value for every type (property test), including minimum, maximum and zero.
- [ ] The descending encoding reverses the order exactly (property test).
- [ ] No encoded key is a prefix of a different encoded key when that would change the order (property test on variable-length types).
- [ ] An encoded primary key over 512 bytes is rejected with a typed error.
- [ ] Encodings are identical on Linux x86-64 and macOS ARM (golden vectors for every type).
- [ ] Every NaN bit pattern encodes to the one canonical NaN (test over all special values and 1,000,000 random patterns).
- [ ] The encoding of each type is specified byte by byte in document 03 and marked as part of the format.

## T1.4.2 Checked arithmetic and casts for integers and decimals

**Goal.** Arithmetic that never wraps, truncates or rounds silently; every failure is a typed error.

**Work.**
- Add, subtract, multiply, divide, remainder, negate and compare for all integer widths and for Decimal with precision up to 76.
- Decimal scale rules: the result scale of each operation is fixed and documented; no implicit rescale.
- Explicit casts between all numeric types, failing on any loss.
- Error codes: overflow, division by zero, loss of precision.

**Not in this card.** Floating-point reductions (T6.2.3).

**Depends on.** T1.4.1.

**Gates.**
- [ ] For 8-bit and 16-bit types, every operation is tested exhaustively over all input pairs against a wide-integer reference.
- [ ] For wider types, property tests of 10,000,000 cases per operation compare with an arbitrary-precision reference, with inputs weighted toward the boundaries.
- [ ] Every operation at the minimum and maximum of every type has an explicit test.
- [ ] A lint forbids wrapping, saturating and unchecked arithmetic methods and the `as` operator on numeric types in `crates/types`, `crates/txn` and `crates/shard`.
- [ ] Every cast that would lose information returns the typed error; a table of all type pairs is tested.
- [ ] Decimal results are identical on Linux x86-64 and macOS ARM (golden vectors).
- [ ] Overflow checks stay enabled in release builds (test that runs in the release profile).

## T1.4.3 Schema catalog: table groups, tables, validation, static row bound

**Goal.** The in-memory and persisted description of a cluster's tables, and the rules that reject a schema that cannot work.

**Work.**
- Catalog objects: table group (partition key, hash or range), table (kind, columns, primary key, layout, options), with a schema version.
- Validation: the partition key is a prefix of every primary key in the group; layout is declared; no nullable or float key columns; limits of document 03 section 8.
- The static upper bound of a row's encoded size, computed from the schema.
- Binary encoding of the catalog, versioned.
- A closed set of error codes, one per rule.

**Not in this card.** Text syntax (T1.4.5), indexes and derived tables (phase 3), schema changes (T3.5.1).

**Depends on.** T1.4.1.

**Gates.**
- [ ] Every validation rule has a test with a schema that breaks only that rule and is rejected with that rule's code; the list of rules and codes is a table in document 03.
- [ ] A table without a declared layout is rejected.
- [ ] A schema whose static row bound exceeds 64 KiB, or does not fit a message with headroom, is rejected.
- [ ] Property test: for random valid schemas and random valid rows, the encoded row never exceeds the computed static bound.
- [ ] The limits (1024 columns, 4096 tables, 512-byte keys) are each tested at the limit (accepted) and one over (rejected).
- [ ] Fuzz gate 4 on the catalog decoder.
- [ ] Encoding a catalog is deterministic: the same catalog always gives the same bytes.

## T1.4.4 Row batch wire format and validator

**Goal.** The body format for writes and reads: readable without copying, and validated completely before it reaches the state machine.

**Work.**
- The batch layout `[count][rows][heap]`: a fixed-size row struct for fixed-width columns, then (offset, length) slots for variable-length columns pointing into the heap.
- A validator run once on arrival: lengths, offsets inside the heap, UTF-8, enum values, reserved bits, row count against the message size.
- Zero-copy accessors over a validated batch.
- The sparse result list for writes: (event index, result code).

**Not in this card.** Column batches for scan results (T6.3.4 extends them); a simple row-batch reply is enough for phase 1.

**Depends on.** T1.4.3, T0.2.2.

**Gates.**
- [ ] Fuzz gate 8 on the validator with arbitrary bytes and arbitrary schemas: no panic; every input is either accepted or rejected with a typed error.
- [ ] Property: any batch the validator accepts can be read through the accessors without a bounds failure (fuzz target with the accessors called on every accepted input).
- [ ] Reading a validated batch performs zero copies and zero allocations (address check and counting allocator).
- [ ] Overlapping heap slots, an offset beyond the heap, invalid UTF-8, an unknown enum value and a non-zero reserved bit are each rejected with their own code (one test each).
- [ ] A malformed batch is rejected whole; no event of it is applied (test).
- [ ] The layout is specified in document 05 with offsets, and a golden batch file for the exchange schema's `entry` table parses in CI.
- [ ] An empty result list means every event succeeded, and a test asserts that reading.

## T1.4.5 Schema text syntax and compiler in client tooling

**Goal.** Schemas are written as text files and compiled to the binary catalog by a tool, as decided in the data-model review.

**Work.**
- A grammar for table groups and tables, as used in document 03 section 10.
- A compiler in the client tooling: parse, validate with the catalog rules, emit the binary form.
- Error messages with file, line and column.
- A formatter that prints a catalog back as canonical text.
- The exchange reference schema's tables, without derived tables and programs, as the first example.

**Not in this card.** Programs (T3.3.4), derived tables (T3.4.1), code generation (T3.5.2).

**Depends on.** T1.4.3.

**Gates.**
- [ ] The grammar is written in the documentation, and the example schemas under `schemas/` compile to the binary form in CI.
- [ ] Round trip: compile, print as text, compile again gives a byte-identical binary catalog (property test over random valid schemas).
- [ ] Every catalog validation rule surfaces as a compiler error that names the file, line, column and rule (one test per rule).
- [ ] Fuzz gate 4 on the parser: no panic and no hang on arbitrary text; parse time is bounded for inputs up to 1 MiB.
- [ ] The compiler has no access to the network or the clock; its output is a function of its input files only (test that runs it twice and compares bytes).
- [ ] The server never parses text; only the binary form crosses the wire (checked by the absence of the parser crate in `cargo tree -p granite`).

## T1.5.1 Operations: insert into immutable tables with uniqueness check

**Goal.** The first state-machine operation: a batch of inserts into an immutable table, with exact, retry-safe duplicate handling.

**Work.**
- The `insert` operation: validate, check uniqueness of each primary key, write to the table's tree.
- Result per event: success, `exists_identical` when a row with the same key and identical content exists, `exists_different` when the content differs, or a typed validation error.
- Results returned as a sparse list.
- Each row stamped with its commit timestamp `_ts`.
- Duplicates within one batch handled by the same rule.

**Not in this card.** Versioned tables (T3.1.3), conditions (T3.2.1), derived tables (T3.4.1).

**Depends on.** T1.3.5, T1.4.4, T1.2.6.

**Gates.**
- [ ] Model test against the reference model: every result code and the final table content match for random batches including duplicates within a batch and across batches.
- [ ] Re-sending an identical batch returns `exists_identical` for every event and changes no stored byte (data-file hash equal before and after, ignoring the WAL).
- [ ] A row with an existing key and one differing column returns `exists_different` and leaves the stored row unchanged (test for a difference in each column position).
- [ ] The operation is a pure function of (state, op number, timestamp, body): running it on two instances gives identical results and byte-identical data files.
- [ ] One failing event does not affect the others in the batch (test).
- [ ] The result codes are a closed enum documented in document 03 and are part of the protocol version.
- [ ] No execution slice exceeds 1 ms for a full 1 MiB batch.

## T1.5.2 Operations: get and scan

**Goal.** Reading rows back: by primary key, and by key range within one partition.

**Work.**
- `get`: a batch of primary keys and a column list; returns the rows found.
- `scan`: a key range within a partition, a limit, a column list; returns rows in key order.
- Only the requested columns are read from disk and returned.
- Results larger than one message continue with a continuation token.

**Not in this card.** Snapshot reads (T3.1.1), analytical scans (phase 6), cross-partition scans.

**Depends on.** T1.5.1, T1.2.5.

**Gates.**
- [ ] Model test: `get` and `scan` results equal the reference model for random keys, ranges, limits and column lists, including absent keys and empty ranges.
- [ ] A scan with a limit returns exactly the first `limit` rows in key order.
- [ ] A scan whose result exceeds one message returns a token, and following the tokens yields every row exactly once, in order, even when inserts happen between pages at later timestamps (test).
- [ ] A `get` for 3 of 50 columns on a `columnar` table reads only those 3 column groups from the device (counter).
- [ ] A scan range that spans two partitions is rejected with a typed error.
- [ ] Reads never change stored state: the data-file hash is equal before and after 1,000,000 reads.
- [ ] No read slice exceeds 1 ms.

## T1.5.3 Reference model in `sim`

**Goal.** A small, obviously correct in-memory implementation of the data model, which every model test and checker compares the real engine against.

**Work.**
- An in-memory model of tables and the operations of E1.5, written for clarity, not speed, sharing no code with the engine beyond the type definitions.
- A generator of random valid and invalid workloads for a given schema.
- A comparison harness: apply the same ops to engine and model, compare every result and, on demand, the full table content.

**Not in this card.** Linearizability checking of concurrent histories (T2.3.2).

**Depends on.** T1.4.3, T1.4.4.

**Gates.**
- [ ] The model covers insert, get and scan with all result codes of T1.5.1 and T1.5.2.
- [ ] The model imports nothing from `crates/storage`, `crates/lsm`, `crates/columnar` or `crates/shard` (dependency check in CI).
- [ ] Three deliberate defects in the engine are each caught by the harness: a lost row after compaction, a wrong duplicate result code, a scan that skips the last row (three injected bugs).
- [ ] The workload generator produces every result code at least once per 10,000 operations (coverage counter in a test).
- [ ] The model is under 1,500 lines for phase 1, and has its own unit tests for each operation.
- [ ] Model and engine agree over seed gate 10,000 with crashes and storage faults enabled.

## T1.5.4 Single-shard server binary and minimal Rust client

**Goal.** The first end-to-end path: a client process inserts and reads rows from a running server over TCP.

**Work.**
- `granite format` and `granite start` for one shard without replication.
- The request path: receive, validate, append to the WAL, execute, reply.
- A minimal Rust client: connect, insert, get, scan, with typed result codes.
- Graceful shutdown and restart.

**Not in this card.** Sessions and exactly-once retries (T2.2.9), routing (T4.3.2), TLS (T3.5.4).

**Depends on.** T1.5.1, T1.5.2, T0.3.3, T0.3.5.

**Gates.**
- [ ] An integration test starts a server, inserts 1,000,000 rows from a client over TCP, reads them all back by `get` and by `scan`, and compares with the input.
- [ ] The same test passes on Linux (io_uring backend) and macOS (portable backend).
- [ ] The server is killed with `SIGKILL` at a random moment during inserts, restarted, and every insert that was acknowledged to the client is present (100 iterations).
- [ ] A reply is sent only after the prepare is durable (test using the simulated backend's acknowledgement points).
- [ ] A malformed request closes the connection and leaves the server serving other clients (test).
- [ ] The whole server and client run in the simulator from one seed with the same code (seed gate 1,000 for the end-to-end path).
- [ ] A new developer can reproduce the test with two documented commands.

## T1.5.5 Benchmark harness and first tracked numbers

**Goal.** Performance is measured the same way from now on, so regressions are seen the day they appear.

**Work.**
- `granite bench`: workloads for insert throughput by batch size, point reads, and mixed read-write, with latency histograms.
- Results written as machine-readable files with commit hash and hardware description.
- A nightly job on fixed hardware that stores results and compares them with a baseline.
- The first experiment: 1 MiB against 4 MiB message size for 1 KiB rows.

**Not in this card.** The G1 layout experiment (T1.3.7), which uses this harness.

**Depends on.** T1.5.4. One production-like server for the stored numbers.

**Gates.**
- [ ] `just bench` runs the workloads and writes a result file containing commit hash, hardware, configuration, throughput and latency at median, p99 and p99.9.
- [ ] The harness corrects for coordinated omission: with an injected 100 ms server stall, the reported p99.9 reflects the stall (test).
- [ ] Three consecutive runs of the same commit on the fixed hardware differ by under 5% in throughput; the spread is recorded.
- [ ] The nightly job has stored results for 7 consecutive nights, and a deliberate 20% slowdown fails it (injected bug).
- [ ] A report compares 1 MiB and 4 MiB messages for 1 KiB rows on throughput, latency and memory, on real hardware, and the message-size decision in `DESIGN.md` is confirmed or changed.
- [ ] Every benchmark run fails if p99.9 loop iteration time exceeds 5 ms.
- [ ] The load generator runs on a different machine or on cores isolated from the server, and the report says which.
