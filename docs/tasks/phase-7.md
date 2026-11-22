# Phase 7. Operations

Ongoing; starts alongside phase 4. There is no single exit: each epic is complete when its tasks are.

Design references: `docs/02-replication.md` sections 9 and 10, `docs/04-cluster-and-cross-shard.md` sections 3 and 7.

## T7.1.1 Model-check group reconfiguration

**Goal.** A machine-checked design for changing a group's members, which is the least-proven part of the replication design.

**Work.**
- A specification under `models/`: the logged `reconfigure` op naming the new member set, taking effect at the next checkpoint boundary; the old configuration commits the op; the new one takes over at the boundary; a new member counts toward quorums only after reaching that checkpoint by state sync.
- Interaction with view changes before, at and after the boundary, and with crashes.
- Properties: replication invariants 1 and 2 across the change.

**Not in this card.** The implementation (T7.1.2).

**Depends on.** T2.1.2.

**Gates.**
- [ ] The specification is in `models/` and `just model reconfigure` runs it to completion.
- [ ] Invariants 1 and 2 hold when replacing one member of a 3-replica group, with a view change at each of: before the op commits, between commit and boundary, at the boundary, after it; state count and bounds recorded.
- [ ] The same holds for growing from 3 to 5 and shrinking from 5 to 3.
- [ ] Three deliberate defects each produce a counterexample: the new member voting before it has reached the checkpoint; old and new configurations both able to commit the same op number; the change taking effect at different ops on different replicas (three injected bugs).
- [ ] A property shows that at no state can two disjoint quorums from different configurations both commit.
- [ ] A removed replica that keeps running cannot disturb the group (property).
- [ ] The design in document 02 section 9 is updated to match the checked specification.

## T7.1.2 Reconfigure op, epoch change at checkpoint boundary

**Goal.** A group's members can be changed while it serves traffic.

**Work.**
- The `reconfigure` logged op and the configuration epoch.
- The switch at the checkpoint boundary on every replica.
- New members brought up by state sync as standbys, then promoted by the op.
- Messages from an older epoch rejected.
- The metadata group informed of the new configuration.

**Not in this card.** Deciding which replica to replace (T7.1.3).

**Depends on.** T7.1.1, T2.2.11, T2.2.7.

**Gates.**
- [ ] Replication invariants 1 to 4 hold over seed gate 1,000,000 with replica replacement under load and all faults, including crashes of old and new members at every step.
- [ ] All replicas switch configuration at the same op (convergence checker).
- [ ] The implementation's steps match the specification of T7.1.1, and each of that model's three defects, injected in code, is caught by the simulator.
- [ ] A removed replica that restarts with its old configuration is rejected by every member and cannot affect a quorum (scenario test).
- [ ] During a replacement, the group remains available except for ordinary view changes; the longest write outage is under 200 ms (simulator, then real hardware).
- [ ] A replacement on real hardware of a 100 GiB replica completes, and its duration and the commit-latency impact are in a report.
- [ ] A reconfiguration interrupted by the loss of the new member can be cancelled or completed with another member, without manual repair of data files (test).

## T7.1.3 Node add, drain and remove workflows

**Goal.** Operators change the cluster's machines with commands that are safe to run and safe to interrupt.

**Work.**
- Add node: register, create groups on it or add its shards as standbys, let the balancer use it.
- Drain node: move each of its replicas elsewhere by reconfiguration, with a cap on concurrency.
- Remove node: deregister once it holds nothing.
- Replace a failed replica automatically after a timeout, or manually.
- Creation and retirement of whole groups.
- Commands that show progress and can be resumed.

**Not in this card.** Rolling upgrades (T7.2.1).

**Depends on.** T7.1.2, T4.4.5.

**Gates.**
- [ ] In the cluster simulator, node add, drain, remove and failed-replica replacement under load and all faults hold every replication and cluster invariant (cluster seed gate 100,000).
- [ ] After a drain completes, the node holds zero replicas and zero tablets, verified by a command, and removing it causes no error.
- [ ] A drain never reduces any group below its replication factor of healthy members: the new member is caught up before the old one is removed (assertion and checker).
- [ ] Each workflow interrupted at any step and re-run completes without duplicates or orphans (test with a failure injected at each step).
- [ ] On real hardware: add a seventh node, drain one node, remove it, under the exchange workload, with zero failed client calls and commit latency p99 within 20% of baseline (report).
- [ ] A node that fails permanently is replaced automatically within the configured timeout, restoring full replication (simulator and real hardware).
- [ ] The operator documentation covers each workflow, and its commands run in CI against a small cluster.

