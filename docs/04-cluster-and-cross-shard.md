# 04. Cluster layout and cross-shard transactions

## 0. Reference sizing (confirmed 2026-11-18)

| Input | Value |
|---|---|
| Nodes | 6 in the first year, one data center |
| Data after two years | 10 to 100 TB before replication, so 30 to 300 TB on disk at RF 3, 5 to 50 TB per node |
| User activity | Fairly uniform; no dominant hot users |
| Site loss | Asynchronous standby site (section 7) |

What follows from it, taking 32 cores per node as an example:
- About 64 groups (6 x 32 / 3). Each group's replicas sit on one of the 20 possible node triples; groups are spread evenly over the triples so that losing a node shifts load evenly onto the other five.
- Each shard's data file holds roughly 150 GB to 1.5 TB. Nodes need several NVMe drives; shards are assigned to drives evenly, and a drive failure is handled as the loss of those shards' replicas (repair from peers, or replacement).
- **Failure tolerance at RF 3:** any one node. A second simultaneous node failure takes out the groups whose triple contains both nodes, about 20% of groups, and every transaction involving those groups waits. If two-node tolerance is required, use RF 5 for the cluster.
- Target tablet size 16 GiB, so 10 to 100 tablets per group and a tablet moves in minutes.
- The metadata group runs on 3 or 5 of the 6 nodes.
- Heavy users can optionally be split into sub-accounts with the sub-account in the partition key; with uniform activity this is a pattern, not a requirement.

## 1. Topology

- A **node** is one process on one machine with C shards, one per core, each pinned.
- A **group** is RF shards on RF different nodes. A cluster of M nodes with C cores each has about `M * C / RF` groups.
- Group placement spreads each group's replicas across failure domains (rack, zone labels on nodes) and spreads primaries evenly across nodes. Because the VSR primary is `view mod RF`, primary placement is controlled by the replica ordering within a group and corrected by a deliberate view change when unbalanced.
- Each shard listens on its own port (`base_port + shard_index`). Clients and peers connect directly to the shard they need. No cross-thread handoff on the data path.
- Shards in the same process talk over bounded single-producer single-consumer rings; across nodes over TCP.

The **metadata group** is a VSR group of 3 or 5 replicas hosted on designated nodes (it can share cores with data shards on small clusters). Its state machine holds:
- nodes, their addresses, labels and liveness,
- groups and their member shards, with a configuration epoch per group,
- table groups, schemas, programs, with a schema version,
- the tablet map: for each table group, an ordered list of `(token range end, group id, tablet epoch)`,
- in-progress tablet moves, splits and merges,
- per-group safe timestamps (section 5).

It is off the data path. Shards subscribe to changes; clients fetch and cache. Every data request carries the tablet epoch the sender believed; a shard that no longer owns the tablet, or sees an older epoch, replies `stale_map` with the newer entry.

## 2. Sharding

- `token = hash64(encoded partition key)` with a fixed, versioned, keyed hash (candidate: XXH3-64 with a cluster-constant seed). The function is part of the on-disk format.
- A table group's token space `[0, 2^64)` is cut into tablets. A new table group starts with a configurable number of tablets (default: one per group, evenly spaced).
- Range-partitioned table groups use the encoded partition key itself instead of a token.

