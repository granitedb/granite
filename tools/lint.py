#!/usr/bin/env python3
"""`just lint`: clippy, the determinism bans, and the rules clippy cannot express.

The rules and their reasons are in docs/coding-rules.md. Clippy enforces the
bans listed in clippy.toml; this script runs it twice so that tests and
test-only modules are exempt from them, then checks what clippy cannot: where
`unsafe` may appear, `usize`/`isize` in `#[repr(C)]` layouts, dependencies
on random-number crates, unchecked arithmetic and `as` casts in the crates
that compute replicated state, names of the clock or the file system's
free space in the storage crate, direct clock, network and disk access in
the replica crate, and the engine's crates named in the reference model.

Before linting the workspace it lints every file in tools/lint-fixtures/ as
if it sat at a given path in the repository, and fails unless each banned
fixture is rejected for the expected reason and the io fixture is accepted.
"""
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "tools" / "lint-fixtures"

# The only files that may contain `unsafe` (docs/06 section 3: io, pools and
# SIMD kernels). Add a file here in the change that needs it, with its reason.
UNSAFE_MODULES = {
    "crates/io/src/message_pool.rs",  # the message pool
    "crates/io/src/uring.rs",  # io_uring submissions and registered buffers
    "crates/io/src/portable.rs",  # fcntl with F_NOCACHE and F_FULLFSYNC in the macOS backend
    "crates/io/src/pin.rs",  # sched_setaffinity and set_mempolicy for shard threads
    "crates/io/tests/common/counting_alloc.rs",  # the counting global allocator of the io tests
    "crates/base/src/checksum/simd.rs",  # checksum SIMD kernels
    "crates/base/src/checksum/aegis.rs",  # test-only kernels of decision 0002
}

# The one crate that may use the banned APIs; crates/io/clippy.toml exempts it from clippy's bans.
IO_CRATE = "crates/io/"

# Crates that produce random numbers, banned as normal dependencies outside io.
RANDOM_CRATES = {"rand", "rand_core", "getrandom", "fastrand"}

# Crates whose library and binary code may not wrap, saturate, skip an overflow
# check or cast with `as` (docs/coding-rules.md), and their packages. Their
# `#[cfg(test)]` modules and tests/ are exempt, as tests are from the determinism bans.
CHECKED_CRATES = ("crates/types/src/", "crates/txn/src/", "crates/shard/src/")
CHECKED_PACKAGES = ["granite-types", "granite-txn", "granite-shard"]
UNCHECKED = re.compile(r"\b(?:(?:wrapping|saturating|unchecked|overflowing)_\w+|Wrapping|Saturating"
                       r"|cast_(?:un)?signed|unbounded_sh[lr])\b")
TEST_MODULE = re.compile(r"#\[cfg\(test\)\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{")
# Clippy knows the types, so it finds every `as` cast, through type aliases too.
AS_PASS = ["--lib", "--bins", "--locked", "--no-deps", "--", "-D", "warnings", "-D", "clippy::as_conversions"]

# The storage crate decides when the grid grows, which must depend only on the ops applied
# (docs/01 section 2): it names neither the clock nor the file system's free space.
STORAGE_CRATE = "crates/storage/src/"
GROWTH_INPUTS = re.compile(r"\b(?:Time|monotonic|realtime|f?statv?fs)\b")

# The replica reaches the clock, the network and the disk only through the `Io` traits it is handed
# (T2.2.1, docs/06 section 1): no system interface, socket or event crate, and no concrete io backend.
VSR_CRATE = "crates/vsr/src/"
DIRECT_IO = re.compile(r"\b(?:std::(?:time|net|fs|os|process)|libc|nix|mio|socket2|tokio"
                       r"|granite_io::(?:uring|portable|file|pin))\b")

# The reference model shares no code with the engine (T1.5.3): it names neither the storage, lsm,
# columnar or shard crate, nor, through `crate::`, the rest of the simulator, which links them all.
MODEL_DIR = "crates/sim/src/model/"
ENGINE_NAMES = re.compile(r"\bgranite_(?:storage|lsm|columnar|shard)\b|\bcrate::")

BANS = ["clippy::disallowed_methods", "clippy::disallowed_types", "clippy::disallowed_macros"]
CLIPPY_PASSES = [
    # Every target, with the determinism bans off: tests may read files and time themselves.
    ["--all-targets", "--locked", "--", "-D", "warnings", *(a for ban in BANS for a in ("-A", ban))],
    # Library and binary code with the bans on; `#[cfg(test)]` modules are not compiled here.
    ["--lib", "--bins", "--locked", "--", "-D", "warnings"],
]

