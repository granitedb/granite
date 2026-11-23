# Reconfiguration model

A TLA+ specification of a change of one replication group's members, [document 02 section 9](../../docs/02-replication.md#9-standbys-and-reconfiguration), on the VSR model of T2.1.2 ([models/vsr](../vsr/README.md)). It is roadmap task T7.1.1. It covers the logged `reconfigure` op, the switch to the new members at the next checkpoint boundary, state sync of new members before they count toward quorums, view changes before, at and after the boundary in either configuration, crashes, and a removed replica that keeps running.

`just model reconfigure` checks it. The rules of this folder are in [models/README.md](../README.md).

## Files

| File | What it is |
|---|---|
| [Reconfigure.tla](Reconfigure.tla) | The specification, its properties and the witness predicates |
| `ReplaceOld`, `ReplaceNew`, `GrowOld`, `GrowNew`, `ShrinkNew`, `ReplaceOldCrash`, `ReplaceNewCrash` (`.tla` and `.cfg`) | The runs `just model reconfigure` checks; each `.tla` names the two member lists, each `.cfg` the bounds |
| [large/ShrinkOld](large) | Shrinking with a view change in the old configuration: too large to check here, so `just model` does not run it (see Results) |
| [defects/](defects) | The three injected defects of the card, each of which must produce a counterexample |

## The protocol as modelled

