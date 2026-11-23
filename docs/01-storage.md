# 01. Storage engine

One data file per shard. Everything a shard persists lives in it. All numeric constants here are initial values to be benchmarked in phase 1, and are fixed at cluster format time.

## 1. Fault model

The engine must tolerate, on any single replica:
- crash at any instant, including mid-write (torn writes),
- bit rot in any sector,
- misdirected writes and reads (right data, wrong place),
- lost writes that were acknowledged by the device,
- latent sector errors and slow I/O.

It does this by never trusting a block without verifying a checksum that is stored somewhere other than the block itself, and by repairing from other replicas. A replica that cannot repair stops rather than serving doubtful data.

## 2. Data file layout

Opened with `O_DIRECT | O_DSYNC`, sector size 4096. The fixed zones are preallocated at format time. The grid grows on demand in fixed steps (initially 1 GiB) up to a configured limit. Growth is triggered by the free set running low, which is a deterministic function of the ops applied, so all replicas grow at the same op. The limit is a per-group setting stored in the metadata group, identical for all replicas; at the limit the group rejects writes with `no_space` rather than diverging.

Encryption at rest is delegated to the operating system or volume (LUKS, encrypted cloud volumes). The database does not encrypt blocks.

| Zone | Contents | Size |
|---|---|---|
| Superblock | 4 copies of the root record | 4 x 64 KiB |
| WAL headers | Ring of prepare headers, one 256-byte header per slot | slots x 256 B |
| WAL prepares | Ring of full prepares | slots x 1 MiB |
| Grid | Fixed-size blocks, everything else | rest of file |

Client sessions and their last replies live in an internal LSM tree, not a dedicated zone.

The zones follow each other in the order of the table, with no gap, so the grid starts at a fixed offset and the file ends where the grid ends. `granite_storage::layout::zones` is the one function that computes every offset and size; the backends take their zone sizes from it, a compile-time assertion keeps every zone sector-aligned, and a test compares it with this table.

`granite format --cluster <id> --group <id> --replica <index> <path>` creates the data file. It refuses, changing nothing, if anything exists at the path. It writes the superblock zone (every copy holding the initial record below) and zeros over both WAL zones, so their space is allocated at format time and a later WAL write cannot fail for lack of room, then makes the file and its directory entry durable. The grid starts empty and gets its first step when the first block is needed.

The grid size is a whole number of growth steps, and so is the limit. When the free set runs low the grid grows by exactly one step; at the limit it cannot, and the write that needed a block gets `no_space`. The storage crate names neither the clock nor the file system's free space (`tools/lint.py`), so growth cannot depend on them.

Initial constants, to be tuned by benchmark in phase 1. They are format constants: stored in the superblock, and a data file whose stored values differ from the binary's is refused. Their home is `crates/base/src/constants.rs`; a test compares this table with it.

| Constant | Value |
|---|---|
| Sector size | 4 KiB |
| WAL slots | 1,024 |
| Message size, header included | 1 MiB |
| Grid block size | 128 KiB |
| Grid growth step | 1 GiB |
| Pipeline depth (document 02 section 3) | 8 |
| Compaction interval | 32 ops |
| Checkpoint interval | 960 ops |
| Client sessions per group (document 02 section 7) | 16,384 |
| LSM level growth factor | 8 |
| Chunk size target, multi-column groups | 32 KiB |
| Rows per chunk, single-column groups | 8,192 |
| Filter bits per key | 10 |

The checkpoint interval is a multiple of the compaction interval, so a checkpoint falls between compaction beats, and it is smaller than the WAL slots minus the pipeline depth minus one compaction interval, so the WAL never overwrites a prepare the next checkpoint still needs (section 7).

### Superblock

Holds: cluster id, group id, replica index, format version, VSR state (view, log view, the headers of the log installed with log view (document 02 section 4), commit op, checkpoint op), checkpoint references (manifest root block, free-set blocks, client-session table block), each with address and checksum. Written as 4 copies in sequence, each with its own checksum and a sequence number. On open, the highest sequence number with a valid quorum of copies wins. A superblock update is the atomic commit point of a checkpoint.

Format version 2. `granite format` writes sequence number 0, zero VSR state and zero references (address 0 is no block). Each copy is 64 KiB, little-endian; `C` is 48 plus 8 per format constant:

