//! The memtable: a sorted, row-oriented buffer of fixed capacity that receives
//! writes before they are flushed to level 0 (docs/01-storage.md section 3).
//!
//! A skip list whose nodes, keys and values all live in one byte arena
//! allocated at construction. Links are `u32` offsets into the arena, so the
//! contents never depend on allocation addresses, and node levels come from a
//! generator seeded by a fixed constant: two memtables given the same inserts
//! hold the same bytes. An insert that does not fit returns [`Full`] and
//! changes nothing.
//!
//! Node layout, little-endian, at offset `n`:
//!
//! | Bytes | Field |
//! |---|---|
//! | 0..4 | key length |
//! | 4..8 | value offset |
//! | 8..12 | value length |
//! | 12 | height `h`, 1 to [`MAX_HEIGHT`] |
//! | 13..13+4h | offset of the next node at each level, 0 at the end |
//! | then | key |
//!
//! The head node sits at offset 0 with the full height and an empty key, so
//! offset 0 also serves as "no next node". The value of a new key follows its
//! key; a later insert of the same key appends the new value and repoints the
//! node, so each key appears once and the newest value wins.

const MAX_HEIGHT: usize = 12;
const HEIGHT: usize = 12;
const NEXT: usize = 13;
const HEAD: usize = 0;
const END: usize = 0;
const HEAD_SIZE: usize = NEXT + 4 * MAX_HEIGHT;
/// Seeds the level generator; the same on every replica.
const SEED: u64 = 0x6D65_6D74_6162_6C65;

/// The insert did not fit in the arena; the memtable is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

/// A sorted map from byte-string keys to byte-string values in a fixed arena.
pub struct Memtable {
    arena: Vec<u8>,
    used: usize,
    len: u32,
    rng: u64,
}

impl Memtable {
    /// A memtable whose arena holds `capacity` bytes, including 61 bytes of
    /// head node. The arena is the only allocation the memtable ever makes.
    pub fn new(capacity: u32) -> Self {
        assert!(
            capacity as usize >= HEAD_SIZE,
            "capacity {capacity} below the head node"
        );
        let mut memtable = Self {
            arena: vec![0; capacity as usize],
            used: 0,
            len: 0,
            rng: 0,
        };
        memtable.reset();
        memtable
    }

    /// Empties the memtable for reuse, keeping its arena.
    pub fn reset(&mut self) {
        self.arena[..HEAD_SIZE].fill(0);
        self.arena[HEAD + HEIGHT] = MAX_HEIGHT as u8;
        self.used = HEAD_SIZE;
        self.len = 0;
        self.rng = SEED;
    }

    /// Inserts `key` with `value`, replacing the value of an existing key.
    pub fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<(), Full> {
        let mut preds = [HEAD; MAX_HEIGHT];
        let found = self.seek_node(key, &mut preds);
        if found != END && self.key(found) == key {
            let at = self.append(&[value]).ok_or(Full)?;
            self.set_u32(found + 4, at as u32);
            self.set_u32(found + 8, value.len() as u32);
            return Ok(());
        }

        // Draw the height from a copy, so a rejected insert leaves the generator as it was.
        let mut rng = self.rng;
        let height = 1 + (split_mix(&mut rng).trailing_zeros() as usize / 2).min(MAX_HEIGHT - 1);
        let mut header = [0u8; HEAD_SIZE];
        let size = NEXT + 4 * height;
        header[0..4].copy_from_slice(&(key.len() as u32).to_le_bytes());
        header[HEIGHT] = height as u8;
        let node = self.append(&[&header[..size], key, value]).ok_or(Full)?;
        self.rng = rng;
        self.set_u32(node + 4, (node + size + key.len()) as u32);
        self.set_u32(node + 8, value.len() as u32);

        assert!(
            preds[0] == HEAD || self.key(preds[0]) < key,
            "predecessor not below the key"
        );
        assert!(
            found == END || self.key(found) > key,
            "successor not above the key"
        );
        for (level, &pred) in preds.iter().enumerate().take(height) {
            self.set_u32(node + NEXT + 4 * level, self.next(pred, level) as u32);
            self.set_u32(pred + NEXT + 4 * level, node as u32);
        }
        self.len += 1;
        Ok(())
    }

