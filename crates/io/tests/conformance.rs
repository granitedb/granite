//! The in-memory backend passes the conformance suite, and its own checks of
//! the trait preconditions hold.

use granite_base::constants::FORMAT;
use granite_io::conformance::seal;
use granite_io::memory::{Hub, Memory};
use granite_io::message_pool::{MESSAGE_SIZE, Message, MessagePool, SECTOR_SIZE};
use granite_io::{Completion, Connection, Io, Network, NetworkError, Peer, Storage, Zone};

const ZONES: [u64; 4] = [
    4 * SECTOR_SIZE as u64,
    4 * SECTOR_SIZE as u64,
    8 * SECTOR_SIZE as u64,
    2 * MESSAGE_SIZE as u64,
];

fn pair(seed: u64) -> [(Memory, Peer); 2] {
    let hub = Hub::new();
    [Peer(1), Peer(2)].map(|peer| (Memory::new(&hub, peer, ZONES, 4, seed ^ peer.0), peer))
}

#[test]
fn memory_passes_the_conformance_suite() {
    // Several seeds, so the suite holds for several completion orders.
    for seed in 0..32 {
        granite_io::conformance::run(pair(seed));
    }
}

/// The network checks alone, as a backend of `Network` without storage runs
/// them.
#[test]
fn memory_passes_the_network_checks_alone() {
    let pool = MessagePool::new(8);
    for seed in 0..32 {
        let mut nodes = pair(seed);
        granite_io::conformance::run_network(&mut nodes, Memory::poll, &pool);
    }
    assert_eq!(pool.counters().in_use, 0);
}

#[test]
#[should_panic(expected = "a read buffer is not shared")]
fn read_into_a_shared_buffer_panics() {
    let [(mut io, _), _] = pair(0);
    let pool = MessagePool::new(1);
    let message = pool.acquire().unwrap();
    let _keep = message.clone();
    let _ = io.read(1, Zone::Grid, 0, SECTOR_SIZE as u32, message);
}

#[test]
#[should_panic(expected = "no write overlaps")]
fn overlapping_write_in_flight_panics() {
    let [(mut io, _), _] = pair(0);
    let pool = MessagePool::new(2);
    let sector = SECTOR_SIZE as u32;
    io.read(1, Zone::Grid, 0, 2 * sector, pool.acquire().unwrap())
        .unwrap();
    let _ = io.write(
        2,
        Zone::Grid,
        u64::from(sector),
        sector,
        pool.acquire().unwrap(),
    );
}

#[test]
#[should_panic(expected = "range inside the zone")]
fn range_past_the_zone_panics() {
    let [(mut io, _), _] = pair(0);
    let pool = MessagePool::new(1);
    let _ = io.read(
        1,
        Zone::Superblock,
        ZONES[0],
        SECTOR_SIZE as u32,
        pool.acquire().unwrap(),
    );
}

/// Polls `io` until it has nothing ready.
fn drain(io: &mut Memory) -> Vec<Completion> {
    std::iter::from_fn(|| io.poll()).collect()
}

/// Node 2 connects to node 1; returns node 1's end, then node 2's.
fn connect(a: &mut Memory, b: &mut Memory, to_a: Peer) -> (Connection, Connection) {
    a.accept(1).unwrap();
    b.connect(2, to_a).unwrap();
    let [
        Completion::Accepted {
            result: Ok(near), ..
        },
    ] = drain(a)[..]
    else {
        panic!()
    };
    let [
        Completion::Connected {
            result: Ok(far), ..
        },
    ] = drain(b)[..]
    else {
        panic!()
    };
    (near, far)
}

/// A header-only message, the shortest a connection carries.
const HEADER: u32 = FORMAT.header_size as u32;

fn framed(pool: &MessagePool) -> Message {
    let mut message = pool.acquire().unwrap();
    seal(message.bytes_mut().unwrap(), HEADER, 0);
    message
}

