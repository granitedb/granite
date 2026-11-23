//! The write-ahead log: a ring of prepares and a ring of their headers
//! (document 01 section 2, WAL). This is the write path and the read by op;
//! [`recovery`] classifies the slots after a crash; repair from peers
//! (T2.2.4) builds on it.
//!
//! An append goes through these I/Os, each submitted only after the previous
//! one completed: a prepare of more than one sector first has its first
//! sector written and made durable with an fsync; the whole prepare is then
//! written to its slot of the prepare ring and made durable with an fsync;
//! its header is then copied into the in-memory header ring, and the header
//! ring's sector that holds it is written and made durable with an fsync.
//! Only then is the append reported done. So the header ring on disk never
//! names a prepare that is not durable, and a crash leaves either the old
//! header (the prepare slot may be torn) or the new header over a whole
//! prepare. Writing the first sector first means a torn overwrite always
//! shows the new prepare's header, so recovery can tell it from a damaged
//! old prepare (document 01, WAL recovery).
//!
//! A sector of the header ring holds 16 headers. Headers enter the in-memory
//! ring in op order, once every earlier op's prepare is durable. One header
//! write is in flight at a time; it carries every header of its sector in
//! the ring, so neighbouring appends share it, and the sector of the oldest
//! header not yet written goes first. So the header ring on disk always
//! names a prefix of the appended ops, and appends complete in op order.
//!
//! The prepare is written from the caller's buffer, without a copy. Every
//! table is sized when the log is built; an append or a read allocates
//! nothing. At most `FORMAT.pipeline_depth` appends are in flight; beyond
//! that, and while a read of the slot is in flight, an append is refused
//! with [`Full`], which is how backpressure reaches the caller.

use granite_base::constants::FORMAT;
use granite_base::header::{self, Command, Header};
use granite_io::message_pool::Message;
use granite_io::{Completion, Full, Storage, StorageError, Tag, Zone};
use std::ops::Range;

pub mod recovery;

pub(crate) const SLOTS: u64 = FORMAT.wal_slot_count;
pub(crate) const HEADER: usize = FORMAT.header_size as usize;
const SECTOR: u64 = FORMAT.sector_size;
/// Headers in one sector of the header ring.
const PER_SECTOR: u64 = SECTOR / FORMAT.header_size;

// The tags of the log's operations, relative to its first tag.
const WRITE: u64 = 0; // plus the slot
const SYNC: u64 = SLOTS; // plus the slot
const READ: u64 = 2 * SLOTS; // plus the slot
const HEADER_WRITE: u64 = 3 * SLOTS;
const HEADER_SYNC: u64 = 3 * SLOTS + 1;
/// The number of tags a log uses, from the first one it is given.
pub const TAGS: u64 = 3 * SLOTS + 2;

const _: () =
    assert!(SECTOR.is_multiple_of(FORMAT.header_size) && SLOTS.is_multiple_of(PER_SECTOR));
const _: () = assert!(FORMAT.pipeline_depth < SLOTS);

/// Where the append in flight in a slot is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// No append in flight.
    Idle,
    /// The write of the first sector of a prepare of more than one sector
    /// is in flight.
    Leading,
    /// The first sector is written; its fsync is still to be submitted.
    Led,
    /// The first sector's fsync is in flight.
    LeadSyncing,
    /// The first sector is durable; the whole prepare's write is still to
    /// be submitted.
    Lead,
    /// The prepare write is in flight.
    Writing,
    /// The prepare write completed; its fsync is still to be submitted.
    Written,
    /// The prepare's fsync is in flight.
    Syncing,
    /// The prepare is durable; its header enters the ring once every earlier
    /// op's has.
    Synced,
    /// The prepare is durable and its header is in the ring; the header
    /// write is still to be submitted.
    Durable,
    /// The header is in the header write in flight, or in its fsync.
    HeaderWriting,
}

/// Where the one header write is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeaderStep {
    Idle,
    Writing,
    /// Written; its fsync is still to be submitted.
    Written,
    Syncing,
}

/// What a completion led to.
#[derive(Debug)]
pub enum Event {
    /// The prepare and its header are durable.
    Appended { op: u64 },
    /// A read by op finished. On success the buffer holds the prepare,
    /// header and body, both checked; on failure the bytes are not returned.
    Read {
        op: u64,
        result: Result<Message, ReadError>,
    },
}

