//! The gates of T1.1.4, WAL recovery, on the simulated disk: each row of the
//! classification table in document 01 (WAL recovery) on the exact on-disk
//! state it names; crashes during appends never make a slot corrupt nor lose
//! a completed append; bit flips never make a slot torn; and every recovery
//! reads the header ring once and each slot once.
//!
//! Set `GRANITE_FULL=1` for the 10,000 seeds of the seed gates, about 20
//! minutes with `--release`; the default is 20.

use granite_base::checksum::checksum;
use granite_base::constants::FORMAT;
use granite_base::header::{Command, Header, VERSION};
use granite_io::message_pool::{Message, MessagePool};
use granite_io::{Completion, Full, Random, Storage, Tag, Zone};
use granite_sim::storage::MILLION;
use granite_sim::{Disk, Fault, FaultKind, Faults, Group, Scheduler};
use granite_storage::layout::zone_sizes;
use granite_storage::wal::recovery::{Recovery, Repair, Slot};
use granite_storage::wal::{Event, Wal};
use std::mem::offset_of;

const HEADER: usize = FORMAT.header_size as usize;
const SLOTS: u64 = FORMAT.wal_slot_count;
const SLOT_SIZE: u64 = FORMAT.message_size_max;
const SECTOR: usize = FORMAT.sector_size as usize;
/// The log's first tag, and the tags of the test's own raw writes.
const TAG: Tag = 1 << 40;
const RAW: Tag = 7;
/// The checksum the first prepare names as its parent.
const ROOT: u128 = 0x5eed;
/// A head at which every slot holds an op and the ring has wrapped: slots 1
/// to 10 hold ops `SLOTS + 1` to `SLOTS + 10`, the others their own index.
const WRAPPED: u64 = SLOTS + 10;

fn seeds() -> u64 {
    if std::env::var_os("GRANITE_FULL").is_some() {
        10_000
    } else {
        20
    }
}

/// The zones of a data file, with the grid cut to one sector.
fn zones() -> [u64; 4] {
    let mut zones = zone_sizes(FORMAT.grid_growth_step);
    zones[Zone::Grid as usize] = FORMAT.sector_size;
    zones
}

/// A body length that differs from op to op: one to four sectors in all.
fn body_len(op: u64) -> usize {
    (op * 7919 % (4 * SECTOR as u64 - HEADER as u64)) as usize
}

/// Prepare `op` with parent `parent` and a `len`-byte body, zero to the end
/// of its last sector, and its header checksum.
fn prepare(op: u64, parent: u128, len: usize) -> (Vec<u8>, u128) {
    let size = HEADER + len;
    let mut bytes = vec![0; size.next_multiple_of(SECTOR)];
    for (i, b) in bytes[HEADER..size].iter_mut().enumerate() {
        *b = (op as u8) ^ (i as u8) ^ (parent as u8) ^ 0xa5;
    }
    let mut put = |offset: usize, value: &[u8]| {
        bytes[offset..offset + value.len()].copy_from_slice(value);
    };
    put(offset_of!(Header, command), &[Command::Prepare as u8]);
    put(offset_of!(Header, version), &[VERSION]);
    put(offset_of!(Header, op), &op.to_le_bytes());
    put(offset_of!(Header, parent), &parent.to_le_bytes());
    put(offset_of!(Header, size), &(size as u32).to_le_bytes());
    let body = checksum(&bytes[HEADER..size]);
    bytes[offset_of!(Header, checksum_body)..][..16].copy_from_slice(&body.to_le_bytes());
    let sum = checksum(&bytes[offset_of!(Header, checksum_body)..HEADER]);
    bytes[..16].copy_from_slice(&sum.to_le_bytes());
    assert!(Header::decode(&bytes).is_ok_and(|h| h.op.get() == op));
    (bytes, sum)
}

/// A disk that counts recovery's reads: of the header ring, and of each slot.
struct Counted<'a> {
    disk: &'a mut Disk,
    reads: &'a mut [u32],
}

