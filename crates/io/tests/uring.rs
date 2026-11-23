//! The io_uring backend on a real file and on TCP: the storage and network
//! conformance suites, durability of completed writes across `SIGKILL`,
//! backpressure, resets, framing limits, no allocation after startup, and
//! the echo benchmark.
//!
//! The data files go under the temporary directory (`TMPDIR`), which must be
//! on a file system with direct I/O; `just vm-check` points it at the VM's
//! data disk.

#![cfg(target_os = "linux")]

#[path = "common/counting_alloc.rs"]
mod counting_alloc;

use granite_base::checksum::checksum;
use granite_base::constants::FORMAT;
use granite_base::header::{Error, Header};
use granite_io::conformance;
use granite_io::histogram::Histogram;
use granite_io::memory::SplitMix64;
use granite_io::message_pool::{MESSAGE_SIZE, Message, MessagePool, SECTOR_SIZE};
use granite_io::uring::{CONNECTION_QUEUE, Uring};
use granite_io::{Completion, Connection, Network, NetworkError, Peer, Storage, Zone};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::mem::offset_of;
use std::net::{SocketAddr, TcpStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SECTOR: u64 = SECTOR_SIZE as u64;

/// Registered pools count against `ulimit -l`, so the tests take turns.
static SERIAL: Mutex<()> = Mutex::new(());

/// A directory under the temporary directory, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("granite-io-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn data(&self) -> PathBuf {
        self.0.join("data")
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

/// Opens the backend on `path`. The kernel releases a closed ring
/// asynchronously, once the I/O it had in flight is done: only then does its
/// registered memory go back to `ulimit -l` and its data file's lock go away.
/// A ring opened just after another one closed (an earlier test's, or a
/// killed child's) can be refused for that moment, so such an open is
/// retried for up to five seconds.
fn open(path: &Path, zones: [u64; 4], capacity: u32, pool: &MessagePool) -> Uring {
    let mut refused = None;
    for _ in 0..500 {
        match Uring::open(path, zones, capacity, pool) {
            Ok(storage) => return storage,
            Err(e) if matches!(e.kind(), ErrorKind::OutOfMemory | ErrorKind::WouldBlock) => {
                refused = Some(e);
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    }
    let limits = std::fs::read_to_string("/proc/self/limits").unwrap();
    let memlock = limits
        .lines()
        .find(|line| line.starts_with("Max locked memory"));
    panic!("still refused after 5 s: {refused:?}; {memlock:?}");
}

/// A second backend on a data file another one holds is refused, so a
/// restarted process never shares the file with writes still in flight.
#[test]
fn a_data_file_in_use_is_refused() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new("lock");
    let pool = MessagePool::new(1);
    let _first = open(&scratch.data(), kill_zones(), 1, &pool);
    let Err(error) = Uring::open(&scratch.data(), kill_zones(), 1, &pool) else {
        panic!("a second backend opened the data file");
    };
    assert_eq!(error.kind(), ErrorKind::WouldBlock, "{error}");
}

#[test]
fn uring_passes_the_storage_conformance_suite() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new("conformance");
    let pool = MessagePool::new(4);
    let zones = [4 * SECTOR, 4 * SECTOR, 8 * SECTOR, 2 * MESSAGE_SIZE as u64];
    let mut storage = open(&scratch.data(), zones, 8, &pool);
    granite_io::conformance::run_storage(&mut storage, Uring::poll, &pool);
}

/// Grid sectors the child of the kill test writes, one each, in order.
const KILL_SECTORS: u64 = 64;
const KILL_ITERATIONS: u32 = 1_000;
/// Set in the child's environment: the data file and the iteration.
const KILL_CHILD: &str = "GRANITE_IO_KILL_CHILD";

fn kill_zones() -> [u64; 4] {
    [SECTOR, SECTOR, SECTOR, KILL_SECTORS * SECTOR]
}

/// Each iteration starts this test binary as a child that writes sectors
/// one after the other and reports each completed write. After a number of
/// reports drawn from a seed, the child is killed with `SIGKILL`; the
/// restarted backend then reads every reported sector back.
#[test]
fn completed_writes_survive_sigkill() {
    if let Ok(setting) = std::env::var(KILL_CHILD) {
        kill_child(&setting);
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new("kill");
    let seed = 0x6b69_6c6c;
    println!("seed {seed:#x}, {KILL_ITERATIONS} iterations");
    let mut random = SplitMix64(seed);
    for iteration in 0..KILL_ITERATIONS {
        let wanted = 1 + random.next_u64() % 8;
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "completed_writes_survive_sigkill", "--nocapture"])
            .env(
                KILL_CHILD,
                format!("{iteration} {}", scratch.data().display()),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        // The reader lives until the child is dead, so the child never
        // writes to a closed pipe.
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut reported = 0;
        for line in lines.by_ref() {
            if let Some(sector) = line.unwrap().strip_prefix("wrote ") {
                assert_eq!(sector.parse::<u64>().unwrap(), reported, "in order");
                reported += 1;
                if reported == wanted {
                    break;
                }
            }
        }
        child.kill().unwrap(); // SIGKILL
        let status = child.wait().unwrap();
        drop(lines);
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "iteration {iteration}: {status}"
        );
        assert_eq!(
            reported, wanted,
            "iteration {iteration}: the child stopped early"
        );

        let pool = MessagePool::new(1);
        let mut storage = open(&scratch.data(), kill_zones(), 1, &pool);
        let len = (wanted * SECTOR) as u32;
        storage
            .read(0, Zone::Grid, 0, len, pool.acquire().unwrap())
            .unwrap();
        let Completion::Read {
            buffer,
            result: Ok(()),
            ..
        } = wait(&mut storage)
        else {
            panic!("iteration {iteration}: the read back failed");
        };
        for sector in 0..wanted {
            let at = (sector * SECTOR) as usize;
            assert_eq!(
                stamp(&buffer.bytes()[at..at + 16]),
                (u64::from(iteration), sector),
                "iteration {iteration}: sector {sector} was reported written"
            );
        }
    }
}

