//! A fixed pool of message buffers (document 01 section 8).
//!
//! All buffers are allocated once, when the pool is built. Acquiring a buffer
//! never allocates and never blocks: when every buffer is in use it returns
//! `None`, which the caller turns into backpressure. A [`Message`] handle is
//! reference counted, so one buffer can sit in a WAL write and in a network
//! send at once; dropping the last handle returns the buffer to the pool.
//!
//! The pool belongs to one shard thread, so it is neither `Send` nor `Sync`.

// The pool owns one raw allocation and hands out disjoint buffers from it.
#![allow(unsafe_code, reason = "a pool may use unsafe (docs/06 section 3)")]

use granite_base::constants::FORMAT;
use std::alloc::{self, Layout};
use std::cell::{Cell, RefCell};
use std::fmt;
use std::ptr::NonNull;
use std::rc::Rc;

/// Size of every buffer: the largest message, header included.
pub const MESSAGE_SIZE: usize = FORMAT.message_size_max as usize;
/// Alignment of every buffer, so that it can go to direct I/O as it is.
pub const SECTOR_SIZE: usize = FORMAT.sector_size as usize;

const _: () = assert!(MESSAGE_SIZE.is_multiple_of(SECTOR_SIZE));

/// A pool of `capacity` sector-aligned buffers of [`MESSAGE_SIZE`] bytes.
pub struct MessagePool {
    inner: Rc<Inner>,
}

struct Inner {
    /// Start of `capacity * MESSAGE_SIZE` zeroed bytes, aligned to the sector.
    memory: NonNull<u8>,
    layout: Layout,
    /// Handles alive per buffer; zero means the buffer is free.
    references: Box<[Cell<u32>]>,
    /// Indexes of free buffers. Its capacity is the pool's, so it never grows.
    free: RefCell<Vec<u32>>,
    high_water: Cell<u32>,
    acquire_failures: Cell<u64>,
}

/// Counters of a pool, for metrics and tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolCounters {
    /// Buffers currently held by at least one handle.
    pub in_use: u32,
    /// Most buffers ever in use at once.
    pub high_water: u32,
    /// Acquisitions that returned `None` because the pool was empty.
    pub acquire_failures: u64,
}

impl MessagePool {
    /// Allocates every buffer of the pool. This is the pool's only
    /// allocation; nothing allocates afterwards.
    ///
    /// Panics if `capacity` is zero or the memory cannot be allocated, which
    /// happens only at startup.
    pub fn new(capacity: u32) -> Self {
        assert!(capacity > 0, "a message pool has at least one buffer");
        let size = MESSAGE_SIZE
            .checked_mul(capacity as usize)
            .expect("pool size fits in usize");
        let layout = Layout::from_size_align(size, SECTOR_SIZE).expect("valid pool layout");
        // SAFETY: `layout` has a non-zero size, since capacity and
        // MESSAGE_SIZE are both positive.
        let memory = unsafe { alloc::alloc_zeroed(layout) };
        let memory = NonNull::new(memory).unwrap_or_else(|| alloc::handle_alloc_error(layout));
        // Popping from the end hands out buffer 0 first.
        let free = (0..capacity).rev().collect::<Vec<_>>();
        MessagePool {
            inner: Rc::new(Inner {
                memory,
                layout,
                references: (0..capacity).map(|_| Cell::new(0)).collect(),
                free: RefCell::new(free),
                high_water: Cell::new(0),
                acquire_failures: Cell::new(0),
            }),
        }
    }

    /// Number of buffers in the pool.
    pub fn capacity(&self) -> u32 {
        self.inner.capacity()
    }

    /// Takes a free buffer, or returns `None` when every buffer is in use.
    /// Never allocates, blocks or panics on an empty pool. The buffer holds
    /// whatever its previous user wrote.
    pub fn acquire(&self) -> Option<Message> {
        let inner = &self.inner;
        let Some(index) = inner.free.borrow_mut().pop() else {
            inner.acquire_failures.set(inner.acquire_failures.get() + 1);
            return None;
        };
        let references = &inner.references[index as usize];
        assert_eq!(references.get(), 0, "a free buffer has no handle");
        references.set(1);
        let in_use = inner.in_use();
        if in_use > inner.high_water.get() {
            inner.high_water.set(in_use);
        }
        Some(Message {
            pool: Rc::clone(inner),
            index,
        })
    }

    /// The pool's counters.
    pub fn counters(&self) -> PoolCounters {
        PoolCounters {
            in_use: self.inner.in_use(),
            high_water: self.inner.high_water.get(),
            acquire_failures: self.inner.acquire_failures.get(),
        }
    }
}

