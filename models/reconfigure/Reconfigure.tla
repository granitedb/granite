----------------------------- MODULE Reconfigure -----------------------------
(***************************************************************************)
(* Changing the members of one replication group (document 02 section 9), *)
(* on the VSR model of models/vsr: normal operation, view change with      *)
(* nack-based truncation, and crash recovery with a torn log, now in two   *)
(* configurations.                                                         *)
(*                                                                         *)
(* Op RecOp is the reconfigure op, which names the members New; every     *)
(* primary that prepares that op number prepares it. The change takes effect at *)
(* the next checkpoint boundary after it, op Boundary: the old             *)
(* configuration prepares and commits ops up to Boundary and none after    *)
(* it; the new configuration starts once op Boundary is committed, at view *)
(* 0 of epoch 1, from the checkpoint at Boundary, and commits the ops after *)
(* it. A member of the new configuration enters epoch 1, and so counts     *)
(* toward its quorums, only once it holds that checkpoint: a new member by *)
(* state sync, a member of both configurations from its own log or by      *)
(* repair. A removed member keeps running in epoch 0.                      *)
(*                                                                         *)
(* Views are ordered by (epoch, view). Since the views of each epoch are   *)
(* bounded here, the pair is numbered as one integer: views 0 .. MaxView0  *)
(* are epoch 0, and view w of epoch 1 is MaxView0 + 1 + w. Every message   *)
(* carries its view, so its epoch; a replica takes messages of its own     *)
(* epoch only.                                                             *)
(*                                                                         *)
(* Fault model: models/vsr, without slot corruption. Messages live in sets *)
(* that are never emptied (loss, duplication, reordering); a crash loses   *)
(* volatile state and writes in flight, and may tear the first of them.    *)
(* models/reconfigure/README.md lists what is modelled and what is not.    *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Old,           \* members before the change, in primary order
    New,           \* members named by the reconfigure op, in primary order
    RecOp,         \* the op number of the reconfigure op
    CheckpointOps, \* a checkpoint every CheckpointOps ops
    MaxOps,        \* bound: longest log
    MaxView0,      \* bound: highest view of epoch 0
    MaxView1,      \* bound: highest view of epoch 1
    MaxCrash       \* bound: crashes in a behaviour

Range(s) == {s[i] : i \in DOMAIN s}
R == Range(Old) \cup Range(New)
Removed == Range(Old) \ Range(New)
Max(S) == CHOOSE x \in S : \A y \in S : y <= x

\* The next checkpoint boundary after the reconfigure op.
Boundary == (RecOp \div CheckpointOps + 1) * CheckpointOps

\* The op after which replica r takes the new configuration: the same on every replica.
EffectOp(r) == Boundary

EpochStart == MaxView0 + 1
Epoch(v) == IF v < EpochStart THEN 0 ELSE 1
Members(e) == IF e = 0 THEN Old ELSE New
FirstView(e) == IF e = 0 THEN 0 ELSE EpochStart
LastView(e) == IF e = 0 THEN MaxView0 ELSE EpochStart + MaxView1
Views == 0 .. LastView(1)

\* Primary, quorums and membership in view v, from the configuration of its epoch
\* (document 02 section 1: Qr + Qv > n, Qv > n / 2, Qn = n - Qr + 1).
Primary(v) == LET m == Members(Epoch(v)) IN m[((v - FirstView(Epoch(v))) % Len(m)) + 1]
Size(v) == Len(Members(Epoch(v)))
Qr(v) == IF Size(v) = 6 THEN 3 ELSE Size(v) \div 2 + 1
Qv(v) == Size(v) \div 2 + 1
Qn(v) == Size(v) - Qr(v) + 1
Member(vw, r) == r \in Range(Members(Epoch(vw[r])))

ASSUME
    /\ Range(Old) # {} /\ Range(New) # {}
    /\ Len(Old) = Cardinality(Range(Old)) /\ Len(New) = Cardinality(Range(New))
    /\ RecOp >= 1 /\ CheckpointOps >= 1 /\ Boundary <= MaxOps

Unreadable == {"faulty", "torn"}

VARIABLES
    status,     \* volatile: normal, view_change, recovering, recovering_head
    view,       \* superblock; its epoch is the replica's epoch
    logView,    \* superblock
    vhead,      \* superblock: headers of the log installed with logView
    log,        \* WAL and checkpoint: sequence of [v, s], s in ok, pending, faulty, torn
    svc,        \* start_view_change votes [v, from]
    dvc,        \* do_view_change [v, from, lv, log (headers)]
    sv,         \* start_view [v, log (headers)]
    prep,       \* prepare [v, op, parent]
    nack,       \* nack_prepare [v, op, e, from]
    acks,       \* prepare_ok [from, v, op, e]
    committed,  \* history: [op, v, cv], commit of prepare (op, v), first in view cv
    crashes

vars == <<status, view, logView, vhead, log, svc, dvc, sv, prep, nack, acks, committed,
          crashes>>

Headers(l) == [i \in 1 .. Len(l) |-> l[i].v]

Merge(own, hs) ==
    [i \in 1 .. Len(hs) |->
        IF i <= Len(own) /\ own[i].v = hs[i] THEN own[i]
        ELSE [v |-> hs[i], s |-> "faulty"]]

Voters(v) == {m.from : m \in {m \in svc : m.v = v}}

NacksPrepare(l, i, e) ==
    IF i > Len(l) THEN TRUE
    ELSE l[i].s \notin Unreadable /\ (l[i].v # e \/ l[i].s = "pending")

\* The view of the committed prepare at op i.
CommittedView(i) == (CHOOSE c \in committed : c.op = i).v

\* A primary of epoch 0 holding log l may prepare its next op: not past the boundary.
OldMayPrepare(l, p) == Len(l) < EffectOp(p)

\* The log a member of the new configuration enters epoch 1 with: the checkpoint.
EntryLog(r, checkpoint) == checkpoint

Init ==
    /\ status = [r \in R |-> "normal"]
    /\ view = [r \in R |-> 0]
    /\ logView = [r \in R |-> 0]
    /\ vhead = [r \in R |-> <<>>]
    /\ log = [r \in R |-> <<>>]
    /\ svc = {} /\ dvc = {} /\ sv = {} /\ prep = {} /\ nack = {}
    /\ acks = {} /\ committed = {}
    /\ crashes = 0

---------------------------------------------------------------------------
\* Normal operation (document 02 section 3)

\* The primary assigns the next op. Every prepare at op RecOp is the reconfigure op.
ClientRequest(p) ==
    /\ status[p] = "normal" /\ p = Primary(view[p])
    /\ Len(log[p]) < MaxOps
    /\ Epoch(view[p]) = 0 => OldMayPrepare(log[p], p)
    /\ log' = [log EXCEPT ![p] = Append(@, [v |-> view[p], s |-> "pending"])]
    /\ prep' = prep \cup {[v |-> view[p], op |-> Len(log[p]) + 1,
                           parent |-> IF log[p] = <<>> THEN 0
                                      ELSE log[p][Len(log[p])].v]}
    /\ UNCHANGED <<status, view, logView, vhead, dvc, crashes>>

