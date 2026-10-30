//! Fleet filters preserve level reads and full-inventory addressability.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

#[path = "../common/mod.rs"]
mod common;

use std::time::{Duration, Instant};

use common::ServerGuard;

/// How long to wait for the shell's live pane fields to agree on both reads.
const PANE_SETTLE_DEADLINE: Duration = Duration::from_secs(5);
const PANE_SETTLE_POLL: Duration = Duration::from_millis(50);

/// Fields a shell can still be changing between two level reads. `title` is
/// the OSC title the prompt paints; `cwd` is the process directory.
const LIVE_PANE_FIELDS: &[&str] = &["title", "cwd"];

const FILTERED_LIST: &[&str] = &[
    "list",
    "--state",
    "done",
    "--state",
    "blocked",
    "--query",
    "WORK reviewer",
    "--json",
];

fn agent(server: &ServerGuard, args: &[&str]) -> String {
    let mut command = vec!["agent"];
    command.extend_from_slice(args);
    server.success(&command)
}

fn agent_json(server: &ServerGuard, args: &[&str]) -> serde_json::Value {
    serde_json::from_str(&agent(server, args)).expect("agent JSON")
}

/// Drop live shell fields so a mismatch can be classified as those fields alone.
fn without_live_pane_fields(agents: &serde_json::Value) -> serde_json::Value {
    let mut copy = agents.clone();
    if let Some(rows) = copy.as_array_mut() {
        for row in rows {
            if let Some(object) = row.as_object_mut() {
                for field in LIVE_PANE_FIELDS {
                    object.remove(*field);
                }
            }
        }
    }
    copy
}

fn agents_differ_only_by_live_pane_fields(
    left: &serde_json::Value,
    right: &serde_json::Value,
) -> bool {
    let (Some(left_rows), Some(right_rows)) = (left.as_array(), right.as_array()) else {
        return false;
    };
    left_rows.len() == right_rows.len()
        && without_live_pane_fields(left) == without_live_pane_fields(right)
}

/// `agent show` and the filtered `agent list` once their records match in full.
///
/// The session shell paints an OSC title when its prompt is drawn, and its
/// process directory can still be moving at the same time. Those two level
/// reads can straddle that, so the records differ only in `title` (`null`
/// against `user@host: ~`) or `cwd`. Poll until one pair agrees, including
/// both fields. Any other difference fails immediately. A shell that never
/// paints a title, and whose directory is already still, agrees on the first
/// pair.
fn show_and_filtered_list(server: &ServerGuard) -> (serde_json::Value, serde_json::Value) {
    let deadline = Instant::now() + PANE_SETTLE_DEADLINE;
    loop {
        let shown = agent_json(server, &["show", "work", "--json"]);
        let filtered = agent_json(server, FILTERED_LIST);
        if filtered["agents"] == shown["agents"] {
            return (shown, filtered);
        }
        let list_agents = &filtered["agents"];
        let show_agents = &shown["agents"];
        assert!(
            agents_differ_only_by_live_pane_fields(list_agents, show_agents),
            "filtered list and agent show differ beyond live pane fields {LIVE_PANE_FIELDS:?}\n\
             list: {list_agents}\n\
             show: {show_agents}"
        );
        assert!(
            Instant::now() < deadline,
            "pane title and cwd did not settle across agent show and agent list within {PANE_SETTLE_DEADLINE:?}\n\
             list: {list_agents}\n\
             show: {show_agents}"
        );
        std::thread::sleep(PANE_SETTLE_POLL);
    }
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn agent_list_filters_preserve_record_and_addressability() {
    let server = ServerGuard::start("agent-list");
    agent(
        &server,
        &[
            "set", "work", "--name", "reviewer", "--kind", "claude", "--state", "blocked",
        ],
    );
    let (shown, filtered) = show_and_filtered_list(&server);
    assert_eq!(filtered["agents"], shown["agents"]);
    let empty = agent(&server, &["list", "--state", "working", "--json"]);
    let empty: serde_json::Value = serde_json::from_str(&empty).expect("empty list JSON");
    assert_eq!(empty["schema_version"], 1);
    assert_eq!(empty["agents"], serde_json::json!([]));
    let dash_query = agent(&server, &["list", "--query=--state", "--json"]);
    let dash_query: serde_json::Value = serde_json::from_str(&dash_query).expect("dash query JSON");
    assert_eq!(dash_query["agents"], serde_json::json!([]));
    assert_eq!(
        agent(&server, &["list", "--query", "no-such-agent"]).trim(),
        "No panes match the agent filters."
    );
    // A filtered-out duplicate still makes `%reviewer` ambiguous.
    let spawned = server.success(&["spawn", "--json", "--", "/bin/sh"]);
    let spawned: serde_json::Value = serde_json::from_str(&spawned).expect("spawn JSON");
    let other = format!("@{}", spawned["terminal_id"].as_u64().expect("spawned id"));
    agent(
        &server,
        &[
            "set", &other, "--name", "reviewer", "--kind", "claude", "--state", "done",
        ],
    );
    let filtered = agent(&server, &["list", "--state", "blocked", "--json"]);
    let filtered: serde_json::Value =
        serde_json::from_str(&filtered).expect("filtered duplicate JSON");
    assert_eq!(filtered["agents"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["agents"][0]["address"], serde_json::Value::Null);
    assert_eq!(
        filtered["agents"][0]["address_refusal"],
        "selector_not_single"
    );
    let both = agent(
        &server,
        &["list", "--state", "blocked", "--state", "done", "--json"],
    );
    let both: serde_json::Value = serde_json::from_str(&both).expect("both states JSON");
    assert_eq!(both["agents"].as_array().unwrap().len(), 2);
}

#[test]
fn agent_list_rejects_invalid_state_and_documents_filters() {
    let output = common::phux_cmd(crate::runner::phux_bin())
        .args(["agent", "list", "--state", "sleeping"])
        .output()
        .expect("invalid filter");
    assert_eq!(
        output.status.code(),
        Some(2),
        "invalid state must fail parsing before any server access"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("sleeping"));
    let help = common::phux_cmd(crate::runner::phux_bin())
        .args(["agent", "list", "--help"])
        .output()
        .expect("list help");
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("--state"));
    assert!(help.contains("--query"));
}
