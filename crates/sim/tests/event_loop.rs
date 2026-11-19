//! The gates of T0.3.5 that do not need Linux: the shard event loop, driven
//! on the simulator's virtual clock, gives each scheduling class its budget,
//! keeps the lowest class from starving, lets a class alone use the whole
//! iteration, delivers completions and ticks, and allocates nothing per
//! iteration. The core-pinning gate is a test in `granite_io::pin`.
//!
//! Every task "runs" by moving the virtual clock forward by its cost, so the
//! shares measured here are what the loop read through `Time`.

#[path = "../../io/tests/common/counting_alloc.rs"]
mod counting_alloc;

use granite_io::event_loop::{Class, EventLoop, Tasks};
use granite_io::memory::{Hub, Memory};
use granite_io::message_pool::{Message, MessagePool, SECTOR_SIZE};
use granite_io::{
    Completion, Connection, Full, Io, Network, Peer, Random, Storage, Tag, Time, Zone,
};
use granite_sim::{Clock, Scheduler};

const MICROSECOND: u64 = 1_000;
/// Nanoseconds per iteration, in `Class::ALL` order; 1 ms in all.
const BUDGETS: [u64; 6] = [400_000, 200_000, 100_000, 100_000, 150_000, 50_000];
/// Nanoseconds one task of each class takes; none divides its budget evenly.
const COSTS: [u64; 6] = [7_000, 13_000, 31_000, 45_000, 3_000, 11_000];
const TICK: u64 = 100 * MICROSECOND;
const COMPLETIONS: u32 = 4;
const ITERATIONS: u32 = 10_000;

/// A node of the in-memory backend that reads the virtual clock.
struct Node {
    memory: Memory,
    clock: Clock,
}

impl Storage for Node {
    fn size(&self, zone: Zone) -> u64 {
        self.memory.size(zone)
    }
    fn read(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        self.memory.read(tag, zone, offset, len, buffer)
    }
    fn write(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        self.memory.write(tag, zone, offset, len, buffer)
    }
    fn fsync(&mut self, tag: Tag) -> Result<(), Full<()>> {
        self.memory.fsync(tag)
    }
}

impl Network for Node {
    fn connect(&mut self, tag: Tag, to: Peer) -> Result<(), Full<()>> {
        self.memory.connect(tag, to)
    }
    fn accept(&mut self, tag: Tag) -> Result<(), Full<()>> {
        self.memory.accept(tag)
    }
    fn send(
        &mut self,
        tag: Tag,
        connection: Connection,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        self.memory.send(tag, connection, len, buffer)
    }
    fn recv(
        &mut self,
        tag: Tag,
        connection: Connection,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        self.memory.recv(tag, connection, buffer)
    }
    fn close(&mut self, connection: Connection) {
        self.memory.close(connection)
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
        self.memory.next_u64()
    }
}

impl Io for Node {
    fn poll(&mut self) -> Option<Completion> {
        self.memory.poll()
    }
}

/// Classes that are ready or not, with tasks of a fixed cost. A commit task
/// also writes a sector while `writes` lasts and a buffer is free; with
/// `chain`, each completed write submits the next one at once.
struct Synthetic {
    sim: Scheduler<()>,
    pool: MessagePool,
    ready: [bool; 6],
    costs: [u64; 6],
    spent: [u64; 6],
    writes: u32,
    completions: u32,
    chain: bool,
    ticks: u64,
}

impl Tasks<Node> for Synthetic {
    fn complete(&mut self, io: &mut Node, completion: Completion) {
        let Completion::Write {
            buffer,
            result: Ok(()),
            ..
        } = completion
        else {
            panic!("only writes are submitted");
        };
        self.completions += 1;
        if self.chain {
            // 100 capped iterations handle 400; an uncapped drain never stops.
            assert!(self.completions < 1_000, "the drain is not capped");
            io.write(0, Zone::Grid, 0, SECTOR_SIZE as u32, buffer)
                .expect("room");
        }
    }

    fn tick(&mut self, _io: &mut Node) {
        self.ticks += 1;
    }

    fn run(&mut self, io: &mut Node, class: Class) -> bool {
        let c = class as usize;
        if !self.ready[c] {
            return false;
        }
        if class == Class::Commit
            && self.writes > 0
            && let Some(buffer) = self.pool.acquire()
        {
            io.write(0, Zone::Grid, 0, SECTOR_SIZE as u32, buffer)
                .expect("room");
            self.writes -= 1;
        }
        let at = self.sim.now() + self.costs[c];
        self.sim.schedule(at, ()).expect("room");
        self.sim.step();
        self.spent[c] += self.costs[c];
        true
    }
}

fn setup(ready: [bool; 6], costs: [u64; 6]) -> (EventLoop, Node, Synthetic) {
    let sim = Scheduler::new(7, 1);
    let hub = Hub::new();
    let sector = SECTOR_SIZE as u64;
    let node = Node {
        memory: Memory::new(&hub, Peer(0), [sector; 4], 4, 7),
        clock: sim.clock(),
    };
    let tasks = Synthetic {
        sim,
        pool: MessagePool::new(1),
        ready,
        costs,
        spent: [0; 6],
        writes: 0,
        completions: 0,
        chain: false,
        ticks: 0,
    };
    (EventLoop::new(BUDGETS, COMPLETIONS, TICK, 0), node, tasks)
}

