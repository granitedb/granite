//! Recovery of the write-ahead log after a crash (document 01, WAL
//! recovery). It reads the header ring once, decides the head from it, then
//! reads every prepare slot once, as far as the header ring's entry says the
//! prepare reaches or one sector, and classifies it as clean, torn, corrupt
//! or missing by the table in document 01, which follows Alagappan et al.,
//! "Protocol-Aware Recovery for Consensus-Based Storage" (FAST 2018) §3.3.3.
//!
//! The classification rests on two properties of the append (see the parent
//! module): the header ring on disk names a prefix of the appended ops, each
//! of whose prepares was durable before its header was written; and a torn
//! overwrite of a slot always shows the new prepare's header, because the
//! first sector is made durable before the rest. So a crash alone can leave
//! a slot clean, missing, or torn by an op beyond the head, and never
//! corrupt; damage to a slot the header ring names is corrupt, never torn.
//!
//! Recovery fetches nothing: it reports the slots that need repair from a
//! peer (T2.2.4) and whether the head is uncertain (`recovering_head`,
//! T2.2.5).

use super::{HEADER, SLOTS, Wal, entry};
use granite_base::constants::FORMAT;
use granite_base::header::{Command, Header};
use granite_io::message_pool::{Message, MessagePool};
use granite_io::{Completion, Full, Storage, Tag, Zone};

// The tags of recovery's reads, relative to its first tag: the slot's read,
// then the header ring's.
const HEADERS: u64 = SLOTS;
/// The number of tags recovery uses, from the first one it is given. They
/// fit in the log's ([`super::TAGS`]), which is not running yet.
pub const TAGS: u64 = SLOTS + 1;

const _: () = assert!(TAGS <= super::TAGS);
const _: () = assert!(SLOTS * FORMAT.header_size <= FORMAT.message_size_max);

/// What recovery found in a slot (document 01, WAL recovery).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// The header ring names a prepare and the slot holds it whole.
    Clean,
    /// An append of an op beyond the head that a crash interrupted. It was
    /// never acknowledged, so it is discarded.
    Torn,
    /// The header ring names, or may name, a prepare the slot does not hold
    /// whole. The prepare may be committed: it needs repair.
    Corrupt,
    /// Nothing was ever appended to the slot.
    Missing,
}

/// A prepare to fetch from a peer: op `op`, whose header checksum is
/// `checksum` when the header ring still holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Repair {
    pub op: u64,
    pub checksum: Option<u128>,
}

/// What the header ring holds for a slot.
enum Named<'a> {
    /// Zeros: no header was ever written there.
    Nothing,
    /// A valid prepare header that belongs in the slot.
    Prepare(&'a Header),
    /// Anything else.
    Damaged,
}

