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
- **Superblock headers.** `vhead` is the headers of the log a replica installed with its log_view (view change, start_view, truncation). After a crash, a slot they name but the WAL lost is torn (unreadable, header known), not gone, and does not make the head uncertain. This is the correction the model forced into document 02 (02a, C9); the defect `LogViewWithoutHeaders` shows the loss of a committed op without it.
- **Repair** is atomic: a replica takes an intact copy of the prepare its header names. A crash during the repair write leaves the slot as it was.
- **Nacks.** The new primary asks for nacks on each unreadable slot of the log it installed; a replica that has left its old view answers, and the new primary counts its own from the log it had before. Nacks count only within the view they were sent for.
- **Commit** is per op and in order on the primary, from Qr prepare_ok of its view, its own durable write included. A new primary recommits the kept suffix in its view (the order of VR Revisited, open point O3 of document 02a).
- **request_start_view** is not a message: a primary resends start_view, with its current log, whenever some replica is behind in or before its view.
- **Sends at the first step they may.** prepare_ok, nack_prepare, start_view and commits are not separate steps: after every step, each one that may be sent is (`Next`). Each only adds to a set, and only enables an optional receiving step, so this reaches every state the protocol reaches and makes the message and history sets as large, and commit views as early, as they can be; the invariants over them are checked in their strongest form. Without this, the 3-replica run at 3 ops and 3 views passed 53 million distinct states at depth 20 with the queue still doubling.
- **Eager votes (`EagerVotes = TRUE`: Vsr3Views, Vsr5, Vsr5Crash, Vsr6).** With it, every replica that may vote does so at once. A vote only enables an optional `JoinView`, so invariants 1, 2 and 6 are checked as fully; `QuorumGatedSuspicion` is not meaningful then and is checked in `Vsr3` and the defects, where votes are separate steps.
- **Execution, client sessions, the commit heartbeat and checkpoints** are not modelled; backups never learn the commit number.

## Results

`just model vsr` on 2026-11-20, with the specification and runs of commit 9181c7d, on a 14-core Apple Silicon laptop (macOS, Java 21, TLC 2.19 from tla2tools.jar v1.7.4), 25 min 34 s in all. Every run is exhaustive (breadth-first, no states left on the queue). Views are numbered from 0, so `MaxView = 2` is three views. Every run has message loss, duplication and reordering.

| Run | Replicas (Qr, Qv, nack) | Ops | Views | Crash | Corrupt | Votes | Distinct states | Generated | Depth | Time | Result |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `Vsr3` | 3 (2, 2, 2) | 2 | 0–1 | 1 | 1 | steps | 19,401,836 | 87,266,181 | 35 | 257 s | no violation, `QuorumGatedSuspicion` holds |
| `Vsr3Views` | 3 (2, 2, 2) | 1 | 0–2 | 1 | 1 | eager | 6,960,677 | 28,104,547 | 34 | 78 s | no violation |
| `Vsr5` | 5 (3, 3, 3) | 1 | 0–1 | 0 | 1 | eager | 7,291,514 | 46,733,256 | 31 | 133 s | no violation |
| `Vsr5Crash` | 5 (3, 3, 3) | 1 | 0–1 | 1 | 0 | eager | 12,425,421 | 83,955,840 | 31 | 257 s | no violation |
| `Vsr6` | 6 (3, 4, 4) | 1 | 0–1 | 0 | 0 | eager | 27,234,957 | 195,427,887 | 34 | 800 s | no violation |

| Defect | Bounds (N, ops, views, crash, corrupt) | Counterexample |
|---|---|---|
| `AckBeforeDurable` | 3, 1, 0–1, 1, 0 | `AckedIsDurable` violated, 5 states |
| `FewNacks` | 3, 1, 0–1, 0, 1 | `CommittedNotLost` violated, 12 states |
| `LogViewWithoutHeaders` | 3, 1, 0–2, 1, 0 | `CommittedNotLost` violated, 18 states |
| `QuorumsDisjoint` | 3 (Qr 1, Qv 2), 1, 0–1, 0, 0 | `CommittedNotLost` violated, 9 states |
| `VoteAlone` | 3, 0, 0–1, 0, 0 | `QuorumGatedSuspicion` violated, 3 states |

