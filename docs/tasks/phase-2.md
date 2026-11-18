# Phase 2. Replication

Phase exit: a single group of 3 or 5 replicas passes the invariants in document 02 section 11 over a large number of simulator seeds with all fault types enabled, and runs on real machines.

Design reference for the whole phase: `docs/02-replication.md`. "Invariant N" below means invariant N of its section 11. "All faults" means network, storage, crash, pause and clock faults together, within the bounds of the fault model.

## T2.1.1 Read and annotate VR Revisited, protocol-aware recovery, TigerBeetle protocol docs

**Goal.** A written, reviewed list of every protocol rule we adopt and every place we deviate, before any protocol code exists, so nothing is implemented from memory.

**Work.**
- Read Liskov and Cowling, "Viewstamped Replication Revisited" (2012); Alagappan et al., "Protocol-Aware Recovery for Consensus-Based Storage" (FAST 2018); TigerBeetle's published protocol description.
- For each rule in document 02: the source and section it comes from, or "ours" with the reason.
- A list of deviations: quorum-gated suspicion, flexible quorums, nack-based truncation, checkpoint-based reconfiguration, reads without the log, request coalescing.
- A list of rules in the sources that we do not adopt, each with a reason.

**Not in this card.** The formal model (T2.1.2).

**Depends on.** Nothing.

**Gates.**
- [ ] A document under `docs/` lists every rule of document 02 sections 1 to 9 with a citation (paper and section) or the label "ours".
- [ ] Every deviation has: what the source does, what we do, why, and what could go wrong.
- [ ] Every statement in document 02 marked "verify against the paper" is either confirmed with a citation or corrected.
- [ ] Each of the six deviations named above is covered.
- [ ] The document is approved in review by two engineers, at least one of whom did not write document 02.
- [ ] Each deviation is linked to the task that model-checks or tests it.

## T2.1.2 Model-check normal operation and view change with storage faults

**Goal.** A machine-checked argument that the protocol is safe, including the storage faults the papers' proofs do not cover, before it is coded.

**Work.**
- A specification in TLA+ or with `stateright`, kept under `models/`: normal operation, view change with nack-based truncation, recovery with a corrupt or torn log, flexible quorums, quorum-gated suspicion.
- Safety properties: invariants 1, 2 and 6.
- A fault model inside the specification: message loss, duplication and reordering; crash; loss or corruption of log entries on fewer than a quorum.
- A recipe that runs the checker.

**Not in this card.** Reconfiguration (T7.1.1), clocks, liveness.

**Depends on.** T2.1.1.

**Gates.**
- [ ] The specification is in `models/` and `just model vsr` runs it to completion.
- [ ] Invariants 1, 2 and 6 hold for 3 replicas with at least 3 ops, 3 views, one crash and one corrupted log entry; the explored state count and the bounds are recorded.
- [ ] The same properties hold for 5 replicas, and for 6 replicas with quorums 3 and 4, at stated bounds.
- [ ] Three deliberate defects in the specification each produce a counterexample: quorums that do not intersect, truncation without enough nacks, a backup acknowledging before its write is durable (three injected bugs).
- [ ] Quorum-gated suspicion is modelled, and a property shows a single backup's vote does not change any replica's status.
- [ ] A table maps every action in the specification to the function that will implement it; the table is kept current by later tasks.
- [ ] The checker runs in the nightly workflow.

## T2.2.1 Replica skeleton: status, message dispatch, timeouts

**Goal.** The frame every protocol piece plugs into: a replica that starts, receives messages and fires timeouts, all under the simulator.

**Work.**
- Replica state: status (`normal`, `view_change`, `recovering`, `recovering_head`), view, log view, op, commit numbers.
- Message dispatch by command, with per-status acceptance rules.
- Timeouts driven by the `Time` trait, with deterministic backoff and per-replica jitter derived from (group, replica).
- A state-machine interface taking (op, timestamp, operation, body) and nothing else.

**Not in this card.** The handlers themselves (T2.2.2 onward).

**Depends on.** T0.4.5, T0.2.2, T0.3.1.

