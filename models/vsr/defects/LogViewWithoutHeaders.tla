---- MODULE LogViewWithoutHeaders ----
\* Injected defect: log_view is made durable without the headers of the log it
\* names. A new primary that crashes before its installed prepares are durable
\* restarts with that log_view and a shorter log, and its log can win the next
\* view change over the replicas that hold a committed op.
EXTENDS Vsr
NoHeaders(hs) == <<>>
====
