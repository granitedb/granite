# Phase 4. Multi-shard cluster

Phase exit: many groups across many nodes; shard-aware client; tablets split, merge and move under load with no invariant violation.

The phase is built in two parts. Phase 4a (E4.1 to E4.3) is a static cluster and comes before phase 5. Phase 4b (E4.4, E4.5) is the tablet lifecycle and comes after it.

Design reference: `docs/04-cluster-and-cross-shard.md` sections 0 to 3. "Invariant N" below means invariant N of its section 6.

## T4.1.1 Multi-shard process: one thread and data file per core, per-shard ports

**Goal.** One process per machine runs many independent shards, one per core, with nothing shared on the data path.

**Work.**
- A node process that starts C shard threads, each pinned to a core, each with its own data file, event loop, pools and listening port (`base_port + shard index`).
- Assignment of shards to drives, evenly.
- Supervision: a shard's fatal error is reported with its identity before the process aborts.
- Node configuration: cores for shards, cores for scan workers, drives, ports.
- Start, stop and status commands.

**Not in this card.** Cross-shard messaging inside the process (T4.1.3).

**Depends on.** T2.3.1, T0.3.5.

**Gates.**
- [ ] A node with N shards runs N independent 3-replica groups across three nodes; the phase 2 seed gates pass for every group in the simulator (10,000 seeds with 8 shards per node).
- [ ] No memory is written by more than one shard thread: the process passes thread sanitizer under load with zero reports.
- [ ] On real hardware, total throughput with N shards on N cores is at least 0.9 times N times the single-shard throughput, for N up to the core count (report).
- [ ] Each shard thread's affinity is exactly its configured core, and its pools are allocated on that core's NUMA node (checked at startup and in a test).
- [ ] One shard under heavy load does not raise another shard's p99.9 loop iteration time above 5 ms (real hardware).
- [ ] A drive failure affects only the shards assigned to that drive; the others keep serving (test with a fault target).

## T4.1.2 Group id and epochs in every message; rejection of misrouted traffic

**Goal.** A message can never be applied by the wrong group or under an outdated configuration.

**Work.**
- The header's group id, tablet epoch and schema version filled on every message.
- Checks on receipt: wrong cluster, wrong group, older configuration epoch.
- Typed replies for client traffic (`stale_map` with the newer entry); silent drop with a counter for peer traffic.
- The same checks for repair and sync messages.

**Not in this card.** Tablet ownership checks per key (T4.3.3).

**Depends on.** T4.1.1, T0.2.2.

**Gates.**
- [ ] Simulator test with deliberate misrouting (messages delivered to a random other shard, seed gate 100,000 with all faults): no misrouted message changes any state, and every phase 2 invariant holds for every group.
- [ ] A prepare, a prepare_ok, a view-change message, a repair block and a client request, each sent to the wrong group, are each rejected (five tests).
- [ ] A block offered for repair by a replica of another group is never installed, even with a matching checksum request (test).
- [ ] Removing the group check, as a deliberate defect, is caught within 1,000 seeds (injected bug).
- [ ] A client request with an old tablet epoch receives `stale_map` containing the current entry.
- [ ] Every rejection increments a per-reason counter visible in metrics.

## T4.1.3 Intra-process shard rings and cross-node connection management

**Goal.** Shards talk to each other efficiently: without the network inside a process, and with a bounded number of connections across nodes.

**Work.**
- Bounded single-producer single-consumer rings between every pair of shards in a process.
- One TCP connection per (peer node, shard) pair across nodes, established on demand, kept warm, re-established with backoff.
- A send interface that hides whether the peer is local or remote.
- Backpressure on both transports.
- Connection limits and accounting per shard.

**Not in this card.** Client connections (T4.3.2), TLS details (T3.5.4).

**Depends on.** T4.1.1, T0.3.3.

**Gates.**
- [ ] A full ring or a full send queue returns backpressure to the sender; nothing is dropped silently and memory stays constant (tests for both transports).
- [ ] The rings pass a concurrency model check (`loom` or equivalent) for the producer and consumer, and thread sanitizer under load.
- [ ] The number of open connections per shard never exceeds a stated bound derived from the cluster size; a test with 6 nodes of 32 shards checks the bound.
- [ ] A message between two shards in one process makes no system call (trace).
- [ ] After a peer node restarts, connections are re-established and traffic resumes within 100 ms of the peer listening (test).
- [ ] The simulator uses the same send interface, and messages between simulated shards of one node can be delayed and reordered like any other.
- [ ] Round-trip latency between two shards in one process is under 10 microseconds at the median on real hardware (report).

