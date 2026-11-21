# Standby site model

A TLA+ specification of the asynchronous standby site of [document 04 section 7](../../docs/04-cluster-and-cross-shard.md#7-asynchronous-standby-site-confirmed-2026-11-18), checked with TLC (task T7.4.1). It covers log shipping at different rates per group, durable and non-durable receipt with standby crashes, the apply timestamp Ta, execution up to Ta, promotion at any moment, discarding everything beyond Ta, and epoch fencing of the old site. It checks invariant 8 of document 04 section 6.

`just model standby-site` checks it. The rules of this folder are in [models/README.md](../README.md).

## Files

| File | Content |
|---|---|
| [StandbySite.tla](StandbySite.tla) | The specification, the scenario and the properties |
| `StandbySite.cfg` | 3 groups, 3 cross-shard transactions, crashes; no new work at site B after promotion. Must finish with no violation |
| `Fencing.tla`, `Fencing.cfg` | 3 groups, 2 cross-shard transactions, crashes; site B takes new transactions after promotion while site A keeps running. Must finish with no violation |
| `defects/*.tla`, `defects/*.cfg` | Injected defects. Each must produce a counterexample |

## What is modelled

- **Site A** is the primary site. Each group has a committed log of independent cross-shard transactions (document 04 section 4): a `prop` op with a proposed timestamp one above the group's clock floor, then, once every participant has logged its `prop`, a `fin` op with the maximum of the proposals. Votes, view changes and coordinated transactions are the cross-shard model's ([xshard](../xshard/README.md)) and are not repeated: what the standby site sees of a group is its committed log, and xshard checks that the logs are right. Site A runs to the end in every behaviour, before, during and after promotion, always at epoch 1.
- **Shipping.** The remote standby of a group receives the next op of the group's committed log at any moment, independently of the other groups, so the two legs of a cross-shard transaction arrive at different times and in any interleaving. It holds at most `Unflushed` ops received but not yet durable; `Flush` makes them durable, and `Crash` drops them (at most `MaxCrashes` in all), after which shipping resumes from the durable prefix.
- **Ta.** The coordinator sets Ta to the minimum over groups of `shard_safe_ts` of the durable prefix of each group's log: one below the oldest proposal still pending in it, or the clock floor if none is. It recomputes at any moment, so Ta lags by any amount, and it takes the computed value as it is, with no maximum, so that `TaMonotone` checks that the rule itself never decreases.
- **Apply.** A standby group learns Ta at any moment after it changes and executes, from its durable log, every transaction finalized at or below Ta, in `(timestamp, id)` order. Groups do so independently of each other.
- **Promotion** can start at any moment. The command freezes Ta and moves site B to epoch 2, which rejects every shipment from then on. Each group then, at its own pace, executes up to Ta, keeps in its log only the ops of the transactions it executed, appends a `promote` op that raises its clock floor to Ta, and becomes a voting group of epoch 2.
- **Site B after promotion** (`NewWork = TRUE`, the `Fencing` run): a promoted group proposes any transaction it has not proposed (new work, or a client's retry of one that was discarded) and finalizes it from the votes of the other participants. It accepts a vote only from a promoted group, at epoch 2: a standby never votes, and site A's votes are of epoch 1. An op records the lowest epoch it was made from.

Not modelled: coordinated transactions, single-shard transactions and heartbeat ops (each only adds ops that move a group's clock floor, as `prop` and `fin` do), uncommitted prepares and view changes at site A (the standby counts only ops it knows are committed), batching, checksums and re-requests of shipments, checkpoint sync of a standby that falls behind, the coordinator's own crash (Ta is persisted; a restarted coordinator recomputes it by the same rule, which `ComputeTa` already allows at any moment), a crash during promotion, the metadata group's standby, and failback.

## Properties

| Name | Statement | Card gate |
|---|---|---|
| `Inv8` | Invariant 8: once every group is promoted, there is one timestamp T, at which site A's state is complete, such that every group's executed state equals site A's state at T on that group. So no cross-shard transaction is present on some groups and absent on others. | invariant 8 |
| `Fenced` | Every op of epoch 1 in a promoted group's log belongs to a transaction the promoted state executed, which site A committed before promotion at or below Ta. Every other op was made at site B from votes of site B only. So nothing site A commits after promotion is accepted by site B. | fencing |
| `TaMonotone` | Ta never decreases (action property). | Ta never decreases |
| `ApplyIsAState` | At every moment, each standby group's executed state is site A's state on that group at the timestamp it has executed up to, in timestamp order. | (T7.4.3) |
| `PromotedAtTa` | Each promoted group's state is site A's state at Ta, the timestamp promotion reports. | (T7.4.4) |
| `SameTsB` | Invariant 2 at site B: promoted groups finalize a transaction at the same timestamp. | |
| `NewAfterTa` | Every transaction site B takes after promotion is ordered above Ta, so the promoted state does not change at any timestamp at or below Ta. | |
| `TypeOK` | Bounds and types. | |

Deadlock checking is on in the passing runs: every behaviour ends with every transaction finalized at site A, every group promoted and, in `Fencing`, every transaction finalized at site B.

Each defect run checks only `Inv8`, `Fenced` and `TaMonotone`, the card's properties, with deadlock checking off, so that its counterexample is to one of them.

## Scenario

Three groups. Transactions are cross-shard and in a ring: 1 on groups 1 and 2, 2 on groups 2 and 3, 3 on groups 3 and 1, so every group takes part in two of them and every pair of groups shares one.

## Results

Recorded on 2026-11-22 with `just model standby-site` (exit 0) on an Apple-silicon Mac with 14 cores, TLC 2.19 from the pinned `tla2tools.jar` v1.7.4, Java 21. Every run has 3 groups, at most one op per standby received but not durable (`Unflushed = 1`), one standby crash in all (`MaxCrashes = 1`), arbitrary shipping delays, and promotion at any moment.

| Run | Transactions | New work at site B | Result | Distinct states | Generated | Depth | Time |
|---|---|---|---|---|---|---|---|
| `StandbySite` | 1, 2, 3 | no | no violation | 26,142,353 | 121,503,100 | 37 | 646 s |
| `Fencing` | 1, 2 | yes | no violation | 2,496,977 | 9,437,633 | 34 | 34 s |

`StandbySite` is the gate run for invariant 8 and Ta: three cross-shard transactions. `Fencing` adds new transactions at site B, against site A still running; with three transactions, even with no crash and nothing held non-durable, it passed 16 million distinct states in 5 minutes with its queue still growing, so it checks two.

Injected defects, each found by `just model standby-site`:

| Defect | Card gate | Bounds | Counterexample | Trace | Distinct states | Time |
|---|---|---|---|---|---|---|
| `ApplyOnArrival`: a standby executes what it has durably received up to its own group's safe timestamp, as a local standby would, instead of up to Ta | applying prepares as they arrive | `StandbySite` | `Inv8` | 12 states | 217,416 | 5 s |
| `TaFromReceived`: Ta is computed from what the standbys received instead of what they made durable | computing Ta from received logs | `StandbySite` | `TaMonotone` | 17 states | 3,287,430 | 94 s |
| `NoDiscard`: promotion executes everything a group durably received and finalized, instead of stopping at Ta | promoting without discarding beyond Ta | `StandbySite` | `Inv8` | 11 states | 82,456 | 4 s |
| `NoFence`: a promoted group accepts votes of epoch 1 from the old site | (fencing) | `Fencing` | `Fenced` | 6 states | 693 | 1 s |

- `ApplyOnArrival` and `NoDiscard`: group 2 has durably received both ops of transaction 1, group 1 none, and Ta is 0. Group 2 executes transaction 1 (on arrival, or at promotion), group 1 cannot, and the promoted site holds transaction 1 on group 2 only.
- `TaFromReceived`: a standby has received an op it has not written, Ta counts it, the standby crashes and loses it, and the recomputed Ta is lower. With `TaMonotone` left out of the `.cfg`, the same defect violates `Inv8` in 20 states, with no crash: group 2 has received transaction 1's finalize but written only its propose, Ta reaches 1 from the received prefix, and at promotion group 1 executes transaction 1 while group 2 cannot.
- `NoFence`: site A's group 1 proposes transaction 1; site B is promoted; its group 2 proposes transaction 1 too and finalizes it with site A's vote.

`NewAfterTa` is not vacuous: with the `promote` op's timestamp set to 0 instead of Ta, the `Fencing` bounds violate it in 21 states. These two extra runs are not part of `just model`; their command is the runner's, with the changed `.cfg` or module in place.

## Action map

Every action of `StandbySite.tla` and the task that implements it. Function names are planned names; the implementing task replaces them with the real ones and updates this table.

| Action | Models | Task | Function |
|---|---|---|---|
| `Propose`, `Finalize` | site A's committed cross-shard ops | T5.2.2 (see the [xshard](../xshard/README.md) action map) | `Participant::propose`, `Participant::finalize` |
| `Ship` | a remote standby receives the next committed op of its group; rejected once the site is at the new epoch | T7.4.2 (shipping), T7.4.4 (epoch check) | `RemoteStandby::on_shipment` |
| `Flush` | received ops written to the standby's WAL | T7.4.2 | `RemoteStandby::on_durable` |
| `Crash` | a standby restarts from its WAL and re-requests what it lost | T7.4.2 | `RemoteStandby::recover` |
| `SafeTs`, `MinSafe`, `ComputeTa` | Ta from each group's `shard_safe_ts` of its durable prefix, persisted | T7.4.3 | `StandbyCoordinator::compute_ta`, `RemoteStandby::durable_safe_ts` |
| `Apply`, `ExecTo` | a standby executes, in timestamp order, the transactions finalized at or below Ta | T7.4.3 | `RemoteStandby::apply_to` |
| `Promote` | the command freezes Ta and takes the new cluster epoch | T7.4.4 | `promote` command, `StandbyCoordinator::freeze` |
| `PromoteGroup` | a group executes up to Ta, discards the rest, raises its clock floor to Ta and becomes voting | T7.4.4 | `RemoteStandby::promote` |
| `BPropose`, `BFinalize`, `VotesFrom` | the promoted site takes transactions and accepts only votes of its epoch | T7.4.4 (epoch check), T5.2.2 | `Participant::on_vote` (epoch check) |
