//! The superblock (document 01 section 2): the root record of a data file,
//! kept as four copies so that an update is atomic across a crash and open
//! survives the corruption of some copies. The layout of a copy is the table
//! in document 01 section 2.
//!
//! An update writes one copy at a time, each made durable before the next
//! is written (the quorum rule). A crash then leaves the old record or the
//! new one on at least [`QUORUM`] copies, and open chooses the record with the
//! highest sequence number that a quorum of copies holds, then rewrites the
//! copies that do not hold it.
//!
//! An update and the rewrites of an open finish only once all four copies,
//! read back, hold the record at their own offsets; a copy that does not is
//! written again. A write that landed elsewhere then cannot leave fewer than
//! four good copies behind a finished update or open, so a crash in the next
//! update still leaves a quorum.

use granite_base::checksum::checksum;
use granite_base::constants::{FORMAT, FormatMismatch};
use granite_io::message_pool::{MESSAGE_SIZE, Message};
use granite_io::{Completion, Full, Storage, Tag, Zone};

/// The format version this binary writes. A file with any other is refused.
pub const VERSION: u32 = 2;

/// Copies of the record in the superblock zone.
pub const COPIES: usize = FORMAT.superblock_copies as usize;

/// Copies that must hold the same record for open to choose it: a crash
/// during an update leaves the old or the new record on half of them.
pub const QUORUM: usize = COPIES / 2;

const COPY: usize = FORMAT.superblock_copy_size as usize;
const HEADER: usize = FORMAT.header_size as usize;

// Offsets in a copy.
const COPY_INDEX: usize = 41;
const CONSTANTS: usize = 48;
const SEQUENCE: usize = CONSTANTS + FORMAT.entries().len() * 8;
const VSR: usize = SEQUENCE + 8;
const REFERENCES: usize = VSR + 24;
const HEADER_COUNT: usize = REFERENCES + 3 * 24;
const HEADERS: usize = HEADER_COUNT + 8;

/// Reads of all four copies in one open or update that may find copies to
/// rewrite; the next such read stops the replica, since a device that keeps
/// losing or misdirecting writes cannot hold the superblock.
pub const READ_BACKS_MAX: u32 = 4;

/// Most headers a record holds: as many as fit in a copy.
pub const HEADERS_MAX: usize = (COPY - HEADERS) / HEADER;

// The buffer holds a staging copy and every copy as read; a copy index fits a byte.
const _: () = assert!((COPIES + 1) * COPY <= MESSAGE_SIZE && COPIES <= u8::MAX as usize + 1);
const _: () = assert!(QUORUM > 0 && HEADERS_MAX > 0);

/// Which replica of which group of which cluster a data file belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub cluster: u128,
    pub group: u32,
    pub replica: u8,
}

/// A grid block: its address and the checksum it must have.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reference {
    pub address: u64,
    pub checksum: u128,
}

/// The VSR state a replica keeps across a restart (document 02 section 4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VsrState {
    pub view: u32,
    pub log_view: u32,
    pub commit_op: u64,
    pub checkpoint_op: u64,
    /// The headers of the log installed with `log_view`, at most
    /// [`HEADERS_MAX`].
    pub headers: Vec<[u8; HEADER]>,
}

/// Where the state of the last checkpoint lies in the grid (document 01
/// section 7). What a checkpoint writes there is T1.2.6's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Checkpoint {
    pub manifest: Reference,
    pub free_set: Reference,
    pub client_sessions: Reference,
}

/// The root record. Every copy holds it; the format version and the format
/// constants are this binary's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub identity: Identity,
    /// One more than the record it replaced; zero as formatted.
    pub sequence: u64,
    pub vsr: VsrState,
    pub checkpoint: Checkpoint,
}

impl Record {
    /// The record `granite format` writes.
    pub fn new(identity: Identity) -> Self {
        Record {
            identity,
            sequence: 0,
            vsr: VsrState::default(),
            checkpoint: Checkpoint::default(),
        }
    }
}

/// Why a data file was refused at open. Each reason is its own variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// No record is held by a quorum of copies, each with a valid checksum
    /// and at its own offset; or two records with one sequence number are.
    NoQuorum,
    /// The file was written in another format version.
    Version { stored: u32 },
    /// A format constant differs from this binary's.
    Format(FormatMismatch),
    /// The record holds more headers than a copy has room for.
    Headers { stored: u32 },
    /// A byte the layout keeps zero is not.
    Reserved,
    /// The file belongs to another cluster.
    Cluster { stored: u128 },
    /// The file belongs to another group.
    Group { stored: u32 },
    /// The file belongs to another replica.
    Replica { stored: u8 },
}

