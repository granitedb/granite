---- MODULE TaFromReceived ----
\* Injected defect: the coordinator computes Ta from what the standbys received instead of what they made durable. Must produce a counterexample.
EXTENDS StandbySite
====
