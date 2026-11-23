//! The io_uring backend of [`Storage`] and [`Network`], the production one
//! (Linux only).
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
//!
//! The data file is locked (`flock`, exclusive) for as long as a ring holds
//! it. A process killed with writes in flight keeps the lock until the kernel
//! has finished them, so a restarted backend cannot start while those writes
//! may still land on top of its own.
//!
//! # Network
//!
//! Connections are TCP, with `TCP_NODELAY`; [`Uring::listen`] opens the
//! listener that [`Network::accept`] takes connections from, and
//! [`Uring::peer`] gives the address [`Network::connect`] resolves a peer to.
//! Connect, accept, send and receive are ring operations on the sockets. A
//! connect that takes longer than [`CONNECT_TIMEOUT`] fails with
//! [`NetworkError::TimedOut`].
//!
//! A message is framed by its header: a receive reads the header into the
//! posted buffer, checks it, then reads the rest of the `size` it gives.
//! Short sends and receives are resubmitted for what is left. A header that
//! fails its check, including a size above `FORMAT.message_size_max`, closes
//! the connection before anything more is read, so it never costs a buffer
//! beyond the posted one.
//!
//! Each connection has a queue of at most [`CONNECTION_QUEUE`] sends and as
//! many receives; the head of each is in the ring, the rest wait for it. A
//! send or receive beyond that is refused with [`Full`], so a peer that stops
//! reading fills its queue and pushes back on the sender. A failed send or
//! receive, or the end of the stream, resets the connection: what waits in
//! its queues fails with [`NetworkError::Reset`] and new sends and receives
//! on it fail at once, until the caller closes it.

#![allow(
    unsafe_code,
    reason = "io_uring submissions and buffer registration are unsafe (docs/06 section 3)"
)]

use crate::message_pool::{MESSAGE_SIZE, Message, MessagePool, SECTOR_SIZE};
use crate::traits::{
    CONNECT_TIMEOUT, Completion, Connection, Full, Network, NetworkError, Peer, Storage,
    StorageError, Tag, Zone,
};
use granite_base::constants::FORMAT;
use granite_base::header::Header;
use io_uring::{IoUring, opcode, squeue, types};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// The oldest kernel the backend runs on, as (major, minor).
pub const MIN_KERNEL: (u32, u32) = (5, 6);

/// The data file's only entry in the ring's registered files.
const FILE: types::Fixed = types::Fixed(0);
/// The pool's only entry in the ring's registered buffers.
const POOL: u16 = 0;

/// Sends queued on one connection at most, and as many receives.
pub const CONNECTION_QUEUE: usize = 8;
/// User data of the timeout linked to each connect, whose own completion is
/// ignored.
const TIMEOUT: u64 = u64::MAX;
const HEADER: u32 = FORMAT.header_size as u32;

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
    /// Connections by index; a [`Connection`] is a generation and an index.
    sockets: Box<[Socket]>,
    /// Indexes of the unused sockets; it never grows past the socket count.
    free_sockets: Vec<u32>,
    /// Network operations failed without reaching the ring, waiting for
    /// `poll`: their slot and error. Each holds a slot, so it never grows
    /// past the slot count.
    ready: VecDeque<(u32, NetworkError)>,
    listener: Option<TcpListener>,
    peers: BTreeMap<Peer, SocketAddr>,
    /// The connect timeout. The kernel reads it when the entry is submitted,
    /// so it is boxed to stay put if the backend moves before that.
    timeout: Box<types::Timespec>,
    #[cfg(test)]
    fault: Option<Fault>,
}

/// One connection, or an unused place for one.
struct Socket {
    /// `None` while unused.
    stream: Option<TcpStream>,
    /// Counts the connections this place has held, so that a stale
    /// [`Connection`] is caught.
    generation: u32,
    /// The connect's destination, which the kernel reads while it is in
    /// flight.
    address: Option<Address>,
    /// The connection failed or the peer closed it.
    reset: bool,
    /// The caller closed it; the place is freed once nothing is in flight.
    closing: bool,
    /// Slots of the sends and receives on the connection, oldest first; the
    /// head of each is in the ring. At most [`CONNECTION_QUEUE`] each.
    sends: VecDeque<u32>,
    recvs: VecDeque<u32>,
}

