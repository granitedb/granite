//! The gates of T0.4.4: the toy replicated counter holds over 10,000 seeds
//! with network, storage and process faults, and fails within 1,000 seeds
//! when a replica acknowledges before its write is durable; a crash is
//! injected at every I/O call site of the toy, with a report of the counts;
//! a restarted process sees nothing from before its crash; a paused shard
//! resumes with its state intact and virtual time moved on. The machinery
//! under them follows: a crash at a submission and at a completion, the
//! network side of a crash, a slow shard, and runs that are a function of
//! the seed.

use std::collections::BTreeSet;

use granite_base::constants::FORMAT;
use granite_io::conformance::seal;
use granite_io::message_pool::{MessagePool, SECTOR_SIZE};
use granite_io::{Completion, Connection, Io, Network, NetworkError, Peer, Storage, Time, Zone};
use granite_sim::counter::{self, Config, NODES, Outcome};
use granite_sim::network::{self, Fabric, Latency, Nic};
use granite_sim::node::{self, Op, Site, Visits};
use granite_sim::storage::{self, MILLION};
use granite_sim::{Disk, Group, Node, Scheduler};

const MILLISECOND: u64 = 1_000_000;
/// The seed gate's seeds; a run of `cargo test` checks the first 1,000.
const SEEDS: u64 = 10_000;
const QUICK_SEEDS: u64 = 1_000;
const BUG_SEEDS: u64 = 1_000;
const OPS: u64 = 30;
const LEN: u32 = FORMAT.header_size as u32;

fn pool() -> MessagePool {
    MessagePool::new(48)
}

/// Every fault the simulator has, at rates that leave the counter able to
/// make progress; the slow node is drawn from the seed.
fn faulty(seed: u64) -> Config {
    let storage = storage::Faults {
        torn_writes: true,
        bit_flip: 5_000,
        misdirected_write: 5_000,
        lost_write: 5_000,
        misdirected_read: 5_000,
        read_error: 50_000,
        latency: 100_000,
        latency_max: MILLISECOND,
    };
    let network = network::Faults {
        reset: 2_000,
        drop: 20_000,
        delay: 20_000,
        delay_max: 5 * MILLISECOND,
        duplicate: 20_000,
        reorder: 20_000,
        symmetric_partition: 2_000,
        asymmetric_partition: 2_000,
        heal: 50_000,
    };
    let mut process = [node::Faults {
        crash: 10_000,
        pause: 1_000,
        pause_max: 20 * MILLISECOND,
        slow: 0,
    }; NODES];
    process[(seed % NODES as u64) as usize].slow = 200_000;
    Config {
        ops: OPS,
        storage,
        network,
        process,
        skip_durable_write: false,
    }
}

/// The lines of the toy that submit I/O, one call to a line.
fn call_sites() -> BTreeSet<u32> {
    let ops = [
        "read", "write", "fsync", "connect", "accept", "send", "recv", "close",
    ];
    include_str!("../src/counter.rs")
        .lines()
        .zip(1..)
        .filter(|(line, _)| !line.trim_start().starts_with("//"))
        .filter(|(line, _)| ops.iter().any(|op| line.contains(&format!("io.{op}("))))
        .map(|(_, number)| number)
        .collect()
}

