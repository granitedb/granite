//! Insert and lookup throughput for 1 KiB rows, for the report in
//! docs/decisions/t1-2-1-memtable-throughput.md. Ignored by default; run with
//! `cargo test --release --locked -p granite-lsm --test throughput -- --ignored --nocapture`.

use granite_lsm::memtable::Memtable;
use std::time::Instant;

#[test]
#[ignore = "a measurement, run by hand in release mode"]
fn rows_of_one_kib() {
    const ROWS: u64 = 1_000_000;
    // 16-byte key + 1008-byte value = 1 KiB per row, plus 13 to 61 bytes of node.
    let value = [0xA5u8; 1008];
    let key = |i: u64| {
        let x = i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&x.to_be_bytes());
        key[8..].copy_from_slice(&i.to_be_bytes());
        key
    };
    let mut memtable = Memtable::new(1100 << 20);

    let start = Instant::now();
    for i in 0..ROWS {
        memtable
            .insert(&key(i), &value)
            .expect("the arena has room");
    }
    let insert = start.elapsed();

    let start = Instant::now();
    let mut found = 0u64;
    for i in 0..ROWS {
        // A different order from the inserts.
        let i = i.wrapping_mul(0x2545_F491_4F6C_DD1D) % ROWS;
        found += u64::from(memtable.get(&key(i)).is_some());
    }
    let lookup = start.elapsed();
    assert_eq!(found, ROWS);

    let rate = |d: std::time::Duration| ROWS as f64 / d.as_secs_f64();
    println!("rows: {ROWS}, arena used: {} bytes", memtable.bytes().len());
    println!(
        "insert: {insert:?}, {:.0} rows/s, {:.0} ns/row",
        rate(insert),
        insert.as_nanos() as f64 / ROWS as f64
    );
    println!(
        "lookup: {lookup:?}, {:.0} rows/s, {:.0} ns/row",
        rate(lookup),
        lookup.as_nanos() as f64 / ROWS as f64
    );
}
