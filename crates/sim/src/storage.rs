//! Simulated storage: an in-memory data file per simulated shard that
//! injects the faults of the fault model (document 01 section 1) and no more
//! than it allows (document 06 section 1).
//!
//! A [`Disk`] carries out an operation when its completion is delivered by
//! [`Disk::poll`], not when it is submitted, so a crash between the two
//! leaves the write in flight. Every fault decision is drawn from a
//! generator seeded from the run's [`Scheduler`], so the faults are a
//! function of the seed.
//!
//! Faults:
//! - **Torn write**, on [`Disk::crash`]: each sector of a write in flight
//!   holds either its old or its new content. Without this fault a write in
//!   flight lands whole or not at all.
//! - **Bit flip**: a write lands, then one bit of it flips on the device.
//! - **Misdirected write**: the data lands at another place in the zone and
//!   the intended place keeps its old content.
//! - **Lost write**: the write is acknowledged but nothing changes.
//! - **Misdirected read**: the data comes from another place in the zone.
//! - **Read error**: the read fails with [`StorageError`].
//! - **Latency**: the completion is delivered only after a delay of virtual
//!   time.
//!
//! The bound: the replicas of a group share a [`Group`], which knows which
//! sectors of each replica hold content their process did not write (a bit
//! flip, a lost or misdirected write). A fault that would make a unit of
//! redundancy corrupt on a replication quorum of the group is not injected,
//! and an assertion checks this after every injection. A unit is a grid
//! block, a WAL slot (its header and its prepare) or the superblock (all its
//! copies), at the same offsets on every replica. A torn write is not counted: it was
//! never acknowledged, and recovery truncates it. Read faults are not
//! counted either: they change no stored content.
//!
//! Every injected fault is recorded in a log that the driver drains with
//! [`Disk::faults`], so a checker can tell an injected fault from a bug. The
//! log holds as many faults as the disk holds operations, which is the most
//! one call injects; a full log stops injection until it is drained.

use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::mem;
use std::ops::Range;
use std::rc::Rc;

use granite_base::constants::FORMAT;
use granite_io::memory::SplitMix64;
use granite_io::message_pool::{MESSAGE_SIZE, Message, SECTOR_SIZE};
use granite_io::{Completion, Full, Random, Storage, StorageError, Tag, Time, Zone};

use crate::{Clock, Scheduler};

/// Chances are counted in millionths.
pub const MILLION: u32 = 1_000_000;

const SECTOR: u64 = SECTOR_SIZE as u64;

/// Which faults a disk injects. A chance is in millionths; the write chances
/// together, and the read chances together, are at most [`MILLION`], since
/// an operation suffers at most one of them. The default injects none.
#[derive(Clone, Copy, Debug, Default)]
pub struct Faults {
    /// A crash tears the writes in flight sector by sector.
    pub torn_writes: bool,
    /// Chance that a write has one bit flipped.
    pub bit_flip: u32,
    /// Chance that a write lands at the wrong place.
    pub misdirected_write: u32,
    /// Chance that a write is acknowledged and lost.
    pub lost_write: u32,
    /// Chance that a read reads the wrong place.
    pub misdirected_read: u32,
    /// Chance that a read fails.
    pub read_error: u32,
    /// Chance that an operation is delayed.
    pub latency: u32,
    /// Longest delay, in nanoseconds of virtual time.
    pub latency_max: u64,
}

/// One injected fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    /// Virtual time of the injection.
    pub at: u64,
    pub kind: FaultKind,
    /// The operation's zone, offset and length; `None` for an fsync.
    pub range: Option<(Zone, u64, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaultKind {
    /// A write in flight at a crash: each sector holds its old or new content.
    TornWrite,
    /// Bit `bit` of the written range, counted from its start, flipped.
    BitFlip {
        bit: u64,
    },
    /// The data landed at offset `to` of the same zone.
    MisdirectedWrite {
        to: u64,
    },
    /// The write was acknowledged and nothing changed.
    LostWrite,
    /// The data came from offset `from` of the same zone.
    MisdirectedRead {
        from: u64,
    },
    ReadError,
    /// The completion was delivered `delay` nanoseconds late.
    Latency {
        delay: u64,
    },
}

/// By hand, since [`Zone`] has no `Hash`.
impl Hash for Fault {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.at.hash(state);
        self.kind.hash(state);
        self.range
            .map(|(zone, offset, len)| (zone as u8, offset, len))
            .hash(state);
    }
}

/// The replicas of one group, as far as the bound needs them.
#[derive(Clone)]
pub struct Group(Rc<RefCell<GroupState>>);

