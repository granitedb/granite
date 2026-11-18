# Phase 5. Cross-shard transactions

Phase exit: independent and coordinated transactions pass the invariants in document 04 section 6; consistent multi-shard snapshot reads; gate G3 answered.

Design reference: `docs/04-cluster-and-cross-shard.md` sections 4 to 6. "Invariant N" below means invariant N of its section 6. "Cluster seed gate N" means the seed gate on a simulated multi-group cluster with all faults, including whole-group outages, with the global serializability checker of T5.4.1 active once it exists.

## T5.1.1 Read and annotate the Granola paper and thesis; list deviations

**Goal.** A written, reviewed account of what we take from Granola and where we differ, before the protocol is coded.

**Work.**
- Read Cowling and Liskov, "Granola: Low-Overhead Distributed Transaction Coordination" (2012) and Cowling's thesis.
- For each rule in document 04 section 4: the source section, or "ours" with the reason.
- Deviations to cover: finalize as a logged op, votes derived from logged state, conflict vote instead of waiting, conflict-aware bypass, the finished-transaction table, the remote timestamp guard, fail-fast `unavailable`.
- The paper's proof obligations for timestamp mode, and how each is met.

**Not in this card.** The formal model (T5.1.2).

**Depends on.** Nothing.

**Gates.**
- [ ] A document under `docs/` lists every rule of document 04 section 4 with a citation or the label "ours".
- [ ] Each of the seven deviations has: what the paper does, what we do, why, and what could go wrong.
- [ ] Every proof obligation the paper states for timestamp-based execution is listed with the mechanism that satisfies it.
- [ ] The three open points of document 04 section 8 about Granola and classification are answered or turned into explicit tasks.
- [ ] The bounds are fixed: maximum participants per transaction, pending transactions per shard, queue length; and they are added to the constants module's tables.
- [ ] Approved in review by two engineers, at least one of whom did not write document 04.

## T5.1.2 Model-check our protocol including view changes and locks

**Goal.** A machine-checked argument that cross-shard transactions are atomic and serializable, including under failover.

**Work.**
- A specification under `models/`: independent transactions (propose, vote, finalize, execute), coordinated transactions (prepare with locks, vote, finalize), timestamp-order execution, the conflict-aware bypass, vote retransmission after a view change, the conflict vote.
- Each group modelled as a log that can lose its uncommitted suffix on a view change.
- Properties: invariants 1 to 4, and absence of deadlock.

**Not in this card.** Tablet moves, the standby site.

**Depends on.** T5.1.1.

**Gates.**
- [ ] The specification is in `models/` and `just model xshard` runs it to completion.
- [ ] Invariants 1, 2, 3 and 4 hold for 3 groups, at least 3 transactions with overlapping keys, a mix of independent and coordinated, and one view change per group; state count and bounds are recorded.
- [ ] A property shows no state exists in which every pending transaction waits on another (no deadlock) under the locking rules.
- [ ] Four deliberate defects each produce a counterexample: finalizing at a timestamp below a vote; executing before all votes are in; a coordinated prepare that waits for a lock instead of voting conflict; bypass of a transaction whose keys intersect a pending one (four injected bugs).
- [ ] The bypass is modelled, and the checker confirms that results equal strict timestamp-order execution.
- [ ] A table maps every action to the task and function that will implement it.
- [ ] The checker runs in the nightly workflow.

## T5.2.1 Pending-transaction table and timestamp-order queue

**Goal.** The replicated state a shard needs to take part in cross-shard transactions: what is pending, and what must wait behind it.

**Work.**
- A pending-transaction table in an internal LSM tree: transaction id, parameters, declared key set, proposed timestamp, votes received.
- The queue of work waiting for an older proposal to finalize, ordered by timestamp.
- Bounds on both; `busy` when full.
- Everything changed only by executing logged ops.

**Not in this card.** The protocol messages (T5.2.2).

**Depends on.** T4.3.3, T1.2.4.

