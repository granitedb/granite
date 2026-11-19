//! The conformance suite every backend of the [`Io`] traits passes (first
//! version). A backend's tests call [`run`] with two fresh nodes that can
//! connect to each other, or [`run_storage`] for a backend of [`Storage`]
//! alone.

use crate::message_pool::{MESSAGE_SIZE, Message, MessagePool, SECTOR_SIZE};
use crate::traits::{Completion, Connection, Full, Io, NetworkError, Peer, Storage, Zone};

/// Polls before a backend is taken to have lost a completion.
const POLLS_MAX: u32 = 10_000_000;

/// Runs every check on `nodes`: two freshly made nodes, each with the peer
/// id the other connects to it by. Every zone is at least four sectors, the
/// grid at least two messages, and each node holds at least four operations
/// in flight.
pub fn run<I: Io>(mut nodes: [(I, Peer); 2]) {
    let pool = MessagePool::new(8);
    run_storage(&mut nodes[0].0, I::poll, &pool);
    messages_arrive_whole_and_in_order(&mut nodes, &pool);
    close_fails_both_ends(&mut nodes, &pool);
    clocks_and_random(&mut nodes[0].0);
    for (io, _) in &mut nodes {
        assert!(
            io.poll().is_none(),
            "no completion beyond the submitted ones"
        );
    }
    assert_eq!(pool.counters().in_use, 0, "every buffer came back");
}

/// Runs the storage checks on `storage`, a fresh data file whose
/// completions `poll` returns. Its buffers come from `pool`, which has at
/// least four free. Every zone is at least four sectors, the grid at least
/// two messages, and the backend holds at least four operations in flight.
pub fn run_storage<S: Storage>(
    storage: &mut S,
    mut poll: impl FnMut(&mut S) -> Option<Completion>,
    pool: &MessagePool,
) {
    let in_use = pool.counters().in_use;
    storage_round_trip(storage, &mut poll, pool);
    completes_once(storage, &mut poll, pool);
    full_returns_the_submission(storage, &mut poll, pool);
    many_in_flight(storage, &mut poll, pool);
    assert!(
        poll(storage).is_none(),
        "no completion beyond the submitted ones"
    );
    assert_eq!(pool.counters().in_use, in_use, "every buffer came back");
}

/// Polls both nodes until `count` completions arrived, and returns them with
/// the index of the node that returned each.
fn collect<I: Io>(nodes: &mut [(I, Peer); 2], count: usize) -> Vec<(usize, Completion)> {
    let mut completions = Vec::with_capacity(count);
    for _ in 0..POLLS_MAX {
        for (index, (io, _)) in nodes.iter_mut().enumerate() {
            if let Some(completion) = io.poll() {
                completions.push((index, completion));
            }
        }
        if completions.len() >= count {
            assert_eq!(completions.len(), count, "no extra completion");
            return completions;
        }
    }
    panic!("{} of {count} completions arrived", completions.len());
}

/// Polls `storage` until `count` completions arrived.
fn drain<S, P>(storage: &mut S, poll: &mut P, count: usize) -> Vec<Completion>
where
    P: FnMut(&mut S) -> Option<Completion>,
{
    let mut completions = Vec::with_capacity(count);
    for _ in 0..POLLS_MAX {
        if completions.len() == count {
            return completions;
        }
        completions.extend(poll(storage));
    }
    panic!("{} of {count} completions arrived", completions.len());
}

fn filled(pool: &MessagePool, byte: u8) -> Message {
    let mut message = pool.acquire().expect("pool has room");
    message.bytes_mut().expect("only handle").fill(byte);
    message
}

fn storage_round_trip<S, P>(storage: &mut S, poll: &mut P, pool: &MessagePool)
where
    S: Storage,
    P: FnMut(&mut S) -> Option<Completion>,
{
    let sector = SECTOR_SIZE as u32;
    for zone in Zone::ALL {
        let size = storage.size(zone);
        assert!(size >= 4 * u64::from(sector));
        let last = size - u64::from(sector);
        // Two sectors at the start, one at the end; the second sector from
        // the end is never written.
        for (offset, len, byte) in [(0, 2 * sector, 0xa1), (last, sector, 0xb2)] {
            storage
                .write(1, zone, offset, len, filled(pool, byte))
                .expect("room");
            let [
                Completion::Write {
                    tag: 1,
                    buffer,
                    result: Ok(()),
                },
            ] = &drain(storage, poll, 1)[..]
            else {
                panic!("expected the write's completion");
            };
            assert_eq!(buffer.bytes()[0], byte);
        }
        let unwritten = last - u64::from(sector);
        for (offset, len, byte) in [
            (0, 2 * sector, 0xa1),
            (last, sector, 0xb2),
            (unwritten, sector, 0),
        ] {
            storage
                .read(2, zone, offset, len, filled(pool, 0x5a))
                .expect("room");
            let [
                Completion::Read {
                    tag: 2,
                    buffer,
                    result: Ok(()),
                },
            ] = &drain(storage, poll, 1)[..]
            else {
                panic!("expected the read's completion");
            };
            let bytes = buffer.bytes();
            assert!(
                bytes[..len as usize].iter().all(|&b| b == byte),
                "{zone:?} at {offset}"
            );
            assert!(
                bytes[len as usize..].iter().all(|&b| b == 0x5a),
                "bytes past len kept"
            );
        }
    }
    storage.fsync(3).expect("room");
    assert!(matches!(
        drain(storage, poll, 1)[..],
        [Completion::Fsync {
            tag: 3,
            result: Ok(())
        }]
    ));
}

