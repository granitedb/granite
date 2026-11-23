//! The gates of T1.1.5, grid blocks, on the simulated disk: a block read
//! through a reference to another address, a misdirected read and a block of
//! the wrong type all fail with a typed error naming the reference, and one
//! block read is one device read into the caller's buffer, with no copy and
//! no allocation after it. The bit flip gates are unit tests of
//! `granite_storage::grid`; the fuzz gate is `fuzz/fuzz_targets/grid_read.rs`.

// The io tests' counting allocator, shared rather than copied: it is the one
// module outside `io` proper that tools/lint.py lets use `unsafe` for this.
#[path = "../../io/tests/common/counting_alloc.rs"]
mod counting_alloc;

use granite_base::constants::FORMAT;
use granite_io::message_pool::{Message, MessagePool};
use granite_io::{Completion, Full, Storage, Tag, Zone};
use granite_sim::storage::MILLION;
use granite_sim::{Disk, FaultKind, Faults, Group, Scheduler};
use granite_storage::grid::{
    BLOCK_SIZE, BlockType, Event, Grid, HEADER_SIZE, ReadError, Reference,
};

/// The grid's first tag: anything but zero, so a test that ignored the base
/// would fail.
const TAG: Tag = 1 << 40;
/// The tag of the test's own operations on the raw disk.
const RAW: Tag = 7;
/// Blocks in the test grid.
const BLOCKS: u64 = 4;

/// A disk whose grid holds [`BLOCKS`] blocks; the other zones are one
/// sector, since the grid never touches them.
fn disk(seed: u64, capacity: usize, faults: Faults) -> Disk {
    let sector = FORMAT.sector_size;
    let zones = [sector, sector, sector, BLOCKS * FORMAT.grid_block_size];
    let mut sim = Scheduler::<()>::new(seed, 1);
    Disk::new(&Group::new(1, 1, zones), 0, capacity, faults, &mut sim)
}

/// A buffer whose content, after the header, differs from block to block.
fn content(pool: &MessagePool, seed: u64) -> Message {
    let mut message = pool.acquire().expect("a free buffer");
    let bytes = message.bytes_mut().expect("only handle");
    for (i, b) in bytes[HEADER_SIZE..BLOCK_SIZE].iter_mut().enumerate() {
        *b = (i as u64 ^ seed.wrapping_mul(0x9e37_79b9)) as u8;
    }
    message
}

/// Delivers the one completion due on `disk` to `grid`.
fn complete(disk: &mut Disk, grid: &mut Grid) -> Event {
    let completion = disk.poll().expect("a completion is due");
    grid.complete(completion)
}

/// Writes a block of `kind` with content `seed` to `address`.
fn write(
    disk: &mut Disk,
    grid: &mut Grid,
    pool: &MessagePool,
    address: u64,
    kind: BlockType,
    seed: u64,
) -> Reference {
    let reference = grid
        .write(disk, address, kind, content(pool, seed))
        .expect("room");
    assert!(matches!(
        complete(disk, grid),
        Event::Written { reference: r } if r == reference
    ));
    reference
}

/// Reads `reference` as a block of `kind`, checking that the completion
/// names it.
fn read(
    disk: &mut Disk,
    grid: &mut Grid,
    pool: &MessagePool,
    reference: Reference,
    kind: BlockType,
) -> Result<Message, ReadError> {
    let buffer = pool.acquire().expect("a free buffer");
    grid.read(disk, reference, kind, buffer).expect("room");
    match complete(disk, grid) {
        Event::Read {
            reference: r,
            result,
        } => {
            assert_eq!(r, reference, "a read reports the reference it expected");
            result
        }
        Event::Written { .. } => panic!("a read completes as a read"),
    }
}

/// Runs one raw operation on the disk, outside the grid, and returns its
/// buffer.
fn raw(disk: &mut Disk, write: bool, address: u64, buffer: Message) -> Message {
    let offset = (address - 1) * FORMAT.grid_block_size;
    let len = BLOCK_SIZE as u32;
    if write {
        disk.write(RAW, Zone::Grid, offset, len, buffer).unwrap();
    } else {
        disk.read(RAW, Zone::Grid, offset, len, buffer).unwrap();
    }
    match disk.poll() {
        Some(
            Completion::Read { buffer, result, .. } | Completion::Write { buffer, result, .. },
        ) => {
            result.unwrap();
            buffer
        }
        _ => panic!("a raw read or write"),
    }
}

