//! Random programs over the test catalog, run by the reference evaluator of
//! `support` on random parameters and random state:
//! - every program the validator accepts runs without a type error (the
//!   evaluator checks the type of every value at every operation and panics
//!   on a mismatch);
//! - no invocation reads or writes more rows than the validator's bound, so
//!   the bound is exact or an upper bound, never lower.
//!
//! One expression in 20 is generated at a random type, so that many
//! programs are invalid and the validator's rejections are exercised too.
//!
//! `just test` runs 20,000 executions. The gate count of T3.3.2, 1,000,000,
//! runs in release with
//! `GRANITE_FULL=1 cargo test --release --locked -p granite-txn --test execution -- --nocapture`.
//! Rerun a failure with `GRANITE_SEED=<seed>`.

mod support;

use granite_base::constants::FORMAT;
use granite_types::Value;
use std::collections::BTreeMap;
use support::*;

#[test]
fn accepted_programs_run_without_type_errors_within_their_bounds() {
    let mut rng = seeded("accepted_programs_run_without_type_errors_within_their_bounds");
    let catalog = catalog();
    let target: u64 = if full() { 1_000_000 } else { 20_000 };
    let (mut executions, mut programs, mut committed) = (0u64, 0u64, 0u64);
    let (mut reached, mut accessing) = (0u64, 0u64);
    let mut rejected: BTreeMap<String, u64> = BTreeMap::new();
    while executions < target {
        let program = Gen::new(&mut rng, &catalog, 20).program();
        let report = match program.validate(&catalog) {
            Ok(r) => r,
            Err(e) => {
                *rejected.entry(format!("{:?}", e.error)).or_default() += 1;
                continue;
            }
        };
        programs += 1;
        let mut state = state(&mut rng, &catalog);
        let mut most = (0, 0);
        for _ in 0..4 {
            let params: Vec<Value> = (program.params.iter())
                .map(|p| value(&mut rng, &p.ty))
                .collect();
            let commit_ts = rng.next();
            let run = run(&catalog, &program, &report, &params, commit_ts, &mut state);
            let context = || format!("{program:?}\n{params:?}\n{report:?}\n{run:?}");
            assert!(
                run.reads <= report.reads,
                "reads over the bound: {}",
                context()
            );
            assert!(
                run.writes <= report.writes,
                "writes over the bound: {}",
                context()
            );
            assert!(run.reads + run.writes <= FORMAT.program_accesses_max);
            most = (most.0.max(run.reads), most.1.max(run.writes));
            committed += u64::from(run.result.is_ok());
            executions += 1;
        }
        if report.reads + report.writes > 0 {
            accessing += 1;
            reached += u64::from(most == (report.reads, report.writes));
        }
    }
    println!("{executions} executions of {programs} accepted programs, {committed} committed");
    println!("{reached} of {accessing} programs with accesses reached both bounds exactly");
    println!("rejected: {rejected:?}");
    assert!(committed > executions / 10 && reached > accessing / 10);
    for code in ["TypeMismatch", "ImplicitCast", "NullableOperand"] {
        assert!(
            rejected.contains_key(code),
            "the generator exercises the validator's {code}"
        );
    }
}
