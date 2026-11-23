//! The library side of the seed runner, `cargo run -p granite-sim --bin sim`
//! (document 06 section 1, "Operation"): the scenarios it can run, the log
//! of events a failure prints, and running one seed so that a panic, an
//! assertion or a hang is a failure like a failed checker.
//!
//! A scenario is a name and two functions of the seed: its swarm
//! configuration, printed with a failure, and the run itself, which reports
//! its events to a [`Log`] and returns the checker that failed. A new
//! scenario is one more entry in [`SCENARIOS`].
//!
//! Each worker thread hands every seed it runs the same message pool, of
//! [`POOL`] buffers: allocating one per seed costs more than the seed, and
//! with many threads its unmapping stalls them all. A seed must return every
//! buffer, and a scenario must read only bytes it wrote in that seed, so
//! that a seed run alone, from a fresh pool, does the same.

use std::collections::VecDeque;
use std::panic::{self, AssertUnwindSafe};

use granite_io::message_pool::MessagePool;

use crate::node::Op;
use crate::{Scheduler, counter, network, storage};

/// Buffers of the message pool a scenario runs with.
pub const POOL: u32 = 48;

/// Events a run may log at one virtual time before it counts as hung.
// shortcut: a run that spins without logging is not caught and stalls the
// runner until CI's timeout; add a wall-clock watchdog if a scenario does.
pub const STALL: u64 = 1_000_000;
/// How the panic of a hung run starts.
pub const HANG: &str = "no progress in virtual time";

/// What a failure names: the checker and what it saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// The checker that failed, `panic` (which covers a failed assertion) or
    /// `hang`.
    pub checker: &'static str,
    pub message: String,
}

pub struct Scenario {
    pub name: &'static str,
    /// What the seed turns on; a function of the seed only.
    pub swarm: fn(u64) -> String,
    /// Runs the seed, reporting its events to the log, with buffers from
    /// the pool.
    pub run: fn(u64, &mut Log, &MessagePool) -> Result<(), Failure>,
}

/// Every scenario the runner knows; the first is the default.
pub const SCENARIOS: &[Scenario] = &[
    counter::SCENARIO,
    counter::LOST_WRITE,
    crate::replica::SCENARIO,
    FIXTURE_PANIC,
    FIXTURE_ASSERTION,
    FIXTURE_HANG,
];

pub fn scenario(name: &str) -> Option<&'static Scenario> {
    SCENARIOS.iter().find(|s| s.name == name)
}

/// One thing that happened in a run.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// A node handed its process a completion.
    Completion {
        node: usize,
        op: Op,
    },
    /// A crashed node's process was dropped and a new one started.
    Restart {
        node: usize,
    },
    Storage {
        node: usize,
        fault: storage::Fault,
    },
    Network(network::Fault),
    /// A scenario's own event, by name, with a value.
    Note(&'static str, u64),
}

#[derive(Clone, Copy, Debug)]
pub struct Entry {
    /// The event's number in the run, from zero.
    pub index: u64,
    /// Virtual time, in nanoseconds.
    pub at: u64,
    pub event: Event,
}

/// The events of a run: the last few, kept for a failure report, or every
/// one printed as it happens.
pub struct Log {
    last: VecDeque<Entry>,
    keep: usize,
    print: bool,
    count: u64,
    /// Events pushed since virtual time last moved.
    still: u64,
    last_at: Option<u64>,
}

impl Log {
    /// Keeps the last `keep` events.
    pub fn new(keep: usize) -> Self {
        Log {
            last: VecDeque::with_capacity(keep),
            keep,
            print: false,
            count: 0,
            still: 0,
            last_at: None,
        }
    }

    /// Prints every event to standard output as it happens, and keeps none.
    pub fn printing() -> Self {
        Log {
            print: true,
            ..Log::new(0)
        }
    }

    /// Adds an event at virtual time `at`. Panics if [`STALL`] events in a
    /// row come at one time: the run is hung.
    pub fn push(&mut self, at: u64, event: Event) {
        let same = self.last_at == Some(at);
        self.last_at = Some(at);
        self.still = if same { self.still + 1 } else { 0 };
        assert!(
            self.still < STALL,
            "{HANG}: {STALL} events at {at} ns without the clock moving"
        );
        let entry = Entry {
            index: self.count,
            at,
            event,
        };
        self.count += 1;
        if self.print {
            println!("{}", line(&entry));
        }
        if self.keep == 0 {
            return;
        }
        if self.last.len() == self.keep {
            self.last.pop_front();
        }
        self.last.push_back(entry);
        assert!(self.last.len() <= self.keep);
    }

    /// The events kept, oldest first.
    pub fn last(&self) -> impl Iterator<Item = &Entry> {
        self.last.iter()
    }

