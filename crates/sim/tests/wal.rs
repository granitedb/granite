//! The gates of T1.1.3, the WAL write path, on the simulated disk: the hash
//! chain is enforced, each prepare is durable before its header is written,
//! an append completes only once both are durable, a wrapped slot never
//! returns the newer prepare, a damaged slot returns a typed error, the
//! prepare is written from the caller's buffer, and a steady append
//! allocates nothing.

// The io tests' counting allocator, shared rather than copied: it is the one
// module outside `io` proper that tools/lint.py lets use `unsafe` for this.
#[path = "../../io/tests/common/counting_alloc.rs"]
mod counting_alloc;

use granite_base::checksum::checksum;
use granite_base::constants::FORMAT;
use granite_base::header::{self, Command, Header, VERSION};
use granite_io::message_pool::{Message, MessagePool, SECTOR_SIZE};
use granite_io::{Completion, Full, Random, Storage, StorageError, Tag, Zone};
use granite_sim::storage::MILLION;
use granite_sim::{Disk, Faults, Group, Scheduler};
use granite_storage::layout::zone_sizes;
use granite_storage::wal::{Event, ReadError, Refused, TAGS, Wal};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::mem::offset_of;

const HEADER: usize = FORMAT.header_size as usize;
const SLOTS: u64 = FORMAT.wal_slot_count;
const SLOT_SIZE: u64 = FORMAT.message_size_max;
/// The log's first tag: anything but zero, so a test that ignored the base
/// would fail.
const TAG: Tag = 1 << 40;
/// Tags of the test's own reads of the raw disk, outside the log's.
const RAW: Tag = 7;
/// The checksum the first prepare names as its parent.
const ROOT: u128 = 0x5eed;
/// Pool buffers: a full pipeline, the header buffer, and reads.
const POOL: u32 = 16;

/// The zones of a data file, with the grid cut to one sector: the log never
/// touches it.
fn zones() -> [u64; 4] {
    let mut zones = zone_sizes(FORMAT.grid_growth_step);
    zones[Zone::Grid as usize] = FORMAT.sector_size;
    zones
}

/// A body of `len` bytes that differs from op to op.
fn body_len(op: u64) -> usize {
    (op * 7919 % (3 * SECTOR_SIZE as u64)) as usize
}

