//! After construction, the memtables never touch the heap.
//!
//! This test binary installs the io tests' global allocator, which counts the
//! allocations of each thread, so the test harness's own threads do not
//! disturb the count.

#[path = "../../io/tests/common/counting_alloc.rs"]
mod counting_alloc;

use granite_lsm::memtable::Memtables;

#[test]
fn a_million_inserts_do_not_allocate() {
    let (inserts, capacity) = if cfg!(miri) {
        (1_000u64, 60_000)
    } else {
        (1_000_000, 64 << 20)
    };
    let mut memtables = Memtables::new(capacity);
    let before = counting_alloc::allocations();
    let mut x = 0u64;
    for i in 0..inserts {
        // A permutation of 0..2^64, so keys arrive out of order and never repeat.
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        memtables
            .insert(&x.to_be_bytes(), &i.to_le_bytes())
            .expect("the arena has room");
        assert_eq!(memtables.get(&x.to_be_bytes()), Some(&i.to_le_bytes()[..]));
    }
    // Overwrites, iteration, freeze, release and a full arena do not allocate either.
    memtables.insert(&x.to_be_bytes(), &[0; 16]).unwrap();
    memtables.freeze();
    let (mutable, frozen) = memtables.split();
    assert_eq!(frozen.unwrap().iter().count() as u64, inserts);
    while mutable.insert(&x.to_be_bytes(), &[0; 3_000]).is_ok() {}
    memtables.release();
    let after = counting_alloc::allocations();
    assert_eq!(after - before, 0, "allocations during {inserts} inserts");
}
