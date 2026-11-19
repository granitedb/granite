//! The core of the simulator (document 06 section 1): one thread, one seed,
//! one order of events.
//!
//! A [`Scheduler`] holds the events of a run, ordered by virtual time and,
//! at equal times, by the order they were scheduled. Running the next event
//! moves the virtual clock to its time, so idle time costs nothing. The
//! scheduler's generator is the only randomness of a run, and every event it
//! runs is folded into a trace hash that two runs compare.

use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use granite_io::memory::SplitMix64;
use granite_io::{Full, Random, Time};

/// What [`Clock::realtime`] reads at virtual time zero: 2026-01-01T00:00:00Z.
pub const REALTIME_START: i64 = 1_767_225_600 * 1_000_000_000;

/// The simulated [`Time`]: the virtual clock of one run. Every node of a run
/// reads a clone of the same clock, which only its [`Scheduler`] moves.
#[derive(Clone, Debug)]
pub struct Clock(Rc<Cell<u64>>);

impl Time for Clock {
    fn monotonic(&self) -> u64 {
        self.0.get()
    }

    fn realtime(&self) -> i64 {
        REALTIME_START
            .checked_add_unsigned(self.0.get())
            .expect("realtime fits in i64")
    }
}

/// A running hash of everything a run did: each word is xored into the state,
/// which then takes one SplitMix64 step. Integers are hashed as little-endian
/// `u64` or `u128` whatever the platform, so a derived [`Hash`] gives the same
/// trace on every machine.
struct Trace(u64);

impl Hasher for Trace {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u64(u64::from(byte));
        }
    }

    fn write_u16(&mut self, i: u16) {
        self.write_u64(u64::from(i));
    }

    fn write_u32(&mut self, i: u32) {
        self.write_u64(u64::from(i));
    }

    fn write_u64(&mut self, i: u64) {
        self.0 = SplitMix64(self.0 ^ i).next_u64();
    }

    fn write_u128(&mut self, i: u128) {
        self.write_u64(i as u64);
        self.write_u64((i >> 64) as u64);
    }

    fn write_usize(&mut self, i: usize) {
        self.write_u64(u64::try_from(i).expect("usize fits in u64"));
    }
}

/// An event and its place in the order: its time in the high 64 bits of
/// `key`, its scheduling order in the low 64 bits.
struct Entry<E> {
    key: u128,
    event: E,
}

impl<E> PartialEq for Entry<E> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl<E> Eq for Entry<E> {}

impl<E> PartialOrd for Entry<E> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Reversed, so that [`BinaryHeap`], a max-heap, pops the earliest entry.
impl<E> Ord for Entry<E> {
    fn cmp(&self, other: &Self) -> Ordering {
        other.key.cmp(&self.key)
    }
}

/// The event queue, virtual clock, generator and trace of one run.
pub struct Scheduler<E> {
    clock: Clock,
    queue: BinaryHeap<Entry<E>>,
    capacity: usize,
    scheduled: u64,
    random: SplitMix64,
    trace: Trace,
}

impl<E: Hash> Scheduler<E> {
    /// A run from `seed`, at virtual time zero, holding at most `capacity`
    /// pending events.
    pub fn new(seed: u64, capacity: usize) -> Self {
        assert!(capacity > 0);
        Scheduler {
            clock: Clock(Rc::new(Cell::new(0))),
            // Sized up front, so that a running simulation allocates nothing.
            queue: BinaryHeap::with_capacity(capacity),
            capacity,
            scheduled: 0,
            random: SplitMix64(seed),
            trace: Trace(0),
        }
    }

    /// The run's clock, to hand to the nodes.
    pub fn clock(&self) -> Clock {
        self.clock.clone()
    }

    /// The current virtual time in nanoseconds.
    pub fn now(&self) -> u64 {
        self.clock.monotonic()
    }

    /// Events scheduled and not yet run.
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// Schedules `event` to run at virtual time `at`, after every event
    /// already scheduled for `at`. A full queue refuses it.
    pub fn schedule(&mut self, at: u64, event: E) -> Result<(), Full<E>> {
        assert!(at >= self.now(), "an event cannot run in the past");
        if self.queue.len() == self.capacity {
            return Err(Full(event));
        }
        let key = u128::from(at) << 64 | u128::from(self.scheduled);
        self.queue.push(Entry { key, event });
        self.scheduled += 1;
        Ok(())
    }

    /// Moves the clock to the earliest event, adds it to the trace and
    /// returns it; `None` when no event is left.
    pub fn step(&mut self) -> Option<E> {
        let entry = self.queue.pop()?;
        let at = (entry.key >> 64) as u64;
        assert!(at >= self.now(), "the clock never goes back");
        self.clock.0.set(at);
        at.hash(&mut self.trace);
        entry.event.hash(&mut self.trace);
        Some(entry.event)
    }

    /// The hash of every event run so far, with its time.
    pub fn trace(&self) -> u64 {
        self.trace.finish()
    }
}

/// The run's generator: the only randomness of a simulation.
impl<E> Random for Scheduler<E> {
    fn next_u64(&mut self) -> u64 {
        self.random.next_u64()
    }
}
