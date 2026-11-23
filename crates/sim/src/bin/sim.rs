//! The seed runner: runs one seed, a range, a list from a file, or random
//! seeds for a while, of one scenario, in parallel across cores, and prints
//! every failure with the command that reproduces it. Exits with 1 if any
//! seed failed, 2 on a usage error or when no seed ran, and 0 only if every
//! seed passed.
//!
//! Each seed runs on one thread from start to end, so a run stays a function
//! of its seed; the threads only share out the seeds.

use std::collections::BTreeMap;
use std::process::{Command, ExitCode};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use granite_io::memory::SplitMix64;
use granite_io::message_pool::MessagePool;
use granite_sim::runner::{self, Log, SCENARIOS, Scenario};

const USAGE: &str = "usage: sim [--scenario NAME] [--jobs N] \
(--seed SEED [--trace] | --range FIRST COUNT | --list FILE | --random SECONDS)";

/// Events printed before a failure.
const LAST: usize = 32;
/// Failures reported in full, those of the lowest seeds; the others are
/// only counted.
const REPORTS: usize = 100;

enum Seeds {
    One(u64),
    Range(u64, u64),
    List(Vec<u64>),
    Random { start: u64, seconds: u64 },
}

struct Args {
    scenario: &'static Scenario,
    jobs: u64,
    trace: bool,
    seeds: Seeds,
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}\n{USAGE}\nscenarios: {}", names());
            return ExitCode::from(2);
        }
    };
    let commit = commit();
    let scenario = args.scenario;
    if let (true, Seeds::One(seed)) = (args.trace, &args.seeds) {
        let mut log = Log::printing();
        let pool = MessagePool::new(runner::POOL);
        return match runner::run(scenario, *seed, &mut log, &pool) {
            Ok(()) => {
                println!("seed {seed} of scenario {} passed", scenario.name);
                ExitCode::SUCCESS
            }
            Err(failure) => {
                print!(
                    "{}",
                    runner::report(scenario, *seed, &commit, &failure, &log)
                );
                ExitCode::FAILURE
            }
        };
    }
    if let Seeds::Random { start, seconds } = args.seeds {
        println!("random seeds for {seconds} s, drawn from start {start}");
    }
    let next = AtomicU64::new(0);
    let ran = AtomicU64::new(0);
    let failed = AtomicU64::new(0);
    let reports = Mutex::new(BTreeMap::new());
    let deadline = deadline(&args.seeds);
    let work = || {
        let mut log = Log::new(LAST);
        let pool = MessagePool::new(runner::POOL);
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(seed) = seed_at(&args.seeds, i, deadline) else {
                break;
            };
            log.clear();
            if let Err(failure) = runner::run(scenario, seed, &mut log, &pool) {
                failed.fetch_add(1, Ordering::Relaxed);
                let report = runner::report(scenario, seed, &commit, &failure, &log);
                let mut reports = reports.lock().expect("no worker panics");
                reports.insert(seed, report);
                if reports.len() > REPORTS {
                    reports.pop_last();
                }
            }
            ran.fetch_add(1, Ordering::Relaxed);
        }
    };
    spawn(args.jobs, &work);
    let reports = reports.into_inner().expect("no worker panics");
    for report in reports.values() {
        println!("{report}");
    }
    let (ran, failed) = (ran.into_inner(), failed.into_inner());
    let seeds: Vec<String> = reports.keys().map(u64::to_string).collect();
    let more = if failed > reports.len() as u64 {
        " ..."
    } else {
        ""
    };
    println!(
        "scenario {}, commit {commit}: {ran} seeds, {failed} failed{}{}{more}",
        scenario.name,
        if failed == 0 { "" } else { ": " },
        seeds.join(" ")
    );
    match (ran, failed) {
        // An empty list or range passes nothing.
        (0, _) => ExitCode::from(2),
        (_, 0) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

fn names() -> String {
    let names: Vec<&str> = SCENARIOS.iter().map(|s| s.name).collect();
    names.join(", ")
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut scenario = &SCENARIOS[0];
    let mut jobs = None;
    let mut trace = false;
    let mut seeds = None;
    let number = |args: &mut dyn Iterator<Item = String>, what: &str| {
        let arg = args.next().ok_or(format!("{what} needs a number"))?;
        arg.parse::<u64>()
            .map_err(|_| format!("{what}: {arg} is not a number"))
    };
    while let Some(arg) = args.next() {
        let one = match arg.as_str() {
            "--scenario" => {
                let name = args.next().unwrap_or_default();
                scenario = runner::scenario(&name).ok_or(format!("no scenario {name:?}"))?;
                None
            }
            "--jobs" => {
                jobs = Some(number(&mut args, "--jobs")?.max(1));
                None
            }
            "--trace" => {
                trace = true;
                None
            }
            "--seed" => Some(Seeds::One(number(&mut args, "--seed")?)),
            "--range" => {
                let first = number(&mut args, "--range")?;
                let count = number(&mut args, "--range")?;
                first
                    .checked_add(count)
                    .ok_or("--range: the range passes the last seed")?;
                Some(Seeds::Range(first, count))
            }
            "--list" => {
                let path = args.next().ok_or("--list needs a file")?;
                Some(Seeds::List(list(&path)?))
            }
            "--random" => Some(Seeds::Random {
                start: random_start(),
                seconds: number(&mut args, "--random")?,
            }),
            _ => return Err(format!("unknown argument {arg:?}")),
        };
        if one.is_some() && seeds.replace(one).is_some() {
            return Err("give one of --seed, --range, --list and --random".into());
        }
    }
    let seeds = seeds.flatten().ok_or("no seeds given")?;
    if trace && !matches!(seeds, Seeds::One(_)) {
        return Err("--trace needs --seed".into());
    }
    Ok(Args {
        scenario,
        jobs: jobs.unwrap_or_else(cores),
        trace,
        seeds,
    })
}

/// A seed list: one seed or one range `FIRST..END` (END excluded) per line,
/// with `#` comments; each seed runs once, in order.
#[expect(
    clippy::disallowed_methods,
    reason = "the runner reads its seed list; no simulated code runs here"
)]
fn list(path: &str) -> Result<Vec<u64>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let mut seeds = Vec::new();
    for (number, line) in (1..).zip(text.lines()) {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let bad = || format!("{path}:{number}: {line:?} is neither a seed nor FIRST..END");
        let seed = |s: &str| s.trim().parse::<u64>().map_err(|_| bad());
        match line.split_once("..") {
            Some((first, end)) => seeds.extend(seed(first)?..seed(end)?),
            None => seeds.push(seed(line)?),
        }
    }
    seeds.sort_unstable();
    seeds.dedup();
    Ok(seeds)
}

