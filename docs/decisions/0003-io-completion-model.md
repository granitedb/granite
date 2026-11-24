# 0003. The `Io` traits and the completion model

Status: proposed on 2026-11-19 (task T0.3.1). It becomes accepted when the change is approved in review for tracks A, B, C and D (document 06 section 7) and merged.

## Question

All production code reaches the disk, the network, the clock and randomness through one boundary (document 06 section 1), so that the same code runs on io_uring, on macOS and in the simulator. What are the traits, how does a result come back, who owns a buffer while an operation is in flight, and should protocol code be written as explicit state machines with callbacks or as `async` on a minimal custom executor?

## The traits

`crates/io/src/traits.rs`; its module documentation is the reference.

- **`Storage`**: `size(zone)`, `read(tag, zone, offset, len, buffer)`, `write(tag, zone, offset, len, buffer)`, `fsync(tag)`, `grow_grid(tag, size)`. A `Zone` is one of the fixed zones of document 01 section 2 (superblock, WAL headers, WAL prepares, grid); offsets are relative to it. Offsets and lengths are whole sectors. `grow_grid` extends the grid, the last zone, to `size` bytes with the space allocated (`fallocate` on Linux, `F_PREALLOCATE` on macOS) and durable, and changes nothing if the zone is already that large; it was added with the free set (T1.1.6), because the grid grows in steps while the replica runs (document 01 section 2).
- **`Network`**: `connect(tag, to)`, `accept(tag)`, `send(tag, connection, len, buffer)`, `recv(tag, connection, buffer)` and `close(connection)`. A peer is a configured id, `Peer(u64)`, that the backend resolves to an address; a connection is an opaque `Connection(u64)`. The backend frames messages with the header's size field, so a receive completes with one whole message; on one connection, messages arrive in the order they were sent and fill posted buffers in the order those were posted.
- **`Time`**: `monotonic()` and `realtime()`, in nanoseconds as integers.
- **`Random`**: `next_u64()`, seeded, used only outside the state machine.
- **`Io`**: all four, plus `poll()`, which returns one completed operation or `None`.

**Completion model.** Storage and network calls only submit. Each accepted submission completes exactly once, as a `Completion` returned by a later `poll`, carrying the caller's `Tag`. Completions come in any order. One queue for storage and network keeps a single delivery order, which is the order the simulator chooses from its seed.

**Which calls complete synchronously.**

| Call | Completes |
|---|---|
| `read`, `write`, `fsync`, `grow_grid`, `connect`, `accept`, `send`, `recv` | Never inside the call. The result comes only from a later `poll`, even when a backend could finish at once (the in-memory backend copies at submission and still queues the completion). A refusal, `Err(Full(..))`, is synchronous and means the operation was not submitted. |
| `size`, `close`, `monotonic`, `realtime`, `next_u64`, `poll` | Always, without blocking. `poll` returns `None` when nothing has completed. `close` completes nothing itself: the operations in flight on the connection complete later, with `Closed` unless they finished first. |

Never completing inside the call means no handler runs while its caller is half way through an update, and the simulator can crash a node between any submission and its completion.

**Buffer ownership.** Buffers are `Message` handles from the shard's pool (T0.2.3). Submitting moves the handle into the backend; the completion, whether the operation succeeded or failed, or the `Full` refusal, moves it back. While the operation is in flight the caller holds nothing the backend touches, and the backend's handle keeps the memory alive, so dropping a waiting state machine or future cannot free memory the kernel is still reading or writing. A `read` or `recv` writes into the buffer, so its handle must be the only one (asserted at submission). A `write` or `send` only reads it, so it may be shared: one prepare is written to the WAL and sent to every backup from one buffer, and a shared buffer is read-only (`Message::bytes_mut` refuses).

**Backpressure.** A backend holds a fixed number of operations in flight; one more is refused with `Full`, which returns what was submitted. A node that posts no receive buffer on a connection reads nothing from it, which pushes back on its sender. The number of open connections is bounded too; one more is refused with `NetworkError::Refused`.

**Errors.** Every storage completion carries `Result<_, StorageError>` (`EIO` and the like), and every network completion `Result<_, NetworkError>`: `Refused`, `TimedOut`, `Reset` (the peer closed or reset the connection, or is gone) or `Closed` (this side closed it). A connection failure, a reset and a timeout therefore reach the caller as completions (T0.3.3). What to do is the caller's: a failed read is repaired from a peer; after a failed write, fsync or growth the data's state is unknown, and a later fsync that succeeds does not prove it durable, because the kernel may have dropped the dirty pages, so `storage` will stop the process and let replication absorb the lost node.

### Differences from the sketch in document 06

Document 06 section 1 is corrected in this change.

- `Network::send` and `recv` name a connection, and `close` is added. `connect` takes a configured peer id rather than an address.
- `Time` has no `tick`. Periodic work is driven by the event loop calling a component's own `tick` at a fixed period, and timeouts are counted in ticks, so the trait needs no timer queue. The shard event loop (T0.3.5) and the simulator (E0.4) each provide the ticks.
- `Storage::size` is added, so callers and the conformance suite can check ranges. Growing the grid (document 01 section 2) is not in the trait yet; it is added with the grid.
- Completions come from one `Io::poll`, not one queue per trait.

