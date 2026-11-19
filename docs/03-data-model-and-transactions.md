# 03. Data model and single-shard transactions

## 1. Schema objects

```
cluster
 └─ table group        partition key definition, partitioning (hash | range)
     ├─ table          kind (immutable | versioned), columns, primary key, options
     │   └─ index      local secondary index
     ├─ derived table  deterministic fold over a source table
     └─ program        registered transaction or query
```

Rules:
- Every table belongs to exactly one table group. All tables of a group share the partition key columns (same types, same order) as a prefix of their primary key.
- All rows of all tables in a group with the same partition key value live in the same tablet. This is the co-location guarantee that single-shard transactions rely on.
- Every table declares its storage layout: `row`, `columnar` or `groups(...)` (document 01 section 4). There is no default. The layout can be changed later; existing parts are rewritten by compaction.
- Schema changes are ops in the metadata group, then applied by each data group at a defined op. Each has a schema version; parts record the version they were written with.

Supported changes in the first release: create/drop table, add nullable or defaulted column, drop column, add/drop index, register/retire program. Not supported: changing a column's type, changing keys, changing table kind.

### 1.1 Catalog validation

The catalog (`crates/schema`) holds the schema version and the table groups; a group holds its partitioning (hash or range), its partition key columns (name and type) and its tables; a table holds its kind, columns, primary key (column indexes, each ascending or descending), layout and retention. A catalog is validated as a whole, when it is built and when it is decoded, and the first broken rule rejects it with that rule's code and its place: the group, the table and the column, key entry, layout or retention that breaks it. The codes are part of the protocol.

| Code | Name | Rule |
|---|---|---|
| 1 | `Malformed` | The bytes are a catalog encoding (section 1.2): every tag known, no truncation, no trailing bytes, names valid UTF-8, types nested at most three wrappers deep. |
| 2 | `UnknownVersion` | The encoding version is one this binary reads. |
| 3 | `TooManyGroups` | At most as many table groups as the table limit of section 8. |
| 4 | `TooManyTables` | At most the table limit of section 8 in the whole cluster. |
| 5 | `TooManyColumns` | At most the column limit of section 8 per table, and per partition key. |
| 6 | `InvalidName` | Every group, table and column name is 1 to 255 bytes. |
| 7 | `DuplicateName` | Group names and table names are unique in the cluster; column names are unique in their table and in their partition key. |
| 8 | `InvalidType` | Every column type is valid (section 2: decimal precision and scale, FixedBytes size, nesting of Nullable and LowCardinality). |
| 9 | `EmptyKey` | Every partition key and every primary key has at least one column. |
| 10 | `UnknownColumn` | Every column index in a primary key or a layout group names a column of the table. |
| 11 | `RepeatedKeyColumn` | No column appears twice in a primary key. |
| 12 | `KeyColumnNotAllowed` | No primary key or partition key column is a float or Nullable, also under LowCardinality (section 2.1). |
| 13 | `PartitionKeyNotPrefix` | Every primary key in a group starts with the group's partition key columns: same names, same types, same order. |
| 14 | `KeyTooLarge` | The longest encoding of the primary key, and of the partition key, is within the key limit of section 8: the sum over its columns of the fixed width, or 2 x max + 2 for String and Bytes, whose every byte may be escaped. A key that passes can never fail with `KeyTooLarge` at run time. |
| 15 | `RowTooLarge` | The static row bound (section 2.4) is within the row limit of section 8 and, with a message header, within one message. |
| 16 | `NoLayout` | Every table declares its layout: `row`, `columnar` or `groups(...)`. |
| 17 | `LayoutGroups` | A `groups(...)` layout puts every column in exactly one group and has no empty group. |
| 18 | `RetentionUnsupported` | Retention is `forever`, or for a versioned table a time after which old versions are dropped. Archiving, and dropping rows of an immutable table, need time segments and are rejected until phase 7. |

### 1.2 Catalog encoding (part of the format)