/// What a prepare slot holds.
enum Held<'a> {
    /// Its header is zeros: nothing was ever written there.
    Nothing,
    /// A valid prepare that belongs in the slot, body included.
    Whole(&'a Header),
    /// Anything else, with the header when it is a valid prepare header
    /// that belongs in the slot.
    Damaged(Option<&'a Header>),
}

/// Recovery of one log, driven by completions like [`super::Wal`].
pub struct Recovery {
    /// The first of the [`TAGS`] tags recovery uses.
    tag: Tag,
    /// The header ring, once read.
    headers: Option<Message>,
    /// Reads in flight.
    reads: u32,
    /// The highest op the header ring names, or 0.
    head: u64,
    /// The next slot to read.
    next: u64,
    /// Each slot's class, once its read completed.
    slots: Vec<Option<Slot>>,
}

impl Recovery {
    /// Recovery of the log on `io`, using the tags from `tag` to
    /// `tag + TAGS`. Call [`Recovery::pump`] to start it.
    pub fn new(tag: Tag) -> Recovery {
        assert!(tag.checked_add(TAGS).is_some(), "the tags fit in a Tag");
        Recovery {
            tag,
            headers: None,
            reads: 0,
            head: 0,
            next: 0,
            slots: vec![None; SLOTS as usize],
        }
    }

    /// Whether `tag` is one of recovery's; its completions go to
    /// [`Recovery::complete`].
    pub fn owns(&self, tag: Tag) -> bool {
        tag.checked_sub(self.tag).is_some_and(|at| at < TAGS)
    }

    /// Submits the reads that buffers of `pool` and room in `io` allow: the
    /// header ring's first, then, once it is read, each slot's. Recovery
    /// returns every buffer when its read completes. [`Recovery::complete`]
    /// calls it; call it too after another component's completion, when the
    /// backend refused a read for lack of room and recovery has nothing in
    /// flight.
    pub fn pump<S: Storage>(&mut self, io: &mut S, pool: &MessagePool) {
        if self.headers.is_none() {
            if self.reads == 0 {
                let len = (SLOTS * FORMAT.header_size) as u32;
                self.read(io, pool, HEADERS, Zone::WalHeaders, 0, len);
            }
        } else {
            while self.next < SLOTS {
                let offset = self.next * FORMAT.message_size_max;
                // A prepare other than the named one is classified by its
                // header alone, so the named one's length is enough.
                let len = read_len(&self.named(self.next));
                if !self.read(io, pool, self.next, Zone::WalPrepares, offset, len) {
                    break;
                }
                self.next += 1;
            }
        }
    }

    /// Submits one read; whether it was.
    fn read<S: Storage>(
        &mut self,
        io: &mut S,
        pool: &MessagePool,
        at: u64,
        zone: Zone,
        offset: u64,
        len: u32,
    ) -> bool {
        let Some(buffer) = pool.acquire() else {
            assert!(
                self.reads > 0,
                "recovery has a free buffer or a read in flight"
            );
            return false;
        };
        if let Err(Full(_)) = io.read(self.tag + at, zone, offset, len, buffer) {
            return false;
        }
        self.reads += 1;
        true
    }

    /// Handles the completion of one of recovery's reads and submits what
    /// it unblocked.
    pub fn complete<S: Storage>(&mut self, io: &mut S, pool: &MessagePool, completion: Completion) {
        assert!(self.owns(completion.tag()), "a completion of recovery");
        let at = completion.tag() - self.tag;
        let Completion::Read {
            mut buffer, result, ..
        } = completion
        else {
            unreachable!("recovery only reads");
        };
        self.reads -= 1;
        if at == HEADERS {
            if result.is_err() {
                // An unreadable header ring names nothing for certain:
                // bytes that are neither zeros nor a valid header make every
                // entry damaged.
                let bytes = buffer.bytes_mut().expect("a read buffer is not shared");
                bytes[..SLOTS as usize * HEADER].fill(0xff);
            }
            self.headers = Some(buffer);
            self.head = (0..SLOTS)
                .filter_map(|slot| match self.named(slot) {
                    Named::Prepare(header) => Some(header.op.get()),
                    _ => None,
                })
                .max()
                .unwrap_or(0);
        } else {
            assert!(self.slots[at as usize].is_none(), "each slot is read once");
            let len = read_len(&self.named(at)) as usize;
            let bytes = result.is_ok().then(|| &buffer.bytes()[..len]);
            let held = held(at, bytes);
            let class = classify(self.head, at, self.named(at), &held);
            if class == Slot::Torn {
                // The op whose append tore: the slot's own header, or the
                // first op the slot gets after the head.
                let op = match held {
                    Held::Whole(header) | Held::Damaged(Some(header)) => header.op.get(),
                    _ => self.head + 1 + (at + SLOTS - (self.head + 1) % SLOTS) % SLOTS,
                };
                assert!(op > self.head, "slot {at} is torn below the head");
            }
            self.slots[at as usize] = Some(class);
        }
        self.pump(io, pool);
    }

    /// Whether every slot is classified.
    pub fn done(&self) -> bool {
        self.slots.iter().all(Option::is_some)
    }

    /// The recovered head: the highest op the header ring names, or 0 when
    /// it names none. Every op up to it was appended; its prepare may need
    /// repair.
    pub fn head(&self) -> u64 {
        assert!(self.headers.is_some(), "the header ring is read");
        self.head
    }

    /// The header checksum of the head, when the header ring names one.
    pub fn head_checksum(&self) -> Option<u128> {
        match self.named(self.head % SLOTS) {
            Named::Prepare(header) if header.op.get() == self.head => Some(header.checksum.get()),
            _ => None,
        }
    }

    /// Whether the head is uncertain: the header ring's entry for the op
    /// after the head is damaged, so that op may have been appended.
    pub fn uncertain(&self) -> bool {
        matches!(self.named((self.head + 1) % SLOTS), Named::Damaged)
    }

    /// The class of `slot`.
    pub fn slot(&self, slot: u64) -> Slot {
        self.slots[slot as usize].expect("the slot is classified")
    }

    /// The prepares that corrupt slots should hold, for repair from peers.
    pub fn repairs(&self) -> impl Iterator<Item = Repair> + '_ {
        assert!(self.done(), "recovery is done");
        (0..SLOTS).filter_map(|slot| {
            let op = expected(self.head, slot)?;
            (self.slot(slot) == Slot::Corrupt).then(|| Repair {
                op,
                checksum: match self.named(slot) {
                    Named::Prepare(header) if header.op.get() == op => Some(header.checksum.get()),
                    _ => None,
                },
            })
        })
    }

    /// The log as recovery found it, to append after the head; the
    /// arguments are those of [`Wal::new`], and `root` is the head's
    /// checksum when the header ring names no op. Its in-memory header ring
    /// holds what the header ring holds for each slot's op up to the head,
    /// so its header writes keep naming those ops; a read of a corrupt or
    /// torn one fails with a typed error. With an uncertain head, appending
    /// waits until the replica learns the head (T2.2.5).
    pub fn wal(&self, tag: Tag, header_buffer: Message, root: u128) -> Wal {
        assert!(self.done(), "recovery is done");
        let checksum = self.head_checksum().unwrap_or(root);
        let mut wal = Wal::new(tag, header_buffer, self.head, checksum);
        let ring = self.headers.as_ref().expect("the header ring is read");
        for slot in 0..SLOTS {
            if let Named::Prepare(header) = self.named(slot)
                && Some(header.op.get()) == expected(self.head, slot)
            {
                wal.ring[entry(slot)].copy_from_slice(&ring.bytes()[entry(slot)]);
            }
        }
        wal
    }

    /// What the header ring holds for `slot`.
    fn named(&self, slot: u64) -> Named<'_> {
        let ring = self.headers.as_ref().expect("the header ring is read");
        let bytes = &ring.bytes()[entry(slot)];
        if bytes.iter().all(|&b| b == 0) {
            return Named::Nothing;
        }
        match Header::decode(bytes) {
            Ok(header) if belongs(header, slot) => Named::Prepare(header),
            _ => Named::Damaged,
        }
    }
}