**Gates.**
- [ ] A group of 3 and of 5 replicas with a no-op state machine starts in the simulator and stays in status normal with no faults for 1,000 seeds.
- [ ] Every message kind of document 02 section 2 has a dispatch entry; a message with an unknown command, a wrong cluster id or a wrong group is dropped and counted (tests).
- [ ] Timeouts are a function of the seed and (group, replica) only: the same seed gives the same firing times (trace hash).
- [ ] Jitter differs between replicas of a group and between groups (test over 64 groups).
- [ ] The replica code compiles with no direct use of clock, network or disk APIs (lint).
- [ ] The state-machine interface cannot observe the view, the replica index or the wall clock (by its type signature).

## T2.2.2 Normal operation: prepare, prepare_ok, commit, pipeline

**Goal.** The steady-state protocol: a request becomes a durable, ordered, executed op on a replication quorum.

**Work.**
- Primary: assign op and timestamp, chain to the previous prepare, write to the WAL and send to backups concurrently.
- Backup: accept only in the same view, in status normal, extending its hash chain; write, then acknowledge. Gaps start repair.
- Commit on a replication quorum of acknowledgements including the primary's own durable write; execute in order; reply.
- Backups learn the commit number from the next prepare or a heartbeat and execute up to it.
- A pipeline of up to 8 prepares in flight; further requests queue with a bound.

**Not in this card.** View change (T2.2.3), repair (T2.2.4), sessions (T2.2.9).

**Depends on.** T2.2.1, T1.1.3.

**Gates.**
- [ ] With no faults, seed gate 10,000: all replicas hold identical committed ops in identical order, and invariant 3's log part holds.
- [ ] Invariant 6: a backup never sends an acknowledgement before its WAL write has completed. An assertion checks it, and removing the wait fails within 1,000 seeds with crash faults (injected bug).
- [ ] The primary never executes or replies before a replication quorum has acknowledged (assertion; test with one backup partitioned).
- [ ] An op is executed exactly once per replica and strictly in op order (assertion).
- [ ] With 8 prepares in flight, the ninth request waits; the queue is bounded and overflow returns `busy` (test).
- [ ] Under message drop, delay, reorder and duplication (no crashes), seed gate 10,000 holds invariant 1.
- [ ] A prepare that does not extend the backup's chain is never written to the WAL (test).

## T2.2.3 View change including nack-based truncation

**Goal.** When a primary fails, the group agrees on a new one without losing any committed op and without keeping an op that could not have committed.

**Work.**
- start_view_change, do_view_change, start_view as in document 02 section 4.
- The new primary selects the log with the highest (log view, op) from a view-change quorum.
- Uncommitted ops: kept unless enough nack_prepare messages prove no replication quorum held them; then truncated.
- An op whose prepare is corrupt everywhere but might have committed blocks the view change.
- Durable update of view and log view before acting on them.

**Not in this card.** Quorum-gated suspicion and timing (T2.2.14), WAL repair (T2.2.4).

**Depends on.** T2.2.2, T2.1.2.

**Gates.**
- [ ] Seed gate 100,000 with network faults and crashes: invariants 1 and 2 hold.
- [ ] A committed op is never truncated: a checker compares every truncation with the set of ops acknowledged to clients.
- [ ] Truncating with one nack fewer than required, as a deliberate defect, violates invariant 2 within 10,000 seeds (injected bug).
- [ ] Selecting the log by op alone instead of (log view, op), as a deliberate defect, is caught within 10,000 seeds (injected bug).
- [ ] View and log view are durable in the superblock before any message of the new view is sent (assertion; crash test).
- [ ] A scenario test constructs the "corrupt everywhere, possibly committed" case and shows the view change does not complete and nothing is truncated.
- [ ] After faults stop, a group with a healthy view-change quorum returns to status normal within a bounded virtual time in 10,000 seeds.
- [ ] Every handler corresponds to an action in the specification of T2.1.2; the mapping table is updated.

## T2.2.4 WAL repair: headers and prepares

**Goal.** A replica with a hole or a damaged entry in its log heals it from peers instead of stopping.

**Work.**
- request_headers and headers to repair the header chain.
- request_prepare by (op, checksum), answered by any replica that has it.
- nack_prepare from a replica that never had the prepare.
- Repair driven by the hash chain: the replica always knows the checksum it needs.
- Repair work in its own scheduling class, bounded in rate.

**Not in this card.** Grid repair (T2.2.6), state sync (T2.2.7).

**Depends on.** T2.2.3, T1.1.4.