/// A buffer holding prepare `op` with parent `parent` and a `len`-byte body,
/// zero to the end of its last sector, and its header checksum.
fn prepare(pool: &MessagePool, op: u64, parent: u128, len: usize) -> (Message, u128) {
    let mut message = pool.acquire().expect("a free buffer");
    let bytes = message.bytes_mut().expect("only handle");
    let size = HEADER + len;
    bytes[..size.next_multiple_of(SECTOR_SIZE)].fill(0);
    for (i, b) in bytes[HEADER..size].iter_mut().enumerate() {
        *b = (op as u8) ^ (i as u8) ^ 0xa5;
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
    assert!(Header::decode(bytes).is_ok_and(|h| h.op.get() == op));
    (message, sum)
}

/// A backend the rig drives: a disk, or a disk under the trace checker.
trait Backend: Storage {
    fn poll(&mut self) -> Option<Completion>;
    fn ready_at(&self) -> Option<u64>;
    /// Called as the log reports op `op` appended, before the next poll.
    fn appended(&self, _op: u64) {}
}

impl Backend for Disk {
    fn poll(&mut self) -> Option<Completion> {
        Disk::poll(self)
    }
    fn ready_at(&self) -> Option<u64> {
        Disk::ready_at(self)
    }
}

/// The log on one disk, appending prepares of its own making.
struct Rig<B> {
    sim: Scheduler<()>,
    io: B,
    pool: MessagePool,
    wal: Wal,
    /// The next op to append, and its parent.
    next: u64,
    parent: u128,
    /// The header checksum of each op appended, by op.
    checksums: Vec<u128>,
    /// Whether each op's append completed, by op.
    appended: Vec<bool>,
    /// Read completions not yet taken.
    reads: Vec<(u64, Result<Message, ReadError>)>,
}

fn sim(seed: u64) -> Scheduler<()> {
    Scheduler::new(seed, 1)
}

fn disk(sim: &mut Scheduler<()>, capacity: usize, faults: Faults) -> Disk {
    Disk::new(&Group::new(1, 1, zones()), 0, capacity, faults, sim)
}

impl<B: Backend> Rig<B> {
    fn new(sim: Scheduler<()>, io: B) -> Self {
        let pool = MessagePool::new(POOL);
        let wal = Wal::new(TAG, pool.acquire().unwrap(), 0, ROOT);
        Rig {
            sim,
            io,
            pool,
            wal,
            next: 1,
            parent: ROOT,
            checksums: vec![ROOT],
            appended: vec![true],
            reads: Vec::new(),
        }
    }

    /// Delivers one completion to the log, moving virtual time to the next
    /// due operation if none is due. Returns `false` when nothing is in
    /// flight.
    fn step(&mut self) -> bool {
        let completion = match self.io.poll() {
            Some(completion) => completion,
            None => {
                let Some(at) = self.io.ready_at() else {
                    return false;
                };
                self.sim.schedule(at, ()).unwrap();
                self.sim.step();
                self.io.poll().expect("an operation is due")
            }
        };
        assert!(self.wal.owns(completion.tag()));
        // One header write completes at most a sector of appends.
        let mut done = [0; SECTOR_SIZE / HEADER];
        let mut count = 0;
        let (appended, reads) = (&mut self.appended, &mut self.reads);
        self.wal
            .complete(&mut self.io, completion, |event| match event {
                Event::Appended { op } => {
                    assert!(!appended[op as usize], "op {op} completes once");
                    appended[op as usize] = true;
                    done[count] = op;
                    count += 1;
                }
                Event::Read { op, result } => reads.push((op, result)),
            });
        for &op in &done[..count] {
            self.io.appended(op);
        }
        true
    }

    /// Submits the append of the next op, delivering completions until the
    /// log takes it, and returns the address of its bytes.
    fn append(&mut self) -> *const u8 {
        let op = self.next;
        let (mut message, sum) = prepare(&self.pool, op, self.parent, body_len(op));
        let address = message.bytes().as_ptr();
        loop {
            match self.wal.append(&mut self.io, message) {
                Ok(()) => break,
                Err(Full(back)) => message = back,
            }
            assert!(self.step(), "a refused append has something to wait for");
        }
        self.checksums.push(sum);
        self.appended.push(false);
        self.parent = sum;
        self.next += 1;
        address
    }

    fn settle(&mut self) {
        while self.step() {}
        assert!(
            self.appended.iter().all(|&done| done),
            "every append completed"
        );
    }

    /// Reads `op` through the log and waits for the result.
    fn read(&mut self, op: u64) -> Result<Result<Message, ReadError>, ()> {
        let buffer = self.pool.acquire().expect("a free buffer");
        match self.wal.read(&mut self.io, op, buffer) {
            Ok(()) => {}
            Err(Refused::NotPresent) => return Err(()),
            Err(Refused::Full(_)) => panic!("the disk has room"),
        }
        while self.reads.is_empty() {
            assert!(self.step());
        }
        let (read_op, result) = self.reads.pop().unwrap();
        assert_eq!(read_op, op);
        Ok(result)
    }

    /// Checks that `buffer` holds op `op` as it was appended.
    fn assert_holds(&self, op: u64, buffer: &Message) {
        let (expected, _) = prepare(
            &self.pool,
            op,
            self.checksums[op as usize - 1],
            body_len(op),
        );
        let size = HEADER + body_len(op);
        assert_eq!(
            &buffer.bytes()[..size],
            &expected.bytes()[..size],
            "op {op}"
        );
    }
}

/// Reads `len` raw bytes of `zone` at `offset` straight from the disk.
fn raw_read(
    disk: &mut Disk,
    sim: &mut Scheduler<()>,
    buffer: Message,
    zone: Zone,
    offset: u64,
    len: u32,
) -> Message {
    disk.read(RAW, zone, offset, len, buffer).unwrap();
    loop {
        if let Some(Completion::Read { buffer, result, .. }) = disk.poll() {
            result.unwrap();
            return buffer;
        }
        sim.schedule(disk.ready_at().unwrap(), ()).unwrap();
        sim.step();
    }
}

fn raw_write(disk: &mut Disk, buffer: Message, zone: Zone, offset: u64, len: u32) {
    disk.write(RAW, zone, offset, len, buffer).unwrap();
    assert!(matches!(
        disk.poll(),
        Some(Completion::Write { result: Ok(()), .. })
    ));
}

#[test]
#[should_panic(expected = "the prepare's parent is the previous header's checksum")]
fn a_prepare_that_does_not_extend_the_chain_is_rejected() {
    let mut sim = sim(1);
    let disk = disk(&mut sim, 16, Faults::default());
    let mut rig = Rig::new(sim, disk);
    rig.append();
    rig.parent ^= 1;
    rig.append();
}

#[test]
fn the_log_owns_exactly_its_tags() {
    let rig = Rig::new(sim(1), disk(&mut sim(1), 16, Faults::default()));
    assert!(!rig.wal.owns(TAG - 1));
    assert!(rig.wal.owns(TAG));
    assert!(rig.wal.owns(TAG + TAGS - 1));
    assert!(!rig.wal.owns(TAG + TAGS));
}

/// What one operation in flight is, for the trace checker.
enum Pending {
    Prepare(u64),
    Headers(Vec<u64>),
    /// An fsync, with the number of writes completed when it was submitted.
    Fsync(u64),
    Read,
}

/// A disk that checks the log's I/O as it happens: every op's prepare write
/// has completed, and an fsync submitted after it has completed, before a
/// header write naming the op is submitted. It records when each prepare and
/// header write completed and how far the fsyncs reach, for the rig to check
/// each append completion against.
struct Traced {
    disk: Disk,
    pending: BTreeMap<Tag, Pending>,
    /// Write completions so far; the number of each one.
    writes: u64,
    /// Every write numbered up to this one is durable.
    durable: u64,
    /// By op, the number of its prepare write's and its header write's
    /// completion, or zero.
    prepared: Vec<u64>,
    headed: Vec<u64>,
    /// By op, the address its prepare was written from.
    addresses: Vec<usize>,
    /// Header write submissions checked, one per header named.
    checked: u64,
    /// Append completions checked.
    acknowledged: Cell<u64>,
}

impl Traced {
    fn durable(&self, number: u64) -> bool {
        number != 0 && number <= self.durable
    }

    fn grow(&mut self, op: u64) {
        let len = op as usize + 1;
        if self.prepared.len() < len {
            self.prepared.resize(len, 0);
            self.headed.resize(len, 0);
            self.addresses.resize(len, 0);
        }
    }
}

impl Storage for Traced {
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
        self.disk.read(tag, zone, offset, len, buffer)?;
        self.pending.insert(tag, Pending::Read);
        Ok(())
    }

    fn write(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        let address = buffer.bytes().as_ptr() as usize;
        let what = match zone {
            Zone::WalPrepares => {
                let op = Header::decode(buffer.bytes()).unwrap().op.get();
                assert_eq!(offset, op % SLOTS * SLOT_SIZE);
                Pending::Prepare(op)
            }
            Zone::WalHeaders => {
                let ops: Vec<u64> = buffer.bytes()[..SECTOR_SIZE]
                    .chunks(HEADER)
                    .filter_map(|h| Header::decode(h).ok().map(|h| h.op.get()))
                    .collect();
                for &op in &ops {
                    self.grow(op);
                    assert!(
                        self.durable(self.prepared[op as usize]),
                        "op {op}'s prepare is durable before its header write is submitted"
                    );
                    self.checked += 1;
                }
                Pending::Headers(ops)
            }
            _ => panic!("the log writes only its rings"),
        };
        self.disk.write(tag, zone, offset, len, buffer)?;
        if let Pending::Prepare(op) = what {
            self.grow(op);
            self.addresses[op as usize] = address;
        }
        assert!(
            self.pending.insert(tag, what).is_none(),
            "one operation per tag"
        );
        Ok(())
    }

    fn fsync(&mut self, tag: Tag) -> Result<(), Full<()>> {
        self.disk.fsync(tag)?;
        assert!(
            self.pending
                .insert(tag, Pending::Fsync(self.writes))
                .is_none()
        );
        Ok(())
    }

    fn grow_grid(&mut self, _: Tag, _: u64) -> Result<(), Full<()>> {
        unreachable!("the WAL never grows the grid")
    }
}

