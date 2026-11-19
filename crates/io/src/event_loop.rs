//! The event loop of one shard (document 05 section 4, decision 0003).
//!
//! One iteration collects completions, up to a set number, delivers the
//! ticks that are due, then runs ready tasks by scheduling class. Tasks submit new I/O through the
//! same [`Io`], and the backend sends it on at the next [`Io::poll`].
//!
//! # Scheduling
//!
//! Each class has a budget in nanoseconds per iteration. Classes are visited
//! in priority order ([`Class::ALL`]); each runs tasks while it has credit,
//! one budget per iteration. The time a task takes, read through [`Time`](crate::Time), is
//! charged to its class, so a task that overruns leaves its class in debt,
//! repaid out of its next budgets. A class with nothing ready gives its unused
//! credit to the ready classes, again in priority order, so budgets never
//! leave the core idle while work is waiting.
//!
//! What this guarantees: a class that stays ready gets its budget in every
//! iteration, less its debt, which is at most one task. So over a window of
//! iterations it gets at least `budget / sum of budgets` of the time, however
//! often the classes above it are ready: repair and compaction cannot be
//! starved. When every class is ready each gets exactly its budget, and a
//! class alone gets all of it.
//!
//! The loop reads time only through [`Time`](crate::Time), so it runs unchanged on the
//! simulator's virtual clock.

use crate::{Completion, Io};

/// The scheduling classes of a shard, highest priority first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// Prepares, commits and their replication.
    Commit,
    /// Foreground reads.
    Reads,
    /// Repair of the WAL and the grid from peers.
    Repair,
    /// LSM compaction.
    Compaction,
    /// Analytical queries.
    Queries,
    /// New-connection handshakes (document 05 section 5a).
    Handshakes,
}

impl Class {
    /// Every class, in priority order.
    pub const ALL: [Class; 6] = [
        Class::Commit,
        Class::Reads,
        Class::Repair,
        Class::Compaction,
        Class::Queries,
        Class::Handshakes,
    ];
}

/// The work of one shard, as the loop drives it. `complete` and `tick` run
/// outside every class's budget, so they only record what happened and leave
/// the work to a task of the right class.
pub trait Tasks<I: Io> {
    /// Handles one completion.
    fn complete(&mut self, io: &mut I, completion: Completion);
    /// Called once per tick period that has passed; timeouts count ticks.
    fn tick(&mut self, io: &mut I);
    /// Runs one ready task of `class` and returns `true`, or returns `false`
    /// at once when `class` has nothing ready.
    fn run(&mut self, io: &mut I, class: Class) -> bool;
}

/// The scheduling state of one shard's loop. It allocates nothing after
/// [`EventLoop::new`].
pub struct EventLoop {
    /// Nanoseconds per iteration, by class.
    budgets: [u64; 6],
    /// Nanoseconds a class may still run, by class; at most zero between
    /// iterations, and at least minus one task.
    credit: [i64; 6],
    /// Most completions handled per iteration.
    completions: u32,
    tick: u64,
    next_tick: u64,
}

impl EventLoop {
    /// A loop with `budgets` nanoseconds per iteration for each class (in
    /// [`Class::ALL`] order), at most `completions` completions handled per
    /// iteration, and a tick every `tick` nanoseconds, starting at time
    /// `now`. Every budget is above zero, as that is what keeps a class from
    /// starving.
    pub fn new(budgets: [u64; 6], completions: u32, tick: u64, now: u64) -> Self {
        assert!(budgets.iter().all(|&b| b > 0), "every class has a budget");
        let total = budgets.iter().try_fold(0u64, |sum, &b| sum.checked_add(b));
        assert!(
            total.is_some_and(|t| i64::try_from(t).is_ok()),
            "budgets fit in i64"
        );
        assert!(completions > 0 && tick > 0);
        EventLoop {
            budgets,
            credit: [0; 6],
            completions,
            tick,
            next_tick: now.checked_add(tick).expect("tick time fits"),
        }
    }

    /// Runs one iteration.
    pub fn iterate<I: Io, T: Tasks<I>>(&mut self, io: &mut I, tasks: &mut T) {
        // Capped, as a handler may submit I/O that completes before the
        // backend runs dry; what is left waits for the next iteration.
        for _ in 0..self.completions {
            let Some(completion) = io.poll() else { break };
            tasks.complete(io, completion);
        }
        let mut now = io.monotonic();
        while now >= self.next_tick {
            tasks.tick(io);
            self.next_tick += self.tick;
        }
        let mut spare = 0;
        for class in Class::ALL {
            self.credit[class as usize] += self.budgets[class as usize] as i64;
            self.drain(io, tasks, class, &mut now, &mut spare);
        }
        // What idle classes left goes to the ready ones, highest first; it
        // repays debt before it runs anything.
        for class in Class::ALL {
            if spare == 0 {
                break;
            }
            self.credit[class as usize] += spare;
            spare = 0;
            self.drain(io, tasks, class, &mut now, &mut spare);
        }
        assert!(self.credit.iter().all(|&c| c <= 0), "no class keeps credit");
    }

    /// Runs tasks of `class` while it has credit. If it runs out of work
    /// first, its credit moves to `spare`.
    fn drain<I: Io, T: Tasks<I>>(
        &mut self,
        io: &mut I,
        tasks: &mut T,
        class: Class,
        now: &mut u64,
        spare: &mut i64,
    ) {
        let credit = &mut self.credit[class as usize];
        while *credit > 0 {
            if !tasks.run(io, class) {
                *spare += *credit;
                *credit = 0;
                return;
            }
            let after = io.monotonic();
            assert!(after >= *now, "the monotonic clock never goes back");
            *credit -= i64::try_from(after - *now).expect("a task ends within 292 years");
            *now = after;
        }
    }
}
