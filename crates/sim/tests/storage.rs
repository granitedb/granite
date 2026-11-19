//! The gates of T0.4.3: each storage fault occurs only when enabled, a crash
//! tears a write in flight by sector, the quorum bound holds at maximum fault
//! rates, the disk passes the conformance suite with faults disabled, and the
//! faults are a function of the seed.

use granite_base::constants::FORMAT;
use granite_io::message_pool::{MessagePool, SECTOR_SIZE};
use granite_io::{Completion, Random, Storage, StorageError, Tag, Time, Zone};
use granite_sim::storage::MILLION;
use granite_sim::{Disk, Fault, FaultKind, Faults, Group, Scheduler};

const SECTOR: u64 = SECTOR_SIZE as u64;
const MILLISECOND: u64 = 1_000_000;
const REPLICAS: usize = 3;
const QUORUM: usize = 2;
const CAPACITY: usize = 4;
const TICKS: u32 = 64;

/// Small zones that still hold several units each: four superblock copies,
/// one sector of WAL headers (16 slots), two WAL prepare slots and four grid
/// blocks.
fn zones() -> [u64; 4] {
    [
        FORMAT.superblock_copies * FORMAT.superblock_copy_size,
        SECTOR,
        2 * FORMAT.message_size_max,
        4 * FORMAT.grid_block_size,
    ]
}

/// Faults 0 to 6, in the order of [`FaultKind`], switched on at their
/// highest rate or off.
fn with(mut faults: Faults, kind: usize, on: bool) -> Faults {
    let chance = |share: u32| if on { share } else { 0 };
    match kind {
        0 => faults.torn_writes = on,
        1 => faults.bit_flip = chance(MILLION / 3),
        2 => faults.misdirected_write = chance(MILLION / 3),
        3 => faults.lost_write = chance(MILLION / 3),
        4 => faults.misdirected_read = chance(MILLION / 2),
        5 => faults.read_error = chance(MILLION / 2),
        6 => faults.latency = chance(MILLION),
        _ => unreachable!(),
    }
    faults
}

fn kind(fault: &Fault) -> usize {
    match fault.kind {
        FaultKind::TornWrite => 0,
        FaultKind::BitFlip { .. } => 1,
        FaultKind::MisdirectedWrite { .. } => 2,
        FaultKind::LostWrite => 3,
        FaultKind::MisdirectedRead { .. } => 4,
        FaultKind::ReadError => 5,
        FaultKind::Latency { .. } => 6,
    }
}

fn none() -> Faults {
    Faults {
        latency_max: 5 * MILLISECOND,
        ..Faults::default()
    }
}

/// Every fault at its highest rate: every write and every read suffers one
/// (bar one in a million writes), every operation is delayed, and crashes
/// tear writes.
fn max() -> Faults {
    (0..7).fold(none(), |faults, kind| with(faults, kind, true))
}

#[derive(Debug, Hash)]
enum Event {
    Tick,
    Completed {
        replica: usize,
        tag: Tag,
        ok: bool,
        first: u8,
    },
    Fault {
        replica: usize,
        fault: Fault,
    },
}

#[derive(Default)]
struct Outcome {
    trace: u64,
    faults: Vec<Fault>,
    /// Sectors read back and compared with what the replica wrote.
    checked: u32,
    /// Sectors read back that differ.
    mismatches: u32,
    errors: u32,
    /// Polls that found operations in flight and none due.
    late: u32,
}

/// An operation in flight: tag, zone, offset, length, write, fill byte.
type Pending = (Tag, Zone, u64, u64, bool, u8);