struct GroupState {
    quorum: usize,
    /// The sizes each disk starts with; the grid grows per disk.
    zone_sizes: [u64; 4],
    /// Per replica, per zone, per sector: holds content its process did not
    /// write.
    corrupt: Vec<[Vec<bool>; 4]>,
}

impl Group {
    /// `replicas` data files of `zone_sizes` bytes (in [`Zone::ALL`] order),
    /// of which `quorum` form a replication quorum.
    pub fn new(replicas: usize, quorum: usize, zone_sizes: [u64; 4]) -> Self {
        assert!(0 < quorum && quorum <= replicas);
        for size in zone_sizes {
            assert!(size.is_multiple_of(SECTOR));
        }
        let sectors = zone_sizes.map(|size| vec![false; usize::try_from(size / SECTOR).unwrap()]);
        Group(Rc::new(RefCell::new(GroupState {
            quorum,
            zone_sizes,
            corrupt: vec![sectors; replicas],
        })))
    }

    /// Whether `replica` may corrupt `sectors` of `zone`: no unit they lie in
    /// would then be corrupt on a quorum.
    fn allows(&self, replica: usize, zone: Zone, sectors: Range<u64>) -> bool {
        let state = self.0.borrow();
        units(zone, sectors).all(|unit| {
            let others = (0..state.corrupt.len())
                .filter(|&r| r != replica && state.is_corrupt(r, unit))
                .count();
            others + 1 < state.quorum
        })
    }

    /// Marks `sectors` of `zone` on `replica` as corrupt or as holding what
    /// its process wrote, and checks the bound.
    fn mark(&self, replica: usize, zone: Zone, sectors: Range<u64>, corrupt: bool) {
        let mut state = self.0.borrow_mut();
        let range = usize::try_from(sectors.start).unwrap()..usize::try_from(sectors.end).unwrap();
        state.corrupt[replica][zone as usize][range].fill(corrupt);
        for unit in units(zone, sectors) {
            let count = (0..state.corrupt.len())
                .filter(|&r| state.is_corrupt(r, unit))
                .count();
            assert!(
                count < state.quorum,
                "{unit:?} is corrupt on a quorum of replicas"
            );
        }
    }
}

/// The size of the unit of redundancy in `zone`.
fn unit_size(zone: Zone) -> u64 {
    match zone {
        // One unit: a replica needs a quorum of its own copies to open.
        Zone::Superblock => u64::MAX,
        Zone::WalHeaders => FORMAT.header_size,
        Zone::WalPrepares => FORMAT.message_size_max,
        Zone::Grid => FORMAT.grid_block_size,
    }
}

/// The units `sectors` of `zone` lie in, as (zone, index); a WAL slot is
/// named by [`Zone::WalPrepares`], whichever ring the sectors are in.
fn units(zone: Zone, sectors: Range<u64>) -> impl Iterator<Item = (Zone, u64)> {
    let size = unit_size(zone);
    let name = if zone == Zone::WalHeaders {
        Zone::WalPrepares
    } else {
        zone
    };
    (sectors.start * SECTOR / size..(sectors.end * SECTOR).div_ceil(size)).map(move |i| (name, i))
}

impl GroupState {
    fn is_corrupt(&self, replica: usize, (name, index): (Zone, u64)) -> bool {
        let zones: &[Zone] = if name == Zone::WalPrepares {
            &[Zone::WalHeaders, Zone::WalPrepares]
        } else {
            &[name]
        };
        zones.iter().any(|&zone| {
            let size = unit_size(zone);
            let flags = &self.corrupt[replica][zone as usize];
            let sectors = flags.len() as u64;
            let start = (index * size / SECTOR).min(sectors);
            let end = ((index + 1) * size).div_ceil(SECTOR).min(sectors);
            flags[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()].contains(&true)
        })
    }
}

/// A read or a write in flight.
struct Access {
    write: bool,
    zone: Zone,
    offset: u64,
    len: u32,
    buffer: Message,
}

struct Op {
    tag: Tag,
    /// Virtual time from which it may complete.
    ready: u64,
    /// `None` for an fsync or a growth.
    access: Option<Access>,
    /// The grid's new size, for a growth.
    grow: Option<u64>,
}

/// Bytes of a zone allocated together, on the first write to them.
const CHUNK: usize = 16 * SECTOR_SIZE;

/// The bytes of a zone, allocated a chunk at a time on the first write to
/// it; an untouched chunk reads as zeros. A zone as large as the WAL's is
/// then cheap to build, which a seed does several times.
struct Sparse {
    len: u64,
    chunks: Vec<Option<Box<[u8]>>>,
}

