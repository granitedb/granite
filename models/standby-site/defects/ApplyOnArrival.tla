---- MODULE ApplyOnArrival ----
\* Injected defect: each standby group executes what it has durably received up to its own safe timestamp, as a local standby would, instead of up to Ta. Must produce a counterexample.
EXTENDS StandbySite
====
