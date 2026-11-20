---- MODULE FewNacks ----
\* Injected defect: truncation with one nack fewer than the nack quorum
\* N - Qr + 1. A committed op whose copy on the new primary is corrupt is dropped.
EXTENDS Vsr
FewerNacks == N - Qr
====