**Gates.**
- [ ] Seed gate 100,000 with storage faults in the WAL (bit flips, torn writes, lost writes, misdirection), network faults and crashes: invariants 1, 2 and 3 hold.
- [ ] A repaired prepare is accepted only if its checksum equals the one the chain requires (assertion; test with a wrong prepare offered).
- [ ] A replica never sends nack_prepare for a prepare it might have held: slots classified corrupt are never nacked (assertion; injected bug that nacks corrupt slots fails within 10,000 seeds).
- [ ] After faults stop, every replica's log is complete and identical within a bounded virtual time (liveness check in 10,000 seeds).
- [ ] Repair traffic never delays the commit path beyond its budget: with one replica repairing its whole WAL, commit latency p99 on the others rises by under 10% in the simulator's latency model.
- [ ] Requests for ops outside the WAL window are answered with "not available", not silence (test).

## T2.2.5 Recovery and `recovering_head`

**Goal.** A replica that restarts with an uncertain log head rejoins without ever voting on the basis of a log it cannot verify.

**Work.**
- Recovery sequence: superblock, WAL classification, replay from checkpoint.
- If the head is uncertain (torn or corrupt head slots), enter `recovering_head`: take no part in view changes and do not acknowledge prepares until a start_view from a normal primary fixes the head.
- Transition rules into and out of each status.

**Not in this card.** Lagging by more than the WAL (T2.2.7).

**Depends on.** T2.2.4, T1.2.6.

**Gates.**
- [ ] Seed gate 100,000 with crashes combined with a torn or corrupted head: invariants 1 and 2 hold.
- [ ] A replica in `recovering_head` sends no do_view_change and no prepare_ok (assertion and test).
- [ ] Skipping `recovering_head` and voting immediately, as a deliberate defect, violates invariant 2 within 10,000 seeds (injected bug).
- [ ] A replica with a certain head goes straight to normal recovery; `recovering_head` is entered only when needed (counter in a scenario test).
- [ ] All replicas of a group crashing at once and restarting recover to a committed state that includes every op acknowledged to a client (seed gate 10,000).
- [ ] The status transition diagram is in document 02 and each edge has a test.

## T2.2.6 Grid repair and background scrubber

**Goal.** Damage in the grid is found before a reader trips on it and is repaired from a peer, relying on storage being byte-identical.

**Work.**
- request_block and block by (address, checksum).
- A read that fails verification queues the block for repair and the reader waits or retries.
- A scrubber that reads the whole grid at a fixed pace and repairs what fails.
- Scrub pace derived from op count or a virtual-time budget that does not affect storage state.

**Not in this card.** Fetching blocks from another group during a tablet move (T4.4.3), which reuses this path.

**Depends on.** T2.2.2, T1.1.5.

**Gates.**
- [ ] Seed gate 100,000 with grid corruption (bit flips, misdirected and lost writes) within the fault bound: every read eventually succeeds with correct data, and invariant 3 holds.
- [ ] A block offered by a peer is installed only if its checksum matches the reference (assertion; test with a wrong block).
- [ ] A corrupt block that nothing reads is found and repaired by the scrubber within one full scrub cycle (test).
- [ ] The scrubber never changes which blocks are allocated: two replicas with different scrub positions have byte-identical grids at checkpoints.
- [ ] A full scrub of a 1 TiB data file completes within a stated period at the default pace, and the commit-latency cost of scrubbing is measured on real hardware (report).
- [ ] A replica that cannot repair a block it needs (no peer has it) stops serving that read with a typed error and reports the condition; it does not return doubtful data.

## T2.2.7 Checkpoint coordination and state sync

**Goal.** A replica that has fallen further behind than the log can cover catches up by copying state instead of replaying.

**Work.**
- All replicas checkpoint at the same op numbers.
- Detection of "more than one checkpoint behind".
- State sync: obtain the latest checkpoint's superblock roots from a peer, confirmed by a quorum; adopt them; fetch missing grid blocks lazily and by scrubbing; become a normal backup when the manifest is complete.
- The syncing replica does not count toward quorums for ops it has not durably caught up to.

**Not in this card.** Reconfiguration and new members (T7.1.2).

**Depends on.** T2.2.6, T1.2.6.

