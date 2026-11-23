//! `granite format`, run as the binary on a real file.

use granite_base::checksum::checksum;
use granite_base::constants::FORMAT;
use granite_io::Zone;
use granite_storage::layout::{file_size, zones};
use granite_storage::superblock::{self, Identity, OpenError};
use std::path::PathBuf;
use std::process::{Command, Output};

const A: Identity = Identity {
    cluster: 0xA,
    group: 7,
    replica: 2,
};

/// A path in the temporary directory, removed with everything under it on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("granite-format-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn granite_format(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_granite"))
        .arg("format")
        .args(args)
        .output()
        .unwrap()
}

fn format_as(identity: Identity, path: &str) -> Output {
    let (cluster, group, replica) = (
        identity.cluster.to_string(),
        identity.group.to_string(),
        identity.replica.to_string(),
    );
    granite_format(&[
        "--cluster",
        &cluster,
        "--group",
        &group,
        "--replica",
        &replica,
        path,
    ])
}

fn hash(path: &PathBuf) -> u128 {
    checksum(&std::fs::read(path).unwrap())
}

#[test]
fn format_creates_the_fixed_zones_and_the_superblock() {
    let scratch = Scratch::new("creates");
    let path = scratch.0.join("data");
    let out = format_as(A, path.to_str().unwrap());
    assert!(out.status.success(), "{out:?}");

    let file = std::fs::read(&path).unwrap();
    assert_eq!(file.len() as u64, file_size(0));
    let sb = zones(0)[Zone::Superblock as usize];
    let sb = &file[sb.offset as usize..sb.end() as usize];
    assert_eq!(superblock::open(sb, A), Ok(()));
    assert_eq!(
        superblock::open(sb, Identity { cluster: 0xB, ..A }),
        Err(OpenError::Cluster { stored: 0xA })
    );
    assert!(
        file[sb.len()..].iter().all(|&b| b == 0),
        "the WAL zones are zero"
    );
}

#[test]
fn format_refuses_an_existing_file_and_changes_nothing() {
    let scratch = Scratch::new("refuses");
    let data = scratch.0.join("data");
    assert!(format_as(A, data.to_str().unwrap()).status.success());
    // Not a data file, and shorter than one: still refused, not extended.
    let other = scratch.0.join("other");
    let pattern: Vec<u8> = (0..FORMAT.sector_size).map(|i| i as u8).collect();
    std::fs::write(&other, &pattern).unwrap();

    for path in [&data, &other] {
        let before = hash(path);
        let out = format_as(Identity { cluster: 0xB, ..A }, path.to_str().unwrap());
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("exists"),
            "{out:?}"
        );
        assert_eq!(hash(path), before, "{}", path.display());
    }
}

#[test]
fn format_with_bad_arguments_creates_nothing() {
    let scratch = Scratch::new("arguments");
    let path = scratch.0.join("data");
    let p = path.to_str().unwrap();
    for args in [
        &["--cluster", "1", "--group", "2", p][..],
        &["--cluster", "1", "--group", "2", "--replica", "256", p],
        &[
            "--cluster",
            "1",
            "--group",
            "2",
            "--replica",
            "3",
            "--replica",
            "3",
            p,
        ],
        &["--cluster", "x", "--group", "2", "--replica", "3", p],
        &["--cluster", "1", "--group", "2", "--replica", "3"],
    ] {
        assert_eq!(granite_format(args).status.code(), Some(2), "{args:?}");
    }
    assert!(std::fs::read_dir(&scratch.0).unwrap().next().is_none());
}