impl Sparse {
    fn new(len: u64) -> Self {
        let chunks = usize::try_from(len.div_ceil(CHUNK as u64)).unwrap();
        Sparse {
            len,
            chunks: (0..chunks).map(|_| None).collect(),
        }
    }

    fn read(&self, offset: u64, out: &mut [u8]) {
        self.check(offset, out.len());
        spans(offset, out.len(), |c, chunk, span| match &self.chunks[c] {
            Some(bytes) => out[span].copy_from_slice(&bytes[chunk]),
            None => out[span].fill(0),
        });
    }

    fn write(&mut self, offset: u64, bytes: &[u8]) {
        self.check(offset, bytes.len());
        spans(offset, bytes.len(), |c, chunk, span| {
            let stored = self.chunks[c].get_or_insert_with(|| vec![0; CHUNK].into_boxed_slice());
            stored[chunk].copy_from_slice(&bytes[span]);
        });
    }

    fn flip(&mut self, byte: u64, mask: u8) {
        let mut value = [0];
        self.read(byte, &mut value);
        self.write(byte, &[value[0] ^ mask]);
    }

    fn check(&self, offset: u64, len: usize) {
        assert!(offset + len as u64 <= self.len, "inside the zone");
    }

    /// Extends the zone to `len` bytes; the new bytes read as zeros until written.
    fn grow(&mut self, len: u64) {
        assert!(len >= self.len, "a zone only grows");
        self.len = len;
        let chunks = usize::try_from(len.div_ceil(CHUNK as u64)).unwrap();
        self.chunks.resize_with(chunks, || None);
    }
}

/// Calls `f` with each chunk that `len` bytes at `offset` touch, the range
/// of that chunk, and the matching range of the `len` bytes.
fn spans(offset: u64, len: usize, mut f: impl FnMut(usize, Range<usize>, Range<usize>)) {
    let mut done = 0;
    while done < len {
        let at = usize::try_from(offset).unwrap() + done;
        let within = at % CHUNK;
        let n = (CHUNK - within).min(len - done);
        f(at / CHUNK, within..within + n, done..done + n);
        done += n;
    }
}

/// The data file of one replica of a [`Group`].
pub struct Disk {
    group: Group,
    replica: usize,
    zones: [Sparse; 4],
    capacity: usize,
    in_flight: Vec<Op>,
    log: Vec<Fault>,
    faults: Faults,
    random: SplitMix64,
    clock: Clock,
}

impl Disk {
    /// The zeroed data file of `replica` in `group`, holding `capacity`
    /// operations in flight, injecting `faults`, on the clock of `sim` and
    /// with a generator seeded from it.
    pub fn new<E: Hash>(
        group: &Group,
        replica: usize,
        capacity: usize,
        faults: Faults,
        sim: &mut Scheduler<E>,
    ) -> Self {
        assert!(capacity > 0);
        assert!(replica < group.0.borrow().corrupt.len());
        let [flip, write, lost, read, error] = [
            faults.bit_flip,
            faults.misdirected_write,
            faults.lost_write,
            faults.misdirected_read,
            faults.read_error,
        ]
        .map(u64::from);
        let million = u64::from(MILLION);
        assert!(flip + write + lost <= million && read + error <= million);
        assert!(faults.latency <= MILLION && (faults.latency == 0 || faults.latency_max > 0));
        let sizes = group.0.borrow().zone_sizes;
        Disk {
            group: group.clone(),
            replica,
            zones: sizes.map(Sparse::new),
            capacity,
            // Sized up front, so that a running disk allocates nothing.
            in_flight: Vec::with_capacity(capacity),
            log: Vec::with_capacity(capacity),
            faults,
            random: SplitMix64(sim.next_u64()),
            clock: sim.clock(),
        }
    }

    /// One delivered completion, or `None` when none is due yet.
    pub fn poll(&mut self) -> Option<Completion> {
        let now = self.clock.monotonic();
        let due = self.in_flight.iter().filter(|op| op.ready <= now).count();
        if due == 0 {
            return None;
        }
        let pick = (self.random.next_u64() % due as u64) as usize;
        let index = (0..self.in_flight.len())
            .filter(|&i| self.in_flight[i].ready <= now)
            .nth(pick)
            .expect("picked among the due");
        let Op {
            tag, access, grow, ..
        } = self.in_flight.swap_remove(index);
        if let Some(size) = grow {
            self.grow(size);
            return Some(Completion::Grown {
                tag,
                result: Ok(()),
            });
        }
        let Some(mut access) = access else {
            return Some(Completion::Fsync {
                tag,
                result: Ok(()),
            });
        };
        Some(if access.write {
            self.deliver_write(&access);
            Completion::Write {
                tag,
                buffer: access.buffer,
                result: Ok(()),
            }
        } else {
            let result = self.deliver_read(&mut access);
            Completion::Read {
                tag,
                buffer: access.buffer,
                result,
            }
        })
    }

