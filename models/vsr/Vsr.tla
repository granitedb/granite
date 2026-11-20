-------------------------------- MODULE Vsr --------------------------------
(***************************************************************************)
(* One replication group of document 02 (docs/02-replication.md): normal   *)
(* operation (section 3), view change with quorum-gated suspicion and      *)
(* nack-based truncation (sections 0 and 4), recovery with a torn or       *)
(* corrupt log (section 5) and flexible quorums (section 1).               *)
(*                                                                         *)
(* Fault model. Messages live in sets and are never removed, so any       *)
(* message may be lost (never received), duplicated (received again) or  *)
(* reordered. A crash loses volatile state and every write not yet        *)
(* durable; the write in flight at the head may be torn. A durable log    *)
(* slot may be corrupted or its write lost, on fewer than a replication   *)
(* quorum; both are detected (checksum, redundant header) and the slot    *)
(* becomes faulty. The header (op and view) of a faulty slot stays known, *)
(* as the redundant header and the hash chain give it, except at the head *)
(* after a restart beyond the headers the superblock holds, which makes   *)
(* the head uncertain.                                                    *)
(*                                                                         *)
(* A prepare is identified by (op, view): one primary per view assigns    *)
(* each op once, so the view a prepare was created in stands for its      *)
(* checksum. A log slot is [v |-> that view, s |-> its state].            *)
(* models/vsr/README.md maps every action to the function implementing it.*)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    N,          \* replicas
    Qr,         \* replication quorum
    Qv,         \* view-change quorum
    MaxOps,     \* bound: longest log
    MaxView,    \* bound: highest view
    MaxCrash,   \* bound: crashes in a behaviour
    MaxCorrupt, \* bound: corrupted or lost slot writes in a behaviour
    EagerVotes  \* TRUE: every replica that may vote does so at once (see Next)

R == 0 .. N - 1
Primary(v) == v % N
NackQuorum == N - Qr + 1                  \* document 02 section 1
JoinQuorum == Qv                          \* votes that move a replica
QuorumsSound == Qr + Qv > N /\ 2 * Qv > N \* document 02 section 1

ASSUME N >= 1 /\ Qr \in 1 .. N /\ Qv \in 1 .. N /\ QuorumsSound /\ EagerVotes \in BOOLEAN

Unreadable == {"faulty", "torn"}
Max(S) == CHOOSE x \in S : \A y \in S : y <= x

VARIABLES
    status,     \* volatile: normal, view_change, recovering, recovering_head
    view,       \* superblock
    logView,    \* superblock
    vhead,      \* superblock: headers of the log installed with logView
    log,        \* WAL: sequence of [v, s], s in ok, pending, faulty, torn
    svc,        \* start_view_change votes [v, from]
    dvc,        \* do_view_change [v, from, lv, log (headers)]
    sv,         \* start_view [v, log (headers)]
    prep,       \* prepare [v, op, parent]
    nack,       \* nack_prepare [v, op, e, from]
    acks,       \* prepare_ok [from, v, op, e], sent at the first step it may be (Acks)
    committed,  \* history: [op, v, cv], commit of prepare (op, v), first in view cv (Commits)
    crashes,
    corruptions

vars == <<status, view, logView, vhead, log, svc, dvc, sv, prep, nack, acks, committed,
          crashes, corruptions>>

Headers(l) == [i \in 1 .. Len(l) |-> l[i].v]

\* A slot keeps its content when it already holds the prepare a header names.
Merge(own, hs) ==
    [i \in 1 .. Len(hs) |->
        IF i <= Len(own) /\ own[i].v = hs[i] THEN own[i]
        ELSE [v |-> hs[i], s |-> "faulty"]]

Voters(v) == {m.from : m \in {m \in svc : m.v = v}}

