//! The gates of T1.1.2 that need a disk: the superblock opened and updated
//! through a simulated disk, with crashes at its I/O boundaries.
//!
//! The seeded tests run 1,000 seeds; `GRANITE_FULL=1` runs the 10,000 of the
//! seed gate, in about three minutes in a debug build.

use granite_base::constants::FORMAT;
use granite_io::memory::SplitMix64;
use granite_io::message_pool::{Message, MessagePool};
use granite_io::{Completion, Full, Random, Storage, Tag, Zone};
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
/// Operations of an update that rewrites nothing: a write and an fsync per
/// copy, then a read of each copy.
const STEPS: usize = 3 * COPIES;
/// Updates, each after a restart, in one seed.
const ROUNDS: usize = 4;

fn seeds() -> u64 {
    if std::env::var_os("GRANITE_FULL").is_some() {
        10_000
    } else {
        1_000
    }
}

/// A simulated disk that, with `misdirect` at `n`, lands each write with a
/// chance of 1 in `n` at the offset of another copy, at most once per run;
/// and, with `lose`, lands every write in the grid instead.
struct Io {
    disk: Disk,
    random: SplitMix64,
    misdirect: Option<u64>,
    misdirected: u32,
    lose: bool,
}

impl Storage for Io {
    fn size(&self, zone: Zone) -> u64 {
        self.disk.size(zone)
    }

    fn read(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        self.disk.read(tag, zone, offset, len, buffer)
    }

    fn write(
        &mut self,
        tag: Tag,
        zone: Zone,
        mut offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        if self.lose {
            return self.disk.write(tag, Zone::Grid, 0, len, buffer);
        }
        let copies = COPIES as u64;
        if let Some(n) = self.misdirect.filter(|_| self.misdirected == 0)
            && self.random.next_u64().is_multiple_of(n)
        {
            let other = offset / COPY as u64 + 1 + self.random.next_u64() % (copies - 1);
            offset = other % copies * COPY as u64;
            self.misdirected += 1;
        }
        self.disk.write(tag, zone, offset, len, buffer)
    }

    fn fsync(&mut self, tag: Tag) -> Result<(), Full<()>> {
        self.disk.fsync(tag)
    }

    fn grow_grid(&mut self, tag: Tag, size: u64) -> Result<(), Full<()>> {
        self.disk.grow_grid(tag, size)
    }
}

fn io(sim: &mut Scheduler<()>, faults: Faults, capacity: usize, misdirect: Option<u64>) -> Io {
    let sector = FORMAT.sector_size;
    let zones = [ZONE as u64, sector, sector, COPY as u64];
    Io {
        disk: Disk::new(&Group::new(1, 1, zones), 0, capacity, faults, sim),
        random: SplitMix64(sim.next_u64()),
        misdirect,
        misdirected: 0,
        lose: false,
    }
}

/// Writes `zone` over the superblock zone, one write per copy.
fn put(io: &mut Io, pool: &MessagePool, zone: &[u8]) {
    for (i, copy) in zone.chunks_exact(COPY).enumerate() {
        let mut buffer = pool.acquire().unwrap();
        buffer.bytes_mut().unwrap()[..COPY].copy_from_slice(copy);
        let offset = (i * COPY) as u64;
        io.disk
            .write(0, Zone::Superblock, offset, COPY as u32, buffer)
            .unwrap();
        assert!(matches!(io.disk.poll(), Some(Completion::Write { .. })));
    }
}

/// The superblock zone as the disk holds it.
fn get(io: &mut Io, pool: &MessagePool) -> Vec<u8> {
    let buffer = pool.acquire().unwrap();
    io.disk
        .read(0, Zone::Superblock, 0, ZONE as u32, buffer)
        .unwrap();
    let Some(Completion::Read { buffer, result, .. }) = io.disk.poll() else {
        panic!("a read completes");
    };
    result.unwrap();
    buffer.bytes()[..ZONE].to_vec()
}

/// Delivers completions to `sb` until its open or update finishes, or, with
/// `crash` at `k`, crashes the disk once `k` completions are delivered.
/// Returns the outcome, or `None` after a crash.
fn drive(io: &mut Io, sb: &mut Superblock, crash: Option<usize>) -> Option<Result<(), OpenError>> {
    for step in 0.. {
        if crash == Some(step) {
            io.disk.crash();
            return None;
        }
        let completion = io.disk.poll().expect("one operation in flight");
        if let Some(outcome) = sb.complete(io, completion) {
            return Some(outcome);
        }
    }
    unreachable!()
}

