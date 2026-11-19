//! The io_uring backend of [`Storage`], the production one (Linux only).
//!
//! One [`Uring`] per shard: one ring, the data file opened with
//! `O_DIRECT | O_DSYNC` and registered with the ring, and the shard's
//! [`MessagePool`] registered as one fixed buffer. Reads and writes are
//! `READ_FIXED` and `WRITE_FIXED` straight between the pool and the device;
//! after [`Uring::open`] the data file is never touched by a `read`, `write`,
//! `pread` or `pwrite` system call.
//!
//! Submissions are queued in the ring and handed to the kernel by the next
//! [`Uring::poll`], which also returns at most one completion. A short read
//! or write is resubmitted for the rest of its range, and `EINTR` or `EAGAIN`
//! for the whole of what is left, so the caller sees one completion per
//! operation. Any other error reaches it as [`StorageError`].
//!
//! The minimum kernel is [`MIN_KERNEL`], 5.6, checked by [`Uring::open`].
//! Storage needs 5.1 (registered files and buffers, fixed reads and writes,
//! fsync); 5.6 adds what the network half of this backend needs (accept and
//! connect in 5.5, send and receive in 5.6).
//!
//! Registered buffers count against `RLIMIT_MEMLOCK` (`ulimit -l`), so the
//! limit must cover the pool.

#![allow(
    unsafe_code,
    reason = "io_uring submissions and buffer registration are unsafe (docs/06 section 3)"
)]

use crate::message_pool::{MESSAGE_SIZE, Message, MessagePool, SECTOR_SIZE};
use crate::traits::{Completion, Full, Storage, StorageError, Tag, Zone};
use io_uring::{IoUring, opcode, squeue, types};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// The oldest kernel the backend runs on, as (major, minor).
pub const MIN_KERNEL: (u32, u32) = (5, 6);

/// The data file's only entry in the ring's registered files.
const FILE: types::Fixed = types::Fixed(0);
/// The pool's only entry in the ring's registered buffers.
const POOL: u16 = 0;

/// One storage backend: a ring, the data file and the registered pool.
pub struct Uring {
    ring: IoUring,
    /// Start of each zone in the file, in [`Zone::ALL`] order.
    starts: [u64; 4],
    sizes: [u64; 4],
    /// Operations in flight, indexed by the user data of their entries.
    slots: Box<[Option<Op>]>,
    /// Indexes of the empty slots; it never grows past the slot count.
    free: Vec<u32>,
    /// The registered memory: a handle that keeps it allocated, its start
    /// and its length.
    pool: (MessagePool, *mut u8, usize),
    /// The ring holds its own reference to the registered file; this keeps
    /// the descriptor for as long as the backend lives.
    _file: File,
    #[cfg(test)]
    fault: Option<Fault>,
}

/// An operation in flight.
struct Op {
    tag: Tag,
    kind: Kind,
    /// Position in the file and length of the whole range.
    position: u64,
    len: u32,
    /// Bytes already transferred; short transfers resubmit the rest.
    done: u32,
    /// First byte of the buffer.
    address: *mut u8,
}

enum Kind {
    Read(Message),
    Write(Message),
    Fsync,
}

/// A fault injected at the ring, for the tests of the retry and error paths.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
enum Fault {
    /// The next read or write entry asks for one sector only, so the kernel
    /// really transfers less than the operation's range.
    Short,
    /// The next completion carries this error number instead of its result.
    Errno(i32),
}

