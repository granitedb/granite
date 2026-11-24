//! The replica (document 02 sections 1 to 4): its state, the dispatch of
//! received messages by command with the statuses that accept each, its
//! timeouts, its connections to the other replicas, and normal operation.
//!
//! A replica is a [`Tasks`] of the shard's event loop and reaches the
//! outside world only through the [`Io`] it is handed.
//!
//! Normal operation (section 3):
//! - The primary takes a request into a bounded queue, or refuses it with
//!   [`Busy`]. While fewer than `FORMAT.pipeline_depth` prepares are
//!   uncommitted, it turns the oldest queued request into the next prepare
//!   (op, timestamp, and the parent checksum that chains it to the previous
//!   one), and hands it to its WAL and to every backup at once.
//! - A backup accepts a prepare from the primary of its view, in status
//!   normal, only if it is the next op and its parent is the checksum of
//!   the backup's last prepare. It writes it to its WAL and sends
//!   prepare_ok once the WAL reports it durable (invariant 6). A prepare
//!   beyond the next op, or one that does not extend the chain, is counted
//!   for repair (T2.2.4) and never written.
//! - The primary commits an op once a replication quorum, its own durable
//!   write included, holds it, in op order; it then executes it and replies
//!   to the client on the connection the request came on.
//! - A backup learns commit_max from each prepare and commit of the primary
//!   and executes, in op order, every committed op it holds.
//! - On its heartbeat the primary sends commit_max, and sends each
//!   uncommitted prepare again to every backup that has not acknowledged it
//!   and has nothing queued still on its way.
//!
//! Both roles keep their prepares in memory, from the oldest not yet both
//! executed and handed to the WAL to the newest; a prepare's op is at most
//! `pipeline_depth` past the commit_max it carries, so that is at most
//! `pipeline_depth` prepares.
//!
//! A suspicion timeout that fires is counted and backs off; voting for a
//! view change is T2.2.3.
//!
//! Connections: a replica connects to every other replica and sends only on
//! those connections, and receives only on the connections others open to
//! it, so it never has to learn who is at the other end of an accepted one.
//! It replies to a client on the accepted connection its request came on.

use std::collections::VecDeque;

use granite_base::checksum::checksum;
use granite_base::constants::{FORMAT, PROCESS_CONFIG_DEFAULT};
use granite_base::header::{Command, Error, Header, Operation, VERSION};
use granite_io::event_loop::{Class, Tasks};
use granite_io::message_pool::{Message, MessagePool};
use granite_io::{Completion, Connection, Full, Io, Peer, Tag};
use granite_storage::wal::{Event, Wal};
use zerocopy::{FromZeros, IntoBytes};

use crate::state_machine::StateMachine;
use crate::timeout::Timeout;

const MILLISECOND: u64 = 1_000_000;
const HEADER: usize = size_of::<Header>();
const SECTOR: usize = FORMAT.sector_size as usize;
/// Prepares in flight at the primary.
const PIPELINE: usize = FORMAT.pipeline_depth as usize;
/// Requests the primary queues while the pipeline is full.
// shortcut: one pipeline's worth; size it by the clients a group serves
// with client sessions (T2.2.9).
const REQUESTS_MAX: usize = PIPELINE;
/// Messages queued per peer, and sends in flight per peer: a pipeline of
/// prepares and their resends, or as many acknowledgements.
const OUTBOX: usize = 2 * PIPELINE;
/// Accepted connections kept per replica of the group: a peer's old
/// connection may linger until its error completes.
const INBOUND_PER_REPLICA: usize = 2;
/// One more than the largest command code, to index counters by command.
const COMMANDS: usize = Command::Vote as usize + 1;

/// Replica status (document 02 section 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Normal,
    ViewChange,
    Recovering,
    /// The head of the WAL is uncertain after a fault.
    RecoveringHead,
}

/// Why a received message was dropped before dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// The header or body does not check, or the sender's index is not one
    /// of the group's replicas.
    Invalid,
    /// The command code is not a [`Command`].
    UnknownCommand,
    WrongCluster,
    WrongGroup,
    /// The command is not accepted in the replica's status.
    WrongStatus,
    /// A command only clients receive.
    NotForReplica,
}

impl Dropped {
    const ALL: [Dropped; 6] = [
        Dropped::Invalid,
        Dropped::UnknownCommand,
        Dropped::WrongCluster,
        Dropped::WrongGroup,
        Dropped::WrongStatus,
        Dropped::NotForReplica,
    ];
}

/// A request refused because the primary's queue is full, or no buffer is
/// free to hold it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Busy;

/// The statuses that accept `command`, or why every status drops it
/// (document 02 section 2, "Accepted in").
fn accepted(command: Command) -> Result<&'static [Status], Dropped> {
    use Command as C;
    use Status::*;
    Ok(match command {
        C::Reply | C::Eviction => return Err(Dropped::NotForReplica),
        // Only the primary of a normal view takes client and cross-shard
        // work, sends commits and answers request_start_view; prepare_ok
        // acknowledges a prepare of a normal view.
        C::Request
        | C::PrepareOk
        | C::Commit
        | C::RequestStartView
        | C::RequestSyncCheckpoint
        | C::SyncCheckpoint
        | C::Propose
        | C::Vote => &[Normal],
        // The new primary repairs its log during the view change.
        C::Prepare | C::RequestHeaders | C::Headers | C::RequestPrepare => &[Normal, ViewChange],
        // Only a new primary collecting its log reads a nack.
        C::NackPrepare => &[ViewChange],
        // A recovering replica votes for the next view (document 02
        // section 5); one with an uncertain head never votes.
        C::StartViewChange | C::DoViewChange => &[Normal, ViewChange, Recovering],
        // A start_view is how a replica with an uncertain head learns it;
        // grid repair and pings do not depend on the log.
        C::StartView | C::RequestBlock | C::Block | C::Ping | C::Pong => {
            &[Normal, ViewChange, Recovering, RecoveringHead]
        }
    })
}

/// Who a replica is and who its peers are.
#[derive(Clone, Debug)]
pub struct Config {
    pub cluster: u128,
    pub group: u32,
    /// This replica's index in the group.
    pub replica: u8,
    /// The node of each replica of the group, by index; its length is the
    /// replica count.
    pub peers: Vec<Peer>,
}

