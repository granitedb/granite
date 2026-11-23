# 0004. The gateway's query engine

Status: accepted on 2026-11-23 (task T6.4.1). Document 05 and `DESIGN.md` record the engine as confirmed.

## Question

The SQL gateway (document 05 section 4) is built on an existing Rust query engine. Document 05 named Apache DataFusion as the candidate, to be checked at the start of phase 6. Is it fit: what is its state, what does its table-provider interface push down to the shards, how will partial aggregates be pushed down, and what is the protocol towards users?

## Decision

Apache DataFusion, pinned to **55.2.0** (released 2026-10-07), with default features off and only `sql` on (`gateway/Cargo.toml`). Every `datafusion-*` crate in `gateway/Cargo.lock` is at 55.2.0 too. Arrow comes through DataFusion's re-export (`datafusion::arrow`, arrow 59.3.3), so it is not a separate dependency. 55.3.0, published on 2026-11-17, was too new to take.

Shards push down projection, filter and limit through DataFusion's `TableProvider`. Partial aggregates are pushed down by a physical optimizer rule of our own (section "Partial aggregates"), with a logical rewrite as the fallback. Arrow Flight SQL stays the candidate user protocol; it is assessed below and decided with T6.4.5.

## The engine

| Item | Finding |
|---|---|
| Project | Apache Software Foundation top-level project; repository `github.com/apache/datafusion`, documentation `datafusion.apache.org`. |
| Licence | Apache-2.0 for all 125 releases on crates.io, 0.1.0 to 55.3.0. |
| Release cadence | A new major version every one to three months: 52.0.0 on 2026-01-12, 53.0.0 on 2026-03-23, 54.0.0 on 2026-06-08, 55.0.0 on 2026-08-18. Minor releases follow, and fixes are backported to the previous major (54.2.0 came out with 55.3.0 on 2026-11-17). |
| Stability | Each major version changes the public API. That is why the version is pinned exactly and upgraded on purpose, with the spike's tests as the check. |
| Toolchain | `rust-version` 1.94; the repository builds with 1.96.1. |
| Size | 241 packages in `gateway/Cargo.lock` with `default-features = false, features = ["sql"]`. |

Dates and licences are from the crates.io registry; the version list was read on 2026-11-23.

## The table-provider interface

References are to the published crates at 55.2.0, as `crate: path:line`. The same items are documented at `https://docs.rs/datafusion/55.2.0/datafusion/catalog/trait.TableProvider.html`.

The trait is `TableProvider` (`datafusion-session: src/table.rs:52`), re-exported as `datafusion::catalog::TableProvider`. A query reaches the provider through one call, `scan(state, projection, filters, limit)` (`src/table.rs:185`), or `scan_with_args` with the same three items in a `ScanArgs` (`src/table.rs:210`, `:428`). It returns an `ExecutionPlan` that produces Arrow `RecordBatch`es, one stream per partition. Its documentation (`src/table.rs:84`–`184`) fixes the order: filters, then limit, then projection.

| Pushdown | Supported through `TableProvider` | How |
|---|---|---|
| Projection | Yes, columns only | `projection` is a list of column indexes (`src/table.rs:90`–`96`). Computed expressions stay above the scan, in a `ProjectionExec`. A column used only by a pushed filter may be missing from the list (`src/table.rs:116`). |
| Filter | Yes | The provider answers `supports_filters_pushdown` (`src/table.rs:303`) with one of `Unsupported`, `Inexact` or `Exact` per conjunct (`datafusion-expr: src/table_source.rs:37`). `Inexact` and `Exact` filters arrive in `scan` as logical `Expr`s; for `Inexact` ones DataFusion keeps its own filter above the scan, for `Exact` ones it drops it. |
| Limit | Yes, only without `Inexact` filters | `limit` asks for at least that many rows. It is not pushed when any filter is `Inexact` (`src/table.rs:157`–`160`). |
| Partial aggregate | **No** | `scan` carries no aggregate, and no other method of the trait does. |
| Top-N, sort | No | Not in `scan`. At the physical level, `ExecutionPlan::try_pushdown_sort` (`datafusion-physical-plan: src/execution_plan.rs:991`) lets a source promise an order; a top-N fragment needs the same kind of rule as partial aggregates. |

