//! Unseen marks (`phux.unseen/v1`): a bell nobody saw is listed by
//! `phux ls --json`, and `phux seen` clears it.
//!
//! `#[ignore]`: it spawns a real server. Run via `just e2e`.

#![allow(clippy::expect_used, reason = "tests")]

use crate::common;

use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

fn phux(server: &common::ServerGuard, args: &[&str]) -> Output {
    let out = common::phux_cmd(crate::runner::phux_bin())
        .arg("--socket")
        .arg(&server.socket)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run phux");
    assert!(out.status.success(), "`phux {}`: {out:?}", args.join(" "));
    out
}

fn json(server: &common::ServerGuard, args: &[&str]) -> Value {
    serde_json::from_slice(&phux(server, args).stdout).expect("JSON")
}

fn unseen(server: &common::ServerGuard, pane: &str) -> Option<String> {
    let listing = json(server, &["ls", "--json"]);
    let rows = listing["resources"].as_array().expect("resources").clone();
    let row = (rows.iter())
        .find(|r| r["id"] == pane)
        .expect("the pane is listed");
    row["unseen"].as_str().map(str::to_owned)
}

fn until(server: &common::ServerGuard, pane: &str, want: Option<&str>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while unseen(server, pane).as_deref() != want {
        assert!(Instant::now() < deadline, "{:?}", unseen(server, pane));
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn a_bell_nobody_saw_is_listed_until_it_is_marked_seen() {
    let server = common::ServerGuard::builder("seen-marks")
        .seed_command("exec sleep 600")
        .start();
    let script = "printf '\\a'; exec sleep 600";
    let spawned = json(&server, &["spawn", "--json", "--", "sh", "-c", script]);
    let pane = format!("@{}", spawned["terminal_id"]);
    until(&server, &pane, Some("bell"));
    // The bell's output burst settles first, or its `idle` would mark again.
    std::thread::sleep(Duration::from_secs(1));
    let cleared = json(&server, &["seen", "--json", &pane]);
    assert_eq!(cleared["terminals"][0], pane.as_str());
    assert_eq!(unseen(&server, &pane), None);
}