impl Storage for Counted<'_> {
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
        let at = match zone {
            Zone::WalHeaders => {
                assert_eq!((offset, u64::from(len)), (0, SLOTS * HEADER as u64));
                SLOTS
            }
            Zone::WalPrepares => {
                assert_eq!(offset % SLOT_SIZE, 0, "a read starts at a slot");
                offset / SLOT_SIZE
            }
            _ => panic!("recovery reads only the log's rings"),
        };
        self.disk.read(tag, zone, offset, len, buffer)?;
        self.reads[at as usize] += 1;
        Ok(())
    }

    fn write(&mut self, _: Tag, _: Zone, _: u64, _: u32, _: Message) -> Result<(), Full<Message>> {
        panic!("recovery only reads")
    }

    fn fsync(&mut self, _: Tag) -> Result<(), Full<()>> {
        panic!("recovery only reads")
    }

    fn grow_grid(&mut self, _: Tag, _: u64) -> Result<(), Full<()>> {
        panic!("recovery only reads")
    }
}

/// One replica's disk, and the log on it through crashes and recoveries.
struct Run {
    sim: Scheduler<()>,
    disk: Disk,
    pool: MessagePool,
    /// Every fault injected so far.
    faults: Vec<Fault>,
    /// By op, its header checksum (op 0 is the root), whether its append
    /// completed, and whether a later op's append was submitted to its slot.
    checksums: Vec<u128>,
    acknowledged: Vec<bool>,
    overwritten: Vec<bool>,
}

impl Run {
    /// Replica 0 of a group of `replicas` with a quorum of 2 (1 alone), so
    /// that faults may damage any one replica's slots.
    fn new(seed: u64, replicas: usize, faults: Faults) -> Run {
        let mut sim = Scheduler::new(seed, 1);
        let group = Group::new(replicas, replicas.min(2), zones());
        let disk = Disk::new(&group, 0, 6, faults, &mut sim);
        Run {
            sim,
            disk,
            pool: MessagePool::new(16),
            faults: Vec::new(),
            checksums: vec![ROOT],
            acknowledged: vec![true],
            overwritten: vec![false],
        }
    }

    /// The next completion, moving virtual time to the next due operation
    /// if none is due; `None` when nothing is in flight. Drains the fault
    /// log, which stops injecting when full.
    fn next(&mut self) -> Option<Completion> {
        let completion = self.disk.poll().or_else(|| {
            let at = self.disk.ready_at()?;
            self.sim.schedule(at, ()).unwrap();
            self.sim.step();
            Some(self.disk.poll().expect("an operation is due"))
        });
        self.faults.extend(self.disk.faults());
        completion
    }

    /// Delivers one completion to `wal`; `false` when nothing is in flight.
    fn step(&mut self, wal: &mut Wal) -> bool {
        let Some(completion) = self.next() else {
            return false;
        };
        let acknowledged = &mut self.acknowledged;
        wal.complete(&mut self.disk, completion, |event| match event {
            Event::Appended { op } => acknowledged[op as usize] = true,
            Event::Read { .. } => unreachable!("the test reads nothing through the log"),
        });
        true
    }

    /// Submits the append of the next op, delivering completions until the
    /// log takes it.
    fn append(&mut self, wal: &mut Wal) {
        let op = self.checksums.len() as u64;
        let (bytes, sum) = prepare(op, self.checksums[op as usize - 1], body_len(op));
        let mut message = self.message(&bytes);
        loop {
            match wal.append(&mut self.disk, message) {
                Ok(()) => break,
                Err(Full(back)) => message = back,
            }
            assert!(self.step(wal), "a refused append has something to wait for");
        }
        self.checksums.push(sum);
        self.acknowledged.push(false);
        self.overwritten.push(false);
        if op > SLOTS {
            self.overwritten[(op - SLOTS) as usize] = true;
        }
    }

    /// A pool buffer holding `bytes`.
    fn message(&self, bytes: &[u8]) -> Message {
        let mut message = self.pool.acquire().expect("a free buffer");
        message.bytes_mut().unwrap()[..bytes.len()].copy_from_slice(bytes);
        message
    }