The spike shows the first three: the scan receives projection `[0, 1]` of three columns and the filter `v > Int64(2)`, and the limit arrives only when no `Inexact` filter is present (`gateway/src/spike.rs`, tests `filter_projection_group_by` and `limit_reaches_scan_only_without_inexact_filters`).

What T6.4.2 has to translate: a filter arrives as a DataFusion `Expr` and must be turned into a fragment predicate (document 05, fragment IR). Each conjunct that maps onto fragment expressions is answered `Exact`, and the rest `Unsupported`, so the gateway evaluates them. Granite's types also need a mapping onto Arrow's, for example the 256-bit integers, which Arrow has only as `Decimal256`.

## Partial aggregates

DataFusion already plans every distributed aggregate in two steps. `AggregateMode` (`datafusion-physical-plan: src/aggregates/mod.rs:286`) has `Partial`, which runs on each input partition, and `Final` or `FinalPartitioned`, which merges the partial states. The spike's plan for `SELECT g, sum(v), count(*) FROM t WHERE v > 2 GROUP BY g ORDER BY g`, with a two-partition stub (one partition per shard group in the real provider), is:

```text
SortPreservingMergeExec: [g@0 ASC NULLS LAST]
  ProjectionExec: expr=[g@0 as g, sum(t.v)@1 as s, count(Int64(1))@2 as n]
    SortExec: expr=[g@0 ASC NULLS LAST], preserve_partitioning=[true]
      AggregateExec: mode=FinalPartitioned, gby=[g@0 as g], aggr=[sum(t.v), count(Int64(1))]
        RepartitionExec: partitioning=Hash([g@0], 2), input_partitions=2
          AggregateExec: mode=Partial, gby=[g@0 as g], aggr=[sum(t.v), count(Int64(1))]
            FilterExec: v@1 > 2
              DataSourceExec: partitions=2, partition_sizes=[1, 1]
```

The `Partial` aggregate sits on each scan partition with no exchange in between; the spike's test asserts that. That node is what the shards' `PartialAggregate` fragment replaces.

**How.** A `PhysicalOptimizerRule` of the gateway's own (`datafusion-session: src/physical_optimizer.rs:52`), registered with `SessionStateBuilder::with_physical_optimizer_rules` (`datafusion: src/execution/session_state.rs:1331`). It finds an `AggregateExec` in `Partial` mode whose input is Granite's scan, possibly through a `FilterExec` or `ProjectionExec` it can express in the fragment. If every group key is a column or fragment expression and every aggregate is `count`, `sum`, `min` or `max` without `DISTINCT`, it replaces the aggregate and the nodes under it with one Granite scan that sends a fragment ending in `PartialAggregate`. That scan's output schema is the replaced node's schema, that is, DataFusion's partial-state columns. The `Final` aggregate above is unchanged, merges the states with each accumulator's `merge_batch` (`datafusion-expr-common: src/accumulator.rs:279`), and so also merges a group that a shard flushed more than once (document 05, `PartialAggregate`).

Points the rule must respect, for T6.4.3:

- Rule order: it must run before `CombinePartialFinalAggregate` (`datafusion-physical-optimizer: src/optimizer.rs:138`), which fuses an adjacent `Partial` and `Final` into `Single` when there is one input partition. With a single shard group the rule must also handle `Single`, turning it into a `Final` over the fragment scan.
- State types: `count`'s state is a non-null `Int64` (`datafusion-functions-aggregate: src/count.rs:317`–`322`), while a fragment counts in `u64`. The scan must cast, and fail with an error on a value over `i64::MAX`.
- Overflow: DataFusion's integer `sum` wraps on overflow (`datafusion-functions-aggregate: src/sum.rs:316`, `:551`), while Granite's arithmetic is checked. A wrapped final sum would be a silently wrong answer, so the gateway needs a checked `sum` (a user-defined aggregate) or a wider state type, both in the partial state it reads from the shards and in the final merge.

