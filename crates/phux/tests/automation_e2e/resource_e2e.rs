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

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
const SOCKET_DEADLINE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// A `phux server` child with a long-lived seed pane, killed on drop.
struct Server {
    _process: common::ServerProcess,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Server {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir
            .path()
            .join(format!("resource-{}.sock", std::process::id()));
        let child = Command::new(PHUX)
            .args([
                "server",
                "--session",
                "work",
                "--seed-command",
                "exec sleep 600",
                "--exit-after-idle",
                "600",
                "--socket",
            ])
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");
        let server = Self {
            _process: common::ServerProcess::from_child(child, socket.clone()),
            socket,
            _dir: dir,
        };
        let deadline = Instant::now() + SOCKET_DEADLINE;
        while !server.socket.exists() {
            assert!(
                Instant::now() < deadline,
                "the server never bound its socket"
            );
            std::thread::sleep(POLL);
        }
        server
    }

    /// Run `phux --socket SOCKET ARGS...`. `--socket` goes first so a
    /// trailing `-- COMMAND` cannot swallow it.
    fn phux(&self, args: &[&str]) -> Output {
        phux_at(&self.socket, args)
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
    /// Whether `phux status --json` lists `feature`.
    fn advertises(&self, feature: &str) -> bool {
        let (status, _) = self.json(&["status", "--json"]);
        status["features"]
            .as_array()
            .is_some_and(|features| features.iter().any(|f| f == feature))
    }

    /// How many Terminals `phux ls --json` lists.
    fn terminal_count(&self) -> usize {
        let (listing, _) = self.json(&["ls", "--json"]);
        listing["terminals"].as_array().expect("terminals").len()
    }
}

fn phux_at(socket: &Path, args: &[&str]) -> Output {
    Command::new(PHUX)
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
    let deadline = Instant::now() + SOCKET_DEADLINE;
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
fn spawn_with_idempotency_key_twice_yields_one_pane() {
    const KEY: &str = "5d8f0c2a9e7b41c6a3f1e0d9c8b7a6f5";
    let server = Server::start();
    assert!(
        server.advertises("spawn_idempotency"),
        "this build's server honors keyed spawns"
    );
    let args = [
        "spawn",
        "--json",
        "--idempotency-key",
        KEY,
        "--",
        "sleep",
        "600",
    ];
    let (first, out) = server.json(&args);
    assert!(out.status.success(), "{out:?}");
    let (second, out) = server.json(&args);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(first["terminal_id"], second["terminal_id"], "one pane");
    assert_eq!(first["replayed"], false);
    assert_eq!(second["replayed"], true, "the retry is the replay path");
    assert_eq!(
        server.terminal_count(),
        2,
        "the seed pane plus one spawned pane"
    );
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

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn watch_after_cursor_replays_missed_events() {
    let server = Server::start();
    let pane = server.spawn(&["--retain=600"], &["/bin/sh", "-c", "sleep 1; exit 3"]);

    // A first, short watch hands back a cursor from before the exit.
    let first = server.phux(&["watch", "--json", "--timeout", "0", &pane]);
    let cursor = last_stderr_json(&first)["cursor"]
        .as_str()
        .expect("a journaling server issues a cursor")
        .to_owned();

    // Nobody is watching when the pane exits.
    let (doc, _) = server.json(&["resource", "wait", "--json", "--timeout", "20", &pane]);
    assert_eq!(doc["outcome"], "exited", "{doc}");

    // Resuming from the cursor replays the exit the watcher missed.
    let replay = server.phux(&[
        "watch",
        "--json",
        "--after",
        &cursor,
        "--until",
        "terminal_control",
        "--timeout",
        "10",
        &pane,
    ]);
    assert_eq!(replay.status.code(), Some(0), "{replay:?}");
    let lines: Vec<Value> = String::from_utf8_lossy(&replay.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("NDJSON line"))
        .collect();
    let control = lines
        .iter()
        .find(|line| line["event"] == "terminal_control")
        .expect("the missed exit is replayed");
    assert_eq!(control["action"], "exited");
    assert_eq!(control["exit_status"], 3);
    assert!(control["seq"].as_u64().is_some(), "{control}");
    let reached = last_stderr_json(&replay)["cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(reached, cursor, "the cursor advances past the replay");
}

/// `watch --after CURSOR @N` on a pane that closed unretained: the pane is
/// no longer in the inventory, but the replay still reaches its close.
#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn watch_after_cursor_replays_the_close_of_an_unretained_pane() {
    let server = Server::start();
    let pane = server.spawn(&[], &["/bin/sh", "-c", "sleep 1; exit 3"]);
    let first = server.phux(&["watch", "--json", "--timeout", "0", &pane]);
    let cursor = last_stderr_json(&first)["cursor"]
        .as_str()
        .expect("a journaling server issues a cursor")
        .to_owned();

    // Nobody is watching when the pane closes; it is not retained.
    let (doc, _) = server.json(&["resource", "wait", "--json", "--timeout", "20", &pane]);
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

/// PHA-406 closeout: the whole resource-lifecycle journey through the CLI,
/// one agent step at a time, the way it composes for a real caller: a keyed
/// spawn with retain (a repeat under the same key is the replay, not a
/// second task) -> `resource wait` from a second, later process observes
/// the exit and its status race-free -> `resource show` reads the same
/// exit facet off the retained pane -> `watch --after` resumes from a
/// cursor anchored to the pane's own `pane_spawned` event and replays the
/// close nobody watched live -> `kill` purges the retained pane, after
/// which `resource show` answers a plain miss instead of a stale exit
/// facet.
#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
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

    // A second, later process — a fresh `phux resource wait` subprocess —
    // sees the exit and its status: subscribe-then-read is race-free
    // whether the exit already happened or is still coming. Its cursor's
    // `server_id` prefix names this server incarnation's journal; the `seq`
    // half is not used below (a fresh replay from 0 is taken instead, so
    // the pre-exit cursor is anchored to a real event rather than a race
    // against wall-clock time).
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

    // A full replay of this pane's journal (`--after server_id:0`), gated
    // on its own `pane_spawned`, hands back a cursor anchored to that one
    // event — proven (by journal order, not by timing) to predate the
    // exit, since the server journals the spawn before it can journal the
    // exit of the process it spawned.
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
    let deadline = Instant::now() + SOCKET_DEADLINE;
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
