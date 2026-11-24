//! The portable backend on a real file and on TCP (macOS only): the storage,
//! network and stream conformance suites, the file lock, the connection
//! limit, failed connects, backpressure, resets and framing limits.

#![cfg(target_os = "macos")]

use granite_base::checksum::checksum;
use granite_base::constants::FORMAT;
use granite_base::header::{Error, Header};
use granite_io::conformance;
use granite_io::message_pool::{MESSAGE_SIZE, Message, MessagePool, SECTOR_SIZE};
use granite_io::portable::{CONNECTION_QUEUE, Portable};
use granite_io::{Completion, Connection, Network, NetworkError, Peer};
use std::io::{ErrorKind, Read, Write};
use std::mem::offset_of;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SECTOR: u64 = SECTOR_SIZE as u64;
const HEADER: u32 = FORMAT.header_size as u32;
const LOOPBACK: &str = "127.0.0.1:0";

/// A directory under the temporary directory, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("granite-portable-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// A backend on the data file `name`, with room for `capacity`
    /// operations and connections.
    fn open(&self, name: &str, capacity: u32) -> Portable {
        let zones = [4 * SECTOR, 4 * SECTOR, 8 * SECTOR, 2 * MESSAGE_SIZE as u64];
        Portable::open(&self.0.join(name), zones, capacity).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wait(node: &mut Portable) -> Completion {
    let start = Instant::now();
    loop {
        if let Some(completion) = node.poll() {
            return completion;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "no completion");
    }
}

/// Polls `node` until nothing completes for `quiet`, and returns the
/// completions.
fn settle(node: &mut Portable, quiet: Duration) -> Vec<Completion> {
    let mut completions = Vec::new();
    let mut last = Instant::now();
    while last.elapsed() < quiet {
        if let Some(completion) = node.poll() {
            completions.push(completion);
            last = Instant::now();
        }
    }
    completions
}

/// Two listening nodes with peer ids 0 and 1 that can connect to each other.
fn pair(scratch: &Scratch) -> [(Portable, Peer); 2] {
    let mut nodes = ["a", "b"].map(|name| scratch.open(name, 16));
    let addresses = nodes
        .each_mut()
        .map(|node| node.listen(LOOPBACK.parse().unwrap()).unwrap());
    nodes[0].peer(Peer(1), addresses[1]);
    nodes[1].peer(Peer(0), addresses[0]);
    let [a, b] = nodes;
    [(a, Peer(0)), (b, Peer(1))]
}

#[test]
fn portable_passes_the_storage_conformance_suite() {
    let scratch = Scratch::new("storage");
    let pool = MessagePool::new(4);
    let mut storage = scratch.open("data", 8);
    conformance::run_storage(&mut storage, Portable::poll, &pool);
    drop(storage);
    // The suite grows the grid by two sectors; the file grew with it.
    let zones = 16 * SECTOR + 2 * MESSAGE_SIZE as u64;
    let metadata = std::fs::metadata(scratch.0.join("data")).unwrap();
    assert_eq!(metadata.len(), zones + 2 * SECTOR);
}

/// A grown grid is allocated, not a hole, so a later write cannot fail for
/// lack of space.
#[test]
fn a_grown_grid_is_allocated() {
    use granite_io::{Storage, Zone};
    use std::os::unix::fs::MetadataExt;
    let scratch = Scratch::new("grow");
    let path = scratch.0.join("data");
    let mut storage = Portable::open(&path, [SECTOR, SECTOR, SECTOR, 0], 1).unwrap();
    let size = 64 << 20;
    storage.grow_grid(1, size).unwrap();
    assert!(matches!(
        wait(&mut storage),
        Completion::Grown {
            tag: 1,
            result: Ok(())
        }
    ));
    assert_eq!(storage.size(Zone::Grid), size);
    let metadata = std::fs::metadata(&path).unwrap();
    assert_eq!(metadata.len(), 3 * SECTOR + size);
    assert!(
        // st_blocks counts 512-byte units.
        metadata.blocks() >= size >> 9,
        "{} blocks",
        metadata.blocks()
    );
}

#[test]
fn portable_passes_the_network_conformance_suite() {
    let scratch = Scratch::new("network");
    let mut nodes = pair(&scratch);
    let pool = MessagePool::new(8);
    conformance::run_network(&mut nodes, Portable::poll, &pool);
}

/// Messages of 256 bytes, 4 KiB and 1 MiB arrive whole, in order and with
/// valid checksums.
#[test]
fn portable_passes_the_stream_check() {
    let scratch = Scratch::new("stream");
    let mut nodes = pair(&scratch);
    let pool = MessagePool::new(8);
    let mut poll = Portable::poll;
    let connections = conformance::connect(&mut nodes, &mut poll);
    conformance::run_stream(&mut nodes, poll, &pool, connections, 3_000);
}

/// A second backend on a data file another one holds is refused.
#[test]
fn a_data_file_in_use_is_refused() {
    let scratch = Scratch::new("lock");
    let _first = scratch.open("data", 1);
    let zones = [4 * SECTOR, 4 * SECTOR, 8 * SECTOR, 2 * MESSAGE_SIZE as u64];
    let Err(error) = Portable::open(&scratch.0.join("data"), zones, 1) else {
        panic!("a second backend opened the data file");
    };
    assert_eq!(error.kind(), ErrorKind::WouldBlock, "{error}");
}

fn connect_result(node: &mut Portable, to: Peer) -> Result<Connection, NetworkError> {
    node.connect(9, to).unwrap();
    let Completion::Connected { tag: 9, result } = wait(node) else {
        panic!("not the connect's completion");
    };
    result
}

/// A connect to an unknown peer or a closed port is refused, and one the
/// peer never answers times out after `CONNECT_TIMEOUT`.
#[test]
fn failed_connects_are_reported() {
    let scratch = Scratch::new("connect");
    let mut node = scratch.open("data", 2);
    assert_eq!(
        connect_result(&mut node, Peer(5)),
        Err(NetworkError::Refused)
    );

    let closed = TcpListener::bind(LOOPBACK).unwrap();
    node.peer(Peer(1), closed.local_addr().unwrap());
    drop(closed);
    assert_eq!(
        connect_result(&mut node, Peer(1)),
        Err(NetworkError::Refused)
    );

    // macOS configures only 127.0.0.1 on the loopback, so nothing answers
    // a connect to 127.0.0.2 and it hangs.
    node.peer(Peer(2), "127.0.0.2:9".parse().unwrap());
    let start = Instant::now();
    assert_eq!(
        connect_result(&mut node, Peer(2)),
        Err(NetworkError::TimedOut)
    );
    let timeout = Duration::from_nanos(granite_io::CONNECT_TIMEOUT);
    assert!(start.elapsed() >= timeout, "{:?}", start.elapsed());
}

/// A backend holds `capacity` connections: past that an accept is refused,
/// and closing one makes room again.
#[test]
fn connections_past_the_capacity_are_refused() {
    let scratch = Scratch::new("sockets");
    let mut node = scratch.open("data", 2);
    let address = node.listen(LOOPBACK.parse().unwrap()).unwrap();
    let mut ends = [0, 1].map(|tag| {
        node.accept(tag).unwrap();
        let plain = TcpStream::connect(address).unwrap();
        let Completion::Accepted { result: Ok(c), .. } = wait(&mut node) else {
            panic!("the accept failed");
        };
        (c, plain)
    });
    node.accept(3).unwrap();
    let _plain = TcpStream::connect(address).unwrap();
    let Completion::Accepted {
        tag: 3,
        result: Err(NetworkError::Refused),
    } = wait(&mut node)
    else {
        panic!("an accept past the capacity is not refused");
    };
    node.close(ends[0].0);
    node.accept(4).unwrap();
    let plain = TcpStream::connect(address).unwrap();
    let Completion::Accepted {
        tag: 4,
        result: Ok(end),
    } = wait(&mut node)
    else {
        panic!("a closed connection does not make room");
    };
    ends[0] = (end, plain);
    for (end, _) in ends {
        node.close(end);
    }
}

/// Node accepts a connection from a plain socket.
fn accept_plain(node: &mut Portable, address: SocketAddr) -> (Connection, TcpStream) {
    node.accept(1).unwrap();
    let plain = TcpStream::connect(address).unwrap();
    let Completion::Accepted {
        result: Ok(connection),
        ..
    } = wait(node)
    else {
        panic!("the accept failed");
    };
    (connection, plain)
}

fn listening(scratch: &Scratch) -> (Portable, SocketAddr) {
    let mut node = scratch.open("data", 16);
    let address = node.listen(LOOPBACK.parse().unwrap()).unwrap();
    (node, address)
}

/// A buffer from `pool` holding a valid message of `len` bytes.
fn message(pool: &MessagePool, len: u32) -> Message {
    let mut message = pool.acquire().unwrap();
    conformance::seal(message.bytes_mut().unwrap(), len, 0);
    message
}

/// A peer that stops reading: the sender's queue fills and a send is
/// refused with `Full`; closing fails what is queued with `Closed`.
#[test]
fn a_peer_that_stops_reading_fills_the_queue() {
    let scratch = Scratch::new("backpressure");
    let (mut node, address) = listening(&scratch);
    let (connection, plain) = accept_plain(&mut node, address);
    // More buffers than the queue holds, so the queue's bound is what stops
    // the sender.
    let pool = MessagePool::new(2 * CONNECTION_QUEUE as u32);
    let len = MESSAGE_SIZE as u32;
    let mut idle: Vec<Message> = (0..2 * CONNECTION_QUEUE)
        .map(|_| message(&pool, len))
        .collect();
    let (mut in_flight, mut sent) = (0, 0u64);
    let mut quiet = Instant::now();
    let refused = loop {
        if let Some(buffer) = idle.pop() {
            match node.send(sent, connection, len, buffer) {
                Ok(()) => {
                    in_flight += 1;
                    sent += 1;
                    quiet = Instant::now();
                }
                Err(full) => idle.push(full.0),
            }
        }
        if let Some(completion) = node.poll() {
            let Completion::Sent {
                buffer,
                result: Ok(()),
                ..
            } = completion
            else {
                panic!("{completion:?}");
            };
            idle.push(buffer);
            in_flight -= 1;
            quiet = Instant::now();
        }
        // The socket's buffers are full and the queue too: nothing moves.
        if quiet.elapsed() > Duration::from_millis(500) {
            assert_eq!(in_flight, CONNECTION_QUEUE, "the queue is full");
            break node.send(sent, connection, len, idle.pop().unwrap());
        }
    };
    assert!(refused.is_err(), "a send past a full queue is accepted");
    drop(refused);
    node.close(connection);
    let completions = settle(&mut node, Duration::from_millis(200));
    assert_eq!(completions.len(), CONNECTION_QUEUE);
    for completion in completions {
        assert!(
            matches!(
                completion,
                Completion::Sent {
                    result: Err(NetworkError::Closed),
                    ..
                }
            ),
            "{completion:?}"
        );
    }
    drop((idle, plain));
    assert_eq!(pool.counters().in_use, 0);
}

/// A connection the peer resets: every operation on it fails with `Reset`,
/// and once it is closed every buffer is back in the pool.
#[test]
fn a_reset_by_the_peer_is_reported_and_returns_the_buffers() {
    let scratch = Scratch::new("reset");
    let (mut node, address) = listening(&scratch);
    let (connection, plain) = accept_plain(&mut node, address);
    let pool = MessagePool::new(CONNECTION_QUEUE as u32 + 1);
    for tag in 0..CONNECTION_QUEUE as u64 {
        node.recv(tag, connection, pool.acquire().unwrap()).unwrap();
    }
    node.send(100, connection, HEADER, message(&pool, HEADER))
        .unwrap();
    let Completion::Sent { result: Ok(()), .. } = wait(&mut node) else {
        panic!("the send failed");
    };
    // The message is in the peer's receive queue, unread, so closing the
    // socket sends a reset rather than an orderly end.
    plain.peek(&mut [0]).unwrap();
    drop(plain);

    let completions = settle(&mut node, Duration::from_millis(200));
    assert_eq!(completions.len(), CONNECTION_QUEUE);
    for completion in &completions {
        assert!(
            matches!(
                completion,
                Completion::Received {
                    result: Err(NetworkError::Reset),
                    ..
                }
            ),
            "{completion:?}"
        );
    }
    drop(completions);
    node.send(101, connection, HEADER, message(&pool, HEADER))
        .unwrap();
    let Completion::Sent {
        tag: 101,
        result: Err(NetworkError::Reset),
        ..
    } = wait(&mut node)
    else {
        panic!("a send on a reset connection does not fail");
    };
    node.close(connection);
    assert!(settle(&mut node, Duration::from_millis(50)).is_empty());
    assert_eq!(pool.counters().in_use, 0, "every buffer came back");
}

/// A header whose size is above the limit resets the connection: the posted
/// receive fails and the peer sees the end of the stream.
#[test]
fn a_size_above_the_limit_closes_the_connection() {
    let scratch = Scratch::new("oversize");
    let (mut node, address) = listening(&scratch);
    let (connection, mut plain) = accept_plain(&mut node, address);
    let pool = MessagePool::new(1);

    let mut header = [0u8; HEADER as usize];
    conformance::seal(&mut header, HEADER, 0);
    let size = offset_of!(Header, size);
    header[size..size + 4].copy_from_slice(&(MESSAGE_SIZE as u32 + 1).to_le_bytes());
    let sum = checksum(&header[16..]);
    header[..16].copy_from_slice(&sum.to_le_bytes());
    assert_eq!(Header::decode(&header), Err(Error::SizeAboveMax));

    node.recv(1, connection, pool.acquire().unwrap()).unwrap();
    plain.write_all(&header).unwrap();
    let completion = wait(&mut node);
    let Completion::Received {
        result: Err(NetworkError::Reset),
        ..
    } = completion
    else {
        panic!("{completion:?}");
    };
    drop(completion);
    assert_eq!(pool.counters().in_use, 0);
    // The backend shut the connection down.
    plain
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(plain.read(&mut [0; 16]).unwrap(), 0);
    node.close(connection);
}