### Conformance

`granite_io::conformance::run` is the first version of the suite every backend passes: round trips in every zone, including the last sector and a never-written sector; every submission completes exactly once with its tag; `Full` returns the refused buffer and a completion makes room; a connection opens; messages arrive whole and in order, including a shared buffer sent twice and a message sent before the receiver posted a buffer; closing one end fails the receive posted there with `Closed` and resets the other end; the monotonic clock never goes back; every buffer comes back to the pool. The trivial in-memory backend, `granite_io::memory`, passes it for 32 completion orders, and its own tests check the precondition asserts, a crashed peer and the connection limit (`crates/io/tests/conformance.rs`). T0.3.2 and T0.3.3 extend the suite with what only a real device or socket shows.

## The spike

`crates/io/tests/spike/`. One small replicated component, written twice:

- `callbacks.rs`: an explicit state machine. Each completion is one call of `step`, which dispatches on its kind.
- `futures.rs`: `async` tasks on a minimal executor. Each operation is a future that resolves when its completion is delivered. The executor has no `unsafe`: wakers are `std::task::Wake` flags, and woken tasks run in spawn order.

The component: the primary writes 8 ops to its WAL and sends each to two backups; a backup writes the op and acknowledges it; the primary commits in order once its own write and one acknowledgement are done. A toy simulator (`main.rs`) connects the primary to each backup, the same way for both versions, then runs the three nodes on the in-memory backend, delivers one completion at a time, chosen by the seed, and crashes one backup before a delivery chosen by the seed.

Command, on an Apple M4 Pro laptop with rustc 1.96.1, at the commit that adds this file:

```text
$ cargo test -p granite-io --locked --test spike -- --nocapture
running 4 tests
test traces_differ_between_seeds ... ok
8 ops, 88 deliveries; allocations at start and in handlers: callbacks [0, 0], futures [53, 11]
test allocations_per_operation ... ok
seed 0: 89 crash points
seed 1: 89 crash points
test a_crash_at_every_point_is_injectable_in_both ... ok
1000 seeds, identical traces, 930 with a crash
test both_versions_leave_the_same_trace_for_1000_seeds ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

- Both versions leave the same event trace (writes, acknowledgements, commits, crashes, in order) for seeds 0 to 999; 930 of those runs crash a backup. The traces differ between seeds, so the comparison is not vacuous.
- For two seeds, one per backup, a crash before each of the 89 deliveries of the run leaves the same trace in both versions, and every op still commits.

### Comparison

| | State machine with callbacks | `async` on a minimal executor |
|---|---|---|
| Lines of code (non-blank, non-comment, after `cargo fmt`) | 116 | 246: 87 for the task bodies, 159 for the executor, the operation future and the dispatch |
| Allocations by the component's own code, one run of 8 ops | 0 at start, 0 while handling 88 completions | 53 at start, 11 while handling completions |
| Allocations per operation | 0: state is in fixed arrays, buffers come from the pool | 2 when each operation is a task (its boxed future and its waker), plus growth of the maps of parked completions and waiting tasks |
| Mid-operation crash at every callback or await point | Yes. Every callback is one delivery, and the simulator can stop between any two. | Yes, at every await that waits for a completion: the simulator drops the node's tasks between two deliveries. An await on a completion that has already arrived does not yield, so it is not a separate point; neither is the code between two awaits, as in the callback version. Dropping a future in flight is safe because the backend, not the future, holds the buffer. |
| Where in-flight state lives | In named fields that a checker or a debugger can read | In compiler-generated futures, not visible to checkers |

The figures come from `crates/io/tests/spike/main.rs`, which counts the allocations made inside the component's start and completion handlers with a counting global allocator. The task bodies are shorter than the callbacks, because the sequence of one operation reads top to bottom. The executor is a fixed cost that real code would share, but it is also where determinism has to be argued: the order in which woken tasks run, and the maps of parked completions, are part of the replicated behaviour.

## Decision

| Crate | Style | Why |
|---|---|---|
| `vsr` | Explicit state machine | No allocation per operation, every in-flight state in a named field a checker can inspect, and crash points that are exactly the deliveries. |
| `storage` | Explicit state machine | The same, on the hottest path; a WAL write or a block read must not allocate. |
| `client` | Explicit state machine at its core | The C ABI (`clients/c`) and through it the Go and Python clients need callbacks, and the simulator drives the same client code as the replicas. The Rust client may add an `async` interface on top of the state machine for its users; it is outside the server and outside the simulated core. |

So `async` is not used inside the server. Both spike versions stay in `crates/io/tests/spike/`, so the comparison can be run again; they are test code only.

## Open

- The real backends implement these traits and pass the conformance suite: io_uring (T0.3.2, T0.3.3) and portable (T0.3.4). The shard event loop (T0.3.5) provides the ticks.
- The simulator (E0.4) adds faults to the in-memory model: torn and lost writes on crash, read errors, latency, and network drop, delay, duplication and partitions.
