//! The gates of T0.4.2: each network fault occurs only when enabled, a
//! fault-free network delivers 1,000,000 messages exactly once and in order,
//! an asymmetric partition blocks one direction only, the measured fault
//! rates match the configured chances, the faults are a function of the
//! seed, and a simulated node passes the conformance suite.
//!
//! The workload: two nodes, one connection, each node streaming numbered
//! messages to the other with a window of sends in flight and of receives
//! posted. A node that sees an error waits for its operations on the
//! connection, closes it and opens a new one. Every completion and every
//! fault goes through the scheduler, so it is in the trace.

use granite_base::constants::FORMAT;
use granite_io::conformance::seal;
use granite_io::message_pool::{MessagePool, SECTOR_SIZE};
use granite_io::{Completion, Connection, Network, NetworkError, Peer, Tag, Time};
use granite_sim::network::{Fabric, Fault, FaultKind, Faults, Latency, Nic};
use granite_sim::storage::{self, MILLION};
use granite_sim::{Clock, Disk, Group, Node, Scheduler};

const MILLISECOND: u64 = 1_000_000;
const TICK: u64 = MILLISECOND;
const WINDOW: u32 = 8;
/// A window of sends, one of receives and an accept or connect.
const CAPACITY: u32 = 2 * WINDOW + 1;
const LOG: usize = 1_000;
/// Where a message's body starts: after its header.
const BODY: usize = FORMAT.header_size as usize;
/// A message: a header, then its number and the virtual time it was sent.
const LEN: u32 = BODY as u32 + 16;
const SEND: Tag = 1;
const OPEN: Tag = 2;
/// A receive's tag is this plus the index of its post on the node.
const RECV: Tag = 3;

#[derive(Debug, Hash)]
enum Event {
    Tick,
    Wake,
    Done { node: usize, tag: Tag, ok: bool },
    Fault(Fault),
}

/// One node of the workload, and what it received.
struct Side {
    nic: Nic,
    connection: Option<Connection>,
    opening: bool,
    /// An operation on the connection failed; close it once none is left.
    broken: bool,
    sending: u32,
    posted: u32,
    /// The number of the next message to send.
    next: u64,
    /// By post, the number of the message received into it, if any:
    /// posted buffers fill in the order they were posted.
    posts: Vec<Option<u64>>,
    stats: Received,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Received {
    messages: u64,
    duplicates: u64,
    /// Received after a higher number.
    out_of_order: u64,
    /// Received later than the latency model allows.
    late: u64,
    /// The latest send time of a received message.
    last_sent_at: u64,
    /// Operations that failed with `Reset`.
    resets: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    trace: u64,
    faults: Vec<Fault>,
    /// By receiving node.
    received: [Received; 2],
    /// Messages the fabric put on the wire.
    messages: u64,
}

struct Run<'a> {
    sim: Scheduler<Event>,
    clock: Clock,
    fabric: Fabric,
    sides: [Side; 2],
    per_link: u64,
    latency: Latency,
    pool: &'a MessagePool,
    log: Vec<Fault>,
    faults: Vec<Fault>,
    wake: Option<u64>,
}

/// Runs the workload until each node has sent `per_link` messages and the
/// network has nothing more to deliver.
fn scenario(
    seed: u64,
    faults: Faults,
    latency: Latency,
    per_link: u64,
    pool: &MessagePool,
) -> Outcome {
    let mut sim = Scheduler::new(seed, 200);
    let fabric = Fabric::new(faults, latency, LOG, &mut sim);
    let sides = [0, 1].map(|peer| Side {
        nic: Nic::new(&fabric, Peer(peer), CAPACITY, &mut sim),
        connection: None,
        opening: false,
        broken: false,
        sending: 0,
        posted: 0,
        next: 0,
        posts: Vec::new(),
        stats: Received::default(),
    });
    let mut run = Run {
        clock: sim.clock(),
        sim,
        fabric,
        sides,
        per_link,
        latency,
        pool,
        log: Vec::with_capacity(LOG),
        faults: Vec::new(),
        wake: None,
    };
    run.sim.schedule(TICK, Event::Tick).unwrap();
    run.act();
    while let Some(event) = run.sim.step() {
        match event {
            Event::Tick => {
                run.fabric.tick();
                if !run.finished() {
                    run.sim.schedule(run.now() + TICK, Event::Tick).unwrap();
                }
            }
            Event::Wake => run.wake = None,
            Event::Done { .. } | Event::Fault(_) => continue,
        }
        run.act();
    }
    Outcome {
        trace: run.sim.trace(),
        faults: run.faults,
        received: run.sides.map(|side| side.stats(per_link)),
        messages: run.fabric.messages(),
    }
}

