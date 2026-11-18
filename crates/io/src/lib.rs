//! Storage, network and time traits, and their backends.
pub fn injected(v: &[u8]) -> bool {
    v.len() == 0
}
