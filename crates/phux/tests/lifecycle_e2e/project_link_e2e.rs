//! Binary-level tests for `phux project link` / `unlink`: the link lands on
//! the session's own server as `project.link`, `phux ls --json` reports it
//! as `project_link`, and the CLI keeps the `--json` and exit-code contract
//! for a missing session and a pane target.
//!
//! Like the other binary e2e tests these are `#[ignore]`: each spawns a real
//! daemon. Run via `just e2e`. A server that predates `SESSION_PROJECT`
//! (`server_too_old`) cannot be spawned from this checkout, so that refusal
//! is not covered here.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use crate::common::{self, Out};

use std::path::Path;

fn phux(args: &[&str], socket: &Path) -> Out {
    let (key, value) = common::AutoSpawnedServer::IDLE_BACKSTOP;
    Out::of(
        common::phux_cmd(crate::runner::phux_bin())
            .args(args)
            .arg("--socket")
            .arg(socket)
            .env(key, value),
    )
}

fn json_error(out: &Out) -> serde_json::Value {
    assert!(out.stdout.is_empty(), "{out:?}");
    let stderr = &out.stderr;
    serde_json::from_str(stderr.trim())
        .unwrap_or_else(|err| panic!("stderr is not one JSON line ({err}): {stderr}"))
}

/// Each session's `project_link` in `phux ls --json`.
fn links(socket: &Path) -> Vec<(String, serde_json::Value)> {
    let doc = phux(&["ls", "--json"], socket).json();
    doc["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap().to_owned(),
                s["project_link"].clone(),
            )
        })
        .collect()
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn project_link_sets_trims_reports_and_clears_the_link() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    let created = phux(&["new", "--json", "-s", "work"], &socket).json();
    let mut server = common::AutoSpawnedServer::new(crate::runner::phux_bin(), socket.clone());
    server.capture_pid();
    let pane = format!("@{}", created["terminal_id"].as_u64().expect("seed pane"));
    assert!(phux(&["new", "--empty", "--json", "-s", "notes"], &socket).success());

    let linked = phux(&["project", "link", "notes", "  phux ", "--json"], &socket).json();
    assert_eq!(linked["schema_version"], 1, "{linked}");
    assert_eq!(linked["verb"], "project link", "{linked}");
    assert_eq!(linked["session"], "notes", "{linked}");
    assert_eq!(linked["project"], "phux", "the name is trimmed: {linked}");
    assert!(linked["key"].as_str().is_some_and(|k| k.starts_with('^')));
    assert!(linked["revision"].as_u64().is_some(), "{linked}");
    assert_eq!(
        links(&socket),
        [
            ("notes".to_owned(), serde_json::json!("phux")),
            ("work".to_owned(), serde_json::Value::Null),
        ]
    );

    let missing = phux(&["project", "link", "gone", "phux", "--json"], &socket);
    assert_eq!(missing.code, 1, "{missing:?}");
    assert_eq!(json_error(&missing)["error"]["code"], "no_such_target");

    let on_pane = phux(&["project", "link", &pane, "phux", "--json"], &socket);
    assert_eq!(on_pane.code, 1, "{on_pane:?}");
    assert_eq!(json_error(&on_pane)["error"]["code"], "wrong_resource_kind");

    let blank = phux(&["project", "link", "notes", " ", "--json"], &socket);
    assert_eq!(blank.code, 2, "{blank:?}");
    assert_eq!(json_error(&blank)["error"]["code"], "invalid_argument");

    let unlinked = phux(&["project", "unlink", "notes", "--json"], &socket).json();
    assert_eq!(unlinked["verb"], "project unlink", "{unlinked}");
    assert_eq!(unlinked["project"], serde_json::Value::Null, "{unlinked}");
    assert!(links(&socket).iter().all(|(_, link)| link.is_null()));

    drop(server);
    drop(dir);
}