enum Address {
    V4(libc::sockaddr_in),
    V6(libc::sockaddr_in6),
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
    /// The socket being connected; `None` when the connect failed before it
    /// had one.
    Connect(Option<u32>),
    Accept,
    /// The socket and the buffer.
    Send(u32, Message),
    /// The socket and the buffer. `len` is the header's size until the
    /// header is in, then the message's.
    Recv(u32, Message),
}

/// A fault injected at the ring, for the tests of the retry and error paths.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
enum Fault {
    /// The next read or write entry asks for one sector only, so the kernel
    /// really transfers less than the operation's range.
    Short,
    /// The next completion carries this error number instead of its result.
    /// A send or receive queued meanwhile is not carried out.
    Errno(i32),
    /// Every send and receive entry asks for one byte only, until cleared.
    Trickle,
}

impl Uring {
    /// Opens (creating it if needed) the data file at `path`, with zones of
    /// `zone_sizes` bytes in [`Zone::ALL`] order, room for `capacity`
    /// operations in flight, and `pool` as the buffers. A file shorter than
    /// the zones is extended with zeros.
    ///
    /// Fails with a message naming the cause if the kernel is older than
    /// [`MIN_KERNEL`], io_uring is not allowed, the file cannot be opened
    /// for direct I/O, or the pool cannot be registered. Fails with
    /// [`io::ErrorKind::WouldBlock`] while the file is locked by another
    /// backend, including the ring of a killed process whose I/O has not
    /// finished yet.
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
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "locking {}: the data file is in use by another backend, or by I/O \
                         a killed process left in flight",
                        path.display()
                    ),
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(context(&format!("locking {}", path.display()))(error));
            }
        }
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
        // A connect takes two entries: itself and its timeout.
        let entries = capacity.checked_mul(2).expect("capacity fits");
        let ring = IoUring::new(entries.next_power_of_two()).map_err(context(
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
            sockets: (0..capacity)
                .map(|_| Socket {
                    stream: None,
                    generation: 0,
                    address: None,
                    reset: false,
                    closing: false,
                    sends: VecDeque::with_capacity(CONNECTION_QUEUE),
                    recvs: VecDeque::with_capacity(CONNECTION_QUEUE),
                })
                .collect(),
            free_sockets: (0..capacity).rev().collect(),
            ready: VecDeque::with_capacity(capacity as usize),
            listener: None,
            peers: BTreeMap::new(),
            timeout: Box::new(
                types::Timespec::new()
                    .sec(CONNECT_TIMEOUT / 1_000_000_000)
                    .nsec((CONNECT_TIMEOUT % 1_000_000_000) as u32),
            ),
            #[cfg(test)]
            fault: None,
        })
    }

    /// Listens for connections on `address` and returns the address it got,
    /// which tells the port when `address` asks for any.
    pub fn listen(&mut self, address: SocketAddr) -> io::Result<SocketAddr> {
        assert!(self.listener.is_none(), "one listener per backend");
        let listener = TcpListener::bind(address)?;
        let address = listener.local_addr()?;
        self.listener = Some(listener);
        Ok(address)
    }

    /// Sets the address that a connect to `peer` goes to.
    pub fn peer(&mut self, peer: Peer, address: SocketAddr) {
        self.peers.insert(peer, address);
    }

    /// One completed operation, or `None` when none has completed. Hands the
    /// queued submissions to the kernel first; never blocks.
    pub fn poll(&mut self) -> Option<Completion> {
        if let Some((index, error)) = self.ready.pop_front() {
            return Some(self.finish(index, Err(error)));
        }
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
            if entry.user_data() == TIMEOUT {
                continue;
            }
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
        if !matches!(op.kind, Kind::Read(_) | Kind::Write(_) | Kind::Fsync) {
            return self.complete_network(index, result);
        }
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
            _ => unreachable!("a storage operation"),
        })
    }

    /// Handles the kernel's `result` for the network operation in slot
    /// `index`, and returns its completion once it is done.
    fn complete_network(&mut self, index: u32, result: i32) -> Option<Completion> {
        let retry = result == -libc::EINTR || result == -libc::EAGAIN;
        let op = self.slots[index as usize].as_mut().expect("in flight");
        let (socket, recv) = match op.kind {
            Kind::Connect(socket) => {
                let socket = socket.expect("a connect in the ring has a socket");
                let outcome = if result == 0 {
                    Ok(self.id(socket))
                } else {
                    self.release(socket);
                    let timed_out = result == -libc::ECANCELED || result == -libc::ETIMEDOUT;
                    Err(if timed_out {
                        NetworkError::TimedOut
                    } else {
                        NetworkError::Refused
                    })
                };
                return Some(self.finish(index, outcome));
            }
            Kind::Accept => {
                if retry || result == -libc::ECONNABORTED {
                    self.push(index);
                    return None;
                }
                let outcome = match result {
                    ..0 => Err(NetworkError::Refused),
                    fd => {
                        // SAFETY: the kernel returned a new descriptor, which
                        // nothing else owns.
                        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
                        match self.free_sockets.pop() {
                            // Too many connections: dropping it closes it.
                            None => Err(NetworkError::Refused),
                            Some(socket) => {
                                self.open_socket(socket, TcpStream::from(fd));
                                Ok(self.id(socket))
                            }
                        }
                    }
                };
                return Some(self.finish(index, outcome));
            }
            Kind::Send(socket, _) => (socket, false),
            Kind::Recv(socket, _) => (socket, true),
            Kind::Read(_) | Kind::Write(_) | Kind::Fsync => unreachable!("a network operation"),
        };
        let progress = match result {
            _ if retry => Some(false),
            n if n > 0 => {
                let n = n.unsigned_abs();
                assert!(n <= op.len - op.done, "the kernel moved more than asked");
                op.done += n;
                let mut valid = true;
                if let Kind::Recv(_, buffer) = &op.kind
                    && op.done == HEADER
                    && op.len == HEADER
                {
                    // The header is in: it gives the length, or closes the
                    // connection if it is not valid.
                    match Header::decode(&buffer.bytes()[..HEADER as usize]) {
                        Ok(header) => op.len = header.size.get(),
                        Err(_) => valid = false,
                    }
                }
                valid.then_some(op.done == op.len)
            }
            // The end of the stream, or an error.
            _ => None,
        };
        let len = op.len;
        if progress == Some(false) {
            self.push(index);
            return None;
        }
        if progress.is_none() {
            self.break_socket(socket);
        }
        let s = &mut self.sockets[socket as usize];
        let outcome = match progress {
            Some(_) => Ok(u64::from(len)),
            None if s.closing => Err(NetworkError::Closed),
            None => Err(NetworkError::Reset),
        };
        let queue = if recv { &mut s.recvs } else { &mut s.sends };
        assert_eq!(queue.pop_front(), Some(index), "the head is in the ring");
        // A reset or closed socket has nothing queued behind its heads.
        if let Some(&next) = queue.front() {
            self.push(next);
        } else if s.closing && s.sends.is_empty() && s.recvs.is_empty() {
            self.release(socket);
        }
        Some(self.finish(index, outcome))
    }

    /// Frees slot `index` and returns the network operation's completion.
    fn finish(&mut self, index: u32, result: Result<u64, NetworkError>) -> Completion {
        let op = self.slots[index as usize].take().expect("in flight");
        self.free.push(index);
        let tag = op.tag;
        match op.kind {
            Kind::Connect(_) => Completion::Connected {
                tag,
                result: result.map(Connection),
            },
            Kind::Accept => Completion::Accepted {
                tag,
                result: result.map(Connection),
            },
            Kind::Send(_, buffer) => Completion::Sent {
                tag,
                buffer,
                result: result.map(|_| ()),
            },
            Kind::Recv(_, buffer) => Completion::Received {
                tag,
                buffer,
                result: result.map(|len| u32::try_from(len).expect("a message length")),
            },
            Kind::Read(_) | Kind::Write(_) | Kind::Fsync => unreachable!("a network operation"),
        }
    }

    /// Puts `op` in a free slot and queues it. The caller checked there is
    /// one.
    fn start(&mut self, op: Op) {
        let index = self.insert(op);
        self.push(index);
    }

    /// Puts `op` in a free slot, which the caller checked there is, and
    /// returns its index.
    fn insert(&mut self, op: Op) -> u32 {
        let index = self.free.pop().expect("a free slot");
        self.slots[index as usize] = Some(op);
        index
    }

    /// The id of the connection in `socket`.
    fn id(&self, socket: u32) -> u64 {
        u64::from(self.sockets[socket as usize].generation) << 32 | u64::from(socket)
    }

    /// The socket of `connection`, which must be open.
    fn socket(&self, connection: Connection) -> u32 {
        let index = connection.0 as u32;
        let open = self.sockets.get(index as usize).is_some_and(|s| {
            s.stream.is_some() && !s.closing && u64::from(s.generation) == connection.0 >> 32
        });
        assert!(open, "an open connection");
        index
    }

    /// Makes the unused `socket` hold `stream`.
    fn open_socket(&mut self, socket: u32, stream: TcpStream) {
        // Messages go out as soon as they are sent; a failure costs latency only.
        let _ = stream.set_nodelay(true);
        let s = &mut self.sockets[socket as usize];
        assert!(s.stream.is_none() && s.sends.is_empty() && s.recvs.is_empty());
        s.stream = Some(stream);
        s.reset = false;
        s.closing = false;
    }

    /// Closes the connection in `socket`, which has nothing in flight, and
    /// makes the place unused.
    fn release(&mut self, socket: u32) {
        let s = &mut self.sockets[socket as usize];
        assert!(
            s.sends.is_empty() && s.recvs.is_empty(),
            "nothing in flight"
        );
        s.stream = None;
        s.address = None;
        s.generation = s.generation.wrapping_add(1);
        self.free_sockets.push(socket);
    }

    /// Resets the connection in `socket`: shuts it down, so that what it has
    /// in the ring completes, and fails what waits behind.
    fn break_socket(&mut self, socket: u32) {
        let s = &mut self.sockets[socket as usize];
        if s.reset || s.closing {
            return;
        }
        s.reset = true;
        // An error means the socket is already disconnected.
        let _ = s.stream.as_ref().expect("open").shutdown(Shutdown::Both);
        self.fail_waiting(socket, NetworkError::Reset);
    }

    /// Fails every send and receive queued on `socket` that is not in the
    /// ring.
    fn fail_waiting(&mut self, socket: u32, error: NetworkError) {
        let s = &mut self.sockets[socket as usize];
        for queue in [&mut s.sends, &mut s.recvs] {
            for index in queue.drain(queue.len().min(1)..) {
                self.ready.push_back((index, error));
            }
        }
    }

    /// Submits a send of `len` bytes, or a receive, on `connection`: queues
    /// it, or fails it at once if the connection is reset. Refused when the
    /// connection's queue or the slots are full.
    fn transfer(
        &mut self,
        tag: Tag,
        connection: Connection,
        len: u32,
        buffer: Message,
        recv: bool,
    ) -> Result<(), Full<Message>> {
        let socket = self.socket(connection);
        let s = &self.sockets[socket as usize];
        let queued = if recv { s.recvs.len() } else { s.sends.len() };
        if self.free.is_empty() || queued == CONNECTION_QUEUE {
            return Err(Full(buffer));
        }
        let address = buffer.bytes().as_ptr().cast_mut();
        let kind = if recv {
            Kind::Recv(socket, buffer)
        } else {
            Kind::Send(socket, buffer)
        };
        let index = self.insert(Op {
            tag,
            kind,
            position: 0,
            len,
            done: 0,
            address,
        });
        let s = &mut self.sockets[socket as usize];
        if s.reset {
            self.ready.push_back((index, NetworkError::Reset));
            return Ok(());
        }
        let queue = if recv { &mut s.recvs } else { &mut s.sends };
        queue.push_back(index);
        if queue.len() == 1 {
            self.push(index);
        }
        Ok(())
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
        #[cfg(test)]
        let len = match self.fault {
            Some(Fault::Trickle) if matches!(op.kind, Kind::Send(..) | Kind::Recv(..)) => {
                len.min(1)
            }
            _ => len,
        };
        let fd = |socket: u32| {
            let stream = self.sockets[socket as usize].stream.as_ref();
            types::Fd(stream.expect("an open socket").as_raw_fd())
        };
        let mut timeout = None;
        let entry = match op.kind {
            Kind::Read(_) => opcode::ReadFixed::new(FILE, address, len, POOL)
                .offset(position)
                .build(),
            Kind::Write(_) => opcode::WriteFixed::new(FILE, address, len, POOL)
                .offset(position)
                .build(),
            Kind::Fsync => opcode::Fsync::new(FILE).build(),
            Kind::Connect(socket) => {
                let socket = socket.expect("a connect in the ring has a socket");
                let (to, to_len) = match &self.sockets[socket as usize].address {
                    Some(Address::V4(a)) => (std::ptr::from_ref(a).cast(), size_of_val(a)),
                    Some(Address::V6(a)) => (std::ptr::from_ref(a).cast(), size_of_val(a)),
                    None => unreachable!("a connect has its address"),
                };
                timeout = Some(
                    opcode::LinkTimeout::new(&raw const *self.timeout)
                        .build()
                        .user_data(TIMEOUT),
                );
                opcode::Connect::new(fd(socket), to, to_len as libc::socklen_t)
                    .build()
                    .flags(squeue::Flags::IO_LINK)
            }
            Kind::Accept => {
                let listener = self.listener.as_ref().expect("listening");
                let listener = types::Fd(listener.as_raw_fd());
                opcode::Accept::new(listener, std::ptr::null_mut(), std::ptr::null_mut())
                    .flags(libc::SOCK_CLOEXEC)
                    .build()
            }
            Kind::Send(socket, _) => opcode::Send::new(fd(socket), address, len)
                .flags(libc::MSG_NOSIGNAL)
                .build(),
            Kind::Recv(socket, _) => opcode::Recv::new(fd(socket), address, len).build(),
        };
        // An injected error on a send or receive replaces the operation, so
        // that nothing moves, as with a real one.
        #[cfg(test)]
        let entry = match self.fault {
            Some(Fault::Errno(_)) if matches!(op.kind, Kind::Send(..) | Kind::Recv(..)) => {
                opcode::Nop::new().build()
            }
            _ => entry,
        };
        let entry: squeue::Entry = entry.user_data(u64::from(index));
        let mut submission = self.ring.submission();
        // SAFETY: a read or write entry names `len` bytes inside the buffer
        // its slot holds (checked in `range` and `buffer`), and that buffer
        // lies in the registered pool. A send or receive names `len` bytes
        // inside its slot's buffer, from any pool. The slot keeps the
        // buffer's handle until the completion is returned, so the memory
        // stays allocated; nothing else writes to it meanwhile (a read's or
        // receive's handle is the only one, and a written or sent buffer is
        // read-only while shared or was given up by its only holder). FILE
        // is registered for the ring's life. A socket's descriptor and a
        // connect's address stay in `sockets` until its operations have
        // completed (`release`), the listener for the backend's life, and
        // the timeout is boxed and read at submission.
        let pushed = unsafe {
            submission
                .push(&entry)
                .and_then(|()| timeout.map_or(Ok(()), |t| submission.push(&t)))
        };
        pushed.expect("the submission queue holds two entries for every slot");
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
                !matches!(op.kind, Kind::Read(_) | Kind::Write(_))
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

impl Network for Uring {
    fn connect(&mut self, tag: Tag, to: Peer) -> Result<(), Full<()>> {
        if self.free.is_empty() {
            return Err(Full(()));
        }
        let socket = self
            .peers
            .get(&to)
            .copied()
            .and_then(|to| self.new_socket(to));
        let op = Op {
            tag,
            kind: Kind::Connect(socket),
            position: 0,
            len: 0,
            done: 0,
            address: std::ptr::null_mut(),
        };
        if socket.is_some() {
            self.start(op);
        } else {
            // Unknown, or no room for another connection.
            let index = self.insert(op);
            self.ready.push_back((index, NetworkError::Refused));
        }
        Ok(())
    }

    fn accept(&mut self, tag: Tag) -> Result<(), Full<()>> {
        assert!(self.listener.is_some(), "listen before accept");
        if self.free.is_empty() {
            return Err(Full(()));
        }
        self.start(Op {
            tag,
            kind: Kind::Accept,
            position: 0,
            len: 0,
            done: 0,
            address: std::ptr::null_mut(),
        });
        Ok(())
    }

    fn send(
        &mut self,
        tag: Tag,
        connection: Connection,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        assert!(
            Header::decode(buffer.bytes()).is_ok_and(|h| h.size.get() == len),
            "a send is one whole message"
        );
        self.transfer(tag, connection, len, buffer, false)
    }

    fn recv(
        &mut self,
        tag: Tag,
        connection: Connection,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        assert_eq!(buffer.references(), 1, "a receive buffer is not shared");
        self.transfer(tag, connection, HEADER, buffer, true)
    }

    fn close(&mut self, connection: Connection) {
        let socket = self.socket(connection);
        let s = &mut self.sockets[socket as usize];
        s.closing = true;
        // An error means the socket is already disconnected.
        let _ = s.stream.as_ref().expect("open").shutdown(Shutdown::Both);
        self.fail_waiting(socket, NetworkError::Closed);
        let s = &self.sockets[socket as usize];
        if s.sends.is_empty() && s.recvs.is_empty() {
            self.release(socket);
        }
    }
}

impl Uring {
    /// A socket to connect to `to`, or `None` when there is no room for
    /// another connection or the system refuses one.
    fn new_socket(&mut self, to: SocketAddr) -> Option<u32> {
        let socket = self.free_sockets.pop()?;
        let family = if to.is_ipv4() {
            libc::AF_INET
        } else {
            libc::AF_INET6
        };
        // SAFETY: socket() takes no pointers.
        let fd = unsafe { libc::socket(family, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            self.free_sockets.push(socket);
            return None;
        }
        // SAFETY: socket() returned a new descriptor, which nothing else owns.
        let stream = TcpStream::from(unsafe { OwnedFd::from_raw_fd(fd) });
        self.open_socket(socket, stream);
        self.sockets[socket as usize].address = Some(match to {
            SocketAddr::V4(to) => Address::V4(libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: to.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(to.ip().octets()),
                },
                sin_zero: [0; 8],
            }),
            SocketAddr::V6(to) => Address::V6(libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: to.port().to_be(),
                sin6_flowinfo: to.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: to.ip().octets(),
                },
                sin6_scope_id: to.scope_id(),
            }),
        });
        Some(socket)
    }
}

