//! Overflow checks and assertions stay enabled in the release profile
//! (document 06 section 3; `[profile.release]` in the root `Cargo.toml`).
//! `just check` runs this file a second time with `--release` and
//! `GRANITE_PROFILE=release` (the `test-release` recipe); then the test also
//! checks that its own binary was built in the release profile.

use std::hint::black_box;
use std::panic::catch_unwind;

#[test]
fn overflow_and_assertions_panic() {
    if std::env::var_os("GRANITE_PROFILE").is_some_and(|p| p == "release") {
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().and_then(|deps| deps.parent()).unwrap();
        assert!(
            dir.ends_with("release"),
            "{} is not a release build",
            exe.display()
        );
    }
    let overflow = catch_unwind(|| black_box(u8::MAX) + black_box(1));
    assert!(
        overflow.is_err(),
        "integer overflow did not panic: overflow checks are off"
    );
    let negation = catch_unwind(|| -black_box(i64::MIN));
    assert!(
        negation.is_err(),
        "negation overflow did not panic: overflow checks are off"
    );
    let assertion = catch_unwind(|| debug_assert!(black_box(false)));
    assert!(
        assertion.is_err(),
        "debug_assert! did not run: debug assertions are off"
    );
}
