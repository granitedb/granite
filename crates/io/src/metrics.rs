//! The stall metric of one shard's event loop (document 02 section 1): how
//! long its iterations take, how long each class's tasks run, and how many
//! tasks broke the slice rule. A stalled primary looks exactly like a dead
//! one, so the 200 ms failover budget rests on these numbers.

use crate::event_loop::Class;
use crate::histogram::Histogram;

/// The longest a task may run without yielding, in nanoseconds.
pub const SLICE: u64 = 1_000_000;

/// The longest the p99.9 iteration time of a benchmark run may be, in
/// nanoseconds.
pub const STALL_LIMIT: u64 = 5_000_000;

/// What one shard's loop measured since it started.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metrics {
    /// Iteration times.
    pub iterations: Histogram,
    /// Task run times, by class in [`Class::ALL`] order.
    pub tasks: [Histogram; 6],
    /// Tasks that ran longer than [`SLICE`], by class.
    pub long_tasks: [u64; 6],
}

/// Receives a shard's metrics: the production exporter implements it, and
/// tests read through it.
pub trait Sink {
    /// The histogram `name` of `shard`, and of `class` if it is per class.
    fn histogram(&mut self, shard: u32, class: Option<Class>, name: &'static str, h: &Histogram);
    /// The counter `name` of `shard`, and of `class` if it is per class.
    fn counter(&mut self, shard: u32, class: Option<Class>, name: &'static str, value: u64);
}

impl Metrics {
    /// Hands every value to `sink`, as the metrics of `shard`:
    /// `loop_iteration_ns`, and per class `task_run_ns` and `long_tasks`.
    pub fn export(&self, shard: u32, sink: &mut impl Sink) {
        sink.histogram(shard, None, "loop_iteration_ns", &self.iterations);
        for class in Class::ALL {
            let c = class as usize;
            sink.histogram(shard, Some(class), "task_run_ns", &self.tasks[c]);
            sink.counter(shard, Some(class), "long_tasks", self.long_tasks[c]);
        }
    }

    /// The benchmark harness check: an error carrying the p99.9 iteration
    /// time if it is above [`STALL_LIMIT`].
    pub fn check_stalls(&self) -> Result<(), u64> {
        let p999 = self.iterations.quantile(999_000);
        if p999 > STALL_LIMIT {
            Err(p999)
        } else {
            Ok(())
        }
    }
}