    /// The earliest virtual time at which an operation in flight may
    /// complete, for the driver to schedule a poll.
    pub fn ready_at(&self) -> Option<u64> {
        self.in_flight.iter().map(|op| op.ready).min()
    }

    /// The process crashes: the operations in flight never complete, and
    /// their buffers go back to the pool. Each write in flight lands whole or
    /// not at all, or, with torn writes, sector by sector. The disk keeps its
    /// content for the process that restarts.
    pub fn crash(&mut self) {
        let mut ops = mem::take(&mut self.in_flight);
        for op in ops.drain(..) {
            let Some(access) = op.access.filter(|access| access.write) else {
                continue;
            };
            let torn = self.faults.torn_writes && self.log.len() < self.capacity;
            let whole = self.random.next_u64() & 1 == 1;
            for sector in 0..u64::from(access.len) / SECTOR {
                if (torn && self.random.next_u64() & 1 == 1) || (!torn && whole) {
                    let offset = access.offset + sector * SECTOR;
                    let at = (sector * SECTOR) as usize..((sector + 1) * SECTOR) as usize;
                    self.store(access.zone, offset, &access.buffer.bytes()[at]);
                }
            }
            if torn {
                self.record(FaultKind::TornWrite, Some(&access));
            }
        }
        self.in_flight = ops;
    }

