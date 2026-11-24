//! The free set (document 01 section 2, Free set): which grid blocks are
//! acquired, as a bitset sized for the group's grid limit when it is built.
//!
//! Allocation is a function of the calls alone: [`FreeSet::acquire`] takes
//! the lowest free block outside every reservation, and a reservation takes
//! the lowest free blocks in its own range, so every replica that makes the
//! same calls gets the same addresses, however its compactions interleave.
//!
//! A released block stays acquired until the checkpoint after its release is
//! durable, so a replica that crashes can always reopen the previous
//! checkpoint with every block it references intact. Between
//! [`FreeSet::checkpoint`] and [`FreeSet::checkpoint_durable`] the set is
//! frozen: the replica applies no op until its checkpoint is durable, which
//! is what makes released blocks free at the same op on every replica.
//!
//! The free set does no I/O. The checkpoint encodes it into grid blocks of
//! type `FreeSet`, chained from the last to the first, whose first reference
//! the superblock keeps; recovery decodes them in chain order.

use granite_base::constants::FORMAT;

use crate::grid::{BLOCK_SIZE, HEADER_SIZE, Reference};
use crate::growth::{GridSize, NoSpace};

/// Blocks in one growth step.
const STEP_BLOCKS: u64 = FORMAT.grid_growth_step / FORMAT.grid_block_size;
// The grid is whole words of the bitset, so no word is part in, part out.
const _: () = assert!(STEP_BLOCKS.is_multiple_of(64));

/// The grid grows when fewer blocks than this are free outside reservations:
/// an eighth of a step (document 01 section 2).
pub const GROW_BELOW: u64 = STEP_BLOCKS / 8;

// The content of a free-set block, after the grid header: the grid size in
// blocks (`u64`), the reference of the next block of the chain (address
// `u64`, checksum `u128`; address 0 on the last), then this block's share of
// the bitset, little-endian `u64` words, zero to the end of the block.
const NEXT: usize = 8;
const WORDS: usize = 32;
/// Bytes of a block's content.
pub const CONTENT_SIZE: usize = BLOCK_SIZE - HEADER_SIZE;
/// Bitset words one block holds.
const BLOCK_WORDS: usize = (CONTENT_SIZE - WORDS) / 8;

/// Blocks [`FreeSet::reserve`] promised to one caller, in a range of the
/// grid no other caller acquires from.
#[derive(Debug)]
pub struct Reservation {
    /// The lowest block index that may still be free in the range.
    next: u64,
    end: u64,
    left: u64,
}

impl Reservation {
    /// Blocks it still holds.
    pub fn left(&self) -> u64 {
        self.left
    }
}

/// Which blocks of the grid are acquired.
pub struct FreeSet {
    grid: GridSize,
    /// Bit `i` of word `w`: block `64 w + i + 1` is acquired.
    acquired: Vec<u64>,
    /// Released since the last checkpoint: still acquired, free once the
    /// next checkpoint is durable.
    released: Vec<u64>,
    /// Blocks of the grid that are not acquired.
    free: u64,
    /// Every word below this one is full; only a hint for speed.
    full_below: usize,
    /// The block index after the last reservation's range; acquire takes
    /// blocks from here on.
    cursor: u64,
    reservations: u32,
    /// Blocks reservations hold and have not acquired yet.
    promised: u64,
    /// The addresses of the last checkpoint's encoding, in chain order.
    blocks: Vec<u64>,
    checkpointing: bool,
}

/// The blocks that encode a free set over a grid of `blocks` blocks.
fn encoded_blocks(blocks: u64) -> usize {
    (blocks / 64).div_ceil(BLOCK_WORDS as u64) as usize
}

impl FreeSet {
    /// An empty free set over `grid`.
    pub fn new(grid: GridSize) -> FreeSet {
        let words = (grid.limit() / FORMAT.grid_block_size / 64) as usize;
        FreeSet {
            grid,
            acquired: vec![0; words],
            released: vec![0; words],
            free: grid.blocks(),
            full_below: 0,
            cursor: 0,
            reservations: 0,
            promised: 0,
            blocks: Vec::with_capacity(encoded_blocks(grid.limit() / FORMAT.grid_block_size)),
            checkpointing: false,
        }
    }

