//! Child panes (`phux.parent/v1`): `phux spawn --parent`, and by default
//! the pane the spawn runs in, records the parent; `phux ls --json` lists it,
//! and it outlives the parent's exit.
//!
//! `#[ignore]`: it spawns a real server. Run via `just e2e`.

#![allow(clippy::expect_used, reason = "tests")]

use crate::common;

use std::process::{Output, Stdio};

use serde_json::Value;

fn phux(server: &common::ServerGuard, env: Option<&str>, args: &[&str]) -> Output {
    let mut cmd = common::phux_cmd(crate::runner::phux_bin());
    if let Some(pane) = env {
        cmd.env("PHUX_TERMINAL_ID", pane);
    }
    cmd.arg("--socket")
        .arg(&server.socket)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run phux")
}

fn json(server: &common::ServerGuard, env: Option<&str>, args: &[&str]) -> Value {
    let out = phux(server, env, args);
    assert!(out.status.success(), "`phux {}`: {out:?}", args.join(" "));
    serde_json::from_slice(&out.stdout).expect("JSON")
}

fn spawn(server: &common::ServerGuard, env: Option<&str>, flags: &[&str]) -> String {
    let mut args = vec!["spawn", "--json"];
    args.extend_from_slice(flags);
    args.extend_from_slice(&["--", "sleep", "600"]);
    format!("@{}", json(server, env, &args)["terminal_id"])
}

/// Each listed resource's `parent`, by id.
fn parent_of(server: &common::ServerGuard, pane: &str) -> Value {
    let listing = json(server, None, &["ls", "--json"]);
    let resources = listing["resources"].as_array().expect("resources");
    (resources.iter())
        .find(|r| r["id"] == pane)
        .map(|r| r["parent"].clone())
        .expect("the pane is listed")
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn a_spawned_pane_names_its_parent_and_keeps_it_when_the_parent_exits() {
    let server = common::ServerGuard::builder("child-panes")
        .seed_command("exec sleep 600")
        .start();
    let agent = spawn(&server, None, &[]);
    let explicit = spawn(&server, None, &["--parent", &agent]);
    let inherited = spawn(&server, Some(agent.trim_start_matches('@')), &[]);
    let orphan = spawn(
        &server,
        Some(agent.trim_start_matches('@')),
        &["--parent", "none"],
    );
    assert_eq!(parent_of(&server, &agent), Value::Null);
    assert_eq!(parent_of(&server, &explicit), agent.as_str());
    assert_eq!(parent_of(&server, &inherited), agent.as_str());
    assert_eq!(parent_of(&server, &orphan), Value::Null);

    let killed = phux(&server, None, &["kill", "--yes", &agent]);
    assert!(killed.status.success(), "{killed:?}");
    assert_eq!(
        parent_of(&server, &explicit),
        agent.as_str(),
        "closing the parent leaves the child naming it"
    );
}

/// phux-dsako.5: a spawn from inside a pane joins that pane's session and
/// starts in its directory, not the session touched last and the home
/// directory; one that outlives its pane still spawns, where it used to.
#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn a_spawn_from_a_pane_joins_its_session_and_directory() {
    let server = common::ServerGuard::builder("child-session")
        .seed_command("exec sleep 600")
        .start();
    let dir = tempfile::tempdir().expect("temp dir");
    let dir = dir.path().canonicalize().expect("canonical");
    let cwd = dir.to_str().expect("UTF-8");
    let sleep = ["--", "sleep", "600"];
    let new = |args: &[&str]| json(&server, None, &[&["new", "--json"], args, &sleep].concat());
    let alpha = new(&["-s", "alpha", "-c", cwd])["terminal_id"].to_string();
    new(&["-s", "beta"]);
    let show = |pane: &str| json(&server, None, &["resource", "show", pane, "--json"]);

    let child = show(&spawn(&server, Some(&alpha), &[]));
    assert_eq!(child["session"], "alpha", "{child}");
    assert_eq!(child["cwd"], cwd, "{child}");

    let outlived = show(&spawn(&server, Some("9999"), &[]));
    assert_ne!(outlived["session"], "alpha", "{outlived}");
}
