# 04a. Granola: sources, rule by rule, and our deviations

Companion to [04-cluster-and-cross-shard.md](04-cluster-and-cross-shard.md) section 4. It records, for every rule there, where the rule comes from in Granola or why it is ours; what we change and what could go wrong; and the proof obligations of Granola's timestamp mode with the mechanism that meets each. Written for task T5.1.1, 2026-11-18. The corrections it forced are already in document 04.

## 1. Sources

| Key | Source | Where read |
|---|---|---|
| **P** | James Cowling and Barbara Liskov, "Granola: Low-Overhead Distributed Transaction Coordination", USENIX ATC 2012, pages 223 to 235 | https://www.usenix.org/system/files/conference/atc12/atc12-final118.pdf, read in full |
| **T** | James Cowling, "Low-Overhead Distributed Transaction Coordination", PhD thesis, MIT, June 2012 | https://dspace.mit.edu/handle/1721.1/75706 (173 pages), read 2026-11-18: chapters 4, 5, 7 and 9, sections 2.1 to 2.2.1 and 3.2 to 3.3, and appendices A and B in full; the rest (introduction, evaluation, related work) by the contents only |

Citations have the form `P §4.4` (paper section) or `T §5.3.1` (thesis section). The thesis is the fuller account: it adds the view-change rules, request forwarding, retries and duplicate detection, timestamp-mode recovery, implicit votes and the external-consistency proof, so most rules cite it. "Ours" means the rule is in neither source; the reason follows it. Where the two differ, the thesis is cited.

Terms. Granola's *repository* is our *group*; its *primary* is the group's VSR primary; its *stable log write* is a committed op; *TID* is our transaction id; *highTS* is our `client_ts`. Granola has two modes per repository: *timestamp mode* (no coordinated transaction present, everything runs in timestamp order without locks) and *locking mode* (T §4.3.1). We have no modes; section 3 explains what replaces them.

## 2. Every rule of document 04 section 4

Numbered in the order they appear in document 04 section 4. "Same" means our rule is Granola's; "adapted" means the idea is Granola's and the detail differs; D1 to D7 refer to section 3.

### Cross-shard is the main path

| # | Rule | Source | Note |
|---|---|---|---|
| 1 | Settlement is independent because funds are reserved first; coordinated only for conditional two-party work and global unique indexes | Same: T §2.2.1, §9.1.2 | The thesis describes this exact conversion: an escrow transaction reserves the funds in one repository, then an independent transaction completes the transfer. It warns that a hold can be stranded if the client fails between the two steps (T §9.1.2); in our schema the hold is an open order, released by cancel or by settlement (document 03 section 10), so no protocol action is needed. |
| 2 | Everything is batched: proposals share a prepare, votes per group pair share a message, finalize records share a prepare | Adapted: P §5.1, T §5.1 | The paper and thesis batch log messages to the backups, like group commit. Coalescing requests into one prepare (document 02 section 3) and batching votes per group pair are ours, needed because almost every transaction crosses shards. |
| 3 | No convoy: a vote is produced when the propose op commits, never behind the execution queue | Same: P §4.4 steps 2 to 3, T §4.3.4 step 3 | The repository goes on with other transactions once the vote is sent; it keeps voting while execution is queued (T §4.3.4, last paragraph). |
| 4 | Same-group shortcut: participants are computed per group | Adapted: P §4.3, §3.2, T §3.2.1, §7.1 | A single-repository transaction is Granola's first class. In Granola the client application chooses the repositories, using a name service that maps application names to repository ids (T §7.1); we compute them per group from the static keys and the tablet map, so keys on two tablets of one group make one participant. |
| 5 | Expected cost under half of single-shard throughput, latency about one replication round higher | Ours | An estimate to be measured at G3. For comparison only: the paper measured TPC-C throughput falling about 50% from 0% to 100% distributed (P §6.2), and distributed latency about twice single-repository latency (P §6.1.2); a distributed transaction takes three one-way message delays and one stable log write (T §9.2). Our extra finalize round (D1) is not in those numbers. |
| 6 | Cross-shard transactions are built right after a static cluster exists | Ours | Scheduling, not protocol. |