/// The seed gate, with the coverage report of crash sites. The full gate
/// runs with `GRANITE_FULL=1`, in the release profile:
/// `GRANITE_FULL=1 cargo test --release --locked -p granite-sim --test crash seed_gate -- --nocapture`.
#[test]
fn seed_gate_the_counter_never_goes_back_or_loses_an_acknowledged_increment() {
    let pool = pool();
    let seeds = if std::env::var_os("GRANITE_FULL").is_some() {
        SEEDS
    } else {
        QUICK_SEEDS
    };
    let mut total = Outcome::default();
    for seed in 0..seeds {
        let outcome = counter::run(seed, &faulty(seed), &pool);
        if let Some(violation) = outcome.violation {
            panic!("seed {seed}: {violation}");
        }
        total.ops += outcome.ops;
        total.storage_faults += outcome.storage_faults;
        total.network_faults += outcome.network_faults;
        for (site, visits) in outcome.sites {
            let sum = total.sites.entry(site).or_default();
            sum.reached += visits.reached;
            sum.crashed += visits.crashed;
        }
    }
    // The coverage report of crash sites.
    println!("{:<44} {:>12} {:>8}", "site", "reached", "crashed");
    for (site, Visits { reached, crashed }) in &total.sites {
        let name = match site {
            Site::Submit(op, at) => format!("submit {op:?} at {}:{}", at.file(), at.line()),
            Site::Complete(op) => format!("complete {op:?}"),
        };
        println!("{name:<44} {reached:>12} {crashed:>8}");
    }
    let crashes: u64 = total.sites.values().map(|v| v.crashed).sum();
    println!(
        "{seeds} seeds: {} operations, {crashes} crashes, {} storage faults, {} network faults",
        total.ops, total.storage_faults, total.network_faults
    );
    for (site, visits) in &total.sites {
        assert!(visits.crashed > 0, "no crash at {site:?}");
    }
    let reached: BTreeSet<u32> = total
        .sites
        .keys()
        .filter_map(|site| match site {
            Site::Submit(_, at) if at.file().ends_with("counter.rs") => Some(at.line()),
            _ => None,
        })
        .collect();
    assert_eq!(reached, call_sites(), "every I/O call site of the toy");
    let completions = total
        .sites
        .keys()
        .filter(|s| matches!(s, Site::Complete(_)));
    assert_eq!(completions.count(), 7, "every kind of completion");
    // Most seeds finish their operations under these faults.
    assert!(total.ops > seeds * OPS * 9 / 10, "{} operations", total.ops);
    assert!(total.storage_faults > 0 && total.network_faults > 0);
}

/// Injected bug: a replica that acknowledges a write from memory, without
/// writing it, loses acknowledged increments once its holders crash.
#[test]
fn acknowledging_without_a_durable_write_fails_within_1000_seeds() {
    let pool = pool();
    let failing = (0..BUG_SEEDS).find_map(|seed| {
        let config = Config {
            skip_durable_write: true,
            ..faulty(seed)
        };
        counter::run(seed, &config, &pool)
            .violation
            .map(|violation| (seed, violation))
    });
    let (seed, violation) = failing.expect("the bug is found within 1,000 seeds");
    println!("seed {seed}: {violation}");
}

#[test]
fn without_faults_the_counter_completes_every_operation() {
    let pool = pool();
    let config = Config {
        ops: 100,
        ..Config::default()
    };
    for seed in 0..10 {
        let outcome = counter::run(seed, &config, &pool);
        assert_eq!(outcome.violation, None);
        assert_eq!(outcome.ops, 100);
        assert!(outcome.sites.values().all(|v| v.crashed == 0));
    }
}

#[test]
fn a_run_is_a_function_of_the_seed() {
    let pool = pool();
    let mut traces = BTreeSet::new();
    for seed in 0..50 {
        let [a, b] = [0, 1].map(|_| counter::run(seed, &faulty(seed), &pool));
        assert_eq!((a.trace, a.ops, &a.sites), (b.trace, b.ops, &b.sites));
        assert_eq!(
            (a.storage_faults, a.network_faults),
            (b.storage_faults, b.network_faults)
        );
        traces.insert(a.trace);
    }
    assert!(traces.len() > 1, "seeds differ");
}

/// Two nodes on one fabric, the first injecting `faults`.
struct Pair {
    sim: Scheduler<()>,
    a: Node,
    b: Node,
}

fn pair(latency: Latency, faults: node::Faults) -> Pair {
    let mut sim = Scheduler::new(7, 16);
    let zones = [0, 0, 0, 8 * SECTOR_SIZE as u64];
    let group = Group::new(2, 2, zones);
    let fabric = Fabric::new(network::Faults::default(), latency, 100, &mut sim);
    let mut node = |i: usize, faults| {
        let disk = Disk::new(&group, i, 8, storage::Faults::default(), &mut sim);
        let nic = Nic::new(&fabric, Peer(i as u64), 8, &mut sim);
        Node::with_faults(disk, nic, faults, &mut sim)
    };
    let a = node(0, faults);
    let b = node(1, node::Faults::default());
    Pair { sim, a, b }
}

impl Pair {
    fn advance(&mut self, by: u64) {
        let at = self.sim.now() + by;
        self.sim.schedule(at, ()).unwrap();
        self.sim.step();
    }

    /// A connection from `b` to `a`, as (a's end, b's end).
    fn connect(&mut self) -> (Connection, Connection) {
        self.a.accept(1).unwrap();
        self.b.connect(2, Peer(0)).unwrap();
        self.advance(MILLISECOND);
        let Some(Completion::Accepted { result: Ok(a), .. }) = self.a.poll() else {
            panic!("accepted");
        };
        let Some(Completion::Connected { result: Ok(b), .. }) = self.b.poll() else {
            panic!("connected");
        };
        (a, b)
    }
}

