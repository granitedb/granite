//! A global allocator that counts the allocations of each thread, shared by
//! the tests that check how often code allocates. Every test binary that
//! includes this module installs it; counting per thread keeps the test
//! harness's own threads out of the count.

#![allow(unsafe_code, reason = "a global allocator is an unsafe trait")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    // `const` initialisation, so reading it never allocates.
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

/// Allocations made by the calling thread so far.
pub fn allocations() -> u64 {
    ALLOCATIONS.get()
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
