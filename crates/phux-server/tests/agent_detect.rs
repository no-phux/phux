//! Wire-level integration test for the server-side agent-state detector
//! (ADR-0046).
//!
//! The detector is the *producer* the `phux.agent/v1` record never had. Before
//! it, the only writer was a human running `phux agent set`, so `state` was
//! `unknown` forever and a consumer's sidebar was blind. This test pins the
//! whole chain from the wire's point of view — the only vantage point that
//! actually matters:
//!
//! 1. Seed a PTY-backed pane running a **fake agent**: a shell script that
//!    paints a prompt box shaped like a real permission dialog, then idles so
//!    the screen stays put.
//! 2. Attach, and `SUBSCRIBE_METADATA` on the pane's `phux.agent/v1` key.
//! 3. Assert a `METADATA_CHANGED` arrives carrying `state: "blocked"`.
//!
//! Note what this exercises that a unit test cannot: the actor's detector
//! timer actually fires; `foreground_pgid` + `process_argv` actually resolve a
//! real process through a real PTY; the identity comes back as `claude`
//! because the fake agent is *named* `claude` on disk; the region extractor
//! runs against a real libghostty grid projection; and the drain performs the
//! arbitration and the `metadata_set` that fans out to a real L3 subscriber.
//!
//! There is deliberately NO new wire surface here: the detector rides the
//! shipped `SET_METADATA` / `METADATA_CHANGED` path.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{FrameKind, Scope, TERMINAL_AGENT_KEY, TYPE_ATTACHED};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// The startup grace these tests run under, via the detector's
/// `PHUX_AGENT_STARTUP_GRACE_MS` seam (production default: 3 s). The grace
/// exists so a real agent's splash screen cannot flash `blocked`; the fake
/// agent here paints its dialog instantly, so a short grace changes nothing
/// about what is being proven and saves >=3 s of pure waiting per test.
const TEST_STARTUP_GRACE: Duration = Duration::from_millis(200);

/// The detector's unidentified-pane tick floor (`TICK_UNIDENTIFIED`),
/// restated for the negative test's window arithmetic.
const TICK_UNIDENTIFIED: Duration = Duration::from_millis(500);

/// How long to wait for a detector verdict. A failure ceiling, not a timing
/// gate: with the shortened grace a verdict lands within ~1 s, and this only
/// elapses in full when the detector never publishes at all.
const DETECT_DEADLINE: Duration = Duration::from_secs(8);

/// Shrink the detector's startup grace for this test process. Must run
/// before the server (and therefore any detector) is spawned; the override
/// is read once per process.
fn shorten_startup_grace() {
    // SAFETY-adjacent: `set_var` is unsafe on edition 2024 because of
    // concurrent env access; nextest runs each test in its own process and
    // this runs before the server thread exists.
    unsafe {
        std::env::set_var(
            "PHUX_AGENT_STARTUP_GRACE_MS",
            TEST_STARTUP_GRACE.as_millis().to_string(),
        );
    }
}

/// Write an executable fake agent named `claude` into `dir`, and return its
/// path.
///
/// The name on disk is the entire point: identification reads the PTY's
/// foreground process group and resolves the kind from that process's argv,
/// so a script literally named `claude` is what makes the shipped
/// `rules/claude.toml` manifest apply. Nothing about the *content* of the
/// script identifies it — which is the property we want, because a title or
/// a screen can be forged and a process name is what the kernel says.
///
/// The script paints a permission dialog in the shape Claude Code renders
/// one: a rounded box containing the question and a numbered option list.
/// Then it sleeps, so the live screen keeps saying `blocked` while the test
/// collects.
fn write_fake_agent(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("claude");
    // `exec` is load-bearing: without it the shell stays as the process group
    // leader and argv[0] would be `sh`, not `claude`. With it, the script
    // itself IS the foreground process group.
    //
    // The screen reproduces the shape Claude Code 2.1.207 ACTUALLY paints for
    // a permission dialog — captured in
    // `src/agent_detect/fixtures/claude/blocked_permission.txt`. That shape is
    // a horizontal rule (U+2500) with the dialog below it, NOT a box-drawn
    // frame: the dialog REPLACES the input box, so it is the only thing under
    // the final rule. An earlier version of this test painted a rounded box
    // (U+256D/U+2570 corners) that no shipped Claude has ever drawn, and it
    // passed against a manifest that matched nothing in the real CLI.
    //
    // The transcript line above the rule is load-bearing too: it is where a
    // real agent would print dialog-shaped text, and `after-last-rule` must
    // structurally exclude it.
    //
    // The screen carries both halves the `prompt-permission-dialog` rule
    // requires: the "Do you want to " question stem AND a numbered option
    // line. Either alone must NOT be enough — that AND is what keeps a quoted
    // diff from ever reading as a live prompt.
    let script = "#!/bin/sh\n\
         printf '\\033[2J\\033[H'\n\
         echo 'some transcript output above the live chrome'\n\
         echo ''\n\
         printf '\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\n'\n\
         echo ' Bash command'\n\
         echo ''\n\
         echo '   touch /tmp/probe.txt'\n\
         echo ''\n\
         echo ' Do you want to proceed?'\n\
         printf ' \\342\\235\\257 1. Yes\\n'\n\
         echo '   2. Yes, and always allow access'\n\
         echo '   3. No'\n\
         echo ''\n\
         echo ' Esc to cancel'\n\
         sleep 30\n";
    std::fs::write(&path, script).expect("write fake agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake agent");
    }
    path
}