**Fallback,** if the physical rule cannot be kept working (for example, a later DataFusion release changes how aggregates are split): a logical `OptimizerRule` (`datafusion-optimizer: src/optimizer.rs:83`, registered with `SessionStateBuilder::with_optimizer_rule`, `datafusion: src/execution/session_state.rs:1294`) rewrites `Aggregate(Filter(TableScan))` into a re-aggregation over a scan of a pre-aggregating table provider. That provider sends the `PartialAggregate` fragment and returns partial groups. The outer aggregate re-aggregates them: `count` becomes `sum` of counts, `sum` stays `sum`, `min` and `max` stay. It uses only public logical-plan interfaces. An aggregate that neither path can express (`DISTINCT` before the sketches of T6.2.5, or a user-defined aggregate) runs in the gateway over the projected and filtered rows, within the gateway's memory limit and spilling (T6.4.5).

## The spike

`gateway/src/spike.rs`, compiled only for tests. A stub `TableProvider` holds three columns in two partitions, answers every filter `Inexact`, records what `scan` receives, and returns its rows as Arrow batches through `MemorySourceConfig`.

```console
$ cargo test --manifest-path gateway/Cargo.toml --locked
running 2 tests
test spike::limit_reaches_scan_only_without_inexact_filters ... ok
test spike::filter_projection_group_by ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
```

`filter_projection_group_by` runs the query above and checks its result rows, the projection and filter the scan received, and the shape of the plan under the partial aggregate.

## Licence check and the cost in dependencies

The gateway has its own policy, `gateway/deny.toml`: the root `deny.toml` plus the exceptions below, checked separately so that they never apply to the server or fuzz workspaces. `just deny` runs it, and all three runs pass:

```console
$ just deny
cargo deny --locked check
advisories ok, bans ok, licenses ok, sources ok
cargo deny --locked --manifest-path gateway/Cargo.toml --config gateway/deny.toml check
warning[duplicate]: found 2 duplicate entries for crate 'foldhash'
... (one warning per duplicated crate, listed below)
advisories ok, bans ok, licenses ok, sources ok
cargo deny --locked --manifest-path fuzz/Cargo.toml check
advisories ok, bans ok, licenses ok, sources ok
```

Licences need no exception: DataFusion and every crate in its tree are under licences the root policy allows.

What the choice costs, as exceptions in `gateway/deny.toml`:

- Three advisories, all "unmaintained", none a vulnerability, and none with a maintained replacement on crates.io: RUSTSEC-2026-0543 (`chrono`), RUSTSEC-2026-0555 (`chrono-tz`) and RUSTSEC-2026-0542 (`humantime`, unmaintained because it wraps `chrono`). Arrow's date and time types are built on `chrono`, so every DataFusion version has them. Under the root policy they fail the check.
- Several versions of one crate, which the root policy forbids: `foldhash` 0.1 and 0.2, `getrandom` 0.2, 0.3 and 0.4, `hashbrown` 0.14, 0.15 and 0.17, `itertools` 0.14 and 0.15, `num-bigint` 0.4 and 0.5, `r-efi` 5 and 6, `syn` 2 and 3. In the gateway's policy, `multiple-versions` is "warn", so a new duplicate still shows in the output.
- Build time. `just gateway` (clippy, then the tests) from an empty target directory, on an Apple M4 Pro with 14 cores, shared with other builds (load average 59 to 220): before this change 14 s wall, 8 s CPU; after it 803 s wall, 470 s CPU (417 s user, 53 s system). The CPU time is the steadier figure on a shared machine. DataFusion is built twice, once checked for clippy and once compiled for the tests.