The persisted and transmitted form of a catalog. Integers are little-endian. Only a valid catalog is encoded, a decoded catalog is validated, and the encoding is canonical: decoding then encoding gives the same bytes. A change needs a new encoding version.

```
catalog    u16 encoding version (1), u64 schema version, u16 group count, groups
group      name, u8 partitioning (0 hash, 1 range), columns (the partition key),
           u16 table count, tables
table      name, u8 kind (0 immutable, 1 versioned), columns,
           u16 key column count, per key column: u16 column index, u8 order (0 asc, 1 desc),
           layout, retention
columns    u16 count, per column: name, type
name       u8 length, UTF-8 bytes
layout     u8: 0 not declared (rejected), 1 row, 2 columnar,
           3 groups: u16 group count, per group: u16 count, u16 column indexes
retention  u8: 0 forever, 1 archive after u64 ms, 2 drop after u64 ms
type       u8 tag, then its parameters:
           0 u8, 1 u16, 2 u32, 3 u64, 4 u128, 5 u256, 6 i8, 7 i16, 8 i32, 9 i64,
           10 i128, 11 i256, 12 Decimal (u8 P, u8 S), 13 f32, 14 f64, 15 bool,
           16 Date, 17 Timestamp (u8 p), 18 Duration (u8 p), 19 Uuid, 20 Id128,
           21 String (u32 max), 22 Bytes (u32 max), 23 FixedBytes (u32 N),
           24 Enum8 (u8 index max), 25 Enum16 (u16 index max),
           26 Nullable (type), 27 LowCardinality (type)
           where p is 0 for seconds, 1 milliseconds, 2 microseconds, 3 nanoseconds
```

### 1.3 Schema text syntax

Schemas are written as text files and compiled into the encoding of section 1.2 by `granite-schemac` (`crates/schemac`), which is client tooling: the server never links it and only the binary form crosses the wire. Its output is a function of the input bytes alone. Derived tables, programs and indexes are added to the syntax by their own tasks.

```
schema      = "schema" "version" number { group | table }
group       = "table" "group" name "partition" "by" "(" [ column { "," column } ] ")" ( "hash" | "range" )
table       = "table" name ( "immutable" | "versioned" ) [ "layout" layout ] "in" name
              "key" "(" [ key-column { "," key-column } ] ")"
              column { "," column }
              [ "retention" ( "forever" | ( "archive" | "drop" ) "after" number "ms" ) ]
layout      = "row" | "columnar" | "groups" "(" [ names { "," names } ] ")"
names       = "(" [ name { "," name } ] ")"
key-column  = name [ "asc" | "desc" ]
column      = name type
type        = "u8" | "u16" | "u32" | "u64" | "u128" | "u256" | "i8" | "i16" | "i32" | "i64" | "i128" | "i256"
            | "Decimal" "(" number "," number ")" | "f32" | "f64" | "bool" | "Date"
            | ( "Timestamp" | "Duration" ) "(" ( "0" | "3" | "6" | "9" ) ")" | "Uuid" | "Id128"
            | ( "String" | "Bytes" | "FixedBytes" ) "(" number ")"
            | ( "Enum8" | "Enum16" ) "(" ( number | name { "," name } ) ")"
            | ( "Nullable" | "LowCardinality" ) "(" type ")"
name        = word | quoted
word        = ( letter | "_" ) { letter | digit | "_" }       ASCII letters and digits
quoted      = '"' { any character but '"' and '\', or '\"', or '\\' } '"'
number      = digit { digit }                                  decimal, at most 2^64 - 1
```

