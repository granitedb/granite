# 02. Replication (VSR)

Each group is an independent Viewstamped Replication cluster of RF replicas (3 or 5, optionally 6 with flexible quorums), plus optional standbys. This document covers one group. Multi-group concerns are in document 04.

Basis: Liskov and Cowling, "Viewstamped Replication Revisited" (2012), with the changes TigerBeetle made for storage faults. Every rule here has been checked against the paper, against Alagappan et al., "Protocol-Aware Recovery for Consensus-Based Storage" (FAST 2018), and against TigerBeetle's published protocol description; the source of each rule, every deviation and every source rule we do not adopt are in [02a-replication-sources.md](02a-replication-sources.md).

## 0. Deployment assumptions (confirmed 2026-11-17)

| Assumption | Value | Consequence |
|---|---|---|
| Hardware | Bare metal, enterprise NVMe with power-loss protection | A durable write is expected in well under a millisecond; drives without power-loss protection are unsupported for production |
| Geography | One data center | Sub-millisecond network round trips; clocks disciplined by PTP or chrony to well under a millisecond |
| Clients | Hundreds to thousands of processes per cluster | Large session tables, server-side request coalescing |
| Failover | A group accepts writes again within 200 ms of primary failure | Tight timeouts, and a hard bound on event-loop stalls |

### Timing budget for 200 ms failover

| Step | Budget |
|---|---|
| Heartbeat interval (commit message when idle) | 10 ms |
| Suspicion timeout on a backup (no message from primary) | 60 ms, plus per-replica jitter up to 20 ms |
| start_view_change quorum, do_view_change, start_view | 3 network round trips plus one durable write each: under 10 ms |
| Repair of the uncommitted pipeline suffix | under 20 ms |
| Client discovers the new primary | client retry timeout 50 ms, then broadcast to all replicas; replicas also push a view hint |