### The card's bounds: 3 ops and 3 views (gate open)

T2.1.2 asks that invariants 1, 2 and 6 hold for 3 replicas with at least 3 ops, 3 views, one crash and one corrupted entry. That gate is open: no exhaustive run at those bounds has been made. What is shown instead:

- Exhaustive at split bounds, each with one crash and one corruption: `Vsr3` (2 ops, views 0–1, 19,401,836 distinct states) and `Vsr3Views` (1 op, views 0–2, 6,960,677 distinct states), in the table above.
- A random simulation at the full bounds, with votes as steps and every invariant and `QuorumGatedSuspicion` checked, found no violation. Recorded command:

  ```bash
  d=target/tla/sim; mkdir -p $d; cp models/vsr/Vsr.tla models/vsr/Vsr3.tla $d/
  sed -e 's/MaxOps = .*/MaxOps = 3/' -e 's/MaxView = .*/MaxView = 2/' models/vsr/Vsr3.cfg > $d/Vsr3.cfg
  java -XX:+UseParallelGC -cp target/tla/tla2tools.jar tlc2.TLC -simulate num=100000 -depth 100 -seed 1 -workers auto -metadir $d/st -config $d/Vsr3.cfg $d/Vsr3.tla
  ```

  Result (2026-11-20, same machine): 100,000 behaviours of up to 100 steps, at least 251,562,227 states checked (TLC's last progress line), 10 min 55 s, exit 0. A simulation samples behaviours; it proves nothing at these bounds and does not say how many behaviours had a crash, a corruption or three views.
- Measured growth, 3 replicas with one crash and one corruption: one more op multiplies the distinct states by about 230 (views 0–1, votes as steps: 83,635 with 1 op, 19,401,836 with 2), and a third view by about 400 with votes as steps (1 op: 83,635 to about 35 million) or about 120 with `EagerVotes` (1 op: 57,775 to 6,960,677).

Estimate for an exhaustive run at the full bounds, with `EagerVotes = TRUE` (enough for invariants 1, 2 and 6; `QuorumGatedSuspicion` is covered by `Vsr3` and `VoteAlone`): 2 ops with views 0–1 is about 15.6 million distinct states, so 2 ops with views 0–2 is about 2 × 10^9 and 3 ops with views 0–2 about 4 × 10^11. TLC used about 110 bytes of disk per distinct state here (the 27-million-state `Vsr6` run peaked near 3 GB) and found about 2 million distinct states a minute on 14 cores. That gives:

| Bounds | Distinct states | Disk | Time, 14 cores |
|---|---|---|---|
| 2 ops, views 0–2 | about 2 × 10^9 | about 200 GB | about 16 hours |
| 3 ops, views 0–2 (the gate) | about 4 × 10^11 | about 40 TB | about 3,500 hours |

The growth factors are measured at small bounds and may fall as the bounds fill, so these are upper-end guesses within an order of magnitude. The first row fits a large machine or a long CI job. The second needs a smaller state space first: symmetry over the backups (TLC 2.19 accepts symmetry only over model values, so replicas would have to become model values), or a checker that does not enumerate states.

## What the model found

With log_view made durable alone, as document 02 said, a committed op is lost. Op 1 commits in view 0 on replicas 0 and 2, while replica 1 holds it with the write in flight. Replica 1, primary of view 1, installs a log with op 1 and makes log_view 1 durable, then crashes before the write completes. In view 2 its do_view_change carries log_view 1 and an empty log, which is the highest (log_view, op), so the new primary starts without op 1. The defect `LogViewWithoutHeaders` reproduces it in 18 states. The fix, now in document 02 (section 1, section 4 step 3, section 5) and recorded in 02a as C9: log_view is durable together with the headers of the log it names. The model also needs a rule document 02 lacked, now added as C10: a restarted primary does not resume its view.

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
| [LogViewWithoutHeaders](defects/LogViewWithoutHeaders.tla) | log_view is made durable without the headers of the log it names | a crashed new primary's shorter log wins the next view change and a committed op is lost (found by this model; document 02 corrected) |
| [VoteAlone](defects/VoteAlone.tla) | one vote moves a replica into a view change | `QuorumGatedSuspicion` fails, so the property is not vacuous |
