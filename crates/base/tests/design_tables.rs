//! Compares the limits in `granite_base::constants` with the tables and
//! sentences of the design documents, and checks that every limit names the
//! document section it comes from. A mismatch fails `cargo test`, and so
//! `just check` and CI.

use granite_base::constants::{FORMAT as F, GIB, KIB, MIB, PROCESS_CONFIG_DEFAULT as P};
/// The documents and the module, read at compile time so the test needs no
/// file system access and is rebuilt when one of them changes.
fn read(path: &str) -> &'static str {
    match path {
        "docs/01-storage.md" => include_str!("../../../docs/01-storage.md"),
        "docs/02-replication.md" => include_str!("../../../docs/02-replication.md"),
        "docs/03-data-model-and-transactions.md" => {
            include_str!("../../../docs/03-data-model-and-transactions.md")
        }
        "docs/04-cluster-and-cross-shard.md" => {
            include_str!("../../../docs/04-cluster-and-cross-shard.md")
        }
        "docs/05-query-and-protocol.md" => include_str!("../../../docs/05-query-and-protocol.md"),
        "crates/base/src/constants.rs" => include_str!("../src/constants.rs"),
        _ => panic!("{path} is not included in this test"),
    }
}

fn bytes(v: u64) -> String {
    for (unit, name) in [(GIB, "GiB"), (MIB, "MiB"), (KIB, "KiB")] {
        if v >= unit && v.is_multiple_of(unit) {
            return format!("{} {name}", v / unit);
        }
    }
    format!("{v} bytes")
}

fn grouped(v: u64) -> String {
    let digits = v.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn ms(v: u64) -> String {
    format!("{v} ms")
}

fn hours(v: u64) -> String {
    let h = v / 1000 / 3600;
    assert!(
        v.is_multiple_of(1000) && h * 3600 == v / 1000,
        "a whole number of hours"
    );
    format!("{h} hour{}", if h == 1 { "" } else { "s" })
}

/// A table cell: (constants, document, first cell of the row, column, expected text).
type Cell = (
    &'static [&'static str],
    &'static str,
    &'static str,
    usize,
    String,
);

