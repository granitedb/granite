---- MODULE ReplaceOld ----
\* A TLC run of Reconfigure; the bounds are in ReplaceOld.cfg.
EXTENDS Reconfigure
Before == <<0, 1, 2>>
After == <<3, 0, 1>>
====