    /// Events pushed so far.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Forgets every event, to run another seed.
    pub fn clear(&mut self) {
        self.last.clear();
        self.count = 0;
        self.still = 0;
        self.last_at = None;
    }
}

fn line(entry: &Entry) -> String {
    format!("#{} at {} ns: {:?}", entry.index, entry.at, entry.event)
}

/// The command that reruns one seed of a scenario, printing its full trace
/// once `--trace` is added.
pub fn command(scenario: &Scenario, seed: u64) -> String {
    format!(
        "cargo run --profile sim --locked -p granite-sim --bin sim -- --scenario {} --seed {seed}",
        scenario.name
    )
}

/// What the runner prints for a failed seed.
pub fn report(
    scenario: &Scenario,
    seed: u64,
    commit: &str,
    failure: &Failure,
    log: &Log,
) -> String {
    let mut out = String::new();
    let mut put = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };
    put(format!("FAILED seed {seed} of scenario {}", scenario.name));
    put(format!("commit: {commit}"));
    put(format!("checker: {}", failure.checker));
    put(format!("message: {}", failure.message));
    put(format!("events: {}", log.count()));
    put(format!("swarm: {}", (scenario.swarm)(seed)));
    put(format!("last {} events:", log.last.len()));
    for entry in log.last() {
        put(format!("  {}", line(entry)));
    }
    put(format!("reproduce: {}", command(scenario, seed)));
    out
}

/// Runs one seed of `scenario`, its events going to `log` and its buffers
/// coming from `pool`, which it leaves as it found it: every buffer free. A
/// panic is a failure, and a panic of the log's hang check is a `hang`; the
/// default panic hook still prints where it happened.
pub fn run(
    scenario: &Scenario,
    seed: u64,
    log: &mut Log,
    pool: &MessagePool,
) -> Result<(), Failure> {
    assert_eq!(pool.counters().in_use, 0, "the pool starts free");
    let result = panic::catch_unwind(AssertUnwindSafe(|| (scenario.run)(seed, log, pool)));
    assert_eq!(
        pool.counters().in_use,
        0,
        "seed {seed} returned every buffer"
    );
    let payload = match result {
        Ok(result) => return result,
        Err(payload) => payload,
    };
    let message = match (
        payload.downcast_ref::<&str>(),
        payload.downcast_ref::<String>(),
    ) {
        (Some(s), _) => (*s).to_owned(),
        (_, Some(s)) => s.clone(),
        _ => String::from("a panic without a message"),
    };
    let checker = if message.starts_with(HANG) {
        "hang"
    } else {
        "panic"
    };
    Err(Failure { checker, message })
}

/// Fixtures that fail on purpose, one of every third seed, to test the
/// runner on each kind of failure.
const FIXTURE_PANIC: Scenario = Scenario {
    name: "fixture-panic",
    swarm: |_| String::from("none"),
    run: |seed, log, _| {
        log.push(0, Event::Note("seed", seed));
        if seed % 3 == 0 {
            panic!("fixture panic at seed {seed}");
        }
        Ok(())
    },
};

const FIXTURE_ASSERTION: Scenario = Scenario {
    name: "fixture-assertion",
    swarm: |_| String::from("none"),
    run: |seed, log, _| {
        log.push(0, Event::Note("seed", seed));
        assert!(seed % 3 != 0, "fixture assertion at seed {seed}");
        Ok(())
    },
};

const FIXTURE_HANG: Scenario = Scenario {
    name: "fixture-hang",
    swarm: |_| String::from("none"),
    run: |seed, log, _| {
        let mut sim = Scheduler::new(seed, 1);
        sim.schedule(0, ()).expect("room");
        // The event reschedules itself at the same time on every third seed:
        // the clock stops.
        while sim.step().is_some() {
            log.push(sim.now(), Event::Note("step", sim.now()));
            let next = if seed % 3 == 0 { 0 } else { 1 };
            if sim.now() < 100 {
                sim.schedule(sim.now() + next, ()).expect("room");
            }
        }
        Ok(())
    },
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_keeps_the_last_events() {
        let mut log = Log::new(3);
        for i in 0..10 {
            log.push(i, Event::Note("i", i));
        }
        let kept: Vec<u64> = log.last().map(|e| e.index).collect();
        assert_eq!(kept, [7, 8, 9]);
        assert_eq!(log.count(), 10);
        log.clear();
        assert_eq!((log.last().count(), log.count()), (0, 0));
    }

    #[test]
    fn scenario_names_are_unique() {
        for (i, a) in SCENARIOS.iter().enumerate() {
            assert!(SCENARIOS[i + 1..].iter().all(|b| b.name != a.name));
        }
    }
}