**Gates.**
- [ ] A replica partitioned for longer than the WAL covers rejoins and reaches a byte-identical grid at the next checkpoint (seed gate 10,000 with all faults).
- [ ] A replica never adopts a checkpoint that a quorum has not confirmed (assertion; test with a single lying peer in the simulator).
- [ ] A crash at any point during sync leaves the replica able to restart sync or resume, never with a mix of two checkpoints that it treats as valid (seed gate 10,000).
- [ ] During sync the replica serves no reads and acknowledges no prepares beyond what it holds durably (assertion).
- [ ] Storage convergence holds at every checkpoint for all replicas that reached it (checker).
- [ ] Sync time for a 100 GiB data file over a 10 Gbit link is measured on real hardware and recorded.
- [ ] Commit latency on the healthy replicas during a peer's sync stays within 10% of baseline (report).

## T2.2.8 Clock synchronisation and timestamp assignment

**Goal.** Timestamps that strictly increase with op order and stay close to real time, without trusting any single machine's clock.

**Work.**
- A node-level sampler: ping and pong carrying monotonic and wall-clock samples, shared read-only by the shards of a node.
- Marzullo's algorithm over the samples to find the offset interval a quorum agrees on.
- A primary whose clock is outside the interval assigns no timestamps and steps down.
- Prepare timestamp: the maximum of the previous prepare's timestamp plus its batch size, the synchronised clock, and the hybrid-clock floor. Each event gets a distinct increasing timestamp.

**Not in this card.** Raising the floor from other groups (T5.2.5).

**Depends on.** T2.2.2.

**Gates.**
- [ ] Invariant 5: timestamps strictly increase in op order, across view changes, under clock drift, jumps and offsets (seed gate 100,000 with clock faults).
- [ ] Every event in a batch has a distinct timestamp, and all lie below the next prepare's first timestamp (assertion).
- [ ] A primary whose clock is set 1 second away from the others assigns no timestamps and a view change follows (scenario test).
- [ ] Backups never read a clock while executing: the timestamp comes from the prepare only (lint; and a test where backup clocks are set to random values gives byte-identical storage).
- [ ] With all clocks within 1 ms of true time, assigned timestamps stay within 2 ms of true time under load (simulator measurement).
- [ ] A wall clock stepping backwards never causes a decreasing timestamp (test).
- [ ] The Marzullo implementation has unit tests for agreeing, partially agreeing and disjoint sample sets.

## T2.2.9 Client sessions for writes in an LSM tree, inline and block-backed replies, eviction

**Goal.** Every write is executed exactly once even when the client retries or the primary changes, for thousands of clients per group.

**Work.**
- `register` as a logged op that creates a session.
- A session table in an internal LSM tree keyed by client id: last request number and last reply.
- Replies up to 4 KiB stored inline; larger ones in referenced grid blocks.
- Duplicate of the last request returns the stored reply; an older number is dropped; a number that skips ahead is rejected.
- Capacity 16,384; deterministic eviction of the least recently used session; an eviction message to the client.

**Not in this card.** Sessions for reads; reads have none.

**Depends on.** T2.2.2, T1.2.4.

**Gates.**
- [ ] Invariant 4 under retries, duplicated and reordered requests, and view changes: seed gate 100,000, with a checker that counts executions per (client, request number).
- [ ] The reply to a retried request is byte-identical to the original reply, including after a view change and after a restart of every replica.
- [ ] A group with 16,384 active sessions passes the same gate (10,000 seeds at that size).
- [ ] Eviction is deterministic: all replicas evict the same session at the same op, and storage stays byte-identical (convergence checker).
- [ ] An evicted client that retries is told to re-register and its old request is never executed a second time (test).
- [ ] A reply of 4 KiB is stored inline and one of 4 KiB plus one byte in a block; both are returned intact (tests).
- [ ] Removing the duplicate check, as a deliberate defect, violates invariant 4 within 1,000 seeds (injected bug).

## T2.2.12 Request coalescing: multi-frame prepares across clients and operations

**Goal.** Throughput no longer depends on clients batching: many small requests from many clients share one prepare.

**Work.**
- A prepare body as a sequence of request frames: (client, request number, operation, events).
- The primary fills the next prepare with every queued request that fits, in arrival order, from any client and any operation.
- Execution frame by frame, updating each client's session entry.
- Replies split back per client.
- Fairness: no client can starve the others.

**Not in this card.** Client-side batching (T4.3.2).

**Depends on.** T2.2.9.

