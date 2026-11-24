//! A toy replicated counter, with a deliberately simple protocol, that
//! proves the process faults of [`crate::node`] on top of the storage and
//! network faults.
//!
//! Three replicas each keep the counter in one checksummed sector of their
//! grid zone. One client, a fourth node, runs one operation at a time:
//! - **Increment**: send `Write(acked + 1)` to every replica until two
//!   acknowledge it.
//! - **Read**: send `Read(id)` to every replica until all three answer with
//!   their value, then write the largest back as an increment does, so that
//!   what a read returns is never lost either.
//!
//! A replica asked to write `v` writes it only if `v` is above what it
//! holds, one write and fsync at a time, and acknowledges `Write(v)` with
//! its value once that value is durable and at least `v`. A restarted
//! replica reads its whole zone, so a misdirected read has nowhere else to
//! read from, and takes an invalid sector as zero. A restarted client starts
//! with a read. Every request is resent each tick until answered.
//!
//! Why it holds: an acknowledged value is durable on two replicas, and the
//! storage bound keeps the sector corrupt on at most one of them at a time;
//! a replica only ever overwrites its value with a larger one, and a crash
//! drops every message sent before it, so no stale request lowers a
//! restarted replica. A read asks all three, so it reaches the replica that
//! still holds the value.
//!
//! The checker, outside every process, asserts that each completed
//! operation returns at least the largest value returned before (the
//! counter never goes backwards and never loses an acknowledged increment),
//! and that an increment returns more.

use std::collections::BTreeMap;

use granite_base::checksum::checksum;
use granite_base::constants::FORMAT;
use granite_io::conformance::seal;
use granite_io::memory::SplitMix64;
use granite_io::message_pool::{Message, MessagePool, SECTOR_SIZE};
use granite_io::{Completion, Connection, Io, Network, Peer, Random, Storage, Tag, Time, Zone};

use crate::network::{self, Fabric, Latency, Nic};
use crate::node::{self, Site, Visits};
use crate::runner::{Event as LogEvent, Failure, Log, POOL, Scenario};
use crate::storage::{self, Disk, Group};
use crate::{Node, Scheduler};

pub const REPLICAS: usize = 3;
const QUORUM: usize = 2;
const CLIENT: usize = REPLICAS;
pub const NODES: usize = REPLICAS + 1;

const MILLISECOND: u64 = 1_000_000;
const TICK: u64 = MILLISECOND;
/// A run that has not finished its operations by then stops.
const LIMIT: u64 = 10_000 * MILLISECOND;

/// The grid zone, all of it read at once.
const ZONE: u32 = 8 * SECTOR_SIZE as u32;
const ZONES: [u64; 4] = [0, 0, 0, ZONE as u64];
const DISK_CAPACITY: usize = 4;
const NIC_CAPACITY: u32 = 16;
/// Sends in flight a replica allows itself.
const SENDING: u32 = 2;
const LOG: usize = 1_000;

/// A message: a header, then its kind, value and read id.
const BODY: usize = FORMAT.header_size as usize;
const LEN: u32 = BODY as u32 + 24;
const WRITE: u64 = 1;
const READ: u64 = 2;
const ACK: u64 = 3;
const VALUE: u64 = 4;

/// A tag is a kind in its high 16 bits and an argument below.
const T_READ: u64 = 1;
const T_WRITE: u64 = 2;
const T_FSYNC: u64 = 3;
const T_ACCEPT: u64 = 4;
/// The argument is the replica.
const T_CONNECT: u64 = 5;
/// The argument is the connection.
const T_RECV: u64 = 6;
const T_SEND: u64 = 7;

fn tag(kind: u64, arg: u64) -> Tag {
    assert!(arg < 1 << 48);
    kind << 48 | arg
}

fn split(tag: Tag) -> (u64, u64) {
    (tag >> 48, tag & ((1 << 48) - 1))
}

/// What a run injects, and the injected bug.
#[derive(Clone, Copy, Debug, Default)]
pub struct Config {
    /// Client operations to complete.
    pub ops: u64,
    pub storage: storage::Faults,
    pub network: network::Faults,
    /// By node: the replicas, then the client.
    pub process: [node::Faults; NODES],
    /// Injected bug: a replica acknowledges a write without making it
    /// durable.
    pub skip_durable_write: bool,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub trace: u64,
    /// Client operations completed.
    pub ops: u64,
    /// The first check that failed.
    pub violation: Option<String>,
    /// Every node's I/O boundaries, merged.
    pub sites: BTreeMap<Site, Visits>,
    pub storage_faults: u64,
    pub network_faults: u64,
}

