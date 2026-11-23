---- MODULE ShrinkNew ----
\* A TLC run of Reconfigure; the bounds are in ShrinkNew.cfg.
EXTENDS Reconfigure
Before == <<0, 1, 2, 3, 4>>
After == <<2, 0, 1>>
====
