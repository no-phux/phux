//! The real binary's answer to a server about to re-exec into it: a released
//! blob passes, and one it could not resume is refused before anything
//! irreversible. The server's side is in `runtime/upgrade/validate.rs`.

#![allow(clippy::expect_used, reason = "tests")]

use std::process::{Command, Output, Stdio};

/// Run the candidate as the upgrading server does, with `blob` on stdin.
fn preflight(blob: Stdio) -> Output {
    let phux = std::env::var("NEXTEST_BIN_EXE_phux")
        .or_else(|_| std::env::var("CARGO_BIN_EXE_phux"))
        .expect("the test runner exports the phux binary");
    Command::new(phux)
        .arg("--version")
        .env("PHUX_PROBE_UPGRADE_BLOB", "1")
        .stdin(blob)
        .output()
        .expect("run phux")
}

fn stdin_of(bytes: &[u8]) -> Stdio {
    use std::io::{Seek as _, Write as _};

    let mut file = tempfile::tempfile().expect("temp file");
    file.write_all(bytes).expect("write");
    file.rewind().expect("rewind");
    file.into()
}

#[test]
fn a_released_blob_is_answered_as_readable() {
    let fixture = std::path::Path::new(
        &std::env::var("CARGO_MANIFEST_DIR").expect("the test runner exports the manifest dir"),
    )
    .join("../phux-server/tests/fixtures/upgrade-blob/0001.json");
    let output = preflight(std::fs::File::open(fixture).expect("fixture").into());
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout.trim_ascii(), b"upgrade-blob-readable");
}

#[test]
fn a_blob_from_a_newer_format_is_refused_with_the_reason() {
    let output = preflight(stdin_of(br#"{"version": 4294967295}"#));
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported upgrade blob version"),
        "{stderr}"
    );
}
