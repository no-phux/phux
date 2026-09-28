//! What the user sees when a federation hub could only answer for part of
//! the fleet (one `ERROR` per unreachable satellite ahead of the merged ack,
//! served by [`phux_client::testkit`]). Black-box on the exact stderr, JSON
//! key, and exit status:
//!
//! - `ls` warns, exits 0, and records it in the `--json` document.
//! - `kill` / `tag` treat a miss under degradation as unresolved (exit 3),
//!   never "no such target".
//! - `rename` resolves sessions, which hubs never aggregate: warn, exit 0.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

use std::os::unix::net::UnixListener as StdUnixListener;
use std::process::{Command, Output};

use phux_client::testkit::{ScriptSpec, ScriptedServer};
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

/// One satellite's worth of prose, in the shape `hub::relay` writes it.
const OUTAGE: &str = "satellite build-box is unreachable: link is down";

/// A one-session, one-pane hub — the panes the hub can still see. The
/// satellite's panes are absent, which is the entire point: nothing in this
/// value says they are missing.
fn fleet() -> SessionSnapshot {
    fleet_named("work")
}

fn fleet_named(name: &str) -> SessionSnapshot {
    let session = SessionId::new(1);
    let window = WindowId::new(10);
    SessionSnapshot::new(session, window, ResourceId::local(100))
        .with_sessions(vec![SessionInfo::new(session, name)])
        .with_windows(vec![
            WindowInfo::new(window, session, "shell").with_index(0),
        ])
        .with_resources(vec![ResourceInfo::new(
            ResourceId::local(100),
            window,
            80,
            24,
        )])
}

/// Run the real binary against a scripted server bound before the child
/// starts, on its own runtime.
fn run_verb(spec: ScriptSpec, args: &[&str]) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    let std_listener = StdUnixListener::bind(&socket).expect("bind scripted socket");
    std_listener.set_nonblocking(true).expect("nonblocking");
    let server = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("scripted runtime");
        runtime.block_on(async move {
            let listener =
                tokio::net::UnixListener::from_std(std_listener).expect("tokio listener");
            ScriptedServer::accept(&listener, spec).await
        });
    });

    let output = phux()
        .args(args)
        .arg("--socket")
        .arg(&socket)
        .output()
        .expect("run phux");
    // The harness serves until the client hangs up; the child has exited, so
    // its socket is closed and this joins immediately.
    server.join().expect("scripted server thread");
    output
}

fn phux() -> Command {
    crate::common::phux_cmd(env!("CARGO_BIN_EXE_phux"))
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A healthy server: the merged answer is the whole truth.
fn whole_fleet() -> ScriptSpec {
    ScriptSpec::new().state(fleet())
}

/// A hub with one satellite it could not reach. The notice rides ahead of the
/// ack because that is where the reference server puts it — the harness owns
/// that ordering, not this test.
fn partial_fleet() -> ScriptSpec {
    ScriptSpec::new().degradation_notice(OUTAGE).state(fleet())
}

/// `ls` answers a whole fleet with no warning and `unreachable: []` (present,
/// not absent); a partial fleet still lists and exits 0, naming the missing
/// satellite on stderr and in the `--json` document (`schema_version` 3).
#[test]
fn ls_answers_and_reports_completeness() {
    let output = run_verb(whole_fleet(), &["ls"]);
    assert!(output.status.success());
    assert!(stdout_of(&output).contains("work"));
    assert!(
        !stderr_of(&output).contains("saw only part of the fleet"),
        "a complete listing must not cry partial"
    );
    let output = run_verb(whole_fleet(), &["ls", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout_of(&output)).expect("ls --json");
    assert_eq!(doc["unreachable"], serde_json::json!([]));

    let output = run_verb(partial_fleet(), &["ls"]);
    assert!(
        output.status.success(),
        "a dead satellite must not fail the listing"
    );
    assert!(stdout_of(&output).contains("work"));
    assert!(
        stderr_of(&output).contains(OUTAGE),
        "{}",
        stderr_of(&output)
    );
    let output = run_verb(partial_fleet(), &["ls", "--json"]);
    assert!(output.status.success());
    let doc: serde_json::Value = serde_json::from_str(&stdout_of(&output)).expect("ls --json");
    assert_eq!(doc["schema_version"], 3);
    assert_eq!(doc["unreachable"], serde_json::json!([OUTAGE]));
}

/// Terminal resolvers: a miss on a whole fleet is `no such target` (exit 1);
/// on a partial fleet it is unresolved (exit 3), names the outage, and never
/// claims the pane is gone.
#[test]
fn kill_and_tag_never_call_an_unsearchable_pane_absent() {
    for args in [&["kill", "--yes", "@999"][..], &["tag", "ls", "@999"][..]] {
        let complete = run_verb(whole_fleet(), args);
        assert_eq!(complete.status.code(), Some(1), "{args:?}");
        assert!(
            stderr_of(&complete).contains("no such target: @999"),
            "{args:?}"
        );

        let degraded = run_verb(partial_fleet(), args);
        assert_eq!(degraded.status.code(), Some(3), "{args:?}");
        let stderr = stderr_of(&degraded);
        assert!(!stderr.contains("no such target"), "{args:?}: {stderr}");
        assert!(stderr.contains(OUTAGE), "{args:?}: {stderr}");
    }
}

// --- `rename`: a session verb, which federation cannot mislead. ------------

#[test]
fn rename_warns_but_still_renames_under_a_partial_fleet() {
    // Session names never aggregate, so a partial fleet cannot hide the rename
    // or a collision: warn and succeed, unlike `kill`/`tag`. The script's second
    // `GET_STATE` is the applied roster, as the real server's barrier reply is.
    let spec = ScriptSpec::new()
        .degradation_notice(OUTAGE)
        .states([fleet(), fleet_named("play")]);
    let output = run_verb(spec, &["rename", "work", "play"]);
    assert!(
        output.status.success(),
        "stderr was: {}",
        stderr_of(&output)
    );
    assert!(stdout_of(&output).contains("renamed"));
    assert!(
        stderr_of(&output).contains(OUTAGE),
        "still worth saying; it just does not change the answer"
    );
}

#[test]
fn rename_still_refuses_an_unknown_session_under_a_partial_fleet() {
    // The session name space is complete even when the fleet is not.
    let output = run_verb(partial_fleet(), &["rename", "ghost", "play"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr_of(&output).contains("no such session"));
}
