//! Slot tabs end to end (`docs/organization.md#slot-tabs`): three terminals
//! made one slot with `phux slot`, its default moved, and the slot as the
//! server stores it read back before and after a cold restart.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::hermetic::{NO_ENV, await_success, json_doc, prepare_dirs, start_server};

/// The org store a server on `dir/s.sock` keeps.
fn store(dir: &Path) -> PathBuf {
    fn walk(at: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(at).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.to_string_lossy().ends_with("v1.sqlite3")
                && path.to_string_lossy().contains("/org/")
            {
                out.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(&dir.join("state"), &mut found);
    found.pop().expect("an org store")
}

/// Every stored slot with tabs, as (tab count, `shown_at`).
fn stored_slots(dir: &Path) -> Vec<(usize, u64)> {
    fn collect(node: &serde_json::Value, out: &mut Vec<(usize, u64)>) {
        if let Some(leaf) = node.get("leaf") {
            if let Some(tabs) = leaf["tabs"].as_array() {
                out.push((tabs.len(), leaf["shown_at"].as_u64().unwrap_or(0)));
            }
        } else if let Some(split) = node.get("split") {
            collect(&split["left"], out);
            collect(&split["right"], out);
        }
    }
    let db = rusqlite::Connection::open(store(dir)).expect("open the store");
    let mut rows = db.prepare("SELECT record FROM arrangements").unwrap();
    let records: Vec<String> = rows
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut slots = Vec::new();
    for record in records {
        let arrangement: serde_json::Value = serde_json::from_str(&record).unwrap();
        for group in arrangement["groups"].as_array().unwrap() {
            collect(&group["tree"], &mut slots);
        }
    }
    slots
}

/// `args` aimed at the test's own server.
fn on<'a>(socket: &'a str, args: &[&'a str]) -> Vec<&'a str> {
    [&["--socket", socket][..], args].concat()
}

fn terminal(dir: &Path, args: &[&str]) -> String {
    let id = json_doc(&await_success(dir, args, NO_ENV))["terminal_id"].clone();
    format!("@{id}")
}

#[test]
#[ignore = "spawns a real server; runs in the e2e lane"]
fn a_three_tab_slot_survives_a_server_restart() {
    let dir = TempDir::new().expect("tempdir");
    let dir = dir.path();
    prepare_dirs(dir);
    let socket = dir.join("s.sock");
    let socket = socket.to_str().expect("utf-8 path");
    let server = start_server(dir, &[], &[]);

    let a = terminal(
        dir,
        &on(
            socket,
            &["new", "--json", "-s", "work", "--", "sh", "-c", "sleep 600"],
        ),
    );
    let b = terminal(
        dir,
        &on(
            socket,
            &[
                "spawn",
                "--json",
                "--target",
                &a,
                "--",
                "sh",
                "-c",
                "sleep 600",
            ],
        ),
    );
    let c = terminal(
        dir,
        &on(
            socket,
            &[
                "spawn",
                "--json",
                "--target",
                &a,
                "--",
                "sh",
                "-c",
                "sleep 600",
            ],
        ),
    );
    await_success(dir, &on(socket, &["slot", "add", &b, "--to", &a]), NO_ENV);
    let added = json_doc(&await_success(
        dir,
        &on(socket, &["slot", "add", &c, "--to", &a, "--json"]),
        NO_ENV,
    ));
    assert_eq!(added["operation"], "slot-add", "{added}");
    assert_eq!(stored_slots(dir), [(2, 0)], "one slot: a, then b and c");
    await_success(dir, &on(socket, &["slot", "next", &a]), NO_ENV);
    assert_eq!(stored_slots(dir), [(2, 1)], "the default moved on to b");

    drop(server);
    let _server = start_server(dir, &[], &[]);
    await_success(dir, &on(socket, &["ls"]), NO_ENV);
    assert_eq!(stored_slots(dir), [(2, 1)], "the slot survives the restart");
}
