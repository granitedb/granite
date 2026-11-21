---------------------------- MODULE StandbySite ----------------------------
(***************************************************************************)
(* The asynchronous standby site of document 04 section 7: consistent     *)
(* apply at the timestamp Ta, promotion, and epoch fencing of the old     *)
(* site. Invariant 8 of document 04 section 6 is checked.                 *)
(*                                                                         *)
(* Site A is the primary site. Each of its groups has a committed log of   *)
(* the ops of document 04 section 4 for independent cross-shard           *)
(* transactions: a "prop" op with the proposed timestamp, then a "fin" op *)
(* with the final timestamp, the maximum of the proposals of all          *)
(* participants. Site B is the standby site: each group's remote standby  *)
(* receives a prefix of that log, op by op and at its own pace, and makes *)
(* what it received durable. A coordinator computes Ta from the durable   *)
(* prefixes; each group executes the transactions finalized up to Ta.     *)
(* Promotion freezes Ta and moves the site to epoch 2; each group then    *)
(* executes up to Ta, discards the rest and becomes a voting group, which *)
(* takes new transactions when NewWork is TRUE. Site A keeps running at   *)
(* epoch 1 throughout.                                                    *)
(***************************************************************************)
EXTENDS Integers, Sequences

CONSTANTS
    Groups,      \* the groups
    Txns,        \* transaction ids, positive integers: equal timestamps are ordered by id
    Parts,       \* [Txns -> SUBSET Groups]: the participants of each transaction
    MaxCrashes,  \* crashes of standby groups, in all
    Unflushed,   \* ops a standby may hold received but not yet durable
    NewWork,     \* TRUE: site B takes new transactions after promotion
    Defect       \* "none", or the injected defect to run (see README)

Max(S) == CHOOSE x \in S : \A y \in S : y <= x
Min(S) == CHOOSE x \in S : \A y \in S : x <= y

\* Transactions are ordered by (timestamp, transaction id).
Before(ts1, t1, ts2, t2) == ts1 < ts2 \/ (ts1 = ts2 /\ t1 < t2)

VARIABLES
    plog,     \* [Groups -> Seq(Op)]: site A's committed log of each group
    recv,     \* [Groups -> Nat]: length of the prefix of plog[g] the standby received
    dur,      \* [Groups -> Nat]: length of the prefix it made durable, <= recv
    ta,       \* the apply timestamp Ta, persisted by the coordinator
    gta,      \* [Groups -> Nat]: the timestamp each standby group has executed up to
    done,     \* [Groups -> Seq(Txns)]: transactions the standby executed, in order
    phase,    \* "standby", or "promoting" once the promotion command has frozen Ta
    pg,       \* the groups that have been promoted
    blog,     \* [Groups -> Seq(Op)]: site B's log of a promoted group
    crashes   \* standby crashes so far

vars == <<plog, recv, dur, ta, gta, done, phase, pg, blog, crashes>>

\* The checked scenario: three transactions in a ring over three groups, so
\* that every group takes part in two of them.
SParts == <<{1, 2}, {2, 3}, {3, 1}>>

(* An op carries the epoch it was made in: 1 at site A, 2 at site B after  *)
(* promotion. A finalize made from a vote of epoch 1 has epoch 1.          *)
Op(type, t, ts, ep) == [type |-> type, t |-> t, ts |-> ts, ep |-> ep]

Ops(L) == {L[i] : i \in 1..Len(L)}
HasProp(L, t) == \E o \in Ops(L) : o.type = "prop" /\ o.t = t
HasFin(L, t) == \E o \in Ops(L) : o.type = "fin" /\ o.t = t
PropOf(L, t) == CHOOSE o \in Ops(L) : o.type = "prop" /\ o.t = t
FinOf(L, t) == CHOOSE o \in Ops(L) : o.type = "fin" /\ o.t = t
\* The clock floor: every timestamp the group has issued or seen.
Floor(L) == IF L = <<>> THEN 0 ELSE Max({o.ts : o \in Ops(L)})
Seen(E) == {E[i] : i \in 1..Len(E)}

(* shard_safe_ts of a log (document 04 section 4): one below the oldest     *)
(* pending proposal, or the floor if nothing is pending. No transaction     *)
(* can be finalized on that group at or below it after that log.            *)
SafeTs(L) ==
    LET pending == {t \in Txns : HasProp(L, t) /\ ~HasFin(L, t)}
    IN IF pending = {} THEN Floor(L) ELSE Min({PropOf(L, t).ts : t \in pending}) - 1

\* The transactions finalized in L at or below T: the group's state at T.
FinBelow(L, T) == {t \in Txns : HasFin(L, t) /\ FinOf(L, t).ts <= T}

Prefix(g, n) == SubSeq(plog[g], 1, n)
Dur(g) == Prefix(g, dur[g])

\* Ta: the minimum over groups of the safe timestamp of the prefix n[g].
MinSafe(n) == Min({SafeTs(Prefix(g, n[g])) : g \in Groups})

\* E followed by the transactions of S in (timestamp, id) order, timestamps from L.
RECURSIVE AddSorted(_, _, _)
AddSorted(E, S, L) ==
    IF S = {} THEN E
    ELSE LET m == CHOOSE t \in S : \A u \in S :
                      ~Before(FinOf(L, u).ts, u, FinOf(L, t).ts, t)
         IN AddSorted(Append(E, m), S \ {m}, L)

\* The standby of g executes, from its durable log, everything finalized up to T.
ExecTo(g, T) == AddSorted(done[g], FinBelow(Dur(g), T) \ Seen(done[g]), Dur(g))

-----------------------------------------------------------------------------
(* Site A. Votes are derived from committed prop ops (document 04a D2), so  *)
(* a participant finalizes once every participant's prop is in its log.    *)

Propose(g, t) ==
    /\ g \in Parts[t]
    /\ ~HasProp(plog[g], t)
    /\ plog' = [plog EXCEPT ![g] = Append(@, Op("prop", t, Floor(@) + 1, 1))]
    /\ UNCHANGED <<recv, dur, ta, gta, done, phase, pg, blog, crashes>>

Finalize(g, t) ==
    /\ g \in Parts[t]
    /\ ~HasFin(plog[g], t)
    /\ \A h \in Parts[t] : HasProp(plog[h], t)
    /\ plog' = [plog EXCEPT ![g] =
                  Append(@, Op("fin", t, Max({PropOf(plog[h], t).ts : h \in Parts[t]}), 1))]
    /\ UNCHANGED <<recv, dur, ta, gta, done, phase, pg, blog, crashes>>

-----------------------------------------------------------------------------
(* Site B before promotion. *)

(* The next op reaches the standby of g, at any time: shipping delays are   *)
(* arbitrary and differ per group. Once the promotion command has run, the  *)
(* site is at epoch 2 and rejects shipments of epoch 1.                     *)
Ship(g) ==
    /\ phase = "standby"
    /\ recv[g] < Len(plog[g])
    /\ recv' = [recv EXCEPT ![g] = @ + 1]
    \* At most Unflushed ops are held received but not durable.
    /\ dur' = [dur EXCEPT ![g] = Max({@, recv'[g] - Unflushed})]
    /\ UNCHANGED <<plog, ta, gta, done, phase, pg, blog, crashes>>

