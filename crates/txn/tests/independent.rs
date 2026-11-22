//! Soundness of the classification (document 03 section 5.4): no invocation
//! of a program classified independent fails, on random parameters and
//! random state, under the fresh-id rule (an invocation inserts into an
//! immutable table only at keys that hold no row). The full run is
//! `GRANITE_FULL=1 cargo test --release --locked -p granite-txn --test independent -- --nocapture`.

mod support;

use granite_txn::{Class, Classification};
use granite_types::Value;
use std::collections::BTreeSet;
use support::*;

#[test]
fn independent_programs_never_fail() {
    let mut rng = seeded("independent_programs_never_fail");
    let catalog = catalog();
    let target: u64 = if full() { 1_000_000 } else { 5_000 };
    let (mut runs, mut generated, mut reused, mut writes) = (0, 0u64, 0u64, 0u64);
    while runs < target {
        generated += 1;
        // About 1 in 15 generated programs is independent.
        assert!(
            generated <= 100 * target,
            "too few programs are independent"
        );
        // Mostly programs without fallible elements, so that enough are
        // independent; the rest from the full generator.
        let safe = rng.below(8) != 0;
        let mut g = Gen::new(&mut rng, &catalog, u64::MAX);
        g.safe = safe;
        let p = g.program();
        let Ok(report) = p.validate(&catalog) else {
            continue;
        };
        let Ok(Classification { class, partitions }) = p.classify(&catalog, &report) else {
            continue;
        };
        assert!(
            !(safe && matches!(class, Class::Coordinated { .. })),
            "nothing can fail, yet {class:?}: {p:?}"
        );
        if class != Class::Independent {
            continue;
        }
        assert!(partitions.len() >= 2);
        let params: Vec<Value> = p.params.iter().map(|q| value(&mut rng, &q.ty)).collect();
        let mut state = state(&mut rng, &catalog);
        let keys = immutable_inserts(&catalog, &p, &report, &params)
            .expect("an independent program evaluates its keys without failing");
        if keys.iter().collect::<BTreeSet<_>>().len() < keys.len() {
            // The invocation inserts two rows at one key: not a fresh id each.
            reused += 1;
            continue;
        }
        for (table, key) in &keys {
            state.entry(*table).or_default().remove(key);
        }
        let run = run(&catalog, &p, &report, &params, rng.next(), &mut state);
        assert!(
            run.result.is_ok(),
            "an independent program failed with {:?}: {p:?} on {params:?}",
            run.result
        );
        writes += run.writes;
        runs += 1;
    }
    println!(
        "{runs} independent programs ran without failing, {writes} writes; {generated} generated, {reused} skipped for reusing a key"
    );
    assert!(writes >= runs, "the programs write");
}
