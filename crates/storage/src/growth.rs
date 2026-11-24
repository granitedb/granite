//! How large the grid is, and how it grows (document 01 section 2).
//!
//! The grid grows by one `FORMAT.grid_growth_step` when the free set runs
//! low, up to the group's size limit. Nothing here reads the clock, the file
//! system's free space or the read load, so every replica of a group grows at
//! the same op; `tools/lint.py` keeps those names out of this crate.

use granite_base::constants::FORMAT;

/// The grid is at its group's size limit and cannot grow; the write that
/// needed a block is rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoSpace;

/// The size of the grid and its limit, both whole growth steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    size: u64,
    limit: u64,
}

impl GridSize {
    /// A grid of `size` bytes that may grow to `limit` bytes.
    pub fn new(size: u64, limit: u64) -> Self {
        assert!(
            size.is_multiple_of(FORMAT.grid_growth_step),
            "size is whole growth steps"
        );
        assert!(
            limit.is_multiple_of(FORMAT.grid_growth_step),
            "limit is whole growth steps"
        );
        assert!(size <= limit, "the grid is within its limit");
        GridSize { size, limit }
    }

    /// Size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The group's size limit in bytes.
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Size in blocks.
    pub fn blocks(&self) -> u64 {
        self.size / FORMAT.grid_block_size
    }

    /// Grows the grid by one step, or returns [`NoSpace`] at the limit.
    pub fn grow(&mut self) -> Result<(), NoSpace> {
        if self.size == self.limit {
            return Err(NoSpace);
        }
        self.size += FORMAT.grid_growth_step;
        assert!(self.size <= self.limit);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_by_whole_steps_up_to_the_limit() {
        let step = FORMAT.grid_growth_step;
        let mut grid = GridSize::new(0, 2 * step);
        assert_eq!(grid.grow(), Ok(()));
        assert_eq!(
            (grid.size(), grid.blocks()),
            (step, step / FORMAT.grid_block_size)
        );
        assert_eq!(grid.grow(), Ok(()));
        assert_eq!(grid.grow(), Err(NoSpace));
        assert_eq!(grid.size(), 2 * step);
    }

    #[test]
    #[should_panic(expected = "limit is whole growth steps")]
    fn a_limit_of_part_of_a_step_is_refused() {
        GridSize::new(0, FORMAT.grid_block_size);
    }
}