\* "I certainly never acknowledged prepare (i, e)" (document 02 section 2).
NacksPrepare(l, i, e) ==
    IF i > Len(l) THEN TRUE \* no prepare there, and the head is certain
    ELSE l[i].s \notin Unreadable /\ (l[i].v # e \/ l[i].s = "pending")

\* Installing a log for a view writes log_view to the superblock together with the
\* headers of that log (document 02 section 4), so a restart never reports a
\* log_view with a shorter log than the one it installed.
SuperblockHeaders(hs) == hs

\* A backup may acknowledge a slot once its write is durable (invariant 6).
AckReady(slot) == slot.s = "ok"

Init ==
    /\ status = [r \in R |-> "normal"]
    /\ view = [r \in R |-> 0]
    /\ logView = [r \in R |-> 0]
    /\ vhead = [r \in R |-> <<>>]
    /\ log = [r \in R |-> <<>>]
    /\ svc = {} /\ dvc = {} /\ sv = {} /\ prep = {} /\ nack = {}
    /\ acks = {} /\ committed = {}
    /\ crashes = 0 /\ corruptions = 0

---------------------------------------------------------------------------
\* Normal operation (section 3)

\* The primary assigns the next op, starts its own write and sends the prepare.
ClientRequest(p) ==
    /\ status[p] = "normal" /\ p = Primary(view[p])
    /\ Len(log[p]) < MaxOps
    /\ log' = [log EXCEPT ![p] = Append(@, [v |-> view[p], s |-> "pending"])]
    /\ prep' = prep \cup {[v |-> view[p], op |-> Len(log[p]) + 1,
                           parent |-> IF log[p] = <<>> THEN 0
                                      ELSE log[p][Len(log[p])].v]}
    /\ UNCHANGED <<status, view, logView, vhead, dvc, crashes, corruptions>>

\* A backup takes a prepare of its view that extends its hash chain.
ReceivePrepare(r, m) ==
    /\ status[r] = "normal" /\ view[r] = m.v /\ r # Primary(m.v)
    /\ m.op = Len(log[r]) + 1
    /\ IF m.op = 1 THEN TRUE ELSE log[r][m.op - 1].v = m.parent
    /\ log' = [log EXCEPT ![r] = Append(@, [v |-> m.v, s |-> "pending"])]
    /\ UNCHANGED <<status, view, logView, vhead, dvc, prep, crashes, corruptions>>

\* A WAL write completes.
WriteDurable(r, i) ==
    /\ log[r][i].s = "pending"
    /\ log' = [log EXCEPT ![r][i].s = "ok"]
    /\ UNCHANGED <<status, view, logView, vhead, dvc, prep, crashes, corruptions>>

---------------------------------------------------------------------------
\* View change (sections 0 and 4)

\* Suspicion, or a stalled view change: a vote, which changes nothing locally.
MayVote(st, vw, r) ==
    /\ \/ st[r] = "normal" /\ r # Primary(vw[r])
       \/ st[r] \in {"view_change", "recovering"}
    /\ vw[r] < MaxView

Vote(r) ==
    /\ MayVote(status, view, r)
    /\ svc' = svc \cup {[v |-> view[r] + 1, from |-> r]}
    /\ UNCHANGED <<status, view, logView, vhead, log, dvc, prep, nack, sv, acks,
                   committed, crashes, corruptions>>

\* On a quorum of votes, or a do_view_change of the view: make the view durable,
\* stop taking old prepares, send do_view_change.
JoinView(r, v) ==
    /\ status[r] \in {"normal", "view_change", "recovering"}
    /\ view[r] < v
    /\ \/ Cardinality(Voters(v)) >= JoinQuorum
       \/ \E d \in dvc : d.v = v
    /\ status' = [status EXCEPT ![r] = "view_change"]
    /\ view' = [view EXCEPT ![r] = v]
    /\ dvc' = dvc \cup {[v |-> v, from |-> r, lv |-> logView[r], log |-> Headers(log[r])]}
    /\ UNCHANGED <<logView, vhead, log, prep, crashes, corruptions>>

\* The do_view_change with the highest (log_view, op).
Best(Q) == CHOOSE d \in Q : \A d2 \in Q : \/ d2.lv < d.lv
                                          \/ d2.lv = d.lv /\ Len(d2.log) <= Len(d.log)

\* The new primary installs the best log of Qv do_view_change messages, its own
\* among them. Unreadable slots await repair or a nack quorum (Nacks).
InstallLog(p) ==
    /\ p = Primary(view[p]) /\ status[p] = "view_change" /\ logView[p] < view[p]
    /\ \E Q \in SUBSET {d \in dvc : d.v = view[p]} :
          /\ \E d \in Q : d.from = p
          /\ Cardinality({d.from : d \in Q}) >= Qv
          /\ LET new == Merge(log[p], Best(Q).log)
             IN log' = [log EXCEPT ![p] = new]
    /\ logView' = [logView EXCEPT ![p] = view[p]]
    /\ vhead' = [vhead EXCEPT ![p] = SuperblockHeaders(Headers(log'[p]))]
    /\ UNCHANGED <<status, view, dvc, prep, crashes, corruptions>>