**Gates.**
- [ ] On real hardware, 1,000 clients each sending single-row inserts reach at least half the throughput of one client sending full batches (report). The roadmap's "within 2x" is this ratio.
- [ ] Invariant 4 still holds with coalescing (seed gate 100,000 with all faults).
- [ ] Frames execute in the order they appear, and that order is identical on every replica (convergence checker).
- [ ] One failing frame does not affect other frames in the same prepare (test).
- [ ] With one client sending as fast as it can and 99 sending one request per second, the slow clients' p99 latency is within 2 times their latency without the fast client (simulator or real hardware; report).
- [ ] A prepare never exceeds the message size, and a request that alone fits a message is never rejected for size (property test).
- [ ] Fuzz gate 4 on the frame decoder.

## T2.2.13 Linearizable reads without the log (quorum-confirmed leadership)

**Goal.** Strongly consistent reads that cost no log write and assume nothing about clocks.

**Work.**
- On a read, the primary notes its commit number, then waits until a replication quorum has acknowledged a message it sent after that moment (the next prepare, or a heartbeat when idle).
- The read is then served from state at or after the noted commit.
- Backups acknowledge heartbeats so an idle primary can confirm.
- Reads queued during a view change are rejected so the client retries on the new primary.

**Not in this card.** Snapshot reads on backups (T3.1.4).

**Depends on.** T2.2.3, T1.5.2.

**Gates.**
- [ ] Invariant 7 under partitions and view changes: seed gate 100,000, checked by the linearizability checker (T2.3.2) over histories that mix reads and writes.
- [ ] Serving the read without waiting for the quorum confirmation, as a deliberate defect, produces a stale read within 10,000 seeds (injected bug).
- [ ] A primary cut off from every backup answers no linearizable read (scenario test).
- [ ] A read performs zero WAL writes (counter).
- [ ] When idle, added read latency is at most one heartbeat interval plus one round trip; under load it is under one round trip (simulator measurement; then confirmed on real hardware in a report).
- [ ] The technique is in the model of T2.1.2 or in an extension of it, with the property that a deposed primary cannot answer.

## T2.2.14 Quorum-gated suspicion and failover timing

**Goal.** A group accepts writes again within 200 ms of losing its primary, and one slow backup cannot depose a healthy primary.

**Work.**
- A backup that times out sends start_view_change as a vote and stays in the current view; status changes only on a view-change quorum of votes for the same view.
- The timing budget of document 02 section 0: 10 ms heartbeat, 60 ms suspicion plus up to 20 ms jitter.
- Jitter derived from (group, replica) so that the groups of a failed node do not synchronise.
- A view hint pushed to clients.
- A measurement harness for failover time on real machines.

**Not in this card.** Client retry policy (T4.3.2).

**Depends on.** T2.2.3, T0.3.6.

**Gates.**
- [ ] Invariant 9: over seed gate 100,000 with exactly one backup slow, paused or partitioned, zero view changes occur.
- [ ] Invariant 8 in simulation with the data-center latency model: after a primary crash with a healthy view-change quorum, the first write is accepted within 200 ms of virtual time in every one of 100,000 seeds.
- [ ] On real hardware (3 servers), 1,000 primary kills (`SIGKILL` and power-off of the node): time from kill to first acknowledged write is under 200 ms in at least 99.9% of trials and never over 500 ms; the distribution is in a report.
- [ ] On real hardware, with 64 groups on the killed node, all groups meet the same bound in the same trial.
- [ ] Reverting to "change status on own timeout", as a deliberate defect, violates invariant 9 (injected bug).
- [ ] The deviation from VR Revisited is in the specification of T2.1.2 and the checker confirms safety with it.
- [ ] No false view change occurs in a 24-hour run under benchmark load with no injected fault, on real hardware.

## T2.2.10 Ring replication with fallback

**Goal.** The primary's network bandwidth is not multiplied by the replica count.

**Work.**
- Each replica forwards a prepare to the next in the ring; the primary sends to one.
- Fallback: if an acknowledgement is not seen within a timeout, the primary sends directly.
- Ring order recomputed per view.

**Not in this card.** Changing quorum rules; this is transport only.

**Depends on.** T2.2.2.

**Gates.**
- [ ] With no faults, the primary sends each prepare body to exactly one replica (counter).
- [ ] A report gives the primary's outbound bytes per committed byte for direct and ring replication at replication factors 3 and 5, on real hardware.
- [ ] With one ring member down, commits continue and commit latency p99 rises by at most the fallback timeout (simulator test).
- [ ] All seed gates of T2.2.2, T2.2.3 and T2.2.4 pass unchanged with ring replication enabled.
- [ ] Commit latency at the median with a healthy ring is within 10% of direct replication at replication factor 3 (report on real hardware); if not, the default is direct and the report says so.