### Preconditions

| # | Rule | Source | Note |
|---|---|---|---|
| 7 | Registered program with static keys only, so participants and per-participant key sets are computable | Adapted: P §2, T §2.1 | Granola's one-round transactions are the same shape: one operation per participant, no client interaction, at most one vote exchanged. Granola leaves operations uninterpreted; we require static keys so we can compute participants and key sets ourselves, which the bypass (D4) and the lock rules (D3) depend on. Static key sets are what the thesis calls order-independent lock-supersets (T §5.3.1). |
| 8 | The client sends the same transaction, a unique id and its highest seen timestamp to each participant primary | Same: P §4.2, §3.2, T §4.2 | Granola's TID is the client id plus a per-client sequence number (T §4.2.1); ours is a client-chosen time-ordered id (D5). A backup that receives a request forwards it to the primary (T §4). |

### Classification

| # | Rule | Source | Note |
|---|---|---|---|
| 9 | Independent: only statements that cannot fail | Adapted: P §2.1, T §2.2.1 | Granola's condition is weaker: every participant reaches the same commit or abort decision on its own, for example one that depends only on replicated data. Ours is "no participant can fail", which is simpler to check and covers the exchange. |
| 10 | Coordinated: any `require`, any fallible write, or a global unique index | Same: P §4.5, T §4.3.5 | Participants must agree on commit or abort. |
| 11 | The compiler reports the class; an `independent` annotation fails compilation if the program stops being independent | Ours | In Granola the client application chooses the class for each call (T §3.2.1). The thesis names automatic classification as future work and an open question, possible only for transactions with a well-defined syntax such as SQL over a schema (T §9.1.2). Our registered programs are that case (section 5, open point 2). |

### Independent transaction

| # | Rule | Source | Note |
|---|---|---|---|
| 12 | `propose` op: log it, proposed timestamp above every timestamp issued and above `client_ts`, record as pending | Same: P §4.4 steps 1 to 2, T §4.3.4 steps 1 to 2, §7.4 | Granola's proposal is above highTS and the last executed timestamp (T §4.3.4). **Corrected:** document 04 said `>= client_ts`; Granola and task T5.2.5 say strictly above. "Every timestamp issued" is the stronger rule the thesis requires for implicit votes: proposals assigned in increasing order (T §7.4). We need it because `shard_safe_ts` is computed from issued proposals. |
| 13 | Ties broken by transaction id | Same: P §4.1, T §4.1 | **Added** to document 04, which did not say how equal final timestamps are ordered. |
| 14 | Vote to the other participants | Same: P §4.4 step 3, T §4.3.4 step 3 | Primary to primary, no coordinator (T §4, §4.3.4). |
| 15 | `finalize` op when all votes are in; final timestamp is the maximum vote | Adapted: P §4.4 step 5, T §4.3.4 step 5, §5.2.1 | The maximum is Granola's. Logging finalize as an op is D1. |
| 16 | Execute at the head of the timestamp order | Same: P §4.4 step 6, T §4.3.4 step 6, §4.4.2 | |
| 17 | Reply to the client with this participant's part of the result | Same: P §4.4 step 6, T §4.3.4 step 7, §4.2.2 | |
| 18 | Two log ops per participant per transaction | Ours: D1 | |
| 19 | Votes are derived from logged state; the new primary re-sends them; a participant missing a vote re-requests it | Same: P §5.2, T §5.2.1, §5.4, appendix A.1.2 | A new primary re-sends a vote for every logged transaction without a final timestamp, with a retry flag that asks the receivers to re-send theirs (T §5.2.1). D2. |
| 20 | The client is not needed after the first message (proposal forwarding) | Same: T §5.2.2, §5.4 | A repository keeps each request so it can forward it to a participant that never received it, and retry rules let a transaction finish when the client fails before sending to every participant. The paper does not describe this. D2. |
| 21 | Refusal vote from a participant that has not proposed | Same: P §4.4 step 4, T §4.3.4 step 4, §5.2.1, §5.3.1 | **Added** to document 04. A participant that receives a CONFLICT vote stops processing the transaction and returns it to the client. The thesis uses this to refuse new work without blocking others: a new primary answers new requests with a CONFLICT vote while its view change runs (T §5.2.1), and a transaction a repository has not yet voted for can be aborted safely (T §5.3.1). We use it for `busy`, `unavailable`, `too_old` and the timestamp guard. |
| 22 | No aborts from conflicts, no locks | Same: P §4.4, T §4.3.4 (timestamp mode) | In Granola this holds only in timestamp mode; in locking mode independent transactions take locks and can get CONFLICT (T §4.3.5). We keep it always: D3. |

