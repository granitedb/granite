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

Schemas are written as text files and compiled into the encoding of section 1.2 by `granite-schemac` (`crates/schemac`), which is client tooling: the server never links it and only the binary form crosses the wire. Its output is a function of the input bytes alone. Derived tables and indexes are added to the syntax by their own tasks; programs are written in the syntax of section 5.3 and compiled by the same tool.

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
            | ( "Timestamp" | "Duration" ) "(" number ")" | "Uuid" | "Id128"
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
- Arithmetic takes operands of one type and returns that type. Decimal(P, S) keeps scale S: a product is a*b/10^S and a quotient a*10^S/b, and each fails with loss of precision unless it is whole. Integer division truncates toward zero; the remainder has the dividend's sign. A result that does not fit P digits, or the integer's range, is overflow (`granite_types::arith`).
- Default is NOT NULL. Defaults must be constants.
- Row size has a static upper bound computed from the schema (section 2.4); it must fit the message size with headroom, otherwise the schema is rejected.

Values in memory (`crates/types`): integers are the Rust integers of their width; u256 and i256 are two 128-bit halves, `hi` (u128, or i128 for i256) and `lo` (u128), whose two's complement value is `hi * 2^128 + lo`. A Decimal(P, S) holds its mantissa `m`, the value being `m / 10^S`, in an i32 for P <= 9, an i64 for P <= 18, an i128 for P <= 38 and an i256 above. The mantissa has at most P digits, |m| < 10^P: a value with more is overflow, and an encoding of one is malformed. Date is an i32, Timestamp and Duration an i64 (units of 10^-p seconds), Uuid 16 bytes in RFC 9562 order, Id128 a u128, Enum8 and Enum16 the u8 or u16 index of a declared value. P is from 1 to 76, S from 0 to P; FixedBytes(N) needs N from 1 to the row size limit of section 8; Nullable and LowCardinality do not nest in themselves, nor does Nullable through LowCardinality.

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
program transfer(from: Id128, to: Id128, amount: Decimal(38, 2), id: Id128)
  let available = match get balance[from] {    -- read
    null => Decimal(38, 2) 0,
    src => src.credits - src.debits
  }
  require available >= amount else InsufficientFunds   -- precondition, else abort code
  require exists account[to] else NoSuchAccount
  insert transfer { id, from, to, amount }      -- write; derived tables update
  return available - amount
```

The text syntax is that of section 5.3, and the tables are those of `schemas/bank.schema`. A `get` gives the row or null, and a null is never an operand (section 9), so the balance is read with a `match`.

IR elements:
- `get table[key]`, `scan table[range] limit n` with n constant,
- `let`, arithmetic and comparison expressions, `if/else`,
- `require cond else Code`,
- `insert`, `update`, `delete`, `upsert`,
- `for row in scan(...)` with constant bound,
- `return` of a typed value.

Not in the IR: unbounded loops, recursion, time, randomness, external calls, dynamic table names.

Static checks at registration (section 5.2):
- type checking with no implicit casts,
- maximum number of reads and writes, computed from the bounds,
- **key provenance:** for each read and write, whether its key is derived only from parameters ("static keys") or from earlier reads ("dependent keys"). An access that happens or not depending on a read (inside an `if` or `match` on a read, or inside a `for`) is dependent too, since which keys it touches depends on the read. Programs with only static keys can run cross-shard. Programs with dependent keys are single-shard only.
- **shard set:** the partition-key expressions, so the client and server can compute which tablets an invocation touches from the parameters alone.

Execution model inside a shard:
- One invocation runs to completion on the shard thread with no interleaving. No locks, no latches, no MVCC conflict detection.
- Writes go to a per-op write buffer; on `require` failure the buffer for that invocation is discarded. So abort is free and leaves no trace except the error code in the reply.
- Reads inside a program see the writes of earlier events in the same op.

Later extension, not planned in detail: deterministic WASM procedures with fuel metering, for logic the IR cannot express.

### 5.1 Program IR and its encoding (part of the format)

A program travels and is stored as a typed tree (`granite_txn::Program`) in the binary form below, IR version 1. Integers are little-endian. The decoder reads at most one message (section 8) and returns the tree or a typed error; whether the tree is a valid program (types, names, bounds, scopes) is decided by the validator, not by the decoder. The encoding is canonical: a tree has exactly one encoding, and bytes the decoder accepts are exactly the encoding of the tree it returns. A change to any element needs a new IR version.

```
program    u16 IR version (1), name, u8 parameter count,
           per parameter: name, type,
           block (the body)