fn opened(io: &mut Io, pool: &MessagePool) -> (Superblock, Result<(), OpenError>) {
    let mut sb = Superblock::new(A, TAG, pool.acquire().unwrap());
    sb.open(io);
    let outcome = drive(io, &mut sb, None).unwrap();
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
/// opens, through an [`Io`] that misdirects as `misdirect` says. Every
/// finished open must succeed and return the record last made durable or the
/// one being written, never anything else, and never a lower sequence number.
/// Counts the crash positions of updates in `coverage`.
fn seed(
    seed: u64,
    pool: &MessagePool,
    misdirect: Option<u64>,
    torn_writes: bool,
    coverage: &mut [u32; STEPS],
) -> u32 {
    let mut sim = Scheduler::new(seed, 1);
    let faults = Faults {
        torn_writes,
        ..Faults::default()
    };
    let mut io = io(&mut sim, faults, 1, misdirect);
    put(&mut io, pool, &superblock::format(A));
    let mut durable = Record::new(A);
    // The record of the update a crash interrupted, if any.
    let mut writing: Option<Record> = None;
    for _ in 0..ROUNDS {
        let mut sb = loop {
            let mut sb = Superblock::new(A, TAG, pool.acquire().unwrap());
            sb.open(&mut io);
            let crash = (sim.next_u64() % 4 == 0).then(|| (sim.next_u64() % 16) as usize);
            let Some(outcome) = drive(&mut io, &mut sb, crash) else {
                continue;
            };
            assert_eq!(outcome, Ok(()), "seed {seed}: open failed");
            let record = sb.record().unwrap();
            assert!(
                record.sequence >= durable.sequence,
                "seed {seed}: the sequence number dropped"
            );
            assert!(
                *record == durable || Some(record) == writing.as_ref(),
                "seed {seed}: open returned neither the old record nor the new one"
            );
            durable = record.clone();
            break sb;
        };
        let record = after(&durable, &mut sim);
        writing = Some(record.clone());
        sb.update(&mut io, record);
        let crash = (sim.next_u64() % 8 != 0).then(|| (sim.next_u64() % STEPS as u64) as usize);
        match drive(&mut io, &mut sb, crash) {
            None => coverage[crash.unwrap()] += 1,
            Some(outcome) => {
                assert_eq!(outcome, Ok(()));
                assert_eq!(sb.record(), writing.as_ref());
                durable = writing.take().unwrap();
            }
        }
    }
    io.misdirected
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
    let seeds = seeds();
    let pool = MessagePool::new(3);
    let mut coverage = [0; STEPS];
    for s in 0..seeds {
        // Torn writes on or off, from the seed.
        seed(s, &pool, None, s % 2 == 1, &mut coverage);
    }
    println!("{seeds} seeds; crashes by position in the update:");
    for (step, count) in coverage.iter().enumerate() {
        let copy = step / 2;
        let at = match step {
            _ if step >= 2 * COPIES => {
                format!("during the read back of copy {}", step - 2 * COPIES)
            }
            _ if step % 2 == 0 => format!("during the write of copy {copy}"),
            _ if copy + 1 < COPIES => {
                format!("between the writes of copies {copy} and {}", copy + 1)
            }
            _ => format!("after the write of copy {copy}, before the read back"),
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
        let mut io = io(&mut sim, Faults::default(), 1, None);
        let mut zone = formatted.clone();
        zone[corrupt * COPY + 100] ^= 1;
        put(&mut io, &pool, &zone);
        let (sb, outcome) = opened(&mut io, &pool);
        assert_eq!(outcome, Ok(()));
        assert_eq!(sb.record(), Some(&Record::new(A)));
        assert_eq!(get(&mut io, &pool), formatted, "copy {corrupt} rewritten");
    }
}

/// Gate: with no quorum, open fails with a typed error and writes nothing.
#[test]
fn open_without_a_quorum_fails_and_writes_nothing() {
    let pool = MessagePool::new(2);
    let mut sim = Scheduler::new(0, 1);
    let mut io = io(&mut sim, Faults::default(), 1, None);
    let mut zone = superblock::format(A);
    for copy in 1..COPIES {
        zone[copy * COPY + 100] ^= 1;
    }
    put(&mut io, &pool, &zone);
    let (sb, outcome) = opened(&mut io, &pool);
    assert_eq!(outcome, Err(OpenError::NoQuorum));
    assert_eq!(sb.record(), None);
    assert!(io.disk.poll().is_none(), "nothing in flight");
    assert_eq!(get(&mut io, &pool), zone);
}

/// Opens a formatted superblock and updates it to `sequence`.
fn update_to(sequence: u64) {
    let pool = MessagePool::new(2);
    let mut sim = Scheduler::new(0, 1);
    let mut io = io(&mut sim, Faults::default(), 1, None);
    put(&mut io, &pool, &superblock::format(A));
    let (mut sb, _) = opened(&mut io, &pool);
    let mut record = Record::new(A);
    record.sequence = sequence;
    sb.update(&mut io, record);
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
    let mut io = io(&mut sim, Faults::default(), 1, None);
    put(&mut io, &pool, &superblock::format(A));
    let (mut sb, _) = opened(&mut io, &pool);
    // Another operation fills the disk, so the update's first write is refused.
    io.disk.fsync(0).unwrap();
    let record = after(&Record::new(A), &mut sim);
    sb.update(&mut io, record.clone());
    sb.resume(&mut io);
    assert!(matches!(
        io.disk.poll(),
        Some(Completion::Fsync { tag: 0, .. })
    ));
    assert!(io.disk.poll().is_none(), "the write was refused");
    sb.resume(&mut io);
    assert_eq!(drive(&mut io, &mut sb, None), Some(Ok(())));
    let (sb, outcome) = opened(&mut io, &pool);
    assert_eq!((outcome, sb.record()), (Ok(()), Some(&record)));
}

/// An unreadable copy counts as a corrupt one, whatever an earlier use left
/// in the buffer: after its first reads, open goes on while a quorum of
/// copies is readable, and fails otherwise. (At this rate of read errors the
/// read back would keep finding copies to rewrite and stop the replica.)
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
        let mut io = io(&mut sim, faults, COPIES, None);
        put(&mut io, &pool, &superblock::format(A));
        let mut sb = Superblock::new(A, TAG, pool.acquire().unwrap());
        sb.open(&mut io);
        let mut outcome = None;
        for _ in 0..COPIES {
            let completion = io.disk.poll().unwrap();
            outcome = sb.complete(&mut io, completion);
        }
        let unreadable = io.disk.faults().count();
        if unreadable <= COPIES - superblock::QUORUM {
            assert!(matches!(outcome, None | Some(Ok(()))), "seed {seed}");
        } else {
            assert_eq!(outcome, Some(Err(OpenError::NoQuorum)), "seed {seed}");
        }
        outcomes[usize::from(unreadable > COPIES - superblock::QUORUM)] += 1;
    }
    assert!(outcomes.iter().all(|&n| n > 0), "{outcomes:?}");
}

