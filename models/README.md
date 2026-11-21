# Protocol models

Specifications of the replication, reconfiguration, tablet move and cross-shard protocols, written before the corresponding code.

They are written in TLA+ and checked with TLC, from the pinned release of the TLA+ tools that `tools/model.sh` fetches into `target/tla/` and verifies by SHA-256 on every run. Java 11 or later must be installed.

## Layout

- `models/<name>/` holds the TLA+ modules of one model.
- Every `models/<name>/<Base>.cfg` is a TLC run of `models/<name>/<Base>.tla`, and must finish with no violation. A run module is often a one-line `EXTENDS` of the model's main module, so that each set of bounds has its own file.
- Every `models/<name>/defects/<Base>.cfg`, with its `.tla`, is an injected defect, and must produce a counterexample: a deadlock (when its `.cfg` sets `CHECK_DEADLOCK TRUE`), a violated invariant or property, or a failed `Assert` (TLC exits 11 to 14). Any other outcome, a parse or evaluation error included, fails the run. The runner never turns deadlock checking off; each `.cfg` decides. A defect module may extend the model's main module (the script checks it next to copies of the model's modules, so its name must differ from theirs) and replace one of its definitions from the `.cfg` with `Name <- Replacement`.
- `models/<name>/README.md` holds the results of every run (bounds, states, depth, time) and a table mapping each action to the function that implements it. Tasks that implement or change an action keep that table current.

## Running

```bash
just model vsr
```

`just model <name>` runs every run of that model, then every defect run. It prints, for each run, its bounds, the states explored and the depth (for a defect, the length of the counterexample), and fails if a run finds a violation or a defect run finds none. TLC's state files and logs stay under `target/tla/<name>/`; nothing is written to the repository. The nightly workflow runs it for every directory here.

| Model | Protocol | Task |
|---|---|---|
| [standby-site](standby-site/README.md) | Asynchronous standby site: log shipping, apply timestamp Ta, promotion and epoch fencing (document 04 section 7) | T7.4.1 |
| [selftest](selftest/Counter.tla) | None: a counter with one passing run and one defect for each kind of counterexample, so the runner's verdicts are checked on every run of the nightly job | the runner |
| [vsr](vsr/README.md) | Normal operation and view change with storage faults (document 02) | T2.1.2 |
| [xshard](xshard/README.md) | Cross-shard transactions: independent and coordinated, timestamp-order execution with the bypass, conflict vote, view changes (document 04 section 4) | T5.1.2 |
