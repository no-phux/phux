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

/// The identity recheck interval these tests run under, via the detector's
/// `PHUX_AGENT_IDENTIFY_RECHECK_MS` seam (production default: 5 s).
///
/// A departure is only acted on after `VACANT_CONFIRMATIONS` *confirmed*
/// vacant observations, so a test that watches a real agent die otherwise
/// waits out two full production rechecks. Nothing being proven depends on
/// the interval's length — only on the confirmation COUNT, which the seam
/// does not touch.
const TEST_IDENTIFY_RECHECK: Duration = Duration::from_millis(200);

/// How long an end-to-end case waits for a departure to be noticed: the
/// confirmations, the drain, and the fanout, with generous slack for a
/// loaded parallel test pool.
const DEPARTURE_DEADLINE: Duration = Duration::from_secs(12);

/// Shrink the detector's identity recheck for this test process. Same
/// constraints as [`shorten_startup_grace`].
fn shorten_identify_recheck() {
    // SAFETY-adjacent: as `shorten_startup_grace`.
    unsafe {
        std::env::set_var(
            "PHUX_AGENT_IDENTIFY_RECHECK_MS",
            TEST_IDENTIFY_RECHECK.as_millis().to_string(),
        );
    }
}

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
    write_fake_agent_ending_with(dir, "sleep 30")
}

/// As [`write_fake_agent`], but the agent LEAVES the pane — replacing itself
/// with `successor` — the moment the test creates `depart_when`.
///
/// `exec` is the honest shape of the departure this file cares about: the
/// agent's process is replaced with no `EXIT` trap, no `phux agent clear`, and
/// no PTY EOF, which is exactly what a `kill -9` or a force-closed agent
/// leaves behind. A script that simply exited would take the pane with it, and
/// the pane's reap would then clear the record for reasons that have nothing
/// to do with what is being tested.
///
/// The departure is gated on a file rather than on a timer because the ORDER
/// matters: the declaration has to be in the store before the process goes
/// away, or the test is proving something else. A timer makes that ordering a
/// race against a loaded parallel pool.
///
/// The polling `sleep` is a CHILD of the script, so it shares the script's
/// process group and the pane's foreground pgid still resolves to `claude` —
/// identification is unaffected, which is precisely why the detector reads the
/// process group leader rather than whatever happens to be running.
fn write_fake_agent_departing_on(
    dir: &std::path::Path,
    depart_when: &std::path::Path,
    successor: &str,
) -> std::path::PathBuf {
    write_fake_agent_ending_with(
        dir,
        &format!(
            "while [ ! -f '{}' ]; do sleep 0.1; done\nexec {successor}",
            depart_when.display(),
        ),
    )
}

/// As [`write_fake_agent`], with `tail` as the script's last lines.
fn write_fake_agent_ending_with(dir: &std::path::Path, tail: &str) -> std::path::PathBuf {
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
         echo ' Esc to cancel'\n";
    let script = format!("{script}{tail}\n");
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

/// Write an executable fake `codex` into `dir`: it clears the screen, paints
/// the command-approval prompt the shipped `rules/codex.toml` matches, and
/// idles.
///
/// A SECOND kind is what makes "the pane's occupant changed" expressible end
/// to end. The screen is painted from the same three strings the manifest's
/// `prompt-command-approval` rule requires, so the derived state for this
/// pane is unambiguously codex's — and unambiguously not the one claude's
/// dialog produced a moment earlier.
fn write_fake_codex(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("codex");
    let script = "#!/bin/sh\n\
         printf '\\033[2J\\033[H'\n\
         echo 'Would you like to run the following command?'\n\
         echo ''\n\
         echo '$ curl -s https://example.com | head -5'\n\
         echo ''\n\
         echo ' 1. Yes, proceed (y)'\n\
         echo ' 3. No, and tell Codex what to do differently (esc)'\n\
         echo ''\n\
         echo 'Press enter to confirm or esc to cancel'\n\
         sleep 30\n";
    std::fs::write(&path, script).expect("write fake codex");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake codex");
    }
    path
}

