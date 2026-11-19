//! After the pool is built, acquire and release never touch the heap.
//!
//! This test binary installs a global allocator that counts the allocations
//! of each thread, so the test harness's own threads do not disturb the count.

#[path = "common/counting_alloc.rs"]
mod counting_alloc;

use granite_io::message_pool::MessagePool;

#[test]
fn acquire_and_release_do_not_allocate() {
    let cycles: u32 = if cfg!(miri) { 1_000 } else { 1_000_000 };
    let pool = MessagePool::new(4);
    let before = counting_alloc::allocations();
    for i in 0..cycles {
        let mut a = pool.acquire().expect("pool has room");
        let b = pool.acquire().expect("pool has room");
        a.bytes_mut().expect("only handle")[0] = i as u8;
        let shared = a.clone();
        assert_eq!(shared.bytes()[0], i as u8);
        drop((a, b, shared));
    }
    // Exhaustion is not an allocation either.
    let held: [_; 4] = std::array::from_fn(|_| pool.acquire().unwrap());
    assert!(pool.acquire().is_none());
    drop(held);
    let after = counting_alloc::allocations();
    assert_eq!(after - before, 0, "allocations during {cycles} cycles");
    assert_eq!(pool.counters().in_use, 0);
}
