//! The gates of T1.1.6, the free set: a model test against a reference set;
//! the seed gate with crashes at any point; identical addresses for
//! 1,000,000 calls; growth at the same op for 100 workloads of 100,000 ops,
//! whatever the clock, the file system's free space and the read load. The
//! encode and decode gate and the full-set gate are unit tests of
//! `granite_storage::free_set`; the find-free timing is
//! `find_free_on_a_1_tib_grid_at_90_percent`, which only prints a number.
//!
//! The full gates run in the release profile with `GRANITE_FULL=1`:
//! `GRANITE_FULL=1 cargo test --release --locked -p granite-sim --test free_set -- --nocapture`.

use std::collections::BTreeSet;

use granite_base::constants::FORMAT;
use granite_io::memory::{Hub, Memory, SplitMix64};
use granite_io::message_pool::{Message, MessagePool};
use granite_io::{Completion, Io, Peer, Storage, Tag, Time};
use granite_sim::{Disk, Faults, Group, Scheduler};
use granite_storage::free_set::{CONTENT_SIZE, FreeSet, Reservation};
use granite_storage::grid::{BLOCK_SIZE, BlockType, Event, Grid, HEADER_SIZE, Reference};
use granite_storage::growth::{GridSize, NoSpace};

const STEP: u64 = FORMAT.grid_growth_step;
const STEP_BLOCKS: u64 = STEP / FORMAT.grid_block_size;
/// The grid's first tag, and the tags of an fsync and a growth.
const TAG: Tag = 1 << 40;
const FSYNC: Tag = 7;
const GROW: Tag = 8;
/// Grid operations in flight, and the disk's room for them and an fsync.
const OPS: u32 = 16;

/// Grows `set` when it asks to, as if the file grew at once.
fn grow(set: &mut FreeSet) -> Option<u64> {
    let size = set.growth()?;
    set.grown();
    Some(size)
}

fn full() -> bool {
    std::env::var_os("GRANITE_FULL").is_some()
}

/// Seeds for one call, from a workload's seed and the call's number.
fn draw(seed: u64, n: u64) -> SplitMix64 {
    SplitMix64(seed ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15))
}

/// The reference: plain sets, and the allocation policy written the
/// obvious way.
struct Model {
    blocks: u64,
    limit: u64,
    /// Free block indexes; released blocks are not in it until a durable
    /// checkpoint.
    free: BTreeSet<u64>,
    released: BTreeSet<u64>,
    cursor: u64,
    /// (next, end, left) of each outstanding reservation.
    reservations: Vec<(u64, u64, u64)>,
}

impl Model {
    fn promised(&self) -> u64 {
        self.reservations.iter().map(|r| r.2).sum()
    }

    /// One block encodes the set of any grid below a million blocks.
    fn headroom(&self) -> u64 {
        assert!(self.limit < 1_000_000);
        u64::from(self.blocks > 0)
    }

    fn acquire(&mut self) -> Result<u64, NoSpace> {
        if self.free.len() as u64 - self.promised() <= self.headroom() {
            return Err(NoSpace);
        }
        let index = *self.free.range(self.cursor..).next().ok_or(NoSpace)?;
        self.free.remove(&index);
        Ok(index + 1)
    }

    fn reserve(&mut self, count: u64) -> Result<(), NoSpace> {
        if (self.free.len() as u64) < self.promised() + count + self.headroom() {
            return Err(NoSpace);
        }
        let last = *self
            .free
            .range(self.cursor..)
            .nth(count as usize - 1)
            .ok_or(NoSpace)?;
        self.reservations.push((self.cursor, last + 1, count));
        self.cursor = last + 1;
        Ok(())
    }

    fn acquire_reserved(&mut self, at: usize) -> u64 {
        let (next, end, left) = &mut self.reservations[at];
        let index = *self.free.range(*next..*end).next().unwrap();
        self.free.remove(&index);
        (*next, *left) = (index + 1, *left - 1);
        index + 1
    }

    fn forfeit(&mut self, at: usize) {
        self.reservations.remove(at);
        if self.reservations.is_empty() {
            self.cursor = 0;
        }
    }