/// The seed numbered `i` of a run, or `None` once the seeds are done.
fn seed_at(seeds: &Seeds, i: u64, deadline: Option<Instant>) -> Option<u64> {
    match seeds {
        Seeds::One(seed) => (i == 0).then_some(*seed),
        Seeds::Range(first, count) => (i < *count).then(|| first + i),
        Seeds::List(list) => list.get(usize::try_from(i).ok()?).copied(),
        Seeds::Random { start, .. } => {
            let deadline = deadline.expect("random seeds have a deadline");
            (now() < deadline).then(|| SplitMix64(start ^ i).next_u64())
        }
    }
}

// The runner is a tool around the simulator, not simulated code: it reads
// the clock, the machine's cores and git, and starts threads.
#[expect(clippy::disallowed_types, reason = "see above")]
type Instant = std::time::Instant;

#[expect(clippy::disallowed_methods, reason = "see above")]
fn now() -> Instant {
    Instant::now()
}

fn deadline(seeds: &Seeds) -> Option<Instant> {
    match seeds {
        Seeds::Random { seconds, .. } => {
            #[expect(clippy::disallowed_types, reason = "see above")]
            let duration = std::time::Duration::from_secs(*seconds);
            Some(now() + duration)
        }
        _ => None,
    }
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "see above"
)]
fn random_start() -> u64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    SplitMix64(since.as_nanos() as u64).next_u64()
}

#[expect(clippy::disallowed_methods, reason = "see above")]
fn cores() -> u64 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u64)
}

#[expect(clippy::disallowed_methods, reason = "see above")]
fn spawn(jobs: u64, work: &(dyn Fn() + Sync)) {
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(work);
        }
    });
}

/// The commit the runner was started at, marked `-dirty` with uncommitted
/// changes; `unknown` outside a git checkout.
fn commit() -> String {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    match (
        git(&["rev-parse", "HEAD"]),
        git(&["status", "--porcelain", "--untracked-files=no"]),
    ) {
        (Some(hash), Some(changes)) if changes.is_empty() => hash,
        (Some(hash), Some(_)) => format!("{hash}-dirty"),
        _ => "unknown".into(),
    }
}