/// A message holding `value`.
fn message(pool: &MessagePool, value: u64) -> granite_io::message_pool::Message {
    let mut message = pool.acquire().unwrap();
    seal(message.bytes_mut().unwrap(), LEN, value);
    message
}

fn sequence(buffer: &granite_io::message_pool::Message) -> u64 {
    granite_base::header::Header::decode(buffer.bytes())
        .unwrap()
        .op
        .get()
}

/// The memory of a process: what it received.
#[derive(Debug, Default)]
struct Memory {
    received: Vec<u64>,
}

impl Memory {
    fn take(&mut self, completion: Completion) {
        let Completion::Received {
            buffer,
            result: Ok(_),
            ..
        } = completion
        else {
            panic!("a message: {completion:?}");
        };
        self.received.push(sequence(&buffer));
    }
}

#[test]
fn a_restarted_process_sees_no_memory_from_before_the_crash() {
    let pool = pool();
    let mut p = pair(Latency::ZERO, node::Faults::default());
    let (a, b) = p.connect();
    let mut memory = Memory::default();
    // The process learns 41, and 42 waits for it in the node's mailbox.
    for value in [41, 42] {
        p.a.recv(3, a, pool.acquire().unwrap()).unwrap();
        p.b.send(4, b, LEN, message(&pool, value)).unwrap();
    }
    memory.take(p.a.poll().unwrap());
    assert_eq!(memory.received, [41]);
    // A read of the sector it wrote, in flight.
    let mut sector = pool.acquire().unwrap();
    sector.bytes_mut().unwrap()[..8].copy_from_slice(&43u64.to_le_bytes());
    p.a.disk
        .write(5, Zone::Grid, 0, SECTOR_SIZE as u32, sector)
        .unwrap();
    p.a.read(
        6,
        Zone::Grid,
        SECTOR_SIZE as u64,
        SECTOR_SIZE as u32,
        pool.acquire().unwrap(),
    )
    .unwrap();

    p.a.crash();
    assert!(p.a.down());
    drop(memory);
    p.a.restart();
    let memory = Memory::default();
    assert!(memory.received.is_empty());
    // Nothing from before the crash reaches the new process: not 42, not
    // the read, not the write's completion.
    for _ in 0..10 {
        p.advance(MILLISECOND);
        assert!(p.a.poll().is_none(), "a completion from before the crash");
    }
    assert_eq!(p.a.ready_at(), None);
    // What is on the disk is still there: the write landed whole or not at
    // all, since torn writes are off.
    p.a.read(
        7,
        Zone::Grid,
        0,
        SECTOR_SIZE as u32,
        pool.acquire().unwrap(),
    )
    .unwrap();
    let Some(Completion::Read {
        tag: 7,
        buffer,
        result: Ok(()),
    }) = p.a.poll()
    else {
        panic!("the read");
    };
    let first = u64::from_le_bytes(buffer.bytes()[..8].try_into().unwrap());
    assert!(first == 0 || first == 43);
}

#[test]
fn a_crash_resets_the_nodes_connections_and_drops_its_messages() {
    let pool = pool();
    let latency = Latency {
        min: MILLISECOND,
        max: MILLISECOND,
    };
    let mut p = pair(latency, node::Faults::default());
    let (a, b) = p.connect();
    // A message from a in flight to a posted receive of b, and one from b
    // waiting at a, which posted nothing.
    p.b.recv(3, b, pool.acquire().unwrap()).unwrap();
    p.a.send(4, a, LEN, message(&pool, 1)).unwrap();
    p.b.send(5, b, LEN, message(&pool, 2)).unwrap();
    // a connects to b, which has not accepted.
    p.a.connect(6, Peer(1)).unwrap();
    p.a.crash();
    p.a.restart();

    p.advance(2 * MILLISECOND);
    let mut seen = Vec::new();
    while let Some(completion) = p.b.poll() {
        match completion {
            Completion::Received { tag: 3, result, .. } => seen.push(("recv", result.err())),
            Completion::Sent { tag: 5, result, .. } => seen.push(("send", result.err())),
            other => panic!("{other:?}"),
        }
    }
    seen.sort_by_key(|s| s.0);
    let reset = Some(NetworkError::Reset);
    assert_eq!(
        seen,
        [("recv", reset), ("send", reset)],
        "message 1 never arrives"
    );
    // The connect from before the crash is gone.
    p.b.accept(7).unwrap();
    p.advance(2 * MILLISECOND);
    assert!(p.a.poll().is_none() && p.b.poll().is_none());
    // The restarted node is on the fabric under the same peer.
    p.a.connect(8, Peer(1)).unwrap();
    p.advance(2 * MILLISECOND);
    assert!(matches!(
        p.a.poll(),
        Some(Completion::Connected {
            tag: 8,
            result: Ok(_)
        })
    ));
    assert!(matches!(
        p.b.poll(),
        Some(Completion::Accepted {
            tag: 7,
            result: Ok(_)
        })
    ));
}