/// Tag kinds, in the high 16 bits of a tag; the argument is below.
const T_CONNECT: u64 = 1; // the replica index
const T_ACCEPT: u64 = 2;
const T_RECV: u64 = 3; // the connection
const T_SEND: u64 = 4; // the replica index
const T_REPLY: u64 = 5; // the connection
const T_WAL: u64 = 6; // the log's tags, from zero

fn tag(kind: u64, arg: u64) -> Tag {
    assert!(arg < 1 << 48);
    kind << 48 | arg
}

/// Connection to a replica, as the sender of that link.
#[derive(Debug, Default)]
struct Link {
    connection: Option<Connection>,
    connecting: bool,
    /// Sends in flight.
    sending: u32,
    /// Messages not yet sent, oldest first.
    queue: VecDeque<Message>,
}

/// A connection another node opened to this one.
#[derive(Clone, Copy, Debug)]
struct Inbound {
    connection: Connection,
    /// A receive is posted on it.
    posted: bool,
    /// A reply is being sent on it.
    sending: bool,
}

/// A prepare this replica holds.
struct Entry {
    op: u64,
    checksum: u128,
    /// The prepare, its header sealed and zeros to the end of its sector.
    message: Message,
    /// At the primary: the replicas known to hold it durably, a bit each.
    acks: u64,
    /// At the primary: the connection the request came on.
    client: Option<Connection>,
}

pub struct Replica<'a, S> {
    config: Config,
    status: Status,
    view: u32,
    /// The view in which the log was last installed.
    log_view: u32,
    /// Highest op prepared.
    op: u64,
    /// Highest op executed.
    commit_min: u64,
    /// Highest op known committed.
    commit_max: u64,
    /// Highest op handed to the WAL.
    appended: u64,
    /// Highest op the WAL reports durable.
    durable: u64,
    /// Highest op acknowledged with prepare_ok.
    acknowledged: u64,
    /// The header checksum of op `op`: the next prepare's parent.
    parent: u128,
    /// The timestamp of op `op`.
    timestamp: u64,
    /// The prepares from the oldest not yet both executed and appended to
    /// op `op`, in op order.
    pipeline: VecDeque<Entry>,
    /// At the primary: requests waiting for room in the pipeline, each with
    /// the connection it came on.
    requests: VecDeque<(Message, Option<Connection>)>,
    wal: Wal,
    state_machine: S,
    pool: &'a MessagePool,
    /// The primary's commit heartbeat.
    heartbeat: Timeout,
    /// A backup's silence from the primary.
    suspicion: Timeout,
    /// To each replica, by index; this replica's own entry stays unused.
    links: Vec<Link>,
    inbound: Vec<Inbound>,
    accepting: bool,
    received: [u64; COMMANDS],
    dropped: [u64; Dropped::ALL.len()],
    busy: u64,
    gaps: u64,
    replies: u64,
}