impl Backend for Traced {
    fn poll(&mut self) -> Option<Completion> {
        let completion = self.disk.poll()?;
        match self.pending.remove(&completion.tag()).expect("submitted") {
            Pending::Prepare(op) => {
                self.writes += 1;
                self.prepared[op as usize] = self.writes;
            }
            Pending::Headers(ops) => {
                self.writes += 1;
                for op in ops {
                    self.headed[op as usize] = self.writes;
                }
            }
            Pending::Fsync(reach) => self.durable = self.durable.max(reach),
            Pending::Read => {}
        }
        Some(completion)
    }

    fn ready_at(&self) -> Option<u64> {
        self.disk.ready_at()
    }

    fn appended(&self, op: u64) {
        assert!(
            self.durable(self.prepared[op as usize]),
            "op {op}'s prepare is durable at completion"
        );
        assert!(
            self.durable(self.headed[op as usize]),
            "op {op}'s header is durable at completion"
        );
        self.acknowledged.set(self.acknowledged.get() + 1);
    }
}

/// The trace gate, the acknowledgement-point gate and the no-copy gate,
/// over 10,000 appends with random latency, so completions come out of
/// order, on a disk with little room, so the log meets backpressure.
#[test]
fn ten_thousand_appends_are_ordered_durable_and_uncopied() {
    const APPENDS: u64 = 10_000;
    let mut sim = sim(42);
    let faults = Faults {
        latency: MILLION / 2,
        latency_max: 1_000_000,
        ..Faults::default()
    };
    let traced = Traced {
        disk: disk(&mut sim, 6, faults),
        pending: BTreeMap::new(),
        writes: 0,
        durable: 0,
        prepared: Vec::new(),
        headed: Vec::new(),
        addresses: Vec::new(),
        checked: 0,
        acknowledged: Cell::new(0),
    };
    let mut rig = Rig::new(sim, traced);
    while rig.next <= APPENDS {
        let op = rig.next;
        let address = rig.append();
        assert_eq!(
            rig.io.addresses[op as usize], address as usize,
            "op {op} written from the caller's buffer"
        );
    }
    rig.settle();
    assert_eq!(
        rig.io.acknowledged.get(),
        APPENDS,
        "every completion was checked"
    );
    assert!(rig.io.checked >= APPENDS, "every header named was checked");
    println!(
        "{} headers in header writes checked over {APPENDS} appends",
        rig.io.checked
    );
}