fn shares(spent: [u64; 6]) -> [f64; 6] {
    let total: u64 = spent.iter().sum();
    spent.map(|s| s as f64 / total as f64)
}

fn budget_share(class: Class) -> f64 {
    BUDGETS[class as usize] as f64 / BUDGETS.iter().sum::<u64>() as f64
}

#[test]
fn every_class_gets_its_budget_when_all_are_ready() {
    let (mut shard, mut node, mut tasks) = setup([true; 6], COSTS);
    for _ in 0..ITERATIONS {
        shard.iterate(&mut node, &mut tasks);
    }
    let shares = shares(tasks.spent);
    for class in Class::ALL {
        let (got, want) = (shares[class as usize], budget_share(class));
        println!(
            "{class:?}: {:.3}% of the time, budget {:.3}%",
            got * 100.0,
            want * 100.0
        );
        assert!(
            (got - want).abs() < 0.05,
            "{class:?} got {got}, budget {want}"
        );
    }
}

#[test]
fn lowest_class_keeps_its_share_under_a_busy_commit_class() {
    // Commit tasks of 1.5 ms, longer than a whole iteration, always ready:
    // first with only the lowest class also ready, then with every class.
    let mut costs = COSTS;
    costs[Class::Commit as usize] = 1_500 * MICROSECOND;
    let lowest = Class::Handshakes as usize;
    let mut only_lowest = [false; 6];
    only_lowest[Class::Commit as usize] = true;
    only_lowest[lowest] = true;
    for ready in [only_lowest, [true; 6]] {
        let (mut shard, mut node, mut tasks) = setup(ready, costs);
        for _ in 0..ITERATIONS {
            shard.iterate(&mut node, &mut tasks);
        }
        let got = shares(tasks.spent)[lowest];
        let minimum = budget_share(Class::Handshakes);
        println!(
            "ready {ready:?}: lowest class {:.3}%, minimum {:.3}%",
            got * 100.0,
            minimum * 100.0
        );
        // Less than one task short over the run.
        assert!(
            got >= minimum * 0.999,
            "lowest class got {got}, minimum {minimum}"
        );
    }
}

#[test]
fn a_class_alone_gets_the_whole_iteration() {
    let iteration: u64 = BUDGETS.iter().sum();
    for class in Class::ALL {
        let c = class as usize;
        let mut ready = [false; 6];
        ready[c] = true;
        let (mut shard, mut node, mut tasks) = setup(ready, COSTS);
        for _ in 0..ITERATIONS {
            let (start, spent) = (node.monotonic(), tasks.spent[c]);
            shard.iterate(&mut node, &mut tasks);
            let length = node.monotonic() - start;
            assert_eq!(tasks.spent[c] - spent, length, "{class:?} ran all of it");
            // Its budget and every idle class's, less a debt of one task.
            assert!(
                length > iteration - COSTS[c],
                "{class:?} ran {length} of {iteration}"
            );
        }
    }
}

#[test]
fn completions_and_ticks_are_delivered() {
    let mut ready = [false; 6];
    ready[Class::Commit as usize] = true;
    let (mut shard, mut node, mut tasks) = setup(ready, COSTS);
    // One buffer, so one write in flight: each completes in the next
    // iteration and frees the buffer for the next write.
    tasks.writes = 50;
    for _ in 0..100 {
        shard.iterate(&mut node, &mut tasks);
    }
    assert_eq!((tasks.writes, tasks.completions), (0, 50));
    // An iteration with nothing ready moves no time and delivers every tick due.
    tasks.ready = [false; 6];
    shard.iterate(&mut node, &mut tasks);
    assert_eq!(tasks.ticks, node.monotonic() / TICK);
    assert!(tasks.ticks >= 100 * 1_000 * MICROSECOND / TICK);
}

#[test]
fn an_iteration_allocates_nothing_after_startup() {
    let (mut shard, mut node, mut tasks) = setup([true; 6], COSTS);
    tasks.writes = u32::MAX;
    shard.iterate(&mut node, &mut tasks);
    let before = counting_alloc::allocations();
    for _ in 0..ITERATIONS {
        shard.iterate(&mut node, &mut tasks);
    }
    assert_eq!(counting_alloc::allocations() - before, 0);
    assert!(
        tasks.completions > 0 && tasks.ticks > 0,
        "the loop did its work"
    );
}

#[test]
fn completions_that_submit_more_do_not_starve_the_classes() {
    let (mut shard, mut node, mut tasks) = setup([true; 6], COSTS);
    // In the in-memory backend a write is ready to poll as soon as it is
    // submitted, so without a cap this chain would keep the loop polling.
    tasks.writes = 1;
    tasks.chain = true;
    for i in 1..=100 {
        let spent: u64 = tasks.spent.iter().sum();
        shard.iterate(&mut node, &mut tasks);
        assert_eq!(tasks.completions, (i - 1) * COMPLETIONS);
        assert!(tasks.spent.iter().sum::<u64>() > spent, "the classes ran");
    }
}
