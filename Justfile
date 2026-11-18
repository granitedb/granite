# Recipes for working on Granite. Run `just` to list them.
# CI runs `just check`, so a change that passes locally passes there.

set shell := ["bash", "-euo", "pipefail", "-c"]

# List the recipes.
default:
    @just --list --unsorted

# Everything CI checks: crate directions, formatting, lints, tests, the API docs, and the gateway build.
check: crate-graph fmt-check lint test doc gateway roadmap-plan

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

# Run clippy with warnings as errors.
lint:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Check the SQL gateway, which is built outside the workspace.
gateway:
    cargo clippy --manifest-path gateway/Cargo.toml --all-targets --locked -- -D warnings

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

# Inside the VM: check the kernel and toolchain, and write one sector with O_DIRECT | O_DSYNC.
vm-check:
    tools/vm/vm.sh check

# Open a shell inside the VM, in the repository.
vm-shell:
    tools/vm/vm.sh shell

# Delete the VM and its data disk.
vm-down:
    tools/vm/vm.sh down
