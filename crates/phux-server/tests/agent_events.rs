//! Wire-level integration test for the agent-event stream (SPEC §7.5,
//! ADR-0022 'events', `phux-y2t`).
//!
//! The push half of the agent surface: a client SUBSCRIBES to events and
//! the server PUSHES `EVENT` frames as a pane bells, retitles, and
//! ultimately closes. This test pins the server half of the contract from
//! the wire's point of view:
//!
//! 1. Pre-seed a PTY-backed pane with a shell that, after a short delay
//!    (so the client wins the race to subscribe), emits an OSC 2 title
//!    change and a BEL, then exits.
//! 2. Attach a client and `SUBSCRIBE_EVENTS { terminal: None }`
//!    (server-wide).
//! 3. Assert `title_changed`, `bell`, and `pane_closed` events arrive.
//!
//! The delay is the same race-avoidance trick `eof_detach.rs` uses — the
//! event stream is a best-effort accelerator, so events emitted before the
//! subscription lands are legitimately dropped; the seed defers its
//! observable output until the client has subscribed.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::wire::frame::{AgentEvent, FrameKind, TYPE_ATTACHED};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::tracing_capture::TracingCapture;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// A shell that defers its observable output so the test client wins the
/// race to `SUBSCRIBE_EVENTS` before any event fires. After ~250ms it sets
/// the terminal title via OSC 2 (`ESC ] 2 ; phux-watch BEL`) →
/// `title_changed`, rings the bell (`printf '\a'`) → `bell`, then exits 0 →
/// PTY EOF → `pane_closed`.
fn seed_with_title_bell_then_exit() -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    // `printf` is POSIX. `\033]2;...\007` is OSC 2 set-title; `\007` alone
    // is the BEL. The leading sleep lets the client subscribe first.
    cmd.arg("sleep 0.25; printf '\\033]2;phux-watch\\007'; printf '\\007'; sleep 0.1; exit 0");
    cmd
}

/// Drain `EVENT` frames until `complete(&seen)` says every event the caller
/// is waiting on has arrived, or `deadline` elapses. Non-`EVENT` frames
/// (`ATTACHED`, `TERMINAL_SNAPSHOT`, `TERMINAL_OUTPUT`, `TERMINAL_CLOSED`,
/// etc.) are skipped — we assert on the event stream specifically.
///
/// The stop condition is the caller's, not a hardcoded title+bell+closed
/// triple: a test that only ever receives `pane_closed` would otherwise sit
/// out the full deadline waiting for events that can never come.
async fn collect_events(
    stream: &mut UnixStream,
    deadline: Duration,
    complete: impl Fn(&[AgentEvent]) -> bool,
) -> Vec<AgentEvent> {
    let end = tokio::time::Instant::now() + deadline;
    let mut seen = Vec::new();
    loop {
        let remaining = end.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return seen;
        }
        let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            return seen;
        };
        if let FrameKind::Event { event, .. } = frame {
            seen.push(event);
            if complete(&seen) {
                return seen;
            }
        }
    }
}

