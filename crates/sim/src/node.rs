//! A simulated node: the [`Io`] a shard sees in the simulator, made of a
//! simulated [`Disk`], a [`Nic`] on the run's [`Fabric`](crate::network::Fabric),
//! the run's virtual [`Clock`] and a generator seeded from the run.
//!
//! The node also injects the process faults of document 06 section 1:
//! - **Crash** at an I/O boundary: after a submission is taken, or after a
//!   completion is taken from the device and before the process sees it,
//!   with a chance drawn from the seed. The disk tears its writes in flight
//!   ([`Disk::crash`]), the network drops the node's connections and
//!   messages in flight ([`Nic::crash`]), and the node is [`down`]: it
//!   swallows every later call. The driver then drops the process, which
//!   loses its memory, calls [`restart`] and starts a new process from what
//!   the disk holds.
//! - **Pause**: the process stops for a while, then resumes with its memory
//!   intact and virtual time moved on. Drawn on a completion, which is held
//!   and handed over after the pause, or asked for with [`Node::pause`].
//! - **Slow shard**: after each completion, the process is busy for `slow`
//!   nanoseconds of virtual time before it is handed the next one.
//!
//! While a process is paused or busy, [`Node::poll`] returns nothing; the
//! driver does not run it, and asks [`Node::ready_at`] when to poll again.
//!
//! Every boundary is counted by [`Site`], so a run can show that a crash
//! was injected at each one.
//!
//! [`down`]: Node::down
//! [`restart`]: Node::restart

use std::collections::BTreeMap;
use std::hash::Hash;
use std::panic::Location;

use granite_io::memory::SplitMix64;
use granite_io::message_pool::Message;
use granite_io::{
    Completion, Connection, Full, Io, Network, Peer, Random, Storage, Tag, Time, Zone,
};

use crate::network::Nic;
use crate::storage::MILLION;
use crate::{Clock, Disk, Scheduler};

/// Which process faults a node injects. A chance is in millionths. The
/// default injects none.
#[derive(Clone, Copy, Debug, Default)]
pub struct Faults {
    /// Chance that the process crashes at an I/O boundary.
    pub crash: u32,
    /// Chance that the process pauses before it is handed a completion.
    pub pause: u32,
    /// Longest pause, in nanoseconds of virtual time.
    pub pause_max: u64,
    /// Virtual nanoseconds the process is busy after each completion.
    pub slow: u64,
}

/// An operation of [`Io`], to name a [`Site`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Op {
    Read,
    Write,
    Fsync,
    Connect,
    Accept,
    Send,
    Recv,
    Close,
}

/// An I/O boundary where a crash can be injected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Site {
    /// A submission, by the place in the code that made it.
    Submit(Op, &'static Location<'static>),
    /// The delivery of a completion, by the operation it completes.
    Complete(Op),
}

/// How often a [`Site`] was reached, and how often a crash was injected
/// there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Visits {
    pub reached: u64,
    pub crashed: u64,
}

pub struct Node {
    pub disk: Disk,
    pub nic: Nic,
    clock: Clock,
    random: SplitMix64,
    faults: Faults,
    /// Draws the process faults, apart from `random`, so that enabling them
    /// leaves the rest of the run alone.
    chaos: SplitMix64,
    down: bool,
    /// The process is paused or busy until then.
    awake_at: u64,
    /// A completion taken from the device and held over a pause.
    held: Option<Completion>,
    /// One entry per site in the code, so bounded by the program.
    sites: BTreeMap<Site, Visits>,
}

impl Node {
    /// A node of `disk` and `nic`, on the clock of `sim` and with a generator
    /// seeded from it, injecting no process fault.
    pub fn new<E: Hash>(disk: Disk, nic: Nic, sim: &mut Scheduler<E>) -> Self {
        Node::with_faults(disk, nic, Faults::default(), sim)
    }

    /// A node as [`Node::new`] makes it, injecting `faults`.
    pub fn with_faults<E: Hash>(
        disk: Disk,
        nic: Nic,
        faults: Faults,
        sim: &mut Scheduler<E>,
    ) -> Self {
        assert!(faults.crash <= MILLION && faults.pause <= MILLION);
        assert!(faults.pause == 0 || faults.pause_max > 0);
        Node {
            disk,
            nic,
            clock: sim.clock(),
            random: SplitMix64(sim.next_u64()),
            faults,
            chaos: SplitMix64(sim.next_u64()),
            down: false,
            awake_at: 0,
            held: None,
            sites: BTreeMap::new(),
        }
    }

    /// The process crashes now: the disk tears its writes in flight, the
    /// network drops the node's connections and messages in flight, and the
    /// node swallows every call until [`Node::restart`].
    pub fn crash(&mut self) {
        self.disk.crash();
        self.nic.crash();
        self.held = None;
        self.awake_at = 0;
        self.down = true;
    }

    /// The process crashed, at an injected boundary or by [`Node::crash`]:
    /// the driver drops it and restarts the node.
    pub fn down(&self) -> bool {
        self.down
    }

    /// A new process runs on the node, with nothing from before the crash.
    pub fn restart(&mut self) {
        assert!(self.down, "only a crashed node restarts");
        self.down = false;
    }

    /// The process stops for `duration` nanoseconds of virtual time.
    pub fn pause(&mut self, duration: u64) {
        self.awake_at = self.awake_at.max(self.clock.monotonic() + duration);
    }