/// How much of a slot recovery reads: as far as the prepare the header ring
/// names reaches, or one sector. A prepare other than the named one is
/// classified by its header alone.
fn read_len(named: &Named) -> u32 {
    match named {
        Named::Prepare(header) => header.size.get(),
        _ => 1,
    }
    .next_multiple_of(FORMAT.sector_size as u32)
}

/// Whether `header` is a prepare's that belongs in `slot`.
fn belongs(header: &Header, slot: u64) -> bool {
    let op = header.op.get();
    header.command() == Some(Command::Prepare) && op >= 1 && op % SLOTS == slot
}

/// What slot `slot` holds, given the bytes read, or `None` if they were
/// unreadable. A prepare that reaches beyond them counts as damaged.
fn held(slot: u64, bytes: Option<&[u8]>) -> Held<'_> {
    let Some(bytes) = bytes else {
        return Held::Damaged(None);
    };
    if bytes[..HEADER].iter().all(|&b| b == 0) {
        return Held::Nothing;
    }
    match Header::decode(bytes) {
        Ok(header) if belongs(header, slot) => {
            let body = bytes.get(HEADER..header.size.get() as usize);
            match body.map(|body| header.check_body(body)) {
                Some(Ok(())) => Held::Whole(header),
                _ => Held::Damaged(Some(header)),
            }
        }
        _ => Held::Damaged(None),
    }
}

/// The op that `slot` holds when the head is `head`: the highest op up to
/// the head that maps to it, if there is one (op 0 is the root, never
/// appended).
fn expected(head: u64, slot: u64) -> Option<u64> {
    head.checked_sub((head % SLOTS + SLOTS - slot) % SLOTS)
        .filter(|&op| op >= 1)
}

/// The classification table of document 01 (WAL recovery); the row numbers
/// are the table's.
fn classify(head: u64, slot: u64, named: Named, held: &Held) -> Slot {
    let expected = expected(head, slot);
    match named {
        // Rows 1 and 2: no op up to the head maps here and no header was
        // ever written. Without its header, an append never completed.
        Named::Nothing if expected.is_none() => match held {
            Held::Nothing => Slot::Missing,
            _ => Slot::Torn,
        },
        // Row 3: the header of an op up to the head is lost.
        Named::Nothing => Slot::Corrupt,
        // Row 4: the header is damaged.
        Named::Damaged => Slot::Corrupt,
        // Row 5: the header is older than the op the slot holds.
        Named::Prepare(header) if Some(header.op.get()) != expected => Slot::Corrupt,
        Named::Prepare(header) => match *held {
            // Row 6.
            Held::Whole(prepare) if prepare.checksum == header.checksum => Slot::Clean,
            // Row 7: an overwrite by an op beyond the head did not finish.
            Held::Whole(prepare) | Held::Damaged(Some(prepare)) if prepare.op.get() > head => {
                Slot::Torn
            }
            // Rows 8 to 11: the named prepare is damaged or not there.
            _ => Slot::Corrupt,
        },
    }
}