/// A subscribed client receives `title_changed`, `bell`, and `pane_closed`
/// agent events as the seed pane retitles, bells, and exits (SPEC §7.5).
#[test]
fn subscribed_client_receives_title_bell_and_pane_closed_events() {
    run_local(async {
        // CI-flake forensics: this test once saw `exit_status: None` where
        // a clean exit should report `Some(0)` (the EOF-vs-waitable reap
        // race). The capture dumps the server's debug log on panic so any
        // recurrence shows the reap path's own telemetry.
        let _cap = TracingCapture::install("agent_events");
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        let cmd = seed_with_title_bell_then_exit();
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // ---- ATTACH ---- (so the client has an `attached` mailbox the
        // event fanout can target).
        send_frame(&mut stream, &attach_by_name("demo")).await;
        let (type_byte, _attached) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "first server-to-client frame must be ATTACHED",
        );

        // ---- SUBSCRIBE_EVENTS (server-wide) ---- before the seed's
        // ~250ms deferred output fires.
        send_frame(&mut stream, &FrameKind::SubscribeEvents { terminal: None }).await;

        // ---- collect EVENT frames ----
        let events = collect_events(&mut stream, Duration::from_secs(3), |seen| {
            seen.iter()
                .any(|e| matches!(e, AgentEvent::TitleChanged { .. }))
                && seen.iter().any(|e| matches!(e, AgentEvent::Bell))
                && seen
                    .iter()
                    .any(|e| matches!(e, AgentEvent::PaneClosed { .. }))
        })
        .await;

        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::TitleChanged { title } if title == "phux-watch")),
            "expected a title_changed event carrying the OSC-2 title; got {events:?}",
        );
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::Bell)),
            "expected a bell event from the BEL byte; got {events:?}",
        );
        assert!(
            events.iter().any(
                |e| matches!(e, AgentEvent::PaneClosed { exit_status } if *exit_status == Some(0))
            ),
            "expected a pane_closed event with exit_status Some(0); got {events:?}",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// A client that SUBSCRIBES *without* attaching still receives events
/// (SPEC §7.5, phux-y2t). This pins the `phux watch` path — `watch`
/// connects and subscribes but never sends `ATTACH`, so event fanout MUST
/// resolve the client's mailbox from the subscription registry rather than
/// from the `attached` map (the regression the mailbox-in-subscription fix
/// closed). Here the unattached watcher subscribes server-wide and must
/// observe the seed pane's `pane_closed` when it exits.
#[test]
fn unattached_subscriber_receives_events() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // A seed shell that lives long enough for the watcher to subscribe,
        // then exits → PTY EOF → pane_closed.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("sleep 0.3; exit 0");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // NO ATTACH. Subscribe server-wide straight away.
        send_frame(&mut stream, &FrameKind::SubscribeEvents { terminal: None }).await;

        // `pane_closed` is the ONLY event this seed produces, so it is also
        // the stop condition — the collector returns the moment it lands
        // instead of sitting out the full deadline.
        let events = collect_events(&mut stream, Duration::from_secs(3), |seen| {
            seen.iter()
                .any(|e| matches!(e, AgentEvent::PaneClosed { .. }))
        })
        .await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::PaneClosed { .. })),
            "an unattached subscriber must still receive pane_closed; got {events:?}",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// phux-foz.4: a subscribed client receives `command_started`,
/// `command_finished` (WITH the OSC-133 `D`-mark exit code), and
/// `cwd_changed` as the seed shell runs a shell-integration-marked
/// command after a `cd`. Pins the raw-PTY-stream OSC-133 scan (the grid
/// projection cannot yield the exit code) and the prompt-boundary kernel
/// cwd re-query.
#[test]
fn subscribed_client_receives_command_and_cwd_events() {
    run_local(async {
        let _cap = TracingCapture::install("agent_events_cmd_cwd");
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // Deferred like the title/bell seed so the client wins the race to
        // subscribe. The shell: cd to / (a directory that cannot equal the
        // spawn cwd of a tempdir-homed test server), then emit an OSC-133
        // C mark (command executing), fake output, and a D mark carrying
        // exit code 7. It lingers briefly so the prompt-boundary kernel
        // cwd query still finds a live child.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg(
            "sleep 0.25; cd /; \
             printf '\\033]133;C\\007out\\r\\n\\033]133;D;7\\007'; \
             sleep 0.3; exit 0",
        );
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        send_frame(&mut stream, &attach_by_name("demo")).await;
        let (type_byte, _attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED);

        send_frame(&mut stream, &FrameKind::SubscribeEvents { terminal: None }).await;

        // Drain EVENT frames until all three targets are seen, or the
        // pane closes (no further events can follow, and reading on would
        // run into the server's post-exit socket teardown), or the
        // deadline elapses.
        let events = collect_events(&mut stream, Duration::from_secs(4), |seen| {
            let saw_all = seen.iter().any(|e| matches!(e, AgentEvent::CommandStarted))
                && seen
                    .iter()
                    .any(|e| matches!(e, AgentEvent::CommandFinished { .. }))
                && seen
                    .iter()
                    .any(|e| matches!(e, AgentEvent::CwdChanged { .. }));
            saw_all
                || seen
                    .iter()
                    .any(|e| matches!(e, AgentEvent::PaneClosed { .. }))
        })
        .await;

        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::CommandStarted)),
            "expected command_started from the OSC-133 C mark; got {events:?}",
        );
        assert!(
            events.iter().any(
                |e| matches!(e, AgentEvent::CommandFinished { exit_code } if *exit_code == Some(7))
            ),
            "expected command_finished carrying the D-mark exit code 7; got {events:?}",
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::CwdChanged { cwd } if cwd == "/")),
            "expected cwd_changed to / after the cd; got {events:?}",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}