- Whitespace separates tokens and is otherwise free; `--` starts a comment that runs to the end of the line. Keywords are words that the grammar expects at that place, so a column may be called `key`; only a table called `group` must be quoted.
- The file is the catalog: its schema version, its groups in file order, and each group's tables in file order. A group is declared before its tables. Columns are stored in declaration order, and keys and layout groups name columns of their own table.
- Retention is `forever` when it is not written. A table without `layout` is rejected with `NoLayout`: there is no default.
- `Timestamp(p)` and `Duration(p)` take the number of decimal digits of the second: 0, 3, 6 or 9.
- An enum declares its values by name, which the client uses, or by their number. The catalog keeps only the number, as the highest index (section 1.2), so a printed catalog shows the number.
- Errors name the file, the line and the column (both from 1, the column in characters) and either a rule of section 1.1 with its code or what the grammar expected. A file that is not UTF-8 breaks rule 1, `Malformed`, at its first invalid byte. A type nested more than three wrappers deep, which the encoding cannot hold, always repeats a wrapper and is reported as `InvalidType`. Rule 2, `UnknownVersion`, is about the version of the binary encoding, which text does not carry; the compiler always writes the current one.
- `granite-schemac print` prints a binary catalog as canonical text: one declaration per paragraph, each column on its own line, names quoted only where needed, `asc` and `retention forever` left out. Compiling the printed text gives the same bytes.

## 2. Types

| Group | Types | Notes |
|---|---|---|
| Integers | u8, u16, u32, u64, u128, u256, i8 ... i256 | Checked arithmetic |
| Decimal | Decimal(P, S), P <= 76 | Backed by i32/i64/i128/i256 by precision |
| Float | f32, f64 | Not allowed in keys; NaN canonicalised; no fast-math |
| Bool | bool | |
| Time | Date (days), Timestamp(p) UTC with p in {0,3,6,9}, Duration(p) | No time zones in storage |
| Identity | Uuid, Id128 | Id128 is a time-ordered u128 generated by the client library |
| Text | String(max) | UTF-8 validated, binary collation |
| Bytes | Bytes(max), FixedBytes(N) | |
| Enum | Enum8, Enum16 | Values declared in schema, append-only |
| Wrapper | Nullable(T) | Not allowed in primary keys |
| Hint | LowCardinality(T) | Storage encoding only, same logical type |
| Later | Array(T, max), Tuple(...), Map(K, V, max), Json(max) | Phase 6 or after |

Strictness:
- No implicit conversion between any two types. Explicit casts are checked and fail on loss.
- Overflow, division by zero, invalid UTF-8, length over max, unknown enum value: the event fails with a typed error code. Nothing is truncated or wrapped.
- Default is NOT NULL. Defaults must be constants.
- Row size has a static upper bound computed from the schema (section 2.4); it must fit the message size with headroom, otherwise the schema is rejected.

Values in memory (`crates/types`): integers are the Rust integers of their width; u256 and i256 are two 128-bit halves, `hi` (u128, or i128 for i256) and `lo` (u128), whose two's complement value is `hi * 2^128 + lo`. A Decimal(P, S) holds its mantissa `m`, the value being `m / 10^S`, in an i32 for P <= 9, an i64 for P <= 18, an i128 for P <= 38 and an i256 above. Date is an i32, Timestamp and Duration an i64 (units of 10^-p seconds), Uuid 16 bytes in RFC 9562 order, Id128 a u128, Enum8 and Enum16 the u8 or u16 index of a declared value. P is from 1 to 76, S from 0 to P; FixedBytes(N) needs N from 1 to the row size limit of section 8; Nullable and LowCardinality do not nest in themselves, nor does Nullable through LowCardinality.

### 2.1 Key encoding (part of the format)

Every byte below is part of the on-disk format: keys of LSM trees and indexes are compared as unsigned bytes, lowest address first (`memcmp` order), so the encoding must give the logical order. A change to any of it needs a new format version (document 01 section 2). The golden vectors in `crates/types/tests/golden_vectors.txt` show each type's bytes and are checked on every CI platform.

A key is the concatenation of its columns' encodings, in key order, with no separator or length. Floats and Nullable(T) are rejected in every key with the error `NotAllowedInKey`; LowCardinality(T) is encoded as T.

