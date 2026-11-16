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

use crate::common;

use std::path::Path;

/// Each session's `project_link` in `phux ls --json`.
fn links(socket: &Path) -> Vec<(String, serde_json::Value)> {
    let doc = common::backstopped(socket, &["ls", "--json"]).json();
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
    let socket = &dir.path().join("phux.sock");
    let created = common::backstopped(socket, &["new", "--json", "-s", "work"]).json();
    let mut server = common::AutoSpawnedServer::new(crate::runner::phux_bin(), socket.clone());
    server.capture_pid();
    let pane = format!("@{}", created["terminal_id"].as_u64().expect("seed pane"));
    assert!(common::backstopped(socket, &["new", "--empty", "--json", "-s", "notes"]).success());

    let linked =
        common::backstopped(socket, &["project", "link", "notes", "  phux ", "--json"]).json();
    assert_eq!(linked["schema_version"], 1, "{linked}");
    assert_eq!(linked["verb"], "project link", "{linked}");
    assert_eq!(linked["session"], "notes", "{linked}");
    assert_eq!(linked["project"], "phux", "the name is trimmed: {linked}");
    assert!(linked["key"].as_str().is_some_and(|k| k.starts_with('^')));
    assert!(linked["revision"].as_u64().is_some(), "{linked}");
    assert_eq!(
        links(socket),
        [
            ("notes".to_owned(), serde_json::json!("phux")),
            ("work".to_owned(), serde_json::Value::Null),
        ]
    );

    let missing = common::backstopped(socket, &["project", "link", "gone", "phux", "--json"]);
    assert_eq!(missing.code, 1, "{missing:?}");
    assert_eq!(missing.json_error()["error"]["code"], "no_such_target");

    let on_pane = common::backstopped(socket, &["project", "link", &pane, "phux", "--json"]);
    assert_eq!(on_pane.code, 1, "{on_pane:?}");
    assert_eq!(on_pane.json_error()["error"]["code"], "wrong_resource_kind");

    let blank = common::backstopped(socket, &["project", "link", "notes", " ", "--json"]);
    assert_eq!(blank.code, 2, "{blank:?}");
    assert_eq!(blank.json_error()["error"]["code"], "invalid_argument");

    let unlinked = common::backstopped(socket, &["project", "unlink", "notes", "--json"]).json();
    assert_eq!(unlinked["verb"], "project unlink", "{unlinked}");
    assert_eq!(unlinked["project"], serde_json::Value::Null, "{unlinked}");
    assert!(links(socket).iter().all(|(_, link)| link.is_null()));

    drop(server);
    drop(dir);
}
