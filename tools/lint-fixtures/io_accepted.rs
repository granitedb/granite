//! Accepted: the io crate may use every API the other crates may not.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

thread_local! {
    static COUNTER: u64 = const { 0 };
}

#[repr(C)]
pub struct Iovec {
    pub base: *const u8,
    pub len: usize,
}

pub fn everything(path: &Path, address: SocketAddr, x: f64) -> f64 {
    let map: HashMap<u64, u64> = HashMap::new();
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(1));
    let read = std::fs::read(path).map_or(0, |b| b.len());
    let connected = TcpStream::connect(address).is_ok();
    let counter = COUNTER.with(|c| *c);
    let n = map.len() + read + usize::from(connected);
    x.mul_add(start.elapsed().as_secs_f64(), n as f64 + counter as f64)
}
