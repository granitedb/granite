---- MODULE EffectDiffers ----
\* Injected defect: the change takes effect at different ops on different replicas.
\* A new member takes it from the op after the reconfigure op, the old members
\* from the checkpoint boundary.
EXTENDS Reconfigure
Before == <<0, 1, 2>>
After == <<3, 0, 1>>
EffectAtReconfigure(r) == IF r \in Range(Old) THEN Boundary ELSE RecOp
====
