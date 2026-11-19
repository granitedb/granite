//! Rejected: starting threads outside the io crate.

thread_local! {
    static COUNTER: u64 = const { 0 };
}

pub fn run() -> u64 {
    std::thread::spawn(|| 1).join().unwrap_or(0) + COUNTER.with(|c| *c)
}