# A fixture, the path it is linted as, and what the lint must say (empty: accepted).
FIXTURE_CASES = [
    ("hash_map.rs", "crates/storage/src/lib.rs", [
        "disallowed type `std::collections::HashMap`",
        "disallowed type `std::collections::HashSet`",
        "disallowed type `std::hash::RandomState`",
    ]),
    ("time.rs", "crates/storage/src/lib.rs", [
        "disallowed type `std::time::Instant`",
        "disallowed type `std::time::SystemTime`",
        "disallowed type `std::time::Duration`",
        "disallowed method `std::time::SystemTime::elapsed`",
    ]),
    ("thread.rs", "crates/storage/src/lib.rs", [
        "disallowed method `std::thread::spawn`",
        "disallowed macro `std::thread_local`",
    ]),
    ("fs.rs", "crates/storage/src/lib.rs", [
        "disallowed type `std::fs::File`",
        "disallowed method `std::fs::read`",
        "disallowed method `std::path::Path::exists`",
    ]),
    ("net.rs", "crates/storage/src/lib.rs", [
        "disallowed type `std::net::SocketAddr`",
        "disallowed type `std::net::TcpStream`",
    ]),
    ("mul_add.rs", "crates/storage/src/lib.rs", [
        "disallowed method `f64::mul_add`",
        "disallowed method `f32::mul_add`",
    ]),
    ("usize_persisted.rs", "crates/storage/src/lib.rs", [
        "crates/storage/src/lib.rs:8: usize or isize",
        "crates/storage/src/lib.rs:12: usize or isize",
        "crates/storage/src/lib.rs:16: usize or isize",
    ]),
    ("rand.toml", "crates/storage/Cargo.toml", [
        "crates/storage/Cargo.toml: depends on rand",
        "crates/storage/Cargo.toml: depends on getrandom",
    ]),
    ("unsafe_outside_list.rs", "crates/storage/src/lib.rs", ["crates/storage/src/lib.rs:9: unsafe outside"]),
    ("unsafe_without_safety.rs", "crates/io/src/message_pool.rs", ["unsafe block missing a safety comment"]),
    ("allow_without_reason.rs", "crates/storage/src/lib.rs", ["`allow` attribute without specifying a reason"]),
    *(("unchecked_arithmetic.rs", f"crates/{c}/src/lib.rs", [
        *(f"crates/{c}/src/lib.rs:{line}: unchecked arithmetic `{name}`" for line, name in [
            (7, "wrapping_add"), (11, "saturating_sub"), (15, "overflowing_mul"), (19, "wrapping_neg"),
            (23, "cast_unsigned"), (27, "unchecked_add"), (30, "Wrapping"), (31, "Saturating"),
        ]),
        "using a potentially dangerous silent `as` conversion",
        # Clippy names the fixture crate's own path; a type alias and a full path are casts too.
        *(f"src/lib.rs:{line}:5" for line in (35, 39, 43, 47)),
    ]) for c in ("types", "txn", "shard")),
    ("unchecked_arithmetic.rs", "crates/storage/src/lib.rs", []),
    ("unchecked_arithmetic.rs", "crates/types/tests/arith.rs", []),
    ("checked_arithmetic.rs", "crates/types/src/lib.rs", []),
    ("growth_inputs.rs", "crates/storage/src/growth.rs", [
        f"crates/storage/src/growth.rs:{line}: `{name}` in the storage crate" for line, name in [
            (5, "Time"), (6, "monotonic"), (9, "statvfs"), (13, "Time"), (14, "monotonic"), (14, "statvfs"),
        ]
    ]),
    ("growth_inputs.rs", "crates/vsr/src/lib.rs", []),
    ("replica_io.rs", "crates/vsr/src/replica.rs", [
        f"crates/vsr/src/replica.rs:{line}: `{name}` in the replica crate" for line, name in [
            (16, "granite_io::uring"), (17, "std::net"), (18, "std::fs"), (19, "std::os"), (20, "libc"),
        ]
    ]),
    ("replica_io.rs", "crates/io/src/lib.rs", []),
    ("model_imports.rs", "crates/sim/src/model/mod.rs", [
        f"crates/sim/src/model/mod.rs:{line}: `{name}` in the reference model" for line, name in [
            (4, "granite_storage"), (7, "granite_lsm"), (10, "granite_columnar"), (13, "granite_shard"),
            (17, "granite_storage"), (21, "granite_lsm"), (22, "crate::"), (22, "granite_columnar"),
            (23, "granite_shard"),
        ]
    ]),
    ("model_imports.rs", "crates/sim/src/node.rs", []),
    ("io_accepted.rs", "crates/io/src/lib.rs", []),
    ("rand.toml", "crates/io/Cargo.toml", []),
]