\* The standby writes what it received to its WAL.
Flush(g) ==
    /\ g \notin pg
    /\ dur[g] < recv[g]
    /\ dur' = [dur EXCEPT ![g] = recv[g]]
    /\ UNCHANGED <<plog, recv, ta, gta, done, phase, pg, blog, crashes>>

\* A standby crashes and loses what it had not made durable; shipping resumes after it.
Crash(g) ==
    /\ g \notin pg
    /\ crashes < MaxCrashes
    /\ dur[g] < recv[g]
    /\ recv' = [recv EXCEPT ![g] = dur[g]]
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<plog, dur, ta, gta, done, phase, pg, blog>>

(* The coordinator recomputes Ta from the groups' durable prefixes and      *)
(* persists it. It takes the computed value as it is, so that a decrease is *)
(* visible to TaMonotone rather than hidden by a maximum.                   *)
ComputeTa ==
    LET v == MinSafe(IF Defect = "ta_from_received" THEN recv ELSE dur)
    IN /\ phase = "standby"
       /\ v # ta
       /\ ta' = v
       /\ UNCHANGED <<plog, recv, dur, gta, done, phase, pg, blog, crashes>>

\* A standby group learns Ta and executes up to it. Groups do so at their own pace.
Apply(g) ==
    LET T == IF Defect = "apply_on_arrival" THEN SafeTs(Dur(g)) ELSE ta
    IN /\ g \notin pg
       /\ gta[g] < T
       /\ gta' = [gta EXCEPT ![g] = T]
       /\ done' = [done EXCEPT ![g] = ExecTo(g, T)]
       /\ UNCHANGED <<plog, recv, dur, ta, phase, pg, blog, crashes>>

-----------------------------------------------------------------------------
(* Promotion. *)

\* The manual command, at any moment: Ta is frozen and the site takes epoch 2.
Promote ==
    /\ phase = "standby"
    /\ phase' = "promoting"
    /\ UNCHANGED <<plog, recv, dur, ta, gta, done, pg, blog, crashes>>

(* Each group, at its own pace, executes up to Ta and discards every op of  *)
(* a transaction it did not execute. Its log keeps the executed ones and a  *)
(* "promote" op that raises its clock floor to Ta, and it becomes a voting  *)
(* group at epoch 2.                                                        *)
PromoteGroup(g) ==
    LET T == IF Defect = "no_discard" THEN Floor(Dur(g)) ELSE ta
        E == ExecTo(g, T)
    IN /\ phase = "promoting"
       /\ g \notin pg
       /\ done' = [done EXCEPT ![g] = E]
       /\ gta' = [gta EXCEPT ![g] = Max({@, T})]
       /\ pg' = pg \cup {g}
       /\ blog' = [blog EXCEPT ![g] =
                     Append(SelectSeq(Dur(g), LAMBDA o : o.t \in Seen(E)),
                            Op("promote", 0, T, 2))]
       /\ UNCHANGED <<plog, recv, dur, ta, phase, crashes>>