/// The child of the kill test: writes `KILL_SECTORS` sectors stamped with
/// its iteration, reports each completed one, then waits to be killed. It
/// exits if its parent goes away first, which closes its standard input.
fn kill_child(setting: &str) -> ! {
    let (iteration, path) = setting.split_once(' ').unwrap();
    let iteration: u64 = iteration.parse().unwrap();
    let pool = MessagePool::new(1);
    let mut storage = open(Path::new(path), kill_zones(), 1, &pool);
    let mut buffer = pool.acquire().unwrap();
    for sector in 0..KILL_SECTORS {
        let bytes = buffer.bytes_mut().unwrap();
        bytes[..8].copy_from_slice(&iteration.to_le_bytes());
        bytes[8..16].copy_from_slice(&sector.to_le_bytes());
        storage
            .write(0, Zone::Grid, sector * SECTOR, SECTOR as u32, buffer)
            .unwrap();
        let Completion::Write {
            buffer: back,
            result: Ok(()),
            ..
        } = wait(&mut storage)
        else {
            panic!("a write failed");
        };
        buffer = back;
        println!("wrote {sector}");
    }
    let _ = std::io::stdin().read(&mut [0]);
    std::process::exit(1);
}

fn stamp(bytes: &[u8]) -> (u64, u64) {
    let word = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    (word(0), word(8))
}

const OPERATIONS: u32 = 1_000_000;
/// Operations in flight, each with its own buffer and its own grid sectors
/// (sector `s` belongs to slot `s % SLOTS`), so none overlaps another.
const SLOTS: u64 = 4;
const GRID_SECTORS: u64 = 128;

/// Starts an operation for `slot` drawn from `random`: mostly reads, some
/// writes and a few fsyncs. Returns the buffer if the operation does not
/// take it.
fn start(storage: &mut Uring, random: u64, slot: u64, buffer: Message) -> Option<Message> {
    let sector = (random >> 8) % (GRID_SECTORS / SLOTS) * SLOTS + slot;
    let (offset, len) = (sector * SECTOR, SECTOR as u32);
    match random % 64 {
        0 => {
            storage.fsync(slot).unwrap();
            return Some(buffer);
        }
        1..=4 => storage
            .write(slot, Zone::Grid, offset, len, buffer)
            .unwrap(),
        _ => storage.read(slot, Zone::Grid, offset, len, buffer).unwrap(),
    }
    None
}