### Timestamp-order execution

| # | Rule | Source | Note |
|---|---|---|---|
| 23 | Execute at t only when no transaction can still get a smaller timestamp on this shard | Same: P §4.4, T §4.3.4 last paragraph | A transaction runs only after every concurrent transaction with a lower timestamp at the repository. |
| 24 | `shard_safe_ts` = smallest pending proposal minus one, or the latest issued timestamp | Adapted: T §7.2 | The thesis serves a read-only transaction at a backup at that backup's last executed timestamp, which is safe because everything before it in the serial order has executed (T §7.2). `shard_safe_ts` serves snapshot reads the same way (document 04 section 5). It is safe because a final timestamp is never below the shard's own proposal (rule 15). |
| 25 | Step 1, strict order | Same: P §4.4, T §4.3.4, §4.5.2 | |
| 26 | Step 2, conflict-aware bypass | Adapted: T §9.1.1, §5.3.1 | The thesis proposes exactly this as future work: lock only the blocked transaction and let later transactions that do not conflict pass it (T §9.1.1). D4. **Corrected:** the bypass must check queued transactions too, not only pending ones. |
| 27 | The queue is replicated state, bounded, `busy` when full | Adapted: T §5.2.1 | Backups keep the log in timestamp order and execute its prefix of finalized entries (T §5.2.1, Figure 5.2), so the queue is replicated there too, but no bound is given. Bounds: document 04 section 4, "Bounds". |

### Failure isolation

| # | Rule | Source | Note |
|---|---|---|---|
| 28 | Bypass limits the damage of a dead group to keys involved with it | Adapted: T §9.1.1, §5.3.1 | D4. |
| 29 | Fail fast with `unavailable` | Ours: D7 | |
| 30 | No unilateral abort after voting; pending transactions wait for the dead group | Same: P §5.4, T §5.3 | The unresponsive participant may already have received every vote and executed the transaction (T §5.3). |
| 31 | `shard_safe_ts` stops on shards with such pending transactions | Ours | Follows from rule 24. |

### Duplicate requests

| # | Rule | Source | Note |
|---|---|---|---|
| 32 | Finished ids and outcomes kept for a retention window; older ids get `too_old` | Adapted: T §5.4 | Granola keeps recent replies in a circular buffer that covers a time window, and votes until every participant has executed (T §5.4). It does not say how a duplicate that arrives after its reply has been dropped is kept from executing again; `too_old` is ours. D5. |

### Coordinated transaction

