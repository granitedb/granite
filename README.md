# Granite

A strict, deterministic, replicated database for append-heavy transactional workloads with analytical scans over the same data. Written in Rust.

**Status: design phase. There is no usable software here yet.**

## What it aims to be

- **Replicated with Viewstamped Replication**, one consensus group per CPU core, shared-nothing.
- **Typed and strict:** schema-defined tables, no implicit casts, no silent overflow, every size bounded.
- **Append-first:** tables are declared `immutable` (event logs) or `versioned`; nothing is overwritten in place.
- **One-shot transactions** over a typed binary protocol, including serializable transactions across shards.
- **Row, columnar or mixed layout per table:** each table declares how its columns are grouped on disk, in one storage engine.
- **Analytics** through a separate read-only SQL gateway, at a consistent snapshot.
- **Tested by deterministic simulation** with fault injection as the primary method.

It draws on ideas from TigerBeetle, TiDB, ClickHouse and ScyllaDB. It is not affiliated with any of them.

## Documents

- [Design overview](DESIGN.md)
- [Storage engine](docs/01-storage.md)
- [Replication](docs/02-replication.md)
- [Data model and transactions](docs/03-data-model-and-transactions.md)
- [Cluster and cross-shard transactions](docs/04-cluster-and-cross-shard.md)
- [Query and protocol](docs/05-query-and-protocol.md)
- [Testing and engineering](docs/06-testing-and-engineering.md)
- [Roadmap](docs/ROADMAP.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: see [SECURITY.md](SECURITY.md).

## Licence

Apache-2.0. See [LICENSE](LICENSE).