impl Uring {
    /// Opens (creating it if needed) the data file at `path`, with zones of
    /// `zone_sizes` bytes in [`Zone::ALL`] order, room for `capacity`
    /// operations in flight, and `pool` as the buffers. A file shorter than
    /// the zones is extended with zeros.
    ///
    /// Fails with a message naming the cause if the kernel is older than
    /// [`MIN_KERNEL`], io_uring is not allowed, the file cannot be opened
    /// for direct I/O, or the pool cannot be registered.
    pub fn open(
        path: &Path,
        zone_sizes: [u64; 4],
        capacity: u32,
        pool: &MessagePool,
    ) -> io::Result<Uring> {
        assert!(capacity > 0);
        let mut starts = [0; 4];
        let mut end = 0u64;
        for (start, size) in starts.iter_mut().zip(zone_sizes) {
            assert!(size.is_multiple_of(SECTOR_SIZE as u64));
            *start = end;
            end = end.checked_add(size).expect("the file size fits in u64");
        }
        let release = std::fs::read_to_string("/proc/sys/kernel/osrelease")?;
        check_kernel(release.trim())?;

        let context = |what: &str| {
            let what = what.to_owned();
            move |error: io::Error| io::Error::new(error.kind(), format!("{what}: {error}"))
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_DIRECT | libc::O_DSYNC)
            .open(path)
            .map_err(context(&format!(
                "opening {} with O_DIRECT | O_DSYNC",
                path.display()
            )))?;
        if file.metadata()?.len() < end {
            // The size, and for a new file its directory entry, are durable
            // before any write is reported done.
            let directory = path.parent().filter(|p| !p.as_os_str().is_empty());
            file.set_len(end)
                .and_then(|()| file.sync_all())
                .and_then(|()| File::open(directory.unwrap_or(Path::new(".")))?.sync_all())
                .map_err(context(&format!(
                    "sizing {} to {end} bytes",
                    path.display()
                )))?;
        }
        let ring = IoUring::new(capacity.next_power_of_two()).map_err(context(
            "creating an io_uring instance (is kernel.io_uring_disabled set?)",
        ))?;
        ring.submitter()
            .register_files(&[file.as_raw_fd()])
            .map_err(context("registering the data file"))?;
        let (pool, start, len) = pool.memory();
        let iovec = libc::iovec {
            iov_base: start.cast(),
            iov_len: len,
        };
        // SAFETY: the iovec covers exactly the pool's allocation, which
        // `self.pool` keeps allocated until the ring is gone (fields drop
        // after `Drop::drop`, which waits for every operation). The kernel
        // reaches the memory only through fixed reads and writes, whose
        // ranges are each inside one buffer held by the backend until the
        // operation completes.
        unsafe { ring.submitter().register_buffers(&[iovec]) }.map_err(context(
            "registering the message pool (does `ulimit -l` cover the pool?)",
        ))?;
        Ok(Uring {
            ring,
            starts,
            sizes: zone_sizes,
            slots: (0..capacity).map(|_| None).collect(),
            free: (0..capacity).rev().collect(),
            pool: (pool, start, len),
            _file: file,
            #[cfg(test)]
            fault: None,
        })
    }

    /// One completed operation, or `None` when none has completed. Hands the
    /// queued submissions to the kernel first; never blocks.
    pub fn poll(&mut self) -> Option<Completion> {
        loop {
            let mut entry = self.ring.completion().next();
            if entry.is_none() || !self.ring.submission().is_empty() {
                if self.free.len() == self.slots.len() {
                    return None;
                }
                self.enter();
                entry = entry.or_else(|| self.ring.completion().next());
            }
            let entry = entry?;
            let index = u32::try_from(entry.user_data()).expect("user data is a slot index");
            if let Some(completion) = self.complete(index, entry.result()) {
                return Some(completion);
            }
        }
    }