ReceivePrepare(r, m) ==
    /\ Member(view, r)
    /\ status[r] = "normal" /\ view[r] = m.v /\ r # Primary(m.v)
    /\ m.op = Len(log[r]) + 1
    /\ IF m.op = 1 THEN TRUE ELSE log[r][m.op - 1].v = m.parent
    /\ log' = [log EXCEPT ![r] = Append(@, [v |-> m.v, s |-> "pending"])]
    /\ UNCHANGED <<status, view, logView, vhead, dvc, prep, crashes>>

WriteDurable(r, i) ==
    /\ log[r][i].s = "pending"
    /\ log' = [log EXCEPT ![r][i].s = "ok"]
    /\ UNCHANGED <<status, view, logView, vhead, dvc, prep, crashes>>

---------------------------------------------------------------------------
\* View change (document 02 sections 0 and 4), within an epoch

MayVote(st, vw, r) ==
    /\ Member(vw, r)
    /\ \/ st[r] = "normal" /\ r # Primary(vw[r])
       \/ st[r] \in {"view_change", "recovering"}
    /\ vw[r] < LastView(Epoch(vw[r]))

JoinView(r, v) ==
    /\ Member(view, r)
    /\ status[r] \in {"normal", "view_change", "recovering"}
    /\ view[r] < v /\ Epoch(v) = Epoch(view[r])
    /\ \/ Cardinality(Voters(v)) >= Qv(v)
       \/ \E d \in dvc : d.v = v
    /\ status' = [status EXCEPT ![r] = "view_change"]
    /\ view' = [view EXCEPT ![r] = v]
    /\ dvc' = dvc \cup {[v |-> v, from |-> r, lv |-> logView[r], log |-> Headers(log[r])]}
    /\ UNCHANGED <<logView, vhead, log, prep, crashes>>