#[test]
fn a_paused_shard_resumes_with_its_state_intact_and_time_advanced() {
    let pool = pool();
    let mut p = pair(Latency::ZERO, node::Faults::default());
    let (a, b) = p.connect();
    let mut memory = Memory::default();
    for value in [1, 2] {
        p.a.recv(3, a, pool.acquire().unwrap()).unwrap();
        p.b.send(4, b, LEN, message(&pool, value)).unwrap();
    }
    memory.take(p.a.poll().unwrap());
    let paused_at = p.a.monotonic();
    p.a.pause(10 * MILLISECOND);
    assert!(!p.a.running());
    assert_eq!(p.a.ready_at(), Some(paused_at + 10 * MILLISECOND));
    p.advance(9 * MILLISECOND);
    assert!(p.a.poll().is_none(), "nothing reaches a paused process");
    p.advance(MILLISECOND);
    assert!(p.a.running());
    memory.take(p.a.poll().expect("message 2 after the pause"));
    assert_eq!(memory.received, [1, 2], "state intact");
    assert_eq!(p.a.monotonic(), paused_at + 10 * MILLISECOND);
}

#[test]
fn a_drawn_pause_holds_the_completion_until_it_ends() {
    let faults = node::Faults {
        pause: MILLION,
        pause_max: 5 * MILLISECOND,
        ..node::Faults::default()
    };
    let mut p = pair(Latency::ZERO, faults);
    p.a.fsync(1).unwrap();
    assert!(p.a.poll().is_none(), "paused instead");
    let resume = p.a.ready_at().unwrap();
    assert!(resume > p.sim.now() && resume <= p.sim.now() + 5 * MILLISECOND);
    p.advance(resume - p.sim.now());
    assert!(matches!(p.a.poll(), Some(Completion::Fsync { tag: 1, .. })));
}

#[test]
fn a_slow_shard_takes_one_completion_per_interval() {
    let faults = node::Faults {
        slow: MILLISECOND,
        ..node::Faults::default()
    };
    let mut p = pair(Latency::ZERO, faults);
    for tag in 0..4 {
        p.a.fsync(tag).unwrap();
    }
    let start = p.sim.now();
    let mut times = Vec::new();
    while times.len() < 4 {
        match p.a.poll() {
            Some(_) => times.push(p.sim.now() - start),
            None => {
                let at = p.a.ready_at().unwrap();
                p.advance(at - p.sim.now());
            }
        }
    }
    assert_eq!(times, [0, MILLISECOND, 2 * MILLISECOND, 3 * MILLISECOND]);
}

#[test]
fn a_crash_is_injected_at_a_submission_and_at_a_completion() {
    let pool = pool();
    let always = node::Faults {
        crash: MILLION,
        ..node::Faults::default()
    };
    // At a submission: the call is taken, then the process is gone, and the
    // node swallows what the dying process still submits.
    let mut p = pair(Latency::ZERO, always);
    p.a.fsync(1).unwrap();
    assert!(p.a.down());
    p.a.write(
        2,
        Zone::Grid,
        0,
        SECTOR_SIZE as u32,
        pool.acquire().unwrap(),
    )
    .unwrap();
    assert!(p.a.poll().is_none());
    let sites: Vec<_> = p.a.sites().iter().map(|(s, v)| (*s, *v)).collect();
    assert_eq!(sites.len(), 1);
    assert!(matches!(sites[0].0, Site::Submit(Op::Fsync, at) if at.file().ends_with("crash.rs")));
    assert_eq!(
        sites[0].1,
        Visits {
            reached: 1,
            crashed: 1
        }
    );
    // At a completion: the device carried the operation out, and the
    // process never sees it.
    p.a.restart();
    p.a.disk.fsync(3).unwrap();
    assert!(p.a.poll().is_none());
    assert!(p.a.down());
    let complete = p.a.sites()[&Site::Complete(Op::Fsync)];
    assert_eq!(
        complete,
        Visits {
            reached: 1,
            crashed: 1
        }
    );
}