## T4.2.1 Metadata state machine: nodes, groups, schema, tablet map

**Goal.** One replicated source of truth for what the cluster consists of and who owns which keys.

**Work.**
- A second state machine on VSR holding: nodes with addresses, labels and liveness; groups with members and a configuration epoch; table groups, schemas and programs with a schema version; the tablet map per table group as an ordered list of (token range end, group id, tablet epoch); in-progress moves, splits and merges.
- Operations to change each, with validation.
- Queries: full snapshot, and changes since a version.

**Not in this card.** Distribution to shards (T4.2.2, T4.2.3), the balancer (T4.4.5).

**Depends on.** T2.3.1.

**Gates.**
- [ ] Model test, replicated: every operation's result and the full state equal a reference model, over random operation sequences.
- [ ] After every operation the tablet map of every table group covers `[0, 2^64)` exactly once: no gap, no overlap (assertion and model test).
- [ ] Every change to a tablet's ownership increases its epoch; epochs never decrease (assertion).
- [ ] Invalid operations are rejected with typed errors: a tablet assigned to an unknown group, a group with duplicate members, a group with two members on one node, a schema that fails validation (one test each).
- [ ] The metadata group passes the phase 2 seed gates as a VSR group (10,000 seeds with all faults).
- [ ] The state is bounded: limits on nodes, groups, tablets and tables are stated and tested at the limit.

## T4.2.2 Schema and program distribution to data groups at a defined op

**Goal.** A schema or program change reaches every data group, and within a group every replica switches at exactly the same op.

**Work.**
- A schema or program change committed in the metadata group is delivered to each data group and logged there as an op.
- The data group applies it when executing that op.
- Requests carry the sender's schema version; a mismatch gets a typed reply and the client refreshes.
- Ordering: a data group applies schema versions in order and never skips one.

**Not in this card.** The semantics of each schema change (T3.5.1).

**Depends on.** T4.2.1, T3.5.1, T3.3.5.

**Gates.**
- [ ] Within every group, all replicas apply a schema change at the same op (convergence checker, seed gate 100,000 with all faults and schema changes during load).
- [ ] No data group ever applies version N+1 before N, and none skips a version (assertion; test with reordered and duplicated deliveries).
- [ ] A schema change delivered twice is applied once.
- [ ] A group that was unreachable during a change applies it after it returns, before serving any request that needs it (test).
- [ ] A request with an older schema version than the group's gets `stale_schema`, and one with a newer version waits or gets a retryable error; neither is executed under the wrong schema (tests).
- [ ] After faults stop, all groups reach the latest schema version within a bounded virtual time (liveness check).

## T4.2.3 Subscription and cache protocol for shards and clients

**Goal.** Shards and clients keep an up-to-date copy of the cluster map without putting the metadata group on the data path.

**Work.**
- Shards subscribe to metadata changes and keep a local copy.
- Clients fetch the map from any node and cache it.
- Correction of stale caches by `stale_map` replies carrying the newer entry.
- Versioned incremental updates; a full resync when too far behind.
- Behaviour when the metadata group is unreachable: data requests keep working on the cached map.

**Not in this card.** Client routing logic (T4.3.2).

**Depends on.** T4.2.1, T4.1.2.

**Gates.**
- [ ] With the metadata group completely stopped, reads and writes to data groups continue for at least 10 minutes of virtual time with no error (simulator test).
- [ ] A client with a stale map receives `stale_map`, updates, and its retried request succeeds; no request is executed by a group that does not own the key (checker, seed gate 100,000 with map changes and all faults).
- [ ] A shard never acts on a map version older than one it has already applied (assertion).
- [ ] After a map change and after faults stop, every shard and client converges to the latest version within a bounded virtual time (liveness check).
- [ ] An incremental update applied to version N gives exactly the full snapshot of version N+1 (property test).
- [ ] The metadata group receives zero messages per data request (counter over 1,000,000 requests).

## T4.2.4 Cluster bootstrap and node join

**Goal.** A multi-node cluster is created by a script, not by hand, and a new node can join it.

