//! Grid block reads over corrupted blocks: a read returns the bytes only if
//! they are exactly the block its reference was made for, at its address and
//! of its type, and returns them in the buffer it was given.
//!
//! A block is written through `Grid::write` to a device that holds one
//! block. The input then may replace it with another valid block, written for
//! another address or of another type; flips bytes of what the device holds;
//! and reads through the first block's reference, or through one naming
//! another address, expecting its type or another. The first input byte
//! chooses; the rest are edits of five bytes, an offset in the block (`u32`,
//! little-endian, modulo the block size) and a mask to xor there.

#![no_main]

use granite_io::message_pool::{Message, MessagePool};
use granite_io::{Completion, Full, Storage, Tag, Zone};
use granite_storage::grid::{BLOCK_SIZE, BlockType, Event, Grid, HEADER_SIZE, Reference};

const TAG: Tag = 100;

/// A device whose grid holds the one block in `block`, wherever it is read:
/// a read from any address models a misdirected or a correct read alike.
struct Device {
    block: Vec<u8>,
    done: Option<Completion>,
    reads: u32,
}

impl Storage for Device {
    fn size(&self, zone: Zone) -> u64 {
        assert_eq!(zone, Zone::Grid);
        2 * BLOCK_SIZE as u64
    }
    fn read(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        mut buffer: Message,
    ) -> Result<(), Full<Message>> {
        assert!(zone == Zone::Grid && len as usize == BLOCK_SIZE && self.done.is_none());
        assert!(offset.is_multiple_of(BLOCK_SIZE as u64));
        self.reads += 1;
        buffer.bytes_mut().unwrap()[..BLOCK_SIZE].copy_from_slice(&self.block);
        self.done = Some(Completion::Read {
            tag,
            buffer,
            result: Ok(()),
        });
        Ok(())
    }
    fn write(
        &mut self,
        tag: Tag,
        zone: Zone,
        _offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        assert!(zone == Zone::Grid && len as usize == BLOCK_SIZE && self.done.is_none());
        self.block.copy_from_slice(&buffer.bytes()[..BLOCK_SIZE]);
        self.done = Some(Completion::Write {
            tag,
            buffer,
            result: Ok(()),
        });
        Ok(())
    }
    fn fsync(&mut self, _: Tag) -> Result<(), Full<()>> {
        unreachable!("the grid does not fsync")
    }
}

/// Writes a block of `kind` to `address` whose content starts with `fill`,
/// and returns its reference.
fn write(
    grid: &mut Grid,
    device: &mut Device,
    pool: &MessagePool,
    address: u64,
    kind: BlockType,
    fill: u8,
) -> Reference {
    let mut buffer = pool.acquire().unwrap();
    buffer.bytes_mut().unwrap()[HEADER_SIZE..BLOCK_SIZE].fill(fill);
    let reference = grid.write(device, address, kind, buffer).unwrap();
    let event = grid.complete(device.done.take().unwrap());
    assert!(matches!(event, Event::Written { reference: r } if r == reference));
    reference
}

fn kind(bit: bool) -> BlockType {
    if bit {
        BlockType::FreeSet
    } else {
        BlockType::Manifest
    }
}

libfuzzer_sys::fuzz_target!(|input: &[u8]| {
    let Some((&flags, edits)) = input.split_first() else {
        return;
    };
    let bit = |n: u8| flags & (1 << n) != 0;
    let pool = MessagePool::new(1);
    let mut device = Device {
        block: vec![0; BLOCK_SIZE],
        done: None,
        reads: 0,
    };
    let mut grid = Grid::new(TAG, 1);

    let address = 1 + u64::from(bit(0));
    let written = kind(bit(1));
    let reference = write(&mut grid, &mut device, &pool, address, written, flags);
    let original = device.block.clone();
    if bit(2) {
        // The device holds another valid block instead.
        let other = 1 + u64::from(bit(3));
        write(&mut grid, &mut device, &pool, other, kind(bit(4)), !flags);
    }
    for edit in edits.chunks_exact(5) {
        let offset = u32::from_le_bytes(edit[..4].try_into().unwrap()) as usize % BLOCK_SIZE;
        device.block[offset] ^= edit[4];
    }
    let read_through = if bit(5) {
        Reference {
            address: 3 - address,
            ..reference
        }
    } else {
        reference
    };
    let expected = kind(bit(6) != bit(1));

    let buffer = pool.acquire().unwrap();
    let at = buffer.bytes().as_ptr();
    grid.read(&mut device, read_through, expected, buffer).unwrap();
    let Event::Read { reference: r, result } = grid.complete(device.done.take().unwrap()) else {
        panic!("a read completes as a read")
    };
    assert_eq!(r, read_through, "a read reports the reference it expected");
    assert_eq!(device.reads, 1, "one device read");
    // The oracle compares bytes, independently of how the grid checks them:
    // only the block the reference was made for may pass.
    let valid = device.block == original && read_through == reference && expected == written;
    match result {
        Ok(block) => {
            assert!(valid, "a read returned bytes that failed a check");
            assert_eq!(block.bytes().as_ptr(), at, "the block is not copied");
            assert_eq!(&block.bytes()[..BLOCK_SIZE], &original[..]);
        }
        Err(error) => assert!(!valid, "a valid block was rejected: {error:?}"),
    }
});