| # | Rule | Source | Note |
|---|---|---|---|
| 33 | `prepare` op: evaluate local requires, lock the declared keys, vote commit, abort with a code, or conflict | Same: P §4.5, T §4.3.5 steps 1 to 4, Figure 4.7 | The prepare upcall acquires locks and returns COMMIT, ABORT or CONFLICT; the request, vote and proposed timestamp are logged together. |
| 34 | On ABORT or CONFLICT the participant replies at once | Same: P §4.5 step 4, T §4.3.5 step 4 | |
| 35 | `finalize` op: commit at the maximum proposed timestamp if all voted commit, else abort; release locks; execute | Adapted: P §4.5, T §4.3.5 steps 5 to 7 | Granola commits and replies as soon as the votes are in. Logged finalize is D1. Executing at finalize, out of timestamp order, is Granola's locking-mode rule: safe because the transaction's timestamp is above everything executed before it was prepared, and locks make every concurrent transaction commute with it (T §4.4.2). |
| 36 | A prepare that finds a key locked, or in the key set of an unexecuted transaction, votes conflict and takes no locks | Adapted: T §4.3.6 | Granola prepares, in timestamp order, every independent transaction that has voted but not executed before it prepares a coordinated one, and blocks the coordinated one on a conflict (T §4.3.6). D3. **Corrected:** document 04 checked locks only. |
| 37 | A single-shard or independent transaction touching a locked key waits | Ours | Granola answers CONFLICT in locking mode (T §4.3.5, "Impact on Other Transactions"). D3. **Corrected:** the waiting transaction takes its timestamp after the lock is released. |
| 38 | Locks are an internal LSM tree, replicated, surviving view changes | Adapted: T §5.2.1 | In Granola only the primary manages locks; backups do not (T §5.2.1), and neither source says how a new primary rebuilds them. We keep the lock table itself as replicated state, so nothing has to be rebuilt. |
| 39 | Replica failure is masked by VSR | Same: P §5.1, T §5.1 | Viewstamped Replication with 2f + 1 replicas. |
| 40 | A whole participant group unavailable: others hold pending state and locks; no abort after voting commit | Same: P §5.4, T §5.3 | |
| 41 | Before voting, a participant may time out and vote abort | Same: T §5.3.1 | Transactions a repository has not yet voted for can be aborted safely. Same reason as the refusal vote (rule 21). |
| 42 | Client failure is irrelevant after the first message reaches one participant | Same: T §5.2.2, §5.4 | See rule 20. |

### Clock coupling between groups

| # | Rule | Source | Note |
|---|---|---|---|
| 43 | Raise the hybrid-clock floor to the highest timestamp seen on every vote and finalize | Adapted: P §4.4 step 1, T §7.4 | Granola's proposals are above the last executed timestamp; with implicit votes a repository also raises its proposals for a participant to the highest vote received from it (T §7.4). We raise the floor whenever a timestamp is seen, which gives the strict "above anything seen" rule of T5.2.5. |
| 44 | Remote timestamp guard | Ours: D6 | |
| 45 | Clients send their highest seen timestamp on every request | Same: P §4.2, T §4.2.2, §4.5.1 | The client keeps the maximum timestamp of any reply (T §4.5.1). |
| 46 | Optional commit-wait for strict serializability | Adapted: T §4.4.3 | Granola does not provide external consistency. The thesis proves a violation needs out-of-band communication faster than the maximum clock skew between repositories, and notes it could be closed by delaying a new transaction until every independent transaction voted on before it has completed (T §4.4.3). Commit-wait closes the same window on the reply side. |

## 3. The seven deviations

### D1. Finalize as a logged op