# Comments, raw strings, char literals and strings, in the order they must be matched.
NOT_CODE = re.compile(r'//[^\n]*|/\*.*?\*/|r(#*)".*?"\1|\'(?:\\.|[^\'\\\n])\'|"(?:\\.|[^"\\])*"', re.S)
REPR = re.compile(r"#\[repr\(([^\]]*)\)\]")
ITEM = re.compile(r"(?:\s*#\[[^\]]*\])*\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|union)\b[^{(;]*([{(])")


def code_only(text):
    """`text` with comments and literals blanked, keeping line numbers."""
    return NOT_CODE.sub(lambda m: "\n" * m.group(0).count("\n"), text)


def line_of(text, offset):
    return text.count("\n", 0, offset) + 1


def closing(code, open_):
    """The offset of the bracket that closes the one at `open_`."""
    depth = 0
    for end in range(open_, len(code)):
        depth += {"{": 1, "(": 1, "}": -1, ")": -1}.get(code[end], 0)
        if depth == 0:
            return end
    return len(code)


def without_tests(code):
    """`code` with its `#[cfg(test)]` modules blanked, keeping line numbers."""
    for m in reversed(list(TEST_MODULE.finditer(code))):
        end = closing(code, m.end() - 1)
        code = code[:m.start()] + re.sub(r"[^\n]", " ", code[m.start():end + 1]) + code[end + 1:]
    return code


def unchecked_arithmetic(path, code):
    """Errors for wrapping, saturating and unchecked arithmetic outside test modules."""
    code = without_tests(code)
    return [f"{path}:{line_of(code, m.start())}: unchecked arithmetic `{m.group(0)}`; use the checked "
            "form or granite_types::arith (docs/coding-rules.md)" for m in UNCHECKED.finditer(code)]


def source_violations(path, text):
    """Errors for one Rust file at `path`, relative to the repository root."""
    code, errors = code_only(text), []
    if path.startswith(CHECKED_CRATES):
        errors += unchecked_arithmetic(path, code)
    if path.startswith(STORAGE_CRATE):
        errors += [f"{path}:{line_of(code, m.start())}: `{m.group(0)}` in the storage crate, whose grid growth "
                   "depends only on the ops applied (docs/01 section 2)" for m in GROWTH_INPUTS.finditer(code)]
    if path.startswith(VSR_CRATE):
        errors += [f"{path}:{line_of(code, m.start())}: `{m.group(0)}` in the replica crate, which reaches the "
                   "clock, network and disk only through `Io` (T2.2.1)" for m in DIRECT_IO.finditer(without_tests(code))]
    if path.startswith(MODEL_DIR):
        errors += [f"{path}:{line_of(code, m.start())}: `{m.group(0)}` in the reference model, which shares no "
                   "code with the engine (T1.5.3)" for m in ENGINE_NAMES.finditer(code)]
    if path not in UNSAFE_MODULES:
        errors += [f"{path}:{line_of(code, m.start())}: unsafe outside the modules listed in tools/lint.py"
                   for m in re.finditer(r"\bunsafe\b", code)]
    if not path.startswith(IO_CRATE):
        for m in REPR.finditer(code):
            item = ITEM.match(code, m.end())
            if not re.search(r"\b(C|packed)\b", m.group(1)) or not item:
                continue
            # The body runs to the bracket that closes the one ITEM stopped at.
            open_ = item.end() - 1
            end = closing(code, open_)
            errors += [f"{path}:{line_of(code, open_ + u.start())}: usize or isize in a #[repr({m.group(1)})] "
                       "type, which is a persisted or transmitted layout"
                       for u in re.finditer(r"\b[iu]size\b", code[open_:end])]
    return errors


def manifest_violations(path, text):
    """Errors for one Cargo.toml at `path`: normal dependencies on random-number crates."""
    if path.startswith(IO_CRATE):
        return []
    data = tomllib.loads(text)
    tables = [data.get("dependencies", {})] + [t.get("dependencies", {}) for t in data.get("target", {}).values()]
    names = {spec.get("package", name) if isinstance(spec, dict) else name for t in tables for name, spec in t.items()}
    return [f"{path}: depends on {name}, a random-number crate" for name in sorted(names & RANDOM_CRATES)]


def clippy(scope, cwd, env=None, capture=False, passes=CLIPPY_PASSES):
    """Runs the clippy passes; returns (passed, output)."""
    ok, out = True, ""
    for args in passes:
        r = subprocess.run(["cargo", "clippy", *scope, *args], cwd=cwd, env=env, text=True,
                           stdout=subprocess.PIPE if capture else None, stderr=subprocess.STDOUT if capture else None)
        ok, out = ok and r.returncode == 0, out + (r.stdout or "")
    return ok, out