    /// The grid the set covers.
    pub fn grid(&self) -> GridSize {
        self.grid
    }

    /// Blocks of the grid that are not acquired.
    pub fn free(&self) -> u64 {
        self.free
    }

    /// Whether the block at `address` is acquired, released blocks waiting
    /// for a durable checkpoint included.
    pub fn is_acquired(&self, address: u64) -> bool {
        let index = self.index(address);
        self.acquired[index / 64] & 1 << (index % 64) != 0
    }

    /// Acquires the lowest free block outside every reservation, or returns
    /// [`NoSpace`] when none is free beyond the blocks the next checkpoint
    /// needs for its own encoding.
    pub fn acquire(&mut self) -> Result<u64, NoSpace> {
        assert!(!self.checkpointing, "the set is frozen for a checkpoint");
        if self.free - self.promised <= self.headroom() {
            return Err(NoSpace);
        }
        let index = self.find(self.cursor, self.grid.blocks()).ok_or(NoSpace)?;
        self.take(index);
        Ok(index + 1)
    }

    /// Reserves `count` free blocks, the lowest after the previous
    /// reservation's, or returns [`NoSpace`] and changes nothing.
    pub fn reserve(&mut self, count: u64) -> Result<Reservation, NoSpace> {
        assert!(count > 0 && !self.checkpointing);
        if self.free - self.promised < count + self.headroom() {
            return Err(NoSpace);
        }
        let mut end = self.cursor;
        for _ in 0..count {
            end = self.find(end, self.grid.blocks()).ok_or(NoSpace)? + 1;
        }
        let reservation = Reservation {
            next: self.cursor,
            end,
            left: count,
        };
        self.cursor = end;
        self.reservations += 1;
        self.promised += count;
        Ok(reservation)
    }

    /// Acquires the lowest free block of `reservation`, which has one left.
    pub fn acquire_reserved(&mut self, reservation: &mut Reservation) -> u64 {
        assert!(!self.checkpointing && reservation.left > 0, "a block left");
        let index = self
            .find(reservation.next, reservation.end)
            .expect("a reservation's range holds its blocks");
        self.take(index);
        reservation.next = index + 1;
        reservation.left -= 1;
        self.promised -= 1;
        index + 1
    }

    /// Gives back the blocks `reservation` did not acquire. Once every
    /// reservation is forfeited, the next starts from the first block again.
    pub fn forfeit(&mut self, reservation: Reservation) {
        assert!(self.reservations > 0 && !self.checkpointing);
        self.promised -= reservation.left;
        self.reservations -= 1;
        if self.reservations == 0 {
            assert_eq!(self.promised, 0);
            self.cursor = 0;
        }
    }

    /// Releases the acquired block at `address`. It is free once the next
    /// checkpoint is durable.
    pub fn release(&mut self, address: u64) {
        assert!(!self.checkpointing, "the set is frozen for a checkpoint");
        let index = self.index(address);
        let bit = 1 << (index % 64);
        assert!(
            self.acquired[index / 64] & bit != 0,
            "block {address} is acquired"
        );
        assert!(
            self.released[index / 64] & bit == 0,
            "block {address} is released once"
        );
        self.released[index / 64] |= bit;
    }

    /// The grid's size one step up, in bytes, when fewer than
    /// [`GROW_BELOW`] blocks are free outside reservations and the grid is
    /// below its limit. The state machine asks before each op it applies, and
    /// if so extends the data file's grid zone to that size
    /// (`Storage::grow_grid`) and calls [`FreeSet::grown`] once it completes,
    /// before the op goes on. It reads only the set, so every replica grows
    /// at the same op.
    pub fn growth(&self) -> Option<u64> {
        let low = self.free - self.promised < GROW_BELOW;
        let size = self.grid.size() + FORMAT.grid_growth_step;
        (low && size <= self.grid.limit()).then_some(size)
    }