| Offset | Field |
|---|---|
| 0 | Checksum of bytes 16 to the end of the copy, 128 bits |
| 16 | Cluster id, `u128` |
| 32 | Format version, `u32` |
| 36 | Group id, `u32` |
| 40 | Replica index, `u8` |
| 41 | Copy index, 0 to 3, `u8` |
| 42 | Zero, 6 bytes |
| 48 | The format constants (`granite_base::constants::FORMAT`), each a `u64`, in declaration order |
| C | Sequence number, `u64` |
| C + 8 | View, `u32` |
| C + 12 | Log view, `u32` |
| C + 16 | Commit op, `u64` |
| C + 24 | Checkpoint op, `u64` |
| C + 32 | Manifest root reference: address `u64`, checksum `u128` |
| C + 56 | Free set reference, the same way |
| C + 80 | Client session table reference, the same way |
| C + 104 | Number of headers, `u32`, at most as many as fit in the copy |
| C + 108 | Zero, 4 bytes |
| C + 112 | The headers of the log installed with log view, 256 bytes each |
| after them | Zero, to the end of the copy |

The copies of one record differ only in the copy index and the checksum. How many headers VSR stores, and what a checkpoint puts into the references, belong to the VSR and checkpoint tasks.

An update writes the whole record with the next sequence number to copy 0, 1, 2 and 3 in turn, and makes each copy durable (write, then fsync) before it writes the next. This is the quorum rule: at every instant the old record or the new one is on at least 2 copies, so a crash leaves one of them, never a mix. A sequence number other than the previous plus one is a bug and fails an assertion. Then the update reads the four copies back and writes again, in order, every copy that does not hold the new record at its own offset, until all four do. The update has finished, and the checkpoint is committed, only then. If the copies still need rewriting after 4 reads (`READ_BACKS_MAX`), the device keeps losing or misdirecting writes and the replica stops, as it does when a superblock write fails.

The read back is there for a misdirected write. A copy written at another copy's offset leaves its own offset with the old record and the other copy invalid, so only 2 copies may hold the new record. If the update finished there, a crash during the next update could leave no record on 2 copies. After the read back every finished update or open leaves 4 good copies, so a crash in the next update leaves a quorum. What 4 copies with a quorum of 2 cannot survive is a misdirected write and a torn write in the same update, before it finishes: they can leave one copy of each record and one invalid copy. Such a replica stops and repairs from its peers.

Open reads the four copies. A copy counts for its position only if its checksum holds and its copy index is its position, so a copy written at another copy's offset (a misdirected write) does not count; an unreadable copy does not count either. Open chooses the record with the highest sequence number held by at least 2 copies, then rewrites, in order, every copy that does not hold it, and reads back and rewrites as an update does. Opening checks, in this order, each failure with its own typed error and before anything is written: no copy with a valid checksum has another version (the version decides how the rest is laid out, so a file with a higher or lower version is refused), a record is held by 2 copies and no other record with its sequence number is, every format constant matches the binary's, the number of headers fits, the zero bytes are zero, and the cluster id, group id and replica index are the ones the replica was started with.

### WAL

- A prepare is header plus body. The header contains the checksum of the body, the checksum of the header itself, and the checksum of the previous prepare's header (hash chain).
- Slot index = op number mod slot count. A prepare is written to the prepare ring first, then its header to the header ring. The redundant header ring lets recovery tell a torn write (header ring has the old header, prepare slot is garbage) from corruption (header ring says the prepare should be valid).
- An append (`granite_storage::wal`) takes four I/Os, each submitted only after the previous one completed: the prepare is written to its slot from the caller's buffer, without a copy, and made durable with an fsync; its header then enters the in-memory copy of the header ring, whose sector holding it (16 headers) is written and made durable with an fsync. Only then is the append reported complete. So the header ring on disk never names a prepare that is not durable. One header write is in flight at a time and carries every header of its sector whose prepare is durable, so appends complete in any order, and the header ring may hold a later op's header before an earlier one's. A prepare is written up to the end of its last sector, and the bytes after its body must be zero, so the slot content is a function of the prepare.
- The append checks that the prepare's op follows the last one and that its parent is the last header's checksum; a prepare that breaks the chain is a bug in the caller and stops the process. A failed write or fsync stops the process too (decision 0003). At most the pipeline depth of appends are in flight; beyond that, and while a read of the slot is in flight, an append is refused and handed back, which is the caller's backpressure.
- A read by op is answered from the in-memory header ring: an op that a later op overwrote, or is overwriting, is not present, without any I/O. Otherwise the prepare slot is read and must hold a valid header with the checksum the ring holds for that op, and a body that matches it; each failure, and a failed read, is its own typed error, and the bytes are not returned.
- Recovery classifies each slot as: clean, torn, corrupt, or missing. Torn tails are safe to truncate. Corrupt committed slots are repaired from peers. This distinction is the core of protocol-aware recovery and must be verified against Alagappan et al., FAST 2018.

### Grid