    /// Writes `bytes` at `offset` of `zone`, as the process asked.
    fn write_raw(&mut self, zone: Zone, offset: u64, bytes: &[u8]) {
        let message = self.message(bytes);
        let len = u32::try_from(bytes.len()).unwrap();
        self.disk.write(RAW, zone, offset, len, message).unwrap();
        assert!(matches!(
            self.next(),
            Some(Completion::Write { result: Ok(()), .. })
        ));
    }

    /// Recovers the log, checking that it reads the header ring once and
    /// each slot once.
    fn recover(&mut self) -> Recovery {
        let mut recovery = Recovery::new(TAG);
        let mut reads = [0; SLOTS as usize + 1];
        let mut io = Counted {
            disk: &mut self.disk,
            reads: &mut reads,
        };
        recovery.pump(&mut io, &self.pool);
        while !recovery.done() {
            let completion = self.next().expect("recovery has a read in flight");
            let mut io = Counted {
                disk: &mut self.disk,
                reads: &mut reads,
            };
            recovery.complete(&mut io, &self.pool, completion);
        }
        assert!(self.disk.poll().is_none() && self.disk.ready_at().is_none());
        assert!(
            reads.iter().all(|&n| n == 1),
            "the header ring is read once and each slot once"
        );
        recovery
    }

    /// Forgets the ops beyond the recovered head and opens the log after it.
    fn reopen(&mut self, recovery: &Recovery) -> Wal {
        let keep = recovery.head() as usize + 1;
        self.checksums.truncate(keep);
        self.acknowledged.truncate(keep);
        self.overwritten.truncate(keep);
        recovery.wal(TAG, self.pool.acquire().unwrap(), ROOT)
    }

    /// The header ring as it is on disk.
    fn ring(&mut self) -> Vec<u8> {
        let len = u32::try_from(SLOTS * HEADER as u64).unwrap();
        let buffer = self.pool.acquire().unwrap();
        self.disk
            .read(RAW, Zone::WalHeaders, 0, len, buffer)
            .unwrap();
        match self.next() {
            Some(Completion::Read {
                buffer,
                result: Ok(()),
                ..
            }) => buffer.bytes()[..len as usize].to_vec(),
            _ => panic!("the raw read completes"),
        }
    }
}

// The rows of the table. Each builds the log of ops 1 to the head as
// completed appends leave it, written raw, changes one slot into the row's
// state, and recovers.

/// The on-disk content of the log: the header ring and each slot's prepare
/// (`None` leaves the slot's zeros).
struct Image {
    ring: Vec<u8>,
    slots: Vec<Option<Vec<u8>>>,
    checksums: Vec<u128>,
}

impl Image {
    /// The log of ops 1 to `head`, every append completed.
    fn new(head: u64) -> Image {
        let mut image = Image {
            ring: vec![0; SLOTS as usize * HEADER],
            slots: vec![None; SLOTS as usize],
            checksums: vec![ROOT],
        };
        for op in 1..=head {
            let (bytes, sum) = image.prepare(op);
            image.header(op % SLOTS).copy_from_slice(&bytes[..HEADER]);
            image.slots[(op % SLOTS) as usize] = Some(bytes);
            image.checksums.push(sum);
        }
        image
    }

    /// Op `op`'s prepare, chained to the op before it.
    fn prepare(&self, op: u64) -> (Vec<u8>, u128) {
        prepare(op, self.checksums[op as usize - 1], body_len(op))
    }

    /// The header ring's entry for `slot`.
    fn header(&mut self, slot: u64) -> &mut [u8] {
        &mut self.ring[slot as usize * HEADER..][..HEADER]
    }

    /// The prepare slot `slot`.
    fn slot(&mut self, slot: u64) -> &mut Vec<u8> {
        self.slots[slot as usize].as_mut().expect("a written slot")
    }

    fn recover(self) -> Recovery {
        let mut run = Run::new(0, 1, Faults::default());
        run.write_raw(Zone::WalHeaders, 0, &self.ring);
        for (slot, bytes) in self.slots.iter().enumerate() {
            if let Some(bytes) = bytes {
                run.write_raw(Zone::WalPrepares, slot as u64 * SLOT_SIZE, bytes);
            }
        }
        run.recover()
    }
}

