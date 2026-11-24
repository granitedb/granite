//! `granite_vsr` replicas under the simulator: one group, each replica
//! driven by its own shard event loop on a simulated node, with a state
//! machine that records what it executes. A load generator hands bursts of
//! requests straight to the primary, replica 0, retrying those refused as
//! busy, until [`REQUESTS`] are taken.
//!
//! Checked on every run:
//! - Each replica executes ops exactly once and in op order (an assertion
//!   in [`Recorder`]), and the ops every two replicas executed agree: one
//!   replica's list is a prefix of the other's (invariants 1 and 3, the log
//!   part).
//! - A replica that crashes stays down. Its WAL is then recovered from its
//!   disk, and must hold every op the replica acknowledged with prepare_ok
//!   or counted as its own durable write (invariant 6).
//!
//! With no fault, also: every replica executes every request, stays in
//! status normal, and hears the primary often enough that no suspicion
//! timeout fires. Every deadline a timeout is armed with, and every
//! firing, is folded into [`Outcome::firings`], which two runs of a seed
//! compare.

use granite_base::checksum::checksum;
use granite_base::constants::FORMAT;
use granite_base::header::{Command, Header, Operation, VERSION};
use granite_io::conformance::seal;
use granite_io::event_loop::EventLoop;
use granite_io::memory::SplitMix64;
use granite_io::message_pool::MessagePool;
use granite_io::{Peer, Zone};
use granite_storage::layout::zone_sizes;
use granite_storage::wal::recovery::Recovery;
use granite_vsr::{Config, Replica, StateMachine, Status};

use crate::network::{self, Fabric, Latency, Nic};
use crate::node;
use crate::runner::{Event as LogEvent, Failure, Log, Scenario};
use crate::storage::{self, Disk, Group};
use crate::{Node, Scheduler};

const MILLISECOND: u64 = 1_000_000;
const TICK: u64 = MILLISECOND;
/// Virtual time each group runs: a hundred heartbeats, and more than ten
/// suspicion timeouts.
const DURATION: u64 = 1_000 * MILLISECOND;
/// Nanoseconds per event-loop iteration, by class; 1 ms in all.
const BUDGETS: [u64; 6] = [400_000, 200_000, 100_000, 100_000, 150_000, 50_000];
const COMPLETIONS: u32 = 16;
const NIC_CAPACITY: u32 = 64;
const DISK_CAPACITY: usize = 16;
/// Requests the load generator hands the primary in a run.
pub const REQUESTS: u64 = 200;
/// Most requests in one burst: more than the pipeline and the queue hold.
const BURST_MAX: u64 = 24;
const LARGEST_BODY: u64 = 64;
/// Most buffers a group of 5 with a full pipeline was seen to hold at once:
/// posted receives, WAL header buffers, the prepares each replica holds,
/// queued requests, and acknowledgements in flight.
const POOL_PEAK: u32 = 127;
/// Buffers of the scenario's pool: twice the peak, rounded up.
pub const POOL: u32 = 2 * (POOL_PEAK + 1);

/// What a run injects.
#[derive(Clone, Copy, Debug, Default)]
pub struct Faults {
    pub network: network::Faults,
    /// Chance, in millionths, that a node crashes at an I/O boundary.
    pub crash: u32,
    /// Replicas, a bit each, whose process never runs: they receive and
    /// send nothing, as if partitioned from the start.
    pub cut_off: u8,
}

impl Faults {
    pub const NONE: Faults = Faults {
        network: network::Faults {
            reset: 0,
            drop: 0,
            delay: 0,
            delay_max: 0,
            duplicate: 0,
            reorder: 0,
            symmetric_partition: 0,
            asymmetric_partition: 0,
            heal: 0,
        },
        crash: 0,
        cut_off: 0,
    };

    /// Message drop, delay, reordering and duplication.
    pub const MESSAGES: Faults = Faults {
        network: network::Faults {
            reset: 0,
            drop: 20_000,
            delay: 50_000,
            delay_max: 5 * MILLISECOND,
            duplicate: 20_000,
            reorder: 20_000,
            symmetric_partition: 0,
            asymmetric_partition: 0,
            heal: 0,
        },
        crash: 0,
        cut_off: 0,
    };

    /// Process crashes at I/O boundaries, and nothing else.
    pub const CRASHES: Faults = Faults {
        crash: 300,
        ..Faults::NONE
    };

    fn none(&self) -> bool {
        let n = self.network;
        let message = n.reset | n.drop | n.delay | n.duplicate | n.reorder;
        message
            | n.symmetric_partition
            | n.asymmetric_partition
            | self.crash
            | u32::from(self.cut_off)
            == 0
    }
}

