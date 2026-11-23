//! The `Io` boundary: the only way code reaches the disk, the network, the
//! clock and randomness (document 06 section 1, decision 0003).
//!
//! # Completion model
//!
//! Storage and network calls only submit an operation. Its result comes back
//! later as one [`Completion`], returned by [`Io::poll`] and carrying the
//! caller's [`Tag`]. Every accepted submission completes exactly once.
//!
//! Which calls complete synchronously:
//! - **Never:** [`Storage::read`], [`Storage::write`], [`Storage::fsync`],
//!   [`Network::connect`], [`Network::accept`], [`Network::send`],
//!   [`Network::recv`]. Their completion is never delivered inside the
//!   submitting call, even when a backend could finish at once, so no handler
//!   runs while its caller is half way through an update, and the simulator
//!   can crash a node between submission and completion. A refusal
//!   ([`Full`]) is synchronous: the operation was not submitted.
//! - **Always:** [`Storage::size`], [`Network::close`], [`Time::monotonic`],
//!   [`Time::realtime`], [`Random::next_u64`] and [`Io::poll`]. They return
//!   at once and never block; `poll` returns `None` when nothing has
//!   completed. `close` completes nothing itself: the operations in flight on
//!   the connection complete later, with [`NetworkError::Closed`] unless
//!   they finished first.
//!
//! Completions come in any order; two in-flight operations are not ordered.
//!
//! # Buffer ownership
//!
//! A buffer is a [`Message`] from the shard's pool. Submitting an operation
//! moves the handle into the backend; its completion (or a [`Full`] refusal)
//! moves it back, whether the operation succeeded or not. While the
//! operation is in flight the caller holds nothing the backend reads or
//! writes, and the backend's handle keeps the memory alive, so dropping a
//! waiting state machine or future cannot free a buffer the kernel still
//! uses.
//! - [`Storage::read`] and [`Network::recv`] write into the buffer, so its
//!   handle must be the only one (asserted at submission).
//! - [`Storage::write`] and [`Network::send`] only read it, so it may be
//!   shared: one prepare can be written to the WAL and sent to every backup
//!   at once. A shared buffer is read-only ([`Message::bytes_mut`]).
//!
//! # Rule for this file
//!
//! No signature here names a type from `std::fs`, `std::net` or `std::time`;
//! `tests/no_std_types.rs` fails if one does. Time is in nanoseconds as
//! integers, and peers and connections are opaque ids.

use crate::message_pool::Message;

/// The caller's identifier of an operation, returned in its completion.
pub type Tag = u64;

/// The fixed zones of a data file (document 01 section 2). An offset is
/// relative to the start of its zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Zone {
    /// The copies of the superblock root record.
    Superblock,
    /// The ring of prepare headers.
    WalHeaders,
    /// The ring of full prepares.
    WalPrepares,
    /// Fixed-size blocks.
    Grid,
}

impl Zone {
    /// Every zone, in file order.
    pub const ALL: [Zone; 4] = [
        Zone::Superblock,
        Zone::WalHeaders,
        Zone::WalPrepares,
        Zone::Grid,
    ];
}

/// A node or service that can be connected to, by its configured id; the
/// backend resolves it to an address from its configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Peer(pub u64);

/// An open connection, as the backend names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Connection(pub u64);

/// A submission refused because the backend already has as many operations
/// in flight as it can hold. It returns what was submitted; the caller
/// retries after a completion, which is how backpressure reaches it.
#[derive(Debug)]
pub struct Full<T>(pub T);

/// The device reported an error for this operation (`EIO` and the like).
///
/// A failed read is repaired from a peer (document 01 section 2). After a
/// failed write or fsync the data's state is unknown, and a later fsync that
/// succeeds does not prove it durable: the kernel may have dropped the dirty
/// pages. Deciding what to do is the caller's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageError;

/// Why a network operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkError {
    /// The peer is unknown, refused the connection, or has too many.
    Refused,
    /// The connection could not be opened in time.
    TimedOut,
    /// The peer closed or reset the connection, or is gone.
    Reset,
    /// This side closed the connection.
    Closed,
}

/// How long a connect may take before it fails with
/// [`NetworkError::TimedOut`], in nanoseconds. Every backend uses it, so a
/// simulated connect times out as a real one does.
pub const CONNECT_TIMEOUT: u64 = 2_000_000_000;

