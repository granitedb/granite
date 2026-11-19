//! Rejected: hash maps and sets with a random hasher iterate in a different
//! order on every replica.

use std::collections::{HashMap, HashSet};
use std::hash::RandomState;

pub fn count(keys: &[u64]) -> usize {
    let map: HashMap<u64, u64> = keys.iter().map(|&k| (k, k)).collect();
    let set: HashSet<u64> = keys.iter().copied().collect();
    let _state = RandomState::new();
    map.len() + set.len()
}
