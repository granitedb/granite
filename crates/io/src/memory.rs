//! A trivial in-memory implementation of the [`Io`] traits.
//!
//! It shows that the traits need nothing from the real system, and it is the
//! backend of the conformance suite and of the completion-model spike. It
//! injects no faults and its clocks stand still; the simulator (E0.4) adds
//! both.
//!
//! Every operation is carried out as far as it can be when it is submitted,
//! and its completion is queued for [`Io::poll`], which returns the ready
//! completions in an order drawn from the node's seed. Nodes made from one
//! [`Hub`] can connect to each other. Dropping a node is a crash: its peers
//! see their connections to it reset.

use crate::message_pool::{MESSAGE_SIZE, Message, SECTOR_SIZE};
use crate::traits::{
    Completion, Connection, Full, Io, Network, NetworkError, Peer, Random, Storage, Tag, Time, Zone,
};
use granite_base::header::Header;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

/// The network that connects in-memory nodes.
#[derive(Clone, Default)]
pub struct Hub {
    state: Rc<RefCell<HubState>>,
}

#[derive(Default)]
struct HubState {
    nodes: BTreeMap<Peer, Mailbox>,
    /// Both ends of every open connection, by connection id.
    ends: BTreeMap<Connection, End>,
    next_end: u64,
}

struct Mailbox {
    /// Completions waiting for `poll`, with the range of a read or write;
    /// at most `capacity`.
    ready: Vec<(Completion, Option<Span>)>,
    /// Posted accepts, oldest first.
    accepts: VecDeque<Tag>,
    /// Connects to this node waiting for an accept: the connecting node and
    /// its tag. Bounded by the connecting nodes' capacities.
    connects: VecDeque<(Peer, Tag)>,
    /// Ends this node owns; at most `capacity`.
    connections: u32,
    capacity: u32,
}

/// The range of an in-flight read or write: zone, offset, end, is_write.
type Span = (Zone, u64, u64, bool);

struct End {
    owner: Peer,
    remote: Connection,
    /// The remote end is gone.
    reset: bool,
    /// Buffers posted by `recv`, oldest first.
    posted: VecDeque<(Tag, Message)>,
    /// Messages to this end waiting for a posted buffer: the sending node,
    /// its tag, the length and the buffer. Never non-empty with `posted`.
    inbox: VecDeque<(Peer, Tag, u32, Message)>,
}

impl Hub {
    /// A network with no nodes.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Queues a completion for `peer`; it is lost if the node is gone.
fn push(nodes: &mut BTreeMap<Peer, Mailbox>, peer: Peer, completion: Completion) {
    if let Some(mailbox) = nodes.get_mut(&peer) {
        mailbox.ready.push((completion, None));
    }
}

impl HubState {
    fn push(&mut self, peer: Peer, completion: Completion) {
        push(&mut self.nodes, peer, completion);
    }

