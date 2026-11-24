//! The block cache (document 01 section 2, Grid): verified grid blocks kept
//! in memory, so repeated reads of index and filter blocks do not touch the
//! device. One per shard, sized when it is built; no operation allocates.
//!
//! The cache is set-associative: an address maps to one set of [`WAYS`]
//! slots. A slot holds a block and the checksum it was verified against, and
//! a lookup hits only when both the address and the checksum match, so a
//! reader holding an out-of-date [`Reference`] never gets the newer block,
//! nor the reverse. An address is in at most one slot: inserting it again
//! overwrites that slot, and [`Cache::release`] empties it, so an address
//! released and reused for new content never returns the old.
//!
//! Eviction is CLOCK within a set: a hit marks its slot, and the set's hand
//! passes over a marked slot once, clearing the mark, before it evicts. A
//! block enters unmarked, so a block read once leaves before one read twice.

use crate::grid::{BLOCK_SIZE, Reference};
use granite_io::metrics::Sink;

/// Slots in a set; the marks of a set fit one byte.
pub const WAYS: usize = 8;
/// The address of an empty slot. Grid addresses are block numbers within a
/// file, far below it.
const EMPTY: u64 = u64::MAX;

#[derive(Clone, Copy)]
struct Set {
    addresses: [u64; WAYS],
    checksums: [u128; WAYS],
    /// Bit `w` is set when slot `w` was hit since the hand last passed it.
    marks: u8,
    /// The slot the hand points at.
    hand: u8,
}

/// What the cache counted since it was built.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub hits: u64,
    pub misses: u64,
    /// Valid blocks pushed out to make room; a release or an overwrite of
    /// the same address is not one.
    pub evictions: u64,
}

impl Stats {
    /// Hits per million lookups; 0 before the first lookup.
    pub fn hit_rate_ppm(&self) -> u64 {
        // In u128: hits times a million passes u64 after 1.8e13 hits.
        let lookups = u128::from(self.hits) + u128::from(self.misses);
        (u128::from(self.hits) * 1_000_000)
            .checked_div(lookups)
            .map_or(0, |ppm| ppm as u64)
    }
}

/// The block cache of one shard.
pub struct Cache {
    sets: Vec<Set>,
    /// Slot `w` of set `s` is block `s * WAYS + w`.
    blocks: Vec<u8>,
    stats: Stats,
}

impl Cache {
    /// A cache of `blocks` blocks, a positive multiple of [`WAYS`].
    pub fn new(blocks: usize) -> Cache {
        assert!(blocks > 0 && blocks.is_multiple_of(WAYS), "{blocks} blocks");
        let empty = Set {
            addresses: [EMPTY; WAYS],
            checksums: [0; WAYS],
            marks: 0,
            hand: 0,
        };
        Cache {
            sets: vec![empty; blocks / WAYS],
            blocks: vec![0; blocks * BLOCK_SIZE],
            stats: Stats::default(),
        }
    }