    fn grow(&mut self) -> Option<u64> {
        let available = self.free.len() as u64 - self.promised();
        if available >= STEP_BLOCKS / 8 || self.blocks == self.limit {
            return None;
        }
        self.free.extend(self.blocks..self.blocks + STEP_BLOCKS);
        self.blocks += STEP_BLOCKS;
        Some(self.blocks * FORMAT.grid_block_size)
    }

    /// Takes the checkpoint's `count` blocks, the lowest free, after
    /// releasing the previous checkpoint's.
    fn checkpoint(&mut self, previous: &[u64], count: usize) -> Vec<u64> {
        self.released.extend(previous.iter().map(|a| a - 1));
        let taken: Vec<u64> = self.free.iter().take(count).copied().collect();
        for index in &taken {
            self.free.remove(index);
        }
        taken.iter().map(|i| i + 1).collect()
    }

    fn durable(&mut self) {
        self.free.append(&mut self.released);
    }
}

/// Model test: 1,000 cases (100 without `GRANITE_FULL`) of 1,000 ops, each
/// a burst of calls, against
/// [`Model`]. Every address, refusal and growth is the reference's; no block
/// is handed out while acquired, and none released is handed out before the
/// checkpoint after its release is durable.
#[test]
fn model_test_the_free_set_matches_the_reference() {
    let seed = 0x0f5e_e5e7;
    println!("model test seed {seed:#x}");
    let cases = if full() { 1_000 } else { 100 };
    for case in 0..cases {
        let mut random = draw(seed, case);
        let limit = 1 + random.next_u64() % 2;
        let mut set = FreeSet::new(GridSize::new(0, limit * STEP));
        let mut model = Model {
            blocks: 0,
            limit: limit * STEP_BLOCKS,
            free: BTreeSet::new(),
            released: BTreeSet::new(),
            cursor: 0,
            reservations: Vec::new(),
        };
        let mut reservations: Vec<Reservation> = Vec::new();
        // Acquired and not released, and the last checkpoint's blocks.
        let mut live: Vec<u64> = Vec::new();
        let mut chain: Vec<u64> = Vec::new();
        let handed = |address: u64, live: &mut Vec<u64>, model: &Model| {
            let index = address - 1;
            assert!(!live.contains(&address), "case {case}: {address} twice");
            assert!(!model.released.contains(&index), "case {case}: released");
            live.push(address);
        };
        for op in 0..1_000 {
            let burst = 1 + random.next_u64() % 64;
            match random.next_u64() % 16 {
                0..5 => {
                    for _ in 0..burst {
                        let got = set.acquire();
                        assert_eq!(got, model.acquire(), "case {case} op {op}");
                        if let Ok(address) = got {
                            handed(address, &mut live, &model);
                        }
                    }
                }
                5..9 => {
                    for _ in 0..burst.min(live.len() as u64) {
                        let at = (random.next_u64() % live.len() as u64) as usize;
                        let address = live.swap_remove(at);
                        set.release(address);
                        model.released.insert(address - 1);
                    }
                }
                9 if reservations.len() < 4 => {
                    let count = 1 + random.next_u64() % 16;
                    let got = set.reserve(count);
                    assert_eq!(got.is_ok(), model.reserve(count).is_ok(), "case {case}");
                    reservations.extend(got);
                }
                10..13 if !reservations.is_empty() => {
                    let at = (random.next_u64() % reservations.len() as u64) as usize;
                    for _ in 0..burst.min(model.reservations[at].2) {
                        let address = set.acquire_reserved(&mut reservations[at]);
                        assert_eq!(address, model.acquire_reserved(at), "case {case}");
                        handed(address, &mut live, &model);
                    }
                }
                13 if !reservations.is_empty() => {
                    let at = (random.next_u64() % reservations.len() as u64) as usize;
                    set.forfeit(reservations.remove(at));
                    model.forfeit(at);
                }
                14 => assert_eq!(grow(&mut set), model.grow(), "case {case} op {op}"),
                _ if reservations.is_empty() => {
                    let count = set.checkpoint();
                    chain = model.checkpoint(&chain, count);
                    let mut content = vec![0; CONTENT_SIZE];
                    for at in 0..count {
                        let next = Reference {
                            address: chain.get(at + 1).copied().unwrap_or(0),
                            checksum: 0,
                        };
                        assert_eq!(set.encode(at, next, &mut content), chain[at]);
                    }
                    set.checkpoint_durable();
                    model.durable();
                }
                _ => {}
            }
            assert_eq!(set.free(), model.free.len() as u64, "case {case} op {op}");
        }
        for index in 0..model.blocks {
            let free = model.free.contains(&index);
            assert_eq!(set.is_acquired(index + 1), !free, "case {case}");
        }
    }
}