/// Why a read found no valid prepare. The slot needs repair from a peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// The device failed the read.
    Io(StorageError),
    /// The header in the prepare slot is not valid.
    Header(header::Error),
    /// The header in the prepare slot is valid but is not the one the log
    /// holds for the op: another prepare is there.
    Mismatch,
    /// The body does not match the header.
    Body(header::Error),
}

/// Why a read was not submitted.
#[derive(Debug)]
pub enum Refused {
    /// The log does not hold the op: it was never appended, it was
    /// overwritten by a later op in the same slot, or that overwrite is in
    /// flight.
    NotPresent,
    /// The backend is full; retry after a completion.
    Full(Message),
}

/// The write-ahead log of one replica.
pub struct Wal {
    /// The first of the [`TAGS`] tags the log uses.
    tag: Tag,
    /// The header ring as it is on disk once the header write in flight
    /// lands: in each slot, the header of the last prepare made durable
    /// there, or zeros.
    ring: Vec<u8>,
    /// The header of the append in flight in each slot.
    pending: Vec<u8>,
    steps: Vec<Step>,
    /// The prepare of each slot between its first-sector write and its
    /// whole write.
    held: Vec<Option<Message>>,
    /// Reads in flight per slot.
    reads: Vec<u32>,
    /// The last op appended, and its header checksum: the next prepare's
    /// parent.
    head: u64,
    head_checksum: u128,
    /// The lowest op whose append is in flight, or `head + 1`.
    oldest: u64,
    /// The last op whose header is in the ring.
    ringed: u64,
    /// The sector-sized buffer of the header write, while it is not in
    /// flight.
    header_buffer: Option<Message>,
    header_step: HeaderStep,
}

impl Wal {
    /// An empty log whose last op is `head`, with header checksum
    /// `head_checksum`; the next append is op `head + 1`. It uses the tags
    /// from `tag` to `tag + TAGS`, and `header_buffer`, a buffer of the
    /// shard's pool that it keeps.
    pub fn new(tag: Tag, header_buffer: Message, head: u64, head_checksum: u128) -> Wal {
        assert!(tag.checked_add(TAGS).is_some(), "the tags fit in a Tag");
        assert!(head < u64::MAX, "there is a next op");
        assert_eq!(
            header_buffer.references(),
            1,
            "the header buffer is not shared"
        );
        let slots = SLOTS as usize;
        Wal {
            tag,
            ring: vec![0; slots * HEADER],
            pending: vec![0; slots * HEADER],
            steps: vec![Step::Idle; slots],
            held: (0..slots).map(|_| None).collect(),
            reads: vec![0; slots],
            head,
            head_checksum,
            oldest: head + 1,
            ringed: head,
            header_buffer: Some(header_buffer),
            header_step: HeaderStep::Idle,
        }
    }

    /// Whether `tag` is one of the log's; its completions go to
    /// [`Wal::complete`].
    pub fn owns(&self, tag: Tag) -> bool {
        tag.checked_sub(self.tag).is_some_and(|at| at < TAGS)
    }

    /// Appends the prepare in `prepare`: a valid header whose command is
    /// prepare, whose op follows the last one and whose parent is the last
    /// header's checksum, then its body, then zeros to the end of the
    /// sector. Anything else is a bug in the caller and panics. Its
    /// completion is [`Event::Appended`]. The buffer may be shared, for
    /// example with the sends to the backups; the log only reads it.
    pub fn append<S: Storage>(
        &mut self,
        io: &mut S,
        prepare: Message,
    ) -> Result<(), Full<Message>> {
        let bytes = prepare.bytes();
        let header = Header::decode(bytes).expect("a valid prepare header");
        assert_eq!(header.command(), Some(Command::Prepare), "a prepare");
        let op = header.op.get();
        assert_eq!(op, self.head + 1, "ops are appended in order");
        assert_eq!(
            header.parent.get(),
            self.head_checksum,
            "the prepare's parent is the previous header's checksum"
        );
        let size = header.size.get() as usize;
        assert_eq!(
            header.check_body(&bytes[HEADER..size]),
            Ok(()),
            "the body matches"
        );
        let len = size.next_multiple_of(SECTOR as usize);
        assert!(
            bytes[size..len].iter().all(|&b| b == 0),
            "zeros to the end of the sector"
        );
        let checksum = header.checksum.get();

        let slot = op % SLOTS;
        if op - self.oldest >= FORMAT.pipeline_depth || self.reads[slot as usize] > 0 {
            return Err(Full(prepare));
        }
        assert_eq!(self.steps[slot as usize], Step::Idle, "one append per slot");
        self.pending[entry(slot)].copy_from_slice(&bytes[..HEADER]);
        let len = u32::try_from(len).expect("a prepare fits in u32");
        let lead = u64::from(len) > SECTOR;
        io.write(
            self.tag + WRITE + slot,
            Zone::WalPrepares,
            slot * FORMAT.message_size_max,
            if lead { SECTOR as u32 } else { len },
            prepare,
        )?;
        self.steps[slot as usize] = if lead { Step::Leading } else { Step::Writing };
        self.head = op;
        self.head_checksum = checksum;
        Ok(())
    }

