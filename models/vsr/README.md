# VSR model

A TLA+ specification of one replication group of [document 02](../../docs/02-replication.md): normal operation, view change with quorum-gated suspicion and nack-based truncation, recovery with a torn or corrupt log, and flexible quorums, under message loss, duplication and reordering, crashes, and corruption or loss of log entries on fewer than a replication quorum. It is roadmap task T2.1.2. Reconfiguration (T7.1.1), clocks and liveness are not modelled.

`just model vsr` checks it. The rules of this folder are in [models/README.md](../README.md).

## Modules

| File | What it is |
|---|---|
| [Vsr.tla](Vsr.tla) | The specification, its fault model and its properties |
| `Vsr3`, `Vsr3Views`, `Vsr5`, `Vsr6` (`.tla` and `.cfg`) | Runs with 3, 5 and 6 replicas; each `.cfg` states its bounds |
| [defects/](defects) | Injected defects, each of which must produce a counterexample |

## Properties

| Property | Document 02 | Kind |
|---|---|---|
| `OneCommitPerOp` | Invariant 1: at most one op is committed at an op number | invariant |
| `CommittedNotLost` | Invariant 2: a prepare committed in view cv is in every log of view cv or later, and no primary of such a view starts without it | invariant |
| `AckedIsDurable` | Invariant 6: an acknowledged prepare stays durable on its replica (corruption aside) until that replica adopts a later view's log | invariant |
| `QuorumGatedSuspicion` | Section 0 and D1 of document 02a: no replica leaves status normal for a view change without Qv distinct votes for the new view, so a single backup's vote changes no replica's status (Qv is at least 2) | action property |
| `TypeOK` | Types and bounds | invariant |

## Modelling choices

- **Messages** are sets that are never emptied: a message may be received any number of times, in any order, or never. That is loss, duplication and reordering at once.
- **A prepare** is identified by its op and the view it was created in, which stands for its checksum: one primary per view assigns each op once, and a restarted primary never resumes its view (status `recovering`, it votes for the next one).
- **Storage faults.** A write is `pending` until durable. A crash drops every pending write at the end of the log or, if `torn`, leaves the first one torn; a pending write before a durable slot becomes torn. `Corrupt` turns a durable slot faulty: a bit flip, or a lost write detected by the redundant header. The header of an unreadable slot stays known, except at the head after a restart, which makes the head uncertain and the replica `recovering_head`. An undetected lost write (both copies of the header lost) is outside the fault model.
- **Repair** is atomic: a replica takes an intact copy of the prepare its header names. A crash during the repair write leaves the slot as it was.
- **Nacks.** The new primary asks for nacks on each unreadable slot of the log it installed; a replica that has left its old view answers, and the new primary counts its own from the log it had before. Nacks count only within the view they were sent for.
- **Commit** is per op and in order on the primary, from Qr prepare_ok of its view, its own durable write included. A new primary recommits the kept suffix in its view (the order of VR Revisited, open point O3 of document 02a).
- **request_start_view** is not a message: a primary resends start_view, with its current log, whenever some replica is behind in or before its view.
- **Sends at the first step they may.** prepare_ok, nack_prepare, start_view and commits are not separate steps: after every step, each one that may be sent is (`Next`). Each only adds to a set, and only enables an optional receiving step, so this reaches every state the protocol reaches and makes the message and history sets as large, and commit views as early, as they can be; the invariants over them are checked in their strongest form. Without this, the 3-replica run at 3 ops and 3 views passed 53 million distinct states at depth 20 with the queue still doubling.
- **Eager votes (`EagerVotes = TRUE`: Vsr3Views, Vsr5, Vsr5Crash, Vsr6).** With it, every replica that may vote does so at once. A vote only enables an optional `JoinView`, so invariants 1, 2 and 6 are checked as fully; `QuorumGatedSuspicion` is not meaningful then and is checked in `Vsr3` and the defects, where votes are separate steps.
- **Execution, client sessions, the commit heartbeat and checkpoints** are not modelled; backups never learn the commit number.

## Actions and the functions that implement them

The replica does not exist yet; the names are the planned ones in `crates/vsr`. The task that writes or changes a function updates its row.

| Action | Document 02 | Function | Task | Status |
|---|---|---|---|---|
| `ClientRequest` | 3, steps 1 and 2 | `Replica::on_request` | T2.2.2 | planned |
| `ReceivePrepare` | 3, step 3 | `Replica::on_prepare` | T2.2.2 | planned |
| `WriteDurable` | 3, steps 2 and 3 | `Replica::on_write_prepare` (WAL write completion) | T2.2.2 | planned |
| `Ack` | 3, step 3 | `Replica::send_prepare_ok` | T2.2.2 | planned |
| `Commit` | 3, step 4 | `Replica::on_prepare_ok`, `Replica::commit_pipeline` | T2.2.2 | planned |
| `Vote` | 4, step 1 | `Replica::on_suspect_timeout`, `Replica::send_start_view_change` | T2.2.3 | planned |
| `JoinView` | 4, step 2 | `Replica::on_start_view_change`, `Replica::on_do_view_change` (a higher view), `Replica::transition_to_view_change` | T2.2.3 | planned |
| `InstallLog` | 4, step 3 | `Replica::on_do_view_change` (the new primary, on a quorum) | T2.2.3 | planned |
| `Nack` | 2 (nack_prepare), 4, step 4 | `Replica::send_nack_prepare` | T2.2.3, T2.2.4 | planned |
| `Truncate` | 4, step 4 | `Replica::on_nack_prepare` | T2.2.3 | planned |
| `StartView` | 4, step 5 | `Replica::send_start_view` | T2.2.3 | planned |
| `ResendStartView` | 2 (request_start_view) | `Replica::on_request_start_view` | T2.2.3 | planned |
| `ReceiveStartView` | 4, step 5; 5 (recovering_head) | `Replica::on_start_view` | T2.2.3, T2.2.5 | planned |
| `Repair` | 5 (WAL repair) | `Replica::on_request_prepare`, `Replica::on_repair_prepare` | T2.2.4 | planned |
| `Crash` | 5 (recovery) | `Replica::open` (WAL classification, status on restart); the crash itself is a simulator fault | T2.2.5 | planned |
| `Corrupt` | 5; document 06 section 1 (storage faults) | a simulator storage fault; detected by WAL slot classification in `Journal` | T2.2.4, T2.2.5 | planned |

## Injected defects

Each replaces one definition of `Vsr.tla` from its `.cfg`.

| Defect | Replaces | Expected failure |
|---|---|---|
| [QuorumsDisjoint](defects/QuorumsDisjoint.tla) | Qr = 1, Qv = 2 for 3 replicas (Qr + Qv = N); the quorum assumption is switched off | a view change misses a committed op |
| [FewNacks](defects/FewNacks.tla) | the nack quorum N - Qr + 1 by N - Qr | a committed op whose copy on the new primary is corrupt is truncated |
| [AckBeforeDurable](defects/AckBeforeDurable.tla) | a backup acknowledges a pending write | invariant 6 fails after a crash |
| [VoteAlone](defects/VoteAlone.tla) | one vote moves a replica into a view change | `QuorumGatedSuspicion` fails, so the property is not vacuous |
