//! What the user sees when the server could only answer for part of the fleet.
//!
//! A federation hub that cannot reach a satellite still answers `GET_STATE`:
//! it merges what it has and pushes one uncorrelated `ERROR` per unreachable
//! satellite ahead of the ack (`handle_get_state_federated` —
//! "observable degradation, not silence"). The client keeps those notices
//! now, but keeping is not showing, and the bug this file pins down was
//! entirely at the surface: `phux ls` printed a listing indistinguishable
//! from a complete one, and `phux kill @9` said "no such target" about a pane
//! that was alive on the other side of a downed link.
//!
//! These are black-box tests on purpose. The thing under test is the *user's
//! eye level* — the exact sentence on stderr, the JSON key, the exit status —
//! so they run the real binary and read its real streams. The server side is
//! [`phux_client::testkit`], where the reference ordering (degradation ERRORs
//! first, then the merged ack) is written down once.
//!
//! The split being asserted, verb by verb:
//!
//! - `ls` enumerates: warn, exit 0, and put it in the `--json` document too.
//! - `kill` / `tag` resolve a Terminal against `panes`, the one list a hub
//!   merges: a miss under degradation is *unresolved*, exit 3, and must not
//!   use the words "no such target".
//! - `rename` resolves a session name, and session lists never aggregate —
//!   `handle_get_state_federated` discards each satellite's — so a partial
//!   fleet cannot change its answer: warn, exit 0.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::Path;
use std::process::{Command, Output};

use phux_client::testkit::{ScriptSpec, ScriptedServer};
use phux_protocol::ids::{SessionId, TerminalId, WindowId};
use phux_protocol::wire::info::{SessionInfo, SessionSnapshot, TerminalInfo, WindowInfo};

/// One satellite's worth of prose, in the shape `hub::relay` writes it.
const OUTAGE: &str = "satellite build-box is unreachable: link is down";

/// A one-session, one-pane hub — the panes the hub can still see. The
/// satellite's panes are absent, which is the entire point: nothing in this
/// value says they are missing.
fn fleet() -> SessionSnapshot {
    let session = SessionId::new(1);
    let window = WindowId::new(10);
    SessionSnapshot::new(session, window, TerminalId::local(100))
        .with_sessions(vec![SessionInfo::new(session, "work")])
        .with_windows(vec![
            WindowInfo::new(window, session, "shell").with_index(0),
        ])
        .with_panes(vec![TerminalInfo::new(
            TerminalId::local(100),
            window,
            80,
            24,
        )])
}

/// Run the real `phux` binary with `args` against a scripted server.
///
/// The listener is bound *before* the child starts, so the connect can never
/// lose a race with the accept. The server thread owns its own runtime
/// because the child is a separate process — nothing here shares the client's
/// reactor.
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
    Command::new(env!("CARGO_BIN_EXE_phux"))
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

// --- `ls`: a reader. Warn, but answer. -------------------------------------

#[test]
fn ls_against_a_whole_fleet_says_nothing_extra() {
    let output = run_verb(whole_fleet(), &["ls"]);
    assert!(output.status.success());
    assert!(
        stdout_of(&output).contains("work"),
        "the listing still prints"
    );
    let stderr = stderr_of(&output);
    assert!(
        !stderr.contains("saw only part of the fleet"),
        "a complete listing must not cry partial; a warning printed every run \
         is a warning nobody reads, got {stderr:?}"
    );
}

#[test]
fn ls_against_a_partial_fleet_says_so_and_still_succeeds() {
    let output = run_verb(partial_fleet(), &["ls"]);
    assert!(
        output.status.success(),
        "a dead satellite elsewhere must not fail the listing of the panes right here"
    );
    assert!(
        stdout_of(&output).contains("work"),
        "the listing still prints"
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains(OUTAGE),
        "the user must be told which satellite went missing, got {stderr:?}"
    );
}