#[test]
fn a_written_block_reads_back_whole() {
    let pool = MessagePool::new(4);
    let mut disk = disk(1, 4, Faults::default());
    let mut grid = Grid::new(TAG, 4);
    for address in 1..=BLOCKS {
        let reference = write(
            &mut disk,
            &mut grid,
            &pool,
            address,
            BlockType::Manifest,
            address,
        );
        let block = read(&mut disk, &mut grid, &pool, reference, BlockType::Manifest).unwrap();
        let expected = content(&pool, address);
        assert_eq!(
            block.bytes()[HEADER_SIZE..BLOCK_SIZE],
            expected.bytes()[HEADER_SIZE..BLOCK_SIZE]
        );
    }
}

/// Gate: a block written to address A and read through a reference to
/// address B fails, even when B holds valid content.
#[test]
fn a_block_read_through_a_reference_to_another_address_fails() {
    let pool = MessagePool::new(4);
    let mut disk = disk(2, 4, Faults::default());
    let mut grid = Grid::new(TAG, 4);
    let a = write(&mut disk, &mut grid, &pool, 1, BlockType::Manifest, 1);
    let to_b = Reference {
        address: 2,
        checksum: a.checksum,
    };

    // B holds A's block, byte for byte: valid, and the checksum the
    // reference names, but written for A.
    let bytes = raw(&mut disk, false, 1, pool.acquire().unwrap());
    drop(raw(&mut disk, true, 2, bytes));
    let result = read(&mut disk, &mut grid, &pool, to_b, BlockType::Manifest);
    assert_eq!(result.err(), Some(ReadError::Address));

    // B holds a valid block of its own.
    write(&mut disk, &mut grid, &pool, 2, BlockType::Manifest, 2);
    let result = read(&mut disk, &mut grid, &pool, to_b, BlockType::Manifest);
    assert_eq!(result.err(), Some(ReadError::Mismatch));

    // A itself still reads.
    assert!(read(&mut disk, &mut grid, &pool, a, BlockType::Manifest).is_ok());
}

/// Gate: a read that returns a different valid block is detected. Every
/// read is misdirected, to any other sector-aligned place in the grid; one
/// that lands on another block's start returns that valid block.
#[test]
fn a_misdirected_read_is_detected() {
    let faults = Faults {
        misdirected_read: MILLION,
        ..Faults::default()
    };
    let pool = MessagePool::new(4);
    let mut disk = disk(3, 4, faults);
    let mut grid = Grid::new(TAG, 4);
    let references: Vec<Reference> = (1..=BLOCKS)
        .map(|address| {
            write(
                &mut disk,
                &mut grid,
                &pool,
                address,
                BlockType::FreeSet,
                address,
            )
        })
        .collect();
    let mut other_blocks = 0;
    for i in 0..3_000 {
        let reference = references[i % references.len()];
        let result = read(&mut disk, &mut grid, &pool, reference, BlockType::FreeSet);
        let faults: Vec<_> = disk.faults().collect();
        let [fault] = faults[..] else {
            panic!("one fault per read: {faults:?}")
        };
        let FaultKind::MisdirectedRead { from } = fault.kind else {
            panic!("a misdirected read: {fault:?}")
        };
        if from.is_multiple_of(FORMAT.grid_block_size) {
            other_blocks += 1;
            assert_eq!(result.err(), Some(ReadError::Mismatch), "from {from}");
        } else {
            assert_eq!(result.err(), Some(ReadError::Checksum), "from {from}");
        }
    }
    // 3 of the 97 places a block read can land on are another block's start.
    assert!(
        other_blocks > 30,
        "{other_blocks} reads returned another block"
    );
}

/// Gate: a block of the wrong type for the reference is rejected.
#[test]
fn a_block_of_the_wrong_type_is_rejected() {
    let pool = MessagePool::new(4);
    let mut disk = disk(4, 4, Faults::default());
    let mut grid = Grid::new(TAG, 4);
    let reference = write(&mut disk, &mut grid, &pool, 3, BlockType::Manifest, 3);
    for kind in [BlockType::FreeSet, BlockType::ClientSessions] {
        let result = read(&mut disk, &mut grid, &pool, reference, kind);
        assert_eq!(result.err(), Some(ReadError::Type));
    }
    assert!(read(&mut disk, &mut grid, &pool, reference, BlockType::Manifest).is_ok());
}