| Type | Bytes | Encoding |
|---|---|---|
| u8, u16, u32, u64, u128 | 1, 2, 4, 8, 16 | The value, big-endian. |
| u256 | 32 | `hi` big-endian, then `lo` big-endian: the value big-endian. |
| i8, i16, i32, i64, i128 | 1, 2, 4, 8, 16 | The two's complement value with its sign bit inverted, big-endian. The minimum encodes as all 0x00, -1 as 0x7F FF.., zero as 0x80 00.., the maximum as all 0xFF. |
| i256 | 32 | `hi` with its sign bit inverted, big-endian, then `lo` big-endian. |
| Decimal(P, S) | 4, 8, 16, 32 | The mantissa as an i32, i64, i128 or i256 by P, as above. S is not encoded: all values of a column share it. |
| bool | 1 | 0x00 false, 0x01 true. |
| Date | 4 | As i32. |
| Timestamp(p), Duration(p) | 8 | As i64. p is not encoded. |
| Uuid | 16 | The 16 bytes unchanged. |
| Id128 | 16 | The u128 big-endian, so the byte order is the numeric order. |
| String(max), Bytes(max) | 2 to 2 x len + 2 | Each byte of the content (UTF-8 for String) is copied, except that 0x00 is written as 0x00 0xFF; then the terminator 0x00 0x00. |
| FixedBytes(N) | N | The N bytes unchanged. |
| Enum8, Enum16 | 1, 2 | The index, big-endian. Indexes are assigned in declaration order and values are append-only, so the key order is the declaration order. |

Why the variable-length encoding is order-preserving: inside the content a 0x00 is always followed by 0xFF, so the terminator 0x00 0x00 cannot occur there. At the first byte where two encodings differ, either both are content bytes (the contents differ there), or one is the terminator's second 0x00 against an escape 0xFF (the shorter content ends where the longer one has a 0x00), or the terminator's first 0x00 against a content byte above 0x00 (the shorter content ends first). In every case the shorter or byte-wise smaller content sorts first, which is binary collation. No encoding is a proper prefix of a different encoding of the same type, so a column's encoding ends where the next column begins and composite keys sort column by column.

Descending columns: every byte of the column's ascending encoding is inverted (`b XOR 0xFF`), including the terminator. Because encodings of one type are prefix-free, the first differing byte lies inside both, and inverting it reverses the comparison exactly.

The encoded primary key, all columns included, must not exceed the primary key limit of section 8; a longer key is rejected with `KeyTooLarge`. Escaping counts toward the limit.

The key encoding treats an Id128 only as an unsigned 128-bit integer; its bit layout, which makes the numeric order the creation order, is section 2.3.

### 2.2 Fixed-width storage representation (part of the format)

How one value is stored in a row or a column. Every type except String and Bytes has a fixed width. Integers, Decimal mantissas, Date, Timestamp, Duration, Id128 and enum indexes are little-endian two's complement in the widths of section 2.1; u256 and i256 are `lo` then `hi`, both little-endian, which is the 32-byte value little-endian. f32 and f64 are their IEEE 754 bits, little-endian, with every NaN replaced by the canonical NaN (0x7FC00000 for f32, 0x7FF8000000000000 for f64); a stored NaN with other bits is rejected on read. -0.0 is kept. bool is 0x00 or 0x01. Uuid and FixedBytes(N) are their bytes. Nullable(T) is one flag byte, 0x00 null and 0x01 present, followed by T's representation; for a null those bytes are all zero, which is checked on read. LowCardinality(T) is stored as T; its dictionary encoding belongs to the column layout of document 01.

### 2.3 Id128 layout (part of the format)

An Id128 is created by the client (`granite_base::id128`). Its bits, numbered from the least significant, are part of the format; a change needs a new format version (document 01 section 2):

| Bits | Width | Content |
|---|---|---|
| 127 to 80 | 48 | Unix time in milliseconds, which lasts until the year 10889. |
| 79 to 0 | 80 | Random bits. |

