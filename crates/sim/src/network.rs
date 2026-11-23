//! Simulated network: in-memory connections between simulated nodes that
//! inject the network faults of document 06 section 1.
//!
//! A [`Fabric`] connects the [`Nic`]s made from it, as [`granite_io::memory`]'s
//! hub connects its nodes, with three differences: a message arrives after a
//! delay drawn from a [`Latency`] model, on the run's virtual clock; the
//! fabric injects faults; and every fault decision is drawn from a generator
//! seeded from the run's [`Scheduler`], so the faults are a function of the
//! seed.
//!
//! A sent message waits in the receiving end's inbox until its arrival time
//! and a posted buffer; delivery happens in [`Nic::poll`]. Its sender keeps
//! the buffer until then, so a receiver that posts nothing pushes back on its
//! sender. Without faults, the messages of one connection arrive exactly once
//! and in the order they were sent.
//!
//! Faults, drawn once per message, at most one per message:
//! - **Reset**: the connection is reset; both ends fail what is posted and
//!   what waits in their inboxes, as a peer's close does.
//! - **Drop**: the message is acknowledged and never arrives.
//! - **Delay**: the message arrives up to `delay_max` late, and the messages
//!   after it on its connection no earlier.
//! - **Duplicate**: the message arrives twice in a row.
//! - **Reorder**: the message overtakes the one sent before it on its
//!   connection, if that one has not arrived yet; otherwise nothing happens
//!   and no fault is recorded, so the measured rate is a little under the
//!   chance when the inbox is often empty.
//!
//! Drawn on each [`Fabric::tick`], at most one partition at a time:
//! - **Symmetric partition**: the nodes split into two sides, and no message
//!   crosses between them.
//! - **Asymmetric partition**: the same split, but only messages from the
//!   first side to the second are lost.
//!
//! A message across a partition is acknowledged and lost, and a connect
//! across one times out after [`CONNECT_TIMEOUT`]. Connections are not reset
//! by a partition; a heal lets messages through again.
//!
//! With reorder, duplicate or drop enabled, the network breaks the promise of
//! [`Network`] that a connection carries every message once and in order: the
//! protocols above it must not rely on it (document 02).
//!
//! A process crash ([`Nic::crash`]) resets every connection of its node, as
//! if each were closed, except that the messages the node sent and those
//! sent to it that have not arrived are dropped, and nothing it posted ever
//! completes. Connects from other nodes wait for the restarted process to
//! accept them.
//!
//! Every injected fault is recorded in a log that the driver drains with
//! [`Fabric::faults`]. The log holds `log_capacity` entries; a full log stops
//! injection until it is drained.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use granite_base::header::Header;
use granite_io::memory::SplitMix64;
use granite_io::message_pool::Message;
use granite_io::{
    CONNECT_TIMEOUT, Completion, Connection, Full, Network, NetworkError, Peer, Random, Tag, Time,
};

use crate::storage::MILLION;
use crate::{Clock, Scheduler};

const MICROSECOND: u64 = 1_000;

/// The one-way delay of a message: uniform in `min..=max` nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Latency {
    pub min: u64,
    pub max: u64,
}

impl Latency {
    /// Every message arrives at once.
    pub const ZERO: Latency = Latency { min: 0, max: 0 };
    /// One data center (document 02): round trips stay under a millisecond.
    pub const DATA_CENTER: Latency = Latency {
        min: 25 * MICROSECOND,
        max: 250 * MICROSECOND,
    };
}

/// Which faults a fabric injects. A chance is in millionths. The message
/// chances together, and the partition chances together, are at most
/// [`MILLION`]. The default injects none.
#[derive(Clone, Copy, Debug, Default)]
pub struct Faults {
    /// Chance that sending a message resets its connection.
    pub reset: u32,
    /// Chance that a message is lost.
    pub drop: u32,
    /// Chance that a message is delayed.
    pub delay: u32,
    /// Longest delay, in nanoseconds of virtual time.
    pub delay_max: u64,
    /// Chance that a message arrives twice.
    pub duplicate: u32,
    /// Chance that a message overtakes the one before it.
    pub reorder: u32,
    /// Chance per tick, with no partition, that a symmetric one starts.
    pub symmetric_partition: u32,
    /// Chance per tick, with no partition, that an asymmetric one starts.
    pub asymmetric_partition: u32,
    /// Chance per tick that the partition heals.
    pub heal: u32,
}