impl<'a, S: StateMachine> Replica<'a, S> {
    /// A replica of a new group, in status normal at view zero with an
    /// empty log, with buffers from `pool`. It starts connecting to its
    /// peers at once.
    pub fn new<I: Io>(io: &mut I, config: Config, state_machine: S, pool: &'a MessagePool) -> Self {
        let count = config.peers.len();
        // The primary keeps acknowledgements as a bit per replica.
        assert!(0 < count && count <= 64);
        assert!(usize::from(config.replica) < count);
        // shortcut: the process configuration is the default one; take it as
        // a parameter once `granite start` reads it.
        let c = PROCESS_CONFIG_DEFAULT;
        let (group, replica) = (config.group, config.replica);
        // Op 0 is the root of every replica's hash chain.
        let root = checksum(&config.cluster.to_le_bytes());
        let buffer = pool
            .acquire()
            .expect("a buffer for the WAL's header writes");
        let mut this = Replica {
            status: Status::Normal,
            view: 0,
            log_view: 0,
            op: 0,
            commit_min: 0,
            commit_max: 0,
            appended: 0,
            durable: 0,
            acknowledged: 0,
            parent: root,
            timestamp: 0,
            pipeline: VecDeque::with_capacity(PIPELINE),
            requests: VecDeque::with_capacity(REQUESTS_MAX),
            wal: Wal::new(tag(T_WAL, 0), buffer, 0, root),
            state_machine,
            pool,
            heartbeat: Timeout::new(c.heartbeat_interval_ms * MILLISECOND, 0, group, replica, 0),
            suspicion: Timeout::new(
                c.suspicion_timeout_ms * MILLISECOND,
                c.suspicion_jitter_max_ms * MILLISECOND,
                group,
                replica,
                1,
            ),
            links: (0..count)
                .map(|_| Link {
                    queue: VecDeque::with_capacity(OUTBOX),
                    ..Link::default()
                })
                .collect(),
            inbound: Vec::with_capacity(INBOUND_PER_REPLICA * count),
            accepting: false,
            received: [0; COMMANDS],
            dropped: [0; Dropped::ALL.len()],
            busy: 0,
            gaps: 0,
            replies: 0,
            config,
        };
        this.arm(io.monotonic());
        this.connect(io);
        this
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn view(&self) -> u32 {
        self.view
    }

    pub fn log_view(&self) -> u32 {
        self.log_view
    }

    pub fn op(&self) -> u64 {
        self.op
    }

    pub fn commit_min(&self) -> u64 {
        self.commit_min
    }

    pub fn commit_max(&self) -> u64 {
        self.commit_max
    }

    /// Highest op the WAL reports durable.
    pub fn durable(&self) -> u64 {
        self.durable
    }

    /// Highest op this replica sent prepare_ok for.
    pub fn acknowledged(&self) -> u64 {
        self.acknowledged
    }

    /// Prepares held: at the primary, those not yet committed.
    pub fn pipeline(&self) -> usize {
        self.pipeline.len()
    }

    /// Requests waiting at the primary for room in the pipeline.
    pub fn queued(&self) -> usize {
        self.requests.len()
    }

    /// Requests refused with [`Busy`].
    pub fn busy(&self) -> u64 {
        self.busy
    }

    /// Prepares not written because they do not extend the chain: a gap
    /// before them, or another parent. Repair (T2.2.4) starts from them.
    pub fn gaps(&self) -> u64 {
        self.gaps
    }

    /// Replies made by the primary, sent or not.
    pub fn replies(&self) -> u64 {
        self.replies
    }

    pub fn state_machine(&self) -> &S {
        &self.state_machine
    }

    /// The heartbeat and suspicion timeouts.
    pub fn timeouts(&self) -> [&Timeout; 2] {
        [&self.heartbeat, &self.suspicion]
    }

    /// Messages of `command` dispatched.
    pub fn received(&self, command: Command) -> u64 {
        self.received[command as usize]
    }

    /// Messages dropped for `reason`.
    pub fn dropped(&self, reason: Dropped) -> u64 {
        self.dropped[reason as usize]
    }

    fn replica_count(&self) -> u8 {
        u8::try_from(self.config.peers.len()).expect("checked in new")
    }

    fn primary(&self) -> u8 {
        u8::try_from(self.view % u32::from(self.replica_count())).expect("below the replica count")
    }

    /// Whether this replica is the primary of a normal view.
    fn is_primary(&self) -> bool {
        self.status == Status::Normal && self.primary() == self.config.replica
    }

    /// The replication quorum (document 02 section 1).
    fn quorum(&self) -> u32 {
        u32::from(self.replica_count()).div_ceil(2)
    }

    /// Whether the primary may commit `entry`: a replication quorum holds
    /// it durably, the primary included.
    fn replicated(&self, entry: &Entry) -> bool {
        entry.acks & 1 << self.config.replica != 0 && entry.acks.count_ones() >= self.quorum()
    }

    fn index(&self, op: u64) -> Option<usize> {
        let first = self.pipeline.front()?.op;
        let index = usize::try_from(op.checked_sub(first)?).ok()?;
        (index < self.pipeline.len()).then_some(index)
    }

    /// Arms the timeouts of the replica's role in its view.
    fn arm(&mut self, now: u64) {
        if self.is_primary() {
            self.heartbeat.start(now);
            self.suspicion.stop();
        } else {
            self.heartbeat.stop();
            self.suspicion.start(now);
        }
    }

    /// Checks a received message and hands it to its command's handler, or
    /// drops and counts it. A request taken this way has no connection to
    /// reply on.
    pub fn receive<I: Io>(&mut self, io: &mut I, bytes: &[u8]) {
        self.receive_on(io, bytes, None);
    }

    fn receive_on<I: Io>(&mut self, io: &mut I, bytes: &[u8], from: Option<Connection>) {
        match self.check(bytes) {
            Ok(header) => self.dispatch(io, &header, &bytes[HEADER..], from),
            Err(reason) => self.dropped[reason as usize] += 1,
        }
    }

    fn check(&self, bytes: &[u8]) -> Result<Header, Dropped> {
        let header = match Header::decode(bytes) {
            Ok(header) => *header,
            Err(Error::Command) => return Err(Dropped::UnknownCommand),
            Err(_) => return Err(Dropped::Invalid),
        };
        if header.check_body(&bytes[HEADER..]).is_err() {
            return Err(Dropped::Invalid);
        }
        if header.cluster.get() != self.config.cluster {
            return Err(Dropped::WrongCluster);
        }
        if header.group.get() != self.config.group {
            return Err(Dropped::WrongGroup);
        }
        let command = header.command().expect("decode checked the command");
        // A client's request names no replica.
        if command != Command::Request && header.replica >= self.replica_count() {
            return Err(Dropped::Invalid);
        }
        if !accepted(command)?.contains(&self.status) {
            return Err(Dropped::WrongStatus);
        }
        Ok(header)
    }

    /// One entry per command; the handlers of the protocol fill them in.
    fn dispatch<I: Io>(
        &mut self,
        io: &mut I,
        header: &Header,
        body: &[u8],
        from: Option<Connection>,
    ) {
        let command = header.command().expect("checked");
        self.received[command as usize] += 1;
        use Command as C;
        match command {
            // A backup refuses nothing: the client learns the primary with
            // sessions (T2.2.9). A busy primary's refusal is counted.
            C::Request => {
                if self.is_primary() {
                    let _ = self.queue(io, header, body, from);
                }
            }
            C::Prepare => {
                self.heard(io, header);
                self.on_prepare(io, header, body);
            }
            C::Commit => {
                self.heard(io, header);
                if self.is_from_primary(header) {
                    self.commit_max = self.commit_max.max(header.commit.get());
                    self.execute(io);
                }
            }
            C::PrepareOk => self.on_prepare_ok(io, header),
            // View change (T2.2.3).
            C::StartViewChange | C::DoViewChange | C::StartView | C::RequestStartView => {}
            // Repair (T2.2.4) and the nack of a truncation (T2.2.3).
            C::RequestHeaders | C::Headers | C::RequestPrepare | C::NackPrepare => {}
            // Grid repair and state sync.
            C::RequestBlock | C::Block | C::RequestSyncCheckpoint | C::SyncCheckpoint => {}
            // Liveness and clock samples.
            C::Ping | C::Pong => {}
            // Cross-shard transactions (document 04 section 4).
            C::Propose | C::Vote => {}
            C::Reply | C::Eviction => unreachable!("check drops what only clients receive"),
        }
    }

    /// A prepare or commit: from the primary of this view, it ends the
    /// backup's suspicion.
    fn heard<I: Io>(&mut self, io: &mut I, header: &Header) {
        let from_primary =
            header.replica == self.primary() && header.replica != self.config.replica;
        if header.view.get() == self.view && from_primary {
            self.suspicion.start(io.monotonic());
        }
    }

    /// Whether this replica is a backup in status normal and `header` comes
    /// from the primary of its view.
    fn is_from_primary(&self, header: &Header) -> bool {
        self.status == Status::Normal
            && header.view.get() == self.view
            && header.replica == self.primary()
            && header.replica != self.config.replica
    }

    /// A client request, taken at the primary as one received with no
    /// connection to reply on: it is prepared once the pipeline has room.
    /// Refused with [`Busy`] when the queue is full.
    pub fn request<I: Io>(&mut self, io: &mut I, header: &Header, body: &[u8]) -> Result<(), Busy> {
        assert!(self.is_primary(), "requests go to the primary");
        assert_eq!(header.command(), Some(Command::Request));
        assert_eq!(header.check_body(body), Ok(()));
        self.queue(io, header, body, None)
    }

    fn queue<I: Io>(
        &mut self,
        io: &mut I,
        header: &Header,
        body: &[u8],
        client: Option<Connection>,
    ) -> Result<(), Busy> {
        assert!(self.is_primary());
        let message = (self.requests.len() < REQUESTS_MAX)
            .then(|| self.pool.acquire())
            .flatten();
        let Some(mut message) = message else {
            self.busy += 1;
            return Err(Busy);
        };
        let bytes = message.bytes_mut().expect("only handle");
        bytes[..HEADER].copy_from_slice(header.as_bytes());
        bytes[HEADER..HEADER + body.len()].copy_from_slice(body);
        self.requests.push_back((message, client));
        self.prepare(io);
        Ok(())
    }

    /// At the primary: turns queued requests into prepares while the
    /// pipeline has room, and sends each to the WAL and the backups.
    fn prepare<I: Io>(&mut self, io: &mut I) {
        while self.is_primary() && self.pipeline.len() < PIPELINE {
            let Some((mut message, client)) = self.requests.pop_front() else {
                break;
            };
            let request = *Header::decode(message.bytes()).expect("a checked request");
            let size = request.size.get() as usize;
            let mut header = self.header(Command::Prepare);
            header.parent = self.parent.into();
            header.client = request.client;
            header.request = request.request;
            header.operation = request.operation;
            header.op = (self.op + 1).into();
            header.commit = self.commit_max.into();
            // shortcut: the wall clock, kept strictly increasing; T2.2.8
            // synchronises it and adds the previous batch's size.
            let now = u64::try_from(io.realtime()).unwrap_or(0);
            self.timestamp = now.max(self.timestamp + 1);
            header.timestamp = self.timestamp.into();
            let bytes = message.bytes_mut().expect("only handle");
            header.seal(&bytes[HEADER..size]);
            bytes[..HEADER].copy_from_slice(header.as_bytes());
            bytes[size..size.next_multiple_of(SECTOR)].fill(0);
            assert!(self.commit_max + (PIPELINE as u64) > self.op);
            self.push(&header, message.clone(), client);
            self.broadcast(io, &message);
        }
        self.append(io);
    }

    /// Adds the prepare in `message`, whose header is `header`, to the log.
    fn push(&mut self, header: &Header, message: Message, client: Option<Connection>) {
        let op = header.op.get();
        assert_eq!(op, self.op + 1, "prepares extend the log in op order");
        assert_eq!(header.parent.get(), self.parent, "and its hash chain");
        assert!(self.pipeline.len() < PIPELINE);
        self.op = op;
        self.parent = header.checksum.get();
        self.timestamp = header.timestamp.get();
        self.pipeline.push_back(Entry {
            op,
            checksum: self.parent,
            message,
            acks: 0,
            client,
        });
    }

    /// At a backup: accepts the primary's next prepare, writes it and
    /// acknowledges it once durable; learns commit_max either way.
    fn on_prepare<I: Io>(&mut self, io: &mut I, header: &Header, body: &[u8]) {
        if !self.is_from_primary(header) {
            return;
        }
        self.commit_max = self.commit_max.max(header.commit.get());
        // Executing first makes room for this prepare.
        self.execute(io);
        let op = header.op.get();
        if op <= self.op {
            // A resend or a duplicate: acknowledge again what is durable.
            let held = self
                .index(op)
                .is_some_and(|i| self.pipeline[i].checksum == header.checksum.get());
            if held && op <= self.durable {
                self.acknowledge(io, op);
            }
        } else if op > self.op + 1 || header.parent.get() != self.parent {
            // shortcut: counted only; repair (T2.2.4) fetches what is missing.
            self.gaps += 1;
        } else if self.pipeline.len() < PIPELINE
            && let Some(mut message) = self.pool.acquire()
        {
            let size = header.size.get() as usize;
            let bytes = message.bytes_mut().expect("only handle");
            bytes[..HEADER].copy_from_slice(header.as_bytes());
            bytes[HEADER..size].copy_from_slice(body);
            bytes[size..size.next_multiple_of(SECTOR)].fill(0);
            self.push(header, message, None);
            self.append(io);
        }
        // Otherwise there is no room: the primary sends it again.
    }

    /// At the primary: a backup holds a prepare durably.
    fn on_prepare_ok<I: Io>(&mut self, io: &mut I, header: &Header) {
        if !self.is_primary() || header.view.get() != self.view {
            return;
        }
        if let Some(i) = self.index(header.op.get())
            && header.replica != self.config.replica
        {
            self.pipeline[i].acks |= 1 << header.replica;
            self.commit(io);
        }
    }

    /// Hands the prepares not yet in the WAL to it, in op order, as far as
    /// it takes them.
    fn append<I: Io>(&mut self, io: &mut I) {
        while self.appended < self.op {
            let i = self.index(self.appended + 1).expect("held until appended");
            let message = self.pipeline[i].message.clone();
            if self.wal.append(io, message).is_err() {
                return; // Retried after a completion or on the next tick.
            }
            self.appended += 1;
        }
    }

    /// The WAL made every op up to `op` durable.
    fn on_durable<I: Io>(&mut self, io: &mut I, op: u64) {
        assert!(self.durable < op && op <= self.appended);
        let from = self.durable + 1;
        self.durable = op;
        if self.is_primary() {
            let me = 1 << self.config.replica;
            for entry in self.pipeline.iter_mut().filter(|e| e.op <= op) {
                entry.acks |= me;
            }
            self.commit(io);
        } else if self.status == Status::Normal {
            for op in from..=op {
                self.acknowledge(io, op);
            }
        }
    }

    /// Sends prepare_ok for op `op` to the primary.
    fn acknowledge<I: Io>(&mut self, io: &mut I, op: u64) {
        // Invariant 6: a replica acknowledges only a prepare it holds durably.
        assert!(
            op <= self.durable,
            "op {op} is acknowledged before it is durable"
        );
        self.acknowledged = self.acknowledged.max(op);
        let mut header = self.header(Command::PrepareOk);
        header.op = op.into();
        if let Some(message) = self.message(&mut header) {
            self.send(io, self.primary(), &message);
        }
    }

    /// At the primary: commits, in op order, the prepares a replication
    /// quorum holds, then executes them.
    fn commit<I: Io>(&mut self, io: &mut I) {
        while let Some(i) = self.index(self.commit_max + 1)
            && self.replicated(&self.pipeline[i])
        {
            self.commit_max += 1;
        }
        self.execute(io);
        self.prepare(io);
    }

    /// Executes, in op order, every committed op this replica holds, and
    /// at the primary replies to each; then lets go of the prepares that
    /// are executed and in the WAL.
    fn execute<I: Io>(&mut self, io: &mut I) {
        while self.commit_min < self.commit_max.min(self.op) {
            let op = self.commit_min + 1;
            let Some(mut reply) = self.pool.acquire() else {
                return; // Retried on the next tick.
            };
            let entry = &self.pipeline[self.index(op).expect("held until executed")];
            assert_eq!(entry.op, op, "ops are executed once each, in op order");
            let primary = self.is_primary();
            // The primary never executes, and so never replies, before a
            // replication quorum holds the op.
            assert!(
                !primary || self.replicated(entry),
                "op {op} is not replicated"
            );
            let (message, client) = (entry.message.clone(), entry.client);
            let header = *Header::decode(message.bytes()).expect("sealed");
            let body = &message.bytes()[HEADER..header.size.get() as usize];
            let len = self.state_machine.execute(
                op,
                header.timestamp.get(),
                header.operation().expect("checked"),
                body,
                &mut reply.bytes_mut().expect("only handle")[HEADER..],
            );
            self.commit_min = op;
            if primary {
                self.reply(io, &header, reply, len, client);
            }
        }
        while let Some(entry) = self.pipeline.front()
            && entry.op <= self.commit_min.min(self.appended)
        {
            self.pipeline.pop_front();
        }
    }

    /// Sends the reply to `prepare`, whose body of `len` bytes is in
    /// `reply`, on the connection the request came on.
    fn reply<I: Io>(
        &mut self,
        io: &mut I,
        prepare: &Header,
        mut reply: Message,
        len: u32,
        client: Option<Connection>,
    ) {
        let entry = &self.pipeline[self.index(prepare.op.get()).expect("held")];
        assert!(
            self.replicated(entry),
            "a reply follows a replication quorum"
        );
        self.replies += 1;
        let Some(connection) = client else { return };
        let mut header = self.header(Command::Reply);
        header.client = prepare.client;
        header.request = prepare.request;
        header.operation = prepare.operation;
        header.op = prepare.op;
        header.commit = self.commit_max.into();
        header.timestamp = prepare.timestamp;
        let bytes = reply.bytes_mut().expect("only handle");
        header.seal(&bytes[HEADER..HEADER + len as usize]);
        bytes[..HEADER].copy_from_slice(header.as_bytes());
        let Some(inbound) = self.inbound.iter_mut().find(|c| c.connection == connection) else {
            return; // The client is gone.
        };
        // shortcut: one reply in flight per connection, and a reply made
        // while one is in flight is dropped; client sessions (T2.2.9) keep
        // replies for the client's retry.
        if !inbound.sending {
            let tag = tag(T_REPLY, connection.0);
            let size = header.size.get();
            inbound.sending = io.send(tag, connection, size, reply).is_ok();
        }
    }

    fn on_heartbeat<I: Io>(&mut self, io: &mut I, now: u64) {
        assert!(self.is_primary());
        let mut header = self.header(Command::Commit);
        header.commit = self.commit_max.into();
        if let Some(message) = self.message(&mut header) {
            self.broadcast(io, &message);
        }
        // Prepares lost on the way, or refused for lack of room, go again,
        // to each backup that has nothing queued still on its way.
        for i in 0..self.pipeline.len() {
            let (message, acks) = (self.pipeline[i].message.clone(), self.pipeline[i].acks);
            for r in 0..self.replica_count() {
                let idle = self.links[usize::from(r)].queue.is_empty();
                if acks & 1 << r == 0 && r != self.config.replica && idle {
                    self.send(io, r, &message);
                }
            }
        }
        // Periodic, so no backoff.
        self.heartbeat.start(now);
    }

    fn on_suspicion<I: Io>(&mut self, _io: &mut I) {
        // T2.2.3: vote for the next view with start_view_change. Until then
        // the timeout only counts and backs off.
    }

    /// A header of `command` from this replica in its view.
    fn header(&self, command: Command) -> Header {
        let mut header = Header::new_zeroed();
        header.cluster = self.config.cluster.into();
        header.group = self.config.group.into();
        header.view = self.view.into();
        header.replica = self.config.replica;
        header.command = command as u8;
        header.operation = Operation::None as u8;
        header.version = VERSION;
        header
    }

    /// Seals `header` with an empty body into a buffer, if one is free;
    /// without one the message is not sent, as the protocol resends.
    fn message(&self, header: &mut Header) -> Option<Message> {
        header.seal(&[]);
        let mut message = self.pool.acquire()?;
        message.bytes_mut().expect("only handle")[..HEADER].copy_from_slice(header.as_bytes());
        Some(message)
    }

    /// Sends `message` to every other replica.
    fn broadcast<I: Io>(&mut self, io: &mut I, message: &Message) {
        for r in 0..self.replica_count() {
            if r != self.config.replica {
                self.send(io, r, message);
            }
        }
    }

    /// Sends the sealed message in `message` to replica `to`, after those
    /// queued before it. Without a connection open or being opened, or with
    /// a full queue, it is dropped, as the protocol resends.
    fn send<I: Io>(&mut self, io: &mut I, to: u8, message: &Message) {
        let link = &mut self.links[usize::from(to)];
        if (link.connection.is_some() || link.connecting) && link.queue.len() < OUTBOX {
            link.queue.push_back(message.clone());
            self.flush(io, to);
        }
    }

    /// Sends what is queued for replica `to`, in order, as far as the
    /// connection, the bound on sends in flight and the backend allow.
    fn flush<I: Io>(&mut self, io: &mut I, to: u8) {
        let link = &mut self.links[usize::from(to)];
        let Some(connection) = link.connection else {
            return;
        };
        while link.sending < OUTBOX as u32
            && let Some(message) = link.queue.pop_front()
        {
            let len = Header::decode(message.bytes()).expect("sealed").size.get();
            if let Err(Full(message)) =
                io.send(tag(T_SEND, u64::from(to)), connection, len, message)
            {
                link.queue.push_front(message);
                return; // Retried on the next completion or tick.
            }
            link.sending += 1;
        }
    }

    /// Opens missing connections, accepts, and posts receives that a full
    /// pool or backend refused before.
    fn connect<I: Io>(&mut self, io: &mut I) {
        for r in 0..self.replica_count() {
            let link = &mut self.links[usize::from(r)];
            if r == self.config.replica || link.connection.is_some() || link.connecting {
                continue;
            }
            let peer = self.config.peers[usize::from(r)];
            link.connecting = io.connect(tag(T_CONNECT, u64::from(r)), peer).is_ok();
        }
        if !self.accepting {
            self.accepting = io.accept(tag(T_ACCEPT, 0)).is_ok();
        }
        for i in 0..self.inbound.len() {
            if !self.inbound[i].posted
                && let Some(buffer) = self.pool.acquire()
            {
                self.post(io, i, buffer);
            }
        }
    }

    fn post<I: Io>(&mut self, io: &mut I, i: usize, buffer: Message) {
        let connection = self.inbound[i].connection;
        let tag = tag(T_RECV, connection.0);
        self.inbound[i].posted = io.recv(tag, connection, buffer).is_ok();
    }
}

impl<I: Io, S: StateMachine> Tasks<I> for Replica<'_, S> {
    fn complete(&mut self, io: &mut I, completion: Completion) {
        match completion {
            Completion::Connected { tag, result } => {
                let to = u8::try_from(tag & 0xff).expect("an index");
                let link = &mut self.links[usize::from(to)];
                link.connecting = false;
                link.connection = result.ok();
                if link.connection.is_none() {
                    link.queue.clear();
                }
                self.flush(io, to);
            }
            Completion::Accepted { result, .. } => {
                self.accepting = false;
                // A failed accept is retried on the next tick, not at once,
                // so a failing network cannot spin with the clock stopped.
                let Ok(connection) = result else { return };
                if self.inbound.len() == INBOUND_PER_REPLICA * self.config.peers.len() {
                    io.close(connection); // The peer reconnects later.
                } else {
                    self.inbound.push(Inbound {
                        connection,
                        posted: false,
                        sending: false,
                    });
                }
                self.connect(io);
            }
            Completion::Received {
                tag,
                buffer,
                result,
            } => {
                let connection = Connection(tag & ((1 << 48) - 1));
                let Some(i) = self.inbound.iter().position(|c| c.connection == connection) else {
                    return; // Closed already.
                };
                let Ok(len) = result else {
                    self.inbound.swap_remove(i);
                    io.close(connection);
                    return;
                };
                self.inbound[i].posted = false;
                self.receive_on(io, &buffer.bytes()[..len as usize], Some(connection));
                self.post(io, i, buffer);
            }
            Completion::Sent { tag, .. } if tag >> 48 == T_REPLY => {
                let connection = Connection(tag & ((1 << 48) - 1));
                if let Some(c) = self.inbound.iter_mut().find(|c| c.connection == connection) {
                    c.sending = false;
                }
            }
            Completion::Sent { tag, result, .. } => {
                let to = u8::try_from(tag & 0xff).expect("an index");
                let link = &mut self.links[usize::from(to)];
                link.sending -= 1;
                if result.is_err() {
                    link.queue.clear();
                    if let Some(connection) = link.connection.take() {
                        io.close(connection); // The next tick reconnects.
                    }
                }
                self.flush(io, to);
            }
            Completion::Read { .. } | Completion::Write { .. } | Completion::Fsync { .. } => {
                let mut last = None;
                self.wal.complete(io, completion, |event| match event {
                    Event::Appended { op } => last = Some(op),
                    Event::Read { .. } => unreachable!("the replica reads nothing back yet"),
                });
                if let Some(op) = last {
                    self.on_durable(io, op);
                }
                self.append(io);
            }
        }
    }