/// Drain frames until a `METADATA_CHANGED` for `phux.agent/v1` on `terminal`
/// arrives with a value, or the deadline elapses. Every other frame
/// (`TERMINAL_OUTPUT`, snapshots, ...) is skipped.
async fn collect_agent_record(
    stream: &mut UnixStream,
    terminal: &TerminalId,
    deadline: Duration,
) -> Option<serde_json::Value> {
    let end = tokio::time::Instant::now() + deadline;
    loop {
        let remaining = end.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            return None;
        };
        if let FrameKind::MetadataChanged { scope, key, value } = frame
            && key == TERMINAL_AGENT_KEY
            && scope == Scope::Terminal(terminal.clone())
            && let Some(bytes) = value
        {
            return serde_json::from_slice(&bytes).ok();
        }
    }
}

/// Drain `METADATA_CHANGED` frames for `terminal` until one carries
/// `state: <want>`, or `deadline` elapses.
///
/// The detector derives its verdict incrementally — e.g. it can identify and
/// publish a pane as `idle` a tick before it parses the grid and republishes
/// `blocked` — so sampling only the FIRST published record (as
/// `collect_agent_record` alone does) races that convergence: under the
/// shared parallel nextest pool the first record is sometimes still `idle`
/// when the test asserts `blocked` (phux-manu). This keeps consuming
/// `METADATA_CHANGED` frames against ONE bounded deadline (never extended,
/// never slept past) until the wanted state actually shows up, which is the
/// honest fix — the assertion should wait for the real terminal state, not
/// for however far the detector happened to get before the first frame.
async fn await_agent_state(
    stream: &mut UnixStream,
    terminal: &TerminalId,
    want: &str,
    deadline: Duration,
) -> Option<serde_json::Value> {
    let end = tokio::time::Instant::now() + deadline;
    loop {
        let remaining = end.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let record = collect_agent_record(stream, terminal, remaining).await?;
        if record.get("state").and_then(serde_json::Value::as_str) == Some(want) {
            return Some(record);
        }
    }
}

/// The end-to-end contract: a real agent process, painting a real permission
/// dialog into a real grid, produces a `phux.agent/v1` record with
/// `state: "blocked"` on a subscribed client — with no human ever running
/// `phux agent set`.
#[test]
fn detector_publishes_blocked_from_a_live_prompt_box() {
    shorten_startup_grace();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let agent = write_fake_agent(tmp.path());

        let cmd = CommandBuilder::new(&agent);
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // ---- ATTACH ---- (gives the client a mailbox the L3 fanout targets,
        // and tells us the pane's wire id).
        send_frame(&mut stream, &attach_by_name("demo")).await;
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "first frame must be ATTACHED");
        let FrameKind::Attached { snapshot, .. } = attached else {
            panic!("expected ATTACHED");
        };
        let terminal = snapshot.focused_pane.clone();

        // ---- SUBSCRIBE_METADATA on this pane's agent record ----
        send_frame(
            &mut stream,
            &FrameKind::SubscribeMetadata {
                scope: Scope::Terminal(terminal.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
            },
        )
        .await;

        // ---- the detector should derive `blocked` and publish it ----
        // (poll for the converged state, not just the first publish: see
        // `await_agent_state`.)
        let record = await_agent_state(&mut stream, &terminal, "blocked", DETECT_DEADLINE).await;

        let record = record.expect(
            "the detector must publish a phux.agent/v1 record for a pane running a known agent \
             that is showing a live permission dialog",
        );
        assert_eq!(
            record.get("state").and_then(serde_json::Value::as_str),
            Some("blocked"),
            "a live prompt box asking the human a question is `blocked`: {record}",
        );
        assert_eq!(
            record.get("kind").and_then(serde_json::Value::as_str),
            Some("claude"),
            "identity comes from the foreground process group, not the screen: {record}",
        );
        assert_eq!(
            record.get("name").and_then(serde_json::Value::as_str),
            Some("claude"),
            "name comes from the manifest: {record}",
        );
        // The detector never sets `attention`: L3 §3.7 derives it from `state`.
        assert!(
            record.get("attention").is_none(),
            "the detector must not write `attention`: {record}",
        );

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
    });
}