/// One injected fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    /// Virtual time of the injection.
    pub at: u64,
    pub kind: FaultKind,
    /// The message's sender and receiver; for a partition or a heal, the
    /// direction it blocks or opens. A partition is one entry per direction.
    pub from: Peer,
    pub to: Peer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaultKind {
    Reset,
    Drop,
    /// The message arrives `delay` nanoseconds late.
    Delay {
        delay: u64,
    },
    Duplicate,
    Reorder,
    SymmetricPartition,
    AsymmetricPartition,
    /// Not a fault: the end of a partition, logged to place it in the trace.
    Heal,
}

/// By hand, since [`Peer`] has no `Hash`.
impl Hash for Fault {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.at, self.kind, self.from.0, self.to.0).hash(state);
    }
}

/// A message in an inbox.
struct Packet {
    /// Virtual time from which it may be received.
    arrival: u64,
    from: Peer,
    tag: Tag,
    len: u32,
    buffer: Message,
    /// Times it is still to be received: 2 for a duplicate.
    copies: u8,
}

struct End {
    owner: Peer,
    remote: Connection,
    /// The connection was reset or the remote end closed; nothing more
    /// arrives, and `posted` and `inbox` stay empty.
    reset: bool,
    /// Buffers posted by `recv`, oldest first.
    posted: VecDeque<(Tag, Message)>,
    /// Messages to this end, by arrival time.
    inbox: VecDeque<Packet>,
}

struct Mailbox {
    /// Completions and the virtual time from which `poll` may return them.
    ready: Vec<(u64, Completion)>,
    /// Posted accepts, oldest first.
    accepts: VecDeque<Tag>,
    /// Connects to this node waiting for an accept.
    connects: VecDeque<(Peer, Tag)>,
    connections: u32,
    capacity: u32,
}

struct State {
    nodes: BTreeMap<Peer, Mailbox>,
    ends: BTreeMap<Connection, End>,
    next_end: u64,
    /// Directions (from, to) a partition blocks.
    blocked: BTreeSet<(Peer, Peer)>,
    faults: Faults,
    latency: Latency,
    log: Vec<Fault>,
    log_capacity: usize,
    messages: u64,
    random: SplitMix64,
    clock: Clock,
}

/// The network of one run, shared by its [`Nic`]s.
#[derive(Clone)]
pub struct Fabric(Rc<RefCell<State>>);

impl Fabric {
    /// A network with no nodes, injecting `faults`, delaying messages by
    /// `latency`, logging up to `log_capacity` faults between drains, on the
    /// clock of `sim` and with a generator seeded from it.
    pub fn new<E: Hash>(
        faults: Faults,
        latency: Latency,
        log_capacity: usize,
        sim: &mut Scheduler<E>,
    ) -> Self {
        let message = [
            faults.reset,
            faults.drop,
            faults.delay,
            faults.duplicate,
            faults.reorder,
        ];
        let message: u64 = message.into_iter().map(u64::from).sum();
        let partition =
            u64::from(faults.symmetric_partition) + u64::from(faults.asymmetric_partition);
        assert!(message <= u64::from(MILLION) && partition <= u64::from(MILLION));
        assert!(faults.heal <= MILLION && (faults.delay == 0 || faults.delay_max > 0));
        assert!(latency.min <= latency.max);
        Fabric(Rc::new(RefCell::new(State {
            nodes: BTreeMap::new(),
            ends: BTreeMap::new(),
            next_end: 0,
            blocked: BTreeSet::new(),
            faults,
            latency,
            log: Vec::with_capacity(log_capacity),
            log_capacity,
            messages: 0,
            random: SplitMix64(sim.next_u64()),
            clock: sim.clock(),
        })))
    }

    /// Draws the partition faults: heals the partition, or starts one. The
    /// driver calls it on its own schedule, for example on every tick.
    pub fn tick(&self) {
        self.0.borrow_mut().tick();
    }

    /// Appends the faults injected since the last call to `into`, oldest
    /// first.
    pub fn faults(&self, into: &mut Vec<Fault>) {
        into.append(&mut self.0.borrow_mut().log);
    }

    /// Messages sent on connections that were not reset, faulty or not.
    pub fn messages(&self) -> u64 {
        self.0.borrow().messages
    }

    /// The earliest virtual time at which a completion may be returned or a
    /// message received into a posted buffer, for the driver to schedule a
    /// poll.
    pub fn ready_at(&self) -> Option<u64> {
        let state = self.0.borrow();
        let ready = state
            .nodes
            .values()
            .flat_map(|m| m.ready.iter().map(|r| r.0));
        let arrivals = state
            .ends
            .values()
            .filter(|end| !end.posted.is_empty())
            .filter_map(|end| end.inbox.front().map(|packet| packet.arrival));
        ready.chain(arrivals).min()
    }
}

