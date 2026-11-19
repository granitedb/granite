//! The component as `async` tasks on a minimal executor. Each operation is a
//! future that resolves when its completion is delivered; `step` delivers one
//! completion, wakes the task waiting for it and runs every woken task until
//! it waits again. No `unsafe`: wakers are `std::task::Wake` flags.

use crate::{ACK, Event, LEN, Links, Node, OPS, POSTED, PREPARE, SECTOR, commit, decode, encode};
use granite_io::memory::Memory;
use granite_io::message_pool::{Message, MessagePool};
use granite_io::{Completion, Connection, Full, Io, Network, Storage, Tag, Zone};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Wake, Waker};

/// State shared by a node's tasks.
struct State {
    io: Memory,
    pool: MessagePool,
    links: Links,
    next_tag: Tag,
    /// Completions delivered and not yet awaited.
    done: BTreeMap<Tag, Completion>,
    /// Tasks waiting for a completion.
    waiting: BTreeMap<Tag, Waker>,
    events: Vec<Event>,
    written: [bool; OPS],
    acks: [u8; OPS],
    committed: usize,
}

type Shared = Rc<RefCell<State>>;

/// A task is runnable when its flag is set.
struct Flag(AtomicBool);

impl Wake for Flag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Relaxed);
    }
}

struct Task {
    future: Option<Pin<Box<dyn Future<Output = ()>>>>,
    flag: Arc<Flag>,
    waker: Waker,
}

pub struct Replica {
    state: Shared,
    tasks: Vec<Task>,
}

/// Submits an operation with a fresh tag and returns its completion's future.
fn submit(state: &Shared, op: impl FnOnce(&mut Memory, Tag) -> Result<(), Full<Message>>) -> Wait {
    let mut s = state.borrow_mut();
    let tag = s.next_tag;
    s.next_tag += 1;
    op(&mut s.io, tag).expect("room");
    Wait {
        state: Rc::clone(state),
        tag,
    }
}

struct Wait {
    state: Shared,
    tag: Tag,
}

