//! The completion-model spike (task T0.3.1, decision 0003).
//!
//! One small replicated component written twice: as an explicit state machine
//! with callbacks (`callbacks.rs`) and as `async` on a minimal executor
//! (`futures.rs`). A toy simulator runs three nodes on the in-memory backend
//! and delivers one completion at a time, chosen by the seed, so each
//! delivery is one callback or one resumption from an await. It crashes a
//! backup between two deliveries, at a step chosen by the seed. Both versions
//! must leave the same event trace for every seed.
//!
//! The component: node 0, the primary, writes ops 0 to `OPS - 1` to its WAL
//! and sends each to both backups. A backup writes the op to its WAL and
//! acknowledges it. The primary commits ops in order, each once its own write
//! and one backup's acknowledgement are done (a quorum of 2 of 3).
//!
//! The simulator connects the primary to each backup before the component
//! starts, the same way for both versions. The test binary counts
//! allocations, for the decision's comparison.

#![allow(unsafe_code, reason = "a global allocator is an unsafe trait")]

mod callbacks;
mod futures;

use granite_io::memory::{Hub, Memory, SplitMix64};
use granite_io::message_pool::{Message, MessagePool, SECTOR_SIZE};
use granite_io::{Completion, Connection, Io, Network, Peer};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

pub const NODES: usize = 3;
pub const OPS: usize = 8;
/// Receive buffers each node keeps posted.
pub const POSTED: usize = 4;
pub const PREPARE: u8 = 1;
pub const ACK: u8 = 2;
/// Length of a message: kind and op number.
pub const LEN: u32 = 9;
pub const SECTOR: u64 = SECTOR_SIZE as u64;
const ZONES: [u64; 4] = [4 * SECTOR, 4 * SECTOR, OPS as u64 * SECTOR, 4 * SECTOR];
/// Crash steps are drawn below this; a run has fewer deliveries, so some
/// seeds never crash.
const CRASH_STEPS: u64 = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// `node` finished writing `op` to its WAL.
    Written {
        node: u8,
        op: u64,
    },
    /// The primary received the acknowledgement of `op` from `from`.
    Acked {
        op: u64,
        from: u8,
    },
    Committed {
        op: u64,
    },
    Crashed {
        node: u8,
    },
}

/// One node of the component, in either style.
pub trait Node {
    /// Starts the node on `io`, with `pool` for its buffers and
    /// `links[peer]` its connection to each peer it is connected to.
    fn new(io: Memory, pool: MessagePool, index: u8, links: Links) -> Self;
    /// The node's completions ready for delivery.
    fn ready(&self) -> usize;
    /// Delivers one completion and appends what it caused to `trace`.
    fn step(&mut self, trace: &mut Vec<Event>);
}

/// A node's connection to each other node, if it has one.
pub type Links = [Option<Connection>; NODES];

/// Connects the primary to each backup; returns every node's links.
fn connect(ios: &mut [Memory; NODES]) -> [Links; NODES] {
    for backup in 1..NODES {
        ios[0]
            .connect(backup as u64, Peer(backup as u64))
            .expect("room");
        ios[backup].accept(0).expect("room");
    }
    let mut links = [[None; NODES]; NODES];
    for (index, io) in ios.iter_mut().enumerate() {
        while let Some(completion) = io.poll() {
            let (Completion::Connected { tag, result } | Completion::Accepted { tag, result }) =
                completion
            else {
                unreachable!()
            };
            links[index][tag as usize] = Some(result.expect("connected"));
        }
    }
    links
}

pub fn encode(message: &mut Message, kind: u8, op: u64) {
    let bytes = message.bytes_mut().expect("only handle");
    bytes[0] = kind;
    bytes[1..9].copy_from_slice(&op.to_le_bytes());
}

pub fn decode(message: &Message) -> (u8, u64) {
    let bytes = message.bytes();
    (
        bytes[0],
        u64::from_le_bytes(bytes[1..9].try_into().unwrap()),
    )
}

/// Commits every op whose write and one acknowledgement are done, in order.
pub fn commit(
    written: &[bool; OPS],
    acks: &[u8; OPS],
    committed: &mut usize,
    events: &mut Vec<Event>,
) {
    while *committed < OPS && written[*committed] && acks[*committed] > 0 {
        events.push(Event::Committed {
            op: *committed as u64,
        });
        *committed += 1;
    }
}

/// What one run left.
#[derive(Debug, PartialEq)]
struct Run {
    trace: Vec<Event>,
    /// Completions delivered.
    steps: u64,
    /// Allocations made by the component's own code: while starting the
    /// nodes, and while handling completions.
    allocations: [u64; 2],
}