    fn tick(&mut self, io: &mut I) {
        self.connect(io);
        for r in 0..self.replica_count() {
            self.flush(io, r);
        }
        // Retry what a full backend or pool refused.
        self.wal.pump(io);
        self.append(io);
        self.execute(io);
        let now = io.monotonic();
        if self.heartbeat.fire(now) {
            self.on_heartbeat(io, now);
        }
        if self.suspicion.fire(now) {
            self.on_suspicion(io);
        }
    }

    fn run(&mut self, _io: &mut I, _class: Class) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use granite_base::checksum::checksum;
    use granite_io::Network;
    use granite_io::memory::{Hub, Memory};
    use std::mem::offset_of;
    use zerocopy::TryFromBytes;

    const CLUSTER: u128 = 7;
    const GROUP: u32 = 3;
    const ALL: [Status; 4] = [
        Status::Normal,
        Status::ViewChange,
        Status::Recovering,
        Status::RecoveringHead,
    ];

    struct Noop;

    impl StateMachine for Noop {
        fn execute(&mut self, _: u64, _: u64, _: Operation, _: &[u8], _: &mut [u8]) -> u32 {
            0
        }
    }

    fn commands() -> impl Iterator<Item = Command> {
        (0..COMMANDS).filter_map(|c| Command::try_read_from_bytes(&[c as u8]).ok())
    }