impl fmt::Debug for MessagePool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MessagePool")
            .field("capacity", &self.capacity())
            .field("counters", &self.counters())
            .finish()
    }
}

impl Inner {
    fn capacity(&self) -> u32 {
        self.references.len() as u32
    }

    fn in_use(&self) -> u32 {
        self.capacity() - self.free.borrow().len() as u32
    }

    fn buffer(&self, index: u32) -> *mut u8 {
        assert!(index < self.capacity());
        // SAFETY: index < capacity, so the offset stays inside the
        // allocation of capacity * MESSAGE_SIZE bytes.
        let buffer = unsafe { self.memory.as_ptr().add(index as usize * MESSAGE_SIZE) };
        assert!(buffer.addr().is_multiple_of(SECTOR_SIZE));
        buffer
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Every handle holds an `Rc` to this, so none is left.
        assert_eq!(self.in_use(), 0);
        // SAFETY: `memory` was allocated in `MessagePool::new` with `layout`
        // and is freed only here.
        unsafe { alloc::dealloc(self.memory.as_ptr(), self.layout) };
    }
}

/// A handle to one buffer of a [`MessagePool`]. Cloning it shares the
/// buffer; the buffer returns to the pool when the last handle is dropped.
pub struct Message {
    pool: Rc<Inner>,
    index: u32,
}

impl Message {
    /// The whole buffer, [`MESSAGE_SIZE`] bytes, aligned to [`SECTOR_SIZE`].
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the buffer lies inside the pool's allocation, which lives
        // as long as `self.pool`. Its bytes were zeroed at allocation, so
        // they are initialised. A mutable view exists only through
        // `bytes_mut`, which needs `&mut self` on the only handle, so no
        // mutable view of this buffer is alive while `self` is borrowed.
        unsafe { std::slice::from_raw_parts(self.pool.buffer(self.index), MESSAGE_SIZE) }
    }

    /// The whole buffer for writing, or `None` while another handle shares
    /// it.
    pub fn bytes_mut(&mut self) -> Option<&mut [u8]> {
        if self.pool.references[self.index as usize].get() != 1 {
            return None;
        }
        // SAFETY: as in `bytes`; and this is the only handle to the buffer,
        // borrowed mutably, so no other view of the buffer can exist while
        // the returned slice lives. Other buffers are disjoint ranges.
        Some(unsafe { std::slice::from_raw_parts_mut(self.pool.buffer(self.index), MESSAGE_SIZE) })
    }

    /// Number of handles that share this buffer.
    pub fn references(&self) -> u32 {
        self.pool.references[self.index as usize].get()
    }
}

impl Clone for Message {
    fn clone(&self) -> Self {
        let references = &self.pool.references[self.index as usize];
        assert!(references.get() > 0);
        references.set(references.get().checked_add(1).expect("handle count fits"));
        Message {
            pool: Rc::clone(&self.pool),
            index: self.index,
        }
    }
}

impl Drop for Message {
    fn drop(&mut self) {
        let references = &self.pool.references[self.index as usize];
        assert!(references.get() > 0, "a buffer is not released twice");
        references.set(references.get() - 1);
        if references.get() == 0 {
            let mut free = self.pool.free.borrow_mut();
            assert!(
                free.len() < self.pool.references.len(),
                "free list never grows"
            );
            free.push(self.index);
        }
    }
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Message")
            .field("index", &self.index)
            .field("references", &self.references())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_pool_returns_none() {
        let pool = MessagePool::new(2);
        let a = pool.acquire().unwrap();
        let b = pool.acquire().unwrap();
        assert!(pool.acquire().is_none());
        assert!(pool.acquire().is_none());
        assert_eq!(pool.counters().acquire_failures, 2);
        drop(a);
        let c = pool.acquire().expect("a released buffer is reused");
        drop((b, c));
    }

    #[test]
    fn buffers_are_sector_aligned_and_sized() {
        let pool = MessagePool::new(3);
        let messages: Vec<_> = (0..3).map(|_| pool.acquire().unwrap()).collect();
        for message in &messages {
            let bytes = message.bytes();
            assert!(bytes.as_ptr().addr().is_multiple_of(SECTOR_SIZE));
            assert!(bytes.len().is_multiple_of(SECTOR_SIZE));
            assert_eq!(bytes.len(), MESSAGE_SIZE);
        }
    }

