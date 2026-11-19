# 0001. io_uring on hosted CI runners

Status: accepted on 2026-11-18, from the first run of the `io-uring` job (evidence below).

## Question

The io_uring backend (T0.3.2, T0.3.3) is Linux-only, so its tests run in CI. Do GitHub-hosted Linux runners allow a process to create an io_uring instance and complete I/O through it, on the runner itself and inside a container? (document 06, section 0; task T0.1.3)

## How it is checked

The `io-uring` job in `.github/workflows/ci.yml` runs on `ubuntu-24.04` on every pull request and push to main. Its log is the evidence:

1. `uname -a` and `kernel.io_uring_disabled` (0 allows all processes, 1 only a privileged group, 2 nobody; absent before Linux 6.6).
2. On the runner: `fio --name=probe --ioengine=io_uring --rw=read --bs=4k --size=4k --direct=1`. fio's io_uring engine creates a ring with `io_uring_setup`, submits the read and reaps its completion; the step fails if the ring is refused or fewer than one read is issued (`issued rwts: total=1,0,...`).
3. The same command inside `ubuntu:24.04` under Docker's default seccomp profile, in the first run only (result below). The step was then removed: it was not required, and on 2026-11-19 it stalled past the job's time limit twice, which cancels the required `io-uring` check and blocks every pull request.

fio is the Ubuntu package, so the check adds no Rust dependency before T0.3.2 needs the `io-uring` crate.

## Decision

Linux-only jobs (the io_uring backend's conformance suite and anything else that needs io_uring or direct I/O) run directly on the hosted runner, never under a job-level `container:`.

If step 2 fails, hosted runners are not usable for these jobs, and the alternative is: the Linux-only suites run in the local Linux virtual machine of T0.1.6 for development, and in CI on a self-hosted Linux runner registered to this repository, which T0.3.2 sets up before its CI gate. This file is then updated with the failure output.

## Evidence

From the first run:

| Item | Value |
|---|---|
| Run | <https://github.com/granitedb/granite/actions/runs/44299680996>, job `io-uring` |
| Commit | a8b14817499901f1d7e051616ce84e60aa3c03f3 |
| Runner image | `ubuntu-24.04`, version 20261111.370.1 |
| Kernel (`uname -a`) | `Linux 7.0.0-1017-azure #17~24.04.1-Ubuntu SMP PREEMPT x86_64` |
| `kernel.io_uring_disabled` | 0 |
| Read on the runner | pass: fio-3.36, `err= 0`, `issued rwts: total=1,0,0,0 short=0,0,0,0 dropped=0,0,0,0` |
| Read inside a container | fail, as expected under Docker's default seccomp profile: `fio: pid=689, err=1/file:engines/io_uring.c:1049, func=io_queue_init, error=Operation not permitted` |