/// Recovers `image` and checks that `slot` is `class`, every other slot
/// below `head` clean, the head certain unless `uncertain`, and `repairs`
/// what recovery asks for.
fn check(image: Image, head: u64, slot: u64, class: Slot, uncertain: bool, repairs: &[Repair]) {
    let checksums = image.checksums.clone();
    let recovery = image.recover();
    assert_eq!(recovery.slot(slot), class, "slot {slot}");
    assert_eq!(recovery.head(), head);
    assert_eq!(
        recovery.head_checksum(),
        (head > 0).then(|| checksums[head as usize])
    );
    assert_eq!(recovery.uncertain(), uncertain);
    for other in (0..SLOTS).filter(|&other| other != slot) {
        let expected = if head < SLOTS && (other == 0 || other > head) {
            Slot::Missing
        } else {
            Slot::Clean
        };
        assert_eq!(recovery.slot(other), expected, "slot {other}");
    }
    assert_eq!(recovery.repairs().collect::<Vec<_>>(), repairs);
}

/// Flips one bit of `bytes`, at `byte`.
fn flip(bytes: &mut [u8], byte: usize) {
    bytes[byte] ^= 0x10;
}

#[test]
fn row_1_no_header_and_no_prepare_is_missing() {
    check(Image::new(10), 10, 500, Slot::Missing, false, &[]);
}

#[test]
fn row_2_a_prepare_without_its_header_is_torn() {
    // Op 11's prepare landed whole, or partly, before a crash; its header
    // never did.
    for damaged in [false, true] {
        let mut image = Image::new(10);
        let (mut bytes, _) = image.prepare(11);
        if damaged {
            flip(&mut bytes, HEADER);
        }
        image.slots[11] = Some(bytes);
        check(image, 10, 11, Slot::Torn, false, &[]);
    }
}

#[test]
fn row_3_a_lost_header_is_corrupt() {
    let mut image = Image::new(WRAPPED);
    image.header(5).fill(0);
    let repair = Repair {
        op: SLOTS + 5,
        checksum: None,
    };
    check(image, WRAPPED, 5, Slot::Corrupt, false, &[repair]);
}

#[test]
fn row_4_a_damaged_header_is_corrupt() {
    let mut image = Image::new(WRAPPED);
    flip(image.header(5), 40);
    let repair = Repair {
        op: SLOTS + 5,
        checksum: None,
    };
    check(image, WRAPPED, 5, Slot::Corrupt, false, &[repair]);

    // The entry of the op after the head: that op may have been appended.
    let mut image = Image::new(WRAPPED);
    flip(image.header(11), 40);
    let repair = Repair {
        op: 11,
        checksum: None,
    };
    check(image, WRAPPED, 11, Slot::Corrupt, true, &[repair]);
}

#[test]
fn row_5_a_stale_header_is_corrupt() {
    let mut image = Image::new(WRAPPED);
    let (old, _) = image.prepare(5);
    image.header(5).copy_from_slice(&old[..HEADER]);
    let repair = Repair {
        op: SLOTS + 5,
        checksum: None,
    };
    check(image, WRAPPED, 5, Slot::Corrupt, false, &[repair]);
}

#[test]
fn row_6_the_named_prepare_whole_is_clean() {
    check(Image::new(WRAPPED), WRAPPED, 5, Slot::Clean, false, &[]);
    check(Image::new(10), 10, 5, Slot::Clean, false, &[]);
}

#[test]
fn row_7_a_newer_prepare_over_the_named_one_is_torn() {
    // Op 11 is in the ring; the overwrite by op SLOTS + 11, the next append,
    // did not finish: its prepare is whole, or its first sector landed.
    for damaged in [false, true] {
        let mut image = Image::new(WRAPPED);
        let parent = image.checksums[WRAPPED as usize];
        let (mut bytes, _) = prepare(WRAPPED + 1, parent, 2 * SECTOR);
        if damaged {
            flip(&mut bytes, SECTOR);
        }
        image.slots[11] = Some(bytes);
        check(image, WRAPPED, 11, Slot::Torn, false, &[]);
    }
}