/// Three replicas of one group, each driven by random writes, reads, fsyncs,
/// polls and crashes, with every fault and completion in the trace.
fn scenario(seed: u64, faults: Faults, pool: &MessagePool) -> Outcome {
    let mut sim = Scheduler::new(seed, 1_000);
    let group = Group::new(REPLICAS, QUORUM, zones());
    let mut disks: Vec<Disk> = (0..REPLICAS)
        .map(|replica| Disk::new(&group, replica, CAPACITY, faults, &mut sim))
        .collect();
    // What each sector holds by the replica's own writes; `None` once a crash
    // left a write in flight over it.
    let empty = zones().map(|size| vec![Some(0u8); (size / SECTOR) as usize]);
    let mut model = vec![empty; REPLICAS];
    let mut in_flight: Vec<Vec<Pending>> = vec![Vec::new(); REPLICAS];
    let mut out = Outcome::default();
    let mut tags = 0;
    sim.schedule(0, Event::Tick).unwrap();
    for _ in 0..TICKS {
        while !matches!(sim.step().expect("a tick is scheduled"), Event::Tick) {}
        let now = sim.now();
        let r = (sim.next_u64() % REPLICAS as u64) as usize;
        let disk = &mut disks[r];
        let action = sim.next_u64() % 16;
        let zone = Zone::ALL[(sim.next_u64() % 4) as usize];
        let size = disk.size(zone);
        let len = ((1 + sim.next_u64() % 2) * SECTOR).min(size);
        // Near the start of a quarter of the zone, so replicas hit the same
        // units.
        let quarter = (sim.next_u64() % 4) * size / 4 / SECTOR;
        let offset = ((quarter + sim.next_u64() % 2) * SECTOR).min(size - len);
        let write = action <= 5;
        let clear = in_flight[r].iter().all(|&(_, z, o, l, w, _)| {
            z != zone || o + l <= offset || offset + len <= o || !(w || write)
        });
        tags += 1;
        let fill = 1 + (tags % 255) as u8;
        match action {
            0 => {
                for (_, zone, offset, len, write, _) in in_flight[r].drain(..) {
                    if write {
                        let sectors =
                            (offset / SECTOR) as usize..((offset + len) / SECTOR) as usize;
                        model[r][zone as usize][sectors].fill(None);
                    }
                }
                disk.crash();
            }
            1..=9 if clear => {
                let mut buffer = pool.acquire().expect("pool holds every operation");
                buffer.bytes_mut().unwrap()[..len as usize].fill(fill);
                let submitted = if write {
                    disk.write(tags, zone, offset, len as u32, buffer).is_ok()
                } else {
                    disk.read(tags, zone, offset, len as u32, buffer).is_ok()
                };
                if submitted {
                    in_flight[r].push((tags, zone, offset, len, write, fill));
                }
            }
            10 => {
                if disk.fsync(tags).is_ok() {
                    in_flight[r].push((tags, Zone::Grid, 0, 0, false, 0));
                }
            }
            11.. => match disk.poll() {
                None => out.late += u32::from(!in_flight[r].is_empty()),
                Some(completion) => {
                    let tag = completion.tag();
                    let index = in_flight[r].iter().position(|p| p.0 == tag).unwrap();
                    let (_, zone, offset, len, _, fill) = in_flight[r].swap_remove(index);
                    let sectors = (offset / SECTOR) as usize..((offset + len) / SECTOR) as usize;
                    let (ok, first) = match completion {
                        Completion::Write { .. } => {
                            model[r][zone as usize][sectors].fill(Some(fill));
                            (true, fill)
                        }
                        Completion::Read { buffer, result, .. } => {
                            out.errors += u32::from(result.is_err());
                            if result.is_ok() {
                                let bytes = &buffer.bytes()[..len as usize];
                                for (sector, expected) in model[r][zone as usize][sectors]
                                    .iter()
                                    .enumerate()
                                    .filter_map(|(i, e)| Some((i, (*e)?)))
                                {
                                    let at = sector * SECTOR_SIZE..(sector + 1) * SECTOR_SIZE;
                                    out.checked += 1;
                                    out.mismatches +=
                                        u32::from(bytes[at].iter().any(|&b| b != expected));
                                }
                            }
                            (result.is_ok(), buffer.bytes()[0])
                        }
                        _ => (true, 0),
                    };
                    let event = Event::Completed {
                        replica: r,
                        tag,
                        ok,
                        first,
                    };
                    sim.schedule(now, event).unwrap();
                }
            },
            _ => {}
        }
        for fault in disks[r].faults() {
            out.faults.push(fault);
            sim.schedule(now, Event::Fault { replica: r, fault })
                .unwrap();
        }
        let next = now + sim.next_u64() % MILLISECOND;
        sim.schedule(next, Event::Tick).unwrap();
    }
    while sim.step().is_some() {}
    out.trace = sim.trace();
    out
}