impl State {
    fn now(&self) -> u64 {
        self.clock.monotonic()
    }

    fn push(&mut self, peer: Peer, at: u64, completion: Completion) {
        push(&mut self.nodes, peer, at, completion);
    }

    /// A draw in `0..MILLION`, or [`MILLION`], which no chance reaches, when
    /// the log has no room for `entries`.
    fn roll(&mut self, entries: usize) -> u64 {
        if self.log.len() + entries > self.log_capacity {
            return u64::from(MILLION);
        }
        self.random.next_u64() % u64::from(MILLION)
    }

    fn record(&mut self, kind: FaultKind, from: Peer, to: Peer) {
        assert!(
            self.log.len() < self.log_capacity,
            "injected only with room"
        );
        let at = self.now();
        self.log.push(Fault { at, kind, from, to });
    }

    fn latency(&mut self) -> u64 {
        let Latency { min, max } = self.latency;
        min + self.random.next_u64() % (max - min + 1)
    }

    fn tick(&mut self) {
        let f = self.faults;
        if !self.blocked.is_empty() {
            if self.roll(self.blocked.len()) < u64::from(f.heal) {
                for (from, to) in std::mem::take(&mut self.blocked) {
                    self.record(FaultKind::Heal, from, to);
                }
            }
            return;
        }
        let peers = self.nodes.len() as u64;
        if peers < 2 {
            return;
        }
        let roll = self.random.next_u64() % u64::from(MILLION);
        let symmetric = u64::from(f.symmetric_partition);
        let kind = if roll < symmetric {
            FaultKind::SymmetricPartition
        } else if roll < symmetric + u64::from(f.asymmetric_partition) {
            FaultKind::AsymmetricPartition
        } else {
            return;
        };
        // The first side: a random pivot, never the peer after it, and each
        // other peer by a coin drawn from `salt`.
        let pivot = self.random.next_u64() % peers;
        let anchor = (pivot + 1) % peers;
        let salt = self.random.next_u64();
        let keys: Vec<Peer> = self.nodes.keys().copied().collect();
        let first = |i: u64| {
            i == pivot || (i != anchor && SplitMix64(salt ^ keys[i as usize].0).next_u64() & 1 == 1)
        };
        let ones = (0..peers).filter(|&i| first(i)).count();
        let directions = if kind == FaultKind::SymmetricPartition {
            2
        } else {
            1
        };
        if self.log.len() + directions * ones * (keys.len() - ones) > self.log_capacity {
            return; // No room in the log.
        }
        for a in (0..peers).filter(|&i| first(i)) {
            for b in (0..peers).filter(|&i| !first(i)) {
                let (a, b) = (keys[a as usize], keys[b as usize]);
                self.blocked.insert((a, b));
                self.record(kind, a, b);
                if kind == FaultKind::SymmetricPartition {
                    self.blocked.insert((b, a));
                    self.record(kind, b, a);
                }
            }
        }
    }

    /// Pairs waiting connects to `peer` with its posted accepts.
    fn pair(&mut self, peer: Peer) {
        loop {
            let now = self.now();
            let at = self.nodes.get_mut(&peer).expect("node exists");
            if at.accepts.is_empty() {
                return;
            }
            let Some((from, tag)) = at.connects.pop_front() else {
                return;
            };
            let full = at.connections == at.capacity;
            if self.blocked.contains(&(from, peer)) || self.blocked.contains(&(peer, from)) {
                let result = Err(NetworkError::TimedOut);
                self.push(
                    from,
                    now + CONNECT_TIMEOUT,
                    Completion::Connected { tag, result },
                );
                continue;
            }
            if full || self.nodes[&from].connections == self.nodes[&from].capacity {
                let result = Err(NetworkError::Refused);
                self.push(from, now, Completion::Connected { tag, result });
                continue;
            }
            let at = self.nodes.get_mut(&peer).expect("node exists");
            let accept = at.accepts.pop_front().expect("checked");
            let (near, far) = (Connection(self.next_end), Connection(self.next_end + 1));
            self.next_end += 2;
            self.open(far, peer, near);
            self.open(near, from, far);
            // The handshake takes one trip.
            let at = now + self.latency();
            let result = Ok(near);
            self.push(from, at, Completion::Connected { tag, result });
            let result = Ok(far);
            self.push(
                peer,
                at,
                Completion::Accepted {
                    tag: accept,
                    result,
                },
            );
        }
    }