/// What open found in the superblock zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub record: Record,
    /// The copies that do not hold the record, which open rewrites.
    pub stale: [bool; COPIES],
}

/// The superblock zone of a newly formatted data file: every copy holds
/// [`Record::new`].
pub fn format(identity: Identity) -> Vec<u8> {
    let mut zone = vec![0; COPY * COPIES];
    for (index, copy) in zone.chunks_exact_mut(COPY).enumerate() {
        encode(&Record::new(identity), copy);
        seal(copy, index);
    }
    zone
}

/// Chooses the record of the superblock zone `zone` of a data file opened
/// as `expected`, checking in the order of document 01 section 2.
pub fn open(zone: &[u8], expected: Identity) -> Result<Opened, OpenError> {
    assert_eq!(zone.len(), COPY * COPIES);
    let copy = |i: usize| &zone[i * COPY..][..COPY];
    let sealed: [bool; COPIES] =
        std::array::from_fn(|i| checksum(&copy(i)[16..]).to_le_bytes() == copy(i)[..16]);
    // The version comes first: another version may lay out the rest differently.
    for i in (0..COPIES).filter(|&i| sealed[i]) {
        let version = u32::from_le_bytes(at(copy(i), 32));
        if version != VERSION {
            return Err(OpenError::Version { stored: version });
        }
    }
    // A copy written at another copy's offset holds that copy's index.
    let valid: [bool; COPIES] =
        std::array::from_fn(|i| sealed[i] && usize::from(copy(i)[COPY_INDEX]) == i);
    let same = |i: usize, j: usize| {
        let (a, b) = (copy(i), copy(j));
        valid[i]
            && valid[j]
            && a[16..COPY_INDEX] == b[16..COPY_INDEX]
            && a[COPY_INDEX + 1..] == b[COPY_INDEX + 1..]
    };
    let quorum = |i: usize| (0..COPIES).filter(|&j| same(i, j)).count() >= QUORUM;
    let sequence = |i: usize| u64::from_le_bytes(at(copy(i), SEQUENCE));
    let chosen = (0..COPIES)
        .filter(|&i| quorum(i))
        .max_by_key(|&i| sequence(i))
        .ok_or(OpenError::NoQuorum)?;
    if (0..COPIES).any(|i| quorum(i) && sequence(i) == sequence(chosen) && !same(i, chosen)) {
        return Err(OpenError::NoQuorum);
    }
    let copy = copy(chosen);
    for (i, (constant, binary)) in FORMAT.entries().into_iter().enumerate() {
        let stored = u64::from_le_bytes(at(copy, CONSTANTS + i * 8));
        if stored != binary {
            return Err(OpenError::Format(FormatMismatch {
                constant,
                stored,
                binary,
            }));
        }
    }
    let count = u32::from_le_bytes(at(copy, HEADER_COUNT));
    let headers_end = usize::try_from(count)
        .ok()
        .filter(|&n| n <= HEADERS_MAX)
        .ok_or(OpenError::Headers { stored: count })?
        * HEADER
        + HEADERS;
    if copy[COPY_INDEX + 1..CONSTANTS]
        .iter()
        .chain(&copy[HEADER_COUNT + 4..HEADERS])
        .chain(&copy[headers_end..])
        .any(|&b| b != 0)
    {
        return Err(OpenError::Reserved);
    }
    let record = decode(copy);
    let stored = record.identity;
    if stored.cluster != expected.cluster {
        return Err(OpenError::Cluster {
            stored: stored.cluster,
        });
    }
    if stored.group != expected.group {
        return Err(OpenError::Group {
            stored: stored.group,
        });
    }
    if stored.replica != expected.replica {
        return Err(OpenError::Replica {
            stored: stored.replica,
        });
    }
    Ok(Opened {
        record,
        stale: std::array::from_fn(|i| !same(i, chosen)),
    })
}

/// `N` bytes of `copy` from `offset`.
fn at<const N: usize>(copy: &[u8], offset: usize) -> [u8; N] {
    copy[offset..offset + N]
        .try_into()
        .expect("inside the copy")
}