/// The value every process outside reads off the client's results.
#[derive(Default)]
struct Checker {
    /// The largest value an operation returned.
    floor: u64,
    /// The value the last operation returned.
    last: u64,
    ops: u64,
    violation: Option<String>,
}

impl Checker {
    fn done(&mut self, now: u64, value: u64, increment: bool) {
        self.ops += 1;
        self.last = value;
        let ok = if increment {
            value > self.floor
        } else {
            value >= self.floor
        };
        if !ok && self.violation.is_none() {
            let op = if increment { "increment" } else { "read" };
            self.violation = Some(format!(
                "at {now} ns, {op} returned {value} after {} was returned",
                self.floor
            ));
        }
        self.floor = self.floor.max(value);
    }
}

/// A message from `pool` of `kind`, `value` and `id`; `None` when the pool
/// is empty.
fn message(pool: &MessagePool, kind: u64, value: u64, id: u64) -> Option<Message> {
    let mut message = pool.acquire()?;
    let bytes = message.bytes_mut().expect("only handle");
    for (i, word) in [kind, value, id].into_iter().enumerate() {
        bytes[BODY + 8 * i..][..8].copy_from_slice(&word.to_le_bytes());
    }
    seal(bytes, LEN, 0);
    Some(message)
}

/// The kind, value and id of a received message.
fn parse(buffer: &Message, len: u32) -> (u64, u64, u64) {
    assert_eq!(len, LEN, "only the toy's messages are sent");
    let word =
        |i: usize| u64::from_le_bytes(buffer.bytes()[BODY + 8 * i..][..8].try_into().unwrap());
    (word(0), word(1), word(2))
}

/// The counter's sector: a checksum of the rest, then the value.
fn encode(bytes: &mut [u8], value: u64) {
    let sector = &mut bytes[..SECTOR_SIZE];
    sector.fill(0);
    sector[16..24].copy_from_slice(&value.to_le_bytes());
    let sum = checksum(&sector[16..]);
    sector[..16].copy_from_slice(&sum.to_le_bytes());
}

/// The value in the counter's sector, or zero if it does not check.
fn decode(bytes: &[u8]) -> u64 {
    let sector = &bytes[..SECTOR_SIZE];
    if sector[..16] != checksum(&sector[16..]).to_le_bytes() {
        return 0;
    }
    u64::from_le_bytes(sector[16..24].try_into().unwrap())
}

struct Replica {
    /// The value it answers with; `None` while it recovers.
    value: Option<u64>,
    durable: u64,
    /// The largest value it was asked to write.
    want: u64,
    /// The value being written and synced.
    writing: Option<u64>,
    /// The recovery read is to be submitted.
    recover: bool,
    accepting: bool,
    /// Open connections, and whether a receive is posted on each.
    connections: Vec<(Connection, bool)>,
    sending: u32,
    bug: bool,
}

impl Replica {
    fn start(io: &mut Node, pool: &MessagePool, bug: bool) -> Self {
        let mut replica = Replica {
            value: None,
            durable: 0,
            want: 0,
            writing: None,
            recover: true,
            accepting: false,
            connections: Vec::with_capacity(NIC_CAPACITY as usize),
            sending: 0,
            bug,
        };
        replica.tick(io, pool);
        replica
    }

    /// Retries what a full pool or device refused.
    fn tick(&mut self, io: &mut Node, pool: &MessagePool) {
        if self.recover
            && let Some(buffer) = pool.acquire()
        {
            self.recover = false;
            io.read(tag(T_READ, 0), Zone::Grid, 0, ZONE, buffer)
                .expect("room");
        }
        if !self.accepting && io.accept(tag(T_ACCEPT, 0)).is_ok() {
            self.accepting = true;
        }
        for i in 0..self.connections.len() {
            let (connection, posted) = self.connections[i];
            if !posted && let Some(buffer) = pool.acquire() {
                let tag = tag(T_RECV, connection.0);
                self.connections[i].1 = io.recv(tag, connection, buffer).is_ok();
            }
        }
        self.flush(io, pool);
    }

