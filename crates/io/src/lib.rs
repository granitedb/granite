//! Storage, network and time traits, and their backends.

pub mod conformance;
pub mod event_loop;
pub mod file;
pub mod histogram;
pub mod memory;
pub mod message_pool;
pub mod metrics;
#[cfg(target_os = "linux")]
pub mod pin;
#[cfg(target_os = "macos")]
pub mod portable;
pub mod traits;
#[cfg(target_os = "linux")]
pub mod uring;

pub use traits::{
    CONNECT_TIMEOUT, Completion, Connection, Full, Io, Network, NetworkError, Peer, Random,
    Storage, StorageError, Tag, Time, Zone,
};
