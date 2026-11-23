//! The gates of T1.1.2 that need a disk: the superblock opened and updated
//! through a simulated disk, with crashes at its I/O boundaries.

use granite_base::constants::FORMAT;
use granite_io::message_pool::MessagePool;
use granite_io::{Completion, Random, Storage, Tag, Zone};
use granite_sim::{Disk, Faults, Group, Scheduler};
use granite_storage::superblock::{
    self, COPIES, HEADERS_MAX, Identity, OpenError, Record, Reference, Superblock,
};

const A: Identity = Identity {
    cluster: 0xA,
    group: 7,
    replica: 2,
};
const COPY: usize = FORMAT.superblock_copy_size as usize;
const ZONE: usize = COPIES * COPY;
const TAG: Tag = 7;
/// Operations of an update: a write and an fsync per copy.
const STEPS: usize = 2 * COPIES;
/// Updates, each after a restart, in one seed.
const ROUNDS: usize = 4;

fn disk(sim: &mut Scheduler<()>, faults: Faults, capacity: usize) -> Disk {
    let sector = FORMAT.sector_size;
    let zones = [ZONE as u64, sector, sector, sector];
    Disk::new(&Group::new(1, 1, zones), 0, capacity, faults, sim)
}

/// Writes `zone` over the superblock zone, one write per copy.
fn put(disk: &mut Disk, pool: &MessagePool, zone: &[u8]) {
    for (i, copy) in zone.chunks_exact(COPY).enumerate() {
        let mut buffer = pool.acquire().unwrap();
        buffer.bytes_mut().unwrap()[..COPY].copy_from_slice(copy);
        let offset = (i * COPY) as u64;
        disk.write(0, Zone::Superblock, offset, COPY as u32, buffer)
            .unwrap();
        assert!(matches!(disk.poll(), Some(Completion::Write { .. })));
    }
}

/// The superblock zone as the disk holds it.
fn get(disk: &mut Disk, pool: &MessagePool) -> Vec<u8> {
    let buffer = pool.acquire().unwrap();
    disk.read(0, Zone::Superblock, 0, ZONE as u32, buffer)
        .unwrap();
    let Some(Completion::Read { buffer, result, .. }) = disk.poll() else {
        panic!("a read completes");
    };
    result.unwrap();
    buffer.bytes()[..ZONE].to_vec()
}

/// Delivers completions to `sb` until its open or update finishes, or, with
/// `crash` at `k`, crashes the disk once `k` completions are delivered.
/// Returns the outcome, or `None` after a crash.
fn drive(
    disk: &mut Disk,
    sb: &mut Superblock,
    crash: Option<usize>,
) -> Option<Result<(), OpenError>> {
    for step in 0.. {
        if crash == Some(step) {
            disk.crash();
            return None;
        }
        let completion = disk.poll().expect("one operation in flight");
        if let Some(outcome) = sb.complete(disk, completion) {
            return Some(outcome);
        }
    }
    unreachable!()
}

fn opened(disk: &mut Disk, pool: &MessagePool) -> (Superblock, Result<(), OpenError>) {
    let mut sb = Superblock::new(A, TAG, pool.acquire().unwrap());
    sb.open(disk);
    let outcome = drive(disk, &mut sb, None).unwrap();
    (sb, outcome)
}

/// A record after `previous`, whose headers span a random number of sectors.
fn after(previous: &Record, sim: &mut Scheduler<()>) -> Record {
    let mut record = previous.clone();
    record.sequence += 1;
    record.vsr.view = sim.next_u64() as u32;
    record.vsr.commit_op = sim.next_u64();
    let count = (sim.next_u64() % (HEADERS_MAX as u64 + 1)) as usize;
    let fill = sim.next_u64() as u8;
    record.vsr.headers = (0..count)
        .map(|i| [fill ^ i as u8; FORMAT.header_size as usize])
        .collect();
    record.checkpoint.manifest = Reference {
        address: sim.next_u64(),
        checksum: u128::from(sim.next_u64()),
    };
    record
}