## T7.2.1 Versioned upgrade op and multi-version binaries

**Goal.** A cluster is upgraded one node at a time without stopping, and every replica changes behaviour at the same op.

**Work.**
- A protocol and format version in every message and in the superblock.
- A group runs at the minimum version all its replicas support.
- The switch to a new version as a logged op.
- Binaries carrying the previous release's state-machine logic, to replay old ops.
- Rules for what a release may change, and a compatibility test between consecutive releases.

**Not in this card.** Downgrade after the version op has committed, which is not supported.

**Depends on.** T2.3.1, T7.1.2.

**Gates.**
- [ ] In the simulator, a rolling upgrade with mixed versions under load and all faults holds every replication invariant, and storage is byte-identical at every checkpoint (seed gate 100,000 with two real versions of the state machine).
- [ ] All replicas of a group switch behaviour at the same op (convergence checker).
- [ ] A new binary replays a WAL written by the previous release and reaches a byte-identical state (test with committed data files from the previous release).
- [ ] A replica running a version older than the group's active version cannot join, and says why (test).
- [ ] Before the version op commits, replacing new binaries with old ones again works with no data change (rollback test).
- [ ] A CI job runs the upgrade test between the last release and the main branch on every pull request.
- [ ] A rolling upgrade of a 6-node cluster on real hardware under the exchange workload completes with zero failed client calls.
- [ ] The compatibility rules are documented, with a checklist that release reviews must follow.

## T7.2.2 Backup and restore

**Goal.** The cluster's data can be copied out and brought back as one consistent moment, as protection against operator error and software bugs.

**Work.**
- A backup of every group to object storage or a file system: checkpoint blocks plus the log needed to reach a chosen timestamp.
- One cluster-wide backup timestamp, chosen so every group's safe timestamp has passed it.
- Incremental backups using the immutability of parts.
- Restore into a new cluster, to the backup timestamp.
- Verification of a backup without restoring it.

**Not in this card.** The live standby site (E7.4), point-in-time restore to arbitrary timestamps.

**Depends on.** T5.3.1, T2.2.7.

**Gates.**
- [ ] A cluster restored from a backup equals the reference model's state at the backup timestamp, for every table, including under a cross-shard workload: no transaction is present on some groups and absent on others (cluster model test, seed gate 10,000 with backups taken during load and faults).
- [ ] A backup taken while a tablet move is in progress restores correctly: every row present exactly once (test).
- [ ] An incremental backup uploads only blocks not already in the previous backup (counter), and restoring from a chain of 10 incrementals gives the same result as from a full backup.
- [ ] Corrupting any single block of a backup is detected by verification and by restore; restore never produces a silently wrong cluster (test over 1,000 random corruptions).
- [ ] Commit latency p99 during a backup stays within 10% of baseline (real hardware).
- [ ] Backup and restore of 1 TiB on real hardware complete, with durations in a report.
- [ ] A restore drill is documented and its commands run in CI on a small cluster.

## T7.2.3 Server-stored named cursors for the change feed

**Goal.** A consumer can restart without keeping its own cursor.

**Work.**
- Named cursors stored in the group's replicated state: name, position, owner, last update.
- Commit of a cursor position by the consumer; resume by name.
- Limits on the number of cursors; expiry of abandoned ones by commit time.
- Authorisation by table group.

**Not in this card.** Exactly-once effect, which still needs the consumer to store its cursor with its state (T3.6.3); this is a convenience for at-least-once consumers.

**Depends on.** T3.6.2, T5.3.4.

**Gates.**
- [ ] A consumer that crashes and resumes by name receives every change after its last committed position, at least once, in order (seed gate 10,000 with all faults and consumer crashes).
- [ ] A cursor position survives a view change, a full-group restart and a state sync (three tests).
- [ ] Committing a position older than the stored one is rejected; positions never move backwards without an explicit reset (test).
- [ ] The cursor limit is enforced with a typed error, and an abandoned cursor expires deterministically on all replicas (convergence checker).
- [ ] A cursor on a tablet that moves to another group continues without a gap (test).
- [ ] The documentation states plainly that named cursors give at-least-once delivery and links to the exactly-once pattern.

