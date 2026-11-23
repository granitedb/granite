//! Every test of this crate that some system skips, because it or its file
//! is built for one system only or because it is ignored, is listed with its
//! reason in `tests/skipped.md`, and every test listed there exists.

use std::collections::BTreeSet;
use std::path::Path;

/// (file, test) pairs listed in `skipped.md`.
fn listed() -> BTreeSet<(String, String)> {
    let mut file = "";
    let mut tests = BTreeSet::new();
    let quoted = |rest: &str| rest.split('`').next().unwrap().to_owned();
    for line in include_str!("skipped.md").lines() {
        if let Some(rest) = line.strip_prefix("## `") {
            file = rest.split('`').next().unwrap();
        } else if let Some(rest) = line.strip_prefix("- `") {
            tests.insert((file.to_owned(), quoted(rest)));
        }
    }
    tests
}

/// (file, test) pairs that some system skips, from the sources of `src/`
/// and `tests/`.
fn skipped() -> BTreeSet<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
    let lib: Vec<&str> = lib.lines().collect();
    let mut tests = BTreeSet::new();
    for dir in ["src", "tests"] {
        for entry in std::fs::read_dir(root.join(dir)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let stem = path.file_stem().unwrap().to_str().unwrap();
            let code = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = code.lines().map(str::trim).collect();
            let module = format!("pub mod {stem};");
            let one_system = lines.iter().any(|l| l.starts_with("#![cfg(target_os"))
                || lib
                    .windows(2)
                    .any(|w| w[0].starts_with("#[cfg(target_os") && w[1] == module);
            for (i, line) in lines.iter().enumerate() {
                let Some(name) = line.strip_prefix("fn ") else {
                    continue;
                };
                let attributes = lines[..i].iter().rev().take_while(|l| l.starts_with("#["));
                let (mut test, mut ignored) = (false, false);
                for attribute in attributes {
                    test |= *attribute == "#[test]";
                    ignored |= attribute.starts_with("#[ignore")
                        || attribute.starts_with("#[cfg(target_os")
                        || attribute.starts_with("#[cfg(not(target_os");
                }
                if test && (one_system || ignored) {
                    let name = name.split('(').next().unwrap().to_owned();
                    tests.insert((format!("{dir}/{stem}.rs"), name));
                }
            }
        }
    }
    tests
}

#[test]
fn every_skipped_test_is_listed_with_its_reason() {
    let skipped = skipped();
    assert!(skipped.len() > 20, "the scan finds the skipped tests");
    assert_eq!(listed(), skipped);
}