/// Drain `phux.agent/v1` records for `terminal` until one satisfies `done` or
/// `deadline` elapses, returning every record in the order a subscriber saw
/// them.
///
/// The ORDER is the point for the transient-consistency cases: an invariant
/// about what a consumer may never observe cannot be checked by sampling the
/// final state.
async fn collect_agent_records_until(
    stream: &mut UnixStream,
    terminal: &TerminalId,
    deadline: Duration,
    done: impl Fn(&serde_json::Value) -> bool,
) -> Vec<serde_json::Value> {
    let end = tokio::time::Instant::now() + deadline;
    let mut seen = Vec::new();
    loop {
        let remaining = end.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return seen;
        }
        match collect_agent_record(stream, terminal, remaining).await {
            Some(record) => {
                let finished = done(&record);
                seen.push(record);
                if finished {
                    return seen;
                }
            }
            None => return seen,
        }
    }
}

/// The `kind` / `state` pair of a record, for the invariant assertions.
fn kind_and_state(record: &serde_json::Value) -> (&str, &str) {
    (
        record
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(""),
        record
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(""),
    )
}

/// THE WEDGE (phux-w7z2.13), end to end, driven to the actual failure.
///
/// A human runs `phux agent set @N --name me --kind claude --state working`.
/// That is a DECLARATION: `docs/spec/L3.md` §3.7 says it outranks any
/// derivation, and the detector correctly stands down. Then the agent is
/// killed — no `EXIT` trap runs, no `phux agent clear` is issued, and the pane
/// survives, so the two things that clear a declaration (an explicit
/// `DELETE_METADATA`, and pane reap) both never happen.
///
/// The pane then sat at `working` for the life of the session. Every
/// `phux agent list`, every sidebar, every `agent wait` saw a live agent
/// working away in a pane that had been empty for hours, and there was no path
/// back to the truth from inside the system — precisely the failure the ADR
/// says level-triggering exists to prevent.
///
/// The fix is a WITHDRAWAL, not an overwrite and not a delete: on positive,
/// confirmed evidence that the declared occupant is gone, `state` goes to
/// `unknown` and the human's `name`, `kind` and `session` stay exactly as they
/// wrote them. The server asserts nothing it derived, and the record outlives
/// the process only as an honest "I don't know".
#[test]
fn a_declared_state_does_not_survive_the_death_of_the_process_it_describes() {
    shorten_startup_grace();
    shorten_identify_recheck();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // Paints the dialog, and leaves the pane the moment the test says so —
        // after which something that is not an agent owns its foreground
        // process group.
        let depart = tmp.path().join("depart");
        let agent = write_fake_agent_departing_on(tmp.path(), &depart, "sleep 300");

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

        // Precondition: the detector has identified a live agent in this pane.
        assert!(
            await_agent_state(&mut stream, &terminal, "blocked", DETECT_DEADLINE)
                .await
                .is_some(),
            "precondition: the detector never saw the agent at all",
        );

        // `phux agent set @N --name me --kind claude --state working`.
        send_frame(
            &mut stream,
            &FrameKind::SetMetadata {
                request_id: 11,
                scope: Scope::Terminal(terminal.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
                value: br#"{"name":"me","kind":"claude","state":"working","attention":"high"}"#
                    .to_vec(),
            },
        )
        .await;
        // The declaration is in flight ahead of the departure on the same
        // ordered connection, so the record IS declared by the time the
        // process goes away — which is the whole scenario.
        assert!(
            await_agent_state(&mut stream, &terminal, "working", DETECT_DEADLINE)
                .await
                .is_some(),
            "precondition: the declaration never landed",
        );

        std::fs::write(&depart, b"go").expect("signal the departure");

        // The agent dies. Nothing else happens: no trap, no clear, no EOF.
        let healed = await_agent_state(&mut stream, &terminal, "unknown", DEPARTURE_DEADLINE).await;
        let healed = healed.expect(
            "a declared record must not outlive the process it describes: with no EXIT trap \
             and no `agent clear`, a withdrawal to `unknown` is the ONLY path back to the \
             truth, and without it the pane reports `working` forever",
        );

        assert_eq!(
            healed.get("name").and_then(serde_json::Value::as_str),
            Some("me"),
            "the human's name is not the server's to take: {healed}",
        );
        assert_eq!(
            healed.get("kind").and_then(serde_json::Value::as_str),
            Some("claude"),
            "nor their kind — L3 §3.7 requires both preserved: {healed}",
        );
        assert!(
            healed.get("attention").is_none(),
            "but an unknown pane must not keep a red badge for a dead process: {healed}",
        );

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
    });
}