impl Side {
    /// The received counts, with duplicates and messages out of order found
    /// by walking the posts in order.
    fn stats(&self, per_link: u64) -> Received {
        let mut stats = self.stats;
        let mut seen = vec![false; usize::try_from(per_link).unwrap()];
        let mut highest = None;
        for &number in self.posts.iter().flatten() {
            let seen = &mut seen[usize::try_from(number).unwrap()];
            if *seen {
                stats.duplicates += 1;
            } else if highest.is_some_and(|highest| number < highest) {
                stats.out_of_order += 1;
            }
            *seen = true;
            highest = highest.max(Some(number));
        }
        stats
    }
}

impl Run<'_> {
    fn now(&self) -> u64 {
        self.clock.monotonic()
    }

    fn finished(&self) -> bool {
        self.sides.iter().all(|side| side.next == self.per_link)
    }

    /// One round: submit what each node can, take every due completion and
    /// log every fault as an event, and wake up when there is more to do.
    fn act(&mut self) {
        let now = self.now();
        for node in 0..2 {
            self.submit(node);
        }
        let mut progress = false;
        for node in 0..2 {
            while let Some(completion) = self.sides[node].nic.poll() {
                progress = true;
                let event = self.handle(node, completion);
                self.sim.schedule(now, event).unwrap();
            }
        }
        self.fabric.faults(&mut self.log);
        for fault in self.log.drain(..) {
            self.faults.push(fault);
            self.sim.schedule(now, Event::Fault(fault)).unwrap();
        }
        let at = if progress {
            Some(now)
        } else {
            self.fabric.ready_at()
        };
        if let Some(at) = at.filter(|&at| self.wake.is_none_or(|wake| at < wake)) {
            self.sim.schedule(at, Event::Wake).unwrap();
            self.wake = Some(at);
        }
    }

    fn submit(&mut self, node: usize) {
        let now = self.now();
        let finished = self.finished();
        let side = &mut self.sides[node];
        match side.connection {
            None if !side.opening && !finished => {
                side.opening = true;
                let opened = if node == 0 {
                    side.nic.accept(OPEN)
                } else {
                    side.nic.connect(OPEN, Peer(0))
                };
                opened.expect("room");
            }
            Some(connection) if !side.broken => {
                while side.posted < WINDOW {
                    let buffer = self.pool.acquire().expect("pool has room");
                    let tag = RECV + side.posts.len() as u64;
                    side.nic.recv(tag, connection, buffer).expect("room");
                    side.posts.push(None);
                    side.posted += 1;
                }
                while side.sending < WINDOW && side.next < self.per_link {
                    let mut buffer = self.pool.acquire().expect("pool has room");
                    let bytes = buffer.bytes_mut().expect("only handle");
                    bytes[BODY..BODY + 8].copy_from_slice(&side.next.to_le_bytes());
                    bytes[BODY + 8..BODY + 16].copy_from_slice(&now.to_le_bytes());
                    seal(bytes, LEN, side.next);
                    side.nic.send(SEND, connection, LEN, buffer).expect("room");
                    side.sending += 1;
                    side.next += 1;
                }
            }
            Some(connection) if side.sending == 0 && side.posted == 0 => {
                side.nic.close(connection);
                side.connection = None;
                side.broken = false;
            }
            _ => {}
        }
    }

    fn handle(&mut self, node: usize, completion: Completion) -> Event {
        let now = self.now();
        let max = self.latency.max;
        let side = &mut self.sides[node];
        let tag = completion.tag();
        let failed = |error: NetworkError, side: &mut Side| {
            side.broken = true;
            if error == NetworkError::Reset {
                side.stats.resets += 1;
            }
        };
        let ok = match completion {
            Completion::Accepted { result, .. } | Completion::Connected { result, .. } => {
                side.opening = false;
                side.connection = result.ok();
                result.is_ok()
            }
            Completion::Sent { result, .. } => {
                side.sending -= 1;
                if let Err(error) = result {
                    failed(error, side);
                }
                result.is_ok()
            }
            Completion::Received { buffer, result, .. } => {
                side.posted -= 1;
                match result {
                    Ok(len) => {
                        assert_eq!(len, LEN);
                        let bytes = buffer.bytes();
                        let number = u64::from_le_bytes(bytes[BODY..BODY + 8].try_into().unwrap());
                        let sent_at =
                            u64::from_le_bytes(bytes[BODY + 8..BODY + 16].try_into().unwrap());
                        side.posts[(tag - RECV) as usize] = Some(number);
                        let stats = &mut side.stats;
                        stats.messages += 1;
                        stats.late += u64::from(now - sent_at > max);
                        stats.last_sent_at = stats.last_sent_at.max(sent_at);
                    }
                    Err(error) => failed(error, side),
                }
                result.is_ok()
            }
            other => panic!("unexpected completion {other:?}"),
        };
        Event::Done { node, tag, ok }
    }
}