    /// The data file's grid zone grew by the step [`FreeSet::growth`] asked
    /// for: its blocks are free.
    pub fn grown(&mut self) {
        assert!(!self.checkpointing, "the set is frozen for a checkpoint");
        self.grid.grow().expect("a growth below the limit");
        self.free += STEP_BLOCKS;
    }

    /// Starts a checkpoint: releases the previous checkpoint's encoding,
    /// acquires the blocks of this one and freezes the set until
    /// [`FreeSet::checkpoint_durable`]. Returns how many blocks the encoding
    /// takes, none for an empty grid; [`FreeSet::encode`] fills each.
    pub fn checkpoint(&mut self) -> usize {
        assert!(self.reservations == 0, "no reservation spans a checkpoint");
        for at in 0..self.blocks.len() {
            self.release(self.blocks[at]);
        }
        self.blocks.clear();
        let count = encoded_blocks(self.grid.blocks());
        for _ in 0..count {
            let index = self
                .find(0, self.grid.blocks())
                .expect("acquire and reserve leave room for the encoding");
            self.take(index);
            self.blocks.push(index + 1);
        }
        self.checkpointing = true;
        count
    }

    /// Writes block `at` of the checkpoint's encoding into `content`, the
    /// block after its header, chained to `next`, the reference of block
    /// `at + 1` (address 0 for the last), and returns the address to write
    /// it to. Released blocks are encoded as free: the checkpoint no longer
    /// references them.
    pub fn encode(&self, at: usize, next: Reference, content: &mut [u8]) -> u64 {
        assert!(self.checkpointing && content.len() == CONTENT_SIZE);
        assert_eq!(next.address == 0, at + 1 == self.blocks.len(), "the chain");
        content[..NEXT].copy_from_slice(&self.grid.blocks().to_le_bytes());
        content[NEXT..NEXT + 8].copy_from_slice(&next.address.to_le_bytes());
        content[NEXT + 8..WORDS].copy_from_slice(&next.checksum.to_le_bytes());
        let words = self.chunk(at);
        let bytes = &mut content[WORDS..];
        for (i, w) in words.clone().enumerate() {
            let word = self.acquired[w] & !self.released[w];
            bytes[8 * i..8 * i + 8].copy_from_slice(&word.to_le_bytes());
        }
        bytes[8 * words.len()..].fill(0);
        self.blocks[at]
    }

    /// The checkpoint is durable: blocks released before it are free.
    pub fn checkpoint_durable(&mut self) {
        assert!(self.checkpointing, "a checkpoint was started");
        for w in 0..self.released.len() {
            let released = self.released[w];
            if released != 0 {
                self.acquired[w] &= !released;
                self.free += u64::from(released.count_ones());
                self.full_below = self.full_below.min(w);
                self.released[w] = 0;
            }
        }
        self.checkpointing = false;
    }

    /// Decodes the next block of a checkpoint's encoding, read from
    /// `address`, into a set built empty over a grid of no blocks, and
    /// returns the reference of the block after it (address 0 after the
    /// last).
    pub fn decode(&mut self, address: u64, content: &[u8]) -> Reference {
        assert!(content.len() == CONTENT_SIZE && !self.checkpointing);
        let at = self.blocks.len();
        let blocks = u64::from_le_bytes(content[..NEXT].try_into().expect("8 bytes"));
        if at == 0 {
            assert!(self.grid.size() == 0 && self.free == 0, "a set built empty");
            let size = blocks.checked_mul(FORMAT.grid_block_size).expect("fits");
            self.grid = GridSize::new(size, self.grid.limit());
            self.free = blocks;
        }
        assert_eq!(blocks, self.grid.blocks(), "every block of a chain agrees");
        assert!(
            at < encoded_blocks(blocks),
            "the chain is as long as the set"
        );
        let bytes = &content[WORDS..];
        for (i, w) in self.chunk(at).enumerate() {
            let word = u64::from_le_bytes(bytes[8 * i..8 * i + 8].try_into().expect("8 bytes"));
            self.acquired[w] = word;
            self.free -= u64::from(word.count_ones());
        }
        self.blocks.push(address);
        let next = Reference {
            address: u64::from_le_bytes(content[NEXT..NEXT + 8].try_into().expect("8 bytes")),
            checksum: u128::from_le_bytes(content[NEXT + 8..WORDS].try_into().expect("16 bytes")),
        };
        if next.address == 0 {
            assert_eq!(self.blocks.len(), encoded_blocks(blocks), "the whole chain");
            for at in 0..self.blocks.len() {
                assert!(
                    self.is_acquired(self.blocks[at]),
                    "the encoding is acquired"
                );
            }
        }
        next
    }

