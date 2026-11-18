//! Other crates reach the checksum only through
//! `granite_base::checksum::checksum` (decision 0002).
//!
//! The implementation inside `base` is private, so the compiler already
//! rejects calls into it from another crate. This test rejects the other way
//! around that: a second copy of the function. No manifest or Rust file
//! outside `crates/base` may name XXH3/xxHash or AEGIS (a crate such as
//! `xxhash-rust`, `twox-hash` or `aegis`, or code that uses one).

use std::fs;
use std::path::{Path, PathBuf};

const NAMES: [&str; 3] = ["xxh", "twox", "aegis"];
const BASE: &str = "crates/base";

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy();
        if path.is_dir() {
            if !name.starts_with('.') && name != "target" {
                files(&path, out);
            }
        } else if name.ends_with(".rs") || name == "Cargo.toml" {
            out.push(path);
        }
    }
}

/// The names in `NAMES` that `path` mentions, ignoring case.
fn mentions(path: &Path) -> Vec<&'static str> {
    let text = fs::read_to_string(path).unwrap().to_lowercase();
    NAMES.into_iter().filter(|n| text.contains(n)).collect()
}

#[test]
fn only_base_names_the_checksum_implementation() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let mut all = Vec::new();
    files(&root, &mut all);
    let (inside, outside): (Vec<_>, Vec<_>) =
        all.iter().partition(|f| f.starts_with(root.join(BASE)));
    assert!(
        inside.iter().any(|f| !mentions(f).is_empty()),
        "the scan reaches the implementation and recognises its names"
    );
    let found: Vec<String> = outside
        .iter()
        .filter_map(|f| {
            let names = mentions(f);
            (!names.is_empty()).then(|| {
                format!(
                    "{}: {}",
                    f.strip_prefix(&root).unwrap().display(),
                    names.join(", ")
                )
            })
        })
        .collect();
    assert!(
        found.is_empty(),
        "call granite_base::checksum::checksum instead of another implementation:\n{}",
        found.join("\n")
    );
}