fn pool() -> MessagePool {
    MessagePool::new(4 * WINDOW)
}

/// Heals come quickly, so partitioned runs still finish.
fn none() -> Faults {
    Faults {
        delay_max: MILLISECOND,
        heal: MILLION / 4,
        ..Faults::default()
    }
}

/// Faults 0 to 6, in the order of [`FaultKind`], switched on at a high rate
/// or off.
fn with(mut faults: Faults, kind: usize, on: bool) -> Faults {
    let chance = |share: u32| if on { share } else { 0 };
    match kind {
        0 => faults.reset = chance(MILLION / 20),
        1 => faults.drop = chance(MILLION / 5),
        2 => faults.delay = chance(MILLION / 5),
        3 => faults.duplicate = chance(MILLION / 5),
        4 => faults.reorder = chance(MILLION / 5),
        5 => faults.symmetric_partition = chance(MILLION / 10),
        6 => faults.asymmetric_partition = chance(MILLION / 10),
        _ => unreachable!(),
    }
    faults
}

fn all() -> Faults {
    (0..7).fold(none(), |faults, kind| with(faults, kind, true))
}

/// The index of a fault in [`with`]; `None` for a heal.
fn kind(fault: &Fault) -> Option<usize> {
    match fault.kind {
        FaultKind::Reset => Some(0),
        FaultKind::Drop => Some(1),
        FaultKind::Delay { .. } => Some(2),
        FaultKind::Duplicate => Some(3),
        FaultKind::Reorder => Some(4),
        FaultKind::SymmetricPartition => Some(5),
        FaultKind::AsymmetricPartition => Some(6),
        FaultKind::Heal => None,
    }
}

fn total(outcome: &Outcome, field: fn(&Received) -> u64) -> u64 {
    outcome.received.iter().map(field).sum()
}

#[test]
fn each_fault_occurs_only_when_enabled() {
    let pool = pool();
    let per_link = 2_000;
    for k in 0..7 {
        let on = scenario(
            k as u64,
            with(none(), k, true),
            Latency::DATA_CENTER,
            per_link,
            &pool,
        );
        assert!(
            on.faults.iter().any(|f| kind(f) == Some(k)),
            "fault {k} occurs"
        );
        let off = scenario(
            k as u64,
            with(all(), k, false),
            Latency::DATA_CENTER,
            per_link,
            &pool,
        );
        assert!(
            off.faults.iter().all(|f| kind(f) != Some(k)),
            "fault {k} never occurs"
        );
        assert!(
            off.faults.iter().any(|f| kind(f).is_some()),
            "the others do"
        );
        // The effect a receiver sees. Resets, duplicates and messages out
        // of order come only from their fault, so they are checked both ways.
        let delivered = total(&on, |r| r.messages - r.duplicates);
        let unique: Option<fn(&Received) -> u64> = match k {
            0 => Some(|r| r.resets),
            1 | 5 | 6 => {
                assert!(delivered < 2 * per_link, "fault {k} loses messages");
                None
            }
            2 => {
                assert!(total(&on, |r| r.late) > 0, "delay makes messages late");
                None
            }
            3 => Some(|r| r.duplicates),
            _ => Some(|r| r.out_of_order),
        };
        if let Some(effect) = unique {
            assert!(total(&on, effect) > 0, "fault {k} is seen");
            assert_eq!(total(&off, effect), 0, "fault {k} is not seen");
        }
    }
}

#[test]
fn without_faults_a_million_messages_arrive_once_and_in_order() {
    let per_link = 500_000;
    let out = scenario(
        1,
        Faults::default(),
        Latency::DATA_CENTER,
        per_link,
        &pool(),
    );
    assert_eq!(out.messages, 2 * per_link);
    assert!(out.faults.is_empty());
    for received in out.received {
        assert_eq!(
            (
                received.messages,
                received.duplicates,
                received.out_of_order,
                received.resets
            ),
            (per_link, 0, 0, 0)
        );
    }
}

