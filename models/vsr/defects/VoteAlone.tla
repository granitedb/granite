---- MODULE VoteAlone ----
\* Injected defect: a single start_view_change vote moves a replica into a view
\* change (the letter of VR Revisited section 4.2). Shows that the property
\* QuorumGatedSuspicion can fail.
EXTENDS Vsr
OneVote == 1
====
