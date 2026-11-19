//! The io_uring backend on a real file: the storage conformance suite,
//! durability of completed writes across `SIGKILL`, and no allocation after
//! startup.
//!
//! The data files go under the temporary directory (`TMPDIR`), which must be
//! on a file system with direct I/O; `just vm-check` points it at the VM's
//! data disk.

#![cfg(target_os = "linux")]

#[path = "common/counting_alloc.rs"]
mod counting_alloc;

use granite_io::memory::SplitMix64;
use granite_io::message_pool::{MESSAGE_SIZE, Message, MessagePool, SECTOR_SIZE};
use granite_io::uring::Uring;
use granite_io::{Completion, Storage, Zone};
use std::io::{BufRead, BufReader, ErrorKind, Read};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

const SECTOR: u64 = SECTOR_SIZE as u64;

/// Registered pools count against `ulimit -l`, so the tests take turns.
static SERIAL: Mutex<()> = Mutex::new(());

/// A directory under the temporary directory, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("granite-io-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn data(&self) -> PathBuf {
        self.0.join("data")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wait(storage: &mut Uring) -> Completion {
    loop {
        if let Some(completion) = storage.poll() {
            return completion;
        }
    }
}

/// Opens the backend on `path`. The kernel releases a closed ring
/// asynchronously, once the I/O it had in flight is done: only then does its
/// registered memory go back to `ulimit -l` and its data file's lock go away.
/// A ring opened just after another one closed (an earlier test's, or a
/// killed child's) can be refused for that moment, so such an open is
/// retried for up to five seconds.
fn open(path: &Path, zones: [u64; 4], capacity: u32, pool: &MessagePool) -> Uring {
    let mut refused = None;
    for _ in 0..500 {
        match Uring::open(path, zones, capacity, pool) {
            Ok(storage) => return storage,
            Err(e) if matches!(e.kind(), ErrorKind::OutOfMemory | ErrorKind::WouldBlock) => {
                refused = Some(e);
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    }
    let limits = std::fs::read_to_string("/proc/self/limits").unwrap();
    let memlock = limits
        .lines()
        .find(|line| line.starts_with("Max locked memory"));
    panic!("still refused after 5 s: {refused:?}; {memlock:?}");
}

/// A second backend on a data file another one holds is refused, so a
/// restarted process never shares the file with writes still in flight.
#[test]
fn a_data_file_in_use_is_refused() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new("lock");
    let pool = MessagePool::new(1);
    let _first = open(&scratch.data(), kill_zones(), 1, &pool);
    let Err(error) = Uring::open(&scratch.data(), kill_zones(), 1, &pool) else {
        panic!("a second backend opened the data file");
    };
    assert_eq!(error.kind(), ErrorKind::WouldBlock, "{error}");
}

#[test]
fn uring_passes_the_storage_conformance_suite() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new("conformance");
    let pool = MessagePool::new(4);
    let zones = [4 * SECTOR, 4 * SECTOR, 8 * SECTOR, 2 * MESSAGE_SIZE as u64];
    let mut storage = open(&scratch.data(), zones, 8, &pool);
    granite_io::conformance::run_storage(&mut storage, Uring::poll, &pool);
}

/// Grid sectors the child of the kill test writes, one each, in order.
const KILL_SECTORS: u64 = 64;
const KILL_ITERATIONS: u32 = 1_000;
/// Set in the child's environment: the data file and the iteration.
const KILL_CHILD: &str = "GRANITE_IO_KILL_CHILD";

fn kill_zones() -> [u64; 4] {
    [SECTOR, SECTOR, SECTOR, KILL_SECTORS * SECTOR]
}

/// Each iteration starts this test binary as a child that writes sectors
/// one after the other and reports each completed write. After a number of
/// reports drawn from a seed, the child is killed with `SIGKILL`; the
/// restarted backend then reads every reported sector back.
#[test]
fn completed_writes_survive_sigkill() {
    if let Ok(setting) = std::env::var(KILL_CHILD) {
        kill_child(&setting);
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new("kill");
    let seed = 0x6b69_6c6c;
    println!("seed {seed:#x}, {KILL_ITERATIONS} iterations");
    let mut random = SplitMix64(seed);
    for iteration in 0..KILL_ITERATIONS {
        let wanted = 1 + random.next_u64() % 8;
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "completed_writes_survive_sigkill", "--nocapture"])
            .env(
                KILL_CHILD,
                format!("{iteration} {}", scratch.data().display()),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        // The reader lives until the child is dead, so the child never
        // writes to a closed pipe.
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut reported = 0;
        for line in lines.by_ref() {
            if let Some(sector) = line.unwrap().strip_prefix("wrote ") {
                assert_eq!(sector.parse::<u64>().unwrap(), reported, "in order");
                reported += 1;
                if reported == wanted {
                    break;
                }
            }
        }
        child.kill().unwrap(); // SIGKILL
        let status = child.wait().unwrap();
        drop(lines);
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "iteration {iteration}: {status}"
        );
        assert_eq!(
            reported, wanted,
            "iteration {iteration}: the child stopped early"
        );

        let pool = MessagePool::new(1);
        let mut storage = open(&scratch.data(), kill_zones(), 1, &pool);
        let len = (wanted * SECTOR) as u32;
        storage
            .read(0, Zone::Grid, 0, len, pool.acquire().unwrap())
            .unwrap();
        let Completion::Read {
            buffer,
            result: Ok(()),
            ..
        } = wait(&mut storage)
        else {
            panic!("iteration {iteration}: the read back failed");
        };
        for sector in 0..wanted {
            let at = (sector * SECTOR) as usize;
            assert_eq!(
                stamp(&buffer.bytes()[at..at + 16]),
                (u64::from(iteration), sector),
                "iteration {iteration}: sector {sector} was reported written"
            );
        }
    }
}