- **Granola.** Each log message records the request, the proposed timestamp and the vote. The final timestamp is piggybacked on later log messages to the backups and is not itself a stable write; once the primary has every vote the transaction is ready and runs in timestamp order without a further stable write, and backups execute in timestamp order once they learn the final timestamp (P §5.1, §4.4 steps 5 and 6). The thesis gives the log entry as the request, the proposed timestamp, the status and a piggybacked list of final timestamps; backups keep it sorted by timestamp and execute the finalized prefix (T §5.2.1, Figure 5.2). After a failover the new primary re-sends a vote for every entry without a final timestamp to collect the others' (T §5.2.1).
- **Ours.** `finalize` is an op of its own, committed through VSR before the transaction executes.
- **Why.** Every replica must execute the transaction at the same op so that storage stays byte-identical (document 01) and checkpoints and state sync work by bytes. A new primary also knows every final timestamp from its log.
- **What could go wrong.** In Granola's locking mode a single-repository transaction runs on the primary before it is logged, so an old primary that missed a view change must roll back to a checkpoint (T §4.3.5, §5.2.1); since we execute only committed ops, that case does not exist here. One extra replication round before the reply, which is in the cost estimate of document 04. A finalize logged from an incomplete set of votes would give different final timestamps on different participants (invariant 2); the finalize op must carry or check the full vote set. The extra record also costs log space; finalize records batch (rule 2).

### D2. Votes derived from logged state

- **Granola.** The same idea: the vote is logged with the request; a new primary re-sends every vote that has no final timestamp, with a retry flag that asks the receivers to re-send theirs (T §5.2.1, appendix A.1.2); a repository keeps each request to forward it to a participant that never received it, and keeps its votes until every participant has executed (T §5.4); a repository that hears nothing from a participant re-sends its votes to the others, which answer with the outcome if they finished (T §5.3).
- **Ours.** The same mechanisms. The difference is where the state lives: the vote is recomputed from the pending table instead of being kept as a message copy, and forwarding uses the logged proposal.
- **Why.** The client is not needed after its first message reaches one participant, so a crashed client cannot leave transactions pending forever.
- **What could go wrong.** A vote sent before its propose op commits could be lost in a view change, and the new primary would propose a different timestamp; T5.2.3 asserts the re-sent vote equals the original. A forwarded proposal can reach a participant that already refused or finished the id; the logged refusal and the finished-transaction table answer it, and it must never be proposed again.

### D3. Conflict vote instead of waiting

- **Granola.** A coordinated prepare that cannot take its locks returns CONFLICT and the client retries after a random backoff (T §3.2.2, §4.3.5). When a coordinated transaction arrives in timestamp mode, the repository prepares, in timestamp order, every independent transaction it has voted on but not executed, and **blocks** the coordinated one until those prepares succeed (T §4.3.6). In locking mode, independent and single-repository transactions that hit a lock get CONFLICT (T §4.3.5).
- **Ours.** A coordinated prepare never waits: if any of its keys is locked or belongs to an unexecuted transaction, it votes conflict at once and takes no locks. Independent and single-shard transactions never get conflict; they wait for the lock, and take their timestamp only when it is released.
- **Why.** A coordinated prepare that never waits adds no waits-for edge, so no distributed deadlock can form, and the lock holder waits only on votes that other shards produce without queueing. Independent transactions must not fail, which is what lets the exchange settle without a retry path.
- **What could go wrong.**
  - *Stale `require`* (found in this reading, corrected): checking locks alone, a coordinated prepare can read state that a pending independent transaction on the same key has not yet changed, and that transaction may end up ordered first. Example: a pending settlement debits a balance; a coordinated withdrawal checks the balance before the debit executes, and both commit. Granola avoids this by preparing the in-flight independent transactions first (T §4.3.6); we vote conflict.
  - *Timestamp below the lock holder* (corrected): a transaction that took its timestamp while waiting for a lock can be ordered before the holder and still read its writes. Taking the timestamp at release puts it after the holder's final timestamp.
  - *Starvation*: on a key with constant independent traffic, a coordinated prepare can conflict every time. Client backoff (T5.2.7) is the only remedy now. Coordinated transactions are meant for low-rate work (document 03); if they starve in practice, a reservation that holds new independent work on the key for a bounded time is the next step.

### D4. Conflict-aware bypass