    /// Writes the largest value asked for, if above the durable one.
    fn flush(&mut self, io: &mut Node, pool: &MessagePool) {
        if self.value.is_none() || self.writing.is_some() || self.want <= self.durable {
            return;
        }
        let Some(mut buffer) = pool.acquire() else {
            return; // The next tick retries.
        };
        encode(buffer.bytes_mut().expect("only handle"), self.want);
        let len = SECTOR_SIZE as u32;
        io.write(tag(T_WRITE, 0), Zone::Grid, 0, len, buffer)
            .expect("room");
        self.writing = Some(self.want);
    }

    fn respond(&mut self, io: &mut Node, pool: &MessagePool, to: Connection, kind: u64, id: u64) {
        let value = self.value.expect("answers only once recovered");
        if self.sending == SENDING {
            return; // The client resends.
        }
        if let Some(buffer) = message(pool, kind, value, id)
            && io.send(tag(T_SEND, 0), to, LEN, buffer).is_ok()
        {
            self.sending += 1;
        }
    }

    fn receive(
        &mut self,
        io: &mut Node,
        pool: &MessagePool,
        from: Connection,
        buffer: &Message,
        len: u32,
    ) {
        let (kind, value, id) = parse(buffer, len);
        match kind {
            WRITE => {
                if self.bug {
                    // The value is acknowledged from memory, never written.
                    if self.value.is_some_and(|v| v < value) {
                        self.value = Some(value);
                    }
                } else {
                    self.want = self.want.max(value);
                    self.flush(io, pool);
                }
                if self.value.is_some_and(|v| v >= value) {
                    self.respond(io, pool, from, ACK, id);
                }
            }
            READ if self.value.is_some() => self.respond(io, pool, from, VALUE, id),
            READ => {}
            _ => panic!("a replica receives requests only"),
        }
    }

    fn drop_connection(&mut self, io: &mut Node, connection: Connection) {
        if let Some(i) = self.connections.iter().position(|c| c.0 == connection) {
            self.connections.swap_remove(i);
            io.close(connection);
        }
    }

    fn complete(&mut self, io: &mut Node, pool: &MessagePool, completion: Completion) {
        match completion {
            Completion::Read { buffer, result, .. } => {
                if result.is_err() {
                    io.read(tag(T_READ, 0), Zone::Grid, 0, ZONE, buffer)
                        .expect("room");
                    return;
                }
                let value = decode(buffer.bytes());
                self.durable = value;
                self.value = Some(value);
                self.flush(io, pool);
            }
            Completion::Write { result, .. } => {
                result.expect("the simulated disk fails no write");
                io.fsync(tag(T_FSYNC, 0)).expect("room");
            }
            Completion::Fsync { result, .. } => {
                result.expect("the simulated disk fails no fsync");
                self.durable = self.writing.take().expect("a write was synced");
                self.value = Some(self.durable);
                self.flush(io, pool);
            }
            Completion::Grown { .. } => unreachable!("the toy never grows its grid"),
            Completion::Accepted { result, .. } => {
                self.accepting = false;
                if let Ok(connection) = result {
                    self.connections.push((connection, false));
                }
                self.tick(io, pool);
            }
            Completion::Received {
                tag,
                buffer,
                result,
            } => {
                let connection = Connection(split(tag).1);
                let Some(i) = self.connections.iter().position(|c| c.0 == connection) else {
                    return; // Closed already.
                };
                let Ok(len) = result else {
                    self.drop_connection(io, connection);
                    return;
                };
                self.connections[i].1 = false;
                self.receive(io, pool, connection, &buffer, len);
                // Its buffer goes back to the pool, and a fresh one is posted.
                drop(buffer);
                self.tick(io, pool);
            }
            Completion::Sent { .. } => self.sending -= 1,
            Completion::Connected { .. } => panic!("a replica does not connect"),
        }
    }
}

/// The client's operation in progress.
enum Phase {
    /// Waiting for every replica's value.
    Read {
        id: u64,
        values: [Option<u64>; REPLICAS],
    },
    /// Waiting for a quorum to hold `value`; `read` if it completes a read.
    Write {
        value: u64,
        read: bool,
        acks: [bool; REPLICAS],
    },
}

struct Client {
    phase: Phase,
    connections: [Option<Connection>; REPLICAS],
    connecting: [bool; REPLICAS],
    posted: [bool; REPLICAS],
    sending: [bool; REPLICAS],
}