    /// Whether the process may run now: not down, paused or busy.
    pub fn running(&self) -> bool {
        !self.down && self.clock.monotonic() >= self.awake_at
    }

    /// The earliest virtual time at which [`Io::poll`] may return a
    /// completion, for the driver to schedule a poll.
    pub fn ready_at(&self) -> Option<u64> {
        if self.down {
            return None;
        }
        let device = match (self.disk.ready_at(), self.nic.ready_at()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let due = if self.held.is_some() {
            Some(self.awake_at)
        } else {
            device
        };
        due.map(|at| at.max(self.awake_at))
    }

    /// The boundaries reached so far, with their counts.
    pub fn sites(&self) -> &BTreeMap<Site, Visits> {
        &self.sites
    }

    fn roll(&mut self, chance: u32) -> bool {
        chance > 0 && self.chaos.next_u64() % u64::from(MILLION) < u64::from(chance)
    }

    /// Counts `site` and draws whether the process crashes there.
    fn boundary(&mut self, site: Site) {
        let crash = self.roll(self.faults.crash);
        let visits = self.sites.entry(site).or_default();
        visits.reached += 1;
        if crash {
            visits.crashed += 1;
            self.crash();
        }
    }

    #[track_caller]
    fn submitted(&mut self, op: Op) {
        self.boundary(Site::Submit(op, Location::caller()));
    }
}

/// The operation a completion completes.
pub(crate) fn op(completion: &Completion) -> Op {
    match completion {
        Completion::Read { .. } => Op::Read,
        Completion::Write { .. } => Op::Write,
        Completion::Fsync { .. } => Op::Fsync,
        Completion::Connected { .. } => Op::Connect,
        Completion::Accepted { .. } => Op::Accept,
        Completion::Sent { .. } => Op::Send,
        Completion::Received { .. } => Op::Recv,
    }
}

/// While the node is down, a submission is taken and dropped: the process
/// that made it no longer exists, and its buffer goes back to the pool.
impl Storage for Node {
    fn size(&self, zone: Zone) -> u64 {
        self.disk.size(zone)
    }

    #[track_caller]
    fn read(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        if self.down {
            return Ok(());
        }
        self.disk.read(tag, zone, offset, len, buffer)?;
        self.submitted(Op::Read);
        Ok(())
    }

    #[track_caller]
    fn write(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        if self.down {
            return Ok(());
        }
        self.disk.write(tag, zone, offset, len, buffer)?;
        self.submitted(Op::Write);
        Ok(())
    }

    #[track_caller]
    fn fsync(&mut self, tag: Tag) -> Result<(), Full<()>> {
        if self.down {
            return Ok(());
        }
        self.disk.fsync(tag)?;
        self.submitted(Op::Fsync);
        Ok(())
    }
}

impl Network for Node {
    #[track_caller]
    fn connect(&mut self, tag: Tag, to: Peer) -> Result<(), Full<()>> {
        if self.down {
            return Ok(());
        }
        self.nic.connect(tag, to)?;
        self.submitted(Op::Connect);
        Ok(())
    }

    #[track_caller]
    fn accept(&mut self, tag: Tag) -> Result<(), Full<()>> {
        if self.down {
            return Ok(());
        }
        self.nic.accept(tag)?;
        self.submitted(Op::Accept);
        Ok(())
    }

    #[track_caller]
    fn send(
        &mut self,
        tag: Tag,
        connection: Connection,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        if self.down {
            return Ok(());
        }
        self.nic.send(tag, connection, len, buffer)?;
        self.submitted(Op::Send);
        Ok(())
    }

    #[track_caller]
    fn recv(
        &mut self,
        tag: Tag,
        connection: Connection,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        if self.down {
            return Ok(());
        }
        self.nic.recv(tag, connection, buffer)?;
        self.submitted(Op::Recv);
        Ok(())
    }

    #[track_caller]
    fn close(&mut self, connection: Connection) {
        // A crash already closed every connection of the node.
        if self.down {
            return;
        }
        self.nic.close(connection);
        self.submitted(Op::Close);
    }
}

impl Time for Node {
    fn monotonic(&self) -> u64 {
        self.clock.monotonic()
    }

    fn realtime(&self) -> i64 {
        self.clock.realtime()
    }
}

impl Random for Node {
    fn next_u64(&mut self) -> u64 {
        self.random.next_u64()
    }
}

impl Io for Node {
    /// A due completion of the disk or the network, which first drawn from
    /// the node's generator; none while the process is down, paused or busy.
    fn poll(&mut self) -> Option<Completion> {
        if !self.running() {
            return None;
        }
        let held = self.held.take();
        let fresh = held.is_none();
        let completion = held.or_else(|| {
            if self.random.next_u64() & 1 == 0 {
                self.disk.poll().or_else(|| self.nic.poll())
            } else {
                self.nic.poll().or_else(|| self.disk.poll())
            }
        })?;
        // A held completion is not paused again: a pause ends.
        if fresh && self.roll(self.faults.pause) {
            let pause = 1 + self.chaos.next_u64() % self.faults.pause_max;
            self.held = Some(completion);
            self.pause(pause);
            return None;
        }
        self.boundary(Site::Complete(op(&completion)));
        if self.down {
            return None; // The process never sees it.
        }
        self.pause(self.faults.slow);
        Some(completion)
    }
}
