//! Gates of T2.2.1 on the simulator: timeout firing times are a function of
//! the seed and of (group, replica) only. The seed gate itself is the
//! `replica` scenario of the seed runner.

use granite_io::message_pool::MessagePool;
use granite_sim::replica::{ids, run, violation};
use granite_sim::runner::{Log, POOL};

#[test]
fn the_same_seed_gives_the_same_firing_times() {
    let pool = MessagePool::new(POOL);
    for seed in 0..8 {
        let (cluster, group) = ids(seed);
        for replicas in [3, 5] {
            let a = run(seed, replicas, cluster, group, &pool, &mut Log::new(0));
            let b = run(seed, replicas, cluster, group, &pool, &mut Log::new(0));
            assert_eq!(violation(&a), None);
            assert_eq!(a, b, "seed {seed}");
            // The cluster id is in every message, but not in any timeout.
            let other = run(seed, replicas, cluster ^ 1, group, &pool, &mut Log::new(0));
            assert_eq!(other.firings, a.firings, "seed {seed}");
            // The group is in the jitter, so the backups' suspicion
            // deadlines move, though none fires.
            let moved = run(seed, replicas, cluster, group ^ 1, &pool, &mut Log::new(0));
            assert_eq!(moved.fired, a.fired);
            assert_ne!(moved.firings, a.firings, "seed {seed}");
            assert_eq!(pool.counters().in_use, 0);
        }
    }
}