- **Granola.** In timestamp mode every transaction runs in timestamp order (T §4.3.4). When a participant stays unresponsive, the repository switches to locking mode: it prepares the blocked and incomplete transactions in timestamp order, executes at once each blocked one whose locks do not conflict, and uses `forcePrepare` to take each transaction's lock-superset, the locks it could need in any serial order (T §5.3.1). This is safe because an incomplete transaction later in the queue finalizes at or above its proposal and so stays later, and the executed one conflicts with nothing that may end up ahead of it (T §5.3.1). The thesis proposes the lighter form we use, locking only the blocked transaction and letting non-conflicting work pass it, as future work (T §9.1.1).
- **Ours.** Always on, not a failure mode: a transaction whose declared key set intersects no unexecuted transaction ordered before it executes at once.
- **Why.** Almost every transaction crosses shards, so one slow or dead group would otherwise stall every shard that has a transaction pending with it (document 04, failure isolation). Static key sets are exactly the lock-superset `forcePrepare` has to compute, so the check is exact and cheap and needs no mode switch. The thesis's first recovery design searched all orderings, O(n!) in the incomplete transactions (T §5.3.1); static keys avoid that.
- **What could go wrong.**
  - *Overtaking a queued transaction* (corrected): document 04 checked only pending key sets. A transaction finalized and waiting in the queue behind an older pending one is just as unexecuted; overtaking it on a shared key breaks timestamp order. The thesis's recovery covers both, incomplete and blocked (T §5.3.1).
  - *Incomplete key sets*: a key the transaction reads or writes but does not declare (a derived-table row, an index entry) makes the check pass when it should not, and serializability breaks silently. The declared set must be computed by the compiler, never written by hand.
  - *Snapshot reads*: a bypassed transaction executes above `shard_safe_ts`, so reads at or below it are unaffected; the safe timestamp must not be advanced past a bypassed transaction's timestamp while older transactions are still pending.

### D5. Finished-transaction table

- **Granola.** A TID is the client id plus a sequence number (T §4.2.1). A client keeps TIDs unique after a crash with sequence-number leases written to disk, and recovers its highest timestamp by waiting out the maximum clock skew or with timestamp leases (T §5.2.2). A repository keeps recent replies in a circular buffer covering a time window; a client away for longer loses the reply (T §5.4). Neither source says what stops a duplicate that arrives after its reply was dropped from executing again.
- **Ours.** A client-chosen time-ordered id; each participant keeps finished ids and outcomes for one hour of commit time and answers duplicates from it; an older id gets `too_old`.
- **Why.** Thousands of clients send cross-shard work to many groups; per-client sequence state in every group would be large and need its own eviction. A time-ordered id makes "older than the window" decidable from the id alone, so the table can be bounded and a late duplicate is refused instead of executed.
- **What could go wrong.** The id's time comes from the client's clock: a clock far behind makes fresh ids look old (`too_old` on a first attempt), far ahead is caught by the guard (D6). `too_old` must be presented to the application as "outcome unknown", not "failed". A duplicate that arrives while the original is pending must attach to it, not propose again (T5.4.5).

### D6. Remote timestamp guard

- **Granola.** Clock values serve performance only; correctness comes from clients propagating timestamps (T §3.3, §4.1). The exceptions are one client-recovery option and the primary leases, which need loosely synchronised clock values and rates (T §5.2.1, §5.2.2). There is no guard. Every proposal is above the client's highTS (T §4.3.4), so one far-future timestamp is inherited by every repository the client or transaction touches.
- **Ours.** A timestamp from a client or another group more than 100 ms ahead of the receiver's synchronised clock is rejected.
- **Why.** With hybrid-clock floors one bad timestamp spreads through votes and client timestamps to the whole cluster and cannot be undone, since timestamps never go back.
- **What could go wrong.** Rejecting a vote after this participant has voted would leave the transaction committed on others and missing here. So (clarified in document 04) client timestamps are checked before propose, a vote that arrives before this participant proposes is refused with a refusal vote, and a vote after it has voted leaves the transaction pending until this participant's own clock is within the bound of that timestamp, then finalizes it, and reports the sender. That last case needs a whole group's clock to be wrong, because a primary outside the quorum's clock interval does not assign timestamps (document 02 section 6). A bound too tight for the real clock discipline would reject healthy traffic; 100 ms is about a hundred times the expected skew.