**Gates.**
- [ ] The table and queue are replicated state: all replicas hold identical contents at every op (convergence checker, seed gate 100,000 with all faults).
- [ ] They survive a view change, a restart of all replicas and a state sync with contents intact (three tests).
- [ ] At the pending limit (proposed 4096) the next proposal gets `busy`, nothing is dropped, and memory stays constant (test).
- [ ] The queue releases work in strictly increasing timestamp order (assertion and model test).
- [ ] A transaction id present in the table is never inserted twice (assertion).
- [ ] Table lookups on the hot path cost one point read and can be served by the row cache (counter).

## T5.2.2 Independent transactions: propose, vote, finalize, execute

**Goal.** The product's main path: a transaction across several groups that cannot fail, with no locks and no aborts.

**Work.**
- `propose` as a logged op: pick a proposed timestamp above every timestamp issued and strictly above the client's (document 04 section 4), record the transaction as pending.
- A vote sent to the other participants when the propose op commits.
- `finalize` as a logged op once all votes are in: final timestamp is the maximum of the votes.
- Execution when the transaction reaches the head of the timestamp order.
- The reply to the client with this participant's part of the result.
- Strict-order execution first; bypass comes in T5.4.3.

**Not in this card.** Retransmission after failures (T5.2.3), batching of votes (T5.2.9).

**Depends on.** T5.2.1, T5.1.2.

**Gates.**
- [ ] Invariants 1, 2 and 3 hold over cluster seed gate 100,000 with network faults and single-replica crashes.
- [ ] A participant's vote is sent when its propose op commits, never later behind the execution queue (assertion; test with a long queue shows the vote time unchanged).
- [ ] The final timestamp is at least every participant's proposed timestamp and identical on all participants (assertion and checker).
- [ ] All replicas of a participant execute the transaction at the same log position (convergence checker).
- [ ] An independent transaction never returns an abort; the only non-success outcomes are `busy`, `unavailable` and `too_old`, all before it is proposed anywhere (test).
- [ ] Each participant writes exactly two log records per transaction (counter).
- [ ] The implementation's steps match the specification of T5.1.2; the mapping table is updated.

## T5.2.3 Vote retransmission and proposal forwarding after failures

**Goal.** A cross-shard transaction completes even when the client dies after its first message, or a primary fails mid-protocol.

**Work.**
- Votes are derived from logged state; a new primary re-sends the votes for every pending transaction.
- A participant missing a vote requests it.
- A participant that learns of a transaction another participant does not have forwards the proposal to it.
- Duplicate votes and duplicate proposals are idempotent.

**Not in this card.** A participant group that is entirely unreachable (T5.4.4).

**Depends on.** T5.2.2.

**Gates.**
- [ ] Cluster seed gate 100,000 with crashes, partitions, view changes in every participant and clients that crash after sending to exactly one participant: invariants 1 to 3 hold.
- [ ] After faults stop and every group has a quorum, every pending transaction finalizes and executes within a bounded virtual time (liveness check).
- [ ] A transaction whose client sent to only one of three participants and then crashed completes on all three (scenario test).
- [ ] A vote re-sent after a view change carries the same proposed timestamp as the original (assertion; injected bug that picks a new timestamp is caught).
- [ ] A duplicate vote or proposal changes no state (test).
- [ ] Removing retransmission on view change, as a deliberate defect, is caught by the liveness checker within 10,000 seeds (injected bug).

## T5.2.4 Lock table and coordinated transactions

**Goal.** Cross-shard transactions that can fail on a condition, built so that no distributed deadlock can form.

**Work.**
- `prepare` as a logged op: evaluate local requires, try to lock the declared keys, vote commit, abort with a code, or conflict.
- The rule that a coordinated prepare finding a locked key, or a key of a transaction that has not executed yet (pending or queued), votes conflict at once and takes no locks.
- `finalize`: commit at the maximum proposed timestamp if all voted commit, else abort; release locks; execute the writes if committed.
- A lock table in an internal LSM tree, replicated, surviving view changes.
- Single-shard and independent transactions touching a locked key wait in the bounded shard queue, and take their timestamp only when the lock is released (document 04 section 4).

**Not in this card.** Client retry on conflict (T5.2.7).