- Blocks are addressed by a 64-bit index. A reference to a block is always (address, checksum). The checksum covers the block including its own address and type, so a misdirected write fails verification.
- Free set: a bitset of allocated blocks, persisted at checkpoint. Blocks released during a checkpoint interval become reusable only after the next checkpoint is durable, so a crash can always fall back to the previous checkpoint.
- Block cache: set-associative, fixed size, per shard, keyed by address, validated by checksum.
- Grid repair: on checksum failure the shard requests the block by (address, checksum) from peers. This works because storage is byte-identical across replicas of a group.

Checksums: 128-bit XXH3-128 (default secret, seed 0), computed only through `granite_base::checksum::checksum`. AEGIS-128L, what TigerBeetle uses, was the other candidate; the comparison and the reasons are in [decisions/0002](decisions/0002-checksum.md), which stays proposed until the x86-64 report confirms it. The choice is part of the on-disk and wire formats.

## 3. LSM forest

A shard holds one LSM tree per table, plus one per secondary index, plus internal trees (schema, session table, lock table, pending cross-shard transactions, derived-table state). The set of trees is the forest; the manifest lists every part of every tree.

### Keys and ordering

- User table sort key: `(token, primary key)` for hash-partitioned tables, `(primary key)` for range-partitioned ones. `token` is the 64-bit hash of the partition key, so a tablet is a contiguous key range in the tree and all rows of one partition key are adjacent.
- `versioned` tables append `commit timestamp descending` to the sort key.
- Keys are encoded order-preserving into bytes for comparison in indexes; column data is stored typed.

### Memtable

- Row-oriented, sorted (skip list or B-tree over an arena), fixed capacity.
- Two memtables: mutable and immutable-being-flushed.
- Not durable by itself; rebuilt by replaying the WAL from the last checkpoint.

### Levels

- Level 0: parts flushed from the memtable, may overlap.
- Levels 1..L: non-overlapping parts, each level 8x the previous (growth factor to benchmark).
- Tiered-into-levelled compaction: a part from level N is merged with overlapping parts in level N+1.

### Deterministic pacing

Compaction is driven by op number, not by time or load. Every committed op grants a fixed quota of compaction work, and flushes happen at fixed op intervals. Checkpoints happen every fixed number of ops. Two replicas that have committed the same ops have performed the same compaction steps and allocated the same blocks. This is what makes the grid byte-identical.

Consequence: compaction must be provisioned for the worst case, and it cannot fall behind. If the quota cannot be met by the disk, commit latency rises; that is backpressure by design.

## 4. Part format

A part is an immutable run of rows sorted by the sort key, stored in PAX layout.

```
part
 ├─ part header block: schema version, row count, key range, timestamp range,
 │                     level, per-column stats, references to the blocks below
 ├─ filter blocks:     split-block bloom (or ribbon) over primary keys
 ├─ key index blocks:  sparse: first key of each key chunk -> row ordinal
 ├─ group index blocks: per column group: ordinal -> (block ref, offset,
 │                     length, chunk checksum)
 ├─ skip-index blocks: per chunk min/max per column; optional bloom/set
 └─ data blocks:       encoded chunks, several per block, one group per block
```

### Column groups and layout (revised 2026-11-17)

Review finding: with 20 to 100 columns per row and point reads of any age, one block per column makes a whole-row read from a deep level cost up to 100 block reads. Column groups are therefore the central concept, not an option.

- **Column group:** a set of columns stored together. Within a group's chunk each column is still encoded separately and contiguously (true PAX), so one read fetches every column of the group and decoding stays vectorisable.
- **Layout** is declared per table, with no default (the schema is rejected without it):
  - `row`: one group holding all columns. A point read of a whole row is one chunk read per level hit. Scans read all columns from disk but still decode only what they need.
  - `columnar`: one group per column. Best scans and compression; a point read costs one chunk read per requested column, issued in parallel.
  - `groups(...)`: explicit groups, for example a hot group of five columns read by transactions and a cold group for the rest.
- **Chunk:** one group's values for a run of consecutive rows, independently decodable. Each group has its own chunk size, set by a byte target rather than a row count: about 32 KiB for multi-column groups (so a point read fetches 32 KiB, not megabytes), up to 8192 rows for single-column groups.
- **Addressing:** rows in a part are numbered by ordinal. The key index maps a key to an ordinal; each group has an ordinal index mapping to (block, offset, length, chunk checksum).
- **Partial block reads:** because the index entry carries the chunk's own checksum, a point read fetches only the sector-aligned byte range of the chunk, not the whole 128 KiB block. Block-level checksums remain for scrubbing and repair; a chunk that fails verification triggers a full-block repair.
- **Columnar projection (phase 6, optional):** a `row` table may additionally keep a columnar copy of selected columns in its deepest levels, for tables that need both whole-row reads of any age and fast scans. It costs space and compaction work, so it is opt-in.

Expected device reads for a point read that misses the caches, per level that passes the filter: `row` layout 1; `columnar` or `groups` one per requested group, in parallel. Index and filter blocks are expected to be cached.