### D7. Fail-fast `unavailable`

- **Granola.** When a whole repository is down, the others stall: they re-send votes, hold locks in locking mode, and in timestamp mode switch to locking mode to make what progress they can (T §5.3). Transactions not yet voted on may be aborted (T §5.3.1), and a new primary refuses new work during its view change (T §5.2.1), but neither source refuses new transactions because they name a dead repository. The thesis also suggests giving transactions with a slow participant higher timestamps so that others pass them (T §9.1.1).
- **Ours.** Clients and shards track group reachability; a new transaction naming an unreachable group is rejected with `unavailable` before it is proposed.
- **Why.** Every transaction accepted for a dead group becomes pending somewhere, holds back `shard_safe_ts` there and widens the set of keys that cannot be bypassed. Refusing early keeps the damage to what was already in flight.
- **What could go wrong.** The client and a participant can disagree about reachability. If the client sends and one participant refuses after others have proposed, those others would wait forever for a vote; the refusal vote (rule 21) ends the transaction on all of them. A short view change must not be taken for an outage, hence hysteresis (T5.4.4). Transactions already pending still wait (rule 30); fail-fast does not help them.

## 4. Proof obligations of timestamp mode

The thesis argues serializability in T §4.4.2 (the paper in P §4.6), resting on the rules of T §4.1 to §4.3 and the replication rules of T §5.2.1; the only formal proof in either source is the external-consistency window of T §4.4.3 (O11). These are the conditions the argument needs, each with the Granite mechanism that meets it and where that is checked. "Invariant n" is document 04 section 6.

| # | Obligation | Source | Granite mechanism | Checked by |
|---|---|---|---|---|
| O1 | Timestamps define a total order on transactions; equal timestamps are ordered by transaction id | P §4.1, T §4.1 | Order is `(timestamp, transaction id)` (document 04, independent transaction) | Invariant 4 (global serializability checker, T5.4.1) |
| O2 | Every participant executes a transaction at the same timestamp | T §4.3.4 step 5, §4.4.2 | Final timestamp is the maximum of the logged proposals, so it is the same wherever the full vote set is known; finalize is logged (D1) and must be built from the full vote set | Invariant 2; T5.2.2 gate on identical final timestamps |
| O3 | Each repository executes in timestamp order: a transaction runs only after every transaction with a lower timestamp that could still reach that repository | T §4.3.3 step 3, §4.3.4 last paragraph, §4.4.2 | `shard_safe_ts` and the timestamp-order queue; the bypass may reorder only transactions with disjoint key sets, including queued ones (D4) | Invariant 3; T5.2.1 queue-order assertion; T5.4.3 equivalence of bypass and strict order |
| O4 | A proposal is above every timestamp the repository has executed, so the final timestamp, which is at least the proposal, cannot fall below anything already executed there | T §4.3.4 step 1, §5.3.1 | Proposal above every timestamp issued, which includes every executed one; final is the maximum vote | Invariant 3; T5.2.2 gate "final at least every proposal" |
| O5 | A proposal is above the client's highTS, so a transaction is ordered after everything its client has observed | T §4.3.4 step 1, §4.4.2 | Proposal strictly above `client_ts`; clients send the highest timestamp seen; floor raised on every vote and finalize | T5.2.5 causal-order checker and the injected bug that drops the client timestamp |
| O6 | After a client sees T at one participant, its later requests execute after T at every participant, even one that has not executed T yet | T §4.4.2 (example of Figure 4.9) | Same as O5: the later request carries `client_ts` at least T's timestamp, so its proposal is above T's and it queues behind T where T is unexecuted | T5.2.5 causal-order checker |
| O7 | The request and proposed timestamp are stable before the vote is sent, so a new primary never sends a different proposal for the same transaction | T §4.3.4 step 2, §5.2.1 (view changes) | The vote is derived from the committed propose op (D2) | T5.2.3 gate that a re-sent vote carries the original timestamp, with its injected bug |
| O8 | Backups execute the same transactions in the same order as the primary | T §5.2.1 (logging) | Execution happens only by executing committed ops (finalize, then queue release), so every replica executes at the same op (D1) | T5.2.2 convergence gate; T5.2.1 convergence checker |
| O9 | A read served by a primary that has been replaced must not return stale data | T §5.2.1 (single primary), §7.2 | Not by leases: linearizable reads wait for a replication quorum to acknowledge a later message (document 02 section 8); snapshot reads wait for `shard_safe_ts` (document 04 section 5) | Invariant 5; replication tests of document 02 |
| O10 | A transaction executed out of timestamp order must not conflict with any transaction it overtakes | T §4.4.2, §4.3.5, §5.3.1 | Used in two places: a coordinated transaction executes at finalize while holding locks on its keys, and no unexecuted transaction can share them (D3); the bypass checks key sets (D4) | T5.1.2 model (bypass and conflict-vote injected bugs); T5.4.3 |
| O11 | Without commit-wait, external consistency can fail only when out-of-band communication between clients is faster than the maximum clock skew between repositories (proved) | T §4.4.3 | The window is bounded by the clock discipline (document 02 section 0) and the remote timestamp guard (D6) keeps timestamps from running far ahead; optional commit-wait closes it per transaction | T5.3.3 strictness checker with commit-wait enabled |