/// Rows 8 to 11: the slot does not hold the named prepare whole.
fn corrupt(change: impl FnOnce(&mut Image)) {
    let mut image = Image::new(WRAPPED);
    let repair = Repair {
        op: SLOTS + 5,
        checksum: Some(image.checksums[SLOTS as usize + 5]),
    };
    change(&mut image);
    check(image, WRAPPED, 5, Slot::Corrupt, false, &[repair]);
}

#[test]
fn row_8_the_named_prepare_with_a_damaged_body_is_corrupt() {
    assert!(body_len(SLOTS + 5) > 0);
    corrupt(|image| flip(image.slot(5), HEADER));
}

#[test]
fn row_9_a_damaged_prepare_header_is_corrupt() {
    corrupt(|image| flip(image.slot(5), 40));
}

#[test]
fn row_10_no_prepare_under_a_header_is_corrupt() {
    corrupt(|image| image.slot(5).fill(0));
}

#[test]
fn row_11_another_prepare_up_to_the_head_is_corrupt() {
    // An older op of the slot: the overwrite was lost.
    corrupt(|image| *image.slot(5) = image.prepare(5).0);
    // Another prepare of the same op, with another parent.
    corrupt(|image| *image.slot(5) = prepare(SLOTS + 5, 1, body_len(SLOTS + 5)).0);
}

/// Seed gate: crashes, with torn writes, during appends and no other fault.
/// Each seed runs one to three lives of the log, each appending up to three
/// rings' worth of prepares, crashing at a random point and recovering. No
/// slot is ever corrupt, the head is never uncertain, and every op whose
/// append completed, unless a later op's append was submitted to its slot,
/// is up to the head, clean, and the one appended.
///
/// Injected bugs, each failing this test with a corrupt slot:
/// - swapping `Slot::Torn` and `Slot::Corrupt` in the arms of `classify`
///   fails it on seed 0;
/// - writing a prepare whole at once (`let lead = false` in `Wal::append`)
///   fails it on seed 1: a torn overwrite leaves the old header over new
///   body sectors;
/// - letting a header into the ring before an earlier op's (in the loop
///   after a prepare's fsync, in `Wal::complete`) fails it on seed 5.
#[test]
fn a_crash_during_appends_never_corrupts_nor_loses_a_completed_append() {
    let seeds = seeds();
    let (mut crashes, mut torn, mut wrapped) = (0, 0, 0);
    for seed in 0..seeds {
        let faults = Faults {
            torn_writes: true,
            latency: MILLION / 2,
            latency_max: 1_000_000,
            ..Faults::default()
        };
        let mut run = Run::new(seed, 1, faults);
        let mut wal = Wal::new(TAG, run.pool.acquire().unwrap(), 0, ROOT);
        for _ in 0..1 + run.sim.next_u64() % 3 {
            for _ in 0..1 + run.sim.next_u64() % (3 * SLOTS) {
                run.append(&mut wal);
            }
            for _ in 0..run.sim.next_u64() % 32 {
                run.step(&mut wal);
            }
            run.disk.crash();
            run.faults.extend(run.disk.faults());
            drop(wal);
            crashes += 1;

            let recovery = run.recover();
            let head = recovery.head();
            let submitted = run.checksums.len() as u64 - 1;
            assert!(!recovery.uncertain(), "seed {seed}");
            assert!(head <= submitted, "seed {seed}");
            let ring = run.ring();
            for slot in 0..SLOTS {
                let class = recovery.slot(slot);
                assert_ne!(class, Slot::Corrupt, "seed {seed} slot {slot}");
                torn += u64::from(class == Slot::Torn);
            }
            for op in (1..=submitted)
                .filter(|&op| run.acknowledged[op as usize] && !run.overwritten[op as usize])
            {
                let slot = op % SLOTS;
                assert!(op <= head, "seed {seed}: op {op} is beyond the head {head}");
                assert_eq!(recovery.slot(slot), Slot::Clean, "seed {seed} op {op}");
                let header = Header::decode(&ring[slot as usize * HEADER..]).unwrap();
                assert_eq!(
                    header.checksum.get(),
                    run.checksums[op as usize],
                    "seed {seed} op {op}"
                );
            }
            wrapped += u64::from(head > SLOTS);
            wal = run.reopen(&recovery);
        }
    }
    println!(
        "{seeds} seeds, {crashes} crashes, {torn} torn slots, {wrapped} recoveries past a wrap"
    );
    assert!(torn > 0 && wrapped > 0);
}