    /// A sealed message of `command` from replica 1, with no body.
    fn message(command: Command, cluster: u128, group: u32) -> Vec<u8> {
        let mut header = Header::new_zeroed();
        header.cluster = cluster.into();
        header.group = group.into();
        header.replica = 1;
        header.command = command as u8;
        header.version = VERSION;
        header.seal(&[]);
        header.as_bytes().to_vec()
    }

    /// The zones of a data file, with the grid cut to one sector.
    fn zones() -> [u64; 4] {
        let mut zones = granite_storage::layout::zone_sizes(FORMAT.grid_growth_step);
        zones[granite_io::Zone::Grid as usize] = FORMAT.sector_size;
        zones
    }

    /// Hands `r` every completion `io` has.
    fn drain(io: &mut Memory, r: &mut Replica<'_, Noop>) {
        while let Some(completion) = io.poll() {
            r.complete(io, completion);
        }
    }

    /// A sealed prepare of op `op` from replica 0 in view 0, chained to
    /// `parent`, with `body`.
    fn prepare(op: u64, parent: u128, body: &[u8]) -> Vec<u8> {
        let mut header = Header::new_zeroed();
        header.cluster = CLUSTER.into();
        header.group = GROUP.into();
        header.command = Command::Prepare as u8;
        header.version = VERSION;
        header.op = op.into();
        header.parent = parent.into();
        header.timestamp = op.into();
        header.seal(body);
        [header.as_bytes(), body].concat()
    }