/// Two sets given the same 1,000,000 calls return identical addresses; the
/// second is rebuilt from the first's encoding at every checkpoint.
#[test]
fn the_same_calls_give_the_same_addresses() {
    let seed = 0xadd_7e55;
    let grid = GridSize::new(0, 4 * STEP);
    let [mut a, mut b] = [FreeSet::new(grid), FreeSet::new(grid)];
    let mut random = SplitMix64(seed);
    let mut live: Vec<u64> = Vec::new();
    let mut reservation: Option<[Reservation; 2]> = None;
    let mut contents = Vec::new();
    for call in 0..1_000_000u64 {
        match random.next_u64() % 100 {
            0..45 => {
                let got = a.acquire();
                assert_eq!(got, b.acquire(), "seed {seed:#x} call {call}");
                live.extend(got);
            }
            45..90 if !live.is_empty() => {
                let address = live.swap_remove((random.next_u64() % live.len() as u64) as usize);
                a.release(address);
                b.release(address);
            }
            90..94 => match reservation.take() {
                None => {
                    let count = 1 + random.next_u64() % 32;
                    match (a.reserve(count), b.reserve(count)) {
                        (Ok(x), Ok(y)) => reservation = Some([x, y]),
                        (Err(NoSpace), Err(NoSpace)) => {}
                        _ => panic!("seed {seed:#x} call {call}: one reserved"),
                    }
                }
                Some([x, y]) => {
                    a.forfeit(x);
                    b.forfeit(y);
                }
            },
            94..97 => {
                if let Some([x, y]) = &mut reservation
                    && x.left() > 0
                {
                    let address = a.acquire_reserved(x);
                    assert_eq!(address, b.acquire_reserved(y), "call {call}");
                    live.push(address);
                }
            }
            97 => assert_eq!(grow(&mut a), grow(&mut b), "call {call}"),
            _ if reservation.is_none() => {
                let count = a.checkpoint();
                assert_eq!(count, b.checkpoint());
                contents.resize(count, vec![0; CONTENT_SIZE]);
                let mut addresses = vec![0; count];
                let mut next = Reference::default();
                for at in (0..count).rev() {
                    addresses[at] = a.encode(at, next, &mut contents[at]);
                    next = Reference {
                        address: addresses[at],
                        checksum: 0,
                    };
                }
                a.checkpoint_durable();
                b = FreeSet::new(grid);
                for at in 0..count {
                    b.decode(addresses[at], &contents[at]);
                }
            }
            _ => {}
        }
    }
}

/// One op of a growth workload: grow if low, acquire 0 to 3 blocks, release
/// 0 to 2, and a checkpoint every 960 ops. Returns whether the grid grew and
/// the blocks acquired.
fn growth_op(
    set: &mut FreeSet,
    live: &mut Vec<u64>,
    seed: u64,
    op: u64,
    grow: impl FnOnce(&mut FreeSet) -> bool,
) -> (bool, Vec<u64>) {
    let mut random = draw(seed, op);
    let grew = grow(set);
    let acquired: Vec<u64> = (0..random.next_u64() % 4)
        .filter_map(|_| set.acquire().ok())
        .collect();
    live.extend(&acquired);
    for _ in 0..(random.next_u64() % 3).min(live.len() as u64) {
        let at = (random.next_u64() % live.len() as u64) as usize;
        set.release(live.swap_remove(at));
    }
    if op.is_multiple_of(FORMAT.checkpoint_interval_ops) {
        set.checkpoint();
        set.checkpoint_durable();
    }
    (grew, acquired)
}