    /// The faults injected since the last call, oldest first.
    pub fn faults(&mut self) -> std::vec::Drain<'_, Fault> {
        self.log.drain(..)
    }

    /// A draw in `0..MILLION`, or [`MILLION`], which no chance reaches, when
    /// the log is full.
    fn roll(&mut self) -> u64 {
        if self.log.len() == self.capacity {
            return u64::from(MILLION);
        }
        self.random.next_u64() % u64::from(MILLION)
    }

    fn record(&mut self, kind: FaultKind, access: Option<&Access>) {
        assert!(self.log.len() < self.capacity, "injected only with room");
        self.log.push(Fault {
            at: self.clock.monotonic(),
            kind,
            range: access.map(|a| (a.zone, a.offset, a.len)),
        });
    }

    /// Makes the grid at least `size` bytes, zeros after what it held.
    fn grow(&mut self, size: u64) {
        let grid = Zone::Grid as usize;
        if size > self.size(Zone::Grid) {
            self.zones[grid].grow(size);
            let sectors = usize::try_from(size / SECTOR).unwrap();
            self.group.0.borrow_mut().corrupt[self.replica][grid].resize(sectors, false);
        }
    }

    fn submit(&mut self, tag: Tag, access: Option<Access>, grow: Option<u64>) {
        assert!(self.in_flight.len() < self.capacity);
        let mut ready = self.clock.monotonic();
        if self.roll() < u64::from(self.faults.latency) {
            let delay = 1 + self.random.next_u64() % self.faults.latency_max;
            ready += delay;
            self.record(FaultKind::Latency { delay }, access.as_ref());
        }
        self.in_flight.push(Op {
            tag,
            ready,
            access,
            grow,
        });
    }

    /// Checks the storage preconditions against the operations in flight.
    fn check(&self, zone: Zone, offset: u64, len: u32, write: bool) {
        let end = offset + u64::from(len);
        assert!(offset.is_multiple_of(SECTOR) && u64::from(len).is_multiple_of(SECTOR));
        assert!(len > 0 && len as usize <= MESSAGE_SIZE);
        assert!(end <= self.size(zone), "range inside the zone");
        for other in self.in_flight.iter().filter_map(|op| op.access.as_ref()) {
            let other_end = other.offset + u64::from(other.len);
            assert!(
                other.zone != zone
                    || other_end <= offset
                    || end <= other.offset
                    || !(other.write || write),
                "no write overlaps another operation in flight"
            );
        }
    }

    /// Another sector-aligned offset of `zone` where `len` bytes fit, if any.
    fn elsewhere(&mut self, zone: Zone, offset: u64, len: u32) -> Option<u64> {
        let places = (self.size(zone) - u64::from(len)) / SECTOR + 1;
        if places < 2 {
            return None;
        }
        let mut place = self.random.next_u64() % (places - 1);
        if place >= offset / SECTOR {
            place += 1;
        }
        Some(place * SECTOR)
    }

    /// Writes `bytes` at `offset` as the process asked.
    fn store(&mut self, zone: Zone, offset: u64, bytes: &[u8]) {
        self.zones[zone as usize].write(offset, bytes);
        self.mark(zone, offset, bytes.len() as u64, false);
    }

    fn mark(&self, zone: Zone, offset: u64, len: u64, corrupt: bool) {
        let sectors = offset / SECTOR..(offset + len) / SECTOR;
        self.group.mark(self.replica, zone, sectors, corrupt);
    }

    fn allows(&self, zone: Zone, offset: u64, len: u64) -> bool {
        let sectors = offset / SECTOR..(offset + len).div_ceil(SECTOR);
        self.group.allows(self.replica, zone, sectors)
    }

    fn deliver_write(&mut self, access: &Access) {
        let Access {
            zone, offset, len, ..
        } = *access;
        let bytes = &access.buffer.bytes()[..len as usize];
        let size = u64::from(len);
        let f = self.faults;
        let roll = self.roll();
        let lost = u64::from(f.lost_write);
        let misdirected = lost + u64::from(f.misdirected_write);
        let flipped = misdirected + u64::from(f.bit_flip);
        if roll < lost && self.allows(zone, offset, size) {
            self.mark(zone, offset, size, true);
            self.record(FaultKind::LostWrite, Some(access));
            return;
        }
        if lost <= roll && roll < misdirected {
            let to = self.elsewhere(zone, offset, len);
            if let Some(to) =
                to.filter(|&to| self.allows(zone, offset, size) && self.allows(zone, to, size))
            {
                self.store(zone, to, bytes);
                self.mark(zone, to, size, true);
                self.mark(zone, offset, size, true);
                self.record(FaultKind::MisdirectedWrite { to }, Some(access));
                return;
            }
        }
        self.store(zone, offset, bytes);
        if misdirected <= roll && roll < flipped {
            let bit = self.random.next_u64() % (size * 8);
            let byte = offset + bit / 8;
            if self.allows(zone, byte, 1) {
                self.zones[zone as usize].flip(byte, 1 << (bit % 8));
                self.mark(zone, byte / SECTOR * SECTOR, SECTOR, true);
                self.record(FaultKind::BitFlip { bit }, Some(access));
            }
        }
    }

    fn deliver_read(&mut self, access: &mut Access) -> Result<(), StorageError> {
        let f = self.faults;
        let roll = self.roll();
        let misdirected = u64::from(f.misdirected_read);
        let mut from = access.offset;
        if roll < misdirected {
            if let Some(place) = self.elsewhere(access.zone, access.offset, access.len) {
                from = place;
                self.record(FaultKind::MisdirectedRead { from }, Some(&*access));
            }
        } else if roll < misdirected + u64::from(f.read_error) {
            self.record(FaultKind::ReadError, Some(&*access));
            return Err(StorageError);
        }
        let len = access.len as usize;
        let bytes = access
            .buffer
            .bytes_mut()
            .expect("a read buffer is not shared");
        self.zones[access.zone as usize].read(from, &mut bytes[..len]);
        Ok(())
    }
}

impl Storage for Disk {
    fn size(&self, zone: Zone) -> u64 {
        self.zones[zone as usize].len
    }

    fn read(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        assert_eq!(buffer.references(), 1, "a read buffer is not shared");
        if self.in_flight.len() == self.capacity {
            return Err(Full(buffer));
        }
        self.check(zone, offset, len, false);
        let access = Access {
            write: false,
            zone,
            offset,
            len,
            buffer,
        };
        self.submit(tag, Some(access), None);
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
        if self.in_flight.len() == self.capacity {
            return Err(Full(buffer));
        }
        self.check(zone, offset, len, true);
        let access = Access {
            write: true,
            zone,
            offset,
            len,
            buffer,
        };
        self.submit(tag, Some(access), None);
        Ok(())
    }

    fn fsync(&mut self, tag: Tag) -> Result<(), Full<()>> {
        if self.in_flight.len() == self.capacity {
            return Err(Full(()));
        }
        self.submit(tag, None, None);
        Ok(())
    }

    /// The growth lands when it completes: a crash before keeps the old size.
    fn grow_grid(&mut self, tag: Tag, size: u64) -> Result<(), Full<()>> {
        assert!(size.is_multiple_of(SECTOR));
        if self.in_flight.len() == self.capacity {
            return Err(Full(()));
        }
        self.submit(tag, None, Some(size));
        Ok(())
    }
}