/// One seed: `ROUNDS` restarts, each opening the superblock and then
/// updating it, with a crash at a random step of most updates and of some
/// opens. Every open must return the record last made durable or the one
/// being written, never anything else and never an error. Counts the crash
/// positions of updates in `coverage`.
fn seed(seed: u64, pool: &MessagePool, coverage: &mut [u32; STEPS]) {
    let mut sim = Scheduler::new(seed, 1);
    let torn_writes = sim.next_u64() & 1 == 1;
    let faults = Faults {
        torn_writes,
        ..Faults::default()
    };
    let mut disk = disk(&mut sim, faults, 1);
    put(&mut disk, pool, &superblock::format(A));
    let mut durable = Record::new(A);
    // The record of the update a crash interrupted, if any.
    let mut writing: Option<Record> = None;
    for _ in 0..ROUNDS {
        let mut sb = loop {
            let mut sb = Superblock::new(A, TAG, pool.acquire().unwrap());
            sb.open(&mut disk);
            let crash = (sim.next_u64() % 4 == 0).then(|| (sim.next_u64() % 12) as usize);
            let Some(outcome) = drive(&mut disk, &mut sb, crash) else {
                continue;
            };
            assert_eq!(outcome, Ok(()), "seed {seed}: open failed");
            let record = sb.record().unwrap();
            assert!(
                *record == durable || Some(record) == writing.as_ref(),
                "seed {seed}: open returned neither the old record nor the new one"
            );
            durable = record.clone();
            break sb;
        };
        let record = after(&durable, &mut sim);
        writing = Some(record.clone());
        sb.update(&mut disk, record);
        let crash = (sim.next_u64() % 8 != 0).then(|| (sim.next_u64() % STEPS as u64) as usize);
        match drive(&mut disk, &mut sb, crash) {
            None => coverage[crash.unwrap()] += 1,
            Some(outcome) => {
                assert_eq!(outcome, Ok(()));
                assert_eq!(sb.record(), writing.as_ref());
                durable = writing.take().unwrap();
            }
        }
    }
}

/// Gates: seed gate 10,000 with a crash injected during superblock updates,
/// every open returning exactly the old record or the new one; and a crash
/// between every pair of consecutive writes of an update, listed in a
/// coverage report.
///
/// Injected bug: an update that writes the four copies in one write of the
/// whole zone, so that no quorum of copies keeps the old record while it is
/// in flight, fails this test on seed 1 with "open failed" (`NoQuorum`).
#[test]
fn a_crash_during_an_update_opens_the_old_record_or_the_new_one() {
    let seeds = 10_000;
    let pool = MessagePool::new(3);
    let mut coverage = [0; STEPS];
    for s in 0..seeds {
        seed(s, &pool, &mut coverage);
    }
    println!("{seeds} seeds; crashes by position in the update:");
    for (step, count) in coverage.iter().enumerate() {
        let (copy, position) = (step / 2, step % 2);
        let at = if position == 0 {
            format!("during the write of copy {copy}")
        } else if copy + 1 < COPIES {
            format!("between the writes of copies {copy} and {}", copy + 1)
        } else {
            format!("after the write of copy {copy}, before the update finishes")
        };
        println!("  {at}: {count}");
    }
    assert!(coverage.iter().all(|&count| count > 0), "{coverage:?}");
}

/// Gate: with any one copy corrupted, open succeeds and rewrites it.
#[test]
fn open_rewrites_one_corrupt_copy() {
    let pool = MessagePool::new(2);
    let formatted = superblock::format(A);
    for corrupt in 0..COPIES {
        let mut sim = Scheduler::new(corrupt as u64, 1);
        let mut disk = disk(&mut sim, Faults::default(), 1);
        let mut zone = formatted.clone();
        zone[corrupt * COPY + 100] ^= 1;
        put(&mut disk, &pool, &zone);
        let (sb, outcome) = opened(&mut disk, &pool);
        assert_eq!(outcome, Ok(()));
        assert_eq!(sb.record(), Some(&Record::new(A)));
        assert_eq!(get(&mut disk, &pool), formatted, "copy {corrupt} rewritten");
    }
}