fn completes_once<S, P>(storage: &mut S, poll: &mut P, pool: &MessagePool)
where
    S: Storage,
    P: FnMut(&mut S) -> Option<Completion>,
{
    assert!(
        poll(storage).is_none(),
        "nothing completes before it is submitted"
    );
    let sector = SECTOR_SIZE as u32;
    storage
        .write(10, Zone::Grid, 0, sector, filled(pool, 1))
        .expect("room");
    storage
        .read(11, Zone::Superblock, 0, sector, filled(pool, 2))
        .expect("room");
    storage.fsync(12).expect("room");
    let mut tags: Vec<_> = drain(storage, poll, 3)
        .iter()
        .map(Completion::tag)
        .collect();
    tags.sort_unstable();
    assert_eq!(tags, [10, 11, 12]);
}

fn full_returns_the_submission<S, P>(storage: &mut S, poll: &mut P, pool: &MessagePool)
where
    S: Storage,
    P: FnMut(&mut S) -> Option<Completion>,
{
    let mut accepted = 0;
    while storage.fsync(20).is_ok() {
        accepted += 1;
        assert!(
            accepted < POLLS_MAX,
            "a backend bounds its operations in flight"
        );
    }
    let buffer = filled(pool, 3);
    let address = buffer.bytes().as_ptr();
    let Err(Full(back)) = storage.read(21, Zone::Grid, 0, SECTOR_SIZE as u32, buffer) else {
        panic!("a full backend refuses a read");
    };
    assert_eq!(
        back.bytes().as_ptr(),
        address,
        "the refused buffer comes back"
    );
    // One completion makes room for one submission.
    drain(storage, poll, 1);
    storage.fsync(20).expect("room after a completion");
    drain(storage, poll, accepted as usize);
}

/// Four writes of sizes from one sector to a whole message, in flight at
/// once, then reads of the same ranges, in flight at once.
fn many_in_flight<S, P>(storage: &mut S, poll: &mut P, pool: &MessagePool)
where
    S: Storage,
    P: FnMut(&mut S) -> Option<Completion>,
{
    let sector = SECTOR_SIZE as u32;
    let lens = [
        sector,
        3 * sector,
        MESSAGE_SIZE as u32 / 2,
        MESSAGE_SIZE as u32,
    ];
    let mut ranges = [(0, 0); 4];
    let mut offset = 0;
    for (tag, len) in lens.into_iter().enumerate() {
        ranges[tag] = (offset, len);
        offset += u64::from(len);
    }
    assert!(offset <= storage.size(Zone::Grid));
    for (tag, &(offset, len)) in ranges.iter().enumerate() {
        let buffer = filled(pool, 0x30 + tag as u8);
        storage
            .write(tag as u64, Zone::Grid, offset, len, buffer)
            .expect("room");
    }
    let mut buffers: Vec<_> = drain(storage, poll, 4)
        .into_iter()
        .map(|completion| match completion {
            Completion::Write {
                tag,
                mut buffer,
                result: Ok(()),
            } => {
                buffer.bytes_mut().expect("only handle").fill(0);
                (tag, buffer)
            }
            other => panic!("expected a write's completion: {other:?}"),
        })
        .collect();
    buffers.sort_by_key(|(tag, _)| *tag);
    for (tag, buffer) in buffers {
        let (offset, len) = ranges[tag as usize];
        storage
            .read(tag, Zone::Grid, offset, len, buffer)
            .expect("room");
    }
    for completion in drain(storage, poll, 4) {
        let Completion::Read {
            tag,
            buffer,
            result: Ok(()),
        } = completion
        else {
            panic!("expected a read's completion: {completion:?}");
        };
        let len = ranges[tag as usize].1 as usize;
        let bytes = buffer.bytes();
        assert!(
            bytes[..len].iter().all(|&b| b == 0x30 + tag as u8),
            "range {tag}"
        );
        assert!(bytes[len..].iter().all(|&b| b == 0), "bytes past len kept");
    }
}

