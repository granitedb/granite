//! Tests of the memtable, from a logged seed. Rerun a failure with
//! `GRANITE_SEED=<seed> cargo test -p granite-lsm`.

use granite_lsm::memtable::{Full, Memtable, Memtables};
use std::collections::BTreeMap;

/// The head node, the arena's fixed overhead.
const HEAD_SIZE: usize = 61;

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn new(test: &str) -> Self {
        let seed = std::env::var("GRANITE_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0x5EED);
        println!("{test}: GRANITE_SEED={seed}");
        Rng(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// Up to 3 bytes from a 4-letter alphabet: 85 keys, so duplicates, prefixes
    /// and neighbours are common.
    fn key(&mut self) -> Vec<u8> {
        let len = self.below(4);
        (0..len)
            .map(|_| [0, 1, 0x7F, 0xFF][self.below(4) as usize])
            .collect()
    }

    fn value(&mut self) -> Vec<u8> {
        let len = self.below(17);
        (0..len).map(|_| self.next() as u8).collect()
    }
}

#[test]
fn matches_a_btreemap() {
    let (cases, ops) = if cfg!(miri) { (2, 200) } else { (1_000, 1_000) };
    let mut rng = Rng::new("matches_a_btreemap");
    let (mut fulls, mut ranges) = (0u64, 0u64);
    for _ in 0..cases {
        // Small arenas fill up mid-case, large ones never do.
        let capacity = HEAD_SIZE as u32 + [500, 2_000, 60_000][rng.below(3) as usize];
        let mut memtable = Memtable::new(capacity);
        let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for _ in 0..ops {
            match rng.below(5) {
                0..=2 => {
                    let (key, value) = (rng.key(), rng.value());
                    let before = (memtable.bytes().len(), memtable.len());
                    match memtable.insert(&key, &value) {
                        Ok(()) => drop(model.insert(key, value)),
                        Err(Full) => {
                            fulls += 1;
                            assert_eq!((memtable.bytes().len(), memtable.len()), before);
                        }
                    }
                }
                3 => {
                    let key = rng.key();
                    assert_eq!(
                        memtable.get(&key),
                        model.get(&key).map(Vec::as_slice),
                        "get {key:?}"
                    );
                }
                _ => {
                    let (start, end) = (rng.key(), rng.key());
                    let got: Vec<_> = memtable
                        .iter_from(&start)
                        .take_while(|(k, _)| *k < &end[..])
                        .collect();
                    let want: Vec<_> = if start <= end {
                        model
                            .range(start.clone()..end.clone())
                            .map(|(k, v)| (&k[..], &v[..]))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    ranges += u64::from(want.is_empty());
                    assert_eq!(got, want, "range {start:?}..{end:?}");
                }
            }
            assert!(memtable.bytes().len() <= capacity as usize);
        }
        let all: Vec<_> = model.iter().map(|(k, v)| (&k[..], &v[..])).collect();
        assert_eq!(memtable.iter().collect::<Vec<_>>(), all);
        assert_eq!(memtable.len() as usize, model.len());
    }
    // The run covered the cases the gate names.
    assert!(
        fulls > 0 && ranges > 0,
        "{fulls} full inserts, {ranges} empty ranges"
    );
}

#[test]
fn a_rejected_insert_changes_nothing() {
    let capacity = 4_000;
    let (mut a, mut b) = (Memtable::new(capacity), Memtable::new(capacity));
    let big = [7u8; 200];
    let mut i = 0u32;
    // Fill both with the same rows until a 200-byte value no longer fits in `a`.
    loop {
        let key = i.to_be_bytes();
        let before = a.bytes().to_vec();
        if a.insert(&key, &big) == Err(Full) {
            assert_eq!(a.bytes(), before);
            // It really did not fit: even the smallest node would have needed more.
            assert!(capacity as usize - a.bytes().len() < HEAD_SIZE + key.len() + big.len());
            // Nor does a new value for an existing key.
            assert_eq!(a.insert(&0u32.to_be_bytes(), &big), Err(Full));
            assert_eq!(a.bytes(), before);
            assert_eq!(a.get(&0u32.to_be_bytes()), Some(&big[..]));
            break;
        }
        b.insert(&key, &big).unwrap();
        i += 1;
    }
    assert_eq!(a.bytes(), b.bytes());
    // The rejected insert did not advance the level generator: small rows go
    // to the same places in both, up to the last byte.
    loop {
        i += 1;
        let (ra, rb) = (
            a.insert(&i.to_be_bytes(), &[]),
            b.insert(&i.to_be_bytes(), &[]),
        );
        assert_eq!(ra, rb);
        assert_eq!(a.bytes(), b.bytes());
        if ra == Err(Full) {
            break;
        }
    }
    assert!(a.bytes().len() <= capacity as usize);
    assert_eq!(a.capacity(), capacity);
}

#[test]
fn same_inserts_give_the_same_bytes() {
    let mut rng = Rng::new("same_inserts_give_the_same_bytes");
    let rows: Vec<(Vec<u8>, Vec<u8>)> = (0..if cfg!(miri) { 300 } else { 10_000 })
        .map(|_| {
            let key = rng.next().to_be_bytes()[..1 + rng.below(8) as usize].to_vec();
            (key, rng.value())
        })
        .collect();
    let capacity = 1_000_000;
    let (mut a, mut b) = (Memtable::new(capacity), Memtable::new(capacity));
    // `b` is reused: its arena holds stale bytes from an earlier, different fill.
    for i in 0..500u32 {
        b.insert(&i.to_le_bytes(), &[1; 9]).unwrap();
    }
    b.reset();
    for (key, value) in &rows {
        a.insert(key, value).unwrap();
        b.insert(key, value).unwrap();
    }
    assert_eq!(a.bytes(), b.bytes());
    assert!(
        !a.is_empty() && a.len() < rows.len() as u32,
        "the rows have duplicate keys"
    );
}

#[test]
fn a_frozen_memtable_iterates_unchanged_while_writes_continue() {
    let mut rng = Rng::new("a_frozen_memtable_iterates_unchanged_while_writes_continue");
    let mut memtables = Memtables::new(60_000);
    let mut frozen_model = BTreeMap::new();
    for _ in 0..300 {
        let (key, value) = (rng.key(), rng.value());
        memtables.insert(&key, &value).unwrap();
        frozen_model.insert(key, value);
    }
    memtables.freeze();
    let mut model = frozen_model.clone();
    let (mutable, frozen) = memtables.split();
    assert!(mutable.is_empty());
    let mut seen = Vec::new();
    // Every step of the iteration is followed by writes, many to the same keys.
    for (key, value) in frozen.unwrap().iter() {
        seen.push((key.to_vec(), value.to_vec()));
        for _ in 0..3 {
            let (key, value) = (rng.key(), rng.value());
            mutable.insert(&key, &value).unwrap();
            model.insert(key, value);
        }
    }
    assert_eq!(seen, frozen_model.into_iter().collect::<Vec<_>>());
    // Reads see the newest value, from either memtable.
    for (key, value) in &model {
        assert_eq!(memtables.get(key), Some(&value[..]));
    }
    memtables.release();
    assert_eq!(memtables.split().1.map(Memtable::len), None);
    // The released arena is the spare for the next freeze.
    memtables.freeze();
    assert!(memtables.split().0.is_empty());
}