/// Records a digest of each op it executes, and replies with the op.
#[derive(Default)]
pub struct Recorder {
    pub digests: Vec<u128>,
}

impl StateMachine for Recorder {
    fn execute(
        &mut self,
        op: u64,
        timestamp: u64,
        operation: Operation,
        body: &[u8],
        reply: &mut [u8],
    ) -> u32 {
        assert_eq!(
            op,
            self.digests.len() as u64 + 1,
            "ops are executed exactly once, in op order"
        );
        let fields = [
            op.to_le_bytes(),
            timestamp.to_le_bytes(),
            [operation as u8; 8],
        ];
        self.digests
            .push(checksum(&[fields.concat().as_slice(), body].concat()));
        reply[..8].copy_from_slice(&op.to_le_bytes());
        8
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The scheduler's trace.
    pub trace: u64,
    /// A hash of every deadline each timeout was armed with, and of each
    /// firing, by replica and timeout.
    pub firings: u64,
    /// Firings of each replica's heartbeat and suspicion timeouts.
    pub fired: Vec<[u64; 2]>,
    /// Commits each replica dispatched.
    pub commits: Vec<u64>,
    /// The first replica seen out of status normal, and when.
    pub left_normal: Option<(usize, u64)>,
    /// The digests of the ops each replica executed, in op order.
    pub executed: Vec<Vec<u128>>,
    /// Requests the primary took.
    pub requests: u64,
    /// Requests the primary refused as busy.
    pub busy: u64,
    /// Most prepares the primary held at once.
    pub pipeline_max: usize,
    /// Prepares the backups did not write because they did not extend the
    /// chain.
    pub gaps: u64,
    /// Replicas that crashed.
    pub crashed: Vec<usize>,
    /// The first crashed replica whose disk lacks an op it acknowledged.
    pub lost: Option<String>,
}

#[derive(Debug, Hash)]
enum Event {
    Tick,
    Wake,
}

/// The zones of a data file, with the grid cut to one sector: the replica
/// writes only its WAL.
fn zones() -> [u64; 4] {
    let mut zones = zone_sizes(FORMAT.grid_growth_step);
    zones[Zone::Grid as usize] = FORMAT.sector_size;
    zones
}

/// A request of `body` from client 1, number `number`.
fn request(cluster: u128, group: u32, number: u32, body: &[u8]) -> Header {
    // A valid header to start from, whose fields are then set.
    let mut bytes = [0; FORMAT.header_size as usize];
    seal(&mut bytes, FORMAT.header_size as u32, 0);
    let mut header = *Header::decode(&bytes).expect("sealed");
    header.op = 0.into();
    header.cluster = cluster.into();
    header.group = group.into();
    header.client = 1.into();
    header.request = number.into();
    header.command = Command::Request as u8;
    header.operation = Operation::Insert as u8;
    header.version = VERSION;
    header.seal(body);
    header
}

/// The earliest time a node that runs has a completion due.
fn ready(nodes: &[Node], faults: Faults) -> Option<u64> {
    let running = |i: &usize| faults.cut_off & 1 << i == 0;
    (0..nodes.len())
        .filter(running)
        .filter_map(|i| nodes[i].ready_at())
        .min()
}

/// Recovers the WAL on the disk of a crashed node and returns its head.
fn recovered_head(disk: &mut Disk, pool: &MessagePool) -> u64 {
    let mut recovery = Recovery::new(0);
    for _ in 0..1_000_000 {
        if recovery.done() {
            return recovery.head();
        }
        recovery.pump(disk, pool);
        while let Some(completion) = disk.poll() {
            recovery.complete(disk, pool, completion);
        }
    }
    panic!("recovery of a crashed replica's WAL does not finish");
}

/// Runs a group of `replicas` replicas of `cluster` and `group` from `seed`
/// for [`DURATION`] of virtual time, injecting `faults`.
pub fn run(
    seed: u64,
    replicas: u8,
    cluster: u128,
    group: u32,
    faults: Faults,
    pool: &MessagePool,
    log: &mut Log,
) -> Outcome {
    let n = usize::from(replicas);
    let mut sim = Scheduler::new(seed, 64);
    let storage = Group::new(n, n / 2 + 1, zones());
    let fabric = Fabric::new(faults.network, Latency::DATA_CENTER, 64, &mut sim);
    let process = node::Faults {
        crash: faults.crash,
        ..node::Faults::default()
    };
    let mut nodes: Vec<Node> = (0..n)
        .map(|i| {
            let disk = Disk::new(
                &storage,
                i,
                DISK_CAPACITY,
                storage::Faults::default(),
                &mut sim,
            );
            let nic = Nic::new(&fabric, Peer(i as u64), NIC_CAPACITY, &mut sim);
            Node::with_faults(disk, nic, process, &mut sim)
        })
        .collect();
    let mut loops: Vec<EventLoop> = (0..n)
        .map(|_| EventLoop::new(BUDGETS, COMPLETIONS, TICK, 0))
        .collect();
    for l in &mut loops {
        l.assert_slices();
    }
    let peers: Vec<Peer> = (0..replicas).map(|i| Peer(u64::from(i))).collect();
    let mut group_replicas: Vec<Option<Replica<'_, Recorder>>> = (0..replicas)
        .map(|replica| {
            let config = Config {
                cluster,
                group,
                replica,
                peers: peers.clone(),
            };
            let node = &mut nodes[usize::from(replica)];
            Some(Replica::new(node, config, Recorder::default(), pool))
        })
        .collect();
    let mut outcome = Outcome {
        fired: vec![[0; 2]; n],
        executed: vec![Vec::new(); n],
        commits: vec![0; n],
        ..Outcome::default()
    };
    let mut firings = SplitMix64(0);
    let mut deadlines = vec![[None; 2]; n];
    let mut load = SplitMix64(seed ^ 0x4c4f_4144);
    let mut body = [0; LARGEST_BODY as usize];
    let mut network_faults = Vec::new();
    let mut wake = None;
    sim.schedule(TICK, Event::Tick).expect("room");
    while sim.now() < DURATION && outcome.left_normal.is_none() {
        let now = sim.now();
        // Iterate every loop, then again while any node has a completion due.
        let mut first = true;
        while first || ready(&nodes, faults).is_some_and(|at| at <= now) {
            first = false;
            for i in 0..n {
                let Some(replica) = group_replicas[i].as_mut() else {
                    continue;
                };
                if faults.cut_off & 1 << i != 0 {
                    continue;
                }
                loops[i].iterate(&mut nodes[i], replica);
                for (t, timeout) in replica.timeouts().into_iter().enumerate() {
                    let fired = timeout.fired() > outcome.fired[i][t];
                    if fired || timeout.deadline() != deadlines[i][t] {
                        let at = timeout.deadline().unwrap_or(u64::MAX);
                        let word = at ^ (fired as u64) << 63 ^ (i as u64) << 56 ^ (t as u64) << 48;
                        firings = SplitMix64(firings.next_u64() ^ word);
                    }
                    deadlines[i][t] = timeout.deadline();
                    while outcome.fired[i][t] < timeout.fired() {
                        outcome.fired[i][t] += 1;
                        log.push(now, LogEvent::Note(["heartbeat", "suspicion"][t], i as u64));
                    }
                }
                if replica.status() != Status::Normal && outcome.left_normal.is_none() {
                    outcome.left_normal = Some((i, now));
                }
                if i == 0 {
                    outcome.pipeline_max = outcome.pipeline_max.max(replica.pipeline());
                }
            }
            for i in 0..n {
                if nodes[i].down()
                    && let Some(replica) = group_replicas[i].take()
                {
                    crashed(i, replica, &mut nodes[i], &mut outcome, pool);
                    log.push(now, LogEvent::Note("crash", i as u64));
                }
            }
        }
        fabric.faults(&mut network_faults);
        for fault in network_faults.drain(..) {
            log.push(now, LogEvent::Network(fault));
        }
        if let Some(at) = ready(&nodes, faults)
            && wake.is_none_or(|w| at < w)
        {
            sim.schedule(at, Event::Wake).expect("room");
            wake = Some(at);
        }
        match sim.step().expect("the tick is always scheduled") {
            Event::Tick => {
                sim.schedule(sim.now() + TICK, Event::Tick).expect("room");
                // A burst of requests, each refused one tried again later.
                let burst = load.next_u64() % (BURST_MAX + 1);
                for _ in 0..burst {
                    let Some(primary) = group_replicas[0].as_mut() else {
                        break;
                    };
                    if outcome.requests == REQUESTS {
                        break;
                    }
                    let len = 1 + load.next_u64() % LARGEST_BODY;
                    let body = &mut body[..len as usize];
                    body.fill(load.next_u64() as u8);
                    let number = u32::try_from(outcome.requests + 1).expect("fits");
                    let header = request(cluster, group, number, body);
                    if primary.request(&mut nodes[0], &header, body).is_err() {
                        break;
                    }
                    outcome.requests += 1;
                }
            }
            Event::Wake => {
                if wake == Some(sim.now()) {
                    wake = None;
                }
            }
        }
    }
    outcome.trace = sim.trace();
    outcome.firings = firings.next_u64();
    for (i, replica) in group_replicas.iter().enumerate() {
        if let Some(replica) = replica {
            take(i, replica, &mut outcome);
        }
    }
    outcome
}

/// Copies what the outcome reports of replica `i`.
fn take(i: usize, replica: &Replica<'_, Recorder>, outcome: &mut Outcome) {
    outcome.executed[i] = replica.state_machine().digests.clone();
    outcome.commits[i] = replica.received(Command::Commit);
    outcome.gaps += replica.gaps();
    if i == 0 {
        outcome.busy = replica.busy();
    }
}

/// Replica `i` crashed: its process is dropped, and its disk must hold
/// every op it acknowledged or counted as durable.
fn crashed(
    i: usize,
    replica: Replica<'_, Recorder>,
    node: &mut Node,
    outcome: &mut Outcome,
    pool: &MessagePool,
) {
    take(i, &replica, outcome);
    outcome.crashed.push(i);
    let claimed = replica.acknowledged().max(replica.durable());
    drop(replica);
    let head = recovered_head(&mut node.disk, pool);
    if head < claimed && outcome.lost.is_none() {
        outcome.lost = Some(format!(
            "replica {i} acknowledged op {claimed}, but its WAL holds ops up to {head} after its crash"
        ));
    }
}

/// The checker's verdict on a run with `faults`: `None` if it holds.
pub fn violation(outcome: &Outcome, faults: Faults) -> Option<String> {
    if let Some(lost) = &outcome.lost {
        return Some(lost.clone());
    }
    let longest = outcome.executed.iter().max_by_key(|e| e.len())?;
    for (r, executed) in outcome.executed.iter().enumerate() {
        if let Some(op) = (0..executed.len()).find(|&k| executed[k] != longest[k]) {
            return Some(format!(
                "replica {r} executed another op {} than another replica",
                op + 1
            ));
        }
    }
    if !faults.none() {
        return None;
    }
    if let Some((replica, at)) = outcome.left_normal {
        return Some(format!("replica {replica} left status normal at {at} ns"));
    }
    let primary = 0;
    for (r, fired) in outcome.fired.iter().enumerate() {
        if fired[1] > 0 {
            return Some(format!(
                "replica {r}'s suspicion timeout fired {} times",
                fired[1]
            ));
        }
        if r != primary && outcome.commits[r] == 0 {
            return Some(format!("replica {r} never heard the primary"));
        }
        let executed = outcome.executed[r].len() as u64;
        if executed != REQUESTS {
            return Some(format!(
                "replica {r} executed {executed} of {REQUESTS} requests"
            ));
        }
    }
    let heartbeats = outcome.fired[primary][0];
    (heartbeats < DURATION / MILLISECOND / 20)
        .then(|| format!("the primary sent only {heartbeats} heartbeats"))
}

/// The cluster and group ids a seed runs with, from their own generator.
pub fn ids(seed: u64) -> (u128, u32) {
    let mut random = SplitMix64(seed ^ 0x5245_504c_4943_4100);
    let cluster = u128::from(random.next_u64()) << 64 | u128::from(random.next_u64());
    (cluster, random.next_u64() as u32)
}

/// The runs of a seed: groups of 3 and 5 with no fault and with message
/// faults, then a group of 3 with crashes.
pub const RUNS: [(u8, Faults, &str); 5] = [
    (3, Faults::NONE, "no faults"),
    (5, Faults::NONE, "no faults"),
    (3, Faults::MESSAGES, "message faults"),
    (5, Faults::MESSAGES, "message faults"),
    (3, Faults::CRASHES, "crashes"),
];

const CHECKER: &str = "replica: normal operation (invariants 1, 3 and 6)";

pub const SCENARIO: Scenario = Scenario {
    name: "replica",
    pool: POOL,
    swarm: |seed| {
        let (cluster, group) = ids(seed);
        format!(
            "cluster {cluster:#x}, group {group}; 3 and 5 replicas with no faults, \
             3 and 5 with message faults, 3 with crashes"
        )
    },
    run: |seed, log, pool| {
        let (cluster, group) = ids(seed);
        for (replicas, faults, name) in RUNS {
            log.push(0, LogEvent::Note("replicas", u64::from(replicas)));
            let outcome = run(seed, replicas, cluster, group, faults, pool, log);
            if let Some(message) = violation(&outcome, faults) {
                return Err(Failure {
                    checker: CHECKER,
                    message: format!("{replicas} replicas, {name}: {message}"),
                });
            }
        }
        Ok(())
    },
};