/// Crashes in updates and opens, and one write per run that may land at
/// another copy's offset. Every finished open succeeds, returns the old
/// record or the new one, and its sequence number never drops.
///
/// Torn writes are off: a misdirected write and a torn one in the same
/// update can leave one copy of each record (document 01 section 2), and
/// with them on this test fails on seed 191.
///
/// Injected bug: without the read back after an update or a repair, this
/// test fails on seed 167 with "open failed" (`NoQuorum`).
#[test]
fn a_misdirected_write_and_crashes_open_the_old_record_or_the_new_one() {
    let seeds = seeds();
    let pool = MessagePool::new(3);
    let mut coverage = [0; STEPS];
    let misdirected: u32 = (0..seeds)
        .map(|s| seed(s, &pool, Some(6), false, &mut coverage))
        .sum();
    println!("{seeds} seeds, {misdirected} misdirected writes");
    assert!(misdirected > seeds as u32 / 2);
}

/// A device that loses every write stops the replica after
/// `READ_BACKS_MAX` reads of the copies, rather than rewriting forever.
#[test]
#[should_panic(expected = "the superblock copies do not hold what was written")]
fn writes_that_never_land_stop_the_replica() {
    let pool = MessagePool::new(2);
    let mut sim = Scheduler::new(0, 1);
    let mut io = io(&mut sim, Faults::default(), 1, None);
    let mut zone = superblock::format(A);
    zone[100] ^= 1;
    put(&mut io, &pool, &zone);
    io.lose = true;
    let _ = opened(&mut io, &pool);
}