/// The child of the kill test: writes `KILL_SECTORS` sectors stamped with
/// its iteration, reports each completed one, then waits to be killed. It
/// exits if its parent goes away first, which closes its standard input.
fn kill_child(setting: &str) -> ! {
    let (iteration, path) = setting.split_once(' ').unwrap();
    let iteration: u64 = iteration.parse().unwrap();
    let pool = MessagePool::new(1);
    let mut storage = open(Path::new(path), kill_zones(), 1, &pool);
    let mut buffer = pool.acquire().unwrap();
    for sector in 0..KILL_SECTORS {
        let bytes = buffer.bytes_mut().unwrap();
        bytes[..8].copy_from_slice(&iteration.to_le_bytes());
        bytes[8..16].copy_from_slice(&sector.to_le_bytes());
        storage
            .write(0, Zone::Grid, sector * SECTOR, SECTOR as u32, buffer)
            .unwrap();
        let Completion::Write {
            buffer: back,
            result: Ok(()),
            ..
        } = wait(&mut storage)
        else {
            panic!("a write failed");
        };
        buffer = back;
        println!("wrote {sector}");
    }
    let _ = std::io::stdin().read(&mut [0]);
    std::process::exit(1);
}

fn stamp(bytes: &[u8]) -> (u64, u64) {
    let word = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    (word(0), word(8))
}

const OPERATIONS: u32 = 1_000_000;
/// Operations in flight, each with its own buffer and its own grid sectors
/// (sector `s` belongs to slot `s % SLOTS`), so none overlaps another.
const SLOTS: u64 = 4;
const GRID_SECTORS: u64 = 128;

/// Starts an operation for `slot` drawn from `random`: mostly reads, some
/// writes and a few fsyncs. Returns the buffer if the operation does not
/// take it.
fn start(storage: &mut Uring, random: u64, slot: u64, buffer: Message) -> Option<Message> {
    let sector = (random >> 8) % (GRID_SECTORS / SLOTS) * SLOTS + slot;
    let (offset, len) = (sector * SECTOR, SECTOR as u32);
    match random % 64 {
        0 => {
            storage.fsync(slot).unwrap();
            return Some(buffer);
        }
        1..=4 => storage
            .write(slot, Zone::Grid, offset, len, buffer)
            .unwrap(),
        _ => storage.read(slot, Zone::Grid, offset, len, buffer).unwrap(),
    }
    None
}

#[test]
fn a_million_mixed_operations_allocate_nothing_after_startup() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new("allocations");
    let pool = MessagePool::new(SLOTS as u32);
    let zones = [SECTOR, SECTOR, SECTOR, GRID_SECTORS * SECTOR];
    let mut storage = open(&scratch.data(), zones, SLOTS as u32, &pool);
    let mut random = SplitMix64(0x61_6c6c_6f63);
    // A slot's buffer is idle, in an operation, or parked during its fsync.
    let mut idle: [Option<Message>; SLOTS as usize] = std::array::from_fn(|_| pool.acquire());
    let mut parked: [Option<Message>; SLOTS as usize] = Default::default();

    let before = counting_alloc::allocations();
    let (mut started, mut completed, mut kinds) = (0, 0, [0u32; 3]);
    while completed < OPERATIONS {
        for slot in 0..SLOTS {
            if started < OPERATIONS
                && let Some(buffer) = idle[slot as usize].take()
            {
                parked[slot as usize] = start(&mut storage, random.next_u64(), slot, buffer);
                started += 1;
            }
        }
        let Some(completion) = storage.poll() else {
            continue;
        };
        completed += 1;
        let slot = completion.tag() as usize;
        let (kind, buffer) = match completion {
            Completion::Read {
                buffer,
                result: Ok(()),
                ..
            } => (0, Some(buffer)),
            Completion::Write {
                buffer,
                result: Ok(()),
                ..
            } => (1, Some(buffer)),
            Completion::Fsync { result: Ok(()), .. } => (2, parked[slot].take()),
            other => panic!("{other:?}"),
        };
        kinds[kind] += 1;
        idle[slot] = buffer;
    }
    let allocations = counting_alloc::allocations() - before;
    println!(
        "{completed} operations (reads, writes, fsyncs: {kinds:?}), {allocations} allocations"
    );
    assert_eq!(allocations, 0);
    assert!(kinds.iter().all(|&count| count > 0), "every kind ran");
}