fn table_cells() -> Vec<Cell> {
    const D01: &str = "docs/01-storage.md";
    const D02: &str = "docs/02-replication.md";
    const D03: &str = "docs/03-data-model-and-transactions.md";
    const D04: &str = "docs/04-cluster-and-cross-shard.md";
    vec![
        (
            &["superblock_copies", "superblock_copy_size"],
            D01,
            "Superblock",
            2,
            format!(
                "{} x {}",
                F.superblock_copies,
                bytes(F.superblock_copy_size)
            ),
        ),
        (
            &["superblock_copies"],
            D01,
            "Superblock",
            1,
            format!("{} copies of the root record", F.superblock_copies),
        ),
        (
            &["header_size"],
            D01,
            "WAL headers",
            2,
            format!("slots x {} B", F.header_size),
        ),
        (
            &["message_size_max"],
            D01,
            "WAL prepares",
            2,
            format!("slots x {}", bytes(F.message_size_max)),
        ),
        (
            &["sector_size"],
            D01,
            "Sector size",
            1,
            bytes(F.sector_size),
        ),
        (
            &["wal_slot_count"],
            D01,
            "WAL slots",
            1,
            grouped(F.wal_slot_count),
        ),
        (
            &["message_size_max"],
            D01,
            "Message size, header included",
            1,
            bytes(F.message_size_max),
        ),
        (
            &["grid_block_size"],
            D01,
            "Grid block size",
            1,
            bytes(F.grid_block_size),
        ),
        (
            &["grid_growth_step"],
            D01,
            "Grid growth step",
            1,
            bytes(F.grid_growth_step),
        ),
        (
            &["pipeline_depth"],
            D01,
            "Pipeline depth (document 02 section 3)",
            1,
            F.pipeline_depth.to_string(),
        ),
        (
            &["compaction_interval_ops"],
            D01,
            "Compaction interval",
            1,
            format!("{} ops", F.compaction_interval_ops),
        ),
        (
            &["checkpoint_interval_ops"],
            D01,
            "Checkpoint interval",
            1,
            format!("{} ops", F.checkpoint_interval_ops),
        ),
        (
            &["client_sessions_max"],
            D01,
            "Client sessions per group (document 02 section 7)",
            1,
            grouped(F.client_sessions_max),
        ),
        (
            &["lsm_growth_factor"],
            D01,
            "LSM level growth factor",
            1,
            F.lsm_growth_factor.to_string(),
        ),
        (
            &["chunk_size_target"],
            D01,
            "Chunk size target, multi-column groups",
            1,
            bytes(F.chunk_size_target),
        ),
        (
            &["chunk_rows_max"],
            D01,
            "Rows per chunk, single-column groups",
            1,
            grouped(F.chunk_rows_max),
        ),
        (
            &["filter_bits_per_key"],
            D01,
            "Filter bits per key",
            1,
            F.filter_bits_per_key.to_string(),
        ),
        (
            &["failover_budget_ms"],
            D02,
            "Failover",
            1,
            format!(
                "A group accepts writes again within {} of primary failure",
                ms(P.failover_budget_ms)
            ),
        ),
        (
            &["heartbeat_interval_ms"],
            D02,
            "Heartbeat interval (commit message when idle)",
            1,
            ms(P.heartbeat_interval_ms),
        ),
        (
            &["suspicion_timeout_ms", "suspicion_jitter_max_ms"],
            D02,
            "Suspicion timeout on a backup (no message from primary)",
            1,
            format!(
                "{}, plus per-replica jitter up to {}",
                ms(P.suspicion_timeout_ms),
                ms(P.suspicion_jitter_max_ms)
            ),
        ),
        (
            &["view_change_budget_ms"],
            D02,
            "start_view_change quorum, do_view_change, start_view",
            1,
            format!(
                "3 network round trips plus one durable write each: under {}",
                ms(P.view_change_budget_ms)
            ),
        ),
        (
            &["pipeline_repair_budget_ms"],
            D02,
            "Repair of the uncommitted pipeline suffix",
            1,
            format!("under {}", ms(P.pipeline_repair_budget_ms)),
        ),
        (
            &["client_retry_timeout_ms"],
            D02,
            "Client discovers the new primary",
            1,
            format!(
                "client retry timeout {}, then broadcast to all replicas; replicas also push a view hint",
                ms(P.client_retry_timeout_ms)
            ),
        ),
        (
            &["message_size_max"],
            D03,
            "Message size",
            1,
            bytes(F.message_size_max),
        ),
        (
            &["columns_per_table_max"],
            D03,
            "Columns per table",
            1,
            F.columns_per_table_max.to_string(),
        ),
        (
            &["primary_key_size_max"],
            D03,
            "Primary key encoded size",
            1,
            bytes(F.primary_key_size_max),
        ),
        (
            &["row_size_max"],
            D03,
            "Row encoded size",
            1,
            bytes(F.row_size_max),
        ),
        (
            &["program_accesses_max"],
            D03,
            "Reads + writes per program invocation",
            1,
            F.program_accesses_max.to_string(),
        ),
        (
            &["program_depth_max"],
            D03,
            "Nesting depth of a program",
            1,
            F.program_depth_max.to_string(),
        ),
        (
            &["program_nodes_max"],
            D03,
            "Statements and expressions per program",
            1,
            grouped(F.program_nodes_max),
        ),
        (
            &["tables_max"],
            D03,
            "Tables per cluster",
            1,
            F.tables_max.to_string(),
        ),
        (
            &["programs_max"],
            D03,
            "Programs per cluster",
            1,
            F.programs_max.to_string(),
        ),
        (
            &["participants_max"],
            D04,
            "Participants per transaction",
            1,
            grouped(F.participants_max),
        ),
        (
            &["pending_transactions_max"],
            D04,
            "Pending transactions per shard",
            1,
            grouped(F.pending_transactions_max),
        ),
        (
            &["queue_length_max"],
            D04,
            "Queue length (work waiting behind an older transaction)",
            1,
            grouped(F.queue_length_max),
        ),
    ]
}

/// A phrase in prose: (constants, document, expected text).
type Phrase = (&'static [&'static str], &'static str, String);

