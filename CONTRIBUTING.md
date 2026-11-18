# Contributing

The project is in its design phase. The most useful contributions right now are reviews of the design documents in `docs/` and corrections against the primary papers they cite.

## Ground rules

- Design changes are made in the documents under `docs/`, in the same change as any code they affect.
- Work is tracked as GitHub issues. Each task issue states what "done" means; a change is complete when that statement is true and demonstrated.
- Correctness comes first. Changes to replication, storage or transaction code need simulator coverage, and protocol changes need an updated specification.
- `just check` must pass before a change is proposed; CI runs the same recipe.
- Follow the determinism rules in [docs/06-testing-and-engineering.md](docs/06-testing-and-engineering.md) section 3.

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
