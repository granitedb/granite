# Local Linux virtual machine

The io_uring backend and direct I/O run only on Linux ([06-testing-and-engineering.md](06-testing-and-engineering.md) section 0). On macOS they are built and tested in a local Linux virtual machine run by [Lima](https://lima-vm.io). Numbers measured in it are never performance evidence.

## Requirements

- macOS 13 or later (Lima's `vz` driver uses Apple's Virtualization framework), on Apple silicon or Intel.
- [Homebrew](https://brew.sh) packages: `just` and `lima` (`brew install just lima`). Lima 2.0 or later.
- Network access on the first `just vm-up`: it downloads the Linux image (about 600 MiB, kept in Lima's cache for later runs), the Ubuntu packages, and the Rust toolchain.
- About 8 GiB of free memory while the VM runs and a few GiB of disk.

Nothing else is needed on the host; Rust inside the VM is installed by the VM itself.

## Use

```bash
just vm-up      # create and start the VM (first run takes a few minutes)
just vm-check   # check kernel, toolchain, direct I/O and the io_uring backend inside the VM
just vm-shell   # a shell inside the VM, in the repository
just vm-down    # delete the VM and its data disk
```

`just vm-up` on an existing VM only starts it. The checkout that created the VM is mounted read-write at the same path as on the host, so edits on the host are seen at once; to use the VM from another checkout or worktree, run `just vm-down` and `just vm-up` there. Build inside the VM with the usual `just` and `cargo` commands from `just vm-shell`.

## What the VM is

| Item | Value | Where it is fixed |
|---|---|---|
| Instance name | `granite` | `tools/vm/vm.sh` |
| Image | Ubuntu 24.04 server cloud image `release-20260321`, arm64 or amd64 to match the host | URL and sha256 digest in `tools/vm/granite.yaml` |
| Kernel | `6.8.0-106-generic` | comes with the image; `apt-mark hold` and disabled automatic upgrades keep it; `pinned_kernel` in `tools/vm/check.sh` |
| Minimum kernel | 5.6 | `MIN_KERNEL` in `crates/io/src/uring.rs`, and `min_kernel` in `tools/vm/check.sh` |
| Rust | 1.96.1 | `rust-toolchain.toml`, installed by Ubuntu's `rustup` package |
| Data disk | `granite-data`, 4 GiB raw, ext4, mounted at `/mnt/lima-granite-data` | `tools/vm/granite.yaml` |
| Resources | 4 CPUs, 8 GiB memory | `tools/vm/granite.yaml` |

The minimum kernel of 5.6 is the first one with every io_uring operation the design uses: registered files and buffers (5.1), accept and connect (5.5), send and receive (5.6). It is the minimum of the io_uring backend, which refuses to start on an older kernel with an error naming both versions; a test in `crates/io/src/uring.rs` fails if `min_kernel` in `tools/vm/check.sh` differs from it.

The backend registers the shard's message pool with the kernel, and registered memory counts against `RLIMIT_MEMLOCK` (`ulimit -l`, 8 MiB by default on Ubuntu 24.04), so the limit must cover the pool. The tests keep their pools at 4 MiB or less and run one at a time.

Tests that open files with `O_DIRECT` put them on the data disk, not in the repository. The shared folder (virtiofs) accepts `O_DIRECT`, but the write reaches a file on the host through FUSE and the host's page cache, so it says nothing about direct I/O on a block device. Inside the VM, Cargo builds into `/var/tmp/granite-target`, so Linux builds do not overwrite the host's `target/`.

`just vm-check` fails unless:

- the kernel is the pinned one and at least the minimum, and io_uring is not disabled by `kernel.io_uring_disabled`;
- `rustc` matches `rust-toolchain.toml`;
- one sector written to the data disk by `dd oflag=direct,dsync` is opened with both `O_DIRECT` and `O_DSYNC` (checked in an `strace` of the `openat` call) and reads back unchanged with `O_DIRECT`;
- the io crate's tests pass with their data files on the data disk, including the io_uring backend's storage conformance suite, fault injection, kill and allocation tests;
- an `strace` of the storage conformance suite on io_uring shows the data file opened with `O_DIRECT` and `O_DSYNC`, and no `read`, `write`, `pread`, `pwrite` or vectored variant on it until it is closed.

## Changing the image

Take the dated image URL and its sha256 for both architectures from `https://cloud-images.ubuntu.com/releases/`, put them in `tools/vm/granite.yaml`, run `just vm-down && just vm-up`, then set `pinned_kernel` in `tools/vm/check.sh` to the new `uname -r`. Lima keeps downloaded images in `~/Library/Caches/lima`; `limactl prune` removes them.

## Known limits

- The VM runs the host's architecture. On Apple silicon it is arm64, while production is x86-64; x86-64-specific code (SIMD paths) is tested in CI, not here.
- The data disk is a file on the host's disk. `O_DSYNC` reaches the virtual disk, but how far it reaches the physical device depends on the hypervisor; durability results from the VM are not evidence.
- Ubuntu keeps dated cloud images for a limited time. When the pinned URL disappears, `just vm-up` fails on the download; move to a newer image as described above.
- Lima cannot resume a broken image download. If `just vm-up` fails with `unexpected EOF`, run it again; it reuses the instance it created.
- `just vm-down` deletes the data disk too; keep nothing there you need.
- Other Lima or Colima instances are not touched, but they share the host's memory.
