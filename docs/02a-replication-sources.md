# 02a. Replication: sources, rules and deviations

This document annotates [document 02](02-replication.md). For every rule in sections 0 to 9 it names the source and section the rule comes from, or labels it "ours" with the reason. It then lists every deviation from the sources, every source rule we do not adopt, and the corrections this reading forced into document 02. It is the output of roadmap task T2.1.1 and exists so that no protocol code is written from memory.

Status: written from the sources listed in section 1. Not yet approved in review (see section 8).

## 1. Sources

Every citation below names one of these sources and a section of it. Nothing was cited from memory: each source was read in the version and copy given here.

| Key | Work | Copy read | Notes |
|---|---|---|---|
| VRR | Barbara Liskov and James Cowling, "Viewstamped Replication Revisited", MIT CSAIL technical report MIT-CSAIL-TR-2012-021, 2012 | <https://www.cs.princeton.edu/courses/archive/fall19/cos418/papers/vr-revisited.pdf>, 14 pages, SHA-256 `1b16284a0a443d08992bd0fd0f032587e34e3e81f61f1871d39a4e4a6e22cfa6` | The canonical record is <https://dspace.mit.edu/handle/1721.1/71763>. The MIT copies could not be downloaded on 2026-11-18 (`pmg.csail.mit.edu` failed the TLS handshake; DSpace answered 403 and then 429). The copy read is a course mirror; it has the same title, authors and abstract as the DSpace record and sections 1 to 9. The copy itself does not print the report number. |
| PAR | Ramnatthan Alagappan, Aishwarya Ganesan, Eric Lee, Aws Albarghouthi, Vijay Chidambaram, Andrea C. Arpaci-Dusseau and Remzi H. Arpaci-Dusseau, "Protocol-Aware Recovery for Consensus-Based Storage", 16th USENIX Conference on File and Storage Technologies (FAST 2018) | <https://www.usenix.org/system/files/conference/fast18/fast18-alagappan.pdf> | Sections 2.3 and 3 were read in full. Sections 4 and 5 (implementation and evaluation) were not read closely and nothing is cited from them. The paper calls its protocol CTRL. |
| TB | TigerBeetle repository, <https://github.com/tigerbeetle/tigerbeetle>, commit `a2a3c7064519e08233abe26c54a43fca2f9ad9bc` (2026-11-10) | `docs/internals/vsr.md`, `docs/internals/sync.md`, `docs/concepts/safety.md` (sections up to Storage Fault Tolerance), and the comments of `src/vsr.zig`, `src/vsr/replica.zig`, `src/vsr/clock.zig`, `src/vsr/client_sessions.zig`, `src/vsr/client_replies.zig`, `src/vsr/grid_scrubber.zig`, `src/vsr/multi_batch.zig`, `src/vsr/message_header.zig`, `src/config.zig`, `src/state_machine.zig` | `vsr.md` says its terms follow VRR but renames the view-change messages: `exit_view` is start_view_change, `join_view` is do_view_change, `view` is start_view, `get_view` is request_start_view. TigerBeetle changes over time; a claim cited to TB holds for this commit only. |

Citation form: `VRR §4.2 step 1`, `PAR §3.4.3`, `TB vsr.md › Protocol: Start-View-Change` (file and heading), `TB replica.zig on_exit_view` (file and function or field whose comment or code is cited).

## 2. Statements in document 02 marked for checking

Search used, on document 02 as it was before this change (commit `b084eaf`):

```
$ git show b084eaf:docs/02-replication.md | grep -niE "verify|check against|to confirm|confirm|against the paper|paper"
5:Basis: Liskov and Cowling, "Viewstamped Replication Revisited" (2012), with the changes TigerBeetle made for storage faults. Verify every rule here against the paper and against TigerBeetle's published protocol description before coding.
7:## 0. Deployment assumptions (confirmed 2026-11-17)
98:- **Recovery after crash:** load superblock, classify WAL slots (document 01), replay from checkpoint. If the head of the WAL is uncertain, enter `recovering_head` and learn the true head from a start_view before participating. A replica never votes on the basis of a log it cannot verify.
100:- **State sync:** a replica that is more than one checkpoint behind (the WAL ring has wrapped) cannot catch up by replaying. It fetches the latest checkpoint's superblock roots from a quorum-confirmed peer, adopts them, and then repairs every grid block it lacks, lazily and by scrubbing. It becomes a normal backup once the checkpoint's manifest is complete.
```

Line 7 records when the deployment assumptions were agreed and line 98 is a protocol rule (it is R5.2 below); neither asks for a check. Line 100 matched only on "confirmed" inside "quorum-confirmed". So there is one marker, on line 5, and it covers every rule: section 3 below is its outcome. Two further sentences make a claim about a source and were checked as well.

| Statement | Outcome |
|---|---|
| Line 5: "Verify every rule here against the paper and against TigerBeetle's published protocol description before coding." | Done: every rule of sections 0 to 9 is in section 3 with its source. Eight corrections followed (section 6). Line 5 now points to this document. |
| Section 0: quorum-gated suspicion "differs from the letter of VR Revisited". | Confirmed. VRR §4.2 step 1: a replica that notices the need for a view change "advances its view-number, sets its status to view-change", and a replica receiving a STARTVIEWCHANGE for a larger view does the same (VRR §8.1, Liveness). TB does what we do (TB vsr.md › Protocol: Start-View-Change; TB replica.zig on_exit_view). The sentence now says so. |
| Section 9: reconfiguration "is the VR Revisited epoch mechanism adapted to checkpoints". | Confirmed with differences: VRR §7 has epochs, a logged reconfiguration request and state transfer to new members, but the switch happens when the request commits and the old group stops accepting requests at once. Deviation D4. |

## 3. Every rule of document 02 sections 0 to 9