    /// Reads op `op` into `buffer`, which must not be shared. Its completion
    /// is [`Event::Read`].
    pub fn read<S: Storage>(
        &mut self,
        io: &mut S,
        op: u64,
        buffer: Message,
    ) -> Result<(), Refused> {
        let slot = op % SLOTS;
        // Until its header is in the ring, the ring names the old op, whose
        // prepare may already be gone.
        let overwriting = !matches!(
            self.steps[slot as usize],
            Step::Idle | Step::Durable | Step::HeaderWriting
        );
        let size = match self.header(slot) {
            Some(header) if header.op.get() == op && !overwriting => header.size.get(),
            _ => return Err(Refused::NotPresent),
        };
        let len = size.next_multiple_of(SECTOR as u32);
        io.read(
            self.tag + READ + slot,
            Zone::WalPrepares,
            slot * FORMAT.message_size_max,
            len,
            buffer,
        )
        .map_err(|Full(buffer)| Refused::Full(buffer))?;
        self.reads[slot as usize] += 1;
        Ok(())
    }

    /// Handles the completion of one of the log's operations, passing what
    /// it led to to `event`, and submits what it unblocked. A failed write or
    /// fsync leaves the data's state unknown, so it stops the process
    /// (decision 0003); replication absorbs the lost replica.
    pub fn complete<S: Storage>(
        &mut self,
        io: &mut S,
        completion: Completion,
        mut event: impl FnMut(Event),
    ) {
        assert!(self.owns(completion.tag()), "a completion of the log");
        let at = completion.tag() - self.tag;
        match completion {
            Completion::Write { buffer, result, .. } => {
                result.expect("a WAL write failed");
                if at == HEADER_WRITE {
                    assert_eq!(self.header_step, HeaderStep::Writing);
                    self.header_buffer = Some(buffer);
                    self.header_step = HeaderStep::Written;
                } else {
                    let slot = (at - WRITE) as usize;
                    self.steps[slot] = match self.steps[slot] {
                        Step::Leading => {
                            self.held[slot] = Some(buffer);
                            Step::Led
                        }
                        step => {
                            assert_eq!(step, Step::Writing);
                            Step::Written
                        }
                    };
                }
            }
            Completion::Fsync { result, .. } if at == HEADER_SYNC => {
                result.expect("a WAL fsync failed");
                assert_eq!(self.header_step, HeaderStep::Syncing);
                self.header_step = HeaderStep::Idle;
                for op in self.oldest..=self.head {
                    let slot = (op % SLOTS) as usize;
                    if self.steps[slot] == Step::HeaderWriting {
                        self.steps[slot] = Step::Idle;
                        event(Event::Appended { op });
                    }
                }
                while self.oldest <= self.head
                    && self.steps[(self.oldest % SLOTS) as usize] == Step::Idle
                {
                    self.oldest += 1;
                }
            }
            Completion::Fsync { result, .. } => {
                result.expect("a WAL fsync failed");
                let slot = (at - SYNC) as usize;
                self.steps[slot] = match self.steps[slot] {
                    Step::LeadSyncing => Step::Lead,
                    step => {
                        assert_eq!(step, Step::Syncing);
                        Step::Synced
                    }
                };
                while self.ringed < self.head {
                    let slot = (self.ringed + 1) % SLOTS;
                    if self.steps[slot as usize] != Step::Synced {
                        break;
                    }
                    let range = entry(slot);
                    self.ring[range.clone()].copy_from_slice(&self.pending[range]);
                    self.steps[slot as usize] = Step::Durable;
                    self.ringed += 1;
                }
            }
            Completion::Read { buffer, result, .. } => {
                let slot = at - READ;
                self.reads[slot as usize] -= 1;
                let expected = *self.header(slot).expect("a read slot keeps its header");
                let result = result
                    .map_err(ReadError::Io)
                    .and_then(|()| check(&expected, buffer));
                event(Event::Read {
                    op: expected.op.get(),
                    result,
                });
            }
            _ => unreachable!("the log submits only storage operations"),
        }
        self.pump(io);
    }