def clippy_conf_dir(path):
    """The directory of the clippy.toml that clippy reads for the crate holding `path`, a file under src/."""
    crate = ROOT / path.split("/src/")[0]
    return next(d for d in [crate, *crate.parents] if (d / "clippy.toml").exists())


def fixture_workspace():
    """Writes a workspace under target/ with one crate per Rust fixture; returns its directory.

    It sits inside the repository so it uses the real rust-toolchain.toml, and it copies the
    real [workspace.lints]. Files are rewritten only when they change, so reruns are fast.
    """
    ws = ROOT / "target" / "lint-fixtures"
    lints = re.findall(r"^\[workspace\.lints[^\n]*\n(?:[^\[][^\n]*\n|\n)*", (ROOT / "Cargo.toml").read_text(), re.M)
    assert lints, "no [workspace.lints] in Cargo.toml"
    names = sorted({f.removesuffix(".rs") for f, _, _ in FIXTURE_CASES if f.endswith(".rs")})
    files = {"Cargo.toml": '[workspace]\nresolver = "3"\nmembers = [%s]\n\n' % ", ".join(f'"{n}"' for n in names)
             + "".join(lints)}
    for n in names:
        files[f"{n}/Cargo.toml"] = (f'[package]\nname = "{n}"\nversion = "0.0.0"\nedition = "2024"\n'
                                    "publish = false\n\n[lints]\nworkspace = true\n")
        files[f"{n}/src/lib.rs"] = (FIXTURES / f"{n}.rs").read_text()
    changed = set()
    for rel, text in files.items():
        p = ws / rel
        if not p.exists() or p.read_text() != text:
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text)
            changed.add(rel)
    # A new fixture is a new member, which a lock file from an earlier run lacks.
    if "Cargo.toml" in changed or not (ws / "Cargo.lock").exists():
        subprocess.run(["cargo", "generate-lockfile", "--offline", "--quiet"], cwd=ws, check=True)
    return ws


def self_test():
    """Lints every fixture as if it were at its path; returns the errors."""
    ws, errors = fixture_workspace(), []
    for fixture, path, expected in FIXTURE_CASES:
        text = (FIXTURES / fixture).read_text()
        if fixture.endswith(".toml"):
            ok, out = True, ""
            found = manifest_violations(path, text)
        else:
            env = dict(os.environ, CLIPPY_CONF_DIR=str(clippy_conf_dir(path)))
            passes = CLIPPY_PASSES + [AS_PASS] * path.startswith(CHECKED_CRATES)
            ok, out = clippy(["-p", fixture.removesuffix(".rs")], ws, env, capture=True, passes=passes)
            found = source_violations(path, text)
        out += "".join(e + "\n" for e in found)
        rejected = not ok or bool(found)
        missing = [e for e in expected if e not in out]
        if missing or rejected != bool(expected):
            if not expected:
                verdict = "rejected, but it must be accepted"
            else:
                verdict = "accepted" if not rejected else "rejected without: " + "; ".join(missing)
            errors.append(f"fixture {fixture} as {path}: {verdict}\n{out}")
    # Literals and comments never count as code.
    assert code_only('let s = "unsafe usize"; // unsafe\nlet c = \'"\'; /* unsafe */ x') == "let s = ; \nlet c = ;  x"
    return errors


def main():
    errors = self_test()
    for e in errors:
        print(f"lint self-test: {e}", file=sys.stderr)
    if errors:
        sys.exit(1)
    print(f"lint self-test: {len(FIXTURE_CASES)} fixtures rejected or accepted as expected")

    ok, _ = clippy(["--workspace"], ROOT)
    ok &= clippy([a for p in CHECKED_PACKAGES for a in ("-p", p)], ROOT, passes=[AS_PASS])[0]
    files = subprocess.run(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=ROOT,
                           check=True, stdout=subprocess.PIPE, text=True).stdout.rstrip("\0").split("\0")
    files = [f for f in files if not f.startswith("tools/lint-fixtures/") and (ROOT / f).exists()]
    errors = [e for f in files if f.endswith(".rs") for e in source_violations(f, (ROOT / f).read_text())]
    errors += [e for f in files if f.endswith("Cargo.toml") for e in manifest_violations(f, (ROOT / f).read_text())]
    errors += [f"{m} is listed in UNSAFE_MODULES but does not exist" for m in sorted(UNSAFE_MODULES - set(files))]
    for e in errors:
        print(f"lint: {e}", file=sys.stderr)
    if errors or not ok:
        sys.exit(1)
    print(f"lint: clippy clean, {len(files)} files follow the unsafe, layout and dependency rules")


if __name__ == "__main__":
    main()