    fn open(&mut self, id: Connection, owner: Peer, remote: Connection) {
        let mailbox = self.nodes.get_mut(&owner).expect("owner exists");
        mailbox.connections += 1;
        // Sized up front, so that a running connection allocates nothing.
        let capacity = mailbox.capacity as usize;
        let end = End {
            owner,
            remote,
            reset: false,
            posted: VecDeque::with_capacity(capacity),
            inbox: VecDeque::with_capacity(capacity),
        };
        self.ends.insert(id, end);
    }

    /// The end `id`, checking that `owner` holds it.
    fn end(&mut self, id: Connection, owner: Peer) -> &mut End {
        let end = self.ends.get_mut(&id).expect("an open connection");
        assert_eq!(end.owner, owner, "a connection is used only by its node");
        end
    }

    /// Marks the end `id` reset: what is posted on it fails with `received`,
    /// what waits in its inbox goes back to its senders with `sent`.
    fn fail(&mut self, id: Connection, received: NetworkError, sent: NetworkError) {
        let now = self.now();
        let State { nodes, ends, .. } = self;
        let Some(end) = ends.get_mut(&id) else {
            return; // Already closed.
        };
        end.reset = true;
        for (tag, buffer) in end.posted.drain(..) {
            let result = Err(received);
            push(
                nodes,
                end.owner,
                now,
                Completion::Received {
                    tag,
                    buffer,
                    result,
                },
            );
        }
        for packet in end.inbox.drain(..) {
            let (tag, buffer, result) = (packet.tag, packet.buffer, Err(sent));
            push(
                nodes,
                packet.from,
                now,
                Completion::Sent {
                    tag,
                    buffer,
                    result,
                },
            );
        }
    }

    /// Receives every message that has arrived into the buffers posted for
    /// it, oldest first.
    fn deliver(&mut self) {
        let now = self.now();
        let State { nodes, ends, .. } = self;
        // shortcut: scans every connection on every poll; keep a queue by
        // arrival time if runs with many connections get slow.
        for end in ends.values_mut() {
            while !end.posted.is_empty() && end.inbox.front().is_some_and(|p| p.arrival <= now) {
                let (tag, mut buffer) = end.posted.pop_front().expect("checked");
                let packet = end.inbox.front_mut().expect("checked");
                let len = packet.len as usize;
                let bytes = buffer.bytes_mut().expect("posted buffers are not shared");
                bytes[..len].copy_from_slice(&packet.buffer.bytes()[..len]);
                let result = Ok(packet.len);
                push(
                    nodes,
                    end.owner,
                    now,
                    Completion::Received {
                        tag,
                        buffer,
                        result,
                    },
                );
                packet.copies -= 1;
                if packet.copies == 0 {
                    let packet = end.inbox.pop_front().expect("checked");
                    let (tag, buffer, result) = (packet.tag, packet.buffer, Ok(()));
                    push(
                        nodes,
                        packet.from,
                        now,
                        Completion::Sent {
                            tag,
                            buffer,
                            result,
                        },
                    );
                }
            }
        }
    }