**Depends on.** T5.2.3.

**Gates.**
- [ ] Invariants 1 to 3 hold over cluster seed gate 100,000 with all faults for mixes of coordinated, independent and single-shard transactions.
- [ ] No deadlock: the liveness checker passes over the same seeds, and no transaction waits longer than a stated bound after faults stop.
- [ ] A coordinated prepare never waits for a lock (assertion); making it wait, as a deliberate defect, produces a deadlock found by the liveness checker (injected bug).
- [ ] A coordinated transaction with one participant voting abort commits on none; no participant's state changes (checker).
- [ ] Every lock is released by the finalize op, on commit and on abort; after all transactions finish the lock table is empty (checker at the end of every seed).
- [ ] Locks survive a view change: a key locked before the change is still locked after it (test).
- [ ] The lock table is bounded and returns `busy` at the limit.

## T5.2.5 Hybrid clock floor and client timestamp propagation

**Goal.** Cross-shard transactions are ordered consistently with what clients have already seen, which is what makes the serializable order sensible to an observer.

**Work.**
- Each shard's timestamp floor is raised to the highest timestamp seen on any vote or finalize.
- Clients track the highest timestamp they have seen and send it on every request.
- The floor is persisted through the log, so it survives failover.
- The causal-order checker: if transaction B was started by a client after it saw A's reply, B's timestamp is higher than A's.

**Not in this card.** The guard against far-future timestamps (T5.4.6), commit-wait (T5.3.3).

**Depends on.** T5.2.2, T2.2.8.

**Gates.**
- [ ] The causal-order checker passes over cluster seed gate 100,000 with all faults, including clock faults.
- [ ] Dropping the client timestamp, as a deliberate defect, is caught by the checker within 10,000 seeds (injected bug).
- [ ] A shard never assigns a timestamp at or below one it has seen in a vote, a finalize or a client request (assertion).
- [ ] The floor survives a view change and a full-group restart (tests).
- [ ] Timestamps within each group remain strictly increasing in op order (replication invariant 5 still holds).
- [ ] The guarantee is stated precisely in `DESIGN.md` section 4: serializable across shards, causal for a client's own sequence of calls, strict only with commit-wait.

## T5.2.6 Automatic classification of programs

**Goal.** The compiler tells the author whether a program is single-shard, independent or coordinated, and stops a silent change from one to another.

**Work.**
- Classification from the IR: single-shard (one partition key expression, or dependent keys); independent (static keys, only statements that cannot fail); coordinated (static keys with any `require`, any fallible write to a versioned table, or a global unique index).
- The `on <participant>:` blocks of the program syntax.
- An `independent` annotation that makes compilation fail if the program is not independent.
- The class and the shard set in the compiler's report.
- The server re-derives the class at registration and never trusts the client's.

**Not in this card.** `audit` (T5.2.11).

**Depends on.** T3.3.4, T3.3.2.

**Gates.**
- [ ] The classifier's output matches the hand classification for a corpus of at least 100 programs covering every rule, committed with the expected class for each.
- [ ] Adding a `require` to a program annotated `independent` fails compilation with a message naming the statement (test).
- [ ] A program with a dependent key that spans partitions is rejected, with a message explaining that it must be single-shard.
- [ ] Soundness: no program classified independent can fail on any participant. Property test executing 1,000,000 random independent programs on random state with zero failures.
- [ ] The server's class for a registered program equals the compiler's for the whole corpus, and a registration claiming a different class is rejected.
- [ ] `settle_trade` from document 03 is classified independent and `place_order` single-shard; both in the golden report.

## T5.2.7 Client: participant computation, fan-out, retry policy

**Goal.** An application calls a cross-shard program like any other; the client library does the rest.

**Work.**
- Computing the participant groups from the parameters using the program's shard set, per group and not per tablet.
- A time-ordered transaction id chosen by the client.
- Sending the transaction to every participant primary with the client's highest seen timestamp.
- Collecting and merging the per-participant replies.
- Retry policy: the same id on timeout; backoff with jitter on `conflict` and `busy`; fail fast on `unavailable`.