/// Two sets fed the same 100,000 ops grow their grids at the same op, for
/// 100 random workloads.
#[test]
fn two_sets_fed_the_same_ops_grow_at_the_same_op() {
    let (workloads, ops) = if full() { (100, 100_000) } else { (10, 20_000) };
    let mut growths = 0;
    for workload in 0..workloads {
        let seed = 0x6e0_0000 + workload;
        let grid = GridSize::new(0, 16 * STEP);
        let mut sets = [FreeSet::new(grid), FreeSet::new(grid)];
        let mut live = [Vec::new(), Vec::new()];
        for op in 1..=ops {
            let [a, b] = [0, 1]
                .map(|i| growth_op(&mut sets[i], &mut live[i], seed, op, |s| grow(s).is_some()));
            assert_eq!(a, b, "workload {workload} op {op}");
            growths += u64::from(a.0);
        }
        assert!(sets[0].grid().size() >= 2 * STEP, "the workload grows");
    }
    println!("{workloads} workloads of {ops} ops: {growths} growths, each at the same op");
}

/// Runs a growth workload of `ops` ops on `io`, growing its grid zone and
/// writing every 64th block acquired; with `reads`, reads every written block
/// back after each op. Returns the ops at which the grid grew.
fn grow_on<S: Storage>(
    io: &mut S,
    mut poll: impl FnMut(&mut S) -> Option<Completion>,
    pool: &MessagePool,
    ops: u64,
    reads: bool,
) -> Vec<u64> {
    let seed = 0x6e0_1111;
    let mut set = FreeSet::new(GridSize::new(0, 4 * STEP));
    let mut grid = Grid::new(TAG, OPS);
    let mut live = Vec::new();
    let mut written: Vec<Reference> = Vec::new();
    let mut grew = Vec::new();
    let mut wait = |io: &mut S| loop {
        if let Some(completion) = poll(io) {
            return completion;
        }
    };
    for op in 1..=ops {
        let grow = |s: &mut FreeSet| {
            let Some(size) = s.growth() else {
                return false;
            };
            io.grow_grid(GROW, size).unwrap();
            let completion = wait(io);
            assert!(matches!(
                completion,
                Completion::Grown { result: Ok(()), .. }
            ));
            s.grown();
            true
        };
        let (g, acquired) = growth_op(&mut set, &mut live, seed, op, grow);
        if g {
            grew.push(op);
        }
        for address in acquired.into_iter().filter(|a| a % 64 == 0) {
            let block = pool.acquire().expect("a buffer");
            written.push(grid.write(io, address, BlockType::Manifest, block).unwrap());
            assert!(matches!(grid.complete(wait(io)), Event::Written { .. }));
        }
        if reads && !written.is_empty() {
            let reference = written[op as usize % written.len()];
            {
                let buffer = pool.acquire().expect("a buffer");
                grid.read(io, reference, BlockType::Manifest, buffer)
                    .unwrap();
                let Event::Read { result, .. } = grid.complete(wait(io)) else {
                    panic!("a read");
                };
                result.expect("the block reads back");
            }
        }
    }
    assert!(grew.len() >= 2, "the workload grows the grid twice");
    grew
}

