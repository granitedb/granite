---- MODULE ReplaceOldCrash ----
\* A TLC run of Reconfigure; the bounds are in ReplaceOldCrash.cfg.
EXTENDS Reconfigure
Before == <<0, 1, 2>>
After == <<3, 0, 1>>
====
