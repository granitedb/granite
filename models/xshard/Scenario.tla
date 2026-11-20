------------------------------ MODULE Scenario ------------------------------
(***************************************************************************)
(* The scenario: 3 groups, 4 transactions with overlapping keys, two      *)
(* independent and two coordinated; each run checks three of them (Txns). *)
(* Keys a and d live on group 1, b on group 2, c on group 3.               *)
(*   1 independent  {a, b}                                                 *)
(*   2 coordinated  {b, c}  requires b and c never written                 *)
(*   3 independent  {c, d}  disjoint from 1 on group 1: the bypass applies *)
(*   4 coordinated  {b, c}  requires fewer than 2 writes; crosses 2 on     *)
(*                          both groups, so its locks could deadlock       *)
(***************************************************************************)
EXTENDS XShard

SOwner == [k \in {"a", "b", "c", "d"} |->
              CASE k = "a" -> 1 [] k = "b" -> 2 [] k = "c" -> 3 [] k = "d" -> 1]
SKind == <<"ind", "coord", "ind", "coord">>
SCap == <<0, 1, 0, 2>>
SKeys == <<{"a", "b"}, {"b", "c"}, {"c", "d"}, {"b", "c"}>>

\* Transaction 3 shares key a with transaction 1, for the bypass defect.
SKeysShared == <<{"a", "b"}, {"b", "c"}, {"c", "a"}, {"b", "c"}>>
\* Three independent transactions for the queued-bypass defect: on group 1,
\* 2 can queue behind 1 on key a, and 3 shares key d with 2 but not with 1.
SKindChain == <<"ind", "ind", "ind">>
SCapChain == <<0, 0, 0>>
SKeysChain == <<{"a", "b"}, {"a", "d", "c"}, {"d", "c"}>>
=============================================================================
