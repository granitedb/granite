---- MODULE QuorumsDisjoint ----
\* Injected defect: a replication quorum and a view-change quorum that do not
\* intersect (Qr + Qv = N). A view change can miss a committed op.
EXTENDS Vsr
NoQuorumCheck == TRUE
====
