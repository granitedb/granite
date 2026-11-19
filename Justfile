# Recipes for working on Granite. Run `just` to list them.
# CI runs `just check`, so a change that passes locally passes there.

set shell := ["bash", "-euo", "pipefail", "-c"]

# List the recipes.
default:
    @just --list --unsorted

# Everything CI checks: crate directions, formatting, lints, tests, the API docs, the gateway build, and dependency licences and advisories.
check: crate-graph fmt-check lint test doc gateway roadmap-plan deny

# Fail if a crate depends on another against the allowed direction (docs/06 section 5).
crate-graph:
    python3 tools/crate_graph.py

# Build every crate in the workspace.
build:
    cargo build --workspace --all-targets --locked

# Run the tests.
test:
    cargo test --workspace --locked

# Format the code.
fmt:
    cargo fmt --all
    cargo fmt --manifest-path gateway/Cargo.toml

# Fail if the code is not formatted.
fmt-check:
    cargo fmt --all -- --check
    cargo fmt --manifest-path gateway/Cargo.toml -- --check

# Run clippy with warnings as errors, the determinism bans and the unsafe rule (docs/coding-rules.md).
lint:
    python3 tools/lint.py

# Check the SQL gateway, which is built outside the workspace.
gateway:
    cargo clippy --manifest-path gateway/Cargo.toml --all-targets --locked -- -D warnings

# Check dependency licences, advisories, bans and sources (deny.toml), for the workspace, the gateway and the fuzz targets.
deny:
    cargo deny --locked check
    cargo deny --locked --manifest-path gateway/Cargo.toml check
    cargo deny --locked --manifest-path fuzz/Cargo.toml check

# Run the granite binary, for example `just run version`.
run *args:
    cargo run --locked -p granite -- {{ args }}

# Build the API documentation.
doc:
    cargo doc --workspace --no-deps --locked

# Check that every roadmap task has a complete specification, and print a summary.
roadmap-plan:
    python3 tools/roadmap_issues.py plan

# Create and start the Linux VM for io_uring and direct I/O work (docs/linux-vm.md).
vm-up:
    tools/vm/vm.sh up

# Inside the VM: check the kernel and toolchain, write one sector with O_DIRECT | O_DSYNC, and test the io_uring backend.
vm-check:
    tools/vm/vm.sh check

# Open a shell inside the VM, in the repository.
vm-shell:
    tools/vm/vm.sh shell

# Delete the VM and its data disk.
vm-down:
    tools/vm/vm.sh down