/// Growth depends only on the ops applied: the same workload grows at the
/// same ops on a memory backend whose clock stands still, on a simulated
/// disk whose virtual clock is moved by random latencies and which reads
/// every block back after every op, and (on macOS) on a real data file next
/// to a 256 MiB ballast file that takes file-system free space.
#[test]
fn growth_ignores_time_free_space_and_read_load() {
    let ops = 20_000;
    let pool = MessagePool::new(4);
    let s = FORMAT.sector_size;
    let grid_zone = [s, s, s, 0];

    let mut memory = Memory::new(&Hub::new(), Peer(0), grid_zone, OPS + 1, 1);
    let still = grow_on(&mut memory, Memory::poll, &pool, ops, false);
    assert_eq!(memory.monotonic(), 0, "the clock stood still");
    drop(memory);

    let mut sim = Scheduler::<()>::new(2, 1);
    let faults = Faults {
        latency: 500_000,
        latency_max: 50_000_000,
        ..Faults::default()
    };
    let mut disk = Disk::new(
        &Group::new(1, 1, grid_zone),
        0,
        OPS as usize + 1,
        faults,
        &mut sim,
    );
    let clock = sim.clock();
    let poll = |disk: &mut Disk| {
        if let Some(at) = disk.ready_at()
            && at > sim.now()
        {
            sim.schedule(at, ()).unwrap();
            sim.step();
        }
        disk.poll()
    };
    let moving = grow_on(&mut disk, poll, &pool, ops, true);
    assert!(clock.monotonic() > 0, "the clock moved");
    assert_eq!(still, moving, "the same growth ops");
    drop(disk);

    #[cfg(target_os = "macos")]
    {
        use granite_io::portable::Portable;
        let dir = std::env::temp_dir().join(format!("granite-free-set-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ballast"),
            vec![1u8; usize::try_from(STEP / 4).unwrap()],
        )
        .unwrap();
        let mut file = Portable::open(&dir.join("data"), grid_zone, OPS + 1).unwrap();
        let on_file = grow_on(&mut file, Portable::poll, &pool, ops, false);
        drop(file);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(still, on_file, "the same growth ops");
    }
    println!("growth at ops {still:?} on every backend");
}

/// What the superblock keeps of a checkpoint, and what the harness knows of
/// the state it references.
#[derive(Clone, Default)]
struct Checkpoint {
    op: u64,
    free_set: Reference,
    live: Vec<Reference>,
    chain: Vec<u64>,
}

/// The process crashed.
struct Crashed;

/// Ops in one crash run, and the checkpoint interval of the harness.
const CRASH_OPS: u64 = 48;
const INTERVAL: u64 = 8;

/// One replica of the crash harness: data blocks stamped with their op,
/// written through the grid to a simulated disk, a checkpoint every
/// [`INTERVAL`] ops that writes the free set and fsyncs before the
/// superblock (here [`Checkpoint`]) names it, and a crash at submission
/// `crash_at`.
struct Replica<'a> {
    seed: u64,
    disk: Disk,
    grid: Grid,
    set: FreeSet,
    pool: &'a MessagePool,
    live: Vec<Reference>,
    chain: Vec<u64>,
    /// Released since the last durable checkpoint.
    staged: Vec<u64>,
    committed: Checkpoint,
    steps: u64,
    crash_at: u64,
}