    /// Pairs waiting connects to `peer` with its posted accepts.
    fn pair(&mut self, peer: Peer) {
        loop {
            let at = self.nodes.get_mut(&peer).expect("node exists");
            if at.accepts.is_empty() {
                return;
            }
            let Some((from, tag)) = at.connects.pop_front() else {
                return;
            };
            let full = at.connections == at.capacity;
            let Some(origin) = self.nodes.get(&from) else {
                continue; // The connecting node is gone.
            };
            if full || origin.connections == origin.capacity {
                let result = Err(NetworkError::Refused);
                self.push(from, Completion::Connected { tag, result });
                continue;
            }
            let at = self.nodes.get_mut(&peer).expect("node exists");
            let accept = at.accepts.pop_front().expect("checked");
            let (near, far) = (Connection(self.next_end), Connection(self.next_end + 1));
            self.next_end += 2;
            self.open(far, peer, near);
            self.open(near, from, far);
            self.push(
                from,
                Completion::Connected {
                    tag,
                    result: Ok(near),
                },
            );
            self.push(
                peer,
                Completion::Accepted {
                    tag: accept,
                    result: Ok(far),
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

    /// Fills posted buffers of `id` from its inbox, and fails what is left
    /// posted once the remote end is gone.
    fn deliver(&mut self, id: Connection) {
        let HubState { nodes, ends, .. } = self;
        let end = ends.get_mut(&id).expect("an open connection");
        while !end.posted.is_empty() && !end.inbox.is_empty() {
            let (tag, mut buffer) = end.posted.pop_front().expect("checked");
            let (from, sent, len, message) = end.inbox.pop_front().expect("checked");
            let bytes = buffer.bytes_mut().expect("posted buffers are not shared");
            bytes[..len as usize].copy_from_slice(&message.bytes()[..len as usize]);
            let result = Ok(len);
            push(
                nodes,
                end.owner,
                Completion::Received {
                    tag,
                    buffer,
                    result,
                },
            );
            let result = Ok(());
            push(
                nodes,
                from,
                Completion::Sent {
                    tag: sent,
                    buffer: message,
                    result,
                },
            );
        }
        if end.reset {
            for (tag, buffer) in end.posted.drain(..) {
                let result = Err(NetworkError::Reset);
                push(
                    nodes,
                    end.owner,
                    Completion::Received {
                        tag,
                        buffer,
                        result,
                    },
                );
            }
        }
    }

    /// Removes the end `id`: what is posted on it and what it sent that is not
    /// yet received fail with `error`, what waits in it goes back to its
    /// senders, and the remote end is reset.
    fn remove(&mut self, id: Connection, error: NetworkError) {
        let end = self.ends.remove(&id).expect("an open connection");
        if let Some(mailbox) = self.nodes.get_mut(&end.owner) {
            mailbox.connections -= 1;
        }
        for (tag, buffer) in end.posted {
            let result = Err(error);
            self.push(
                end.owner,
                Completion::Received {
                    tag,
                    buffer,
                    result,
                },
            );
        }
        for (from, tag, _, buffer) in end.inbox {
            let result = Err(NetworkError::Reset);
            self.push(
                from,
                Completion::Sent {
                    tag,
                    buffer,
                    result,
                },
            );
        }
        if let Some(remote) = self.ends.get_mut(&end.remote) {
            remote.reset = true;
            // Everything in the remote inbox was sent from this end.
            for (from, tag, _, buffer) in remote.inbox.drain(..) {
                let result = Err(error);
                push(
                    &mut self.nodes,
                    from,
                    Completion::Sent {
                        tag,
                        buffer,
                        result,
                    },
                );
            }
            self.deliver(end.remote);
        }
    }
}

/// One node: a data file, a network endpoint, a manual clock and a seeded
/// generator.
pub struct Memory {
    peer: Peer,
    hub: Hub,
    zones: [Vec<u8>; 4],
    capacity: u32,
    /// Operations submitted and not yet returned by `poll`.
    in_flight: u32,
    /// Reads and writes not yet returned by `poll`.
    spans: Vec<Span>,
    rng: SplitMix64,
}

impl Memory {
    /// Joins `hub` as `peer`, with zeroed zones of `zone_sizes` bytes (in
    /// [`Zone::ALL`] order), room for `capacity` operations in flight and as
    /// many connections.
    pub fn new(hub: &Hub, peer: Peer, zone_sizes: [u64; 4], capacity: u32, seed: u64) -> Self {
        assert!(capacity > 0);
        for size in zone_sizes {
            assert!(size.is_multiple_of(SECTOR_SIZE as u64));
        }
        // Sized up front, so that a running node allocates nothing.
        let mailbox = Mailbox {
            ready: Vec::with_capacity(capacity as usize),
            accepts: VecDeque::with_capacity(capacity as usize),
            connects: VecDeque::with_capacity(capacity as usize),
            connections: 0,
            capacity,
        };
        let previous = hub.state.borrow_mut().nodes.insert(peer, mailbox);
        assert!(previous.is_none(), "peer ids are unique on a hub");
        Memory {
            peer,
            hub: hub.clone(),
            zones: zone_sizes.map(|size| vec![0; usize::try_from(size).expect("zone fits")]),
            capacity,
            in_flight: 0,
            spans: Vec::with_capacity(capacity as usize),
            rng: SplitMix64(seed),
        }
    }

    /// Completions that `poll` can return now.
    pub fn ready(&self) -> usize {
        self.hub.state.borrow().nodes[&self.peer].ready.len()
    }

    fn admit(&mut self) -> bool {
        if self.in_flight == self.capacity {
            return false;
        }
        self.in_flight += 1;
        true
    }

    /// Queues a read's or write's completion with its range.
    fn push(&self, completion: Completion, span: Span) {
        let mut state = self.hub.state.borrow_mut();
        let mailbox = state.nodes.get_mut(&self.peer).expect("node is on the hub");
        mailbox.ready.push((completion, Some(span)));
    }

    /// Checks the storage preconditions and records the range in flight.
    fn range(&mut self, zone: Zone, offset: u64, len: u32, write: bool) -> std::ops::Range<usize> {
        let sector = SECTOR_SIZE as u64;
        let end = offset + u64::from(len);
        assert!(offset.is_multiple_of(sector) && u64::from(len).is_multiple_of(sector));
        assert!(len > 0 && len as usize <= MESSAGE_SIZE);
        assert!(end <= self.size(zone), "range inside the zone");
        for &(z, o, e, w) in &self.spans {
            assert!(
                z != zone || e <= offset || end <= o || !(w || write),
                "no write overlaps another operation in flight"
            );
        }
        self.spans.push((zone, offset, end, write));
        offset as usize..end as usize
    }
}

impl Drop for Memory {
    /// A crash: the node leaves the hub, its connections are reset at the
    /// other end, and connects waiting for it are refused.
    fn drop(&mut self) {
        let state = &mut *self.hub.state.borrow_mut();
        let mailbox = state.nodes.remove(&self.peer).expect("node is on the hub");
        for (from, tag) in mailbox.connects {
            state.push(
                from,
                Completion::Connected {
                    tag,
                    result: Err(NetworkError::Refused),
                },
            );
        }
        let owned: Vec<_> = state
            .ends
            .iter()
            .filter(|(_, end)| end.owner == self.peer)
            .map(|(&id, _)| id)
            .collect();
        for id in owned {
            state.remove(id, NetworkError::Closed);
        }
    }
}

impl Storage for Memory {
    fn size(&self, zone: Zone) -> u64 {
        self.zones[zone as usize].len() as u64
    }

    fn read(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        mut buffer: Message,
    ) -> Result<(), Full<Message>> {
        assert_eq!(buffer.references(), 1, "a read buffer is not shared");
        if !self.admit() {
            return Err(Full(buffer));
        }
        let range = self.range(zone, offset, len, false);
        let bytes = buffer.bytes_mut().expect("only handle");
        bytes[..len as usize].copy_from_slice(&self.zones[zone as usize][range]);
        let span = *self.spans.last().expect("just recorded");
        self.push(
            Completion::Read {
                tag,
                buffer,
                result: Ok(()),
            },
            span,
        );
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
        if !self.admit() {
            return Err(Full(buffer));
        }
        let range = self.range(zone, offset, len, true);
        self.zones[zone as usize][range].copy_from_slice(&buffer.bytes()[..len as usize]);
        let span = *self.spans.last().expect("just recorded");
        self.push(
            Completion::Write {
                tag,
                buffer,
                result: Ok(()),
            },
            span,
        );
        Ok(())
    }

    fn fsync(&mut self, tag: Tag) -> Result<(), Full<()>> {
        if !self.admit() {
            return Err(Full(()));
        }
        self.hub.state.borrow_mut().push(
            self.peer,
            Completion::Fsync {
                tag,
                result: Ok(()),
            },
        );
        Ok(())
    }

    fn grow_grid(&mut self, tag: Tag, size: u64) -> Result<(), Full<()>> {
        assert!(size.is_multiple_of(SECTOR_SIZE as u64));
        if !self.admit() {
            return Err(Full(()));
        }
        let grid = &mut self.zones[Zone::Grid as usize];
        if size > grid.len() as u64 {
            *grid = grown(grid, size);
        }
        self.hub.state.borrow_mut().push(
            self.peer,
            Completion::Grown {
                tag,
                result: Ok(()),
            },
        );
        Ok(())
    }
}

/// A zone of `size` bytes holding `zone` and zeros after it. Zero chunks are
/// not copied, so memory nobody wrote stays untouched: a grid grows by a
/// whole gibibyte at a time, mostly never written in a test.
pub fn grown(zone: &[u8], size: u64) -> Vec<u8> {
    let mut grown = vec![0; usize::try_from(size).expect("zone fits")];
    for (to, from) in grown.chunks_mut(SECTOR_SIZE).zip(zone.chunks(SECTOR_SIZE)) {
        if from.iter().any(|&b| b != 0) {
            to.copy_from_slice(from);
        }
    }
    grown
}

impl Network for Memory {
    fn connect(&mut self, tag: Tag, to: Peer) -> Result<(), Full<()>> {
        if !self.admit() {
            return Err(Full(()));
        }
        let state = &mut *self.hub.state.borrow_mut();
        match state.nodes.get_mut(&to) {
            Some(mailbox) => {
                mailbox.connects.push_back((self.peer, tag));
                state.pair(to);
            }
            None => state.push(
                self.peer,
                Completion::Connected {
                    tag,
                    result: Err(NetworkError::Refused),
                },
            ),
        }
        Ok(())
    }

    fn accept(&mut self, tag: Tag) -> Result<(), Full<()>> {
        if !self.admit() {
            return Err(Full(()));
        }
        let state = &mut *self.hub.state.borrow_mut();
        state
            .nodes
            .get_mut(&self.peer)
            .expect("node is on the hub")
            .accepts
            .push_back(tag);
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
        let state = &mut *self.hub.state.borrow_mut();
        let end = state.end(connection, self.peer);
        if end.reset {
            let result = Err(NetworkError::Reset);
            state.push(
                self.peer,
                Completion::Sent {
                    tag,
                    buffer,
                    result,
                },
            );
            return Ok(());
        }
        let remote = end.remote;
        let far = state
            .ends
            .get_mut(&remote)
            .expect("an end that is not reset has a remote");
        far.inbox.push_back((self.peer, tag, len, buffer));
        state.deliver(remote);
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
        let state = &mut *self.hub.state.borrow_mut();
        state
            .end(connection, self.peer)
            .posted
            .push_back((tag, buffer));
        state.deliver(connection);
        Ok(())
    }

    fn close(&mut self, connection: Connection) {
        let state = &mut *self.hub.state.borrow_mut();
        state.end(connection, self.peer);
        state.remove(connection, NetworkError::Closed);
    }
}

/// The clocks stand still; the simulator's virtual clock (T0.4.1) moves.
impl Time for Memory {
    fn monotonic(&self) -> u64 {
        0
    }

    fn realtime(&self) -> i64 {
        0
    }
}

impl Random for Memory {
    fn next_u64(&mut self) -> u64 {
        self.rng.next_u64()
    }
}

impl Io for Memory {
    fn poll(&mut self) -> Option<Completion> {
        let mut state = self.hub.state.borrow_mut();
        let ready = &mut state
            .nodes
            .get_mut(&self.peer)
            .expect("node is on the hub")
            .ready;
        if ready.is_empty() {
            return None;
        }
        let index = (self.rng.next_u64() % ready.len() as u64) as usize;
        let (completion, span) = ready.swap_remove(index);
        drop(state);
        self.in_flight -= 1;
        if let Some(span) = span {
            // Equal spans are interchangeable, so any match is the right one.
            let index = self.spans.iter().position(|&s| s == span);
            self.spans.swap_remove(index.expect("span was recorded"));
        }
        Some(completion)
    }
}

/// SplitMix64, a small deterministic generator.
#[derive(Clone)]
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}