- **Relocatable parts (added 2026-11-18):** blocks inside a part refer to each other by index within the part; the part header holds the extent map from index to grid address, and each block identifies itself by (part id, index). This lets a part be installed at different addresses in another group's grid during a tablet move (document 04 section 2) by rewriting only its header. A manifest entry may also carry a visible key range that clips the part.

- **Hidden columns:** `_ts` (commit timestamp, u64) on every table; `_tombstone` (bool) on versioned tables.
- **Nullable columns:** a validity bitmap chunk next to the value chunk.
- **Variable-length columns:** offsets chunk plus bytes chunk.

### Read paths

Point read by primary key:
1. Memtables.
2. Row cache (below).
3. For each level, newest first: skip parts whose key range excludes the key, check the filter, binary-search the key index, read and decode the key chunk to find the row ordinal, then read the chunk of each requested group and decode the single row.
4. Stop at the first hit (`immutable`) or first version with `_ts <= snapshot` (`versioned`).

Because point reads hit data of any age uniformly, deep levels matter as much as shallow ones:
- **Filters:** about 10 bits per key. Filter blocks are cached, not pinned, so the filter cache needs its own memory budget; an uncached filter costs one extra read. With seven levels a miss-everywhere lookup probes up to seven filters.
- **Row cache:** a fixed-size cache of decoded latest versions by primary key, per shard, for hot rows such as balances in derived tables. Invalidated by writes on the same thread, so it needs no locking.
- **Parallel I/O:** all chunk reads for one lookup, and all lookups in one batch, are submitted to io_uring together.

Scan: merge iterators across memtables and parts, with granule pruning by skip indexes, reading only requested column chunks.

### Level-dependent encoding

Point reads want random access into a chunk; scans want maximum compression. So:
- Levels 0 and 1: lightweight encodings only, all random-access: plain, frame-of-reference with bit-packing, dictionary, fixed-width.
- Deeper levels: add delta and double-delta for sorted and time columns, RLE, FSST for strings, then LZ4.
- Bottom level, optionally: ZSTD.

Codec choice per column chunk is made by a deterministic rule from the data (not by a timed trial), so replicas agree.

### Uniqueness checks

Inserting into a table requires proving the primary key is absent, which is a point read. Mitigations:
- Client-generated time-ordered ids (as TigerBeetle recommends). New keys are above every older part's key range, so the check prunes on key range alone and touches no filter.
- Filters for everything else.
- An "insert, replace if exists" mode on versioned tables skips the check.

## 5. Table kinds in storage

| | `immutable` | `versioned` |
|---|---|---|
| Sort key | (token, pk) | (token, pk, _ts desc) |
| Update, delete | rejected | new version, tombstone |
| Compaction | re-sort, re-encode | additionally drops versions older than the retention horizon when a newer version exists |
| Retention | forever, or drop by time segment | horizon per table, or keep all |

Time segments (phase 7): a table may declare a time bucket on a timestamp column. Each bucket is a separate tree, which gives cheap retention (drop a tree) and time-range pruning that survives full compaction.

## 6. Secondary indexes

- Local to the shard. An index is an LSM tree with sort key `(token, indexed columns, pk)` and no value columns, or with covering columns if declared.
- Maintained in the same op as the base write, so always consistent.
- Global (cross-tablet) lookups by a non-partition column fan out to all tablets. A true global index is a table partitioned by the indexed column, maintained by a cross-shard transaction; deferred.

## 7. Checkpoint

Every fixed number of ops (bounded by WAL size minus the pipeline and one compaction interval):
1. Flush pending manifest changes and the free set to grid blocks.
2. Write the client-session table.
3. Write the superblock with the new roots and checkpoint op.
4. Released blocks become reusable.

Recovery: open superblock, load manifest and free set, replay WAL prepares after the checkpoint op.

## 8. Memory

All pools sized at startup from configuration: block cache, memtable arenas, compaction buffers, message pool, scan buffers. No allocation on the commit path after startup. Out-of-pool conditions are handled by backpressure, never by growing.

## 9. To verify before implementation

- Protocol-aware recovery slot classification rules (Alagappan et al., FAST 2018).
- TigerBeetle's free-set reservation scheme and compaction pacing ("beats" and "bars"), as a reference for deterministic allocation.
- Gate G1 experiment, redefined: for tables of 20, 50 and 100 columns at about 1 KiB per row, with point reads uniformly distributed over data of all ages, measure point-read latency, write amplification, space and scan speed for the `row`, `columnar` and a two-group layout. The outcome sets the recommended layout per workload and the chunk byte targets, and tells us whether the columnar projection is needed earlier than phase 6.
- Filter memory at scale: bits per key against cache budget for a shard holding billions of rows.
