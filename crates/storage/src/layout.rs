//! Where each zone of the data file lies (document 01 section 2). Every
//! offset and size in the file comes from [`zones`]; backends take its sizes.

use granite_base::constants::FORMAT;
use granite_io::Zone;

/// A byte range of the data file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub offset: u64,
    pub size: u64,
}

impl Extent {
    /// The first byte after the range.
    pub const fn end(self) -> u64 {
        self.offset + self.size
    }
}

/// The zones in [`Zone::ALL`] order, for a grid of `grid_size` bytes, which
/// is a whole number of growth steps. The zones follow each other with no gap.
pub const fn zones(grid_size: u64) -> [Extent; 4] {
    assert!(
        grid_size.is_multiple_of(FORMAT.grid_growth_step),
        "the grid is a whole number of growth steps"
    );
    let superblock = Extent {
        offset: 0,
        size: FORMAT.superblock_copies * FORMAT.superblock_copy_size,
    };
    let wal_headers = Extent {
        offset: superblock.end(),
        size: FORMAT.wal_slot_count * FORMAT.header_size,
    };
    let wal_prepares = Extent {
        offset: wal_headers.end(),
        size: FORMAT.wal_slot_count * FORMAT.message_size_max,
    };
    let grid = Extent {
        offset: wal_prepares.end(),
        size: grid_size,
    };
    [superblock, wal_headers, wal_prepares, grid]
}

/// The zone sizes in [`Zone::ALL`] order, as the backends take them.
pub const fn zone_sizes(grid_size: u64) -> [u64; 4] {
    let z = zones(grid_size);
    [z[0].size, z[1].size, z[2].size, z[3].size]
}

/// The size of a data file whose grid is `grid_size` bytes.
pub const fn file_size(grid_size: u64) -> u64 {
    zones(grid_size)[Zone::Grid as usize].end()
}

// Every zone starts and ends on a sector boundary, whatever the grid size:
// the grid is whole growth steps, which FORMAT.validate makes whole sectors.
const _: () = {
    let z = zones(FORMAT.grid_growth_step);
    let mut i = 0;
    while i < z.len() {
        assert!(z[i].offset.is_multiple_of(FORMAT.sector_size));
        assert!(z[i].size.is_multiple_of(FORMAT.sector_size));
        assert!(i == 0 || z[i].offset == z[i - 1].end());
        i += 1;
    }
};

#[cfg(test)]
mod tests {
    use super::*;
    use granite_base::constants::{KIB, MIB};

    /// A size cell of the zone table, such as `4 x 64 KiB` or `slots x 256 B`.
    fn cell_bytes(cell: &str) -> u64 {
        cell.split(" x ")
            .map(|factor| match factor.split_once(' ') {
                None if factor == "slots" => FORMAT.wal_slot_count,
                None => factor.parse().expect("a count"),
                Some((n, unit)) => {
                    let unit = match unit {
                        "B" => 1,
                        "KiB" => KIB,
                        "MiB" => MIB,
                        _ => panic!("unit {unit}"),
                    };
                    n.parse::<u64>().expect("a number") * unit
                }
            })
            .product()
    }

    /// The zone table of document 01 section 2: same zones, same order, same
    /// sizes, and the grid is the rest of the file.
    #[test]
    fn zones_match_document_01() {
        let doc = include_str!("../../../docs/01-storage.md");
        let table = doc
            .split_once("| Zone | Contents | Size |\n|---|---|---|\n")
            .expect("the zone table")
            .1;
        let rows: Vec<Vec<&str>> = table
            .lines()
            .take_while(|l| l.starts_with('|'))
            .map(|l| l.trim_matches('|').split(" | ").map(str::trim).collect())
            .collect();
        let names = ["Superblock", "WAL headers", "WAL prepares", "Grid"];
        assert_eq!(rows.iter().map(|r| r[0]).collect::<Vec<_>>(), names);
        let z = zones(FORMAT.grid_growth_step);
        for (zone, row) in Zone::ALL.into_iter().zip(&rows) {
            if zone == Zone::Grid {
                assert_eq!(row[2], "rest of file");
                assert_eq!(z[zone as usize].end(), file_size(FORMAT.grid_growth_step));
            } else {
                assert_eq!(z[zone as usize].size, cell_bytes(row[2]), "{zone:?}");
            }
        }
    }

    #[test]
    #[should_panic(expected = "whole number of growth steps")]
    fn grid_of_part_of_a_step_is_refused() {
        zones(FORMAT.grid_block_size);
    }
}
