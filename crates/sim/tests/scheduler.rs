//! The gates of T0.4.1: reproducible traces within and across processes and
//! platforms, a fixed order for simultaneous events, idle time skipped, and
//! traces that depend on the seed.

use std::process::Command;
use std::time::Instant;

use granite_io::{Full, Random, Time};
use granite_sim::Scheduler;
use granite_sim::scheduler::REALTIME_START;

const MILLISECOND: u64 = 1_000_000;
const SECOND: u64 = 1_000 * MILLISECOND;
const HOUR: u64 = 3_600 * SECOND;
const TICK: u64 = 10 * MILLISECOND;
const NODES: u8 = 5;

#[derive(Debug, Hash)]
enum Event {
    Tick { node: u8 },
    Message { from: u8, to: u8, id: u32 },
}

/// A small cluster that gossips: on each tick a node may send a message to
/// a random peer after a random delay (zero included, so messages tie with
/// ticks), and a node that receives one may answer. One second of virtual
/// time; returns the trace hash.
fn scenario(seed: u64) -> u64 {
    let mut sim = Scheduler::new(seed, 1_000);
    for node in 0..NODES {
        sim.schedule(TICK, Event::Tick { node }).unwrap();
    }
    let mut messages = 0u32;
    while let Some(event) = sim.step() {
        let now = sim.now();
        let (from, chance) = match event {
            Event::Tick { node } => {
                if now + TICK <= SECOND {
                    sim.schedule(now + TICK, Event::Tick { node }).unwrap();
                }
                (node, 3)
            }
            Event::Message { to, .. } => (to, 2),
        };
        if sim.next_u64().is_multiple_of(chance) {
            let to = (from + 1 + (sim.next_u64() % u64::from(NODES - 1)) as u8) % NODES;
            let delay = sim.next_u64() % (5 * TICK / MILLISECOND) * MILLISECOND;
            messages += 1;
            let message = Event::Message {
                from,
                to,
                id: messages,
            };
            // A full queue drops the message, as a congested network would.
            let _ = sim.schedule(now + delay, message);
        }
    }
    sim.trace()
}

#[test]
fn same_seed_same_trace_in_one_process() {
    for seed in 0..1_000 {
        assert_eq!(scenario(seed), scenario(seed), "seed {seed}");
    }
}

/// Prints the trace hash of seeds 0 to 999 when run as a child of
/// `same_seed_same_trace_in_separate_processes`; does nothing otherwise.
#[test]
fn print_traces_for_parent() {
    if std::env::var_os("GRANITE_SIM_PRINT_TRACES").is_none() {
        return;
    }
    for seed in 0..1_000 {
        println!("trace {seed} {:016x}", scenario(seed));
    }
}

fn traces_in_child_process() -> Vec<String> {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "print_traces_for_parent", "--nocapture"])
        .env("GRANITE_SIM_PRINT_TRACES", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    stdout
        .lines()
        .filter(|line| line.starts_with("trace "))
        .map(str::to_owned)
        .collect()
}

#[test]
fn same_seed_same_trace_in_separate_processes() {
    let expected: Vec<String> = (0..1_000)
        .map(|seed| format!("trace {seed} {:016x}", scenario(seed)))
        .collect();
    assert_eq!(traces_in_child_process(), expected);
    assert_eq!(traces_in_child_process(), expected);
}

/// Checked on Linux x86-64 and macOS ARM by CI.
#[test]
fn trace_hashes_are_the_same_on_every_platform() {
    let expected: [u64; 10] = [
        0xc800d23755769c14,
        0x4ddde713d4b04a85,
        0x026d6f0c121dd206,
        0xf59962f20f360f17,
        0xe26aa7359aaa0163,
        0x68b3a513bc46b6e3,
        0xa3307c44bb348eed,
        0xb19a61371cb701a1,
        0x1791ef22a4e7ef11,
        0x16f2a56156972c3e,
    ];
    let actual: Vec<u64> = (0..10).map(scenario).collect();
    assert_eq!(actual, expected);
}

