# 0001. io_uring on hosted CI runners

Status: proposed. The evidence below is filled in from the first run of the `io-uring` job; the decision is confirmed or replaced then.

## Question

The io_uring backend (T0.3.2, T0.3.3) is Linux-only, so its tests run in CI. Do GitHub-hosted Linux runners allow a process to create an io_uring instance and complete I/O through it, on the runner itself and inside a container? (document 06, section 0; task T0.1.3)

## How it is checked

The `io-uring` job in `.github/workflows/ci.yml` runs on `ubuntu-24.04` on every pull request and push to main. Its log is the evidence:

1. `uname -a` and `kernel.io_uring_disabled` (0 allows all processes, 1 only a privileged group, 2 nobody; absent before Linux 6.6).
2. On the runner: `fio --name=probe --ioengine=io_uring --rw=read --bs=4k --size=4k --direct=1`. fio's io_uring engine creates a ring with `io_uring_setup`, submits the read and reaps its completion; the step fails if the ring is refused or fewer than one read is issued (`issued rwts: total=1,0,...`).
3. The same command inside `ubuntu:24.04` under Docker's default seccomp profile. Recorded, not required (`continue-on-error`).

fio is the Ubuntu package, so the check adds no Rust dependency before T0.3.2 needs the `io-uring` crate.

## Decision

Linux-only jobs (the io_uring backend's conformance suite and anything else that needs io_uring or direct I/O) run directly on the hosted runner, never under a job-level `container:`.

If step 2 fails, hosted runners are not usable for these jobs, and the alternative is: the Linux-only suites run in the local Linux virtual machine of T0.1.6 for development, and in CI on a self-hosted Linux runner registered to this repository, which T0.3.2 sets up before its CI gate. This file is then updated with the failure output.

## Evidence

To be filled in from the first run (paste the lines, keep the link):

| Item | Value |
|---|---|
| Run | (link to the Actions run) |
| Commit | |
| Runner image | (from "Set up job": image name and version) |
| Kernel (`uname -a`) | |
| `kernel.io_uring_disabled` | |
| Read on the runner | pass / fail, with the fio `issued rwts` line or the error |
| Read inside a container | pass / fail, with the fio `issued rwts` line or the error |