/// After a crash at a random point, every append that completed is on disk:
/// its header in the header ring and its prepare whole in its slot.
#[test]
fn a_completed_append_survives_a_crash() {
    for seed in 0..200 {
        let mut sim = sim(seed);
        let faults = Faults {
            latency: MILLION / 2,
            latency_max: 1_000_000,
            torn_writes: true,
            ..Faults::default()
        };
        let disk = disk(&mut sim, 6, faults);
        let mut rig = Rig::new(sim, disk);
        let stop = 1 + rig.sim.next_u64() % (SLOTS - FORMAT.pipeline_depth);
        while rig.next <= stop {
            rig.append();
        }
        for _ in 0..rig.sim.next_u64() % 16 {
            rig.step();
        }
        rig.io.crash();
        let Rig {
            mut sim,
            io: mut disk,
            pool,
            checksums,
            appended,
            ..
        } = rig;
        let headers = raw_read(
            &mut disk,
            &mut sim,
            pool.acquire().unwrap(),
            Zone::WalHeaders,
            0,
            u32::try_from(SLOTS * FORMAT.header_size).unwrap(),
        );
        for op in (1..appended.len() as u64).filter(|&op| appended[op as usize]) {
            let entry = &headers.bytes()[op as usize * HEADER..][..HEADER];
            let header =
                Header::decode(entry).unwrap_or_else(|e| panic!("seed {seed} op {op}: {e:?}"));
            assert_eq!(
                header.checksum.get(),
                checksums[op as usize],
                "seed {seed} op {op}"
            );
            let len = (HEADER + body_len(op)).next_multiple_of(SECTOR_SIZE) as u32;
            let slot = raw_read(
                &mut disk,
                &mut sim,
                pool.acquire().unwrap(),
                Zone::WalPrepares,
                op * SLOT_SIZE,
                len,
            );
            let header = Header::decode(slot.bytes()).unwrap();
            assert_eq!(
                header.checksum.get(),
                checksums[op as usize],
                "seed {seed} op {op}"
            );
            assert_eq!(
                header.check_body(&slot.bytes()[HEADER..HEADER + body_len(op)]),
                Ok(())
            );
        }
    }
}