Because the time is in the high bits and the key encoding is the u128 big-endian (section 2.1), ids sort by creation time and new keys land above older data.

The generator takes the time and the random bits from its caller and reads neither itself. For each id: if the time is later than the time field of the generator's last id, the id is that time and fresh random bits; otherwise (the same millisecond, or a clock that went backwards) it is the last id plus one. A full random field carries into the time field, so a generator that creates more ids in one millisecond than the field holds borrows from later milliseconds. Ids from one generator therefore strictly increase whatever the clock does, and may carry a time slightly ahead of the wall clock after a burst or while the clock is behind. A client uses one generator per process. Ids from different generators are ordered only by their time fields.

Collision probability. Assume each generator's random bits are uniform and independent of every other generator's, and count a process that restarts as a new generator. A generator's ids with a given time field t form one run of consecutive integers: it starts at a uniform random value, or at zero when it was borrowed from millisecond t - 1, and grows by one. Two runs of lengths a and b with independent uniform starts overlap only if the difference of their starts is one of a + b - 1 values out of 2^80, so with probability below (a + b) / 2^80. A borrowed run needs the run before it to start within its length of the field's top, which has probability below that length divided by 2^80. Summing over every millisecond and every pair of generators active in it, each id is counted at most G - 1 times among G generators, so for N ids in total

P(any two ids are equal) <= (G - 1) N / 2^80 + N / 2^80 = G N / 2^80.

For example, 1,000 clients each creating 1,000,000 ids per second for a year (N = 3.2 x 10^16) give at most 2.6 x 10^-5, and the 100,000,000 ids of two generators in the test give at most 1.7 x 10^-16.

### 2.4 Row encoding and static row bound (part of the format)

One row of a table is its columns in declaration order, then a heap. A fixed-width column is its storage representation (section 2.2). A String or Bytes column is a slot of two u32 little-endian, the offset of its content in the heap and its length, and the content goes to the heap, in column order; Nullable puts its flag byte before the slot, and a null is the flag and the slot all zero with nothing in the heap; LowCardinality(T) is encoded as T. Offsets count from the start of the row's heap. A row batch (T1.4.4) places the slots of many rows over one shared heap.

The static row bound of a table is the sum over its columns of the fixed width, or the slot (8 bytes, plus 1 for Nullable) and `max` for String and Bytes. No encoded row exceeds it, and a row with every String and Bytes at its maximum length reaches it. Hidden columns (`_ts`, `_tombstone`, document 01 section 4) are added by the server and are not part of this encoding.

## 3. Table kinds

**`immutable`**: rows are inserted once and never change. Insert of an existing primary key fails (or is reported as a duplicate, idempotently, if all columns match). This is the event log.

Retention option, per table: `keep forever` (default), `archive after <duration>` (moved to object storage, still queryable), or `drop after <duration>`. The last two depend on time segments and are rejected until phase 7.

**`versioned`**: insert, update, delete. Each change writes a new version stamped with the commit timestamp. Reads see the latest version at their snapshot. Options: `retention = duration | forever`.

**derived**: a table whose rows are computed from a source table by a fold, maintained in the same op as the source insert. Definition:

```
derived table balance
  from   entry                          -- immutable source, same table group
  key    (user_id, asset)               -- must start with the partition key
  fold   credits  = sum(amount where kind = credit),
         debits   = sum(amount where kind = debit),
         held     = sum(amount where kind = hold) - sum(amount where kind = release),
         count    = count(),
         last_ts  = max(_ts)
```

- Allowed aggregates: sum, count, min, max, last, first. All deterministic.
- **Derived tables are always in the same table group as their source**, so maintenance never crosses a shard. Something that affects two owners (a trade, a transfer) is modelled as one source row per owner, each in its owner's partition, written by one transaction (section 10).
- **Overflow-free sums.** The validator sizes each sum's accumulator so it provably cannot overflow: value width plus 64 bits for the row count, for example a `Decimal(38)` sum is held in 256 bits. A fold therefore can never fail, which is what lets inserts into the source be used in independent cross-shard transactions (document 04).
- Stored as a versioned table, so it supports snapshot reads and time travel.
- Preconditions may read derived tables. This is how "place the hold only if credits - debits - held >= amount" is expressed.