-----------------------------------------------------------------------------
(* Site B after promotion: new transactions, or retries of discarded ones. *)

BPropose(g, t) ==
    /\ NewWork
    /\ g \in pg
    /\ g \in Parts[t]
    /\ ~HasProp(blog[g], t)
    /\ blog' = [blog EXCEPT ![g] = Append(@, Op("prop", t, Floor(@) + 1, 2))]
    /\ UNCHANGED <<plog, recv, dur, ta, gta, done, phase, pg, crashes>>

(* The votes for t that a promoted group accepts from h: h's, if h is       *)
(* promoted; a standby never votes. Fencing rejects every vote of epoch 1;  *)
(* the defect "no_fence" also accepts the vote of h at site A.              *)
VotesFrom(h, t) ==
    (IF h \in pg /\ HasProp(blog[h], t)
     THEN {[ts |-> PropOf(blog[h], t).ts, ep |-> PropOf(blog[h], t).ep]} ELSE {})
    \cup
    (IF Defect = "no_fence" /\ HasProp(plog[h], t)
     THEN {[ts |-> PropOf(plog[h], t).ts, ep |-> 1]} ELSE {})

BFinalize(g, t) ==
    LET others == Parts[t] \ {g}
        own == PropOf(blog[g], t)
    IN /\ g \in pg
       /\ HasProp(blog[g], t)
       /\ ~HasFin(blog[g], t)
       /\ \E f \in [others -> UNION {VotesFrom(h, t) : h \in others}] :
             /\ \A h \in others : f[h] \in VotesFrom(h, t)
             /\ blog' = [blog EXCEPT ![g] = Append(@,
                           Op("fin", t, Max({own.ts} \cup {f[h].ts : h \in others}),
                              Min({own.ep} \cup {f[h].ep : h \in others})))]
       /\ UNCHANGED <<plog, recv, dur, ta, gta, done, phase, pg, crashes>>

-----------------------------------------------------------------------------

