//! Rejected: touching files outside the io crate.

use std::fs::File;
use std::path::Path;

pub fn load(path: &Path) -> Vec<u8> {
    if !path.exists() {
        return Vec::new();
    }
    let _file = File::open(path);
    std::fs::read(path).unwrap_or_default()
}