    #[test]
    fn counters_after_a_known_sequence() {
        let pool = MessagePool::new(3);
        let zero = PoolCounters {
            in_use: 0,
            high_water: 0,
            acquire_failures: 0,
        };
        assert_eq!(pool.counters(), zero);
        let a = pool.acquire().unwrap();
        let b = pool.acquire().unwrap();
        let a2 = a.clone();
        drop(a);
        let c = pool.acquire().unwrap();
        assert!(pool.acquire().is_none());
        drop((b, c));
        let expected = PoolCounters {
            in_use: 1,
            high_water: 3,
            acquire_failures: 1,
        };
        assert_eq!(pool.counters(), expected);
        drop(a2);
        let expected = PoolCounters {
            in_use: 0,
            ..expected
        };
        assert_eq!(pool.counters(), expected);
    }

    #[test]
    fn shared_buffer_is_read_only_until_unshared() {
        let pool = MessagePool::new(1);
        let mut a = pool.acquire().unwrap();
        a.bytes_mut().unwrap()[..3].copy_from_slice(b"wal");
        let b = a.clone();
        assert_eq!(a.references(), 2);
        assert!(a.bytes_mut().is_none());
        assert_eq!(&b.bytes()[..3], b"wal");
        drop(b);
        assert!(a.bytes_mut().is_some());
    }

    #[test]
    fn writes_to_one_buffer_leave_others_alone() {
        let pool = MessagePool::new(2);
        let mut a = pool.acquire().unwrap();
        let mut b = pool.acquire().unwrap();
        a.bytes_mut().unwrap().fill(1);
        b.bytes_mut().unwrap().fill(2);
        assert!(a.bytes().iter().all(|&x| x == 1));
        assert!(b.bytes().iter().all(|&x| x == 2));
    }

    #[test]
    fn handles_keep_the_memory_alive_after_the_pool() {
        let pool = MessagePool::new(1);
        let mut a = pool.acquire().unwrap();
        drop(pool);
        a.bytes_mut().unwrap()[0] = 7;
        assert_eq!(a.bytes()[0], 7);
    }

    /// Random acquire, clone and drop against a reference count per buffer.
    /// A leak shows as a buffer the model calls free that the pool cannot
    /// hand out; a double free as two live buffers with the same address.
    #[test]
    fn model_random_acquire_clone_drop() {
        const SEED: u64 = 0x0067_7261_6e69_7465;
        let (cases, ops) = if cfg!(miri) { (4, 200) } else { (1_000, 1_000) };
        println!("model seed {SEED:#x}, {cases} cases of {ops} ops");
        let mut rng = SplitMix64(SEED);
        for _ in 0..cases {
            let capacity = 1 + (rng.next() % 8) as u32;
            let pool = MessagePool::new(capacity);
            let mut handles: Vec<Message> = Vec::new();
            // Model: live handle count per buffer address.
            let mut model: std::collections::BTreeMap<usize, u32> = Default::default();
            for _ in 0..ops {
                match rng.next() % 3 {
                    0 => match pool.acquire() {
                        Some(message) => {
                            let address = message.bytes().as_ptr().addr();
                            assert!(
                                model.insert(address, 1).is_none(),
                                "buffer handed out twice"
                            );
                            handles.push(message);
                        }
                        None => assert_eq!(model.len(), capacity as usize, "leak"),
                    },
                    1 if !handles.is_empty() => {
                        let message = handles[rng.next() as usize % handles.len()].clone();
                        *model.get_mut(&message.bytes().as_ptr().addr()).unwrap() += 1;
                        handles.push(message);
                    }
                    _ if !handles.is_empty() => {
                        let message = handles.swap_remove(rng.next() as usize % handles.len());
                        let address = message.bytes().as_ptr().addr();
                        drop(message);
                        let count = model.get_mut(&address).unwrap();
                        *count -= 1;
                        if *count == 0 {
                            model.remove(&address);
                        }
                    }
                    _ => {}
                }
                for message in &handles {
                    assert_eq!(
                        message.references(),
                        model[&message.bytes().as_ptr().addr()]
                    );
                }
                assert_eq!(pool.counters().in_use as usize, model.len());
            }
            drop(handles);
            assert_eq!(pool.counters().in_use, 0);
            // Every buffer comes back: the pool can hand out all of them again.
            let all: Vec<_> = (0..capacity).map(|_| pool.acquire().unwrap()).collect();
            assert!(pool.acquire().is_none());
            drop(all);
        }
    }

    /// Small deterministic generator for the model test.
    struct SplitMix64(u64);

    impl SplitMix64 {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }
    }
}