O10 belongs to Granola's locking mode and recovery, and O11 to external consistency; neither is a timestamp-mode rule. Both are listed because we rely on them: O10 without the mode switch, O11 for commit-wait.

## 5. The open points of document 04 section 8

1. **Granola's vote exchange path, recovery rules and proof obligations.** Answered from the paper and the thesis.
   - Path: votes go directly from each participant's primary to the other participants' primaries; there is no coordinator, and the client only sends the request and collects replies (T §4.3.4, §9.2; P Figures 6 and 7). Document 04 already had this.
   - Recovery: the vote is logged with the request; a new primary refuses new work while it re-sends every vote that has no final timestamp (T §5.2.1); requests and votes are kept so that a missing participant can be sent the request and a missing vote re-sent (T §5.4); a repository that hears nothing from a participant re-sends its votes to the others, which answer with the outcome if they finished; in timestamp mode it then takes lock-supersets to execute what does not conflict (T §5.3.1). Our versions are D2 and D4.
   - Proof obligations: section 4 above.
2. **Can classification be fully automatic from the IR?** Yes. The thesis leaves this open because Granola does not interpret operations, and expects it to be possible for transactions with a well-defined syntax over a schema (T §9.1.2); registered programs are that case. Every rule that decides the class is a property of the program's IR: whether keys are static, whether any `require` is present, whether any write can fail, whether a global unique index is touched. The classifier must be conservative: a statement it cannot prove infallible makes the program coordinated, which costs speed, never correctness. The `independent` annotation does not decide anything; it only makes compilation fail if the derived class changes. Both are already task T5.2.6, so no new task is needed. Two cases the classifier must treat as fallible: arithmetic that can return a typed error (overflow, division by zero; document 03 section 2), and an insert into an immutable table whose key is not fixed by the transaction, which can return `exists_different` (document 03 section 9). See finding F5.
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
| F7 | T5.2.9 | Not a disagreement: the thesis's implicit votes (T §7.4) let a vote from a participant stand in for that participant's later proposals, so a finalized transaction need not wait behind later transactions with lower proposals. Our proposals already rise monotonically, which is one of its two requirements; the other is in-order vote delivery per participant pair, with logged sequence numbers | Consider it when batching votes per group pair; measure first |
