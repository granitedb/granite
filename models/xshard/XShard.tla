------------------------------- MODULE XShard -------------------------------
(***************************************************************************)
(* Cross-shard transactions of document 04 section 4: independent          *)
(* transactions (propose, vote, finalize, execute), coordinated            *)
(* transactions (prepare with locks, vote, finalize), timestamp-order      *)
(* execution, the conflict-aware bypass, the conflict vote, and vote       *)
(* retransmission after a view change. Invariants 1 to 4 of document 04    *)
(* section 6 and absence of deadlock are checked.                          *)
(*                                                                         *)
(* A group is one replicated log. Its primary appends ops and takes every  *)
(* decision (timestamp, vote, lock) from its whole log, as the request,    *)
(* vote and proposed timestamp are logged together (document 04a rule 33). *)
(* Votes and execution use only the committed prefix, which never rolls   *)
(* back. A view change replaces the primary: the new one keeps the        *)
(* committed prefix only, so the uncommitted suffix and every vote the old *)
(* primary had received are lost.                                          *)
(***************************************************************************)
EXTENDS Integers, Sequences

CONSTANTS
    Groups,          \* the groups
    Txns,            \* transaction ids, integers: equal timestamps are ordered by id
    Kind,            \* [Txns -> {"ind", "coord"}]
    Keys,            \* [Txns -> SUBSET DOMAIN Owner]: the declared key set
    Owner,           \* [key -> Groups]: the group that owns each key
    Cap,             \* [Txns -> Nat]: a coordinated transaction requires fewer
                     \* than Cap[t] earlier writes on each of its keys
    MaxViewChanges,  \* view changes per group
    Bypass,          \* TRUE: conflict-aware bypass; FALSE: strict order
    Defect           \* "none", or the injected defect to run (see README)

KeySet == DOMAIN Owner
Parts(t) == {Owner[k] : k \in Keys[t]}
Local(g, t) == {k \in Keys[t] : Owner[k] = g}

Max(S) == CHOOSE x \in S : \A y \in S : y <= x
Min(S) == CHOOSE x \in S : \A y \in S : x <= y

\* Transactions are ordered by (timestamp, transaction id).
Before(ts1, t1, ts2, t2) == ts1 < ts2 \/ (ts1 = ts2 /\ t1 < t2)

VARIABLES
    log,    \* [Groups -> Seq(Op)]: the group's log
    ci,     \* [Groups -> Nat]: commit index
    views,  \* [Groups -> Nat]: view changes so far
    got,    \* [Groups -> set of votes]: votes the primary has received (volatile)
    exec    \* [Groups -> Seq(Txns)]: transactions executed, in execution order

vars == <<log, ci, views, got, exec>>

(* Ops. "prop" is propose (independent) or prepare (coordinated); its vote  *)
(* is "commit", "abort" or "conflict". "fin" is finalize.                   *)
PropOp(t, ts, v) == [type |-> "prop", t |-> t, ts |-> ts, vote |-> v]
FinOp(t, ts, ok) == [type |-> "fin", t |-> t, ts |-> ts, ok |-> ok]

Ops(L) == {L[i] : i \in 1..Len(L)}
HasProp(L, t) == \E o \in Ops(L) : o.type = "prop" /\ o.t = t
HasFin(L, t) == \E o \in Ops(L) : o.type = "fin" /\ o.t = t
PropOf(L, t) == CHOOSE o \in Ops(L) : o.type = "prop" /\ o.t = t
FinOf(L, t) == CHOOSE o \in Ops(L) : o.type = "fin" /\ o.t = t
\* The clock floor: every timestamp the group has issued or seen in a finalize.
Floor(L) == IF L = <<>> THEN 0 ELSE Max({o.ts : o \in Ops(L)})

Committed(g) == SubSeq(log[g], 1, ci[g])
Seen(E) == {E[i] : i \in 1..Len(E)}
Executed(g) == Seen(exec[g])