#[test]
fn changing_the_seed_changes_the_trace() {
    for seed in 0..100 {
        assert_ne!(
            scenario(seed),
            scenario(seed + 100),
            "seeds {seed} and {}",
            seed + 100
        );
    }
}

#[test]
fn simultaneous_events_run_in_scheduling_order() {
    const SIMULTANEOUS: u32 = 10_000;
    let mut sim = Scheduler::new(0, 3 * SIMULTANEOUS as usize);
    // Earlier and later events in between, so the heap reorders a lot.
    for label in 0..SIMULTANEOUS {
        sim.schedule(7, SIMULTANEOUS + label).unwrap();
        sim.schedule(5, label).unwrap();
        sim.schedule(3, 2 * SIMULTANEOUS + label).unwrap();
    }
    let mut ran = Vec::new();
    while let Some(label) = sim.step() {
        ran.push((sim.now(), label));
    }
    let expected: Vec<(u64, u32)> = (0..SIMULTANEOUS)
        .map(|label| (3, 2 * SIMULTANEOUS + label))
        .chain((0..SIMULTANEOUS).map(|label| (5, label)))
        .chain((0..SIMULTANEOUS).map(|label| (7, SIMULTANEOUS + label)))
        .collect();
    assert_eq!(ran, expected);
}

#[test]
fn an_idle_hour_takes_under_a_second() {
    let start = Instant::now();
    let mut sim = Scheduler::new(0, usize::from(NODES));
    for node in 0..NODES {
        sim.schedule(TICK, Event::Tick { node }).unwrap();
    }
    let mut ticks = 0u64;
    while let Some(Event::Tick { node }) = sim.step() {
        ticks += 1;
        if sim.now() < HOUR {
            sim.schedule(sim.now() + TICK, Event::Tick { node })
                .unwrap();
        }
    }
    let elapsed = start.elapsed();
    assert_eq!(sim.now(), HOUR);
    assert_eq!(ticks, HOUR / TICK * u64::from(NODES));
    eprintln!("idle hour: {elapsed:?}");
    // The time is checked on the optimised build, which `just test-release` runs with
    // GRANITE_PROFILE=release (about 30 ms). An unoptimised build takes 0.7 s idle and
    // crossed one second on loaded runners.
    if std::env::var_os("GRANITE_PROFILE").is_some_and(|p| p == "release") {
        assert!(elapsed.as_secs_f64() < 1.0, "{elapsed:?}");
    }
}

#[test]
fn the_clock_moves_only_when_an_event_runs() {
    let mut sim = Scheduler::new(0, 2);
    let clock = sim.clock();
    assert_eq!((clock.monotonic(), clock.realtime()), (0, REALTIME_START));
    sim.schedule(HOUR, "later").unwrap();
    sim.schedule(0, "now").unwrap();
    assert_eq!(clock.monotonic(), 0);
    assert_eq!(sim.step(), Some("now"));
    assert_eq!(clock.monotonic(), 0);
    assert_eq!(sim.step(), Some("later"));
    assert_eq!(clock.monotonic(), HOUR);
    assert_eq!(clock.realtime(), REALTIME_START + HOUR as i64);
    assert_eq!(sim.step(), None);
    assert_eq!(clock.monotonic(), HOUR);
}

#[test]
fn a_full_queue_refuses_the_event() {
    let mut sim = Scheduler::new(0, 2);
    sim.schedule(1, 'a').unwrap();
    sim.schedule(2, 'b').unwrap();
    assert!(matches!(sim.schedule(3, 'c'), Err(Full('c'))));
    assert_eq!(sim.pending(), 2);
    assert_eq!(sim.step(), Some('a'));
    sim.schedule(3, 'c').unwrap();
    assert_eq!(sim.pending(), 2);
}

#[test]
#[should_panic(expected = "an event cannot run in the past")]
fn an_event_in_the_past_is_a_bug() {
    let mut sim = Scheduler::new(0, 2);
    sim.schedule(5, ()).unwrap();
    sim.step();
    let _ = sim.schedule(4, ());
}
