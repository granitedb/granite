# Coding rules

Replicas apply the same operations and must end in byte-identical state, so the code that computes that state must give the same result on every machine, every run. [06-testing-and-engineering.md](06-testing-and-engineering.md) section 3 sets the determinism rules. This document says, for each one, why it exists and how it is enforced: by `just lint`, which CI runs, or by review where a tool cannot decide it.

## How `just lint` enforces them

`just lint` runs [tools/lint.py](../tools/lint.py), which does three things:

1. **Clippy, twice.** The first pass lints every target (tests included) with `-D warnings` and the determinism bans switched off. The second pass lints library and binary code only, with the bans on. Tests and `#[cfg(test)]` modules may therefore read files and time themselves, while the code that ships may not.
2. **Checks clippy cannot express**, over every Rust file and `Cargo.toml` in the repository: where `unsafe` may appear, `usize`/`isize` in fixed layouts, and dependencies on random-number crates.
3. **A self-test, first.** Each file in [tools/lint-fixtures/](../tools/lint-fixtures/) is linted as if it sat at a given path (the `FIXTURE_CASES` table in `tools/lint.py`), with the real `clippy.toml` files and the real `[workspace.lints]`. The run fails unless every banned fixture is rejected for the expected reason and the io fixtures are accepted, so a rule that stops working fails CI instead of passing silently.

The bans live in [clippy.toml](../clippy.toml) at the root. Clippy reads the nearest `clippy.toml` above a crate, so they apply to every crate except `crates/io`, whose own [crates/io/clippy.toml](../crates/io/clippy.toml) lists none.

## The rules

