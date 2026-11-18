//! After the pool is built, acquire and release never touch the heap.
//!
//! This test binary installs a global allocator that counts the allocations
//! of each thread, so the test harness's own threads do not disturb the count.

#![allow(unsafe_code, reason = "a global allocator is an unsafe trait")]

use granite_io::message_pool::MessagePool;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    // `const` initialisation, so reading it never allocates.
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

fn count() {
    ALLOCATIONS.set(ALLOCATIONS.get() + 1);
}

// SAFETY: every call is forwarded unchanged to the system allocator; the
// counter has no effect on the memory handed out.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: the caller's contract for `alloc` is passed on.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: the caller's contract for `alloc_zeroed` is passed on.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: the caller's contract for `realloc` is passed on.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract for `dealloc` is passed on.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn acquire_and_release_do_not_allocate() {
    let cycles: u32 = if cfg!(miri) { 1_000 } else { 1_000_000 };
    let pool = MessagePool::new(4);
    let before = ALLOCATIONS.get();
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
    let after = ALLOCATIONS.get();
    assert_eq!(after - before, 0, "allocations during {cycles} cycles");
    assert_eq!(pool.counters().in_use, 0);
}
