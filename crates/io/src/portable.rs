//! The portable backend of [`Storage`] and [`Network`], for development on
//! macOS only: correctness work without io_uring. It is never benchmarked and
//! never deployed; `granite start` prints [`WARNING`] when it is chosen, and
//! Linux builds do not have it.
//!
//! Every operation is carried out by a later [`Portable::poll`], never inside
//! the submitting call. Storage is plain `pread` and `pwrite` on the data
//! file, one operation per poll in submission order. Sockets are
//! non-blocking, and each poll moves what it can on every connection; a
//! connect runs `connect` with [`CONNECT_TIMEOUT`] on a helper thread, as std
//! has no non-blocking connect. Framing, the per-connection queues of
//! [`CONNECTION_QUEUE`], resets and the bounds on operations and connections
//! are those of the io_uring backend (`crate::uring`).
//!
//! # Durability
//!
//! The data file is opened with `F_NOCACHE`, and [`Storage::fsync`] is
//! `fcntl(F_FULLFSYNC)`. Unlike the io_uring backend's `O_DIRECT | O_DSYNC`,
//! a completed write is not durable before the next fsync: it may sit in
//! the drive's volatile cache, or in the page cache where `F_NOCACHE` is
//! only a hint. `F_FULLFSYNC` asks the drive to flush its cache, which some
//! external drives ignore. Document 06 section 1 has the comparison.
//!
//! The data file is locked (`flock`, exclusive) for as long as the backend
//! holds it, so two backends never share it.

#![allow(
    unsafe_code,
    reason = "fcntl with F_NOCACHE and F_FULLFSYNC (docs/06 section 3)"
)]

use crate::message_pool::{MESSAGE_SIZE, Message, SECTOR_SIZE};
use crate::traits::{
    CONNECT_TIMEOUT, Completion, Connection, Full, Network, NetworkError, Peer, Storage,
    StorageError, Tag, Zone,
};
use granite_base::constants::FORMAT;
use granite_base::header::Header;
use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// Printed when a node starts on this backend.
pub const WARNING: &str = "the portable I/O backend is for development only and unsupported in \
     production: it is never benchmarked, and a completed write is durable only after the next \
     fsync (docs/06-testing-and-engineering.md section 1)";

/// Sends queued on one connection at most, and as many receives.
pub const CONNECTION_QUEUE: usize = 8;
const HEADER: u32 = FORMAT.header_size as u32;

/// One storage and network backend: the data file, the sockets, and the
/// operations in flight.
pub struct Portable {
    file: File,
    /// Start of each zone in the file, in [`Zone::ALL`] order.
    starts: [u64; 4],
    sizes: [u64; 4],
    /// Operations in flight, by index.
    slots: Box<[Option<Op>]>,
    /// Indexes of the empty slots; it never grows past the slot count.
    free: Vec<u32>,
    /// Storage operations not yet carried out, oldest first.
    storage: VecDeque<u32>,
    /// Accepts waiting for a connection, oldest first.
    accepts: VecDeque<u32>,
    /// Network operations that are done, with their result, waiting for
    /// `poll`. Each holds a slot, so it never grows past the slot count.
    ready: VecDeque<(u32, Result<u64, NetworkError>)>,
    /// Where the connect threads report.
    connected: (Sender<Connected>, Receiver<Connected>),
    /// Connections by index; a [`Connection`] is a generation and an index.
    sockets: Box<[Socket]>,
    /// Indexes of the unused sockets; it never grows past the socket count.
    free_sockets: Vec<u32>,
    listener: Option<TcpListener>,
    peers: BTreeMap<Peer, SocketAddr>,
}

/// A connect thread's report: the slot and the stream.
type Connected = (u32, io::Result<TcpStream>);

