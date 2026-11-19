//! Shard threads pinned to a core, with their memory on that core's NUMA
//! node (document 06 section 4).

#![allow(
    unsafe_code,
    reason = "sched_setaffinity and set_mempolicy are system calls (docs/06 section 3)"
)]

use std::io;
use std::thread::{Builder, JoinHandle};

/// Starts a thread pinned to `core` and runs `shard` on it. Build the shard's
/// state inside `shard`, not before: memory is placed on the node of the core
/// that first touches it. The thread returns the pinning error, if any,
/// without running `shard`.
pub fn spawn<T, F>(core: usize, shard: F) -> io::Result<JoinHandle<io::Result<T>>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    Builder::new().name(format!("shard-{core}")).spawn(move || {
        pin(core)?;
        Ok(shard())
    })
}

/// Pins the calling thread to `core` and makes its allocations local to the
/// core's node.
fn pin(core: usize) -> io::Result<()> {
    if core >= libc::CPU_SETSIZE as usize {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    // SAFETY: `cpu_set_t` is a plain bit array, for which all zeroes is valid.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: `core` is below `CPU_SETSIZE`, checked above, so the bit is in
    // the set.
    unsafe { libc::CPU_SET(core, &mut set) };
    // SAFETY: `set` is a valid `cpu_set_t` of the size passed; pid 0 is the
    // calling thread.
    if unsafe { libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // MPOL_LOCAL places a page on the node of the CPU that first touches it,
    // which, pinned, is this core's node, whatever policy the process has.
    // SAFETY: MPOL_LOCAL takes no node mask: a null mask of length zero.
    let result = unsafe {
        libc::syscall(
            libc::SYS_set_mempolicy,
            libc::MPOL_LOCAL,
            std::ptr::null::<libc::c_ulong>(),
            0usize,
        )
    };
    let error = io::Error::last_os_error();
    // A kernel built without NUMA has one node, so there is nothing to place.
    if result != 0 && error.raw_os_error() != Some(libc::ENOSYS) {
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cores the calling thread may run on.
    fn affinity() -> Vec<usize> {
        // SAFETY: as in `pin`, all zeroes is a valid `cpu_set_t`.
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: `set` is a valid, writable `cpu_set_t` of the size passed.
        let result = unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut set) };
        assert_eq!(result, 0, "{}", io::Error::last_os_error());
        (0..libc::CPU_SETSIZE as usize)
            // SAFETY: every index is below `CPU_SETSIZE`.
            .filter(|&core| unsafe { libc::CPU_ISSET(core, &set) })
            .collect()
    }

    /// The calling thread's memory policy, or `None` without NUMA support.
    fn mempolicy() -> Option<libc::c_int> {
        let mut mode: libc::c_int = -1;
        // SAFETY: `mode` is writable; a null node mask of length zero asks
        // for the mode alone, of the calling thread (flags 0).
        let result = unsafe {
            libc::syscall(
                libc::SYS_get_mempolicy,
                &mut mode,
                std::ptr::null_mut::<libc::c_ulong>(),
                0usize,
                std::ptr::null_mut::<libc::c_void>(),
                0usize,
            )
        };
        let error = io::Error::last_os_error();
        if result != 0 && error.raw_os_error() == Some(libc::ENOSYS) {
            return None;
        }
        assert_eq!(result, 0, "{error}");
        Some(mode)
    }

    #[test]
    fn shard_thread_runs_on_exactly_its_core() {
        let allowed = affinity();
        assert!(!allowed.is_empty());
        for &core in &allowed {
            let thread = spawn(core, || (affinity(), mempolicy())).expect("thread starts");
            let (cores, policy) = thread.join().expect("no panic").expect("pinned");
            println!("pinned to {core}: affinity {cores:?}, memory policy {policy:?}");
            assert_eq!(cores, [core], "affinity of the thread pinned to {core}");
            assert!(
                matches!(policy, None | Some(libc::MPOL_LOCAL)),
                "{policy:?}"
            );
        }
    }

    #[test]
    fn core_outside_the_set_is_refused() {
        let thread = spawn(libc::CPU_SETSIZE as usize, || ()).expect("thread starts");
        let error = thread.join().expect("no panic").expect_err("refused");
        assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
    }
}
