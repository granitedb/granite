//! The conformance suite every backend of the [`Io`] traits passes (first
//! version). A backend's tests call [`run`] with two fresh nodes that can
//! connect to each other, or [`run_storage`] and [`run_network`] for a
//! backend of [`Storage`] or [`Network`] alone. [`run_stream`] is the long
//! check of a network, which [`run`] leaves out.

use crate::message_pool::{MESSAGE_SIZE, Message, MessagePool, SECTOR_SIZE};
use crate::traits::{Completion, Connection, Full, Io, Network, NetworkError, Peer, Storage, Zone};
use granite_base::checksum::checksum;
use granite_base::constants::FORMAT;
use granite_base::header::{Command, Header, VERSION};
use std::mem::offset_of;

/// Polls before a backend is taken to have lost a completion.
const POLLS_MAX: u32 = 10_000_000;

/// Runs every check on `nodes`: two freshly made nodes, each with the peer
/// id the other connects to it by. Every zone is at least four sectors, the
/// grid at least two messages, and each node holds at least four operations
/// in flight.
pub fn run<I: Io>(mut nodes: [(I, Peer); 2]) {
    let pool = MessagePool::new(8);
    run_storage(&mut nodes[0].0, I::poll, &pool);
    run_network(&mut nodes, I::poll, &pool);
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

/// Runs the network checks on `nodes`: two fresh nodes as for [`run`], whose
/// completions `poll` returns. Their buffers come from `pool`, which has at
/// least six free.
pub fn run_network<N: Network>(
    nodes: &mut [(N, Peer); 2],
    mut poll: impl FnMut(&mut N) -> Option<Completion>,
    pool: &MessagePool,
) {
    let in_use = pool.counters().in_use;
    messages_arrive_whole_and_in_order(nodes, &mut poll, pool);
    close_fails_both_ends(nodes, &mut poll, pool);
    for (node, _) in nodes.iter_mut() {
        assert!(
            poll(node).is_none(),
            "no completion beyond the submitted ones"
        );
    }
    assert_eq!(pool.counters().in_use, in_use, "every buffer came back");
}

/// Writes at the start of `bytes` the header of a `len`-byte message whose
/// body, `bytes[header size..len]`, is in place, with `sequence` in its `op`
/// field. The header is a `Ping`, which any receiver accepts as framing.
pub fn seal(bytes: &mut [u8], len: u32, sequence: u64) {
    let header = FORMAT.header_size as usize;
    assert!(header <= len as usize && len as usize <= bytes.len());
    let mut put = |offset: usize, value: &[u8]| {
        bytes[offset..offset + value.len()].copy_from_slice(value);
    };
    put(0, &[0; FORMAT.header_size as usize]);
    put(offset_of!(Header, command), &[Command::Ping as u8]);
    put(offset_of!(Header, version), &[VERSION]);
    put(offset_of!(Header, op), &sequence.to_le_bytes());
    put(offset_of!(Header, size), &len.to_le_bytes());
    let body = checksum(&bytes[header..len as usize]);
    bytes[offset_of!(Header, checksum_body)..][..16].copy_from_slice(&body.to_le_bytes());
    let sum = checksum(&bytes[offset_of!(Header, checksum_body)..header]);
    bytes[..16].copy_from_slice(&sum.to_le_bytes());
    assert!(
        Header::decode(bytes).is_ok_and(|h| h.size.get() == len && h.op.get() == sequence),
        "the header is valid"
    );
}

/// A buffer from `pool` holding a valid `len`-byte message of `byte`s.
fn message(pool: &MessagePool, len: u32, byte: u8) -> Message {
    let mut message = filled(pool, byte);
    seal(message.bytes_mut().expect("only handle"), len, 0);
    message
}

/// Polls both nodes until `count` completions arrived, and returns them with
/// the index of the node that returned each.
fn collect<N, P>(nodes: &mut [(N, Peer); 2], poll: &mut P, count: usize) -> Vec<(usize, Completion)>
where
    P: FnMut(&mut N) -> Option<Completion>,
{
    let mut completions = Vec::with_capacity(count);
    for _ in 0..POLLS_MAX {
        for (index, (node, _)) in nodes.iter_mut().enumerate() {
            if let Some(completion) = poll(node) {
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
pub fn connect<N, P>(nodes: &mut [(N, Peer); 2], poll: &mut P) -> (Connection, Connection)
where
    N: Network,
    P: FnMut(&mut N) -> Option<Completion>,
{
    let to_a = nodes[0].1;
    nodes[0].0.accept(50).expect("room");
    nodes[1].0.connect(51, to_a).expect("room");
    let (mut a, mut b) = (None, None);
    for (index, completion) in collect(nodes, poll, 2) {
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

fn messages_arrive_whole_and_in_order<N, P>(
    nodes: &mut [(N, Peer); 2],
    poll: &mut P,
    pool: &MessagePool,
) where
    N: Network,
    P: FnMut(&mut N) -> Option<Completion>,
{
    let (a, b) = connect(nodes, poll);
    // b sends a header alone before a posts a buffer, then one whole-size
    // buffer twice, shared.
    let header = FORMAT.header_size as u32;
    let first = message(pool, header, 0);
    let first_bytes = first.bytes()[..header as usize].to_vec();
    nodes[1].0.send(30, b, header, first).expect("room");
    let len = MESSAGE_SIZE as u32;
    let shared = message(pool, len, 7);
    let shared_bytes = shared.bytes().to_vec();
    nodes[1].0.send(31, b, len, shared.clone()).expect("room");
    nodes[1].0.send(32, b, len, shared).expect("room");
    for tag in [40, 41, 42] {
        nodes[0].0.recv(tag, a, filled(pool, 0)).expect("room");
    }
    let mut sent = Vec::new();
    let mut received = Vec::new();
    for (index, completion) in collect(nodes, poll, 6) {
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
    assert_eq!(received[0], (40, header, first_bytes));
    assert_eq!(received[1], (41, len, shared_bytes.clone()));
    assert_eq!(received[2], (42, len, shared_bytes));
    nodes[0].0.close(a);
    nodes[1].0.close(b);
}

fn close_fails_both_ends<N, P>(nodes: &mut [(N, Peer); 2], poll: &mut P, pool: &MessagePool)
where
    N: Network,
    P: FnMut(&mut N) -> Option<Completion>,
{
    let (a, b) = connect(nodes, poll);
    nodes[0].0.recv(60, a, filled(pool, 0)).expect("room");
    nodes[1].0.recv(61, b, filled(pool, 0)).expect("room");
    nodes[1].0.close(b);
    let mut completions = collect(nodes, poll, 2);
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
    let header = FORMAT.header_size as u32;
    nodes[0]
        .0
        .send(62, a, header, message(pool, header, 0))
        .expect("room");
    let completions = collect(nodes, poll, 1);
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

/// Messages in flight each way in [`run_stream`].
const STREAM_DEPTH: usize = 4;

/// Sends `count` messages over `connections`, an open connection from node 1
/// (the second) to node 0 (the first), cycling through sizes of 256 bytes,
/// 4 KiB and [`MESSAGE_SIZE`], with up to four in flight each way. Checks
/// that each arrives whole, in the buffer posted for it and with valid
/// checksums. Its buffers come from `pool`, which has at least eight free.
/// Allocates nothing.
pub fn run_stream<N, P>(
    nodes: &mut [(N, Peer); 2],
    mut poll: P,
    pool: &MessagePool,
    connections: (Connection, Connection),
    count: u64,
) where
    N: Network,
    P: FnMut(&mut N) -> Option<Completion>,
{
    let sizes = [FORMAT.header_size as u32, 4096, MESSAGE_SIZE as u32];
    let (a, b) = connections;
    let in_use = pool.counters().in_use;
    // The body bytes are written once; each send rewrites the header only.
    let mut idle: [Option<Message>; STREAM_DEPTH] = std::array::from_fn(|_| {
        let mut message = pool.acquire().expect("pool has room");
        for (i, byte) in message
            .bytes_mut()
            .expect("only handle")
            .iter_mut()
            .enumerate()
        {
            *byte = (i % 251) as u8;
        }
        Some(message)
    });
    // A receive's tag is the sequence of the message it must get.
    let (mut sent, mut sent_done, mut posted, mut received) = (0u64, 0u64, 0u64, 0u64);
    while posted < count.min(STREAM_DEPTH as u64) {
        let buffer = pool.acquire().expect("pool has room");
        nodes[0].0.recv(posted, a, buffer).expect("room");
        posted += 1;
    }
    let mut idle_polls = 0;
    while sent_done < count || received < count {
        let mut progress = false;
        for slot in &mut idle {
            if sent == count {
                break;
            }
            let Some(mut buffer) = slot.take() else {
                continue;
            };
            let len = sizes[(sent % 3) as usize];
            seal(buffer.bytes_mut().expect("only handle"), len, sent);
            if let Err(Full(back)) = nodes[1].0.send(sent, b, len, buffer) {
                *slot = Some(back);
                break;
            }
            sent += 1;
            progress = true;
        }
        if let Some(completion) = poll(&mut nodes[1].0) {
            let Completion::Sent {
                buffer,
                result: Ok(()),
                ..
            } = completion
            else {
                panic!("node 1 returned {completion:?}");
            };
            let slot = idle.iter_mut().find(|slot| slot.is_none());
            *slot.expect("a send buffer is missing from idle") = Some(buffer);
            sent_done += 1;
            progress = true;
        }
        if let Some(completion) = poll(&mut nodes[0].0) {
            let Completion::Received {
                tag,
                buffer,
                result: Ok(len),
            } = completion
            else {
                panic!("node 0 returned {completion:?}");
            };
            let bytes = &buffer.bytes()[..len as usize];
            let header = Header::decode(bytes).expect("a valid header");
            assert_eq!(header.op.get(), tag, "messages fill buffers in order");
            assert_eq!(len, sizes[(tag % 3) as usize], "message {tag} whole");
            assert_eq!(header.size.get(), len);
            let body = &bytes[FORMAT.header_size as usize..];
            assert_eq!(header.check_body(body), Ok(()), "message {tag} body");
            received += 1;
            progress = true;
            if posted < count {
                nodes[0].0.recv(posted, a, buffer).expect("room");
                posted += 1;
            }
        }
        idle_polls = if progress { 0 } else { idle_polls + 1 };
        assert!(
            idle_polls < POLLS_MAX,
            "stuck after {received} of {count} messages"
        );
    }
    drop(idle);
    assert_eq!(pool.counters().in_use, in_use, "every buffer came back");
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