/// Writes `record` into `copy`, all but the checksum and the copy index.
fn encode(record: &Record, copy: &mut [u8]) {
    let (vsr, c) = (&record.vsr, &record.checkpoint);
    assert!(
        vsr.headers.len() <= HEADERS_MAX,
        "the headers fit in a copy"
    );
    copy.fill(0);
    let mut put = |offset: usize, bytes: &[u8]| {
        copy[offset..offset + bytes.len()].copy_from_slice(bytes);
    };
    put(16, &record.identity.cluster.to_le_bytes());
    put(32, &VERSION.to_le_bytes());
    put(36, &record.identity.group.to_le_bytes());
    put(40, &[record.identity.replica]);
    for (i, (_, value)) in FORMAT.entries().into_iter().enumerate() {
        put(CONSTANTS + i * 8, &value.to_le_bytes());
    }
    put(SEQUENCE, &record.sequence.to_le_bytes());
    put(VSR, &vsr.view.to_le_bytes());
    put(VSR + 4, &vsr.log_view.to_le_bytes());
    put(VSR + 8, &vsr.commit_op.to_le_bytes());
    put(VSR + 16, &vsr.checkpoint_op.to_le_bytes());
    for (i, r) in [c.manifest, c.free_set, c.client_sessions]
        .into_iter()
        .enumerate()
    {
        put(REFERENCES + i * 24, &r.address.to_le_bytes());
        put(REFERENCES + i * 24 + 8, &r.checksum.to_le_bytes());
    }
    let count = u32::try_from(vsr.headers.len()).expect("at most HEADERS_MAX");
    put(HEADER_COUNT, &count.to_le_bytes());
    for (i, header) in vsr.headers.iter().enumerate() {
        put(HEADERS + i * HEADER, header);
    }
}

/// Sets the copy index of `copy` and its checksum.
fn seal(copy: &mut [u8], index: usize) {
    copy[COPY_INDEX] = u8::try_from(index).expect("a copy index fits a byte");
    let sum = checksum(&copy[16..]);
    copy[..16].copy_from_slice(&sum.to_le_bytes());
}

/// The record of a copy that open has checked.
fn decode(copy: &[u8]) -> Record {
    let u32_at = |offset| u32::from_le_bytes(at(copy, offset));
    let u64_at = |offset| u64::from_le_bytes(at(copy, offset));
    let reference = |i: usize| Reference {
        address: u64_at(REFERENCES + i * 24),
        checksum: u128::from_le_bytes(at(copy, REFERENCES + i * 24 + 8)),
    };
    let count = usize::try_from(u32_at(HEADER_COUNT)).expect("checked by open");
    Record {
        identity: Identity {
            cluster: u128::from_le_bytes(at(copy, 16)),
            group: u32_at(36),
            replica: copy[40],
        },
        sequence: u64_at(SEQUENCE),
        vsr: VsrState {
            view: u32_at(VSR),
            log_view: u32_at(VSR + 4),
            commit_op: u64_at(VSR + 8),
            checkpoint_op: u64_at(VSR + 16),
            headers: copy[HEADERS..][..count * HEADER]
                .chunks_exact(HEADER)
                .map(|header| header.try_into().expect("one header"))
                .collect(),
        },
        checkpoint: Checkpoint {
            manifest: reference(0),
            free_set: reference(1),
            client_sessions: reference(2),
        },
    }
}

/// The operation a [`Superblock`] has in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Idle,
    Read(usize),
    Write(usize),
    Fsync(usize),
}

/// Opens and updates the superblock of one data file through its
/// [`Storage`], one operation at a time. The caller hands every completion
/// with the superblock's tag to [`Superblock::complete`].
pub struct Superblock {
    identity: Identity,
    tag: Tag,
    /// Bytes `0..COPY` stage the copy being read or written; copy `i` as
    /// read lies at `(i + 1) * COPY`. `None` while the storage holds it.
    buffer: Option<Message>,
    /// The record on disk, once an open has finished.
    record: Option<Record>,
    /// The record the open or update in progress is writing.
    staging: Option<Record>,
    step: Step,
    /// Copies the open or update in progress has still to write.
    pending: [bool; COPIES],
    /// The storage refused the current step.
    refused: bool,
    /// Reads of all copies in this open or update.
    read_backs: u32,
}

impl Superblock {
    /// The superblock of the data file of `identity`, submitting with `tag`
    /// and staging its copies in `buffer`.
    pub fn new(identity: Identity, tag: Tag, buffer: Message) -> Self {
        assert_eq!(buffer.references(), 1, "the buffer is not shared");
        Superblock {
            identity,
            tag,
            buffer: Some(buffer),
            record: None,
            staging: None,
            step: Step::Idle,
            pending: [false; COPIES],
            refused: false,
            read_backs: 0,
        }
    }