block      u16 statement count, statements
exprs      u16 expression count, expressions
name       u8 length, UTF-8 bytes (table names, error codes, the program and its parameters)
type       as in section 1.2, a valid type
```

Statements, one opcode byte then the operands:

| Opcode | Statement | Operands | Meaning |
|---|---|---|---|
| 0 | `let` | expr | Binds the value to the next local. |
| 1 | `if` | expr, block, block | The first block if the condition holds, else the second (empty for no `else`). |
| 2 | `require` | expr, name | Aborts the invocation with the named error code unless the condition holds. |
| 3 | `insert` | name, exprs | Inserts a full row into the named table, columns in declaration order. |
| 4 | `upsert` | name, exprs | Inserts or replaces a full row. |
| 5 | `update` | name, exprs, u16 count, per column: u16 column index, expr | Reads the row with the primary key `exprs`, replaces the listed columns and writes the full new row (section 9). |
| 6 | `delete` | name, exprs | Deletes the row with the primary key `exprs`. |
| 7 | `for` | scan, block | Runs the block once per scanned row, with the row bound to the next local. |
| 8 | `return` | expr | Returns the value. |

```
scan       name (table), exprs (key prefix), bound (lower), bound (upper), u16 limit
bound      u8: 0 none, 1 inclusive, then expr, 2 exclusive, then expr
```

A scan reads, in key order, at most `limit` rows of the table whose primary key starts with the prefix values and whose next key column is within the bounds. `scan table[range] limit n` of section 5 is the `for` over it; the limit is a constant of the encoding, so no loop is unbounded.

Expressions, one opcode byte then the operands:

| Opcode | Expression | Operands | Meaning |
|---|---|---|---|
| 0 | literal | type, value | A constant of the type. |
| 1 | parameter | u8 index | A parameter of the program. |
| 2 | local | u16 number | A local. |
| 3 | field | expr, u16 column index | A column of a row. |
| 4 | `get` | name, exprs | The row of the named table with that primary key, or null. `exists t[k]` is `not is_null(get t[k])`. |
| 5 | `commit_ts()` | | The commit timestamp of the transaction (section 9). |
| 6 | `-` | expr | Negation. |
| 7 | `not` | expr | Boolean negation. |
| 8 | `is_null` | expr | Whether a nullable value is null. |
| 9 | `unwrap_or` | expr, expr | The value, or the second when it is null. |
| 10 | `match` | expr, expr, expr | The second when the first is null; otherwise the third, with the value bound to the next local. |
| 11 | cast | expr, type | The value as the type; fails unless it is unchanged (section 2). |
| 12 to 24 | binary | expr, expr | `+`, `-`, `*`, `/`, `%`, `=`, `!=`, `<`, `<=`, `>`, `>=`, `and`, `or`, in that order. |

Every other opcode is rejected with `UnknownOpcode`. There is no element for time other than `commit_ts()`, for randomness, external calls, dynamic table names, unbounded loops or recursion: tables are named by constants, and the only loop is `for` over a scan with a constant limit.

Locals are numbered from 0 in the order of their binding elements (`let`, `for`, `match`) in the encoding.

A literal's value is the storage representation of section 2.2 for a fixed-width type, so a NaN must be the canonical one and a null's bytes zero. Otherwise LowCardinality(T) is T's value; Nullable(T) is a flag byte, 0x00 for null or 0x01 followed by T's value; and String and Bytes are a u32 length and the bytes. The value must be a valid value of its type (`Type::check`).

Limits, each tested at its value and one over:

| Limit | Value |
|---|---|
| Encoded program | one message (section 8) |
| Nesting depth: a body statement is at depth 1, and each statement or expression one deeper than the one containing it | the nesting depth of a program (section 8, `program_depth_max`) |
| Statements and expressions in one program | the statements and expressions per program (section 8, `program_nodes_max`), at most 65,535, so every count fits its u16 |
| Parameters | 255 |
| Name | 255 bytes |

The two limits from section 8 are format constants (`granite_base::constants::FORMAT`), compared between binaries by `check_format`: they decide which programs a replica accepts, so changing either value is a format change.

Because each statement and expression takes at least one byte, nesting is limited and lists grow element by element, the decoder's memory is proportional to its input and its recursion to the depth limit. Its largest single allocation is below 2 MiB: a list of at most `program_nodes_max` elements, or a copy of part of an input of at most one message.

Errors (`granite_txn::Error`), whose numbers are part of the protocol; the codes from 8 to 23 are the validator's (section 5.2), from 24 the classifier's (section 5.4):

| Code | Name | Meaning |
|---|---|---|
| 1 | `Malformed` | Truncated, trailing bytes, a tag out of range, invalid UTF-8 in a name, an invalid type; or, when encoding, a name over 255 bytes or more than 255 parameters. |
| 2 | `UnsupportedVersion` | An IR version this binary does not read, such as a newer one. |
| 3 | `UnknownOpcode` | A statement or expression opcode not in the tables above. |
| 4 | `TooDeep` | Nesting deeper than `program_depth_max`. |
| 5 | `TooManyNodes` | More statements and expressions than `program_nodes_max`. |
| 6 | `TooLarge` | An encoding longer than one message. |
| 7 | `InvalidLiteral` | A literal that is not a valid value of its type, or not in its canonical form. |

Example: `program t(a: u64)` with the body `require not is_null(get acc[a]) else E` and `return a + u64 1` encodes as

```
01 00  01 74  01  01 61 03  02 00                          version 1, "t", 1 parameter "a": u64, 2 statements
02 07 08 04 03 61 63 63 01 00 01 00  01 45                 require not is_null(get "acc"[param 0]) else "E"
08 0c 01 00 00 03 01 00 00 00 00 00 00 00                  return param 0 + (u64 1)
```

### 5.2 Validation

`Program::validate` checks a program against a valid catalog once, when it is registered or arrives inline, so that execution cannot be surprised. The first broken rule rejects the program with its code (table below) and its node: statements and expressions are numbered from 1 in the order of the encoding, and node 0 is the program as a whole. A tree that `Program::encode` rejects is rejected with that error at node 0, so the validator sees only trees within the limits of section 5.1.

Types. Every expression has a type: a value type, LowCardinality hints removed (they do not change the logical type), or a row of a table, which `get` gives as possibly null and `for` and `match` give as present. There is no implicit conversion: wherever a value of type T is needed, the expression must have type T, except that a T may be given where a Nullable(T) is needed, as every value of T is one of Nullable(T).

| Element | Rule | Type |
|---|---|---|
| literal, parameter, local | the parameter or local exists, and the local is in scope | its type |
| `commit_ts()` | | u64, like the hidden `_ts` column (document 01 section 4) |
| field | of a present row; the column exists | the column's type |
| `get t[k]` | t exists; one value per primary key column, each of the column's type | a row of t, or null |
| `-a` | an integer or decimal | a's |
| `a + b`, `-`, `*`, `/`, `%` | a is an integer or decimal, b of a's type | a's |
| `a = b`, `!=`, `<`, `<=`, `>`, `>=` | a is neither a float, nullable nor a row; b of a's type | bool |
| `a and b`, `a or b`, `not a`, conditions of `if` and `require` | bool | bool |
| `is_null(a)` | a nullable value or a row that may be null | bool |
| `unwrap_or(a, b)` | a nullable; b of the type a has when present | that type |
| `match v, n, p` | v nullable; the local is v present, in scope in p only; n and p of one type, or T and Nullable(T) | that type, Nullable(T) for T and Nullable(T) |
| cast | a non-null integer, decimal or float to an integer, decimal or float type | the target |
| `insert`, `upsert` | one value per column, each of the column's type | |
| `update t[k]` | the key as for `get`; each set column exists, is not a key column, is set once, and gets a value of its type | |
| `delete t[k]` | the key as for `get` | |
| `for` over a scan | the prefix gives at least the partition key and at most the primary key, each of its column's type; a bound has the type of the next key column, and needs one | |
| `return` | a value, not a row; every `return` of one type, or T and Nullable(T) | |

Nulls are never operands (section 9): a nullable value or a row that may be null where a value is needed is `NullableOperand`, and the program must test it with `is_null`, `unwrap_or` or `match`. Comparison orders integers and decimals by value and every other type by its key encoding (section 2.1); floats are not compared. `upsert`, `update` and `delete` need a versioned table (section 4).

Evaluation. Every operand of an expression is evaluated, left to right: `and` and `or` do not short-circuit and `unwrap_or` evaluates its default. Only `if` and `match` choose: `if` runs one of its blocks, `match` evaluates one of its two branches. So an access inside an expression happens whenever the expression is evaluated, unless it is in a branch of a `match`.

Scopes and returns. A `let` local is in scope from the statement after the `let` to the end of its block, a `for` local in the body, a `match` local in the third operand. A statement may return when it is a `return`, an `if` with a block that may return, or a `for` whose body may return; it must be the last of its block (`CodeAfterReturn`), so no statement runs after a return on any path. The program then returns on every path, or on none (`MissingReturn`); a `for` returns on no path when it finds no rows.

Bounds. A `get` reads one row, a scan up to its limit, an `update` reads and writes one row, and `insert`, `upsert` and `delete` write one. Statements in sequence add up, an `if` and a `match` take their costlier branch, and a `for` counts its scan plus its body times the limit. The validator reports the most reads and the most writes of one invocation, and rejects a program in which some path reads and writes more than the limit of section 8 (`program_accesses_max`, 256) with `TooManyAccesses`. The bound is exact for straight-line programs and an upper bound otherwise.

Key provenance. An expression depends on a read when it contains a `get`, `commit_ts()` (unknown until the transaction is ordered) or a local bound to such an expression, to a scanned row, or by a `match` on such a value. An access is **static** when its key does not depend on a read and nothing on its path does: no condition of an enclosing `if`, no value of an enclosing `match`, and no enclosing `for`. Otherwise it is **dependent**. The key is the primary key values of `get`, `update`, `delete` and the row of `insert` and `upsert`, and the prefix and bounds of a scan. A `require` or a run-time error before an access does not make it dependent: it aborts the whole invocation.

Shard set. For each access the validator reports its kind, its table, its provenance, its partition-key expressions (the first values of its key, one per partition key column), and its guard: the chain of conditions under which it runs, each an `if` condition or `is_null` of a `match` value with the value it must have, evaluated from the outermost. These are the program's own expressions, which the report borrows, so validation copies no part of the tree. Locals in them are resolved through the report: a `let` local is its expression, a `match` local the present value of its expression. For a program whose accesses are all static, evaluating from the parameters alone each access whose guards hold gives exactly the partitions an invocation that commits touches; an invocation that aborts touches a subset.

Validation takes time linear in the size of the program: programs of `program_nodes_max` nodes validate in under 0.2 ms in a release build (test `the_largest_valid_programs_validate_in_under_10_ms`).

| Code | Name | Rule |
|---|---|---|
| 8 | `UnknownTable` | Every table the program names is in the catalog. |
| 9 | `UnknownColumn` | Every column index, of a field or of an `update`, names a column of the table. |
| 10 | `UnknownParam` | Every parameter index names a parameter. |
| 11 | `UnknownLocal` | Every local is in scope where it is used. |
| 12 | `TypeMismatch` | Every value has the type its place needs (table above), where the two types have no cast between them, and every operator gets a type it takes. |
| 13 | `ImplicitCast` | Two different numeric types never meet without an explicit cast: `u64 + u32`, or a Decimal(18, 2) where a Decimal(18, 4) is needed. |
| 14 | `NullableOperand` | No nullable value, and no row that may be null, is used where a value is needed: in arithmetic, a comparison, a condition, a key, a non-null column or a field access. |
| 15 | `WrongArity` | A key has one value per primary key column, a row one per column, a scan prefix at most the primary key, and a bound needs a key column after the prefix. |
| 16 | `ImmutableTable` | `upsert`, `update` and `delete` write only versioned tables. |
| 17 | `KeyColumnUpdated` | An `update` sets no primary key column. |
| 18 | `ColumnSetTwice` | An `update` sets each column at most once. |
| 19 | `ScanAcrossPartitions` | A scan's prefix gives the whole partition key, so a scan stays in one partition (section 4). |
| 20 | `TooManyAccesses` | No path reads and writes more than `program_accesses_max` rows. |
| 21 | `MissingReturn` | A program returns on every path or on none. |
| 22 | `CodeAfterReturn` | No statement follows one that may return. |
| 23 | `ParamType` | An invocation's parameter values match the declared types: one per parameter, each a valid value of its type (`Type::check`), so null only for a Nullable parameter and a decimal within its precision. Not a rule of the program: `Program::check_params` checks each parameter set, and the executor runs it before the program. |
| - | write to a derived table | Reserved: rejected once derived tables exist (T3.4.1); the catalog has none yet. |

### 5.3 Program text syntax

Programs are written as text files and compiled into the IR of section 5.1 by `granite-schemac` (section 1.3), against the catalog of a schema file. The compiler runs the validator and reports its errors at the text of their node. Like schemas, programs are client tooling: only the IR crosses the wire.

```
program     = [ "independent" ] "program" name "(" [ param { "," param } ] ")" { statement } { section }
section     = "on" name { "," name } ":" { statement }
param       = name ":" type
statement   = "let" name "=" expr
            | if
            | "require" expr "else" name
            | ( "insert" | "upsert" ) name "{" [ column { "," column } ] "}"
            | "update" name key "{" [ name ":" expr { "," name ":" expr } ] "}"
            | "delete" name key
            | "for" name "in" "scan" name range "limit" number block
            | "return" expr