    /// Submits what is queued, which also lets the kernel post completions.
    fn enter(&mut self) {
        match self.ring.submit() {
            Ok(_) => {}
            // Interrupted or short of kernel memory: the entries stay queued
            // and the next poll submits them again.
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(libc::EINTR | libc::EAGAIN | libc::EBUSY)
                ) => {}
            Err(e) => panic!("io_uring_enter failed: {e}"),
        }
    }

    /// Handles the kernel's `result` for the operation in slot `index`, and
    /// returns its completion once nothing is left to resubmit.
    fn complete(&mut self, index: u32, result: i32) -> Option<Completion> {
        #[cfg(test)]
        let result = match self.fault {
            Some(Fault::Errno(errno)) => {
                self.fault = None;
                -errno
            }
            _ => result,
        };
        let op = self.slots[index as usize]
            .as_mut()
            .expect("a completion belongs to an operation in flight");
        let result = match result {
            e if e == -libc::EINTR || e == -libc::EAGAIN => {
                self.push(index);
                return None;
            }
            e if e < 0 => Err(StorageError),
            _ if matches!(op.kind, Kind::Fsync) => Ok(()),
            // No progress: past the end of the file, which open sized.
            0 => Err(StorageError),
            n => {
                let n = n.unsigned_abs();
                assert!(n <= op.len - op.done, "the kernel moved more than asked");
                op.done += n;
                if op.done < op.len {
                    self.push(index);
                    return None;
                }
                Ok(())
            }
        };
        let op = self.slots[index as usize].take().expect("checked");
        self.free.push(index);
        Some(match op.kind {
            Kind::Read(buffer) => Completion::Read {
                tag: op.tag,
                buffer,
                result,
            },
            Kind::Write(buffer) => Completion::Write {
                tag: op.tag,
                buffer,
                result,
            },
            Kind::Fsync => Completion::Fsync {
                tag: op.tag,
                result,
            },
        })
    }

    /// Puts `op` in a free slot and queues it. The caller checked there is
    /// one.
    fn start(&mut self, op: Op) {
        let index = self.free.pop().expect("a free slot");
        self.slots[index as usize] = Some(op);
        self.push(index);
    }

    /// Queues an entry for what is left of the operation in slot `index`.
    fn push(&mut self, index: u32) {
        let op = self.slots[index as usize].as_ref().expect("in flight");
        let len = op.len - op.done;
        #[cfg(test)]
        let len = match self.fault {
            Some(Fault::Short) if !matches!(op.kind, Kind::Fsync) => {
                self.fault = None;
                len.min(SECTOR_SIZE as u32)
            }
            _ => len,
        };
        // In bounds: `address + len` is inside the operation's buffer.
        let address = op.address.wrapping_add(op.done as usize);
        let position = op.position + u64::from(op.done);
        let entry = match op.kind {
            Kind::Read(_) => opcode::ReadFixed::new(FILE, address, len, POOL)
                .offset(position)
                .build(),
            Kind::Write(_) => opcode::WriteFixed::new(FILE, address, len, POOL)
                .offset(position)
                .build(),
            Kind::Fsync => opcode::Fsync::new(FILE).build(),
        };
        let entry: squeue::Entry = entry.user_data(u64::from(index));
        // SAFETY: a read or write entry names `len` bytes inside the buffer
        // its slot holds (checked in `range` and `buffer`), and that buffer
        // lies in the registered pool. The slot keeps the buffer's handle
        // until the completion is returned, so the memory stays allocated;
        // nothing else writes to it meanwhile (a read's handle is the only
        // one, and a written buffer is read-only while shared or was given
        // up by its only holder). FILE is registered for the ring's life.
        let pushed = unsafe { self.ring.submission().push(&entry) };
        pushed.expect("the submission queue holds an entry for every slot");
    }

    /// Checks the preconditions of a read or write of `len` bytes at
    /// `offset` in `zone`, and returns the position in the file.
    fn range(&self, zone: Zone, offset: u64, len: u32, write: bool) -> u64 {
        let sector = SECTOR_SIZE as u64;
        assert!(offset.is_multiple_of(sector) && u64::from(len).is_multiple_of(sector));
        assert!(len > 0 && len as usize <= MESSAGE_SIZE);
        let end = offset + u64::from(len);
        assert!(end <= self.size(zone), "range inside the zone");
        let (position, end) = (
            self.starts[zone as usize] + offset,
            self.starts[zone as usize] + end,
        );
        for op in self.slots.iter().flatten() {
            let other_write = matches!(op.kind, Kind::Write(_));
            let other_end = op.position + u64::from(op.len);
            assert!(
                matches!(op.kind, Kind::Fsync)
                    || other_end <= position
                    || end <= op.position
                    || !(write || other_write),
                "no write overlaps another operation in flight"
            );
        }
        position
    }

    /// The first byte of `buffer`, which must come from the registered pool.
    fn buffer(&self, buffer: &Message) -> *mut u8 {
        let (_, start, len) = self.pool;
        let address = buffer.bytes().as_ptr().cast_mut();
        assert!(
            (start.addr()..start.addr() + len).contains(&address.addr()),
            "the buffer comes from the registered pool"
        );
        address
    }
}