    /// The record on disk, once an open has finished.
    pub fn record(&self) -> Option<&Record> {
        self.record.as_ref()
    }

    /// Starts reading every copy. When they are read, open chooses the
    /// record and rewrites the stale copies until every copy, read back,
    /// holds it; if it fails, it writes nothing.
    pub fn open(&mut self, storage: &mut impl Storage) {
        assert!(self.step == Step::Idle && self.record.is_none());
        self.read_backs = 0;
        self.step = Step::Read(0);
        self.submit(storage);
    }

    /// Starts writing `record` to every copy, the atomic commit point of a
    /// checkpoint, and rewrites copies until every copy, read back, holds it.
    /// Its sequence number is one more than the record's on disk.
    pub fn update(&mut self, storage: &mut impl Storage, record: Record) {
        assert_eq!(self.step, Step::Idle);
        let current = self.record.as_ref().expect("the superblock is open");
        assert_eq!(
            Some(record.sequence),
            current.sequence.checked_add(1),
            "an update follows the record on disk"
        );
        assert_eq!(record.identity, self.identity);
        encode(&record, &mut self.bytes()[..COPY]);
        self.staging = Some(record);
        self.pending = [true; COPIES];
        self.read_backs = 0;
        assert!(
            self.write_next(storage).is_none(),
            "an update writes every copy"
        );
    }

    /// Takes the completion of the operation in flight. Returns the outcome
    /// once the open or update has finished; an update always succeeds.
    pub fn complete(
        &mut self,
        storage: &mut impl Storage,
        completion: Completion,
    ) -> Option<Result<(), OpenError>> {
        assert_eq!(completion.tag(), self.tag);
        assert!(!self.refused, "a refused operation does not complete");
        match (self.step, completion) {
            (Step::Read(copy), Completion::Read { buffer, result, .. }) => {
                self.buffer = Some(buffer);
                let bytes = self.bytes();
                if result.is_ok() {
                    bytes.copy_within(..COPY, (copy + 1) * COPY);
                } else {
                    // Zeros fail the checksum: an unreadable copy is a corrupt one.
                    bytes[(copy + 1) * COPY..][..COPY].fill(0);
                }
                if copy + 1 < COPIES {
                    self.step = Step::Read(copy + 1);
                    self.submit(storage);
                    return None;
                }
                // Without a record staged, this is the first read of an open.
                if self.staging.is_none() {
                    let identity = self.identity;
                    match open(&self.bytes()[COPY..][..COPIES * COPY], identity) {
                        Err(error) => {
                            self.step = Step::Idle;
                            return Some(Err(error));
                        }
                        Ok(opened) => self.staging = Some(opened.record),
                    }
                }
                let record = self.staging.take().expect("a record is staged");
                let bytes = self.bytes();
                encode(&record, &mut bytes[..COPY]);
                let mut pending = [false; COPIES];
                for (copy, pending) in pending.iter_mut().enumerate() {
                    seal(&mut bytes[..COPY], copy);
                    *pending = bytes[..COPY] != bytes[(copy + 1) * COPY..][..COPY];
                }
                self.read_backs += 1;
                assert!(
                    !pending.contains(&true) || self.read_backs <= READ_BACKS_MAX,
                    "the superblock copies do not hold what was written: the replica stops"
                );
                self.staging = Some(record);
                self.pending = pending;
                self.write_next(storage)
            }
            (Step::Write(copy), Completion::Write { buffer, result, .. }) => {
                self.buffer = Some(buffer);
                result.expect("the superblock cannot be written: the replica stops");
                self.step = Step::Fsync(copy);
                self.submit(storage);
                None
            }
            (Step::Fsync(copy), Completion::Fsync { result, .. }) => {
                result.expect("the superblock cannot be made durable: the replica stops");
                self.pending[copy] = false;
                if self.pending.contains(&true) {
                    return self.write_next(storage);
                }
                // Read every copy back: a write may have landed elsewhere.
                self.step = Step::Read(0);
                self.submit(storage);
                None
            }
            (step, completion) => panic!("{completion:?} during {step:?}"),
        }
    }

    /// Submits again the operation the storage refused, if any. Call it after
    /// a completion has made room.
    pub fn resume(&mut self, storage: &mut impl Storage) {
        if self.refused {
            self.submit(storage);
        }
    }

