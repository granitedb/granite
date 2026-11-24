//! The replica frame (document 02 sections 1, 2 and 4): its state, the
//! dispatch of received messages by command with the statuses that accept
//! each, its timeouts, and its connections to the other replicas. The
//! protocol's handlers plug into the dispatch by command and into the
//! timeout handlers.
//!
//! A replica is a [`Tasks`] of the shard's event loop and reaches the
//! outside world only through the [`Io`] it is handed.
//!
//! What it does so far: the primary sends a commit heartbeat to every backup
//! on its heartbeat timeout, and a backup restarts its suspicion timeout on a
//! prepare or commit from the primary of its view. A suspicion timeout that
//! fires is counted and backs off; voting for a view change is T2.2.3.
//!
//! Connections: a replica connects to every other replica and sends only on
//! those connections, and receives only on the connections others open to
//! it, so it never has to learn who is at the other end of an accepted one.

use granite_base::constants::PROCESS_CONFIG_DEFAULT;
use granite_base::header::{Command, Error, Header, Operation, VERSION};
use granite_io::event_loop::{Class, Tasks};
use granite_io::message_pool::{Message, MessagePool};
use granite_io::{Completion, Connection, Io, Peer, Tag};
use zerocopy::{FromZeros, IntoBytes};

use crate::state_machine::StateMachine;
use crate::timeout::Timeout;

const MILLISECOND: u64 = 1_000_000;
const HEADER: usize = size_of::<Header>();
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

fn tag(kind: u64, arg: u64) -> Tag {
    assert!(arg < 1 << 48);
    kind << 48 | arg
}

/// Connection to a replica, as the sender of that link.
#[derive(Clone, Copy, Debug, Default)]
struct Link {
    connection: Option<Connection>,
    connecting: bool,
    sending: bool,
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
    state_machine: S,
    pool: &'a MessagePool,
    /// The primary's commit heartbeat.
    heartbeat: Timeout,
    /// A backup's silence from the primary.
    suspicion: Timeout,
    /// To each replica, by index; this replica's own entry stays unused.
    links: Vec<Link>,
    /// Connections other nodes opened to this one, and whether a receive is
    /// posted on each.
    inbound: Vec<(Connection, bool)>,
    accepting: bool,
    received: [u64; COMMANDS],
    dropped: [u64; Dropped::ALL.len()],
}

