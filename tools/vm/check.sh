#!/usr/bin/env bash
# Runs inside the VM (`just vm-check`). Exits non-zero on the first unmet requirement.
set -euo pipefail

# The minimum kernel of the io_uring backend, MIN_KERNEL in crates/io/src/uring.rs, which
# checks it at startup; a test there fails if the two differ. It is the first kernel with
# every io_uring operation the design uses: registered files and buffers (5.1), accept and
# connect (5.5), send and recv (5.6).
min_kernel=5.6
# Kernel shipped in the image pinned by tools/vm/granite.yaml.
pinned_kernel=6.8.0-106-generic
data=/mnt/lima-granite-data

fail() { echo "vm-check: FAIL: $*" >&2; exit 1; }

kernel=$(uname -r)
echo "kernel: $kernel (pinned $pinned_kernel, minimum $min_kernel)"
[[ $kernel == "$pinned_kernel" ]] || fail "kernel $kernel is not the pinned $pinned_kernel"
printf '%s\n%s\n' "$min_kernel" "${kernel%%[-+]*}" | sort -V -C || fail "kernel $kernel is below $min_kernel"
[[ $(sysctl -n kernel.io_uring_disabled 2>/dev/null || echo 0) == 0 ]] || fail "io_uring is disabled by sysctl"

echo "toolchain: $(rustc --version), $(cargo --version)"
grep -q "channel = \"$(rustc --version | cut -d' ' -f2)\"" rust-toolchain.toml || fail "rustc does not match rust-toolchain.toml"

# One sector written with O_DIRECT | O_DSYNC on the data disk, the open flags checked
# with strace, then read back with O_DIRECT and compared.
dev=$(findmnt -no SOURCE "$data") || fail "$data is not mounted"
[[ -b $dev ]] || fail "$data is not on a block device ($dev)"
sector=$(lsblk -no LOG-SEC "$dev" | tr -d ' ')
tmp=$(mktemp -d)
file=$data/vm-check-sector
tests=$data/granite-io-tests
trap 'rm -rf "$tmp"; sudo rm -rf "$file" "$tests"' EXIT
head -c "$sector" /dev/urandom > "$tmp/in"
sudo strace -o "$tmp/trace" -e trace=openat \
    dd if="$tmp/in" of="$file" bs="$sector" count=1 oflag=direct,dsync status=none
open=$(grep -F "\"$file\"" "$tmp/trace") || fail "no open of $file in the trace"
echo "open: $open"
[[ $open == *O_DIRECT* && $open == *O_DSYNC* ]] || fail "$file not opened with O_DIRECT|O_DSYNC"
sudo dd if="$file" of="$tmp/out" bs="$sector" count=1 iflag=direct status=none
cmp "$tmp/in" "$tmp/out" || fail "sector read back differs"
echo "direct I/O: one $sector-byte sector written with O_DIRECT|O_DSYNC on $dev and read back"

# The io_uring backend: the io crate's tests with their data files on the data disk, then
# the storage conformance suite under strace, which must show the data file opened with
# O_DIRECT|O_DSYNC and no read or write system call on it until it is closed.
sudo install -d -o "$(id -u)" -g "$(id -g)" "$tests"
TMPDIR=$tests cargo test --locked -p granite-io 2>&1 | grep -E "^error|^test result|FAILED|panicked" \
    || fail "the io crate's tests failed"
bin=$(cargo test --locked -p granite-io --test uring --no-run 2>&1 |
    sed -n 's/^ *Executable tests\/uring.rs (\(.*\))$/\1/p')
[[ -x $bin ]] || fail "no test binary for crates/io/tests/uring.rs"
TMPDIR=$tests strace -f -o "$tmp/uring" \
    -e trace=openat,close,read,write,pread64,pwrite64,readv,writev,preadv,pwritev,preadv2,pwritev2 \
    "$bin" --exact uring_passes_the_storage_conformance_suite --test-threads=1 > "$tmp/uring.out" \
    || fail "the storage conformance suite failed on io_uring: $(cat "$tmp/uring.out")"
open=$(grep -E "openat\(.*\"$tests/granite-io-conformance-[0-9]+/data\"" "$tmp/uring") \
    || fail "no open of the data file in the trace"
echo "open: $open"
[[ $open == *O_DIRECT* && $open == *O_DSYNC* ]] || fail "the data file is not opened with O_DIRECT|O_DSYNC"
fd=${open##*= }
calls=$(awk -v fd="$fd" -v open="$open" '
    $0 == open { on = 1; next }
    on && $0 ~ ("close\\(" fd "\\)") { on = 0 }
    on && $0 ~ ("(read|write|pread64|pwrite64|readv|writev|preadv|pwritev|preadv2|pwritev2)\\(" fd ",")
' "$tmp/uring")
[[ -z $calls ]] || fail "read or write system calls on the data file: $calls"
echo "io_uring: storage conformance suite passed; no read or write system call on the data file (fd $fd)"

echo "vm-check: ok"