## T2.2.11 Standbys

**Goal.** A non-voting replica that follows the log, used for replacing members and later for the standby site.

**Work.**
- A standby receives prepares and executes committed ops, but never acknowledges for quorum and never votes.
- Standbys are listed in the group configuration.
- Promotion of a standby to a voting member by configuration (the mechanism of T7.1.2 completes this).
- Standbys serve snapshot reads and repair requests.

**Not in this card.** Online reconfiguration (T7.1.2), remote standbys (T7.4.2).

**Depends on.** T2.2.7.

**Gates.**
- [ ] A standby's acknowledgement is never counted toward a replication quorum, and it sends no view-change vote (assertions; test where only standbys acknowledge and nothing commits).
- [ ] A standby reaches byte-identical storage at every checkpoint it reaches (convergence checker, seed gate 10,000 with all faults).
- [ ] A standby that is down or slow has no effect on commit latency or on view changes (seed gate 10,000 comparing against runs without the standby).
- [ ] A group restarted with a configuration that promotes the standby runs with it as a voter and holds invariants 1 to 4 (seed gate 10,000).
- [ ] A standby that falls behind catches up by repair or state sync like any replica (test).

## T2.3.1 Connect the phase 1 state machine to VSR

**Goal.** The storage engine and the replication protocol become one replicated shard.

**Work.**
- The phase 1 operations run as the VSR state machine.
- One data file per replica holding the WAL, superblock and grid for both layers.
- Compaction, flush and checkpoint paced by committed op number.
- The storage convergence checker: compare grids and superblock roots of all replicas at every checkpoint.

**Not in this card.** New operations (phase 3).

**Depends on.** T2.2.7, T2.2.9, T1.5.1, T1.5.2.

**Gates.**
- [ ] The convergence checker passes at every checkpoint over seed gate 100,000 with all faults: every replica that reached the checkpoint has a byte-identical grid and identical superblock roots.
- [ ] A deliberate nondeterminism in the engine (iteration over a randomly seeded hash map in compaction) is caught by the checker within 1,000 seeds (injected bug).
- [ ] The checker also passes when replicas run with different cache sizes, different simulated disk speeds and different read loads.
- [ ] Results returned to clients are identical regardless of which replica was primary (checker).
- [ ] A 3-replica cluster on real machines runs the phase 1 end-to-end test and the data files of the three replicas hash identically at a quiesced checkpoint.
- [ ] No state-machine code reads anything but (state, op, timestamp, operation, body) (lint from T0.1.2).

## T2.3.2 Linearizability checker against the reference model

**Goal.** A checker that decides, for any recorded history of client calls and replies, whether a single correct database could have produced it.

**Work.**
- Recording of client histories in the simulator: invocation, completion, result, including calls that never return.
- A checker that searches for a linearization consistent with the reference model.
- Use of the commit timestamp or op number as a hint to keep the search fast, with the result still verified independently.
- A minimised history printed on failure.

**Not in this card.** Cross-shard serializability (T5.4.1).

**Depends on.** T1.5.3, T2.2.2.

**Gates.**
- [ ] Each of these deliberate defects is detected within 10,000 seeds: a stale read from a deposed primary, a lost acknowledged write after a view change, a write executed twice, a read that sees an uncommitted write (four injected bugs).
- [ ] No false alarm over seed gate 100,000 on the correct implementation with all faults.
- [ ] Calls with no reply are treated as possibly executed, and a test covers both outcomes.
- [ ] A history of 100,000 operations from 100 clients is checked in under 60 seconds.
- [ ] On failure the output contains the seed, a minimised history and the first operation that cannot be placed.
- [ ] The checker has its own unit tests with hand-written linearizable and non-linearizable histories.

## T2.3.3 Liveness checker

**Goal.** Safety checks cannot see a cluster that has stopped; this checker can.

**Work.**
- A phase at the end of each simulation where faults stop and a quorum is healthy.
- A check that, within a bound of virtual time, every replica is in status normal in the same view, every pending client request completes, and logs and grids converge.
- A report naming what was stuck when the bound is missed.