    /// Submits what is ready: fsyncs of written prepares and first sectors,
    /// whole writes of prepares whose first sector is durable, then the
    /// header write or its fsync. [`Wal::complete`] calls it; call it too after
    /// another component's completion, when the backend refused one of these
    /// for lack of room and the log has nothing else in flight.
    pub fn pump<S: Storage>(&mut self, io: &mut S) {
        for op in self.oldest..=self.head {
            let slot = op % SLOTS;
            let at = slot as usize;
            match self.steps[at] {
                Step::Written | Step::Led => {
                    if io.fsync(self.tag + SYNC + slot).is_err() {
                        return;
                    }
                    self.steps[at] = if self.steps[at] == Step::Led {
                        Step::LeadSyncing
                    } else {
                        Step::Syncing
                    };
                }
                Step::Lead => {
                    let prepare = self.held[at].take().expect("a held prepare");
                    let size = Header::decode(prepare.bytes()).expect("checked").size.get();
                    let len = size.next_multiple_of(SECTOR as u32);
                    let offset = slot * FORMAT.message_size_max;
                    let tag = self.tag + WRITE + slot;
                    if let Err(Full(prepare)) =
                        io.write(tag, Zone::WalPrepares, offset, len, prepare)
                    {
                        self.held[at] = Some(prepare);
                        return;
                    }
                    self.steps[at] = Step::Writing;
                }
                _ => {}
            }
        }
        match self.header_step {
            HeaderStep::Idle => self.write_headers(io),
            HeaderStep::Written => {
                if io.fsync(self.tag + HEADER_SYNC).is_ok() {
                    self.header_step = HeaderStep::Syncing;
                }
            }
            HeaderStep::Writing | HeaderStep::Syncing => {}
        }
    }

    /// Writes the sector of the header ring that holds the oldest durable
    /// prepare's header, with every durable header of that sector.
    fn write_headers<S: Storage>(&mut self, io: &mut S) {
        let Some(op) =
            (self.oldest..=self.head).find(|op| self.steps[(op % SLOTS) as usize] == Step::Durable)
        else {
            return;
        };
        let first = op % SLOTS / PER_SECTOR * PER_SECTOR;
        let slots = first..first + PER_SECTOR;
        let mut buffer = self
            .header_buffer
            .take()
            .expect("no header write in flight");
        let bytes = buffer.bytes_mut().expect("the header buffer is not shared");
        bytes[..SECTOR as usize]
            .copy_from_slice(&self.ring[entry(first).start..entry(slots.end - 1).end]);
        let offset = first * FORMAT.header_size;
        match io.write(
            self.tag + HEADER_WRITE,
            Zone::WalHeaders,
            offset,
            SECTOR as u32,
            buffer,
        ) {
            Err(Full(buffer)) => self.header_buffer = Some(buffer),
            Ok(()) => {
                for slot in slots {
                    if self.steps[slot as usize] == Step::Durable {
                        self.steps[slot as usize] = Step::HeaderWriting;
                    }
                }
                self.header_step = HeaderStep::Writing;
            }
        }
    }

    /// The header the ring holds for `slot`, if any.
    fn header(&self, slot: u64) -> Option<&Header> {
        Header::decode(&self.ring[entry(slot)]).ok()
    }
}

/// The bytes of `slot`'s entry in a header table.
fn entry(slot: u64) -> Range<usize> {
    let start = slot as usize * HEADER;
    start..start + HEADER
}

/// `buffer`, if it holds the prepare whose header is `expected`.
fn check(expected: &Header, buffer: Message) -> Result<Message, ReadError> {
    let bytes = buffer.bytes();
    let header = Header::decode(bytes).map_err(ReadError::Header)?;
    if header.checksum != expected.checksum {
        return Err(ReadError::Mismatch);
    }
    header
        .check_body(&bytes[HEADER..header.size.get() as usize])
        .map_err(ReadError::Body)?;
    Ok(buffer)
}
