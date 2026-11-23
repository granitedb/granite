//! Growth decided by the clock and the file system's free space. Rejected in
//! the storage crate, accepted elsewhere.

/// A clock, as `granite_io::Time` is.
pub trait Time {
    fn monotonic(&self) -> u64;
}

pub fn statvfs() -> u64 {
    0
}

pub fn should_grow(clock: &dyn Time, free_blocks: u64) -> bool {
    clock.monotonic() > 0 && statvfs() > free_blocks
}