impl Client {
    fn start(io: &mut Node, pool: &MessagePool) -> Self {
        let mut client = Client {
            phase: Phase::Read {
                id: io.next_u64() >> 16,
                values: [None; REPLICAS],
            },
            connections: [None; REPLICAS],
            connecting: [false; REPLICAS],
            posted: [false; REPLICAS],
            sending: [false; REPLICAS],
        };
        client.tick(io, pool);
        client
    }

    fn replica(&self, connection: Connection) -> Option<usize> {
        self.connections.iter().position(|&c| c == Some(connection))
    }

    /// Connects, posts receives and sends the request to every replica that
    /// has not answered it.
    fn tick(&mut self, io: &mut Node, pool: &MessagePool) {
        for r in 0..REPLICAS {
            let Some(connection) = self.connections[r] else {
                if !self.connecting[r]
                    && io.connect(tag(T_CONNECT, r as u64), Peer(r as u64)).is_ok()
                {
                    self.connecting[r] = true;
                }
                continue;
            };
            if !self.posted[r]
                && let Some(buffer) = pool.acquire()
            {
                let tag = tag(T_RECV, connection.0);
                self.posted[r] = io.recv(tag, connection, buffer).is_ok();
            }
            let (kind, value, id, answered) = match self.phase {
                Phase::Read { id, values } => (READ, 0, id, values[r].is_some()),
                Phase::Write { value, acks, .. } => (WRITE, value, 0, acks[r]),
            };
            if answered || self.sending[r] {
                continue;
            }
            if let Some(buffer) = message(pool, kind, value, id) {
                let tag = tag(T_SEND, connection.0);
                self.sending[r] = io.send(tag, connection, LEN, buffer).is_ok();
            }
        }
    }

    fn receive(
        &mut self,
        io: &mut Node,
        r: usize,
        buffer: &Message,
        len: u32,
        checker: &mut Checker,
    ) {
        let (kind, value, id) = parse(buffer, len);
        match (&mut self.phase, kind) {
            (Phase::Read { id: want, values }, VALUE) if id == *want => {
                values[r] = Some(value);
                if values.iter().all(Option::is_some) {
                    let value = values
                        .iter()
                        .flatten()
                        .copied()
                        .max()
                        .expect("three values");
                    self.phase = Phase::Write {
                        value,
                        read: true,
                        acks: [false; REPLICAS],
                    };
                }
            }
            (
                Phase::Write {
                    value: want,
                    read,
                    acks,
                },
                ACK,
            ) if value >= *want => {
                acks[r] = true;
                if acks.iter().filter(|&&a| a).count() == QUORUM {
                    let value = *want;
                    checker.done(io.monotonic(), value, !*read);
                    self.phase = if io.next_u64() & 1 == 0 {
                        Phase::Write {
                            value: value + 1,
                            read: false,
                            acks: [false; REPLICAS],
                        }
                    } else {
                        Phase::Read {
                            id: io.next_u64() >> 16,
                            values: [None; REPLICAS],
                        }
                    };
                }
            }
            _ => {} // A late or duplicated answer.
        }
    }

    fn complete(
        &mut self,
        io: &mut Node,
        pool: &MessagePool,
        completion: Completion,
        checker: &mut Checker,
    ) {
        match completion {
            Completion::Connected { tag, result } => {
                let r = split(tag).1 as usize;
                self.connecting[r] = false;
                self.connections[r] = result.ok();
            }
            Completion::Received {
                tag,
                buffer,
                result,
            } => {
                let Some(r) = self.replica(Connection(split(tag).1)) else {
                    return; // Closed already.
                };
                self.posted[r] = false;
                match result {
                    Ok(len) => self.receive(io, r, &buffer, len, checker),
                    Err(_) => {
                        let connection = self.connections[r].take().expect("open");
                        self.sending[r] = false;
                        io.close(connection);
                    }
                }
            }
            Completion::Sent { tag, .. } => {
                if let Some(r) = self.replica(Connection(split(tag).1)) {
                    self.sending[r] = false;
                }
                // The next tick resends: a send across a partition completes
                // at once, and resending here would spin with the clock stopped.
                return;
            }
            _ => panic!("the client only uses the network"),
        }
        self.tick(io, pool);
    }
}

enum Process {
    Replica(Replica),
    Client(Client),
}