**Not in this card.** The same-group shortcut (T5.2.10).

**Depends on.** T5.2.4, T5.2.6, T4.3.2.

**Gates.**
- [ ] An end-to-end test runs a cross-shard transfer between accounts on two different groups of a real cluster, and the balances on both sides are correct.
- [ ] Over cluster seed gate 100,000 with all faults, every client call returns an outcome consistent with the global serializability checker; no call reports success for a transaction that did not commit, or failure for one that did.
- [ ] A retried call with the same id never executes twice (checker).
- [ ] The participant set computed by the client equals the one the servers compute; a mismatch is rejected by the servers (test).
- [ ] Under a workload where 20% of coordinated transactions conflict, every transaction eventually commits or returns a definite error, and retries per transaction stay under a stated bound (simulator measurement).
- [ ] A transaction naming more than the maximum number of participants is rejected in the client before anything is sent.

## T5.2.8 Interaction with tablet moves (done in phase 4b)

**Goal.** A tablet can move while cross-shard transactions use it, without losing or splitting a transaction.

**Work.**
- Before a tablet freezes, pending cross-shard transactions that touch it finish on the source.
- New cross-shard transactions naming the tablet are rejected with `moving` from the moment the drain starts.
- Participants are identified by group; after the epoch bump, clients recompute participants.
- The finished-transaction entries for the tablet's transactions remain answerable.
- An extension of the tablet-move model with pending transactions.

**Not in this card.** The move itself (T4.4.4). This task is scheduled with phase 4b.

**Depends on.** T4.4.4, T5.2.4, T5.4.5.

**Gates.**
- [ ] The model of T4.4.1 is extended with pending cross-shard transactions, and invariants 1, 6 and 7 hold in the checker.
- [ ] Cluster seed gate 1,000,000 with moves during a cross-shard workload and all faults: invariants 1 to 4, 6 and 7 hold.
- [ ] `freeze` is never logged while a pending transaction touches the tablet (assertion).
- [ ] The drain completes within a bounded time under load; if a pending transaction is blocked by an unreachable group, the move aborts cleanly and does not hang (test).
- [ ] A duplicate of a transaction that finished on the source before the move is answered from the finished table and not re-executed on the target (test).
- [ ] Clients see only retryable errors, and the client library hides them (end-to-end test with zero failed calls).

## T5.2.9 Vote and finalize batching between group pairs

**Goal.** The per-transaction cost of the main path is a few dozen bytes and a share of a log record, not a message and a prepare each.

**Work.**
- All votes from one group to another, produced in one loop iteration, travel in one message.
- Finalize records for many transactions share one prepare; propose records already do through request coalescing.
- Flush every loop iteration, with no added timer.
- Accounting: vote bytes and log bytes per transaction.

**Not in this card.** Changing the protocol's logic; this is transport and log packing only.

**Depends on.** T5.2.2, T2.2.12.

**Gates.**
- [ ] A report on real hardware gives, per cross-shard transaction at saturation: vote bytes on the wire, log bytes per participant, messages per transaction and prepares per transaction.
- [ ] At saturation with two groups, the number of vote messages is under one tenth of the number of transactions (counter).
- [ ] Batching adds no latency when idle: a single transaction alone completes in the same time as without batching, within 5% (test).
- [ ] All protocol seed gates of T5.2.2 to T5.2.4 pass unchanged with batching enabled.
- [ ] Fuzz gate 4 on the batched vote decoder.
- [ ] A vote batch larger than one message is split, never truncated (property test).

## T5.2.10 Same-group shortcut

**Goal.** A transaction whose keys all live in one group pays the single-shard price, even when it was written as a cross-shard program.

**Work.**
- Participants computed per group: tablets of the same group count as one participant.
- A transaction with a single participant runs as an ordinary single-shard op, with one log record and no votes.
- The same result codes and the same reply shape as the cross-shard path.

**Not in this card.** Co-locating users to make this common; that is an application choice.

**Depends on.** T5.2.7.

