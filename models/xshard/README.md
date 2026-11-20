# Cross-shard protocol model

A TLA+ specification of the cross-shard transaction protocol of [document 04 section 4](../../docs/04-cluster-and-cross-shard.md#4-cross-shard-transactions), checked with TLC (task T5.1.2). It covers independent transactions (propose, vote, finalize, execute), coordinated transactions (prepare with locks, vote, finalize), timestamp-order execution, the conflict-aware bypass, the conflict vote and vote retransmission after a view change. Tablet moves and the standby site are not modelled.

## Files

| File | Content |
|---|---|
| `XShard.tla` | The protocol, the invariants, the deadlock property and the witnesses |
| `Scenario.tla` | The transactions, keys and groups that are checked |
| `Check.tla`, `Check.cfg` | Transactions 1, 2, 3, bypass on. Must finish with no violation |
| `CheckLocks.tla`, `CheckLocks.cfg` | Transactions 1, 2, 4: two coordinated transactions crossing on two groups. Must finish with no violation |
| `CheckStrict.tla`, `CheckStrict.cfg` | As `Check`, in strict timestamp order. Must finish with no violation |
| `defects/*.tla`, `defects/*.cfg` | Injected defects. Each must produce a counterexample |
| `witnesses/*.tla`, `witnesses/*.cfg` | Reachability checks. Each must produce a counterexample, which shows that the case is reached and the passing runs are not vacuous |

`just model xshard` runs the runs of this directory and of `defects/`, each next to copies of this directory's modules, which is how the defect modules find `Scenario`. It does not run `witnesses/`; their command is under Results.

## What is modelled

- **A group is one replicated log.** Its primary appends ops; `Commit` advances the commit index; `ViewChange` replaces the primary, which keeps only the committed prefix: the uncommitted suffix and every vote the old primary had received are lost. At most `MaxViewChanges` per group. A view change that would lose nothing is not taken, since it changes nothing.
- **Decisions are logged with the op.** The primary takes each timestamp, vote and lock from its whole log, including uncommitted ops, and writes them into the op (document 04a rule 33). A new primary decides again from the committed prefix.
- **Votes are derived from the committed prop op** (D2). `DeliverVote` may happen any number of times until the receiver has the vote, which covers loss, duplicates and the re-send after a view change of either group.
- **Timestamps.** A proposal is one above the highest timestamp in the group's log, which includes every proposal and every final timestamp it has logged (the clock floor). Clients send no timestamp (`client_ts` is 0): causal order is T5.2.5's.
- **Execution is deterministic on committed state** (D1, O8). After each committed op the group executes every ready transaction in `(timestamp, id)` order. An independent transaction is ready when nothing that could still be ordered before it is unexecuted (strict order), or nothing such that shares a key with it (bypass). A pending transaction counts at its proposed timestamp, a finalized one at its final timestamp. A coordinated commit executes when its finalize commits, holding its locks until then (rule 35).
- **Coordinated prepare** votes `conflict` if one of its keys is locked or belongs to a transaction not executed yet, pending or queued; otherwise it evaluates its `require` against the executed state and votes `commit` (taking the locks) or `abort`. An independent transaction whose key is locked waits, and takes its timestamp when the lock is released.
- **Data.** A key's value is the sequence of transactions that wrote it, so every read and every final value is determined by the per-key order. A coordinated transaction `t` requires each of its keys to have fewer than `Cap[t]` earlier writes.

Not modelled: refusal votes (`busy`, `unavailable`, `too_old`, the timestamp guard), the finished-transaction table, client retries after a conflict, the clock itself and the 100 ms guard, snapshot reads, batching, tablet moves and the standby site.

## Properties

| Name | Statement |
|---|---|
| `Atomic` | Invariant 1: no transaction is finalized as committed on one participant and as aborted on another. With `NoDeadlock` every behaviour ends with every transaction finalized on all participants. |
| `SameTs` | Invariant 2: every participant finalizes, and so executes, a transaction at the same timestamp. |
| `ShardOrder` | Invariant 3: on every shard, any two executed transactions that share a key executed in `(final timestamp, id)` order, so the history is equivalent to timestamp-order execution. |
| `Serializable` | Invariant 4: on every key, the executed writes are exactly the globally committed transactions in global `(timestamp, id)` order, up to the latest executed one; the global timestamp is the maximum committed proposal. So every read and every final value equals serial execution in timestamp order. Each committed coordinated `require` holds at its place in that order. |
| `NoDeadlock` | No reachable state has unfinished transactions and no step other than a view change. TLC's deadlock check (`CHECK_DEADLOCK TRUE`) reports the same states once view changes are used up. |
| `TypeOK` | Commit indexes stay within the log; transactions execute only on their participants. |

Bypass and strict order: the bypass run checks `ShardOrder` and `Serializable`, which say that results equal strict timestamp-order execution, and `witnesses/NeverBypass` shows that the bypass is taken in the scenario.

## Scenario

Three groups; keys `a` and `d` on group 1, `b` on group 2, `c` on group 3; one view change per group.

| Transaction | Class | Keys | Require |
|---|---|---|---|
| 1 | independent | a, b | |
| 2 | coordinated | b, c | b and c never written |
| 3 | independent | c, d | |
| 4 | coordinated | b, c | fewer than 2 writes on each |

Each run checks three of them, so that it takes seconds. All four at once was tried once and stopped unfinished after 20 minutes, at 61.2 million distinct states and depth 35 with 9.5 million states still queued; so transactions 3 and 4 are never checked together.

- `Check` (1, 2, 3): transactions 1 and 3 have disjoint keys on group 1, so 3 can bypass 1; both share a key with 2, so they wait for its lock, and 2 votes conflict when one of them is unexecuted.
- `CheckLocks` (1, 2, 4): transactions 2 and 4 cross on groups 2 and 3 and 1 waits for their lock on b, so without the conflict vote they deadlock (`defects/PrepareWaits`).
- `defects/BypassPending` gives transaction 3 the keys `c, a`, so that two independent transactions share a key. `defects/BypassQueued` uses three independent transactions, `{a, b}`, `{a, d, c}` and `{d, c}`: on group 1 the second can queue behind the first, and the third shares `d` with the second only.

## Results

Recorded on 2026-11-20 with `just model xshard` (exit 0) on an Apple-silicon Mac with 14 cores, TLC 2.19 from the pinned `tla2tools.jar` v1.7.4, Java 21, while another TLC run shared the machine. Every run has 3 groups and one view change per group (`MaxViewChanges = 1`).

| Run | Transactions | Bypass | Result | Distinct states | Generated | Depth or trace | Time |
|---|---|---|---|---|---|---|---|
| `Check` | 1, 2, 3 | on | no violation | 504,520 | 2,291,377 | depth 37 | 44 s |
| `CheckLocks` | 1, 2, 4 | on | no violation | 1,086,544 | 4,801,109 | depth 37 | 97 s |
| `CheckStrict` | 1, 2, 3 | off | no violation | 504,520 | 2,291,377 | depth 37 | 45 s |

`Check` and `CheckStrict` have the same state counts because no decision in that scenario reads group 1's execution order, the only thing the bypass changes there; `witnesses/NeverBypass` holds in strict order and fails with the bypass.

Injected defects, each found by `just model xshard`:

| Defect | Card gate | Transactions | Counterexample | Trace | Distinct states | Time |
|---|---|---|---|---|---|---|
| `FinalizeBelowVote`: final timestamp is the minimum vote | finalizing at a timestamp below a vote | 1, 2, 3 | `ShardOrder` (invariant 3) | 15 states | 31,443 | 3 s |
| `FinalizeEarly`: finalize, and so execute, before all votes are in | executing before all votes are in | 1, 2, 3 | `Serializable` (invariant 4) | 5 states | 414 | 1 s |
| `PrepareWaits`: a coordinated prepare waits instead of voting conflict | a prepare that waits for a lock | 1, 2, 4 | `NoDeadlock` | 7 states | 420 | 1 s |
| `BypassPending`: the bypass ignores intersecting pending transactions | bypass of a transaction whose keys intersect a pending one | 1, 2, 3 (3 = `{c, a}`) | `Serializable` (invariant 4) | 12 states | 8,517 | 2 s |
| `BypassQueued`: the bypass ignores intersecting queued transactions | document 04a finding F3 | three independent | `Serializable` (invariant 4) | 17 states | 106,537 | 6 s |
| `ConflictLocksOnly`: a prepare checks locks only | document 04a finding F4 | 1, 2, 3 | `Serializable` (invariant 4) | 12 states | 5,552 | 2 s |

In the `PrepareWaits` trace transaction 2 holds the lock on b, 4 the lock on c, each prepare waits for the other's key, and transaction 1 waits for b: no step is left but view changes.

Witnesses, run with the bounds of `Check` (`NeverConflict` with those of `CheckLocks`). `just model` does not run them; the command is the runner's, with the witness in place of a defect:

```bash
java -XX:+UseParallelGC -cp target/tla/tla2tools.jar tlc2.TLC -workers auto -metadir <dir>/states -config <dir>/spec/NeverBypass.cfg <dir>/spec/NeverBypass.tla
```

where `<dir>/spec/` holds copies of `models/xshard/*.tla` and of the witness's `.tla` and `.cfg`.

| Witness | Shows | Trace | Distinct states | Time |
|---|---|---|---|---|
| `NeverBypass` | an independent transaction executes ahead of an earlier pending one on another key | 10 states | 5,356 | 1 s |
| `NeverConflict` | a coordinated prepare votes conflict | 3 states | 37 | 1 s |
| `NeverAbort` | a coordinated prepare votes abort on its `require` | 9 states | 3,452 | 2 s |
| `NeverLockWait` | an independent transaction waits for a lock | 3 states | 69 | 1 s |

## Action map

Every action of `XShard.tla` and the task that implements it. Function names are the planned names in `crates/xshard`, which is empty today; the implementing task replaces them with the real ones and updates this table (T5.2.2 gate).

| Action | Models | Task | Function |
|---|---|---|---|
| `Propose`, independent | `propose` op: timestamp above the floor, record as pending | T5.2.2 | `Participant::propose` |
| `Propose`, independent, key locked | wait for the lock, take the timestamp at release | T5.2.4 | `Participant::propose` (lock check), `ShardQueue::wait_for_lock` |
| `Propose`, coordinated | `prepare` op: conflict check against locks and unexecuted key sets, `require`, locks, vote | T5.2.4 | `Participant::prepare`, `LockTable::try_lock` |
| `Floor` (inside `Propose` and `Finalize`) | clock floor raised by every logged timestamp | T5.2.5 | `HybridClock::raise_floor` |
| `DeliverVote` | vote sent when the prop op commits; re-sent by a new primary; re-requested when missing | T5.2.2 (send), T5.2.3 (re-send, re-request, forwarding), T5.2.9 (batching) | `Participant::send_votes`, `Participant::on_vote`, `Participant::on_view_change` |
| `Finalize` | `finalize` op from the full vote set: maximum vote, commit only if all voted commit | T5.2.2, T5.2.4 | `Participant::finalize` |
| `Commit` | an op commits and is applied | T2.2.2 (VSR commit), T2.3.1 (state machine) | `vsr` commit path, `xshard::StateMachine::apply` |
| `Drain`, `Ready`, `Blocks` (inside `Commit`) | timestamp-order queue release; bypass; coordinated execute and lock release at finalize | T5.2.1 (queue), T5.2.2 (strict order), T5.4.3 (bypass), T5.2.4 (coordinated) | `ShardQueue::release`, `KeySetIndex::intersects` |
| `ViewChange` | new primary keeps the committed prefix; volatile votes lost | T2.2.3 (VSR view change), T5.2.3 (re-send) | `vsr` view change, `Participant::on_view_change` |
| `Before` | order by `(timestamp, transaction id)` | T5.2.1 | ordering key of `PendingTable` and `ShardQueue` |
