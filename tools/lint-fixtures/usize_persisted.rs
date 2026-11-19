//! Rejected: a persisted layout whose size depends on the platform.

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Header {
    pub checksum: u128,
    /// Platform-sized, so a 32-bit and a 64-bit replica disagree on the layout.
    pub size: usize,
}

#[repr(C, packed)]
pub struct Offset(pub isize);

#[repr(C, align(64))]
pub struct Slot {
    pub index: usize,
}