    /// Blocks the next checkpoint's encoding takes, which acquire and
    /// reserve leave free.
    fn headroom(&self) -> u64 {
        encoded_blocks(self.grid.blocks()) as u64
    }

    /// The bitset words of block `at` of the encoding.
    fn chunk(&self, at: usize) -> std::ops::Range<usize> {
        let words = (self.grid.blocks() / 64) as usize;
        (at * BLOCK_WORDS).min(words)..((at + 1) * BLOCK_WORDS).min(words)
    }

    /// The index of the block at `address`, which is in the grid.
    fn index(&self, address: u64) -> usize {
        assert!(
            (1..=self.grid.blocks()).contains(&address),
            "block {address} is in the grid"
        );
        (address - 1) as usize
    }

    /// Marks the free block `index` acquired.
    fn take(&mut self, index: u64) {
        let (w, bit) = ((index / 64) as usize, 1 << (index % 64));
        assert!(self.acquired[w] & bit == 0, "a free block");
        self.acquired[w] |= bit;
        self.free -= 1;
    }

    /// The lowest free block index in `from..to`.
    fn find(&mut self, from: u64, to: u64) -> Option<u64> {
        let below = self.full_below as u64 * 64;
        let start = from.max(below);
        if start >= to {
            return None;
        }
        let last = to.div_ceil(64) as usize;
        let mut w = (start / 64) as usize;
        let mut bits = !self.acquired[w] & u64::MAX << (start % 64);
        while bits == 0 {
            w += 1;
            if w == last {
                return None;
            }
            bits = !self.acquired[w];
        }
        // A search from the hint passed only full words.
        if from <= below {
            self.full_below = w;
        }
        let index = w as u64 * 64 + u64::from(bits.trailing_zeros());
        (index < to).then_some(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use granite_io::memory::SplitMix64;

    const STEP: u64 = FORMAT.grid_growth_step;

    /// Grows `set` when it asks to, as if the file grew at once.
    fn grow(set: &mut FreeSet) -> Option<u64> {
        let size = set.growth()?;
        set.grown();
        Some(size)
    }

    /// Encodes `set` at a checkpoint and decodes it into a new set.
    fn round_trip(set: &mut FreeSet) -> FreeSet {
        let count = set.checkpoint();
        let mut contents = vec![vec![0; CONTENT_SIZE]; count];
        let mut addresses = vec![0; count];
        let mut next = Reference::default();
        for at in (0..count).rev() {
            addresses[at] = set.encode(at, next, &mut contents[at]);
            // Any distinct checksum: the set only carries it.
            next = Reference {
                address: addresses[at],
                checksum: u128::from(addresses[at]) << 64 | at as u128,
            };
        }
        set.checkpoint_durable();
        let mut decoded = FreeSet::new(GridSize::new(0, set.grid.limit()));
        for at in 0..count {
            let next = decoded.decode(addresses[at], &contents[at]);
            assert_eq!(next.address, addresses.get(at + 1).copied().unwrap_or(0));
        }
        decoded
    }

    fn same(a: &FreeSet, b: &FreeSet) {
        assert_eq!(a.grid, b.grid);
        assert_eq!(a.free, b.free);
        assert_eq!(a.acquired, b.acquired);
        assert_eq!(a.blocks, b.blocks);
    }

    /// Encode then decode returns the same set for 10,000 random sets over
    /// grids of 0 to 3 steps, including empty and full ones.
    #[test]
    fn encode_then_decode_returns_the_same_set() {
        assert_eq!(BLOCK_WORDS, 16_376, "the free set block of document 01");
        let seed = 0x5eed_f5e7;
        let mut random = SplitMix64(seed);
        for case in 0..10_000u64 {
            let steps = random.next_u64() % 4;
            let mut set = FreeSet::new(GridSize::new(steps * STEP, 3 * STEP));
            let blocks = set.grid.blocks();
            // Every fourth set is empty and every fourth full; the rest have
            // a random share of the grid acquired.
            let share = match case % 4 {
                0 => 0,
                1 => 100,
                _ => random.next_u64() % 101,
            };
            for index in 0..blocks.saturating_sub(set.headroom()) {
                if random.next_u64() % 100 < share {
                    set.take(index);
                }
            }
            let decoded = round_trip(&mut set);
            same(&set, &decoded);
            if case % 4 == 1 && blocks > 0 {
                assert_eq!(decoded.free, 0, "seed {seed:#x} case {case}: full");
            }
        }
    }

    /// Acquire and reserve on a full set return `NoSpace`, never an address,
    /// and leave the set as it was; so do they on a grid of no blocks.
    #[test]
    fn a_full_set_returns_no_space() {
        let mut set = FreeSet::new(GridSize::new(0, STEP));
        assert_eq!(set.acquire(), Err(NoSpace));
        assert!(set.reserve(1).is_err());
        assert_eq!(grow(&mut set), Some(STEP));
        let mut addresses = Vec::new();
        while let Ok(address) = set.acquire() {
            addresses.push(address);
        }
        // One block stays free for the checkpoint's encoding.
        assert_eq!(addresses.len() as u64, STEP_BLOCKS - 1);
        assert!(addresses.iter().zip(1..).all(|(&a, i)| a == i));
        assert_eq!(set.acquire(), Err(NoSpace));
        assert!(set.reserve(1).is_err());
        assert_eq!(grow(&mut set), None, "at the limit");
        assert_eq!((set.free(), set.promised, set.cursor), (1, 0, 0));
        assert_eq!(set.checkpoint(), 1);
        set.checkpoint_durable();
        assert_eq!(set.free(), 0);
        assert_eq!(set.acquire(), Err(NoSpace));
    }

    /// A released block is not handed out again until the checkpoint after
    /// its release is durable.
    #[test]
    fn a_released_block_waits_for_a_durable_checkpoint() {
        let mut set = FreeSet::new(GridSize::new(STEP, STEP));
        assert_eq!(set.acquire(), Ok(1));
        assert_eq!(set.acquire(), Ok(2));
        set.release(1);
        assert_eq!(set.acquire(), Ok(3));
        assert_eq!(set.checkpoint(), 1);
        assert!(
            set.is_acquired(1),
            "still acquired while the checkpoint is written"
        );
        set.checkpoint_durable();
        assert_eq!(set.acquire(), Ok(1));
    }

    /// Reservations take their blocks in the order they were made, from
    /// disjoint ranges, whatever order their acquires come in.
    #[test]
    fn reservations_are_disjoint_ranges_in_order() {
        let mut set = FreeSet::new(GridSize::new(STEP, STEP));
        assert_eq!(set.acquire(), Ok(1));
        let mut a = set.reserve(2).unwrap();
        let mut b = set.reserve(3).unwrap();
        assert_eq!(set.acquire(), Ok(7), "outside both ranges");
        assert_eq!(set.acquire_reserved(&mut b), 4);
        assert_eq!(set.acquire_reserved(&mut a), 2);
        set.forfeit(a);
        assert_eq!(set.acquire_reserved(&mut b), 5);
        set.forfeit(b);
        assert_eq!(set.acquire(), Ok(3), "every reservation forfeited");
    }

    #[test]
    #[should_panic(expected = "a block left")]
    fn a_reservation_gives_no_more_than_it_holds() {
        let mut set = FreeSet::new(GridSize::new(STEP, STEP));
        let mut a = set.reserve(1).unwrap();
        set.acquire_reserved(&mut a);
        set.acquire_reserved(&mut a);
    }
}