    /// A sealed request with `body`.
    fn request(body: &[u8]) -> (Header, Vec<u8>) {
        let mut header = Header::new_zeroed();
        header.cluster = CLUSTER.into();
        header.group = GROUP.into();
        header.command = Command::Request as u8;
        header.version = VERSION;
        header.client = 9.into();
        header.seal(body);
        (header, body.to_vec())
    }

    #[test]
    fn with_a_full_pipeline_the_ninth_request_waits_and_a_full_queue_is_busy() {
        let (hub, pool) = (Hub::new(), MessagePool::new(24));
        let mut io = Memory::new(&hub, Peer(0), zones(), 64, 0);
        // Backups 1 and 2 never answer, so nothing commits.
        let mut r = replica(&mut io, &pool);
        let (header, body) = request(b"x");
        for n in 1..=PIPELINE + REQUESTS_MAX {
            assert_eq!(r.request(&mut io, &header, &body), Ok(()));
            drain(&mut io, &mut r);
            assert_eq!(r.pipeline(), n.min(PIPELINE));
            assert_eq!(r.queued(), n.saturating_sub(PIPELINE));
        }
        assert_eq!((r.op(), r.durable(), r.commit_max()), (8, 8, 0));
        assert_eq!(r.request(&mut io, &header, &body), Err(Busy));
        assert_eq!((r.busy(), r.queued(), r.op()), (1, REQUESTS_MAX, 8));
        // An acknowledgement from backup 1 commits op 1, which makes room.
        let mut ok = Header::new_zeroed();
        ok.cluster = CLUSTER.into();
        ok.group = GROUP.into();
        ok.replica = 1;
        ok.command = Command::PrepareOk as u8;
        ok.version = VERSION;
        ok.op = 1.into();
        ok.seal(&[]);
        r.receive(&mut io, ok.as_bytes());
        drain(&mut io, &mut r);
        assert_eq!(
            (r.commit_min(), r.op(), r.pipeline(), r.queued()),
            (1, 9, 8, 7)
        );
        assert_eq!(r.replies(), 1);
    }