    /// Writes the next pending copy, or finishes.
    fn write_next(&mut self, storage: &mut impl Storage) -> Option<Result<(), OpenError>> {
        let Some(copy) = self.pending.iter().position(|&p| p) else {
            self.step = Step::Idle;
            self.record = self.staging.take();
            return Some(Ok(()));
        };
        seal(&mut self.bytes()[..COPY], copy);
        self.step = Step::Write(copy);
        self.submit(storage);
        None
    }

    fn submit(&mut self, storage: &mut impl Storage) {
        let len = u32::try_from(COPY).expect("a copy fits a message");
        self.refused = match self.step {
            Step::Fsync(_) => storage.fsync(self.tag).is_err(),
            Step::Read(copy) | Step::Write(copy) => {
                let offset = u64::try_from(copy * COPY).expect("inside the zone");
                let buffer = self.buffer.take().expect("the buffer is home");
                let submitted = if let Step::Read(_) = self.step {
                    storage.read(self.tag, Zone::Superblock, offset, len, buffer)
                } else {
                    storage.write(self.tag, Zone::Superblock, offset, len, buffer)
                };
                submitted
                    .map_err(|Full(buffer)| self.buffer = Some(buffer))
                    .is_err()
            }
            Step::Idle => unreachable!("nothing to submit"),
        };
    }