/// A disk that counts the reads submitted to it.
struct Counted {
    disk: Disk,
    reads: u32,
}

impl Storage for Counted {
    fn size(&self, zone: Zone) -> u64 {
        self.disk.size(zone)
    }
    fn read(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        self.reads += 1;
        self.disk.read(tag, zone, offset, len, buffer)
    }
    fn write(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        self.disk.write(tag, zone, offset, len, buffer)
    }
    fn fsync(&mut self, tag: Tag) -> Result<(), Full<()>> {
        self.disk.fsync(tag)
    }
}

/// Gate: one block read is exactly one device read, and the block comes
/// back in the buffer it was read into, with no copy and no allocation.
#[test]
fn a_block_read_is_one_device_read_and_no_copy() {
    let pool = MessagePool::new(4);
    let mut io = Counted {
        disk: disk(5, 4, Faults::default()),
        reads: 0,
    };
    let mut grid = Grid::new(TAG, 4);
    let reference = grid
        .write(&mut io, 2, BlockType::ClientSessions, content(&pool, 2))
        .unwrap();
    grid.complete(io.disk.poll().unwrap());

    let buffer = pool.acquire().unwrap();
    let address = buffer.bytes().as_ptr();
    let before = counting_alloc::allocations();
    grid.read(&mut io, reference, BlockType::ClientSessions, buffer)
        .unwrap();
    let event = grid.complete(io.disk.poll().unwrap());
    let allocations = counting_alloc::allocations() - before;
    let Event::Read {
        result: Ok(block), ..
    } = event
    else {
        panic!("a verified read: {event:?}")
    };
    assert_eq!(io.reads, 1, "device reads");
    assert_eq!(block.bytes().as_ptr(), address, "the block is not copied");
    assert_eq!(allocations, 0);
}

/// Standard gate: the grid's table of operations is bounded; beyond it, and
/// when the backend is full, a submission is refused and handed back, and a
/// refusal takes no slot.
#[test]
fn a_full_grid_refuses_and_hands_the_buffer_back() {
    let pool = MessagePool::new(4);
    let mut disk = disk(6, 1, Faults::default());
    let mut grid = Grid::new(TAG, 1);
    assert!(!grid.owns(TAG - 1) && grid.owns(TAG) && !grid.owns(TAG + 1));
    let reference = write(&mut disk, &mut grid, &pool, 1, BlockType::Manifest, 1);

    // The grid's one slot is taken.
    grid.read(
        &mut disk,
        reference,
        BlockType::Manifest,
        pool.acquire().unwrap(),
    )
    .unwrap();
    let Err(Full(buffer)) = grid.read(
        &mut disk,
        reference,
        BlockType::Manifest,
        pool.acquire().unwrap(),
    ) else {
        panic!("the grid is full")
    };
    let Event::Read { result: Ok(_), .. } = complete(&mut disk, &mut grid) else {
        panic!("a verified read")
    };

    // The backend is full: a raw read takes its one slot.
    disk.read(
        RAW,
        Zone::Grid,
        0,
        BLOCK_SIZE as u32,
        pool.acquire().unwrap(),
    )
    .unwrap();
    let Err(Full(buffer)) = grid.read(&mut disk, reference, BlockType::Manifest, buffer) else {
        panic!("the backend is full")
    };
    assert!(matches!(
        disk.poll(),
        Some(Completion::Read { tag: RAW, .. })
    ));

    // Neither refusal kept the slot.
    assert!(read(&mut disk, &mut grid, &pool, reference, BlockType::Manifest).is_ok());
    drop(buffer);
}

#[test]
#[should_panic(expected = "is in the grid")]
fn address_zero_names_no_block() {
    let pool = MessagePool::new(1);
    let mut disk = disk(7, 1, Faults::default());
    let reference = Reference {
        address: 0,
        checksum: 0,
    };
    let _ = Grid::new(TAG, 1).read(
        &mut disk,
        reference,
        BlockType::Manifest,
        pool.acquire().unwrap(),
    );
}