**Work.**
- A bootstrap procedure: create the cluster id, format the metadata group, register nodes, create data groups with replicas spread across nodes and labels, create the initial tablet map.
- Placement: each group's replicas on different nodes and failure domains; primaries spread evenly; groups spread evenly over node triples.
- Node join: register, then create new groups on it or add its shards as standbys.
- Commands and a declarative cluster description file.

**Not in this card.** Removing nodes and replacing replicas (T7.1.3).

**Depends on.** T4.2.3, T4.1.1.

**Gates.**
- [ ] One documented command, given a cluster description file, creates a 6-node cluster with 32 shards per node from freshly installed machines; a CI job does the same with 3 nodes of 4 shards.
- [ ] No group has two replicas on the same node or, when labels allow, in the same failure domain (checked by a validator run on the result).
- [ ] For 6 nodes at replication factor 3, the number of groups on each of the 20 node triples differs by at most 1, and primaries per node differ by at most 1.
- [ ] Bootstrap is idempotent: re-running after a failure at any step completes without creating duplicates (test with a failure injected at each step).
- [ ] A node formatted for another cluster is refused when it tries to join.
- [ ] A seventh node joins a running cluster under load with no failed request other than retryable ones (simulator and real hardware).

## T4.3.1 Token hashing and tablet lookup

**Goal.** The function that decides which tablet a row belongs to, fixed forever as part of the format.

**Work.**
- `token = hash64(encoded partition key)` with a keyed hash and a cluster-constant seed stored at format time.
- Lookup of the owning tablet by binary search in the tablet map.
- Range-partitioned table groups use the encoded partition key itself.
- The same function in the server and in the client.

**Not in this card.** Routing and batching (T4.3.2).

**Depends on.** T1.4.1, T4.2.1.

**Gates.**
- [ ] At least 64 fixed test vectors (partition key bytes, seed, expected token) are committed and pass on Linux x86-64 and macOS ARM, in the server and in the client.
- [ ] The hash function, its version and the seed's origin are specified in document 04 and marked as part of the on-disk format.
- [ ] Over 100,000,000 sequential integer keys and over 100,000,000 random keys, the tokens spread across 1,024 equal ranges with a chi-squared statistic within the 99% interval for a uniform distribution (report).
- [ ] Lookup returns the owning tablet for tokens at 0, at 2^64 - 1 and at every range boundary and its neighbours (property test against a linear scan).
- [ ] Two clusters with different seeds give different tokens for the same key (test).
- [ ] Hashing a 64-byte key costs under 20 ns on real hardware (report).

## T4.3.2 Shard-aware client: split batches by tablet, reassemble

**Goal.** The application hands the client a batch; the client sends each event to the right group and returns results in the caller's order.

**Work.**
- Per-event routing by token to the primary of the owning group.
- Splitting a user batch by destination; coalescing concurrent calls into one request per (group, operation) with a small linger.
- Reassembly of results in the caller's order.
- Sessions per group; retry with the same request number; re-register on eviction.
- Map refresh on `stale_map` and on view hints; warm connections to every replica of each group in use.

**Not in this card.** Cross-shard programs (T5.2.7).

**Depends on.** T4.3.1, T4.2.3, T2.2.9.

**Gates.**
- [ ] For random batches across many tablets, results are returned in the caller's order and match the reference model, under retries, view changes and map changes (seed gate 100,000 with all faults).
- [ ] Every event is executed exactly once despite retries and re-routing (checker on per-event execution counts).
- [ ] After a primary fails, the client's requests succeed on the new primary without a new TLS handshake, using its warm connection (test), and the client-observed outage is under 300 ms in 99.9% of trials on real hardware.
- [ ] With 100 concurrent callers each inserting one row, the client sends fewer requests than calls; the coalescing ratio and the added latency (under the configured linger) are measured.
- [ ] A batch that spans a tablet whose group is unreachable returns per-event `unavailable` for those events only; the other events succeed.
- [ ] The client never sends a request larger than the message size (property test).

## T4.3.3 Ownership checks on the shard

**Goal.** A shard executes a write only for keys in tablets it currently owns, whatever the client believed.

**Work.**
- For every event, check that the key's token falls in a tablet owned by this group at the current epoch.
- The check happens during execution, on replicated state, so every replica decides identically.
- `stale_map` for keys not owned; `moving` for frozen tablets once phase 4b exists.
- The same check for reads.

**Not in this card.** Changing ownership (E4.4).

**Depends on.** T4.3.1, T4.2.2.