## 4. Operations

Every request carries one operation code and a batch of events for it.

| Operation | Events | Result |
|---|---|---|
| insert | rows of one table | per-event error codes, sparse (only failures listed) |
| upsert, update, delete | versioned tables only | per-event error codes |
| get | primary keys, column list | rows |
| scan | key range within a partition, limit, column list | rows |
| execute | program id, parameter sets | per-invocation result |
| query | plan (document 05) | column batches |

**Linked events:** an event may set a `linked` flag; a chain of linked events succeeds or fails as a unit. This covers simple multi-row atomic writes without a program.

**Conditions:** each write event may carry a precondition: `not_exists`, `exists`, `version = ts`, or `column op constant`. This is the lightweight-transaction feature.

## 5. Transaction programs

A program is a typed, bounded, deterministic procedure. It is validated when registered (or when sent inline) and invoked with parameters.

```
program transfer(from: Id128, to: Id128, amount: Decimal(38,2), id: Id128)
  let src = get balance[from]                  -- read
  require src.credits - src.debits >= amount   -- precondition, else abort code
        else InsufficientFunds
  require exists account[to] else NoSuchAccount
  insert transfer { id, from, to, amount }     -- write; derived tables update
  return src.credits - src.debits - amount
```

IR elements:
- `get table[key]`, `scan table[range] limit n` with n constant,
- `let`, arithmetic and comparison expressions, `if/else`,
- `require cond else Code`,
- `insert`, `update`, `delete`, `upsert`,
- `for row in scan(...)` with constant bound,
- `return` of a typed value.

Not in the IR: unbounded loops, recursion, time, randomness, external calls, dynamic table names.

Static checks at registration:
- type checking with no implicit casts,
- maximum number of reads and writes, computed from the bounds,
- **key provenance:** for each read and write, whether its key is derived only from parameters ("static keys") or from earlier reads ("dependent keys"). Programs with only static keys can run cross-shard. Programs with dependent keys are single-shard only.
- **shard set:** the partition-key expressions, so the client and server can compute which tablets an invocation touches from the parameters alone.

Execution model inside a shard:
- One invocation runs to completion on the shard thread with no interleaving. No locks, no latches, no MVCC conflict detection.
- Writes go to a per-op write buffer; on `require` failure the buffer for that invocation is discarded. So abort is free and leaves no trace except the error code in the reply.
- Reads inside a program see the writes of earlier events in the same op.

Later extension, not planned in detail: deterministic WASM procedures with fuel metering, for logic the IR cannot express.

## 6. Two-phase business patterns

For workflows that span external systems, the schema library provides a standard pattern rather than a protocol feature:
- insert a `pending` row with a timeout,
- later `post` or `void` it with a conditional write,
- a derived table counts pending amounts separately from posted,
- expiry is evaluated lazily against commit timestamps, deterministically.

## 7. Error model

- Every event returns a result code from a closed enum per operation. Codes are part of the protocol.
- Errors are per event, not per batch, unless events are linked.
- Results are deterministic, so they are identical on every replica and safe to cache in the session table.

## 8. Limits (initial)

| Limit | Value |
|---|---|
| Message size | 1 MiB |
| Columns per table | 1024 |
| Primary key encoded size | 512 bytes |
| Row encoded size | 64 KiB |
| Reads + writes per program invocation | 256 |
| Tables per cluster | 4096 |
| Programs per cluster | 4096 |

## 9. Semantics fixed during review (2026-11-17)

