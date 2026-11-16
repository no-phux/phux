//! Binary-level tests for `phux new --empty` with no server running
//! (ADR-0105). The server it starts carries no seed session, so the only
//! session is the empty one that was asked for, including when that session
//! is named `default`, the name an ordinary auto-spawn would have seeded.
//!
//! Like the other binary e2e tests these are `#[ignore]`: each spawns a real
//! daemon. Run via `just e2e`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use crate::common;

use std::path::{Path, PathBuf};

/// The session names `phux ls --json` reports.
fn session_names(socket: &Path) -> Vec<String> {
    let out = common::backstopped(socket, &["ls", "--json"]);
    assert!(
        out.success(),
        "phux ls --json failed.\nstderr: {}",
        out.stderr
    );
    let doc: serde_json::Value = serde_json::from_str(&out.stdout).expect("ls json");
    doc["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .map(|s| s["name"].as_str().expect("session name").to_owned())
        .collect()
}

/// A server auto-spawned by `phux new --empty --json -s NAME` on a fresh
/// socket, its pid captured so the guard can reap it.
fn parked_server(name: &str) -> (tempfile::TempDir, PathBuf, common::AutoSpawnedServer) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    let first = common::backstopped(&socket, &["new", "--empty", "--json", "-s", name]);
    let mut server = common::AutoSpawnedServer::new(crate::runner::phux_bin(), socket.clone());
    assert!(first.success(), "{first:?}");
    server.capture_pid();
    (dir, socket, server)
}

/// `phux new --empty --json -s NAME` against no server: the server it starts
/// holds exactly one session, the empty `NAME`.
fn new_empty_starts_only(name: &str) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    let out = common::backstopped(&socket, &["new", "--empty", "--json", "-s", name]);
    let mut server = common::AutoSpawnedServer::new(crate::runner::phux_bin(), socket.clone());
    assert!(
        out.success(),
        "phux new --empty must start a server and create {name:?}.\nstderr: {}",
        out.stderr
    );
    server.capture_pid();

    let doc: serde_json::Value = serde_json::from_str(&out.stdout).expect("new json");
    assert_eq!(doc["session"], name);
    assert_eq!(doc["empty"], true);
    assert!(doc["terminal_id"].is_null());
    assert_eq!(
        session_names(&socket),
        [name],
        "the auto-started server must not seed a session of its own"
    );

    drop(server);
    drop(dir);
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn new_empty_without_a_server_creates_only_the_requested_session() {
    new_empty_starts_only("parked");
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn new_empty_named_default_without_a_server_succeeds() {
    new_empty_starts_only("default");
}

/// A second `phux new --json` for a name in use fails with the `--json`
/// contract: empty stdout and one JSON error line (`session_exists`) on
/// stderr, for the empty and the seeded create alike.
#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn new_json_for_a_taken_name_is_a_json_error() {
    let (dir, socket, server) = parked_server("taken");

    for args in [
        &["new", "--empty", "--json", "-s", "taken"][..],
        &["new", "--json", "-s", "taken"][..],
    ] {
        let out = common::backstopped(&socket, args);
        assert_eq!(out.code, 1, "{args:?}: {out:?}");
        let doc = out.json_error();
        assert_eq!(doc["error"]["code"], "session_exists", "{doc}");
        assert_eq!(doc["exit_code"], 1, "{doc}");
        assert!(
            doc["remedy"]
                .as_str()
                .is_some_and(|r| r.contains("phux attach taken")),
            "{doc}"
        );
    }

    drop(server);
    drop(dir);
}

/// A seed pane that ends before its layout is stored (a command that exits
/// at once, or one that never executes) still makes `phux new --json`
/// answer success with the terminal id (#2403). The race is timing
/// dependent, so it is run many times.
#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn new_json_with_a_seed_that_ends_at_once_succeeds() {
    let (dir, socket, server) = parked_server("parked");

    for i in 0..40 {
        let name = format!("brief{i}");
        let seed = if i % 2 == 0 {
            "true"
        } else {
            "/nonexistent/phux-seed"
        };
        let out = common::backstopped(&socket, &["new", "--json", "-s", &name, "--", seed]);
        assert!(
            out.success(),
            "{name} ({seed}) must be created.\nstderr: {}",
            out.stderr
        );
        let doc: serde_json::Value = serde_json::from_str(&out.stdout).expect("new json");
        assert_eq!(doc["session"], name.as_str(), "{doc}");
        assert!(doc["terminal_id"].is_u64(), "{doc}");
    }

    drop(server);
    drop(dir);
}
