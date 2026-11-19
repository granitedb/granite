---- MODULE Counter ----
\* A self-test of tools/model.sh, not a protocol: one run that passes and one
\* defect for each kind of counterexample the runner accepts (models/README.md).
EXTENDS Naturals, TLC
CONSTANT Max
VARIABLE x
Init == x = 0
Next == x < Max /\ Assert(x + 1 # 99, "x reached 99") /\ x' = x + 1 \* 99 only in AssertFails
Spec == Init /\ [][Next]_x
Small == x <= 3
StepsOnce == [][x = 0]_x
====
