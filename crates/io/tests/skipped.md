# Tests of granite-io that do not run everywhere

`cargo test` runs every other test of this crate (`crates/io`) on both Linux and macOS. The io_uring backend's suites run on Linux, the portable backend's on macOS, and the in-memory backend's on both. `tests/skipped.rs` fails if a test that some system skips is missing here, or if a test listed here is gone.

## `src/uring.rs`

Not on macOS: io_uring is Linux-only, so the module is not built there.

- `kernel_check`
- `vm_check_uses_the_same_minimum`
- `a_short_write_is_completed_by_resubmission`
- `interrupted_and_busy_operations_are_retried`
- `short_sends_and_receives_are_reassembled`
- `interrupted_network_operations_are_retried`
- `failed_connects_are_reported`
- `connections_past_the_capacity_are_refused`
- `an_io_error_reaches_the_caller`

## `src/pin.rs`

Not on macOS: `sched_setaffinity` and `set_mempolicy` are Linux-only, so the module is not built there.

- `shard_thread_runs_on_exactly_its_core`
- `core_outside_the_set_is_refused`

## `tests/uring.rs`

Not on macOS: the io_uring backend's storage and network conformance suites and its own tests, which need io_uring and direct I/O.

- `a_data_file_in_use_is_refused`
- `uring_passes_the_storage_conformance_suite`
- `completed_writes_survive_sigkill`
- `a_million_mixed_operations_allocate_nothing_after_startup`
- `uring_passes_the_network_conformance_suite`
- `a_stream_of_messages_arrives_whole_without_allocating`
- `a_peer_that_stops_reading_fills_the_queue`
- `a_reset_by_the_peer_is_reported_and_returns_the_buffers`
- `a_size_above_the_limit_closes_the_connection`
- `echo_between_two_shards`: also ignored on Linux, as it is a measurement, run by hand in a release build on real hardware.

## `tests/portable.rs`

Not on Linux: the portable backend's storage and network conformance suites and its own tests. The backend exists only in macOS builds, so that no Linux build can select it.

- `portable_passes_the_storage_conformance_suite`
- `portable_passes_the_network_conformance_suite`
- `portable_passes_the_stream_check`
- `a_data_file_in_use_is_refused`
- `failed_connects_are_reported`
- `connections_past_the_capacity_are_refused`
- `a_peer_that_stops_reading_fills_the_queue`
- `a_reset_by_the_peer_is_reported_and_returns_the_buffers`
- `a_size_above_the_limit_closes_the_connection`

## `tests/record_cost.rs`

Ignored on both: a measurement, run by hand in a release build on real hardware.

- `record_cost`
