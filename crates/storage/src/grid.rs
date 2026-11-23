//! The grid (document 01 section 2, Grid): fixed-size blocks, each read only
//! against a [`Reference`] kept somewhere else, so that bit rot and a torn,
//! lost or misdirected write or a misdirected read are caught before the
//! bytes reach a reader. Allocation (T1.1.6), the block cache (T1.1.7) and
//! repair from peers (T2.2.6) build on it.
//!
//! A block starts with a [`HEADER_SIZE`]-byte header: the checksum of the
//! rest of the block, the block's own address and its [`BlockType`]. So the
//! checksum covers the address and the type, and a valid block at the wrong
//! place or of the wrong kind still fails its reference.
//!
//! A write seals the header into the caller's buffer and writes the block
//! from it. A read takes the reference, reads the whole block into the
//! caller's buffer with one device read and checks it there, in place. There
//! is no read by address alone, and the bytes reach the caller only through
//! [`Grid::complete`], after every check. Every table is sized when the grid
//! is built; a read or a write allocates nothing, and beyond the operations
//! it was built for one is refused with [`Full`].

use granite_base::checksum::checksum;
use granite_base::constants::FORMAT;
use granite_io::message_pool::{MESSAGE_SIZE, Message};
use granite_io::{Completion, Full, Storage, StorageError, Tag, Zone};

pub use crate::superblock::Reference;

/// Bytes in a block, header included.
pub const BLOCK_SIZE: usize = FORMAT.grid_block_size as usize;
/// Bytes of the block header; the content follows it.
pub const HEADER_SIZE: usize = 32;
// Offsets in the header. The checksum is at 0 and covers bytes 16 to the
// end of the block; bytes 25 to 31 are zero.
const ADDRESS: usize = 16;
const TYPE: usize = 24;

const _: () = assert!(BLOCK_SIZE <= MESSAGE_SIZE && HEADER_SIZE < BLOCK_SIZE);

/// What a block holds. The value is part of the on-disk format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BlockType {
    Manifest = 1,
    FreeSet = 2,
    ClientSessions = 3,
}

/// What a completion led to.
#[derive(Debug)]
pub enum Event {
    /// The block reached the device; it is durable after a later fsync.
    Written { reference: Reference },
    /// A read finished. On success the buffer holds the block, checked; on
    /// failure the bytes are not returned and the block needs repair from a
    /// peer by `reference`, the (address, checksum) the read expected.
    Read {
        reference: Reference,
        result: Result<Message, ReadError>,
    },
}

/// Why a read found no valid block for its reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// The device failed the read.
    Io(StorageError),
    /// The block does not match its own checksum: bit rot or a torn write.
    Checksum,
    /// The block is valid but is not the one referenced: a lost or
    /// misdirected write, or a misdirected read.
    Mismatch,
    /// The block is the one referenced but was written for another address.
    Address,
    /// The block is the one referenced but is of another type.
    Type,
}

/// An operation in flight.
#[derive(Clone, Copy, Debug)]
enum Op {
    Write(Reference),
    Read(Reference, BlockType),
}

/// The block reads and writes of one shard.
pub struct Grid {
    /// The first of the tags the grid uses, one per operation in flight.
    tag: Tag,
    ops: Vec<Option<Op>>,
}

impl Grid {
    /// A grid with at most `ops` reads and writes in flight, using the tags
    /// from `tag` to `tag + ops`.
    pub fn new(tag: Tag, ops: u32) -> Grid {
        assert!(ops > 0);
        assert!(tag.checked_add(u64::from(ops)).is_some(), "the tags fit");
        Grid {
            tag,
            ops: vec![None; ops as usize],
        }
    }

    /// Whether `tag` is one of the grid's; its completions go to
    /// [`Grid::complete`].
    pub fn owns(&self, tag: Tag) -> bool {
        tag.checked_sub(self.tag)
            .is_some_and(|at| at < self.ops.len() as u64)
    }

    /// Writes the block in `block` to `address`: the caller fills the
    /// content, after [`HEADER_SIZE`] bytes, and the grid seals the header.
    /// The buffer must not be shared. Returns the block's reference; its
    /// completion is [`Event::Written`].
    pub fn write<S: Storage>(
        &mut self,
        io: &mut S,
        address: u64,
        kind: BlockType,
        mut block: Message,
    ) -> Result<Reference, Full<Message>> {
        let offset = offset(io, address);
        let bytes = block.bytes_mut().expect("a block to write is not shared");
        bytes[ADDRESS..TYPE].copy_from_slice(&address.to_le_bytes());
        bytes[TYPE] = kind as u8;
        bytes[TYPE + 1..HEADER_SIZE].fill(0);
        let sum = checksum(&bytes[16..BLOCK_SIZE]);
        bytes[..16].copy_from_slice(&sum.to_le_bytes());
        let reference = Reference {
            address,
            checksum: sum,
        };
        self.submit(Op::Write(reference), block, |tag, block| {
            io.write(tag, Zone::Grid, offset, BLOCK_SIZE as u32, block)
        })?;
        Ok(reference)
    }