Init ==
    /\ plog = [g \in Groups |-> <<>>]
    /\ recv = [g \in Groups |-> 0]
    /\ dur = [g \in Groups |-> 0]
    /\ ta = 0
    /\ gta = [g \in Groups |-> 0]
    /\ done = [g \in Groups |-> <<>>]
    /\ phase = "standby"
    /\ pg = {}
    /\ blog = [g \in Groups |-> <<>>]
    /\ crashes = 0

\* Every transaction is finalized at site A and, after promotion, at site B.
Finished ==
    /\ pg = Groups
    /\ \A t \in Txns, g \in Groups :
          g \in Parts[t] => HasFin(plog[g], t) /\ (NewWork => HasFin(blog[g], t))

\* Stuttering once finished, so that TLC's deadlock check reports only states
\* where unfinished work can make no progress.
Next ==
    \/ \E g \in Groups, t \in Txns :
          \/ Propose(g, t) \/ Finalize(g, t) \/ BPropose(g, t) \/ BFinalize(g, t)
    \/ \E g \in Groups :
          \/ Ship(g) \/ Flush(g) \/ Crash(g) \/ Apply(g) \/ PromoteGroup(g)
    \/ ComputeTa
    \/ Promote
    \/ Finished /\ UNCHANGED vars

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Properties *)

TypeOK ==
    /\ \A g \in Groups : 0 <= dur[g] /\ dur[g] <= recv[g] /\ recv[g] <= Len(plog[g])
    /\ phase \in {"standby", "promoting"}
    /\ pg \subseteq Groups /\ (pg # {} => phase = "promoting")
    /\ crashes <= MaxCrashes

(* The state of site A at T, every group included. It is complete only at a *)
(* T no group can still finalize a transaction at or below.                 *)
ACompleteTs == MinSafe([g \in Groups |-> Len(plog[g])])

(* Each standby group's executed state, at every moment, is site A's state  *)
(* on that group at the timestamp it executed up to, in timestamp order.    *)
(* Groups execute Ta at their own pace, so the whole standby site is at one *)
(* timestamp for reads at or below the lowest of them (document 04 section  *)
(* 5), and at Ta once promoted.                                             *)
ApplyIsAState ==
    \A g \in Groups :
        /\ Seen(done[g]) = FinBelow(plog[g], gta[g])
        /\ \A i, j \in 1..Len(done[g]) :
               i < j => Before(FinOf(plog[g], done[g][i]).ts, done[g][i],
                               FinOf(plog[g], done[g][j]).ts, done[g][j])

(* Invariant 8: the promoted standby site holds site A's state at one single *)
(* timestamp: no cross-shard transaction is present on some groups and       *)
(* absent on others.                                                         *)
Inv8 ==
    pg = Groups =>
        \E T \in 0..ACompleteTs :
            \A g \in Groups : Seen(done[g]) = FinBelow(plog[g], T)

\* And that timestamp is Ta, the one the promotion reports (T7.4.4).
PromotedAtTa ==
    \A g \in pg : Seen(done[g]) = FinBelow(plog[g], ta)

(* Fencing: after promotion, nothing site A commits is accepted by site B.  *)
(* Every op of epoch 1 in a promoted group's log was committed at site A    *)
(* before promotion and is at or below Ta; every other op was made at site B *)
(* from votes of site B only.                                               *)
Fenced ==
    \A g \in pg : \A o \in Ops(blog[g]) : o.ep = 1 => o.t \in Seen(done[g])

\* Invariant 2 at site B: its groups finalize a transaction at the same timestamp.
SameTsB ==
    \A t \in Txns : \A g, h \in pg :
        HasFin(blog[g], t) /\ HasFin(blog[h], t) =>
            FinOf(blog[g], t).ts = FinOf(blog[h], t).ts

\* Site B orders every transaction it takes after promotion above Ta.
NewAfterTa ==
    \A g \in pg : \A o \in Ops(blog[g]) :
        o.ep = 2 /\ o.type \in {"prop", "fin"} => o.ts > ta

\* Ta never decreases.
TaMonotone == [][ta' >= ta]_ta

=============================================================================
