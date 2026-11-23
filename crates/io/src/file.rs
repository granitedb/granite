//! Creating the data file, the one step before a backend opens it.

use granite_base::constants::MIB;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

/// Creates the data file at `path`, or fails with `AlreadyExists` and touches
/// nothing if anything is already there. Writes zeros up to `size`, so the
/// space is allocated now and a later write cannot run out of room, then
/// `head` at offset 0, and makes the data, the size and the directory entry
/// durable. If any step fails, it removes the partly written file; `head` goes
/// last and durable after the zeros, so a file left by a crash has no head.
pub fn create(path: &Path, size: u64, head: &[u8]) -> io::Result<()> {
    assert!(head.len() as u64 <= size, "head fits in the file");
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let written = fill(&file, size, head).and_then(|()| {
        let directory = path.parent().filter(|p| !p.as_os_str().is_empty());
        File::open(directory.unwrap_or(Path::new(".")))?.sync_all()
    });
    if written.is_err() {
        drop(file);
        let _ = std::fs::remove_file(path);
    }
    written
}

fn fill(file: &File, size: u64, head: &[u8]) -> io::Result<()> {
    let zeros = vec![0; MIB as usize];
    let mut offset = head.len() as u64;
    while offset < size {
        let len = (size - offset).min(MIB);
        file.write_all_at(&zeros[..len as usize], offset)?;
        offset += len;
    }
    file.sync_all()?;
    file.write_all_at(head, 0)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("granite-io-file-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn creates_the_head_then_zeros() {
        let dir = scratch("creates");
        let path = dir.join("data");
        let size = 3 * MIB + 1;
        create(&path, size, b"head").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(bytes.len() as u64, size);
        assert_eq!(&bytes[..4], b"head");
        assert!(bytes[4..].iter().all(|&b| b == 0));
    }

    #[test]
    fn an_existing_file_is_refused_and_left_unchanged() {
        let dir = scratch("existing");
        let path = dir.join("data");
        std::fs::write(&path, b"existing").unwrap();
        let error = create(&path, MIB, b"head").unwrap_err();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(bytes, b"existing");
    }
}