/// Across three wraps of the ring, an op that a later one overwrote, or is
/// overwriting, is not present; every op still in the ring reads back as it
/// was appended.
#[test]
fn an_overwritten_op_is_not_present_across_three_wraps() {
    let mut sim = sim(7);
    let faults = Faults {
        latency: MILLION / 4,
        latency_max: 100_000,
        ..Faults::default()
    };
    let disk = disk(&mut sim, 8, faults);
    let mut rig = Rig::new(sim, disk);
    let mut not_present = 0;
    while rig.next <= 4 * SLOTS {
        rig.append();
        let head = rig.next - 1;
        // The op this append overwrites is gone at once, while the write is
        // in flight.
        if head > SLOTS {
            assert!(
                rig.read(head - SLOTS).is_err(),
                "op {} was overwritten",
                head - SLOTS
            );
            not_present += 1;
        }
        if head.is_multiple_of(SLOTS / 2) {
            rig.settle();
            for op in head.saturating_sub(SLOTS + 16).max(1)..=head {
                match rig.read(op) {
                    Err(()) => {
                        assert!(op + SLOTS <= head, "op {op} is in the ring");
                        not_present += 1;
                    }
                    Ok(result) => {
                        assert!(op + SLOTS > head, "op {op} was overwritten");
                        rig.assert_holds(op, &result.expect("a clean read"));
                    }
                }
            }
        }
    }
    assert!(rig.read(4 * SLOTS + 1).is_err(), "an op not yet appended");
    assert!(not_present > 3 * SLOTS);
}