    /// The value of `key`, if present.
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        let found = self.seek_node(key, &mut [HEAD; MAX_HEIGHT]);
        (found != END && self.key(found) == key).then(|| self.value(found))
    }

    /// The entries in key order, from the first key not below `start`.
    pub fn iter_from(&self, start: &[u8]) -> Iter<'_> {
        Iter {
            memtable: self,
            node: self.seek_node(start, &mut [HEAD; MAX_HEIGHT]),
        }
    }

    /// The entries in key order.
    pub fn iter(&self) -> Iter<'_> {
        self.iter_from(&[])
    }

    /// The number of distinct keys.
    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The used part of the arena: the whole state of the memtable apart from
    /// the generator, which is fixed by the number of keys inserted.
    pub fn bytes(&self) -> &[u8] {
        &self.arena[..self.used]
    }

    /// The arena size given to [`Memtable::new`].
    pub fn capacity(&self) -> u32 {
        self.arena.len() as u32
    }

    /// The first node whose key is not below `key`, or [`END`]; fills `preds`
    /// with the last node below `key` at each level.
    fn seek_node(&self, key: &[u8], preds: &mut [usize; MAX_HEIGHT]) -> usize {
        let mut node = HEAD;
        for level in (0..MAX_HEIGHT).rev() {
            loop {
                let next = self.next(node, level);
                if next == END || self.key(next) >= key {
                    break;
                }
                node = next;
            }
            preds[level] = node;
        }
        self.next(node, 0)
    }

    /// Copies `parts` to the end of the arena and returns where they start,
    /// or `None`, changing nothing, if they do not fit.
    fn append(&mut self, parts: &[&[u8]]) -> Option<usize> {
        let size: usize = parts.iter().map(|part| part.len()).sum();
        if size > self.arena.len() - self.used {
            return None;
        }
        let start = self.used;
        for part in parts {
            self.arena[self.used..self.used + part.len()].copy_from_slice(part);
            self.used += part.len();
        }
        Some(start)
    }

    fn u32_at(&self, at: usize) -> usize {
        u32::from_le_bytes(self.arena[at..at + 4].try_into().expect("four bytes")) as usize
    }

    fn set_u32(&mut self, at: usize, value: u32) {
        self.arena[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn next(&self, node: usize, level: usize) -> usize {
        self.u32_at(node + NEXT + 4 * level)
    }

    fn key(&self, node: usize) -> &[u8] {
        let start = node + NEXT + 4 * self.arena[node + HEIGHT] as usize;
        &self.arena[start..start + self.u32_at(node)]
    }

    fn value(&self, node: usize) -> &[u8] {
        let start = self.u32_at(node + 4);
        &self.arena[start..start + self.u32_at(node + 8)]
    }
}

/// The entries of a memtable in key order.
pub struct Iter<'a> {
    memtable: &'a Memtable,
    node: usize,
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a [u8], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.node == END {
            return None;
        }
        let item = (self.memtable.key(self.node), self.memtable.value(self.node));
        self.node = self.memtable.next(self.node, 0);
        Some(item)
    }
}

/// The two memtables of a tree: the mutable one that takes writes, and the
/// immutable one while it is flushed. Both arenas are allocated up front.
pub struct Memtables {
    mutable: Memtable,
    immutable: Memtable,
    frozen: bool,
}

impl Memtables {
    /// Two memtables of `capacity` bytes each.
    pub fn new(capacity: u32) -> Self {
        Self {
            mutable: Memtable::new(capacity),
            immutable: Memtable::new(capacity),
            frozen: false,
        }
    }

    /// Inserts into the mutable memtable.
    pub fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<(), Full> {
        self.mutable.insert(key, value)
    }

    /// The newest value of `key`: the mutable memtable first, then the frozen one.
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.mutable
            .get(key)
            .or_else(|| self.frozen.then(|| self.immutable.get(key)).flatten())
    }

    /// Freezes the mutable memtable for flushing; an empty one takes its place.
    pub fn freeze(&mut self) {
        assert!(!self.frozen, "a memtable is already being flushed");
        assert!(self.immutable.is_empty(), "the spare memtable is not empty");
        std::mem::swap(&mut self.mutable, &mut self.immutable);
        self.frozen = true;
    }

    /// Drops the frozen memtable once it is flushed; its arena becomes the spare.
    pub fn release(&mut self) {
        assert!(self.frozen, "no memtable is being flushed");
        self.immutable.reset();
        self.frozen = false;
    }

    /// The mutable memtable, and the frozen one if a flush is in progress.
    pub fn split(&mut self) -> (&mut Memtable, Option<&Memtable>) {
        (&mut self.mutable, self.frozen.then_some(&self.immutable))
    }
}

/// SplitMix64.
fn split_mix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