| Rule (section 3) | Why | Enforcement |
|---|---|---|
| No `HashMap` or `HashSet` with random state | `std`'s hasher is seeded randomly per process, so iteration order differs between replicas and runs. | Enforced by lint: `clippy::disallowed_types` (`HashMap`, `HashSet`, `RandomState`) and `clippy::disallowed_methods` (`RandomState::new`). Use `BTreeMap`/`BTreeSet`. |
| A hash map, if used, has a fixed hasher and is never iterated | A fixed hasher removes the randomness but not the dependence on capacity and insertion history. | Review only: whether a map is ever iterated depends on how every caller uses it. Such a map is written with an exception (below) whose reason says "fixed hasher, never iterated", which makes it visible to the reviewer. |
| No `std::time` outside `io` | The clock differs between replicas; time enters the state machine only through the io traits, so the simulator controls it. | Enforced by lint: `clippy::disallowed_types` (`Instant`, `SystemTime`, `Duration`) and `clippy::disallowed_methods` (`Instant::now`, `SystemTime::now`, and their `elapsed`). |
| No `std::thread` outside `io` | Thread scheduling is not reproducible, and the simulator must run everything on one thread. | Enforced by lint: `clippy::disallowed_methods` (`spawn`, `scope`, `sleep`, `current`, `yield_now`, `park`, `available_parallelism`), `clippy::disallowed_types` (`Builder`, `JoinHandle`, `Thread`, `ThreadId`, `LocalKey`) and `clippy::disallowed_macros` (`thread_local!`). |
| No `std::fs` outside `io` | File contents and errors differ between machines; storage reaches the disk through the io traits so faults can be injected. | Enforced by lint: `clippy::disallowed_types` (`File`, `OpenOptions`, `DirBuilder`, `DirEntry`, `ReadDir`, `Metadata`) and `clippy::disallowed_methods` (the free functions of `std::fs`, and the `Path` methods that read the file system). |
| No `std::net` outside `io` | The network is the main source of nondeterminism; messages reach the state machine only through the io traits. | Enforced by lint: `clippy::disallowed_types` (`TcpListener`, `TcpStream`, `UdpSocket`, `IpAddr`, `Ipv4Addr`, `Ipv6Addr`, `SocketAddr`, `SocketAddrV4`, `SocketAddrV6`, `ToSocketAddrs`). |
| No `rand` outside `io` | Random numbers differ between replicas; randomness comes from a seed the simulator controls. | Enforced by lint: the dependency check of `tools/lint.py` rejects a normal dependency on `rand`, `rand_core`, `getrandom` or `fastrand` (also when renamed) in any crate but `io`. Dev-dependencies are allowed, as tests are exempt. A crate that pulls one in transitively is caught by the dependency audit in [CONTRIBUTING.md](../CONTRIBUTING.md), not by the lint. |
| No `usize` or `isize` in a persisted or transmitted format | Their size depends on the platform, so two replicas could disagree on a layout. | Enforced by lint: the layout check of `tools/lint.py` rejects `usize` or `isize` in any `#[repr(C)]` or `#[repr(packed)]` type outside `io`. A persisted or transmitted struct is declared with one of these, since its layout must be fixed. A format encoded by hand, field by field, is review only: whether a value is written to disk depends on the encoder, not on a declaration. |
| No floating-point reductions in unspecified order | Floating-point addition is not associative, so a different order gives different bits. | Review only: the order of a reduction depends on the data flow of the whole program (parallel or reordered sums), which a lint cannot follow. |
| No `mul_add` | A fused multiply-add rounds once, so its result differs from `a * b + c` and between machines with and without the instruction. | Enforced by lint: `clippy::disallowed_methods` (`f32::mul_add`, `f64::mul_add`). |
| No fast-math | Fast-math lets the compiler reorder and fuse operations. | Review only, as there is nothing to lint: Rust has no fast-math flag, and its fast-math intrinsics and `algebraic_*` float methods are unstable, which the pinned stable toolchain (`rust-toolchain.toml`) rejects. |
| Canonical NaN | NaN payloads differ between machines and operations. | Review only: whether a NaN reaches persisted state depends on data flow. The canonical values are `CANONICAL_NAN_F32` and `CANONICAL_NAN_F64` in `crates/types`. |
| No dependence on allocation addresses or pointer ordering | Addresses differ between runs. | Review only: a lint cannot tell whether an address or a pointer comparison influences state. |
| Padding bytes in persisted structs are zeroed and checked | Uninitialised padding leaks memory contents into the format. | Review only: each format's tests check its padding. Reading uninitialised memory needs `unsafe`, which is confined by the next rule. |
| Third-party codecs are vendored and pinned; a codec version bump is a format change | A new codec version may encode the same data differently. | Review only: `--locked` builds pin every version, but whether a crate is a codec and whether a bump changes the format is a judgement, made in the dependency audit of [CONTRIBUTING.md](../CONTRIBUTING.md). |
| `unsafe` only in `io`, pools and SIMD kernels | Undefined behaviour makes every other guarantee void, so it is kept to code that needs it and is reviewed closely. | Enforced by lint: `unsafe_code = "deny"` (rustc) in `[workspace.lints]`, and the `unsafe` check of `tools/lint.py`, which rejects the `unsafe` keyword in any file not listed in `UNSAFE_MODULES` there, even under a local `allow`. |
| Each `unsafe` block has a written safety argument | The argument is what the reviewer checks. | Enforced by lint: `clippy::undocumented_unsafe_blocks` requires a `// SAFETY:` comment before every `unsafe` block and `unsafe impl`. |

One more rule applies to all code: every `#[allow]` or `#[expect]` says why, with `reason = "..."`. It is enforced by `clippy::allow_attributes_without_reason`.

## Requesting an exception

An exception is a reviewed change, and the reason is written where the exception is.

- **One use in one place**, for example a fixed-hasher map that is never iterated: put `#[expect(clippy::disallowed_types, reason = "...")]` (or `disallowed_methods`) on the smallest item that needs it. Prefer `expect` to `allow`, so that the attribute fails once the use is gone.
- **`unsafe` in a new module:** add the file to `UNSAFE_MODULES` in `tools/lint.py` with a one-line reason, and allow `unsafe_code` on that module with a `reason`. Section 3 allows `unsafe` only in io, pools and SIMD kernels; anything else first needs a change to that section.
- **A whole crate**, as `io` has: a `clippy.toml` in the crate's directory replaces the root one. Section 3 names `io` as the only such crate, so this also needs a change to that section.
- **The layout or dependency check:** these have no local escape; change the check in `tools/lint.py`, with a fixture case that shows the new behaviour.