#[test]
fn ls_json_carries_the_incompleteness_in_the_document_not_on_stderr() {
    // `--json` consumers do not read stderr, and an agent that cannot tell a
    // partial inventory from a complete one will act on the difference.
    let output = run_verb(partial_fleet(), &["ls", "--json"]);
    assert!(output.status.success());
    let doc: serde_json::Value = serde_json::from_str(&stdout_of(&output)).expect("ls --json");
    assert_eq!(doc["schema_version"], 3, "the `unreachable` key bumped it");
    assert_eq!(doc["unreachable"], serde_json::json!([OUTAGE]));
}

#[test]
fn ls_json_states_completeness_positively() {
    // Present-and-empty, never absent: an absent key is what an older phux
    // emits, and a consumer cannot tell that apart from a degraded answer.
    let output = run_verb(whole_fleet(), &["ls", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout_of(&output)).expect("ls --json");
    assert_eq!(doc["unreachable"], serde_json::json!([]));
}

// --- `kill` / `tag`: resolvers. A miss is not an absence. ------------------

#[test]
fn kill_reports_a_real_miss_as_a_plain_miss() {
    let output = run_verb(whole_fleet(), &["kill", "@999"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr_of(&output).contains("no such target: @999"),
        "the established wording for a genuine miss is unchanged"
    );
}

#[test]
fn kill_refuses_to_call_an_unsearchable_pane_absent() {
    let output = run_verb(partial_fleet(), &["kill", "@999"]);
    assert_eq!(
        output.status.code(),
        Some(3),
        "a script must be able to branch: retry a partial view, do not retry a miss"
    );
    let stderr = stderr_of(&output);
    assert!(
        !stderr.contains("no such target"),
        "this client does not know the target is gone, got {stderr:?}"
    );
    assert!(
        stderr.contains("incomplete") && stderr.contains(OUTAGE),
        "the message must say what could not be seen, got {stderr:?}"
    );
}

#[test]
fn tag_draws_the_same_distinction_as_kill() {
    // `phux tag` addresses Terminals too, so it inherits the same hazard —
    // and the same two answers.
    let complete = run_verb(whole_fleet(), &["tag", "ls", "@999"]);
    assert_eq!(complete.status.code(), Some(1));
    assert!(stderr_of(&complete).contains("no such target: @999"));

    let degraded = run_verb(partial_fleet(), &["tag", "ls", "@999"]);
    assert_eq!(degraded.status.code(), Some(3));
    assert!(!stderr_of(&degraded).contains("no such target"));
    assert!(stderr_of(&degraded).contains(OUTAGE));
}

// --- `rename`: a session verb, which federation cannot mislead. ------------

#[test]
fn rename_warns_but_still_renames_under_a_partial_fleet() {
    // Session names never aggregate: `handle_get_state_federated` discards
    // each satellite's `sessions` list, so an unreachable satellite can
    // neither hide the session being renamed nor conceal a collision. The
    // right answer is a warning and a full success — deliberately *not* the
    // exit-3 refusal `kill` and `tag` give, because the reason for that
    // refusal does not exist here.
    let output = run_verb(partial_fleet(), &["rename", "work", "play"]);
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
    // The corollary: because the session name space is complete even when the
    // fleet is not, "no such session" stays a confident, exit-2 refusal here.
    // Weakening it would be the mirror-image error of the one this bead fixes.
    let output = run_verb(partial_fleet(), &["rename", "ghost", "play"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr_of(&output).contains("no such session"));
}

/// The socket path is the only server this test binary knows about, so a verb
/// that ignored `--socket` would silently talk to the developer's own daemon.
#[test]
fn verbs_under_test_honour_the_socket_override() {
    let missing = Path::new("/nonexistent/phux-partial-fleet.sock");
    let output = phux()
        .args(["ls", "--socket"])
        .arg(missing)
        .output()
        .expect("run phux");
    assert!(!output.status.success());
    assert!(stderr_of(&output).contains("no server running"));
}
