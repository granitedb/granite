//! The simulated disk allocates a zone's bytes on the first write to them,
//! and reads what was never written as zeros.

use granite_base::constants::FORMAT;
use granite_io::message_pool::{MessagePool, SECTOR_SIZE};
use granite_io::{Completion, Storage, Zone};
use granite_sim::{Disk, Faults, Group, Scheduler};

const SECTOR: u64 = SECTOR_SIZE as u64;
/// The disk's chunk: 64 KiB.
const CHUNK: u64 = 16 * SECTOR;

/// Waits for the one operation in flight and returns its buffer.
fn complete(disk: &mut Disk) -> granite_io::message_pool::Message {
    match disk.poll() {
        Some(
            Completion::Write { buffer, result, .. } | Completion::Read { buffer, result, .. },
        ) => {
            result.unwrap();
            buffer
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_untouched_range_reads_as_zeros_after_writes_elsewhere() {
    let pool = MessagePool::new(2);
    let mut sim = Scheduler::<()>::new(1, 1);
    let zones = [0, 0, 4 * FORMAT.message_size_max, 0];
    let group = Group::new(1, 1, zones);
    let mut disk = Disk::new(&group, 0, 1, Faults::default(), &mut sim);
    // Two sectors written: one across a chunk boundary in the first slot,
    // one in the third slot.
    let writes = [
        (CHUNK - SECTOR, 2 * SECTOR, 0xA5),
        (2 * FORMAT.message_size_max, SECTOR, 0x5A),
    ];
    for (offset, len, byte) in writes {
        let mut buffer = pool.acquire().unwrap();
        buffer.bytes_mut().unwrap().fill(byte);
        disk.write(0, Zone::WalPrepares, offset, len as u32, buffer)
            .unwrap();
        drop(complete(&mut disk));
    }
    // What was written reads back; the bytes around it, the slot between
    // and the last slot read as zeros, whatever the buffer held before.
    let reads = [
        (0, CHUNK - SECTOR, 0),
        (CHUNK - SECTOR, 2 * SECTOR, 0xA5),
        (CHUNK + SECTOR, SECTOR, 0),
        (FORMAT.message_size_max, FORMAT.message_size_max, 0),
        (2 * FORMAT.message_size_max, SECTOR, 0x5A),
        (2 * FORMAT.message_size_max + SECTOR, SECTOR, 0),
        (3 * FORMAT.message_size_max, FORMAT.message_size_max, 0),
    ];
    for (offset, len, byte) in reads {
        let mut buffer = pool.acquire().unwrap();
        buffer.bytes_mut().unwrap().fill(0xFF);
        disk.read(0, Zone::WalPrepares, offset, len as u32, buffer)
            .unwrap();
        let buffer = complete(&mut disk);
        assert!(
            buffer.bytes()[..len as usize].iter().all(|&b| b == byte),
            "at {offset}"
        );
    }
    assert_eq!(disk.size(Zone::WalPrepares), 4 * FORMAT.message_size_max);
}
