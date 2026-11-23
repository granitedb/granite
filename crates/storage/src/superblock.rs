//! The superblock record as `granite format` writes it, and the identity
//! checks of opening a data file (document 01 section 2).
//!
//! A copy is `FORMAT.superblock_copy_size` bytes, little-endian:
//!
//! | Offset | Field |
//! |---|---|
//! | 0 | checksum of bytes 16 to the end of the copy, `u128` |
//! | 16 | cluster id, `u128` |
//! | 32 | format version, `u32` |
//! | 36 | group id, `u32` |
//! | 40 | replica index, `u8` |
//! | 41 | zero, 7 bytes |
//! | 48 | the format constants as `u64`, in [`FORMAT`] declaration order |
//! | after them | zero, to the end of the copy |

use granite_base::checksum::checksum;
use granite_base::constants::{FORMAT, FormatMismatch};

/// The format version this binary writes. A file with any other is refused.
pub const VERSION: u32 = 1;

const CONSTANTS: usize = 48;
const COPY: usize = FORMAT.superblock_copy_size as usize;

const _: () = assert!(CONSTANTS + FORMAT.entries().len() * 8 <= COPY);

/// Which replica of which group of which cluster a data file belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub cluster: u128,
    pub group: u32,
    pub replica: u8,
}

/// Why a data file was refused at open. Each reason is its own variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// No copy has a valid checksum.
    Corrupt,
    /// The file was written in another format version.
    Version { stored: u32 },
    /// A format constant differs from this binary's.
    Format(FormatMismatch),
    /// The file belongs to another cluster.
    Cluster { stored: u128 },
    /// The file belongs to another group.
    Group { stored: u32 },
    /// The file belongs to another replica.
    Replica { stored: u8 },
    /// A byte the layout keeps zero is not.
    Reserved,
}

/// The superblock zone of a newly formatted data file: every copy holds the
/// same record.
pub fn format(identity: Identity) -> Vec<u8> {
    let mut copy = vec![0; COPY];
    copy[16..32].copy_from_slice(&identity.cluster.to_le_bytes());
    copy[32..36].copy_from_slice(&VERSION.to_le_bytes());
    copy[36..40].copy_from_slice(&identity.group.to_le_bytes());
    copy[40] = identity.replica;
    for (i, (_, value)) in FORMAT.entries().into_iter().enumerate() {
        copy[CONSTANTS + i * 8..][..8].copy_from_slice(&value.to_le_bytes());
    }
    let sum = checksum(&copy[16..]);
    copy[..16].copy_from_slice(&sum.to_le_bytes());
    copy.repeat(FORMAT.superblock_copies as usize)
}

/// Checks the superblock zone `zone` of a data file opened as `expected`.
///
/// shortcut: reads the first copy whose checksum holds; T1.1.2 replaces this
/// with the quorum of the highest sequence number.
pub fn open(zone: &[u8], expected: Identity) -> Result<(), OpenError> {
    assert_eq!(zone.len(), COPY * FORMAT.superblock_copies as usize);
    let copy = zone
        .chunks_exact(COPY)
        .find(|c| checksum(&c[16..]).to_le_bytes() == c[..16])
        .ok_or(OpenError::Corrupt)?;
    let u32_at = |at: usize| u32::from_le_bytes(copy[at..at + 4].try_into().expect("4 bytes"));
    // The version comes first: another version may lay out the rest differently.
    let version = u32_at(32);
    if version != VERSION {
        return Err(OpenError::Version { stored: version });
    }
    for (i, (constant, binary)) in FORMAT.entries().into_iter().enumerate() {
        let stored =
            u64::from_le_bytes(copy[CONSTANTS + i * 8..][..8].try_into().expect("8 bytes"));
        if stored != binary {
            return Err(OpenError::Format(FormatMismatch {
                constant,
                stored,
                binary,
            }));
        }
    }
    let end = CONSTANTS + FORMAT.entries().len() * 8;
    if copy[41..CONSTANTS]
        .iter()
        .chain(&copy[end..])
        .any(|&b| b != 0)
    {
        return Err(OpenError::Reserved);
    }
    let cluster = u128::from_le_bytes(copy[16..32].try_into().expect("16 bytes"));
    if cluster != expected.cluster {
        return Err(OpenError::Cluster { stored: cluster });
    }
    let group = u32_at(36);
    if group != expected.group {
        return Err(OpenError::Group { stored: group });
    }
    if copy[40] != expected.replica {
        return Err(OpenError::Replica { stored: copy[40] });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Identity = Identity {
        cluster: 0xA,
        group: 7,
        replica: 2,
    };

    /// Rewrites every copy of `zone` with `edit` applied, checksums resealed.
    fn edited(mut zone: Vec<u8>, edit: impl Fn(&mut [u8])) -> Vec<u8> {
        for copy in zone.chunks_exact_mut(COPY) {
            edit(copy);
            let sum = checksum(&copy[16..]);
            copy[..16].copy_from_slice(&sum.to_le_bytes());
        }
        zone
    }

    #[test]
    fn a_formatted_file_opens_as_itself() {
        assert_eq!(open(&format(A), A), Ok(()));
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
    fn a_higher_version_is_refused() {
        let zone = edited(format(A), |c| {
            c[32..36].copy_from_slice(&(VERSION + 1).to_le_bytes())
        });
        assert_eq!(
            open(&zone, A),
            Err(OpenError::Version {
                stored: VERSION + 1
            })
        );
    }

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
        for at in [41, COPY - 1] {
            let zone = edited(format(A), |c| c[at] = 1);
            assert_eq!(open(&zone, A), Err(OpenError::Reserved));
        }
    }

    #[test]
    fn a_bad_copy_is_skipped_and_no_good_copy_is_corrupt() {
        let mut zone = format(A);
        zone[20] ^= 1;
        assert_eq!(open(&zone, A), Ok(()));
        for copy in 1..FORMAT.superblock_copies as usize {
            zone[copy * COPY + 20] ^= 1;
        }
        assert_eq!(open(&zone, A), Err(OpenError::Corrupt));
    }
}