**Gates.**
- [ ] A transaction between two users in the same group writes one log record and sends zero votes (counters).
- [ ] Its result is identical to the result the cross-shard path gives for the same transaction (model test running both paths on copies of the same state).
- [ ] The global serializability checker passes for workloads mixing shortcut and full-path transactions (cluster seed gate 100,000).
- [ ] After a tablet move puts the two users in different groups, the same program takes the full path with no application change (test).
- [ ] The shortcut's latency equals an ordinary single-shard program's, within 5% (report).

## T5.2.11 `audit` statement and system violations table

**Goal.** Independent programs can check for application bugs without gaining the ability to abort.

**Work.**
- The `audit cond else Code` statement in the IR and the text syntax.
- A system violations table: transaction id, program, code, timestamp, participant.
- On a false condition the transaction still commits and a violation row is written in the same op.
- `audit` allowed in independent programs; classification unchanged by it.
- A way to read and subscribe to the violations table.

**Not in this card.** Alerting on violations (T7.3.1).

**Depends on.** T5.2.6, T3.3.3.

**Gates.**
- [ ] Model test, replicated: a false `audit` never changes the transaction's outcome or its writes, and always produces exactly one violation row per false audit per participant.
- [ ] A program with `audit` and no `require` is classified independent (test in the corpus).
- [ ] The violation row is written in the same op as the transaction on every replica (convergence checker).
- [ ] A settlement without a matching hold, in the exchange schema, commits and appears in the violations table (scenario test).
- [ ] The violations table is readable by `scan` and appears in the change feed.
- [ ] An `audit` condition cannot have side effects or reads beyond the program's declared key set (validator test).

## T5.2.12 Global unique indexes over coordinated transactions

**Goal.** Uniqueness of a column set across the whole cluster, for low-rate cases such as external references.

**Work.**
- `unique global (columns)` in the schema.
- A hidden index table partitioned by the indexed columns.
- Every insert into the base table becomes a coordinated transaction between the base row's group and the index entry's group.
- Maintenance on update and delete.
- Documentation of the cost and of when to use the local form instead.

**Not in this card.** Global non-unique secondary indexes.

**Depends on.** T5.2.4, T3.4.5.

**Gates.**
- [ ] Over cluster seed gate 100,000 with concurrent inserts of colliding values from many clients and all faults: at every snapshot, no two live rows in the cluster share the indexed values (checker).
- [ ] Of N concurrent inserts with the same value, exactly one succeeds and the rest get `unique_violation`.
- [ ] A base row never exists without its index entry, nor an entry without its row, at any consistent snapshot (checker).
- [ ] After a delete, the value can be inserted again (test).
- [ ] An insert whose base row and index entry fall in the same group takes the same-group shortcut (counter).
- [ ] A report gives insert throughput and latency with a global unique index against the same table without one; the documentation quotes the ratio.

## T5.3.1 Shard safe timestamp and idle heartbeats

**Goal.** Each shard knows the timestamp below which nothing can be added any more, and that timestamp keeps moving even with no traffic.

**Work.**
- `shard_safe_ts`: the minimum proposed timestamp of pending transactions minus one, or the latest issued timestamp if none are pending.
- Heartbeat ops on idle groups to advance it.
- The value available on every replica as of its executed position.
- Exposed in metrics, with its lag behind real time.

**Not in this card.** Reads that use it (T5.3.2).

**Depends on.** T5.2.2.

**Gates.**
- [ ] After `shard_safe_ts` has passed T on a replica, no op with timestamp at or below T is ever executed on that group (assertion and checker over cluster seed gate 100,000).
- [ ] On a group with no traffic and nothing pending, the safe timestamp lags the synchronised clock by under 20 ms (simulator and real hardware).
- [ ] The safe timestamp never decreases on any replica, across view changes (assertion).
- [ ] With a transaction pending against an unreachable group, the safe timestamp stops at that proposal and does not pass it (test).
- [ ] Heartbeat ops cost under 1% of an idle group's log capacity (measured).

## T5.3.2 Snapshot reads that wait for the shard safe timestamp, with timeout

**Goal.** A read across many shards sees one consistent moment of the whole cluster.

