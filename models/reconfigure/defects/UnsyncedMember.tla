---- MODULE UnsyncedMember ----
\* Injected defect: a new member enters the new configuration, and so votes and
\* counts toward its quorums, before it has reached the checkpoint by state sync:
\* it keeps the log it had.
EXTENDS Reconfigure
Before == <<0, 1, 2>>
After == <<3, 0, 1>>
EnterUnsynced(r, checkpoint) == IF r \in Range(Old) THEN checkpoint ELSE log[r]
====
