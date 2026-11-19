//! No method of the `Io` traits takes or returns a type from `std::fs`,
//! `std::net` or `std::time` (task T0.3.1). Every type in their signatures is
//! defined or imported in `src/traits.rs`, so the check reads that file: it
//! fails on any path into those modules (or their `core` twins) and on any
//! name of one of their types, outside comments.

const TRAITS: &str = include_str!("../src/traits.rs");

const MODULES: [&str; 5] = [
    "std::fs",
    "std::net",
    "std::time",
    "core::net",
    "core::time",
];

#[rustfmt::skip]
const TYPES: [&str; 27] = [
    // std::fs
    "File", "OpenOptions", "Metadata", "DirEntry", "ReadDir", "Permissions", "FileType",
    "FileTimes", "DirBuilder",
    // std::net
    "TcpStream", "TcpListener", "UdpSocket", "SocketAddr", "SocketAddrV4", "SocketAddrV6",
    "IpAddr", "Ipv4Addr", "Ipv6Addr", "Shutdown", "ToSocketAddrs", "Incoming",
    // std::time
    "Duration", "Instant", "SystemTime", "SystemTimeError", "UNIX_EPOCH", "TryFromFloatSecsError",
];

/// Every forbidden path or type name in `source`, with its line number.
fn violations(source: &str) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    for (number, line) in source.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        for module in MODULES {
            if code.contains(module) {
                found.push((number + 1, module));
            }
        }
        let words = code.split(|c: char| !(c.is_alphanumeric() || c == '_'));
        for word in words {
            if let Some(name) = TYPES.iter().find(|&&name| name == word) {
                found.push((number + 1, *name));
            }
        }
    }
    found
}

#[test]
fn trait_signatures_name_no_std_io_or_time_type() {
    for name in [
        "trait Storage",
        "trait Network",
        "trait Time",
        "trait Random",
        "trait Io",
    ] {
        assert!(TRAITS.contains(name), "src/traits.rs defines `{name}`");
    }
    assert_eq!(violations(TRAITS), []);
}

/// The injected bugs: each of these must be caught.
#[test]
fn the_check_catches_injected_std_types() {
    let injected = [
        "    fn monotonic(&self) -> std::time::Instant;",
        "use std::net::SocketAddr;",
        "    fn send(&mut self, to: SocketAddr, buffer: Message);",
        "    fn open(&mut self, file: File) -> Result<(), Full<()>>;",
        "    fn sleep(&mut self, how_long: core::time::Duration);",
        "use std::time::{self as clock};",
        "use std::{fs, time::Instant as Clock};",
    ];
    for line in injected {
        assert!(!violations(line).is_empty(), "missed: {line}");
    }
    // Comments may name them, and longer identifiers are not matches.
    assert_eq!(
        violations("/// Not a `std::time::Instant`.\nstruct Filed;"),
        []
    );
}
