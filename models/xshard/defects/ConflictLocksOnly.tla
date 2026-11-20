---- MODULE ConflictLocksOnly ----
\* Injected defect: a coordinated prepare checks locks only, not keys of unexecuted transactions (document 04a finding F4). Must produce a counterexample.
EXTENDS Scenario
====
