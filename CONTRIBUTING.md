# Contributing

The project is in its design phase. The most useful contributions right now are reviews of the design documents in `docs/` and corrections against the primary papers they cite.

## Ground rules

- Design changes are made in the documents under `docs/`, in the same change as any code they affect.
- Work is tracked as GitHub issues. Each task issue states what "done" means; a change is complete when that statement is true and demonstrated.
- Correctness comes first. Changes to replication, storage or transaction code need simulator coverage, and protocol changes need an updated specification.
- `just check` must pass before a change is proposed; CI runs the same recipe.
- Follow the determinism rules in [docs/06-testing-and-engineering.md](docs/06-testing-and-engineering.md) section 3.

## Dependencies

Every external crate is code we ship and must trust, so the dependency tree is kept small. This policy covers every `Cargo.toml` in the repository, the gateway included.

**What may be added.** The design names the external crates the database expects to need: `io-uring`, `zerocopy` or `bytemuck`, `rustls`, vendored `lz4` and `zstd`, and a checksum crate ([docs/06-testing-and-engineering.md](docs/06-testing-and-engineering.md) section 5). Anything else needs a case that it does real work we should not write ourselves; a crate is never added to save a few lines. The simulator (`granite-sim`) is never a dependency of the `granite` binary, and the gateway's query engine stays outside the workspace. Dependencies between workspace crates follow the table in the same section; `just check` rejects any other direction.

**Approval rule.** A change that adds an external crate, or moves an existing one to a new major version, states the reason in its description and is approved by a maintainer who is not its author. Without that approval it is not merged.

**Audit step.** Before approving, the reviewer:

1. Checks the licence of the crate and of every crate it pulls in against Apache-2.0.
2. Checks the crate and version against the RustSec advisory database (<https://rustsec.org>).
3. Compares `cargo tree -p granite` before and after the change and reads the `Cargo.lock` diff, so every new transitive crate is visible and accounted for.
4. Reads the crate's `unsafe` code and build script, if it has any, and checks that it is maintained.

`Cargo.lock` is committed and every build uses `--locked`, so an update to an existing dependency is a reviewed change to the lock file and is audited the same way.

## Sign-off (Developer Certificate of Origin)

Every commit must be signed off, certifying that you have the right to submit it under the project's licence, as described at <https://developercertificate.org/>:

```
git commit -s
```

This adds a `Signed-off-by: Your Name <your@email>` line. Commits without it cannot be merged.

## Licence

By contributing you agree that your contributions are licensed under Apache-2.0.

## Conduct

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
