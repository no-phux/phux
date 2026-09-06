//! Wire-level integration test for the agent-event stream (SPEC §7.5,
//! ADR-0022 'events', `phux-y2t`).
//!
//! The push half of the agent surface: a client SUBSCRIBES to events and
//! the server PUSHES `EVENT` frames as a pane retitles, bells, runs an
//! OSC-133-marked command, changes directory, and ultimately closes. These
//! tests pin the server half of the contract from the wire's point of view:
//!
//! 1. Pre-seed a PTY-backed pane with a shell that blocks on a *release
//!    file* before emitting anything observable.
//! 2. Attach a client and `SUBSCRIBE_EVENTS { terminal: None }`
//!    (server-wide), then wait for a barrier reply proving the server has
//!    processed the subscribe.
//! 3. Release the seed and assert the events arrive.
//!
//! ## Why the seeds are file-gated and not `sleep`-gated
//!
//! Every seed here used to defer its output with a fixed `sleep 0.25`,
//! betting that the client could connect, ATTACH and SUBSCRIBE inside that
//! window. Event fanout resolves subscribers *at emit time*, so an event
//! emitted before the subscription lands is legitimately dropped — losing
//! the bet does not produce a flaky assertion, it produces a permanently
//! failing one.
//!
//! That bet lost **deterministically on any machine with the `tailscale`
//! CLI on `$PATH`**. `ServerRuntime::run_async` pre-seeded the pane (which
//! starts the seed shell's clock) and only *then* computed its optional
//! WebSocket and QUIC listen addresses. Each of those called
//! `phux_config::overlay::detect()`, which shells out to `tailscale ip -4`
//! — roughly 150ms per call, twice, synchronously, on the server's
//! current-thread runtime, before the accept loop ever ran. The client
//! could not be served for ~300-400ms, by which time the seed had already
//! spoken. CI has no `tailscale` binary, so `detect()` degrades to a fast
//! UDP route probe, no stall occurred, and the same code passed there. A
//! test whose outcome turns on whether a VPN client happens to be
//! installed is a test nobody can trust.
//!
//! The startup path no longer works that way — phux-c6g6 stopped detection
//! running behind closed gates, and phux-90j5 moved the open-gate case off
//! the startup path entirely (`runtime::serve_auto_overlay_listeners`,
//! `tests/overlay_startup.rs`). The barrier below is kept regardless, and
//! the history is kept with it: what makes these tests trustworthy is that
//! they do not depend on server-startup latency being small, not that it
//! currently is.
//!
//! These tests are made **independent** of that dependency rather than
//! skipped when it is present. Nothing here is about overlay networking or
//! server startup latency — the ambient dependency only ever showed up as
//! stolen wall-clock, and a test that does not race the wall clock cannot
//! be robbed of it. Skipping would also hide the assertion on precisely
//! the machines developers use.
//!
//! The replacement is the two-part barrier from `agent_asked.rs`:
//!
//! * the seed spins on `until [ -f <release> ]` and emits nothing until the
//!   test writes that file, so no amount of server-startup latency can let
//!   the pane speak first; and
//! * the test writes the release file only after a `GET_STATE` reply
//!   proves the server has already installed the subscription —
//!   `SUBSCRIBE_EVENTS` carries no request id and is answered with no
//!   frame, so `send_frame` returning proves only that the bytes left this
//!   end. The per-connection frame loop handles frames in order, so a
//!   `CommandResult` for a request sent *after* the subscribe is proof the
//!   subscribe ahead of it already ran.
//!
//! No sleep was lengthened to fix this, and no deadline is load-bearing on
//! the happy path: each collector stops the moment its events arrive.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::Path;
use std::time::Duration;

use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, FrameKind, StateScope, TYPE_ATTACHED,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::tracing_capture::TracingCapture;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// A `/bin/sh -c` seed that blocks until `release` exists and then runs
/// `script`.
///
/// The gate is the whole point: the pane produces no observable output —
/// no title, no BEL, no OSC-133 mark, no EOF — until the test says so, so
/// the client's subscription is always installed first no matter how long
/// the server took to reach its accept loop. `until`/`[ -f ]`/`sleep` are
/// POSIX; the 10ms poll is the seed's own idle cost, not a test deadline.
fn gated_seed(release: &Path, script: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(format!(
        "until [ -f '{}' ]; do sleep 0.01; done; {script}",
        release.display(),
    ));
    cmd
}

/// Drain frames until the `COMMAND_RESULT` for `request_id` arrives.
async fn recv_command_result(stream: &mut UnixStream, request_id: u32) -> CommandResult {
    loop {
        let (_type_byte, frame) = recv_typed(stream).await;
        if let FrameKind::CommandResult {
            request_id: got,
            result,
        } = frame
            && got == request_id
        {
            return result;
        }
    }
}