    /// Puts a message from `from` on the connection whose sending end is
    /// `id`, injecting the message faults.
    fn send(&mut self, from: Peer, id: Connection, tag: Tag, len: u32, buffer: Message) {
        let now = self.now();
        let end = self.end(id, from);
        if end.reset {
            let result = Err(NetworkError::Reset);
            self.push(
                from,
                now,
                Completion::Sent {
                    tag,
                    buffer,
                    result,
                },
            );
            return;
        }
        let remote = end.remote;
        let to = self.ends[&remote].owner;
        self.messages += 1;
        if self.blocked.contains(&(from, to)) {
            let result = Ok(());
            self.push(
                from,
                now,
                Completion::Sent {
                    tag,
                    buffer,
                    result,
                },
            );
            return;
        }
        let f = self.faults;
        let roll = self.roll(1);
        let reset = u64::from(f.reset);
        let drop = reset + u64::from(f.drop);
        let delay = drop + u64::from(f.delay);
        let duplicate = delay + u64::from(f.duplicate);
        let reorder = duplicate + u64::from(f.reorder);
        if roll < reset {
            self.record(FaultKind::Reset, from, to);
            self.fail(id, NetworkError::Reset, NetworkError::Reset);
            self.fail(remote, NetworkError::Reset, NetworkError::Reset);
            let result = Err(NetworkError::Reset);
            self.push(
                from,
                now,
                Completion::Sent {
                    tag,
                    buffer,
                    result,
                },
            );
            return;
        }
        if roll < drop {
            self.record(FaultKind::Drop, from, to);
            let result = Ok(());
            self.push(
                from,
                now,
                Completion::Sent {
                    tag,
                    buffer,
                    result,
                },
            );
            return;
        }
        let mut arrival = now + self.latency();
        if roll < delay {
            let late = 1 + self.random.next_u64() % f.delay_max;
            arrival += late;
            self.record(FaultKind::Delay { delay: late }, from, to);
        }
        let copies = if (delay..duplicate).contains(&roll) {
            self.record(FaultKind::Duplicate, from, to);
            2
        } else {
            1
        };
        let inbox = &self.ends[&remote].inbox;
        // Messages on one connection arrive in order.
        let arrival = inbox
            .back()
            .map_or(arrival, |last| arrival.max(last.arrival));
        let overtake = (duplicate..reorder).contains(&roll) && !inbox.is_empty();
        let mut packet = Packet {
            arrival,
            from,
            tag,
            len,
            buffer,
            copies,
        };
        let inbox = &mut self.ends.get_mut(&remote).expect("checked").inbox;
        if overtake {
            // Swap arrival times, so the inbox stays in arrival order.
            let last = inbox.back_mut().expect("checked");
            std::mem::swap(&mut packet.arrival, &mut last.arrival);
            inbox.insert(inbox.len() - 1, packet);
            self.record(FaultKind::Reorder, from, to);
        } else {
            inbox.push_back(packet);
        }
    }

    /// The process on `peer` crashes; see [`Nic::crash`].
    fn crash(&mut self, peer: Peer) {
        while let Some((&id, end)) = self.ends.iter().find(|(_, end)| end.owner == peer) {
            let remote = end.remote;
            // The remote end fails what it posted; the messages in its inbox
            // came from `peer` and go back to it, where they are dropped below.
            self.fail(remote, NetworkError::Reset, NetworkError::Reset);
            // What was sent to `peer` goes back to its senders, failed.
            self.fail(id, NetworkError::Reset, NetworkError::Reset);
            self.ends.remove(&id);
        }
        for mailbox in self.nodes.values_mut() {
            mailbox.connects.retain(|&(from, _)| from != peer);
        }
        let mailbox = self.nodes.get_mut(&peer).expect("node is on the fabric");
        mailbox.ready.clear();
        mailbox.accepts.clear();
        mailbox.connections = 0;
    }

    /// The owner closes the end `id`: what is posted on it fails with
    /// `Closed`, what waits in its inbox goes back with `Reset`, and the
    /// remote end is reset.
    fn close(&mut self, owner: Peer, id: Connection) {
        let remote = self.end(id, owner).remote;
        self.fail(id, NetworkError::Closed, NetworkError::Reset);
        self.fail(remote, NetworkError::Reset, NetworkError::Closed);
        self.ends.remove(&id);
        self.nodes
            .get_mut(&owner)
            .expect("owner exists")
            .connections -= 1;
    }
}

fn push(nodes: &mut BTreeMap<Peer, Mailbox>, peer: Peer, at: u64, completion: Completion) {
    let mailbox = nodes.get_mut(&peer).expect("node is on the fabric");
    mailbox.ready.push((at, completion));
}

/// The network endpoint of one simulated node.
pub struct Nic {
    peer: Peer,
    fabric: Fabric,
    capacity: u32,
    /// Operations submitted and not yet returned by `poll`.
    in_flight: u32,
    random: SplitMix64,
}

impl Nic {
    /// Joins `fabric` as `peer`, with room for `capacity` operations in
    /// flight and as many connections, and a generator seeded from `sim`.
    pub fn new<E: Hash>(
        fabric: &Fabric,
        peer: Peer,
        capacity: u32,
        sim: &mut Scheduler<E>,
    ) -> Self {
        assert!(capacity > 0);
        // Sized up front, so that a running node allocates nothing.
        let mailbox = Mailbox {
            ready: Vec::with_capacity(capacity as usize),
            accepts: VecDeque::with_capacity(capacity as usize),
            connects: VecDeque::with_capacity(capacity as usize),
            connections: 0,
            capacity,
        };
        let previous = fabric.0.borrow_mut().nodes.insert(peer, mailbox);
        assert!(previous.is_none(), "peer ids are unique on a fabric");
        Nic {
            peer,
            fabric: fabric.clone(),
            capacity,
            in_flight: 0,
            random: SplitMix64(sim.next_u64()),
        }
    }

