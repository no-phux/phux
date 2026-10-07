//! Binary-level acceptance for the resource noun and the durability flags
//! (PHA-406 L10): `phux spawn --retain`, `phux resource wait|show`,
//! `phux spawn --idempotency-key`, and `phux watch --after`, each run as its
//! own `phux` subprocess against a real PTY-backed `phux server`.
//!
//! `#[ignore]`: they spawn a real server. Run via `just e2e`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const DEADLINE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// A `phux server` child with a long-lived seed pane, killed on drop.
struct Server(common::ServerGuard);

impl Server {
    fn start() -> Self {
        Self(
            common::ServerGuard::builder("resource")
                .seed_command("exec sleep 600")
                .start(),
        )
    }

    /// Run `phux --socket SOCKET ARGS...`. `--socket` goes first so a
    /// trailing `-- COMMAND` cannot swallow it.
    fn phux(&self, args: &[&str]) -> Output {
        phux_at(&self.0.socket, args)
    }

    fn json(&self, args: &[&str]) -> (Value, Output) {
        let out = self.phux(args);
        let doc = serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
            panic!(
                "`phux {}` did not print JSON ({err}): stdout={:?} stderr={:?}",
                args.join(" "),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (doc, out)
    }

    /// Spawn COMMAND with the extra spawn flags and return its `@N`.
    fn spawn(&self, flags: &[&str], command: &[&str]) -> String {
        let mut args = vec!["spawn", "--json"];
        args.extend_from_slice(flags);
        args.push("--");
        args.extend_from_slice(command);
        let (doc, out) = self.json(&args);
        assert!(out.status.success(), "spawn failed: {out:?}");
        format!("@{}", doc["terminal_id"])
    }
}

impl Server {
    /// How many Terminals `phux ls --json` lists.
    fn terminal_count(&self) -> usize {
        let (listing, _) = self.json(&["ls", "--json"]);
        listing["terminals"].as_array().expect("terminals").len()
    }
}

fn phux_at(socket: &Path, args: &[&str]) -> Output {
    common::phux_cmd(crate::runner::phux_bin())
        .arg("--socket")
        .arg(socket)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run phux")
}

fn last_stderr_json(out: &Output) -> Value {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr.lines().last().unwrap_or_default().to_owned();
    serde_json::from_str(&line).unwrap_or_else(|err| panic!("stderr tail {line:?}: {err}"))
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn spawn_retain_wait_exit_end_to_end() {
    let server = Server::start();
    let pane = server.spawn(&["--retain=600"], &["/bin/sh", "-c", "exit 42"]);

    // The waiter needs no timing luck: whether the exit already happened or
    // is still coming, subscribe-then-read reports it.
    let (doc, out) = server.json(&["resource", "wait", "--json", "--timeout", "20", &pane]);
    assert_eq!(out.status.code(), Some(0), "{doc}");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["resource"], pane.as_str());
    assert_eq!(doc["outcome"], "exited");
    assert_eq!(doc["exit"]["status"], 42);
    assert_eq!(doc["retained"], true);
    assert!(
        doc["cursor"].as_str().is_some_and(|c| c.contains(':')),
        "{doc}"
    );

    let (show, out) = server.json(&["resource", "show", "--json", &pane]);
    assert!(out.status.success());
    assert_eq!(show["lifecycle"], "exited");
    assert_eq!(show["exit"]["status"], 42);

    // `ls --json` lists the retained pane with its lifecycle and exit.
    let (listing, _) = server.json(&["ls", "--json"]);
    let row = listing["resources"]
        .as_array()
        .expect("resources")
        .iter()
        .find(|row| row["id"] == pane.as_str())
        .expect("the retained pane is listed");
    assert_eq!(row["lifecycle"], "exited");
    assert_eq!(row["exit"]["status"], 42);

    // The purge is an explicit kill; afterwards the pane is gone (exit 1).
    assert!(server.phux(&["kill", "--yes", &pane]).status.success());
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (doc, out) = server.json(&["resource", "wait", "--json", "--timeout", "5", &pane]);
        if doc["outcome"] == "gone" {
            assert_eq!(out.status.code(), Some(1));
            break;
        }
        assert!(Instant::now() < deadline, "the purge never landed: {doc}");
        std::thread::sleep(POLL);
    }
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn spawn_with_a_key_reused_for_another_command_is_an_idempotency_conflict() {
    const KEY: &str = "6e9f1d3b0f8c52d7b4a2f1e0d9c8b7a6";
    let server = Server::start();
    let first = server.phux(&[
        "spawn",
        "--json",
        "--idempotency-key",
        KEY,
        "--",
        "sleep",
        "600",
    ]);
    assert!(first.status.success(), "{first:?}");
    let conflict = server.phux(&[
        "spawn",
        "--json",
        "--idempotency-key",
        KEY,
        "--",
        "sleep",
        "601",
    ]);
    assert_eq!(conflict.status.code(), Some(2), "{conflict:?}");
    assert!(conflict.stdout.is_empty(), "a refusal prints no document");
    assert_eq!(
        last_stderr_json(&conflict)["error"]["code"],
        "idempotency_conflict"
    );
    assert_eq!(server.terminal_count(), 2, "the conflict spawned nothing");
}

/// `spawn --json` keeps the JSON error contract on every failure: a
/// `--target` that matches nothing and a command the server cannot spawn
/// each leave stdout empty and put one error document on stderr.
#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn spawn_json_failures_keep_the_json_error_contract() {
    let server = Server::start();
    for (args, code) in [
        (
            &["spawn", "--json", "--target", "@999", "--", "sleep", "600"][..],
            "no_such_target",
        ),
        (
            &["spawn", "--json", "--", "/nonexistent/phux-e2e-command"][..],
            "spawn_failed",
        ),
    ] {
        let out = server.phux(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {out:?}");
        assert!(
            out.stdout.is_empty(),
            "{args:?}: a failure prints no document"
        );
        let doc = last_stderr_json(&out);
        assert_eq!(doc["error"]["code"], code, "{args:?}: {doc}");
        assert_eq!(doc["exit_code"], 1, "{args:?}: {doc}");
    }
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn new_with_idempotency_key_twice_replays_the_first_create() {
    const KEY: &str = "7a0b2e4c1d9f63e8c5b3a2f1e0d9c8b7";
    let server = Server::start();
    let args = ["new", "-s", "keyed", "--json", "--idempotency-key", KEY];
    let (first, out) = server.json(&args);
    assert!(out.status.success(), "{out:?}");
    // Without the key, the client-side duplicate check would refuse this.
    let (second, out) = server.json(&args);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(first["session"], "keyed");
    assert_eq!(
        first["terminal_id"], second["terminal_id"],
        "the retry answers the first create"
    );
    let (listing, _) = server.json(&["ls", "--json"]);
    let named = listing["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .filter(|session| session["name"] == "keyed")
        .count();
    assert_eq!(named, 1, "one session");
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn new_with_a_key_reused_for_another_request_registers_nothing() {
    const KEY: &str = "8b1c3f5d2e0a74f9d6c4b3a2f1e0d9c8";
    let server = Server::start();
    let first = server.phux(&["new", "-s", "alpha", "--json", "--idempotency-key", KEY]);
    assert!(first.status.success(), "{first:?}");
    // The server ignores a different request under a used token and
    // publishes nothing (no refusal form exists for a create result).
    let reused = server.phux(&["new", "-s", "beta", "--json", "--idempotency-key", KEY]);
    assert_eq!(reused.status.code(), Some(1), "{reused:?}");
    assert!(reused.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&reused.stderr);
    assert!(stderr.contains("--idempotency-key"), "{stderr}");
    let (listing, _) = server.json(&["ls", "--json"]);
    assert!(
        !listing["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .any(|session| session["name"] == "beta"),
        "nothing was created: {listing}"
    );
}

/// `watch --after CURSOR @N` on a pane that closed unretained: the pane is
/// no longer in the inventory, but the replay still reaches its close.
///
/// The pane exits only once the test releases it, after the first watch has
/// its cursor (phux-l3ox): a timed exit could beat a loaded host's first
/// `watch` to the pane, and that watch must resolve a live `@N`. Every later
/// step resumes from that cursor, so none of them races the exit either.
#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn watch_after_cursor_replays_the_close_of_an_unretained_pane() {
    let server = Server::start();
    let gate = tempfile::tempdir().expect("release dir");
    let release = gate.path().join("release");
    let script = format!(
        "until [ -f '{}' ]; do sleep 0.01; done; exit 3",
        release.display()
    );
    let pane = server.spawn(&[], &["/bin/sh", "-c", &script]);
    let first = server.phux(&["watch", "--json", "--timeout", "0", &pane]);
    let cursor = last_stderr_json(&first)["cursor"]
        .as_str()
        .expect("a journaling server issues a cursor")
        .to_owned();
    std::fs::write(&release, b"").expect("release the pane");

    // Nobody is watching when the pane closes; it is not retained. The wait
    // resumes from the same cursor, so it finds the close whether it
    // subscribes before the exit or after the pane has left the inventory.
    let (doc, _) = server.json(&[
        "resource",
        "wait",
        "--json",
        "--after",
        &cursor,
        "--timeout",
        "20",
        &pane,
    ]);
    assert_eq!(doc["outcome"], "exited", "{doc}");
    assert_eq!(doc["retained"], false, "{doc}");

    let replay = server.phux(&[
        "watch",
        "--json",
        "--after",
        &cursor,
        "--until",
        "pane_closed",
        "--timeout",
        "10",
        &pane,
    ]);
    assert_eq!(replay.status.code(), Some(0), "{replay:?}");
    let closed = String::from_utf8_lossy(&replay.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("NDJSON line"))
        .find(|line| line["event"] == "pane_closed")
        .expect("the missed close is replayed");
    assert!(closed["seq"].as_u64().is_some(), "{closed}");
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn resource_show_json_has_lifecycle_and_process() {
    let server = Server::start();
    let (listing, _) = server.json(&["ls", "--json"]);
    let seed = listing["terminals"][0]
        .as_str()
        .expect("the seed pane")
        .to_owned();
    let (doc, out) = server.json(&["resource", "show", "--json", &seed]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["kind"], "terminal");
    assert_eq!(doc["session"], "work");
    assert_eq!(doc["lifecycle"], "running");
    assert!(doc["exit"].is_null());
    assert!(doc["process"]["child"]["pid"].as_i64().is_some(), "{doc}");
    assert_eq!(doc["tags"], serde_json::json!([]));

    let (methods, out) = server.json(&["resource", "methods", "--json", &seed]);
    assert!(out.status.success());
    let screen = methods["methods"]
        .as_array()
        .expect("methods")
        .iter()
        .find(|method| method["name"] == "GET_SCREEN")
        .expect("GET_SCREEN is in the terminal facet");
    assert_eq!(screen["available"], true);
    assert_eq!(screen["mutating"], false);
}

/// The whole resource lifecycle through the CLI: a keyed, retained spawn
/// (a repeat under the key replays), `resource wait` and `show` read the
/// exit, `watch --after` replays the unwatched exit from a spawn-anchored
/// cursor, and `kill` purges the pane so `show` answers a plain miss.
#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
#[expect(
    clippy::cognitive_complexity,
    reason = "one linear resource lifecycle through the CLI; every assert! scores as a branch"
)]
fn scripted_task_lifecycle_pha406() {
    const KEY: &str = "3c1a9e7f5d2b48c6a1f0e9d8c7b6a5f4";
    let server = Server::start();

    let spawn_args = [
        "spawn",
        "--json",
        "--retain=600",
        "--idempotency-key",
        KEY,
        "--",
        "/bin/sh",
        "-c",
        "exit 7",
    ];
    let (first, out) = server.json(&spawn_args);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(first["replayed"], false);
    let pane = format!("@{}", first["terminal_id"]);

    // A second spawn under the same key addresses the same task instead of
    // starting a duplicate.
    let (second, out) = server.json(&spawn_args);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(second["terminal_id"], first["terminal_id"], "one pane");
    assert_eq!(second["replayed"], true, "the retry is the replay path");
    assert_eq!(
        server.terminal_count(),
        2,
        "the seed pane plus the one spawned pane, not two"
    );

    // A later process sees the exit race-free; only the cursor's
    // `server_id` half is used below.
    let (wait, out) = server.json(&["resource", "wait", "--json", "--timeout", "20", &pane]);
    assert_eq!(out.status.code(), Some(0), "{wait}");
    assert_eq!(wait["outcome"], "exited");
    assert_eq!(wait["exit"]["status"], 7);
    assert_eq!(wait["retained"], true);
    let server_id = wait["cursor"]
        .as_str()
        .and_then(|cursor| cursor.split_once(':'))
        .map(|(server_id, _seq)| server_id.to_owned())
        .expect("resource wait prints a server_id:seq cursor");

    // `resource show` reads the same exit facet off the retained pane.
    let (show, out) = server.json(&["resource", "show", "--json", &pane]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(show["lifecycle"], "exited");
    assert_eq!(show["exit"]["status"], 7);

    // Replaying from 0 up to `pane_spawned` yields a cursor that predates
    // the exit by journal order, not timing.
    let after_zero = format!("{server_id}:0");
    let spawn_replay = watch_after_until(&server, &after_zero, "pane_spawned", &pane);
    assert_eq!(spawn_replay.status.code(), Some(0), "{spawn_replay:?}");
    let spawn_cursor = last_stderr_json(&spawn_replay)["cursor"]
        .as_str()
        .expect("a journaling server issues a cursor")
        .to_owned();
    let spawn_seq: u64 = spawn_cursor
        .rsplit_once(':')
        .and_then(|(_server_id, seq)| seq.parse().ok())
        .expect("cursor has a numeric seq");

    // `watch --after` resumes from that spawn-anchored cursor and replays
    // the close nobody was watching for live.
    let replay = watch_after_until(&server, &spawn_cursor, "terminal_control", &pane);
    assert_eq!(replay.status.code(), Some(0), "{replay:?}");
    let control = String::from_utf8_lossy(&replay.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("NDJSON line"))
        .find(|line| line["event"] == "terminal_control")
        .expect("the missed exit is replayed");
    assert_eq!(control["action"], "exited");
    assert_eq!(control["exit_status"], 7);
    let control_seq = control["seq"]
        .as_u64()
        .expect("a journaling server stamps seq on terminal_control");
    assert!(
        control_seq > spawn_seq,
        "the replayed exit (seq {control_seq}) should follow the spawn cursor (seq {spawn_seq})"
    );

    // Kill purges the retained pane; afterwards `resource show` answers a
    // plain miss, not a stale exit facet.
    assert!(server.phux(&["kill", "--yes", &pane]).status.success());
    let deadline = Instant::now() + DEADLINE;
    loop {
        let show = server.phux(&["resource", "show", "--json", &pane]);
        if show.status.code() == Some(1) {
            assert!(show.stdout.is_empty(), "a miss prints no document");
            assert_eq!(last_stderr_json(&show)["error"]["code"], "no_such_target");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the purge never landed: {}",
            String::from_utf8_lossy(&show.stdout)
        );
        std::thread::sleep(POLL);
    }
}

/// `phux watch --json --after CURSOR --until EVENT --timeout 10 PANE`: a
/// bounded cursor replay that stops at the first `EVENT`.
fn watch_after_until(server: &Server, cursor: &str, until: &str, pane: &str) -> Output {
    server.phux(&[
        "watch",
        "--json",
        "--after",
        cursor,
        "--until",
        until,
        "--timeout",
        "10",
        pane,
    ])
}