/// A damaged header, a damaged body, another prepare in the slot and a
/// failed read each give their typed error and no bytes.
#[test]
fn a_damaged_slot_reads_as_a_typed_error() {
    let mut sim = sim(3);
    let disk = disk(&mut sim, 16, Faults::default());
    let mut rig = Rig::new(sim, disk);
    for _ in 0..4 {
        rig.append();
    }
    rig.settle();
    for op in 1..=4 {
        let read = rig.read(op).unwrap().unwrap();
        rig.assert_holds(op, &read);
    }
    let len = |op: u64| (HEADER + body_len(op)).next_multiple_of(SECTOR_SIZE) as u32;
    let damage = |rig: &mut Rig<Disk>, op: u64, from: u64, byte: usize| {
        let mut buffer = raw_read(
            &mut rig.io,
            &mut rig.sim,
            rig.pool.acquire().unwrap(),
            Zone::WalPrepares,
            from * SLOT_SIZE,
            len(op),
        );
        if byte < SECTOR_SIZE {
            buffer.bytes_mut().unwrap()[byte] ^= 1;
        }
        raw_write(
            &mut rig.io,
            buffer,
            Zone::WalPrepares,
            op * SLOT_SIZE,
            len(op),
        );
    };
    damage(&mut rig, 1, 1, HEADER + 3);
    damage(&mut rig, 2, 2, offset_of!(Header, op));
    damage(&mut rig, 3, 4, SECTOR_SIZE); // op 4's prepare lands in op 3's slot
    assert_eq!(
        rig.read(1).unwrap().unwrap_err(),
        ReadError::Body(header::Error::BodyChecksum)
    );
    assert_eq!(
        rig.read(2).unwrap().unwrap_err(),
        ReadError::Header(header::Error::HeaderChecksum)
    );
    assert_eq!(rig.read(3).unwrap().unwrap_err(), ReadError::Mismatch);
    let read = rig.read(4).unwrap().unwrap();
    rig.assert_holds(4, &read);
    drop(read);
    assert_eq!(
        rig.pool.counters().in_use,
        1,
        "no failed read keeps its buffer"
    );

    let mut sim = self::sim(3);
    let failing = Faults {
        read_error: MILLION,
        ..Faults::default()
    };
    let disk = self::disk(&mut sim, 16, failing);
    let mut rig = Rig::new(sim, disk);
    rig.append();
    rig.settle();
    assert_eq!(
        rig.read(1).unwrap().unwrap_err(),
        ReadError::Io(StorageError)
    );
}

/// The pipeline is bounded: past `pipeline_depth` appends in flight, or
/// while a read of the slot is in flight, an append is refused and handed
/// back; it goes through once the log has room.
#[test]
fn a_full_pipeline_refuses_an_append() {
    let mut sim = sim(5);
    let disk = disk(&mut sim, 64, Faults::default());
    let mut rig = Rig::new(sim, disk);
    for _ in 0..FORMAT.pipeline_depth {
        rig.append();
    }
    let (message, _) = prepare(&rig.pool, rig.next, rig.parent, 0);
    let Err(Full(message)) = rig.wal.append(&mut rig.io, message) else {
        panic!("a full pipeline takes no append");
    };
    drop(message);
    rig.settle();

    while rig.next <= SLOTS {
        rig.append();
    }
    rig.settle();
    // Op SLOTS + 1 goes to op 1's slot, which a read is using.
    let buffer = rig.pool.acquire().unwrap();
    rig.wal.read(&mut rig.io, 1, buffer).unwrap();
    let (message, _) = prepare(&rig.pool, rig.next, rig.parent, 0);
    let Err(Full(message)) = rig.wal.append(&mut rig.io, message) else {
        panic!("a slot being read takes no append");
    };
    drop(message);
    rig.settle();
    assert!(rig.reads.pop().unwrap().1.is_ok());
    rig.append();
    rig.settle();
}

/// Once the log has run through its ring, appending and completing a
/// prepare allocates nothing.
#[test]
fn a_steady_append_allocates_nothing() {
    let mut sim = sim(9);
    let disk = disk(&mut sim, 32, Faults::default());
    let mut rig = Rig::new(sim, disk);
    let target = SLOTS + 2_000;
    // Room for every op, so the bookkeeping below does not grow.
    rig.checksums.reserve(target as usize + 1);
    rig.appended.reserve(target as usize + 1);
    rig.reads.reserve(1);
    while rig.next <= SLOTS {
        rig.append();
    }
    rig.settle();
    let before = counting_alloc::allocations();
    while rig.next <= target {
        rig.append();
    }
    rig.settle();
    let allocations = counting_alloc::allocations() - before;
    assert_eq!(allocations, 0, "allocations over 2,000 appends");
}