impl<'a, S: StateMachine> Replica<'a, S> {
    /// A replica of a new group, in status normal at view zero, with buffers
    /// from `pool`. It starts connecting to its peers at once.
    pub fn new<I: Io>(io: &mut I, config: Config, state_machine: S, pool: &'a MessagePool) -> Self {
        let count = config.peers.len();
        assert!(0 < count && count <= usize::from(u8::MAX));
        assert!(usize::from(config.replica) < count);
        // shortcut: the process configuration is the default one; take it as
        // a parameter once `granite start` reads it.
        let c = PROCESS_CONFIG_DEFAULT;
        let (group, replica) = (config.group, config.replica);
        let mut this = Replica {
            status: Status::Normal,
            view: 0,
            log_view: 0,
            op: 0,
            commit_min: 0,
            commit_max: 0,
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
            links: vec![Link::default(); count],
            inbound: Vec::with_capacity(INBOUND_PER_REPLICA * count),
            accepting: false,
            received: [0; COMMANDS],
            dropped: [0; Dropped::ALL.len()],
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

    /// Arms the timeouts of the replica's role in its view.
    fn arm(&mut self, now: u64) {
        if self.status == Status::Normal && self.primary() == self.config.replica {
            self.heartbeat.start(now);
            self.suspicion.stop();
        } else {
            self.heartbeat.stop();
            self.suspicion.start(now);
        }
    }

    /// Checks a received message and hands it to its command's handler, or
    /// drops and counts it.
    pub fn receive<I: Io>(&mut self, io: &mut I, bytes: &[u8]) {
        match self.check(bytes) {
            Ok(header) => self.dispatch(io, &header, &bytes[HEADER..]),
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
    fn dispatch<I: Io>(&mut self, io: &mut I, header: &Header, _body: &[u8]) {
        let command = header.command().expect("checked");
        self.received[command as usize] += 1;
        use Command as C;
        match command {
            // Normal operation (T2.2.2); a backup hears the primary in both.
            C::Prepare | C::Commit => self.heard(io, header),
            C::Request | C::PrepareOk => {}
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

    fn on_heartbeat<I: Io>(&mut self, io: &mut I, now: u64) {
        assert!(self.status == Status::Normal && self.primary() == self.config.replica);
        let mut header = self.header(Command::Commit);
        header.commit = self.commit_max.into();
        self.broadcast(io, &mut header);
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

    /// Seals `header` with an empty body and sends it to every other
    /// replica it is connected to.
    fn broadcast<I: Io>(&mut self, io: &mut I, header: &mut Header) {
        header.seal(&[]);
        let Some(mut message) = self.pool.acquire() else {
            return; // The protocol resends; the pool is backpressure.
        };
        message.bytes_mut().expect("only handle")[..HEADER].copy_from_slice(header.as_bytes());
        for r in 0..self.replica_count() {
            if r != self.config.replica {
                self.send(io, r, &message);
            }
        }
    }

    /// Sends the sealed message in `message` to replica `to`.
    // shortcut: one send in flight per peer, and a message sent while one is
    // in flight is dropped, as the protocol resends; add a bounded queue per
    // peer when a handler needs ordered delivery.
    fn send<I: Io>(&mut self, io: &mut I, to: u8, message: &Message) {
        let link = &mut self.links[usize::from(to)];
        let Some(connection) = link.connection else {
            return;
        };
        if link.sending {
            return;
        }
        let len = Header::decode(message.bytes()).expect("sealed").size.get();
        link.sending = io
            .send(tag(T_SEND, u64::from(to)), connection, len, message.clone())
            .is_ok();
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
            let (connection, posted) = self.inbound[i];
            if !posted && let Some(buffer) = self.pool.acquire() {
                self.post(io, i, connection, buffer);
            }
        }
    }

    fn post<I: Io>(&mut self, io: &mut I, i: usize, connection: Connection, buffer: Message) {
        let tag = tag(T_RECV, connection.0);
        self.inbound[i].1 = io.recv(tag, connection, buffer).is_ok();
    }
}

impl<I: Io, S: StateMachine> Tasks<I> for Replica<'_, S> {
    fn complete(&mut self, io: &mut I, completion: Completion) {
        match completion {
            Completion::Connected { tag, result } => {
                let link = &mut self.links[usize::try_from(tag & 0xff).expect("an index")];
                link.connecting = false;
                link.connection = result.ok();
            }
            Completion::Accepted { result, .. } => {
                self.accepting = false;
                // A failed accept is retried on the next tick, not at once,
                // so a failing network cannot spin with the clock stopped.
                let Ok(connection) = result else { return };
                if self.inbound.len() == INBOUND_PER_REPLICA * self.config.peers.len() {
                    io.close(connection); // The peer reconnects later.
                } else {
                    self.inbound.push((connection, false));
                }
                self.connect(io);
            }
            Completion::Received {
                tag,
                buffer,
                result,
            } => {
                let connection = Connection(tag & ((1 << 48) - 1));
                let Some(i) = self.inbound.iter().position(|c| c.0 == connection) else {
                    return; // Closed already.
                };
                let Ok(len) = result else {
                    self.inbound.swap_remove(i);
                    io.close(connection);
                    return;
                };
                self.inbound[i].1 = false;
                self.receive(io, &buffer.bytes()[..len as usize]);
                self.post(io, i, connection, buffer);
            }
            Completion::Sent { tag, result, .. } => {
                let link = &mut self.links[usize::try_from(tag & 0xff).expect("an index")];
                link.sending = false;
                if result.is_err()
                    && let Some(connection) = link.connection.take()
                {
                    io.close(connection); // The next tick reconnects.
                }
            }
            Completion::Read { .. }
            | Completion::Write { .. }
            | Completion::Fsync { .. }
            | Completion::Grown { .. } => {
                unreachable!("the replica submits no storage operation yet")
            }
        }
    }

    fn tick(&mut self, io: &mut I) {
        self.connect(io);
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
        let mut io = Memory::new(&hub, Peer(0), [0; 4], 8, 0);
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
        let mut io = Memory::new(&hub, Peer(0), [0; 4], 8, 0);
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
        assert!(r.inbound.iter().all(|&(_, posted)| posted));
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