/// The result of one operation.
#[derive(Debug)]
pub enum Completion {
    /// `buffer[..len]` holds the range, if `result` is `Ok`.
    Read {
        tag: Tag,
        buffer: Message,
        result: Result<(), StorageError>,
    },
    /// The write reached the device; it is durable after a later fsync.
    Write {
        tag: Tag,
        buffer: Message,
        result: Result<(), StorageError>,
    },
    /// Every write completed before the fsync was submitted is durable.
    Fsync {
        tag: Tag,
        result: Result<(), StorageError>,
    },
    Connected {
        tag: Tag,
        result: Result<Connection, NetworkError>,
    },
    Accepted {
        tag: Tag,
        result: Result<Connection, NetworkError>,
    },
    /// The backend no longer needs the buffer. `Ok` does not promise that
    /// the peer received the message.
    Sent {
        tag: Tag,
        buffer: Message,
        result: Result<(), NetworkError>,
    },
    /// `Ok(len)`: one whole message is in `buffer[..len]`.
    Received {
        tag: Tag,
        buffer: Message,
        result: Result<u32, NetworkError>,
    },
}

impl Completion {
    /// The tag the operation was submitted with.
    pub fn tag(&self) -> Tag {
        match *self {
            Completion::Read { tag, .. }
            | Completion::Write { tag, .. }
            | Completion::Fsync { tag, .. }
            | Completion::Connected { tag, .. }
            | Completion::Accepted { tag, .. }
            | Completion::Sent { tag, .. }
            | Completion::Received { tag, .. } => tag,
        }
    }
}

/// The data file, addressed by zone (document 01 section 2).
///
/// Offsets and lengths are multiples of the sector size; a length is at least
/// one sector and at most the buffer; the range lies inside the zone. Two
/// in-flight operations on overlapping ranges, one of them a write, are not
/// allowed. Breaking these rules is a bug in the caller and panics.
pub trait Storage {
    /// Size of `zone` in bytes.
    fn size(&self, zone: Zone) -> u64;
    /// Reads `len` bytes at `offset` into `buffer[..len]`.
    fn read(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>>;
    /// Writes `buffer[..len]` at `offset`.
    fn write(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>>;
    /// Makes every completed write durable.
    fn fsync(&mut self, tag: Tag) -> Result<(), Full<()>>;
}

/// Connections that carry whole messages. Framing (the header's size field)
/// belongs to the backend: a receive completes with one whole message.
///
/// On one connection, messages arrive in the order they were sent, and fill
/// the posted buffers in the order those were posted. A connection id is
/// used only by the node it was returned to, and only until it closes it.
pub trait Network {
    /// Opens a connection to `to`.
    fn connect(&mut self, tag: Tag, to: Peer) -> Result<(), Full<()>>;
    /// Takes the next connection another node opens to this one.
    fn accept(&mut self, tag: Tag) -> Result<(), Full<()>>;
    /// Sends the message in `buffer[..len]`: a valid header whose `size` is
    /// `len`, then its body.
    fn send(
        &mut self,
        tag: Tag,
        connection: Connection,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>>;
    /// Posts `buffer` for the next message on `connection`. A backend reads
    /// from a connection only into posted buffers, so a node that posts none
    /// pushes back on its sender.
    fn recv(
        &mut self,
        tag: Tag,
        connection: Connection,
        buffer: Message,
    ) -> Result<(), Full<Message>>;
    /// Closes `connection`; see the module documentation.
    fn close(&mut self, connection: Connection);
}

/// The clocks. Readings are nanoseconds.
pub trait Time {
    /// Time since an arbitrary start; never decreases.
    fn monotonic(&self) -> u64;
    /// Wall-clock time since the Unix epoch; may jump in either direction.
    fn realtime(&self) -> i64;
}

/// A seeded generator, used only outside the state machine.
pub trait Random {
    /// The next 64 random bits.
    fn next_u64(&mut self) -> u64;
}

/// Everything a shard reaches the outside world through.
pub trait Io: Storage + Network + Time + Random {
    /// One completed operation, or `None` when none has completed.
    fn poll(&mut self) -> Option<Completion>;
}