impl Replica<'_> {
    /// A step at which the process may crash.
    fn step(&mut self) -> Result<(), Crashed> {
        self.steps += 1;
        if self.steps == self.crash_at {
            self.disk.crash();
            return Err(Crashed);
        }
        Ok(())
    }

    fn complete(&mut self) -> Event {
        let completion = self.disk.poll().expect("a completion is due");
        self.grid.complete(completion)
    }

    /// A buffer whose content is zero but for `stamp` at its start.
    fn buffer(&self, stamp: u64) -> Message {
        let mut block = self.pool.acquire().expect("a buffer");
        let bytes = block.bytes_mut().unwrap();
        bytes[HEADER_SIZE..BLOCK_SIZE].fill(0);
        bytes[HEADER_SIZE..HEADER_SIZE + 8].copy_from_slice(&stamp.to_le_bytes());
        block
    }

    fn op(&mut self, op: u64) -> Result<(), Crashed> {
        let mut random = draw(self.seed, op);
        if let Some(size) = self.set.growth() {
            self.disk.grow_grid(GROW, size).unwrap();
            self.step()?;
            let grown = self.disk.poll();
            assert!(matches!(
                grown,
                Some(Completion::Grown { result: Ok(()), .. })
            ));
            self.set.grown();
        }
        for _ in 0..random.next_u64() % 4 {
            let address = self.set.acquire().expect("the grid has room");
            let taken = self.live.iter().any(|r| r.address == address)
                || self.chain.contains(&address)
                || self.staged.contains(&address);
            assert!(
                !taken,
                "seed {}: block {address} handed out twice",
                self.seed
            );
            let block = self.buffer(op);
            let reference = self
                .grid
                .write(&mut self.disk, address, BlockType::Manifest, block)
                .unwrap();
            self.step()?;
            assert!(matches!(self.complete(), Event::Written { .. }));
            self.live.push(reference);
        }
        let mut releases = random.next_u64() % 3;
        if self.live.len() > 64 {
            releases += 2;
        }
        for _ in 0..releases.min(self.live.len() as u64) {
            let at = (random.next_u64() % self.live.len() as u64) as usize;
            let reference = self.live.swap_remove(at);
            self.set.release(reference.address);
            self.staged.push(reference.address);
        }
        if op.is_multiple_of(INTERVAL) {
            self.checkpoint(op)?;
        }
        Ok(())
    }

    fn checkpoint(&mut self, op: u64) -> Result<(), Crashed> {
        let count = self.set.checkpoint();
        self.staged.append(&mut self.chain);
        let mut chain = vec![0; count];
        let mut next = Reference::default();
        for at in (0..count).rev() {
            let mut block = self.pool.acquire().expect("a buffer");
            let content = &mut block.bytes_mut().unwrap()[HEADER_SIZE..BLOCK_SIZE];
            chain[at] = self.set.encode(at, next, content);
            next = self
                .grid
                .write(&mut self.disk, chain[at], BlockType::FreeSet, block)
                .unwrap();
        }
        for _ in 0..count {
            self.step()?;
            assert!(matches!(self.complete(), Event::Written { .. }));
        }
        self.disk.fsync(FSYNC).unwrap();
        self.step()?;
        assert!(matches!(
            self.disk.poll(),
            Some(Completion::Fsync { result: Ok(()), .. })
        ));
        self.step()?;
        self.committed = Checkpoint {
            op,
            free_set: next,
            live: self.live.clone(),
            chain: chain.clone(),
        };
        self.chain = chain;
        self.step()?;
        self.set.checkpoint_durable();
        self.staged.clear();
        Ok(())
    }

    /// Reads `reference` as a block of `kind`.
    fn read(&mut self, reference: Reference, kind: BlockType) -> Message {
        let buffer = self.pool.acquire().expect("a buffer");
        self.grid
            .read(&mut self.disk, reference, kind, buffer)
            .unwrap();
        let Event::Read { result, .. } = self.complete() else {
            panic!("a read");
        };
        result.unwrap_or_else(|error| {
            panic!(
                "seed {}: block {} of the recovered checkpoint: {error:?}",
                self.seed, reference.address
            )
        })
    }

    /// Restarts from the committed checkpoint: decodes the free set it
    /// names, reads back every block it references, and takes its state.
    fn recover(&mut self) {
        let checkpoint = self.committed.clone();
        self.grid = Grid::new(TAG, OPS);
        self.set = FreeSet::new(GridSize::new(0, STEP));
        let mut next = checkpoint.free_set;
        while next.address != 0 {
            let block = self.read(next, BlockType::FreeSet);
            next = self
                .set
                .decode(next.address, &block.bytes()[HEADER_SIZE..BLOCK_SIZE]);
        }
        for &reference in &checkpoint.live {
            self.read(reference, BlockType::Manifest);
        }
        self.live = checkpoint.live;
        self.chain = checkpoint.chain;
        self.staged.clear();
    }

    /// The acquired blocks are exactly those the state references: the
    /// live blocks, the checkpoint's encoding and the blocks released since.
    fn check(&self) {
        let expected: BTreeSet<u64> = (self.live.iter().map(|r| r.address))
            .chain(self.chain.iter().copied())
            .chain(self.staged.iter().copied())
            .collect();
        let acquired: BTreeSet<u64> = (1..=self.set.grid().blocks())
            .filter(|&a| self.set.is_acquired(a))
            .collect();
        assert_eq!(
            acquired, expected,
            "seed {}: leak or double allocation",
            self.seed
        );
    }
}