    /// Receives what has arrived, then returns one due completion, picked by
    /// the node's generator, or `None` when none is due.
    pub fn poll(&mut self) -> Option<Completion> {
        let mut state = self.fabric.0.borrow_mut();
        state.deliver();
        let now = state.now();
        let ready = &mut state
            .nodes
            .get_mut(&self.peer)
            .expect("on the fabric")
            .ready;
        let due = ready.iter().filter(|r| r.0 <= now).count();
        if due == 0 {
            return None;
        }
        let pick = (self.random.next_u64() % due as u64) as usize;
        let index = (0..ready.len())
            .filter(|&i| ready[i].0 <= now)
            .nth(pick)
            .expect("picked among the due");
        let (_, completion) = ready.swap_remove(index);
        self.in_flight -= 1;
        Some(completion)
    }

    /// The process on this node crashes: every connection of the node is
    /// reset, its peers' posted receives on them fail, messages sent to it
    /// go back to their senders failed, messages it sent that have not
    /// arrived are dropped, and nothing it submitted completes. The node
    /// stays on the fabric under the same peer, for the restarted process.
    pub fn crash(&mut self) {
        self.fabric.0.borrow_mut().crash(self.peer);
        self.in_flight = 0;
    }

    /// The earliest virtual time at which this node's [`Nic::poll`] may
    /// return a completion, for the driver to schedule a poll.
    pub fn ready_at(&self) -> Option<u64> {
        let state = self.fabric.0.borrow();
        let ready = state.nodes[&self.peer].ready.iter().map(|r| r.0);
        // An arrival on another node's end completes there, but its sender
        // learns of it through this node's mailbox only once it happens.
        let arrivals = state
            .ends
            .values()
            .filter(|end| !end.posted.is_empty())
            .filter_map(|end| {
                let packet = end.inbox.front()?;
                (end.owner == self.peer || packet.from == self.peer).then_some(packet.arrival)
            });
        ready.chain(arrivals).min()
    }

    fn admit(&mut self) -> bool {
        if self.in_flight == self.capacity {
            return false;
        }
        self.in_flight += 1;
        true
    }
}

impl Network for Nic {
    fn connect(&mut self, tag: Tag, to: Peer) -> Result<(), Full<()>> {
        if !self.admit() {
            return Err(Full(()));
        }
        let state = &mut *self.fabric.0.borrow_mut();
        match state.nodes.get_mut(&to) {
            Some(mailbox) => {
                mailbox.connects.push_back((self.peer, tag));
                state.pair(to);
            }
            None => {
                let result = Err(NetworkError::Refused);
                let now = state.now();
                state.push(self.peer, now, Completion::Connected { tag, result });
            }
        }
        Ok(())
    }

    fn accept(&mut self, tag: Tag) -> Result<(), Full<()>> {
        if !self.admit() {
            return Err(Full(()));
        }
        let state = &mut *self.fabric.0.borrow_mut();
        let mailbox = state.nodes.get_mut(&self.peer).expect("on the fabric");
        mailbox.accepts.push_back(tag);
        state.pair(self.peer);
        Ok(())
    }

    fn send(
        &mut self,
        tag: Tag,
        connection: Connection,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        assert!(
            Header::decode(buffer.bytes()).is_ok_and(|h| h.size.get() == len),
            "a send is one whole message"
        );
        if !self.admit() {
            return Err(Full(buffer));
        }
        let state = &mut *self.fabric.0.borrow_mut();
        state.send(self.peer, connection, tag, len, buffer);
        Ok(())
    }

    fn recv(
        &mut self,
        tag: Tag,
        connection: Connection,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        assert_eq!(buffer.references(), 1, "a receive buffer is not shared");
        if !self.admit() {
            return Err(Full(buffer));
        }
        let state = &mut *self.fabric.0.borrow_mut();
        let now = state.now();
        let end = state.end(connection, self.peer);
        if end.reset {
            let result = Err(NetworkError::Reset);
            state.push(
                self.peer,
                now,
                Completion::Received {
                    tag,
                    buffer,
                    result,
                },
            );
        } else {
            end.posted.push_back((tag, buffer));
        }
        Ok(())
    }

    fn close(&mut self, connection: Connection) {
        self.fabric.0.borrow_mut().close(self.peer, connection);
    }
}
