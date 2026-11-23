//! Wire message definitions and validation.
//!
//! Message bodies of document 05 section 1. [`batch`] is the row batch of
//! writes and gets, [`results`] the sparse result list of writes. Each is
//! validated whole on arrival, so a malformed body is rejected before any of
//! it reaches the state machine, and is then read in place.

pub mod batch;
pub mod results;

/// Why a body was rejected. The numbers are part of the protocol
/// (document 05 section 1, "Row batches").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Error {
    /// Shorter than its fixed part, or a result list not a whole number of entries.
    Truncated = 1,
    /// Longer than a message body.
    TooLarge = 2,
    /// The batch header's reserved field is not zero.
    ReservedBits = 3,
    /// The rows the count gives do not fit in the batch.
    CountTooLarge = 4,
    /// A slot's content ends past the end of the heap.
    OffsetBeyondHeap = 5,
    /// A slot starts before the end of the slot before it.
    OverlappingSlots = 6,
    /// Heap bytes that no slot covers.
    HeapGap = 7,
    /// A String or Bytes content longer than its column's max.
    TooLong = 8,
    /// String content that is not UTF-8.
    InvalidUtf8 = 9,
    /// An enum index above the column's declared values.
    UnknownEnumValue = 10,
    /// Fixed-width bytes that no value of the column's type encodes to.
    InvalidValue = 11,
    /// A result entry out of order, for an event the batch does not have, or with code 0.
    InvalidResult = 12,
}

/// The longest body: one message less its header.
const BODY_SIZE_MAX: usize = {
    use granite_base::constants::FORMAT;
    (FORMAT.message_size_max - FORMAT.header_size) as usize
};