if          = "if" expr block [ "else" ( block | if ) ]
block       = "{" { statement } "}"
column      = name [ ":" expr ]
key         = "[" [ expr { "," expr } ] "]"
range       = "[" [ item { "," item } ] "]"
item        = expr | ( ">" | ">=" | "<" | "<=" ) expr
expr        = expr binary expr | "not" expr | "-" expr | expr "." name | "(" expr ")"
            | literal | name
            | ( "get" | "exists" ) name key
            | "is_null" "(" expr ")" | "unwrap_or" "(" expr "," expr ")"
            | "commit_ts" "(" ")" | "cast" "(" expr "," type ")"
            | "match" expr "{" "null" "=>" expr "," name "=>" expr "}"
binary      = "or" | "and" | "=" | "!=" | "<" | "<=" | ">" | ">=" | "+" | "-" | "*" | "/" | "%"
literal     = "true" | "false" | type value
value       = "null" | [ "-" ] ( decimal | "inf" | "nan" ) | "true" | "false" | quoted
decimal     = digit { digit | letter | "." }                   with "+" or "-" allowed after "e" or "E"
```

`type`, `name`, `quoted`, `number` and comments are those of section 1.3. A range gives the key prefix first, then at most one lower bound (`>` or `>=`), then at most one upper bound (`<` or `<=`): `[u, >= a, < b]` is a prefix of one value and both bounds.

- A file holds one program. Statements need no separator: each starts with its keyword.
- `on p:` starts a section for the participant whose partition key is `p`, a parameter or a local of the statements before the first section (one name per partition key column, in order). A section runs to the next `on` or the end of the file; its locals are in scope only in it. Every access in a section must name exactly that partition: its partition-key expressions (section 5.2) are the section's names, as written, and all its accesses are in one table group. Sections are text only: the IR holds their statements one after another, in order, so a section changes neither the program nor its class, and `print-program` prints the statements without them. They let the author state which participant does what, and the compiler check it.
- `independent` before `program` makes compilation fail unless the program is classified independent (section 5.4): at `independent` for a single-shard program, else at the first statement or expression that makes it coordinated, with what can fail there. Like sections, the annotation is not in the IR; a registration that states a class is checked against the class the server derives (section 5.4).
- Operators from the loosest: `or`, `and`, `not`, the comparisons, `+` and `-`, `*`, `/` and `%`, unary `-`, then field access. Binary operators are left-associative, so `a = b = c` is `(a = b) = c`. Every operand is evaluated (section 5.2).
- A name in an expression stands for the innermost local of that name in scope (section 5.2), else for the parameter. Locals are bound by `let`, `for` and the second branch of `match`. The words `and`, `or`, `not`, `get`, `exists`, `is_null`, `unwrap_or`, `match`, `commit_ts`, `cast`, `true`, `false`, `null` and the type names start an expression or join two, so a parameter or local spelled like one is quoted where it is used. Elsewhere, as after `get`, `.` or `else`, any name may be bare.
- `exists t[k]` is `not is_null(get t[k])`, and `else if` is an `else` block holding one `if`.
- `insert` and `upsert` give every column of the table, in declaration order, each as `column: expr`, or as the column alone for the parameter or local of that name. `update` names the columns it sets, in any order.
- `.column` names a column of the row an expression gives: a `get`, a `for` row, the local of a `match` on a row, or a `let` local bound to one of these.
- A literal is its type and its value, as in `u64 1`, `Decimal(38, 2) 0.50`, `Nullable(u64) null` or `String(16) "text"`; `true` and `false` alone are `bool`. Integers are written in decimal; a decimal with at most its scale of fractional digits; a float as Rust writes it, or `inf` or `nan`; a Date as days since 1970-01-01, a Timestamp or Duration as its units, an Id128 as its u128, an enum as the index of its value; String in quotes with `\"` and `\\` as escapes; Uuid, Bytes and FixedBytes as hexadecimal digits in quotes.
- Errors name the file, the line and the column, and either a program rule with its code (section 5.1 for codes 1 to 7, 5.2 from 8) or what the grammar expected. A validator error is reported where its node starts: a statement at its keyword, a binary operator at the operator, a field at its column's name, a literal at its type; node 0 (`MissingReturn`, and an encoding limit over the whole program) at `program`. The compiler finds unknown tables (`UnknownTable`), columns (`UnknownColumn`) and names itself: a name that is neither in scope nor a parameter is `UnknownLocal` where it was bound to a local earlier in the text and `UnknownParam` otherwise. A value that is not one of its type is `InvalidLiteral`, at the value; an invalid type, a file that is not UTF-8 (at its first invalid byte) and a 256th parameter are `Malformed`. More than `program_nodes_max` statements and expressions is `TooManyNodes` at the first one over, and text nested more than twice `program_depth_max` deep, parentheses included, is `TooDeep` where it gets too deep; a program within those but deeper than the IR allows is `TooDeep` at `program`. Code 23, `ParamType`, is a rule of each invocation's values, not of the text. The classifier's codes 24 to 26 are reported at their node like the validator's, with a sentence saying why.
- `granite-schemac print-program` prints an IR that is valid against the catalog, and whose parameters have distinct names, as canonical text: one statement per line, indented by two spaces per block; locals named by their numbers, `l0`, `l1` and so on, with an `_` added after the `l` while a parameter's name starts with the prefix; every column of a row written with its value; parentheses only where precedence needs them; decimals with exactly their scale of fractional digits; enums by their count. Compiling the printed text gives the same IR.
- `granite-schemac report` prints a program's canonical text, then, as comments: the most rows one invocation reads and writes, the return type, what each local is bound to, every access with its kind, table, key provenance, partition-key expressions and the conditions it runs under (section 5.2), and the class, with what makes a coordinated program coordinated, and the shard set (section 5.4). The reference programs are in `schemas/<schema>/*.program`, compiled against `schemas/<schema>.schema`; their reports are the golden file `schemas/programs.report`, which a test compares on every `just test`.