#[test]
fn an_asymmetric_partition_blocks_one_direction() {
    let pool = pool();
    let per_link = 10_000;
    let faults = Faults {
        asymmetric_partition: MILLION,
        ..Faults::default()
    };
    for seed in 0..8 {
        let out = scenario(seed, faults, Latency::DATA_CENTER, per_link, &pool);
        // The first tick partitions the two nodes, after they connected.
        let [
            Fault {
                at,
                kind: FaultKind::AsymmetricPartition,
                from,
                to,
            },
        ] = out.faults[..]
        else {
            panic!("one asymmetric partition: {:?}", out.faults);
        };
        assert_eq!(at, TICK);
        let (b, a) = (from.0 as usize, to.0 as usize);
        // From A to B: every message, also those sent after the partition.
        assert_eq!(out.received[b].messages, per_link);
        assert!(out.received[b].last_sent_at >= at);
        // From B to A: nothing sent after the partition.
        assert!(out.received[a].last_sent_at < at);
        assert!(out.received[a].messages < per_link);
    }
}

#[test]
fn measured_fault_rates_match_their_chances() {
    let faults = Faults {
        reset: MILLION / 100,
        drop: MILLION / 20,
        delay: MILLION / 20,
        delay_max: MILLISECOND,
        duplicate: MILLION / 20,
        reorder: MILLION / 20,
        ..Faults::default()
    };
    let out = scenario(2, faults, Latency::DATA_CENTER, 520_000, &pool());
    let messages = out.messages;
    assert!(messages >= 1_000_000, "{messages} messages");
    for (k, chance) in [
        (0, faults.reset),
        (1, faults.drop),
        (2, faults.delay),
        (3, faults.duplicate),
        (4, faults.reorder),
    ] {
        let count = out.faults.iter().filter(|f| kind(f) == Some(k)).count() as u64;
        // Within 1% absolute, in millionths.
        let measured = count * u64::from(MILLION) / messages;
        eprintln!("fault {k}: configured {chance}, measured {measured} per million");
        assert!(
            measured.abs_diff(u64::from(chance)) < u64::from(MILLION / 100),
            "fault {k}"
        );
    }
}

/// Swarm testing: each fault on or off by the seed.
fn swarm(seed: u64) -> Faults {
    let mut random = granite_io::memory::SplitMix64(seed);
    (0..7).fold(none(), |faults, k| {
        with(faults, k, random.next_u64() & 1 == 1)
    })
}

#[test]
fn faults_are_a_function_of_the_seed() {
    let pool = pool();
    let mut last = None;
    let mut seen = [false; 7];
    for seed in 0..1_000 {
        let a = scenario(seed, swarm(seed), Latency::DATA_CENTER, 100, &pool);
        let b = scenario(seed, swarm(seed), Latency::DATA_CENTER, 100, &pool);
        assert_eq!(a, b, "seed {seed}");
        assert_ne!(Some(a.trace), last, "seed {seed} changes the trace");
        last = Some(a.trace);
        for k in a.faults.iter().filter_map(kind) {
            seen[k] = true;
        }
    }
    assert_eq!(seen, [true; 7], "every fault ran");
}

#[test]
fn node_passes_the_conformance_suite() {
    let sector = SECTOR_SIZE as u64;
    let zones = [
        4 * sector,
        4 * sector,
        8 * sector,
        2 * FORMAT.message_size_max,
    ];
    // Several seeds, so the suite holds for several completion orders.
    for seed in 0..32 {
        let mut sim = Scheduler::<()>::new(seed, 1);
        let fabric = Fabric::new(Faults::default(), Latency::ZERO, 1, &mut sim);
        let group = Group::new(2, 2, zones);
        let nodes = [0, 1].map(|i| {
            let disk = Disk::new(&group, i, 4, storage::Faults::default(), &mut sim);
            let nic = Nic::new(&fabric, Peer(i as u64), 8, &mut sim);
            (Node::new(disk, nic, &mut sim), Peer(i as u64))
        });
        granite_io::conformance::run(nodes);
    }
}

/// Sends are held to the rule io_uring needs for framing, so code tested in
/// the simulator cannot send bytes a real connection would misframe.
#[test]
#[should_panic(expected = "a send is one whole message")]
fn a_send_of_raw_bytes_is_refused() {
    let mut sim = Scheduler::<()>::new(0, 1);
    let fabric = Fabric::new(Faults::default(), Latency::ZERO, 1, &mut sim);
    let mut nics = [0, 1].map(|i| (Nic::new(&fabric, Peer(i), 8, &mut sim), Peer(i)));
    let (_, b) = granite_io::conformance::connect(&mut nics, &mut Nic::poll);
    let pool = MessagePool::new(1);
    let _ = nics[1].0.send(SEND, b, LEN, pool.acquire().unwrap());
}