/// The same departure, for a record the DETECTOR wrote alone. The pane keeps
/// running (only the agent left), so the record must be removed rather than
/// left describing a process that is gone.
///
/// The half that already worked, pinned end to end: the `VACANT_CONFIRMATIONS`
/// gate is new, and a retraction that never fired would be a regression
/// nothing else in this file would catch.
#[test]
fn a_detector_written_record_is_retracted_when_the_agent_leaves_the_pane() {
    shorten_startup_grace();
    shorten_identify_recheck();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let depart = tmp.path().join("depart");
        let agent = write_fake_agent_departing_on(tmp.path(), &depart, "sleep 300");

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

        assert!(
            await_agent_state(&mut stream, &terminal, "blocked", DETECT_DEADLINE)
                .await
                .is_some(),
            "precondition: the detector never converged on `blocked`",
        );

        std::fs::write(&depart, b"go").expect("signal the departure");

        // The agent leaves. The DELETE arrives as a `METADATA_CHANGED` with no
        // value, which `collect_agent_record` skips — so wait for the frame
        // itself rather than for a record.
        let end = tokio::time::Instant::now() + DEPARTURE_DEADLINE;
        let mut deleted = false;
        while !deleted {
            let remaining = end.saturating_duration_since(tokio::time::Instant::now());
            assert!(!remaining.is_zero(), "the record was never retracted");
            let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(&mut stream)).await else {
                panic!("the record was never retracted");
            };
            if let FrameKind::MetadataChanged { scope, key, value } = frame
                && key == TERMINAL_AGENT_KEY
                && scope == Scope::Terminal(terminal.clone())
            {
                deleted = value.is_none();
            }
        }

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
    });
}

/// THE mixed-process record (phux-w7z2.27), end to end. A pane hosting
/// `claude` is replaced by `codex` in the same pane.
///
/// The detector used to reset its own memory on a kind change and emit
/// NOTHING, and the arbiter's `compose` preserved any `kind` already in the
/// record. So the record kept `kind: "claude"` and the next write gave it a
/// state derived from CODEX's screen. Nothing looked stale — the state was
/// fresh, the name was present, the record was live — and the kind was simply
/// a lie. That is worse than a stale record, because there is no signal in it
/// that anything is wrong.
///
/// The invariant (I2): a subscriber must NEVER observe one record whose `kind`
/// and `state` describe two different processes, not even for a single tick.
/// The correction is therefore one write that lands on `unknown` — the only
/// value that describes no process and so cannot describe the wrong one.
#[test]
fn a_kind_change_never_leaves_a_stale_kind_beside_a_live_state() {
    shorten_startup_grace();
    shorten_identify_recheck();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let codex = write_fake_codex(tmp.path());
        // Claude paints its dialog; when the test says so, codex takes the pane
        // over. `exec` keeps the pane and its pgid alive, so this is an
        // occupant change and not a pane teardown.
        let depart = tmp.path().join("depart");
        let agent =
            write_fake_agent_departing_on(tmp.path(), &depart, &codex.display().to_string());

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

        assert!(
            await_agent_state(&mut stream, &terminal, "blocked", DETECT_DEADLINE)
                .await
                .is_some(),
            "precondition: claude was never detected in this pane",
        );

        std::fs::write(&depart, b"go").expect("signal the handover");

        // Everything the subscriber sees from the moment claude is live, up to
        // and including the pane converging on codex's own derived state.
        let records =
            collect_agent_records_until(&mut stream, &terminal, DEPARTURE_DEADLINE, |record| {
                kind_and_state(record) == ("codex", "blocked")
            })
            .await;
        let pairs: Vec<(String, String)> = records
            .iter()
            .map(|r| {
                let (k, s) = kind_and_state(r);
                (k.to_owned(), s.to_owned())
            })
            .collect();

        let switch = pairs
            .iter()
            .position(|(kind, _)| kind == "codex")
            .unwrap_or_else(|| {
                panic!(
                    "the record never learned the pane's occupant had changed; it still says \
                     claude: {pairs:?}"
                )
            });

        assert_eq!(
            pairs[switch].1, "unknown",
            "the correcting write must land on `unknown`: a state derived from codex's \
             screen written in the same breath as the new kind would be fine, but the \
             record is only allowed ONE source per write and nothing has been derived \
             for codex yet: {pairs:?}",
        );
        assert!(
            pairs[switch..].iter().all(|(kind, _)| kind == "codex"),
            "once the occupant changed, no record may say claude again: {pairs:?}",
        );
        assert!(
            pairs[switch..]
                .iter()
                .any(|(kind, state)| kind == "codex" && state == "blocked"),
            "and the pane converges on codex's OWN derived state: {pairs:?}",
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
