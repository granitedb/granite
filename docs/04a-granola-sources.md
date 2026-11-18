# 04a. Granola: sources, rule by rule, and our deviations

Companion to [04-cluster-and-cross-shard.md](04-cluster-and-cross-shard.md) section 4. It records, for every rule there, where the rule comes from in Granola or why it is ours; what we change and what could go wrong; and the proof obligations of Granola's timestamp mode with the mechanism that meets each. Written for task T5.1.1, 2026-11-18. The corrections it forced are already in document 04.

## 1. Sources

| Key | Source | Where read |
|---|---|---|
| **P** | James Cowling and Barbara Liskov, "Granola: Low-Overhead Distributed Transaction Coordination", USENIX ATC 2012, pages 223 to 235 | https://www.usenix.org/system/files/conference/atc12/atc12-final118.pdf, read in full |
| **T** | James Cowling, "Low-Overhead Distributed Transaction Coordination", PhD thesis, MIT, June 2012 | **Not read.** MIT DSpace (https://dspace.mit.edu/handle/1721.1/75706) answered every automated download with a firewall page (HTTP 429, "log in, then open the item again") on 2026-11-18, and the CORE mirror (https://core.ac.uk/download/9592489.pdf) with HTTP 403. Nothing below is cited from the thesis. |

Citations have the form `P §4.4`: a section of the paper. "Ours" means the rule is not in the paper; the reason follows it. The paper refers to the thesis for reconfiguration, the clock-skew window of external consistency and the coordinator extension (P §3.4, §4.6, §6.1.4); a rule marked "ours" may have a counterpart there. Checking every "ours" label and the proof obligations against the thesis is the part of this work still open (section 5, open point 1).

Terms. Granola's *repository* is our *group*; its *primary* is the group's VSR primary; its *stable log write* is a committed op; *TID* is our transaction id; *highTS* is our `client_ts`. Granola has two modes per repository: *timestamp mode* (no coordinated transaction present, everything runs in timestamp order without locks) and *locking mode* (P §4). We have no modes; section 3 explains what replaces them.

## 2. Every rule of document 04 section 4

Numbered in the order they appear in document 04 section 4. "Same" means our rule is the paper's; "adapted" means the idea is the paper's and the detail differs; D1 to D7 refer to section 3.

### Cross-shard is the main path

| # | Rule | Source | Note |
|---|---|---|---|
| 1 | Settlement is independent because funds are reserved first; coordinated only for conditional two-party work and global unique indexes | Adapted: P §2.1 | The paper motivates independent transactions with TPC-C, where replicating the Item table makes new-order independent. Reserving funds first is our way of making settlement independent: ours, for the exchange schema of document 03. |
| 2 | Everything is batched: proposals share a prepare, votes per group pair share a message, finalize records share a prepare | Adapted: P §5.1 | The paper batches log messages to the backups. Coalescing requests into one prepare (document 02 section 3) and batching votes per group pair are ours, needed because almost every transaction crosses shards. |
| 3 | No convoy: a vote is produced when the propose op commits, never behind the execution queue | Same: P §4.4 steps 2 to 3 | The repository goes on with other transactions once the vote is sent. Execution waits; voting does not. |
| 4 | Same-group shortcut: participants are computed per group | Adapted: P §4.3, §3.2 | A single-repository transaction is the paper's first class. In the paper the client application chooses the repositories; we compute them per group from the static keys, so keys on two tablets of one group make one participant. |
| 5 | Expected cost under half of single-shard throughput, latency about one replication round higher | Ours | An estimate to be measured at G3. For comparison only: the paper measured TPC-C throughput falling about 50% from 0% to 100% distributed (P §6.2), and distributed latency about twice single-repository latency (P §6.1.2). Our extra finalize round (D1) is not in those numbers. |
| 6 | Cross-shard transactions are built right after a static cluster exists | Ours | Scheduling, not protocol. |

### Preconditions

| # | Rule | Source | Note |
|---|---|---|---|
| 7 | Registered program with static keys only, so participants and per-participant key sets are computable | Adapted: P §2 | Granola's one-round transactions are the same shape: one operation per participant, no client interaction, at most one vote exchanged (P §2). Granola leaves operations uninterpreted; we require static keys so we can compute participants and key sets ourselves, which the bypass (D4) and the lock rules (D3) depend on. |
| 8 | The client sends the same transaction, a unique id and its highest seen timestamp to each participant primary | Same: P §4.2, §3.2 | Granola's TID is client id plus a per-client sequence number (P §3.2); ours is a client-chosen time-ordered id (D5). |

### Classification

| # | Rule | Source | Note |
|---|---|---|---|
| 9 | Independent: only statements that cannot fail | Adapted: P §2.1 | The paper's condition is weaker: every participant reaches the same commit or abort decision on its own, for example a decision that depends only on replicated data. Ours is "no participant can fail", which is simpler to check and covers the exchange. |
| 10 | Coordinated: any `require`, any fallible write, or a global unique index | Same: P §4.5 | Participants must agree on commit or abort. |
| 11 | The compiler reports the class; an `independent` annotation fails compilation if the program stops being independent | Ours | In the paper the client application chooses the class for each call (P §3.2). We derive it from the program so a client cannot pick a wrong one (section 5, open point 2). |

### Independent transaction

| # | Rule | Source | Note |
|---|---|---|---|
| 12 | `propose` op: log it, proposed timestamp above every timestamp issued and above `client_ts`, record as pending | Same: P §4.4 steps 1 to 2 | The paper's proposal is above highTS, above the last executed timestamp, and above the clock. **Corrected:** document 04 said `>= client_ts`; the paper and task T5.2.5 say strictly above. "Every timestamp issued" is stronger than the paper's "last executed" and is needed because our `shard_safe_ts` is computed from issued proposals. |
| 13 | Ties broken by transaction id | Same: P §4.1 | **Added** to document 04, which did not say how equal final timestamps are ordered. |
| 14 | Vote to the other participants | Same: P §4.4 step 3 | Primary to primary (P §4). |
| 15 | `finalize` op when all votes are in; final timestamp is the maximum vote | Adapted: P §4.4 step 5, §5.1 | The maximum is the paper's. Logging finalize as an op is D1. |
| 16 | Execute at the head of the timestamp order | Same: P §4.4 step 6, §4.6 | |
| 17 | Reply to the client with this participant's part of the result | Same: P §4.4 step 6, §4.2 | |
| 18 | Two log ops per participant per transaction | Ours: D1 | |
| 19 | Votes are derived from logged state; the new primary re-sends them; a participant missing a vote re-requests it | Same: P §5.1, §5.2, §5.4 | D2. |
| 20 | The client is not needed after the first message (proposal forwarding) | Ours: D2 | |
| 21 | Refusal vote from a participant that has not proposed | Same: P §4.4 step 4 | **Added** to document 04. The paper's participant that receives a CONFLICT vote stops processing the transaction and returns it to the client. We use the same message for `busy`, `unavailable`, `too_old` and the timestamp guard. |
| 22 | No aborts from conflicts, no locks | Same: P §4.4, §4.6 (timestamp mode) | In the paper this holds only in timestamp mode; in locking mode independent transactions take locks and can get CONFLICT (P §4.5). We keep it always: D3. |

### Timestamp-order execution

| # | Rule | Source | Note |
|---|---|---|---|
| 23 | Execute at t only when no transaction can still get a smaller timestamp on this shard | Same: P §4.4 last paragraph, §4.6 | A transaction runs only after every concurrent transaction with a lower timestamp at the repository. |
| 24 | `shard_safe_ts` = smallest pending proposal minus one, or the latest issued timestamp | Ours | The paper has no snapshot reads at a timestamp; this value serves them (document 04 section 5). It is safe because a final timestamp is never below the shard's own proposal (rule 15). |
| 25 | Step 1, strict order | Same: P §4.4, §4.6 | |
| 26 | Step 2, conflict-aware bypass | Adapted: P §5.4 | D4. **Corrected:** the bypass must check queued transactions too, not only pending ones. |
| 27 | The queue is replicated state, bounded, `busy` when full | Ours | The paper's backups execute in timestamp order once the final timestamp is known (P §5.1), so the queue is replicated there too, but the paper states no bound. Bounds: document 04 section 4, "Bounds". |

### Failure isolation

| # | Rule | Source | Note |
|---|---|---|---|
| 28 | Bypass limits the damage of a dead group to keys involved with it | Adapted: P §5.4 | D4. |
| 29 | Fail fast with `unavailable` | Ours: D7 | |
| 30 | No unilateral abort after voting; pending transactions wait for the dead group | Same: P §5.4 | "Since Granola provides strong consistency, we may have to stall because of failure." |
| 31 | `shard_safe_ts` stops on shards with such pending transactions | Ours | Follows from rule 24. |

### Duplicate requests

| # | Rule | Source | Note |
|---|---|---|---|
| 32 | Finished ids and outcomes kept for a retention window; older ids get `too_old` | Ours: D5 | The paper's participants answer a resent vote with the outcome if they finished the transaction (P §5.4), so some record is kept, but no retention rule is given. |

### Coordinated transaction

| # | Rule | Source | Note |
|---|---|---|---|
| 33 | `prepare` op: evaluate local requires, lock the declared keys, vote commit, abort with a code, or conflict | Same: P §4.5 steps 1 to 4, Figure 4 | The paper's prepare upcall acquires locks and returns COMMIT, ABORT or CONFLICT; the request, vote and proposed timestamp are logged together. |
| 34 | On ABORT or CONFLICT the participant replies at once | Same: P §4.5 step 4 | |
| 35 | `finalize` op: commit at the maximum proposed timestamp if all voted commit, else abort; release locks; execute | Adapted: P §4.5 steps 5 to 6 | The paper commits and replies as soon as the votes are in. Logged finalize is D1. Executing at finalize, out of timestamp order, is the paper's locking-mode rule: safe because no transaction that conflicts with it can run in between (P §4.6). |
| 36 | A prepare that finds a key locked, or in the key set of an unexecuted transaction, votes conflict and takes no locks | Adapted: P §4.5 | D3. **Corrected:** document 04 checked locks only. |
| 37 | A single-shard or independent transaction touching a locked key waits | Ours | The paper answers CONFLICT in locking mode (P §4.5, "Locking Mode"). D3. **Corrected:** the waiting transaction takes its timestamp after the lock is released. |
| 38 | Locks are an internal LSM tree, replicated, surviving view changes | Adapted: P §5.1, §5.4 | The paper logs the vote with the request (P §5.1) but does not say how a new primary rebuilds its locks. We keep the lock table itself as replicated state, so nothing has to be rebuilt. |
| 39 | Replica failure is masked by VSR | Same: P §5.1 | The paper uses Viewstamped Replication with 2f + 1 replicas. |
| 40 | A whole participant group unavailable: others hold pending state and locks; no abort after voting commit | Same: P §5.4 ("Locking Mode") | |
| 41 | Before voting, a participant may time out and vote abort | Ours | Standard two-phase commit; the paper does not discuss it. It is safe for the same reason as the refusal vote (rule 21). |
| 42 | Client failure is irrelevant after the first message reaches one participant | Ours: D2 | |

### Clock coupling between groups

| # | Rule | Source | Note |
|---|---|---|---|
| 43 | Raise the hybrid-clock floor to the highest timestamp seen on every vote and finalize | Adapted: P §4.4 step 1 | The paper's proposals are above the last executed timestamp; we raise the floor when the timestamp is seen, which is earlier and gives the strict "above anything seen" rule of T5.2.5. |
| 44 | Remote timestamp guard | Ours: D6 | |
| 45 | Clients send their highest seen timestamp on every request | Same: P §4.2, §4.6 | |
| 46 | Optional commit-wait for strict serializability | Ours | The paper does not provide external consistency (P §4.6) and notes violations are possible only within a window about the clock skew. |

## 3. The seven deviations

### D1. Finalize as a logged op

- **Paper.** Each log message records the request, the proposed timestamp and the vote. The final timestamp is piggybacked on later log messages to the backups and is not itself a stable write; once the primary has every vote the transaction is ready and runs in timestamp order without a further stable write, and backups execute in timestamp order once they learn the final timestamp (P §5.1, §4.4 steps 5 and 6). After a failover the new primary may not know some final timestamps and recovers them by re-sending its votes to collect the others' (P §5.2).
- **Ours.** `finalize` is an op of its own, committed through VSR before the transaction executes.
- **Why.** Every replica must execute the transaction at the same op so that storage stays byte-identical (document 01) and checkpoints and state sync work by bytes. A new primary also knows every final timestamp from its log.
- **What could go wrong.** One extra replication round before the reply, which is in the cost estimate of document 04. A finalize logged from an incomplete set of votes would give different final timestamps on different participants (invariant 2); the finalize op must carry or check the full vote set. The extra record also costs log space; finalize records batch (rule 2).

### D2. Votes derived from logged state

- **Paper.** The same idea: the vote is logged with the request (P §5.1); a new primary knows every vote its predecessor sent and re-sends them (P §5.2); a repository that does not hear from a participant re-sends its votes to the others, which answer with the outcome if they finished (P §5.4).
- **Ours.** The same, plus re-requesting a missing vote and forwarding the proposal to a participant that never received it from the client.
- **Why.** The client is not needed after its first message reaches one participant, so a crashed client cannot leave transactions pending forever.
- **What could go wrong.** A vote sent before its propose op commits could be lost in a view change, and the new primary would propose a different timestamp; T5.2.3 asserts the re-sent vote equals the original. A forwarded proposal can reach a participant that already refused or finished the id; the logged refusal and the finished-transaction table answer it, and it must never be proposed again.

### D3. Conflict vote instead of waiting

- **Paper.** A coordinated prepare that cannot take its locks returns CONFLICT and the client retries (P §4, Figure 4). When a coordinated transaction arrives in timestamp mode, the repository prepares the in-flight independent transactions first and **blocks** the coordinated one until those prepares succeed (P §4.5, "Switching Modes"). In locking mode, independent and single-repository transactions that hit a lock get CONFLICT (P §4.5, "Locking Mode").
- **Ours.** A coordinated prepare never waits: if any of its keys is locked or belongs to an unexecuted transaction, it votes conflict at once and takes no locks. Independent and single-shard transactions never get conflict; they wait for the lock, and take their timestamp only when it is released.
- **Why.** A coordinated prepare that never waits adds no waits-for edge, so no distributed deadlock can form, and the lock holder waits only on votes that other shards produce without queueing. Independent transactions must not fail, which is what lets the exchange settle without a retry path.
- **What could go wrong.**
  - *Stale `require`* (found in this reading, corrected): checking locks alone, a coordinated prepare can read state that a pending independent transaction on the same key has not yet changed, and that transaction may end up ordered first. Example: a pending settlement debits a balance; a coordinated withdrawal checks the balance before the debit executes, and both commit. The paper avoids this by preparing the in-flight independent transactions; we vote conflict.
  - *Timestamp below the lock holder* (corrected): a transaction that took its timestamp while waiting for a lock can be ordered before the holder and still read its writes. Taking the timestamp at release puts it after the holder's final timestamp.
  - *Starvation*: on a key with constant independent traffic, a coordinated prepare can conflict every time. Client backoff (T5.2.7) is the only remedy now. Coordinated transactions are meant for low-rate work (document 03); if they starve in practice, a reservation that holds new independent work on the key for a bounded time is the next step.

### D4. Conflict-aware bypass

- **Paper.** In timestamp mode every transaction runs in timestamp order (P §4.4). When a participant stays unresponsive, the repository switches to locking mode: it prepares the blocked transactions in timestamp order, executes at once each one whose locks do not conflict with anything earlier in the queue, and uses `forcePrepare` to take the locks a transaction could need in any order (P §5.4). The paper argues this is safe because everything later in the queue gets a later timestamp, and the executed transaction conflicts with nothing earlier in the queue (P §5.4).
- **Ours.** Always on, not a failure mode: a transaction whose declared key set intersects no unexecuted transaction ordered before it executes at once.
- **Why.** Almost every transaction crosses shards, so one slow or dead group would otherwise stall every shard that has a transaction pending with it (document 04, failure isolation). Static key sets are exactly the order-independent lock set that `forcePrepare` approximates, so the check is exact and cheap and needs no mode switch.
- **What could go wrong.**
  - *Overtaking a queued transaction* (corrected): document 04 checked only pending key sets. A transaction finalized and waiting in the queue behind an older pending one is just as unexecuted; overtaking it on a shared key breaks timestamp order. The paper's "earlier in the queue" includes both.
  - *Incomplete key sets*: a key the transaction reads or writes but does not declare (a derived-table row, an index entry) makes the check pass when it should not, and serializability breaks silently. The declared set must be computed by the compiler, never written by hand.
  - *Snapshot reads*: a bypassed transaction executes above `shard_safe_ts`, so reads at or below it are unaffected; the safe timestamp must not be advanced past a bypassed transaction's timestamp while older transactions are still pending.

### D5. Finished-transaction table

- **Paper.** A TID is the client id plus a sequence number that rises with each request (P §3.2). A client keeps TIDs unique after a crash by persisting ranges of sequence numbers, and recovers its highest timestamp by waiting out the maximum clock skew (P §5.3). A participant answers a re-sent vote with the outcome of a finished transaction (P §5.4). No retention rule is given.
- **Ours.** A client-chosen time-ordered id; each participant keeps finished ids and outcomes for one hour of commit time and answers duplicates from it; an older id gets `too_old`.
- **Why.** Thousands of clients send cross-shard work to many groups; per-client sequence state in every group would be large and need its own eviction. A time-ordered id makes "older than the window" decidable from the id alone, so the table can be bounded.
- **What could go wrong.** The id's time comes from the client's clock: a clock far behind makes fresh ids look old (`too_old` on a first attempt), far ahead is caught by the guard (D6). `too_old` must be presented to the application as "outcome unknown", not "failed". A duplicate that arrives while the original is pending must attach to it, not propose again (T5.4.5).

### D6. Remote timestamp guard

- **Paper.** Loosely synchronised clocks are needed for performance, not for correctness (P §3.4), except for one client-recovery option (P §5.3). There is no guard. Every proposal is above the client's highTS (P §4.4), so one far-future timestamp is inherited by every repository the client or transaction touches.
- **Ours.** A timestamp from a client or another group more than 100 ms ahead of the receiver's synchronised clock is rejected.
- **Why.** With hybrid-clock floors one bad timestamp spreads through votes and client timestamps to the whole cluster and cannot be undone, since timestamps never go back.
- **What could go wrong.** Rejecting a vote after this participant has voted would leave the transaction committed on others and missing here. So (clarified in document 04) client timestamps are checked before propose, a vote that arrives before this participant proposes is refused with a refusal vote, and a vote after it has voted leaves the transaction pending until this participant's own clock is within the bound of that timestamp, then finalizes it, and reports the sender. That last case needs a whole group's clock to be wrong, because a primary outside the quorum's clock interval does not assign timestamps (document 02 section 6). A bound too tight for the real clock discipline would reject healthy traffic; 100 ms is about a hundred times the expected skew.

### D7. Fail-fast `unavailable`

- **Paper.** When a whole repository is down, the others stall: they re-send votes, hold locks in locking mode, and in timestamp mode switch to locking mode to make what progress they can (P §5.4). The paper describes no early refusal of new transactions that name the dead repository.
- **Ours.** Clients and shards track group reachability; a new transaction naming an unreachable group is rejected with `unavailable` before it is proposed.
- **Why.** Every transaction accepted for a dead group becomes pending somewhere, holds back `shard_safe_ts` there and widens the set of keys that cannot be bypassed. Refusing early keeps the damage to what was already in flight.
- **What could go wrong.** The client and a participant can disagree about reachability. If the client sends and one participant refuses after others have proposed, those others would wait forever for a vote; the refusal vote (rule 21) ends the transaction on all of them. A short view change must not be taken for an outage, hence hysteresis (T5.4.4). Transactions already pending still wait (rule 30); fail-fast does not help them.

## 4. Proof obligations of timestamp mode

The paper argues serializability in timestamp mode in P §4.6, resting on the rules of P §4.1 to §4.4 and the replication rules of P §5.1 and §5.2. It has no formal proof; these are the conditions that argument needs, each with the Granite mechanism that meets it and where that is checked. "Invariant n" is document 04 section 6.

| # | Obligation | Source | Granite mechanism | Checked by |
|---|---|---|---|---|
| O1 | Timestamps define a total order on transactions; equal timestamps are ordered by transaction id | P §4.1 | Order is `(timestamp, transaction id)` (document 04, independent transaction) | Invariant 4 (global serializability checker, T5.4.1) |
| O2 | Every participant executes a transaction at the same timestamp | P §4.4 step 5, §4.6 | Final timestamp is the maximum of the logged proposals, so it is the same wherever the full vote set is known; finalize is logged (D1) and must be built from the full vote set | Invariant 2; T5.2.2 gate on identical final timestamps |
| O3 | Each repository executes in timestamp order: a transaction runs only after every transaction with a lower timestamp that could still reach that repository | P §4.3 step 3, §4.4 last paragraph | `shard_safe_ts` and the timestamp-order queue; the bypass may reorder only transactions with disjoint key sets, including queued ones (D4) | Invariant 3; T5.2.1 queue-order assertion; T5.4.3 equivalence of bypass and strict order |
| O4 | A proposal is above every timestamp the repository has executed, so the final timestamp, which is at least the proposal, cannot fall below anything already executed there | P §4.4 step 1 | Proposal above every timestamp issued, which includes every executed one; final is the maximum vote | Invariant 3; T5.2.2 gate "final at least every proposal" |
| O5 | A proposal is above the client's highTS, so a transaction is ordered after everything its client has observed | P §4.4 step 1, §4.6 | Proposal strictly above `client_ts`; clients send the highest timestamp seen; floor raised on every vote and finalize | T5.2.5 causal-order checker and the injected bug that drops the client timestamp |
| O6 | After a client sees T at one participant, its later requests execute after T at every participant, even one that has not executed T yet | P §4.6, second paragraph | Same as O5: the later request carries `client_ts` at least T's timestamp, so its proposal is above T's and it queues behind T where T is unexecuted | T5.2.5 causal-order checker |
| O7 | The request and proposed timestamp are stable before the vote is sent, so a new primary never sends a different proposal for the same transaction | P §4.4 step 2, §5.2 third item | The vote is derived from the committed propose op (D2) | T5.2.3 gate that a re-sent vote carries the original timestamp, with its injected bug |
| O8 | Backups execute the same transactions in the same order as the primary | P §5.1 | Execution happens only by executing committed ops (finalize, then queue release), so every replica executes at the same op (D1) | T5.2.2 convergence gate; T5.2.1 convergence checker |
| O9 | A read served by a primary that has been replaced must not return stale data | P §5.2 first item (leases) | Not by leases: linearizable reads wait for a replication quorum to acknowledge a later message (document 02 section 8); snapshot reads wait for `shard_safe_ts` (document 04 section 5) | Invariant 5; replication tests of document 02 |
| O10 | A transaction executed out of timestamp order must not conflict with any transaction it overtakes | P §4.6, last paragraph; P §5.4 | Used in two places: a coordinated transaction executes at finalize while holding locks on its keys, and no unexecuted transaction can share them (D3); the bypass checks key sets (D4) | T5.1.2 model (bypass and conflict-vote injected bugs); T5.4.3 |

O10 belongs to the paper's locking mode, not its timestamp mode. It is listed because we use the same argument without the mode switch.

## 5. The open points of document 04 section 8

1. **Granola's vote exchange path, recovery rules and proof obligations.** Answered from the paper; the thesis cross-check is open.
   - Path: votes go directly from each participant's primary to the other participants' primaries; there is no coordinator, and the client only sends the request and collects replies (P §4, §4.4 step 3, Figures 6 and 7). Document 04 already had this.
   - Recovery: the vote is logged with the request; a new primary re-sends every vote to recover missing final timestamps (P §5.1, §5.2); a repository that hears nothing from a participant re-sends its votes to the others, which answer with the outcome if they finished; in timestamp mode it then moves to locking mode to execute what does not conflict (P §5.4). Our versions are D2 and D4.
   - Proof obligations: section 4 above.
   - Open: the thesis could not be fetched (section 1). Document 04 section 8 keeps this as an open point until each "ours" label and each obligation has been checked against it.
2. **Can classification be fully automatic from the IR?** Yes. Every rule that decides the class is a property of the program's IR: whether keys are static, whether any `require` is present, whether any write can fail, whether a global unique index is touched. The classifier must be conservative: a statement it cannot prove infallible makes the program coordinated, which costs speed, never correctness. The `independent` annotation does not decide anything; it only makes compilation fail if the derived class changes. Both are already task T5.2.6, so no new task is needed. Two cases the classifier must treat as fallible: arithmetic that can return a typed error (overflow, division by zero; document 03 section 2), and an insert into an immutable table whose key is not fixed by the transaction, which can return `exists_different` (document 03 section 9). See finding F5.
3. **Bounds.** Fixed in document 04 section 4, "Bounds": at most 8 participants per transaction, 4,096 pending transactions per shard, a queue of 16,384. They go into the constants module when it exists (T0.1.4); until then document 04 is their only home.

## 6. Findings for other tasks

The corrections in document 04 change behaviour that some task texts describe. Task texts and gates are not changed here; each needs its own reviewed change to `docs/tasks/`.

| # | Task | What disagrees | Suggested change |
|---|---|---|---|
| F1 | T5.2.2 | Work says the proposal is "at least the client's" timestamp | Strictly above, as document 04 and T5.2.5's gate now say |
| F2 | T5.2.2, T5.4.4 | Gates say `busy`, `unavailable` and `too_old` are all decided before the transaction is proposed anywhere. With the client sending to every participant, one participant can be full or have a different reachability view while another has already proposed. | Decided before *the refusing participant* proposes, with the refusal vote ending the transaction on the others; a test that no participant is left pending after a refusal |
| F3 | T5.4.3, T5.1.2 | The bypass check is described against pending key sets only | Against every unexecuted transaction ordered earlier, pending or queued; the injected bug list of T5.1.2 could add "bypass of a queued transaction" |
| F4 | T5.2.4, T5.1.2 | The conflict rule checks locks only, and a waiting transaction's timestamp is not specified | Conflict also on keys of unexecuted transactions; timestamp taken at lock release. Both are needed in the model of T5.1.2 |
| F5 | T5.2.6 | Soundness corpus must cover inserts that can return `exists_different` and arithmetic that can fail | Add both to the corpus as coordinated unless the IR proves them infallible |
| F6 | T0.1.4 | The three bounds of document 04 section 4 are not yet in a constants module | Add them with the static assertion named there |