Best(Q) == CHOOSE d \in Q : \A d2 \in Q : \/ d2.lv < d.lv
                                          \/ d2.lv = d.lv /\ Len(d2.log) <= Len(d.log)

InstallLog(p) ==
    /\ p = Primary(view[p]) /\ status[p] = "view_change" /\ logView[p] < view[p]
    /\ \E Q \in SUBSET {d \in dvc : d.v = view[p]} :
          /\ \E d \in Q : d.from = p
          /\ Cardinality({d.from : d \in Q}) >= Qv(view[p])
          /\ log' = [log EXCEPT ![p] = Merge(log[p], Best(Q).log)]
    /\ logView' = [logView EXCEPT ![p] = view[p]]
    /\ vhead' = [vhead EXCEPT ![p] = Headers(log'[p])]
    /\ UNCHANGED <<status, view, dvc, prep, crashes>>

Truncate(p, i) ==
    /\ p = Primary(view[p]) /\ status[p] = "view_change" /\ logView[p] = view[p]
    /\ log[p][i].s \in Unreadable
    /\ Cardinality({n.from : n \in {n \in nack : n.v = view[p] /\ n.op = i
                                               /\ n.e = log[p][i].v}}) >= Qn(view[p])
    /\ log' = [log EXCEPT ![p] = SubSeq(@, 1, i - 1)]
    /\ vhead' = [vhead EXCEPT ![p] = Headers(log'[p])]
    /\ UNCHANGED <<status, view, logView, dvc, prep, crashes>>

StartView(p) ==
    /\ p = Primary(view[p]) /\ status[p] = "view_change" /\ logView[p] = view[p]
    /\ \A i \in 1 .. Len(log[p]) : log[p][i].s = "ok"
    /\ status' = [status EXCEPT ![p] = "normal"]
    /\ UNCHANGED <<view, logView, vhead, log, dvc, prep, crashes>>

ReceiveStartView(r, m) ==
    /\ Member(view, r) /\ Epoch(m.v) = Epoch(view[r])
    /\ r # Primary(m.v)
    /\ \/ m.v > view[r]
       \/ m.v = view[r] /\ status[r] = "view_change" /\ logView[r] < m.v
       \/ /\ m.v = view[r] /\ status[r] = "recovering_head"
          /\ logView[r] < m.v \/ Len(m.log) >= Len(log[r])
    /\ status' = [status EXCEPT ![r] = "normal"]
    /\ view' = [view EXCEPT ![r] = m.v]
    /\ logView' = [logView EXCEPT ![r] = m.v]
    /\ vhead' = [vhead EXCEPT ![r] = m.log]
    /\ log' = [log EXCEPT ![r] = Merge(@, m.log)]
    /\ UNCHANGED <<dvc, prep, crashes>>

---------------------------------------------------------------------------
\* Reconfiguration (document 02 section 9)