**Gates.**
- [ ] Invariant 6: over seed gate 100,000 with stale clients, misrouting and all faults, no write is accepted by a group that does not own the key at that tablet epoch. A checker verifies every accepted write against the metadata history.
- [ ] The ownership decision is identical on all replicas (convergence checker).
- [ ] Removing the check, as a deliberate defect, is caught within 1,000 seeds (injected bug).
- [ ] A batch with some owned and some not-owned keys executes the owned ones and returns `stale_map` for the others, each with the correct entry.
- [ ] The check costs under 50 ns per event on real hardware (report).
- [ ] Reads of a key not owned return `stale_map`, never "absent".

## T4.4.1 Model-check tablet move

**Goal.** A machine-checked design for moving a tablet between groups before any of it is coded, because a mistake here loses or duplicates data.

**Work.**
- A specification under `models/` of the move steps of document 04 section 2: preparing, reserve_ingest, block fetch, install_parts, tail catch-up, freeze, adopt, epoch bump, release.
- Interaction with view changes in both groups, crashes of either group and the metadata group, and concurrent writes.
- Properties: invariants 6 and 7.

**Not in this card.** Interaction with pending cross-shard transactions (T5.2.8), though the model leaves a hook for it.

**Depends on.** T4.2.1, T2.1.2.

**Gates.**
- [ ] The specification is in `models/` and `just model tablet-move` runs it to completion.
- [ ] Invariants 6 and 7 hold for two groups of 3 replicas, at least 3 keys, 2 concurrent writes, one view change in each group and one crash at any step; the explored state count and bounds are recorded.
- [ ] Three deliberate defects each produce a counterexample: adopting before the final delta, bumping the epoch before adopt is committed, releasing before the epoch bump (three injected bugs).
- [ ] A property shows that at every state exactly one group accepts writes for the tablet, or none during the freeze.
- [ ] A property shows that an aborted move leaves the source as the owner with no data lost and the target with no visible rows.
- [ ] A table maps every action to the task and function that will implement it.

## T4.4.2 Split and merge

**Goal.** Tablets change size without moving any data.

**Work.**
- Split: cut a tablet at a token chosen from its key distribution; both halves stay in the group. A metadata change plus a marker op in the group's log.
- Merge: join two adjacent tablets in the same group.
- Thresholds on size and load; the target size of 16 GiB.
- Per-tablet size and load accounting on the shard, reported to the metadata group.

**Not in this card.** Deciding when to split (T4.4.5).

**Depends on.** T4.4.1, T4.3.3.

**Gates.**
- [ ] A split or merge writes no data block: the data-file hash, ignoring the WAL and the tablet marker, is unchanged (test).
- [ ] The tablet map covers the token space exactly once before, during and after every split and merge (assertion; seed gate 100,000 with all faults and concurrent load).
- [ ] Invariant 6 holds throughout (ownership checker).
- [ ] Requests in flight during a split or merge either succeed or get `stale_map` and succeed on retry; none is lost or executed twice.
- [ ] The split point divides the tablet's data so that neither half holds more than 60% (test on uniform and on skewed key distributions).
- [ ] A crash of the group or of the metadata group at any step leaves a consistent map after recovery (seed gate 10,000).
- [ ] Reported tablet size is within 5% of the true size (test).

## T4.4.3 Move: reserve_ingest, block fetch by checksum, install clipped parts

**Goal.** The bulk of a tablet moves as immutable part files, not row by row through the log, so a move takes minutes at production data sizes.

**Work.**
- Source: pin a manifest snapshot at timestamp Ts and list the parts overlapping the tablet.
- Target: log `reserve_ingest` with the part list, sizes and checksums; executing it allocates grid addresses deterministically and records the blocks as expected but absent.
- Each target replica fetches blocks from any source replica by checksum, at low priority.
- When a replication quorum of the target has all blocks, log `install_parts`, adding the clipped parts at their original levels.
- Ownership stays with the source.

**Not in this card.** Tail catch-up (T4.4.6), cut-over (T4.4.4).

**Depends on.** T4.4.1, T1.3.10, T2.2.6.