    /// The heap bytes the cache holds, all allocated by [`Cache::new`]:
    /// [`BLOCK_SIZE`] per block and one set header per [`WAYS`] blocks.
    pub fn memory(&self) -> usize {
        self.blocks.capacity() + self.sets.capacity() * size_of::<Set>()
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Hands the counters to `sink` as the metrics of `shard`:
    /// `block_cache_hits`, `block_cache_misses`, `block_cache_evictions` and
    /// `block_cache_hit_rate_ppm`.
    pub fn export(&self, shard: u32, sink: &mut impl Sink) {
        let s = self.stats;
        sink.counter(shard, None, "block_cache_hits", s.hits);
        sink.counter(shard, None, "block_cache_misses", s.misses);
        sink.counter(shard, None, "block_cache_evictions", s.evictions);
        sink.counter(shard, None, "block_cache_hit_rate_ppm", s.hit_rate_ppm());
    }

    /// The block `reference` names, if the cache holds it verified against
    /// that checksum: the whole block, header included.
    pub fn get(&mut self, reference: Reference) -> Option<&[u8]> {
        let s = self.set(reference.address);
        let set = &mut self.sets[s];
        match find(set, reference.address) {
            // An empty slot keeps a stale checksum: the empty address never hits.
            Some(w) if reference.address != EMPTY && set.checksums[w] == reference.checksum => {
                set.marks |= 1 << w;
                self.stats.hits += 1;
                let at = (s * WAYS + w) * BLOCK_SIZE;
                Some(&self.blocks[at..at + BLOCK_SIZE])
            }
            _ => {
                self.stats.misses += 1;
                None
            }
        }
    }

    /// Keeps `block`, read and verified against `reference` by the grid,
    /// in place of any block cached at its address; when the set is full,
    /// evicts one.
    pub fn insert(&mut self, reference: Reference, block: &[u8]) {
        assert_eq!(block.len(), BLOCK_SIZE);
        assert_ne!(reference.address, EMPTY);
        // The grid header starts with the block's checksum.
        let sealed = u128::from_le_bytes(block[..16].try_into().expect("16 bytes"));
        assert_eq!(
            sealed, reference.checksum,
            "the block is the referenced one"
        );
        let s = self.set(reference.address);
        let set = &mut self.sets[s];
        let w = match find(set, reference.address).or_else(|| find(set, EMPTY)) {
            Some(w) => w,
            None => {
                self.stats.evictions += 1;
                clock(set)
            }
        };
        set.addresses[w] = reference.address;
        set.checksums[w] = reference.checksum;
        set.marks &= !(1 << w);
        let at = (s * WAYS + w) * BLOCK_SIZE;
        self.blocks[at..at + BLOCK_SIZE].copy_from_slice(block);
    }

    /// Forgets the block at `address`, which the free set released: its
    /// next content must come from the device.
    pub fn release(&mut self, address: u64) {
        let s = self.set(address);
        let set = &mut self.sets[s];
        if let Some(w) = find(set, address) {
            set.addresses[w] = EMPTY;
            set.marks &= !(1 << w);
        }
    }

    /// The set of `address`: a multiplicative hash, so that neighbouring
    /// addresses spread over the sets, scaled to their number.
    fn set(&self, address: u64) -> usize {
        let hash = address.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        ((u128::from(hash) * self.sets.len() as u128) >> 64) as usize
    }
}

fn find(set: &Set, address: u64) -> Option<usize> {
    set.addresses.iter().position(|&a| a == address)
}

/// Moves the hand to the first unmarked slot, clearing the marks it passes,
/// and returns that slot. It stops within one turn plus one slot.
fn clock(set: &mut Set) -> usize {
    loop {
        let w = usize::from(set.hand);
        set.hand = ((w + 1) % WAYS) as u8;
        if set.marks & (1 << w) == 0 {
            return w;
        }
        set.marks &= !(1 << w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block whose header carries `checksum` and whose content is `fill`.
    fn block(checksum: u128, fill: u8) -> Vec<u8> {
        let mut b = vec![fill; BLOCK_SIZE];
        b[..16].copy_from_slice(&checksum.to_le_bytes());
        b
    }

    fn reference(address: u64, checksum: u128) -> Reference {
        Reference { address, checksum }
    }

    #[derive(Default)]
    struct Counters(Vec<(&'static str, u64)>);
    impl Sink for Counters {
        fn histogram(
            &mut self,
            _: u32,
            _: Option<granite_io::event_loop::Class>,
            _: &'static str,
            _: &granite_io::histogram::Histogram,
        ) {
            unreachable!("the cache has no histogram")
        }
        fn counter(
            &mut self,
            shard: u32,
            class: Option<granite_io::event_loop::Class>,
            name: &'static str,
            value: u64,
        ) {
            assert_eq!((shard, class), (3, None));
            self.0.push((name, value));
        }
    }

    /// Addresses that share set 0 of a one-set cache: every address.
    #[test]
    fn counters_follow_a_known_sequence() {
        let mut cache = Cache::new(WAYS);
        assert_eq!(cache.stats().hit_rate_ppm(), 0);
        for a in 0..WAYS as u64 {
            assert!(cache.get(reference(a, 1)).is_none());
            cache.insert(reference(a, 1), &block(1, a as u8));
        }
        // Hit the first half twice: they are marked, the second half is not.
        for _ in 0..2 {
            for a in 0..WAYS as u64 / 2 {
                assert_eq!(cache.get(reference(a, 1)).unwrap()[100], a as u8);
            }
        }
        // A wrong checksum is a miss.
        assert!(cache.get(reference(0, 2)).is_none());
        // The set is full: two new blocks evict the two first unmarked slots.
        cache.insert(reference(100, 1), &block(1, 100));
        cache.insert(reference(101, 1), &block(1, 101));
        assert!(cache.get(reference(WAYS as u64 / 2, 1)).is_none());
        assert!(cache.get(reference(WAYS as u64 / 2 + 1, 1)).is_none());
        assert!(cache.get(reference(0, 1)).is_some());
        assert!(cache.get(reference(100, 1)).is_some());
        // An overwrite and a release are not evictions.
        cache.insert(reference(100, 2), &block(2, 7));
        cache.release(101);
        cache.insert(reference(102, 1), &block(1, 102));
        let stats = cache.stats();
        let hits = (WAYS as u64 / 2) * 2 + 2;
        let misses = WAYS as u64 + 1 + 2;
        assert_eq!(
            stats,
            Stats {
                hits,
                misses,
                evictions: 2
            }
        );
        assert_eq!(stats.hit_rate_ppm(), hits * 1_000_000 / (hits + misses));
        let mut sink = Counters::default();
        cache.export(3, &mut sink);
        assert_eq!(
            sink.0,
            [
                ("block_cache_hits", hits),
                ("block_cache_misses", misses),
                ("block_cache_evictions", 2),
                ("block_cache_hit_rate_ppm", stats.hit_rate_ppm()),
            ]
        );
    }

    #[test]
    fn a_released_and_reused_address_returns_the_new_content() {
        let mut cache = Cache::new(2 * WAYS);
        cache.insert(reference(5, 1), &block(1, 0xaa));
        cache.release(5);
        assert!(cache.get(reference(5, 1)).is_none(), "released");
        cache.insert(reference(5, 2), &block(2, 0xbb));
        assert!(cache.get(reference(5, 1)).is_none(), "the old checksum");
        assert_eq!(cache.get(reference(5, 2)).unwrap()[BLOCK_SIZE - 1], 0xbb);
        // Reused without a release in between: the overwrite replaces it too.
        cache.insert(reference(5, 3), &block(3, 0xcc));
        assert!(cache.get(reference(5, 2)).is_none());
        assert_eq!(cache.get(reference(5, 3)).unwrap()[BLOCK_SIZE - 1], 0xcc);
    }

    #[test]
    fn the_empty_address_never_hits() {
        let mut cache = Cache::new(WAYS);
        assert!(cache.get(reference(EMPTY, 0)).is_none(), "a new cache");
        cache.insert(reference(1, 1), &block(1, 0));
        cache.release(1);
        assert!(cache.get(reference(EMPTY, 1)).is_none(), "a released slot");
    }

    #[test]
    fn the_hit_rate_does_not_overflow() {
        let stats = Stats {
            hits: u64::MAX / 2,
            misses: u64::MAX / 2,
            evictions: 0,
        };
        assert_eq!(stats.hit_rate_ppm(), 500_000);
    }

    #[test]
    #[should_panic(expected = "the block is the referenced one")]
    fn a_block_must_carry_the_referenced_checksum() {
        Cache::new(WAYS).insert(reference(1, 1), &block(2, 0));
    }

    #[test]
    fn the_hand_skips_a_marked_slot_once() {
        let mut set = Set {
            addresses: [0; WAYS],
            checksums: [0; WAYS],
            marks: 0b0000_0111,
            hand: 1,
        };
        assert_eq!(clock(&mut set), 3);
        assert_eq!((set.marks, set.hand), (0b0000_0001, 4));
        set.marks = 0xff;
        assert_eq!(clock(&mut set), 4, "a full turn clears every mark");
        assert_eq!(set.marks, 0);
    }
}
