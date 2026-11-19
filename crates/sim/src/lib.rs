//! Deterministic simulator, fault injection, checkers and reference model.

pub mod scheduler;
pub mod storage;

pub use scheduler::{Clock, Scheduler};
pub use storage::{Disk, Fault, FaultKind, Faults, Group};
