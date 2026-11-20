---- MODULE AckBeforeDurable ----
\* Injected defect: a backup acknowledges a prepare whose write is still in flight.
EXTENDS Vsr
AckOnReceive(slot) == slot.s \in {"ok", "pending"}
====