/// Node 1 connects to node 0; returns node 0's end, then node 1's.
fn connect<I: Io>(nodes: &mut [(I, Peer); 2]) -> (Connection, Connection) {
    let to_a = nodes[0].1;
    nodes[0].0.accept(50).expect("room");
    nodes[1].0.connect(51, to_a).expect("room");
    let (mut a, mut b) = (None, None);
    for (index, completion) in collect(nodes, 2) {
        match (index, completion) {
            (
                0,
                Completion::Accepted {
                    tag: 50,
                    result: Ok(c),
                },
            ) => a = Some(c),
            (
                1,
                Completion::Connected {
                    tag: 51,
                    result: Ok(c),
                },
            ) => b = Some(c),
            (index, completion) => panic!("node {index} returned {completion:?}"),
        }
    }
    (a.expect("accepted"), b.expect("connected"))
}

fn messages_arrive_whole_and_in_order<I: Io>(nodes: &mut [(I, Peer); 2], pool: &MessagePool) {
    let (a, b) = connect(nodes);
    // b sends before a posts a buffer, then sends one buffer twice, shared.
    let mut message = pool.acquire().expect("room");
    message.bytes_mut().expect("only handle")[..3].copy_from_slice(b"one");
    nodes[1].0.send(30, b, 3, message).expect("room");
    let shared = filled(pool, 7);
    let len = MESSAGE_SIZE as u32;
    nodes[1].0.send(31, b, len, shared.clone()).expect("room");
    nodes[1].0.send(32, b, len, shared).expect("room");
    for tag in [40, 41, 42] {
        nodes[0].0.recv(tag, a, filled(pool, 0)).expect("room");
    }
    let mut sent = Vec::new();
    let mut received = Vec::new();
    for (index, completion) in collect(nodes, 6) {
        match (index, completion) {
            (
                1,
                Completion::Sent {
                    tag,
                    result: Ok(()),
                    ..
                },
            ) => sent.push(tag),
            (
                0,
                Completion::Received {
                    tag,
                    buffer,
                    result: Ok(len),
                },
            ) => {
                received.push((tag, len, buffer.bytes()[..len as usize].to_vec()));
            }
            (index, completion) => panic!("node {index} returned {completion:?}"),
        }
    }
    sent.sort_unstable();
    assert_eq!(sent, [30, 31, 32]);
    // Posted buffers fill in the order they were posted.
    received.sort();
    assert_eq!(received[0], (40, 3, b"one".to_vec()));
    assert_eq!(received[1], (41, len, vec![7; MESSAGE_SIZE]));
    assert_eq!(received[2], (42, len, vec![7; MESSAGE_SIZE]));
    nodes[0].0.close(a);
    nodes[1].0.close(b);
}

fn close_fails_both_ends<I: Io>(nodes: &mut [(I, Peer); 2], pool: &MessagePool) {
    let (a, b) = connect(nodes);
    nodes[0].0.recv(60, a, filled(pool, 0)).expect("room");
    nodes[1].0.recv(61, b, filled(pool, 0)).expect("room");
    nodes[1].0.close(b);
    let mut completions = collect(nodes, 2);
    completions.sort_by_key(|(index, _)| *index);
    let [
        (
            0,
            Completion::Received {
                tag: 60,
                result: Err(NetworkError::Reset),
                ..
            },
        ),
        (
            1,
            Completion::Received {
                tag: 61,
                result: Err(NetworkError::Closed),
                ..
            },
        ),
    ] = &completions[..]
    else {
        panic!("a close resets the other end: {completions:?}");
    };
    nodes[0].0.send(62, a, 1, filled(pool, 0)).expect("room");
    let completions = collect(nodes, 1);
    let [
        (
            0,
            Completion::Sent {
                tag: 62,
                result: Err(NetworkError::Reset),
                ..
            },
        ),
    ] = &completions[..]
    else {
        panic!("a send on a reset connection fails: {completions:?}");
    };
    nodes[0].0.close(a);
}

fn clocks_and_random<I: Io>(io: &mut I) {
    let mut last = io.monotonic();
    for _ in 0..1_000 {
        let now = io.monotonic();
        assert!(now >= last, "the monotonic clock never goes back");
        last = now;
    }
    io.realtime();
    let first = io.next_u64();
    assert!((0..64).any(|_| io.next_u64() != first), "random bits vary");
}