fn pool() -> MessagePool {
    // One more than can be in flight: a buffer is taken before a refusal.
    MessagePool::new((REPLICAS * CAPACITY + 1) as u32)
}

/// One replica of a group of three, with a pool and a clock.
struct One {
    sim: Scheduler<()>,
    disk: Disk,
    pool: MessagePool,
}

impl One {
    fn new(seed: u64, faults: Faults) -> Self {
        let mut sim = Scheduler::new(seed, 1);
        let group = Group::new(REPLICAS, QUORUM, zones());
        let disk = Disk::new(&group, 0, CAPACITY, faults, &mut sim);
        let pool = MessagePool::new(CAPACITY as u32);
        One { sim, disk, pool }
    }

    /// Delivers the next completion, moving the clock to it if needed.
    fn complete(&mut self) -> Completion {
        loop {
            if let Some(completion) = self.disk.poll() {
                return completion;
            }
            let at = self.disk.ready_at().expect("an operation in flight");
            self.sim.schedule(at, ()).unwrap();
            self.sim.step();
        }
    }

    fn submit_write(&mut self, offset: u64, sectors: u64, byte: u8) {
        let mut buffer = self.pool.acquire().unwrap();
        buffer.bytes_mut().unwrap().fill(byte);
        let len = (sectors * SECTOR) as u32;
        self.disk.write(1, Zone::Grid, offset, len, buffer).unwrap();
    }

    fn write(&mut self, offset: u64, sectors: u64, byte: u8) {
        self.submit_write(offset, sectors, byte);
        let Completion::Write { result: Ok(()), .. } = self.complete() else {
            panic!("a write is acknowledged");
        };
    }

    fn read(&mut self, offset: u64, sectors: u64) -> Result<Vec<u8>, StorageError> {
        let buffer = self.pool.acquire().unwrap();
        let len = (sectors * SECTOR) as usize;
        self.disk
            .read(2, Zone::Grid, offset, len as u32, buffer)
            .unwrap();
        let Completion::Read { buffer, result, .. } = self.complete() else {
            panic!("expected the read");
        };
        result.map(|()| buffer.bytes()[..len].to_vec())
    }

    fn faults(&mut self) -> Vec<FaultKind> {
        self.disk.faults().map(|fault| fault.kind).collect()
    }
}

#[test]
fn disk_passes_the_conformance_suite() {
    let zones = [
        4 * SECTOR,
        4 * SECTOR,
        8 * SECTOR,
        2 * FORMAT.message_size_max,
    ];
    // Several seeds, so the suite holds for several completion orders.
    for seed in 0..32 {
        let mut sim = Scheduler::<()>::new(seed, 1);
        let group = Group::new(1, 1, zones);
        let mut disk = Disk::new(&group, 0, CAPACITY, Faults::default(), &mut sim);
        granite_io::conformance::run_storage(&mut disk, Disk::poll, &MessagePool::new(8));
    }
}

#[test]
fn no_fault_occurs_when_all_are_disabled() {
    let pool = pool();
    for seed in 0..100 {
        let out = scenario(seed, none(), &pool);
        let out = (
            out.faults.len(),
            out.mismatches,
            out.errors,
            out.late,
            out.checked > 0,
        );
        assert_eq!(out, (0, 0, 0, 0, true), "seed {seed}");
    }
}