### 5.4 Classification

`Program::classify` decides, from a valid program and its report, how it runs (document 04 section 4). The compiler runs it after the validator, and the server runs it again when a program is registered, so a class is never taken from a client: a registration that states another class is rejected.

A **partition** is a table group and the partition-key expressions of an access (section 5.2). Two accesses name the same partition when they are in one group and their expressions are equal as written: `u` and a `let` local bound to `u` are two partitions. This is conservative: a program whose partitions coincide at run time is still classified by its expressions, and the client finds at run time that one group holds them all (document 04 section 4, same-group shortcut). The program's **shard set** is its partitions, in the order of their first access.

- **Single-shard:** every access names one partition, and its expressions are computed from the parameters alone, so the client finds the shard before the invocation runs. A program with no access is single-shard. A program with a dependent access (section 5.2) must be single-shard: if its accesses name more than one partition, or the partition depends on a read or on `commit_ts()`, it is rejected with `MustBeSingleShard`, at the first access that names another partition or such a one.
- **Independent:** all accesses are static, they name at least two partitions, and nothing the classifier cannot prove infallible.
- **Coordinated:** all accesses are static, they name at least two partitions, and something is not proved infallible: a `require`; an expression that can fail at run time (arithmetic, a negation or a cast, which can overflow, divide by zero or not fit); an `insert` into a versioned table, which fails when the key exists; an `insert` into an immutable table whose row is not fixed by the transaction (below); an `upsert`, a `delete` or an `update` (an update fails when the row does not exist; upserts become fallible with local unique indexes, T3.4.5). The classifier is conservative (document 04a section 5): what it cannot prove infallible costs speed, never correctness. The class names the first of these in the order of the encoding. Writes to a global unique index make a program coordinated too, once those indexes exist (T5.2.12).
- A program across more than `participants_max` partitions (8, document 04 section 4, "Bounds") is rejected with `TooManyParticipants`, at the first access that names one more.