/// The fail-safe, end-to-end. A pane running a plain shell — no agent — must
/// never acquire an agent record. This is the property that keeps the sidebar
/// honest: an unidentified pane is not an idle agent, it is *not an agent*.
#[test]
fn a_plain_shell_pane_never_gets_an_agent_record() {
    shorten_startup_grace();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // A shell that paints something a naive substring matcher would
        // happily call a permission prompt — and that a process-group-based
        // identifier correctly ignores, because no agent is running here.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("echo 'Do you want to proceed?'; echo '1. Yes'; sleep 20");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        send_frame(&mut stream, &attach_by_name("demo")).await;
        let (_type_byte, attached) = recv_typed(&mut stream).await;
        let FrameKind::Attached { snapshot, .. } = attached else {
            panic!("expected ATTACHED");
        };
        let terminal = snapshot.focused_pane.clone();

        send_frame(
            &mut stream,
            &FrameKind::SubscribeMetadata {
                scope: Scope::Terminal(terminal.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
            },
        )
        .await;

        // Well past the (shortened) startup grace plus two unidentified-pane
        // detector ticks: had the shell been (wrongly) identified, the grace
        // would have expired and a publish landed well inside this window.
        let absence_window = TEST_STARTUP_GRACE + TICK_UNIDENTIFIED * 2;
        let record = collect_agent_record(&mut stream, &terminal, absence_window).await;
        assert!(
            record.is_none(),
            "a pane with no agent in its foreground process group must never get a \
             phux.agent/v1 record, however suggestive its output: {record:?}",
        );

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
    });
}

/// ADR-0046 §E promises that `DELETE`ing the record hands it back: "the
/// detector makes no further writes to that Terminal **until the record is
/// `DELETE`d**". It did not.
///
/// The detector's edge filter is a model of its OWN emissions, so after the
/// `DELETE` it still held the tuple it last derived. The next tick re-derived
/// the same tuple, the filter suppressed it, and nothing was written — so the
/// pane showed NO agent at all until the agent's state next changed. For an
/// agent sitting `blocked` on a human (this one), that is never: it is waiting
/// for the answer, so it emits nothing, so the grid never changes, so no
/// transition ever comes. The pane is invisible in the sidebar indefinitely,
/// which is the exact opposite of what the delete was supposed to do.
///
/// Reachable from the shipped CLI: `phux agent clear`.
#[test]
fn deleting_the_record_hands_it_back_to_the_detector() {
    shorten_startup_grace();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let agent = write_fake_agent(tmp.path());

        let cmd = CommandBuilder::new(&agent);
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        send_frame(&mut stream, &attach_by_name("demo")).await;
        let (_type_byte, attached) = recv_typed(&mut stream).await;
        let FrameKind::Attached { snapshot, .. } = attached else {
            panic!("expected ATTACHED");
        };
        let terminal = snapshot.focused_pane.clone();

        send_frame(
            &mut stream,
            &FrameKind::SubscribeMetadata {
                scope: Scope::Terminal(terminal.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
            },
        )
        .await;

        // Poll for the converged `blocked` state rather than sampling the
        // first publish (see `await_agent_state`): the detector can land on
        // an intermediate state like `idle` a tick before it derives
        // `blocked`, and under the shared parallel nextest pool that first
        // tick sometimes wins the race, flaking this precondition
        // (phux-manu).
        let first = await_agent_state(&mut stream, &terminal, "blocked", DETECT_DEADLINE).await;
        assert!(
            first.is_some(),
            "precondition: the detector never converged on `blocked`",
        );

        // `phux agent clear`. The row is gone; the screen is unchanged and the
        // agent — being blocked on a human — will never emit another byte.
        send_frame(
            &mut stream,
            &FrameKind::DeleteMetadata {
                request_id: 7,
                scope: Scope::Terminal(terminal.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
            },
        )
        .await;

        // The detector must resume: the record comes back, WITHOUT the agent
        // having to change state. (`collect_agent_record` skips the delete's
        // tombstone, which carries no value, so this is the republish.) Same
        // convergence caveat as the precondition above: poll for `blocked`
        // rather than trusting the first post-DELETE publish.
        let again = await_agent_state(&mut stream, &terminal, "blocked", DETECT_DEADLINE).await;
        let again = again.expect(
            "after a DELETE the detector must resume ownership and rewrite the record; \
             an idle or blocked agent never changes state again, so a detector whose edge \
             filter still models the pre-delete store leaves the pane blank forever",
        );
        assert_eq!(
            again.get("state").and_then(serde_json::Value::as_str),
            Some("blocked"),
            "and it republishes the truth it can still see on the screen: {again}",
        );

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
    });
}