**Work.**
- The coordinator picks a read timestamp T.
- Each shard serving a fragment waits until its own safe timestamp is at or beyond T, then reads at T.
- A timeout: a shard blocked behind a pending transaction against a dead group answers `unavailable`.
- The option to pick T slightly in the past to avoid waiting.

**Not in this card.** Query fragments and aggregation (phase 6); here the reads are `get` and `scan` across shards.

**Depends on.** T5.3.1, T3.1.4.

**Gates.**
- [ ] Invariant 5 over cluster seed gate 100,000 with all faults: every multi-shard snapshot read, repeated at the end of the run, returns the same result.
- [ ] A multi-shard read never observes one leg of a cross-shard transaction without the other. Checker: every read's result equals the reference model's state at T.
- [ ] Reading before the safe timestamp passes T, as a deliberate defect, is caught within 10,000 seeds (injected bug).
- [ ] With one group down and a transaction pending against it, reads at later timestamps on the affected shard return `unavailable` after the timeout; reads at earlier timestamps and on other shards succeed (scenario test).
- [ ] The added wait for a read at "now" is under one replication round plus 5 ms at the 99th percentile in a healthy cluster (real hardware).
- [ ] The metadata group is not contacted for any read (counter).

## T5.3.3 Optional commit-wait

**Goal.** Applications that need strict serializability across shards can ask for it per transaction and pay with latency.

**Work.**
- A per-transaction flag.
- With the flag, the reply is withheld until the clock-uncertainty bound has passed the transaction's timestamp, so no later transaction anywhere can receive a smaller timestamp.
- The uncertainty bound taken from the clock synchronisation interval.
- The strictness checker: real-time order across all shards implies timestamp order.

**Not in this card.** Making it the default.

**Depends on.** T5.2.5, T2.2.8.

**Gates.**
- [ ] With the flag on every transaction, the strictness checker passes over cluster seed gate 100,000 with clock faults within the synchronisation bound.
- [ ] With the flag off, the same checker finds a violation in the same seeds, confirming that the checker and the flag both work.
- [ ] With clocks outside the assumed bound, the affected primary steps down instead of serving a commit-wait reply it cannot justify (scenario test).
- [ ] The added latency equals the clock uncertainty bound within 1 ms, measured on real hardware; the report gives the bound achieved with the deployed clock discipline.
- [ ] The guarantee and its cost are documented with the exact definition of "strict".

## T5.3.4 Multi-group change feed merged by timestamp in the client

**Goal.** A consumer sees one globally ordered stream in which the legs of a cross-shard transaction arrive together.

**Work.**
- The client subscribes to every relevant group.
- Events are released in timestamp order, and only once every group's feed has passed that timestamp.
- Idle groups advance through safe-timestamp heartbeats in the feed.
- A cursor made of one position per group.
- Records carry the cross-shard transaction id.

**Not in this card.** Server-stored cursors (T7.2.3).

**Depends on.** T3.6.3, T5.3.1.

**Gates.**
- [ ] Over cluster seed gate 100,000 with all faults and consumer restarts: the merged stream is in non-decreasing timestamp order, and all legs of every cross-shard transaction are delivered before any event with a higher timestamp (checker).
- [ ] Applying the merged stream to an empty model reproduces the cluster's state, and at every transaction boundary the model is a state the cluster actually had (checker).
- [ ] An idle group does not stall the stream: with one group idle, delivery lag stays under 50 ms (test).
- [ ] An unreachable group stalls the merged stream, and the client reports which group; it never releases events out of order to keep going (test).
- [ ] Resuming from the multi-group cursor after a consumer crash loses nothing and repeats nothing beyond the cursor (test).
- [ ] The risk-service example follows balances across the whole cluster and matches them at the end of every seed.

## T5.4.1 Global serializability checker

**Goal.** The checker that decides whether the whole cluster behaved like one serial database.

**Work.**
- Collection of every committed transaction with its final timestamp and results, from all groups.
- Replay of all of them serially in timestamp order against the reference model.
- Comparison of every result and of the final state.
- Checks of invariants 1 and 2 from the participants' logs.
- A minimised report on failure.