- **Duplicate inserts** into an immutable table with identical content succeed with the result code `exists_identical`; with different content they fail with `exists_different`. This makes every insert safely retryable and is required for independent cross-shard transactions.
- **Nulls.** No three-valued logic. A `Nullable(T)` value cannot be used in arithmetic or comparison directly; the program must use `is_null`, `unwrap_or(default)` or a `match`. The validator rejects anything else.
- **Time.** Programs cannot read a clock. They can read `commit_ts()`, the timestamp assigned to the transaction, which is identical on every replica and every participant.
- **Updates write full rows.** An update to a versioned table reads the current version and writes a complete new version. Partial-column storage is not supported.
- **Foreign keys** are not a schema feature. Referential checks are written as `require exists` in programs, which makes their cost and shard set explicit.
- **Derived tables fold over immutable sources only.**
- **Unique indexes.** `unique local (cols)` is enforced within a partition, inside one shard, at no extra cost. `unique global (cols)` is enforced across the cluster: the database maintains a hidden index table partitioned by those columns, and every insert into the base table becomes a coordinated cross-shard transaction between the base row's shard and the index entry's shard. It is available from phase 5 and costs what a coordinated transaction costs; use it for low-rate inserts such as external references, not on the hot path.
- **Definition language.** Schemas and programs are text files compiled by a tool into the binary form. The tool type-checks, classifies programs (single-shard, independent, coordinated), reports their shard sets and generates client code for Rust, Go and Python. There is no builder API.

## 10. Worked example: exchange settlement

The first application is an exchange. Transactions are between two arbitrary users, so with hash partitioning by user almost every trade crosses shards. The schema is designed so that the cross-shard part can never fail, which makes it an independent transaction: no locks, no aborts.

```
table group by_user partition by (user_id u64) hash

table entry immutable layout row in by_user          -- the ledger, one row per leg
  key (user_id, id)
  id Id128, user_id u64, asset u32, kind Enum8(credit, debit, hold, release),
  amount Decimal(38, 18), ref Id128                  -- order or trade id

derived table balance from entry key (user_id, asset) fold ...   -- as in section 3

table order versioned layout groups((status, filled), (rest)) in by_user
  key (user_id, id) ...

table trade_leg immutable layout columnar in by_user  -- analytics-facing
  key (user_id, trade_id) ...

-- single-shard, conditional: reserve funds when the order is placed
program place_order(user, order_id, asset, amount, ...)
  let b = get balance[user, asset]
  require b.credits - b.debits - b.held >= amount else InsufficientFunds
  insert entry { user, kind: hold, asset, amount, ref: order_id }
  insert order { ... }

-- cross-shard, independent: both sides are unconditional because funds are held
program settle_trade(trade_id, buyer, seller, base, quote, qty, cost, fee)
  on buyer:
    insert entry { buyer, kind: release, asset: quote, amount: cost, ref: trade_id }
    insert entry { buyer, kind: debit,   asset: quote, amount: cost, ref: trade_id }
    insert entry { buyer, kind: credit,  asset: base,  amount: qty,  ref: trade_id }
    insert trade_leg { buyer, trade_id, side: buy, ... }
  on seller:
    ... mirror image ...
```

Points this example fixes in the design:
- **Reserve, then settle.** The conditional step (is there enough balance?) is single-shard. The two-party step has no condition left to check. This is the pending/post pattern of section 6 and how exchanges work in practice.
- **Independent programs may contain only statements that cannot fail:** idempotent inserts into immutable tables, with their overflow-free folds. The validator enforces this; a program with a `require` that spans participants is classified coordinated.
- **`audit cond else Code`** is a non-aborting check allowed in independent programs: if the condition is false the transaction still commits and a row is written to a system violations table. It catches application bugs (a settlement without a matching hold) without breaking atomicity.
- **Shared hot rows.** An exchange fee paid into one fee account would put one shard into every transaction. Pattern: credit fees to a per-partition fee row on the payer's own shard (`user_id` of the payer, `asset = fee`), and total them with a query or a periodic sweep.
- **Same-group shortcut.** When both users happen to live in the same group, the transaction runs as an ordinary single-shard transaction.
