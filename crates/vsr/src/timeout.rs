//! Timeouts of a replica (document 02 sections 0 and 4).
//!
//! A timeout is a deadline in nanoseconds of [`Time::monotonic`], checked on
//! every tick of the event loop; `Time` has no timer. Each consecutive firing
//! doubles the base duration, up to [`BACKOFF_MAX`] doublings, and adds a
//! jitter that is a function of (group, replica, which timeout, attempt)
//! only. So the firing times of a run depend on when the replica started and
//! when it last heard what resets the timeout, never on a random generator,
//! and the replicas of a failed node's groups do not fire together.
//!
//! [`Time::monotonic`]: granite_io::Time::monotonic

/// Most doublings of a timeout's base duration by backoff.
pub const BACKOFF_MAX: u32 = 4;

/// A deadline that, once passed, fires and rearms itself with backoff.
#[derive(Clone, Copy, Debug)]
pub struct Timeout {
    /// Nanoseconds before the first firing, without jitter.
    base: u64,
    /// Largest jitter, in nanoseconds.
    jitter_max: u64,
    /// Derived from (group, replica, which timeout).
    salt: u64,
    /// Firings since the timeout was last started.
    attempts: u32,
    deadline: Option<u64>,
    fired: u64,
}

impl Timeout {
    /// A stopped timeout of `base` nanoseconds plus up to `jitter_max` of
    /// jitter, for timeout `kind` of `replica` in `group`.
    pub fn new(base: u64, jitter_max: u64, group: u32, replica: u8, kind: u8) -> Self {
        assert!(base > 0);
        assert!(
            base.checked_shl(BACKOFF_MAX)
                .is_some_and(|b| b >> BACKOFF_MAX == base && b.checked_add(jitter_max).is_some()),
            "the longest backoff fits in u64"
        );
        let salt = u64::from(group) << 16 | u64::from(replica) << 8 | u64::from(kind);
        Timeout {
            base,
            jitter_max,
            salt,
            attempts: 0,
            deadline: None,
            fired: 0,
        }
    }

    /// Arms the timeout from `now`, ending any backoff.
    pub fn start(&mut self, now: u64) {
        self.attempts = 0;
        self.deadline = Some(
            now.checked_add(self.duration())
                .expect("deadline fits in u64"),
        );
    }

    pub fn stop(&mut self) {
        self.deadline = None;
    }

    /// Whether the deadline passed by `now`; if so the timeout counts the
    /// firing and rearms from `now` with one more step of backoff.
    pub fn fire(&mut self, now: u64) -> bool {
        if self.deadline.is_none_or(|d| d > now) {
            return false;
        }
        self.attempts = self.attempts.saturating_add(1);
        self.fired += 1;
        self.deadline = Some(
            now.checked_add(self.duration())
                .expect("deadline fits in u64"),
        );
        true
    }

    /// Nanoseconds from arming to firing at the current attempt.
    pub fn duration(&self) -> u64 {
        let doublings = self.attempts.min(BACKOFF_MAX);
        let jitter = jitter(self.salt, self.attempts) % (self.jitter_max + 1);
        (self.base << doublings) + jitter
    }

    pub fn deadline(&self) -> Option<u64> {
        self.deadline
    }

    /// Firings since the timeout was made.
    pub fn fired(&self) -> u64 {
        self.fired
    }
}

/// 64 bits mixed from `salt` and `attempt`: the SplitMix64 finaliser, which
/// spreads any one-bit change over every output bit.
fn jitter(salt: u64, attempt: u32) -> u64 {
    let mut z = (salt ^ u64::from(attempt) << 32).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const MS: u64 = 1_000_000;

    #[test]
    fn jitter_differs_between_replicas_and_between_groups() {
        let mut seen = BTreeSet::new();
        for group in 0..64 {
            for replica in 0..6 {
                let t = Timeout::new(60 * MS, 20 * MS, group, replica, 1);
                assert!(t.duration() >= 60 * MS && t.duration() <= 80 * MS);
                seen.insert(t.duration());
            }
        }
        assert_eq!(
            seen.len(),
            64 * 6,
            "every (group, replica) has its own jitter"
        );
    }

    #[test]
    fn it_fires_once_per_deadline_and_backs_off() {
        let mut t = Timeout::new(10, 0, 0, 0, 0);
        assert!(!t.fire(1_000), "a stopped timeout never fires");
        t.start(0);
        assert!(!t.fire(9));
        assert!(t.fire(10));
        assert!(!t.fire(10));
        let mut at = 10;
        for doublings in 1..=BACKOFF_MAX + 2 {
            at += 10 << doublings.min(BACKOFF_MAX);
            assert!(!t.fire(at - 1));
            assert!(t.fire(at));
        }
        assert_eq!(t.fired(), u64::from(BACKOFF_MAX) + 3);
        t.start(at);
        assert_eq!(
            t.deadline(),
            Some(at + 10),
            "starting again ends the backoff"
        );
        t.stop();
        assert!(!t.fire(u64::MAX));
    }

    #[test]
    fn the_same_inputs_give_the_same_durations() {
        let durations = |group| {
            let mut t = Timeout::new(60 * MS, 20 * MS, group, 2, 1);
            t.start(0);
            (0..8)
                .map(|_| {
                    let d = t.deadline().unwrap();
                    assert!(t.fire(d));
                    d
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(durations(7), durations(7));
        assert_ne!(durations(7), durations(8));
    }
}