What is proved infallible: reads, comparisons, `and`, `or`, `not`, `is_null`, `unwrap_or`, `match`, field access and `commit_ts()`; and an `insert` into an immutable table whose **whole row is fixed by the transaction**: every value, key and non-key, computed from parameters and literals alone, with no read, no `commit_ts()`, and no condition on either. A retry of the transaction writes the same row and gets `exists_identical` (section 9). Such an insert fails only when another row already holds its key, which the **fresh-id rule** excludes: the client gives every row it inserts into an immutable table an id that no other transaction, and no other insert of the same invocation, has used (section 9). That is a duty of the client, not a condition of the data, so the classifier assumes it. The fee of `settle_trade` in section 10 is a separate entry because `cost - fee` could overflow.

**No value crosses participants.** The participants of a program across partitions exchange votes and timestamps, never values (document 04 section 4), so each participant runs only the part of the program on its partitions and the part that needs no read. Such a program is rejected with `ValueAcrossParticipants`, at the node where the rule breaks, when:

- an expression, or the conditions it runs under, uses values read on two partitions;
- an access on one partition uses a value read on another (in its row, its new values, or the conditions it runs under);
- `commit_ts()` reaches a `require` or an expression that can fail. A coordinated participant decides whether its part fails when it votes, before the final timestamp exists; a value that cannot fail may use `commit_ts()`.

