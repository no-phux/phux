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
use std::process::{Output, Stdio};

/// Run one `phux` verb against `socket`, with the idle backstop set so a
/// daemon this test fails to reap cannot outlive it.
fn phux(args: &[&str], socket: &Path) -> Output {
    let (key, value) = common::AutoSpawnedServer::IDLE_BACKSTOP;
    // Ahead of the verb: anything after a `--` is the seed's argv.
    common::phux_cmd(crate::runner::phux_bin())
        .arg("--socket")
        .arg(socket)
        .args(args)
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
    let mut server = common::AutoSpawnedServer::new(crate::runner::phux_bin(), socket.clone());
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

/// A second `phux new --json` for a name in use fails with the `--json`
/// contract: empty stdout and one JSON error line (`session_exists`) on
/// stderr, for the empty and the seeded create alike.
#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn new_json_for_a_taken_name_is_a_json_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    let first = phux(&["new", "--empty", "--json", "-s", "taken"], &socket);
    let mut server = common::AutoSpawnedServer::new(crate::runner::phux_bin(), socket.clone());
    assert!(first.status.success(), "{first:?}");
    server.capture_pid();

    for args in [
        &["new", "--empty", "--json", "-s", "taken"][..],
        &["new", "--json", "-s", "taken"][..],
    ] {
        let out = phux(args, &socket);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let doc: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap_or_else(|err| {
            panic!("{args:?}: stderr is not one JSON line ({err}): {stderr}")
        });
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
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    let first = phux(&["new", "--empty", "--json", "-s", "parked"], &socket);
    let mut server = common::AutoSpawnedServer::new(crate::runner::phux_bin(), socket.clone());
    assert!(first.status.success(), "{first:?}");
    server.capture_pid();

    for i in 0..40 {
        let name = format!("brief{i}");
        let seed = if i % 2 == 0 {
            "true"
        } else {
            "/nonexistent/phux-seed"
        };
        let out = phux(&["new", "--json", "-s", &name, "--", seed], &socket);
        assert!(
            out.status.success(),
            "{name} ({seed}) must be created.\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("new json");
        assert_eq!(doc["session"], name.as_str(), "{doc}");
        assert!(doc["terminal_id"].is_u64(), "{doc}");
    }

    drop(server);
    drop(dir);
}