## Not in the server binary

The gateway is its own Cargo workspace (`gateway/Cargo.toml`, document 06 section 5), so none of its dependencies reach the server. On the root workspace at commit 726ad6f plus this change:

```console
$ cargo tree -p granite --locked -e all -i datafusion
error: package ID specification `datafusion` did not match any packages
$ cargo tree -p granite --locked -e all --prefix none | grep -ciE "datafusion|arrow|sqlparser|tokio|chrono"
0
```

The only external packages in `granite`'s tree are `proc-macro2`, `quote`, `syn`, `unicode-ident`, `zerocopy` and `zerocopy-derive`.

## Arrow Flight SQL

Flight SQL is a gRPC protocol for SQL over Arrow batches. Its Rust server side is `arrow-flight` with the `flight-sql` feature, from the same `arrow-rs` project and release train as DataFusion's arrow. At 59.3.3, the version that matches arrow 59.3.3, it is Apache-2.0, built on `tonic` 0.14, and the server is a trait, `FlightSqlService` (`arrow-flight: src/sql/server.rs:56`).

- **Fit for a read-only gateway.** Every method except `register_sql_info` has a default that refuses the call with `unimplemented`. The gateway implements only query execution (`get_flight_info_statement`, `do_get_statement`), prepared queries, and the catalog calls clients use to list tables. Updates (`do_put_statement_update`, `src/sql/server.rs:399`), ingestion and transactions stay unimplemented, so writes are refused by the protocol itself.
- **Clients.** The Apache Arrow project publishes a Flight SQL JDBC driver, and the Arrow ADBC project a Flight SQL driver for Python, Go and C. Both clients come from the Arrow project, not from us. ODBC drivers for Flight SQL exist; T6.4.5 checks which one to name.
- **Cost.** gRPC adds `tonic`, `prost` and `hyper` to the gateway's tree. TLS goes through `tonic`'s rustls features, as document 06 section 5 requires. Mutual TLS and the read-only credential map onto gRPC's TLS and request metadata.
- **Ready-made code.** `datafusion-flight-sql-server` (datafusion-contrib, 0.4.19) wraps a DataFusion `SessionContext` in a Flight SQL service. It is Apache-2.0 but outside the Apache project; 0.4.19 requires `datafusion ^55.1` and `arrow-flight ^59.3`, so it matches the pinned versions. T6.4.5 decides whether to use it or only learn from it.

Assessment: Flight SQL fits, and stays the candidate. It is not added in this change; T6.4.5 adds it with the gateway service.

## Alternatives considered

- **DuckDB**, through the `duckdb` crate (MIT). An embedded C++ engine with a strong optimiser. Rejected: it brings a C++ build and a foreign-function boundary into the gateway; a remote scan would be a table function, which receives projection and filters as DataFusion's provider does, so aggregate pushdown would need the same kind of optimizer extension, written against a C++ engine.
- **Polars** (MIT). A DataFrame library with a SQL front end. Rejected: its SQL is translated onto its own lazy DataFrame API, and we found no Rust interface comparable to `TableProvider` through which a remote source receives pushdowns.
- **Our own planner on `sqlparser`** (Apache-2.0, which DataFusion uses itself). Full control over pushdown, but joins, an optimiser, spilling and window functions would be ours to write. That contradicts "built on an existing engine" and is years of work for a team of 3 to 4.
- **Trino or Presto**, on the JVM. Trino's connector interface does have an aggregation pushdown hook. Rejected: a JVM service to run and tune, beside a team and a code base that are Rust only.

## Open

- T6.4.2: the real `TableProvider`, filter translation, and the mapping of Granite's types onto Arrow types.
- T6.4.3: the physical rule above, a checked `sum`, and the count cast.
- T6.4.5: Flight SQL and the ODBC driver to name.