\* With a nack quorum for an unreadable op, drop it and every later op.
Truncate(p, i) ==
    /\ p = Primary(view[p]) /\ status[p] = "view_change" /\ logView[p] = view[p]
    /\ log[p][i].s \in Unreadable
    /\ Cardinality({n.from : n \in {n \in nack : n.v = view[p] /\ n.op = i
                                               /\ n.e = log[p][i].v}}) >= NackQuorum
    /\ log' = [log EXCEPT ![p] = SubSeq(@, 1, i - 1)]
    /\ vhead' = [vhead EXCEPT ![p] = SuperblockHeaders(Headers(log'[p]))]
    /\ UNCHANGED <<status, view, logView, dvc, prep, crashes, corruptions>>

\* With every kept prepare durable, the new primary starts the view.
StartView(p) ==
    /\ p = Primary(view[p]) /\ status[p] = "view_change" /\ logView[p] = view[p]
    /\ \A i \in 1 .. Len(log[p]) : log[p][i].s = "ok"
    /\ status' = [status EXCEPT ![p] = "normal"]
    /\ UNCHANGED <<view, logView, vhead, log, dvc, prep, crashes, corruptions>>

\* Adopt the new view's log. A replica with an uncertain head takes only a
\* start_view that covers its whole log.
ReceiveStartView(r, m) ==
    /\ r # Primary(m.v)
    /\ \/ m.v > view[r]
       \/ m.v = view[r] /\ status[r] = "view_change" /\ logView[r] < m.v
       \/ /\ m.v = view[r] /\ status[r] = "recovering_head"
          /\ logView[r] < m.v \/ Len(m.log) >= Len(log[r])
    /\ status' = [status EXCEPT ![r] = "normal"]
    /\ view' = [view EXCEPT ![r] = m.v]
    /\ logView' = [logView EXCEPT ![r] = m.v]
    /\ vhead' = [vhead EXCEPT ![r] = SuperblockHeaders(m.log)]
    /\ log' = [log EXCEPT ![r] = Merge(@, m.log)]
    /\ UNCHANGED <<dvc, prep, crashes, corruptions>>

---------------------------------------------------------------------------
\* Repair and faults (section 5)

\* request_prepare by (op, checksum), answered by a replica holding it intact.
\* The repair write is atomic here: a crash during it leaves the slot as it was.
Repair(r, s, i) ==
    /\ status[r] # "recovering_head"
    /\ log[r][i].s \in Unreadable
    /\ i <= Len(log[s]) /\ log[s][i].v = log[r][i].v /\ log[s][i].s = "ok"
    /\ log' = [log EXCEPT ![r][i].s = "ok"]
    /\ UNCHANGED <<status, view, logView, vhead, dvc, prep, crashes, corruptions>>

\* Crash and restart. Writes in flight are lost; with torn, the first one after the
\* last durable slot is torn. A slot named by the superblock headers but lost from
\* the WAL is torn; an unreadable head beyond them makes the head uncertain.
Crash(r, torn) ==
    /\ crashes < MaxCrash
    /\ LET l == log[r]
           durable == {i \in 1 .. Len(l) : l[i].s # "pending"}
           last == IF durable = {} THEN 0 ELSE Max(durable)
           kept == [i \in 1 .. last |->
                      IF l[i].s = "pending" THEN [l[i] EXCEPT !.s = "torn"] ELSE l[i]]
           wal == IF torn /\ last < Len(l)
                  THEN Append(kept, [l[last + 1] EXCEPT !.s = "torn"]) ELSE kept
           hs == vhead[r]
           \* The superblock still names the installed log's prepares the WAL lost.
           new == [i \in 1 .. IF Len(wal) > Len(hs) THEN Len(wal) ELSE Len(hs) |->
                     IF i <= Len(wal) THEN wal[i] ELSE [v |-> hs[i], s |-> "torn"]]
       IN /\ log' = [log EXCEPT ![r] = new]
          /\ status' = [status EXCEPT ![r] =
                \* Uncertain head: unreadable, and beyond the headers the superblock holds.
                IF Len(new) > Len(hs) /\ new[Len(new)].s \in Unreadable THEN "recovering_head"
                ELSE IF logView[r] < view[r] THEN "view_change"
                \* A restarted primary lost its pipeline and gives up its view.
                ELSE IF r = Primary(view[r]) THEN "recovering"
                ELSE "normal"]
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<view, logView, vhead, dvc, prep, corruptions>>

