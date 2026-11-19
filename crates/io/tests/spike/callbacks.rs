//! The component as an explicit state machine: every completion is one call
//! of `step`, which dispatches on its kind.

use crate::{ACK, Event, LEN, Links, Node, OPS, POSTED, PREPARE, SECTOR, commit, decode, encode};
use granite_io::memory::Memory;
use granite_io::message_pool::MessagePool;
use granite_io::{Completion, Connection, Io, Network, Storage, Zone};

pub struct Replica {
    io: Memory,
    index: u8,
    links: Links,
    // Primary only.
    written: [bool; OPS],
    acks: [u8; OPS],
    committed: usize,
}

impl Replica {
    fn link(&self, peer: usize) -> Connection {
        self.links[peer].expect("connected")
    }
}

impl Node for Replica {
    fn new(io: Memory, pool: MessagePool, index: u8, links: Links) -> Self {
        let mut replica = Replica {
            io,
            index,
            links,
            written: [false; OPS],
            acks: [0; OPS],
            committed: 0,
        };
        // A receive's tag is the peer it receives from.
        let peers: &[usize] = if index == 0 { &[1, 2] } else { &[0] };
        for _ in 0..POSTED / peers.len() {
            for &peer in peers {
                let buffer = pool.acquire().expect("pool");
                replica
                    .io
                    .recv(peer as u64, replica.link(peer), buffer)
                    .expect("room");
            }
        }
        if index == 0 {
            for op in 0..OPS as u64 {
                let mut message = pool.acquire().expect("pool");
                encode(&mut message, PREPARE, op);
                let wal = message.clone();
                replica
                    .io
                    .write(op, Zone::WalPrepares, op * SECTOR, SECTOR as u32, wal)
                    .expect("room");
                replica
                    .io
                    .send(op, replica.link(1), LEN, message.clone())
                    .expect("room");
                replica
                    .io
                    .send(op, replica.link(2), LEN, message)
                    .expect("room");
            }
        }
        replica
    }

    fn ready(&self) -> usize {
        self.io.ready()
    }

    fn step(&mut self, trace: &mut Vec<Event>) {
        let completion = self.io.poll().expect("a completion is ready");
        let primary = self.index == 0;
        match completion {
            Completion::Write {
                tag: op,
                mut buffer,
                result,
            } => {
                result.expect("the in-memory disk does not fail");
                trace.push(Event::Written {
                    node: self.index,
                    op,
                });
                if primary {
                    self.written[op as usize] = true;
                    commit(&self.written, &self.acks, &mut self.committed, trace);
                } else {
                    encode(&mut buffer, ACK, op);
                    self.io.send(op, self.link(0), LEN, buffer).expect("room");
                }
            }
            // The other end crashed: the buffer goes back to the pool.
            Completion::Received { result: Err(_), .. } => {}
            Completion::Received {
                tag: from,
                buffer,
                result: Ok(_),
            } => {
                let (kind, op) = decode(&buffer);
                if primary {
                    assert_eq!(kind, ACK);
                    trace.push(Event::Acked {
                        op,
                        from: from as u8,
                    });
                    self.acks[op as usize] += 1;
                    self.io
                        .recv(from, self.link(from as usize), buffer)
                        .expect("room");
                    commit(&self.written, &self.acks, &mut self.committed, trace);
                } else {
                    assert_eq!(kind, PREPARE);
                    self.io
                        .write(op, Zone::WalPrepares, op * SECTOR, SECTOR as u32, buffer)
                        .expect("room");
                }
            }
            // A backup's acknowledgement is out: its buffer receives again.
            Completion::Sent { buffer, .. } if !primary => {
                self.io.recv(0, self.link(0), buffer).expect("room")
            }
            Completion::Sent { .. } => {}
            other => unreachable!("{other:?}"),
        }
    }
}
