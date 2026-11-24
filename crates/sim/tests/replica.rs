//! Gates of T2.2.1 and T2.2.2 on the simulator. The seed gates themselves
//! are the `replica` scenario of the seed runner.

use granite_io::message_pool::MessagePool;
use granite_sim::replica::{Faults, POOL, REQUESTS, ids, run, violation};
use granite_sim::runner::Log;

/// Timeout firing times are a function of the seed and of (group,
/// replica) only.
#[test]
fn the_same_seed_gives_the_same_firing_times() {
    let pool = MessagePool::new(POOL);
    let none = Faults::NONE;
    for seed in 0..8 {
        let (cluster, group) = ids(seed);
        for replicas in [3, 5] {
            let a = run(
                seed,
                replicas,
                cluster,
                group,
                none,
                &pool,
                &mut Log::new(0),
            );
            let b = run(
                seed,
                replicas,
                cluster,
                group,
                none,
                &pool,
                &mut Log::new(0),
            );
            assert_eq!(violation(&a, none), None);
            assert_eq!(a, b, "seed {seed}");
            // The cluster id is in every message, but not in any timeout.
            let other = run(
                seed,
                replicas,
                cluster ^ 1,
                group,
                none,
                &pool,
                &mut Log::new(0),
            );
            assert_eq!(other.firings, a.firings, "seed {seed}");
            // The group is in the jitter, so the backups' suspicion
            // deadlines move, though none fires.
            let moved = run(
                seed,
                replicas,
                cluster,
                group ^ 1,
                none,
                &pool,
                &mut Log::new(0),
            );
            assert_eq!(moved.fired, a.fired);
            assert_ne!(moved.firings, a.firings, "seed {seed}");
            assert_eq!(pool.counters().in_use, 0);
        }
    }
}

/// The load fills the pipeline, and the primary refuses what overflows its
/// queue, in every run with no faults.
#[test]
fn the_load_fills_the_pipeline_and_overflows_the_queue() {
    let pool = MessagePool::new(POOL);
    for seed in 0..8 {
        let (cluster, group) = ids(seed);
        let outcome = run(
            seed,
            3,
            cluster,
            group,
            Faults::NONE,
            &pool,
            &mut Log::new(0),
        );
        assert_eq!(violation(&outcome, Faults::NONE), None);
        assert_eq!(outcome.pipeline_max, 8, "seed {seed}");
        assert!(outcome.busy > 0, "seed {seed}");
        assert_eq!(outcome.requests, REQUESTS);
    }
}

/// With one backup of three or of five cut off, the others still commit
/// every request; with every backup of three cut off, the primary executes
/// nothing. The primary's assertions check, at each execution and reply,
/// that a replication quorum holds the op.
#[test]
fn the_primary_executes_only_what_a_replication_quorum_holds() {
    let pool = MessagePool::new(POOL);
    for seed in 0..8 {
        let (cluster, group) = ids(seed);
        for (replicas, cut_off) in [(3, 0b100), (5, 0b10000), (3, 0b110)] {
            let faults = Faults {
                cut_off,
                ..Faults::NONE
            };
            let outcome = run(
                seed,
                replicas,
                cluster,
                group,
                faults,
                &pool,
                &mut Log::new(0),
            );
            assert_eq!(violation(&outcome, faults), None);
            for (r, executed) in outcome.executed.iter().enumerate() {
                let expected = if cut_off & 1 << r != 0 || cut_off == 0b110 {
                    0
                } else {
                    REQUESTS as usize
                };
                assert_eq!(executed.len(), expected, "seed {seed}, replica {r}");
            }
            assert_eq!(pool.counters().in_use, 0);
        }
    }
}

/// A short run of every configuration of the `replica` scenario, so that
/// `cargo test` covers what the seed gate runs at length.
#[test]
fn a_few_seeds_of_every_configuration_hold() {
    let pool = MessagePool::new(POOL);
    let mut crashes = 0;
    for seed in 0..8 {
        let (cluster, group) = ids(seed);
        for (replicas, faults, name) in granite_sim::replica::RUNS {
            let outcome = run(
                seed,
                replicas,
                cluster,
                group,
                faults,
                &pool,
                &mut Log::new(0),
            );
            assert_eq!(violation(&outcome, faults), None, "seed {seed}, {name}");
            crashes += outcome.crashed.len();
            assert_eq!(pool.counters().in_use, 0);
        }
    }
    assert!(crashes > 0, "the crash runs crash");
}

/// Under message faults the primary still executes every request: it sends
/// each prepare again on its heartbeat until a replication quorum holds it.
#[test]
fn lost_prepares_are_sent_again() {
    let pool = MessagePool::new(POOL);
    for seed in 0..8 {
        let (cluster, group) = ids(seed);
        let faults = Faults::MESSAGES;
        let outcome = run(seed, 3, cluster, group, faults, &pool, &mut Log::new(0));
        assert_eq!(violation(&outcome, faults), None);
        assert_eq!(outcome.executed[0].len() as u64, REQUESTS, "seed {seed}");
    }
}
