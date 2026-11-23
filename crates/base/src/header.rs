//! The 256-byte header that every message, WAL prepare and reply starts with
//! (document 05 section 1). It lives here, not in `granite-protocol`, because
//! `granite-storage` writes it into the WAL and may depend only on `base` and
//! `io`.
//!
//! [`Header`] is the bytes on the wire: little-endian fields with alignment 1,
//! so a received buffer is read in place ([`Header::decode`]) and a header is
//! sent as `header.as_bytes()`. The derives refuse to compile a layout with
//! padding (`IntoBytes`) or with a field that is not plain bytes (`FromBytes`).

use crate::checksum::checksum;
use crate::constants::FORMAT;
use std::mem::{offset_of, size_of};
use zerocopy::little_endian::{U32, U64, U128};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, TryFromBytes, Unaligned};

/// The protocol version this binary speaks. Any other value is rejected.
pub const VERSION: u8 = 1;

/// The header of a message. Field order and offsets are those of document 05
/// section 1; the assertions below fix them.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned,
)]
pub struct Header {
    /// Over the rest of the header (bytes 16 to the end).
    pub checksum: U128,
    /// Over the body.
    pub checksum_body: U128,
    /// Checksum of the previous prepare (hash chain); zero elsewhere.
    pub parent: U128,
    /// Cluster id.
    pub cluster: U128,
    /// Client session id, or zero.
    pub client: U128,
    /// Cross-shard transaction id, or zero.
    pub txn: U128,
    /// Target group.
    pub group: U32,
    /// Sender's view of the tablet map.
    pub tablet_epoch: U32,
    /// Sender's schema version.
    pub schema_version: U32,
    pub view: U32,
    pub op: U64,
    pub commit: U64,
    /// Prepare timestamp, or the sender's highest seen timestamp.
    pub timestamp: U64,
    /// Session request number.
    pub request: U32,
    /// Header plus body, in bytes.
    pub size: U32,
    /// Sender replica index.
    pub replica: u8,
    /// A [`Command`] code.
    pub command: u8,
    /// An [`Operation`] code.
    pub operation: u8,
    /// [`VERSION`].
    pub version: u8,
    /// Must be zero.
    pub reserved: [u8; 108],
}

/// Fails, and so fails compilation in a const context, unless `T` has the
/// size of a header, `FORMAT.header_size`:
/// ```compile_fail,E0080
/// struct Other([u8; 255]);
/// const _: () = granite_base::header::assert_header_size::<Other>();
/// ```
/// ```
/// const _: () = granite_base::header::assert_header_size::<granite_base::header::Header>();
/// ```
/// The derives refuse a layout with padding:
/// ```compile_fail
/// #[derive(zerocopy::IntoBytes, zerocopy::Immutable)]
/// #[repr(C)]
/// struct Padded { a: u8, b: u32 }
/// ```
/// while the same fields without the gap compile:
/// ```
/// #[derive(zerocopy::IntoBytes, zerocopy::Immutable)]
/// #[repr(C)]
/// struct Unpadded { a: u8, b: [u8; 4] }
/// ```
pub const fn assert_header_size<T>() {
    assert!(
        size_of::<T>() as u64 == FORMAT.header_size,
        "header is FORMAT.header_size bytes"
    );
}

const _: () = {
    assert_header_size::<Header>();
    assert!(offset_of!(Header, checksum) == 0);
    assert!(offset_of!(Header, checksum_body) == 16);
    assert!(offset_of!(Header, parent) == 32);
    assert!(offset_of!(Header, cluster) == 48);
    assert!(offset_of!(Header, client) == 64);
    assert!(offset_of!(Header, txn) == 80);
    assert!(offset_of!(Header, group) == 96);
    assert!(offset_of!(Header, tablet_epoch) == 100);
    assert!(offset_of!(Header, schema_version) == 104);
    assert!(offset_of!(Header, view) == 108);
    assert!(offset_of!(Header, op) == 112);
    assert!(offset_of!(Header, commit) == 120);
    assert!(offset_of!(Header, timestamp) == 128);
    assert!(offset_of!(Header, request) == 136);
    assert!(offset_of!(Header, size) == 140);
    assert!(offset_of!(Header, replica) == 144);
    assert!(offset_of!(Header, command) == 145);
    assert!(offset_of!(Header, operation) == 146);
    assert!(offset_of!(Header, version) == 147);
    assert!(offset_of!(Header, reserved) == 148);
};

