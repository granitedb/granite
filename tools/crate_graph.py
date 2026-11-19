#!/usr/bin/env python3
"""Fail if a workspace crate depends on another against the allowed direction.

Cargo only rejects cycles, so a wrong-direction import that forms no cycle
builds. `just check` runs this script to reject it. ALLOWED mirrors the table
in docs/06-testing-and-engineering.md section 5 and changes with it.

To see it reject a wrong import, add `granite-io.workspace = true` under
[dependencies] in crates/types/Cargo.toml and run `just crate-graph`.
"""
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Direct dependencies from the table. A crate may also depend on anything
# below them, so only the direction is enforced, not the exact list.
ALLOWED = {
    "granite-base": [],
    "granite-io": ["granite-base"],
    "granite-storage": ["granite-base", "granite-io"],
    "granite-lsm": ["granite-storage"],
    "granite-columnar": ["granite-lsm"],
    "granite-types": ["granite-base"],
    "granite-schema": ["granite-types"],
    "granite-txn": ["granite-schema", "granite-lsm"],
    "granite-shard": ["granite-txn", "granite-columnar"],
    "granite-vsr": ["granite-storage", "granite-io"],
    "granite-meta": ["granite-vsr"],
    "granite-xshard": ["granite-vsr", "granite-shard"],
    "granite-query": ["granite-columnar", "granite-types"],
    "granite-protocol": ["granite-base", "granite-types"],
    "granite-client": ["granite-protocol"],
    "granite-client-c": ["granite-client"],
}
ALLOWED["granite-sim"] = [n for n in ALLOWED if not n.startswith("granite-client")]
ALLOWED["granite"] = [n for n in ALLOWED if n not in ("granite-sim", "granite-client-c")]
# Client tooling, added after `granite` so the server never links it: it reads only the binary catalog.
ALLOWED["granite-schemac"] = ["granite-schema"]
# Built outside the workspace (see the root Cargo.toml), checked all the same.
ALLOWED["granite-gateway"] = ["granite-client"]


def below(name):
    """Every crate that `name` may depend on."""
    seen, stack = set(), list(ALLOWED[name])
    while stack:
        dep = stack.pop()
        if dep not in seen:
            seen.add(dep)
            stack += ALLOWED[dep]
    return seen


def violations(packages):
    """packages maps each workspace crate to the workspace crates it depends on."""
    errors = []
    for name, deps in sorted(packages.items()):
        if name not in ALLOWED:
            errors.append(f"{name} is not in the crate table of tools/crate_graph.py")
            continue
        errors += [f"{name} must not depend on {dep}" for dep in sorted(deps) if dep not in below(name)]
    return errors


def self_test():
    # The simulator is never linked into the server.
    assert "granite-sim" not in below("granite")
    # The server never parses schema text (T1.4.5).
    assert "granite-schemac" not in below("granite")
    assert violations({"granite-lsm": ["granite-storage", "granite-base"]}) == []
    # Injected bugs: wrong directions that form no cycle, so Cargo accepts them.
    assert violations({"granite-types": ["granite-base", "granite-io"]}) == [
        "granite-types must not depend on granite-io"
    ]
    assert violations({"granite": ["granite-sim"]}) == ["granite must not depend on granite-sim"]
    assert violations({"granite-schema": ["granite-schemac"]}) == ["granite-schema must not depend on granite-schemac"]
    assert violations({"granite-new": []}) == ["granite-new is not in the crate table of tools/crate_graph.py"]


def main():
    self_test()
    members = []
    for manifest in ("Cargo.toml", "gateway/Cargo.toml"):
        out = subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked", "--manifest-path", manifest],
            check=True, stdout=subprocess.PIPE, text=True, cwd=ROOT,
        ).stdout
        members += json.loads(out)["packages"]
    names = {p["name"] for p in members}
    # Every dependency kind counts: Cargo also allows cycles through dev-dependencies.
    packages = {p["name"]: {d["name"] for d in p["dependencies"] if d["name"] in names} for p in members}
    errors = violations(packages)
    for e in errors:
        print(f"crate graph: {e}", file=sys.stderr)
    if errors:
        sys.exit(1)
    print(f"crate graph: {len(packages)} crates, every dependency in the allowed direction")


if __name__ == "__main__":
    main()