\* A bit flip or a lost write on a durable slot, on fewer than Qr replicas.
Corrupt(r, i) ==
    /\ corruptions < MaxCorrupt
    /\ log[r][i].s = "ok"
    /\ Cardinality({s \in R : i <= Len(log[s]) /\ log[s][i].v = log[r][i].v
                              /\ log[s][i].s \in Unreadable}) + 1 < Qr
    /\ log' = [log EXCEPT ![r][i].s = "faulty"]
    /\ corruptions' = corruptions + 1
    /\ UNCHANGED <<status, view, logView, vhead, dvc, prep, crashes>>

---------------------------------------------------------------------------
Slots(r) == 1 .. Len(log[r])

Step ==
    \E r \in R :
        \/ ClientRequest(r)
        \/ \E m \in prep : ReceivePrepare(r, m)
        \/ \E i \in Slots(r) :
              \/ WriteDurable(r, i)
              \/ Corrupt(r, i)
              \/ Truncate(r, i)
        \/ \E v \in 1 .. MaxView : JoinView(r, v)
        \/ InstallLog(r)
        \/ StartView(r)
        \/ \E m \in sv : ReceiveStartView(r, m)
        \/ \E s \in R : \E i \in Slots(r) : Repair(r, s, i)
        \/ \E torn \in BOOLEAN : Crash(r, torn)