/// Protocol message kinds: document 02 section 2, then document 04 section 4.
/// Zero is no command, so an all-zero header is rejected.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, TryFromBytes, KnownLayout, Immutable)]
pub enum Command {
    Request = 1,
    Prepare = 2,
    PrepareOk = 3,
    Commit = 4,
    Reply = 5,
    StartViewChange = 6,
    DoViewChange = 7,
    StartView = 8,
    RequestStartView = 9,
    RequestHeaders = 10,
    Headers = 11,
    RequestPrepare = 12,
    NackPrepare = 13,
    RequestBlock = 14,
    Block = 15,
    Ping = 16,
    Pong = 17,
    RequestSyncCheckpoint = 18,
    SyncCheckpoint = 19,
    Eviction = 20,
    Propose = 21,
    Vote = 22,
}

/// State machine operations: document 03 section 4. `None` is for messages
/// that carry no operation.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, TryFromBytes, KnownLayout, Immutable)]
pub enum Operation {
    None = 0,
    Insert = 1,
    Upsert = 2,
    Update = 3,
    Delete = 4,
    Get = 5,
    Scan = 6,
    Execute = 7,
    Query = 8,
}

/// Why a header or a body was rejected. Each reason is its own variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Fewer bytes than a header.
    Short,
    /// The header checksum does not match.
    HeaderChecksum,
    /// The version is not [`VERSION`].
    Version,
    /// The command code is not a [`Command`].
    Command,
    /// The operation code is not an [`Operation`].
    Operation,
    /// A reserved byte is not zero.
    Reserved,
    /// `size` is smaller than a header.
    SizeBelowHeader,
    /// `size` is larger than `FORMAT.message_size_max`.
    SizeAboveMax,
    /// The body length is not `size` minus the header.
    BodyLength,
    /// The body checksum does not match.
    BodyChecksum,
}

impl Header {
    /// Sets `size` and both checksums for `body`. The encoded header is then
    /// `self.as_bytes()`.
    pub fn seal(&mut self, body: &[u8]) {
        let size = body.len() as u64 + FORMAT.header_size;
        assert!(
            size <= FORMAT.message_size_max,
            "message fits in FORMAT.message_size_max"
        );
        self.size = U32::new(u32::try_from(size).expect("message_size_max fits in u32"));
        self.checksum_body = U128::new(checksum(body));
        self.checksum = U128::new(self.checksum_calculated());
        debug_assert_eq!(self.check(), Ok(()));
    }

    /// The header at the start of `bytes`, read in place and checked. The
    /// bytes after it are not looked at; check them with [`Header::check_body`].
    pub fn decode(bytes: &[u8]) -> Result<&Header, Error> {
        let (header, _) = Header::ref_from_prefix(bytes).map_err(|_| Error::Short)?;
        header.check().map(|()| header)
    }

    /// Checks everything that the header alone decides.
    pub fn check(&self) -> Result<(), Error> {
        if self.checksum.get() != self.checksum_calculated() {
            return Err(Error::HeaderChecksum);
        }
        if self.version != VERSION {
            return Err(Error::Version);
        }
        if self.command().is_none() {
            return Err(Error::Command);
        }
        if self.operation().is_none() {
            return Err(Error::Operation);
        }
        if self.reserved.iter().any(|&b| b != 0) {
            return Err(Error::Reserved);
        }
        let size = u64::from(self.size.get());
        if size < FORMAT.header_size {
            return Err(Error::SizeBelowHeader);
        }
        if size > FORMAT.message_size_max {
            return Err(Error::SizeAboveMax);
        }
        Ok(())
    }

    /// Checks `body` against a header that passed [`Header::check`].
    pub fn check_body(&self, body: &[u8]) -> Result<(), Error> {
        if body.len() as u64 + FORMAT.header_size != u64::from(self.size.get()) {
            return Err(Error::BodyLength);
        }
        if checksum(body) != self.checksum_body.get() {
            return Err(Error::BodyChecksum);
        }
        Ok(())
    }

    /// The command, if the code is known.
    pub fn command(&self) -> Option<Command> {
        Command::try_read_from_bytes(&[self.command]).ok()
    }

    /// The operation, if the code is known.
    pub fn operation(&self) -> Option<Operation> {
        Operation::try_read_from_bytes(&[self.operation]).ok()
    }

    fn checksum_calculated(&self) -> u128 {
        checksum(&self.as_bytes()[offset_of!(Header, checksum_body)..])
    }
}