impl Process {
    fn start(node: usize, io: &mut Node, pool: &MessagePool, config: &Config) -> Self {
        if node == CLIENT {
            Process::Client(Client::start(io, pool))
        } else {
            Process::Replica(Replica::start(io, pool, config.skip_durable_write))
        }
    }

    fn tick(&mut self, io: &mut Node, pool: &MessagePool) {
        match self {
            Process::Replica(replica) => replica.tick(io, pool),
            Process::Client(client) => client.tick(io, pool),
        }
    }

    fn complete(
        &mut self,
        io: &mut Node,
        pool: &MessagePool,
        completion: Completion,
        checker: &mut Checker,
    ) {
        match self {
            Process::Replica(replica) => replica.complete(io, pool, completion),
            Process::Client(client) => client.complete(io, pool, completion, checker),
        }
    }
}

#[derive(Debug, Hash)]
enum Event {
    Tick,
    Wake,
}

/// Runs the counter from `seed` until the client has completed
/// `config.ops` operations, a check fails, or virtual time runs out.
pub fn run(seed: u64, config: &Config, pool: &MessagePool) -> Outcome {
    run_logged(seed, config, pool, &mut Log::new(0))
}

/// [`run`], reporting completions, restarts, faults and operations to `log`.
pub fn run_logged(seed: u64, config: &Config, pool: &MessagePool, log: &mut Log) -> Outcome {
    let mut sim = Scheduler::new(seed, 64);
    let group = Group::new(REPLICAS, QUORUM, ZONES);
    let alone = Group::new(1, 1, ZONES);
    let fabric = Fabric::new(config.network, Latency::DATA_CENTER, LOG, &mut sim);
    let mut nodes: Vec<Node> = (0..NODES)
        .map(|i| {
            let (group, replica) = if i == CLIENT {
                (&alone, 0)
            } else {
                (&group, i)
            };
            let disk = Disk::new(group, replica, DISK_CAPACITY, config.storage, &mut sim);
            let nic = Nic::new(&fabric, Peer(i as u64), NIC_CAPACITY, &mut sim);
            Node::with_faults(disk, nic, config.process[i], &mut sim)
        })
        .collect();
    let mut processes: Vec<Process> = (0..NODES)
        .map(|i| Process::start(i, &mut nodes[i], pool, config))
        .collect();
    let mut checker = Checker::default();
    let mut outcome = Outcome::default();
    let mut network_faults = Vec::with_capacity(LOG);
    let mut wake = None;
    sim.schedule(TICK, Event::Tick).expect("room");
    // A process that crashed is dropped, with its memory, and a new one
    // starts from what the disk holds; its start may crash too.
    let settle = |i: usize, nodes: &mut [Node], processes: &mut [Process], log: &mut Log| {
        while nodes[i].down() {
            log.push(nodes[i].monotonic(), LogEvent::Restart { node: i });
            nodes[i].restart();
            processes[i] = Process::start(i, &mut nodes[i], pool, config);
        }
    };
    loop {
        let now = sim.now();
        if checker.violation.is_some() || checker.ops >= config.ops || now >= LIMIT {
            break;
        }
        // Hand every due completion over until none is left.
        let mut busy = true;
        while busy {
            busy = false;
            for i in 0..NODES {
                loop {
                    let completion = nodes[i].poll();
                    // A crash at the completion leaves the node down, and
                    // may have failed operations of nodes already drained.
                    busy |= nodes[i].down();
                    settle(i, &mut nodes, &mut processes, log);
                    let Some(completion) = completion else {
                        break;
                    };
                    busy = true;
                    let op = node::op(&completion);
                    log.push(now, LogEvent::Completion { node: i, op });
                    let ops = checker.ops;
                    processes[i].complete(&mut nodes[i], pool, completion, &mut checker);
                    if checker.ops > ops {
                        log.push(now, LogEvent::Note("operation returned", checker.last));
                    }
                    settle(i, &mut nodes, &mut processes, log);
                }
            }
        }
        for (i, node) in nodes.iter_mut().enumerate() {
            for fault in node.disk.faults() {
                outcome.storage_faults += 1;
                log.push(now, LogEvent::Storage { node: i, fault });
            }
        }
        fabric.faults(&mut network_faults);
        outcome.network_faults += network_faults.len() as u64;
        for fault in network_faults.drain(..) {
            log.push(now, LogEvent::Network(fault));
        }
        let next = nodes.iter().filter_map(Node::ready_at).min();
        if let Some(at) = next {
            assert!(at > now, "every due completion was handed over");
            if wake.is_none_or(|w| at < w) {
                sim.schedule(at, Event::Wake).expect("room");
                wake = Some(at);
            }
        }
        match sim.step().expect("the tick is always scheduled") {
            Event::Tick => {
                fabric.tick();
                for i in 0..NODES {
                    if nodes[i].running() {
                        processes[i].tick(&mut nodes[i], pool);
                        settle(i, &mut nodes, &mut processes, log);
                    }
                }
                sim.schedule(sim.now() + TICK, Event::Tick).expect("room");
            }
            Event::Wake => {
                if wake == Some(sim.now()) {
                    wake = None;
                }
            }
        }
    }
    outcome.trace = sim.trace();
    outcome.ops = checker.ops;
    outcome.violation = checker.violation;
    for node in &nodes {
        for (&site, visits) in node.sites() {
            let total = outcome.sites.entry(site).or_default();
            total.reached += visits.reached;
            total.crashed += visits.crashed;
        }
    }
    outcome
}