/// Runs the component for `seed`, crashing a backup before delivery
/// `crash_at`.
fn run<N: Node>(seed: u64, crash_at: Option<u64>) -> Run {
    let hub = Hub::new();
    let mut rng = SplitMix64(seed);
    // Every node joins the hub before any starts sending.
    let mut ios: [Memory; NODES] = std::array::from_fn(|index| {
        Memory::new(&hub, Peer(index as u64), ZONES, 32, rng.next_u64())
    });
    let links = connect(&mut ios);
    let pools: [MessagePool; NODES] = std::array::from_fn(|_| MessagePool::new(16));
    let mut trace = Vec::with_capacity(8 * OPS);
    let mut allocations = [0; 2];
    let mut nodes: [Option<N>; NODES] = [None, None, None];
    for (index, (io, pool)) in ios.into_iter().zip(pools).enumerate() {
        nodes[index] = Some(counted(&mut allocations[0], || {
            N::new(io, pool, index as u8, links[index])
        }));
    }
    let crash_node = 1 + (seed % 2) as usize;
    let mut steps = 0;
    loop {
        if crash_at == Some(steps) {
            nodes[crash_node] = None;
            trace.push(Event::Crashed {
                node: crash_node as u8,
            });
        }
        let mut ready = [0; NODES];
        let mut count = 0;
        for (index, node) in nodes.iter().enumerate() {
            if node.as_ref().is_some_and(|node| node.ready() > 0) {
                ready[count] = index;
                count += 1;
            }
        }
        if count == 0 {
            break;
        }
        let index = ready[(rng.next_u64() % count as u64) as usize];
        let node = nodes[index].as_mut().unwrap();
        counted(&mut allocations[1], || node.step(&mut trace));
        steps += 1;
    }
    let committed: Vec<_> = trace
        .iter()
        .filter_map(|event| match event {
            Event::Committed { op } => Some(*op),
            _ => None,
        })
        .collect();
    assert_eq!(
        committed,
        (0..OPS as u64).collect::<Vec<_>>(),
        "seed {seed}"
    );
    Run {
        trace,
        steps,
        allocations,
    }
}

/// Calls `call`, adding the allocations it makes to `allocations`.
fn counted<T>(allocations: &mut u64, call: impl FnOnce() -> T) -> T {
    let before = ALLOCATIONS.get();
    let result = call();
    *allocations += ALLOCATIONS.get() - before;
    result
}

fn crash_at(seed: u64) -> Option<u64> {
    Some((seed / 2) % CRASH_STEPS)
}

/// Runs both versions and checks that they leave the same trace.
fn both(seed: u64, crash_at: Option<u64>) -> Run {
    let expected = run::<callbacks::Replica>(seed, crash_at);
    let actual = run::<futures::Replica>(seed, crash_at);
    assert_eq!(
        expected.trace, actual.trace,
        "seed {seed}, crash at {crash_at:?}"
    );
    assert_eq!(expected.steps, actual.steps);
    expected
}

#[test]
fn both_versions_leave_the_same_trace_for_1000_seeds() {
    let mut crashes = 0;
    for seed in 0..1_000 {
        let run = both(seed, crash_at(seed));
        crashes += run
            .trace
            .iter()
            .filter(|e| matches!(e, Event::Crashed { .. }))
            .count();
    }
    println!("1000 seeds, identical traces, {crashes} with a crash");
    assert!(crashes > 500);
}

/// The traces depend on the seed, so the comparison above is not vacuous.
#[test]
fn traces_differ_between_seeds() {
    let first = run::<callbacks::Replica>(0, None).trace;
    assert!((1..10).any(|seed| run::<callbacks::Replica>(seed, None).trace != first));
}

/// A crash before every delivery of one run, of either backup, leaves the
/// same trace in both versions and still commits every op.
#[test]
fn a_crash_at_every_point_is_injectable_in_both() {
    for seed in [0, 1] {
        let steps = both(seed, None).steps;
        for step in 0..=steps {
            both(seed, Some(step));
        }
        println!("seed {seed}: {} crash points", steps + 1);
    }
}

/// The figures of decision 0003. The state machine allocates nothing once
/// its buffers come from the pool; the executor allocates per task and per
/// completion it parks.
#[test]
fn allocations_per_operation() {
    let callbacks = run::<callbacks::Replica>(0, None);
    let futures = run::<futures::Replica>(0, None);
    println!(
        "{OPS} ops, {} deliveries; allocations at start and in handlers: callbacks {:?}, futures {:?}",
        callbacks.steps, callbacks.allocations, futures.allocations
    );
    assert_eq!(callbacks.allocations, [0, 0]);
    assert!(futures.allocations[0] > 0);
}

struct Counting;

thread_local! {
    // `const` initialisation, so reading it never allocates.
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

fn count() {
    ALLOCATIONS.set(ALLOCATIONS.get() + 1);
}

// SAFETY: every call is forwarded unchanged to the system allocator; the
// counter has no effect on the memory handed out.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: the caller's contract for `alloc` is passed on.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: the caller's contract for `alloc_zeroed` is passed on.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: the caller's contract for `realloc` is passed on.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract for `dealloc` is passed on.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;