    #[test]
    fn a_prepare_that_does_not_extend_the_chain_is_never_written() {
        let (hub, pool) = (Hub::new(), MessagePool::new(8));
        let mut io = Memory::new(&hub, Peer(1), zones(), 64, 0);
        let config = Config {
            cluster: CLUSTER,
            group: GROUP,
            replica: 1,
            peers: (0..3).map(Peer).collect(),
        };
        let mut r = Replica::new(&mut io, config, Noop, &pool);
        drain(&mut io, &mut r);
        let root = checksum(&CLUSTER.to_le_bytes());
        // Another parent, then a gap: neither submits a write.
        for bytes in [prepare(1, root ^ 1, b"a"), prepare(2, root, b"a")] {
            r.receive(&mut io, &bytes);
            assert_eq!(io.ready(), 0, "nothing was submitted");
        }
        assert_eq!((r.op(), r.gaps()), (0, 2));
        // The next op on the right parent is written, and acknowledged only
        // once the WAL reports it durable.
        r.receive(&mut io, &prepare(1, root, b"a"));
        assert!(io.ready() > 0, "the write was submitted");
        assert_eq!((r.op(), r.durable(), r.acknowledged()), (1, 0, 0));
        drain(&mut io, &mut r);
        assert_eq!((r.durable(), r.acknowledged()), (1, 1));
    }

    #[test]
    fn the_primary_replies_on_the_connection_the_request_came_on() {
        let (hub, pool) = (Hub::new(), MessagePool::new(8));
        let mut io = Memory::new(&hub, Peer(0), zones(), 64, 0);
        let config = Config {
            cluster: CLUSTER,
            group: GROUP,
            replica: 0,
            peers: vec![Peer(0)],
        };
        // A group of one: its own durable write is a replication quorum.
        let mut r = Replica::new(&mut io, config, Noop, &pool);
        let mut client = Memory::new(&hub, Peer(7), [0; 4], 8, 7);
        client.connect(1, Peer(0)).unwrap();
        let mut connection = None;
        let (header, body) = request(b"hello");
        let mut reply = None;
        for _ in 0..20 {
            drain(&mut io, &mut r);
            while let Some(completion) = client.poll() {
                match completion {
                    Completion::Connected { tag: 1, result } => {
                        let c = result.unwrap();
                        connection = Some(c);
                        let mut message = pool.acquire().unwrap();
                        let bytes = message.bytes_mut().unwrap();
                        bytes[..HEADER].copy_from_slice(header.as_bytes());
                        bytes[HEADER..HEADER + body.len()].copy_from_slice(&body);
                        client.send(2, c, header.size.get(), message).unwrap();
                        client.recv(3, c, pool.acquire().unwrap()).unwrap();
                    }
                    Completion::Received { buffer, result, .. } => {
                        reply = Some(
                            *Header::decode(&buffer.bytes()[..result.unwrap() as usize]).unwrap(),
                        );
                    }
                    _ => {}
                }
            }
        }
        assert!(connection.is_some());
        let reply = reply.expect("a reply");
        assert_eq!(reply.command(), Some(Command::Reply));
        assert_eq!((reply.op.get(), reply.client.get()), (1, 9));
        assert_eq!((r.commit_min(), r.replies()), (1, 1));
    }