A single-shard program is not subject to this rule: one shard has every value.

| Code | Name | Rule |
|---|---|---|
| 24 | `MustBeSingleShard` | A program with dependent keys is single-shard: all its accesses name one partition, computed from the parameters. |
| 25 | `ValueAcrossParticipants` | A program across partitions moves no value between its participants, and decides every failure without `commit_ts()`. |
| 26 | `TooManyParticipants` | A program's accesses name at most `participants_max` partitions. |

Evidence: a corpus of hand-classified programs covering every class, cause and rejection, at most and one over `participants_max` partitions (`crates/schemac/tests/classification.programs`, test `classes_match_the_hand_classification`), and a property test that runs random programs classified independent on random state and parameters under the fresh-id rule, with no failure (`crates/txn/tests/independent.rs`).

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
| Nesting depth of a program | 64 |
| Statements and expressions per program | 8,000 |
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
program place_order(user: u64, order_id: Id128, entry_id: Id128, market: u32, asset: u32,
                    side: Enum8(buy, sell), price: Decimal(38, 18), qty: Decimal(38, 18),
                    amount: Decimal(38, 18), placed: Timestamp(6))
  let available = match get balance[user, asset] {
    null => Decimal(38, 18) 0,
    b => b.credits - b.debits - b.held
  }
  require available >= amount else InsufficientFunds
  insert entry { id: entry_id, user_id: user, asset, kind: Enum8(credit, debit, hold, release) 2,
                 amount, ref: order_id }
  insert order { user_id: user, id: order_id, market, side, price, qty,
                 filled: Decimal(38, 18) 0, status: Enum8(open, filled, cancelled) 0, placed }

