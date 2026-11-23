---- MODULE OldPastBoundary ----
\* Injected defect: the old configuration keeps preparing past the boundary, so
\* the old and the new configuration can both commit at the same op number.
EXTENDS Reconfigure
Before == <<0, 1, 2>>
After == <<3, 0, 1>>
PrepareOn(l, p) == TRUE
====
