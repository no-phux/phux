//! What works here, end to end: what a phone reads over `HOST_CHECKLIST` is
//! the `checklist` of the host's own `phux doctor --json`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use common::ServerGuard;

/// Same items, same order, same verdicts over the wire as from the CLI.
#[test]
#[ignore = "spawns real phux processes; runs in the e2e lane"]
fn the_wire_checklist_is_doctors_checklist() {
    let server = ServerGuard::start("checklist");
    let (_, stdout, stderr) = server.run(&["doctor", "--json"]);
    let doctor: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|err| panic!("{err}: {stdout}\n{stderr}"));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let wire = rt
        .block_on(phux_client::checklist::fetch(&server.socket))
        .unwrap();
    let summary = |doc: &serde_json::Value| {
        doc["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| (item["id"].clone(), item["status"].clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(wire["schema_version"], 1);
    assert_eq!(summary(&wire), summary(&doctor["checklist"]), "{wire}");
    let ids: Vec<_> = summary(&wire).into_iter().map(|(id, _)| id).collect();
    assert_eq!(
        ids,
        [
            "agent_status",
            "notifications",
            "remote_reach",
            "file_transfer",
            "restore",
            "skill"
        ]
    );
    // The server answered, so its own items are known.
    assert_eq!(wire["items"][3]["status"], "ok", "{wire}");
}