Rule ids are the section of document 02 and a number in the order the rules appear there; a bullet or step that states two rules gets two rows. Every bullet, numbered step and table row of sections 0 to 9 is in a row below. Status: **adopted** (the rule is the source's), **ours** (no source; reason given), **Dn** (a deviation, section 4), **corrected** (document 02 changed in this change, section 6).

### Section 0. Deployment assumptions

| Id | Rule | Source | Status |
|---|---|---|---|
| R0.1 | The timing budget: 10 ms heartbeat, 60 ms suspicion plus up to 20 ms jitter, view change under 10 ms, pipeline repair under 20 ms, client retry 50 ms. | None. VRR §8.1 (Liveness) only says liveness depends on "properly setting the timeouts". | Ours: derived from the 200 ms failover target of the deployment. Tested by T2.2.14. |
| R0.2 | Quorum-gated suspicion. | TB vsr.md › Protocol: Start-View-Change; TB replica.zig on_exit_view. | D1 (from VRR). |
| R0.3 | Event-loop stall bound: no task over 1 ms without yielding, p99.9 loop iteration under 5 ms. | None. | Ours: a stalled primary is indistinguishable from a dead one under a 60 ms suspicion timeout. Tested by T0.3.6. |
| R0.4 | Correlated failover: jitter derived from (group, replica). | None. | Ours: several groups on one node fail over at once; see D11. Tested by T2.2.14. |

### Section 1. Roles and state

| Id | Rule | Source | Status |
|---|---|---|---|
| R1.1 | Primary of view v is replica `v mod replica_count`. | VRR §4: the primary "is computed from the view-number and the configuration", chosen round-robin. TB replica.zig primary_index: `view mod replica_count`. | Adopted. |
| R1.2 | Statuses normal, view_change, recovering, recovering_head. | VRR §4 Figure 2: normal, view-change, recovering. TB replica.zig Status: adds recovering_head, entered at startup when persistent state is corrupt; the replica "can not participate in consensus". | Adopted. Note that VRR's recovering means running the recovery protocol of §4.3, which we do not have (N1); ours is TB's startup state. |
| R1.3 | Persistent state: superblock (view, log_view, checkpoint), WAL, grid. | TB vsr.md › Glossary (superblock, WAL, grid) and › Recovery ("replicas persist their VSR state to the superblock"); TB replica.zig view_durable_update persists view and log_view. | Adopted from TB; D7 from VRR, which keeps no persistent state. |
| R1.4 | Volatile state: op, commit_min, commit_max, pipeline. | VRR §4 Figure 2: op-number, commit-number. TB splits the commit number into commit_min (executed) and commit_max (known committed) (TB sync.md › Algorithm; TB vsr.md › Request/View); pipeline: TB vsr.md › `view`. | Adopted. |
| R1.5 | Flexible quorums Qr and Qv with Qr + Qv > N. | TB vsr.md › Quorums; TB vsr.zig quorums. VRR §2.2 uses f + 1 of 2f + 1 for every step. | D2. Corrected: Qv a majority, and the nack quorum, were missing. |
| R1.6 | Quorum table for 3, 5 and 6 replicas. | TB vsr.md › Quorums gives the same replication and view-change quorums for 3, 5 and 6 replicas, and the nack quorum. | Adopted. Corrected: nack quorum column added. |

### Section 2. Messages

| Id | Rule | Source | Status |
|---|---|---|---|
| R2.1 | A 256-byte header on every message, checksummed. | TB message_header.zig: `@sizeOf(Header) == 256`, checksum fields in every header. VRR §2.1 assumes no corruption. | Adopted from TB. |
| R2.2 | request: a batch of events for one operation type. | VRR §4.1 step 1 (REQUEST op, c, s); TB multi_batch.zig (batches of one operation in one request). | Adopted. |
| R2.3 | prepare: header with op, view, commit, parent checksum, timestamp, client, request, plus body. | VRR §4.1 step 3 (PREPARE v, m, n, k); TB message_header.zig Prepare has view, parent, client, op, commit, timestamp, request, operation. | Adopted. Corrected: commit was missing from the row though section 3 relies on it. |
| R2.4 | prepare_ok: durable acknowledgement of one prepare. | VRR §4.1 step 4 (PREPAREOK v, n, i); durability: TB vsr.md › Protocol: Normal step 4. | Adopted; durability is D7. |
| R2.5 | commit: heartbeat carrying commit_max. | VRR §4.1 step 6 (COMMIT v, k); TB vsr.md › Protocol: Normal step 7. | Adopted. |
| R2.6 | reply. | VRR §4.1 step 5 (REPLY v, s, x). | Adopted. |
| R2.7 | start_view_change: vote to leave the current view. | VRR §4.2 step 1; TB `exit_view`. | Adopted; its meaning as a vote is D1. |
| R2.8 | do_view_change: log suffix headers, log_view, op, commit. | VRR §4.2 step 2 (DOVIEWCHANGE v, l, v', n, k) with the whole log; VRR §5.3 sends only a suffix; TB `join_view` sends headers only. | Adopted. |
| R2.9 | start_view: authoritative suffix headers. | VRR §4.2 step 3 (STARTVIEW v, l, n, k); TB `view`. | Adopted. |
| R2.10 | request_start_view: ask for start_view after falling behind. | TB vsr.md › Protocol: Request/View (`get_view`). VRR §5.2 GETSTATE is the nearest. | Adopted from TB. |
| R2.11 | request_headers, headers: repair of the header chain. | TB vsr.md › Protocol: Repair Journal. | Adopted. |
| R2.12 | request_prepare by (op, checksum). | TB vsr.md › Protocol: Repair WAL. | Adopted. |
| R2.13 | nack_prepare. | PAR §3.4.3 (the dontHave response); TB carries nacks as a bitset inside `join_view` (TB replica.zig, the comment above JVQuorum and the "Nack bit" cases where the bitset is built). | D3. Corrected: the meaning was "I never had this prepare", which is wrong in two ways (section 6, C3). TB's command table (TB vsr.md › Commands) has no nack message, although the comment on its get_prepare handler still speaks of sending one. Carrying nacks as a separate message is open point O4. |
| R2.14 | request_block, block: grid repair by (address, checksum). | TB vsr.md › Protocol: Repair Grid (`get_blocks`, `block`). | Adopted. |
| R2.15 | ping, pong: liveness and clock samples. | TB vsr.md › Protocol: Ping (Replica-Replica); TB clock.zig module comment. | Adopted. |
| R2.16 | request_sync_checkpoint, sync_checkpoint: state sync. | None. TB moves the checkpoint inside the `view` message so that "the state (superblock) and the WAL are updated atomically" (TB sync.md, introduction). | Ours; part of D9. |
| R2.17 | eviction: session evicted. | TB vsr.md › Protocol: Client. | Adopted. |

### Section 3. Normal operation

| Id | Rule | Source | Status |
|---|---|---|---|
| R3.1 | Primary checks the session, assigns op + 1 and a timestamp, sets parent to the checksum of prepare op - 1. | VRR §4.1 steps 2 and 3; parent hash chain: TB message_header.zig Prepare.parent and TB safety.md › Storage Fault Tolerance ("hash-chained"). | Adopted. Timestamps: section 6. |
| R3.2 | Primary writes the prepare to its WAL and sends it concurrently. | TB replica.zig on_prepare comment: the primary sends the prepare "in parallel to writing to its own journal"; TB vsr.md › Protocol: Normal step 3. | Adopted. |
| R3.3 | Ring replication by default, direct fallback on timeout. | None. TB vsr.md › Protocol: Normal step 3 describes a chain, but TB replica.zig on_prepare at the commit read describes star replication. VRR §4.1 step 3 sends to all. | D8. |
| R3.4 | Backup accepts a prepare only in the same view, in status normal, extending its hash chain; writes, then acknowledges; gaps start repair. | VRR §4.1 (only status normal and only the same view-number). Hash chain and repair of gaps: TB vsr.md › Protocol: Repair Journal. Write then acknowledge: TB vsr.md › Protocol: Normal step 4. | Adopted. Accepting with gaps is N2; writing before acknowledging is D7. |
| R3.5 | Commit on Qr prepare_oks including the primary's own durable write; execute in order; reply; advance commit_max. | VRR §4.1 step 5 (f prepare_oks from backups, executes after all earlier ops); TB vsr.md › Protocol: Normal step 5 ("and it has committed all preceding prepares"). | Adopted; Qr instead of f + 1 is D2. |
| R3.6 | Backups learn commit_max from the next prepare or a commit heartbeat. | VRR §4.1 step 6; TB vsr.md › Protocol: Normal step 7. | Adopted. |
| R3.7 | Backups execute in order up to commit_max once they hold the prepare. | VRR §4.1 step 7 and the paragraph after it (backups should not lag far behind). | Adopted. |
| R3.8 | Pipeline of up to 8 prepares; further requests queue. | TB config.zig `pipeline_prepare_queue_max: u32 = 8`. VRR has no pipeline bound. | Adopted from TB. |
| R3.9 | Request coalescing: one prepare holds request frames from many clients and operations. | VRR §6.2 (batching); TB multi_batch.zig. | D6. |
| R3.10 | Execution is sequential and deterministic; the state machine receives (op, timestamp, operation, body) only. | VRR §3 (deterministic operations); VRR §4.4 (the primary predicts non-deterministic values and stores them in the log). | Adopted. |

### Section 4. View change

| Id | Rule | Source | Status |
|---|---|---|---|
| R4.1 | Triggers: suspicion, or a do_view_change or start_view of a higher view; a prepare or commit of a higher view leads to request_start_view; a lone start_view_change moves no replica. | VRR §4.2 step 1 (own timer, or a STARTVIEWCHANGE or DOVIEWCHANGE of a larger view). TB replica.zig on_join_view comment (a backup that receives a `join_view` for a new view moves to it); TB vsr.md › `get_view` (a newer view seen in a prepare or commit). | Corrected (C4); D1. |
| R4.2 | Step 1: a suspecting backup votes with start_view_change, keeps its view and status, repeats the vote, stops when it hears from the primary. | TB vsr.md › Protocol: Start-View-Change steps 1 to 3. | Corrected (C4); D1. |
| R4.3 | Step 2: on Qv votes, increment view, status view_change, stop accepting old prepares, persist view and log_view, send do_view_change. | VRR §4.2 step 2; VRR §8.1 (stop accepting prepares of the old view once in the view change); TB vsr.md › Protocol: Start-View-Change step 4. Durability first: VRR §8.2 (the original VR wrote the view number to disk before DOVIEWCHANGE); T2.2.3 has a gate for it. | Corrected (C4). |
| R4.4 | Step 3: the new primary takes the log with the highest (log_view, op) from Qv do_view_changes, installs the headers, repairs missing prepares. | VRR §4.2 step 3 ("largest v'", then "largest n"); TB replica.zig on_join_view comment; TB vsr.md › Protocol: View-Change steps 3 to 5. A primary with faulty entries may be elected and repairs before accepting commands: PAR §3.4.1. | Adopted. Corrected (C9): log_view is durable with the headers it names. |
| R4.5 | Step 4: an uncommitted op is truncated, with all later ops, only on a nack quorum; an op corrupt everywhere that may have committed blocks the view change. | PAR §3.4.2 and §3.4.3 (Cases 1 to 3); TB vsr.md › Protocol: View-Change ("nack quorum of blank headers") and › Quorums. | D3. Corrected (C5): "enough nack_prepare messages" now names the nack quorum. |
| R4.6 | Step 5: the new primary sends start_view, re-replicates the uncommitted suffix, resumes. | VRR §4.2 steps 3 to 5 (backups send PREPAREOK for uncommitted ops on STARTVIEW). TB vsr.md › Protocol: View-Change steps 6 and 7 commit before broadcasting the view. | Adopted from VRR; the ordering difference with TB is open point O3. |
| R4.7 | Timeouts use deterministic exponential backoff with per-replica jitter derived from the replica index. | TB replica.zig field `prng`: "exponential backoff with random jitter", "Seeded with the replica's random_nonce". | D11. |

### Section 5. Repair, recovery and state sync

| Id | Rule | Source | Status |
|---|---|---|---|
| R5.1 | WAL repair: request a missing or corrupt prepare by (op, checksum); the hash chain names the expected checksum. | TB vsr.md › Protocol: Repair WAL and › Protocol: Repair Journal. A replica whose own copy is corrupt sends nothing rather than a nack: TB vsr.md › Protocol: Repair WAL, citing PAR. Identifying entries by more than their position: PAR §3.3.4. | Adopted. |
| R5.2 | Recovery after a crash: load the superblock, classify WAL slots, replay from the checkpoint; an uncertain head means recovering_head until a start_view; never vote on a log that cannot be verified. | TB replica.zig, open path (the head op is recovered from the WAL or from the superblock's view headers; if it cannot be, the replica enters recovering_head and waits for a `view`); TB replica.zig Status. Telling a torn write from a corruption: PAR §3.3.3. | Adopted. WAL slot classification itself is document 01's subject and was not checked here. |
| R5.3 | Grid repair by (address, checksum); background scrubbing at a fixed pace. | TB vsr.md › Protocol: Repair Grid; TB grid_scrubber.zig module comment. PAR §3.5.1 and §3.5.2: identical checkpoints on every replica make repair of single chunks possible. | Adopted. |
| R5.4 | State sync for a replica more than one checkpoint behind: adopt the latest durable checkpoint's roots, repair grid blocks lazily and by scrubbing, become a normal backup when the manifest is complete. | TB sync.md › Algorithm, › 0: Scenarios ("If a replica lags by more than one checkpoint behind the primary, it must use state sync"; "State sync is incorrect if there is only a single other replica on the next checkpoint") and › Glossary (durable checkpoint: present on a replication quorum); lazy sync: TB sync.md, introduction. VRR §5.1 and §5.2 (state transfer from checkpoints). | Adopted, with D9. Corrected (C6): "quorum-confirmed peer" is now the durable checkpoint. |

### Section 6. Clocks and timestamps

| Id | Rule | Source | Status |
|---|---|---|---|
| R6.1 | One clock sampler per node pair, shared by all shards on the node. | None. TB has one replica per process and one clock per replica. | Ours: a node hosts many groups (document 04); sampling once per node pair saves traffic and is safe because only the primary reads the clock and the value travels in the prepare. Tested by T2.2.8. |
| R6.2 | ping and pong carry monotonic and wall-clock samples; Marzullo's algorithm finds the offset interval a quorum agrees with. | TB clock.zig module comment; TB marzullo.zig; the quorum is the replication quorum (TB replica.zig, `Clock.init(... .quorum = quorum_replication ...)`). | Adopted. Corrected (C7): "a quorum" now says "a replication quorum of clocks". |
| R6.3 | A primary whose clock is outside the interval assigns no timestamps and steps down. | TB clock.zig realtime_synchronized clamps the primary's clock into the interval; only when there is no interval does the primary drop requests and abdicate (TB replica.zig on_request). | D10. |
| R6.4 | Prepare timestamp = max(previous prepare timestamp + previous batch size, synchronised clock, hybrid-clock floor); each event gets a distinct increasing timestamp. | TB replica.zig, where the primary sets `prepare_timestamp = max(max(prepare_timestamp, commit_timestamp) + 1, realtime)`; TB state_machine.zig adds the batch's delta; TB clock.zig module comment (objects get `prepare_timestamp + object_index`). | Adopted, except the hybrid-clock floor, which is ours (R6.5). TB also takes commit_timestamp because a newly elected primary may be behind; ours uses the head of the log the new primary installs, which is at least every committed op. Invariant 5 across view changes is a gate of T2.2.8. |
| R6.5 | The hybrid-clock floor is raised by timestamps from clients and other groups. | None. | Ours: cross-group ordering (document 04). Tested by T5.2.5. |
| R6.6 | Timestamps are u64 nanoseconds, unique within a group and increasing with op order. | TB clock.zig module comment ("u64 nanosecond-precision creation timestamp", unique, consistent with linearization order). | Adopted. |

### Section 7. Client sessions

| Id | Rule | Source | Status |
|---|---|---|---|
| R7.1 | Random u128 client id; `register` through the log before the first write. | TB vsr.md › Protocol: Client; TB client_sessions.zig comment (sessions are registered through the state machine because VRR's client table has a correctness bug). | Adopted from TB; N4. |
| R7.2 | Sessions cover writes only. | None. | Ours: reads never enter the log (section 8), so they need no deduplication. Part of D12. |
| R7.3 | One write in flight per session, numbered consecutively; table in an internal LSM tree; replies inline up to 4 KiB, larger ones in grid blocks; no separate client-replies zone. | One request at a time with increasing numbers: VRR §4. TB keeps reply headers in the sessions trailer and bodies in a client_replies zone (TB client_replies.zig). | One in flight adopted; storage is ours, D12. |
| R7.4 | A duplicate of the last request returns the stored reply; an older number is dropped. | VRR §4.1 step 2. | Adopted. |
| R7.5 | A session entry changes only when its request executes. | TB client_sessions.zig comment ("a more careful distinction between uncommitted and committed request numbers"). | Corrected (C8); N5. |
| R7.6 | Capacity 16,384 sessions; deterministic least-recently-used eviction by last request op. | TB client_sessions.zig evictee (deterministic, "the entry with the oldest commit number"); TB config.zig `clients_max = 64` by default. | Eviction adopted; capacity is ours, D12. |
| R7.7 | A client that needs concurrent writes opens several sessions. | None. | Ours: follows from one write in flight per session. Tested by T2.2.9. |

### Section 8. Reads

| Id | Rule | Source | Status |
|---|---|---|---|
| R8.1 | Linearizable read without the log, by quorum-confirmed leadership. | VRR §6.3.1 uses leases; TB puts reads through the log. | D5. |
| R8.2 | Snapshot read at T on any replica whose commit timestamp is at least T. | VRR §6.3.2 (reads at backups, with the client's last request number for causality). | D13. |

### Section 9. Standbys and reconfiguration

| Id | Rule | Source | Status |
|---|---|---|---|
| R9.1 | A standby receives prepares, never votes, can be promoted. | TB replica.zig: `standby_count`; prepares are broadcast "to every other replica and standby"; standby clocks are ignored for cluster time. | Adopted. Promotion needs reconfiguration (R9.2). |
| R9.2 | Replica replacement by a logged reconfigure op that takes effect at the next checkpoint boundary. | VRR §7. TB vsr.md › Protocol: Reconfiguration: "TODO (Unimplemented)". | D4. |

## 4. Deviations

The six deviations the task names are D1 to D6. D7 to D13 are further places where document 02 does something other than a source.

### D1. Quorum-gated suspicion

- **Source.** VRR §4.2 step 1: a replica whose timer expires advances its view and sets status view-change at once; one that receives a STARTVIEWCHANGE for a larger view does the same and sends its own (VRR §8.1, Liveness). One backup with a bad network link can therefore move the whole group to a new view.
- **We.** A suspecting backup sends start_view_change as a vote and stays in its view, still acknowledging the primary. Only a replica holding Qv votes for the same view changes status (document 02 section 4 steps 1 and 2). This is TigerBeetle's protocol (TB vsr.md › Protocol: Start-View-Change; TB replica.zig on_exit_view: "This ensures that we do not start a view-change while normal request processing is possible").
- **Why.** Invariant 9: a single slow or partitioned backup never causes a view change. TB vsr.md gives the same reason for asymmetric partitions: a replica that can send but not receive cannot reach a quorum, so the view stays stable.
- **What could go wrong.** VRR's safety argument (VRR §8.1) needs replicas to stop accepting prepares of the old view once they take part in a view change; under gating a voter keeps acknowledging, so the rule must be that no replica sends prepare_ok in view v after it has sent do_view_change for a later view. If votes for different views never reach Qv at once (they are retried and expire), the group can fail to leave a dead primary; liveness then depends on the retry and jitter timing of R0.1. A primary that still sends heartbeats but whose prepares do not get through keeps every backup quiet; TB counters this with primary abdication, which document 02 does not have (open point O1).
- **Checked by.** T2.1.2 (gate: a single backup's vote changes no replica's status), T2.2.14 (invariant 9, and the injected bug "change status on own timeout").

### D2. Flexible quorums

- **Source.** VRR §2.2: groups of 2f + 1 and quorums of f + 1 for every step; for other group sizes the quorum is K - f, and a larger group "requires larger quorums ... but does not tolerate more failures".
- **We.** A replication quorum Qr and a view-change quorum Qv with Qr + Qv > N and Qv a majority, so 3/4 at six replicas (document 02 section 1). This is TB vsr.md › Quorums and TB vsr.zig quorums, which cites Flexible Paxos and asserts both conditions.
- **Why.** Commits wait for the smaller quorum; view changes, which are rare, pay for the larger one (TB vsr.zig quorums comment). Six replicas survive the loss of any two (TB safety.md › High Availability argues the same for three providers of two).
- **What could go wrong.** A pair that does not intersect allows a view change that misses a committed op (invariant 2). At six replicas a committed op may be on only three replicas, and losing three replicas stops view changes while the remaining three could still commit. PAR's nack rule ("a majority of the followers", PAR §3.4.2) is written for majority quorums; with flexible quorums the bound is N - Qr + 1 (D3). At six replicas a majority of the five followers is three nacks, one fewer than needed, and would truncate committed ops.
- **Checked by.** T2.1.2 (5 replicas, and 6 replicas with quorums 3 and 4; injected bug "quorums that do not intersect"), T2.2.3.

### D3. Nack-based truncation

- **Source.** VRR §4.2 keeps everything in the chosen log ("Operations that had not committed might also survive") and assumes no storage faults (VRR §2.1). PAR §3.4.2 and §3.4.3: the leader asks for each faulty entry; one intact copy repairs it, a majority of dontHave answers proves it uncommitted and it is discarded with every later entry, haveFaulty answers make the leader wait. PAR §2.3 shows that truncating or crashing locally instead loses data or availability (PAR Table 1, Figure 2).
- **We.** A new primary truncates an op, and every later op, only on a nack quorum Qn = N - Qr + 1. A nack means the sender certainly never acknowledged that prepare: it has no prepare at that op and its slot is not faulty, or it has a different prepare there, or it has this one but not durably (TB replica.zig, the three "Nack bit" cases; the JV header table above JVQuorum). A corrupt slot never nacks (TB vsr.md › Protocol: Repair WAL). An op corrupt on every replica that could have acknowledged it blocks the view change (PAR §3.4.3 Case 3).
- **Why.** Availability with storage faults: without it, one corrupt uncommitted entry on the new primary can stop the group for good (PAR §3.4.2, Figure 4 case (c)(i)).
- **What could go wrong.** A replica that nacks a prepare it did acknowledge (a slot overwritten by a later wrap, a misdirected write read back as "absent", a nack counted for the wrong view) lets a committed op be truncated: invariant 2 fails silently. Nacks are only meaningful for the log_view they were collected in. Blocking on corruption everywhere is unavailability by design.
- **Checked by.** T2.1.2 (injected bug "truncation without enough nacks"), T2.2.3 (one nack fewer than required; the corrupt-everywhere scenario; a checker that no acknowledged op is truncated).

### D4. Checkpoint-based reconfiguration

- **Source.** VRR §7: a RECONFIGURATION request is committed by the old group; the primary stops accepting other requests as soon as it receives it (VRR §7.1); the new group starts in epoch e + 1 at view 0 (VRR §7.1.1, §8.3) and serves only after f' + 1 new replicas have the state; old replicas wait for f' + 1 EPOCHSTARTED messages (VRR §7.1.2); CHECKEPOCH tells the administrator when it is safe to shut old nodes down (VRR §7.3). VRR §7.5 names the cost: no client requests are accepted while state transfer runs. TB has not implemented reconfiguration (TB vsr.md › Protocol: Reconfiguration).
- **We.** The reconfigure op takes effect at the next checkpoint boundary; the old configuration keeps committing until then; a new member counts toward quorums only after reaching that checkpoint by state sync (document 02 section 9).
- **Why.** No window without writes: new members are warmed by state sync beforehand, which VRR §7.5 itself recommends, and the switch lands on a checkpoint where all replicas hold identical state.
- **What could go wrong.** Ops between the reconfigure op and the boundary, and view changes in that interval, are where both configurations could commit at the same op number. Document 02 does not say which view the new configuration starts in; VRR §8.3 shows that inheriting the old view number can give two primaries (open point O5). A removed replica that keeps running must not disturb the new group.
- **Checked by.** T7.1.1 (model, with these defects as injected bugs), T7.1.2 (implementation).

### D5. Linearizable reads without the log

- **Source.** VRR §6.3.1: the primary answers reads alone while it holds leases from f other replicas, and a new view starts only after those leases expire; this assumes loosely synchronised clock rates. TB does not use leases (TB safety.md › High Availability) and reads go through consensus: every request becomes a prepare (TB replica.zig on_request), lookups included.
- **We.** The primary notes its commit number when the read arrives, waits until a replication quorum has acknowledged a message it sent after that moment, then serves from state at or after the noted commit (document 02 section 8).
- **Why.** No WAL write and no clock assumption per read. The argument: a view change completes only after Qv replicas have left view v; a replica that acknowledges in view v after the read arrived had not left; Qr + Qv > N means no later view had started when the read arrived.
- **What could go wrong.** An acknowledgement of a message sent before the read arrived proves nothing; the argument needs strictly later messages. A newly elected primary must have committed every op kept by the view change before it serves, or it misses writes the old primary acknowledged. Backups must acknowledge heartbeats, and document 02 section 2 has no message for that (open point O6). Under D1 a backup that has voted still acknowledges, which is safe only because a view change needs Qv replicas to have changed status, not merely voted.
- **Checked by.** T2.2.13 (injected bug "serve without waiting"; a cut-off primary answers nothing; the technique in the model of T2.1.2 with the property that a deposed primary cannot answer), T2.3.2 (linearizability checker).

### D6. Request coalescing

- **Source.** VRR §6.2: the primary collects requests and runs the protocol once for the batch when it is busy. TB: one request per prepare; a multi-batch request lets one client put several batches of the same operation in one message (TB multi_batch.zig).
- **We.** A prepare body is a sequence of request frames (client, request number, operation, events) from any client and any operation, filled in arrival order; frames execute in order, each updates its own session; replies are split per client (document 02 section 3).
- **Why.** Thousands of clients with small batches (document 02 section 0); one prepare per request would cap throughput at prepares per second.
- **What could go wrong.** Exactly-once is now per frame: a frame's session update, its reply and its timestamps must be exact even when another frame in the same prepare fails. One fast client can fill every prepare. A prepare must never exceed the message size, and a request that fits alone must never be refused.
- **Checked by.** T2.2.12 (invariant 4 with coalescing, frame order, failing frame isolation, fairness, size property, fuzzing of the frame decoder).

### D7. A durable log and persistent protocol state

- **Source.** VRR §4.1: "The protocol does not require any writing to disk"; VRR §4.3 recovers a replica's state from f + 1 others with a nonce, and warns that simultaneous failure of all replicas loses state unless they have UPS or non-volatile memory.
- **We.** Prepares are durable before prepare_ok; view, log_view and the checkpoint are in the superblock (document 02 sections 1 and 3). This is TB (TB vsr.md › Recovery: "Unlike [VRR], TigerBeetle does not implement Recovery Protocol").
- **Why.** One data center (document 02 section 0): a power loss takes every replica at once. Storage faults need on-disk state to repair from.
- **What could go wrong.** A replica that acknowledges before its write is durable breaks invariant 6 and, with a crash, invariant 2.
- **Checked by.** T2.1.2 (injected bug "a backup acknowledging before its write is durable"), T2.2.2, T2.2.5.

### D8. Ring replication

- **Source.** VRR §4.1 step 3: the primary sends PREPARE to every backup. TB vsr.md › Protocol: Normal step 3 describes a chain from the primary, but TB replica.zig on_prepare at the commit read says the primary broadcasts to every replica and standby: "Star replication prioritizes latency: with Flexible Paxos the primary can commit after the fastest prepare_ok quorum returns, without waiting for a ring of forwards." TB's own document and code disagree.
- **We.** A ring by default, with direct sending on timeout.
- **Why.** The primary's outbound bandwidth stops growing with the replica count.
- **What could go wrong.** Commit latency follows the ring, not the fastest quorum; a slow ring member delays everyone after it until the fallback fires, which eats into the 200 ms budget. T2.2.10 already switches the default to direct if the ring is more than 10% slower at the median.
- **Checked by.** T2.2.10.

### D9. State sync: who counts, and how the checkpoint arrives

- **Source.** TB sync.md › Syncing Replica: a syncing replica takes part in replication and can become primary, but withholds prepare_ok until the cluster's commit_max shows its checkpoint is durable. The checkpoint and the log suffix arrive together in one `view` message so they are installed atomically (TB sync.md, introduction).
- **We.** The syncing replica becomes a normal backup only when the checkpoint's manifest is complete (document 02 section 5), and the checkpoint arrives in its own messages (R2.16).
- **Why.** Ours is the simpler rule: a replica that is still fetching its state never counts. T2.2.7 states the same.
- **What could go wrong.** Availability: while a large replica syncs, the group runs with one fewer voter for longer than TB would. Installing a checkpoint apart from the log suffix can leave a crash point with a new checkpoint and an old suffix; T2.2.7 has a gate for crashes during sync.
- **Checked by.** T2.2.7.

### D10. Clock outside the agreed interval

- **Source.** TB clock.zig realtime_synchronized clamps the primary's own clock into the interval the quorum agrees on; the primary drops requests and abdicates only when no such interval exists (TB replica.zig on_request).
- **We.** A primary whose own clock lies outside the interval assigns no timestamps and steps down (document 02 section 6; T2.2.8 has a gate for it).
- **Why.** Clocks are disciplined to well under a millisecond in one data center (document 02 section 0); a clock outside the interval points to a broken node, which should not lead.
- **What could go wrong.** Each time the faulty node's turn as primary comes round, the group pays one extra view change. When no interval agrees with a quorum, every primary steps down and the group stops accepting writes, as in TB.
- **Checked by.** T2.2.8.

### D11. Deterministic timeout jitter

- **Source.** TB replica.zig: backoff jitter from a PRNG seeded with the replica's random nonce. VRR §8.1 says only that timeouts must be set well.
- **We.** Exponential backoff with jitter derived from (group, replica) (document 02 sections 0 and 4).
- **Why.** The simulator must reproduce timings from a seed; groups that lose a primary on the same node must not time out together.
- **What could go wrong.** The jitter order between two replicas of one group is the same in every view change, so a replica that always fires first and is itself slow keeps starting view changes that fail.
- **Checked by.** T2.2.14 (jitter from (group, replica); failover bound over 100,000 seeds).

### D12. Session capacity and reply storage

- **Source.** TB: `clients_max = 64` by default (TB config.zig); reply headers in the client-sessions trailer and bodies in a separate client_replies zone, repaired from peers (TB client_replies.zig).
- **We.** 16,384 sessions per group, writes only, the table in an internal LSM tree with replies inline up to 4 KiB and in grid blocks above that (document 02 section 7).
- **Why.** Hundreds to thousands of client processes (document 02 section 0).
- **What could go wrong.** A reply lost on every replica locks its client out, retrying a request that already ran; TB client_replies.zig describes the same case and leaves it to the operator. Least-recently-used eviction under churn evicts sessions with requests still in flight; the client must re-register and must never see the old request run twice.
- **Checked by.** T2.2.9.

### D13. Snapshot reads by timestamp

- **Source.** VRR §6.3.2: reads at backups, where the client sends the op number of its last write and the backup answers only once it has executed that far.
- **We.** A snapshot read at timestamp T on any replica whose commit timestamp is at least T (document 02 section 8).
- **Why.** Timestamps are the version numbers of the data (document 02 section 6), so a timestamp names the snapshot directly and works across groups.
- **What could go wrong.** A replica must know that every op with a timestamp at or below T has been committed and executed, not merely that its latest commit's timestamp passed T.
- **Checked by.** T3.1.4.

Every deviation above is linked to a task; none is uncovered.

## 5. Source rules we do not adopt

| Id | Rule | Source | Reason |
|---|---|---|---|
| N1 | Recovery protocol: RECOVERY to all, f + 1 RECOVERYRESPONSE with a nonce, state taken from the primary. | VRR §4.3, §8.2 | State is durable (D7); TB drops it for the same reason (TB vsr.md › Recovery). |
| N2 | Backups accept prepares strictly in op order and wait for missing ones. | VRR §4.1 step 4 | We accept with gaps and repair through the hash chain (TB vsr.md › Protocol: Repair Journal). VRR allows this but warns that it complicates the view change (VRR §4.1, after step 7); the do_view_change header rules of D3 are that complication. |
| N3 | do_view_change and start_view carry the whole log. | VRR §4.2 | Suffix headers only, as VRR §5.3 itself proposes and TB does. |
| N4 | A recovering client fetches its last request number and adds 2. | VRR §4.5 | Sessions are registered through the log instead; TB client_sessions.zig records that VRR's client table lets request numbers collide after successive client crashes. |
| N5 | The primary updates the client table when it prepares a request. | VRR §4.1 step 3 | The table changes only on execution (R7.5); TB client_sessions.zig calls the VRR rule a liveness bug, because a request reordered through a view change locks its client out. |
| N6 | Checkpoints of application pages with a Merkle tree; state transfer compares trees; cloning a disk for long outages. | VRR §5.1, §5.2 | Our state is the grid of checksummed blocks; sync adopts the checkpoint's roots and repairs blocks by (address, checksum) (TB sync.md). |
| N7 | Witnesses: f replicas that store no state. | VRR §6.1 | Every replica stores the full state, which grid repair and byte-identical checkpoints rely on. Standbys (R9.1) are not witnesses: they store state but never vote. |
| N8 | Read leases. | VRR §6.3.1 | D5. |
| N9 | Stop accepting client requests when a reconfiguration request arrives; switch when it commits. | VRR §7.1, §7.5 | D4. |
| N10 | A persist record per log entry in a separate region, written before one fsync; two copies of the metainfo file; snapshots written to a temporary file and renamed. | PAR §3.3.3, §3.3.4, §3.3.1 | These are CLSTORE's answers for a log kept in files on a file system. Our WAL and superblock layout and their crash and corruption classification are document 01's subject and outside this check. |
| N11 | snap and gc markers in the log to make every node snapshot at the same index. | PAR §3.5.1 | All replicas checkpoint at the same op numbers by rule (T2.2.7), which gives the same identical checkpoints without markers. |
| N12 | Leader restriction: a node with a faulty entry cannot be elected. | PAR §3.4 ("Naive Approach") | PAR shows one fault can then make the group unavailable; we follow PAR §3.4.1 and let a faulty replica lead and repair (R4.4). |

## 6. Corrections made to document 02 in this change

| Id | Where | Before | After | Why |
|---|---|---|---|---|
| C1 | Line 5 | "Verify every rule here ... before coding." | Says the check is done and points here. | The check is this document. |
| C2 | Section 1, quorums | Only Qr + Qv > N; no nack quorum. | Adds "Qv a majority" and Qn = N - Qr + 1, with a column in the table. | TB vsr.zig quorums asserts both; section 4 relied on an undefined "enough" nacks. |
| C3 | Section 2, prepare and nack_prepare rows | prepare without commit; nack_prepare as "I never had this prepare". | prepare carries commit; a nack means "I certainly never acknowledged this prepare", with the three cases, and a corrupt slot never nacks. | Section 3 step 4 sends commit_max in prepares (VRR §4.1 step 3, k). "Never had" is too wide (a replica whose slot is corrupt cannot know, PAR §3.4.3 haveFaulty) and too narrow (holding a different prepare, or holding it not yet durably, are safe nacks, TB replica.zig "Nack bit" cases). |
| C4 | Section 4, trigger and steps 1 and 2 | Step 1 "increments view, sets status view_change, sends start_view_change" on any suspicion; "sees a higher view" as a trigger. | Step 1 votes without changing status; step 2 changes status on Qv votes and makes view and log_view durable before do_view_change; triggers narrowed. | The old step 1 is VRR §4.2 step 1 and contradicted quorum-gated suspicion in section 0 of the same document. A lone start_view_change for a higher view would let one backup move others. Durability before sending was in T2.2.3 but not in document 02. |
| C5 | Section 4 step 4 | "enough nack_prepare messages" | "a nack quorum (section 1)", and truncation takes every later op too. | PAR §3.4.2: the entry "and all subsequent entries" are discarded, since entries commit in order. |
| C6 | Section 5, state sync | "from a quorum-confirmed peer" | "the latest durable checkpoint", defined as held by a replication quorum and learned from commit_max, which works because replicas acknowledge prepares beyond a checkpoint only once they hold it; one peer alone is never enough. | TB sync.md › Glossary, › 0: Scenarios and › Syncing Replica. "Quorum-confirmed peer" did not say what was confirmed. The acknowledgement rule is what T2.2.7's gate "acknowledges no prepares beyond what it holds durably" already requires. |
| C7 | Section 6 | "agrees with a quorum" | "agrees with a replication quorum of clocks" | TB replica.zig passes quorum_replication to the clock. |
| C8 | Section 7 | Session table update time unstated. | A session entry changes only when its request executes. | N5. |
| C9 | Sections 1, 4 step 3, 5 | log_view durable in the superblock, with no rule for the log it names. | The superblock holds the headers of the log installed with log_view, written with it on a view change, on start_view and on truncation; a slot they name but whose prepare is lost is repaired and does not make the head uncertain. | Ours, after this document's review: the model of T2.1.2 (`models/vsr`, defect `LogViewWithoutHeaders`) found that without it a new primary that crashes before its installed prepares are durable restarts with that log_view and a shorter log, which wins the next view change and loses a committed op (invariant 2). |
| C10 | Section 5, recovery | What a restarted primary does was unstated. | It does not resume its view; it takes status recovering and votes for the next view. | Ours, after this document's review: the model of T2.1.2 identifies a prepare by (op, view), which holds only if no primary assigns an op number twice in one view; a primary that restarted and resumed could. |

Section 0's sentence on quorum-gated suspicion now also says that it matches TigerBeetle (section 2 of this document).

## 7. Open points

These are places where a source has a rule that document 02 neither adopts nor rejects, or where the sources disagree. They are not decided here; each names the task that must decide it.

| Id | Point | Source | Decided by |
|---|---|---|---|
| O1 | Primary abdication: a primary that has a prepare without a quorum of prepare_oks for a while, or that drops requests for want of a synchronised clock, stops sending commit heartbeats so the backups suspect it. Without it, a primary whose heartbeats get through but whose prepares do not is never replaced. | TB replica.zig fields `primary_abdicating`, `primary_abdicate_timeout`; TB vsr.md › Protocol: Start-View-Change step 1 | T2.2.14 |
| O2 | Forfeit: a new primary whose do_view_change quorum holds a log more than one checkpoint ahead of its own gives up the view at once rather than repair across a wrapped WAL. | TB vsr.md › Protocol: View-Change, steps 1 and 2 | T2.2.3, with T2.2.7 |
| O3 | Order at the end of a view change: VRR sends STARTVIEW and collects PREPAREOKs for the uncommitted suffix afterwards; TB commits every op not known to be uncommitted before it sends `view`. | VRR §4.2 steps 3 to 5; TB vsr.md › Protocol: View-Change steps 6 and 7 | T2.2.3 |
| O4 | Nacks as a separate nack_prepare message, which document 02 uses both in the view change and in WAL repair (T2.2.4), or as a bitset inside do_view_change as in TB, which saves a round trip in the view change. | TB replica.zig, the JV header table above JVQuorum; TB vsr.md › Commands | T2.2.3, with T2.2.4 |
| O5 | The view number a new configuration starts in. VRR starts every epoch at view 0 and explains why inheriting the old view can give two primaries. | VRR §7.1.1 step 1, §8.3 | T7.1.1 |
| O6 | Backups acknowledging heartbeats (needed by D5) have no message in document 02 section 2. | Document 02 section 8 | T2.2.13 |

## 8. Review

This document and the corrections to document 02 need approval by two engineers, at least one of whom did not write document 02 (gate of T2.1.1). That review has not happened yet.