## T7.2.5 Retention `drop after`: drop whole segments

**Goal.** Old data in immutable tables is removed by dropping whole time segments, cheaply and identically on every replica.

**Work.**
- The `drop after <duration>` table option, accepted now that time segments exist.
- A segment is dropped when its whole time range is older than the horizon, judged by commit timestamps.
- The drop as a deterministic step in op execution; blocks released through the free set.
- Reads below the horizon return a typed error.
- Interaction with derived tables, which keep their folded totals.

**Not in this card.** Archiving (T7.2.6).

**Depends on.** T6.1.5, T3.1.2.

**Gates.**
- [ ] All replicas drop the same segment at the same op (convergence checker, seed gate 100,000 with all faults).
- [ ] After a drop, the space of the segment is reclaimed within two checkpoints (measured).
- [ ] No row younger than the horizon is ever dropped (model test; assertion on the segment's maximum timestamp).
- [ ] A read or scan restricted to a dropped range returns the typed error `retention_expired`; a scan spanning live and dropped ranges states that it is truncated and does not return a silently partial result (tests).
- [ ] Derived tables over a source with dropped segments still hold the full folded totals (model test).
- [ ] The option is rejected on a table without time segments, with a typed error.
- [ ] The change feed for a dropped range returns `cursor_too_old`.

## T7.2.6 Retention `archive after`: move segments to object storage

**Goal.** Old segments leave the expensive local drives and stay queryable.

**Work.**
- The `archive after <duration>` table option.
- Upload of a segment's parts to object storage, with checksums; a manifest entry pointing to the archived location.
- Reads of archived data fetched on demand, through a local cache, at low priority.
- Archive decisions as deterministic ops; the upload itself outside the state machine, confirmed by an op.
- Handling of object-storage outages.

**Not in this card.** Tiering of hot data; only whole old segments are archived.

**Depends on.** T7.2.5, T7.2.2.

**Gates.**
- [ ] A scan over an archived segment returns exactly what it returned before archiving (model test).
- [ ] Local blocks of a segment are released only after a logged op confirms that a quorum has verified the archived copy by checksum (assertion; crash test at every step with no data loss).
- [ ] Replicas stay byte-identical in their local data files through archiving (convergence checker, seed gate 10,000).
- [ ] With object storage unreachable, queries on archived data fail with a typed error, and all operations on local data are unaffected (test).
- [ ] An archived block that fails its checksum is reported and never returned as data (test).
- [ ] Reads of archived data do not raise commit latency beyond the OLTP bound of phase 6 (real hardware).
- [ ] Latency of a first read and of a cached read of archived data are in a report, and the documentation states what "still queryable" costs.

## T7.4.1 Model-check consistent apply and promotion

**Goal.** A machine-checked design for the standby site, whose consistent-apply rule is new.

**Work.**
- A specification under `models/`: groups shipping logs asynchronously at different rates; the standby coordinator computing the apply timestamp Ta as the minimum over groups of the safe timestamp of what each has durably received; groups executing only up to Ta; promotion discarding everything beyond Ta; epoch fencing of the old site.
- Cross-shard transactions whose legs arrive at different times.
- Property: invariant 8 of document 04.

**Not in this card.** Implementation (T7.4.2 to T7.4.4).

**Depends on.** T5.1.2.

**Gates.**
- [ ] The specification is in `models/` and `just model standby-site` runs it to completion.
- [ ] Invariant 8 holds for 3 groups, at least 3 cross-shard transactions, arbitrary shipping delays and promotion at any moment: the promoted state equals the primary site's state at one timestamp; state count and bounds recorded.
- [ ] Three deliberate defects each produce a counterexample: applying prepares as they arrive; computing Ta from received instead of durably received logs; promoting without discarding beyond Ta (three injected bugs).
- [ ] A property shows the old site, after promotion, cannot commit anything the new site would accept (fencing).
- [ ] A property shows Ta never decreases.
- [ ] Document 04 section 7 is updated to match the checked specification.

## T7.4.2 Remote standbys with asynchronous log shipping

**Goal.** A second site receives every group's log without ever slowing the primary site.

**Work.**
- A remote standby per group at the standby site: receives prepares asynchronously, never votes, never acknowledges for quorum.
- Shipping from a backup, not the primary, where possible; batching and compression over the long link.
- Received prepares held durably in the standby's WAL, unexecuted, until the apply timestamp allows.
- Catch-up by checkpoint sync when the lag exceeds the WAL.
- Lag metrics per group.

**Not in this card.** The apply rule (T7.4.3), promotion (T7.4.4).

**Depends on.** T7.4.1, T2.2.11.

**Gates.**
- [ ] With the link to the standby site cut, slowed to 1 Mbit/s or delayed by 500 ms, commit latency p99 at the primary site stays within 2% of baseline (simulator, then real hardware).
- [ ] No primary-site quorum ever includes a remote standby (assertion).
- [ ] After the link returns, every group's standby catches up, by log or by checkpoint sync, and lag returns under 1 second (liveness check and real hardware).
- [ ] A standby never executes a prepare beyond the apply timestamp (assertion; trivially true until T7.4.3, then re-checked).
- [ ] A corrupted or misdirected shipment is rejected by checksum and re-requested (test).
- [ ] Lag per group is exposed in metrics in seconds and in ops.
- [ ] The metadata group has a remote standby that follows it (test).

## T7.4.3 Standby coordinator and apply timestamp

**Goal.** The standby site's state is always a state the primary site actually had, even though groups receive their logs at different speeds.

**Work.**
- A coordinator at the standby site computing Ta: the minimum over groups of the safe timestamp of what each has durably received.
- Each group executes ops with timestamps up to Ta and no further.
- Ta distributed to the standby shards and persisted.
- Behaviour when one group's shipping stalls: Ta stalls for all; alarms.
- The coordinator itself tolerant of its own crash.

**Not in this card.** Promotion (T7.4.4).

**Depends on.** T7.4.2, T5.3.1.

**Gates.**
- [ ] At every moment of the simulation, the standby site's executed state equals the primary site's state at timestamp Ta, for every table and every group (checker over cluster seed gate 100,000 with arbitrary shipping delays and all faults).
- [ ] No read at the standby site, and no promoted state, ever sees a cross-shard transaction applied on some groups and not others, although groups execute up to Ta at their own pace (checker).
- [ ] Ta never decreases, including across coordinator restarts (assertion and test).
- [ ] A coordinator crash stops Ta advancing but breaks nothing; a restarted coordinator resumes (test).
- [ ] With one group's shipping stalled, Ta stalls, the lag is reported with the group named, and unapplied prepares stay inside the WAL bound or the group falls back to checkpoint sync (test).
- [ ] The implementation matches the specification of T7.4.1, and each of that model's three defects, injected in code, is caught by the checker.
- [ ] In normal operation on real hardware, the standby's apply lag stays under 5 seconds (report).

## T7.4.4 Promotion with epoch fencing, and failback by state sync

**Goal.** After losing the data center, an operator makes the standby site the cluster with one command, loses at most seconds of data, and can later bring the old site back.

**Work.**
- A manual promotion command: each group discards prepares beyond Ta, the site takes a new cluster epoch, standby shards become voting replicas.
- Fencing: the old site, if it returns, is rejected by clients and by the new site.
- A report at promotion of the exact timestamp promoted to, so the data-loss window is known.
- Failback: the old site rejoins as the standby by state sync; no merge of divergent histories.
- A documented drill.

**Not in this card.** Automatic failover between sites.

**Depends on.** T7.4.3, T7.1.2.

**Gates.**
- [ ] Invariant 8: over cluster seed gate 100,000 with promotion at a random moment, the promoted cluster's state equals the primary site's state at exactly the reported timestamp.
- [ ] After promotion, the old site cannot commit anything that any client or the new site accepts: every message with the old epoch is rejected (scenario tests with the old site returning at each step).
- [ ] The promotion command prints the timestamp promoted to and the wall-clock time it corresponds to; the data lost is exactly the transactions after it (checker).
- [ ] A scripted site-failover drill on real hardware completes: the standby site serves the exchange workload within 10 minutes of the command (proposed threshold), and the measured data-loss window under normal conditions is under 5 seconds. Both figures are in a report.
- [ ] Promotion interrupted at any step and re-run completes; it never leaves some groups promoted and others not in a state that accepts writes (test with a failure injected at each step).
- [ ] Failback: the old site rejoins as a standby by state sync, with its divergent tail discarded, and the checker of T7.4.3 passes afterwards.
- [ ] The drill is documented step by step and is repeated on every release.

## T7.3.1 Metrics, tracing and structured logs per shard

**Goal.** An operator can see what every shard is doing and why it is slow, without attaching a debugger.

**Work.**
- Metrics per shard: commit latency, pipeline depth, compaction debt, cache hit rates, repair and scrub activity, loop iteration time, pool use, pending cross-shard transactions, safe-timestamp lag, feed and standby lag.
- An export endpoint in a standard format.
- Structured logs with shard, group, view and op.
- Tracing of a request across client, shard and participants by transaction id.
- Reference dashboards and alert rules.

**Not in this card.** A hosted monitoring service.

**Depends on.** T0.3.6, T4.1.1.

**Gates.**
- [ ] Recording metrics costs under 1% of throughput on the insert benchmark with everything enabled (real hardware).
- [ ] Metric and log recording never allocates on the commit path and never blocks the shard thread (counting allocator; a test with the export endpoint stalled).
- [ ] Reference dashboards exist for commit latency, compaction debt and repair, and each is checked against a live cluster in a CI job that queries the panels' expressions.
- [ ] Every metric has a name, unit and description in generated documentation, and CI fails if one is missing.
- [ ] A cross-shard transaction can be followed from the client through every participant by its id in traces (test).
- [ ] Alert rules exist for: no quorum, safe timestamp stalled, loop stall over 5 ms, pool exhaustion, replica behind, standby lag; each fires in a test that causes its condition.
- [ ] No secret, key or row content appears in logs or traces at default settings (test that greps a run's output).

## T7.3.2 Admin tool: inspect data file, WAL, manifest

**Goal.** An engineer can look inside a data file when something has gone wrong.

**Work.**
- `granite inspect` subcommands: superblock copies; WAL slots with their classification; manifest, levels and parts; a part's header, indexes and chunk statistics; a block by address with verification; the free set; session and pending-transaction tables.
- Read-only operation on a file that may belong to a running or crashed replica.
- Machine-readable output as well as text.
- A verify mode that checks every checksum in a file.

**Not in this card.** Repair by hand; the tool never writes.

**Depends on.** T1.2.6, T1.3.1.

**Gates.**
- [ ] The tool opens files read-only and cannot write: it has no write path to the data file, shown by the file's hash being unchanged after every subcommand (test) and by opening it on a read-only mount.
- [ ] Each subcommand has a test against a committed sample data file with expected output.
- [ ] The verify mode detects a single flipped bit anywhere in the file and names the zone, the address and the owner of the block (test over 10,000 random positions).
- [ ] The tool works on a file from a crashed replica, with a torn WAL head and a damaged superblock copy, and reports those conditions instead of failing (test).
- [ ] The tool has been used in at least one real debugging session, and the session is written up with what was missing; the gaps found are fixed or filed.
- [ ] Output formats are versioned.

## T7.3.3 C ABI over the Rust client core

**Goal.** One stable C interface to the Rust client, which the Go and Python clients are built on.

**Work.**
- A C header exposing: client creation, requests for each operation, program execution, queries, subscriptions, completion delivery, error codes.
- A threading and memory-ownership contract: who allocates, who frees, which calls may come from which thread.
- A completion-based interface that host languages can integrate with their own schedulers.
- Versioning of the interface.
- Builds as a static and a shared library.

**Not in this card.** The language bindings themselves (T7.3.4, T7.3.5).

**Depends on.** T4.3.2, T5.2.7.

**Gates.**
- [ ] The header is generated from the Rust source, committed, and CI fails if it is stale.
- [ ] A C test program exercises every function and runs under address sanitizer, thread sanitizer and Valgrind with zero reports.
- [ ] The interface is stable across a patch release: an ABI check tool compares the library with the previous release in CI.
- [ ] Every function documents ownership and thread rules, and a test violates none of them under a randomised multi-threaded driver for 1 hour.
- [ ] No Rust panic can cross the boundary: a test forces a panic inside and receives an error code.
- [ ] Both bindings below use this interface and nothing else: neither links Rust symbols directly (checked by symbol inspection).

## T7.3.4 Go client

**Goal.** Go applications use the database with an idiomatic, typed client.

**Work.**
- A Go module over the C ABI: connection, all operations, program execution, queries, subscriptions.
- Integration with Go's scheduler: completions delivered without blocking operating-system threads per request.
- Code generation of Go types and stubs from a schema.
- The client conformance suite, runnable by any client.
- Packaging with prebuilt libraries for Linux x86-64 and macOS.

**Not in this card.** A pure-Go implementation.

**Depends on.** T7.3.3, T3.5.2.

**Gates.**
- [ ] The Go client passes the full client conformance suite, including retries, failover, stale maps, cross-shard transactions and subscriptions, in CI against a real cluster.
- [ ] The suite passes under Go's race detector with zero reports.
- [ ] 10,000 concurrent goroutines making requests use a bounded number of operating-system threads (measured).
- [ ] Generated Go code for the exchange schema compiles and is used by an example that runs in CI.
- [ ] A row encoded by the Go client is byte-identical to the Rust client's encoding of the same row (differential test over all types).
- [ ] No memory leak over a 1-hour soak (heap profile flat) and no use-after-free across the boundary (address sanitizer on the C side).
- [ ] `go get` of the module on a clean Linux x86-64 machine works with no manual build step.

## T7.3.5 Python client

**Goal.** Python applications and analysts use the database, with query results as Arrow.

**Work.**
- A Python package over the C ABI: all operations, program execution, queries, subscriptions.
- Both blocking and `asyncio` interfaces.
- Query results exposed as Arrow tables without copying.
- Code generation of typed Python classes from a schema.
- Wheels for Linux x86-64 and macOS.

**Not in this card.** A SQL driver; SQL goes through the gateway's Flight SQL endpoint.

**Depends on.** T7.3.3, T6.3.4.

**Gates.**
- [ ] The Python client passes the full client conformance suite in CI against a real cluster, through both the blocking and the `asyncio` interface.
- [ ] A query result is an Arrow table whose buffers are the client's receive buffers: zero copies (address check), and it converts to a data frame with the standard Arrow call.
- [ ] The interpreter lock is released during every blocking call (test with a second thread making progress).
- [ ] A row encoded by the Python client is byte-identical to the Rust client's encoding (differential test over all types).
- [ ] No leak over a 1-hour soak (memory flat), and object lifetimes across the boundary are checked with address sanitizer on the C side.
- [ ] `pip install` of the wheel on a clean Linux x86-64 machine works with no compiler present.
- [ ] Generated classes for the exchange schema are used by an example that runs in CI.

## T7.3.6 External correctness audit (Jepsen-style)

**Goal.** An independent party tests the system's claims before any production release, because the people who built it are the worst placed to find what they did not think of.

**Work.**
- Selection of an external auditor with a record in distributed-systems testing.
- A written statement of the claims to test: guarantees per operation, the fault model, the limits.
- Support for the audit: a test cluster, access to the simulator and checkers, answers to questions.
- A response to every finding: fix, or a documented limit.
- Publication of the report.

**Not in this card.** A security audit, which is separate.

**Depends on.** Phases 0 to 5 complete; T7.1.3 and T7.2.1 complete.

**Gates.**
- [ ] The audit is performed by a party outside the team, with no contributor to the code base among the testers.
- [ ] The claims document is published before the audit starts and is what the audit tests.
- [ ] The audit covers at least: single-shard linearizability, cross-shard serializability, exactly-once execution under retries, durability under process and power failure, behaviour under partitions and clock faults, tablet moves and reconfiguration under load.
- [ ] Every finding is closed as fixed, with a regression test or simulator seed that reproduces it, or is documented as a stated limit in the user documentation.
- [ ] Every finding that the simulator could have caught results in a simulator or checker improvement, listed in the response.
- [ ] The report is published unedited, with the response beside it.
- [ ] No release is labelled production-ready before this card is closed.
