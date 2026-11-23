//! The replica frame of T2.2.1 under the simulator: one group of
//! `granite_vsr` replicas with a no-op state machine, each driven by its own
//! shard event loop on a simulated node, with no fault injected.
//!
//! Checked: every replica stays in status normal, no backup's suspicion
//! timeout fires (the primary's heartbeats reach every backup in time), and
//! every backup hears the primary. Every deadline a timeout is armed with,
//! and every firing, is folded into [`Outcome::firings`], which two runs of
//! a seed compare.

use granite_base::header::{Command, Operation};
use granite_io::Peer;
use granite_io::event_loop::EventLoop;
use granite_io::memory::SplitMix64;
use granite_io::message_pool::{MessagePool, SECTOR_SIZE};
use granite_vsr::{Config, Replica, StateMachine, Status};

use crate::network::{self, Fabric, Latency, Nic};
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
const NIC_CAPACITY: u32 = 32;
const ZONES: [u64; 4] = [0, 0, 0, SECTOR_SIZE as u64];

/// Executes nothing and replies with nothing.
pub struct Noop;

impl StateMachine for Noop {
    fn execute(&mut self, _: u64, _: u64, _: Operation, _: &[u8], _: &mut [u8]) -> u32 {
        0
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
}

#[derive(Debug, Hash)]
enum Event {
    Tick,
    Wake,
}

/// Runs a group of `replicas` replicas of `cluster` and `group` from `seed`
/// for [`DURATION`] of virtual time.
pub fn run(
    seed: u64,
    replicas: u8,
    cluster: u128,
    group: u32,
    pool: &MessagePool,
    log: &mut Log,
) -> Outcome {
    let n = usize::from(replicas);
    let mut sim = Scheduler::new(seed, 64);
    let storage = Group::new(n, n / 2 + 1, ZONES);
    let fabric = Fabric::new(
        network::Faults::default(),
        Latency::DATA_CENTER,
        1,
        &mut sim,
    );
    let mut nodes: Vec<Node> = (0..n)
        .map(|i| {
            let disk = Disk::new(&storage, i, 1, storage::Faults::default(), &mut sim);
            let nic = Nic::new(&fabric, Peer(i as u64), NIC_CAPACITY, &mut sim);
            Node::new(disk, nic, &mut sim)
        })
        .collect();
    let mut loops: Vec<EventLoop> = (0..n)
        .map(|_| EventLoop::new(BUDGETS, COMPLETIONS, TICK, 0))
        .collect();
    for l in &mut loops {
        l.assert_slices();
    }
    let peers: Vec<Peer> = (0..replicas).map(|i| Peer(u64::from(i))).collect();
    let mut group_replicas: Vec<Replica<'_, Noop>> = (0..replicas)
        .map(|replica| {
            let config = Config {
                cluster,
                group,
                replica,
                peers: peers.clone(),
            };
            Replica::new(&mut nodes[usize::from(replica)], config, Noop, pool)
        })
        .collect();
    let mut outcome = Outcome {
        fired: vec![[0; 2]; n],
        ..Outcome::default()
    };
    let mut firings = SplitMix64(0);
    let mut deadlines = vec![[None; 2]; n];
    let mut wake = None;
    sim.schedule(TICK, Event::Tick).expect("room");
    while sim.now() < DURATION && outcome.left_normal.is_none() {
        let now = sim.now();
        // Iterate every loop, then again while any node has a completion due.
        let mut first = true;
        while first
            || nodes
                .iter()
                .any(|node| node.ready_at().is_some_and(|at| at <= now))
        {
            first = false;
            for i in 0..n {
                loops[i].iterate(&mut nodes[i], &mut group_replicas[i]);
                for (t, timeout) in group_replicas[i].timeouts().into_iter().enumerate() {
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
                if group_replicas[i].status() != Status::Normal && outcome.left_normal.is_none() {
                    outcome.left_normal = Some((i, now));
                }
            }
        }
        if let Some(at) = nodes.iter().filter_map(Node::ready_at).min()
            && wake.is_none_or(|w| at < w)
        {
            sim.schedule(at, Event::Wake).expect("room");
            wake = Some(at);
        }
        match sim.step().expect("the tick is always scheduled") {
            Event::Tick => {
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
    outcome.firings = firings.next_u64();
    outcome.commits = group_replicas
        .iter()
        .map(|r| r.received(Command::Commit))
        .collect();
    outcome
}

/// The checker's verdict on a run: `None` if it holds.
pub fn violation(outcome: &Outcome) -> Option<String> {
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

const CHECKER: &str = "replica: stays in status normal with no faults";

/// A group of 3, then a group of 5, from the same seed.
pub const SCENARIO: Scenario = Scenario {
    name: "replica",
    swarm: |seed| {
        let (cluster, group) = ids(seed);
        format!("cluster {cluster:#x}, group {group}, 3 then 5 replicas, no faults")
    },
    run: |seed, log, pool| {
        let (cluster, group) = ids(seed);
        for replicas in [3, 5] {
            log.push(0, LogEvent::Note("replicas", u64::from(replicas)));
            if let Some(message) = violation(&run(seed, replicas, cluster, group, pool, log)) {
                return Err(Failure {
                    checker: CHECKER,
                    message: format!("{replicas} replicas: {message}"),
                });
            }
        }
        Ok(())
    },
};
