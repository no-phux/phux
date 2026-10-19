//! `phux download` exercised as a real CLI process on disposable servers. The
//! `#[ignore]`d case runs in `just e2e`.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod common;
#[path = "common/runner.rs"]
mod runner;

use common::ServerGuard;
use serde_json::Value;

#[test]
fn download_help_and_malformed_arguments_do_not_connect_or_write() {
    let help = common::phux_cmd(runner::phux_bin())
        .args(["download", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    for expected in [
        "--force",
        "--timeout",
        "--json",
        "--remote",
        "SHA-256",
        "HOST:/path",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("taken");
    std::fs::write(&existing, b"keep").unwrap();
    for options in [
        vec!["--timeout", "0"],
        vec!["--timeout", "3601"],
        vec!["--remote", "mini"],
        vec!["--bogus"],
    ] {
        let out = common::phux_cmd(runner::phux_bin())
            .args([
                "--socket",
                "/nonexistent/download-test.sock",
                "download",
                "/nonexistent/source",
                "--json",
            ])
            .args(options)
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !err.contains("no server"),
            "connected before validation: {err}"
        );
    }
    // A relative source, a host prefix on a relative path, and an occupied
    // local path are refused before any connection; the occupied file is
    // untouched and no part file appears.
    for (source, local) in [
        (
            "relative/deck.pptx",
            dir.path().join("out").display().to_string(),
        ),
        (
            "mini:relative/deck.pptx",
            dir.path().join("out").display().to_string(),
        ),
        ("/nonexistent/source", existing.display().to_string()),
    ] {
        let out = common::phux_cmd(runner::phux_bin())
            .args([
                "--socket",
                "/nonexistent/download-test.sock",
                "download",
                source,
                &local,
            ])
            .output()
            .unwrap();
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !err.contains("no server"),
            "connected before validation: {err}"
        );
    }
    assert_eq!(std::fs::read(&existing).unwrap(), b"keep");
    assert!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .all(|entry| entry.unwrap().file_name() == "taken"),
        "a refusal must leave nothing behind"
    );
}

#[test]
#[ignore = "spawns a real private server; runs in the e2e lane"]
fn download_pulls_multichunk_files_verifies_them_and_respects_force() {
    let dir = tempfile::tempdir().unwrap();
    let server = ServerGuard::builder("download-pull").start();
    let source = dir.path().join("deck.pptx");
    let data = vec![0xa5; phux_protocol::wire::frame::MAX_FILE_UPLOAD_CHUNK + 321];
    std::fs::write(&source, &data).unwrap();
    let dest = dir.path().join("landed");
    std::fs::create_dir(&dest).unwrap();
    let before: Value = serde_json::from_str(&server.success(&["ls", "--json"])).unwrap();

    // Into a directory: the remote file name is kept.
    let output = server.success(&[
        "download",
        source.to_str().unwrap(),
        dest.to_str().unwrap(),
        "--json",
    ]);
    let doc: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(doc["receipt"]["outcome"], "completed", "{doc}");
    assert!(doc["receipt"]["host"].is_null());
    let landed = dest.join("deck.pptx");
    assert_eq!(doc["receipt"]["local_path"], landed.to_str().unwrap());
    assert_eq!(std::fs::read(&landed).unwrap(), data);
    assert_eq!(
        std::fs::read_dir(&dest).unwrap().count(),
        1,
        "no part file left"
    );

    // An existing destination is refused without --force and replaced with it.
    std::fs::write(&source, b"second").unwrap();
    let (status, output, error) = server.run(&[
        "download",
        source.to_str().unwrap(),
        landed.to_str().unwrap(),
    ]);
    assert_eq!(status, 2, "{error}");
    assert!(output.is_empty());
    assert!(error.contains("--force"), "{error}");
    assert_eq!(std::fs::read(&landed).unwrap(), data);
    let printed = server.success(&[
        "download",
        source.to_str().unwrap(),
        landed.to_str().unwrap(),
        "--force",
    ]);
    assert_eq!(printed.trim(), landed.to_str().unwrap());
    assert_eq!(std::fs::read(&landed).unwrap(), b"second");

    // An empty file lands as an empty file.
    std::fs::write(&source, []).unwrap();
    let empty = dir.path().join("empty.pptx");
    server.success(&[
        "download",
        source.to_str().unwrap(),
        empty.to_str().unwrap(),
    ]);
    assert_eq!(std::fs::read(&empty).unwrap(), Vec::<u8>::new());

    // A missing remote file is a refusal receipt that leaves nothing behind.
    let missing_dest = dir.path().join("never");
    let (status, output, warning) = server.run(&[
        "download",
        dir.path().join("missing").to_str().unwrap(),
        missing_dest.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(status, 1, "{warning}");
    let doc: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(doc["receipt"]["outcome"], "refused");
    assert!(
        doc["receipt"]["message"]
            .as_str()
            .unwrap()
            .contains("does not exist")
    );
    assert!(!missing_dest.exists());

    // A host prefix on a server that is not a hub is refused, not read locally.
    let (status, output, warning) = server.run(&[
        "download",
        &format!("mini:{}", source.display()),
        dir.path().join("hub-only").to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(status, 1, "{warning}");
    let doc: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(doc["receipt"]["outcome"], "refused");
    assert_eq!(doc["receipt"]["host"], "mini");
    assert!(!dir.path().join("hub-only").exists());

    let after: Value = serde_json::from_str(&server.success(&["ls", "--json"])).unwrap();
    assert_eq!(
        before, after,
        "download changed session/attached-client state"
    );
}