/// One connection, or an unused place for one.
struct Socket {
    /// `None` while unused.
    stream: Option<TcpStream>,
    /// Counts the connections this place has held, so that a stale
    /// [`Connection`] is caught.
    generation: u32,
    /// The connection failed or the peer closed it.
    reset: bool,
    /// Slots of the sends and receives on the connection, oldest first; only
    /// the head of each moves. At most [`CONNECTION_QUEUE`] each.
    sends: VecDeque<u32>,
    recvs: VecDeque<u32>,
}

/// An operation in flight.
struct Op {
    tag: Tag,
    kind: Kind,
    /// Position in the file of a read or write.
    position: u64,
    /// Length of the whole transfer. A receive's is the header's size until
    /// the header is in, then the message's.
    len: u32,
    /// Bytes a send or receive has moved.
    done: u32,
}

enum Kind {
    Read(Message),
    Write(Message),
    Fsync,
    Connect,
    Accept,
    Send(Message),
    Recv(Message),
}

impl Portable {
    /// Opens (creating it if needed) the data file at `path`, with zones of
    /// `zone_sizes` bytes in [`Zone::ALL`] order and room for `capacity`
    /// operations in flight and as many connections. A file shorter than the
    /// zones is extended with zeros. Fails with [`ErrorKind::WouldBlock`]
    /// while another backend holds the file.
    pub fn open(path: &Path, zone_sizes: [u64; 4], capacity: u32) -> io::Result<Portable> {
        assert!(capacity > 0);
        let mut starts = [0; 4];
        let mut end = 0u64;
        for (start, size) in starts.iter_mut().zip(zone_sizes) {
            assert!(size.is_multiple_of(SECTOR_SIZE as u64));
            *start = end;
            end = end.checked_add(size).expect("the file size fits in u64");
        }
        let context = |what: String| {
            move |error: io::Error| io::Error::new(error.kind(), format!("{what}: {error}"))
        };
        let shown = path.display();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(context(format!("opening {shown}")))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    ErrorKind::WouldBlock,
                    format!("locking {shown}: the data file is in use by another backend"),
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(context(format!("locking {shown}"))(error));
            }
        }
        // SAFETY: fcntl(F_NOCACHE) takes an integer, no pointer; the
        // descriptor is open while `file` lives.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1) } == -1 {
            return Err(context(format!("setting F_NOCACHE on {shown}"))(
                io::Error::last_os_error(),
            ));
        }
        if file.metadata()?.len() < end {
            // The size, and for a new file its directory entry, are durable
            // before any write is reported done.
            let directory = path.parent().filter(|p| !p.as_os_str().is_empty());
            file.set_len(end)
                .and_then(|()| full_fsync(&file))
                .and_then(|()| File::open(directory.unwrap_or(Path::new(".")))?.sync_all())
                .map_err(context(format!("sizing {shown} to {end} bytes")))?;
        }
        Ok(Portable {
            file,
            starts,
            sizes: zone_sizes,
            slots: (0..capacity).map(|_| None).collect(),
            free: (0..capacity).rev().collect(),
            storage: VecDeque::with_capacity(capacity as usize),
            accepts: VecDeque::with_capacity(capacity as usize),
            ready: VecDeque::with_capacity(capacity as usize),
            connected: channel(),
            sockets: (0..capacity)
                .map(|_| Socket {
                    stream: None,
                    generation: 0,
                    reset: false,
                    sends: VecDeque::with_capacity(CONNECTION_QUEUE),
                    recvs: VecDeque::with_capacity(CONNECTION_QUEUE),
                })
                .collect(),
            free_sockets: (0..capacity).rev().collect(),
            listener: None,
            peers: BTreeMap::new(),
        })
    }

    /// Listens for connections on `address` and returns the address it got,
    /// which tells the port when `address` asks for any.
    pub fn listen(&mut self, address: SocketAddr) -> io::Result<SocketAddr> {
        assert!(self.listener.is_none(), "one listener per backend");
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        self.listener = Some(listener);
        Ok(address)
    }

    /// Sets the address that a connect to `peer` goes to.
    pub fn peer(&mut self, peer: Peer, address: SocketAddr) {
        self.peers.insert(peer, address);
    }

    /// One completed operation, or `None` when none has completed. Carries
    /// out the oldest storage operation, or else moves the network along;
    /// never waits for the network.
    pub fn poll(&mut self) -> Option<Completion> {
        if let Some(index) = self.storage.pop_front() {
            return Some(self.run(index));
        }
        self.progress();
        let (index, result) = self.ready.pop_front()?;
        Some(self.finish(index, result))
    }

    /// Carries out the storage operation in slot `index`.
    fn run(&mut self, index: u32) -> Completion {
        let op = self.slots[index as usize].take().expect("in flight");
        self.free.push(index);
        let (tag, position, len) = (op.tag, op.position, op.len as usize);
        // pread and pwrite resume after EINTR and short transfers.
        match op.kind {
            Kind::Read(mut buffer) => {
                let bytes = &mut buffer.bytes_mut().expect("the only handle")[..len];
                let result = self.file.read_exact_at(bytes, position);
                Completion::Read {
                    tag,
                    buffer,
                    result: result.map_err(|_| StorageError),
                }
            }
            Kind::Write(buffer) => {
                let result = self.file.write_all_at(&buffer.bytes()[..len], position);
                Completion::Write {
                    tag,
                    buffer,
                    result: result.map_err(|_| StorageError),
                }
            }
            Kind::Fsync => Completion::Fsync {
                tag,
                result: full_fsync(&self.file).map_err(|_| StorageError),
            },
            _ => unreachable!("a storage operation"),
        }
    }

    /// Takes what the connect threads reported, one accepted connection, and
    /// whatever each connection's head send and receive can move now.
    fn progress(&mut self) {
        while let Ok((index, stream)) = self.connected.1.try_recv() {
            let result = match stream {
                Ok(stream) => self.open_socket(stream),
                Err(e) if e.kind() == ErrorKind::TimedOut => Err(NetworkError::TimedOut),
                Err(_) => Err(NetworkError::Refused),
            };
            self.ready.push_back((index, result));
        }
        if let Some(&index) = self.accepts.front() {
            let listener = self.listener.as_ref().expect("listening");
            let result = match listener.accept() {
                Ok((stream, _)) => Some(self.open_socket(stream)),
                Err(e) if is_transient(&e) || e.kind() == ErrorKind::ConnectionAborted => None,
                Err(_) => Some(Err(NetworkError::Refused)),
            };
            if let Some(result) = result {
                self.accepts.pop_front();
                self.ready.push_back((index, result));
            }
        }
        for socket in 0..self.sockets.len() as u32 {
            self.step(socket, false);
            self.step(socket, true);
        }
    }

    /// Moves what the head send, or receive, of `socket` can move now.
    fn step(&mut self, socket: u32, recv: bool) {
        let s = &mut self.sockets[socket as usize];
        let queue = if recv { &s.recvs } else { &s.sends };
        let Some(&index) = queue.front() else {
            return;
        };
        let stream = s
            .stream
            .as_mut()
            .expect("a connection with operations is open");
        let op = self.slots[index as usize].as_mut().expect("in flight");
        let (done, len) = (op.done as usize, op.len as usize);
        let moved = match &mut op.kind {
            Kind::Send(buffer) => stream.write(&buffer.bytes()[done..len]),
            Kind::Recv(buffer) => {
                stream.read(&mut buffer.bytes_mut().expect("the only handle")[done..len])
            }
            _ => unreachable!("a send or receive"),
        };
        let valid = match moved {
            Err(e) if is_transient(&e) => return,
            // The end of the stream, or an error.
            Ok(0) | Err(_) => false,
            Ok(n) => {
                op.done += u32::try_from(n).expect("at most len");
                match &op.kind {
                    // The header is in: it gives the length, or resets the
                    // connection if it is not valid.
                    Kind::Recv(buffer) if op.done == HEADER && op.len == HEADER => {
                        match Header::decode(&buffer.bytes()[..HEADER as usize]) {
                            Ok(header) => {
                                op.len = header.size.get();
                                true
                            }
                            Err(_) => false,
                        }
                    }
                    _ => true,
                }
            }
        };
        if !valid {
            self.break_socket(socket);
        } else if op.done == op.len {
            let len = op.len;
            let s = &mut self.sockets[socket as usize];
            let queue = if recv { &mut s.recvs } else { &mut s.sends };
            queue.pop_front();
            self.ready.push_back((index, Ok(u64::from(len))));
        }
    }

    /// Frees slot `index` and returns the network operation's completion.
    fn finish(&mut self, index: u32, result: Result<u64, NetworkError>) -> Completion {
        let op = self.slots[index as usize].take().expect("in flight");
        self.free.push(index);
        let tag = op.tag;
        match op.kind {
            Kind::Connect => Completion::Connected {
                tag,
                result: result.map(Connection),
            },
            Kind::Accept => Completion::Accepted {
                tag,
                result: result.map(Connection),
            },
            Kind::Send(buffer) => Completion::Sent {
                tag,
                buffer,
                result: result.map(|_| ()),
            },
            Kind::Recv(buffer) => Completion::Received {
                tag,
                buffer,
                result: result.map(|len| u32::try_from(len).expect("a message length")),
            },
            Kind::Read(_) | Kind::Write(_) | Kind::Fsync => unreachable!("a network operation"),
        }
    }

    /// Puts `op` in a free slot, which the caller checked there is, and
    /// returns its index.
    fn insert(&mut self, tag: Tag, kind: Kind, position: u64, len: u32) -> u32 {
        let index = self.free.pop().expect("a free slot");
        self.slots[index as usize] = Some(Op {
            tag,
            kind,
            position,
            len,
            done: 0,
        });
        index
    }

    /// Puts `stream` in an unused socket and returns the connection's id, or
    /// refuses it when there is no room for another connection.
    fn open_socket(&mut self, stream: TcpStream) -> Result<u64, NetworkError> {
        stream
            .set_nonblocking(true)
            .map_err(|_| NetworkError::Refused)?;
        // Messages go out as soon as they are sent; a failure costs latency only.
        let _ = stream.set_nodelay(true);
        // Too many connections: dropping the stream closes it.
        let socket = self.free_sockets.pop().ok_or(NetworkError::Refused)?;
        let s = &mut self.sockets[socket as usize];
        assert!(s.stream.is_none() && s.sends.is_empty() && s.recvs.is_empty());
        s.stream = Some(stream);
        s.reset = false;
        Ok(u64::from(s.generation) << 32 | u64::from(socket))
    }

    /// The socket of `connection`, which must be open.
    fn socket(&self, connection: Connection) -> u32 {
        let index = connection.0 as u32;
        let open = self
            .sockets
            .get(index as usize)
            .is_some_and(|s| s.stream.is_some() && u64::from(s.generation) == connection.0 >> 32);
        assert!(open, "an open connection");
        index
    }

    /// Resets the connection in `socket`: shuts it down and fails what is
    /// queued on it.
    fn break_socket(&mut self, socket: u32) {
        let s = &mut self.sockets[socket as usize];
        if !s.reset {
            s.reset = true;
            self.fail(socket, NetworkError::Reset);
        }
    }

    /// Shuts `socket` down and fails every send and receive queued on it.
    fn fail(&mut self, socket: u32, error: NetworkError) {
        let s = &mut self.sockets[socket as usize];
        // An error means the socket is already disconnected.
        let _ = s.stream.as_ref().expect("open").shutdown(Shutdown::Both);
        for index in s.sends.drain(..).chain(s.recvs.drain(..)) {
            self.ready.push_back((index, Err(error)));
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
        let kind = if recv {
            Kind::Recv(buffer)
        } else {
            Kind::Send(buffer)
        };
        let index = self.insert(tag, kind, 0, len);
        let s = &mut self.sockets[socket as usize];
        if s.reset {
            self.ready.push_back((index, Err(NetworkError::Reset)));
        } else if recv {
            s.recvs.push_back(index);
        } else {
            s.sends.push_back(index);
        }
        Ok(())
    }

    /// Checks the preconditions of a read or write of `len` bytes at
    /// `offset` in `zone`, and returns the position in the file.
    fn range(&self, zone: Zone, offset: u64, len: u32, write: bool) -> u64 {
        let sector = SECTOR_SIZE as u64;
        assert!(offset.is_multiple_of(sector) && u64::from(len).is_multiple_of(sector));
        assert!(len > 0 && len as usize <= MESSAGE_SIZE);
        let end = offset + u64::from(len);
        assert!(end <= self.size(zone), "range inside the zone");
        let start = self.starts[zone as usize];
        let (position, end) = (start + offset, start + end);
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

    /// Queues a read or write of `buffer[..len]` at `offset` in `zone`.
    fn queue_storage(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
        write: bool,
    ) -> Result<(), Full<Message>> {
        if self.free.is_empty() {
            return Err(Full(buffer));
        }
        let position = self.range(zone, offset, len, write);
        let kind = if write {
            Kind::Write(buffer)
        } else {
            Kind::Read(buffer)
        };
        let index = self.insert(tag, kind, position, len);
        self.storage.push_back(index);
        Ok(())
    }
}

impl Storage for Portable {
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
        self.queue_storage(tag, zone, offset, len, buffer, false)
    }

    fn write(
        &mut self,
        tag: Tag,
        zone: Zone,
        offset: u64,
        len: u32,
        buffer: Message,
    ) -> Result<(), Full<Message>> {
        self.queue_storage(tag, zone, offset, len, buffer, true)
    }

    fn fsync(&mut self, tag: Tag) -> Result<(), Full<()>> {
        if self.free.is_empty() {
            return Err(Full(()));
        }
        let index = self.insert(tag, Kind::Fsync, 0, 0);
        self.storage.push_back(index);
        Ok(())
    }
}