#[test]
fn a_million_mixed_operations_allocate_nothing_after_startup() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new("allocations");
    let pool = MessagePool::new(SLOTS as u32);
    let zones = [SECTOR, SECTOR, SECTOR, GRID_SECTORS * SECTOR];
    let mut storage = open(&scratch.data(), zones, SLOTS as u32, &pool);
    let mut random = SplitMix64(0x61_6c6c_6f63);
    // A slot's buffer is idle, in an operation, or parked during its fsync.
    let mut idle: [Option<Message>; SLOTS as usize] = std::array::from_fn(|_| pool.acquire());
    let mut parked: [Option<Message>; SLOTS as usize] = Default::default();

    let before = counting_alloc::allocations();
    let (mut started, mut completed, mut kinds) = (0, 0, [0u32; 3]);
    while completed < OPERATIONS {
        for slot in 0..SLOTS {
            if started < OPERATIONS
                && let Some(buffer) = idle[slot as usize].take()
            {
                parked[slot as usize] = start(&mut storage, random.next_u64(), slot, buffer);
                started += 1;
            }
        }
        let Some(completion) = storage.poll() else {
            continue;
        };
        completed += 1;
        let slot = completion.tag() as usize;
        let (kind, buffer) = match completion {
            Completion::Read {
                buffer,
                result: Ok(()),
                ..
            } => (0, Some(buffer)),
            Completion::Write {
                buffer,
                result: Ok(()),
                ..
            } => (1, Some(buffer)),
            Completion::Fsync { result: Ok(()), .. } => (2, parked[slot].take()),
            other => panic!("{other:?}"),
        };
        kinds[kind] += 1;
        idle[slot] = buffer;
    }
    let allocations = counting_alloc::allocations() - before;
    println!(
        "{completed} operations (reads, writes, fsyncs: {kinds:?}), {allocations} allocations"
    );
    assert_eq!(allocations, 0);
    assert!(kinds.iter().all(|&count| count > 0), "every kind ran");
}

const HEADER: u32 = FORMAT.header_size as u32;
/// Zones of the data file of a node that only uses the network.
const NETWORK_ZONES: [u64; 4] = [SECTOR; 4];
const LOOPBACK: &str = "127.0.0.1:0";

/// A backend listening on the loopback, with a data file in `scratch` named
/// after `name` and a one-buffer registered pool; the network's buffers come
/// from other pools.
fn node(scratch: &Scratch, name: &str, pool: &MessagePool) -> (Uring, SocketAddr) {
    let mut node = open(&scratch.0.join(name), NETWORK_ZONES, 16, pool);
    let address = node.listen(LOOPBACK.parse().unwrap()).unwrap();
    (node, address)
}

/// Two nodes with peer ids 0 and 1 that can connect to each other.
fn pair(scratch: &Scratch, pools: &[MessagePool; 2]) -> [(Uring, Peer); 2] {
    let (mut a, to_a) = node(scratch, "a", &pools[0]);
    let (mut b, to_b) = node(scratch, "b", &pools[1]);
    a.peer(Peer(1), to_b);
    b.peer(Peer(0), to_a);
    [(a, Peer(0)), (b, Peer(1))]
}

#[test]
fn uring_passes_the_network_conformance_suite() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new("network");
    let registered = [MessagePool::new(1), MessagePool::new(1)];
    let mut nodes = pair(&scratch, &registered);
    let pool = MessagePool::new(8);
    conformance::run_network(&mut nodes, Uring::poll, &pool);
}

/// The network gates of T0.3.3 on one stream: messages of 256 bytes, 4 KiB
/// and 1 MiB arrive whole, in order and with valid checksums, and the backend
/// allocates nothing per message. The full count is 1,000,000 messages, run
/// in the release profile:
///
/// GRANITE_FULL=1 cargo test --release --locked -p granite-io --test uring -- stream --nocapture
#[test]
fn a_stream_of_messages_arrives_whole_without_allocating() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let count = if std::env::var_os("GRANITE_FULL").is_some() {
        1_000_000
    } else {
        3_000
    };
    let scratch = Scratch::new("stream");
    let registered = [MessagePool::new(1), MessagePool::new(1)];
    let mut nodes = pair(&scratch, &registered);
    let pool = MessagePool::new(8);
    let mut poll = Uring::poll;
    let connections = conformance::connect(&mut nodes, &mut poll);
    let start = Instant::now();
    let before = counting_alloc::allocations();
    conformance::run_stream(&mut nodes, poll, &pool, connections, count);
    let allocations = counting_alloc::allocations() - before;
    println!(
        "{count} messages in {:?}, {allocations} allocations",
        start.elapsed()
    );
    assert_eq!(allocations, 0);
}

