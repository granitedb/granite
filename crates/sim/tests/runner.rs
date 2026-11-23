//! The gates of T0.4.5 that the seed runner shows by itself: its exit code,
//! a report with a reproduction command for a failed checker, a panic, a
//! failed assertion and a hang, the same failing seeds on one core and on
//! several, a swarm configuration that is a function of the seed, and the
//! seed list.

use std::process::Command;

use granite_sim::counter;

/// Runs the runner binary; returns whether it passed, and what it printed.
fn sim(args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sim"))
        .args(args)
        .output()
        .expect("the runner starts");
    let text = String::from_utf8(out.stdout).expect("utf-8");
    (out.status.code(), text)
}

/// The failing seeds, from the summary line.
fn failed(out: &str) -> String {
    let summary = out.lines().last().expect("a summary");
    summary
        .split_once("failed")
        .expect("a summary")
        .1
        .to_owned()
}

#[test]
fn exits_zero_only_if_every_seed_passes() {
    let (code, out) = sim(&["--range", "0", "20"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.ends_with("20 seeds, 0 failed\n"), "{out}");
    // Seed 13 of the injected bug fails, the others of 10..20 pass.
    let (code, out) = sim(&["--scenario", "counter-lost-write", "--range", "10", "10"]);
    assert_eq!(code, Some(1), "{out}");
    assert_eq!(failed(&out), ": 13");
    let (code, out) = sim(&["--range", "0"]);
    assert_eq!(code, Some(2), "{out}");
    let (code, out) = sim(&["--range", "0", "0"]);
    assert_eq!(code, Some(2), "no seed ran: {out}");
}

#[test]
fn a_failed_checker_is_reported_with_its_reproduction() {
    let (code, out) = sim(&["--scenario", "counter-lost-write", "--seed", "13"]);
    assert_eq!(code, Some(1));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "FAILED seed 13 of scenario counter-lost-write");
    assert!(lines[1].starts_with("commit: "));
    assert!(lines[2].starts_with("checker: counter: never goes back"));
    assert!(lines[3].starts_with("message: at "), "{out}");
    let swarm = format!(
        "swarm: {:?}",
        counter::Config {
            skip_durable_write: true,
            ..counter::swarm(13)
        }
    );
    assert_eq!(lines[5], swarm);
    assert_eq!(lines[6], "last 32 events:");
    assert!(lines[7..39].iter().all(|l| l.starts_with("  #")));
    let reproduce = lines[39].strip_prefix("reproduce: ").expect("a command");
    assert_eq!(
        reproduce,
        "cargo run --profile sim --locked -p granite-sim --bin sim -- --scenario counter-lost-write --seed 13"
    );
    // The command's arguments give the same report.
    let args: Vec<&str> = reproduce.split(" -- ").nth(1).unwrap().split(' ').collect();
    assert_eq!(sim(&args), (code, out));
}

/// Runs seeds 0, 1 and 2 of a fixture, of which seed 0 fails, and checks
/// the report.
fn fixture(scenario: &str, checker: &str, message: &str) {
    let (code, out) = sim(&["--scenario", scenario, "--range", "0", "3"]);
    assert_eq!(code, Some(1), "{out}");
    assert_eq!(failed(&out), ": 0");
    assert!(out.starts_with(&format!("FAILED seed 0 of scenario {scenario}\n")));
    assert!(out.contains(&format!("\nchecker: {checker}\n")), "{out}");
    assert!(out.contains(&format!("\nmessage: {message}")), "{out}");
    let command = format!("--bin sim -- --scenario {scenario} --seed 0\n");
    assert!(out.contains(&command), "{out}");
}

#[test]
fn a_panic_is_a_failure() {
    fixture("fixture-panic", "panic", "fixture panic at seed 0\n");
}

#[test]
fn a_failed_assertion_is_a_failure() {
    fixture(
        "fixture-assertion",
        "panic",
        "fixture assertion at seed 0\n",
    );
}

#[test]
fn a_hang_is_a_failure() {
    fixture(
        "fixture-hang",
        "hang",
        "no progress in virtual time: 1000000 events at 0 ns",
    );
}

#[test]
fn one_core_and_four_fail_the_same_seeds() {
    let range = ["--scenario", "counter-lost-write", "--range", "0", "100"];
    let (_, one) = sim(&[&range[..], &["--jobs", "1"]].concat());
    let (_, four) = sim(&[&range[..], &["--jobs", "4"]].concat());
    assert_eq!(failed(&one), ": 13 39 46 88 95");
    assert_eq!(one, four);
}

#[test]
fn the_swarm_is_a_function_of_the_seed() {
    let configs: Vec<String> = (0..20)
        .map(|seed| format!("{:?}", counter::swarm(seed)))
        .collect();
    for (seed, config) in (0..).zip(&configs) {
        assert_eq!(*config, format!("{:?}", counter::swarm(seed)));
    }
    let mut distinct = configs.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        configs.len(),
        "seeds turn on different faults"
    );
}

#[test]
fn a_seed_list_takes_seeds_ranges_and_comments() {
    let path = format!("{}/seeds.txt", env!("CARGO_TARGET_TMPDIR"));
    std::fs::write(&path, "# comment\n\n10..14 # a range\n13\n 2 \n").unwrap();
    let (code, out) = sim(&["--scenario", "counter-lost-write", "--list", &path]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.ends_with("5 seeds, 1 failed: 13\n"), "{out}");
    std::fs::write(&path, "1..x\n").unwrap();
    let (code, _) = sim(&["--list", &path]);
    assert_eq!(code, Some(2));
}

#[test]
fn the_fixed_seed_list_parses() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/ci-seeds.txt");
    let (code, out) = sim(&["--scenario", "fixture-panic", "--list", path, "--jobs", "4"]);
    // Seeds 0 to 9,999 at least; seed 0 is one of the fixture's failures.
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("FAILED seed 0 of scenario fixture-panic"));
}

#[test]
fn only_the_lowest_100_failures_are_reported() {
    let (code, out) = sim(&["--scenario", "fixture-panic", "--range", "0", "400"]);
    assert_eq!(code, Some(1));
    assert_eq!(out.matches("FAILED seed").count(), 100);
    let seeds: Vec<String> = (0..100).map(|i| (3 * i).to_string()).collect();
    let summary = format!("400 seeds, 134 failed: {} ...\n", seeds.join(" "));
    assert!(out.ends_with(&summary), "{out}");
}