- **Two configurations.** `Old` and `New` are member lists in primary order. Op `RecOp` is the reconfigure op naming `New`: every primary that prepares that op number prepares it. The change takes effect at `Boundary`, the next checkpoint boundary after it (`RecOp = 1`, a checkpoint every 2 ops, so `Boundary = 2`). Every replica computes `Boundary` from `RecOp` alone (`EffectOp`).
- **Epochs.** The old configuration is epoch 0 and the new one epoch 1. Views are ordered by (epoch, view). Since the views of each epoch are bounded, the pair is one integer here: views `0 .. MaxView0` are epoch 0 and view `w` of epoch 1 is `MaxView0 + 1 + w`. The primary of a view is member `w mod n` of its epoch's list, and its quorums are those of document 02 section 1 for `n` members. Every message carries its view, so its epoch, and a replica takes messages of its own epoch only.
- **Old configuration.** It runs the VSR model unchanged, except that a primary of epoch 0 never prepares an op after `Boundary`. Its view changes go on before and after the boundary, for as long as its members stay in epoch 0.
- **Entering the new configuration** (`EnterEpoch`). A member of `New` still in epoch 0 that knows op `Boundary` is committed (from the commit messages of epoch 0) installs the committed checkpoint at `Boundary` and enters view 0 of epoch 1, in status normal. A new member gets the checkpoint by state sync, a member of both configurations from its own log or by repair; the model does both in one atomic step, written with the superblock, so a crash leaves it either before or after. The first primary of epoch 1 starts view 0 from the checkpoint without a view change, since every op up to `Boundary` is committed.
- **New configuration.** It runs the VSR model with its own members and quorums, and commits only ops after `Boundary`.
- **Removed replicas** keep running in epoch 0 in every behaviour: they vote, join and lead view changes of epoch 0, acknowledge, and crash.
- **Modelling choices of [models/vsr](../vsr/README.md#modelling-choices)** carry over: messages are sets never emptied (loss, duplication, reordering); a prepare is identified by (op, view), and the view here includes the epoch; sends happen at the first step they may (votes eagerly, `EagerVotes = TRUE` there); `request_start_view` is a resent `start_view`, sent to members of the primary's own epoch.

Not modelled: slot corruption (it is the VSR model's and is independent of the configuration); more than one reconfiguration; a reconfigure op that a view change drops and replaces by an ordinary op (then there is no change, which is the VSR model); the state sync protocol itself (a new member takes the committed checkpoint at once; document 02 section 5 says how it learns that the checkpoint is durable); client sessions, execution and checkpoint contents; liveness, including whether a group whose old members never learn the boundary is stuck; shutting the removed replica down.

## Properties

| Property | Statement | Card gate |
|---|---|---|
| `OneCommitPerOp` | Invariant 1 of document 02: at most one prepare is committed at an op number, across both configurations | gates 2, 3 |
| `CommittedNotLost` | Invariant 2: a prepare committed in view cv is in every log of view cv or later, of either epoch, and no primary of such a view starts without it | gates 2, 3 |
| `OneConfigurationPerOp` | The two configurations never both commit at an op number. A quorum of the old configuration and one of the new may be disjoint (for replacement, `{1, 2}` and `{3, 0}`); this says they never both commit, whatever the op | gate 5 |
| `Handover` | Action property: once the new configuration has committed an op, the old one commits no prepare it had not committed, so the two never commit side by side | gate 5 |
| `RemovedCannotDisturb` | A removed replica never enters the new configuration, no message of the new configuration comes from it, and the old configuration commits nothing after `Boundary`, while removed replicas keep running | gate 6 |
| `CheckpointHeld` | Every replica of epoch 1 holds the committed checkpoint at `Boundary` | |
| `AckedIsDurable` | Invariant 6, as in the VSR model | |
| `TypeOK` | Types and bounds | |

The defect runs check only `OneCommitPerOp`, `CommittedNotLost`, `OneConfigurationPerOp` and `RemovedCannotDisturb` (`OldPastBoundary` only the first and third, see below), so that each counterexample is to an invariant of the card and not to a restatement of the defect.

## Results

`just model reconfigure`'s runs, the defects and the witnesses were run on 2026-11-23 and 24 as a launchd job on an internal-disk copy of the tree, with the specification of the commit that adds this folder, on a 14-core Apple Silicon laptop (macOS, Java 21, TLC 2.19 from tla2tools.jar v1.7.4, 14 workers). The machine was shared with other heavy jobs: the load average (1 minute) at the start and end of each run is given with its time, so the times are not typical. Every run is exhaustive (breadth-first, no states left on the queue), with message loss, duplication and reordering, `RecOp = 1`, a checkpoint every 2 ops (`Boundary = 2`), and 3 ops, so one op after the boundary. Views are per epoch: `0–1` is two views, `0` one.

| Run | Old → New | Views old, new | Crash | Distinct states | Generated | Depth | Time | Load start, end | Result |
|---|---|---|---|---|---|---|---|---|---|
| `ReplaceOld` | 0,1,2 → 3,0,1 | 0–1, 0 | 0 | 1,042,608 | 3,977,521 | 37 | 181 s | 25, 33 | no violation |
| `ReplaceNew` | 0,1,2 → 3,0,1 | 0, 0–1 | 0 | 31,017 | 115,729 | 33 | 13 s | 33, 33 | no violation |
| `GrowOld` | 0,1,2 → 3,0,1,2,4 | 0–1, 0 | 0 | 7,177,428 | 33,455,231 | 43 | 833 s | 98, 84 | no violation |
| `GrowNew` | 0,1,2 → 3,0,1,2,4 | 0, 0–1 | 0 | 14,134,118 | 80,093,241 | 46 | 1,917 s | 84, 51 | no violation |
| `ShrinkNew` | 0,1,2,3,4 → 2,0,1 | 0, 0–1 | 0 | 631,013 | 2,920,970 | 41 | 21 s | 54, 57 | no violation |
| `ReplaceOldCrash` | 0,1,2 → 3,0,1 | 0–1, 0 | 1 | 11,073,361 | 49,715,414 | 40 | 1,510 s | 88, 92 | no violation |
| `ReplaceNewCrash` | 0,1,2 → 3,0,1 | 0, 0–1 | 1 | 426,358 | 1,904,591 | 36 | 32 s | 92, 98 | no violation |

`just model reconfigure` itself, from that tree, also as a launchd job: exit 0 in 41 min 38 s (2026-11-24 02:33 to 03:15, load average 85 at the start and 50 at the end), every run with the state counts above and every defect with the counterexample below.

Every run checks `TypeOK`, `OneCommitPerOp`, `CommittedNotLost`, `AckedIsDurable`, `OneConfigurationPerOp`, `RemovedCannotDisturb`, `CheckpointHeld` and the action property `Handover`. In each, the first view of the new configuration is led by a replica that did not lead the old configuration's first view; for replacement and growth it is a new member, so the epoch's first primary got its state by state sync.

A view change is either in the old configuration (`*Old`: before the reconfigure op commits, between its commit and the boundary, or at the boundary while the new configuration starts) or in the new one (`*New`: at its start, or after it commits op 3). One view change per behaviour, at any point, keeps each run exhaustive; the witnesses below show that each position is reached.

### Shrinking with a view change in the old configuration (gate open)

`large/ShrinkOld` (5 replicas in the old configuration, views 0–1 there, ops up to the boundary, the smallest bounds the gate allows) is not checked. Measured, under load averages of 22 to 58:

- with ops up to the boundary (2): more than 23,142,019 distinct states at depth 21 after 16 minutes, with 12,759,287 still on the queue, the queue more than half of the states found and growing about 1.5 times per depth level;
- with 3 ops, as the other `*Old` runs: 6,892,628 distinct states at depth 18, with 3,929,637 on the queue.

The other runs finish at depth 37 to 46. With the queue still growing at depth 21, the 2-op run is likely beyond 10^9 distinct states: about 100 GB of disk at the VSR model's 110 bytes per state, and well over a day on this machine. The same explosion bounded the VSR model's 5-replica runs to 1 op (models/vsr, `Vsr5`). What would shrink it: symmetry over the removed replicas 3 and 4, which never lead a view within these bounds (TLC 2.19 needs replicas as model values for that, and it gives at most a factor of 2), or a checker that does not enumerate states. The view change in the new configuration after shrinking (`ShrinkNew`) and in the old configuration with 3 members (`ReplaceOld`, `GrowOld`) are checked.

Supporting evidence, which proves nothing at these bounds: a random simulation of `large/ShrinkOld` with 3 ops, every invariant and `Handover` checked:

```bash
d=target/tla/sim; mkdir -p $d; cp models/reconfigure/*.tla models/reconfigure/large/ShrinkOld.tla $d/
sed 's/MaxOps = 2/MaxOps = 3/' models/reconfigure/large/ShrinkOld.cfg > $d/ShrinkOld.cfg
java -XX:+UseParallelGC -cp target/tla/tla2tools.jar tlc2.TLC -simulate num=100000 -depth 100 -seed 1 -workers auto -metadir $d/st -config $d/ShrinkOld.cfg $d/ShrinkOld.tla
```

Result (2026-11-24, same machine, load average 54 to 105): stopped by hand after 51 minutes, before reaching 100,000 behaviours, with 306,720,791 states checked (TLC's last progress line) and no violation. A simulation samples behaviours; it proves nothing at these bounds and does not say how many behaviours had a view change at each position.

## Witnesses

An exhaustive run can pass because the case it is meant to check is never reached. `Reconfigure.tla` defines one predicate per position of a view change that gate 2 names, as an invariant `Never...` that must fail in the run it is checked with:

| Witness | Position | Runs |
|---|---|---|
| `NeverBeforeCommit` | a replica of the old configuration in a view change while the reconfigure op is prepared and not committed | `*Old` |
| `NeverBeforeBoundary` | the same, with the reconfigure op committed and op `Boundary` not | `*Old` |
| `NeverAtBoundaryOld` | the same, with op `Boundary` committed and the new configuration started | `*Old` |
| `NeverAtBoundaryNew` | a replica of the new configuration in a view change before it has prepared any op | `*New` |
| `NeverAfterBoundary` | the same, after the new configuration has committed op `Boundary + 1` | `*New` |

`just model` does not run them, since each must fail. Command, from the repository root:

```bash
d=target/tla/reconfigure-witness
for run in ReplaceOld GrowOld ReplaceNew GrowNew ShrinkNew; do
  case $run in
    *Old) ws="NeverBeforeCommit NeverBeforeBoundary NeverAtBoundaryOld" ;;
    *) ws="NeverAtBoundaryNew NeverAfterBoundary" ;;
  esac
  for w in $ws; do
    rm -rf $d && mkdir -p $d && cp models/reconfigure/*.tla $d/
    sed "s/^INVARIANTS .*/INVARIANT $w/; /^PROPERTY/d" models/reconfigure/$run.cfg > $d/$run.cfg
    java -XX:+UseParallelGC -cp target/tla/tla2tools.jar tlc2.TLC -workers auto -metadir $d/states \
      -config $d/$run.cfg $d/$run.tla > $d.log 2>&1
    echo "$run $w: TLC exit $?, trace of $(grep -cE '^State [0-9]+:' $d.log) states"
  done
done
```

Run it with bash: zsh does not split `$ws`. Result on 2026-11-24, same machine: every witness fails (TLC exit 12, an invariant violated), so every position is reached.

| Run | Witnesses | Traces (states) |
|---|---|---|
| `ReplaceOld` | `NeverBeforeCommit`, `NeverBeforeBoundary`, `NeverAtBoundaryOld` | 3, 6, 11 |
| `GrowOld` | `NeverBeforeCommit`, `NeverBeforeBoundary`, `NeverAtBoundaryOld` | 3, 6, 11 |
| `ReplaceNew` | `NeverAtBoundaryNew`, `NeverAfterBoundary` | 12, 17 |
| `GrowNew` | `NeverAtBoundaryNew`, `NeverAfterBoundary` | 13, 20 |
| `ShrinkNew` | `NeverAtBoundaryNew`, `NeverAfterBoundary` | 16, 21 |

`RemovedCannotDisturb` and `Handover` are not vacuous either. With the defect `OldPastBoundary`, checking only `RemovedCannotDisturb` gives a counterexample of 13 states (the old configuration commits op 3), and checking only `Handover` one of 19 states (TLC exit 13).

## Injected defects

Each replaces one definition of `Reconfigure.tla` from its `.cfg`. Bounds: replacement of 2 by 3, views 0 of each epoch only, no crash.

| Defect | Card gate | Replaces | Counterexample | Distinct states |
|---|---|---|---|---|
| [UnsyncedMember](defects/UnsyncedMember.tla) | the new member voting before it has reached the checkpoint | `EntryLog`: a new member enters the new configuration with its own, empty, log instead of the checkpoint | `CommittedNotLost`, 10 states: replica 3 enters view 0 of the new epoch, which it leads, without ops 1 and 2, both committed | 210 |
| [OldPastBoundary](defects/OldPastBoundary.tla) | old and new configurations both able to commit the same op number | `OldMayPrepare`: the old configuration keeps preparing past the boundary | `OneCommitPerOp`, 19 states: the old configuration commits op 3 in its view 0 with replicas 0 and 1, and the new one commits a different op 3 with replicas 3 and 0 | 5,198 |
| [EffectDiffers](defects/EffectDiffers.tla) | the change taking effect at different ops on different replicas | `EffectOp`: a new member takes the change from the op after the reconfigure op, the old members from the boundary | `CommittedNotLost`, 10 states: replica 3 enters the new epoch, which it leads, once op 1 commits; the old configuration then commits op 2, which 3 never holds | 294 |

The distinct-state counts of a defect run vary by a few per cent between runs, since TLC's 14 workers stop at the first counterexample found.

`OldPastBoundary` checks only `OneCommitPerOp` and `OneConfigurationPerOp`: `RemovedCannotDisturb`, which also says that the old configuration commits nothing past the boundary, would fail first.

## Card gates (T7.1.1)

| Gate | Evidence | State |
|---|---|---|
| The specification is in `models/` and `just model reconfigure` runs it to completion | this folder; every run in Results finishes exhaustively | met |
| Invariants 1 and 2 when replacing one member of 3, with a view change before the op commits, between commit and boundary, at the boundary, after it | `ReplaceOld`, `ReplaceNew` (and with a crash, `ReplaceOldCrash`, `ReplaceNewCrash`); witnesses for each position | met |
| The same for growing from 3 to 5 and shrinking from 5 to 3 | growing: `GrowOld`, `GrowNew`. Shrinking: `ShrinkNew` (view change in the new configuration); the view change in the old configuration, `large/ShrinkOld`, is too large to check here | open for shrinking with a view change in the old configuration |
| Three defects each produce a counterexample | `defects/` | met |
| No state where two disjoint quorums of different configurations both commit | `OneConfigurationPerOp` and `Handover`, in every run | met |
| A removed replica that keeps running cannot disturb the group | `RemovedCannotDisturb`, in every run; removed replicas run in every behaviour | met |
| Document 02 section 9 matches the checked specification | document 02 section 9; 02a D4 and O5 | met |

## Actions and the functions that implement them

The actions of the VSR model keep their rows in [models/vsr/README.md](../vsr/README.md#actions-and-the-functions-that-implement-them); here they take the quorums and members of the replica's epoch and only messages of that epoch. The actions this model adds or changes:

| Action | Document 02 section 9 | Function | Task | Status |
|---|---|---|---|---|
| `ClientRequest` (no op after the boundary in the old configuration) | step 2 | `Replica::on_request` | T7.1.2 | planned |
| `EnterEpoch` | steps 3 and 4 | `Replica::enter_epoch`, after `Replica::on_sync_checkpoint` for a new member | T7.1.2 | planned |
| `Commits` (only ops after the boundary in the new configuration) | step 4 | `Replica::commit_pipeline` | T7.1.2 | planned |
| the epoch check on every message | step 3 | `Replica::on_message` | T7.1.2 | planned |