impl Future for Wait {
    type Output = Completion;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Completion> {
        let mut s = self.state.borrow_mut();
        match s.done.remove(&self.tag) {
            Some(completion) => Poll::Ready(completion),
            None => {
                s.waiting.insert(self.tag, cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

fn link(state: &Shared, peer: usize) -> Connection {
    state.borrow().links[peer].expect("connected")
}

fn acquire(state: &Shared) -> Message {
    state.borrow().pool.acquire().expect("pool")
}

/// The primary's work for one op.
async fn replicate(state: Shared, op: u64) {
    let mut message = acquire(&state);
    encode(&mut message, PREPARE, op);
    let wal = message.clone();
    let write = submit(&state, |io, tag| {
        io.write(tag, Zone::WalPrepares, op * SECTOR, SECTOR as u32, wal)
    });
    let copy = message.clone();
    let (one, two) = (link(&state, 1), link(&state, 2));
    let sends = [
        submit(&state, |io, tag| io.send(tag, one, LEN, copy)),
        submit(&state, |io, tag| io.send(tag, two, LEN, message)),
    ];
    let Completion::Write { result, .. } = write.await else {
        unreachable!()
    };
    result.expect("the in-memory disk does not fail");
    {
        let s = &mut *state.borrow_mut();
        s.events.push(Event::Written { node: 0, op });
        s.written[op as usize] = true;
        commit(&s.written, &s.acks, &mut s.committed, &mut s.events);
    }
    for send in sends {
        send.await;
    }
}

/// The primary receives acknowledgements from `from` into one buffer, until
/// the backup is gone.
async fn acknowledgements(state: Shared, from: u8) {
    let mut buffer = acquire(&state);
    let connection = link(&state, from.into());
    loop {
        let completion = submit(&state, |io, tag| io.recv(tag, connection, buffer)).await;
        let Completion::Received {
            buffer: received,
            result,
            ..
        } = completion
        else {
            unreachable!()
        };
        if result.is_err() {
            return;
        }
        let (kind, op) = decode(&received);
        assert_eq!(kind, ACK);
        let s = &mut *state.borrow_mut();
        s.events.push(Event::Acked { op, from });
        s.acks[op as usize] += 1;
        commit(&s.written, &s.acks, &mut s.committed, &mut s.events);
        buffer = received;
    }
}

/// A backup writes each prepare it receives and acknowledges it.
async fn backup(state: Shared, node: u8) {
    let mut buffer = acquire(&state);
    let primary = link(&state, 0);
    loop {
        let completion = submit(&state, |io, tag| io.recv(tag, primary, buffer)).await;
        let Completion::Received {
            buffer: received, ..
        } = completion
        else {
            unreachable!()
        };
        let (kind, op) = decode(&received);
        assert_eq!(kind, PREPARE);
        let write = submit(&state, |io, tag| {
            io.write(tag, Zone::WalPrepares, op * SECTOR, SECTOR as u32, received)
        });
        let Completion::Write {
            buffer: mut written,
            result,
            ..
        } = write.await
        else {
            unreachable!()
        };
        result.expect("the in-memory disk does not fail");
        state.borrow_mut().events.push(Event::Written { node, op });
        encode(&mut written, ACK, op);
        let Completion::Sent { buffer: sent, .. } =
            submit(&state, |io, tag| io.send(tag, primary, LEN, written)).await
        else {
            unreachable!()
        };
        buffer = sent;
    }
}

impl Replica {
    fn spawn(&mut self, future: impl Future<Output = ()> + 'static) {
        let flag = Arc::new(Flag(AtomicBool::new(true)));
        let waker = Waker::from(Arc::clone(&flag));
        self.tasks.push(Task {
            future: Some(Box::pin(future)),
            flag,
            waker,
        });
    }

    /// Polls woken tasks, in spawn order, until none is woken.
    fn run(&mut self) {
        let mut woken = true;
        while woken {
            woken = false;
            for task in &mut self.tasks {
                if task.flag.0.swap(false, Ordering::Relaxed) {
                    woken = true;
                    let future = task
                        .future
                        .as_mut()
                        .expect("a finished task is never woken");
                    if future
                        .as_mut()
                        .poll(&mut Context::from_waker(&task.waker))
                        .is_ready()
                    {
                        task.future = None;
                    }
                }
            }
        }
    }
}

impl Node for Replica {
    fn new(io: Memory, pool: MessagePool, index: u8, links: Links) -> Self {
        let state = Rc::new(RefCell::new(State {
            io,
            pool,
            links,
            next_tag: 0,
            done: BTreeMap::new(),
            waiting: BTreeMap::new(),
            events: Vec::new(),
            written: [false; OPS],
            acks: [0; OPS],
            committed: 0,
        }));
        let mut replica = Replica {
            state: Rc::clone(&state),
            tasks: Vec::new(),
        };
        // Receives are posted in the order the callback version posts them.
        if index == 0 {
            for _ in 0..POSTED / 2 {
                for from in [1, 2] {
                    replica.spawn(acknowledgements(Rc::clone(&state), from));
                }
            }
            for op in 0..OPS as u64 {
                replica.spawn(replicate(Rc::clone(&state), op));
            }
        } else {
            for _ in 0..POSTED {
                replica.spawn(backup(Rc::clone(&state), index));
            }
        }
        replica.run();
        replica
    }

    fn ready(&self) -> usize {
        self.state.borrow().io.ready()
    }

    fn step(&mut self, trace: &mut Vec<Event>) {
        let waker = {
            let mut s = self.state.borrow_mut();
            let completion = s.io.poll().expect("a completion is ready");
            let tag = completion.tag();
            s.done.insert(tag, completion);
            s.waiting.remove(&tag)
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        self.run();
        trace.append(&mut self.state.borrow_mut().events);
    }
}