/// Gate: with no quorum, open fails with a typed error and writes nothing.
#[test]
fn open_without_a_quorum_fails_and_writes_nothing() {
    let pool = MessagePool::new(2);
    let mut sim = Scheduler::new(0, 1);
    let mut disk = disk(&mut sim, Faults::default(), 1);
    let mut zone = superblock::format(A);
    for copy in 1..COPIES {
        zone[copy * COPY + 100] ^= 1;
    }
    put(&mut disk, &pool, &zone);
    let (sb, outcome) = opened(&mut disk, &pool);
    assert_eq!(outcome, Err(OpenError::NoQuorum));
    assert_eq!(sb.record(), None);
    assert!(disk.poll().is_none(), "nothing in flight");
    assert_eq!(get(&mut disk, &pool), zone);
}

/// Opens a formatted superblock and updates it to `sequence`.
fn update_to(sequence: u64) {
    let pool = MessagePool::new(2);
    let mut sim = Scheduler::new(0, 1);
    let mut disk = disk(&mut sim, Faults::default(), 1);
    put(&mut disk, &pool, &superblock::format(A));
    let (mut sb, _) = opened(&mut disk, &pool);
    let mut record = Record::new(A);
    record.sequence = sequence;
    sb.update(&mut disk, record);
}

/// Gate: an update to a sequence number other than the next is rejected.
#[test]
#[should_panic(expected = "an update follows the record on disk")]
fn an_update_to_the_same_sequence_number_is_rejected() {
    update_to(0);
}

#[test]
#[should_panic(expected = "an update follows the record on disk")]
fn an_update_that_skips_a_sequence_number_is_rejected() {
    update_to(2);
}

/// A storage that refuses a submission gets it again from `resume`.
#[test]
fn a_refused_submission_is_resumed() {
    let pool = MessagePool::new(3);
    let mut sim = Scheduler::new(0, 1);
    let mut disk = disk(&mut sim, Faults::default(), 1);
    put(&mut disk, &pool, &superblock::format(A));
    let (mut sb, _) = opened(&mut disk, &pool);
    // Another operation fills the disk, so the update's first write is refused.
    disk.fsync(0).unwrap();
    let record = after(&Record::new(A), &mut sim);
    sb.update(&mut disk, record.clone());
    sb.resume(&mut disk);
    assert!(matches!(
        disk.poll(),
        Some(Completion::Fsync { tag: 0, .. })
    ));
    assert!(disk.poll().is_none(), "the write was refused");
    sb.resume(&mut disk);
    assert_eq!(drive(&mut disk, &mut sb, None), Some(Ok(())));
    let (sb, outcome) = opened(&mut disk, &pool);
    assert_eq!((outcome, sb.record()), (Ok(()), Some(&record)));
}

/// An unreadable copy counts as a corrupt one, whatever an earlier use left
/// in the buffer: open succeeds while a quorum of copies is readable, and
/// fails otherwise.
#[test]
fn an_unreadable_copy_counts_as_corrupt() {
    let pool = MessagePool::new(3);
    let mut outcomes = [0; 2];
    for seed in 0..200 {
        let mut sim = Scheduler::new(seed, 1);
        let faults = Faults {
            read_error: granite_sim::storage::MILLION / 2,
            ..Faults::default()
        };
        // Room in the fault log for an error on every copy.
        let mut disk = disk(&mut sim, faults, COPIES);
        put(&mut disk, &pool, &superblock::format(A));
        let (sb, outcome) = opened(&mut disk, &pool);
        let unreadable = disk.faults().count();
        if unreadable <= COPIES - superblock::QUORUM {
            assert_eq!(outcome, Ok(()), "seed {seed}");
            assert_eq!(sb.record(), Some(&Record::new(A)));
        } else {
            assert_eq!(outcome, Err(OpenError::NoQuorum), "seed {seed}");
        }
        outcomes[usize::from(outcome.is_err())] += 1;
    }
    assert!(outcomes.iter().all(|&n| n > 0), "{outcomes:?}");
}
