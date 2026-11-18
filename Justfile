# Recipes for working on Granite. Run `just` to list them.
# CI runs `just check`, so a change that passes locally passes there.

set shell := ["bash", "-euo", "pipefail", "-c"]

# List the recipes.
default:
    @just --list --unsorted

# Everything CI checks: formatting, lints, tests, and the gateway build.
check: fmt-check lint test gateway

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

# Parse the roadmap and print a summary of the issues it describes.
roadmap-plan:
    python3 tools/roadmap_issues.py plan