**Not in this card.** Strictness in real time (T5.3.3).

**Depends on.** T4.5.1, T5.2.2.

**Gates.**
- [ ] Each of these deliberate defects is detected within 10,000 seeds: two participants executing a transaction at different timestamps; a transaction executed on one participant only; two conflicting transactions executed in different orders on two shards; a bypass of a conflicting transaction (four injected bugs).
- [ ] No false alarm over cluster seed gate 100,000 on the correct implementation.
- [ ] The checker handles a run of 1,000,000 transactions across 16 groups in under 5 minutes.
- [ ] On failure the output names the first transaction whose result differs, its participants, and the seed.
- [ ] The checker has unit tests with hand-written serializable and non-serializable histories.
- [ ] It is active by default in every cluster seed and in the fleet.

## T5.4.2 Exchange settlement benchmark (place order, settle trade) at 10%, 50%, 100% cross-shard; G3 report

**Goal.** Answer gate G3: on the workload the product exists for, does cluster throughput reach the agreed fraction of single-shard throughput?

**Work.**
- The exchange workload: `place_order` then `settle_trade` between random users, on the production shape of 6 nodes at replication factor 3.
- Runs at 10%, 50% and 100% cross-shard settlement.
- The single-shard baseline on the same hardware.
- Throughput, latency percentiles, per-group utilisation, and the cost breakdown per transaction.

**Not in this card.** The performance work if the gate fails; that becomes an inserted phase.

**Depends on.** T5.2.9, T5.2.10, T3.4.6, T4.5.2. Six production-like servers.

**Gates.**
- [ ] All numbers are measured on real hardware: 6 servers, replication factor 3, the group count of the reference sizing. The report lists the hardware.
- [ ] The agreed fraction is written into this card before the run. Proposed threshold: at 100% cross-shard, cluster throughput is at least 0.40 times the single-shard baseline, with p99 latency under 3 times the single-shard p99.
- [ ] The report gives, for each of the three mixes and the baseline: settlements per second, latency (median, p99, p99.9), and log records and vote bytes per transaction.
- [ ] Each figure comes from 3 runs of at least 10 minutes after warm-up, with the spread.
- [ ] The report states pass or fail against the threshold. If fail: a breakdown of where the time goes and a proposed plan for the inserted performance phase.
- [ ] The global serializability checker runs on a sample of each benchmark run and passes.
- [ ] The decision on scheduling the conflict-aware bypass relative to the benchmark is recorded; the bypass itself stays required.
- [ ] The benchmark reruns with one `just` recipe and joins the tracked set.

## T5.4.3 Conflict-aware bypass (required)

**Goal.** A transaction that does not touch the keys of any earlier transaction that has not executed yet (pending, or finalized and still queued) runs immediately. This is required for failure isolation: without it one dead group stalls the cluster.

**Work.**
- A check of each incoming transaction's declared key set against the key sets of all earlier transactions that have not executed yet: pending, and finalized but still queued (document 04 section 4).
- No intersection: execute at once, at a timestamp later than the pending proposals.
- Intersection: wait in the timestamp-order queue.
- An index over those key sets so the check is cheap.
- Snapshot reads still respect the shard safe timestamp.

**Not in this card.** Fail-fast rejection (T5.4.4).

**Depends on.** T5.2.4, T5.1.2.

**Gates.**
- [ ] The global serializability checker and the causal-order checker pass over cluster seed gate 1,000,000 with the bypass enabled and all faults.
- [ ] Invariant 9: with one group permanently down, a transaction on another shard is delayed only if it names the dead group or its keys intersect a transaction pending against it. A checker classifies every delayed transaction and finds no other cause (cluster seed gate 100,000 with a whole-group outage in every seed).
- [ ] Bypassing a transaction whose keys intersect a pending one, as a deliberate defect, is caught by the serializability checker within 10,000 seeds (injected bug).
- [ ] Results with bypass equal results with strict order: the same seeds run both ways give identical final states and identical per-transaction results.
- [ ] The intersection check costs under 200 ns per key with 4,096 pending transactions (report on real hardware).
- [ ] With one of 64 groups down, cluster throughput for transactions not naming it stays above 90% of normal (real hardware).

