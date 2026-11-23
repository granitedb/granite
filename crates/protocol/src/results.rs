//! The sparse result list of a write batch (document 05 section 1, "Results
//! for writes"): one `(event index, result code)` entry per event that did
//! not succeed, in event order. An event without an entry succeeded, so an
//! empty list means every event succeeded.

use crate::{BODY_SIZE_MAX, Error};

/// Bytes of one entry: the u32 event index, then the u32 result code.
pub(crate) const ENTRY: usize = 8;

/// The code of an event that succeeded. It is never listed.
pub const OK: u32 = 0;

/// A result list that passed [`Results::validate`].
#[derive(Clone, Copy, Debug)]
pub struct Results<'a> {
    entries: &'a [[u8; ENTRY]],
}

impl<'a> Results<'a> {
    /// Checks the result list of a batch of `events` events: whole entries,
    /// indexes strictly increasing and below `events`, no code [`OK`].
    pub fn validate(bytes: &'a [u8], events: u32) -> Result<Self, Error> {
        if bytes.len() > BODY_SIZE_MAX {
            return Err(Error::TooLarge);
        }
        let (entries, rest) = bytes.as_chunks::<ENTRY>();
        if !rest.is_empty() {
            return Err(Error::Truncated);
        }
        let mut next = 0;
        for &e in entries {
            let (index, code) = split(e);
            if index < next || index >= events || code == OK {
                return Err(Error::InvalidResult);
            }
            next = index + 1;
        }
        Ok(Self { entries })
    }

    /// The entries, in event order.
    pub fn iter(&self) -> impl Iterator<Item = (u32, u32)> + use<'a> {
        self.entries.iter().map(|e| split(*e))
    }

    /// The result code of event `index`: [`OK`] unless it is listed.
    pub fn code(&self, index: u32) -> u32 {
        match self.entries.binary_search_by_key(&index, |e| split(*e).0) {
            Ok(i) => split(self.entries[i]).1,
            Err(_) => OK,
        }
    }
}

fn split(e: [u8; ENTRY]) -> (u32, u32) {
    let [a, b, c, d, e, f, g, h] = e;
    (
        u32::from_le_bytes([a, b, c, d]),
        u32::from_le_bytes([e, f, g, h]),
    )
}

/// Encodes the entries of the events that did not succeed. They must be in
/// strictly increasing event order, none with code [`OK`].
pub fn encode(entries: &[(u32, u32)]) -> Vec<u8> {
    assert!(
        entries.windows(2).all(|w| w[0].0 < w[1].0),
        "in event order"
    );
    let mut out = Vec::with_capacity(entries.len() * ENTRY);
    for &(index, code) in entries {
        assert_ne!(code, OK, "a success is not listed");
        out.extend(index.to_le_bytes());
        out.extend(code.to_le_bytes());
    }
    // A batch has at most `batch::ROWS_MAX` events, one entry each.
    assert!(out.len() <= BODY_SIZE_MAX);
    out
}
