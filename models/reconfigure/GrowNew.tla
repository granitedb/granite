---- MODULE GrowNew ----
\* A TLC run of Reconfigure; the bounds are in GrowNew.cfg.
EXTENDS Reconfigure
Before == <<0, 1, 2>>
After == <<3, 0, 1, 2, 4>>
====
