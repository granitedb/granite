#!/usr/bin/env bash
# Host side of `just vm-up`, `vm-check`, `vm-shell` and `vm-down`; see docs/linux-vm.md.
# Touches only the instance and disk named below, never other Lima or Colima instances.
set -euo pipefail

name=granite
disk=granite-data
repo=$(cd "$(dirname "$0")/../.." && pwd)

has_vm() { limactl list -q 2>/dev/null | grep -qx "$name"; }
has_disk() { limactl disk list 2>/dev/null | awk 'NR > 1 { print $1 }' | grep -qx "$disk"; }
guest() { limactl shell --workdir "$repo" "$name" "$@"; }

case "${1:-}" in
up)
    if has_vm; then
        limactl start --tty=false "$name"
    else
        has_disk || limactl disk create "$disk" --size 4GiB --format raw
        limactl start --tty=false --name "$name" --mount "$repo:w" "$repo/tools/vm/granite.yaml"
    fi
    # The first proxy call makes rustup install the toolchain rust-toolchain.toml pins.
    guest cargo --version
    ;;
check) guest bash tools/vm/check.sh ;;
shell) guest ;;
down)
    if has_vm; then limactl delete --force "$name"; fi
    if has_disk; then limactl disk delete "$disk"; fi
    ;;
*)
    echo "usage: $0 up|check|shell|down" >&2
    exit 2
    ;;
esac
