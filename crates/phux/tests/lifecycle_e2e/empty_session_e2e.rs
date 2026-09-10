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

#[path = "../common/mod.rs"]
mod common;

use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Path to the freshly-built `phux` binary, injected by cargo.
const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// Run one `phux` verb against `socket`, with the idle backstop set so a
/// daemon this test fails to reap cannot outlive it.
fn phux(args: &[&str], socket: &Path) -> Output {
    let (key, value) = common::AutoSpawnedServer::IDLE_BACKSTOP;
    Command::new(PHUX)
        .args(args)
        .arg("--socket")
        .arg(socket)
        .env(key, value)
        .stdin(Stdio::null())
        .output()
        .expect("run phux")
}

/// The session names `phux ls --json` reports.
fn session_names(socket: &Path) -> Vec<String> {
    let out = phux(&["ls", "--json"], socket);
    assert!(
        out.status.success(),
        "phux ls --json failed.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("ls json");
    doc["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .map(|s| s["name"].as_str().expect("session name").to_owned())
        .collect()
}

/// `phux new --empty --json -s NAME` against no server: the server it starts
/// holds exactly one session, the empty `NAME`.
fn new_empty_starts_only(name: &str) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    let out = phux(&["new", "--empty", "--json", "-s", name], &socket);
    let mut server = common::AutoSpawnedServer::new(PHUX, socket.clone());
    assert!(
        out.status.success(),
        "phux new --empty must start a server and create {name:?}.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    server.capture_pid();

    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("new json");
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