#[test]
fn each_fault_occurs_only_when_enabled() {
    let pool = pool();
    for fault in 0..7 {
        let only: Vec<_> = (0..20)
            .flat_map(|seed| scenario(seed, with(none(), fault, true), &pool).faults)
            .collect();
        assert!(!only.is_empty(), "fault {fault} occurs when enabled");
        assert!(only.iter().all(|f| kind(f) == fault), "fault {fault} alone");
        let others: Vec<_> = (0..100)
            .flat_map(|seed| scenario(seed, with(max(), fault, false), &pool).faults)
            .collect();
        assert!(!others.is_empty());
        assert!(
            others.iter().all(|f| kind(f) != fault),
            "fault {fault} never occurs when disabled"
        );
    }
}

#[test]
fn a_bit_flip_changes_one_bit_of_a_write() {
    let mut one = One::new(0, with(none(), 1, true));
    // Every write runs the chance; the one in three that flips is found.
    let flipped = (0..64).find_map(|i| {
        one.write(0, 2, 0xaa);
        let read = one.read(0, 2).unwrap();
        let faults = one.faults();
        let changed: u32 = read.iter().map(|b| (b ^ 0xaa).count_ones()).sum();
        match faults[..] {
            [] => {
                assert_eq!(changed, 0, "write {i} without a fault");
                None
            }
            [FaultKind::BitFlip { bit }] => Some((bit, read)),
            _ => panic!("{faults:?}"),
        }
    });
    let (bit, read) = flipped.expect("a bit flips");
    let changed: u32 = read.iter().map(|b| (b ^ 0xaa).count_ones()).sum();
    assert_eq!(changed, 1);
    assert_eq!(read[(bit / 8) as usize] ^ 0xaa, 1 << (bit % 8));
}

#[test]
fn a_misdirected_write_lands_elsewhere() {
    let mut one = One::new(0, with(none(), 2, true));
    for byte in 1.. {
        one.write(0, 1, byte);
        if let [FaultKind::MisdirectedWrite { to }] = one.faults()[..] {
            assert_ne!(to, 0);
            assert!(one.read(0, 1).unwrap().iter().all(|&b| b != byte));
            assert_eq!(one.read(to, 1).unwrap(), vec![byte; SECTOR_SIZE]);
            return;
        }
        assert_eq!(one.read(0, 1).unwrap(), vec![byte; SECTOR_SIZE]);
    }
}

#[test]
fn a_lost_write_is_acknowledged_and_changes_nothing() {
    let mut one = One::new(0, with(none(), 3, true));
    let mut before = vec![0; SECTOR_SIZE];
    for byte in 1.. {
        one.write(0, 1, byte);
        let after = one.read(0, 1).unwrap();
        if one.faults() == [FaultKind::LostWrite] {
            assert_eq!(after, before);
            return;
        }
        assert_eq!(after, vec![byte; SECTOR_SIZE]);
        before = after;
    }
}

#[test]
fn a_misdirected_read_reads_elsewhere() {
    let mut one = One::new(0, with(none(), 4, true));
    one.write(0, 1, 0xaa);
    loop {
        let read = one.read(0, 1).unwrap();
        match one.faults()[..] {
            [FaultKind::MisdirectedRead { from }] => {
                assert_ne!(from, 0);
                assert_eq!(read, vec![0; SECTOR_SIZE]);
                break;
            }
            [] => assert_eq!(read, vec![0xaa; SECTOR_SIZE]),
            _ => panic!("only misdirected reads"),
        }
    }
}

#[test]
fn a_read_error_fails_the_read() {
    let mut one = One::new(0, with(none(), 5, true));
    while one.read(0, 1).is_ok() {
        assert_eq!(one.faults(), []);
    }
    assert_eq!(one.faults(), [FaultKind::ReadError]);
}

#[test]
fn latency_delays_the_completion_in_virtual_time() {
    let mut one = One::new(0, with(none(), 6, true));
    one.submit_write(0, 1, 0xaa);
    assert!(one.disk.poll().is_none(), "not due at submission");
    let [FaultKind::Latency { delay }] = one.faults()[..] else {
        panic!("every operation is delayed");
    };
    assert_eq!(one.disk.ready_at(), Some(delay));
    one.complete();
    assert_eq!(one.sim.clock().monotonic(), delay);
}