/// Node `node` accepts a connection from a plain socket.
fn accept_plain(node: &mut Uring, address: SocketAddr) -> (Connection, TcpStream) {
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

/// A buffer from `pool` holding a valid message of `len` bytes.
fn message(pool: &MessagePool, len: u32) -> Message {
    let mut message = pool.acquire().unwrap();
    conformance::seal(message.bytes_mut().unwrap(), len, 0);
    message
}

/// Polls `node` until nothing completes for `quiet`, and returns the
/// completions.
fn settle(node: &mut Uring, quiet: Duration) -> Vec<Completion> {
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

/// A peer that stops reading: the sender's queue fills, a send is refused
/// with `Full`, and nothing is allocated meanwhile.
#[test]
fn a_peer_that_stops_reading_fills_the_queue() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new("backpressure");
    let registered = MessagePool::new(1);
    let (mut node, address) = node(&scratch, "a", &registered);
    let (connection, plain) = accept_plain(&mut node, address);
    let pool = MessagePool::new(CONNECTION_QUEUE as u32 + 1);
    let len = MESSAGE_SIZE as u32;
    // Buffers the backend gave back, ready to be sent again.
    let mut idle: Vec<Message> = (0..=CONNECTION_QUEUE)
        .map(|_| message(&pool, len))
        .collect();
    let (mut in_flight, mut sent) = (0, 0u64);

    let before = counting_alloc::allocations();
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
        // The kernel's buffers are full and the queue too: nothing moves.
        if quiet.elapsed() > Duration::from_millis(500) {
            break node.send(sent, connection, len, idle.pop().unwrap());
        }
    };
    let allocations = counting_alloc::allocations() - before;
    let Err(full) = refused else {
        panic!("a send past a full queue is accepted");
    };
    println!("{sent} messages accepted before the queue filled");
    assert_eq!(in_flight, CONNECTION_QUEUE, "the queue is full");
    assert_eq!(allocations, 0, "allocations while filling");
    drop(full);

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
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new("reset");
    let registered = MessagePool::new(1);
    let (mut node, address) = node(&scratch, "a", &registered);
    let (connection, plain) = accept_plain(&mut node, address);
    let pool = MessagePool::new(CONNECTION_QUEUE as u32 + 1);
    for tag in 0..CONNECTION_QUEUE as u64 {
        node.recv(tag, connection, pool.acquire().unwrap()).unwrap();
    }
    node.send(100, connection, HEADER, message(&pool, HEADER))
        .unwrap();
    let Completion::Sent {
        buffer,
        result: Ok(()),
        ..
    } = wait(&mut node)
    else {
        panic!("the send failed");
    };
    drop(buffer);
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

/// A header whose size is above the limit closes the connection: the posted
/// receive fails, the peer sees the end of the stream, and nothing is
/// allocated or acquired for the message.
#[test]
fn a_size_above_the_limit_closes_the_connection() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new("oversize");
    let registered = MessagePool::new(1);
    let (mut node, address) = node(&scratch, "a", &registered);
    let (connection, mut plain) = accept_plain(&mut node, address);
    let pool = MessagePool::new(1);

    let mut header = [0u8; HEADER as usize];
    conformance::seal(&mut header, HEADER, 0);
    let size = offset_of!(Header, size);
    header[size..size + 4].copy_from_slice(&(MESSAGE_SIZE as u32 + 1).to_le_bytes());
    let sum = checksum(&header[16..]);
    header[..16].copy_from_slice(&sum.to_le_bytes());
    assert_eq!(Header::decode(&header), Err(Error::SizeAboveMax));

    let before = counting_alloc::allocations();
    node.recv(1, connection, pool.acquire().unwrap()).unwrap();
    plain.write_all(&header).unwrap();
    let completion = wait(&mut node);
    let allocations = counting_alloc::allocations() - before;
    let Completion::Received {
        result: Err(NetworkError::Reset),
        ..
    } = completion
    else {
        panic!("{completion:?}");
    };
    drop(completion);
    assert_eq!(allocations, 0);
    let counters = pool.counters();
    assert_eq!((counters.in_use, counters.high_water), (0, 1));
    // The backend shut the connection down.
    plain
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(plain.read(&mut [0; 16]).unwrap(), 0);
    node.close(connection);
}