/// The memory backend holds sends to the rule io_uring needs for framing,
/// so code tested on it cannot send bytes a real connection would misframe.
#[test]
#[should_panic(expected = "a send is one whole message")]
fn a_send_of_raw_bytes_is_refused() {
    let [(mut a, to_a), (mut b, _)] = pair(0);
    let pool = MessagePool::new(1);
    let (near, _) = connect(&mut a, &mut b, to_a);
    let _ = a.send(4, near, HEADER, pool.acquire().unwrap());
}

#[test]
fn a_crashed_node_resets_its_connections_and_refuses_new_ones() {
    let [(mut a, to_a), (mut b, to_b)] = pair(0);
    let pool = MessagePool::new(2);
    let (near, _) = connect(&mut a, &mut b, to_a);
    a.recv(3, near, pool.acquire().unwrap()).unwrap();
    drop(b);
    a.send(4, near, HEADER, framed(&pool)).unwrap();
    a.connect(5, to_b).unwrap();
    let mut results: Vec<_> = drain(&mut a)
        .into_iter()
        .map(|completion| match completion {
            Completion::Received { tag, result, .. } => (tag, result.map(|_| ())),
            Completion::Sent { tag, result, .. } => (tag, result),
            Completion::Connected { tag, result, .. } => (tag, result.map(|_| ())),
            other => panic!("{other:?}"),
        })
        .collect();
    results.sort_unstable_by_key(|r| r.0);
    let reset = Err(NetworkError::Reset);
    assert_eq!(
        results,
        [(3, reset), (4, reset), (5, Err(NetworkError::Refused))]
    );
    assert_eq!(pool.counters().in_use, 0);
}

/// A message still waiting for the peer's buffer comes back to its sender
/// when the sender closes the connection.
#[test]
fn close_returns_the_sends_still_waiting() {
    let [(mut a, to_a), (mut b, _)] = pair(0);
    let pool = MessagePool::new(1);
    let (_, far) = connect(&mut a, &mut b, to_a);
    b.send(3, far, HEADER, framed(&pool)).unwrap();
    assert!(drain(&mut b).is_empty(), "nobody posted a buffer");
    b.close(far);
    let completions = drain(&mut b);
    let [
        Completion::Sent {
            tag: 3,
            result: Err(NetworkError::Closed),
            ..
        },
    ] = completions[..]
    else {
        panic!("{completions:?}");
    };
    drop(completions);
    assert_eq!(pool.counters().in_use, 0);
}

/// Two writes in flight under one tag. Once either completes, its range is
/// free again while the other's is not.
#[test]
fn equal_tags_keep_the_right_ranges_in_flight() {
    for seed in 0..8 {
        let [(mut io, _), _] = pair(seed);
        let pool = MessagePool::new(3);
        let sector = u64::from(SECTOR_SIZE as u32);
        let buffers = [pool.acquire().unwrap(), pool.acquire().unwrap()];
        let addresses = buffers.each_ref().map(|b| b.bytes().as_ptr());
        for (offset, buffer) in [0, sector].into_iter().zip(buffers) {
            io.write(7, Zone::Superblock, offset, sector as u32, buffer)
                .unwrap();
        }
        let Some(Completion::Write { buffer, .. }) = io.poll() else {
            panic!()
        };
        let first = addresses
            .iter()
            .position(|&a| a == buffer.bytes().as_ptr())
            .unwrap();
        // The completed write's range takes a new write.
        io.write(
            8,
            Zone::Superblock,
            first as u64 * sector,
            sector as u32,
            buffer,
        )
        .unwrap();
        assert_eq!(drain(&mut io).len(), 2);
    }
}

/// A node holds at most `capacity` connections; one more is refused.
#[test]
fn connections_are_bounded() {
    let [(mut a, to_a), (mut b, _)] = pair(0);
    for _ in 0..4 {
        connect(&mut a, &mut b, to_a);
    }
    a.accept(1).unwrap();
    b.connect(2, to_a).unwrap();
    let completions = drain(&mut b);
    let [
        Completion::Connected {
            result: Err(NetworkError::Refused),
            ..
        },
    ] = completions[..]
    else {
        panic!("{completions:?}");
    };
    assert!(drain(&mut a).is_empty(), "the accept still waits");
}
