# Recipes for working on Granite. Run `just` to list them.
# CI runs `just check`, so a change that passes locally passes there.

set shell := ["bash", "-euo", "pipefail", "-c"]

# List the recipes.
default:
    @just --list --unsorted

# Everything CI checks: crate directions, formatting, lints, tests, the API docs, the gateway build, and dependency licences and advisories.
check: crate-graph fmt-check lint test test-release doc gateway roadmap-plan deny

# Fail if a crate depends on another against the allowed direction (docs/06 section 5).
crate-graph:
    python3 tools/crate_graph.py

# Build every crate in the workspace.
build:
    cargo build --workspace --all-targets --locked

# Run the tests.
test:
    cargo test --workspace --locked

# Check that overflow checks and assertions stay on in the release profile, with a test built in it.
test-release:
    GRANITE_PROFILE=release cargo test --release --locked -p granite-types --test release

# The full arithmetic gates of T1.4.2, in the release profile: every 16-bit pair and 10,000,000 cases per operation and type (about half an hour). Pass a seed with `GRANITE_SEED=<n> just test-arithmetic`.
test-arithmetic:
    GRANITE_FULL=1 cargo test --release --locked -p granite-types --test arithmetic -- --nocapture

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

# Run the simulator's fixed seeds, crates/sim/ci-seeds.txt, as CI does.
sim:
    cargo run --profile sim --locked -p granite-sim --bin sim -- --list crates/sim/ci-seeds.txt

# Run COUNT simulator seeds from FIRST, for example `just sim-range 10000 10000`.
sim-range first count:
    cargo run --profile sim --locked -p granite-sim --bin sim -- --range {{ first }} {{ count }}

# Run random simulator seeds for SECONDS.
sim-random seconds:
    cargo run --profile sim --locked -p granite-sim --bin sim -- --random {{ seconds }}

# Run one simulator seed and print every event of it.
sim-trace seed:
    cargo run --profile sim --locked -p granite-sim --bin sim -- --seed {{ seed }} --trace

# Model-check one protocol model under models/ with TLC, for example `just model vsr` (models/README.md).
model name:
    tools/model.sh {{ name }}

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