**Not in this card.** Performance bounds under faults.

**Depends on.** T2.2.5, T0.4.5.

**Gates.**
- [ ] Each of these deliberate defects is detected within 10,000 seeds: a timeout that is never rearmed, a repair request that is never retried, a view change that waits for a replica that is permanently down (three injected bugs).
- [ ] No false alarm over seed gate 100,000 on the correct implementation.
- [ ] The bound is stated in virtual time, justified in writing from the protocol's timeouts, and is not larger than 60 seconds.
- [ ] The failure report names the replica, its status, and the oldest unanswered message.
- [ ] The checker runs at the end of every simulator seed by default.

## T2.3.4 Continuous simulator fleet

**Goal.** Random seeds run around the clock against the main branch, and a failure turns into an issue without anyone watching.

**Work.**
- A service or scheduled job that pulls the main branch, builds it, and runs random seeds continuously.
- On failure: an issue filed automatically with seed, commit, checker, reproduction command and the trace tail; duplicates grouped.
- A dashboard: seeds per day, failures, time since last failure, coverage of fault types.
- A budget and a host for it.

**Not in this card.** Benchmarks (T1.5.5).

**Depends on.** T0.4.5, T2.3.2, T2.3.3.

**Gates.**
- [ ] The fleet has run for 14 consecutive days without manual restarts, and the seeds-per-day figure is recorded.
- [ ] At least 1,000,000 seeds per day run against the current main branch.
- [ ] A commit with a deliberate protocol defect, pushed to a test branch the fleet follows, produces an automatically filed issue within 1 hour, containing a working reproduction command (injected bug).
- [ ] The same failure found twice results in one issue with a count, not two.
- [ ] New main-branch commits are picked up within 15 minutes.
- [ ] The dashboard shows, per fault type, how many seeds exercised it in the last 24 hours, and none is zero.

## T2.3.5 Real-cluster deployment, crash and power-fail tests

**Goal.** Confirmation on real machines that the storage and replication stack survives what the simulator claims it survives.

**Work.**
- Deployment scripts for a 3-node cluster on production-like servers.
- Scripted faults under load: process kill, node power-off, network partition with firewall rules, disk faults with a device-mapper fault target, clock steps.
- A client-side checker using the same history format as the simulator.
- A written runbook of the tests.

**Not in this card.** The external audit (T7.3.6).

**Depends on.** T2.3.1, T2.3.2, T2.2.14. Three production-like servers.

**Gates.**
- [ ] Runs on real hardware: three servers with enterprise NVMe with power-loss protection; the hardware is listed in the report.
- [ ] 1,000 process kills, 100 real power cuts, 100 partitions and 100 injected disk-fault episodes under write load, with zero invariant violations reported by the checker.
- [ ] After every power cut, every write acknowledged to a client is present.
- [ ] After the whole run, the three data files hash identically at a quiesced checkpoint.
- [ ] A 72-hour soak under mixed load with random faults completes with zero violations and no unbounded growth of memory or disk beyond the data written.
- [ ] One test uses a drive without power-loss protection and records what happens, supporting the "unsupported" statement in document 02.
- [ ] The scripts are in the repository and one documented command starts the suite.

## T2.3.6 Replicated benchmark numbers

**Goal.** The first performance figures for a replicated group, tracked nightly from here on.

**Work.**
- Benchmark workloads against a 3-replica group: insert throughput by batch size, latency percentiles, point reads on the primary.
- The same with replication factor 5.
- Comparison with the single-shard, unreplicated numbers of T1.5.5.
- Results added to the nightly tracking.

**Not in this card.** The ledger benchmark against TigerBeetle (T3.5.3).

**Depends on.** T2.3.1, T1.5.5. Three production-like servers.

**Gates.**
- [ ] A report on real hardware gives insert throughput and latency (median, p99, p99.9) at replication factor 3 for batch sizes 1, 100 and full-message, and the ratio to the unreplicated numbers.
- [ ] The same for replication factor 5.
- [ ] p99.9 loop iteration stays under 5 ms on every replica during every run.
- [ ] Commit latency p99 for a full-message batch is under 5 ms at replication factor 3 (proposed threshold).
- [ ] The nightly job stores these numbers and fails on a regression of more than 10% (injected slowdown shows it failing).
- [ ] The report states where the time goes for one committed batch: WAL write, network, execution, reply.