Why an explicit map and not a consistent-hash ring: with strong consistency every member must agree exactly on ownership at every moment, which requires a replicated map anyway. Given the map, ranges give minimal movement (only the moved tablet's data), precise hot-spot splitting, and no dependence on token randomness for balance.

### Split and merge

- Split: a tablet over a size or load threshold is cut at a token chosen from its key distribution. Both halves stay in the same group, so a split is a metadata change plus a marker in the group's log. No data moves.
- Merge: adjacent tablets in the same group under a threshold are merged, again metadata only.

### Move

A move transfers a tablet from source group S to target group T. At 10 to 100 TB, pushing every row through T's log again is too slow and doubles write load, so the bulk of a tablet moves as immutable part files and only the recent tail goes through the log. (Revised 2026-11-18; this replaces the re-logging design.)

Requirements this places on the part format (document 01):
- **Relocatable parts.** Blocks inside a part refer to each other by index within the part, not by grid address. The part header holds the extent map from index to grid address. A block identifies itself by (part id, index, checksum), so misdirection is still detected, and a part can be placed at different addresses in another group's grid by rewriting only its header.
- **Clipped parts.** A part may cover more than the moving tablet. The target's manifest records the part with a visible key range; rows outside it are invisible and dropped at the next compaction.

Steps:
1. Metadata group records the move as `preparing`.
2. S pins a manifest snapshot at timestamp Ts and lists the parts overlapping the tablet.
3. T logs `reserve_ingest(part list with sizes and checksums)`. Executing it allocates grid addresses for every block deterministically, so all T replicas reserve the same addresses, and records the blocks as expected-but-absent.
4. Each T replica fetches the blocks from any S replica by checksum and writes them at the reserved addresses. This reuses the grid repair path and runs at low priority within the 1 ms slice rule.
5. When a replication quorum of T replicas reports all blocks present, T logs `install_parts`, adding the clipped parts to its trees at their original levels. The tablet is still owned by S.
6. S streams rows with `_ts > Ts` for the tablet to T through T's log until the lag is small.
7. Cut-over: S logs `freeze tablet` (new writes get `moving`, a retryable code) after pending cross-shard transactions on it have finished, sends the final delta, T logs `adopt tablet` and raises its clock floor above the tablet's highest timestamp, the metadata group bumps the tablet epoch, S logs `release tablet` and its range is dropped by compaction.

The freeze window is the only unavailability and should be tens of milliseconds. This procedure gets a model-checked specification before code (T4.4.1).

The **balancer** runs in the metadata group's state machine (deterministic, driven by reported per-tablet size and load) and schedules at most a fixed number of concurrent moves.

## 3. Node and group membership

- Add node: register in metadata, create new groups on its cores or add its shards as standbys to existing groups, then balance.
- Replace a failed replica: reconfigure the group (document 02 section 9) to swap in a standby or a shard on another node.
- Remove node: move its replicas elsewhere by reconfiguration, then deregister.
- Group count changes only by creating or retiring whole groups; tablets are moved to fill or drain them.

## 4. Cross-shard transactions

Derived from Granola (Cowling and Liskov, 2012). The description below is our adaptation; details that differ from the paper are marked. Checked against the paper and Cowling's thesis on 2026-11-18: [04a-granola-sources.md](04a-granola-sources.md) gives the source of every rule in this section, or marks it as ours, and describes each deviation and the timestamp-mode proof obligations.

### Cross-shard is the main path (confirmed 2026-11-17)

The first application is an exchange whose transactions are between two arbitrary users. With G groups, only about 1 in G trades stays in one group. So the independent transaction path is the hot path of the product, not an exception, and is designed and scheduled accordingly:

- The reference schema (document 03 section 10) makes settlement an **independent** transaction by reserving funds first. Coordinated transactions are reserved for genuinely conditional two-party work and global unique indexes.
- **Everything is batched.** Proposals from many clients share a prepare (request coalescing). Votes between a pair of groups travel in one message carrying many votes, flushed every loop iteration. Finalize records for many transactions share a prepare. The per-transaction cost is two small log records per participant and a few dozen bytes of vote traffic.
- **No convoy.** A participant's vote is produced when its propose op commits, never behind the execution queue. So the wait for any transaction is one replication round on the slowest participant plus one network hop, regardless of how many other transactions are pending.
- **Same-group shortcut.** Participants are computed per group, not per tablet. If all keys fall in one group the transaction is an ordinary single-shard transaction.
- **Expected cost, to be measured at gate G3:** a group spends two log records per cross-shard transaction instead of one, and each transaction occupies two groups, so cluster throughput for an all-cross-shard workload should be somewhat under half of the single-shard figure for the same hardware, with latency higher by roughly one replication round. This is an estimate from the message flow, not a measurement.
- **Schedule.** Cross-shard transactions are built immediately after a static multi-shard cluster exists, before tablet split, merge and move (see roadmap).

### Preconditions

- The transaction is a registered program with static keys only (document 03 section 5), so the set of participant tablets, and the key set on each, is computable from the parameters.
- The client library (or any shard acting as proxy) computes the participants and sends the same transaction, with a client-chosen unique transaction id and its highest seen timestamp, to the primary of each participant group.

### Classification, decided statically per program

- **Independent:** the program contains only statements that cannot fail: idempotent inserts into immutable tables (with their overflow-free derived folds) and non-aborting `audit` checks. No participant can abort, so no participant needs another's decision. Example: `settle_trade` in document 03 section 10.
- **Coordinated:** any `require`, any write to a versioned table that can fail, or a global unique index. Example: debit A only if A has funds and credit B atomically, without a prior hold.

The compiler reports the class of every program, and a program can be annotated `independent` so that compilation fails if a later edit would silently make it coordinated.

### Independent transaction

```
client -> each participant primary:  propose(txn id, params, client_ts)
participant:  op "propose": log it, pick proposed_ts > every ts it has
              issued and > client_ts, record txn as pending
participant -> other participants:    vote(txn id, proposed_ts)
participant:  when all votes are in:  op "finalize": final_ts = max(votes)
              txn becomes executable at final_ts
participant:  executes txn when it reaches the head of the timestamp order
participant -> client:                reply(result part)
```

- The proposed timestamp is strictly above `client_ts` (corrected 2026-11-18; it said `>=`). With `>=` a transaction could tie with one the client has already seen and, on the tie-break below, be ordered before it.
- Transactions are ordered by `(timestamp, transaction id)`: two transactions finalized at the same timestamp on a shard execute in transaction-id order, as in Granola.
- Two log ops per participant per transaction (propose, finalize). Both are small and batch with other work. (Granola piggybacks the final timestamp on later log messages and does not wait for it to be stable before executing; we log finalize as an op of its own, so that every replica executes the transaction at the same log position and storage stays byte-identical. This costs one replication round of latency.)
- Votes are derived from logged state, so after a view change the new primary re-sends them. A participant that has not heard a vote re-requests it. The client is not needed after the first message.
- **Refusal vote** (added 2026-11-18). A participant that refuses a transaction it has not proposed (`busy`, `unavailable`, `too_old`, or the timestamp guard) logs the refusal in its finished-transaction table and sends a refusal vote instead of a timestamp. A participant that receives a refusal drops the transaction with a logged op, records the refusal in its finished-transaction table, and returns the refusal to the client; the drop is logged because the pending table is replicated state that decides `shard_safe_ts` and the bypass. This is safe because the refusing participant never voted, so no participant can hold a full set of votes; it is not an abort after voting. Granola does the same with its CONFLICT vote. The refusal is logged so that a later forwarded or retried proposal of the same id is refused again, never proposed.
- No aborts from conflicts, no locks.

### Timestamp-order execution

A shard may execute work at timestamp t only when no transaction can still be assigned a smaller timestamp on that shard. A pending transaction with proposed timestamp p will finalize at some ts >= p. So:

- `shard_safe_ts = min(proposed_ts of pending transactions) - 1`, or the latest issued timestamp if none are pending.
- Step 1, strict order: any transaction with timestamp above the oldest pending proposal waits in a queue until that proposal finalizes. Simple and correct, and the first thing built and model-checked.
- Step 2, conflict-aware bypass, **required before release** (revised 2026-11-18): because every transaction has a declared key set, a later transaction whose keys do not intersect the key set of any transaction ordered before it that has not executed yet executes immediately. That set is the pending transactions **and** the finalized transactions still waiting in the queue (corrected 2026-11-18; it said pending only, which lets a transaction overtake a queued one on the same key). Its effects carry a later timestamp and commute with every transaction it overtakes, so the result equals timestamp-order execution. The declared key set must name every key the transaction reads or writes, including derived-table and index entries. Snapshot reads still respect `shard_safe_ts`.

The queue is part of replicated state and bounded; when full, new requests get `busy`.

### Bounds (fixed 2026-11-18)

| Bound | Value | Reasoning |
|---|---|---|
| Participants per transaction | 8 | A settlement has 2; a coordinated insert with global unique indexes adds one per index. Vote traffic grows with the square of the count and a transaction waits for its slowest participant, so a larger set buys little and costs availability. The compiler rejects a program whose static keys can name more than 8 groups. |
| Pending transactions per shard | 4,096 | A transaction is pending from propose to execute: two replication rounds (propose, finalize) plus one hop, under 2 ms in one data center. At 100,000 cross-shard transactions per second on one shard that is about 200 pending; 4,096 covers a 40 ms stall at that rate, or a 200 ms view change of a peer at 20,000 per second, before `busy`. It is also the size the bypass check is measured at (T5.4.3). |
| Queue length (work waiting behind an older transaction) | 16,384 | Single-shard writes in the queue each hold a client session, which has one write in flight (document 02 section 7), so they cannot exceed the 16,384 sessions of a group. A cross-shard transaction takes a pending entry when it is accepted, including while it waits for a lock before proposing, so cross-shard work in the queue is counted in the pending limit. The queue therefore fills only if that rule is broken, and `busy` is the safety net. |

These values move to the constants module (T0.1.4), with a static assertion that the queue length is at least the session capacity of a group. Until that module exists, this table is their only home.

### Failure isolation

With strict order alone, one group that loses its quorum would stall the whole cluster: every shard with a transaction pending against it would queue all later work behind that transaction, and the stall would spread through further cross-shard transactions. Three measures contain it:
- **Bypass** limits the damage to keys actually involved in transactions with the dead group.
- **Fail fast.** Shards and clients track group reachability; a new transaction naming an unreachable group is rejected with `unavailable` before it is proposed anywhere, so pending work does not pile up. The client checks first; a participant whose view differs refuses with a refusal vote (independent transaction, above), so the others drop the transaction instead of waiting for a vote that will not come.
- **No unilateral abort.** A participant that has voted cannot abort on its own, because the unreachable group may already hold all votes and finalize when it returns. Transactions already pending with a dead group stay pending until it recovers. This is the unavoidable blocking case of any atomic commit; replication makes it rare (it needs two of a group's three nodes down).
- `shard_safe_ts` stops advancing on shards with such pending transactions, so snapshot reads on those shards at newer timestamps wait; reads at older timestamps and reads on other shards continue.

### Duplicate requests

A cross-shard transaction is identified by its client-chosen time-ordered id. Each participant keeps finished transaction ids and outcomes in an internal tree for a retention window (initially one hour of commit time) and answers a duplicate from it. A transaction id older than the window is rejected with `too_old`, never re-executed.

### Coordinated transaction

```
client -> participants:   prepare(txn id, params, client_ts)
participant:  op "prepare": evaluate local requires against current state,
              try to lock the declared keys,
              vote = (commit | abort(code) | conflict, proposed_ts)
participant -> others:    vote
participant:  when all votes are in: op "finalize":
              commit at max(proposed_ts) if all voted commit, else abort
              release locks; execute writes if committed
participant -> client:    reply
```

Locking rules, designed so no distributed deadlock can form:
- A coordinated prepare that finds any of its keys locked, **or in the key set of a transaction that has not executed yet** (pending, or queued), votes `conflict` immediately and takes no locks. The client retries with backoff. It never waits. (Corrected 2026-11-18: checking locks alone lets a prepare evaluate its `require` against state that an earlier independent transaction on the same key has not yet changed. Granola avoids the same hazard by preparing in-flight independent transactions when a coordinated one arrives.)
- A single-shard transaction that touches a locked key waits in the shard queue (bounded) until the lock is released. The lock holder is waiting only on votes, which other shards produce at prepare time without queueing, so the wait is finite.
- An independent transaction that touches a locked key waits the same way.
- A transaction that waited for a lock takes its timestamp (or, if independent, its proposed timestamp) only when the lock is released, above the lock holder's final timestamp (corrected 2026-11-18). A timestamp taken earlier could be below the holder's final timestamp, and the waiting transaction would then execute after the holder's writes while being ordered before them. Granola avoids this by answering CONFLICT in locking mode; we wait, so that independent transactions never fail.
- Locks live in an internal LSM tree, are replicated state, and survive view changes.

Failure handling:
- Participant replica failure: masked by VSR.
- Whole participant group unavailable: other participants hold pending state (and locks, for coordinated) until it returns. No participant may unilaterally abort after voting commit. Before voting, a participant may time out and vote abort.
- Client failure: irrelevant after the first message has reached at least one participant; that participant forwards the proposal to the others if they report not having it.

### Clock coupling between groups

- On every vote and finalize, a participant raises its hybrid-clock floor to the highest timestamp seen. So later transactions on that shard order after it.
- Guard: a timestamp from a client or another group that is more than a fixed bound (initially 100 ms) ahead of the receiver's synchronised clock is rejected as a fault. Without this, one bad clock could drag the whole cluster's timestamps arbitrarily far into the future. Where the rejection happens (clarified 2026-11-18): a client timestamp is checked before propose, and a vote that arrives before this participant has proposed is answered with a refusal vote. A vote that arrives after this participant has voted cannot be refused without breaking atomicity, so the transaction stays pending until this participant's synchronised clock is within the bound of that timestamp, and then finalizes as usual: the floor is never raised early, the transaction is never left unfinished, and the sender is reported. Since a group assigns timestamps only from a quorum-agreed clock (document 02 section 6), that case needs a whole group's clock to be wrong.
- Clients send their highest seen timestamp on every request (section 4 of DESIGN.md explains why).
- Optional per-transaction commit-wait for strict serializability.

## 5. Consistent multi-shard reads

Revised 2026-11-18: no cluster-wide safe timestamp is computed and the metadata group is not involved.

- The query coordinator picks a read timestamp T (its current hybrid clock, or slightly earlier to avoid waiting).
- Each shard serving a fragment waits until its own `shard_safe_ts >= T`, then reads at T. Nothing with timestamp <= T can be added on that shard afterwards, so the union of fragments is a consistent, serializable cut.
- A group with no traffic advances its safe timestamp with heartbeat ops, so an idle shard answers promptly.
- The wait is normally about one replication round. It is unbounded only on a shard with a transaction pending against an unreachable group; the query then fails with `unavailable` after a timeout rather than hanging.

## 6. Invariants the simulator must check

1. A cross-shard transaction commits on all participants or on none.
2. All participants execute it at the same final timestamp.
3. On every shard, the executed history is equivalent to executing all transactions in timestamp order.
4. The global history is serializable in timestamp order (checked by replaying all committed transactions serially against a model and comparing every result).
5. No snapshot read at T, served after each shard's safe timestamp passed T, ever observes a later change to its result.
6. Every key is owned by exactly one group at every tablet epoch; no write is accepted by a non-owner.
7. A tablet move loses and duplicates no row.

8. A promoted standby site holds a state equal to the primary site's state at one single timestamp: no cross-shard transaction is present on some groups and absent on others.
9. A group that is unreachable delays only transactions that name it or that conflict by key with transactions pending against it, directly or through a chain of queued transactions that each conflict by key with the next (the bypass of section 4 lets nothing overtake an unexecuted transaction on the same key).

## 7. Asynchronous standby site (confirmed 2026-11-18)

Goal: survive the loss of the data center with at most seconds of data loss and a manual failover.

- The standby site runs a cluster with the same group layout. Each of its shards is a **remote standby** of the corresponding group: it receives prepares asynchronously, never votes, and never delays commits at the primary site.
- **Consistent apply.** Groups receive their logs at different speeds, and a cross-shard transaction may have arrived for one group and not the other. So remote standbys do not execute prepares as they arrive. A coordinator at the standby site continuously computes an apply timestamp Ta, the minimum over groups of the safe timestamp of what each has durably received, and each group executes only ops with timestamps up to Ta. Received-but-unapplied prepares wait in the standby's WAL, so the lag between sites must stay well inside the WAL's capacity; if a group falls further behind it catches up by checkpoint sync, as any lagging replica does.
- **Promotion** is a manual command. Each group discards prepares beyond Ta, the site takes a new cluster epoch that fences the old site, and the standby shards become voting replicas. Data after Ta is lost: normally under a few seconds.
- **Failback:** the old site rejoins as the standby by state sync. No automatic merge of divergent histories.
- The metadata group has a remote standby too.

This is planned in the operations phase (roadmap E7.4). It reuses standbys, state sync and the safe-timestamp machinery, but the consistent-apply rule is new and is model-checked first.

## 8. To verify or decide

- Granola's vote exchange path, recovery rules and timestamp-mode proof obligations: **answered** from the paper and the thesis in [04a-granola-sources.md](04a-granola-sources.md) sections 3 to 5 (votes go primary to primary, with no coordinator).
- Classification: **answered**, fully automatic from the IR and conservative (anything not provably infallible is coordinated); the `independent` annotation only guards against a silent change. Task T5.2.6. Reasoning in 04a section 5.
- Bounds: **fixed** in section 4, "Bounds": 8 participants, 4,096 pending transactions per shard, a queue of 16,384.
- The relocatable-part and clipped-part rules against the storage determinism invariant, including a crash of a target replica between reserve and install.