/// Swarm testing (document 06 section 1): each seed turns each storage,
/// network and process fault on or off, at the rates of the seed gate of
/// T0.4.4, and picks how many operations the client runs. A function of the
/// seed only, drawn from its own generator so that the run's is untouched.
pub fn swarm(seed: u64) -> Config {
    let mut random = SplitMix64(seed ^ 0x5357_4152_4d00_0000);
    let mut on = |rate: u32| if random.next_u64() & 1 == 0 { 0 } else { rate };
    let storage = storage::Faults {
        torn_writes: on(1) == 1,
        bit_flip: on(5_000),
        misdirected_write: on(5_000),
        lost_write: on(5_000),
        misdirected_read: on(5_000),
        read_error: on(50_000),
        latency: on(100_000),
        latency_max: MILLISECOND,
    };
    let network = network::Faults {
        reset: on(2_000),
        drop: on(20_000),
        delay: on(20_000),
        delay_max: 5 * MILLISECOND,
        duplicate: on(20_000),
        reorder: on(20_000),
        symmetric_partition: on(2_000),
        asymmetric_partition: on(2_000),
        heal: 50_000,
    };
    let process = node::Faults {
        crash: on(10_000),
        pause: on(1_000),
        pause_max: 20 * MILLISECOND,
        slow: 0,
    };
    let mut process = [process; NODES];
    let slow = u64::from(on(200_000));
    process[(random.next_u64() % NODES as u64) as usize].slow = slow;
    Config {
        ops: 10 + random.next_u64() % 41,
        storage,
        network,
        process,
        skip_durable_write: false,
    }
}

const CHECKER: &str = "counter: never goes back, never loses an acknowledged increment";

fn check(seed: u64, config: &Config, log: &mut Log, pool: &MessagePool) -> Result<(), Failure> {
    match run_logged(seed, config, pool, log).violation {
        Some(message) => Err(Failure {
            checker: CHECKER,
            message,
        }),
        None => Ok(()),
    }
}

/// The counter under its swarm configuration.
pub const SCENARIO: Scenario = Scenario {
    name: "counter",
    pool: POOL,
    swarm: |seed| format!("{:?}", swarm(seed)),
    run: |seed, log, pool| check(seed, &swarm(seed), log, pool),
};

/// Injected bug: the counter whose replicas acknowledge a write before it is
/// durable. Fails on some seeds; the runner's own failing scenario.
pub const LOST_WRITE: Scenario = Scenario {
    name: "counter-lost-write",
    pool: POOL,
    swarm: |seed| format!("{:?}", lost_write(seed)),
    run: |seed, log, pool| check(seed, &lost_write(seed), log, pool),
};

fn lost_write(seed: u64) -> Config {
    Config {
        skip_durable_write: true,
        ..swarm(seed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sector_round_trips_and_a_flipped_bit_reads_as_zero() {
        let mut bytes = vec![0; SECTOR_SIZE];
        encode(&mut bytes, 42);
        assert_eq!(decode(&bytes), 42);
        bytes[100] ^= 1;
        assert_eq!(decode(&bytes), 0);
        assert_eq!(decode(&[0; SECTOR_SIZE]), 0);
    }
}