impl Drop for Uring {
    /// Waits for every operation in flight: until it completes, the kernel
    /// may still read or write its buffer, which must not go back to the
    /// pool before then. Shutting every socket down first makes the network
    /// operations complete.
    fn drop(&mut self) {
        for stream in self.sockets.iter().filter_map(|s| s.stream.as_ref()) {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(listener) = &self.listener {
            // SAFETY: shutdown() takes no pointers; the descriptor is open.
            // On a listener it wakes a pending accept, which fails.
            unsafe { libc::shutdown(listener.as_raw_fd(), libc::SHUT_RD) };
        }
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
    use std::sync::{Mutex, MutexGuard};

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
    /// I/O. Tests that hold one take turns, as registered pools count
    /// against `ulimit -l`.
    struct Scratch(
        PathBuf,
        #[allow(dead_code, reason = "held for its drop, which lets the next test run")]
        MutexGuard<'static, ()>,
    );

    static SERIAL: Mutex<()> = Mutex::new(());

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir =
                std::env::temp_dir().join(format!("granite-uring-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir, SERIAL.lock().unwrap_or_else(|e| e.into_inner()))
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

    /// Two backends that can connect to each other, peers 0 and 1.
    fn pair(scratch: &Scratch, pools: &[MessagePool; 2]) -> [Uring; 2] {
        let zones = [SECTOR_SIZE as u64; 4];
        let loopback = "127.0.0.1:0".parse().unwrap();
        let mut nodes = [("a", &pools[0]), ("b", &pools[1])]
            .map(|(name, pool)| Uring::open(&scratch.0.join(name), zones, 8, pool).unwrap());
        let addresses = nodes.each_mut().map(|node| node.listen(loopback).unwrap());
        nodes[0].peer(Peer(1), addresses[1]);
        nodes[1].peer(Peer(0), addresses[0]);
        nodes
    }

    fn connected(nodes: &mut [Uring; 2]) -> [Connection; 2] {
        nodes[0].accept(1).unwrap();
        nodes[1].connect(2, Peer(0)).unwrap();
        let mut ends = [None, None];
        while ends.iter().any(Option::is_none) {
            for (end, node) in ends.iter_mut().zip(nodes.iter_mut()) {
                match node.poll() {
                    Some(
                        Completion::Accepted { result: Ok(c), .. }
                        | Completion::Connected { result: Ok(c), .. },
                    ) => *end = Some(c),
                    None => {}
                    Some(other) => panic!("{other:?}"),
                }
            }
        }
        ends.map(Option::unwrap)
    }

    /// Sends a one-sector (4 KiB) message from node 1 to node 0 and checks that it
    /// arrives whole.
    fn send_and_receive(nodes: &mut [Uring; 2], [a, b]: [Connection; 2], pool: &MessagePool) {
        const LEN: u32 = SECTOR_SIZE as u32;
        let mut message = pool.acquire().unwrap();
        for (i, byte) in message.bytes_mut().unwrap().iter_mut().enumerate() {
            *byte = pattern(i);
        }
        crate::conformance::seal(message.bytes_mut().unwrap(), LEN, 7);
        let sent = message.bytes()[..LEN as usize].to_vec();
        nodes[0].recv(3, a, pool.acquire().unwrap()).unwrap();
        nodes[1].send(4, b, LEN, message).unwrap();
        let Completion::Sent { result: Ok(()), .. } = wait(&mut nodes[1]) else {
            panic!("the send failed");
        };
        let Completion::Received {
            buffer,
            result: Ok(LEN),
            ..
        } = wait(&mut nodes[0])
        else {
            panic!("the message did not arrive whole");
        };
        assert_eq!(&buffer.bytes()[..LEN as usize], &sent[..]);
    }

    /// Sends and receives that move one byte at a time are reassembled into
    /// the whole message.
    #[test]
    fn short_sends_and_receives_are_reassembled() {
        let scratch = Scratch::new("trickle");
        let registered = [MessagePool::new(1), MessagePool::new(1)];
        let mut nodes = pair(&scratch, &registered);
        let pool = MessagePool::new(2);
        let connections = connected(&mut nodes);
        for node in &mut nodes {
            node.fault = Some(Fault::Trickle);
        }
        send_and_receive(&mut nodes, connections, &pool);
        assert_eq!(pool.counters().in_use, 0);
    }

    /// `EINTR` and `EAGAIN` on a send or a receive resubmit it.
    #[test]
    fn interrupted_network_operations_are_retried() {
        let scratch = Scratch::new("net-retry");
        let registered = [MessagePool::new(1), MessagePool::new(1)];
        let mut nodes = pair(&scratch, &registered);
        let pool = MessagePool::new(2);
        let connections = connected(&mut nodes);
        for errno in [libc::EINTR, libc::EAGAIN] {
            for node in &mut nodes {
                node.fault = Some(Fault::Errno(errno));
            }
            send_and_receive(&mut nodes, connections, &pool);
            assert!(
                nodes.iter().all(|node| node.fault.is_none()),
                "the faults were used"
            );
        }
    }

    fn connect_result(node: &mut Uring, to: Peer) -> Result<Connection, NetworkError> {
        node.connect(9, to).unwrap();
        let Completion::Connected { tag: 9, result } = wait(node) else {
            panic!("not the connect's completion");
        };
        result
    }

    /// A connect to an unknown peer or a closed port is refused, and one
    /// the peer never answers times out.
    #[test]
    fn failed_connects_are_reported() {
        let scratch = Scratch::new("connect");
        let pool = MessagePool::new(1);
        let mut node = scratch.open(&pool);
        assert_eq!(
            connect_result(&mut node, Peer(5)),
            Err(NetworkError::Refused)
        );

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        node.peer(Peer(1), closed.local_addr().unwrap());
        drop(closed);
        assert_eq!(
            connect_result(&mut node, Peer(1)),
            Err(NetworkError::Refused)
        );

        // A listener whose accept queue is full drops new connection requests.
        let full = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        // SAFETY: listen() takes no pointers; the descriptor is open.
        assert_eq!(unsafe { libc::listen(full.as_raw_fd(), 0) }, 0);
        let _queued = std::net::TcpStream::connect(full.local_addr().unwrap()).unwrap();
        node.peer(Peer(2), full.local_addr().unwrap());
        *node.timeout = types::Timespec::new().nsec(100_000_000);
        assert_eq!(
            connect_result(&mut node, Peer(2)),
            Err(NetworkError::TimedOut)
        );
        assert_eq!(
            node.free_sockets.len(),
            node.sockets.len(),
            "sockets released"
        );
    }

    /// A backend holds `capacity` connections: past that a connect and an
    /// accept are refused, and closing one makes room again.
    #[test]
    fn connections_past_the_capacity_are_refused() {
        let scratch = Scratch::new("sockets");
        let pool = MessagePool::new(1);
        let mut node = scratch.open(&pool); // Room for two.
        let address = node.listen("127.0.0.1:0".parse().unwrap()).unwrap();
        node.peer(Peer(0), address);
        // A connection to itself holds both ends.
        node.accept(1).unwrap();
        node.connect(2, Peer(0)).unwrap();
        let mut ends = [wait(&mut node), wait(&mut node)].map(|completion| match completion {
            Completion::Accepted { result: Ok(c), .. }
            | Completion::Connected { result: Ok(c), .. } => c,
            other => panic!("{other:?}"),
        });
        assert_eq!(
            connect_result(&mut node, Peer(0)),
            Err(NetworkError::Refused)
        );
        node.accept(3).unwrap();
        let _plain = std::net::TcpStream::connect(address).unwrap();
        let Completion::Accepted {
            tag: 3,
            result: Err(NetworkError::Refused),
        } = wait(&mut node)
        else {
            panic!("an accept past the capacity is not refused");
        };
        node.close(ends[0]);
        node.accept(4).unwrap();
        let _plain = std::net::TcpStream::connect(address).unwrap();
        let Completion::Accepted {
            tag: 4,
            result: Ok(end),
        } = wait(&mut node)
        else {
            panic!("a closed connection does not make room");
        };
        ends[0] = end;
        for end in ends {
            node.close(end);
        }
        assert_eq!(node.free_sockets.len(), 2);
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
