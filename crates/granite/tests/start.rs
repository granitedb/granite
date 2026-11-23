//! `granite start` and its choice of I/O backend, run as the binary.

use std::process::{Command, Output};

fn granite_start(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_granite"))
        .arg("start")
        .args(args)
        .output()
        .unwrap()
}

const WARNING: &str = "warning: the portable I/O backend is for development only and \
     unsupported in production";

/// On macOS a node starts on the portable backend, by default or by name, and
/// says it is unsupported in production. Linux builds have no such backend:
/// selecting it is rejected, and nothing warns.
#[test]
fn the_portable_backend_warns_and_is_absent_on_linux() {
    for args in [&[][..], &["--io", "portable"]] {
        let output = granite_start(args);
        let stderr = String::from_utf8(output.stderr).unwrap();
        let rejected = stderr.contains("unknown I/O backend 'portable'");
        if cfg!(target_os = "linux") {
            assert_eq!(rejected, !args.is_empty(), "{args:?}: {stderr}");
            assert!(!stderr.contains(WARNING), "{stderr}");
        } else {
            assert!(!rejected && stderr.contains(WARNING), "{args:?}: {stderr}");
        }
        assert_eq!(output.status.code(), Some(2), "{stderr}");
    }
}

/// The io_uring backend is not in a macOS build either.
#[test]
fn only_the_system_backend_is_accepted() {
    let other = if cfg!(target_os = "linux") {
        "portable"
    } else {
        "uring"
    };
    let output = granite_start(&["--io", other]);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(&format!("unknown I/O backend '{other}'")),
        "{stderr}"
    );
    assert!(!stderr.contains(WARNING), "{stderr}");
    assert_eq!(output.status.code(), Some(2));
}