    fn bytes(&mut self) -> &mut [u8] {
        let buffer = self.buffer.as_mut().expect("the buffer is home");
        buffer.bytes_mut().expect("the buffer is not shared")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Identity = Identity {
        cluster: 0xA,
        group: 7,
        replica: 2,
    };

    /// Rewrites every copy of `zone` with `edit` applied, resealed.
    fn edited(mut zone: Vec<u8>, edit: impl Fn(&mut [u8])) -> Vec<u8> {
        for (index, copy) in zone.chunks_exact_mut(COPY).enumerate() {
            edit(copy);
            seal(copy, index);
        }
        zone
    }

    /// `record` encoded as copy `index`.
    fn copy_of(record: &Record, index: usize) -> Vec<u8> {
        let mut copy = vec![0; COPY];
        encode(record, &mut copy);
        seal(&mut copy, index);
        copy
    }

    /// A record after the formatted one, whose headers span many sectors.
    fn next() -> Record {
        let mut record = Record::new(A);
        record.sequence = 1;
        record.vsr.view = 3;
        record.vsr.headers = (0..HEADERS_MAX).map(|i| [i as u8; HEADER]).collect();
        record.checkpoint.free_set = Reference {
            address: 9,
            checksum: 10,
        };
        record
    }

    fn opened(record: Record, stale: [bool; COPIES]) -> Result<Opened, OpenError> {
        Ok(Opened { record, stale })
    }

    #[test]
    fn a_formatted_file_opens_as_itself() {
        assert_eq!(open(&format(A), A), opened(Record::new(A), [false; 4]));
    }

    #[test]
    fn a_record_round_trips() {
        let zone: Vec<u8> = (0..COPIES).flat_map(|i| copy_of(&next(), i)).collect();
        assert_eq!(open(&zone, A), opened(next(), [false; 4]));
    }

    #[test]
    fn another_identity_is_refused_with_its_own_error() {
        let zone = format(A);
        let b = |identity| open(&zone, identity);
        assert_eq!(
            b(Identity { cluster: 0xB, ..A }),
            Err(OpenError::Cluster { stored: 0xA })
        );
        assert_eq!(
            b(Identity { group: 8, ..A }),
            Err(OpenError::Group { stored: 7 })
        );
        assert_eq!(
            b(Identity { replica: 0, ..A }),
            Err(OpenError::Replica { stored: 2 })
        );
    }

    #[test]
    fn another_version_is_refused() {
        for version in [1, VERSION + 1] {
            let zone = edited(format(A), |c| {
                c[32..36].copy_from_slice(&version.to_le_bytes())
            });
            assert_eq!(open(&zone, A), Err(OpenError::Version { stored: version }));
        }
    }

    /// Gate of T0.1.4: the stored constant that differs is named.
    #[test]
    fn a_differing_format_constant_is_named() {
        let zone = edited(format(A), |c| c[CONSTANTS] += 1);
        let (constant, binary) = FORMAT.entries()[0];
        assert_eq!(
            open(&zone, A),
            Err(OpenError::Format(FormatMismatch {
                constant,
                stored: binary + 1,
                binary
            }))
        );
    }

    #[test]
    fn a_reserved_byte_that_is_not_zero_is_refused() {
        for at in [
            COPY_INDEX + 1,
            CONSTANTS - 1,
            HEADER_COUNT + 4,
            HEADERS,
            COPY - 1,
        ] {
            let zone = edited(format(A), |c| c[at] = 1);
            assert_eq!(open(&zone, A), Err(OpenError::Reserved), "byte {at}");
        }
    }

    #[test]
    fn more_headers_than_fit_are_refused() {
        let stored = u32::try_from(HEADERS_MAX + 1).unwrap();
        let zone = edited(format(A), |c| {
            c[HEADER_COUNT..][..4].copy_from_slice(&stored.to_le_bytes())
        });
        assert_eq!(open(&zone, A), Err(OpenError::Headers { stored }));
    }

    #[test]
    fn one_corrupt_copy_is_stale() {
        for corrupt in 0..COPIES {
            let mut zone = format(A);
            zone[corrupt * COPY + COPY - 1] ^= 1;
            let mut stale = [false; COPIES];
            stale[corrupt] = true;
            assert_eq!(open(&zone, A), opened(Record::new(A), stale));
        }
    }

    #[test]
    fn the_highest_sequence_number_with_a_quorum_wins() {
        let old = format(A);
        let new: Vec<u8> = (0..COPIES).flat_map(|i| copy_of(&next(), i)).collect();
        let mix = |new_copies: &[usize]| -> Vec<u8> {
            (0..COPIES)
                .flat_map(|i| {
                    let from = if new_copies.contains(&i) { &new } else { &old };
                    from[i * COPY..][..COPY].to_vec()
                })
                .collect()
        };
        assert_eq!(
            open(&mix(&[0]), A),
            opened(Record::new(A), [true, false, false, false])
        );
        assert_eq!(
            open(&mix(&[0, 1]), A),
            opened(next(), [false, false, true, true])
        );
    }

    /// Without a quorum nothing is chosen: three corrupt copies, or two
    /// records with one sequence number on two copies each.
    #[test]
    fn no_quorum_is_refused() {
        let mut zone = format(A);
        for copy in 1..COPIES {
            zone[copy * COPY + 20] ^= 1;
        }
        assert_eq!(open(&zone, A), Err(OpenError::NoQuorum));

        let mut other = next();
        other.vsr.view += 1;
        let zone: Vec<u8> = [&next(), &next(), &other, &other]
            .iter()
            .enumerate()
            .flat_map(|(i, r)| copy_of(r, i))
            .collect();
        assert_eq!(open(&zone, A), Err(OpenError::NoQuorum));
    }

    /// Gate: a copy written at the wrong offset is not a copy for its
    /// position. Copies 0 and 1 of a newer record, misdirected onto the
    /// offsets of copies 2 and 3, would otherwise be a quorum for it.
    #[test]
    fn a_misdirected_copy_is_not_accepted() {
        let mut zone = format(A);
        zone[2 * COPY..][..COPY].copy_from_slice(&copy_of(&next(), 0));
        zone[3 * COPY..][..COPY].copy_from_slice(&copy_of(&next(), 1));
        assert_eq!(
            open(&zone, A),
            opened(Record::new(A), [false, false, true, true])
        );
    }

    /// Gate: a crash between any two consecutive sector writes of an update,
    /// the copies written in order and each copy's sectors in order, leaves
    /// the old record or the new one. Prints the positions it covers.
    #[test]
    fn a_crash_between_any_two_sector_writes_opens_old_or_new() {
        let old = format(A);
        let new: Vec<u8> = (0..COPIES).flat_map(|i| copy_of(&next(), i)).collect();
        let sector = FORMAT.sector_size as usize;
        let mut outcomes = Vec::new();
        for landed in 0..=new.len() / sector {
            let mut zone = old.clone();
            zone[..landed * sector].copy_from_slice(&new[..landed * sector]);
            let record = open(&zone, A).expect("a quorum survives").record;
            let outcome = match record.sequence {
                0 => "old",
                _ => "new",
            };
            assert!(record == Record::new(A) || record == next());
            outcomes.push(format!("{landed}:{outcome}"));
        }
        assert_eq!(outcomes.len(), COPIES * COPY / sector + 1);
        println!("sectors landed:outcome {}", outcomes.join(" "));
    }
}