**Gates.**
- [ ] Target replicas stay byte-identical: the convergence checker passes at every checkpoint during and after ingest (seed gate 100,000 with all faults).
- [ ] A crash of any target replica between `reserve_ingest` and `install_parts` recovers: the replica resumes fetching, or state-syncs, and converges (seed gate 10,000 with crashes concentrated in that window).
- [ ] A block is written at its reserved address only if its checksum matches the reserved one (assertion; test with a wrong block offered).
- [ ] After `install_parts`, a read on the target restricted to the tablet's range at timestamp Ts equals the same read on the source (model test).
- [ ] Rows outside the tablet's range in a shared part are never visible on the target.
- [ ] An abandoned ingest releases every reserved block; none leaks (test).
- [ ] Fetching obeys the 1 ms slice rule, and commit latency on source and target stays within 10% of baseline during the fetch (report on real hardware).
- [ ] The open point on relocatable and clipped parts in document 04 section 8 is closed.

## T4.4.6 Move: tail catch-up through the target log

**Goal.** Writes that reach the source after the snapshot follow the tablet to the target, until the two are nearly level.

**Work.**
- The source streams rows of the tablet with `_ts` above Ts to the target.
- The target applies them through its own log as an ingest operation that keeps the original timestamps.
- Lag measurement, and a rate that outpaces the incoming writes.
- Idempotent application, so a retried batch is harmless.

**Not in this card.** The freeze and the final delta (T4.4.4).

**Depends on.** T4.4.3.

**Gates.**
- [ ] Under sustained write load on the tablet, the lag converges below 100 ms within a bounded time, in the simulator and on real hardware (report).
- [ ] If the write rate exceeds what catch-up can sustain, the move reports that it cannot converge and aborts cleanly; it does not run forever (test).
- [ ] Rows keep their original commit timestamps on the target (model test).
- [ ] A tail batch applied twice changes nothing the second time (test).
- [ ] At any point of the catch-up, the target's content for the tablet at timestamp T equals the source's at T, for every T at or below the target's applied position (model test).
- [ ] Target replicas stay byte-identical (convergence checker, seed gate 10,000 with all faults).
- [ ] Derived tables and indexes for the tablet's rows are correct on the target after catch-up (consistency scan).

## T4.4.4 Move: freeze, adopt, release, range delete

**Goal.** The cut-over: ownership passes from source to target with a pause of tens of milliseconds and no lost or duplicated row.

**Work.**
- Source logs `freeze tablet`: new writes get `moving`, a retryable code.
- The final delta is sent.
- Target logs `adopt tablet` and raises its clock floor above the tablet's highest timestamp.
- The metadata group bumps the tablet epoch.
- Source logs `release tablet`; its range is dropped by compaction.
- Abort and rollback at every step before adopt.

**Not in this card.** Draining pending cross-shard transactions before freeze (T5.2.8).

**Depends on.** T4.4.6.

**Gates.**
- [ ] Invariant 7: over seed gate 1,000,000 with moves under load and all faults, including crashes of source, target and metadata group at every step, no row is lost and none is duplicated. A checker compares every tablet's content with the reference model after each move.
- [ ] Invariant 6 holds throughout: at no moment do two groups accept writes for the tablet (ownership checker).
- [ ] Every timestamp the target assigns after adopt is higher than every timestamp in the moved tablet (assertion).
- [ ] The implementation's steps match the specification of T4.4.1; the mapping table is updated and each of that model's three defects, injected in code, is caught by the simulator.
- [ ] The freeze window, from `freeze` committed to the first write accepted by the target, is under 100 ms at the 99th percentile on real hardware (report).
- [ ] A move aborted at any step before adopt leaves the source serving the tablet with all data, and the target with nothing visible and no leaked blocks (seed gate 10,000).
- [ ] After release and compaction, the source's space for the tablet is reclaimed (measured).
- [ ] Clients see only retryable errors during the cut-over, and the shard-aware client hides them (end-to-end test with zero failed calls).

## T4.4.7 Move rate benchmark

**Goal.** A measured answer to how fast a tablet moves and what it costs the transactions running beside it.

**Work.**
- A benchmark that moves a 16 GiB tablet between two groups on different nodes while both serve an OLTP workload.
- Measurements: total move time, fetch rate, freeze window, and OLTP latency on both groups before, during and after.
- Runs at several fetch-rate limits.

**Not in this card.** Tuning the balancer.

**Depends on.** T4.4.4. Production-like servers.