/// Seed gate: bit flips and no crashes. Each seed appends up to three
/// rings' worth of prepares with a chance of 1 in 20 that a write has a bit
/// flipped, lets every append complete, and recovers. No slot is torn, and
/// every corrupt slot had a bit flipped in it.
///
/// Injected bug: swapping `Slot::Torn` and `Slot::Corrupt` in `classify`
/// fails this test on seed 0, at the assertion that a torn slot is beyond
/// the head.
#[test]
fn a_bit_flip_never_makes_a_slot_torn() {
    let seeds = seeds();
    let (mut flips, mut corrupt, mut uncertain) = (0, 0, 0);
    for seed in 0..seeds {
        let faults = Faults {
            bit_flip: MILLION / 20,
            latency: MILLION / 2,
            latency_max: 1_000_000,
            ..Faults::default()
        };
        let mut run = Run::new(seed, 3, faults);
        let mut wal = Wal::new(TAG, run.pool.acquire().unwrap(), 0, ROOT);
        for _ in 0..1 + run.sim.next_u64() % (3 * SLOTS) {
            run.append(&mut wal);
        }
        while run.step(&mut wal) {}
        assert!(run.acknowledged.iter().all(|&done| done), "seed {seed}");
        drop(wal);

        let mut flipped = [false; SLOTS as usize];
        for fault in &run.faults {
            let FaultKind::BitFlip { bit } = fault.kind else {
                continue;
            };
            let (zone, offset, _) = fault.range.unwrap();
            let byte = offset + bit / 8;
            let slot = match zone {
                Zone::WalPrepares => byte / SLOT_SIZE,
                Zone::WalHeaders => byte / HEADER as u64,
                _ => unreachable!("the log writes only its rings"),
            };
            flipped[slot as usize] = true;
            flips += 1;
        }
        let recovery = run.recover();
        for slot in 0..SLOTS {
            let class = recovery.slot(slot);
            assert_ne!(class, Slot::Torn, "seed {seed} slot {slot}");
            if class == Slot::Corrupt {
                assert!(flipped[slot as usize], "seed {seed} slot {slot}");
                corrupt += 1;
            }
        }
        uncertain += u64::from(recovery.uncertain());
    }
    println!(
        "{seeds} seeds, {flips} bit flips, {corrupt} corrupt slots, {uncertain} uncertain heads"
    );
    assert!(corrupt > 0);
}

/// Gate: recovery reads each ring once (checked in every recovery above),
/// here over 1,024 slots each holding a prepare of the largest size, 1 GiB
/// in all. Run it in release for the timing:
/// `cargo test --release -p granite-sim --test wal_recovery -- --ignored --nocapture`.
#[test]
#[ignore = "a timing, run on purpose in release"]
fn recovery_of_a_full_ring_of_the_largest_prepares() {
    let mut run = Run::new(0, 1, Faults::default());
    let mut ring = vec![0; SLOTS as usize * HEADER];
    let mut parent = ROOT;
    let len = FORMAT.message_size_max as usize - HEADER;
    for op in 1..=SLOTS {
        let (bytes, sum) = prepare(op, parent, len);
        let slot = op % SLOTS;
        ring[slot as usize * HEADER..][..HEADER].copy_from_slice(&bytes[..HEADER]);
        run.write_raw(Zone::WalPrepares, slot * SLOT_SIZE, &bytes);
        parent = sum;
    }
    run.write_raw(Zone::WalHeaders, 0, &ring);
    let start = std::time::Instant::now();
    let recovery = run.recover();
    let elapsed = start.elapsed();
    assert!((0..SLOTS).all(|slot| recovery.slot(slot) == Slot::Clean));
    assert_eq!(recovery.head(), SLOTS);
    println!("recovered {SLOTS} slots of {len} B bodies in {elapsed:?}");
}