    /// Reads the block `reference` names, which must be of type `kind`, into
    /// `buffer`, which must not be shared. Its completion is [`Event::Read`].
    pub fn read<S: Storage>(
        &mut self,
        io: &mut S,
        reference: Reference,
        kind: BlockType,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        let offset = offset(io, reference.address);
        self.submit(Op::Read(reference, kind), buffer, |tag, buffer| {
            io.read(tag, Zone::Grid, offset, BLOCK_SIZE as u32, buffer)
        })
    }

    /// Handles the completion of one of the grid's operations. A failed
    /// write leaves the block's state unknown, so it stops the process
    /// (decision 0003).
    pub fn complete(&mut self, completion: Completion) -> Event {
        assert!(self.owns(completion.tag()), "a completion of the grid");
        let at = (completion.tag() - self.tag) as usize;
        let op = self.ops[at].take().expect("an operation in flight");
        match (op, completion) {
            (Op::Write(reference), Completion::Write { result, .. }) => {
                result.expect("a grid write failed");
                Event::Written { reference }
            }
            (Op::Read(reference, kind), Completion::Read { buffer, result, .. }) => {
                let result = result.map_err(ReadError::Io).and_then(|()| {
                    verify(reference, kind, &buffer.bytes()[..BLOCK_SIZE])?;
                    Ok(buffer)
                });
                Event::Read { reference, result }
            }
            _ => unreachable!("a completion of the operation submitted"),
        }
    }

    /// Submits `op` with a free tag, or refuses it when none is free or the
    /// backend is full.
    fn submit(
        &mut self,
        op: Op,
        buffer: Message,
        io: impl FnOnce(Tag, Message) -> Result<(), Full<Message>>,
    ) -> Result<(), Full<Message>> {
        let Some(at) = self.ops.iter().position(Option::is_none) else {
            return Err(Full(buffer));
        };
        io(self.tag + at as u64, buffer)?;
        self.ops[at] = Some(op);
        Ok(())
    }
}

/// The offset in the grid zone of the block at `address`, which is 1 or
/// more (0 names no block) and inside the grid.
fn offset<S: Storage>(io: &S, address: u64) -> u64 {
    let blocks = io.size(Zone::Grid) / FORMAT.grid_block_size;
    assert!(
        (1..=blocks).contains(&address),
        "block {address} is in the grid"
    );
    (address - 1) * FORMAT.grid_block_size
}

/// Whether `block` is the block `reference` names, of type `kind`.
fn verify(reference: Reference, kind: BlockType, block: &[u8]) -> Result<(), ReadError> {
    let stored = u128::from_le_bytes(block[..16].try_into().expect("16 bytes"));
    if checksum(&block[16..]) != stored {
        return Err(ReadError::Checksum);
    }
    if stored != reference.checksum {
        return Err(ReadError::Mismatch);
    }
    let address = u64::from_le_bytes(block[ADDRESS..TYPE].try_into().expect("8 bytes"));
    if address != reference.address {
        return Err(ReadError::Address);
    }
    if block[TYPE] != kind as u8 {
        return Err(ReadError::Type);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use granite_io::memory::SplitMix64;

    /// A block of `kind` sealed for `address`, as [`Grid::write`] seals it,
    /// holding content that differs from byte to byte.
    fn block(address: u64, kind: BlockType) -> (Vec<u8>, Reference) {
        let mut bytes: Vec<u8> = (0..BLOCK_SIZE).map(|i| (i * 31 % 251) as u8).collect();
        bytes[ADDRESS..TYPE].copy_from_slice(&address.to_le_bytes());
        bytes[TYPE] = kind as u8;
        bytes[TYPE + 1..HEADER_SIZE].fill(0);
        let sum = checksum(&bytes[16..]);
        bytes[..16].copy_from_slice(&sum.to_le_bytes());
        let reference = Reference {
            address,
            checksum: sum,
        };
        assert_eq!(verify(reference, kind, &bytes), Ok(()));
        (bytes, reference)
    }

    /// Flips bit `bit` of `bytes`, checks the block fails, and flips it back.
    fn flip_is_detected(bytes: &mut [u8], reference: Reference, bit: usize) {
        bytes[bit / 8] ^= 1 << (bit % 8);
        let result = verify(reference, BlockType::FreeSet, bytes);
        // A flip in the stored checksum makes it disagree with the content.
        assert_eq!(result, Err(ReadError::Checksum), "bit {bit}");
        bytes[bit / 8] ^= 1 << (bit % 8);
    }

    #[test]
    fn every_header_bit_flip_is_detected() {
        let (mut bytes, reference) = block(7, BlockType::FreeSet);
        for bit in 0..HEADER_SIZE * 8 {
            flip_is_detected(&mut bytes, reference, bit);
        }
        assert_eq!(verify(reference, BlockType::FreeSet, &bytes), Ok(()));
    }

    #[test]
    fn a_bit_flip_at_100_000_random_positions_is_detected() {
        let seed = 0x9e37_79b9;
        let (mut bytes, reference) = block(7, BlockType::FreeSet);
        let mut random = SplitMix64(seed);
        for _ in 0..100_000 {
            let bit = (random.next_u64() % (BLOCK_SIZE as u64 * 8)) as usize;
            flip_is_detected(&mut bytes, reference, bit);
        }
        assert_eq!(verify(reference, BlockType::FreeSet, &bytes), Ok(()));
    }
}