/// The headless half, end to end: a connection that **never attaches** must
/// still receive the detector's record.
///
/// `phux watch` connects, subscribes, and streams — it deliberately does not
/// attach, because watching a pane must not disturb the session someone is
/// working in. Metadata fanout used to resolve each subscriber's mailbox
/// through the attached-client table alone, so a watcher had no mailbox to
/// resolve and every `phux.agent/v1` record ADR-0046 derived was computed,
/// broadcast, and dropped. Every other test in this file attaches first, so
/// none of them could see it.
#[test]
fn an_unattached_subscriber_receives_the_detectors_record() {
    shorten_startup_grace();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let agent = write_fake_agent(tmp.path());

        let cmd = CommandBuilder::new(&agent);
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);

        // One attached connection, used only to learn the pane's wire id —
        // the same thing `phux watch` gets from its client-side `GET_STATE`
        // resolution before it opens the watch connection.
        let mut attached = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut attached, &attach_by_name("demo")).await;
        let (_type_byte, frame) = recv_typed(&mut attached).await;
        let FrameKind::Attached { snapshot, .. } = frame else {
            panic!("expected ATTACHED");
        };
        let terminal = snapshot.focused_pane.clone();

        // The watcher: HELLO, SUBSCRIBE_METADATA, and nothing else. No
        // ATTACH, no ATTACH_TERMINAL, no viewport.
        let mut watcher = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut watcher,
            &FrameKind::SubscribeMetadata {
                scope: Scope::Terminal(terminal.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
            },
        )
        .await;

        let record = await_agent_state(&mut watcher, &terminal, "blocked", DETECT_DEADLINE).await;
        let record = record.expect(
            "a subscriber that never attached must still receive the detector's record; \
             without it `phux watch` observes nothing an agent does",
        );
        assert_eq!(
            record.get("name").and_then(serde_json::Value::as_str),
            Some("claude"),
            "and the whole record, not a stub: {record}",
        );

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
    });
}

/// The documented "useful half" of the feature (ADR-0046 §8): a human supplies
/// the identity, the detector fills the lifecycle in around it. An
/// identity-only `SET_METADATA` is deliberately NOT a declaration, so the
/// detector keeps running — but its edge filter still held the state it had
/// already derived, so it wrote nothing, and `state` stayed as the human left
/// it (absent => `unknown`) forever. The half of the feature that is supposed
/// to work did not.
#[test]
fn an_identity_only_set_gets_its_state_filled_in_by_the_detector() {
    shorten_startup_grace();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let agent = write_fake_agent(tmp.path());

        let cmd = CommandBuilder::new(&agent);
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        send_frame(&mut stream, &attach_by_name("demo")).await;
        let (_type_byte, attached) = recv_typed(&mut stream).await;
        let FrameKind::Attached { snapshot, .. } = attached else {
            panic!("expected ATTACHED");
        };
        let terminal = snapshot.focused_pane.clone();

        send_frame(
            &mut stream,
            &FrameKind::SubscribeMetadata {
                scope: Scope::Terminal(terminal.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
            },
        )
        .await;

        let first = collect_agent_record(&mut stream, &terminal, DETECT_DEADLINE).await;
        assert!(first.is_some(), "precondition: the detector published");

        // `phux agent set --name reviewer --session fleet-7` — no `--state`.
        send_frame(
            &mut stream,
            &FrameKind::SetMetadata {
                request_id: 9,
                scope: Scope::Terminal(terminal.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
                value: br#"{"name":"reviewer","session":"fleet-7"}"#.to_vec(),
            },
        )
        .await;

        // The detector must fill `state` in around them, without ever having to
        // wait for the agent to change state.
        let end = tokio::time::Instant::now() + DETECT_DEADLINE;
        let filled = loop {
            let left = end.saturating_duration_since(tokio::time::Instant::now());
            assert!(!left.is_zero(), "the detector never filled `state` in");
            let Some(record) = collect_agent_record(&mut stream, &terminal, left).await else {
                panic!("the detector never filled `state` in");
            };
            if record.get("state").and_then(serde_json::Value::as_str) == Some("blocked") {
                break record;
            }
        };
        assert_eq!(
            filled.get("name").and_then(serde_json::Value::as_str),
            Some("reviewer"),
            "and the human's name is preserved field-for-field: {filled}",
        );
        assert_eq!(
            filled.get("session").and_then(serde_json::Value::as_str),
            Some("fleet-7"),
            "as is their label: {filled}",
        );

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
    });
}