impl Storage for Uring {
    fn size(&self, zone: Zone) -> u64 {
        self.sizes[zone as usize]
    }

    fn read(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        assert_eq!(buffer.references(), 1, "a read buffer is not shared");
        if self.free.is_empty() {
            return Err(Full(buffer));
        }
        let position = self.range(zone, offset, len, false);
        let address = self.buffer(&buffer);
        self.start(Op {
            tag,
            kind: Kind::Read(buffer),
            position,
            len,
            done: 0,
            address,
        });
        Ok(())
    }

    fn write(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        if self.free.is_empty() {
            return Err(Full(buffer));
        }
        let position = self.range(zone, offset, len, true);
        let address = self.buffer(&buffer);
        self.start(Op {
            tag,
            kind: Kind::Write(buffer),
            position,
            len,
            done: 0,
            address,
        });
        Ok(())
    }

    fn fsync(&mut self, tag: Tag) -> Result<(), Full<()>> {
        if self.free.is_empty() {
            return Err(Full(()));
        }
        self.start(Op {
            tag,
            kind: Kind::Fsync,
            position: 0,
            len: 0,
            done: 0,
            address: std::ptr::null_mut(),
        });
        Ok(())
    }
}

impl Drop for Uring {
    /// Waits for every operation in flight: until it completes, the kernel
    /// may still read or write its buffer, which must not go back to the
    /// pool before then.
    fn drop(&mut self) {
        while self.free.len() < self.slots.len() {
            if self.poll().is_none() {
                // Interrupted waits are retried by the loop.
                let _ = self.ring.submit_and_wait(1);
            }
        }
    }
}