fn phrases() -> Vec<Phrase> {
    const D01: &str = "docs/01-storage.md";
    const D02: &str = "docs/02-replication.md";
    const D03: &str = "docs/03-data-model-and-transactions.md";
    const D04: &str = "docs/04-cluster-and-cross-shard.md";
    const D05: &str = "docs/05-query-and-protocol.md";
    vec![
        (
            &["sector_size"],
            D01,
            format!("sector size {}", F.sector_size),
        ),
        (
            &["grid_growth_step"],
            D01,
            format!("(initially {})", bytes(F.grid_growth_step)),
        ),
        (
            &["lsm_growth_factor"],
            D01,
            format!("each level {}x the previous", F.lsm_growth_factor),
        ),
        (
            &["chunk_size_target"],
            D01,
            format!(
                "about {} for multi-column groups",
                bytes(F.chunk_size_target)
            ),
        ),
        (
            &["chunk_rows_max"],
            D01,
            format!("up to {} rows for single-column groups", F.chunk_rows_max),
        ),
        (
            &["filter_bits_per_key"],
            D01,
            format!("about {} bits per key", F.filter_bits_per_key),
        ),
        (
            &["grid_block_size"],
            D01,
            format!("not the whole {} block", bytes(F.grid_block_size)),
        ),
        (
            &["pipeline_depth"],
            D02,
            format!("Pipeline: up to {} prepares in flight", F.pipeline_depth),
        ),
        (
            &["client_sessions_max"],
            D02,
            format!(
                "Capacity: {} sessions per group",
                grouped(F.client_sessions_max)
            ),
        ),
        (
            &["reply_inline_max"],
            D02,
            format!("stored inline up to {}", bytes(F.reply_inline_max)),
        ),
        (
            &["task_slice_max_ms"],
            D02,
            format!(
                "may run longer than {} without yielding",
                ms(P.task_slice_max_ms)
            ),
        ),
        (
            &["loop_iteration_p999_max_ms"],
            D02,
            format!(
                "p99.9 iteration time must stay under {}",
                ms(P.loop_iteration_p999_max_ms)
            ),
        ),
        (
            &["decimal_precision_max"],
            D03,
            format!("P <= {}", F.decimal_precision_max),
        ),
        (
            &["tablet_size_target"],
            D04,
            format!("Target tablet size {}", bytes(F.tablet_size_target)),
        ),
        (
            &["participants_max"],
            D04,
            format!("can name more than {} groups", F.participants_max),
        ),
        (
            &["finished_retention_ms"],
            D04,
            format!(
                "(initially {} of commit time)",
                hours(F.finished_retention_ms)
            ),
        ),
        (
            &["clock_guard_ms"],
            D04,
            format!("(initially {})", ms(P.clock_guard_ms)),
        ),
        (
            &["message_size_max"],
            D05,
            format!("total at most {}", bytes(F.message_size_max)),
        ),
        (
            &["header_size"],
            D05,
            format!("### Header ({} bytes,", F.header_size),
        ),
        (
            &["batch_rows_max"],
            D05,
            format!("column batches of up to {} rows", P.batch_rows_max),
        ),
    ]
}

fn cell(doc: &str, label: &str, column: usize) -> Option<String> {
    let prefix = format!("| {label} |");
    let line = doc.lines().find(|l| l.starts_with(&prefix))?;
    line.trim()
        .trim_matches('|')
        .split('|')
        .nth(column)
        .map(|c| c.trim().to_string())
}

#[test]
fn design_tables_match_the_module() {
    let mut errors = Vec::new();
    for (_, path, label, column, want) in table_cells() {
        match cell(read(path), label, column) {
            Some(got) if got == want => {}
            got => errors.push(format!(
                "{path}: row `{label}` column {column} is {got:?}, the module says {want:?}"
            )),
        }
    }
    for (_, path, want) in phrases() {
        if !read(path).contains(&want) {
            errors.push(format!(
                "{path}: no phrase {want:?}, which the module implies"
            ));
        }
    }
    assert!(
        errors.is_empty(),
        "design documents differ from crates/base/src/constants.rs:\n{}",
        errors.join("\n")
    );
}

#[test]
fn every_limit_is_compared_with_a_design_document() {
    let compared: Vec<&str> = table_cells()
        .iter()
        .flat_map(|c| c.0.iter().copied())
        .chain(phrases().iter().flat_map(|p| p.0.iter().copied()))
        .collect();
    let names = F
        .entries()
        .into_iter()
        .chain(P.entries())
        .map(|(name, _)| name);
    let missing: Vec<&str> = names.filter(|n| !compared.contains(n)).collect();
    assert!(
        missing.is_empty(),
        "limits not compared with any design document: {missing:?}"
    );
    for name in compared {
        assert!(
            F.entries()
                .iter()
                .chain(P.entries().iter())
                .any(|(n, _)| *n == name),
            "unknown limit {name}"
        );
    }
}

#[test]
fn every_limit_names_its_document_section() {
    let source = read("crates/base/src/constants.rs");
    let mut docs = String::new();
    let mut fields = 0;
    let mut in_limits = false;
    for line in source.lines() {
        // Only the `limits! { ... }` invocations declare limits.
        in_limits = if in_limits {
            line != "}"
        } else {
            line == "limits! {"
        };
        let line = line.trim();
        if !in_limits {
            continue;
        }
        if let Some(doc) = line.strip_prefix("///") {
            docs.push_str(doc);
            continue;
        }
        let is_field = line.ends_with(',')
            && line.split_once(": ").is_some_and(|(name, _)| {
                !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            });
        if is_field {
            fields += 1;
            let names_section = docs.split("Document 0").skip(1).any(|rest| {
                rest.chars().next().is_some_and(|c| c.is_ascii_digit())
                    && rest[1..].starts_with(" section ")
            });
            assert!(
                names_section,
                "`{line}` has no doc comment naming `Document 0N section N`: {docs:?}"
            );
        }
        docs.clear();
    }
    assert_eq!(
        fields,
        F.entries().len() + P.entries().len(),
        "every limit was found in the source"
    );
}