/// Runs `seed` once without a crash and once with a crash at a step drawn
/// from it; after recovery and replay to the last op, the replica's set and
/// state match the run without the crash. Returns the steps of the run.
fn crash_seed(seed: u64, pool: &MessagePool) -> u64 {
    let replica = |crash_at| {
        let mut sim = Scheduler::<()>::new(seed, 1);
        let s = FORMAT.sector_size;
        let group = Group::new(1, 1, [s, s, s, 0]);
        let faults = Faults {
            torn_writes: true,
            ..Faults::default()
        };
        Replica {
            seed,
            disk: Disk::new(&group, 0, OPS as usize + 1, faults, &mut sim),
            grid: Grid::new(TAG, OPS),
            set: FreeSet::new(GridSize::new(0, STEP)),
            pool,
            live: Vec::new(),
            chain: Vec::new(),
            staged: Vec::new(),
            committed: Checkpoint::default(),
            steps: 0,
            crash_at,
        }
    };
    let mut clean = replica(u64::MAX);
    for op in 1..=CRASH_OPS {
        assert!(clean.op(op).is_ok());
    }
    clean.check();
    let crash_at = 1 + draw(seed, u64::MAX).next_u64() % clean.steps;
    let mut crashed = replica(crash_at);
    let at = (1..=CRASH_OPS)
        .find(|&op| crashed.op(op).is_err())
        .expect("the crash step is reached");
    crashed.recover();
    crashed.crash_at = u64::MAX;
    for op in crashed.committed.op + 1..=CRASH_OPS {
        assert!(crashed.op(op).is_ok());
        if op == at {
            crashed.check();
        }
    }
    crashed.check();
    let blocks = clean.set.grid().blocks();
    assert_eq!(crashed.set.grid().blocks(), blocks, "seed {seed}");
    for address in 1..=blocks {
        assert_eq!(
            crashed.set.is_acquired(address),
            clean.set.is_acquired(address),
            "seed {seed}: block {address}"
        );
    }
    assert_eq!(crashed.live, clean.live, "seed {seed}: the same addresses");
    clean.steps
}

/// Seed gate 10,000, crashes at any point: a crash at a step drawn from the
/// seed, anywhere in a run of 48 ops with 6 checkpoints, tearing the writes
/// in flight. After recovery every block the recovered checkpoint references
/// reads back, and after replay the acquired blocks are exactly those the
/// state references and those of the run without a crash.
///
/// Injected bug: `FreeSet::release` clearing the block's acquired bit and
/// adding one to `free` at once, instead of staging it, fails this test on
/// seed 0 with "block 1 handed out twice": a block the last checkpoint
/// still references is handed out again before the next one is durable.
/// With that check and `Replica::check` removed, recovery still fails it,
/// on seed 2 with "block 1 of the recovered checkpoint: Mismatch": the
/// block was overwritten.
#[test]
fn seed_gate_a_crash_at_any_point_recovers_the_set() {
    let seeds = if full() { 10_000 } else { 200 };
    let pool = MessagePool::new(OPS + 2);
    let steps: u64 = (0..seeds).map(|seed| crash_seed(seed, &pool)).sum();
    println!("{seeds} seeds, one crash each among {steps} steps");
}

/// Find-free on a 1 TiB grid at 90% occupancy, the blocks acquired at
/// random: prints the mean time of an acquire. The gate needs real
/// hardware, so this only measures:
/// `cargo test --release --locked -p granite-sim --test free_set find_free -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, run by hand"]
fn find_free_on_a_1_tib_grid_at_90_percent() {
    let tib = STEP << 10;
    let mut set = FreeSet::new(GridSize::new(tib, tib));
    let blocks = set.grid().blocks();
    let mut random = SplitMix64(0x1e1b);
    let mut acquired: Vec<u64> = std::iter::from_fn(|| set.acquire().ok()).collect();
    // Release a random tenth, so the free blocks are spread over the grid.
    for _ in 0..blocks / 10 {
        let at = (random.next_u64() % acquired.len() as u64) as usize;
        set.release(acquired.swap_remove(at));
    }
    set.checkpoint();
    set.checkpoint_durable();
    let calls: u32 = 100_000;
    let start = std::time::Instant::now();
    for _ in 0..calls {
        std::hint::black_box(set.acquire().unwrap());
    }
    let mean = start.elapsed().as_nanos() as f64 / f64::from(calls);
    println!(
        "{blocks} blocks, {} free before: {mean:.1} ns per find-free and acquire",
        set.free() + u64::from(calls)
    );
}
