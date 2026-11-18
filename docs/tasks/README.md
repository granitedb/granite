# Task specifications

One file per phase. Each `## T<phase>.<epic>.<n> Title` section is the full specification of one task and is published as the body of its GitHub issue by `tools/roadmap_issues.py`. [../ROADMAP.md](../ROADMAP.md) stays the index: order, tracks and a one-line summary.

A task is done when every gate in its section and every standard gate below holds. A gate is a statement that is either true or false and that someone other than the author can check. If a gate cannot be met, the task is not done: either the work continues, or the gate is changed in this directory by a reviewed change that says why. Gates are never waived in a pull request.

Each section has the same parts:

- **Goal.** What exists afterwards and why it is needed.
- **Work.** What has to be built or written.
- **Not in this card.** Nearby work that belongs elsewhere.
- **Depends on.** Tasks that must be done first.
- **Gates.** The task-specific conditions, as a checklist.

<!-- standard:begin -->
## Standard gates

These apply to every task, in addition to its own gates.

- [ ] Every task gate is shown by an automated test in the repository, by a committed report, or by a recorded command with its output linked from the pull request. A claim without evidence does not count.
- [ ] `just check` passes, and CI is green on Linux and macOS for the commit that is merged.
- [ ] New logic has a test that fails when the logic is broken. Where a gate says "injected bug", that failure has been demonstrated.
- [ ] The determinism rules in `docs/06-testing-and-engineering.md` section 3 hold for all new code. `unsafe` appears only where that section allows it, and every block has a written safety argument.
- [ ] Assertions cover the preconditions, postconditions and invariants of new code and stay enabled in release builds.
- [ ] No dependency is added without a stated reason in the change, and the licence check passes.
- [ ] Any change to an on-disk or wire format is versioned and described in `docs/` in the same change.
- [ ] If behaviour differs from the design documents, the documents are corrected in the same change.
- [ ] No pool, queue or table introduced by the task can grow without bound; exhaustion is turned into backpressure or a typed error, and a test shows it.
- [ ] Every commit is signed off, and the change is reviewed by an engineer who did not write it. Changes to `vsr`, `storage`, `lsm` or `xshard` are reviewed by the owner of that track.

## Terms used in gates

- **Seed gate N:** the simulator runs seeds 0 to N-1 with every fault class the task names enabled, and all checkers active. Zero failures. The command is recorded.
- **Model test:** a property test that applies random operations to the implementation and to a reference (the `sim` reference model, or a `BTreeMap` where stated) and compares every result. At least 1,000 cases of at least 1,000 operations each, from a logged seed.
- **Fuzz gate H:** a coverage-guided fuzz target under `fuzz/` runs for H CPU-hours with no panic, crash, hang or sanitizer report. The corpus is committed.
- **Injected bug:** a named, deliberate defect is introduced and the test or checker is shown to fail because of it. The defect is kept as a disabled test case or a documented patch.
- **Report:** a file under `bench/results/` or `docs/decisions/` with raw numbers, the hardware, the commit hash and the exact commands.
- **Real hardware:** production-like servers as defined in `docs/06-testing-and-engineering.md` section 0. Numbers from a laptop or a virtual machine do not close a gate that says "real hardware".
- **Proposed threshold:** a number the design does not yet fix. It must be confirmed or replaced in this file, by a reviewed change, before work on the task starts.
<!-- standard:end -->