/// Fails unless the kernel `release` (as in `uname -r`) is at least
/// [`MIN_KERNEL`].
fn check_kernel(release: &str) -> io::Result<()> {
    let mut parts = release.split(['.', '-']).map(str::parse::<u32>);
    let version = match (parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor))) => Some((major, minor)),
        _ => None,
    };
    if version.is_some_and(|version| version >= MIN_KERNEL) {
        return Ok(());
    }
    let (major, minor) = MIN_KERNEL;
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "kernel {release} is older than {major}.{minor}, the minimum for the io_uring backend"
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn kernel_check() {
        for release in [
            "5.6.0",
            "5.15.0-91-generic",
            "6.8.0-106-generic",
            "7.0.0-1017-azure",
        ] {
            check_kernel(release).unwrap();
        }
        for release in ["5.5.19", "4.19.0-25-amd64", "5", "", "linux"] {
            let error = check_kernel(release).unwrap_err().to_string();
            assert!(error.contains("older than 5.6"), "{error}");
        }
    }

    /// The VM check asserts the same minimum (`just vm-check`).
    #[test]
    fn vm_check_uses_the_same_minimum() {
        let script = include_str!("../../../tools/vm/check.sh");
        let (major, minor) = MIN_KERNEL;
        let line = format!("\nmin_kernel={major}.{minor}\n");
        assert!(script.contains(&line), "tools/vm/check.sh has{line}");
    }

    /// A data file in a fresh directory under the temporary directory,
    /// removed when dropped. `TMPDIR` must be on a file system with direct
    /// I/O.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir =
                std::env::temp_dir().join(format!("granite-uring-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn open(&self, pool: &MessagePool) -> Uring {
            let sector = SECTOR_SIZE as u64;
            Uring::open(
                &self.0.join("data"),
                [sector, sector, sector, 8 * sector],
                2,
                pool,
            )
            .unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn wait(storage: &mut Uring) -> Completion {
        loop {
            if let Some(completion) = storage.poll() {
                return completion;
            }
        }
    }

    /// Byte `i` of a pattern that differs in every sector.
    fn pattern(i: usize) -> u8 {
        (i / SECTOR_SIZE * 7 + i % 251) as u8
    }

    const LEN: u32 = 4 * SECTOR_SIZE as u32;

    fn write(storage: &mut Uring, buffer: Message) -> (Message, Result<(), StorageError>) {
        storage.write(1, Zone::Grid, 0, LEN, buffer).unwrap();
        match wait(storage) {
            Completion::Write { buffer, result, .. } => (buffer, result),
            other => panic!("{other:?}"),
        }
    }

    fn read_back(storage: &mut Uring, mut buffer: Message) -> Message {
        buffer.bytes_mut().unwrap().fill(0);
        storage.read(2, Zone::Grid, 0, LEN, buffer).unwrap();
        let Completion::Read {
            buffer,
            result: Ok(()),
            ..
        } = wait(storage)
        else {
            panic!("read back failed");
        };
        buffer
    }

    /// The kernel writes one sector of four; the backend writes the rest.
    #[test]
    fn a_short_write_is_completed_by_resubmission() {
        let scratch = Scratch::new("short");
        let pool = MessagePool::new(1);
        let mut storage = scratch.open(&pool);
        let mut buffer = pool.acquire().unwrap();
        for (i, byte) in buffer.bytes_mut().unwrap().iter_mut().enumerate() {
            *byte = pattern(i);
        }
        storage.fault = Some(Fault::Short);
        let (buffer, result) = write(&mut storage, buffer);
        assert_eq!(result, Ok(()));
        assert!(storage.fault.is_none(), "the fault was used");
        let buffer = read_back(&mut storage, buffer);
        let bytes = &buffer.bytes()[..LEN as usize];
        assert!(
            bytes
                .iter()
                .enumerate()
                .all(|(i, &byte)| byte == pattern(i))
        );
    }

    /// `EINTR` and `EAGAIN` resubmit the operation, which then succeeds.
    #[test]
    fn interrupted_and_busy_operations_are_retried() {
        let scratch = Scratch::new("retry");
        let pool = MessagePool::new(1);
        let mut storage = scratch.open(&pool);
        let mut buffer = pool.acquire().unwrap();
        for (round, errno) in [libc::EINTR, libc::EAGAIN].into_iter().enumerate() {
            buffer.bytes_mut().unwrap().fill(round as u8 + 1);
            storage.fault = Some(Fault::Errno(errno));
            let (back, result) = write(&mut storage, buffer);
            assert_eq!(result, Ok(()), "errno {errno}");
            assert!(storage.fault.is_none(), "the fault was used");
            buffer = read_back(&mut storage, back);
            assert!(
                buffer.bytes()[..LEN as usize]
                    .iter()
                    .all(|&b| b == round as u8 + 1)
            );
        }
    }

    /// `EIO` from a read, a write or an fsync reaches the caller as a typed
    /// error, with the buffer.
    #[test]
    fn an_io_error_reaches_the_caller() {
        let scratch = Scratch::new("eio");
        let pool = MessagePool::new(1);
        let mut storage = scratch.open(&pool);
        let mut buffer = pool.acquire().unwrap();
        storage.fault = Some(Fault::Errno(libc::EIO));
        let (back, result) = write(&mut storage, buffer);
        assert_eq!(result, Err(StorageError));
        buffer = back;

        storage.fault = Some(Fault::Errno(libc::EIO));
        storage.read(3, Zone::Grid, 0, LEN, buffer).unwrap();
        let Completion::Read {
            tag: 3,
            result: Err(StorageError),
            ..
        } = wait(&mut storage)
        else {
            panic!("the read's error is lost");
        };

        storage.fault = Some(Fault::Errno(libc::EIO));
        storage.fsync(4).unwrap();
        let Completion::Fsync {
            tag: 4,
            result: Err(StorageError),
        } = wait(&mut storage)
        else {
            panic!("the fsync's error is lost");
        };
        assert_eq!(pool.counters().in_use, 0, "the buffer came back");
    }
}