\* A member of the new configuration, still in epoch 0, learns that op EffectOp(r),
\* and so the reconfigure op before it, is committed (commit messages of epoch 0),
\* holds the checkpoint at that op (state sync, or its own log and repair) and
\* enters view 0 of epoch 1. The checkpoint is written with the superblock, so
\* entering is atomic here.
EnterEpoch(r) ==
    /\ r \in Range(New) /\ Epoch(view[r]) = 0
    /\ \E c \in committed : c.op = EffectOp(r)
    /\ LET checkpoint == [i \in 1 .. EffectOp(r) |-> [v |-> CommittedView(i), s |-> "ok"]]
           l == EntryLog(r, checkpoint)
       IN /\ log' = [log EXCEPT ![r] = l]
          /\ vhead' = [vhead EXCEPT ![r] = Headers(l)]
    /\ status' = [status EXCEPT ![r] = "normal"]
    /\ view' = [view EXCEPT ![r] = EpochStart]
    /\ logView' = [logView EXCEPT ![r] = EpochStart]
    /\ UNCHANGED <<dvc, prep, crashes>>

---------------------------------------------------------------------------
\* Repair and crashes (document 02 section 5)

Repair(r, s, i) ==
    /\ status[r] # "recovering_head"
    /\ log[r][i].s \in Unreadable
    /\ i <= Len(log[s]) /\ log[s][i].v = log[r][i].v /\ log[s][i].s = "ok"
    /\ log' = [log EXCEPT ![r][i].s = "ok"]
    /\ UNCHANGED <<status, view, logView, vhead, dvc, prep, crashes>>

Crash(r, torn) ==
    /\ Member(view, r)
    /\ crashes < MaxCrash
    /\ LET l == log[r]
           durable == {i \in 1 .. Len(l) : l[i].s # "pending"}
           last == IF durable = {} THEN 0 ELSE Max(durable)
           kept == [i \in 1 .. last |->
                      IF l[i].s = "pending" THEN [l[i] EXCEPT !.s = "torn"] ELSE l[i]]
           wal == IF torn /\ last < Len(l)
                  THEN Append(kept, [l[last + 1] EXCEPT !.s = "torn"]) ELSE kept
           hs == vhead[r]
           new == [i \in 1 .. IF Len(wal) > Len(hs) THEN Len(wal) ELSE Len(hs) |->
                     IF i <= Len(wal) THEN wal[i] ELSE [v |-> hs[i], s |-> "torn"]]
       IN /\ log' = [log EXCEPT ![r] = new]
          /\ status' = [status EXCEPT ![r] =
                IF Len(new) > Len(hs) /\ new[Len(new)].s \in Unreadable THEN "recovering_head"
                ELSE IF logView[r] < view[r] THEN "view_change"
                ELSE IF r = Primary(view[r]) THEN "recovering"
                ELSE "normal"]
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<view, logView, vhead, dvc, prep>>

---------------------------------------------------------------------------
Slots(r) == 1 .. Len(log[r])

Step ==
    \E r \in R :
        \/ ClientRequest(r)
        \/ \E m \in prep : ReceivePrepare(r, m)
        \/ \E i \in Slots(r) : WriteDurable(r, i) \/ Truncate(r, i)
        \/ \E v \in Views : JoinView(r, v)
        \/ InstallLog(r)
        \/ StartView(r)
        \/ \E m \in sv : ReceiveStartView(r, m)
        \/ \E s \in R : \E i \in Slots(r) : Repair(r, s, i)
        \/ \E torn \in BOOLEAN : Crash(r, torn)
        \/ EnterEpoch(r)

