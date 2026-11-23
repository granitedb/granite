//! Reaching the clock, the network and the disk without the `Io` traits.
//! Rejected in the replica crate, accepted in the io crate.

mod libc {
    pub fn clock_gettime() -> i64 {
        0
    }
}
mod granite_io {
    pub mod uring {
        pub fn submit() {}
    }
}

pub fn bypass() -> i64 {
    granite_io::uring::submit();
    let _ = std::net::Shutdown::Both;
    let _: Option<std::fs::FileType> = None;
    let _: Option<std::os::raw::c_int> = None;
    libc::clock_gettime()
}