\* Proposed or prepared in L, not in the executed sequence E, and not aborted:
\* pending, or finalized and queued.
Unexec(L, E) ==
    {t \in Txns : /\ HasProp(L, t)
                  /\ t \notin Seen(E)
                  /\ ~(HasFin(L, t) /\ ~FinOf(L, t).ok)}

\* Keys locked by a coordinated transaction that voted commit and is not finalized.
Locked(g, L) ==
    UNION {Local(g, t) : t \in {u \in Txns : /\ HasProp(L, u)
                                             /\ Kind[u] = "coord"
                                             /\ PropOf(L, u).vote = "commit"
                                             /\ ~HasFin(L, u)}}

\* The executed writes of key k, oldest first.
Hist(k) == SelectSeq(exec[Owner[k]], LAMBDA t : k \in Keys[t])

Require(g, t) == \A k \in Local(g, t) : Len(Hist(k)) < Cap[t]

-----------------------------------------------------------------------------
(* Actions *)

(* The request reaches the primary of g, from the client or forwarded by a  *)
(* participant. Its timestamp is above every timestamp in the log.           *)
Propose(g, t) ==
    LET L == log[g]
        ts == Floor(L) + 1
        busy == IF Defect = "conflict_locks_only" THEN Locked(g, L)
                ELSE UNION {Local(g, u) : u \in Unexec(L, exec[g])}
        conflict == Local(g, t) \cap busy # {}
    IN
    /\ g \in Parts(t)
    /\ ~HasProp(L, t)
    /\ IF Kind[t] = "ind"
       \* An independent transaction waits while a key is locked, and takes
       \* its timestamp when the lock is released.
       THEN /\ Local(g, t) \cap Locked(g, L) = {}
            /\ log' = [log EXCEPT ![g] = Append(L, PropOp(t, ts, "commit"))]
       \* A coordinated prepare never waits: it votes conflict and takes no lock.
       ELSE /\ Defect = "prepare_waits" => ~conflict
            /\ log' = [log EXCEPT ![g] = Append(L,
                         PropOp(t, ts, IF conflict THEN "conflict"
                                       ELSE IF Require(g, t) THEN "commit"
                                       ELSE "abort"))]
    /\ UNCHANGED <<ci, views, got, exec>>

\* g's vote for t, derived from its committed prop op.
VoteOf(g, t) ==
    LET o == PropOf(Committed(g), t)
    IN [t |-> t, from |-> g, ts |-> o.ts, vote |-> o.vote]

(* g's vote reaches the primary of h. Sending again until h has it covers     *)
(* loss, duplicates, and retransmission after a view change of either group. *)
DeliverVote(g, t, h) ==
    /\ h \in Parts(t) \ {g}
    /\ HasProp(Committed(g), t)
    \* Earlier delivery adds nothing: h finalizes only after its own prop commits.
    /\ HasProp(Committed(h), t)
    /\ ~HasFin(log[h], t)
    /\ VoteOf(g, t) \notin got[h]
    /\ got' = [got EXCEPT ![h] = @ \cup {VoteOf(g, t)}]
    /\ UNCHANGED <<log, ci, views, exec>>

(* With every vote in: final timestamp is the maximum vote; a coordinated  *)
(* transaction commits only if every participant voted commit.             *)
Finalize(g, t) ==
    LET V == {VoteOf(g, t)} \cup {v \in got[g] : v.t = t}
        tss == {v.ts : v \in V}
        ts == IF Defect = "finalize_below_vote" THEN Min(tss) ELSE Max(tss)
    IN
    /\ HasProp(Committed(g), t)
    /\ ~HasFin(log[g], t)
    /\ Defect = "finalize_early" \/ {v.from : v \in V} = Parts(t)
    /\ log' = [log EXCEPT ![g] =
                 Append(@, FinOp(t, ts, \A v \in V : v.vote = "commit"))]
    /\ got' = [got EXCEPT ![g] = {v \in @ : v.t # t}]
    /\ UNCHANGED <<ci, views, exec>>