/// Round trips between two shard threads on one machine, for the report of
/// T0.3.3: one message in flight, echoed back, for each message size.
/// Meaningful only in the release profile on real hardware:
///
/// cargo test --release --locked -p granite-io --test uring -- echo --ignored --nocapture
///
/// `GRANITE_ECHO_CORES=<client>,<server>` picks the cores (0,1 by default).
#[test]
#[ignore = "a measurement, run by hand in a release build"]
fn echo_between_two_shards() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let cores = std::env::var("GRANITE_ECHO_CORES").unwrap_or("0,1".to_owned());
    let (client_core, server_core) = cores.split_once(',').expect("two cores");
    let sizes = [
        (HEADER, 1_000_000),
        (4096, 1_000_000),
        (MESSAGE_SIZE as u32, 10_000),
    ];
    let scratch = std::sync::Arc::new(Scratch::new("echo"));
    let (to_client, from_server) = std::sync::mpsc::channel();

    let dir = scratch.clone();
    let server = granite_io::pin::spawn(server_core.parse().unwrap(), move || {
        let registered = MessagePool::new(1);
        let (mut node, address) = node(&dir, "server", &registered);
        to_client.send(address).unwrap();
        node.accept(0).unwrap();
        let Completion::Accepted {
            result: Ok(connection),
            ..
        } = wait(&mut node)
        else {
            panic!("the accept failed");
        };
        let pool = MessagePool::new(1);
        let mut buffer = pool.acquire().unwrap();
        // Echoes every message until the client closes the connection.
        loop {
            node.recv(0, connection, buffer).unwrap();
            let Completion::Received {
                buffer: message,
                result,
                ..
            } = wait(&mut node)
            else {
                unreachable!()
            };
            let Ok(len) = result else { break };
            node.send(0, connection, len, message).unwrap();
            let Completion::Sent {
                buffer: back,
                result: Ok(()),
                ..
            } = wait(&mut node)
            else {
                panic!("the echo failed");
            };
            buffer = back;
        }
        node.close(connection);
    })
    .unwrap();

    let dir = scratch.clone();
    let client = granite_io::pin::spawn(client_core.parse().unwrap(), move || {
        let registered = MessagePool::new(1);
        let mut node = open(&dir.0.join("client"), NETWORK_ZONES, 16, &registered);
        node.peer(Peer(1), from_server.recv().unwrap());
        node.connect(0, Peer(1)).unwrap();
        let Completion::Connected {
            result: Ok(connection),
            ..
        } = wait(&mut node)
        else {
            panic!("the connect failed");
        };
        let pool = MessagePool::new(2);
        for (len, rounds) in sizes {
            let mut out = message(&pool, len);
            let mut back = pool.acquire().unwrap();
            let mut round_trips = Histogram::new();
            let start = Instant::now();
            for _ in 0..rounds {
                let sent = Instant::now();
                node.recv(0, connection, back).unwrap();
                node.send(1, connection, len, out).unwrap();
                let (mut received, mut returned) = (None, None);
                while received.is_none() || returned.is_none() {
                    match node.poll() {
                        Some(Completion::Received {
                            buffer,
                            result: Ok(n),
                            ..
                        }) if n == len => received = Some(buffer),
                        Some(Completion::Sent {
                            buffer,
                            result: Ok(()),
                            ..
                        }) => returned = Some(buffer),
                        None => {}
                        Some(other) => panic!("{other:?}"),
                    }
                }
                round_trips.record(sent.elapsed().as_nanos() as u64);
                (back, out) = (received.unwrap(), returned.unwrap());
            }
            let elapsed = start.elapsed();
            println!(
                "echo {len} bytes: {rounds} round trips, median {} ns, p99 {} ns, \
                 p99.9 {} ns, {:.0} round trips/s (a message each way)",
                round_trips.quantile(500_000),
                round_trips.quantile(990_000),
                round_trips.quantile(999_000),
                rounds as f64 / elapsed.as_secs_f64()
            );
        }
        node.close(connection);
    })
    .unwrap();
    client.join().unwrap().unwrap();
    server.join().unwrap().unwrap();
}
