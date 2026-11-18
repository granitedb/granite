//! Every numeric limit of the design, in one place.
//!
//! Two kinds:
//! - [`FORMAT`]: format constants, fixed when a cluster is formatted and
//!   stored in the superblock. Replicas must agree on them byte for byte,
//!   because they decide the data file layout or replicated state. A data
//!   file whose stored values differ from the binary's is refused
//!   ([`check_format`]).
//! - [`PROCESS_CONFIG_DEFAULT`]: process configuration, set when a process
//!   starts. It may differ between processes without breaking determinism.
//!
//! The values are initial ones, to be benchmarked in phase 1. No other file
//! may spell one of them as a literal; `crates/base/tests/limit_literals.rs`
//! enforces it, and `crates/base/tests/design_tables.rs` compares them with
//! the tables of the design documents. Sizes are in bytes, durations in
//! milliseconds.

use std::fmt;

/// One kibibyte. A unit, not a limit.
pub const KIB: u64 = 1 << 10;
/// One mebibyte. A unit, not a limit.
pub const MIB: u64 = KIB << 10;
/// One gibibyte. A unit, not a limit.
pub const GIB: u64 = MIB << 10;

/// Declares a struct of named `u64` limits, its value, and `entries()`.
macro_rules! limits {
    (
        $(#[$struct_meta:meta])* struct $Struct:ident;
        $(#[$const_meta:meta])* const $CONST:ident;
        $($(#[$meta:meta])* $name:ident: $value:expr,)*
    ) => {
        $(#[$struct_meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct $Struct {
            $($(#[$meta])* pub $name: u64,)*
        }

        $(#[$const_meta])*
        pub const $CONST: $Struct = $Struct { $($name: $value,)* };

        impl $Struct {
            const LEN: usize = [$(stringify!($name)),*].len();

            /// Every limit with its name, in declaration order.
            pub const fn entries(&self) -> [(&'static str, u64); Self::LEN] {
                [$((stringify!($name), self.$name)),*]
            }

            #[cfg(test)]
            #[allow(dead_code, reason = "only the format test uses it")]
            fn values_mut(&mut self) -> [&mut u64; Self::LEN] {
                [$(&mut self.$name),*]
            }
        }
    };
}

limits! {
    /// Constants fixed at cluster format and stored in the superblock.
    struct FormatConstants;
    /// The format constants of this binary.
    const FORMAT;

    /// Sector size; every direct I/O offset and length is a multiple of it.
    /// Document 01 section 2.
    sector_size: 4 * KIB,
    /// Copies of the superblock root record. Document 01 section 2.
    superblock_copies: 4,
    /// Size of one superblock copy. Document 01 section 2.
    superblock_copy_size: 64 * KIB,
    /// Size of the header every message, prepare and reply starts with.
    /// Document 05 section 1.
    header_size: 256,
    /// Slots in the WAL rings; slot index is op mod this. Document 01 section 2.
    wal_slot_count: 1024,
    /// Largest message, header included; also the size of a WAL prepare slot.
    /// Document 01 section 2, document 03 section 8, document 05 section 1.
    message_size_max: MIB,
    /// Size of a grid block. Document 01 section 2.
    grid_block_size: 128 * KIB,
    /// Step by which the grid grows when the free set runs low.
    /// Document 01 section 2.
    grid_growth_step: GIB,
    /// Ops between two compaction beats, and between memtable flushes.
    /// Document 01 section 3.
    compaction_interval_ops: 32,
    /// Size ratio between consecutive LSM levels. Document 01 section 3.
    lsm_growth_factor: 8,
    /// Target size of a chunk of a multi-column group. Document 01 section 4.
    chunk_size_target: 32 * KIB,
    /// Most rows in a chunk of a single-column group. Document 01 section 4.
    chunk_rows_max: 8192,
    /// Filter bits per key. Document 01 section 4.
    filter_bits_per_key: 10,
    /// Ops between two checkpoints. Document 01 section 7.
    checkpoint_interval_ops: 960,
    /// Prepares in flight at the primary. Document 02 section 3.
    pipeline_depth: 8,
    /// Client sessions per group. Document 02 section 7.
    client_sessions_max: 16_384,
    /// Largest write reply stored inline in the session table.
    /// Document 02 section 7.
    reply_inline_max: 4 * KIB,
    /// Largest decimal precision P. Document 03 section 2.
    decimal_precision_max: 76,
    /// Columns per table. Document 03 section 8.
    columns_per_table_max: 1024,
    /// Largest encoded primary key. Document 03 section 8.
    primary_key_size_max: 512,
    /// Largest encoded row. Document 03 section 8.
    row_size_max: 64 * KIB,
    /// Reads plus writes per program invocation. Document 03 section 8.
    program_accesses_max: 256,
    /// Tables per cluster. Document 03 section 8.
    tables_max: 4096,
    /// Programs per cluster. Document 03 section 8.
    programs_max: 4096,
    /// Tablet size at which a tablet is split. Document 04 section 1.
    tablet_size_target: 16 * GIB,
    /// Participant groups per cross-shard transaction. Document 04 section 4.
    participants_max: 8,
    /// Pending cross-shard transactions per shard. Document 04 section 4.
    pending_transactions_max: 4096,
    /// Work waiting in a shard's timestamp-order queue. Document 04 section 4.
    queue_length_max: 16_384,
    /// Commit time for which finished transaction ids are kept, in ms.
    /// Document 04 section 4.
    finished_retention_ms: 60 * 60 * 1000,
}

limits! {
    /// Configuration set when a process starts.
    struct ProcessConfig;
    /// Initial values of the process configuration.
    const PROCESS_CONFIG_DEFAULT;

    /// Interval of the commit heartbeat when idle, in ms. Document 02 section 0.
    heartbeat_interval_ms: 10,
    /// Silence from the primary before a backup votes for a view change, in ms.
    /// Document 02 section 0.
    suspicion_timeout_ms: 60,
    /// Largest per-replica jitter added to the suspicion timeout, in ms.
    /// Document 02 section 0.
    suspicion_jitter_max_ms: 20,
    /// Budget for start_view_change, do_view_change and start_view, in ms.
    /// Document 02 section 0.
    view_change_budget_ms: 10,
    /// Budget for repairing the uncommitted pipeline suffix, in ms.
    /// Document 02 section 0.
    pipeline_repair_budget_ms: 20,
    /// Client retry timeout before broadcasting to all replicas, in ms.
    /// Document 02 section 0.
    client_retry_timeout_ms: 50,
    /// Time within which a group accepts writes again after its primary
    /// fails, in ms. Document 02 section 0.
    failover_budget_ms: 200,
    /// Longest a task may run on a shard thread without yielding, in ms.
    /// Document 02 section 0.
    task_slice_max_ms: 1,
    /// Bound on the p99.9 event-loop iteration time, in ms.
    /// Document 02 section 0.
    loop_iteration_p999_max_ms: 5,
    /// How far ahead of the synchronised clock a received timestamp may be,
    /// in ms. Document 04 section 4.
    clock_guard_ms: 100,
    /// Rows in one column batch of query execution. Document 05 section 4.
    batch_rows_max: 4096,
}

const _: () = FORMAT.validate();
const _: () = PROCESS_CONFIG_DEFAULT.validate();

impl FormatConstants {
    /// Panics, and so fails compilation in a const context, when a
    /// combination of format constants cannot work.
    ///
    /// Message size must be a multiple of the sector size:
    /// ```compile_fail,E0080
    /// # use granite_base::constants::{FormatConstants, FORMAT};
    /// const _: () = FormatConstants { message_size_max: FORMAT.message_size_max + 1, ..FORMAT }.validate();
    /// ```
    /// Grid block size must be a multiple of the sector size (half a sector,
    /// with a chunk target that still fits in it):
    /// ```compile_fail,E0080
    /// # use granite_base::constants::{FormatConstants, FORMAT};
    /// const _: () = FormatConstants {
    ///     grid_block_size: FORMAT.sector_size / 2,
    ///     chunk_size_target: FORMAT.sector_size / 2,
    ///     ..FORMAT
    /// }
    /// .validate();
    /// ```
    /// The checkpoint interval must be smaller than the WAL slot count minus
    /// the pipeline depth minus one compaction interval:
    /// ```compile_fail,E0080
    /// # use granite_base::constants::{FormatConstants, FORMAT};
    /// const _: () = FormatConstants {
    ///     pipeline_depth: FORMAT.wal_slot_count - FORMAT.compaction_interval_ops - FORMAT.checkpoint_interval_ops,
    ///     ..FORMAT
    /// }.validate();
    /// ```
    /// The row size limit plus the header must fit in a message:
    /// ```compile_fail,E0080
    /// # use granite_base::constants::{FormatConstants, FORMAT};
    /// const _: () = FormatConstants {
    ///     row_size_max: FORMAT.message_size_max - FORMAT.header_size + 1,
    ///     ..FORMAT
    /// }.validate();
    /// ```
    /// The header must be exactly 256 bytes:
    /// ```compile_fail,E0080
    /// # use granite_base::constants::{FormatConstants, FORMAT};
    /// const _: () = FormatConstants { header_size: 2 * FORMAT.header_size, ..FORMAT }.validate();
    /// ```
    /// The queue must hold at least one write per session of a group:
    /// ```compile_fail,E0080
    /// # use granite_base::constants::{FormatConstants, FORMAT};
    /// const _: () = FormatConstants { queue_length_max: FORMAT.client_sessions_max - 1, ..FORMAT }.validate();
    /// ```
    /// The unchanged constants compile:
    /// ```
    /// # use granite_base::constants::FORMAT;
    /// const _: () = FORMAT.validate();
    /// ```
    pub const fn validate(&self) {
        let f = self;
        assert!(
            f.sector_size.is_power_of_two(),
            "sector size is a power of two"
        );
        assert!(
            f.message_size_max.is_multiple_of(f.sector_size),
            "message size is a multiple of the sector size"
        );
        assert!(
            f.grid_block_size.is_multiple_of(f.sector_size),
            "grid block size is a multiple of the sector size"
        );
        assert!(
            f.superblock_copy_size.is_multiple_of(f.sector_size),
            "superblock copy is a multiple of the sector size"
        );
        assert!(
            (f.wal_slot_count * f.header_size).is_multiple_of(f.sector_size),
            "WAL header ring is a multiple of the sector size"
        );
        assert!(
            f.grid_growth_step.is_multiple_of(f.grid_block_size),
            "grid grows by whole blocks"
        );
        assert!(f.header_size == 256, "header is exactly 256 bytes");
        assert!(
            f.row_size_max + f.header_size <= f.message_size_max,
            "row size limit plus header fits in a message"
        );
        assert!(
            f.reply_inline_max + f.header_size <= f.message_size_max,
            "inline reply plus header fits in a message"
        );
        assert!(
            f.primary_key_size_max <= f.row_size_max,
            "primary key fits in a row"
        );
        assert!(
            f.chunk_size_target <= f.grid_block_size,
            "chunk fits in a grid block"
        );
        assert!(
            f.checkpoint_interval_ops > 0,
            "checkpoint interval is positive"
        );
        assert!(
            f.checkpoint_interval_ops + f.pipeline_depth + f.compaction_interval_ops
                < f.wal_slot_count,
            "checkpoint interval is smaller than WAL slots minus pipeline minus one compaction interval"
        );
        assert!(
            f.checkpoint_interval_ops
                .is_multiple_of(f.compaction_interval_ops),
            "checkpoints fall on compaction interval boundaries"
        );
        assert!(
            f.queue_length_max >= f.client_sessions_max,
            "queue length is at least the session capacity of a group"
        );
        assert!(
            f.participants_max >= 2,
            "a cross-shard transaction has at least two participants"
        );
    }
}

impl ProcessConfig {
    /// Panics, and so fails compilation in a const context, when a
    /// combination of process settings cannot work.
    ///
    /// shortcut: panics instead of returning a typed error; upgrade when
    /// configuration is read from operator input at startup.
    pub const fn validate(&self) {
        let c = self;
        assert!(
            c.heartbeat_interval_ms < c.suspicion_timeout_ms,
            "an idle primary heartbeats before backups suspect it"
        );
        assert!(
            c.suspicion_timeout_ms
                + c.suspicion_jitter_max_ms
                + c.view_change_budget_ms
                + c.pipeline_repair_budget_ms
                + c.client_retry_timeout_ms
                <= c.failover_budget_ms,
            "failover steps fit in the failover budget"
        );
        assert!(
            c.task_slice_max_ms <= c.loop_iteration_p999_max_ms,
            "a task slice fits in a loop iteration"
        );
    }
}

/// A data file's stored format constant differs from this binary's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatMismatch {
    /// Name of the constant, as in [`FormatConstants`].
    pub constant: &'static str,
    /// Value stored in the data file.
    pub stored: u64,
    /// Value compiled into this binary.
    pub binary: u64,
}

impl fmt::Display for FormatMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "format constant {} is {} in the data file but {} in this binary",
            self.constant, self.stored, self.binary
        )
    }
}

impl std::error::Error for FormatMismatch {}

/// Compares a data file's stored format constants with this binary's.
/// Returns the first constant that differs, in declaration order.
pub fn check_format(stored: &FormatConstants) -> Result<(), FormatMismatch> {
    for ((constant, binary), (_, stored)) in FORMAT.entries().into_iter().zip(stored.entries()) {
        if stored != binary {
            return Err(FormatMismatch {
                constant,
                stored,
                binary,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_format_is_accepted() {
        assert_eq!(check_format(&FORMAT), Ok(()));
    }

    #[test]
    fn every_differing_constant_is_named() {
        let entries = FORMAT.entries();
        for (index, (name, value)) in entries.iter().enumerate() {
            let mut stored = FORMAT;
            *stored.values_mut()[index] += 1;
            let error = check_format(&stored).unwrap_err();
            assert_eq!(
                error,
                FormatMismatch {
                    constant: name,
                    stored: value + 1,
                    binary: *value
                }
            );
            assert!(error.to_string().contains(name));
        }
    }
}