\* u, not executed, may still be ordered before the finalized t, in the committed log C.
Blocks(g, C, t, u) ==
    LET queued == HasFin(C, u)
        ts == IF queued THEN FinOf(C, u).ts ELSE PropOf(C, u).ts
    IN
    /\ Before(ts, u, FinOf(C, t).ts, t)
    /\ Bypass => /\ Local(g, u) \cap Local(g, t) # {}
                 /\ ~(Defect = "bypass_pending" /\ ~queued)
                 /\ ~(Defect = "bypass_queued" /\ queued)

(* A committed finalize of t can execute when nothing that can be ordered  *)
(* before t is left (strict order), or nothing such that shares a key with *)
(* t (bypass). A coordinated commit holds its keys locked until finalize   *)
(* and executes at once (document 04a rule 35).                            *)
Ready(g, C, E, t) ==
    /\ HasFin(C, t) /\ FinOf(C, t).ok
    /\ t \notin Seen(E)
    /\ Kind[t] = "ind" => \A u \in Unexec(C, E) : ~Blocks(g, C, t, u)

(* Execution is deterministic on committed state: after each committed op,  *)
(* every ready transaction executes, in (timestamp, id) order.              *)
RECURSIVE Drain(_, _, _)
Drain(g, C, E) ==
    LET R == {t \in Txns : Ready(g, C, E, t)}
        first == CHOOSE t \in R : \A u \in R :
                     ~Before(FinOf(C, u).ts, u, FinOf(C, t).ts, t)
    IN IF R = {} THEN E ELSE Drain(g, C, Append(E, first))

\* An op commits, and whatever it makes ready executes.
Commit(g) ==
    /\ ci[g] < Len(log[g])
    /\ ci' = [ci EXCEPT ![g] = @ + 1]
    /\ exec' = [exec EXCEPT ![g] = Drain(g, SubSeq(log[g], 1, ci[g] + 1), @)]
    /\ UNCHANGED <<log, views, got>>

(* The primary is replaced: the uncommitted suffix and received votes are lost. *)
ViewChange(g) ==
    /\ views[g] < MaxViewChanges
    /\ Len(log[g]) > ci[g] \/ got[g] # {}   \* a view change that loses nothing changes nothing
    /\ views' = [views EXCEPT ![g] = @ + 1]
    /\ log' = [log EXCEPT ![g] = Committed(g)]
    /\ got' = [got EXCEPT ![g] = {}]
    /\ UNCHANGED <<ci, exec>>

Finished(t) ==
    \A g \in Parts(t) :
        /\ HasFin(Committed(g), t)
        /\ FinOf(Committed(g), t).ok => t \in Executed(g)

AllFinished == \A t \in Txns : Finished(t)

Progress ==
    \/ \E g \in Groups, t \in Txns :
          Propose(g, t) \/ Finalize(g, t)
    \/ \E g, h \in Groups, t \in Txns : DeliverVote(g, t, h)
    \/ \E g \in Groups : Commit(g)

Init ==
    /\ log = [g \in Groups |-> <<>>]
    /\ ci = [g \in Groups |-> 0]
    /\ views = [g \in Groups |-> 0]
    /\ got = [g \in Groups |-> {}]
    /\ exec = [g \in Groups |-> <<>>]

\* Stuttering once every transaction is finished, so that TLC's deadlock check
\* reports only states where unfinished work can make no progress.
Next ==
    \/ Progress
    \/ \E g \in Groups : ViewChange(g)
    \/ AllFinished /\ UNCHANGED vars

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Properties *)

TypeOK ==
    /\ \A g \in Groups : ci[g] \in 0..Len(log[g]) /\ views[g] <= MaxViewChanges
    /\ \A g \in Groups, t \in Txns : t \in Executed(g) => g \in Parts(t)

