//! Rejected: reading the clock outside the io crate.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn now() -> u64 {
    let start = Instant::now();
    let wall = SystemTime::now();
    let _ = (start, wall);
    UNIX_EPOCH.elapsed().map_or(0, |d: Duration| d.as_secs())
}