-- cross-shard, independent: both sides are unconditional because funds are held;
-- b1 to b3 and s1 to s4 are fresh entry ids
independent program settle_trade(trade_id: Id128, buyer: u64, seller: u64, base: u32, quote: u32,
                                 qty: Decimal(38, 18), cost: Decimal(38, 18), fee: Decimal(38, 18),
                                 traded: Timestamp(6), b1: Id128, b2: Id128, b3: Id128,
                                 s1: Id128, s2: Id128, s3: Id128, s4: Id128)
  on buyer:
    insert entry { id: b1, user_id: buyer, asset: quote, kind: Enum8(credit, debit, hold, release) 3,
                   amount: cost, ref: trade_id }
    insert entry { id: b2, user_id: buyer, asset: quote, kind: Enum8(credit, debit, hold, release) 1,
                   amount: cost, ref: trade_id }
    insert entry { id: b3, user_id: buyer, asset: base, kind: Enum8(credit, debit, hold, release) 0,
                   amount: qty, ref: trade_id }
    insert trade_leg { user_id: buyer, trade_id, side: Enum8(buy, sell) 0, base, quote, qty, cost,
                       fee: Decimal(38, 18) 0, traded }
  on seller:
    insert entry { id: s1, user_id: seller, asset: base, kind: Enum8(credit, debit, hold, release) 3,
                   amount: qty, ref: trade_id }
    insert entry { id: s2, user_id: seller, asset: base, kind: Enum8(credit, debit, hold, release) 1,
                   amount: qty, ref: trade_id }
    insert entry { id: s3, user_id: seller, asset: quote, kind: Enum8(credit, debit, hold, release) 0,
                   amount: cost, ref: trade_id }
    insert entry { id: s4, user_id: seller, asset: quote, kind: Enum8(credit, debit, hold, release) 1,
                   amount: fee, ref: trade_id }
    insert trade_leg { user_id: seller, trade_id, side: Enum8(buy, sell) 1, base, quote, qty, cost,
                       fee, traded }
```

Points this example fixes in the design:
- **Reserve, then settle.** The conditional step (is there enough balance?) is single-shard. The two-party step has no condition left to check. This is the pending/post pattern of section 6 and how exchanges work in practice.
- **Independent programs may contain only statements that cannot fail:** idempotent inserts into immutable tables, whose rows the parameters fix, with their overflow-free folds; and reads. The classifier enforces this (section 5.4): a program across partitions with a `require`, arithmetic, an upsert, a delete or another element it cannot prove infallible is classified coordinated, and `independent` makes compilation fail if an edit makes it so. The `on` sections state which participant writes which rows (section 5.3).
- **`audit cond else Code`** is a non-aborting check allowed in independent programs: if the condition is false the transaction still commits and a row is written to a system violations table. It catches application bugs (a settlement without a matching hold) without breaking atomicity.
- **Shared hot rows.** An exchange fee paid into one fee account would put one shard into every transaction. Pattern: credit fees to a per-partition fee row on the payer's own shard (`user_id` of the payer, `asset = fee`), and total them with a query or a periodic sweep.
- **Same-group shortcut.** When both users happen to live in the same group, the transaction runs as an ordinary single-shard transaction.