## T5.4.4 Group reachability tracking and fail-fast `unavailable`

**Goal.** Work aimed at a dead group is refused immediately instead of piling up as pending transactions that block others.

**Work.**
- Shards and clients track which groups are reachable, from heartbeats, vote traffic and the metadata group's liveness view.
- A new transaction naming an unreachable group is rejected with `unavailable` before it is proposed anywhere.
- Hysteresis so a brief view change does not flip the state.
- Transactions already pending stay pending; no unilateral abort after voting.

**Not in this card.** Recovering the dead group; that is ordinary replication.

**Depends on.** T5.4.3.

**Gates.**
- [ ] With one group down for 10 minutes of virtual time under load, the number of transactions pending against it stays below a stated bound and does not grow with time (simulator test).
- [ ] No transaction is rejected `unavailable` after it has been proposed on any participant: the decision is taken before the first propose (assertion and checker).
- [ ] A normal view change (under 200 ms) does not cause `unavailable` for that group in more than 1% of requests (simulator and real hardware).
- [ ] No participant aborts on its own after voting commit (assertion; the model of T5.1.2 covers the rule).
- [ ] When the group returns, pending transactions complete and `unavailable` stops within 1 second (test).
- [ ] The reachability view of two shards may differ without breaking any invariant (cluster seed gate 100,000 with asymmetric partitions).

## T5.4.5 Finished-transaction table with retention window; `too_old`

**Goal.** A retried cross-shard transaction gets its original answer and is never executed twice.

**Work.**
- An internal tree per participant: finished transaction ids and their outcomes.
- A retention window of one hour of commit time, enforced by compaction on commit timestamps.
- A duplicate inside the window is answered from the table.
- A transaction id older than the window is rejected with `too_old`.
- The id's embedded time used for the age test, with the remote timestamp guard bounding skew.

**Not in this card.** Session-based deduplication for single-shard requests (T2.2.9).

**Depends on.** T5.2.2, T3.1.2.

**Gates.**
- [ ] Over cluster seed gate 100,000 with duplicated, delayed and reordered client messages and all faults: every transaction id is executed at most once per participant (checker).
- [ ] A duplicate inside the window returns a reply byte-identical to the original, including after a view change and a full-group restart.
- [ ] An id older than the window gets `too_old` on every participant and executes on none (test at the boundary: one second inside, one second outside).
- [ ] A duplicate arriving while the original is still pending attaches to it; it is not proposed again (test).
- [ ] Removing entries is deterministic across replicas (convergence checker).
- [ ] The table's size at 100,000 cross-shard transactions per second for one hour is computed and measured, and fits a stated budget (report).

## T5.4.6 Remote timestamp guard

**Goal.** One machine with a wrong clock cannot drag the whole cluster's timestamps into the future.

**Work.**
- A timestamp from a client or another group that is more than a fixed bound (initially 100 ms) ahead of the receiver's synchronised clock is rejected as a fault.
- Rejection happens before the timestamp can raise the receiver's floor.
- A typed error to the sender and a metric.
- The interaction with a tablet's adopted timestamps, which are exempt because they come through the move protocol.

**Not in this card.** Fixing the bad clock; the node with it is reported.

**Depends on.** T5.2.5.

**Gates.**
- [ ] In simulation, one node's clock is set 1 hour ahead: no shard on any other node ever issues a timestamp more than the guard bound ahead of true time (checker over cluster seed gate 10,000).
- [ ] The bad node's transactions are rejected with a typed error, and the rest of the cluster keeps committing (scenario test).
- [ ] Removing the guard, as a deliberate defect, lets timestamps jump and is caught by the checker (injected bug).
- [ ] A timestamp exactly at the bound is accepted and one nanosecond past it is rejected (boundary test).
- [ ] With all clocks within 1 ms, zero rejections occur in a 24-hour run on real hardware.
- [ ] The guard bound is a format constant in the constants module with its reasoning in document 04.