**Gates.**
- [ ] All numbers are measured on real hardware and the report lists it.
- [ ] A 16 GiB tablet moves in under 10 minutes at the default rate limit (proposed threshold; must be confirmed before the run).
- [ ] During the move, OLTP latency p99 on source and target groups stays within 20% of the baseline, and p99.9 loop iteration stays under 5 ms (proposed threshold).
- [ ] The report gives move time against OLTP impact for at least three rate limits, and recommends a default.
- [ ] The freeze window distribution over 100 moves is in the report.
- [ ] The benchmark reruns with one `just` recipe.

## T4.4.5 Balancer

**Goal.** The cluster spreads data and load evenly by itself, without causing instability.

**Work.**
- A deterministic balancer inside the metadata state machine, driven by reported per-tablet size and load.
- Decisions: split, merge, move; a cap on concurrent moves.
- Hysteresis so it does not undo its own decisions.
- Constraints: failure domains, node capacity, draining nodes.
- Manual controls: pause, pin a tablet, move by hand.

**Not in this card.** Group creation and retirement (T7.1.3).

**Depends on.** T4.4.4, T4.4.2.

**Gates.**
- [ ] In simulation, from 100 random unbalanced starting layouts, the cluster converges: size per group within 10% of the mean and load per group within 20% of the mean, and then makes no further move for 1 hour of virtual time.
- [ ] No oscillation: over 24 hours of virtual time with steady load, no tablet moves more than once.
- [ ] The number of concurrent moves never exceeds the configured cap (assertion).
- [ ] The balancer is deterministic: all metadata replicas compute the same decisions (convergence checker on the metadata group).
- [ ] After a node is added, data spreads onto it within a stated time, with invariants 6 and 7 holding (seed gate 10,000).
- [ ] With the balancer paused, no split, merge or move is started.
- [ ] No decision places two replicas of a group on one node or breaks a failure-domain constraint (validator run on every decision in simulation).

## T4.5.1 Simulator: multi-group clusters with topology faults

**Goal.** The simulator runs whole clusters and randomises their topology changes, so cluster-level bugs are found the same way protocol bugs are.

**Work.**
- Simulated clusters with many nodes, many groups and the metadata group, in one thread from one seed.
- Per-seed randomisation: cluster shape, tablet moves, splits, merges, node add and remove, node crash and restart, all during load.
- Cluster-level checkers: ownership (invariant 6), row conservation (invariant 7), map coverage.
- Scaling of the simulator so a useful cluster runs fast enough.

**Not in this card.** The cross-shard serializability checker (T5.4.1).

**Depends on.** T4.2.4, T2.3.2, T2.3.3. The gates on moves need T4.4.4.

**Gates.**
- [ ] A simulated cluster of 6 nodes with 8 shards each runs 10 minutes of virtual time in under 60 seconds of real time.
- [ ] Over 10,000 seeds, every topology fault type (move, split, merge, node add, node remove, node crash) occurs in at least 1,000 seeds (coverage report).
- [ ] Each cluster-level checker detects its deliberate defect: a write accepted by a non-owner, a row dropped during a move, a gap in the tablet map (three injected bugs).
- [ ] No false alarm over seed gate 100,000 on the correct implementation.
- [ ] A failing cluster seed reproduces exactly from its printed command.
- [ ] The fleet (T2.3.4) runs cluster seeds, and its dashboard shows their share.

## T4.5.2 Scale benchmark: throughput against cores and nodes

**Goal.** Evidence that the shared-nothing design scales as claimed, or a clear account of where it does not.

**Work.**
- A single-shard-transaction workload with uniformly distributed keys.
- Runs at 1, 2, 4, 8, 16 and all cores on one node, and at 3 and 6 nodes.
- Throughput, latency and per-shard utilisation at each point.
- An analysis of any shortfall from linear scaling.

**Not in this card.** Cross-shard workloads (T5.4.2).

**Depends on.** T4.3.2, T4.2.4. Up to six production-like servers.

**Gates.**
- [ ] All numbers are measured on real hardware and the report lists it.
- [ ] The report gives throughput and latency (median, p99, p99.9) at every core count and node count listed above.
- [ ] Throughput at N shards is at least 0.85 times N times the single-shard figure up to the full core count of one node, and throughput at 6 nodes is at least 1.8 times that at 3 nodes; or every shortfall is explained with a measurement that identifies the cause (memory bandwidth, network, drive, client).
- [ ] The load generators are shown not to be the bottleneck (their utilisation is in the report).
- [ ] p99.9 loop iteration stays under 5 ms on every shard in every run.
- [ ] The benchmark reruns with one `just` recipe and is added to the tracked set.