What makes this safe rather than twitchy:
- **Quorum-gated suspicion.** A backup that times out sends start_view_change as a vote but stays in the current view. A replica changes status only after a view-change quorum of votes for the same view. One slow or partitioned backup cannot depose a healthy primary. (This differs from the letter of VR Revisited, section 4.2, and matches TigerBeetle's start-view-change protocol; include it in the model-checked specification.)
- **Event-loop stall bound.** No task on a shard thread may run longer than 1 ms without yielding; the loop's p99.9 iteration time must stay under 5 ms. Compaction, scans, bulk ingest and checkpoint writes are all sliced to this budget. This is measured in production and asserted in benchmarks, because a stalled primary looks exactly like a dead one.
- **Correlated failover.** A node failure triggers view changes in every group with a replica there, at once. The jitter is derived from (group, replica) so they do not synchronise.

## 1. Roles and state

- Primary of view v is replica `v mod replica_count`. No election, no randomness.
- Replica status: `normal`, `view_change`, `recovering`, `recovering_head` (WAL head is uncertain after a fault).
- Persistent state: superblock (view, log_view, checkpoint), WAL, grid.
- Volatile state: op (highest prepared), commit_min (highest executed), commit_max (highest known committed), the pipeline of in-flight prepares.

Quorums (flexible): replication quorum Qr and view-change quorum Qv with Qr + Qv > N, and Qv a majority (Qv > N / 2) so that any two view-change quorums intersect. The nack quorum, the number of replicas that must each prove they never acknowledged a prepare before it may be truncated (section 4), is Qn = N - Qr + 1: then the replicas that could have acknowledged it number at most Qr - 1, too few to have committed it.

| Replicas | Replication quorum | View-change quorum | Nack quorum |
|---|---|---|---|
| 3 | 2 | 2 | 2 |
| 5 | 3 | 3 | 3 |
| 6 | 3 | 4 | 4 |

## 2. Messages

All messages share a 256-byte header (document 05) and are checksummed.

| Message | Direction | Purpose |
|---|---|---|
| request | client -> primary | A batch of events for one operation type |
| prepare | primary -> backups | Header (op, view, commit, parent checksum, timestamp, client, request) plus body |
| prepare_ok | backup -> primary | Durable acknowledgement of one prepare |
| commit | primary -> backups | Heartbeat carrying commit_max |
| reply | primary -> client | Result of a request |
| start_view_change | any -> all | Vote to leave the current view |
| do_view_change | any -> new primary | Log suffix headers, log_view, op, commit |
| start_view | new primary -> all | Authoritative log suffix headers for the new view |
| request_start_view | replica -> primary | Ask for start_view after falling behind |
| request_headers, headers | any | Repair of the header chain |
| request_prepare | any | Repair of a missing or corrupt prepare by (op, checksum) |
| nack_prepare | any | "I certainly never acknowledged this prepare": the sender has no prepare at that op and its slot is not faulty, holds a different prepare there, or holds this one but has not made it durable. A replica whose slot is corrupt never nacks. Lets a new primary truncate safely |
| request_block, block | any | Grid repair by (address, checksum) |
| ping, pong | any | Liveness and clock samples |
| request_sync_checkpoint, sync_checkpoint | any | State sync |
| eviction | primary -> client | Session evicted |

## 3. Normal operation

1. Primary receives a request, checks the session (section 7), assigns `op = op + 1` and a timestamp (section 6), builds the prepare with `parent = checksum of prepare op-1`.
2. Primary writes the prepare to its WAL and sends it to backups concurrently. Replication is a ring by default (each replica forwards to the next) to save primary bandwidth, with direct fallback on timeout.
3. A backup accepts a prepare only if it is in the same view, status normal, and the prepare extends its hash chain. It writes to the WAL, then sends prepare_ok. Gaps trigger repair, not rejection.
4. On Qr prepare_oks (including its own durable write), the primary marks the op committed, executes it in order, replies to the client, and advances commit_max, which backups learn from the next prepare or commit heartbeat.
5. Backups execute ops in order up to commit_max once they hold the prepare.

Pipeline: up to 8 prepares in flight. Requests beyond that queue at the primary.

**Request coalescing (revised).** With thousands of clients each sending small batches, one prepare per request would cap throughput at prepares per second. So a prepare body is a sequence of request frames, each `(client, request number, operation, events)`. The primary fills the next prepare with every queued request that fits, from any client and any operation, in arrival order. The state machine executes frames in order and updates each client's session entry. Replies are split back per client. Batching in the client library still matters, but throughput no longer depends on it.

Execution is strictly sequential and deterministic. The state machine receives (op, timestamp, operation, body) and nothing else.

## 4. View change

Triggered when a backup hears nothing from the primary for a timeout (suspicion, step 1), or when a replica receives a do_view_change or start_view for a higher view, each of which implies a view-change quorum has already left the old view. A prepare or commit from a higher view makes the replica ask for that view with request_start_view. A start_view_change for a higher view on its own moves no replica (quorum-gated suspicion, section 0).

1. A backup that suspects the primary sends start_view_change for view + 1 to all replicas as a vote, and keeps its view and status: it still accepts the current view's prepares and commits. It repeats the vote while the suspicion lasts and stops when it hears from the primary again. A replica in status view_change whose view change does not complete in time votes for the next view in the same way.
2. A replica that holds Qv start_view_change votes for the same view (its own included) increments its view, sets status view_change and stops accepting prepares from the old view. It makes view and log_view durable in the superblock, then sends do_view_change to the new primary with its log_view, op, commit and the headers of its log suffix.
3. The new primary, on Qv do_view_change messages, selects the log from the replica with the highest (log_view, op), installs those headers, and repairs any prepares it lacks.
4. Uncommitted ops in the suffix may or may not have been committed by the old primary. An op is kept unless the new primary collects a nack quorum (section 1) of nack_prepare messages for it, which proves no replication quorum could have acknowledged it; then it and every later op are truncated. An op whose prepare is corrupt everywhere but might have committed blocks the view change rather than risking data loss.
5. The new primary sends start_view, re-replicates the uncommitted suffix, and resumes.

Timeouts use deterministic exponential backoff with per-replica jitter derived from the replica index, so the simulator can reproduce them.

## 5. Repair, recovery and state sync

- **WAL repair:** a replica with a missing or corrupt prepare requests it by (op, checksum) from any peer. The hash chain tells it exactly which checksum to expect.
- **Recovery after crash:** load superblock, classify WAL slots (document 01), replay from checkpoint. If the head of the WAL is uncertain, enter `recovering_head` and learn the true head from a start_view before participating. A replica never votes on the basis of a log it cannot verify. A replica that was the primary of its log_view does not resume that view after a restart: its pipeline is lost, and resuming could assign again op numbers whose prepares it already sent. It takes status `recovering` and votes for the next view.
- **Grid repair:** request blocks by (address, checksum). A replica also scrubs its grid in the background at a fixed pace and repairs what it finds.
- **State sync:** a replica that is more than one checkpoint behind (the WAL ring has wrapped) cannot catch up by replaying. It fetches the latest durable checkpoint's superblock roots from a peer, adopts them, and then repairs every grid block it lacks, lazily and by scrubbing. It becomes a normal backup once the checkpoint's manifest is complete. A checkpoint is durable once a replication quorum holds it, which a replica learns from commit_max being far enough past that checkpoint (replicas acknowledge prepares beyond a checkpoint only once they hold it); one peer alone claiming a newer checkpoint is never enough, because that peer's state could be corrupt.

## 6. Clocks and timestamps

- Clock sampling is a node-level service: one sampler per node pair, shared read-only by all shards on the node, rather than every shard measuring separately. This is safe for determinism because only the primary reads the clock, and the value it picks travels inside the prepare.
- Nodes exchange ping/pong with monotonic and wall-clock samples. Each replica computes, with Marzullo's algorithm, the interval of wall-clock offsets that agrees with a replication quorum of clocks. A primary whose own clock is outside that interval does not assign timestamps and steps down.
- The primary assigns each prepare a timestamp `max(previous prepare timestamp + previous batch size, synchronised clock, hybrid-clock floor)`. Each event in the batch gets a distinct, strictly increasing timestamp below the prepare timestamp range's end.
- The hybrid-clock floor is raised by timestamps received from clients and other groups (document 04).
- Timestamps are u64 nanoseconds. They are the version numbers of versioned tables and the `_ts` of every row, so they are unique within a group and strictly increasing with op order.

## 7. Client sessions

Revised for hundreds to thousands of client processes.

- A client has a random u128 id. Its first write to a group is preceded by `register`, which goes through the log and creates a session in that group.
- **Sessions cover writes only.** Reads are idempotent, never enter the log (section 8) and need no session, so read concurrency per client is unlimited.
- Each session has one write request in flight, numbered consecutively. The session table is an internal LSM tree (replicated state) keyed by client id, storing the last request number and the last reply. Write replies are sparse result lists and usually tiny; they are stored inline up to 4 KiB and in referenced grid blocks above that. There is no separate client-replies zone.
- A duplicate of the last request returns the stored reply. An older request number is dropped. This gives exactly-once execution across retries and view changes.
- A session entry changes only when its request executes (commits), never when it is prepared: an uncommitted request may not survive a view change, and a table updated at prepare would then lock the client out. VR Revisited section 4.1 updates the table at prepare; TigerBeetle found this to be a liveness bug.
- Capacity: 16,384 sessions per group initially. When full, the least recently used session is evicted deterministically (by last request op) and that client re-registers on its next write.
- A client that needs concurrent writes to the same group opens several sessions.

## 8. Reads

- **Linearizable read (revised):** never written to the log. The primary notes its commit number when the read arrives, then waits until a replication quorum has acknowledged any message it sent after that moment (the next prepare under load, or a heartbeat that backups acknowledge when idle). That proves it was still primary when the read arrived. It then serves the read from state at or after the noted commit. Cost: at most one heartbeat interval of added latency when idle, none under load; no WAL writes, no clock or lease assumptions.
- **Snapshot read at T:** any replica with commit timestamp >= T. Does not go through the log.

## 9. Standbys and reconfiguration

- **Standby:** receives prepares, never votes, can be promoted. Cheap to implement; planned with the first replicated release.
- **Replica replacement:** a logged `reconfigure` op names the new member set and takes effect at a defined future op (the next checkpoint boundary). The old configuration commits the op; the new configuration takes over at the boundary; a new member must have reached that checkpoint by state sync before it counts toward quorums. This is the VR Revisited epoch mechanism adapted to checkpoints. It is the least-proven part of this document and gets a model-checked specification before any code (roadmap epic E7.1).

## 10. Upgrades

- Every message and the superblock carry a protocol/format version.
- The cluster runs the minimum version supported by all replicas of a group; the switch to a new version is a logged op, so all replicas change behaviour at the same op.
- Binaries carry at least the previous release's state machine logic to replay old ops.

## 11. Invariants the simulator must check

1. At most one op is ever committed at a given op number in a group, across all views.
2. A committed op is never lost while a view-change quorum of non-corrupt replicas exists.
3. Every replica executes the same ops in the same order and reaches a byte-identical grid at each checkpoint.
4. A client request is executed at most once, and its reply is stable.
5. Timestamps are strictly increasing in op order.
6. A replica never acknowledges a prepare it has not made durable.
7. A linearizable read never returns state older than the latest write acknowledged before the read was sent.
8. With a healthy view-change quorum, a group accepts writes within 200 ms of its primary crashing (liveness, checked in simulation with realistic latencies and on real hardware).
9. A single slow or partitioned backup never causes a view change.