\* As in models/vsr, votes, prepare_ok, nack_prepare, start_view and commits are
\* sent at the first step they may be (votes eagerly: EagerVotes there).
Acks(st, vw, lg) ==
    UNION {{[from |-> r, v |-> vw[r], op |-> i, e |-> lg[r][i].v] :
               i \in {i \in 1 .. Len(lg[r]) : lg[r][i].s = "ok"}} :
             r \in {r \in R : Member(vw, r) /\ st[r] = "normal" /\ r # Primary(vw[r])}}

\* The primary commits in order, from Qr prepare_ok of its view. In epoch 1 the ops
\* up to the checkpoint are already committed and are not committed again.
Commits(st, vw, lg, ak) ==
    LET Low(p) == IF Epoch(vw[p]) = 0 THEN 0 ELSE EffectOp(p)
        Acked(p, j) ==
            /\ lg[p][j].s = "ok"
            /\ Cardinality({a.from : a \in {a \in ak : a.v = vw[p] /\ a.op = j
                                                      /\ a.e = lg[p][j].v}} \cup {p})
                 >= Qr(vw[p])
    IN UNION {{[op |-> i, v |-> lg[p][i].v, cv |-> vw[p]] :
                  i \in {i \in Low(p) + 1 .. Len(lg[p]) :
                           \A j \in Low(p) + 1 .. i : Acked(p, j)}} :
                p \in {p \in R : st[p] = "normal" /\ p = Primary(vw[p])}}

Nacks(st, vw, lv, lg) ==
    UNION {{[v |-> vw[s], op |-> i, e |-> lg[Primary(vw[s])][i].v, from |-> s] :
               i \in {i \in 1 .. Len(lg[Primary(vw[s])]) :
                        /\ lg[Primary(vw[s])][i].s \in Unreadable
                        /\ NacksPrepare(lg[s], i, lg[Primary(vw[s])][i].v)}} :
             s \in {s \in R : LET p == Primary(vw[s]) IN
                              /\ Member(vw, s) /\ s # p
                              /\ st[s] = "view_change" /\ lv[s] < vw[s]
                              /\ st[p] = "view_change" /\ vw[p] = vw[s] /\ lv[p] = vw[p]}}

OwnNacks ==
    UNION {{[v |-> view'[p], op |-> i, e |-> log'[p][i].v, from |-> p] :
               i \in {i \in 1 .. Len(log'[p]) : /\ log'[p][i].s \in Unreadable
                                                 /\ NacksPrepare(log[p], i, log'[p][i].v)}} :
             p \in {p \in R : /\ p = Primary(view'[p]) /\ status'[p] = "view_change"
                              /\ logView[p] < logView'[p]}}

\* start_view when the primary starts its view, and again with its current log
\* while a member of its epoch is behind in or before that view.
StartViews ==
    {[v |-> view'[p], log |-> Headers(log'[p])] :
        p \in {p \in R : /\ status'[p] = "normal" /\ p = Primary(view'[p])
                         /\ \/ status[p] # "normal"
                            \/ \E r \in R :
                                 /\ Member(view', r) /\ Epoch(view'[r]) = Epoch(view'[p])
                                 /\ \/ view'[r] < view'[p]
                                    \/ view'[r] = view'[p] /\ status'[r] # "normal"}}

Next ==
    /\ Step
    /\ svc' = svc \cup {[v |-> view'[r] + 1, from |-> r] :
                           r \in {r \in R : MayVote(status', view', r)}}
    /\ nack' = nack \cup Nacks(status', view', logView', log') \cup OwnNacks
    /\ sv' = sv \cup StartViews
    /\ acks' = acks \cup Acks(status', view', log')
    /\ LET new == Commits(status', view', log', acks')
       IN committed' = committed \cup
                         {c \in new : ~\E o \in committed : o.op = c.op /\ o.v = c.v}

Spec == Init /\ [][Next]_vars

---------------------------------------------------------------------------
\* Properties

Status == {"normal", "view_change", "recovering", "recovering_head"}
Slot == [v : Views, s : {"ok", "pending", "faulty", "torn"}]

TypeOK ==
    /\ status \in [R -> Status]
    /\ view \in [R -> Views]
    /\ logView \in [R -> Views]
    /\ \A r \in R :
          /\ logView[r] <= view[r]
          /\ Len(log[r]) <= MaxOps
          /\ \A i \in Slots(r) : log[r][i] \in Slot
          /\ Len(vhead[r]) <= Len(log[r])
          /\ \A i \in 1 .. Len(vhead[r]) : log[r][i].v = vhead[r][i]

\* Invariant 1 of document 02: at most one prepare is committed at an op number,
\* across both configurations.
OneCommitPerOp == \A a, b \in committed : a.op = b.op => a.v = b.v

\* Invariant 2: once committed in view cv, a prepare is in every log of view cv or
\* later, in either configuration, and no primary of such a view starts without it.
CommittedNotLost ==
    \A c \in committed, r \in R : logView[r] >= c.cv =>
        /\ c.op <= Len(log[r]) => log[r][c.op].v = c.v
        /\ status[r] = "normal" /\ r = Primary(view[r]) => c.op <= Len(log[r])

\* Invariant 6: an acknowledged prepare stays durable on its replica until that
\* replica adopts the log of a later view.
AckedIsDurable ==
    \A a \in acks :
        \/ logView[a.from] > a.v
        \/ /\ a.op <= Len(log[a.from])
           /\ log[a.from][a.op].v = a.e
           /\ log[a.from][a.op].s = "ok"

\* The two configurations never both commit at an op number: a quorum of the old
\* configuration and one of the new may be disjoint, so they must commit
\* different ops, the old up to the boundary and the new after it.
OneConfigurationPerOp == \A a, b \in committed : a.op = b.op => Epoch(a.cv) = Epoch(b.cv)

\* Once the new configuration has committed an op, the old one commits no prepare
\* it had not committed: the two never commit side by side.
Handover ==
    [][(\E c \in committed : Epoch(c.cv) = 1) =>
         \A c \in committed' \ committed : Epoch(c.cv) = 1]_vars

\* A removed replica, which keeps running, never enters the new configuration, no
\* message of the new configuration comes from it, and the old configuration
\* commits nothing past the boundary.
RemovedCannotDisturb ==
    /\ \A r \in Removed : Epoch(view[r]) = 0
    /\ \A m \in svc \cup dvc \cup nack \cup acks : Epoch(m.v) = 1 => m.from \in Range(New)
    /\ \A c \in committed : Epoch(c.cv) = 0 => c.op <= Boundary

\* Every replica of epoch 1 holds the committed checkpoint at the boundary.
CheckpointHeld ==
    \A r \in R : Epoch(view[r]) = 1 =>
        /\ Len(log[r]) >= Boundary
        /\ \A i \in 1 .. Boundary : log[r][i].v = CommittedView(i)

---------------------------------------------------------------------------
\* Witnesses: where a view change happens relative to the change. Each Never*
\* must fail in the run it is checked with, which shows that run reaches that
\* case (README.md, Witnesses).

InViewChange(e) == \E r \in R : /\ Member(view, r) /\ Epoch(view[r]) = e
                                /\ view[r] > FirstView(e) /\ status[r] = "view_change"
IsCommitted(i) == \E c \in committed : c.op = i

\* Before the reconfigure op commits: it is prepared, not committed, and the old
\* configuration is in a view change.
NeverBeforeCommit == ~(InViewChange(0) /\ (\E m \in prep : m.op = RecOp) /\ ~IsCommitted(RecOp))
\* Between its commit and the boundary.
NeverBeforeBoundary == ~(InViewChange(0) /\ IsCommitted(RecOp) /\ ~IsCommitted(Boundary))
\* At the boundary, in the old configuration: the boundary op is committed, the new
\* configuration has started, and the old one is in a view change.
NeverAtBoundaryOld == ~(InViewChange(0) /\ IsCommitted(Boundary) /\ \E r \in R : Epoch(view[r]) = 1)
\* At the boundary, in the new configuration: a view change before it prepares any op.
NeverAtBoundaryNew == ~(InViewChange(1) /\ ~\E m \in prep : m.op > Boundary)
\* After it: a view change in the new configuration once it has committed an op.
NeverAfterBoundary == ~(InViewChange(1) /\ IsCommitted(Boundary + 1))
=============================================================================