impl Network for Portable {
    fn connect(&mut self, tag: Tag, to: Peer) -> Result<(), Full<()>> {
        if self.free.is_empty() {
            return Err(Full(()));
        }
        let index = self.insert(tag, Kind::Connect, 0, 0);
        let report = self.connected.0.clone();
        let started = self.peers.get(&to).copied().is_some_and(|to| {
            std::thread::Builder::new()
                .name("granite-connect".into())
                .spawn(move || {
                    let timeout = Duration::from_nanos(CONNECT_TIMEOUT);
                    // The backend may be gone by now; the stream then closes.
                    let _ = report.send((index, TcpStream::connect_timeout(&to, timeout)));
                })
                .is_ok()
        });
        if !started {
            // Unknown, or the system has no thread to spare.
            self.ready.push_back((index, Err(NetworkError::Refused)));
        }
        Ok(())
    }

    fn accept(&mut self, tag: Tag) -> Result<(), Full<()>> {
        assert!(self.listener.is_some(), "listen before accept");
        if self.free.is_empty() {
            return Err(Full(()));
        }
        let index = self.insert(tag, Kind::Accept, 0, 0);
        self.accepts.push_back(index);
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
        self.fail(socket, NetworkError::Closed);
        let s = &mut self.sockets[socket as usize];
        s.stream = None;
        s.generation = s.generation.wrapping_add(1);
        self.free_sockets.push(socket);
    }
}

/// `EINTR` or `EAGAIN`: nothing moved, try again on a later poll.
fn is_transient(error: &io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted)
}

/// Flushes `file` to the drive and asks the drive to flush its cache. Plain
/// `fsync` on macOS stops at the drive's cache.
fn full_fsync(file: &File) -> io::Result<()> {
    loop {
        // SAFETY: fcntl(F_FULLFSYNC) takes no argument; the descriptor is
        // open while `file` lives.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } != -1 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