/// `SUBSCRIBE_EVENTS { terminal: None }` plus the barrier that proves the
/// server processed it, then release the seed.
///
/// `GET_STATE { scope: Server }` is the barrier command: read-only, so
/// waiting on it changes nothing else, and it needs neither an ATTACH nor a
/// terminal id — so the attached and unattached tests can share one
/// barrier. Its `CommandResult` cannot be produced before the frame loop
/// has run `handle_subscribe_events`, which registers the subscription
/// synchronously in `ServerState`.
///
/// Returns only once the pane has been released, so everything the seed
/// emits from here on is guaranteed to have an observer.
async fn subscribe_then_release(stream: &mut UnixStream, request_id: u32, release: &Path) {
    send_frame(stream, &FrameKind::SubscribeEvents { terminal: None }).await;
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::GetState {
                scope: StateScope::Server,
            },
        },
    )
    .await;
    let barrier = timeout(WIRE_RECV_TIMEOUT, recv_command_result(stream, request_id))
        .await
        .expect("the server must answer GET_STATE before the collection window");
    assert!(
        !matches!(barrier, CommandResult::Error { .. }),
        "the subscribe barrier must succeed, got {barrier:?}",
    );
    std::fs::write(release, b"go").expect("release the seed pane");
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
///
/// Environment-independent by construction: the seed emits nothing until
/// the subscribe barrier releases it (see the module docs — a `tailscale`
/// binary on `$PATH` used to eat the fixed head start this test relied on).
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
        let release = tmp.path().join("release");

        // `printf` is POSIX. `\033]2;...\007` is OSC 2 set-title; `\007`
        // alone is the BEL. Then exit 0 -> PTY EOF -> pane_closed.
        let cmd = gated_seed(
            &release,
            "printf '\\033]2;phux-watch\\007'; printf '\\007'; exit 0",
        );
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

        // ---- SUBSCRIBE_EVENTS (server-wide), barrier, release ----
        subscribe_then_release(&mut stream, 1, &release).await;

        // ---- collect EVENT frames ----
        let events = collect_events(&mut stream, WIRE_RECV_TIMEOUT, |seen| {
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
///
/// Environment-independent by construction: same release-file gate as its
/// siblings, and the `GET_STATE` barrier deliberately needs no ATTACH.
#[test]
fn unattached_subscriber_receives_events() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let release = tmp.path().join("release");

        // A seed that stays silent until released, then exits -> PTY EOF ->
        // pane_closed.
        let cmd = gated_seed(&release, "exit 0");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // NO ATTACH. Subscribe server-wide straight away, barrier, release.
        subscribe_then_release(&mut stream, 1, &release).await;

        // `pane_closed` is the ONLY event this seed produces, so it is also
        // the stop condition — the collector returns the moment it lands
        // instead of sitting out the full deadline.
        let events = collect_events(&mut stream, WIRE_RECV_TIMEOUT, |seen| {
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
///
/// This test does NOT depend on the ambient login shell having OSC-133
/// shell integration: the seed writes the `C` and `D` marks itself with
/// `printf` over a fixed `/bin/sh`. What it *did* depend on was winning a
/// 250ms race against server startup, which a machine with `tailscale`
/// installed loses every time — see the module docs. The release-file gate
/// plus the subscribe barrier removes the race entirely.
#[test]
fn subscribed_client_receives_command_and_cwd_events() {
    run_local(async {
        let _cap = TracingCapture::install("agent_events_cmd_cwd");
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let release = tmp.path().join("release");

        // Once released: cd to / (a directory that cannot equal the spawn
        // cwd of the test server), then emit an OSC-133 C mark (command
        // executing), fake output, and a D mark carrying exit code 7.
        //
        // The trailing sleep only has to outlive the collection window, so
        // it comfortably does: the kernel cwd re-query at the D mark needs
        // a live child, and a pane that exits mid-window would reap the
        // last session, self-exit the server, and fail this test with
        // "early eof" rather than with anything about OSC-133. The
        // collector stops the moment its three events land, so the sleep
        // costs nothing on the happy path.
        let cmd = gated_seed(
            &release,
            "cd /; printf '\\033]133;C\\007out\\r\\n\\033]133;D;7\\007'; sleep 60",
        );
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        send_frame(&mut stream, &attach_by_name("demo")).await;
        let (type_byte, _attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED);

        subscribe_then_release(&mut stream, 1, &release).await;

        // Drain EVENT frames until all three targets are seen, or the
        // pane closes (no further events can follow, and reading on would
        // run into the server's post-exit socket teardown), or the
        // deadline elapses.
        let events = collect_events(&mut stream, WIRE_RECV_TIMEOUT, |seen| {
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
