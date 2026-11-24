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

`granite format --cluster <id> --group <id> --replica <index> <path>` creates the data file. It refuses, changing nothing, if anything exists at the path. It writes the superblock zone (every copy holding the initial record below) and zeros over both WAL zones, so their space is allocated at format time and a later WAL write cannot fail for lack of room, then makes the file and its directory entry durable. The grid starts empty and gets its first step at the first op the replica applies, when the free set is found low.

The grid size is a whole number of growth steps, and so is the limit. When the free set runs low the grid grows by exactly one step; at the limit it cannot, and the write that needed a block gets `no_space`. The storage crate names neither the clock nor the file system's free space (`tools/lint.py`), so growth cannot depend on them.

Before each op it applies, the replica asks the free set whether the grid must grow. The free set is low when fewer than an eighth of a step of blocks (1,024) are free outside reservations (`free_set::GROW_BELOW`, to be tuned with compaction's worst case); the decision reads only the free set, so it is a function of the ops applied. When it is low, `FreeSet::growth` returns the grid size one step up; the replica submits `Storage::grow_grid` with it and waits for its completion before applying the op. The growth makes the grid zone at least the new size, with the space allocated rather than a hole (`fallocate` on Linux, `F_PREALLOCATE` on macOS), so a later write cannot fail for lack of room, the new bytes zero and the size durable; a file already longer, as a crash after an earlier growth can leave it, is not shortened. Once it completes, `FreeSet::grown` adds the step's blocks to the set. A failed growth stops the process, as a failed write does (decision 0003). The free set records its grid size, so a replica that recovers an earlier checkpoint grows again at the same op as it replays.

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
- An append (`granite_storage::wal`) takes these I/Os, each submitted only after the previous one completed. A prepare of more than one sector first has its first sector, which holds its header, written and made durable with an fsync. The whole prepare is then written to its slot from the caller's buffer, without a copy, and made durable with an fsync. Its header then enters the in-memory copy of the header ring, whose sector holding it (16 headers) is written and made durable with an fsync. Only then is the append reported complete. So the header ring on disk never names a prepare that is not durable, and a write that tears over a slot always leaves the new prepare's header there if it leaves any of its sectors (recovery, below, depends on this). A prepare is written up to the end of its last sector, and the bytes after its body must be zero, so the slot content is a function of the prepare.
- Headers enter the in-memory header ring in op order: an op's header waits until every earlier op's prepare is durable. One header write is in flight at a time; it carries every header of its sector in the ring, and the sector of the oldest header not yet written goes first. So the header ring on disk always names a prefix of the appended ops, and appends complete in op order, as VRR §4.1 has a backup acknowledge. The first-sector write costs one more write and fsync per prepare of more than one sector; with `O_DSYNC` both of a prepare's fsyncs are redundant on Linux, which the phase-1 benchmarks decide.
- The append checks that the prepare's op follows the last one and that its parent is the last header's checksum; a prepare that breaks the chain is a bug in the caller and stops the process. A failed write or fsync stops the process too (decision 0003). At most the pipeline depth of appends are in flight; beyond that, and while a read of the slot is in flight, an append is refused and handed back, which is the caller's backpressure.
- A read by op is answered from the in-memory header ring: an op that a later op overwrote, or is overwriting, is not present, without any I/O. Otherwise the prepare slot is read and must hold a valid header with the checksum the ring holds for that op, and a body that matches it; each failure, and a failed read, is its own typed error, and the bytes are not returned.

#### WAL recovery

Recovery (`granite_storage::wal::recovery`) reads the header ring once, takes the head from it, then reads every prepare slot once, as far as the prepare the header ring names reaches or one sector, and classifies each slot. A prepare other than the named one is classified by its header alone. It fetches nothing: it reports what needs repair from peers (T2.2.4) and whether the head is uncertain (`recovering_head`, T2.2.5). A failed read counts as damage, of every entry for the header ring.

- A header ring entry is *zeros*, *valid* (a prepare header whose op maps to the slot, op 1 or later) or *damaged* (anything else, including a valid header of another slot, which is a misdirected write).
- A prepare slot is *zeros* (its header bytes are zero), *whole* (a valid prepare header that maps to the slot, and a body that matches it) or damaged, with or without a valid header that maps to the slot.
- The **head** is the highest op the header ring names, or 0 when it names none. Because the header ring on disk is a prefix of the appended ops, every op up to the head was appended, and each slot's op is the highest op up to the head that maps to it, if any.
- The head is **uncertain** when the header ring entry of the slot that the op after the head maps to is damaged: that op may have been appended and acknowledged. A damaged entry anywhere else cannot hide a later op, since the prefix would then include the op after the head. A corrupt slot below the head does not make the head uncertain (document 02 section 5).
- Every **corrupt** slot to which an op up to the head maps needs repair: that op, with its header checksum when the header ring still holds it (the hash chain gives it otherwise).

| Row | Header ring entry | Prepare slot | Class |
|---|---|---|---|
| 1 | zeros, and no op up to the head maps to the slot | zeros | missing |
| 2 | zeros, and no op up to the head maps to the slot | anything else | torn |
| 3 | zeros, and an op up to the head maps to the slot | anything | corrupt |
| 4 | damaged | anything | corrupt |
| 5 | valid, an older op than the slot's op | anything | corrupt |
| 6 | valid, the slot's op | that prepare whole (same header checksum) | clean |
| 7 | valid, the slot's op | a valid header of an op beyond the head, body whole or not | torn |
| 8 | valid, the slot's op | that op's header with a damaged body | corrupt |
| 9 | valid, the slot's op | damaged header | corrupt |
| 10 | valid, the slot's op | zeros | corrupt |
| 11 | valid, the slot's op | another valid prepare, of an op up to the head | corrupt |

*Missing* means nothing was ever appended to the slot. *Torn* means an append of an op beyond the head that a crash interrupted: it was never acknowledged, so it is discarded, and an assertion checks that a torn slot's op is beyond the head. *Corrupt* means the header ring names, or may name, a prepare that the slot does not hold whole: it may be committed and is repaired. Each row has a test in `crates/sim/tests/wal_recovery.rs` that writes that state to a simulated disk.

Why a crash alone never yields a corrupt slot: the header ring is a prefix (so rows 3 and 5 need a lost or misdirected header write), a header is written only over a durable prepare (rows 8 to 10 need damage or a lost write), and a torn overwrite leaves either the old prepare whole (row 6) or the new prepare's header (row 7, never row 8). Why damage alone never yields a torn slot: a slot is torn only when no op up to the head maps to it (row 2, a slot the log never completed an append to) or when it holds the header of an op beyond the head (row 7); damage cannot create a valid header, and the head and the header ring are what the log wrote. Seed gates in the same test file check both with 10,000 seeds.

Checked against Alagappan et al., "Protocol-Aware Recovery for Consensus-Based Storage", FAST 2018 (PAR; sections 2.3 and 3, see [02a](02a-replication-sources.md)). The header ring entry is PAR's persist record and identifier together: written after the entry, stored apart from it, checksummed and small enough to land whole (PAR §3.3.3, §3.3.4).

| Row | PAR |
|---|---|
| 1 | No counterpart: PAR's log is a preallocated file appended at its end, so an unused part is never read as an entry. |
| 2 | §3.3.3, ordered update: an entry without its persist record means a crash during the update, and the entry is discarded, which is safe because it was never acknowledged. |
| 3, 5 | No counterpart; see difference 4. |
| 4 | §3.3.3: a damaged persist record means corruption, not a crash. |
| 6 | §3.3.2: an item that matches its checksum is not faulty. |
| 7 | §3.3.3, ordered update: the newer entry has no persist record, so the crash case. |
| 8, 9, 10 | §3.3.3, ordered update: a mismatch while the persist record is present means corruption. |
| 11 | §3.3.4: the identifier, stored apart from the item, shows that the item is not the one recorded (a lost or misdirected write). |

Differences from PAR:

1. **Ordering.** PAR's log does not order an entry before its persist record (write both, then one fsync) and so cannot tell a crash from corruption for the last entry; it marks that entry corrupt and leaves it to the distributed protocol (PAR §3.3.3). We use the ordered update that PAR describes first, at the cost of an fsync between the prepare and its header, so no entry is ambiguous and the head is uncertain only when the header ring itself is damaged.
2. **A ring, overwritten in place.** PAR's log is appended; ours reuses slots. A torn overwrite that left the old header sector and some new body sectors would look exactly like a damaged old prepare. Writing the first sector first (WAL above) rules that state out; PAR has no counterpart.
3. **Later entries.** PAR also concludes corruption when the next entry or its persist record is present (§3.3.3). We do not look at later entries; the position of a slot's op against the head plays that role, and is exact because headers are written in op order.
4. **Lost and misdirected header writes** (rows 3 and 5). PAR counts lost and misdirected writes among the causes of corrupted data (§3.1, Table 2) and catches them by the checksum and the identifier kept apart (§3.3.4), but does not discuss a lost write of the persist record itself, which in its log would look like a crash. Here a header entry that should hold the slot's op, by the prefix, but is zeros or older can only be such a write, so the slot is corrupt. A lost write of the head's own header entry cannot be seen locally: recovery then takes the op before as the head and treats the acknowledged op as torn or absent. Replication covers it as it covers a lost replica, and the headers the superblock keeps with the log view (document 02 section 4) bound it; it stays a known limit of local recovery.
5. **Item and identifier both damaged.** PAR crashes the node (§3.3.4). We classify the slot corrupt and repair it: a slot's position and the head identify the op, so no identifier is needed to name it, except in the slot after the head, which makes the head uncertain instead.
6. **Snapshots and metadata.** PAR's renamed temporary files have no ambiguity (§3.3.3); ours are the grid and the superblock, whose 4 copies and quorum rule are above.

### Grid

- Blocks are addressed by a 64-bit index. A reference to a block is always (address, checksum). The checksum covers the block including its own address and type, so a misdirected write fails verification.
- Block I/O (`granite_storage::grid`): address 0 names no block; block `a` lies at offset `(a - 1) x 128 KiB` of the grid zone. A block starts with a 32-byte header, little-endian, and its content fills the rest:

  | Offset | Field |
  |---|---|
  | 0 | Checksum of bytes 16 to the end of the block, 128 bits |
  | 16 | The block's own address, `u64` |
  | 24 | Block type, `u8`: 1 manifest, 2 free set, 3 client session table |
  | 25 | Zero, 7 bytes |

  The layout is part of format version 2 (superblock) and needs no version bump: no file of that version held a block before it was defined. A write seals the header into the caller's buffer and writes the whole block from it. There is no read by address alone: a read takes the reference and the type it expects, reads the whole block into the caller's buffer with one device read, and checks it there, in this order, each failure its own typed error: the block matches its own checksum (bit rot, a torn write), that checksum is the reference's (a lost or misdirected write, a misdirected read), the address is the reference's, and the type is the expected one. The bytes reach the caller only after every check; a failed read reports the reference it expected, which is what repair from peers (T2.2.6) requests. The grid's table of operations in flight is sized when it is built; beyond it, or when the backend is full, a read or write is refused and handed back. A failed write stops the process (decision 0003). The grid does not fsync: a written block is durable only after a later fsync, which the checkpoint (section 7) issues before the superblock names the block.
- Free set (`granite_storage::free_set`): a bitset of allocated blocks, persisted at checkpoint. Blocks released during a checkpoint interval become reusable only after the next checkpoint is durable, so a crash can always fall back to the previous checkpoint.

  Allocation depends only on the calls made. An acquire takes the lowest free block outside every reservation. A reservation, made for compaction's worst case before it starts, is the range after the previous reservation's that holds the number of free blocks asked for; acquires against it take its lowest free block, so concurrent compactions get the same addresses on every replica whatever order their acquires come in. When every reservation has been forfeited, the next starts from the first block again. Acquire and reserve keep free the blocks the next checkpoint needs for the set's own encoding, and return `no_space` rather than hand them out. The bitset is sized for the group's limit at startup and nothing in the set allocates afterwards.

  At a checkpoint the set releases the blocks of the previous checkpoint's encoding, acquires those of the new one, and is frozen until the checkpoint is durable: the replica applies no op in between, so released blocks become free at the same op on every replica. No reservation spans a checkpoint. The encoding is a chain of blocks of type 2, the superblock's free set reference naming the first; a grid of no blocks has no encoding and reference address 0. Each block's content, after the block header, little-endian:

  | Offset | Field |
  |---|---|
  | 0 | Grid size in blocks, `u64`, the same in every block of the chain |
  | 8 | Next block of the chain: address `u64`, checksum `u128`; address 0 on the last |
  | 32 | This block's share of the bitset, 16,376 `u64` words in order (fewer in the last block); bit `i` of word `w` is block `64 w + i + 1`, set if acquired |
  | after them | Zero, to the end of the block |

  Blocks released since the last checkpoint are written as free, and the chain's own blocks as acquired. Like the block header, the layout needs no version bump: no file of format version 2 held a free set block before it was defined. Recovery reads the chain from the superblock's reference, each block through the grid against the reference the previous one holds.
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

- TigerBeetle's free-set reservation scheme and compaction pacing ("beats" and "bars"), as a reference for deterministic allocation.
- Gate G1 experiment, redefined: for tables of 20, 50 and 100 columns at about 1 KiB per row, with point reads uniformly distributed over data of all ages, measure point-read latency, write amplification, space and scan speed for the `row`, `columnar` and a two-group layout. The outcome sets the recommended layout per workload and the chunk byte targets, and tells us whether the columnar projection is needed earlier than phase 6.
- Filter memory at scale: bits per key against cache budget for a shard holding billions of rows.
- Relocatable parts (T1.3.10) against the grid block header: a block header carries its own grid address, which its checksum covers (section 2, Grid), so moving a part to other addresses rewrites every block's header and checksum, not only the part header. Section 4 must settle this, for example by verifying the part-relative index instead of the grid address for blocks inside a part.