/// Writes 0x11 over eight sectors, crashes while 0x22 is in flight over
/// them, and returns how many sectors hold the new content.
fn crash_during_overwrite(seed: u64, torn: bool) -> (usize, Vec<FaultKind>) {
    let faults = Faults {
        torn_writes: torn,
        ..Faults::default()
    };
    let mut one = One::new(seed, faults);
    one.write(0, 8, 0x11);
    one.submit_write(0, 8, 0x22);
    one.disk.crash();
    let read = one.read(0, 8).unwrap();
    let mut new = 0;
    for sector in read.chunks(SECTOR_SIZE) {
        assert!(
            sector.iter().all(|&b| b == 0x11) || sector.iter().all(|&b| b == 0x22),
            "seed {seed}: a sector holds its old or its new content"
        );
        new += usize::from(sector[0] == 0x22);
    }
    (new, one.faults())
}

#[test]
fn a_crash_tears_a_write_in_flight_by_sector() {
    let mut mixed = 0;
    for seed in 0..1_000 {
        let (new, faults) = crash_during_overwrite(seed, true);
        assert_eq!(faults, [FaultKind::TornWrite]);
        mixed += u32::from(0 < new && new < 8);
    }
    assert!(mixed > 0, "some crash leaves a mix of old and new sectors");
}

#[test]
fn without_torn_writes_a_crash_keeps_a_write_whole() {
    let (mut old, mut new) = (0, 0);
    for seed in 0..1_000 {
        let (sectors, faults) = crash_during_overwrite(seed, false);
        assert_eq!(faults, []);
        match sectors {
            0 => old += 1,
            8 => new += 1,
            _ => panic!("seed {seed}: a torn write without torn writes"),
        }
    }
    assert!(old > 0 && new > 0, "a write in flight lands or not");
}

/// The assertion in `Group::mark` checks the bound on every injection.
///
/// Injected bug: with `Group::allows` returning `true` at its top, this test
/// fails on seed 0 with "(Grid, 3) is corrupt on a quorum of replicas"
/// (recorded in the change that adds it).
#[test]
fn the_bound_holds_at_maximum_fault_rates() {
    let pool = pool();
    let mut seen = [0u32; 7];
    for seed in 0..10_000 {
        for fault in scenario(seed, max(), &pool).faults {
            seen[kind(&fault)] += 1;
        }
    }
    assert!(
        seen.iter().all(|&count| count > 0),
        "every fault ran: {seen:?}"
    );
}

#[test]
fn faults_are_a_function_of_the_seed() {
    let pool = pool();
    let mut last = None;
    for seed in 0..1_000 {
        let a = scenario(seed, max(), &pool);
        let b = scenario(seed, max(), &pool);
        assert_eq!((a.trace, &a.faults), (b.trace, &b.faults), "seed {seed}");
        assert!(!a.faults.is_empty());
        assert_ne!(Some(a.trace), last, "seed {seed} changes the trace");
        last = Some(a.trace);
    }
}

#[test]
fn a_full_fault_log_stops_injection_until_drained() {
    let mut lossy = One::new(
        0,
        Faults {
            lost_write: MILLION,
            ..Faults::default()
        },
    );
    for _ in 0..CAPACITY {
        lossy.write(0, 1, 0xaa);
    }
    // The log is full: this write lands.
    lossy.write(0, 1, 0xbb);
    assert_eq!(lossy.read(0, 1).unwrap(), vec![0xbb; SECTOR_SIZE]);
    assert_eq!(lossy.faults(), [FaultKind::LostWrite; CAPACITY]);
    lossy.write(0, 1, 0xcc);
    assert_eq!(lossy.faults(), [FaultKind::LostWrite]);
}

#[test]
#[should_panic(expected = "no write overlaps")]
fn overlapping_write_in_flight_panics() {
    let mut one = One::new(0, Faults::default());
    one.submit_write(0, 2, 1);
    one.submit_write(SECTOR, 1, 2);
}