FinTs(g, t) == FinOf(Committed(g), t).ts

\* The decision as the full set of committed votes defines it.
Decided(t) == \A g \in Parts(t) : HasProp(Committed(g), t)
GlobalTs(t) == Max({PropOf(Committed(g), t).ts : g \in Parts(t)})
GlobalOk(t) == \A g \in Parts(t) : PropOf(Committed(g), t).vote = "commit"

\* Invariant 1: a transaction commits on all participants or on none.
Atomic ==
    \A t \in Txns : \A g, h \in Parts(t) :
        ~(/\ HasFin(Committed(g), t) /\ FinOf(Committed(g), t).ok
          /\ HasFin(Committed(h), t) /\ ~FinOf(Committed(h), t).ok)

\* Invariant 2: all participants finalize, and so execute, at the same timestamp.
SameTs ==
    \A t \in Txns : \A g, h \in Parts(t) :
        HasFin(Committed(g), t) /\ HasFin(Committed(h), t) => FinTs(g, t) = FinTs(h, t)

(* Invariant 3: each shard's executed history is equivalent to executing in *)
(* timestamp order: on every key, executions follow the local final order.  *)
ShardOrder ==
    \A k \in KeySet :
        LET H == Hist(k)
            g == Owner[k]
        IN \A i, j \in 1..Len(H) :
               i < j => Before(FinTs(g, H[i]), H[i], FinTs(g, H[j]), H[j])

(* Invariant 4: the global history is serializable in timestamp order. On  *)
(* every key the executed writes are exactly the committed transactions in *)
(* global (timestamp, id) order, up to the latest one executed, so every   *)
(* read and every final value equal serial execution; and each coordinated *)
(* require holds at the transaction's place in that serial order.          *)
Serializable ==
    \A k \in KeySet :
        LET H == Hist(k)
        IN \A i \in 1..Len(H) :
               /\ Decided(H[i]) /\ GlobalOk(H[i])
               /\ \A j \in 1..(i - 1) :
                      Before(GlobalTs(H[j]), H[j], GlobalTs(H[i]), H[i])
               /\ \A u \in Txns :
                      (/\ k \in Keys[u] /\ Decided(u) /\ GlobalOk(u)
                       /\ Before(GlobalTs(u), u, GlobalTs(H[i]), H[i]))
                          => \E j \in 1..(i - 1) : H[j] = u
               /\ Kind[H[i]] = "coord" => i - 1 < Cap[H[i]]

(* No deadlock: there is no state in which unfinished transactions all wait *)
(* and no step other than a view change can be taken.                       *)
NoDeadlock == AllFinished \/ ENABLED Progress

-----------------------------------------------------------------------------
(* Witnesses. Each is false in some reachable state, which TLC must report, *)
(* to show that the case is exercised and the checks above are not vacuous. *)

\* No transaction ever executes ahead of an unexecuted one ordered before it.
NeverBypass ==
    \A g \in Groups : \A t \in Executed(g) :
        LET C == Committed(g)
        IN \A u \in Unexec(C, exec[g]) :
               ~Before(IF HasFin(C, u) THEN FinOf(C, u).ts ELSE PropOf(C, u).ts, u,
                       FinOf(C, t).ts, t)

\* No coordinated prepare ever votes v.
NeverVotes(v) ==
    \A g \in Groups, t \in Txns :
        HasProp(log[g], t) => PropOf(log[g], t).vote # v
NeverConflict == NeverVotes("conflict")
NeverAbort == NeverVotes("abort")

\* No independent transaction, proposed elsewhere, ever waits for a lock.
NeverLockWait ==
    \A g \in Groups, t \in Txns :
        ~(/\ Kind[t] = "ind" /\ g \in Parts(t) /\ ~HasProp(log[g], t)
          /\ \E h \in Parts(t) : HasProp(log[h], t)
          /\ Local(g, t) \cap Locked(g, log[g]) # {})

=============================================================================