    fn replica<'a>(io: &mut Memory, pool: &'a MessagePool) -> Replica<'a, Noop> {
        let config = Config {
            cluster: CLUSTER,
            group: GROUP,
            replica: 0,
            peers: (0..3).map(Peer).collect(),
        };
        Replica::new(io, config, Noop, pool)
    }

    fn counts(r: &Replica<'_, Noop>) -> (Vec<u64>, Vec<u64>) {
        (
            commands().map(|c| r.received(c)).collect(),
            Dropped::ALL.iter().map(|&d| r.dropped(d)).collect(),
        )
    }

    #[test]
    fn messages_for_another_cluster_or_group_or_of_unknown_command_are_dropped_and_counted() {
        let (hub, pool) = (Hub::new(), MessagePool::new(4));
        let mut io = Memory::new(&hub, Peer(0), zones(), 8, 0);
        let mut r = replica(&mut io, &pool);
        let mut unknown = message(Command::Commit, CLUSTER, GROUP);
        unknown[offset_of!(Header, command)] = 0xEE;
        let sum = checksum(&unknown[offset_of!(Header, checksum_body)..]);
        unknown[..16].copy_from_slice(&sum.to_le_bytes());
        let mut corrupt = message(Command::Commit, CLUSTER, GROUP);
        corrupt[200] ^= 1;
        for (bytes, reason) in [
            (unknown, Dropped::UnknownCommand),
            (
                message(Command::Commit, CLUSTER + 1, GROUP),
                Dropped::WrongCluster,
            ),
            (
                message(Command::Commit, CLUSTER, GROUP + 1),
                Dropped::WrongGroup,
            ),
            (corrupt, Dropped::Invalid),
            (vec![0; 10], Dropped::Invalid),
            (
                message(Command::Reply, CLUSTER, GROUP),
                Dropped::NotForReplica,
            ),
        ] {
            let (received, mut dropped) = counts(&r);
            r.receive(&mut io, &bytes);
            dropped[reason as usize] += 1;
            assert_eq!(counts(&r), (received, dropped), "{reason:?}");
        }
        // Re-sealed with a replica index outside the group of three.
        let mut header = *Header::decode(&message(Command::Commit, CLUSTER, GROUP)).unwrap();
        header.replica = 3;
        header.seal(&[]);
        r.receive(&mut io, header.as_bytes());
        assert_eq!(r.dropped(Dropped::Invalid), 3);
        assert_eq!(r.received(Command::Commit), 0);
    }

    #[test]
    fn every_command_is_dispatched_in_the_statuses_that_accept_it_and_dropped_in_the_others() {
        let (hub, pool) = (Hub::new(), MessagePool::new(4));
        let mut io = Memory::new(&hub, Peer(0), zones(), 8, 0);
        let mut r = replica(&mut io, &pool);
        for command in commands() {
            for status in ALL {
                r.status = status;
                let (mut received, mut dropped) = counts(&r);
                r.receive(&mut io, &message(command, CLUSTER, GROUP));
                match accepted(command) {
                    Ok(statuses) if statuses.contains(&status) => {
                        received[command as usize - 1] += 1;
                    }
                    Ok(_) => dropped[Dropped::WrongStatus as usize] += 1,
                    Err(reason) => dropped[reason as usize] += 1,
                }
                assert_eq!(counts(&r), (received, dropped), "{command:?} in {status:?}");
            }
            assert!(
                accepted(command).is_err() || r.received(command) > 0,
                "{command:?} has a dispatch entry"
            );
        }
    }

    #[test]
    fn accepted_connections_beyond_the_bound_are_closed() {
        let (hub, pool) = (Hub::new(), MessagePool::new(8));
        let mut io = Memory::new(&hub, Peer(0), [0; 4], 8, 0);
        let config = Config {
            cluster: CLUSTER,
            group: GROUP,
            replica: 0,
            peers: vec![Peer(0)],
        };
        let mut r = Replica::new(&mut io, config, Noop, &pool);
        let mut others: Vec<Memory> = (1..=3)
            .map(|i| Memory::new(&hub, Peer(i), [0; 4], 8, i))
            .collect();
        for other in &mut others {
            other.connect(0, Peer(0)).unwrap();
        }
        for _ in 0..10 {
            while let Some(completion) = io.poll() {
                r.complete(&mut io, completion);
            }
            others.iter_mut().for_each(|o| while o.poll().is_some() {});
        }
        assert_eq!(r.inbound.len(), INBOUND_PER_REPLICA);
        assert!(r.inbound.iter().all(|c| c.posted));
        assert!(
            r.accepting,
            "it still accepts, and closes what is over the bound"
        );
    }

    fn snake(name: &str) -> String {
        let mut out = String::new();
        for c in name.chars() {
            if c.is_uppercase() && !out.is_empty() {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        }
        out
    }

    /// The table of document 02 section 2 lists every message the replica
    /// dispatches, and its "Accepted in" column is `accepted`.
    #[test]
    fn document_02_lists_each_message_with_the_statuses_that_accept_it() {
        let doc = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/02-replication.md"
        ))
        .unwrap();
        let section = doc.split("## 2. Messages").nth(1).unwrap();
        let section = section.split("\n## ").next().unwrap();
        let mut listed = Vec::new();
        for row in section.lines().filter(|l| l.starts_with("| ")).skip(1) {
            let cells: Vec<&str> = row.split('|').map(str::trim).collect();
            let statuses = match cells[3] {
                "all" => Ok(ALL.to_vec()),
                "none: clients receive it" => Err(Dropped::NotForReplica),
                list => Ok(list
                    .split(", ")
                    .map(|s| *ALL.iter().find(|a| snake(&format!("{a:?}")) == s).expect(s))
                    .collect()),
            };
            for name in cells[1].split(", ") {
                let command = commands()
                    .find(|c| snake(&format!("{c:?}")) == name)
                    .unwrap_or_else(|| panic!("{name} is a command"));
                assert_eq!(accepted(command).map(<[_]>::to_vec), statuses, "{name}");
                listed.push(command);
            }
        }
        // Document 04 section 4 adds the cross-shard messages.
        let rest: Vec<_> = commands().filter(|c| !listed.contains(c)).collect();
        assert_eq!(rest, [Command::Propose, Command::Vote]);
    }
}