\* prepare_ok, nack_prepare, start_view and commits happen at the first step
\* they may, in Next. Each only adds to a message or history set, and a message
\* only enables an optional receiving step (Truncate, ReceiveStartView, a
\* commit), so taking each as early as possible reaches every state the
\* protocol reaches, apart from those sets, which it makes as large, and the
\* commit views as early, as they can be: the invariants over them are checked
\* in their strongest form, with far fewer interleavings to explore.
\*
\* A backup acknowledges every durable slot of its view.
Acks(st, vw, lg) ==
    UNION {{[from |-> r, v |-> vw[r], op |-> i, e |-> lg[r][i].v] :
               i \in {i \in 1 .. Len(lg[r]) : AckReady(lg[r][i])}} :
             r \in {r \in R : st[r] = "normal" /\ r # Primary(vw[r])}}

\* The primary commits in order: op i once ops 1 to i each have Qr prepare_ok of
\* its view, its own durable write included.
Commits(st, vw, lg, ak) ==
    LET Acked(p, j) ==
            /\ lg[p][j].s = "ok"
            /\ Cardinality({a.from : a \in {a \in ak : a.v = vw[p] /\ a.op = j
                                                      /\ a.e = lg[p][j].v}} \cup {p}) >= Qr
    IN UNION {{[op |-> i, v |-> lg[p][i].v, cv |-> vw[p]] :
                  i \in {i \in 1 .. Len(lg[p]) : \A j \in 1 .. i : Acked(p, j)}} :
                p \in {p \in R : st[p] = "normal" /\ p = Primary(vw[p])}}

\* A replica that has left its old view answers the new primary's request for a
\* nack on each unreadable slot of the log it installed.
Nacks(st, vw, lv, lg) ==
    UNION {{[v |-> vw[s], op |-> i, e |-> lg[Primary(vw[s])][i].v, from |-> s] :
               i \in {i \in 1 .. Len(lg[Primary(vw[s])]) :
                        /\ lg[Primary(vw[s])][i].s \in Unreadable
                        /\ NacksPrepare(lg[s], i, lg[Primary(vw[s])][i].v)}} :
             s \in {s \in R : LET p == Primary(vw[s]) IN
                              /\ s # p /\ st[s] = "view_change" /\ lv[s] < vw[s]
                              /\ st[p] = "view_change" /\ vw[p] = vw[s] /\ lv[p] = vw[p]}}

\* The new primary's own nacks, from the log it had before installing one.
OwnNacks ==
    UNION {{[v |-> view'[p], op |-> i, e |-> log'[p][i].v, from |-> p] :
               i \in {i \in 1 .. Len(log'[p]) : /\ log'[p][i].s \in Unreadable
                                                 /\ NacksPrepare(log[p], i, log'[p][i].v)}} :
             p \in {p \in R : /\ p = Primary(view'[p]) /\ status'[p] = "view_change"
                              /\ logView[p] < logView'[p]}}

\* start_view: once when the primary starts its view, and again, with its current
\* log, whenever a replica is behind in or before that view (request_start_view).
StartViews ==
    {[v |-> view'[p], log |-> Headers(log'[p])] :
        p \in {p \in R : /\ status'[p] = "normal" /\ p = Primary(view'[p])
                         /\ \/ status[p] # "normal"
                            \/ \E r \in R : \/ view'[r] < view'[p]
                                          \/ /\ view'[r] = view'[p]
                                             /\ status'[r] # "normal"}}

\* With EagerVotes, a replica votes at the first step it may. A vote only enables
\* an optional JoinView, so invariants 1, 2 and 6 are checked as fully as with
\* Vote steps, with fewer interleavings; QuorumGatedSuspicion is then not
\* meaningful, since replicas vote together, and is checked in runs without it.
Next ==
    \/ ~EagerVotes /\ \E r \in R : Vote(r)
    \/ /\ Step
       /\ svc' = IF EagerVotes
                 THEN svc \cup {[v |-> view'[r] + 1, from |-> r] :
                                   r \in {r \in R : MayVote(status', view', r)}}
                 ELSE svc
       /\ nack' = nack \cup Nacks(status', view', logView', log') \cup OwnNacks
       /\ sv' = sv \cup StartViews
       /\ acks' = acks \cup Acks(status', view', log')
       /\ LET new == Commits(status', view', log', acks')
          IN committed' = committed \cup
                            {c \in new : ~\E o \in committed : o.op = c.op /\ o.v = c.v}

Spec == Init /\ [][Next]_vars

---------------------------------------------------------------------------
\* Properties (document 02 section 11)

Status == {"normal", "view_change", "recovering", "recovering_head"}
Slot == [v : 0 .. MaxView, s : {"ok", "pending", "faulty", "torn"}]

TypeOK ==
    /\ status \in [R -> Status]
    /\ view \in [R -> 0 .. MaxView]
    /\ logView \in [R -> 0 .. MaxView]
    /\ \A r \in R :
          /\ logView[r] <= view[r]
          /\ Len(log[r]) <= MaxOps
          /\ \A i \in Slots(r) : log[r][i] \in Slot
          \* The WAL keeps every slot the superblock headers name.
          /\ Len(vhead[r]) <= Len(log[r])
          /\ \A i \in 1 .. Len(vhead[r]) : log[r][i].v = vhead[r][i]

\* Invariant 1: at most one prepare is committed at an op number.
OneCommitPerOp == \A a, b \in committed : a.op = b.op => a.v = b.v

\* Invariant 2: once committed in view cv, a prepare is in every log of view cv or
\* later, and no primary of such a view starts without it.
CommittedNotLost ==
    \A c \in committed, r \in R : logView[r] >= c.cv =>
        /\ c.op <= Len(log[r]) => log[r][c.op].v = c.v
        /\ status[r] = "normal" /\ r = Primary(view[r]) => c.op <= Len(log[r])

\* Invariant 6: an acknowledged prepare is durable on its replica (corruption
\* aside) until that replica adopts the log of a later view.
AckedIsDurable ==
    \A a \in acks :
        \/ logView[a.from] > a.v
        \/ /\ a.op <= Len(log[a.from])
           /\ log[a.from][a.op].v = a.e
           /\ log[a.from][a.op].s \in {"ok", "faulty"}

\* Quorum-gated suspicion (D1): no replica leaves status normal for a view change
\* without Qv distinct votes for its new view, so a single backup's vote moves no
\* replica (Qv >= 2 whenever N >= 2).
QuorumGatedSuspicion ==
    [][\A r \in R : status[r] = "normal" /\ status'[r] = "view_change"
                    => Cardinality(Voters(view'[r])) >= Qv]_vars
=============================================================================
